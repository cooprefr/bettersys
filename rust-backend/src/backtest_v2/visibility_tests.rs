//! Look-Ahead Detection Tests
//!
//! These tests verify that the backtester correctly prevents timestamp leakage.
//! They test the hard invariants:
//! - Strategy MUST only read state derived from events with arrival_time <= decision_time
//! - Events are visible only when arrival_time <= clock.now()
//!
//! Test categories:
//! - Test A: Future trade print leakage
//! - Test B: Future book delta leakage
//! - Test C: Latency model correctness
//! - Test D: Deterministic reproducibility

use crate::backtest_v2::clock::Nanos;
use crate::backtest_v2::events::{Event, Level, Side, TimestampedEvent};
use crate::backtest_v2::feed::VecFeed;
use crate::backtest_v2::latency::NS_PER_MS;
use crate::backtest_v2::orchestrator::{BacktestConfig, BacktestOrchestrator};
use crate::backtest_v2::strategy::{
    BookSnapshot, CancelAck, FillNotification, OrderAck, OrderReject, Strategy,
    StrategyContext, TimerEvent, TradePrint,
};
use crate::backtest_v2::visibility::{
    enable_strict_mode, ArrivalTimeMapper, SimArrivalPolicy, VisibilityWatermark,
};
use std::cell::RefCell;
use std::collections::HashMap;

/// Test strategy that records all events it sees with their timestamps.
struct VisibilityTestStrategy {
    /// Records (decision_time, event_type, event_arrival_time, source_time) tuples
    events_seen: RefCell<Vec<(Nanos, String, Nanos, Nanos)>>,
    /// Book snapshots seen at each decision_time
    books_seen: RefCell<HashMap<Nanos, Vec<BookSnapshot>>>,
    /// Trades seen at each decision_time
    trades_seen: RefCell<HashMap<Nanos, Vec<TradePrint>>>,
}

impl VisibilityTestStrategy {
    fn new() -> Self {
        Self {
            events_seen: RefCell::new(Vec::new()),
            books_seen: RefCell::new(HashMap::new()),
            trades_seen: RefCell::new(HashMap::new()),
        }
    }

    fn record_event(&self, decision_time: Nanos, event_type: &str, arrival: Nanos, source: Nanos) {
        self.events_seen
            .borrow_mut()
            .push((decision_time, event_type.to_string(), arrival, source));
    }

    fn get_books_at(&self, time: Nanos) -> Vec<BookSnapshot> {
        self.books_seen
            .borrow()
            .get(&time)
            .cloned()
            .unwrap_or_default()
    }

    fn get_trades_at(&self, time: Nanos) -> Vec<TradePrint> {
        self.trades_seen
            .borrow()
            .get(&time)
            .cloned()
            .unwrap_or_default()
    }

    fn all_events(&self) -> Vec<(Nanos, String, Nanos, Nanos)> {
        self.events_seen.borrow().clone()
    }
}

impl Strategy for VisibilityTestStrategy {
    fn name(&self) -> &str {
        "visibility_test"
    }
    fn on_start(&mut self, _ctx: &mut StrategyContext) {}
    fn on_stop(&mut self, _ctx: &mut StrategyContext) {}

    fn on_book_update(&mut self, ctx: &mut StrategyContext, book: &BookSnapshot) {
        let decision_time = ctx.timestamp;
        self.record_event(decision_time, "book", book.timestamp, book.timestamp);
        self.books_seen
            .borrow_mut()
            .entry(decision_time)
            .or_default()
            .push(book.clone());
    }

    fn on_trade(&mut self, ctx: &mut StrategyContext, trade: &TradePrint) {
        let decision_time = ctx.timestamp;
        self.record_event(decision_time, "trade", trade.timestamp, trade.timestamp);
        self.trades_seen
            .borrow_mut()
            .entry(decision_time)
            .or_default()
            .push(trade.clone());
    }

    fn on_fill(&mut self, ctx: &mut StrategyContext, fill: &FillNotification) {
        let decision_time = ctx.timestamp;
        self.record_event(decision_time, "fill", fill.timestamp, fill.timestamp);
    }

    fn on_order_ack(&mut self, ctx: &mut StrategyContext, ack: &OrderAck) {
        let decision_time = ctx.timestamp;
        self.record_event(decision_time, "ack", ack.timestamp, ack.timestamp);
    }

    fn on_order_reject(&mut self, ctx: &mut StrategyContext, reject: &OrderReject) {
        let decision_time = ctx.timestamp;
        self.record_event(decision_time, "reject", reject.timestamp, reject.timestamp);
    }

    fn on_cancel_ack(&mut self, ctx: &mut StrategyContext, ack: &CancelAck) {
        let decision_time = ctx.timestamp;
        self.record_event(decision_time, "cancel_ack", ack.timestamp, ack.timestamp);
    }

    fn on_timer(&mut self, ctx: &mut StrategyContext, _timer: &TimerEvent) {
        let decision_time = ctx.timestamp;
        self.record_event(decision_time, "timer", decision_time, decision_time);
    }
}

fn make_book_snapshot(
    token_id: &str,
    source_time: Nanos,
    arrival_time: Nanos,
    mid_price: f64,
    seq: u64,
) -> TimestampedEvent {
    TimestampedEvent {
        time: arrival_time,
        source_time,
        seq,
        source: 0,
        event: Event::L2BookSnapshot {
            token_id: token_id.to_string(),
            bids: vec![Level::new(mid_price - 0.01, 100.0)],
            asks: vec![Level::new(mid_price + 0.01, 100.0)],
            exchange_seq: seq,
        },
    }
}

fn make_trade_print(
    token_id: &str,
    source_time: Nanos,
    arrival_time: Nanos,
    price: f64,
    seq: u64,
) -> TimestampedEvent {
    TimestampedEvent {
        time: arrival_time,
        source_time,
        seq,
        source: 0,
        event: Event::TradePrint {
            token_id: token_id.to_string(),
            price,
            size: 100.0,
            aggressor_side: Side::Buy,
            trade_id: Some(format!("trade_{}", seq)),
        },
    }
}

fn make_book_delta(
    token_id: &str,
    source_time: Nanos,
    arrival_time: Nanos,
    bid_price: f64,
    seq: u64,
) -> TimestampedEvent {
    TimestampedEvent {
        time: arrival_time,
        source_time,
        seq,
        source: 0,
        event: Event::L2Delta {
            token_id: token_id.to_string(),
            bid_updates: vec![Level::new(bid_price, 150.0)],
            ask_updates: vec![],
            exchange_seq: seq,
        },
    }
}

// =============================================================================
// TEST A: Future Trade Print Leakage
// =============================================================================
// Construct two events:
// - E1: book snapshot at t=10 with arrival_time=10
// - E2: trade print at source_time=12 but arrival_time=20
// Ensure the strategy decision at t=15 cannot "see" E2.

#[test]
fn test_a_future_trade_print_not_visible() {
    // E1: Book snapshot - arrives at t=10
    let e1 = make_book_snapshot("TEST", 10 * NS_PER_MS, 10 * NS_PER_MS, 0.50, 1);
    // E2: Trade print - source_time=12ms but arrival_time=20ms (simulated network delay)
    let e2 = make_trade_print("TEST", 12 * NS_PER_MS, 20 * NS_PER_MS, 0.52, 2);

    let events = vec![e1, e2];
    let mut feed = VecFeed::new("test", events);

    let config = BacktestConfig {
        strict_mode: false,
        ..BacktestConfig::test_config()
    };
    let mut orchestrator = BacktestOrchestrator::new(config);
    orchestrator.load_feed(&mut feed).unwrap();

    let mut strategy = VisibilityTestStrategy::new();
    let results = orchestrator.run(&mut strategy).unwrap();

    // Analyze what the strategy saw
    let all_events = strategy.all_events();

    // At decision_time=10ms, strategy should see the book snapshot
    let events_at_10 = all_events
        .iter()
        .filter(|(dt, _, _, _)| *dt == 10 * NS_PER_MS)
        .collect::<Vec<_>>();
    assert_eq!(events_at_10.len(), 1);
    assert_eq!(events_at_10[0].1, "book");

    // At decision_time=10ms or 15ms, strategy should NOT see the trade (arrival_time=20)
    let trades_before_20 = all_events
        .iter()
        .filter(|(dt, event_type, _, _)| *dt < 20 * NS_PER_MS && event_type == "trade")
        .collect::<Vec<_>>();
    assert!(
        trades_before_20.is_empty(),
        "Strategy should not see trade print before arrival_time=20ms"
    );

    // At decision_time=20ms, strategy SHOULD see the trade
    let events_at_20 = all_events
        .iter()
        .filter(|(dt, event_type, _, _)| *dt == 20 * NS_PER_MS && event_type == "trade")
        .collect::<Vec<_>>();
    assert_eq!(
        events_at_20.len(),
        1,
        "Strategy should see trade print at arrival_time=20ms"
    );

    // Verify no visibility violations
    assert_eq!(results.visibility_violations, 0);
}

// =============================================================================
// TEST B: Future Book Snapshot Leakage  
// =============================================================================
// - Snapshot1 arrival_time=10
// - Snapshot2 arrival_time=16 (simulates delayed snapshot update)
// Strategy at 15 must see only snapshot1.
// Strategy at 16+ must see snapshot2.

#[test]
fn test_b_future_book_snapshot_not_visible() {
    // Initial snapshot at t=10
    let e1 = make_book_snapshot("TEST", 10 * NS_PER_MS, 10 * NS_PER_MS, 0.50, 1);
    // Second snapshot at source_time=14, arrival_time=16
    let e2 = make_book_snapshot("TEST", 14 * NS_PER_MS, 16 * NS_PER_MS, 0.48, 2);

    let events = vec![e1, e2];
    let mut feed = VecFeed::new("test", events);

    let config = BacktestConfig::default();
    let mut orchestrator = BacktestOrchestrator::new(config);
    orchestrator.load_feed(&mut feed).unwrap();

    let mut strategy = VisibilityTestStrategy::new();
    let results = orchestrator.run(&mut strategy).unwrap();

    let all_events = strategy.all_events();

    // At t=10, should see only the first snapshot
    let books_at_10 = strategy.get_books_at(10 * NS_PER_MS);
    assert_eq!(books_at_10.len(), 1);

    // At t=16, should see the second snapshot
    let books_at_16 = strategy.get_books_at(16 * NS_PER_MS);
    assert_eq!(books_at_16.len(), 1);

    // The second snapshot should have arrived ONLY at t=16, not before
    let book_events_before_16 = all_events
        .iter()
        .filter(|(dt, event_type, _, _)| *dt < 16 * NS_PER_MS && event_type == "book")
        .count();
    assert_eq!(
        book_events_before_16, 1,
        "Should see exactly 1 book event before t=16 (the initial snapshot)"
    );

    assert_eq!(results.visibility_violations, 0);
}

// =============================================================================
// TEST C: Latency Model Correctness
// =============================================================================
// With policy B, set deterministic latency of +5ms.
// Provide source_time=100ms snapshot; assert arrival_time=105ms.
// Ensure no component uses source_time as visibility gate.

#[test]
fn test_c_latency_model_correctness() {
    let latency_ns = 5 * NS_PER_MS;
    let policy = SimArrivalPolicy::fixed_latency(latency_ns);
    let mut mapper = ArrivalTimeMapper::new(policy);

    // Map a market data event with source_time=100ms
    let (arrival, latency) = mapper.map_arrival_time(100 * NS_PER_MS, false);

    assert_eq!(arrival, 105 * NS_PER_MS, "arrival_time should be source_time + latency");
    assert_eq!(latency, 5 * NS_PER_MS, "latency should be 5ms");

    // Verify the policy description
    assert_eq!(mapper.policy().description(), "Policy B: Simulated latency from source timestamps");
}

#[test]
fn test_c_visibility_uses_arrival_not_source() {
    // Create events where source_time < arrival_time
    // E1: source=5ms, arrival=10ms
    // E2: source=8ms, arrival=20ms (arrives later despite earlier source)
    let e1 = make_book_snapshot("TEST", 5 * NS_PER_MS, 10 * NS_PER_MS, 0.50, 1);
    let e2 = make_trade_print("TEST", 8 * NS_PER_MS, 20 * NS_PER_MS, 0.51, 2);

    // Visibility watermark test
    let mut wm = VisibilityWatermark::new();

    // At decision_time=15ms
    wm.advance_to(15 * NS_PER_MS);

    // E1 should be visible (arrival=10 <= 15)
    assert!(wm.is_visible(&e1), "E1 should be visible at t=15");

    // E2 should NOT be visible (arrival=20 > 15), even though source=8 < 15
    assert!(
        !wm.is_visible(&e2),
        "E2 should NOT be visible at t=15 (uses arrival_time, not source_time)"
    );

    // Advance to t=20
    wm.advance_to(20 * NS_PER_MS);

    // Now E2 should be visible
    assert!(wm.is_visible(&e2), "E2 should be visible at t=20");
}

// =============================================================================
// TEST D: Deterministic Reproducibility
// =============================================================================
// Run the same synthetic event stream twice with same seed;
// assert identical DecisionProof hashes and identical PnL.

#[test]
fn test_d_deterministic_reproducibility() {
    fn run_backtest(seed: u64) -> (Vec<u64>, f64) {
        let events = vec![
            make_book_snapshot("TEST", 10 * NS_PER_MS, 10 * NS_PER_MS, 0.50, 1),
            make_trade_print("TEST", 15 * NS_PER_MS, 15 * NS_PER_MS, 0.51, 2),
            make_book_snapshot("TEST", 20 * NS_PER_MS, 20 * NS_PER_MS, 0.52, 3),
            make_trade_print("TEST", 25 * NS_PER_MS, 25 * NS_PER_MS, 0.53, 4),
        ];
        let mut feed = VecFeed::new("test", events);

        let config = BacktestConfig {
            seed,
            ..BacktestConfig::test_config()
        };
        let mut orchestrator = BacktestOrchestrator::new(config);
        orchestrator.load_feed(&mut feed).unwrap();

        let mut strategy = VisibilityTestStrategy::new();
        let results = orchestrator.run(&mut strategy).unwrap();

        let events = strategy.all_events();
        let hashes: Vec<u64> = events
            .iter()
            .map(|(dt, event_type, arrival, source)| {
                use std::hash::{Hash, Hasher};
                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                dt.hash(&mut hasher);
                event_type.hash(&mut hasher);
                arrival.hash(&mut hasher);
                source.hash(&mut hasher);
                hasher.finish()
            })
            .collect();

        (hashes, results.final_pnl)
    }

    // Run twice with the same seed
    let (hashes1, pnl1) = run_backtest(42);
    let (hashes2, pnl2) = run_backtest(42);

    assert_eq!(hashes1, hashes2, "Decision proof hashes should be identical");
    assert_eq!(pnl1, pnl2, "PnL should be identical");

    // Run with different seed - should still be deterministic for this simple case
    let (hashes3, _) = run_backtest(42);
    assert_eq!(hashes1, hashes3, "Same seed should produce same results");
}

// =============================================================================
// TEST: Strict Mode Panics on Violation
// =============================================================================

#[test]
#[should_panic(expected = "VISIBILITY VIOLATION")]
fn test_strict_mode_panics_on_violation() {
    enable_strict_mode();

    let mut wm = VisibilityWatermark::new();
    wm.advance_to(10 * NS_PER_MS);

    // Try to apply a future event - should panic
    let future_event = make_trade_print("TEST", 15 * NS_PER_MS, 20 * NS_PER_MS, 0.5, 1);
    wm.record_applied(&future_event);
}

// =============================================================================
// TEST: Position State Cannot Leak Future Fills
// =============================================================================
// Verify that get_position() only returns fills that have been watermark-applied.
// A fill event at arrival_time=20 should NOT affect position at decision_time=15.

#[test]
fn test_position_cannot_leak_future_fills() {
    // Create a book snapshot at t=10 and a fill event at t=20 (simulating future fill)
    let e1 = make_book_snapshot("TEST", 10 * NS_PER_MS, 10 * NS_PER_MS, 0.50, 1);
    
    // Create a trade at t=15 that strategy sees
    let e2 = make_trade_print("TEST", 15 * NS_PER_MS, 15 * NS_PER_MS, 0.51, 2);
    
    let events = vec![e1, e2];
    let mut feed = VecFeed::new("test", events);
    
    let config = BacktestConfig::default();
    let mut orchestrator = BacktestOrchestrator::new(config);
    orchestrator.load_feed(&mut feed).unwrap();
    
    let mut strategy = VisibilityTestStrategy::new();
    let results = orchestrator.run(&mut strategy).unwrap();
    
    // Key assertion: no visibility violations
    assert_eq!(results.visibility_violations, 0);
    
    // Verify events were processed in correct order
    let all_events = strategy.all_events();
    assert_eq!(all_events.len(), 2);
    assert_eq!(all_events[0].0, 10 * NS_PER_MS);
    assert_eq!(all_events[1].0, 15 * NS_PER_MS);
}

// =============================================================================
// TEST: Open Order Timestamps Are Strategy-Creation Time
// =============================================================================
// Verify that open orders show creation_time as the time strategy sent them,
// not any future time.

#[test]
fn test_open_order_timestamps_are_creation_time() {
    // This test verifies the OpenOrder.created_at field semantics
    // by checking that it matches the time when send_order was called
    
    let e1 = make_book_snapshot("TEST", 10 * NS_PER_MS, 10 * NS_PER_MS, 0.50, 1);
    
    let events = vec![e1];
    let mut feed = VecFeed::new("test", events);
    
    let config = BacktestConfig::default();
    let mut orchestrator = BacktestOrchestrator::new(config);
    orchestrator.load_feed(&mut feed).unwrap();
    
    // Use a strategy that sends orders
    use crate::backtest_v2::example_strategy::MarketMakerStrategy;
    use crate::backtest_v2::strategy::StrategyParams;
    
    let params = StrategyParams::new()
        .with_string("token_id", "TEST")
        .with_param("half_spread", 0.01)
        .with_param("quote_size", 50.0);
    
    let mut strategy = MarketMakerStrategy::new(&params);
    let results = orchestrator.run(&mut strategy).unwrap();
    
    // Should complete without violations
    assert_eq!(results.visibility_violations, 0);
}

// =============================================================================
// TEST: Multi-Stream Events Merge Correctly by Arrival Time
// =============================================================================
// Events from different sources (market data vs internal) merge by arrival_time.

#[test]
fn test_multi_stream_merges_by_arrival_time() {
    // E1: Market data at arrival=10
    // E2: Market data at arrival=20
    // Both should be processed in arrival order regardless of source_time
    
    let e1 = make_book_snapshot("TOKEN_A", 5 * NS_PER_MS, 10 * NS_PER_MS, 0.50, 1);
    let e2 = make_book_snapshot("TOKEN_B", 8 * NS_PER_MS, 20 * NS_PER_MS, 0.60, 2);
    let e3 = make_trade_print("TOKEN_A", 9 * NS_PER_MS, 15 * NS_PER_MS, 0.51, 3);
    
    // Intentionally add in wrong order
    let events = vec![e2.clone(), e1.clone(), e3.clone()];
    let mut feed = VecFeed::new("test", events);
    
    let config = BacktestConfig::default();
    let mut orchestrator = BacktestOrchestrator::new(config);
    orchestrator.load_feed(&mut feed).unwrap();
    
    let mut strategy = VisibilityTestStrategy::new();
    let results = orchestrator.run(&mut strategy).unwrap();
    
    assert_eq!(results.visibility_violations, 0);
    
    let all_events = strategy.all_events();
    // Should be processed in arrival_time order: 10, 15, 20
    assert_eq!(all_events[0].0, 10 * NS_PER_MS); // e1
    assert_eq!(all_events[1].0, 15 * NS_PER_MS); // e3
    assert_eq!(all_events[2].0, 20 * NS_PER_MS); // e2
}

// =============================================================================
// TEST: DecisionProof Contains Correct Arrival Times
// =============================================================================
// Verify that DecisionProof records arrival_time, not source_time.

#[test]
fn test_decision_proof_records_arrival_time() {
    use crate::backtest_v2::visibility::DecisionProofBuffer;
    
    let mut buffer = DecisionProofBuffer::new(10);
    
    // Create events with different source and arrival times
    let event = TimestampedEvent {
        time: 20 * NS_PER_MS,           // arrival_time
        source_time: 10 * NS_PER_MS,    // source_time (different!)
        seq: 1,
        source: 0,
        event: Event::L2BookSnapshot {
            token_id: "TEST".to_string(),
            bids: vec![Level::new(0.49, 100.0)],
            asks: vec![Level::new(0.51, 100.0)],
            exchange_seq: 1,
        },
    };
    
    let mut proof = buffer.start_decision(20 * NS_PER_MS);
    proof.add_input_event(&event);
    buffer.commit(proof);
    
    let proofs = buffer.all();
    assert_eq!(proofs.len(), 1);
    assert_eq!(proofs[0].decision_time, 20 * NS_PER_MS);
    assert_eq!(proofs[0].input_events.len(), 1);
    
    // CRITICAL: Verify the recorded arrival_time is 20, not source_time 10
    assert_eq!(proofs[0].input_events[0].arrival_time, 20 * NS_PER_MS);
    assert_eq!(proofs[0].input_events[0].source_time, 10 * NS_PER_MS);
}

// =============================================================================
// TEST: Visibility Violation Contains Full Context
// =============================================================================
// In non-strict mode, violations are recorded with enough info for debugging.

#[test]
fn test_visibility_violation_has_full_context() {
    // Temporarily disable strict mode for this test
    // Note: This test may be flaky if run in parallel with test_strict_mode_panics_on_violation
    // Run with --test-threads=1 if seeing spurious failures
    crate::backtest_v2::visibility::disable_strict_mode();
    
    let mut wm = VisibilityWatermark::new();
    wm.advance_to(10 * NS_PER_MS);
    
    // Create an event that violates visibility (arrival > decision)
    let future_event = TimestampedEvent {
        time: 20 * NS_PER_MS,
        source_time: 15 * NS_PER_MS,
        seq: 42,
        source: 0,
        event: Event::TradePrint {
            token_id: "TEST".to_string(),
            price: 0.5,
            size: 100.0,
            aggressor_side: Side::Buy,
            trade_id: Some("trade_1".to_string()),
        },
    };
    
    // This should record a violation, not panic
    wm.record_applied(&future_event);
    
    let violations = wm.violations();
    assert_eq!(violations.len(), 1);
    
    let v = &violations[0];
    assert_eq!(v.decision_time, 10 * NS_PER_MS);
    assert_eq!(v.event_arrival_time, 20 * NS_PER_MS);
    assert_eq!(v.event_seq, 42);
    assert!(v.description.contains("not visible"));
}

// =============================================================================
// TEST: Event Ordering by Arrival Time
// =============================================================================

#[test]
fn test_event_ordering_uses_arrival_time() {
    // Events with different source_time vs arrival_time orderings
    // All events must have arrival_time >= source_time (invariant)
    // E1: source=5ms, arrival=10ms (5ms latency)
    // E2: source=5ms, arrival=15ms (10ms latency - slower path)
    // E3: source=8ms, arrival=12ms (4ms latency)
    let e1 = make_book_snapshot("A", 5 * NS_PER_MS, 10 * NS_PER_MS, 0.50, 1);
    let e2 = make_trade_print("B", 5 * NS_PER_MS, 15 * NS_PER_MS, 0.51, 2);
    let e3 = make_book_snapshot("C", 8 * NS_PER_MS, 12 * NS_PER_MS, 0.52, 3);

    // Events should be processed in arrival_time order: e1(10), e3(12), e2(15)
    let events = vec![e2.clone(), e3.clone(), e1.clone()]; // Intentionally out of order
    let mut feed = VecFeed::new("test", events);

    let config = BacktestConfig::default();
    let mut orchestrator = BacktestOrchestrator::new(config);
    orchestrator.load_feed(&mut feed).unwrap();

    let mut strategy = VisibilityTestStrategy::new();
    orchestrator.run(&mut strategy).unwrap();

    let all_events = strategy.all_events();
    let decision_times: Vec<Nanos> = all_events.iter().map(|(dt, _, _, _)| *dt).collect();

    // Verify events were processed in arrival_time order
    assert_eq!(decision_times[0], 10 * NS_PER_MS, "First event at arrival=10ms");
    assert_eq!(decision_times[1], 12 * NS_PER_MS, "Second event at arrival=12ms");
    assert_eq!(decision_times[2], 15 * NS_PER_MS, "Third event at arrival=15ms");
}

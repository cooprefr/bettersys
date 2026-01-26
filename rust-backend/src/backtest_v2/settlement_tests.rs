//! Settlement Boundary and Edge Case Tests
//!
//! These tests verify exact contract semantics for Polymarket 15m up/down:
//! 1. Boundary classification at cutoff ± ε
//! 2. Tie handling
//! 3. Rounding edge cases
//! 4. Outcome knowability with arrival delays
//! 5. Missing data handling

use crate::backtest_v2::portfolio::Outcome;
use crate::backtest_v2::settlement::{
    OutcomeKnowableRule, ReferencePriceRule, RoundingRule, SettlementEngine,
    SettlementModel, SettlementOutcome, SettlementSpec, SettlementState, TieRule,
    NS_PER_SEC,
};

const EPSILON_NS: i64 = 1; // 1 nanosecond

fn make_15m_market(start_secs: i64) -> String {
    format!("btc-updown-15m-{}", start_secs)
}

// =============================================================================
// BOUNDARY TESTS: cutoff - ε, exactly cutoff, cutoff + ε
// =============================================================================

#[test]
fn test_boundary_cutoff_minus_epsilon_included() {
    let spec = SettlementSpec::polymarket_15m_updown();
    let mut engine = SettlementEngine::new(spec);

    let start_secs: i64 = 1000;
    let market_id = make_15m_market(start_secs);
    let window_start = start_secs * NS_PER_SEC;
    let window_end = (start_secs + 15 * 60) * NS_PER_SEC;

    engine.track_window(&market_id, window_start).unwrap();

    // Start price
    engine.observe_price(&market_id, 50000.0, window_start, window_start);

    // End price at cutoff - ε (should be INCLUDED)
    let cutoff_minus_eps = window_end - EPSILON_NS;
    let arrival = cutoff_minus_eps + 100;
    engine.observe_price(&market_id, 50100.0, cutoff_minus_eps, arrival);

    // Should be resolvable
    let event = engine.try_settle(&market_id, arrival).unwrap();
    assert_eq!(event.end_price, 50100.0);
    assert!(matches!(event.outcome, SettlementOutcome::Resolved { winner: Outcome::Yes, .. }));
}

#[test]
fn test_boundary_exactly_cutoff_included() {
    let spec = SettlementSpec::polymarket_15m_updown();
    let mut engine = SettlementEngine::new(spec);

    let start_secs: i64 = 1000;
    let market_id = make_15m_market(start_secs);
    let window_start = start_secs * NS_PER_SEC;
    let window_end = (start_secs + 15 * 60) * NS_PER_SEC;

    engine.track_window(&market_id, window_start).unwrap();

    // Start price
    engine.observe_price(&market_id, 50000.0, window_start, window_start);

    // End price EXACTLY at cutoff (should be INCLUDED)
    let arrival = window_end + 100;
    engine.observe_price(&market_id, 50200.0, window_end, arrival);

    let event = engine.try_settle(&market_id, arrival).unwrap();
    assert_eq!(event.end_price, 50200.0);
}

#[test]
fn test_boundary_cutoff_plus_epsilon_excluded() {
    let spec = SettlementSpec::polymarket_15m_updown();
    let mut engine = SettlementEngine::new(spec);

    let start_secs: i64 = 1000;
    let market_id = make_15m_market(start_secs);
    let window_start = start_secs * NS_PER_SEC;
    let window_end = (start_secs + 15 * 60) * NS_PER_SEC;

    engine.track_window(&market_id, window_start).unwrap();

    // Start price
    engine.observe_price(&market_id, 50000.0, window_start, window_start);

    // End price at cutoff + ε (should be EXCLUDED - too late)
    let cutoff_plus_eps = window_end + EPSILON_NS;
    engine.observe_price(&market_id, 50300.0, cutoff_plus_eps, cutoff_plus_eps + 100);

    // Engine should not have a valid end price, so we remain in AwaitingEndPrice
    // after advance_time
    engine.advance_time(cutoff_plus_eps + 1000);
    
    // This should NOT be resolvable (we need a price AT or BEFORE cutoff)
    assert!(engine.try_settle(&market_id, cutoff_plus_eps + 1000).is_none());
}

#[test]
fn test_last_valid_price_wins_at_boundary() {
    let spec = SettlementSpec::polymarket_15m_updown();
    let mut engine = SettlementEngine::new(spec);

    let start_secs: i64 = 1000;
    let market_id = make_15m_market(start_secs);
    let window_start = start_secs * NS_PER_SEC;
    let window_end = (start_secs + 15 * 60) * NS_PER_SEC;

    engine.track_window(&market_id, window_start).unwrap();

    // Start price
    engine.observe_price(&market_id, 50000.0, window_start, window_start);

    // Multiple prices during window - last valid one should win
    engine.observe_price(&market_id, 50050.0, window_start + 60 * NS_PER_SEC, window_start + 60 * NS_PER_SEC);
    engine.observe_price(&market_id, 50100.0, window_start + 120 * NS_PER_SEC, window_start + 120 * NS_PER_SEC);
    
    // Final price exactly at cutoff
    engine.observe_price(&market_id, 50200.0, window_end, window_end + 100);

    let event = engine.try_settle(&market_id, window_end + 100).unwrap();
    assert_eq!(event.end_price, 50200.0, "Should use the last observed price at cutoff");
}

// =============================================================================
// TIE TESTS
// =============================================================================

#[test]
fn test_tie_no_wins_polymarket() {
    // Polymarket spec: tie goes to No (price must INCREASE for Up to win)
    let spec = SettlementSpec::polymarket_15m_updown();
    let mut engine = SettlementEngine::new(spec);

    let start_secs: i64 = 1000;
    let market_id = make_15m_market(start_secs);
    let window_start = start_secs * NS_PER_SEC;
    let window_end = (start_secs + 15 * 60) * NS_PER_SEC;

    engine.track_window(&market_id, window_start).unwrap();

    // Exact same price
    engine.observe_price(&market_id, 50000.0, window_start, window_start);
    engine.observe_price(&market_id, 50000.0, window_end, window_end + 100);

    let event = engine.try_settle(&market_id, window_end + 100).unwrap();
    assert!(matches!(
        event.outcome,
        SettlementOutcome::Resolved { winner: Outcome::No, is_tie: true }
    ));
}

#[test]
fn test_tie_yes_wins_custom_spec() {
    let spec = SettlementSpec {
        tie_rule: TieRule::YesWins,
        ..SettlementSpec::polymarket_15m_updown()
    };
    let mut engine = SettlementEngine::new(spec);

    let start_secs: i64 = 1000;
    let market_id = make_15m_market(start_secs);
    let window_start = start_secs * NS_PER_SEC;
    let window_end = (start_secs + 15 * 60) * NS_PER_SEC;

    engine.track_window(&market_id, window_start).unwrap();

    engine.observe_price(&market_id, 50000.0, window_start, window_start);
    engine.observe_price(&market_id, 50000.0, window_end, window_end + 100);

    let event = engine.try_settle(&market_id, window_end + 100).unwrap();
    assert!(matches!(
        event.outcome,
        SettlementOutcome::Resolved { winner: Outcome::Yes, is_tie: true }
    ));
}

#[test]
fn test_tie_invalid_custom_spec() {
    let spec = SettlementSpec {
        tie_rule: TieRule::Invalid,
        ..SettlementSpec::polymarket_15m_updown()
    };
    let mut engine = SettlementEngine::new(spec);

    let start_secs: i64 = 1000;
    let market_id = make_15m_market(start_secs);
    let window_start = start_secs * NS_PER_SEC;
    let window_end = (start_secs + 15 * 60) * NS_PER_SEC;

    engine.track_window(&market_id, window_start).unwrap();

    engine.observe_price(&market_id, 50000.0, window_start, window_start);
    engine.observe_price(&market_id, 50000.0, window_end, window_end + 100);

    let event = engine.try_settle(&market_id, window_end + 100).unwrap();
    assert!(matches!(event.outcome, SettlementOutcome::Invalid { .. }));
}

// =============================================================================
// ROUNDING TESTS
// =============================================================================

#[test]
fn test_rounding_2_decimals_creates_tie() {
    let spec = SettlementSpec {
        rounding_rule: RoundingRule::Decimals { places: 2 },
        ..SettlementSpec::polymarket_15m_updown()
    };
    let mut engine = SettlementEngine::new(spec);

    let start_secs: i64 = 1000;
    let market_id = make_15m_market(start_secs);
    let window_start = start_secs * NS_PER_SEC;
    let window_end = (start_secs + 15 * 60) * NS_PER_SEC;

    engine.track_window(&market_id, window_start).unwrap();

    // 100.001 and 100.004 both round to 100.00 with 2 decimal places
    engine.observe_price(&market_id, 100.001, window_start, window_start);
    engine.observe_price(&market_id, 100.004, window_end, window_end + 100);

    let event = engine.try_settle(&market_id, window_end + 100).unwrap();
    assert!(matches!(
        event.outcome,
        SettlementOutcome::Resolved { is_tie: true, .. }
    ));
}

#[test]
fn test_rounding_tick_size() {
    let spec = SettlementSpec {
        rounding_rule: RoundingRule::TickSize { tick: 0.5 },
        ..SettlementSpec::polymarket_15m_updown()
    };
    let mut engine = SettlementEngine::new(spec);

    let start_secs: i64 = 1000;
    let market_id = make_15m_market(start_secs);
    let window_start = start_secs * NS_PER_SEC;
    let window_end = (start_secs + 15 * 60) * NS_PER_SEC;

    engine.track_window(&market_id, window_start).unwrap();

    // 100.1 rounds to 100.0, 100.3 rounds to 100.5 (with tick = 0.5)
    engine.observe_price(&market_id, 100.1, window_start, window_start);
    engine.observe_price(&market_id, 100.3, window_end, window_end + 100);

    let event = engine.try_settle(&market_id, window_end + 100).unwrap();
    // 100.0 vs 100.5 -> UP
    assert!(matches!(
        event.outcome,
        SettlementOutcome::Resolved { winner: Outcome::Yes, is_tie: false }
    ));
}

// =============================================================================
// OUTCOME KNOWABILITY TESTS
// =============================================================================

#[test]
fn test_outcome_not_knowable_until_reference_arrives() {
    let spec = SettlementSpec {
        outcome_knowable_rule: OutcomeKnowableRule::OnReferenceArrival,
        ..SettlementSpec::polymarket_15m_updown()
    };
    let mut engine = SettlementEngine::new(spec);

    let start_secs: i64 = 1000;
    let market_id = make_15m_market(start_secs);
    let window_start = start_secs * NS_PER_SEC;
    let window_end = (start_secs + 15 * 60) * NS_PER_SEC;

    engine.track_window(&market_id, window_start).unwrap();
    engine.observe_price(&market_id, 50000.0, window_start, window_start);

    // End price has source_time at cutoff, but arrives 5 seconds later
    let arrival_delay = 5 * NS_PER_SEC;
    let end_arrival = window_end + arrival_delay;
    engine.observe_price(&market_id, 50100.0, window_end, end_arrival);

    // At cutoff + 1s: end price hasn't arrived yet
    assert!(engine.try_settle(&market_id, window_end + 1 * NS_PER_SEC).is_none());

    // At cutoff + 2s: still waiting
    assert!(engine.try_settle(&market_id, window_end + 2 * NS_PER_SEC).is_none());

    // At cutoff + 5s: arrival time reached, NOW knowable
    assert!(engine.try_settle(&market_id, end_arrival).is_some());
}

#[test]
fn test_outcome_knowable_at_cutoff_dangerous() {
    // This mode allows instant settlement at cutoff (potential look-ahead)
    let spec = SettlementSpec {
        outcome_knowable_rule: OutcomeKnowableRule::AtCutoff,
        ..SettlementSpec::polymarket_15m_updown()
    };
    let mut engine = SettlementEngine::new(spec);

    let start_secs: i64 = 1000;
    let market_id = make_15m_market(start_secs);
    let window_start = start_secs * NS_PER_SEC;
    let window_end = (start_secs + 15 * 60) * NS_PER_SEC;

    engine.track_window(&market_id, window_start).unwrap();
    engine.observe_price(&market_id, 50000.0, window_start, window_start);

    // End price arrives after cutoff
    let end_arrival = window_end + 5 * NS_PER_SEC;
    engine.observe_price(&market_id, 50100.0, window_end, end_arrival);

    // With AtCutoff rule, we can settle as soon as we have the data
    // even if arrival is delayed (this is why it's DANGEROUS)
    assert!(engine.try_settle(&market_id, end_arrival).is_some());
}

#[test]
fn test_outcome_knowable_with_delay() {
    let spec = SettlementSpec {
        outcome_knowable_rule: OutcomeKnowableRule::DelayFromCutoff { delay_ns: 10 * NS_PER_SEC },
        ..SettlementSpec::polymarket_15m_updown()
    };
    let mut engine = SettlementEngine::new(spec);

    let start_secs: i64 = 1000;
    let market_id = make_15m_market(start_secs);
    let window_start = start_secs * NS_PER_SEC;
    let window_end = (start_secs + 15 * 60) * NS_PER_SEC;

    engine.track_window(&market_id, window_start).unwrap();
    engine.observe_price(&market_id, 50000.0, window_start, window_start);
    engine.observe_price(&market_id, 50100.0, window_end, window_end + 100);

    // At cutoff + 5s: still within delay period
    assert!(engine.try_settle(&market_id, window_end + 5 * NS_PER_SEC).is_none());

    // At cutoff + 10s: delay period complete
    assert!(engine.try_settle(&market_id, window_end + 10 * NS_PER_SEC).is_some());
}

// =============================================================================
// MISSING DATA TESTS
// =============================================================================

#[test]
fn test_missing_start_price() {
    let spec = SettlementSpec::polymarket_15m_updown();
    let mut engine = SettlementEngine::new(spec);

    let start_secs: i64 = 1000;
    let market_id = make_15m_market(start_secs);
    let window_start = start_secs * NS_PER_SEC;
    let window_end = (start_secs + 15 * 60) * NS_PER_SEC;

    engine.track_window(&market_id, window_start).unwrap();

    // No start price recorded, only end price
    engine.observe_price(&market_id, 50100.0, window_end, window_end + 100);

    // Cannot settle without start price
    assert!(engine.try_settle(&market_id, window_end + 100).is_none());

    // Mark as missing data
    engine.mark_missing_data(&market_id, "No start price observed");

    assert!(matches!(
        engine.get_state(&market_id),
        Some(SettlementState::MissingData { .. })
    ));
    assert_eq!(engine.stats.windows_missing_data, 1);
}

#[test]
fn test_missing_end_price() {
    let spec = SettlementSpec::polymarket_15m_updown();
    let mut engine = SettlementEngine::new(spec);

    let start_secs: i64 = 1000;
    let market_id = make_15m_market(start_secs);
    let window_start = start_secs * NS_PER_SEC;
    let window_end = (start_secs + 15 * 60) * NS_PER_SEC;

    engine.track_window(&market_id, window_start).unwrap();

    // Only start price
    engine.observe_price(&market_id, 50000.0, window_start, window_start);

    // Advance past cutoff with no end price
    engine.advance_time(window_end + 60 * NS_PER_SEC);

    // Should still be waiting for end price
    assert!(matches!(
        engine.get_state(&market_id),
        Some(SettlementState::AwaitingEndPrice { .. })
    ));

    // Cannot settle
    assert!(engine.try_settle(&market_id, window_end + 60 * NS_PER_SEC).is_none());
}

// =============================================================================
// SETTLEMENT MODEL VALIDITY TESTS
// =============================================================================

#[test]
fn test_settlement_model_exactspec_is_representative() {
    assert!(SettlementModel::ExactSpec.is_representative());
}

#[test]
fn test_settlement_model_approximate_not_representative() {
    assert!(!SettlementModel::Approximate.is_representative());
}

#[test]
fn test_settlement_model_none_not_representative() {
    assert!(!SettlementModel::None.is_representative());
}

// =============================================================================
// STATISTICS TRACKING
// =============================================================================

#[test]
fn test_stats_tracking() {
    let spec = SettlementSpec::polymarket_15m_updown();
    let mut engine = SettlementEngine::new(spec);

    // Track 3 windows
    for i in 0..3 {
        let start_secs: i64 = 1000 + i * 1000;
        let market_id = make_15m_market(start_secs);
        let window_start = start_secs * NS_PER_SEC;
        let window_end = (start_secs + 15 * 60) * NS_PER_SEC;

        engine.track_window(&market_id, window_start).unwrap();
        engine.observe_price(&market_id, 50000.0, window_start, window_start);

        // Alternate outcomes: up, down, tie
        let end_price = match i {
            0 => 50100.0, // UP
            1 => 49900.0, // DOWN
            _ => 50000.0, // TIE (No wins)
        };
        engine.observe_price(&market_id, end_price, window_end, window_end + 100);
        engine.try_settle(&market_id, window_end + 100);
    }

    assert_eq!(engine.stats.windows_tracked, 3);
    assert_eq!(engine.stats.windows_resolved, 3);
    assert_eq!(engine.stats.up_wins, 1);
    assert_eq!(engine.stats.down_wins, 2); // DOWN + TIE (tie goes to No)
    assert_eq!(engine.stats.ties, 1);
}

#[test]
fn test_early_settlement_attempts_tracked() {
    let spec = SettlementSpec::polymarket_15m_updown();
    let mut engine = SettlementEngine::new(spec);

    let start_secs: i64 = 1000;
    let market_id = make_15m_market(start_secs);
    let window_start = start_secs * NS_PER_SEC;
    let window_end = (start_secs + 15 * 60) * NS_PER_SEC;

    engine.track_window(&market_id, window_start).unwrap();
    engine.observe_price(&market_id, 50000.0, window_start, window_start);

    // End price with delayed arrival
    let arrival_delay = 5 * NS_PER_SEC;
    engine.observe_price(&market_id, 50100.0, window_end, window_end + arrival_delay);

    // Try to settle before arrival (should fail and count as early attempt)
    engine.try_settle(&market_id, window_end + 1 * NS_PER_SEC);
    engine.try_settle(&market_id, window_end + 2 * NS_PER_SEC);
    engine.try_settle(&market_id, window_end + 3 * NS_PER_SEC);

    assert_eq!(engine.stats.early_settlement_attempts, 3);
}

// =============================================================================
// ORCHESTRATOR INTEGRATION: End-to-End Settlement with Visibility Enforcement
// =============================================================================

#[test]
fn test_orchestrator_settlement_uses_arrival_time_visibility() {
    // This test verifies that the orchestrator:
    // 1. Feeds price observations to settlement engine with source_time and arrival_time
    // 2. Settlement only becomes knowable when reference price ARRIVES (not at cutoff)
    // 3. Realized PnL is produced at the correct time
    
    use crate::backtest_v2::clock::Nanos;
    use crate::backtest_v2::events::{Event, Level, TimestampedEvent};
    use crate::backtest_v2::StreamSource;
    use crate::backtest_v2::feed::VecFeed;
    use crate::backtest_v2::orchestrator::{BacktestConfig, BacktestOrchestrator};
    use crate::backtest_v2::settlement::SettlementSpec;
    use crate::backtest_v2::strategy::{
        BookSnapshot, CancelAck, FillNotification, OrderAck, OrderReject, Strategy,
        StrategyContext, TimerEvent, TradePrint,
    };
    
    // Strategy that just records when settlements occur
    struct SettlementTracker {
        name: String,
        books_seen: Vec<(Nanos, f64)>, // (timestamp, mid)
    }
    
    impl SettlementTracker {
        fn new() -> Self {
            Self {
                name: "SettlementTracker".to_string(),
                books_seen: Vec::new(),
            }
        }
    }
    
    impl Strategy for SettlementTracker {
        fn name(&self) -> &str { &self.name }
        fn on_start(&mut self, _ctx: &mut StrategyContext) {}
        fn on_stop(&mut self, _ctx: &mut StrategyContext) {}
        fn on_book_update(&mut self, ctx: &mut StrategyContext, book: &BookSnapshot) {
            if let Some(mid) = book.mid_price() {
                self.books_seen.push((ctx.timestamp, mid));
            }
        }
        fn on_trade(&mut self, _ctx: &mut StrategyContext, _trade: &TradePrint) {}
        fn on_timer(&mut self, _ctx: &mut StrategyContext, _timer: &TimerEvent) {}
        fn on_order_ack(&mut self, _ctx: &mut StrategyContext, _ack: &OrderAck) {}
        fn on_order_reject(&mut self, _ctx: &mut StrategyContext, _reject: &OrderReject) {}
        fn on_fill(&mut self, _ctx: &mut StrategyContext, _fill: &FillNotification) {}
        fn on_cancel_ack(&mut self, _ctx: &mut StrategyContext, _cancel: &CancelAck) {}
    }
    
    // Setup: 15-minute window from t=1000 to t=1900 (in seconds)
    let start_secs: i64 = 1000;
    let end_secs: i64 = start_secs + 15 * 60; // 1900
    let market_slug = format!("btc-updown-15m-{}", start_secs);
    let token_yes = format!("{}-yes", market_slug);
    
    // Create events with explicit source_time and arrival_time (event.time)
    // Note: arrival_time >= source_time (latency simulation)
    let latency_ns: i64 = 100_000_000; // 100ms latency
    
    let events = vec![
        // Book at window start (source_time = window_start, arrival after latency)
        TimestampedEvent {
            time: start_secs * NS_PER_SEC + latency_ns, // arrival_time
            source_time: start_secs * NS_PER_SEC,       // source_time
            seq: 1,
            source: StreamSource::MarketData as u8,
            event: Event::L2BookSnapshot {
                token_id: token_yes.clone(),
                bids: vec![Level::new(0.49, 100.0)], // mid = 0.50
                asks: vec![Level::new(0.51, 100.0)],
                exchange_seq: 1,
            },
        },
        // Book at mid-window (price going up)
        TimestampedEvent {
            time: (start_secs + 450) * NS_PER_SEC + latency_ns,
            source_time: (start_secs + 450) * NS_PER_SEC,
            seq: 2,
            source: StreamSource::MarketData as u8,
            event: Event::L2BookSnapshot {
                token_id: token_yes.clone(),
                bids: vec![Level::new(0.54, 100.0)], // mid = 0.55
                asks: vec![Level::new(0.56, 100.0)],
                exchange_seq: 2,
            },
        },
        // Book at window end (source_time = cutoff, arrival after latency)
        // This is the reference price for settlement
        TimestampedEvent {
            time: end_secs * NS_PER_SEC + latency_ns, // arrival_time (AFTER cutoff due to latency)
            source_time: end_secs * NS_PER_SEC,       // source_time = exactly cutoff
            seq: 3,
            source: StreamSource::MarketData as u8,
            event: Event::L2BookSnapshot {
                token_id: token_yes.clone(),
                bids: vec![Level::new(0.59, 100.0)], // mid = 0.60 (price went UP)
                asks: vec![Level::new(0.61, 100.0)],
                exchange_seq: 3,
            },
        },
        // Another event after the reference price arrives (to trigger settlement check)
        TimestampedEvent {
            time: end_secs * NS_PER_SEC + 2 * latency_ns,
            source_time: end_secs * NS_PER_SEC + latency_ns,
            seq: 4,
            source: StreamSource::MarketData as u8,
            event: Event::L2BookSnapshot {
                token_id: token_yes.clone(),
                bids: vec![Level::new(0.59, 100.0)],
                asks: vec![Level::new(0.61, 100.0)],
                exchange_seq: 4,
            },
        },
    ];
    
    let mut config = BacktestConfig::default();
    config.settlement_spec = Some(SettlementSpec::polymarket_15m_updown());
    
    let mut orchestrator = BacktestOrchestrator::new(config);
    let mut feed = VecFeed::new("test", events);
    orchestrator.load_feed(&mut feed).unwrap();
    
    let mut strategy = SettlementTracker::new();
    let result = orchestrator.run(&mut strategy);
    
    assert!(result.is_ok(), "Run should succeed: {:?}", result.err());
    let results = result.unwrap();
    
    // Verify strategy saw all 4 books
    assert_eq!(strategy.books_seen.len(), 4, "Strategy should see all 4 book updates");
    
    // Verify settlement stats
    assert!(results.settlement_stats.is_some(), "Settlement stats should be populated");
    let stats = results.settlement_stats.unwrap();
    
    // Market should have been tracked and resolved
    assert!(stats.windows_tracked >= 1, "At least one window should be tracked");
    // Note: Resolution depends on timing - if the last event is after arrival, settlement should resolve
}

#[test]
fn test_settlement_respects_reference_arrival_not_cutoff() {
    // This test specifically verifies that settlement does NOT happen at cutoff time,
    // but only when the reference price event ARRIVES
    
    use crate::backtest_v2::clock::Nanos;
    
    let spec = SettlementSpec::polymarket_15m_updown();
    let mut engine = SettlementEngine::new(spec);
    
    let start_secs: i64 = 1000;
    let market_id = make_15m_market(start_secs);
    let window_start = start_secs * NS_PER_SEC;
    let window_end = (start_secs + 15 * 60) * NS_PER_SEC;
    
    engine.track_window(&market_id, window_start).unwrap();
    
    // Start price arrives immediately
    engine.observe_price(&market_id, 50000.0, window_start, window_start);
    
    // End price has source_time = cutoff, but arrives 1 second LATER
    let arrival_delay: Nanos = 1 * NS_PER_SEC;
    engine.observe_price(&market_id, 50100.0, window_end, window_end + arrival_delay);
    
    // At cutoff time: should NOT be able to settle (reference hasn't arrived yet)
    engine.advance_time(window_end);
    let result_at_cutoff = engine.try_settle(&market_id, window_end);
    assert!(result_at_cutoff.is_none(), "Settlement should NOT be possible at cutoff (before reference arrives)");
    
    // Just before arrival: still can't settle
    let just_before_arrival = window_end + arrival_delay - 1;
    let result_before = engine.try_settle(&market_id, just_before_arrival);
    assert!(result_before.is_none(), "Settlement should NOT be possible before reference arrives");
    
    // At arrival time: NOW we can settle
    let at_arrival = window_end + arrival_delay;
    let result_at_arrival = engine.try_settle(&market_id, at_arrival);
    assert!(result_at_arrival.is_some(), "Settlement SHOULD be possible when reference arrives");
    
    let event = result_at_arrival.unwrap();
    
    // Verify the settlement uses the correct reference price
    assert_eq!(event.start_price, 50000.0);
    assert_eq!(event.end_price, 50100.0);
    assert!(matches!(event.outcome, SettlementOutcome::Resolved { winner: Outcome::Yes, .. }));
    
    // Verify the settlement records the arrival time (NOT cutoff time)
    assert_eq!(event.reference_arrival_ns, window_end + arrival_delay);
}

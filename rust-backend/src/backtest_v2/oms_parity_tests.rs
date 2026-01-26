//! OMS Parity Adversarial Tests
//!
//! These tests verify that the backtest OMS matches live trading semantics:
//! 1. Rate limits enforced identically
//! 2. Validation rules enforced identically
//! 3. Market status checks enforced
//! 4. Rejects and retries behave identically

use crate::backtest_v2::events::{Level, Side, TimestampedEvent, Event};
use crate::backtest_v2::feed::VecFeed;
use crate::backtest_v2::latency::NS_PER_MS;
use crate::backtest_v2::oms::VenueConstraints;
use crate::backtest_v2::orchestrator::{BacktestConfig, BacktestOrchestrator};
use crate::backtest_v2::sim_adapter::OmsParityMode;
use crate::backtest_v2::strategy::{
    BookSnapshot, CancelAck, FillNotification, OrderAck, OrderReject, Strategy,
    StrategyContext, StrategyOrder, TimerEvent, TradePrint,
};
use std::cell::RefCell;

/// Test strategy that attempts to abuse rate limits
struct RateLimitAbuseStrategy {
    name: String,
    token_id: String,
    orders_attempted: RefCell<u64>,
    orders_rejected: RefCell<u64>,
}

impl RateLimitAbuseStrategy {
    fn new(token_id: &str) -> Self {
        Self {
            name: "rate_limit_abuse".to_string(),
            token_id: token_id.to_string(),
            orders_attempted: RefCell::new(0),
            orders_rejected: RefCell::new(0),
        }
    }

    fn orders_attempted(&self) -> u64 {
        *self.orders_attempted.borrow()
    }

    fn orders_rejected(&self) -> u64 {
        *self.orders_rejected.borrow()
    }
}

impl Strategy for RateLimitAbuseStrategy {
    fn name(&self) -> &str {
        &self.name
    }

    fn on_book_update(&mut self, ctx: &mut StrategyContext, book: &BookSnapshot) {
        if book.token_id != self.token_id {
            return;
        }

        // Try to submit many orders in a single decision (rate limit abuse)
        for i in 0..20 {
            let order = StrategyOrder::limit(
                &format!("order_{}", i),
                &self.token_id,
                Side::Buy,
                0.50,
                10.0,
            );

            *self.orders_attempted.borrow_mut() += 1;
            if ctx.orders.send_order(order).is_err() {
                *self.orders_rejected.borrow_mut() += 1;
            }
        }
    }

    fn on_trade(&mut self, _ctx: &mut StrategyContext, _trade: &TradePrint) {}
    fn on_timer(&mut self, _ctx: &mut StrategyContext, _timer: &TimerEvent) {}
    fn on_order_ack(&mut self, _ctx: &mut StrategyContext, _ack: &OrderAck) {}
    fn on_order_reject(&mut self, _ctx: &mut StrategyContext, _reject: &OrderReject) {}
    fn on_fill(&mut self, _ctx: &mut StrategyContext, _fill: &FillNotification) {}
    fn on_cancel_ack(&mut self, _ctx: &mut StrategyContext, _ack: &CancelAck) {}
    fn on_start(&mut self, _ctx: &mut StrategyContext) {}
    fn on_stop(&mut self, _ctx: &mut StrategyContext) {}
}

/// Test strategy that attempts invalid orders
struct InvalidOrderStrategy {
    name: String,
    token_id: String,
    test_case: String,
    order_result: RefCell<Option<Result<u64, String>>>,
}

impl InvalidOrderStrategy {
    fn new(token_id: &str, test_case: &str) -> Self {
        Self {
            name: "invalid_order".to_string(),
            token_id: token_id.to_string(),
            test_case: test_case.to_string(),
            order_result: RefCell::new(None),
        }
    }

    fn order_result(&self) -> Option<Result<u64, String>> {
        self.order_result.borrow().clone()
    }
}

impl Strategy for InvalidOrderStrategy {
    fn name(&self) -> &str {
        &self.name
    }

    fn on_book_update(&mut self, ctx: &mut StrategyContext, book: &BookSnapshot) {
        if book.token_id != self.token_id || self.order_result.borrow().is_some() {
            return;
        }

        let order = match self.test_case.as_str() {
            "invalid_price_high" => {
                // Price above max (0.99)
                StrategyOrder::limit("test", &self.token_id, Side::Buy, 1.50, 10.0)
            }
            "invalid_price_low" => {
                // Price below min (0.01)
                StrategyOrder::limit("test", &self.token_id, Side::Buy, 0.001, 10.0)
            }
            "invalid_size_low" => {
                // Size below min (1.0)
                StrategyOrder::limit("test", &self.token_id, Side::Buy, 0.50, 0.1)
            }
            "invalid_size_high" => {
                // Size above max (100,000)
                StrategyOrder::limit("test", &self.token_id, Side::Buy, 0.50, 500_000.0)
            }
            "invalid_tick_size" => {
                // Price not on tick (0.01)
                StrategyOrder::limit("test", &self.token_id, Side::Buy, 0.501, 10.0)
            }
            _ => return,
        };

        let result = ctx.orders.send_order(order);
        *self.order_result.borrow_mut() = Some(result.map_err(|e| e.to_string()));
    }

    fn on_trade(&mut self, _ctx: &mut StrategyContext, _trade: &TradePrint) {}
    fn on_timer(&mut self, _ctx: &mut StrategyContext, _timer: &TimerEvent) {}
    fn on_order_ack(&mut self, _ctx: &mut StrategyContext, _ack: &OrderAck) {}
    fn on_order_reject(&mut self, _ctx: &mut StrategyContext, _reject: &OrderReject) {}
    fn on_fill(&mut self, _ctx: &mut StrategyContext, _fill: &FillNotification) {}
    fn on_cancel_ack(&mut self, _ctx: &mut StrategyContext, _ack: &CancelAck) {}
    fn on_start(&mut self, _ctx: &mut StrategyContext) {}
    fn on_stop(&mut self, _ctx: &mut StrategyContext) {}
}

fn make_book_snapshot(token_id: &str, time: i64, seq: u64) -> TimestampedEvent {
    TimestampedEvent {
        time,
        source_time: time,
        seq,
        source: 0,
        event: Event::L2BookSnapshot {
            token_id: token_id.to_string(),
            bids: vec![Level::new(0.49, 1000.0)],
            asks: vec![Level::new(0.51, 1000.0)],
            exchange_seq: seq,
        },
    }
}

// =============================================================================
// TEST 1: Rate limit enforcement in Full OMS parity mode
// =============================================================================

#[test]
fn test_rate_limit_abuse_full_mode_rejects() {
    // Use very strict rate limits
    let constraints = VenueConstraints {
        max_orders_per_second: 5,
        ..VenueConstraints::polymarket()
    };

    let events = vec![
        make_book_snapshot("TEST", 10 * NS_PER_MS, 1),
    ];
    let mut feed = VecFeed::new("test", events);

    let config = BacktestConfig {
        oms_parity_mode: OmsParityMode::Full,
        venue_constraints: constraints,
        ..BacktestConfig::test_config()
    };
    let mut orchestrator = BacktestOrchestrator::new(config);
    orchestrator.load_feed(&mut feed).unwrap();

    let mut strategy = RateLimitAbuseStrategy::new("TEST");
    let results = orchestrator.run(&mut strategy).unwrap();

    // Strategy tried 20 orders but rate limit is 5/second
    assert_eq!(strategy.orders_attempted(), 20);
    assert!(strategy.orders_rejected() > 0, "Rate limits should reject some orders");
    
    // OMS parity should show rate limited orders
    let oms_parity = results.oms_parity.unwrap();
    assert!(oms_parity.rate_limited_orders > 0);
}

// =============================================================================
// TEST 2: Rate limits NOT enforced in Bypass mode
// =============================================================================

#[test]
fn test_rate_limit_bypass_mode_allows_all() {
    let constraints = VenueConstraints {
        max_orders_per_second: 5,
        ..VenueConstraints::polymarket()
    };

    let events = vec![
        make_book_snapshot("TEST", 10 * NS_PER_MS, 1),
    ];
    let mut feed = VecFeed::new("test", events);

    let config = BacktestConfig {
        oms_parity_mode: OmsParityMode::Bypass,
        venue_constraints: constraints,
        ..BacktestConfig::test_config()
    };
    let mut orchestrator = BacktestOrchestrator::new(config);
    orchestrator.load_feed(&mut feed).unwrap();

    let mut strategy = RateLimitAbuseStrategy::new("TEST");
    let results = orchestrator.run(&mut strategy).unwrap();

    // All orders should go through in bypass mode
    assert_eq!(strategy.orders_attempted(), 20);
    assert_eq!(strategy.orders_rejected(), 0, "Bypass mode should not reject");
    
    // But results should be marked invalid for production
    let oms_parity = results.oms_parity.unwrap();
    assert!(!oms_parity.valid_for_production);
}

// =============================================================================
// TEST 3: Invalid price rejected in Full mode
// =============================================================================

#[test]
fn test_invalid_price_high_rejected() {
    let events = vec![make_book_snapshot("TEST", 10 * NS_PER_MS, 1)];
    let mut feed = VecFeed::new("test", events);

    let config = BacktestConfig {
        oms_parity_mode: OmsParityMode::Full,
        ..BacktestConfig::test_config()
    };
    let mut orchestrator = BacktestOrchestrator::new(config);
    orchestrator.load_feed(&mut feed).unwrap();

    let mut strategy = InvalidOrderStrategy::new("TEST", "invalid_price_high");
    let _ = orchestrator.run(&mut strategy);

    let result = strategy.order_result().expect("Order should have been attempted");
    assert!(result.is_err(), "Order with price > 0.99 should be rejected");
    assert!(result.unwrap_err().contains("validation failed"));
}

#[test]
fn test_invalid_price_low_rejected() {
    let events = vec![make_book_snapshot("TEST", 10 * NS_PER_MS, 1)];
    let mut feed = VecFeed::new("test", events);

    let config = BacktestConfig {
        oms_parity_mode: OmsParityMode::Full,
        ..BacktestConfig::test_config()
    };
    let mut orchestrator = BacktestOrchestrator::new(config);
    orchestrator.load_feed(&mut feed).unwrap();

    let mut strategy = InvalidOrderStrategy::new("TEST", "invalid_price_low");
    let _ = orchestrator.run(&mut strategy);

    let result = strategy.order_result().expect("Order should have been attempted");
    assert!(result.is_err(), "Order with price < 0.01 should be rejected");
}

// =============================================================================
// TEST 4: Invalid size rejected in Full mode
// =============================================================================

#[test]
fn test_invalid_size_low_rejected() {
    let events = vec![make_book_snapshot("TEST", 10 * NS_PER_MS, 1)];
    let mut feed = VecFeed::new("test", events);

    let config = BacktestConfig {
        oms_parity_mode: OmsParityMode::Full,
        ..BacktestConfig::test_config()
    };
    let mut orchestrator = BacktestOrchestrator::new(config);
    orchestrator.load_feed(&mut feed).unwrap();

    let mut strategy = InvalidOrderStrategy::new("TEST", "invalid_size_low");
    let _ = orchestrator.run(&mut strategy);

    let result = strategy.order_result().expect("Order should have been attempted");
    assert!(result.is_err(), "Order with size < 1.0 should be rejected");
}

#[test]
fn test_invalid_size_high_rejected() {
    let events = vec![make_book_snapshot("TEST", 10 * NS_PER_MS, 1)];
    let mut feed = VecFeed::new("test", events);

    let config = BacktestConfig {
        oms_parity_mode: OmsParityMode::Full,
        ..BacktestConfig::test_config()
    };
    let mut orchestrator = BacktestOrchestrator::new(config);
    orchestrator.load_feed(&mut feed).unwrap();

    let mut strategy = InvalidOrderStrategy::new("TEST", "invalid_size_high");
    let _ = orchestrator.run(&mut strategy);

    let result = strategy.order_result().expect("Order should have been attempted");
    assert!(result.is_err(), "Order with size > 100,000 should be rejected");
}

// =============================================================================
// TEST 5: OMS parity mode descriptions
// =============================================================================

#[test]
fn test_oms_parity_mode_descriptions() {
    assert!(OmsParityMode::Full.description().contains("production-grade"));
    assert!(OmsParityMode::Relaxed.description().contains("INVALID"));
    assert!(OmsParityMode::Bypass.description().contains("legacy"));
}

// =============================================================================
// TEST 6: OMS parity mode validity for production
// =============================================================================

#[test]
fn test_oms_parity_mode_validity() {
    assert!(OmsParityMode::Full.is_valid_for_production());
    assert!(!OmsParityMode::Relaxed.is_valid_for_production());
    assert!(!OmsParityMode::Bypass.is_valid_for_production());
}

// =============================================================================
// TEST 7: Relaxed mode allows but marks invalid
// =============================================================================

#[test]
fn test_relaxed_mode_allows_but_marks_invalid() {
    let events = vec![make_book_snapshot("TEST", 10 * NS_PER_MS, 1)];
    let mut feed = VecFeed::new("test", events);

    let config = BacktestConfig {
        oms_parity_mode: OmsParityMode::Relaxed,
        ..BacktestConfig::test_config()
    };
    let mut orchestrator = BacktestOrchestrator::new(config);
    orchestrator.load_feed(&mut feed).unwrap();

    let mut strategy = InvalidOrderStrategy::new("TEST", "invalid_price_high");
    let results = orchestrator.run(&mut strategy).unwrap();

    let result = strategy.order_result().expect("Order should have been attempted");
    assert!(result.is_ok(), "Relaxed mode should allow invalid orders");
    
    // But results should be marked invalid
    let oms_parity = results.oms_parity.unwrap();
    assert!(!oms_parity.valid_for_production);
    assert!(oms_parity.validation_failures > 0);
}

// =============================================================================
// TEST 8: BacktestResults contains OMS statistics
// =============================================================================

#[test]
fn test_results_contain_oms_stats() {
    let events = vec![make_book_snapshot("TEST", 10 * NS_PER_MS, 1)];
    let mut feed = VecFeed::new("test", events);

    let config = BacktestConfig::default();
    let mut orchestrator = BacktestOrchestrator::new(config);
    orchestrator.load_feed(&mut feed).unwrap();

    let mut strategy = RateLimitAbuseStrategy::new("TEST");
    let results = orchestrator.run(&mut strategy).unwrap();

    // OMS parity should be present
    assert!(results.oms_parity.is_some());
    
    let oms_parity = results.oms_parity.unwrap();
    assert!(oms_parity.oms_stats.is_some());
}

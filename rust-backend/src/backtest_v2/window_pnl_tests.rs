//! Window PnL Accounting Tests
//!
//! Tests for the per-15-minute window PnL accounting system.

use crate::backtest_v2::clock::Nanos;
use crate::backtest_v2::events::{Event, Level, Side, TimestampedEvent};
use crate::backtest_v2::example_strategy::MarketMakerStrategy;
use crate::backtest_v2::feed::VecFeed;
use crate::backtest_v2::ledger::{to_amount, from_amount, AMOUNT_SCALE, LedgerConfig};
use crate::backtest_v2::orchestrator::{BacktestConfig, BacktestOrchestrator};
use crate::backtest_v2::portfolio::Outcome;
use crate::backtest_v2::queue::StreamSource;
use crate::backtest_v2::settlement::{
    NS_PER_SEC, SettlementEvent, SettlementOutcome, SettlementSpec, WINDOW_15M_SECS,
};
use crate::backtest_v2::strategy::{
    BookSnapshot, CancelAck, FillNotification, OrderAck, OrderReject, Strategy,
    StrategyContext, StrategyParams, TimerEvent, TradePrint,
};
use crate::backtest_v2::window_pnl::{
    WindowAccountingEngine, WindowAccountingError, WindowPnL, WindowPnLSeries,
    align_to_window_start, parse_window_start_from_slug,
};

// =============================================================================
// UNIT TESTS
// =============================================================================

#[test]
fn test_window_pnl_creation() {
    let window = WindowPnL::new(1000 * NS_PER_SEC, "btc-updown-15m-1000".to_string());
    
    assert_eq!(window.window_start_ns, 1000 * NS_PER_SEC);
    assert_eq!(window.window_end_ns, (1000 + WINDOW_15M_SECS) * NS_PER_SEC);
    assert_eq!(window.gross_pnl, 0);
    assert_eq!(window.fees, 0);
    assert_eq!(window.net_pnl, 0);
    assert!(!window.is_finalized);
}

#[test]
fn test_window_pnl_add_fill_and_fee() {
    let mut window = WindowPnL::new(1000 * NS_PER_SEC, "btc-updown-15m-1000".to_string());
    
    // Add a taker fill
    let volume = to_amount(100.0 * 0.50); // 100 shares at $0.50 = $50
    let pnl_delta = to_amount(0.0); // No immediate PnL (realized at settlement)
    window.add_fill(1, volume, pnl_delta, false);
    
    assert_eq!(window.trades_count, 1);
    assert_eq!(window.taker_fills_count, 1);
    assert_eq!(window.maker_fills_count, 0);
    
    // Add a fee
    let fee = to_amount(0.25);
    window.add_fee(2, fee);
    
    assert_eq!(window.fees, fee);
    assert_eq!(window.net_pnl, -fee); // gross(0) - fees(0.25) = -0.25
}

#[test]
fn test_window_pnl_finalize_settlement() {
    let mut window = WindowPnL::new(1000 * NS_PER_SEC, "btc-updown-15m-1000".to_string());
    
    // Add some trading activity
    window.add_fill(1, to_amount(50.0), to_amount(5.0), true);
    window.add_fee(2, to_amount(0.25));
    
    // Create settlement event
    let settlement = SettlementEvent {
        market_id: "btc-updown-15m-1000".to_string(),
        window_start_ns: 1000 * NS_PER_SEC,
        window_end_ns: (1000 + WINDOW_15M_SECS) * NS_PER_SEC,
        outcome: SettlementOutcome::Resolved {
            winner: Outcome::Yes,
            is_tie: false,
        },
        start_price: 50000.0,
        end_price: 50100.0,
        settle_decision_time_ns: (1000 + WINDOW_15M_SECS + 10) * NS_PER_SEC,
        reference_arrival_ns: (1000 + WINDOW_15M_SECS + 5) * NS_PER_SEC,
    };
    
    // Settlement: held 10 winning shares, receive $10
    let settlement_cash = to_amount(10.0);
    
    window.finalize_settlement(&settlement, settlement_cash, settlement.settle_decision_time_ns);
    
    assert!(window.is_finalized);
    assert_eq!(window.settlement_transfer, settlement_cash);
    assert_eq!(window.start_price, Some(50000.0));
    assert_eq!(window.end_price, Some(50100.0));
    
    // net_pnl = gross(5) - fees(0.25) + settlement(10) = 14.75
    assert!((window.net_pnl_f64() - 14.75).abs() < 0.01);
}

#[test]
fn test_window_series_sum_invariant() {
    let mut series = WindowPnLSeries::new();
    
    let mut w1 = WindowPnL::new(1000 * NS_PER_SEC, "btc-updown-15m-1000".to_string());
    w1.gross_pnl = to_amount(10.0);
    w1.fees = to_amount(1.0);
    w1.settlement_transfer = to_amount(5.0);
    w1.recompute_net_pnl();
    w1.is_finalized = true;
    
    let mut w2 = WindowPnL::new(2000 * NS_PER_SEC, "btc-updown-15m-2000".to_string());
    w2.gross_pnl = to_amount(-5.0);
    w2.fees = to_amount(0.5);
    w2.settlement_transfer = to_amount(0.0);
    w2.recompute_net_pnl();
    w2.is_finalized = true;
    
    series.add_window(w1);
    series.add_window(w2);
    
    // Validate sum invariant
    assert!(series.validate_sum_invariant().is_ok());
    
    // Check totals
    assert!((series.total_gross_pnl_f64() - 5.0).abs() < 0.01); // 10 - 5 = 5
    assert!((series.total_fees_f64() - 1.5).abs() < 0.01); // 1 + 0.5 = 1.5
}

#[test]
fn test_window_series_ordering() {
    let mut series = WindowPnLSeries::new();
    
    let w1 = WindowPnL::new(1000 * NS_PER_SEC, "market1".to_string());
    let w2 = WindowPnL::new(2000 * NS_PER_SEC, "market2".to_string());
    
    series.add_window(w1);
    series.add_window(w2);
    
    assert_eq!(series.windows.len(), 2);
    assert!(series.windows[0].window_start_ns < series.windows[1].window_start_ns);
}

#[test]
#[should_panic(expected = "Windows must be added in order")]
fn test_window_series_rejects_out_of_order() {
    let mut series = WindowPnLSeries::new();
    
    let w1 = WindowPnL::new(2000 * NS_PER_SEC, "market1".to_string());
    let w2 = WindowPnL::new(1000 * NS_PER_SEC, "market2".to_string()); // Earlier!
    
    series.add_window(w1);
    series.add_window(w2); // Should panic
}

#[test]
fn test_parse_window_start_from_slug() {
    assert_eq!(
        parse_window_start_from_slug("btc-updown-15m-1700000000"),
        Some(1700000000 * NS_PER_SEC)
    );
    
    assert_eq!(
        parse_window_start_from_slug("eth-updown-15m-1234567890-yes"),
        Some(1234567890 * NS_PER_SEC)
    );
    
    assert_eq!(
        parse_window_start_from_slug("invalid-market-slug"),
        None
    );
}

#[test]
fn test_align_to_window_start() {
    // 15 minutes = 900 seconds = 900_000_000_000 ns
    let window_ns = WINDOW_15M_SECS * NS_PER_SEC;
    
    // Exactly at boundary
    assert_eq!(align_to_window_start(window_ns), window_ns);
    
    // Just after boundary
    assert_eq!(align_to_window_start(window_ns + 1), window_ns);
    
    // Just before next boundary
    assert_eq!(align_to_window_start(2 * window_ns - 1), window_ns);
    
    // At next boundary
    assert_eq!(align_to_window_start(2 * window_ns), 2 * window_ns);
}

#[test]
fn test_window_fingerprint_deterministic() {
    let mut w1 = WindowPnL::new(1000 * NS_PER_SEC, "btc-updown-15m-1000".to_string());
    w1.gross_pnl = to_amount(10.0);
    w1.fees = to_amount(1.0);
    w1.trades_count = 5;
    
    let mut w2 = WindowPnL::new(1000 * NS_PER_SEC, "btc-updown-15m-1000".to_string());
    w2.gross_pnl = to_amount(10.0);
    w2.fees = to_amount(1.0);
    w2.trades_count = 5;
    
    assert_eq!(w1.fingerprint_hash(), w2.fingerprint_hash());
}

#[test]
fn test_window_fingerprint_changes_on_different_values() {
    let mut w1 = WindowPnL::new(1000 * NS_PER_SEC, "btc-updown-15m-1000".to_string());
    w1.gross_pnl = to_amount(10.0);
    
    let mut w2 = WindowPnL::new(1000 * NS_PER_SEC, "btc-updown-15m-1000".to_string());
    w2.gross_pnl = to_amount(11.0); // Different!
    
    assert_ne!(w1.fingerprint_hash(), w2.fingerprint_hash());
}

#[test]
fn test_window_accounting_engine_basic() {
    let mut engine = WindowAccountingEngine::new(false);
    
    let window = engine.get_or_create_window("btc-updown-15m-1000", 1000 * NS_PER_SEC);
    assert_eq!(window.market_id, "btc-updown-15m-1000");
    assert_eq!(window.window_start_ns, 1000 * NS_PER_SEC);
}

#[test]
fn test_series_hash_changes_with_windows() {
    let mut series = WindowPnLSeries::new();
    let initial_hash = series.series_hash;
    
    let w1 = WindowPnL::new(1000 * NS_PER_SEC, "market1".to_string());
    series.add_window(w1);
    
    let after_one = series.series_hash;
    assert_ne!(initial_hash, after_one);
    
    let w2 = WindowPnL::new(2000 * NS_PER_SEC, "market2".to_string());
    series.add_window(w2);
    
    let after_two = series.series_hash;
    assert_ne!(after_one, after_two);
}

#[test]
fn test_empty_window_finalization() {
    let mut engine = WindowAccountingEngine::new(false);
    
    // Finalize a window with no trades
    let outcome = SettlementOutcome::Resolved {
        winner: Outcome::Yes,
        is_tie: false,
    };
    
    let window = engine.finalize_empty_window(
        "btc-updown-15m-1000",
        1000 * NS_PER_SEC,
        (1000 + WINDOW_15M_SECS) * NS_PER_SEC,
        50000.0,
        50100.0,
        outcome,
        (1000 + WINDOW_15M_SECS + 10) * NS_PER_SEC,
    );
    
    assert!(window.is_finalized);
    assert_eq!(window.trades_count, 0);
    assert_eq!(window.net_pnl, 0);
    assert!(matches!(window.outcome, Some(SettlementOutcome::Resolved { .. })));
}

#[test]
fn test_window_pnl_with_negative_pnl() {
    let mut window = WindowPnL::new(1000 * NS_PER_SEC, "btc-updown-15m-1000".to_string());
    
    // Add a losing trade
    window.add_fill(1, to_amount(50.0), to_amount(-10.0), false);
    window.add_fee(2, to_amount(0.50));
    
    // Settlement: held losing positions, get nothing back
    let settlement_cash = to_amount(0.0);
    
    let settlement = SettlementEvent {
        market_id: "btc-updown-15m-1000".to_string(),
        window_start_ns: 1000 * NS_PER_SEC,
        window_end_ns: (1000 + WINDOW_15M_SECS) * NS_PER_SEC,
        outcome: SettlementOutcome::Resolved {
            winner: Outcome::No,
            is_tie: false,
        },
        start_price: 50000.0,
        end_price: 49900.0,
        settle_decision_time_ns: (1000 + WINDOW_15M_SECS + 10) * NS_PER_SEC,
        reference_arrival_ns: (1000 + WINDOW_15M_SECS + 5) * NS_PER_SEC,
    };
    
    window.finalize_settlement(&settlement, settlement_cash, settlement.settle_decision_time_ns);
    
    // net_pnl = gross(-10) - fees(0.50) + settlement(0) = -10.50
    assert!((window.net_pnl_f64() - (-10.50)).abs() < 0.01);
}

// =============================================================================
// INTEGRATION TESTS
// =============================================================================

/// Simple strategy that does nothing (for testing window accounting with no trades)
struct NoopStrategy {
    name: String,
}

impl NoopStrategy {
    fn new() -> Self {
        Self {
            name: "NoopStrategy".to_string(),
        }
    }
}

impl Strategy for NoopStrategy {
    fn name(&self) -> &str { &self.name }
    fn on_start(&mut self, _ctx: &mut StrategyContext) {}
    fn on_stop(&mut self, _ctx: &mut StrategyContext) {}
    fn on_book_update(&mut self, _ctx: &mut StrategyContext, _book: &BookSnapshot) {}
    fn on_trade(&mut self, _ctx: &mut StrategyContext, _trade: &TradePrint) {}
    fn on_timer(&mut self, _ctx: &mut StrategyContext, _timer: &TimerEvent) {}
    fn on_order_ack(&mut self, _ctx: &mut StrategyContext, _ack: &OrderAck) {}
    fn on_order_reject(&mut self, _ctx: &mut StrategyContext, _reject: &OrderReject) {}
    fn on_fill(&mut self, _ctx: &mut StrategyContext, _fill: &FillNotification) {}
    fn on_cancel_ack(&mut self, _ctx: &mut StrategyContext, _cancel: &CancelAck) {}
}

fn make_book_event(time: Nanos, token_id: &str, mid: f64) -> TimestampedEvent {
    TimestampedEvent::new(
        time,
        StreamSource::MarketData as u8,
        Event::L2BookSnapshot {
            token_id: token_id.into(),
            bids: vec![Level::new(mid - 0.02, 100.0)],
            asks: vec![Level::new(mid + 0.02, 100.0)],
            exchange_seq: 1,
        },
    )
}

#[test]
fn test_window_pnl_in_results_when_configured() {
    // When settlement_spec and ledger_config are both configured,
    // window_pnl should be populated in results
    
    let mut config = BacktestConfig::test_config();
    config.settlement_spec = Some(SettlementSpec::polymarket_15m_updown());
    config.ledger_config = Some(LedgerConfig::production_grade());
    
    // Verify window accounting will be created
    assert!(config.settlement_spec.is_some());
    assert!(config.ledger_config.is_some());
    
    let mut orchestrator = BacktestOrchestrator::new(config.clone());
    
    // Empty run - no events
    let mut strategy = NoopStrategy::new();
    let result = orchestrator.run(&mut strategy);
    
    // Should succeed even with no events
    assert!(result.is_ok(), "Run should succeed: {:?}", result.err());
    
    let results = result.unwrap();
    
    // Window PnL should be populated (even if empty)
    assert!(results.window_pnl.is_some(), "window_pnl should be Some when configured");
    
    let window_series = results.window_pnl.unwrap();
    // With no settlement events, we should have an empty series
    assert_eq!(window_series.windows.len(), 0, "No windows should be finalized without settlement events");
}

#[test]
fn test_window_pnl_not_populated_without_ledger() {
    // When ledger_config is not configured, window_pnl should be None
    
    let mut config = BacktestConfig::test_config();
    config.settlement_spec = Some(SettlementSpec::polymarket_15m_updown());
    config.ledger_config = None;
    
    let mut orchestrator = BacktestOrchestrator::new(config.clone());
    
    let mut strategy = NoopStrategy::new();
    let result = orchestrator.run(&mut strategy);
    
    assert!(result.is_ok());
    let results = result.unwrap();
    
    // Window PnL should NOT be populated
    assert!(results.window_pnl.is_none(), "window_pnl should be None without ledger_config");
}

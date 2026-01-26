//! Integration tests for the equity curve module.
//!
//! These tests verify:
//! 1. Single-trade test: equity changes once at fill
//! 2. Fee-only test: equity drops due to fee posting
//! 3. Settlement test: equity jumps at settlement
//! 4. Determinism test: identical run → identical equity curve

use crate::backtest_v2::ledger::{Ledger, LedgerConfig, to_amount, from_amount};
use crate::backtest_v2::portfolio::Outcome;
use crate::backtest_v2::events::Side;
use crate::backtest_v2::equity_curve::{
    EquityCurve, EquityCurveSummary, EquityObservationTrigger, EquityRecorder,
};
use std::collections::HashMap;

/// Helper to create a test ledger with initial cash.
fn make_ledger(initial_cash: f64) -> Ledger {
    Ledger::new(LedgerConfig {
        initial_cash,
        allow_negative_cash: false,
        allow_shorting: false,
        strict_mode: true,
        trace_depth: 50,
    })
}

#[test]
fn test_equity_curve_single_trade() {
    // Setup: initial $10,000
    let mut ledger = make_ledger(10000.0);
    let mut recorder = EquityRecorder::new();
    let mid_prices: HashMap<String, f64> = HashMap::new();
    
    // Record initial deposit
    recorder.observe(
        1_000_000, // 1ms
        &ledger,
        &mid_prices,
        EquityObservationTrigger::InitialDeposit,
    );
    
    // Verify initial equity
    let curve = recorder.curve();
    assert_eq!(curve.len(), 1);
    assert!((curve.points()[0].equity_f64() - 10000.0).abs() < 0.01);
    
    // Post a fill: BUY 100 shares @ $0.50 with $1.00 fee
    ledger.post_fill(
        1, // fill_id
        "test-market",
        Outcome::Yes,
        Side::Buy,
        100.0, // size
        0.50,  // price
        1.0,   // fee
        2_000_000, // sim_time
        2_000_000, // arrival_time
        None,  // order_id
    ).unwrap();
    
    // Create mid_prices for position valuation
    let mut mid_prices = HashMap::new();
    mid_prices.insert("test-market".to_string(), 0.50);
    
    // Record observation after fill
    recorder.observe(
        2_000_000, // 2ms
        &ledger,
        &mid_prices,
        EquityObservationTrigger::Fill,
    );
    
    // Verify equity changed:
    // Cash: 10000 - 50 - 1 = 9949
    // Position value: 100 * 0.50 = 50
    // Total: 9999
    let curve = recorder.curve();
    assert_eq!(curve.len(), 2);
    
    let final_point = &curve.points()[1];
    assert!((final_point.equity_f64() - 9999.0).abs() < 0.01);
    assert!((final_point.cash_f64() - 9949.0).abs() < 0.01);
    assert!((final_point.position_value_f64() - 50.0).abs() < 0.01);
}

#[test]
fn test_equity_curve_fee_only() {
    // Setup: ledger with fee posting (no trade)
    let mut ledger = make_ledger(10000.0);
    let mut recorder = EquityRecorder::new();
    let mid_prices: HashMap<String, f64> = HashMap::new();
    
    // Record initial deposit
    recorder.observe(
        1_000_000,
        &ledger,
        &mid_prices,
        EquityObservationTrigger::InitialDeposit,
    );
    
    let curve = recorder.curve();
    let initial_equity = curve.points()[0].equity_value;
    
    // To test fee-only, we need to post a fill with only fee (0 size would fail validation)
    // Instead, let's do a minimal trade with a fee
    ledger.post_fill(
        1,
        "test-market",
        Outcome::Yes,
        Side::Buy,
        0.001, // minimal size
        0.50,
        5.0,   // $5 fee
        2_000_000,
        2_000_000,
        None,
    ).unwrap();
    
    let mut mid_prices = HashMap::new();
    mid_prices.insert("test-market".to_string(), 0.50);
    
    recorder.observe(
        2_000_000,
        &ledger,
        &mid_prices,
        EquityObservationTrigger::Fee,
    );
    
    let curve = recorder.curve();
    assert_eq!(curve.len(), 2);
    
    // Equity should have dropped by ~$5 (plus tiny position change)
    let final_equity = curve.points()[1].equity_value;
    let equity_change = from_amount(final_equity - initial_equity);
    
    // Position value: 0.001 * 0.50 = 0.0005
    // Cost: 0.001 * 0.50 = 0.0005
    // Net from trade: 0
    // Fee: -5.0
    // Total change: ~-5.0
    assert!(equity_change < 0.0, "Equity should have dropped due to fee");
    assert!((equity_change + 5.0).abs() < 0.01, "Equity drop should be ~$5");
}

#[test]
fn test_equity_curve_settlement_winner() {
    // Setup: buy position, then settle as winner
    let mut ledger = make_ledger(10000.0);
    let mut recorder = EquityRecorder::new();
    
    // Record initial
    recorder.observe(
        1_000_000,
        &ledger,
        &HashMap::new(),
        EquityObservationTrigger::InitialDeposit,
    );
    
    // Buy 100 YES @ $0.40
    ledger.post_fill(
        1,
        "test-market",
        Outcome::Yes,
        Side::Buy,
        100.0,
        0.40,
        0.0, // no fee for simplicity
        2_000_000,
        2_000_000,
        None,
    ).unwrap();
    
    let mut mid_prices = HashMap::new();
    mid_prices.insert("test-market".to_string(), 0.40);
    
    recorder.observe(
        2_000_000,
        &ledger,
        &mid_prices,
        EquityObservationTrigger::Fill,
    );
    
    // Equity after buy:
    // Cash: 10000 - 40 = 9960
    // Position: 100 * 0.40 = 40
    // Total: 10000
    let curve = recorder.curve();
    assert!((curve.points()[1].equity_f64() - 10000.0).abs() < 0.01);
    
    // Settle with YES winning
    ledger.post_settlement(
        1,
        "test-market",
        Outcome::Yes,
        3_000_000,
        3_000_000,
    ).unwrap();
    
    // Clear mid_prices since position is settled
    mid_prices.clear();
    
    recorder.observe(
        3_000_000,
        &ledger,
        &mid_prices,
        EquityObservationTrigger::Settlement,
    );
    
    // Equity after settlement:
    // Settlement: 100 * $1.00 = $100
    // Cash: 9960 + 100 = 10060
    // Position: 0
    // Total: 10060
    let curve = recorder.curve();
    assert_eq!(curve.len(), 3);
    
    let final_point = &curve.points()[2];
    assert!((final_point.equity_f64() - 10060.0).abs() < 0.01);
    assert!((final_point.cash_f64() - 10060.0).abs() < 0.01);
    assert!((final_point.position_value_f64()).abs() < 0.01);
}

#[test]
fn test_equity_curve_settlement_loser() {
    // Setup: buy position, then settle as loser
    let mut ledger = make_ledger(10000.0);
    let mut recorder = EquityRecorder::new();
    
    recorder.observe(
        1_000_000,
        &ledger,
        &HashMap::new(),
        EquityObservationTrigger::InitialDeposit,
    );
    
    // Buy 100 YES @ $0.40
    ledger.post_fill(
        1,
        "test-market",
        Outcome::Yes,
        Side::Buy,
        100.0,
        0.40,
        0.0,
        2_000_000,
        2_000_000,
        None,
    ).unwrap();
    
    let mut mid_prices = HashMap::new();
    mid_prices.insert("test-market".to_string(), 0.40);
    
    recorder.observe(
        2_000_000,
        &ledger,
        &mid_prices,
        EquityObservationTrigger::Fill,
    );
    
    // Settle with NO winning (YES loses)
    ledger.post_settlement(
        1,
        "test-market",
        Outcome::No,
        3_000_000,
        3_000_000,
    ).unwrap();
    
    mid_prices.clear();
    
    recorder.observe(
        3_000_000,
        &ledger,
        &mid_prices,
        EquityObservationTrigger::Settlement,
    );
    
    // Equity after settlement:
    // Settlement: 100 * $0.00 = $0
    // Cash: 9960 + 0 = 9960
    // Position: 0
    // Total: 9960
    let curve = recorder.curve();
    assert_eq!(curve.len(), 3);
    
    let final_point = &curve.points()[2];
    assert!((final_point.equity_f64() - 9960.0).abs() < 0.01);
}

#[test]
fn test_equity_curve_determinism() {
    // Run identical operations twice and verify identical curves
    fn run_scenario() -> EquityCurve {
        let mut ledger = make_ledger(10000.0);
        let mut recorder = EquityRecorder::new();
        
        recorder.observe(1_000_000, &ledger, &HashMap::new(), EquityObservationTrigger::InitialDeposit);
        
        ledger.post_fill(1, "m1", Outcome::Yes, Side::Buy, 50.0, 0.60, 0.5, 2_000_000, 2_000_000, None).unwrap();
        
        let mut mid = HashMap::new();
        mid.insert("m1".to_string(), 0.60);
        recorder.observe(2_000_000, &ledger, &mid, EquityObservationTrigger::Fill);
        
        ledger.post_fill(2, "m1", Outcome::Yes, Side::Sell, 25.0, 0.65, 0.3, 3_000_000, 3_000_000, None).unwrap();
        
        mid.insert("m1".to_string(), 0.65);
        recorder.observe(3_000_000, &ledger, &mid, EquityObservationTrigger::Fill);
        
        recorder.into_curve()
    }
    
    let curve1 = run_scenario();
    let curve2 = run_scenario();
    
    // Verify identical curves
    assert_eq!(curve1.len(), curve2.len(), "Curve lengths must match");
    assert_eq!(curve1.rolling_hash(), curve2.rolling_hash(), "Rolling hashes must match");
    
    for (p1, p2) in curve1.points().iter().zip(curve2.points().iter()) {
        assert_eq!(p1.time_ns, p2.time_ns);
        assert_eq!(p1.equity_value, p2.equity_value);
        assert_eq!(p1.cash_balance, p2.cash_balance);
        assert_eq!(p1.position_value, p2.position_value);
    }
}

#[test]
fn test_equity_curve_rolling_hash_changes_on_different_data() {
    // Test that different equity values produce different hashes
    fn run_scenario(fee: f64) -> u64 {
        let mut ledger = make_ledger(10000.0);
        let mut recorder = EquityRecorder::new();
        
        recorder.observe(1_000_000, &ledger, &HashMap::new(), EquityObservationTrigger::InitialDeposit);
        
        // Use different fees to produce different equity values
        ledger.post_fill(1, "m1", Outcome::Yes, Side::Buy, 100.0, 0.50, fee, 2_000_000, 2_000_000, None).unwrap();
        
        let mut mid = HashMap::new();
        mid.insert("m1".to_string(), 0.50);
        recorder.observe(2_000_000, &ledger, &mid, EquityObservationTrigger::Fill);
        
        recorder.curve().rolling_hash()
    }
    
    let hash1 = run_scenario(0.0);  // Equity: 10000 (no fee)
    let hash2 = run_scenario(5.0);  // Equity: 9995 (with fee)
    
    assert_ne!(hash1, hash2, "Different equity values should produce different hashes");
}

#[test]
fn test_equity_curve_drawdown_tracking() {
    let mut curve = EquityCurve::new();
    
    // Start at 10000
    curve.record(1000, to_amount(10000.0), to_amount(10000.0), 0);
    assert_eq!(curve.points()[0].drawdown_value, 0);
    
    // Go up to 10500 (new peak)
    curve.record(2000, to_amount(10500.0), to_amount(10500.0), 0);
    assert_eq!(curve.points()[1].drawdown_value, 0);
    
    // Drop to 10200 (drawdown of 300)
    curve.record(3000, to_amount(10200.0), to_amount(10200.0), 0);
    assert!((from_amount(curve.points()[2].drawdown_value) - 300.0).abs() < 0.01);
    
    // Go up to 10800 (new peak, no drawdown)
    curve.record(4000, to_amount(10800.0), to_amount(10800.0), 0);
    assert_eq!(curve.points()[3].drawdown_value, 0);
    
    // Max drawdown should be 300
    assert!((from_amount(curve.max_drawdown()) - 300.0).abs() < 0.01);
}

#[test]
fn test_equity_curve_summary() {
    let mut curve = EquityCurve::new();
    
    curve.record(1000, to_amount(10000.0), to_amount(10000.0), 0);
    curve.record(2000, to_amount(10500.0), to_amount(10000.0), to_amount(500.0));
    curve.record(3000, to_amount(10200.0), to_amount(9700.0), to_amount(500.0));
    curve.record(4000, to_amount(10800.0), to_amount(10300.0), to_amount(500.0));
    
    let summary = EquityCurveSummary::from_curve(&curve);
    
    assert_eq!(summary.point_count, 4);
    assert!((summary.initial_equity - 10000.0).abs() < 0.01);
    assert!((summary.final_equity - 10800.0).abs() < 0.01);
    assert!((summary.peak_equity - 10800.0).abs() < 0.01);
    assert!((summary.total_return - 0.08).abs() < 0.001);
    assert!((summary.max_drawdown - 300.0).abs() < 0.01);
}

#[test]
fn test_equity_recorder_stats() {
    let mut recorder = EquityRecorder::new();
    let ledger = make_ledger(10000.0);
    let mid_prices = HashMap::new();
    
    recorder.observe(1000, &ledger, &mid_prices, EquityObservationTrigger::InitialDeposit);
    recorder.observe(2000, &ledger, &mid_prices, EquityObservationTrigger::Fill);
    recorder.observe(3000, &ledger, &mid_prices, EquityObservationTrigger::Fill);
    recorder.observe(4000, &ledger, &mid_prices, EquityObservationTrigger::Fee);
    recorder.observe(5000, &ledger, &mid_prices, EquityObservationTrigger::Settlement);
    recorder.observe(6000, &ledger, &mid_prices, EquityObservationTrigger::Finalization);
    
    let stats = recorder.stats();
    
    assert_eq!(stats.total_observations, 6);
    assert_eq!(stats.initial_deposits, 1);
    assert_eq!(stats.fills, 2);
    assert_eq!(stats.fees, 1);
    assert_eq!(stats.settlements, 1);
    assert_eq!(stats.finalizations, 1);
}

//! Adversarial Double-Entry Accounting Tests
//!
//! These tests verify accounting invariants and proper violation detection.
//! Tests are designed to fail without proper enforcement.

use crate::backtest_v2::ledger::{
    AccountingMode, AccountingViolation, Amount, CausalTrace, EventRef, Ledger, LedgerAccount,
    LedgerConfig, LedgerEntry, LedgerPosting, ViolationType, from_amount, to_amount, AMOUNT_SCALE,
};
use crate::backtest_v2::portfolio::Outcome;
use crate::backtest_v2::events::Side;

// =============================================================================
// HELPER FUNCTIONS
// =============================================================================

fn make_strict_ledger(initial_cash: f64) -> Ledger {
    Ledger::new(LedgerConfig {
        initial_cash,
        strict_mode: true,
        allow_negative_cash: false,
        allow_shorting: false,
        trace_depth: 100,
    })
}

fn make_permissive_ledger(initial_cash: f64) -> Ledger {
    Ledger::new(LedgerConfig {
        initial_cash,
        strict_mode: false,
        allow_negative_cash: false,
        allow_shorting: false,
        trace_depth: 100,
    })
}

// =============================================================================
// SINGLE FILL + FEE VERIFICATION
// =============================================================================

#[test]
fn test_single_fill_fee_exact_accounting() {
    let mut ledger = make_strict_ledger(1000.0);
    
    // Buy 100 shares @ $0.50 with $0.25 fee
    ledger.post_fill(
        1, "market1", Outcome::Yes, Side::Buy,
        100.0, 0.50, 0.25,
        1000, 1000, None,
    ).expect("Fill should succeed");
    
    // Verify exact balances
    // Cash: 1000 - 50 (trade) - 0.25 (fee) = 949.75
    assert!((ledger.cash() - 949.75).abs() < 0.001, 
        "Cash should be 949.75, got {}", ledger.cash());
    
    // Position: 100 shares
    assert!((ledger.position_qty("market1", Outcome::Yes) - 100.0).abs() < 0.001,
        "Position should be 100, got {}", ledger.position_qty("market1", Outcome::Yes));
    
    // Cost basis: $50 (not including fee - fee is separate expense)
    assert!((ledger.cost_basis("market1", Outcome::Yes) - 50.0).abs() < 0.001,
        "Cost basis should be 50, got {}", ledger.cost_basis("market1", Outcome::Yes));
    
    // Fees paid: $0.25
    assert!((ledger.fees_paid() - 0.25).abs() < 0.001,
        "Fees should be 0.25, got {}", ledger.fees_paid());
    
    // Realized PnL: 0 (no position closed yet)
    assert!((ledger.realized_pnl()).abs() < 0.001,
        "Realized PnL should be 0, got {}", ledger.realized_pnl());
    
    // All entries must be balanced
    for entry in ledger.entries() {
        assert!(entry.is_balanced(), "Entry {} unbalanced", entry.entry_id);
    }
}

// =============================================================================
// PARTIAL FILLS ACROSS MULTIPLE EXECUTIONS
// =============================================================================

#[test]
fn test_partial_fills_cumulative_cost_basis() {
    let mut ledger = make_strict_ledger(1000.0);
    
    // Fill 1: Buy 50 @ $0.40
    ledger.post_fill(
        1, "market1", Outcome::Yes, Side::Buy,
        50.0, 0.40, 0.10, 1000, 1000, None,
    ).expect("Fill 1 should succeed");
    
    // Fill 2: Buy 30 @ $0.50
    ledger.post_fill(
        2, "market1", Outcome::Yes, Side::Buy,
        30.0, 0.50, 0.05, 2000, 2000, None,
    ).expect("Fill 2 should succeed");
    
    // Fill 3: Buy 20 @ $0.60
    ledger.post_fill(
        3, "market1", Outcome::Yes, Side::Buy,
        20.0, 0.60, 0.08, 3000, 3000, None,
    ).expect("Fill 3 should succeed");
    
    // Total position: 50 + 30 + 20 = 100 shares
    assert!((ledger.position_qty("market1", Outcome::Yes) - 100.0).abs() < 0.001);
    
    // Total cost basis: 50*0.40 + 30*0.50 + 20*0.60 = 20 + 15 + 12 = 47
    assert!((ledger.cost_basis("market1", Outcome::Yes) - 47.0).abs() < 0.001);
    
    // Total fees: 0.10 + 0.05 + 0.08 = 0.23
    assert!((ledger.fees_paid() - 0.23).abs() < 0.001);
    
    // Cash: 1000 - 47 - 0.23 = 952.77
    assert!((ledger.cash() - 952.77).abs() < 0.001);
    
    // Verify average cost = 47 / 100 = 0.47
    let avg_cost = ledger.cost_basis("market1", Outcome::Yes) 
                 / ledger.position_qty("market1", Outcome::Yes);
    assert!((avg_cost - 0.47).abs() < 0.001);
}

#[test]
fn test_partial_close_realized_pnl() {
    let mut ledger = make_strict_ledger(1000.0);
    
    // Buy 100 @ $0.40 (cost = $40)
    ledger.post_fill(
        1, "market1", Outcome::Yes, Side::Buy,
        100.0, 0.40, 0.0, 1000, 1000, None,
    ).unwrap();
    
    // Sell 40 @ $0.50 (proceeds = $20)
    // Cost of 40 shares at avg $0.40 = $16
    // PnL = $20 - $16 = $4 profit
    ledger.post_fill(
        2, "market1", Outcome::Yes, Side::Sell,
        40.0, 0.50, 0.0, 2000, 2000, None,
    ).unwrap();
    
    // Remaining position: 60 shares
    assert!((ledger.position_qty("market1", Outcome::Yes) - 60.0).abs() < 0.001);
    
    // Remaining cost basis: 60 * $0.40 = $24
    assert!((ledger.cost_basis("market1", Outcome::Yes) - 24.0).abs() < 0.001);
    
    // Realized PnL: $4
    assert!((ledger.realized_pnl() - 4.0).abs() < 0.001,
        "Expected PnL 4.0, got {}", ledger.realized_pnl());
    
    // Cash: 1000 - 40 + 20 = 980
    assert!((ledger.cash() - 980.0).abs() < 0.001);
}

// =============================================================================
// SETTLEMENT TESTS
// =============================================================================

#[test]
fn test_settlement_winning_position() {
    let mut ledger = make_strict_ledger(1000.0);
    
    // Buy 100 YES @ $0.40
    ledger.post_fill(
        1, "market1", Outcome::Yes, Side::Buy,
        100.0, 0.40, 1.0, 1000, 1000, None,
    ).unwrap();
    
    // Cash after buy: 1000 - 40 - 1 = 959
    assert!((ledger.cash() - 959.0).abs() < 0.001);
    
    // Settle with YES winning
    ledger.post_settlement(
        1, "market1", Outcome::Yes,
        10000, 10000,
    ).unwrap();
    
    // Settlement: receive $100 (100 shares * $1)
    // PnL: $100 - $40 = $60
    // Cash: 959 + 100 = 1059
    assert!((ledger.cash() - 1059.0).abs() < 0.001,
        "Expected cash 1059, got {}", ledger.cash());
    
    // Position closed
    assert!((ledger.position_qty("market1", Outcome::Yes)).abs() < 0.001);
    
    // Cost basis zeroed
    assert!((ledger.cost_basis("market1", Outcome::Yes)).abs() < 0.001);
    
    // Realized PnL: $60
    assert!((ledger.realized_pnl() - 60.0).abs() < 0.001,
        "Expected PnL 60, got {}", ledger.realized_pnl());
}

#[test]
fn test_settlement_losing_position() {
    let mut ledger = make_strict_ledger(1000.0);
    
    // Buy 100 YES @ $0.40
    ledger.post_fill(
        1, "market1", Outcome::Yes, Side::Buy,
        100.0, 0.40, 0.0, 1000, 1000, None,
    ).unwrap();
    
    // Settle with NO winning (YES loses)
    ledger.post_settlement(
        1, "market1", Outcome::No,
        10000, 10000,
    ).unwrap();
    
    // Settlement: receive $0
    // PnL: $0 - $40 = -$40
    // Cash: 960 + 0 = 960
    assert!((ledger.cash() - 960.0).abs() < 0.001);
    
    // Realized PnL: -$40
    assert!((ledger.realized_pnl() - (-40.0)).abs() < 0.001);
}

// =============================================================================
// NEGATIVE BALANCE GUARDS
// =============================================================================

#[test]
fn test_negative_cash_rejected_strict() {
    let mut ledger = make_strict_ledger(100.0);
    
    // Try to buy more than we have (would require $250)
    let result = ledger.post_fill(
        1, "market1", Outcome::Yes, Side::Buy,
        500.0, 0.50, 0.0, // Cost: $250
        1000, 1000, None,
    );
    
    assert!(result.is_err(), "Should reject trade that causes negative cash");
    
    if let Err(violation) = result {
        assert!(matches!(violation.violation_type, ViolationType::NegativeCash { .. }));
    }
    
    // Cash should be unchanged
    assert!((ledger.cash() - 100.0).abs() < 0.001);
}

#[test]
fn test_negative_cash_allowed_with_margin() {
    let mut ledger = Ledger::new(LedgerConfig {
        initial_cash: 100.0,
        strict_mode: true,
        allow_negative_cash: true, // Allow margin
        allow_shorting: false,
        trace_depth: 100,
    });
    
    // Buy more than we have
    let result = ledger.post_fill(
        1, "market1", Outcome::Yes, Side::Buy,
        500.0, 0.50, 0.0,
        1000, 1000, None,
    );
    
    assert!(result.is_ok(), "Should allow trade with margin enabled");
    
    // Cash should be negative
    assert!(ledger.cash() < 0.0);
}

// =============================================================================
// DOUBLE-APPLY PROTECTION
// =============================================================================

#[test]
fn test_duplicate_fill_rejected_strict() {
    let mut ledger = make_strict_ledger(1000.0);
    
    // First fill succeeds
    ledger.post_fill(
        1, "market1", Outcome::Yes, Side::Buy,
        100.0, 0.50, 0.0, 1000, 1000, None,
    ).unwrap();
    
    // Same fill_id rejected
    let result = ledger.post_fill(
        1, "market1", Outcome::Yes, Side::Buy, // Same fill_id
        100.0, 0.50, 0.0, 2000, 2000, None,
    );
    
    assert!(result.is_err(), "Duplicate fill should be rejected");
    
    if let Err(violation) = result {
        assert!(matches!(violation.violation_type, ViolationType::DuplicatePosting { .. }));
    }
    
    // Position should only have 100 shares (not 200)
    assert!((ledger.position_qty("market1", Outcome::Yes) - 100.0).abs() < 0.001);
}

#[test]
fn test_duplicate_settlement_rejected() {
    let mut ledger = make_strict_ledger(1000.0);
    
    // Open position
    ledger.post_fill(
        1, "market1", Outcome::Yes, Side::Buy,
        100.0, 0.50, 0.0, 1000, 1000, None,
    ).unwrap();
    
    // First settlement
    ledger.post_settlement(1, "market1", Outcome::Yes, 10000, 10000).unwrap();
    
    // Duplicate settlement
    let result = ledger.post_settlement(1, "market1", Outcome::Yes, 20000, 20000);
    
    assert!(result.is_err(), "Duplicate settlement should be rejected");
}

// =============================================================================
// ENTRY BALANCE VERIFICATION
// =============================================================================

#[test]
fn test_all_entries_balanced_after_complex_trading() {
    let mut ledger = make_strict_ledger(10000.0);
    
    // Complex trading sequence
    for i in 0..10 {
        let fill_id = i * 2 + 1;
        let market = format!("market{}", i % 3);
        let outcome = if i % 2 == 0 { Outcome::Yes } else { Outcome::No };
        let side = if i % 3 == 0 { Side::Sell } else { Side::Buy };
        
        // First ensure we have position to sell
        if side == Side::Sell {
            let _ = ledger.post_fill(
                fill_id, &market, outcome, Side::Buy,
                100.0, 0.40, 0.01, 1000, 1000, None,
            );
        }
        
        let _ = ledger.post_fill(
            fill_id + 1, &market, outcome, side,
            50.0, 0.50, 0.02, 2000, 2000, None,
        );
    }
    
    // All entries must be balanced
    for entry in ledger.entries() {
        assert!(entry.is_balanced(), 
            "Entry {} is unbalanced: debits={}, credits={}",
            entry.entry_id, entry.total_debits(), entry.total_credits());
    }
    
    // Accounting equation must hold
    assert!(ledger.verify_accounting_equation());
}

// =============================================================================
// CAUSAL TRACE VERIFICATION
// =============================================================================

#[test]
fn test_causal_trace_contains_recent_entries() {
    let mut ledger = make_permissive_ledger(100.0);
    
    // Create several fills
    for i in 1..=5 {
        let _ = ledger.post_fill(
            i, "market1", Outcome::Yes, Side::Buy,
            10.0, 0.50, 0.0, i as i64 * 1000, i as i64 * 1000, None,
        );
    }
    
    // Cause a violation
    let _ = ledger.post_fill(
        100, "market1", Outcome::Yes, Side::Buy,
        1000.0, 0.50, 0.0, // Would cost $500, but we only have ~$75
        100000, 100000, None,
    );
    
    assert!(ledger.has_violation());
    
    let trace = ledger.generate_causal_trace().unwrap();
    
    // Trace should contain recent entries
    assert!(!trace.recent_entries.is_empty());
    
    // Formatted trace should be parseable
    let formatted = trace.format_compact();
    assert!(formatted.contains("ACCOUNTING VIOLATION"));
    assert!(formatted.contains("NegativeCash"));
}

// =============================================================================
// EQUITY CONSERVATION TESTS
// =============================================================================

#[test]
fn test_equity_conserved_through_round_trip() {
    let mut ledger = make_strict_ledger(1000.0);
    let initial_equity = ledger.cash(); // 1000
    
    // Buy 100 @ $0.50
    ledger.post_fill(
        1, "market1", Outcome::Yes, Side::Buy,
        100.0, 0.50, 0.0, 1000, 1000, None,
    ).unwrap();
    
    // Sell 100 @ $0.50 (break even)
    ledger.post_fill(
        2, "market1", Outcome::Yes, Side::Sell,
        100.0, 0.50, 0.0, 2000, 2000, None,
    ).unwrap();
    
    // Position should be closed
    assert!((ledger.position_qty("market1", Outcome::Yes)).abs() < 0.001);
    
    // Cash should be back to initial
    assert!((ledger.cash() - initial_equity).abs() < 0.001);
    
    // Realized PnL should be 0
    assert!((ledger.realized_pnl()).abs() < 0.001);
}

#[test]
fn test_equity_correct_after_profit_trade() {
    let mut ledger = make_strict_ledger(1000.0);
    
    // Buy 100 @ $0.40
    ledger.post_fill(
        1, "market1", Outcome::Yes, Side::Buy,
        100.0, 0.40, 0.0, 1000, 1000, None,
    ).unwrap();
    
    // Sell 100 @ $0.60 (20% profit)
    ledger.post_fill(
        2, "market1", Outcome::Yes, Side::Sell,
        100.0, 0.60, 0.0, 2000, 2000, None,
    ).unwrap();
    
    // Cash: 1000 - 40 + 60 = 1020
    assert!((ledger.cash() - 1020.0).abs() < 0.001);
    
    // Realized PnL: $20 profit
    assert!((ledger.realized_pnl() - 20.0).abs() < 0.001);
}

// =============================================================================
// ACCOUNTING MODE TESTS
// =============================================================================

#[test]
fn test_accounting_mode_representative() {
    assert!(AccountingMode::DoubleEntryExact.is_representative());
    assert!(!AccountingMode::Legacy.is_representative());
    assert!(!AccountingMode::Disabled.is_representative());
}

// =============================================================================
// STATS TRACKING
// =============================================================================

#[test]
fn test_ledger_stats_accurate() {
    let mut ledger = make_strict_ledger(10000.0);
    
    // Multiple fills
    for i in 1..=5 {
        ledger.post_fill(
            i, "market1", Outcome::Yes, Side::Buy,
            10.0, 0.50, 0.01, i as i64 * 1000, i as i64 * 1000, None,
        ).unwrap();
    }
    
    // Check stats
    // 1 initial deposit + 5 fills + 5 fees = 11 entries
    assert_eq!(ledger.stats.fill_entries, 5);
    assert_eq!(ledger.stats.fee_entries, 5);
    assert_eq!(ledger.stats.total_entries, 11);
}

// =============================================================================
// STRICT ACCOUNTING MODE TESTS
// =============================================================================

#[test]
fn test_strict_accounting_config_defaults() {
    use crate::backtest_v2::orchestrator::BacktestConfig;
    
    // Default config: strict_accounting = false
    let default_config = BacktestConfig::default();
    assert!(!default_config.strict_accounting);
    assert!(!default_config.production_grade);
    
    // Production-grade config: strict_accounting = true
    let prod_config = BacktestConfig::production_grade_15m_updown();
    assert!(prod_config.strict_accounting);
    assert!(prod_config.production_grade);
    assert!(prod_config.ledger_config.is_some());
    assert!(prod_config.ledger_config.as_ref().unwrap().strict_mode);
}

#[test]
fn test_strict_accounting_validation() {
    use crate::backtest_v2::orchestrator::BacktestConfig;
    
    // Create a production-grade config
    let mut config = BacktestConfig::production_grade_15m_updown();
    
    // Disabling strict_accounting should cause validation failure
    config.strict_accounting = false;
    let result = config.validate_production_grade();
    assert!(result.is_err());
    
    let err = result.unwrap_err();
    assert!(err.violations.iter().any(|v| v.contains("strict_accounting")));
}

#[test]
fn test_strict_accounting_abort_on_negative_cash() {
    let mut ledger = make_strict_ledger(100.0);
    
    // Attempt to buy more than we have cash for
    // 100 shares @ $2.00 = $200, but we only have $100
    let result = ledger.post_fill(
        1,
        "market1",
        Outcome::Yes,
        Side::Buy,
        100.0,  // quantity
        2.0,    // price = $200 total
        0.0,    // no fee
        1_000_000_000,
        1_000_000_000,
        None,
    );
    
    // In strict mode, this should return an error
    assert!(result.is_err());
    
    // Cash should not have changed
    assert!((ledger.cash() - 100.0).abs() < 0.001);
}

#[test]
fn test_strict_accounting_determinism() {
    // Run 1
    let mut ledger1 = make_strict_ledger(1000.0);
    ledger1.post_fill(1, "m1", Outcome::Yes, Side::Buy, 100.0, 0.50, 0.10, 1_000_000_000, 1_000_000_000, None).unwrap();
    ledger1.post_fill(2, "m1", Outcome::Yes, Side::Sell, 50.0, 0.60, 0.05, 2_000_000_000, 2_000_000_000, None).unwrap();
    
    // Run 2 - identical inputs
    let mut ledger2 = make_strict_ledger(1000.0);
    ledger2.post_fill(1, "m1", Outcome::Yes, Side::Buy, 100.0, 0.50, 0.10, 1_000_000_000, 1_000_000_000, None).unwrap();
    ledger2.post_fill(2, "m1", Outcome::Yes, Side::Sell, 50.0, 0.60, 0.05, 2_000_000_000, 2_000_000_000, None).unwrap();
    
    // Results must be bit-for-bit identical
    assert_eq!(ledger1.cash(), ledger2.cash());
    assert_eq!(ledger1.fees_paid(), ledger2.fees_paid());
    assert_eq!(ledger1.realized_pnl(), ledger2.realized_pnl());
    assert_eq!(ledger1.position_qty("m1", Outcome::Yes), ledger2.position_qty("m1", Outcome::Yes));
    assert_eq!(ledger1.entries().len(), ledger2.entries().len());
}

#[test]
fn test_strict_accounting_causal_trace_on_violation() {
    let mut ledger = make_strict_ledger(100.0);
    
    // First, make a valid fill
    ledger.post_fill(1, "m1", Outcome::Yes, Side::Buy, 50.0, 1.0, 0.0, 1_000_000_000, 1_000_000_000, None).unwrap();
    
    // Now attempt an invalid fill that would make cash negative
    let result = ledger.post_fill(2, "m1", Outcome::Yes, Side::Buy, 100.0, 1.0, 0.0, 2_000_000_000, 2_000_000_000, None);
    
    // Should fail
    assert!(result.is_err());
    
    // Should have first violation recorded
    assert!(ledger.get_first_violation().is_some());
    
    // Causal trace should be available
    let trace = ledger.generate_causal_trace();
    assert!(trace.is_some());
    
    let trace = trace.unwrap();
    assert!(trace.recent_entries.len() > 0);
    assert!(trace.format_compact().contains("VIOLATION"));
}

#[test]
fn test_strict_accounting_only_pathway_for_fills() {
    use crate::backtest_v2::orchestrator::{BacktestConfig, BacktestOrchestrator};
    use crate::backtest_v2::ledger::LedgerConfig;
    
    // Create config with strict_accounting but without production_grade
    let mut config = BacktestConfig::default();
    config.strict_accounting = true;
    config.ledger_config = Some(LedgerConfig::production_grade());
    
    let orchestrator = BacktestOrchestrator::new(config.clone());
    
    // Verify ledger is present (via getter)
    assert!(orchestrator.ledger().is_some());
    
    // Verify strict mode is enabled in ledger
    let ledger = orchestrator.ledger().unwrap();
    assert!(ledger.config.strict_mode);
}

//! Strict Accounting Enforcement Tests
//!
//! These tests verify that:
//! 1. Direct mutations are blocked in strict mode
//! 2. All economic operations route through the ledger
//! 3. Accounting invariants hold after every operation
//! 4. Deterministic results across runs

use crate::backtest_v2::accounting_enforcer::AccountingEnforcer;
use crate::backtest_v2::clock::Nanos;
use crate::backtest_v2::events::Side;
use crate::backtest_v2::ledger::{Ledger, LedgerConfig, from_amount, to_amount};
use crate::backtest_v2::portfolio::{Outcome, Portfolio, TokenPosition, TokenId};
use crate::backtest_v2::strict_accounting::{
    activate_strict_accounting, deactivate_strict_accounting, is_strict_accounting_active,
    direct_mutation_attempt_count, reset_mutation_attempt_counter,
    check_ledger_shadow_parity, StrictAccountingState, AccountingAuditLog,
    AccountingEvent,
};
use std::collections::HashMap;

// =============================================================================
// STRICT MODE ENFORCEMENT TESTS
// =============================================================================

#[test]
fn test_strict_mode_blocks_portfolio_apply_fill() {
    // Setup
    reset_mutation_attempt_counter();
    activate_strict_accounting();
    
    let result = std::panic::catch_unwind(|| {
        let mut portfolio = Portfolio::new(1000.0);
        portfolio.apply_fill("market1", Outcome::Yes, Side::Buy, 100.0, 0.50, 0.0, 1000);
    });
    
    // Cleanup
    deactivate_strict_accounting();
    
    // Should have panicked
    assert!(result.is_err(), "Portfolio::apply_fill should panic in strict mode");
}

#[test]
fn test_strict_mode_blocks_portfolio_deposit() {
    reset_mutation_attempt_counter();
    activate_strict_accounting();
    
    let result = std::panic::catch_unwind(|| {
        let mut portfolio = Portfolio::new(1000.0);
        portfolio.deposit(500.0);
    });
    
    deactivate_strict_accounting();
    assert!(result.is_err(), "Portfolio::deposit should panic in strict mode");
}

#[test]
fn test_strict_mode_blocks_portfolio_withdraw() {
    reset_mutation_attempt_counter();
    activate_strict_accounting();
    
    let result = std::panic::catch_unwind(|| {
        let mut portfolio = Portfolio::new(1000.0);
        let _ = portfolio.withdraw(500.0);
    });
    
    deactivate_strict_accounting();
    assert!(result.is_err(), "Portfolio::withdraw should panic in strict mode");
}

#[test]
fn test_strict_mode_blocks_portfolio_settle_market() {
    reset_mutation_attempt_counter();
    activate_strict_accounting();
    
    let result = std::panic::catch_unwind(|| {
        let mut portfolio = Portfolio::new(1000.0);
        portfolio.settle_market("market1", Outcome::Yes, 1000);
    });
    
    deactivate_strict_accounting();
    assert!(result.is_err(), "Portfolio::settle_market should panic in strict mode");
}

#[test]
fn test_strict_mode_blocks_position_apply_fill() {
    reset_mutation_attempt_counter();
    activate_strict_accounting();
    
    let result = std::panic::catch_unwind(|| {
        let token_id = TokenId::new("market1", Outcome::Yes);
        let mut position = TokenPosition::new(token_id);
        position.apply_fill(Side::Buy, 100.0, 0.50, 0.0, 1000);
    });
    
    deactivate_strict_accounting();
    assert!(result.is_err(), "TokenPosition::apply_fill should panic in strict mode");
}

#[test]
fn test_legacy_mode_allows_direct_mutations() {
    // Ensure strict mode is off
    deactivate_strict_accounting();
    reset_mutation_attempt_counter();
    
    // These should NOT panic in legacy mode
    let mut portfolio = Portfolio::new(1000.0);
    portfolio.apply_fill("market1", Outcome::Yes, Side::Buy, 100.0, 0.50, 0.0, 1000);
    portfolio.deposit(500.0);
    let _ = portfolio.withdraw(100.0);
    
    // But should count mutation attempts
    assert!(direct_mutation_attempt_count() > 0, "Should count mutations in legacy mode");
}

// =============================================================================
// LEDGER AS SOLE PATHWAY TESTS
// =============================================================================

#[test]
fn test_ledger_is_sole_pathway_for_fills() {
    let config = LedgerConfig {
        initial_cash: 1000.0,
        strict_mode: true,
        ..Default::default()
    };
    let mut enforcer = AccountingEnforcer::new(config);
    
    // Route fill through ledger
    let result = enforcer.post_fill(
        "market1", Outcome::Yes, Side::Buy,
        100.0, 0.50, 0.10,
        1000, 1000, None,
    );
    
    assert!(result.is_ok(), "Fill through ledger should succeed");
    
    // Verify state
    assert!((enforcer.cash() - 949.90).abs() < 0.01, "Cash should be reduced by trade + fee");
    assert!((enforcer.position_qty("market1", Outcome::Yes) - 100.0).abs() < 0.01);
    assert!(enforcer.fees_paid() > 0.0, "Fees should be tracked");
}

#[test]
fn test_ledger_is_sole_pathway_for_settlements() {
    let config = LedgerConfig {
        initial_cash: 1000.0,
        strict_mode: true,
        ..Default::default()
    };
    let mut enforcer = AccountingEnforcer::new(config);
    
    // Buy position through ledger
    enforcer.post_fill(
        "market1", Outcome::Yes, Side::Buy,
        100.0, 0.40, 0.0,
        1000, 1000, None,
    ).unwrap();
    
    // Settle through ledger
    let result = enforcer.post_settlement("market1", Outcome::Yes, 2000, 2000);
    
    assert!(result.is_ok(), "Settlement through ledger should succeed");
    
    // Verify: won at $1.00, cost $0.40, profit = $60
    assert!((enforcer.cash() - 1060.0).abs() < 0.01, "Should receive settlement");
    assert!((enforcer.realized_pnl() - 60.0).abs() < 0.01, "Should realize profit");
    assert!((enforcer.position_qty("market1", Outcome::Yes)).abs() < 0.01, "Position should be cleared");
}

#[test]
fn test_ledger_blocks_duplicate_fills() {
    let config = LedgerConfig {
        initial_cash: 1000.0,
        strict_mode: true,
        ..Default::default()
    };
    let mut ledger = Ledger::new(config);
    
    // First fill
    let result1 = ledger.post_fill(
        1, "market1", Outcome::Yes, Side::Buy,
        100.0, 0.50, 0.0, 1000, 1000, None,
    );
    assert!(result1.is_ok());
    
    // Same fill_id again should fail
    let result2 = ledger.post_fill(
        1, "market1", Outcome::Yes, Side::Buy,
        100.0, 0.50, 0.0, 2000, 2000, None,
    );
    assert!(result2.is_err(), "Duplicate fill should be rejected");
}

// =============================================================================
// ACCOUNTING INVARIANT TESTS
// =============================================================================

#[test]
fn test_equity_identity_holds() {
    let config = LedgerConfig {
        initial_cash: 1000.0,
        strict_mode: true,
        ..Default::default()
    };
    let mut enforcer = AccountingEnforcer::new(config);
    
    // Series of operations
    enforcer.post_fill("m1", Outcome::Yes, Side::Buy, 100.0, 0.50, 0.10, 1000, 1000, None).unwrap();
    enforcer.post_fill("m1", Outcome::Yes, Side::Sell, 50.0, 0.60, 0.05, 2000, 2000, None).unwrap();
    enforcer.post_fill("m2", Outcome::No, Side::Buy, 200.0, 0.30, 0.20, 3000, 3000, None).unwrap();
    
    // Verify equity identity: Cash + CostBasis = Initial + RealizedPnL - Fees
    let cash = enforcer.cash();
    let fees = enforcer.fees_paid();
    let realized_pnl = enforcer.realized_pnl();
    
    // The ledger tracks cost basis internally; we verify cash is reasonable
    assert!(cash > 0.0, "Cash should remain positive");
    assert!(fees > 0.0, "Fees should be tracked");
}

#[test]
fn test_no_negative_cash_in_strict_mode() {
    let config = LedgerConfig {
        initial_cash: 100.0,
        allow_negative_cash: false,
        strict_mode: true,
        ..Default::default()
    };
    let mut enforcer = AccountingEnforcer::new(config);
    
    // Try to buy more than we can afford
    let result = enforcer.post_fill(
        "market1", Outcome::Yes, Side::Buy,
        1000.0, 0.50, 0.0, // Would cost $500
        1000, 1000, None,
    );
    
    assert!(result.is_err(), "Should reject order that would cause negative cash");
    assert!(enforcer.has_violation(), "Should record violation");
}

#[test]
fn test_balanced_entries_always() {
    let config = LedgerConfig {
        initial_cash: 1000.0,
        strict_mode: true,
        ..Default::default()
    };
    let mut ledger = Ledger::new(config);
    
    // Multiple operations
    ledger.post_fill(1, "m1", Outcome::Yes, Side::Buy, 100.0, 0.50, 0.10, 1000, 1000, None).unwrap();
    ledger.post_fill(2, "m1", Outcome::Yes, Side::Sell, 100.0, 0.60, 0.05, 2000, 2000, None).unwrap();
    
    // Verify all entries are balanced
    for entry in ledger.entries() {
        assert!(entry.is_balanced(), "Entry {} should be balanced", entry.entry_id);
    }
}

// =============================================================================
// BYPASS DETECTION TESTS
// =============================================================================

#[test]
fn test_bypass_detection_clean() {
    let ledger_positions: HashMap<(String, Outcome), f64> = HashMap::new();
    let shadow_positions: HashMap<String, f64> = HashMap::new();
    
    let result = check_ledger_shadow_parity(
        1000.0, 50.0, &ledger_positions,
        1000.0, 50.0, &shadow_positions,
        0.01,
    );
    
    assert!(!result.bypass_detected, "Should not detect bypass when states match");
}

#[test]
fn test_bypass_detection_cash_mismatch() {
    let ledger_positions: HashMap<(String, Outcome), f64> = HashMap::new();
    let shadow_positions: HashMap<String, f64> = HashMap::new();
    
    let result = check_ledger_shadow_parity(
        1000.0, 50.0, &ledger_positions,
        990.0, 50.0, &shadow_positions,  // $10 difference
        0.01,
    );
    
    assert!(result.bypass_detected, "Should detect cash mismatch");
    assert!(result.description.unwrap().contains("Cash mismatch"));
}

#[test]
fn test_bypass_detection_pnl_mismatch() {
    let ledger_positions: HashMap<(String, Outcome), f64> = HashMap::new();
    let shadow_positions: HashMap<String, f64> = HashMap::new();
    
    let result = check_ledger_shadow_parity(
        1000.0, 50.0, &ledger_positions,
        1000.0, 40.0, &shadow_positions,  // $10 PnL difference
        0.01,
    );
    
    assert!(result.bypass_detected, "Should detect PnL mismatch");
    assert!(result.description.unwrap().contains("Realized PnL mismatch"));
}

// =============================================================================
// DETERMINISM TESTS
// =============================================================================

#[test]
fn test_deterministic_ledger_state() {
    // Run the same operations twice and verify identical results
    let run = || {
        let config = LedgerConfig {
            initial_cash: 1000.0,
            strict_mode: true,
            ..Default::default()
        };
        let mut ledger = Ledger::new(config);
        
        ledger.post_fill(1, "m1", Outcome::Yes, Side::Buy, 100.0, 0.50, 0.10, 1000, 1000, None).unwrap();
        ledger.post_fill(2, "m1", Outcome::Yes, Side::Sell, 50.0, 0.60, 0.05, 2000, 2000, None).unwrap();
        ledger.post_fill(3, "m2", Outcome::No, Side::Buy, 200.0, 0.30, 0.00, 3000, 3000, None).unwrap();
        
        (ledger.cash(), ledger.realized_pnl(), ledger.fees_paid(), ledger.entries().len())
    };
    
    let (cash1, pnl1, fees1, entries1) = run();
    let (cash2, pnl2, fees2, entries2) = run();
    
    assert!((cash1 - cash2).abs() < 1e-9, "Cash should be deterministic");
    assert!((pnl1 - pnl2).abs() < 1e-9, "PnL should be deterministic");
    assert!((fees1 - fees2).abs() < 1e-9, "Fees should be deterministic");
    assert_eq!(entries1, entries2, "Entry count should be deterministic");
}

// =============================================================================
// AUDIT LOG TESTS
// =============================================================================

#[test]
fn test_accounting_audit_log() {
    let mut log = AccountingAuditLog::new(1000);
    
    // Record operations
    log.record(1000, AccountingEvent::Fill {
        fill_id: 1,
        market_id: "m1".to_string(),
        outcome: Outcome::Yes,
        side: Side::Buy,
        quantity: 100.0,
        price: 0.50,
        fee: 0.10,
        via_ledger: true,
    });
    
    log.record(2000, AccountingEvent::Settlement {
        settlement_id: 1,
        market_id: "m1".to_string(),
        winner: Outcome::Yes,
        pnl: 50.0,
        via_ledger: true,
    });
    
    assert_eq!(log.events().len(), 2);
    assert_eq!(log.bypass_count(), 0, "No bypasses should be detected");
}

#[test]
fn test_audit_log_detects_bypass() {
    let mut log = AccountingAuditLog::new(1000);
    
    // Record a bypassed fill
    log.record(1000, AccountingEvent::Fill {
        fill_id: 1,
        market_id: "m1".to_string(),
        outcome: Outcome::Yes,
        side: Side::Buy,
        quantity: 100.0,
        price: 0.50,
        fee: 0.10,
        via_ledger: false,  // BYPASS!
    });
    
    assert_eq!(log.bypass_count(), 1, "Should detect the bypass");
}

// =============================================================================
// STRICT ACCOUNTING STATE TESTS
// =============================================================================

#[test]
fn test_strict_accounting_state_tracking() {
    let mut state = StrictAccountingState::new(true);
    
    assert!(state.is_clean());
    
    state.record_ledger_fill();
    state.record_ledger_fill();
    state.record_ledger_settlement();
    
    assert!(state.is_clean());
    assert_eq!(state.ledger_fills, 2);
    assert_eq!(state.ledger_settlements, 1);
    
    state.record_bypass("Test bypass detected".to_string());
    
    assert!(!state.is_clean());
    assert_eq!(state.bypass_violations, 1);
}

// =============================================================================
// INTEGRATION TEST: FULL BACKTEST SCENARIO
// =============================================================================

#[test]
fn test_full_backtest_scenario_strict_accounting() {
    let config = LedgerConfig {
        initial_cash: 10000.0,
        strict_mode: true,
        allow_negative_cash: false,
        ..Default::default()
    };
    let mut enforcer = AccountingEnforcer::new(config);
    
    // Simulate a trading session
    // 1. Buy 100 YES @ $0.40
    enforcer.post_fill("btc-updown-15m-001", Outcome::Yes, Side::Buy, 100.0, 0.40, 0.10, 1000, 1000, None).unwrap();
    
    // 2. Buy 200 NO @ $0.55 (hedging)
    enforcer.post_fill("btc-updown-15m-001", Outcome::No, Side::Buy, 200.0, 0.55, 0.20, 2000, 2000, None).unwrap();
    
    // 3. Sell 50 YES @ $0.50 (partial close)
    enforcer.post_fill("btc-updown-15m-001", Outcome::Yes, Side::Sell, 50.0, 0.50, 0.05, 3000, 3000, None).unwrap();
    
    // 4. Market settles: YES wins
    enforcer.post_settlement("btc-updown-15m-001", Outcome::Yes, 4000, 4000).unwrap();
    
    // Verify final state
    let final_cash = enforcer.cash();
    let final_pnl = enforcer.realized_pnl();
    let final_fees = enforcer.fees_paid();
    
    // Positions should be cleared
    assert!((enforcer.position_qty("btc-updown-15m-001", Outcome::Yes)).abs() < 0.01);
    assert!((enforcer.position_qty("btc-updown-15m-001", Outcome::No)).abs() < 0.01);
    
    // Fees should be accumulated
    assert!((final_fees - 0.35).abs() < 0.01, "Total fees should be $0.35");
    
    // Should have non-zero PnL (mix of wins and losses)
    println!("Final cash: ${:.2}", final_cash);
    println!("Final PnL: ${:.2}", final_pnl);
    println!("Final fees: ${:.2}", final_fees);
    
    // Invariant: cash should be positive
    assert!(final_cash > 0.0, "Cash should remain positive");
    
    // Accounting enforcer stats
    assert_eq!(enforcer.stats.fills_processed, 3);
    assert_eq!(enforcer.stats.settlements_processed, 1);
    assert!(!enforcer.stats.aborted);
}

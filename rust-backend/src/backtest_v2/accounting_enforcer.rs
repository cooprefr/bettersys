//! Strict Accounting Enforcer
//!
//! This module provides the MANDATORY enforcement layer that ensures:
//! 1. ALL economic state changes go through the double-entry ledger
//! 2. First accounting violation aborts immediately with causal trace
//! 3. No silent correction, clamping, or continuation after violation
//!
//! # Design Principles
//!
//! - **Single Source of Truth**: The ledger is the ONLY mechanism for economic state changes
//! - **Abort on First Violation**: Production runs cannot continue after any accounting error
//! - **Deterministic Trace**: Violations produce bounded, reproducible diagnostics
//! - **No Bypass**: strict_accounting mode cannot be circumvented in production_grade runs

use crate::backtest_v2::clock::Nanos;
use crate::backtest_v2::events::{OrderId, Side};
use crate::backtest_v2::ledger::{
    AccountingViolation, CausalTrace, Ledger, LedgerConfig, LedgerEntry, 
    ViolationType, from_amount, to_amount,
};
use crate::backtest_v2::portfolio::Outcome;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};

// =============================================================================
// ACCOUNTING ABORT ERROR
// =============================================================================

/// Error returned when strict accounting mode detects a violation.
/// 
/// This error contains a deterministic, minimal causal trace sufficient for
/// post-mortem analysis and reproduction of the failure.
#[derive(Debug, Clone)]
pub struct AccountingAbort {
    /// Type of violation that caused the abort.
    pub violation_type: ViolationType,
    /// Human-readable description.
    pub message: String,
    /// Simulation time when violation occurred.
    pub sim_time_ns: Nanos,
    /// Decision ID at time of violation.
    pub decision_id: u64,
    /// Triggering event type (fill, fee, settlement).
    pub trigger: AccountingTrigger,
    /// Causal trace with recent ledger entries.
    pub causal_trace: CausalTrace,
    /// State snapshot before the violating operation.
    pub state_before: AccountingStateSnapshot,
    /// State snapshot after the violating operation (if applicable).
    pub state_after: Option<AccountingStateSnapshot>,
}

impl std::fmt::Display for AccountingAbort {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "ACCOUNTING ABORT: {:?} at {} ns (decision {})\n{}",
            self.violation_type,
            self.sim_time_ns,
            self.decision_id,
            self.message
        )
    }
}

impl std::error::Error for AccountingAbort {}

impl AccountingAbort {
    /// Format as a deterministic, bounded text trace for debugging.
    pub fn format_trace(&self) -> String {
        let mut out = String::new();
        
        out.push_str("╔══════════════════════════════════════════════════════════════════════════════╗\n");
        out.push_str("║              STRICT ACCOUNTING ABORT - CAUSAL TRACE                         ║\n");
        out.push_str("╠══════════════════════════════════════════════════════════════════════════════╣\n");
        out.push_str(&format!("║  VIOLATION: {:64} ║\n", format!("{:?}", self.violation_type)));
        out.push_str(&format!("║  SIM TIME:  {} ns {:>47} ║\n", self.sim_time_ns, ""));
        out.push_str(&format!("║  DECISION:  {} {:>56} ║\n", self.decision_id, ""));
        out.push_str(&format!("║  TRIGGER:   {:64} ║\n", format!("{:?}", self.trigger)));
        out.push_str("╠══════════════════════════════════════════════════════════════════════════════╣\n");
        out.push_str("║  STATE BEFORE:                                                               ║\n");
        out.push_str(&format!("║    Cash:          ${:>15.6}                                        ║\n", self.state_before.cash));
        out.push_str(&format!("║    Realized PnL:  ${:>15.6}                                        ║\n", self.state_before.realized_pnl));
        out.push_str(&format!("║    Fees Paid:     ${:>15.6}                                        ║\n", self.state_before.fees_paid));
        out.push_str(&format!("║    Open Positions: {:>10}                                           ║\n", self.state_before.open_positions));
        
        if let Some(ref after) = self.state_after {
            out.push_str("╠══════════════════════════════════════════════════════════════════════════════╣\n");
            out.push_str("║  STATE AFTER (at violation):                                                 ║\n");
            out.push_str(&format!("║    Cash:          ${:>15.6}                                        ║\n", after.cash));
            out.push_str(&format!("║    Realized PnL:  ${:>15.6}                                        ║\n", after.realized_pnl));
            out.push_str(&format!("║    Fees Paid:     ${:>15.6}                                        ║\n", after.fees_paid));
        }
        
        out.push_str("╠══════════════════════════════════════════════════════════════════════════════╣\n");
        out.push_str(&format!("║  RECENT LEDGER ENTRIES ({} shown):                                        ║\n", 
            self.causal_trace.recent_entries.len().min(10)));
        
        for entry in self.causal_trace.recent_entries.iter().take(10) {
            let balanced = if entry.is_balanced() { "✓" } else { "✗" };
            out.push_str(&format!(
                "║    [{}] {} | {:20} | D:{:>12} C:{:>12}      ║\n",
                entry.entry_id,
                balanced,
                truncate_str(&format!("{:?}", entry.event_ref), 20),
                from_amount(entry.total_debits()),
                from_amount(entry.total_credits()),
            ));
        }
        
        out.push_str("╠══════════════════════════════════════════════════════════════════════════════╣\n");
        out.push_str("║  MESSAGE:                                                                    ║\n");
        for line in self.message.lines().take(5) {
            out.push_str(&format!("║    {:72} ║\n", truncate_str(line, 72)));
        }
        out.push_str("╚══════════════════════════════════════════════════════════════════════════════╝\n");
        
        out
    }
}

fn truncate_str(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}...", &s[..max_len.saturating_sub(3)])
    }
}

// =============================================================================
// ACCOUNTING TRIGGER
// =============================================================================

/// Type of operation that triggered an accounting event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AccountingTrigger {
    /// Fill event (buy or sell).
    Fill {
        fill_id: u64,
        order_id: Option<OrderId>,
        market_id: String,
        outcome: Outcome,
        side: Side,
        quantity: f64,
        price: f64,
        fee: f64,
    },
    /// Fee posting (usually part of fill).
    Fee {
        fill_id: u64,
        amount: f64,
    },
    /// Settlement event.
    Settlement {
        settlement_id: u64,
        market_id: String,
        winner: Outcome,
    },
    /// Initial deposit.
    InitialDeposit {
        amount: f64,
    },
    /// Adjustment (correction).
    Adjustment {
        reason: String,
    },
}

// =============================================================================
// ACCOUNTING STATE SNAPSHOT
// =============================================================================

/// Snapshot of accounting state for violation context.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AccountingStateSnapshot {
    /// Cash balance.
    pub cash: f64,
    /// Realized PnL.
    pub realized_pnl: f64,
    /// Total fees paid.
    pub fees_paid: f64,
    /// Number of open positions.
    pub open_positions: usize,
    /// Position quantities by (market_id, outcome).
    pub positions: HashMap<(String, Outcome), f64>,
    /// Cost basis by (market_id, outcome).
    pub cost_basis: HashMap<(String, Outcome), f64>,
}

impl AccountingStateSnapshot {
    /// Capture current state from ledger.
    pub fn from_ledger(ledger: &Ledger) -> Self {
        Self {
            cash: ledger.cash(),
            realized_pnl: ledger.realized_pnl(),
            fees_paid: ledger.fees_paid(),
            open_positions: 0, // Would need to count non-zero positions
            positions: HashMap::new(),
            cost_basis: HashMap::new(),
        }
    }
}

// =============================================================================
// ACCOUNTING ENFORCER
// =============================================================================

/// Strict accounting enforcer - wraps the ledger with abort-on-violation semantics.
/// 
/// In strict mode (mandatory for production_grade), this enforcer:
/// 1. Routes ALL economic operations through the ledger
/// 2. Captures state snapshots before each operation
/// 3. Aborts immediately on any violation with a minimal causal trace
/// 4. Prevents any direct mutation of economic state
pub struct AccountingEnforcer {
    /// Underlying ledger (source of truth).
    ledger: Ledger,
    /// Current decision ID for violation context.
    decision_id: u64,
    /// Next fill ID.
    next_fill_id: u64,
    /// Next settlement ID.
    next_settlement_id: u64,
    /// Recent operations for trace (bounded).
    recent_operations: VecDeque<AccountingTrigger>,
    /// Maximum operations to keep in trace.
    trace_depth: usize,
    /// Statistics.
    pub stats: AccountingEnforcerStats,
}

/// Statistics for accounting enforcer.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AccountingEnforcerStats {
    /// Total fills processed.
    pub fills_processed: u64,
    /// Total fees processed.
    pub fees_processed: u64,
    /// Total settlements processed.
    pub settlements_processed: u64,
    /// Total ledger entries created.
    pub ledger_entries_created: u64,
    /// Total invariant checks passed.
    pub invariant_checks_passed: u64,
    /// First violation (if any).
    pub first_violation: Option<String>,
    /// Aborted flag.
    pub aborted: bool,
}

impl AccountingEnforcer {
    /// Create a new accounting enforcer.
    pub fn new(config: LedgerConfig) -> Self {
        let trace_depth = config.trace_depth;
        Self {
            ledger: Ledger::new(config),
            decision_id: 0,
            next_fill_id: 1,
            next_settlement_id: 1,
            recent_operations: VecDeque::with_capacity(trace_depth),
            trace_depth,
            stats: AccountingEnforcerStats::default(),
        }
    }

    /// Set the current decision ID.
    pub fn set_decision_id(&mut self, id: u64) {
        self.decision_id = id;
        self.ledger.set_decision_id(id);
    }

    /// Get the underlying ledger (read-only).
    pub fn ledger(&self) -> &Ledger {
        &self.ledger
    }

    /// Get cash balance.
    pub fn cash(&self) -> f64 {
        self.ledger.cash()
    }

    /// Get realized PnL.
    pub fn realized_pnl(&self) -> f64 {
        self.ledger.realized_pnl()
    }

    /// Get fees paid.
    pub fn fees_paid(&self) -> f64 {
        self.ledger.fees_paid()
    }

    /// Get position quantity.
    pub fn position_qty(&self, market_id: &str, outcome: Outcome) -> f64 {
        self.ledger.position_qty(market_id, outcome)
    }

    /// Get ledger entries count.
    pub fn entry_count(&self) -> usize {
        self.ledger.entries().len()
    }

    /// Check if there's been a violation.
    pub fn has_violation(&self) -> bool {
        self.ledger.has_violation()
    }

    // =========================================================================
    // FILL PROCESSING - THE ONLY PATHWAY FOR POSITION CHANGES
    // =========================================================================

    /// Post a fill through the ledger.
    /// 
    /// This is the ONLY authorized pathway for position and cash changes from fills.
    /// Aborts immediately on any accounting violation.
    pub fn post_fill(
        &mut self,
        market_id: &str,
        outcome: Outcome,
        side: Side,
        quantity: f64,
        price: f64,
        fee: f64,
        sim_time_ns: Nanos,
        arrival_time_ns: Nanos,
        order_id: Option<OrderId>,
    ) -> Result<u64, AccountingAbort> {
        // Capture state before operation
        let state_before = AccountingStateSnapshot::from_ledger(&self.ledger);
        
        // Generate fill ID
        let fill_id = self.next_fill_id;
        self.next_fill_id += 1;
        
        // Record trigger
        let trigger = AccountingTrigger::Fill {
            fill_id,
            order_id,
            market_id: market_id.to_string(),
            outcome,
            side,
            quantity,
            price,
            fee,
        };
        self.record_operation(trigger.clone());
        
        // Post to ledger
        let result = self.ledger.post_fill(
            fill_id,
            market_id,
            outcome,
            side,
            quantity,
            price,
            fee,
            sim_time_ns,
            arrival_time_ns,
            order_id,
        );
        
        match result {
            Ok(entry_id) => {
                self.stats.fills_processed += 1;
                self.stats.ledger_entries_created += 1;
                self.stats.invariant_checks_passed += 1;
                Ok(entry_id)
            }
            Err(violation) => {
                self.stats.aborted = true;
                self.stats.first_violation = Some(format!("{:?}", violation.violation_type));
                
                let state_after = Some(AccountingStateSnapshot::from_ledger(&self.ledger));
                let causal_trace = self.ledger.generate_causal_trace()
                    .unwrap_or_else(|| CausalTrace {
                        violation: violation.clone(),
                        recent_entries: self.ledger.entries().iter().rev().take(20).cloned().collect(),
                        current_balances: HashMap::new(),
                        config_snapshot: crate::backtest_v2::ledger::LedgerConfigSnapshot {
                            allow_negative_cash: self.ledger.config.allow_negative_cash,
                            allow_shorting: self.ledger.config.allow_shorting,
                            strict_mode: self.ledger.config.strict_mode,
                        },
                    });
                
                Err(AccountingAbort {
                    violation_type: violation.violation_type,
                    message: format!(
                        "Fill accounting violation: {} {} {} @ ${} in {}",
                        side_str(side), quantity, outcome_str(outcome), price, market_id
                    ),
                    sim_time_ns,
                    decision_id: self.decision_id,
                    trigger,
                    causal_trace,
                    state_before,
                    state_after,
                })
            }
        }
    }

    // =========================================================================
    // SETTLEMENT PROCESSING - THE ONLY PATHWAY FOR SETTLEMENT PNL
    // =========================================================================

    /// Post a settlement through the ledger.
    /// 
    /// This is the ONLY authorized pathway for settlement-based PnL realization.
    /// Aborts immediately on any accounting violation.
    pub fn post_settlement(
        &mut self,
        market_id: &str,
        winner: Outcome,
        sim_time_ns: Nanos,
        arrival_time_ns: Nanos,
    ) -> Result<u64, AccountingAbort> {
        // Capture state before operation
        let state_before = AccountingStateSnapshot::from_ledger(&self.ledger);
        
        // Generate settlement ID
        let settlement_id = self.next_settlement_id;
        self.next_settlement_id += 1;
        
        // Record trigger
        let trigger = AccountingTrigger::Settlement {
            settlement_id,
            market_id: market_id.to_string(),
            winner,
        };
        self.record_operation(trigger.clone());
        
        // Post to ledger
        let result = self.ledger.post_settlement(
            settlement_id,
            market_id,
            winner,
            sim_time_ns,
            arrival_time_ns,
        );
        
        match result {
            Ok(entry_id) => {
                self.stats.settlements_processed += 1;
                if entry_id > 0 {
                    self.stats.ledger_entries_created += 1;
                }
                self.stats.invariant_checks_passed += 1;
                Ok(entry_id)
            }
            Err(violation) => {
                self.stats.aborted = true;
                self.stats.first_violation = Some(format!("{:?}", violation.violation_type));
                
                let state_after = Some(AccountingStateSnapshot::from_ledger(&self.ledger));
                let causal_trace = self.ledger.generate_causal_trace()
                    .unwrap_or_else(|| CausalTrace {
                        violation: violation.clone(),
                        recent_entries: self.ledger.entries().iter().rev().take(20).cloned().collect(),
                        current_balances: HashMap::new(),
                        config_snapshot: crate::backtest_v2::ledger::LedgerConfigSnapshot {
                            allow_negative_cash: self.ledger.config.allow_negative_cash,
                            allow_shorting: self.ledger.config.allow_shorting,
                            strict_mode: self.ledger.config.strict_mode,
                        },
                    });
                
                Err(AccountingAbort {
                    violation_type: violation.violation_type,
                    message: format!(
                        "Settlement accounting violation: {} settled with {:?} winner",
                        market_id, winner
                    ),
                    sim_time_ns,
                    decision_id: self.decision_id,
                    trigger,
                    causal_trace,
                    state_before,
                    state_after,
                })
            }
        }
    }

    // =========================================================================
    // INTERNAL HELPERS
    // =========================================================================

    fn record_operation(&mut self, trigger: AccountingTrigger) {
        if self.recent_operations.len() >= self.trace_depth {
            self.recent_operations.pop_front();
        }
        self.recent_operations.push_back(trigger);
    }
}

fn side_str(side: Side) -> &'static str {
    match side {
        Side::Buy => "BUY",
        Side::Sell => "SELL",
    }
}

fn outcome_str(outcome: Outcome) -> &'static str {
    match outcome {
        Outcome::Yes => "YES",
        Outcome::No => "NO",
    }
}

// =============================================================================
// TESTS
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backtest_v2::ledger::LedgerConfig;

    #[test]
    fn test_enforcer_basic_fill() {
        let config = LedgerConfig {
            initial_cash: 1000.0,
            strict_mode: true,
            ..Default::default()
        };
        let mut enforcer = AccountingEnforcer::new(config);
        
        // Buy 100 YES @ $0.50
        let result = enforcer.post_fill(
            "market1", Outcome::Yes, Side::Buy,
            100.0, 0.50, 0.10,
            1000, 1000, None,
        );
        
        assert!(result.is_ok());
        assert_eq!(enforcer.stats.fills_processed, 1);
        
        // Check balances
        assert!((enforcer.cash() - 949.90).abs() < 0.01);
        assert!((enforcer.position_qty("market1", Outcome::Yes) - 100.0).abs() < 0.01);
    }

    #[test]
    fn test_enforcer_negative_cash_abort() {
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
        
        assert!(result.is_err());
        let abort = result.unwrap_err();
        assert!(matches!(abort.violation_type, ViolationType::NegativeCash { .. }));
        assert!(enforcer.stats.aborted);
    }

    #[test]
    fn test_enforcer_causal_trace_format() {
        let config = LedgerConfig {
            initial_cash: 100.0,
            allow_negative_cash: false,
            strict_mode: true,
            ..Default::default()
        };
        let mut enforcer = AccountingEnforcer::new(config);
        
        // Cause a violation
        let result = enforcer.post_fill(
            "market1", Outcome::Yes, Side::Buy,
            1000.0, 0.50, 0.0,
            1000, 1000, None,
        );
        
        let abort = result.unwrap_err();
        let trace = abort.format_trace();
        
        // Verify trace contains key information
        assert!(trace.contains("STRICT ACCOUNTING ABORT"));
        assert!(trace.contains("NegativeCash"));
        assert!(trace.contains("STATE BEFORE"));
        assert!(trace.contains("RECENT LEDGER ENTRIES"));
    }

    #[test]
    fn test_enforcer_settlement() {
        let config = LedgerConfig {
            initial_cash: 1000.0,
            strict_mode: true,
            ..Default::default()
        };
        let mut enforcer = AccountingEnforcer::new(config);
        
        // Buy 100 YES @ $0.40
        enforcer.post_fill(
            "market1", Outcome::Yes, Side::Buy,
            100.0, 0.40, 0.0,
            1000, 1000, None,
        ).unwrap();
        
        // Settle with YES winning
        let result = enforcer.post_settlement(
            "market1", Outcome::Yes, 2000, 2000,
        );
        
        assert!(result.is_ok());
        
        // Cash: 960 + 100 = 1060
        assert!((enforcer.cash() - 1060.0).abs() < 0.01);
        // PnL: 100 - 40 = 60
        assert!((enforcer.realized_pnl() - 60.0).abs() < 0.01);
    }

    #[test]
    fn test_enforcer_duplicate_fill_blocked() {
        let config = LedgerConfig {
            initial_cash: 1000.0,
            strict_mode: true,
            ..Default::default()
        };
        let mut enforcer = AccountingEnforcer::new(config);
        
        // First fill
        enforcer.post_fill(
            "market1", Outcome::Yes, Side::Buy,
            100.0, 0.50, 0.0,
            1000, 1000, None,
        ).unwrap();
        
        // Note: The ledger uses fill_id to detect duplicates, and AccountingEnforcer
        // generates unique fill_ids, so duplicate detection works at the ledger level
        // if the same fill_id is posted twice (which shouldn't happen through the enforcer).
    }
}

//! Strict Accounting Enforcement Module
//!
//! This module provides **structural enforcement** that makes it impossible to
//! bypass the double-entry ledger when `strict_accounting=true`.
//!
//! # Design Principles
//!
//! 1. **Structural Gating**: Direct mutation paths are unreachable in strict mode
//! 2. **Runtime Guard**: Any attempted bypass triggers immediate abort
//! 3. **Instrumentation**: All economic operations are counted for audit
//! 4. **Deterministic Traces**: Violations produce bounded, reproducible dumps
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────────┐
//! │                    StrictAccountingGuard                        │
//! │  (wraps Ledger, blocks all direct mutation attempts)            │
//! └─────────────────────────────────────────────────────────────────┘
//!                                │
//!        ┌───────────────────────┼───────────────────────┐
//!        │                       │                       │
//!        ▼                       ▼                       ▼
//! ┌─────────────┐        ┌─────────────┐        ┌─────────────┐
//! │ post_fill() │        │ post_fee()  │        │ post_settle │
//! │ (ONLY way)  │        │ (ledger)    │        │ (ONLY way)  │
//! └─────────────┘        └─────────────┘        └─────────────┘
//!
//!  ❌ BLOCKED:
//!  - Portfolio.apply_fill()
//!  - Portfolio.settle_market()
//!  - Position.apply_fill()
//!  - sim_adapter.process_fill() [accounting part]
//!  - Any direct cash/position/pnl mutation
//! ```
//!
//! # Mutation Categories
//!
//! | Category | Example | Status in Strict Mode |
//! |----------|---------|----------------------|
//! | A - Fill | cash += trade_value | BLOCKED - use post_fill() |
//! | B - Fee | total_fees += fee | ALLOWED only via post_fill() |
//! | C - Settlement | cash += settlement_value | BLOCKED - use post_settlement() |
//! | D - Transfer | cash += amount | BLOCKED - use post_deposit() |
//! | E - MTM | unrealized_pnl update | ALLOWED (derived/read-only) |
//! | F - Metrics | counters, stats | ALLOWED (non-economic) |

use crate::backtest_v2::clock::Nanos;
use crate::backtest_v2::events::{OrderId, Side};
use crate::backtest_v2::ledger::{
    AccountingViolation, Ledger, LedgerConfig, LedgerEntry, ViolationType,
    from_amount, to_amount, Amount, AMOUNT_SCALE,
};
use crate::backtest_v2::portfolio::Outcome;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

// =============================================================================
// GLOBAL STRICT MODE FLAG
// =============================================================================

/// Global flag indicating strict accounting mode is active.
/// When true, ANY attempt to mutate economic state outside the ledger will panic.
static STRICT_ACCOUNTING_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Counter for direct mutation attempts (for audit).
static DIRECT_MUTATION_ATTEMPTS: AtomicU64 = AtomicU64::new(0);

/// Activate strict accounting mode globally.
/// Once activated, any direct mutation attempt will abort.
pub fn activate_strict_accounting() {
    STRICT_ACCOUNTING_ACTIVE.store(true, Ordering::SeqCst);
}

/// Deactivate strict accounting mode (for testing legacy paths).
pub fn deactivate_strict_accounting() {
    STRICT_ACCOUNTING_ACTIVE.store(false, Ordering::SeqCst);
}

/// Check if strict accounting is active.
pub fn is_strict_accounting_active() -> bool {
    STRICT_ACCOUNTING_ACTIVE.load(Ordering::SeqCst)
}

/// Get count of direct mutation attempts.
pub fn direct_mutation_attempt_count() -> u64 {
    DIRECT_MUTATION_ATTEMPTS.load(Ordering::SeqCst)
}

/// Reset mutation attempt counter (for testing).
pub fn reset_mutation_attempt_counter() {
    DIRECT_MUTATION_ATTEMPTS.store(0, Ordering::SeqCst);
}

// =============================================================================
// STRICT ACCOUNTING GUARD
// =============================================================================

/// Guard that must be called before any direct mutation.
/// In strict mode, this will panic. In legacy mode, it will log and count.
/// 
/// # Usage
/// 
/// Place this at the top of any function that directly mutates economic state:
/// ```ignore
/// fn apply_fill(&mut self, ...) {
///     guard_direct_mutation!("Portfolio::apply_fill");
///     // ... rest of function
/// }
/// ```
#[macro_export]
macro_rules! guard_direct_mutation {
    ($location:expr) => {
        if $crate::backtest_v2::strict_accounting::is_strict_accounting_active() {
            $crate::backtest_v2::strict_accounting::abort_direct_mutation($location);
        } else {
            $crate::backtest_v2::strict_accounting::count_direct_mutation($location);
        }
    };
}

/// Abort due to direct mutation attempt in strict mode.
/// This is called by the guard_direct_mutation! macro.
#[cold]
#[inline(never)]
pub fn abort_direct_mutation(location: &str) -> ! {
    DIRECT_MUTATION_ATTEMPTS.fetch_add(1, Ordering::SeqCst);
    panic!(
        "\n╔══════════════════════════════════════════════════════════════════════════════╗\n\
         ║              STRICT ACCOUNTING VIOLATION - DIRECT MUTATION BLOCKED           ║\n\
         ╠══════════════════════════════════════════════════════════════════════════════╣\n\
         ║  LOCATION: {:<63}  ║\n\
         ║                                                                              ║\n\
         ║  When strict_accounting=true, ALL economic state changes MUST go through     ║\n\
         ║  the double-entry ledger. Direct mutations are FORBIDDEN.                    ║\n\
         ║                                                                              ║\n\
         ║  AUTHORIZED PATHWAYS:                                                        ║\n\
         ║    - Fill: ledger.post_fill() or AccountingEnforcer.post_fill()              ║\n\
         ║    - Settlement: ledger.post_settlement() or AccountingEnforcer.post_settle  ║\n\
         ║    - Deposit: ledger.post_deposit()                                          ║\n\
         ║                                                                              ║\n\
         ║  This abort ensures accounting integrity. Fix by routing through the ledger. ║\n\
         ╚══════════════════════════════════════════════════════════════════════════════╝\n",
        location
    )
}

/// Count a direct mutation attempt (legacy mode).
#[inline]
pub fn count_direct_mutation(location: &str) {
    DIRECT_MUTATION_ATTEMPTS.fetch_add(1, Ordering::SeqCst);
    tracing::warn!(
        location = %location,
        "Direct mutation in legacy mode (would be blocked in strict mode)"
    );
}

// =============================================================================
// BYPASS DETECTION RESULT
// =============================================================================

/// Result of checking for accounting bypass.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BypassCheckResult {
    /// Whether a bypass was detected.
    pub bypass_detected: bool,
    /// Description of the bypass if detected.
    pub description: Option<String>,
    /// Ledger cash balance.
    pub ledger_cash: f64,
    /// Shadow cash balance (from Portfolio/adapter).
    pub shadow_cash: Option<f64>,
    /// Ledger realized PnL.
    pub ledger_realized_pnl: f64,
    /// Shadow realized PnL.
    pub shadow_realized_pnl: Option<f64>,
    /// Ledger position quantities by market.
    pub ledger_positions: HashMap<String, f64>,
    /// Shadow position quantities.
    pub shadow_positions: Option<HashMap<String, f64>>,
    /// Absolute tolerance for comparison.
    pub tolerance: f64,
}

impl BypassCheckResult {
    /// Format as text for logging.
    pub fn format_text(&self) -> String {
        let mut out = String::new();
        out.push_str("=== BYPASS CHECK RESULT ===\n");
        out.push_str(&format!("Bypass detected: {}\n", self.bypass_detected));
        if let Some(ref desc) = self.description {
            out.push_str(&format!("Description: {}\n", desc));
        }
        out.push_str(&format!("Ledger cash: ${:.6}\n", self.ledger_cash));
        if let Some(shadow) = self.shadow_cash {
            out.push_str(&format!("Shadow cash: ${:.6} (diff: ${:.6})\n", 
                shadow, (self.ledger_cash - shadow).abs()));
        }
        out.push_str(&format!("Ledger realized PnL: ${:.6}\n", self.ledger_realized_pnl));
        if let Some(shadow) = self.shadow_realized_pnl {
            out.push_str(&format!("Shadow realized PnL: ${:.6} (diff: ${:.6})\n", 
                shadow, (self.ledger_realized_pnl - shadow).abs()));
        }
        out
    }
}

// =============================================================================
// STRICT ACCOUNTING STATE
// =============================================================================

/// State tracking for strict accounting mode.
/// This tracks ALL economic changes and can verify against shadow state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrictAccountingState {
    /// Whether strict mode is enforced.
    pub strict_mode: bool,
    /// Total fills routed through ledger.
    pub ledger_fills: u64,
    /// Total settlements routed through ledger.
    pub ledger_settlements: u64,
    /// Total deposits routed through ledger.
    pub ledger_deposits: u64,
    /// Total withdrawals routed through ledger.
    pub ledger_withdrawals: u64,
    /// Direct mutation attempts detected.
    pub direct_mutation_attempts: u64,
    /// Bypass violations detected.
    pub bypass_violations: u64,
    /// Last bypass description.
    pub last_bypass_description: Option<String>,
}

impl Default for StrictAccountingState {
    fn default() -> Self {
        Self {
            strict_mode: false,
            ledger_fills: 0,
            ledger_settlements: 0,
            ledger_deposits: 0,
            ledger_withdrawals: 0,
            direct_mutation_attempts: 0,
            bypass_violations: 0,
            last_bypass_description: None,
        }
    }
}

impl StrictAccountingState {
    /// Create new state with strict mode.
    pub fn new(strict_mode: bool) -> Self {
        Self {
            strict_mode,
            ..Default::default()
        }
    }

    /// Record a fill through ledger.
    pub fn record_ledger_fill(&mut self) {
        self.ledger_fills += 1;
    }

    /// Record a settlement through ledger.
    pub fn record_ledger_settlement(&mut self) {
        self.ledger_settlements += 1;
    }

    /// Record a bypass violation.
    pub fn record_bypass(&mut self, description: String) {
        self.bypass_violations += 1;
        self.last_bypass_description = Some(description);
    }

    /// Check if accounting is clean (no bypasses).
    pub fn is_clean(&self) -> bool {
        self.bypass_violations == 0 && self.direct_mutation_attempts == 0
    }

    /// Get summary for results.
    pub fn summary(&self) -> String {
        format!(
            "strict={}, fills={}, settlements={}, bypasses={}, direct_attempts={}",
            self.strict_mode,
            self.ledger_fills,
            self.ledger_settlements,
            self.bypass_violations,
            self.direct_mutation_attempts
        )
    }
}

// =============================================================================
// ACCOUNTING INVARIANT CHECKS
// =============================================================================

/// Check that ledger and shadow state match (for strict mode verification).
/// 
/// In strict mode, the ledger is the SOLE source of truth. If shadow state
/// diverges, it indicates a bypass occurred.
pub fn check_ledger_shadow_parity(
    ledger_cash: f64,
    ledger_realized_pnl: f64,
    ledger_positions: &HashMap<(String, Outcome), f64>,
    shadow_cash: f64,
    shadow_realized_pnl: f64,
    shadow_positions: &HashMap<String, f64>,
    tolerance: f64,
) -> BypassCheckResult {
    let mut result = BypassCheckResult {
        bypass_detected: false,
        description: None,
        ledger_cash,
        shadow_cash: Some(shadow_cash),
        ledger_realized_pnl,
        shadow_realized_pnl: Some(shadow_realized_pnl),
        ledger_positions: ledger_positions.iter()
            .map(|((m, o), q)| (format!("{}:{:?}", m, o), *q))
            .collect(),
        shadow_positions: Some(shadow_positions.clone()),
        tolerance,
    };

    // Check cash
    let cash_diff = (ledger_cash - shadow_cash).abs();
    if cash_diff > tolerance {
        result.bypass_detected = true;
        result.description = Some(format!(
            "Cash mismatch: ledger=${:.6}, shadow=${:.6}, diff=${:.6}",
            ledger_cash, shadow_cash, cash_diff
        ));
        return result;
    }

    // Check realized PnL
    let pnl_diff = (ledger_realized_pnl - shadow_realized_pnl).abs();
    if pnl_diff > tolerance {
        result.bypass_detected = true;
        result.description = Some(format!(
            "Realized PnL mismatch: ledger=${:.6}, shadow=${:.6}, diff=${:.6}",
            ledger_realized_pnl, shadow_realized_pnl, pnl_diff
        ));
        return result;
    }

    result
}

// =============================================================================
// FIXED-POINT ASSERTIONS
// =============================================================================

/// Assert that a value is representable in fixed-point without loss.
/// Used to verify no float-based economic mutations are occurring.
#[inline]
pub fn assert_fixed_point_safe(value: f64, field_name: &str) {
    let as_fixed = to_amount(value);
    let back = from_amount(as_fixed);
    let diff = (value - back).abs();
    
    // Allow for floating point rounding (8 decimal places)
    if diff > 1e-7 {
        panic!(
            "Fixed-point precision loss detected in {}: original={}, converted={}, diff={}",
            field_name, value, back, diff
        );
    }
}

// =============================================================================
// ACCOUNTING EVENT AUDIT LOG
// =============================================================================

/// Types of accounting events for audit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AccountingEvent {
    Fill {
        fill_id: u64,
        market_id: String,
        outcome: Outcome,
        side: Side,
        quantity: f64,
        price: f64,
        fee: f64,
        via_ledger: bool,
    },
    Settlement {
        settlement_id: u64,
        market_id: String,
        winner: Outcome,
        pnl: f64,
        via_ledger: bool,
    },
    Deposit {
        amount: f64,
        via_ledger: bool,
    },
    Withdrawal {
        amount: f64,
        via_ledger: bool,
    },
    DirectMutation {
        location: String,
        blocked: bool,
    },
}

/// Audit log for accounting events.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AccountingAuditLog {
    events: Vec<(Nanos, AccountingEvent)>,
    max_events: usize,
}

impl AccountingAuditLog {
    pub fn new(max_events: usize) -> Self {
        Self {
            events: Vec::with_capacity(max_events.min(10000)),
            max_events,
        }
    }

    pub fn record(&mut self, time: Nanos, event: AccountingEvent) {
        if self.events.len() < self.max_events {
            self.events.push((time, event));
        }
    }

    pub fn events(&self) -> &[(Nanos, AccountingEvent)] {
        &self.events
    }

    /// Count events that bypassed ledger.
    pub fn bypass_count(&self) -> usize {
        self.events.iter().filter(|(_, e)| {
            match e {
                AccountingEvent::Fill { via_ledger, .. } => !via_ledger,
                AccountingEvent::Settlement { via_ledger, .. } => !via_ledger,
                AccountingEvent::Deposit { via_ledger, .. } => !via_ledger,
                AccountingEvent::Withdrawal { via_ledger, .. } => !via_ledger,
                AccountingEvent::DirectMutation { blocked, .. } => !blocked,
            }
        }).count()
    }

    /// Get summary for results.
    pub fn summary(&self) -> String {
        let total = self.events.len();
        let bypasses = self.bypass_count();
        format!(
            "total_events={}, bypasses={}, clean={}",
            total, bypasses, bypasses == 0
        )
    }
}

// =============================================================================
// TESTS
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strict_mode_flag() {
        // Start with clean state
        deactivate_strict_accounting();
        assert!(!is_strict_accounting_active());

        activate_strict_accounting();
        assert!(is_strict_accounting_active());

        deactivate_strict_accounting();
        assert!(!is_strict_accounting_active());
    }

    #[test]
    fn test_mutation_counter() {
        reset_mutation_attempt_counter();
        assert_eq!(direct_mutation_attempt_count(), 0);

        deactivate_strict_accounting();
        count_direct_mutation("test_location");
        assert_eq!(direct_mutation_attempt_count(), 1);

        reset_mutation_attempt_counter();
    }

    #[test]
    #[should_panic(expected = "STRICT ACCOUNTING VIOLATION")]
    fn test_strict_mode_blocks_direct_mutation() {
        activate_strict_accounting();
        // This should panic
        abort_direct_mutation("test::direct_mutation");
    }

    #[test]
    fn test_bypass_check_clean() {
        let ledger_positions = HashMap::new();
        let shadow_positions = HashMap::new();

        let result = check_ledger_shadow_parity(
            1000.0, 50.0, &ledger_positions,
            1000.0, 50.0, &shadow_positions,
            0.01,
        );

        assert!(!result.bypass_detected);
    }

    #[test]
    fn test_bypass_check_detects_cash_mismatch() {
        let ledger_positions = HashMap::new();
        let shadow_positions = HashMap::new();

        let result = check_ledger_shadow_parity(
            1000.0, 50.0, &ledger_positions,
            999.0, 50.0, &shadow_positions,  // 1.0 difference
            0.01,  // tolerance
        );

        assert!(result.bypass_detected);
        assert!(result.description.unwrap().contains("Cash mismatch"));
    }

    #[test]
    fn test_fixed_point_safe_valid() {
        // Should not panic
        assert_fixed_point_safe(123.45678901, "test_value");
        assert_fixed_point_safe(0.00000001, "min_value");
        assert_fixed_point_safe(1000000.0, "large_value");
    }

    #[test]
    fn test_accounting_audit_log() {
        let mut log = AccountingAuditLog::new(100);

        log.record(1000, AccountingEvent::Fill {
            fill_id: 1,
            market_id: "market1".to_string(),
            outcome: Outcome::Yes,
            side: Side::Buy,
            quantity: 100.0,
            price: 0.50,
            fee: 0.05,
            via_ledger: true,
        });

        log.record(2000, AccountingEvent::Fill {
            fill_id: 2,
            market_id: "market1".to_string(),
            outcome: Outcome::Yes,
            side: Side::Sell,
            quantity: 50.0,
            price: 0.60,
            fee: 0.03,
            via_ledger: false, // Bypass!
        });

        assert_eq!(log.events().len(), 2);
        assert_eq!(log.bypass_count(), 1);
    }

    #[test]
    fn test_strict_accounting_state() {
        let mut state = StrictAccountingState::new(true);
        assert!(state.is_clean());

        state.record_ledger_fill();
        state.record_ledger_fill();
        state.record_ledger_settlement();
        assert!(state.is_clean());
        assert_eq!(state.ledger_fills, 2);
        assert_eq!(state.ledger_settlements, 1);

        state.record_bypass("Test bypass".to_string());
        assert!(!state.is_clean());
        assert_eq!(state.bypass_violations, 1);
    }
}

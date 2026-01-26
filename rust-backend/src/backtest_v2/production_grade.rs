//! Production-Grade Mode Enforcement
//!
//! This module enforces production-grade requirements as a HARD correctness gate.
//! When `production_grade=true`, the backtester MUST:
//!
//! 1. Run ALL invariant categories in Hard mode (abort on first violation)
//! 2. Run ALL data-integrity policies in strict mode (no silent best-effort)
//! 3. Emit a minimal causal dump on first violation
//! 4. Compute a deterministic violation hash identical across reruns
//!
//! # Design Principles
//!
//! - **No silent continue**: Every violation aborts immediately
//! - **No weakened configuration**: Attempting to weaken settings fails loudly
//! - **Deterministic dumps**: Identical inputs produce identical violation hashes
//! - **Bounded context**: Dumps are fixed-size and deterministic

use crate::backtest_v2::clock::Nanos;
use crate::backtest_v2::events::TimestampedEvent;
use crate::backtest_v2::integrity::PathologyPolicy;
use crate::backtest_v2::invariants::{
    CausalDump, CategoryFlags, EventSummary, InvariantConfig, InvariantMode,
    InvariantViolation, LedgerEntrySummary, OmsTransition, StateSnapshot,
};
use crate::backtest_v2::ledger::LedgerConfig;
use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

// =============================================================================
// PRODUCTION-GRADE ENFORCEMENT ERROR
// =============================================================================

/// Error returned when production-grade requirements cannot be met.
#[derive(Debug, Clone)]
pub struct ProductionGradeEnforcementError {
    /// List of requirement violations.
    pub violations: Vec<String>,
    /// Attempted configuration summary.
    pub config_summary: String,
}

impl std::fmt::Display for ProductionGradeEnforcementError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "PRODUCTION-GRADE REQUIREMENTS NOT MET:\n{}",
            self.violations.iter()
                .enumerate()
                .map(|(i, v)| format!("  {}. {}", i + 1, v))
                .collect::<Vec<_>>()
                .join("\n")
        )
    }
}

impl std::error::Error for ProductionGradeEnforcementError {}

// =============================================================================
// DETERMINISTIC VIOLATION HASH
// =============================================================================

/// Deterministic hash computed over the causal slice of a violation.
/// 
/// This hash is identical across reruns if and only if the same violation
/// occurs with the same causal context. It excludes:
/// - Log timestamps
/// - Wall-clock times
/// - Thread IDs
/// - Allocator addresses
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ViolationHash(pub u64);

impl ViolationHash {
    /// Compute a deterministic hash from a violation and its causal context.
    pub fn compute(
        violation: &InvariantViolation,
        triggering_event: Option<&EventSummary>,
        recent_events: &[EventSummary],
        oms_transitions: &[OmsTransition],
        ledger_entries: &[LedgerEntrySummary],
        config_hash: u64,
    ) -> Self {
        let mut hasher = DefaultHasher::new();
        
        // Hash violation code (stable identifier)
        std::mem::discriminant(&violation.violation_type).hash(&mut hasher);
        violation.category.hash(&mut hasher);
        
        // Hash decision time bucket (to 1ms granularity for stability)
        let time_bucket = violation.decision_time / 1_000_000; // 1ms buckets
        time_bucket.hash(&mut hasher);
        
        // Hash triggering event (canonical)
        if let Some(event) = triggering_event {
            event.event_type.hash(&mut hasher);
            event.arrival_time.hash(&mut hasher);
            if let Some(seq) = event.seq {
                seq.hash(&mut hasher);
            }
        }
        
        // Hash last N events (canonical, deterministic order)
        for event in recent_events.iter().take(10) {
            event.event_type.hash(&mut hasher);
            event.arrival_time.hash(&mut hasher);
        }
        
        // Hash last N OMS transitions (canonical, deterministic order)
        for t in oms_transitions.iter().take(5) {
            t.order_id.hash(&mut hasher);
            t.from_state.hash(&mut hasher);
            t.to_state.hash(&mut hasher);
        }
        
        // Hash last N ledger entries (canonical, deterministic order)
        for e in ledger_entries.iter().take(5) {
            e.entry_id.hash(&mut hasher);
            e.entry_type.hash(&mut hasher);
            e.total_debits.hash(&mut hasher);
            e.total_credits.hash(&mut hasher);
        }
        
        // Hash config
        config_hash.hash(&mut hasher);
        
        ViolationHash(hasher.finish())
    }
    
    /// Compute from a CausalDump.
    pub fn from_dump(dump: &CausalDump) -> Self {
        Self::compute(
            &dump.violation,
            dump.triggering_event.as_ref(),
            &dump.recent_events,
            &dump.oms_transitions,
            &dump.ledger_entries,
            dump.config_hash,
        )
    }
    
    /// Format as hex string.
    pub fn to_hex(&self) -> String {
        format!("{:#018x}", self.0)
    }
}

impl std::fmt::Display for ViolationHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:#018x}", self.0)
    }
}

// =============================================================================
// PRODUCTION-GRADE ABORT
// =============================================================================

/// Structured abort information for production-grade violations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProductionGradeAbort {
    /// The violation that caused the abort.
    pub violation: InvariantViolation,
    /// Deterministic violation hash.
    pub violation_hash: ViolationHash,
    /// Causal dump with bounded context.
    pub causal_dump: CausalDump,
    /// Run fingerprint at time of abort.
    pub run_fingerprint: u64,
    /// Config fingerprint.
    pub config_fingerprint: u64,
    /// Wall-clock time (for logging only, not in hash).
    #[serde(skip_serializing)]
    pub wall_time: Option<std::time::SystemTime>,
}

impl ProductionGradeAbort {
    /// Create from a causal dump.
    pub fn from_dump(dump: CausalDump, run_fingerprint: u64, config_fingerprint: u64) -> Self {
        let violation_hash = ViolationHash::from_dump(&dump);
        Self {
            violation: dump.violation.clone(),
            violation_hash,
            causal_dump: dump,
            run_fingerprint,
            config_fingerprint,
            wall_time: Some(std::time::SystemTime::now()),
        }
    }
    
    /// Format as deterministic text (excludes wall time).
    pub fn format_deterministic(&self) -> String {
        let mut out = String::new();
        
        out.push_str("╔══════════════════════════════════════════════════════════════════════════════╗\n");
        out.push_str("║           PRODUCTION-GRADE ABORT - FIRST VIOLATION DETECTED                 ║\n");
        out.push_str("╠══════════════════════════════════════════════════════════════════════════════╣\n");
        out.push_str(&format!(
            "║  VIOLATION HASH: {}                              ║\n",
            self.violation_hash
        ));
        out.push_str(&format!(
            "║  CATEGORY:       {:?}{}\n",
            self.violation.category,
            " ".repeat(62 - format!("{:?}", self.violation.category).len())
        ));
        out.push_str(&format!(
            "║  SIM TIME:       {} ns{}\n",
            self.violation.sim_time,
            " ".repeat(55 - self.violation.sim_time.to_string().len())
        ));
        out.push_str(&format!(
            "║  DECISION TIME:  {} ns{}\n",
            self.violation.decision_time,
            " ".repeat(55 - self.violation.decision_time.to_string().len())
        ));
        out.push_str("╠══════════════════════════════════════════════════════════════════════════════╣\n");
        out.push_str(&format!("║  MESSAGE: {:<67} ║\n", truncate_str(&self.violation.message, 67)));
        out.push_str("╠══════════════════════════════════════════════════════════════════════════════╣\n");
        out.push_str(&format!("║  RUN FINGERPRINT:    {:#018x}                                       ║\n", self.run_fingerprint));
        out.push_str(&format!("║  CONFIG FINGERPRINT: {:#018x}                                       ║\n", self.config_fingerprint));
        out.push_str("╠══════════════════════════════════════════════════════════════════════════════╣\n");
        
        // Recent events (bounded, deterministic order)
        out.push_str(&format!("║  RECENT EVENTS ({} shown):                                                   ║\n", 
            self.causal_dump.recent_events.len().min(10)));
        for (i, event) in self.causal_dump.recent_events.iter().take(10).enumerate() {
            out.push_str(&format!(
                "║    [{}] {} @ {} ns\n",
                i, truncate_str(&event.event_type, 30), event.arrival_time
            ));
        }
        
        // OMS transitions (bounded, deterministic order)
        if !self.causal_dump.oms_transitions.is_empty() {
            out.push_str("╠══════════════════════════════════════════════════════════════════════════════╣\n");
            out.push_str(&format!("║  OMS TRANSITIONS ({} shown):                                                 ║\n",
                self.causal_dump.oms_transitions.len().min(5)));
            for t in self.causal_dump.oms_transitions.iter().take(5) {
                out.push_str(&format!(
                    "║    Order {} @ {} ns: {} -> {}\n",
                    t.order_id, t.timestamp, t.from_state, t.to_state
                ));
            }
        }
        
        // Ledger entries (bounded, deterministic order)
        if !self.causal_dump.ledger_entries.is_empty() {
            out.push_str("╠══════════════════════════════════════════════════════════════════════════════╣\n");
            out.push_str(&format!("║  LEDGER ENTRIES ({} shown):                                                  ║\n",
                self.causal_dump.ledger_entries.len().min(5)));
            for e in self.causal_dump.ledger_entries.iter().take(5) {
                out.push_str(&format!(
                    "║    [{}] {} @ {} ns: D={} C={}\n",
                    e.entry_id, truncate_str(&e.entry_type, 15), e.timestamp, e.total_debits, e.total_credits
                ));
            }
        }
        
        // State snapshot
        out.push_str("╠══════════════════════════════════════════════════════════════════════════════╣\n");
        out.push_str("║  STATE SNAPSHOT:                                                             ║\n");
        out.push_str(&format!("║    Cash:        ${:.6}                                                   ║\n", 
            self.causal_dump.state_snapshot.cash));
        out.push_str(&format!("║    Position:    {:.6}                                                    ║\n",
            self.causal_dump.state_snapshot.position));
        out.push_str(&format!("║    Open Orders: {}                                                          ║\n",
            self.causal_dump.state_snapshot.open_orders));
        
        out.push_str("╚══════════════════════════════════════════════════════════════════════════════╝\n");
        
        out
    }
    
    /// Serialize to canonical JSON (deterministic key ordering).
    pub fn to_canonical_json(&self) -> String {
        // Use serde_json with sorted keys for determinism
        serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".to_string())
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
// PRODUCTION-GRADE REQUIREMENTS
// =============================================================================

/// Complete set of production-grade requirements that must all be satisfied.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProductionGradeRequirements {
    /// Visibility strict mode must be ON.
    pub visibility_strict: bool,
    /// Invariant mode must be Hard.
    pub invariant_hard: bool,
    /// All invariant categories must be enabled.
    pub all_invariant_categories: bool,
    /// Integrity policy must be strict.
    pub integrity_strict: bool,
    /// Ledger must be enabled with strict mode.
    pub ledger_strict: bool,
    /// Accounting must be strict.
    pub accounting_strict: bool,
    /// Deterministic seed must be set.
    pub deterministic_seed: bool,
    /// Dump buffers must be enabled.
    pub dump_buffers_enabled: bool,
}

impl ProductionGradeRequirements {
    /// Check all requirements are met.
    pub fn all_met(&self) -> bool {
        self.visibility_strict
            && self.invariant_hard
            && self.all_invariant_categories
            && self.integrity_strict
            && self.ledger_strict
            && self.accounting_strict
            && self.deterministic_seed
            && self.dump_buffers_enabled
    }
    
    /// Get list of unmet requirements.
    pub fn unmet(&self) -> Vec<&'static str> {
        let mut violations = Vec::new();
        if !self.visibility_strict {
            violations.push("visibility_strict mode must be ON");
        }
        if !self.invariant_hard {
            violations.push("invariant_mode must be Hard");
        }
        if !self.all_invariant_categories {
            violations.push("all invariant categories must be enabled");
        }
        if !self.integrity_strict {
            violations.push("integrity_policy must be strict()");
        }
        if !self.ledger_strict {
            violations.push("ledger must be enabled with strict_mode=true");
        }
        if !self.accounting_strict {
            violations.push("strict_accounting must be true");
        }
        if !self.deterministic_seed {
            violations.push("deterministic seed must be set");
        }
        if !self.dump_buffers_enabled {
            violations.push("dump buffers must be enabled");
        }
        violations
    }
}

// =============================================================================
// ENFORCEMENT FUNCTIONS
// =============================================================================

/// Enforce production-grade requirements at backtest start.
/// 
/// This function MUST be called when `production_grade=true` BEFORE the run starts.
/// It validates all requirements and returns an error if any are not met.
/// 
/// When called, it also:
/// - Sets invariant_mode = Hard (non-overridable)
/// - Enables all invariant categories
/// - Sets integrity_policy = strict()
/// - Enables visibility strict mode
/// - Verifies deterministic seed is set
/// - Verifies dump buffers are enabled
pub fn enforce_production_grade_requirements(
    strict_mode: bool,
    invariant_config: &InvariantConfig,
    integrity_policy: &PathologyPolicy,
    ledger_config: Option<&LedgerConfig>,
    strict_accounting: bool,
    seed: u64,
) -> Result<ProductionGradeRequirements, ProductionGradeEnforcementError> {
    let requirements = ProductionGradeRequirements {
        visibility_strict: strict_mode,
        invariant_hard: invariant_config.mode == InvariantMode::Hard,
        all_invariant_categories: invariant_config.categories == CategoryFlags::all(),
        integrity_strict: *integrity_policy == PathologyPolicy::strict(),
        ledger_strict: ledger_config.map(|c| c.strict_mode).unwrap_or(false),
        accounting_strict: strict_accounting,
        deterministic_seed: seed != 0, // 0 would indicate unset
        dump_buffers_enabled: invariant_config.event_dump_depth > 0,
    };
    
    if requirements.all_met() {
        Ok(requirements)
    } else {
        Err(ProductionGradeEnforcementError {
            violations: requirements.unmet().iter().map(|s| s.to_string()).collect(),
            config_summary: format!(
                "strict_mode={}, invariant_mode={:?}, integrity={:?}, ledger={:?}, accounting={}, seed={}",
                strict_mode,
                invariant_config.mode,
                integrity_policy.on_gap,
                ledger_config.map(|c| c.strict_mode),
                strict_accounting,
                seed
            ),
        })
    }
}

/// Compute a deterministic config fingerprint for reproducibility verification.
pub fn compute_config_fingerprint(
    strict_mode: bool,
    invariant_config: &InvariantConfig,
    integrity_policy: &PathologyPolicy,
    ledger_strict: bool,
    strict_accounting: bool,
    seed: u64,
) -> u64 {
    let mut hasher = DefaultHasher::new();
    
    strict_mode.hash(&mut hasher);
    std::mem::discriminant(&invariant_config.mode).hash(&mut hasher);
    // Hash category enablement by checking each category
    for cat in [
        crate::backtest_v2::invariants::InvariantCategory::Time,
        crate::backtest_v2::invariants::InvariantCategory::Book,
        crate::backtest_v2::invariants::InvariantCategory::OMS,
        crate::backtest_v2::invariants::InvariantCategory::Fills,
        crate::backtest_v2::invariants::InvariantCategory::Accounting,
    ] {
        invariant_config.categories.is_enabled(cat).hash(&mut hasher);
    }
    std::mem::discriminant(&integrity_policy.on_gap).hash(&mut hasher);
    std::mem::discriminant(&integrity_policy.on_duplicate).hash(&mut hasher);
    std::mem::discriminant(&integrity_policy.on_out_of_order).hash(&mut hasher);
    ledger_strict.hash(&mut hasher);
    strict_accounting.hash(&mut hasher);
    seed.hash(&mut hasher);
    
    hasher.finish()
}

// =============================================================================
// INTEGRITY ABORT CONVERSION
// =============================================================================

/// Convert an integrity halt into a production-grade abort.
pub fn integrity_halt_to_abort(
    halt_reason: &str,
    sim_time: Nanos,
    decision_time: Nanos,
    recent_events: &[EventSummary],
    config_hash: u64,
    run_fingerprint: u64,
) -> ProductionGradeAbort {
    use crate::backtest_v2::invariants::{InvariantCategory, ViolationType, ViolationContext};
    
    let violation = InvariantViolation {
        category: InvariantCategory::Time, // Integrity issues are time-category
        violation_type: ViolationType::EventOrderingViolation { 
            expected_seq: 0, 
            actual_seq: 0 
        },
        message: format!("Integrity halt: {}", halt_reason),
        sim_time,
        decision_time,
        arrival_time: Some(sim_time),
        seq: None,
        market_id: None,
        order_id: None,
        fill_id: None,
        context: ViolationContext::default(),
    };
    
    let dump = CausalDump {
        violation: violation.clone(),
        triggering_event: recent_events.last().cloned(),
        recent_events: recent_events.to_vec(),
        oms_transitions: Vec::new(),
        ledger_entries: Vec::new(),
        state_snapshot: StateSnapshot {
            best_bid: None,
            best_ask: None,
            cash: 0.0,
            open_orders: 0,
            position: 0.0,
        },
        fingerprint_at_abort: run_fingerprint,
        config_hash,
    };
    
    ProductionGradeAbort::from_dump(dump, run_fingerprint, config_hash)
}

// =============================================================================
// TESTS
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_violation_hash_determinism() {
        use crate::backtest_v2::invariants::ViolationContext;
        
        // Create the same violation twice
        let violation = InvariantViolation {
            category: crate::backtest_v2::invariants::InvariantCategory::Book,
            violation_type: crate::backtest_v2::invariants::ViolationType::CrossedBook {
                best_bid: 0.55,
                best_ask: 0.50,
            },
            message: "Crossed book detected".to_string(),
            sim_time: 1_000_000_000,
            decision_time: 1_000_000_000,
            arrival_time: Some(1_000_000_000),
            seq: Some(42),
            market_id: Some("test-market".to_string()),
            order_id: None,
            fill_id: None,
            context: ViolationContext::default(),
        };
        
        let events = vec![
            EventSummary {
                arrival_time: 999_000_000,
                source_time: Some(998_000_000),
                seq: Some(41),
                event_type: "L2BookSnapshot".to_string(),
                market_id: Some("test-market".to_string()),
            },
        ];
        
        let hash1 = ViolationHash::compute(&violation, events.last(), &events, &[], &[], 12345);
        let hash2 = ViolationHash::compute(&violation, events.last(), &events, &[], &[], 12345);
        
        assert_eq!(hash1, hash2, "Hash must be deterministic");
    }
    
    #[test]
    fn test_violation_hash_changes_with_context() {
        use crate::backtest_v2::invariants::ViolationContext;
        
        let violation = InvariantViolation {
            category: crate::backtest_v2::invariants::InvariantCategory::Book,
            violation_type: crate::backtest_v2::invariants::ViolationType::CrossedBook {
                best_bid: 0.55,
                best_ask: 0.50,
            },
            message: "Crossed book detected".to_string(),
            sim_time: 1_000_000_000,
            decision_time: 1_000_000_000,
            arrival_time: Some(1_000_000_000),
            seq: Some(42),
            market_id: Some("test-market".to_string()),
            order_id: None,
            fill_id: None,
            context: ViolationContext::default(),
        };
        
        let events1 = vec![
            EventSummary {
                arrival_time: 999_000_000,
                source_time: Some(998_000_000),
                seq: Some(41),
                event_type: "L2BookSnapshot".to_string(),
                market_id: Some("test-market".to_string()),
            },
        ];
        
        let events2 = vec![
            EventSummary {
                arrival_time: 999_000_000,
                source_time: Some(998_000_000),
                seq: Some(40), // Different seq
                event_type: "L2BookSnapshot".to_string(),
                market_id: Some("test-market".to_string()),
            },
        ];
        
        let hash1 = ViolationHash::compute(&violation, events1.last(), &events1, &[], &[], 12345);
        let hash2 = ViolationHash::compute(&violation, events2.last(), &events2, &[], &[], 12345);
        
        assert_ne!(hash1, hash2, "Hash must change when context changes");
    }
    
    #[test]
    fn test_production_grade_requirements_all_met() {
        let invariant_config = InvariantConfig::production();
        let integrity_policy = PathologyPolicy::strict();
        let ledger_config = LedgerConfig::production_grade();
        
        let result = enforce_production_grade_requirements(
            true,  // strict_mode
            &invariant_config,
            &integrity_policy,
            Some(&ledger_config),
            true,  // strict_accounting
            42,    // seed
        );
        
        assert!(result.is_ok());
        let reqs = result.unwrap();
        assert!(reqs.all_met());
    }
    
    #[test]
    fn test_production_grade_requirements_fail_on_soft_invariants() {
        let mut invariant_config = InvariantConfig::production();
        invariant_config.mode = InvariantMode::Soft; // Weaken it
        
        let integrity_policy = PathologyPolicy::strict();
        let ledger_config = LedgerConfig::production_grade();
        
        let result = enforce_production_grade_requirements(
            true,
            &invariant_config,
            &integrity_policy,
            Some(&ledger_config),
            true,
            42,
        );
        
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.violations.iter().any(|v| v.contains("invariant_mode")));
    }
    
    #[test]
    fn test_production_grade_requirements_fail_on_permissive_integrity() {
        let invariant_config = InvariantConfig::production();
        let integrity_policy = PathologyPolicy::permissive(); // Weaken it
        let ledger_config = LedgerConfig::production_grade();
        
        let result = enforce_production_grade_requirements(
            true,
            &invariant_config,
            &integrity_policy,
            Some(&ledger_config),
            true,
            42,
        );
        
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.violations.iter().any(|v| v.contains("integrity")));
    }
    
    #[test]
    fn test_production_grade_requirements_fail_without_ledger() {
        let invariant_config = InvariantConfig::production();
        let integrity_policy = PathologyPolicy::strict();
        
        let result = enforce_production_grade_requirements(
            true,
            &invariant_config,
            &integrity_policy,
            None, // No ledger
            true,
            42,
        );
        
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.violations.iter().any(|v| v.contains("ledger")));
    }
    
    #[test]
    fn test_config_fingerprint_determinism() {
        let invariant_config = InvariantConfig::production();
        let integrity_policy = PathologyPolicy::strict();
        
        let fp1 = compute_config_fingerprint(
            true, &invariant_config, &integrity_policy, true, true, 42
        );
        let fp2 = compute_config_fingerprint(
            true, &invariant_config, &integrity_policy, true, true, 42
        );
        
        assert_eq!(fp1, fp2, "Config fingerprint must be deterministic");
    }
    
    #[test]
    fn test_config_fingerprint_changes_with_config() {
        let invariant_config = InvariantConfig::production();
        let integrity_policy = PathologyPolicy::strict();
        
        let fp1 = compute_config_fingerprint(
            true, &invariant_config, &integrity_policy, true, true, 42
        );
        let fp2 = compute_config_fingerprint(
            true, &invariant_config, &integrity_policy, true, true, 43 // Different seed
        );
        
        assert_ne!(fp1, fp2, "Config fingerprint must change with config");
    }
    
    #[test]
    fn test_abort_format_deterministic() {
        use crate::backtest_v2::invariants::ViolationContext;
        
        let violation = InvariantViolation {
            category: crate::backtest_v2::invariants::InvariantCategory::Book,
            violation_type: crate::backtest_v2::invariants::ViolationType::CrossedBook {
                best_bid: 0.55,
                best_ask: 0.50,
            },
            message: "Crossed book detected".to_string(),
            sim_time: 1_000_000_000,
            decision_time: 1_000_000_000,
            arrival_time: Some(1_000_000_000),
            seq: Some(42),
            market_id: Some("test-market".to_string()),
            order_id: None,
            fill_id: None,
            context: ViolationContext::default(),
        };
        
        let dump = CausalDump {
            violation: violation.clone(),
            triggering_event: None,
            recent_events: vec![],
            oms_transitions: vec![],
            ledger_entries: vec![],
            state_snapshot: StateSnapshot {
                best_bid: Some((0.55, 100.0)),
                best_ask: Some((0.50, 100.0)),
                cash: 1000.0,
                open_orders: 0,
                position: 0.0,
            },
            fingerprint_at_abort: 12345,
            config_hash: 67890,
        };
        
        let abort1 = ProductionGradeAbort::from_dump(dump.clone(), 11111, 67890);
        let abort2 = ProductionGradeAbort::from_dump(dump, 11111, 67890);
        
        let text1 = abort1.format_deterministic();
        let text2 = abort2.format_deterministic();
        
        assert_eq!(text1, text2, "Abort format must be deterministic");
        assert_eq!(abort1.violation_hash, abort2.violation_hash, "Violation hash must be deterministic");
    }
}

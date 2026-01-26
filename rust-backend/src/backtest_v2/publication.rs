//! Publication Gating for Backtest Runs
//!
//! This module enforces strict requirements for publishing backtest runs to the public API.
//! A run CANNOT be "published" (exposed publicly) unless ALL of the following are true:
//!
//! 1. `production_grade` mode is enabled
//! 2. Dataset readiness is compatible with the strategy type
//! 3. GateSuite has been executed AND passed
//! 4. TrustLevel is final (not Unknown or Bypassed)
//! 5. RunFingerprint is present and complete
//!
//! This gating is critical for institutional and audit-facing use cases where
//! only certified, trustworthy results should be publicly accessible.

use crate::backtest_v2::data_contract::DatasetReadiness;
use crate::backtest_v2::fingerprint::RunFingerprint;
use crate::backtest_v2::gate_suite::TrustLevel;
use crate::backtest_v2::orchestrator::{BacktestConfig, BacktestResults, MakerFillModel};
use serde::{Deserialize, Serialize};
use std::fmt;

// =============================================================================
// PUBLICATION ERROR
// =============================================================================

/// Reasons why a run cannot be published.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PublicationError {
    /// Run was not executed in production-grade mode.
    NotProductionGrade,

    /// Dataset readiness is not compatible with the strategy.
    DatasetNotCompatible {
        actual: String,
        required: String,
        reason: String,
    },

    /// GateSuite was not executed.
    GateSuiteNotExecuted,

    /// GateSuite was executed but failed.
    GateSuiteFailed { failed_gates: Vec<String> },

    /// GateSuite was explicitly bypassed.
    GateSuiteBypassed,

    /// TrustLevel is not final (Unknown or still being evaluated).
    TrustLevelNotFinal { actual: String },

    /// TrustLevel is Untrusted (run failed trust requirements).
    TrustLevelUntrusted { reasons: Vec<String> },

    /// RunFingerprint is missing entirely.
    FingerprintMissing,

    /// RunFingerprint is present but incomplete.
    FingerprintIncomplete { missing_components: Vec<String> },

    /// Strict accounting was not enabled.
    StrictAccountingDisabled,

    /// Invariant violations were detected during the run.
    InvariantViolationsDetected { count: u64 },

    /// Accounting violations were detected during the run.
    AccountingViolationDetected { violation: String },
}

impl fmt::Display for PublicationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotProductionGrade => {
                write!(f, "Run was not executed in production-grade mode")
            }
            Self::DatasetNotCompatible { actual, required, reason } => {
                write!(
                    f,
                    "Dataset readiness '{}' is not compatible (requires '{}'): {}",
                    actual, required, reason
                )
            }
            Self::GateSuiteNotExecuted => {
                write!(f, "GateSuite was not executed")
            }
            Self::GateSuiteFailed { failed_gates } => {
                write!(f, "GateSuite failed: {}", failed_gates.join(", "))
            }
            Self::GateSuiteBypassed => {
                write!(f, "GateSuite was explicitly bypassed")
            }
            Self::TrustLevelNotFinal { actual } => {
                write!(f, "TrustLevel is not final: {}", actual)
            }
            Self::TrustLevelUntrusted { reasons } => {
                write!(f, "Run is untrusted: {}", reasons.join("; "))
            }
            Self::FingerprintMissing => {
                write!(f, "RunFingerprint is missing")
            }
            Self::FingerprintIncomplete { missing_components } => {
                write!(
                    f,
                    "RunFingerprint is incomplete, missing: {}",
                    missing_components.join(", ")
                )
            }
            Self::StrictAccountingDisabled => {
                write!(f, "Strict accounting was not enabled")
            }
            Self::InvariantViolationsDetected { count } => {
                write!(f, "{} invariant violation(s) detected during run", count)
            }
            Self::AccountingViolationDetected { violation } => {
                write!(f, "Accounting violation detected: {}", violation)
            }
        }
    }
}

impl std::error::Error for PublicationError {}

// =============================================================================
// PUBLICATION DECISION
// =============================================================================

/// Result of publication gating evaluation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PublicationDecision {
    /// Run can be published - all requirements satisfied.
    Approved,

    /// Run cannot be published - one or more requirements failed.
    Rejected { errors: Vec<PublicationError> },
}

impl PublicationDecision {
    /// Check if publication is approved.
    pub fn is_approved(&self) -> bool {
        matches!(self, Self::Approved)
    }

    /// Get all rejection reasons (empty if approved).
    pub fn rejection_reasons(&self) -> &[PublicationError] {
        match self {
            Self::Approved => &[],
            Self::Rejected { errors } => errors,
        }
    }

    /// Format as a compact summary.
    pub fn format_compact(&self) -> String {
        match self {
            Self::Approved => "APPROVED".to_string(),
            Self::Rejected { errors } => {
                format!("REJECTED ({} reasons)", errors.len())
            }
        }
    }
}

impl fmt::Display for PublicationDecision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.format_compact())
    }
}

// =============================================================================
// PUBLICATION GATE
// =============================================================================

/// PublicationGate is the SOLE PATHWAY for determining if a run can be published.
///
/// A run CANNOT be exposed via the public API unless `PublicationGate::evaluate()`
/// returns `PublicationDecision::Approved`.
///
/// This is a critical security and integrity gate for institutional use.
pub struct PublicationGate;

impl PublicationGate {
    /// Evaluate whether a run can be published.
    ///
    /// Returns `PublicationDecision::Approved` if ALL requirements are met,
    /// `PublicationDecision::Rejected` with all failure reasons otherwise.
    ///
    /// ALL checks are performed even if early ones fail, so the full set of
    /// rejection reasons is collected.
    pub fn evaluate(
        config: &BacktestConfig,
        results: &BacktestResults,
    ) -> PublicationDecision {
        let mut errors = Vec::new();

        // Check 1: production_grade mode
        Self::check_production_grade(config, results, &mut errors);

        // Check 2: Dataset readiness compatibility
        Self::check_dataset_readiness(config, results, &mut errors);

        // Check 3: GateSuite execution and pass status
        Self::check_gate_suite(results, &mut errors);

        // Check 4: TrustLevel is final and Trusted
        Self::check_trust_level(results, &mut errors);

        // Check 5: RunFingerprint is present and complete
        Self::check_fingerprint(results, &mut errors);

        // Check 6: Strict accounting enabled and no violations
        Self::check_accounting(config, results, &mut errors);

        // Check 7: No invariant violations
        Self::check_invariants(results, &mut errors);

        if errors.is_empty() {
            PublicationDecision::Approved
        } else {
            PublicationDecision::Rejected { errors }
        }
    }

    /// Quick check if a run is likely publishable (without full evaluation).
    /// Use for fast filtering before detailed evaluation.
    pub fn quick_check(config: &BacktestConfig, results: &BacktestResults) -> bool {
        config.production_grade
            && results.gate_suite_passed
            && results.strict_accounting_enabled
            && matches!(results.trust_level, TrustLevel::Trusted)
            && results.run_fingerprint.is_some()
    }

    /// Require publication approval, returning error if not approved.
    pub fn require_approved(
        config: &BacktestConfig,
        results: &BacktestResults,
    ) -> Result<(), PublicationGateError> {
        let decision = Self::evaluate(config, results);
        match decision {
            PublicationDecision::Approved => Ok(()),
            PublicationDecision::Rejected { errors } => {
                Err(PublicationGateError { errors })
            }
        }
    }

    // =========================================================================
    // INDIVIDUAL CHECKS
    // =========================================================================

    fn check_production_grade(
        config: &BacktestConfig,
        results: &BacktestResults,
        errors: &mut Vec<PublicationError>,
    ) {
        if !config.production_grade || !results.production_grade {
            errors.push(PublicationError::NotProductionGrade);
        }
    }

    fn check_dataset_readiness(
        config: &BacktestConfig,
        results: &BacktestResults,
        errors: &mut Vec<PublicationError>,
    ) {
        let readiness = results.dataset_readiness;

        // NonRepresentative dataset can never be published
        if readiness == DatasetReadiness::NonRepresentative {
            errors.push(PublicationError::DatasetNotCompatible {
                actual: format!("{:?}", readiness),
                required: "TakerOnly or MakerViable".to_string(),
                reason: "NonRepresentative datasets cannot produce publishable results".to_string(),
            });
            return;
        }

        // If maker fills were requested/used, require MakerViable
        let uses_maker = config.maker_fill_model == MakerFillModel::ExplicitQueue
            && results.maker_fills > 0;

        if uses_maker && readiness != DatasetReadiness::MakerViable {
            errors.push(PublicationError::DatasetNotCompatible {
                actual: format!("{:?}", readiness),
                required: "MakerViable".to_string(),
                reason: "Maker strategy requires MakerViable dataset".to_string(),
            });
        }
    }

    fn check_gate_suite(results: &BacktestResults, errors: &mut Vec<PublicationError>) {
        // Check if gate suite was bypassed
        if matches!(results.trust_level, TrustLevel::Bypassed) {
            errors.push(PublicationError::GateSuiteBypassed);
            return;
        }

        // Check if gate suite passed
        if !results.gate_suite_passed {
            let failed_gates: Vec<String> = results
                .gate_failures
                .iter()
                .map(|(name, reason)| format!("{}: {}", name, reason))
                .collect();

            errors.push(PublicationError::GateSuiteFailed { failed_gates });
        }
    }

    fn check_trust_level(results: &BacktestResults, errors: &mut Vec<PublicationError>) {
        match &results.trust_level {
            TrustLevel::Trusted => {
                // Good - run is trusted
            }
            TrustLevel::Unknown => {
                errors.push(PublicationError::TrustLevelNotFinal {
                    actual: "Unknown".to_string(),
                });
            }
            TrustLevel::Bypassed => {
                errors.push(PublicationError::TrustLevelNotFinal {
                    actual: "Bypassed".to_string(),
                });
            }
            TrustLevel::Untrusted { reasons } => {
                let reason_strings: Vec<String> = reasons
                    .iter()
                    .map(|r| r.description.clone())
                    .collect();
                errors.push(PublicationError::TrustLevelUntrusted {
                    reasons: reason_strings,
                });
            }
        }
    }

    fn check_fingerprint(results: &BacktestResults, errors: &mut Vec<PublicationError>) {
        let Some(ref fp) = results.run_fingerprint else {
            errors.push(PublicationError::FingerprintMissing);
            return;
        };

        // Check for completeness
        let mut missing = Vec::new();

        if fp.code.hash == 0 {
            missing.push("code".to_string());
        }
        if fp.config.hash == 0 {
            missing.push("config".to_string());
        }
        if fp.dataset.hash == 0 {
            missing.push("dataset".to_string());
        }
        if fp.seed.hash == 0 {
            missing.push("seed".to_string());
        }
        if fp.behavior.hash == 0 && fp.behavior.event_count == 0 {
            missing.push("behavior".to_string());
        }

        if !missing.is_empty() {
            errors.push(PublicationError::FingerprintIncomplete {
                missing_components: missing,
            });
        }
    }

    fn check_accounting(
        config: &BacktestConfig,
        results: &BacktestResults,
        errors: &mut Vec<PublicationError>,
    ) {
        if !config.strict_accounting || !results.strict_accounting_enabled {
            errors.push(PublicationError::StrictAccountingDisabled);
        }

        if let Some(ref violation) = results.first_accounting_violation {
            errors.push(PublicationError::AccountingViolationDetected {
                violation: violation.clone(),
            });
        }
    }

    fn check_invariants(results: &BacktestResults, errors: &mut Vec<PublicationError>) {
        if results.invariant_violations_detected > 0 {
            errors.push(PublicationError::InvariantViolationsDetected {
                count: results.invariant_violations_detected,
            });
        }
    }
}

// =============================================================================
// PUBLICATION GATE ERROR
// =============================================================================

/// Error returned when publication is required but not approved.
#[derive(Debug, Clone)]
pub struct PublicationGateError {
    pub errors: Vec<PublicationError>,
}

impl fmt::Display for PublicationGateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "Publication rejected with {} reason(s):", self.errors.len())?;
        for (i, err) in self.errors.iter().enumerate() {
            writeln!(f, "  {}. {}", i + 1, err)?;
        }
        Ok(())
    }
}

impl std::error::Error for PublicationGateError {}

// =============================================================================
// PUBLICATION STATUS
// =============================================================================

/// Publication status stored with each artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PublicationStatus {
    /// Run is internal only - not exposed via public API.
    Internal,
    /// Run is published - exposed via public API.
    Published,
    /// Run was previously published but has been retracted.
    Retracted,
}

impl Default for PublicationStatus {
    fn default() -> Self {
        Self::Internal
    }
}

impl PublicationStatus {
    /// Check if this status allows public access.
    pub fn is_public(&self) -> bool {
        matches!(self, Self::Published)
    }

    /// Convert to integer for database storage.
    pub fn to_db_int(&self) -> i32 {
        match self {
            Self::Internal => 0,
            Self::Published => 1,
            Self::Retracted => 2,
        }
    }

    /// Convert from database integer.
    pub fn from_db_int(val: i32) -> Self {
        match val {
            1 => Self::Published,
            2 => Self::Retracted,
            _ => Self::Internal,
        }
    }
}

// =============================================================================
// TESTS
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backtest_v2::gate_suite::GateFailureReason;

    fn make_production_config() -> BacktestConfig {
        BacktestConfig::production_grade_15m_updown()
    }

    fn make_publishable_results() -> BacktestResults {
        use crate::backtest_v2::fingerprint::*;
        use crate::backtest_v2::invariants::InvariantMode;
        use crate::backtest_v2::settlement::SettlementModel;
        use crate::backtest_v2::sim_adapter::{OmsParityMode, OmsParityStats};

        let mut results = BacktestResults::default();
        results.production_grade = true;
        results.strict_accounting_enabled = true;
        results.gate_suite_passed = true;
        results.trust_level = TrustLevel::Trusted;
        results.dataset_readiness = DatasetReadiness::MakerViable;
        results.invariant_mode = InvariantMode::Hard;
        results.invariant_violations_detected = 0;
        results.settlement_model = SettlementModel::ExactSpec;
        results.maker_fills_valid = true;
        results.oms_parity = Some(OmsParityStats {
            mode: OmsParityMode::Full,
            valid_for_production: true,
            ..Default::default()
        });

        // Add complete fingerprint
        results.run_fingerprint = Some(RunFingerprint {
            version: FINGERPRINT_VERSION.to_string(),
            strategy: StrategyFingerprint {
                name: "test_strategy".to_string(),
                version: "1.0.0".to_string(),
                code_hash: "abc123".to_string(),
                hash: 11111,
            },
            code: CodeFingerprint::new(),
            config: ConfigFingerprint {
                settlement_reference_rule: Some("LastUpdateAtOrBeforeCutoff".to_string()),
                settlement_tie_rule: Some("NoWins".to_string()),
                chainlink_feed_id: None,
                oracle_chain_id: None,
                oracle_feed_proxies: vec![],
                oracle_decimals: vec![],
                oracle_visibility_rule: None,
                oracle_rounding_policy: None,
                oracle_config_hash: None,
                latency_model: "Fixed".to_string(),
                order_latency_ns: Some(1_000_000),
                oms_parity_mode: "Full".to_string(),
                maker_fill_model: "ExplicitQueue".to_string(),
                integrity_policy: "Strict".to_string(),
                invariant_mode: "Hard".to_string(),
                fee_rate_bps: Some(10),
                strategy_params_hash: 12345,
                arrival_policy: "RecordedArrival".to_string(),
                strict_accounting: true,
                production_grade: true,
                allow_non_production: false,
                hash: 67890,
            },
            dataset: DatasetFingerprint {
                classification: "FullIncremental".to_string(),
                readiness: "MakerViable".to_string(),
                orderbook_type: "FullIncrementalL2DeltasWithExchangeSeq".to_string(),
                trade_type: "TradePrints".to_string(),
                arrival_semantics: "RecordedArrival".to_string(),
                streams: vec![],
                hash: 11111,
            },
            seed: SeedFingerprint {
                primary_seed: 42,
                sub_seeds: vec![],
                hash: 22222,
            },
            behavior: BehaviorFingerprint {
                event_count: 100,
                hash: 33333,
            },
            registry: None,
            hash: 99999,
            hash_hex: "000000000001869f".to_string(),
        });

        results
    }

    #[test]
    fn test_publishable_run_is_approved() {
        let config = make_production_config();
        let results = make_publishable_results();

        let decision = PublicationGate::evaluate(&config, &results);

        assert!(
            decision.is_approved(),
            "Expected Approved, got {:?}",
            decision
        );
    }

    #[test]
    fn test_non_production_run_rejected() {
        let mut config = make_production_config();
        config.production_grade = false;
        config.allow_non_production = true;

        let results = make_publishable_results();

        let decision = PublicationGate::evaluate(&config, &results);

        assert!(!decision.is_approved());
        assert!(decision
            .rejection_reasons()
            .iter()
            .any(|e| matches!(e, PublicationError::NotProductionGrade)));
    }

    #[test]
    fn test_gate_suite_failed_rejected() {
        let config = make_production_config();
        let mut results = make_publishable_results();
        results.gate_suite_passed = false;
        results.gate_failures = vec![
            ("Gate A".to_string(), "Reason A".to_string()),
            ("Gate B".to_string(), "Reason B".to_string()),
        ];

        let decision = PublicationGate::evaluate(&config, &results);

        assert!(!decision.is_approved());
        assert!(decision
            .rejection_reasons()
            .iter()
            .any(|e| matches!(e, PublicationError::GateSuiteFailed { .. })));
    }

    #[test]
    fn test_untrusted_run_rejected() {
        let config = make_production_config();
        let mut results = make_publishable_results();
        results.trust_level = TrustLevel::Untrusted {
            reasons: vec![GateFailureReason::new(
                "test",
                "test reason",
                "field",
                "actual",
                "expected",
            )],
        };

        let decision = PublicationGate::evaluate(&config, &results);

        assert!(!decision.is_approved());
        assert!(decision
            .rejection_reasons()
            .iter()
            .any(|e| matches!(e, PublicationError::TrustLevelUntrusted { .. })));
    }

    #[test]
    fn test_missing_fingerprint_rejected() {
        let config = make_production_config();
        let mut results = make_publishable_results();
        results.run_fingerprint = None;

        let decision = PublicationGate::evaluate(&config, &results);

        assert!(!decision.is_approved());
        assert!(decision
            .rejection_reasons()
            .iter()
            .any(|e| matches!(e, PublicationError::FingerprintMissing)));
    }

    #[test]
    fn test_invariant_violations_rejected() {
        let config = make_production_config();
        let mut results = make_publishable_results();
        results.invariant_violations_detected = 5;

        let decision = PublicationGate::evaluate(&config, &results);

        assert!(!decision.is_approved());
        assert!(decision
            .rejection_reasons()
            .iter()
            .any(|e| matches!(e, PublicationError::InvariantViolationsDetected { count: 5 })));
    }

    #[test]
    fn test_multiple_failures_all_reported() {
        let mut config = make_production_config();
        config.production_grade = false;
        config.allow_non_production = true;
        config.strict_accounting = false;

        let mut results = make_publishable_results();
        results.production_grade = false;
        results.strict_accounting_enabled = false;
        results.gate_suite_passed = false;
        results.run_fingerprint = None;

        let decision = PublicationGate::evaluate(&config, &results);

        assert!(!decision.is_approved());
        // Should have multiple rejection reasons
        assert!(decision.rejection_reasons().len() >= 3);
    }

    #[test]
    fn test_quick_check_matches_full_evaluation() {
        let config = make_production_config();
        let results = make_publishable_results();

        let quick = PublicationGate::quick_check(&config, &results);
        let full = PublicationGate::evaluate(&config, &results);

        // Quick check should be true if full evaluation approves
        // (but quick check may be false even when full would approve in edge cases)
        if full.is_approved() {
            assert!(quick, "Quick check should be true when full evaluation approves");
        }
    }

    #[test]
    fn test_publication_status_db_roundtrip() {
        for status in [
            PublicationStatus::Internal,
            PublicationStatus::Published,
            PublicationStatus::Retracted,
        ] {
            let db_val = status.to_db_int();
            let restored = PublicationStatus::from_db_int(db_val);
            assert_eq!(status, restored);
        }
    }
}

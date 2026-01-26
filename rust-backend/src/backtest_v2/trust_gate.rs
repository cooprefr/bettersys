//! Trust Gate - Single Authoritative Source for Trust Classification
//!
//! This module implements the SOLE PATHWAY for labeling a backtest run as "Trusted".
//! No other code path may assign TrustLevel::Trusted - all trust classification must
//! flow through TrustGate::evaluate().
//!
//! # Trust Requirements
//!
//! A backtest run MAY be labeled "Trusted" if and only if ALL of the following are true:
//!
//! 1. GateSuite has been executed
//! 2. GateSuite TrustLevel == Trusted
//! 3. Sensitivity sweeps have been executed
//! 4. Sensitivity results fall within configured tolerances
//! 5. A reproducible RunFingerprint is present and complete
//! 6. production_grade == true
//! 7. DatasetReadiness allows the claimed strategy type (Maker/Taker)
//!
//! If ANY condition is false, the run MUST be labeled Untrusted.
//!
//! # Usage
//!
//! ```ignore
//! let decision = TrustGate::evaluate(
//!     &config,
//!     &results,
//!     gate_suite_report.as_ref(),
//!     sensitivity_report.as_ref(),
//!     run_fingerprint.as_ref(),
//! );
//!
//! match decision {
//!     TrustDecision::Trusted => { /* proceed */ }
//!     TrustDecision::Untrusted { reasons } => {
//!         // Handle failure - cannot claim trusted status
//!         for reason in &reasons {
//!             eprintln!("Trust failure: {}", reason);
//!         }
//!     }
//! }
//! ```

use crate::backtest_v2::data_contract::DatasetReadiness;
use crate::backtest_v2::fingerprint::RunFingerprint;
use crate::backtest_v2::gate_suite::{GateSuiteReport, TrustLevel as GateTrustLevel};
use crate::backtest_v2::orchestrator::{BacktestConfig, BacktestResults, MakerFillModel};
use crate::backtest_v2::sensitivity::{SensitivityReport, TrustRecommendation};
use serde::{Deserialize, Serialize};

// =============================================================================
// TRUST FAILURE REASONS
// =============================================================================

/// Explicit enumeration of all reasons why a run cannot be labeled Trusted.
///
/// This is a CLOSED enum - all possible trust failure reasons are enumerated here.
/// Each variant must be human-readable and appear in reports/logs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrustFailureReason {
    /// GateSuite was not executed at all.
    MissingGateSuite,

    /// GateSuite was executed but one or more gates failed.
    GateSuiteFailed {
        /// Names of failed gates.
        failed_gates: Vec<String>,
        /// Trust level returned by GateSuite.
        trust_level: String,
    },

    /// GateSuite was explicitly bypassed (GateMode::Disabled).
    GateSuiteBypassed,

    /// Sensitivity analysis was not executed.
    MissingSensitivityAnalysis,

    /// Sensitivity results indicate fragility or instability.
    SensitivityOutsideTolerance {
        /// Which sensitivity dimension failed.
        dimension: String,
        /// Human-readable reason.
        reason: String,
    },

    /// RunFingerprint was not computed.
    MissingRunFingerprint,

    /// RunFingerprint is present but incomplete (missing components).
    IncompleteRunFingerprint {
        /// Which components are missing.
        missing_components: Vec<String>,
    },

    /// RunFingerprint indicates non-reproducible behavior.
    FingerprintNotReproducible {
        /// Reason for non-reproducibility.
        reason: String,
    },

    /// production_grade flag is false.
    ProductionGradeDisabled,

    /// Dataset readiness does not support the claimed strategy type.
    DatasetNotCompatibleWithStrategy {
        /// The dataset readiness level.
        dataset_readiness: String,
        /// The strategy type that was attempted.
        strategy_type: String,
        /// Explanation of the incompatibility.
        reason: String,
    },

    /// Strict accounting was not enabled (required for production-grade).
    StrictAccountingDisabled,

    /// Accounting violations were detected during the run.
    AccountingViolationDetected {
        /// Description of the first violation.
        violation: String,
    },

    /// Invariant mode is not Hard (required for production-grade).
    InvariantModeNotHard {
        /// The actual invariant mode.
        actual_mode: String,
    },

    /// Invariant violations were detected during the run.
    InvariantViolationDetected {
        /// Count of violations.
        count: u64,
        /// First violation description.
        first_violation: String,
    },

    /// Hermetic strategy mode was not enabled (required for production-grade).
    HermeticModeDisabled,

    /// Settlement model is not ExactSpec (required for production-grade).
    SettlementModelNotExact {
        /// The actual settlement model.
        actual_model: String,
    },

    /// OMS parity mode is not Full (required for production-grade).
    OmsParityModeNotFull {
        /// The actual OMS parity mode.
        actual_mode: String,
    },

    /// Maker fills are invalid for this dataset.
    MakerFillsInvalid {
        /// Reason why maker fills are invalid.
        reason: String,
    },

    /// Settlement reference mapping version mismatch.
    SettlementReferenceMappingMismatch {
        /// Expected mapping version.
        expected_version: u32,
        /// Actual mapping version in dataset.
        actual_version: u32,
    },

    /// Settlement reference mapping not present in dataset.
    SettlementReferenceMappingMissing,

    /// Settlement reference fallback rate exceeded threshold.
    SettlementReferenceFallbackRateExceeded {
        /// The observed fallback rate in basis points (100 = 1%).
        fallback_rate_bps: u32,
        /// The threshold for production-grade in basis points.
        threshold_bps: u32,
    },

    /// Settlement reference carry-forward used in production-grade run.
    SettlementReferenceCarryForwardUsed {
        /// Number of carry-forward ticks.
        carry_forward_count: u64,
        /// Total ticks.
        total_ticks: u64,
    },

    /// Settlement reference mapping invariant validation failed.
    SettlementReferenceMappingInvalid {
        /// Validation errors.
        errors: Vec<String>,
    },

    /// Market registry is required but not provided.
    MissingMarketRegistry {
        /// Context explaining why registry is required.
        context: String,
    },

    /// Market registry is invalid or failed validation.
    InvalidMarketRegistry {
        /// Reason for invalidity.
        reason: String,
    },

    /// Dataset contains tokens not found in the market registry.
    DatasetRegistryIncompatible {
        /// Number of unknown tokens.
        unknown_token_count: usize,
        /// Sample of unknown tokens (first 3).
        sample_tokens: Vec<String>,
    },
}

impl TrustFailureReason {
    /// Get a human-readable short code for this failure reason.
    pub fn code(&self) -> &'static str {
        match self {
            Self::MissingGateSuite => "MISSING_GATE_SUITE",
            Self::GateSuiteFailed { .. } => "GATE_SUITE_FAILED",
            Self::GateSuiteBypassed => "GATE_SUITE_BYPASSED",
            Self::MissingSensitivityAnalysis => "MISSING_SENSITIVITY",
            Self::SensitivityOutsideTolerance { .. } => "SENSITIVITY_FRAGILE",
            Self::MissingRunFingerprint => "MISSING_FINGERPRINT",
            Self::IncompleteRunFingerprint { .. } => "INCOMPLETE_FINGERPRINT",
            Self::FingerprintNotReproducible { .. } => "FINGERPRINT_NOT_REPRODUCIBLE",
            Self::ProductionGradeDisabled => "PRODUCTION_GRADE_DISABLED",
            Self::DatasetNotCompatibleWithStrategy { .. } => "DATASET_INCOMPATIBLE",
            Self::StrictAccountingDisabled => "STRICT_ACCOUNTING_DISABLED",
            Self::AccountingViolationDetected { .. } => "ACCOUNTING_VIOLATION",
            Self::InvariantModeNotHard { .. } => "INVARIANT_MODE_NOT_HARD",
            Self::InvariantViolationDetected { .. } => "INVARIANT_VIOLATION",
            Self::HermeticModeDisabled => "HERMETIC_MODE_DISABLED",
            Self::SettlementModelNotExact { .. } => "SETTLEMENT_NOT_EXACT",
            Self::OmsParityModeNotFull { .. } => "OMS_PARITY_NOT_FULL",
            Self::MakerFillsInvalid { .. } => "MAKER_FILLS_INVALID",
            Self::SettlementReferenceMappingMismatch { .. } => "SETTLEMENT_REF_MAPPING_MISMATCH",
            Self::SettlementReferenceMappingMissing => "SETTLEMENT_REF_MAPPING_MISSING",
            Self::SettlementReferenceFallbackRateExceeded { .. } => "SETTLEMENT_REF_FALLBACK_EXCEEDED",
            Self::SettlementReferenceCarryForwardUsed { .. } => "SETTLEMENT_REF_CARRY_FORWARD_USED",
            Self::SettlementReferenceMappingInvalid { .. } => "SETTLEMENT_REF_MAPPING_INVALID",
            Self::MissingMarketRegistry { .. } => "MISSING_MARKET_REGISTRY",
            Self::InvalidMarketRegistry { .. } => "INVALID_MARKET_REGISTRY",
            Self::DatasetRegistryIncompatible { .. } => "DATASET_REGISTRY_INCOMPATIBLE",
        }
    }

    /// Get a human-readable description of this failure reason.
    pub fn description(&self) -> String {
        match self {
            Self::MissingGateSuite => {
                "GateSuite was not executed. All production-grade backtests MUST run the gate suite.".to_string()
            }
            Self::GateSuiteFailed { failed_gates, trust_level } => {
                format!(
                    "GateSuite failed (trust_level={}). Failed gates: {}",
                    trust_level,
                    failed_gates.join(", ")
                )
            }
            Self::GateSuiteBypassed => {
                "GateSuite was explicitly bypassed. Trust cannot be established without gate validation.".to_string()
            }
            Self::MissingSensitivityAnalysis => {
                "Sensitivity analysis was not executed. Production-grade backtests MUST include sensitivity sweeps.".to_string()
            }
            Self::SensitivityOutsideTolerance { dimension, reason } => {
                format!("Sensitivity analysis failed on {}: {}", dimension, reason)
            }
            Self::MissingRunFingerprint => {
                "RunFingerprint was not computed. Production-grade backtests MUST have a reproducibility fingerprint.".to_string()
            }
            Self::IncompleteRunFingerprint { missing_components } => {
                format!(
                    "RunFingerprint is incomplete. Missing components: {}",
                    missing_components.join(", ")
                )
            }
            Self::FingerprintNotReproducible { reason } => {
                format!("RunFingerprint indicates non-reproducible behavior: {}", reason)
            }
            Self::ProductionGradeDisabled => {
                "production_grade flag is false. Only production-grade backtests can be trusted.".to_string()
            }
            Self::DatasetNotCompatibleWithStrategy { dataset_readiness, strategy_type, reason } => {
                format!(
                    "Dataset readiness ({}) is not compatible with {} strategy: {}",
                    dataset_readiness, strategy_type, reason
                )
            }
            Self::StrictAccountingDisabled => {
                "strict_accounting is false. Production-grade backtests MUST use strict accounting.".to_string()
            }
            Self::AccountingViolationDetected { violation } => {
                format!("Accounting violation detected: {}", violation)
            }
            Self::InvariantModeNotHard { actual_mode } => {
                format!(
                    "Invariant mode is {} (must be Hard for production-grade)",
                    actual_mode
                )
            }
            Self::InvariantViolationDetected { count, first_violation } => {
                format!(
                    "{} invariant violation(s) detected. First: {}",
                    count, first_violation
                )
            }
            Self::HermeticModeDisabled => {
                "Hermetic strategy mode is disabled. Production-grade backtests MUST sandbox strategy code.".to_string()
            }
            Self::SettlementModelNotExact { actual_model } => {
                format!(
                    "Settlement model is {} (must be ExactSpec for production-grade)",
                    actual_model
                )
            }
            Self::OmsParityModeNotFull { actual_mode } => {
                format!(
                    "OMS parity mode is {} (must be Full for production-grade)",
                    actual_mode
                )
            }
            Self::MakerFillsInvalid { reason } => {
                format!("Maker fills are invalid: {}", reason)
            }
            Self::SettlementReferenceMappingMismatch { expected_version, actual_version } => {
                format!(
                    "Settlement reference mapping version mismatch: expected v{}, dataset has v{}",
                    expected_version, actual_version
                )
            }
            Self::SettlementReferenceMappingMissing => {
                "Settlement reference mapping not present in dataset. Production-grade 15M runs require explicit mapping.".to_string()
            }
            Self::SettlementReferenceFallbackRateExceeded { fallback_rate_bps, threshold_bps } => {
                format!(
                    "Settlement reference fallback rate {:.2}% exceeded threshold {:.2}%",
                    *fallback_rate_bps as f64 / 100.0, *threshold_bps as f64 / 100.0
                )
            }
            Self::SettlementReferenceCarryForwardUsed { carry_forward_count, total_ticks } => {
                format!(
                    "Settlement reference carry-forward used {} of {} ticks ({:.2}%). Production-grade disallows carry-forward.",
                    carry_forward_count, total_ticks, 
                    (*carry_forward_count as f64 / *total_ticks as f64) * 100.0
                )
            }
            Self::SettlementReferenceMappingInvalid { errors } => {
                format!(
                    "Settlement reference mapping validation failed: {}",
                    errors.join("; ")
                )
            }
            Self::MissingMarketRegistry { context } => {
                format!(
                    "Market registry is required but not provided: {}",
                    context
                )
            }
            Self::InvalidMarketRegistry { reason } => {
                format!("Market registry is invalid: {}", reason)
            }
            Self::DatasetRegistryIncompatible { unknown_token_count, sample_tokens } => {
                format!(
                    "Dataset contains {} token(s) not found in market registry. Samples: {}",
                    unknown_token_count,
                    sample_tokens.join(", ")
                )
            }
        }
    }
}

impl std::fmt::Display for TrustFailureReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{}] {}", self.code(), self.description())
    }
}

// =============================================================================
// TRUST DECISION
// =============================================================================

/// The authoritative trust decision for a backtest run.
///
/// This enum has exactly two variants - there is no "maybe" or "partial" trust.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrustDecision {
    /// All trust requirements are satisfied. The run can be labeled Trusted.
    Trusted,

    /// One or more trust requirements failed. The run MUST be labeled Untrusted.
    Untrusted {
        /// All reasons why trust cannot be established.
        /// This list is exhaustive - all failure reasons are included.
        reasons: Vec<TrustFailureReason>,
    },
}

impl TrustDecision {
    /// Check if this decision is Trusted.
    pub fn is_trusted(&self) -> bool {
        matches!(self, Self::Trusted)
    }

    /// Get all failure reasons (empty if Trusted).
    pub fn failure_reasons(&self) -> &[TrustFailureReason] {
        match self {
            Self::Trusted => &[],
            Self::Untrusted { reasons } => reasons,
        }
    }

    /// Get the count of failure reasons.
    pub fn failure_count(&self) -> usize {
        self.failure_reasons().len()
    }

    /// Convert to GateTrustLevel for backward compatibility.
    pub fn to_gate_trust_level(&self) -> GateTrustLevel {
        match self {
            Self::Trusted => GateTrustLevel::Trusted,
            Self::Untrusted { reasons } => {
                let gate_reasons = reasons
                    .iter()
                    .map(|r| crate::backtest_v2::gate_suite::GateFailureReason::new(
                        r.code(),
                        r.description(),
                        "trust_gate",
                        "false",
                        "true",
                    ))
                    .collect();
                GateTrustLevel::Untrusted { reasons: gate_reasons }
            }
        }
    }

    /// Format as a compact one-line summary.
    pub fn format_compact(&self) -> String {
        match self {
            Self::Trusted => "TRUSTED".to_string(),
            Self::Untrusted { reasons } => {
                format!("UNTRUSTED ({} reasons)", reasons.len())
            }
        }
    }

    /// Format as a detailed report.
    pub fn format_report(&self) -> String {
        let mut out = String::new();

        out.push_str("╔══════════════════════════════════════════════════════════════════════════════╗\n");
        out.push_str("║                          TRUST GATE DECISION                                 ║\n");
        out.push_str("╠══════════════════════════════════════════════════════════════════════════════╣\n");

        match self {
            Self::Trusted => {
                out.push_str("║  DECISION: ✓ TRUSTED                                                        ║\n");
                out.push_str("║                                                                              ║\n");
                out.push_str("║  All trust requirements satisfied:                                          ║\n");
                out.push_str("║    ✓ GateSuite executed and passed                                          ║\n");
                out.push_str("║    ✓ Sensitivity analysis within tolerances                                 ║\n");
                out.push_str("║    ✓ RunFingerprint present and complete                                    ║\n");
                out.push_str("║    ✓ Production-grade mode enabled                                          ║\n");
                out.push_str("║    ✓ Dataset compatible with strategy                                       ║\n");
            }
            Self::Untrusted { reasons } => {
                out.push_str("║  DECISION: ✗ UNTRUSTED                                                      ║\n");
                out.push_str("║                                                                              ║\n");
                out.push_str(&format!("║  {} trust requirement(s) failed:                                             ║\n", reasons.len()));
                out.push_str("╠══════════════════════════════════════════════════════════════════════════════╣\n");

                for (i, reason) in reasons.iter().enumerate() {
                    let code = reason.code();
                    let desc = reason.description();
                    
                    // Truncate description to fit
                    let max_desc_len = 70;
                    let display_desc = if desc.len() > max_desc_len {
                        format!("{}...", &desc[..max_desc_len - 3])
                    } else {
                        desc
                    };

                    out.push_str(&format!("║  {}. [{}]                                                   ║\n", i + 1, code));
                    out.push_str(&format!("║     {}  ║\n", display_desc));
                }
            }
        }

        out.push_str("╚══════════════════════════════════════════════════════════════════════════════╝\n");
        out
    }
}

impl std::fmt::Display for TrustDecision {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.format_compact())
    }
}

// =============================================================================
// TRUST GATE - THE SINGLE AUTHORITATIVE SOURCE
// =============================================================================

/// TrustGate is the SOLE PATHWAY for establishing trust in a backtest run.
///
/// # Invariant
///
/// No other code path may assign TrustLevel::Trusted. All trust classification
/// MUST flow through `TrustGate::evaluate()`.
///
/// # Evaluation Order
///
/// Checks are performed in a specific order. ALL checks are performed even if
/// early ones fail, so that the full set of failure reasons is collected.
pub struct TrustGate;

impl TrustGate {
    /// Evaluate trust eligibility for a backtest run.
    ///
    /// This is the ONLY method that can determine if a run is Trusted.
    ///
    /// # Arguments
    ///
    /// * `config` - The backtest configuration used
    /// * `results` - The backtest results
    /// * `gate_suite_report` - Optional GateSuite report (None if not run)
    /// * `sensitivity_report` - Optional Sensitivity report (None if not run)
    /// * `run_fingerprint` - Optional RunFingerprint (None if not computed)
    ///
    /// # Returns
    ///
    /// `TrustDecision::Trusted` if ALL requirements are satisfied,
    /// `TrustDecision::Untrusted` with all failure reasons otherwise.
    pub fn evaluate(
        config: &BacktestConfig,
        results: &BacktestResults,
        gate_suite_report: Option<&GateSuiteReport>,
        sensitivity_report: Option<&SensitivityReport>,
        run_fingerprint: Option<&RunFingerprint>,
    ) -> TrustDecision {
        let mut reasons = Vec::new();

        // Check 1: production_grade must be true
        Self::check_production_grade(config, &mut reasons);

        // Check 2: GateSuite must be executed and passed
        Self::check_gate_suite(config, gate_suite_report, &mut reasons);

        // Check 3: Sensitivity analysis must be executed and within tolerances
        Self::check_sensitivity(config, sensitivity_report, &mut reasons);

        // Check 4: RunFingerprint must be present and complete
        Self::check_fingerprint(run_fingerprint, &mut reasons);

        // Check 5: DatasetReadiness must allow the strategy type
        Self::check_dataset_readiness(config, results, &mut reasons);

        // Check 6: Strict accounting must be enabled and no violations
        Self::check_accounting(config, results, &mut reasons);

        // Check 7: Invariant mode must be Hard and no violations
        Self::check_invariants(config, results, &mut reasons);

        // Check 8: Hermetic mode must be enabled
        Self::check_hermetic(config, &mut reasons);

        // Check 9: Settlement model must be ExactSpec
        Self::check_settlement(config, results, &mut reasons);

        // Check 10: OMS parity must be Full
        Self::check_oms_parity(config, results, &mut reasons);

        // Check 11: Maker fills must be valid (if maker fills occurred)
        Self::check_maker_fills(results, &mut reasons);

        // Final decision
        if reasons.is_empty() {
            TrustDecision::Trusted
        } else {
            TrustDecision::Untrusted { reasons }
        }
    }

    /// Quick check if a run can potentially be trusted (without full evaluation).
    /// This is a fast-path that checks only the most critical requirements.
    pub fn quick_check(config: &BacktestConfig) -> bool {
        config.production_grade
            && config.strict_accounting
            && config.hermetic_config.enabled
            && config.sensitivity.enabled
            && config.gate_mode == crate::backtest_v2::gate_suite::GateMode::Strict
    }

    /// Require trusted status, aborting if not met.
    ///
    /// # Panics
    ///
    /// Panics with a detailed error message if the run is not trusted.
    pub fn require_trusted(
        config: &BacktestConfig,
        results: &BacktestResults,
        gate_suite_report: Option<&GateSuiteReport>,
        sensitivity_report: Option<&SensitivityReport>,
        run_fingerprint: Option<&RunFingerprint>,
    ) -> Result<(), TrustGateError> {
        let decision = Self::evaluate(config, results, gate_suite_report, sensitivity_report, run_fingerprint);

        match decision {
            TrustDecision::Trusted => Ok(()),
            TrustDecision::Untrusted { reasons } => Err(TrustGateError {
                decision: TrustDecision::Untrusted { reasons },
            }),
        }
    }

    // =========================================================================
    // INDIVIDUAL CHECKS
    // =========================================================================

    fn check_production_grade(config: &BacktestConfig, reasons: &mut Vec<TrustFailureReason>) {
        if !config.production_grade {
            reasons.push(TrustFailureReason::ProductionGradeDisabled);
        }
    }

    fn check_gate_suite(
        config: &BacktestConfig,
        gate_suite_report: Option<&GateSuiteReport>,
        reasons: &mut Vec<TrustFailureReason>,
    ) {
        // Check if gate suite was bypassed
        if config.gate_mode == crate::backtest_v2::gate_suite::GateMode::Disabled {
            reasons.push(TrustFailureReason::GateSuiteBypassed);
            return;
        }

        // Check if gate suite was executed
        let Some(report) = gate_suite_report else {
            reasons.push(TrustFailureReason::MissingGateSuite);
            return;
        };

        // Check if gate suite passed
        if !report.passed {
            let failed_gates: Vec<String> = report
                .gates
                .iter()
                .filter(|g| !g.passed)
                .map(|g| g.name.clone())
                .collect();

            let trust_level = match &report.trust_level {
                GateTrustLevel::Trusted => "Trusted".to_string(),
                GateTrustLevel::Untrusted { .. } => "Untrusted".to_string(),
                GateTrustLevel::Unknown => "Unknown".to_string(),
                GateTrustLevel::Bypassed => "Bypassed".to_string(),
            };

            reasons.push(TrustFailureReason::GateSuiteFailed {
                failed_gates,
                trust_level,
            });
        }
    }

    fn check_sensitivity(
        config: &BacktestConfig,
        sensitivity_report: Option<&SensitivityReport>,
        reasons: &mut Vec<TrustFailureReason>,
    ) {
        // Sensitivity must be enabled in config
        if !config.sensitivity.enabled {
            reasons.push(TrustFailureReason::MissingSensitivityAnalysis);
            return;
        }

        // Check if sensitivity was actually run
        let Some(report) = sensitivity_report else {
            reasons.push(TrustFailureReason::MissingSensitivityAnalysis);
            return;
        };

        if !report.sensitivity_run {
            reasons.push(TrustFailureReason::MissingSensitivityAnalysis);
            return;
        }

        // Check for fragility
        let fragility = &report.fragility;

        if fragility.latency_fragile {
            reasons.push(TrustFailureReason::SensitivityOutsideTolerance {
                dimension: "latency".to_string(),
                reason: fragility
                    .latency_fragility_reason
                    .clone()
                    .unwrap_or_else(|| "Latency fragile".to_string()),
            });
        }

        if fragility.sampling_fragile {
            reasons.push(TrustFailureReason::SensitivityOutsideTolerance {
                dimension: "sampling".to_string(),
                reason: fragility
                    .sampling_fragility_reason
                    .clone()
                    .unwrap_or_else(|| "Sampling fragile".to_string()),
            });
        }

        if fragility.execution_fragile {
            reasons.push(TrustFailureReason::SensitivityOutsideTolerance {
                dimension: "execution".to_string(),
                reason: fragility
                    .execution_fragility_reason
                    .clone()
                    .unwrap_or_else(|| "Execution fragile".to_string()),
            });
        }

        if fragility.requires_optimistic_assumptions {
            reasons.push(TrustFailureReason::SensitivityOutsideTolerance {
                dimension: "assumptions".to_string(),
                reason: "Strategy requires optimistic assumptions to be profitable".to_string(),
            });
        }

        // Check overall trust recommendation
        if !matches!(report.trust_recommendation, TrustRecommendation::Trusted) {
            // Only add if we haven't already captured the specific fragility
            if reasons.iter().all(|r| !matches!(r, TrustFailureReason::SensitivityOutsideTolerance { .. })) {
                reasons.push(TrustFailureReason::SensitivityOutsideTolerance {
                    dimension: "overall".to_string(),
                    reason: report.trust_recommendation.description().to_string(),
                });
            }
        }
    }

    fn check_fingerprint(
        run_fingerprint: Option<&RunFingerprint>,
        reasons: &mut Vec<TrustFailureReason>,
    ) {
        let Some(fp) = run_fingerprint else {
            reasons.push(TrustFailureReason::MissingRunFingerprint);
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
            // Zero events is suspicious but not necessarily incomplete
            // Only flag if hash is also zero
            missing.push("behavior".to_string());
        }

        if !missing.is_empty() {
            reasons.push(TrustFailureReason::IncompleteRunFingerprint {
                missing_components: missing,
            });
        }
    }

    fn check_dataset_readiness(
        config: &BacktestConfig,
        results: &BacktestResults,
        reasons: &mut Vec<TrustFailureReason>,
    ) {
        let readiness = results.dataset_readiness;

        // Check if dataset allows backtest at all
        if !readiness.allows_backtest() {
            reasons.push(TrustFailureReason::DatasetNotCompatibleWithStrategy {
                dataset_readiness: format!("{:?}", readiness),
                strategy_type: "Any".to_string(),
                reason: readiness.rejection_reason().unwrap_or("Unknown").to_string(),
            });
            return;
        }

        // Check maker compatibility
        if config.maker_fill_model == MakerFillModel::ExplicitQueue
            && results.maker_fills > 0
            && !readiness.allows_maker()
        {
            reasons.push(TrustFailureReason::DatasetNotCompatibleWithStrategy {
                dataset_readiness: format!("{:?}", readiness),
                strategy_type: "Maker".to_string(),
                reason: "Dataset does not support maker strategies (requires MakerViable)".to_string(),
            });
        }
    }

    fn check_accounting(
        config: &BacktestConfig,
        results: &BacktestResults,
        reasons: &mut Vec<TrustFailureReason>,
    ) {
        if !config.strict_accounting {
            reasons.push(TrustFailureReason::StrictAccountingDisabled);
        }

        if !results.strict_accounting_enabled {
            reasons.push(TrustFailureReason::StrictAccountingDisabled);
        }

        if let Some(violation) = &results.first_accounting_violation {
            reasons.push(TrustFailureReason::AccountingViolationDetected {
                violation: violation.clone(),
            });
        }
    }

    fn check_invariants(
        config: &BacktestConfig,
        results: &BacktestResults,
        reasons: &mut Vec<TrustFailureReason>,
    ) {
        use crate::backtest_v2::invariants::InvariantMode;

        let actual_mode = results.invariant_mode;

        if actual_mode != InvariantMode::Hard {
            reasons.push(TrustFailureReason::InvariantModeNotHard {
                actual_mode: format!("{:?}", actual_mode),
            });
        }

        if results.invariant_violations_detected > 0 {
            reasons.push(TrustFailureReason::InvariantViolationDetected {
                count: results.invariant_violations_detected,
                first_violation: results
                    .first_invariant_violation
                    .clone()
                    .unwrap_or_else(|| "Unknown".to_string()),
            });
        }
    }

    fn check_hermetic(config: &BacktestConfig, reasons: &mut Vec<TrustFailureReason>) {
        if !config.hermetic_config.enabled {
            reasons.push(TrustFailureReason::HermeticModeDisabled);
        }
    }

    fn check_settlement(
        config: &BacktestConfig,
        results: &BacktestResults,
        reasons: &mut Vec<TrustFailureReason>,
    ) {
        use crate::backtest_v2::settlement::SettlementModel;

        if !matches!(results.settlement_model, SettlementModel::ExactSpec) {
            reasons.push(TrustFailureReason::SettlementModelNotExact {
                actual_model: format!("{:?}", results.settlement_model),
            });
        }
    }

    fn check_oms_parity(
        config: &BacktestConfig,
        results: &BacktestResults,
        reasons: &mut Vec<TrustFailureReason>,
    ) {
        use crate::backtest_v2::sim_adapter::OmsParityMode;

        let actual_mode = results
            .oms_parity
            .as_ref()
            .map(|p| p.mode)
            .unwrap_or(OmsParityMode::Relaxed);

        if actual_mode != OmsParityMode::Full {
            reasons.push(TrustFailureReason::OmsParityModeNotFull {
                actual_mode: format!("{:?}", actual_mode),
            });
        }
    }

    fn check_maker_fills(results: &BacktestResults, reasons: &mut Vec<TrustFailureReason>) {
        if !results.maker_fills_valid && results.maker_fills > 0 {
            reasons.push(TrustFailureReason::MakerFillsInvalid {
                reason: if results.maker_auto_disabled {
                    "Maker fills auto-disabled due to data contract limitations".to_string()
                } else {
                    "Maker fills invalid (optimistic or data-incompatible)".to_string()
                },
            });
        }
    }
}

// =============================================================================
// TRUST GATE ERROR
// =============================================================================

/// Error returned when trust is required but not met.
#[derive(Debug, Clone)]
pub struct TrustGateError {
    pub decision: TrustDecision,
}

impl std::fmt::Display for TrustGateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.decision {
            TrustDecision::Trusted => write!(f, "Trust gate passed"),
            TrustDecision::Untrusted { reasons } => {
                writeln!(f, "Trust gate failed with {} reason(s):", reasons.len())?;
                for (i, reason) in reasons.iter().enumerate() {
                    writeln!(f, "  {}. {}", i + 1, reason)?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for TrustGateError {}

// =============================================================================
// TRUST GATE CONFIGURATION
// =============================================================================

/// Configuration for trust gate behavior.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrustGateConfig {
    /// If true, require trust for all production-grade runs.
    pub require_trusted_for_production: bool,

    /// If true, abort on first trust failure (fail-fast).
    pub fail_fast: bool,

    /// If true, include detailed diagnostics in error messages.
    pub verbose_errors: bool,
}

impl Default for TrustGateConfig {
    fn default() -> Self {
        Self {
            require_trusted_for_production: true,
            fail_fast: false, // Collect all failures by default
            verbose_errors: true,
        }
    }
}

// =============================================================================
// TESTS
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backtest_v2::gate_suite::{GateSuiteConfig, GateTestResult, GateMetrics};
    use crate::backtest_v2::sensitivity::{FragilityFlags, SensitivityConfig};
    use crate::backtest_v2::fingerprint::{
        BehaviorFingerprint, CodeFingerprint, ConfigFingerprint, DatasetFingerprint, 
        RunFingerprint, SeedFingerprint, StrategyFingerprint,
    };

    fn make_production_config() -> BacktestConfig {
        BacktestConfig::production_grade_15m_updown()
    }

    fn make_passing_gate_suite_report() -> GateSuiteReport {
        GateSuiteReport {
            passed: true,
            gates: vec![
                GateTestResult {
                    name: "Gate A: Zero-Edge Matching".to_string(),
                    passed: true,
                    failure_reason: None,
                    metrics: GateMetrics::default(),
                    failed_seeds: vec![],
                    execution_ms: 100,
                },
            ],
            trust_level: GateTrustLevel::Trusted,
            config: GateSuiteConfig::default(),
            total_execution_ms: 100,
            timestamp: 0,
        }
    }

    fn make_failing_gate_suite_report() -> GateSuiteReport {
        GateSuiteReport {
            passed: false,
            gates: vec![
                GateTestResult {
                    name: "Gate A: Zero-Edge Matching".to_string(),
                    passed: false,
                    failure_reason: Some("PnL too high".to_string()),
                    metrics: GateMetrics::default(),
                    failed_seeds: vec![42],
                    execution_ms: 100,
                },
            ],
            trust_level: GateTrustLevel::Untrusted { 
                reasons: vec![crate::backtest_v2::gate_suite::GateFailureReason::new(
                    "Gate A",
                    "PnL too high",
                    "pnl",
                    "100",
                    "< 0",
                )] 
            },
            config: GateSuiteConfig::default(),
            total_execution_ms: 100,
            timestamp: 0,
        }
    }

    fn make_passing_sensitivity_report() -> SensitivityReport {
        SensitivityReport {
            sensitivity_run: true,
            latency_sweep: None,
            sampling_sweep: None,
            execution_sweep: None,
            fragility: FragilityFlags::default(), // All false
            trust_recommendation: TrustRecommendation::Trusted,
        }
    }

    fn make_failing_sensitivity_report() -> SensitivityReport {
        SensitivityReport {
            sensitivity_run: true,
            latency_sweep: None,
            sampling_sweep: None,
            execution_sweep: None,
            fragility: FragilityFlags {
                latency_fragile: true,
                latency_fragility_reason: Some("PnL drops 80% at 50ms".to_string()),
                ..Default::default()
            },
            trust_recommendation: TrustRecommendation::CautionFragile,
        }
    }

    fn make_valid_fingerprint() -> RunFingerprint {
        RunFingerprint {
            version: "RUNFP_V2".to_string(),
            strategy: StrategyFingerprint {
                name: "test_strategy".to_string(),
                version: "1.0.0".to_string(),
                code_hash: "abc123".to_string(),
                hash: 11111,
            },
            code: CodeFingerprint {
                crate_version: "1.0.0".to_string(),
                git_commit: Some("abc123".to_string()),
                build_profile: "release".to_string(),
                hash: 12345,
            },
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
        }
    }

    fn make_production_results() -> BacktestResults {
        use crate::backtest_v2::settlement::SettlementModel;
        use crate::backtest_v2::sim_adapter::OmsParityMode;
        use crate::backtest_v2::invariants::InvariantMode;

        let mut results = BacktestResults::default();
        results.production_grade = true;
        results.strict_accounting_enabled = true;
        results.first_accounting_violation = None;
        results.invariant_mode = InvariantMode::Hard;
        results.invariant_violations_detected = 0;
        results.settlement_model = SettlementModel::ExactSpec;
        results.oms_parity = Some(crate::backtest_v2::sim_adapter::OmsParityStats {
            mode: OmsParityMode::Full,
            valid_for_production: true,
            ..Default::default()
        });
        results.dataset_readiness = DatasetReadiness::MakerViable;
        results.maker_fills_valid = true;
        results.maker_fills = 0;
        results
    }

    #[test]
    fn test_all_requirements_satisfied_is_trusted() {
        let config = make_production_config();
        let results = make_production_results();
        let gate_report = make_passing_gate_suite_report();
        let sensitivity_report = make_passing_sensitivity_report();
        let fingerprint = make_valid_fingerprint();

        let decision = TrustGate::evaluate(
            &config,
            &results,
            Some(&gate_report),
            Some(&sensitivity_report),
            Some(&fingerprint),
        );

        assert!(decision.is_trusted(), "Expected Trusted, got {:?}", decision);
    }

    #[test]
    fn test_missing_gate_suite_is_untrusted() {
        let config = make_production_config();
        let results = make_production_results();
        let sensitivity_report = make_passing_sensitivity_report();
        let fingerprint = make_valid_fingerprint();

        let decision = TrustGate::evaluate(
            &config,
            &results,
            None, // Missing gate suite
            Some(&sensitivity_report),
            Some(&fingerprint),
        );

        assert!(!decision.is_trusted());
        assert!(decision.failure_reasons().iter().any(|r| 
            matches!(r, TrustFailureReason::MissingGateSuite)
        ));
    }

    #[test]
    fn test_gate_suite_failed_is_untrusted() {
        let config = make_production_config();
        let results = make_production_results();
        let gate_report = make_failing_gate_suite_report();
        let sensitivity_report = make_passing_sensitivity_report();
        let fingerprint = make_valid_fingerprint();

        let decision = TrustGate::evaluate(
            &config,
            &results,
            Some(&gate_report),
            Some(&sensitivity_report),
            Some(&fingerprint),
        );

        assert!(!decision.is_trusted());
        assert!(decision.failure_reasons().iter().any(|r| 
            matches!(r, TrustFailureReason::GateSuiteFailed { .. })
        ));
    }

    #[test]
    fn test_missing_sensitivity_is_untrusted() {
        let config = make_production_config();
        let results = make_production_results();
        let gate_report = make_passing_gate_suite_report();
        let fingerprint = make_valid_fingerprint();

        let decision = TrustGate::evaluate(
            &config,
            &results,
            Some(&gate_report),
            None, // Missing sensitivity
            Some(&fingerprint),
        );

        assert!(!decision.is_trusted());
        assert!(decision.failure_reasons().iter().any(|r| 
            matches!(r, TrustFailureReason::MissingSensitivityAnalysis)
        ));
    }

    #[test]
    fn test_sensitivity_fragile_is_untrusted() {
        let config = make_production_config();
        let results = make_production_results();
        let gate_report = make_passing_gate_suite_report();
        let sensitivity_report = make_failing_sensitivity_report();
        let fingerprint = make_valid_fingerprint();

        let decision = TrustGate::evaluate(
            &config,
            &results,
            Some(&gate_report),
            Some(&sensitivity_report),
            Some(&fingerprint),
        );

        assert!(!decision.is_trusted());
        assert!(decision.failure_reasons().iter().any(|r| 
            matches!(r, TrustFailureReason::SensitivityOutsideTolerance { .. })
        ));
    }

    #[test]
    fn test_missing_fingerprint_is_untrusted() {
        let config = make_production_config();
        let results = make_production_results();
        let gate_report = make_passing_gate_suite_report();
        let sensitivity_report = make_passing_sensitivity_report();

        let decision = TrustGate::evaluate(
            &config,
            &results,
            Some(&gate_report),
            Some(&sensitivity_report),
            None, // Missing fingerprint
        );

        assert!(!decision.is_trusted());
        assert!(decision.failure_reasons().iter().any(|r| 
            matches!(r, TrustFailureReason::MissingRunFingerprint)
        ));
    }

    #[test]
    fn test_production_grade_disabled_is_untrusted() {
        let mut config = make_production_config();
        config.production_grade = false;
        config.allow_non_production = true; // Allow non-production
        
        let results = make_production_results();
        let gate_report = make_passing_gate_suite_report();
        let sensitivity_report = make_passing_sensitivity_report();
        let fingerprint = make_valid_fingerprint();

        let decision = TrustGate::evaluate(
            &config,
            &results,
            Some(&gate_report),
            Some(&sensitivity_report),
            Some(&fingerprint),
        );

        assert!(!decision.is_trusted());
        assert!(decision.failure_reasons().iter().any(|r| 
            matches!(r, TrustFailureReason::ProductionGradeDisabled)
        ));
    }

    #[test]
    fn test_multiple_failures_all_reported() {
        let mut config = make_production_config();
        config.production_grade = false;
        config.allow_non_production = true;
        config.strict_accounting = false;

        let mut results = make_production_results();
        results.production_grade = false;
        results.strict_accounting_enabled = false;

        let decision = TrustGate::evaluate(
            &config,
            &results,
            None, // Missing gate suite
            None, // Missing sensitivity
            None, // Missing fingerprint
        );

        assert!(!decision.is_trusted());
        // Should have multiple failure reasons
        assert!(decision.failure_count() >= 3);
    }

    #[test]
    fn test_trust_failure_reason_codes_unique() {
        // Verify all failure reason codes are unique
        let reasons = vec![
            TrustFailureReason::MissingGateSuite,
            TrustFailureReason::GateSuiteFailed { failed_gates: vec![], trust_level: "".to_string() },
            TrustFailureReason::GateSuiteBypassed,
            TrustFailureReason::MissingSensitivityAnalysis,
            TrustFailureReason::SensitivityOutsideTolerance { dimension: "".to_string(), reason: "".to_string() },
            TrustFailureReason::MissingRunFingerprint,
            TrustFailureReason::IncompleteRunFingerprint { missing_components: vec![] },
            TrustFailureReason::FingerprintNotReproducible { reason: "".to_string() },
            TrustFailureReason::ProductionGradeDisabled,
            TrustFailureReason::DatasetNotCompatibleWithStrategy { dataset_readiness: "".to_string(), strategy_type: "".to_string(), reason: "".to_string() },
            TrustFailureReason::StrictAccountingDisabled,
            TrustFailureReason::AccountingViolationDetected { violation: "".to_string() },
            TrustFailureReason::InvariantModeNotHard { actual_mode: "".to_string() },
            TrustFailureReason::InvariantViolationDetected { count: 0, first_violation: "".to_string() },
            TrustFailureReason::HermeticModeDisabled,
            TrustFailureReason::SettlementModelNotExact { actual_model: "".to_string() },
            TrustFailureReason::OmsParityModeNotFull { actual_mode: "".to_string() },
            TrustFailureReason::MakerFillsInvalid { reason: "".to_string() },
        ];

        let mut codes: Vec<&str> = reasons.iter().map(|r| r.code()).collect();
        codes.sort();
        codes.dedup();
        assert_eq!(codes.len(), reasons.len(), "Duplicate failure reason codes found");
    }

    #[test]
    fn test_trust_decision_format() {
        let decision = TrustDecision::Untrusted {
            reasons: vec![
                TrustFailureReason::MissingGateSuite,
                TrustFailureReason::ProductionGradeDisabled,
            ],
        };

        let report = decision.format_report();
        assert!(report.contains("UNTRUSTED"));
        assert!(report.contains("MISSING_GATE_SUITE"));
        assert!(report.contains("PRODUCTION_GRADE_DISABLED"));
    }

    #[test]
    fn test_require_trusted_error() {
        let config = make_production_config();
        let results = make_production_results();

        let result = TrustGate::require_trusted(
            &config,
            &results,
            None, // Missing gate suite
            None,
            None,
        );

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("Trust gate failed"));
    }

    #[test]
    fn test_quick_check() {
        let config = make_production_config();
        assert!(TrustGate::quick_check(&config));

        let mut non_prod = config.clone();
        non_prod.production_grade = false;
        assert!(!TrustGate::quick_check(&non_prod));
    }
}

//! Backtest Disclaimers
//!
//! Structured, programmatically generated disclaimers for backtest artifacts.
//! Every published backtest result carries the correct caveats automatically,
//! derived from config + dataset readiness + trust/gate outcomes.
//!
//! # Design Principles
//!
//! - Disclaimers are structured data, not ad-hoc strings
//! - Generated deterministically from run conditions
//! - Included in JSON manifests for auditability
//! - Stable ordering for reproducibility

use crate::backtest_v2::clock::Nanos;
use crate::backtest_v2::data_contract::DatasetReadiness;
use crate::backtest_v2::fingerprint::RunFingerprint;
use crate::backtest_v2::gate_suite::{GateSuiteReport, TrustLevel as GateTrustLevel};
use crate::backtest_v2::invariants::InvariantMode;
use crate::backtest_v2::orchestrator::{BacktestConfig, BacktestResults, MakerFillModel};
use crate::backtest_v2::sensitivity::SensitivityReport;
use crate::backtest_v2::settlement::SettlementModel;
use crate::backtest_v2::trust_gate::TrustDecision;
use serde::{Deserialize, Serialize};

// =============================================================================
// SEVERITY
// =============================================================================

/// Severity level for a disclaimer.
/// Ordered from least to most severe for sorting purposes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Severity {
    /// Informational - context that may be useful but doesn't affect trust.
    Info = 0,
    /// Warning - condition that may affect reliability but doesn't invalidate results.
    Warning = 1,
    /// Critical - condition that invalidates results or prevents production use.
    Critical = 2,
}

impl Severity {
    /// Get a human-readable label.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Info => "INFO",
            Self::Warning => "WARNING",
            Self::Critical => "CRITICAL",
        }
    }
}

impl std::fmt::Display for Severity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.label())
    }
}

// =============================================================================
// CATEGORY
// =============================================================================

/// Category of disclaimer - what aspect of the run it pertains to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Category {
    /// Production mode status.
    ProductionMode,
    /// Dataset readiness classification.
    DatasetReadiness,
    /// Maker fill validity.
    MakerValidity,
    /// Gate suite execution and results.
    GateSuite,
    /// Sensitivity analysis and fragility.
    Sensitivity,
    /// Settlement reference configuration.
    SettlementReference,
    /// Integrity policy and data pathologies.
    IntegrityPolicy,
    /// Reproducibility and fingerprint status.
    Reproducibility,
    /// Data coverage and quality.
    DataCoverage,
}

impl Category {
    /// Get a human-readable label.
    pub fn label(&self) -> &'static str {
        match self {
            Self::ProductionMode => "PRODUCTION_MODE",
            Self::DatasetReadiness => "DATASET_READINESS",
            Self::MakerValidity => "MAKER_VALIDITY",
            Self::GateSuite => "GATE_SUITE",
            Self::Sensitivity => "SENSITIVITY",
            Self::SettlementReference => "SETTLEMENT_REFERENCE",
            Self::IntegrityPolicy => "INTEGRITY_POLICY",
            Self::Reproducibility => "REPRODUCIBILITY",
            Self::DataCoverage => "DATA_COVERAGE",
        }
    }
}

impl std::fmt::Display for Category {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.label())
    }
}

// =============================================================================
// DISCLAIMER
// =============================================================================

/// A single structured disclaimer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Disclaimer {
    /// Stable identifier (e.g., "NON_PRODUCTION_RUN").
    pub id: String,
    /// Severity level.
    pub severity: Severity,
    /// Category of this disclaimer.
    pub category: Category,
    /// Human-readable standardized message.
    pub message: String,
    /// Compact evidence strings pointing to conditions.
    /// Example: ["production_grade=false", "allow_non_production=true"]
    pub evidence: Vec<String>,
}

impl Disclaimer {
    /// Create a new disclaimer.
    pub fn new(
        id: impl Into<String>,
        severity: Severity,
        category: Category,
        message: impl Into<String>,
        evidence: Vec<String>,
    ) -> Self {
        Self {
            id: id.into(),
            severity,
            category,
            message: message.into(),
            evidence,
        }
    }

    /// Create an Info disclaimer.
    pub fn info(
        id: impl Into<String>,
        category: Category,
        message: impl Into<String>,
        evidence: Vec<String>,
    ) -> Self {
        Self::new(id, Severity::Info, category, message, evidence)
    }

    /// Create a Warning disclaimer.
    pub fn warning(
        id: impl Into<String>,
        category: Category,
        message: impl Into<String>,
        evidence: Vec<String>,
    ) -> Self {
        Self::new(id, Severity::Warning, category, message, evidence)
    }

    /// Create a Critical disclaimer.
    pub fn critical(
        id: impl Into<String>,
        category: Category,
        message: impl Into<String>,
        evidence: Vec<String>,
    ) -> Self {
        Self::new(id, Severity::Critical, category, message, evidence)
    }

    /// Format as a single-line string.
    pub fn format_line(&self) -> String {
        format!(
            "[{}] [{}] {}: {}",
            self.severity.label(),
            self.category.label(),
            self.id,
            self.message
        )
    }
}

impl std::fmt::Display for Disclaimer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.format_line())
    }
}

// =============================================================================
// DISCLAIMERS BLOCK
// =============================================================================

/// Container for all disclaimers generated for a backtest run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DisclaimersBlock {
    /// Timestamp when disclaimers were generated (nanoseconds).
    pub generated_at_ns: Nanos,
    /// Trust level snapshot at generation time.
    pub trust_level: TrustLevelSnapshot,
    /// All disclaimers, sorted by (severity desc, id asc).
    pub disclaimers: Vec<Disclaimer>,
}

impl Default for DisclaimersBlock {
    fn default() -> Self {
        Self {
            generated_at_ns: 0,
            trust_level: TrustLevelSnapshot::Unknown,
            disclaimers: Vec::new(),
        }
    }
}

impl DisclaimersBlock {
    /// Create an empty block.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Count of critical disclaimers.
    pub fn critical_count(&self) -> usize {
        self.disclaimers
            .iter()
            .filter(|d| d.severity == Severity::Critical)
            .count()
    }

    /// Count of warning disclaimers.
    pub fn warning_count(&self) -> usize {
        self.disclaimers
            .iter()
            .filter(|d| d.severity == Severity::Warning)
            .count()
    }

    /// Count of info disclaimers.
    pub fn info_count(&self) -> usize {
        self.disclaimers
            .iter()
            .filter(|d| d.severity == Severity::Info)
            .count()
    }

    /// Get the first critical disclaimer message (for UI banners).
    pub fn first_critical_message(&self) -> Option<&str> {
        self.disclaimers
            .iter()
            .find(|d| d.severity == Severity::Critical)
            .map(|d| d.message.as_str())
    }

    /// Check if there are any critical disclaimers.
    pub fn has_critical(&self) -> bool {
        self.critical_count() > 0
    }

    /// Get all disclaimers of a specific category.
    pub fn by_category(&self, category: Category) -> Vec<&Disclaimer> {
        self.disclaimers
            .iter()
            .filter(|d| d.category == category)
            .collect()
    }

    /// Get a compact summary string.
    pub fn summary(&self) -> String {
        format!(
            "trust={} critical={} warning={} info={}",
            self.trust_level,
            self.critical_count(),
            self.warning_count(),
            self.info_count()
        )
    }

    /// Format as a multi-line report.
    pub fn format_report(&self) -> String {
        let mut out = String::new();

        out.push_str("╔══════════════════════════════════════════════════════════════════════════════╗\n");
        out.push_str("║                          BACKTEST DISCLAIMERS                                ║\n");
        out.push_str("╠══════════════════════════════════════════════════════════════════════════════╣\n");
        out.push_str(&format!(
            "║  Trust Level: {:62} ║\n",
            self.trust_level.to_string()
        ));
        out.push_str(&format!(
            "║  Critical: {}  Warning: {}  Info: {:42} ║\n",
            self.critical_count(),
            self.warning_count(),
            self.info_count()
        ));
        out.push_str("╠══════════════════════════════════════════════════════════════════════════════╣\n");

        if self.disclaimers.is_empty() {
            out.push_str("║  No disclaimers - all conditions nominal.                                    ║\n");
        } else {
            for (i, d) in self.disclaimers.iter().enumerate() {
                let severity_icon = match d.severity {
                    Severity::Critical => "✗",
                    Severity::Warning => "⚠",
                    Severity::Info => "ℹ",
                };

                out.push_str(&format!(
                    "║  {}. {} [{:8}] {:54} ║\n",
                    i + 1,
                    severity_icon,
                    d.severity.label(),
                    d.id
                ));

                // Truncate message to fit
                let msg = if d.message.len() > 68 {
                    format!("{}...", &d.message[..65])
                } else {
                    d.message.clone()
                };
                out.push_str(&format!("║     {:72} ║\n", msg));

                // Show evidence (limited)
                for ev in d.evidence.iter().take(3) {
                    let ev_display = if ev.len() > 66 {
                        format!("{}...", &ev[..63])
                    } else {
                        ev.clone()
                    };
                    out.push_str(&format!("║       • {:68} ║\n", ev_display));
                }
                if d.evidence.len() > 3 {
                    out.push_str(&format!(
                        "║       ... and {} more evidence items                                        ║\n",
                        d.evidence.len() - 3
                    ));
                }
            }
        }

        out.push_str("╚══════════════════════════════════════════════════════════════════════════════╝\n");
        out
    }
}

/// Snapshot of trust level for the disclaimers block.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrustLevelSnapshot {
    /// Trusted - all requirements satisfied.
    Trusted,
    /// Untrusted - one or more requirements failed.
    Untrusted,
    /// Unknown - trust evaluation not performed.
    Unknown,
    /// Bypassed - trust evaluation was explicitly bypassed.
    Bypassed,
}

impl Default for TrustLevelSnapshot {
    fn default() -> Self {
        Self::Unknown
    }
}

impl std::fmt::Display for TrustLevelSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Trusted => write!(f, "TRUSTED"),
            Self::Untrusted => write!(f, "UNTRUSTED"),
            Self::Unknown => write!(f, "UNKNOWN"),
            Self::Bypassed => write!(f, "BYPASSED"),
        }
    }
}

impl From<&TrustDecision> for TrustLevelSnapshot {
    fn from(decision: &TrustDecision) -> Self {
        match decision {
            TrustDecision::Trusted => Self::Trusted,
            TrustDecision::Untrusted { .. } => Self::Untrusted,
        }
    }
}

impl From<&GateTrustLevel> for TrustLevelSnapshot {
    fn from(level: &GateTrustLevel) -> Self {
        match level {
            GateTrustLevel::Trusted => Self::Trusted,
            GateTrustLevel::Untrusted { .. } => Self::Untrusted,
            GateTrustLevel::Unknown => Self::Unknown,
            GateTrustLevel::Bypassed => Self::Bypassed,
        }
    }
}

// =============================================================================
// DISCLAIMER GENERATION - INPUT CONTEXT
// =============================================================================

/// Input context for generating disclaimers.
/// Aggregates all information needed to produce disclaimers deterministically.
pub struct DisclaimerContext<'a> {
    /// Backtest configuration.
    pub config: &'a BacktestConfig,
    /// Backtest results.
    pub results: &'a BacktestResults,
    /// Gate suite report (if executed).
    pub gate_suite_report: Option<&'a GateSuiteReport>,
    /// Sensitivity report (if executed).
    pub sensitivity_report: Option<&'a SensitivityReport>,
    /// Run fingerprint (if computed).
    pub run_fingerprint: Option<&'a RunFingerprint>,
    /// Trust decision (if evaluated).
    pub trust_decision: Option<&'a TrustDecision>,
    /// Current simulation time (for timestamp).
    pub current_time_ns: Nanos,
}

// =============================================================================
// DISCLAIMER GENERATION
// =============================================================================

/// Generate disclaimers from run context.
///
/// This is the SINGLE function that produces all disclaimers for a backtest run.
/// It should be called exactly once at finalization time.
///
/// # Determinism
///
/// Disclaimers are generated deterministically:
/// - Same inputs produce identical outputs
/// - Ordering is stable: (severity desc, id asc)
/// - Evidence vectors are in deterministic order
pub fn generate_disclaimers(ctx: &DisclaimerContext) -> DisclaimersBlock {
    let mut disclaimers = Vec::new();

    // === PART C: Required disclaimer rules ===

    // 1. Production-grade downgrade
    generate_production_mode_disclaimers(ctx, &mut disclaimers);

    // 2. Dataset readiness and maker validity
    generate_dataset_disclaimers(ctx, &mut disclaimers);
    generate_maker_validity_disclaimers(ctx, &mut disclaimers);

    // 3. Gate suite and trust
    generate_gate_suite_disclaimers(ctx, &mut disclaimers);

    // 4. Settlement reference
    generate_settlement_disclaimers(ctx, &mut disclaimers);

    // 5. Integrity policy / data issues
    generate_integrity_disclaimers(ctx, &mut disclaimers);

    // 6. Sensitivity / fragility
    generate_sensitivity_disclaimers(ctx, &mut disclaimers);

    // 7. Reproducibility
    generate_reproducibility_disclaimers(ctx, &mut disclaimers);

    // === Sort by (severity desc, id asc) ===
    // Severity::Critical > Warning > Info, so we reverse the severity comparison
    disclaimers.sort_by(|a, b| {
        match b.severity.cmp(&a.severity) {
            std::cmp::Ordering::Equal => a.id.cmp(&b.id),
            other => other,
        }
    });

    // Determine trust level snapshot
    let trust_level = if let Some(td) = ctx.trust_decision {
        TrustLevelSnapshot::from(td)
    } else {
        TrustLevelSnapshot::from(&ctx.results.trust_level)
    };

    DisclaimersBlock {
        generated_at_ns: ctx.current_time_ns,
        trust_level,
        disclaimers,
    }
}

// =============================================================================
// RULE 1: PRODUCTION MODE DISCLAIMERS
// =============================================================================

fn generate_production_mode_disclaimers(ctx: &DisclaimerContext, disclaimers: &mut Vec<Disclaimer>) {
    // Non-production run
    if !ctx.config.production_grade {
        disclaimers.push(Disclaimer::critical(
            "NON_PRODUCTION_RUN",
            Category::ProductionMode,
            "Non-production run: do not trust results for deployment decisions.",
            vec!["production_grade=false".to_string()],
        ));
    }

    // Downgraded mode with allow_non_production
    if ctx.config.allow_non_production && !ctx.results.downgraded_subsystems.is_empty() {
        let evidence: Vec<String> = ctx
            .results
            .downgraded_subsystems
            .iter()
            .map(|s| format!("downgrade: {}", s))
            .collect();

        disclaimers.push(Disclaimer::critical(
            "DOWNGRADED_MODE",
            Category::ProductionMode,
            "Run executed with downgraded constraints; results are untrusted by construction.",
            evidence,
        ));
    }

    // Strict accounting disabled
    if !ctx.config.strict_accounting && ctx.config.production_grade {
        disclaimers.push(Disclaimer::critical(
            "STRICT_ACCOUNTING_DISABLED",
            Category::ProductionMode,
            "Strict accounting is disabled; economic state may be inconsistent.",
            vec!["strict_accounting=false".to_string()],
        ));
    }

    // Hermetic mode disabled in production
    if !ctx.config.hermetic_config.enabled && ctx.config.production_grade {
        disclaimers.push(Disclaimer::critical(
            "HERMETIC_MODE_DISABLED",
            Category::ProductionMode,
            "Hermetic strategy sandboxing disabled; strategy may have external dependencies.",
            vec!["hermetic_config.enabled=false".to_string()],
        ));
    }
}

// =============================================================================
// RULE 2: DATASET READINESS DISCLAIMERS
// =============================================================================

fn generate_dataset_disclaimers(ctx: &DisclaimerContext, disclaimers: &mut Vec<Disclaimer>) {
    let readiness = ctx.results.dataset_readiness;

    match readiness {
        DatasetReadiness::TakerOnly => {
            // TakerOnly is a warning normally, but critical if maker was requested
            let is_maker_requested =
                ctx.config.maker_fill_model == MakerFillModel::ExplicitQueue;

            if is_maker_requested {
                disclaimers.push(Disclaimer::critical(
                    "TAKER_ONLY_DATASET_WITH_MAKER",
                    Category::DatasetReadiness,
                    "Dataset is TakerOnly but maker fills were requested; maker results are invalid.",
                    vec![
                        format!("dataset_readiness={:?}", readiness),
                        format!("maker_fill_model={:?}", ctx.config.maker_fill_model),
                    ],
                ));
            } else {
                disclaimers.push(Disclaimer::warning(
                    "TAKER_ONLY_DATASET",
                    Category::DatasetReadiness,
                    "Dataset is TakerOnly: passive fill viability cannot be certified.",
                    vec![format!("dataset_readiness={:?}", readiness)],
                ));
            }
        }
        DatasetReadiness::NonRepresentative => {
            disclaimers.push(Disclaimer::critical(
                "NON_REPRESENTATIVE_DATASET",
                Category::DatasetReadiness,
                "Dataset is NonRepresentative: results are not reliable for any claims.",
                vec![format!("dataset_readiness={:?}", readiness)],
            ));
        }
        DatasetReadiness::MakerViable => {
            // No disclaimer needed for fully viable dataset
        }
    }
}

// =============================================================================
// RULE 2b: MAKER VALIDITY DISCLAIMERS
// =============================================================================

fn generate_maker_validity_disclaimers(ctx: &DisclaimerContext, disclaimers: &mut Vec<Disclaimer>) {
    // Maker requested on non-maker-viable dataset
    if ctx.config.maker_fill_model == MakerFillModel::ExplicitQueue
        && ctx.results.dataset_readiness != DatasetReadiness::MakerViable
        && ctx.results.maker_fills > 0
    {
        disclaimers.push(Disclaimer::critical(
            "MAKER_REQUEST_ON_NON_MAKER_DATASET",
            Category::MakerValidity,
            "Maker strategy requested on non-MakerViable dataset: maker results must not be used.",
            vec![
                format!("maker_fill_model={:?}", ctx.config.maker_fill_model),
                format!("dataset_readiness={:?}", ctx.results.dataset_readiness),
                format!("maker_fills={}", ctx.results.maker_fills),
            ],
        ));
    }

    // Maker fills not valid
    if !ctx.results.maker_fills_valid && ctx.results.maker_fills > 0 {
        disclaimers.push(Disclaimer::critical(
            "MAKER_FILLS_INVALID",
            Category::MakerValidity,
            "Maker fills are marked invalid; passive execution claims are not reliable.",
            vec![
                format!("maker_fills_valid={}", ctx.results.maker_fills_valid),
                format!("maker_fills={}", ctx.results.maker_fills),
                format!("maker_auto_disabled={}", ctx.results.maker_auto_disabled),
            ],
        ));
    }

    // Optimistic maker model
    if ctx.config.maker_fill_model == MakerFillModel::Optimistic {
        disclaimers.push(Disclaimer::critical(
            "OPTIMISTIC_MAKER_MODEL",
            Category::MakerValidity,
            "Optimistic maker fill model used: fills are assumed without queue validation.",
            vec!["maker_fill_model=Optimistic".to_string()],
        ));
    }
}

// =============================================================================
// RULE 3: GATE SUITE AND TRUST DISCLAIMERS
// =============================================================================

fn generate_gate_suite_disclaimers(ctx: &DisclaimerContext, disclaimers: &mut Vec<Disclaimer>) {
    // Gates not run
    if ctx.gate_suite_report.is_none() {
        if ctx.config.gate_mode == crate::backtest_v2::gate_suite::GateMode::Disabled {
            disclaimers.push(Disclaimer::critical(
                "GATES_BYPASSED",
                Category::GateSuite,
                "Gate suite was explicitly bypassed: results are untrusted by construction.",
                vec![format!("gate_mode={:?}", ctx.config.gate_mode)],
            ));
        } else {
            disclaimers.push(Disclaimer::critical(
                "GATES_NOT_RUN",
                Category::GateSuite,
                "Gate suite not executed: results are untrusted by construction.",
                vec![
                    format!("gate_mode={:?}", ctx.config.gate_mode),
                    "gate_suite=missing".to_string(),
                ],
            ));
        }
    }

    // Gates run but failed
    if let Some(report) = ctx.gate_suite_report {
        if !report.passed {
            let failed_gates: Vec<String> = report
                .gates
                .iter()
                .filter(|g| !g.passed)
                .map(|g| g.name.clone())
                .collect();

            disclaimers.push(Disclaimer::critical(
                "GATES_FAILED",
                Category::GateSuite,
                "Gate suite tests failed: strategy may have look-ahead bias or edge inflation.",
                failed_gates
                    .iter()
                    .map(|g| format!("failed_gate={}", g))
                    .collect(),
            ));
        }
    }

    // Trust level not Trusted
    let trust_level = if let Some(td) = ctx.trust_decision {
        TrustLevelSnapshot::from(td)
    } else {
        TrustLevelSnapshot::from(&ctx.results.trust_level)
    };

    if trust_level != TrustLevelSnapshot::Trusted {
        let evidence = match ctx.trust_decision {
            Some(TrustDecision::Untrusted { reasons }) => {
                reasons.iter().map(|r| format!("reason={}", r.code())).collect()
            }
            _ => vec![format!("trust_level={:?}", trust_level)],
        };

        disclaimers.push(Disclaimer::critical(
            "UNTRUSTED_RESULTS",
            Category::GateSuite,
            "Results are not Trusted: do not use for profitability claims or capital allocation.",
            evidence,
        ));
    }
}

// =============================================================================
// RULE 4: SETTLEMENT REFERENCE DISCLAIMERS
// =============================================================================

fn generate_settlement_disclaimers(ctx: &DisclaimerContext, disclaimers: &mut Vec<Disclaimer>) {
    // Settlement model not ExactSpec
    if !matches!(ctx.results.settlement_model, SettlementModel::ExactSpec) {
        disclaimers.push(Disclaimer::critical(
            "NON_PRODUCTION_SETTLEMENT_RULE",
            Category::SettlementReference,
            "Settlement reference rule differs from production: results are not certifiable.",
            vec![format!("settlement_model={:?}", ctx.results.settlement_model)],
        ));
    }

    // Oracle config missing in production mode
    if ctx.config.production_grade && ctx.config.oracle_config.is_none() {
        disclaimers.push(Disclaimer::critical(
            "MISSING_ORACLE_CONFIG",
            Category::SettlementReference,
            "Oracle configuration is missing: settlement prices are not canonical.",
            vec!["oracle_config=None".to_string()],
        ));
    }

    // Oracle coverage issues
    if let Some(coverage) = &ctx.results.oracle_coverage {
        if coverage.settlements_missing_oracle > 0 {
            disclaimers.push(Disclaimer::critical(
                "SETTLEMENT_REFERENCE_NOT_CANONICAL",
                Category::SettlementReference,
                "Settlement reference is not canonical (Chainlink missing/incomplete): results are non-representative.",
                vec![
                    format!(
                        "settlements_missing_oracle={}",
                        coverage.settlements_missing_oracle
                    ),
                    format!("settlements_attempted={}", coverage.settlements_attempted),
                ],
            ));
        }

        if coverage.settlements_stale_oracle > 0 {
            disclaimers.push(Disclaimer::warning(
                "SETTLEMENT_STALE_ORACLE",
                Category::SettlementReference,
                "Some settlements used stale oracle data: results may not match production.",
                vec![
                    format!(
                        "settlements_stale_oracle={}",
                        coverage.settlements_stale_oracle
                    ),
                    format!("settlements_attempted={}", coverage.settlements_attempted),
                ],
            ));
        }
    }
}

// =============================================================================
// RULE 5: INTEGRITY POLICY DISCLAIMERS
// =============================================================================

fn generate_integrity_disclaimers(ctx: &DisclaimerContext, disclaimers: &mut Vec<Disclaimer>) {
    // Non-strict integrity policy in production mode
    if ctx.config.production_grade
        && ctx.config.integrity_policy
            != crate::backtest_v2::integrity::PathologyPolicy::strict()
    {
        disclaimers.push(Disclaimer::critical(
            "NON_STRICT_INTEGRITY_POLICY",
            Category::IntegrityPolicy,
            "Integrity policy is not strict: gaps/duplicates/out-of-order may be silently handled; results are not publishable.",
            vec![format!("integrity_policy={:?}", ctx.config.integrity_policy)],
        ));
    }

    // Data pathologies detected
    let counters = &ctx.results.pathology_counters;
    if counters.has_pathologies() {
        let mut evidence = Vec::new();

        if counters.gaps_detected > 0 {
            evidence.push(format!("gaps={}", counters.gaps_detected));
            evidence.push(format!(
                "missing_sequences={}",
                counters.total_missing_sequences
            ));
        }
        if counters.duplicates_dropped > 0 {
            evidence.push(format!("duplicates={}", counters.duplicates_dropped));
        }
        if counters.out_of_order_detected > 0 {
            evidence.push(format!("out_of_order={}", counters.out_of_order_detected));
        }
        if counters.halted {
            evidence.push(format!(
                "halt_reason={}",
                counters.halt_reason.as_deref().unwrap_or("unknown")
            ));
        }

        let severity = if counters.halted || counters.gaps_detected > 0 {
            Severity::Critical
        } else {
            Severity::Warning
        };

        disclaimers.push(Disclaimer::new(
            "DATA_PATHOLOGIES_DETECTED",
            severity,
            Category::DataCoverage,
            "Data pathologies detected during replay; results must be treated as non-representative unless explicitly justified.",
            evidence,
        ));
    }

    // Invariant mode not Hard
    if ctx.results.invariant_mode != InvariantMode::Hard && ctx.config.production_grade {
        disclaimers.push(Disclaimer::critical(
            "INVARIANT_MODE_NOT_HARD",
            Category::IntegrityPolicy,
            "Invariant mode is not Hard: violations may have been tolerated.",
            vec![format!("invariant_mode={:?}", ctx.results.invariant_mode)],
        ));
    }

    // Invariant violations detected
    if ctx.results.invariant_violations_detected > 0 {
        disclaimers.push(Disclaimer::critical(
            "INVARIANT_VIOLATIONS_DETECTED",
            Category::IntegrityPolicy,
            "Invariant violations were detected during the run.",
            vec![
                format!(
                    "violations_count={}",
                    ctx.results.invariant_violations_detected
                ),
                format!(
                    "first_violation={}",
                    ctx.results
                        .first_invariant_violation
                        .as_deref()
                        .unwrap_or("unknown")
                ),
            ],
        ));
    }

    // Accounting violations
    if let Some(violation) = &ctx.results.first_accounting_violation {
        disclaimers.push(Disclaimer::critical(
            "ACCOUNTING_VIOLATION_DETECTED",
            Category::IntegrityPolicy,
            "Accounting violation detected: economic state is inconsistent.",
            vec![format!("violation={}", violation)],
        ));
    }
}

// =============================================================================
// RULE 6: SENSITIVITY DISCLAIMERS
// =============================================================================

fn generate_sensitivity_disclaimers(ctx: &DisclaimerContext, disclaimers: &mut Vec<Disclaimer>) {
    // Sensitivity not run
    if ctx.sensitivity_report.is_none() && ctx.config.production_grade {
        disclaimers.push(Disclaimer::critical(
            "SENSITIVITY_NOT_RUN",
            Category::Sensitivity,
            "Sensitivity analysis was not executed: fragility status is unknown.",
            vec!["sensitivity_report=None".to_string()],
        ));
        return;
    }

    if let Some(report) = ctx.sensitivity_report {
        if !report.sensitivity_run && ctx.config.production_grade {
            disclaimers.push(Disclaimer::critical(
                "SENSITIVITY_NOT_RUN",
                Category::Sensitivity,
                "Sensitivity analysis was not executed: fragility status is unknown.",
                vec!["sensitivity_run=false".to_string()],
            ));
            return;
        }

        let fragility = &report.fragility;

        // Latency fragile
        if fragility.latency_fragile {
            let reason = fragility
                .latency_fragility_reason
                .as_deref()
                .unwrap_or("PnL degrades significantly with modest latency increase");

            let severity = if ctx.config.production_grade {
                Severity::Critical
            } else {
                Severity::Warning
            };

            disclaimers.push(Disclaimer::new(
                "FRAGILE_TO_LATENCY",
                severity,
                Category::Sensitivity,
                "Performance is fragile to modest latency/execution assumptions; do not extrapolate to live PnL distribution.",
                vec![
                    "latency_fragile=true".to_string(),
                    format!("reason={}", reason),
                ],
            ));
        }

        // Sampling fragile
        if fragility.sampling_fragile {
            let reason = fragility
                .sampling_fragility_reason
                .as_deref()
                .unwrap_or("Results vary significantly with sampling assumptions");

            disclaimers.push(Disclaimer::warning(
                "FRAGILE_TO_SAMPLING",
                Category::Sensitivity,
                "Results are sensitive to sampling assumptions; may not generalize.",
                vec![
                    "sampling_fragile=true".to_string(),
                    format!("reason={}", reason),
                ],
            ));
        }

        // Execution fragile
        if fragility.execution_fragile {
            let reason = fragility
                .execution_fragility_reason
                .as_deref()
                .unwrap_or("Results vary significantly with execution assumptions");

            disclaimers.push(Disclaimer::warning(
                "FRAGILE_TO_EXECUTION",
                Category::Sensitivity,
                "Results are sensitive to execution assumptions; slippage estimates may be optimistic.",
                vec![
                    "execution_fragile=true".to_string(),
                    format!("reason={}", reason),
                ],
            ));
        }

        // Requires optimistic assumptions
        if fragility.requires_optimistic_assumptions {
            disclaimers.push(Disclaimer::critical(
                "REQUIRES_OPTIMISTIC_ASSUMPTIONS",
                Category::Sensitivity,
                "Strategy requires optimistic assumptions to be profitable; likely not viable in production.",
                vec!["requires_optimistic_assumptions=true".to_string()],
            ));
        }
    }
}

// =============================================================================
// RULE 7: REPRODUCIBILITY DISCLAIMERS
// =============================================================================

fn generate_reproducibility_disclaimers(
    ctx: &DisclaimerContext,
    disclaimers: &mut Vec<Disclaimer>,
) {
    // Fingerprint missing
    if ctx.run_fingerprint.is_none() && ctx.config.production_grade {
        disclaimers.push(Disclaimer::critical(
            "MISSING_RUN_FINGERPRINT",
            Category::Reproducibility,
            "Run fingerprint was not computed: reproducibility cannot be verified.",
            vec!["run_fingerprint=None".to_string()],
        ));
        return;
    }

    if let Some(fp) = ctx.run_fingerprint {
        // Check for incomplete fingerprint
        let mut missing = Vec::new();

        if fp.code.hash == 0 {
            missing.push("code");
        }
        if fp.config.hash == 0 {
            missing.push("config");
        }
        if fp.dataset.hash == 0 {
            missing.push("dataset");
        }
        if fp.seed.hash == 0 {
            missing.push("seed");
        }
        if fp.behavior.hash == 0 && fp.behavior.event_count == 0 {
            missing.push("behavior");
        }

        if !missing.is_empty() {
            disclaimers.push(Disclaimer::warning(
                "INCOMPLETE_RUN_FINGERPRINT",
                Category::Reproducibility,
                "Run fingerprint is incomplete: some components could not be hashed.",
                missing
                    .iter()
                    .map(|c| format!("missing_component={}", c))
                    .collect(),
            ));
        }

        // Strategy identity missing
        if ctx.results.strategy_id.is_none() && ctx.config.production_grade {
            disclaimers.push(Disclaimer::warning(
                "MISSING_STRATEGY_ID",
                Category::Reproducibility,
                "Strategy identity not provided: results cannot be attributed to a specific strategy version.",
                vec!["strategy_id=None".to_string()],
            ));
        }
    }
}

// =============================================================================
// TESTS
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backtest_v2::gate_suite::GateMode;

    #[allow(dead_code)]
    fn make_default_config() -> BacktestConfig {
        BacktestConfig::default()
    }

    fn make_research_config() -> BacktestConfig {
        BacktestConfig::research_mode()
    }

    fn make_default_results() -> BacktestResults {
        BacktestResults::default()
    }

    fn make_production_results() -> BacktestResults {
        let mut results = BacktestResults::default();
        results.production_grade = true;
        results.strict_accounting_enabled = true;
        results.invariant_mode = InvariantMode::Hard;
        results.dataset_readiness = DatasetReadiness::MakerViable;
        results.settlement_model = SettlementModel::ExactSpec;
        results.gate_suite_passed = true;
        results.trust_level = GateTrustLevel::Trusted;
        results.maker_fills_valid = true;
        results
    }

    #[test]
    fn test_non_production_run_generates_critical_disclaimer() {
        let config = make_research_config();
        let results = make_default_results();

        let ctx = DisclaimerContext {
            config: &config,
            results: &results,
            gate_suite_report: None,
            sensitivity_report: None,
            run_fingerprint: None,
            trust_decision: None,
            current_time_ns: 0,
        };

        let block = generate_disclaimers(&ctx);

        assert!(block.has_critical());
        assert!(block
            .disclaimers
            .iter()
            .any(|d| d.id == "NON_PRODUCTION_RUN"));
    }

    #[test]
    fn test_taker_only_dataset_generates_warning() {
        let config = make_research_config();
        let mut results = make_default_results();
        results.dataset_readiness = DatasetReadiness::TakerOnly;

        let ctx = DisclaimerContext {
            config: &config,
            results: &results,
            gate_suite_report: None,
            sensitivity_report: None,
            run_fingerprint: None,
            trust_decision: None,
            current_time_ns: 0,
        };

        let block = generate_disclaimers(&ctx);

        assert!(block
            .disclaimers
            .iter()
            .any(|d| d.id == "TAKER_ONLY_DATASET"));
    }

    #[test]
    fn test_gates_not_run_generates_critical() {
        let mut config = make_research_config();
        config.gate_mode = GateMode::Strict;
        let results = make_default_results();

        let ctx = DisclaimerContext {
            config: &config,
            results: &results,
            gate_suite_report: None, // Not run
            sensitivity_report: None,
            run_fingerprint: None,
            trust_decision: None,
            current_time_ns: 0,
        };

        let block = generate_disclaimers(&ctx);

        assert!(block.disclaimers.iter().any(|d| d.id == "GATES_NOT_RUN"));
    }

    #[test]
    fn test_gates_bypassed_generates_critical() {
        let mut config = make_research_config();
        config.gate_mode = GateMode::Disabled;
        let results = make_default_results();

        let ctx = DisclaimerContext {
            config: &config,
            results: &results,
            gate_suite_report: None,
            sensitivity_report: None,
            run_fingerprint: None,
            trust_decision: None,
            current_time_ns: 0,
        };

        let block = generate_disclaimers(&ctx);

        assert!(block.disclaimers.iter().any(|d| d.id == "GATES_BYPASSED"));
    }

    #[test]
    fn test_pathology_counters_generate_disclaimer() {
        let config = make_research_config();
        let mut results = make_default_results();
        results.pathology_counters.gaps_detected = 5;
        results.pathology_counters.total_missing_sequences = 10;

        let ctx = DisclaimerContext {
            config: &config,
            results: &results,
            gate_suite_report: None,
            sensitivity_report: None,
            run_fingerprint: None,
            trust_decision: None,
            current_time_ns: 0,
        };

        let block = generate_disclaimers(&ctx);

        assert!(block
            .disclaimers
            .iter()
            .any(|d| d.id == "DATA_PATHOLOGIES_DETECTED"));
    }

    #[test]
    fn test_determinism_same_input_same_output() {
        let config = make_research_config();
        let results = make_default_results();

        let ctx = DisclaimerContext {
            config: &config,
            results: &results,
            gate_suite_report: None,
            sensitivity_report: None,
            run_fingerprint: None,
            trust_decision: None,
            current_time_ns: 12345,
        };

        let block1 = generate_disclaimers(&ctx);
        let block2 = generate_disclaimers(&ctx);

        // Same count
        assert_eq!(block1.disclaimers.len(), block2.disclaimers.len());

        // Same order and content
        for (d1, d2) in block1.disclaimers.iter().zip(block2.disclaimers.iter()) {
            assert_eq!(d1.id, d2.id);
            assert_eq!(d1.severity, d2.severity);
            assert_eq!(d1.category, d2.category);
            assert_eq!(d1.message, d2.message);
            assert_eq!(d1.evidence, d2.evidence);
        }
    }

    #[test]
    fn test_ordering_severity_then_id() {
        let config = make_research_config();
        let mut results = make_default_results();
        results.pathology_counters.duplicates_dropped = 1; // Warning
        results.dataset_readiness = DatasetReadiness::NonRepresentative; // Critical

        let ctx = DisclaimerContext {
            config: &config,
            results: &results,
            gate_suite_report: None,
            sensitivity_report: None,
            run_fingerprint: None,
            trust_decision: None,
            current_time_ns: 0,
        };

        let block = generate_disclaimers(&ctx);

        // All critical disclaimers should come before warnings
        let mut seen_warning = false;
        for d in &block.disclaimers {
            if d.severity == Severity::Warning {
                seen_warning = true;
            }
            if d.severity == Severity::Critical && seen_warning {
                panic!(
                    "Critical disclaimer {} found after warning disclaimer",
                    d.id
                );
            }
        }
    }

    #[test]
    fn test_disclaimer_format_line() {
        let d = Disclaimer::critical(
            "TEST_ID",
            Category::ProductionMode,
            "Test message",
            vec!["evidence=1".to_string()],
        );

        let line = d.format_line();
        assert!(line.contains("CRITICAL"));
        assert!(line.contains("PRODUCTION_MODE"));
        assert!(line.contains("TEST_ID"));
        assert!(line.contains("Test message"));
    }

    #[test]
    fn test_block_summary() {
        let config = make_research_config();
        let results = make_default_results();

        let ctx = DisclaimerContext {
            config: &config,
            results: &results,
            gate_suite_report: None,
            sensitivity_report: None,
            run_fingerprint: None,
            trust_decision: None,
            current_time_ns: 0,
        };

        let block = generate_disclaimers(&ctx);
        let summary = block.summary();

        assert!(summary.contains("critical="));
        assert!(summary.contains("warning="));
        assert!(summary.contains("info="));
    }

    #[test]
    fn test_untrusted_results_generates_critical() {
        let config = make_research_config();
        let mut results = make_default_results();
        results.trust_level = GateTrustLevel::Untrusted {
            reasons: vec![crate::backtest_v2::gate_suite::GateFailureReason::new(
                "test",
                "test reason",
                "field",
                "actual",
                "expected",
            )],
        };

        let ctx = DisclaimerContext {
            config: &config,
            results: &results,
            gate_suite_report: None,
            sensitivity_report: None,
            run_fingerprint: None,
            trust_decision: None,
            current_time_ns: 0,
        };

        let block = generate_disclaimers(&ctx);

        assert!(block
            .disclaimers
            .iter()
            .any(|d| d.id == "UNTRUSTED_RESULTS"));
    }
}

//! Run Artifact Storage and API
//!
//! This module implements immutable run artifact storage for backtest results.
//! Every completed backtest run is persisted as a "run artifact" containing:
//! - Full BacktestResults
//! - Run manifest (fingerprint + config + dataset metadata + trust decision + disclaimers)
//! - Time-series blobs (equity curve, drawdown, per-window PnL)
//!
//! # Design Principles
//!
//! 1. **Immutability**: Once persisted, a run artifact CANNOT be modified.
//!    The `run_id` is derived from the run fingerprint hash, ensuring
//!    content-addressable storage.
//!
//! 2. **Auditability**: Every API response includes the run fingerprint,
//!    manifest hash, and ETag headers for verification.
//!
//! 3. **Trust Visibility**: All endpoints return `trust_level` and `disclaimers`
//!    so the UI cannot omit them.
//!
//! 4. **No UI Computation**: The UI receives pre-computed data; it does NOT
//!    compute "its own" versions of equity curves, metrics, etc.
//!
//! # API Schema Version
//!
//! All endpoints use a versioned schema. Breaking changes increment the version.

use crate::backtest_v2::clock::Nanos;
use crate::backtest_v2::fingerprint::{RunFingerprint, StrategyId, StrategyFingerprint};
use crate::backtest_v2::orchestrator::{BacktestConfig, BacktestResults, TrustVerdict};
use serde::{Deserialize, Serialize};
use std::hash::{Hash, Hasher};

/// Current API schema version for run artifacts.
/// Increment on breaking changes.
pub const RUN_ARTIFACT_API_VERSION: &str = "1.0.0";

/// Current storage schema version.
/// Increment when storage format changes.
pub const RUN_ARTIFACT_STORAGE_VERSION: u32 = 1;

// =============================================================================
// RUN ID
// =============================================================================

/// Unique identifier for a backtest run.
/// 
/// The run_id is derived from the run fingerprint hash, ensuring
/// content-addressable storage. Two runs with identical fingerprints
/// will have identical run_ids.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RunId(pub String);

impl RunId {
    /// Create a new RunId from a fingerprint hash.
    pub fn from_fingerprint(fingerprint: &RunFingerprint) -> Self {
        Self(format!("run_{}", fingerprint.hash_hex))
    }
    
    /// Create a new RunId from a hash hex string.
    pub fn from_hash_hex(hash_hex: &str) -> Self {
        Self(format!("run_{}", hash_hex))
    }
    
    /// Get the raw string value.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for RunId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

// =============================================================================
// RUN MANIFEST
// =============================================================================

/// Complete manifest for a backtest run.
/// 
/// Contains all metadata needed to understand and verify the run.
/// This is the "birth certificate" of the run - everything needed
/// to prove what was run, how, and whether results can be trusted.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunManifest {
    /// Schema version for this manifest.
    pub schema_version: u32,
    
    /// Unique run identifier (derived from fingerprint).
    pub run_id: RunId,
    
    /// Unix timestamp when the run was persisted.
    pub persisted_at: i64,
    
    /// Run fingerprint - cryptographic proof of the run's identity.
    pub fingerprint: RunFingerprint,
    
    /// Strategy identity (name, version, code hash).
    pub strategy: StrategyIdentity,
    
    /// Dataset metadata.
    pub dataset: DatasetMetadata,
    
    /// Configuration summary (non-sensitive subset of BacktestConfig).
    pub config_summary: ConfigSummary,
    
    /// Trust decision from TrustGate.
    pub trust_decision: TrustDecisionSummary,
    
    /// Disclaimers that MUST be displayed with results.
    pub disclaimers: Vec<Disclaimer>,
    
    /// Human-readable methodology explanation generated from config.
    /// This explains trust status in a consistent, non-editable format.
    pub methodology_capsule: MethodologyCapsule,
    
    /// Hash of this manifest for ETag/caching.
    pub manifest_hash: String,
}

impl RunManifest {
    /// Compute the manifest hash.
    pub fn compute_hash(&self) -> String {
        use std::collections::hash_map::DefaultHasher;
        let mut hasher = DefaultHasher::new();
        
        self.run_id.0.hash(&mut hasher);
        self.fingerprint.hash_hex.hash(&mut hasher);
        self.strategy.name.hash(&mut hasher);
        self.strategy.version.hash(&mut hasher);
        self.trust_decision.verdict.hash(&mut hasher);
        self.persisted_at.hash(&mut hasher);
        
        format!("{:016x}", hasher.finish())
    }
    
    /// Create a new manifest from run results.
    pub fn from_results(
        results: &BacktestResults,
        config: &BacktestConfig,
    ) -> Self {
        let fingerprint = results.run_fingerprint.clone()
            .unwrap_or_else(|| {
                // Create a minimal fingerprint if not present
                RunFingerprint {
                    version: crate::backtest_v2::fingerprint::FINGERPRINT_VERSION.to_string(),
                    strategy: StrategyFingerprint::default(),
                    code: crate::backtest_v2::fingerprint::CodeFingerprint::new(),
                    config: crate::backtest_v2::fingerprint::ConfigFingerprint::from_config(config),
                    dataset: crate::backtest_v2::fingerprint::DatasetFingerprint {
                        classification: format!("{:?}", results.data_quality.mode),
                        readiness: format!("{:?}", results.dataset_readiness),
                        orderbook_type: "Unknown".to_string(),
                        trade_type: "Unknown".to_string(),
                        arrival_semantics: "Unknown".to_string(),
                        streams: vec![],
                        hash: 0,
                    },
                    seed: crate::backtest_v2::fingerprint::SeedFingerprint::new(config.seed),
                    behavior: crate::backtest_v2::fingerprint::BehaviorFingerprint {
                        event_count: results.events_processed,
                        hash: 0,
                    },
                    registry: None,
                    hash: 0,
                    hash_hex: "0000000000000000".to_string(),
                }
            });
        
        let run_id = RunId::from_fingerprint(&fingerprint);
        
        let strategy = StrategyIdentity::from_strategy_id(
            results.strategy_id.as_ref()
                .or(config.strategy_id.as_ref())
        );
        
        let dataset = DatasetMetadata {
            classification: format!("{:?}", results.data_quality.mode),
            readiness: format!("{:?}", results.dataset_readiness),
            events_processed: results.events_processed,
            delta_events_processed: results.delta_events_processed,
            time_range: TimeRangeSummary {
                start_ns: results.duration_ns.saturating_sub(results.duration_ns), // Placeholder
                end_ns: results.duration_ns,
                duration_ns: results.duration_ns,
            },
        };
        
        let config_summary = ConfigSummary {
            production_grade: config.production_grade,
            strict_mode: config.strict_mode,
            strict_accounting: config.strict_accounting,
            maker_fill_model: format!("{:?}", config.maker_fill_model),
            oms_parity_mode: format!("{:?}", config.oms_parity_mode),
            seed: config.seed,
        };
        
        let trust_decision = TrustDecisionSummary::from_results(results);
        
        let disclaimers = generate_disclaimers(results, config);
        
        // Generate methodology capsule - human-readable explanation of trust status
        let methodology_capsule = MethodologyCapsule::generate(config, results);
        
        let mut manifest = Self {
            schema_version: RUN_ARTIFACT_STORAGE_VERSION,
            run_id,
            persisted_at: chrono::Utc::now().timestamp(),
            fingerprint,
            strategy,
            dataset,
            config_summary,
            trust_decision,
            disclaimers,
            methodology_capsule,
            manifest_hash: String::new(),
        };
        
        manifest.manifest_hash = manifest.compute_hash();
        manifest
    }
}

/// Strategy identity summary for the manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyIdentity {
    pub name: String,
    pub version: String,
    pub code_hash: Option<String>,
}

impl StrategyIdentity {
    pub fn from_strategy_id(id: Option<&StrategyId>) -> Self {
        match id {
            Some(sid) => Self {
                name: sid.name.clone(),
                version: sid.version.clone(),
                code_hash: sid.code_hash.clone(),
            },
            None => Self {
                name: "unknown".to_string(),
                version: "0.0.0".to_string(),
                code_hash: None,
            },
        }
    }
}

/// Dataset metadata for the manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatasetMetadata {
    pub classification: String,
    pub readiness: String,
    pub events_processed: u64,
    pub delta_events_processed: u64,
    pub time_range: TimeRangeSummary,
}

/// Time range summary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimeRangeSummary {
    pub start_ns: Nanos,
    pub end_ns: Nanos,
    pub duration_ns: Nanos,
}

/// Configuration summary (non-sensitive).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigSummary {
    pub production_grade: bool,
    pub strict_mode: bool,
    pub strict_accounting: bool,
    pub maker_fill_model: String,
    pub oms_parity_mode: String,
    pub seed: u64,
}

// =============================================================================
// METHODOLOGY CAPSULE
// =============================================================================

/// A human-readable methodology explanation generated from config flags.
/// 
/// This is an audit artifact - it cannot be edited after generation.
/// Institutional users can share runs with consistent, non-editable explanations
/// of why results are trusted or untrusted.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MethodologyCapsule {
    /// Schema version for this capsule format.
    pub version: String,
    
    /// One-paragraph methodology summary (2-4 sentences).
    pub summary: String,
    
    /// Key-value pairs for structured display.
    pub details: Vec<MethodologyDetail>,
    
    /// Hash of inputs used to generate this capsule (for verification).
    pub input_hash: String,
}

/// A single key-value detail in the methodology capsule.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MethodologyDetail {
    /// Display label (e.g., "Production Grade").
    pub label: String,
    /// Display value (e.g., "Yes").
    pub value: String,
    /// Optional tooltip explaining this field.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tooltip: Option<String>,
}

/// Current version of the methodology capsule schema.
pub const METHODOLOGY_CAPSULE_VERSION: &str = "1.0";

impl MethodologyCapsule {
    /// Generate a methodology capsule from config and results.
    /// 
    /// This produces a human-readable explanation of:
    /// - Whether the run is production-grade
    /// - Settlement source and reference rule
    /// - Dataset readiness (maker viability)
    /// - Why the run is trusted or untrusted
    pub fn generate(config: &BacktestConfig, results: &BacktestResults) -> Self {
        let mut summary_parts: Vec<String> = Vec::new();
        
        // 1. Production grade status
        if config.production_grade {
            summary_parts.push(
                "This backtest was executed in production-grade mode with all \
                 correctness invariants enforced.".to_string()
            );
        } else {
            summary_parts.push(
                "This backtest was executed in research mode and may use \
                 optimistic assumptions.".to_string()
            );
        }
        
        // 2. Settlement source
        let settlement_desc = if let Some(ref spec) = config.settlement_spec {
            let source = if config.oracle_config.is_some() {
                "Chainlink oracles on Polygon"
            } else {
                "configured oracle feeds"
            };
            format!(
                "Settlement prices are derived from {} using the {:?} reference rule.",
                source,
                spec.reference_price_rule
            )
        } else {
            "Settlement uses simulated prices (no oracle configuration).".to_string()
        };
        summary_parts.push(settlement_desc);
        
        // 3. Maker viability / dataset readiness
        let readiness_desc = match format!("{:?}", results.dataset_readiness).as_str() {
            "MakerViable" => 
                "The dataset supports maker (passive) order simulation with queue modeling.",
            "TakerOnly" => 
                "The dataset supports taker (aggressive) execution only; maker fills are not simulated.",
            _ =>
                "The dataset lacks sufficient fidelity for production-grade claims.",
        };
        summary_parts.push(readiness_desc.to_string());
        
        // 4. Trust status explanation
        if results.truthfulness.is_trusted() {
            summary_parts.push(
                "All trust requirements passed; results may be used for deployment decisions.".to_string()
            );
        } else {
            let reasons = if results.truthfulness.untrusted_reasons.is_empty() {
                "unspecified requirements not met".to_string()
            } else {
                results.truthfulness.untrusted_reasons.join("; ")
            };
            summary_parts.push(format!(
                "Trust requirements not satisfied: {}. Exercise caution when interpreting results.",
                reasons
            ));
        }
        
        let summary = summary_parts.join(" ");
        
        // Build structured details
        let details = vec![
            MethodologyDetail {
                label: "Production Grade".into(),
                value: if config.production_grade { "Yes" } else { "No" }.into(),
                tooltip: Some("Whether all correctness invariants were enforced".into()),
            },
            MethodologyDetail {
                label: "Settlement Source".into(),
                value: if config.oracle_config.is_some() {
                    "Chainlink (Polygon)".into()
                } else if config.settlement_spec.is_some() {
                    "Configured Oracle".into()
                } else {
                    "Simulated".into()
                },
                tooltip: Some("Source of settlement/reference prices".into()),
            },
            MethodologyDetail {
                label: "Settlement Rule".into(),
                value: config.settlement_spec.as_ref()
                    .map(|s| format!("{:?}", s.reference_price_rule))
                    .unwrap_or_else(|| "N/A".into()),
                tooltip: Some("Rule for selecting settlement price from oracle data".into()),
            },
            MethodologyDetail {
                label: "Dataset Readiness".into(),
                value: format!("{:?}", results.dataset_readiness),
                tooltip: Some("Whether dataset supports maker/taker simulation".into()),
            },
            MethodologyDetail {
                label: "Maker Fill Model".into(),
                value: format!("{:?}", config.maker_fill_model),
                tooltip: Some("Model used for passive order fills".into()),
            },
            MethodologyDetail {
                label: "Strict Accounting".into(),
                value: if config.strict_accounting { "Enabled" } else { "Disabled" }.into(),
                tooltip: Some("Whether ledger-based accounting was enforced".into()),
            },
            MethodologyDetail {
                label: "Trust Status".into(),
                value: format!("{:?}", results.truthfulness.verdict),
                tooltip: results.truthfulness.untrusted_reasons.first().cloned(),
            },
        ];
        
        // Compute input hash for verification
        let input_hash = Self::compute_input_hash(config, results);
        
        Self {
            version: METHODOLOGY_CAPSULE_VERSION.into(),
            summary,
            details,
            input_hash,
        }
    }
    
    /// Compute a hash of the inputs used to generate this capsule.
    fn compute_input_hash(config: &BacktestConfig, results: &BacktestResults) -> String {
        use std::collections::hash_map::DefaultHasher;
        let mut hasher = DefaultHasher::new();
        
        config.production_grade.hash(&mut hasher);
        config.strict_accounting.hash(&mut hasher);
        format!("{:?}", config.maker_fill_model).hash(&mut hasher);
        format!("{:?}", results.dataset_readiness).hash(&mut hasher);
        format!("{:?}", results.truthfulness.verdict).hash(&mut hasher);
        results.truthfulness.untrusted_reasons.len().hash(&mut hasher);
        
        format!("{:016x}", hasher.finish())
    }
}

// =============================================================================
// TRUST LEVEL DTO (Structured JSON - no Debug strings)
// =============================================================================

/// Trust status enum for API responses.
/// This is serialized as a simple string like "Trusted", "Untrusted", etc.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum TrustStatus {
    Trusted,
    Untrusted,
    Bypassed,
    Unknown,
    NonRepresentative,
}

impl TrustStatus {
    /// Convert from internal TrustLevel
    pub fn from_trust_level(trust_level: &crate::backtest_v2::gate_suite::TrustLevel) -> Self {
        use crate::backtest_v2::gate_suite::TrustLevel;
        match trust_level {
            TrustLevel::Trusted => TrustStatus::Trusted,
            TrustLevel::Untrusted { .. } => TrustStatus::Untrusted,
            TrustLevel::Bypassed => TrustStatus::Bypassed,
            TrustLevel::Unknown => TrustStatus::Unknown,
        }
    }
}

/// Structured trust level for API responses.
/// This replaces the Debug-formatted string with proper JSON.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrustLevelDto {
    /// Simple status enum: "Trusted", "Untrusted", "Bypassed", "Unknown"
    pub status: TrustStatus,
    /// Human-readable reasons (empty for Trusted runs)
    pub reasons: Vec<String>,
}

impl TrustLevelDto {
    /// Create from internal TrustLevel
    pub fn from_trust_level(trust_level: &crate::backtest_v2::gate_suite::TrustLevel) -> Self {
        use crate::backtest_v2::gate_suite::TrustLevel;
        let status = TrustStatus::from_trust_level(trust_level);
        let reasons = match trust_level {
            TrustLevel::Untrusted { reasons } => {
                reasons.iter().map(|r| format!("{:?}", r)).collect()
            }
            _ => vec![],
        };
        Self { status, reasons }
    }
    
    /// Create from BacktestResults
    pub fn from_results(results: &BacktestResults) -> Self {
        Self::from_trust_level(&results.trust_level)
    }
}

/// Trust decision summary for the manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrustDecisionSummary {
    pub verdict: String,
    /// Structured trust level (no Debug strings)
    /// Uses custom deserializer for backward compatibility with old string format
    #[serde(deserialize_with = "deserialize_trust_level")]
    pub trust_level: TrustLevelDto,
    pub is_trusted: bool,
    pub failure_reasons: Vec<String>,
}

/// Custom deserializer that handles both old string format and new TrustLevelDto format
fn deserialize_trust_level<'de, D>(deserializer: D) -> Result<TrustLevelDto, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::{self, Visitor};
    
    struct TrustLevelVisitor;
    
    impl<'de> Visitor<'de> for TrustLevelVisitor {
        type Value = TrustLevelDto;
        
        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            formatter.write_str("a TrustLevelDto object or a legacy string")
        }
        
        // Handle old string format: "Trusted" or "Untrusted { reasons: [...] }"
        fn visit_str<E>(self, value: &str) -> Result<TrustLevelDto, E>
        where
            E: de::Error,
        {
            let (status, reasons) = if value == "Trusted" {
                (TrustStatus::Trusted, vec![])
            } else if value.starts_with("Untrusted") {
                // Parse reasons from "Untrusted { reasons: [...] }" if present
                let reasons = if let Some(start) = value.find('[') {
                    if let Some(end) = value.rfind(']') {
                        value[start+1..end]
                            .split(',')
                            .map(|s| s.trim().to_string())
                            .filter(|s| !s.is_empty())
                            .collect()
                    } else {
                        vec![]
                    }
                } else {
                    vec![]
                };
                (TrustStatus::Untrusted, reasons)
            } else if value == "Bypassed" {
                (TrustStatus::Bypassed, vec![])
            } else {
                (TrustStatus::Unknown, vec![])
            };
            
            Ok(TrustLevelDto { status, reasons })
        }
        
        // Handle new structured format
        fn visit_map<M>(self, map: M) -> Result<TrustLevelDto, M::Error>
        where
            M: de::MapAccess<'de>,
        {
            #[derive(Deserialize)]
            struct TrustLevelDtoHelper {
                status: TrustStatus,
                reasons: Vec<String>,
            }
            
            let helper = TrustLevelDtoHelper::deserialize(de::value::MapAccessDeserializer::new(map))?;
            Ok(TrustLevelDto {
                status: helper.status,
                reasons: helper.reasons,
            })
        }
    }
    
    deserializer.deserialize_any(TrustLevelVisitor)
}

impl TrustDecisionSummary {
    pub fn from_results(results: &BacktestResults) -> Self {
        let verdict = match results.truthfulness.verdict {
            TrustVerdict::Trusted => "Trusted".to_string(),
            TrustVerdict::Untrusted => "Untrusted".to_string(),
            TrustVerdict::Inconclusive => "Inconclusive".to_string(),
        };
        let trust_level = TrustLevelDto::from_results(results);
        let is_trusted = results.truthfulness.verdict == TrustVerdict::Trusted;
        
        let failure_reasons = if let Some(ref decision) = results.trust_decision {
            decision.failure_reasons()
                .iter()
                .map(|r| r.description().to_string())
                .collect()
        } else {
            results.truthfulness.untrusted_reasons.clone()
        };
        
        Self {
            verdict,
            trust_level,
            is_trusted,
            failure_reasons,
        }
    }
}

/// Disclaimer that MUST be displayed with results.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Disclaimer {
    /// Unique code for this disclaimer type.
    pub code: String,
    /// Severity level: "info", "warning", "danger".
    pub severity: String,
    /// Human-readable title.
    pub title: String,
    /// Detailed explanation.
    pub description: String,
    /// Whether this disclaimer MUST be acknowledged before acting on results.
    pub requires_acknowledgment: bool,
}

/// Generate disclaimers based on run results.
fn generate_disclaimers(results: &BacktestResults, config: &BacktestConfig) -> Vec<Disclaimer> {
    let mut disclaimers = Vec::new();
    
    // Always include the standard backtest disclaimer
    disclaimers.push(Disclaimer {
        code: "BACKTEST_DISCLAIMER".to_string(),
        severity: "warning".to_string(),
        title: "Backtest Results - Not Live Performance".to_string(),
        description: "These results are from historical simulation and do not represent \
            actual trading performance. Past performance does not guarantee future results. \
            Backtests may suffer from look-ahead bias, survivorship bias, and other \
            limitations that can inflate apparent returns.".to_string(),
        requires_acknowledgment: true,
    });
    
    // Trust level disclaimer
    if !results.truthfulness.is_trusted() {
        disclaimers.push(Disclaimer {
            code: "UNTRUSTED_RESULTS".to_string(),
            severity: "danger".to_string(),
            title: "Results Cannot Be Trusted".to_string(),
            description: format!(
                "This backtest did not pass all trust requirements. Reasons: {}",
                results.truthfulness.untrusted_reasons.join("; ")
            ),
            requires_acknowledgment: true,
        });
    }
    
    // Non-production mode disclaimer
    if !config.production_grade {
        disclaimers.push(Disclaimer {
            code: "NON_PRODUCTION_MODE".to_string(),
            severity: "warning".to_string(),
            title: "Non-Production Mode".to_string(),
            description: "This backtest was run in non-production mode. Results may use \
                optimistic assumptions and should not be used for deployment decisions.".to_string(),
            requires_acknowledgment: false,
        });
    }
    
    // Maker fill model disclaimer
    if results.maker_fill_model == crate::backtest_v2::orchestrator::MakerFillModel::Optimistic {
        disclaimers.push(Disclaimer {
            code: "OPTIMISTIC_FILLS".to_string(),
            severity: "warning".to_string(),
            title: "Optimistic Fill Assumptions".to_string(),
            description: "This backtest used optimistic fill assumptions for maker orders. \
                Actual queue position and fill probability may differ significantly.".to_string(),
            requires_acknowledgment: false,
        });
    }
    
    // Sensitivity fragility disclaimer
    if results.sensitivity_report.fragility.is_fragile() {
        disclaimers.push(Disclaimer {
            code: "SENSITIVITY_FRAGILITY".to_string(),
            severity: "warning".to_string(),
            title: "Results Are Sensitive to Assumptions".to_string(),
            description: format!(
                "Sensitivity analysis detected fragility in results. Small changes in \
                assumptions may significantly affect outcomes. Fragility score: {:.2}",
                results.sensitivity_report.fragility.fragility_score
            ),
            requires_acknowledgment: false,
        });
    }
    
    disclaimers
}

// =============================================================================
// RUN ARTIFACT
// =============================================================================

/// Complete run artifact containing all data from a backtest run.
/// 
/// This is the immutable record persisted to storage.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunArtifact {
    /// Manifest containing metadata, fingerprint, trust decision, disclaimers.
    pub manifest: RunManifest,
    
    /// Full backtest results.
    pub results: BacktestResults,
    
    /// Time-series data (equity curve, drawdown, per-window PnL).
    pub time_series: RunTimeSeries,
    
    /// Distribution data for histograms.
    pub distributions: RunDistributions,
}

impl RunArtifact {
    /// Create a new run artifact from results.
    pub fn from_results(results: BacktestResults, config: &BacktestConfig) -> Self {
        let manifest = RunManifest::from_results(&results, config);
        let time_series = RunTimeSeries::from_results(&results);
        let distributions = RunDistributions::from_results(&results);
        
        Self {
            manifest,
            results,
            time_series,
            distributions,
        }
    }
    
    /// Get the run ID.
    pub fn run_id(&self) -> &RunId {
        &self.manifest.run_id
    }
    
    /// Get the ETag value for HTTP caching.
    pub fn etag(&self) -> String {
        format!("\"{}\"", self.manifest.manifest_hash)
    }
    
    /// Check if results are trusted.
    pub fn is_trusted(&self) -> bool {
        self.manifest.trust_decision.is_trusted
    }
}

/// Time-series data extracted from results.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunTimeSeries {
    /// Equity curve points.
    pub equity_curve: Option<Vec<EquityPoint>>,
    
    /// Drawdown series (computed from equity curve).
    pub drawdown_series: Option<Vec<DrawdownPoint>>,
    
    /// Per-window PnL.
    pub window_pnl: Option<Vec<WindowPnLPoint>>,
    
    /// PnL history (legacy format).
    pub pnl_history: Vec<f64>,
}

/// Single point on the equity curve.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EquityPoint {
    pub timestamp_ns: Nanos,
    pub equity: f64,
    pub cash: f64,
    pub positions_value: f64,
}

/// Single point on the drawdown series.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DrawdownPoint {
    pub timestamp_ns: Nanos,
    pub drawdown_abs: f64,
    pub drawdown_pct: f64,
    pub peak_equity: f64,
}

/// Per-window PnL point.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowPnLPoint {
    pub window_start_ns: Nanos,
    pub window_end_ns: Nanos,
    pub market_id: String,
    pub gross_pnl: f64,
    pub fees: f64,
    pub net_pnl: f64,
    pub trades_count: u32,
    pub outcome: String,
}

impl RunTimeSeries {
    pub fn from_results(results: &BacktestResults) -> Self {
        use crate::backtest_v2::ledger::from_amount;
        
        // Extract equity curve using the public points() method
        let equity_curve: Option<Vec<EquityPoint>> = results.equity_curve.as_ref().map(|curve| {
            curve.points().iter().map(|p| EquityPoint {
                timestamp_ns: p.time_ns,
                equity: from_amount(p.equity_value),
                cash: from_amount(p.cash_balance),
                positions_value: from_amount(p.position_value),
            }).collect()
        });
        
        // Compute drawdown series from equity curve
        let drawdown_series: Option<Vec<DrawdownPoint>> = equity_curve.as_ref().map(|points| {
            compute_drawdown_series(points)
        });
        
        // Extract window PnL
        let window_pnl: Option<Vec<WindowPnLPoint>> = results.window_pnl.as_ref().map(|series| {
            series.windows.iter().map(|w| WindowPnLPoint {
                window_start_ns: w.window_start_ns,
                window_end_ns: w.window_end_ns,
                market_id: w.market_id.clone(),
                gross_pnl: w.gross_pnl_f64(),
                fees: w.fees_f64(),
                net_pnl: w.net_pnl_f64(),
                trades_count: w.trades_count as u32,
                outcome: w.outcome.as_ref()
                    .map(|o| format!("{:?}", o))
                    .unwrap_or_else(|| "Unknown".to_string()),
            }).collect()
        });
        
        Self {
            equity_curve,
            drawdown_series,
            window_pnl,
            pnl_history: vec![], // Legacy - not used in v2
        }
    }
}

/// Compute drawdown series from equity points.
fn compute_drawdown_series(equity_points: &[EquityPoint]) -> Vec<DrawdownPoint> {
    let mut peak = f64::MIN;
    let mut drawdowns = Vec::with_capacity(equity_points.len());
    
    for point in equity_points {
        if point.equity > peak {
            peak = point.equity;
        }
        
        let drawdown_abs = peak - point.equity;
        let drawdown_pct = if peak > 0.0 {
            drawdown_abs / peak
        } else {
            0.0
        };
        
        drawdowns.push(DrawdownPoint {
            timestamp_ns: point.timestamp_ns,
            drawdown_abs,
            drawdown_pct,
            peak_equity: peak,
        });
    }
    
    drawdowns
}

/// Distribution data for histograms.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunDistributions {
    /// Trade PnL distribution bins.
    pub trade_pnl_bins: Vec<DistributionBin>,
    
    /// Trade size distribution bins.
    pub trade_size_bins: Vec<DistributionBin>,
    
    /// Hold time distribution bins (if available).
    pub hold_time_bins: Vec<DistributionBin>,
    
    /// Fill price slippage distribution bins.
    pub slippage_bins: Vec<DistributionBin>,
}

/// Single bin in a distribution histogram.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DistributionBin {
    pub bin_start: f64,
    pub bin_end: f64,
    pub count: u64,
    pub percentage: f64,
}

impl RunDistributions {
    pub fn from_results(_results: &BacktestResults) -> Self {
        // TODO: Compute actual distributions from fill records
        // For now, return empty distributions
        Self {
            trade_pnl_bins: vec![],
            trade_size_bins: vec![],
            hold_time_bins: vec![],
            slippage_bins: vec![],
        }
    }
}

// =============================================================================
// RUN SUMMARY (for list view)
// =============================================================================

/// Strategy identity for API responses.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyIdDto {
    pub name: String,
    pub version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code_hash: Option<String>,
}

/// Dataset readiness level for API responses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DatasetReadinessDto {
    TakerOnly,
    MakerViable,
    NonRepresentative,
}

impl DatasetReadinessDto {
    pub fn from_readiness(readiness: &crate::backtest_v2::data_contract::DatasetReadiness) -> Self {
        use crate::backtest_v2::data_contract::DatasetReadiness;
        match readiness {
            DatasetReadiness::TakerOnly => DatasetReadinessDto::TakerOnly,
            DatasetReadiness::MakerViable => DatasetReadinessDto::MakerViable,
            DatasetReadiness::NonRepresentative { .. } => DatasetReadinessDto::NonRepresentative,
        }
    }
}

/// Lightweight summary of a run for list views.
/// All fields use structured JSON types (no Debug strings).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunSummary {
    pub run_id: RunId,
    pub persisted_at: i64,
    
    /// Strategy identity (structured).
    pub strategy_id: StrategyIdDto,
    
    /// Key metrics.
    pub final_pnl: f64,
    pub total_fills: u64,
    pub sharpe_ratio: Option<f64>,
    pub max_drawdown: f64,
    pub win_rate: f64,
    
    /// Structured trust status (no Debug strings).
    pub trust_level: TrustLevelDto,
    pub is_trusted: bool,
    
    /// Visibility and certification flags.
    pub is_published: bool,
    pub is_certified: bool,
    
    /// Production mode flag.
    pub production_grade: bool,
    
    /// Dataset readiness level.
    pub dataset_readiness: DatasetReadinessDto,
    
    /// Fingerprint hash for verification.
    pub fingerprint_hash: String,
    
    /// Manifest hash for verification.
    pub manifest_hash: String,
    
    // Provenance fields for CertifiedRunFooter (mandatory)
    /// Results schema version (e.g., "v1").
    pub schema_version: String,
    /// Unix timestamp when the run was published (UTC, seconds).
    pub publish_timestamp: i64,
}

impl RunSummary {
    pub fn from_artifact(artifact: &RunArtifact) -> Self {
        let strategy_id = StrategyIdDto {
            name: artifact.manifest.strategy.name.clone(),
            version: artifact.manifest.strategy.version.clone(),
            code_hash: artifact.manifest.strategy.code_hash.clone(),
        };
        
        let trust_level = artifact.manifest.trust_decision.trust_level.clone();
        let is_trusted = artifact.manifest.trust_decision.is_trusted;
        
        // A run is "published" if it has valid strategy identity and is finalized
        let is_published = artifact.manifest.strategy.name != "unknown" 
            && artifact.manifest.strategy.version != "0.0.0"
            && !artifact.manifest.fingerprint.hash_hex.is_empty();
        
        // A run is "certified" if it's trusted, production-grade, and published
        let is_certified = is_trusted 
            && artifact.results.production_grade 
            && is_published;
        
        let dataset_readiness = DatasetReadinessDto::from_readiness(&artifact.results.dataset_readiness);
        
        Self {
            run_id: artifact.manifest.run_id.clone(),
            persisted_at: artifact.manifest.persisted_at,
            strategy_id,
            final_pnl: artifact.results.final_pnl,
            total_fills: artifact.results.total_fills,
            sharpe_ratio: artifact.results.sharpe_ratio,
            max_drawdown: artifact.results.max_drawdown,
            win_rate: artifact.results.win_rate,
            trust_level,
            is_trusted,
            is_published,
            is_certified,
            production_grade: artifact.results.production_grade,
            dataset_readiness,
            fingerprint_hash: artifact.manifest.fingerprint.hash_hex.clone(),
            manifest_hash: artifact.manifest.manifest_hash.clone(),
            schema_version: format!("v{}", artifact.manifest.schema_version),
            publish_timestamp: artifact.manifest.persisted_at,
        }
    }
    
    /// Check if this run should be shown in public/certified views.
    pub fn is_visible_public(&self) -> bool {
        self.is_published
    }
}

// =============================================================================
// API RESPONSE TYPES
// =============================================================================

/// Provenance block for audit and institutional use.
/// 
/// Contains all the information needed to verify the integrity and lineage
/// of a backtest run. This is immutable - set at publication time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProvenanceBlock {
    /// Schema version of the stored artifact.
    pub schema_version: String,
    
    /// Unix timestamp when the run was published (UTC, seconds).
    pub publish_timestamp: i64,
    
    /// Dataset version identifier used for this run.
    pub dataset_version_id: String,
    
    /// Dataset readiness level (e.g., "MakerViable", "TakerOnly").
    pub dataset_readiness: String,
    
    /// Settlement source (e.g., "Chainlink/ETH-USD", "Simulated").
    pub settlement_source: String,
    
    /// Integrity policy applied during the run.
    pub integrity_policy: String,
    
    /// Hash of the strategy code used.
    pub strategy_code_hash: String,
    
    /// Run fingerprint hash for cryptographic verification.
    pub fingerprint_hash: String,
}

impl ProvenanceBlock {
    /// Create provenance block from artifact.
    pub fn from_artifact(artifact: &RunArtifact) -> Self {
        Self {
            schema_version: format!("v{}", artifact.manifest.schema_version),
            publish_timestamp: artifact.manifest.persisted_at,
            dataset_version_id: artifact.manifest.fingerprint.dataset.hash.to_string(),
            dataset_readiness: artifact.manifest.dataset.readiness.clone(),
            settlement_source: artifact.manifest.fingerprint.config.chainlink_feed_id
                .as_ref()
                .map(|f| format!("Chainlink/{}", f))
                .unwrap_or_else(|| "Simulated".to_string()),
            integrity_policy: artifact.manifest.fingerprint.config.integrity_policy.clone(),
            strategy_code_hash: artifact.manifest.strategy.code_hash
                .clone()
                .unwrap_or_else(|| "none".to_string()),
            fingerprint_hash: artifact.manifest.fingerprint.hash_hex.clone(),
        }
    }
}

/// Base response wrapper that includes trust info, fingerprint, and provenance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactResponse<T> {
    /// API schema version.
    pub api_version: String,
    
    /// Run identifier.
    pub run_id: RunId,
    
    /// Fingerprint hash for verification.
    pub fingerprint_hash: String,
    
    /// Manifest hash (for ETag).
    pub manifest_hash: String,
    
    /// Trust level - ALWAYS included (structured DTO).
    pub trust_level: TrustLevelDto,
    
    /// Whether results are trusted.
    pub is_trusted: bool,
    
    /// Disclaimers - ALWAYS included.
    pub disclaimers: Vec<Disclaimer>,
    
    /// Provenance block for audit and institutional use.
    pub provenance: ProvenanceBlock,
    
    /// The actual data.
    pub data: T,
}

impl<T> ArtifactResponse<T> {
    pub fn new(artifact: &RunArtifact, data: T) -> Self {
        Self {
            api_version: RUN_ARTIFACT_API_VERSION.to_string(),
            run_id: artifact.manifest.run_id.clone(),
            fingerprint_hash: artifact.manifest.fingerprint.hash_hex.clone(),
            manifest_hash: artifact.manifest.manifest_hash.clone(),
            trust_level: artifact.manifest.trust_decision.trust_level.clone(),
            is_trusted: artifact.manifest.trust_decision.is_trusted,
            disclaimers: artifact.manifest.disclaimers.clone(),
            provenance: ProvenanceBlock::from_artifact(artifact),
            data,
        }
    }
    
    /// Get the ETag value for HTTP caching.
    pub fn etag(&self) -> String {
        format!("\"{}\"", self.manifest_hash)
    }
}

// =============================================================================
// PAGINATION AND SORTING
// =============================================================================

/// Sort field for run listings.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RunSortField {
    /// Sort by publication timestamp (default).
    #[default]
    PersistedAt,
    /// Sort by final PnL.
    FinalPnl,
    /// Sort by Sharpe ratio.
    SharpeRatio,
    /// Sort by win rate.
    WinRate,
    /// Sort by strategy name.
    StrategyName,
    /// Sort by max drawdown.
    MaxDrawdown,
}

impl RunSortField {
    /// Get the SQL column name for this sort field.
    pub fn sql_column(&self) -> &'static str {
        match self {
            Self::PersistedAt => "persisted_at",
            Self::FinalPnl => "final_pnl",
            Self::SharpeRatio => "sharpe_ratio",
            Self::WinRate => "win_rate",
            Self::StrategyName => "strategy_name",
            Self::MaxDrawdown => "max_drawdown",
        }
    }
}

/// Sort direction.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SortOrder {
    /// Descending order (default for most fields).
    #[default]
    Desc,
    /// Ascending order.
    Asc,
}

impl SortOrder {
    /// Get the SQL keyword for this sort direction.
    pub fn sql_keyword(&self) -> &'static str {
        match self {
            Self::Desc => "DESC",
            Self::Asc => "ASC",
        }
    }
}

/// Response for listing runs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListRunsResponse {
    /// API schema version.
    pub api_version: String,
    
    /// Total number of matching runs.
    pub total_count: usize,
    
    /// Current page (0-indexed).
    pub page: usize,
    
    /// Page size used.
    pub page_size: usize,
    
    /// Total number of pages.
    pub total_pages: usize,
    
    /// Whether there is a next page.
    pub has_next: bool,
    
    /// Whether there is a previous page.
    pub has_prev: bool,
    
    /// Sort field used.
    pub sort_by: RunSortField,
    
    /// Sort direction used.
    pub sort_order: SortOrder,
    
    /// The runs on this page.
    pub runs: Vec<RunSummary>,
}

/// Filters for listing runs.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ListRunsFilter {
    /// Filter by strategy name.
    pub strategy_name: Option<String>,
    
    /// Filter by trust status.
    pub trusted_only: Option<bool>,
    
    /// Filter by production grade.
    pub production_grade_only: Option<bool>,
    
    /// Filter to show only published runs (default: true for public views).
    /// Published runs have valid strategy identity and finalized fingerprint.
    pub published_only: Option<bool>,
    
    /// Filter to show only certified runs.
    /// Certified = trusted + production_grade + published.
    pub certified_only: Option<bool>,
    
    /// Include internal/test runs (default: false).
    /// Only for debugging purposes.
    pub include_internal: Option<bool>,
    
    /// Filter by minimum PnL.
    pub min_pnl: Option<f64>,
    
    /// Filter by date range (start timestamp).
    pub after: Option<i64>,
    
    /// Filter by date range (end timestamp).
    pub before: Option<i64>,
    
    /// Page number (0-indexed).
    pub page: Option<usize>,
    
    /// Page size (default 20, max 100).
    pub page_size: Option<usize>,
    
    /// Sort field (default: persisted_at).
    pub sort_by: Option<RunSortField>,
    
    /// Sort direction (default: desc).
    pub sort_order: Option<SortOrder>,
}

// =============================================================================
// WINDOW PNL HISTOGRAM (CERTIFIED / DETERMINISTIC)
// =============================================================================

/// Schema version for window PnL histogram responses.
pub const WINDOW_PNL_HISTOGRAM_SCHEMA_VERSION: &str = "v1";

/// Binning method used for histogram computation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BinningMethod {
    /// Fixed edges computed from min/max with uniform spacing.
    FixedEdges,
    /// Backend-computed bins (deterministic).
    BackendV1,
}

/// Binning configuration for the histogram.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BinningConfig {
    /// Method used for binning.
    pub method: BinningMethod,
    /// Number of bins.
    pub bin_count: usize,
    /// Minimum value (left edge of first bin).
    pub min: f64,
    /// Maximum value (right edge of last bin).
    pub max: f64,
}

/// A single histogram bin with explicit edges.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HistogramBin {
    /// Left edge of the bin (inclusive).
    pub left: f64,
    /// Right edge of the bin (exclusive, except for the last bin).
    pub right: f64,
    /// Number of samples in this bin.
    pub count: u64,
}

/// Response for GET /api/runs/{run_id}/distribution/window_pnl
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowPnlHistogramResponse {
    /// Schema version for forwards compatibility.
    pub schema_version: String,
    /// Run identifier.
    pub run_id: String,
    /// Manifest hash for verification.
    pub manifest_hash: String,
    /// Unit of the PnL values.
    pub unit: String,
    /// Binning configuration.
    pub binning: BinningConfig,
    /// Histogram bins (backend-computed, deterministic).
    pub bins: Vec<HistogramBin>,
    /// Count of samples below the minimum bin edge.
    pub underflow_count: u64,
    /// Count of samples above the maximum bin edge.
    pub overflow_count: u64,
    /// Total number of windows included.
    pub total_samples: u64,
    /// Trust level of the run (structured DTO).
    pub trust_level: TrustLevelDto,
    /// Whether the run is trusted.
    pub is_trusted: bool,
}

impl WindowPnlHistogramResponse {
    /// Compute histogram from window PnL data.
    ///
    /// This computation is deterministic: given the same window PnL series,
    /// it always produces identical bins regardless of platform or locale.
    pub fn from_artifact(artifact: &RunArtifact, bin_count: usize) -> Option<Self> {
        let window_pnl = artifact.time_series.window_pnl.as_ref()?;
        
        if window_pnl.is_empty() {
            return None;
        }
        
        // Extract net PnL values
        let values: Vec<f64> = window_pnl.iter().map(|w| w.net_pnl).collect();
        
        // Compute min/max deterministically
        let min_val = values.iter().cloned().fold(f64::INFINITY, f64::min);
        let max_val = values.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        
        // Handle edge case where all values are the same
        let (bin_min, bin_max) = if (max_val - min_val).abs() < f64::EPSILON {
            (min_val - 1.0, max_val + 1.0)
        } else {
            // Add small padding to ensure all values fit in bins
            let range = max_val - min_val;
            let padding = range * 0.001; // 0.1% padding
            (min_val - padding, max_val + padding)
        };
        
        // Compute bin width
        let bin_count = bin_count.max(1).min(1000); // Clamp to reasonable range
        let bin_width = (bin_max - bin_min) / (bin_count as f64);
        
        // Initialize bins with explicit edges
        let mut bins: Vec<HistogramBin> = (0..bin_count)
            .map(|i| {
                let left = bin_min + (i as f64) * bin_width;
                let right = bin_min + ((i + 1) as f64) * bin_width;
                HistogramBin { left, right, count: 0 }
            })
            .collect();
        
        // Ensure last bin's right edge exactly equals bin_max (avoid floating point drift)
        if let Some(last) = bins.last_mut() {
            last.right = bin_max;
        }
        
        // Count samples into bins
        let mut underflow_count = 0u64;
        let mut overflow_count = 0u64;
        
        for &val in &values {
            if val < bin_min {
                underflow_count += 1;
            } else if val >= bin_max {
                // Include max in last bin
                if let Some(last) = bins.last_mut() {
                    last.count += 1;
                } else {
                    overflow_count += 1;
                }
            } else {
                // Compute bin index deterministically
                let idx = ((val - bin_min) / bin_width).floor() as usize;
                let idx = idx.min(bin_count - 1); // Clamp to last bin
                bins[idx].count += 1;
            }
        }
        
        Some(Self {
            schema_version: WINDOW_PNL_HISTOGRAM_SCHEMA_VERSION.to_string(),
            run_id: artifact.manifest.run_id.0.clone(),
            manifest_hash: artifact.manifest.manifest_hash.clone(),
            unit: "USD".to_string(),
            binning: BinningConfig {
                method: BinningMethod::FixedEdges,
                bin_count,
                min: bin_min,
                max: bin_max,
            },
            bins,
            underflow_count,
            overflow_count,
            total_samples: values.len() as u64,
            trust_level: artifact.manifest.trust_decision.trust_level.clone(),
            is_trusted: artifact.manifest.trust_decision.is_trusted,
        })
    }
    
    /// Validate that the histogram is internally consistent.
    pub fn validate(&self) -> Result<(), String> {
        // Check schema version
        if self.schema_version != WINDOW_PNL_HISTOGRAM_SCHEMA_VERSION {
            return Err(format!(
                "Unsupported schema version: {} (expected {})",
                self.schema_version, WINDOW_PNL_HISTOGRAM_SCHEMA_VERSION
            ));
        }
        
        // Check bin count matches
        if self.bins.len() != self.binning.bin_count {
            return Err(format!(
                "Bin count mismatch: {} bins but binning.bin_count = {}",
                self.bins.len(), self.binning.bin_count
            ));
        }
        
        // Check bins are contiguous and non-overlapping
        for window in self.bins.windows(2) {
            if (window[0].right - window[1].left).abs() > 1e-10 {
                return Err(format!(
                    "Bins are not contiguous: {} != {}",
                    window[0].right, window[1].left
                ));
            }
        }
        
        // Check total count matches
        let bin_sum: u64 = self.bins.iter().map(|b| b.count).sum();
        let expected_total = bin_sum + self.underflow_count + self.overflow_count;
        if expected_total != self.total_samples {
            return Err(format!(
                "Count mismatch: bins({}) + underflow({}) + overflow({}) = {} != total_samples({})",
                bin_sum, self.underflow_count, self.overflow_count, expected_total, self.total_samples
            ));
        }
        
        Ok(())
    }
}

// =============================================================================
// TESTS
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_run_id_from_fingerprint() {
        let fingerprint = RunFingerprint {
            version: "RUNFP_V2".to_string(),
            strategy: StrategyFingerprint::default(),
            code: crate::backtest_v2::fingerprint::CodeFingerprint::new(),
            config: crate::backtest_v2::fingerprint::ConfigFingerprint {
                settlement_reference_rule: None,
                settlement_tie_rule: None,
                chainlink_feed_id: None,
                oracle_chain_id: None,
                oracle_feed_proxies: vec![],
                oracle_decimals: vec![],
                oracle_visibility_rule: None,
                oracle_rounding_policy: None,
                oracle_config_hash: None,
                latency_model: "Fixed".to_string(),
                order_latency_ns: None,
                oms_parity_mode: "Full".to_string(),
                maker_fill_model: "Disabled".to_string(),
                integrity_policy: "Strict".to_string(),
                invariant_mode: "Hard".to_string(),
                fee_rate_bps: None,
                strategy_params_hash: 0,
                arrival_policy: "RecordedArrival".to_string(),
                strict_accounting: true,
                production_grade: true,
                allow_non_production: false,
                hash: 0x12345678,
            },
            dataset: crate::backtest_v2::fingerprint::DatasetFingerprint {
                classification: "FullIncremental".to_string(),
                readiness: "MakerViable".to_string(),
                orderbook_type: "FullIncrementalL2DeltasWithExchangeSeq".to_string(),
                trade_type: "TradePrints".to_string(),
                arrival_semantics: "RecordedArrival".to_string(),
                streams: vec![],
                hash: 0xABCD,
            },
            seed: crate::backtest_v2::fingerprint::SeedFingerprint::new(42),
            behavior: crate::backtest_v2::fingerprint::BehaviorFingerprint {
                event_count: 100,
                hash: 0xDEAD,
            },
            registry: None,
            hash: 0x123456789ABCDEF0,
            hash_hex: "123456789abcdef0".to_string(),
        };
        
        let run_id = RunId::from_fingerprint(&fingerprint);
        assert_eq!(run_id.as_str(), "run_123456789abcdef0");
    }
    
    #[test]
    fn test_disclaimer_generation() {
        let mut results = BacktestResults::default();
        results.truthfulness.verdict = TrustVerdict::Untrusted;
        results.truthfulness.untrusted_reasons = vec!["Test failure".to_string()];
        
        let config = BacktestConfig::default();
        
        let disclaimers = generate_disclaimers(&results, &config);
        
        // Should have at least the standard disclaimer
        assert!(!disclaimers.is_empty());
        assert!(disclaimers.iter().any(|d| d.code == "BACKTEST_DISCLAIMER"));
    }
    
    #[test]
    fn test_artifact_response_etag() {
        let manifest = RunManifest {
            schema_version: 1,
            run_id: RunId("run_test".to_string()),
            persisted_at: 1234567890,
            fingerprint: RunFingerprint {
                version: "RUNFP_V2".to_string(),
                strategy: StrategyFingerprint::default(),
                code: crate::backtest_v2::fingerprint::CodeFingerprint::new(),
                config: crate::backtest_v2::fingerprint::ConfigFingerprint {
                    settlement_reference_rule: None,
                    settlement_tie_rule: None,
                    chainlink_feed_id: None,
                    oracle_chain_id: None,
                    oracle_feed_proxies: vec![],
                    oracle_decimals: vec![],
                    oracle_visibility_rule: None,
                    oracle_rounding_policy: None,
                    oracle_config_hash: None,
                    latency_model: "Fixed".to_string(),
                    order_latency_ns: None,
                    oms_parity_mode: "Full".to_string(),
                    maker_fill_model: "Disabled".to_string(),
                    integrity_policy: "Strict".to_string(),
                    invariant_mode: "Hard".to_string(),
                    fee_rate_bps: None,
                    strategy_params_hash: 0,
                    arrival_policy: "RecordedArrival".to_string(),
                    strict_accounting: true,
                    production_grade: true,
                    allow_non_production: false,
                    hash: 0,
                },
                dataset: crate::backtest_v2::fingerprint::DatasetFingerprint {
                    classification: "FullIncremental".to_string(),
                    readiness: "MakerViable".to_string(),
                    orderbook_type: "FullIncrementalL2DeltasWithExchangeSeq".to_string(),
                    trade_type: "TradePrints".to_string(),
                    arrival_semantics: "RecordedArrival".to_string(),
                    streams: vec![],
                    hash: 0,
                },
                seed: crate::backtest_v2::fingerprint::SeedFingerprint::new(42),
                behavior: crate::backtest_v2::fingerprint::BehaviorFingerprint {
                    event_count: 0,
                    hash: 0,
                },
                registry: None,
                hash: 0,
                hash_hex: "0000000000000000".to_string(),
            },
            strategy: StrategyIdentity {
                name: "test".to_string(),
                version: "1.0.0".to_string(),
                code_hash: None,
            },
            dataset: DatasetMetadata {
                classification: "FullIncremental".to_string(),
                readiness: "MakerViable".to_string(),
                events_processed: 0,
                delta_events_processed: 0,
                time_range: TimeRangeSummary {
                    start_ns: 0,
                    end_ns: 0,
                    duration_ns: 0,
                },
            },
            config_summary: ConfigSummary {
                production_grade: true,
                strict_mode: true,
                strict_accounting: true,
                maker_fill_model: "Disabled".to_string(),
                oms_parity_mode: "Full".to_string(),
                seed: 42,
            },
            trust_decision: TrustDecisionSummary {
                verdict: "Trusted".to_string(),
                trust_level: TrustLevelDto {
                    status: TrustStatus::Trusted,
                    reasons: vec![],
                },
                is_trusted: true,
                failure_reasons: vec![],
            },
            disclaimers: vec![],
            methodology_capsule: MethodologyCapsule {
                version: "v1".to_string(),
                summary: "Test capsule".to_string(),
                details: vec![],
                input_hash: "0".to_string(),
            },
            manifest_hash: "abcd1234".to_string(),
        };
        
        let artifact = RunArtifact {
            manifest,
            results: BacktestResults::default(),
            time_series: RunTimeSeries {
                equity_curve: None,
                drawdown_series: None,
                window_pnl: None,
                pnl_history: vec![],
            },
            distributions: RunDistributions {
                trade_pnl_bins: vec![],
                trade_size_bins: vec![],
                hold_time_bins: vec![],
                slippage_bins: vec![],
            },
        };
        
        let response: ArtifactResponse<()> = ArtifactResponse::new(&artifact, ());
        assert_eq!(response.etag(), "\"abcd1234\"");
    }

    // =========================================================================
    // WINDOW PNL HISTOGRAM TESTS
    // =========================================================================

    fn make_test_artifact_with_window_pnl(pnl_values: Vec<f64>) -> RunArtifact {
        let window_pnl: Vec<WindowPnLPoint> = pnl_values
            .iter()
            .enumerate()
            .map(|(i, &pnl)| WindowPnLPoint {
                window_start_ns: (i as i64) * 900_000_000_000,
                window_end_ns: ((i as i64) + 1) * 900_000_000_000,
                market_id: format!("btc-updown-15m-{}", i),
                gross_pnl: pnl,
                fees: 0.1,
                net_pnl: pnl,
                trades_count: 1,
                outcome: "Up".to_string(),
            })
            .collect();

        let manifest = RunManifest {
            schema_version: 1,
            run_id: RunId("run_test_histogram".to_string()),
            persisted_at: 1234567890,
            fingerprint: RunFingerprint {
                version: "RUNFP_V2".to_string(),
                strategy: StrategyFingerprint::default(),
                code: crate::backtest_v2::fingerprint::CodeFingerprint::new(),
                config: crate::backtest_v2::fingerprint::ConfigFingerprint {
                    settlement_reference_rule: None,
                    settlement_tie_rule: None,
                    chainlink_feed_id: None,
                    oracle_chain_id: None,
                    oracle_feed_proxies: vec![],
                    oracle_decimals: vec![],
                    oracle_visibility_rule: None,
                    oracle_rounding_policy: None,
                    oracle_config_hash: None,
                    latency_model: "Fixed".to_string(),
                    order_latency_ns: None,
                    oms_parity_mode: "Full".to_string(),
                    maker_fill_model: "Disabled".to_string(),
                    integrity_policy: "Strict".to_string(),
                    invariant_mode: "Hard".to_string(),
                    fee_rate_bps: None,
                    strategy_params_hash: 0,
                    arrival_policy: "RecordedArrival".to_string(),
                    strict_accounting: true,
                    production_grade: true,
                    allow_non_production: false,
                    hash: 0,
                },
                dataset: crate::backtest_v2::fingerprint::DatasetFingerprint {
                    classification: "FullIncremental".to_string(),
                    readiness: "MakerViable".to_string(),
                    orderbook_type: "FullIncrementalL2DeltasWithExchangeSeq".to_string(),
                    trade_type: "TradePrints".to_string(),
                    arrival_semantics: "RecordedArrival".to_string(),
                    streams: vec![],
                    hash: 0,
                },
                seed: crate::backtest_v2::fingerprint::SeedFingerprint::new(42),
                behavior: crate::backtest_v2::fingerprint::BehaviorFingerprint {
                    event_count: 0,
                    hash: 0,
                },
                registry: None,
                hash: 0,
                hash_hex: "deadbeef12345678".to_string(),
            },
            strategy: StrategyIdentity {
                name: "test".to_string(),
                version: "1.0.0".to_string(),
                code_hash: None,
            },
            dataset: DatasetMetadata {
                classification: "FullIncremental".to_string(),
                readiness: "MakerViable".to_string(),
                events_processed: 0,
                delta_events_processed: 0,
                time_range: TimeRangeSummary {
                    start_ns: 0,
                    end_ns: 0,
                    duration_ns: 0,
                },
            },
            config_summary: ConfigSummary {
                production_grade: true,
                strict_mode: true,
                strict_accounting: true,
                maker_fill_model: "Disabled".to_string(),
                oms_parity_mode: "Full".to_string(),
                seed: 42,
            },
            trust_decision: TrustDecisionSummary {
                verdict: "Trusted".to_string(),
                trust_level: TrustLevelDto {
                    status: TrustStatus::Trusted,
                    reasons: vec![],
                },
                is_trusted: true,
                failure_reasons: vec![],
            },
            disclaimers: vec![],
            methodology_capsule: MethodologyCapsule {
                version: "v1".to_string(),
                summary: "Test capsule".to_string(),
                details: vec![],
                input_hash: "0".to_string(),
            },
            manifest_hash: "testhash123".to_string(),
        };

        RunArtifact {
            manifest,
            results: BacktestResults::default(),
            time_series: RunTimeSeries {
                equity_curve: None,
                drawdown_series: None,
                window_pnl: Some(window_pnl),
                pnl_history: vec![],
            },
            distributions: RunDistributions {
                trade_pnl_bins: vec![],
                trade_size_bins: vec![],
                hold_time_bins: vec![],
                slippage_bins: vec![],
            },
        }
    }

    #[test]
    fn test_histogram_deterministic_binning() {
        // Given the same input, histogram should be identical
        let pnl_values = vec![-10.0, -5.0, 0.0, 5.0, 10.0, 15.0, 20.0, -3.0, 7.0, 12.0];
        let artifact = make_test_artifact_with_window_pnl(pnl_values.clone());
        
        let hist1 = WindowPnlHistogramResponse::from_artifact(&artifact, 10).unwrap();
        let hist2 = WindowPnlHistogramResponse::from_artifact(&artifact, 10).unwrap();
        
        // Should be identical
        assert_eq!(hist1.bins, hist2.bins);
        assert_eq!(hist1.binning.min, hist2.binning.min);
        assert_eq!(hist1.binning.max, hist2.binning.max);
        assert_eq!(hist1.total_samples, hist2.total_samples);
    }

    #[test]
    fn test_histogram_bin_count_matches() {
        let pnl_values = vec![-10.0, -5.0, 0.0, 5.0, 10.0];
        let artifact = make_test_artifact_with_window_pnl(pnl_values);
        
        for bin_count in [5, 10, 20, 50, 100] {
            let hist = WindowPnlHistogramResponse::from_artifact(&artifact, bin_count).unwrap();
            assert_eq!(hist.bins.len(), bin_count, "bin_count={}", bin_count);
            assert_eq!(hist.binning.bin_count, bin_count);
        }
    }

    #[test]
    fn test_histogram_bins_are_contiguous() {
        let pnl_values: Vec<f64> = (-50..50).map(|i| i as f64).collect();
        let artifact = make_test_artifact_with_window_pnl(pnl_values);
        
        let hist = WindowPnlHistogramResponse::from_artifact(&artifact, 20).unwrap();
        
        // Each bin's right edge should equal next bin's left edge
        for i in 0..hist.bins.len() - 1 {
            let diff = (hist.bins[i].right - hist.bins[i + 1].left).abs();
            assert!(diff < 1e-10, "Bins {} and {} are not contiguous: {} != {}", 
                i, i + 1, hist.bins[i].right, hist.bins[i + 1].left);
        }
    }

    #[test]
    fn test_histogram_total_count_matches() {
        let pnl_values: Vec<f64> = (0..100).map(|i| (i as f64) - 50.0).collect();
        let artifact = make_test_artifact_with_window_pnl(pnl_values.clone());
        
        let hist = WindowPnlHistogramResponse::from_artifact(&artifact, 25).unwrap();
        
        let bin_sum: u64 = hist.bins.iter().map(|b| b.count).sum();
        let total = bin_sum + hist.underflow_count + hist.overflow_count;
        
        assert_eq!(total, hist.total_samples);
        assert_eq!(hist.total_samples, pnl_values.len() as u64);
    }

    #[test]
    fn test_histogram_validates_correctly() {
        let pnl_values = vec![1.0, 2.0, 3.0, 4.0, 5.0];
        let artifact = make_test_artifact_with_window_pnl(pnl_values);
        
        let hist = WindowPnlHistogramResponse::from_artifact(&artifact, 5).unwrap();
        
        // Should pass validation
        assert!(hist.validate().is_ok(), "Valid histogram should pass validation");
    }

    #[test]
    fn test_histogram_schema_version() {
        let pnl_values = vec![1.0, 2.0, 3.0];
        let artifact = make_test_artifact_with_window_pnl(pnl_values);
        
        let hist = WindowPnlHistogramResponse::from_artifact(&artifact, 5).unwrap();
        
        assert_eq!(hist.schema_version, WINDOW_PNL_HISTOGRAM_SCHEMA_VERSION);
        assert_eq!(hist.schema_version, "v1");
    }

    #[test]
    fn test_histogram_manifest_hash_included() {
        let pnl_values = vec![1.0, 2.0, 3.0];
        let artifact = make_test_artifact_with_window_pnl(pnl_values);
        
        let hist = WindowPnlHistogramResponse::from_artifact(&artifact, 5).unwrap();
        
        assert_eq!(hist.manifest_hash, "testhash123");
        assert_eq!(hist.run_id, "run_test_histogram");
    }

    #[test]
    fn test_histogram_empty_window_pnl_returns_none() {
        let artifact = make_test_artifact_with_window_pnl(vec![]);
        
        let hist = WindowPnlHistogramResponse::from_artifact(&artifact, 10);
        
        assert!(hist.is_none(), "Empty window PnL should return None");
    }

    #[test]
    fn test_histogram_single_value() {
        // Edge case: single value should still produce valid histogram
        let pnl_values = vec![42.0];
        let artifact = make_test_artifact_with_window_pnl(pnl_values);
        
        let hist = WindowPnlHistogramResponse::from_artifact(&artifact, 5).unwrap();
        
        assert_eq!(hist.total_samples, 1);
        assert!(hist.validate().is_ok());
        
        // One bin should have count 1
        let total_count: u64 = hist.bins.iter().map(|b| b.count).sum();
        assert_eq!(total_count, 1);
    }

    #[test]
    fn test_histogram_identical_values() {
        // Edge case: all values the same
        let pnl_values = vec![10.0, 10.0, 10.0, 10.0, 10.0];
        let artifact = make_test_artifact_with_window_pnl(pnl_values);
        
        let hist = WindowPnlHistogramResponse::from_artifact(&artifact, 10).unwrap();
        
        assert_eq!(hist.total_samples, 5);
        assert!(hist.validate().is_ok());
        
        // All values should be in bins (not overflow/underflow)
        let bin_count: u64 = hist.bins.iter().map(|b| b.count).sum();
        assert_eq!(bin_count, 5);
    }

    #[test]
    fn test_histogram_negative_values_only() {
        let pnl_values = vec![-100.0, -50.0, -25.0, -10.0, -5.0];
        let artifact = make_test_artifact_with_window_pnl(pnl_values);
        
        let hist = WindowPnlHistogramResponse::from_artifact(&artifact, 10).unwrap();
        
        assert!(hist.binning.min < 0.0);
        assert!(hist.binning.max < 0.0);
        assert!(hist.validate().is_ok());
    }

    #[test]
    fn test_histogram_validation_rejects_bad_schema() {
        let pnl_values = vec![1.0, 2.0, 3.0];
        let artifact = make_test_artifact_with_window_pnl(pnl_values);
        
        let mut hist = WindowPnlHistogramResponse::from_artifact(&artifact, 5).unwrap();
        hist.schema_version = "v999".to_string();
        
        let result = hist.validate();
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Unsupported schema version"));
    }

    #[test]
    fn test_histogram_validation_rejects_bin_count_mismatch() {
        let pnl_values = vec![1.0, 2.0, 3.0];
        let artifact = make_test_artifact_with_window_pnl(pnl_values);
        
        let mut hist = WindowPnlHistogramResponse::from_artifact(&artifact, 5).unwrap();
        hist.binning.bin_count = 999; // Mismatch
        
        let result = hist.validate();
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Bin count mismatch"));
    }

    #[test]
    fn test_histogram_validation_rejects_count_mismatch() {
        let pnl_values = vec![1.0, 2.0, 3.0];
        let artifact = make_test_artifact_with_window_pnl(pnl_values);
        
        let mut hist = WindowPnlHistogramResponse::from_artifact(&artifact, 5).unwrap();
        hist.total_samples = 999; // Wrong total
        
        let result = hist.validate();
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Count mismatch"));
    }
}

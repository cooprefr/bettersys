//! Maker Validation Ladder
//!
//! Validates maker strategies under progressively relaxed execution assumptions.
//! Strategies must survive conservative assumptions before being trusted under
//! more realistic conditions.
//!
//! # Validation Ladder (Strictest → Most Realistic)
//!
//! 1. **ConservativeMaker** - Worst-case assumptions
//!    - Pessimistic queue model (full visible size ahead)
//!    - Worst-case cancel latency (p99)
//!    - FIFO fill priority
//!    - No hidden liquidity
//!
//! 2. **NeutralMaker** - Explicit tracking, no optimism
//!    - Explicit queue tracking from deltas
//!    - Fixed conservative cancel latency
//!    - Strict FIFO
//!
//! 3. **MeasuredLiveMaker** - Empirical parameters
//!    - Explicit queue model
//!    - Sampled from live latency distributions
//!    - Based on measured telemetry
//!
//! # Survival Criteria
//!
//! - Strategy must maintain PnL >= threshold under ConservativeMaker
//! - Degradation between profiles must be bounded and explainable
//! - Catastrophic sign flips are flagged as fragile

use crate::backtest_v2::clock::Nanos;
use crate::backtest_v2::latency::{LatencyConfig, LatencyDistribution, NS_PER_MS};
use crate::backtest_v2::orchestrator::MakerFillModel;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// =============================================================================
// MAKER EXECUTION PROFILE
// =============================================================================

/// Maker execution profile - ordered from strictest to most realistic.
/// 
/// Profiles are explicitly ordered:
/// `ConservativeMaker < NeutralMaker < MeasuredLiveMaker`
/// 
/// Validation MUST start from ConservativeMaker and progress forward.
/// A strategy cannot claim maker viability without passing Conservative.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
pub enum MakerExecutionProfile {
    /// Strictest assumptions - pessimistic queue, worst-case latency.
    /// If profitable here, likely robust in production.
    ConservativeMaker = 0,
    
    /// Neutral assumptions - explicit tracking, no optimism or pessimism.
    /// Standard baseline for comparison.
    NeutralMaker = 1,
    
    /// Measured live parameters - derived from actual venue telemetry.
    /// Closest to real execution conditions.
    MeasuredLiveMaker = 2,
}

impl MakerExecutionProfile {
    /// All profiles in validation order (strictest first).
    pub fn all_ordered() -> &'static [MakerExecutionProfile] {
        &[
            MakerExecutionProfile::ConservativeMaker,
            MakerExecutionProfile::NeutralMaker,
            MakerExecutionProfile::MeasuredLiveMaker,
        ]
    }
    
    /// Human-readable description.
    pub fn description(&self) -> &'static str {
        match self {
            Self::ConservativeMaker => "CONSERVATIVE: Pessimistic queue + worst-case latency",
            Self::NeutralMaker => "NEUTRAL: Explicit tracking + conservative latency",
            Self::MeasuredLiveMaker => "MEASURED-LIVE: Empirical latency distributions",
        }
    }
    
    /// Short name for reporting.
    pub fn short_name(&self) -> &'static str {
        match self {
            Self::ConservativeMaker => "Conservative",
            Self::NeutralMaker => "Neutral",
            Self::MeasuredLiveMaker => "MeasuredLive",
        }
    }
    
    /// Whether this profile is valid for production deployment decisions.
    /// Only profiles that have passed ConservativeMaker validation can be trusted.
    pub fn is_production_valid(&self) -> bool {
        // All profiles are valid IF the strategy passed ConservativeMaker first.
        // The validation runner enforces this ordering.
        true
    }
    
    /// Check if this profile is stricter than another.
    pub fn is_stricter_than(&self, other: MakerExecutionProfile) -> bool {
        (*self as u8) < (other as u8)
    }
}

impl std::fmt::Display for MakerExecutionProfile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.description())
    }
}

// =============================================================================
// PROFILE CONFIGURATION
// =============================================================================

/// Configuration for ConservativeMaker profile.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConservativeConfig {
    /// Extra queue ahead percentage (assume more orders ahead than visible).
    /// Default: 100% (assume double the visible queue).
    pub extra_queue_ahead_pct: f64,
    
    /// Order latency (strategy → venue) in milliseconds.
    /// Default: 150ms (worst-case network + processing).
    pub order_latency_ms: f64,
    
    /// Cancel latency in milliseconds.
    /// Default: 200ms (worst-case p99).
    pub cancel_latency_ms: f64,
    
    /// Fill report latency in milliseconds.
    /// Default: 100ms.
    pub fill_report_latency_ms: f64,
    
    /// Minimum queue consumption required before fill.
    /// If true, require queue to fully clear at price level.
    pub require_level_clear: bool,
    
    /// No partial fills allowed (all-or-nothing).
    pub no_partial_fills: bool,
}

impl Default for ConservativeConfig {
    fn default() -> Self {
        Self {
            extra_queue_ahead_pct: 100.0,  // Assume 2x visible queue
            order_latency_ms: 150.0,
            cancel_latency_ms: 200.0,
            fill_report_latency_ms: 100.0,
            require_level_clear: false,  // Not full level clear, but pessimistic queue
            no_partial_fills: false,
        }
    }
}

impl ConservativeConfig {
    /// Very conservative settings for fragility testing.
    pub fn very_conservative() -> Self {
        Self {
            extra_queue_ahead_pct: 200.0,  // 3x visible queue
            order_latency_ms: 250.0,
            cancel_latency_ms: 300.0,
            fill_report_latency_ms: 150.0,
            require_level_clear: true,
            no_partial_fills: true,
        }
    }
}

/// Configuration for NeutralMaker profile.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NeutralConfig {
    /// Order latency in milliseconds.
    pub order_latency_ms: f64,
    
    /// Cancel latency in milliseconds.
    pub cancel_latency_ms: f64,
    
    /// Fill report latency in milliseconds.
    pub fill_report_latency_ms: f64,
    
    /// Use explicit FIFO queue tracking.
    pub explicit_queue: bool,
}

impl Default for NeutralConfig {
    fn default() -> Self {
        Self {
            order_latency_ms: 75.0,
            cancel_latency_ms: 100.0,
            fill_report_latency_ms: 50.0,
            explicit_queue: true,
        }
    }
}

/// Configuration for MeasuredLiveMaker profile.
/// Parameters derived from live telemetry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeasuredLiveConfig {
    /// Order latency mean (ms).
    pub order_latency_mean_ms: f64,
    /// Order latency std dev (ms).
    pub order_latency_std_ms: f64,
    /// Order latency p99 (ms) - used for outlier capping.
    pub order_latency_p99_ms: f64,
    
    /// Cancel latency mean (ms).
    pub cancel_latency_mean_ms: f64,
    /// Cancel latency std dev (ms).
    pub cancel_latency_std_ms: f64,
    
    /// Fill report latency mean (ms).
    pub fill_report_latency_mean_ms: f64,
    
    /// Use stochastic latency sampling.
    pub use_stochastic_latency: bool,
}

impl Default for MeasuredLiveConfig {
    fn default() -> Self {
        // Default values based on typical Polymarket measurements
        Self {
            order_latency_mean_ms: 35.0,
            order_latency_std_ms: 15.0,
            order_latency_p99_ms: 100.0,
            cancel_latency_mean_ms: 45.0,
            cancel_latency_std_ms: 20.0,
            fill_report_latency_mean_ms: 25.0,
            use_stochastic_latency: true,
        }
    }
}

impl MeasuredLiveConfig {
    /// Create from measured live statistics.
    pub fn from_measurements(
        order_mean_ms: f64,
        order_std_ms: f64,
        order_p99_ms: f64,
        cancel_mean_ms: f64,
        cancel_std_ms: f64,
        fill_report_mean_ms: f64,
    ) -> Self {
        Self {
            order_latency_mean_ms: order_mean_ms,
            order_latency_std_ms: order_std_ms,
            order_latency_p99_ms: order_p99_ms,
            cancel_latency_mean_ms: cancel_mean_ms,
            cancel_latency_std_ms: cancel_std_ms,
            fill_report_latency_mean_ms: fill_report_mean_ms,
            use_stochastic_latency: true,
        }
    }
}

/// Complete profile configuration set.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MakerProfileConfigs {
    pub conservative: ConservativeConfig,
    pub neutral: NeutralConfig,
    pub measured_live: MeasuredLiveConfig,
}

impl Default for MakerProfileConfigs {
    fn default() -> Self {
        Self {
            conservative: ConservativeConfig::default(),
            neutral: NeutralConfig::default(),
            measured_live: MeasuredLiveConfig::default(),
        }
    }
}

impl MakerProfileConfigs {
    /// Get latency config for a profile.
    pub fn latency_config_for(&self, profile: MakerExecutionProfile) -> LatencyConfig {
        match profile {
            MakerExecutionProfile::ConservativeMaker => {
                let c = &self.conservative;
                LatencyConfig {
                    market_data: LatencyDistribution::Fixed { 
                        latency_ns: (c.fill_report_latency_ms * NS_PER_MS as f64) as Nanos 
                    },
                    decision: LatencyDistribution::Fixed { latency_ns: 0 },
                    order_send: LatencyDistribution::Fixed { 
                        latency_ns: (c.order_latency_ms * NS_PER_MS as f64) as Nanos 
                    },
                    venue_process: LatencyDistribution::Fixed { 
                        latency_ns: (10.0 * NS_PER_MS as f64) as Nanos  // Assume 10ms venue
                    },
                    cancel_process: LatencyDistribution::Fixed { 
                        latency_ns: (c.cancel_latency_ms * NS_PER_MS as f64) as Nanos 
                    },
                    fill_report: LatencyDistribution::Fixed { 
                        latency_ns: (c.fill_report_latency_ms * NS_PER_MS as f64) as Nanos 
                    },
                }
            }
            MakerExecutionProfile::NeutralMaker => {
                let n = &self.neutral;
                LatencyConfig {
                    market_data: LatencyDistribution::Fixed { 
                        latency_ns: (n.fill_report_latency_ms * NS_PER_MS as f64) as Nanos 
                    },
                    decision: LatencyDistribution::Fixed { latency_ns: 0 },
                    order_send: LatencyDistribution::Fixed { 
                        latency_ns: (n.order_latency_ms * NS_PER_MS as f64) as Nanos 
                    },
                    venue_process: LatencyDistribution::Fixed { 
                        latency_ns: (5.0 * NS_PER_MS as f64) as Nanos 
                    },
                    cancel_process: LatencyDistribution::Fixed { 
                        latency_ns: (n.cancel_latency_ms * NS_PER_MS as f64) as Nanos 
                    },
                    fill_report: LatencyDistribution::Fixed { 
                        latency_ns: (n.fill_report_latency_ms * NS_PER_MS as f64) as Nanos 
                    },
                }
            }
            MakerExecutionProfile::MeasuredLiveMaker => {
                let m = &self.measured_live;
                if m.use_stochastic_latency {
                    LatencyConfig {
                        market_data: LatencyDistribution::Normal {
                            mean_ns: (m.fill_report_latency_mean_ms * NS_PER_MS as f64) as Nanos,
                            std_ns: (m.fill_report_latency_mean_ms * 0.3 * NS_PER_MS as f64) as Nanos,
                            max_ns: (m.fill_report_latency_mean_ms * 3.0 * NS_PER_MS as f64) as Nanos,
                        },
                        decision: LatencyDistribution::Fixed { latency_ns: 0 },
                        order_send: LatencyDistribution::Normal {
                            mean_ns: (m.order_latency_mean_ms * NS_PER_MS as f64) as Nanos,
                            std_ns: (m.order_latency_std_ms * NS_PER_MS as f64) as Nanos,
                            max_ns: (m.order_latency_p99_ms * NS_PER_MS as f64) as Nanos,
                        },
                        venue_process: LatencyDistribution::Fixed { 
                            latency_ns: (3.0 * NS_PER_MS as f64) as Nanos 
                        },
                        cancel_process: LatencyDistribution::Normal {
                            mean_ns: (m.cancel_latency_mean_ms * NS_PER_MS as f64) as Nanos,
                            std_ns: (m.cancel_latency_std_ms * NS_PER_MS as f64) as Nanos,
                            max_ns: (m.cancel_latency_mean_ms * 3.0 * NS_PER_MS as f64) as Nanos,
                        },
                        fill_report: LatencyDistribution::Normal {
                            mean_ns: (m.fill_report_latency_mean_ms * NS_PER_MS as f64) as Nanos,
                            std_ns: (m.fill_report_latency_mean_ms * 0.3 * NS_PER_MS as f64) as Nanos,
                            max_ns: (m.fill_report_latency_mean_ms * 3.0 * NS_PER_MS as f64) as Nanos,
                        },
                    }
                } else {
                    // Fixed mean values
                    LatencyConfig {
                        market_data: LatencyDistribution::Fixed { 
                            latency_ns: (m.fill_report_latency_mean_ms * NS_PER_MS as f64) as Nanos 
                        },
                        decision: LatencyDistribution::Fixed { latency_ns: 0 },
                        order_send: LatencyDistribution::Fixed { 
                            latency_ns: (m.order_latency_mean_ms * NS_PER_MS as f64) as Nanos 
                        },
                        venue_process: LatencyDistribution::Fixed { 
                            latency_ns: (3.0 * NS_PER_MS as f64) as Nanos 
                        },
                        cancel_process: LatencyDistribution::Fixed { 
                            latency_ns: (m.cancel_latency_mean_ms * NS_PER_MS as f64) as Nanos 
                        },
                        fill_report: LatencyDistribution::Fixed { 
                            latency_ns: (m.fill_report_latency_mean_ms * NS_PER_MS as f64) as Nanos 
                        },
                    }
                }
            }
        }
    }
    
    /// Get queue model adjustment for a profile.
    /// Returns extra queue ahead multiplier (1.0 = no adjustment).
    pub fn queue_ahead_multiplier(&self, profile: MakerExecutionProfile) -> f64 {
        match profile {
            MakerExecutionProfile::ConservativeMaker => {
                1.0 + (self.conservative.extra_queue_ahead_pct / 100.0)
            }
            MakerExecutionProfile::NeutralMaker => 1.0,
            MakerExecutionProfile::MeasuredLiveMaker => 1.0,
        }
    }
}

// =============================================================================
// SURVIVAL CRITERIA
// =============================================================================

/// Survival criteria for maker validation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MakerSurvivalCriteria {
    /// Minimum PnL under ConservativeMaker to pass.
    /// Default: 0.0 (must not lose money).
    pub min_conservative_pnl: f64,
    
    /// Maximum allowed PnL degradation from Conservative → Neutral (%).
    /// If PnL improves when relaxing assumptions, strategy is suspect.
    /// Degradation beyond this indicates fragility.
    pub max_conservative_to_neutral_improvement_pct: f64,
    
    /// Maximum allowed PnL degradation from Neutral → MeasuredLive (%).
    pub max_neutral_to_live_degradation_pct: f64,
    
    /// Minimum maker fill rate under ConservativeMaker.
    /// If fills drop to zero, strategy cannot be validated as maker.
    pub min_conservative_maker_fill_rate: f64,
    
    /// Maximum drawdown under ConservativeMaker.
    pub max_conservative_drawdown_pct: f64,
    
    /// Require profitable (PnL > 0) under all profiles.
    pub require_profitable_all_profiles: bool,
    
    /// Allow sign flip between profiles.
    /// If false, going from positive to negative PnL is a failure.
    pub allow_sign_flip: bool,
}

impl Default for MakerSurvivalCriteria {
    fn default() -> Self {
        Self {
            min_conservative_pnl: 0.0,
            max_conservative_to_neutral_improvement_pct: 200.0,  // Suspicious if > 2x improvement
            max_neutral_to_live_degradation_pct: 50.0,  // Allow 50% degradation
            min_conservative_maker_fill_rate: 0.01,  // At least 1% maker fills
            max_conservative_drawdown_pct: 50.0,
            require_profitable_all_profiles: false,
            allow_sign_flip: false,
        }
    }
}

impl MakerSurvivalCriteria {
    /// Strict criteria for production deployment.
    pub fn strict() -> Self {
        Self {
            min_conservative_pnl: 0.0,
            max_conservative_to_neutral_improvement_pct: 100.0,  // Max 2x
            max_neutral_to_live_degradation_pct: 30.0,
            min_conservative_maker_fill_rate: 0.05,
            max_conservative_drawdown_pct: 30.0,
            require_profitable_all_profiles: true,
            allow_sign_flip: false,
        }
    }
    
    /// Lenient criteria for research exploration.
    pub fn lenient() -> Self {
        Self {
            min_conservative_pnl: -1000.0,  // Allow small loss
            max_conservative_to_neutral_improvement_pct: 500.0,
            max_neutral_to_live_degradation_pct: 75.0,
            min_conservative_maker_fill_rate: 0.0,
            max_conservative_drawdown_pct: 75.0,
            require_profitable_all_profiles: false,
            allow_sign_flip: true,
        }
    }
}

// =============================================================================
// VALIDATION METRICS PER PROFILE
// =============================================================================

/// Metrics collected for each execution profile.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProfileMetrics {
    /// Profile tested.
    pub profile: Option<MakerExecutionProfile>,
    /// Net PnL.
    pub net_pnl: f64,
    /// PnL from maker fills only.
    pub maker_pnl: f64,
    /// PnL from taker fills only.
    pub taker_pnl: f64,
    /// Total fills.
    pub total_fills: u64,
    /// Maker fills.
    pub maker_fills: u64,
    /// Taker fills.
    pub taker_fills: u64,
    /// Maker fill rate (maker_fills / total submitted maker orders).
    pub maker_fill_rate: f64,
    /// Cancel-fill race losses (fills lost to cancel race).
    pub cancel_fill_race_losses: u64,
    /// Maximum drawdown.
    pub max_drawdown: f64,
    /// Trade count.
    pub trade_count: u64,
    /// Total volume.
    pub total_volume: f64,
    /// Total fees.
    pub total_fees: f64,
    /// Sharpe ratio.
    pub sharpe_ratio: Option<f64>,
    /// Run duration (ns).
    pub duration_ns: Nanos,
}

impl ProfileMetrics {
    /// Check if profitable.
    pub fn is_profitable(&self) -> bool {
        self.net_pnl > 0.0
    }
    
    /// Check if has any maker fills.
    pub fn has_maker_fills(&self) -> bool {
        self.maker_fills > 0
    }
    
    /// PnL change from another profile (%).
    pub fn pnl_change_pct_from(&self, other: &ProfileMetrics) -> f64 {
        if other.net_pnl.abs() < 1e-9 {
            if self.net_pnl.abs() < 1e-9 {
                0.0
            } else if self.net_pnl > 0.0 {
                f64::INFINITY
            } else {
                f64::NEG_INFINITY
            }
        } else {
            (self.net_pnl - other.net_pnl) / other.net_pnl.abs() * 100.0
        }
    }
}

// =============================================================================
// MAKER FRAGILITY FLAGS
// =============================================================================

/// Maker-specific fragility flags.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MakerFragilityFlags {
    /// Strategy fails under conservative assumptions.
    pub fragile_conservative: bool,
    /// Reason for conservative fragility.
    pub conservative_reason: Option<String>,
    
    /// Strategy is sensitive to latency assumptions.
    pub fragile_latency: bool,
    /// Reason for latency fragility.
    pub latency_reason: Option<String>,
    
    /// Strategy depends on queue position optimism.
    pub fragile_queue: bool,
    /// Reason for queue fragility.
    pub queue_reason: Option<String>,
    
    /// Strategy depends on cancel timing.
    pub fragile_cancel: bool,
    /// Reason for cancel fragility.
    pub cancel_reason: Option<String>,
    
    /// PnL sign flips between profiles (highly suspect).
    pub sign_flip_detected: bool,
    /// Profiles between which sign flip occurred.
    pub sign_flip_profiles: Option<(MakerExecutionProfile, MakerExecutionProfile)>,
    
    /// Relaxing assumptions improves results (optimism artifact).
    pub improves_when_relaxed: bool,
    /// Improvement ratio when relaxing assumptions.
    pub relaxation_improvement_ratio: Option<f64>,
}

impl MakerFragilityFlags {
    /// Check if any fragility detected.
    pub fn is_fragile(&self) -> bool {
        self.fragile_conservative
            || self.fragile_latency
            || self.fragile_queue
            || self.fragile_cancel
            || self.sign_flip_detected
            || self.improves_when_relaxed
    }
    
    /// Get all fragility reasons.
    pub fn all_reasons(&self) -> Vec<String> {
        let mut reasons = Vec::new();
        if let Some(ref r) = self.conservative_reason {
            reasons.push(format!("Conservative: {}", r));
        }
        if let Some(ref r) = self.latency_reason {
            reasons.push(format!("Latency: {}", r));
        }
        if let Some(ref r) = self.queue_reason {
            reasons.push(format!("Queue: {}", r));
        }
        if let Some(ref r) = self.cancel_reason {
            reasons.push(format!("Cancel: {}", r));
        }
        if self.sign_flip_detected {
            if let Some((from, to)) = self.sign_flip_profiles {
                reasons.push(format!(
                    "Sign flip: {} → {}",
                    from.short_name(),
                    to.short_name()
                ));
            } else {
                reasons.push("Sign flip detected".to_string());
            }
        }
        if self.improves_when_relaxed {
            if let Some(ratio) = self.relaxation_improvement_ratio {
                reasons.push(format!(
                    "Suspiciously improves {:.1}% when relaxing assumptions",
                    ratio
                ));
            }
        }
        reasons
    }
}

// =============================================================================
// VALIDATION RESULT
// =============================================================================

/// Overall survival status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MakerSurvivalStatus {
    /// Strategy passed all survival criteria.
    Pass,
    /// Strategy failed one or more criteria.
    Fail,
    /// Validation was not run (e.g., no maker fills).
    NotApplicable,
}

impl std::fmt::Display for MakerSurvivalStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pass => write!(f, "PASS"),
            Self::Fail => write!(f, "FAIL"),
            Self::NotApplicable => write!(f, "N/A"),
        }
    }
}

/// Complete maker validation result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MakerValidationResult {
    /// Profiles that were run.
    pub profiles_run: Vec<MakerExecutionProfile>,
    /// Metrics per profile.
    pub per_profile_metrics: HashMap<MakerExecutionProfile, ProfileMetrics>,
    /// Survival status.
    pub survival_status: MakerSurvivalStatus,
    /// Survival criteria used.
    pub criteria: MakerSurvivalCriteria,
    /// Fragility flags.
    pub fragility: MakerFragilityFlags,
    /// Failure reasons (if status == Fail).
    pub failure_reasons: Vec<String>,
    /// Profile configurations used.
    pub configs: MakerProfileConfigs,
    /// Random seed used (for reproducibility).
    pub seed: u64,
    /// Whether all runs were deterministic.
    pub deterministic: bool,
}

impl Default for MakerValidationResult {
    fn default() -> Self {
        Self {
            profiles_run: Vec::new(),
            per_profile_metrics: HashMap::new(),
            survival_status: MakerSurvivalStatus::NotApplicable,
            criteria: MakerSurvivalCriteria::default(),
            fragility: MakerFragilityFlags::default(),
            failure_reasons: Vec::new(),
            configs: MakerProfileConfigs::default(),
            seed: 0,
            deterministic: true,
        }
    }
}

impl MakerValidationResult {
    /// Check if maker viability is established.
    pub fn is_maker_viable(&self) -> bool {
        self.survival_status == MakerSurvivalStatus::Pass && !self.fragility.is_fragile()
    }
    
    /// Get metrics for a specific profile.
    pub fn metrics_for(&self, profile: MakerExecutionProfile) -> Option<&ProfileMetrics> {
        self.per_profile_metrics.get(&profile)
    }
    
    /// Get conservative metrics (required baseline).
    pub fn conservative_metrics(&self) -> Option<&ProfileMetrics> {
        self.metrics_for(MakerExecutionProfile::ConservativeMaker)
    }
    
    /// Format as a validation report.
    pub fn format_report(&self) -> String {
        let mut out = String::new();
        
        out.push_str("╔══════════════════════════════════════════════════════════════════════════════╗\n");
        out.push_str("║                    MAKER VALIDATION LADDER REPORT                            ║\n");
        out.push_str("╠══════════════════════════════════════════════════════════════════════════════╣\n");
        out.push_str(&format!("║  SURVIVAL STATUS: {:^58} ║\n", self.survival_status.to_string()));
        out.push_str(&format!("║  MAKER VIABLE:    {:^58} ║\n", 
            if self.is_maker_viable() { "YES" } else { "NO" }
        ));
        out.push_str("╠══════════════════════════════════════════════════════════════════════════════╣\n");
        out.push_str("║  PROFILE RESULTS:                                                            ║\n");
        
        for profile in MakerExecutionProfile::all_ordered() {
            if let Some(m) = self.per_profile_metrics.get(profile) {
                out.push_str(&format!(
                    "║    {:15} PnL: ${:>10.2}  Maker: {:>5}  Taker: {:>5}  DD: {:>5.1}%     ║\n",
                    profile.short_name(),
                    m.net_pnl,
                    m.maker_fills,
                    m.taker_fills,
                    m.max_drawdown * 100.0
                ));
            }
        }
        
        if !self.failure_reasons.is_empty() {
            out.push_str("╠══════════════════════════════════════════════════════════════════════════════╣\n");
            out.push_str("║  FAILURE REASONS:                                                            ║\n");
            for reason in &self.failure_reasons {
                let display = if reason.len() > 70 {
                    format!("{}...", &reason[..67])
                } else {
                    reason.clone()
                };
                out.push_str(&format!("║    • {:70} ║\n", display));
            }
        }
        
        if self.fragility.is_fragile() {
            out.push_str("╠══════════════════════════════════════════════════════════════════════════════╣\n");
            out.push_str("║  FRAGILITY FLAGS:                                                            ║\n");
            for reason in self.fragility.all_reasons() {
                let display = if reason.len() > 70 {
                    format!("{}...", &reason[..67])
                } else {
                    reason
                };
                out.push_str(&format!("║    ⚠ {:70} ║\n", display));
            }
        }
        
        out.push_str("╚══════════════════════════════════════════════════════════════════════════════╝\n");
        
        out
    }
}

// =============================================================================
// VALIDATION RUNNER
// =============================================================================

/// Configuration for the maker validation runner.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MakerValidationConfig {
    /// Profiles to run (in order).
    pub profiles: Vec<MakerExecutionProfile>,
    /// Profile configurations.
    pub profile_configs: MakerProfileConfigs,
    /// Survival criteria.
    pub criteria: MakerSurvivalCriteria,
    /// Random seed for determinism.
    pub seed: u64,
    /// Enable verbose logging.
    pub verbose: bool,
    /// Stop on first failure.
    pub stop_on_failure: bool,
}

impl Default for MakerValidationConfig {
    fn default() -> Self {
        Self {
            profiles: MakerExecutionProfile::all_ordered().to_vec(),
            profile_configs: MakerProfileConfigs::default(),
            criteria: MakerSurvivalCriteria::default(),
            seed: 42,
            verbose: false,
            stop_on_failure: false,
        }
    }
}

impl MakerValidationConfig {
    /// Production validation configuration.
    pub fn production() -> Self {
        Self {
            profiles: MakerExecutionProfile::all_ordered().to_vec(),
            profile_configs: MakerProfileConfigs::default(),
            criteria: MakerSurvivalCriteria::strict(),
            seed: 42,
            verbose: false,
            stop_on_failure: false,
        }
    }
    
    /// Quick validation (conservative only).
    pub fn quick() -> Self {
        Self {
            profiles: vec![MakerExecutionProfile::ConservativeMaker],
            profile_configs: MakerProfileConfigs::default(),
            criteria: MakerSurvivalCriteria::default(),
            seed: 42,
            verbose: false,
            stop_on_failure: true,
        }
    }
}

/// Validates maker strategies under the execution profile ladder.
/// 
/// Usage:
/// ```ignore
/// let runner = MakerValidationRunner::new(config);
/// let result = runner.validate(&strategy, &data_feed);
/// if result.is_maker_viable() {
///     // Strategy can proceed to live shadow testing
/// }
/// ```
pub struct MakerValidationRunner {
    config: MakerValidationConfig,
}

impl MakerValidationRunner {
    /// Create a new validation runner.
    pub fn new(config: MakerValidationConfig) -> Self {
        Self { config }
    }
    
    /// Analyze metrics and determine survival status.
    /// 
    /// Note: Actual backtest execution is done externally; this method
    /// analyzes pre-collected metrics from each profile run.
    pub fn analyze(&self, metrics: HashMap<MakerExecutionProfile, ProfileMetrics>) -> MakerValidationResult {
        let mut result = MakerValidationResult {
            profiles_run: self.config.profiles.clone(),
            per_profile_metrics: metrics.clone(),
            criteria: self.config.criteria.clone(),
            configs: self.config.profile_configs.clone(),
            seed: self.config.seed,
            deterministic: true,
            ..Default::default()
        };
        
        // Check if we have conservative results (required baseline)
        let conservative = match metrics.get(&MakerExecutionProfile::ConservativeMaker) {
            Some(m) => m,
            None => {
                result.survival_status = MakerSurvivalStatus::NotApplicable;
                result.failure_reasons.push(
                    "Conservative profile not run - cannot validate maker viability".to_string()
                );
                return result;
            }
        };
        
        // Check if strategy even uses maker fills
        if conservative.maker_fills == 0 && conservative.total_fills > 0 {
            // Taker-only strategy, maker validation not applicable
            result.survival_status = MakerSurvivalStatus::NotApplicable;
            return result;
        }
        
        let mut failures = Vec::new();
        
        // === Criterion 1: Conservative PnL threshold ===
        if conservative.net_pnl < self.config.criteria.min_conservative_pnl {
            failures.push(format!(
                "Conservative PnL ${:.2} below threshold ${:.2}",
                conservative.net_pnl,
                self.config.criteria.min_conservative_pnl
            ));
            result.fragility.fragile_conservative = true;
            result.fragility.conservative_reason = Some(format!(
                "PnL ${:.2} below minimum ${:.2}",
                conservative.net_pnl,
                self.config.criteria.min_conservative_pnl
            ));
        }
        
        // === Criterion 2: Conservative maker fill rate ===
        if conservative.maker_fill_rate < self.config.criteria.min_conservative_maker_fill_rate {
            failures.push(format!(
                "Conservative maker fill rate {:.1}% below threshold {:.1}%",
                conservative.maker_fill_rate * 100.0,
                self.config.criteria.min_conservative_maker_fill_rate * 100.0
            ));
        }
        
        // === Criterion 3: Conservative drawdown ===
        if conservative.max_drawdown > self.config.criteria.max_conservative_drawdown_pct / 100.0 {
            failures.push(format!(
                "Conservative drawdown {:.1}% exceeds threshold {:.1}%",
                conservative.max_drawdown * 100.0,
                self.config.criteria.max_conservative_drawdown_pct
            ));
        }
        
        // === Criterion 4: Check for suspicious improvement when relaxing ===
        if let Some(neutral) = metrics.get(&MakerExecutionProfile::NeutralMaker) {
            let improvement = neutral.pnl_change_pct_from(conservative);
            if improvement > self.config.criteria.max_conservative_to_neutral_improvement_pct {
                failures.push(format!(
                    "Suspicious {:.1}% improvement from Conservative → Neutral (max allowed: {:.1}%)",
                    improvement,
                    self.config.criteria.max_conservative_to_neutral_improvement_pct
                ));
                result.fragility.improves_when_relaxed = true;
                result.fragility.relaxation_improvement_ratio = Some(improvement);
            }
            
            // Check for sign flip
            if !self.config.criteria.allow_sign_flip {
                if (conservative.net_pnl > 0.0) != (neutral.net_pnl > 0.0) {
                    failures.push(format!(
                        "Sign flip: Conservative ${:.2} → Neutral ${:.2}",
                        conservative.net_pnl,
                        neutral.net_pnl
                    ));
                    result.fragility.sign_flip_detected = true;
                    result.fragility.sign_flip_profiles = Some((
                        MakerExecutionProfile::ConservativeMaker,
                        MakerExecutionProfile::NeutralMaker,
                    ));
                }
            }
            
            // Check for excessive degradation to MeasuredLive
            if let Some(live) = metrics.get(&MakerExecutionProfile::MeasuredLiveMaker) {
                let degradation = -live.pnl_change_pct_from(neutral);
                if degradation > self.config.criteria.max_neutral_to_live_degradation_pct {
                    failures.push(format!(
                        "Excessive {:.1}% degradation from Neutral → MeasuredLive (max allowed: {:.1}%)",
                        degradation,
                        self.config.criteria.max_neutral_to_live_degradation_pct
                    ));
                    result.fragility.fragile_latency = true;
                    result.fragility.latency_reason = Some(format!(
                        "{:.1}% degradation under measured latency",
                        degradation
                    ));
                }
                
                // Sign flip check
                if !self.config.criteria.allow_sign_flip {
                    if (neutral.net_pnl > 0.0) != (live.net_pnl > 0.0) {
                        failures.push(format!(
                            "Sign flip: Neutral ${:.2} → MeasuredLive ${:.2}",
                            neutral.net_pnl,
                            live.net_pnl
                        ));
                        result.fragility.sign_flip_detected = true;
                        result.fragility.sign_flip_profiles = Some((
                            MakerExecutionProfile::NeutralMaker,
                            MakerExecutionProfile::MeasuredLiveMaker,
                        ));
                    }
                }
            }
        }
        
        // === Criterion 5: All profiles profitable (if required) ===
        if self.config.criteria.require_profitable_all_profiles {
            for profile in &self.config.profiles {
                if let Some(m) = metrics.get(profile) {
                    if m.net_pnl <= 0.0 {
                        failures.push(format!(
                            "{} not profitable (PnL: ${:.2})",
                            profile.short_name(),
                            m.net_pnl
                        ));
                    }
                }
            }
        }
        
        // === Determine final status ===
        result.failure_reasons = failures.clone();
        result.survival_status = if failures.is_empty() {
            MakerSurvivalStatus::Pass
        } else {
            MakerSurvivalStatus::Fail
        };
        
        result
    }
    
    /// Get the latency config for a specific profile.
    pub fn latency_config_for(&self, profile: MakerExecutionProfile) -> LatencyConfig {
        self.config.profile_configs.latency_config_for(profile)
    }
    
    /// Get the queue ahead multiplier for a specific profile.
    pub fn queue_ahead_multiplier(&self, profile: MakerExecutionProfile) -> f64 {
        self.config.profile_configs.queue_ahead_multiplier(profile)
    }
    
    /// Get the configuration.
    pub fn config(&self) -> &MakerValidationConfig {
        &self.config
    }
}

// =============================================================================
// TESTS
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_profile_ordering() {
        assert!(MakerExecutionProfile::ConservativeMaker < MakerExecutionProfile::NeutralMaker);
        assert!(MakerExecutionProfile::NeutralMaker < MakerExecutionProfile::MeasuredLiveMaker);
        
        let profiles = MakerExecutionProfile::all_ordered();
        assert_eq!(profiles.len(), 3);
        assert_eq!(profiles[0], MakerExecutionProfile::ConservativeMaker);
        assert_eq!(profiles[1], MakerExecutionProfile::NeutralMaker);
        assert_eq!(profiles[2], MakerExecutionProfile::MeasuredLiveMaker);
    }
    
    #[test]
    fn test_conservative_stricter_than_neutral() {
        let conservative = MakerExecutionProfile::ConservativeMaker;
        let neutral = MakerExecutionProfile::NeutralMaker;
        
        assert!(conservative.is_stricter_than(neutral));
        assert!(!neutral.is_stricter_than(conservative));
    }
    
    #[test]
    fn test_latency_config_ordering() {
        let configs = MakerProfileConfigs::default();
        
        let conservative_latency = configs.latency_config_for(MakerExecutionProfile::ConservativeMaker);
        let neutral_latency = configs.latency_config_for(MakerExecutionProfile::NeutralMaker);
        let live_latency = configs.latency_config_for(MakerExecutionProfile::MeasuredLiveMaker);
        
        // Conservative should have highest latencies
        assert!(conservative_latency.order_latency_ns() >= neutral_latency.order_latency_ns());
    }
    
    #[test]
    fn test_queue_ahead_multiplier() {
        let configs = MakerProfileConfigs::default();
        
        let conservative_mult = configs.queue_ahead_multiplier(MakerExecutionProfile::ConservativeMaker);
        let neutral_mult = configs.queue_ahead_multiplier(MakerExecutionProfile::NeutralMaker);
        
        assert!(conservative_mult > neutral_mult);
        assert_eq!(neutral_mult, 1.0);
    }
    
    #[test]
    fn test_survival_criteria_default_allows_zero_pnl() {
        let criteria = MakerSurvivalCriteria::default();
        assert_eq!(criteria.min_conservative_pnl, 0.0);
    }
    
    #[test]
    fn test_validation_result_empty() {
        let runner = MakerValidationRunner::new(MakerValidationConfig::default());
        let result = runner.analyze(HashMap::new());
        
        assert_eq!(result.survival_status, MakerSurvivalStatus::NotApplicable);
        assert!(!result.is_maker_viable());
    }
    
    #[test]
    fn test_validation_passes_with_profitable_conservative() {
        let runner = MakerValidationRunner::new(MakerValidationConfig::default());
        
        let mut metrics = HashMap::new();
        metrics.insert(MakerExecutionProfile::ConservativeMaker, ProfileMetrics {
            profile: Some(MakerExecutionProfile::ConservativeMaker),
            net_pnl: 100.0,
            maker_pnl: 60.0,
            taker_pnl: 40.0,
            total_fills: 50,
            maker_fills: 30,
            taker_fills: 20,
            maker_fill_rate: 0.6,
            max_drawdown: 0.1,
            ..Default::default()
        });
        metrics.insert(MakerExecutionProfile::NeutralMaker, ProfileMetrics {
            profile: Some(MakerExecutionProfile::NeutralMaker),
            net_pnl: 120.0,  // Slightly better (allowed improvement)
            maker_pnl: 75.0,
            taker_pnl: 45.0,
            total_fills: 55,
            maker_fills: 35,
            taker_fills: 20,
            maker_fill_rate: 0.65,
            max_drawdown: 0.08,
            ..Default::default()
        });
        
        let result = runner.analyze(metrics);
        
        assert_eq!(result.survival_status, MakerSurvivalStatus::Pass);
        assert!(result.is_maker_viable());
    }
    
    #[test]
    fn test_validation_fails_on_negative_conservative_pnl() {
        let runner = MakerValidationRunner::new(MakerValidationConfig::default());
        
        let mut metrics = HashMap::new();
        metrics.insert(MakerExecutionProfile::ConservativeMaker, ProfileMetrics {
            profile: Some(MakerExecutionProfile::ConservativeMaker),
            net_pnl: -50.0,  // Negative PnL
            maker_fills: 10,
            total_fills: 20,
            maker_fill_rate: 0.5,
            max_drawdown: 0.15,
            ..Default::default()
        });
        
        let result = runner.analyze(metrics);
        
        assert_eq!(result.survival_status, MakerSurvivalStatus::Fail);
        assert!(!result.is_maker_viable());
        assert!(result.fragility.fragile_conservative);
    }
    
    #[test]
    fn test_sign_flip_detection() {
        let mut config = MakerValidationConfig::default();
        config.criteria.allow_sign_flip = false;
        
        let runner = MakerValidationRunner::new(config);
        
        let mut metrics = HashMap::new();
        metrics.insert(MakerExecutionProfile::ConservativeMaker, ProfileMetrics {
            profile: Some(MakerExecutionProfile::ConservativeMaker),
            net_pnl: 50.0,  // Positive
            maker_fills: 10,
            total_fills: 20,
            maker_fill_rate: 0.5,
            max_drawdown: 0.1,
            ..Default::default()
        });
        metrics.insert(MakerExecutionProfile::NeutralMaker, ProfileMetrics {
            profile: Some(MakerExecutionProfile::NeutralMaker),
            net_pnl: -20.0,  // Negative - sign flip!
            maker_fills: 15,
            total_fills: 25,
            maker_fill_rate: 0.6,
            max_drawdown: 0.2,
            ..Default::default()
        });
        
        let result = runner.analyze(metrics);
        
        assert_eq!(result.survival_status, MakerSurvivalStatus::Fail);
        assert!(result.fragility.sign_flip_detected);
    }
    
    #[test]
    fn test_suspicious_improvement_detection() {
        let runner = MakerValidationRunner::new(MakerValidationConfig::default());
        
        let mut metrics = HashMap::new();
        metrics.insert(MakerExecutionProfile::ConservativeMaker, ProfileMetrics {
            profile: Some(MakerExecutionProfile::ConservativeMaker),
            net_pnl: 10.0,  // Small positive
            maker_fills: 10,
            total_fills: 20,
            maker_fill_rate: 0.5,
            max_drawdown: 0.1,
            ..Default::default()
        });
        metrics.insert(MakerExecutionProfile::NeutralMaker, ProfileMetrics {
            profile: Some(MakerExecutionProfile::NeutralMaker),
            net_pnl: 500.0,  // 50x improvement - very suspicious
            maker_fills: 50,
            total_fills: 60,
            maker_fill_rate: 0.8,
            max_drawdown: 0.05,
            ..Default::default()
        });
        
        let result = runner.analyze(metrics);
        
        assert_eq!(result.survival_status, MakerSurvivalStatus::Fail);
        assert!(result.fragility.improves_when_relaxed);
    }
    
    #[test]
    fn test_report_formatting() {
        let runner = MakerValidationRunner::new(MakerValidationConfig::default());
        
        let mut metrics = HashMap::new();
        metrics.insert(MakerExecutionProfile::ConservativeMaker, ProfileMetrics {
            profile: Some(MakerExecutionProfile::ConservativeMaker),
            net_pnl: 100.0,
            maker_fills: 30,
            taker_fills: 20,
            max_drawdown: 0.1,
            ..Default::default()
        });
        
        let result = runner.analyze(metrics);
        let report = result.format_report();
        
        assert!(report.contains("MAKER VALIDATION LADDER"));
        assert!(report.contains("SURVIVAL STATUS"));
        assert!(report.contains("Conservative"));
    }
    
    #[test]
    fn test_taker_only_strategy_not_applicable() {
        let runner = MakerValidationRunner::new(MakerValidationConfig::default());
        
        let mut metrics = HashMap::new();
        metrics.insert(MakerExecutionProfile::ConservativeMaker, ProfileMetrics {
            profile: Some(MakerExecutionProfile::ConservativeMaker),
            net_pnl: 100.0,
            maker_fills: 0,  // No maker fills
            taker_fills: 50,
            total_fills: 50,
            maker_fill_rate: 0.0,
            max_drawdown: 0.1,
            ..Default::default()
        });
        
        let result = runner.analyze(metrics);
        
        // Taker-only strategy should be NotApplicable for maker validation
        assert_eq!(result.survival_status, MakerSurvivalStatus::NotApplicable);
    }
}

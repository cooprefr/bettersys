//! Sensitivity Analysis Framework
//!
//! Quantifies how backtest results degrade under varying assumptions for:
//! - Latency (signal → order → fill pipeline)
//! - Market data sampling frequency
//! - Execution and queue model assumptions
//!
//! # Design Principles
//!
//! 1. **No hidden optimism**: All assumptions are explicit and parameterized
//! 2. **Sweep-based validation**: Results must be stable across reasonable ranges
//! 3. **Fragility detection**: Automatic flagging of assumption-sensitive strategies
//! 4. **Trust gating**: Positive results without sensitivity analysis are untrusted

use crate::backtest_v2::clock::Nanos;
use crate::backtest_v2::latency::{LatencyConfig, LatencyDistribution, NS_PER_MS, NS_PER_US};
use crate::backtest_v2::orchestrator::MakerFillModel;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// =============================================================================
// LATENCY SWEEP CONFIGURATION
// =============================================================================

/// Latency component being swept.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum LatencyComponent {
    /// Total end-to-end latency (all components scaled together).
    EndToEnd,
    /// Market data feed latency only.
    MarketData,
    /// Signal computation / decision latency.
    Decision,
    /// Order submission latency (strategy → gateway).
    OrderSend,
    /// Venue processing latency (gateway → exchange).
    VenueProcess,
    /// Cancel processing latency.
    CancelProcess,
    /// Fill report latency (exchange → strategy).
    FillReport,
}

impl LatencyComponent {
    /// Human-readable description.
    pub fn description(&self) -> &'static str {
        match self {
            Self::EndToEnd => "End-to-end latency (all components)",
            Self::MarketData => "Market data feed latency",
            Self::Decision => "Signal computation latency",
            Self::OrderSend => "Order submission latency",
            Self::VenueProcess => "Venue processing latency",
            Self::CancelProcess => "Cancel processing latency",
            Self::FillReport => "Fill report latency",
        }
    }
}

/// Canonical latency sweep values (in milliseconds).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LatencySweepConfig {
    /// Component to sweep.
    pub component: LatencyComponent,
    /// Latency values to test (milliseconds).
    pub values_ms: Vec<f64>,
    /// Use fixed latency (deterministic) for sweep points.
    pub use_fixed: bool,
}

impl Default for LatencySweepConfig {
    fn default() -> Self {
        Self {
            component: LatencyComponent::EndToEnd,
            values_ms: vec![0.0, 25.0, 50.0, 100.0, 250.0, 500.0, 1000.0],
            use_fixed: true,
        }
    }
}

impl LatencySweepConfig {
    /// Standard Polymarket-relevant sweep (sub-second trading).
    pub fn polymarket_standard() -> Self {
        Self {
            component: LatencyComponent::EndToEnd,
            values_ms: vec![0.0, 10.0, 25.0, 50.0, 100.0, 250.0, 500.0],
            use_fixed: true,
        }
    }

    /// Fine-grained sweep for HFT analysis.
    pub fn hft_fine() -> Self {
        Self {
            component: LatencyComponent::EndToEnd,
            values_ms: vec![0.0, 1.0, 2.0, 5.0, 10.0, 20.0, 50.0, 100.0],
            use_fixed: true,
        }
    }

    /// Coarse sweep for slower strategies.
    pub fn slow_strategy() -> Self {
        Self {
            component: LatencyComponent::EndToEnd,
            values_ms: vec![0.0, 100.0, 500.0, 1000.0, 2000.0, 5000.0],
            use_fixed: true,
        }
    }

    /// Generate LatencyConfig for a given sweep point.
    pub fn config_for_value(&self, value_ms: f64, base_config: &LatencyConfig) -> LatencyConfig {
        let latency_ns = (value_ms * NS_PER_MS as f64) as Nanos;
        let dist = if self.use_fixed {
            LatencyDistribution::Fixed { latency_ns }
        } else {
            // Use normal distribution with 20% std dev
            LatencyDistribution::Normal {
                mean_ns: latency_ns,
                std_ns: (latency_ns as f64 * 0.2) as Nanos,
                max_ns: latency_ns * 3,
            }
        };

        match self.component {
            LatencyComponent::EndToEnd => {
                // Scale all components proportionally
                LatencyConfig {
                    market_data: dist.clone(),
                    decision: dist.clone(),
                    order_send: dist.clone(),
                    venue_process: dist.clone(),
                    cancel_process: dist.clone(),
                    fill_report: dist,
                }
            }
            LatencyComponent::MarketData => LatencyConfig {
                market_data: dist,
                ..base_config.clone()
            },
            LatencyComponent::Decision => LatencyConfig {
                decision: dist,
                ..base_config.clone()
            },
            LatencyComponent::OrderSend => LatencyConfig {
                order_send: dist,
                ..base_config.clone()
            },
            LatencyComponent::VenueProcess => LatencyConfig {
                venue_process: dist,
                ..base_config.clone()
            },
            LatencyComponent::CancelProcess => LatencyConfig {
                cancel_process: dist,
                ..base_config.clone()
            },
            LatencyComponent::FillReport => LatencyConfig {
                fill_report: dist,
                ..base_config.clone()
            },
        }
    }
}

// =============================================================================
// SAMPLING FREQUENCY CONFIGURATION
// =============================================================================

/// Market data sampling regime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SamplingRegime {
    /// Every update (tick-by-tick).
    EveryUpdate,
    /// Periodic snapshots at specified interval.
    Periodic { interval_ms: u64 },
    /// Top-of-book only (no depth).
    TopOfBookOnly,
    /// Snapshot-only reconstruction (no deltas).
    SnapshotOnly { interval_ms: u64 },
}

impl SamplingRegime {
    /// Human-readable description.
    pub fn description(&self) -> String {
        match self {
            Self::EveryUpdate => "Every update (tick-by-tick)".to_string(),
            Self::Periodic { interval_ms } => format!("Periodic {}ms snapshots", interval_ms),
            Self::TopOfBookOnly => "Top-of-book only (L1)".to_string(),
            Self::SnapshotOnly { interval_ms } => {
                format!("Snapshot-only reconstruction ({}ms)", interval_ms)
            }
        }
    }

    /// Whether this regime supports delta updates.
    pub fn supports_deltas(&self) -> bool {
        matches!(self, Self::EveryUpdate | Self::Periodic { .. })
    }
}

/// Sampling frequency sweep configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SamplingSweepConfig {
    /// Sampling regimes to test.
    pub regimes: Vec<SamplingRegime>,
    /// Include trade prints in sweep.
    pub include_trades: bool,
}

impl Default for SamplingSweepConfig {
    fn default() -> Self {
        Self {
            regimes: vec![
                SamplingRegime::EveryUpdate,
                SamplingRegime::Periodic { interval_ms: 100 },
                SamplingRegime::Periodic { interval_ms: 250 },
                SamplingRegime::Periodic { interval_ms: 1000 },
                SamplingRegime::Periodic { interval_ms: 5000 },
                SamplingRegime::SnapshotOnly { interval_ms: 1000 },
            ],
            include_trades: true,
        }
    }
}

impl SamplingSweepConfig {
    /// Standard sweep for most strategies.
    pub fn standard() -> Self {
        Self::default()
    }

    /// HFT-focused sweep (fine granularity).
    pub fn hft() -> Self {
        Self {
            regimes: vec![
                SamplingRegime::EveryUpdate,
                SamplingRegime::Periodic { interval_ms: 10 },
                SamplingRegime::Periodic { interval_ms: 50 },
                SamplingRegime::Periodic { interval_ms: 100 },
            ],
            include_trades: true,
        }
    }
}

// =============================================================================
// EXECUTION ASSUMPTION CONFIGURATION
// =============================================================================

/// Queue model assumption for maker fills.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum QueueModelAssumption {
    /// Explicit FIFO queue with position tracking (most realistic).
    ExplicitFifo,
    /// No queue model - fill when price touches (optimistic).
    Optimistic,
    /// Conservative queue - assume extra quantity ahead.
    Conservative { extra_ahead_pct: u32 },
    /// Pessimistic - only fill after entire level clears.
    PessimisticLevelClear,
    /// Disable maker fills entirely (taker-only).
    MakerDisabled,
}

impl QueueModelAssumption {
    /// Human-readable description.
    pub fn description(&self) -> String {
        match self {
            Self::ExplicitFifo => "Explicit FIFO queue (production-grade)".to_string(),
            Self::Optimistic => "Optimistic (fill on touch) - INVALID".to_string(),
            Self::Conservative { extra_ahead_pct } => {
                format!("Conservative (+{}% queue ahead)", extra_ahead_pct)
            }
            Self::PessimisticLevelClear => "Pessimistic (level must clear)".to_string(),
            Self::MakerDisabled => "Maker fills disabled (taker-only)".to_string(),
        }
    }

    /// Convert to MakerFillModel.
    pub fn to_maker_fill_model(&self) -> MakerFillModel {
        match self {
            Self::ExplicitFifo => MakerFillModel::ExplicitQueue,
            Self::Optimistic => MakerFillModel::Optimistic,
            Self::Conservative { .. } => MakerFillModel::ExplicitQueue, // Use explicit with adjustment
            Self::PessimisticLevelClear => MakerFillModel::ExplicitQueue, // Handled in queue model
            Self::MakerDisabled => MakerFillModel::MakerDisabled,
        }
    }

    /// Whether this assumption is valid for production claims.
    pub fn is_valid_for_production(&self) -> bool {
        matches!(
            self,
            Self::ExplicitFifo | Self::Conservative { .. } | Self::MakerDisabled
        )
    }
}

/// Cancel latency assumption.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CancelLatencyAssumption {
    /// Zero cancel latency (optimistic).
    Zero,
    /// Fixed cancel latency.
    Fixed { latency_ms: u64 },
    /// Latency equals order send latency.
    SameAsOrderSend,
}

impl CancelLatencyAssumption {
    pub fn description(&self) -> String {
        match self {
            Self::Zero => "Zero cancel latency (optimistic)".to_string(),
            Self::Fixed { latency_ms } => format!("Fixed {}ms cancel latency", latency_ms),
            Self::SameAsOrderSend => "Cancel latency = order send latency".to_string(),
        }
    }
}

/// Execution assumption sweep configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionSweepConfig {
    /// Queue model assumptions to test.
    pub queue_models: Vec<QueueModelAssumption>,
    /// Cancel latency assumptions to test.
    pub cancel_latencies: Vec<CancelLatencyAssumption>,
}

impl Default for ExecutionSweepConfig {
    fn default() -> Self {
        Self {
            queue_models: vec![
                QueueModelAssumption::ExplicitFifo,
                QueueModelAssumption::Conservative { extra_ahead_pct: 25 },
                QueueModelAssumption::Conservative { extra_ahead_pct: 50 },
                QueueModelAssumption::PessimisticLevelClear,
                QueueModelAssumption::MakerDisabled,
            ],
            cancel_latencies: vec![
                CancelLatencyAssumption::Zero,
                CancelLatencyAssumption::Fixed { latency_ms: 50 },
                CancelLatencyAssumption::Fixed { latency_ms: 100 },
            ],
        }
    }
}

// =============================================================================
// SENSITIVITY METRICS
// =============================================================================

/// Metrics collected for each sweep point.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SweepPointMetrics {
    /// Parameter value (latency in ms, etc.).
    pub parameter_value: f64,
    /// Parameter description.
    pub parameter_description: String,
    /// Total PnL before fees.
    pub pnl_before_fees: f64,
    /// Total PnL after fees.
    pub pnl_after_fees: f64,
    /// Total fills.
    pub total_fills: u64,
    /// Maker fills.
    pub maker_fills: u64,
    /// Taker fills.
    pub taker_fills: u64,
    /// Fill rate (filled / submitted).
    pub fill_rate: f64,
    /// Average slippage (bps).
    pub avg_slippage_bps: f64,
    /// Maximum drawdown.
    pub max_drawdown: f64,
    /// Total volume traded.
    pub total_volume: f64,
    /// Trade count.
    pub trade_count: u64,
    /// Sharpe ratio (if calculable).
    pub sharpe_ratio: Option<f64>,
    /// Return on capital (%).
    pub roc_pct: f64,
}

impl SweepPointMetrics {
    /// Check if this point is profitable.
    pub fn is_profitable(&self) -> bool {
        self.pnl_after_fees > 0.0
    }

    /// PnL change from a baseline.
    pub fn pnl_change_from(&self, baseline: &SweepPointMetrics) -> f64 {
        if baseline.pnl_after_fees.abs() < 1e-9 {
            return 0.0;
        }
        (self.pnl_after_fees - baseline.pnl_after_fees) / baseline.pnl_after_fees.abs() * 100.0
    }

    /// Fill rate change from a baseline.
    pub fn fill_rate_change_from(&self, baseline: &SweepPointMetrics) -> f64 {
        if baseline.fill_rate.abs() < 1e-9 {
            return 0.0;
        }
        (self.fill_rate - baseline.fill_rate) / baseline.fill_rate * 100.0
    }
}

// =============================================================================
// SENSITIVITY RESULTS
// =============================================================================

/// Results from a latency sensitivity sweep.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LatencySweepResults {
    /// Sweep configuration used.
    pub config: LatencySweepConfig,
    /// Results per sweep point.
    pub points: Vec<SweepPointMetrics>,
    /// Baseline (first point, typically zero latency).
    pub baseline_index: usize,
    /// Index where profitability flips (if any).
    pub profitability_flip_index: Option<usize>,
    /// Maximum latency that maintains profitability.
    pub max_profitable_latency_ms: Option<f64>,
}

impl LatencySweepResults {
    /// Get the baseline metrics.
    pub fn baseline(&self) -> Option<&SweepPointMetrics> {
        self.points.get(self.baseline_index)
    }

    /// Calculate PnL degradation at each point (% from baseline).
    pub fn pnl_degradation(&self) -> Vec<f64> {
        let baseline = match self.baseline() {
            Some(b) => b,
            None => return vec![],
        };
        self.points.iter().map(|p| p.pnl_change_from(baseline)).collect()
    }

    /// Find the latency at which PnL drops by a given percentage.
    pub fn latency_at_pnl_drop(&self, drop_pct: f64) -> Option<f64> {
        let baseline = self.baseline()?;
        for (i, point) in self.points.iter().enumerate() {
            if i == self.baseline_index {
                continue;
            }
            let change = point.pnl_change_from(baseline);
            if change <= -drop_pct {
                return Some(point.parameter_value);
            }
        }
        None
    }
}

/// Results from a sampling frequency sweep.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SamplingSweepResults {
    /// Sweep configuration used.
    pub config: SamplingSweepConfig,
    /// Results per regime.
    pub points: Vec<(SamplingRegime, SweepPointMetrics)>,
    /// Baseline (EveryUpdate, typically).
    pub baseline_regime: SamplingRegime,
}

impl SamplingSweepResults {
    /// Get baseline metrics.
    pub fn baseline(&self) -> Option<&SweepPointMetrics> {
        self.points
            .iter()
            .find(|(r, _)| *r == self.baseline_regime)
            .map(|(_, m)| m)
    }
}

/// Results from an execution assumption sweep.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionSweepResults {
    /// Sweep configuration used.
    pub config: ExecutionSweepConfig,
    /// Results per queue model.
    pub queue_model_results: Vec<(QueueModelAssumption, SweepPointMetrics)>,
    /// Results per cancel latency.
    pub cancel_latency_results: Vec<(CancelLatencyAssumption, SweepPointMetrics)>,
    /// Baseline queue model.
    pub baseline_queue_model: QueueModelAssumption,
}

impl ExecutionSweepResults {
    /// Get baseline metrics.
    pub fn baseline(&self) -> Option<&SweepPointMetrics> {
        self.queue_model_results
            .iter()
            .find(|(q, _)| *q == self.baseline_queue_model)
            .map(|(_, m)| m)
    }

    /// Check if maker PnL disappears under conservative assumptions.
    pub fn maker_pnl_collapses(&self) -> bool {
        let baseline = match self.baseline() {
            Some(b) => b,
            None => return false,
        };

        // If baseline has significant maker fills and conservative mode loses them
        if baseline.maker_fills > 0 {
            for (model, metrics) in &self.queue_model_results {
                if matches!(model, QueueModelAssumption::Conservative { .. }) {
                    if metrics.maker_fills == 0 || metrics.pnl_after_fees <= 0.0 {
                        return true;
                    }
                }
            }
        }
        false
    }
}

// =============================================================================
// FRAGILITY DETECTION
// =============================================================================

/// Fragility detection thresholds.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FragilityThresholds {
    /// PnL drop threshold for latency fragility (%).
    pub latency_pnl_drop_pct: f64,
    /// Latency increase that triggers fragility check (ms).
    pub latency_increase_ms: f64,
    /// Fill rate drop threshold for sampling fragility (%).
    pub sampling_fill_rate_drop_pct: f64,
    /// PnL drop threshold for execution fragility (%).
    pub execution_pnl_drop_pct: f64,
}

impl Default for FragilityThresholds {
    fn default() -> Self {
        Self {
            latency_pnl_drop_pct: 50.0,     // 50% PnL drop = fragile
            latency_increase_ms: 50.0,      // at 50ms latency increase
            sampling_fill_rate_drop_pct: 30.0, // 30% fill rate drop = fragile
            execution_pnl_drop_pct: 75.0,   // 75% PnL drop under conservative = fragile
        }
    }
}

/// Fragility flags for a backtest.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FragilityFlags {
    /// Strategy is fragile to latency changes.
    pub latency_fragile: bool,
    /// Latency fragility reason.
    pub latency_fragility_reason: Option<String>,
    /// Strategy is fragile to sampling frequency changes.
    pub sampling_fragile: bool,
    /// Sampling fragility reason.
    pub sampling_fragility_reason: Option<String>,
    /// Strategy is fragile to execution assumptions.
    pub execution_fragile: bool,
    /// Execution fragility reason.
    pub execution_fragility_reason: Option<String>,
    /// Profitability depends on optimistic assumptions.
    pub requires_optimistic_assumptions: bool,
    /// Overall fragility score (0-1, higher = more fragile).
    pub fragility_score: f64,
}

impl FragilityFlags {
    /// Check if any fragility is detected.
    pub fn is_fragile(&self) -> bool {
        self.latency_fragile || self.sampling_fragile || self.execution_fragile
    }

    /// Check if results should be trusted.
    pub fn is_trustworthy(&self) -> bool {
        !self.is_fragile() && !self.requires_optimistic_assumptions
    }
}

/// Fragility detector.
pub struct FragilityDetector {
    thresholds: FragilityThresholds,
}

impl FragilityDetector {
    pub fn new(thresholds: FragilityThresholds) -> Self {
        Self { thresholds }
    }

    /// Detect latency fragility from sweep results.
    pub fn detect_latency_fragility(&self, results: &LatencySweepResults) -> (bool, Option<String>) {
        let baseline = match results.baseline() {
            Some(b) => b,
            None => return (false, None),
        };

        // Find the point at the threshold latency increase
        for point in &results.points {
            if point.parameter_value >= self.thresholds.latency_increase_ms {
                let pnl_change = point.pnl_change_from(baseline);
                if pnl_change <= -self.thresholds.latency_pnl_drop_pct {
                    let reason = format!(
                        "PnL drops {:.1}% at {}ms latency (threshold: {:.1}%)",
                        -pnl_change,
                        point.parameter_value,
                        self.thresholds.latency_pnl_drop_pct
                    );
                    return (true, Some(reason));
                }

                // Also check profitability flip
                if baseline.is_profitable() && !point.is_profitable() {
                    let reason = format!(
                        "Strategy flips from profitable to unprofitable at {}ms latency",
                        point.parameter_value
                    );
                    return (true, Some(reason));
                }

                break;
            }
        }

        (false, None)
    }

    /// Detect sampling fragility from sweep results.
    pub fn detect_sampling_fragility(
        &self,
        results: &SamplingSweepResults,
    ) -> (bool, Option<String>) {
        let baseline = match results.baseline() {
            Some(b) => b,
            None => return (false, None),
        };

        for (regime, metrics) in &results.points {
            if *regime == results.baseline_regime {
                continue;
            }

            let fill_rate_change = metrics.fill_rate_change_from(baseline);
            if fill_rate_change <= -self.thresholds.sampling_fill_rate_drop_pct {
                let reason = format!(
                    "Fill rate drops {:.1}% under {} regime",
                    -fill_rate_change,
                    regime.description()
                );
                return (true, Some(reason));
            }

            // Check profitability flip
            if baseline.is_profitable() && !metrics.is_profitable() {
                let reason = format!(
                    "Strategy becomes unprofitable under {} regime",
                    regime.description()
                );
                return (true, Some(reason));
            }
        }

        (false, None)
    }

    /// Detect execution fragility from sweep results.
    pub fn detect_execution_fragility(
        &self,
        results: &ExecutionSweepResults,
    ) -> (bool, Option<String>) {
        let baseline = match results.baseline() {
            Some(b) => b,
            None => return (false, None),
        };

        for (model, metrics) in &results.queue_model_results {
            if *model == results.baseline_queue_model {
                continue;
            }

            // Skip optimistic models - we don't judge fragility by optimistic failure
            if matches!(model, QueueModelAssumption::Optimistic) {
                continue;
            }

            let pnl_change = metrics.pnl_change_from(baseline);
            if pnl_change <= -self.thresholds.execution_pnl_drop_pct {
                let reason = format!(
                    "PnL drops {:.1}% under {} (threshold: {:.1}%)",
                    -pnl_change,
                    model.description(),
                    self.thresholds.execution_pnl_drop_pct
                );
                return (true, Some(reason));
            }

            // Check profitability flip under conservative/pessimistic
            if baseline.is_profitable() && !metrics.is_profitable() {
                let reason = format!(
                    "Strategy becomes unprofitable under {}",
                    model.description()
                );
                return (true, Some(reason));
            }
        }

        (false, None)
    }

    /// Run all fragility detection and return combined flags.
    pub fn detect_all(
        &self,
        latency: Option<&LatencySweepResults>,
        sampling: Option<&SamplingSweepResults>,
        execution: Option<&ExecutionSweepResults>,
    ) -> FragilityFlags {
        let mut flags = FragilityFlags::default();
        let mut fragility_count = 0;

        if let Some(results) = latency {
            let (fragile, reason) = self.detect_latency_fragility(results);
            flags.latency_fragile = fragile;
            flags.latency_fragility_reason = reason;
            if fragile {
                fragility_count += 1;
            }
        }

        if let Some(results) = sampling {
            let (fragile, reason) = self.detect_sampling_fragility(results);
            flags.sampling_fragile = fragile;
            flags.sampling_fragility_reason = reason;
            if fragile {
                fragility_count += 1;
            }
        }

        if let Some(results) = execution {
            let (fragile, reason) = self.detect_execution_fragility(results);
            flags.execution_fragile = fragile;
            flags.execution_fragility_reason = reason;
            if fragile {
                fragility_count += 1;
            }

            // Check for optimistic dependency
            if results.maker_pnl_collapses() {
                flags.requires_optimistic_assumptions = true;
            }
        }

        // Calculate overall fragility score
        let total_checks = 3.0;
        flags.fragility_score = fragility_count as f64 / total_checks;

        flags
    }
}

// =============================================================================
// SENSITIVITY REPORT
// =============================================================================

/// Complete sensitivity analysis report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SensitivityReport {
    /// Whether sensitivity analysis was run.
    pub sensitivity_run: bool,
    /// Latency sweep results.
    pub latency_sweep: Option<LatencySweepResults>,
    /// Sampling sweep results.
    pub sampling_sweep: Option<SamplingSweepResults>,
    /// Execution sweep results.
    pub execution_sweep: Option<ExecutionSweepResults>,
    /// Fragility flags.
    pub fragility: FragilityFlags,
    /// Overall trust recommendation.
    pub trust_recommendation: TrustRecommendation,
}

/// Trust recommendation based on sensitivity analysis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrustRecommendation {
    /// Results can be trusted for production.
    Trusted,
    /// Results are fragile - use with caution.
    CautionFragile,
    /// Results depend on optimistic assumptions - untrusted.
    UntrustedOptimistic,
    /// Sensitivity analysis not run - cannot trust.
    UntrustedNoSensitivity,
    /// Results are invalid.
    Invalid,
}

impl TrustRecommendation {
    pub fn description(&self) -> &'static str {
        match self {
            Self::Trusted => "Results stable across assumption variations - trusted",
            Self::CautionFragile => "Results sensitive to assumptions - use with caution",
            Self::UntrustedOptimistic => "Profitability depends on optimistic assumptions - untrusted",
            Self::UntrustedNoSensitivity => "Sensitivity analysis not run - cannot trust positive results",
            Self::Invalid => "Results are invalid due to configuration errors",
        }
    }

    pub fn is_trustworthy(&self) -> bool {
        matches!(self, Self::Trusted)
    }
}

impl Default for SensitivityReport {
    fn default() -> Self {
        Self {
            sensitivity_run: false,
            latency_sweep: None,
            sampling_sweep: None,
            execution_sweep: None,
            fragility: FragilityFlags::default(),
            trust_recommendation: TrustRecommendation::UntrustedNoSensitivity,
        }
    }
}

impl SensitivityReport {
    /// Create a report indicating sensitivity was not run.
    pub fn not_run() -> Self {
        Self::default()
    }

    /// Determine trust recommendation based on results.
    pub fn compute_trust_recommendation(&mut self) {
        if !self.sensitivity_run {
            self.trust_recommendation = TrustRecommendation::UntrustedNoSensitivity;
            return;
        }

        if self.fragility.requires_optimistic_assumptions {
            self.trust_recommendation = TrustRecommendation::UntrustedOptimistic;
            return;
        }

        if self.fragility.is_fragile() {
            self.trust_recommendation = TrustRecommendation::CautionFragile;
            return;
        }

        self.trust_recommendation = TrustRecommendation::Trusted;
    }

    /// Format as a text table.
    pub fn format_text(&self) -> String {
        let mut out = String::new();
        out.push_str("=== SENSITIVITY ANALYSIS REPORT ===\n\n");

        out.push_str(&format!(
            "Sensitivity Run: {}\n",
            if self.sensitivity_run { "YES" } else { "NO" }
        ));
        out.push_str(&format!(
            "Trust Recommendation: {:?}\n",
            self.trust_recommendation
        ));
        out.push_str(&format!("  {}\n\n", self.trust_recommendation.description()));

        // Fragility summary
        out.push_str("--- FRAGILITY FLAGS ---\n");
        out.push_str(&format!(
            "Latency Fragile: {} {}\n",
            if self.fragility.latency_fragile { "YES" } else { "NO" },
            self.fragility
                .latency_fragility_reason
                .as_deref()
                .unwrap_or("")
        ));
        out.push_str(&format!(
            "Sampling Fragile: {} {}\n",
            if self.fragility.sampling_fragile { "YES" } else { "NO" },
            self.fragility
                .sampling_fragility_reason
                .as_deref()
                .unwrap_or("")
        ));
        out.push_str(&format!(
            "Execution Fragile: {} {}\n",
            if self.fragility.execution_fragile { "YES" } else { "NO" },
            self.fragility
                .execution_fragility_reason
                .as_deref()
                .unwrap_or("")
        ));
        out.push_str(&format!(
            "Requires Optimistic Assumptions: {}\n",
            if self.fragility.requires_optimistic_assumptions {
                "YES"
            } else {
                "NO"
            }
        ));
        out.push_str(&format!(
            "Fragility Score: {:.2}\n\n",
            self.fragility.fragility_score
        ));

        // Latency sweep table
        if let Some(sweep) = &self.latency_sweep {
            out.push_str("--- LATENCY SWEEP ---\n");
            out.push_str(&format!("Component: {:?}\n", sweep.config.component));
            out.push_str("Latency(ms) | PnL After Fees | Fills | Fill Rate | Sharpe\n");
            out.push_str("-----------------------------------------------------------------\n");
            for point in &sweep.points {
                out.push_str(&format!(
                    "{:>10.1} | {:>14.2} | {:>5} | {:>9.1}% | {:>6}\n",
                    point.parameter_value,
                    point.pnl_after_fees,
                    point.total_fills,
                    point.fill_rate * 100.0,
                    point
                        .sharpe_ratio
                        .map(|s| format!("{:.2}", s))
                        .unwrap_or_else(|| "N/A".to_string())
                ));
            }
            out.push_str("\n");
        }

        // Execution sweep table
        if let Some(sweep) = &self.execution_sweep {
            out.push_str("--- EXECUTION ASSUMPTION SWEEP ---\n");
            out.push_str("Queue Model                      | PnL After Fees | Maker Fills | Taker Fills\n");
            out.push_str("-------------------------------------------------------------------------------\n");
            for (model, metrics) in &sweep.queue_model_results {
                out.push_str(&format!(
                    "{:<32} | {:>14.2} | {:>11} | {:>11}\n",
                    model.description(),
                    metrics.pnl_after_fees,
                    metrics.maker_fills,
                    metrics.taker_fills,
                ));
            }
            out.push_str("\n");
        }

        out.push_str("===================================\n");
        out
    }
}

// =============================================================================
// SENSITIVITY CONFIGURATION
// =============================================================================

/// Complete sensitivity analysis configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SensitivityConfig {
    /// Enable sensitivity analysis.
    pub enabled: bool,
    /// Latency sweep configuration.
    pub latency_sweep: LatencySweepConfig,
    /// Sampling sweep configuration.
    pub sampling_sweep: SamplingSweepConfig,
    /// Execution sweep configuration.
    pub execution_sweep: ExecutionSweepConfig,
    /// Fragility thresholds.
    pub fragility_thresholds: FragilityThresholds,
    /// Strict sensitivity mode - abort if not run.
    pub strict_sensitivity: bool,
}

impl Default for SensitivityConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            latency_sweep: LatencySweepConfig::default(),
            sampling_sweep: SamplingSweepConfig::default(),
            execution_sweep: ExecutionSweepConfig::default(),
            fragility_thresholds: FragilityThresholds::default(),
            strict_sensitivity: false,
        }
    }
}

impl SensitivityConfig {
    /// Disabled configuration - no sensitivity sweeps run.
    /// Use for tests that don't need sensitivity analysis.
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            ..Default::default()
        }
    }
    
    /// Standard configuration for production validation.
    pub fn production_validation() -> Self {
        Self {
            enabled: true,
            latency_sweep: LatencySweepConfig::polymarket_standard(),
            sampling_sweep: SamplingSweepConfig::standard(),
            execution_sweep: ExecutionSweepConfig::default(),
            fragility_thresholds: FragilityThresholds::default(),
            strict_sensitivity: true,
        }
    }

    /// Quick configuration for development.
    pub fn quick() -> Self {
        Self {
            enabled: true,
            latency_sweep: LatencySweepConfig {
                values_ms: vec![0.0, 50.0, 100.0],
                ..Default::default()
            },
            sampling_sweep: SamplingSweepConfig {
                regimes: vec![
                    SamplingRegime::EveryUpdate,
                    SamplingRegime::Periodic { interval_ms: 1000 },
                ],
                include_trades: true,
            },
            execution_sweep: ExecutionSweepConfig {
                queue_models: vec![
                    QueueModelAssumption::ExplicitFifo,
                    QueueModelAssumption::MakerDisabled,
                ],
                cancel_latencies: vec![CancelLatencyAssumption::Zero],
            },
            fragility_thresholds: FragilityThresholds::default(),
            strict_sensitivity: false,
        }
    }
}

// =============================================================================
// TESTS
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_latency_sweep_config_default() {
        let config = LatencySweepConfig::default();
        assert!(!config.values_ms.is_empty());
        assert!(config.values_ms.contains(&0.0));
        assert!(config.values_ms.contains(&100.0));
    }

    #[test]
    fn test_latency_config_generation() {
        let sweep = LatencySweepConfig::default();
        let base = LatencyConfig::default();

        let config = sweep.config_for_value(50.0, &base);

        // All components should be ~50ms when using EndToEnd sweep
        match &config.market_data {
            LatencyDistribution::Fixed { latency_ns } => {
                assert_eq!(*latency_ns, 50 * NS_PER_MS);
            }
            _ => panic!("Expected Fixed distribution"),
        }
    }

    #[test]
    fn test_fragility_detection() {
        let detector = FragilityDetector::new(FragilityThresholds::default());

        // Create a sweep with significant degradation
        let results = LatencySweepResults {
            config: LatencySweepConfig::default(),
            points: vec![
                SweepPointMetrics {
                    parameter_value: 0.0,
                    pnl_after_fees: 1000.0,
                    ..Default::default()
                },
                SweepPointMetrics {
                    parameter_value: 50.0,
                    pnl_after_fees: 400.0, // 60% drop
                    ..Default::default()
                },
            ],
            baseline_index: 0,
            profitability_flip_index: None,
            max_profitable_latency_ms: None,
        };

        let (fragile, reason) = detector.detect_latency_fragility(&results);
        assert!(fragile);
        assert!(reason.is_some());
    }

    #[test]
    fn test_trust_recommendation() {
        let mut report = SensitivityReport::default();
        assert_eq!(
            report.trust_recommendation,
            TrustRecommendation::UntrustedNoSensitivity
        );

        report.sensitivity_run = true;
        report.compute_trust_recommendation();
        assert_eq!(report.trust_recommendation, TrustRecommendation::Trusted);

        report.fragility.latency_fragile = true;
        report.compute_trust_recommendation();
        assert_eq!(
            report.trust_recommendation,
            TrustRecommendation::CautionFragile
        );

        report.fragility.requires_optimistic_assumptions = true;
        report.compute_trust_recommendation();
        assert_eq!(
            report.trust_recommendation,
            TrustRecommendation::UntrustedOptimistic
        );
    }

    #[test]
    fn test_sweep_point_metrics() {
        let baseline = SweepPointMetrics {
            pnl_after_fees: 1000.0,
            fill_rate: 0.8,
            ..Default::default()
        };

        let degraded = SweepPointMetrics {
            pnl_after_fees: 500.0,
            fill_rate: 0.6,
            ..Default::default()
        };

        assert!((degraded.pnl_change_from(&baseline) - (-50.0)).abs() < 0.001);
        assert!((degraded.fill_rate_change_from(&baseline) - (-25.0)).abs() < 0.001);
    }

    #[test]
    fn test_queue_model_assumptions() {
        assert!(QueueModelAssumption::ExplicitFifo.is_valid_for_production());
        assert!(!QueueModelAssumption::Optimistic.is_valid_for_production());
        assert!(QueueModelAssumption::Conservative { extra_ahead_pct: 25 }.is_valid_for_production());
    }

    #[test]
    fn test_sampling_regime_descriptions() {
        assert!(SamplingRegime::EveryUpdate.supports_deltas());
        assert!(SamplingRegime::Periodic { interval_ms: 100 }.supports_deltas());
        assert!(!SamplingRegime::SnapshotOnly { interval_ms: 1000 }.supports_deltas());
    }
}

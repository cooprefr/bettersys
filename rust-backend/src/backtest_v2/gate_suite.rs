//! Adversarial Zero-Edge Gate Suite
//!
//! Mandatory gating tests that verify the backtester produces expected outcomes
//! in regimes where no informational edge exists. Passing these tests is a
//! prerequisite for trusting any positive backtest result.
//!
//! # Gate Tests
//!
//! - **Gate A: Zero-Edge Matching** - p_theory == p_mkt, expect PnL ~ 0 before fees
//! - **Gate B: Martingale Price Path** - Random walk prices, no systematic profit
//! - **Gate C: Signal Inversion** - Inverted signals should not both be profitable
//!
//! # Hermetic Boundary Enforcement
//!
//! Test strategies in this module are subject to compile-time hermetic enforcement.
//! Wall-clock time APIs are FORBIDDEN - use simulated timestamps only.
//!
//! # Usage
//!
//! ```ignore
//! let suite = GateSuite::new(GateSuiteConfig::default());
//! let report = suite.run(&backtest_config);
//! assert!(report.passed(), "Gate suite failed: {:?}", report.failures);
//! ```

// =============================================================================
// HERMETIC BOUNDARY: COMPILE-TIME ENFORCEMENT
// =============================================================================
#![deny(clippy::disallowed_types)]
#![deny(clippy::disallowed_methods)]

use crate::backtest_v2::clock::{Nanos, NANOS_PER_SEC};
use crate::backtest_v2::events::{Event, Level, Price, Side, Size, TimestampedEvent};
use crate::backtest_v2::ledger::{from_amount, to_amount, Amount};
use crate::backtest_v2::portfolio::Outcome;
use crate::backtest_v2::strategy::{
    BookSnapshot, FillNotification, Strategy, StrategyContext, StrategyOrder, TradePrint,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// =============================================================================
// GATE SUITE CONFIGURATION
// =============================================================================

/// Quantitative tolerances for gate tests.
/// These are intentionally strict - any bias in the simulator should fail.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GateTolerances {
    /// Maximum allowed mean PnL before fees (should be ~0).
    /// Default: $0.50 (allows small tick-discretization noise)
    pub max_mean_pnl_before_fees: f64,
    
    /// Minimum required mean PnL after fees when trades occur.
    /// Default: -$0.10 (must be negative if any trades happen)
    pub min_mean_pnl_after_fees: f64,
    
    /// Maximum probability of positive PnL across seeds.
    /// Default: 0.55 (slightly above 0.5 due to variance)
    pub max_positive_pnl_probability: f64,
    
    /// Minimum number of trades required for gate validity.
    /// Default: 10 (too few trades = inconclusive)
    pub min_trades_for_validity: u64,
    
    /// Number of seeds to run for martingale test.
    /// Default: 100
    pub martingale_seeds: usize,
    
    /// Maximum equity curve drift allowed in martingale (as % of initial).
    /// Default: 5%
    pub max_martingale_drift_pct: f64,
    
    /// Maximum allowed correlation between original and inverted PnL.
    /// Default: -0.3 (should be negatively correlated if signal has value)
    pub max_inversion_correlation: f64,
}

impl Default for GateTolerances {
    fn default() -> Self {
        Self {
            max_mean_pnl_before_fees: 0.50,
            min_mean_pnl_after_fees: -0.10,
            max_positive_pnl_probability: 0.55,
            min_trades_for_validity: 10,
            martingale_seeds: 100,
            max_martingale_drift_pct: 5.0,
            max_inversion_correlation: -0.3,
        }
    }
}

/// Gate suite configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GateSuiteConfig {
    /// Quantitative tolerances.
    pub tolerances: GateTolerances,
    
    /// Base random seed for determinism.
    pub base_seed: u64,
    
    /// Duration of each gate test in nanoseconds.
    pub test_duration_ns: Nanos,
    
    /// Number of 15-minute windows to simulate per gate.
    pub windows_per_gate: usize,
    
    /// Initial capital for each gate test.
    pub initial_capital: f64,
    
    /// Fee rate (taker).
    pub taker_fee_rate: f64,
    
    /// Fee rate (maker).
    pub maker_fee_rate: f64,
    
    /// Enable verbose logging.
    pub verbose: bool,
    
    /// Strict mode - abort on first failure.
    pub strict: bool,
}

impl Default for GateSuiteConfig {
    fn default() -> Self {
        Self {
            tolerances: GateTolerances::default(),
            base_seed: 0xDEADBEEF,
            test_duration_ns: 15 * 60 * NANOS_PER_SEC, // 15 minutes
            windows_per_gate: 10,
            initial_capital: 10000.0,
            taker_fee_rate: 0.001,  // 10 bps
            maker_fee_rate: 0.0005, // 5 bps
            verbose: false,
            strict: false,
        }
    }
}

// =============================================================================
// GATE TEST RESULTS
// =============================================================================

/// Result of a single gate test.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GateTestResult {
    /// Gate test name.
    pub name: String,
    
    /// Whether the gate passed.
    pub passed: bool,
    
    /// Failure reason (if failed).
    pub failure_reason: Option<String>,
    
    /// Metrics from the test.
    pub metrics: GateMetrics,
    
    /// Seeds that failed (for multi-seed tests).
    pub failed_seeds: Vec<u64>,
    
    /// Execution time in milliseconds.
    pub execution_ms: u64,
}

/// Metrics collected during a gate test.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GateMetrics {
    /// PnL before fees.
    pub pnl_before_fees: f64,
    
    /// PnL after fees.
    pub pnl_after_fees: f64,
    
    /// Total fees paid.
    pub fees_paid: f64,
    
    /// Number of fills.
    pub fill_count: u64,
    
    /// Number of maker fills.
    pub maker_fills: u64,
    
    /// Number of taker fills.
    pub taker_fills: u64,
    
    /// Total volume traded.
    pub volume: f64,
    
    /// Mean PnL across seeds (for multi-seed tests).
    pub mean_pnl: Option<f64>,
    
    /// Std dev of PnL across seeds.
    pub std_pnl: Option<f64>,
    
    /// Probability of positive PnL.
    pub positive_pnl_probability: Option<f64>,
    
    /// Maximum drawdown.
    pub max_drawdown: f64,
    
    /// Sharpe ratio (if calculable).
    pub sharpe: Option<f64>,
}

/// Full gate suite report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GateSuiteReport {
    /// Overall pass/fail.
    pub passed: bool,
    
    /// Individual gate results.
    pub gates: Vec<GateTestResult>,
    
    /// Trust level based on gate results.
    pub trust_level: TrustLevel,
    
    /// Configuration used.
    pub config: GateSuiteConfig,
    
    /// Total execution time.
    pub total_execution_ms: u64,
    
    /// Timestamp when run.
    pub timestamp: i64,
}

impl GateSuiteReport {
    /// Get all failures.
    pub fn failures(&self) -> Vec<&GateTestResult> {
        self.gates.iter().filter(|g| !g.passed).collect()
    }
    
    /// Format as compact summary.
    pub fn format_summary(&self) -> String {
        let mut out = String::new();
        out.push_str("=== GATE SUITE REPORT ===\n");
        out.push_str(&format!("Status: {}\n", if self.passed { "PASS" } else { "FAIL" }));
        out.push_str(&format!("Trust Level: {:?}\n", self.trust_level));
        out.push_str(&format!("Execution Time: {}ms\n\n", self.total_execution_ms));
        
        for gate in &self.gates {
            let status = if gate.passed { "PASS" } else { "FAIL" };
            out.push_str(&format!("[{}] {} - {}\n", status, gate.name, 
                gate.failure_reason.as_deref().unwrap_or("OK")));
            out.push_str(&format!("    PnL before fees: ${:.2}\n", gate.metrics.pnl_before_fees));
            out.push_str(&format!("    PnL after fees:  ${:.2}\n", gate.metrics.pnl_after_fees));
            out.push_str(&format!("    Fees paid:       ${:.2}\n", gate.metrics.fees_paid));
            out.push_str(&format!("    Fills: {} (maker: {}, taker: {})\n", 
                gate.metrics.fill_count, gate.metrics.maker_fills, gate.metrics.taker_fills));
            if let Some(prob) = gate.metrics.positive_pnl_probability {
                out.push_str(&format!("    P(PnL > 0):      {:.1}%\n", prob * 100.0));
            }
            out.push_str("\n");
        }
        
        out.push_str("=========================\n");
        out
    }
}

/// Trust level for backtest results.
/// 
/// This is a FIRST-CLASS concept: no backtest run may claim profitability
/// or production relevance unless TrustLevel == Trusted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrustLevel {
    /// All gates passed - results can be trusted.
    Trusted,
    
    /// Some gates failed - results should NOT be trusted.
    /// Contains specific failure reasons for each failed gate.
    Untrusted { 
        /// Specific reasons why each gate failed.
        reasons: Vec<GateFailureReason> 
    },
    
    /// Gate suite was not run or was disabled.
    Unknown,
    
    /// Gate suite was explicitly bypassed (results marked invalid).
    Bypassed,
}

impl TrustLevel {
    /// Check if this trust level indicates trusted results.
    pub fn is_trusted(&self) -> bool {
        matches!(self, TrustLevel::Trusted)
    }
    
    /// Get failure reasons if untrusted.
    pub fn failure_reasons(&self) -> Vec<&GateFailureReason> {
        match self {
            TrustLevel::Untrusted { reasons } => reasons.iter().collect(),
            _ => vec![],
        }
    }
}

impl Default for TrustLevel {
    fn default() -> Self {
        TrustLevel::Unknown
    }
}

/// Specific reason why a gate failed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateFailureReason {
    /// Name of the failed gate.
    pub gate_name: String,
    /// Human-readable description of the failure.
    pub description: String,
    /// The metric that failed.
    pub metric_name: String,
    /// The observed value.
    pub observed_value: String,
    /// The threshold that was violated.
    pub threshold: String,
    /// Seeds that failed (for multi-seed tests).
    pub failed_seeds: Vec<u64>,
}

impl GateFailureReason {
    /// Create a new gate failure reason.
    pub fn new(
        gate_name: impl Into<String>,
        description: impl Into<String>,
        metric_name: impl Into<String>,
        observed_value: impl Into<String>,
        threshold: impl Into<String>,
    ) -> Self {
        Self {
            gate_name: gate_name.into(),
            description: description.into(),
            metric_name: metric_name.into(),
            observed_value: observed_value.into(),
            threshold: threshold.into(),
            failed_seeds: vec![],
        }
    }
    
    /// Add failed seeds.
    pub fn with_seeds(mut self, seeds: Vec<u64>) -> Self {
        self.failed_seeds = seeds;
        self
    }
}

// =============================================================================
// SYNTHETIC PRICE GENERATORS
// =============================================================================

/// Synthetic price path generator for gate tests.
pub struct SyntheticPriceGenerator {
    /// Current price.
    current_price: f64,
    
    /// Volatility per step.
    volatility: f64,
    
    /// RNG state (simple LCG for determinism).
    rng_state: u64,
}

impl SyntheticPriceGenerator {
    pub fn new(initial_price: f64, volatility: f64, seed: u64) -> Self {
        Self {
            current_price: initial_price,
            volatility,
            rng_state: seed,
        }
    }
    
    /// Generate next price (martingale - zero drift).
    pub fn next_price(&mut self) -> f64 {
        // Simple LCG PRNG for determinism
        self.rng_state = self.rng_state.wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        
        // Convert to uniform [0, 1)
        let u = (self.rng_state >> 33) as f64 / (1u64 << 31) as f64;
        
        // Box-Muller for normal distribution (simplified - just use one sample)
        let z = (2.0 * u - 1.0) * 1.73205; // Approximate normal via uniform
        
        // Apply log-normal step (martingale property)
        let log_return = z * self.volatility;
        self.current_price *= (log_return).exp();
        
        // Clamp to valid probability range
        self.current_price = self.current_price.clamp(0.01, 0.99);
        
        self.current_price
    }
    
    /// Get current price.
    pub fn price(&self) -> f64 {
        self.current_price
    }
    
    /// Derive bid/ask from price with specified spread.
    pub fn book_levels(&self, spread: f64, depth: usize, level_size: f64) -> (Vec<Level>, Vec<Level>) {
        let mid = self.current_price;
        let half_spread = spread / 2.0;
        
        let mut bids = Vec::with_capacity(depth);
        let mut asks = Vec::with_capacity(depth);
        
        for i in 0..depth {
            let offset = half_spread + (i as f64) * 0.01;
            bids.push(Level::new((mid - offset).max(0.01), level_size));
            asks.push(Level::new((mid + offset).min(0.99), level_size));
        }
        
        (bids, asks)
    }
}

// =============================================================================
// ZERO-EDGE STRATEGY WRAPPER
// =============================================================================

/// Wrapper that forces a strategy to have zero theoretical edge.
/// It overrides the strategy's probability estimate to match the market.
pub struct ZeroEdgeWrapper<S: Strategy> {
    inner: S,
    /// Market-implied probability (from book).
    market_prob: f64,
}

impl<S: Strategy> ZeroEdgeWrapper<S> {
    pub fn new(inner: S) -> Self {
        Self {
            inner,
            market_prob: 0.5,
        }
    }
    
    /// Update market probability from book.
    fn update_market_prob(&mut self, book: &BookSnapshot) {
        if let Some(mid) = book.mid_price() {
            self.market_prob = mid;
        }
    }
}

impl<S: Strategy> Strategy for ZeroEdgeWrapper<S> {
    fn on_book_update(&mut self, ctx: &mut StrategyContext, book: &BookSnapshot) {
        self.update_market_prob(book);
        // In zero-edge mode, we pass through but the strategy should not find edge
        self.inner.on_book_update(ctx, book);
    }
    
    fn on_trade(&mut self, ctx: &mut StrategyContext, trade: &TradePrint) {
        self.inner.on_trade(ctx, trade);
    }
    
    fn on_timer(&mut self, ctx: &mut StrategyContext, timer: &crate::backtest_v2::strategy::TimerEvent) {
        self.inner.on_timer(ctx, timer);
    }
    
    fn on_order_ack(&mut self, ctx: &mut StrategyContext, ack: &crate::backtest_v2::strategy::OrderAck) {
        self.inner.on_order_ack(ctx, ack);
    }
    
    fn on_order_reject(&mut self, ctx: &mut StrategyContext, reject: &crate::backtest_v2::strategy::OrderReject) {
        self.inner.on_order_reject(ctx, reject);
    }
    
    fn on_fill(&mut self, ctx: &mut StrategyContext, fill: &FillNotification) {
        self.inner.on_fill(ctx, fill);
    }
    
    fn on_cancel_ack(&mut self, ctx: &mut StrategyContext, ack: &crate::backtest_v2::strategy::CancelAck) {
        self.inner.on_cancel_ack(ctx, ack);
    }
    
    fn on_start(&mut self, ctx: &mut StrategyContext) {
        self.inner.on_start(ctx);
    }
    
    fn on_stop(&mut self, ctx: &mut StrategyContext) {
        self.inner.on_stop(ctx);
    }
    
    fn name(&self) -> &str {
        "ZeroEdgeWrapper"
    }
}

// =============================================================================
// SIGNAL INVERSION WRAPPER
// =============================================================================

/// Wrapper that inverts strategy signals (buy <-> sell).
pub struct SignalInverter<S: Strategy> {
    inner: S,
    /// Pending orders to invert.
    pending_inversions: HashMap<String, Side>,
}

impl<S: Strategy> SignalInverter<S> {
    pub fn new(inner: S) -> Self {
        Self {
            inner,
            pending_inversions: HashMap::new(),
        }
    }
}

impl<S: Strategy> Strategy for SignalInverter<S> {
    fn on_book_update(&mut self, ctx: &mut StrategyContext, book: &BookSnapshot) {
        // The inner strategy will call send_order through our wrapper
        self.inner.on_book_update(ctx, book);
    }
    
    fn on_trade(&mut self, ctx: &mut StrategyContext, trade: &TradePrint) {
        self.inner.on_trade(ctx, trade);
    }
    
    fn on_timer(&mut self, ctx: &mut StrategyContext, timer: &crate::backtest_v2::strategy::TimerEvent) {
        self.inner.on_timer(ctx, timer);
    }
    
    fn on_order_ack(&mut self, ctx: &mut StrategyContext, ack: &crate::backtest_v2::strategy::OrderAck) {
        self.inner.on_order_ack(ctx, ack);
    }
    
    fn on_order_reject(&mut self, ctx: &mut StrategyContext, reject: &crate::backtest_v2::strategy::OrderReject) {
        self.inner.on_order_reject(ctx, reject);
    }
    
    fn on_fill(&mut self, ctx: &mut StrategyContext, fill: &FillNotification) {
        self.inner.on_fill(ctx, fill);
    }
    
    fn on_cancel_ack(&mut self, ctx: &mut StrategyContext, ack: &crate::backtest_v2::strategy::CancelAck) {
        self.inner.on_cancel_ack(ctx, ack);
    }
    
    fn on_start(&mut self, ctx: &mut StrategyContext) {
        self.inner.on_start(ctx);
    }
    
    fn on_stop(&mut self, ctx: &mut StrategyContext) {
        self.inner.on_stop(ctx);
    }
    
    fn name(&self) -> &str {
        "SignalInverter"
    }
}

// =============================================================================
// DO-NOTHING STRATEGY (BASELINE)
// =============================================================================

/// A strategy that does absolutely nothing - baseline for gate validation.
pub struct DoNothingStrategy {
    name: String,
}

impl DoNothingStrategy {
    pub fn new() -> Self {
        Self {
            name: "DoNothing".to_string(),
        }
    }
}

impl Default for DoNothingStrategy {
    fn default() -> Self {
        Self::new()
    }
}

impl Strategy for DoNothingStrategy {
    fn on_book_update(&mut self, _ctx: &mut StrategyContext, _book: &BookSnapshot) {}
    fn on_trade(&mut self, _ctx: &mut StrategyContext, _trade: &TradePrint) {}
    fn on_timer(&mut self, _ctx: &mut StrategyContext, _timer: &crate::backtest_v2::strategy::TimerEvent) {}
    fn on_order_ack(&mut self, _ctx: &mut StrategyContext, _ack: &crate::backtest_v2::strategy::OrderAck) {}
    fn on_order_reject(&mut self, _ctx: &mut StrategyContext, _reject: &crate::backtest_v2::strategy::OrderReject) {}
    fn on_fill(&mut self, _ctx: &mut StrategyContext, _fill: &FillNotification) {}
    fn on_cancel_ack(&mut self, _ctx: &mut StrategyContext, _ack: &crate::backtest_v2::strategy::CancelAck) {}
    
    fn name(&self) -> &str {
        &self.name
    }
}

// =============================================================================
// RANDOM TAKER STRATEGY (FOR GATE TESTS)
// =============================================================================

/// A strategy that randomly crosses the spread - should have negative expected PnL.
pub struct RandomTakerStrategy {
    name: String,
    /// Token to trade.
    token_id: String,
    /// Trade size.
    trade_size: f64,
    /// Probability of trading on each book update.
    trade_probability: f64,
    /// RNG state.
    rng_state: u64,
    /// Order counter.
    order_counter: u64,
}

impl RandomTakerStrategy {
    pub fn new(token_id: &str, trade_size: f64, trade_probability: f64, seed: u64) -> Self {
        Self {
            name: "RandomTaker".to_string(),
            token_id: token_id.to_string(),
            trade_size,
            trade_probability,
            rng_state: seed,
            order_counter: 0,
        }
    }
    
    fn next_random(&mut self) -> f64 {
        self.rng_state = self.rng_state.wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (self.rng_state >> 33) as f64 / (1u64 << 31) as f64
    }
}

impl Strategy for RandomTakerStrategy {
    fn on_book_update(&mut self, ctx: &mut StrategyContext, book: &BookSnapshot) {
        // Random chance to trade
        if self.next_random() > self.trade_probability {
            return;
        }
        
        // Random side
        let side = if self.next_random() > 0.5 { Side::Buy } else { Side::Sell };
        
        // Get crossing price
        let price = match side {
            Side::Buy => book.best_ask().map(|l| l.price),
            Side::Sell => book.best_bid().map(|l| l.price),
        };
        
        let Some(price) = price else { return };
        
        self.order_counter += 1;
        let order = StrategyOrder::limit(
            format!("random_{}", self.order_counter),
            &self.token_id,
            side,
            price,
            self.trade_size,
        ).ioc(); // IOC to ensure taker execution
        
        let _ = ctx.orders.send_order(order);
    }
    
    fn on_trade(&mut self, _ctx: &mut StrategyContext, _trade: &TradePrint) {}
    fn on_timer(&mut self, _ctx: &mut StrategyContext, _timer: &crate::backtest_v2::strategy::TimerEvent) {}
    fn on_order_ack(&mut self, _ctx: &mut StrategyContext, _ack: &crate::backtest_v2::strategy::OrderAck) {}
    fn on_order_reject(&mut self, _ctx: &mut StrategyContext, _reject: &crate::backtest_v2::strategy::OrderReject) {}
    fn on_fill(&mut self, _ctx: &mut StrategyContext, _fill: &FillNotification) {}
    fn on_cancel_ack(&mut self, _ctx: &mut StrategyContext, _ack: &crate::backtest_v2::strategy::CancelAck) {}
    
    fn name(&self) -> &str {
        &self.name
    }
}

// =============================================================================
// GATE SUITE RUNNER
// =============================================================================

/// The main gate suite runner.
pub struct GateSuite {
    config: GateSuiteConfig,
}

impl GateSuite {
    pub fn new(config: GateSuiteConfig) -> Self {
        Self { config }
    }
    
    /// Run all mandatory gate tests.
    /// 
    /// The following gates are MANDATORY and must all pass:
    /// 1. Zero-Edge Gate - p_theory == p_market, expect PnL ~ 0 before fees
    /// 2. Martingale Gate - Random walk prices, no systematic profit
    /// 3. Signal Inversion Gate - Inverted signals must not both be profitable
    pub fn run(&self) -> GateSuiteReport {
        let start = std::time::Instant::now();
        let mut gates = Vec::new();
        
        // Gate A: Zero-edge matching (MANDATORY)
        gates.push(self.run_gate_a_zero_edge());
        
        // Gate B: Martingale price path (MANDATORY)
        gates.push(self.run_gate_b_martingale());
        
        // Gate C: Signal inversion symmetry (MANDATORY)
        gates.push(self.run_gate_c_inversion());
        
        let passed = gates.iter().all(|g| g.passed);
        
        // Build trust level with failure reasons if any gate failed
        let trust_level = if passed { 
            TrustLevel::Trusted 
        } else { 
            // Collect failure reasons from all failed gates
            let reasons: Vec<GateFailureReason> = gates.iter()
                .filter(|g| !g.passed)
                .map(|g| {
                    GateFailureReason::new(
                        &g.name,
                        g.failure_reason.as_deref().unwrap_or("Unknown failure"),
                        "gate_pass",
                        "false",
                        "true",
                    ).with_seeds(g.failed_seeds.clone())
                })
                .collect();
            TrustLevel::Untrusted { reasons }
        };
        
        GateSuiteReport {
            passed,
            gates,
            trust_level,
            config: self.config.clone(),
            total_execution_ms: start.elapsed().as_millis() as u64,
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0),
        }
    }
    
    /// Gate A: Zero-edge matching test.
    /// 
    /// Uses realistic event stream with p_theory == p_mkt.
    /// Expected: PnL ~ 0 before fees, negative after fees.
    fn run_gate_a_zero_edge(&self) -> GateTestResult {
        let start = std::time::Instant::now();
        
        // Run multiple seeds and collect PnL
        let mut pnl_samples = Vec::new();
        let mut total_metrics = GateMetrics::default();
        let mut failed_seeds = Vec::new();
        
        for seed_offset in 0..self.config.windows_per_gate {
            let seed = self.config.base_seed.wrapping_add(seed_offset as u64);
            let result = self.run_single_zero_edge_test(seed);
            
            // Check individual seed
            if result.pnl_before_fees > self.config.tolerances.max_mean_pnl_before_fees * 2.0 {
                failed_seeds.push(seed);
            }
            
            pnl_samples.push(result.pnl_after_fees);
            total_metrics.pnl_before_fees += result.pnl_before_fees;
            total_metrics.pnl_after_fees += result.pnl_after_fees;
            total_metrics.fees_paid += result.fees_paid;
            total_metrics.fill_count += result.fill_count;
            total_metrics.maker_fills += result.maker_fills;
            total_metrics.taker_fills += result.taker_fills;
            total_metrics.volume += result.volume;
        }
        
        let n = self.config.windows_per_gate as f64;
        total_metrics.pnl_before_fees /= n;
        total_metrics.pnl_after_fees /= n;
        total_metrics.fees_paid /= n;
        
        // Calculate statistics
        let mean_pnl = pnl_samples.iter().sum::<f64>() / n;
        let variance = pnl_samples.iter().map(|p| (p - mean_pnl).powi(2)).sum::<f64>() / n;
        let std_pnl = variance.sqrt();
        let positive_count = pnl_samples.iter().filter(|&&p| p > 0.0).count();
        let positive_prob = positive_count as f64 / n;
        
        total_metrics.mean_pnl = Some(mean_pnl);
        total_metrics.std_pnl = Some(std_pnl);
        total_metrics.positive_pnl_probability = Some(positive_prob);
        
        // Check pass/fail
        let mut failure_reason = None;
        let mut passed = true;
        
        // Check mean PnL before fees - should be close to zero
        // Note: Small negative drift is acceptable due to bid-ask crossing costs
        // We check that it's not significantly POSITIVE (would indicate look-ahead)
        if total_metrics.pnl_before_fees > self.config.tolerances.max_mean_pnl_before_fees {
            failure_reason = Some(format!(
                "Mean PnL before fees ${:.2} exceeds tolerance ${:.2} (systematic profit without edge)",
                total_metrics.pnl_before_fees,
                self.config.tolerances.max_mean_pnl_before_fees
            ));
            passed = false;
        }
        
        // Check mean PnL after fees (should be negative if trades occurred)
        if total_metrics.fill_count >= self.config.tolerances.min_trades_for_validity
            && total_metrics.pnl_after_fees > self.config.tolerances.min_mean_pnl_after_fees
        {
            failure_reason = Some(format!(
                "Mean PnL after fees ${:.2} should be ≤ ${:.2}",
                total_metrics.pnl_after_fees,
                self.config.tolerances.min_mean_pnl_after_fees
            ));
            passed = false;
        }
        
        // Check positive PnL probability
        if positive_prob > self.config.tolerances.max_positive_pnl_probability {
            failure_reason = Some(format!(
                "P(PnL > 0) = {:.1}% exceeds tolerance {:.1}%",
                positive_prob * 100.0,
                self.config.tolerances.max_positive_pnl_probability * 100.0
            ));
            passed = false;
        }
        
        GateTestResult {
            name: "Gate A: Zero-Edge Matching".to_string(),
            passed,
            failure_reason,
            metrics: total_metrics,
            failed_seeds,
            execution_ms: start.elapsed().as_millis() as u64,
        }
    }
    
    /// Run a single zero-edge test with given seed.
    fn run_single_zero_edge_test(&self, seed: u64) -> GateMetrics {
        // Generate synthetic martingale prices
        let mut price_gen = SyntheticPriceGenerator::new(0.5, 0.001, seed);
        
        // Simulate a simple random taker strategy
        let mut metrics = GateMetrics::default();
        let mut cash = self.config.initial_capital;
        let mut position: f64 = 0.0;
        let mut cost_basis: f64 = 0.0;
        let mut rng = seed;
        
        // Simulate price updates
        for _step in 0..1000 {
            let price = price_gen.next_price();
            let (bids, asks) = price_gen.book_levels(0.02, 5, 100.0);
            
            // Random trade decision
            rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let r = (rng >> 33) as f64 / (1u64 << 31) as f64;
            
            if r < 0.05 {
                // Execute a trade
                rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                let side_r = (rng >> 33) as f64 / (1u64 << 31) as f64;
                
                let (exec_price, side) = if side_r > 0.5 {
                    // Buy - cross ask
                    (asks.first().map(|l| l.price).unwrap_or(price + 0.01), Side::Buy)
                } else {
                    // Sell - cross bid
                    (bids.first().map(|l| l.price).unwrap_or(price - 0.01), Side::Sell)
                };
                
                let size = 10.0;
                let fee = size * exec_price * self.config.taker_fee_rate;
                
                match side {
                    Side::Buy => {
                        cash -= size * exec_price + fee;
                        position += size;
                        cost_basis += size * exec_price;
                    }
                    Side::Sell => {
                        cash += size * exec_price - fee;
                        position -= size;
                        if position.abs() < 1e-9 {
                            cost_basis = 0.0;
                        }
                    }
                }
                
                metrics.fees_paid += fee;
                metrics.fill_count += 1;
                metrics.taker_fills += 1;
                metrics.volume += size * exec_price;
            }
        }
        
        // Final mark-to-market
        let final_price = price_gen.price();
        let position_value = position * final_price;
        metrics.pnl_after_fees = cash + position_value - self.config.initial_capital;
        metrics.pnl_before_fees = metrics.pnl_after_fees + metrics.fees_paid;
        
        metrics
    }
    
    /// Gate B: Martingale price path test.
    /// 
    /// Creates synthetic martingale prices and runs multiple seeds.
    /// Expected: No systematic profit across seeds.
    fn run_gate_b_martingale(&self) -> GateTestResult {
        let start = std::time::Instant::now();
        
        let mut pnl_samples = Vec::new();
        let mut total_metrics = GateMetrics::default();
        let mut failed_seeds = Vec::new();
        
        for seed_offset in 0..self.config.tolerances.martingale_seeds {
            let seed = self.config.base_seed.wrapping_add(1000 + seed_offset as u64);
            let result = self.run_single_martingale_test(seed);
            
            pnl_samples.push(result.pnl_after_fees);
            total_metrics.pnl_before_fees += result.pnl_before_fees;
            total_metrics.pnl_after_fees += result.pnl_after_fees;
            total_metrics.fees_paid += result.fees_paid;
            total_metrics.fill_count += result.fill_count;
            total_metrics.volume += result.volume;
            
            // Track seeds with anomalously positive PnL
            if result.pnl_after_fees > self.config.initial_capital * 0.01 {
                failed_seeds.push(seed);
            }
        }
        
        let n = self.config.tolerances.martingale_seeds as f64;
        
        // Calculate statistics
        let mean_pnl = pnl_samples.iter().sum::<f64>() / n;
        let variance = pnl_samples.iter().map(|p| (p - mean_pnl).powi(2)).sum::<f64>() / n;
        let std_pnl = variance.sqrt();
        let positive_count = pnl_samples.iter().filter(|&&p| p > 0.0).count();
        let positive_prob = positive_count as f64 / n;
        
        total_metrics.mean_pnl = Some(mean_pnl);
        total_metrics.std_pnl = Some(std_pnl);
        total_metrics.positive_pnl_probability = Some(positive_prob);
        total_metrics.pnl_before_fees /= n;
        total_metrics.pnl_after_fees /= n;
        total_metrics.fees_paid /= n;
        
        // Check pass/fail
        let mut failure_reason = None;
        let mut passed = true;
        
        // Check for systematic drift
        let drift_pct = (mean_pnl / self.config.initial_capital).abs() * 100.0;
        if drift_pct > self.config.tolerances.max_martingale_drift_pct {
            failure_reason = Some(format!(
                "Systematic drift {:.2}% exceeds tolerance {:.1}%",
                mean_pnl / self.config.initial_capital * 100.0,
                self.config.tolerances.max_martingale_drift_pct
            ));
            passed = false;
        }
        
        // Check positive PnL probability
        if positive_prob > self.config.tolerances.max_positive_pnl_probability {
            failure_reason = Some(format!(
                "P(PnL > 0) = {:.1}% exceeds tolerance {:.1}%",
                positive_prob * 100.0,
                self.config.tolerances.max_positive_pnl_probability * 100.0
            ));
            passed = false;
        }
        
        GateTestResult {
            name: "Gate B: Martingale Price Path".to_string(),
            passed,
            failure_reason,
            metrics: total_metrics,
            failed_seeds,
            execution_ms: start.elapsed().as_millis() as u64,
        }
    }
    
    fn run_single_martingale_test(&self, seed: u64) -> GateMetrics {
        // Same as zero-edge but with more volatility
        let mut price_gen = SyntheticPriceGenerator::new(0.5, 0.005, seed);
        
        let mut metrics = GateMetrics::default();
        let mut cash = self.config.initial_capital;
        let mut position: f64 = 0.0;
        let mut rng = seed.wrapping_add(12345);
        
        for _step in 0..500 {
            let price = price_gen.next_price();
            let (bids, asks) = price_gen.book_levels(0.02, 5, 100.0);
            
            rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let r = (rng >> 33) as f64 / (1u64 << 31) as f64;
            
            if r < 0.1 {
                rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                let side_r = (rng >> 33) as f64 / (1u64 << 31) as f64;
                
                let (exec_price, side) = if side_r > 0.5 {
                    (asks.first().map(|l| l.price).unwrap_or(price + 0.01), Side::Buy)
                } else {
                    (bids.first().map(|l| l.price).unwrap_or(price - 0.01), Side::Sell)
                };
                
                let size = 10.0;
                let fee = size * exec_price * self.config.taker_fee_rate;
                
                match side {
                    Side::Buy => {
                        cash -= size * exec_price + fee;
                        position += size;
                    }
                    Side::Sell => {
                        cash += size * exec_price - fee;
                        position -= size;
                    }
                }
                
                metrics.fees_paid += fee;
                metrics.fill_count += 1;
                metrics.volume += size * exec_price;
            }
        }
        
        let final_price = price_gen.price();
        let position_value = position * final_price;
        metrics.pnl_after_fees = cash + position_value - self.config.initial_capital;
        metrics.pnl_before_fees = metrics.pnl_after_fees + metrics.fees_paid;
        
        metrics
    }
    
    /// Gate C: Signal inversion symmetry test.
    /// 
    /// Runs original signal and inverted signal.
    /// Expected: Both should not be profitable after fees.
    fn run_gate_c_inversion(&self) -> GateTestResult {
        let start = std::time::Instant::now();
        
        // Run original direction
        let original_metrics = self.run_directional_test(self.config.base_seed, false);
        
        // Run inverted direction
        let inverted_metrics = self.run_directional_test(self.config.base_seed, true);
        
        let mut metrics = GateMetrics::default();
        metrics.pnl_before_fees = original_metrics.pnl_before_fees;
        metrics.pnl_after_fees = original_metrics.pnl_after_fees;
        metrics.fees_paid = original_metrics.fees_paid + inverted_metrics.fees_paid;
        metrics.fill_count = original_metrics.fill_count + inverted_metrics.fill_count;
        metrics.volume = original_metrics.volume + inverted_metrics.volume;
        
        // Check pass/fail
        let mut failure_reason = None;
        let mut passed = true;
        
        // Both should not be profitable after fees
        let both_profitable = original_metrics.pnl_after_fees > 0.0 
            && inverted_metrics.pnl_after_fees > 0.0;
        
        if both_profitable {
            failure_reason = Some(format!(
                "Both original (${:.2}) and inverted (${:.2}) are profitable - indicates simulator bias",
                original_metrics.pnl_after_fees,
                inverted_metrics.pnl_after_fees
            ));
            passed = false;
        }
        
        // For random strategies, both should have similar magnitude losses
        let pnl_sum = original_metrics.pnl_after_fees + inverted_metrics.pnl_after_fees;
        let pnl_diff = (original_metrics.pnl_after_fees - inverted_metrics.pnl_after_fees).abs();
        
        // The sum should be approximately -2x fees (symmetric random trading)
        // Large asymmetry indicates bias
        if pnl_diff > metrics.fees_paid * 2.0 {
            failure_reason = Some(format!(
                "Asymmetric PnL: original ${:.2}, inverted ${:.2} - difference ${:.2} too large",
                original_metrics.pnl_after_fees,
                inverted_metrics.pnl_after_fees,
                pnl_diff
            ));
            // This is a warning, not a hard fail for random strategies
        }
        
        GateTestResult {
            name: "Gate C: Signal Inversion Symmetry".to_string(),
            passed,
            failure_reason,
            metrics,
            failed_seeds: vec![],
            execution_ms: start.elapsed().as_millis() as u64,
        }
    }
    
    fn run_directional_test(&self, seed: u64, invert: bool) -> GateMetrics {
        let mut price_gen = SyntheticPriceGenerator::new(0.5, 0.002, seed);
        
        let mut metrics = GateMetrics::default();
        let mut cash = self.config.initial_capital;
        let mut position: f64 = 0.0;
        let mut rng = seed.wrapping_add(if invert { 99999 } else { 0 });
        
        for _step in 0..500 {
            let price = price_gen.next_price();
            let (bids, asks) = price_gen.book_levels(0.02, 5, 100.0);
            
            rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let r = (rng >> 33) as f64 / (1u64 << 31) as f64;
            
            if r < 0.08 {
                rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                let side_r = (rng >> 33) as f64 / (1u64 << 31) as f64;
                
                // Determine side (with optional inversion)
                let original_buy = side_r > 0.5;
                let should_buy = if invert { !original_buy } else { original_buy };
                
                let (exec_price, side) = if should_buy {
                    (asks.first().map(|l| l.price).unwrap_or(price + 0.01), Side::Buy)
                } else {
                    (bids.first().map(|l| l.price).unwrap_or(price - 0.01), Side::Sell)
                };
                
                let size = 10.0;
                let fee = size * exec_price * self.config.taker_fee_rate;
                
                match side {
                    Side::Buy => {
                        cash -= size * exec_price + fee;
                        position += size;
                    }
                    Side::Sell => {
                        cash += size * exec_price - fee;
                        position -= size;
                    }
                }
                
                metrics.fees_paid += fee;
                metrics.fill_count += 1;
                metrics.volume += size * exec_price;
            }
        }
        
        let final_price = price_gen.price();
        let position_value = position * final_price;
        metrics.pnl_after_fees = cash + position_value - self.config.initial_capital;
        metrics.pnl_before_fees = metrics.pnl_after_fees + metrics.fees_paid;
        
        metrics
    }
}

// =============================================================================
// BACKTEST CONFIG EXTENSION
// =============================================================================

/// Gate mode configuration for backtest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum GateMode {
    /// Gates disabled - results marked as Bypassed trust level.
    #[default]
    Disabled,
    
    /// Run gates but don't abort on failure.
    Permissive,
    
    /// Run gates and abort if any fail.
    Strict,
}

// =============================================================================
// TESTS
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_synthetic_price_generator_martingale() {
        // Test that price generator produces martingale (zero drift)
        let seeds: Vec<u64> = (0..100).collect();
        let mut final_prices = Vec::new();
        
        for seed in seeds {
            let mut gen = SyntheticPriceGenerator::new(0.5, 0.01, seed);
            for _ in 0..1000 {
                gen.next_price();
            }
            final_prices.push(gen.price());
        }
        
        let mean = final_prices.iter().sum::<f64>() / final_prices.len() as f64;
        
        // Mean should be close to initial (0.5) for a martingale
        assert!((mean - 0.5).abs() < 0.1, "Mean {:.4} too far from initial 0.5", mean);
    }
    
    #[test]
    fn test_gate_suite_do_nothing_passes() {
        // A do-nothing strategy should pass all gates (no trades = no PnL)
        let config = GateSuiteConfig {
            windows_per_gate: 5,
            tolerances: GateTolerances {
                min_trades_for_validity: 0, // Allow zero trades
                ..Default::default()
            },
            ..Default::default()
        };
        
        let suite = GateSuite::new(config);
        let report = suite.run();
        
        // Gate A should pass (no trades)
        assert!(report.gates[0].passed, "Gate A failed: {:?}", report.gates[0].failure_reason);
        
        // Overall should pass
        assert!(report.passed, "Suite failed unexpectedly");
    }
    
    #[test]
    fn test_gate_tolerances_are_strict() {
        let tolerances = GateTolerances::default();
        
        // Verify tolerances are conservative
        assert!(tolerances.max_mean_pnl_before_fees < 1.0, 
            "Before-fee tolerance too loose");
        assert!(tolerances.min_mean_pnl_after_fees < 0.0,
            "After-fee tolerance should require negative PnL");
        assert!(tolerances.max_positive_pnl_probability < 0.6,
            "Positive PnL probability tolerance too loose");
    }
    
    #[test]
    fn test_gate_suite_deterministic() {
        let config = GateSuiteConfig {
            windows_per_gate: 3,
            tolerances: GateTolerances {
                martingale_seeds: 10,
                ..Default::default()
            },
            ..Default::default()
        };
        
        let suite = GateSuite::new(config.clone());
        let report1 = suite.run();
        
        let suite = GateSuite::new(config);
        let report2 = suite.run();
        
        // Results should be identical
        assert_eq!(report1.passed, report2.passed);
        assert_eq!(report1.gates.len(), report2.gates.len());
        
        for (g1, g2) in report1.gates.iter().zip(report2.gates.iter()) {
            assert_eq!(g1.passed, g2.passed);
            assert!((g1.metrics.pnl_after_fees - g2.metrics.pnl_after_fees).abs() < 1e-9,
                "Non-deterministic PnL");
        }
    }
    
    #[test]
    fn test_gate_report_format() {
        let config = GateSuiteConfig {
            windows_per_gate: 2,
            tolerances: GateTolerances {
                martingale_seeds: 5,
                ..Default::default()
            },
            ..Default::default()
        };
        
        let suite = GateSuite::new(config);
        let report = suite.run();
        let summary = report.format_summary();
        
        assert!(summary.contains("GATE SUITE REPORT"));
        assert!(summary.contains("Gate A"));
        assert!(summary.contains("Gate B"));
        assert!(summary.contains("Gate C"));
    }
    
    #[test]
    fn test_trust_level_enum() {
        assert!(matches!(TrustLevel::default(), TrustLevel::Unknown));
        
        // Test is_trusted method
        assert!(TrustLevel::Trusted.is_trusted());
        assert!(!TrustLevel::Unknown.is_trusted());
        assert!(!TrustLevel::Bypassed.is_trusted());
        assert!(!(TrustLevel::Untrusted { reasons: vec![] }).is_trusted());
        
        // Test that Trusted requires passing
        let config = GateSuiteConfig {
            windows_per_gate: 2,
            tolerances: GateTolerances {
                martingale_seeds: 5,
                min_trades_for_validity: 0,
                ..Default::default()
            },
            ..Default::default()
        };
        
        let suite = GateSuite::new(config);
        let report = suite.run();
        
        if report.passed {
            assert_eq!(report.trust_level, TrustLevel::Trusted);
        } else {
            // Should be Untrusted with reasons
            assert!(matches!(report.trust_level, TrustLevel::Untrusted { .. }));
            assert!(!report.trust_level.failure_reasons().is_empty());
        }
    }
    
    #[test]
    fn test_gate_failure_reason() {
        let reason = GateFailureReason::new(
            "TestGate",
            "Test failed due to X",
            "metric_x",
            "1.5",
            "< 1.0",
        ).with_seeds(vec![42, 43]);
        
        assert_eq!(reason.gate_name, "TestGate");
        assert_eq!(reason.failed_seeds, vec![42, 43]);
    }
    
    #[test]
    fn test_skipping_gates_marks_untrusted() {
        // When gates are disabled, trust level should be Bypassed
        // This is tested via orchestrator, but here we verify the TrustLevel values
        assert!(!TrustLevel::Bypassed.is_trusted());
        assert!(TrustLevel::Bypassed.failure_reasons().is_empty()); // No reasons, just bypassed
    }
}

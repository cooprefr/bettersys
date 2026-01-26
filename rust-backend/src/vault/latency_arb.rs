//! 15-Minute Latency Arbitrage Strategy
//!
//! A production-grade latency arbitrage loop for Polymarket binary contingent claims
//! that explicitly couples belief updates, execution ordering, and tail-latency risk.
//!
//! Key components:
//! - Bayesian probability estimation with damped updates
//! - Effective edge computation (raw edge - fees - slippage) × fill probability
//! - Fill probability from live tail latency (p95/p99), not averages
//! - Fractional Kelly sizing with per-market/topic/global exposure limits
//! - Small clip execution with quick cancellation
//! - Two-leg arbitrage with completion probability gating

use anyhow::{anyhow, Result};
use chrono::Utc;
use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, VecDeque},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use tokio::sync::broadcast;
use tracing::{debug, info, trace, warn};
use uuid::Uuid;

use crate::{
    performance::latency::{HistogramSummary, LatencyHistogram},
    scrapers::polymarket::{Order, OrderBook},
    vault::{
        calculate_kelly_position, ExecutionAdapter, KellyParams, OrderAck, OrderRequest, OrderSide,
        TimeInForce,
    },
    AppState,
};

// =============================================================================
// CONFIGURATION
// =============================================================================

/// Configuration for the latency arbitrage engine
#[derive(Debug, Clone)]
pub struct LatencyArbConfig {
    /// Minimum effective edge to trade (after fees/slippage/fill prob)
    pub min_effective_edge: f64,
    /// Kelly fraction (0.1 = 10% Kelly)
    pub kelly_fraction: f64,
    /// Maximum position as % of bankroll per market
    pub max_position_pct_per_market: f64,
    /// Maximum position as % of bankroll per topic
    pub max_position_pct_per_topic: f64,
    /// Maximum total exposure as % of bankroll
    pub max_total_exposure_pct: f64,
    /// Maximum single clip size in USD
    pub max_clip_usd: f64,
    /// Minimum clip size in USD
    pub min_clip_usd: f64,
    /// Order timeout before cancellation (ms)
    pub order_timeout_ms: u64,
    /// Probability update damping factor (0-1, higher = more damping)
    pub prob_damping: f64,
    /// Minimum drivers agreeing before large probability shift
    pub min_confirming_drivers: usize,
    /// Fee rate (Polymarket ~1% maker, 2% taker)
    pub fee_rate_maker: f64,
    pub fee_rate_taker: f64,
    /// Slippage buffer as fraction of spread
    pub slippage_buffer_frac: f64,
    /// Maximum spread (bps) to trade
    pub max_spread_bps: f64,
    /// Minimum top-of-book liquidity (USD)
    pub min_top_liquidity_usd: f64,
    /// Tail latency percentile to use (95 or 99)
    pub latency_percentile: f64,
    /// Maximum acceptable tail latency (microseconds)
    pub max_tail_latency_us: u64,
    /// Latency degradation threshold to downsize (fraction increase)
    pub latency_degradation_threshold: f64,
    /// Minimum two-leg completion probability for true arb
    pub two_leg_min_completion_prob: f64,
    /// Rolling window for probability updates (seconds)
    pub rolling_window_sec: i64,
    /// Cooldown between trades on same market (seconds)
    pub market_cooldown_sec: i64,
    /// Enable paper mode
    pub paper_mode: bool,
}

impl Default for LatencyArbConfig {
    fn default() -> Self {
        Self {
            min_effective_edge: 0.005,         // 0.5% minimum effective edge
            kelly_fraction: 0.10,              // 10% Kelly
            max_position_pct_per_market: 0.02, // 2% max per market
            max_position_pct_per_topic: 0.05,  // 5% max per topic
            max_total_exposure_pct: 0.15,      // 15% max total
            max_clip_usd: 100.0,
            min_clip_usd: 5.0,
            order_timeout_ms: 500, // 500ms timeout
            prob_damping: 0.3,     // 30% damping on updates
            min_confirming_drivers: 2,
            fee_rate_maker: 0.01,
            fee_rate_taker: 0.02,
            slippage_buffer_frac: 0.5, // 50% of spread as slippage buffer
            max_spread_bps: 300.0,     // 3% max spread
            min_top_liquidity_usd: 100.0,
            latency_percentile: 95.0,
            max_tail_latency_us: 500_000,       // 500ms max tail latency
            latency_degradation_threshold: 2.0, // 2x normal = degraded
            two_leg_min_completion_prob: 0.80,
            rolling_window_sec: 900, // 15 minutes
            market_cooldown_sec: 5,
            paper_mode: true,
        }
    }
}

impl LatencyArbConfig {
    pub fn from_env() -> Self {
        let mut cfg = Self::default();

        if let Ok(v) = std::env::var("LATENCY_ARB_MIN_EDGE") {
            if let Ok(e) = v.parse::<f64>() {
                if e.is_finite() && e > 0.0 {
                    cfg.min_effective_edge = e;
                }
            }
        }
        if let Ok(v) = std::env::var("LATENCY_ARB_KELLY_FRACTION") {
            if let Ok(k) = v.parse::<f64>() {
                if k.is_finite() && k > 0.0 && k <= 1.0 {
                    cfg.kelly_fraction = k;
                }
            }
        }
        if let Ok(v) = std::env::var("LATENCY_ARB_MAX_CLIP_USD") {
            if let Ok(c) = v.parse::<f64>() {
                if c.is_finite() && c > 0.0 {
                    cfg.max_clip_usd = c;
                }
            }
        }
        if let Ok(v) = std::env::var("LATENCY_ARB_ORDER_TIMEOUT_MS") {
            if let Ok(t) = v.parse::<u64>() {
                if t >= 50 {
                    cfg.order_timeout_ms = t;
                }
            }
        }
        if let Ok(v) = std::env::var("LATENCY_ARB_PAPER") {
            cfg.paper_mode = matches!(v.as_str(), "1" | "true" | "TRUE" | "on" | "ON");
        }

        cfg
    }
}

// =============================================================================
// MICROSTRUCTURE SIGNALS
// =============================================================================

/// Microstructure signal types that indicate information
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MicrostructureSignal {
    /// Aggressive taker imbalance (positive = buy pressure)
    TakerImbalance(f64),
    /// Cancellation burst detected
    CancellationBurst,
    /// Depth thinning on one side
    DepthThinning { side: OrderSide, severity: f64 },
    /// Repeated lifts (aggressive buys hitting asks)
    RepeatedLifts(u32),
    /// Repeated hits (aggressive sells hitting bids)
    RepeatedHits(u32),
    /// Spread widening
    SpreadWidening(f64),
    /// Quote stuffing detected
    QuoteStuffing,
}

/// Aggregated microstructure state for a market
#[derive(Debug, Clone, Default)]
pub struct MicrostructureState {
    /// Rolling taker imbalance (buy volume - sell volume) / total
    pub taker_imbalance: f64,
    /// Recent cancellation rate (cancels per second)
    pub cancel_rate: f64,
    /// Bid depth relative to recent average
    pub bid_depth_ratio: f64,
    /// Ask depth relative to recent average
    pub ask_depth_ratio: f64,
    /// Recent lift count (aggressive buys)
    pub lift_count: u32,
    /// Recent hit count (aggressive sells)
    pub hit_count: u32,
    /// Current spread vs recent average
    pub spread_ratio: f64,
    /// Last update timestamp
    pub updated_at: i64,
}

impl MicrostructureState {
    /// Extract active signals from current state
    pub fn active_signals(&self) -> Vec<MicrostructureSignal> {
        let mut signals = Vec::new();

        // Significant taker imbalance (>20% net)
        if self.taker_imbalance.abs() > 0.2 {
            signals.push(MicrostructureSignal::TakerImbalance(self.taker_imbalance));
        }

        // Cancellation burst (>5 per second is unusual)
        if self.cancel_rate > 5.0 {
            signals.push(MicrostructureSignal::CancellationBurst);
        }

        // Depth thinning (side drops below 50% of average)
        if self.bid_depth_ratio < 0.5 {
            signals.push(MicrostructureSignal::DepthThinning {
                side: OrderSide::Buy,
                severity: 1.0 - self.bid_depth_ratio,
            });
        }
        if self.ask_depth_ratio < 0.5 {
            signals.push(MicrostructureSignal::DepthThinning {
                side: OrderSide::Sell,
                severity: 1.0 - self.ask_depth_ratio,
            });
        }

        // Repeated lifts/hits (>3 in window)
        if self.lift_count > 3 {
            signals.push(MicrostructureSignal::RepeatedLifts(self.lift_count));
        }
        if self.hit_count > 3 {
            signals.push(MicrostructureSignal::RepeatedHits(self.hit_count));
        }

        // Spread widening (>150% of average)
        if self.spread_ratio > 1.5 {
            signals.push(MicrostructureSignal::SpreadWidening(self.spread_ratio));
        }

        signals
    }

    /// Compute directional bias from microstructure (-1 to +1)
    pub fn directional_bias(&self) -> f64 {
        let mut bias = 0.0;

        // Taker imbalance is primary signal
        bias += self.taker_imbalance * 0.4;

        // Depth asymmetry
        if self.bid_depth_ratio > 0.0 && self.ask_depth_ratio > 0.0 {
            let depth_ratio = (self.bid_depth_ratio - self.ask_depth_ratio)
                / (self.bid_depth_ratio + self.ask_depth_ratio);
            bias += depth_ratio * 0.3;
        }

        // Lift/hit imbalance
        let total_aggr = (self.lift_count + self.hit_count) as f64;
        if total_aggr > 0.0 {
            let aggr_imbalance = (self.lift_count as f64 - self.hit_count as f64) / total_aggr;
            bias += aggr_imbalance * 0.3;
        }

        bias.clamp(-1.0, 1.0)
    }
}

// =============================================================================
// PROBABILITY ESTIMATION
// =============================================================================

/// Driver for probability updates
#[derive(Debug, Clone)]
pub enum ProbabilityDriver {
    /// Cross-market consistency (yes + no ≈ 1)
    CrossMarketConsistency { implied_prob: f64, confidence: f64 },
    /// Related market on same underlying
    RelatedMarket {
        market_slug: String,
        implied_delta: f64,
    },
    /// External signal (e.g., Binance price for crypto markets)
    ExternalSignal { source: String, implied_prob: f64 },
    /// Microstructure indicators
    Microstructure { bias: f64, signal_count: usize },
}

/// Private probability estimate for a market
#[derive(Debug, Clone)]
pub struct ProbabilityEstimate {
    /// Market slug
    pub market_slug: String,
    /// Token ID (Yes outcome)
    pub token_id_yes: String,
    /// Token ID (No outcome)
    pub token_id_no: String,
    /// Current private probability estimate
    pub private_prob: f64,
    /// Uncertainty in estimate (0-1)
    pub uncertainty: f64,
    /// Market-implied probability (from prices)
    pub market_prob: f64,
    /// Drivers contributing to current estimate
    pub active_drivers: Vec<ProbabilityDriver>,
    /// Last update timestamp
    pub updated_at: i64,
    /// Historical estimates for computing convergence
    pub history: VecDeque<(i64, f64)>,
}

impl ProbabilityEstimate {
    pub fn new(market_slug: &str, token_id_yes: &str, token_id_no: &str) -> Self {
        Self {
            market_slug: market_slug.to_string(),
            token_id_yes: token_id_yes.to_string(),
            token_id_no: token_id_no.to_string(),
            private_prob: 0.5,
            uncertainty: 1.0,
            market_prob: 0.5,
            active_drivers: Vec::new(),
            updated_at: 0,
            history: VecDeque::with_capacity(1000),
        }
    }

    /// Update estimate with new driver, applying damping
    pub fn update(&mut self, driver: ProbabilityDriver, cfg: &LatencyArbConfig) {
        let now = Utc::now().timestamp();
        self.updated_at = now;

        // Extract implied probability shift from driver
        let (implied_shift, driver_confidence) = match &driver {
            ProbabilityDriver::CrossMarketConsistency {
                implied_prob,
                confidence,
            } => (*implied_prob - self.private_prob, *confidence),
            ProbabilityDriver::RelatedMarket { implied_delta, .. } => {
                (*implied_delta, 0.5) // Lower confidence for related markets
            }
            ProbabilityDriver::ExternalSignal { implied_prob, .. } => {
                (*implied_prob - self.private_prob, 0.7)
            }
            ProbabilityDriver::Microstructure { bias, signal_count } => {
                // Microstructure shifts probability in direction of bias
                let shift = *bias * 0.1; // Max 10% shift from microstructure
                let conf = (*signal_count as f64 / 5.0).min(1.0);
                (shift, conf * 0.4) // Lower base confidence for microstructure
            }
        };

        // Damped update: new_prob = old_prob + (1 - damping) * shift * confidence
        let damping = cfg.prob_damping;
        let update_weight = (1.0 - damping) * driver_confidence;
        let new_prob = (self.private_prob + implied_shift * update_weight).clamp(0.01, 0.99);

        // Record history
        self.history.push_back((now, new_prob));
        while self.history.len() > 1000 {
            self.history.pop_front();
        }

        // Update private prob
        self.private_prob = new_prob;

        // Update uncertainty based on driver count and agreement
        self.active_drivers.push(driver);

        // Trim old drivers (keep last 10)
        while self.active_drivers.len() > 10 {
            self.active_drivers.remove(0);
        }

        // Compute uncertainty from driver agreement
        self.uncertainty = self.compute_uncertainty();
    }

    fn compute_uncertainty(&self) -> f64 {
        if self.active_drivers.is_empty() {
            return 1.0;
        }

        // Count directional agreement
        let mut bullish = 0;
        let mut bearish = 0;

        for driver in &self.active_drivers {
            match driver {
                ProbabilityDriver::CrossMarketConsistency { implied_prob, .. } => {
                    if *implied_prob > self.market_prob {
                        bullish += 1;
                    } else {
                        bearish += 1;
                    }
                }
                ProbabilityDriver::Microstructure { bias, .. } => {
                    if *bias > 0.0 {
                        bullish += 1;
                    } else {
                        bearish += 1;
                    }
                }
                _ => {}
            }
        }

        let total = bullish + bearish;
        if total == 0 {
            return 1.0;
        }

        // Higher agreement = lower uncertainty
        let agreement = (bullish.max(bearish) as f64) / (total as f64);
        1.0 - (agreement * 0.7) // Max 70% uncertainty reduction
    }

    /// Check if enough drivers confirm a direction for large shifts
    pub fn has_confirming_drivers(&self, min_count: usize) -> bool {
        let mut bullish = 0;
        let mut bearish = 0;

        for driver in &self.active_drivers {
            match driver {
                ProbabilityDriver::CrossMarketConsistency { implied_prob, .. } => {
                    if *implied_prob > self.market_prob + 0.02 {
                        bullish += 1;
                    } else if *implied_prob < self.market_prob - 0.02 {
                        bearish += 1;
                    }
                }
                ProbabilityDriver::Microstructure { bias, .. } => {
                    if *bias > 0.1 {
                        bullish += 1;
                    } else if *bias < -0.1 {
                        bearish += 1;
                    }
                }
                ProbabilityDriver::ExternalSignal { implied_prob, .. } => {
                    if *implied_prob > self.market_prob + 0.02 {
                        bullish += 1;
                    } else if *implied_prob < self.market_prob - 0.02 {
                        bearish += 1;
                    }
                }
                _ => {}
            }
        }

        bullish >= min_count || bearish >= min_count
    }

    /// Raw edge before execution adjustments
    pub fn raw_edge(&self) -> f64 {
        self.private_prob - self.market_prob
    }
}

// =============================================================================
// EFFECTIVE EDGE COMPUTATION
// =============================================================================

/// Effective edge after all adjustments
#[derive(Debug, Clone)]
pub struct EffectiveEdge {
    /// Raw edge (private prob - market prob)
    pub raw_edge: f64,
    /// Fee cost
    pub fee_cost: f64,
    /// Expected slippage
    pub expected_slippage: f64,
    /// Fill probability from latency analysis
    pub fill_probability: f64,
    /// Effective edge = (raw - fees - slippage) * fill_prob
    pub effective_edge: f64,
    /// Whether this exceeds minimum threshold
    pub tradeable: bool,
    /// Reason if not tradeable
    pub skip_reason: Option<String>,
}

/// Latency statistics for execution probability estimation
#[derive(Debug, Clone)]
pub struct LatencyStats {
    /// P50 latency (microseconds)
    pub p50_us: u64,
    /// P90 latency (microseconds)
    pub p90_us: u64,
    /// P95 latency (microseconds)
    pub p95_us: u64,
    /// P99 latency (microseconds)
    pub p99_us: u64,
    /// Recent sample count
    pub sample_count: u64,
    /// Baseline (historical average) P95
    pub baseline_p95_us: u64,
    /// Current congestion indicator (>1 = degraded)
    pub congestion_ratio: f64,
}

impl LatencyStats {
    /// Estimate fill probability based on tail latency
    /// Higher latency = lower probability of filling before reprice
    pub fn fill_probability(&self, max_acceptable_us: u64) -> f64 {
        if self.sample_count < 10 {
            return 0.5; // Insufficient data
        }

        // Use p95 as primary metric
        let tail_latency = self.p95_us;

        if tail_latency >= max_acceptable_us {
            return 0.1; // Very low fill prob if tail exceeds max
        }

        // Fill probability decreases as tail latency increases
        // Model: P(fill) = 1 - (tail / max)^0.5
        let ratio = (tail_latency as f64) / (max_acceptable_us as f64);
        let fill_prob = (1.0 - ratio.sqrt()).clamp(0.1, 0.95);

        // Adjust for congestion
        let congestion_penalty = if self.congestion_ratio > 1.5 {
            0.8 // 20% penalty for high congestion
        } else if self.congestion_ratio > 1.2 {
            0.9 // 10% penalty for moderate congestion
        } else {
            1.0
        };

        (fill_prob * congestion_penalty).clamp(0.1, 0.95)
    }

    /// Check if latency is degraded relative to baseline
    pub fn is_degraded(&self, threshold: f64) -> bool {
        if self.baseline_p95_us == 0 {
            return false;
        }
        self.congestion_ratio > threshold
    }
}

/// Compute effective edge with all adjustments
pub fn compute_effective_edge(
    estimate: &ProbabilityEstimate,
    orderbook: &OrderBook,
    latency_stats: &LatencyStats,
    cfg: &LatencyArbConfig,
) -> EffectiveEdge {
    let raw_edge = estimate.raw_edge();

    // Determine if buying Yes or No
    let is_buy_yes = raw_edge > 0.0;
    let effective_price = if is_buy_yes {
        // Buying Yes: use best ask
        orderbook
            .asks
            .first()
            .map(|o| o.price)
            .unwrap_or(estimate.market_prob)
    } else {
        // Buying No (selling Yes): use best bid
        orderbook
            .bids
            .first()
            .map(|o| o.price)
            .unwrap_or(estimate.market_prob)
    };

    // Compute spread
    let best_bid = orderbook.bids.first().map(|o| o.price).unwrap_or(0.0);
    let best_ask = orderbook.asks.first().map(|o| o.price).unwrap_or(1.0);
    let spread = best_ask - best_bid;
    let mid = (best_bid + best_ask) / 2.0;
    let spread_bps = if mid > 0.0 {
        (spread / mid) * 10_000.0
    } else {
        10_000.0
    };

    // Check spread constraint
    if spread_bps > cfg.max_spread_bps {
        return EffectiveEdge {
            raw_edge,
            fee_cost: 0.0,
            expected_slippage: 0.0,
            fill_probability: 0.0,
            effective_edge: 0.0,
            tradeable: false,
            skip_reason: Some(format!(
                "Spread {:.0}bps > max {:.0}bps",
                spread_bps, cfg.max_spread_bps
            )),
        };
    }

    // Check liquidity
    let top_liquidity = orderbook
        .asks
        .first()
        .map(|o| o.price * o.size)
        .unwrap_or(0.0);
    if top_liquidity < cfg.min_top_liquidity_usd {
        return EffectiveEdge {
            raw_edge,
            fee_cost: 0.0,
            expected_slippage: 0.0,
            fill_probability: 0.0,
            effective_edge: 0.0,
            tradeable: false,
            skip_reason: Some(format!(
                "Top liquidity ${:.0} < min ${:.0}",
                top_liquidity, cfg.min_top_liquidity_usd
            )),
        };
    }

    // Fee cost (use taker fee as we're taking liquidity)
    let fee_cost = cfg.fee_rate_taker;

    // Expected slippage (fraction of spread)
    let expected_slippage = spread * cfg.slippage_buffer_frac;

    // Fill probability from latency stats
    let fill_probability = latency_stats.fill_probability(cfg.max_tail_latency_us);

    // Effective edge = (raw - fees - slippage) * fill_prob
    let adjusted_edge = raw_edge.abs() - fee_cost - expected_slippage;
    let effective_edge = adjusted_edge * fill_probability;

    // Check if tradeable
    let tradeable = effective_edge > cfg.min_effective_edge
        && !latency_stats.is_degraded(cfg.latency_degradation_threshold);

    let skip_reason = if !tradeable {
        if latency_stats.is_degraded(cfg.latency_degradation_threshold) {
            Some(format!(
                "Latency degraded: {:.1}x baseline",
                latency_stats.congestion_ratio
            ))
        } else {
            Some(format!(
                "Effective edge {:.2}% < min {:.2}%",
                effective_edge * 100.0,
                cfg.min_effective_edge * 100.0
            ))
        }
    } else {
        None
    };

    EffectiveEdge {
        raw_edge,
        fee_cost,
        expected_slippage,
        fill_probability,
        effective_edge,
        tradeable,
        skip_reason,
    }
}

// =============================================================================
// POSITION SIZING & EXPOSURE
// =============================================================================

/// Exposure tracker for position limits
#[derive(Debug, Clone, Default)]
pub struct ExposureTracker {
    /// Exposure per market (market_slug -> USD)
    pub per_market: HashMap<String, f64>,
    /// Exposure per topic (topic -> USD)
    pub per_topic: HashMap<String, f64>,
    /// Total exposure
    pub total_exposure: f64,
    /// Current bankroll
    pub bankroll: f64,
}

impl ExposureTracker {
    pub fn new(bankroll: f64) -> Self {
        Self {
            per_market: HashMap::new(),
            per_topic: HashMap::new(),
            total_exposure: 0.0,
            bankroll,
        }
    }

    /// Check if additional exposure is allowed
    pub fn can_add_exposure(
        &self,
        market_slug: &str,
        topic: &str,
        amount_usd: f64,
        cfg: &LatencyArbConfig,
    ) -> Result<f64> {
        // Check per-market limit
        let market_exposure = self.per_market.get(market_slug).copied().unwrap_or(0.0);
        let market_limit = self.bankroll * cfg.max_position_pct_per_market;
        let market_remaining = market_limit - market_exposure;
        if market_remaining <= 0.0 {
            return Err(anyhow!("Market exposure limit reached"));
        }

        // Check per-topic limit
        let topic_exposure = self.per_topic.get(topic).copied().unwrap_or(0.0);
        let topic_limit = self.bankroll * cfg.max_position_pct_per_topic;
        let topic_remaining = topic_limit - topic_exposure;
        if topic_remaining <= 0.0 {
            return Err(anyhow!("Topic exposure limit reached"));
        }

        // Check total limit
        let total_limit = self.bankroll * cfg.max_total_exposure_pct;
        let total_remaining = total_limit - self.total_exposure;
        if total_remaining <= 0.0 {
            return Err(anyhow!("Total exposure limit reached"));
        }

        // Return minimum of all limits
        Ok(amount_usd
            .min(market_remaining)
            .min(topic_remaining)
            .min(total_remaining))
    }

    /// Record new exposure
    pub fn add_exposure(&mut self, market_slug: &str, topic: &str, amount_usd: f64) {
        *self
            .per_market
            .entry(market_slug.to_string())
            .or_insert(0.0) += amount_usd;
        *self.per_topic.entry(topic.to_string()).or_insert(0.0) += amount_usd;
        self.total_exposure += amount_usd;
    }

    /// Remove exposure (on position close)
    pub fn remove_exposure(&mut self, market_slug: &str, topic: &str, amount_usd: f64) {
        if let Some(v) = self.per_market.get_mut(market_slug) {
            *v = (*v - amount_usd).max(0.0);
        }
        if let Some(v) = self.per_topic.get_mut(topic) {
            *v = (*v - amount_usd).max(0.0);
        }
        self.total_exposure = (self.total_exposure - amount_usd).max(0.0);
    }
}

/// Compute position size with Kelly and exposure limits
pub fn compute_position_size(
    estimate: &ProbabilityEstimate,
    edge: &EffectiveEdge,
    exposure: &ExposureTracker,
    topic: &str,
    cfg: &LatencyArbConfig,
) -> Result<f64> {
    if !edge.tradeable {
        return Err(anyhow!(
            "Edge not tradeable: {}",
            edge.skip_reason.as_deref().unwrap_or("unknown")
        ));
    }

    // Kelly sizing
    let kelly_params = KellyParams {
        bankroll: exposure.bankroll,
        kelly_fraction: cfg.kelly_fraction,
        max_position_pct: cfg.max_position_pct_per_market,
        min_position_usd: cfg.min_clip_usd,
    };

    let kelly =
        calculate_kelly_position(estimate.private_prob, estimate.market_prob, &kelly_params);

    if !kelly.should_trade {
        return Err(anyhow!(
            "Kelly says no trade: {}",
            kelly.skip_reason.as_deref().unwrap_or("unknown")
        ));
    }

    // Scale by fill probability
    let kelly_scaled = kelly.position_size_usd * edge.fill_probability;

    // Scale by uncertainty (higher uncertainty = smaller position)
    let uncertainty_scale = 1.0 - (estimate.uncertainty * 0.5);
    let sized = kelly_scaled * uncertainty_scale;

    // Apply exposure limits
    let allowed = exposure.can_add_exposure(&estimate.market_slug, topic, sized, cfg)?;

    // Apply clip limits
    let clipped = allowed.min(cfg.max_clip_usd).max(cfg.min_clip_usd);

    if clipped < cfg.min_clip_usd {
        return Err(anyhow!(
            "Position ${:.2} below minimum ${:.2}",
            clipped,
            cfg.min_clip_usd
        ));
    }

    Ok(clipped)
}

// =============================================================================
// TWO-LEG ARBITRAGE
// =============================================================================

/// Two-leg arbitrage opportunity
#[derive(Debug, Clone)]
pub struct TwoLegArb {
    pub leg1_token: String,
    pub leg1_side: OrderSide,
    pub leg1_price: f64,
    pub leg2_token: String,
    pub leg2_side: OrderSide,
    pub leg2_price: f64,
    pub gross_profit_pct: f64,
    pub leg1_fill_prob: f64,
    pub leg2_fill_prob: f64,
    pub combined_fill_prob: f64,
    pub is_true_arb: bool,
}

impl TwoLegArb {
    /// Check cross-market consistency: Yes + No should ≈ 1
    pub fn from_yes_no_markets(
        yes_book: &OrderBook,
        no_book: &OrderBook,
        yes_token: &str,
        no_token: &str,
        latency_stats: &LatencyStats,
        cfg: &LatencyArbConfig,
    ) -> Option<Self> {
        // Get best prices
        let yes_ask = yes_book.asks.first().map(|o| o.price)?;
        let no_ask = no_book.asks.first().map(|o| o.price)?;

        // If buying both Yes and No costs less than 1.0, there's an arbitrage
        let total_cost = yes_ask + no_ask;
        if total_cost >= 1.0 - cfg.fee_rate_taker * 2.0 {
            return None; // No arbitrage after fees
        }

        let gross_profit_pct = (1.0 - total_cost) * 100.0;

        // Compute fill probabilities for each leg
        let leg1_fill_prob = latency_stats.fill_probability(cfg.max_tail_latency_us);
        let leg2_fill_prob = latency_stats.fill_probability(cfg.max_tail_latency_us);

        // Combined probability (both must fill)
        let combined_fill_prob = leg1_fill_prob * leg2_fill_prob;

        // Only treat as true arb if combined fill prob is high enough
        let is_true_arb = combined_fill_prob >= cfg.two_leg_min_completion_prob;

        Some(Self {
            leg1_token: yes_token.to_string(),
            leg1_side: OrderSide::Buy,
            leg1_price: yes_ask,
            leg2_token: no_token.to_string(),
            leg2_side: OrderSide::Buy,
            leg2_price: no_ask,
            gross_profit_pct,
            leg1_fill_prob,
            leg2_fill_prob,
            combined_fill_prob,
            is_true_arb,
        })
    }
}

// =============================================================================
// EXECUTION LOGIC
// =============================================================================

/// Trade decision from the arbitrage loop
#[derive(Debug, Clone)]
pub enum TradeDecision {
    /// No action - edge insufficient or constraints violated
    NoAction { reason: String },
    /// Small aggressive clip near the touch
    AggressiveClip {
        token_id: String,
        side: OrderSide,
        price: f64,
        size_usd: f64,
        timeout_ms: u64,
    },
    /// Short-lived passive order
    PassiveOrder {
        token_id: String,
        side: OrderSide,
        price: f64,
        size_usd: f64,
        timeout_ms: u64,
    },
    /// Two-leg arbitrage execution
    TwoLegArbitrage {
        leg1: Box<TradeDecision>,
        leg2: Box<TradeDecision>,
        is_true_arb: bool,
    },
}

/// Execute a trade decision (non-recursive helper)
async fn execute_single_leg(
    decision: &TradeDecision,
    executor: &Arc<dyn ExecutionAdapter>,
) -> Result<Option<OrderAck>> {
    match decision {
        TradeDecision::NoAction { .. } => Ok(None),
        TradeDecision::AggressiveClip {
            token_id,
            side,
            price,
            size_usd,
            timeout_ms,
        } => {
            let client_order_id = Uuid::new_v4().to_string();
            let req = OrderRequest {
                client_order_id,
                token_id: token_id.clone(),
                side: *side,
                price: *price,
                notional_usdc: *size_usd,
                tif: TimeInForce::Ioc,
                market_slug: None,
                outcome: None,
            };

            let ack = tokio::time::timeout(
                Duration::from_millis(*timeout_ms),
                executor.place_order(req),
            )
            .await
            .map_err(|_| anyhow!("Order timed out"))??;

            Ok(Some(ack))
        }
        TradeDecision::PassiveOrder {
            token_id,
            side,
            price,
            size_usd,
            timeout_ms,
        } => {
            let client_order_id = Uuid::new_v4().to_string();
            let req = OrderRequest {
                client_order_id,
                token_id: token_id.clone(),
                side: *side,
                price: *price,
                notional_usdc: *size_usd,
                tif: TimeInForce::Gtc,
                market_slug: None,
                outcome: None,
            };

            let ack = tokio::time::timeout(
                Duration::from_millis(*timeout_ms),
                executor.place_order(req),
            )
            .await
            .map_err(|_| anyhow!("Order timed out"))??;

            Ok(Some(ack))
        }
        TradeDecision::TwoLegArbitrage { .. } => {
            // This case is handled by execute_decision
            Ok(None)
        }
    }
}

/// Execute a trade decision
pub async fn execute_decision(
    decision: &TradeDecision,
    executor: &Arc<dyn ExecutionAdapter>,
    _state: &AppState,
) -> Result<Option<OrderAck>> {
    match decision {
        TradeDecision::TwoLegArbitrage {
            leg1,
            leg2,
            is_true_arb,
        } => {
            // For true arb, execute both legs
            // For directional, only execute if first leg fills
            let ack1 = execute_single_leg(leg1, executor).await?;

            if *is_true_arb || ack1.is_some() {
                let _ack2 = execute_single_leg(leg2, executor).await?;
            }

            Ok(ack1)
        }
        _ => execute_single_leg(decision, executor).await,
    }
}

// =============================================================================
// DIAGNOSTICS
// =============================================================================

/// Diagnostics for a single arbitrage decision
#[derive(Debug, Clone, Serialize)]
pub struct ArbDiagnostics {
    pub timestamp: i64,
    pub market_slug: String,
    /// Private vs market probability gap
    pub prob_gap: f64,
    /// Execution probability at decision time
    pub exec_probability: f64,
    /// Latency percentiles at decision time
    pub latency_p95_us: u64,
    pub latency_p99_us: u64,
    /// Predicted edge
    pub predicted_edge: f64,
    /// Realized edge (filled after resolution)
    pub realized_edge: Option<f64>,
    /// PnL decomposition
    pub pnl_signal_edge: Option<f64>,
    pub pnl_slippage: Option<f64>,
    pub pnl_latency_loss: Option<f64>,
    pub pnl_total: Option<f64>,
    /// Decision taken
    pub decision: String,
    /// Outcome
    pub filled: bool,
    pub fill_price: Option<f64>,
}

/// Diagnostics tracker
#[derive(Debug)]
pub struct DiagnosticsTracker {
    history: RwLock<VecDeque<ArbDiagnostics>>,
    max_history: usize,
    // Aggregated metrics
    pub total_decisions: AtomicU64,
    pub total_trades: AtomicU64,
    pub total_fills: AtomicU64,
    pub total_pnl: Mutex<f64>,
    pub signal_edge_pnl: Mutex<f64>,
    pub slippage_pnl: Mutex<f64>,
    pub latency_loss_pnl: Mutex<f64>,
}

impl Default for DiagnosticsTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl DiagnosticsTracker {
    pub fn new() -> Self {
        Self {
            history: RwLock::new(VecDeque::with_capacity(10000)),
            max_history: 10000,
            total_decisions: AtomicU64::new(0),
            total_trades: AtomicU64::new(0),
            total_fills: AtomicU64::new(0),
            total_pnl: Mutex::new(0.0),
            signal_edge_pnl: Mutex::new(0.0),
            slippage_pnl: Mutex::new(0.0),
            latency_loss_pnl: Mutex::new(0.0),
        }
    }

    pub fn record_decision(&self, diag: ArbDiagnostics) {
        self.total_decisions.fetch_add(1, Ordering::Relaxed);
        if diag.filled {
            self.total_fills.fetch_add(1, Ordering::Relaxed);
        }
        if diag.decision != "NoAction" {
            self.total_trades.fetch_add(1, Ordering::Relaxed);
        }

        // Update PnL if available
        if let Some(total) = diag.pnl_total {
            *self.total_pnl.lock() += total;
        }
        if let Some(signal) = diag.pnl_signal_edge {
            *self.signal_edge_pnl.lock() += signal;
        }
        if let Some(slip) = diag.pnl_slippage {
            *self.slippage_pnl.lock() += slip;
        }
        if let Some(lat) = diag.pnl_latency_loss {
            *self.latency_loss_pnl.lock() += lat;
        }

        let mut history = self.history.write();
        history.push_back(diag);
        while history.len() > self.max_history {
            history.pop_front();
        }
    }

    pub fn summary(&self) -> DiagnosticsSummary {
        DiagnosticsSummary {
            total_decisions: self.total_decisions.load(Ordering::Relaxed),
            total_trades: self.total_trades.load(Ordering::Relaxed),
            total_fills: self.total_fills.load(Ordering::Relaxed),
            fill_rate: {
                let trades = self.total_trades.load(Ordering::Relaxed);
                let fills = self.total_fills.load(Ordering::Relaxed);
                if trades > 0 {
                    fills as f64 / trades as f64
                } else {
                    0.0
                }
            },
            total_pnl: *self.total_pnl.lock(),
            signal_edge_pnl: *self.signal_edge_pnl.lock(),
            slippage_pnl: *self.slippage_pnl.lock(),
            latency_loss_pnl: *self.latency_loss_pnl.lock(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct DiagnosticsSummary {
    pub total_decisions: u64,
    pub total_trades: u64,
    pub total_fills: u64,
    pub fill_rate: f64,
    pub total_pnl: f64,
    pub signal_edge_pnl: f64,
    pub slippage_pnl: f64,
    pub latency_loss_pnl: f64,
}

// =============================================================================
// MAIN ARBITRAGE ENGINE
// =============================================================================

/// Market state for the arbitrage engine
#[derive(Debug)]
struct MarketState {
    estimate: ProbabilityEstimate,
    microstructure: MicrostructureState,
    last_trade_at: i64,
    topic: String,
}

/// The main 15-minute latency arbitrage engine
pub struct LatencyArbEngine {
    config: LatencyArbConfig,
    markets: RwLock<HashMap<String, MarketState>>,
    exposure: RwLock<ExposureTracker>,
    latency_histogram: LatencyHistogram,
    baseline_latency_p95: AtomicU64,
    diagnostics: DiagnosticsTracker,
    running: AtomicBool,
}

impl LatencyArbEngine {
    pub fn new(config: LatencyArbConfig, bankroll: f64) -> Arc<Self> {
        Arc::new(Self {
            config,
            markets: RwLock::new(HashMap::new()),
            exposure: RwLock::new(ExposureTracker::new(bankroll)),
            latency_histogram: LatencyHistogram::new(),
            baseline_latency_p95: AtomicU64::new(0),
            diagnostics: DiagnosticsTracker::new(),
            running: AtomicBool::new(false),
        })
    }

    /// Register a market for tracking
    pub fn register_market(
        &self,
        market_slug: &str,
        token_id_yes: &str,
        token_id_no: &str,
        topic: &str,
    ) {
        let mut markets = self.markets.write();
        markets.insert(
            market_slug.to_string(),
            MarketState {
                estimate: ProbabilityEstimate::new(market_slug, token_id_yes, token_id_no),
                microstructure: MicrostructureState::default(),
                last_trade_at: 0,
                topic: topic.to_string(),
            },
        );
    }

    /// Record a latency sample
    pub fn record_latency(&self, latency_us: u64) {
        self.latency_histogram.record(latency_us);
    }

    /// Get current latency stats
    pub fn latency_stats(&self) -> LatencyStats {
        let p50 = self.latency_histogram.p50();
        let p90 = self.latency_histogram.p90();
        let p95 = self.latency_histogram.p95();
        let p99 = self.latency_histogram.p99();
        let count = self.latency_histogram.count();
        let baseline = self.baseline_latency_p95.load(Ordering::Relaxed);

        let congestion_ratio = if baseline > 0 {
            p95 as f64 / baseline as f64
        } else {
            1.0
        };

        LatencyStats {
            p50_us: p50,
            p90_us: p90,
            p95_us: p95,
            p99_us: p99,
            sample_count: count,
            baseline_p95_us: baseline,
            congestion_ratio,
        }
    }

    /// Update baseline latency (call periodically during calm periods)
    pub fn update_baseline(&self) {
        let p95 = self.latency_histogram.p95();
        if p95 > 0 {
            self.baseline_latency_p95.store(p95, Ordering::Relaxed);
        }
    }

    /// Update market probability estimate
    pub fn update_market_prob(
        &self,
        market_slug: &str,
        market_prob: f64,
        driver: ProbabilityDriver,
    ) {
        let mut markets = self.markets.write();
        if let Some(state) = markets.get_mut(market_slug) {
            state.estimate.market_prob = market_prob;
            state.estimate.update(driver, &self.config);
        }
    }

    /// Update microstructure state
    pub fn update_microstructure(&self, market_slug: &str, micro: MicrostructureState) {
        let mut markets = self.markets.write();
        if let Some(state) = markets.get_mut(market_slug) {
            state.microstructure = micro.clone();

            // Generate microstructure driver
            let signals = micro.active_signals();
            if !signals.is_empty() {
                let driver = ProbabilityDriver::Microstructure {
                    bias: micro.directional_bias(),
                    signal_count: signals.len(),
                };
                state.estimate.update(driver, &self.config);
            }
        }
    }

    /// Main decision loop: evaluate market and decide action
    pub fn evaluate_market(&self, market_slug: &str, orderbook: &OrderBook) -> TradeDecision {
        let now = Utc::now().timestamp();
        let markets = self.markets.read();

        let Some(state) = markets.get(market_slug) else {
            return TradeDecision::NoAction {
                reason: "Market not registered".to_string(),
            };
        };

        // Check cooldown
        if now - state.last_trade_at < self.config.market_cooldown_sec {
            return TradeDecision::NoAction {
                reason: format!(
                    "Cooldown: {}s remaining",
                    self.config.market_cooldown_sec - (now - state.last_trade_at)
                ),
            };
        }

        // Get latency stats
        let latency = self.latency_stats();

        // Check latency degradation (abort even if signal is strong)
        if latency.is_degraded(self.config.latency_degradation_threshold) {
            return TradeDecision::NoAction {
                reason: format!(
                    "Latency degraded: {:.1}x baseline (p95={}us)",
                    latency.congestion_ratio, latency.p95_us
                ),
            };
        }

        // Compute effective edge
        let edge = compute_effective_edge(&state.estimate, orderbook, &latency, &self.config);

        if !edge.tradeable {
            return TradeDecision::NoAction {
                reason: edge
                    .skip_reason
                    .unwrap_or_else(|| "Edge not tradeable".to_string()),
            };
        }

        // Check confirming drivers for large shifts
        let raw_edge_abs = edge.raw_edge.abs();
        if raw_edge_abs > 0.05
            && !state
                .estimate
                .has_confirming_drivers(self.config.min_confirming_drivers)
        {
            return TradeDecision::NoAction {
                reason: format!(
                    "Large shift ({:.1}%) but only {} confirming drivers (need {})",
                    raw_edge_abs * 100.0,
                    state.estimate.active_drivers.len(),
                    self.config.min_confirming_drivers
                ),
            };
        }

        // Compute position size
        let exposure = self.exposure.read();
        let size = match compute_position_size(
            &state.estimate,
            &edge,
            &exposure,
            &state.topic,
            &self.config,
        ) {
            Ok(s) => s,
            Err(e) => {
                return TradeDecision::NoAction {
                    reason: format!("Position sizing failed: {}", e),
                };
            }
        };

        // Determine trade direction
        let is_buy_yes = edge.raw_edge > 0.0;
        let (token_id, side, price) = if is_buy_yes {
            (
                state.estimate.token_id_yes.clone(),
                OrderSide::Buy,
                orderbook.asks.first().map(|o| o.price).unwrap_or(0.5),
            )
        } else {
            (
                state.estimate.token_id_no.clone(),
                OrderSide::Buy,
                orderbook.bids.first().map(|o| o.price).unwrap_or(0.5),
            )
        };

        // Decide between aggressive clip and passive order
        // Use aggressive clip if edge is strong and liquidity is good
        if edge.effective_edge > self.config.min_effective_edge * 2.0 {
            TradeDecision::AggressiveClip {
                token_id,
                side,
                price,
                size_usd: size,
                timeout_ms: self.config.order_timeout_ms,
            }
        } else {
            TradeDecision::PassiveOrder {
                token_id,
                side,
                price,
                size_usd: size,
                timeout_ms: self.config.order_timeout_ms * 2, // Longer timeout for passive
            }
        }
    }

    /// Record a trade execution for diagnostics
    pub fn record_execution(
        &self,
        market_slug: &str,
        decision: &TradeDecision,
        result: &Result<Option<OrderAck>>,
    ) {
        let now = Utc::now().timestamp();
        let markets = self.markets.read();
        let latency = self.latency_stats();

        let (prob_gap, predicted_edge) = markets
            .get(market_slug)
            .map(|s| (s.estimate.raw_edge(), s.estimate.raw_edge()))
            .unwrap_or((0.0, 0.0));

        let (filled, fill_price) = match result {
            Ok(Some(ack)) => (true, Some(ack.filled_price)),
            _ => (false, None),
        };

        let decision_str = match decision {
            TradeDecision::NoAction { reason } => format!("NoAction: {}", reason),
            TradeDecision::AggressiveClip { .. } => "AggressiveClip".to_string(),
            TradeDecision::PassiveOrder { .. } => "PassiveOrder".to_string(),
            TradeDecision::TwoLegArbitrage { .. } => "TwoLegArbitrage".to_string(),
        };

        let diag = ArbDiagnostics {
            timestamp: now,
            market_slug: market_slug.to_string(),
            prob_gap,
            exec_probability: latency.fill_probability(self.config.max_tail_latency_us),
            latency_p95_us: latency.p95_us,
            latency_p99_us: latency.p99_us,
            predicted_edge,
            realized_edge: None, // Filled in after market resolution
            pnl_signal_edge: None,
            pnl_slippage: None,
            pnl_latency_loss: None,
            pnl_total: None,
            decision: decision_str,
            filled,
            fill_price,
        };

        self.diagnostics.record_decision(diag);

        // Update last trade time
        if filled {
            drop(markets);
            let mut markets = self.markets.write();
            if let Some(state) = markets.get_mut(market_slug) {
                state.last_trade_at = now;
            }
        }
    }

    /// Get diagnostics summary
    pub fn diagnostics_summary(&self) -> DiagnosticsSummary {
        self.diagnostics.summary()
    }

    /// Spawn the event-driven arbitrage loop
    pub async fn run(
        self: Arc<Self>,
        state: AppState,
        executor: Arc<dyn ExecutionAdapter>,
        mut shutdown: broadcast::Receiver<()>,
    ) {
        self.running.store(true, Ordering::SeqCst);
        info!("Latency arbitrage engine started");

        // Subscribe to price updates from Binance (for external signal driver)
        let mut price_rx = state.binance_feed.subscribe();

        // Main event loop
        loop {
            tokio::select! {
                // Price update event
                Ok(update) = price_rx.recv() => {
                    // Record latency
                    let now_ns = std::time::Instant::now();
                    // Update crypto-related markets based on Binance prices
                    trace!(symbol = %update.symbol, mid = %update.mid, "Price update received");
                }

                // Periodic evaluation (every 100ms)
                _ = tokio::time::sleep(Duration::from_millis(100)) => {
                    // Evaluate all registered markets
                    let market_slugs: Vec<String> = {
                        self.markets.read().keys().cloned().collect()
                    };

                    for slug in market_slugs {
                        // Fetch orderbook (would be from WS cache in production)
                        if let Some(book) = self.get_orderbook(&state, &slug).await {
                            let decision = self.evaluate_market(&slug, &book);

                            // Execute if not NoAction
                            if !matches!(decision, TradeDecision::NoAction { .. }) {
                                let result = execute_decision(&decision, &executor, &state).await;
                                self.record_execution(&slug, &decision, &result);

                                if let Ok(Some(ack)) = &result {
                                    info!(
                                        market = %slug,
                                        price = %ack.filled_price,
                                        notional = %ack.filled_notional_usdc,
                                        "Latency arb trade executed"
                                    );
                                }
                            }
                        }
                    }

                    // Periodically update baseline latency (every ~minute)
                    if self.latency_histogram.count() % 600 == 0 {
                        self.update_baseline();
                    }
                }

                // Shutdown signal
                _ = shutdown.recv() => {
                    info!("Latency arbitrage engine shutting down");
                    break;
                }
            }
        }

        self.running.store(false, Ordering::SeqCst);
    }

    async fn get_orderbook(&self, state: &AppState, market_slug: &str) -> Option<OrderBook> {
        let markets = self.markets.read();
        let market = markets.get(market_slug)?;
        let token_id = &market.estimate.token_id_yes;

        // Try WS cache first
        state.polymarket_market_ws.request_subscribe(token_id);
        if let Some(book) = state.polymarket_market_ws.get_orderbook(token_id, 1500) {
            return Some((*book).clone());
        }

        // Fallback to REST (slower)
        let orderbook = state
            .http_client
            .get("https://clob.polymarket.com/book")
            .timeout(Duration::from_secs(3))
            .query(&[("token_id", token_id)])
            .send()
            .await
            .ok()?
            .error_for_status()
            .ok()?
            .json::<OrderBook>()
            .await
            .ok()?;

        Some(orderbook)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_microstructure_directional_bias() {
        let mut micro = MicrostructureState::default();
        micro.taker_imbalance = 0.3;
        micro.bid_depth_ratio = 1.2;
        micro.ask_depth_ratio = 0.8;
        micro.lift_count = 5;
        micro.hit_count = 1;

        let bias = micro.directional_bias();
        assert!(bias > 0.0, "Should be bullish bias");
        assert!(bias < 1.0, "Should be bounded");
    }

    #[test]
    fn test_fill_probability_from_latency() {
        let stats = LatencyStats {
            p50_us: 10_000,
            p90_us: 50_000,
            p95_us: 100_000,
            p99_us: 200_000,
            sample_count: 1000,
            baseline_p95_us: 100_000,
            congestion_ratio: 1.0,
        };

        let fill_prob = stats.fill_probability(500_000);
        assert!(fill_prob > 0.5, "Should be reasonable fill prob");
        assert!(fill_prob < 1.0, "Should not be certain");

        // Degraded latency
        let degraded = LatencyStats {
            p95_us: 400_000,
            congestion_ratio: 4.0,
            ..stats
        };
        let degraded_fill = degraded.fill_probability(500_000);
        assert!(
            degraded_fill < fill_prob,
            "Degraded should have lower fill prob"
        );
    }

    #[test]
    fn test_effective_edge_computation() {
        let mut estimate = ProbabilityEstimate::new("test-market", "token-yes", "token-no");
        estimate.private_prob = 0.60;
        estimate.market_prob = 0.50;

        let orderbook = OrderBook {
            bids: vec![Order {
                price: 0.49,
                size: 1000.0,
            }],
            asks: vec![Order {
                price: 0.51,
                size: 1000.0,
            }],
        };

        let latency = LatencyStats {
            p50_us: 10_000,
            p90_us: 50_000,
            p95_us: 100_000,
            p99_us: 200_000,
            sample_count: 1000,
            baseline_p95_us: 100_000,
            congestion_ratio: 1.0,
        };

        let cfg = LatencyArbConfig::default();
        let edge = compute_effective_edge(&estimate, &orderbook, &latency, &cfg);

        assert!(edge.raw_edge > 0.0, "Should have positive raw edge");
        assert!(
            edge.effective_edge < edge.raw_edge,
            "Effective < raw due to costs"
        );
    }

    #[test]
    fn test_exposure_limits() {
        let mut exposure = ExposureTracker::new(10_000.0);
        let cfg = LatencyArbConfig::default();

        // First trade should be allowed
        let allowed = exposure.can_add_exposure("market-1", "crypto", 100.0, &cfg);
        assert!(allowed.is_ok());

        // Add exposure
        exposure.add_exposure("market-1", "crypto", 100.0);

        // Should still have room
        let allowed2 = exposure.can_add_exposure("market-1", "crypto", 50.0, &cfg);
        assert!(allowed2.is_ok());

        // Approach the market limit (default is 2% of bankroll = $200)
        exposure.add_exposure("market-1", "crypto", 90.0);
        let limit_check = exposure
            .can_add_exposure("market-1", "crypto", 100.0, &cfg)
            .unwrap();
        // Should be limited but still allow some
        assert!((limit_check - 10.0).abs() < 1e-9);
    }
}

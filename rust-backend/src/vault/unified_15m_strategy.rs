//! Unified 15M Up/Down Trading Strategy
//!
//! Production-ready strategy for LIVE TRADING combining:
//! - RN-JD (Risk-Neutral Jump-Diffusion) probability model
//! - Dynamic belief volatility tracking
//! - Jump regime detection with adaptive edge requirements
//! - Vol-adjusted Kelly position sizing
//! - Fee-aware exit logic (favorable/reversed/timeout)
//! - Binary outcome settlement
//!
//! This is the SINGLE source of truth for 15M trading.

use anyhow::Result;
use chrono::Utc;
use parking_lot::RwLock;
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use tokio::sync::broadcast;
use tracing::{debug, info, warn};

use crate::{
    scrapers::binance_price_feed::{BinancePriceFeed, PriceUpdateEvent},
    vault::{
        belief_vol::BeliefVolTracker, estimate_p_up_rnjd, kelly_with_belief_vol, shrink_to_half,
        KellyParams, RnjdParams, UpDownAsset,
    },
};

// ============================================================================
// Configuration
// ============================================================================

/// Configuration for the Unified 15M Strategy
#[derive(Debug, Clone)]
pub struct Unified15mConfig {
    // === Risk Controls ===
    /// Minimum edge required to enter (e.g., 0.05 = 5%)
    pub min_edge: f64,
    /// Edge multiplier during jump regime (2.0 = require 2x edge)
    pub jump_regime_edge_mult: f64,
    /// Kelly fraction (0.25 = quarter Kelly)
    pub kelly_fraction: f64,
    /// Maximum position as fraction of bankroll
    pub max_position_pct: f64,
    /// Minimum position size in USD
    pub min_position_usd: f64,
    /// Maximum position size in USD
    pub max_position_usd: f64,
    /// Total bankroll for sizing
    pub bankroll: f64,

    // === RN-JD Parameters ===
    /// Base belief volatility (if dynamic tracking unavailable)
    pub sigma_b_default: f64,
    /// Shrink factor toward 0.5 (0.35 = 35% shrinkage)
    pub shrink_factor: f64,
    /// Jump intensity (0 = pure diffusion)
    pub lambda: f64,

    // === Timing ===
    /// Cooldown between trades per asset (seconds)
    pub cooldown_sec: i64,
    /// Skip last N seconds of window (too risky)
    pub window_end_skip_sec: i64,
    /// Exit timeout (seconds) - force exit after this
    pub exit_timeout_sec: i64,

    // === Jump Detection ===
    /// Z-score threshold for jump detection
    pub jump_z_threshold: f64,
    /// Window for counting jumps (seconds)
    pub jump_window_sec: i64,
    /// Number of jumps to trigger regime
    pub jump_count_threshold: usize,

    // === Fee Model ===
    /// Base fee rate at price 0.5 (e.g., 0.03 = 3%)
    pub fee_rate_at_mid: f64,
}

impl Default for Unified15mConfig {
    fn default() -> Self {
        Self {
            // Risk - conservative for real money
            min_edge: 0.05,             // 5% minimum edge
            jump_regime_edge_mult: 2.0, // Double edge requirement during jumps
            kelly_fraction: 0.25,       // Quarter Kelly
            max_position_pct: 0.02,     // 2% max per trade
            min_position_usd: 10.0,     // Skip tiny trades
            max_position_usd: 500.0,    // Cap per trade
            bankroll: 10_000.0,         // Default bankroll

            // RN-JD
            sigma_b_default: 2.0, // Default belief vol (annualized)
            shrink_factor: 0.35,  // 35% shrink toward 0.5
            lambda: 0.0,          // No jumps in diffusion

            // Timing
            cooldown_sec: 30,        // 30s between trades per asset
            window_end_skip_sec: 90, // Skip last 90s
            exit_timeout_sec: 180,   // 3 min max hold

            // Jump detection
            jump_z_threshold: 3.0,
            jump_window_sec: 300, // 5 min window
            jump_count_threshold: 2,

            // Fees
            fee_rate_at_mid: 0.03, // 3% at mid prices
        }
    }
}

impl Unified15mConfig {
    pub fn from_env() -> Self {
        let mut cfg = Self::default();

        if let Ok(v) = std::env::var("UNIFIED_15M_MIN_EDGE") {
            if let Ok(val) = v.parse::<f64>() {
                if val > 0.0 && val < 0.5 {
                    cfg.min_edge = val;
                }
            }
        }
        if let Ok(v) = std::env::var("UNIFIED_15M_KELLY_FRACTION") {
            if let Ok(val) = v.parse::<f64>() {
                if val > 0.0 && val <= 1.0 {
                    cfg.kelly_fraction = val;
                }
            }
        }
        if let Ok(v) = std::env::var("UNIFIED_15M_MAX_POSITION_USD") {
            if let Ok(val) = v.parse::<f64>() {
                if val > 0.0 {
                    cfg.max_position_usd = val;
                }
            }
        }
        if let Ok(v) = std::env::var("UNIFIED_15M_BANKROLL") {
            if let Ok(val) = v.parse::<f64>() {
                if val > 0.0 {
                    cfg.bankroll = val;
                }
            }
        }
        if let Ok(v) = std::env::var("UNIFIED_15M_COOLDOWN_SEC") {
            if let Ok(val) = v.parse::<i64>() {
                if val >= 0 {
                    cfg.cooldown_sec = val;
                }
            }
        }
        if let Ok(v) = std::env::var("UNIFIED_15M_SHRINK") {
            if let Ok(val) = v.parse::<f64>() {
                if val >= 0.0 && val <= 1.0 {
                    cfg.shrink_factor = val;
                }
            }
        }

        cfg
    }
}

// ============================================================================
// Position & Trade Types
// ============================================================================

/// Open position state
#[derive(Debug, Clone)]
pub struct OpenPosition {
    pub market_slug: String,
    pub side: PositionSide,
    pub entry_price: f64,
    pub shares: f64,
    pub entry_ts: i64,
    pub entry_edge: f64,
    pub sigma_b_at_entry: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PositionSide {
    BuyUp,
    BuyDown,
}

impl PositionSide {
    pub fn as_str(&self) -> &'static str {
        match self {
            PositionSide::BuyUp => "BUY_UP",
            PositionSide::BuyDown => "BUY_DOWN",
        }
    }

    pub fn outcome(&self) -> &'static str {
        match self {
            PositionSide::BuyUp => "Up",
            PositionSide::BuyDown => "Down",
        }
    }

    pub fn is_up(&self) -> bool {
        matches!(self, PositionSide::BuyUp)
    }
}

/// Trade record for audit trail
#[derive(Debug, Clone)]
pub struct TradeRecord {
    pub id: String,
    pub timestamp: i64,
    pub market_slug: String,
    pub side: String,
    pub outcome: String,
    pub entry_price: f64,
    pub exit_price: f64,
    pub shares: f64,
    pub gross_pnl: f64,
    pub fees: f64,
    pub net_pnl: f64,
    pub edge_at_entry: f64,
    pub exit_reason: ExitReason,
    pub sigma_b: f64,
    pub jump_regime: bool,
    pub hold_time_sec: i64,
}

#[derive(Debug, Clone, Copy)]
pub enum ExitReason {
    Favorable,
    EdgeReversed,
    Timeout,
    Settlement,
}

impl ExitReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            ExitReason::Favorable => "favorable",
            ExitReason::EdgeReversed => "reversed",
            ExitReason::Timeout => "timeout",
            ExitReason::Settlement => "settled",
        }
    }
}

// ============================================================================
// Strategy Metrics
// ============================================================================

#[derive(Debug, Default)]
pub struct StrategyMetrics {
    pub signals_received: AtomicU64,
    pub opportunities_found: AtomicU64,
    pub trades_entered: AtomicU64,
    pub trades_exited: AtomicU64,
    pub trades_settled: AtomicU64,
    pub skipped_cooldown: AtomicU64,
    pub skipped_no_edge: AtomicU64,
    pub skipped_jump_regime: AtomicU64,
    pub skipped_no_sigma: AtomicU64,
    pub skipped_window_end: AtomicU64,
    pub total_gross_pnl: RwLock<f64>,
    pub total_fees: RwLock<f64>,
    pub total_net_pnl: RwLock<f64>,
    pub trade_history: RwLock<Vec<TradeRecord>>,
}

impl StrategyMetrics {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record_pnl(&self, gross: f64, fees: f64) {
        *self.total_gross_pnl.write() += gross;
        *self.total_fees.write() += fees;
        *self.total_net_pnl.write() += gross - fees;
    }

    pub fn add_trade(&self, trade: TradeRecord) {
        let mut history = self.trade_history.write();
        if history.len() >= 1000 {
            history.remove(0);
        }
        history.push(trade);
    }

    pub fn summary(&self) -> MetricsSummary {
        MetricsSummary {
            signals: self.signals_received.load(Ordering::Relaxed),
            opportunities: self.opportunities_found.load(Ordering::Relaxed),
            entries: self.trades_entered.load(Ordering::Relaxed),
            exits: self.trades_exited.load(Ordering::Relaxed),
            settlements: self.trades_settled.load(Ordering::Relaxed),
            gross_pnl: *self.total_gross_pnl.read(),
            fees: *self.total_fees.read(),
            net_pnl: *self.total_net_pnl.read(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct MetricsSummary {
    pub signals: u64,
    pub opportunities: u64,
    pub entries: u64,
    pub exits: u64,
    pub settlements: u64,
    pub gross_pnl: f64,
    pub fees: f64,
    pub net_pnl: f64,
}

// ============================================================================
// Per-Asset State
// ============================================================================

#[derive(Debug, Clone, Default)]
struct AssetState {
    last_trade_ts: i64,
    window_start_price: Option<f64>,
    current_window_ts: i64,
}

// ============================================================================
// Unified 15M Strategy Engine
// ============================================================================

pub struct Unified15mStrategy {
    config: Unified15mConfig,
    binance_feed: Arc<BinancePriceFeed>,
    belief_vol_tracker: Arc<RwLock<BeliefVolTracker>>,

    // State
    positions: HashMap<String, OpenPosition>, // market_slug -> position
    asset_state: HashMap<UpDownAsset, AssetState>,
    metrics: Arc<StrategyMetrics>,

    // Control
    shutdown: AtomicBool,
    bankroll: f64,
}

impl Unified15mStrategy {
    pub fn new(
        config: Unified15mConfig,
        binance_feed: Arc<BinancePriceFeed>,
        belief_vol_tracker: Arc<RwLock<BeliefVolTracker>>,
    ) -> Self {
        let bankroll = config.bankroll;
        let mut asset_state = HashMap::new();
        for asset in [
            UpDownAsset::Btc,
            UpDownAsset::Eth,
            UpDownAsset::Sol,
            UpDownAsset::Xrp,
        ] {
            asset_state.insert(asset, AssetState::default());
        }

        Self {
            config,
            binance_feed,
            belief_vol_tracker,
            positions: HashMap::new(),
            asset_state,
            metrics: Arc::new(StrategyMetrics::new()),
            shutdown: AtomicBool::new(false),
            bankroll,
        }
    }

    pub fn metrics(&self) -> Arc<StrategyMetrics> {
        Arc::clone(&self.metrics)
    }

    pub fn shutdown(&self) {
        self.shutdown.store(true, Ordering::Release);
    }

    /// Calculate Polymarket 15m Up/Down fee
    /// Formula: fee = 0.25 * shares * (p * (1-p))^2
    /// Or: fee_per_share = 0.25 * (p * (1-p))^2
    ///
    /// This is symmetric around p=0.5 (max fee) and approaches 0 at extremes.
    /// At p=0.50: fee_per_share = 0.015625 (1.5625%)
    /// At p=0.30 or p=0.70: fee_per_share = 0.011025 (1.1%)  
    /// At p=0.10 or p=0.90: fee_per_share = 0.002025 (0.2%)
    fn calculate_fee(&self, shares: f64, price: f64) -> f64 {
        let p = price.clamp(0.001, 0.999);
        let p_1_p = p * (1.0 - p);
        0.25 * shares * p_1_p * p_1_p
    }

    /// Fee per share at a given price
    fn fee_per_share(&self, price: f64) -> f64 {
        let p = price.clamp(0.001, 0.999);
        let p_1_p = p * (1.0 - p);
        0.25 * p_1_p * p_1_p
    }

    /// Get dynamic sigma_b or fall back to default
    fn get_sigma_b(&self, market_slug: &str) -> f64 {
        let tracker = self.belief_vol_tracker.read();
        let sigma_b = tracker.get_sigma_b(market_slug);
        if sigma_b > 0.0 && sigma_b.is_finite() {
            sigma_b
        } else {
            self.config.sigma_b_default
        }
    }

    /// Detect if we're in a jump regime
    fn is_jump_regime(&self, market_slug: &str, now: i64) -> bool {
        let tracker = self.belief_vol_tracker.read();
        tracker.count_recent_jumps(
            market_slug,
            self.config.jump_window_sec,
            now,
            self.config.jump_z_threshold,
        ) >= self.config.jump_count_threshold
    }

    /// Process a market order signal (from Dome WebSocket)
    pub fn on_order(
        &mut self,
        market_slug: &str,
        outcome: &str,
        order_price: f64,
        order_timestamp: i64,
    ) -> Option<TradeRecord> {
        self.metrics
            .signals_received
            .fetch_add(1, Ordering::Relaxed);

        let now = Utc::now().timestamp();
        let is_up = outcome.to_lowercase() == "up";

        // Parse asset from slug
        let asset = if market_slug.starts_with("btc-") {
            UpDownAsset::Btc
        } else if market_slug.starts_with("eth-") {
            UpDownAsset::Eth
        } else if market_slug.starts_with("sol-") {
            UpDownAsset::Sol
        } else if market_slug.starts_with("xrp-") {
            UpDownAsset::Xrp
        } else {
            return None;
        };

        let binance_symbol = asset.binance_symbol();

        // Parse window from slug (e.g., "btc-updown-15m-1768533300")
        let window_ts: i64 = market_slug
            .rsplit('-')
            .next()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);

        if window_ts == 0 {
            return None;
        }

        let window_end = window_ts + 15 * 60;
        let t_rem = (window_end - now).max(0);
        let t_rem_sec = t_rem as f64;

        // Skip last N seconds of window
        if t_rem < self.config.window_end_skip_sec {
            self.metrics
                .skipped_window_end
                .fetch_add(1, Ordering::Relaxed);
            return None;
        }

        // Get Binance prices
        let p_now = self
            .binance_feed
            .latest_mid(&binance_symbol)
            .map(|p| p.mid)?;

        // Get or set window start price
        let asset_state = self.asset_state.entry(asset).or_default();
        if asset_state.current_window_ts != window_ts {
            asset_state.current_window_ts = window_ts;
            asset_state.window_start_price = self
                .binance_feed
                .mid_near(&binance_symbol, window_ts, 120)
                .or_else(|| self.binance_feed.latest_mid(&binance_symbol))
                .map(|p| p.mid);
        }
        let p_start = asset_state.window_start_price?;

        // Get volatility (sigma per sqrt second)
        let sigma = match self.binance_feed.sigma_per_sqrt_s(&binance_symbol) {
            Some(s) if s > 0.0 && s.is_finite() => s,
            _ => {
                self.metrics
                    .skipped_no_sigma
                    .fetch_add(1, Ordering::Relaxed);
                return None;
            }
        };

        // Annualize sigma for RN-JD
        let sigma_annual = sigma * (365.25 * 24.0 * 3600.0_f64).sqrt();

        // Get dynamic belief volatility
        let sigma_b = self.get_sigma_b(market_slug);

        // Record observation for belief vol tracking
        {
            let mut tracker = self.belief_vol_tracker.write();
            tracker.record_observation(market_slug, order_price, now);
        }

        // RN-JD parameters
        let rnjd_params = RnjdParams {
            sigma_b,
            lambda: self.config.lambda,
            mu_j: 0.0,
            sigma_j: 0.1,
        };

        // Market price as prior
        let market_p = order_price.clamp(0.01, 0.99);

        // === RN-JD PROBABILITY ESTIMATION ===
        let rnjd_estimate = estimate_p_up_rnjd(
            p_start,
            p_now,
            market_p,
            sigma_annual,
            t_rem_sec,
            &rnjd_params,
        )?;

        // Apply shrinkage
        let model_p_up = shrink_to_half(rnjd_estimate.p_up, self.config.shrink_factor);
        let model_p_down = 1.0 - model_p_up;

        // Determine model's probability for this outcome
        let model_p = if is_up { model_p_up } else { model_p_down };

        // Calculate edge
        let edge = model_p - order_price;
        let edge_abs = edge.abs();

        // Record opportunity if edge exists
        if edge_abs >= self.config.min_edge {
            self.metrics
                .opportunities_found
                .fetch_add(1, Ordering::Relaxed);
        }

        // === CHECK FOR EXIT ON EXISTING POSITION ===
        if let Some(position) = self.positions.remove(market_slug) {
            // Only exit if order outcome matches position
            let order_matches_position =
                (is_up && position.side.is_up()) || (!is_up && !position.side.is_up());

            if !order_matches_position {
                // Wrong outcome - put position back
                self.positions.insert(market_slug.to_string(), position);
                return None;
            }

            let hold_time = now - position.entry_ts;
            let price_move = order_price - position.entry_price;

            // Calculate min profitable move using Polymarket fee formula
            // Entry fee at entry_price + Exit fee at current price (conservative estimate)
            let entry_fee_per_share = self.fee_per_share(position.entry_price);
            let exit_fee_per_share = self.fee_per_share(order_price);
            let min_profitable_move = entry_fee_per_share + exit_fee_per_share;

            // Exit conditions
            let favorable = match position.side {
                PositionSide::BuyUp => price_move > min_profitable_move,
                PositionSide::BuyDown => price_move > min_profitable_move, // DOWN price rising = profit
            };

            let reversed = edge < -self.config.min_edge;
            let timeout = hold_time >= self.config.exit_timeout_sec;

            if favorable || reversed || timeout {
                let exit_reason = if favorable {
                    ExitReason::Favorable
                } else if reversed {
                    ExitReason::EdgeReversed
                } else {
                    ExitReason::Timeout
                };

                // PnL calculation for binary options
                let gross_pnl = position.shares * (order_price - position.entry_price);
                // Polymarket fee: 0.25 * shares * (p*(1-p))^2 at both entry and exit
                let entry_fee = self.calculate_fee(position.shares, position.entry_price);
                let exit_fee = self.calculate_fee(position.shares, order_price);
                let fees = entry_fee + exit_fee;
                let net_pnl = gross_pnl - fees;

                self.metrics.trades_exited.fetch_add(1, Ordering::Relaxed);
                self.metrics.record_pnl(gross_pnl, fees);
                self.bankroll += net_pnl;

                let trade = TradeRecord {
                    id: uuid::Uuid::new_v4().to_string(),
                    timestamp: now,
                    market_slug: market_slug.to_string(),
                    side: format!("{}_EXIT", position.side.as_str()),
                    outcome: position.side.outcome().to_string(),
                    entry_price: position.entry_price,
                    exit_price: order_price,
                    shares: position.shares,
                    gross_pnl,
                    fees,
                    net_pnl,
                    edge_at_entry: position.entry_edge,
                    exit_reason,
                    sigma_b: position.sigma_b_at_entry,
                    jump_regime: self.is_jump_regime(market_slug, now),
                    hold_time_sec: hold_time,
                };

                info!(
                    "[UNIFIED] EXIT ({}): {} {} @ {:.4} -> {:.4} (PnL: ${:.2}, hold: {}s)",
                    exit_reason.as_str(),
                    position.side.as_str(),
                    market_slug,
                    position.entry_price,
                    order_price,
                    net_pnl,
                    hold_time
                );

                self.metrics.add_trade(trade.clone());
                return Some(trade);
            } else {
                // Keep position open
                self.positions.insert(market_slug.to_string(), position);
                return None;
            }
        }

        // === CHECK FOR ENTRY ===

        // Already have a position?
        if self.positions.contains_key(market_slug) {
            return None;
        }

        // Cooldown check
        let asset_state = self.asset_state.entry(asset).or_default();
        if now - asset_state.last_trade_ts < self.config.cooldown_sec {
            self.metrics
                .skipped_cooldown
                .fetch_add(1, Ordering::Relaxed);
            return None;
        }

        // Jump regime check
        let jump_regime = self.is_jump_regime(market_slug, now);
        let effective_min_edge = if jump_regime {
            self.config.min_edge * self.config.jump_regime_edge_mult
        } else {
            self.config.min_edge
        };

        // Edge check (only enter on positive edge for this outcome)
        if edge < effective_min_edge {
            if jump_regime && edge >= self.config.min_edge {
                self.metrics
                    .skipped_jump_regime
                    .fetch_add(1, Ordering::Relaxed);
            } else {
                self.metrics.skipped_no_edge.fetch_add(1, Ordering::Relaxed);
            }
            return None;
        }

        // === VOL-ADJUSTED KELLY SIZING ===
        let kelly_params = KellyParams {
            bankroll: self.bankroll,
            kelly_fraction: self.config.kelly_fraction,
            max_position_pct: self.config.max_position_pct,
            min_position_usd: self.config.min_position_usd,
        };

        let t_years = t_rem_sec / (365.25 * 24.0 * 3600.0);
        let kelly = kelly_with_belief_vol(model_p, order_price, sigma_b, t_years, &kelly_params);

        if !kelly.should_trade {
            debug!(
                "[UNIFIED] Kelly skip for {}: {:?}",
                market_slug, kelly.skip_reason
            );
            return None;
        }

        // Apply position caps
        let mut notional = kelly.position_size_usd;
        notional = notional.min(self.config.max_position_usd);
        notional = notional.min(self.bankroll * self.config.max_position_pct);

        if notional < self.config.min_position_usd {
            return None;
        }

        // === EXECUTE ENTRY ===
        let shares = notional / order_price;
        let side = if is_up {
            PositionSide::BuyUp
        } else {
            PositionSide::BuyDown
        };

        let position = OpenPosition {
            market_slug: market_slug.to_string(),
            side,
            entry_price: order_price,
            shares,
            entry_ts: now,
            entry_edge: edge,
            sigma_b_at_entry: sigma_b,
        };

        self.positions.insert(market_slug.to_string(), position);
        self.asset_state.get_mut(&asset).unwrap().last_trade_ts = now;
        self.metrics.trades_entered.fetch_add(1, Ordering::Relaxed);

        info!(
            "[UNIFIED] ENTRY: {} {} @ {:.4} (model: {:.4}, edge: {:.2}%, size: ${:.2}, sigma_b: {:.2}, jump: {})",
            side.as_str(), market_slug, order_price, model_p, edge * 100.0, notional, sigma_b, jump_regime
        );

        None // Entry doesn't produce a trade record yet
    }

    /// Handle settlement at window expiry
    pub fn on_settlement(
        &mut self,
        market_slug: &str,
        winning_outcome: &str, // "Up" or "Down"
    ) -> Option<TradeRecord> {
        let position = self.positions.remove(market_slug)?;
        let now = Utc::now().timestamp();
        let hold_time = now - position.entry_ts;

        let won = (winning_outcome == "Up" && position.side.is_up())
            || (winning_outcome == "Down" && !position.side.is_up());

        // Binary settlement PnL
        let (gross_pnl, exit_price) = if won {
            // Win: shares * (1 - entry_price)
            let pnl = position.shares * (1.0 - position.entry_price);
            (pnl, 1.0)
        } else {
            // Lose: -shares * entry_price
            let pnl = -position.shares * position.entry_price;
            (pnl, 0.0)
        };

        // Only entry fee for settlement (settlement at $1 or $0 has negligible exit fee)
        let fees = self.calculate_fee(position.shares, position.entry_price);
        let net_pnl = gross_pnl - fees;

        self.metrics.trades_settled.fetch_add(1, Ordering::Relaxed);
        self.metrics.record_pnl(gross_pnl, fees);
        self.bankroll += net_pnl;

        let trade = TradeRecord {
            id: uuid::Uuid::new_v4().to_string(),
            timestamp: now,
            market_slug: market_slug.to_string(),
            side: format!("{}_SETTLED", position.side.as_str()),
            outcome: winning_outcome.to_string(),
            entry_price: position.entry_price,
            exit_price,
            shares: position.shares,
            gross_pnl,
            fees,
            net_pnl,
            edge_at_entry: position.entry_edge,
            exit_reason: ExitReason::Settlement,
            sigma_b: position.sigma_b_at_entry,
            jump_regime: false,
            hold_time_sec: hold_time,
        };

        info!(
            "[UNIFIED] SETTLED: {} {} @ {:.4} -> {} (PnL: ${:.2})",
            position.side.as_str(),
            market_slug,
            position.entry_price,
            winning_outcome,
            net_pnl
        );

        self.metrics.add_trade(trade.clone());
        Some(trade)
    }

    /// Get current open positions
    pub fn open_positions(&self) -> Vec<&OpenPosition> {
        self.positions.values().collect()
    }

    /// Get current bankroll
    pub fn bankroll(&self) -> f64 {
        self.bankroll
    }

    /// Update bankroll (e.g., for deposits/withdrawals)
    pub fn set_bankroll(&mut self, bankroll: f64) {
        self.bankroll = bankroll;
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_defaults() {
        let cfg = Unified15mConfig::default();
        assert!(cfg.min_edge >= 0.01);
        assert!(cfg.kelly_fraction <= 1.0);
        assert!(cfg.max_position_usd > 0.0);
    }

    #[test]
    fn test_polymarket_fee_formula() {
        // Test the Polymarket 15m Up/Down fee formula directly
        // fee = 0.25 * shares * (p * (1-p))^2

        fn fee_per_share(price: f64) -> f64 {
            let p = price.clamp(0.001, 0.999);
            let p_1_p = p * (1.0 - p);
            0.25 * p_1_p * p_1_p
        }

        fn calculate_fee(shares: f64, price: f64) -> f64 {
            let p = price.clamp(0.001, 0.999);
            let p_1_p = p * (1.0 - p);
            0.25 * shares * p_1_p * p_1_p
        }

        // At p=0.50: fee_per_share = 0.25 * (0.5 * 0.5)^2 = 0.25 * 0.0625 = 0.015625
        let fee_mid = fee_per_share(0.5);
        assert!(
            (fee_mid - 0.015625).abs() < 0.0001,
            "fee at 0.5 should be 0.015625"
        );

        // At p=0.30: fee_per_share = 0.25 * (0.3 * 0.7)^2 = 0.25 * 0.0441 = 0.011025
        let fee_30 = fee_per_share(0.30);
        assert!(
            (fee_30 - 0.011025).abs() < 0.0001,
            "fee at 0.30 should be 0.011025"
        );

        // At p=0.10: fee_per_share = 0.25 * (0.1 * 0.9)^2 = 0.25 * 0.0081 = 0.002025
        let fee_10 = fee_per_share(0.10);
        assert!(
            (fee_10 - 0.002025).abs() < 0.0001,
            "fee at 0.10 should be 0.002025"
        );

        // Symmetric: fee at 0.3 == fee at 0.7
        let fee_70 = fee_per_share(0.70);
        assert!((fee_30 - fee_70).abs() < 0.0001, "fee should be symmetric");

        // Total fee for 100 shares at 0.50: 0.25 * 100 * 0.0625 = 1.5625
        let total_fee = calculate_fee(100.0, 0.50);
        assert!(
            (total_fee - 1.5625).abs() < 0.001,
            "total fee for 100 shares at 0.50 should be 1.5625"
        );

        // Total fee for 100 shares at 0.30: 0.25 * 100 * 0.0441 = 1.1025
        let total_fee_30 = calculate_fee(100.0, 0.30);
        assert!(
            (total_fee_30 - 1.1025).abs() < 0.001,
            "total fee for 100 shares at 0.30 should be 1.1025"
        );
    }

    #[test]
    fn test_position_side() {
        assert_eq!(PositionSide::BuyUp.as_str(), "BUY_UP");
        assert_eq!(PositionSide::BuyDown.outcome(), "Down");
        assert!(PositionSide::BuyUp.is_up());
        assert!(!PositionSide::BuyDown.is_up());
    }
}

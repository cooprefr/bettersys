//! HFT Paper Trading Strategy with RN-JD Core
//!
//! This module implements a high-frequency, low-risk paper trading strategy that:
//! 1. Uses the HFT book store (no REST in hot path)
//! 2. Integrates RN-JD (Risk-Neutral Jump-Diffusion) for probability estimation
//! 3. Applies belief volatility (σ_b) for position sizing
//! 4. Detects jump regimes and adjusts edge requirements
//! 5. Uses vol-adjusted Kelly criterion for conservative sizing
//!
//! Design Principles:
//! - LOW RISK: Conservative Kelly fraction, jump regime detection, strict edge requirements
//! - HIGH FREQUENCY: Event-driven on Binance price updates, skip-tick on cache miss
//! - NO BLOCKING: Cache-only orderbook access, never awaits REST
//! - RNJD CORE: All probability estimates use the theoretically-grounded RN-JD model

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
use tracing::{debug, info, trace, warn};
use uuid::Uuid;

use crate::{
    scrapers::{
        binance_price_feed::PriceUpdateEvent, polymarket_gamma,
        polymarket_ws::PolymarketMarketWsCache,
    },
    vault::{
        belief_vol::{BeliefVolTracker, JumpDetectionResult},
        book_access::StalenessConfig,
        estimate_p_up_enhanced, kelly_with_belief_vol, shrink_to_half, ExecutionAdapter,
        KellyParams, OrderRequest, OrderSide, PaperExecutionAdapter, RnjdEstimate, TimeInForce,
        UpDownAsset, VaultPaperLedger,
    },
    AppState,
};

// ============================================================================
// Configuration
// ============================================================================

/// Configuration for the HFT Paper Strategy
#[derive(Debug, Clone)]
pub struct HftPaperStrategyConfig {
    // Risk controls (LOW RISK settings)
    /// Minimum edge required to trade (higher = more conservative)
    pub min_edge: f64,
    /// Edge multiplier during jump regime (2.0 = require 2x edge)
    pub jump_regime_edge_mult: f64,
    /// Kelly fraction (0.01-0.05 is conservative)
    pub kelly_fraction: f64,
    /// Maximum position as fraction of bankroll
    pub max_position_pct: f64,
    /// Minimum position size in USD (skip smaller trades)
    pub min_position_usd: f64,
    /// Maximum position size in USD (cap large trades)
    pub max_position_usd: f64,
    /// Shrink-to-half factor for p_up (0.35 = 35% shrinkage toward 0.5)
    pub shrink_factor: f64,

    // Timing controls
    /// Cooldown between trades on same asset (seconds)
    pub cooldown_sec: i64,
    /// Skip trading in last N seconds of 15m window (too late)
    pub window_end_skip_sec: i64,
    /// Aggressive trading in first N seconds (fresh window)
    pub window_start_aggressive_sec: i64,

    // Book staleness (HFT settings)
    /// Maximum book age for trading decisions (ms)
    pub book_max_stale_ms: u64,
    /// Hard staleness that triggers resubscription (ms)
    pub book_hard_stale_ms: u64,

    // RNJD settings
    /// Minimum confidence from RN-JD estimate
    pub min_rnjd_confidence: f64,
    /// Z-score threshold for jump detection
    pub jump_z_threshold: f64,
    /// Window for counting recent jumps (seconds)
    pub jump_window_sec: i64,
    /// Number of jumps to trigger regime detection
    pub jump_count_threshold: usize,
}

impl Default for HftPaperStrategyConfig {
    fn default() -> Self {
        Self {
            // Conservative risk settings
            min_edge: 0.015,            // 1.5% minimum edge (higher than default)
            jump_regime_edge_mult: 2.5, // Require 2.5x edge during jumps
            kelly_fraction: 0.02,       // 2% Kelly (very conservative)
            max_position_pct: 0.005,    // 0.5% max of bankroll per trade
            min_position_usd: 5.0,      // Skip tiny trades
            max_position_usd: 100.0,    // Cap at $100 per trade
            shrink_factor: 0.40,        // 40% shrinkage (more conservative)

            // Timing
            cooldown_sec: 45,        // 45 second cooldown
            window_end_skip_sec: 90, // Skip last 90 seconds
            window_start_aggressive_sec: 45,

            // Book staleness (tight for HFT)
            book_max_stale_ms: 1000,  // 1 second max
            book_hard_stale_ms: 3000, // 3 second hard limit

            // RNJD
            min_rnjd_confidence: 0.5,
            jump_z_threshold: 3.0,
            jump_window_sec: 300, // 5 minute window
            jump_count_threshold: 2,
        }
    }
}

impl HftPaperStrategyConfig {
    pub fn from_env() -> Self {
        let mut cfg = Self::default();

        if let Ok(v) = std::env::var("HFT_PAPER_MIN_EDGE") {
            if let Ok(val) = v.parse::<f64>() {
                if val.is_finite() && val > 0.0 && val < 0.5 {
                    cfg.min_edge = val;
                }
            }
        }
        if let Ok(v) = std::env::var("HFT_PAPER_KELLY_FRACTION") {
            if let Ok(val) = v.parse::<f64>() {
                if val.is_finite() && val > 0.0 && val <= 0.25 {
                    cfg.kelly_fraction = val;
                }
            }
        }
        if let Ok(v) = std::env::var("HFT_PAPER_MAX_POSITION_PCT") {
            if let Ok(val) = v.parse::<f64>() {
                if val.is_finite() && val > 0.0 && val <= 0.1 {
                    cfg.max_position_pct = val;
                }
            }
        }
        if let Ok(v) = std::env::var("HFT_PAPER_MAX_POSITION_USD") {
            if let Ok(val) = v.parse::<f64>() {
                if val.is_finite() && val > 0.0 {
                    cfg.max_position_usd = val;
                }
            }
        }
        if let Ok(v) = std::env::var("HFT_PAPER_COOLDOWN_SEC") {
            if let Ok(val) = v.parse::<i64>() {
                if val >= 0 {
                    cfg.cooldown_sec = val;
                }
            }
        }
        if let Ok(v) = std::env::var("HFT_PAPER_SHRINK_FACTOR") {
            if let Ok(val) = v.parse::<f64>() {
                if val.is_finite() && val >= 0.0 && val <= 1.0 {
                    cfg.shrink_factor = val;
                }
            }
        }
        if let Ok(v) = std::env::var("HFT_PAPER_BOOK_MAX_STALE_MS") {
            if let Ok(val) = v.parse::<u64>() {
                cfg.book_max_stale_ms = val;
            }
        }

        cfg
    }
}

// ============================================================================
// Strategy State
// ============================================================================

/// Per-asset trading state
#[derive(Debug, Clone, Default)]
struct AssetState {
    /// Last trade timestamp (unix seconds)
    last_trade_ts: i64,
    /// Last evaluation timestamp
    last_eval_ts: i64,
    /// Last computed p_up (for change detection)
    last_p_up: f64,
    /// Last computed edge
    last_edge: f64,
    /// Consecutive skips (for debugging)
    consecutive_skips: u32,
    /// Token cache: (token_up, token_down)
    tokens: Option<(String, String)>,
}

/// Trade record for paper trading
#[derive(Debug, Clone)]
pub struct HftPaperTrade {
    pub id: String,
    pub timestamp: i64,
    pub asset: String,
    pub market_slug: String,
    pub side: String,
    pub outcome: String,
    pub price: f64,
    pub notional_usd: f64,
    pub shares: f64,
    pub fees_usd: f64,
    /// RN-JD probability estimate
    pub p_estimate: f64,
    /// Edge at time of trade
    pub edge: f64,
    /// Belief volatility used
    pub sigma_b: f64,
    /// Was this during jump regime?
    pub jump_regime: bool,
    /// RN-JD confidence
    pub rnjd_confidence: f64,
    /// Tick-to-trade latency (microseconds)
    pub t2t_us: u64,
}

/// Strategy metrics
#[derive(Debug, Default)]
pub struct HftPaperMetrics {
    /// Total price updates received
    pub price_updates: AtomicU64,
    /// Total evaluations performed
    pub evaluations: AtomicU64,
    /// Skipped due to cooldown
    pub skipped_cooldown: AtomicU64,
    /// Skipped due to window timing
    pub skipped_window: AtomicU64,
    /// Skipped due to no book data
    pub skipped_no_book: AtomicU64,
    /// Skipped due to insufficient edge
    pub skipped_no_edge: AtomicU64,
    /// Skipped due to jump regime
    pub skipped_jump_regime: AtomicU64,
    /// Skipped due to low RNJD confidence
    pub skipped_low_confidence: AtomicU64,
    /// Skipped due to Kelly skip
    pub skipped_kelly: AtomicU64,
    /// Trades executed
    pub trades_executed: AtomicU64,
    /// Total PnL (paper)
    pub total_pnl_usd: RwLock<f64>,
    /// Recent trades for inspection
    pub recent_trades: RwLock<Vec<HftPaperTrade>>,
}

impl HftPaperMetrics {
    fn new() -> Self {
        Self {
            recent_trades: RwLock::new(Vec::with_capacity(100)),
            ..Default::default()
        }
    }

    fn record_trade(&self, trade: HftPaperTrade) {
        self.trades_executed.fetch_add(1, Ordering::Relaxed);

        let mut trades = self.recent_trades.write();
        if trades.len() >= 100 {
            trades.remove(0);
        }
        trades.push(trade);
    }

    pub fn summary(&self) -> HftPaperMetricsSummary {
        HftPaperMetricsSummary {
            price_updates: self.price_updates.load(Ordering::Relaxed),
            evaluations: self.evaluations.load(Ordering::Relaxed),
            trades_executed: self.trades_executed.load(Ordering::Relaxed),
            skipped_cooldown: self.skipped_cooldown.load(Ordering::Relaxed),
            skipped_window: self.skipped_window.load(Ordering::Relaxed),
            skipped_no_book: self.skipped_no_book.load(Ordering::Relaxed),
            skipped_no_edge: self.skipped_no_edge.load(Ordering::Relaxed),
            skipped_jump_regime: self.skipped_jump_regime.load(Ordering::Relaxed),
            skipped_low_confidence: self.skipped_low_confidence.load(Ordering::Relaxed),
            skipped_kelly: self.skipped_kelly.load(Ordering::Relaxed),
            total_pnl_usd: *self.total_pnl_usd.read(),
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct HftPaperMetricsSummary {
    pub price_updates: u64,
    pub evaluations: u64,
    pub trades_executed: u64,
    pub skipped_cooldown: u64,
    pub skipped_window: u64,
    pub skipped_no_book: u64,
    pub skipped_no_edge: u64,
    pub skipped_jump_regime: u64,
    pub skipped_low_confidence: u64,
    pub skipped_kelly: u64,
    pub total_pnl_usd: f64,
}

// ============================================================================
// HFT Paper Strategy Engine
// ============================================================================

/// The main HFT Paper Trading Strategy engine
pub struct HftPaperStrategy {
    config: HftPaperStrategyConfig,
    state: Arc<AppState>,
    exec: Arc<PaperExecutionAdapter>,
    asset_state: HashMap<UpDownAsset, AssetState>,
    metrics: Arc<HftPaperMetrics>,
    staleness_config: StalenessConfig,
    /// Shutdown flag
    shutdown: AtomicBool,
}

impl HftPaperStrategy {
    pub fn new(state: Arc<AppState>, config: HftPaperStrategyConfig) -> Self {
        let mut asset_state = HashMap::new();
        for asset in [
            UpDownAsset::Btc,
            UpDownAsset::Eth,
            UpDownAsset::Sol,
            UpDownAsset::Xrp,
        ] {
            asset_state.insert(asset, AssetState::default());
        }

        let staleness_config = StalenessConfig {
            max_stale_ms: config.book_max_stale_ms,
            hard_stale_ms: config.book_hard_stale_ms,
        };

        // Use low-latency paper execution (no simulated delay for HFT testing)
        let exec_config = crate::vault::PaperExecutionConfig {
            base_latency_ms: 0, // No artificial delay
            latency_jitter_ms: 0,
            slippage_bps_per_1k: 10.0, // Realistic slippage
            base_slippage_bps: 5.0,
            fee_rate: 0.005,         // 0.5% fee
            partial_fill_prob: 0.05, // 5% partial fills
            min_fill_ratio: 0.7,
            reject_prob: 0.01, // 1% rejection
        };

        Self {
            config,
            state,
            exec: Arc::new(PaperExecutionAdapter::new(exec_config)),
            asset_state,
            metrics: Arc::new(HftPaperMetrics::new()),
            staleness_config,
            shutdown: AtomicBool::new(false),
        }
    }

    /// Get metrics handle
    pub fn metrics(&self) -> Arc<HftPaperMetrics> {
        Arc::clone(&self.metrics)
    }

    /// Shutdown the strategy
    pub fn shutdown(&self) {
        self.shutdown.store(true, Ordering::Release);
    }

    /// Process a price update event (main entry point)
    pub async fn on_price_update(
        &mut self,
        event: PriceUpdateEvent,
    ) -> Result<Option<HftPaperTrade>> {
        let eval_start = Instant::now();
        self.metrics.price_updates.fetch_add(1, Ordering::Relaxed);

        trace!(
            symbol = %event.symbol,
            mid = event.mid,
            "HFT_PAPER received price update"
        );

        let now = Utc::now().timestamp();

        // Determine asset from symbol
        let asset = match event.symbol.as_str() {
            "BTCUSDT" => UpDownAsset::Btc,
            "ETHUSDT" => UpDownAsset::Eth,
            "SOLUSDT" => UpDownAsset::Sol,
            "XRPUSDT" => UpDownAsset::Xrp,
            _ => return Ok(None),
        };

        // Compute current 15m window
        let start_ts = now - (now % (15 * 60));
        let end_ts = start_ts + 15 * 60;
        let t_rem = (end_ts - now).max(0);
        let t_elapsed = (now - start_ts).max(0);

        // Window timing check: skip if too late
        if t_rem < self.config.window_end_skip_sec {
            self.metrics.skipped_window.fetch_add(1, Ordering::Relaxed);
            return Ok(None);
        }

        let slug = format!("{}-updown-15m-{}", asset.as_str(), start_ts);

        // Get asset state
        let asset_state = self.asset_state.entry(asset).or_default();

        // Cooldown check
        if now - asset_state.last_trade_ts < self.config.cooldown_sec {
            self.metrics
                .skipped_cooldown
                .fetch_add(1, Ordering::Relaxed);
            asset_state.consecutive_skips += 1;
            return Ok(None);
        }

        self.metrics.evaluations.fetch_add(1, Ordering::Relaxed);

        // Get start price for this window
        // First try exact window start, then allow a wider window, then fall back to current
        let p_start = self
            .state
            .binance_feed
            .mid_near(&event.symbol, start_ts, 60)
            .or_else(|| {
                self.state
                    .binance_feed
                    .mid_near(&event.symbol, start_ts, 120)
            })
            .or_else(|| self.state.binance_feed.latest_mid(&event.symbol))
            .map(|p| p.mid);

        let Some(p_start) = p_start else {
            debug!(symbol = %event.symbol, start_ts = start_ts, "No start price available");
            return Ok(None);
        };

        // Get volatility
        let Some(sigma) = self.state.binance_feed.sigma_per_sqrt_s(&event.symbol) else {
            debug!(symbol = %event.symbol, "No volatility estimate available");
            return Ok(None);
        };

        // Resolve token IDs (cached)
        let (token_up, token_down) = match &asset_state.tokens {
            Some(t) => t.clone(),
            None => {
                debug!(slug = %slug, "Resolving token IDs for new 15m window");
                let up = polymarket_gamma::resolve_clob_token_id_by_slug(
                    self.state.signal_storage.as_ref(),
                    &self.state.http_client,
                    &slug,
                    "Up",
                )
                .await?
                .unwrap_or_default();

                let down = polymarket_gamma::resolve_clob_token_id_by_slug(
                    self.state.signal_storage.as_ref(),
                    &self.state.http_client,
                    &slug,
                    "Down",
                )
                .await?
                .unwrap_or_default();

                if up.is_empty() || down.is_empty() {
                    debug!(slug = %slug, "Token IDs not resolved (market may not exist yet)");
                    return Ok(None);
                }

                info!(slug = %slug, token_up = %up, token_down = %down, "Resolved token IDs");
                asset_state.tokens = Some((up.clone(), down.clone()));
                (up, down)
            }
        };

        // Ensure subscriptions (non-blocking)
        self.state.polymarket_market_ws.request_subscribe(&token_up);
        self.state
            .polymarket_market_ws
            .request_subscribe(&token_down);

        // Get orderbook from cache ONLY (no REST fallback - skip-tick semantics)
        let ask_up = self
            .state
            .polymarket_market_ws
            .get_orderbook(&token_up, self.staleness_config.max_stale_ms as i64)
            .and_then(|book| book.asks.first().map(|o| o.price));

        let ask_down = self
            .state
            .polymarket_market_ws
            .get_orderbook(&token_down, self.staleness_config.max_stale_ms as i64)
            .and_then(|book| book.asks.first().map(|o| o.price));

        // Skip if no book data
        if ask_up.is_none() && ask_down.is_none() {
            self.metrics.skipped_no_book.fetch_add(1, Ordering::Relaxed);
            asset_state.consecutive_skips += 1;
            return Ok(None);
        }

        // Get market mid price for RNJD
        let market_mid = match (ask_up, ask_down) {
            (Some(a), Some(b)) => 0.5 * (a + (1.0 - b)),
            (Some(a), None) => a,
            (None, Some(b)) => 1.0 - b,
            (None, None) => 0.5,
        };

        // Record observation for belief vol tracking
        {
            let mut tracker = self.state.belief_vol_tracker.write();
            tracker.record_observation(&slug, market_mid, now);
        }

        // === RN-JD CORE: Estimate probability using Risk-Neutral Jump-Diffusion ===
        let rnjd_estimate = match estimate_p_up_enhanced(
            p_start,
            event.mid,
            market_mid,
            sigma,
            t_rem as f64,
            Some(&*self.state.belief_vol_tracker.read()),
            &slug,
            now,
        ) {
            Some(e) => e,
            None => {
                debug!(slug = %slug, "RN-JD estimation failed");
                return Ok(None);
            }
        };

        // Check RNJD confidence
        if rnjd_estimate.confidence < self.config.min_rnjd_confidence {
            self.metrics
                .skipped_low_confidence
                .fetch_add(1, Ordering::Relaxed);
            return Ok(None);
        }

        // Apply shrink-to-half for additional conservatism
        let p_up = shrink_to_half(rnjd_estimate.p_up, self.config.shrink_factor);
        let p_down = 1.0 - p_up;

        // Determine best side
        let (side_token, side_outcome, side_price, side_conf) = match (ask_up, ask_down) {
            (Some(a_up), Some(a_down)) => {
                let edge_up = p_up - a_up;
                let edge_down = p_down - a_down;
                if edge_up >= edge_down && edge_up > 0.0 {
                    (token_up, "Up".to_string(), a_up, p_up)
                } else if edge_down > 0.0 {
                    (token_down, "Down".to_string(), a_down, p_down)
                } else {
                    // No positive edge
                    self.metrics.skipped_no_edge.fetch_add(1, Ordering::Relaxed);
                    return Ok(None);
                }
            }
            (Some(a_up), None) if p_up > a_up => (token_up, "Up".to_string(), a_up, p_up),
            (None, Some(a_down)) if p_down > a_down => {
                (token_down, "Down".to_string(), a_down, p_down)
            }
            _ => {
                self.metrics.skipped_no_edge.fetch_add(1, Ordering::Relaxed);
                return Ok(None);
            }
        };

        let edge = side_conf - side_price;

        // === JUMP REGIME DETECTION ===
        let jump_regime = rnjd_estimate.jump_regime || {
            let tracker = self.state.belief_vol_tracker.read();
            tracker.count_recent_jumps(
                &slug,
                self.config.jump_window_sec,
                now,
                self.config.jump_z_threshold,
            ) >= self.config.jump_count_threshold
        };

        // Compute effective minimum edge (higher during jump regime)
        let effective_min_edge = if jump_regime {
            self.config.min_edge * self.config.jump_regime_edge_mult
        } else {
            self.config.min_edge
        };

        // Edge check
        if edge < effective_min_edge {
            if jump_regime {
                self.metrics
                    .skipped_jump_regime
                    .fetch_add(1, Ordering::Relaxed);
                debug!(
                    slug = %slug,
                    edge = edge,
                    required = effective_min_edge,
                    "Edge below threshold during jump regime"
                );
            } else {
                self.metrics.skipped_no_edge.fetch_add(1, Ordering::Relaxed);
            }
            asset_state.last_p_up = p_up;
            asset_state.last_edge = edge;
            return Ok(None);
        }

        // Get bankroll from paper ledger
        let bankroll = self.state.vault.ledger.lock().await.cash_usdc;
        if bankroll <= 0.0 {
            return Ok(None);
        }

        let kelly_params = KellyParams {
            bankroll,
            kelly_fraction: self.config.kelly_fraction,
            max_position_pct: self.config.max_position_pct,
            min_position_usd: self.config.min_position_usd,
        };

        // === VOL-ADJUSTED KELLY with belief volatility ===
        let sigma_b = {
            let tracker = self.state.belief_vol_tracker.read();
            tracker.get_sigma_b(&slug)
        };
        let t_years = t_rem as f64 / (365.25 * 24.0 * 3600.0);

        let kelly = kelly_with_belief_vol(side_conf, side_price, sigma_b, t_years, &kelly_params);

        if !kelly.should_trade {
            self.metrics.skipped_kelly.fetch_add(1, Ordering::Relaxed);
            debug!(
                slug = %slug,
                skip_reason = ?kelly.skip_reason,
                sigma_b = sigma_b,
                "Vol-adjusted Kelly skip"
            );
            return Ok(None);
        }

        // Apply position caps
        let mut notional = kelly.position_size_usd;
        notional = notional.min(self.config.max_position_usd);
        notional = notional.min(bankroll * self.config.max_position_pct);

        if notional < self.config.min_position_usd {
            return Ok(None);
        }

        // === EXECUTE TRADE ===
        let client_order_id = Uuid::new_v4().to_string();
        let req = OrderRequest {
            client_order_id: client_order_id.clone(),
            token_id: side_token.clone(),
            side: OrderSide::Buy,
            price: side_price,
            notional_usdc: notional,
            tif: TimeInForce::Ioc,
            market_slug: Some(slug.clone()),
            outcome: Some(side_outcome.clone()),
        };

        let ack = self.exec.place_order(req.clone()).await?;

        // Update paper ledger
        {
            let mut ledger = self.state.vault.ledger.lock().await;
            ledger.apply_buy(
                &side_token,
                &side_outcome,
                ack.filled_price,
                ack.filled_notional_usdc,
                ack.fees_usdc,
            );
        }

        // Update asset state
        asset_state.last_trade_ts = now;
        asset_state.last_eval_ts = now;
        asset_state.last_p_up = p_up;
        asset_state.last_edge = edge;
        asset_state.consecutive_skips = 0;

        // Calculate tick-to-trade latency
        let t2t_us = eval_start.elapsed().as_micros() as u64;

        // Create trade record
        let trade = HftPaperTrade {
            id: ack.order_id.clone(),
            timestamp: ack.filled_at,
            asset: asset.as_str().to_string(),
            market_slug: slug.clone(),
            side: "BUY".to_string(),
            outcome: side_outcome.clone(),
            price: ack.filled_price,
            notional_usd: ack.filled_notional_usdc,
            shares: ack.filled_notional_usdc / ack.filled_price.max(1e-9),
            fees_usd: ack.fees_usdc,
            p_estimate: side_conf,
            edge,
            sigma_b,
            jump_regime,
            rnjd_confidence: rnjd_estimate.confidence,
            t2t_us,
        };

        self.metrics.record_trade(trade.clone());

        info!(
            slug = %slug,
            outcome = %side_outcome,
            price = ack.filled_price,
            notional = ack.filled_notional_usdc,
            edge = edge,
            sigma_b = sigma_b,
            jump_regime = jump_regime,
            t2t_us = t2t_us,
            "HFT_PAPER trade executed"
        );

        Ok(Some(trade))
    }

    /// Run the strategy loop (spawned as a task)
    pub async fn run(mut self, mut price_rx: broadcast::Receiver<PriceUpdateEvent>) {
        info!("HFT Paper Strategy started with RN-JD core");
        info!(
            min_edge = self.config.min_edge,
            kelly_fraction = self.config.kelly_fraction,
            max_position_pct = self.config.max_position_pct,
            "Strategy configuration"
        );

        let mut event_count: u64 = 0;
        let mut last_log_time = Instant::now();

        loop {
            if self.shutdown.load(Ordering::Acquire) {
                info!("HFT Paper Strategy shutting down");
                break;
            }

            match price_rx.recv().await {
                Ok(event) => {
                    event_count += 1;
                    // Log every 10 seconds with summary
                    if last_log_time.elapsed() > Duration::from_secs(10) {
                        let summary = self.metrics.summary();
                        info!(
                            events = event_count,
                            evaluations = summary.evaluations,
                            trades = summary.trades_executed,
                            skipped_no_book = summary.skipped_no_book,
                            skipped_no_edge = summary.skipped_no_edge,
                            skipped_cooldown = summary.skipped_cooldown,
                            "HFT_PAPER periodic stats"
                        );
                        last_log_time = Instant::now();
                    }
                    if let Err(e) = self.on_price_update(event).await {
                        warn!(error = %e, "HFT Paper Strategy error");
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    warn!(skipped = n, "HFT Paper Strategy receiver lagged");
                }
                Err(broadcast::error::RecvError::Closed) => {
                    info!("Price channel closed, HFT Paper Strategy stopping");
                    break;
                }
            }
        }

        // Log final metrics
        let summary = self.metrics.summary();
        info!(
            trades = summary.trades_executed,
            evaluations = summary.evaluations,
            skipped_cooldown = summary.skipped_cooldown,
            skipped_no_edge = summary.skipped_no_edge,
            skipped_jump = summary.skipped_jump_regime,
            "HFT Paper Strategy final metrics"
        );
    }
}

// ============================================================================
// Spawn Helper
// ============================================================================

/// Spawn the HFT Paper Strategy
pub fn spawn_hft_paper_strategy(
    state: Arc<AppState>,
    config: HftPaperStrategyConfig,
) -> Arc<HftPaperMetrics> {
    let price_rx = state.binance_feed.subscribe();
    let strategy = HftPaperStrategy::new(state, config);
    let metrics = strategy.metrics();

    tokio::spawn(strategy.run(price_rx));

    metrics
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_defaults() {
        let cfg = HftPaperStrategyConfig::default();

        // Verify conservative defaults
        assert!(cfg.min_edge >= 0.01, "Min edge should be at least 1%");
        assert!(
            cfg.kelly_fraction <= 0.05,
            "Kelly should be conservative (<= 5%)"
        );
        assert!(
            cfg.max_position_pct <= 0.01,
            "Max position should be small (<= 1%)"
        );
        assert!(
            cfg.max_position_usd <= 200.0,
            "Max position USD should be capped"
        );
    }

    #[test]
    fn test_metrics_new() {
        let metrics = HftPaperMetrics::new();
        assert_eq!(metrics.price_updates.load(Ordering::Relaxed), 0);
        assert_eq!(metrics.trades_executed.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn test_metrics_summary() {
        let metrics = HftPaperMetrics::new();
        metrics.price_updates.store(100, Ordering::Relaxed);
        metrics.evaluations.store(50, Ordering::Relaxed);
        metrics.trades_executed.store(5, Ordering::Relaxed);

        let summary = metrics.summary();
        assert_eq!(summary.price_updates, 100);
        assert_eq!(summary.evaluations, 50);
        assert_eq!(summary.trades_executed, 5);
    }
}

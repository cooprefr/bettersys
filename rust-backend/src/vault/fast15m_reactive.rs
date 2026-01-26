//! Event-Driven FAST15M Engine
//!
//! Replaces polling-based architecture with reactive price-driven execution.
//! Target: reduce tick-to-trade latency from 0-2000ms to <10ms.
//!
//! Architecture:
//! - BinancePriceFeed broadcasts price updates via tokio::sync::broadcast
//! - FAST15M engine subscribes and reacts within microseconds
//! - Adaptive gating: evaluate only when price crosses edge threshold
//! - Window-aware: aggressive in first 60s, idle in last 60s

use anyhow::Result;
use chrono::Utc;
use parking_lot::RwLock;
use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::{broadcast, Mutex};
use tracing::{debug, info, trace, warn};
use uuid::Uuid;

use crate::{
    scrapers::polymarket_gamma,
    vault::{
        calculate_kelly_position, p_up_driftless_lognormal, shrink_to_half, ExecutionAdapter,
        KellyParams, OrderRequest, OrderSide, TimeInForce, UpDown15mMarket, UpDownAsset,
        VaultActivityRecord, VaultNavSnapshotRecord,
    },
    AppState,
};

use super::engine::best_ask;

// Re-export PriceUpdateEvent from binance_price_feed
pub use crate::scrapers::binance_price_feed::PriceUpdateEvent;

/// Latency span for a single trade attempt
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct TradeSpan {
    pub span_id: String,
    pub market_slug: String,
    pub asset: String,

    // Timestamps (nanoseconds since arbitrary epoch via quanta/Instant)
    pub price_received_ns: u64,
    pub evaluation_start_ns: u64,
    pub gamma_lookup_done_ns: u64,
    pub book_fetch_done_ns: u64,
    pub kelly_done_ns: u64,
    pub order_submitted_ns: u64,
    pub order_acked_ns: u64,
    pub ledger_updated_ns: u64,

    // Derived latencies (microseconds)
    pub latency_to_eval_us: u64,
    pub latency_gamma_us: u64,
    pub latency_book_us: u64,
    pub latency_kelly_us: u64,
    pub latency_submit_us: u64,
    pub latency_ledger_us: u64,
    pub latency_total_us: u64,

    // Cache hits
    pub gamma_cache_hit: bool,
    pub book_cache_hit: bool,

    // Outcome
    pub traded: bool,
    pub skip_reason: Option<String>,
}

impl TradeSpan {
    pub fn new(market_slug: &str, asset: &str, price_received_ns: u64) -> Self {
        Self {
            span_id: Uuid::new_v4().to_string(),
            market_slug: market_slug.to_string(),
            asset: asset.to_string(),
            price_received_ns,
            ..Default::default()
        }
    }

    pub fn finalize(&mut self) {
        let ns_to_us = |ns: u64| -> u64 { ns / 1000 };

        if self.evaluation_start_ns > self.price_received_ns {
            self.latency_to_eval_us = ns_to_us(self.evaluation_start_ns - self.price_received_ns);
        }
        if self.gamma_lookup_done_ns > self.evaluation_start_ns {
            self.latency_gamma_us = ns_to_us(self.gamma_lookup_done_ns - self.evaluation_start_ns);
        }
        if self.book_fetch_done_ns > self.gamma_lookup_done_ns {
            self.latency_book_us = ns_to_us(self.book_fetch_done_ns - self.gamma_lookup_done_ns);
        }
        if self.kelly_done_ns > self.book_fetch_done_ns {
            self.latency_kelly_us = ns_to_us(self.kelly_done_ns - self.book_fetch_done_ns);
        }
        if self.order_submitted_ns > self.kelly_done_ns {
            self.latency_submit_us = ns_to_us(self.order_submitted_ns - self.kelly_done_ns);
        }
        if self.ledger_updated_ns > self.order_acked_ns {
            self.latency_ledger_us = ns_to_us(self.ledger_updated_ns - self.order_acked_ns);
        }

        let end_ns = if self.traded {
            self.ledger_updated_ns
        } else {
            self.kelly_done_ns
                .max(self.book_fetch_done_ns)
                .max(self.gamma_lookup_done_ns)
                .max(self.evaluation_start_ns)
        };

        if end_ns > self.price_received_ns {
            self.latency_total_us = ns_to_us(end_ns - self.price_received_ns);
        }
    }
}

/// Simple HDR-like histogram using pre-allocated buckets
/// Covers 1us to 10s with logarithmic buckets
#[derive(Debug)]
pub struct LatencyHistogram {
    buckets: Vec<u64>,
    bucket_bounds_us: Vec<u64>,
    count: u64,
    sum_us: u64,
    min_us: u64,
    max_us: u64,
}

impl Default for LatencyHistogram {
    fn default() -> Self {
        Self::new()
    }
}

impl LatencyHistogram {
    pub fn new() -> Self {
        // Logarithmic buckets from 1us to 10s
        let bucket_bounds_us: Vec<u64> = vec![
            1,
            2,
            5,
            10,
            20,
            50,
            100,
            200,
            500, // microseconds
            1_000,
            2_000,
            5_000,
            10_000,
            20_000,
            50_000, // milliseconds (as us)
            100_000,
            200_000,
            500_000,
            1_000_000,
            2_000_000,
            5_000_000,
            10_000_000, // seconds
            u64::MAX,
        ];
        let buckets = vec![0u64; bucket_bounds_us.len()];

        Self {
            buckets,
            bucket_bounds_us,
            count: 0,
            sum_us: 0,
            min_us: u64::MAX,
            max_us: 0,
        }
    }

    pub fn record(&mut self, latency_us: u64) {
        self.count += 1;
        self.sum_us = self.sum_us.saturating_add(latency_us);
        self.min_us = self.min_us.min(latency_us);
        self.max_us = self.max_us.max(latency_us);

        for (i, &bound) in self.bucket_bounds_us.iter().enumerate() {
            if latency_us <= bound {
                self.buckets[i] += 1;
                break;
            }
        }
    }

    pub fn percentile(&self, p: f64) -> u64 {
        if self.count == 0 {
            return 0;
        }

        let target = ((p / 100.0) * self.count as f64).ceil() as u64;
        let mut cumulative = 0u64;

        for (i, &bucket_count) in self.buckets.iter().enumerate() {
            cumulative += bucket_count;
            if cumulative >= target {
                return self.bucket_bounds_us[i];
            }
        }

        self.max_us
    }

    pub fn p50(&self) -> u64 {
        self.percentile(50.0)
    }
    pub fn p95(&self) -> u64 {
        self.percentile(95.0)
    }
    pub fn p99(&self) -> u64 {
        self.percentile(99.0)
    }
    pub fn p999(&self) -> u64 {
        self.percentile(99.9)
    }
    pub fn mean(&self) -> f64 {
        if self.count == 0 {
            0.0
        } else {
            self.sum_us as f64 / self.count as f64
        }
    }
    pub fn count(&self) -> u64 {
        self.count
    }
    pub fn min(&self) -> u64 {
        if self.count == 0 {
            0
        } else {
            self.min_us
        }
    }
    pub fn max(&self) -> u64 {
        self.max_us
    }
}

/// Latency registry for all FAST15M components
#[derive(Debug, Default)]
pub struct Fast15mLatencyRegistry {
    pub total_t2t: LatencyHistogram,
    pub gamma_lookup: LatencyHistogram,
    pub book_fetch: LatencyHistogram,
    pub kelly_calc: LatencyHistogram,
    pub order_submit: LatencyHistogram,
    pub ledger_update: LatencyHistogram,

    // Cache hit rates
    pub gamma_cache_hits: u64,
    pub gamma_cache_misses: u64,
    pub book_cache_hits: u64,
    pub book_cache_misses: u64,

    // Trade outcomes
    pub evaluations: u64,
    pub trades_executed: u64,
    pub trades_skipped_no_edge: u64,
    pub trades_skipped_cooldown: u64,
    pub trades_skipped_window: u64,
    pub trades_skipped_no_data: u64,

    // Recent spans for debugging
    pub recent_spans: std::collections::VecDeque<TradeSpan>,
}

impl Fast15mLatencyRegistry {
    pub fn new() -> Self {
        Self {
            recent_spans: std::collections::VecDeque::with_capacity(100),
            ..Default::default()
        }
    }

    pub fn record_span(&mut self, span: TradeSpan) {
        self.evaluations += 1;

        if span.latency_total_us > 0 {
            self.total_t2t.record(span.latency_total_us);
        }
        if span.latency_gamma_us > 0 {
            self.gamma_lookup.record(span.latency_gamma_us);
        }
        if span.latency_book_us > 0 {
            self.book_fetch.record(span.latency_book_us);
        }
        if span.latency_kelly_us > 0 {
            self.kelly_calc.record(span.latency_kelly_us);
        }
        if span.latency_submit_us > 0 {
            self.order_submit.record(span.latency_submit_us);
        }
        if span.latency_ledger_us > 0 {
            self.ledger_update.record(span.latency_ledger_us);
        }

        if span.gamma_cache_hit {
            self.gamma_cache_hits += 1;
        } else {
            self.gamma_cache_misses += 1;
        }

        if span.book_cache_hit {
            self.book_cache_hits += 1;
        } else {
            self.book_cache_misses += 1;
        }

        if span.traded {
            self.trades_executed += 1;
        } else if let Some(reason) = &span.skip_reason {
            match reason.as_str() {
                "no_edge" => self.trades_skipped_no_edge += 1,
                "cooldown" => self.trades_skipped_cooldown += 1,
                "window" => self.trades_skipped_window += 1,
                _ => self.trades_skipped_no_data += 1,
            }
        }

        // Keep last 100 spans
        if self.recent_spans.len() >= 100 {
            self.recent_spans.pop_front();
        }
        self.recent_spans.push_back(span);
    }

    pub fn summary(&self) -> LatencySummary {
        LatencySummary {
            evaluations: self.evaluations,
            trades_executed: self.trades_executed,
            t2t_p50_us: self.total_t2t.p50(),
            t2t_p95_us: self.total_t2t.p95(),
            t2t_p99_us: self.total_t2t.p99(),
            t2t_p999_us: self.total_t2t.p999(),
            t2t_mean_us: self.total_t2t.mean(),
            gamma_cache_hit_rate: if self.gamma_cache_hits + self.gamma_cache_misses > 0 {
                self.gamma_cache_hits as f64
                    / (self.gamma_cache_hits + self.gamma_cache_misses) as f64
            } else {
                0.0
            },
            book_cache_hit_rate: if self.book_cache_hits + self.book_cache_misses > 0 {
                self.book_cache_hits as f64 / (self.book_cache_hits + self.book_cache_misses) as f64
            } else {
                0.0
            },
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct LatencySummary {
    pub evaluations: u64,
    pub trades_executed: u64,
    pub t2t_p50_us: u64,
    pub t2t_p95_us: u64,
    pub t2t_p99_us: u64,
    pub t2t_p999_us: u64,
    pub t2t_mean_us: f64,
    pub gamma_cache_hit_rate: f64,
    pub book_cache_hit_rate: f64,
}

/// Per-asset state for edge-threshold gating
#[derive(Debug, Clone)]
struct AssetState {
    last_p_up: f64,
    last_edge: f64,
    last_trade_ts: i64,
    last_eval_ts: i64,
}

impl Default for AssetState {
    fn default() -> Self {
        Self {
            last_p_up: 0.5,
            last_edge: 0.0,
            last_trade_ts: 0,
            last_eval_ts: 0,
        }
    }
}

/// Configuration for reactive FAST15M engine
#[derive(Debug, Clone)]
pub struct ReactiveFast15mConfig {
    pub min_edge: f64,
    pub kelly_fraction: f64,
    pub max_position_pct: f64,
    pub shrink_to_half: f64,
    pub cooldown_sec: i64,

    // Reactive gating thresholds
    pub edge_change_threshold: f64, // Re-evaluate if edge changes by this much
    pub min_eval_interval_ms: u64,  // Minimum time between evaluations per asset
    pub window_start_aggressive_sec: i64, // Aggressive evaluation in first N seconds
    pub window_end_idle_sec: i64,   // Stop evaluating in last N seconds
}

impl Default for ReactiveFast15mConfig {
    fn default() -> Self {
        Self {
            min_edge: 0.01,
            kelly_fraction: 0.05,
            max_position_pct: 0.01,
            shrink_to_half: 0.35,
            cooldown_sec: 30,
            edge_change_threshold: 0.005, // 0.5% edge change triggers re-eval
            min_eval_interval_ms: 50,     // Max 20 evals/sec per asset
            window_start_aggressive_sec: 60,
            window_end_idle_sec: 60,
        }
    }
}

impl ReactiveFast15mConfig {
    pub fn from_env() -> Self {
        let mut cfg = Self::default();

        if let Ok(v) = std::env::var("REACTIVE_FAST15M_MIN_EDGE") {
            if let Ok(val) = v.parse::<f64>() {
                if val.is_finite() && val > 0.0 {
                    cfg.min_edge = val;
                }
            }
        }
        if let Ok(v) = std::env::var("REACTIVE_FAST15M_KELLY_FRACTION") {
            if let Ok(val) = v.parse::<f64>() {
                if val.is_finite() && val > 0.0 && val <= 1.0 {
                    cfg.kelly_fraction = val;
                }
            }
        }
        if let Ok(v) = std::env::var("REACTIVE_FAST15M_MAX_POSITION_PCT") {
            if let Ok(val) = v.parse::<f64>() {
                if val.is_finite() && val > 0.0 && val <= 1.0 {
                    cfg.max_position_pct = val;
                }
            }
        }
        if let Ok(v) = std::env::var("REACTIVE_FAST15M_SHRINK") {
            if let Ok(val) = v.parse::<f64>() {
                if val.is_finite() && val > 0.0 && val <= 1.0 {
                    cfg.shrink_to_half = val;
                }
            }
        }
        if let Ok(v) = std::env::var("REACTIVE_FAST15M_COOLDOWN_SEC") {
            if let Ok(val) = v.parse::<i64>() {
                if val >= 0 {
                    cfg.cooldown_sec = val;
                }
            }
        }
        if let Ok(v) = std::env::var("REACTIVE_FAST15M_EDGE_THRESHOLD") {
            if let Ok(val) = v.parse::<f64>() {
                if val.is_finite() && val > 0.0 {
                    cfg.edge_change_threshold = val;
                }
            }
        }
        if let Ok(v) = std::env::var("REACTIVE_FAST15M_MIN_EVAL_INTERVAL_MS") {
            if let Ok(val) = v.parse::<u64>() {
                cfg.min_eval_interval_ms = val;
            }
        }

        cfg
    }
}

/// Event-driven FAST15M engine
pub struct ReactiveFast15mEngine<E: ExecutionAdapter> {
    state: Arc<AppState>,
    exec: Arc<E>,
    cfg: ReactiveFast15mConfig,
    asset_state: HashMap<UpDownAsset, AssetState>,
    token_cache: HashMap<String, (String, String)>, // slug -> (token_up, token_down)
    latency_registry: Arc<RwLock<Fast15mLatencyRegistry>>,
}

impl<E: ExecutionAdapter + Send + Sync + 'static> ReactiveFast15mEngine<E> {
    pub fn new(
        state: Arc<AppState>,
        exec: Arc<E>,
        cfg: ReactiveFast15mConfig,
        latency_registry: Arc<RwLock<Fast15mLatencyRegistry>>,
    ) -> Self {
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
            state,
            exec,
            cfg,
            asset_state,
            token_cache: HashMap::new(),
            latency_registry,
        }
    }

    /// Get current nanosecond timestamp
    #[inline]
    fn now_ns() -> u64 {
        // Use Instant for monotonic timing within process
        static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
        let start = START.get_or_init(Instant::now);
        start.elapsed().as_nanos() as u64
    }

    /// Process a price update event
    pub async fn on_price_update(&mut self, event: PriceUpdateEvent) -> Result<Option<TradeSpan>> {
        let now = Utc::now().timestamp();
        let now_ms = Utc::now().timestamp_millis();

        // Determine which asset this is
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

        // Window gating: skip if in last N seconds (too late to trade profitably)
        if t_rem < self.cfg.window_end_idle_sec {
            return Ok(None);
        }

        let slug = format!("{}-updown-15m-{}", asset.as_str(), start_ts);
        let mut span = TradeSpan::new(&slug, asset.as_str(), event.received_at_ns);

        // Get asset state
        let asset_state = self.asset_state.entry(asset).or_default();

        // Cooldown check
        if now - asset_state.last_trade_ts < self.cfg.cooldown_sec {
            span.skip_reason = Some("cooldown".to_string());
            span.finalize();
            self.latency_registry.write().record_span(span.clone());
            return Ok(Some(span));
        }

        // Rate limiting: min interval between evaluations
        let min_interval_sec = self.cfg.min_eval_interval_ms as i64 / 1000;
        if now - asset_state.last_eval_ts < min_interval_sec.max(1)
            && t_elapsed > self.cfg.window_start_aggressive_sec
        {
            // Outside aggressive window, enforce rate limit
            return Ok(None);
        }

        // Get price data for p_up calculation
        let Some(p_start) = self
            .state
            .binance_feed
            .mid_near(event.symbol.as_str(), start_ts, 60)
            .map(|p| p.mid)
        else {
            span.skip_reason = Some("no_start_price".to_string());
            span.finalize();
            return Ok(Some(span));
        };

        let Some(sigma) = self.state.binance_feed.sigma_per_sqrt_s(&event.symbol) else {
            span.skip_reason = Some("no_sigma".to_string());
            span.finalize();
            return Ok(Some(span));
        };

        // Calculate p_up
        let Some(p_up_raw) = p_up_driftless_lognormal(p_start, event.mid, sigma, t_rem as f64)
        else {
            span.skip_reason = Some("p_up_calc_failed".to_string());
            span.finalize();
            return Ok(Some(span));
        };

        let p_up = shrink_to_half(p_up_raw, self.cfg.shrink_to_half);
        let p_down = 1.0 - p_up;

        // Edge-threshold gating: only proceed if edge changed significantly
        // or we're in the aggressive window
        let edge_estimate = (p_up - 0.5).abs(); // Rough edge from fair value
        let edge_change = (edge_estimate - asset_state.last_edge).abs();

        if edge_change < self.cfg.edge_change_threshold
            && t_elapsed > self.cfg.window_start_aggressive_sec
        {
            // Edge hasn't changed enough, skip evaluation
            asset_state.last_p_up = p_up;
            asset_state.last_edge = edge_estimate;
            return Ok(None);
        }

        // Mark evaluation start
        span.evaluation_start_ns = Self::now_ns();
        asset_state.last_eval_ts = now;
        asset_state.last_p_up = p_up;
        asset_state.last_edge = edge_estimate;

        // Resolve token IDs (cached)
        let (token_up, token_down) = match self.token_cache.get(&slug).cloned() {
            Some(t) => {
                span.gamma_cache_hit = true;
                t
            }
            None => {
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
                    span.skip_reason = Some("no_tokens".to_string());
                    span.gamma_lookup_done_ns = Self::now_ns();
                    span.finalize();
                    self.latency_registry.write().record_span(span.clone());
                    return Ok(Some(span));
                }

                span.gamma_cache_hit = false;
                self.token_cache
                    .insert(slug.clone(), (up.clone(), down.clone()));
                (up, down)
            }
        };
        span.gamma_lookup_done_ns = Self::now_ns();

        // Fetch orderbook from cache ONLY - no REST fallback in hot path
        // If cache misses, we skip this tick (skip-tick semantics)
        self.state.polymarket_market_ws.request_subscribe(&token_up);
        self.state
            .polymarket_market_ws
            .request_subscribe(&token_down);

        // Cache-only lookups with 1500ms staleness threshold for FAST15M
        let ask_up = self
            .state
            .polymarket_market_ws
            .get_orderbook(&token_up, 1500)
            .and_then(|book| book.asks.first().map(|o| o.price));

        let ask_down = self
            .state
            .polymarket_market_ws
            .get_orderbook(&token_down, 1500)
            .and_then(|book| book.asks.first().map(|o| o.price));

        // Track cache hit status
        span.book_cache_hit = ask_up.is_some() || ask_down.is_some();

        span.book_fetch_done_ns = Self::now_ns();

        // Determine best side
        let (side_token, side_outcome, side_price, side_conf) = match (ask_up, ask_down) {
            (Some(a_up), Some(a_down)) => {
                let edge_up = p_up - a_up;
                let edge_down = p_down - a_down;
                if edge_up >= edge_down {
                    (token_up, "Up".to_string(), a_up, p_up)
                } else {
                    (token_down, "Down".to_string(), a_down, p_down)
                }
            }
            (Some(a_up), None) => (token_up, "Up".to_string(), a_up, p_up),
            (None, Some(a_down)) => (token_down, "Down".to_string(), a_down, p_down),
            (None, None) => {
                span.skip_reason = Some("no_asks".to_string());
                span.finalize();
                self.latency_registry.write().record_span(span.clone());
                return Ok(Some(span));
            }
        };

        // Edge check
        let edge = side_conf - side_price;
        if edge < self.cfg.min_edge {
            span.skip_reason = Some("no_edge".to_string());
            span.kelly_done_ns = Self::now_ns();
            span.finalize();
            self.latency_registry.write().record_span(span.clone());
            return Ok(Some(span));
        }

        // Kelly sizing
        let bankroll = self.state.vault.ledger.lock().await.cash_usdc;
        if bankroll <= 0.0 {
            span.skip_reason = Some("no_bankroll".to_string());
            span.kelly_done_ns = Self::now_ns();
            span.finalize();
            self.latency_registry.write().record_span(span.clone());
            return Ok(Some(span));
        }

        let kelly_params = KellyParams {
            bankroll,
            kelly_fraction: self.cfg.kelly_fraction,
            max_position_pct: self.cfg.max_position_pct,
            min_position_usd: 1.0,
        };
        let kelly = calculate_kelly_position(side_conf, side_price, &kelly_params);
        span.kelly_done_ns = Self::now_ns();

        if !kelly.should_trade {
            span.skip_reason = Some("kelly_skip".to_string());
            span.finalize();
            self.latency_registry.write().record_span(span.clone());
            return Ok(Some(span));
        }

        // Submit order
        let req = OrderRequest {
            client_order_id: Uuid::new_v4().to_string(),
            token_id: side_token.clone(),
            side: OrderSide::Buy,
            price: side_price,
            notional_usdc: kelly.position_size_usd,
            tif: TimeInForce::Ioc,
            market_slug: Some(slug.clone()),
            outcome: Some(side_outcome.clone()),
        };

        span.order_submitted_ns = Self::now_ns();
        let ack = self.exec.place_order(req.clone()).await?;
        span.order_acked_ns = Self::now_ns();

        // Update ledger
        if req.side == OrderSide::Buy {
            let cash_usdc = {
                let mut ledger = self.state.vault.ledger.lock().await;
                ledger.apply_buy(
                    &req.token_id,
                    req.outcome.as_deref().unwrap_or(""),
                    ack.filled_price,
                    ack.filled_notional_usdc,
                    ack.fees_usdc,
                );
                ledger.cash_usdc
            };

            let total_shares = self.state.vault.shares.lock().await.total_shares;
            let _ = self
                .state
                .vault
                .db
                .upsert_state(cash_usdc, total_shares, ack.filled_at)
                .await;

            let (nav_usdc, positions_value_usdc, nav_per_share) = {
                let ledger = self.state.vault.ledger.lock().await;
                let nav = crate::vault::approximate_nav_usdc(&ledger);
                let pos_v = (nav - ledger.cash_usdc).max(0.0);
                let nav_ps = if total_shares > 0.0 {
                    nav / total_shares
                } else {
                    1.0
                };
                (nav, pos_v, nav_ps)
            };

            let _ = self
                .state
                .vault
                .db
                .insert_activity(&VaultActivityRecord {
                    id: ack.order_id.clone(),
                    ts: ack.filled_at,
                    kind: "TRADE".to_string(),
                    wallet_address: None,
                    amount_usdc: None,
                    shares: Some(ack.filled_notional_usdc / ack.filled_price.max(1e-9)),
                    token_id: Some(req.token_id.clone()),
                    market_slug: req.market_slug.clone(),
                    outcome: req.outcome.clone(),
                    side: Some("BUY".to_string()),
                    price: Some(ack.filled_price),
                    notional_usdc: Some(ack.filled_notional_usdc),
                    strategy: Some("FAST15M_REACTIVE".to_string()),
                    decision_id: None,
                })
                .await;

            let _ = self
                .state
                .vault
                .db
                .insert_nav_snapshot(&VaultNavSnapshotRecord {
                    id: Uuid::new_v4().to_string(),
                    ts: ack.filled_at,
                    nav_usdc,
                    cash_usdc,
                    positions_value_usdc,
                    total_shares,
                    nav_per_share,
                    source: "trade:FAST15M_REACTIVE".to_string(),
                })
                .await;
        }

        span.ledger_updated_ns = Self::now_ns();
        span.traded = true;

        // Update asset state
        if let Some(state) = self.asset_state.get_mut(&asset) {
            state.last_trade_ts = now;
        }

        span.finalize();
        self.latency_registry.write().record_span(span.clone());

        // Record comprehensive metrics for the trade
        let comp = crate::latency::global_comprehensive();
        comp.t2t.record_stage(
            crate::latency::T2TStage::SignalCompute,
            span.gamma_lookup_done_ns
                .saturating_sub(span.evaluation_start_ns)
                / 1000,
        );
        comp.t2t.record_stage(
            crate::latency::T2TStage::RiskCheck,
            span.kelly_done_ns.saturating_sub(span.book_fetch_done_ns) / 1000,
        );
        comp.t2t.record_stage(
            crate::latency::T2TStage::OrderBuild,
            span.order_submitted_ns.saturating_sub(span.kelly_done_ns) / 1000,
        );
        comp.t2t.record_stage(
            crate::latency::T2TStage::WireSend,
            span.order_acked_ns.saturating_sub(span.order_submitted_ns) / 1000,
        );
        comp.t2t
            .record_stage(crate::latency::T2TStage::Total, span.latency_total_us);

        comp.throughput
            .strategy_evals
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        comp.throughput
            .signals_generated
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        comp.throughput
            .orders_sent
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        comp.throughput
            .orders_filled
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        comp.order_lifecycle
            .orders_created
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        comp.order_lifecycle
            .orders_sent
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        comp.order_lifecycle
            .orders_acked
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        comp.order_lifecycle
            .orders_filled
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        info!(
            market_slug = %slug,
            asset = %asset.as_str(),
            price = side_price,
            edge = edge,
            notional = kelly.position_size_usd,
            latency_us = span.latency_total_us,
            "FAST15M_REACTIVE trade"
        );

        Ok(Some(span))
    }
}

/// Spawn the reactive FAST15M engine
/// Returns a handle to the latency registry for monitoring
pub async fn spawn_reactive_fast15m<E: ExecutionAdapter + Send + Sync + 'static>(
    state: Arc<AppState>,
    exec: Arc<E>,
    cfg: ReactiveFast15mConfig,
    mut price_rx: broadcast::Receiver<PriceUpdateEvent>,
) -> Arc<RwLock<Fast15mLatencyRegistry>> {
    let registry = Arc::new(RwLock::new(Fast15mLatencyRegistry::new()));
    let mut engine = ReactiveFast15mEngine::new(state, exec, cfg, registry.clone());

    tokio::spawn(async move {
        loop {
            match price_rx.recv().await {
                Ok(event) => {
                    if let Err(e) = engine.on_price_update(event).await {
                        warn!(error = %e, "FAST15M_REACTIVE evaluation error");
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    warn!(skipped = n, "FAST15M_REACTIVE price receiver lagged");
                }
                Err(broadcast::error::RecvError::Closed) => {
                    info!("FAST15M_REACTIVE price channel closed, shutting down");
                    break;
                }
            }
        }
    });

    registry
}

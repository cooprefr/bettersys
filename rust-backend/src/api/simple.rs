//! Simplified API routes that just work
//! No complexity, just results
//!
//! Optimizations:
//! - Minimal allocations in hot paths
//! - Direct database access without intermediate layers

use crate::{
    models::{MarketSignal, SignalContext, SignalContextRecord, SignalType},
    scrapers::polymarket::OrderBook,
    scrapers::polymarket_gamma,
    signals::wallet_analytics::{
        get_or_compute_wallet_analytics, wallet_analytics_cache_key, CopyCurveModel, FrictionMode,
        WalletAnalytics, WalletAnalyticsParams, WALLET_ANALYTICS_CACHE_TTL_SECONDS,
    },
    vault::{
        VaultActivityRecord, VaultDepositRequest, VaultEngineConfig, VaultStateResponse,
        VaultWithdrawRequest,
    },
    AppState,
};
use axum::{
    extract::{Json as AxumJson, Query, State as AxumState},
    http::StatusCode,
    response::Json,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::HashMap, env, sync::OnceLock, time::Duration};
use tokio::sync::Mutex;
use tracing::warn;
use uuid::Uuid;

static WALLET_ANALYTICS_REFRESH_GUARD: OnceLock<Mutex<HashMap<String, i64>>> = OnceLock::new();

fn wallet_analytics_refresh_guard() -> &'static Mutex<HashMap<String, i64>> {
    WALLET_ANALYTICS_REFRESH_GUARD.get_or_init(|| Mutex::new(HashMap::new()))
}

#[derive(Debug, Deserialize)]
pub struct SignalQuery {
    pub limit: Option<usize>,
    pub before: Option<String>,
    pub before_id: Option<String>,
    /// If true, exclude signals from up/down markets (btc-updown, eth-updown, etc.)
    pub exclude_updown: Option<bool>,
    /// If true, include the full stored context payload. Defaults to lite context.
    pub full_context: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct SignalSearchQuery {
    pub q: String,
    pub limit: Option<usize>,
    pub before: Option<String>,
    pub before_id: Option<String>,
    pub exclude_updown: Option<bool>,
    pub min_confidence: Option<f64>,
    pub full_context: Option<bool>,
}

/// Patterns that identify up/down markets in market slugs
const UPDOWN_PATTERNS: &[&str] = &["updown", "up-or-down", "up-down"];

#[derive(Debug, Serialize)]
pub struct SignalWithContext {
    #[serde(flatten)]
    pub signal: MarketSignal,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<SignalContext>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_version: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_enriched_at: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct SignalResponse {
    pub signals: Vec<SignalWithContext>,
    pub count: usize,
    pub timestamp: String,
}

fn extract_quoted_market_title(s: &str) -> Option<String> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }

    // Fast, allocation-light parsing for strings like:
    // "TRACKED WALLET ENTRY: ~$1 BUY on 'Bitcoin Up or Down - ...' by 0x..."
    // Support both single and double quotes.
    let lower = s.to_ascii_lowercase();
    for (needle, quote) in [(" on '", '\''), (" on \"", '"')] {
        if let Some(pos) = lower.find(needle) {
            let start = pos + needle.len();
            if start >= s.len() {
                continue;
            }
            let rest = &s[start..];
            if let Some(end) = rest.find(quote) {
                let extracted = rest[..end].trim();
                if !extracted.is_empty() {
                    return Some(extracted.to_string());
                }
            }
        }
    }

    None
}

fn normalize_signal_market_title(mut signal: MarketSignal) -> MarketSignal {
    // Backward-compat: older tracked-wallet signals stored the *headline* in `details.market_title`.
    // Normalize it at the API boundary so the UI never has to deal with corrupted titles.
    if matches!(signal.signal_type, SignalType::TrackedWalletEntry { .. }) {
        let lower = signal.details.market_title.to_ascii_lowercase();
        let looks_like_headline = lower.starts_with("tracked wallet entry")
            || lower.starts_with("insider entry")
            || lower.starts_with("world class trader entry");
        if looks_like_headline {
            if let Some(extracted) = extract_quoted_market_title(&signal.details.market_title) {
                signal.details.market_title = extracted;
            }
        }
    }

    signal
}

#[derive(Debug, Serialize)]
pub struct SignalStatsResponse {
    pub total_signals: usize,
    pub high_confidence_count: usize,
    pub avg_confidence: f64,
    pub timestamp: String,
}

#[derive(Debug, Deserialize)]
pub struct SignalContextQuery {
    pub signal_id: String,
}

#[derive(Debug, Deserialize)]
pub struct SignalEnrichQuery {
    pub signal_id: String,
    #[serde(default)]
    pub levels: Option<usize>,
    #[serde(default)]
    pub fresh: Option<bool>,
}

#[derive(Debug, Serialize)]
pub struct Binance15mEnrichment {
    pub symbol: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mid: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_mid: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sigma_per_sqrt_s: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub t_rem_sec: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub p_up_raw: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub p_up_shrunk: Option<f64>,
}

#[derive(Debug, Serialize)]
pub struct SignalEnrichResponse {
    pub signal_id: String,
    pub market_slug: String,
    pub fetched_at: i64,
    pub is_updown_15m: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub up: Option<MarketSnapshotResponse>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub down: Option<MarketSnapshotResponse>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binance: Option<Binance15mEnrichment>,
}

#[derive(Debug, Deserialize)]
pub struct WalletAnalyticsQuery {
    pub wallet_address: String,
    pub force: Option<bool>,
    /// Friction mode for copy trading simulation: "optimistic", "base", or "pessimistic"
    pub friction_mode: Option<String>,
    /// Copy curve model: "scaled" (default) or "mtm".
    pub copy_model: Option<String>,
    /// If true, only return cached analytics. If not cached, returns 204.
    pub cached_only: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct WalletAnalyticsPrimeRequest {
    pub wallets: Vec<String>,
    pub force: Option<bool>,
    pub friction_mode: Option<String>,
    pub copy_model: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct WalletAnalyticsPrimeResponse {
    pub scheduled: usize,
    pub skipped: usize,
}

#[derive(Debug, Deserialize)]
pub struct MarketSnapshotQuery {
    /// Polymarket CLOB token id (a large integer string). If you only have a condition id / Dome token_id
    /// (0x...), pass `market_slug` + `outcome` instead.
    pub token_id: Option<String>,
    /// Polymarket market slug (preferred).
    pub market_slug: Option<String>,
    /// Outcome label (e.g. "Yes"/"No", "Up"/"Down").
    pub outcome: Option<String>,
    pub levels: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketSnapshotLevel {
    pub price: f64,
    pub size: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketSnapshotDepth {
    pub bps_10: f64,
    pub bps_25: f64,
    pub bps_50: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketSnapshotResponse {
    pub token_id: String,
    pub fetched_at: i64,
    pub best_bid: Option<f64>,
    pub best_ask: Option<f64>,
    pub mid: Option<f64>,
    pub spread: Option<f64>,
    pub depth: Option<MarketSnapshotDepth>,
    pub imbalance_10bps: Option<f64>,
    pub bids: Vec<MarketSnapshotLevel>,
    pub asks: Vec<MarketSnapshotLevel>,
}

/// Get enrichment context for a signal
pub async fn get_signal_context_simple(
    Query(params): Query<SignalContextQuery>,
    AxumState(state): AxumState<AppState>,
) -> Result<Json<SignalContextRecord>, StatusCode> {
    state
        .signal_storage
        .get_signal_context(&params.signal_id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .map(Json)
        .ok_or(StatusCode::NOT_FOUND)
}

/// Ephemeral enrichment for high-frequency 15m Up/Down markets.
pub async fn get_signal_enrich(
    Query(params): Query<SignalEnrichQuery>,
    AxumState(state): AxumState<AppState>,
) -> Result<Json<SignalEnrichResponse>, StatusCode> {
    let levels = params.levels.unwrap_or(10).clamp(1, 50);
    let fresh = params.fresh.unwrap_or(false);
    let now = Utc::now().timestamp();

    let signal = state
        .signal_storage
        .get_signal(&params.signal_id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    let market_slug = signal.market_slug.clone();
    let updown = crate::vault::parse_updown_15m_slug(&market_slug);
    let is_updown_15m = updown.is_some();

    if !is_updown_15m {
        return Ok(Json(SignalEnrichResponse {
            signal_id: signal.id,
            market_slug,
            fetched_at: now,
            is_updown_15m,
            up: None,
            down: None,
            binance: None,
        }));
    }

    let updown = updown.unwrap();

    let token_up = polymarket_gamma::resolve_clob_token_id_by_slug(
        state.signal_storage.as_ref(),
        &state.http_client,
        &market_slug,
        "Up",
    )
    .await
    .map_err(|_| StatusCode::BAD_GATEWAY)?
    .unwrap_or_default();
    let token_down = polymarket_gamma::resolve_clob_token_id_by_slug(
        state.signal_storage.as_ref(),
        &state.http_client,
        &market_slug,
        "Down",
    )
    .await
    .map_err(|_| StatusCode::BAD_GATEWAY)?
    .unwrap_or_default();

    let (up, down) = if token_up.is_empty() || token_down.is_empty() {
        (None, None)
    } else {
        let max_age_ms = if fresh { 250 } else { 5_000 };

        let up = get_orderbook_snapshot(&state, &token_up, levels, max_age_ms, fresh).await;
        let down = get_orderbook_snapshot(&state, &token_down, levels, max_age_ms, fresh).await;
        (up, down)
    };

    let symbol = updown.asset.binance_symbol().to_string();
    let mid = state.binance_feed.latest_mid(&symbol).map(|p| p.mid);
    let start_mid = state
        .binance_feed
        .mid_near(&symbol, updown.start_ts, 60)
        .map(|p| p.mid);
    let sigma = state.binance_feed.sigma_per_sqrt_s(&symbol);
    let t_rem_sec = (updown.end_ts - now).max(0) as f64;

    let (p_up_raw, p_up_shrunk) = match (start_mid, mid, sigma) {
        (Some(p_start), Some(p_now), Some(sigma)) if t_rem_sec > 0.0 => {
            let raw = crate::vault::p_up_driftless_lognormal(p_start, p_now, sigma, t_rem_sec);
            let shrunk = raw.map(|p| crate::vault::shrink_to_half(p, 0.35));
            (raw, shrunk)
        }
        _ => (None, None),
    };

    Ok(Json(SignalEnrichResponse {
        signal_id: signal.id,
        market_slug,
        fetched_at: now,
        is_updown_15m,
        up,
        down,
        binance: Some(Binance15mEnrichment {
            symbol,
            mid,
            start_mid,
            sigma_per_sqrt_s: sigma,
            t_rem_sec: Some(t_rem_sec),
            p_up_raw,
            p_up_shrunk,
        }),
    }))
}

/// Get per-wallet analytics (cached briefly for UI responsiveness):
/// - Wallet realized equity curve (Dome wallet/pnl)
/// - Copy-trade strategy curve (simulated follower with fixed notional per BUY)
pub async fn get_wallet_analytics(
    Query(params): Query<WalletAnalyticsQuery>,
    AxumState(state): AxumState<AppState>,
) -> Result<Json<WalletAnalytics>, StatusCode> {
    let Some(rest) = state.dome_rest.as_ref() else {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    };
    let now = Utc::now().timestamp();
    let force = params.force.unwrap_or(false);

    // Parse friction mode from query param (defaults to Base)
    let friction_mode = params
        .friction_mode
        .as_ref()
        .map(|s| FrictionMode::from_str(s))
        .unwrap_or(FrictionMode::Base);

    // Parse copy curve model from query param (defaults to Scaled)
    let copy_model = params
        .copy_model
        .as_ref()
        .map(|s| CopyCurveModel::from_str(s))
        .unwrap_or(CopyCurveModel::Scaled);

    let mut analytics_params = WalletAnalyticsParams::default();
    analytics_params.friction_mode = friction_mode;
    analytics_params.copy_model = copy_model;

    let cached_only = params.cached_only.unwrap_or(false);
    if cached_only {
        let cache_key =
            wallet_analytics_cache_key(&params.wallet_address, friction_mode, copy_model);
        if let Ok(Some((cache_json, _fetched_at))) = state.signal_storage.get_cache(&cache_key) {
            if let Ok(cached) = serde_json::from_str::<WalletAnalytics>(&cache_json) {
                return Ok(Json(cached));
            }
        }
        return Err(StatusCode::NO_CONTENT);
    }

    // SWR: if we have *any* cached blob, serve it immediately for UI snappiness.
    // If it's stale, kick off a background refresh so the next open is instant.
    if !force {
        let cache_key =
            wallet_analytics_cache_key(&params.wallet_address, friction_mode, copy_model);
        if let Ok(Some((cache_json, fetched_at))) = state.signal_storage.get_cache(&cache_key) {
            if let Ok(cached) = serde_json::from_str::<WalletAnalytics>(&cache_json) {
                if now - fetched_at > WALLET_ANALYTICS_CACHE_TTL_SECONDS {
                    // Avoid stampeding: refresh each wallet at most once per 30s.
                    let mut guard = wallet_analytics_refresh_guard().lock().await;
                    let last_started = guard.get(&cache_key).copied().unwrap_or(0);
                    if now - last_started >= 30 {
                        guard.insert(cache_key.clone(), now);
                        drop(guard);

                        let storage = state.signal_storage.clone();
                        let rest = rest.clone();
                        let wallet = params.wallet_address.clone();
                        let analytics_params = analytics_params.clone();

                        tokio::spawn(async move {
                            let now = Utc::now().timestamp();
                            let _ = get_or_compute_wallet_analytics(
                                &storage,
                                &rest,
                                &wallet,
                                true,
                                now,
                                analytics_params,
                            )
                            .await;
                        });
                    }
                }

                return Ok(Json(cached));
            }
        }
    }

    let analytics = get_or_compute_wallet_analytics(
        &state.signal_storage,
        rest,
        &params.wallet_address,
        force,
        now,
        analytics_params,
    )
    .await
    .map_err(|_| StatusCode::BAD_GATEWAY)?;

    Ok(Json(analytics))
}

/// Pre-warm wallet analytics caches for a batch of wallets.
pub async fn post_wallet_analytics_prime(
    AxumState(state): AxumState<AppState>,
    AxumJson(req): AxumJson<WalletAnalyticsPrimeRequest>,
) -> Result<Json<WalletAnalyticsPrimeResponse>, StatusCode> {
    let Some(rest) = state.dome_rest.as_ref() else {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    };
    let now = Utc::now().timestamp();
    let force = req.force.unwrap_or(false);

    let friction_mode = req
        .friction_mode
        .as_ref()
        .map(|s| FrictionMode::from_str(s))
        .unwrap_or(FrictionMode::Base);
    let copy_model = req
        .copy_model
        .as_ref()
        .map(|s| CopyCurveModel::from_str(s))
        .unwrap_or(CopyCurveModel::Scaled);

    let mut analytics_params = WalletAnalyticsParams::default();
    analytics_params.friction_mode = friction_mode;
    analytics_params.copy_model = copy_model;

    let mut scheduled = 0usize;
    let mut skipped = 0usize;

    let mut guard = wallet_analytics_refresh_guard().lock().await;
    for wallet in req
        .wallets
        .iter()
        .map(|w| w.trim())
        .filter(|w| !w.is_empty())
    {
        let cache_key = wallet_analytics_cache_key(wallet, friction_mode, copy_model);
        let last_started = guard.get(&cache_key).copied().unwrap_or(0);
        if now - last_started < 30 {
            skipped += 1;
            continue;
        }
        guard.insert(cache_key, now);
        scheduled += 1;

        let storage = state.signal_storage.clone();
        let rest = rest.clone();
        let wallet = wallet.to_string();
        let analytics_params = analytics_params.clone();

        tokio::spawn(async move {
            let now = Utc::now().timestamp();
            let _ = get_or_compute_wallet_analytics(
                &storage,
                &rest,
                &wallet,
                force,
                now,
                analytics_params,
            )
            .await;
        });
    }
    drop(guard);

    Ok(Json(WalletAnalyticsPrimeResponse { scheduled, skipped }))
}

/// Get current Polymarket orderbook snapshot + derived depth metrics.
pub async fn get_market_snapshot(
    Query(params): Query<MarketSnapshotQuery>,
    AxumState(state): AxumState<AppState>,
) -> Result<Json<MarketSnapshotResponse>, StatusCode> {
    let levels = params.levels.unwrap_or(10).clamp(1, 50);

    let (cache_key, clob_token_id) = if let (Some(slug), Some(outcome)) =
        (params.market_slug.as_ref(), params.outcome.as_ref())
    {
        let clob = polymarket_gamma::resolve_clob_token_id_by_slug(
            state.signal_storage.as_ref(),
            &state.http_client,
            slug,
            outcome,
        )
        .await
        .map_err(|e| {
            warn!(
                "gamma lookup failed for slug={} outcome={}: {}",
                slug, outcome, e
            );
            StatusCode::BAD_GATEWAY
        })?
        .unwrap_or_default();
        if clob.is_empty() {
            return Err(StatusCode::NOT_FOUND);
        }
        (
            format!(
                "orderbook_now:slug:{}:outcome:{}:levels:{}",
                slug, outcome, levels
            ),
            clob,
        )
    } else if let Some(token_id) = params.token_id.as_ref() {
        // Dome's "token_id" is often the condition id (0x...). The CLOB /book endpoint wants
        // the outcome-level clobTokenId (large integer string). Prefer `market_slug` + `outcome`.
        if token_id.starts_with("0x") {
            return Err(StatusCode::BAD_REQUEST);
        }
        // If a client already has the CLOB token id, we can use it directly.
        // (These are large integer strings, not 0x... condition ids.)
        (
            format!("orderbook_now:token:{}:levels:{}", token_id, levels),
            token_id.clone(),
        )
    } else {
        return Err(StatusCode::BAD_REQUEST);
    };

    let now = Utc::now().timestamp();
    let ttl_seconds = 2;

    // Prefer ultra-fast WS cache (sub-ms read) when available.
    state.polymarket_market_ws.request_subscribe(&clob_token_id);
    if let Some(book) = state
        .polymarket_market_ws
        .get_orderbook(&clob_token_id, 1500)
    {
        let response = snapshot_from_sorted_orderbook(book.as_ref(), &clob_token_id, levels, now);
        return Ok(Json(response));
    }

    if let Ok(Some((cache_json, fetched_at))) = state.signal_storage.get_cache(&cache_key) {
        if now - fetched_at <= ttl_seconds {
            if let Ok(resp) = serde_json::from_str::<MarketSnapshotResponse>(&cache_json) {
                return Ok(Json(resp));
            }
        }
    }

    let orderbook = fetch_polymarket_orderbook(&state, &clob_token_id)
        .await
        .map_err(|e| {
            warn!("clob /book fetch failed token_id={}: {}", clob_token_id, e);
            StatusCode::BAD_GATEWAY
        })?;

    let mut orderbook = orderbook;
    sort_orderbook(&mut orderbook);
    let response = snapshot_from_sorted_orderbook(&orderbook, &clob_token_id, levels, now);
    if let Ok(json) = serde_json::to_string(&response) {
        let _ = state.signal_storage.upsert_cache(&cache_key, &json, now);
    }

    Ok(Json(response))
}

#[derive(Debug, Deserialize)]
pub struct TradeOrderRequest {
    pub signal_id: Option<String>,
    pub market_slug: Option<String>,
    pub outcome: Option<String>,
    pub side: String,
    pub notional_usd: f64,
    pub order_type: String,
    pub price_mode: String,
    pub limit_price: Option<f64>,
}

#[derive(Debug, Serialize)]
pub struct TradeOrderResponse {
    pub ok: bool,
    pub trading_enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
}

/// Trade endpoint (manual, one-click). For now this is a feature-flagged stub.
///
/// Env:
/// - ENABLE_TRADING=true|false
/// - TRADING_MODE=paper|live
pub async fn post_trade_order(
    AxumState(_state): AxumState<AppState>,
    AxumJson(req): AxumJson<TradeOrderRequest>,
) -> (StatusCode, Json<TradeOrderResponse>) {
    let enabled = env::var("ENABLE_TRADING")
        .map(|v| matches!(v.as_str(), "1" | "true" | "TRUE" | "on" | "ON"))
        .unwrap_or(false);

    let mode_raw = env::var("TRADING_MODE").unwrap_or_else(|_| "paper".to_string());
    let mode = if mode_raw.eq_ignore_ascii_case("live") {
        "live"
    } else {
        "paper"
    };

    let request_id = Uuid::new_v4().to_string();

    if !enabled {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(TradeOrderResponse {
                ok: false,
                trading_enabled: false,
                mode: Some(mode.to_string()),
                message: "Trading disabled (ENABLE_TRADING=false)".to_string(),
                request_id: Some(request_id),
            }),
        );
    }

    if !(req.side.eq_ignore_ascii_case("buy") || req.side.eq_ignore_ascii_case("sell")) {
        return (
            StatusCode::BAD_REQUEST,
            Json(TradeOrderResponse {
                ok: false,
                trading_enabled: true,
                mode: Some(mode.to_string()),
                message: "Invalid side (expected BUY/SELL)".to_string(),
                request_id: Some(request_id),
            }),
        );
    }

    if !(req.notional_usd.is_finite() && req.notional_usd > 0.0) {
        return (
            StatusCode::BAD_REQUEST,
            Json(TradeOrderResponse {
                ok: false,
                trading_enabled: true,
                mode: Some(mode.to_string()),
                message: "Invalid notional_usd".to_string(),
                request_id: Some(request_id),
            }),
        );
    }

    if mode == "live" {
        return (
            StatusCode::NOT_IMPLEMENTED,
            Json(TradeOrderResponse {
                ok: false,
                trading_enabled: true,
                mode: Some(mode.to_string()),
                message: "Live trading not wired yet (expected: Privy signing + Polymarket execution provider)"
                    .to_string(),
                request_id: Some(request_id),
            }),
        );
    }

    let market = req.market_slug.clone().unwrap_or_else(|| "".to_string());
    let outcome = req.outcome.clone().unwrap_or_else(|| "".to_string());
    let msg = format!(
        "PAPER: accepted {} ${:.2} market={} outcome={} order_type={} price_mode={} limit_price={}",
        req.side.to_uppercase(),
        req.notional_usd,
        market,
        outcome,
        req.order_type,
        req.price_mode,
        req.limit_price
            .map(|p| format!("{:.4}", p))
            .unwrap_or_else(|| "-".to_string())
    );

    (
        StatusCode::OK,
        Json(TradeOrderResponse {
            ok: true,
            trading_enabled: true,
            mode: Some(mode.to_string()),
            message: msg,
            request_id: Some(request_id),
        }),
    )
}

async fn fetch_polymarket_orderbook(
    state: &AppState,
    token_id: &str,
) -> Result<OrderBook, reqwest::Error> {
    state
        .http_client
        .get("https://clob.polymarket.com/book")
        .timeout(Duration::from_secs(1))
        .query(&[("token_id", token_id)])
        .send()
        .await?
        .error_for_status()?
        .json::<OrderBook>()
        .await
}

async fn get_orderbook_snapshot(
    state: &AppState,
    token_id: &str,
    levels: usize,
    max_age_ms: i64,
    allow_rest: bool,
) -> Option<MarketSnapshotResponse> {
    let fetched_at = Utc::now().timestamp();

    state.polymarket_market_ws.request_subscribe(token_id);
    if let Some(book) = state
        .polymarket_market_ws
        .get_orderbook(token_id, max_age_ms)
    {
        let mut ob = (*book).clone();
        sort_orderbook(&mut ob);
        return Some(snapshot_from_sorted_orderbook(
            &ob, token_id, levels, fetched_at,
        ));
    }

    if !allow_rest {
        return None;
    }

    let mut orderbook = fetch_polymarket_orderbook(state, token_id).await.ok()?;
    sort_orderbook(&mut orderbook);
    Some(snapshot_from_sorted_orderbook(
        &orderbook, token_id, levels, fetched_at,
    ))
}

fn sort_orderbook(orderbook: &mut OrderBook) {
    orderbook.bids.sort_by(|a, b| {
        b.price
            .partial_cmp(&a.price)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    orderbook.asks.sort_by(|a, b| {
        a.price
            .partial_cmp(&b.price)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
}

fn snapshot_from_sorted_orderbook(
    orderbook: &OrderBook,
    token_id: &str,
    levels: usize,
    fetched_at: i64,
) -> MarketSnapshotResponse {
    let best_bid = orderbook.bids.first().map(|o| o.price);
    let best_ask = orderbook.asks.first().map(|o| o.price);
    let mid = match (best_bid, best_ask) {
        (Some(b), Some(a)) => Some((a + b) / 2.0),
        _ => None,
    };
    let spread = match (best_bid, best_ask) {
        (Some(b), Some(a)) => Some(a - b),
        _ => None,
    };

    let (depth, imbalance_10bps) = mid
        .map(|m| {
            let d10 = depth_notional(&orderbook, m, 10.0);
            let d25 = depth_notional(&orderbook, m, 25.0);
            let d50 = depth_notional(&orderbook, m, 50.0);
            let imbalance = compute_imbalance(&orderbook, m, 10.0);

            (
                Some(MarketSnapshotDepth {
                    bps_10: d10,
                    bps_25: d25,
                    bps_50: d50,
                }),
                imbalance,
            )
        })
        .unwrap_or((None, None));

    MarketSnapshotResponse {
        token_id: token_id.to_string(),
        fetched_at,
        best_bid,
        best_ask,
        mid,
        spread,
        depth,
        imbalance_10bps,
        bids: orderbook
            .bids
            .iter()
            .take(levels)
            .map(|o| MarketSnapshotLevel {
                price: o.price,
                size: o.size,
            })
            .collect(),
        asks: orderbook
            .asks
            .iter()
            .take(levels)
            .map(|o| MarketSnapshotLevel {
                price: o.price,
                size: o.size,
            })
            .collect(),
    }
}

fn depth_notional(orderbook: &OrderBook, mid: f64, bps: f64) -> f64 {
    let pct = bps / 10_000.0;
    let bid_cutoff = mid * (1.0 - pct);
    let ask_cutoff = mid * (1.0 + pct);

    let bid_notional = orderbook
        .bids
        .iter()
        .filter(|o| o.price >= bid_cutoff)
        .map(|o| o.price * o.size)
        .sum::<f64>();

    let ask_notional = orderbook
        .asks
        .iter()
        .filter(|o| o.price <= ask_cutoff)
        .map(|o| o.price * o.size)
        .sum::<f64>();

    bid_notional + ask_notional
}

fn compute_imbalance(orderbook: &OrderBook, mid: f64, bps: f64) -> Option<f64> {
    let pct = bps / 10_000.0;
    let bid_cutoff = mid * (1.0 - pct);
    let ask_cutoff = mid * (1.0 + pct);

    let bid_notional = orderbook
        .bids
        .iter()
        .filter(|o| o.price >= bid_cutoff)
        .map(|o| o.price * o.size)
        .sum::<f64>();

    let ask_notional = orderbook
        .asks
        .iter()
        .filter(|o| o.price <= ask_cutoff)
        .map(|o| o.price * o.size)
        .sum::<f64>();

    let denom = bid_notional + ask_notional;
    if denom <= 0.0 {
        return None;
    }

    Some((bid_notional - ask_notional) / denom)
}

/// Check if a market slug indicates an up/down market
fn is_updown_market(slug: &str) -> bool {
    let lower = slug.to_ascii_lowercase();
    UPDOWN_PATTERNS.iter().any(|p| lower.contains(p))
}

fn looks_like_advanced_fts_query(q: &str) -> bool {
    let s = q.trim();
    if s.is_empty() {
        return false;
    }

    s.contains('"')
        || s.contains(':')
        || s.contains('*')
        || s.contains('(')
        || s.contains(')')
        || s.contains(" OR ")
        || s.contains(" AND ")
        || s.contains(" NOT ")
        || s.contains("NEAR")
}

fn build_default_fts_query(raw: &str) -> String {
    let s = raw.trim();
    if s.is_empty() {
        return String::new();
    }

    if looks_like_advanced_fts_query(s) {
        return s.to_string();
    }

    // Default: AND all whitespace-separated terms, with prefix matching for 3+ char terms.
    // Also support leading '-' for negation.
    let mut parts: Vec<String> = Vec::new();
    for t in s.split_whitespace() {
        let t = t.trim();
        if t.is_empty() {
            continue;
        }

        let (neg, term) = if let Some(rest) = t.strip_prefix('-') {
            (true, rest)
        } else {
            (false, t)
        };

        let term = term.trim_matches('"');
        if term.is_empty() {
            continue;
        }

        let mut token = term.to_string();
        if token.len() >= 3 && !token.ends_with('*') {
            token.push('*');
        }

        if neg {
            parts.push(format!("NOT {}", token));
        } else {
            parts.push(token);
        }
    }

    parts.join(" AND ")
}

fn build_safe_fallback_fts_query(raw: &str) -> String {
    let s = raw.trim();
    if s.is_empty() {
        return String::new();
    }

    // Aggressive sanitize: drop characters that commonly break MATCH parsing.
    let cleaned: String = s
        .chars()
        .map(|c| match c {
            '\\' | '(' | ')' | '[' | ']' | '{' | '}' => ' ',
            _ => c,
        })
        .collect();

    build_default_fts_query(&cleaned)
}

/// Get signals - simplified version that actually works
pub async fn get_signals_simple(
    Query(params): Query<SignalQuery>,
    AxumState(state): AxumState<AppState>,
) -> Json<SignalResponse> {
    let requested_limit = params.limit.unwrap_or(100);
    let exclude_updown = params.exclude_updown.unwrap_or(false);
    let full_context = params.full_context.unwrap_or(false);

    // When filtering, fetch more signals to ensure we return enough after filtering
    let fetch_limit = if exclude_updown {
        requested_limit * 10 // Up/down markets dominate, so fetch 10x
    } else {
        requested_limit
    };

    // Get signals from storage
    let mut signals = match params.before.as_deref() {
        Some(before) => state
            .signal_storage
            .get_before(before, params.before_id.as_deref(), fetch_limit)
            .unwrap_or_default(),
        None => state
            .signal_storage
            .get_recent(fetch_limit)
            .unwrap_or_default(),
    };

    // Apply server-side up/down filter
    if exclude_updown {
        signals.retain(|s| !is_updown_market(&s.market_slug));
        signals.truncate(requested_limit);
    }

    // Fetch contexts for this exact set of signals to avoid ordering mismatches.
    let signal_ids: Vec<String> = signals.iter().map(|s| s.id.clone()).collect();
    let contexts = state
        .signal_storage
        .get_contexts_for_signals(&signal_ids)
        .unwrap_or_default();

    let signals_with_context: Vec<SignalWithContext> = signals
        .into_iter()
        .map(|signal| {
            let signal = normalize_signal_market_title(signal);
            let is_dome_order = signal.id.starts_with("dome_order_");
            let ctx = contexts.get(&signal.id);
            SignalWithContext {
                signal,
                context: ctx.map(|c| {
                    if full_context {
                        c.context.clone()
                    } else {
                        c.context.lite()
                    }
                }),
                context_status: ctx.map(|c| c.status.clone()).or_else(|| {
                    if is_dome_order {
                        Some("pending".to_string())
                    } else {
                        None
                    }
                }),
                context_version: ctx.map(|c| c.context_version),
                context_enriched_at: ctx.map(|c| c.enriched_at),
            }
        })
        .collect();

    Json(SignalResponse {
        count: signals_with_context.len(),
        signals: signals_with_context,
        timestamp: Utc::now().to_rfc3339(),
    })
}

/// Search signals - robust full-history FTS (SQLite FTS5).
pub async fn get_signals_search(
    Query(params): Query<SignalSearchQuery>,
    AxumState(state): AxumState<AppState>,
) -> Result<Json<SignalResponse>, (StatusCode, String)> {
    let requested_limit = params.limit.unwrap_or(100).clamp(1, 500);
    let exclude_updown = params.exclude_updown.unwrap_or(false);
    let full_context = params.full_context.unwrap_or(false);
    let min_confidence = params
        .min_confidence
        .and_then(|v| if v.is_finite() { Some(v.clamp(0.0, 1.0)) } else { None });

    let raw = params.q.trim();
    if raw.is_empty() {
        return Ok(Json(SignalResponse {
            count: 0,
            signals: Vec::new(),
            timestamp: Utc::now().to_rfc3339(),
        }));
    }

    // Best-effort: ensure we have at least a small warm index window on first page.
    if params.before.is_none() {
        if let Err(e) = state.signal_storage.ensure_search_warm(500) {
            warn!("search warm-up failed: {}", e);
        }
    }

    let fts_query = build_default_fts_query(raw);

    if fts_query.trim().is_empty() {
        return Ok(Json(SignalResponse {
            count: 0,
            signals: Vec::new(),
            timestamp: Utc::now().to_rfc3339(),
        }));
    }

    let mut signals = match state.signal_storage.search_signals_fts(
        &fts_query,
        params.before.as_deref(),
        params.before_id.as_deref(),
        requested_limit,
        exclude_updown,
        min_confidence,
    ) {
        Ok(s) => s,
        Err(e) => {
            warn!("FTS search failed (retrying safe fallback): {}", e);
            let fallback = build_safe_fallback_fts_query(raw);
            match state.signal_storage.search_signals_fts(
                &fallback,
                params.before.as_deref(),
                params.before_id.as_deref(),
                requested_limit,
                exclude_updown,
                min_confidence,
            ) {
                Ok(s) => s,
                Err(e2) => {
                    let msg = e2.to_string();
                    if msg.contains("no such table: signal_search_fts")
                        || msg.contains("no such table: signal_search")
                    {
                        return Err((
                            StatusCode::SERVICE_UNAVAILABLE,
                            "Search index not initialized yet. Restart the backend to apply schema.".to_string(),
                        ));
                    }
                    if msg.contains("malformed")
                        || msg.contains("fts5")
                        || msg.contains("MATCH")
                        || msg.contains("syntax")
                    {
                        return Err((
                            StatusCode::BAD_REQUEST,
                            "Invalid search query (FTS syntax error)".to_string(),
                        ));
                    }

                    Err((StatusCode::INTERNAL_SERVER_ERROR, format!("Search failed: {msg}")))?
                }
            }
        }
    };

    // Secondary guard: `exclude_updown` is enforced in SQL, but keep a defensive filter.
    if exclude_updown {
        signals.retain(|s| !is_updown_market(&s.market_slug));
        signals.truncate(requested_limit);
    }

    let signal_ids: Vec<String> = signals.iter().map(|s| s.id.clone()).collect();
    let contexts = state
        .signal_storage
        .get_contexts_for_signals(&signal_ids)
        .unwrap_or_default();

    let signals_with_context: Vec<SignalWithContext> = signals
        .into_iter()
        .map(|signal| {
            let signal = normalize_signal_market_title(signal);
            let is_dome_order = signal.id.starts_with("dome_order_");
            let ctx = contexts.get(&signal.id);
            SignalWithContext {
                signal,
                context: ctx.map(|c| if full_context { c.context.clone() } else { c.context.lite() }),
                context_status: ctx.map(|c| c.status.clone()).or_else(|| {
                    if is_dome_order {
                        Some("pending".to_string())
                    } else {
                        None
                    }
                }),
                context_version: ctx.map(|c| c.context_version),
                context_enriched_at: ctx.map(|c| c.enriched_at),
            }
        })
        .collect();

    Ok(Json(SignalResponse {
        count: signals_with_context.len(),
        signals: signals_with_context,
        timestamp: Utc::now().to_rfc3339(),
    }))
}

#[derive(Debug, Serialize)]
pub struct SignalSearchStatusResponse {
    pub schema_ready: bool,
    pub backfill_done: bool,
    pub total_signals: usize,
    pub indexed_rows: usize,
    pub cursor_detected_at: Option<String>,
    pub cursor_id: Option<String>,
    pub timestamp: String,
}

pub async fn get_signals_search_status(
    AxumState(state): AxumState<AppState>,
) -> Json<SignalSearchStatusResponse> {
    let status = state.signal_storage.get_search_index_status();
    Json(SignalSearchStatusResponse {
        schema_ready: status.schema_ready,
        backfill_done: status.backfill_done,
        total_signals: status.total_signals,
        indexed_rows: status.indexed_rows,
        cursor_detected_at: status.cursor_detected_at,
        cursor_id: status.cursor_id,
        timestamp: Utc::now().to_rfc3339(),
    })
}

/// Get signal stats - total count, avg confidence, etc.
pub async fn get_signal_stats(AxumState(state): AxumState<AppState>) -> Json<SignalStatsResponse> {
    // Get total signals ever (cumulative counter)
    let total_ever = state.signal_storage.get_total_signals_ever().unwrap_or(0) as usize;

    // Get recent signals for confidence stats
    let signals = state.signal_storage.get_recent(1000).unwrap_or_default();

    let high_conf = signals.iter().filter(|s| s.confidence >= 0.7).count();
    let avg_conf = if !signals.is_empty() {
        signals.iter().map(|s| s.confidence).sum::<f64>() / signals.len() as f64
    } else {
        0.0
    };

    Json(SignalStatsResponse {
        total_signals: total_ever,
        high_confidence_count: high_conf,
        avg_confidence: avg_conf,
        timestamp: Utc::now().to_rfc3339(),
    })
}

#[derive(Debug, Serialize)]
pub struct RiskStatsResponse {
    pub var_95: f64,
    pub cvar_95: f64,
    pub current_bankroll: f64,
    pub kelly_fraction: f64,
    pub win_rate: f64,
    pub sample_size: usize,
}

/// Get risk stats - simplified version
pub async fn get_risk_stats_simple(
    AxumState(state): AxumState<AppState>,
) -> Json<RiskStatsResponse> {
    let risk_manager = state.risk_manager.read(); // parking_lot - no await needed

    let var_stats = risk_manager.var.get_stats();
    let win_rate = risk_manager.kelly.get_win_rate();

    Json(RiskStatsResponse {
        var_95: var_stats.var_95,
        cvar_95: var_stats.cvar_95,
        current_bankroll: risk_manager.kelly.bankroll,
        kelly_fraction: risk_manager.kelly.fraction,
        win_rate,
        sample_size: var_stats.sample_size,
    })
}

/// Get pooled vault state (cash + approximate NAV + shares).
pub async fn get_vault_state(AxumState(state): AxumState<AppState>) -> Json<VaultStateResponse> {
    Json(state.vault.state().await)
}

/// Mint vault shares against a USDC deposit (accounting-only; on-chain settlement TBD).
pub async fn post_vault_deposit(
    AxumState(state): AxumState<AppState>,
    AxumJson(req): AxumJson<VaultDepositRequest>,
) -> Result<Json<crate::vault::VaultDepositResponse>, StatusCode> {
    state
        .vault
        .deposit(&req.wallet_address, req.amount_usdc)
        .await
        .map(Json)
        .map_err(|_| StatusCode::BAD_REQUEST)
}

/// Burn vault shares for a USDC withdrawal (accounting-only; on-chain settlement TBD).
pub async fn post_vault_withdraw(
    AxumState(state): AxumState<AppState>,
    AxumJson(req): AxumJson<VaultWithdrawRequest>,
) -> Result<Json<crate::vault::VaultWithdrawResponse>, StatusCode> {
    match state.vault.withdraw(&req.wallet_address, req.shares).await {
        Ok(resp) => Ok(Json(resp)),
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("insufficient") {
                Err(StatusCode::CONFLICT)
            } else {
                Err(StatusCode::BAD_REQUEST)
            }
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct VaultOverviewQuery {
    pub wallet: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct VaultOverviewResponse {
    pub fetched_at: i64,
    pub engine_enabled: bool,
    pub paper: bool,
    pub cash_usdc: f64,
    pub nav_usdc: f64,
    pub total_shares: f64,
    pub nav_per_share: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wallet_address: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_shares: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_value_usdc: Option<f64>,
}

pub async fn get_vault_overview(
    Query(params): Query<VaultOverviewQuery>,
    AxumState(state): AxumState<AppState>,
) -> Result<Json<VaultOverviewResponse>, StatusCode> {
    let now = Utc::now().timestamp();
    let cfg = VaultEngineConfig::from_env();
    let s = state.vault.state().await;

    let (wallet_address, user_shares, user_value_usdc) = if let Some(w) = params.wallet.as_deref() {
        let w = w.trim().to_lowercase();
        if w.is_empty() {
            (None, None, None)
        } else {
            let shares = state.vault.shares.lock().await.shares_of(&w);
            let value = shares * s.nav_per_share;
            (Some(w), Some(shares), Some(value))
        }
    } else {
        (None, None, None)
    };

    Ok(Json(VaultOverviewResponse {
        fetched_at: now,
        engine_enabled: cfg.enabled,
        paper: cfg.paper,
        cash_usdc: s.cash_usdc,
        nav_usdc: s.nav_usdc,
        total_shares: s.total_shares,
        nav_per_share: s.nav_per_share,
        wallet_address,
        user_shares,
        user_value_usdc,
    }))
}

#[derive(Debug, Deserialize)]
pub struct VaultPerformanceQuery {
    pub range: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct VaultNavPoint {
    pub ts: i64,
    pub nav_per_share: f64,
    pub nav_usdc: f64,
    pub cash_usdc: f64,
    pub positions_value_usdc: f64,
    pub total_shares: f64,
    pub source: String,
}

#[derive(Debug, Serialize)]
pub struct VaultPerformanceResponse {
    pub fetched_at: i64,
    pub range: String,
    pub points: Vec<VaultNavPoint>,
}

fn parse_range_start(now: i64, range: &str) -> i64 {
    let r = range.trim().to_lowercase();
    match r.as_str() {
        "24h" | "1d" => now - 24 * 3600,
        "7d" => now - 7 * 24 * 3600,
        "30d" => now - 30 * 24 * 3600,
        "90d" => now - 90 * 24 * 3600,
        "all" => 0,
        _ => now - 7 * 24 * 3600,
    }
}

pub async fn get_vault_performance(
    Query(params): Query<VaultPerformanceQuery>,
    AxumState(state): AxumState<AppState>,
) -> Result<Json<VaultPerformanceResponse>, StatusCode> {
    let now = Utc::now().timestamp();
    let range = params.range.unwrap_or_else(|| "7d".to_string());
    let start_ts = parse_range_start(now, &range);

    let snaps = state
        .vault
        .db
        .list_nav_snapshots(start_ts, None, 20_000)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let mut points: Vec<VaultNavPoint> = snaps
        .into_iter()
        .map(|s| VaultNavPoint {
            ts: s.ts,
            nav_per_share: s.nav_per_share,
            nav_usdc: s.nav_usdc,
            cash_usdc: s.cash_usdc,
            positions_value_usdc: s.positions_value_usdc,
            total_shares: s.total_shares,
            source: s.source,
        })
        .collect();

    if points.is_empty() {
        let s = state.vault.state().await;
        points.push(VaultNavPoint {
            ts: now,
            nav_per_share: s.nav_per_share,
            nav_usdc: s.nav_usdc,
            cash_usdc: s.cash_usdc,
            positions_value_usdc: (s.nav_usdc - s.cash_usdc).max(0.0),
            total_shares: s.total_shares,
            source: "live".to_string(),
        });
    }

    // Downsample to keep payloads small.
    let max_points = 1200usize;
    if points.len() > max_points {
        let stride = (points.len() + max_points - 1) / max_points;
        points = points
            .into_iter()
            .enumerate()
            .filter_map(|(i, p)| if i % stride == 0 { Some(p) } else { None })
            .collect();
    }

    Ok(Json(VaultPerformanceResponse {
        fetched_at: now,
        range,
        points,
    }))
}

#[derive(Debug, Clone, Serialize)]
pub struct VaultPositionResponse {
    pub token_id: String,
    pub outcome: String,
    pub shares: f64,
    pub avg_price: f64,
    pub cost_usdc: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub market_slug: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub market_question: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_date_iso: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tte_sec: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strategy: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decision_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub best_bid: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub best_ask: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mid: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spread: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value_usdc: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pnl_unrealized_usdc: Option<f64>,
}

#[derive(Debug, Serialize)]
pub struct VaultPositionsResponse {
    pub fetched_at: i64,
    pub positions: Vec<VaultPositionResponse>,
}

fn parse_rfc3339_ts(s: &str) -> Option<i64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|dt| dt.timestamp())
}

pub async fn get_vault_positions(
    AxumState(state): AxumState<AppState>,
) -> Result<Json<VaultPositionsResponse>, StatusCode> {
    let now = Utc::now().timestamp();

    let positions = {
        let ledger = state.vault.ledger.lock().await;
        ledger.positions.values().cloned().collect::<Vec<_>>()
    };

    let mut out: Vec<VaultPositionResponse> = Vec::with_capacity(positions.len());
    for p in positions {
        if p.shares <= 0.0 {
            continue;
        }

        let meta = state
            .vault
            .db
            .get_token_meta(&p.token_id)
            .await
            .ok()
            .flatten();

        let (market_slug, strategy, decision_id) = match meta {
            Some(m) => (Some(m.market_slug), m.strategy, m.decision_id),
            None => (None, None, None),
        };

        let mut market_question: Option<String> = None;
        let mut end_date_iso: Option<String> = None;
        let mut tte_sec: Option<i64> = None;
        if let Some(slug) = market_slug.as_deref() {
            if let Ok(Some(g)) = polymarket_gamma::gamma_market_lookup(
                state.signal_storage.as_ref(),
                &state.http_client,
                slug,
            )
            .await
            {
                market_question = g.question.clone();
                end_date_iso = g.end_date_iso.clone();
                if let Some(ts) = g.end_date_iso.as_deref().and_then(parse_rfc3339_ts) {
                    tte_sec = Some(ts - now);
                }
            }
        }

        let snap = get_orderbook_snapshot(&state, &p.token_id, 1, 1500, true).await;
        let bid = snap.as_ref().and_then(|s| s.best_bid);
        let ask = snap.as_ref().and_then(|s| s.best_ask);
        let mid = snap.as_ref().and_then(|s| s.mid);
        let spread = snap.as_ref().and_then(|s| s.spread);
        let value = mid.map(|m| p.shares * m);
        let pnl = value.map(|v| v - p.cost_usdc);

        out.push(VaultPositionResponse {
            token_id: p.token_id,
            outcome: p.outcome,
            shares: p.shares,
            avg_price: p.avg_price,
            cost_usdc: p.cost_usdc,
            market_slug,
            market_question,
            end_date_iso,
            tte_sec,
            strategy,
            decision_id,
            best_bid: bid,
            best_ask: ask,
            mid,
            spread,
            value_usdc: value,
            pnl_unrealized_usdc: pnl,
        });
    }

    out.sort_by(|a, b| {
        let av = a.value_usdc.unwrap_or(0.0);
        let bv = b.value_usdc.unwrap_or(0.0);
        bv.partial_cmp(&av).unwrap_or(std::cmp::Ordering::Equal)
    });

    Ok(Json(VaultPositionsResponse {
        fetched_at: now,
        positions: out,
    }))
}

#[derive(Debug, Deserialize)]
pub struct VaultActivityQuery {
    pub limit: Option<usize>,
    pub wallet: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct VaultActivityResponse {
    pub fetched_at: i64,
    pub events: Vec<VaultActivityRecord>,
}

pub async fn get_vault_activity(
    Query(params): Query<VaultActivityQuery>,
    AxumState(state): AxumState<AppState>,
) -> Result<Json<VaultActivityResponse>, StatusCode> {
    let now = Utc::now().timestamp();
    let limit = params.limit.unwrap_or(200).clamp(1, 1000);
    let wallet = params
        .wallet
        .as_deref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty());

    let events = state
        .vault
        .db
        .list_activity(limit, wallet)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(VaultActivityResponse {
        fetched_at: now,
        events,
    }))
}

#[derive(Debug, Serialize)]
pub struct VaultConfigResponse {
    pub fetched_at: i64,
    pub engine_enabled: bool,
    pub paper: bool,
    pub updown_poll_ms: u64,
    pub updown_min_edge: f64,
    pub updown_kelly_fraction: f64,
    pub updown_max_position_pct: f64,
    pub updown_shrink_to_half: f64,
    pub updown_cooldown_sec: i64,

    pub long_enabled: bool,
    pub long_poll_ms: u64,
    pub long_min_edge: f64,
    pub long_kelly_fraction: f64,
    pub long_max_position_pct: f64,
    pub long_min_trade_usd: f64,
    pub long_max_trade_usd: f64,
    pub long_min_infer_interval_sec: i64,
    pub long_cooldown_sec: i64,
    pub long_max_calls_per_day: u32,
    pub long_max_calls_per_market_per_day: u32,
    pub long_max_tokens_per_day: u64,
    pub long_llm_timeout_sec: u64,
    pub long_llm_max_tokens: u32,
    pub long_llm_temperature: f64,
    pub long_max_tte_days: f64,
    pub long_max_spread_bps: f64,
    pub long_min_top_of_book_usd: f64,
    pub long_fee_buffer: f64,
    pub long_slippage_buffer_min: f64,
    pub long_dispersion_max: f64,
    pub long_exit_price_90: f64,
    pub long_exit_price_95: f64,
    pub long_exit_frac_90: f64,
    pub long_exit_frac_95: f64,
    pub long_wallet_window_sec: i64,
    pub long_wallet_max_trades_per_window: usize,
    pub long_wallet_min_notional_usd: f64,
    pub long_models: Vec<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub llm_usage_today: Option<crate::signals::db_storage::VaultLlmUsageStats>,
}

pub async fn get_vault_config(
    AxumState(state): AxumState<AppState>,
) -> Result<Json<VaultConfigResponse>, StatusCode> {
    let now = Utc::now().timestamp();
    let cfg = VaultEngineConfig::from_env();

    let llm_usage_today = state.signal_storage.get_vault_llm_usage_today(now).ok();

    Ok(Json(VaultConfigResponse {
        fetched_at: now,
        engine_enabled: cfg.enabled,
        paper: cfg.paper,
        updown_poll_ms: cfg.updown_poll_ms,
        updown_min_edge: cfg.updown_min_edge,
        updown_kelly_fraction: cfg.updown_kelly_fraction,
        updown_max_position_pct: cfg.updown_max_position_pct,
        updown_shrink_to_half: cfg.updown_shrink_to_half,
        updown_cooldown_sec: cfg.updown_cooldown_sec,
        long_enabled: cfg.long_enabled,
        long_poll_ms: cfg.long_poll_ms,
        long_min_edge: cfg.long_min_edge,
        long_kelly_fraction: cfg.long_kelly_fraction,
        long_max_position_pct: cfg.long_max_position_pct,
        long_min_trade_usd: cfg.long_min_trade_usd,
        long_max_trade_usd: cfg.long_max_trade_usd,
        long_min_infer_interval_sec: cfg.long_min_infer_interval_sec,
        long_cooldown_sec: cfg.long_cooldown_sec,
        long_max_calls_per_day: cfg.long_max_calls_per_day,
        long_max_calls_per_market_per_day: cfg.long_max_calls_per_market_per_day,
        long_max_tokens_per_day: cfg.long_max_tokens_per_day,
        long_llm_timeout_sec: cfg.long_llm_timeout_sec,
        long_llm_max_tokens: cfg.long_llm_max_tokens,
        long_llm_temperature: cfg.long_llm_temperature,
        long_max_tte_days: cfg.long_max_tte_days,
        long_max_spread_bps: cfg.long_max_spread_bps,
        long_min_top_of_book_usd: cfg.long_min_top_of_book_usd,
        long_fee_buffer: cfg.long_fee_buffer,
        long_slippage_buffer_min: cfg.long_slippage_buffer_min,
        long_dispersion_max: cfg.long_dispersion_max,
        long_exit_price_90: cfg.long_exit_price_90,
        long_exit_price_95: cfg.long_exit_price_95,
        long_exit_frac_90: cfg.long_exit_frac_90,
        long_exit_frac_95: cfg.long_exit_frac_95,
        long_wallet_window_sec: cfg.long_wallet_window_sec,
        long_wallet_max_trades_per_window: cfg.long_wallet_max_trades_per_window,
        long_wallet_min_notional_usd: cfg.long_wallet_min_notional_usd,
        long_models: cfg.long_models,
        llm_usage_today,
    }))
}

#[derive(Debug, Deserialize)]
pub struct VaultLlmDecisionsQuery {
    pub market_slug: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Debug, Serialize)]
pub struct VaultLlmDecisionsResponse {
    pub fetched_at: i64,
    pub decisions: Vec<crate::signals::db_storage::VaultLlmDecisionRow>,
}

pub async fn get_vault_llm_decisions(
    Query(params): Query<VaultLlmDecisionsQuery>,
    AxumState(state): AxumState<AppState>,
) -> Result<Json<VaultLlmDecisionsResponse>, StatusCode> {
    let now = Utc::now().timestamp();
    let limit = params.limit.unwrap_or(200).clamp(1, 500);
    let market_slug = params
        .market_slug
        .as_deref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty());

    let decisions = state
        .signal_storage
        .get_vault_llm_decisions(limit, market_slug)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(VaultLlmDecisionsResponse {
        fetched_at: now,
        decisions,
    }))
}

#[derive(Debug, Deserialize)]
pub struct VaultLlmModelsQuery {
    pub decision_id: String,
    pub limit: Option<usize>,
}

#[derive(Debug, Serialize)]
pub struct VaultLlmModelsResponse {
    pub fetched_at: i64,
    pub records: Vec<crate::signals::db_storage::VaultLlmModelRecordRow>,
}

pub async fn get_vault_llm_models(
    Query(params): Query<VaultLlmModelsQuery>,
    AxumState(state): AxumState<AppState>,
) -> Result<Json<VaultLlmModelsResponse>, StatusCode> {
    let now = Utc::now().timestamp();
    let limit = params.limit.unwrap_or(20).clamp(1, 100);

    let records = state
        .signal_storage
        .get_vault_llm_model_records(&params.decision_id, limit)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(VaultLlmModelsResponse {
        fetched_at: now,
        records,
    }))
}

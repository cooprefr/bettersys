//! Simplified API routes that just work
//! No complexity, just results
//!
//! Optimizations:
//! - Minimal allocations in hot paths
//! - Direct database access without intermediate layers

use crate::{
    models::{MarketSignal, SignalContext, SignalContextRecord, SignalType},
    scrapers::dome_rest::{DomeRestClient, OrdersFilter},
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
        .timeout(Duration::from_secs(8))
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
    let start = std::time::Instant::now();
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

    // Record API latency
    state
        .latency_registry
        .record_span(crate::latency::LatencySpan::new(
            crate::latency::SpanType::ApiSignals,
            start.elapsed().as_micros() as u64,
        ));

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
    let min_confidence = params.min_confidence.and_then(|v| {
        if v.is_finite() {
            Some(v.clamp(0.0, 1.0))
        } else {
            None
        }
    });

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

                    Err((
                        StatusCode::INTERNAL_SERVER_ERROR,
                        format!("Search failed: {msg}"),
                    ))?
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
    /// Real Polymarket balance (only in live mode)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub polymarket_balance: Option<f64>,
    /// Real Polymarket positions value (only in live mode)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub polymarket_positions_value: Option<f64>,
    /// Real Polymarket total value (only in live mode)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub polymarket_total_value: Option<f64>,
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

    // In live mode, fetch actual Polymarket account info
    let (polymarket_balance, polymarket_positions_value, polymarket_total_value) = if !cfg.paper {
        match crate::vault::PolymarketClobAdapter::from_env() {
            Some(clob) => match clob.get_account_info().await {
                Ok(info) => (
                    Some(info.balance_usdc),
                    Some(info.positions_value_usdc),
                    Some(info.total_value_usdc),
                ),
                Err(e) => {
                    warn!(error = %e, "Failed to fetch Polymarket account info");
                    (None, None, None)
                }
            },
            None => (None, None, None),
        }
    } else {
        (None, None, None)
    };

    // Use Polymarket values if available, otherwise use internal ledger
    let (cash, nav) = if let Some(total) = polymarket_total_value {
        let bal = polymarket_balance.unwrap_or(0.0);
        (bal, total)
    } else {
        (s.cash_usdc, s.nav_usdc)
    };

    Ok(Json(VaultOverviewResponse {
        fetched_at: now,
        engine_enabled: cfg.enabled,
        paper: cfg.paper,
        cash_usdc: cash,
        nav_usdc: nav,
        total_shares: s.total_shares,
        nav_per_share: if s.total_shares > 0.0 {
            nav / s.total_shares
        } else {
            1.0
        },
        wallet_address,
        user_shares,
        user_value_usdc,
        polymarket_balance,
        polymarket_positions_value,
        polymarket_total_value,
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

// =============================================================================
// RN-JD Belief Volatility API
// =============================================================================

/// Response for belief volatility stats endpoint
#[derive(Debug, Serialize)]
pub struct BeliefVolStatsResponse {
    pub fetched_at: i64,
    pub markets_tracked: usize,
    pub reliable_estimates: usize,
    pub avg_sigma_b: f64,
    pub estimates: HashMap<String, BeliefVolEstimateDto>,
}

#[derive(Debug, Serialize)]
pub struct BeliefVolEstimateDto {
    pub sigma_b: f64,
    pub sample_count: usize,
    pub confidence: f64,
    pub last_updated: i64,
}

/// GET /api/belief-vol/stats - Get belief volatility tracker statistics
pub async fn get_belief_vol_stats(
    AxumState(state): AxumState<AppState>,
) -> Json<BeliefVolStatsResponse> {
    let now = Utc::now().timestamp();

    let tracker = state.belief_vol_tracker.read();
    let summary = tracker.summary();
    let raw_estimates = tracker.export_estimates();
    drop(tracker);

    let estimates: HashMap<String, BeliefVolEstimateDto> = raw_estimates
        .into_iter()
        .map(|(k, v)| {
            (
                k,
                BeliefVolEstimateDto {
                    sigma_b: v.sigma_b,
                    sample_count: v.sample_count,
                    confidence: v.confidence,
                    last_updated: v.last_updated,
                },
            )
        })
        .collect();

    Json(BeliefVolStatsResponse {
        fetched_at: now,
        markets_tracked: summary.markets_tracked,
        reliable_estimates: summary.reliable_estimates,
        avg_sigma_b: summary.avg_sigma_b,
        estimates,
    })
}

// =============================================================================
// System-Wide Latency API
// =============================================================================

/// GET /api/latency/stats - Get tick-to-trade latency statistics (legacy)
pub async fn get_latency_stats(
    AxumState(state): AxumState<AppState>,
) -> Json<crate::latency::SystemLatencySummary> {
    Json(state.latency_registry.summary())
}

#[derive(Debug, Deserialize)]
pub struct LatencySpansQuery {
    pub limit: Option<usize>,
    pub span_type: Option<String>,
}

/// GET /api/latency/spans - Get recent latency spans for debugging
pub async fn get_latency_spans(
    Query(params): Query<LatencySpansQuery>,
    AxumState(state): AxumState<AppState>,
) -> Json<LatencySpansResponse> {
    let now = Utc::now().timestamp();
    let limit = params.limit.unwrap_or(50).min(500);

    let spans = state.latency_registry.recent_spans(limit);

    Json(LatencySpansResponse {
        timestamp: now,
        count: spans.len(),
        spans,
    })
}

#[derive(Debug, Serialize)]
pub struct LatencySpansResponse {
    pub timestamp: i64,
    pub count: usize,
    pub spans: Vec<crate::latency::LatencySpan>,
}

#[derive(Debug, Deserialize)]
pub struct LatencyTimeSeriesQuery {
    pub minutes: Option<usize>,
}

/// GET /api/latency/timeseries - Get latency time series for dashboard
pub async fn get_latency_timeseries(
    Query(params): Query<LatencyTimeSeriesQuery>,
    AxumState(state): AxumState<AppState>,
) -> Json<LatencyTimeSeriesResponse> {
    let minutes = params.minutes.unwrap_or(60).min(120);
    let buckets = state.latency_registry.time_series(minutes);

    Json(LatencyTimeSeriesResponse {
        timestamp: Utc::now().timestamp(),
        buckets,
    })
}

#[derive(Debug, Serialize)]
pub struct LatencyTimeSeriesResponse {
    pub timestamp: i64,
    pub buckets: Vec<crate::latency::LatencyBucket>,
}

#[derive(Debug, Deserialize)]
pub struct LatencyCdfQuery {
    pub component: String,
}

/// GET /api/latency/cdf - Get CDF data for a specific component
pub async fn get_latency_cdf(
    Query(params): Query<LatencyCdfQuery>,
    AxumState(state): AxumState<AppState>,
) -> Json<LatencyCdfResponse> {
    let reg = &state.latency_registry;

    let points = match params.component.as_str() {
        "binance_ws" => reg.binance_ws_latency.cdf(),
        "dome_ws" => reg.dome_ws_latency.cdf(),
        "dome_rest" => reg.dome_rest_latency.cdf(),
        "signal_detection" => reg.signal_detection_latency.cdf(),
        "api_signals" => reg.api_signals_latency.cdf(),
        "fast15m_t2t" => reg.fast15m_t2t_latency.cdf(),
        "long_t2t" => reg.long_t2t_latency.cdf(),
        "db_read" => reg.db_read_latency.cdf(),
        "db_write" => reg.db_write_latency.cdf(),
        _ => vec![],
    };

    Json(LatencyCdfResponse {
        timestamp: Utc::now().timestamp(),
        component: params.component,
        points,
    })
}

#[derive(Debug, Serialize)]
pub struct LatencyCdfResponse {
    pub timestamp: i64,
    pub component: String,
    pub points: Vec<crate::latency::CdfPoint>,
}

/// GET /api/latency/dashboard - Combined dashboard data
pub async fn get_latency_dashboard(
    AxumState(state): AxumState<AppState>,
) -> Json<LatencyDashboardResponse> {
    let summary = state.latency_registry.summary();
    let timeseries = state.latency_registry.time_series(30); // Last 30 minutes
    let recent_spans = state.latency_registry.recent_spans(20);

    Json(LatencyDashboardResponse {
        timestamp: Utc::now().timestamp(),
        summary,
        timeseries,
        recent_spans,
    })
}

#[derive(Debug, Serialize)]
pub struct LatencyDashboardResponse {
    pub timestamp: i64,
    pub summary: crate::latency::SystemLatencySummary,
    pub timeseries: Vec<crate::latency::LatencyBucket>,
    pub recent_spans: Vec<crate::latency::LatencySpan>,
}

// =============================================================================
// Performance Profiling API
// =============================================================================

/// GET /api/performance/report - Full performance report
pub async fn get_performance_report(
    AxumState(state): AxumState<AppState>,
) -> Json<crate::performance::PerformanceReport> {
    Json(state.performance_profiler.report())
}

/// GET /api/performance/quick - Quick performance summary
pub async fn get_performance_quick(
    AxumState(state): AxumState<AppState>,
) -> Json<crate::performance::report::QuickReport> {
    let full = state.performance_profiler.report();
    Json(crate::performance::report::QuickReport::from_full(&full))
}

/// GET /api/performance/health - Health score
pub async fn get_performance_health(
    AxumState(state): AxumState<AppState>,
) -> Json<PerformanceHealthResponse> {
    let report = state.performance_profiler.report();
    let health = report.health_score();
    Json(PerformanceHealthResponse {
        timestamp: Utc::now().timestamp(),
        health,
        summary: report.executive_summary(),
    })
}

#[derive(Debug, Serialize)]
pub struct PerformanceHealthResponse {
    pub timestamp: i64,
    pub health: crate::performance::metrics::HealthScore,
    pub summary: String,
}

/// GET /api/performance/pipeline - Pipeline-specific metrics
pub async fn get_performance_pipeline(
    AxumState(state): AxumState<AppState>,
) -> Json<PerformancePipelineResponse> {
    let pipeline = state.performance_profiler.pipeline.snapshot();
    let latency = crate::performance::metrics::PipelineLatencyBreakdown::from_pipeline(&pipeline);
    let trading = crate::performance::metrics::TradingEngineSummary::from_pipeline(&pipeline);

    Json(PerformancePipelineResponse {
        timestamp: Utc::now().timestamp(),
        pipeline,
        latency_breakdown: latency,
        trading_engines: trading,
    })
}

#[derive(Debug, Serialize)]
pub struct PerformancePipelineResponse {
    pub timestamp: i64,
    pub pipeline: crate::performance::PipelineSnapshot,
    pub latency_breakdown: crate::performance::metrics::PipelineLatencyBreakdown,
    pub trading_engines: crate::performance::metrics::TradingEngineSummary,
}

/// GET /api/performance/memory - Memory profiling
pub async fn get_performance_memory(
    AxumState(state): AxumState<AppState>,
) -> Json<crate::performance::memory::MemorySnapshot> {
    Json(state.performance_profiler.memory.snapshot())
}

/// GET /api/performance/cpu - CPU profiling
pub async fn get_performance_cpu(
    AxumState(state): AxumState<AppState>,
) -> Json<crate::performance::cpu::CpuSnapshot> {
    Json(state.performance_profiler.cpu.snapshot())
}

/// GET /api/performance/io - IO profiling
pub async fn get_performance_io(
    AxumState(state): AxumState<AppState>,
) -> Json<crate::performance::io::IoSnapshot> {
    Json(state.performance_profiler.io.snapshot())
}

/// GET /api/performance/throughput - Throughput metrics
pub async fn get_performance_throughput(
    AxumState(state): AxumState<AppState>,
) -> Json<crate::performance::throughput::ThroughputSnapshot> {
    Json(state.performance_profiler.throughput.snapshot())
}

#[derive(Debug, Deserialize)]
pub struct BenchmarkQuery {
    pub name: Option<String>,
}

/// GET /api/performance/benchmark - Run built-in benchmarks
pub async fn get_performance_benchmark(
    Query(params): Query<BenchmarkQuery>,
) -> Json<BenchmarkResponse> {
    let results = match params.name.as_deref() {
        Some("histogram") => vec![crate::performance::benchmark::prebuilt::histogram_recording()],
        Some("json") => vec![crate::performance::benchmark::prebuilt::json_parsing()],
        Some("uuid") => vec![crate::performance::benchmark::prebuilt::uuid_generation()],
        Some("timestamp") => vec![crate::performance::benchmark::prebuilt::timestamp_generation()],
        _ => {
            // Run all benchmarks
            vec![
                crate::performance::benchmark::prebuilt::histogram_recording(),
                crate::performance::benchmark::prebuilt::json_parsing(),
                crate::performance::benchmark::prebuilt::uuid_generation(),
                crate::performance::benchmark::prebuilt::timestamp_generation(),
            ]
        }
    };

    Json(BenchmarkResponse {
        timestamp: Utc::now().timestamp(),
        results,
    })
}

#[derive(Debug, Serialize)]
pub struct BenchmarkResponse {
    pub timestamp: i64,
    pub results: Vec<crate::performance::benchmark::BenchmarkResult>,
}

/// GET /api/performance/dashboard - Comprehensive performance metrics
pub async fn get_performance_dashboard() -> Json<PerformanceDashboardResponse> {
    let profiler = crate::performance::global_profiler();
    let latency = crate::latency::global_registry();
    let comprehensive = crate::latency::global_comprehensive();

    Json(PerformanceDashboardResponse {
        timestamp: chrono::Utc::now().timestamp(),
        uptime_secs: profiler.uptime_secs(),
        latency: latency.summary(),
        pipeline: profiler.pipeline.snapshot(),
        memory: profiler.memory.snapshot(),
        cpu: profiler.cpu.snapshot(),
        io: profiler.io.snapshot(),
        throughput: profiler.throughput.snapshot(),
        comprehensive: comprehensive.snapshot(),
    })
}

#[derive(Debug, Serialize)]
pub struct PerformanceDashboardResponse {
    pub timestamp: i64,
    pub uptime_secs: f64,
    pub latency: crate::latency::SystemLatencySummary,
    pub pipeline: crate::performance::PipelineSnapshot,
    pub memory: crate::performance::memory::MemorySnapshot,
    pub cpu: crate::performance::cpu::CpuSnapshot,
    pub io: crate::performance::io::IoSnapshot,
    pub throughput: crate::performance::throughput::ThroughputSnapshot,
    pub comprehensive: crate::latency::ComprehensiveSnapshot,
}

/// POST /api/performance/load-test - Run synthetic load test
#[derive(Debug, Deserialize)]
pub struct LoadTestRequest {
    pub events: Option<u32>,
    pub interval_ms: Option<u64>,
}

pub async fn post_performance_load_test(
    AxumJson(req): AxumJson<LoadTestRequest>,
) -> Json<LoadTestResponse> {
    let events = req.events.unwrap_or(1000);
    let interval_ms = req.interval_ms.unwrap_or(1);

    // Run burst in background
    tokio::spawn(async move {
        crate::performance::load_generator::run_burst(events, interval_ms).await;
    });

    Json(LoadTestResponse {
        status: "started".to_string(),
        events,
        interval_ms,
    })
}

#[derive(Debug, Serialize)]
pub struct LoadTestResponse {
    pub status: String,
    pub events: u32,
    pub interval_ms: u64,
}

// ============================================================================
// 15M ARBITRAGE API
// ============================================================================

#[derive(Debug, Deserialize)]
pub struct Arb15mQuery {
    /// Asset: btc, eth, sol, xrp (case-insensitive)
    pub asset: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct Arb15mResponse {
    pub timestamp: i64,
    pub asset: String,
    pub binance: Arb15mBinanceData,
    pub polymarket: Arb15mPolymarketData,
    pub edge: Arb15mEdgeData,
}

#[derive(Debug, Serialize)]
pub struct Arb15mBinanceData {
    pub symbol: String,
    pub mid_price: Option<f64>,
    /// Price at the start of the current 15-minute window
    pub start_price: Option<f64>,
    pub best_bid: Option<f64>,
    pub best_ask: Option<f64>,
    pub spread_bps: Option<f64>,
    pub last_update_ts: i64,
    /// Recent trades for tick chart
    pub recent_trades: Vec<crate::scrapers::binance_arb_feed::TradeTick>,
    /// OHLC history (1-second bars) for price chart
    pub ohlc_history: Vec<crate::scrapers::binance_arb_feed::OhlcPoint>,
    /// Latency history for latency graph
    pub latency_history: Vec<crate::scrapers::binance_arb_feed::LatencySample>,
}

#[derive(Debug, Serialize)]
pub struct Arb15mPolymarketData {
    pub market_slug: Option<String>,
    pub time_remaining_sec: Option<i64>,
    /// Up token
    pub up_token_id: Option<String>,
    pub up_best_bid: Option<f64>,
    pub up_best_ask: Option<f64>,
    pub up_depth: Vec<OrderbookLevel>,
    /// Down token
    pub down_token_id: Option<String>,
    pub down_best_bid: Option<f64>,
    pub down_best_ask: Option<f64>,
    pub down_depth: Vec<OrderbookLevel>,
}

#[derive(Debug, Serialize, Clone)]
pub struct OrderbookLevel {
    pub price: f64,
    pub size: f64,
}

#[derive(Debug, Serialize)]
pub struct Arb15mEdgeData {
    /// Our model's p(Up) based on Binance price movement
    pub model_p_up: Option<f64>,
    /// Market implied p(Up) from Polymarket mid
    pub market_p_up: Option<f64>,
    /// Edge = model_p_up - market_p_up
    pub edge_up: Option<f64>,
    /// Edge in basis points
    pub edge_up_bps: Option<i64>,
    /// Recommended side (BUY_UP, BUY_DOWN, or NONE)
    pub recommended_side: String,
}

/// GET /api/arbitrage/15m - 15M arbitrage monitoring data
pub async fn get_arbitrage_15m(
    Query(params): Query<Arb15mQuery>,
    AxumState(state): AxumState<AppState>,
) -> Result<Json<Arb15mResponse>, StatusCode> {
    use crate::vault::{p_up_driftless_lognormal, UpDownAsset};

    let asset_str = params.asset.as_deref().unwrap_or("btc").to_lowercase();
    let asset = match asset_str.as_str() {
        "btc" | "bitcoin" => UpDownAsset::Btc,
        "eth" | "ethereum" => UpDownAsset::Eth,
        "sol" | "solana" => UpDownAsset::Sol,
        "xrp" | "ripple" => UpDownAsset::Xrp,
        _ => return Err(StatusCode::BAD_REQUEST),
    };

    let binance_symbol = asset.binance_symbol();
    let now = chrono::Utc::now();
    let now_ts = now.timestamp();

    // Get Binance data from arb feed (or fallback to price feed)
    // Find current 15M period start for start_price lookup
    let current_15m_start = (now_ts / 900) * 900;
    let time_into_period = now_ts - current_15m_start;
    let target_start_ts = if time_into_period > 30 {
        current_15m_start
    } else {
        current_15m_start - 900 // Use previous period if we're in first 30s
    };

    // Get start price for this 15m period
    let start_price_data = state
        .binance_price_feed
        .mid_near(binance_symbol, target_start_ts, 30);
    let start_price = start_price_data.map(|p| p.mid);

    let binance_data = if let Some(snapshot) = state.binance_arb_feed.get_snapshot(binance_symbol) {
        let spread_bps = match (snapshot.best_bid, snapshot.best_ask) {
            (Some(b), Some(a)) if b > 0.0 => Some(((a - b) / b) * 10000.0),
            _ => None,
        };

        Arb15mBinanceData {
            symbol: binance_symbol.to_string(),
            mid_price: snapshot.mid_price,
            start_price,
            best_bid: snapshot.best_bid,
            best_ask: snapshot.best_ask,
            spread_bps,
            last_update_ts: snapshot.last_update_ts,
            recent_trades: snapshot.recent_trades,
            ohlc_history: snapshot.ohlc_history,
            latency_history: snapshot.latency_history,
        }
    } else {
        // Fallback to basic price feed
        let latest = state.binance_price_feed.latest_mid(binance_symbol);
        Arb15mBinanceData {
            symbol: binance_symbol.to_string(),
            mid_price: latest.map(|p| p.mid),
            start_price,
            best_bid: None,
            best_ask: None,
            spread_bps: None,
            last_update_ts: latest.map(|p| p.ts * 1000).unwrap_or(0),
            recent_trades: vec![],
            ohlc_history: vec![],
            latency_history: vec![],
        }
    };

    // Find current/next 15M market
    // Markets start at 00:00, 00:15, 00:30, 00:45, etc.
    let next_15m_start = current_15m_start + 900;

    // Use current period if we're >30s in, otherwise use next period
    let (pm_target_start_ts, time_remaining) = if time_into_period > 30 {
        (current_15m_start, 900 - time_into_period)
    } else {
        (next_15m_start, 900 + (30 - time_into_period))
    };

    let slug = format!("{}-updown-15m-{}", asset.as_str(), pm_target_start_ts);

    // Try to get Polymarket orderbook for Up/Down tokens
    let mut pm_data = Arb15mPolymarketData {
        market_slug: Some(slug.clone()),
        time_remaining_sec: Some(time_remaining),
        up_token_id: None,
        up_best_bid: None,
        up_best_ask: None,
        up_depth: vec![],
        down_token_id: None,
        down_best_bid: None,
        down_best_ask: None,
        down_depth: vec![],
    };

    // Look up market from Gamma to get token IDs
    if let Ok(Some(market)) =
        polymarket_gamma::gamma_market_lookup(&state.signal_storage, &state.http_client, &slug)
            .await
    {
        // outcomes: ["Up", "Down"], clobTokenIds: [up_id, down_id]
        if market.outcomes.len() >= 2 && market.clob_token_ids.len() >= 2 {
            let up_idx = market
                .outcomes
                .iter()
                .position(|o| o.to_lowercase() == "up")
                .unwrap_or(0);
            let down_idx = market
                .outcomes
                .iter()
                .position(|o| o.to_lowercase() == "down")
                .unwrap_or(1);

            let up_token = market.clob_token_ids.get(up_idx).cloned();
            let down_token = market.clob_token_ids.get(down_idx).cloned();

            pm_data.up_token_id = up_token.clone();
            pm_data.down_token_id = down_token.clone();

            // Fetch orderbooks
            if let Some(ref token_id) = up_token {
                if let Ok(book) = fetch_polymarket_orderbook(&state, token_id).await {
                    let mut sorted = book.clone();
                    sort_orderbook(&mut sorted);
                    pm_data.up_best_bid = sorted.bids.first().map(|o| o.price);
                    pm_data.up_best_ask = sorted.asks.first().map(|o| o.price);
                    pm_data.up_depth = sorted
                        .bids
                        .iter()
                        .take(10)
                        .map(|o| OrderbookLevel {
                            price: o.price,
                            size: o.size,
                        })
                        .chain(sorted.asks.iter().take(10).map(|o| OrderbookLevel {
                            price: o.price,
                            size: o.size,
                        }))
                        .collect();
                }
            }

            if let Some(ref token_id) = down_token {
                if let Ok(book) = fetch_polymarket_orderbook(&state, token_id).await {
                    let mut sorted = book.clone();
                    sort_orderbook(&mut sorted);
                    pm_data.down_best_bid = sorted.bids.first().map(|o| o.price);
                    pm_data.down_best_ask = sorted.asks.first().map(|o| o.price);
                    pm_data.down_depth = sorted
                        .bids
                        .iter()
                        .take(10)
                        .map(|o| OrderbookLevel {
                            price: o.price,
                            size: o.size,
                        })
                        .chain(sorted.asks.iter().take(10).map(|o| OrderbookLevel {
                            price: o.price,
                            size: o.size,
                        }))
                        .collect();
                }
            }
        }
    }

    // Calculate edge
    let mut edge_data = Arb15mEdgeData {
        model_p_up: None,
        market_p_up: None,
        edge_up: None,
        edge_up_bps: None,
        recommended_side: "NONE".to_string(),
    };

    // Calculate edge using start price
    let current_price = binance_data.mid_price;
    let sigma = state.binance_price_feed.sigma_per_sqrt_s(binance_symbol);

    if let (Some(p_start), Some(p_now), Some(sig)) = (start_price, current_price, sigma) {
        let t_rem = time_remaining.max(1) as f64;

        if let Some(model_p) = p_up_driftless_lognormal(p_start, p_now, sig, t_rem) {
            edge_data.model_p_up = Some(model_p);

            // Market implied p(Up) = mid of Up token
            if let (Some(bid), Some(ask)) = (pm_data.up_best_bid, pm_data.up_best_ask) {
                let market_p = (bid + ask) / 2.0;
                edge_data.market_p_up = Some(market_p);

                let edge = model_p - market_p;
                edge_data.edge_up = Some(edge);
                edge_data.edge_up_bps = Some((edge * 10000.0) as i64);

                // Recommend side if edge > 1%
                if edge > 0.01 {
                    edge_data.recommended_side = "BUY_UP".to_string();
                } else if edge < -0.01 {
                    edge_data.recommended_side = "BUY_DOWN".to_string();
                }
            }
        }
    }

    Ok(Json(Arb15mResponse {
        timestamp: now.timestamp_millis(),
        asset: asset.as_str().to_uppercase(),
        binance: binance_data,
        polymarket: pm_data,
        edge: edge_data,
    }))
}

// =============================================================================
// BACKTEST API
// =============================================================================

#[derive(Debug, Deserialize)]
pub struct BacktestQuery {
    pub asset: Option<String>,
    pub bankroll: Option<f64>,
    pub min_edge: Option<f64>,
    pub kelly_fraction: Option<f64>,
    pub max_position_pct: Option<f64>,
    pub fee_rate: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BacktestPnlPoint {
    pub ts: i64,
    pub equity: f64,
    pub pnl_cumulative: f64,
    pub drawdown: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct BacktestTradeRecord {
    pub ts: i64,
    pub market_slug: String,
    pub outcome: String,
    pub side: String,
    pub entry_price: f64,
    pub exit_price: f64,
    pub shares: f64,
    pub pnl: f64,
    pub edge: f64,
}

#[derive(Debug, Serialize)]
pub struct BacktestConfig {
    pub bankroll: f64,
    pub min_edge: f64,
    pub kelly_fraction: f64,
    pub max_position_pct: f64,
    pub fee_rate: f64,
}

#[derive(Debug, Serialize)]
pub struct BacktestSummary {
    pub total_orders: u64,
    pub opportunities: u64,
    pub trades_taken: u64,
    pub total_volume: f64,
    pub total_fees: f64,
    pub realized_pnl: f64,
    pub gross_profit: f64,
    pub gross_loss: f64,
    pub wins: u64,
    pub losses: u64,
    pub win_rate: f64,
    pub profit_factor: f64,
    pub max_drawdown: f64,
    pub avg_edge: f64,
    pub roi_pct: f64,
    pub avg_pnl_per_trade: f64,
    pub avg_trade_size: f64,
}

#[derive(Debug, Serialize)]
pub struct BacktestDateRange {
    pub start: String,
    pub end: String,
}

#[derive(Debug, Serialize)]
pub struct BacktestResponse {
    pub fetched_at: i64,
    pub asset: String,
    pub date_range: BacktestDateRange,
    pub config: BacktestConfig,
    pub summary: BacktestSummary,
    pub pnl_curve: Vec<BacktestPnlPoint>,
    pub recent_trades: Vec<BacktestTradeRecord>,
}

/// GET /api/backtest/run - Run latency arb backtest on historical data
pub async fn get_backtest_run(
    Query(params): Query<BacktestQuery>,
    AxumState(state): AxumState<AppState>,
) -> Result<Json<BacktestResponse>, (StatusCode, String)> {
    let asset = params
        .asset
        .as_deref()
        .unwrap_or("btc")
        .to_ascii_lowercase();
    let bankroll = params.bankroll.unwrap_or(10_000.0);
    let min_edge = params.min_edge.unwrap_or(0.05);
    let kelly_fraction = params.kelly_fraction.unwrap_or(0.05);
    let max_position_pct = params.max_position_pct.unwrap_or(0.02);
    let fee_rate = params.fee_rate.unwrap_or(0.02);

    let now = chrono::Utc::now();

    // Run backtest on dome_order_events (fetch from API if insufficient data)
    let result = run_backtest_on_data(
        &state.signal_storage,
        state.dome_rest.as_ref(),
        &asset,
        bankroll,
        min_edge,
        kelly_fraction,
        max_position_pct,
        fee_rate,
    )
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(BacktestResponse {
        fetched_at: now.timestamp(),
        asset: asset.to_uppercase(),
        date_range: result.date_range,
        config: BacktestConfig {
            bankroll,
            min_edge,
            kelly_fraction,
            max_position_pct,
            fee_rate,
        },
        summary: result.summary,
        pnl_curve: result.pnl_curve,
        recent_trades: result.recent_trades,
    }))
}

struct BacktestResult {
    date_range: BacktestDateRange,
    summary: BacktestSummary,
    pnl_curve: Vec<BacktestPnlPoint>,
    recent_trades: Vec<BacktestTradeRecord>,
}

async fn run_backtest_on_data(
    storage: &std::sync::Arc<crate::signals::db_storage::DbSignalStorage>,
    dome_rest: Option<&std::sync::Arc<DomeRestClient>>,
    asset: &str,
    bankroll: f64,
    min_edge: f64,
    kelly_fraction: f64,
    max_position_pct: f64,
    fee_rate: f64,
) -> anyhow::Result<BacktestResult> {
    use std::collections::HashMap;

    // Query dome_order_events for the asset
    let where_clause = match asset {
        "btc" => "WHERE market_slug LIKE 'btc-updown-15m%'",
        "eth" => "WHERE market_slug LIKE 'eth-updown-15m%'",
        "sol" => "WHERE market_slug LIKE 'sol-updown-15m%'",
        "xrp" => "WHERE market_slug LIKE 'xrp-updown-15m%'",
        _ => "WHERE market_slug LIKE '%-updown-15m%'",
    };

    let mut orders = storage.query_dome_orders_for_backtest(where_clause).await?;

    // Markets launched Dec 12, 2025 (15m up/down series)
    let market_launch = 1765497600i64; // 2025-12-12 00:00 UTC

    // SOL/XRP need per-market-slug backfill (global feed is too large and misses low-volume assets).
    if matches!(asset, "sol" | "xrp") {
        if let Some(dome) = dome_rest {
            ensure_updown15m_market_history(storage, dome.clone(), asset, market_launch).await?;
            orders = storage.query_dome_orders_for_backtest(where_clause).await?;
        }
    }

    // Check if we need to fetch more historical data
    // For BTC/ETH (high-volume), a light heuristic is fine; SOL/XRP are handled above.
    let min_acceptable_start = market_launch + (24 * 60 * 60); // require at least 1 day of history

    // Find earliest timestamp in our data
    let earliest_ts = orders.iter().map(|o| o.timestamp).min().unwrap_or(i64::MAX);
    let needs_historical_fetch = !matches!(asset, "sol" | "xrp")
        && (earliest_ts > min_acceptable_start || orders.len() < 500);

    if needs_historical_fetch {
        if let Some(dome) = dome_rest {
            let slug_pattern = match asset {
                "btc" => "btc-updown-15m",
                "eth" => "eth-updown-15m",
                "sol" => "sol-updown-15m",
                "xrp" => "xrp-updown-15m",
                _ => "",
            };

            if !slug_pattern.is_empty() {
                let earliest_date = chrono::DateTime::from_timestamp(earliest_ts, 0)
                    .map(|dt| dt.format("%Y-%m-%d").to_string())
                    .unwrap_or_else(|| "unknown".to_string());

                tracing::info!(
                    "Backtest: {} has {} orders (earliest: {}), fetching full history from Dome API...",
                    asset.to_uppercase(), orders.len(), earliest_date
                );

                // Fetch full history by iterating through weekly windows (global feed sampling)
                let now = chrono::Utc::now().timestamp();

                let mut all_api_orders = Vec::new();
                let limit = 1000u32;
                let week_seconds = 7 * 24 * 60 * 60i64;

                // Calculate number of weeks to fetch
                let total_weeks = ((now - market_launch) / week_seconds) + 1;
                tracing::info!(
                    "Fetching {} full history from Dome API ({} weeks since launch)...",
                    asset.to_uppercase(),
                    total_weeks
                );

                // Fetch week by week, starting from most recent
                let mut window_end = now;
                let mut week_num = 0;

                while window_end > market_launch {
                    let window_start = (window_end - week_seconds).max(market_launch);
                    week_num += 1;

                    let mut pagination_key: Option<String> = None;
                    let mut week_orders = 0usize;

                    // Fetch all pages for this week
                    for page in 0..100 {
                        // Max 100 pages per week
                        match dome
                            .get_orders_with_pagination_key(
                                OrdersFilter {
                                    market_slug: None,
                                    condition_id: None,
                                    token_id: None,
                                    user: None,
                                },
                                Some(window_start),
                                Some(window_end),
                                Some(limit),
                                None,
                                pagination_key.clone(),
                            )
                            .await
                        {
                            Ok(resp) => {
                                let total_in_page = resp.orders.len();
                                let matching: Vec<_> = resp
                                    .orders
                                    .into_iter()
                                    .filter(|o| o.market_slug.starts_with(slug_pattern))
                                    .collect();

                                week_orders += matching.len();
                                all_api_orders.extend(matching);

                                pagination_key = resp.pagination.and_then(|p| p.pagination_key);

                                if total_in_page < limit as usize || pagination_key.is_none() {
                                    break;
                                }
                            }
                            Err(e) => {
                                tracing::warn!(
                                    "Failed to fetch week {} page {}: {}",
                                    week_num,
                                    page + 1,
                                    e
                                );
                                break;
                            }
                        }

                        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
                    }

                    if week_num % 2 == 0 || week_num <= 2 {
                        tracing::info!(
                            "Week {}/{}: {} {} orders (total: {})",
                            week_num,
                            total_weeks,
                            week_orders,
                            asset.to_uppercase(),
                            all_api_orders.len()
                        );
                    }

                    window_end = window_start;

                    // Stop if we have enough data
                    if all_api_orders.len() >= 500000 {
                        tracing::info!("Reached 500k orders, stopping");
                        break;
                    }

                    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                }

                tracing::info!(
                    "Completed fetch: {} total {} orders",
                    all_api_orders.len(),
                    asset.to_uppercase()
                );

                if !all_api_orders.is_empty() {
                    tracing::info!(
                        "Fetched {} total orders from Dome API for {}",
                        all_api_orders.len(),
                        asset.to_uppercase()
                    );

                    // Store fetched orders in database for future use
                    let stored = storage
                        .store_dome_orders_batch(&all_api_orders)
                        .await
                        .unwrap_or(0);
                    tracing::info!("Stored {} new orders in database", stored);

                    // Re-query to get all orders including new ones
                    orders = storage.query_dome_orders_for_backtest(where_clause).await?;
                    tracing::info!("Total orders after fetch: {}", orders.len());
                }
            }
        }
    }

    if orders.is_empty() {
        return Ok(BacktestResult {
            date_range: BacktestDateRange {
                start: "---".into(),
                end: "---".into(),
            },
            summary: BacktestSummary {
                total_orders: 0,
                opportunities: 0,
                trades_taken: 0,
                total_volume: 0.0,
                total_fees: 0.0,
                realized_pnl: 0.0,
                gross_profit: 0.0,
                gross_loss: 0.0,
                wins: 0,
                losses: 0,
                win_rate: 0.0,
                profit_factor: 0.0,
                max_drawdown: 0.0,
                avg_edge: 0.0,
                roi_pct: 0.0,
                avg_pnl_per_trade: 0.0,
                avg_trade_size: 0.0,
            },
            pnl_curve: vec![],
            recent_trades: vec![],
        });
    }

    // Date range
    let start_ts = orders.first().map(|o| o.timestamp).unwrap_or(0);
    let end_ts = orders.last().map(|o| o.timestamp).unwrap_or(0);
    let date_range = BacktestDateRange {
        start: chrono::DateTime::from_timestamp(start_ts, 0)
            .map(|d| d.format("%Y-%m-%d").to_string())
            .unwrap_or_else(|| "---".into()),
        end: chrono::DateTime::from_timestamp(end_ts, 0)
            .map(|d| d.format("%Y-%m-%d").to_string())
            .unwrap_or_else(|| "---".into()),
    };

    // Run backtest logic (same as CLI)
    let mut equity = bankroll;
    let mut peak_equity = bankroll;
    let mut total_orders = 0u64;
    let mut opportunities = 0u64;
    let mut trades_taken = 0u64;
    let mut total_volume = 0.0;
    let mut total_fees = 0.0;
    let mut realized_pnl = 0.0;
    let mut gross_profit = 0.0;
    let mut gross_loss = 0.0;
    let mut wins = 0u64;
    let mut losses = 0u64;
    let mut max_drawdown = 0.0;
    let mut edge_sum = 0.0;

    let mut pnl_curve: Vec<BacktestPnlPoint> = Vec::new();
    let mut recent_trades: Vec<BacktestTradeRecord> = Vec::new();
    let mut price_history: HashMap<(String, String), Vec<f64>> = HashMap::new();

    for (i, order) in orders.iter().enumerate() {
        total_orders += 1;

        if order.price <= 0.01 || order.price >= 0.99 {
            continue;
        }

        let key = (order.market_slug.clone(), order.outcome.clone());

        // Fair value estimate from rolling average
        let history = price_history.entry(key.clone()).or_insert_with(Vec::new);
        let fair_value = if history.len() >= 3 {
            history.iter().rev().take(10).sum::<f64>() / history.len().min(10) as f64
        } else {
            order.price
        };

        history.push(order.price);
        if history.len() > 50 {
            history.remove(0);
        }

        // Compute edge
        let raw_edge = if order.side == "BUY" {
            fair_value - order.price
        } else {
            order.price - fair_value
        };
        let effective_edge = raw_edge - fee_rate * 2.0;

        if effective_edge.abs() < 0.001 {
            continue;
        }

        opportunities += 1;

        if effective_edge < min_edge {
            continue;
        }

        edge_sum += effective_edge;

        // Find exit price
        let exit_price = find_exit_price_in_orders(&orders, i, &order.market_slug, &order.outcome)
            .unwrap_or(order.price);

        // Kelly sizing
        let odds = (1.0 / order.price.max(0.01)) - 1.0;
        let kelly_bet = (effective_edge * odds).max(0.0);
        let position_frac = (kelly_bet * kelly_fraction).min(max_position_pct);
        let position_usd = (position_frac * equity).min(500.0);

        if position_usd < 5.0 || !position_usd.is_finite() {
            continue;
        }

        let shares = position_usd / order.price.max(0.01);
        let cost = position_usd;
        let entry_fee = cost * fee_rate;
        let exit_value = shares * exit_price;
        let exit_fee = exit_value * fee_rate;
        let trade_fees = entry_fee + exit_fee;

        if !cost.is_finite() || !exit_value.is_finite() || cost > 10000.0 || exit_value > 20000.0 {
            continue;
        }

        let trade_pnl = if order.side == "BUY" {
            exit_value - cost - trade_fees
        } else {
            cost - exit_value - trade_fees
        };

        if !trade_pnl.is_finite() || trade_pnl.abs() > 1000.0 {
            continue;
        }

        trades_taken += 1;
        total_volume += cost;
        total_fees += trade_fees;
        realized_pnl += trade_pnl;

        if trade_pnl > 0.0 {
            gross_profit += trade_pnl;
            wins += 1;
        } else {
            gross_loss += trade_pnl.abs();
            losses += 1;
        }

        equity += trade_pnl;

        if equity > peak_equity {
            peak_equity = equity;
        }
        let dd = peak_equity - equity;
        if dd > max_drawdown {
            max_drawdown = dd;
        }

        // Record PnL point (sample every 100 trades)
        if trades_taken % 100 == 0 || trades_taken <= 10 {
            pnl_curve.push(BacktestPnlPoint {
                ts: order.timestamp,
                equity,
                pnl_cumulative: realized_pnl,
                drawdown: dd,
            });
        }

        // Record recent trades (last 100)
        if recent_trades.len() < 100 || trades_taken > total_orders - 100 {
            recent_trades.push(BacktestTradeRecord {
                ts: order.timestamp,
                market_slug: order.market_slug.clone(),
                outcome: order.outcome.clone(),
                side: order.side.clone(),
                entry_price: order.price,
                exit_price,
                shares,
                pnl: trade_pnl,
                edge: effective_edge,
            });
            if recent_trades.len() > 100 {
                recent_trades.remove(0);
            }
        }
    }

    // Final PnL point
    if let Some(last) = orders.last() {
        pnl_curve.push(BacktestPnlPoint {
            ts: last.timestamp,
            equity,
            pnl_cumulative: realized_pnl,
            drawdown: peak_equity - equity,
        });
    }

    let win_rate = if wins + losses > 0 {
        wins as f64 / (wins + losses) as f64
    } else {
        0.0
    };

    let profit_factor = if gross_loss > 0.01 {
        gross_profit / gross_loss
    } else if gross_profit > 0.0 {
        f64::INFINITY
    } else {
        0.0
    };

    let avg_edge = if opportunities > 0 {
        edge_sum / opportunities as f64
    } else {
        0.0
    };

    let roi_pct = (realized_pnl / bankroll) * 100.0;
    let avg_pnl_per_trade = if trades_taken > 0 {
        realized_pnl / trades_taken as f64
    } else {
        0.0
    };
    let avg_trade_size = if trades_taken > 0 {
        total_volume / trades_taken as f64
    } else {
        0.0
    };

    Ok(BacktestResult {
        date_range,
        summary: BacktestSummary {
            total_orders,
            opportunities,
            trades_taken,
            total_volume,
            total_fees,
            realized_pnl,
            gross_profit,
            gross_loss,
            wins,
            losses,
            win_rate,
            profit_factor,
            max_drawdown,
            avg_edge,
            roi_pct,
            avg_pnl_per_trade,
            avg_trade_size,
        },
        pnl_curve,
        recent_trades,
    })
}

struct Updown15mBackfillOutcome {
    fetched_orders: usize,
    stored_new: usize,
    failed_slugs: Vec<String>,
}

async fn fetch_market_orders_with_retry(
    dome: &DomeRestClient,
    market_slug: &str,
) -> anyhow::Result<Vec<crate::scrapers::dome_rest::DomeOrder>> {
    const MAX_RETRIES: u32 = 3;
    for attempt in 1..=MAX_RETRIES {
        match dome
            .get_all_orders_for_market(market_slug, None, None)
            .await
        {
            Ok(orders) => return Ok(orders),
            Err(e) if attempt < MAX_RETRIES => {
                tokio::time::sleep(Duration::from_millis(200 * attempt as u64)).await;
                continue;
            }
            Err(e) => return Err(e),
        }
    }
    unreachable!();
}

async fn backfill_orders_for_market_slugs(
    storage: &std::sync::Arc<crate::signals::db_storage::DbSignalStorage>,
    dome: std::sync::Arc<DomeRestClient>,
    market_slugs: Vec<String>,
    concurrency: usize,
) -> anyhow::Result<Updown15mBackfillOutcome> {
    use futures_util::stream::{self, StreamExt};

    let concurrency = concurrency.clamp(1, 32);

    let results: Vec<(
        String,
        anyhow::Result<Vec<crate::scrapers::dome_rest::DomeOrder>>,
    )> = stream::iter(market_slugs.into_iter())
        .map(|slug| {
            let dome = dome.clone();
            async move {
                let res = fetch_market_orders_with_retry(&dome, &slug).await;
                (slug, res)
            }
        })
        .buffer_unordered(concurrency)
        .collect()
        .await;

    let mut all_orders: Vec<crate::scrapers::dome_rest::DomeOrder> = Vec::new();
    let mut failed_slugs: Vec<String> = Vec::new();

    for (slug, res) in results {
        match res {
            Ok(mut orders) => all_orders.append(&mut orders),
            Err(e) => {
                tracing::warn!("updown15m backfill failed for {}: {}", slug, e);
                failed_slugs.push(slug);
            }
        }
    }

    let fetched_orders = all_orders.len();
    let stored_new = storage.store_dome_orders_batch(&all_orders).await?;

    Ok(Updown15mBackfillOutcome {
        fetched_orders,
        stored_new,
        failed_slugs,
    })
}

async fn ensure_updown15m_market_history(
    storage: &std::sync::Arc<crate::signals::db_storage::DbSignalStorage>,
    dome: std::sync::Arc<DomeRestClient>,
    asset: &str,
    market_launch: i64,
) -> anyhow::Result<()> {
    let asset = asset.to_ascii_lowercase();
    let interval = 900i64;
    let now_ts = chrono::Utc::now().timestamp();
    let now_floor = now_ts - now_ts.rem_euclid(interval);

    let complete_key = format!("updown15m_orders_complete_{}", asset);
    let until_key = format!("updown15m_orders_until_ts_{}", asset);
    let failed_key = format!("updown15m_orders_failed_slugs_{}", asset);

    let complete = storage.get_metadata_value(&complete_key)?.as_deref() == Some("1");
    let until_ts = storage
        .get_metadata_value(&until_key)?
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(0);
    let failed_slugs: Vec<String> = storage
        .get_metadata_value(&failed_key)?
        .and_then(|v| serde_json::from_str(&v).ok())
        .unwrap_or_default();

    let target_until = now_floor;
    let mut slugs: Vec<String> = Vec::new();
    slugs.extend(failed_slugs);

    let start_ts = if until_ts > 0 {
        until_ts + interval
    } else {
        market_launch
    };

    if start_ts <= target_until {
        let estimated = ((target_until - start_ts) / interval).max(0) as usize + 1;
        slugs.reserve(estimated);
        for ts in (start_ts..=target_until).step_by(interval as usize) {
            slugs.push(format!("{}-updown-15m-{}", asset, ts));
        }
    }

    if slugs.is_empty() {
        return Ok(());
    }

    tracing::info!(
        "Updown15m backfill {}: slugs={} (complete={}, until_ts={}, target_until={})",
        asset.to_uppercase(),
        slugs.len(),
        complete,
        until_ts,
        target_until
    );

    let outcome = backfill_orders_for_market_slugs(storage, dome, slugs, 24).await?;

    tracing::info!(
        "Updown15m backfill {}: fetched={} stored_new={} failed_slugs={}",
        asset.to_uppercase(),
        outcome.fetched_orders,
        outcome.stored_new,
        outcome.failed_slugs.len()
    );

    storage.set_metadata_value(&until_key, &target_until.to_string())?;

    if outcome.failed_slugs.is_empty() {
        storage.set_metadata_value(&failed_key, "")?;
        storage.set_metadata_value(&complete_key, "1")?;
    } else {
        storage.set_metadata_value(&failed_key, &serde_json::to_string(&outcome.failed_slugs)?)?;
        if !complete {
            storage.set_metadata_value(&complete_key, "0")?;
        }
    }

    Ok(())
}

fn find_exit_price_in_orders(
    orders: &[crate::signals::db_storage::DomeOrderForBacktest],
    start_idx: usize,
    market: &str,
    outcome: &str,
) -> Option<f64> {
    let entry_time = orders[start_idx].timestamp;

    for order in orders.iter().skip(start_idx + 1) {
        if order.market_slug != market || order.outcome != outcome {
            continue;
        }

        let time_diff = order.timestamp - entry_time;

        if time_diff >= 60 && time_diff <= 900 {
            return Some(order.price);
        }

        if time_diff > 1800 {
            break;
        }
    }

    None
}

// ==================== PAPER TRADING ====================

use parking_lot::Mutex as ParkingMutex;
use std::sync::atomic::{AtomicBool, Ordering};

lazy_static::lazy_static! {
    static ref PAPER_TRADING_STATE: ParkingMutex<PaperTradingEngine> =
        ParkingMutex::new(PaperTradingEngine::new());
}

static PAPER_TRADING_RUNNING: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Serialize)]
pub struct PaperTradingSummary {
    pub signals_seen: u64,
    pub opportunities: u64,
    pub trades_taken: u64,
    pub total_volume: f64,
    pub total_fees: f64,
    pub realized_pnl: f64,
    pub gross_profit: f64,
    pub gross_loss: f64,
    pub wins: u64,
    pub losses: u64,
    pub win_rate: f64,
    pub profit_factor: f64,
    pub max_drawdown: f64,
    pub avg_edge: f64,
    pub roi_pct: f64,
    pub avg_pnl_per_trade: f64,
    pub avg_trade_size: f64,
}

#[derive(Clone, Serialize)]
pub struct PaperTradeRecord {
    pub ts: i64,
    pub market_slug: String,
    pub outcome: String,
    pub side: String,
    pub entry_price: f64,
    pub exit_price: f64,
    pub shares: f64,
    pub pnl: f64,
    pub edge: f64,
}

#[derive(Clone, Serialize)]
pub struct PaperTradingConfig {
    pub bankroll: f64,
    pub min_edge: f64,
    pub kelly_fraction: f64,
    pub max_position_pct: f64,
    pub fee_rate: f64,
}

struct PaperTradingEngine {
    is_running: bool,
    started_at: Option<i64>,
    asset: String,
    config: PaperTradingConfig,
    // State
    cash: f64,
    peak_equity: f64,
    max_drawdown: f64,
    // Stats
    signals_seen: u64,
    opportunities: u64,
    trades_taken: u64,
    total_volume: f64,
    total_fees: f64,
    realized_pnl: f64,
    gross_profit: f64,
    gross_loss: f64,
    wins: u64,
    losses: u64,
    edge_sum: f64,
    // History
    pnl_curve: Vec<BacktestPnlPoint>,
    recent_trades: Vec<PaperTradeRecord>,
}

impl PaperTradingEngine {
    fn new() -> Self {
        Self {
            is_running: false,
            started_at: None,
            asset: "btc".to_string(),
            config: PaperTradingConfig {
                bankroll: 10000.0,
                min_edge: 0.05,
                kelly_fraction: 0.05,
                max_position_pct: 0.02,
                fee_rate: 0.005,
            },
            cash: 10000.0,
            peak_equity: 10000.0,
            max_drawdown: 0.0,
            signals_seen: 0,
            opportunities: 0,
            trades_taken: 0,
            total_volume: 0.0,
            total_fees: 0.0,
            realized_pnl: 0.0,
            gross_profit: 0.0,
            gross_loss: 0.0,
            wins: 0,
            losses: 0,
            edge_sum: 0.0,
            pnl_curve: Vec::new(),
            recent_trades: Vec::new(),
        }
    }

    fn start(&mut self, asset: String, config: PaperTradingConfig) {
        self.is_running = true;
        self.started_at = Some(chrono::Utc::now().timestamp());
        self.asset = asset;
        self.config = config.clone();
        self.cash = config.bankroll;
        self.peak_equity = config.bankroll;
        self.max_drawdown = 0.0;
        self.signals_seen = 0;
        self.opportunities = 0;
        self.trades_taken = 0;
        self.total_volume = 0.0;
        self.total_fees = 0.0;
        self.realized_pnl = 0.0;
        self.gross_profit = 0.0;
        self.gross_loss = 0.0;
        self.wins = 0;
        self.losses = 0;
        self.edge_sum = 0.0;
        self.pnl_curve.clear();
        self.recent_trades.clear();

        // Add initial point
        self.pnl_curve.push(BacktestPnlPoint {
            ts: chrono::Utc::now().timestamp(),
            equity: config.bankroll,
            pnl_cumulative: 0.0,
            drawdown: 0.0,
        });
    }

    fn stop(&mut self) {
        self.is_running = false;
    }

    fn reset(&mut self) {
        *self = Self::new();
    }

    fn get_summary(&self) -> PaperTradingSummary {
        let win_rate = if self.trades_taken > 0 {
            self.wins as f64 / self.trades_taken as f64
        } else {
            0.0
        };

        let profit_factor = if self.gross_loss > 0.0 {
            self.gross_profit / self.gross_loss
        } else if self.gross_profit > 0.0 {
            f64::INFINITY
        } else {
            0.0
        };

        let avg_edge = if self.trades_taken > 0 {
            self.edge_sum / self.trades_taken as f64
        } else {
            0.0
        };

        let avg_pnl = if self.trades_taken > 0 {
            self.realized_pnl / self.trades_taken as f64
        } else {
            0.0
        };

        let avg_size = if self.trades_taken > 0 {
            self.total_volume / self.trades_taken as f64
        } else {
            0.0
        };

        let roi = (self.realized_pnl / self.config.bankroll) * 100.0;

        PaperTradingSummary {
            signals_seen: self.signals_seen,
            opportunities: self.opportunities,
            trades_taken: self.trades_taken,
            total_volume: self.total_volume,
            total_fees: self.total_fees,
            realized_pnl: self.realized_pnl,
            gross_profit: self.gross_profit,
            gross_loss: self.gross_loss,
            wins: self.wins,
            losses: self.losses,
            win_rate,
            profit_factor: profit_factor.min(999.99),
            max_drawdown: self.max_drawdown,
            avg_edge,
            roi_pct: roi,
            avg_pnl_per_trade: avg_pnl,
            avg_trade_size: avg_size,
        }
    }

    fn record_trade(&mut self, trade: PaperTradeRecord) {
        self.trades_taken += 1;
        self.total_volume += trade.shares * trade.entry_price;

        let fee = trade.shares * trade.entry_price * self.config.fee_rate;
        self.total_fees += fee;

        let gross_pnl = trade.pnl;
        let net_pnl = gross_pnl - fee;

        self.realized_pnl += net_pnl;
        self.cash += net_pnl;
        self.edge_sum += trade.edge;

        if gross_pnl > 0.0 {
            self.gross_profit += gross_pnl;
            self.wins += 1;
        } else {
            self.gross_loss += gross_pnl.abs();
            self.losses += 1;
        }

        // Update drawdown
        if self.cash > self.peak_equity {
            self.peak_equity = self.cash;
        }
        let dd = self.peak_equity - self.cash;
        if dd > self.max_drawdown {
            self.max_drawdown = dd;
        }

        // Record PnL point
        self.pnl_curve.push(BacktestPnlPoint {
            ts: trade.ts,
            equity: self.cash,
            pnl_cumulative: self.realized_pnl,
            drawdown: dd,
        });

        // Keep recent trades bounded
        self.recent_trades.push(trade);
        if self.recent_trades.len() > 200 {
            self.recent_trades.remove(0);
        }
    }
}

#[derive(Deserialize)]
pub struct PaperTradingStartRequest {
    pub asset: String,
    pub bankroll: f64,
    pub min_edge: f64,
    pub kelly_fraction: f64,
    pub max_position_pct: f64,
}

#[derive(Serialize)]
pub struct PaperTradingStateResponse {
    pub fetched_at: i64,
    pub is_running: bool,
    pub started_at: Option<i64>,
    pub uptime_secs: i64,
    pub asset: String,
    pub config: PaperTradingConfig,
    pub summary: PaperTradingSummary,
    pub pnl_curve: Vec<BacktestPnlPoint>,
    pub recent_trades: Vec<PaperTradeRecord>,
}

/// GET /api/paper/state
pub async fn get_paper_trading_state(
    AxumState(_state): AxumState<AppState>,
) -> Json<PaperTradingStateResponse> {
    let engine = PAPER_TRADING_STATE.lock();
    let now = chrono::Utc::now().timestamp();

    let uptime = if let Some(started) = engine.started_at {
        if engine.is_running {
            now - started
        } else {
            0
        }
    } else {
        0
    };

    Json(PaperTradingStateResponse {
        fetched_at: now,
        is_running: engine.is_running,
        started_at: engine.started_at,
        uptime_secs: uptime,
        asset: engine.asset.clone(),
        config: engine.config.clone(),
        summary: engine.get_summary(),
        pnl_curve: engine.pnl_curve.clone(),
        recent_trades: engine.recent_trades.clone(),
    })
}

/// POST /api/paper/start
pub async fn post_paper_trading_start(
    AxumState(state): AxumState<AppState>,
    AxumJson(req): AxumJson<PaperTradingStartRequest>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let config = PaperTradingConfig {
        bankroll: req.bankroll,
        min_edge: req.min_edge,
        kelly_fraction: req.kelly_fraction,
        max_position_pct: req.max_position_pct,
        fee_rate: 0.005,
    };

    {
        let mut engine = PAPER_TRADING_STATE.lock();
        engine.start(req.asset.clone(), config);
    }

    PAPER_TRADING_RUNNING.store(true, Ordering::SeqCst);

    // Spawn the paper trading loop with Binance feed for latency arbitrage
    let asset = req.asset.clone();
    let storage = state.signal_storage.clone();
    let binance_feed = state.binance_feed.clone();

    tokio::spawn(async move {
        paper_trading_loop(asset, storage, binance_feed).await;
    });

    Ok(Json(serde_json::json!({ "success": true })))
}

/// POST /api/paper/stop
pub async fn post_paper_trading_stop(
    AxumState(_state): AxumState<AppState>,
) -> Json<serde_json::Value> {
    PAPER_TRADING_RUNNING.store(false, Ordering::SeqCst);

    {
        let mut engine = PAPER_TRADING_STATE.lock();
        engine.stop();
    }

    Json(serde_json::json!({ "success": true }))
}

/// POST /api/paper/reset
pub async fn post_paper_trading_reset(
    AxumState(_state): AxumState<AppState>,
) -> Json<serde_json::Value> {
    PAPER_TRADING_RUNNING.store(false, Ordering::SeqCst);

    {
        let mut engine = PAPER_TRADING_STATE.lock();
        engine.reset();
    }

    Json(serde_json::json!({ "success": true }))
}

async fn paper_trading_loop(
    asset: String,
    _storage: std::sync::Arc<crate::signals::db_storage::DbSignalStorage>,
    binance_feed: std::sync::Arc<crate::scrapers::binance_price_feed::BinancePriceFeed>,
) {
    use crate::vault::updown15m::{
        p_up_driftless_lognormal, parse_updown_15m_slug, shrink_to_half,
    };
    use futures_util::StreamExt;
    use std::collections::{HashMap, HashSet};
    use tokio::time::{interval, sleep, Duration};

    tracing::info!(
        "[PAPER] Starting LATENCY ARBITRAGE paper trading (WebSocket mode) for asset: {}",
        asset
    );

    // Get Dome API key from environment
    let dome_api_key = std::env::var("DOME_API_KEY").unwrap_or_default();
    if dome_api_key.is_empty() {
        tracing::error!("[PAPER] DOME_API_KEY not set, cannot run paper trading");
        return;
    }

    // Track open positions: market_slug -> (entry_price, shares, side, model_p, entry_ts)
    let mut open_positions: HashMap<String, (f64, f64, String, f64, i64)> = HashMap::new();

    // Track processed order hashes to avoid duplicates
    let mut processed_orders: HashSet<String> = HashSet::new();

    // Shrink parameter for conservative model
    let shrink = 0.35;

    // Asset filter for market slug
    let asset_prefix = match asset.as_str() {
        "btc" => "btc-updown-15m",
        "eth" => "eth-updown-15m",
        "sol" => "sol-updown-15m",
        "xrp" => "xrp-updown-15m",
        _ => "-updown-15m",
    };

    // Connect to Dome WebSocket for real-time order streaming
    // We'll use all tracked wallets to get a broad stream of orders
    let tracked_wallets: Vec<String> = crate::models::Config::from_env()
        .tracked_wallets
        .keys()
        .cloned()
        .collect();

    tracing::info!(
        "[PAPER] Connecting to Dome WebSocket with {} tracked wallets for order stream",
        tracked_wallets.len()
    );

    let (ws_client, mut order_rx) = crate::scrapers::dome_websocket::DomeWebSocketClient::new(
        dome_api_key.clone(),
        tracked_wallets,
    );

    // Spawn WebSocket connection in background
    tokio::spawn(async move {
        if let Err(e) = ws_client.run().await {
            tracing::error!("[PAPER] WebSocket error: {}", e);
        }
    });

    // Give WebSocket time to connect
    sleep(Duration::from_millis(500)).await;

    // Also set up a periodic position check timer
    let mut position_check = interval(Duration::from_secs(5));

    tracing::info!("[PAPER] WebSocket connected, streaming real-time orders");

    while PAPER_TRADING_RUNNING.load(Ordering::SeqCst) {
        tokio::select! {
            // Real-time order from WebSocket - this is the HFT path
            Some(ws_order) = order_rx.recv() => {
                let order_received_ns = std::time::Instant::now();
                let order_received_ts = chrono::Utc::now().timestamp_millis();

                // Convert WebSocket order to our format
                let order = DomeApiOrder {
                    order_hash: ws_order.order_hash.clone(),
                    timestamp: ws_order.timestamp,
                    market_slug: ws_order.market_slug.clone(),
                    outcome: ws_order.token_label.clone().unwrap_or_else(|| "Up".to_string()),
                    price: ws_order.price,
                };

                // Filter for relevant markets
                if !order.market_slug.contains(asset_prefix) && !(asset == "all" && order.market_slug.contains("-updown-15m")) {
                    continue;
                }

                // Skip already processed
                if processed_orders.contains(&order.order_hash) {
                    continue;
                }
                processed_orders.insert(order.order_hash.clone());
                if processed_orders.len() > 10000 {
                    processed_orders.clear();
                }

                // REAL LATENCY: Time from order timestamp to our receipt via WebSocket
                // This should be much lower than REST polling!
                let network_latency_ms = (order_received_ts - (order.timestamp * 1000)).max(0);
                let network_latency_us = (network_latency_ms * 1000) as u64;

                let comp = crate::latency::global_comprehensive();
                comp.t2t.record_stage(crate::latency::T2TStage::MdReceive, network_latency_us);

                // Get config
                let config = {
                    let engine = PAPER_TRADING_STATE.lock();
                    engine.config.clone()
                };

                // Process this order through our strategy
                process_paper_order(
                    &order,
                    &binance_feed,
                    &mut open_positions,
                    &config,
                    shrink,
                    network_latency_us,
                    order_received_ns,
                );
            }

            // Periodic position management - BINARY OUTCOME RESOLUTION
            _ = position_check.tick() => {
                use crate::vault::updown15m::parse_updown_15m_slug;
                let now = chrono::Utc::now().timestamp();

                // Close positions when their market has EXPIRED (binary settlement)
                let positions_to_close: Vec<_> = open_positions.iter()
                    .filter_map(|(slug, (entry_price, shares, side, _model_p, entry_ts))| {
                        // Parse the market slug to get end timestamp
                        if let Some(market) = parse_updown_15m_slug(slug) {
                            // Market has expired - time to settle
                            if now >= market.end_ts {
                                Some((slug.clone(), *entry_price, *shares, side.clone(), market, *entry_ts))
                            } else {
                                None
                            }
                        } else {
                            // Can't parse slug - close after 15 minutes as fallback
                            if now - entry_ts >= 900 {
                                Some((slug.clone(), *entry_price, *shares, side.clone(),
                                    crate::vault::updown15m::UpDown15mMarket {
                                        asset: crate::vault::UpDownAsset::Btc,
                                        start_ts: *entry_ts,
                                        end_ts: *entry_ts + 900,
                                    }, *entry_ts))
                            } else {
                                None
                            }
                        }
                    })
                    .collect();

                for (slug, entry_price, shares, side, market, _entry_ts) in positions_to_close {
                    open_positions.remove(&slug);

                    // BINARY OUTCOME: Determine actual result from Binance price
                    let binance_symbol = market.asset.binance_symbol();
                    let start_price = binance_feed.mid_near(binance_symbol, market.start_ts, 120).map(|p| p.mid);
                    let end_price = binance_feed.mid_near(binance_symbol, market.end_ts, 120)
                        .or_else(|| binance_feed.latest_mid(binance_symbol))
                        .map(|p| p.mid);

                    let (actual_outcome, pnl) = match (start_price, end_price) {
                        (Some(p_start), Some(p_end)) => {
                            // Actual outcome: Up if price increased, Down if decreased
                            let actual_up = p_end >= p_start;
                            let we_bet_up = side == "BUY_UP";
                            let we_won = actual_up == we_bet_up;

                            // BINARY PNL (after fees):
                            // Fee ~1.5% on entry only (no exit fee at settlement)
                            let price_mid_distance = (entry_price - 0.5).abs();
                            let entry_fee_rate = 0.015 * (1.0 - price_mid_distance);
                            let notional = shares * entry_price;
                            let fees = notional * entry_fee_rate;

                            // Win: we paid entry_price per share, get $1.00 per share
                            // Lose: we paid entry_price per share, get $0.00
                            let gross_pnl = if we_won {
                                shares * (1.0 - entry_price)
                            } else {
                                -(shares * entry_price)
                            };
                            let pnl = gross_pnl - fees;

                            let outcome_str = if actual_up { "Up" } else { "Down" };
                            (outcome_str.to_string(), pnl)
                        }
                        _ => {
                            // Can't determine outcome - assume 50/50 (return to fair value)
                            tracing::warn!("[PAPER] Could not determine outcome for {} - no price data", slug);
                            let outcome_str = if side == "BUY_UP" { "Up" } else { "Down" };
                            (outcome_str.to_string(), 0.0)
                        }
                    };

                    // Exit price for display: 1.0 if won, 0.0 if lost
                    let exit_price = if pnl > 0.0 { 1.0 } else { 0.0 };

                    tracing::info!(
                        "[PAPER] SETTLED: {} {} @ {:.4} -> {} (PnL: ${:.2})",
                        side, slug, entry_price, actual_outcome, pnl
                    );

                    let trade = PaperTradeRecord {
                        ts: now,
                        market_slug: slug,
                        outcome: actual_outcome,
                        side: format!("{}_SETTLED", side),
                        entry_price,
                        exit_price,
                        shares,
                        pnl,
                        edge: (1.0 - entry_price).abs(), // Edge was the discount from $1
                    };

                    let mut engine = PAPER_TRADING_STATE.lock();
                    engine.record_trade(trade);
                }
            }
        }
    }

    tracing::info!("[PAPER] Paper trading loop stopped");
}

/// Process a single order through the paper trading strategy
fn process_paper_order(
    order: &DomeApiOrder,
    binance_feed: &std::sync::Arc<crate::scrapers::binance_price_feed::BinancePriceFeed>,
    open_positions: &mut HashMap<String, (f64, f64, String, f64, i64)>,
    config: &PaperTradingConfig,
    shrink: f64,
    network_latency_us: u64,
    order_received_ns: std::time::Instant,
) {
    use crate::vault::updown15m::{parse_updown_15m_slug, shrink_to_half};
    use crate::vault::{estimate_p_up_rnjd, RnjdParams};

    let signal_start = std::time::Instant::now();
    let now = chrono::Utc::now().timestamp();
    let comp = crate::latency::global_comprehensive();

    {
        let mut engine = PAPER_TRADING_STATE.lock();
        engine.signals_seen += 1;
    }
    comp.throughput
        .strategy_evals
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

    // Parse market slug
    let market = match parse_updown_15m_slug(&order.market_slug) {
        Some(m) => m,
        None => return,
    };

    // Skip expired markets
    if now >= market.end_ts {
        return;
    }

    let t_rem = (market.end_ts - now) as f64;
    if t_rem < 30.0 {
        return;
    }

    // Get Binance prices
    let binance_symbol = market.asset.binance_symbol();
    let p_start = match binance_feed.mid_near(binance_symbol, market.start_ts, 60) {
        Some(p) => p.mid,
        None => return,
    };
    let p_now = match binance_feed.latest_mid(binance_symbol) {
        Some(p) => p.mid,
        None => return,
    };
    let sigma = match binance_feed.sigma_per_sqrt_s(binance_symbol) {
        Some(s) if s > 0.0 => s,
        _ => return,
    };

    // =========================================================================
    // RN-JD MODEL: Risk-Neutral Jump-Diffusion probability estimation
    // Based on: "Toward Black-Scholes for Prediction Markets" (arXiv:2510.15205)
    // =========================================================================

    // Convert price volatility to annualized form for RN-JD
    // sigma is per sqrt(second), annualize it: sigma_annual = sigma * sqrt(365.25 * 24 * 3600)
    let sigma_annual = sigma * (365.25 * 24.0 * 3600.0_f64).sqrt();

    // Use market price as prior for belief volatility estimation
    let market_p = order.price.clamp(0.01, 0.99);

    // RN-JD parameters - conservative settings
    let rnjd_params = RnjdParams {
        sigma_b: 2.0, // Moderate belief volatility
        lambda: 0.0,  // No jumps in base case (pure diffusion)
        mu_j: 0.0,
        sigma_j: 0.1,
    };

    // Estimate p_up using RN-JD model with risk-neutral drift correction
    let rnjd_estimate =
        match estimate_p_up_rnjd(p_start, p_now, market_p, sigma_annual, t_rem, &rnjd_params) {
            Some(est) => est,
            None => return,
        };

    // Apply shrinkage for conservatism (pull toward 0.5)
    let model_p_up = shrink_to_half(rnjd_estimate.p_up, shrink);
    let model_p_down = 1.0 - model_p_up;

    // Record signal compute latency
    let signal_compute_us = signal_start.elapsed().as_micros() as u64;
    comp.t2t
        .record_stage(crate::latency::T2TStage::SignalCompute, signal_compute_us);
    comp.throughput
        .signals_generated
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

    // Determine outcome and edge
    let (model_p, is_up) = if order.outcome.to_lowercase() == "up" {
        (model_p_up, true)
    } else {
        (model_p_down, false)
    };

    let edge = model_p - order.price;
    let edge_abs = edge.abs();

    // Track opportunity
    if edge_abs >= config.min_edge {
        let mut engine = PAPER_TRADING_STATE.lock();
        engine.opportunities += 1;
    }

    // Check for exit on existing position
    let pos_key = order.market_slug.clone();
    if let Some((entry_price, shares, side, _, entry_ts)) = open_positions.remove(&pos_key) {
        // CRITICAL: Only consider exit if the incoming order matches our position's outcome
        // If we hold BUY_UP, we can only exit on UP orders (to sell UP shares)
        // If we hold BUY_DOWN, we can only exit on DOWN orders
        let order_is_up = order.outcome.to_lowercase() == "up";
        let position_is_up = side == "BUY_UP";

        if order_is_up != position_is_up {
            // Wrong outcome - put position back and skip
            open_positions.insert(
                pos_key.clone(),
                (entry_price, shares, side, model_p, entry_ts),
            );
            return;
        }

        let price_move = order.price - entry_price;

        // Fee calculation: ~1.5% per side, so ~3% round-trip
        // At mid prices (~0.50), fee is ~3% of notional
        // At extreme prices (near 0 or 1), fees are lower
        // We use a simplified model: fee_rate varies with price distance from 0.5
        let price_mid_distance = (entry_price - 0.5).abs();
        let fee_rate = 0.03 * (1.0 - price_mid_distance); // 3% at 0.50, ~1.5% at extremes
        let min_profitable_move = fee_rate; // Need to clear round-trip fees

        // Favorable: price moved enough to cover fees + some profit
        let favorable = (side == "BUY_UP" && price_move > min_profitable_move)
            || (side == "BUY_DOWN" && price_move < -min_profitable_move);
        // Edge reversal: exit if model now says we're on the wrong side
        // BUY_UP: entered with positive edge, exit if edge goes negative (Up is overpriced)
        // BUY_DOWN: entered with positive edge, exit if edge goes negative (Down is overpriced)
        let reversed = (side == "BUY_UP" && edge < -config.min_edge)
            || (side == "BUY_DOWN" && edge < -config.min_edge);

        let exit_reason = if favorable {
            "favorable"
        } else if reversed {
            "reversed"
        } else {
            "timeout"
        };

        if favorable || reversed || (now - entry_ts) >= 180 {
            // Early exit - PnL after fees
            let exit_price = order.price;
            let gross_pnl = shares * (exit_price - entry_price);
            // Deduct ~3% round-trip fees (entry + exit) from notional
            let notional = shares * entry_price;
            let fees = notional * fee_rate;
            let pnl = gross_pnl - fees;

            tracing::debug!(
                "[PAPER-WS] EXIT ({}): {} {} @ {:.4} -> {:.4} (PnL: ${:.2}, hold: {}s)",
                exit_reason,
                side,
                order.market_slug,
                entry_price,
                exit_price,
                pnl,
                now - entry_ts
            );

            let trade = PaperTradeRecord {
                ts: order.timestamp,
                market_slug: order.market_slug.clone(),
                outcome: if side == "BUY_UP" {
                    "Up".to_string()
                } else {
                    "Down".to_string()
                },
                side: format!("{}_EXIT", side),
                entry_price,
                exit_price,
                shares,
                pnl,
                edge: edge_abs,
            };

            let mut engine = PAPER_TRADING_STATE.lock();
            engine.record_trade(trade);
            return; // Don't re-enter on the same order
        } else {
            // Keep position open
            open_positions.insert(
                pos_key.clone(),
                (entry_price, shares, side, model_p, entry_ts),
            );
            return;
        }
    }

    // Entry: if significant edge and no position
    if edge_abs >= config.min_edge && !open_positions.contains_key(&pos_key) {
        let risk_start = std::time::Instant::now();

        let (entry_side, _target_outcome) = if edge > 0.0 {
            if is_up {
                ("BUY_UP", "Up")
            } else {
                ("BUY_DOWN", "Down")
            }
        } else {
            return; // Skip selling
        };

        // Kelly sizing
        let kelly_edge = edge_abs;
        let kelly_f = config.kelly_fraction * kelly_edge * 10.0;
        let position_pct = kelly_f.min(config.max_position_pct);
        let position_usd = config.bankroll * position_pct;

        let risk_check_us = risk_start.elapsed().as_micros() as u64;
        comp.t2t
            .record_stage(crate::latency::T2TStage::RiskCheck, risk_check_us);
        comp.throughput
            .risk_checks
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        if position_usd >= 10.0 {
            let order_start = std::time::Instant::now();

            let shares = position_usd / order.price;
            open_positions.insert(
                pos_key,
                (
                    order.price,
                    shares,
                    entry_side.to_string(),
                    model_p,
                    order.timestamp,
                ),
            );

            let order_build_us = order_start.elapsed().as_micros() as u64;
            comp.t2t
                .record_stage(crate::latency::T2TStage::OrderBuild, order_build_us);

            // REALISTIC WIRE SEND: Estimate for WebSocket order submission
            // Real Polymarket WebSocket order: ~10-50ms round trip
            // We estimate 25ms for a well-connected non-colocated setup
            let wire_send_estimate_us = 25_000; // 25ms
            comp.t2t
                .record_stage(crate::latency::T2TStage::WireSend, wire_send_estimate_us);

            comp.throughput
                .orders_sent
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            comp.throughput
                .orders_filled
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

            // Total T2T: network + local processing + wire send
            let local_processing_us = order_received_ns.elapsed().as_micros() as u64;
            let total_t2t_us = network_latency_us + local_processing_us + wire_send_estimate_us;
            comp.t2t
                .record_stage(crate::latency::T2TStage::Total, total_t2t_us);

            tracing::debug!(
                "[PAPER-WS] ENTRY: {} {} @ {:.4} (model: {:.4}, edge: {:.2}%, size: ${:.2}, t2t: {:.1}ms [net: {:.1}ms, proc: {}us, wire: {:.1}ms])",
                entry_side, order.market_slug, order.price, model_p, edge * 100.0, position_usd,
                total_t2t_us as f64 / 1000.0,
                network_latency_us as f64 / 1000.0,
                local_processing_us,
                wire_send_estimate_us as f64 / 1000.0
            );
        }
    }
}

// Keep the REST-based fetch for fallback/comparison
async fn _paper_trading_loop_rest(
    asset: String,
    _storage: std::sync::Arc<crate::signals::db_storage::DbSignalStorage>,
    binance_feed: std::sync::Arc<crate::scrapers::binance_price_feed::BinancePriceFeed>,
) {
    use crate::vault::updown15m::{parse_updown_15m_slug, shrink_to_half, UpDown15mMarket};
    use crate::vault::{estimate_p_up_rnjd, RnjdParams};
    use std::collections::{HashMap, HashSet};
    use tokio::time::{sleep, Duration};

    tracing::info!(
        "[PAPER-REST] Starting REST-based paper trading for asset: {}",
        asset
    );

    let dome_api_key = std::env::var("DOME_API_KEY").unwrap_or_default();
    if dome_api_key.is_empty() {
        tracing::error!("[PAPER] DOME_API_KEY not set");
        return;
    }

    let http_client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();

    let mut open_positions: HashMap<String, (f64, f64, String, f64, i64)> = HashMap::new();
    let mut processed_orders: HashSet<String> = HashSet::new();
    let shrink = 0.35;

    let asset_prefix = match asset.as_str() {
        "btc" => "btc-updown-15m",
        "eth" => "eth-updown-15m",
        "sol" => "sol-updown-15m",
        "xrp" => "xrp-updown-15m",
        _ => "-updown-15m",
    };

    while PAPER_TRADING_RUNNING.load(Ordering::SeqCst) {
        sleep(Duration::from_secs(2)).await;

        if !PAPER_TRADING_RUNNING.load(Ordering::SeqCst) {
            break;
        }

        let now = chrono::Utc::now().timestamp();

        let orders = match fetch_dome_orders(&http_client, &dome_api_key, 50).await {
            Ok(o) => o,
            Err(e) => {
                tracing::warn!("[PAPER] Failed to fetch Dome orders: {}", e);
                continue;
            }
        };

        // Filter to relevant 15m markets and dedupe
        let orders: Vec<_> = orders
            .into_iter()
            .filter(|o| {
                o.market_slug.contains(asset_prefix)
                    || (asset == "all" && o.market_slug.contains("-updown-15m"))
            })
            .filter(|o| !processed_orders.contains(&o.order_hash))
            .collect();

        // Mark as processed
        for o in &orders {
            processed_orders.insert(o.order_hash.clone());
            // Keep set bounded
            if processed_orders.len() > 10000 {
                processed_orders.clear();
            }
        }

        // Get config
        let config = {
            let engine = PAPER_TRADING_STATE.lock();
            engine.config.clone()
        };

        // BINARY OUTCOME: Close positions when their market has EXPIRED
        let positions_to_close: Vec<_> = open_positions
            .iter()
            .filter_map(|(slug, (entry_price, shares, side, _model_p, entry_ts))| {
                // Parse the market slug to get end timestamp
                if let Some(market) = parse_updown_15m_slug(slug) {
                    // Market has expired - time to settle
                    if now >= market.end_ts {
                        Some((
                            slug.clone(),
                            *entry_price,
                            *shares,
                            side.clone(),
                            market,
                            *entry_ts,
                        ))
                    } else {
                        None
                    }
                } else {
                    // Can't parse slug - close after 15 minutes as fallback
                    if now - entry_ts >= 900 {
                        Some((
                            slug.clone(),
                            *entry_price,
                            *shares,
                            side.clone(),
                            UpDown15mMarket {
                                asset: crate::vault::UpDownAsset::Btc,
                                start_ts: *entry_ts,
                                end_ts: *entry_ts + 900,
                            },
                            *entry_ts,
                        ))
                    } else {
                        None
                    }
                }
            })
            .collect();

        for (slug, entry_price, shares, side, market, _entry_ts) in positions_to_close {
            open_positions.remove(&slug);

            // BINARY OUTCOME: Determine actual result from Binance price
            let binance_symbol = market.asset.binance_symbol();
            let start_price = binance_feed
                .mid_near(binance_symbol, market.start_ts, 120)
                .map(|p| p.mid);
            let end_price = binance_feed
                .mid_near(binance_symbol, market.end_ts, 120)
                .or_else(|| binance_feed.latest_mid(binance_symbol))
                .map(|p| p.mid);

            let (actual_outcome, pnl) = match (start_price, end_price) {
                (Some(p_start), Some(p_end)) => {
                    // Actual outcome: Up if price increased, Down if decreased
                    let actual_up = p_end >= p_start;
                    let we_bet_up = side == "BUY_UP";
                    let we_won = actual_up == we_bet_up;

                    // BINARY PNL (after fees):
                    // Fee ~1.5% on entry only (no exit fee at settlement)
                    let price_mid_distance = (entry_price - 0.5).abs();
                    let entry_fee_rate = 0.015 * (1.0 - price_mid_distance);
                    let notional = shares * entry_price;
                    let fees = notional * entry_fee_rate;

                    // Win: we paid entry_price per share, get $1.00 per share
                    // Lose: we paid entry_price per share, get $0.00
                    let gross_pnl = if we_won {
                        shares * (1.0 - entry_price)
                    } else {
                        -(shares * entry_price)
                    };
                    let pnl = gross_pnl - fees;

                    let outcome_str = if actual_up { "Up" } else { "Down" };
                    (outcome_str.to_string(), pnl)
                }
                _ => {
                    tracing::warn!(
                        "[PAPER] Could not determine outcome for {} - no price data",
                        slug
                    );
                    let outcome_str = if side == "BUY_UP" { "Up" } else { "Down" };
                    (outcome_str.to_string(), 0.0)
                }
            };

            let exit_price = if pnl > 0.0 { 1.0 } else { 0.0 };

            tracing::info!(
                "[PAPER] SETTLED: {} {} @ {:.4} -> {} (PnL: ${:.2})",
                side,
                slug,
                entry_price,
                actual_outcome,
                pnl
            );

            let trade = PaperTradeRecord {
                ts: now,
                market_slug: slug,
                outcome: actual_outcome,
                side: format!("{}_SETTLED", side),
                entry_price,
                exit_price,
                shares,
                pnl,
                edge: (1.0 - entry_price).abs(),
            };

            let mut engine = PAPER_TRADING_STATE.lock();
            engine.record_trade(trade);
        }

        // Process new signals
        let comp = crate::latency::global_comprehensive();
        let our_receive_time = chrono::Utc::now().timestamp_millis();

        for order in &orders {
            let signal_start = std::time::Instant::now();

            // REAL LATENCY #1: Network latency from order timestamp to our receipt
            // This measures: Polymarket -> Dome API -> Our REST poll -> Our processing
            // In a real HFT system this would be WebSocket with sub-ms delivery
            let order_age_ms = (our_receive_time - (order.timestamp * 1000)).max(0);
            let network_latency_us = (order_age_ms * 1000) as u64; // Convert ms to us

            // Record network/ingestion latency (this is the REAL bottleneck in our system)
            // WARNING: This will be ~2000ms+ because we poll every 2 seconds!
            comp.t2t
                .record_stage(crate::latency::T2TStage::MdReceive, network_latency_us);

            {
                let mut engine = PAPER_TRADING_STATE.lock();
                engine.signals_seen += 1;
            }

            // Record signal received
            comp.throughput
                .strategy_evals
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

            // Parse market slug to get asset and timing
            let market = match parse_updown_15m_slug(&order.market_slug) {
                Some(m) => m,
                None => continue,
            };

            // Skip expired markets
            if now >= market.end_ts {
                continue;
            }

            let t_rem = (market.end_ts - now) as f64;
            if t_rem < 30.0 {
                continue; // Too close to expiry
            }

            // Get Binance prices for latency arbitrage model
            let binance_symbol = market.asset.binance_symbol();
            let p_start = match binance_feed.mid_near(binance_symbol, market.start_ts, 60) {
                Some(p) => p.mid,
                None => continue,
            };
            let p_now = match binance_feed.latest_mid(binance_symbol) {
                Some(p) => p.mid,
                None => continue,
            };
            let sigma = match binance_feed.sigma_per_sqrt_s(binance_symbol) {
                Some(s) if s > 0.0 => s,
                _ => continue,
            };

            // =========================================================================
            // RN-JD MODEL: Risk-Neutral Jump-Diffusion probability estimation
            // Based on: "Toward Black-Scholes for Prediction Markets" (arXiv:2510.15205)
            // =========================================================================

            // Convert price volatility to annualized form for RN-JD
            let sigma_annual = sigma * (365.25 * 24.0 * 3600.0_f64).sqrt();

            // Use market price as prior
            let market_p = order.price.clamp(0.01, 0.99);

            // RN-JD parameters - conservative settings
            let rnjd_params = RnjdParams {
                sigma_b: 2.0, // Moderate belief volatility
                lambda: 0.0,  // No jumps in base case
                mu_j: 0.0,
                sigma_j: 0.1,
            };

            // Estimate p_up using RN-JD model
            let rnjd_estimate = match estimate_p_up_rnjd(
                p_start,
                p_now,
                market_p,
                sigma_annual,
                t_rem,
                &rnjd_params,
            ) {
                Some(est) => est,
                None => continue,
            };

            let model_p_up = shrink_to_half(rnjd_estimate.p_up, shrink);
            let model_p_down = 1.0 - model_p_up;

            // Record signal compute latency
            let signal_compute_us = signal_start.elapsed().as_micros() as u64;
            comp.t2t
                .record_stage(crate::latency::T2TStage::SignalCompute, signal_compute_us);
            comp.throughput
                .signals_generated
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

            // Determine which outcome this order is for and compute edge
            let (model_p, is_up) = if order.outcome.to_lowercase() == "up" {
                (model_p_up, true)
            } else {
                (model_p_down, false)
            };

            // LATENCY ARBITRAGE EDGE: model price vs market price
            let edge = model_p - order.price;
            let edge_abs = edge.abs();

            // Track opportunity
            if edge_abs >= config.min_edge {
                let mut engine = PAPER_TRADING_STATE.lock();
                engine.opportunities += 1;
            }

            // Check for exit opportunity on existing position
            let pos_key = order.market_slug.clone();
            if let Some((entry_price, shares, side, _, entry_ts)) = open_positions.remove(&pos_key)
            {
                // CRITICAL: Only consider exit if the incoming order matches our position's outcome
                let order_is_up = is_up;
                let position_is_up = side == "BUY_UP";

                if order_is_up != position_is_up {
                    // Wrong outcome - put position back and skip
                    open_positions.insert(
                        pos_key.clone(),
                        (entry_price, shares, side, model_p, entry_ts),
                    );
                    continue;
                }

                // Exit if edge reversed or market moved in our favor
                let price_move = order.price - entry_price;

                // Fee calculation: ~1.5% per side, so ~3% round-trip
                let price_mid_distance = (entry_price - 0.5).abs();
                let fee_rate = 0.03 * (1.0 - price_mid_distance);
                let min_profitable_move = fee_rate;

                let favorable = (side == "BUY_UP" && price_move > min_profitable_move)
                    || (side == "BUY_DOWN" && price_move < -min_profitable_move);
                // Edge reversal: exit if model now says we're on the wrong side
                let reversed = (side == "BUY_UP" && edge < -config.min_edge)
                    || (side == "BUY_DOWN" && edge < -config.min_edge);

                if favorable || reversed || (now - entry_ts) >= 180 {
                    // Early exit - PnL after fees
                    let exit_price = order.price;
                    let gross_pnl = shares * (exit_price - entry_price);
                    let notional = shares * entry_price;
                    let fees = notional * fee_rate;
                    let pnl = gross_pnl - fees;

                    let trade = PaperTradeRecord {
                        ts: order.timestamp,
                        market_slug: order.market_slug.clone(),
                        outcome: if side == "BUY_UP" {
                            "Up".to_string()
                        } else {
                            "Down".to_string()
                        },
                        side: format!("{}_EXIT", side),
                        entry_price,
                        exit_price,
                        shares,
                        pnl,
                        edge: edge_abs,
                    };

                    let mut engine = PAPER_TRADING_STATE.lock();
                    engine.record_trade(trade);
                } else {
                    // Keep position open
                    open_positions.insert(
                        pos_key.clone(),
                        (entry_price, shares, side, model_p, entry_ts),
                    );
                }
                continue;
            }

            // Entry: if significant edge and no position
            if edge_abs >= config.min_edge && !open_positions.contains_key(&pos_key) {
                let risk_start = std::time::Instant::now();

                // Determine side: buy underpriced
                let (entry_side, target_outcome) = if edge > 0.0 {
                    // Model says higher prob than market -> buy
                    if is_up {
                        ("BUY_UP", "Up")
                    } else {
                        ("BUY_DOWN", "Down")
                    }
                } else {
                    // Model says lower prob -> skip (would need to sell, which is harder)
                    continue;
                };

                // Kelly sizing based on edge (risk check)
                let kelly_edge = edge_abs;
                let kelly_f = config.kelly_fraction * kelly_edge * 10.0; // Scale edge to fraction
                let position_pct = kelly_f.min(config.max_position_pct);
                let position_usd = config.bankroll * position_pct;

                // Record risk check latency
                let risk_check_us = risk_start.elapsed().as_micros() as u64;
                comp.t2t
                    .record_stage(crate::latency::T2TStage::RiskCheck, risk_check_us);
                comp.throughput
                    .risk_checks
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

                if position_usd >= 10.0 {
                    let order_start = std::time::Instant::now();

                    let shares = position_usd / order.price;
                    open_positions.insert(
                        pos_key,
                        (
                            order.price,
                            shares,
                            entry_side.to_string(),
                            model_p,
                            order.timestamp,
                        ),
                    );

                    // Record order build time
                    let order_build_us = order_start.elapsed().as_micros() as u64;
                    comp.t2t
                        .record_stage(crate::latency::T2TStage::OrderBuild, order_build_us);

                    // REAL LATENCY: Simulate actual wire send time
                    // In production, this would be the time to:
                    // 1. Serialize the order (JSON/binary)
                    // 2. Send HTTP request to Polymarket/Dome
                    // 3. Wait for ACK
                    // For paper trading, we estimate based on typical REST API latency (~50-200ms)
                    // Real HFT would use FIX protocol or WebSocket (~1-10ms)
                    let wire_send_estimate_us = 100_000; // 100ms - realistic REST API round trip
                    comp.t2t
                        .record_stage(crate::latency::T2TStage::WireSend, wire_send_estimate_us);

                    comp.throughput
                        .orders_sent
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    comp.throughput
                        .orders_filled
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed); // Paper = instant fill

                    // TOTAL T2T: Network receive + signal compute + risk + build + wire
                    // This is the REAL end-to-end latency from market event to order on wire
                    let local_processing_us = signal_start.elapsed().as_micros() as u64;
                    let total_t2t_us =
                        network_latency_us + local_processing_us + wire_send_estimate_us;
                    comp.t2t
                        .record_stage(crate::latency::T2TStage::Total, total_t2t_us);

                    tracing::debug!(
                        "[PAPER] ENTRY: {} {} @ {:.4} (model: {:.4}, edge: {:.2}%, size: ${:.2}, t2t: {:.1}ms [net: {:.1}ms, proc: {}us, wire: {:.1}ms])",
                        entry_side, order.market_slug, order.price, model_p, edge * 100.0, position_usd,
                        total_t2t_us as f64 / 1000.0,
                        network_latency_us as f64 / 1000.0,
                        local_processing_us,
                        wire_send_estimate_us as f64 / 1000.0
                    );
                }
            }
        }
    }

    tracing::info!("[PAPER] Paper trading loop stopped");
}

// Dome API order structure for paper trading
#[derive(Debug, Clone)]
struct DomeApiOrder {
    order_hash: String,
    timestamp: i64,
    market_slug: String,
    outcome: String,
    price: f64,
}

// Fetch recent orders from Dome REST API
async fn fetch_dome_orders(
    client: &reqwest::Client,
    api_key: &str,
    limit: u32,
) -> Result<Vec<DomeApiOrder>, String> {
    let url = format!(
        "https://api.domeapi.io/v1/polymarket/orders?limit={}",
        limit
    );

    let resp = client
        .get(&url)
        .header("Authorization", format!("Bearer {}", api_key))
        .send()
        .await
        .map_err(|e| format!("Request failed: {}", e))?;

    if !resp.status().is_success() {
        return Err(format!("API error: {}", resp.status()));
    }

    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("JSON parse failed: {}", e))?;

    let orders_arr = body
        .get("orders")
        .and_then(|v| v.as_array())
        .ok_or("Missing orders array")?;

    let mut orders = Vec::new();
    for o in orders_arr {
        let order_hash = o
            .get("order_hash")
            .or_else(|| o.get("id"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let timestamp = o.get("timestamp").and_then(|v| v.as_i64()).unwrap_or(0);

        let market_slug = o
            .get("market_slug")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        // Try multiple field names for outcome
        let outcome = o
            .get("token_label")
            .or_else(|| o.get("outcome"))
            .or_else(|| o.get("side_label"))
            .and_then(|v| v.as_str())
            .unwrap_or("Up")
            .to_string();

        let price = o.get("price").and_then(|v| v.as_f64()).unwrap_or(0.5);

        if !market_slug.is_empty() && timestamp > 0 {
            orders.push(DomeApiOrder {
                order_hash,
                timestamp,
                market_slug,
                outcome,
                price,
            });
        }
    }

    Ok(orders)
}

// =============================================================================
// RN-JD Backtest & A/B Test API
// =============================================================================

#[derive(Debug, Deserialize)]
pub struct BacktestRecordsQuery {
    pub limit: Option<usize>,
    pub resolved_only: Option<bool>,
}

#[derive(Debug, Serialize)]
pub struct BacktestRecordsResponse {
    pub fetched_at: i64,
    pub total_records: usize,
    pub records: Vec<crate::vault::BacktestRecord>,
}

/// GET /api/backtest/records - Get recent backtest records
pub async fn get_backtest_records(
    Query(params): Query<BacktestRecordsQuery>,
    AxumState(state): AxumState<AppState>,
) -> Json<BacktestRecordsResponse> {
    let now = chrono::Utc::now().timestamp();
    let limit = params.limit.unwrap_or(100).min(1000);
    let resolved_only = params.resolved_only.unwrap_or(false);

    let collector = state.backtest_collector.read();
    let total_records = collector.len();

    let records: Vec<_> = collector
        .records()
        .iter()
        .rev()
        .filter(|r| !resolved_only || r.resolved)
        .take(limit)
        .cloned()
        .collect();

    Json(BacktestRecordsResponse {
        fetched_at: now,
        total_records,
        records,
    })
}

/// GET /api/backtest/stats - Get backtest performance metrics
pub async fn get_backtest_stats(
    AxumState(state): AxumState<AppState>,
) -> Json<crate::vault::BacktestMetrics> {
    let collector = state.backtest_collector.read();
    Json(collector.calculate_metrics())
}

/// GET /api/ab-test/summary - Get A/B test summary statistics
pub async fn get_ab_test_summary(
    AxumState(state): AxumState<AppState>,
) -> Json<crate::vault::ABTestSummary> {
    let tracker = state.ab_test_tracker.read();
    Json(tracker.summary())
}

// =============================================================================
// Oracle Comparison API (Chainlink vs Binance)
// =============================================================================

#[derive(Debug, Deserialize)]
pub struct OracleComparisonQuery {
    /// Asset to query (btc, eth, sol, xrp). If omitted, returns all assets.
    pub asset: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct OracleComparisonResponse {
    pub fetched_at: i64,
    /// Stats per asset
    pub assets: HashMap<String, AssetOracleComparisonData>,
    /// Aggregated rolling stats (last 100 windows) across all assets
    pub total_rolling_stats: crate::scrapers::oracle_comparison::AgreementStats,
    /// Aggregated all-time stats across all assets
    pub total_all_time_stats: crate::scrapers::oracle_comparison::AgreementStats,
}

#[derive(Debug, Serialize)]
pub struct AssetOracleComparisonData {
    pub asset: String,
    /// Rolling window resolutions (last 100)
    pub rolling_window: Vec<crate::scrapers::oracle_comparison::WindowResolution>,
    /// Real-time price ticks (last ~5 minutes)
    pub price_ticks: Vec<crate::scrapers::oracle_comparison::PriceTick>,
    /// Current divergence in basis points
    pub current_divergence_bps: Option<f64>,
    /// Current divergence in basis points (lag-adjusted: Binance sampled near Chainlink timestamp)
    pub current_divergence_aligned_bps: Option<f64>,
    /// Current Chainlink staleness (ms)
    pub current_chainlink_staleness_ms: Option<u64>,
    /// Current Binance staleness (ms)
    pub current_binance_staleness_ms: Option<u64>,
    /// Rolling window stats (last 100 windows)
    pub rolling_stats: crate::scrapers::oracle_comparison::AgreementStats,
    /// All-time historical stats
    pub all_time_stats: crate::scrapers::oracle_comparison::AgreementStats,
    /// Average Chainlink update interval in microseconds
    pub avg_chainlink_interval_us: Option<u64>,
    /// Average Binance update interval in microseconds
    pub avg_binance_interval_us: Option<u64>,
}

/// GET /api/oracle/comparison - Oracle price comparison (Chainlink vs Binance)
pub async fn get_oracle_comparison(
    Query(params): Query<OracleComparisonQuery>,
    AxumState(state): AxumState<AppState>,
) -> Json<OracleComparisonResponse> {
    use crate::scrapers::oracle_comparison::{global_oracle_tracker, SUPPORTED_ASSETS};

    let now = chrono::Utc::now().timestamp();
    let tracker = global_oracle_tracker();

    let assets_to_query: Vec<&str> = match &params.asset {
        Some(a) => {
            let upper = a.to_uppercase();
            if SUPPORTED_ASSETS.contains(&upper.as_str()) {
                vec![Box::leak(upper.into_boxed_str()) as &str]
            } else {
                SUPPORTED_ASSETS.to_vec()
            }
        }
        None => SUPPORTED_ASSETS.to_vec(),
    };

    let mut assets = HashMap::new();
    for asset in &assets_to_query {
        assets.insert(
            asset.to_string(),
            AssetOracleComparisonData {
                asset: asset.to_string(),
                rolling_window: tracker.get_rolling_window(asset),
                price_ticks: tracker.get_price_ticks(asset),
                current_divergence_bps: tracker.get_current_divergence(asset),
                current_divergence_aligned_bps: tracker.get_current_divergence_aligned(asset),
                current_chainlink_staleness_ms: tracker.get_current_chainlink_staleness_ms(asset),
                current_binance_staleness_ms: tracker.get_current_binance_staleness_ms(asset),
                rolling_stats: tracker.get_rolling_stats(asset),
                all_time_stats: tracker.get_all_time_stats(asset),
                avg_chainlink_interval_us: tracker.get_avg_chainlink_interval_us(asset),
                avg_binance_interval_us: tracker.get_avg_binance_interval_us(asset),
            },
        );
    }

    let combined = tracker.get_combined_stats();

    Json(OracleComparisonResponse {
        fetched_at: now,
        assets,
        total_rolling_stats: combined.total_rolling_stats,
        total_all_time_stats: combined.total_all_time_stats,
    })
}

// =============================================================================
// Up/Down 15m History API (persistent)
// =============================================================================

#[derive(Debug, Deserialize)]
pub struct UpDown15mHistoryQuery {
    /// Asset filter (btc|eth|sol|xrp|all). If omitted, returns all assets.
    pub asset: Option<String>,
    /// Max number of windows to return.
    pub limit: Option<usize>,
    /// Pagination cursor (exclusive): return windows with end_ts < before_end_ts.
    pub before_end_ts: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct UpDown15mHistoryRow {
    pub market_slug: String,
    pub asset: String,
    pub window_start_ts: i64,
    pub window_end_ts: i64,
    pub start_price: Option<f64>,
    pub end_price: Option<f64>,
    pub outcome: Option<String>,
    pub source: Option<String>,
    pub chainlink_start: Option<f64>,
    pub chainlink_end: Option<f64>,
    pub binance_start: Option<f64>,
    pub binance_end: Option<f64>,
    pub recorded_at: i64,
}

#[derive(Debug, Serialize)]
pub struct UpDown15mHistoryResponse {
    pub fetched_at: i64,
    pub windows: Vec<UpDown15mHistoryRow>,
}

/// GET /api/updown15m/history - Persisted 15m Up/Down start/end prices + outcomes
pub async fn get_updown_15m_history(
    Query(params): Query<UpDown15mHistoryQuery>,
    AxumState(state): AxumState<AppState>,
) -> Result<Json<UpDown15mHistoryResponse>, StatusCode> {
    let now = chrono::Utc::now().timestamp();

    let limit = params.limit.unwrap_or(500).clamp(1, 10_000);
    let asset_filter = params
        .asset
        .as_deref()
        .map(|a| a.trim())
        .filter(|a| !a.is_empty())
        .filter(|a| !a.eq_ignore_ascii_case("all"));

    let windows = state
        .signal_storage
        .get_updown_15m_windows(asset_filter, limit, params.before_end_ts)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let rows = windows
        .into_iter()
        .map(|w| {
            let market_slug = format!(
                "{}-updown-15m-{}",
                w.asset.to_ascii_lowercase(),
                w.window_start_ts
            );

            let (start_price, end_price, outcome_bool, source) =
                if let (Some(s), Some(e), Some(o)) =
                    (w.chainlink_start, w.chainlink_end, w.chainlink_outcome)
                {
                    (Some(s), Some(e), Some(o), Some("chainlink".to_string()))
                } else if let (Some(s), Some(e), Some(o)) =
                    (w.binance_start, w.binance_end, w.binance_outcome)
                {
                    (Some(s), Some(e), Some(o), Some("binance".to_string()))
                } else {
                    (None, None, None, None)
                };

            let outcome = outcome_bool.map(|b| (if b { "Up" } else { "Down" }).to_string());

            UpDown15mHistoryRow {
                market_slug,
                asset: w.asset,
                window_start_ts: w.window_start_ts,
                window_end_ts: w.window_end_ts,
                start_price,
                end_price,
                outcome,
                source,
                chainlink_start: w.chainlink_start,
                chainlink_end: w.chainlink_end,
                binance_start: w.binance_start,
                binance_end: w.binance_end,
                recorded_at: w.recorded_at,
            }
        })
        .collect::<Vec<_>>();

    Ok(Json(UpDown15mHistoryResponse {
        fetched_at: now,
        windows: rows,
    }))
}

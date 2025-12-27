//! Simplified API routes that just work
//! No complexity, just results
//!
//! Optimizations:
//! - Minimal allocations in hot paths
//! - Direct database access without intermediate layers

use crate::{
    models::{MarketSignal, SignalContext, SignalContextRecord, SignalType},
    scrapers::polymarket::OrderBook,
    signals::wallet_analytics::{
        get_or_compute_wallet_analytics, FrictionMode, WalletAnalytics, WalletAnalyticsParams,
    },
    AppState,
};
use axum::{
    extract::{Json as AxumJson, Query, State as AxumState},
    http::StatusCode,
    response::Json,
};
use chrono::Utc;
use serde::Deserializer;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{env, time::Duration};
use tracing::warn;
use uuid::Uuid;

#[derive(Debug, Deserialize)]
pub struct SignalQuery {
    pub limit: Option<usize>,
    pub before: Option<String>,
    pub before_id: Option<String>,
    /// If true, exclude signals from up/down markets (btc-updown, eth-updown, etc.)
    pub exclude_updown: Option<bool>,
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
pub struct WalletAnalyticsQuery {
    pub wallet_address: String,
    pub force: Option<bool>,
    /// Friction mode for copy trading simulation: "optimistic", "base", or "pessimistic"
    pub friction_mode: Option<String>,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GammaMarketLookup {
    pub slug: String,
    #[serde(rename = "conditionId")]
    pub condition_id: String,
    #[serde(deserialize_with = "de_string_vec")]
    pub outcomes: Vec<String>,
    #[serde(rename = "clobTokenIds", deserialize_with = "de_string_vec")]
    pub clob_token_ids: Vec<String>,
}

fn de_string_vec<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let v = Value::deserialize(deserializer)?;
    match v {
        Value::Array(arr) => Ok(arr
            .into_iter()
            .filter_map(|x| match x {
                Value::String(s) => Some(s),
                Value::Number(n) => Some(n.to_string()),
                _ => None,
            })
            .collect()),
        Value::String(s) => {
            // Some Gamma responses return JSON arrays as a string (e.g. "[\"Yes\",\"No\"]").
            serde_json::from_str::<Vec<String>>(&s).map_err(serde::de::Error::custom)
        }
        _ => Ok(Vec::new()),
    }
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

    let mut analytics_params = WalletAnalyticsParams::default();
    analytics_params.friction_mode = friction_mode;

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

/// Get current Polymarket orderbook snapshot + derived depth metrics.
pub async fn get_market_snapshot(
    Query(params): Query<MarketSnapshotQuery>,
    AxumState(state): AxumState<AppState>,
) -> Result<Json<MarketSnapshotResponse>, StatusCode> {
    let levels = params.levels.unwrap_or(10).clamp(1, 50);

    let (cache_key, clob_token_id) = if let (Some(slug), Some(outcome)) =
        (params.market_slug.as_ref(), params.outcome.as_ref())
    {
        let clob = resolve_clob_token_id_by_slug(&state, slug, outcome)
            .await
            .map_err(|e| {
                warn!(
                    "gamma lookup failed for slug={} outcome={}: {}",
                    slug, outcome, e
                );
                StatusCode::BAD_GATEWAY
            })?;
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

async fn resolve_clob_token_id_by_slug(
    state: &AppState,
    market_slug: &str,
    outcome: &str,
) -> Result<String, reqwest::Error> {
    let now = Utc::now().timestamp();
    let ttl_seconds = 24 * 3600;
    let cache_key = format!("gamma_market_lookup_v1:{}", market_slug.to_lowercase());

    if let Ok(Some((cache_json, fetched_at))) = state.signal_storage.get_cache(&cache_key) {
        if now - fetched_at <= ttl_seconds {
            if let Ok(m) = serde_json::from_str::<GammaMarketLookup>(&cache_json) {
                if let Some(i) = m
                    .outcomes
                    .iter()
                    .position(|o| o.eq_ignore_ascii_case(outcome))
                {
                    return Ok(m.clob_token_ids.get(i).cloned().unwrap_or_default());
                }
            }
        }
    }

    let markets = state
        .http_client
        .get("https://gamma-api.polymarket.com/markets")
        .timeout(Duration::from_secs(1))
        .header(reqwest::header::USER_AGENT, "BetterBot/1.0")
        .query(&[("slug", market_slug), ("limit", "1")])
        .send()
        .await?
        .error_for_status()?
        .json::<Vec<GammaMarketLookup>>()
        .await?;

    let Some(m) = markets.into_iter().next() else {
        return Ok(String::new());
    };

    if let Ok(json) = serde_json::to_string(&m) {
        let _ = state.signal_storage.upsert_cache(&cache_key, &json, now);
    }

    let idx = m
        .outcomes
        .iter()
        .position(|o| o.eq_ignore_ascii_case(outcome));
    let Some(i) = idx else {
        return Ok(String::new());
    };

    Ok(m.clob_token_ids.get(i).cloned().unwrap_or_default())
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

/// Get signals - simplified version that actually works
pub async fn get_signals_simple(
    Query(params): Query<SignalQuery>,
    AxumState(state): AxumState<AppState>,
) -> Json<SignalResponse> {
    let requested_limit = params.limit.unwrap_or(100);
    let exclude_updown = params.exclude_updown.unwrap_or(false);

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
            let ctx = contexts.get(&signal.id);
            SignalWithContext {
                signal,
                context: ctx.map(|c| c.context.clone()),
                context_status: ctx.map(|c| c.status.clone()),
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

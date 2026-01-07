use crate::{
    scrapers::dome_rest::{ActivityItem, DomeOrder, DomeRestClient, WalletPnlGranularity},
    signals::db_storage::DbSignalStorage,
};
use anyhow::{Context, Result};
use futures_util::stream::{FuturesUnordered, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;
use tracing::warn;

/// Friction mode for copy trading simulation.
/// Models realistic execution costs for a follower strategy.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum FrictionMode {
    /// Optimistic: 0.35% per trade (0.25% spread + 0.10% slippage)
    Optimistic,
    /// Base case: 1.00% per trade (0.75% spread + 0.25% slippage)
    #[default]
    Base,
    /// Pessimistic: 2.00% per trade (1.50% spread + 0.50% slippage)
    Pessimistic,
}

/// Copy-curve model.
///
/// - `scaled`: fast, stable, and explainable (scaled wallet pnl curve net of execution costs)
/// - `mtm`: trade replay + daily mark-to-market using price history (more realistic, heavier)
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum CopyCurveModel {
    #[default]
    Scaled,
    Mtm,
}

impl CopyCurveModel {
    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "mtm" => Self::Mtm,
            _ => Self::Scaled,
        }
    }
}

impl FrictionMode {
    /// Total friction cost as a percentage per trade.
    pub fn total_friction_pct(&self) -> f64 {
        match self {
            Self::Optimistic => 0.35,
            Self::Base => 1.00,
            Self::Pessimistic => 2.00,
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "optimistic" => Self::Optimistic,
            "pessimistic" => Self::Pessimistic,
            _ => Self::Base,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EquityPoint {
    pub timestamp: i64,
    pub value: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WalletAnalytics {
    pub wallet_address: String,
    pub updated_at: i64,

    pub lookback_days: u32,
    pub fixed_buy_notional_usd: f64,

    pub wallet_realized_curve: Vec<EquityPoint>,
    pub copy_trade_curve: Vec<EquityPoint>,

    // Wallet stats (derived from realized PnL curve + locally persisted order flow for sizing)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wallet_total_pnl: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wallet_roe_pct: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wallet_roe_denom_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wallet_win_rate: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wallet_profit_factor: Option<f64>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub copy_total_pnl: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub copy_roe_pct: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub copy_roe_denom_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub copy_win_rate: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub copy_profit_factor: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub copy_sharpe_7d: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub copy_sharpe_14d: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub copy_sharpe_30d: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub copy_sharpe_90d: Option<f64>,

    // Friction modeling for realistic copy trading simulation
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub copy_friction_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub copy_friction_pct_per_trade: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub copy_total_friction_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub copy_trade_count: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct WalletAnalyticsParams {
    pub lookback_days: u32,
    pub warmup_days: u32,
    pub fixed_buy_notional_usd: f64,
    pub max_orders: usize,
    pub friction_mode: FrictionMode,
    pub copy_model: CopyCurveModel,
}

impl Default for WalletAnalyticsParams {
    fn default() -> Self {
        Self {
            lookback_days: 90,
            warmup_days: 30,
            fixed_buy_notional_usd: 1.0,
            // Keep this modest so /api/wallet/analytics stays responsive even for very active wallets.
            max_orders: 2_000,
            friction_mode: FrictionMode::Base,
            copy_model: CopyCurveModel::Scaled,
        }
    }
}

// Serve cached analytics very aggressively for UI snappiness.
// A background refresher and on-demand SWR updates keep it reasonably fresh.
pub const WALLET_ANALYTICS_CACHE_TTL_SECONDS: i64 = 900;

#[inline]
fn cache_key(wallet: &str, friction_mode: FrictionMode, copy_model: CopyCurveModel) -> String {
    // v5: includes friction mode + copy model in cache key
    let mode_str = match friction_mode {
        FrictionMode::Optimistic => "opt",
        FrictionMode::Base => "base",
        FrictionMode::Pessimistic => "pess",
    };

    let model_str = match copy_model {
        CopyCurveModel::Scaled => "scaled",
        CopyCurveModel::Mtm => "mtm",
    };

    format!(
        "wallet_analytics_v5:{}:{}:{}",
        wallet.to_lowercase(),
        mode_str,
        model_str
    )
}

pub fn wallet_analytics_cache_key(
    wallet: &str,
    friction_mode: FrictionMode,
    copy_model: CopyCurveModel,
) -> String {
    cache_key(wallet, friction_mode, copy_model)
}

pub async fn get_or_compute_wallet_analytics(
    storage: &DbSignalStorage,
    rest: &DomeRestClient,
    wallet: &str,
    force: bool,
    now: i64,
    params: WalletAnalyticsParams,
) -> Result<WalletAnalytics> {
    let wallet_norm = wallet.to_lowercase();
    let key = cache_key(&wallet_norm, params.friction_mode, params.copy_model);

    // If cache exists but is stale, keep it around as a fallback when upstream APIs fail.
    let mut stale_fallback: Option<WalletAnalytics> = None;
    if let Ok(Some((json, fetched_at))) = storage.get_cache(&key) {
        if !force && now - fetched_at <= WALLET_ANALYTICS_CACHE_TTL_SECONDS {
            if let Ok(v) = serde_json::from_str::<WalletAnalytics>(&json) {
                return Ok(v);
            }
        }
        if let Ok(v) = serde_json::from_str::<WalletAnalytics>(&json) {
            stale_fallback = Some(v);
        }
    }

    match compute_wallet_analytics(storage, rest, &wallet_norm, now, &params).await {
        Ok(analytics) => {
            // Avoid writing useless empty blobs; if we have no data at all, keep cache empty so
            // subsequent calls can fill it once WS events arrive.
            let has_any_data = !analytics.copy_trade_curve.is_empty()
                || !analytics.wallet_realized_curve.is_empty();
            if has_any_data {
                let serialized =
                    serde_json::to_string(&analytics).context("serialize wallet analytics")?;
                storage
                    .upsert_cache(&key, &serialized, analytics.updated_at)
                    .context("upsert wallet analytics cache")?;
            }
            Ok(analytics)
        }
        Err(e) => {
            if let Some(stale) = stale_fallback {
                warn!(
                    "wallet analytics compute failed for {} - serving stale cache: {}",
                    wallet_norm, e
                );
                Ok(stale)
            } else {
                Err(e)
            }
        }
    }
}

pub async fn compute_wallet_analytics(
    storage: &DbSignalStorage,
    rest: &DomeRestClient,
    wallet: &str,
    now: i64,
    params: &WalletAnalyticsParams,
) -> Result<WalletAnalytics> {
    let start_time = now - (params.lookback_days as i64 * 24 * 3600);
    let fetch_start_time = start_time - (params.warmup_days as i64 * 24 * 3600);

    // HFT-grade: compute the copy-trade curve from the locally persisted WS event log.
    // Dome REST is best-effort (used only for wallet realized curve).
    let orders = storage
        .get_dome_orders_for_wallet(wallet, fetch_start_time, now, params.max_orders)
        .context("local dome_order_events")?;

    // Pull redemption events so copy curves are not empty for BUY-only wallets.
    // Best-effort: timeboxed and limited.
    let activities: Vec<ActivityItem> = match tokio::time::timeout(
        Duration::from_secs(1),
        rest.get_activity(
            wallet,
            Some(fetch_start_time),
            Some(now),
            None,
            None,
            Some(1000),
            Some(0),
        ),
    )
    .await
    {
        Ok(Ok(resp)) => resp.activities,
        Ok(Err(e)) => {
            warn!("activity fetch failed for {}: {}", wallet, e);
            Vec::new()
        }
        Err(_) => {
            warn!("activity fetch timed out for {}", wallet);
            Vec::new()
        }
    };

    let wallet_realized_curve: Vec<EquityPoint> = match tokio::time::timeout(
        Duration::from_secs(2),
        rest.get_wallet_pnl(
            wallet,
            WalletPnlGranularity::Day,
            Some(start_time),
            Some(now),
        ),
    )
    .await
    {
        Ok(Ok(wallet_pnl)) => wallet_pnl
            .pnl_over_time
            .into_iter()
            .map(|p| EquityPoint {
                timestamp: p.timestamp,
                value: p.pnl_to_date,
            })
            .collect(),
        Ok(Err(e)) => {
            warn!("wallet/pnl failed for {}: {}", wallet, e);
            Vec::new()
        }
        Err(_) => {
            warn!("wallet/pnl timed out for {}", wallet);
            Vec::new()
        }
    };

    // Normalize the wallet curve to start at 0 within the window. This improves UI interpretability
    // and keeps ROE/PF/etc unchanged (since they use deltas).
    let mut wallet_realized_curve = wallet_realized_curve;
    normalize_curve_to_zero(&mut wallet_realized_curve);

    // Determine the copy sizing scale so that an average BUY maps to `fixed_buy_notional_usd`.
    let scale = compute_copy_scale_factor(&orders, start_time, params.fixed_buy_notional_usd);

    // Simulate which fills would actually execute (so SELL friction isn’t overcounted when we don’t
    // have inventory due to truncated warmup), and aggregate notional per day.
    let (daily_trade_notional, trade_count, buy_count) = simulate_copy_trade_notional(
        &orders,
        &activities,
        fetch_start_time,
        start_time,
        now,
        scale,
    );

    let friction_pct = params.friction_mode.total_friction_pct() / 100.0;
    let (daily_friction_costs, total_friction_usd) = trade_notional_to_friction_costs(
        &daily_trade_notional,
        friction_pct,
    );

    // Fast model: scaled wallet curve net of execution costs.
    let scaled_wallet_curve: Vec<EquityPoint> = wallet_realized_curve
        .iter()
        .map(|p| EquityPoint {
            timestamp: p.timestamp,
            value: p.value * scale,
        })
        .collect();
    let scaled_copy_curve = apply_daily_costs_to_curve(&scaled_wallet_curve, &daily_friction_costs);

    let mtm_fallback_needed = params.copy_model == CopyCurveModel::Mtm || scaled_copy_curve.len() < 2;
    let mtm_result = if mtm_fallback_needed {
        compute_copy_trade_curve_mtm(
            storage,
            rest,
            &orders,
            &activities,
            fetch_start_time,
            start_time,
            now,
            scale,
            friction_pct,
        )
        .await
    } else {
        None
    };

    // Select copy curve model.
    let scaled_ok = scaled_copy_curve.len() >= 2;
    let scaled_bundle = (scaled_copy_curve, total_friction_usd, trade_count, buy_count);
    let (mut copy_curve, copy_total_friction_usd, copy_trade_count, copy_buy_count) =
        match params.copy_model {
            CopyCurveModel::Scaled => {
                if scaled_ok {
                    scaled_bundle
                } else if let Some(r) = mtm_result {
                    (r.curve, r.total_friction_usd, r.trade_count, r.buy_count)
                } else {
                    scaled_bundle
                }
            }
            CopyCurveModel::Mtm => match mtm_result {
                Some(r) if r.curve.len() >= 2 => (r.curve, r.total_friction_usd, r.trade_count, r.buy_count),
                _ => scaled_bundle,
            },
        };

    // Ensure curve has at least start/end points for UI rendering
    if copy_curve.is_empty() {
        let start = day_bucket(start_time);
        let end = day_bucket(now);
        copy_curve.push(EquityPoint {
            timestamp: start,
            value: 0.0,
        });
        if end != start {
            copy_curve.push(EquityPoint {
                timestamp: end,
                value: 0.0,
            });
        }
    }

    let wallet_total_pnl = curve_total_pnl(&wallet_realized_curve);
    let copy_total_pnl = curve_total_pnl(&copy_curve);

    let wallet_roe_denom_usd = compute_wallet_roe_denom_usd(&orders, start_time);
    let wallet_roe_pct = compute_roe_pct(wallet_total_pnl, wallet_roe_denom_usd);
    let (wallet_win_rate, wallet_profit_factor) =
        compute_curve_win_rate_profit_factor(&wallet_realized_curve);

    let copy_roe_denom_usd = if copy_buy_count > 0 && params.fixed_buy_notional_usd > 0.0 {
        Some(copy_buy_count as f64 * params.fixed_buy_notional_usd)
    } else {
        None
    };
    let copy_roe_pct = compute_roe_pct(copy_total_pnl, copy_roe_denom_usd);
    let (copy_win_rate, copy_profit_factor) = compute_curve_win_rate_profit_factor(&copy_curve);

    let sharpe_7d = compute_curve_sharpe(&copy_curve, now, 7);
    let sharpe_14d = compute_curve_sharpe(&copy_curve, now, 14);
    let sharpe_30d = compute_curve_sharpe(&copy_curve, now, 30);
    let sharpe_90d = compute_curve_sharpe(&copy_curve, now, 90);

    // Friction stats for the copy trading simulation
    let friction_mode_str = match params.friction_mode {
        FrictionMode::Optimistic => "optimistic",
        FrictionMode::Base => "base",
        FrictionMode::Pessimistic => "pessimistic",
    };

    Ok(WalletAnalytics {
        wallet_address: wallet.to_string(),
        updated_at: now,
        lookback_days: params.lookback_days,
        fixed_buy_notional_usd: params.fixed_buy_notional_usd,
        wallet_realized_curve,
        copy_trade_curve: copy_curve,
        wallet_total_pnl,
        wallet_roe_pct,
        wallet_roe_denom_usd,
        wallet_win_rate,
        wallet_profit_factor,
        copy_total_pnl,
        copy_roe_pct,
        copy_roe_denom_usd,
        copy_win_rate,
        copy_profit_factor,
        copy_sharpe_7d: sharpe_7d,
        copy_sharpe_14d: sharpe_14d,
        copy_sharpe_30d: sharpe_30d,
        copy_sharpe_90d: sharpe_90d,
        copy_friction_mode: Some(friction_mode_str.to_string()),
        copy_friction_pct_per_trade: Some(params.friction_mode.total_friction_pct()),
        copy_total_friction_usd: Some(copy_total_friction_usd),
        copy_trade_count: Some(copy_trade_count),
    })
}

fn curve_total_pnl(curve: &[EquityPoint]) -> Option<f64> {
    if curve.len() < 2 {
        return None;
    }
    Some(curve.last()?.value - curve.first()?.value)
}

fn compute_roe_pct(total_pnl: Option<f64>, denom_usd: Option<f64>) -> Option<f64> {
    let pnl = total_pnl?;
    let denom = denom_usd?;
    if !pnl.is_finite() || !denom.is_finite() || denom <= 0.0 {
        return None;
    }
    Some((pnl / denom) * 100.0)
}

fn compute_wallet_roe_denom_usd(orders: &[DomeOrder], start_time: i64) -> Option<f64> {
    let mut sum = 0.0;
    for o in orders {
        if o.timestamp < start_time {
            continue;
        }
        if o.side.to_uppercase() != "BUY" {
            continue;
        }
        let price = o.price;
        let shares = o.shares_normalized;
        if !price.is_finite() || price <= 0.0 || !shares.is_finite() || shares <= 0.0 {
            continue;
        }
        sum += shares * price;
    }

    if sum > 0.0 {
        Some(sum)
    } else {
        None
    }
}

fn compute_copy_roe_denom_usd(
    orders: &[DomeOrder],
    start_time: i64,
    fixed_buy_notional_usd: f64,
) -> Option<f64> {
    if !fixed_buy_notional_usd.is_finite() || fixed_buy_notional_usd <= 0.0 {
        return None;
    }

    let mut buys = 0u64;
    for o in orders {
        if o.timestamp < start_time {
            continue;
        }
        if o.side.to_uppercase() == "BUY" {
            buys += 1;
        }
    }

    if buys == 0 {
        return None;
    }
    Some(buys as f64 * fixed_buy_notional_usd)
}

fn compute_curve_win_rate_profit_factor(curve: &[EquityPoint]) -> (Option<f64>, Option<f64>) {
    if curve.len() < 2 {
        return (None, None);
    }

    let mut wins = 0u64;
    let mut losses = 0u64;
    let mut gross_profit = 0.0;
    let mut gross_loss = 0.0;

    let mut prev = curve[0].value;
    for p in curve.iter().skip(1) {
        let cur = p.value;
        if !cur.is_finite() || !prev.is_finite() {
            prev = cur;
            continue;
        }
        let d = cur - prev;
        prev = cur;

        if d.abs() < 1e-9 {
            continue;
        }
        if d > 0.0 {
            wins += 1;
            gross_profit += d;
        } else {
            losses += 1;
            gross_loss += -d;
        }
    }

    let total = wins + losses;
    let win_rate = if total > 0 {
        Some((wins as f64) / (total as f64))
    } else {
        None
    };

    // Avoid infinities in JSON; cap instead.
    let profit_factor = if gross_profit > 0.0 && gross_loss <= 0.0 {
        Some(999.0)
    } else if gross_profit > 0.0 && gross_loss > 0.0 {
        Some(gross_profit / gross_loss)
    } else {
        None
    };

    (win_rate, profit_factor)
}

#[derive(Debug, Clone, Default)]
struct Position {
    shares: f64,
    cost_usd: f64,
    token_label: Option<String>, // "Yes", "No", "Up", "Down" - which side trader bet on
    last_price: f64,             // Last known price for mark-to-market
}

#[derive(Debug, Clone, Default)]
struct CopyCurveResult {
    curve: Vec<EquityPoint>,
    total_friction_usd: f64,
    trade_count: u64,
    open_positions: HashMap<String, Position>, // condition_id -> position (for settlement)
}

fn compute_copy_trade_curve(
    orders: &[DomeOrder],
    activities: &[ActivityItem],
    min_realized_ts: i64,
    fixed_buy_notional_usd: f64,
    now: i64,
    friction_mode: FrictionMode,
) -> CopyCurveResult {
    // Follower strategy (v3): fixed notional per order with friction + settlement tracking.
    //
    // - BUY: buy shares = (notional - friction_cost) / price, track token_label
    // - SELL: sell shares = notional / price (clamped to available shares)
    // - Average-cost basis for realized PnL
    // - Friction cost applied on each BUY (spread + slippage)
    // - Open positions tracked for market resolution settlement
    let friction_pct = friction_mode.total_friction_pct() / 100.0;
    let mut total_friction_usd = 0.0;
    let mut trade_count: u64 = 0;

    let mut follower_pos: HashMap<String, Position> = HashMap::new();

    let mut daily_realized: BTreeMap<i64, f64> = BTreeMap::new();
    let mut follower_realized_total = 0.0;

    #[derive(Clone)]
    enum CopyEvent {
        Order {
            instrument_id: String,
            side: String,
            price: f64,
            timestamp: i64,
            token_label: Option<String>,
        },
        Redeem {
            instrument_id: String,
            price: f64,
            timestamp: i64,
        },
    }

    let mut events: Vec<CopyEvent> = Vec::with_capacity(orders.len() + activities.len());
    for o in orders {
        events.push(CopyEvent::Order {
            // Activity doesn't include token_id (often empty), but does include condition_id.
            // Use condition_id as our join key between orders and redemption events.
            instrument_id: o.condition_id.clone(),
            side: o.side.clone(),
            price: o.price,
            timestamp: o.timestamp,
            token_label: o.token_label.clone(),
        });
    }
    for a in activities {
        if a.side.to_uppercase() != "REDEEM" {
            continue;
        }
        events.push(CopyEvent::Redeem {
            instrument_id: a.condition_id.clone(),
            price: a.price,
            timestamp: a.timestamp,
        });
    }

    events.sort_by_key(|e| match e {
        CopyEvent::Order { timestamp, .. } => *timestamp,
        CopyEvent::Redeem { timestamp, .. } => *timestamp,
    });

    for e in events {
        match e {
            CopyEvent::Order {
                instrument_id,
                side,
                price,
                timestamp,
                token_label,
            } => {
                let price = price;
                if !price.is_finite() || price <= 0.0 {
                    continue;
                }
                let side = side.to_uppercase();

                if side == "BUY" {
                    // Apply friction cost on entry (spread + slippage)
                    let friction_cost = fixed_buy_notional_usd * friction_pct;
                    let effective_notional = fixed_buy_notional_usd - friction_cost;
                    let shares = (effective_notional / price).max(0.0);

                    let p = follower_pos.entry(instrument_id).or_default();
                    p.shares += shares;
                    p.cost_usd += fixed_buy_notional_usd; // Track full cost for ROE calc
                    p.last_price = price;
                    // Track which side trader bet on (for market resolution settlement)
                    if p.token_label.is_none() {
                        p.token_label = token_label;
                    }

                    total_friction_usd += friction_cost;
                    trade_count += 1;
                } else if side == "SELL" {
                    let in_window = timestamp >= min_realized_ts;
                    let day = day_bucket(timestamp);

                    let follower = follower_pos.entry(instrument_id).or_default();
                    if follower.shares <= 0.0 {
                        continue;
                    }

                    let desired_sell_shares = (fixed_buy_notional_usd / price).max(0.0);
                    let sell_shares = desired_sell_shares.min(follower.shares);
                    if sell_shares <= 0.0 {
                        continue;
                    }

                    let follower_avg_cost = follower.cost_usd / follower.shares;
                    let proceeds = sell_shares * price;
                    let cost = sell_shares * follower_avg_cost;
                    let realized = proceeds - cost;

                    follower.shares -= sell_shares;
                    follower.cost_usd -= cost;

                    if follower.shares <= 0.0 {
                        follower.shares = 0.0;
                        follower.cost_usd = 0.0;
                    }

                    if in_window {
                        follower_realized_total += realized;
                        *daily_realized.entry(day).or_insert(0.0) += realized;
                    }
                }
            }
            CopyEvent::Redeem {
                instrument_id,
                price,
                timestamp,
            } => {
                let price = if price.is_finite() { price } else { 1.0 };
                let in_window = timestamp >= min_realized_ts;
                let day = day_bucket(timestamp);

                let follower = follower_pos.entry(instrument_id).or_default();
                if follower.shares <= 0.0 {
                    continue;
                }

                // Model redeem as closing the entire remaining position at the redemption price.
                let sell_shares = follower.shares;
                let follower_avg_cost = follower.cost_usd / follower.shares;
                let proceeds = sell_shares * price;
                let cost = sell_shares * follower_avg_cost;
                let realized = proceeds - cost;

                follower.shares = 0.0;
                follower.cost_usd = 0.0;

                if in_window {
                    follower_realized_total += realized;
                    *daily_realized.entry(day).or_insert(0.0) += realized;
                }
            }
        }
    }

    // Convert daily deltas into an equity curve.
    let mut curve = Vec::with_capacity(daily_realized.len());
    let mut equity = 0.0;
    for (day, delta) in daily_realized {
        equity += delta;
        curve.push(EquityPoint {
            timestamp: day,
            value: equity,
        });
    }

    // Ensure the UI has something to draw even if there were no realization events.
    if curve.is_empty() {
        let start = day_bucket(min_realized_ts);
        let end = day_bucket(now);
        curve.push(EquityPoint {
            timestamp: start,
            value: 0.0,
        });
        if end != start {
            curve.push(EquityPoint {
                timestamp: end,
                value: follower_realized_total,
            });
        }
    }

    // Return open positions for market resolution settlement
    let open_positions: HashMap<String, Position> = follower_pos
        .into_iter()
        .filter(|(_, p)| p.shares > 1e-9)
        .collect();

    CopyCurveResult {
        curve,
        total_friction_usd,
        trade_count,
        open_positions,
    }
}

/// Settle open positions using market resolution data.
/// Returns additional realized PnL from settled positions.
async fn settle_open_positions(
    rest: &DomeRestClient,
    storage: &DbSignalStorage,
    open_positions: &HashMap<String, Position>,
    now: i64,
    friction_pct: f64,
) -> (f64, BTreeMap<i64, f64>) {
    let mut total_settlement_pnl = 0.0;
    let mut daily_settlements: BTreeMap<i64, f64> = BTreeMap::new();

    for (condition_id, position) in open_positions {
        if position.shares <= 1e-9 {
            continue;
        }

        // Check cache first
        let cache_key = format!("market_resolution_v1:{}", condition_id);
        let cached = storage.get_cache(&cache_key).ok().flatten();

        let market = if let Some((json, fetched_at)) = cached {
            // Use cache if less than 1 hour old
            if now - fetched_at < 3600 {
                serde_json::from_str::<crate::scrapers::dome_rest::DomeMarket>(&json).ok()
            } else {
                None
            }
        } else {
            None
        };

        let market = match market {
            Some(m) => Some(m),
            None => {
                // Fetch from API with timeout
                match tokio::time::timeout(
                    Duration::from_millis(500),
                    rest.get_market_by_condition_id(condition_id),
                )
                .await
                {
                    Ok(Ok(Some(m))) => {
                        // Cache the result
                        if let Ok(json) = serde_json::to_string(&m) {
                            let _ = storage.upsert_cache(&cache_key, &json, now);
                        }
                        Some(m)
                    }
                    _ => None,
                }
            }
        };

        let Some(market) = market else {
            // Can't fetch market, use mark-to-market with last known price
            let mtm_value = position.shares * position.last_price;
            let unrealized = mtm_value - position.cost_usd;
            // Don't add unrealized to curve (would be misleading)
            continue;
        };

        // Check if market resolved
        let winning_label = match (&market.winning_side, &market.side_a, &market.side_b) {
            (Some(winner), Some(side_a), Some(side_b)) => {
                if winner.to_lowercase() == "a" || winner == &side_a.id {
                    Some(side_a.label.clone())
                } else if winner.to_lowercase() == "b" || winner == &side_b.id {
                    Some(side_b.label.clone())
                } else {
                    None
                }
            }
            _ => None,
        };

        let Some(winning_label) = winning_label else {
            // Market not resolved yet, skip
            continue;
        };

        // Determine if position won or lost
        let position_won = position
            .token_label
            .as_ref()
            .map(|label| label.to_lowercase() == winning_label.to_lowercase())
            .unwrap_or(false);

        // Settlement price: $1.00 if won, $0.00 if lost
        let settlement_price = if position_won { 1.0 } else { 0.0 };
        let proceeds = position.shares * settlement_price;

        // Apply exit friction (slippage on redemption is minimal, but include spread)
        let exit_friction = proceeds * (friction_pct / 2.0); // Half friction on exit
        let net_proceeds = proceeds - exit_friction;

        let realized = net_proceeds - position.cost_usd;
        total_settlement_pnl += realized;

        // Add to settlement day (use market completed_time or now)
        let settlement_day = market
            .completed_time
            .map(|t| day_bucket(t))
            .unwrap_or_else(|| day_bucket(now));
        *daily_settlements.entry(settlement_day).or_insert(0.0) += realized;
    }

    (total_settlement_pnl, daily_settlements)
}

fn normalize_curve_to_zero(curve: &mut Vec<EquityPoint>) {
    if curve.len() < 2 {
        return;
    }
    curve.sort_by_key(|p| p.timestamp);
    let base = curve.first().map(|p| p.value).unwrap_or(0.0);
    if !base.is_finite() {
        return;
    }
    for p in curve.iter_mut() {
        if p.value.is_finite() {
            p.value -= base;
        }
    }
}

fn compute_copy_scale_factor(orders: &[DomeOrder], start_time: i64, fixed_buy_notional_usd: f64) -> f64 {
    if !fixed_buy_notional_usd.is_finite() || fixed_buy_notional_usd <= 0.0 {
        return 1.0;
    }

    let mut notional_sum = 0.0;
    let mut n = 0.0;
    for o in orders {
        if o.timestamp < start_time {
            continue;
        }
        if o.side.to_uppercase() != "BUY" {
            continue;
        }
        let price = o.price;
        let shares = o.shares_normalized;
        if !price.is_finite() || price <= 0.0 || !shares.is_finite() || shares <= 0.0 {
            continue;
        }
        notional_sum += shares * price;
        n += 1.0;
    }

    if n <= 0.0 {
        return 1.0;
    }
    let avg = (notional_sum / n).max(1e-6);
    fixed_buy_notional_usd / avg
}

fn simulate_copy_trade_notional(
    orders: &[DomeOrder],
    activities: &[ActivityItem],
    fetch_start_time: i64,
    start_time: i64,
    now: i64,
    scale: f64,
) -> (BTreeMap<i64, f64>, u64, u64) {
    #[derive(Clone)]
    enum CopyEvent {
        Order {
            token_id: String,
            condition_id: String,
            side: String,
            price: f64,
            shares_normalized: f64,
            timestamp: i64,
        },
        Redeem {
            token_id: Option<String>,
            condition_id: String,
            timestamp: i64,
        },
    }

    let mut events: Vec<CopyEvent> = Vec::with_capacity(orders.len() + activities.len());
    for o in orders {
        if o.timestamp < fetch_start_time || o.timestamp > now {
            continue;
        }
        events.push(CopyEvent::Order {
            token_id: o.token_id.clone(),
            condition_id: o.condition_id.clone(),
            side: o.side.clone(),
            price: o.price,
            shares_normalized: o.shares_normalized,
            timestamp: o.timestamp,
        });
    }
    for a in activities {
        if a.timestamp < fetch_start_time || a.timestamp > now {
            continue;
        }
        if a.side.to_uppercase() != "REDEEM" {
            continue;
        }
        let token_id = if a.token_id.trim().is_empty() {
            None
        } else {
            Some(a.token_id.clone())
        };
        events.push(CopyEvent::Redeem {
            token_id,
            condition_id: a.condition_id.clone(),
            timestamp: a.timestamp,
        });
    }
    events.sort_by_key(|e| match e {
        CopyEvent::Order { timestamp, .. } => *timestamp,
        CopyEvent::Redeem { timestamp, .. } => *timestamp,
    });

    // Positions keyed by token_id (token-specific pricing/settlement), plus token_id -> condition_id.
    let mut shares_by_token: HashMap<String, f64> = HashMap::new();
    let mut condition_by_token: HashMap<String, String> = HashMap::new();

    let mut daily_notional: BTreeMap<i64, f64> = BTreeMap::new();
    let mut trade_count: u64 = 0;
    let mut buy_count: u64 = 0;

    for e in events {
        match e {
            CopyEvent::Order {
                token_id,
                condition_id,
                side,
                price,
                shares_normalized,
                timestamp,
            } => {
                if !price.is_finite() || price <= 0.0 {
                    continue;
                }
                if !shares_normalized.is_finite() || shares_normalized <= 0.0 {
                    continue;
                }

                condition_by_token
                    .entry(token_id.clone())
                    .or_insert(condition_id);

                let side = side.to_uppercase();
                let scaled_shares = shares_normalized * scale;
                if scaled_shares <= 0.0 || !scaled_shares.is_finite() {
                    continue;
                }

                if side == "BUY" {
                    let p = shares_by_token.entry(token_id).or_insert(0.0);
                    *p += scaled_shares;
                    if timestamp >= start_time {
                        buy_count += 1;
                        trade_count += 1;
                        let day = day_bucket(timestamp);
                        *daily_notional.entry(day).or_insert(0.0) += scaled_shares * price;
                    }
                } else if side == "SELL" {
                    let p = shares_by_token.entry(token_id).or_insert(0.0);
                    if *p <= 1e-12 {
                        continue;
                    }
                    let sell_shares = scaled_shares.min(*p);
                    if sell_shares <= 1e-12 {
                        continue;
                    }
                    *p -= sell_shares;
                    if *p <= 1e-12 {
                        *p = 0.0;
                    }

                    if timestamp >= start_time {
                        trade_count += 1;
                        let day = day_bucket(timestamp);
                        *daily_notional.entry(day).or_insert(0.0) += sell_shares * price;
                    }
                }
            }
            CopyEvent::Redeem {
                token_id,
                condition_id,
                ..
            } => {
                if let Some(token_id) = token_id {
                    shares_by_token.insert(token_id, 0.0);
                    continue;
                }

                // If token_id is missing, close any token positions matching this condition.
                let to_close: Vec<String> = condition_by_token
                    .iter()
                    .filter_map(|(tid, cid)| {
                        if cid == &condition_id {
                            Some(tid.clone())
                        } else {
                            None
                        }
                    })
                    .collect();
                for tid in to_close {
                    shares_by_token.insert(tid, 0.0);
                }
            }
        }
    }

    (daily_notional, trade_count, buy_count)
}

fn trade_notional_to_friction_costs(
    daily_trade_notional: &BTreeMap<i64, f64>,
    friction_pct: f64,
) -> (BTreeMap<i64, f64>, f64) {
    let mut daily_costs: BTreeMap<i64, f64> = BTreeMap::new();
    let mut total = 0.0;

    if !friction_pct.is_finite() || friction_pct <= 0.0 {
        return (daily_costs, 0.0);
    }

    for (day, notional) in daily_trade_notional {
        if !notional.is_finite() || *notional <= 0.0 {
            continue;
        }
        let cost = notional * friction_pct;
        if !cost.is_finite() || cost <= 0.0 {
            continue;
        }
        daily_costs.insert(*day, cost);
        total += cost;
    }

    (daily_costs, total)
}

fn apply_daily_costs_to_curve(
    curve: &[EquityPoint],
    daily_costs: &BTreeMap<i64, f64>,
) -> Vec<EquityPoint> {
    if curve.is_empty() {
        return Vec::new();
    }

    let mut out: Vec<EquityPoint> = curve.to_vec();
    out.sort_by_key(|p| p.timestamp);

    let mut running_cost = 0.0;
    let mut cost_iter = daily_costs.iter().peekable();

    for p in out.iter_mut() {
        let day = day_bucket(p.timestamp);
        while let Some((&cost_day, &cost)) = cost_iter.peek() {
            if cost_day > day {
                break;
            }
            running_cost += cost;
            let _ = cost_iter.next();
        }
        p.value -= running_cost;
    }

    out
}

#[derive(Debug, Clone)]
struct CopyMtmResult {
    curve: Vec<EquityPoint>,
    total_friction_usd: f64,
    trade_count: u64,
    buy_count: u64,
}

async fn compute_copy_trade_curve_mtm(
    storage: &DbSignalStorage,
    rest: &DomeRestClient,
    orders: &[DomeOrder],
    activities: &[ActivityItem],
    fetch_start_time: i64,
    start_time: i64,
    now: i64,
    scale: f64,
    friction_pct: f64,
) -> Option<CopyMtmResult> {
    if scale <= 0.0 || !scale.is_finite() {
        return None;
    }

    #[derive(Clone)]
    enum CopyEvent {
        Order {
            token_id: String,
            condition_id: String,
            side: String,
            price: f64,
            shares_normalized: f64,
            timestamp: i64,
        },
        Redeem {
            token_id: Option<String>,
            condition_id: String,
            price: f64,
            timestamp: i64,
        },
    }

    let mut events: Vec<CopyEvent> = Vec::with_capacity(orders.len() + activities.len());

    for o in orders {
        if o.timestamp < fetch_start_time || o.timestamp > now {
            continue;
        }
        events.push(CopyEvent::Order {
            token_id: o.token_id.clone(),
            condition_id: o.condition_id.clone(),
            side: o.side.clone(),
            price: o.price,
            shares_normalized: o.shares_normalized,
            timestamp: o.timestamp,
        });
    }

    for a in activities {
        if a.timestamp < fetch_start_time || a.timestamp > now {
            continue;
        }
        if a.side.to_uppercase() != "REDEEM" {
            continue;
        }
        let token_id = if a.token_id.trim().is_empty() {
            None
        } else {
            Some(a.token_id.clone())
        };
        events.push(CopyEvent::Redeem {
            token_id,
            condition_id: a.condition_id.clone(),
            price: a.price,
            timestamp: a.timestamp,
        });
    }

    events.sort_by_key(|e| match e {
        CopyEvent::Order { timestamp, .. } => *timestamp,
        CopyEvent::Redeem { timestamp, .. } => *timestamp,
    });

    // Fetch daily candles for the traded conditions (token-specific series). Best-effort and cached.
    const MAX_CONDITIONS: usize = 40;
    const CANDLE_LOOKBACK_DAYS: i64 = 200;
    const CANDLE_CACHE_TTL_SECONDS: i64 = 6 * 3600;

    // Pull the most-recent traded conditions first.
    let mut condition_ids: Vec<String> = Vec::new();
    let mut seen_conditions: HashSet<String> = HashSet::new();
    for o in orders.iter().rev() {
        if o.timestamp < fetch_start_time || o.timestamp > now {
            continue;
        }
        if seen_conditions.insert(o.condition_id.clone()) {
            condition_ids.push(o.condition_id.clone());
        }
        if condition_ids.len() >= MAX_CONDITIONS {
            break;
        }
    }

    let candle_start = now - (CANDLE_LOOKBACK_DAYS * 86_400);
    let mut token_close_by_day: HashMap<String, HashMap<i64, f64>> = HashMap::new();

    let sem = Arc::new(tokio::sync::Semaphore::new(6));
    let mut futs: FuturesUnordered<_> = FuturesUnordered::new();
    for condition_id in condition_ids {
        let sem = sem.clone();
        futs.push(async move {
            let _permit = sem.acquire().await.ok()?;

            let cache_key = format!("candles_day_v1:{}", condition_id);
            let cached = storage.get_cache(&cache_key).ok().flatten();

            let raw: Option<Value> = match cached {
                Some((json, fetched_at)) if now - fetched_at < CANDLE_CACHE_TTL_SECONDS => {
                    serde_json::from_str::<Value>(&json).ok()
                }
                _ => {
                    match tokio::time::timeout(
                        Duration::from_millis(750),
                        rest.get_candlesticks_raw(&condition_id, candle_start, now, Some(1440)),
                    )
                    .await
                    {
                        Ok(Ok(v)) => {
                            let _ = storage.upsert_cache(&cache_key, &v.to_string(), now);
                            Some(v)
                        }
                        Ok(Err(e)) => {
                            warn!("candlesticks fetch failed for {}: {}", condition_id, e);
                            None
                        }
                        Err(_) => None,
                    }
                }
            };

            let raw = raw?;
            Some(parse_token_daily_closes(&raw))
        });
    }

    while let Some(res) = futs.next().await {
        let Some(parsed) = res else {
            continue;
        };
        for (token_id, closes) in parsed {
            if closes.is_empty() {
                continue;
            }
            token_close_by_day
                .entry(token_id)
                .or_insert_with(HashMap::new)
                .extend(closes);
        }
    }

    #[derive(Debug, Clone, Default)]
    struct TokenPosition {
        condition_id: String,
        shares: f64,
        cost_usd: f64,
        last_price: f64,
    }

    let mut pos_by_token: HashMap<String, TokenPosition> = HashMap::new();
    let mut condition_by_token: HashMap<String, String> = HashMap::new();

    let mut realized_total = 0.0;
    let mut friction_total = 0.0;

    let mut in_window_trade_count: u64 = 0;
    let mut in_window_buy_count: u64 = 0;
    let mut in_window_friction: f64 = 0.0;

    let mut curve: Vec<EquityPoint> = Vec::new();

    let fetch_day = day_bucket(fetch_start_time);
    let start_day = day_bucket(start_time);
    let end_day = day_bucket(now);
    if end_day < fetch_day {
        return None;
    }

    let mut event_idx = 0usize;
    let mut day = fetch_day;
    while day <= end_day {
        while event_idx < events.len() {
            let e_day = match &events[event_idx] {
                CopyEvent::Order { timestamp, .. } => day_bucket(*timestamp),
                CopyEvent::Redeem { timestamp, .. } => day_bucket(*timestamp),
            };
            if e_day > day {
                break;
            }

            let e = events[event_idx].clone();
            event_idx += 1;

            match e {
                CopyEvent::Order {
                    token_id,
                    condition_id,
                    side,
                    price,
                    shares_normalized,
                    timestamp,
                } => {
                    if !price.is_finite() || price <= 0.0 {
                        continue;
                    }
                    if !shares_normalized.is_finite() || shares_normalized <= 0.0 {
                        continue;
                    }
                    let side = side.to_uppercase();
                    let scaled_shares = shares_normalized * scale;
                    if !scaled_shares.is_finite() || scaled_shares <= 0.0 {
                        continue;
                    }

                    condition_by_token
                        .entry(token_id.clone())
                        .or_insert(condition_id.clone());

                    let p = pos_by_token.entry(token_id.clone()).or_insert_with(|| TokenPosition {
                        condition_id,
                        ..Default::default()
                    });
                    p.last_price = price;

                    if side == "BUY" {
                        p.shares += scaled_shares;
                        p.cost_usd += scaled_shares * price;

                        let notional = scaled_shares * price;
                        let friction_cost = notional * friction_pct;
                        friction_total += friction_cost;

                        if timestamp >= start_time {
                            in_window_buy_count += 1;
                            in_window_trade_count += 1;
                            in_window_friction += friction_cost;
                        }
                    } else if side == "SELL" {
                        if p.shares <= 1e-12 {
                            continue;
                        }
                        let sell_shares = scaled_shares.min(p.shares);
                        if sell_shares <= 1e-12 {
                            continue;
                        }

                        let avg_cost = if p.shares > 0.0 {
                            p.cost_usd / p.shares
                        } else {
                            0.0
                        };
                        let proceeds = sell_shares * price;
                        let cost = sell_shares * avg_cost;
                        let realized = proceeds - cost;

                        p.shares -= sell_shares;
                        p.cost_usd -= cost;
                        if p.shares <= 1e-12 {
                            p.shares = 0.0;
                            p.cost_usd = 0.0;
                        }

                        realized_total += realized;

                        let friction_cost = proceeds * friction_pct;
                        friction_total += friction_cost;

                        if timestamp >= start_time {
                            in_window_trade_count += 1;
                            in_window_friction += friction_cost;
                        }
                    }
                }
                CopyEvent::Redeem {
                    token_id,
                    condition_id,
                    price,
                    timestamp,
                } => {
                    let price = if price.is_finite() && price >= 0.0 {
                        price
                    } else {
                        1.0
                    };

                    let mut to_close: Vec<String> = Vec::new();
                    if let Some(token_id) = token_id {
                        to_close.push(token_id);
                    } else {
                        for (tid, cid) in &condition_by_token {
                            if cid == &condition_id {
                                to_close.push(tid.clone());
                            }
                        }
                    }

                    for tid in to_close {
                        let Some(p) = pos_by_token.get_mut(&tid) else {
                            continue;
                        };
                        if p.shares <= 1e-12 {
                            continue;
                        }
                        let proceeds = p.shares * price;
                        let realized = proceeds - p.cost_usd;
                        realized_total += realized;
                        p.shares = 0.0;
                        p.cost_usd = 0.0;

                        // No execution friction on redeem.
                        if timestamp >= start_time {
                            // Redeems are not counted as trades.
                        }
                    }
                }
            }
        }

        // Mark-to-market at end-of-day.
        let mut unrealized = 0.0;
        for (tid, p) in &pos_by_token {
            if p.shares <= 1e-12 {
                continue;
            }
            let mark = token_close_by_day
                .get(tid)
                .and_then(|m| m.get(&day))
                .copied()
                .unwrap_or(p.last_price);
            if !mark.is_finite() || mark < 0.0 {
                continue;
            }
            unrealized += (p.shares * mark) - p.cost_usd;
        }

        let equity = realized_total - friction_total + unrealized;
        curve.push(EquityPoint {
            timestamp: day,
            value: equity,
        });

        day += 86_400;
    }

    // Slice to window and normalize to start at 0.
    let mut window_curve: Vec<EquityPoint> = curve
        .into_iter()
        .filter(|p| p.timestamp >= start_day)
        .collect();

    if window_curve.len() < 2 {
        return None;
    }

    window_curve.sort_by_key(|p| p.timestamp);
    let base = window_curve
        .iter()
        .find(|p| p.timestamp == start_day)
        .or_else(|| window_curve.first())
        .map(|p| p.value)
        .unwrap_or(0.0);

    if base.is_finite() {
        for p in window_curve.iter_mut() {
            p.value -= base;
        }
    }

    Some(CopyMtmResult {
        curve: window_curve,
        total_friction_usd: in_window_friction,
        trade_count: in_window_trade_count,
        buy_count: in_window_buy_count,
    })
}

fn parse_token_daily_closes(raw: &Value) -> HashMap<String, HashMap<i64, f64>> {
    let mut out: HashMap<String, HashMap<i64, f64>> = HashMap::new();

    let Some(tokens) = raw
        .get("candlesticks")
        .and_then(|v| v.as_array())
        .filter(|a| !a.is_empty())
    else {
        return out;
    };

    for token_entry in tokens {
        let Some(pair) = token_entry.as_array() else {
            continue;
        };
        if pair.len() < 2 {
            continue;
        }
        let Some(candles) = pair[0].as_array() else {
            continue;
        };
        let token_id = pair[1]
            .get("token_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let Some(token_id) = token_id else {
            continue;
        };

        let mut by_day: HashMap<i64, f64> = HashMap::new();
        for c in candles {
            let ts = c
                .get("end_period_ts")
                .and_then(|v| v.as_i64())
                .or_else(|| c.get("timestamp").and_then(|v| v.as_i64()));
            let Some(ts) = ts else {
                continue;
            };

            let close = c
                .get("price")
                .and_then(|p| p.get("close_dollars"))
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse::<f64>().ok())
                .or_else(|| {
                    c.get("price")
                        .and_then(|p| p.get("close"))
                        .and_then(|v| v.as_f64())
                        .map(|n| if n > 1.5 { n / 100.0 } else { n })
                });

            let Some(close) = close else {
                continue;
            };
            if !close.is_finite() {
                continue;
            }

            by_day.insert(day_bucket(ts), close);
        }

        if !by_day.is_empty() {
            out.insert(token_id, by_day);
        }
    }

    out
}

fn compute_scaled_copy_curve(
    wallet_curve: &[EquityPoint],
    orders_sample: &[DomeOrder],
    fixed_buy_notional_usd: f64,
) -> Vec<EquityPoint> {
    if wallet_curve.is_empty() {
        return Vec::new();
    }

    let mut notional_sum = 0.0;
    let mut n = 0.0;
    for o in orders_sample {
        let price = o.price;
        if !price.is_finite() || price <= 0.0 {
            continue;
        }
        let shares = o.shares_normalized;
        if !shares.is_finite() || shares <= 0.0 {
            continue;
        }
        // Approx. USDC spent/received for this fill.
        notional_sum += shares * price;
        n += 1.0;
    }

    let avg_notional = if n > 0.0 {
        (notional_sum / n).max(1e-6)
    } else {
        1.0
    };
    let scale = fixed_buy_notional_usd / avg_notional;

    wallet_curve
        .iter()
        .map(|p| EquityPoint {
            timestamp: p.timestamp,
            value: p.value * scale,
        })
        .collect()
}

#[inline]
fn day_bucket(ts: i64) -> i64 {
    // Dome timestamps are in seconds.
    (ts / 86_400) * 86_400
}

fn winsorize_in_place(values: &mut [f64], pct: f64) {
    if values.len() < 10 {
        return;
    }

    let mut sorted: Vec<f64> = values.iter().copied().filter(|v| v.is_finite()).collect();
    if sorted.len() < 10 {
        return;
    }

    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = sorted.len();

    let lo_idx = (((n - 1) as f64) * pct).floor() as usize;
    let hi_idx = (((n - 1) as f64) * (1.0 - pct)).ceil() as usize;
    let lo = sorted[lo_idx.min(n - 1)];
    let hi = sorted[hi_idx.min(n - 1)];
    if !lo.is_finite() || !hi.is_finite() || lo >= hi {
        return;
    }

    for v in values.iter_mut() {
        if !v.is_finite() {
            continue;
        }
        if *v < lo {
            *v = lo;
        } else if *v > hi {
            *v = hi;
        }
    }
}

fn compute_curve_sharpe(curve: &[EquityPoint], now: i64, window_days: i64) -> Option<f64> {
    if curve.len() < 3 {
        return None;
    }

    let start_day = day_bucket(now - window_days * 86_400);
    let end_day = day_bucket(now);
    if end_day < start_day {
        return None;
    }

    // Normalize to daily deltas + fill missing days with 0 so the Sharpe is stable.
    let mut by_day: BTreeMap<i64, f64> = BTreeMap::new();
    for w in curve.windows(2) {
        let (a, b) = (&w[0], &w[1]);
        let day = day_bucket(b.timestamp);
        if day < start_day || day > end_day {
            continue;
        }
        let delta = b.value - a.value;
        if !delta.is_finite() {
            continue;
        }
        *by_day.entry(day).or_insert(0.0) += delta;
    }

    let mut window: Vec<f64> = Vec::new();
    let mut day = start_day;
    while day <= end_day {
        window.push(*by_day.get(&day).unwrap_or(&0.0));
        day += 86_400;
    }

    if window.len() < 3 {
        return None;
    }

    winsorize_in_place(&mut window, 0.05);

    let mean = window.iter().sum::<f64>() / window.len() as f64;
    let var = window
        .iter()
        .map(|v| {
            let d = v - mean;
            d * d
        })
        .sum::<f64>()
        / (window.len() as f64 - 1.0);

    let std = var.sqrt();
    if !std.is_finite() || std == 0.0 {
        return None;
    }
    Some((mean / std) * 365.0_f64.sqrt())
}

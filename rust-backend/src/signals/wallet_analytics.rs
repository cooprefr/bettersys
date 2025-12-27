use crate::{
    scrapers::dome_rest::{ActivityItem, DomeOrder, DomeRestClient, WalletPnlGranularity},
    signals::db_storage::DbSignalStorage,
};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
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
        }
    }
}

// Keep this fairly short so charts reflect new WS events quickly.
const CACHE_TTL_SECONDS: i64 = 120;

#[inline]
fn cache_key(wallet: &str, friction_mode: FrictionMode) -> String {
    // v4: includes friction mode in cache key
    let mode_str = match friction_mode {
        FrictionMode::Optimistic => "opt",
        FrictionMode::Base => "base",
        FrictionMode::Pessimistic => "pess",
    };
    format!("wallet_analytics_v4:{}:{}", wallet.to_lowercase(), mode_str)
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
    let key = cache_key(&wallet_norm, params.friction_mode);

    // If cache exists but is stale, keep it around as a fallback when upstream APIs fail.
    let mut stale_fallback: Option<WalletAnalytics> = None;
    if let Ok(Some((json, fetched_at))) = storage.get_cache(&key) {
        if !force && now - fetched_at <= CACHE_TTL_SECONDS {
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
        Duration::from_secs(1),
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

    let mut copy_result = compute_copy_trade_curve(
        &orders,
        &activities,
        start_time,
        params.fixed_buy_notional_usd,
        now,
        params.friction_mode,
    );

    // Settle open positions using market resolution data.
    // This is the key difference from the wallet curve - we simulate what a copy trader
    // would actually realize based on market outcomes, not the whale's actual sizing.
    let friction_pct = params.friction_mode.total_friction_pct() / 100.0;
    if !copy_result.open_positions.is_empty() {
        let (settlement_pnl, daily_settlements) = settle_open_positions(
            rest,
            storage,
            &copy_result.open_positions,
            now,
            friction_pct,
        )
        .await;

        // Merge settlement PnL into the curve
        if !daily_settlements.is_empty() {
            // Convert existing curve to a mutable map
            let mut daily_pnl: BTreeMap<i64, f64> = BTreeMap::new();
            let mut running_equity = 0.0;

            // First, convert curve back to daily deltas
            let mut prev_value = 0.0;
            for point in &copy_result.curve {
                let delta = point.value - prev_value;
                *daily_pnl.entry(point.timestamp).or_insert(0.0) += delta;
                prev_value = point.value;
            }

            // Add settlement deltas
            for (day, delta) in daily_settlements {
                *daily_pnl.entry(day).or_insert(0.0) += delta;
            }

            // Rebuild curve from merged deltas
            copy_result.curve.clear();
            for (day, delta) in daily_pnl {
                running_equity += delta;
                copy_result.curve.push(EquityPoint {
                    timestamp: day,
                    value: running_equity,
                });
            }

            // Update friction total with exit friction from settlements
            copy_result.total_friction_usd += settlement_pnl.abs() * (friction_pct / 2.0);
        }
    }

    // Ensure curve has at least start/end points for UI rendering
    if copy_result.curve.is_empty() {
        let start = day_bucket(start_time);
        let end = day_bucket(now);
        copy_result.curve.push(EquityPoint {
            timestamp: start,
            value: 0.0,
        });
        if end != start {
            copy_result.curve.push(EquityPoint {
                timestamp: end,
                value: 0.0,
            });
        }
    }

    let wallet_total_pnl = curve_total_pnl(&wallet_realized_curve);
    let copy_total_pnl = curve_total_pnl(&copy_result.curve);

    let wallet_roe_denom_usd = compute_wallet_roe_denom_usd(&orders, start_time);
    let wallet_roe_pct = compute_roe_pct(wallet_total_pnl, wallet_roe_denom_usd);
    let (wallet_win_rate, wallet_profit_factor) =
        compute_curve_win_rate_profit_factor(&wallet_realized_curve);

    let copy_roe_denom_usd =
        compute_copy_roe_denom_usd(&orders, start_time, params.fixed_buy_notional_usd);
    let copy_roe_pct = compute_roe_pct(copy_total_pnl, copy_roe_denom_usd);
    let (copy_win_rate, copy_profit_factor) =
        compute_curve_win_rate_profit_factor(&copy_result.curve);

    let sharpe_7d = compute_curve_sharpe(&copy_result.curve, now, 7);
    let sharpe_14d = compute_curve_sharpe(&copy_result.curve, now, 14);
    let sharpe_30d = compute_curve_sharpe(&copy_result.curve, now, 30);
    let sharpe_90d = compute_curve_sharpe(&copy_result.curve, now, 90);

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
        copy_trade_curve: copy_result.curve,
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
        copy_total_friction_usd: Some(copy_result.total_friction_usd),
        copy_trade_count: Some(copy_result.trade_count),
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

fn compute_curve_sharpe(curve: &[EquityPoint], now: i64, window_days: i64) -> Option<f64> {
    if curve.len() < 3 {
        return None;
    }

    let cutoff = now - window_days * 86_400;
    let mut deltas: Vec<(i64, f64)> = Vec::with_capacity(curve.len().saturating_sub(1));
    for w in curve.windows(2) {
        let (a, b) = (&w[0], &w[1]);
        deltas.push((b.timestamp, b.value - a.value));
    }

    let window: Vec<f64> = deltas
        .into_iter()
        .filter(|(ts, _)| *ts >= cutoff)
        .map(|(_, d)| d)
        .collect();
    if window.len() < 3 {
        return None;
    }

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

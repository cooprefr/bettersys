use crate::{
    scrapers::dome_rest::{ActivityItem, DomeOrder, DomeRestClient, WalletPnlGranularity},
    signals::db_storage::DbSignalStorage,
};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::time::Duration;
use tracing::warn;

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
}

#[derive(Debug, Clone)]
pub struct WalletAnalyticsParams {
    pub lookback_days: u32,
    pub warmup_days: u32,
    pub fixed_buy_notional_usd: f64,
    pub max_orders: usize,
}

impl Default for WalletAnalyticsParams {
    fn default() -> Self {
        Self {
            lookback_days: 90,
            warmup_days: 30,
            fixed_buy_notional_usd: 1.0,
            // Keep this modest so /api/wallet/analytics stays responsive even for very active wallets.
            max_orders: 2_000,
        }
    }
}

// Keep this fairly short so charts reflect new WS events quickly.
const CACHE_TTL_SECONDS: i64 = 120;

#[inline]
fn cache_key(wallet: &str) -> String {
    // v3: adds ROE / win-rate / profit-factor fields
    format!("wallet_analytics_v3:{}", wallet.to_lowercase())
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
    let key = cache_key(&wallet_norm);

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

    let mut copy_trade_curve = compute_copy_trade_curve(
        &orders,
        &activities,
        start_time,
        params.fixed_buy_notional_usd,
        now,
    );

    // If event-based simulation produces nothing useful (common for wallets whose realized PnL is
    // dominated by on-chain MERGE/REDEEM flows that don't cleanly map to order-side events), fall
    // back to a fast heuristic: scale the wallet's realized curve by an estimated notional-per-
    // order ratio so the UI never shows an empty copy curve.
    let wallet_total = wallet_realized_curve.last().map(|p| p.value).unwrap_or(0.0);
    let copy_total = copy_trade_curve.last().map(|p| p.value).unwrap_or(0.0);
    if wallet_total.abs() > 1e-6 && copy_total.abs() <= 1e-9 {
        copy_trade_curve = compute_scaled_copy_curve(
            &wallet_realized_curve,
            &orders,
            params.fixed_buy_notional_usd,
        );
    }

    let wallet_total_pnl = curve_total_pnl(&wallet_realized_curve);
    let copy_total_pnl = curve_total_pnl(&copy_trade_curve);

    let wallet_roe_denom_usd = compute_wallet_roe_denom_usd(&orders, start_time);
    let wallet_roe_pct = compute_roe_pct(wallet_total_pnl, wallet_roe_denom_usd);
    let (wallet_win_rate, wallet_profit_factor) =
        compute_curve_win_rate_profit_factor(&wallet_realized_curve);

    let copy_roe_denom_usd =
        compute_copy_roe_denom_usd(&orders, start_time, params.fixed_buy_notional_usd);
    let copy_roe_pct = compute_roe_pct(copy_total_pnl, copy_roe_denom_usd);
    let (copy_win_rate, copy_profit_factor) =
        compute_curve_win_rate_profit_factor(&copy_trade_curve);

    let sharpe_7d = compute_curve_sharpe(&copy_trade_curve, now, 7);
    let sharpe_14d = compute_curve_sharpe(&copy_trade_curve, now, 14);
    let sharpe_30d = compute_curve_sharpe(&copy_trade_curve, now, 30);
    let sharpe_90d = compute_curve_sharpe(&copy_trade_curve, now, 90);

    Ok(WalletAnalytics {
        wallet_address: wallet.to_string(),
        updated_at: now,
        lookback_days: params.lookback_days,
        fixed_buy_notional_usd: params.fixed_buy_notional_usd,
        wallet_realized_curve,
        copy_trade_curve,
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
}

fn compute_copy_trade_curve(
    orders: &[DomeOrder],
    activities: &[ActivityItem],
    min_realized_ts: i64,
    fixed_buy_notional_usd: f64,
    now: i64,
) -> Vec<EquityPoint> {
    // Follower strategy (v1): fixed notional per order (BUY or SELL).
    //
    // - BUY: buy shares = notional / price
    // - SELL: sell shares = notional / price (clamped to available shares)
    // - Average-cost basis for realized PnL
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
            } => {
                let price = price;
                if !price.is_finite() || price <= 0.0 {
                    continue;
                }
                let side = side.to_uppercase();

                if side == "BUY" {
                    let p = follower_pos.entry(instrument_id).or_default();
                    let shares = (fixed_buy_notional_usd / price).max(0.0);
                    p.shares += shares;
                    p.cost_usd += fixed_buy_notional_usd;
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

    curve
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

//! Dome signal enrichment pipeline.
//!
//! WebSocket orders remain the primary feed. This module uses Dome REST endpoints
//! to attach contextual information to each tracked-wallet order signal.

use anyhow::{Context, Result};
use chrono::Utc;
use reqwest::Client;
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc, Mutex, Semaphore};
use tracing::{debug, warn};

use crate::{
    models::{
        MarketPriceSnapshot, SignalContext, SignalContextActivity, SignalContextDerived,
        SignalContextOrder, SignalContextOrderbook, SignalContextPrice, SignalContextTradeHistory,
        SignalContextUpdate, TradeFlowSummary, WsServerEvent,
    },
    scrapers::dome_rest::{DomeRestClient, OrdersFilter, WalletPnlGranularity},
    signals::db_storage::DbSignalStorage,
};

#[derive(Debug, Clone)]
pub struct EnrichmentJob {
    pub signal_id: String,
    pub user: String,
    pub market_slug: String,
    pub condition_id: String,
    pub token_id: String,
    pub token_label: Option<String>,
    pub side: String,
    pub price: f64,
    pub shares_normalized: f64,
    pub timestamp: i64,
    pub order_hash: String,
    pub tx_hash: String,
    pub title: String,

    /// Raw WebSocket order payload (lossless). Stored in DB.
    pub raw_payload_json: String,
}

#[derive(Clone)]
pub struct DomeEnrichmentService {
    rest: DomeRestClient,
    gamma_http: Client,
    storage: Arc<DbSignalStorage>,
    ws_tx: broadcast::Sender<WsServerEvent>,

    request_sem: Arc<Semaphore>,
    heavy_request_sem: Arc<Semaphore>,
}

impl DomeEnrichmentService {
    pub fn new(
        rest: DomeRestClient,
        storage: Arc<DbSignalStorage>,
        ws_tx: broadcast::Sender<WsServerEvent>,
        max_concurrent_requests: usize,
        max_concurrent_heavy_requests: usize,
    ) -> Result<Self> {
        let gamma_http = Client::builder()
            .timeout(std::time::Duration::from_secs(2))
            .pool_max_idle_per_host(10)
            .pool_idle_timeout(std::time::Duration::from_secs(90))
            .tcp_keepalive(std::time::Duration::from_secs(60))
            .user_agent("BetterBot/1.0 (Gamma enrichment)")
            .build()
            .context("Failed to build Gamma HTTP client")?;

        Ok(Self {
            rest,
            gamma_http,
            storage,
            ws_tx,
            request_sem: Arc::new(Semaphore::new(max_concurrent_requests.max(1))),
            heavy_request_sem: Arc::new(Semaphore::new(max_concurrent_heavy_requests.max(1))),
        })
    }

    pub fn spawn_workers(self, rx: mpsc::Receiver<EnrichmentJob>, worker_count: usize) {
        let shared_rx = Arc::new(Mutex::new(rx));
        let workers = worker_count.max(1);

        for i in 0..workers {
            let svc = self.clone();
            let rx = shared_rx.clone();
            tokio::spawn(async move {
                loop {
                    let job_opt = { rx.lock().await.recv().await };
                    let Some(job) = job_opt else {
                        break;
                    };
                    if let Err(e) = svc.process_job(job).await {
                        warn!(worker = i, error = %e, "Enrichment job failed");
                    }
                }
            });
        }
    }

    async fn process_job(&self, job: EnrichmentJob) -> Result<()> {
        // Store raw WS event
        let received_at = Utc::now().timestamp();
        let _ = self
            .storage
            .store_dome_order_event(
                &job.order_hash,
                &job.tx_hash,
                &job.user,
                &job.market_slug,
                &job.condition_id,
                &job.token_id,
                job.timestamp,
                &job.raw_payload_json,
                received_at,
            )
            .await;

        let entry_value = job.shares_normalized * job.price;
        let mut errors: Vec<String> = Vec::new();

        // Market metadata (cache)
        // Primary: Polymarket Gamma (faster + more stable for market metadata)
        // Fallback: Dome /polymarket/markets
        let market_key = format!("market_slug:{}", job.market_slug);
        let market_value = self.get_cached_value(&market_key, 1800).unwrap_or(None);

        let market_fut = async {
            if market_value.is_some() {
                return Ok::<Option<Value>, anyhow::Error>(market_value);
            }

            // 1) Gamma-first
            let gamma_val = {
                let _permit = self.request_sem.acquire().await.context("semaphore")?;
                let url = format!(
                    "https://gamma-api.polymarket.com/markets/slug/{}",
                    job.market_slug
                );
                let resp = self.gamma_http.get(url).send().await;
                match resp {
                    Ok(r) if r.status() == reqwest::StatusCode::NOT_FOUND => None,
                    Ok(r) => {
                        let r = r.error_for_status().context("Gamma market fetch failed")?;
                        let v: Value = r
                            .json()
                            .await
                            .context("Failed to parse Gamma market JSON")?;
                        if v.is_null() {
                            None
                        } else {
                            Some(v)
                        }
                    }
                    Err(_) => None,
                }
            };

            if let Some(v) = &gamma_val {
                let _ =
                    self.storage
                        .upsert_cache(&market_key, &v.to_string(), Utc::now().timestamp());
                return Ok(Some(v.clone()));
            }

            // 2) Dome fallback
            let _permit = self.request_sem.acquire().await.context("semaphore")?;
            let markets = self
                .rest
                .get_markets_by_slug(&job.market_slug, Some(5))
                .await?
                .markets;
            let first = markets.into_iter().next();
            let val = first
                .map(|m| serde_json::to_value(m).unwrap_or(serde_json::Value::Null))
                .filter(|v| !v.is_null());
            if let Some(v) = &val {
                let _ =
                    self.storage
                        .upsert_cache(&market_key, &v.to_string(), Utc::now().timestamp());
            }
            Ok(val)
        };

        // Price at entry + latest
        let price_entry_fut = async {
            let _permit = self.request_sem.acquire().await.context("semaphore")?;
            self.rest
                .get_market_price(&job.token_id, Some(job.timestamp))
                .await
        };
        let price_latest_fut = async {
            let _permit = self.request_sem.acquire().await.context("semaphore")?;
            self.rest.get_market_price(&job.token_id, None).await
        };

        // Trade history (market flow around entry)
        let market_orders_fut = async {
            let _permit = self.request_sem.acquire().await.context("semaphore")?;
            self.rest
                .get_orders(
                    OrdersFilter {
                        token_id: Some(job.token_id.clone()),
                        ..Default::default()
                    },
                    Some(job.timestamp - 3600),
                    Some(job.timestamp),
                    Some(200),
                    Some(0),
                )
                .await
        };

        // Wallet recent orders
        let wallet_orders_fut = async {
            let _permit = self.request_sem.acquire().await.context("semaphore")?;
            self.rest
                .get_orders(
                    OrdersFilter {
                        user: Some(job.user.clone()),
                        ..Default::default()
                    },
                    Some(job.timestamp - 86400),
                    Some(job.timestamp),
                    Some(200),
                    Some(0),
                )
                .await
        };

        // Orderbook history around entry (ms)
        let orderbooks_fut = async {
            let _permit = self
                .heavy_request_sem
                .acquire()
                .await
                .context("heavy semaphore")?;
            let end_ms = job.timestamp * 1000;
            let start_ms = (job.timestamp - 300) * 1000;
            self.rest
                .get_orderbooks(&job.token_id, start_ms, end_ms, Some(50), None)
                .await
        };

        // Activity last 24h
        let activity_fut = async {
            let _permit = self.request_sem.acquire().await.context("semaphore")?;
            self.rest
                .get_activity(
                    &job.user,
                    Some(job.timestamp - 86400),
                    Some(job.timestamp),
                    Some(job.market_slug.clone()),
                    None,
                    Some(100),
                    Some(0),
                )
                .await
        };

        // Candlesticks last 24h (1h interval)
        let candles_fut = async {
            let _permit = self
                .heavy_request_sem
                .acquire()
                .await
                .context("heavy semaphore")?;
            self.rest
                .get_candlesticks_raw(
                    &job.condition_id,
                    job.timestamp - 86400,
                    job.timestamp,
                    Some(60),
                )
                .await
        };

        // Wallet mapping (cache)
        let wallet_key = format!("wallet:{}", job.user);
        let wallet_cached = self.get_cached_value(&wallet_key, 86400).unwrap_or(None);
        let wallet_fut = async {
            if wallet_cached.is_some() {
                return Ok::<Option<Value>, anyhow::Error>(wallet_cached);
            }
            let _permit = self.request_sem.acquire().await.context("semaphore")?;
            // Try as EOA first; if that fails, try as proxy.
            let w = match self.rest.get_wallet(Some(&job.user), None).await {
                Ok(w) => Ok(w),
                Err(_) => self.rest.get_wallet(None, Some(&job.user)).await,
            }?;
            let v = serde_json::to_value(w).unwrap_or(serde_json::Value::Null);
            if !v.is_null() {
                let _ =
                    self.storage
                        .upsert_cache(&wallet_key, &v.to_string(), Utc::now().timestamp());
                return Ok(Some(v));
            }
            Ok(None)
        };

        // Wallet PnL (cache). Use `day` granularity to compute 7d/30d/90d aggregates.
        let pnl_key = format!("wallet_pnl_day:{}", job.user);
        let pnl_cached = self.get_cached_value(&pnl_key, 3600).unwrap_or(None);
        let pnl_fut = async {
            if pnl_cached.is_some() {
                return Ok::<Option<Value>, anyhow::Error>(pnl_cached);
            }
            let _permit = self.request_sem.acquire().await.context("semaphore")?;
            let pnl = self
                .rest
                .get_wallet_pnl(&job.user, WalletPnlGranularity::Day, None, None)
                .await?;
            let v = serde_json::to_value(&pnl).unwrap_or(serde_json::Value::Null);
            if !v.is_null() {
                let _ = self
                    .storage
                    .upsert_cache(&pnl_key, &v.to_string(), Utc::now().timestamp());
                return Ok(Some(v));
            }
            Ok(None)
        };

        let (
            market_res,
            price_entry_res,
            price_latest_res,
            market_orders_res,
            wallet_orders_res,
            orderbooks_res,
            activity_res,
            candles_res,
            wallet_res,
            pnl_res,
        ) = tokio::join!(
            market_fut,
            price_entry_fut,
            price_latest_fut,
            market_orders_fut,
            wallet_orders_fut,
            orderbooks_fut,
            activity_fut,
            candles_fut,
            wallet_fut,
            pnl_fut
        );

        let market = match market_res {
            Ok(v) => v,
            Err(e) => {
                errors.push(format!("markets: {e}"));
                None
            }
        };

        let price = {
            let at_entry = match price_entry_res {
                Ok(p) => Some(MarketPriceSnapshot {
                    price: p.price,
                    at_time: p.at_time,
                }),
                Err(e) => {
                    errors.push(format!("market_price_at_entry: {e}"));
                    None
                }
            };
            let latest = match price_latest_res {
                Ok(p) => Some(MarketPriceSnapshot {
                    price: p.price,
                    at_time: p.at_time,
                }),
                Err(e) => {
                    errors.push(format!("market_price_latest: {e}"));
                    None
                }
            };
            if at_entry.is_some() || latest.is_some() {
                Some(SignalContextPrice { at_entry, latest })
            } else {
                None
            }
        };

        let (trade_history, market_orders_value_opt, wallet_orders_value_opt) = {
            let mut sample_market_orders: Option<serde_json::Value> = None;
            let mut sample_wallet_orders: Option<serde_json::Value> = None;
            let mut market_flow_1h: Option<TradeFlowSummary> = None;
            let mut wallet_flow_24h: Option<TradeFlowSummary> = None;

            let market_orders = match market_orders_res {
                Ok(r) => Some(r.orders),
                Err(e) => {
                    errors.push(format!("orders_market: {e}"));
                    None
                }
            };
            let wallet_orders = match wallet_orders_res {
                Ok(r) => Some(r.orders),
                Err(e) => {
                    errors.push(format!("orders_wallet: {e}"));
                    None
                }
            };

            if let Some(orders) = &market_orders {
                market_flow_1h = Some(summarize_orders(orders));
                sample_market_orders = Some(truncate_json_array(
                    serde_json::to_value(orders).unwrap_or(Value::Null),
                    50,
                ));
            }
            if let Some(orders) = &wallet_orders {
                wallet_flow_24h = Some(summarize_orders(orders));
                sample_wallet_orders = Some(truncate_json_array(
                    serde_json::to_value(orders).unwrap_or(Value::Null),
                    50,
                ));
            }

            let th = if market_flow_1h.is_some() || wallet_flow_24h.is_some() {
                Some(SignalContextTradeHistory {
                    market_flow_1h,
                    wallet_flow_24h,
                    sample_market_orders: sample_market_orders.clone(),
                    sample_wallet_orders: sample_wallet_orders.clone(),
                })
            } else {
                None
            };

            (th, sample_market_orders, sample_wallet_orders)
        };

        let orderbook = match orderbooks_res {
            Ok(r) => {
                let snapshot_count = r.snapshots.len();
                if snapshot_count == 0 {
                    None
                } else {
                    let last = r.snapshots.last();
                    let (best_bid, best_ask) = last
                        .map(|s| (best_bid(&s.bids), best_ask(&s.asks)))
                        .unwrap_or((None, None));
                    let (mid, spread) = match (best_bid, best_ask) {
                        (Some(b), Some(a)) => {
                            let mid = (a + b) / 2.0;
                            let spread = a - b;
                            (Some(mid), Some(spread))
                        }
                        _ => (None, None),
                    };
                    Some(SignalContextOrderbook {
                        best_bid,
                        best_ask,
                        mid,
                        spread,
                        snapshot_count: Some(snapshot_count),
                    })
                }
            }
            Err(e) => {
                errors.push(format!("orderbooks: {e}"));
                None
            }
        };

        let activity = match activity_res {
            Ok(r) => {
                let count = r.activities.len();
                let mut merge_count = 0;
                let mut split_count = 0;
                let mut redeem_count = 0;
                for a in &r.activities {
                    match a.side.as_str() {
                        "MERGE" => merge_count += 1,
                        "SPLIT" => split_count += 1,
                        "REDEEM" => redeem_count += 1,
                        _ => {}
                    }
                }
                let sample = if count > 0 {
                    Some(truncate_json_array(
                        serde_json::to_value(&r.activities).unwrap_or(Value::Null),
                        25,
                    ))
                } else {
                    None
                };
                Some(SignalContextActivity {
                    count,
                    merge_count,
                    split_count,
                    redeem_count,
                    sample,
                })
            }
            Err(e) => {
                errors.push(format!("activity: {e}"));
                None
            }
        };

        let candlesticks = match candles_res {
            Ok(v) => Some(v),
            Err(e) => {
                errors.push(format!("candlesticks: {e}"));
                None
            }
        };

        let wallet = match wallet_res {
            Ok(v) => v,
            Err(e) => {
                errors.push(format!("wallet: {e}"));
                None
            }
        };

        let wallet_pnl = match pnl_res {
            Ok(v) => v,
            Err(e) => {
                errors.push(format!("wallet_pnl: {e}"));
                None
            }
        };

        let derived = derive_fields(&price, &orderbook, entry_value, &wallet_pnl, &trade_history);

        let context = SignalContext {
            order: SignalContextOrder {
                user: job.user.clone(),
                market_slug: job.market_slug.clone(),
                condition_id: job.condition_id.clone(),
                token_id: job.token_id.clone(),
                token_label: job.token_label.clone(),
                side: job.side.clone(),
                price: job.price,
                shares_normalized: job.shares_normalized,
                timestamp: job.timestamp,
                order_hash: job.order_hash.clone(),
                tx_hash: job.tx_hash.clone(),
                title: job.title.clone(),
            },
            market,
            price,
            trade_history,
            orderbook,
            activity,
            candlesticks,
            wallet,
            wallet_pnl,
            derived,
            errors: errors.clone(),
        };

        let enriched_at = Utc::now().timestamp();

        let context_version = self
            .storage
            .get_signal_context(&job.signal_id)
            .ok()
            .flatten()
            .map(|r| r.context_version + 1)
            .unwrap_or(1);

        let status = if errors.is_empty() {
            "ok"
        } else if context.market.is_some()
            || context.price.is_some()
            || context.trade_history.is_some()
            || context.orderbook.is_some()
            || context.activity.is_some()
            || context.candlesticks.is_some()
            || context.wallet.is_some()
            || context.wallet_pnl.is_some()
        {
            "partial"
        } else {
            "failed"
        };

        let error_str = if status == "failed" {
            Some(errors.join(" | "))
        } else {
            None
        };

        let _ = self
            .storage
            .store_signal_context(
                &job.signal_id,
                context_version,
                enriched_at,
                status,
                error_str.as_deref(),
                &context,
            )
            .await;

        let update = SignalContextUpdate {
            signal_id: job.signal_id,
            context_version,
            enriched_at,
            status: status.to_string(),
            context,
        };

        let _ = self
            .ws_tx
            .send(WsServerEvent::SignalContext(update))
            .map_err(|e| anyhow::anyhow!("ws broadcast failed: {e}"));

        debug!("Enrichment completed");
        Ok(())
    }

    fn get_cached_value(&self, cache_key: &str, ttl_secs: i64) -> Result<Option<Value>> {
        let Some((json, fetched_at)) = self.storage.get_cache(cache_key)? else {
            return Ok(None);
        };
        let now = Utc::now().timestamp();
        if now - fetched_at > ttl_secs {
            return Ok(None);
        }
        let v: Value = serde_json::from_str(&json).unwrap_or(Value::Null);
        if v.is_null() {
            Ok(None)
        } else {
            Ok(Some(v))
        }
    }
}

fn summarize_orders(orders: &[crate::scrapers::dome_rest::DomeOrder]) -> TradeFlowSummary {
    let mut buy_count = 0usize;
    let mut sell_count = 0usize;
    let mut total_shares = 0.0f64;
    let mut total_value = 0.0f64;
    let mut max_value: Option<f64> = None;

    for o in orders {
        let side = o.side.to_uppercase();
        if side == "BUY" {
            buy_count += 1;
        } else if side == "SELL" {
            sell_count += 1;
        }
        total_shares += o.shares_normalized;
        let value = o.shares_normalized * o.price;
        total_value += value;
        max_value = Some(max_value.unwrap_or(0.0).max(value));
    }

    TradeFlowSummary {
        count: orders.len(),
        buy_count,
        sell_count,
        total_shares,
        total_value_usd: total_value,
        max_trade_value_usd: max_value,
    }
}

fn truncate_json_array(value: Value, max_len: usize) -> Value {
    match value {
        Value::Array(arr) => Value::Array(arr.into_iter().take(max_len).collect()),
        other => other,
    }
}

fn best_bid(levels: &[crate::scrapers::dome_rest::OrderbookLevel]) -> Option<f64> {
    levels
        .iter()
        .filter_map(|l| l.price.parse::<f64>().ok())
        .max_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
}

fn best_ask(levels: &[crate::scrapers::dome_rest::OrderbookLevel]) -> Option<f64> {
    levels
        .iter()
        .filter_map(|l| l.price.parse::<f64>().ok())
        .min_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
}

fn derive_fields(
    price: &Option<SignalContextPrice>,
    orderbook: &Option<SignalContextOrderbook>,
    entry_value: f64,
    wallet_pnl: &Option<Value>,
    trade_history: &Option<SignalContextTradeHistory>,
) -> SignalContextDerived {
    let (price_delta_abs, price_delta_bps) = match price {
        Some(p) => match (p.at_entry.as_ref(), p.latest.as_ref()) {
            (Some(e), Some(n)) => {
                let delta = n.price - e.price;
                // Express delta in basis points relative to the entry price (not the $1.00 range).
                // Example: entry=$0.10, move to $0.101 => +1% => +100 bps.
                let bps = if e.price > 0.0 {
                    (delta / e.price) * 10_000.0
                } else {
                    delta * 10_000.0
                };
                (Some(delta), Some(bps))
            }
            _ => (None, None),
        },
        None => (None, None),
    };

    let spread = orderbook.as_ref().and_then(|o| o.spread);

    let (pnl_7d, pnl_14d, pnl_30d, pnl_90d, sharpe_7d, sharpe_14d, sharpe_30d, sharpe_90d) =
        compute_wallet_pnl_metrics(wallet_pnl);

    let wallet_flow_24h = trade_history
        .as_ref()
        .and_then(|th| th.wallet_flow_24h.as_ref());
    let trade_count_24h = wallet_flow_24h.map(|w| w.count as u32);
    let avg_trade_value_24h = wallet_flow_24h.and_then(|w| {
        if w.count == 0 {
            None
        } else {
            Some(w.total_value_usd / w.count as f64)
        }
    });

    SignalContextDerived {
        price_delta_abs,
        price_delta_bps,
        entry_value_usd: Some(entry_value),
        spread_at_entry: spread,
        pnl_7d,
        pnl_14d,
        pnl_30d,
        pnl_90d,

        sharpe_7d,
        sharpe_14d,
        sharpe_30d,
        sharpe_90d,

        avg_trade_value_24h,
        trade_count_24h,
    }
}

fn compute_wallet_pnl_metrics(
    wallet_pnl: &Option<Value>,
) -> (
    Option<f64>,
    Option<f64>,
    Option<f64>,
    Option<f64>,
    Option<f64>,
    Option<f64>,
    Option<f64>,
    Option<f64>,
) {
    let Some(pnl_value) = wallet_pnl else {
        return (None, None, None, None, None, None, None, None);
    };

    let pnl_over_time = match pnl_value.get("pnl_over_time") {
        Some(Value::Array(arr)) => arr,
        _ => return (None, None, None, None, None, None, None, None),
    };

    if pnl_over_time.is_empty() {
        return (None, None, None, None, None, None, None, None);
    }

    let now = chrono::Utc::now().timestamp();
    let day_7 = now - (7 * 24 * 3600);
    let day_14 = now - (14 * 24 * 3600);
    let day_30 = now - (30 * 24 * 3600);
    let day_90 = now - (90 * 24 * 3600);

    // Get the latest PnL value as baseline
    let latest_pnl = pnl_over_time
        .last()
        .and_then(|v| v.get("pnl_to_date"))
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);

    // Find PnL values at the start of each period
    let pnl_at_7d_start = find_pnl_at_time(pnl_over_time, day_7);
    let pnl_at_14d_start = find_pnl_at_time(pnl_over_time, day_14);
    let pnl_at_30d_start = find_pnl_at_time(pnl_over_time, day_30);
    let pnl_at_90d_start = find_pnl_at_time(pnl_over_time, day_90);

    let pnl_7d = pnl_at_7d_start.map(|start| latest_pnl - start);
    let pnl_14d = pnl_at_14d_start.map(|start| latest_pnl - start);
    let pnl_30d = pnl_at_30d_start.map(|start| latest_pnl - start);
    let pnl_90d = pnl_at_90d_start.map(|start| latest_pnl - start);

    // Sharpe-like ratios on realized daily PnL deltas.
    let daily_deltas = extract_pnl_deltas(pnl_over_time);
    let sharpe_7d = compute_sharpe(&daily_deltas, day_7);
    let sharpe_14d = compute_sharpe(&daily_deltas, day_14);
    let sharpe_30d = compute_sharpe(&daily_deltas, day_30);
    let sharpe_90d = compute_sharpe(&daily_deltas, day_90);

    (
        pnl_7d, pnl_14d, pnl_30d, pnl_90d, sharpe_7d, sharpe_14d, sharpe_30d, sharpe_90d,
    )
}

fn extract_pnl_deltas(pnl_over_time: &[Value]) -> Vec<(i64, f64)> {
    // Returns (timestamp, delta_pnl_since_prev_point), sorted by timestamp.
    let mut points: Vec<(i64, f64)> = pnl_over_time
        .iter()
        .filter_map(|entry| {
            let ts = entry.get("timestamp")?.as_i64()?;
            let pnl = entry.get("pnl_to_date")?.as_f64()?;
            Some((ts, pnl))
        })
        .collect();

    points.sort_by(|a, b| a.0.cmp(&b.0));

    let mut deltas = Vec::with_capacity(points.len().saturating_sub(1));
    for w in points.windows(2) {
        if let [(ts0, pnl0), (ts1, pnl1)] = w {
            deltas.push((*ts1, *pnl1 - *pnl0));
        }
    }

    deltas
}

fn compute_sharpe(deltas: &[(i64, f64)], cutoff_ts: i64) -> Option<f64> {
    let window: Vec<f64> = deltas
        .iter()
        .filter(|(ts, _)| *ts >= cutoff_ts)
        .map(|(_, v)| *v)
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

fn find_pnl_at_time(pnl_over_time: &[Value], target_ts: i64) -> Option<f64> {
    // Find the closest data point at or before target_ts
    let mut closest: Option<(i64, f64)> = None;
    for entry in pnl_over_time {
        let ts = entry.get("timestamp").and_then(|v| v.as_i64()).unwrap_or(0);
        let pnl = entry
            .get("pnl_to_date")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);

        if ts <= target_ts {
            match closest {
                None => closest = Some((ts, pnl)),
                Some((prev_ts, _)) if ts > prev_ts => closest = Some((ts, pnl)),
                _ => {}
            }
        }
    }
    closest.map(|(_, pnl)| pnl)
}

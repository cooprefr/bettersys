//! Orderflow-driven paper trading engine (Polymarket WS book stream)

use anyhow::{Context, Result};
use parking_lot::RwLock;
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::{
    scrapers::{polymarket::OrderBook, polymarket_api::PolymarketScraper},
    vault::{
        approximate_nav_usdc, get_book_auto, ExecutionAdapter, OrderRequest, OrderSide,
        PaperExecutionAdapter, StalenessConfig, TimeInForce, VaultActivityRecord,
        VaultNavSnapshotRecord,
    },
    AppState,
};

#[derive(Debug, Clone)]
pub struct OrderflowPaperConfig {
    pub min_imbalance: f64,
    pub imbalance_bps: f64,
    pub max_spread_bps: f64,
    pub min_top_usd: f64,
    pub min_liquidity_usd: f64,
    pub min_volume_usd: f64,
    pub trade_notional_usd: f64,
    pub min_trade_usd: f64,
    pub max_trade_pct_cash: f64,
    pub cooldown_ms: u64,
    pub book_max_stale_ms: u64,
    pub gamma_page_size: usize,
    pub max_markets: usize,
    pub poll_interval_ms: u64,
    pub event_queue_size: usize,
    pub log_interval_sec: u64,
}

impl Default for OrderflowPaperConfig {
    fn default() -> Self {
        Self {
            min_imbalance: 0.18,
            imbalance_bps: 10.0,
            max_spread_bps: 200.0,
            min_top_usd: 25.0,
            min_liquidity_usd: 500.0,
            min_volume_usd: 2_000.0,
            trade_notional_usd: 25.0,
            min_trade_usd: 5.0,
            max_trade_pct_cash: 0.0025,
            cooldown_ms: 7_500,
            book_max_stale_ms: 1200,
            gamma_page_size: 200,
            max_markets: 600,
            poll_interval_ms: 750,
            event_queue_size: 10_000,
            log_interval_sec: 10,
        }
    }
}

impl OrderflowPaperConfig {
    pub fn from_env() -> Self {
        let mut cfg = Self::default();

        if let Ok(v) = std::env::var("ORDERFLOW_PAPER_MIN_IMBALANCE") {
            if let Ok(val) = v.parse::<f64>() {
                if val.is_finite() && val > 0.0 && val < 1.0 {
                    cfg.min_imbalance = val;
                }
            }
        }
        if let Ok(v) = std::env::var("ORDERFLOW_PAPER_IMBALANCE_BPS") {
            if let Ok(val) = v.parse::<f64>() {
                if val.is_finite() && val > 0.0 {
                    cfg.imbalance_bps = val;
                }
            }
        }
        if let Ok(v) = std::env::var("ORDERFLOW_PAPER_MAX_SPREAD_BPS") {
            if let Ok(val) = v.parse::<f64>() {
                if val.is_finite() && val > 0.0 {
                    cfg.max_spread_bps = val;
                }
            }
        }
        if let Ok(v) = std::env::var("ORDERFLOW_PAPER_MIN_TOP_USD") {
            if let Ok(val) = v.parse::<f64>() {
                if val.is_finite() && val >= 0.0 {
                    cfg.min_top_usd = val;
                }
            }
        }
        if let Ok(v) = std::env::var("ORDERFLOW_PAPER_MIN_LIQUIDITY_USD") {
            if let Ok(val) = v.parse::<f64>() {
                if val.is_finite() && val >= 0.0 {
                    cfg.min_liquidity_usd = val;
                }
            }
        }
        if let Ok(v) = std::env::var("ORDERFLOW_PAPER_MIN_VOLUME_USD") {
            if let Ok(val) = v.parse::<f64>() {
                if val.is_finite() && val >= 0.0 {
                    cfg.min_volume_usd = val;
                }
            }
        }
        if let Ok(v) = std::env::var("ORDERFLOW_PAPER_TRADE_NOTIONAL_USD") {
            if let Ok(val) = v.parse::<f64>() {
                if val.is_finite() && val > 0.0 {
                    cfg.trade_notional_usd = val;
                }
            }
        }
        if let Ok(v) = std::env::var("ORDERFLOW_PAPER_MIN_TRADE_USD") {
            if let Ok(val) = v.parse::<f64>() {
                if val.is_finite() && val >= 0.0 {
                    cfg.min_trade_usd = val;
                }
            }
        }
        if let Ok(v) = std::env::var("ORDERFLOW_PAPER_MAX_TRADE_PCT_CASH") {
            if let Ok(val) = v.parse::<f64>() {
                if val.is_finite() && val >= 0.0 {
                    cfg.max_trade_pct_cash = val;
                }
            }
        }
        if let Ok(v) = std::env::var("ORDERFLOW_PAPER_COOLDOWN_MS") {
            if let Ok(val) = v.parse::<u64>() {
                if val >= 250 {
                    cfg.cooldown_ms = val;
                }
            }
        }
        if let Ok(v) = std::env::var("ORDERFLOW_PAPER_BOOK_MAX_STALE_MS") {
            if let Ok(val) = v.parse::<u64>() {
                if val >= 100 {
                    cfg.book_max_stale_ms = val;
                }
            }
        }
        if let Ok(v) = std::env::var("ORDERFLOW_PAPER_GAMMA_PAGE_SIZE") {
            if let Ok(val) = v.parse::<usize>() {
                if val >= 50 {
                    cfg.gamma_page_size = val;
                }
            }
        }
        if let Ok(v) = std::env::var("ORDERFLOW_PAPER_MAX_MARKETS") {
            if let Ok(val) = v.parse::<usize>() {
                cfg.max_markets = val;
            }
        }
        if let Ok(v) = std::env::var("ORDERFLOW_PAPER_POLL_INTERVAL_MS") {
            if let Ok(val) = v.parse::<u64>() {
                if val >= 100 {
                    cfg.poll_interval_ms = val;
                }
            }
        }
        if let Ok(v) = std::env::var("ORDERFLOW_PAPER_EVENT_QUEUE_SIZE") {
            if let Ok(val) = v.parse::<usize>() {
                if val >= 1000 {
                    cfg.event_queue_size = val;
                }
            }
        }
        if let Ok(v) = std::env::var("ORDERFLOW_PAPER_LOG_INTERVAL_SEC") {
            if let Ok(val) = v.parse::<u64>() {
                if val >= 5 {
                    cfg.log_interval_sec = val;
                }
            }
        }

        cfg
    }
}

#[derive(Debug, Clone)]
struct TokenInfo {
    token_id: String,
    market_slug: String,
    outcome: String,
}

#[derive(Debug, Default)]
pub struct OrderflowPaperMetrics {
    updates: AtomicU64,
    evaluations: AtomicU64,
    trades: AtomicU64,
    skipped_no_book: AtomicU64,
    skipped_spread: AtomicU64,
    skipped_imbalance: AtomicU64,
    skipped_cooldown: AtomicU64,
    skipped_liquidity: AtomicU64,
    queue_drops: AtomicU64,
    errors: AtomicU64,
}

#[derive(Debug, Clone, Default)]
pub struct OrderflowPaperMetricsSummary {
    pub updates: u64,
    pub evaluations: u64,
    pub trades: u64,
    pub skipped_no_book: u64,
    pub skipped_spread: u64,
    pub skipped_imbalance: u64,
    pub skipped_cooldown: u64,
    pub skipped_liquidity: u64,
    pub queue_drops: u64,
    pub errors: u64,
}

impl OrderflowPaperMetrics {
    pub fn summary(&self) -> OrderflowPaperMetricsSummary {
        OrderflowPaperMetricsSummary {
            updates: self.updates.load(Ordering::Relaxed),
            evaluations: self.evaluations.load(Ordering::Relaxed),
            trades: self.trades.load(Ordering::Relaxed),
            skipped_no_book: self.skipped_no_book.load(Ordering::Relaxed),
            skipped_spread: self.skipped_spread.load(Ordering::Relaxed),
            skipped_imbalance: self.skipped_imbalance.load(Ordering::Relaxed),
            skipped_cooldown: self.skipped_cooldown.load(Ordering::Relaxed),
            skipped_liquidity: self.skipped_liquidity.load(Ordering::Relaxed),
            queue_drops: self.queue_drops.load(Ordering::Relaxed),
            errors: self.errors.load(Ordering::Relaxed),
        }
    }
}

struct OrderflowPaperEngine {
    state: Arc<AppState>,
    cfg: OrderflowPaperConfig,
    exec: Arc<PaperExecutionAdapter>,
    metrics: Arc<OrderflowPaperMetrics>,
    tokens: HashMap<String, TokenInfo>,
    last_trade_at: RwLock<HashMap<String, Instant>>,
    staleness: StalenessConfig,
}

impl OrderflowPaperEngine {
    async fn handle_update(&self, token_id: &str) -> Result<()> {
        self.metrics.updates.fetch_add(1, Ordering::Relaxed);

        let Some(meta) = self.tokens.get(token_id) else {
            return Ok(());
        };

        let Some(book) = get_book_auto(self.state.as_ref(), token_id, &self.staleness) else {
            self.metrics.skipped_no_book.fetch_add(1, Ordering::Relaxed);
            return Ok(());
        };

        let (best_bid, best_ask) = match (book.bids.first(), book.asks.first()) {
            (Some(bid), Some(ask)) => (bid.price, ask.price),
            _ => {
                self.metrics.skipped_no_book.fetch_add(1, Ordering::Relaxed);
                return Ok(());
            }
        };

        if best_bid <= 0.0 || best_ask <= 0.0 || best_ask <= best_bid {
            self.metrics.skipped_no_book.fetch_add(1, Ordering::Relaxed);
            return Ok(());
        }

        self.metrics.evaluations.fetch_add(1, Ordering::Relaxed);

        let mid = 0.5 * (best_bid + best_ask);
        let spread_bps = ((best_ask - best_bid) / mid) * 10_000.0;
        if spread_bps > self.cfg.max_spread_bps {
            self.metrics.skipped_spread.fetch_add(1, Ordering::Relaxed);
            return Ok(());
        }

        let imbalance = match compute_imbalance(&book, mid, self.cfg.imbalance_bps) {
            Some(v) => v,
            None => {
                self.metrics
                    .skipped_imbalance
                    .fetch_add(1, Ordering::Relaxed);
                return Ok(());
            }
        };

        if imbalance.abs() < self.cfg.min_imbalance {
            self.metrics
                .skipped_imbalance
                .fetch_add(1, Ordering::Relaxed);
            return Ok(());
        }

        let now = Instant::now();
        if let Some(last) = self.last_trade_at.read().get(token_id) {
            if now.duration_since(*last).as_millis() < self.cfg.cooldown_ms as u128 {
                self.metrics
                    .skipped_cooldown
                    .fetch_add(1, Ordering::Relaxed);
                return Ok(());
            }
        }

        let (cash_usdc, position_shares) = {
            let ledger = self.state.vault.ledger.lock().await;
            let pos = ledger
                .positions
                .get(token_id)
                .map(|p| p.shares)
                .unwrap_or(0.0);
            (ledger.cash_usdc, pos)
        };

        let ask_top_usd = book.asks.first().map(|o| o.price * o.size).unwrap_or(0.0);
        let bid_top_usd = book.bids.first().map(|o| o.price * o.size).unwrap_or(0.0);

        let (side, price, notional) = if imbalance > 0.0 {
            if position_shares > 0.0 {
                return Ok(());
            }
            if ask_top_usd < self.cfg.min_top_usd {
                self.metrics
                    .skipped_liquidity
                    .fetch_add(1, Ordering::Relaxed);
                return Ok(());
            }

            let max_by_cash = cash_usdc * self.cfg.max_trade_pct_cash;
            let notional = self.cfg.trade_notional_usd.min(max_by_cash);
            if notional < self.cfg.min_trade_usd {
                self.metrics
                    .skipped_liquidity
                    .fetch_add(1, Ordering::Relaxed);
                return Ok(());
            }

            (OrderSide::Buy, best_ask, notional)
        } else {
            if position_shares <= 0.0 {
                return Ok(());
            }
            if bid_top_usd < self.cfg.min_top_usd {
                self.metrics
                    .skipped_liquidity
                    .fetch_add(1, Ordering::Relaxed);
                return Ok(());
            }

            let position_value = position_shares * best_bid;
            let notional = self.cfg.trade_notional_usd.min(position_value);
            if notional < self.cfg.min_trade_usd {
                self.metrics
                    .skipped_liquidity
                    .fetch_add(1, Ordering::Relaxed);
                return Ok(());
            }

            (OrderSide::Sell, best_bid, notional)
        };

        let req = OrderRequest {
            client_order_id: Uuid::new_v4().to_string(),
            token_id: token_id.to_string(),
            side,
            price,
            notional_usdc: notional,
            tif: TimeInForce::Ioc,
            market_slug: Some(meta.market_slug.clone()),
            outcome: Some(meta.outcome.clone()),
        };

        let ack = match self.exec.place_order(req.clone()).await {
            Ok(ack) => ack,
            Err(e) => {
                self.metrics.errors.fetch_add(1, Ordering::Relaxed);
                let mut ledger = self.state.vault.ledger.lock().await;
                ledger.record_reject();
                return Err(e);
            }
        };

        let (cash_usdc, total_shares) = match req.side {
            OrderSide::Buy => {
                let mut ledger = self.state.vault.ledger.lock().await;
                ledger.apply_buy(
                    &req.token_id,
                    req.outcome.as_deref().unwrap_or(""),
                    ack.filled_price,
                    ack.filled_notional_usdc,
                    ack.fees_usdc,
                );
                if ack.filled_notional_usdc < req.notional_usdc {
                    ledger.record_partial_fill();
                }
                let cash = ledger.cash_usdc;
                drop(ledger);
                let total_shares = self.state.vault.shares.lock().await.total_shares;
                (cash, total_shares)
            }
            OrderSide::Sell => {
                let mut ledger = self.state.vault.ledger.lock().await;
                ledger.apply_sell(
                    &req.token_id,
                    ack.filled_price,
                    ack.filled_notional_usdc,
                    ack.fees_usdc,
                );
                if ack.filled_notional_usdc < req.notional_usdc {
                    ledger.record_partial_fill();
                }
                let cash = ledger.cash_usdc;
                drop(ledger);
                let total_shares = self.state.vault.shares.lock().await.total_shares;
                (cash, total_shares)
            }
        };

        let _ = self
            .state
            .vault
            .db
            .upsert_state(cash_usdc, total_shares, ack.filled_at)
            .await;

        let (nav_usdc, positions_value_usdc, nav_per_share) = {
            let ledger = self.state.vault.ledger.lock().await;
            let nav = approximate_nav_usdc(&ledger);
            let pos_v = (nav - ledger.cash_usdc).max(0.0);
            let nav_ps = if total_shares > 0.0 {
                nav / total_shares
            } else {
                1.0
            };
            (nav, pos_v, nav_ps)
        };

        let side_str = match req.side {
            OrderSide::Buy => "BUY",
            OrderSide::Sell => "SELL",
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
                side: Some(side_str.to_string()),
                price: Some(ack.filled_price),
                notional_usdc: Some(ack.filled_notional_usdc),
                strategy: Some("ORDERFLOW_PAPER".to_string()),
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
                source: "trade:ORDERFLOW_PAPER".to_string(),
            })
            .await;

        self.last_trade_at.write().insert(token_id.to_string(), now);
        self.metrics.trades.fetch_add(1, Ordering::Relaxed);

        info!(
            token_id = %req.token_id,
            market_slug = %meta.market_slug,
            side = %side_str,
            price = ack.filled_price,
            notional = ack.filled_notional_usdc,
            imbalance = imbalance,
            "ORDERFLOW paper trade"
        );

        Ok(())
    }

    async fn run(self: Arc<Self>, mut rx: mpsc::Receiver<String>) {
        let mut last_log = Instant::now();

        while let Some(token_id) = rx.recv().await {
            if let Err(e) = self.handle_update(&token_id).await {
                self.metrics.errors.fetch_add(1, Ordering::Relaxed);
                debug!(error = %e, token_id = %token_id, "Orderflow paper update failed");
            }

            if last_log.elapsed() >= Duration::from_secs(self.cfg.log_interval_sec) {
                let summary = self.metrics.summary();
                info!(
                    updates = summary.updates,
                    evaluations = summary.evaluations,
                    trades = summary.trades,
                    skipped_no_book = summary.skipped_no_book,
                    skipped_spread = summary.skipped_spread,
                    skipped_imbalance = summary.skipped_imbalance,
                    skipped_cooldown = summary.skipped_cooldown,
                    queue_drops = summary.queue_drops,
                    "ORDERFLOW paper stats"
                );
                last_log = Instant::now();
            }
        }
    }
}

pub async fn spawn_orderflow_paper_engine(
    state: Arc<AppState>,
    cfg: OrderflowPaperConfig,
) -> Result<Arc<OrderflowPaperMetrics>> {
    let (tokens, token_ids) = load_universe(&cfg).await?;
    if token_ids.is_empty() {
        warn!("ORDERFLOW paper: no tokens discovered; engine not started");
        return Ok(Arc::new(OrderflowPaperMetrics::default()));
    }

    let poll_interval = cfg.poll_interval_ms;
    let staleness = StalenessConfig {
        max_stale_ms: cfg.book_max_stale_ms,
        hard_stale_ms: cfg.book_max_stale_ms.saturating_mul(4),
    };

    if let Some(hft) = state.hft_book_cache.as_ref() {
        hft.set_universe(token_ids.clone()).await;
    } else {
        for token in &token_ids {
            state.polymarket_market_ws.request_subscribe(token);
        }
    }

    let (tx, rx) = mpsc::channel::<String>(cfg.event_queue_size);

    if let Some(hft) = state.hft_book_cache.as_ref() {
        let metrics = Arc::new(OrderflowPaperMetrics::default());
        for token_id in &token_ids {
            if let Some(mut update_rx) = hft.subscribe_updates(token_id) {
                let tx = tx.clone();
                let token = token_id.clone();
                let metrics_clone = metrics.clone();
                tokio::spawn(async move {
                    loop {
                        if update_rx.changed().await.is_err() {
                            break;
                        }
                        if tx.try_send(token.clone()).is_err() {
                            metrics_clone.queue_drops.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                });
            }
        }

        let engine = Arc::new(OrderflowPaperEngine {
            state,
            cfg,
            exec: Arc::new(PaperExecutionAdapter::default()),
            metrics: metrics.clone(),
            tokens,
            last_trade_at: RwLock::new(HashMap::new()),
            staleness,
        });

        tokio::spawn(engine.run(rx));
        return Ok(metrics);
    }

    let metrics = Arc::new(OrderflowPaperMetrics::default());
    let poll_tokens = token_ids.clone();
    let poll_tx = tx.clone();
    let poll_metrics = metrics.clone();
    let poll_interval = poll_interval;
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_millis(poll_interval));
        loop {
            interval.tick().await;
            for token in &poll_tokens {
                if poll_tx.try_send(token.clone()).is_err() {
                    poll_metrics.queue_drops.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
    });

    let engine = Arc::new(OrderflowPaperEngine {
        state,
        cfg,
        exec: Arc::new(PaperExecutionAdapter::default()),
        metrics: metrics.clone(),
        tokens,
        last_trade_at: RwLock::new(HashMap::new()),
        staleness,
    });

    tokio::spawn(engine.run(rx));
    Ok(metrics)
}

async fn load_universe(
    cfg: &OrderflowPaperConfig,
) -> Result<(HashMap<String, TokenInfo>, Vec<String>)> {
    let mut scraper = PolymarketScraper::new();
    let mut offset = 0usize;
    let mut candidates = Vec::new();

    loop {
        let response = scraper
            .fetch_gamma_markets(cfg.gamma_page_size, offset)
            .await
            .context("fetch gamma markets")?;

        if response.data.is_empty() {
            break;
        }

        let batch_len = response.data.len();
        for market in response.data {
            if market.closed || !market.active {
                continue;
            }
            let liquidity = market.liquidity.unwrap_or(0.0);
            let volume = market.volume.unwrap_or(0.0);
            if liquidity < cfg.min_liquidity_usd || volume < cfg.min_volume_usd {
                continue;
            }
            if market.clob_token_ids.is_empty() || market.outcomes.is_empty() {
                continue;
            }
            if market.clob_token_ids.len() != market.outcomes.len() {
                continue;
            }
            candidates.push(market);
        }

        offset += cfg.gamma_page_size;
        if cfg.max_markets > 0 && candidates.len() >= cfg.max_markets.saturating_mul(2) {
            break;
        }
        if batch_len < cfg.gamma_page_size {
            break;
        }
    }

    candidates.sort_by(|a, b| {
        let la = a.liquidity.unwrap_or(0.0);
        let lb = b.liquidity.unwrap_or(0.0);
        lb.partial_cmp(&la).unwrap_or(std::cmp::Ordering::Equal)
    });

    let selected = if cfg.max_markets > 0 {
        candidates
            .into_iter()
            .take(cfg.max_markets)
            .collect::<Vec<_>>()
    } else {
        candidates
    };

    let selected_len = selected.len();
    let mut token_map = HashMap::new();
    let mut token_ids = Vec::new();

    for market in selected {
        for (idx, token_id) in market.clob_token_ids.iter().enumerate() {
            let outcome = market
                .outcomes
                .get(idx)
                .cloned()
                .unwrap_or_else(|| "Unknown".to_string());
            let token_id = token_id.trim().to_string();
            if token_id.is_empty() {
                continue;
            }
            if token_map.contains_key(&token_id) {
                continue;
            }
            token_ids.push(token_id.clone());
            token_map.insert(
                token_id.clone(),
                TokenInfo {
                    token_id,
                    market_slug: market.slug.clone(),
                    outcome,
                },
            );
        }
    }

    info!(
        markets = selected_len,
        tokens = token_ids.len(),
        min_liquidity = cfg.min_liquidity_usd,
        min_volume = cfg.min_volume_usd,
        "ORDERFLOW paper universe loaded"
    );

    Ok((token_map, token_ids))
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

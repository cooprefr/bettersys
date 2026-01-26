//! Latency Arbitrage Backtest Runner
//!
//! Runs the latency arbitrage strategy against real Polymarket data from the signals database.
//!
//! Usage:
//!   cargo run --bin latency_arb_backtest -- [OPTIONS]
//!
//! Options:
//!   --bankroll <USD>     Initial bankroll (default: 10000)
//!   --min-edge <PCT>     Minimum effective edge (default: 0.005)
//!   --kelly <FRAC>       Kelly fraction (default: 0.10)
//!   --events <N>         Max events to process (default: 0 = all)
//!   --db <PATH>          Database path (default: betterbot_signals.db)
//!   --asset <NAME>       Filter by asset: btc, eth, sol, xrp (default: btc)
//!   --synthetic          Use synthetic data instead of real data

use betterbot_backend::backtest_v2::{
    BacktestConfig, BacktestOrchestrator, BookSnapshot, CancelAck, Event, FillNotification,
    HistoricalDataContract, Level, Nanos, OrderAck, OrderReject, Side, Strategy, StrategyCancel,
    StrategyContext, StrategyOrder, StrategyParams, TimerEvent, TimestampedEvent, TradePrint,
    VecFeed, NANOS_PER_MILLI, NANOS_PER_SEC,
};
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::env;

// =============================================================================
// STANDALONE STRATEGY IMPLEMENTATION
// =============================================================================

/// Backtest statistics
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BacktestStats {
    pub book_updates: u64,
    pub trade_prints: u64,
    pub signals_generated: u64,
    pub trades_attempted: u64,
    pub fills: u64,
    pub rejects: u64,
    pub total_volume: f64,
    pub total_fees: f64,
    pub realized_pnl: f64,
    pub gross_profit: f64,
    pub gross_loss: f64,
    pub wins: u64,
    pub losses: u64,
    pub avg_effective_edge: f64,
    pub avg_fill_probability: f64,
    edge_sum: f64,
    fill_prob_sum: f64,
    trade_count: u64,
    pub pnl_history: Vec<f64>,
    pub max_drawdown: f64,
    peak_pnl: f64,
}

impl BacktestStats {
    pub fn win_rate(&self) -> f64 {
        let total = self.wins + self.losses;
        if total > 0 {
            self.wins as f64 / total as f64
        } else {
            0.0
        }
    }

    pub fn profit_factor(&self) -> f64 {
        if self.gross_loss.abs() > 1e-9 {
            self.gross_profit / self.gross_loss.abs()
        } else if self.gross_profit > 0.0 {
            f64::INFINITY
        } else {
            0.0
        }
    }

    pub fn sharpe_ratio(&self) -> Option<f64> {
        if self.pnl_history.len() < 10 {
            return None;
        }
        let n = self.pnl_history.len() as f64;
        let mean = self.pnl_history.iter().sum::<f64>() / n;
        let variance = self
            .pnl_history
            .iter()
            .map(|x| (x - mean).powi(2))
            .sum::<f64>()
            / n;
        let stddev = variance.sqrt();
        if stddev > 1e-9 {
            Some((mean / stddev) * (252.0_f64).sqrt())
        } else {
            None
        }
    }

    fn record_fill(&mut self, pnl: f64, fee: f64, volume: f64, edge: f64, fill_prob: f64) {
        self.fills += 1;
        self.total_fees += fee;
        self.total_volume += volume;
        self.realized_pnl += pnl - fee;

        if pnl > 0.0 {
            self.wins += 1;
            self.gross_profit += pnl;
        } else {
            self.losses += 1;
            self.gross_loss += pnl.abs();
        }

        self.edge_sum += edge;
        self.fill_prob_sum += fill_prob;
        self.trade_count += 1;
        self.avg_effective_edge = self.edge_sum / self.trade_count as f64;
        self.avg_fill_probability = self.fill_prob_sum / self.trade_count as f64;

        self.pnl_history.push(pnl - fee);

        if self.realized_pnl > self.peak_pnl {
            self.peak_pnl = self.realized_pnl;
        }
        let drawdown = self.peak_pnl - self.realized_pnl;
        if drawdown > self.max_drawdown {
            self.max_drawdown = drawdown;
        }
    }
}

/// Market state for backtest
#[derive(Debug, Clone)]
struct MarketState {
    token_id_yes: String,
    token_id_no: String,
    topic: String,
    private_prob: f64,
    market_prob: f64,
    taker_imbalance: f64,
    recent_trades: VecDeque<(Nanos, f64, f64)>,
    last_trade_at: Nanos,
    pending_order_id: Option<u64>,
    entry_price: Option<f64>,
    entry_edge: f64,
    entry_fill_prob: f64,
}

/// Standalone latency arbitrage strategy for backtesting
pub struct LatencyArbStrategy {
    name: String,
    bankroll: f64,
    min_effective_edge: f64,
    kelly_fraction: f64,
    max_clip_usd: f64,
    min_clip_usd: f64,
    fee_rate_taker: f64,
    max_spread_bps: f64,
    cooldown_ns: Nanos,
    markets: HashMap<String, MarketState>,
    latency_samples: VecDeque<u64>,
    baseline_latency: u64,
    order_counter: u64,
    pub stats: BacktestStats,
    eval_interval_ns: Nanos,
}

impl LatencyArbStrategy {
    pub fn new(params: &StrategyParams) -> Self {
        let bankroll = params.get_or("bankroll", 10_000.0);
        let eval_interval_ms = params.get_or("eval_interval_ms", 100.0) as u64;

        Self {
            name: "LatencyArb".into(),
            bankroll,
            min_effective_edge: params.get_or("min_effective_edge", 0.005),
            kelly_fraction: params.get_or("kelly_fraction", 0.10),
            max_clip_usd: params.get_or("max_clip_usd", 500.0),
            min_clip_usd: params.get_or("min_clip_usd", 10.0),
            fee_rate_taker: params.get_or("fee_rate_taker", 0.02),
            max_spread_bps: params.get_or("max_spread_bps", 300.0),
            cooldown_ns: (params.get_or("cooldown_sec", 5.0) as i64) * NANOS_PER_SEC,
            markets: HashMap::new(),
            latency_samples: VecDeque::with_capacity(1000),
            baseline_latency: 50_000,
            order_counter: 0,
            stats: BacktestStats::default(),
            eval_interval_ns: (eval_interval_ms as i64) * NANOS_PER_MILLI,
        }
    }

    pub fn register_market(&mut self, token_yes: &str, token_no: &str, topic: &str) {
        let state = MarketState {
            token_id_yes: token_yes.to_string(),
            token_id_no: token_no.to_string(),
            topic: topic.to_string(),
            private_prob: 0.5,
            market_prob: 0.5,
            taker_imbalance: 0.0,
            recent_trades: VecDeque::with_capacity(100),
            last_trade_at: 0,
            pending_order_id: None,
            entry_price: None,
            entry_edge: 0.0,
            entry_fill_prob: 0.0,
        };
        self.markets.insert(token_yes.to_string(), state.clone());
        let mut no_state = state;
        std::mem::swap(&mut no_state.token_id_yes, &mut no_state.token_id_no);
        self.markets.insert(token_no.to_string(), no_state);
    }

    fn generate_client_id(&mut self) -> String {
        self.order_counter += 1;
        format!("latarb_{}", self.order_counter)
    }

    fn latency_p95(&self) -> u64 {
        if self.latency_samples.is_empty() {
            return self.baseline_latency * 3;
        }
        let mut sorted: Vec<u64> = self.latency_samples.iter().copied().collect();
        sorted.sort_unstable();
        sorted[(sorted.len() * 95) / 100]
    }

    fn fill_probability(&self) -> f64 {
        let p95 = self.latency_p95();
        let max_lat: u64 = 500_000; // 500ms
        if p95 >= max_lat {
            return 0.1;
        }
        let ratio = p95 as f64 / max_lat as f64;
        (1.0 - ratio.sqrt()).clamp(0.1, 0.95)
    }

    fn update_microstructure(&mut self, token_id: &str, book: &BookSnapshot, now: Nanos) {
        let Some(state) = self.markets.get_mut(token_id) else {
            return;
        };

        if let Some(mid) = book.mid_price() {
            state.market_prob = mid;
        }

        // Update taker imbalance from recent trades
        let window_start = now.saturating_sub(60 * NANOS_PER_SEC);
        let best_ask = book.best_ask().map(|l| l.price).unwrap_or(1.0);
        let best_bid = book.best_bid().map(|l| l.price).unwrap_or(0.0);

        let recent: Vec<_> = state
            .recent_trades
            .iter()
            .filter(|(t, _, _)| *t >= window_start)
            .collect();

        if !recent.is_empty() {
            let buy_vol: f64 = recent
                .iter()
                .filter(|(_, p, _)| *p >= best_ask * 0.999)
                .map(|(_, _, s)| s)
                .sum();
            let sell_vol: f64 = recent
                .iter()
                .filter(|(_, p, _)| *p <= best_bid * 1.001)
                .map(|(_, _, s)| s)
                .sum();
            let total = buy_vol + sell_vol;
            if total > 0.0 {
                state.taker_imbalance = (buy_vol - sell_vol) / total;
            }
        }

        // Update private probability based on microstructure
        let bias = state.taker_imbalance * 0.05; // Max 5% shift
        state.private_prob = (state.market_prob + bias).clamp(0.01, 0.99);
    }

    fn compute_edge(&self, token_id: &str, book: &BookSnapshot) -> Option<(f64, f64, f64)> {
        let state = self.markets.get(token_id)?;

        let best_bid = book.best_bid().map(|l| l.price)?;
        let best_ask = book.best_ask().map(|l| l.price)?;
        let spread = best_ask - best_bid;
        let mid = (best_bid + best_ask) / 2.0;
        let spread_bps = if mid > 0.0 {
            (spread / mid) * 10_000.0
        } else {
            10_000.0
        };

        if spread_bps > self.max_spread_bps {
            return None;
        }

        let raw_edge = state.private_prob - state.market_prob;
        let fee_cost = self.fee_rate_taker;
        let expected_slippage = spread * 0.5;
        let fill_prob = self.fill_probability();

        let adjusted_edge = raw_edge.abs() - fee_cost - expected_slippage;
        let effective_edge = adjusted_edge * fill_prob;

        if effective_edge > self.min_effective_edge {
            Some((raw_edge, effective_edge, fill_prob))
        } else {
            None
        }
    }

    fn compute_size(&self, edge: f64) -> f64 {
        let raw_kelly = edge.abs() / 0.5_f64.max(0.01);
        let fractional = raw_kelly * self.kelly_fraction;
        let sized = fractional * self.bankroll * self.fill_probability();
        sized.min(self.max_clip_usd).max(self.min_clip_usd)
    }

    fn evaluate(&mut self, ctx: &mut StrategyContext, token_id: &str, book: &BookSnapshot) {
        let now = ctx.timestamp;

        let last_trade = self
            .markets
            .get(token_id)
            .map(|s| s.last_trade_at)
            .unwrap_or(0);
        if now < last_trade + self.cooldown_ns {
            return;
        }

        if self
            .markets
            .get(token_id)
            .and_then(|s| s.pending_order_id)
            .is_some()
        {
            return;
        }

        let Some((raw_edge, eff_edge, fill_prob)) = self.compute_edge(token_id, book) else {
            return;
        };

        let size = self.compute_size(eff_edge);
        if size < self.min_clip_usd {
            return;
        }

        let is_buy_yes = raw_edge > 0.0;

        // For IOC to fill, we need to cross the spread aggressively
        let (side, price) = if is_buy_yes {
            // Buying: cross the ask by pricing at or above best ask
            let ask = book.best_ask().map(|l| l.price).unwrap_or(0.5);
            (Side::Buy, ask + 0.01) // Cross the spread
        } else {
            // Selling: cross the bid by pricing at or below best bid
            let bid = book.best_bid().map(|l| l.price).unwrap_or(0.5);
            (Side::Sell, bid - 0.01) // Cross the spread
        };

        let order = StrategyOrder::limit(
            self.generate_client_id(),
            token_id,
            side,
            price,
            size / price.max(0.01),
        )
        .ioc();

        self.stats.trades_attempted += 1;
        self.stats.signals_generated += 1;

        if let Ok(order_id) = ctx.orders.send_order(order) {
            if let Some(market) = self.markets.get_mut(token_id) {
                market.pending_order_id = Some(order_id);
                market.entry_price = Some(price);
                market.entry_edge = eff_edge;
                market.entry_fill_prob = fill_prob;
            }

            let latency = self.baseline_latency + (self.order_counter % 50) * 1000;
            self.latency_samples.push_back(latency);
            while self.latency_samples.len() > 1000 {
                self.latency_samples.pop_front();
            }
        }
    }
}

impl Strategy for LatencyArbStrategy {
    fn on_book_update(&mut self, ctx: &mut StrategyContext, book: &BookSnapshot) {
        self.stats.book_updates += 1;
        self.update_microstructure(&book.token_id, book, ctx.timestamp);
        self.evaluate(ctx, &book.token_id, book);
    }

    fn on_trade(&mut self, ctx: &mut StrategyContext, trade: &TradePrint) {
        self.stats.trade_prints += 1;
        if let Some(state) = self.markets.get_mut(&trade.token_id) {
            state
                .recent_trades
                .push_back((ctx.timestamp, trade.price, trade.size));
            while state.recent_trades.len() > 100 {
                state.recent_trades.pop_front();
            }
        }
    }

    fn on_timer(&mut self, ctx: &mut StrategyContext, _timer: &TimerEvent) {
        ctx.orders
            .schedule_timer(self.eval_interval_ns, Some("eval".into()));
    }

    fn on_order_ack(&mut self, _ctx: &mut StrategyContext, _ack: &OrderAck) {}

    fn on_order_reject(&mut self, _ctx: &mut StrategyContext, reject: &OrderReject) {
        self.stats.rejects += 1;
        for state in self.markets.values_mut() {
            if state.pending_order_id == Some(reject.order_id) {
                state.pending_order_id = None;
                state.entry_price = None;
            }
        }
    }

    fn on_fill(&mut self, ctx: &mut StrategyContext, fill: &FillNotification) {
        let mut entry_edge = 0.0;
        let mut entry_fill_prob = 0.0;

        for state in self.markets.values_mut() {
            if state.pending_order_id == Some(fill.order_id) {
                entry_edge = state.entry_edge;
                entry_fill_prob = state.entry_fill_prob;

                if fill.leaves_qty <= 0.0 {
                    state.pending_order_id = None;
                    state.last_trade_at = ctx.timestamp;
                    state.entry_price = None;
                }
                break;
            }
        }

        let notional = fill.price * fill.size;
        let pnl = entry_edge * notional;
        self.stats
            .record_fill(pnl, fill.fee, notional, entry_edge, entry_fill_prob);
    }

    fn on_cancel_ack(&mut self, _ctx: &mut StrategyContext, ack: &CancelAck) {
        for state in self.markets.values_mut() {
            if state.pending_order_id == Some(ack.order_id) {
                state.pending_order_id = None;
                state.entry_price = None;
            }
        }
    }

    fn on_start(&mut self, ctx: &mut StrategyContext) {
        ctx.orders
            .schedule_timer(self.eval_interval_ns, Some("eval".into()));
    }

    fn on_stop(&mut self, _ctx: &mut StrategyContext) {
        if self.stats.trade_count > 0 {
            self.stats.avg_effective_edge = self.stats.edge_sum / self.stats.trade_count as f64;
            self.stats.avg_fill_probability =
                self.stats.fill_prob_sum / self.stats.trade_count as f64;
        }
    }

    fn name(&self) -> &str {
        &self.name
    }
}

// =============================================================================
// DATA LOADING FROM DATABASE
// =============================================================================

/// Dome order event from database
#[derive(Debug, Clone)]
struct DomeOrderRow {
    timestamp: i64,
    market_slug: String,
    side: String,
    price: f64,
    shares: f64,
    outcome: String,
    user: String,
}

/// Load real Polymarket data from the signals database
fn load_real_data(db_path: &str, asset: &str, max_events: u64) -> Vec<TimestampedEvent> {
    println!("Opening database: {}", db_path);

    let conn = match Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_ONLY) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Failed to open database: {}", e);
            return vec![];
        }
    };

    // Query dome_order_events for the specified asset
    let pattern = format!("{}-updown-15m%", asset);
    let limit_clause = if max_events > 0 {
        format!("LIMIT {}", max_events)
    } else {
        String::new()
    };

    let query = format!(
        r#"
        SELECT 
            timestamp,
            market_slug,
            json_extract(payload_json, '$.side') as side,
            json_extract(payload_json, '$.price') as price,
            json_extract(payload_json, '$.shares_normalized') as shares,
            json_extract(payload_json, '$.token_label') as outcome,
            user
        FROM dome_order_events
        WHERE market_slug LIKE ?
        ORDER BY timestamp ASC
        {}
        "#,
        limit_clause
    );

    let mut stmt = match conn.prepare(&query) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Failed to prepare query: {}", e);
            return vec![];
        }
    };

    let rows: Vec<DomeOrderRow> = stmt
        .query_map([&pattern], |row| {
            Ok(DomeOrderRow {
                timestamp: row.get(0)?,
                market_slug: row.get(1)?,
                side: row.get::<_, String>(2).unwrap_or_default(),
                price: row.get::<_, f64>(3).unwrap_or(0.5),
                shares: row.get::<_, f64>(4).unwrap_or(0.0),
                outcome: row.get::<_, String>(5).unwrap_or_default(),
                user: row.get(6)?,
            })
        })
        .ok()
        .map(|iter| iter.filter_map(|r| r.ok()).collect())
        .unwrap_or_default();

    println!("Loaded {} orders from database", rows.len());

    // Group orders by market_slug to build synthetic book states
    let mut events: Vec<TimestampedEvent> = Vec::with_capacity(rows.len() * 2);
    let mut market_books: HashMap<String, (Vec<Level>, Vec<Level>)> = HashMap::new();
    let mut seq = 0u64;

    for row in rows {
        let time_ns = row.timestamp * NANOS_PER_SEC;

        // Create trade print event from the order
        let aggressor_side = if row.side.eq_ignore_ascii_case("BUY") {
            Side::Buy
        } else {
            Side::Sell
        };

        let trade_event = Event::TradePrint {
            token_id: format!("{}_{}", row.market_slug, row.outcome),
            price: row.price,
            size: row.shares,
            aggressor_side,
            trade_id: Some(format!("dome_{}", seq)),
        };

        events.push(TimestampedEvent {
            time: time_ns,
            source_time: time_ns,
            seq,
            source: 0,
            event: trade_event,
        });
        seq += 1;

        // Build/update synthetic orderbook from trade flow
        let book_key = row.market_slug.clone();
        let (bids, asks) = market_books.entry(book_key.clone()).or_insert_with(|| {
            // Initialize with synthetic levels around 0.5
            let init_bids = vec![
                Level::new(0.48, 100.0),
                Level::new(0.47, 200.0),
                Level::new(0.46, 300.0),
            ];
            let init_asks = vec![
                Level::new(0.52, 100.0),
                Level::new(0.53, 200.0),
                Level::new(0.54, 300.0),
            ];
            (init_bids, init_asks)
        });

        // Adjust book based on trade (simplified model)
        if aggressor_side == Side::Buy {
            // Buying pushes price up - adjust asks
            if let Some(ask) = asks.first_mut() {
                ask.price = row.price;
                ask.size = (ask.size - row.shares).max(10.0);
            }
            // Add depth behind
            if asks.len() < 5 {
                asks.push(Level::new(row.price + 0.01, 100.0));
            }
        } else {
            // Selling pushes price down - adjust bids
            if let Some(bid) = bids.first_mut() {
                bid.price = row.price;
                bid.size = (bid.size - row.shares).max(10.0);
            }
            if bids.len() < 5 {
                bids.push(Level::new(row.price - 0.01, 100.0));
            }
        }

        // Sort bids desc, asks asc
        bids.sort_by(|a, b| b.price.partial_cmp(&a.price).unwrap());
        asks.sort_by(|a, b| a.price.partial_cmp(&b.price).unwrap());

        // Emit book snapshot event
        let book_event = Event::L2BookSnapshot {
            token_id: format!("{}_{}", row.market_slug, row.outcome),
            bids: bids.clone(),
            asks: asks.clone(),
            exchange_seq: seq,
        };

        events.push(TimestampedEvent {
            time: time_ns + NANOS_PER_MILLI, // Slightly after trade
            source_time: time_ns,
            seq,
            source: 1,
            event: book_event,
        });
        seq += 1;
    }

    // Sort by time
    events.sort_by_key(|e| e.time);
    events
}

// =============================================================================
// MAIN
// =============================================================================

fn main() {
    let args: Vec<String> = env::args().collect();
    let bankroll = parse_arg(&args, "--bankroll", 10_000.0);
    let min_edge = parse_arg(&args, "--min-edge", 0.005);
    let kelly_fraction = parse_arg(&args, "--kelly", 0.10);
    let max_events = parse_arg(&args, "--events", 0.0) as u64; // 0 = all
    let seed = parse_arg(&args, "--seed", 42.0) as u64;
    let use_synthetic = args.iter().any(|a| a == "--synthetic");
    let db_path = parse_str_arg(&args, "--db", "betterbot_signals.db");
    let asset = parse_str_arg(&args, "--asset", "btc");

    println!("=== Latency Arbitrage Backtest ===");
    println!("Bankroll:      ${:.0}", bankroll);
    println!("Min Edge:      {:.2}%", min_edge * 100.0);
    println!("Kelly Frac:    {:.0}%", kelly_fraction * 100.0);
    if use_synthetic {
        println!("Data Source:   SYNTHETIC");
    } else {
        println!(
            "Data Source:   REAL ({} from {})",
            asset.to_uppercase(),
            db_path
        );
    }
    println!();

    let params = StrategyParams::new()
        .with_param("bankroll", bankroll)
        .with_param("min_effective_edge", min_edge)
        .with_param("kelly_fraction", kelly_fraction)
        .with_param("max_clip_usd", 500.0)
        .with_param("min_clip_usd", 10.0)
        .with_param("eval_interval_ms", 100.0)
        .with_param("max_spread_bps", 500.0) // Wider for real data
        .with_param("fee_rate_taker", 0.02);

    let mut strategy = LatencyArbStrategy::new(&params);

    // Load data
    let events = if use_synthetic {
        let n = if max_events > 0 { max_events } else { 100_000 };
        println!("Generating {} synthetic events...", n);
        generate_synthetic_data(n, seed)
    } else {
        load_real_data(&db_path, &asset, max_events)
    };

    if events.is_empty() {
        eprintln!("No events loaded! Check database path and asset filter.");
        return;
    }

    println!("Loaded {} events", events.len());

    // Extract unique token IDs from events and register markets
    let mut token_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    for e in &events {
        match &e.event {
            Event::TradePrint { token_id, .. } => {
                token_ids.insert(token_id.clone());
            }
            Event::L2BookSnapshot { token_id, .. } => {
                token_ids.insert(token_id.clone());
            }
            _ => {}
        }
    }
    println!("Unique tokens: {}", token_ids.len());

    for token_id in &token_ids {
        // Parse token_id format: "market_slug_Outcome"
        if let Some(idx) = token_id.rfind('_') {
            let slug = &token_id[..idx];
            let outcome = &token_id[idx + 1..];
            let other_outcome = if outcome == "Up" { "Down" } else { "Up" };
            let other_token = format!("{}_{}", slug, other_outcome);
            strategy.register_market(token_id, &other_token, "crypto");
        }
    }
    println!();

    let mut feed = VecFeed::new("polymarket", events);

    let config = BacktestConfig {
        strategy_params: params,
        seed,
        data_contract: HistoricalDataContract::polymarket_15m_updown_hybrid_snapshots_and_trades(),
        max_events: if max_events > 0 { max_events } else { u64::MAX },
        verbose: false,
        ..Default::default()
    };

    let mut orchestrator = BacktestOrchestrator::new(config);
    orchestrator.load_feed(&mut feed).unwrap();

    println!("Running backtest...");
    let start = std::time::Instant::now();
    let results = orchestrator.run(&mut strategy).unwrap();
    let elapsed = start.elapsed();

    println!();
    println!("=== Backtest Results ===");
    println!("Duration:           {:.2}s", elapsed.as_secs_f64());
    println!("Events processed:   {}", results.events_processed);
    println!(
        "Events/sec:         {:.0}",
        results.events_processed as f64 / elapsed.as_secs_f64()
    );
    println!();

    println!("=== Strategy Statistics ===");
    let stats = &strategy.stats;
    println!("Book updates:       {}", stats.book_updates);
    println!("Trade prints:       {}", stats.trade_prints);
    println!("Signals generated:  {}", stats.signals_generated);
    println!("Trades attempted:   {}", stats.trades_attempted);
    println!("Fills:              {}", stats.fills);
    println!("Rejects:            {}", stats.rejects);
    println!();

    println!("=== Performance Metrics ===");
    println!("Total volume:       ${:.2}", stats.total_volume);
    println!("Total fees:         ${:.2}", stats.total_fees);

    println!();
    println!("=== Data Quality ===");
    println!(
        "Contract:           {} / {}",
        results.data_quality.contract.venue, results.data_quality.contract.market
    );
    println!("Mode:               {:?}", results.data_quality.mode);
    println!(
        "Production grade:   {}",
        results.data_quality.is_production_grade
    );
    if !results.data_quality.reasons.is_empty() {
        println!("Reasons:");
        for r in &results.data_quality.reasons {
            println!("  - {}", r);
        }
    }
    println!("Realized PnL:       ${:.2}", stats.realized_pnl);
    println!("Gross profit:       ${:.2}", stats.gross_profit);
    println!("Gross loss:         ${:.2}", stats.gross_loss);
    println!("Win rate:           {:.1}%", stats.win_rate() * 100.0);
    println!("Profit factor:      {:.2}", stats.profit_factor());
    println!("Max drawdown:       ${:.2}", stats.max_drawdown);
    if let Some(sharpe) = stats.sharpe_ratio() {
        println!("Sharpe ratio:       {:.2}", sharpe);
    }
    println!();

    println!("=== Edge Analysis ===");
    println!(
        "Avg effective edge: {:.3}%",
        stats.avg_effective_edge * 100.0
    );
    println!(
        "Avg fill prob:      {:.1}%",
        stats.avg_fill_probability * 100.0
    );
    println!();

    let roi = if bankroll > 0.0 {
        (stats.realized_pnl / bankroll) * 100.0
    } else {
        0.0
    };
    println!("=== Summary ===");
    println!("Return on Investment: {:.2}%", roi);
    if stats.fills > 0 {
        println!(
            "Avg PnL per trade:    ${:.2}",
            stats.realized_pnl / stats.fills as f64
        );
    }
}

fn parse_arg(args: &[String], flag: &str, default: f64) -> f64 {
    for i in 0..args.len() - 1 {
        if args[i] == flag {
            if let Ok(v) = args[i + 1].parse::<f64>() {
                return v;
            }
        }
    }
    default
}

fn parse_str_arg<'a>(args: &'a [String], flag: &str, default: &'a str) -> &'a str {
    for i in 0..args.len() - 1 {
        if args[i] == flag {
            return &args[i + 1];
        }
    }
    default
}

/// Generate synthetic market data for backtesting
fn generate_synthetic_data(num_events: u64, seed: u64) -> Vec<TimestampedEvent> {
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    let mut events = Vec::with_capacity(num_events as usize);

    let tokens = vec!["TOKEN_YES_1", "TOKEN_YES_2"];
    let mut prices: std::collections::HashMap<&str, f64> = std::collections::HashMap::new();
    for token in &tokens {
        prices.insert(token, 0.50);
    }

    let start_time: Nanos = 1_000_000_000_000; // 1000 seconds in nanos
    let mut current_time = start_time;
    let mut seq = 0u64;

    for _ in 0..num_events {
        // Advance time (10-500ms between events)
        current_time += rng.gen_range(10..500) * NANOS_PER_MILLI;

        // Pick a random token
        let token = tokens[rng.gen_range(0..tokens.len())];
        let price = prices.get_mut(token).unwrap();

        // Random walk the price
        let drift: f64 = rng.gen_range(-0.02..0.02);
        *price = (*price + drift).clamp(0.05, 0.95);

        // Generate order book
        let spread = rng.gen_range(0.01..0.05);
        let mid = *price;
        let best_bid = (mid - spread / 2.0).max(0.01);
        let best_ask = (mid + spread / 2.0).min(0.99);

        // Create book event with multiple levels
        let mut bids = Vec::new();
        let mut asks = Vec::new();

        for i in 0..5 {
            let bid_price = (best_bid - i as f64 * 0.01).max(0.01);
            let ask_price = (best_ask + i as f64 * 0.01).min(0.99);
            let level_size = rng.gen_range(20.0..200.0);

            bids.push(Level::new(bid_price, level_size));
            asks.push(Level::new(ask_price, level_size));
        }

        // Emit book snapshot
        let book_event = Event::L2BookSnapshot {
            token_id: token.to_string(),
            bids,
            asks,
            exchange_seq: seq,
        };

        events.push(TimestampedEvent {
            time: current_time,
            source_time: current_time,
            seq,
            source: 0,
            event: book_event,
        });
        seq += 1;

        // Occasionally emit a trade (30% chance)
        if rng.gen_bool(0.3) {
            let trade_price = if rng.gen_bool(0.5) {
                best_ask // Aggressive buy
            } else {
                best_bid // Aggressive sell
            };
            let trade_size = rng.gen_range(10.0..100.0);
            let aggressor = if trade_price >= best_ask {
                Side::Buy
            } else {
                Side::Sell
            };

            let trade_event = Event::TradePrint {
                token_id: token.to_string(),
                price: trade_price,
                size: trade_size,
                aggressor_side: aggressor,
                trade_id: Some(format!("trade_{}", seq)),
            };

            current_time += rng.gen_range(1..10) * NANOS_PER_MILLI;
            events.push(TimestampedEvent {
                time: current_time,
                source_time: current_time,
                seq,
                source: 0,
                event: trade_event,
            });
            seq += 1;
        }

        // Add microstructure signals occasionally
        if rng.gen_bool(0.1) {
            // Simulate aggressive flow burst
            for _ in 0..rng.gen_range(2..5) {
                let burst_price = if rng.gen_bool(0.7) {
                    best_ask
                } else {
                    best_bid
                };
                let burst_size = rng.gen_range(50.0..200.0);
                let aggressor = if burst_price >= best_ask {
                    Side::Buy
                } else {
                    Side::Sell
                };

                let burst_event = Event::TradePrint {
                    token_id: token.to_string(),
                    price: burst_price,
                    size: burst_size,
                    aggressor_side: aggressor,
                    trade_id: Some(format!("burst_{}", seq)),
                };

                current_time += rng.gen_range(1..5) * NANOS_PER_MILLI;
                events.push(TimestampedEvent {
                    time: current_time,
                    source_time: current_time,
                    seq,
                    source: 0,
                    event: burst_event,
                });
                seq += 1;
            }
        }
    }

    // Sort by time to ensure determinism
    events.sort_by_key(|e| e.time);
    events
}

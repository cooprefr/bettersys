//! Binance Arbitrage Feed - Enhanced market data for 15M arbitrage monitoring.
//!
//! Provides:
//! - PublicTrades stream for tick charts
//! - L1 orderbook for mid price
//! - Latency breakdown history for visualization
//! - Trade history ring buffer

use anyhow::{Context, Result};
use barter_data::{
    exchange::binance::spot::BinanceSpot,
    streams::{reconnect::Event as ReconnectEvent, Streams},
    subscription::{book::OrderBooksL1, trade::PublicTrades},
};
use barter_instrument::instrument::market_data::{
    kind::MarketDataInstrumentKind, MarketDataInstrument,
};
use futures_util::StreamExt;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
    time::Instant,
};
use tokio::sync::broadcast;
use tracing::{debug, info, trace, warn};

const MAX_TRADE_HISTORY: usize = 1000;
const MAX_LATENCY_HISTORY: usize = 300; // 5 min at 1Hz
const MAX_PRICE_HISTORY: usize = 900; // 15 min at 1Hz

/// Individual trade tick from Binance
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradeTick {
    pub ts_ms: i64,
    pub price: f64,
    pub size: f64,
    pub is_buyer_maker: bool,
    pub receive_latency_us: u64,
}

/// Latency sample for history graph
/// Breakdown: exchange_ts → barter_received → handler_entry → handler_complete
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct LatencySample {
    pub ts_ms: i64,
    /// Total: barter_received → handler_complete (what we display as "receive")
    pub receive_us: u64,
    /// Component 1: barter_received → handler_entry (internal propagation)
    pub propagate_us: u64,
    /// Component 2: handler_entry → state_update_complete (our processing)
    pub process_us: u64,
    /// Component 3: exchange_timestamp → barter_received (network + decode, estimated)
    /// Only available if exchange provides a timestamp in the message
    pub network_us: u64,
}

/// Price point with bid/ask spread
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct OhlcPoint {
    pub ts: i64,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub bid: f64,
    pub ask: f64,
}

/// Per-symbol state for arbitrage monitoring
#[derive(Debug, Clone, Default)]
struct ArbSymbolState {
    // L1 orderbook
    best_bid: Option<f64>,
    best_ask: Option<f64>,
    mid_price: Option<f64>,
    last_update_ts: i64,

    // Trade history for tick chart
    trades: VecDeque<TradeTick>,

    // OHLC history (1-second bars)
    ohlc_history: VecDeque<OhlcPoint>,
    current_bar: Option<OhlcPoint>,

    // Latency history for graph
    latency_history: VecDeque<LatencySample>,
}

/// Broadcast event for real-time updates
#[derive(Debug, Clone)]
pub struct ArbUpdateEvent {
    pub symbol: String,
    pub event_type: ArbEventType,
    pub ts_ms: i64,
}

#[derive(Debug, Clone)]
pub enum ArbEventType {
    Trade(TradeTick),
    Quote { bid: f64, ask: f64, mid: f64 },
}

/// Arbitrage feed with trades + quotes
#[derive(Debug, Clone)]
pub struct BinanceArbFeed {
    inner: Arc<RwLock<HashMap<String, ArbSymbolState>>>,
    update_tx: broadcast::Sender<ArbUpdateEvent>,
}

impl BinanceArbFeed {
    pub fn disabled() -> Arc<Self> {
        let (update_tx, _) = broadcast::channel(2048);
        Arc::new(Self {
            inner: Arc::new(RwLock::new(HashMap::new())),
            update_tx,
        })
    }

    pub async fn spawn() -> Result<Arc<Self>> {
        let (update_tx, _) = broadcast::channel(2048);

        let feed = Arc::new(Self {
            inner: Arc::new(RwLock::new(HashMap::new())),
            update_tx,
        });

        // Initialize both L1 and trades streams
        let l1_streams = init_l1_streams().await?;
        let trade_streams = init_trade_streams().await?;

        // Spawn L1 consumer
        let feed_l1 = feed.clone();
        tokio::spawn(async move {
            if let Err(e) = feed_l1.consume_l1(l1_streams).await {
                warn!(error = %e, "binance arb L1 feed stopped");
            }
        });

        // Spawn trades consumer
        let feed_trades = feed.clone();
        tokio::spawn(async move {
            if let Err(e) = feed_trades.consume_trades(trade_streams).await {
                warn!(error = %e, "binance arb trades feed stopped");
            }
        });

        info!("BinanceArbFeed started with L1 + trades streams");
        Ok(feed)
    }

    pub fn subscribe(&self) -> broadcast::Receiver<ArbUpdateEvent> {
        self.update_tx.subscribe()
    }

    /// Get current state snapshot for a symbol
    pub fn get_snapshot(&self, symbol: &str) -> Option<ArbSnapshot> {
        let inner = self.inner.read();
        let state = inner.get(symbol)?;

        Some(ArbSnapshot {
            symbol: symbol.to_string(),
            best_bid: state.best_bid,
            best_ask: state.best_ask,
            mid_price: state.mid_price,
            last_update_ts: state.last_update_ts,
            recent_trades: state.trades.iter().cloned().collect(),
            ohlc_history: state.ohlc_history.iter().cloned().collect(),
            latency_history: state.latency_history.iter().cloned().collect(),
        })
    }

    /// Get all available symbols
    pub fn symbols(&self) -> Vec<String> {
        self.inner.read().keys().cloned().collect()
    }

    async fn consume_l1(
        self: Arc<Self>,
        streams: Streams<
            barter_data::streams::consumer::MarketStreamResult<
                MarketDataInstrument,
                barter_data::subscription::book::OrderBookL1,
            >,
        >,
    ) -> Result<()> {
        let mut joined = streams.select_all();

        while let Some(event) = joined.next().await {
            match event {
                ReconnectEvent::Reconnecting(exchange) => {
                    warn!(?exchange, "binance arb L1 stream reconnecting");
                }
                ReconnectEvent::Item(result) => match result {
                    Ok(market_event) => {
                        // === LATENCY BREAKDOWN ===
                        // T0: Exchange generated the message (time_exchange_ts)
                        // T1: barter-data received and timestamped (time_received)
                        // T2: Our handler entry (handler_entry_ts)
                        // T3: State update complete (after write lock)

                        let handler_entry_ts = chrono::Utc::now();
                        let handler_entry_instant = Instant::now();

                        let symbol = to_symbol(&market_event.instrument);
                        let ts_ms = market_event.time_received.timestamp_millis();

                        // Component 1: barter_received → handler_entry (propagate_us)
                        let propagate_us = handler_entry_ts
                            .signed_duration_since(market_event.time_received)
                            .num_microseconds()
                            .unwrap_or(0)
                            .max(0) as u64;

                        // Component 3: exchange_timestamp → barter_received (network_us)
                        // This measures: exchange generated timestamp → barter-data received
                        // Includes: network transit + WebSocket parsing + barter-data processing
                        let network_us = market_event
                            .time_received
                            .signed_duration_since(market_event.time_exchange)
                            .num_microseconds()
                            .unwrap_or(0)
                            .max(0) as u64;

                        let bid = market_event
                            .kind
                            .best_bid
                            .and_then(|l| l.price.to_string().parse::<f64>().ok());
                        let ask = market_event
                            .kind
                            .best_ask
                            .and_then(|l| l.price.to_string().parse::<f64>().ok());

                        let mid = match (bid, ask) {
                            (Some(b), Some(a)) => Some((b + a) / 2.0),
                            (Some(b), None) => Some(b),
                            (None, Some(a)) => Some(a),
                            _ => None,
                        };

                        // Update state
                        {
                            let mut inner = self.inner.write();
                            let state = inner.entry(symbol.clone()).or_default();

                            state.best_bid = bid;
                            state.best_ask = ask;
                            state.mid_price = mid;
                            state.last_update_ts = ts_ms;

                            // Update OHLC bar
                            if let Some(m) = mid {
                                let bar_ts = ts_ms / 1000; // 1-second bars

                                if let Some(ref mut bar) = state.current_bar {
                                    if bar.ts == bar_ts {
                                        bar.high = bar.high.max(m);
                                        bar.low = bar.low.min(m);
                                        bar.close = m;
                                        if let Some(b) = bid {
                                            bar.bid = b;
                                        }
                                        if let Some(a) = ask {
                                            bar.ask = a;
                                        }
                                    } else {
                                        // New bar - push old one
                                        state.ohlc_history.push_back(*bar);
                                        while state.ohlc_history.len() > MAX_PRICE_HISTORY {
                                            state.ohlc_history.pop_front();
                                        }
                                        state.current_bar = Some(OhlcPoint {
                                            ts: bar_ts,
                                            open: m,
                                            high: m,
                                            low: m,
                                            close: m,
                                            bid: bid.unwrap_or(m),
                                            ask: ask.unwrap_or(m),
                                        });
                                    }
                                } else {
                                    state.current_bar = Some(OhlcPoint {
                                        ts: bar_ts,
                                        open: m,
                                        high: m,
                                        low: m,
                                        close: m,
                                        bid: bid.unwrap_or(m),
                                        ask: ask.unwrap_or(m),
                                    });
                                }
                            }

                            // Component 2: handler_entry → state_update_complete (process_us)
                            let process_us = handler_entry_instant.elapsed().as_micros() as u64;

                            // Total: propagate + process (what the chart shows as "receive")
                            let receive_us = propagate_us + process_us;

                            // Record latency sample (throttled to ~1Hz)
                            let should_record = state
                                .latency_history
                                .back()
                                .map(|l| ts_ms - l.ts_ms >= 1000)
                                .unwrap_or(true);

                            if should_record {
                                state.latency_history.push_back(LatencySample {
                                    ts_ms,
                                    receive_us,
                                    propagate_us,
                                    process_us,
                                    network_us,
                                });
                                while state.latency_history.len() > MAX_LATENCY_HISTORY {
                                    state.latency_history.pop_front();
                                }
                            }
                        }

                        // Broadcast
                        if let (Some(b), Some(a), Some(m)) = (bid, ask, mid) {
                            let _ = self.update_tx.send(ArbUpdateEvent {
                                symbol,
                                event_type: ArbEventType::Quote {
                                    bid: b,
                                    ask: a,
                                    mid: m,
                                },
                                ts_ms,
                            });
                        }
                    }
                    Err(e) => {
                        debug!(error = %e, "binance arb L1 stream error");
                    }
                },
            }
        }
        Ok(())
    }

    async fn consume_trades(
        self: Arc<Self>,
        streams: Streams<
            barter_data::streams::consumer::MarketStreamResult<
                MarketDataInstrument,
                barter_data::subscription::trade::PublicTrade,
            >,
        >,
    ) -> Result<()> {
        let mut joined = streams.select_all();

        while let Some(event) = joined.next().await {
            match event {
                ReconnectEvent::Reconnecting(exchange) => {
                    warn!(?exchange, "binance arb trades stream reconnecting");
                }
                ReconnectEvent::Item(result) => match result {
                    Ok(market_event) => {
                        let symbol = to_symbol(&market_event.instrument);
                        let ts_ms = market_event.time_received.timestamp_millis();

                        let receive_us = chrono::Utc::now()
                            .signed_duration_since(market_event.time_received)
                            .num_microseconds()
                            .unwrap_or(0)
                            .max(0) as u64;

                        let trade = TradeTick {
                            ts_ms,
                            price: market_event.kind.price,
                            size: market_event.kind.amount,
                            is_buyer_maker: matches!(
                                market_event.kind.side,
                                barter_instrument::Side::Sell
                            ),
                            receive_latency_us: receive_us,
                        };

                        // Update state
                        {
                            let mut inner = self.inner.write();
                            let state = inner.entry(symbol.clone()).or_default();

                            state.trades.push_back(trade.clone());
                            while state.trades.len() > MAX_TRADE_HISTORY {
                                state.trades.pop_front();
                            }
                        }

                        // Broadcast
                        let _ = self.update_tx.send(ArbUpdateEvent {
                            symbol,
                            event_type: ArbEventType::Trade(trade),
                            ts_ms,
                        });
                    }
                    Err(e) => {
                        debug!(error = %e, "binance arb trades stream error");
                    }
                },
            }
        }
        Ok(())
    }
}

/// Snapshot of arbitrage data for API response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArbSnapshot {
    pub symbol: String,
    pub best_bid: Option<f64>,
    pub best_ask: Option<f64>,
    pub mid_price: Option<f64>,
    pub last_update_ts: i64,
    pub recent_trades: Vec<TradeTick>,
    pub ohlc_history: Vec<OhlcPoint>,
    pub latency_history: Vec<LatencySample>,
}

async fn init_l1_streams() -> Result<
    Streams<
        barter_data::streams::consumer::MarketStreamResult<
            MarketDataInstrument,
            barter_data::subscription::book::OrderBookL1,
        >,
    >,
> {
    Streams::<OrderBooksL1>::builder()
        .subscribe([
            (
                BinanceSpot::default(),
                "btc",
                "usdt",
                MarketDataInstrumentKind::Spot,
                OrderBooksL1,
            ),
            (
                BinanceSpot::default(),
                "eth",
                "usdt",
                MarketDataInstrumentKind::Spot,
                OrderBooksL1,
            ),
            (
                BinanceSpot::default(),
                "sol",
                "usdt",
                MarketDataInstrumentKind::Spot,
                OrderBooksL1,
            ),
            (
                BinanceSpot::default(),
                "xrp",
                "usdt",
                MarketDataInstrumentKind::Spot,
                OrderBooksL1,
            ),
        ])
        .init()
        .await
        .context("failed to init binance arb L1 streams")
}

async fn init_trade_streams() -> Result<
    Streams<
        barter_data::streams::consumer::MarketStreamResult<
            MarketDataInstrument,
            barter_data::subscription::trade::PublicTrade,
        >,
    >,
> {
    Streams::<PublicTrades>::builder()
        .subscribe([
            (
                BinanceSpot::default(),
                "btc",
                "usdt",
                MarketDataInstrumentKind::Spot,
                PublicTrades,
            ),
            (
                BinanceSpot::default(),
                "eth",
                "usdt",
                MarketDataInstrumentKind::Spot,
                PublicTrades,
            ),
            (
                BinanceSpot::default(),
                "sol",
                "usdt",
                MarketDataInstrumentKind::Spot,
                PublicTrades,
            ),
            (
                BinanceSpot::default(),
                "xrp",
                "usdt",
                MarketDataInstrumentKind::Spot,
                PublicTrades,
            ),
        ])
        .init()
        .await
        .context("failed to init binance arb trades streams")
}

fn to_symbol(instrument: &MarketDataInstrument) -> String {
    format!("{}{}", instrument.base, instrument.quote).to_ascii_uppercase()
}

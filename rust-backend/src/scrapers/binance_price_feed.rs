use anyhow::{Context, Result};
use barter_data::{
    exchange::binance::spot::BinanceSpot,
    streams::{reconnect::Event as ReconnectEvent, Streams},
    subscription::book::OrderBooksL1,
};
use barter_instrument::instrument::market_data::{
    kind::MarketDataInstrumentKind, MarketDataInstrument,
};
use futures_util::StreamExt;
use parking_lot::RwLock;
use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
    time::Instant,
};
use tokio::sync::broadcast;
use tracing::{debug, trace, warn};

#[derive(Debug, Clone, Copy)]
pub struct PricePoint {
    pub ts: i64,
    pub mid: f64,
}

/// Price update event for reactive consumers (e.g., FAST15M engine)
#[derive(Debug, Clone)]
pub struct PriceUpdateEvent {
    pub symbol: String,
    pub ts: i64,
    pub mid: f64,
    pub received_at_ns: u64,
}

#[derive(Debug, Clone, Default)]
struct SymbolState {
    latest: Option<PricePoint>,
    history: VecDeque<PricePoint>,
    // EWMA variance of per-second log returns.
    ewma_var: Option<f64>,
    last_mid: Option<f64>,
    last_ts: Option<i64>,
}

/// Monotonic nanosecond timestamp for latency measurement
#[inline]
fn now_ns() -> u64 {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    let start = START.get_or_init(Instant::now);
    start.elapsed().as_nanos() as u64
}

#[derive(Debug, Clone)]
pub struct BinancePriceFeed {
    inner: Arc<RwLock<HashMap<String, SymbolState>>>,
    max_history_len: usize,
    ewma_lambda: f64,
    /// Broadcast channel for reactive consumers
    update_tx: broadcast::Sender<PriceUpdateEvent>,
}

impl BinancePriceFeed {
    pub fn disabled() -> Arc<Self> {
        let (update_tx, _) = broadcast::channel(1024);
        Arc::new(Self {
            inner: Arc::new(RwLock::new(HashMap::new())),
            max_history_len: 0,
            ewma_lambda: 0.97,
            update_tx,
        })
    }

    pub async fn spawn_default() -> Result<Arc<Self>> {
        // Broadcast channel capacity: 1024 events (~1Hz per symbol * 4 symbols = ~4/sec)
        // Buffer handles bursts without lagging
        let (update_tx, _) = broadcast::channel(1024);

        let feed = Arc::new(Self {
            inner: Arc::new(RwLock::new(HashMap::new())),
            max_history_len: 3 * 60 * 60, // ~3h at 1Hz
            ewma_lambda: 0.97,
            update_tx,
        });

        // NOTE: `barter-data`'s `StreamBuilder` futures are `!Send`, so we must initialise
        // the streams *outside* of `tokio::spawn`.
        let streams = init_streams().await?;

        let task_feed = feed.clone();
        tokio::spawn(async move {
            if let Err(e) = task_feed.consume(streams).await {
                warn!(error = %e, "binance price feed stopped");
            }
        });

        Ok(feed)
    }

    /// Subscribe to price updates for reactive processing.
    /// Returns a broadcast receiver that will receive PriceUpdateEvent for all symbols.
    pub fn subscribe(&self) -> broadcast::Receiver<PriceUpdateEvent> {
        self.update_tx.subscribe()
    }

    /// Get the broadcast sender (for cloning to multiple consumers)
    pub fn update_sender(&self) -> broadcast::Sender<PriceUpdateEvent> {
        self.update_tx.clone()
    }

    pub fn latest_mid(&self, symbol: &str) -> Option<PricePoint> {
        self.inner.read().get(symbol).and_then(|s| s.latest)
    }

    /// Return the price point closest to `target_ts` within `max_skew_sec`.
    pub fn mid_near(&self, symbol: &str, target_ts: i64, max_skew_sec: i64) -> Option<PricePoint> {
        let state = self.inner.read();
        let sym = state.get(symbol)?;
        let mut best: Option<PricePoint> = None;
        let mut best_abs = i64::MAX;

        for p in sym.history.iter() {
            let abs = (p.ts - target_ts).abs();
            if abs <= max_skew_sec && abs < best_abs {
                best_abs = abs;
                best = Some(*p);
            }
        }

        // Fall back to latest if it is close enough.
        if best.is_none() {
            if let Some(p) = sym.latest {
                if (p.ts - target_ts).abs() <= max_skew_sec {
                    best = Some(p);
                }
            }
        }

        best
    }

    /// Approximate per-sqrt-second volatility (sigma) from EWMA of per-second log returns.
    pub fn sigma_per_sqrt_s(&self, symbol: &str) -> Option<f64> {
        let state = self.inner.read();
        let sym = state.get(symbol)?;
        let v = sym.ewma_var?;
        if v.is_finite() && v > 0.0 {
            Some(v.sqrt())
        } else {
            None
        }
    }

    async fn consume(
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
                    warn!(?exchange, "binance stream reconnecting");
                    // Record reconnect event
                    crate::latency::global_comprehensive()
                        .failures
                        .record_reconnect("binance_ws", 0);
                }
                ReconnectEvent::Item(result) => match result {
                    Ok(market_event) => {
                        // Capture receive timestamp immediately for latency measurement
                        let received_at_ns = now_ns();

                        let symbol = to_symbol(&market_event.instrument);
                        let ts = market_event.time_received.timestamp();

                        let Some(mid) = market_event
                            .kind
                            .mid_price()
                            .and_then(|d| d.to_string().parse::<f64>().ok())
                            .filter(|m| m.is_finite() && *m > 0.0)
                        else {
                            continue;
                        };

                        // Update internal state
                        self.update_symbol(&symbol, ts, mid);

                        // Feed to oracle comparison tracker (map Binance symbol to asset)
                        // BTCUSDT -> BTC, ETHUSDT -> ETH, etc.
                        {
                            use crate::scrapers::oracle_comparison::global_oracle_tracker;
                            let asset = symbol
                                .strip_suffix("USDT")
                                .or_else(|| symbol.strip_suffix("USD"))
                                .unwrap_or(&symbol);
                            global_oracle_tracker().record_binance(asset, mid, ts);
                        }

                        // Latency from when `barter-data` timestamps the event as received
                        // to when we have incorporated it into our internal state.
                        // (This captures internal scheduling/queueing + our handler work.)
                        let receive_latency_us = chrono::Utc::now()
                            .signed_duration_since(market_event.time_received)
                            .num_microseconds()
                            .unwrap_or(0)
                            .max(0) as u64;

                        // Record to latency registry (for dashboard): receive latency
                        crate::latency::global_registry().record_span(
                            crate::latency::LatencySpan::new(
                                crate::latency::SpanType::BinanceWs,
                                receive_latency_us,
                            ),
                        );

                        // Processing time inside our handler (decode/normalize + state update)
                        let processing_ns = now_ns().saturating_sub(received_at_ns);
                        let processing_us = processing_ns / 1000;

                        // Record to performance profiler
                        crate::performance::global_profiler()
                            .pipeline
                            .record_binance(processing_us);
                        crate::performance::global_profiler()
                            .throughput
                            .record_binance_update();
                        // Record to CPU profiler for hot path tracking
                        crate::performance::global_profiler()
                            .cpu
                            .record_span("binance_tick_process", processing_us);

                        // Record comprehensive metrics
                        crate::latency::global_comprehensive()
                            .t2t
                            .record_stage(crate::latency::T2TStage::MdReceive, receive_latency_us);
                        crate::latency::global_comprehensive()
                            .t2t
                            .record_stage(crate::latency::T2TStage::MdDecode, processing_us);
                        crate::latency::global_comprehensive()
                            .throughput
                            .md_messages_in
                            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        crate::latency::global_comprehensive()
                            .throughput
                            .md_decode_count
                            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        crate::latency::global_comprehensive()
                            .md_integrity
                            .record_message(
                                "binance_ws",
                                None,
                                Some(
                                    (market_event.time_exchange.timestamp_millis().max(0) as u64)
                                        * 1000,
                                ),
                            );

                        // Broadcast to reactive consumers
                        let update_event = PriceUpdateEvent {
                            symbol: symbol.clone(),
                            ts,
                            mid,
                            received_at_ns,
                        };

                        // Non-blocking send; if no receivers or lagged, just drop
                        if let Err(e) = self.update_tx.send(update_event) {
                            trace!(
                                symbol = %symbol,
                                "no active price update receivers: {}",
                                e
                            );
                        }
                    }
                    Err(e) => {
                        debug!(error = %e, "binance market stream error")
                    }
                },
            }
        }

        Ok(())
    }

    fn update_symbol(&self, symbol: &str, ts: i64, mid: f64) {
        let mut map = self.inner.write();
        let entry = map.entry(symbol.to_string()).or_default();

        // Update EWMA variance using per-second log returns.
        if let (Some(prev_mid), Some(prev_ts)) = (entry.last_mid, entry.last_ts) {
            let dt = (ts - prev_ts).max(1) as f64;
            if prev_mid > 0.0 && mid > 0.0 {
                let r = (mid / prev_mid).ln() / dt;
                let var_obs = r * r;
                let next = match entry.ewma_var {
                    Some(v) => (self.ewma_lambda * v) + ((1.0 - self.ewma_lambda) * var_obs),
                    None => var_obs,
                };
                if next.is_finite() {
                    entry.ewma_var = Some(next);
                }
            }
        }

        entry.last_mid = Some(mid);
        entry.last_ts = Some(ts);
        entry.latest = Some(PricePoint { ts, mid });

        // Downsample to ~1Hz.
        let should_push = match entry.history.back() {
            Some(last) => last.ts != ts,
            None => true,
        };

        if should_push {
            entry.history.push_back(PricePoint { ts, mid });
            while entry.history.len() > self.max_history_len {
                entry.history.pop_front();
            }
        } else if let Some(last) = entry.history.back_mut() {
            last.mid = mid;
        }
    }
}

async fn init_streams() -> Result<
    Streams<
        barter_data::streams::consumer::MarketStreamResult<
            MarketDataInstrument,
            barter_data::subscription::book::OrderBookL1,
        >,
    >,
> {
    // Subscribe to L1 orderbooks (best bid/ask) and compute mid-price.
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
        .context("failed to init barter-data binance streams")
}

fn to_symbol(instrument: &MarketDataInstrument) -> String {
    // Binance subscriptions are base+quote (e.g., BTCUSDT).
    format!("{}{}", instrument.base, instrument.quote).to_ascii_uppercase()
}

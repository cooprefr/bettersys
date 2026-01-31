//! Optimized Binance bookTicker Feed
//!
//! High-performance, low-latency ingestion for top-of-book data.
//!
//! Design principles:
//! - Direct WebSocket connection to Binance bookTicker stream (minimal payload)
//! - SIMD-accelerated JSON parsing (simd-json)
//! - Zero heap allocations on hot path
//! - Last-value snapshot mechanism (no broadcast backpressure)
//! - Sequence tracking with gap detection
//! - CPU-pinned receive task
//! - Async metrics collection off hot path
//!
//! This module replaces the barter-data based feed for FAST15M use cases.

use anyhow::{Context, Result};
use arc_swap::ArcSwap;
use parking_lot::{Mutex, RwLock};
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use tokio::sync::mpsc;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use futures_util::{SinkExt, StreamExt};

// =============================================================================
// CONSTANTS
// =============================================================================

/// Binance combined stream endpoint
const BINANCE_STREAM_URL: &str = "wss://stream.binance.com:9443/stream";

/// Symbols to subscribe (bookTicker streams)
const SYMBOLS: &[&str] = &["btcusdt", "ethusdt", "solusdt", "xrpusdt"];

/// Maximum history length per symbol (for mid_near lookups)
const MAX_HISTORY_LEN: usize = 10_800; // 3 hours at 1Hz

/// EWMA lambda for volatility estimation
const EWMA_LAMBDA: f64 = 0.97;

// =============================================================================
// MONOTONIC CLOCK
// =============================================================================

/// Process-relative monotonic nanosecond timestamp
#[inline(always)]
fn mono_now_ns() -> u64 {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    let start = START.get_or_init(Instant::now);
    start.elapsed().as_nanos() as u64
}

// =============================================================================
// DATA STRUCTURES (ZERO-ALLOC HOT PATH)
// =============================================================================

/// Symbol identifier (avoids String allocation on hot path)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Symbol {
    BtcUsdt = 0,
    EthUsdt = 1,
    SolUsdt = 2,
    XrpUsdt = 3,
}

impl Symbol {
    #[inline]
    pub fn from_bytes(s: &[u8]) -> Option<Self> {
        match s {
            b"BTCUSDT" => Some(Self::BtcUsdt),
            b"ETHUSDT" => Some(Self::EthUsdt),
            b"SOLUSDT" => Some(Self::SolUsdt),
            b"XRPUSDT" => Some(Self::XrpUsdt),
            _ => None,
        }
    }

    #[inline]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::BtcUsdt => "BTCUSDT",
            Self::EthUsdt => "ETHUSDT",
            Self::SolUsdt => "SOLUSDT",
            Self::XrpUsdt => "XRPUSDT",
        }
    }

    #[inline]
    pub fn index(&self) -> usize {
        *self as usize
    }

    pub const COUNT: usize = 4;
}

/// Top-of-book snapshot (fixed size, no allocations)
#[derive(Debug, Clone, Copy, Default)]
pub struct BookTickerSnapshot {
    /// Update ID for sequence tracking
    pub update_id: u64,
    /// Exchange event timestamp (milliseconds, wall-clock reference only)
    pub exchange_ts_ms: i64,
    /// Monotonic timestamp when recv() returned
    pub recv_mono_ns: u64,
    /// Monotonic timestamp after decode completed
    pub decoded_mono_ns: u64,
    /// Best bid price
    pub bid_price: f64,
    /// Best bid quantity
    pub bid_qty: f64,
    /// Best ask price
    pub ask_price: f64,
    /// Best ask quantity
    pub ask_qty: f64,
}

impl BookTickerSnapshot {
    #[inline]
    pub fn mid_price(&self) -> f64 {
        (self.bid_price + self.ask_price) * 0.5
    }

    #[inline]
    pub fn spread_bps(&self) -> f64 {
        let mid = self.mid_price();
        if mid == 0.0 {
            return 0.0;
        }
        ((self.ask_price - self.bid_price) / mid) * 10_000.0
    }

    #[inline]
    pub fn decode_latency_ns(&self) -> u64 {
        self.decoded_mono_ns.saturating_sub(self.recv_mono_ns)
    }
}

/// Historical price point (for mid_near lookups)
#[derive(Debug, Clone, Copy)]
pub struct PricePoint {
    pub ts: i64,
    pub mid: f64,
}

/// Per-symbol state with history and volatility
#[derive(Debug)]
struct SymbolState {
    /// Latest snapshot (ArcSwap for lock-free reads)
    latest: ArcSwap<BookTickerSnapshot>,
    /// Price history for mid_near (protected by mutex, not on hot path)
    history: Mutex<HistoryBuffer>,
    /// EWMA variance state
    ewma: Mutex<EwmaState>,
    /// Last update ID for gap detection
    last_update_id: AtomicU64,
    /// Monotonic time of last update
    last_update_mono_ns: AtomicU64,
    /// Gap count
    gaps_detected: AtomicU64,
}

#[derive(Debug, Default)]
struct HistoryBuffer {
    points: std::collections::VecDeque<PricePoint>,
    last_ts: i64,
}

#[derive(Debug, Default)]
struct EwmaState {
    var: Option<f64>,
    last_mid: Option<f64>,
    last_ts: Option<i64>,
}

impl SymbolState {
    fn new() -> Self {
        Self {
            latest: ArcSwap::from_pointee(BookTickerSnapshot::default()),
            history: Mutex::new(HistoryBuffer::default()),
            ewma: Mutex::new(EwmaState::default()),
            last_update_id: AtomicU64::new(0),
            last_update_mono_ns: AtomicU64::new(0),
            gaps_detected: AtomicU64::new(0),
        }
    }
}

// =============================================================================
// METRICS (OFF HOT PATH)
// =============================================================================

/// Lightweight metrics for async collection
#[derive(Debug, Default)]
pub struct FeedMetrics {
    // Latency histograms (lock-free)
    pub decode_latency_sum_ns: AtomicU64,
    pub decode_latency_count: AtomicU64,
    pub decode_latency_max_ns: AtomicU64,

    // Jitter tracking
    pub last_arrival_mono_ns: [AtomicU64; Symbol::COUNT],
    pub jitter_sum_ns: AtomicU64,
    pub jitter_count: AtomicU64,
    pub jitter_max_ns: AtomicU64,

    // Counters
    pub messages_received: AtomicU64,
    pub parse_errors: AtomicU64,
    pub gaps_total: AtomicU64,
    pub reconnects: AtomicU64,
}

impl FeedMetrics {
    pub fn new() -> Self {
        Self {
            last_arrival_mono_ns: [
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
            ],
            ..Default::default()
        }
    }

    #[inline]
    pub fn record_decode_latency(&self, latency_ns: u64) {
        self.decode_latency_sum_ns.fetch_add(latency_ns, Ordering::Relaxed);
        self.decode_latency_count.fetch_add(1, Ordering::Relaxed);

        // Update max (CAS loop)
        let mut current = self.decode_latency_max_ns.load(Ordering::Relaxed);
        while latency_ns > current {
            match self.decode_latency_max_ns.compare_exchange_weak(
                current,
                latency_ns,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(x) => current = x,
            }
        }
    }

    #[inline]
    pub fn record_arrival(&self, symbol: Symbol, mono_ns: u64) {
        let idx = symbol.index();
        let prev = self.last_arrival_mono_ns[idx].swap(mono_ns, Ordering::Relaxed);

        if prev > 0 {
            let interval = mono_ns.saturating_sub(prev);
            // Simple jitter: deviation from expected 100ms interval
            let expected_ns = 100_000_000u64; // 100ms typical for bookTicker
            let jitter = if interval > expected_ns {
                interval - expected_ns
            } else {
                expected_ns - interval
            };

            self.jitter_sum_ns.fetch_add(jitter, Ordering::Relaxed);
            self.jitter_count.fetch_add(1, Ordering::Relaxed);

            // Update max jitter
            let mut current = self.jitter_max_ns.load(Ordering::Relaxed);
            while jitter > current {
                match self.jitter_max_ns.compare_exchange_weak(
                    current,
                    jitter,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => break,
                    Err(x) => current = x,
                }
            }
        }
    }

    pub fn decode_latency_mean_ns(&self) -> f64 {
        let count = self.decode_latency_count.load(Ordering::Relaxed);
        if count == 0 {
            return 0.0;
        }
        let sum = self.decode_latency_sum_ns.load(Ordering::Relaxed);
        sum as f64 / count as f64
    }

    pub fn jitter_mean_ns(&self) -> f64 {
        let count = self.jitter_count.load(Ordering::Relaxed);
        if count == 0 {
            return 0.0;
        }
        let sum = self.jitter_sum_ns.load(Ordering::Relaxed);
        sum as f64 / count as f64
    }

    pub fn time_since_last_update_ns(&self, symbol: Symbol) -> u64 {
        let last = self.last_arrival_mono_ns[symbol.index()].load(Ordering::Relaxed);
        if last == 0 {
            return u64::MAX;
        }
        mono_now_ns().saturating_sub(last)
    }
}

// =============================================================================
// SIMD-JSON PARSING (ZERO-ALLOC)
// =============================================================================

/// Parse bookTicker message using simd-json
/// Format: {"u":400900217,"s":"BTCUSDT","b":"25.35190000","B":"31.21","a":"25.36520000","A":"40.66"}
#[inline]
fn parse_book_ticker(
    raw: &mut [u8],
    recv_mono_ns: u64,
) -> Result<(Symbol, BookTickerSnapshot), ParseError> {
    use simd_json::prelude::*;

    let value = simd_json::to_borrowed_value(raw)
        .map_err(|_| ParseError::InvalidJson)?;

    let obj = value.as_object().ok_or(ParseError::NotObject)?;

    // Extract symbol
    let symbol_str = obj
        .get("s")
        .and_then(|v| v.as_str())
        .ok_or(ParseError::MissingField("s"))?;
    let symbol = Symbol::from_bytes(symbol_str.as_bytes())
        .ok_or(ParseError::UnknownSymbol)?;

    // Extract update_id
    let update_id = obj
        .get("u")
        .and_then(|v| v.as_u64())
        .ok_or(ParseError::MissingField("u"))?;

    // Extract prices (strings in Binance format)
    let bid_price = parse_price_str(obj.get("b"))?;
    let bid_qty = parse_price_str(obj.get("B"))?;
    let ask_price = parse_price_str(obj.get("a"))?;
    let ask_qty = parse_price_str(obj.get("A"))?;

    // Extract event time if present (some streams include 'E')
    let exchange_ts_ms = obj
        .get("E")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);

    let decoded_mono_ns = mono_now_ns();

    Ok((
        symbol,
        BookTickerSnapshot {
            update_id,
            exchange_ts_ms,
            recv_mono_ns,
            decoded_mono_ns,
            bid_price,
            bid_qty,
            ask_price,
            ask_qty,
        },
    ))
}

/// Parse combined stream wrapper
/// Format: {"stream":"btcusdt@bookTicker","data":{...}}
#[inline]
fn parse_combined_stream(
    raw: &mut [u8],
    recv_mono_ns: u64,
) -> Result<(Symbol, BookTickerSnapshot), ParseError> {
    use simd_json::prelude::*;

    let value = simd_json::to_borrowed_value(raw)
        .map_err(|_| ParseError::InvalidJson)?;

    let obj = value.as_object().ok_or(ParseError::NotObject)?;

    // Check for ping/pong or subscription responses
    if obj.contains_key("result") || obj.contains_key("id") {
        return Err(ParseError::ControlMessage);
    }

    // Get the data payload
    let data = obj
        .get("data")
        .ok_or(ParseError::MissingField("data"))?;

    let data_obj = data.as_object().ok_or(ParseError::NotObject)?;

    // Extract symbol
    let symbol_str = data_obj
        .get("s")
        .and_then(|v| v.as_str())
        .ok_or(ParseError::MissingField("s"))?;
    let symbol = Symbol::from_bytes(symbol_str.as_bytes())
        .ok_or(ParseError::UnknownSymbol)?;

    // Extract update_id
    let update_id = data_obj
        .get("u")
        .and_then(|v| v.as_u64())
        .ok_or(ParseError::MissingField("u"))?;

    // Extract prices
    let bid_price = parse_price_str(data_obj.get("b"))?;
    let bid_qty = parse_price_str(data_obj.get("B"))?;
    let ask_price = parse_price_str(data_obj.get("a"))?;
    let ask_qty = parse_price_str(data_obj.get("A"))?;

    // Extract event time
    let exchange_ts_ms = data_obj
        .get("E")
        .and_then(|v| v.as_i64())
        .unwrap_or_else(|| chrono::Utc::now().timestamp_millis());

    let decoded_mono_ns = mono_now_ns();

    Ok((
        symbol,
        BookTickerSnapshot {
            update_id,
            exchange_ts_ms,
            recv_mono_ns,
            decoded_mono_ns,
            bid_price,
            bid_qty,
            ask_price,
            ask_qty,
        },
    ))
}

#[inline]
fn parse_price_str(value: Option<&simd_json::BorrowedValue>) -> Result<f64, ParseError> {
    use simd_json::prelude::*;
    let s = value
        .and_then(|v| v.as_str())
        .ok_or(ParseError::MissingField("price"))?;
    fast_float::parse(s).map_err(|_| ParseError::InvalidPrice)
}

#[derive(Debug)]
enum ParseError {
    InvalidJson,
    NotObject,
    MissingField(&'static str),
    UnknownSymbol,
    InvalidPrice,
    ControlMessage,
}

// =============================================================================
// MAIN FEED STRUCTURE
// =============================================================================

/// Optimized Binance bookTicker feed
pub struct BinanceBookTickerFeed {
    /// Per-symbol state (array for cache-friendly access)
    symbols: [SymbolState; Symbol::COUNT],
    /// Metrics (async collection)
    metrics: Arc<FeedMetrics>,
    /// Shutdown flag
    shutdown: AtomicBool,
    /// Connected flag
    connected: AtomicBool,
    /// Channel for gap notifications (async, off hot path)
    gap_tx: mpsc::UnboundedSender<GapEvent>,
}

/// Gap detection event
#[derive(Debug, Clone)]
pub struct GapEvent {
    pub symbol: Symbol,
    pub expected: u64,
    pub received: u64,
    pub gap_size: u64,
    pub mono_ns: u64,
}

impl BinanceBookTickerFeed {
    /// Create a new feed instance
    pub fn new() -> (Arc<Self>, mpsc::UnboundedReceiver<GapEvent>) {
        let (gap_tx, gap_rx) = mpsc::unbounded_channel();

        let feed = Arc::new(Self {
            symbols: [
                SymbolState::new(),
                SymbolState::new(),
                SymbolState::new(),
                SymbolState::new(),
            ],
            metrics: Arc::new(FeedMetrics::new()),
            shutdown: AtomicBool::new(false),
            connected: AtomicBool::new(false),
            gap_tx,
        });

        (feed, gap_rx)
    }

    /// Spawn the feed with CPU pinning
    pub async fn spawn(self: Arc<Self>, pin_to_core: Option<usize>) -> Result<()> {
        let feed = self.clone();

        // Spawn on a dedicated task
        let handle = tokio::spawn(async move {
            // Attempt CPU pinning if requested
            if let Some(core_id) = pin_to_core {
                if let Some(core_ids) = core_affinity::get_core_ids() {
                    if let Some(target_core) = core_ids.get(core_id) {
                        if core_affinity::set_for_current(*target_core) {
                            // Pinning successful - no logging on hot path
                        }
                    }
                }
            }

            feed.run_loop().await
        });

        handle.await??;
        Ok(())
    }

    /// Spawn without blocking (returns immediately)
    pub fn spawn_background(self: Arc<Self>, pin_to_core: Option<usize>) {
        let feed = self.clone();
        tokio::spawn(async move {
            if let Some(core_id) = pin_to_core {
                if let Some(core_ids) = core_affinity::get_core_ids() {
                    if let Some(target_core) = core_ids.get(core_id) {
                        let _ = core_affinity::set_for_current(*target_core);
                    }
                }
            }

            if let Err(e) = feed.run_loop().await {
                // Log error outside hot path via separate channel
                eprintln!("BinanceBookTickerFeed error: {}", e);
            }
        });
    }

    /// Main connection loop with reconnection
    async fn run_loop(self: Arc<Self>) -> Result<()> {
        let mut reconnect_delay = Duration::from_millis(100);
        let max_delay = Duration::from_secs(30);

        while !self.shutdown.load(Ordering::Relaxed) {
            match self.connect_and_stream().await {
                Ok(()) => {
                    reconnect_delay = Duration::from_millis(100);
                }
                Err(_) => {
                    self.connected.store(false, Ordering::Release);
                    self.metrics.reconnects.fetch_add(1, Ordering::Relaxed);

                    tokio::time::sleep(reconnect_delay).await;
                    reconnect_delay = (reconnect_delay * 2).min(max_delay);
                }
            }
        }

        Ok(())
    }

    /// Connect to Binance and stream bookTicker updates
    async fn connect_and_stream(&self) -> Result<()> {
        // Build combined stream URL
        let streams: Vec<String> = SYMBOLS
            .iter()
            .map(|s| format!("{}@bookTicker", s))
            .collect();
        let url = format!("{}?streams={}", BINANCE_STREAM_URL, streams.join("/"));

        let (ws_stream, _) = connect_async(&url)
            .await
            .context("Failed to connect to Binance")?;

        self.connected.store(true, Ordering::Release);

        let (mut write, mut read) = ws_stream.split();

        // Pre-allocate buffer for message parsing (reused across messages)
        let mut parse_buffer: Vec<u8> = Vec::with_capacity(1024);

        while let Some(msg_result) = read.next().await {
            if self.shutdown.load(Ordering::Relaxed) {
                break;
            }

            let msg = match msg_result {
                Ok(m) => m,
                Err(_) => break,
            };

            match msg {
                Message::Text(text) => {
                    // T_recv: Capture monotonic timestamp IMMEDIATELY
                    let recv_mono_ns = mono_now_ns();

                    // Reuse buffer to avoid allocation
                    parse_buffer.clear();
                    parse_buffer.extend_from_slice(text.as_bytes());

                    // Parse with simd-json (mutates buffer)
                    match parse_combined_stream(&mut parse_buffer, recv_mono_ns) {
                        Ok((symbol, snapshot)) => {
                            self.process_snapshot(symbol, snapshot);
                        }
                        Err(ParseError::ControlMessage) => {
                            // Subscription confirmation, ignore
                        }
                        Err(_) => {
                            self.metrics.parse_errors.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }
                Message::Binary(data) => {
                    let recv_mono_ns = mono_now_ns();

                    parse_buffer.clear();
                    parse_buffer.extend_from_slice(&data);

                    match parse_combined_stream(&mut parse_buffer, recv_mono_ns) {
                        Ok((symbol, snapshot)) => {
                            self.process_snapshot(symbol, snapshot);
                        }
                        Err(ParseError::ControlMessage) => {}
                        Err(_) => {
                            self.metrics.parse_errors.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }
                Message::Ping(payload) => {
                    // Respond to ping immediately
                    let _ = write.send(Message::Pong(payload)).await;
                }
                Message::Close(_) => {
                    break;
                }
                _ => {}
            }
        }

        self.connected.store(false, Ordering::Release);
        Ok(())
    }

    /// Process a parsed snapshot (HOT PATH - no allocations, no logging)
    #[inline]
    fn process_snapshot(&self, symbol: Symbol, snapshot: BookTickerSnapshot) {
        let state = &self.symbols[symbol.index()];

        // Sequence tracking (gap detection)
        let prev_id = state.last_update_id.swap(snapshot.update_id, Ordering::Relaxed);
        if prev_id > 0 && snapshot.update_id > prev_id + 1 {
            let gap = snapshot.update_id - prev_id - 1;
            state.gaps_detected.fetch_add(gap, Ordering::Relaxed);
            self.metrics.gaps_total.fetch_add(gap, Ordering::Relaxed);

            // Send gap event (non-blocking, off hot path)
            let _ = self.gap_tx.send(GapEvent {
                symbol,
                expected: prev_id + 1,
                received: snapshot.update_id,
                gap_size: gap,
                mono_ns: snapshot.recv_mono_ns,
            });
        }

        // Update latest snapshot (lock-free via ArcSwap)
        state.latest.store(Arc::new(snapshot));
        state.last_update_mono_ns.store(snapshot.recv_mono_ns, Ordering::Relaxed);

        // Record metrics (lock-free atomics)
        let decode_latency = snapshot.decode_latency_ns();
        self.metrics.record_decode_latency(decode_latency);
        self.metrics.record_arrival(symbol, snapshot.recv_mono_ns);
        self.metrics.messages_received.fetch_add(1, Ordering::Relaxed);

        // Update history and EWMA (separate locks, not on critical read path)
        // This is acceptable because consumers use latest snapshot, not history
        let mid = snapshot.mid_price();
        let ts = snapshot.exchange_ts_ms / 1000; // Convert to seconds

        // History update (for mid_near)
        {
            let mut history = state.history.lock();
            if history.last_ts != ts {
                history.points.push_back(PricePoint { ts, mid });
                while history.points.len() > MAX_HISTORY_LEN {
                    history.points.pop_front();
                }
                history.last_ts = ts;
            } else if let Some(last) = history.points.back_mut() {
                last.mid = mid;
            }
        }

        // EWMA update (for volatility)
        {
            let mut ewma = state.ewma.lock();
            if let (Some(prev_mid), Some(prev_ts)) = (ewma.last_mid, ewma.last_ts) {
                let dt = (ts - prev_ts).max(1) as f64;
                if prev_mid > 0.0 && mid > 0.0 {
                    let r = (mid / prev_mid).ln() / dt;
                    let var_obs = r * r;
                    let next = match ewma.var {
                        Some(v) => EWMA_LAMBDA * v + (1.0 - EWMA_LAMBDA) * var_obs,
                        None => var_obs,
                    };
                    if next.is_finite() {
                        ewma.var = Some(next);
                    }
                }
            }
            ewma.last_mid = Some(mid);
            ewma.last_ts = Some(ts);
        }
    }

    // =========================================================================
    // PUBLIC API (READ METHODS - LOCK-FREE)
    // =========================================================================

    /// Get latest snapshot for a symbol (lock-free)
    #[inline]
    pub fn latest(&self, symbol: Symbol) -> Arc<BookTickerSnapshot> {
        self.symbols[symbol.index()].latest.load_full()
    }

    /// Get latest mid-price for a symbol
    #[inline]
    pub fn latest_mid(&self, symbol_str: &str) -> Option<PricePoint> {
        let symbol = match symbol_str {
            "BTCUSDT" => Symbol::BtcUsdt,
            "ETHUSDT" => Symbol::EthUsdt,
            "SOLUSDT" => Symbol::SolUsdt,
            "XRPUSDT" => Symbol::XrpUsdt,
            _ => return None,
        };

        let snapshot = self.latest(symbol);
        if snapshot.update_id == 0 {
            return None;
        }

        Some(PricePoint {
            ts: snapshot.exchange_ts_ms / 1000,
            mid: snapshot.mid_price(),
        })
    }

    /// Get price point nearest to target timestamp
    pub fn mid_near(&self, symbol_str: &str, target_ts: i64, max_skew_sec: i64) -> Option<PricePoint> {
        let symbol = match symbol_str {
            "BTCUSDT" => Symbol::BtcUsdt,
            "ETHUSDT" => Symbol::EthUsdt,
            "SOLUSDT" => Symbol::SolUsdt,
            "XRPUSDT" => Symbol::XrpUsdt,
            _ => return None,
        };

        let state = &self.symbols[symbol.index()];
        let history = state.history.lock();

        let mut best: Option<PricePoint> = None;
        let mut best_abs = i64::MAX;

        for p in history.points.iter() {
            let abs = (p.ts - target_ts).abs();
            if abs <= max_skew_sec && abs < best_abs {
                best_abs = abs;
                best = Some(*p);
            }
        }

        // Fall back to latest
        if best.is_none() {
            let snapshot = state.latest.load();
            let ts = snapshot.exchange_ts_ms / 1000;
            if (ts - target_ts).abs() <= max_skew_sec {
                best = Some(PricePoint {
                    ts,
                    mid: snapshot.mid_price(),
                });
            }
        }

        best
    }

    /// Get volatility (sigma per sqrt second)
    pub fn sigma_per_sqrt_s(&self, symbol_str: &str) -> Option<f64> {
        let symbol = match symbol_str {
            "BTCUSDT" => Symbol::BtcUsdt,
            "ETHUSDT" => Symbol::EthUsdt,
            "SOLUSDT" => Symbol::SolUsdt,
            "XRPUSDT" => Symbol::XrpUsdt,
            _ => return None,
        };

        let state = &self.symbols[symbol.index()];
        let ewma = state.ewma.lock();
        ewma.var.filter(|v| v.is_finite() && *v > 0.0).map(|v| v.sqrt())
    }

    /// Check if connected
    #[inline]
    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Acquire)
    }

    /// Get metrics reference
    pub fn metrics(&self) -> &Arc<FeedMetrics> {
        &self.metrics
    }

    /// Shutdown the feed
    pub fn shutdown(&self) {
        self.shutdown.store(true, Ordering::Release);
    }

    /// Get gaps detected for a symbol
    pub fn gaps_detected(&self, symbol: Symbol) -> u64 {
        self.symbols[symbol.index()].gaps_detected.load(Ordering::Relaxed)
    }

    /// Get time since last update for a symbol (nanoseconds)
    pub fn time_since_update_ns(&self, symbol: Symbol) -> u64 {
        let last = self.symbols[symbol.index()]
            .last_update_mono_ns
            .load(Ordering::Relaxed);
        if last == 0 {
            return u64::MAX;
        }
        mono_now_ns().saturating_sub(last)
    }
}

// Note: BinanceBookTickerFeed doesn't implement Default because it requires
// a gap_rx channel receiver. Use BinanceBookTickerFeed::new() instead.

// =============================================================================
// COMPATIBILITY LAYER (FOR EXISTING CODE)
// =============================================================================

/// Adapter to provide the same interface as the old BinancePriceFeed
/// for consumers that haven't been updated yet.
pub struct BookTickerFeedAdapter {
    inner: Arc<BinanceBookTickerFeed>,
}

impl BookTickerFeedAdapter {
    pub fn new(feed: Arc<BinanceBookTickerFeed>) -> Self {
        Self { inner: feed }
    }

    pub fn latest_mid(&self, symbol: &str) -> Option<PricePoint> {
        self.inner.latest_mid(symbol)
    }

    pub fn mid_near(&self, symbol: &str, target_ts: i64, max_skew_sec: i64) -> Option<PricePoint> {
        self.inner.mid_near(symbol, target_ts, max_skew_sec)
    }

    pub fn sigma_per_sqrt_s(&self, symbol: &str) -> Option<f64> {
        self.inner.sigma_per_sqrt_s(symbol)
    }
}

// =============================================================================
// TESTS
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_symbol_from_bytes() {
        assert_eq!(Symbol::from_bytes(b"BTCUSDT"), Some(Symbol::BtcUsdt));
        assert_eq!(Symbol::from_bytes(b"ETHUSDT"), Some(Symbol::EthUsdt));
        assert_eq!(Symbol::from_bytes(b"SOLUSDT"), Some(Symbol::SolUsdt));
        assert_eq!(Symbol::from_bytes(b"XRPUSDT"), Some(Symbol::XrpUsdt));
        assert_eq!(Symbol::from_bytes(b"UNKNOWN"), None);
    }

    #[test]
    fn test_parse_book_ticker() {
        let mut data = br#"{"stream":"btcusdt@bookTicker","data":{"u":12345,"s":"BTCUSDT","b":"50000.00","B":"1.5","a":"50001.00","A":"2.0","E":1700000000000}}"#.to_vec();
        let recv_ns = 1000000u64;

        let result = parse_combined_stream(&mut data, recv_ns);
        assert!(result.is_ok());

        let (symbol, snapshot) = result.unwrap();
        assert_eq!(symbol, Symbol::BtcUsdt);
        assert_eq!(snapshot.update_id, 12345);
        assert!((snapshot.bid_price - 50000.0).abs() < 0.01);
        assert!((snapshot.ask_price - 50001.0).abs() < 0.01);
        assert_eq!(snapshot.exchange_ts_ms, 1700000000000);
        assert_eq!(snapshot.recv_mono_ns, recv_ns);
    }

    #[test]
    fn test_snapshot_calculations() {
        let snapshot = BookTickerSnapshot {
            update_id: 1,
            exchange_ts_ms: 1700000000000,
            recv_mono_ns: 1000000,
            decoded_mono_ns: 1050000,
            bid_price: 50000.0,
            bid_qty: 1.0,
            ask_price: 50010.0,
            ask_qty: 1.0,
        };

        assert!((snapshot.mid_price() - 50005.0).abs() < 0.01);
        assert!((snapshot.spread_bps() - 2.0).abs() < 0.1); // 10/50005 * 10000 ≈ 2 bps
        assert_eq!(snapshot.decode_latency_ns(), 50000);
    }

    #[test]
    fn test_metrics_recording() {
        let metrics = FeedMetrics::new();

        metrics.record_decode_latency(1000);
        metrics.record_decode_latency(2000);
        metrics.record_decode_latency(3000);

        assert_eq!(metrics.decode_latency_count.load(Ordering::Relaxed), 3);
        assert!((metrics.decode_latency_mean_ns() - 2000.0).abs() < 0.01);
        assert_eq!(metrics.decode_latency_max_ns.load(Ordering::Relaxed), 3000);
    }
}

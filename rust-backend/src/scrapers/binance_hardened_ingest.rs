//! Hardened Binance Market Data Ingest
//!
//! Production-grade wrapper around the HFT ingest that provides:
//! - State machine-driven connection lifecycle
//! - Exponential backoff with jitter (thundering herd prevention)
//! - Endpoint rotation with circuit breakers
//! - Heartbeat monitoring (ping/pong + data staleness)
//! - Proactive reconnection before 24h hard limit
//! - State resync coordination post-reconnect
//! - Zero-overhead hot path (all management on cold path)
//!
//! Usage:
//! ```ignore
//! let config = SessionConfig::from_env();
//! let ingest = HardenedBinanceIngest::new(config);
//! ingest.start();
//!
//! // Read latest prices (lock-free)
//! if let Some(tick) = ingest.latest("BTCUSDT") {
//!     if ingest.is_symbol_tradeable("BTCUSDT") {
//!         // Safe to trade
//!     }
//! }
//! ```

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use parking_lot::{Mutex, RwLock};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{debug, error, info, trace, warn};

use super::binance_session::{
    HeartbeatAction, SessionConfig, SessionManager, SessionState, TransitionReason,
};

// =============================================================================
// CONFIGURATION
// =============================================================================

/// Configuration for the hardened ingest
#[derive(Debug, Clone)]
pub struct HardenedIngestConfig {
    /// Session management config
    pub session: SessionConfig,
    /// Symbols to subscribe to
    pub symbols: Vec<String>,
    /// CPU core to pin ingest thread to (None = no pinning)
    pub pin_to_core: Option<usize>,
    /// EWMA lambda for volatility calculation
    pub ewma_lambda: f64,
}

impl Default for HardenedIngestConfig {
    fn default() -> Self {
        Self {
            session: SessionConfig::from_env(),
            symbols: vec![
                "BTCUSDT".to_string(),
                "ETHUSDT".to_string(),
                "SOLUSDT".to_string(),
                "XRPUSDT".to_string(),
            ],
            pin_to_core: None,
            ewma_lambda: 0.97,
        }
    }
}

// =============================================================================
// PRICE TICK (Cache-Line Aligned)
// =============================================================================

/// Cache line size for alignment
const CACHE_LINE: usize = 64;

/// A single price tick with all relevant data
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct PriceTick {
    pub exchange_ts_ms: i64,
    pub receive_ts_ns: u64,
    pub bid: f64,
    pub ask: f64,
    pub mid: f64,
    pub bid_qty: f64,
    pub ask_qty: f64,
    pub seq: u64,
}

impl PriceTick {
    #[inline(always)]
    pub fn spread_bps(&self) -> f64 {
        if self.mid > 0.0 {
            ((self.ask - self.bid) / self.mid) * 10_000.0
        } else {
            0.0
        }
    }

    #[inline(always)]
    pub fn is_valid(&self) -> bool {
        self.bid > 0.0 && self.ask > 0.0 && self.ask >= self.bid && self.mid > 0.0
    }

    #[inline(always)]
    pub fn age_ns(&self, now_ns: u64) -> u64 {
        now_ns.saturating_sub(self.receive_ts_ns)
    }
}

// =============================================================================
// SEQLOCK SNAPSHOT (Lock-Free Reads)
// =============================================================================

/// SeqLock-protected snapshot for a single symbol
/// Note: PriceTick is 64 bytes, so total struct is 72 bytes (seq + tick); 
/// we align to 128 bytes (2 cache lines) to avoid false sharing.
#[repr(C, align(128))]
pub struct SeqLockSnapshot {
    seq: AtomicU64,
    tick: std::cell::UnsafeCell<PriceTick>,
    // PriceTick = 64 bytes, seq = 8 bytes = 72 bytes; pad to 128
    _pad: [u8; 128 - 8 - 64],
}

unsafe impl Sync for SeqLockSnapshot {}
unsafe impl Send for SeqLockSnapshot {}

impl SeqLockSnapshot {
    pub fn new() -> Self {
        Self {
            seq: AtomicU64::new(0),
            tick: std::cell::UnsafeCell::new(PriceTick::default()),
            _pad: [0; 128 - 8 - 64],
        }
    }

    #[inline(always)]
    pub fn write(&self, tick: PriceTick) {
        let old_seq = self.seq.load(Ordering::Relaxed);
        self.seq.store(old_seq + 1, Ordering::Release);
        std::sync::atomic::fence(Ordering::Release);
        unsafe { *self.tick.get() = tick };
        std::sync::atomic::fence(Ordering::Release);
        self.seq.store(old_seq + 2, Ordering::Release);
    }

    #[inline(always)]
    pub fn read(&self) -> Option<PriceTick> {
        for _ in 0..10 {
            let seq1 = self.seq.load(Ordering::Acquire);
            if seq1 & 1 == 1 {
                std::hint::spin_loop();
                continue;
            }
            if seq1 == 0 {
                return None;
            }
            let tick = unsafe { *self.tick.get() };
            std::sync::atomic::fence(Ordering::Acquire);
            let seq2 = self.seq.load(Ordering::Acquire);
            if seq1 == seq2 {
                return Some(tick);
            }
            std::hint::spin_loop();
        }
        None
    }
}

impl Default for SeqLockSnapshot {
    fn default() -> Self {
        Self::new()
    }
}

// =============================================================================
// PER-SYMBOL STATE
// =============================================================================

const MAX_SYMBOLS: usize = 8;

/// Per-symbol state
pub struct SymbolState {
    pub symbol: String,
    pub latest: SeqLockSnapshot,
    pub ewma_var: AtomicU64,
    pub update_seq: AtomicU64,
    pub total_ticks: AtomicU64,
    pub last_receive_ns: AtomicU64,
}

impl SymbolState {
    pub fn new(symbol: &str) -> Self {
        Self {
            symbol: symbol.to_string(),
            latest: SeqLockSnapshot::new(),
            ewma_var: AtomicU64::new(0),
            update_seq: AtomicU64::new(0),
            total_ticks: AtomicU64::new(0),
            last_receive_ns: AtomicU64::new(0),
        }
    }

    pub fn update(&self, tick: PriceTick, ewma_lambda: f64) {
        if let Some(prev) = self.latest.read() {
            if prev.mid > 0.0 && tick.mid > 0.0 {
                let dt = ((tick.exchange_ts_ms - prev.exchange_ts_ms).max(1)) as f64 / 1000.0;
                let log_return = (tick.mid / prev.mid).ln();
                let var_obs = (log_return * log_return) / dt;
                let prev_var = f64::from_bits(self.ewma_var.load(Ordering::Relaxed));
                let new_var = if prev_var > 0.0 {
                    ewma_lambda * prev_var + (1.0 - ewma_lambda) * var_obs
                } else {
                    var_obs
                };
                if new_var.is_finite() {
                    self.ewma_var.store(new_var.to_bits(), Ordering::Relaxed);
                }
            }
        }
        self.latest.write(tick);
        self.total_ticks.fetch_add(1, Ordering::Relaxed);
        self.last_receive_ns
            .store(tick.receive_ts_ns, Ordering::Relaxed);
        self.update_seq.fetch_add(1, Ordering::Relaxed);
    }

    #[inline]
    pub fn volatility(&self) -> Option<f64> {
        let var = f64::from_bits(self.ewma_var.load(Ordering::Relaxed));
        if var > 0.0 && var.is_finite() {
            Some(var.sqrt())
        } else {
            None
        }
    }
}

// =============================================================================
// INGEST STATISTICS
// =============================================================================

#[derive(Debug, Default)]
pub struct IngestStats {
    pub messages_received: AtomicU64,
    pub messages_processed: AtomicU64,
    pub parse_errors: AtomicU64,
    pub bytes_received: AtomicU64,
    pub last_message_ns: AtomicU64,
    pub latency_buckets: [AtomicU64; 6],
}

impl IngestStats {
    pub fn new() -> Self {
        Self::default()
    }

    #[inline]
    pub fn record_latency(&self, latency_ns: u64) {
        let bucket = if latency_ns < 1_000 {
            0
        } else if latency_ns < 10_000 {
            1
        } else if latency_ns < 100_000 {
            2
        } else if latency_ns < 1_000_000 {
            3
        } else if latency_ns < 10_000_000 {
            4
        } else {
            5
        };
        self.latency_buckets[bucket].fetch_add(1, Ordering::Relaxed);
    }
}

// =============================================================================
// HARDENED BINANCE INGEST
// =============================================================================

/// Production-grade hardened Binance market data ingest
pub struct HardenedBinanceIngest {
    config: HardenedIngestConfig,
    session: Arc<SessionManager>,
    symbols: Vec<SymbolState>,
    symbol_indices: HashMap<String, usize>,
    running: Arc<AtomicBool>,
    thread_handle: Mutex<Option<JoinHandle<()>>>,
    start_instant: Instant,
    pub stats: IngestStats,
}

impl HardenedBinanceIngest {
    pub fn new(config: HardenedIngestConfig) -> Arc<Self> {
        let session = Arc::new(SessionManager::new(
            config.session.clone(),
            config.symbols.clone(),
        ));

        let mut symbol_indices = HashMap::new();
        let mut symbols = Vec::with_capacity(config.symbols.len().min(MAX_SYMBOLS));

        for (i, sym_name) in config.symbols.iter().enumerate().take(MAX_SYMBOLS) {
            symbols.push(SymbolState::new(sym_name));
            symbol_indices.insert(sym_name.clone(), i);
        }

        Arc::new(Self {
            config,
            session,
            symbols,
            symbol_indices,
            running: Arc::new(AtomicBool::new(false)),
            thread_handle: Mutex::new(None),
            start_instant: Instant::now(),
            stats: IngestStats::new(),
        })
    }

    /// Get monotonic nanosecond timestamp
    #[inline(always)]
    pub fn now_ns(&self) -> u64 {
        self.start_instant.elapsed().as_nanos() as u64
    }

    /// Get the latest tick for a symbol (lock-free read)
    #[inline]
    pub fn latest(&self, symbol: &str) -> Option<PriceTick> {
        self.symbol_indices
            .get(symbol)
            .and_then(|&idx| self.symbols[idx].latest.read())
    }

    /// Get the latest mid price for a symbol
    #[inline]
    pub fn mid(&self, symbol: &str) -> Option<f64> {
        self.latest(symbol).map(|t| t.mid)
    }

    /// Get volatility estimate for a symbol
    #[inline]
    pub fn volatility(&self, symbol: &str) -> Option<f64> {
        self.symbol_indices
            .get(symbol)
            .and_then(|&idx| self.symbols[idx].volatility())
    }

    /// Check if a symbol's data is stale
    #[inline]
    pub fn is_stale(&self, symbol: &str, max_age_ns: u64) -> bool {
        self.latest(symbol)
            .map(|t| t.age_ns(self.now_ns()) > max_age_ns)
            .unwrap_or(true)
    }

    /// Check if trading is allowed for a symbol (resync complete + not stale)
    #[inline]
    pub fn is_symbol_tradeable(&self, symbol: &str) -> bool {
        if let Some(&idx) = self.symbol_indices.get(symbol) {
            self.session.is_symbol_tradeable(idx)
                && !self.is_stale(symbol, self.config.session.stale_data_timeout_ms * 1_000_000)
        } else {
            false
        }
    }

    /// Get session state
    pub fn session_state(&self) -> SessionState {
        self.session.state()
    }

    /// Get session metrics
    pub fn session_metrics(&self) -> &super::binance_session::SessionMetrics {
        self.session.metrics()
    }

    /// Start the ingest
    pub fn start(self: &Arc<Self>) {
        let mut handle = self.thread_handle.lock();
        if handle.is_some() {
            warn!("Ingest thread already running");
            return;
        }

        self.running.store(true, Ordering::SeqCst);
        self.session
            .transition(SessionState::Connecting, TransitionReason::Started);

        let engine = self.clone();
        let thread = thread::Builder::new()
            .name("binance-hardened-ingest".to_string())
            .spawn(move || {
                engine.ingest_loop();
            })
            .expect("Failed to spawn ingest thread");

        *handle = Some(thread);
        info!(
            symbols = ?self.config.symbols,
            "hardened_ingest_started"
        );
    }

    /// Stop the ingest
    pub fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);
        self.session
            .transition(SessionState::Shutdown, TransitionReason::ShutdownRequested);

        if let Some(handle) = self.thread_handle.lock().take() {
            let _ = handle.join();
        }
        info!(
            metrics = %self.session.metrics().summary(),
            "hardened_ingest_stopped"
        );
    }

    /// Check if running
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    /// Main ingest loop
    fn ingest_loop(self: Arc<Self>) {
        // Pin to core if configured (Linux only)
        #[cfg(target_os = "linux")]
        if let Some(core) = self.config.pin_to_core {
            unsafe {
                let mut cpuset: libc::cpu_set_t = std::mem::zeroed();
                libc::CPU_SET(core, &mut cpuset);
                if libc::sched_setaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &cpuset) == 0
                {
                    info!(core, "pinned_to_core");
                } else {
                    warn!(core, "failed_to_pin_core");
                }
            }
        }

        // Create tokio runtime for this thread
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("Failed to create tokio runtime");

        // Main loop with state machine
        while self.running.load(Ordering::Relaxed) {
            let state = self.session.state();

            match state {
                SessionState::Connecting | SessionState::Subscribing | SessionState::Streaming => {
                    match rt.block_on(self.run_connection()) {
                        Ok(reason) => {
                            // Clean exit (proactive refresh or shutdown)
                            if reason == TransitionReason::ProactiveRefresh {
                                self.session
                                    .transition(SessionState::Connecting, reason);
                            }
                        }
                        Err(e) => {
                            error!(error = %e, "connection_error");
                            self.session.transition(
                                SessionState::Reconnecting,
                                TransitionReason::NetworkError,
                            );
                        }
                    }
                }
                SessionState::Reconnecting => {
                    let backoff = self.session.next_backoff();
                    info!(
                        backoff_ms = backoff.as_millis(),
                        attempt = self.session.backoff_attempt(),
                        endpoint = self.session.current_endpoint(),
                        "reconnect_backoff"
                    );
                    thread::sleep(backoff);
                    self.session
                        .transition(SessionState::Connecting, TransitionReason::Started);
                }
                SessionState::Init => {
                    self.session
                        .transition(SessionState::Connecting, TransitionReason::Started);
                }
                SessionState::Shutdown => {
                    break;
                }
            }
        }
    }

    /// Run a single connection lifecycle
    async fn run_connection(&self) -> Result<TransitionReason> {
        // Build URL for combined streams
        let endpoint = self.session.current_endpoint();
        let streams: Vec<String> = self
            .config
            .symbols
            .iter()
            .map(|s| format!("{}@bookTicker", s.to_lowercase()))
            .collect();
        let url = format!("{}/stream?streams={}", endpoint, streams.join("/"));

        debug!(url = %url, "connecting");

        // Connect with timeout
        let connect_result = tokio::time::timeout(
            self.session.connect_timeout(),
            connect_async(&url),
        )
        .await;

        let (ws_stream, _response) = match connect_result {
            Ok(Ok((ws, resp))) => (ws, resp),
            Ok(Err(e)) => {
                self.session
                    .transition(SessionState::Reconnecting, TransitionReason::NetworkError);
                return Err(e.into());
            }
            Err(_) => {
                self.session.transition(
                    SessionState::Reconnecting,
                    TransitionReason::ConnectTimeout,
                );
                return Err(anyhow::anyhow!("connect timeout"));
            }
        };

        self.session
            .transition(SessionState::Subscribing, TransitionReason::ConnectSuccess);

        let (mut write, mut read) = ws_stream.split();

        // Binance combined streams auto-subscribe, but we wait for first message as "subscription ACK"
        let subscribe_timeout = self.session.subscribe_timeout();

        let first_msg = tokio::time::timeout(subscribe_timeout, read.next()).await;

        match first_msg {
            Ok(Some(Ok(_))) => {
                self.session
                    .transition(SessionState::Streaming, TransitionReason::SubscribeSuccess);
            }
            _ => {
                self.session.transition(
                    SessionState::Reconnecting,
                    TransitionReason::SubscribeTimeout,
                );
                return Err(anyhow::anyhow!("subscribe timeout"));
            }
        }

        // Main streaming loop with heartbeat monitoring
        let mut heartbeat_check = tokio::time::interval(Duration::from_millis(500));

        loop {
            if !self.running.load(Ordering::Relaxed) {
                return Ok(TransitionReason::ShutdownRequested);
            }

            // Check for proactive refresh
            if self.session.needs_proactive_refresh() {
                info!("proactive_refresh_triggered");
                return Ok(TransitionReason::ProactiveRefresh);
            }

            // Check for hard timeout
            if self.session.is_hard_timeout() {
                warn!("hard_timeout_triggered");
                self.session
                    .transition(SessionState::Reconnecting, TransitionReason::HardTimeout);
                return Err(anyhow::anyhow!("hard timeout"));
            }

            tokio::select! {
                // Process incoming messages
                msg = read.next() => {
                    let receive_ns = self.now_ns();

                    match msg {
                        Some(Ok(Message::Text(text))) => {
                            self.stats.messages_received.fetch_add(1, Ordering::Relaxed);
                            self.stats.bytes_received.fetch_add(text.len() as u64, Ordering::Relaxed);

                            if let Some(parsed) = self.parse_book_ticker(&text) {
                                if let Some(&idx) = self.symbol_indices.get(&parsed.symbol) {
                                    let tick = PriceTick {
                                        exchange_ts_ms: parsed.timestamp,
                                        receive_ts_ns: receive_ns,
                                        bid: parsed.bid,
                                        ask: parsed.ask,
                                        mid: (parsed.bid + parsed.ask) / 2.0,
                                        bid_qty: parsed.bid_qty,
                                        ask_qty: parsed.ask_qty,
                                        seq: self.symbols[idx].update_seq.load(Ordering::Relaxed) + 1,
                                    };

                                    self.symbols[idx].update(tick, self.config.ewma_lambda);
                                    self.session.record_data_received(idx);

                                    self.stats.messages_processed.fetch_add(1, Ordering::Relaxed);
                                    self.stats.last_message_ns.store(receive_ns, Ordering::Relaxed);

                                    let latency_ns = self.now_ns().saturating_sub(receive_ns);
                                    self.stats.record_latency(latency_ns);
                                }
                            } else {
                                self.stats.parse_errors.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                        Some(Ok(Message::Ping(payload))) => {
                            let _ = write.send(Message::Pong(payload)).await;
                        }
                        Some(Ok(Message::Pong(_))) => {
                            self.session.record_pong_received();
                        }
                        Some(Ok(Message::Close(frame))) => {
                            info!(?frame, "server_close");
                            self.session.transition(SessionState::Reconnecting, TransitionReason::ServerClose);
                            return Err(anyhow::anyhow!("server closed connection"));
                        }
                        Some(Err(e)) => {
                            error!(error = %e, "ws_error");
                            self.session.transition(SessionState::Reconnecting, TransitionReason::NetworkError);
                            return Err(e.into());
                        }
                        None => {
                            warn!("stream_ended");
                            self.session.transition(SessionState::Reconnecting, TransitionReason::ServerClose);
                            return Err(anyhow::anyhow!("stream ended"));
                        }
                        _ => {}
                    }
                }

                // Periodic heartbeat check
                _ = heartbeat_check.tick() => {
                    match self.session.check_heartbeat() {
                        HeartbeatAction::Ok => {}
                        HeartbeatAction::SendPing => {
                            if let Err(e) = write.send(Message::Ping(vec![])).await {
                                warn!(error = %e, "ping_send_failed");
                            } else {
                                self.session.record_ping_sent();
                            }
                        }
                        HeartbeatAction::PongTimeout => {
                            warn!("pong_timeout");
                            self.session.transition(SessionState::Reconnecting, TransitionReason::PongTimeout);
                            return Err(anyhow::anyhow!("pong timeout"));
                        }
                        HeartbeatAction::DataStale => {
                            warn!("data_stale");
                            self.session.transition(SessionState::Reconnecting, TransitionReason::DataStale);
                            return Err(anyhow::anyhow!("data stale"));
                        }
                    }
                }
            }
        }
    }

    /// Parse bookTicker message (zero-allocation where possible)
    fn parse_book_ticker(&self, msg: &str) -> Option<ParsedBookTicker> {
        let data_start = msg.find("\"data\":")?;
        let data_content = &msg[data_start + 7..];

        let s_start = data_content.find("\"s\":\"")?;
        let s_value_start = s_start + 5;
        let s_end = data_content[s_value_start..].find('"')?;
        let symbol = &data_content[s_value_start..s_value_start + s_end];

        let b_start = data_content.find("\"b\":\"")?;
        let b_value_start = b_start + 5;
        let b_end = data_content[b_value_start..].find('"')?;
        let bid: f64 = data_content[b_value_start..b_value_start + b_end]
            .parse()
            .ok()?;

        let bq_start = data_content.find("\"B\":\"")?;
        let bq_value_start = bq_start + 5;
        let bq_end = data_content[bq_value_start..].find('"')?;
        let bid_qty: f64 = data_content[bq_value_start..bq_value_start + bq_end]
            .parse()
            .ok()?;

        let a_start = data_content.find("\"a\":\"")?;
        let a_value_start = a_start + 5;
        let a_end = data_content[a_value_start..].find('"')?;
        let ask: f64 = data_content[a_value_start..a_value_start + a_end]
            .parse()
            .ok()?;

        let aq_start = data_content.find("\"A\":\"")?;
        let aq_value_start = aq_start + 5;
        let aq_end = data_content[aq_value_start..].find('"')?;
        let ask_qty: f64 = data_content[aq_value_start..aq_value_start + aq_end]
            .parse()
            .ok()?;

        let timestamp = if let Some(t_start) = data_content.find("\"T\":") {
            let t_value_start = t_start + 4;
            let t_end = data_content[t_value_start..]
                .find(|c: char| !c.is_ascii_digit())
                .unwrap_or(data_content.len() - t_value_start);
            data_content[t_value_start..t_value_start + t_end]
                .parse()
                .unwrap_or(0)
        } else {
            chrono::Utc::now().timestamp_millis()
        };

        Some(ParsedBookTicker {
            symbol: symbol.to_uppercase(),
            bid,
            bid_qty,
            ask,
            ask_qty,
            timestamp,
        })
    }
}

struct ParsedBookTicker {
    symbol: String,
    bid: f64,
    bid_qty: f64,
    ask: f64,
    ask_qty: f64,
    timestamp: i64,
}

impl Drop for HardenedBinanceIngest {
    fn drop(&mut self) {
        self.running.store(false, Ordering::SeqCst);
    }
}

// =============================================================================
// READER HANDLE
// =============================================================================

/// Lightweight handle for consumers to read snapshots
#[derive(Clone)]
pub struct IngestReader {
    engine: Arc<HardenedBinanceIngest>,
}

impl IngestReader {
    pub fn new(engine: Arc<HardenedBinanceIngest>) -> Self {
        Self { engine }
    }

    #[inline]
    pub fn latest(&self, symbol: &str) -> Option<PriceTick> {
        self.engine.latest(symbol)
    }

    #[inline]
    pub fn mid(&self, symbol: &str) -> Option<f64> {
        self.engine.mid(symbol)
    }

    #[inline]
    pub fn volatility(&self, symbol: &str) -> Option<f64> {
        self.engine.volatility(symbol)
    }

    #[inline]
    pub fn is_stale(&self, symbol: &str, max_age_ns: u64) -> bool {
        self.engine.is_stale(symbol, max_age_ns)
    }

    #[inline]
    pub fn is_symbol_tradeable(&self, symbol: &str) -> bool {
        self.engine.is_symbol_tradeable(symbol)
    }

    #[inline]
    pub fn now_ns(&self) -> u64 {
        self.engine.now_ns()
    }

    pub fn session_state(&self) -> SessionState {
        self.engine.session_state()
    }

    pub fn stats(&self) -> &IngestStats {
        &self.engine.stats
    }
}

// =============================================================================
// TESTS
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_seqlock_read_write() {
        let snapshot = SeqLockSnapshot::new();
        assert!(snapshot.read().is_none());

        let tick = PriceTick {
            exchange_ts_ms: 1000,
            receive_ts_ns: 2000,
            bid: 100.0,
            ask: 101.0,
            mid: 100.5,
            bid_qty: 1.0,
            ask_qty: 2.0,
            seq: 1,
        };
        snapshot.write(tick);

        let read_tick = snapshot.read().unwrap();
        assert_eq!(read_tick.mid, 100.5);
    }

    #[test]
    fn test_symbol_state_update() {
        let state = SymbolState::new("BTCUSDT");
        assert_eq!(state.symbol, "BTCUSDT");

        let tick = PriceTick {
            exchange_ts_ms: 1000,
            receive_ts_ns: 2000,
            bid: 50000.0,
            ask: 50001.0,
            mid: 50000.5,
            bid_qty: 1.0,
            ask_qty: 2.0,
            seq: 1,
        };

        state.update(tick, 0.97);

        let latest = state.latest.read().unwrap();
        assert_eq!(latest.mid, 50000.5);
        assert_eq!(state.total_ticks.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn test_ingest_creation() {
        let config = HardenedIngestConfig::default();
        let ingest = HardenedBinanceIngest::new(config);

        assert_eq!(ingest.session_state(), SessionState::Init);
        assert!(!ingest.is_running());
    }

    #[test]
    fn test_parse_book_ticker() {
        let config = HardenedIngestConfig::default();
        let ingest = HardenedBinanceIngest::new(config);

        let msg = r#"{"stream":"btcusdt@bookTicker","data":{"s":"BTCUSDT","b":"50000.00","B":"1.5","a":"50001.00","A":"2.0","T":1234567890123}}"#;
        let parsed = ingest.parse_book_ticker(msg).unwrap();

        assert_eq!(parsed.symbol, "BTCUSDT");
        assert_eq!(parsed.bid, 50000.0);
        assert_eq!(parsed.ask, 50001.0);
        assert_eq!(parsed.timestamp, 1234567890123);
    }

    #[test]
    fn test_reader() {
        let config = HardenedIngestConfig::default();
        let ingest = HardenedBinanceIngest::new(config);
        let reader = IngestReader::new(ingest.clone());

        assert!(reader.latest("BTCUSDT").is_none());
        assert!(reader.is_stale("BTCUSDT", 1_000_000));
        assert!(!reader.is_symbol_tradeable("BTCUSDT"));
    }
}

//! Zero-Overhead Binance Market Data Ingest
//!
//! This module implements a high-performance market data ingest path with:
//! 1. **Dedicated pinned thread** - OS thread with CPU affinity for deterministic latency
//! 2. **Preallocated buffers** - Arena-style allocation, no heap in hot path
//! 3. **SeqLock snapshots** - Lock-free last-value semantics with torn-read detection
//! 4. **No broadcast fanout** - Slow consumers cannot create lag
//! 5. **Zero-copy decode** - Parse directly into preallocated structs
//!
//! Design principles:
//! - Single writer (ingest thread), multiple readers (trading strategies)
//! - Readers always see latest value, may skip intermediate updates
//! - No allocations after initialization
//! - Cache-line aligned data structures

use std::{
    cell::UnsafeCell,
    hint,
    mem::MaybeUninit,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use parking_lot::Mutex;
use tracing::{debug, error, info, trace, warn};

// ============================================================================
// Constants & Configuration
// ============================================================================

/// Maximum number of symbols we track (fixed at compile time for no-alloc)
pub const MAX_SYMBOLS: usize = 8;

/// Ring buffer size for history (power of 2 for fast modulo)
const HISTORY_SIZE: usize = 4096;

/// Cache line size for alignment
const CACHE_LINE: usize = 64;

/// Preallocated WebSocket receive buffer size
const WS_RECV_BUFFER_SIZE: usize = 8192;

/// Maximum JSON message size we'll process
const MAX_MESSAGE_SIZE: usize = 4096;

// ============================================================================
// Core Data Structures (Cache-Line Aligned)
// ============================================================================

/// A single price tick with all relevant data
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct PriceTick {
    /// Exchange timestamp (milliseconds since epoch)
    pub exchange_ts_ms: i64,
    /// Local receive timestamp (nanoseconds, monotonic)
    pub receive_ts_ns: u64,
    /// Best bid price
    pub bid: f64,
    /// Best ask price
    pub ask: f64,
    /// Mid price ((bid + ask) / 2)
    pub mid: f64,
    /// Bid quantity at best
    pub bid_qty: f64,
    /// Ask quantity at best
    pub ask_qty: f64,
    /// Sequence number (monotonically increasing per symbol)
    pub seq: u64,
}

impl PriceTick {
    #[inline(always)]
    pub fn spread(&self) -> f64 {
        self.ask - self.bid
    }

    #[inline(always)]
    pub fn spread_bps(&self) -> f64 {
        if self.mid > 0.0 {
            (self.spread() / self.mid) * 10_000.0
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

/// SeqLock-protected snapshot for a single symbol
///
/// Uses sequence counter for torn-read detection:
/// - Writer increments seq to odd before write, even after
/// - Reader retries if seq changes or is odd during read
/// Padding size calculation for SeqLockSnapshot
/// AtomicU64 = 8 bytes, PriceTick = 64 bytes, total = 72 bytes
/// With align(64), we want to fill to 128 bytes (2 cache lines) for safety
const SEQLOCK_PAD_SIZE: usize = 128 - 8 - 64; // = 56 bytes

#[repr(C, align(64))]
pub struct SeqLockSnapshot {
    /// Sequence counter (odd = write in progress)
    seq: AtomicU64,
    /// The actual tick data (UnsafeCell for interior mutability)
    tick: UnsafeCell<PriceTick>,
    /// Padding to prevent false sharing
    _pad: [u8; SEQLOCK_PAD_SIZE],
}

// SAFETY: SeqLock provides safe concurrent access via sequence counter protocol
unsafe impl Sync for SeqLockSnapshot {}
unsafe impl Send for SeqLockSnapshot {}

impl SeqLockSnapshot {
    pub const fn new() -> Self {
        Self {
            seq: AtomicU64::new(0),
            tick: UnsafeCell::new(PriceTick {
                exchange_ts_ms: 0,
                receive_ts_ns: 0,
                bid: 0.0,
                ask: 0.0,
                mid: 0.0,
                bid_qty: 0.0,
                ask_qty: 0.0,
                seq: 0,
            }),
            _pad: [0; SEQLOCK_PAD_SIZE],
        }
    }

    /// Write a new tick (single writer only!)
    ///
    /// SAFETY: Must only be called from the single ingest thread
    #[inline(always)]
    pub fn write(&self, tick: PriceTick) {
        // Increment to odd (write in progress)
        let old_seq = self.seq.load(Ordering::Relaxed);
        self.seq.store(old_seq + 1, Ordering::Release);

        // Compiler fence to prevent reordering
        std::sync::atomic::fence(Ordering::Release);

        // Write the data
        // SAFETY: Single writer guarantee from caller
        unsafe {
            *self.tick.get() = tick;
        }

        // Compiler fence
        std::sync::atomic::fence(Ordering::Release);

        // Increment to even (write complete)
        self.seq.store(old_seq + 2, Ordering::Release);
    }

    /// Read the latest tick (may retry on torn read)
    ///
    /// Returns None if no data has been written yet
    #[inline(always)]
    pub fn read(&self) -> Option<PriceTick> {
        const MAX_RETRIES: u32 = 10;
        let mut retries = 0;

        loop {
            // Read sequence before
            let seq1 = self.seq.load(Ordering::Acquire);

            // If odd, write in progress - spin
            if seq1 & 1 == 1 {
                hint::spin_loop();
                retries += 1;
                if retries > MAX_RETRIES {
                    return None; // Give up
                }
                continue;
            }

            // If zero, never written
            if seq1 == 0 {
                return None;
            }

            // Read the data
            // SAFETY: SeqLock protocol ensures consistency
            let tick = unsafe { *self.tick.get() };

            // Compiler fence
            std::sync::atomic::fence(Ordering::Acquire);

            // Read sequence after
            let seq2 = self.seq.load(Ordering::Acquire);

            // If unchanged, read was consistent
            if seq1 == seq2 {
                return Some(tick);
            }

            // Torn read - retry
            hint::spin_loop();
            retries += 1;
            if retries > MAX_RETRIES {
                return None;
            }
        }
    }

    /// Get the current sequence number (for checking staleness)
    #[inline(always)]
    pub fn seq(&self) -> u64 {
        self.seq.load(Ordering::Acquire)
    }
}

/// Per-symbol state including snapshot and history ring buffer
#[repr(C, align(64))]
pub struct SymbolState {
    /// Symbol name (e.g., "BTCUSDT") - fixed size, no allocation
    pub symbol: [u8; 16],
    pub symbol_len: usize,
    /// Latest tick (SeqLock protected)
    pub latest: SeqLockSnapshot,
    /// EWMA volatility (variance of log returns)
    pub ewma_var: AtomicU64, // Stored as f64 bits
    /// Last update sequence
    pub update_seq: AtomicU64,
    /// Circular history buffer for lookback
    history: UnsafeCell<[PriceTick; HISTORY_SIZE]>,
    history_head: AtomicU64,
    /// Statistics
    pub total_ticks: AtomicU64,
    pub last_receive_ns: AtomicU64,
}

// SAFETY: Access patterns are thread-safe (single writer for history)
unsafe impl Sync for SymbolState {}
unsafe impl Send for SymbolState {}

impl SymbolState {
    pub fn new(symbol: &str) -> Self {
        let mut sym_bytes = [0u8; 16];
        let len = symbol.len().min(16);
        sym_bytes[..len].copy_from_slice(&symbol.as_bytes()[..len]);

        Self {
            symbol: sym_bytes,
            symbol_len: len,
            latest: SeqLockSnapshot::new(),
            ewma_var: AtomicU64::new(0),
            update_seq: AtomicU64::new(0),
            history: UnsafeCell::new([PriceTick::default(); HISTORY_SIZE]),
            history_head: AtomicU64::new(0),
            total_ticks: AtomicU64::new(0),
            last_receive_ns: AtomicU64::new(0),
        }
    }

    /// Get the symbol as a string slice
    #[inline]
    pub fn symbol_str(&self) -> &str {
        // SAFETY: We only store valid UTF-8
        unsafe { std::str::from_utf8_unchecked(&self.symbol[..self.symbol_len]) }
    }

    /// Update with a new tick (single writer only!)
    ///
    /// SAFETY: Must only be called from the single ingest thread
    pub fn update(&self, tick: PriceTick, ewma_lambda: f64) {
        // Update EWMA variance
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

        // Write to SeqLock
        self.latest.write(tick);

        // Append to history (single writer, lock-free)
        let head = self.history_head.load(Ordering::Relaxed);
        let idx = (head as usize) & (HISTORY_SIZE - 1);
        // SAFETY: Single writer guarantee
        unsafe {
            (*self.history.get())[idx] = tick;
        }
        self.history_head.store(head + 1, Ordering::Release);

        // Update stats
        self.total_ticks.fetch_add(1, Ordering::Relaxed);
        self.last_receive_ns.store(tick.receive_ts_ns, Ordering::Relaxed);
        self.update_seq.fetch_add(1, Ordering::Relaxed);
    }

    /// Get historical tick at relative offset (0 = latest, 1 = previous, etc.)
    pub fn history_at(&self, offset: usize) -> Option<PriceTick> {
        if offset >= HISTORY_SIZE {
            return None;
        }

        let head = self.history_head.load(Ordering::Acquire);
        if head == 0 {
            return None;
        }

        let idx = ((head - 1 - offset as u64) as usize) & (HISTORY_SIZE - 1);
        // SAFETY: Bounded index, atomic head load
        let tick = unsafe { (*self.history.get())[idx] };

        if tick.receive_ts_ns > 0 {
            Some(tick)
        } else {
            None
        }
    }

    /// Get EWMA volatility (sqrt of variance)
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

// ============================================================================
// Ingest Engine
// ============================================================================

/// Configuration for the ingest engine
#[derive(Debug, Clone)]
pub struct IngestConfig {
    /// Symbols to subscribe to
    pub symbols: Vec<String>,
    /// CPU core to pin the ingest thread to (None = no pinning)
    pub pin_to_core: Option<usize>,
    /// EWMA lambda for volatility calculation
    pub ewma_lambda: f64,
    /// WebSocket URL
    pub ws_url: String,
    /// Reconnect delay range
    pub reconnect_min_ms: u64,
    pub reconnect_max_ms: u64,
    /// Enable busy-polling mode (higher CPU, lower latency)
    pub busy_poll: bool,
}

impl Default for IngestConfig {
    fn default() -> Self {
        Self {
            symbols: vec![
                "BTCUSDT".to_string(),
                "ETHUSDT".to_string(),
                "SOLUSDT".to_string(),
                "XRPUSDT".to_string(),
            ],
            pin_to_core: None,
            ewma_lambda: 0.97,
            ws_url: "wss://stream.binance.com:9443/ws".to_string(),
            reconnect_min_ms: 100,
            reconnect_max_ms: 30_000,
            busy_poll: false,
        }
    }
}

/// The main ingest engine with lock-free snapshot store
pub struct BinanceHftIngest {
    /// Per-symbol state (fixed array, no allocation)
    symbols: Box<[SymbolState; MAX_SYMBOLS]>,
    /// Symbol name to index mapping
    symbol_indices: std::collections::HashMap<String, usize>,
    /// Number of active symbols
    num_symbols: usize,
    /// Running flag
    running: Arc<AtomicBool>,
    /// Ingest thread handle
    thread_handle: Mutex<Option<JoinHandle<()>>>,
    /// Configuration
    config: IngestConfig,
    /// Start time for monotonic timestamps
    start_instant: Instant,
    /// Statistics
    pub stats: IngestStats,
}

/// Statistics for monitoring
#[repr(C, align(64))]
pub struct IngestStats {
    pub messages_received: AtomicU64,
    pub messages_processed: AtomicU64,
    pub parse_errors: AtomicU64,
    pub reconnect_count: AtomicU64,
    pub bytes_received: AtomicU64,
    pub last_message_ns: AtomicU64,
    /// Processing latency histogram buckets (1us, 10us, 100us, 1ms, 10ms, 100ms+)
    pub latency_buckets: [AtomicU64; 6],
}

impl Default for IngestStats {
    fn default() -> Self {
        Self {
            messages_received: AtomicU64::new(0),
            messages_processed: AtomicU64::new(0),
            parse_errors: AtomicU64::new(0),
            reconnect_count: AtomicU64::new(0),
            bytes_received: AtomicU64::new(0),
            last_message_ns: AtomicU64::new(0),
            latency_buckets: [
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
            ],
        }
    }
}

impl IngestStats {
    #[inline]
    pub fn record_latency(&self, latency_ns: u64) {
        let bucket = if latency_ns < 1_000 {
            0 // < 1us
        } else if latency_ns < 10_000 {
            1 // 1-10us
        } else if latency_ns < 100_000 {
            2 // 10-100us
        } else if latency_ns < 1_000_000 {
            3 // 100us-1ms
        } else if latency_ns < 10_000_000 {
            4 // 1-10ms
        } else {
            5 // 10ms+
        };
        self.latency_buckets[bucket].fetch_add(1, Ordering::Relaxed);
    }
}

impl BinanceHftIngest {
    /// Create a new ingest engine with the given configuration
    pub fn new(config: IngestConfig) -> Arc<Self> {
        // Initialize symbol states
        let mut symbols: Box<[MaybeUninit<SymbolState>; MAX_SYMBOLS]> =
            Box::new(unsafe { MaybeUninit::uninit().assume_init() });

        let mut symbol_indices = std::collections::HashMap::new();

        for (i, sym_name) in config.symbols.iter().enumerate().take(MAX_SYMBOLS) {
            symbols[i].write(SymbolState::new(sym_name));
            symbol_indices.insert(sym_name.clone(), i);
        }

        // Fill remaining slots with empty states
        for i in config.symbols.len()..MAX_SYMBOLS {
            symbols[i].write(SymbolState::new(""));
        }

        // SAFETY: All elements initialized
        let symbols: Box<[SymbolState; MAX_SYMBOLS]> =
            unsafe { std::mem::transmute(symbols) };

        Arc::new(Self {
            symbols,
            symbol_indices,
            num_symbols: config.symbols.len().min(MAX_SYMBOLS),
            running: Arc::new(AtomicBool::new(false)),
            thread_handle: Mutex::new(None),
            config,
            start_instant: Instant::now(),
            stats: IngestStats::default(),
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

    /// Get symbol state by name
    pub fn symbol_state(&self, symbol: &str) -> Option<&SymbolState> {
        self.symbol_indices
            .get(symbol)
            .map(|&idx| &self.symbols[idx])
    }

    /// Start the ingest thread
    pub fn start(self: &Arc<Self>) {
        let mut handle = self.thread_handle.lock();
        if handle.is_some() {
            warn!("Ingest thread already running");
            return;
        }

        self.running.store(true, Ordering::SeqCst);

        let engine = self.clone();
        let thread = thread::Builder::new()
            .name("binance-hft-ingest".to_string())
            .spawn(move || {
                engine.ingest_loop();
            })
            .expect("Failed to spawn ingest thread");

        *handle = Some(thread);
        info!("Binance HFT ingest thread started");
    }

    /// Stop the ingest thread
    pub fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);

        if let Some(handle) = self.thread_handle.lock().take() {
            let _ = handle.join();
        }
        info!("Binance HFT ingest thread stopped");
    }

    /// Check if running
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    /// The main ingest loop (runs on dedicated thread)
    fn ingest_loop(self: Arc<Self>) {
        // Pin to core if configured
        #[cfg(target_os = "linux")]
        if let Some(core) = self.config.pin_to_core {
            use std::os::unix::thread::JoinHandleExt;
            unsafe {
                let mut cpuset: libc::cpu_set_t = std::mem::zeroed();
                libc::CPU_SET(core, &mut cpuset);
                let result = libc::sched_setaffinity(
                    0, // current thread
                    std::mem::size_of::<libc::cpu_set_t>(),
                    &cpuset,
                );
                if result == 0 {
                    info!("Pinned ingest thread to core {}", core);
                } else {
                    warn!("Failed to pin to core {}: {}", core, std::io::Error::last_os_error());
                }
            }
        }

        // Preallocated receive buffer (no allocations in hot loop)
        let mut recv_buffer = vec![0u8; WS_RECV_BUFFER_SIZE];

        // Create tokio runtime for this thread (WebSocket needs async)
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("Failed to create tokio runtime");

        // Reconnect loop
        let mut reconnect_delay = Duration::from_millis(self.config.reconnect_min_ms);

        while self.running.load(Ordering::Relaxed) {
            match rt.block_on(self.run_connection(&mut recv_buffer)) {
                Ok(()) => {
                    reconnect_delay = Duration::from_millis(self.config.reconnect_min_ms);
                }
                Err(e) => {
                    warn!(error = %e, "WebSocket connection error");
                    self.stats.reconnect_count.fetch_add(1, Ordering::Relaxed);
                }
            }

            if self.running.load(Ordering::Relaxed) {
                info!(delay_ms = reconnect_delay.as_millis(), "Reconnecting...");
                thread::sleep(reconnect_delay);
                reconnect_delay = std::cmp::min(
                    reconnect_delay * 2,
                    Duration::from_millis(self.config.reconnect_max_ms),
                );
            }
        }
    }

    /// Run a single WebSocket connection
    async fn run_connection(&self, recv_buffer: &mut [u8]) -> anyhow::Result<()> {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::{connect_async, tungstenite::Message};

        // Build subscription URL for combined streams
        let streams: Vec<String> = self
            .config
            .symbols
            .iter()
            .map(|s| format!("{}@bookTicker", s.to_lowercase()))
            .collect();

        let url = format!("{}/stream?streams={}", self.config.ws_url, streams.join("/"));

        info!(url = %url, "Connecting to Binance WebSocket");

        let (ws_stream, _) = connect_async(&url).await?;
        let (mut write, mut read) = ws_stream.split();

        info!("Connected to Binance WebSocket");

        // Message processing loop
        while self.running.load(Ordering::Relaxed) {
            match read.next().await {
                Some(Ok(msg)) => {
                    let receive_ns = self.now_ns();
                    self.stats.messages_received.fetch_add(1, Ordering::Relaxed);

                    match msg {
                        Message::Text(text) => {
                            self.stats
                                .bytes_received
                                .fetch_add(text.len() as u64, Ordering::Relaxed);

                            if text.len() <= MAX_MESSAGE_SIZE {
                                self.process_message(&text, receive_ns);
                            }
                        }
                        Message::Binary(data) => {
                            self.stats
                                .bytes_received
                                .fetch_add(data.len() as u64, Ordering::Relaxed);
                        }
                        Message::Ping(payload) => {
                            let _ = write.send(Message::Pong(payload)).await;
                        }
                        Message::Pong(_) => {}
                        Message::Close(_) => {
                            info!("WebSocket closed by server");
                            break;
                        }
                        _ => {}
                    }
                }
                Some(Err(e)) => {
                    return Err(anyhow::anyhow!("WebSocket error: {}", e));
                }
                None => {
                    info!("WebSocket stream ended");
                    break;
                }
            }
        }

        Ok(())
    }

    /// Process a single message (zero-allocation parse where possible)
    /// 
    /// Public for benchmarking purposes.
    #[inline]
    pub fn process_message(&self, msg: &str, receive_ns: u64) {
        let start_ns = self.now_ns();

        // Parse the combined stream wrapper
        // Format: {"stream":"btcusdt@bookTicker","data":{...}}
        let parsed = match self.parse_book_ticker(msg) {
            Some(p) => p,
            None => {
                self.stats.parse_errors.fetch_add(1, Ordering::Relaxed);
                return;
            }
        };

        // Look up symbol index
        let idx = match self.symbol_indices.get(&parsed.symbol) {
            Some(&i) => i,
            None => return, // Unknown symbol, ignore
        };

        // Create tick
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

        // Update symbol state (single writer)
        self.symbols[idx].update(tick, self.config.ewma_lambda);

        // Record metrics
        self.stats.messages_processed.fetch_add(1, Ordering::Relaxed);
        self.stats.last_message_ns.store(receive_ns, Ordering::Relaxed);

        let latency_ns = self.now_ns().saturating_sub(start_ns);
        self.stats.record_latency(latency_ns);

        trace!(
            symbol = parsed.symbol,
            mid = tick.mid,
            latency_ns = latency_ns,
            "Processed tick"
        );
    }

    /// Zero-allocation JSON parsing for bookTicker messages
    ///
    /// Avoids serde_json heap allocations by manual parsing
    #[inline]
    fn parse_book_ticker(&self, msg: &str) -> Option<ParsedBookTicker> {
        // Fast path: look for required fields directly
        // Format: {"stream":"btcusdt@bookTicker","data":{"s":"BTCUSDT","b":"50000.00","B":"1.5","a":"50001.00","A":"2.0","T":1234567890123}}

        // Find the data object
        let data_start = msg.find("\"data\":")?;
        let data_content = &msg[data_start + 7..];

        // Extract symbol
        let s_start = data_content.find("\"s\":\"")?;
        let s_value_start = s_start + 5;
        let s_end = data_content[s_value_start..].find('"')?;
        let symbol = &data_content[s_value_start..s_value_start + s_end];

        // Extract bid price
        let b_start = data_content.find("\"b\":\"")?;
        let b_value_start = b_start + 5;
        let b_end = data_content[b_value_start..].find('"')?;
        let bid: f64 = data_content[b_value_start..b_value_start + b_end]
            .parse()
            .ok()?;

        // Extract bid qty
        let bq_start = data_content.find("\"B\":\"")?;
        let bq_value_start = bq_start + 5;
        let bq_end = data_content[bq_value_start..].find('"')?;
        let bid_qty: f64 = data_content[bq_value_start..bq_value_start + bq_end]
            .parse()
            .ok()?;

        // Extract ask price
        let a_start = data_content.find("\"a\":\"")?;
        let a_value_start = a_start + 5;
        let a_end = data_content[a_value_start..].find('"')?;
        let ask: f64 = data_content[a_value_start..a_value_start + a_end]
            .parse()
            .ok()?;

        // Extract ask qty
        let aq_start = data_content.find("\"A\":\"")?;
        let aq_value_start = aq_start + 5;
        let aq_end = data_content[aq_value_start..].find('"')?;
        let ask_qty: f64 = data_content[aq_value_start..aq_value_start + aq_end]
            .parse()
            .ok()?;

        // Extract timestamp (optional, may be "T" or "E")
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

/// Intermediate parsed result (stack allocated)
struct ParsedBookTicker {
    symbol: String, // TODO: Use fixed-size array to avoid allocation
    bid: f64,
    bid_qty: f64,
    ask: f64,
    ask_qty: f64,
    timestamp: i64,
}

impl Drop for BinanceHftIngest {
    fn drop(&mut self) {
        self.running.store(false, Ordering::SeqCst);
    }
}

// ============================================================================
// Reader Handle for Consumers
// ============================================================================

/// A lightweight handle for consumers to read snapshots
///
/// This is Clone and cheap to pass around
#[derive(Clone)]
pub struct IngestReader {
    engine: Arc<BinanceHftIngest>,
}

impl IngestReader {
    pub fn new(engine: Arc<BinanceHftIngest>) -> Self {
        Self { engine }
    }

    /// Get the latest tick for a symbol
    #[inline]
    pub fn latest(&self, symbol: &str) -> Option<PriceTick> {
        self.engine.latest(symbol)
    }

    /// Get the latest mid price
    #[inline]
    pub fn mid(&self, symbol: &str) -> Option<f64> {
        self.engine.mid(symbol)
    }

    /// Get volatility estimate
    #[inline]
    pub fn volatility(&self, symbol: &str) -> Option<f64> {
        self.engine.volatility(symbol)
    }

    /// Check staleness
    #[inline]
    pub fn is_stale(&self, symbol: &str, max_age_ns: u64) -> bool {
        self.engine.is_stale(symbol, max_age_ns)
    }

    /// Get current monotonic time
    #[inline]
    pub fn now_ns(&self) -> u64 {
        self.engine.now_ns()
    }

    /// Get stats reference
    pub fn stats(&self) -> &IngestStats {
        &self.engine.stats
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_seqlock_basic() {
        let lock = SeqLockSnapshot::new();

        // Initially empty
        assert!(lock.read().is_none());

        // Write a tick
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
        lock.write(tick);

        // Read it back
        let read_tick = lock.read().unwrap();
        assert_eq!(read_tick.mid, 100.5);
        assert_eq!(read_tick.seq, 1);
    }

    #[test]
    fn test_symbol_state() {
        let state = SymbolState::new("BTCUSDT");
        assert_eq!(state.symbol_str(), "BTCUSDT");

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
    }

    #[test]
    fn test_price_tick_spread() {
        let tick = PriceTick {
            exchange_ts_ms: 0,
            receive_ts_ns: 0,
            bid: 100.0,
            ask: 100.01,
            mid: 100.005,
            bid_qty: 0.0,
            ask_qty: 0.0,
            seq: 0,
        };

        assert!((tick.spread() - 0.01).abs() < 1e-10);
        assert!((tick.spread_bps() - 1.0).abs() < 0.1);
    }
}

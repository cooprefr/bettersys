//! Binance Latency Measurement Harness
//!
//! Production-grade instrumentation for quantifying network and processing latency
//! to Binance market-data endpoints from AWS eu-west-1.
//!
//! # Measurement Philosophy
//!
//! This harness strictly separates:
//! - **Monotonic time** (`std::time::Instant`, `quanta::Instant`): Used for all latency deltas.
//!   Immune to NTP drift, leap seconds, and clock adjustments.
//! - **Wall-clock time** (`chrono::Utc`, `SystemTime`): Used only for correlation with external
//!   systems (Binance exchange timestamps) and CSV output timestamps.
//!
//! # Instrumentation Points
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────────────────────┐
//! │                         BINANCE LATENCY BREAKDOWN                            │
//! ├─────────────────────────────────────────────────────────────────────────────┤
//! │                                                                             │
//! │  T0: DNS lookup start (monotonic)                                           │
//! │   │                                                                         │
//! │   ├── L_dns: DNS resolution time                                            │
//! │   │                                                                         │
//! │  T1: TCP SYN sent (monotonic)                                               │
//! │   │                                                                         │
//! │   ├── L_tcp: TCP 3-way handshake (SYN → SYN-ACK → ACK)                      │
//! │   │                                                                         │
//! │  T2: TCP connected (monotonic)                                              │
//! │   │                                                                         │
//! │   ├── L_tls: TLS handshake (ClientHello → ... → Finished)                   │
//! │   │                                                                         │
//! │  T3: TLS established (monotonic)                                            │
//! │   │                                                                         │
//! │   ├── L_ws_upgrade: WebSocket HTTP upgrade                                  │
//! │   │                                                                         │
//! │  T4: WebSocket ready (monotonic)                                            │
//! │   │                                                                         │
//! │   ├── L_subscribe: Subscription ACK latency                                 │
//! │   │                                                                         │
//! │  T5: Subscribed, awaiting data (monotonic)                                  │
//! │                                                                             │
//! │  ════════════════════════════════════════════════════════════════════════   │
//! │  PER-MESSAGE LATENCY (steady state)                                         │
//! │  ════════════════════════════════════════════════════════════════════════   │
//! │                                                                             │
//! │  T_exchange: Binance exchange timestamp (wall-clock, from message)          │
//! │   │                                                                         │
//! │   ├── L_wire: Network transit (one-way, estimated)                          │
//! │   │           = T_kernel_rx - T_exchange (requires clock sync)              │
//! │   │                                                                         │
//! │  T_kernel_rx: Kernel receives frame (estimated via SO_TIMESTAMPING)         │
//! │   │                                                                         │
//! │   ├── L_userspace: Kernel → userspace delivery                              │
//! │   │                                                                         │
//! │  T_recv: tokio-tungstenite returns frame (monotonic)                        │
//! │   │                                                                         │
//! │   ├── L_decode: JSON/SBE parse + validation                                 │
//! │   │                                                                         │
//! │  T_decoded: Structured data available (monotonic)                           │
//! │   │                                                                         │
//! │   ├── L_handoff: Channel send to strategy                                   │
//! │   │                                                                         │
//! │  T_strategy: Strategy receives event (monotonic)                            │
//! │                                                                             │
//! │  TOTAL T2T = L_wire + L_userspace + L_decode + L_handoff                    │
//! │                                                                             │
//! └─────────────────────────────────────────────────────────────────────────────┘
//! ```
//!
//! # One-Way vs Round-Trip
//!
//! - **One-way latency** requires clock synchronization between client and Binance.
//!   We estimate this using `T_recv_wallclock - T_exchange` with NTP correction.
//!   Accuracy depends on NTP sync quality (typically ±1-5ms without PTP).
//!
//! - **Round-trip latency** is measured via WebSocket ping/pong (no clock sync needed).
//!   RTT / 2 gives a lower bound on one-way latency.
//!
//! # CSV Output Schema
//!
//! See `LatencySample` for the canonical schema.

use std::{
    collections::HashMap,
    io::Write,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};

// =============================================================================
// CONSTANTS
// =============================================================================

/// Binance WebSocket endpoints we actually use (Spot L1 orderbooks)
pub const BINANCE_WS_ENDPOINTS: &[&str] = &[
    "wss://stream.binance.com:9443/ws",      // Primary
    "wss://stream.binance.com:443/ws",       // Fallback (standard HTTPS port)
    "wss://data-stream.binance.com:9443/ws", // Alternative data stream
];

/// Symbols we subscribe to
pub const BINANCE_SYMBOLS: &[&str] = &["btcusdt", "ethusdt", "solusdt", "xrpusdt"];

/// Nanoseconds per microsecond
const NS_PER_US: u64 = 1_000;
/// Nanoseconds per millisecond
const NS_PER_MS: u64 = 1_000_000;

// =============================================================================
// MONOTONIC CLOCK ABSTRACTION
// =============================================================================

/// High-resolution monotonic clock for latency measurement.
/// Uses `quanta` when available for TSC-based timing, falls back to `Instant`.
#[derive(Debug, Clone, Copy)]
pub struct MonotonicInstant {
    /// Nanoseconds since arbitrary epoch (process start)
    nanos: u64,
}

impl MonotonicInstant {
    /// Capture current monotonic timestamp
    #[inline]
    pub fn now() -> Self {
        // Use a static reference point for consistent measurements
        static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
        let start = START.get_or_init(Instant::now);
        Self {
            nanos: start.elapsed().as_nanos() as u64,
        }
    }

    /// Nanoseconds since epoch
    #[inline]
    pub fn as_nanos(&self) -> u64 {
        self.nanos
    }

    /// Microseconds since epoch
    #[inline]
    pub fn as_micros(&self) -> u64 {
        self.nanos / NS_PER_US
    }

    /// Duration since another instant (saturating)
    #[inline]
    pub fn duration_since(&self, earlier: Self) -> Duration {
        Duration::from_nanos(self.nanos.saturating_sub(earlier.nanos))
    }

    /// Elapsed since this instant
    #[inline]
    pub fn elapsed(&self) -> Duration {
        Self::now().duration_since(*self)
    }
}

// =============================================================================
// WALL CLOCK ABSTRACTION
// =============================================================================

/// Wall-clock timestamp for correlation with external systems.
/// NOT for latency measurement (use MonotonicInstant instead).
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct WallClockInstant {
    /// Unix timestamp in nanoseconds
    pub unix_nanos: i64,
}

impl WallClockInstant {
    /// Capture current wall-clock time
    #[inline]
    pub fn now() -> Self {
        let dur = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        Self {
            unix_nanos: dur.as_nanos() as i64,
        }
    }

    /// From Unix milliseconds (Binance timestamp format)
    #[inline]
    pub fn from_unix_millis(millis: i64) -> Self {
        Self {
            unix_nanos: millis * 1_000_000,
        }
    }

    /// Unix milliseconds
    #[inline]
    pub fn as_unix_millis(&self) -> i64 {
        self.unix_nanos / 1_000_000
    }

    /// ISO 8601 string for CSV output
    pub fn to_iso8601(&self) -> String {
        let secs = (self.unix_nanos / 1_000_000_000) as i64;
        let nanos = (self.unix_nanos % 1_000_000_000) as u32;
        chrono::DateTime::from_timestamp(secs, nanos)
            .map(|dt| dt.format("%Y-%m-%dT%H:%M:%S%.6fZ").to_string())
            .unwrap_or_else(|| "INVALID".to_string())
    }
}

// =============================================================================
// CONNECTION LATENCY SAMPLE
// =============================================================================

/// Complete connection establishment latency breakdown.
/// Captured once per connection attempt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionLatencySample {
    /// Wall-clock timestamp when measurement started
    pub wall_clock_start: WallClockInstant,
    /// Monotonic timestamp at start (for internal correlation)
    pub mono_start_ns: u64,

    // --- DNS ---
    /// DNS resolution latency (nanoseconds, monotonic)
    /// Measured from: lookup start → first A/AAAA record received
    pub dns_latency_ns: Option<u64>,
    /// Resolved IP address(es)
    pub resolved_addrs: Vec<String>,

    // --- TCP ---
    /// TCP connect latency (nanoseconds, monotonic)
    /// Measured from: connect() call → socket writable (3-way handshake complete)
    pub tcp_connect_latency_ns: u64,
    /// Remote address connected to
    pub remote_addr: String,

    // --- TLS ---
    /// TLS handshake latency (nanoseconds, monotonic)
    /// Measured from: TLS client hello sent → TLS finished received
    pub tls_handshake_latency_ns: u64,
    /// Negotiated TLS version (e.g., "TLSv1.3")
    pub tls_version: String,
    /// Negotiated cipher suite
    pub tls_cipher: String,

    // --- WebSocket ---
    /// WebSocket upgrade latency (nanoseconds, monotonic)
    /// Measured from: HTTP upgrade request sent → 101 Switching Protocols received
    pub ws_upgrade_latency_ns: u64,

    // --- Subscription ---
    /// Subscription acknowledgment latency (nanoseconds, monotonic)
    /// Measured from: subscribe message sent → subscription confirmed
    pub subscribe_latency_ns: Option<u64>,

    // --- Totals ---
    /// Total connection establishment time (dns + tcp + tls + ws_upgrade)
    pub total_connect_latency_ns: u64,

    /// Success flag
    pub success: bool,
    /// Error message if failed
    pub error: Option<String>,
}

// =============================================================================
// MESSAGE LATENCY SAMPLE
// =============================================================================

/// Per-message latency breakdown.
/// Captured for every market data message in measurement mode.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageLatencySample {
    /// Unique sample ID (monotonically increasing)
    pub sample_id: u64,
    /// Symbol (e.g., "BTCUSDT")
    pub symbol: String,

    // --- Wall-clock timestamps (for correlation) ---
    /// Wall-clock when sample was captured
    pub wall_clock_captured: WallClockInstant,
    /// Exchange timestamp from message (Binance's `E` field, milliseconds)
    pub exchange_ts_ms: i64,

    // --- Monotonic timestamps (for latency calculation) ---
    /// Monotonic timestamp when frame arrived at recv() (nanoseconds)
    pub mono_recv_ns: u64,
    /// Monotonic timestamp after JSON/SBE decode complete (nanoseconds)
    pub mono_decoded_ns: u64,
    /// Monotonic timestamp when handed off to strategy channel (nanoseconds)
    pub mono_handoff_ns: u64,
    /// Monotonic timestamp when strategy callback started (nanoseconds)
    pub mono_strategy_ns: Option<u64>,

    // --- Derived latencies (nanoseconds) ---
    /// Estimated one-way wire latency: wall_clock_captured - exchange_ts
    /// WARNING: Requires NTP sync; may be negative if clocks are skewed
    pub wire_latency_ns: i64,
    /// Decode latency: mono_decoded - mono_recv
    pub decode_latency_ns: u64,
    /// Handoff latency: mono_handoff - mono_decoded
    pub handoff_latency_ns: u64,
    /// Strategy receive latency: mono_strategy - mono_handoff
    pub strategy_latency_ns: Option<u64>,
    /// Total internal latency: mono_strategy - mono_recv (or mono_handoff if no strategy)
    pub total_internal_latency_ns: u64,

    // --- Message metadata ---
    /// Message size in bytes (wire format)
    pub message_size_bytes: usize,
    /// Message sequence number (if available)
    pub sequence: Option<u64>,
    /// Best bid price (for orderbook messages)
    pub best_bid: Option<f64>,
    /// Best ask price (for orderbook messages)
    pub best_ask: Option<f64>,
}

// =============================================================================
// RTT SAMPLE (for one-way estimation)
// =============================================================================

/// WebSocket ping/pong round-trip time sample.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RttSample {
    /// Wall-clock when ping was sent
    pub wall_clock_sent: WallClockInstant,
    /// Monotonic timestamp when ping was sent (nanoseconds)
    pub mono_sent_ns: u64,
    /// Monotonic timestamp when pong was received (nanoseconds)
    pub mono_recv_ns: u64,
    /// Round-trip time (nanoseconds)
    pub rtt_ns: u64,
    /// Estimated one-way latency (rtt / 2)
    pub estimated_one_way_ns: u64,
}

// =============================================================================
// JITTER CALCULATION
// =============================================================================

/// Jitter statistics for a latency series.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct JitterStats {
    /// Number of samples
    pub count: u64,
    /// Mean inter-arrival jitter (RFC 3550 exponential moving average)
    pub mean_jitter_ns: f64,
    /// Max absolute jitter observed
    pub max_jitter_ns: u64,
    /// Standard deviation of latencies
    pub stddev_ns: f64,
}

impl JitterStats {
    /// Update jitter with a new latency sample (RFC 3550 algorithm)
    pub fn update(&mut self, latency_ns: u64, prev_latency_ns: Option<u64>) {
        self.count += 1;

        if let Some(prev) = prev_latency_ns {
            let diff = (latency_ns as i64 - prev as i64).unsigned_abs();
            // RFC 3550: J(i) = J(i-1) + (|D(i-1,i)| - J(i-1))/16
            self.mean_jitter_ns += (diff as f64 - self.mean_jitter_ns) / 16.0;
            self.max_jitter_ns = self.max_jitter_ns.max(diff);
        }
    }
}

// =============================================================================
// HISTOGRAM (HDR-like)
// =============================================================================

/// Lock-free histogram for latency percentiles.
/// Uses logarithmic buckets from 1μs to 10s.
#[derive(Debug)]
pub struct LatencyHistogram {
    /// Bucket counts (atomic for lock-free updates)
    buckets: Vec<AtomicU64>,
    /// Bucket upper bounds (nanoseconds)
    bucket_bounds_ns: Vec<u64>,
    /// Total sample count
    count: AtomicU64,
    /// Sum of all samples (for mean calculation)
    sum_ns: AtomicU64,
    /// Minimum observed value
    min_ns: AtomicU64,
    /// Maximum observed value
    max_ns: AtomicU64,
}

impl Default for LatencyHistogram {
    fn default() -> Self {
        Self::new()
    }
}

impl LatencyHistogram {
    /// Create a new histogram with logarithmic buckets
    pub fn new() -> Self {
        // Logarithmic buckets: 1μs, 2μs, 5μs, 10μs, ..., 10s
        let bucket_bounds_ns: Vec<u64> = vec![
            1_000,       // 1 μs
            2_000,       // 2 μs
            5_000,       // 5 μs
            10_000,      // 10 μs
            20_000,      // 20 μs
            50_000,      // 50 μs
            100_000,     // 100 μs
            200_000,     // 200 μs
            500_000,     // 500 μs
            1_000_000,   // 1 ms
            2_000_000,   // 2 ms
            5_000_000,   // 5 ms
            10_000_000,  // 10 ms
            20_000_000,  // 20 ms
            50_000_000,  // 50 ms
            100_000_000, // 100 ms
            200_000_000, // 200 ms
            500_000_000, // 500 ms
            1_000_000_000,  // 1 s
            2_000_000_000,  // 2 s
            5_000_000_000,  // 5 s
            10_000_000_000, // 10 s
            u64::MAX,
        ];

        let buckets = (0..bucket_bounds_ns.len())
            .map(|_| AtomicU64::new(0))
            .collect();

        Self {
            buckets,
            bucket_bounds_ns,
            count: AtomicU64::new(0),
            sum_ns: AtomicU64::new(0),
            min_ns: AtomicU64::new(u64::MAX),
            max_ns: AtomicU64::new(0),
        }
    }

    /// Record a latency sample (nanoseconds)
    #[inline]
    pub fn record_ns(&self, latency_ns: u64) {
        self.count.fetch_add(1, Ordering::Relaxed);
        self.sum_ns.fetch_add(latency_ns, Ordering::Relaxed);

        // Update min (CAS loop)
        let mut current_min = self.min_ns.load(Ordering::Relaxed);
        while latency_ns < current_min {
            match self.min_ns.compare_exchange_weak(
                current_min,
                latency_ns,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(x) => current_min = x,
            }
        }

        // Update max (CAS loop)
        let mut current_max = self.max_ns.load(Ordering::Relaxed);
        while latency_ns > current_max {
            match self.max_ns.compare_exchange_weak(
                current_max,
                latency_ns,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(x) => current_max = x,
            }
        }

        // Find bucket
        for (i, &bound) in self.bucket_bounds_ns.iter().enumerate() {
            if latency_ns <= bound {
                self.buckets[i].fetch_add(1, Ordering::Relaxed);
                break;
            }
        }
    }

    /// Record a latency sample (microseconds)
    #[inline]
    pub fn record_us(&self, latency_us: u64) {
        self.record_ns(latency_us * NS_PER_US);
    }

    /// Get percentile value (nanoseconds)
    pub fn percentile_ns(&self, p: f64) -> u64 {
        let count = self.count.load(Ordering::Relaxed);
        if count == 0 {
            return 0;
        }

        let target = ((p / 100.0) * count as f64).ceil() as u64;
        let mut cumulative = 0u64;

        for (i, bucket) in self.buckets.iter().enumerate() {
            cumulative += bucket.load(Ordering::Relaxed);
            if cumulative >= target {
                return self.bucket_bounds_ns[i];
            }
        }

        self.max_ns.load(Ordering::Relaxed)
    }

    /// Get statistics snapshot
    pub fn stats(&self) -> HistogramStats {
        let count = self.count.load(Ordering::Relaxed);
        let sum = self.sum_ns.load(Ordering::Relaxed);
        let min = self.min_ns.load(Ordering::Relaxed);
        let max = self.max_ns.load(Ordering::Relaxed);

        HistogramStats {
            count,
            min_ns: if count > 0 { min } else { 0 },
            max_ns: max,
            mean_ns: if count > 0 {
                sum as f64 / count as f64
            } else {
                0.0
            },
            p50_ns: self.percentile_ns(50.0),
            p95_ns: self.percentile_ns(95.0),
            p99_ns: self.percentile_ns(99.0),
            p999_ns: self.percentile_ns(99.9),
        }
    }

    /// Reset all counters
    pub fn reset(&self) {
        self.count.store(0, Ordering::Relaxed);
        self.sum_ns.store(0, Ordering::Relaxed);
        self.min_ns.store(u64::MAX, Ordering::Relaxed);
        self.max_ns.store(0, Ordering::Relaxed);
        for bucket in &self.buckets {
            bucket.store(0, Ordering::Relaxed);
        }
    }
}

/// Histogram statistics snapshot
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistogramStats {
    pub count: u64,
    pub min_ns: u64,
    pub max_ns: u64,
    pub mean_ns: f64,
    pub p50_ns: u64,
    pub p95_ns: u64,
    pub p99_ns: u64,
    pub p999_ns: u64,
}

impl HistogramStats {
    /// Format as microseconds for display
    pub fn to_us_string(&self) -> String {
        format!(
            "n={} min={:.1}μs max={:.1}μs mean={:.1}μs p50={:.1}μs p95={:.1}μs p99={:.1}μs p99.9={:.1}μs",
            self.count,
            self.min_ns as f64 / 1000.0,
            self.max_ns as f64 / 1000.0,
            self.mean_ns / 1000.0,
            self.p50_ns as f64 / 1000.0,
            self.p95_ns as f64 / 1000.0,
            self.p99_ns as f64 / 1000.0,
            self.p999_ns as f64 / 1000.0,
        )
    }
}

// =============================================================================
// HARNESS STATE
// =============================================================================

/// Complete latency measurement harness for Binance market data.
#[derive(Debug)]
pub struct BinanceLatencyHarness {
    /// Configuration
    config: HarnessConfig,

    // --- Connection histograms ---
    pub dns_latency: LatencyHistogram,
    pub tcp_connect_latency: LatencyHistogram,
    pub tls_handshake_latency: LatencyHistogram,
    pub ws_upgrade_latency: LatencyHistogram,
    pub subscribe_latency: LatencyHistogram,
    pub total_connect_latency: LatencyHistogram,

    // --- Per-message histograms ---
    pub wire_latency: LatencyHistogram,
    pub decode_latency: LatencyHistogram,
    pub handoff_latency: LatencyHistogram,
    pub strategy_latency: LatencyHistogram,
    pub total_internal_latency: LatencyHistogram,

    // --- RTT ---
    pub rtt_latency: LatencyHistogram,

    // --- Per-symbol histograms ---
    pub per_symbol_latency: RwLock<HashMap<String, LatencyHistogram>>,

    // --- Jitter tracking ---
    pub wire_jitter: Mutex<JitterStats>,
    pub decode_jitter: Mutex<JitterStats>,

    // --- Sample storage (for CSV export) ---
    connection_samples: Mutex<Vec<ConnectionLatencySample>>,
    message_samples: Mutex<Vec<MessageLatencySample>>,
    rtt_samples: Mutex<Vec<RttSample>>,

    // --- Counters ---
    pub messages_received: AtomicU64,
    pub connections_attempted: AtomicU64,
    pub connections_succeeded: AtomicU64,
    pub connections_failed: AtomicU64,
    pub reconnections: AtomicU64,

    // --- Last values (for jitter calculation) ---
    last_wire_latency_ns: AtomicU64,
    last_decode_latency_ns: AtomicU64,

    // --- Control ---
    enabled: AtomicBool,
    sample_rate: AtomicU64,
}

/// Harness configuration
#[derive(Debug, Clone)]
pub struct HarnessConfig {
    /// Maximum number of connection samples to retain
    pub max_connection_samples: usize,
    /// Maximum number of message samples to retain
    pub max_message_samples: usize,
    /// Maximum number of RTT samples to retain
    pub max_rtt_samples: usize,
    /// Sample rate (1 = every message, 10 = every 10th, etc.)
    pub sample_rate: u64,
    /// RTT ping interval
    pub rtt_ping_interval: Duration,
    /// Enable detailed per-message sampling
    pub detailed_sampling: bool,
}

impl Default for HarnessConfig {
    fn default() -> Self {
        Self {
            max_connection_samples: 1000,
            max_message_samples: 100_000,
            max_rtt_samples: 10_000,
            sample_rate: 1, // Sample every message
            rtt_ping_interval: Duration::from_secs(5),
            detailed_sampling: true,
        }
    }
}

impl BinanceLatencyHarness {
    /// Create a new harness with default configuration
    pub fn new() -> Self {
        Self::with_config(HarnessConfig::default())
    }

    /// Create a new harness with custom configuration
    pub fn with_config(config: HarnessConfig) -> Self {
        let sample_rate = config.sample_rate;
        Self {
            config,
            dns_latency: LatencyHistogram::new(),
            tcp_connect_latency: LatencyHistogram::new(),
            tls_handshake_latency: LatencyHistogram::new(),
            ws_upgrade_latency: LatencyHistogram::new(),
            subscribe_latency: LatencyHistogram::new(),
            total_connect_latency: LatencyHistogram::new(),
            wire_latency: LatencyHistogram::new(),
            decode_latency: LatencyHistogram::new(),
            handoff_latency: LatencyHistogram::new(),
            strategy_latency: LatencyHistogram::new(),
            total_internal_latency: LatencyHistogram::new(),
            rtt_latency: LatencyHistogram::new(),
            per_symbol_latency: RwLock::new(HashMap::new()),
            wire_jitter: Mutex::new(JitterStats::default()),
            decode_jitter: Mutex::new(JitterStats::default()),
            connection_samples: Mutex::new(Vec::new()),
            message_samples: Mutex::new(Vec::new()),
            rtt_samples: Mutex::new(Vec::new()),
            messages_received: AtomicU64::new(0),
            connections_attempted: AtomicU64::new(0),
            connections_succeeded: AtomicU64::new(0),
            connections_failed: AtomicU64::new(0),
            reconnections: AtomicU64::new(0),
            last_wire_latency_ns: AtomicU64::new(0),
            last_decode_latency_ns: AtomicU64::new(0),
            enabled: AtomicBool::new(true),
            sample_rate: AtomicU64::new(sample_rate),
        }
    }

    /// Enable/disable measurement
    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Relaxed);
    }

    /// Check if sampling should occur for this message
    #[inline]
    fn should_sample(&self) -> bool {
        if !self.enabled.load(Ordering::Relaxed) {
            return false;
        }
        let count = self.messages_received.fetch_add(1, Ordering::Relaxed);
        let rate = self.sample_rate.load(Ordering::Relaxed);
        count % rate == 0
    }

    /// Record a connection latency sample
    pub fn record_connection(&self, sample: ConnectionLatencySample) {
        self.connections_attempted.fetch_add(1, Ordering::Relaxed);

        if sample.success {
            self.connections_succeeded.fetch_add(1, Ordering::Relaxed);

            if let Some(dns) = sample.dns_latency_ns {
                self.dns_latency.record_ns(dns);
            }
            self.tcp_connect_latency.record_ns(sample.tcp_connect_latency_ns);
            self.tls_handshake_latency.record_ns(sample.tls_handshake_latency_ns);
            self.ws_upgrade_latency.record_ns(sample.ws_upgrade_latency_ns);
            if let Some(sub) = sample.subscribe_latency_ns {
                self.subscribe_latency.record_ns(sub);
            }
            self.total_connect_latency.record_ns(sample.total_connect_latency_ns);
        } else {
            self.connections_failed.fetch_add(1, Ordering::Relaxed);
        }

        // Store sample
        let mut samples = self.connection_samples.lock();
        if samples.len() >= self.config.max_connection_samples {
            samples.remove(0);
        }
        samples.push(sample);
    }

    /// Record a message latency sample
    pub fn record_message(&self, sample: MessageLatencySample) {
        if !self.should_sample() {
            return;
        }

        // Update histograms
        if sample.wire_latency_ns > 0 {
            self.wire_latency.record_ns(sample.wire_latency_ns as u64);

            // Jitter tracking
            let prev = self.last_wire_latency_ns.swap(sample.wire_latency_ns as u64, Ordering::Relaxed);
            if prev > 0 {
                self.wire_jitter.lock().update(sample.wire_latency_ns as u64, Some(prev));
            }
        }

        self.decode_latency.record_ns(sample.decode_latency_ns);
        self.handoff_latency.record_ns(sample.handoff_latency_ns);

        if let Some(strat) = sample.strategy_latency_ns {
            self.strategy_latency.record_ns(strat);
        }

        self.total_internal_latency.record_ns(sample.total_internal_latency_ns);

        // Per-symbol tracking
        {
            let mut per_sym = self.per_symbol_latency.write();
            let hist = per_sym
                .entry(sample.symbol.clone())
                .or_insert_with(LatencyHistogram::new);
            hist.record_ns(sample.total_internal_latency_ns);
        }

        // Decode jitter
        let prev_decode = self.last_decode_latency_ns.swap(sample.decode_latency_ns, Ordering::Relaxed);
        if prev_decode > 0 {
            self.decode_jitter.lock().update(sample.decode_latency_ns, Some(prev_decode));
        }

        // Store sample if detailed sampling enabled
        if self.config.detailed_sampling {
            let mut samples = self.message_samples.lock();
            if samples.len() >= self.config.max_message_samples {
                samples.remove(0);
            }
            samples.push(sample);
        }
    }

    /// Record an RTT sample
    pub fn record_rtt(&self, sample: RttSample) {
        self.rtt_latency.record_ns(sample.rtt_ns);

        let mut samples = self.rtt_samples.lock();
        if samples.len() >= self.config.max_rtt_samples {
            samples.remove(0);
        }
        samples.push(sample);
    }

    /// Get summary statistics
    pub fn summary(&self) -> HarnessSummary {
        HarnessSummary {
            timestamp: WallClockInstant::now(),
            messages_received: self.messages_received.load(Ordering::Relaxed),
            connections_attempted: self.connections_attempted.load(Ordering::Relaxed),
            connections_succeeded: self.connections_succeeded.load(Ordering::Relaxed),
            connections_failed: self.connections_failed.load(Ordering::Relaxed),
            dns_latency: self.dns_latency.stats(),
            tcp_connect_latency: self.tcp_connect_latency.stats(),
            tls_handshake_latency: self.tls_handshake_latency.stats(),
            ws_upgrade_latency: self.ws_upgrade_latency.stats(),
            subscribe_latency: self.subscribe_latency.stats(),
            total_connect_latency: self.total_connect_latency.stats(),
            wire_latency: self.wire_latency.stats(),
            decode_latency: self.decode_latency.stats(),
            handoff_latency: self.handoff_latency.stats(),
            strategy_latency: self.strategy_latency.stats(),
            total_internal_latency: self.total_internal_latency.stats(),
            rtt_latency: self.rtt_latency.stats(),
            wire_jitter: self.wire_jitter.lock().clone(),
            decode_jitter: self.decode_jitter.lock().clone(),
        }
    }

    /// Export message samples to CSV
    pub fn export_message_csv<W: Write>(&self, writer: &mut W) -> std::io::Result<()> {
        // CSV Header
        writeln!(
            writer,
            "sample_id,symbol,wall_clock_iso,exchange_ts_ms,\
             mono_recv_ns,mono_decoded_ns,mono_handoff_ns,mono_strategy_ns,\
             wire_latency_ns,decode_latency_ns,handoff_latency_ns,strategy_latency_ns,\
             total_internal_latency_ns,message_size_bytes,sequence,best_bid,best_ask"
        )?;

        let samples = self.message_samples.lock();
        for s in samples.iter() {
            writeln!(
                writer,
                "{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{}",
                s.sample_id,
                s.symbol,
                s.wall_clock_captured.to_iso8601(),
                s.exchange_ts_ms,
                s.mono_recv_ns,
                s.mono_decoded_ns,
                s.mono_handoff_ns,
                s.mono_strategy_ns.map(|v| v.to_string()).unwrap_or_default(),
                s.wire_latency_ns,
                s.decode_latency_ns,
                s.handoff_latency_ns,
                s.strategy_latency_ns.map(|v| v.to_string()).unwrap_or_default(),
                s.total_internal_latency_ns,
                s.message_size_bytes,
                s.sequence.map(|v| v.to_string()).unwrap_or_default(),
                s.best_bid.map(|v| format!("{:.8}", v)).unwrap_or_default(),
                s.best_ask.map(|v| format!("{:.8}", v)).unwrap_or_default(),
            )?;
        }

        Ok(())
    }

    /// Export connection samples to CSV
    pub fn export_connection_csv<W: Write>(&self, writer: &mut W) -> std::io::Result<()> {
        writeln!(
            writer,
            "wall_clock_iso,dns_latency_ns,tcp_connect_latency_ns,tls_handshake_latency_ns,\
             ws_upgrade_latency_ns,subscribe_latency_ns,total_connect_latency_ns,\
             success,remote_addr,tls_version,tls_cipher,error"
        )?;

        let samples = self.connection_samples.lock();
        for s in samples.iter() {
            writeln!(
                writer,
                "{},{},{},{},{},{},{},{},{},{},{},\"{}\"",
                s.wall_clock_start.to_iso8601(),
                s.dns_latency_ns.map(|v| v.to_string()).unwrap_or_default(),
                s.tcp_connect_latency_ns,
                s.tls_handshake_latency_ns,
                s.ws_upgrade_latency_ns,
                s.subscribe_latency_ns.map(|v| v.to_string()).unwrap_or_default(),
                s.total_connect_latency_ns,
                s.success,
                s.remote_addr,
                s.tls_version,
                s.tls_cipher,
                s.error.as_deref().unwrap_or(""),
            )?;
        }

        Ok(())
    }

    /// Reset all metrics
    pub fn reset(&self) {
        self.dns_latency.reset();
        self.tcp_connect_latency.reset();
        self.tls_handshake_latency.reset();
        self.ws_upgrade_latency.reset();
        self.subscribe_latency.reset();
        self.total_connect_latency.reset();
        self.wire_latency.reset();
        self.decode_latency.reset();
        self.handoff_latency.reset();
        self.strategy_latency.reset();
        self.total_internal_latency.reset();
        self.rtt_latency.reset();

        self.per_symbol_latency.write().clear();

        *self.wire_jitter.lock() = JitterStats::default();
        *self.decode_jitter.lock() = JitterStats::default();

        self.connection_samples.lock().clear();
        self.message_samples.lock().clear();
        self.rtt_samples.lock().clear();

        self.messages_received.store(0, Ordering::Relaxed);
        self.connections_attempted.store(0, Ordering::Relaxed);
        self.connections_succeeded.store(0, Ordering::Relaxed);
        self.connections_failed.store(0, Ordering::Relaxed);
        self.reconnections.store(0, Ordering::Relaxed);
    }
}

impl Default for BinanceLatencyHarness {
    fn default() -> Self {
        Self::new()
    }
}

/// Summary statistics snapshot
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HarnessSummary {
    pub timestamp: WallClockInstant,
    pub messages_received: u64,
    pub connections_attempted: u64,
    pub connections_succeeded: u64,
    pub connections_failed: u64,
    pub dns_latency: HistogramStats,
    pub tcp_connect_latency: HistogramStats,
    pub tls_handshake_latency: HistogramStats,
    pub ws_upgrade_latency: HistogramStats,
    pub subscribe_latency: HistogramStats,
    pub total_connect_latency: HistogramStats,
    pub wire_latency: HistogramStats,
    pub decode_latency: HistogramStats,
    pub handoff_latency: HistogramStats,
    pub strategy_latency: HistogramStats,
    pub total_internal_latency: HistogramStats,
    pub rtt_latency: HistogramStats,
    pub wire_jitter: JitterStats,
    pub decode_jitter: JitterStats,
}

// =============================================================================
// GLOBAL SINGLETON
// =============================================================================

/// Global harness instance
static GLOBAL_HARNESS: std::sync::OnceLock<Arc<BinanceLatencyHarness>> = std::sync::OnceLock::new();

/// Get or initialize the global harness
pub fn global_harness() -> &'static Arc<BinanceLatencyHarness> {
    GLOBAL_HARNESS.get_or_init(|| Arc::new(BinanceLatencyHarness::new()))
}

// =============================================================================
// INSTRUMENTATION HELPERS
// =============================================================================

/// Timestamp marker for instrumentation points
#[derive(Debug, Clone, Copy)]
pub struct InstrumentationPoint {
    pub mono: MonotonicInstant,
    pub wall: WallClockInstant,
}

impl InstrumentationPoint {
    #[inline]
    pub fn now() -> Self {
        Self {
            mono: MonotonicInstant::now(),
            wall: WallClockInstant::now(),
        }
    }
}

/// Builder for creating a MessageLatencySample with instrumentation
#[derive(Debug)]
pub struct MessageLatencyBuilder {
    sample_id: u64,
    symbol: String,
    recv_point: InstrumentationPoint,
    decoded_mono_ns: Option<u64>,
    handoff_mono_ns: Option<u64>,
    strategy_mono_ns: Option<u64>,
    exchange_ts_ms: i64,
    message_size_bytes: usize,
    sequence: Option<u64>,
    best_bid: Option<f64>,
    best_ask: Option<f64>,
}

impl MessageLatencyBuilder {
    /// Start building a sample at recv() time
    pub fn start(symbol: impl Into<String>, exchange_ts_ms: i64, message_size_bytes: usize) -> Self {
        static SAMPLE_ID: AtomicU64 = AtomicU64::new(0);
        Self {
            sample_id: SAMPLE_ID.fetch_add(1, Ordering::Relaxed),
            symbol: symbol.into(),
            recv_point: InstrumentationPoint::now(),
            decoded_mono_ns: None,
            handoff_mono_ns: None,
            strategy_mono_ns: None,
            exchange_ts_ms,
            message_size_bytes,
            sequence: None,
            best_bid: None,
            best_ask: None,
        }
    }

    /// Mark decode complete
    pub fn mark_decoded(&mut self) {
        self.decoded_mono_ns = Some(MonotonicInstant::now().as_nanos());
    }

    /// Mark handoff to channel complete
    pub fn mark_handoff(&mut self) {
        self.handoff_mono_ns = Some(MonotonicInstant::now().as_nanos());
    }

    /// Mark strategy callback entry
    pub fn mark_strategy(&mut self) {
        self.strategy_mono_ns = Some(MonotonicInstant::now().as_nanos());
    }

    /// Set sequence number
    pub fn sequence(mut self, seq: u64) -> Self {
        self.sequence = Some(seq);
        self
    }

    /// Set best bid/ask
    pub fn book(mut self, bid: f64, ask: f64) -> Self {
        self.best_bid = Some(bid);
        self.best_ask = Some(ask);
        self
    }

    /// Build the final sample
    pub fn build(self) -> MessageLatencySample {
        let mono_recv_ns = self.recv_point.mono.as_nanos();
        let mono_decoded_ns = self.decoded_mono_ns.unwrap_or(mono_recv_ns);
        let mono_handoff_ns = self.handoff_mono_ns.unwrap_or(mono_decoded_ns);

        // Estimated one-way wire latency (wall-clock based, requires NTP sync)
        let wall_recv_ms = self.recv_point.wall.as_unix_millis();
        let wire_latency_ns = ((wall_recv_ms - self.exchange_ts_ms) * 1_000_000).max(0);

        let decode_latency_ns = mono_decoded_ns.saturating_sub(mono_recv_ns);
        let handoff_latency_ns = mono_handoff_ns.saturating_sub(mono_decoded_ns);
        let strategy_latency_ns = self.strategy_mono_ns.map(|s| s.saturating_sub(mono_handoff_ns));

        let total_internal_latency_ns = self
            .strategy_mono_ns
            .unwrap_or(mono_handoff_ns)
            .saturating_sub(mono_recv_ns);

        MessageLatencySample {
            sample_id: self.sample_id,
            symbol: self.symbol,
            wall_clock_captured: self.recv_point.wall,
            exchange_ts_ms: self.exchange_ts_ms,
            mono_recv_ns,
            mono_decoded_ns,
            mono_handoff_ns,
            mono_strategy_ns: self.strategy_mono_ns,
            wire_latency_ns,
            decode_latency_ns,
            handoff_latency_ns,
            strategy_latency_ns,
            total_internal_latency_ns,
            message_size_bytes: self.message_size_bytes,
            sequence: self.sequence,
            best_bid: self.best_bid,
            best_ask: self.best_ask,
        }
    }
}

// =============================================================================
// CSV SCHEMA DOCUMENTATION
// =============================================================================

/// CSV Schema for message latency samples:
///
/// | Column                    | Type    | Description                                           |
/// |---------------------------|---------|-------------------------------------------------------|
/// | sample_id                 | u64     | Unique monotonically increasing sample ID             |
/// | symbol                    | string  | Trading pair (e.g., "BTCUSDT")                        |
/// | wall_clock_iso            | string  | ISO 8601 timestamp when recv() returned               |
/// | exchange_ts_ms            | i64     | Binance exchange timestamp (milliseconds)             |
/// | mono_recv_ns              | u64     | Monotonic ns when frame arrived at recv()             |
/// | mono_decoded_ns           | u64     | Monotonic ns when JSON decode completed               |
/// | mono_handoff_ns           | u64     | Monotonic ns when sent to strategy channel            |
/// | mono_strategy_ns          | u64?    | Monotonic ns when strategy callback started           |
/// | wire_latency_ns           | i64     | Estimated one-way wire latency (requires NTP)         |
/// | decode_latency_ns         | u64     | JSON/SBE decode time (monotonic)                      |
/// | handoff_latency_ns        | u64     | Channel send time (monotonic)                         |
/// | strategy_latency_ns       | u64?    | Strategy queue wait time (monotonic)                  |
/// | total_internal_latency_ns | u64     | Total internal processing (recv → strategy)           |
/// | message_size_bytes        | usize   | Raw message size in bytes                             |
/// | sequence                  | u64?    | Message sequence number if available                  |
/// | best_bid                  | f64?    | Best bid price (for orderbook messages)               |
/// | best_ask                  | f64?    | Best ask price (for orderbook messages)               |
pub const MESSAGE_CSV_SCHEMA: &str = "message_latency_samples";

/// CSV Schema for connection latency samples:
///
/// | Column                    | Type    | Description                                           |
/// |---------------------------|---------|-------------------------------------------------------|
/// | wall_clock_iso            | string  | ISO 8601 timestamp when connection started            |
/// | dns_latency_ns            | u64?    | DNS resolution time (monotonic)                       |
/// | tcp_connect_latency_ns    | u64     | TCP 3-way handshake time (monotonic)                  |
/// | tls_handshake_latency_ns  | u64     | TLS handshake time (monotonic)                        |
/// | ws_upgrade_latency_ns     | u64     | WebSocket HTTP upgrade time (monotonic)               |
/// | subscribe_latency_ns      | u64?    | Subscription ACK time (monotonic)                     |
/// | total_connect_latency_ns  | u64     | Total connection establishment time                   |
/// | success                   | bool    | Whether connection succeeded                          |
/// | remote_addr               | string  | Connected remote address (IP:port)                    |
/// | tls_version               | string  | Negotiated TLS version                                |
/// | tls_cipher                | string  | Negotiated cipher suite                               |
/// | error                     | string? | Error message if failed                               |
pub const CONNECTION_CSV_SCHEMA: &str = "connection_latency_samples";

// =============================================================================
// DASHBOARD SPEC
// =============================================================================

/// Minimal dashboard specification for Grafana/similar
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DashboardSpec {
    pub title: String,
    pub panels: Vec<DashboardPanel>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DashboardPanel {
    pub title: String,
    pub panel_type: String,
    pub metrics: Vec<String>,
    pub unit: String,
}

impl DashboardSpec {
    /// Generate minimal dashboard spec for Binance latency monitoring
    pub fn binance_latency() -> Self {
        Self {
            title: "Binance Market Data Latency (eu-west-1)".to_string(),
            panels: vec![
                DashboardPanel {
                    title: "Connection Establishment".to_string(),
                    panel_type: "timeseries".to_string(),
                    metrics: vec![
                        "binance_dns_latency_p99_us".to_string(),
                        "binance_tcp_connect_p99_us".to_string(),
                        "binance_tls_handshake_p99_us".to_string(),
                        "binance_ws_upgrade_p99_us".to_string(),
                    ],
                    unit: "μs".to_string(),
                },
                DashboardPanel {
                    title: "Per-Message Latency (Internal)".to_string(),
                    panel_type: "timeseries".to_string(),
                    metrics: vec![
                        "binance_decode_latency_p50_us".to_string(),
                        "binance_decode_latency_p99_us".to_string(),
                        "binance_handoff_latency_p50_us".to_string(),
                        "binance_handoff_latency_p99_us".to_string(),
                        "binance_strategy_latency_p50_us".to_string(),
                        "binance_strategy_latency_p99_us".to_string(),
                    ],
                    unit: "μs".to_string(),
                },
                DashboardPanel {
                    title: "Wire Latency (One-Way Estimate)".to_string(),
                    panel_type: "timeseries".to_string(),
                    metrics: vec![
                        "binance_wire_latency_p50_ms".to_string(),
                        "binance_wire_latency_p95_ms".to_string(),
                        "binance_wire_latency_p99_ms".to_string(),
                    ],
                    unit: "ms".to_string(),
                },
                DashboardPanel {
                    title: "RTT (Ping/Pong)".to_string(),
                    panel_type: "timeseries".to_string(),
                    metrics: vec![
                        "binance_rtt_p50_ms".to_string(),
                        "binance_rtt_p99_ms".to_string(),
                    ],
                    unit: "ms".to_string(),
                },
                DashboardPanel {
                    title: "Jitter".to_string(),
                    panel_type: "timeseries".to_string(),
                    metrics: vec![
                        "binance_wire_jitter_mean_us".to_string(),
                        "binance_wire_jitter_max_us".to_string(),
                        "binance_decode_jitter_mean_us".to_string(),
                    ],
                    unit: "μs".to_string(),
                },
                DashboardPanel {
                    title: "Percentile Heatmap (Total Internal)".to_string(),
                    panel_type: "heatmap".to_string(),
                    metrics: vec!["binance_total_internal_latency_bucket".to_string()],
                    unit: "μs".to_string(),
                },
                DashboardPanel {
                    title: "Per-Symbol Latency".to_string(),
                    panel_type: "bargauge".to_string(),
                    metrics: vec![
                        "binance_symbol_latency_p99_us{symbol=\"BTCUSDT\"}".to_string(),
                        "binance_symbol_latency_p99_us{symbol=\"ETHUSDT\"}".to_string(),
                        "binance_symbol_latency_p99_us{symbol=\"SOLUSDT\"}".to_string(),
                        "binance_symbol_latency_p99_us{symbol=\"XRPUSDT\"}".to_string(),
                    ],
                    unit: "μs".to_string(),
                },
                DashboardPanel {
                    title: "Connection Health".to_string(),
                    panel_type: "stat".to_string(),
                    metrics: vec![
                        "binance_connections_succeeded".to_string(),
                        "binance_connections_failed".to_string(),
                        "binance_reconnections".to_string(),
                        "binance_messages_received".to_string(),
                    ],
                    unit: "count".to_string(),
                },
            ],
        }
    }
}

// =============================================================================
// TESTS
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_monotonic_instant() {
        let t1 = MonotonicInstant::now();
        std::thread::sleep(Duration::from_micros(100));
        let t2 = MonotonicInstant::now();

        assert!(t2.as_nanos() > t1.as_nanos());
        let elapsed = t2.duration_since(t1);
        assert!(elapsed.as_micros() >= 100);
    }

    #[test]
    fn test_histogram_percentiles() {
        let hist = LatencyHistogram::new();

        // Record 1000 samples: 1μs to 1000μs
        for i in 1..=1000 {
            hist.record_us(i);
        }

        let stats = hist.stats();
        assert_eq!(stats.count, 1000);

        // p50 should be around 500μs
        let p50_us = stats.p50_ns / 1000;
        assert!(p50_us >= 400 && p50_us <= 600, "p50 = {}μs", p50_us);

        // p99 should be around 990μs
        let p99_us = stats.p99_ns / 1000;
        assert!(p99_us >= 900 && p99_us <= 1100, "p99 = {}μs", p99_us);
    }

    #[test]
    fn test_message_latency_builder() {
        let mut builder = MessageLatencyBuilder::start("BTCUSDT", 1700000000000, 256);

        std::thread::sleep(Duration::from_micros(10));
        builder.mark_decoded();

        std::thread::sleep(Duration::from_micros(5));
        builder.mark_handoff();

        let sample = builder.book(50000.0, 50001.0).build();

        assert_eq!(sample.symbol, "BTCUSDT");
        assert!(sample.decode_latency_ns > 0);
        assert!(sample.handoff_latency_ns > 0);
        assert_eq!(sample.best_bid, Some(50000.0));
    }

    #[test]
    fn test_harness_recording() {
        let harness = BinanceLatencyHarness::new();

        let sample = MessageLatencySample {
            sample_id: 1,
            symbol: "BTCUSDT".to_string(),
            wall_clock_captured: WallClockInstant::now(),
            exchange_ts_ms: 1700000000000,
            mono_recv_ns: 1000000,
            mono_decoded_ns: 1010000,
            mono_handoff_ns: 1015000,
            mono_strategy_ns: Some(1020000),
            wire_latency_ns: 5000000,
            decode_latency_ns: 10000,
            handoff_latency_ns: 5000,
            strategy_latency_ns: Some(5000),
            total_internal_latency_ns: 20000,
            message_size_bytes: 256,
            sequence: None,
            best_bid: Some(50000.0),
            best_ask: Some(50001.0),
        };

        harness.record_message(sample);

        let summary = harness.summary();
        assert_eq!(summary.messages_received, 1);
        assert!(summary.decode_latency.count > 0);
    }

    #[test]
    fn test_csv_export() {
        let harness = BinanceLatencyHarness::new();

        // Add a sample
        let sample = MessageLatencySample {
            sample_id: 1,
            symbol: "BTCUSDT".to_string(),
            wall_clock_captured: WallClockInstant::now(),
            exchange_ts_ms: 1700000000000,
            mono_recv_ns: 1000000,
            mono_decoded_ns: 1010000,
            mono_handoff_ns: 1015000,
            mono_strategy_ns: None,
            wire_latency_ns: 5000000,
            decode_latency_ns: 10000,
            handoff_latency_ns: 5000,
            strategy_latency_ns: None,
            total_internal_latency_ns: 15000,
            message_size_bytes: 256,
            sequence: Some(12345),
            best_bid: Some(50000.0),
            best_ask: Some(50001.0),
        };

        harness.record_message(sample);

        let mut csv_output = Vec::new();
        harness.export_message_csv(&mut csv_output).unwrap();

        let csv_str = String::from_utf8(csv_output).unwrap();
        assert!(csv_str.contains("sample_id"));
        assert!(csv_str.contains("BTCUSDT"));
        assert!(csv_str.contains("50000.00000000"));
    }
}

//! Performance Measurement Module
//!
//! Comprehensive profiling for the BetterSys trading engine:
//! - Memory usage (heap allocations, peak usage)
//! - CPU time and hot path detection
//! - IO bottlenecks (disk, network)
//! - Throughput (requests/sec, messages/sec)
//! - Latency (response time, tick-to-trade, tail latency p99/p999)
//!
//! Integrates with:
//! - tracing ecosystem for structured logging and flamegraph generation
//! - Custom allocator tracking for memory profiling
//! - Histograms for latency distribution analysis

pub mod allocator;
pub mod benchmark;
pub mod config;
pub mod cpu;
pub mod io;
pub mod latency;
pub mod load_generator;
pub mod memory;
pub mod metrics;
pub mod network;
pub mod queues;
pub mod report;
pub mod throughput;
pub mod tracing_layer;
pub mod tui;
pub mod venue;

pub use allocator::*;
pub use benchmark::*;
pub use config::*;
pub use metrics::*;
pub use network::*;
pub use queues::*;
pub use report::*;
pub use venue::*;

use parking_lot::RwLock;
use std::sync::Arc;
use std::time::Instant;

/// Global performance profiler instance
static PROFILER: std::sync::OnceLock<Arc<PerformanceProfiler>> = std::sync::OnceLock::new();

pub fn global_profiler() -> &'static Arc<PerformanceProfiler> {
    PROFILER.get_or_init(|| Arc::new(PerformanceProfiler::new()))
}

/// Initialize the performance profiler (call early in main)
pub fn init() {
    let _ = global_profiler();
    tracing::info!("Performance profiler initialized");
}

/// Central performance profiler that aggregates all metrics
#[derive(Debug)]
pub struct PerformanceProfiler {
    pub memory: memory::MemoryProfiler,
    pub cpu: cpu::CpuProfiler,
    pub io: io::IoProfiler,
    pub throughput: throughput::ThroughputTracker,
    pub start_time: Instant,

    /// Component-specific profilers for the data ingestion pipeline
    pub pipeline: PipelineProfiler,
}

impl PerformanceProfiler {
    pub fn new() -> Self {
        Self {
            memory: memory::MemoryProfiler::new(),
            cpu: cpu::CpuProfiler::new(),
            io: io::IoProfiler::new(),
            throughput: throughput::ThroughputTracker::new(),
            start_time: Instant::now(),
            pipeline: PipelineProfiler::new(),
        }
    }

    /// Get uptime in seconds
    pub fn uptime_secs(&self) -> f64 {
        self.start_time.elapsed().as_secs_f64()
    }

    /// Generate a full performance report
    pub fn report(&self) -> PerformanceReport {
        PerformanceReport {
            timestamp: chrono::Utc::now().timestamp(),
            uptime_secs: self.uptime_secs(),
            memory: self.memory.snapshot(),
            cpu: self.cpu.snapshot(),
            io: self.io.snapshot(),
            throughput: self.throughput.snapshot(),
            pipeline: self.pipeline.snapshot(),
        }
    }
}

impl Default for PerformanceProfiler {
    fn default() -> Self {
        Self::new()
    }
}

/// Profiler for the data ingestion pipeline specifically
#[derive(Debug)]
pub struct PipelineProfiler {
    /// Binance price feed performance
    pub binance_feed: RwLock<ComponentMetrics>,
    /// Dome WebSocket performance
    pub dome_ws: RwLock<ComponentMetrics>,
    /// Dome REST performance
    pub dome_rest: RwLock<ComponentMetrics>,
    /// Polymarket orderbook cache
    pub polymarket_ws: RwLock<ComponentMetrics>,
    /// Signal detection
    pub signal_detection: RwLock<ComponentMetrics>,
    /// Signal storage (SQLite)
    pub signal_storage: RwLock<ComponentMetrics>,
    /// FAST15M engine
    pub fast15m_engine: RwLock<ComponentMetrics>,
    /// LONG engine (LLM)
    pub long_engine: RwLock<ComponentMetrics>,
}

impl PipelineProfiler {
    pub fn new() -> Self {
        Self {
            binance_feed: RwLock::new(ComponentMetrics::new("binance_feed")),
            dome_ws: RwLock::new(ComponentMetrics::new("dome_ws")),
            dome_rest: RwLock::new(ComponentMetrics::new("dome_rest")),
            polymarket_ws: RwLock::new(ComponentMetrics::new("polymarket_ws")),
            signal_detection: RwLock::new(ComponentMetrics::new("signal_detection")),
            signal_storage: RwLock::new(ComponentMetrics::new("signal_storage")),
            fast15m_engine: RwLock::new(ComponentMetrics::new("fast15m_engine")),
            long_engine: RwLock::new(ComponentMetrics::new("long_engine")),
        }
    }

    pub fn snapshot(&self) -> PipelineSnapshot {
        PipelineSnapshot {
            binance_feed: self.binance_feed.read().clone(),
            dome_ws: self.dome_ws.read().clone(),
            dome_rest: self.dome_rest.read().clone(),
            polymarket_ws: self.polymarket_ws.read().clone(),
            signal_detection: self.signal_detection.read().clone(),
            signal_storage: self.signal_storage.read().clone(),
            fast15m_engine: self.fast15m_engine.read().clone(),
            long_engine: self.long_engine.read().clone(),
        }
    }

    // Convenience methods for recording events

    /// Record a Binance feed event
    pub fn record_binance(&self, latency_us: u64) {
        self.binance_feed.write().record_event(latency_us, 0);
    }

    /// Record a Dome WebSocket event
    pub fn record_dome_ws(&self, latency_us: u64) {
        self.dome_ws.write().record_event(latency_us, 0);
    }

    /// Record a Dome REST event
    pub fn record_dome_rest(&self, latency_us: u64) {
        self.dome_rest.write().record_event(latency_us, 0);
    }

    /// Record a Polymarket WS event
    pub fn record_polymarket(&self, latency_us: u64) {
        self.polymarket_ws.write().record_event(latency_us, 0);
    }

    /// Record signal detection
    pub fn record_signal_detection(&self, latency_us: u64) {
        self.signal_detection.write().record_event(latency_us, 0);
    }

    /// Record signal storage
    pub fn record_signal_storage(&self, latency_us: u64) {
        self.signal_storage.write().record_event(latency_us, 0);
    }

    /// Record FAST15M engine event
    pub fn record_fast15m(&self, latency_us: u64) {
        self.fast15m_engine.write().record_event(latency_us, 0);
    }

    /// Record LONG engine event
    pub fn record_long(&self, latency_us: u64) {
        self.long_engine.write().record_event(latency_us, 0);
    }

    /// Record an error for a component
    pub fn record_error(&self, component: &str) {
        match component {
            "binance_feed" => self.binance_feed.write().record_error(),
            "dome_ws" => self.dome_ws.write().record_error(),
            "dome_rest" => self.dome_rest.write().record_error(),
            "polymarket_ws" => self.polymarket_ws.write().record_error(),
            "signal_detection" => self.signal_detection.write().record_error(),
            "signal_storage" => self.signal_storage.write().record_error(),
            "fast15m_engine" => self.fast15m_engine.write().record_error(),
            "long_engine" => self.long_engine.write().record_error(),
            _ => {}
        }
    }
}

impl Default for PipelineProfiler {
    fn default() -> Self {
        Self::new()
    }
}

/// Metrics for a single pipeline component
#[derive(Debug, Clone, serde::Serialize)]
pub struct ComponentMetrics {
    pub name: String,
    pub events_processed: u64,
    pub errors: u64,
    pub bytes_processed: u64,

    // Latency histogram (microseconds)
    pub latency_count: u64,
    pub latency_sum_us: u64,
    pub latency_min_us: u64,
    pub latency_max_us: u64,
    pub latency_buckets: Vec<u64>, // logarithmic buckets

    // Throughput
    pub last_event_ts: i64,
    pub events_per_sec: f64,

    // Memory estimate for this component
    pub estimated_memory_bytes: u64,
}

impl ComponentMetrics {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            events_processed: 0,
            errors: 0,
            bytes_processed: 0,
            latency_count: 0,
            latency_sum_us: 0,
            latency_min_us: u64::MAX,
            latency_max_us: 0,
            latency_buckets: vec![0u64; 20], // 20 log buckets
            last_event_ts: 0,
            events_per_sec: 0.0,
            estimated_memory_bytes: 0,
        }
    }

    /// Record a successful event with latency
    pub fn record_event(&mut self, latency_us: u64, bytes: u64) {
        self.events_processed += 1;
        self.bytes_processed += bytes;
        self.latency_count += 1;
        self.latency_sum_us = self.latency_sum_us.saturating_add(latency_us);
        self.latency_min_us = self.latency_min_us.min(latency_us);
        self.latency_max_us = self.latency_max_us.max(latency_us);
        self.last_event_ts = chrono::Utc::now().timestamp();

        // Record to histogram bucket
        let bucket = latency_to_bucket(latency_us);
        if bucket < self.latency_buckets.len() {
            self.latency_buckets[bucket] += 1;
        }
    }

    /// Record an error
    pub fn record_error(&mut self) {
        self.errors += 1;
    }

    /// Get mean latency in microseconds
    pub fn mean_latency_us(&self) -> f64 {
        if self.latency_count == 0 {
            0.0
        } else {
            self.latency_sum_us as f64 / self.latency_count as f64
        }
    }

    /// Get percentile latency
    pub fn percentile_us(&self, p: f64) -> u64 {
        if self.latency_count == 0 {
            return 0;
        }
        let target = ((p / 100.0) * self.latency_count as f64).ceil() as u64;
        let mut cumulative = 0u64;
        for (i, &count) in self.latency_buckets.iter().enumerate() {
            cumulative += count;
            if cumulative >= target {
                return bucket_to_latency(i);
            }
        }
        self.latency_max_us
    }

    pub fn p50_us(&self) -> u64 {
        self.percentile_us(50.0)
    }
    pub fn p95_us(&self) -> u64 {
        self.percentile_us(95.0)
    }
    pub fn p99_us(&self) -> u64 {
        self.percentile_us(99.0)
    }
    pub fn p999_us(&self) -> u64 {
        self.percentile_us(99.9)
    }
}

/// Convert latency to histogram bucket index (logarithmic)
fn latency_to_bucket(latency_us: u64) -> usize {
    if latency_us == 0 {
        return 0;
    }
    // Buckets: 1, 2, 5, 10, 20, 50, 100, 200, 500, 1000, 2000, 5000, 10000, ...
    let log = (latency_us as f64).log10();
    ((log * 3.0) as usize).min(19)
}

/// Convert bucket index back to representative latency
fn bucket_to_latency(bucket: usize) -> u64 {
    let bounds: [u64; 20] = [
        1, 2, 5, 10, 20, 50, 100, 200, 500, 1000, 2000, 5000, 10_000, 20_000, 50_000, 100_000,
        200_000, 500_000, 1_000_000, 10_000_000,
    ];
    bounds.get(bucket).copied().unwrap_or(10_000_000)
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct PipelineSnapshot {
    pub binance_feed: ComponentMetrics,
    pub dome_ws: ComponentMetrics,
    pub dome_rest: ComponentMetrics,
    pub polymarket_ws: ComponentMetrics,
    pub signal_detection: ComponentMetrics,
    pub signal_storage: ComponentMetrics,
    pub fast15m_engine: ComponentMetrics,
    pub long_engine: ComponentMetrics,
}

/// Convenience macro for timing a block and recording to a component
#[macro_export]
macro_rules! perf_measure {
    ($component:expr, $bytes:expr, $block:expr) => {{
        let start = std::time::Instant::now();
        let result = $block;
        let latency_us = start.elapsed().as_micros() as u64;
        $component.write().record_event(latency_us, $bytes);
        result
    }};
}

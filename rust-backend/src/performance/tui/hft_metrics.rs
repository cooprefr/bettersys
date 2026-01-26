//! HFT Metrics Collection
//!
//! Lock-free, cache-line aligned metrics for ultra-low latency collection.
//! Designed for:
//! - FPGA timestamp integration
//! - Custom NIC hardware timestamping
//! - Kernel bypass network stacks (DPDK, io_uring)

use crossbeam::atomic::AtomicCell;
use parking_lot::RwLock;
use quanta::Clock;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Cache line size for alignment (64 bytes on most architectures)
const CACHE_LINE_SIZE: usize = 64;

/// High-precision clock for nanosecond timing
#[derive(Clone)]
pub struct HftClock {
    clock: Clock,
    /// TSC frequency for raw cycle counting
    tsc_freq_hz: u64,
}

impl HftClock {
    pub fn new() -> Self {
        let clock = Clock::new();
        Self {
            tsc_freq_hz: 1_000_000_000, // Placeholder; would calibrate from /proc/cpuinfo
            clock,
        }
    }

    /// Get current time in nanoseconds (monotonic)
    #[inline(always)]
    pub fn now_ns(&self) -> u64 {
        self.clock.raw()
    }

    /// Convert raw ticks to nanoseconds
    #[inline(always)]
    pub fn ticks_to_ns(&self, ticks: u64) -> u64 {
        self.clock.delta(0, ticks).as_nanos() as u64
    }

    /// Get raw TSC value (for FPGA synchronization)
    #[inline(always)]
    pub fn raw_tsc(&self) -> u64 {
        self.clock.raw()
    }
}

impl Default for HftClock {
    fn default() -> Self {
        Self::new()
    }
}

/// Cache-line aligned latency sample for lock-free collection
#[repr(C, align(64))]
#[derive(Debug)]
pub struct LatencySample {
    /// Timestamp when event started (ns)
    pub start_ns: AtomicU64,
    /// Timestamp when event completed (ns)
    pub end_ns: AtomicU64,
    /// Event type identifier
    pub event_type: AtomicU64,
    /// Sequence number for ordering
    pub seq: AtomicU64,
    /// Padding to fill cache line
    _pad: [u64; 4],
}

impl LatencySample {
    pub fn new() -> Self {
        Self {
            start_ns: AtomicU64::new(0),
            end_ns: AtomicU64::new(0),
            event_type: AtomicU64::new(0),
            seq: AtomicU64::new(0),
            _pad: [0; 4],
        }
    }

    #[inline(always)]
    pub fn record(&self, start: u64, end: u64, event_type: u64, seq: u64) {
        self.start_ns.store(start, Ordering::Release);
        self.end_ns.store(end, Ordering::Release);
        self.event_type.store(event_type, Ordering::Release);
        self.seq.store(seq, Ordering::Release);
    }

    #[inline(always)]
    pub fn latency_ns(&self) -> u64 {
        let end = self.end_ns.load(Ordering::Acquire);
        let start = self.start_ns.load(Ordering::Acquire);
        end.saturating_sub(start)
    }
}

impl Default for LatencySample {
    fn default() -> Self {
        Self::new()
    }
}

/// Ring buffer for lock-free latency samples
pub struct LatencyRingBuffer {
    samples: Box<[LatencySample]>,
    write_idx: AtomicUsize,
    capacity: usize,
}

impl LatencyRingBuffer {
    pub fn new(capacity: usize) -> Self {
        let mut samples = Vec::with_capacity(capacity);
        for _ in 0..capacity {
            samples.push(LatencySample::new());
        }
        Self {
            samples: samples.into_boxed_slice(),
            write_idx: AtomicUsize::new(0),
            capacity,
        }
    }

    /// Push a new sample (lock-free)
    #[inline(always)]
    pub fn push(&self, start: u64, end: u64, event_type: u64) {
        let idx = self.write_idx.fetch_add(1, Ordering::AcqRel) % self.capacity;
        let seq = self.write_idx.load(Ordering::Acquire) as u64;
        self.samples[idx].record(start, end, event_type, seq);
    }

    /// Get recent samples for analysis
    pub fn recent(&self, count: usize) -> Vec<(u64, u64, u64)> {
        let current = self.write_idx.load(Ordering::Acquire);
        let start = current.saturating_sub(count);
        let mut results = Vec::with_capacity(count);

        for i in start..current {
            let idx = i % self.capacity;
            let sample = &self.samples[idx];
            let latency = sample.latency_ns();
            let event_type = sample.event_type.load(Ordering::Acquire);
            let seq = sample.seq.load(Ordering::Acquire);
            if latency > 0 {
                results.push((latency, event_type, seq));
            }
        }
        results
    }

    /// Calculate percentiles from recent samples
    pub fn percentiles(&self, count: usize) -> LatencyPercentiles {
        let samples = self.recent(count);
        if samples.is_empty() {
            return LatencyPercentiles::default();
        }

        let mut latencies: Vec<u64> = samples.iter().map(|(l, _, _)| *l).collect();
        latencies.sort_unstable();

        let len = latencies.len();
        let p = |pct: f64| latencies[(len as f64 * pct / 100.0) as usize].min(latencies[len - 1]);

        LatencyPercentiles {
            min: latencies[0],
            p50: p(50.0),
            p90: p(90.0),
            p95: p(95.0),
            p99: p(99.0),
            p999: p(99.9),
            max: latencies[len - 1],
            count: len as u64,
            mean: latencies.iter().sum::<u64>() / len as u64,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct LatencyPercentiles {
    pub min: u64,
    pub p50: u64,
    pub p90: u64,
    pub p95: u64,
    pub p99: u64,
    pub p999: u64,
    pub max: u64,
    pub count: u64,
    pub mean: u64,
}

/// Event types for the HFT pipeline
#[repr(u64)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HftEventType {
    /// Price tick received from exchange
    TickReceived = 1,
    /// Price processed internally
    TickProcessed = 2,
    /// Signal generated
    SignalGenerated = 3,
    /// Order decision made
    OrderDecision = 4,
    /// Order serialized
    OrderSerialized = 5,
    /// Order sent to network
    OrderSent = 6,
    /// Order acknowledged by exchange
    OrderAcked = 7,
    /// Full tick-to-trade
    TickToTrade = 8,
    /// NIC hardware timestamp
    NicTimestamp = 9,
    /// FPGA timestamp
    FpgaTimestamp = 10,
    /// Kernel bypass event
    KernelBypass = 11,
}

impl From<u64> for HftEventType {
    fn from(v: u64) -> Self {
        match v {
            1 => Self::TickReceived,
            2 => Self::TickProcessed,
            3 => Self::SignalGenerated,
            4 => Self::OrderDecision,
            5 => Self::OrderSerialized,
            6 => Self::OrderSent,
            7 => Self::OrderAcked,
            8 => Self::TickToTrade,
            9 => Self::NicTimestamp,
            10 => Self::FpgaTimestamp,
            11 => Self::KernelBypass,
            _ => Self::TickReceived,
        }
    }
}

/// HFT Metrics Collector - central metrics aggregation
pub struct HftMetricsCollector {
    pub clock: HftClock,

    // Per-component latency buffers
    pub tick_receive: LatencyRingBuffer,
    pub signal_gen: LatencyRingBuffer,
    pub order_exec: LatencyRingBuffer,
    pub tick_to_trade: LatencyRingBuffer,

    // Network stack metrics
    pub nic_rx_packets: AtomicU64,
    pub nic_tx_packets: AtomicU64,
    pub nic_rx_bytes: AtomicU64,
    pub nic_tx_bytes: AtomicU64,
    pub kernel_bypass_packets: AtomicU64,

    // FPGA metrics (placeholders for hardware integration)
    pub fpga_latency_ticks: AtomicU64,
    pub fpga_events: AtomicU64,
    pub fpga_connected: AtomicCell<bool>,

    // Throughput counters
    pub ticks_per_sec: AtomicU64,
    pub signals_per_sec: AtomicU64,
    pub orders_per_sec: AtomicU64,

    // Jitter analysis
    pub jitter_samples: LatencyRingBuffer,
    pub max_jitter_ns: AtomicU64,

    // System health
    pub last_tick_ns: AtomicU64,
    pub gap_warnings: AtomicU64,
}

impl HftMetricsCollector {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            clock: HftClock::new(),
            tick_receive: LatencyRingBuffer::new(10_000),
            signal_gen: LatencyRingBuffer::new(10_000),
            order_exec: LatencyRingBuffer::new(10_000),
            tick_to_trade: LatencyRingBuffer::new(10_000),
            nic_rx_packets: AtomicU64::new(0),
            nic_tx_packets: AtomicU64::new(0),
            nic_rx_bytes: AtomicU64::new(0),
            nic_tx_bytes: AtomicU64::new(0),
            kernel_bypass_packets: AtomicU64::new(0),
            fpga_latency_ticks: AtomicU64::new(0),
            fpga_events: AtomicU64::new(0),
            fpga_connected: AtomicCell::new(false),
            ticks_per_sec: AtomicU64::new(0),
            signals_per_sec: AtomicU64::new(0),
            orders_per_sec: AtomicU64::new(0),
            jitter_samples: LatencyRingBuffer::new(1_000),
            max_jitter_ns: AtomicU64::new(0),
            last_tick_ns: AtomicU64::new(0),
            gap_warnings: AtomicU64::new(0),
        })
    }

    /// Record a tick received event
    #[inline(always)]
    pub fn record_tick(&self, start_ns: u64, end_ns: u64) {
        self.tick_receive
            .push(start_ns, end_ns, HftEventType::TickReceived as u64);

        // Jitter analysis
        let last = self.last_tick_ns.swap(end_ns, Ordering::AcqRel);
        if last > 0 {
            let gap = end_ns.saturating_sub(last);
            let expected = 1_000_000; // 1ms expected tick rate
            let jitter = if gap > expected {
                gap - expected
            } else {
                expected - gap
            };
            self.jitter_samples.push(last, end_ns, jitter);
            self.max_jitter_ns.fetch_max(jitter, Ordering::Relaxed);
        }
    }

    /// Record signal generation
    #[inline(always)]
    pub fn record_signal(&self, start_ns: u64, end_ns: u64) {
        self.signal_gen
            .push(start_ns, end_ns, HftEventType::SignalGenerated as u64);
    }

    /// Record order execution
    #[inline(always)]
    pub fn record_order(&self, start_ns: u64, end_ns: u64) {
        self.order_exec
            .push(start_ns, end_ns, HftEventType::OrderSent as u64);
    }

    /// Record full tick-to-trade latency
    #[inline(always)]
    pub fn record_tick_to_trade(&self, start_ns: u64, end_ns: u64) {
        self.tick_to_trade
            .push(start_ns, end_ns, HftEventType::TickToTrade as u64);
    }

    /// Get comprehensive snapshot for TUI
    pub fn snapshot(&self) -> HftMetricsSnapshot {
        HftMetricsSnapshot {
            tick_latency: self.tick_receive.percentiles(1000),
            signal_latency: self.signal_gen.percentiles(1000),
            order_latency: self.order_exec.percentiles(1000),
            t2t_latency: self.tick_to_trade.percentiles(1000),

            nic_rx_packets: self.nic_rx_packets.load(Ordering::Relaxed),
            nic_tx_packets: self.nic_tx_packets.load(Ordering::Relaxed),
            nic_rx_bytes: self.nic_rx_bytes.load(Ordering::Relaxed),
            nic_tx_bytes: self.nic_tx_bytes.load(Ordering::Relaxed),
            kernel_bypass_packets: self.kernel_bypass_packets.load(Ordering::Relaxed),

            fpga_connected: self.fpga_connected.load(),
            fpga_latency_ns: self.fpga_latency_ticks.load(Ordering::Relaxed),
            fpga_events: self.fpga_events.load(Ordering::Relaxed),

            ticks_per_sec: self.ticks_per_sec.load(Ordering::Relaxed),
            signals_per_sec: self.signals_per_sec.load(Ordering::Relaxed),
            orders_per_sec: self.orders_per_sec.load(Ordering::Relaxed),

            max_jitter_ns: self.max_jitter_ns.load(Ordering::Relaxed),
            gap_warnings: self.gap_warnings.load(Ordering::Relaxed),

            timestamp_ns: self.clock.now_ns(),
        }
    }
}

impl Default for HftMetricsCollector {
    fn default() -> Self {
        Arc::try_unwrap(Self::new()).unwrap_or_else(|arc| (*arc).clone_inner())
    }
}

impl HftMetricsCollector {
    fn clone_inner(&self) -> Self {
        Self {
            clock: self.clock.clone(),
            tick_receive: LatencyRingBuffer::new(10_000),
            signal_gen: LatencyRingBuffer::new(10_000),
            order_exec: LatencyRingBuffer::new(10_000),
            tick_to_trade: LatencyRingBuffer::new(10_000),
            nic_rx_packets: AtomicU64::new(self.nic_rx_packets.load(Ordering::Relaxed)),
            nic_tx_packets: AtomicU64::new(self.nic_tx_packets.load(Ordering::Relaxed)),
            nic_rx_bytes: AtomicU64::new(self.nic_rx_bytes.load(Ordering::Relaxed)),
            nic_tx_bytes: AtomicU64::new(self.nic_tx_bytes.load(Ordering::Relaxed)),
            kernel_bypass_packets: AtomicU64::new(
                self.kernel_bypass_packets.load(Ordering::Relaxed),
            ),
            fpga_latency_ticks: AtomicU64::new(self.fpga_latency_ticks.load(Ordering::Relaxed)),
            fpga_events: AtomicU64::new(self.fpga_events.load(Ordering::Relaxed)),
            fpga_connected: AtomicCell::new(self.fpga_connected.load()),
            ticks_per_sec: AtomicU64::new(self.ticks_per_sec.load(Ordering::Relaxed)),
            signals_per_sec: AtomicU64::new(self.signals_per_sec.load(Ordering::Relaxed)),
            orders_per_sec: AtomicU64::new(self.orders_per_sec.load(Ordering::Relaxed)),
            jitter_samples: LatencyRingBuffer::new(1_000),
            max_jitter_ns: AtomicU64::new(self.max_jitter_ns.load(Ordering::Relaxed)),
            last_tick_ns: AtomicU64::new(self.last_tick_ns.load(Ordering::Relaxed)),
            gap_warnings: AtomicU64::new(self.gap_warnings.load(Ordering::Relaxed)),
        }
    }
}

/// Snapshot of all HFT metrics for rendering
#[derive(Debug, Clone)]
pub struct HftMetricsSnapshot {
    // Latencies
    pub tick_latency: LatencyPercentiles,
    pub signal_latency: LatencyPercentiles,
    pub order_latency: LatencyPercentiles,
    pub t2t_latency: LatencyPercentiles,

    // Network
    pub nic_rx_packets: u64,
    pub nic_tx_packets: u64,
    pub nic_rx_bytes: u64,
    pub nic_tx_bytes: u64,
    pub kernel_bypass_packets: u64,

    // FPGA
    pub fpga_connected: bool,
    pub fpga_latency_ns: u64,
    pub fpga_events: u64,

    // Throughput
    pub ticks_per_sec: u64,
    pub signals_per_sec: u64,
    pub orders_per_sec: u64,

    // Jitter
    pub max_jitter_ns: u64,
    pub gap_warnings: u64,

    pub timestamp_ns: u64,
}

impl Default for HftMetricsSnapshot {
    fn default() -> Self {
        Self {
            tick_latency: LatencyPercentiles::default(),
            signal_latency: LatencyPercentiles::default(),
            order_latency: LatencyPercentiles::default(),
            t2t_latency: LatencyPercentiles::default(),
            nic_rx_packets: 0,
            nic_tx_packets: 0,
            nic_rx_bytes: 0,
            nic_tx_bytes: 0,
            kernel_bypass_packets: 0,
            fpga_connected: false,
            fpga_latency_ns: 0,
            fpga_events: 0,
            ticks_per_sec: 0,
            signals_per_sec: 0,
            orders_per_sec: 0,
            max_jitter_ns: 0,
            gap_warnings: 0,
            timestamp_ns: 0,
        }
    }
}

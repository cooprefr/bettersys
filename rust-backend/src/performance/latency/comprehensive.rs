//! Comprehensive HFT Metrics Collection
//!
//! Covers all performance dimensions for a low-latency trading system:
//! - Tick-to-trade latency breakdown
//! - Venue round-trip metrics
//! - Jitter and spike detection
//! - Throughput counters
//! - Queue/backpressure metrics
//! - Market data integrity
//! - Order lifecycle
//! - Failure/recovery tracking

use parking_lot::RwLock;
use serde::Serialize;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use super::LatencyHistogram;

// ============================================================================
// TICK-TO-TRADE BREAKDOWN
// ============================================================================

/// Granular tick-to-trade latency breakdown
#[derive(Debug)]
pub struct TickToTradeBreakdown {
    /// Market data receive (NIC → userspace)
    pub md_receive: LatencyHistogram,
    /// Market data decode/normalize
    pub md_decode: LatencyHistogram,
    /// Signal/strategy compute
    pub signal_compute: LatencyHistogram,
    /// Risk checks
    pub risk_check: LatencyHistogram,
    /// Order construction/serialization
    pub order_build: LatencyHistogram,
    /// Wire send (userspace → NIC)
    pub wire_send: LatencyHistogram,
    /// Total end-to-end
    pub total_t2t: LatencyHistogram,
    /// Samples for computing jitter
    recent_t2t_us: RwLock<VecDeque<u64>>,
}

impl Default for TickToTradeBreakdown {
    fn default() -> Self {
        Self::new()
    }
}

impl TickToTradeBreakdown {
    pub fn new() -> Self {
        Self {
            md_receive: LatencyHistogram::new(),
            md_decode: LatencyHistogram::new(),
            signal_compute: LatencyHistogram::new(),
            risk_check: LatencyHistogram::new(),
            order_build: LatencyHistogram::new(),
            wire_send: LatencyHistogram::new(),
            total_t2t: LatencyHistogram::new(),
            recent_t2t_us: RwLock::new(VecDeque::with_capacity(10000)),
        }
    }

    pub fn record_stage(&self, stage: T2TStage, latency_us: u64) {
        match stage {
            T2TStage::MdReceive => self.md_receive.record(latency_us),
            T2TStage::MdDecode => self.md_decode.record(latency_us),
            T2TStage::SignalCompute => self.signal_compute.record(latency_us),
            T2TStage::RiskCheck => self.risk_check.record(latency_us),
            T2TStage::OrderBuild => self.order_build.record(latency_us),
            T2TStage::WireSend => self.wire_send.record(latency_us),
            T2TStage::Total => {
                self.total_t2t.record(latency_us);
                let mut recent = self.recent_t2t_us.write();
                if recent.len() >= 10000 {
                    recent.pop_front();
                }
                recent.push_back(latency_us);
            }
        }
    }

    pub fn snapshot(&self) -> T2TSnapshot {
        T2TSnapshot {
            md_receive: self.md_receive.summary("md_receive"),
            md_decode: self.md_decode.summary("md_decode"),
            signal_compute: self.signal_compute.summary("signal_compute"),
            risk_check: self.risk_check.summary("risk_check"),
            order_build: self.order_build.summary("order_build"),
            wire_send: self.wire_send.summary("wire_send"),
            total: self.total_t2t.summary("total_t2t"),
            jitter: self.compute_jitter(),
        }
    }

    fn compute_jitter(&self) -> JitterMetrics {
        let recent = self.recent_t2t_us.read();
        if recent.len() < 2 {
            return JitterMetrics::default();
        }

        let n = recent.len() as f64;
        let mean = recent.iter().map(|&x| x as f64).sum::<f64>() / n;
        let variance = recent
            .iter()
            .map(|&x| {
                let diff = x as f64 - mean;
                diff * diff
            })
            .sum::<f64>()
            / n;
        let stddev = variance.sqrt();

        // Count spikes (>2x mean)
        let spike_threshold = (mean * 2.0) as u64;
        let spikes = recent.iter().filter(|&&x| x > spike_threshold).count();

        JitterMetrics {
            stddev_us: stddev as u64,
            variance_us: variance as u64,
            spike_count: spikes as u64,
            spike_rate_pct: (spikes as f64 / n) * 100.0,
            sample_count: recent.len() as u64,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum T2TStage {
    MdReceive,
    MdDecode,
    SignalCompute,
    RiskCheck,
    OrderBuild,
    WireSend,
    Total,
}

#[derive(Debug, Clone, Serialize)]
pub struct T2TSnapshot {
    pub md_receive: super::HistogramSummary,
    pub md_decode: super::HistogramSummary,
    pub signal_compute: super::HistogramSummary,
    pub risk_check: super::HistogramSummary,
    pub order_build: super::HistogramSummary,
    pub wire_send: super::HistogramSummary,
    pub total: super::HistogramSummary,
    pub jitter: JitterMetrics,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct JitterMetrics {
    pub stddev_us: u64,
    pub variance_us: u64,
    pub spike_count: u64,
    pub spike_rate_pct: f64,
    pub sample_count: u64,
}

// ============================================================================
// VENUE ROUND-TRIP (DETAILED)
// ============================================================================

#[derive(Debug)]
pub struct VenueRoundTrip {
    venues: RwLock<HashMap<String, VenueMetricsDetailed>>,
}

impl Default for VenueRoundTrip {
    fn default() -> Self {
        Self::new()
    }
}

impl VenueRoundTrip {
    pub fn new() -> Self {
        Self {
            venues: RwLock::new(HashMap::new()),
        }
    }

    fn ensure_venue(&self, venue: &str) {
        let mut venues = self.venues.write();
        if !venues.contains_key(venue) {
            venues.insert(venue.to_string(), VenueMetricsDetailed::new(venue));
        }
    }

    pub fn record_order_ack(&self, venue: &str, instrument: &str, latency_us: u64) {
        self.ensure_venue(venue);
        if let Some(v) = self.venues.write().get_mut(venue) {
            v.order_to_ack.record(latency_us);
            v.by_instrument
                .entry(instrument.to_string())
                .or_insert_with(LatencyHistogram::new)
                .record(latency_us);
        }
    }

    pub fn record_order_fill(&self, venue: &str, latency_us: u64) {
        self.ensure_venue(venue);
        if let Some(v) = self.venues.write().get_mut(venue) {
            v.order_to_fill.record(latency_us);
        }
    }

    pub fn record_cancel_ack(&self, venue: &str, latency_us: u64) {
        self.ensure_venue(venue);
        if let Some(v) = self.venues.write().get_mut(venue) {
            v.cancel_to_ack.record(latency_us);
        }
    }

    pub fn record_reject(&self, venue: &str, reason: &str) {
        self.ensure_venue(venue);
        if let Some(v) = self.venues.write().get_mut(venue) {
            v.rejects.fetch_add(1, Ordering::Relaxed);
            *v.reject_reasons.entry(reason.to_string()).or_insert(0) += 1;
        }
    }

    pub fn snapshot(&self) -> Vec<VenueRTSnapshot> {
        self.venues.read().values().map(|v| v.snapshot()).collect()
    }
}

#[derive(Debug)]
struct VenueMetricsDetailed {
    name: String,
    order_to_ack: LatencyHistogram,
    order_to_fill: LatencyHistogram,
    cancel_to_ack: LatencyHistogram,
    by_instrument: HashMap<String, LatencyHistogram>,
    rejects: AtomicU64,
    reject_reasons: HashMap<String, u64>,
}

impl VenueMetricsDetailed {
    fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            order_to_ack: LatencyHistogram::new(),
            order_to_fill: LatencyHistogram::new(),
            cancel_to_ack: LatencyHistogram::new(),
            by_instrument: HashMap::new(),
            rejects: AtomicU64::new(0),
            reject_reasons: HashMap::new(),
        }
    }

    fn snapshot(&self) -> VenueRTSnapshot {
        VenueRTSnapshot {
            venue: self.name.clone(),
            order_to_ack: self.order_to_ack.summary("order_to_ack"),
            order_to_fill: self.order_to_fill.summary("order_to_fill"),
            cancel_to_ack: self.cancel_to_ack.summary("cancel_to_ack"),
            rejects: self.rejects.load(Ordering::Relaxed),
            reject_reasons: self.reject_reasons.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct VenueRTSnapshot {
    pub venue: String,
    pub order_to_ack: super::HistogramSummary,
    pub order_to_fill: super::HistogramSummary,
    pub cancel_to_ack: super::HistogramSummary,
    pub rejects: u64,
    pub reject_reasons: HashMap<String, u64>,
}

// ============================================================================
// THROUGHPUT COUNTERS (PER STAGE)
// ============================================================================

#[derive(Debug)]
pub struct ThroughputCounters {
    start_time: Instant,
    // Market data
    pub md_messages_in: AtomicU64,
    pub md_bytes_in: AtomicU64,
    pub md_decode_count: AtomicU64,
    // Signal/strategy
    pub strategy_evals: AtomicU64,
    pub signals_generated: AtomicU64,
    // Risk
    pub risk_checks: AtomicU64,
    pub risk_rejects: AtomicU64,
    // Orders
    pub orders_sent: AtomicU64,
    pub orders_filled: AtomicU64,
    pub orders_rejected: AtomicU64,
    pub cancels_sent: AtomicU64,
    // Persistence
    pub db_writes: AtomicU64,
    pub db_reads: AtomicU64,
    pub logs_written: AtomicU64,
    pub logs_dropped: AtomicU64,
}

impl Default for ThroughputCounters {
    fn default() -> Self {
        Self::new()
    }
}

impl ThroughputCounters {
    pub fn new() -> Self {
        Self {
            start_time: Instant::now(),
            md_messages_in: AtomicU64::new(0),
            md_bytes_in: AtomicU64::new(0),
            md_decode_count: AtomicU64::new(0),
            strategy_evals: AtomicU64::new(0),
            signals_generated: AtomicU64::new(0),
            risk_checks: AtomicU64::new(0),
            risk_rejects: AtomicU64::new(0),
            orders_sent: AtomicU64::new(0),
            orders_filled: AtomicU64::new(0),
            orders_rejected: AtomicU64::new(0),
            cancels_sent: AtomicU64::new(0),
            db_writes: AtomicU64::new(0),
            db_reads: AtomicU64::new(0),
            logs_written: AtomicU64::new(0),
            logs_dropped: AtomicU64::new(0),
        }
    }

    pub fn snapshot(&self) -> ThroughputSnapshot {
        let elapsed = self.start_time.elapsed().as_secs_f64().max(0.001);
        ThroughputSnapshot {
            uptime_secs: elapsed,
            md_messages_per_sec: self.md_messages_in.load(Ordering::Relaxed) as f64 / elapsed,
            md_bytes_per_sec: self.md_bytes_in.load(Ordering::Relaxed) as f64 / elapsed,
            md_decode_per_sec: self.md_decode_count.load(Ordering::Relaxed) as f64 / elapsed,
            strategy_evals_per_sec: self.strategy_evals.load(Ordering::Relaxed) as f64 / elapsed,
            signals_per_sec: self.signals_generated.load(Ordering::Relaxed) as f64 / elapsed,
            risk_checks_per_sec: self.risk_checks.load(Ordering::Relaxed) as f64 / elapsed,
            orders_per_sec: self.orders_sent.load(Ordering::Relaxed) as f64 / elapsed,
            cancels_per_sec: self.cancels_sent.load(Ordering::Relaxed) as f64 / elapsed,
            db_writes_per_sec: self.db_writes.load(Ordering::Relaxed) as f64 / elapsed,
            fill_rate_pct: {
                let sent = self.orders_sent.load(Ordering::Relaxed);
                let filled = self.orders_filled.load(Ordering::Relaxed);
                if sent > 0 {
                    (filled as f64 / sent as f64) * 100.0
                } else {
                    0.0
                }
            },
            reject_rate_pct: {
                let sent = self.orders_sent.load(Ordering::Relaxed);
                let rejected = self.orders_rejected.load(Ordering::Relaxed);
                if sent > 0 {
                    (rejected as f64 / sent as f64) * 100.0
                } else {
                    0.0
                }
            },
            log_drop_rate_pct: {
                let written = self.logs_written.load(Ordering::Relaxed);
                let dropped = self.logs_dropped.load(Ordering::Relaxed);
                let total = written + dropped;
                if total > 0 {
                    (dropped as f64 / total as f64) * 100.0
                } else {
                    0.0
                }
            },
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ThroughputSnapshot {
    pub uptime_secs: f64,
    pub md_messages_per_sec: f64,
    pub md_bytes_per_sec: f64,
    pub md_decode_per_sec: f64,
    pub strategy_evals_per_sec: f64,
    pub signals_per_sec: f64,
    pub risk_checks_per_sec: f64,
    pub orders_per_sec: f64,
    pub cancels_per_sec: f64,
    pub db_writes_per_sec: f64,
    pub fill_rate_pct: f64,
    pub reject_rate_pct: f64,
    pub log_drop_rate_pct: f64,
}

// ============================================================================
// QUEUE / BACKPRESSURE METRICS
// ============================================================================

#[derive(Debug)]
pub struct QueueMetrics {
    queues: RwLock<HashMap<String, QueueStats>>,
}

impl Default for QueueMetrics {
    fn default() -> Self {
        Self::new()
    }
}

impl QueueMetrics {
    pub fn new() -> Self {
        Self {
            queues: RwLock::new(HashMap::new()),
        }
    }

    pub fn register(&self, name: &str, capacity: usize) {
        let mut queues = self.queues.write();
        queues
            .entry(name.to_string())
            .or_insert_with(|| QueueStats::new(name, capacity));
    }

    pub fn record_enqueue(&self, name: &str, depth: usize, wait_us: u64) {
        if let Some(q) = self.queues.write().get_mut(name) {
            q.current_depth.store(depth as u64, Ordering::Relaxed);
            q.max_depth.fetch_max(depth as u64, Ordering::Relaxed);
            q.enqueues.fetch_add(1, Ordering::Relaxed);
            q.wait_time.record(wait_us);
        }
    }

    pub fn record_dequeue(&self, name: &str, depth: usize) {
        if let Some(q) = self.queues.write().get_mut(name) {
            q.current_depth.store(depth as u64, Ordering::Relaxed);
            q.dequeues.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn record_drop(&self, name: &str) {
        if let Some(q) = self.queues.write().get_mut(name) {
            q.drops.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn record_blocked(&self, name: &str, blocked_us: u64) {
        if let Some(q) = self.queues.write().get_mut(name) {
            q.blocked_time_us.fetch_add(blocked_us, Ordering::Relaxed);
            q.blocked_count.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn snapshot(&self) -> Vec<QueueStatsSnapshot> {
        self.queues.read().values().map(|q| q.snapshot()).collect()
    }
}

#[derive(Debug)]
struct QueueStats {
    name: String,
    capacity: usize,
    current_depth: AtomicU64,
    max_depth: AtomicU64,
    enqueues: AtomicU64,
    dequeues: AtomicU64,
    drops: AtomicU64,
    wait_time: LatencyHistogram,
    blocked_time_us: AtomicU64,
    blocked_count: AtomicU64,
}

impl QueueStats {
    fn new(name: &str, capacity: usize) -> Self {
        Self {
            name: name.to_string(),
            capacity,
            current_depth: AtomicU64::new(0),
            max_depth: AtomicU64::new(0),
            enqueues: AtomicU64::new(0),
            dequeues: AtomicU64::new(0),
            drops: AtomicU64::new(0),
            wait_time: LatencyHistogram::new(),
            blocked_time_us: AtomicU64::new(0),
            blocked_count: AtomicU64::new(0),
        }
    }

    fn snapshot(&self) -> QueueStatsSnapshot {
        let enqueues = self.enqueues.load(Ordering::Relaxed);
        let drops = self.drops.load(Ordering::Relaxed);
        QueueStatsSnapshot {
            name: self.name.clone(),
            capacity: self.capacity,
            current_depth: self.current_depth.load(Ordering::Relaxed),
            max_depth: self.max_depth.load(Ordering::Relaxed),
            utilization_pct: (self.current_depth.load(Ordering::Relaxed) as f64
                / self.capacity as f64)
                * 100.0,
            enqueues,
            dequeues: self.dequeues.load(Ordering::Relaxed),
            drops,
            drop_rate_pct: if enqueues + drops > 0 {
                (drops as f64 / (enqueues + drops) as f64) * 100.0
            } else {
                0.0
            },
            wait_time: self.wait_time.summary("wait"),
            blocked_time_us: self.blocked_time_us.load(Ordering::Relaxed),
            blocked_count: self.blocked_count.load(Ordering::Relaxed),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct QueueStatsSnapshot {
    pub name: String,
    pub capacity: usize,
    pub current_depth: u64,
    pub max_depth: u64,
    pub utilization_pct: f64,
    pub enqueues: u64,
    pub dequeues: u64,
    pub drops: u64,
    pub drop_rate_pct: f64,
    pub wait_time: super::HistogramSummary,
    pub blocked_time_us: u64,
    pub blocked_count: u64,
}

// ============================================================================
// MARKET DATA INTEGRITY
// ============================================================================

#[derive(Debug)]
pub struct MarketDataIntegrity {
    // Sequence tracking per source
    sources: RwLock<HashMap<String, SourceIntegrity>>,
}

impl Default for MarketDataIntegrity {
    fn default() -> Self {
        Self::new()
    }
}

impl MarketDataIntegrity {
    pub fn new() -> Self {
        Self {
            sources: RwLock::new(HashMap::new()),
        }
    }

    fn ensure_source(&self, source: &str) {
        let mut sources = self.sources.write();
        if !sources.contains_key(source) {
            sources.insert(source.to_string(), SourceIntegrity::new(source));
        }
    }

    pub fn record_message(&self, source: &str, seq: Option<u64>, ts_exchange_us: Option<u64>) {
        self.ensure_source(source);
        if let Some(s) = self.sources.write().get_mut(source) {
            s.messages.fetch_add(1, Ordering::Relaxed);

            // Check sequence
            if let Some(seq) = seq {
                let last = s.last_seq.swap(seq, Ordering::Relaxed);
                if last > 0 && seq != last + 1 {
                    if seq < last {
                        s.out_of_order.fetch_add(1, Ordering::Relaxed);
                    } else if seq > last + 1 {
                        s.gaps.fetch_add((seq - last - 1) as u64, Ordering::Relaxed);
                    }
                }
                if seq == last {
                    s.duplicates.fetch_add(1, Ordering::Relaxed);
                }
            }

            // Clock skew
            if let Some(exchange_us) = ts_exchange_us {
                let now_us = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_micros() as u64;
                let skew = if now_us > exchange_us {
                    now_us - exchange_us
                } else {
                    exchange_us - now_us
                };
                s.max_clock_skew_us.fetch_max(skew, Ordering::Relaxed);
            }
        }
    }

    pub fn record_recovery(&self, source: &str, recovery_time_us: u64) {
        self.ensure_source(source);
        if let Some(s) = self.sources.write().get_mut(source) {
            s.recoveries.fetch_add(1, Ordering::Relaxed);
            s.recovery_time.record(recovery_time_us);
        }
    }

    pub fn snapshot(&self) -> Vec<SourceIntegritySnapshot> {
        self.sources.read().values().map(|s| s.snapshot()).collect()
    }
}

#[derive(Debug)]
struct SourceIntegrity {
    name: String,
    messages: AtomicU64,
    gaps: AtomicU64,
    out_of_order: AtomicU64,
    duplicates: AtomicU64,
    last_seq: AtomicU64,
    max_clock_skew_us: AtomicU64,
    recoveries: AtomicU64,
    recovery_time: LatencyHistogram,
}

impl SourceIntegrity {
    fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            messages: AtomicU64::new(0),
            gaps: AtomicU64::new(0),
            out_of_order: AtomicU64::new(0),
            duplicates: AtomicU64::new(0),
            last_seq: AtomicU64::new(0),
            max_clock_skew_us: AtomicU64::new(0),
            recoveries: AtomicU64::new(0),
            recovery_time: LatencyHistogram::new(),
        }
    }

    fn snapshot(&self) -> SourceIntegritySnapshot {
        let msgs = self.messages.load(Ordering::Relaxed);
        SourceIntegritySnapshot {
            source: self.name.clone(),
            messages: msgs,
            gaps: self.gaps.load(Ordering::Relaxed),
            out_of_order: self.out_of_order.load(Ordering::Relaxed),
            duplicates: self.duplicates.load(Ordering::Relaxed),
            gap_rate_pct: if msgs > 0 {
                (self.gaps.load(Ordering::Relaxed) as f64 / msgs as f64) * 100.0
            } else {
                0.0
            },
            ooo_rate_pct: if msgs > 0 {
                (self.out_of_order.load(Ordering::Relaxed) as f64 / msgs as f64) * 100.0
            } else {
                0.0
            },
            dup_rate_pct: if msgs > 0 {
                (self.duplicates.load(Ordering::Relaxed) as f64 / msgs as f64) * 100.0
            } else {
                0.0
            },
            max_clock_skew_us: self.max_clock_skew_us.load(Ordering::Relaxed),
            recoveries: self.recoveries.load(Ordering::Relaxed),
            recovery_time: self.recovery_time.summary("recovery"),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SourceIntegritySnapshot {
    pub source: String,
    pub messages: u64,
    pub gaps: u64,
    pub out_of_order: u64,
    pub duplicates: u64,
    pub gap_rate_pct: f64,
    pub ooo_rate_pct: f64,
    pub dup_rate_pct: f64,
    pub max_clock_skew_us: u64,
    pub recoveries: u64,
    pub recovery_time: super::HistogramSummary,
}

// ============================================================================
// ORDER LIFECYCLE
// ============================================================================

#[derive(Debug)]
pub struct OrderLifecycle {
    pub orders_created: AtomicU64,
    pub orders_sent: AtomicU64,
    pub orders_acked: AtomicU64,
    pub orders_filled: AtomicU64,
    pub orders_partial: AtomicU64,
    pub orders_rejected: AtomicU64,
    pub orders_cancelled: AtomicU64,
    pub cancel_rejects: AtomicU64,
    pub stale_quotes: AtomicU64,
    pub crossed_blocked: AtomicU64,
    pub invalid_blocked: AtomicU64,
    reject_reasons: RwLock<HashMap<String, u64>>,
    // Time in queue before send
    pub queue_time: LatencyHistogram,
}

impl Default for OrderLifecycle {
    fn default() -> Self {
        Self::new()
    }
}

impl OrderLifecycle {
    pub fn new() -> Self {
        Self {
            orders_created: AtomicU64::new(0),
            orders_sent: AtomicU64::new(0),
            orders_acked: AtomicU64::new(0),
            orders_filled: AtomicU64::new(0),
            orders_partial: AtomicU64::new(0),
            orders_rejected: AtomicU64::new(0),
            orders_cancelled: AtomicU64::new(0),
            cancel_rejects: AtomicU64::new(0),
            stale_quotes: AtomicU64::new(0),
            crossed_blocked: AtomicU64::new(0),
            invalid_blocked: AtomicU64::new(0),
            reject_reasons: RwLock::new(HashMap::new()),
            queue_time: LatencyHistogram::new(),
        }
    }

    pub fn record_reject(&self, reason: &str) {
        self.orders_rejected.fetch_add(1, Ordering::Relaxed);
        *self
            .reject_reasons
            .write()
            .entry(reason.to_string())
            .or_insert(0) += 1;
    }

    pub fn snapshot(&self) -> OrderLifecycleSnapshot {
        let sent = self.orders_sent.load(Ordering::Relaxed);
        OrderLifecycleSnapshot {
            orders_created: self.orders_created.load(Ordering::Relaxed),
            orders_sent: sent,
            orders_acked: self.orders_acked.load(Ordering::Relaxed),
            orders_filled: self.orders_filled.load(Ordering::Relaxed),
            orders_partial: self.orders_partial.load(Ordering::Relaxed),
            orders_rejected: self.orders_rejected.load(Ordering::Relaxed),
            orders_cancelled: self.orders_cancelled.load(Ordering::Relaxed),
            cancel_rejects: self.cancel_rejects.load(Ordering::Relaxed),
            stale_quotes: self.stale_quotes.load(Ordering::Relaxed),
            crossed_blocked: self.crossed_blocked.load(Ordering::Relaxed),
            invalid_blocked: self.invalid_blocked.load(Ordering::Relaxed),
            reject_rate_pct: if sent > 0 {
                (self.orders_rejected.load(Ordering::Relaxed) as f64 / sent as f64) * 100.0
            } else {
                0.0
            },
            fill_rate_pct: if sent > 0 {
                (self.orders_filled.load(Ordering::Relaxed) as f64 / sent as f64) * 100.0
            } else {
                0.0
            },
            partial_rate_pct: if sent > 0 {
                (self.orders_partial.load(Ordering::Relaxed) as f64 / sent as f64) * 100.0
            } else {
                0.0
            },
            reject_reasons: self.reject_reasons.read().clone(),
            queue_time: self.queue_time.summary("queue"),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct OrderLifecycleSnapshot {
    pub orders_created: u64,
    pub orders_sent: u64,
    pub orders_acked: u64,
    pub orders_filled: u64,
    pub orders_partial: u64,
    pub orders_rejected: u64,
    pub orders_cancelled: u64,
    pub cancel_rejects: u64,
    pub stale_quotes: u64,
    pub crossed_blocked: u64,
    pub invalid_blocked: u64,
    pub reject_rate_pct: f64,
    pub fill_rate_pct: f64,
    pub partial_rate_pct: f64,
    pub reject_reasons: HashMap<String, u64>,
    pub queue_time: super::HistogramSummary,
}

// ============================================================================
// FAILURE / RECOVERY TRACKING
// ============================================================================

#[derive(Debug)]
pub struct FailureTracking {
    pub reconnects: AtomicU64,
    pub recovery_time: LatencyHistogram,
    pub warmup_time: LatencyHistogram,
    pub circuit_breaker_trips: AtomicU64,
    pub watchdog_resets: AtomicU64,
    pub crash_recoveries: AtomicU64,
    pub degraded_mode_time_us: AtomicU64,
    component_failures: RwLock<HashMap<String, u64>>,
}

impl Default for FailureTracking {
    fn default() -> Self {
        Self::new()
    }
}

impl FailureTracking {
    pub fn new() -> Self {
        Self {
            reconnects: AtomicU64::new(0),
            recovery_time: LatencyHistogram::new(),
            warmup_time: LatencyHistogram::new(),
            circuit_breaker_trips: AtomicU64::new(0),
            watchdog_resets: AtomicU64::new(0),
            crash_recoveries: AtomicU64::new(0),
            degraded_mode_time_us: AtomicU64::new(0),
            component_failures: RwLock::new(HashMap::new()),
        }
    }

    pub fn record_reconnect(&self, component: &str, recovery_us: u64) {
        self.reconnects.fetch_add(1, Ordering::Relaxed);
        self.recovery_time.record(recovery_us);
        *self
            .component_failures
            .write()
            .entry(component.to_string())
            .or_insert(0) += 1;
    }

    pub fn record_warmup(&self, warmup_us: u64) {
        self.warmup_time.record(warmup_us);
    }

    pub fn snapshot(&self) -> FailureSnapshot {
        FailureSnapshot {
            reconnects: self.reconnects.load(Ordering::Relaxed),
            recovery_time: self.recovery_time.summary("recovery"),
            warmup_time: self.warmup_time.summary("warmup"),
            circuit_breaker_trips: self.circuit_breaker_trips.load(Ordering::Relaxed),
            watchdog_resets: self.watchdog_resets.load(Ordering::Relaxed),
            crash_recoveries: self.crash_recoveries.load(Ordering::Relaxed),
            degraded_mode_time_us: self.degraded_mode_time_us.load(Ordering::Relaxed),
            component_failures: self.component_failures.read().clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct FailureSnapshot {
    pub reconnects: u64,
    pub recovery_time: super::HistogramSummary,
    pub warmup_time: super::HistogramSummary,
    pub circuit_breaker_trips: u64,
    pub watchdog_resets: u64,
    pub crash_recoveries: u64,
    pub degraded_mode_time_us: u64,
    pub component_failures: HashMap<String, u64>,
}

// ============================================================================
// SERIALIZATION COSTS
// ============================================================================

#[derive(Debug)]
pub struct SerializationMetrics {
    pub encode_time: LatencyHistogram,
    pub decode_time: LatencyHistogram,
    pub bytes_encoded: AtomicU64,
    pub bytes_decoded: AtomicU64,
    pub encode_count: AtomicU64,
    pub decode_count: AtomicU64,
    pub zero_copy_count: AtomicU64,
    pub memcpy_count: AtomicU64,
    by_message_type: RwLock<HashMap<String, MessageTypeStats>>,
}

impl Default for SerializationMetrics {
    fn default() -> Self {
        Self::new()
    }
}

impl SerializationMetrics {
    pub fn new() -> Self {
        Self {
            encode_time: LatencyHistogram::new(),
            decode_time: LatencyHistogram::new(),
            bytes_encoded: AtomicU64::new(0),
            bytes_decoded: AtomicU64::new(0),
            encode_count: AtomicU64::new(0),
            decode_count: AtomicU64::new(0),
            zero_copy_count: AtomicU64::new(0),
            memcpy_count: AtomicU64::new(0),
            by_message_type: RwLock::new(HashMap::new()),
        }
    }

    pub fn record_encode(&self, msg_type: &str, duration_us: u64, bytes: u64, zero_copy: bool) {
        self.encode_time.record(duration_us);
        self.bytes_encoded.fetch_add(bytes, Ordering::Relaxed);
        self.encode_count.fetch_add(1, Ordering::Relaxed);
        if zero_copy {
            self.zero_copy_count.fetch_add(1, Ordering::Relaxed);
        } else {
            self.memcpy_count.fetch_add(1, Ordering::Relaxed);
        }

        let mut by_type = self.by_message_type.write();
        let stats = by_type
            .entry(msg_type.to_string())
            .or_insert_with(MessageTypeStats::new);
        stats.encode_time.record(duration_us);
        stats.encode_bytes.fetch_add(bytes, Ordering::Relaxed);
        stats.encode_count.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_decode(&self, msg_type: &str, duration_us: u64, bytes: u64) {
        self.decode_time.record(duration_us);
        self.bytes_decoded.fetch_add(bytes, Ordering::Relaxed);
        self.decode_count.fetch_add(1, Ordering::Relaxed);

        let mut by_type = self.by_message_type.write();
        let stats = by_type
            .entry(msg_type.to_string())
            .or_insert_with(MessageTypeStats::new);
        stats.decode_time.record(duration_us);
        stats.decode_bytes.fetch_add(bytes, Ordering::Relaxed);
        stats.decode_count.fetch_add(1, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> SerializationSnapshot {
        let encode_count = self.encode_count.load(Ordering::Relaxed);
        let decode_count = self.decode_count.load(Ordering::Relaxed);
        SerializationSnapshot {
            encode_time: self.encode_time.summary("encode"),
            decode_time: self.decode_time.summary("decode"),
            bytes_encoded: self.bytes_encoded.load(Ordering::Relaxed),
            bytes_decoded: self.bytes_decoded.load(Ordering::Relaxed),
            encode_count,
            decode_count,
            avg_encode_bytes: if encode_count > 0 {
                self.bytes_encoded.load(Ordering::Relaxed) / encode_count
            } else {
                0
            },
            avg_decode_bytes: if decode_count > 0 {
                self.bytes_decoded.load(Ordering::Relaxed) / decode_count
            } else {
                0
            },
            zero_copy_rate_pct: {
                let total = self.zero_copy_count.load(Ordering::Relaxed)
                    + self.memcpy_count.load(Ordering::Relaxed);
                if total > 0 {
                    (self.zero_copy_count.load(Ordering::Relaxed) as f64 / total as f64) * 100.0
                } else {
                    0.0
                }
            },
            by_message_type: self
                .by_message_type
                .read()
                .iter()
                .map(|(k, v)| (k.clone(), v.snapshot()))
                .collect(),
        }
    }
}

#[derive(Debug)]
struct MessageTypeStats {
    encode_time: LatencyHistogram,
    decode_time: LatencyHistogram,
    encode_bytes: AtomicU64,
    decode_bytes: AtomicU64,
    encode_count: AtomicU64,
    decode_count: AtomicU64,
}

impl MessageTypeStats {
    fn new() -> Self {
        Self {
            encode_time: LatencyHistogram::new(),
            decode_time: LatencyHistogram::new(),
            encode_bytes: AtomicU64::new(0),
            decode_bytes: AtomicU64::new(0),
            encode_count: AtomicU64::new(0),
            decode_count: AtomicU64::new(0),
        }
    }

    fn snapshot(&self) -> MessageTypeSnapshot {
        let ec = self.encode_count.load(Ordering::Relaxed);
        let dc = self.decode_count.load(Ordering::Relaxed);
        MessageTypeSnapshot {
            encode_time: self.encode_time.summary("encode"),
            decode_time: self.decode_time.summary("decode"),
            avg_encode_bytes: if ec > 0 {
                self.encode_bytes.load(Ordering::Relaxed) / ec
            } else {
                0
            },
            avg_decode_bytes: if dc > 0 {
                self.decode_bytes.load(Ordering::Relaxed) / dc
            } else {
                0
            },
            encode_count: ec,
            decode_count: dc,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SerializationSnapshot {
    pub encode_time: super::HistogramSummary,
    pub decode_time: super::HistogramSummary,
    pub bytes_encoded: u64,
    pub bytes_decoded: u64,
    pub encode_count: u64,
    pub decode_count: u64,
    pub avg_encode_bytes: u64,
    pub avg_decode_bytes: u64,
    pub zero_copy_rate_pct: f64,
    pub by_message_type: HashMap<String, MessageTypeSnapshot>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MessageTypeSnapshot {
    pub encode_time: super::HistogramSummary,
    pub decode_time: super::HistogramSummary,
    pub avg_encode_bytes: u64,
    pub avg_decode_bytes: u64,
    pub encode_count: u64,
    pub decode_count: u64,
}

// ============================================================================
// GLOBAL COMPREHENSIVE METRICS
// ============================================================================

#[derive(Debug)]
pub struct ComprehensiveMetrics {
    pub t2t: TickToTradeBreakdown,
    pub venue_rt: VenueRoundTrip,
    pub throughput: ThroughputCounters,
    pub queues: QueueMetrics,
    pub md_integrity: MarketDataIntegrity,
    pub order_lifecycle: OrderLifecycle,
    pub failures: FailureTracking,
    pub serialization: SerializationMetrics,
}

impl Default for ComprehensiveMetrics {
    fn default() -> Self {
        Self::new()
    }
}

impl ComprehensiveMetrics {
    pub fn new() -> Self {
        Self {
            t2t: TickToTradeBreakdown::new(),
            venue_rt: VenueRoundTrip::new(),
            throughput: ThroughputCounters::new(),
            queues: QueueMetrics::new(),
            md_integrity: MarketDataIntegrity::new(),
            order_lifecycle: OrderLifecycle::new(),
            failures: FailureTracking::new(),
            serialization: SerializationMetrics::new(),
        }
    }

    pub fn snapshot(&self) -> ComprehensiveSnapshot {
        ComprehensiveSnapshot {
            timestamp: chrono::Utc::now().timestamp(),
            t2t: self.t2t.snapshot(),
            venue_rt: self.venue_rt.snapshot(),
            throughput: self.throughput.snapshot(),
            queues: self.queues.snapshot(),
            md_integrity: self.md_integrity.snapshot(),
            order_lifecycle: self.order_lifecycle.snapshot(),
            failures: self.failures.snapshot(),
            serialization: self.serialization.snapshot(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ComprehensiveSnapshot {
    pub timestamp: i64,
    pub t2t: T2TSnapshot,
    pub venue_rt: Vec<VenueRTSnapshot>,
    pub throughput: ThroughputSnapshot,
    pub queues: Vec<QueueStatsSnapshot>,
    pub md_integrity: Vec<SourceIntegritySnapshot>,
    pub order_lifecycle: OrderLifecycleSnapshot,
    pub failures: FailureSnapshot,
    pub serialization: SerializationSnapshot,
}

/// Global comprehensive metrics instance
pub fn global_comprehensive() -> &'static ComprehensiveMetrics {
    static METRICS: std::sync::OnceLock<ComprehensiveMetrics> = std::sync::OnceLock::new();
    METRICS.get_or_init(ComprehensiveMetrics::new)
}

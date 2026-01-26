//! Queue depth and wait time monitoring
//!
//! Track bounded channel statistics for backpressure analysis.

use parking_lot::RwLock;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Instant;

use crate::latency::LatencyHistogram;

/// Registry of monitored queues
pub struct QueueRegistry {
    queues: RwLock<HashMap<String, QueueMetrics>>,
}

impl Default for QueueRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl QueueRegistry {
    pub fn new() -> Self {
        Self {
            queues: RwLock::new(HashMap::new()),
        }
    }

    /// Register a new queue for monitoring
    pub fn register(&self, name: impl Into<String>, capacity: usize) {
        let name = name.into();
        let mut queues = self.queues.write();
        queues.insert(name.clone(), QueueMetrics::new(name, capacity));
    }

    /// Record an enqueue operation
    pub fn record_enqueue(&self, name: &str, queue_depth: usize, wait_us: u64) {
        if let Some(metrics) = self.queues.write().get_mut(name) {
            metrics.record_enqueue(queue_depth, wait_us);
        }
    }

    /// Record a dequeue operation
    pub fn record_dequeue(&self, name: &str, queue_depth: usize, wait_us: u64) {
        if let Some(metrics) = self.queues.write().get_mut(name) {
            metrics.record_dequeue(queue_depth, wait_us);
        }
    }

    /// Update current depth (for channels that expose len())
    pub fn update_depth(&self, name: &str, depth: usize) {
        if let Some(metrics) = self.queues.write().get_mut(name) {
            metrics.current_depth.store(depth, Ordering::Relaxed);
            metrics.max_depth.fetch_max(depth, Ordering::Relaxed);
        }
    }

    /// Get snapshot of all queue metrics
    pub fn snapshot(&self) -> Vec<QueueSnapshot> {
        self.queues.read().values().map(|m| m.snapshot()).collect()
    }

    /// Get snapshot for a specific queue
    pub fn get(&self, name: &str) -> Option<QueueSnapshot> {
        self.queues.read().get(name).map(|m| m.snapshot())
    }
}

/// Metrics for a single queue
pub struct QueueMetrics {
    pub name: String,
    pub capacity: usize,
    pub current_depth: AtomicUsize,
    pub max_depth: AtomicUsize,
    pub total_enqueued: AtomicU64,
    pub total_dequeued: AtomicU64,
    pub dropped: AtomicU64,
    pub enqueue_wait: LatencyHistogram,
    pub dequeue_wait: LatencyHistogram,
    pub created_at: Instant,
}

impl QueueMetrics {
    pub fn new(name: String, capacity: usize) -> Self {
        Self {
            name,
            capacity,
            current_depth: AtomicUsize::new(0),
            max_depth: AtomicUsize::new(0),
            total_enqueued: AtomicU64::new(0),
            total_dequeued: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
            enqueue_wait: LatencyHistogram::new(),
            dequeue_wait: LatencyHistogram::new(),
            created_at: Instant::now(),
        }
    }

    pub fn record_enqueue(&mut self, depth: usize, wait_us: u64) {
        self.current_depth.store(depth, Ordering::Relaxed);
        self.max_depth.fetch_max(depth, Ordering::Relaxed);
        self.total_enqueued.fetch_add(1, Ordering::Relaxed);
        self.enqueue_wait.record(wait_us);
    }

    pub fn record_dequeue(&mut self, depth: usize, wait_us: u64) {
        self.current_depth.store(depth, Ordering::Relaxed);
        self.total_dequeued.fetch_add(1, Ordering::Relaxed);
        self.dequeue_wait.record(wait_us);
    }

    pub fn record_drop(&self) {
        self.dropped.fetch_add(1, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> QueueSnapshot {
        let uptime = self.created_at.elapsed().as_secs_f64();
        let enqueued = self.total_enqueued.load(Ordering::Relaxed);
        let dequeued = self.total_dequeued.load(Ordering::Relaxed);

        QueueSnapshot {
            name: self.name.clone(),
            capacity: self.capacity,
            current_depth: self.current_depth.load(Ordering::Relaxed),
            max_depth: self.max_depth.load(Ordering::Relaxed),
            utilization_pct: (self.current_depth.load(Ordering::Relaxed) as f64
                / self.capacity as f64)
                * 100.0,
            total_enqueued: enqueued,
            total_dequeued: dequeued,
            dropped: self.dropped.load(Ordering::Relaxed),
            enqueue_rate_per_sec: enqueued as f64 / uptime,
            dequeue_rate_per_sec: dequeued as f64 / uptime,
            enqueue_wait_p50_us: self.enqueue_wait.p50(),
            enqueue_wait_p99_us: self.enqueue_wait.p99(),
            dequeue_wait_p50_us: self.dequeue_wait.p50(),
            dequeue_wait_p99_us: self.dequeue_wait.p99(),
        }
    }
}

/// Snapshot of queue metrics for serialization
#[derive(Debug, Clone, Serialize)]
pub struct QueueSnapshot {
    pub name: String,
    pub capacity: usize,
    pub current_depth: usize,
    pub max_depth: usize,
    pub utilization_pct: f64,
    pub total_enqueued: u64,
    pub total_dequeued: u64,
    pub dropped: u64,
    pub enqueue_rate_per_sec: f64,
    pub dequeue_rate_per_sec: f64,
    pub enqueue_wait_p50_us: u64,
    pub enqueue_wait_p99_us: u64,
    pub dequeue_wait_p50_us: u64,
    pub dequeue_wait_p99_us: u64,
}

/// Global queue registry
pub fn global_queue_registry() -> &'static QueueRegistry {
    static REGISTRY: std::sync::OnceLock<QueueRegistry> = std::sync::OnceLock::new();
    REGISTRY.get_or_init(QueueRegistry::new)
}

/// Helper trait for instrumenting tokio channels
pub trait InstrumentedSender<T> {
    fn send_instrumented(
        &self,
        name: &str,
        item: T,
    ) -> impl std::future::Future<Output = Result<(), T>>;
}

/// RAII guard for tracking queue wait time
pub struct QueueWaitGuard {
    queue_name: String,
    start: Instant,
    is_enqueue: bool,
}

impl QueueWaitGuard {
    pub fn enqueue(queue_name: impl Into<String>) -> Self {
        Self {
            queue_name: queue_name.into(),
            start: Instant::now(),
            is_enqueue: true,
        }
    }

    pub fn dequeue(queue_name: impl Into<String>) -> Self {
        Self {
            queue_name: queue_name.into(),
            start: Instant::now(),
            is_enqueue: false,
        }
    }

    pub fn finish(self, depth: usize) {
        let wait_us = self.start.elapsed().as_micros() as u64;
        let registry = global_queue_registry();
        if self.is_enqueue {
            registry.record_enqueue(&self.queue_name, depth, wait_us);
        } else {
            registry.record_dequeue(&self.queue_name, depth, wait_us);
        }
    }
}

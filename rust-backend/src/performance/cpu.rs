//! CPU Profiling
//!
//! Tracks CPU time, hot paths, and task execution times.
//! Integrates with tracing for flamegraph generation.

use parking_lot::RwLock;
use serde::Serialize;
use std::{
    collections::HashMap,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

/// CPU profiler tracking execution time across components
#[derive(Debug)]
pub struct CpuProfiler {
    /// Total CPU time tracked (microseconds)
    pub total_cpu_us: AtomicU64,
    /// Per-function/span CPU time
    pub spans: RwLock<HashMap<String, SpanCpuMetrics>>,
    /// Hot path detection threshold (microseconds)
    pub hot_path_threshold_us: u64,
    /// Hot paths detected
    pub hot_paths: RwLock<Vec<HotPath>>,
    /// Start time for rate calculations
    start_time: Instant,
}

#[derive(Debug, Clone, Serialize)]
pub struct SpanCpuMetrics {
    pub name: String,
    pub invocations: u64,
    pub total_time_us: u64,
    pub min_time_us: u64,
    pub max_time_us: u64,
    pub last_time_us: u64,
}

impl SpanCpuMetrics {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            invocations: 0,
            total_time_us: 0,
            min_time_us: u64::MAX,
            max_time_us: 0,
            last_time_us: 0,
        }
    }

    pub fn record(&mut self, duration_us: u64) {
        self.invocations += 1;
        self.total_time_us += duration_us;
        self.min_time_us = self.min_time_us.min(duration_us);
        self.max_time_us = self.max_time_us.max(duration_us);
        self.last_time_us = duration_us;
    }

    pub fn mean_time_us(&self) -> f64 {
        if self.invocations == 0 {
            0.0
        } else {
            self.total_time_us as f64 / self.invocations as f64
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct HotPath {
    pub name: String,
    pub total_time_us: u64,
    pub invocations: u64,
    pub percentage_of_total: f64,
    pub detected_at: i64,
}

impl CpuProfiler {
    pub fn new() -> Self {
        Self {
            total_cpu_us: AtomicU64::new(0),
            spans: RwLock::new(HashMap::new()),
            hot_path_threshold_us: 10_000, // 10ms default
            hot_paths: RwLock::new(Vec::new()),
            start_time: Instant::now(),
        }
    }

    /// Record CPU time for a named span
    pub fn record_span(&self, name: &str, duration_us: u64) {
        self.total_cpu_us.fetch_add(duration_us, Ordering::Relaxed);

        let mut spans = self.spans.write();
        spans
            .entry(name.to_string())
            .or_insert_with(|| SpanCpuMetrics::new(name))
            .record(duration_us);

        // Hot path detection
        if duration_us >= self.hot_path_threshold_us {
            drop(spans);
            self.record_hot_path(name, duration_us);
        }
    }

    /// Record a hot path
    fn record_hot_path(&self, name: &str, duration_us: u64) {
        let total = self.total_cpu_us.load(Ordering::Relaxed);
        let percentage = if total > 0 {
            (duration_us as f64 / total as f64) * 100.0
        } else {
            0.0
        };

        let mut hot_paths = self.hot_paths.write();

        // Keep only last 100 hot paths
        if hot_paths.len() >= 100 {
            hot_paths.remove(0);
        }

        hot_paths.push(HotPath {
            name: name.to_string(),
            total_time_us: duration_us,
            invocations: 1,
            percentage_of_total: percentage,
            detected_at: chrono::Utc::now().timestamp(),
        });
    }

    /// Get top N spans by total CPU time
    pub fn top_spans(&self, n: usize) -> Vec<SpanCpuMetrics> {
        let spans = self.spans.read();
        let mut sorted: Vec<_> = spans.values().cloned().collect();
        sorted.sort_by(|a, b| b.total_time_us.cmp(&a.total_time_us));
        sorted.truncate(n);
        sorted
    }

    /// Get CPU utilization estimate
    pub fn cpu_utilization(&self) -> f64 {
        let elapsed_us = self.start_time.elapsed().as_micros() as u64;
        let cpu_us = self.total_cpu_us.load(Ordering::Relaxed);

        if elapsed_us == 0 {
            0.0
        } else {
            (cpu_us as f64 / elapsed_us as f64) * 100.0
        }
    }

    /// Get snapshot of CPU metrics
    pub fn snapshot(&self) -> CpuSnapshot {
        let spans = self.spans.read();
        let hot_paths = self.hot_paths.read();

        CpuSnapshot {
            total_cpu_us: self.total_cpu_us.load(Ordering::Relaxed),
            cpu_utilization_pct: self.cpu_utilization(),
            span_count: spans.len(),
            top_spans: self.top_spans(10),
            hot_paths: hot_paths.clone(),
            uptime_us: self.start_time.elapsed().as_micros() as u64,
        }
    }
}

impl Default for CpuProfiler {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct CpuSnapshot {
    pub total_cpu_us: u64,
    pub cpu_utilization_pct: f64,
    pub span_count: usize,
    pub top_spans: Vec<SpanCpuMetrics>,
    pub hot_paths: Vec<HotPath>,
    pub uptime_us: u64,
}

/// RAII guard for timing a CPU span
pub struct CpuSpanGuard<'a> {
    profiler: &'a CpuProfiler,
    name: String,
    start: Instant,
}

impl<'a> CpuSpanGuard<'a> {
    pub fn new(profiler: &'a CpuProfiler, name: impl Into<String>) -> Self {
        Self {
            profiler,
            name: name.into(),
            start: Instant::now(),
        }
    }
}

impl<'a> Drop for CpuSpanGuard<'a> {
    fn drop(&mut self) {
        let duration_us = self.start.elapsed().as_micros() as u64;
        self.profiler.record_span(&self.name, duration_us);
    }
}

/// Convenience macro for CPU timing
#[macro_export]
macro_rules! cpu_span {
    ($profiler:expr, $name:expr) => {
        $crate::performance::cpu::CpuSpanGuard::new($profiler, $name)
    };
}

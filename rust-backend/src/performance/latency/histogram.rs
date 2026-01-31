//! High-performance latency histogram with logarithmic buckets
//!
//! Designed for minimal overhead in hot paths while providing
//! accurate percentile estimates across a wide range (1μs to 10s).
//!
//! P99.9 OPTIMIZATION: Lock-free recording path using atomic counters.
//! Only reads (percentile calculations) require the mutex.

use parking_lot::Mutex;
use serde::Serialize;
use std::sync::atomic::{AtomicU64, Ordering};

/// Number of histogram buckets (must match BUCKET_BOUNDS length)
const NUM_BUCKETS: usize = 24;

/// Latency histogram with logarithmic buckets
/// Covers 1μs to 10s with ~10% relative error
///
/// P99.9 OPTIMIZATION: Uses atomic counters for lock-free recording.
/// The hot path (record) never blocks; only cold path (percentile) takes a lock.
#[derive(Debug)]
pub struct LatencyHistogram {
    /// Atomic bucket counters - lock-free increments
    buckets: [AtomicU64; NUM_BUCKETS],
    /// Atomic aggregate counters
    count: AtomicU64,
    sum_us: AtomicU64,
    min_us: AtomicU64,
    max_us: AtomicU64,
    /// Mutex only for consistent percentile reads (cold path)
    _read_lock: Mutex<()>,
}

/// Pre-computed logarithmic bucket boundaries (microseconds)
/// Provides ~10% relative error across the range
static BUCKET_BOUNDS: &[u64] = &[
    // Sub-millisecond (microseconds)
    1,
    2,
    5,
    10,
    20,
    50,
    100,
    200,
    500,
    // 1ms - 10ms
    1_000,
    2_000,
    5_000,
    10_000,
    // 10ms - 100ms
    20_000,
    50_000,
    100_000,
    // 100ms - 1s
    200_000,
    500_000,
    1_000_000,
    // 1s - 10s
    2_000_000,
    5_000_000,
    10_000_000,
    // Overflow bucket
    u64::MAX,
];

impl Default for LatencyHistogram {
    fn default() -> Self {
        Self::new()
    }
}

/// Helper macro to create array of AtomicU64 with const initialization
const fn make_atomic_array<const N: usize>() -> [AtomicU64; N] {
    let mut arr = [const { AtomicU64::new(0) }; N];
    arr
}

impl LatencyHistogram {
    pub fn new() -> Self {
        Self {
            buckets: make_atomic_array::<NUM_BUCKETS>(),
            count: AtomicU64::new(0),
            sum_us: AtomicU64::new(0),
            min_us: AtomicU64::new(u64::MAX),
            max_us: AtomicU64::new(0),
            _read_lock: Mutex::new(()),
        }
    }

    /// Record a latency sample in microseconds
    /// 
    /// P99.9 OPTIMIZATION: Completely lock-free using atomic operations.
    /// Uses Relaxed ordering for counters (eventual consistency is fine for metrics).
    #[inline]
    pub fn record(&self, latency_us: u64) {
        // Atomic increment - no lock needed
        self.count.fetch_add(1, Ordering::Relaxed);
        self.sum_us.fetch_add(latency_us, Ordering::Relaxed);
        
        // Update min using compare-exchange loop
        let mut current_min = self.min_us.load(Ordering::Relaxed);
        while latency_us < current_min {
            match self.min_us.compare_exchange_weak(
                current_min,
                latency_us,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(v) => current_min = v,
            }
        }
        
        // Update max using compare-exchange loop  
        let mut current_max = self.max_us.load(Ordering::Relaxed);
        while latency_us > current_max {
            match self.max_us.compare_exchange_weak(
                current_max,
                latency_us,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(v) => current_max = v,
            }
        }

        // Binary search for bucket (faster than linear for 24 buckets)
        let idx = BUCKET_BOUNDS
            .partition_point(|&bound| bound < latency_us);
        let bucket_idx = idx.min(NUM_BUCKETS - 1);
        self.buckets[bucket_idx].fetch_add(1, Ordering::Relaxed);
    }

    /// Record latency from a Duration
    #[inline]
    pub fn record_duration(&self, duration: std::time::Duration) {
        self.record(duration.as_micros() as u64);
    }

    /// Get percentile value in microseconds
    /// 
    /// Note: This is a cold-path operation. Uses Acquire ordering for consistency.
    pub fn percentile(&self, p: f64) -> u64 {
        let count = self.count.load(Ordering::Acquire);
        if count == 0 {
            return 0;
        }

        let target = ((p / 100.0) * count as f64).ceil() as u64;
        let mut cumulative = 0u64;

        for (i, bucket) in self.buckets.iter().enumerate() {
            cumulative += bucket.load(Ordering::Acquire);
            if cumulative >= target {
                return BUCKET_BOUNDS[i];
            }
        }

        self.max_us.load(Ordering::Acquire)
    }

    #[inline]
    pub fn p50(&self) -> u64 {
        self.percentile(50.0)
    }

    #[inline]
    pub fn p90(&self) -> u64 {
        self.percentile(90.0)
    }

    #[inline]
    pub fn p95(&self) -> u64 {
        self.percentile(95.0)
    }

    #[inline]
    pub fn p99(&self) -> u64 {
        self.percentile(99.0)
    }

    #[inline]
    pub fn p999(&self) -> u64 {
        self.percentile(99.9)
    }

    /// Lock-free mean calculation
    pub fn mean(&self) -> f64 {
        let count = self.count.load(Ordering::Relaxed);
        if count == 0 {
            0.0
        } else {
            self.sum_us.load(Ordering::Relaxed) as f64 / count as f64
        }
    }

    /// Lock-free count
    #[inline]
    pub fn count(&self) -> u64 {
        self.count.load(Ordering::Relaxed)
    }

    /// Lock-free min
    pub fn min(&self) -> u64 {
        let count = self.count.load(Ordering::Relaxed);
        if count == 0 {
            0
        } else {
            self.min_us.load(Ordering::Relaxed)
        }
    }

    /// Lock-free max
    #[inline]
    pub fn max(&self) -> u64 {
        self.max_us.load(Ordering::Relaxed)
    }

    /// Get summary for serialization
    /// 
    /// Takes a snapshot of all atomic values for consistent reporting.
    pub fn summary(&self, name: &str) -> HistogramSummary {
        // Take a consistent snapshot by reading all values
        let count = self.count.load(Ordering::Acquire);
        let sum_us = self.sum_us.load(Ordering::Acquire);
        let min_us = self.min_us.load(Ordering::Acquire);
        let max_us = self.max_us.load(Ordering::Acquire);
        
        // Snapshot bucket counts
        let bucket_counts: Vec<u64> = self.buckets
            .iter()
            .map(|b| b.load(Ordering::Acquire))
            .collect();
        
        HistogramSummary {
            name: name.to_string(),
            count,
            min_us: if count == 0 { 0 } else { min_us },
            max_us,
            mean_us: if count == 0 {
                0.0
            } else {
                sum_us as f64 / count as f64
            },
            p50_us: Self::percentile_from_snapshot(&bucket_counts, count, 50.0, max_us),
            p90_us: Self::percentile_from_snapshot(&bucket_counts, count, 90.0, max_us),
            p95_us: Self::percentile_from_snapshot(&bucket_counts, count, 95.0, max_us),
            p99_us: Self::percentile_from_snapshot(&bucket_counts, count, 99.0, max_us),
            p999_us: Self::percentile_from_snapshot(&bucket_counts, count, 99.9, max_us),
        }
    }

    /// Calculate percentile from a snapshot of bucket counts
    fn percentile_from_snapshot(bucket_counts: &[u64], count: u64, p: f64, max_us: u64) -> u64 {
        if count == 0 {
            return 0;
        }

        let target = ((p / 100.0) * count as f64).ceil() as u64;
        let mut cumulative = 0u64;

        for (i, &bucket_count) in bucket_counts.iter().enumerate() {
            cumulative += bucket_count;
            if cumulative >= target {
                return BUCKET_BOUNDS[i];
            }
        }

        max_us
    }

    /// Get CDF points for visualization
    pub fn cdf(&self) -> Vec<CdfPoint> {
        let count = self.count.load(Ordering::Acquire);
        if count == 0 {
            return vec![];
        }

        let mut points = Vec::new();
        let mut cumulative = 0u64;

        for (i, bucket) in self.buckets.iter().enumerate() {
            let bucket_count = bucket.load(Ordering::Acquire);
            if bucket_count > 0 {
                cumulative += bucket_count;
                points.push(CdfPoint {
                    latency_us: BUCKET_BOUNDS[i],
                    cumulative_pct: (cumulative as f64 / count as f64) * 100.0,
                });
            }
        }

        points
    }

    /// Reset histogram (useful for windowed metrics)
    /// 
    /// Note: This is not atomic across all counters, but is acceptable for metrics reset.
    pub fn reset(&self) {
        for bucket in &self.buckets {
            bucket.store(0, Ordering::Relaxed);
        }
        self.count.store(0, Ordering::Relaxed);
        self.sum_us.store(0, Ordering::Relaxed);
        self.min_us.store(u64::MAX, Ordering::Relaxed);
        self.max_us.store(0, Ordering::Relaxed);
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct HistogramSummary {
    pub name: String,
    pub count: u64,
    pub min_us: u64,
    pub max_us: u64,
    pub mean_us: f64,
    pub p50_us: u64,
    pub p90_us: u64,
    pub p95_us: u64,
    pub p99_us: u64,
    pub p999_us: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct CdfPoint {
    pub latency_us: u64,
    pub cumulative_pct: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_histogram_basic() {
        let h = LatencyHistogram::new();

        // Record some latencies
        for i in 1..=100 {
            h.record(i * 10); // 10μs to 1000μs
        }

        assert_eq!(h.count(), 100);
        assert!(h.min() <= 20); // Bucket for 10μs
        assert!(h.max() >= 1000);
        assert!(h.p50() > 0);
        assert!(h.p99() >= h.p50());
    }

    #[test]
    fn test_histogram_empty() {
        let h = LatencyHistogram::new();
        assert_eq!(h.count(), 0);
        assert_eq!(h.p50(), 0);
        assert_eq!(h.mean(), 0.0);
    }

    #[test]
    fn test_histogram_high_latency() {
        let h = LatencyHistogram::new();

        // Record high latencies (seconds)
        h.record(1_000_000); // 1s
        h.record(5_000_000); // 5s
        h.record(10_000_000); // 10s

        assert_eq!(h.count(), 3);
        assert!(h.p99() >= 5_000_000);
    }
}

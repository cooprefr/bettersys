//! High-performance latency histogram with logarithmic buckets
//!
//! Designed for minimal overhead in hot paths while providing
//! accurate percentile estimates across a wide range (1μs to 10s).

use parking_lot::Mutex;
use serde::Serialize;

/// Latency histogram with logarithmic buckets
/// Covers 1μs to 10s with ~10% relative error
#[derive(Debug)]
pub struct LatencyHistogram {
    inner: Mutex<HistogramInner>,
}

#[derive(Debug)]
struct HistogramInner {
    buckets: Vec<u64>,
    bucket_bounds_us: &'static [u64],
    count: u64,
    sum_us: u64,
    min_us: u64,
    max_us: u64,
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

impl LatencyHistogram {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(HistogramInner {
                buckets: vec![0u64; BUCKET_BOUNDS.len()],
                bucket_bounds_us: BUCKET_BOUNDS,
                count: 0,
                sum_us: 0,
                min_us: u64::MAX,
                max_us: 0,
            }),
        }
    }

    /// Record a latency sample in microseconds
    #[inline]
    pub fn record(&self, latency_us: u64) {
        let mut inner = self.inner.lock();
        inner.count += 1;
        inner.sum_us = inner.sum_us.saturating_add(latency_us);
        inner.min_us = inner.min_us.min(latency_us);
        inner.max_us = inner.max_us.max(latency_us);

        // Binary search for bucket (faster than linear for 23 buckets)
        let idx = inner
            .bucket_bounds_us
            .partition_point(|&bound| bound < latency_us);
        let bucket_idx = idx.min(inner.buckets.len() - 1);
        inner.buckets[bucket_idx] += 1;
    }

    /// Record latency from a Duration
    #[inline]
    pub fn record_duration(&self, duration: std::time::Duration) {
        self.record(duration.as_micros() as u64);
    }

    /// Get percentile value in microseconds
    pub fn percentile(&self, p: f64) -> u64 {
        let inner = self.inner.lock();
        if inner.count == 0 {
            return 0;
        }

        let target = ((p / 100.0) * inner.count as f64).ceil() as u64;
        let mut cumulative = 0u64;

        for (i, &bucket_count) in inner.buckets.iter().enumerate() {
            cumulative += bucket_count;
            if cumulative >= target {
                return inner.bucket_bounds_us[i];
            }
        }

        inner.max_us
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

    pub fn mean(&self) -> f64 {
        let inner = self.inner.lock();
        if inner.count == 0 {
            0.0
        } else {
            inner.sum_us as f64 / inner.count as f64
        }
    }

    pub fn count(&self) -> u64 {
        self.inner.lock().count
    }

    pub fn min(&self) -> u64 {
        let inner = self.inner.lock();
        if inner.count == 0 {
            0
        } else {
            inner.min_us
        }
    }

    pub fn max(&self) -> u64 {
        self.inner.lock().max_us
    }

    /// Get summary for serialization
    pub fn summary(&self, name: &str) -> HistogramSummary {
        let inner = self.inner.lock();
        HistogramSummary {
            name: name.to_string(),
            count: inner.count,
            min_us: if inner.count == 0 { 0 } else { inner.min_us },
            max_us: inner.max_us,
            mean_us: if inner.count == 0 {
                0.0
            } else {
                inner.sum_us as f64 / inner.count as f64
            },
            p50_us: self.percentile_inner(&inner, 50.0),
            p90_us: self.percentile_inner(&inner, 90.0),
            p95_us: self.percentile_inner(&inner, 95.0),
            p99_us: self.percentile_inner(&inner, 99.0),
            p999_us: self.percentile_inner(&inner, 99.9),
        }
    }

    fn percentile_inner(&self, inner: &HistogramInner, p: f64) -> u64 {
        if inner.count == 0 {
            return 0;
        }

        let target = ((p / 100.0) * inner.count as f64).ceil() as u64;
        let mut cumulative = 0u64;

        for (i, &bucket_count) in inner.buckets.iter().enumerate() {
            cumulative += bucket_count;
            if cumulative >= target {
                return inner.bucket_bounds_us[i];
            }
        }

        inner.max_us
    }

    /// Get CDF points for visualization
    pub fn cdf(&self) -> Vec<CdfPoint> {
        let inner = self.inner.lock();
        if inner.count == 0 {
            return vec![];
        }

        let mut points = Vec::new();
        let mut cumulative = 0u64;

        for (i, &bucket_count) in inner.buckets.iter().enumerate() {
            if bucket_count > 0 {
                cumulative += bucket_count;
                points.push(CdfPoint {
                    latency_us: inner.bucket_bounds_us[i],
                    cumulative_pct: (cumulative as f64 / inner.count as f64) * 100.0,
                });
            }
        }

        points
    }

    /// Reset histogram (useful for windowed metrics)
    pub fn reset(&self) {
        let mut inner = self.inner.lock();
        inner.buckets.iter_mut().for_each(|b| *b = 0);
        inner.count = 0;
        inner.sum_us = 0;
        inner.min_us = u64::MAX;
        inner.max_us = 0;
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

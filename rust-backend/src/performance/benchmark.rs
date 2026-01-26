//! Benchmark Harness
//!
//! Tools for benchmarking the data ingestion pipeline
//! and trading engine components.

use serde::Serialize;
use std::time::{Duration, Instant};

/// Result of a benchmark run
#[derive(Debug, Clone, Serialize)]
pub struct BenchmarkResult {
    pub name: String,
    pub iterations: u64,
    pub total_time_ns: u64,
    pub min_ns: u64,
    pub max_ns: u64,
    pub mean_ns: f64,
    pub median_ns: u64,
    pub stddev_ns: f64,
    pub p50_ns: u64,
    pub p95_ns: u64,
    pub p99_ns: u64,
    pub throughput_ops_per_sec: f64,
}

impl BenchmarkResult {
    pub fn display(&self) -> String {
        format!(
            "{}: {} iterations in {:.2}ms\n\
             - Mean: {:.2}μs, Median: {:.2}μs\n\
             - Min: {:.2}μs, Max: {:.2}μs\n\
             - p50: {:.2}μs, p95: {:.2}μs, p99: {:.2}μs\n\
             - Throughput: {:.2} ops/sec",
            self.name,
            self.iterations,
            self.total_time_ns as f64 / 1_000_000.0,
            self.mean_ns as f64 / 1000.0,
            self.median_ns as f64 / 1000.0,
            self.min_ns as f64 / 1000.0,
            self.max_ns as f64 / 1000.0,
            self.p50_ns as f64 / 1000.0,
            self.p95_ns as f64 / 1000.0,
            self.p99_ns as f64 / 1000.0,
            self.throughput_ops_per_sec,
        )
    }
}

/// Benchmark runner
pub struct Benchmark {
    name: String,
    warmup_iterations: u64,
    iterations: u64,
    samples: Vec<u64>,
}

impl Benchmark {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            warmup_iterations: 100,
            iterations: 1000,
            samples: Vec::new(),
        }
    }

    pub fn warmup(mut self, iterations: u64) -> Self {
        self.warmup_iterations = iterations;
        self
    }

    pub fn iterations(mut self, iterations: u64) -> Self {
        self.iterations = iterations;
        self
    }

    /// Run the benchmark with a closure
    pub fn run<F: FnMut()>(mut self, mut f: F) -> BenchmarkResult {
        self.samples.clear();
        self.samples.reserve(self.iterations as usize);

        // Warmup phase (not measured)
        for _ in 0..self.warmup_iterations {
            f();
        }

        // Measurement phase
        let total_start = Instant::now();
        for _ in 0..self.iterations {
            let start = Instant::now();
            f();
            let elapsed = start.elapsed().as_nanos() as u64;
            self.samples.push(elapsed);
        }
        let total_time = total_start.elapsed().as_nanos() as u64;

        self.compute_result(total_time)
    }

    /// Run async benchmark
    pub async fn run_async<F, Fut>(mut self, mut f: F) -> BenchmarkResult
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = ()>,
    {
        self.samples.clear();
        self.samples.reserve(self.iterations as usize);

        // Warmup
        for _ in 0..self.warmup_iterations {
            f().await;
        }

        // Measurement
        let total_start = Instant::now();
        for _ in 0..self.iterations {
            let start = Instant::now();
            f().await;
            let elapsed = start.elapsed().as_nanos() as u64;
            self.samples.push(elapsed);
        }
        let total_time = total_start.elapsed().as_nanos() as u64;

        self.compute_result(total_time)
    }

    fn compute_result(&mut self, total_time_ns: u64) -> BenchmarkResult {
        self.samples.sort_unstable();

        let n = self.samples.len();
        let min_ns = *self.samples.first().unwrap_or(&0);
        let max_ns = *self.samples.last().unwrap_or(&0);
        let sum: u64 = self.samples.iter().sum();
        let mean_ns = sum as f64 / n as f64;

        let median_ns = if n % 2 == 0 {
            (self.samples[n / 2 - 1] + self.samples[n / 2]) / 2
        } else {
            self.samples[n / 2]
        };

        let variance: f64 = self
            .samples
            .iter()
            .map(|&x| {
                let diff = x as f64 - mean_ns;
                diff * diff
            })
            .sum::<f64>()
            / n as f64;
        let stddev_ns = variance.sqrt();

        let percentile = |p: f64| -> u64 {
            let idx = ((p / 100.0) * (n as f64 - 1.0)).round() as usize;
            self.samples[idx.min(n - 1)]
        };

        let throughput = if total_time_ns > 0 {
            (self.iterations as f64 / total_time_ns as f64) * 1_000_000_000.0
        } else {
            0.0
        };

        BenchmarkResult {
            name: self.name.clone(),
            iterations: self.iterations,
            total_time_ns,
            min_ns,
            max_ns,
            mean_ns,
            median_ns,
            stddev_ns,
            p50_ns: percentile(50.0),
            p95_ns: percentile(95.0),
            p99_ns: percentile(99.0),
            throughput_ops_per_sec: throughput,
        }
    }
}

/// Benchmark suite for multiple benchmarks
#[derive(Debug, Default)]
pub struct BenchmarkSuite {
    pub name: String,
    pub results: Vec<BenchmarkResult>,
}

impl BenchmarkSuite {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            results: Vec::new(),
        }
    }

    pub fn add(&mut self, result: BenchmarkResult) {
        self.results.push(result);
    }

    pub fn display(&self) -> String {
        let mut output = format!("=== {} ===\n\n", self.name);
        for result in &self.results {
            output.push_str(&result.display());
            output.push_str("\n\n");
        }
        output
    }
}

/// Pre-built benchmarks for core components
pub mod prebuilt {
    use super::*;

    /// Benchmark histogram recording
    pub fn histogram_recording() -> BenchmarkResult {
        use crate::latency::LatencyHistogram;
        let histogram = LatencyHistogram::new();

        Benchmark::new("histogram_record")
            .warmup(1000)
            .iterations(100_000)
            .run(|| {
                histogram.record(1000);
            })
    }

    /// Benchmark JSON parsing (signal-like payload)
    pub fn json_parsing() -> BenchmarkResult {
        let json = r#"{"id":"abc123","market_slug":"btc-price","confidence":0.85,"price":0.65}"#;

        Benchmark::new("json_parse_signal")
            .warmup(1000)
            .iterations(10_000)
            .run(|| {
                let _: serde_json::Value = serde_json::from_str(json).unwrap();
            })
    }

    /// Benchmark UUID generation
    pub fn uuid_generation() -> BenchmarkResult {
        Benchmark::new("uuid_v4_generation")
            .warmup(1000)
            .iterations(100_000)
            .run(|| {
                let _ = uuid::Uuid::new_v4();
            })
    }

    /// Benchmark timestamp generation
    pub fn timestamp_generation() -> BenchmarkResult {
        Benchmark::new("chrono_utc_now")
            .warmup(1000)
            .iterations(100_000)
            .run(|| {
                let _ = chrono::Utc::now().timestamp();
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_benchmark_basic() {
        let result = Benchmark::new("test_add")
            .warmup(10)
            .iterations(100)
            .run(|| {
                let _ = 1 + 1;
            });

        assert_eq!(result.iterations, 100);
        assert!(result.mean_ns > 0.0);
        assert!(result.throughput_ops_per_sec > 0.0);
    }
}

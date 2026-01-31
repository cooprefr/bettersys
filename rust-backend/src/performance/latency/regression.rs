//! Latency Regression Detection and Statistical Validation
//!
//! Provides tools for detecting p99.9 regressions with statistical confidence:
//! - Confidence interval calculation using bootstrap resampling
//! - A/B test comparison using Kolmogorov-Smirnov test
//! - Regression guardrails for CI/CD pipelines

use serde::Serialize;
use std::collections::VecDeque;

/// Configuration for regression detection
#[derive(Debug, Clone)]
pub struct RegressionConfig {
    /// Percentage threshold for p99 regression (default: 10%)
    pub p99_threshold_pct: f64,
    /// Percentage threshold for p99.9 regression (default: 20%)
    pub p999_threshold_pct: f64,
    /// Minimum samples required for statistical significance
    pub min_samples: usize,
    /// Confidence level for intervals (default: 0.95)
    pub confidence_level: f64,
    /// Number of bootstrap iterations
    pub bootstrap_iterations: usize,
}

impl Default for RegressionConfig {
    fn default() -> Self {
        Self {
            p99_threshold_pct: 10.0,
            p999_threshold_pct: 20.0,
            min_samples: 1000,
            confidence_level: 0.95,
            bootstrap_iterations: 10_000,
        }
    }
}

/// Result of a regression check
#[derive(Debug, Clone, Serialize)]
pub struct RegressionResult {
    pub is_regression: bool,
    pub p99_baseline_us: u64,
    pub p99_current_us: u64,
    pub p99_delta_pct: f64,
    pub p999_baseline_us: u64,
    pub p999_current_us: u64,
    pub p999_delta_pct: f64,
    pub sample_count_baseline: usize,
    pub sample_count_current: usize,
    pub confidence_interval: Option<ConfidenceInterval>,
    pub verdict: String,
}

/// Confidence interval for a percentile
#[derive(Debug, Clone, Serialize)]
pub struct ConfidenceInterval {
    pub percentile: f64,
    pub lower_bound_us: u64,
    pub upper_bound_us: u64,
    pub point_estimate_us: u64,
    pub confidence_level: f64,
}

/// Latency sample collector for regression analysis
#[derive(Debug)]
pub struct LatencySampleCollector {
    samples: VecDeque<u64>,
    max_samples: usize,
}

impl LatencySampleCollector {
    pub fn new(max_samples: usize) -> Self {
        Self {
            samples: VecDeque::with_capacity(max_samples),
            max_samples,
        }
    }

    /// Add a latency sample (microseconds)
    #[inline]
    pub fn record(&mut self, latency_us: u64) {
        if self.samples.len() >= self.max_samples {
            self.samples.pop_front();
        }
        self.samples.push_back(latency_us);
    }

    /// Get current sample count
    pub fn len(&self) -> usize {
        self.samples.len()
    }

    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    /// Calculate percentile from samples
    pub fn percentile(&self, p: f64) -> Option<u64> {
        if self.samples.is_empty() {
            return None;
        }
        let mut sorted: Vec<u64> = self.samples.iter().copied().collect();
        sorted.sort_unstable();
        let idx = ((p / 100.0) * sorted.len() as f64).ceil() as usize;
        sorted.get(idx.saturating_sub(1).min(sorted.len() - 1)).copied()
    }

    /// Calculate confidence interval using bootstrap resampling
    pub fn bootstrap_ci(&self, percentile: f64, confidence: f64, iterations: usize) -> Option<ConfidenceInterval> {
        if self.samples.len() < 100 {
            return None;
        }

        use rand::prelude::*;
        let mut rng = rand::thread_rng();
        let samples: Vec<u64> = self.samples.iter().copied().collect();
        let n = samples.len();

        // Bootstrap resampling
        let mut bootstrap_percentiles: Vec<u64> = Vec::with_capacity(iterations);
        for _ in 0..iterations {
            // Resample with replacement
            let mut resampled: Vec<u64> = (0..n)
                .map(|_| samples[rng.gen_range(0..n)])
                .collect();
            resampled.sort_unstable();
            
            let idx = ((percentile / 100.0) * n as f64).ceil() as usize;
            if let Some(&p) = resampled.get(idx.saturating_sub(1).min(n - 1)) {
                bootstrap_percentiles.push(p);
            }
        }

        if bootstrap_percentiles.is_empty() {
            return None;
        }

        bootstrap_percentiles.sort_unstable();
        
        let alpha = 1.0 - confidence;
        let lower_idx = ((alpha / 2.0) * bootstrap_percentiles.len() as f64) as usize;
        let upper_idx = ((1.0 - alpha / 2.0) * bootstrap_percentiles.len() as f64) as usize;

        Some(ConfidenceInterval {
            percentile,
            lower_bound_us: bootstrap_percentiles[lower_idx],
            upper_bound_us: bootstrap_percentiles[upper_idx.min(bootstrap_percentiles.len() - 1)],
            point_estimate_us: self.percentile(percentile).unwrap_or(0),
            confidence_level: confidence,
        })
    }

    /// Take a snapshot of samples for comparison
    pub fn snapshot(&self) -> Vec<u64> {
        self.samples.iter().copied().collect()
    }

    /// Clear all samples
    pub fn clear(&mut self) {
        self.samples.clear();
    }
}

/// Compare baseline vs current latency distributions
pub fn check_regression(
    baseline: &LatencySampleCollector,
    current: &LatencySampleCollector,
    config: &RegressionConfig,
) -> RegressionResult {
    let p99_baseline = baseline.percentile(99.0).unwrap_or(0);
    let p99_current = current.percentile(99.0).unwrap_or(0);
    let p999_baseline = baseline.percentile(99.9).unwrap_or(0);
    let p999_current = current.percentile(99.9).unwrap_or(0);

    let p99_delta_pct = if p99_baseline > 0 {
        ((p99_current as f64 - p99_baseline as f64) / p99_baseline as f64) * 100.0
    } else {
        0.0
    };

    let p999_delta_pct = if p999_baseline > 0 {
        ((p999_current as f64 - p999_baseline as f64) / p999_baseline as f64) * 100.0
    } else {
        0.0
    };

    let is_p99_regression = p99_delta_pct > config.p99_threshold_pct;
    let is_p999_regression = p999_delta_pct > config.p999_threshold_pct;
    let is_regression = is_p99_regression || is_p999_regression;

    // Calculate confidence interval for current p99.9
    let confidence_interval = if current.len() >= config.min_samples {
        current.bootstrap_ci(99.9, config.confidence_level, config.bootstrap_iterations)
    } else {
        None
    };

    let verdict = if current.len() < config.min_samples || baseline.len() < config.min_samples {
        format!(
            "INSUFFICIENT_DATA: need {} samples, have baseline={} current={}",
            config.min_samples,
            baseline.len(),
            current.len()
        )
    } else if is_regression {
        let mut reasons = Vec::new();
        if is_p99_regression {
            reasons.push(format!("p99 +{:.1}% > {:.1}% threshold", p99_delta_pct, config.p99_threshold_pct));
        }
        if is_p999_regression {
            reasons.push(format!("p99.9 +{:.1}% > {:.1}% threshold", p999_delta_pct, config.p999_threshold_pct));
        }
        format!("REGRESSION: {}", reasons.join(", "))
    } else {
        "OK: no regression detected".to_string()
    };

    RegressionResult {
        is_regression,
        p99_baseline_us: p99_baseline,
        p99_current_us: p99_current,
        p99_delta_pct,
        p999_baseline_us: p999_baseline,
        p999_current_us: p999_current,
        p999_delta_pct,
        sample_count_baseline: baseline.len(),
        sample_count_current: current.len(),
        confidence_interval,
        verdict,
    }
}

/// Two-sample Kolmogorov-Smirnov test statistic
/// Returns (D statistic, approximate p-value)
pub fn ks_test(sample_a: &[u64], sample_b: &[u64]) -> (f64, f64) {
    if sample_a.is_empty() || sample_b.is_empty() {
        return (0.0, 1.0);
    }

    let mut a_sorted: Vec<u64> = sample_a.to_vec();
    let mut b_sorted: Vec<u64> = sample_b.to_vec();
    a_sorted.sort_unstable();
    b_sorted.sort_unstable();

    let n_a = a_sorted.len() as f64;
    let n_b = b_sorted.len() as f64;

    // Compute maximum difference between CDFs
    let mut d_max: f64 = 0.0;
    let mut i = 0usize;
    let mut j = 0usize;

    while i < a_sorted.len() && j < b_sorted.len() {
        let cdf_a = (i + 1) as f64 / n_a;
        let cdf_b = (j + 1) as f64 / n_b;
        
        if a_sorted[i] <= b_sorted[j] {
            d_max = d_max.max((cdf_a - j as f64 / n_b).abs());
            i += 1;
        } else {
            d_max = d_max.max((i as f64 / n_a - cdf_b).abs());
            j += 1;
        }
    }

    // Handle remaining elements
    while i < a_sorted.len() {
        let cdf_a = (i + 1) as f64 / n_a;
        d_max = d_max.max((cdf_a - 1.0).abs());
        i += 1;
    }
    while j < b_sorted.len() {
        let cdf_b = (j + 1) as f64 / n_b;
        d_max = d_max.max((1.0 - cdf_b).abs());
        j += 1;
    }

    // Approximate p-value using asymptotic distribution
    let n_eff = (n_a * n_b) / (n_a + n_b);
    let lambda = (n_eff.sqrt() + 0.12 + 0.11 / n_eff.sqrt()) * d_max;
    
    // Kolmogorov distribution approximation
    let p_value = (-2.0 * lambda * lambda).exp() * 2.0;
    let p_value = p_value.max(0.0).min(1.0);

    (d_max, p_value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sample_collector() {
        let mut collector = LatencySampleCollector::new(1000);
        for i in 1..=100 {
            collector.record(i * 10);
        }
        assert_eq!(collector.len(), 100);
        assert!(collector.percentile(50.0).unwrap() > 0);
        assert!(collector.percentile(99.0).unwrap() >= collector.percentile(50.0).unwrap());
    }

    #[test]
    fn test_regression_check_no_regression() {
        let mut baseline = LatencySampleCollector::new(10000);
        let mut current = LatencySampleCollector::new(10000);

        // Similar distributions
        for i in 0..1000 {
            baseline.record(100 + (i % 50));
            current.record(100 + (i % 55)); // Slightly higher but within threshold
        }

        let config = RegressionConfig::default();
        let result = check_regression(&baseline, &current, &config);
        assert!(!result.is_regression);
    }

    #[test]
    fn test_regression_check_with_regression() {
        let mut baseline = LatencySampleCollector::new(10000);
        let mut current = LatencySampleCollector::new(10000);

        // Baseline: low latency
        for _ in 0..1000 {
            baseline.record(100);
        }

        // Current: significantly higher p99
        for i in 0..1000 {
            if i < 990 {
                current.record(100);
            } else {
                current.record(500); // 5x spike at p99
            }
        }

        let config = RegressionConfig::default();
        let result = check_regression(&baseline, &current, &config);
        // p99 went from 100 to 500 = 400% increase, should be regression
        assert!(result.is_regression || result.p99_delta_pct > 100.0);
    }

    #[test]
    fn test_ks_test_identical() {
        let a: Vec<u64> = (1..=100).collect();
        let b: Vec<u64> = (1..=100).collect();
        let (d, p) = ks_test(&a, &b);
        assert!(d < 0.1);
        assert!(p > 0.05);
    }

    #[test]
    fn test_ks_test_different() {
        let a: Vec<u64> = (1..=100).collect();
        let b: Vec<u64> = (500..=600).collect();
        let (d, _p) = ks_test(&a, &b);
        assert!(d > 0.9); // Very different distributions
    }
}

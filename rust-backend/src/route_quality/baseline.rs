//! Baseline Calculation and Anomaly Detection
//!
//! Calculates rolling baselines for RTT, packet loss, and path metrics.
//! Used for detecting anomalies and triggering alerts.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::RwLock;
use tracing::{debug, info};

use super::config::BaselineConfig;
use super::metrics::{BaselineMetrics, RouteQualityMetrics};

/// Sample for baseline calculation
#[derive(Debug, Clone)]
struct Sample {
    value: f64,
    timestamp: Instant,
}

/// Baseline calculator
pub struct BaselineCalculator {
    config: BaselineConfig,
    metrics: Arc<RouteQualityMetrics>,
    
    /// RTT samples per endpoint
    rtt_samples: RwLock<HashMap<String, VecDeque<Sample>>>,
    
    /// Packet loss samples per endpoint
    loss_samples: RwLock<HashMap<String, VecDeque<Sample>>>,
    
    /// Hop count samples per endpoint
    hop_samples: RwLock<HashMap<String, VecDeque<Sample>>>,
    
    /// Last calculation time
    last_calculated: RwLock<Option<Instant>>,
}

impl BaselineCalculator {
    pub fn new(config: BaselineConfig, metrics: Arc<RouteQualityMetrics>) -> Self {
        Self {
            config,
            metrics,
            rtt_samples: RwLock::new(HashMap::new()),
            loss_samples: RwLock::new(HashMap::new()),
            hop_samples: RwLock::new(HashMap::new()),
            last_calculated: RwLock::new(None),
        }
    }
    
    /// Add RTT sample
    pub fn add_rtt_sample(&self, endpoint: &str, rtt_sec: f64) {
        let mut samples = self.rtt_samples.write();
        let queue = samples.entry(endpoint.to_string()).or_insert_with(VecDeque::new);
        
        queue.push_back(Sample {
            value: rtt_sec,
            timestamp: Instant::now(),
        });
        
        // Prune old samples
        self.prune_samples(queue);
    }
    
    /// Add packet loss sample
    pub fn add_loss_sample(&self, endpoint: &str, loss_rate: f64) {
        let mut samples = self.loss_samples.write();
        let queue = samples.entry(endpoint.to_string()).or_insert_with(VecDeque::new);
        
        queue.push_back(Sample {
            value: loss_rate,
            timestamp: Instant::now(),
        });
        
        self.prune_samples(queue);
    }
    
    /// Add hop count sample
    pub fn add_hop_sample(&self, endpoint: &str, hop_count: u32) {
        let mut samples = self.hop_samples.write();
        let queue = samples.entry(endpoint.to_string()).or_insert_with(VecDeque::new);
        
        queue.push_back(Sample {
            value: hop_count as f64,
            timestamp: Instant::now(),
        });
        
        self.prune_samples(queue);
    }
    
    /// Prune samples older than window
    fn prune_samples(&self, samples: &mut VecDeque<Sample>) {
        let cutoff = Instant::now() - self.config.window;
        while let Some(front) = samples.front() {
            if front.timestamp < cutoff {
                samples.pop_front();
            } else {
                break;
            }
        }
    }
    
    /// Calculate baselines if enough time has passed
    pub fn maybe_recalculate(&self) {
        let should_calculate = match *self.last_calculated.read() {
            Some(last) => last.elapsed() >= self.config.recalculate_interval,
            None => true,
        };
        
        if should_calculate {
            self.calculate_baselines();
        }
    }
    
    /// Calculate all baselines
    pub fn calculate_baselines(&self) {
        info!("Calculating baselines");
        
        let mut baseline = BaselineMetrics::default();
        
        // Calculate RTT baselines
        for (endpoint, samples) in self.rtt_samples.read().iter() {
            if samples.len() < 10 {
                continue;
            }
            
            let values: Vec<f64> = samples.iter().map(|s| s.value).collect();
            let filtered = self.remove_outliers(&values);
            
            if filtered.is_empty() {
                continue;
            }
            
            let p50 = percentile(&filtered, 50.0);
            let p99 = percentile(&filtered, 99.0);
            let stddev = standard_deviation(&filtered);
            
            baseline.rtt_p50.insert(endpoint.clone(), p50);
            baseline.rtt_p99.insert(endpoint.clone(), p99);
            baseline.rtt_stddev.insert(endpoint.clone(), stddev);
            
            debug!("Baseline RTT for {}: p50={:.4}s p99={:.4}s stddev={:.4}s",
                endpoint, p50, p99, stddev);
        }
        
        // Calculate packet loss baselines
        for (endpoint, samples) in self.loss_samples.read().iter() {
            if samples.len() < 10 {
                continue;
            }
            
            let values: Vec<f64> = samples.iter().map(|s| s.value).collect();
            let avg = values.iter().sum::<f64>() / values.len() as f64;
            
            baseline.packet_loss_rate.insert(endpoint.clone(), avg);
            
            debug!("Baseline packet loss for {}: {:.4}%", endpoint, avg * 100.0);
        }
        
        // Calculate hop count baselines
        for (endpoint, samples) in self.hop_samples.read().iter() {
            if samples.len() < 5 {
                continue;
            }
            
            let values: Vec<f64> = samples.iter().map(|s| s.value).collect();
            let mode = mode_u32(&values);
            
            baseline.typical_hop_count.insert(endpoint.clone(), mode);
            
            debug!("Baseline hop count for {}: {}", endpoint, mode);
        }
        
        baseline.last_calculated = Some(Instant::now());
        
        // Update metrics with new baseline
        *self.metrics.baseline.write() = baseline;
        *self.last_calculated.write() = Some(Instant::now());
    }
    
    /// Remove outliers using sigma threshold
    fn remove_outliers(&self, values: &[f64]) -> Vec<f64> {
        if values.len() < 3 {
            return values.to_vec();
        }
        
        let mean = values.iter().sum::<f64>() / values.len() as f64;
        let stddev = standard_deviation(values);
        
        if stddev < 1e-10 {
            return values.to_vec();
        }
        
        let sigma = self.config.outlier_sigma;
        values.iter()
            .copied()
            .filter(|&v| ((v - mean) / stddev).abs() <= sigma)
            .collect()
    }
    
    /// Check if a value is anomalous compared to baseline
    pub fn is_anomalous(&self, endpoint: &str, metric: &str, value: f64, sigma_threshold: f64) -> bool {
        let baseline = self.metrics.baseline.read();
        
        let (mean, stddev) = match metric {
            "rtt" => {
                let p50 = baseline.rtt_p50.get(endpoint).copied().unwrap_or(value);
                let sd = baseline.rtt_stddev.get(endpoint).copied().unwrap_or(0.0);
                (p50, sd)
            }
            "loss" => {
                let rate = baseline.packet_loss_rate.get(endpoint).copied().unwrap_or(value);
                (rate, rate * 0.5) // Use 50% of mean as pseudo-stddev for loss
            }
            _ => return false,
        };
        
        if stddev < 1e-10 {
            return false;
        }
        
        ((value - mean) / stddev).abs() > sigma_threshold
    }
    
    /// Get baseline for endpoint
    pub fn get_baseline(&self, endpoint: &str) -> Option<EndpointBaseline> {
        let baseline = self.metrics.baseline.read();
        
        Some(EndpointBaseline {
            rtt_p50: baseline.rtt_p50.get(endpoint).copied()?,
            rtt_p99: baseline.rtt_p99.get(endpoint).copied()?,
            rtt_stddev: baseline.rtt_stddev.get(endpoint).copied()?,
            packet_loss_rate: baseline.packet_loss_rate.get(endpoint).copied().unwrap_or(0.0),
            typical_hop_count: baseline.typical_hop_count.get(endpoint).copied().unwrap_or(0),
        })
    }
}

/// Baseline metrics for a single endpoint
#[derive(Debug, Clone)]
pub struct EndpointBaseline {
    pub rtt_p50: f64,
    pub rtt_p99: f64,
    pub rtt_stddev: f64,
    pub packet_loss_rate: f64,
    pub typical_hop_count: u32,
}

/// Calculate percentile of sorted values
fn percentile(values: &[f64], p: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    
    let mut sorted: Vec<f64> = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    
    let idx = ((p / 100.0) * (sorted.len() - 1) as f64) as usize;
    sorted[idx.min(sorted.len() - 1)]
}

/// Calculate standard deviation
fn standard_deviation(values: &[f64]) -> f64 {
    if values.len() < 2 {
        return 0.0;
    }
    
    let mean = values.iter().sum::<f64>() / values.len() as f64;
    let variance = values.iter()
        .map(|&x| {
            let diff = x - mean;
            diff * diff
        })
        .sum::<f64>() / (values.len() - 1) as f64;
    
    variance.sqrt()
}

/// Calculate mode (most common value) for hop counts
fn mode_u32(values: &[f64]) -> u32 {
    if values.is_empty() {
        return 0;
    }
    
    let mut counts: HashMap<u32, usize> = HashMap::new();
    for &v in values {
        *counts.entry(v as u32).or_default() += 1;
    }
    
    counts.into_iter()
        .max_by_key(|&(_, count)| count)
        .map(|(value, _)| value)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_percentile() {
        let values = vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0];
        assert!((percentile(&values, 50.0) - 5.0).abs() < 0.5);
        assert!((percentile(&values, 99.0) - 10.0).abs() < 0.5);
    }
    
    #[test]
    fn test_standard_deviation() {
        let values = vec![2.0, 4.0, 4.0, 4.0, 5.0, 5.0, 7.0, 9.0];
        let sd = standard_deviation(&values);
        assert!((sd - 2.0).abs() < 0.1);
    }
    
    #[test]
    fn test_mode() {
        let values = vec![1.0, 2.0, 2.0, 3.0, 2.0, 4.0];
        assert_eq!(mode_u32(&values), 2);
    }
}

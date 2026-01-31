//! Prometheus Metrics for Route Quality Monitoring

use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

/// Route quality metrics registry
/// 
/// Exposes metrics in Prometheus format via HTTP endpoint
#[derive(Debug)]
pub struct RouteQualityMetrics {
    /// RTT measurements (endpoint -> probe_type -> histogram)
    pub rtt: RwLock<HashMap<String, LatencyHistogram>>,
    
    /// Packet loss counters
    pub probe_success: RwLock<HashMap<String, AtomicU64>>,
    pub probe_total: RwLock<HashMap<String, AtomicU64>>,
    
    /// TCP connect latency
    pub tcp_connect: RwLock<HashMap<String, LatencyHistogram>>,
    
    /// TLS handshake latency
    pub tls_handshake: RwLock<HashMap<String, LatencyHistogram>>,
    
    /// DNS resolution latency
    pub dns_resolution: RwLock<HashMap<String, LatencyHistogram>>,
    
    /// DNS IP change indicator (endpoint -> changed flag)
    pub dns_ip_changed: RwLock<HashMap<String, bool>>,
    
    /// Hop count (endpoint -> count)
    pub hop_count: RwLock<HashMap<String, u32>>,
    
    /// Path hash for change detection
    pub path_hash: RwLock<HashMap<String, u64>>,
    
    /// Health scores (endpoint -> score 0-100)
    pub health_score: RwLock<HashMap<String, f64>>,
    
    /// Consecutive failures (endpoint -> count)
    pub consecutive_failures: RwLock<HashMap<String, u32>>,
    
    /// Failover counter
    pub failover_total: AtomicU64,
    
    /// Active endpoint
    pub active_endpoint: RwLock<String>,
    
    /// Baseline metrics
    pub baseline: RwLock<BaselineMetrics>,
}

/// Baseline metrics calculated over rolling window
#[derive(Debug, Default, Clone)]
pub struct BaselineMetrics {
    pub rtt_p50: HashMap<String, f64>,
    pub rtt_p99: HashMap<String, f64>,
    pub rtt_stddev: HashMap<String, f64>,
    pub packet_loss_rate: HashMap<String, f64>,
    pub typical_hop_count: HashMap<String, u32>,
    pub last_calculated: Option<Instant>,
}

/// Simple histogram for latency measurements
#[derive(Debug)]
pub struct LatencyHistogram {
    /// Bucket boundaries in microseconds
    buckets: Vec<u64>,
    /// Counts per bucket
    counts: Vec<AtomicU64>,
    /// Sum of all observations (microseconds)
    sum: AtomicU64,
    /// Total count
    count: AtomicU64,
    /// Recent samples for percentile calculation
    recent_samples: RwLock<Vec<u64>>,
    max_recent: usize,
}

impl LatencyHistogram {
    pub fn new() -> Self {
        // Buckets: 100us, 500us, 1ms, 2ms, 5ms, 10ms, 20ms, 50ms, 100ms, 200ms, 500ms, 1s
        let buckets = vec![
            100, 500, 1_000, 2_000, 5_000, 10_000, 20_000, 50_000, 100_000, 200_000, 500_000, 1_000_000,
        ];
        let counts = buckets.iter().map(|_| AtomicU64::new(0)).collect();
        
        Self {
            buckets,
            counts,
            sum: AtomicU64::new(0),
            count: AtomicU64::new(0),
            recent_samples: RwLock::new(Vec::with_capacity(1000)),
            max_recent: 1000,
        }
    }
    
    /// Record a latency observation in microseconds
    pub fn record(&self, value_us: u64) {
        // Update buckets
        for (i, &boundary) in self.buckets.iter().enumerate() {
            if value_us <= boundary {
                self.counts[i].fetch_add(1, Ordering::Relaxed);
                break;
            }
        }
        
        // Update sum and count
        self.sum.fetch_add(value_us, Ordering::Relaxed);
        self.count.fetch_add(1, Ordering::Relaxed);
        
        // Store recent sample
        let mut samples = self.recent_samples.write();
        if samples.len() >= self.max_recent {
            samples.remove(0);
        }
        samples.push(value_us);
    }
    
    /// Record a latency observation in seconds
    pub fn record_seconds(&self, value_sec: f64) {
        self.record((value_sec * 1_000_000.0) as u64);
    }
    
    /// Get mean latency in microseconds
    pub fn mean(&self) -> f64 {
        let count = self.count.load(Ordering::Relaxed);
        if count == 0 {
            return 0.0;
        }
        self.sum.load(Ordering::Relaxed) as f64 / count as f64
    }
    
    /// Get approximate percentile from recent samples
    pub fn percentile(&self, p: f64) -> u64 {
        let samples = self.recent_samples.read();
        if samples.is_empty() {
            return 0;
        }
        
        let mut sorted: Vec<u64> = samples.clone();
        sorted.sort_unstable();
        
        let idx = ((p / 100.0) * (sorted.len() - 1) as f64) as usize;
        sorted[idx.min(sorted.len() - 1)]
    }
    
    pub fn p50(&self) -> u64 {
        self.percentile(50.0)
    }
    
    pub fn p99(&self) -> u64 {
        self.percentile(99.0)
    }
    
    /// Get standard deviation from recent samples
    pub fn stddev(&self) -> f64 {
        let samples = self.recent_samples.read();
        if samples.len() < 2 {
            return 0.0;
        }
        
        let mean = samples.iter().sum::<u64>() as f64 / samples.len() as f64;
        let variance = samples.iter()
            .map(|&x| {
                let diff = x as f64 - mean;
                diff * diff
            })
            .sum::<f64>() / (samples.len() - 1) as f64;
        
        variance.sqrt()
    }
    
    /// Export as Prometheus histogram format
    pub fn to_prometheus(&self, name: &str, labels: &str) -> String {
        let mut output = String::new();
        let mut cumulative = 0u64;
        
        for (i, &boundary) in self.buckets.iter().enumerate() {
            cumulative += self.counts[i].load(Ordering::Relaxed);
            output.push_str(&format!(
                "{}_bucket{{{},le=\"{}\"}} {}\n",
                name, labels, boundary as f64 / 1_000_000.0, cumulative
            ));
        }
        
        // +Inf bucket
        let total = self.count.load(Ordering::Relaxed);
        output.push_str(&format!(
            "{}_bucket{{{},le=\"+Inf\"}} {}\n",
            name, labels, total
        ));
        
        // Sum and count
        output.push_str(&format!(
            "{}_sum{{{}}} {}\n",
            name, labels, self.sum.load(Ordering::Relaxed) as f64 / 1_000_000.0
        ));
        output.push_str(&format!(
            "{}_count{{{}}} {}\n",
            name, labels, total
        ));
        
        output
    }
}

impl Default for LatencyHistogram {
    fn default() -> Self {
        Self::new()
    }
}

impl RouteQualityMetrics {
    pub fn new() -> Self {
        Self {
            rtt: RwLock::new(HashMap::new()),
            probe_success: RwLock::new(HashMap::new()),
            probe_total: RwLock::new(HashMap::new()),
            tcp_connect: RwLock::new(HashMap::new()),
            tls_handshake: RwLock::new(HashMap::new()),
            dns_resolution: RwLock::new(HashMap::new()),
            dns_ip_changed: RwLock::new(HashMap::new()),
            hop_count: RwLock::new(HashMap::new()),
            path_hash: RwLock::new(HashMap::new()),
            health_score: RwLock::new(HashMap::new()),
            consecutive_failures: RwLock::new(HashMap::new()),
            failover_total: AtomicU64::new(0),
            active_endpoint: RwLock::new(String::new()),
            baseline: RwLock::new(BaselineMetrics::default()),
        }
    }
    
    /// Record RTT measurement
    pub fn record_rtt(&self, endpoint: &str, probe_type: &str, rtt_us: u64) {
        let key = format!("{}:{}", endpoint, probe_type);
        let mut rtt_map = self.rtt.write();
        rtt_map.entry(key).or_insert_with(LatencyHistogram::new).record(rtt_us);
    }
    
    /// Record probe result
    pub fn record_probe(&self, endpoint: &str, probe_type: &str, success: bool) {
        let key = format!("{}:{}", endpoint, probe_type);
        
        {
            let mut total = self.probe_total.write();
            total.entry(key.clone())
                .or_insert_with(|| AtomicU64::new(0))
                .fetch_add(1, Ordering::Relaxed);
        }
        
        if success {
            let mut success_map = self.probe_success.write();
            success_map.entry(key)
                .or_insert_with(|| AtomicU64::new(0))
                .fetch_add(1, Ordering::Relaxed);
            
            // Reset consecutive failures
            self.consecutive_failures.write().insert(endpoint.to_string(), 0);
        } else {
            // Increment consecutive failures
            let mut failures = self.consecutive_failures.write();
            let count = failures.entry(endpoint.to_string()).or_insert(0);
            *count += 1;
        }
    }
    
    /// Record TCP connect latency
    pub fn record_tcp_connect(&self, endpoint: &str, latency_us: u64) {
        let mut tcp = self.tcp_connect.write();
        tcp.entry(endpoint.to_string())
            .or_insert_with(LatencyHistogram::new)
            .record(latency_us);
    }
    
    /// Record TLS handshake latency
    pub fn record_tls_handshake(&self, endpoint: &str, latency_us: u64) {
        let mut tls = self.tls_handshake.write();
        tls.entry(endpoint.to_string())
            .or_insert_with(LatencyHistogram::new)
            .record(latency_us);
    }
    
    /// Record DNS resolution
    pub fn record_dns(&self, endpoint: &str, latency_us: u64, ip_changed: bool) {
        {
            let mut dns = self.dns_resolution.write();
            dns.entry(endpoint.to_string())
                .or_insert_with(LatencyHistogram::new)
                .record(latency_us);
        }
        
        if ip_changed {
            self.dns_ip_changed.write().insert(endpoint.to_string(), true);
        }
    }
    
    /// Clear DNS change flag after processing
    pub fn clear_dns_change(&self, endpoint: &str) {
        self.dns_ip_changed.write().insert(endpoint.to_string(), false);
    }
    
    /// Record path information
    pub fn record_path(&self, endpoint: &str, hop_count: u32, path_hash: u64) {
        self.hop_count.write().insert(endpoint.to_string(), hop_count);
        self.path_hash.write().insert(endpoint.to_string(), path_hash);
    }
    
    /// Update health score
    pub fn update_health_score(&self, endpoint: &str, score: f64) {
        self.health_score.write().insert(endpoint.to_string(), score);
    }
    
    /// Record failover event
    pub fn record_failover(&self) {
        self.failover_total.fetch_add(1, Ordering::Relaxed);
    }
    
    /// Set active endpoint
    pub fn set_active_endpoint(&self, endpoint: &str) {
        *self.active_endpoint.write() = endpoint.to_string();
    }
    
    /// Get consecutive failures for endpoint
    pub fn get_consecutive_failures(&self, endpoint: &str) -> u32 {
        *self.consecutive_failures.read().get(endpoint).unwrap_or(&0)
    }
    
    /// Get health score for endpoint
    pub fn get_health_score(&self, endpoint: &str) -> f64 {
        *self.health_score.read().get(endpoint).unwrap_or(&100.0)
    }
    
    /// Calculate packet loss rate
    pub fn packet_loss_rate(&self, endpoint: &str, probe_type: &str) -> f64 {
        let key = format!("{}:{}", endpoint, probe_type);
        
        let total = self.probe_total.read()
            .get(&key)
            .map(|v| v.load(Ordering::Relaxed))
            .unwrap_or(0);
        
        if total == 0 {
            return 0.0;
        }
        
        let success = self.probe_success.read()
            .get(&key)
            .map(|v| v.load(Ordering::Relaxed))
            .unwrap_or(0);
        
        1.0 - (success as f64 / total as f64)
    }
    
    /// Export all metrics in Prometheus format
    pub fn to_prometheus(&self) -> String {
        let mut output = String::new();
        
        // RTT histograms
        output.push_str("# HELP route_quality_rtt_seconds RTT to endpoint\n");
        output.push_str("# TYPE route_quality_rtt_seconds histogram\n");
        for (key, hist) in self.rtt.read().iter() {
            let parts: Vec<&str> = key.split(':').collect();
            if parts.len() == 2 {
                let labels = format!("endpoint=\"{}\",probe_type=\"{}\"", parts[0], parts[1]);
                output.push_str(&hist.to_prometheus("route_quality_rtt_seconds", &labels));
            }
        }
        
        // Packet loss
        output.push_str("\n# HELP route_quality_probe_success_total Successful probes\n");
        output.push_str("# TYPE route_quality_probe_success_total counter\n");
        for (key, count) in self.probe_success.read().iter() {
            let parts: Vec<&str> = key.split(':').collect();
            if parts.len() == 2 {
                output.push_str(&format!(
                    "route_quality_probe_success_total{{endpoint=\"{}\",probe_type=\"{}\"}} {}\n",
                    parts[0], parts[1], count.load(Ordering::Relaxed)
                ));
            }
        }
        
        output.push_str("\n# HELP route_quality_probe_total Total probes\n");
        output.push_str("# TYPE route_quality_probe_total counter\n");
        for (key, count) in self.probe_total.read().iter() {
            let parts: Vec<&str> = key.split(':').collect();
            if parts.len() == 2 {
                output.push_str(&format!(
                    "route_quality_probe_total{{endpoint=\"{}\",probe_type=\"{}\"}} {}\n",
                    parts[0], parts[1], count.load(Ordering::Relaxed)
                ));
            }
        }
        
        // Health scores
        output.push_str("\n# HELP route_quality_health_score Endpoint health score (0-100)\n");
        output.push_str("# TYPE route_quality_health_score gauge\n");
        for (endpoint, score) in self.health_score.read().iter() {
            output.push_str(&format!(
                "route_quality_health_score{{endpoint=\"{}\"}} {:.2}\n",
                endpoint, score
            ));
        }
        
        // Hop count
        output.push_str("\n# HELP route_quality_hop_count Number of hops to endpoint\n");
        output.push_str("# TYPE route_quality_hop_count gauge\n");
        for (endpoint, count) in self.hop_count.read().iter() {
            output.push_str(&format!(
                "route_quality_hop_count{{endpoint=\"{}\"}} {}\n",
                endpoint, count
            ));
        }
        
        // Path hash
        output.push_str("\n# HELP route_quality_path_hash Hash of traceroute path\n");
        output.push_str("# TYPE route_quality_path_hash gauge\n");
        for (endpoint, hash) in self.path_hash.read().iter() {
            output.push_str(&format!(
                "route_quality_path_hash{{endpoint=\"{}\"}} {}\n",
                endpoint, hash
            ));
        }
        
        // DNS IP changed
        output.push_str("\n# HELP route_quality_dns_ip_changed DNS IP changed since last probe\n");
        output.push_str("# TYPE route_quality_dns_ip_changed gauge\n");
        for (endpoint, changed) in self.dns_ip_changed.read().iter() {
            output.push_str(&format!(
                "route_quality_dns_ip_changed{{endpoint=\"{}\"}} {}\n",
                endpoint, if *changed { 1 } else { 0 }
            ));
        }
        
        // Consecutive failures
        output.push_str("\n# HELP route_quality_consecutive_failures Consecutive probe failures\n");
        output.push_str("# TYPE route_quality_consecutive_failures gauge\n");
        for (endpoint, count) in self.consecutive_failures.read().iter() {
            output.push_str(&format!(
                "route_quality_consecutive_failures{{endpoint=\"{}\"}} {}\n",
                endpoint, count
            ));
        }
        
        // Failover total
        output.push_str("\n# HELP route_quality_failover_total Total failover events\n");
        output.push_str("# TYPE route_quality_failover_total counter\n");
        output.push_str(&format!(
            "route_quality_failover_total {}\n",
            self.failover_total.load(Ordering::Relaxed)
        ));
        
        // Active endpoint
        output.push_str("\n# HELP route_quality_active_endpoint Currently active endpoint\n");
        output.push_str("# TYPE route_quality_active_endpoint gauge\n");
        let active = self.active_endpoint.read();
        if !active.is_empty() {
            output.push_str(&format!(
                "route_quality_active_endpoint{{endpoint=\"{}\"}} 1\n",
                active
            ));
        }
        
        output
    }
}

impl Default for RouteQualityMetrics {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_histogram_basic() {
        let h = LatencyHistogram::new();
        h.record(1000);  // 1ms
        h.record(2000);  // 2ms
        h.record(5000);  // 5ms
        
        assert_eq!(h.mean(), 8000.0 / 3.0);
    }
    
    #[test]
    fn test_metrics_prometheus_export() {
        let m = RouteQualityMetrics::new();
        m.record_rtt("binance", "icmp", 5000);
        m.record_probe("binance", "icmp", true);
        m.update_health_score("binance", 95.0);
        
        let output = m.to_prometheus();
        assert!(output.contains("route_quality_rtt_seconds"));
        assert!(output.contains("route_quality_health_score"));
    }
}

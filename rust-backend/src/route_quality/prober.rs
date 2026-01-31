//! Active Probing Implementation
//!
//! Performs continuous probing of endpoints using multiple methods:
//! - ICMP ping (via system command)
//! - TCP connect
//! - TLS handshake (via openssl command for portability)
//! - DNS resolution
//! - Traceroute (via external command)

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::net::TcpStream;
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio::time::{interval, timeout};
use tracing::{debug, error, info, warn};

use super::config::{EndpointConfig, RouteQualityConfig};
use super::metrics::RouteQualityMetrics;
use super::mitigation::MitigationAction;

/// Route quality prober
pub struct RouteQualityProber {
    config: RouteQualityConfig,
    metrics: Arc<RouteQualityMetrics>,
    mitigation_tx: mpsc::Sender<MitigationAction>,
    /// Cached DNS results
    dns_cache: parking_lot::RwLock<std::collections::HashMap<String, (Vec<IpAddr>, Instant)>>,
}

impl RouteQualityProber {
    pub fn new(
        config: RouteQualityConfig,
        metrics: Arc<RouteQualityMetrics>,
        mitigation_tx: mpsc::Sender<MitigationAction>,
    ) -> Self {
        Self {
            config,
            metrics,
            mitigation_tx,
            dns_cache: parking_lot::RwLock::new(std::collections::HashMap::new()),
        }
    }
    
    /// Start the probing loop
    pub async fn run(&self) {
        info!("Starting route quality prober");
        
        let intervals = &self.config.probe_intervals;
        
        let mut icmp_interval = interval(intervals.icmp);
        let mut tcp_interval = interval(intervals.tcp);
        let mut tls_interval = interval(intervals.tls);
        let mut dns_interval = interval(intervals.dns);
        let mut traceroute_interval = interval(intervals.traceroute);
        
        loop {
            tokio::select! {
                _ = icmp_interval.tick() => {
                    self.probe_icmp_all().await;
                }
                _ = tcp_interval.tick() => {
                    self.probe_tcp_all().await;
                }
                _ = tls_interval.tick() => {
                    self.probe_tls_all().await;
                }
                _ = dns_interval.tick() => {
                    self.probe_dns_all().await;
                }
                _ = traceroute_interval.tick() => {
                    self.probe_traceroute_all().await;
                }
            }
            
            // Update health scores after each probe cycle
            self.update_health_scores();
            
            // Check for alert conditions
            self.check_alerts().await;
        }
    }
    
    /// Probe all endpoints with ICMP ping
    async fn probe_icmp_all(&self) {
        for endpoint in &self.config.endpoints {
            if let Some(ip) = self.resolve_endpoint(endpoint).await {
                self.probe_icmp(&endpoint.name, ip).await;
            }
        }
    }
    
    /// ICMP ping probe (using system ping command)
    async fn probe_icmp(&self, endpoint_name: &str, ip: IpAddr) {
        let start = Instant::now();
        
        // Use system ping (requires NET_RAW capability or running as root)
        let output = Command::new("ping")
            .args(["-c", "1", "-W", "1", &ip.to_string()])
            .output()
            .await;
        
        match output {
            Ok(out) if out.status.success() => {
                let rtt_us = start.elapsed().as_micros() as u64;
                
                // Try to parse actual RTT from ping output
                let actual_rtt = parse_ping_rtt(&String::from_utf8_lossy(&out.stdout))
                    .unwrap_or(rtt_us);
                
                self.metrics.record_rtt(endpoint_name, "icmp", actual_rtt);
                self.metrics.record_probe(endpoint_name, "icmp", true);
                
                debug!("ICMP {} -> {} rtt={}us", endpoint_name, ip, actual_rtt);
            }
            Ok(_) => {
                self.metrics.record_probe(endpoint_name, "icmp", false);
                warn!("ICMP probe failed for {} ({})", endpoint_name, ip);
                self.check_consecutive_failures(endpoint_name).await;
            }
            Err(e) => {
                self.metrics.record_probe(endpoint_name, "icmp", false);
                error!("ICMP probe error for {}: {}", endpoint_name, e);
                self.check_consecutive_failures(endpoint_name).await;
            }
        }
    }
    
    /// Probe all endpoints with TCP connect
    async fn probe_tcp_all(&self) {
        for endpoint in &self.config.endpoints {
            if let Some(ip) = self.resolve_endpoint(endpoint).await {
                self.probe_tcp(&endpoint.name, ip, endpoint.port).await;
            }
        }
    }
    
    /// TCP connect probe
    async fn probe_tcp(&self, endpoint_name: &str, ip: IpAddr, port: u16) {
        let addr = SocketAddr::new(ip, port);
        let start = Instant::now();
        
        let connect_timeout = Duration::from_secs(5);
        let result = timeout(connect_timeout, TcpStream::connect(addr)).await;
        
        match result {
            Ok(Ok(_stream)) => {
                let rtt_us = start.elapsed().as_micros() as u64;
                self.metrics.record_tcp_connect(endpoint_name, rtt_us);
                self.metrics.record_probe(endpoint_name, "tcp", true);
                
                debug!("TCP {} -> {}:{} connect={}us", endpoint_name, ip, port, rtt_us);
            }
            Ok(Err(e)) => {
                self.metrics.record_probe(endpoint_name, "tcp", false);
                warn!("TCP connect failed for {}:{} - {}", endpoint_name, port, e);
            }
            Err(_) => {
                self.metrics.record_probe(endpoint_name, "tcp", false);
                warn!("TCP connect timeout for {}:{}", endpoint_name, port);
            }
        }
    }
    
    /// Probe all endpoints with TLS handshake
    async fn probe_tls_all(&self) {
        for endpoint in &self.config.endpoints {
            if endpoint.protocol == "wss" || endpoint.protocol == "https" {
                if let Some(ip) = self.resolve_endpoint(endpoint).await {
                    self.probe_tls(&endpoint.name, &endpoint.host, ip, endpoint.port).await;
                }
            }
        }
    }
    
    /// TLS handshake probe (using openssl s_client for portability)
    async fn probe_tls(&self, endpoint_name: &str, host: &str, ip: IpAddr, port: u16) {
        let start = Instant::now();
        
        // Use openssl s_client for TLS probing (more portable than native-tls)
        let output = Command::new("openssl")
            .args([
                "s_client",
                "-connect", &format!("{}:{}", ip, port),
                "-servername", host,
                "-brief",
            ])
            .stdin(std::process::Stdio::null())
            .output()
            .await;
        
        match output {
            Ok(out) if out.status.success() || out.stderr.len() > 0 => {
                // openssl s_client exits with 0 on successful handshake
                // or outputs connection info to stderr
                let stderr = String::from_utf8_lossy(&out.stderr);
                
                if stderr.contains("CONNECTED") || stderr.contains("Protocol") {
                    let handshake_us = start.elapsed().as_micros() as u64;
                    
                    self.metrics.record_tls_handshake(endpoint_name, handshake_us);
                    self.metrics.record_probe(endpoint_name, "tls", true);
                    
                    debug!("TLS {} -> {}:{} handshake={}us", 
                        endpoint_name, ip, port, handshake_us);
                } else {
                    self.metrics.record_probe(endpoint_name, "tls", false);
                    warn!("TLS handshake failed for {}: {}", endpoint_name, 
                        stderr.lines().next().unwrap_or("unknown error"));
                }
            }
            Ok(_) => {
                self.metrics.record_probe(endpoint_name, "tls", false);
                warn!("TLS probe failed for {} (openssl error)", endpoint_name);
            }
            Err(e) => {
                // openssl not available, fall back to TCP-only
                self.metrics.record_probe(endpoint_name, "tls", false);
                debug!("TLS probe unavailable for {} (openssl not found): {}", endpoint_name, e);
            }
        }
    }
    
    /// Probe all endpoints with DNS resolution
    async fn probe_dns_all(&self) {
        for endpoint in &self.config.endpoints {
            self.probe_dns(&endpoint.name, &endpoint.host).await;
        }
    }
    
    /// DNS resolution probe
    async fn probe_dns(&self, endpoint_name: &str, host: &str) {
        let start = Instant::now();
        
        // Get previous IPs
        let prev_ips = self.dns_cache.read()
            .get(host)
            .map(|(ips, _)| ips.clone())
            .unwrap_or_default();
        
        // Resolve
        let resolve_result = tokio::task::spawn_blocking({
            let host = host.to_string();
            move || {
                (host.clone(), 0u16).to_socket_addrs()
                    .map(|addrs| addrs.map(|a| a.ip()).collect::<Vec<_>>())
            }
        }).await;
        
        match resolve_result {
            Ok(Ok(ips)) if !ips.is_empty() => {
                let latency_us = start.elapsed().as_micros() as u64;
                
                // Check if IPs changed
                let ip_changed = !prev_ips.is_empty() && prev_ips != ips;
                
                self.metrics.record_dns(endpoint_name, latency_us, ip_changed);
                self.metrics.record_probe(endpoint_name, "dns", true);
                
                // Update cache
                self.dns_cache.write().insert(host.to_string(), (ips.clone(), Instant::now()));
                
                if ip_changed {
                    info!("DNS IP changed for {}: {:?} -> {:?}", endpoint_name, prev_ips, ips);
                    self.trigger_dns_change(endpoint_name).await;
                }
                
                debug!("DNS {} -> {:?} latency={}us changed={}", 
                    endpoint_name, ips, latency_us, ip_changed);
            }
            _ => {
                self.metrics.record_probe(endpoint_name, "dns", false);
                warn!("DNS resolution failed for {}", endpoint_name);
            }
        }
    }
    
    /// Probe all endpoints with traceroute
    async fn probe_traceroute_all(&self) {
        for endpoint in &self.config.endpoints {
            if let Some(ip) = self.resolve_endpoint(endpoint).await {
                self.probe_traceroute(&endpoint.name, ip).await;
            }
        }
    }
    
    /// Traceroute probe
    async fn probe_traceroute(&self, endpoint_name: &str, ip: IpAddr) {
        // Use mtr for better output (falls back to traceroute)
        let output = Command::new("mtr")
            .args(["--report", "--report-cycles", "1", "--json", &ip.to_string()])
            .output()
            .await;
        
        match output {
            Ok(out) if out.status.success() => {
                let stdout = String::from_utf8_lossy(&out.stdout);
                
                // Parse hop count and compute path hash
                let (hop_count, path_hash) = parse_mtr_output(&stdout);
                
                // Check for path change
                let prev_hash = self.metrics.path_hash.read()
                    .get(endpoint_name)
                    .copied()
                    .unwrap_or(0);
                
                if prev_hash != 0 && prev_hash != path_hash {
                    info!("Route path changed for {}", endpoint_name);
                    self.trigger_path_change(endpoint_name).await;
                }
                
                self.metrics.record_path(endpoint_name, hop_count, path_hash);
                self.metrics.record_probe(endpoint_name, "traceroute", true);
                
                debug!("Traceroute {} -> {} hops={} hash={}", 
                    endpoint_name, ip, hop_count, path_hash);
            }
            _ => {
                // Try fallback to traceroute
                let fallback = Command::new("traceroute")
                    .args(["-m", "30", "-w", "1", &ip.to_string()])
                    .output()
                    .await;
                
                match fallback {
                    Ok(out) if out.status.success() => {
                        let stdout = String::from_utf8_lossy(&out.stdout);
                        let hop_count = stdout.lines().count() as u32;
                        let path_hash = hash_string(&stdout);
                        
                        self.metrics.record_path(endpoint_name, hop_count, path_hash);
                        self.metrics.record_probe(endpoint_name, "traceroute", true);
                    }
                    _ => {
                        self.metrics.record_probe(endpoint_name, "traceroute", false);
                        debug!("Traceroute unavailable for {}", endpoint_name);
                    }
                }
            }
        }
    }
    
    /// Resolve endpoint hostname to IP
    async fn resolve_endpoint(&self, endpoint: &EndpointConfig) -> Option<IpAddr> {
        // Check cache first
        if let Some((ips, _)) = self.dns_cache.read().get(&endpoint.host) {
            if !ips.is_empty() {
                return Some(ips[0]);
            }
        }
        
        // Resolve
        let host = endpoint.host.clone();
        let port = endpoint.port;
        
        tokio::task::spawn_blocking(move || {
            (host, port).to_socket_addrs()
                .ok()
                .and_then(|mut addrs| addrs.next())
                .map(|a| a.ip())
        }).await.ok().flatten()
    }
    
    /// Update health scores for all endpoints
    fn update_health_scores(&self) {
        for endpoint in &self.config.endpoints {
            let name = &endpoint.name;
            
            // Get baseline metrics
            let baseline = self.metrics.baseline.read();
            let baseline_rtt = baseline.rtt_p99.get(name).copied().unwrap_or(0.05);
            let baseline_stddev = baseline.rtt_stddev.get(name).copied().unwrap_or(0.01);
            
            // Get current metrics
            let rtt_key = format!("{}:icmp", name);
            let current_rtt = self.metrics.rtt.read()
                .get(&rtt_key)
                .map(|h| h.mean() / 1_000_000.0)  // Convert to seconds
                .unwrap_or(baseline_rtt);
            
            let packet_loss = self.metrics.packet_loss_rate(name, "icmp");
            
            // Calculate health score (0-100)
            // RTT component (40% weight)
            let rtt_ratio = current_rtt / baseline_rtt.max(0.001);
            let rtt_score = (1.0 / rtt_ratio).min(1.0).max(0.0) * 40.0;
            
            // Packet loss component (40% weight)
            let loss_score = (1.0 - packet_loss.min(1.0)) * 40.0;
            
            // Stability component (20% weight) - based on consecutive failures
            let failures = self.metrics.get_consecutive_failures(name) as f64;
            let stability_score = (1.0 - (failures / 10.0).min(1.0)) * 20.0;
            
            let total_score = rtt_score + loss_score + stability_score;
            self.metrics.update_health_score(name, total_score);
        }
    }
    
    /// Check for alert conditions and trigger mitigations
    async fn check_alerts(&self) {
        let thresholds = &self.config.thresholds;
        
        for endpoint in &self.config.endpoints {
            let name = &endpoint.name;
            let health = self.metrics.get_health_score(name);
            let failures = self.metrics.get_consecutive_failures(name);
            let packet_loss = self.metrics.packet_loss_rate(name, "icmp");
            
            // Check critical conditions
            if health < thresholds.health_score_critical {
                info!("ALERT: Health critical for {} (score={})", name, health);
                self.trigger_failover(name, "health_critical").await;
            } else if failures >= thresholds.consecutive_failures_critical {
                info!("ALERT: Consecutive failures for {} (count={})", name, failures);
                self.trigger_failover(name, "consecutive_failures").await;
            } else if packet_loss > thresholds.packet_loss_critical {
                info!("ALERT: Packet loss critical for {} (loss={})", name, packet_loss);
                self.trigger_failover(name, "packet_loss_critical").await;
            }
        }
    }
    
    /// Check consecutive failures and trigger mitigation if needed
    async fn check_consecutive_failures(&self, endpoint_name: &str) {
        let failures = self.metrics.get_consecutive_failures(endpoint_name);
        if failures >= self.config.thresholds.consecutive_failures_critical {
            self.trigger_failover(endpoint_name, "consecutive_failures").await;
        }
    }
    
    /// Trigger DNS change mitigation
    async fn trigger_dns_change(&self, endpoint_name: &str) {
        let _ = self.mitigation_tx.send(MitigationAction::DnsRefresh {
            endpoint: endpoint_name.to_string(),
        }).await;
    }
    
    /// Trigger path change investigation
    async fn trigger_path_change(&self, endpoint_name: &str) {
        let _ = self.mitigation_tx.send(MitigationAction::PathChangeInvestigation {
            endpoint: endpoint_name.to_string(),
        }).await;
    }
    
    /// Trigger failover
    async fn trigger_failover(&self, endpoint_name: &str, reason: &str) {
        let _ = self.mitigation_tx.send(MitigationAction::Failover {
            from_endpoint: endpoint_name.to_string(),
            reason: reason.to_string(),
        }).await;
    }
}

/// Parse RTT from ping output (platform-specific)
fn parse_ping_rtt(output: &str) -> Option<u64> {
    // Linux format: "time=1.23 ms"
    // macOS format: "time=1.234 ms"
    if let Some(start) = output.find("time=") {
        let rest = &output[start + 5..];
        if let Some(end) = rest.find(" ms") {
            if let Ok(ms) = rest[..end].parse::<f64>() {
                return Some((ms * 1000.0) as u64); // Convert to microseconds
            }
        }
    }
    None
}

/// Parse mtr JSON output
fn parse_mtr_output(output: &str) -> (u32, u64) {
    // Simple parsing - count hubs and hash the output
    let hop_count = output.matches("\"hub\"").count() as u32;
    let path_hash = hash_string(output);
    (hop_count, path_hash)
}

/// Simple string hash for path change detection
fn hash_string(s: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    s.hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_parse_ping_rtt() {
        let linux = "64 bytes from 1.2.3.4: icmp_seq=1 ttl=56 time=12.3 ms";
        assert_eq!(parse_ping_rtt(linux), Some(12300));
        
        let macos = "64 bytes from 1.2.3.4: icmp_seq=0 ttl=56 time=1.234 ms";
        assert_eq!(parse_ping_rtt(macos), Some(1234));
    }
}

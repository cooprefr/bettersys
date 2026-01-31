//! Integration hooks for BetterBot main application
//!
//! Provides callbacks and events for the route quality monitor to trigger
//! actions in the main trading application (connection refresh, failover, etc.)

use std::sync::Arc;
use parking_lot::RwLock;
use tokio::sync::broadcast;
use tracing::{info, warn};

/// Route quality events sent to the main application
#[derive(Debug, Clone)]
pub enum RouteQualityEvent {
    /// Endpoint health changed
    HealthChanged {
        endpoint: String,
        health_score: f64,
        previous_score: f64,
    },
    
    /// DNS resolution changed
    DnsChanged {
        endpoint: String,
        new_ips: Vec<std::net::IpAddr>,
    },
    
    /// Connection refresh recommended
    RefreshConnections {
        endpoint: String,
        reason: String,
    },
    
    /// Failover triggered
    Failover {
        from_endpoint: String,
        to_endpoint: String,
        reason: String,
    },
    
    /// Failback to primary
    Failback {
        to_endpoint: String,
    },
    
    /// Route path changed (traceroute detected difference)
    PathChanged {
        endpoint: String,
        hop_count_delta: i32,
    },
    
    /// Packet loss detected
    PacketLoss {
        endpoint: String,
        loss_rate: f64,
    },
    
    /// Latency anomaly detected
    LatencyAnomaly {
        endpoint: String,
        current_rtt_ms: f64,
        baseline_rtt_ms: f64,
        sigma: f64,
    },
}

/// Route quality state exposed to main application
#[derive(Debug, Clone)]
pub struct RouteQualityState {
    /// Currently active endpoint
    pub active_endpoint: String,
    
    /// Health scores per endpoint (0-100)
    pub health_scores: std::collections::HashMap<String, f64>,
    
    /// Packet loss rates per endpoint (0-1)
    pub packet_loss: std::collections::HashMap<String, f64>,
    
    /// RTT p99 per endpoint (seconds)
    pub rtt_p99: std::collections::HashMap<String, f64>,
    
    /// DNS cached IPs per endpoint
    pub dns_cache: std::collections::HashMap<String, Vec<std::net::IpAddr>>,
    
    /// Circuit breaker state per endpoint
    pub circuit_open: std::collections::HashMap<String, bool>,
    
    /// Last update timestamp
    pub last_updated: std::time::Instant,
}

impl Default for RouteQualityState {
    fn default() -> Self {
        Self {
            active_endpoint: String::new(),
            health_scores: std::collections::HashMap::new(),
            packet_loss: std::collections::HashMap::new(),
            rtt_p99: std::collections::HashMap::new(),
            dns_cache: std::collections::HashMap::new(),
            circuit_open: std::collections::HashMap::new(),
            last_updated: std::time::Instant::now(),
        }
    }
}

/// Integration handle for main application
pub struct RouteQualityIntegration {
    /// Broadcast channel for events
    event_tx: broadcast::Sender<RouteQualityEvent>,
    
    /// Shared state
    state: Arc<RwLock<RouteQualityState>>,
}

impl RouteQualityIntegration {
    pub fn new() -> (Self, RouteQualityHandle) {
        let (event_tx, _) = broadcast::channel(100);
        let state = Arc::new(RwLock::new(RouteQualityState::default()));
        
        let integration = Self {
            event_tx: event_tx.clone(),
            state: state.clone(),
        };
        
        let handle = RouteQualityHandle {
            event_rx: event_tx.subscribe(),
            state,
        };
        
        (integration, handle)
    }
    
    /// Update active endpoint
    pub fn set_active_endpoint(&self, endpoint: &str) {
        self.state.write().active_endpoint = endpoint.to_string();
        self.state.write().last_updated = std::time::Instant::now();
    }
    
    /// Update health score for endpoint
    pub fn update_health(&self, endpoint: &str, score: f64) {
        let mut state = self.state.write();
        let previous = state.health_scores.get(endpoint).copied().unwrap_or(100.0);
        state.health_scores.insert(endpoint.to_string(), score);
        state.last_updated = std::time::Instant::now();
        
        // Emit event if significant change
        if (score - previous).abs() > 5.0 {
            let _ = self.event_tx.send(RouteQualityEvent::HealthChanged {
                endpoint: endpoint.to_string(),
                health_score: score,
                previous_score: previous,
            });
        }
    }
    
    /// Update packet loss for endpoint
    pub fn update_packet_loss(&self, endpoint: &str, loss_rate: f64) {
        let mut state = self.state.write();
        state.packet_loss.insert(endpoint.to_string(), loss_rate);
        state.last_updated = std::time::Instant::now();
        
        // Emit event if significant loss
        if loss_rate > 0.001 {
            let _ = self.event_tx.send(RouteQualityEvent::PacketLoss {
                endpoint: endpoint.to_string(),
                loss_rate,
            });
        }
    }
    
    /// Update RTT for endpoint
    pub fn update_rtt(&self, endpoint: &str, rtt_p99_sec: f64) {
        let mut state = self.state.write();
        state.rtt_p99.insert(endpoint.to_string(), rtt_p99_sec);
        state.last_updated = std::time::Instant::now();
    }
    
    /// Update DNS cache for endpoint
    pub fn update_dns(&self, endpoint: &str, ips: Vec<std::net::IpAddr>) {
        let mut state = self.state.write();
        let changed = state.dns_cache.get(endpoint) != Some(&ips);
        state.dns_cache.insert(endpoint.to_string(), ips.clone());
        state.last_updated = std::time::Instant::now();
        
        if changed && !ips.is_empty() {
            let _ = self.event_tx.send(RouteQualityEvent::DnsChanged {
                endpoint: endpoint.to_string(),
                new_ips: ips,
            });
        }
    }
    
    /// Set circuit breaker state
    pub fn set_circuit_state(&self, endpoint: &str, open: bool) {
        self.state.write().circuit_open.insert(endpoint.to_string(), open);
        self.state.write().last_updated = std::time::Instant::now();
    }
    
    /// Emit connection refresh event
    pub fn emit_refresh(&self, endpoint: &str, reason: &str) {
        let _ = self.event_tx.send(RouteQualityEvent::RefreshConnections {
            endpoint: endpoint.to_string(),
            reason: reason.to_string(),
        });
    }
    
    /// Emit failover event
    pub fn emit_failover(&self, from: &str, to: &str, reason: &str) {
        info!("Route quality: failover {} -> {} ({})", from, to, reason);
        
        self.state.write().active_endpoint = to.to_string();
        self.state.write().last_updated = std::time::Instant::now();
        
        let _ = self.event_tx.send(RouteQualityEvent::Failover {
            from_endpoint: from.to_string(),
            to_endpoint: to.to_string(),
            reason: reason.to_string(),
        });
    }
    
    /// Emit failback event
    pub fn emit_failback(&self, to: &str) {
        info!("Route quality: failback to {}", to);
        
        self.state.write().active_endpoint = to.to_string();
        self.state.write().last_updated = std::time::Instant::now();
        
        let _ = self.event_tx.send(RouteQualityEvent::Failback {
            to_endpoint: to.to_string(),
        });
    }
    
    /// Emit latency anomaly event
    pub fn emit_latency_anomaly(&self, endpoint: &str, current_ms: f64, baseline_ms: f64, sigma: f64) {
        warn!(
            "Route quality: latency anomaly for {} (current={:.1}ms, baseline={:.1}ms, {:.1}σ)",
            endpoint, current_ms, baseline_ms, sigma
        );
        
        let _ = self.event_tx.send(RouteQualityEvent::LatencyAnomaly {
            endpoint: endpoint.to_string(),
            current_rtt_ms: current_ms,
            baseline_rtt_ms: baseline_ms,
            sigma,
        });
    }
}

/// Handle for main application to receive events
pub struct RouteQualityHandle {
    /// Event receiver
    event_rx: broadcast::Receiver<RouteQualityEvent>,
    
    /// Shared state (read-only)
    state: Arc<RwLock<RouteQualityState>>,
}

impl RouteQualityHandle {
    /// Get current state snapshot
    pub fn get_state(&self) -> RouteQualityState {
        self.state.read().clone()
    }
    
    /// Get active endpoint
    pub fn active_endpoint(&self) -> String {
        self.state.read().active_endpoint.clone()
    }
    
    /// Get health score for endpoint
    pub fn health_score(&self, endpoint: &str) -> f64 {
        self.state.read().health_scores.get(endpoint).copied().unwrap_or(100.0)
    }
    
    /// Check if circuit is open for endpoint
    pub fn is_circuit_open(&self, endpoint: &str) -> bool {
        self.state.read().circuit_open.get(endpoint).copied().unwrap_or(false)
    }
    
    /// Get best IP for endpoint from DNS cache
    pub fn get_ip(&self, endpoint: &str) -> Option<std::net::IpAddr> {
        self.state.read().dns_cache.get(endpoint).and_then(|ips| ips.first().copied())
    }
    
    /// Receive next event (async)
    pub async fn recv_event(&mut self) -> Option<RouteQualityEvent> {
        self.event_rx.recv().await.ok()
    }
    
    /// Try to receive event (non-blocking)
    pub fn try_recv_event(&mut self) -> Option<RouteQualityEvent> {
        self.event_rx.try_recv().ok()
    }
}

impl Clone for RouteQualityHandle {
    fn clone(&self) -> Self {
        Self {
            event_rx: self.event_rx.resubscribe(),
            state: self.state.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_integration_state() {
        let (integration, handle) = RouteQualityIntegration::new();
        
        integration.set_active_endpoint("binance-ws-primary");
        integration.update_health("binance-ws-primary", 95.0);
        integration.update_packet_loss("binance-ws-primary", 0.0001);
        
        assert_eq!(handle.active_endpoint(), "binance-ws-primary");
        assert_eq!(handle.health_score("binance-ws-primary"), 95.0);
    }
}

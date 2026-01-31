//! Automatic Mitigation Actions
//!
//! Handles DNS refresh, connection re-establishment, and endpoint failover
//! based on route quality alerts.

use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::RwLock;
use tokio::sync::mpsc;
use tracing::{info, warn, error};

use super::config::{RouteQualityConfig, FailoverPolicy, ConnectionPolicy, DnsPolicy};
use super::metrics::RouteQualityMetrics;

/// Mitigation action types
#[derive(Debug, Clone)]
pub enum MitigationAction {
    /// Refresh DNS cache and re-resolve endpoints
    DnsRefresh {
        endpoint: String,
    },
    /// Re-establish connections
    ConnectionRefresh {
        endpoint: String,
    },
    /// Failover to backup endpoint
    Failover {
        from_endpoint: String,
        reason: String,
    },
    /// Failback to primary endpoint
    Failback {
        to_endpoint: String,
    },
    /// Investigate path change
    PathChangeInvestigation {
        endpoint: String,
    },
}

/// Failover state machine state
#[derive(Debug, Clone, PartialEq)]
pub enum FailoverState {
    Healthy,
    Degraded,
    FailedOver,
    Impaired,
}

/// Mitigation controller
pub struct MitigationController {
    config: RouteQualityConfig,
    metrics: Arc<RouteQualityMetrics>,
    action_rx: mpsc::Receiver<MitigationAction>,
    
    /// Current failover state
    state: RwLock<FailoverState>,
    
    /// Current active endpoint
    active_endpoint: RwLock<String>,
    
    /// Last action timestamps for cooldown
    last_dns_refresh: RwLock<Option<Instant>>,
    last_connection_refresh: RwLock<Option<Instant>>,
    last_failover: RwLock<Option<Instant>>,
    
    /// Circuit breaker state per endpoint
    circuit_breakers: RwLock<std::collections::HashMap<String, CircuitBreaker>>,
    
    /// Callback for notifying application of changes
    app_callback: Option<Box<dyn Fn(MitigationEvent) + Send + Sync>>,
}

/// Circuit breaker for preventing failover thrashing
#[derive(Debug, Clone)]
pub struct CircuitBreaker {
    pub failures: u32,
    pub successes: u32,
    pub state: CircuitState,
    pub last_failure: Option<Instant>,
    pub opened_at: Option<Instant>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum CircuitState {
    Closed,
    Open,
    HalfOpen,
}

impl Default for CircuitBreaker {
    fn default() -> Self {
        Self {
            failures: 0,
            successes: 0,
            state: CircuitState::Closed,
            last_failure: None,
            opened_at: None,
        }
    }
}

/// Events sent to application
#[derive(Debug, Clone)]
pub enum MitigationEvent {
    DnsRefreshed { endpoint: String, new_ips: Vec<std::net::IpAddr> },
    ConnectionRefreshed { endpoint: String },
    FailoverExecuted { from: String, to: String, reason: String },
    FailbackExecuted { to: String },
    CircuitOpened { endpoint: String },
    CircuitClosed { endpoint: String },
}

impl MitigationController {
    pub fn new(
        config: RouteQualityConfig,
        metrics: Arc<RouteQualityMetrics>,
        action_rx: mpsc::Receiver<MitigationAction>,
    ) -> Self {
        let primary = config.endpoints.first()
            .map(|e| e.name.clone())
            .unwrap_or_default();
        
        Self {
            config,
            metrics,
            action_rx,
            state: RwLock::new(FailoverState::Healthy),
            active_endpoint: RwLock::new(primary),
            last_dns_refresh: RwLock::new(None),
            last_connection_refresh: RwLock::new(None),
            last_failover: RwLock::new(None),
            circuit_breakers: RwLock::new(std::collections::HashMap::new()),
            app_callback: None,
        }
    }
    
    /// Set callback for application notifications
    pub fn set_callback<F>(&mut self, callback: F)
    where
        F: Fn(MitigationEvent) + Send + Sync + 'static,
    {
        self.app_callback = Some(Box::new(callback));
    }
    
    /// Run the mitigation controller
    pub async fn run(mut self) {
        info!("Starting mitigation controller");
        
        while let Some(action) = self.action_rx.recv().await {
            self.handle_action(action).await;
        }
    }
    
    /// Handle a mitigation action
    async fn handle_action(&self, action: MitigationAction) {
        match action {
            MitigationAction::DnsRefresh { endpoint } => {
                self.handle_dns_refresh(&endpoint).await;
            }
            MitigationAction::ConnectionRefresh { endpoint } => {
                self.handle_connection_refresh(&endpoint).await;
            }
            MitigationAction::Failover { from_endpoint, reason } => {
                self.handle_failover(&from_endpoint, &reason).await;
            }
            MitigationAction::Failback { to_endpoint } => {
                self.handle_failback(&to_endpoint).await;
            }
            MitigationAction::PathChangeInvestigation { endpoint } => {
                self.handle_path_change(&endpoint).await;
            }
        }
    }
    
    /// Handle DNS refresh action
    async fn handle_dns_refresh(&self, endpoint: &str) {
        // Check cooldown
        let cooldown = self.config.dns_policy.refresh_interval / 10;
        if !self.check_cooldown(&self.last_dns_refresh, cooldown) {
            info!("DNS refresh skipped (cooldown) for {}", endpoint);
            return;
        }
        
        info!("Executing DNS refresh for {}", endpoint);
        
        // Clear DNS cache (would integrate with actual DNS resolver)
        self.metrics.clear_dns_change(endpoint);
        
        // Update timestamp
        *self.last_dns_refresh.write() = Some(Instant::now());
        
        // Trigger connection refresh after DNS change
        self.handle_connection_refresh(endpoint).await;
        
        // Notify application
        if let Some(ref callback) = self.app_callback {
            callback(MitigationEvent::DnsRefreshed {
                endpoint: endpoint.to_string(),
                new_ips: vec![], // Would contain actual resolved IPs
            });
        }
    }
    
    /// Handle connection refresh action
    async fn handle_connection_refresh(&self, endpoint: &str) {
        let cooldown = self.config.connection_policy.refresh_cooldown;
        if !self.check_cooldown(&self.last_connection_refresh, cooldown) {
            info!("Connection refresh skipped (cooldown) for {}", endpoint);
            return;
        }
        
        info!("Executing connection refresh for {}", endpoint);
        
        // Update timestamp
        *self.last_connection_refresh.write() = Some(Instant::now());
        
        // Notify application to refresh connections
        if let Some(ref callback) = self.app_callback {
            callback(MitigationEvent::ConnectionRefreshed {
                endpoint: endpoint.to_string(),
            });
        }
    }
    
    /// Handle failover action
    async fn handle_failover(&self, from_endpoint: &str, reason: &str) {
        // Check cooldown
        let cooldown = self.config.failover_policy.cooldown;
        if !self.check_cooldown(&self.last_failover, cooldown) {
            info!("Failover skipped (cooldown) from {}", from_endpoint);
            return;
        }
        
        // Check circuit breaker
        if self.is_circuit_open(from_endpoint) {
            info!("Failover skipped (circuit open) from {}", from_endpoint);
            return;
        }
        
        // Find best backup endpoint
        let to_endpoint = match self.select_failover_target(from_endpoint) {
            Some(e) => e,
            None => {
                error!("No healthy backup endpoint available for failover from {}", from_endpoint);
                self.open_circuit(from_endpoint);
                return;
            }
        };
        
        // Verify candidate if configured
        if self.config.failover_policy.verify_candidate {
            let health = self.metrics.get_health_score(&to_endpoint);
            if health < self.config.thresholds.health_score_warning {
                warn!("Failover candidate {} has low health ({}), proceeding anyway", 
                    to_endpoint, health);
            }
        }
        
        info!("Executing failover: {} -> {} (reason: {})", 
            from_endpoint, to_endpoint, reason);
        
        // Update state
        *self.state.write() = FailoverState::FailedOver;
        *self.active_endpoint.write() = to_endpoint.clone();
        *self.last_failover.write() = Some(Instant::now());
        
        // Record metrics
        self.metrics.record_failover();
        self.metrics.set_active_endpoint(&to_endpoint);
        
        // Record failure for circuit breaker
        self.record_failure(from_endpoint);
        
        // Notify application
        if let Some(ref callback) = self.app_callback {
            callback(MitigationEvent::FailoverExecuted {
                from: from_endpoint.to_string(),
                to: to_endpoint,
                reason: reason.to_string(),
            });
        }
    }
    
    /// Handle failback action
    async fn handle_failback(&self, to_endpoint: &str) {
        if !self.config.failover_policy.failback_enabled {
            return;
        }
        
        // Check if primary is healthy enough
        let health = self.metrics.get_health_score(to_endpoint);
        if health < self.config.thresholds.health_score_warning {
            info!("Failback skipped: {} health too low ({})", to_endpoint, health);
            return;
        }
        
        info!("Executing failback to {}", to_endpoint);
        
        // Update state
        *self.state.write() = FailoverState::Healthy;
        *self.active_endpoint.write() = to_endpoint.to_string();
        
        // Update metrics
        self.metrics.set_active_endpoint(to_endpoint);
        
        // Record success for circuit breaker
        self.record_success(to_endpoint);
        
        // Notify application
        if let Some(ref callback) = self.app_callback {
            callback(MitigationEvent::FailbackExecuted {
                to: to_endpoint.to_string(),
            });
        }
    }
    
    /// Handle path change investigation
    async fn handle_path_change(&self, endpoint: &str) {
        info!("Investigating path change for {}", endpoint);
        
        // Compare current metrics with baseline
        let health = self.metrics.get_health_score(endpoint);
        
        if health < self.config.thresholds.health_score_warning {
            // Path change resulted in degradation, refresh connections
            warn!("Path change caused degradation for {}, refreshing connections", endpoint);
            self.handle_connection_refresh(endpoint).await;
        } else {
            info!("Path change for {} did not cause degradation", endpoint);
        }
    }
    
    /// Select best failover target
    fn select_failover_target(&self, exclude: &str) -> Option<String> {
        let mut candidates: Vec<_> = self.config.endpoints.iter()
            .filter(|e| e.name != exclude)
            .filter(|e| !self.is_circuit_open(&e.name))
            .map(|e| {
                let health = self.metrics.get_health_score(&e.name);
                (e.clone(), health)
            })
            .collect();
        
        // Sort by priority first, then by health
        candidates.sort_by(|a, b| {
            match a.0.priority.cmp(&b.0.priority) {
                std::cmp::Ordering::Equal => {
                    b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal)
                }
                other => other,
            }
        });
        
        candidates.first().map(|(e, _)| e.name.clone())
    }
    
    /// Check if cooldown period has passed
    fn check_cooldown(&self, last_action: &RwLock<Option<Instant>>, cooldown: Duration) -> bool {
        match *last_action.read() {
            Some(last) => last.elapsed() >= cooldown,
            None => true,
        }
    }
    
    /// Check if circuit is open for endpoint
    fn is_circuit_open(&self, endpoint: &str) -> bool {
        let breakers = self.circuit_breakers.read();
        if let Some(cb) = breakers.get(endpoint) {
            match cb.state {
                CircuitState::Open => {
                    // Check if timeout has passed
                    if let Some(opened_at) = cb.opened_at {
                        if opened_at.elapsed() >= self.config.failover_policy.circuit_breaker_timeout {
                            return false; // Move to half-open on next check
                        }
                    }
                    true
                }
                _ => false,
            }
        } else {
            false
        }
    }
    
    /// Open circuit for endpoint
    fn open_circuit(&self, endpoint: &str) {
        let mut breakers = self.circuit_breakers.write();
        let cb = breakers.entry(endpoint.to_string()).or_default();
        cb.state = CircuitState::Open;
        cb.opened_at = Some(Instant::now());
        
        info!("Circuit opened for {}", endpoint);
        
        if let Some(ref callback) = self.app_callback {
            callback(MitigationEvent::CircuitOpened {
                endpoint: endpoint.to_string(),
            });
        }
    }
    
    /// Record failure for circuit breaker
    fn record_failure(&self, endpoint: &str) {
        let mut breakers = self.circuit_breakers.write();
        let cb = breakers.entry(endpoint.to_string()).or_default();
        cb.failures += 1;
        cb.successes = 0;
        cb.last_failure = Some(Instant::now());
        
        if cb.failures >= self.config.failover_policy.circuit_breaker_threshold {
            cb.state = CircuitState::Open;
            cb.opened_at = Some(Instant::now());
        }
    }
    
    /// Record success for circuit breaker
    fn record_success(&self, endpoint: &str) {
        let mut breakers = self.circuit_breakers.write();
        let cb = breakers.entry(endpoint.to_string()).or_default();
        
        match cb.state {
            CircuitState::HalfOpen => {
                cb.successes += 1;
                if cb.successes >= self.config.failover_policy.circuit_breaker_success {
                    cb.state = CircuitState::Closed;
                    cb.failures = 0;
                    cb.successes = 0;
                    
                    info!("Circuit closed for {}", endpoint);
                    
                    if let Some(ref callback) = self.app_callback {
                        callback(MitigationEvent::CircuitClosed {
                            endpoint: endpoint.to_string(),
                        });
                    }
                }
            }
            CircuitState::Open => {
                // Check if we should move to half-open
                if let Some(opened_at) = cb.opened_at {
                    if opened_at.elapsed() >= self.config.failover_policy.circuit_breaker_timeout {
                        cb.state = CircuitState::HalfOpen;
                        cb.successes = 1;
                    }
                }
            }
            CircuitState::Closed => {
                cb.failures = 0;
            }
        }
    }
    
    /// Get current failover state
    pub fn get_state(&self) -> FailoverState {
        self.state.read().clone()
    }
    
    /// Get current active endpoint
    pub fn get_active_endpoint(&self) -> String {
        self.active_endpoint.read().clone()
    }
}

//! Route Quality Monitor Configuration

use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Main configuration for route quality monitoring
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteQualityConfig {
    /// Endpoints to monitor
    pub endpoints: Vec<EndpointConfig>,
    
    /// Probe intervals
    pub probe_intervals: ProbeIntervals,
    
    /// Alert thresholds
    pub thresholds: AlertThresholds,
    
    /// DNS policy
    pub dns_policy: DnsPolicy,
    
    /// Connection policy
    pub connection_policy: ConnectionPolicy,
    
    /// Failover policy
    pub failover_policy: FailoverPolicy,
    
    /// Baseline calculation settings
    pub baseline: BaselineConfig,
}

impl Default for RouteQualityConfig {
    fn default() -> Self {
        Self {
            endpoints: vec![
                EndpointConfig::binance_ws_primary(),
                EndpointConfig::binance_ws_backup1(),
                EndpointConfig::binance_ws_backup2(),
            ],
            probe_intervals: ProbeIntervals::default(),
            thresholds: AlertThresholds::default(),
            dns_policy: DnsPolicy::default(),
            connection_policy: ConnectionPolicy::default(),
            failover_policy: FailoverPolicy::default(),
            baseline: BaselineConfig::default(),
        }
    }
}

/// Endpoint configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EndpointConfig {
    /// Human-readable name
    pub name: String,
    /// Hostname
    pub host: String,
    /// Port
    pub port: u16,
    /// Protocol (wss, https, tcp)
    pub protocol: String,
    /// Priority (lower = higher priority)
    pub priority: u8,
    /// Weight for load balancing within same priority
    pub weight: u8,
    /// Cached IP addresses (populated at runtime)
    #[serde(skip)]
    pub cached_ips: Vec<std::net::IpAddr>,
}

impl EndpointConfig {
    pub fn binance_ws_primary() -> Self {
        Self {
            name: "binance-ws-primary".into(),
            host: "stream.binance.com".into(),
            port: 9443,
            protocol: "wss".into(),
            priority: 1,
            weight: 100,
            cached_ips: vec![],
        }
    }
    
    pub fn binance_ws_backup1() -> Self {
        Self {
            name: "binance-ws-backup1".into(),
            host: "stream1.binance.com".into(),
            port: 9443,
            protocol: "wss".into(),
            priority: 2,
            weight: 50,
            cached_ips: vec![],
        }
    }
    
    pub fn binance_ws_backup2() -> Self {
        Self {
            name: "binance-ws-backup2".into(),
            host: "stream2.binance.com".into(),
            port: 9443,
            protocol: "wss".into(),
            priority: 2,
            weight: 50,
            cached_ips: vec![],
        }
    }
    
    pub fn binance_rest_primary() -> Self {
        Self {
            name: "binance-rest-primary".into(),
            host: "api.binance.com".into(),
            port: 443,
            protocol: "https".into(),
            priority: 1,
            weight: 100,
            cached_ips: vec![],
        }
    }
}

/// Probe interval configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProbeIntervals {
    /// ICMP ping interval
    #[serde(with = "duration_serde")]
    pub icmp: Duration,
    /// TCP connect probe interval
    #[serde(with = "duration_serde")]
    pub tcp: Duration,
    /// TLS handshake probe interval
    #[serde(with = "duration_serde")]
    pub tls: Duration,
    /// DNS resolution probe interval
    #[serde(with = "duration_serde")]
    pub dns: Duration,
    /// Traceroute interval
    #[serde(with = "duration_serde")]
    pub traceroute: Duration,
    /// HTTP health check interval
    #[serde(with = "duration_serde")]
    pub http: Duration,
}

impl Default for ProbeIntervals {
    fn default() -> Self {
        Self {
            icmp: Duration::from_secs(1),
            tcp: Duration::from_secs(5),
            tls: Duration::from_secs(10),
            dns: Duration::from_secs(30),
            traceroute: Duration::from_secs(60),
            http: Duration::from_secs(30),
        }
    }
}

/// Alert thresholds
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlertThresholds {
    /// RTT warning threshold (absolute, seconds)
    pub rtt_warning_sec: f64,
    /// RTT critical threshold (absolute, seconds)
    pub rtt_critical_sec: f64,
    /// RTT warning threshold (sigma above baseline)
    pub rtt_warning_sigma: f64,
    /// RTT critical threshold (sigma above baseline)
    pub rtt_critical_sigma: f64,
    /// Packet loss warning threshold (ratio)
    pub packet_loss_warning: f64,
    /// Packet loss critical threshold (ratio)
    pub packet_loss_critical: f64,
    /// Consecutive failures before critical alert
    pub consecutive_failures_critical: u32,
    /// TCP connect warning threshold (seconds)
    pub tcp_connect_warning_sec: f64,
    /// TCP connect critical threshold (seconds)
    pub tcp_connect_critical_sec: f64,
    /// TLS handshake warning threshold (seconds)
    pub tls_handshake_warning_sec: f64,
    /// TLS handshake critical threshold (seconds)
    pub tls_handshake_critical_sec: f64,
    /// DNS resolution warning threshold (seconds)
    pub dns_warning_sec: f64,
    /// DNS resolution critical threshold (seconds)
    pub dns_critical_sec: f64,
    /// Hop count change threshold for alert
    pub hop_count_delta_alert: i32,
    /// Health score warning threshold (0-100)
    pub health_score_warning: f64,
    /// Health score critical threshold (0-100)
    pub health_score_critical: f64,
}

impl Default for AlertThresholds {
    fn default() -> Self {
        Self {
            rtt_warning_sec: 0.05,      // 50ms
            rtt_critical_sec: 0.1,      // 100ms
            rtt_warning_sigma: 2.0,
            rtt_critical_sigma: 4.0,
            packet_loss_warning: 0.001, // 0.1%
            packet_loss_critical: 0.01, // 1%
            consecutive_failures_critical: 5,
            tcp_connect_warning_sec: 0.02,  // 20ms
            tcp_connect_critical_sec: 0.05, // 50ms
            tls_handshake_warning_sec: 0.1, // 100ms
            tls_handshake_critical_sec: 0.2, // 200ms
            dns_warning_sec: 0.05,  // 50ms
            dns_critical_sec: 0.1,  // 100ms
            hop_count_delta_alert: 2,
            health_score_warning: 80.0,
            health_score_critical: 50.0,
        }
    }
}

/// DNS policy configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DnsPolicy {
    /// Normal refresh interval
    #[serde(with = "duration_serde")]
    pub refresh_interval: Duration,
    /// Minimum TTL to respect
    #[serde(with = "duration_serde")]
    pub min_ttl: Duration,
    /// Maximum TTL to respect
    #[serde(with = "duration_serde")]
    pub max_ttl: Duration,
    /// Use cached IP on DNS failure
    pub use_cached_on_failure: bool,
    /// Retry interval on failure
    #[serde(with = "duration_serde")]
    pub failure_retry_interval: Duration,
    /// Max retries on failure
    pub max_retries: u32,
    /// DNS resolvers (empty = system default)
    pub resolvers: Vec<String>,
    /// Resolver timeout
    #[serde(with = "duration_serde")]
    pub resolver_timeout: Duration,
}

impl Default for DnsPolicy {
    fn default() -> Self {
        Self {
            refresh_interval: Duration::from_secs(300),
            min_ttl: Duration::from_secs(60),
            max_ttl: Duration::from_secs(3600),
            use_cached_on_failure: true,
            failure_retry_interval: Duration::from_secs(10),
            max_retries: 6,
            resolvers: vec![
                "8.8.8.8".into(),
                "1.1.1.1".into(),
            ],
            resolver_timeout: Duration::from_secs(2),
        }
    }
}

/// Connection re-establishment policy
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionPolicy {
    /// Minimum connections in pool
    pub min_connections: usize,
    /// Maximum connections in pool
    pub max_connections: usize,
    /// Idle timeout before connection close
    #[serde(with = "duration_serde")]
    pub idle_timeout: Duration,
    /// Health check interval
    #[serde(with = "duration_serde")]
    pub health_check_interval: Duration,
    /// Health check timeout
    #[serde(with = "duration_serde")]
    pub health_check_timeout: Duration,
    /// Unhealthy threshold (failures before marking unhealthy)
    pub unhealthy_threshold: u32,
    /// Healthy threshold (successes before marking healthy)
    pub healthy_threshold: u32,
    /// Maximum connection age before refresh
    #[serde(with = "duration_serde")]
    pub max_age: Duration,
    /// Drain timeout when refreshing
    #[serde(with = "duration_serde")]
    pub drain_timeout: Duration,
    /// Cooldown between refreshes
    #[serde(with = "duration_serde")]
    pub refresh_cooldown: Duration,
}

impl Default for ConnectionPolicy {
    fn default() -> Self {
        Self {
            min_connections: 2,
            max_connections: 5,
            idle_timeout: Duration::from_secs(300),
            health_check_interval: Duration::from_secs(10),
            health_check_timeout: Duration::from_secs(5),
            unhealthy_threshold: 3,
            healthy_threshold: 2,
            max_age: Duration::from_secs(3600),
            drain_timeout: Duration::from_secs(30),
            refresh_cooldown: Duration::from_secs(30),
        }
    }
}

/// Failover policy configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FailoverPolicy {
    /// Enable automatic failover
    pub enabled: bool,
    /// Failover cooldown
    #[serde(with = "duration_serde")]
    pub cooldown: Duration,
    /// Verify candidate before failover
    pub verify_candidate: bool,
    /// Verification timeout
    #[serde(with = "duration_serde")]
    pub verify_timeout: Duration,
    /// Enable automatic failback to primary
    pub failback_enabled: bool,
    /// Primary recovery check interval
    #[serde(with = "duration_serde")]
    pub failback_check_interval: Duration,
    /// Primary stable duration before failback
    #[serde(with = "duration_serde")]
    pub failback_stable_duration: Duration,
    /// Circuit breaker failure threshold
    pub circuit_breaker_threshold: u32,
    /// Circuit breaker success threshold
    pub circuit_breaker_success: u32,
    /// Circuit breaker timeout
    #[serde(with = "duration_serde")]
    pub circuit_breaker_timeout: Duration,
}

impl Default for FailoverPolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            cooldown: Duration::from_secs(300),
            verify_candidate: true,
            verify_timeout: Duration::from_secs(5),
            failback_enabled: true,
            failback_check_interval: Duration::from_secs(60),
            failback_stable_duration: Duration::from_secs(300),
            circuit_breaker_threshold: 5,
            circuit_breaker_success: 3,
            circuit_breaker_timeout: Duration::from_secs(60),
        }
    }
}

/// Baseline calculation configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BaselineConfig {
    /// Window for baseline calculation
    #[serde(with = "duration_serde")]
    pub window: Duration,
    /// Recalculation interval
    #[serde(with = "duration_serde")]
    pub recalculate_interval: Duration,
    /// Sigma for outlier removal
    pub outlier_sigma: f64,
}

impl Default for BaselineConfig {
    fn default() -> Self {
        Self {
            window: Duration::from_secs(86400), // 24h
            recalculate_interval: Duration::from_secs(3600), // 1h
            outlier_sigma: 3.0,
        }
    }
}

// Serde helper for Duration (using milliseconds for simplicity)
mod duration_serde {
    use serde::{Deserialize, Deserializer, Serializer};
    use std::time::Duration;
    
    pub fn serialize<S>(duration: &Duration, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_u64(duration.as_millis() as u64)
    }
    
    pub fn deserialize<'de, D>(deserializer: D) -> Result<Duration, D::Error>
    where
        D: Deserializer<'de>,
    {
        let ms = u64::deserialize(deserializer)?;
        Ok(Duration::from_millis(ms))
    }
}

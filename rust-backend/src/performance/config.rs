//! Performance monitoring configuration
//!
//! Configurable thresholds, refresh rates, and alerting rules.

use serde::{Deserialize, Serialize};
use std::path::Path;

/// Performance monitoring configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PerfConfig {
    /// Refresh rate in Hz (5-60)
    #[serde(default = "default_refresh_hz")]
    pub refresh_hz: u32,

    /// Enable low-frequency mode (1 Hz) to reduce overhead
    #[serde(default)]
    pub low_freq_mode: bool,

    /// Latency thresholds (microseconds)
    #[serde(default)]
    pub thresholds: LatencyThresholds,

    /// Alert configuration
    #[serde(default)]
    pub alerts: AlertConfig,

    /// Time windows for percentile calculation (seconds)
    #[serde(default)]
    pub windows: TimeWindows,

    /// Queue monitoring
    #[serde(default)]
    pub queues: QueueConfig,

    /// Network monitoring
    #[serde(default)]
    pub network: NetworkConfig,

    /// Feature flags
    #[serde(default)]
    pub features: FeatureFlags,
}

fn default_refresh_hz() -> u32 {
    10
}

impl Default for PerfConfig {
    fn default() -> Self {
        Self {
            refresh_hz: 10,
            low_freq_mode: false,
            thresholds: LatencyThresholds::default(),
            alerts: AlertConfig::default(),
            windows: TimeWindows::default(),
            queues: QueueConfig::default(),
            network: NetworkConfig::default(),
            features: FeatureFlags::default(),
        }
    }
}

impl PerfConfig {
    /// Load from TOML file
    pub fn load(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let contents = std::fs::read_to_string(path)?;
        let config: Self = toml::from_str(&contents)?;
        Ok(config)
    }

    /// Load from environment or default path
    pub fn from_env() -> Self {
        let path =
            std::env::var("PERF_CONFIG_PATH").unwrap_or_else(|_| "perf_config.toml".to_string());

        Self::load(&path).unwrap_or_else(|e| {
            tracing::debug!("Using default perf config ({}): {}", path, e);
            Self::default()
        })
    }

    /// Save to TOML file
    pub fn save(&self, path: impl AsRef<Path>) -> anyhow::Result<()> {
        let contents = toml::to_string_pretty(self)?;
        std::fs::write(path, contents)?;
        Ok(())
    }
}

/// Latency thresholds for alerting
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LatencyThresholds {
    /// Tick-to-trade P99.9 threshold (μs) - triggers tail alarm
    #[serde(default = "default_t2t_p999_us")]
    pub t2t_p999_us: u64,

    /// Tick-to-trade P99 warning threshold (μs)
    #[serde(default = "default_t2t_p99_warn_us")]
    pub t2t_p99_warn_us: u64,

    /// Tick receive P99 threshold (μs)
    #[serde(default = "default_tick_recv_p99_us")]
    pub tick_recv_p99_us: u64,

    /// Signal generation P99 threshold (μs)
    #[serde(default = "default_signal_gen_p99_us")]
    pub signal_gen_p99_us: u64,

    /// Order execution P99 threshold (μs)
    #[serde(default = "default_order_exec_p99_us")]
    pub order_exec_p99_us: u64,

    /// Venue ack latency P99 threshold (μs)
    #[serde(default = "default_venue_ack_p99_us")]
    pub venue_ack_p99_us: u64,

    /// Jitter threshold (ns) - triggers spike alert
    #[serde(default = "default_jitter_threshold_ns")]
    pub jitter_threshold_ns: u64,
}

fn default_t2t_p999_us() -> u64 {
    10_000
} // 10ms
fn default_t2t_p99_warn_us() -> u64 {
    5_000
} // 5ms
fn default_tick_recv_p99_us() -> u64 {
    1_000
} // 1ms
fn default_signal_gen_p99_us() -> u64 {
    500
} // 500μs
fn default_order_exec_p99_us() -> u64 {
    2_000
} // 2ms
fn default_venue_ack_p99_us() -> u64 {
    50_000
} // 50ms
fn default_jitter_threshold_ns() -> u64 {
    1_000_000
} // 1ms

impl Default for LatencyThresholds {
    fn default() -> Self {
        Self {
            t2t_p999_us: default_t2t_p999_us(),
            t2t_p99_warn_us: default_t2t_p99_warn_us(),
            tick_recv_p99_us: default_tick_recv_p99_us(),
            signal_gen_p99_us: default_signal_gen_p99_us(),
            order_exec_p99_us: default_order_exec_p99_us(),
            venue_ack_p99_us: default_venue_ack_p99_us(),
            jitter_threshold_ns: default_jitter_threshold_ns(),
        }
    }
}

/// Alert configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlertConfig {
    /// Enable tail alarm when P99.9 exceeds threshold
    #[serde(default = "default_true")]
    pub tail_alarm_enabled: bool,

    /// Enable jitter spike alerts
    #[serde(default = "default_true")]
    pub jitter_alert_enabled: bool,

    /// Consecutive violations before alerting
    #[serde(default = "default_alert_count")]
    pub consecutive_violations: u32,

    /// Cooldown between alerts (seconds)
    #[serde(default = "default_alert_cooldown")]
    pub alert_cooldown_secs: u64,
}

fn default_true() -> bool {
    true
}
fn default_alert_count() -> u32 {
    3
}
fn default_alert_cooldown() -> u64 {
    60
}

impl Default for AlertConfig {
    fn default() -> Self {
        Self {
            tail_alarm_enabled: true,
            jitter_alert_enabled: true,
            consecutive_violations: 3,
            alert_cooldown_secs: 60,
        }
    }
}

/// Time windows for percentile calculations
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimeWindows {
    /// Short window (seconds)
    #[serde(default = "default_short_window")]
    pub short_secs: u64,

    /// Medium window (seconds)
    #[serde(default = "default_medium_window")]
    pub medium_secs: u64,

    /// Long window (seconds)
    #[serde(default = "default_long_window")]
    pub long_secs: u64,
}

fn default_short_window() -> u64 {
    1
}
fn default_medium_window() -> u64 {
    10
}
fn default_long_window() -> u64 {
    60
}

impl Default for TimeWindows {
    fn default() -> Self {
        Self {
            short_secs: 1,
            medium_secs: 10,
            long_secs: 60,
        }
    }
}

/// Queue monitoring configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueueConfig {
    /// Enable queue depth monitoring
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Queue depth warning threshold
    #[serde(default = "default_queue_warn")]
    pub depth_warn_threshold: usize,

    /// Queue depth critical threshold
    #[serde(default = "default_queue_crit")]
    pub depth_critical_threshold: usize,

    /// Queue wait time P99 threshold (μs)
    #[serde(default = "default_queue_wait_p99")]
    pub wait_time_p99_us: u64,
}

fn default_queue_warn() -> usize {
    100
}
fn default_queue_crit() -> usize {
    1000
}
fn default_queue_wait_p99() -> u64 {
    1000
}

impl Default for QueueConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            depth_warn_threshold: 100,
            depth_critical_threshold: 1000,
            wait_time_p99_us: 1000,
        }
    }
}

/// Network monitoring configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkConfig {
    /// Enable network stats collection
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Interfaces to monitor (empty = all)
    #[serde(default)]
    pub interfaces: Vec<String>,

    /// Drop rate warning threshold (%)
    #[serde(default = "default_drop_warn")]
    pub drop_rate_warn_pct: f64,

    /// Retransmit rate warning threshold (%)
    #[serde(default = "default_retrans_warn")]
    pub retrans_rate_warn_pct: f64,
}

fn default_drop_warn() -> f64 {
    0.01
}
fn default_retrans_warn() -> f64 {
    0.1
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            interfaces: Vec::new(),
            drop_rate_warn_pct: 0.01,
            retrans_rate_warn_pct: 0.1,
        }
    }
}

/// Feature flags for performance monitoring
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeatureFlags {
    /// Enable histogram collection (can be disabled for minimal overhead)
    #[serde(default = "default_true")]
    pub histograms: bool,

    /// Enable time series collection for dashboard
    #[serde(default = "default_true")]
    pub time_series: bool,

    /// Enable span tracing (detailed per-request traces)
    #[serde(default)]
    pub span_tracing: bool,

    /// Enable FPGA metrics collection
    #[serde(default)]
    pub fpga_metrics: bool,

    /// Enable kernel bypass metrics (DPDK/io_uring)
    #[serde(default)]
    pub kernel_bypass_metrics: bool,
}

impl Default for FeatureFlags {
    fn default() -> Self {
        Self {
            histograms: true,
            time_series: true,
            span_tracing: false,
            fpga_metrics: false,
            kernel_bypass_metrics: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = PerfConfig::default();
        assert_eq!(config.refresh_hz, 10);
        assert!(config.thresholds.t2t_p999_us > 0);
    }

    #[test]
    fn test_toml_roundtrip() {
        let config = PerfConfig::default();
        let toml = toml::to_string_pretty(&config).unwrap();
        let parsed: PerfConfig = toml::from_str(&toml).unwrap();
        assert_eq!(parsed.refresh_hz, config.refresh_hz);
    }
}

//! Performance TUI - Real-time HFT Performance Visualization
//!
//! A terminal-based performance monitoring system designed for:
//! - Sub-microsecond latency tracking
//! - Hardware-level monitoring (NIC, FPGA, CPU affinity)
//! - Real-time tick-to-trade waterfall analysis
//! - Lock-free metrics collection
//!
//! Architecture:
//! - Metrics collector runs on dedicated core (configurable)
//! - TUI renders at 60fps with minimal jitter
//! - Shared memory region for cross-process metrics

pub mod app;
pub mod hardware;
pub mod hft_metrics;
pub mod renderer;
pub mod widgets;

pub use app::PerfApp;
pub use hardware::HardwareMonitor;
pub use hft_metrics::HftMetricsCollector;

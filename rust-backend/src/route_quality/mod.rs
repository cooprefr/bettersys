//! Route Quality Monitor
//!
//! Continuous monitoring for detecting routing changes, packet loss, and latency drift
//! on paths to market data endpoints (Binance, etc.).
//!
//! Features:
//! - Multi-probe methodology (ICMP, TCP, TLS, DNS, traceroute)
//! - Baseline calculation with anomaly detection
//! - Automatic mitigation (DNS refresh, connection re-establishment, failover)
//! - Prometheus metrics export
//! - Integration hooks for main application
//!
//! See docs/ROUTE_QUALITY_MONITOR_SPEC.md for full specification.
//!
//! ## Usage
//!
//! ```rust,ignore
//! use betterbot_backend::route_quality::{
//!     RouteQualityConfig, RouteQualityMetrics, RouteQualityProber,
//!     MitigationController, RouteQualityIntegration,
//! };
//!
//! // Create integration for main app
//! let (integration, handle) = RouteQualityIntegration::new();
//!
//! // Listen for events in main app
//! tokio::spawn(async move {
//!     while let Some(event) = handle.recv_event().await {
//!         match event {
//!             RouteQualityEvent::Failover { from, to, .. } => {
//!                 // Reconnect WebSocket to new endpoint
//!             }
//!             _ => {}
//!         }
//!     }
//! });
//! ```

pub mod baseline;
pub mod config;
pub mod integration;
pub mod metrics;
pub mod mitigation;
pub mod prober;

pub use baseline::*;
pub use config::*;
pub use integration::*;
pub use metrics::*;
pub use mitigation::*;
pub use prober::*;

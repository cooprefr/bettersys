//! Middleware for observability and rate limiting.
//!
//! This module provides:
//! - Request logging with latency tracking
//! - Rate limiting per IP address
//! - Metrics collection for Prometheus

pub mod logging;
pub mod rate_limit;

pub use logging::{request_logging, request_logging_simple};
pub use rate_limit::{RateLimitConfig, RateLimitLayer};

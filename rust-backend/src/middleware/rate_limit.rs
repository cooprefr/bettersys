//! Rate limiting middleware.
//!
//! Simple in-memory rate limiting per IP address using a sliding window.

use axum::{
    body::Body,
    extract::ConnectInfo,
    http::{Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::warn;

/// Configuration for rate limiting.
#[derive(Clone)]
pub struct RateLimitConfig {
    /// Maximum requests per window.
    pub max_requests: u32,
    /// Window duration.
    pub window: Duration,
    /// Burst allowance (extra requests above limit before hard reject).
    pub burst: u32,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            max_requests: 100,
            window: Duration::from_secs(60),
            burst: 20,
        }
    }
}

/// Rate limiter state tracking requests per IP.
#[derive(Clone)]
pub struct RateLimitLayer {
    config: RateLimitConfig,
    state: Arc<Mutex<HashMap<IpAddr, RateLimitEntry>>>,
}

struct RateLimitEntry {
    count: u32,
    window_start: Instant,
}

impl RateLimitLayer {
    pub fn new(config: RateLimitConfig) -> Self {
        Self {
            config,
            state: Arc::new(Mutex::new(HashMap::new())),
        }
    }
    
    /// Check if request should be allowed.
    fn check(&self, ip: IpAddr) -> RateLimitResult {
        let mut state = self.state.lock();
        let now = Instant::now();
        
        let entry = state.entry(ip).or_insert(RateLimitEntry {
            count: 0,
            window_start: now,
        });
        
        // Reset window if expired
        if now.duration_since(entry.window_start) >= self.config.window {
            entry.count = 0;
            entry.window_start = now;
        }
        
        entry.count += 1;
        
        let limit = self.config.max_requests + self.config.burst;
        let remaining = limit.saturating_sub(entry.count);
        let reset_at = entry.window_start + self.config.window;
        
        if entry.count > limit {
            RateLimitResult::Exceeded {
                retry_after: reset_at.duration_since(now),
            }
        } else if entry.count > self.config.max_requests {
            RateLimitResult::BurstUsed { remaining }
        } else {
            RateLimitResult::Allowed { remaining }
        }
    }
    
    /// Periodic cleanup of old entries (call from a background task).
    pub fn cleanup(&self) {
        let mut state = self.state.lock();
        let now = Instant::now();
        let window = self.config.window;
        
        state.retain(|_, entry| {
            now.duration_since(entry.window_start) < window * 2
        });
    }
}

enum RateLimitResult {
    Allowed { remaining: u32 },
    BurstUsed { remaining: u32 },
    Exceeded { retry_after: Duration },
}

/// Rate limiting middleware function.
pub async fn rate_limit_middleware(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    axum::extract::State(limiter): axum::extract::State<RateLimitLayer>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let ip = addr.ip();
    
    match limiter.check(ip) {
        RateLimitResult::Allowed { .. } | RateLimitResult::BurstUsed { .. } => {
            next.run(request).await
        }
        RateLimitResult::Exceeded { retry_after } => {
            warn!(
                ip = %ip,
                retry_after_secs = retry_after.as_secs(),
                "Rate limit exceeded"
            );
            
            let body = serde_json::json!({
                "error": "rate_limit_exceeded",
                "message": "Too many requests. Please slow down.",
                "retry_after_seconds": retry_after.as_secs(),
            });
            
            (
                StatusCode::TOO_MANY_REQUESTS,
                [("Retry-After", retry_after.as_secs().to_string())],
                axum::Json(body),
            ).into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_rate_limit_allows_under_limit() {
        let config = RateLimitConfig {
            max_requests: 10,
            window: Duration::from_secs(60),
            burst: 5,
        };
        let limiter = RateLimitLayer::new(config);
        let ip: IpAddr = "127.0.0.1".parse().unwrap();
        
        for _ in 0..10 {
            match limiter.check(ip) {
                RateLimitResult::Allowed { .. } => {}
                _ => panic!("Should be allowed"),
            }
        }
    }
    
    #[test]
    fn test_rate_limit_allows_burst() {
        let config = RateLimitConfig {
            max_requests: 5,
            window: Duration::from_secs(60),
            burst: 3,
        };
        let limiter = RateLimitLayer::new(config);
        let ip: IpAddr = "127.0.0.1".parse().unwrap();
        
        // First 5 should be normal allowed
        for _ in 0..5 {
            match limiter.check(ip) {
                RateLimitResult::Allowed { .. } => {}
                _ => panic!("Should be allowed"),
            }
        }
        
        // Next 3 should use burst
        for _ in 0..3 {
            match limiter.check(ip) {
                RateLimitResult::BurstUsed { .. } => {}
                _ => panic!("Should be burst"),
            }
        }
        
        // 9th should be exceeded
        match limiter.check(ip) {
            RateLimitResult::Exceeded { .. } => {}
            _ => panic!("Should be exceeded"),
        }
    }
}

//! Request logging middleware.
//!
//! Logs every HTTP request with method, path, status code, and latency.

use axum::{
    body::Body,
    extract::ConnectInfo,
    http::{Request, StatusCode},
    middleware::Next,
    response::Response,
};
use std::net::SocketAddr;
use std::time::Instant;
use tracing::{info, warn, Span};

/// Middleware that logs HTTP requests with timing information.
///
/// Logs at INFO level for successful requests, WARN level for errors.
/// Includes: method, path, status code, latency in milliseconds.
pub async fn request_logging(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let method = request.method().clone();
    let uri = request.uri().clone();
    let path = uri.path().to_string();
    
    // Skip logging for health checks to reduce noise
    if path == "/health" {
        return next.run(request).await;
    }
    
    let start = Instant::now();
    
    // Create a span for this request
    let span = tracing::info_span!(
        "http_request",
        method = %method,
        path = %path,
        client_ip = %addr.ip(),
        status = tracing::field::Empty,
        latency_ms = tracing::field::Empty,
    );
    
    let _guard = span.enter();
    
    // Process the request
    let response = next.run(request).await;
    
    let latency = start.elapsed();
    let status = response.status().as_u16();
    
    // Record values in span
    Span::current().record("status", status);
    Span::current().record("latency_ms", latency.as_millis() as u64);
    
    // Log based on status code
    if status >= 500 {
        warn!(
            method = %method,
            path = %path,
            status = status,
            latency_ms = latency.as_millis(),
            client_ip = %addr.ip(),
            "Request failed (5xx)"
        );
    } else if status >= 400 {
        info!(
            method = %method,
            path = %path,
            status = status,
            latency_ms = latency.as_millis(),
            client_ip = %addr.ip(),
            "Request completed (4xx)"
        );
    } else {
        info!(
            method = %method,
            path = %path,
            status = status,
            latency_ms = latency.as_millis(),
            "Request completed"
        );
    }
    
    response
}

/// Simplified logging middleware without client address (for use without ConnectInfo).
pub async fn request_logging_simple(
    request: Request<Body>,
    next: Next,
) -> Response {
    let method = request.method().clone();
    let uri = request.uri().clone();
    let path = uri.path().to_string();
    
    // Skip logging for health checks
    if path == "/health" {
        return next.run(request).await;
    }
    
    let start = Instant::now();
    
    let response = next.run(request).await;
    
    let latency = start.elapsed();
    let status = response.status().as_u16();
    
    if status >= 500 {
        warn!(
            method = %method,
            path = %path,
            status = status,
            latency_ms = latency.as_millis(),
            "Request failed (5xx)"
        );
    } else {
        info!(
            method = %method,
            path = %path,
            status = status,
            latency_ms = latency.as_millis(),
            "Request completed"
        );
    }
    
    response
}

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Json, Response},
    routing::get,
    Router,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;

use crate::signals::Database;

/// Shared application state
#[derive(Clone)]
pub struct AppState {
    pub db: Arc<Database>,
}

/// Create the API router
pub fn create_router(db: Arc<Database>) -> Router {
    let state = AppState { db };

    Router::new()
        .route("/health", get(health_check))
        .route("/api/signals", get(get_signals))
        .route("/api/signals/:id", get(get_signal_by_id))
        .route("/api/stats", get(get_stats))
        .with_state(state)
}

// ===== Route Handlers =====

/// Health check endpoint
async fn health_check() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "healthy".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
    })
}

/// Get recent signals with optional filters
async fn get_signals(
    State(state): State<AppState>,
    Query(params): Query<SignalQuery>,
) -> Result<Json<SignalsResponse>, ApiError> {
    let signals = if let Some(signal_type) = params.signal_type {
        let limit = params.limit.unwrap_or(50).min(500);
        state.db.get_signals_by_type(&signal_type, limit as i32)?
    } else if let Some(hours) = params.hours {
        let limit = params.limit.unwrap_or(50).min(500);
        state.db.get_recent_signals(hours as i32, limit as i32)?
    } else {
        // Default: last 24 hours, max 50 signals
        let limit = params.limit.unwrap_or(50).min(500);
        state.db.get_recent_signals(24, limit as i32)?
    };

    Ok(Json(SignalsResponse {
        count: signals.len(),
        signals,
    }))
}

/// Get a specific signal by ID (efficient single-row lookup)
async fn get_signal_by_id(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<crate::models::Signal>, ApiError> {
    state.db
        .get_signal_by_id(id)?
        .map(Json)
        .ok_or(ApiError::NotFound(format!("Signal {} not found", id)))
}

/// Get statistics about signals
async fn get_stats(State(state): State<AppState>) -> Result<Json<serde_json::Value>, ApiError> {
    let stats = state.db.get_stats()?;
    Ok(Json(stats))
}

// ===== Request/Response Types =====

#[derive(Deserialize)]
struct SignalQuery {
    /// Filter by signal type ("insider_edge" or "arbitrage")
    signal_type: Option<String>,
    /// Get signals from last N hours
    hours: Option<u32>,
    /// Limit number of results
    limit: Option<u32>,
}

#[derive(Serialize)]
struct HealthResponse {
    status: String,
    version: String,
}

#[derive(Serialize)]
struct SignalsResponse {
    count: usize,
    signals: Vec<crate::models::Signal>,
}

// ===== Error Handling =====

#[derive(Debug)]
enum ApiError {
    Database(anyhow::Error),
    NotFound(String),
    #[allow(dead_code)] // Reserved for input validation
    BadRequest(String),
}

impl From<anyhow::Error> for ApiError {
    fn from(err: anyhow::Error) -> Self {
        ApiError::Database(err)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, message) = match &self {
            ApiError::Database(err) => {
                tracing::error!("Database error: {}", err);
                (StatusCode::INTERNAL_SERVER_ERROR, "Internal server error".to_string())
            }
            ApiError::NotFound(msg) => (StatusCode::NOT_FOUND, msg.clone()),
            ApiError::BadRequest(msg) => (StatusCode::BAD_REQUEST, msg.clone()),
        };

        let body = Json(json!({
            "error": message,
        }));

        (status, body).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_conversion() {
        let err = anyhow::anyhow!("Test error");
        let api_err: ApiError = err.into();
        
        match api_err {
            ApiError::Database(_) => (),
            _ => panic!("Expected Database error"),
        }
    }
}

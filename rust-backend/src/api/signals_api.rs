//! Signal API Endpoints
//! Mission: Expose signal intelligence through REST API
//! Philosophy: Fast, clear, actionable

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::Json,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::models::MarketSignal;
use crate::signals::{CompositeSignal, CorrelatorConfig, DbSignalStorage, SignalCorrelator};

/// Query parameters for listing signals
#[derive(Debug, Deserialize)]
pub struct SignalsQuery {
    #[serde(default = "default_limit")]
    pub limit: usize,
    #[serde(default)]
    pub min_confidence: Option<f64>,
}

fn default_limit() -> usize {
    50
}

/// Response for signals list endpoint
#[derive(Debug, Serialize)]
pub struct SignalsResponse {
    pub signals: Vec<MarketSignal>,
    pub total: usize,
}

/// Response for composite signals
#[derive(Debug, Serialize)]
pub struct CompositeSignalsResponse {
    pub composite_signals: Vec<CompositeSignal>,
    pub count: usize,
    pub scan_time: String,
}

/// GET /api/v1/signals
/// List recent signals with optional filtering
pub async fn list_signals(
    State(storage): State<Arc<DbSignalStorage>>,
    Query(params): Query<SignalsQuery>,
) -> Result<Json<SignalsResponse>, StatusCode> {
    let mut signals = storage
        .get_recent(params.limit)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // Filter by confidence if specified
    if let Some(min_conf) = params.min_confidence {
        signals.retain(|s| s.confidence >= min_conf);
    }

    let total = signals.len();

    Ok(Json(SignalsResponse { signals, total }))
}

/// GET /api/v1/signals/:id
/// Get a specific signal by ID
pub async fn get_signal(
    State(storage): State<Arc<DbSignalStorage>>,
    Path(id): Path<String>,
) -> Result<Json<MarketSignal>, StatusCode> {
    let signals = storage
        .get_recent(1000)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    signals
        .into_iter()
        .find(|s| s.id == id)
        .map(Json)
        .ok_or(StatusCode::NOT_FOUND)
}

/// GET /api/v1/signals/market/:slug
/// Get signals for a specific market
pub async fn get_market_signals(
    State(storage): State<Arc<DbSignalStorage>>,
    Path(slug): Path<String>,
) -> Result<Json<SignalsResponse>, StatusCode> {
    let signals = storage
        .get_by_market(&slug, 100)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let total = signals.len();

    Ok(Json(SignalsResponse { signals, total }))
}

/// GET /api/v1/signals/composite
/// Get composite signals (patterns detected across multiple signals)
pub async fn get_composite_signals(
    State(storage): State<Arc<DbSignalStorage>>,
) -> Result<Json<CompositeSignalsResponse>, StatusCode> {
    let correlator = SignalCorrelator::new(storage, CorrelatorConfig::default());

    let composite_signals = correlator
        .analyze_correlations()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let count = composite_signals.len();
    let scan_time = chrono::Utc::now().to_rfc3339();

    Ok(Json(CompositeSignalsResponse {
        composite_signals,
        count,
        scan_time,
    }))
}

/// GET /api/v1/signals/stats
/// Get signal statistics
#[derive(Debug, Serialize)]
pub struct SignalStats {
    pub total_signals: usize,
    pub signals_by_type: std::collections::HashMap<String, usize>,
    pub avg_confidence: f64,
    pub high_confidence_count: usize, // >= 0.80
}

pub async fn get_signal_stats(
    State(storage): State<Arc<DbSignalStorage>>,
) -> Result<Json<SignalStats>, StatusCode> {
    let signals = storage
        .get_recent(1000)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let total_signals = signals.len();

    let mut signals_by_type = std::collections::HashMap::new();
    let mut total_confidence = 0.0;
    let mut high_confidence_count = 0;

    for signal in &signals {
        let type_name = format!("{:?}", signal.signal_type);
        *signals_by_type.entry(type_name).or_insert(0) += 1;

        total_confidence += signal.confidence;

        if signal.confidence >= 0.80 {
            high_confidence_count += 1;
        }
    }

    let avg_confidence = if total_signals > 0 {
        total_confidence / total_signals as f64
    } else {
        0.0
    };

    Ok(Json(SignalStats {
        total_signals,
        signals_by_type,
        avg_confidence,
        high_confidence_count,
    }))
}

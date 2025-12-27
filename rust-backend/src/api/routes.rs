//! API Routes
//! Pilot in Command: API Interface
//! Mission: Expose high-performance endpoints for signal consumption

#![allow(dead_code, unused_imports, unused_variables)]

use anyhow::Result;
use axum::{
    extract::{Query, State as AxumState},
    http::StatusCode,
    response::Json,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{
    backtest::{BacktestConfig, BacktestEngine},
    models::MarketSignal,
    risk::RiskStats,
    AppState,
};

#[derive(Debug, Deserialize)]
pub struct SignalQuery {
    pub limit: Option<usize>,
    pub signal_type: Option<String>,
    pub min_confidence: Option<f64>,
}

#[derive(Debug, Serialize)]
pub struct SignalResponse {
    pub signals: Vec<MarketSignal>,
    pub count: usize,
    pub timestamp: String,
}

/// Get signals with optional filtering
pub async fn get_signals(
    Query(params): Query<SignalQuery>,
    AxumState(state): AxumState<AppState>,
) -> Result<Json<SignalResponse>, StatusCode> {
    let limit = params.limit.unwrap_or(100);
    let min_confidence = params.min_confidence.unwrap_or(0.0);

    let all_signals = state.signal_storage.get_recent(limit).unwrap_or_default();

    // Filter signals by confidence
    let filtered_signals: Vec<MarketSignal> = all_signals
        .into_iter()
        .filter(|s| s.confidence >= min_confidence)
        .collect();

    Ok(Json(SignalResponse {
        count: filtered_signals.len(),
        signals: filtered_signals,
        timestamp: Utc::now().to_rfc3339(),
    }))
}

#[derive(Debug, Deserialize)]
pub struct BacktestRequest {
    pub initial_bankroll: f64,
    pub kelly_fraction: f64,
    pub start_date: String,
    pub end_date: String,
    pub slippage_bps: f64,
    pub transaction_cost: f64,
    pub max_positions: usize,
    #[serde(default)]
    pub walk_forward_window_days: Option<i64>,
    #[serde(default)]
    pub test_window_days: Option<i64>,
    #[serde(default)]
    pub embargo_hours: Option<i64>,
    #[serde(default)]
    pub min_training_signals: Option<usize>,
}

#[derive(Debug, Serialize)]
pub struct BacktestResponse {
    pub total_pnl: f64,
    pub win_rate: f64,
    pub sharpe_ratio: f64,
    pub max_drawdown: f64,
    pub total_trades: usize,
    pub profit_factor: f64,
}

/// Run backtest with provided configuration
pub async fn run_backtest_handler(
    Json(request): Json<BacktestRequest>,
    AxumState(state): AxumState<AppState>,
) -> Result<Json<BacktestResponse>, StatusCode> {
    // Parse dates
    let start_date = DateTime::parse_from_rfc3339(&request.start_date)
        .map_err(|_| StatusCode::BAD_REQUEST)?
        .with_timezone(&Utc);

    let end_date = DateTime::parse_from_rfc3339(&request.end_date)
        .map_err(|_| StatusCode::BAD_REQUEST)?
        .with_timezone(&Utc);

    let config = BacktestConfig {
        initial_bankroll: request.initial_bankroll,
        kelly_fraction: request.kelly_fraction,
        start_date,
        end_date,
        slippage_bps: request.slippage_bps,
        transaction_cost: request.transaction_cost,
        max_positions: request.max_positions,
        walk_forward_window_days: request.walk_forward_window_days.unwrap_or(30),
        test_window_days: request.test_window_days.unwrap_or(7),
        embargo_hours: request.embargo_hours.unwrap_or(12),
        min_training_signals: request.min_training_signals.unwrap_or(25),
    };

    let mut engine = BacktestEngine::new(config);

    // Get historical signals (Phase 2: Direct DB call)
    let signals = state.signal_storage.get_recent(10000).unwrap_or_default();

    let result = engine
        .run(signals)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(BacktestResponse {
        total_pnl: result.total_pnl,
        win_rate: result.win_rate,
        sharpe_ratio: result.sharpe_ratio,
        max_drawdown: result.max_drawdown,
        total_trades: result.total_trades,
        profit_factor: result.profit_factor,
    }))
}

#[derive(Debug, Serialize)]
pub struct RiskStatsResponse {
    pub var_95: f64,
    pub cvar_95: f64,
    pub current_bankroll: f64,
    pub kelly_fraction: f64,
    pub win_rate: f64,
    pub sample_size: usize,
}

/// Get current risk statistics
pub async fn get_risk_stats_handler(
    AxumState(state): AxumState<AppState>,
) -> Result<Json<RiskStatsResponse>, StatusCode> {
    let risk_manager = state.risk_manager.read(); // parking_lot - no await needed

    let var_stats = risk_manager.var.get_stats();
    let win_rate = risk_manager.kelly.get_win_rate();

    Ok(Json(RiskStatsResponse {
        var_95: var_stats.var_95,
        cvar_95: var_stats.cvar_95,
        current_bankroll: risk_manager.kelly.bankroll,
        kelly_fraction: risk_manager.kelly.fraction,
        win_rate,
        sample_size: var_stats.sample_size,
    }))
}

/// WebSocket message types
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum WsMessage {
    Signal(MarketSignal),
    RiskUpdate(RiskStats),
    Heartbeat { timestamp: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_signal_filtering() {
        // Add test cases
    }
}

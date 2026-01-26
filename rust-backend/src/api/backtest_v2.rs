//! Backtest V2 API Endpoints
//!
//! Read-only API for accessing backtest run artifacts.
//! All endpoints return trust_level and disclaimers - the UI cannot omit them.
//!
//! # Endpoints
//!
//! - `GET /api/v2/backtest/runs` - List runs with filters
//! - `GET /api/v2/backtest/runs/:run_id` - Get run summary
//! - `GET /api/v2/backtest/runs/:run_id/manifest` - Get full manifest
//! - `GET /api/v2/backtest/runs/:run_id/equity` - Get equity curve
//! - `GET /api/v2/backtest/runs/:run_id/drawdown` - Get drawdown series
//! - `GET /api/v2/backtest/runs/:run_id/window-pnl` - Get per-window PnL
//! - `GET /api/v2/backtest/runs/:run_id/distributions` - Get distribution histograms
//!
//! # ETag Support
//!
//! All single-run endpoints return ETag headers derived from the manifest hash.
//! Clients can use If-None-Match for conditional requests.

use crate::backtest_v2::{
    ArtifactResponse, ArtifactStore, ListRunsFilter, ListRunsResponse, MethodologyCapsule,
    RunArtifact, RunDistributions, RunId, RunManifest, RunSortField, RunSummary, 
    RunTimeSeries, SortOrder, TrustLevelDto, TrustStatus, RUN_ARTIFACT_API_VERSION,
};
use axum::{
    extract::{Path, Query, State as AxumState},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Json, Response},
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::{debug, warn};

/// Shared state for backtest v2 API.
/// This should be added to AppState.
pub struct BacktestV2State {
    pub artifact_store: Arc<ArtifactStore>,
}

// =============================================================================
// RESPONSE HELPERS
// =============================================================================

/// Add ETag and cache headers to a response.
fn with_etag_headers<T: Serialize>(
    data: T,
    etag: &str,
    request_headers: &HeaderMap,
) -> Response {
    // Check If-None-Match
    if let Some(if_none_match) = request_headers.get(header::IF_NONE_MATCH) {
        if let Ok(value) = if_none_match.to_str() {
            if value == etag || value == "*" {
                return StatusCode::NOT_MODIFIED.into_response();
            }
        }
    }
    
    let json = Json(data);
    let mut response = json.into_response();
    
    response.headers_mut().insert(
        header::ETAG,
        etag.parse().unwrap_or_else(|_| "\"unknown\"".parse().unwrap()),
    );
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        "public, max-age=31536000, immutable".parse().unwrap(), // 1 year, immutable
    );
    
    response
}

/// Create an error response with proper status code.
fn error_response(status: StatusCode, message: &str) -> Response {
    let body = serde_json::json!({
        "error": message,
        "api_version": RUN_ARTIFACT_API_VERSION,
    });
    (status, Json(body)).into_response()
}

// =============================================================================
// LIST RUNS
// =============================================================================

/// Query parameters for listing runs.
#[derive(Debug, Deserialize)]
pub struct ListRunsQuery {
    /// Filter by strategy name.
    pub strategy_name: Option<String>,
    /// Filter by trust status.
    pub trusted_only: Option<bool>,
    /// Filter by production grade.
    pub production_grade_only: Option<bool>,
    /// Show only published runs (default: false for internal view).
    pub published_only: Option<bool>,
    /// Show only certified runs.
    pub certified_only: Option<bool>,
    /// Include internal/test runs.
    pub include_internal: Option<bool>,
    /// Filter by minimum PnL.
    pub min_pnl: Option<f64>,
    /// Filter by date range (start timestamp).
    pub after: Option<i64>,
    /// Filter by date range (end timestamp).
    pub before: Option<i64>,
    /// Page number (0-indexed).
    pub page: Option<usize>,
    /// Page size (default 20, max 100).
    pub page_size: Option<usize>,
    /// Sort field (default: persisted_at).
    pub sort_by: Option<RunSortField>,
    /// Sort direction (default: desc).
    pub sort_order: Option<SortOrder>,
}

impl From<ListRunsQuery> for ListRunsFilter {
    fn from(q: ListRunsQuery) -> Self {
        Self {
            strategy_name: q.strategy_name,
            trusted_only: q.trusted_only,
            production_grade_only: q.production_grade_only,
            published_only: q.published_only,
            certified_only: q.certified_only,
            include_internal: q.include_internal,
            min_pnl: q.min_pnl,
            after: q.after,
            before: q.before,
            page: q.page,
            page_size: q.page_size,
            sort_by: q.sort_by,
            sort_order: q.sort_order,
        }
    }
}

/// GET /api/v2/backtest/runs - List runs with filters
pub async fn list_runs(
    AxumState(state): AxumState<Arc<BacktestV2State>>,
    Query(query): Query<ListRunsQuery>,
) -> Response {
    let filter = ListRunsFilter::from(query);
    
    match state.artifact_store.list(&filter) {
        Ok(response) => Json(response).into_response(),
        Err(e) => {
            warn!("Failed to list runs: {}", e);
            error_response(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string())
        }
    }
}

// =============================================================================
// GET RUN
// =============================================================================

/// GET /api/v2/backtest/runs/:run_id - Get run summary
pub async fn get_run_summary(
    AxumState(state): AxumState<Arc<BacktestV2State>>,
    Path(run_id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let run_id = RunId(run_id);
    
    // Get manifest hash for ETag
    let etag = match state.artifact_store.get_manifest_hash(&run_id) {
        Ok(Some(hash)) => format!("\"{}\"", hash),
        Ok(None) => return error_response(StatusCode::NOT_FOUND, "Run not found"),
        Err(e) => {
            warn!("Failed to get manifest hash: {}", e);
            return error_response(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string());
        }
    };
    
    // Get full artifact for response wrapper
    match state.artifact_store.get(&run_id) {
        Ok(Some(artifact)) => {
            let summary = RunSummary::from_artifact(&artifact);
            let response = ArtifactResponse::new(&artifact, summary);
            with_etag_headers(response, &etag, &headers)
        }
        Ok(None) => error_response(StatusCode::NOT_FOUND, "Run not found"),
        Err(e) => {
            warn!("Failed to get run: {}", e);
            error_response(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string())
        }
    }
}

/// GET /api/v2/backtest/runs/:run_id/manifest - Get full manifest
pub async fn get_run_manifest(
    AxumState(state): AxumState<Arc<BacktestV2State>>,
    Path(run_id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let run_id = RunId(run_id);
    
    match state.artifact_store.get(&run_id) {
        Ok(Some(artifact)) => {
            let etag = artifact.etag();
            let response = ArtifactResponse::new(&artifact, artifact.manifest.clone());
            with_etag_headers(response, &etag, &headers)
        }
        Ok(None) => error_response(StatusCode::NOT_FOUND, "Run not found"),
        Err(e) => {
            warn!("Failed to get run manifest: {}", e);
            error_response(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string())
        }
    }
}

// =============================================================================
// TIME SERIES ENDPOINTS
// =============================================================================

/// Query parameters for time series endpoints.
#[derive(Debug, Deserialize)]
pub struct TimeSeriesQuery {
    /// Maximum number of data points to return (for downsampling).
    /// If not specified, returns all points.
    pub points: Option<usize>,
}

/// Downsample an equity curve to a maximum number of points.
/// Uses simple interval sampling (keeps first, last, and evenly spaced points).
fn downsample_equity_curve(
    artifact: &RunArtifact,
    max_points: Option<usize>,
) -> Option<Vec<crate::backtest_v2::run_artifact::EquityPoint>> {
    let points = artifact.time_series.equity_curve.as_ref()?;
    
    let max_points = match max_points {
        Some(n) if n > 0 && n < points.len() => n,
        _ => return Some(points.clone()),
    };
    
    if points.len() <= 1 {
        return Some(points.clone());
    }
    
    // Simple downsampling: keep first, last, and evenly spaced points
    let n = points.len();
    let step = (n as f64) / (max_points as f64 - 1.0);
    
    let mut sampled_points = Vec::with_capacity(max_points);
    for i in 0..max_points {
        let idx = if i == max_points - 1 {
            n - 1
        } else {
            (i as f64 * step) as usize
        };
        sampled_points.push(points[idx].clone());
    }
    
    Some(sampled_points)
}

/// GET /api/v2/backtest/runs/:run_id/equity - Get equity curve
pub async fn get_run_equity(
    AxumState(state): AxumState<Arc<BacktestV2State>>,
    Path(run_id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let run_id = RunId(run_id);
    
    match state.artifact_store.get(&run_id) {
        Ok(Some(artifact)) => {
            let etag = artifact.etag();
            let equity = artifact.time_series.equity_curve.clone();
            let response = ArtifactResponse::new(&artifact, equity);
            with_etag_headers(response, &etag, &headers)
        }
        Ok(None) => error_response(StatusCode::NOT_FOUND, "Run not found"),
        Err(e) => {
            warn!("Failed to get equity curve: {}", e);
            error_response(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string())
        }
    }
}

/// GET /api/v2/backtest/runs/:run_id/drawdown - Get drawdown series
pub async fn get_run_drawdown(
    AxumState(state): AxumState<Arc<BacktestV2State>>,
    Path(run_id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let run_id = RunId(run_id);
    
    match state.artifact_store.get(&run_id) {
        Ok(Some(artifact)) => {
            let etag = artifact.etag();
            let drawdown = artifact.time_series.drawdown_series.clone();
            let response = ArtifactResponse::new(&artifact, drawdown);
            with_etag_headers(response, &etag, &headers)
        }
        Ok(None) => error_response(StatusCode::NOT_FOUND, "Run not found"),
        Err(e) => {
            warn!("Failed to get drawdown series: {}", e);
            error_response(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string())
        }
    }
}

/// GET /api/v2/backtest/runs/:run_id/window-pnl - Get per-window PnL
pub async fn get_run_window_pnl(
    AxumState(state): AxumState<Arc<BacktestV2State>>,
    Path(run_id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let run_id = RunId(run_id);
    
    match state.artifact_store.get(&run_id) {
        Ok(Some(artifact)) => {
            let etag = artifact.etag();
            let window_pnl = artifact.time_series.window_pnl.clone();
            let response = ArtifactResponse::new(&artifact, window_pnl);
            with_etag_headers(response, &etag, &headers)
        }
        Ok(None) => error_response(StatusCode::NOT_FOUND, "Run not found"),
        Err(e) => {
            warn!("Failed to get window PnL: {}", e);
            error_response(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string())
        }
    }
}

// =============================================================================
// DISTRIBUTIONS
// =============================================================================

/// GET /api/v2/backtest/runs/:run_id/distributions - Get distribution histograms
pub async fn get_run_distributions(
    AxumState(state): AxumState<Arc<BacktestV2State>>,
    Path(run_id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let run_id = RunId(run_id);
    
    match state.artifact_store.get(&run_id) {
        Ok(Some(artifact)) => {
            let etag = artifact.etag();
            let distributions = artifact.distributions.clone();
            let response = ArtifactResponse::new(&artifact, distributions);
            with_etag_headers(response, &etag, &headers)
        }
        Ok(None) => error_response(StatusCode::NOT_FOUND, "Run not found"),
        Err(e) => {
            warn!("Failed to get distributions: {}", e);
            error_response(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string())
        }
    }
}

// =============================================================================
// FULL RESULTS (for deep audit)
// =============================================================================

/// GET /api/v2/backtest/runs/:run_id/full - Get full artifact (for audit)
pub async fn get_run_full(
    AxumState(state): AxumState<Arc<BacktestV2State>>,
    Path(run_id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let run_id = RunId(run_id);
    
    match state.artifact_store.get(&run_id) {
        Ok(Some(artifact)) => {
            let etag = artifact.etag();
            // Return full artifact (large payload - use with caution)
            with_etag_headers(artifact, &etag, &headers)
        }
        Ok(None) => error_response(StatusCode::NOT_FOUND, "Run not found"),
        Err(e) => {
            warn!("Failed to get full artifact: {}", e);
            error_response(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string())
        }
    }
}

// =============================================================================
// STORE STATS
// =============================================================================

/// Response for store statistics.
#[derive(Debug, Serialize)]
pub struct StoreStatsResponse {
    pub api_version: String,
    pub total_runs: u64,
    pub trusted_runs: u64,
    pub production_runs: u64,
    pub total_size_bytes: u64,
}

/// GET /api/v2/backtest/stats - Get store statistics
pub async fn get_store_stats(
    AxumState(state): AxumState<Arc<BacktestV2State>>,
) -> Response {
    match state.artifact_store.stats() {
        Ok(stats) => {
            let response = StoreStatsResponse {
                api_version: RUN_ARTIFACT_API_VERSION.to_string(),
                total_runs: stats.total_runs,
                trusted_runs: stats.trusted_runs,
                production_runs: stats.production_runs,
                total_size_bytes: stats.total_size_bytes,
            };
            Json(response).into_response()
        }
        Err(e) => {
            warn!("Failed to get store stats: {}", e);
            error_response(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string())
        }
    }
}

// =============================================================================
// PUBLIC API ENDPOINTS (no authentication required - published runs only)
// =============================================================================

/// GET /api/public/v2/backtest/runs - List published runs only (public API)
pub async fn list_runs_public(
    AxumState(state): AxumState<Arc<BacktestV2State>>,
    Query(query): Query<ListRunsQuery>,
) -> Response {
    let filter = ListRunsFilter::from(query);
    
    match state.artifact_store.list_published(&filter) {
        Ok(response) => Json(response).into_response(),
        Err(e) => {
            warn!("Failed to list published runs: {}", e);
            error_response(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string())
        }
    }
}

/// GET /api/public/v2/backtest/runs/:run_id - Get published run summary (public API)
pub async fn get_run_summary_public(
    AxumState(state): AxumState<Arc<BacktestV2State>>,
    Path(run_id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let run_id = RunId(run_id);
    
    // Use get_if_published - returns None if not published
    match state.artifact_store.get_if_published(&run_id) {
        Ok(Some(artifact)) => {
            let etag = artifact.etag();
            let summary = RunSummary::from_artifact(&artifact);
            let response = ArtifactResponse::new(&artifact, summary);
            with_etag_headers(response, &etag, &headers)
        }
        Ok(None) => error_response(StatusCode::NOT_FOUND, "Run not found or not published"),
        Err(e) => {
            warn!("Failed to get published run: {}", e);
            error_response(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string())
        }
    }
}

/// GET /api/public/v2/backtest/runs/:run_id/manifest - Get published run manifest (public API)
pub async fn get_run_manifest_public(
    AxumState(state): AxumState<Arc<BacktestV2State>>,
    Path(run_id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let run_id = RunId(run_id);
    
    match state.artifact_store.get_if_published(&run_id) {
        Ok(Some(artifact)) => {
            let etag = artifact.etag();
            let response = ArtifactResponse::new(&artifact, artifact.manifest.clone());
            with_etag_headers(response, &etag, &headers)
        }
        Ok(None) => error_response(StatusCode::NOT_FOUND, "Run not found or not published"),
        Err(e) => {
            warn!("Failed to get published run manifest: {}", e);
            error_response(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string())
        }
    }
}

/// GET /api/public/v2/backtest/runs/:run_id/equity - Get published run equity curve (public API)
pub async fn get_run_equity_public(
    AxumState(state): AxumState<Arc<BacktestV2State>>,
    Path(run_id): Path<String>,
    Query(query): Query<TimeSeriesQuery>,
    headers: HeaderMap,
) -> Response {
    let run_id = RunId(run_id);
    
    match state.artifact_store.get_if_published(&run_id) {
        Ok(Some(artifact)) => {
            let etag = artifact.etag();
            let equity = downsample_equity_curve(&artifact, query.points);
            let response = ArtifactResponse::new(&artifact, equity);
            with_etag_headers(response, &etag, &headers)
        }
        Ok(None) => error_response(StatusCode::NOT_FOUND, "Run not found or not published"),
        Err(e) => {
            warn!("Failed to get published run equity: {}", e);
            error_response(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string())
        }
    }
}

/// GET /api/public/v2/backtest/runs/:run_id/full - Get full published artifact (public API)
pub async fn get_run_full_public(
    AxumState(state): AxumState<Arc<BacktestV2State>>,
    Path(run_id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let run_id = RunId(run_id);
    
    match state.artifact_store.get_if_published(&run_id) {
        Ok(Some(artifact)) => {
            let etag = artifact.etag();
            let response = ArtifactResponse::new(&artifact, &artifact);
            with_etag_headers(response, &etag, &headers)
        }
        Ok(None) => error_response(StatusCode::NOT_FOUND, "Run not found or not published"),
        Err(e) => {
            warn!("Failed to get published run full artifact: {}", e);
            error_response(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string())
        }
    }
}

// =============================================================================
// ROUTER
// =============================================================================

use axum::routing::get;
use axum::Router;

/// Create the backtest v2 router (authenticated - all runs).
/// 
/// # Usage
/// 
/// ```ignore
/// let artifact_store = Arc::new(ArtifactStore::new("backtest_artifacts.db")?);
/// let backtest_v2_state = Arc::new(BacktestV2State { artifact_store });
/// 
/// let app = Router::new()
///     .nest("/api/v2/backtest", backtest_v2_router())
///     .with_state(backtest_v2_state);
/// ```
pub fn backtest_v2_router() -> Router<Arc<BacktestV2State>> {
    Router::new()
        .route("/runs", get(list_runs))
        .route("/runs/:run_id", get(get_run_summary))
        .route("/runs/:run_id/manifest", get(get_run_manifest))
        .route("/runs/:run_id/equity", get(get_run_equity))
        .route("/runs/:run_id/drawdown", get(get_run_drawdown))
        .route("/runs/:run_id/window-pnl", get(get_run_window_pnl))
        .route("/runs/:run_id/distributions", get(get_run_distributions))
        .route("/runs/:run_id/full", get(get_run_full))
        .route("/stats", get(get_store_stats))
}

/// Create the public backtest v2 router (no authentication - published runs only).
/// 
/// # Usage
/// 
/// ```ignore
/// let app = Router::new()
///     .nest("/api/public/v2/backtest", backtest_v2_public_router())
///     .with_state(backtest_v2_state);
/// ```
pub fn backtest_v2_public_router() -> Router<Arc<BacktestV2State>> {
    Router::new()
        .route("/runs", get(list_runs_public))
        .route("/runs/:run_id", get(get_run_summary_public))
        .route("/runs/:run_id/manifest", get(get_run_manifest_public))
        .route("/runs/:run_id/equity", get(get_run_equity_public))
        .route("/runs/:run_id/full", get(get_run_full_public))
        // Note: /stats, /drawdown, /window-pnl, /distributions NOT exposed in public API
}

// =============================================================================
// TESTS
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backtest_v2::{
        ConfigSummary, DatasetMetadata, RunDistributions, StrategyIdentity,
        TimeRangeSummary, TrustDecisionSummary, RUN_ARTIFACT_STORAGE_VERSION,
    };
    use crate::backtest_v2::fingerprint::{
        BehaviorFingerprint, CodeFingerprint, ConfigFingerprint, DatasetFingerprint,
        RunFingerprint, SeedFingerprint, StrategyFingerprint,
    };
    use crate::backtest_v2::orchestrator::BacktestResults;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    fn make_test_fingerprint(hash_hex: &str) -> RunFingerprint {
        RunFingerprint {
            version: "RUNFP_V2".to_string(),
            strategy: StrategyFingerprint::default(),
            code: CodeFingerprint::new(),
            config: ConfigFingerprint {
                settlement_reference_rule: None,
                settlement_tie_rule: None,
                chainlink_feed_id: None,
                oracle_chain_id: None,
                oracle_feed_proxies: vec![],
                oracle_decimals: vec![],
                oracle_visibility_rule: None,
                oracle_rounding_policy: None,
                oracle_config_hash: None,
                latency_model: "Fixed".to_string(),
                order_latency_ns: None,
                oms_parity_mode: "Full".to_string(),
                maker_fill_model: "Disabled".to_string(),
                integrity_policy: "Strict".to_string(),
                invariant_mode: "Hard".to_string(),
                fee_rate_bps: None,
                strategy_params_hash: 0,
                arrival_policy: "RecordedArrival".to_string(),
                strict_accounting: true,
                production_grade: true,
                allow_non_production: false,
                hash: 0,
            },
            dataset: DatasetFingerprint {
                classification: "FullIncremental".to_string(),
                readiness: "MakerViable".to_string(),
                orderbook_type: "FullIncrementalL2DeltasWithExchangeSeq".to_string(),
                trade_type: "TradePrints".to_string(),
                arrival_semantics: "RecordedArrival".to_string(),
                streams: vec![],
                hash: 0,
            },
            seed: SeedFingerprint::new(42),
            behavior: BehaviorFingerprint {
                event_count: 1000,
                hash: 0xDEAD,
            },
            registry: None,
            hash: 0,
            hash_hex: hash_hex.to_string(),
        }
    }

    fn make_test_artifact(run_id: &str) -> RunArtifact {
        let fingerprint = make_test_fingerprint(&run_id.replace("run_", ""));
        
        let manifest = RunManifest {
            schema_version: RUN_ARTIFACT_STORAGE_VERSION,
            run_id: RunId(run_id.to_string()),
            persisted_at: 1234567890,
            fingerprint,
            strategy: StrategyIdentity {
                name: "test_strategy".to_string(),
                version: "1.0.0".to_string(),
                code_hash: None,
            },
            dataset: DatasetMetadata {
                classification: "FullIncremental".to_string(),
                readiness: "MakerViable".to_string(),
                events_processed: 1000,
                delta_events_processed: 500,
                time_range: TimeRangeSummary {
                    start_ns: 0,
                    end_ns: 1_000_000_000,
                    duration_ns: 1_000_000_000,
                },
            },
            config_summary: ConfigSummary {
                production_grade: true,
                strict_mode: true,
                strict_accounting: true,
                maker_fill_model: "Disabled".to_string(),
                oms_parity_mode: "Full".to_string(),
                seed: 42,
            },
            trust_decision: TrustDecisionSummary {
                verdict: "Trusted".to_string(),
                trust_level: TrustLevelDto {
                    status: TrustStatus::Trusted,
                    reasons: vec![],
                },
                is_trusted: true,
                failure_reasons: vec![],
            },
            disclaimers: vec![],
            methodology_capsule: MethodologyCapsule {
                version: "v1".to_string(),
                summary: "Test capsule".to_string(),
                details: vec![],
                input_hash: "0".to_string(),
            },
            manifest_hash: "abcd1234".to_string(),
        };
        
        RunArtifact {
            manifest,
            results: BacktestResults::default(),
            time_series: RunTimeSeries {
                equity_curve: None,
                drawdown_series: None,
                window_pnl: None,
                pnl_history: vec![],
            },
            distributions: RunDistributions {
                trade_pnl_bins: vec![],
                trade_size_bins: vec![],
                hold_time_bins: vec![],
                slippage_bins: vec![],
            },
        }
    }

    #[tokio::test]
    async fn test_list_runs_empty() {
        let store = ArtifactStore::in_memory().unwrap();
        let state = Arc::new(BacktestV2State {
            artifact_store: Arc::new(store),
        });
        
        let app = backtest_v2_router().with_state(state);
        
        let response = app
            .oneshot(Request::builder().uri("/runs").body(Body::empty()).unwrap())
            .await
            .unwrap();
        
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_get_run_not_found() {
        let store = ArtifactStore::in_memory().unwrap();
        let state = Arc::new(BacktestV2State {
            artifact_store: Arc::new(store),
        });
        
        let app = backtest_v2_router().with_state(state);
        
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/runs/run_nonexistent")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_get_run_with_etag() {
        let store = ArtifactStore::in_memory().unwrap();
        let artifact = make_test_artifact("run_etag_test");
        store.persist(&artifact).unwrap();
        
        let state = Arc::new(BacktestV2State {
            artifact_store: Arc::new(store),
        });
        
        let app = backtest_v2_router().with_state(state);
        
        // First request - should return full response with ETag
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/runs/run_etag_test")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        
        assert_eq!(response.status(), StatusCode::OK);
        let etag = response.headers().get(header::ETAG).unwrap().to_str().unwrap();
        assert!(etag.starts_with('"'));
        
        // Second request with If-None-Match - should return 304
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/runs/run_etag_test")
                    .header(header::IF_NONE_MATCH, etag)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        
        assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
    }
}

# Institutional-Grade API Enhancements

**Status**: Planning Document  
**Created**: 2026-01-25  
**Priority**: High (institutional users require these features)

---

## Overview

This document outlines four key enhancements to make the backtest API truly institutional-grade:

1. **Methodology Capsule** - Human-readable methodology paragraph per run
2. **Enhanced Pagination** - Stable sorting and complete pagination metadata
3. **Schema Versioning** - Explicit versioning with changelog
4. **Operational Observability** - Request logging, rate limiting, metrics

---

## 1. Methodology Capsule

### Requirement

Generate a short, human-readable paragraph from config flags that explains:
- Whether the run is production-grade
- Settlement source and reference rule
- Dataset readiness (maker viability)
- Why the run is trusted or untrusted

This ensures institutional users can share runs with consistent, non-editable explanations.

### Design

```rust
// In run_artifact.rs

/// A human-readable methodology explanation generated from config.
/// This is an audit artifact - it cannot be edited after generation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MethodologyCapsule {
    /// Schema version for this capsule format.
    pub version: String,
    
    /// One-paragraph methodology summary (2-4 sentences).
    pub summary: String,
    
    /// Key-value pairs for structured display.
    pub details: Vec<MethodologyDetail>,
    
    /// Hash of inputs used to generate this capsule (for verification).
    pub input_hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MethodologyDetail {
    pub label: String,
    pub value: String,
    pub tooltip: Option<String>,
}

impl MethodologyCapsule {
    pub fn generate(config: &BacktestConfig, results: &BacktestResults) -> Self {
        let mut summary_parts = Vec::new();
        
        // Production grade status
        if config.production_grade {
            summary_parts.push(
                "This backtest was executed in production-grade mode with all \
                 correctness invariants enforced."
            );
        } else {
            summary_parts.push(
                "This backtest was executed in research mode and may use \
                 optimistic assumptions."
            );
        }
        
        // Settlement source
        let settlement_desc = match config.settlement_spec.as_ref() {
            Some(spec) => format!(
                "Settlement prices are derived from {} using the {} rule.",
                describe_settlement_source(&spec.oracle_source),
                format!("{:?}", spec.reference_price_rule)
            ),
            None => "Settlement uses simulated prices.".to_string(),
        };
        summary_parts.push(&settlement_desc);
        
        // Maker viability
        let maker_desc = match results.dataset_readiness {
            DatasetReadiness::MakerViable => 
                "The dataset supports maker (passive) order simulation with queue modeling.",
            DatasetReadiness::TakerOnly => 
                "The dataset supports taker (aggressive) execution only; maker fills are not simulated.",
            DatasetReadiness::NonRepresentative =>
                "The dataset lacks sufficient fidelity for production-grade claims.",
        };
        summary_parts.push(maker_desc);
        
        // Trust status explanation
        if results.truthfulness.is_trusted() {
            summary_parts.push(
                "All trust requirements passed; results may be used for deployment decisions."
            );
        } else {
            let reasons = results.truthfulness.untrusted_reasons.join("; ");
            summary_parts.push(&format!(
                "Trust requirements not satisfied: {}. Exercise caution when interpreting results.",
                reasons
            ));
        }
        
        let summary = summary_parts.join(" ");
        
        // Build structured details
        let details = vec![
            MethodologyDetail {
                label: "Production Grade".into(),
                value: if config.production_grade { "Yes" } else { "No" }.into(),
                tooltip: Some("Whether all correctness invariants were enforced".into()),
            },
            MethodologyDetail {
                label: "Settlement Source".into(),
                value: describe_settlement_source_short(config),
                tooltip: Some("Source of settlement/reference prices".into()),
            },
            MethodologyDetail {
                label: "Settlement Rule".into(),
                value: config.settlement_spec.as_ref()
                    .map(|s| format!("{:?}", s.reference_price_rule))
                    .unwrap_or_else(|| "N/A".into()),
                tooltip: Some("Rule for selecting settlement price from oracle data".into()),
            },
            MethodologyDetail {
                label: "Dataset Readiness".into(),
                value: format!("{:?}", results.dataset_readiness),
                tooltip: Some("Whether dataset supports maker/taker simulation".into()),
            },
            MethodologyDetail {
                label: "Maker Fill Model".into(),
                value: format!("{:?}", config.maker_fill_model),
                tooltip: Some("Model used for passive order fills".into()),
            },
            MethodologyDetail {
                label: "Trust Status".into(),
                value: format!("{:?}", results.truthfulness.verdict),
                tooltip: results.truthfulness.untrusted_reasons.first().cloned(),
            },
        ];
        
        // Compute input hash for verification
        let input_hash = compute_capsule_input_hash(config, results);
        
        Self {
            version: "1.0".into(),
            summary,
            details,
            input_hash,
        }
    }
}
```

### Integration Points

1. Add `methodology_capsule: MethodologyCapsule` to `RunManifest`
2. Include in manifest JSON response
3. Frontend displays in a collapsed "Methodology" section
4. Capsule is generated at persist-time and never modified

---

## 2. Enhanced Pagination

### Current State

```rust
pub struct ListRunsFilter {
    pub page: Option<usize>,      // 0-indexed
    pub page_size: Option<usize>, // default 20, max 100
    // ... filters
}

pub struct ListRunsResponse {
    pub total_count: usize,
    pub page: usize,
    pub page_size: usize,
    pub runs: Vec<RunSummary>,
}
```

### Enhancement

```rust
/// Sort field for run listings.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RunSortField {
    #[default]
    PersistedAt,
    FinalPnl,
    SharpeRatio,
    WinRate,
    StrategyName,
}

/// Sort direction.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SortOrder {
    #[default]
    Desc,
    Asc,
}

pub struct ListRunsFilter {
    // ... existing fields
    
    /// Sort field (default: persisted_at)
    pub sort_by: Option<RunSortField>,
    
    /// Sort direction (default: desc)
    pub sort_order: Option<SortOrder>,
}

pub struct ListRunsResponse {
    pub api_version: String,
    
    /// Total number of matching runs.
    pub total_count: usize,
    
    /// Current page (0-indexed).
    pub page: usize,
    
    /// Page size used.
    pub page_size: usize,
    
    /// Total number of pages.
    pub total_pages: usize,
    
    /// Whether there's a next page.
    pub has_next: bool,
    
    /// Whether there's a previous page.
    pub has_prev: bool,
    
    /// Sort field used.
    pub sort_by: RunSortField,
    
    /// Sort direction used.
    pub sort_order: SortOrder,
    
    /// The runs.
    pub runs: Vec<RunSummary>,
}
```

### Stable Permalinks (Already Implemented)

Run IDs are derived from fingerprint hashes:
```
run_id = run_{fingerprint.hash_hex}
```

This ensures:
- Same inputs + config + seed = same run_id
- Links are shareable and will always return identical content
- Content-addressable storage prevents modification

---

## 3. Schema Versioning Strategy

### Current State

- `RUN_ARTIFACT_API_VERSION = "1.0.0"`
- `RUN_ARTIFACT_STORAGE_VERSION = 1`
- `api_version` in most responses
- `schema_version` in manifest and histogram

### Formal Policy

```rust
/// API versioning policy:
/// 
/// 1. Every JSON response MUST include `api_version` or `schema_version`.
/// 2. Fields may be ADDED without version bump (additive-only).
/// 3. Fields may NOT be removed or renamed without major version bump.
/// 4. Deprecation: mark field with `#[deprecated]` for 2 minor versions before removal.
/// 5. Changelog: maintain CHANGELOG.md with all field additions.

/// Response trait that enforces schema versioning.
pub trait VersionedResponse {
    /// The schema version string (e.g., "1.2.0" or "v1").
    fn schema_version(&self) -> &str;
    
    /// The API version string for the endpoint family.
    fn api_version(&self) -> &str {
        RUN_ARTIFACT_API_VERSION
    }
}
```

### Changelog Template

```markdown
# API Changelog

## v1.1.0 (Planned)
### Added
- `methodology_capsule` field in `RunManifest`
- `total_pages`, `has_next`, `has_prev` in `ListRunsResponse`
- `sort_by`, `sort_order` query params for listing

### Deprecated
- (none)

## v1.0.0 (Current)
- Initial release
- Run artifact storage and retrieval
- Trust status in all responses
- ETag-based caching
```

---

## 4. Operational Observability

### 4.1 Request Logging Middleware

```rust
// In middleware/logging.rs

use axum::{
    body::Body,
    http::{Request, Response},
    middleware::Next,
};
use std::time::Instant;
use tracing::{info, warn, Span};

pub async fn request_logging_middleware(
    request: Request<Body>,
    next: Next<Body>,
) -> Response<Body> {
    let method = request.method().clone();
    let uri = request.uri().clone();
    let path = uri.path().to_string();
    
    let start = Instant::now();
    let span = tracing::info_span!(
        "http_request",
        method = %method,
        path = %path,
        status = tracing::field::Empty,
        latency_ms = tracing::field::Empty,
    );
    
    let response = next.run(request).await;
    
    let latency = start.elapsed();
    let status = response.status().as_u16();
    
    span.record("status", status);
    span.record("latency_ms", latency.as_millis() as u64);
    
    if status >= 500 {
        warn!(
            parent: &span,
            "Request failed: {} {} -> {} ({}ms)",
            method, path, status, latency.as_millis()
        );
    } else {
        info!(
            parent: &span,
            "{} {} -> {} ({}ms)",
            method, path, status, latency.as_millis()
        );
    }
    
    response
}
```

### 4.2 Rate Limiting

```rust
// Using tower_governor crate

use tower_governor::{
    governor::GovernorConfigBuilder,
    GovernorLayer,
};

fn rate_limit_layer() -> GovernorLayer {
    let config = GovernorConfigBuilder::default()
        .per_second(10)  // 10 requests per second per IP
        .burst_size(50)  // Allow bursts up to 50
        .finish()
        .unwrap();
    
    GovernorLayer::new(&config)
}

// In main.rs, add to router:
let app = app
    .layer(rate_limit_layer())
    .layer(request_logging_middleware);
```

### 4.3 Prometheus Metrics

```rust
// Using metrics and metrics-exporter-prometheus crates

use metrics::{counter, gauge, histogram};
use metrics_exporter_prometheus::PrometheusBuilder;

fn init_metrics() -> PrometheusHandle {
    PrometheusBuilder::new()
        .install_recorder()
        .expect("Failed to install Prometheus recorder")
}

// In request middleware:
counter!("http_requests_total", "method" => method, "path" => path, "status" => status);
histogram!("http_request_duration_seconds", latency.as_secs_f64());

// In health endpoint:
gauge!("uptime_seconds", uptime.as_secs_f64());

// Expose /metrics endpoint:
async fn metrics_handler(State(handle): State<PrometheusHandle>) -> String {
    handle.render()
}
```

### 4.4 Enhanced Health Check

```rust
#[derive(Serialize)]
pub struct HealthResponse {
    pub status: String,
    pub version: String,
    pub uptime_seconds: u64,
    pub checks: HealthChecks,
}

#[derive(Serialize)]
pub struct HealthChecks {
    pub database: HealthCheckResult,
    pub artifact_store: HealthCheckResult,
}

#[derive(Serialize)]
pub struct HealthCheckResult {
    pub status: String,
    pub latency_ms: Option<u64>,
    pub message: Option<String>,
}

pub async fn health_check(State(state): State<AppState>) -> Json<HealthResponse> {
    let start = state.start_time;
    let uptime = start.elapsed();
    
    // Check database
    let db_check = check_database(&state).await;
    
    // Check artifact store
    let store_check = check_artifact_store(&state).await;
    
    let overall_status = if db_check.status == "healthy" && store_check.status == "healthy" {
        "healthy"
    } else {
        "degraded"
    };
    
    Json(HealthResponse {
        status: overall_status.into(),
        version: env!("CARGO_PKG_VERSION").into(),
        uptime_seconds: uptime.as_secs(),
        checks: HealthChecks {
            database: db_check,
            artifact_store: store_check,
        },
    })
}
```

---

## Implementation Priority

| Feature | Priority | Effort | Impact |
|---------|----------|--------|--------|
| Methodology Capsule | High | Medium | High (trust explanation) |
| Enhanced Pagination | Medium | Low | Medium (UX) |
| Schema Changelog | High | Low | High (API stability) |
| Request Logging | High | Low | High (debugging) |
| Rate Limiting | High | Low | High (protection) |
| Prometheus Metrics | Medium | Medium | Medium (monitoring) |
| Enhanced Health Check | Medium | Low | Medium (ops) |

---

## Files to Create/Modify

### New Files
- `rust-backend/src/middleware/mod.rs`
- `rust-backend/src/middleware/logging.rs`
- `rust-backend/src/middleware/rate_limit.rs`
- `rust-backend/src/middleware/metrics.rs`
- `rust-backend/docs/API_CHANGELOG.md`

### Modified Files
- `rust-backend/src/backtest_v2/run_artifact.rs` (add MethodologyCapsule)
- `rust-backend/src/backtest_v2/artifact_store.rs` (enhanced pagination)
- `rust-backend/src/api/backtest_v2.rs` (sort params)
- `rust-backend/src/main.rs` (add middleware layers)
- `rust-backend/Cargo.toml` (add tower-governor, metrics deps)

---

## Acceptance Criteria

1. **Methodology Capsule**
   - [ ] Every run manifest includes a `methodology_capsule` field
   - [ ] Summary explains trust status in 2-4 sentences
   - [ ] Details array has 6+ key-value pairs
   - [ ] Frontend displays capsule in Provenance panel

2. **Pagination**
   - [ ] `sort_by` and `sort_order` params work
   - [ ] Response includes `total_pages`, `has_next`, `has_prev`
   - [ ] Sorting is deterministic (secondary sort by run_id)

3. **Schema Versioning**
   - [ ] API_CHANGELOG.md exists and is maintained
   - [ ] All responses include version field
   - [ ] No breaking changes without major version bump

4. **Observability**
   - [ ] Request logs include method, path, status, latency
   - [ ] Rate limiting returns 429 on excess
   - [ ] /metrics endpoint returns Prometheus format
   - [ ] /health returns uptime and component status

# Backtest API Changelog

This document tracks all changes to the Backtest V2 API schema. We follow semantic versioning:
- **Major**: Breaking changes (field removals, type changes)
- **Minor**: Additive changes (new fields, new endpoints)
- **Patch**: Bug fixes without schema changes

**Current Version**: 1.1.0

---

## Versioning Policy

1. Every JSON response MUST include `api_version` or `schema_version`.
2. Fields may be ADDED without major version bump (additive-only).
3. Fields may NOT be removed or renamed without major version bump.
4. Deprecation: mark field with `#[deprecated]` for 2 minor versions before removal.
5. Breaking changes require 30-day notice and version negotiation support.

---

## v1.1.0 (2026-01-25)

### Added

**Methodology Capsule** - Human-readable explanation of trust status
- `RunManifest.methodology_capsule` - New field containing:
  - `version` (string): Capsule schema version (currently "1.0")
  - `summary` (string): 2-4 sentence methodology explanation
  - `details` (array): Structured key-value pairs with tooltips
  - `input_hash` (string): Hash of inputs for verification

**Enhanced Pagination** - Complete pagination metadata in list responses
- `ListRunsResponse.total_pages` (integer): Total number of pages
- `ListRunsResponse.has_next` (boolean): Whether next page exists
- `ListRunsResponse.has_prev` (boolean): Whether previous page exists
- `ListRunsResponse.sort_by` (enum): Sort field used
- `ListRunsResponse.sort_order` (enum): Sort direction used

**Sorting Parameters** - Flexible sorting for run listings
- `ListRunsFilter.sort_by` (optional enum): Sort field
  - `persisted_at` (default)
  - `final_pnl`
  - `sharpe_ratio`
  - `win_rate`
  - `strategy_name`
  - `max_drawdown`
- `ListRunsFilter.sort_order` (optional enum): Sort direction
  - `desc` (default)
  - `asc`

### Changed

- Sorting now uses secondary sort by `run_id ASC` for deterministic ordering

### Deprecated

- None

---

## v1.0.0 (Initial Release)

### Features

- Run artifact storage and retrieval
- Content-addressable run IDs (derived from fingerprint hash)
- Trust status in all responses
- ETag-based HTTP caching
- Immutable artifact storage
- Full run manifest with:
  - Fingerprint
  - Strategy identity
  - Dataset metadata
  - Configuration summary
  - Trust decision
  - Disclaimers

### Endpoints

- `GET /api/v2/backtest/runs` - List runs with pagination
- `GET /api/v2/backtest/runs/{run_id}` - Get full run artifact
- `GET /api/v2/backtest/runs/{run_id}/manifest` - Get run manifest only
- `GET /api/v2/backtest/runs/{run_id}/equity-curve` - Get equity curve time series
- `GET /api/v2/backtest/runs/{run_id}/window-pnl-histogram` - Get certified histogram

---

## Migration Guide

### v1.0.0 → v1.1.0

**No breaking changes.** All new fields are additive.

Clients should:
1. Update response types to include new optional fields
2. Use `sort_by` and `sort_order` params for custom sorting
3. Use `total_pages`, `has_next`, `has_prev` for pagination UI
4. Display `methodology_capsule.summary` in the Provenance panel

# Data Pipeline Integrity Audit

## Overview

This document describes the repeatable, auditable data pipeline for Polymarket HFT backtesting.

## Pipeline Architecture

```
Live Recorder
   ↓
Immutable Raw Store (append-only)
   ↓
Nightly Backfill / Integrity Pass
   ↓
Versioned Dataset Snapshot
   ↓
Replay Validation Suite
   ↓
Backtest (production-grade mode)
```

## Step 1: Canonical Raw Data Streams

All raw streams are defined in `RawDataStream` enum:

| Stream | Source | Description |
|--------|--------|-------------|
| `L2Snapshots` | REST/WS | L2 order book snapshots (periodic full book state) |
| `L2Deltas` | WS | L2 incremental deltas (`price_change` messages) |
| `TradePrints` | WS | Trade prints (public trade tape) |
| `MarketMetadata` | REST/WS | Market metadata updates (status, halt, resolution) |
| `OracleRounds` | RPC | Oracle data (Chainlink price rounds) |

### Stream Requirements

- **Taker Minimum**: L2Snapshots + TradePrints
- **Maker Viable**: L2Snapshots + L2Deltas + TradePrints
- **Production Grade**: All 5 streams

### Schema Definitions

Each stream has a formal `StreamSchema` with:
- `required_fields`: Fields that must be present
- `optional_fields`: Fields that may be present
- `source_time_field`: Exchange timestamp field name
- `arrival_time_capture_point`: When arrival time is captured
- `sequence_semantics`: Exchange seq, ingest seq, hash fields

## Step 2: Live Data Recorder

`LiveRecorder` implements append-only recording:

### Record Format (`RawEventRecord`)

```rust
struct RawEventRecord {
    stream: RawDataStream,
    market_id: String,
    token_id: Option<String>,
    payload: RawPayload,  // JSON or Binary
    ingest_arrival_time_ns: u64,  // Captured at EARLIEST point
    ingest_seq: u64,  // Monotonic per (market_id, stream)
    source_time_ns: Option<u64>,  // Exchange timestamp
    exchange_seq: Option<String>,  // Exchange sequence/hash
}
```

### Guarantees

1. **No in-place mutation**: Events are INSERT only
2. **Arrival time capture**: At WebSocket message receipt, BEFORE JSON parsing
3. **Monotonic sequencing**: Per-(market_id, stream) ingest_seq
4. **Strict mode**: Fails on any recording error (configurable)
5. **Full audit trail**: Recording sessions logged with versions

## Step 3: Nightly Backfill / Integrity Pass

`NightlyBackfill` processes raw data with integrity checking:

### Policies

- **DuplicatePolicy**: Drop (log count) or Fail
- **OutOfOrderPolicy**: Reorder or Fail
- **GapPolicy**: Log, Fail, or Resync

### IntegrityReport

```rust
struct IntegrityReport {
    date: String,
    market_id: String,
    event_counts: HashMap<String, u64>,
    duplicates_dropped: HashMap<String, u64>,
    out_of_order_events: HashMap<String, u64>,
    gaps_detected: HashMap<String, Vec<GapInfo>>,
    resyncs_triggered: u64,
    status: IntegrityStatus,
    issues: Vec<IntegrityIssue>,
}
```

### Status Levels

- **Clean**: No issues
- **MinorIssues**: Duplicates dropped, minor reordering
- **MajorIssues**: Gaps detected, resyncs triggered
- **Failed**: Critical errors (data corruption)

## Step 4: Dataset Versioning and Immutability

`DatasetVersion` represents an immutable dataset:

### Version Fields

```rust
struct DatasetVersion {
    dataset_id: String,  // SHA256 hash
    name: String,
    time_range: TimeRange,
    streams: Vec<RawDataStream>,
    markets: Vec<String>,
    integrity_report_hash: String,
    schema_version: u32,
    recorder_version: String,  // git hash
    backfill_version: String,  // git hash
    created_at_ns: u64,
    finalized: bool,
    classification: DatasetClassification,
    readiness: DatasetReadiness,
    trust_level: DatasetTrustLevel,
}
```

### Immutability Rules

1. Once `finalized = true`, dataset cannot be modified
2. Any change produces a NEW dataset version
3. Trusted datasets cannot have trust level downgraded
4. Backtests MUST reference explicit dataset_id

## Step 5: Replay Validation Suite

`ReplayValidation` validates datasets before trust:

### Validation Checks

1. **Ordering**: Events ordered by (arrival_time_ns, ingest_seq)
2. **Integrity**: No violations during replay
3. **Book Invariants**: Reconstruction invariants hold
4. **Determinism**: Multiple passes produce identical fingerprints

### Result

```rust
struct ReplayValidationResult {
    dataset_id: String,
    validated_at_ns: u64,
    ordering_valid: bool,
    integrity_valid: bool,
    book_invariants_valid: bool,
    determinism_valid: bool,
    fingerprints: Vec<String>,  // One per pass
    passed: bool,
    failure_reasons: Vec<String>,
}
```

### Mandatory Execution

- Runs automatically for every new DatasetVersion
- Must pass BEFORE dataset marked "Trusted"
- Failure → dataset rejected for production-grade use

## Step 6: Dataset Trust Classification

`classify_dataset_trust()` computes trust level:

| Classification | Integrity | Validation | Trust Level |
|----------------|-----------|------------|-------------|
| FullIncremental | Clean | Pass | **Trusted** |
| FullIncremental | Minor | Pass | Approximate |
| SnapshotOnly | Any | Pass | Approximate |
| Incomplete | Any | Any | **Rejected** |
| Any | Failed | Any | **Rejected** |
| Any | Any | Fail | **Rejected** |
| Any | Any | None | Pending |

### Trust Level Semantics

- **Trusted**: Production-grade, results can be relied upon
- **Approximate**: Results should be qualified ("may be optimistic")
- **Rejected**: Cannot be used without override
- **Pending**: Not yet validated

## Step 7: Dataset Storage

`DatasetStore` persists dataset versions:

### Operations

- `store(dataset)`: Persist a dataset version
- `load(dataset_id)`: Load by ID
- `list()`: List all datasets
- `exists(dataset_id)`: Check existence

### Storage Schema

```sql
CREATE TABLE datasets (
    dataset_id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    version_json TEXT NOT NULL,
    created_at_ns INTEGER NOT NULL,
    finalized INTEGER NOT NULL DEFAULT 0
);
```

## CLI Commands (Planned)

```bash
# Record live data
record_live --markets btc-updown-15m --streams all

# Run nightly backfill
backfill_nightly --date 2024-01-15 --market btc-updown-15m

# Validate a dataset
validate_dataset --dataset-id abc123...

# List all datasets
list_datasets

# Show dataset details
show_dataset abc123...
```

## Integration with Backtest

Backtests in production-grade mode:

1. MUST specify explicit `dataset_id`
2. Dataset MUST be `Trusted` (or `Approximate` with downgrade)
3. Replay uses dataset's recorded arrival times
4. Results linked to dataset version for audit

## Test Coverage

11 tests verify:

1. Stream enumeration completeness
2. Schema definitions for all streams
3. Payload hashing consistency
4. Live recorder creation and recording
5. Dataset version creation
6. Dataset immutability enforcement
7. Replay validation determinism
8. Trust classification logic
9. Dataset store operations
10. Integrity report status computation

## Files

- `data_pipeline.rs`: Complete pipeline implementation (2050+ lines)
- `mod.rs`: Module registration and exports
- `DATA_PIPELINE_AUDIT.md`: This document

## Version

- Schema version: 1
- Implementation: Phase 37
- Tests: 568 passing (11 new for data pipeline)

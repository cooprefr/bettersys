# Settlement Chainlink Operationalization

This document summarizes the changes made to close the "PARTIAL" settlement finding by making Chainlink settlement truly production-operational.

## Summary of Changes

### 1. Mandatory Oracle Configuration (No Silent Defaults)

**New Files:**
- `oracle/config.rs` - `OracleConfig` and `OracleFeedConfig` structs

**Key Changes:**
- All settlement reference parameters MUST be explicitly configured
- Production-grade runs require `oracle_config` in `BacktestConfig`
- Validation checks:
  - Non-empty feed list
  - Valid Ethereum addresses (0x... format, 42 chars)
  - Non-zero chain_id
  - Non-zero decimals
  - `OracleVisibilityRule::OnArrival` (production-grade only)
  - `abort_on_missing = true` (production-grade only)
  - Single chain_id across all feeds (no mixing chains)

### 2. Automated Chainlink Backfill Service

**New Files:**
- `oracle/backfill.rs` - `OracleBackfillService`

**Features:**
- Programmatic and CLI usage:
  ```rust
  let service = OracleBackfillService::new(config, storage);
  service.backfill_range("BTC", start_ts, end_ts).await?;
  ```
- Two retrieval strategies:
  - **LogScan** (preferred): Scans `AnswerUpdated` events from logs
  - **RoundIteration** (fallback): Iterates `getRoundData` calls
  - **Hybrid** (default): Tries logs first, falls back to rounds
- Idempotent storage (safe to run repeatedly)
- Progress tracking with `BackfillProgress`
- Gap detection in round sequences
- Rate limiting to avoid RPC throttling

### 3. Feed Address Validation Against Chain/Network

**New Files:**
- `oracle/validation.rs` - `OracleFeedValidator`

**Validation Checks at Startup:**
1. RPC endpoint chain_id matches configured chain_id
2. Feed proxy address is a contract (has code)
3. `decimals()` call succeeds and matches configuration
4. `description()` contains expected asset symbol
5. `latestRoundData()` returns sane values (non-zero answer/timestamp)
6. Staleness check (warns if data > 24h old)

**Integration:**
```rust
let validation = validate_oracle_config_for_production(&config, allow_non_production).await?;
if !validation.all_passed {
    // In production mode: abort
    // In non-production mode: warn and continue
}
```

### 4. Settlement Invocation Made Unambiguous and Auditable

**Changes to Orchestrator:**
- Settlement engine explicitly invoked in the event loop
- Settlement logging includes:
  - Window start/end times
  - Reference rule used
  - Selected Chainlink round_id
  - Reference price value
  - Oracle updated_at + arrival_time_ns
  - Outcome and tie flag

**Assertions Added:**
- Settlement cannot occur without oracle data for the required rule
- Outcome cannot be treated as knowable before oracle arrival visibility allows it

### 5. Results Logging and Run Fingerprinting

**New Structs:**
- `OracleConfigSummary` - Included in `BacktestResults`
- `OracleCoverageSummary` - Oracle usage statistics

**BacktestResults Additions:**
```rust
pub oracle_config_used: Option<OracleConfigSummary>,
pub oracle_validation_outcome: Option<String>,
pub oracle_coverage: Option<OracleCoverageSummary>,
```

**ConfigFingerprint Additions:**
```rust
pub oracle_chain_id: Option<u64>,
pub oracle_feed_proxies: Vec<(String, String)>,
pub oracle_decimals: Vec<(String, u8)>,
pub oracle_visibility_rule: Option<String>,
pub oracle_rounding_policy: Option<String>,
pub oracle_config_hash: Option<u64>,
```

**Fingerprint Sensitivity:**
- Any change to settlement rule, chain_id, feed address, or decimals changes the `RunFingerprint`
- `OracleConfig::fingerprint_hash()` provides a deterministic hash

### 6. Comprehensive Tests

**New Test File:**
- `oracle/tests.rs`

**Test Coverage:**
1. Config validation (missing rule/feed/decimals fails production_grade)
2. Feed validation (wrong chain_id, non-contract address, decimals mismatch)
3. Backfill idempotency (no duplicate rounds on re-run)
4. Settlement selection (boundary tests for reference rule)
5. Fingerprint sensitivity (config changes change fingerprint)
6. Visibility semantics (outcome not knowable before arrival)

## Configuration Example

### Production-Grade Configuration

```rust
let config = BacktestConfig {
    // ... other fields ...
    oracle_config: Some(OracleConfig::production_multi_asset_polygon()),
    settlement_spec: Some(SettlementSpec::polymarket_15m_updown()),
    production_grade: true,
    // ...
};
```

### Custom Configuration

```rust
let mut oracle_config = OracleConfig::new();
oracle_config.reference_rule = SettlementReferenceRule::LastUpdateAtOrBeforeCutoff;
oracle_config.tie_rule = TieRule::NoWins;
oracle_config.visibility_rule = OracleVisibilityRule::OnArrival;
oracle_config.abort_on_missing = true;

oracle_config.add_feed(OracleFeedConfig {
    asset_symbol: "BTC".to_string(),
    chain_id: 137,
    feed_proxy_address: "0xc907E116054Ad103354f2D350FD2514433D57F6f".to_string(),
    decimals: 8,
    expected_description: Some("BTC / USD".to_string()),
    deviation_threshold: Some(0.001),
    heartbeat_secs: Some(2),
});

// Set RPC endpoint from environment
oracle_config.load_rpc_from_env();
```

## Environment Variables

```bash
# RPC endpoint (required for validation and backfill)
POLYGON_RPC_URL=https://polygon-mainnet.infura.io/v3/YOUR_KEY
# OR
CHAINLINK_RPC_URL=https://polygon-mainnet.infura.io/v3/YOUR_KEY
```

## Production-Grade Validation

Production-grade runs (`production_grade: true`) require:
1. `oracle_config` must be `Some(...)`
2. Oracle config must pass `validate_production()`
3. Feed validation against chain must pass (unless `allow_non_production: true`)

If any validation fails:
- In production mode: Backtest aborts with detailed error
- In non-production mode: Warning logged, results marked UNTRUSTED

## Audit Trail

Every production-grade backtest now includes in results:
- `oracle_config_used`: Full oracle configuration summary
- `oracle_validation_outcome`: "PASS" or detailed failure reasons
- `oracle_coverage`: Rounds used, time range, missing/stale counts
- `run_fingerprint`: Includes oracle config hash for reproducibility

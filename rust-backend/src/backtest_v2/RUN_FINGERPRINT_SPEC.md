# Run Fingerprint Specification (RUNFP_V1)

## Purpose

The Run Fingerprint provides a production-auditable hash that:
1. **Changes if and only if observable behavior changes**
2. **Is deterministic across machines** given same inputs + config + seed
3. **Enables reproducibility verification** by comparing fingerprints across runs
4. **Provides auditability** through component breakdown

## Fingerprint Structure

```
RunFingerprint = H(
  "RUNFP_V1" ||
  CodeFingerprintHash ||
  ConfigFingerprintHash ||
  DatasetFingerprintHash ||
  SeedFingerprintHash ||
  BehaviorFingerprintHash
)
```

All hashes use `std::collections::hash_map::DefaultHasher` for simplicity and consistency.

## Component Breakdown

### 1. CodeFingerprint

Captures the code version running the backtest.

| Field | Source | Purpose |
|-------|--------|---------|
| `crate_version` | `CARGO_PKG_VERSION` | Package version |
| `git_commit` | `GIT_COMMIT` env var (build time) | Exact commit hash |
| `build_profile` | `cfg!(debug_assertions)` | Debug vs release |

**Hash**: H(crate_version || git_commit || build_profile)

### 2. ConfigFingerprint

Captures all behavior-relevant configuration. Does NOT include:
- Logging verbosity
- Debug flags
- Output formatting options

| Field | Source | Purpose |
|-------|--------|---------|
| `settlement_reference_rule` | SettlementSpec | Which oracle round to use |
| `settlement_tie_rule` | SettlementSpec | How ties are resolved |
| `chainlink_feed_id` | SettlementConfig | Oracle feed identifier |
| `latency_model` | BacktestConfig.latency | Latency distribution type |
| `order_latency_ns` | BacktestConfig.latency | Mean/fixed latency |
| `oms_parity_mode` | BacktestConfig | OMS simulation fidelity |
| `maker_fill_model` | BacktestConfig | Maker fill assumptions |
| `integrity_policy` | BacktestConfig | Integrity enforcement level |
| `invariant_mode` | InvariantConfig | Hard/Soft/Disabled |
| `fee_rate_bps` | MatchingConfig | Fee structure |
| `strategy_params_hash` | StrategyParams | Hash of all strategy params |
| `arrival_policy` | SimArrivalPolicy | How arrival times are derived |
| `strict_accounting` | BacktestConfig | Ledger enforcement |
| `production_grade` | BacktestConfig | Production mode flag |

**Hash**: H(all fields in deterministic order)

### 3. DatasetFingerprint

Captures the data consumed by the run.

| Field | Source | Purpose |
|-------|--------|---------|
| `classification` | HistoricalDataContract.classify() | Data quality tier |
| `readiness` | DatasetReadiness | Maker/Taker viability |
| `orderbook_type` | HistoricalDataContract.orderbook | Snapshot vs deltas |
| `trade_type` | HistoricalDataContract.trades | Trade print availability |
| `arrival_semantics` | HistoricalDataContract.arrival_time | Recorded vs simulated |
| `streams[]` | Per-stream fingerprints | Individual stream hashes |

#### StreamFingerprint

For each input stream (orderbook_snapshots, trades, oracle_rounds, etc.):

| Field | Purpose |
|-------|---------|
| `stream_name` | Identifier (e.g., "orderbook_snapshots") |
| `market_ids` | Markets covered (sorted) |
| `start_time_ns` | First record timestamp |
| `end_time_ns` | Last record timestamp |
| `record_count` | Total records consumed |
| `rolling_hash` | H(prev || record_hash) for each record |

**Rolling Hash Algorithm**:
```rust
// Initial seed
rolling_hash = 0x5555_5555_5555_5555;

// For each record in deterministic order (timestamp, seq):
record_hash = H(canonical_bytes(record));
rolling_hash = H(rolling_hash || record_hash);
```

### 4. SeedFingerprint

Captures RNG seeds for determinism.

| Field | Source | Purpose |
|-------|--------|---------|
| `primary_seed` | BacktestConfig.seed | User-provided seed |
| `sub_seeds[]` | DeterministicSeed | Derived sub-seeds |

Sub-seeds are derived deterministically:
- `latency`: For latency sampling
- `fill_probability`: For maker fill simulation
- `queue_position`: For queue modeling

### 5. BehaviorFingerprint

Captures observable behavior during the run.

**Observable Events** (in deterministic order by decision_time, ingest_seq):

1. **Decision**: Strategy decision made
2. **OrderSubmit**: Order sent to OMS
3. **OrderAck**: Order accepted
4. **OrderReject**: Order rejected
5. **CancelAck**: Cancel confirmed
6. **Fill**: Trade executed
7. **Settlement**: Window settled
8. **LedgerPost**: Accounting entry

**Canonicalization Rules**:
- Prices: `(price * 1e8) as i64`
- Sizes: `(size * 1e8) as i64`
- Strings: Hash to u64 before inclusion
- Enums: `format!("{:?}")` then hash

**Rolling Hash**:
```rust
// Initial seed
rolling_hash = 0xAAAA_AAAA_AAAA_AAAA;

// For each event:
event_hash = H(canonical_event);
rolling_hash = H(rolling_hash || event_hash);
```

## What Changes the Fingerprint

### Changes → Different Fingerprint

- Different code version/commit
- Different settlement rule
- Different latency model params
- Different fee rate
- Different seed
- Different input data (even 1 record)
- Different strategy decision
- Different fill price/size
- Different settlement outcome

### Does NOT Change → Same Fingerprint

- Logging verbosity
- Output file paths
- Debug trace formatting
- Wall-clock time of run
- Machine identifier
- Thread scheduling

## Reproduction Instructions

To reproduce a run given a stored fingerprint:

1. **Verify Code**: Check out the exact `git_commit`
2. **Load Config**: Use the stored configuration
3. **Load Dataset**: Ensure same input streams with matching `rolling_hash`
4. **Set Seed**: Use the stored `primary_seed`
5. **Run**: Execute backtest
6. **Verify**: Final `RunFingerprint.hash` should match

## Integration Points

### At Run Start

```rust
let collector = FingerprintCollector::new();
collector.set_config(&config);
collector.set_dataset_readiness(readiness);
// Print code + config + seed hashes
```

### During Run

```rust
// Record input events
collector.record_input_event("orderbook_snapshots", timestamp, market_id, record_hash);

// Record behavior events
collector.record_decision(id, time, input_count, proof_hash);
collector.record_order_submit(id, side, price, size, time);
collector.record_fill(id, price, size, is_maker, fee, time);
collector.record_settlement(market_id, start, end, start_price, end_price, outcome, time);
```

### At Run End

```rust
let fingerprint = collector.finalize();
results.run_fingerprint = Some(fingerprint);
// Print full report
```

## Versioning Policy

The version prefix (`RUNFP_V1`) is included in the hash computation. When fingerprint format changes:

1. Increment version (e.g., `RUNFP_V2`)
2. Document changes
3. Old fingerprints remain valid for comparison with same-version runs
4. Different versions are considered incomparable

## Storage in BacktestResults

```rust
pub struct BacktestResults {
    // ... existing fields ...
    
    /// Run fingerprint for reproducibility verification.
    pub run_fingerprint: Option<RunFingerprint>,
}
```

## Output Format

### Compact (one-line)
```
RunFingerprint[a1b2c3d4e5f67890] code=12345678 config=87654321 data=abcd1234 seed=dcba4321 behavior=fedcba98
```

### Full Report
```
╔══════════════════════════════════════════════════════════════════════════════╗
║                         RUN FINGERPRINT REPORT                               ║
╠══════════════════════════════════════════════════════════════════════════════╣
║  Version:    RUNFP_V1                                                        ║
║  Hash:       a1b2c3d4e5f67890                                                ║
╠══════════════════════════════════════════════════════════════════════════════╣
║  COMPONENT HASHES:                                                           ║
║    Code:     1234567890abcdef  (v0.1.0 abc1234 (release))                   ║
║    Config:   fedcba0987654321                                                ║
║    Dataset:  1111222233334444  (3 streams, 15000 records)                   ║
║    Seed:     5555666677778888  (primary: 42)                                ║
║    Behavior: 9999aaaabbbbcccc  (1250 events)                                ║
╠══════════════════════════════════════════════════════════════════════════════╣
║  CONFIGURATION:                                                              ║
║    Settlement Rule:  LastUpdateAtOrBeforeCutoff                             ║
║    Latency Model:    Fixed                                                  ║
║    Maker Fill Model: ExplicitQueue                                          ║
║    Production Grade: true                                                   ║
╠══════════════════════════════════════════════════════════════════════════════╣
║  DATASET:                                                                    ║
║    Classification:   SnapshotOnly                                           ║
║    Readiness:        TakerViable                                            ║
║    Stream snapshots: 10000 records, hash 1234abcd5678efgh                   ║
║    Stream trades:    5000 records, hash 8765dcba4321hgfe                    ║
╚══════════════════════════════════════════════════════════════════════════════╝
```

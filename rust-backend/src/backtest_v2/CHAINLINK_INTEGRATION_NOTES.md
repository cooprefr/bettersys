# Chainlink Settlement Integration Notes

## Overview

This document describes the integration of Chainlink price feeds as the canonical settlement reference for Polymarket 15-minute up/down market backtesting.

**CRITICAL**: Polymarket 15m markets settle using **Chainlink oracle prices**, NOT Binance spot. Binance is used only for signal generation and as a predictor. The actual settlement outcome is determined by Chainlink.

---

## Configuration

### Environment Variables

```bash
# Required: Polygon RPC endpoint (HTTP)
POLYGON_RPC_URL=https://polygon-mainnet.infura.io/v3/YOUR_KEY
# Or alternative:
CHAINLINK_RPC_URL=https://polygon-mainnet.infura.io/v3/YOUR_KEY

# Optional: Override feed addresses (if using different feeds)
CHAINLINK_BTC_USD_ADDRESS=0xc907E116054Ad103354f2D350FD2514433D57F6f
CHAINLINK_ETH_USD_ADDRESS=0xF9680D99D6C9589e2a93a78A04A279e509205945
CHAINLINK_SOL_USD_ADDRESS=0x10C8264C0935b3B9870013e057f330Ff3e9C56dC
CHAINLINK_XRP_USD_ADDRESS=0x785ba89291f676b5386652eB12b30cF361020694

# Optional: Polling interval for live mode
CHAINLINK_POLL_INTERVAL_MS=1000

# Optional: Storage path for historical rounds
CHAINLINK_DB_PATH=chainlink_rounds.db

# Optional: Maximum rounds to keep per feed (0 = unlimited)
CHAINLINK_MAX_ROUNDS_PER_FEED=100000
```

### Default Feed Addresses (Polygon Mainnet)

| Asset | Proxy Address | Decimals | Deviation | Heartbeat |
|-------|---------------|----------|-----------|-----------|
| BTC/USD | `0xc907E116054Ad103354f2D350FD2514433D57F6f` | 8 | 0.1% | 2s |
| ETH/USD | `0xF9680D99D6C9589e2a93a78A04A279e509205945` | 8 | 0.1% | 2s |
| SOL/USD | `0x10C8264C0935b3B9870013e057f330Ff3e9C56dC` | 8 | 0.5% | 2s |
| XRP/USD | `0x785ba89291f676b5386652eB12b30cF361020694` | 8 | 0.5% | 2s |

---

## Data Schema

### ChainlinkRound

The primary data structure for oracle price observations:

```rust
pub struct ChainlinkRound {
    pub feed_id: String,              // Hash of chain_id + proxy address
    pub round_id: u128,               // Chainlink round ID (uint80)
    pub answer: i128,                 // Price with decimals (8 for USD)
    pub updated_at: u64,              // Oracle source time (Unix seconds)
    pub answered_in_round: u128,      // Round that computed this answer
    pub started_at: u64,              // When round started
    pub ingest_arrival_time_ns: u64,  // When WE observed this (nanoseconds)
    pub ingest_seq: u64,              // Local monotonic sequence
    pub decimals: u8,                 // Decimals for this feed
    pub asset_symbol: String,         // "BTC", "ETH", etc.
    pub raw_source_hash: Option<String>,
}
```

### Timestamp Semantics

| Field | Meaning | Use |
|-------|---------|-----|
| `updated_at` | Oracle source time | Settlement cutoff comparison |
| `ingest_arrival_time_ns` | When we observed it | Visibility/knowability |
| `ingest_seq` | Local sequence | Deterministic ordering |

---

## Storage Schema (SQLite)

### Main Table: `chainlink_rounds`

```sql
CREATE TABLE chainlink_rounds (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    feed_id TEXT NOT NULL,
    round_id INTEGER NOT NULL,
    answer INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    answered_in_round INTEGER NOT NULL,
    started_at INTEGER NOT NULL,
    ingest_arrival_time_ns INTEGER NOT NULL,
    ingest_seq INTEGER NOT NULL,
    decimals INTEGER NOT NULL,
    asset_symbol TEXT NOT NULL,
    raw_source_hash TEXT,
    created_at INTEGER NOT NULL,
    UNIQUE(feed_id, round_id)
);
```

### Indexes

- `idx_chainlink_rounds_updated_at`: Time-range queries
- `idx_chainlink_rounds_round_id`: Round ID lookups
- `idx_chainlink_rounds_asset`: Asset-based queries
- `idx_chainlink_rounds_arrival`: Visibility queries

---

## Settlement Reference Rules

Four rules are supported for selecting the reference price:

### 1. LastUpdateAtOrBeforeCutoff (DEFAULT)

Select the last oracle round with `updated_at <= cutoff`.

```
Timeline:  |--R1--|--R2--|--CUTOFF--|--R3--|
                     ↑
              Selected (R2)
```

### 2. FirstUpdateAfterCutoff

Select the first oracle round with `updated_at > cutoff`.

```
Timeline:  |--R1--|--R2--|--CUTOFF--|--R3--|
                                      ↑
                              Selected (R3)
```

### 3. ClosestToCutoff

Select the oracle round closest to cutoff. Tie goes to BEFORE.

```
Timeline:  |--R1--|--CUTOFF--|--R2--|
              ↑ 5s    ↑    5s ↑
              Winner (tie to before)
```

### 4. ClosestToCutoffTieAfter

Select the oracle round closest to cutoff. Tie goes to AFTER.

---

## Visibility Semantics

**CRITICAL**: The settlement outcome is NOT knowable at the cutoff time!

The outcome becomes knowable only when the reference price has **ARRIVED** in our system:

```
decision_time_ns >= reference_round.ingest_arrival_time_ns
```

This prevents look-ahead bias in backtesting.

### Example

```
Round 5: updated_at=2000s, ingest_arrival_time=2005s (5s network delay)
Cutoff: 2000s

Decision at 2003s: Outcome NOT knowable (arrival not yet happened)
Decision at 2006s: Outcome IS knowable (arrival has happened)
```

---

## How to Backfill Historical Rounds

### Option 1: Direct RPC Queries (Small Ranges)

```rust
let ingestor = ChainlinkIngestor::new(config);

// Get latest round
let latest = ingestor.fetch_latest_round().await?;

// Backfill 1000 rounds backward
let rounds = ingestor.backfill(latest.round_id, 1000).await?;

// Store to database
storage.store_rounds(&rounds)?;
```

### Option 2: Event Log Backfill (Large Ranges)

For large historical ranges, scan `AnswerUpdated` events from the aggregator contract:

```javascript
// Event signature: AnswerUpdated(int256 indexed current, uint256 indexed roundId, uint256 updatedAt)
const filter = {
    address: AGGREGATOR_ADDRESS,
    topics: [ethers.utils.id("AnswerUpdated(int256,uint256,uint256)")],
    fromBlock: START_BLOCK,
    toBlock: END_BLOCK
};
const logs = await provider.getLogs(filter);
```

### Important Notes

- Chainlink aggregators may be upgraded behind the proxy
- Round IDs are NOT globally monotonic across upgrades
- Validate rounds by checking `updated_at != 0`
- Always record `ingest_arrival_time_ns` for visibility

---

## Basis Diagnostics

The `BasisDiagnostics` module computes the mismatch between Binance and Chainlink:

```rust
let mut diag = BasisDiagnostics::new();

for window in windows {
    let mut basis = WindowBasis::new(start, end, "BTC".to_string());
    basis.binance_mid_at_cutoff = binance_price;
    basis.chainlink_settlement_price = chainlink_price;
    basis.finalize();
    diag.record_window(basis);
}

let stats = diag.overall_stats();
println!("Mean basis: {:.2} bps", stats.mean_basis_bps.unwrap_or(0.0));
println!("P99 |basis|: {:.2} bps", stats.p99_abs_basis_bps.unwrap_or(0.0));
println!("Direction agreement: {:.1}%", stats.direction_agreement_rate.unwrap_or(0.0));
```

### Key Metrics

- **Basis (bps)**: `(Binance - Chainlink) / Chainlink * 10000`
- **Direction Agreement**: Whether both sources agree on up/down
- **P95/P99 |Basis|**: Tail risk of basis mismatch

---

## Integration with Settlement Engine

To use Chainlink in the settlement engine:

```rust
// Load historical rounds
let rounds = storage.load_rounds_by_asset("BTC", start_ts, end_ts)?;

// Create settlement source
let source = ChainlinkSettlementSource::from_rounds(
    rounds,
    "BTC".to_string(),
    config.feed_id(),
);

// Get reference price for settlement
let cutoff = window_end_unix_sec;
let price = source.reference_price_at_or_before(cutoff)?;

// Check if outcome is knowable
if source.is_outcome_knowable(decision_time_ns, cutoff, rule) {
    // Safe to settle
    let outcome = spec.determine_outcome(start_price, price.price);
}
```

---

## Production Checklist

- [ ] Set `POLYGON_RPC_URL` to a reliable RPC provider
- [ ] Backfill historical rounds for desired time range
- [ ] Verify feed addresses match Polymarket's settlement contract
- [ ] Check basis diagnostics for acceptable divergence
- [ ] Ensure arrival times are recorded for all rounds
- [ ] Test visibility semantics with known delays

---

## Files

| File | Purpose |
|------|---------|
| `oracle/mod.rs` | Module exports |
| `oracle/chainlink.rs` | Feed config, round struct, ingestor, replay feed |
| `oracle/storage.rs` | SQLite persistence for rounds |
| `oracle/settlement_source.rs` | Settlement reference trait and implementations |
| `oracle/basis_diagnostics.rs` | Binance vs Chainlink comparison |
| `oracle_tests.rs` | Comprehensive tests |

---

## References

- [Chainlink Data Feeds Documentation](https://docs.chain.link/data-feeds)
- [Polygon Chainlink Feed Registry](https://docs.chain.link/data-feeds/price-feeds/addresses?network=polygon)
- [AggregatorV3Interface](https://github.com/smartcontractkit/chainlink/blob/develop/contracts/src/v0.8/interfaces/AggregatorV3Interface.sol)

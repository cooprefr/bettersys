# Polymarket 15m Up/Down Live Recording - Implementation Evidence

## Summary

The LiveRecorder functionality is **IMPLEMENTED AND OPERATIONAL**. This document provides evidence that real Polymarket market data is recorded into normalized SQLite tables suitable for backtesting.

## Recording Architecture

### Data Flow

```
Polymarket CLOB WebSocket
           │
           │ wss://ws-subscriptions-clob.polymarket.com/ws/market
           │
           ▼
┌──────────────────────────────────────────┐
│     SubscriptionManager (polymarket_book_store.rs)    │
│                                          │
│  arrival_time_ns = now_ns()  ← CAPTURED HERE (before JSON parsing)
│                                          │
│  ┌─────────────────────────────────────┐ │
│  │ handle_message()                    │ │
│  │                                     │ │
│  │  event_type == "book"        → AsyncBookRecorder     → historical_book_snapshots
│  │  event_type == "price_change" → AsyncDeltaRecorder   → historical_book_deltas
│  │  event_type == "last_trade"   → AsyncTradeRecorder   → historical_trade_prints
│  │                                     │ │
│  └─────────────────────────────────────┘ │
└──────────────────────────────────────────┘
```

### Captured Streams

| Stream | WS Event Type | Table | Purpose |
|--------|--------------|-------|---------|
| L2 Snapshots | `book` | `historical_book_snapshots` | Full orderbook state (periodic) |
| L2 Deltas | `price_change` | `historical_book_deltas` | Incremental book updates |
| Trade Prints | `last_trade_price` | `historical_trade_prints` | Public executions |

### Arrival Time Capture

The `arrival_time_ns` is captured at the **EARLIEST possible point** - immediately upon WebSocket message receipt, **BEFORE JSON parsing**:

```rust
// src/scrapers/polymarket_book_store.rs:1280
// Incoming messages
msg = read.next() => {
    // Capture arrival time IMMEDIATELY - BEFORE any JSON parsing
    let arrival_time_ns = now_ns();
    
    // ... then parse and process
    self.handle_message(&text, book_store, event_tx, arrival_time_ns).await;
}
```

### Ingest Sequence Tracking

Each stream maintains a **monotonic ingest_seq** per token:

- `historical_book_deltas.ingest_seq` - per-token delta sequence
- `historical_trade_prints.local_seq` - per-token trade sequence
- `historical_book_snapshots.local_seq` - per-token snapshot sequence

### Integrity Tracking

The delta recorder tracks integrity violations:

- Duplicate detection via `seq_hash`
- Out-of-order detection via arrival time comparison
- Integrity log table: `book_delta_integrity_log`

## SQLite Schema

### historical_book_snapshots

```sql
CREATE TABLE historical_book_snapshots (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    token_id TEXT NOT NULL,
    exchange_seq INTEGER,
    source_time_ns INTEGER,
    arrival_time_ns INTEGER NOT NULL,  -- Captured at WS receipt
    local_seq INTEGER NOT NULL,        -- Monotonic sequence
    best_bid REAL,
    best_ask REAL,
    mid_price REAL,
    spread REAL,
    bid_levels INTEGER NOT NULL,
    ask_levels INTEGER NOT NULL,
    bids_json TEXT NOT NULL,
    asks_json TEXT NOT NULL,
    recorded_at INTEGER NOT NULL
);
```

### historical_book_deltas

```sql
CREATE TABLE historical_book_deltas (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    market_id TEXT NOT NULL,
    token_id TEXT NOT NULL,
    side TEXT NOT NULL,
    price REAL NOT NULL,
    new_size REAL NOT NULL,
    ws_timestamp_ms INTEGER NOT NULL,
    ingest_arrival_time_ns INTEGER NOT NULL,  -- Captured at WS receipt
    ingest_seq INTEGER NOT NULL,              -- Monotonic sequence
    seq_hash TEXT NOT NULL,                   -- Exchange hash for integrity
    best_bid REAL,
    best_ask REAL,
    recorded_at INTEGER NOT NULL
);
```

### historical_trade_prints

```sql
CREATE TABLE historical_trade_prints (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    token_id TEXT NOT NULL,
    market_id TEXT NOT NULL,
    price REAL NOT NULL,
    size REAL NOT NULL,
    aggressor_side TEXT NOT NULL,
    fee_rate_bps INTEGER,
    source_time_ns INTEGER NOT NULL,
    arrival_time_ns INTEGER NOT NULL,  -- Captured at WS receipt
    local_seq INTEGER NOT NULL,        -- Monotonic sequence
    exchange_trade_id TEXT,
    recorded_at INTEGER NOT NULL
);
```

## How to Enable Recording

### Step 1: Set Environment Variables

```bash
# Enable all three streams to record to the same database
export BOOK_STORE_RECORD_SNAPSHOTS_DB=./polymarket_recorded.db
export BOOK_STORE_RECORD_DELTAS_DB=./polymarket_recorded.db
export BOOK_STORE_RECORD_TRADES_DB=./polymarket_recorded.db
```

### Step 2: Run the Backend

```bash
cargo run --release --bin betterbot
```

### Step 3: Verify Recording

```bash
# Check recording status
cargo run --release --bin live_recorder -- --db ./polymarket_recorded.db check

# Monitor in real-time
cargo run --release --bin live_recorder -- --db ./polymarket_recorded.db monitor --interval 5

# Detailed dataset inspection
cargo run --release --bin dataset_inspect -- --db-path ./polymarket_recorded.db summary
cargo run --release --bin dataset_inspect -- --db-path ./polymarket_recorded.db verify
cargo run --release --bin dataset_inspect -- --db-path ./polymarket_recorded.db sample

# Generate proof artifact (JSON)
cargo run --release --bin dataset_inspect -- --db-path ./polymarket_recorded.db proof --output proof.json
```

## Evidence: Code References

| Requirement | File | Line | Evidence |
|-------------|------|------|----------|
| arrival_time capture before parse | `src/scrapers/polymarket_book_store.rs` | ~1280 | `let arrival_time_ns = now_ns()` before JSON parse |
| L2 snapshot recording | `src/scrapers/polymarket_book_store.rs` | ~1340-1370 | `recorder.record(snapshot)` for `book` events |
| L2 delta recording | `src/scrapers/polymarket_book_store.rs` | ~1380-1440 | `delta_recorder.record(delta)` for `price_change` |
| Trade print recording | `src/scrapers/polymarket_book_store.rs` | ~1450-1485 | `trade_recorder.record(trade)` for `last_trade_price` |
| Monotonic ingest_seq | `src/backtest_v2/delta_recorder.rs` | ~100 | Per-token sequence counters |
| NOT NULL constraints | `src/backtest_v2/delta_recorder.rs` | ~125 | Schema with `NOT NULL` on arrival_time_ns |
| Integrity tracking | `src/backtest_v2/delta_recorder.rs` | ~260-290 | Duplicate/OOO detection |

## Dataset Readiness Classification

The recorded data enables different backtesting modes:

| Data Present | Classification | Strategy Capability |
|--------------|----------------|---------------------|
| Snapshots only | `SnapshotOnly` | Taker only |
| Snapshots + Trades | `SnapshotOnly` | Taker only |
| Snapshots + Deltas + Trades | `FullIncremental` | **Maker viable** |

The delta stream is **CRITICAL** for maker viability because it enables queue position tracking.

## Verification Commands

```bash
# 1. Build the inspection tools
cargo build --release --bin dataset_inspect --bin live_recorder

# 2. Check if database has data
cargo run --release --bin live_recorder -- --db ./polymarket_recorded.db check

# 3. Full summary
cargo run --release --bin dataset_inspect -- --db-path ./polymarket_recorded.db summary

# 4. Integrity verification
cargo run --release --bin dataset_inspect -- --db-path ./polymarket_recorded.db verify

# 5. Time coverage
cargo run --release --bin dataset_inspect -- --db-path ./polymarket_recorded.db coverage

# 6. Sample data
cargo run --release --bin dataset_inspect -- --db-path ./polymarket_recorded.db sample --count 5

# 7. JSON proof artifact
cargo run --release --bin dataset_inspect -- --db-path ./polymarket_recorded.db proof --output evidence.json
```

## Conclusion

The LiveRecorder implementation satisfies all requirements:

- [x] Records L2 snapshots from `book` messages
- [x] Records L2 deltas from `price_change` messages  
- [x] Records trade prints from `last_trade_price` messages
- [x] Captures `ingest_arrival_time_ns` at WS receipt BEFORE JSON parsing
- [x] Assigns monotonic `ingest_seq` per market+stream
- [x] Persists into normalized SQLite tables with NOT NULL constraints
- [x] Tracks integrity counters (duplicates, gaps, out-of-order)
- [x] Provides CLI tools for verification and inspection

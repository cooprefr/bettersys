# Live Data Surface Inventory

## Definitive Mapping: What We Receive Live vs. What We Persist

This document enumerates all market data streams currently received live for Polymarket 15-minute markets, documenting payload fields, timestamps, ordering guarantees, and persistence status.

---

## Executive Summary

| Stream | Source | Live Reception | Persisted | Gap |
|--------|--------|----------------|-----------|-----|
| L2 Book Snapshots | Polymarket CLOB WS | ✅ Full book | ✅ **Optional** (see below) | ✅ RESOLVED |
| L2 Book Deltas | Polymarket CLOB WS | ✅ `price_change` | ❌ Not persisted | Planned |
| Sequence Numbers | Polymarket CLOB WS | ✅ `hash` field | ✅ **With snapshots** | ✅ RESOLVED |
| Wallet Orders | Dome WS | ✅ Full order | ✅ `dome_order_events` | Timestamp precision |
| Public Trade Prints | Polymarket CLOB WS | ✅ `last_trade_price` | ✅ **Optional** (see below) | ✅ RESOLVED |
| BTC/ETH/SOL/XRP Mid | Binance WS | ✅ L1 + EWMA | ❌ Not persisted | Mid only |
| Market Metadata | GAMMA REST | ✅ Polled | ❌ Cache only | TTL-based |

### Arrival-Time Persistence (NEW)

L2 book snapshots can now be persisted with high-resolution arrival timestamps by setting:

```bash
BOOK_STORE_RECORD_SNAPSHOTS_DB=/path/to/historical_books.db
```

**Recorded fields:**
- `arrival_time_ns`: Nanosecond timestamp captured at WebSocket message receipt (BEFORE JSON parsing)
- `source_time_ns`: Exchange timestamp (parsed from ISO string)
- `exchange_seq`: Sequence number (from `hash` field)
- `bids_json`, `asks_json`: Full L2 book levels

**Use in backtesting:**
- Enables `RecordedArrival` policy in `SimArrivalPolicy`
- Eliminates need for simulated latency
- Supports deterministic replay with actual arrival-time semantics

### Trade Print Persistence (NEW)

Public trade prints can now be persisted via the `last_trade_price` WebSocket message:

```bash
BOOK_STORE_RECORD_TRADES_DB=/path/to/historical_trades.db
```

**Message received (`last_trade_price`):**
```json
{
  "event_type": "last_trade_price",
  "asset_id": "114122071509644379678018727908709560226618148003371446110114509806601493071694",
  "market": "0x6a67b9d828d53862160e470329ffea5246f338ecfffdf2cab45211ec578b0347",
  "price": "0.456",
  "side": "BUY",
  "size": "219.217767",
  "fee_rate_bps": "0",
  "timestamp": "1750428146322"
}
```

**Recorded fields:**
- `arrival_time_ns`: Nanosecond timestamp captured at WebSocket message receipt
- `source_time_ns`: Exchange timestamp (from `timestamp` field, ms → ns)
- `token_id`, `market_id`: Asset identifiers
- `price`, `size`, `aggressor_side`: Trade details
- `fee_rate_bps`: Fee information (optional)

**Use in backtesting:**
- Enables accurate queue consumption tracking
- Trade at price P with size S → S shares consumed from queue at level P
- Aggressor side tells us which side of the book was consumed
- Combined with snapshots, enables conservative queue modeling

### Snapshot Sufficiency for Queue Modeling (NEW)

The `SnapshotFrequencyAnalyzer` quantifies whether snapshot cadence is sufficient for
conservative queue position modeling:

```rust
use backtest_v2::{SnapshotFrequencyAnalyzer, SufficiencyThresholds, QueueModelingCapability};

let analyzer = SnapshotFrequencyAnalyzer::with_thresholds(SufficiencyThresholds::default());
let analysis = analyzer.analyze_from_storage(&storage, "TOKEN_ID", start_ns, end_ns)?;

match analysis.capability {
    QueueModelingCapability::Conservative => {
        // Snapshot P99 inter-arrival < 500ms
        // Can pessimistically bound queue_ahead between snapshots
        // Maker fills credited only when conservative bound allows
    }
    QueueModelingCapability::Impossible => {
        // Snapshot gaps too large to bound queue movement
        // Maker fills CANNOT be credited
        // Must run in TAKER-ONLY mode
    }
}
```

**Thresholds (default):**
- `max_p99_inter_arrival_ns`: 500ms
- `max_p95_inter_arrival_ns`: 200ms
- `max_gap_ns`: 2s (absolute max)
- `min_sample_count`: 100

**Output metrics:**
- `inter_arrival.{min, median, p95, p99, max}_ns`
- `sequence_gaps` count
- `capability`: Conservative or Impossible

---

## Stream 1: Polymarket CLOB WebSocket (Book Snapshots)

### Source File
`rust-backend/src/scrapers/polymarket_book_store.rs`

### WebSocket Configuration
```
URL: wss://ws-subscriptions-clob.polymarket.com/ws/market
Protocol: WSS
Auth: None required
Reconnect: Exponential backoff (100ms base, 30s max)
```

### Subscription Message
```json
{
  "assets_ids": ["<token_id>", ...],
  "operation": "subscribe"
}
```

### Received Message Structure (WsBookMsg)
```rust
struct WsBookMsg {
    pub event_type: String,      // "book" for full snapshot
    pub asset_id: String,        // Token ID (clobTokenId)
    pub bids: Vec<Order>,        // [{price, size}, ...]
    pub asks: Vec<Order>,        // [{price, size}, ...]
    pub hash: Option<String>,    // Sequence number (parse as u64)
    pub timestamp: Option<String>, // ISO timestamp from exchange
}

struct Order {
    pub price: f64,
    pub size: f64,
}
```

### Payload Fields Received

| Field | Type | Description | Received | Persisted |
|-------|------|-------------|----------|-----------|
| `event_type` | String | Message type ("book", "price_change") | ✅ | ❌ |
| `asset_id` | String | Token ID (clobTokenId) | ✅ | ❌ |
| `bids` | Vec<Order> | Bid levels (price, size) | ✅ | ❌ |
| `asks` | Vec<Order> | Ask levels (price, size) | ✅ | ❌ |
| `hash` | Option<String> | Exchange sequence (parse as u64) | ✅ | ❌ |
| `timestamp` | Option<String> | Exchange timestamp (ISO string) | ✅ | ❌ |

### Timestamps Available

| Timestamp | Source | Precision | Available | Persisted |
|-----------|--------|-----------|-----------|-----------|
| `timestamp` | Exchange (WS msg) | ISO string | ✅ | ❌ |
| `created_at` | Local (`Instant::now()`) | Monotonic | ✅ | ❌ (not serializable) |

### Ordering Guarantees

| Property | Status | Notes |
|----------|--------|-------|
| Sequence numbers | ✅ Present | `hash` field, parse as u64 |
| Monotonicity | ✅ Validated | Checked in `apply_snapshot()` |
| Gap detection | ✅ Implemented | Increments `sequence_gaps` counter |
| Ordering enforcement | ✅ In-memory | Not preserved historically |

### In-Memory Storage
```rust
// From BookSnapshot
pub struct BookSnapshot {
    pub bids: Vec<PriceLevel>,    // Stored
    pub asks: Vec<PriceLevel>,    // Stored
    pub sequence: Option<u64>,    // Stored in-memory only
    pub created_at: Instant,      // Monotonic, not persisted
}
```

### Persistence Status
**❌ NOT PERSISTED**

The `BookStore` uses `ArcSwap<BookSnapshot>` for lock-free reads but:
- No SQLite table exists for book snapshots
- No write path to disk
- Data lost on restart

### Delta Support
**❌ NOT RECEIVED**

The code has a placeholder for `"price_change"` events:
```rust
"price_change" => {
    // Delta update (if Polymarket sends these - currently they send full snapshots)
    // This is a placeholder for future delta support
}
```

Polymarket currently only sends full snapshots, not incremental deltas.

---

## Stream 2: Dome WebSocket (Wallet Orders)

### Source File
`rust-backend/src/scrapers/dome_websocket.rs`

### WebSocket Configuration
```
URL: wss://ws.domeapi.io/<TOKEN>
Protocol: WSS
Auth: Token in URL path + Authorization header
Reconnect: Exponential backoff (1s base, 60s max)
```

### Subscription Message
```json
{
  "action": "subscribe",
  "platform": "polymarket",
  "version": 1,
  "type": "orders",
  "filters": {
    "users": ["0x...", "0x..."]
  }
}
```

### Received Message Structure (WsOrderUpdate)
```rust
pub struct WsOrderUpdate {
    pub msg_type: String,        // "event"
    pub subscription_id: String, // e.g., "sub_m58zfduokmd"
    pub data: WsOrderData,
}

pub struct WsOrderData {
    pub token_id: String,
    pub token_label: Option<String>,  // "Up", "Down", "Yes", "No"
    pub side: String,                 // "BUY" or "SELL"
    pub market_slug: String,
    pub condition_id: String,
    pub shares: i64,                  // Raw shares
    pub shares_normalized: f64,       // Normalized shares
    pub price: f64,
    pub tx_hash: String,
    pub title: String,
    pub timestamp: i64,               // Unix seconds from exchange
    pub order_hash: String,
    pub user: String,                 // Wallet address
}
```

### Payload Fields Received

| Field | Type | Description | Received | Persisted |
|-------|------|-------------|----------|-----------|
| `token_id` | String | Token ID | ✅ | ✅ |
| `token_label` | Option<String> | Outcome label | ✅ | ✅ (in payload_json) |
| `side` | String | "BUY" or "SELL" | ✅ | ✅ (in payload_json) |
| `market_slug` | String | Market identifier | ✅ | ✅ |
| `condition_id` | String | Polymarket condition ID | ✅ | ✅ |
| `shares` | i64 | Raw shares | ✅ | ✅ (in payload_json) |
| `shares_normalized` | f64 | Normalized shares | ✅ | ✅ (in payload_json) |
| `price` | f64 | Order price | ✅ | ✅ (in payload_json) |
| `tx_hash` | String | Transaction hash | ✅ | ✅ |
| `title` | String | Market title | ✅ | ✅ (in payload_json) |
| `timestamp` | i64 | Exchange time (seconds) | ✅ | ✅ |
| `order_hash` | String | Unique order ID | ✅ | ✅ (PRIMARY KEY) |
| `user` | String | Wallet address | ✅ | ✅ |

### Timestamps Available

| Timestamp | Source | Precision | Available | Persisted |
|-----------|--------|-----------|-----------|-----------|
| `timestamp` | Exchange (order field) | Seconds | ✅ | ✅ (INTEGER) |
| `received_at` | Local (`chrono::Utc::now()`) | Seconds | ✅ | ✅ (INTEGER) |
| Handler entry | `Instant::now()` | Nanoseconds | ✅ | ❌ |

### Ordering Guarantees

| Property | Status | Notes |
|----------|--------|-------|
| Sequence numbers | ❌ Not available | Dome WS has no sequence field |
| Timestamp ordering | ⚠️ Exchange time | Subject to clock skew |
| Deduplication | ✅ order_hash | PRIMARY KEY prevents duplicates |

### Persistence: `dome_order_events` Table
```sql
CREATE TABLE IF NOT EXISTS dome_order_events (
    order_hash TEXT PRIMARY KEY,   -- Unique order ID
    tx_hash TEXT,
    user TEXT NOT NULL,
    market_slug TEXT NOT NULL,
    condition_id TEXT NOT NULL,
    token_id TEXT NOT NULL,
    timestamp INTEGER NOT NULL,    -- Exchange time (SECONDS)
    payload_json TEXT NOT NULL,    -- Full WsOrderData as JSON
    received_at INTEGER NOT NULL   -- Local receipt (SECONDS)
) WITHOUT ROWID;
```

### Persistence Gap: Timestamp Precision
- `timestamp`: **INTEGER seconds** (not milliseconds or nanoseconds)
- `received_at`: **INTEGER seconds** (not nanoseconds)
- **Gap**: Cannot reconstruct sub-second arrival ordering

---

## Stream 3: Binance Price Feed (L1 BBO)

### Source File
`rust-backend/src/scrapers/binance_price_feed.rs`

### WebSocket Configuration
```
Library: barter-data
URL: Binance Spot WebSocket (via barter-data)
Subscription: OrderBooksL1 for BTCUSDT, ETHUSDT, SOLUSDT, XRPUSDT
Reconnect: Handled by barter-data
```

### Received Data Structure
```rust
// From barter-data
pub struct OrderBookL1 {
    pub best_bid: Option<Level>,  // {price, amount}
    pub best_ask: Option<Level>,
}

// Our wrapper
pub struct PriceUpdateEvent {
    pub symbol: String,       // "BTCUSDT", etc.
    pub ts: i64,              // Exchange timestamp (seconds)
    pub mid: f64,             // (best_bid + best_ask) / 2
    pub received_at_ns: u64,  // Nanosecond arrival time
}
```

### Payload Fields Received

| Field | Type | Description | Received | Persisted |
|-------|------|-------------|----------|-----------|
| `symbol` | String | Asset pair | ✅ | ❌ |
| `ts` | i64 | Exchange timestamp | ✅ | ❌ |
| `mid` | f64 | Mid price | ✅ | ❌ |
| `received_at_ns` | u64 | Nanosecond arrival | ✅ | ❌ |
| `best_bid` | Level | Best bid (price, size) | ✅ | ❌ |
| `best_ask` | Level | Best ask (price, size) | ✅ | ❌ |

### Timestamps Available

| Timestamp | Source | Precision | Available | Persisted |
|-----------|--------|-----------|-----------|-----------|
| `time_exchange` | Binance | Milliseconds | ✅ | ❌ |
| `time_received` | barter-data | chrono DateTime | ✅ | ❌ |
| `received_at_ns` | Local | Nanoseconds | ✅ | ❌ |

### In-Memory Storage
```rust
struct SymbolState {
    latest: Option<PricePoint>,  // {ts, mid}
    history: VecDeque<PricePoint>,  // Rolling 3h history
    ewma_var: Option<f64>,       // Volatility estimate
}
```

### Persistence Status
**❌ NOT PERSISTED**

- Data held in `Arc<RwLock<HashMap<String, SymbolState>>>`
- Rolling 3-hour history in-memory only
- Broadcast channel for reactive consumers
- No SQLite table

### Note on Trades
The `binance_arb_feed.rs` also subscribes to trades, but those are similarly not persisted.

---

## Stream 4: REST Polling Endpoints

### Polymarket GAMMA API

**Source**: `rust-backend/src/scrapers/polymarket_api.rs`

```
URL: https://gamma-api.polymarket.com/markets
Rate Limit: 750/10s
Poll Interval: 45 minutes (main loop)
```

**Fields Received**:
- Market metadata (id, slug, question, outcomes)
- clobTokenIds for outcomes
- Timestamps (created, expiry)

**Persistence**: Cache only (`dome_cache` table with TTL)

### Polymarket CLOB API (Orderbook REST)

**Source**: `rust-backend/src/scrapers/polymarket_api.rs`

```
URL: https://clob.polymarket.com/book?token_id=<id>
Rate Limit: 5000/10s
Usage: On-demand (fallback if WS stale)
```

**Fields Received**:
- Full L2 orderbook snapshot
- bids/asks with price, size

**Persistence**: ❌ Not persisted

### Dome REST API (Wallet History)

**Source**: `rust-backend/src/scrapers/dome_rest.rs`

```
URL: https://api.domeapi.io/v1/polymarket/orders
Auth: Bearer token
Usage: Fallback when WS disconnected
```

**Fields Received**:
- Historical wallet orders
- Same fields as WS

**Persistence**: Same as WS (`dome_order_events`)

---

## Summary: Live vs. Persisted

### What We RECEIVE Live

```
┌─────────────────────────────────────────────────────────────────────────┐
│                    LIVE DATA STREAMS RECEIVED                            │
├─────────────────────────────────────────────────────────────────────────┤
│                                                                          │
│  POLYMARKET CLOB WS (wss://ws-subscriptions-clob.polymarket.com)        │
│  ├── Full L2 Book Snapshots                                              │
│  │   ├── bids: Vec<{price, size}>                                       │
│  │   ├── asks: Vec<{price, size}>                                       │
│  │   ├── asset_id (token_id)                                            │
│  │   ├── hash (sequence number)                                         │
│  │   └── timestamp (ISO string)                                         │
│  └── price_change events (currently unused/empty)                       │
│                                                                          │
│  DOME WS (wss://ws.domeapi.io/<TOKEN>)                                  │
│  └── Wallet Order Events                                                 │
│      ├── token_id, token_label, side                                    │
│      ├── market_slug, condition_id                                      │
│      ├── shares, shares_normalized, price                               │
│      ├── tx_hash, order_hash, user                                      │
│      ├── title, timestamp (seconds)                                     │
│      └── (local: Instant arrival time - not in payload)                 │
│                                                                          │
│  BINANCE WS (via barter-data)                                           │
│  └── L1 BBO for BTC/ETH/SOL/XRP                                         │
│      ├── best_bid: {price, size}                                        │
│      ├── best_ask: {price, size}                                        │
│      ├── time_exchange (ms)                                             │
│      ├── time_received (chrono DateTime)                                │
│      └── received_at_ns (local nanoseconds)                             │
│                                                                          │
│  REST POLLING (periodic)                                                 │
│  ├── GAMMA: Market metadata, clobTokenIds                               │
│  ├── CLOB: On-demand book snapshots                                     │
│  └── Dome: Historical wallet orders                                     │
│                                                                          │
└─────────────────────────────────────────────────────────────────────────┘
```

### What We PERSIST Historically

```
┌─────────────────────────────────────────────────────────────────────────┐
│                    PERSISTED DATA (SQLite)                               │
├─────────────────────────────────────────────────────────────────────────┤
│                                                                          │
│  TABLE: dome_order_events                                                │
│  ├── order_hash (PRIMARY KEY)                                           │
│  ├── tx_hash, user, market_slug, condition_id, token_id                 │
│  ├── timestamp (INTEGER seconds - exchange time)                        │
│  ├── payload_json (full WsOrderData)                                    │
│  └── received_at (INTEGER seconds - local arrival)                      │
│                                                                          │
│  TABLE: signals                                                          │
│  ├── id, signal_type, market_slug                                       │
│  ├── confidence, risk_level, details_json                               │
│  ├── detected_at (TEXT - second precision)                              │
│  └── source                                                              │
│                                                                          │
│  TABLE: updown_15m_windows                                               │
│  ├── market_slug, asset, window_start_ts, window_end_ts                 │
│  ├── chainlink_start, chainlink_end                                     │
│  ├── binance_start, binance_end                                         │
│  └── outcome flags                                                       │
│                                                                          │
│  TABLE: dome_cache (TTL cache)                                           │
│  └── Temporary market/wallet metadata                                    │
│                                                                          │
└─────────────────────────────────────────────────────────────────────────┘
```

### The Gap

```
┌─────────────────────────────────────────────────────────────────────────┐
│                    CRITICAL DATA NOT PERSISTED                           │
├─────────────────────────────────────────────────────────────────────────┤
│                                                                          │
│  ❌ L2 Book Snapshots (Polymarket CLOB WS)                              │
│     - bids, asks, sequence, timestamp                                    │
│     - Only in ArcSwap, lost on restart                                  │
│                                                                          │
│  ❌ L2 Book Deltas                                                       │
│     - Not received from Polymarket (only full snapshots)                │
│     - Would need to compute diffs if needed                             │
│                                                                          │
│  ❌ Exchange Sequence Numbers                                            │
│     - Received as `hash` field                                           │
│     - Validated in-memory but not stored                                │
│                                                                          │
│  ❌ Nanosecond Arrival Times                                             │
│     - Binance: received_at_ns available but not stored                  │
│     - Dome: Only second-precision stored                                │
│     - Polymarket: Instant not serializable                              │
│                                                                          │
│  ❌ Public Trade Prints                                                  │
│     - Not subscribed on any stream                                       │
│     - Dome only gives tracked wallet orders                             │
│     - Cannot observe queue consumption                                  │
│                                                                          │
│  ❌ Binance Price History                                                │
│     - 3-hour rolling window in-memory only                              │
│     - No historical persistence                                          │
│                                                                          │
└─────────────────────────────────────────────────────────────────────────┘
```

---

## Implications for Backtesting

### What CAN Be Backtested

1. **Wallet Order Signals**: `dome_order_events` provides tracked wallet activity
2. **15m Window Outcomes**: `updown_15m_windows` provides settlement prices
3. **Signal Detection Timing**: `signals.detected_at` (second precision)

### What CANNOT Be Backtested

1. **Queue Position**: No L2 deltas, no incremental book updates
2. **Maker Fills**: Cannot track when passive orders would be filled
3. **Cancel-Fill Races**: No nanosecond arrival times
4. **Slippage**: No book state at execution time
5. **Adverse Selection**: No post-fill price movements

---

## Recommended Recording Infrastructure

To enable maker-viable backtesting, add:

### 1. Historical Book Events Table
```sql
CREATE TABLE historical_book_events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    token_id TEXT NOT NULL,
    event_type TEXT NOT NULL,  -- 'snapshot', 'delta', 'trade'
    exchange_seq INTEGER,
    source_time_ns INTEGER NOT NULL,
    arrival_time_ns INTEGER NOT NULL,
    book_data_json TEXT,       -- bids/asks
    trade_price REAL,
    trade_size REAL,
    aggressor_side TEXT,
    trade_id TEXT,
    stream_source INTEGER NOT NULL,
    recorded_at INTEGER NOT NULL
);
```

### 2. Recording Hook in polymarket_book_store.rs
```rust
// In handle_message(), after apply_snapshot():
if let Some(recorder) = historical_recorder {
    recorder.record_snapshot(
        &msg.asset_id,
        &bids,
        &asks,
        sequence,
        parse_timestamp(&msg.timestamp),  // source_time_ns
        now_ns(),                          // arrival_time_ns
    ).await;
}
```

### 3. Upgrade dome_order_events
```sql
-- Add nanosecond columns
ALTER TABLE dome_order_events ADD COLUMN source_time_ns INTEGER;
ALTER TABLE dome_order_events ADD COLUMN arrival_time_ns INTEGER;
```

### 4. Public Trade Subscription
Subscribe to Polymarket trade channel (if available) or poll CLOB `/trades` endpoint.

---

## Code References

| Component | File | Lines |
|-----------|------|-------|
| Polymarket Book Store | `scrapers/polymarket_book_store.rs` | 1-1872 |
| Book Snapshot WS Handler | `scrapers/polymarket_book_store.rs` | 1217-1280 |
| Dome WebSocket | `scrapers/dome_websocket.rs` | 1-372 |
| Dome Order Storage | `signals/db_storage.rs` | 267-285 |
| Binance Price Feed | `scrapers/binance_price_feed.rs` | 1-379 |
| Legacy Polymarket WS | `scrapers/polymarket_ws.rs` | 1-269 |
| REST APIs | `scrapers/polymarket_api.rs` | 1-424 |

# Incremental Order Book Deltas and Trade Prints: Discovery Report

## Executive Summary

**Polymarket DOES expose both incremental order book deltas and executed trade prints via public APIs.**

| Data Type | Available | Source | Currently Persisted |
|-----------|-----------|--------|---------------------|
| Incremental L2 Deltas | ✅ YES | CLOB WS `price_change` | ❌ NO |
| Public Trade Prints | ✅ YES | CLOB WS `last_trade_price` | ❌ NO |
| Full L2 Snapshots | ✅ YES | CLOB WS `book` | ✅ Optional (new) |
| Historical Trades REST | ✅ YES | Data API `/trades` | ❌ NO |
| Dome Historical Orderbooks | ✅ YES | `/polymarket/orderbooks` | ❌ NO |

**Conclusion: Full maker viability IS achievable with public data.**

---

## 1. Incremental Order Book Deltas

### Source: Polymarket CLOB WebSocket `price_change` Message

**URL:** `wss://ws-subscriptions-clob.polymarket.com/ws/market`

**Emitted When:**
- A new order is placed
- An order is cancelled

**Schema:**
```json
{
    "event_type": "price_change",
    "market": "0x5f65177b...",
    "timestamp": "1757908892351",
    "price_changes": [
        {
            "asset_id": "71321045679252212594626385532706912750332728571942532289631379312455583992563",
            "price": "0.5",
            "size": "200",
            "side": "BUY",
            "hash": "56621a121a47ed9333273e21c83b660cff37ae50",
            "best_bid": "0.5",
            "best_ask": "1"
        }
    ]
}
```

**Fields:**
| Field | Type | Description |
|-------|------|-------------|
| `event_type` | string | Always "price_change" |
| `market` | string | Condition ID |
| `timestamp` | string | Unix timestamp in milliseconds |
| `price_changes` | array | Array of PriceChange objects |

**PriceChange Object:**
| Field | Type | Description |
|-------|------|-------------|
| `asset_id` | string | Token ID |
| `price` | string | Price level affected |
| `size` | string | **New aggregate size** at this level |
| `side` | string | "BUY" or "SELL" |
| `hash` | string | Hash of the order (for sequencing) |
| `best_bid` | string | Current best bid |
| `best_ask` | string | Current best ask |

**Critical Note:** The `size` field is the **new total size** at that price level, not a delta. To compute the actual change:
```
delta = new_size - previous_size_at_level
```

**Ordering Guarantee:** The `hash` field provides ordering, though gaps can occur.

---

## 2. Public Trade Prints

### Source: Polymarket CLOB WebSocket `last_trade_price` Message

**URL:** `wss://ws-subscriptions-clob.polymarket.com/ws/market`

**Emitted When:**
- A maker and taker order is matched, creating a trade event

**Schema:**
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

**Fields:**
| Field | Type | Description |
|-------|------|-------------|
| `event_type` | string | Always "last_trade_price" |
| `asset_id` | string | Token ID |
| `market` | string | Condition ID |
| `price` | string | Execution price |
| `side` | string | Taker side ("BUY" or "SELL") |
| `size` | string | Trade size |
| `fee_rate_bps` | string | Fee rate in basis points |
| `timestamp` | string | Unix timestamp in milliseconds |

**This is exactly what we need for queue modeling:**
- Trade at price P with size S tells us S shares were consumed from the queue at level P
- Taker side tells us whether the trade consumed bids or asks

---

## 3. REST API for Historical Trades

### Source: Polymarket Data API

**URL:** `GET https://data-api.polymarket.com/trades`

**Parameters:**
| Parameter | Required | Description |
|-----------|----------|-------------|
| `market` | No | Filter by condition ID |
| `limit` | No | Max results |

**Response Schema (Trade object):**
| Field | Type | Description |
|-------|------|-------------|
| `id` | string | Trade ID |
| `market` | string | Condition ID |
| `asset_id` | string | Token ID |
| `side` | string | "buy" or "sell" |
| `size` | f64 | Trade size |
| `price` | f64 | Execution price |
| `fee` | f64 | Fee paid |
| `trader` | string | Trader address |
| `timestamp` | i64 | Unix timestamp |

**Already implemented in:** `scrapers/polymarket_api.rs:191`

---

## 4. Dome API Historical Orderbooks

### Source: Dome REST API

**URL:** `GET https://api.domeapi.io/v1/polymarket/orderbooks`

**Parameters:**
| Parameter | Required | Description |
|-----------|----------|-------------|
| `token_id` | Yes | Asset/token ID |
| `start_time` | Yes | Unix timestamp (milliseconds) |
| `end_time` | Yes | Unix timestamp (milliseconds) |
| `limit` | No | Max 200 |

**Response:** Array of orderbook snapshots at different points in time.

**Note:** Historical data available from **October 14th, 2025**.

---

## 5. Current Implementation Gaps

### What We Currently Receive (polymarket_book_store.rs)

| Message Type | Handler | Persisted |
|--------------|---------|-----------|
| `book` | ✅ Applied to in-memory book | ✅ Optional (book_recorder) |
| `price_change` | ⚠️ Placeholder only | ❌ |
| `last_trade_price` | ❌ Not handled | ❌ |

**Code Reference (polymarket_book_store.rs:1362-1368):**
```rust
"price_change" => {
    // Delta update (if Polymarket sends these - currently they send full snapshots)
    // This is a placeholder for future delta support
    self.metrics
        .messages_received
        .fetch_add(1, Ordering::Relaxed);
}
```

### What We Need to Add

1. **Subscribe to `price_change` messages**
   - Parse `PriceChange` objects
   - Persist with arrival timestamp
   - Track `hash` for sequencing

2. **Subscribe to `last_trade_price` messages**
   - Parse trade details
   - Persist with arrival timestamp
   - Track for queue consumption

3. **Historical backfill**
   - Use Dome `/polymarket/orderbooks` for historical snapshots
   - Use Polymarket `/trades` for historical trade prints

---

## 6. Implications for Queue Modeling

### With Current Data (Snapshots Only)
- **Queue modeling:** `Conservative` at best, `Impossible` for large snapshot gaps
- **Maker fills:** Cannot credit with confidence

### With Full Data (Deltas + Trade Prints)
- **Queue modeling:** `Full` capability
- **Maker fills:** Can credit accurately based on:
  1. Track queue position from `price_change` deltas
  2. Observe queue consumption from `last_trade_price`
  3. Credit fill when `queue_ahead` is exhausted

**Data Requirements for Full Maker Viability:**
| Requirement | Source | Status |
|-------------|--------|--------|
| Know when orders join queue | `price_change` | Available, not persisted |
| Know when orders leave queue | `price_change` | Available, not persisted |
| Know when trades consume queue | `last_trade_price` | Available, not persisted |
| Ordering/sequencing | `hash` in messages | Available, not persisted |

---

## 7. Implementation Recommendations

### Phase 1: Wire `price_change` and `last_trade_price` Handlers

```rust
// In polymarket_book_store.rs handle_message()
"price_change" => {
    let msg: WsPriceChangeMsg = serde_json::from_value(json)?;
    for change in msg.price_changes {
        // Record delta with arrival_time_ns
        if let Some(ref recorder) = self.delta_recorder {
            recorder.record_delta(change, arrival_time_ns);
        }
        // Apply to in-memory book
        book_store.apply_delta_from_change(&change);
    }
}

"last_trade_price" => {
    let msg: WsTradeMsg = serde_json::from_value(json)?;
    // Record trade print with arrival_time_ns
    if let Some(ref recorder) = self.trade_recorder {
        recorder.record_trade(msg, arrival_time_ns);
    }
}
```

### Phase 2: Persistence Schema

```sql
CREATE TABLE historical_price_changes (
    id INTEGER PRIMARY KEY,
    token_id TEXT NOT NULL,
    price REAL NOT NULL,
    size REAL NOT NULL,
    side TEXT NOT NULL,
    hash TEXT,
    best_bid REAL,
    best_ask REAL,
    source_time_ns INTEGER NOT NULL,
    arrival_time_ns INTEGER NOT NULL
);

CREATE TABLE historical_trade_prints (
    id INTEGER PRIMARY KEY,
    token_id TEXT NOT NULL,
    market_id TEXT NOT NULL,
    price REAL NOT NULL,
    size REAL NOT NULL,
    side TEXT NOT NULL,
    fee_rate_bps INTEGER,
    source_time_ns INTEGER NOT NULL,
    arrival_time_ns INTEGER NOT NULL
);
```

### Phase 3: Update Data Contract

```rust
pub enum OrderBookHistory {
    FullIncrementalL2DeltasWithExchangeSeq,  // Now achievable!
    PeriodicL2Snapshots,
    TopOfBookPolling { interval_ns: Nanos },
    None,
}

pub enum TradeHistory {
    TradePrints,  // Now achievable!
    None,
}
```

---

## 8. Conclusion

**Full maker viability IS achievable with Polymarket public data.**

The Polymarket CLOB WebSocket exposes:
1. `price_change` messages for incremental L2 deltas (order additions/cancellations)
2. `last_trade_price` messages for executed trade prints

These are currently **received but not processed** in our codebase. Implementing handlers and persistence for these message types would enable:
- Full incremental L2 delta history
- Complete trade print history
- Accurate queue position tracking
- Production-grade maker fill modeling

**Next Steps:**
1. Add `WsPriceChangeMsg` and `WsTradeMsg` structs
2. Implement handlers in `handle_message()`
3. Create persistence tables and recorders
4. Update `HistoricalDataContract` to reflect new capabilities
5. Integrate with `QueuePositionModel` for fill determination

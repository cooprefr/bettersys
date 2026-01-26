# HFT-Grade Orderbook Implementation

## Summary

This implementation provides an HFT-grade orderbook management system for Polymarket CLOB that ensures the trading loop **never awaits a REST call for orderbook data**.

## Components

### 1. BookStore (`src/scrapers/polymarket_book_store.rs`)

The single source of truth for orderbooks with lock-free reads:

- **ArcSwap-based storage**: Zero-allocation reads via atomic pointer swaps
- **Monotonic time tracking**: Uses `Instant` for staleness, not `SystemTime`
- **Per-token state**: Tracks `is_ready`, `last_update_ns`, `sequence`, `update_count`
- **Watch channels**: Event-driven notification for strategies

### 2. SubscriptionManager

Manages WebSocket subscriptions with eager subscription:

- **Eager subscription**: Subscribes to entire universe on startup
- **Auto-reconnect**: Exponential backoff (100ms to 30s)
- **Health monitoring**: Periodic checks for stale tokens
- **Resubscription**: Automatic resubscribe on hard staleness threshold

### 3. WarmupManager

Gating mechanism before trading is enabled:

- **Configurable threshold**: `warmup_min_ready_fraction` (default 80%)
- **Timeout**: `warmup_timeout_ms` (default 10s)
- **Disabled tokens**: Tokens that fail warmup are disabled for session

### 4. Cache-Only Access Functions (`src/vault/book_access.rs`)

Skip-tick semantics with strategy-dependent staleness:

```rust
// HFT latency-arb (100-500ms tolerance)
let config = StalenessConfig::latency_arb();

// FAST15M deterministic (1500ms tolerance)
let config = StalenessConfig::fast15m();

// LONG strategies (5s tolerance)
let config = StalenessConfig::long_strategy();
```

## Key Design Principles

### 1. No REST in Hot Path

Before:
```rust
// OLD: REST fallback blocks trading loop
let book = match cache.get(token, 1500) {
    Some(b) => b,
    None => rest_fetch(token).await  // BLOCKS!
};
```

After:
```rust
// NEW: Skip-tick semantics, never blocks
let book = match cache.get(token, max_stale_ms) {
    Some(b) => b,
    None => return Ok(()),  // Skip this tick, don't block
};
```

### 2. Warmup Phase

Trading is gated until enough tokens are ready:

```rust
// Enable HFT book cache
HFT_BOOK_CACHE_ENABLED=1 cargo run

// The system will:
// 1. Subscribe to all tokens in universe
// 2. Wait for warmup (up to 10s)
// 3. Disable tokens that fail warmup
// 4. Enable trading only after warmup completes
```

### 3. Robust Book Building

- **Snapshot application**: Replaces entire book atomically
- **Delta support**: Sequence validation, gap detection
- **Invalid state detection**: Crossed book rejection
- **Conservative reset**: Mark not ready on any anomaly

## Configuration

Environment variables:

```bash
# Enable HFT book cache
HFT_BOOK_CACHE_ENABLED=1

# Staleness thresholds
BOOK_STORE_DEFAULT_MAX_STALE_MS=1500  # Trading threshold
BOOK_STORE_HARD_STALE_MS=5000         # Resubscription threshold

# Warmup
BOOK_STORE_WARMUP_TIMEOUT_MS=10000
BOOK_STORE_WARMUP_MIN_READY_FRACTION=0.8

# Book depth
BOOK_STORE_MAX_DEPTH=20
```

## Metrics

The system tracks comprehensive metrics:

```rust
// Cache performance
cache_hits: u64
cache_misses_not_subscribed: u64
cache_misses_not_ready: u64
cache_misses_stale: u64
cache_misses_never_seen: u64
cache_misses_crossed: u64

// Book building
snapshots_applied: u64
snapshot_rejects: u64
deltas_applied: u64
sequence_gaps: u64
crossed_book_resets: u64

// Age distribution
mean_served_age_ms: f64
age_histogram: [u64; 8]  // 0-10ms, 10-50ms, 50-100ms, etc.
```

Access via:
```rust
let summary = hft_cache.book_metrics().summary();
println!("Hit rate: {:.2}%", summary.hit_rate * 100.0);
println!("Mean age: {:.1}ms", summary.mean_age_ms);
```

## Files Changed

1. **NEW** `src/scrapers/polymarket_book_store.rs` - Main HFT book store
2. **NEW** `src/scrapers/polymarket_book_store_test.rs` - Testing utilities
3. **NEW** `src/vault/book_access.rs` - Cache-only access functions
4. **MODIFIED** `src/scrapers/mod.rs` - Export new modules
5. **MODIFIED** `src/vault/mod.rs` - Export book_access
6. **MODIFIED** `src/vault/fast15m_reactive.rs` - Remove REST fallback
7. **MODIFIED** `src/vault/engine.rs` - Add cached HFT functions
8. **MODIFIED** `src/main.rs` - Add hft_book_cache to AppState
9. **MODIFIED** `Cargo.toml` - Add arc-swap dependency

## Testing

### Unit Tests
```bash
cargo test polymarket_book_store -- --nocapture
```

### Integration Testing

1. **Verify No REST Calls**:
   - Enable HFT cache: `HFT_BOOK_CACHE_ENABLED=1`
   - Run trading loop
   - Monitor REST call counters (should be 0 after warmup)

2. **Force Disconnect**:
   - Kill WS connection
   - Verify books marked not ready
   - Verify skip-tick behavior (no REST fallback)
   - Verify reconnection

3. **Performance**:
   - Target: <100ns per read
   - Use `book_store.get_book()` in tight loop
   - Measure with `std::time::Instant`

## Migration Path

The implementation is backward compatible:

1. **Opt-in**: Set `HFT_BOOK_CACHE_ENABLED=1` to enable
2. **Fallback**: Without flag, uses existing `polymarket_market_ws`
3. **Gradual**: Individual strategies can switch to `best_ask_cached_hft`

## Future Improvements

1. **Delta support**: Polymarket currently sends full snapshots; delta path is ready
2. **REST warmup**: Optional one-time REST fetch during warmup only
3. **Multi-universe**: Support different universes per strategy
4. **Persistence**: Cache state across restarts

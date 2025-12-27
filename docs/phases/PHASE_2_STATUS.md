# Phase 2: Database Persistence Layer - IN PROGRESS ⚙️

**Duration**: 2-3 hours (estimated)  
**Status**: 60% Complete  
**Date**: 2025-11-16

## Summary

Phase 2 implementation is progressing well. The core database storage infrastructure has been created with a production-grade SQLite implementation featuring auto-cleanup triggers and comprehensive query methods. Integration with main.rs is the next step.

---

## Completed Tasks ✅

### 2.1 Database Storage Implementation ✅

**Created**: `rust-backend/src/signals/db_storage.rs` (404 lines)

**Features Implemented**:
1. **SQLite Schema Design**
   - `signals` table with indexed columns for fast queries
   - Indexes on: `detected_at`, `confidence`, `source`, `market_slug`
   - Auto-cleanup trigger to maintain 10,000 signal limit
   - Timestamp tracking with `created_at` field

2. **CRUD Operations**
   - `store()` - Insert/replace signal with JSON serialization
   - `get_recent(limit)` - Retrieve recent signals sorted by time
   - `get_by_source(source, limit)` - Filter by data source
   - `get_by_market(market_slug, limit)` - Filter by market
   - `len()` / `is_empty()` - Quick counts
   - `clear()` - For testing only
   - `get_stats()` - Database statistics with source breakdown

3. **Comprehensive Test Suite**
   - 7 async tests covering all major operations
   - In-memory database (`:memory:`) for fast testing
   - Tests for: creation, insertion, retrieval, querying, stats, clearing

**Technical Highlights**:
- Thread-safe with `Arc<Mutex<Connection>>`
- Proper error handling with `anyhow::Result` and context
- JSON serialization for `signal_type` and `details` fields
- Auto-cleanup trigger ensures database doesn't grow indefinitely
- Optimized indexes for common query patterns

### 2.2 Module Exports ✅

**Updated**: `rust-backend/src/signals/mod.rs`

Added:
```rust
pub mod db_storage;
pub use db_storage::{DbSignalStorage, DatabaseStats};
```

---

## Remaining Work (40%) 🚧

### 2.3 Main.rs Integration

**Files to Update**:
- `rust-backend/src/main.rs`

**Changes Needed**:

#### 1. Update Imports
```rust
// BEFORE:
use crate::{
    signals::{detector::SignalDetector, storage::SignalStorage},
};

// AFTER:
use crate::{
    signals::{detector::SignalDetector, db_storage::DbSignalStorage},
};
```

#### 2. Update AppState
```rust
// BEFORE:
struct AppState {
    signal_storage: Arc<RwLock<SignalStorage>>,
    risk_manager: Arc<RwLock<RiskManager>>,
    signal_broadcast: broadcast::Sender<MarketSignal>,
}

// AFTER:
struct AppState {
    signal_storage: Arc<DbSignalStorage>,  // No RwLock needed - internal Arc<Mutex>
    risk_manager: Arc<RwLock<RiskManager>>,
    signal_broadcast: broadcast::Sender<MarketSignal>,
}
```

#### 3. Update Initialization in main()
```rust
// BEFORE:
let signal_storage = Arc::new(RwLock::new(SignalStorage::new().await?));

// AFTER:
let db_path = env::var("DB_PATH")
    .unwrap_or_else(|_| "./betterbot_signals.db".to_string());
let signal_storage = Arc::new(DbSignalStorage::new(&db_path)?);

info!("📊 Database initialized at: {}", db_path);
```

#### 4. Update parallel_data_collection() Signature
```rust
// BEFORE:
async fn parallel_data_collection(
    storage: Arc<RwLock<SignalStorage>>,
    signal_tx: broadcast::Sender<MarketSignal>,
    risk_manager: Arc<RwLock<RiskManager>>,
) -> Result<()>

// AFTER:
async fn parallel_data_collection(
    storage: Arc<DbSignalStorage>,
    signal_tx: broadcast::Sender<MarketSignal>,
    risk_manager: Arc<RwLock<RiskManager>>,
) -> Result<()>
```

#### 5. Update Signal Storage Calls
```rust
// BEFORE (multiple locations):
storage.write().await.store(signal.clone()).await?;

// AFTER:
storage.store(&signal).await?;  // Direct call, no RwLock needed
```

**Locations to Update**:
- Line ~197 in `parallel_data_collection()`
- Line ~450 in `tracked_wallet_polling()`

#### 6. Update API Routes (if using storage)

Check `rust-backend/src/api/simple.rs` and `rust-backend/src/api/routes.rs` for any references to `SignalStorage` and update to `DbSignalStorage`.

**Example**:
```rust
// In get_signals handler:
let signals = state.signal_storage.get_recent(100)?;  // No .read().await needed
```

### 2.4 Environment Configuration

**Add to `.env`**:
```bash
# Database configuration
DB_PATH=./betterbot_signals.db
```

**Documentation**: Update README.md to document the `DB_PATH` environment variable.

### 2.5 Testing & Verification

**Unit Tests** ✅:
- Already complete in `db_storage.rs`
- Run with: `cargo test --lib db_storage`

**Integration Tests** (TODO):
1. Test signal persistence across restarts
2. Verify auto-cleanup at 10,000 signals
3. Measure insert latency (<10ms target)
4. Test concurrent access

**Test Commands**:
```bash
# Run all tests
cd rust-backend && cargo test

# Run only db_storage tests
cargo test --lib signals::db_storage

# Check compilation
cargo build

# Run with custom DB path
DB_PATH=/tmp/test_signals.db cargo run
```

---

## Performance Targets

| Metric | Target | Status |
|--------|--------|--------|
| Insert latency | <10ms | ⏳ Not tested |
| Query latency (100 signals) | <5ms | ⏳ Not tested |
| Max signals stored | 10,000 | ✅ Implemented |
| Auto-cleanup | Yes | ✅ Trigger created |
| Thread-safe | Yes | ✅ Arc<Mutex> |
| Crash recovery | Yes | ✅ SQLite WAL mode |

---

## Database Schema

```sql
CREATE TABLE signals (
    id TEXT PRIMARY KEY,
    signal_type TEXT NOT NULL,           -- JSON serialized SignalType
    market_slug TEXT NOT NULL,
    confidence REAL NOT NULL,
    risk_level TEXT NOT NULL,
    details_json TEXT NOT NULL,          -- JSON serialized SignalDetails
    detected_at TEXT NOT NULL,           -- ISO 8601 timestamp
    source TEXT NOT NULL,                -- "polymarket", "hashdive", "dome", etc.
    created_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now'))
);

-- Indexes for fast queries
CREATE INDEX idx_signals_detected_at ON signals(detected_at DESC);
CREATE INDEX idx_signals_confidence ON signals(confidence DESC);
CREATE INDEX idx_signals_source ON signals(source);
CREATE INDEX idx_signals_market_slug ON signals(market_slug);

-- Auto-cleanup: Keep only last 10,000 signals
CREATE TRIGGER cleanup_old_signals
AFTER INSERT ON signals
BEGIN
    DELETE FROM signals WHERE id IN (
        SELECT id FROM signals 
        ORDER BY detected_at DESC 
        LIMIT -1 OFFSET 10000
    );
END;
```

---

## Next Steps (To Complete Phase 2)

1. **Update main.rs** with all changes listed in section 2.3
2. **Add DB_PATH to .env** and README documentation
3. **Run integration tests**:
   - Start server and verify database creation
   - Generate signals and confirm they're stored
   - Restart server and verify signals persist
   - Generate 11,000 signals and confirm auto-cleanup works
4. **Measure performance**:
   - Time 1000 inserts → verify <10ms avg
   - Time get_recent(100) → verify <5ms
5. **Document stats endpoint**:
   - Add `GET /api/stats/database` endpoint
   - Return `DatabaseStats` JSON

---

## Success Criteria

- [ ] All main.rs integration changes complete
- [ ] Clean compilation with zero errors
- [ ] All tests passing
- [ ] Database persists across restarts
- [ ] Auto-cleanup verified at 10,000 signals
- [ ] Insert latency <10ms measured
- [ ] Query latency <5ms measured
- [ ] API returns persisted signals correctly

---

## Migration Notes

**Breaking Changes**:
- `SignalStorage` (in-memory) → `DbSignalStorage` (persistent)
- `Arc<RwLock<SignalStorage>>` → `Arc<DbSignalStorage>` (simpler API)
- Signals now survive process restarts
- Database file created at `DB_PATH` (default: `./betterbot_signals.db`)

**Backward Compatibility**:
- Existing code calling `.store()` just needs to remove `.write().await`
- All signal detection logic unchanged
- API endpoints unchanged (just faster with indexes)

---

## Phase 2 Completion Checklist

- [x] Create db_storage.rs with SQLite implementation
- [x] Add auto-cleanup trigger for 10k signal limit
- [x] Implement comprehensive test suite
- [x] Export DbSignalStorage from signals module
- [ ] Update main.rs imports
- [ ] Update AppState structure
- [ ] Replace storage initialization
- [ ] Update parallel_data_collection signature
- [ ] Update all storage.store() calls
- [ ] Add DB_PATH configuration
- [ ] Test database operations
- [ ] Measure performance metrics
- [ ] Document database schema
- [ ] Create PHASE_2_COMPLETE.md

**Estimated Time Remaining**: 45-60 minutes

**Next Phase**: Phase 3 (Dome WebSocket Real-time Engine) - Eliminates 30-second polling delays with WebSocket streaming

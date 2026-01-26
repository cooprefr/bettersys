//! HFT-Grade L2 Delta Storage
//!
//! Persistent storage for Polymarket L2 snapshots and deltas with:
//! - Append-only writes preserving arrival order
//! - Strict sequence validation
//! - Metadata tracking for dataset classification
//! - Efficient replay loading by time range

use crate::backtest_v2::clock::Nanos;
use crate::backtest_v2::events::Side;
use crate::backtest_v2::l2_delta::{
    BookFingerprint, EventTime, L2DatasetMetadata, PolymarketL2Delta, PolymarketL2Snapshot, 
    SequenceOrigin, SequenceScope, TickPriceLevel, POLYMARKET_TICK_SIZE,
};
use anyhow::{Context, Result};
use parking_lot::Mutex;
use rusqlite::{params, Connection, OpenFlags, Row};
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

// =============================================================================
// STORAGE SCHEMA
// =============================================================================

const L2_STORAGE_SCHEMA: &str = r#"
-- Enable optimizations
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;
PRAGMA cache_size = -64000;
PRAGMA temp_store = MEMORY;
PRAGMA mmap_size = 268435456;

-- ==========================================================================
-- L2 SNAPSHOTS TABLE
-- ==========================================================================
CREATE TABLE IF NOT EXISTS l2_snapshots (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    market_id TEXT NOT NULL,
    token_id TEXT NOT NULL,
    seq_snapshot INTEGER NOT NULL,
    exchange_ts INTEGER,
    ingest_ts INTEGER NOT NULL,
    bid_count INTEGER NOT NULL,
    ask_count INTEGER NOT NULL,
    total_bid_depth_fp INTEGER NOT NULL,
    total_ask_depth_fp INTEGER NOT NULL,
    -- Serialized levels (JSON for simplicity, could use BLOB for perf)
    bids_json TEXT NOT NULL,
    asks_json TEXT NOT NULL,
    -- Fingerprint for verification
    fingerprint_hash INTEGER NOT NULL,
    recorded_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now'))
);

-- Primary index: token + ingest time (for replay ordering)
CREATE INDEX IF NOT EXISTS idx_l2_snapshots_token_ingest
    ON l2_snapshots(token_id, ingest_ts);

-- Index for sequence-based lookup
CREATE INDEX IF NOT EXISTS idx_l2_snapshots_token_seq
    ON l2_snapshots(token_id, seq_snapshot);

-- Index for market-based queries
CREATE INDEX IF NOT EXISTS idx_l2_snapshots_market_ingest
    ON l2_snapshots(market_id, ingest_ts);

-- ==========================================================================
-- L2 DELTAS TABLE
-- ==========================================================================
CREATE TABLE IF NOT EXISTS l2_deltas (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    market_id TEXT NOT NULL,
    token_id TEXT NOT NULL,
    side TEXT NOT NULL CHECK (side IN ('BUY', 'SELL')),
    price_ticks INTEGER NOT NULL,
    size_fp INTEGER NOT NULL,
    is_absolute INTEGER NOT NULL DEFAULT 1,
    seq INTEGER NOT NULL,
    exchange_ts INTEGER,
    ingest_ts INTEGER NOT NULL,
    seq_hash TEXT,
    recorded_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now'))
);

-- Primary index: token + ingest time + seq (deterministic replay order)
CREATE INDEX IF NOT EXISTS idx_l2_deltas_token_ingest_seq
    ON l2_deltas(token_id, ingest_ts, seq);

-- Index for sequence-based queries
CREATE INDEX IF NOT EXISTS idx_l2_deltas_token_seq
    ON l2_deltas(token_id, seq);

-- Index for market-based queries
CREATE INDEX IF NOT EXISTS idx_l2_deltas_market_ingest
    ON l2_deltas(market_id, ingest_ts);

-- Index for deduplication by seq_hash
CREATE INDEX IF NOT EXISTS idx_l2_deltas_token_hash
    ON l2_deltas(token_id, seq_hash) WHERE seq_hash IS NOT NULL;

-- ==========================================================================
-- METADATA TABLE
-- ==========================================================================
CREATE TABLE IF NOT EXISTS l2_metadata (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
) WITHOUT ROWID;

-- ==========================================================================
-- TOKEN STATS TABLE
-- ==========================================================================
CREATE TABLE IF NOT EXISTS l2_token_stats (
    token_id TEXT PRIMARY KEY,
    market_id TEXT NOT NULL,
    first_snapshot_seq INTEGER,
    last_snapshot_seq INTEGER,
    first_delta_seq INTEGER,
    last_delta_seq INTEGER,
    first_ingest_ts INTEGER NOT NULL,
    last_ingest_ts INTEGER NOT NULL,
    snapshot_count INTEGER NOT NULL DEFAULT 0,
    delta_count INTEGER NOT NULL DEFAULT 0,
    sequence_gaps INTEGER NOT NULL DEFAULT 0,
    duplicates_skipped INTEGER NOT NULL DEFAULT 0,
    updated_at INTEGER NOT NULL
) WITHOUT ROWID;

-- ==========================================================================
-- SEQUENCE GAP LOG
-- ==========================================================================
CREATE TABLE IF NOT EXISTS l2_sequence_gaps (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    token_id TEXT NOT NULL,
    scope_key TEXT NOT NULL,
    gap_start INTEGER NOT NULL,
    gap_end INTEGER NOT NULL,
    gap_size INTEGER NOT NULL,
    detected_at_ingest_ts INTEGER NOT NULL,
    healed_by_snapshot_seq INTEGER,
    recorded_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now'))
);

CREATE INDEX IF NOT EXISTS idx_l2_gaps_token
    ON l2_sequence_gaps(token_id, gap_start);

-- ==========================================================================
-- CHECKPOINT FINGERPRINTS
-- ==========================================================================
CREATE TABLE IF NOT EXISTS l2_fingerprints (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    token_id TEXT NOT NULL,
    seq INTEGER NOT NULL,
    fingerprint_hash INTEGER NOT NULL,
    bid_levels INTEGER NOT NULL,
    ask_levels INTEGER NOT NULL,
    computed_at_ingest_ts INTEGER NOT NULL,
    recorded_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now'))
);

CREATE INDEX IF NOT EXISTS idx_l2_fingerprints_token_seq
    ON l2_fingerprints(token_id, seq);
"#;

// =============================================================================
// STORAGE STATS
// =============================================================================

/// Recording statistics.
#[derive(Debug, Default)]
pub struct L2StorageStats {
    pub snapshots_stored: AtomicU64,
    pub deltas_stored: AtomicU64,
    pub deltas_skipped_duplicate: AtomicU64,
    pub sequence_gaps_detected: AtomicU64,
    pub batch_writes: AtomicU64,
    pub fingerprints_stored: AtomicU64,
}

impl L2StorageStats {
    /// Get a summary string.
    pub fn summary(&self) -> String {
        format!(
            "snapshots={}, deltas={}, duplicates_skipped={}, gaps={}, batches={}",
            self.snapshots_stored.load(Ordering::Relaxed),
            self.deltas_stored.load(Ordering::Relaxed),
            self.deltas_skipped_duplicate.load(Ordering::Relaxed),
            self.sequence_gaps_detected.load(Ordering::Relaxed),
            self.batch_writes.load(Ordering::Relaxed),
        )
    }
}

// =============================================================================
// L2 STORAGE
// =============================================================================

/// Persistent storage for L2 snapshots and deltas.
pub struct L2Storage {
    conn: Arc<Mutex<Connection>>,
    /// Per-token last sequence (for gap detection).
    last_seq: Mutex<HashMap<String, u64>>,
    /// Per-token last seq_hash (for duplicate detection).
    last_hash: Mutex<HashMap<String, String>>,
    /// Sequence scope used by this storage.
    seq_scope: SequenceScope,
    /// Sequence origin classification.
    seq_origin: SequenceOrigin,
    /// Tick size for price encoding.
    tick_size: f64,
    /// Statistics.
    stats: L2StorageStats,
}

impl L2Storage {
    /// Open or create storage.
    pub fn open(db_path: &str, seq_scope: SequenceScope, seq_origin: SequenceOrigin) -> Result<Self> {
        let path = Path::new(db_path);

        // Create directory if needed
        if let Some(parent) = path.parent() {
            if !parent.exists() && !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }

        let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX;

        let conn = Connection::open_with_flags(db_path, flags)
            .with_context(|| format!("Failed to open L2 storage: {}", db_path))?;

        // Initialize schema
        conn.execute_batch(L2_STORAGE_SCHEMA)?;

        info!(
            path = %db_path,
            seq_scope = ?seq_scope,
            seq_origin = ?seq_origin,
            "L2 storage opened"
        );

        let storage = Self {
            conn: Arc::new(Mutex::new(conn)),
            last_seq: Mutex::new(HashMap::new()),
            last_hash: Mutex::new(HashMap::new()),
            seq_scope,
            seq_origin,
            tick_size: POLYMARKET_TICK_SIZE,
            stats: L2StorageStats::default(),
        };

        // Store metadata
        storage.set_metadata("seq_scope", &format!("{:?}", seq_scope))?;
        storage.set_metadata("seq_origin", &format!("{:?}", seq_origin))?;
        storage.set_metadata("tick_size", &storage.tick_size.to_string())?;

        Ok(storage)
    }

    /// Open in-memory storage (for testing).
    pub fn open_memory(seq_scope: SequenceScope, seq_origin: SequenceOrigin) -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(L2_STORAGE_SCHEMA)?;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            last_seq: Mutex::new(HashMap::new()),
            last_hash: Mutex::new(HashMap::new()),
            seq_scope,
            seq_origin,
            tick_size: POLYMARKET_TICK_SIZE,
            stats: L2StorageStats::default(),
        })
    }

    /// Set a metadata value.
    pub fn set_metadata(&self, key: &str, value: &str) -> Result<()> {
        let conn = self.conn.lock();
        conn.execute(
            "INSERT OR REPLACE INTO l2_metadata (key, value) VALUES (?1, ?2)",
            params![key, value],
        )?;
        Ok(())
    }

    /// Get a metadata value.
    pub fn get_metadata(&self, key: &str) -> Result<Option<String>> {
        let conn = self.conn.lock();
        let result = conn.query_row(
            "SELECT value FROM l2_metadata WHERE key = ?1",
            params![key],
            |row| row.get(0),
        );
        match result {
            Ok(v) => Ok(Some(v)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Store a snapshot.
    pub fn store_snapshot(&self, snapshot: &PolymarketL2Snapshot) -> Result<()> {
        let bids_json = serde_json::to_string(&snapshot.bids)?;
        let asks_json = serde_json::to_string(&snapshot.asks)?;
        let fingerprint = snapshot.fingerprint();

        let conn = self.conn.lock();
        conn.execute(
            r#"
            INSERT INTO l2_snapshots (
                market_id, token_id, seq_snapshot, exchange_ts, ingest_ts,
                bid_count, ask_count, total_bid_depth_fp, total_ask_depth_fp,
                bids_json, asks_json, fingerprint_hash
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
            "#,
            params![
                snapshot.market_id,
                snapshot.token_id,
                snapshot.seq_snapshot as i64,
                snapshot.time.exchange_ts.map(|t| t as i64),
                snapshot.time.ingest_ts as i64,
                snapshot.bids.len() as i64,
                snapshot.asks.len() as i64,
                snapshot.total_bid_depth_fp,
                snapshot.total_ask_depth_fp,
                bids_json,
                asks_json,
                fingerprint.hash as i64,
            ],
        )?;

        // Update sequence tracking
        {
            let mut last_seq = self.last_seq.lock();
            let scope_key = self.seq_scope.scope_key(&snapshot.market_id, Side::Buy);
            last_seq.insert(scope_key.clone(), snapshot.seq_snapshot);
            if self.seq_scope == SequenceScope::PerMarketSide {
                let ask_scope = self.seq_scope.scope_key(&snapshot.market_id, Side::Sell);
                last_seq.insert(ask_scope, snapshot.seq_snapshot);
            }
        }

        self.stats.snapshots_stored.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    /// Store a delta with sequence validation.
    ///
    /// Returns true if stored, false if skipped (duplicate).
    pub fn store_delta(&self, delta: &PolymarketL2Delta) -> Result<bool> {
        // Duplicate detection by seq_hash
        if let Some(ref hash) = delta.seq_hash {
            let mut last_hash = self.last_hash.lock();
            if let Some(prev) = last_hash.get(&delta.token_id) {
                if prev == hash {
                    self.stats.deltas_skipped_duplicate.fetch_add(1, Ordering::Relaxed);
                    return Ok(false);
                }
            }
            last_hash.insert(delta.token_id.clone(), hash.clone());
        }

        // Sequence gap detection
        let scope_key = self.seq_scope.scope_key(&delta.market_id, delta.side);
        {
            let mut last_seq = self.last_seq.lock();
            if let Some(&prev_seq) = last_seq.get(&scope_key) {
                if delta.seq <= prev_seq {
                    // Non-monotone, skip (handled by caller if needed)
                    debug!(
                        scope = %scope_key,
                        prev_seq = prev_seq,
                        delta_seq = delta.seq,
                        "Skipping non-monotone delta"
                    );
                    return Ok(false);
                }
                
                let expected = prev_seq + 1;
                if delta.seq > expected {
                    let gap_size = delta.seq - expected;
                    self.stats.sequence_gaps_detected.fetch_add(1, Ordering::Relaxed);
                    
                    // Log the gap
                    let conn = self.conn.lock();
                    conn.execute(
                        r#"
                        INSERT INTO l2_sequence_gaps (
                            token_id, scope_key, gap_start, gap_end, gap_size, detected_at_ingest_ts
                        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                        "#,
                        params![
                            delta.token_id,
                            scope_key,
                            expected as i64,
                            delta.seq as i64 - 1,
                            gap_size as i64,
                            delta.time.ingest_ts as i64,
                        ],
                    )?;
                }
            }
            last_seq.insert(scope_key, delta.seq);
        }

        // Store the delta
        let side_str = match delta.side {
            Side::Buy => "BUY",
            Side::Sell => "SELL",
        };

        let conn = self.conn.lock();
        conn.execute(
            r#"
            INSERT INTO l2_deltas (
                market_id, token_id, side, price_ticks, size_fp, is_absolute,
                seq, exchange_ts, ingest_ts, seq_hash
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
            "#,
            params![
                delta.market_id,
                delta.token_id,
                side_str,
                delta.price_ticks,
                delta.size_fp,
                delta.is_absolute as i64,
                delta.seq as i64,
                delta.time.exchange_ts.map(|t| t as i64),
                delta.time.ingest_ts as i64,
                delta.seq_hash,
            ],
        )?;

        self.stats.deltas_stored.fetch_add(1, Ordering::Relaxed);
        Ok(true)
    }

    /// Store a batch of deltas.
    pub fn store_delta_batch(&self, deltas: &[PolymarketL2Delta]) -> Result<usize> {
        if deltas.is_empty() {
            return Ok(0);
        }

        let conn = self.conn.lock();
        conn.execute("BEGIN IMMEDIATE", [])?;

        let mut stored = 0;
        for delta in deltas {
            // Skip duplicates (simplified batch check)
            if let Some(ref hash) = delta.seq_hash {
                let mut last_hash = self.last_hash.lock();
                if let Some(prev) = last_hash.get(&delta.token_id) {
                    if prev == hash {
                        continue;
                    }
                }
                last_hash.insert(delta.token_id.clone(), hash.clone());
            }

            let side_str = match delta.side {
                Side::Buy => "BUY",
                Side::Sell => "SELL",
            };

            conn.execute(
                r#"
                INSERT INTO l2_deltas (
                    market_id, token_id, side, price_ticks, size_fp, is_absolute,
                    seq, exchange_ts, ingest_ts, seq_hash
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                "#,
                params![
                    delta.market_id,
                    delta.token_id,
                    side_str,
                    delta.price_ticks,
                    delta.size_fp,
                    delta.is_absolute as i64,
                    delta.seq as i64,
                    delta.time.exchange_ts.map(|t| t as i64),
                    delta.time.ingest_ts as i64,
                    delta.seq_hash,
                ],
            )?;
            stored += 1;
        }

        conn.execute("COMMIT", [])?;

        self.stats.deltas_stored.fetch_add(stored as u64, Ordering::Relaxed);
        self.stats.batch_writes.fetch_add(1, Ordering::Relaxed);

        Ok(stored)
    }

    /// Store a checkpoint fingerprint.
    pub fn store_fingerprint(&self, token_id: &str, fp: &BookFingerprint, ingest_ts: Nanos) -> Result<()> {
        let conn = self.conn.lock();
        conn.execute(
            r#"
            INSERT INTO l2_fingerprints (
                token_id, seq, fingerprint_hash, bid_levels, ask_levels, computed_at_ingest_ts
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
            "#,
            params![
                token_id,
                fp.seq as i64,
                fp.hash as i64,
                fp.bid_levels as i64,
                fp.ask_levels as i64,
                ingest_ts as i64,
            ],
        )?;
        self.stats.fingerprints_stored.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    /// Load snapshots for a token in a time range.
    pub fn load_snapshots(
        &self,
        token_id: &str,
        start_ns: Nanos,
        end_ns: Nanos,
    ) -> Result<Vec<PolymarketL2Snapshot>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            r#"
            SELECT market_id, token_id, seq_snapshot, exchange_ts, ingest_ts,
                   total_bid_depth_fp, total_ask_depth_fp, bids_json, asks_json
            FROM l2_snapshots
            WHERE token_id = ?1 AND ingest_ts >= ?2 AND ingest_ts < ?3
            ORDER BY ingest_ts ASC, seq_snapshot ASC
            "#,
        )?;

        let rows = stmt.query_map(
            params![token_id, start_ns as i64, end_ns as i64],
            |row| Self::row_to_snapshot(row),
        )?;

        let mut snapshots = Vec::new();
        for row in rows {
            snapshots.push(row?);
        }

        Ok(snapshots)
    }

    /// Load the first snapshot for a token (initial state).
    pub fn load_initial_snapshot(&self, token_id: &str) -> Result<Option<PolymarketL2Snapshot>> {
        let conn = self.conn.lock();
        let result = conn.query_row(
            r#"
            SELECT market_id, token_id, seq_snapshot, exchange_ts, ingest_ts,
                   total_bid_depth_fp, total_ask_depth_fp, bids_json, asks_json
            FROM l2_snapshots
            WHERE token_id = ?1
            ORDER BY ingest_ts ASC, seq_snapshot ASC
            LIMIT 1
            "#,
            params![token_id],
            |row| Self::row_to_snapshot(row),
        );

        match result {
            Ok(snapshot) => Ok(Some(snapshot)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Load deltas for a token in a time range.
    pub fn load_deltas(
        &self,
        token_id: &str,
        start_ns: Nanos,
        end_ns: Nanos,
    ) -> Result<Vec<PolymarketL2Delta>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            r#"
            SELECT market_id, token_id, side, price_ticks, size_fp, is_absolute,
                   seq, exchange_ts, ingest_ts, seq_hash
            FROM l2_deltas
            WHERE token_id = ?1 AND ingest_ts >= ?2 AND ingest_ts < ?3
            ORDER BY ingest_ts ASC, seq ASC
            "#,
        )?;

        let rows = stmt.query_map(
            params![token_id, start_ns as i64, end_ns as i64],
            |row| Self::row_to_delta(row),
        )?;

        let mut deltas = Vec::new();
        for row in rows {
            deltas.push(row?);
        }

        Ok(deltas)
    }

    /// Load all deltas for a token.
    pub fn load_all_deltas(&self, token_id: &str) -> Result<Vec<PolymarketL2Delta>> {
        self.load_deltas(token_id, 0, i64::MAX as Nanos)
    }

    /// Load checkpoint fingerprints for a token.
    pub fn load_fingerprints(&self, token_id: &str) -> Result<Vec<BookFingerprint>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            r#"
            SELECT seq, fingerprint_hash, bid_levels, ask_levels
            FROM l2_fingerprints
            WHERE token_id = ?1
            ORDER BY seq ASC
            "#,
        )?;

        let rows = stmt.query_map(params![token_id], |row| {
            Ok(BookFingerprint {
                seq: row.get::<_, i64>(0)? as u64,
                hash: row.get::<_, i64>(1)? as u64,
                bid_levels: row.get::<_, i64>(2)? as u32,
                ask_levels: row.get::<_, i64>(3)? as u32,
            })
        })?;

        let mut fps = Vec::new();
        for row in rows {
            fps.push(row?);
        }

        Ok(fps)
    }

    /// Load sequence gaps for a token.
    pub fn load_sequence_gaps(&self, token_id: &str) -> Result<Vec<(String, u64, u64)>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            r#"
            SELECT scope_key, gap_start, gap_end
            FROM l2_sequence_gaps
            WHERE token_id = ?1
            ORDER BY gap_start ASC
            "#,
        )?;

        let rows = stmt.query_map(params![token_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)? as u64,
                row.get::<_, i64>(2)? as u64,
            ))
        })?;

        let mut gaps = Vec::new();
        for row in rows {
            gaps.push(row?);
        }

        Ok(gaps)
    }

    /// Get list of unique token IDs.
    pub fn list_tokens(&self) -> Result<Vec<String>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            r#"
            SELECT DISTINCT token_id FROM (
                SELECT token_id FROM l2_snapshots
                UNION
                SELECT token_id FROM l2_deltas
            )
            ORDER BY token_id
            "#,
        )?;

        let rows = stmt.query_map([], |row| row.get(0))?;

        let mut tokens = Vec::new();
        for row in rows {
            tokens.push(row?);
        }

        Ok(tokens)
    }

    /// Get time range covered by data for a token.
    pub fn time_range(&self, token_id: &str) -> Result<Option<(Nanos, Nanos)>> {
        let conn = self.conn.lock();
        let result = conn.query_row(
            r#"
            SELECT MIN(ingest_ts), MAX(ingest_ts) FROM (
                SELECT ingest_ts FROM l2_snapshots WHERE token_id = ?1
                UNION ALL
                SELECT ingest_ts FROM l2_deltas WHERE token_id = ?1
            )
            "#,
            params![token_id],
            |row| {
                let min: Option<i64> = row.get(0)?;
                let max: Option<i64> = row.get(1)?;
                Ok((min, max))
            },
        );

        match result {
            Ok((Some(min), Some(max))) => Ok(Some((min as Nanos, max as Nanos))),
            Ok(_) => Ok(None),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Get snapshot count for a token.
    pub fn snapshot_count(&self, token_id: &str) -> Result<u64> {
        let conn = self.conn.lock();
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM l2_snapshots WHERE token_id = ?1",
            params![token_id],
            |row| row.get(0),
        )?;
        Ok(count as u64)
    }

    /// Get delta count for a token.
    pub fn delta_count(&self, token_id: &str) -> Result<u64> {
        let conn = self.conn.lock();
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM l2_deltas WHERE token_id = ?1",
            params![token_id],
            |row| row.get(0),
        )?;
        Ok(count as u64)
    }

    /// Get gap count for a token.
    pub fn gap_count(&self, token_id: &str) -> Result<u64> {
        let conn = self.conn.lock();
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM l2_sequence_gaps WHERE token_id = ?1",
            params![token_id],
            |row| row.get(0),
        )?;
        Ok(count as u64)
    }

    /// Build dataset metadata.
    pub fn build_metadata(&self) -> Result<L2DatasetMetadata> {
        let tokens = self.list_tokens()?;
        
        let mut all_gaps = Vec::new();
        let mut total_snapshots = 0u64;
        let mut total_deltas = 0u64;
        let mut has_initial_snapshots = true;
        let mut time_min = i64::MAX as Nanos;
        let mut time_max = 0 as Nanos;
        let mut market_id = String::new();
        
        for token_id in &tokens {
            // Check for initial snapshot
            if self.load_initial_snapshot(token_id)?.is_none() {
                has_initial_snapshots = false;
            }
            
            // Aggregate counts
            total_snapshots += self.snapshot_count(token_id)?;
            total_deltas += self.delta_count(token_id)?;
            
            // Aggregate gaps
            let gaps = self.load_sequence_gaps(token_id)?;
            all_gaps.extend(gaps);
            
            // Time range
            if let Some((tmin, tmax)) = self.time_range(token_id)? {
                time_min = time_min.min(tmin);
                time_max = time_max.max(tmax);
            }
            
            // Get market_id from first snapshot
            if market_id.is_empty() {
                if let Some(snap) = self.load_initial_snapshot(token_id)? {
                    market_id = snap.market_id;
                }
            }
        }
        
        // Load checkpoint fingerprints
        let mut checkpoint_fingerprints = Vec::new();
        for token_id in &tokens {
            let fps = self.load_fingerprints(token_id)?;
            checkpoint_fingerprints.extend(fps);
        }
        
        // Collect warnings
        let mut warnings = Vec::new();
        if !all_gaps.is_empty() {
            warnings.push(format!("{} sequence gaps detected", all_gaps.len()));
        }
        if !self.seq_origin.is_production_grade() {
            warnings.push(format!("Sequence origin {:?} is not production-grade", self.seq_origin));
        }
        
        let now_ns = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
            .as_nanos() as Nanos;

        Ok(L2DatasetMetadata {
            version: "L2_V1".to_string(),
            market_id,
            token_ids: tokens,
            tick_size: self.tick_size,
            seq_scope: self.seq_scope,
            seq_origin: self.seq_origin,
            time_range_ns: (time_min, time_max),
            snapshot_count: total_snapshots,
            delta_count: total_deltas,
            has_initial_snapshots,
            sequence_gaps: all_gaps,
            checkpoint_fingerprints,
            recorded_at: now_ns,
            warnings,
        })
    }

    /// Get statistics.
    pub fn stats(&self) -> &L2StorageStats {
        &self.stats
    }

    /// Get sequence scope.
    pub fn seq_scope(&self) -> SequenceScope {
        self.seq_scope
    }

    /// Get sequence origin.
    pub fn seq_origin(&self) -> SequenceOrigin {
        self.seq_origin
    }

    /// Get tick size.
    pub fn tick_size(&self) -> f64 {
        self.tick_size
    }

    // -------------------------------------------------------------------------
    // Private helpers
    // -------------------------------------------------------------------------

    fn row_to_snapshot(row: &Row) -> rusqlite::Result<PolymarketL2Snapshot> {
        let bids_json: String = row.get(7)?;
        let asks_json: String = row.get(8)?;
        let bids: Vec<TickPriceLevel> = serde_json::from_str(&bids_json).unwrap_or_default();
        let asks: Vec<TickPriceLevel> = serde_json::from_str(&asks_json).unwrap_or_default();

        let exchange_ts: Option<i64> = row.get(3)?;
        let ingest_ts: i64 = row.get(4)?;

        Ok(PolymarketL2Snapshot {
            market_id: row.get(0)?,
            token_id: row.get(1)?,
            seq_snapshot: row.get::<_, i64>(2)? as u64,
            time: EventTime {
                exchange_ts: exchange_ts.map(|t| t as Nanos),
                ingest_ts: ingest_ts as Nanos,
                visible_ts: None,
            },
            total_bid_depth_fp: row.get(5)?,
            total_ask_depth_fp: row.get(6)?,
            bids,
            asks,
        })
    }

    fn row_to_delta(row: &Row) -> rusqlite::Result<PolymarketL2Delta> {
        let side_str: String = row.get(2)?;
        let side = match side_str.as_str() {
            "BUY" => Side::Buy,
            "SELL" => Side::Sell,
            _ => Side::Buy,
        };

        let exchange_ts: Option<i64> = row.get(7)?;
        let ingest_ts: i64 = row.get(8)?;

        Ok(PolymarketL2Delta {
            market_id: row.get(0)?,
            token_id: row.get(1)?,
            side,
            price_ticks: row.get(3)?,
            size_fp: row.get(4)?,
            is_absolute: row.get::<_, i64>(5)? != 0,
            seq: row.get::<_, i64>(6)? as u64,
            time: EventTime {
                exchange_ts: exchange_ts.map(|t| t as Nanos),
                ingest_ts: ingest_ts as Nanos,
                visible_ts: None,
            },
            seq_hash: row.get(9)?,
        })
    }
}

// =============================================================================
// ASYNC RECORDER
// =============================================================================

/// Message for the async recorder.
pub enum L2RecorderMessage {
    Snapshot(PolymarketL2Snapshot),
    Delta(PolymarketL2Delta),
    DeltaBatch(Vec<PolymarketL2Delta>),
    Fingerprint(String, BookFingerprint, Nanos),
    Flush,
    Shutdown,
}

/// Async L2 recorder with buffered writes.
pub struct AsyncL2Recorder {
    tx: mpsc::Sender<L2RecorderMessage>,
}

impl AsyncL2Recorder {
    /// Spawn the async recorder.
    pub fn spawn(storage: Arc<L2Storage>, buffer_size: usize) -> Self {
        let (tx, rx) = mpsc::channel(buffer_size);

        tokio::spawn(async move {
            Self::run_writer(storage, rx, buffer_size).await;
        });

        Self { tx }
    }

    /// Record a snapshot (non-blocking).
    pub fn record_snapshot(&self, snapshot: PolymarketL2Snapshot) {
        let _ = self.tx.try_send(L2RecorderMessage::Snapshot(snapshot));
    }

    /// Record a delta (non-blocking).
    pub fn record_delta(&self, delta: PolymarketL2Delta) {
        let _ = self.tx.try_send(L2RecorderMessage::Delta(delta));
    }

    /// Record a batch of deltas (non-blocking).
    pub fn record_delta_batch(&self, deltas: Vec<PolymarketL2Delta>) {
        let _ = self.tx.try_send(L2RecorderMessage::DeltaBatch(deltas));
    }

    /// Record a checkpoint fingerprint.
    pub fn record_fingerprint(&self, token_id: String, fp: BookFingerprint, ingest_ts: Nanos) {
        let _ = self.tx.try_send(L2RecorderMessage::Fingerprint(token_id, fp, ingest_ts));
    }

    /// Flush pending writes.
    pub async fn flush(&self) {
        let _ = self.tx.send(L2RecorderMessage::Flush).await;
    }

    /// Shutdown the recorder.
    pub async fn shutdown(&self) {
        let _ = self.tx.send(L2RecorderMessage::Shutdown).await;
    }

    async fn run_writer(
        storage: Arc<L2Storage>,
        mut rx: mpsc::Receiver<L2RecorderMessage>,
        buffer_size: usize,
    ) {
        let mut delta_buffer: Vec<PolymarketL2Delta> = Vec::with_capacity(buffer_size);
        let flush_interval = Duration::from_millis(100);
        let mut last_flush = std::time::Instant::now();

        loop {
            tokio::select! {
                msg = rx.recv() => {
                    match msg {
                        Some(L2RecorderMessage::Snapshot(snapshot)) => {
                            if let Err(e) = storage.store_snapshot(&snapshot) {
                                warn!(error = %e, "Failed to store L2 snapshot");
                            }
                        }
                        Some(L2RecorderMessage::Delta(delta)) => {
                            delta_buffer.push(delta);

                            if delta_buffer.len() >= buffer_size || last_flush.elapsed() > flush_interval {
                                if let Err(e) = storage.store_delta_batch(&delta_buffer) {
                                    warn!(error = %e, "Failed to store L2 delta batch");
                                }
                                delta_buffer.clear();
                                last_flush = std::time::Instant::now();
                            }
                        }
                        Some(L2RecorderMessage::DeltaBatch(deltas)) => {
                            if let Err(e) = storage.store_delta_batch(&deltas) {
                                warn!(error = %e, "Failed to store L2 delta batch");
                            }
                        }
                        Some(L2RecorderMessage::Fingerprint(token_id, fp, ingest_ts)) => {
                            if let Err(e) = storage.store_fingerprint(&token_id, &fp, ingest_ts) {
                                warn!(error = %e, "Failed to store fingerprint");
                            }
                        }
                        Some(L2RecorderMessage::Flush) => {
                            if !delta_buffer.is_empty() {
                                if let Err(e) = storage.store_delta_batch(&delta_buffer) {
                                    warn!(error = %e, "Failed to store L2 delta batch on flush");
                                }
                                delta_buffer.clear();
                            }
                            last_flush = std::time::Instant::now();
                        }
                        Some(L2RecorderMessage::Shutdown) | None => {
                            // Final flush
                            if !delta_buffer.is_empty() {
                                let _ = storage.store_delta_batch(&delta_buffer);
                            }
                            info!("L2 recorder shutting down");
                            return;
                        }
                    }
                }
                _ = tokio::time::sleep(flush_interval) => {
                    if !delta_buffer.is_empty() {
                        if let Err(e) = storage.store_delta_batch(&delta_buffer) {
                            warn!(error = %e, "Failed to store L2 delta batch on timer");
                        }
                        delta_buffer.clear();
                        last_flush = std::time::Instant::now();
                    }
                }
            }
        }
    }
}

// =============================================================================
// TESTS
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backtest_v2::l2_delta::TickPriceLevel;

    fn make_test_snapshot(seq: u64, ingest_ts: Nanos) -> PolymarketL2Snapshot {
        PolymarketL2Snapshot {
            market_id: "market1".to_string(),
            token_id: "token1".to_string(),
            seq_snapshot: seq,
            bids: vec![
                TickPriceLevel { price_ticks: 4500, size_fp: 1000_00000000 },
                TickPriceLevel { price_ticks: 4400, size_fp: 2000_00000000 },
            ],
            asks: vec![
                TickPriceLevel { price_ticks: 5500, size_fp: 1500_00000000 },
                TickPriceLevel { price_ticks: 5600, size_fp: 2500_00000000 },
            ],
            time: EventTime::ingest_only(ingest_ts),
            total_bid_depth_fp: 3000_00000000,
            total_ask_depth_fp: 4000_00000000,
        }
    }

    fn make_test_delta(seq: u64, side: Side, price_ticks: i64, size_fp: i64, ingest_ts: Nanos) -> PolymarketL2Delta {
        PolymarketL2Delta::absolute(
            "market1".to_string(),
            "token1".to_string(),
            side,
            price_ticks,
            size_fp,
            seq,
            EventTime::ingest_only(ingest_ts),
            Some(format!("hash_{}", seq)),
        )
    }

    #[test]
    fn test_storage_snapshot_roundtrip() {
        let storage = L2Storage::open_memory(SequenceScope::PerMarket, SequenceOrigin::SyntheticFromArrival).unwrap();

        let snapshot = make_test_snapshot(1, 1000000000);
        storage.store_snapshot(&snapshot).unwrap();

        let loaded = storage.load_snapshots("token1", 0, i64::MAX as Nanos).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].seq_snapshot, 1);
        assert_eq!(loaded[0].bids.len(), 2);
        assert_eq!(loaded[0].asks.len(), 2);
    }

    #[test]
    fn test_storage_delta_roundtrip() {
        let storage = L2Storage::open_memory(SequenceScope::PerMarket, SequenceOrigin::SyntheticFromArrival).unwrap();

        let delta = make_test_delta(1, Side::Buy, 4500, 100_00000000, 1000000000);
        storage.store_delta(&delta).unwrap();

        let loaded = storage.load_deltas("token1", 0, i64::MAX as Nanos).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].seq, 1);
        assert_eq!(loaded[0].side, Side::Buy);
    }

    #[test]
    fn test_storage_duplicate_detection() {
        let storage = L2Storage::open_memory(SequenceScope::PerMarket, SequenceOrigin::SyntheticFromArrival).unwrap();

        let delta1 = make_test_delta(1, Side::Buy, 4500, 100_00000000, 1000000000);
        let delta2 = PolymarketL2Delta::absolute(
            "market1".to_string(),
            "token1".to_string(),
            Side::Buy,
            4500,
            200_00000000, // Different size
            2,
            EventTime::ingest_only(1000001000),
            Some("hash_1".to_string()), // Same hash
        );

        assert!(storage.store_delta(&delta1).unwrap());
        assert!(!storage.store_delta(&delta2).unwrap()); // Should be skipped

        assert_eq!(storage.delta_count("token1").unwrap(), 1);
    }

    #[test]
    fn test_storage_gap_detection() {
        let storage = L2Storage::open_memory(SequenceScope::PerMarket, SequenceOrigin::SyntheticFromArrival).unwrap();

        let snapshot = make_test_snapshot(1, 1000000000);
        storage.store_snapshot(&snapshot).unwrap();

        // Delta at seq 2
        let delta1 = make_test_delta(2, Side::Buy, 4500, 100_00000000, 1000001000);
        storage.store_delta(&delta1).unwrap();

        // Skip seq 3-4, delta at seq 5
        let delta2 = make_test_delta(5, Side::Buy, 4600, 200_00000000, 1000002000);
        storage.store_delta(&delta2).unwrap();

        let gaps = storage.load_sequence_gaps("token1").unwrap();
        assert_eq!(gaps.len(), 1);
        assert_eq!(gaps[0].1, 3); // gap_start
        assert_eq!(gaps[0].2, 4); // gap_end
    }

    #[test]
    fn test_storage_metadata() {
        let storage = L2Storage::open_memory(SequenceScope::PerMarket, SequenceOrigin::SyntheticFromArrival).unwrap();

        let snapshot = make_test_snapshot(1, 1000000000);
        storage.store_snapshot(&snapshot).unwrap();

        for seq in 2..=10 {
            let delta = make_test_delta(seq, Side::Buy, 4500 + seq as i64, 100_00000000, 1000000000i64 + seq as i64 * 1000);
            storage.store_delta(&delta).unwrap();
        }

        let metadata = storage.build_metadata().unwrap();
        assert_eq!(metadata.token_ids, vec!["token1"]);
        assert_eq!(metadata.snapshot_count, 1);
        assert_eq!(metadata.delta_count, 9);
        assert!(metadata.has_initial_snapshots);
        assert!(!metadata.is_production_grade()); // Synthetic sequences
    }

    #[test]
    fn test_storage_fingerprint() {
        let storage = L2Storage::open_memory(SequenceScope::PerMarket, SequenceOrigin::SyntheticFromArrival).unwrap();

        let fp = BookFingerprint {
            hash: 12345678,
            seq: 10,
            bid_levels: 5,
            ask_levels: 5,
        };

        storage.store_fingerprint("token1", &fp, 1000000000).unwrap();

        let loaded = storage.load_fingerprints("token1").unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].hash, 12345678);
        assert_eq!(loaded[0].seq, 10);
    }
}

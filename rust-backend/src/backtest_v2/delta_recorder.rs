//! L2 Book Delta Recorder
//!
//! Persists incremental order book updates (`price_change` messages) from
//! Polymarket CLOB WebSocket with high-resolution arrival timestamps for
//! offline backtesting replay.
//!
//! Book deltas are CRITICAL for maker viability because they enable:
//! - Precise queue position tracking (each delta updates aggregate size at a level)
//! - Deterministic book reconstruction from deltas
//! - Proper queue consumption modeling
//!
//! **CRITICAL**: Arrival time is captured at the EARLIEST possible point
//! (WebSocket message receipt, BEFORE JSON parsing) to minimize measurement noise.
//!
//! ## Delta Semantics
//!
//! The `price_change` message contains the NEW AGGREGATE SIZE at each affected level,
//! not a delta. To compute actual change: `delta = new_size - previous_size`.
//!
//! ## Ordering Guarantees
//!
//! - `ingest_arrival_time_ns`: Captured at WS message receipt
//! - `ingest_seq`: Local monotonic sequence for strict total ordering
//! - `seq_hash`: Exchange-provided hash for integrity checking

use anyhow::{Context, Result};
use parking_lot::Mutex;
use rusqlite::{params, Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;
use tracing::{debug, info, warn, error};

use crate::backtest_v2::clock::Nanos;
use crate::backtest_v2::events::{Event, Side, Level};

// =============================================================================
// Nanosecond Timestamp Helper (reuse from book_recorder)
// =============================================================================

/// Get current time as nanoseconds since Unix epoch.
#[inline]
pub fn now_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_nanos() as u64
}

// =============================================================================
// L2 Book Delta Record
// =============================================================================

/// A single L2 book delta with arrival-time semantics.
///
/// Represents a change to aggregate size at a single price level.
/// The `new_size` field is the NEW TOTAL SIZE at this level, not a delta.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct L2BookDeltaRecord {
    /// Market ID (condition_id from `market` field).
    pub market_id: String,
    /// Token ID (clobTokenId / asset_id).
    pub token_id: String,
    /// Side affected (BUY = bid level, SELL = ask level).
    pub side: Side,
    /// Price level affected.
    pub price: f64,
    /// NEW aggregate size at this level (0 = level removed).
    pub new_size: f64,
    /// Exchange timestamp in milliseconds (from `timestamp` field).
    pub ws_timestamp_ms: u64,
    /// Our arrival timestamp (nanoseconds since Unix epoch).
    /// Captured at WebSocket message receipt, BEFORE JSON parsing.
    pub ingest_arrival_time_ns: u64,
    /// Local monotonic sequence for strict total ordering.
    pub ingest_seq: u64,
    /// Exchange-provided hash for integrity/sequencing.
    pub seq_hash: String,
    /// Best bid after this change (if provided).
    pub best_bid: Option<f64>,
    /// Best ask after this change (if provided).
    pub best_ask: Option<f64>,
}

impl L2BookDeltaRecord {
    /// Create from parsed `price_change` message data.
    pub fn from_price_change(
        market_id: String,
        token_id: String,
        side_str: &str,
        price: f64,
        new_size: f64,
        ws_timestamp_ms: u64,
        ingest_arrival_time_ns: u64,
        ingest_seq: u64,
        seq_hash: String,
        best_bid: Option<f64>,
        best_ask: Option<f64>,
    ) -> Self {
        let side = match side_str.to_uppercase().as_str() {
            "BUY" => Side::Buy,
            "SELL" => Side::Sell,
            _ => Side::Buy, // Default to buy if unknown
        };

        Self {
            market_id,
            token_id,
            side,
            price,
            new_size,
            ws_timestamp_ms,
            ingest_arrival_time_ns,
            ingest_seq,
            seq_hash,
            best_bid,
            best_ask,
        }
    }

    /// Convert to backtest Event::L2BookDelta.
    pub fn to_event(&self) -> Event {
        Event::L2BookDelta {
            token_id: self.token_id.clone(),
            side: self.side,
            price: self.price,
            new_size: self.new_size,
            seq_hash: Some(self.seq_hash.clone()),
        }
    }

    /// Get arrival time as Nanos for backtest clock.
    pub fn arrival_time_as_nanos(&self) -> Nanos {
        self.ingest_arrival_time_ns as Nanos
    }

    /// Get source time as Nanos for backtest clock.
    pub fn source_time_as_nanos(&self) -> Nanos {
        (self.ws_timestamp_ms * 1_000_000) as Nanos // ms to ns
    }

    /// Check if this delta removes a level (size becomes zero).
    pub fn is_level_removal(&self) -> bool {
        self.new_size <= 0.0
    }
}

// =============================================================================
// Storage Schema
// =============================================================================

const BOOK_DELTA_SCHEMA: &str = r#"
-- Enable optimizations
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;
PRAGMA cache_size = -32000;
PRAGMA temp_store = MEMORY;

-- Historical book deltas table
CREATE TABLE IF NOT EXISTS historical_book_deltas (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    market_id TEXT NOT NULL,
    token_id TEXT NOT NULL,
    side TEXT NOT NULL,
    price REAL NOT NULL,
    new_size REAL NOT NULL,
    ws_timestamp_ms INTEGER NOT NULL,
    ingest_arrival_time_ns INTEGER NOT NULL,
    ingest_seq INTEGER NOT NULL,
    seq_hash TEXT NOT NULL,
    best_bid REAL,
    best_ask REAL,
    recorded_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now'))
);

-- Primary index: token + arrival time (most common query for replay)
CREATE INDEX IF NOT EXISTS idx_book_deltas_token_arrival
    ON historical_book_deltas(token_id, ingest_arrival_time_ns);

-- Index for strict ordering within token
CREATE INDEX IF NOT EXISTS idx_book_deltas_token_seq
    ON historical_book_deltas(token_id, ingest_seq);

-- Index for integrity checking (dedupe by seq_hash)
CREATE INDEX IF NOT EXISTS idx_book_deltas_token_hash
    ON historical_book_deltas(token_id, seq_hash);

-- Index for market-based queries
CREATE INDEX IF NOT EXISTS idx_book_deltas_market_arrival
    ON historical_book_deltas(market_id, ingest_arrival_time_ns);

-- Index for time range queries across all tokens
CREATE INDEX IF NOT EXISTS idx_book_deltas_arrival
    ON historical_book_deltas(ingest_arrival_time_ns);

-- Metadata table for tracking recording state
CREATE TABLE IF NOT EXISTS book_delta_metadata (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
) WITHOUT ROWID;

-- Stats table for per-token recording statistics
CREATE TABLE IF NOT EXISTS book_delta_stats (
    token_id TEXT PRIMARY KEY,
    first_arrival_ns INTEGER NOT NULL,
    last_arrival_ns INTEGER NOT NULL,
    first_seq INTEGER NOT NULL,
    last_seq INTEGER NOT NULL,
    total_deltas INTEGER NOT NULL,
    duplicate_hashes INTEGER NOT NULL DEFAULT 0,
    out_of_order_arrivals INTEGER NOT NULL DEFAULT 0,
    level_removals INTEGER NOT NULL DEFAULT 0,
    updated_at INTEGER NOT NULL
) WITHOUT ROWID;

-- Integrity violations log
CREATE TABLE IF NOT EXISTS book_delta_integrity_log (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    token_id TEXT NOT NULL,
    violation_type TEXT NOT NULL,
    details TEXT,
    ingest_seq INTEGER,
    arrival_time_ns INTEGER,
    detected_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now'))
);
CREATE INDEX IF NOT EXISTS idx_integrity_log_token
    ON book_delta_integrity_log(token_id, detected_at);
"#;

// =============================================================================
// Book Delta Storage
// =============================================================================

/// Persistent storage for historical book deltas.
pub struct BookDeltaStorage {
    conn: Arc<Mutex<Connection>>,
    /// Per-token ingest sequence counters.
    token_seq: Mutex<HashMap<String, u64>>,
    /// Per-token last seen hash (for duplicate detection).
    last_hash: Mutex<HashMap<String, String>>,
    /// Per-token last arrival time (for out-of-order detection).
    last_arrival: Mutex<HashMap<String, u64>>,
    /// Stats counters.
    stats: DeltaRecorderStats,
}

/// Recording statistics.
#[derive(Debug, Default)]
pub struct DeltaRecorderStats {
    pub deltas_recorded: AtomicU64,
    pub deltas_skipped_duplicate: AtomicU64,
    pub deltas_out_of_order: AtomicU64,
    pub level_removals: AtomicU64,
    pub batch_writes: AtomicU64,
    pub integrity_violations: AtomicU64,
}

impl BookDeltaStorage {
    /// Open or create storage.
    pub fn open(db_path: &str) -> Result<Self> {
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
            .with_context(|| format!("Failed to open database: {}", db_path))?;

        // Initialize schema
        conn.execute_batch(BOOK_DELTA_SCHEMA)?;

        info!(path = %db_path, "Book delta storage opened");

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            token_seq: Mutex::new(HashMap::new()),
            last_hash: Mutex::new(HashMap::new()),
            last_arrival: Mutex::new(HashMap::new()),
            stats: DeltaRecorderStats::default(),
        })
    }

    /// Open in-memory storage (for testing).
    pub fn open_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(BOOK_DELTA_SCHEMA)?;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            token_seq: Mutex::new(HashMap::new()),
            last_hash: Mutex::new(HashMap::new()),
            last_arrival: Mutex::new(HashMap::new()),
            stats: DeltaRecorderStats::default(),
        })
    }

    /// Get next ingest sequence number for a token.
    pub fn next_ingest_seq(&self, token_id: &str) -> u64 {
        let mut seq_map = self.token_seq.lock();
        let seq = seq_map.entry(token_id.to_string()).or_insert(0);
        let current = *seq;
        *seq += 1;
        current
    }

    /// Store a single delta with integrity checking.
    pub fn store_delta(&self, delta: &L2BookDeltaRecord) -> Result<()> {
        // Duplicate detection (same seq_hash)
        {
            let mut last_hash = self.last_hash.lock();
            if let Some(prev_hash) = last_hash.get(&delta.token_id) {
                if prev_hash == &delta.seq_hash {
                    self.stats.deltas_skipped_duplicate.fetch_add(1, Ordering::Relaxed);
                    debug!(
                        token_id = %delta.token_id,
                        seq_hash = %delta.seq_hash,
                        "Skipping duplicate delta"
                    );
                    return Ok(());
                }
            }
            last_hash.insert(delta.token_id.clone(), delta.seq_hash.clone());
        }

        // Out-of-order detection
        {
            let mut last_arrival = self.last_arrival.lock();
            if let Some(&prev_arrival) = last_arrival.get(&delta.token_id) {
                if delta.ingest_arrival_time_ns < prev_arrival {
                    self.stats.deltas_out_of_order.fetch_add(1, Ordering::Relaxed);
                    self.stats.integrity_violations.fetch_add(1, Ordering::Relaxed);
                    warn!(
                        token_id = %delta.token_id,
                        current_arrival = delta.ingest_arrival_time_ns,
                        prev_arrival = prev_arrival,
                        "Out-of-order delta detected"
                    );
                    // Log the violation
                    self.log_integrity_violation(
                        &delta.token_id,
                        "OUT_OF_ORDER",
                        &format!("current={}, prev={}", delta.ingest_arrival_time_ns, prev_arrival),
                        delta.ingest_seq,
                        delta.ingest_arrival_time_ns,
                    )?;
                }
            }
            last_arrival.insert(delta.token_id.clone(), delta.ingest_arrival_time_ns);
        }

        // Track level removals
        if delta.is_level_removal() {
            self.stats.level_removals.fetch_add(1, Ordering::Relaxed);
        }

        let side_str = match delta.side {
            Side::Buy => "BUY",
            Side::Sell => "SELL",
        };

        let conn = self.conn.lock();
        conn.execute(
            r#"
            INSERT INTO historical_book_deltas (
                market_id, token_id, side, price, new_size,
                ws_timestamp_ms, ingest_arrival_time_ns, ingest_seq,
                seq_hash, best_bid, best_ask
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
            "#,
            params![
                delta.market_id,
                delta.token_id,
                side_str,
                delta.price,
                delta.new_size,
                delta.ws_timestamp_ms as i64,
                delta.ingest_arrival_time_ns as i64,
                delta.ingest_seq as i64,
                delta.seq_hash,
                delta.best_bid,
                delta.best_ask,
            ],
        )?;

        self.stats.deltas_recorded.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    /// Store multiple deltas in a batch.
    pub fn store_batch(&self, deltas: &[L2BookDeltaRecord]) -> Result<usize> {
        if deltas.is_empty() {
            return Ok(0);
        }

        let conn = self.conn.lock();
        conn.execute("BEGIN IMMEDIATE", [])?;

        let mut stored = 0;
        for delta in deltas {
            // Skip duplicates
            {
                let mut last_hash = self.last_hash.lock();
                if let Some(prev_hash) = last_hash.get(&delta.token_id) {
                    if prev_hash == &delta.seq_hash {
                        self.stats.deltas_skipped_duplicate.fetch_add(1, Ordering::Relaxed);
                        continue;
                    }
                }
                last_hash.insert(delta.token_id.clone(), delta.seq_hash.clone());
            }

            let side_str = match delta.side {
                Side::Buy => "BUY",
                Side::Sell => "SELL",
            };

            conn.execute(
                r#"
                INSERT INTO historical_book_deltas (
                    market_id, token_id, side, price, new_size,
                    ws_timestamp_ms, ingest_arrival_time_ns, ingest_seq,
                    seq_hash, best_bid, best_ask
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
                "#,
                params![
                    delta.market_id,
                    delta.token_id,
                    side_str,
                    delta.price,
                    delta.new_size,
                    delta.ws_timestamp_ms as i64,
                    delta.ingest_arrival_time_ns as i64,
                    delta.ingest_seq as i64,
                    delta.seq_hash,
                    delta.best_bid,
                    delta.best_ask,
                ],
            )?;
            stored += 1;
        }

        conn.execute("COMMIT", [])?;

        self.stats.deltas_recorded.fetch_add(stored as u64, Ordering::Relaxed);
        self.stats.batch_writes.fetch_add(1, Ordering::Relaxed);

        Ok(stored)
    }

    /// Log an integrity violation.
    fn log_integrity_violation(
        &self,
        token_id: &str,
        violation_type: &str,
        details: &str,
        ingest_seq: u64,
        arrival_time_ns: u64,
    ) -> Result<()> {
        let conn = self.conn.lock();
        conn.execute(
            r#"
            INSERT INTO book_delta_integrity_log (
                token_id, violation_type, details, ingest_seq, arrival_time_ns
            ) VALUES (?1, ?2, ?3, ?4, ?5)
            "#,
            params![
                token_id,
                violation_type,
                details,
                ingest_seq as i64,
                arrival_time_ns as i64,
            ],
        )?;
        Ok(())
    }

    /// Load deltas for a token in a time range, ordered by (arrival_time, ingest_seq).
    pub fn load_deltas(
        &self,
        token_id: &str,
        start_ns: u64,
        end_ns: u64,
    ) -> Result<Vec<L2BookDeltaRecord>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            r#"
            SELECT market_id, token_id, side, price, new_size,
                   ws_timestamp_ms, ingest_arrival_time_ns, ingest_seq,
                   seq_hash, best_bid, best_ask
            FROM historical_book_deltas
            WHERE token_id = ?1
              AND ingest_arrival_time_ns >= ?2
              AND ingest_arrival_time_ns < ?3
            ORDER BY ingest_arrival_time_ns ASC, ingest_seq ASC
            "#,
        )?;

        let rows = stmt.query_map(
            params![token_id, start_ns as i64, end_ns as i64],
            |row| {
                let side_str: String = row.get(2)?;
                let side = match side_str.as_str() {
                    "BUY" => Side::Buy,
                    "SELL" => Side::Sell,
                    _ => Side::Buy,
                };

                Ok(L2BookDeltaRecord {
                    market_id: row.get(0)?,
                    token_id: row.get(1)?,
                    side,
                    price: row.get(3)?,
                    new_size: row.get(4)?,
                    ws_timestamp_ms: row.get::<_, i64>(5)? as u64,
                    ingest_arrival_time_ns: row.get::<_, i64>(6)? as u64,
                    ingest_seq: row.get::<_, i64>(7)? as u64,
                    seq_hash: row.get(8)?,
                    best_bid: row.get(9)?,
                    best_ask: row.get(10)?,
                })
            },
        )?;

        let mut deltas = Vec::new();
        for row in rows {
            deltas.push(row?);
        }

        Ok(deltas)
    }

    /// Load all deltas for a token, ordered by (arrival_time, ingest_seq).
    pub fn load_all_deltas(&self, token_id: &str) -> Result<Vec<L2BookDeltaRecord>> {
        // Use i64::MAX instead of u64::MAX to avoid sign overflow
        self.load_deltas(token_id, 0, i64::MAX as u64)
    }

    /// Get delta count for a token.
    pub fn delta_count(&self, token_id: &str) -> Result<u64> {
        let conn = self.conn.lock();
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM historical_book_deltas WHERE token_id = ?1",
            params![token_id],
            |row| row.get(0),
        )?;
        Ok(count as u64)
    }

    /// Get total delta count across all tokens.
    pub fn total_delta_count(&self) -> Result<u64> {
        let conn = self.conn.lock();
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM historical_book_deltas",
            [],
            |row| row.get(0),
        )?;
        Ok(count as u64)
    }

    /// Get list of unique tokens with deltas.
    pub fn list_tokens(&self) -> Result<Vec<String>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT DISTINCT token_id FROM historical_book_deltas ORDER BY token_id"
        )?;
        let rows = stmt.query_map([], |row| row.get(0))?;
        
        let mut tokens = Vec::new();
        for row in rows {
            tokens.push(row?);
        }
        Ok(tokens)
    }

    /// Get time range covered by deltas for a token.
    pub fn time_range(&self, token_id: &str) -> Result<Option<(u64, u64)>> {
        let conn = self.conn.lock();
        let result: Result<(i64, i64), _> = conn.query_row(
            r#"
            SELECT MIN(ingest_arrival_time_ns), MAX(ingest_arrival_time_ns)
            FROM historical_book_deltas
            WHERE token_id = ?1
            "#,
            params![token_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        );

        match result {
            Ok((min, max)) => Ok(Some((min as u64, max as u64))),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Run integrity scan on stored deltas.
    pub fn integrity_scan(&self, token_id: &str) -> Result<IntegrityScanResult> {
        let conn = self.conn.lock();
        
        // Check for duplicates
        let duplicates: i64 = conn.query_row(
            r#"
            SELECT COUNT(*) FROM (
                SELECT seq_hash, COUNT(*) as cnt
                FROM historical_book_deltas
                WHERE token_id = ?1
                GROUP BY seq_hash
                HAVING cnt > 1
            )
            "#,
            params![token_id],
            |row| row.get(0),
        )?;

        // Check for out-of-order (arrival_time decreases)
        let out_of_order: i64 = conn.query_row(
            r#"
            SELECT COUNT(*) FROM (
                SELECT a.id
                FROM historical_book_deltas a
                JOIN historical_book_deltas b ON a.token_id = b.token_id
                    AND a.ingest_seq > b.ingest_seq
                    AND a.ingest_arrival_time_ns < b.ingest_arrival_time_ns
                WHERE a.token_id = ?1
            )
            "#,
            params![token_id],
            |row| row.get(0),
        )?;

        // Get total count
        let total: i64 = conn.query_row(
            "SELECT COUNT(*) FROM historical_book_deltas WHERE token_id = ?1",
            params![token_id],
            |row| row.get(0),
        )?;

        // Get integrity violations from log
        let logged_violations: i64 = conn.query_row(
            "SELECT COUNT(*) FROM book_delta_integrity_log WHERE token_id = ?1",
            params![token_id],
            |row| row.get(0),
        )?;

        Ok(IntegrityScanResult {
            token_id: token_id.to_string(),
            total_deltas: total as u64,
            duplicate_hashes: duplicates as u64,
            out_of_order_arrivals: out_of_order as u64,
            logged_violations: logged_violations as u64,
            is_clean: duplicates == 0 && out_of_order == 0,
        })
    }

    /// Get recording statistics.
    pub fn stats(&self) -> &DeltaRecorderStats {
        &self.stats
    }
}

/// Result of integrity scan.
#[derive(Debug, Clone)]
pub struct IntegrityScanResult {
    pub token_id: String,
    pub total_deltas: u64,
    pub duplicate_hashes: u64,
    pub out_of_order_arrivals: u64,
    pub logged_violations: u64,
    pub is_clean: bool,
}

impl std::fmt::Display for IntegrityScanResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "IntegrityScan(token={}, total={}, duplicates={}, out_of_order={}, violations={}, clean={})",
            self.token_id,
            self.total_deltas,
            self.duplicate_hashes,
            self.out_of_order_arrivals,
            self.logged_violations,
            self.is_clean
        )
    }
}

// =============================================================================
// Async Delta Recorder
// =============================================================================

/// Non-blocking delta recorder for high-frequency ingest.
pub struct AsyncDeltaRecorder {
    tx: mpsc::UnboundedSender<L2BookDeltaRecord>,
    /// Handle to the background task.
    _handle: tokio::task::JoinHandle<()>,
}

impl AsyncDeltaRecorder {
    /// Spawn a new async recorder.
    pub fn spawn(storage: Arc<BookDeltaStorage>, batch_size: usize) -> Self {
        let (tx, mut rx) = mpsc::unbounded_channel::<L2BookDeltaRecord>();

        let handle = tokio::spawn(async move {
            let mut buffer = Vec::with_capacity(batch_size);
            let flush_interval = tokio::time::interval(Duration::from_millis(100));
            tokio::pin!(flush_interval);

            loop {
                tokio::select! {
                    Some(delta) = rx.recv() => {
                        buffer.push(delta);
                        if buffer.len() >= batch_size {
                            if let Err(e) = storage.store_batch(&buffer) {
                                error!(error = %e, "Failed to store delta batch");
                            }
                            buffer.clear();
                        }
                    }
                    _ = flush_interval.tick() => {
                        if !buffer.is_empty() {
                            if let Err(e) = storage.store_batch(&buffer) {
                                error!(error = %e, "Failed to flush delta buffer");
                            }
                            buffer.clear();
                        }
                    }
                    else => break,
                }
            }

            // Final flush
            if !buffer.is_empty() {
                if let Err(e) = storage.store_batch(&buffer) {
                    error!(error = %e, "Failed to store final delta batch");
                }
            }
        });

        Self { tx, _handle: handle }
    }

    /// Record a delta (non-blocking).
    pub fn record(&self, delta: L2BookDeltaRecord) {
        let _ = self.tx.send(delta);
    }
}

// =============================================================================
// Delta Replay Feed
// =============================================================================

use crate::backtest_v2::events::TimestampedEvent;
use crate::backtest_v2::queue::StreamSource;

/// Feed for replaying stored deltas as backtest events.
pub struct DeltaReplayFeed {
    deltas: Vec<L2BookDeltaRecord>,
    index: usize,
}

impl DeltaReplayFeed {
    /// Create from storage for a specific token.
    pub fn from_storage(
        storage: &BookDeltaStorage,
        token_id: &str,
        start_ns: u64,
        end_ns: u64,
    ) -> Result<Self> {
        let deltas = storage.load_deltas(token_id, start_ns, end_ns)?;
        Ok(Self { deltas, index: 0 })
    }

    /// Create from a list of deltas (for testing).
    pub fn from_deltas(deltas: Vec<L2BookDeltaRecord>) -> Self {
        Self { deltas, index: 0 }
    }

    /// Get total number of deltas.
    pub fn len(&self) -> usize {
        self.deltas.len()
    }

    /// Check if empty.
    pub fn is_empty(&self) -> bool {
        self.deltas.is_empty()
    }

    /// Reset to beginning.
    pub fn reset(&mut self) {
        self.index = 0;
    }

    /// Get next delta as a TimestampedEvent.
    pub fn next_event(&mut self) -> Option<TimestampedEvent> {
        if self.index >= self.deltas.len() {
            return None;
        }

        let delta = &self.deltas[self.index];
        self.index += 1;

        let event = delta.to_event();
        let arrival_time = delta.ingest_arrival_time_ns as Nanos;
        let source_time = delta.source_time_as_nanos();

        Some(TimestampedEvent {
            time: arrival_time,
            source_time,
            seq: delta.ingest_seq,
            source: StreamSource::MarketData as u8,
            event,
        })
    }

    /// Peek at next delta without consuming.
    pub fn peek(&self) -> Option<&L2BookDeltaRecord> {
        self.deltas.get(self.index)
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_delta_creation() {
        let delta = L2BookDeltaRecord::from_price_change(
            "market_123".to_string(),
            "token_456".to_string(),
            "BUY",
            0.55,
            100.0,
            1700000000000,
            1700000000000000000,
            1,
            "hash_abc".to_string(),
            Some(0.55),
            Some(0.56),
        );

        assert_eq!(delta.market_id, "market_123");
        assert_eq!(delta.token_id, "token_456");
        assert_eq!(delta.side, Side::Buy);
        assert_eq!(delta.price, 0.55);
        assert_eq!(delta.new_size, 100.0);
        assert!(!delta.is_level_removal());
    }

    #[test]
    fn test_delta_level_removal() {
        let delta = L2BookDeltaRecord::from_price_change(
            "market".to_string(),
            "token".to_string(),
            "SELL",
            0.45,
            0.0, // Zero size = removal
            1700000000000,
            1700000000000000000,
            1,
            "hash".to_string(),
            None,
            None,
        );

        assert!(delta.is_level_removal());
        assert_eq!(delta.side, Side::Sell);
    }

    #[test]
    fn test_storage_roundtrip() {
        let storage = BookDeltaStorage::open_memory().unwrap();

        let delta = L2BookDeltaRecord::from_price_change(
            "market_123".to_string(),
            "token_456".to_string(),
            "BUY",
            0.55,
            100.0,
            1700000000000,
            1700000000000000000,
            1,
            "hash_abc".to_string(),
            Some(0.55),
            Some(0.56),
        );

        storage.store_delta(&delta).unwrap();

        let loaded = storage.load_all_deltas("token_456").unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].market_id, "market_123");
        assert_eq!(loaded[0].price, 0.55);
        assert_eq!(loaded[0].new_size, 100.0);
    }

    #[test]
    fn test_storage_duplicate_detection() {
        let storage = BookDeltaStorage::open_memory().unwrap();

        let delta1 = L2BookDeltaRecord::from_price_change(
            "market".to_string(),
            "token".to_string(),
            "BUY",
            0.55,
            100.0,
            1700000000000,
            1700000000000000000,
            1,
            "same_hash".to_string(),
            None,
            None,
        );

        let delta2 = L2BookDeltaRecord::from_price_change(
            "market".to_string(),
            "token".to_string(),
            "BUY",
            0.55,
            200.0, // Different size
            1700000000001,
            1700000000001000000,
            2,
            "same_hash".to_string(), // Same hash = duplicate
            None,
            None,
        );

        storage.store_delta(&delta1).unwrap();
        storage.store_delta(&delta2).unwrap();

        // Only one should be stored
        let loaded = storage.load_all_deltas("token").unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(storage.stats().deltas_skipped_duplicate.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn test_storage_ordering() {
        let storage = BookDeltaStorage::open_memory().unwrap();

        // Insert in reverse order
        for i in (0..5).rev() {
            let delta = L2BookDeltaRecord::from_price_change(
                "market".to_string(),
                "token".to_string(),
                "BUY",
                0.50 + i as f64 * 0.01,
                100.0,
                1700000000000 + i as u64,
                1700000000000000000 + i as u64 * 1000000,
                i as u64,
                format!("hash_{}", i),
                None,
                None,
            );
            storage.store_delta(&delta).unwrap();
        }

        // Load should return in arrival time order
        let loaded = storage.load_all_deltas("token").unwrap();
        assert_eq!(loaded.len(), 5);
        for (i, delta) in loaded.iter().enumerate() {
            assert_eq!(delta.ingest_seq, i as u64);
        }
    }

    #[test]
    fn test_integrity_scan() {
        let storage = BookDeltaStorage::open_memory().unwrap();

        // Add clean deltas
        for i in 0..10 {
            let delta = L2BookDeltaRecord::from_price_change(
                "market".to_string(),
                "token".to_string(),
                "BUY",
                0.50,
                100.0 + i as f64,
                1700000000000 + i as u64,
                1700000000000000000 + i as u64 * 1000000,
                i as u64,
                format!("hash_{}", i),
                None,
                None,
            );
            storage.store_delta(&delta).unwrap();
        }

        let result = storage.integrity_scan("token").unwrap();
        assert_eq!(result.total_deltas, 10);
        assert_eq!(result.duplicate_hashes, 0);
        assert_eq!(result.out_of_order_arrivals, 0);
        assert!(result.is_clean);
    }

    #[test]
    fn test_to_event() {
        let delta = L2BookDeltaRecord::from_price_change(
            "market".to_string(),
            "token".to_string(),
            "BUY",
            0.55,
            100.0,
            1700000000000,
            1700000000000000000,
            1,
            "hash".to_string(),
            None,
            None,
        );

        let event = delta.to_event();
        match event {
            Event::L2BookDelta { token_id, side, price, new_size, seq_hash } => {
                assert_eq!(token_id, "token");
                assert_eq!(side, Side::Buy);
                assert_eq!(price, 0.55);
                assert_eq!(new_size, 100.0);
                assert_eq!(seq_hash, Some("hash".to_string()));
            }
            _ => panic!("Expected L2BookDelta event"),
        }
    }

    #[test]
    fn test_replay_feed() {
        let storage = BookDeltaStorage::open_memory().unwrap();

        // Add deltas
        for i in 0..5 {
            let delta = L2BookDeltaRecord::from_price_change(
                "market".to_string(),
                "token".to_string(),
                "BUY",
                0.50 + i as f64 * 0.01,
                100.0,
                1700000000000 + i as u64,
                1700000000000000000 + i as u64 * 1000000,
                i as u64,
                format!("hash_{}", i),
                None,
                None,
            );
            storage.store_delta(&delta).unwrap();
        }

        let mut feed = DeltaReplayFeed::from_storage(
            &storage, "token", 0, i64::MAX as u64
        ).unwrap();

        assert_eq!(feed.len(), 5);

        let mut count = 0;
        while let Some(_event) = feed.next_event() {
            count += 1;
        }
        assert_eq!(count, 5);
    }
}

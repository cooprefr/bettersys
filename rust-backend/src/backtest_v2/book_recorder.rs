//! Historical Book Snapshot Recorder
//!
//! Persists L2 book snapshots received live with high-resolution arrival timestamps
//! for offline backtesting replay. This enables:
//!
//! - RecordedArrival policy: Use actual arrival times instead of simulated latency
//! - Sequence gap detection: Identify missing data
//! - Deterministic replay: Same data → same results
//!
//! **CRITICAL**: Arrival time is captured at the EARLIEST possible point in the
//! message handling path (before JSON parsing) to minimize measurement noise.

use anyhow::{Context, Result};
use parking_lot::Mutex;
use rusqlite::{params, Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

// =============================================================================
// Nanosecond Timestamp Helper
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
// Recorded Book Snapshot
// =============================================================================

/// A single L2 book snapshot with arrival-time semantics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordedBookSnapshot {
    /// Token ID (clobTokenId / asset_id).
    pub token_id: String,
    /// Exchange sequence number (from `hash` field).
    pub exchange_seq: Option<u64>,
    /// Exchange source timestamp (parsed from ISO string, as Unix nanoseconds).
    pub source_time_ns: Option<u64>,
    /// Our arrival timestamp (nanoseconds since Unix epoch).
    /// Captured at WebSocket message receipt, BEFORE JSON parsing.
    pub arrival_time_ns: u64,
    /// Local monotonic sequence for ordering within same arrival_time.
    pub local_seq: u64,
    /// Bid levels (price, size) sorted descending by price.
    pub bids: Vec<PriceLevel>,
    /// Ask levels (price, size) sorted ascending by price.
    pub asks: Vec<PriceLevel>,
    /// Best bid price (for quick access).
    pub best_bid: Option<f64>,
    /// Best ask price (for quick access).
    pub best_ask: Option<f64>,
    /// Mid price (best_bid + best_ask) / 2.
    pub mid_price: Option<f64>,
    /// Spread in ticks.
    pub spread: Option<f64>,
}

/// Price level with price and size.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct PriceLevel {
    pub price: f64,
    pub size: f64,
}

impl RecordedBookSnapshot {
    /// Create from raw WebSocket data.
    pub fn from_ws_message(
        token_id: String,
        bids: Vec<PriceLevel>,
        asks: Vec<PriceLevel>,
        exchange_seq: Option<u64>,
        source_time_ns: Option<u64>,
        arrival_time_ns: u64,
        local_seq: u64,
    ) -> Self {
        let best_bid = bids.first().map(|l| l.price);
        let best_ask = asks.first().map(|l| l.price);
        let mid_price = match (best_bid, best_ask) {
            (Some(b), Some(a)) => Some((b + a) / 2.0),
            _ => None,
        };
        let spread = match (best_bid, best_ask) {
            (Some(b), Some(a)) => Some(a - b),
            _ => None,
        };

        Self {
            token_id,
            exchange_seq,
            source_time_ns,
            arrival_time_ns,
            local_seq,
            bids,
            asks,
            best_bid,
            best_ask,
            mid_price,
            spread,
        }
    }

    /// Convert arrival_time_ns to backtest clock time (Nanos).
    pub fn arrival_time_as_nanos(&self) -> i64 {
        self.arrival_time_ns as i64
    }

    /// Check if this is a valid book (has at least one level on each side).
    pub fn is_valid(&self) -> bool {
        !self.bids.is_empty() && !self.asks.is_empty()
    }

    /// Check if book is crossed (best bid >= best ask).
    pub fn is_crossed(&self) -> bool {
        match (self.best_bid, self.best_ask) {
            (Some(b), Some(a)) => b >= a,
            _ => false,
        }
    }
}

// =============================================================================
// Storage Schema
// =============================================================================

const BOOK_SNAPSHOT_SCHEMA: &str = r#"
-- Enable optimizations
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;
PRAGMA cache_size = -32000;
PRAGMA temp_store = MEMORY;

-- Historical book snapshots table
CREATE TABLE IF NOT EXISTS historical_book_snapshots (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    token_id TEXT NOT NULL,
    exchange_seq INTEGER,
    source_time_ns INTEGER,
    arrival_time_ns INTEGER NOT NULL,
    local_seq INTEGER NOT NULL,
    best_bid REAL,
    best_ask REAL,
    mid_price REAL,
    spread REAL,
    bid_levels INTEGER NOT NULL,
    ask_levels INTEGER NOT NULL,
    bids_json TEXT NOT NULL,
    asks_json TEXT NOT NULL,
    recorded_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now'))
);

-- Primary index: token + arrival time (most common query for replay)
CREATE INDEX IF NOT EXISTS idx_book_snapshots_token_arrival
    ON historical_book_snapshots(token_id, arrival_time_ns);

-- Index for exchange sequence (gap detection)
CREATE INDEX IF NOT EXISTS idx_book_snapshots_token_seq
    ON historical_book_snapshots(token_id, exchange_seq);

-- Index for time range queries
CREATE INDEX IF NOT EXISTS idx_book_snapshots_arrival
    ON historical_book_snapshots(arrival_time_ns);

-- Metadata table for tracking recording state
CREATE TABLE IF NOT EXISTS book_snapshot_metadata (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
) WITHOUT ROWID;

-- Stats table for per-token recording statistics
CREATE TABLE IF NOT EXISTS book_snapshot_stats (
    token_id TEXT PRIMARY KEY,
    first_arrival_ns INTEGER NOT NULL,
    last_arrival_ns INTEGER NOT NULL,
    first_seq INTEGER,
    last_seq INTEGER,
    total_snapshots INTEGER NOT NULL,
    sequence_gaps INTEGER NOT NULL DEFAULT 0,
    crossed_books INTEGER NOT NULL DEFAULT 0,
    updated_at INTEGER NOT NULL
) WITHOUT ROWID;
"#;

// =============================================================================
// Book Snapshot Storage
// =============================================================================

/// Persistent storage for historical book snapshots.
pub struct BookSnapshotStorage {
    conn: Arc<Mutex<Connection>>,
    /// Local sequence counter for ordering.
    local_seq: AtomicU64,
    /// Stats counters.
    stats: BookRecorderStats,
}

/// Recording statistics.
#[derive(Debug, Default)]
pub struct BookRecorderStats {
    pub snapshots_recorded: AtomicU64,
    pub snapshots_skipped_invalid: AtomicU64,
    pub snapshots_skipped_crossed: AtomicU64,
    pub sequence_gaps_detected: AtomicU64,
    pub batch_writes: AtomicU64,
}

impl BookSnapshotStorage {
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
        conn.execute_batch(BOOK_SNAPSHOT_SCHEMA)?;

        info!(path = %db_path, "Book snapshot storage opened");

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            local_seq: AtomicU64::new(0),
            stats: BookRecorderStats::default(),
        })
    }

    /// Open in-memory storage (for testing).
    pub fn open_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(BOOK_SNAPSHOT_SCHEMA)?;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            local_seq: AtomicU64::new(0),
            stats: BookRecorderStats::default(),
        })
    }

    /// Get next local sequence number.
    pub fn next_local_seq(&self) -> u64 {
        self.local_seq.fetch_add(1, Ordering::Relaxed)
    }

    /// Store a single snapshot.
    pub fn store_snapshot(&self, snapshot: &RecordedBookSnapshot) -> Result<()> {
        // Skip invalid books
        if !snapshot.is_valid() {
            self.stats.snapshots_skipped_invalid.fetch_add(1, Ordering::Relaxed);
            return Ok(());
        }

        // Skip crossed books (but still log)
        if snapshot.is_crossed() {
            self.stats.snapshots_skipped_crossed.fetch_add(1, Ordering::Relaxed);
            debug!(
                token_id = %snapshot.token_id,
                best_bid = ?snapshot.best_bid,
                best_ask = ?snapshot.best_ask,
                "Skipping crossed book"
            );
            return Ok(());
        }

        let bids_json = serde_json::to_string(&snapshot.bids)?;
        let asks_json = serde_json::to_string(&snapshot.asks)?;

        let conn = self.conn.lock();
        conn.execute(
            r#"
            INSERT INTO historical_book_snapshots (
                token_id, exchange_seq, source_time_ns, arrival_time_ns, local_seq,
                best_bid, best_ask, mid_price, spread, bid_levels, ask_levels,
                bids_json, asks_json
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
            "#,
            params![
                snapshot.token_id,
                snapshot.exchange_seq.map(|s| s as i64),
                snapshot.source_time_ns.map(|s| s as i64),
                snapshot.arrival_time_ns as i64,
                snapshot.local_seq as i64,
                snapshot.best_bid,
                snapshot.best_ask,
                snapshot.mid_price,
                snapshot.spread,
                snapshot.bids.len() as i64,
                snapshot.asks.len() as i64,
                bids_json,
                asks_json,
            ],
        )?;

        self.stats.snapshots_recorded.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    /// Store multiple snapshots in a batch.
    pub fn store_batch(&self, snapshots: &[RecordedBookSnapshot]) -> Result<usize> {
        if snapshots.is_empty() {
            return Ok(0);
        }

        let conn = self.conn.lock();
        conn.execute("BEGIN IMMEDIATE", [])?;

        let mut count = 0;
        for snapshot in snapshots {
            if !snapshot.is_valid() || snapshot.is_crossed() {
                continue;
            }

            let bids_json = serde_json::to_string(&snapshot.bids)?;
            let asks_json = serde_json::to_string(&snapshot.asks)?;

            let result = conn.execute(
                r#"
                INSERT INTO historical_book_snapshots (
                    token_id, exchange_seq, source_time_ns, arrival_time_ns, local_seq,
                    best_bid, best_ask, mid_price, spread, bid_levels, ask_levels,
                    bids_json, asks_json
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
                "#,
                params![
                    snapshot.token_id,
                    snapshot.exchange_seq.map(|s| s as i64),
                    snapshot.source_time_ns.map(|s| s as i64),
                    snapshot.arrival_time_ns as i64,
                    snapshot.local_seq as i64,
                    snapshot.best_bid,
                    snapshot.best_ask,
                    snapshot.mid_price,
                    snapshot.spread,
                    snapshot.bids.len() as i64,
                    snapshot.asks.len() as i64,
                    bids_json,
                    asks_json,
                ],
            );

            if result.is_ok() {
                count += 1;
            }
        }

        conn.execute("COMMIT", [])?;
        self.stats.batch_writes.fetch_add(1, Ordering::Relaxed);
        self.stats.snapshots_recorded.fetch_add(count, Ordering::Relaxed);

        Ok(count as usize)
    }

    /// Load snapshots for a token in a time range (by arrival_time).
    pub fn load_snapshots_in_range(
        &self,
        token_id: &str,
        start_arrival_ns: u64,
        end_arrival_ns: u64,
    ) -> Result<Vec<RecordedBookSnapshot>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            r#"
            SELECT token_id, exchange_seq, source_time_ns, arrival_time_ns, local_seq,
                   best_bid, best_ask, mid_price, spread, bids_json, asks_json
            FROM historical_book_snapshots
            WHERE token_id = ?1 AND arrival_time_ns >= ?2 AND arrival_time_ns <= ?3
            ORDER BY arrival_time_ns ASC, local_seq ASC
            "#,
        )?;

        let snapshots = stmt
            .query_map(
                params![token_id, start_arrival_ns as i64, end_arrival_ns as i64],
                |row| {
                    let bids_json: String = row.get(9)?;
                    let asks_json: String = row.get(10)?;
                    let bids: Vec<PriceLevel> =
                        serde_json::from_str(&bids_json).unwrap_or_default();
                    let asks: Vec<PriceLevel> =
                        serde_json::from_str(&asks_json).unwrap_or_default();

                    Ok(RecordedBookSnapshot {
                        token_id: row.get(0)?,
                        exchange_seq: row.get::<_, Option<i64>>(1)?.map(|s| s as u64),
                        source_time_ns: row.get::<_, Option<i64>>(2)?.map(|s| s as u64),
                        arrival_time_ns: row.get::<_, i64>(3)? as u64,
                        local_seq: row.get::<_, i64>(4)? as u64,
                        best_bid: row.get(5)?,
                        best_ask: row.get(6)?,
                        mid_price: row.get(7)?,
                        spread: row.get(8)?,
                        bids,
                        asks,
                    })
                },
            )?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(snapshots)
    }

    /// Load all snapshots for multiple tokens in a time range.
    pub fn load_all_snapshots_in_range(
        &self,
        start_arrival_ns: u64,
        end_arrival_ns: u64,
    ) -> Result<Vec<RecordedBookSnapshot>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            r#"
            SELECT token_id, exchange_seq, source_time_ns, arrival_time_ns, local_seq,
                   best_bid, best_ask, mid_price, spread, bids_json, asks_json
            FROM historical_book_snapshots
            WHERE arrival_time_ns >= ?1 AND arrival_time_ns <= ?2
            ORDER BY arrival_time_ns ASC, local_seq ASC
            "#,
        )?;

        let snapshots = stmt
            .query_map(params![start_arrival_ns as i64, end_arrival_ns as i64], |row| {
                let bids_json: String = row.get(9)?;
                let asks_json: String = row.get(10)?;
                let bids: Vec<PriceLevel> = serde_json::from_str(&bids_json).unwrap_or_default();
                let asks: Vec<PriceLevel> = serde_json::from_str(&asks_json).unwrap_or_default();

                Ok(RecordedBookSnapshot {
                    token_id: row.get(0)?,
                    exchange_seq: row.get::<_, Option<i64>>(1)?.map(|s| s as u64),
                    source_time_ns: row.get::<_, Option<i64>>(2)?.map(|s| s as u64),
                    arrival_time_ns: row.get::<_, i64>(3)? as u64,
                    local_seq: row.get::<_, i64>(4)? as u64,
                    best_bid: row.get(5)?,
                    best_ask: row.get(6)?,
                    mid_price: row.get(7)?,
                    spread: row.get(8)?,
                    bids,
                    asks,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(snapshots)
    }

    /// Get time coverage for a token.
    pub fn get_time_coverage(&self, token_id: &str) -> Result<Option<(u64, u64)>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            r#"
            SELECT MIN(arrival_time_ns), MAX(arrival_time_ns)
            FROM historical_book_snapshots
            WHERE token_id = ?1
            "#,
        )?;

        let result = stmt
            .query_row(params![token_id], |row| {
                let min: Option<i64> = row.get(0)?;
                let max: Option<i64> = row.get(1)?;
                Ok((min, max))
            })
            .ok();

        match result {
            Some((Some(min), Some(max))) => Ok(Some((min as u64, max as u64))),
            _ => Ok(None),
        }
    }

    /// Count snapshots for a token.
    pub fn count_snapshots(&self, token_id: &str) -> Result<usize> {
        let conn = self.conn.lock();
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM historical_book_snapshots WHERE token_id = ?1",
            params![token_id],
            |row| row.get(0),
        )?;
        Ok(count as usize)
    }

    /// Get all unique token IDs in storage.
    pub fn get_token_ids(&self) -> Result<Vec<String>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT DISTINCT token_id FROM historical_book_snapshots ORDER BY token_id",
        )?;
        let ids = stmt
            .query_map([], |row| row.get(0))?
            .collect::<Result<Vec<String>, _>>()?;
        Ok(ids)
    }

    /// Detect sequence gaps for a token.
    pub fn detect_sequence_gaps(&self, token_id: &str) -> Result<Vec<(u64, u64)>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            r#"
            SELECT exchange_seq
            FROM historical_book_snapshots
            WHERE token_id = ?1 AND exchange_seq IS NOT NULL
            ORDER BY exchange_seq ASC
            "#,
        )?;

        let seqs: Vec<u64> = stmt
            .query_map(params![token_id], |row| {
                Ok(row.get::<_, i64>(0)? as u64)
            })?
            .collect::<Result<Vec<_>, _>>()?;

        let mut gaps = Vec::new();
        for window in seqs.windows(2) {
            let expected = window[0] + 1;
            let actual = window[1];
            if actual > expected {
                gaps.push((expected, actual - 1));
            }
        }

        Ok(gaps)
    }

    /// Get recording statistics.
    pub fn stats(&self) -> &BookRecorderStats {
        &self.stats
    }

    /// Update per-token statistics.
    pub fn update_token_stats(&self, token_id: &str) -> Result<()> {
        let conn = self.conn.lock();

        // Get aggregated stats
        let (first_arrival, last_arrival, first_seq, last_seq, total) = conn.query_row(
            r#"
            SELECT MIN(arrival_time_ns), MAX(arrival_time_ns),
                   MIN(exchange_seq), MAX(exchange_seq),
                   COUNT(*)
            FROM historical_book_snapshots
            WHERE token_id = ?1
            "#,
            params![token_id],
            |row| {
                Ok((
                    row.get::<_, Option<i64>>(0)?,
                    row.get::<_, Option<i64>>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                    row.get::<_, i64>(4)?,
                ))
            },
        )?;

        if first_arrival.is_none() {
            return Ok(()); // No data
        }

        // Count gaps
        let gaps = self.detect_sequence_gaps(token_id)?;

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        conn.execute(
            r#"
            INSERT INTO book_snapshot_stats (
                token_id, first_arrival_ns, last_arrival_ns, first_seq, last_seq,
                total_snapshots, sequence_gaps, updated_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
            ON CONFLICT(token_id) DO UPDATE SET
                first_arrival_ns = excluded.first_arrival_ns,
                last_arrival_ns = excluded.last_arrival_ns,
                first_seq = excluded.first_seq,
                last_seq = excluded.last_seq,
                total_snapshots = excluded.total_snapshots,
                sequence_gaps = excluded.sequence_gaps,
                updated_at = excluded.updated_at
            "#,
            params![
                token_id,
                first_arrival.unwrap(),
                last_arrival.unwrap(),
                first_seq,
                last_seq,
                total,
                gaps.len() as i64,
                now as i64,
            ],
        )?;

        Ok(())
    }
}

// =============================================================================
// Async Recorder (Buffered Background Writer)
// =============================================================================

/// Message for the recorder channel.
pub enum RecorderMessage {
    Snapshot(RecordedBookSnapshot),
    Flush,
    Shutdown,
}

/// Async book snapshot recorder with buffered writes.
pub struct AsyncBookRecorder {
    tx: mpsc::Sender<RecorderMessage>,
}

impl AsyncBookRecorder {
    /// Spawn the async recorder.
    pub fn spawn(storage: Arc<BookSnapshotStorage>, buffer_size: usize) -> Self {
        let (tx, rx) = mpsc::channel(buffer_size);

        tokio::spawn(async move {
            Self::run_writer(storage, rx, buffer_size).await;
        });

        Self { tx }
    }

    /// Record a snapshot (non-blocking).
    pub fn record(&self, snapshot: RecordedBookSnapshot) {
        let _ = self.tx.try_send(RecorderMessage::Snapshot(snapshot));
    }

    /// Flush pending writes.
    pub async fn flush(&self) {
        let _ = self.tx.send(RecorderMessage::Flush).await;
    }

    /// Shutdown the recorder.
    pub async fn shutdown(&self) {
        let _ = self.tx.send(RecorderMessage::Shutdown).await;
    }

    async fn run_writer(
        storage: Arc<BookSnapshotStorage>,
        mut rx: mpsc::Receiver<RecorderMessage>,
        buffer_size: usize,
    ) {
        let mut buffer: Vec<RecordedBookSnapshot> = Vec::with_capacity(buffer_size);
        let flush_interval = Duration::from_millis(100);
        let mut last_flush = std::time::Instant::now();

        loop {
            tokio::select! {
                msg = rx.recv() => {
                    match msg {
                        Some(RecorderMessage::Snapshot(snapshot)) => {
                            buffer.push(snapshot);

                            // Flush if buffer is full or interval elapsed
                            if buffer.len() >= buffer_size || last_flush.elapsed() > flush_interval {
                                if let Err(e) = storage.store_batch(&buffer) {
                                    warn!(error = %e, "Failed to store book snapshots");
                                }
                                buffer.clear();
                                last_flush = std::time::Instant::now();
                            }
                        }
                        Some(RecorderMessage::Flush) => {
                            if !buffer.is_empty() {
                                if let Err(e) = storage.store_batch(&buffer) {
                                    warn!(error = %e, "Failed to store book snapshots on flush");
                                }
                                buffer.clear();
                            }
                            last_flush = std::time::Instant::now();
                        }
                        Some(RecorderMessage::Shutdown) | None => {
                            // Final flush before shutdown
                            if !buffer.is_empty() {
                                let _ = storage.store_batch(&buffer);
                            }
                            info!("Book snapshot recorder shutting down");
                            return;
                        }
                    }
                }
                _ = tokio::time::sleep(flush_interval) => {
                    // Periodic flush
                    if !buffer.is_empty() {
                        if let Err(e) = storage.store_batch(&buffer) {
                            warn!(error = %e, "Failed to store book snapshots on timer");
                        }
                        buffer.clear();
                        last_flush = std::time::Instant::now();
                    }
                }
            }
        }
    }
}

// =============================================================================
// Replay Feed
// =============================================================================

/// Replay feed for loading recorded book snapshots into backtests.
pub struct RecordedBookFeed {
    snapshots: Vec<RecordedBookSnapshot>,
    current_index: usize,
}

impl RecordedBookFeed {
    /// Create from stored snapshots.
    pub fn new(snapshots: Vec<RecordedBookSnapshot>) -> Self {
        Self {
            snapshots,
            current_index: 0,
        }
    }

    /// Load from storage for a token in a time range.
    pub fn from_storage(
        storage: &BookSnapshotStorage,
        token_id: &str,
        start_ns: u64,
        end_ns: u64,
    ) -> Result<Self> {
        let snapshots = storage.load_snapshots_in_range(token_id, start_ns, end_ns)?;
        Ok(Self::new(snapshots))
    }

    /// Load all tokens from storage in a time range.
    pub fn from_storage_all(
        storage: &BookSnapshotStorage,
        start_ns: u64,
        end_ns: u64,
    ) -> Result<Self> {
        let snapshots = storage.load_all_snapshots_in_range(start_ns, end_ns)?;
        Ok(Self::new(snapshots))
    }

    /// Get total number of snapshots.
    pub fn len(&self) -> usize {
        self.snapshots.len()
    }

    /// Check if empty.
    pub fn is_empty(&self) -> bool {
        self.snapshots.is_empty()
    }

    /// Get time range covered.
    pub fn time_range(&self) -> Option<(u64, u64)> {
        if self.snapshots.is_empty() {
            None
        } else {
            Some((
                self.snapshots.first().unwrap().arrival_time_ns,
                self.snapshots.last().unwrap().arrival_time_ns,
            ))
        }
    }

    /// Reset to beginning for new replay.
    pub fn reset(&mut self) {
        self.current_index = 0;
    }

    /// Get next snapshot in chronological order.
    pub fn next(&mut self) -> Option<&RecordedBookSnapshot> {
        if self.current_index < self.snapshots.len() {
            let snapshot = &self.snapshots[self.current_index];
            self.current_index += 1;
            Some(snapshot)
        } else {
            None
        }
    }

    /// Peek at next snapshot without advancing.
    pub fn peek(&self) -> Option<&RecordedBookSnapshot> {
        self.snapshots.get(self.current_index)
    }

    /// Get all snapshots.
    pub fn snapshots(&self) -> &[RecordedBookSnapshot] {
        &self.snapshots
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_snapshot(token_id: &str, arrival_ns: u64, seq: u64) -> RecordedBookSnapshot {
        RecordedBookSnapshot::from_ws_message(
            token_id.to_string(),
            vec![
                PriceLevel { price: 0.50, size: 100.0 },
                PriceLevel { price: 0.49, size: 200.0 },
            ],
            vec![
                PriceLevel { price: 0.51, size: 150.0 },
                PriceLevel { price: 0.52, size: 250.0 },
            ],
            Some(seq),
            Some(arrival_ns - 1_000_000), // source 1ms before arrival
            arrival_ns,
            seq,
        )
    }

    #[test]
    fn test_snapshot_creation() {
        let snapshot = make_test_snapshot("TOKEN1", 1_000_000_000, 1);

        assert_eq!(snapshot.token_id, "TOKEN1");
        assert!(snapshot.is_valid());
        assert!(!snapshot.is_crossed());
        assert_eq!(snapshot.best_bid, Some(0.50));
        assert_eq!(snapshot.best_ask, Some(0.51));
        assert!((snapshot.mid_price.unwrap() - 0.505).abs() < 0.001);
        assert!((snapshot.spread.unwrap() - 0.01).abs() < 0.001);
    }

    #[test]
    fn test_crossed_book_detection() {
        let snapshot = RecordedBookSnapshot::from_ws_message(
            "TOKEN1".to_string(),
            vec![PriceLevel { price: 0.52, size: 100.0 }], // Bid higher than ask
            vec![PriceLevel { price: 0.51, size: 100.0 }],
            Some(1),
            None,
            1_000_000_000,
            1,
        );

        assert!(snapshot.is_crossed());
    }

    #[test]
    fn test_storage_basic() {
        let storage = BookSnapshotStorage::open_memory().unwrap();
        let snapshot = make_test_snapshot("TOKEN1", 1_000_000_000, 1);

        storage.store_snapshot(&snapshot).unwrap();

        let count = storage.count_snapshots("TOKEN1").unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn test_storage_batch() {
        let storage = BookSnapshotStorage::open_memory().unwrap();

        let snapshots: Vec<_> = (1..=10)
            .map(|i| make_test_snapshot("TOKEN1", i * 1_000_000_000, i as u64))
            .collect();

        let stored = storage.store_batch(&snapshots).unwrap();
        assert_eq!(stored, 10);

        let count = storage.count_snapshots("TOKEN1").unwrap();
        assert_eq!(count, 10);
    }

    #[test]
    fn test_storage_load_range() {
        let storage = BookSnapshotStorage::open_memory().unwrap();

        // Store snapshots at 1s, 2s, 3s, 4s, 5s
        let snapshots: Vec<_> = (1..=5)
            .map(|i| make_test_snapshot("TOKEN1", i * 1_000_000_000_u64, i as u64))
            .collect();
        storage.store_batch(&snapshots).unwrap();

        // Load 2s to 4s
        let loaded = storage
            .load_snapshots_in_range("TOKEN1", 2_000_000_000, 4_000_000_000)
            .unwrap();
        assert_eq!(loaded.len(), 3);
        assert_eq!(loaded[0].exchange_seq, Some(2));
        assert_eq!(loaded[2].exchange_seq, Some(4));
    }

    #[test]
    fn test_sequence_gap_detection() {
        let storage = BookSnapshotStorage::open_memory().unwrap();

        // Store snapshots with gaps: 1, 2, 5, 6, 10
        let seqs = vec![1, 2, 5, 6, 10];
        let snapshots: Vec<_> = seqs
            .iter()
            .map(|&seq| make_test_snapshot("TOKEN1", seq * 1_000_000_000, seq))
            .collect();
        storage.store_batch(&snapshots).unwrap();

        let gaps = storage.detect_sequence_gaps("TOKEN1").unwrap();
        assert_eq!(gaps.len(), 2);
        assert_eq!(gaps[0], (3, 4)); // Gap from 3 to 4
        assert_eq!(gaps[1], (7, 9)); // Gap from 7 to 9
    }

    #[test]
    fn test_replay_feed() {
        let storage = BookSnapshotStorage::open_memory().unwrap();

        let snapshots: Vec<_> = (1..=5)
            .map(|i| make_test_snapshot("TOKEN1", i * 1_000_000_000_u64, i as u64))
            .collect();
        storage.store_batch(&snapshots).unwrap();

        // Use i64::MAX as u64 to avoid overflow issues in SQL
        let mut feed =
            RecordedBookFeed::from_storage(&storage, "TOKEN1", 0, i64::MAX as u64).unwrap();

        assert_eq!(feed.len(), 5);

        // Iterate through
        let mut count = 0;
        while feed.next().is_some() {
            count += 1;
        }
        assert_eq!(count, 5);

        // Reset and iterate again
        feed.reset();
        let first = feed.next().unwrap();
        assert_eq!(first.exchange_seq, Some(1));
    }

    #[test]
    fn test_time_coverage() {
        let storage = BookSnapshotStorage::open_memory().unwrap();

        let snapshots: Vec<_> = (100..=105)
            .map(|i| make_test_snapshot("TOKEN1", i * 1_000_000_000_u64, i as u64))
            .collect();
        storage.store_batch(&snapshots).unwrap();

        let (start, end) = storage.get_time_coverage("TOKEN1").unwrap().unwrap();
        assert_eq!(start, 100_000_000_000);
        assert_eq!(end, 105_000_000_000);
    }
}

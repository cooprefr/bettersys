//! Trade Print Storage and Replay
//!
//! Persistent storage for PolymarketTradePrint with SQLite backend.
//! Supports append-only recording and deterministic replay.

use crate::backtest_v2::events::Side;
use crate::backtest_v2::trade_print::{
    AggressorSideSource, PolymarketTradePrint, TradePrintBuilder, TradePrintDeduplicator,
    TradeIdSource, TradeSequenceTracker, TradeStreamMetadata, DEFAULT_DEDUP_CACHE_SIZE,
    DEFAULT_TICK_SIZE,
};
use anyhow::{Context, Result};
use parking_lot::Mutex;
use rusqlite::{params, Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tracing::{debug, info, warn};

// =============================================================================
// STORAGE SCHEMA
// =============================================================================

const TRADE_PRINT_STORAGE_SCHEMA: &str = r#"
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;
PRAGMA cache_size = -64000;
PRAGMA temp_store = MEMORY;
PRAGMA mmap_size = 268435456;

-- Full trade prints table with complete schema
CREATE TABLE IF NOT EXISTS polymarket_trade_prints (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    
    -- Market identification
    market_id TEXT NOT NULL,
    token_id TEXT NOT NULL,
    market_slug TEXT,
    
    -- Trade identification
    trade_id TEXT NOT NULL,
    trade_id_source TEXT NOT NULL,
    match_id TEXT,
    trade_seq INTEGER,
    synthetic_trade_seq INTEGER NOT NULL,
    sequence_is_synthetic INTEGER NOT NULL,
    
    -- Trade data
    aggressor_side TEXT NOT NULL,
    aggressor_side_source TEXT NOT NULL,
    price REAL NOT NULL,
    price_fixed INTEGER NOT NULL,
    size REAL NOT NULL,
    size_fixed INTEGER NOT NULL,
    fee_rate_bps INTEGER,
    
    -- Timestamps
    exchange_ts_ns INTEGER,
    ingest_ts_ns INTEGER NOT NULL,
    visible_ts_ns INTEGER NOT NULL,
    
    -- Dataset metadata
    local_seq INTEGER NOT NULL,
    tick_size REAL NOT NULL,
    recorded_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now'))
);

-- Unique constraint for deduplication
CREATE UNIQUE INDEX IF NOT EXISTS idx_trade_prints_dedup
    ON polymarket_trade_prints(market_id, trade_id);

-- Primary replay index: market + visible_ts + synthetic_seq
CREATE INDEX IF NOT EXISTS idx_trade_prints_replay
    ON polymarket_trade_prints(market_id, visible_ts_ns, synthetic_trade_seq);

-- Cross-market replay index
CREATE INDEX IF NOT EXISTS idx_trade_prints_visible_ts
    ON polymarket_trade_prints(visible_ts_ns, synthetic_trade_seq);

-- Token-based queries
CREATE INDEX IF NOT EXISTS idx_trade_prints_token
    ON polymarket_trade_prints(token_id, visible_ts_ns);

-- Per-market metadata table
CREATE TABLE IF NOT EXISTS trade_stream_metadata (
    market_id TEXT PRIMARY KEY,
    encoding_version INTEGER NOT NULL,
    tick_size REAL NOT NULL,
    lot_size REAL NOT NULL,
    price_unit_is_probability INTEGER NOT NULL,
    size_unit_is_shares INTEGER NOT NULL,
    trade_stream_present INTEGER NOT NULL,
    trade_id_source TEXT NOT NULL,
    sequence_is_synthetic INTEGER NOT NULL,
    aggressor_side_source TEXT NOT NULL,
    first_trade_ts_ns INTEGER,
    last_trade_ts_ns INTEGER,
    trade_count INTEGER NOT NULL,
    total_volume REAL NOT NULL,
    updated_at INTEGER NOT NULL
) WITHOUT ROWID;

-- Recording statistics
CREATE TABLE IF NOT EXISTS trade_print_recording_stats (
    key TEXT PRIMARY KEY,
    value INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
) WITHOUT ROWID;
"#;

// =============================================================================
// RECORDING STATISTICS
// =============================================================================

/// Statistics for trade print recording.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TradePrintRecordingStats {
    pub prints_recorded: u64,
    pub prints_skipped_duplicate: u64,
    pub prints_skipped_invalid: u64,
    pub batch_writes: u64,
    pub sequence_gaps_detected: u64,
    pub out_of_order_detected: u64,
    pub total_volume: f64,
    pub total_notional: f64,
}

// =============================================================================
// TRADE PRINT STORAGE
// =============================================================================

/// Persistent storage for Polymarket trade prints.
pub struct TradePrintFullStorage {
    conn: Arc<Mutex<Connection>>,
    deduplicator: Mutex<TradePrintDeduplicator>,
    sequence_tracker: Mutex<TradeSequenceTracker>,
    local_seq: AtomicU64,
    stats: Mutex<TradePrintRecordingStats>,
}

impl TradePrintFullStorage {
    /// Open or create storage at the given path.
    pub fn open(db_path: &str) -> Result<Self> {
        let path = Path::new(db_path);

        // Create parent directory if needed
        if let Some(parent) = path.parent() {
            if !parent.exists() && !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }

        let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX;

        let conn = Connection::open_with_flags(db_path, flags)
            .with_context(|| format!("Failed to open trade print database: {}", db_path))?;

        // Initialize schema
        conn.execute_batch(TRADE_PRINT_STORAGE_SCHEMA)?;

        info!(path = %db_path, "Trade print storage opened");

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            deduplicator: Mutex::new(TradePrintDeduplicator::new(DEFAULT_DEDUP_CACHE_SIZE)),
            sequence_tracker: Mutex::new(TradeSequenceTracker::new()),
            local_seq: AtomicU64::new(0),
            stats: Mutex::new(TradePrintRecordingStats::default()),
        })
    }

    /// Open in-memory storage (for testing).
    pub fn open_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(TRADE_PRINT_STORAGE_SCHEMA)?;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            deduplicator: Mutex::new(TradePrintDeduplicator::new(DEFAULT_DEDUP_CACHE_SIZE)),
            sequence_tracker: Mutex::new(TradeSequenceTracker::new()),
            local_seq: AtomicU64::new(0),
            stats: Mutex::new(TradePrintRecordingStats::default()),
        })
    }

    /// Get next local sequence number.
    pub fn next_local_seq(&self) -> u64 {
        self.local_seq.fetch_add(1, Ordering::Relaxed)
    }

    /// Store a single trade print.
    pub fn store(&self, print: &mut PolymarketTradePrint) -> Result<bool> {
        // Validate
        if let Err(e) = print.validate() {
            let mut stats = self.stats.lock();
            stats.prints_skipped_invalid += 1;
            debug!(error = %e, "Skipping invalid trade print");
            return Ok(false);
        }

        // Check for duplicate
        {
            let mut dedup = self.deduplicator.lock();
            if dedup.is_duplicate(print) {
                let mut stats = self.stats.lock();
                stats.prints_skipped_duplicate += 1;
                return Ok(false);
            }
        }

        // Process sequence
        {
            let mut tracker = self.sequence_tracker.lock();
            if let Some(err) = tracker.process(print) {
                let mut stats = self.stats.lock();
                stats.out_of_order_detected += 1;
                warn!(error = %err, "Sequence violation in trade print");
            }
        }

        // Assign local sequence
        print.local_seq = self.next_local_seq();

        // Insert into database
        let conn = self.conn.lock();
        self.insert_print(&conn, print)?;

        // Update stats
        {
            let mut stats = self.stats.lock();
            stats.prints_recorded += 1;
            stats.total_volume += print.size;
            stats.total_notional += print.notional();
        }

        Ok(true)
    }

    /// Store a batch of trade prints.
    pub fn store_batch(&self, prints: &mut [PolymarketTradePrint]) -> Result<usize> {
        if prints.is_empty() {
            return Ok(0);
        }

        let conn = self.conn.lock();
        conn.execute("BEGIN IMMEDIATE", [])?;

        let mut stored = 0;

        for print in prints.iter_mut() {
            // Validate
            if let Err(e) = print.validate() {
                let mut stats = self.stats.lock();
                stats.prints_skipped_invalid += 1;
                debug!(error = %e, "Skipping invalid trade print");
                continue;
            }

            // Check for duplicate
            {
                let mut dedup = self.deduplicator.lock();
                if dedup.is_duplicate(print) {
                    let mut stats = self.stats.lock();
                    stats.prints_skipped_duplicate += 1;
                    continue;
                }
            }

            // Process sequence
            {
                let mut tracker = self.sequence_tracker.lock();
                if let Some(err) = tracker.process(print) {
                    let mut stats = self.stats.lock();
                    stats.out_of_order_detected += 1;
                    warn!(error = %err, "Sequence violation in trade print");
                }
            }

            // Assign local sequence
            print.local_seq = self.next_local_seq();

            // Insert
            if self.insert_print(&conn, print).is_ok() {
                stored += 1;
                let mut stats = self.stats.lock();
                stats.total_volume += print.size;
                stats.total_notional += print.notional();
            }
        }

        conn.execute("COMMIT", [])?;

        // Update stats
        {
            let mut stats = self.stats.lock();
            stats.prints_recorded += stored as u64;
            stats.batch_writes += 1;
        }

        Ok(stored)
    }

    fn insert_print(&self, conn: &Connection, print: &PolymarketTradePrint) -> Result<()> {
        conn.execute(
            r#"
            INSERT OR IGNORE INTO polymarket_trade_prints (
                market_id, token_id, market_slug,
                trade_id, trade_id_source, match_id, trade_seq, synthetic_trade_seq, sequence_is_synthetic,
                aggressor_side, aggressor_side_source,
                price, price_fixed, size, size_fixed, fee_rate_bps,
                exchange_ts_ns, ingest_ts_ns, visible_ts_ns,
                local_seq, tick_size
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21)
            "#,
            params![
                print.market_id,
                print.token_id,
                print.market_slug,
                print.trade_id,
                format!("{:?}", print.trade_id_source),
                print.match_id,
                print.trade_seq.map(|s| s as i64),
                print.synthetic_trade_seq as i64,
                print.sequence_is_synthetic as i32,
                match print.aggressor_side {
                    Side::Buy => "BUY",
                    Side::Sell => "SELL",
                },
                format!("{:?}", print.aggressor_side_source),
                print.price,
                print.price_fixed,
                print.size,
                print.size_fixed,
                print.fee_rate_bps,
                print.exchange_ts_ns,
                print.ingest_ts_ns,
                print.visible_ts_ns,
                print.local_seq as i64,
                print.tick_size,
            ],
        )?;
        Ok(())
    }

    /// Load trade prints for a market in a time range (by visible_ts).
    pub fn load_by_visible_ts(
        &self,
        market_id: &str,
        start_visible_ns: i64,
        end_visible_ns: i64,
    ) -> Result<Vec<PolymarketTradePrint>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            r#"
            SELECT
                market_id, token_id, market_slug,
                trade_id, trade_id_source, match_id, trade_seq, synthetic_trade_seq, sequence_is_synthetic,
                aggressor_side, aggressor_side_source,
                price, price_fixed, size, size_fixed, fee_rate_bps,
                exchange_ts_ns, ingest_ts_ns, visible_ts_ns,
                local_seq, tick_size
            FROM polymarket_trade_prints
            WHERE market_id = ?1 AND visible_ts_ns >= ?2 AND visible_ts_ns <= ?3
            ORDER BY visible_ts_ns ASC, synthetic_trade_seq ASC
            "#,
        )?;

        let prints = stmt
            .query_map(params![market_id, start_visible_ns, end_visible_ns], |row| {
                self.row_to_print(row)
            })?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(prints)
    }

    /// Load all trade prints across all markets in a time range.
    pub fn load_all_by_visible_ts(
        &self,
        start_visible_ns: i64,
        end_visible_ns: i64,
    ) -> Result<Vec<PolymarketTradePrint>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            r#"
            SELECT
                market_id, token_id, market_slug,
                trade_id, trade_id_source, match_id, trade_seq, synthetic_trade_seq, sequence_is_synthetic,
                aggressor_side, aggressor_side_source,
                price, price_fixed, size, size_fixed, fee_rate_bps,
                exchange_ts_ns, ingest_ts_ns, visible_ts_ns,
                local_seq, tick_size
            FROM polymarket_trade_prints
            WHERE visible_ts_ns >= ?1 AND visible_ts_ns <= ?2
            ORDER BY visible_ts_ns ASC, synthetic_trade_seq ASC
            "#,
        )?;

        let prints = stmt
            .query_map(params![start_visible_ns, end_visible_ns], |row| {
                self.row_to_print(row)
            })?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(prints)
    }

    fn row_to_print(&self, row: &rusqlite::Row) -> rusqlite::Result<PolymarketTradePrint> {
        let trade_id_source_str: String = row.get(4)?;
        let aggressor_side_str: String = row.get(9)?;
        let aggressor_side_source_str: String = row.get(10)?;

        Ok(PolymarketTradePrint {
            market_id: row.get(0)?,
            token_id: row.get(1)?,
            market_slug: row.get(2)?,
            trade_id: row.get(3)?,
            trade_id_source: parse_trade_id_source(&trade_id_source_str),
            match_id: row.get(5)?,
            trade_seq: row.get::<_, Option<i64>>(6)?.map(|s| s as u64),
            synthetic_trade_seq: row.get::<_, i64>(7)? as u64,
            sequence_is_synthetic: row.get::<_, i32>(8)? != 0,
            aggressor_side: if aggressor_side_str == "BUY" {
                Side::Buy
            } else {
                Side::Sell
            },
            aggressor_side_source: parse_aggressor_side_source(&aggressor_side_source_str),
            price: row.get(11)?,
            price_fixed: row.get(12)?,
            size: row.get(13)?,
            size_fixed: row.get(14)?,
            fee_rate_bps: row.get(15)?,
            exchange_ts_ns: row.get(16)?,
            ingest_ts_ns: row.get(17)?,
            visible_ts_ns: row.get(18)?,
            local_seq: row.get::<_, i64>(19)? as u64,
            tick_size: row.get(20)?,
            size_unit_is_shares: true,
        })
    }

    /// Get time coverage for a market.
    pub fn get_time_coverage(&self, market_id: &str) -> Result<Option<(i64, i64)>> {
        let conn = self.conn.lock();
        let result = conn.query_row(
            r#"
            SELECT MIN(visible_ts_ns), MAX(visible_ts_ns)
            FROM polymarket_trade_prints
            WHERE market_id = ?1
            "#,
            params![market_id],
            |row| {
                let min: Option<i64> = row.get(0)?;
                let max: Option<i64> = row.get(1)?;
                Ok((min, max))
            },
        )?;

        match result {
            (Some(min), Some(max)) => Ok(Some((min, max))),
            _ => Ok(None),
        }
    }

    /// Count trade prints for a market.
    pub fn count_prints(&self, market_id: &str) -> Result<usize> {
        let conn = self.conn.lock();
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM polymarket_trade_prints WHERE market_id = ?1",
            params![market_id],
            |row| row.get(0),
        )?;
        Ok(count as usize)
    }

    /// Get total count across all markets.
    pub fn total_count(&self) -> Result<usize> {
        let conn = self.conn.lock();
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM polymarket_trade_prints",
            [],
            |row| row.get(0),
        )?;
        Ok(count as usize)
    }

    /// Get all unique market IDs.
    pub fn get_market_ids(&self) -> Result<Vec<String>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT DISTINCT market_id FROM polymarket_trade_prints ORDER BY market_id",
        )?;
        let ids = stmt
            .query_map([], |row| row.get(0))?
            .collect::<Result<Vec<String>, _>>()?;
        Ok(ids)
    }

    /// Get stream metadata for a market.
    pub fn get_stream_metadata(&self, market_id: &str) -> Result<Option<TradeStreamMetadata>> {
        let conn = self.conn.lock();
        let result = conn.query_row(
            r#"
            SELECT
                encoding_version, tick_size, lot_size,
                price_unit_is_probability, size_unit_is_shares,
                trade_stream_present, trade_id_source, sequence_is_synthetic,
                aggressor_side_source, first_trade_ts_ns, last_trade_ts_ns,
                trade_count, total_volume
            FROM trade_stream_metadata
            WHERE market_id = ?1
            "#,
            params![market_id],
            |row| {
                Ok(TradeStreamMetadata {
                    market_id: market_id.to_string(),
                    encoding_version: row.get(0)?,
                    tick_size: row.get(1)?,
                    lot_size: row.get(2)?,
                    price_unit_is_probability: row.get::<_, i32>(3)? != 0,
                    size_unit_is_shares: row.get::<_, i32>(4)? != 0,
                    trade_stream_present: row.get::<_, i32>(5)? != 0,
                    trade_id_source: parse_trade_id_source(&row.get::<_, String>(6)?),
                    sequence_is_synthetic: row.get::<_, i32>(7)? != 0,
                    aggressor_side_source: parse_aggressor_side_source(&row.get::<_, String>(8)?),
                    first_trade_ts_ns: row.get(9)?,
                    last_trade_ts_ns: row.get(10)?,
                    trade_count: row.get::<_, i64>(11)? as u64,
                    total_volume: row.get(12)?,
                })
            },
        );

        match result {
            Ok(meta) => Ok(Some(meta)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Update stream metadata for a market.
    pub fn update_stream_metadata(&self, market_id: &str) -> Result<()> {
        let conn = self.conn.lock();

        // Get aggregated stats
        let (first_ts, last_ts, trade_count, total_volume, tick_size) = conn.query_row(
            r#"
            SELECT
                MIN(visible_ts_ns), MAX(visible_ts_ns),
                COUNT(*), COALESCE(SUM(size), 0),
                MIN(tick_size)
            FROM polymarket_trade_prints
            WHERE market_id = ?1
            "#,
            params![market_id],
            |row| {
                Ok((
                    row.get::<_, Option<i64>>(0)?,
                    row.get::<_, Option<i64>>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, f64>(3)?,
                    row.get::<_, Option<f64>>(4)?.unwrap_or(DEFAULT_TICK_SIZE),
                ))
            },
        )?;

        if trade_count == 0 {
            return Ok(()); // No data
        }

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;

        conn.execute(
            r#"
            INSERT INTO trade_stream_metadata (
                market_id, encoding_version, tick_size, lot_size,
                price_unit_is_probability, size_unit_is_shares,
                trade_stream_present, trade_id_source, sequence_is_synthetic,
                aggressor_side_source, first_trade_ts_ns, last_trade_ts_ns,
                trade_count, total_volume, updated_at
            ) VALUES (?1, 1, ?2, 1.0, 1, 1, 1, 'Synthetic', 1, 'Unknown', ?3, ?4, ?5, ?6, ?7)
            ON CONFLICT(market_id) DO UPDATE SET
                first_trade_ts_ns = COALESCE(excluded.first_trade_ts_ns, trade_stream_metadata.first_trade_ts_ns),
                last_trade_ts_ns = excluded.last_trade_ts_ns,
                trade_count = excluded.trade_count,
                total_volume = excluded.total_volume,
                updated_at = excluded.updated_at
            "#,
            params![market_id, tick_size, first_ts, last_ts, trade_count, total_volume, now],
        )?;

        Ok(())
    }

    /// Check if trade stream is present for a market.
    pub fn has_trade_stream(&self, market_id: &str) -> Result<bool> {
        let count = self.count_prints(market_id)?;
        Ok(count > 0)
    }

    /// Get recording statistics.
    pub fn stats(&self) -> TradePrintRecordingStats {
        self.stats.lock().clone()
    }

    /// Compute deterministic fingerprint of trade stream for a market.
    pub fn compute_stream_fingerprint(&self, market_id: &str) -> Result<u64> {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let prints = self.load_by_visible_ts(market_id, i64::MIN, i64::MAX)?;

        let mut hasher = DefaultHasher::new();
        for print in &prints {
            print.fingerprint().hash(&mut hasher);
        }
        prints.len().hash(&mut hasher);

        Ok(hasher.finish())
    }
}

fn parse_trade_id_source(s: &str) -> TradeIdSource {
    match s {
        "NativeVenueId" => TradeIdSource::NativeVenueId,
        "CompositeDerived" => TradeIdSource::CompositeDerived,
        "HashDerived" => TradeIdSource::HashDerived,
        _ => TradeIdSource::Synthetic,
    }
}

fn parse_aggressor_side_source(s: &str) -> AggressorSideSource {
    match s {
        "VenueProvided" => AggressorSideSource::VenueProvided,
        "InferredTickRule" => AggressorSideSource::InferredTickRule,
        "InferredQuoteRule" => AggressorSideSource::InferredQuoteRule,
        _ => AggressorSideSource::Unknown,
    }
}

// =============================================================================
// REPLAY FEED
// =============================================================================

/// Replay feed for loading trade prints into backtests.
///
/// Events are ordered by (visible_ts, synthetic_trade_seq) for deterministic replay.
pub struct TradePrintReplayFeed {
    prints: Vec<PolymarketTradePrint>,
    current_index: usize,
}

impl TradePrintReplayFeed {
    /// Create from a vector of prints (assumed already sorted).
    pub fn new(prints: Vec<PolymarketTradePrint>) -> Self {
        Self {
            prints,
            current_index: 0,
        }
    }

    /// Load from storage for a market.
    pub fn from_storage(
        storage: &TradePrintFullStorage,
        market_id: &str,
        start_visible_ns: i64,
        end_visible_ns: i64,
    ) -> Result<Self> {
        let prints = storage.load_by_visible_ts(market_id, start_visible_ns, end_visible_ns)?;
        Ok(Self::new(prints))
    }

    /// Load all markets from storage.
    pub fn from_storage_all(
        storage: &TradePrintFullStorage,
        start_visible_ns: i64,
        end_visible_ns: i64,
    ) -> Result<Self> {
        let prints = storage.load_all_by_visible_ts(start_visible_ns, end_visible_ns)?;
        Ok(Self::new(prints))
    }

    /// Total number of prints.
    pub fn len(&self) -> usize {
        self.prints.len()
    }

    /// Check if empty.
    pub fn is_empty(&self) -> bool {
        self.prints.is_empty()
    }

    /// Time range covered (visible_ts).
    pub fn time_range(&self) -> Option<(i64, i64)> {
        if self.prints.is_empty() {
            None
        } else {
            Some((
                self.prints.first().unwrap().visible_ts_ns,
                self.prints.last().unwrap().visible_ts_ns,
            ))
        }
    }

    /// Reset for new replay.
    pub fn reset(&mut self) {
        self.current_index = 0;
    }

    /// Get next print in chronological order.
    pub fn next(&mut self) -> Option<&PolymarketTradePrint> {
        if self.current_index < self.prints.len() {
            let print = &self.prints[self.current_index];
            self.current_index += 1;
            Some(print)
        } else {
            None
        }
    }

    /// Peek at next print without advancing.
    pub fn peek(&self) -> Option<&PolymarketTradePrint> {
        self.prints.get(self.current_index)
    }

    /// Peek at visible_ts of next print.
    pub fn peek_visible_ts(&self) -> Option<i64> {
        self.peek().map(|p| p.visible_ts_ns)
    }

    /// Drain all prints up to (and including) a visible timestamp.
    /// Returns the count of prints drained.
    pub fn drain_until(&mut self, cutoff_visible_ns: i64) -> usize {
        let mut count = 0;
        while let Some(ts) = self.peek_visible_ts() {
            if ts > cutoff_visible_ns {
                break;
            }
            if self.next().is_some() {
                count += 1;
            }
        }
        count
    }

    /// Get prints in range by index (for post-drain inspection).
    pub fn get_range(&self, start: usize, end: usize) -> &[PolymarketTradePrint] {
        let end = end.min(self.prints.len());
        let start = start.min(end);
        &self.prints[start..end]
    }

    /// Get all prints.
    pub fn prints(&self) -> &[PolymarketTradePrint] {
        &self.prints
    }

    /// Total volume in feed.
    pub fn total_volume(&self) -> f64 {
        self.prints.iter().map(|p| p.size).sum()
    }

    /// Total notional in feed.
    pub fn total_notional(&self) -> f64 {
        self.prints.iter().map(|p| p.notional()).sum()
    }

    /// Compute deterministic fingerprint for replay validation.
    pub fn fingerprint(&self) -> u64 {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let mut hasher = DefaultHasher::new();
        for print in &self.prints {
            print.fingerprint().hash(&mut hasher);
        }
        self.prints.len().hash(&mut hasher);
        hasher.finish()
    }
}

// =============================================================================
// TESTS
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_print(
        market_id: &str,
        trade_id: &str,
        visible_ts_ns: i64,
        synthetic_seq: u64,
    ) -> PolymarketTradePrint {
        let mut print = TradePrintBuilder::new()
            .market_id(market_id)
            .token_id("token_123")
            .trade_id(trade_id, TradeIdSource::NativeVenueId)
            .aggressor_side(Side::Buy, AggressorSideSource::VenueProvided)
            .price(0.55)
            .size(100.0)
            .ingest_ts_ns(visible_ts_ns - 1000)
            .build()
            .unwrap();

        print.visible_ts_ns = visible_ts_ns;
        print.synthetic_trade_seq = synthetic_seq;
        print
    }

    #[test]
    fn test_storage_open() {
        let storage = TradePrintFullStorage::open_memory().unwrap();
        assert_eq!(storage.total_count().unwrap(), 0);
    }

    #[test]
    fn test_storage_store_single() {
        let storage = TradePrintFullStorage::open_memory().unwrap();

        let mut print = make_test_print("market_1", "trade_001", 1_000_000_000, 0);
        let stored = storage.store(&mut print).unwrap();

        assert!(stored);
        assert_eq!(storage.count_prints("market_1").unwrap(), 1);
    }

    #[test]
    fn test_storage_deduplication() {
        let storage = TradePrintFullStorage::open_memory().unwrap();

        let mut print1 = make_test_print("market_1", "trade_001", 1_000_000_000, 0);
        let mut print2 = make_test_print("market_1", "trade_001", 1_000_000_000, 0); // Same trade_id

        assert!(storage.store(&mut print1).unwrap());
        assert!(!storage.store(&mut print2).unwrap()); // Duplicate

        assert_eq!(storage.count_prints("market_1").unwrap(), 1);
        assert_eq!(storage.stats().prints_skipped_duplicate, 1);
    }

    #[test]
    fn test_storage_batch() {
        let storage = TradePrintFullStorage::open_memory().unwrap();

        let mut prints: Vec<_> = (1..=5)
            .map(|i| make_test_print("market_1", &format!("trade_{:03}", i), i * 1_000_000_000, i as u64))
            .collect();

        let stored = storage.store_batch(&mut prints).unwrap();
        assert_eq!(stored, 5);
        assert_eq!(storage.count_prints("market_1").unwrap(), 5);
    }

    #[test]
    fn test_storage_load_by_visible_ts() {
        let storage = TradePrintFullStorage::open_memory().unwrap();

        let mut prints: Vec<_> = (1..=5)
            .map(|i| make_test_print("market_1", &format!("trade_{:03}", i), i * 1_000_000_000, i as u64))
            .collect();
        storage.store_batch(&mut prints).unwrap();

        // Load middle range
        let loaded = storage
            .load_by_visible_ts("market_1", 2_000_000_000, 4_000_000_000)
            .unwrap();

        assert_eq!(loaded.len(), 3);
        assert_eq!(loaded[0].visible_ts_ns, 2_000_000_000);
        assert_eq!(loaded[2].visible_ts_ns, 4_000_000_000);
    }

    #[test]
    fn test_replay_feed_ordering() {
        let storage = TradePrintFullStorage::open_memory().unwrap();

        // Insert in random order
        let mut print3 = make_test_print("market_1", "trade_003", 3_000_000_000, 3);
        let mut print1 = make_test_print("market_1", "trade_001", 1_000_000_000, 1);
        let mut print2 = make_test_print("market_1", "trade_002", 2_000_000_000, 2);

        storage.store(&mut print3).unwrap();
        storage.store(&mut print1).unwrap();
        storage.store(&mut print2).unwrap();

        // Load should be ordered by visible_ts
        let mut feed = TradePrintReplayFeed::from_storage(
            &storage,
            "market_1",
            i64::MIN,
            i64::MAX,
        )
        .unwrap();

        assert_eq!(feed.next().unwrap().visible_ts_ns, 1_000_000_000);
        assert_eq!(feed.next().unwrap().visible_ts_ns, 2_000_000_000);
        assert_eq!(feed.next().unwrap().visible_ts_ns, 3_000_000_000);
        assert!(feed.next().is_none());
    }

    #[test]
    fn test_replay_feed_drain_until() {
        let prints: Vec<_> = (1..=5)
            .map(|i| make_test_print("market_1", &format!("trade_{:03}", i), i * 1_000_000_000, i as u64))
            .collect();

        let mut feed = TradePrintReplayFeed::new(prints);

        let drained_count = feed.drain_until(3_000_000_000);
        assert_eq!(drained_count, 3);

        assert_eq!(feed.len() - feed.current_index, 2); // 2 remaining
    }

    #[test]
    fn test_replay_feed_fingerprint_determinism() {
        let prints1: Vec<_> = (1..=3)
            .map(|i| make_test_print("market_1", &format!("trade_{:03}", i), i * 1_000_000_000, i as u64))
            .collect();

        let prints2: Vec<_> = (1..=3)
            .map(|i| make_test_print("market_1", &format!("trade_{:03}", i), i * 1_000_000_000, i as u64))
            .collect();

        let feed1 = TradePrintReplayFeed::new(prints1);
        let feed2 = TradePrintReplayFeed::new(prints2);

        assert_eq!(feed1.fingerprint(), feed2.fingerprint());
    }

    #[test]
    fn test_time_coverage() {
        let storage = TradePrintFullStorage::open_memory().unwrap();

        let mut prints: Vec<_> = (1..=5)
            .map(|i| make_test_print("market_1", &format!("trade_{:03}", i), i * 1_000_000_000, i as u64))
            .collect();
        storage.store_batch(&mut prints).unwrap();

        let coverage = storage.get_time_coverage("market_1").unwrap().unwrap();
        assert_eq!(coverage.0, 1_000_000_000);
        assert_eq!(coverage.1, 5_000_000_000);
    }

    #[test]
    fn test_multiple_markets() {
        let storage = TradePrintFullStorage::open_memory().unwrap();

        let mut print1 = make_test_print("market_1", "trade_001", 1_000_000_000, 0);
        let mut print2 = make_test_print("market_2", "trade_001", 2_000_000_000, 0);

        storage.store(&mut print1).unwrap();
        storage.store(&mut print2).unwrap();

        let markets = storage.get_market_ids().unwrap();
        assert_eq!(markets.len(), 2);
        assert!(markets.contains(&"market_1".to_string()));
        assert!(markets.contains(&"market_2".to_string()));
    }
}

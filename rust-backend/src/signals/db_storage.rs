//! High-Performance Database-backed Signal Storage
//! Optimized for 10M+ signals with minimal latency
//!
//! Key optimizations:
//! - WAL mode for concurrent reads during writes
//! - Prepared statement caching
//! - Batch inserts with transactions
//! - Async-friendly connection pooling via tokio
//! - Optimized indexes for common query patterns
//! - No row limits - scales to 10M+ signals

use crate::{
    models::{MarketSignal, SignalContext, SignalContextRecord},
    scrapers::dome_rest::DomeOrder,
};
use anyhow::{Context, Result};
use parking_lot::Mutex; // Faster than std::sync::Mutex
use rusqlite::{params, params_from_iter, Connection, OpenFlags};
use std::sync::Arc;
use tracing::{debug, info, warn};

/// Schema with optimizations for high-volume storage
const SCHEMA_SQL: &str = r#"
-- Enable WAL mode for better concurrent access
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;
PRAGMA cache_size = -64000;  -- 64MB cache
PRAGMA temp_store = MEMORY;
PRAGMA mmap_size = 268435456;  -- 256MB memory-mapped I/O

CREATE TABLE IF NOT EXISTS signals (
    id TEXT PRIMARY KEY,
    signal_type TEXT NOT NULL,
    market_slug TEXT NOT NULL,
    confidence REAL NOT NULL,
    risk_level TEXT NOT NULL,
    details_json TEXT NOT NULL,
    detected_at TEXT NOT NULL,
    source TEXT NOT NULL,
    created_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now'))
) WITHOUT ROWID;  -- Clustered index on PRIMARY KEY for faster lookups

CREATE TABLE IF NOT EXISTS metadata (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
) WITHOUT ROWID;

-- Covering indexes for common queries (include all needed columns)
CREATE INDEX IF NOT EXISTS idx_signals_recent 
    ON signals(detected_at DESC, id, signal_type, market_slug, confidence, risk_level, details_json, source);
    
CREATE INDEX IF NOT EXISTS idx_signals_confidence 
    ON signals(confidence DESC, detected_at DESC);
    
CREATE INDEX IF NOT EXISTS idx_signals_source 
    ON signals(source, detected_at DESC);
    
CREATE INDEX IF NOT EXISTS idx_signals_market 
    ON signals(market_slug, detected_at DESC);

-- Partial index for high-confidence signals (most queried)
CREATE INDEX IF NOT EXISTS idx_signals_high_conf 
    ON signals(detected_at DESC) WHERE confidence >= 0.7;

-- Signal enrichment context (attached to signals by signal_id)
CREATE TABLE IF NOT EXISTS signal_context (
    signal_id TEXT PRIMARY KEY,
    context_version INTEGER NOT NULL,
    context_json TEXT NOT NULL,
    enriched_at INTEGER NOT NULL,
    status TEXT NOT NULL,
    error TEXT
) WITHOUT ROWID;

CREATE INDEX IF NOT EXISTS idx_signal_context_enriched_at
    ON signal_context(enriched_at DESC);

-- Raw Dome order events (lossless) for debugging and future analytics
CREATE TABLE IF NOT EXISTS dome_order_events (
    order_hash TEXT PRIMARY KEY,
    tx_hash TEXT,
    user TEXT NOT NULL,
    market_slug TEXT NOT NULL,
    condition_id TEXT NOT NULL,
    token_id TEXT NOT NULL,
    timestamp INTEGER NOT NULL,
    payload_json TEXT NOT NULL,
    received_at INTEGER NOT NULL
) WITHOUT ROWID;

CREATE INDEX IF NOT EXISTS idx_dome_order_events_user_ts
    ON dome_order_events(user, timestamp DESC);

-- Simple DB-backed caches to reduce repeated Dome REST calls
CREATE TABLE IF NOT EXISTS dome_cache (
    cache_key TEXT PRIMARY KEY,
    cache_json TEXT NOT NULL,
    fetched_at INTEGER NOT NULL
) WITHOUT ROWID;

CREATE INDEX IF NOT EXISTS idx_dome_cache_fetched_at
    ON dome_cache(fetched_at DESC);
"#;

/// High-performance signal storage
pub struct DbSignalStorage {
    conn: Arc<Mutex<Connection>>,
}

impl DbSignalStorage {
    /// Create new optimized database storage
    pub fn new(db_path: &str) -> Result<Self> {
        let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX; // We handle our own locking

        let conn = Connection::open_with_flags(db_path, flags)
            .with_context(|| format!("Failed to open database at {}", db_path))?;

        // Apply performance pragmas and schema
        conn.execute_batch(SCHEMA_SQL)
            .context("Failed to initialize database schema")?;

        // Verify WAL mode is active
        let journal_mode: String = conn
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap_or_default();

        if journal_mode.to_lowercase() != "wal" {
            warn!("WAL mode not active, journal_mode = {}", journal_mode);
        }

        info!("📊 High-performance database initialized at: {}", db_path);

        // Get signal count efficiently
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM signals", [], |row| row.get(0))
            .unwrap_or(0);

        info!("📈 Existing signals in database: {}", count);

        // Initialize cumulative counter
        conn.execute(
            "INSERT OR IGNORE INTO metadata (key, value) VALUES ('total_signals_ever', ?1)",
            params![count.to_string()],
        )
        .ok();

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Store a signal with optimized single-row insert
    #[inline]
    pub async fn store(&self, signal: &MarketSignal) -> Result<()> {
        // Pre-serialize outside the lock
        let details_json = serde_json::to_string(&signal.details)?;
        let signal_type_json = serde_json::to_string(&signal.signal_type)?;

        let conn = self.conn.lock();

        // Use INSERT OR IGNORE + UPDATE pattern for better performance
        // than INSERT OR REPLACE (avoids deleting and recreating indexes)
        let changes = conn.execute(
            "INSERT OR IGNORE INTO signals 
             (id, signal_type, market_slug, confidence, risk_level, details_json, detected_at, source)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                &signal.id,
                &signal_type_json,
                &signal.market_slug,
                signal.confidence,
                &signal.risk_level,
                &details_json,
                &signal.detected_at,
                &signal.source,
            ],
        )?;

        // Only increment counter if this was a new insert
        if changes > 0 {
            conn.execute(
                "UPDATE metadata SET value = CAST(CAST(value AS INTEGER) + 1 AS TEXT) 
                 WHERE key = 'total_signals_ever'",
                [],
            )
            .ok();
        }

        Ok(())
    }

    /// Batch store multiple signals in a single transaction (much faster)
    pub async fn store_batch(&self, signals: &[MarketSignal]) -> Result<usize> {
        if signals.is_empty() {
            return Ok(0);
        }

        // Pre-serialize all signals outside the lock
        let serialized: Vec<_> = signals
            .iter()
            .map(|s| {
                let details = serde_json::to_string(&s.details).unwrap_or_default();
                let signal_type = serde_json::to_string(&s.signal_type).unwrap_or_default();
                (s, details, signal_type)
            })
            .collect();

        let conn = self.conn.lock();

        conn.execute("BEGIN IMMEDIATE", [])?;

        let mut inserted = 0usize;

        for (signal, details_json, signal_type_json) in &serialized {
            let changes = conn.execute(
                "INSERT OR IGNORE INTO signals 
                 (id, signal_type, market_slug, confidence, risk_level, details_json, detected_at, source)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    &signal.id,
                    signal_type_json,
                    &signal.market_slug,
                    signal.confidence,
                    &signal.risk_level,
                    details_json,
                    &signal.detected_at,
                    &signal.source,
                ],
            )?;
            inserted += changes;
        }

        // Update counter with total new inserts
        if inserted > 0 {
            conn.execute(
                &format!(
                    "UPDATE metadata SET value = CAST(CAST(value AS INTEGER) + {} AS TEXT) 
                     WHERE key = 'total_signals_ever'",
                    inserted
                ),
                [],
            )
            .ok();
        }

        conn.execute("COMMIT", [])?;

        debug!("📦 Batch inserted {} signals", inserted);
        Ok(inserted)
    }

    /// Get total signals ever recorded (cumulative)
    #[inline]
    pub fn get_total_signals_ever(&self) -> Result<i64> {
        let conn = self.conn.lock();
        let total: i64 = conn
            .query_row(
                "SELECT CAST(value AS INTEGER) FROM metadata WHERE key = 'total_signals_ever'",
                [],
                |row| row.get(0),
            )
            .unwrap_or(0);
        Ok(total)
    }

    /// Get recent signals - optimized query using covering index
    #[inline]
    pub fn get_recent(&self, limit: usize) -> Result<Vec<MarketSignal>> {
        let conn = self.conn.lock();

        let mut stmt = conn.prepare_cached(
            "SELECT id, signal_type, market_slug, confidence, risk_level, 
                    details_json, detected_at, source
             FROM signals 
             ORDER BY detected_at DESC, id
             LIMIT ?1",
        )?;

        let signals = stmt
            .query_map([limit], Self::row_to_signal)?
            .filter_map(|r| r.ok())
            .collect();

        Ok(signals)
    }

    /// Get signals strictly older than a cursor (detected_at, id) - for pagination.
    ///
    /// Ordering is deterministic and matches `get_recent`: (detected_at DESC, id ASC).
    #[inline]
    pub fn get_before(
        &self,
        before_detected_at: &str,
        before_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<MarketSignal>> {
        let conn = self.conn.lock();

        // NOTE: `id` is used as a deterministic tie-breaker when multiple signals share the same
        // detected_at timestamp.
        let signals = if let Some(before_id) = before_id {
            let mut stmt = conn.prepare_cached(
                "SELECT id, signal_type, market_slug, confidence, risk_level,
                        details_json, detected_at, source
                 FROM signals
                 WHERE detected_at < ?1 OR (detected_at = ?1 AND id > ?2)
                 ORDER BY detected_at DESC, id
                 LIMIT ?3",
            )?;

            let signals: Vec<MarketSignal> = stmt
                .query_map(
                    params![before_detected_at, before_id, limit],
                    Self::row_to_signal,
                )?
                .filter_map(|r| r.ok())
                .collect();

            signals
        } else {
            let mut stmt = conn.prepare_cached(
                "SELECT id, signal_type, market_slug, confidence, risk_level,
                        details_json, detected_at, source
                 FROM signals
                 WHERE detected_at < ?1
                 ORDER BY detected_at DESC, id
                 LIMIT ?2",
            )?;

            let signals: Vec<MarketSignal> = stmt
                .query_map(params![before_detected_at, limit], Self::row_to_signal)?
                .filter_map(|r| r.ok())
                .collect();

            signals
        };

        Ok(signals)
    }

    /// Get high-confidence signals - uses partial index
    #[inline]
    pub fn get_high_confidence(&self, limit: usize) -> Result<Vec<MarketSignal>> {
        let conn = self.conn.lock();

        let mut stmt = conn.prepare_cached(
            "SELECT id, signal_type, market_slug, confidence, risk_level, 
                    details_json, detected_at, source
             FROM signals 
             WHERE confidence >= 0.7
             ORDER BY detected_at DESC 
             LIMIT ?1",
        )?;

        let signals = stmt
            .query_map([limit], Self::row_to_signal)?
            .filter_map(|r| r.ok())
            .collect();

        Ok(signals)
    }

    /// Get signals by source
    #[inline]
    pub fn get_by_source(&self, source: &str, limit: usize) -> Result<Vec<MarketSignal>> {
        let conn = self.conn.lock();

        let mut stmt = conn.prepare_cached(
            "SELECT id, signal_type, market_slug, confidence, risk_level, 
                    details_json, detected_at, source
             FROM signals 
             WHERE source = ?1
             ORDER BY detected_at DESC 
             LIMIT ?2",
        )?;

        let signals = stmt
            .query_map(params![source, limit], Self::row_to_signal)?
            .filter_map(|r| r.ok())
            .collect();

        Ok(signals)
    }

    /// Get signals by market
    #[inline]
    pub fn get_by_market(&self, market_slug: &str, limit: usize) -> Result<Vec<MarketSignal>> {
        let conn = self.conn.lock();

        let mut stmt = conn.prepare_cached(
            "SELECT id, signal_type, market_slug, confidence, risk_level, 
                    details_json, detected_at, source
             FROM signals 
             WHERE market_slug = ?1
             ORDER BY detected_at DESC 
             LIMIT ?2",
        )?;

        let signals = stmt
            .query_map(params![market_slug, limit], Self::row_to_signal)?
            .filter_map(|r| r.ok())
            .collect();

        Ok(signals)
    }

    /// Convert a database row to MarketSignal
    #[inline]
    fn row_to_signal(row: &rusqlite::Row) -> rusqlite::Result<MarketSignal> {
        let id: String = row.get(0)?;
        let signal_type_str: String = row.get(1)?;
        let market_slug: String = row.get(2)?;
        let confidence: f64 = row.get(3)?;
        let risk_level: String = row.get(4)?;
        let details_str: String = row.get(5)?;
        let detected_at: String = row.get(6)?;
        let source: String = row.get(7)?;

        let signal_type = serde_json::from_str(&signal_type_str)
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
        let details = serde_json::from_str(&details_str)
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;

        Ok(MarketSignal {
            id,
            signal_type,
            market_slug,
            confidence,
            risk_level,
            details,
            detected_at,
            source,
        })
    }

    /// Get current signal count in database
    #[inline]
    pub fn len(&self) -> usize {
        let conn = self.conn.lock();
        conn.query_row("SELECT COUNT(*) FROM signals", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap_or(0) as usize
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Get database statistics
    pub fn get_stats(&self) -> Result<DatabaseStats> {
        let conn = self.conn.lock();

        let total_signals: i64 =
            conn.query_row("SELECT COUNT(*) FROM signals", [], |row| row.get(0))?;

        let total_ever: i64 = conn
            .query_row(
                "SELECT CAST(value AS INTEGER) FROM metadata WHERE key = 'total_signals_ever'",
                [],
                |row| row.get(0),
            )
            .unwrap_or(total_signals);

        let sources: Vec<(String, i64)> = {
            let mut stmt = conn.prepare(
                "SELECT source, COUNT(*) as count 
                 FROM signals 
                 GROUP BY source 
                 ORDER BY count DESC",
            )?;

            let results: Vec<_> = stmt
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
                .filter_map(|r| r.ok())
                .collect();
            results
        };

        let avg_confidence: f64 = conn
            .query_row("SELECT AVG(confidence) FROM signals", [], |row| row.get(0))
            .unwrap_or(0.0);

        let high_conf_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM signals WHERE confidence >= 0.7",
                [],
                |row| row.get(0),
            )
            .unwrap_or(0);

        Ok(DatabaseStats {
            total_signals: total_signals as usize,
            total_signals_ever: total_ever as usize,
            signals_by_source: sources.into_iter().map(|(s, c)| (s, c as usize)).collect(),
            avg_confidence,
            high_confidence_count: high_conf_count as usize,
        })
    }

    /// Optimize database (run periodically, e.g., daily)
    pub fn optimize(&self) -> Result<()> {
        let conn = self.conn.lock();
        conn.execute_batch(
            "PRAGMA optimize;
             PRAGMA wal_checkpoint(TRUNCATE);",
        )?;
        info!("🔧 Database optimized");
        Ok(())
    }

    /// Prune raw Dome order events older than `cutoff_ts` (unix seconds).
    pub fn prune_dome_order_events_before(&self, cutoff_ts: i64) -> Result<usize> {
        let conn = self.conn.lock();
        let deleted = conn.execute(
            "DELETE FROM dome_order_events WHERE timestamp < ?1",
            params![cutoff_ts],
        )?;
        Ok(deleted)
    }

    /// Clear all signals (use only for testing)
    pub async fn clear(&self) -> Result<()> {
        let conn = self.conn.lock();
        conn.execute("DELETE FROM signals", [])?;
        conn.execute("DELETE FROM signal_context", [])?;
        conn.execute("DELETE FROM dome_order_events", [])?;
        info!("🗑️  All signals cleared from database");
        Ok(())
    }

    /// Store a raw Dome WebSocket order event payload (lossless)
    pub async fn store_dome_order_event(
        &self,
        order_hash: &str,
        tx_hash: &str,
        user: &str,
        market_slug: &str,
        condition_id: &str,
        token_id: &str,
        timestamp: i64,
        payload_json: &str,
        received_at: i64,
    ) -> Result<()> {
        let conn = self.conn.lock();
        conn.execute(
            "INSERT OR IGNORE INTO dome_order_events \
             (order_hash, tx_hash, user, market_slug, condition_id, token_id, timestamp, payload_json, received_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                order_hash,
                tx_hash,
                user,
                market_slug,
                condition_id,
                token_id,
                timestamp,
                payload_json,
                received_at,
            ],
        )?;
        Ok(())
    }

    /// Fetch recent Dome orders for a wallet from the locally persisted WS event log.
    ///
    /// This is the HFT-grade fast path for wallet analytics (no Dome REST dependency).
    pub fn get_dome_orders_for_wallet(
        &self,
        wallet: &str,
        start_time: i64,
        end_time: i64,
        limit: usize,
    ) -> Result<Vec<DomeOrder>> {
        let wallet_norm = wallet.to_lowercase();
        let conn = self.conn.lock();
        let mut stmt = conn.prepare_cached(
            "SELECT payload_json FROM dome_order_events \
             WHERE lower(user) = ?1 AND timestamp >= ?2 AND timestamp <= ?3 \
             ORDER BY timestamp ASC LIMIT ?4",
        )?;

        let mut rows = stmt.query(params![wallet_norm, start_time, end_time, limit as i64])?;
        let mut out: Vec<DomeOrder> = Vec::new();

        while let Some(row) = rows.next()? {
            let payload_json: String = row.get(0)?;
            match serde_json::from_str::<DomeOrder>(&payload_json) {
                Ok(order) => out.push(order),
                Err(e) => warn!("failed to deserialize dome order payload_json: {}", e),
            }
        }

        Ok(out)
    }

    /// Upsert signal context.
    pub async fn store_signal_context(
        &self,
        signal_id: &str,
        context_version: i64,
        enriched_at: i64,
        status: &str,
        error: Option<&str>,
        context: &SignalContext,
    ) -> Result<()> {
        let context_json =
            serde_json::to_string(context).context("Failed to serialize SignalContext")?;
        let conn = self.conn.lock();
        conn.execute(
            "INSERT INTO signal_context (signal_id, context_version, context_json, enriched_at, status, error) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
             ON CONFLICT(signal_id) DO UPDATE SET \
                context_version=excluded.context_version, \
                context_json=excluded.context_json, \
                enriched_at=excluded.enriched_at, \
                status=excluded.status, \
                error=excluded.error",
            params![signal_id, context_version, context_json, enriched_at, status, error],
        )?;
        Ok(())
    }

    /// Fetch stored signal context.
    pub fn get_signal_context(&self, signal_id: &str) -> Result<Option<SignalContextRecord>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare_cached(
            "SELECT signal_id, context_version, context_json, enriched_at, status, error \
             FROM signal_context WHERE signal_id = ?1",
        )?;

        let mut rows = stmt.query([signal_id])?;
        let Some(row) = rows.next()? else {
            return Ok(None);
        };

        let signal_id: String = row.get(0)?;
        let context_version: i64 = row.get(1)?;
        let context_json: String = row.get(2)?;
        let enriched_at: i64 = row.get(3)?;
        let status: String = row.get(4)?;
        let error: Option<String> = row.get(5)?;

        let context: SignalContext = serde_json::from_str(&context_json)
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;

        Ok(Some(SignalContextRecord {
            signal_id,
            context_version,
            enriched_at,
            status,
            error,
            context,
        }))
    }

    /// Fetch all signal contexts (for joining with signals in REST API)
    pub fn get_all_contexts(
        &self,
        limit: usize,
    ) -> Result<std::collections::HashMap<String, SignalContextRecord>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare_cached(
            "SELECT signal_id, context_version, context_json, enriched_at, status, error \
             FROM signal_context ORDER BY enriched_at DESC LIMIT ?1",
        )?;

        let mut map = std::collections::HashMap::new();
        let mut rows = stmt.query([limit])?;

        while let Some(row) = rows.next()? {
            let signal_id: String = row.get(0)?;
            let context_version: i64 = row.get(1)?;
            let context_json: String = row.get(2)?;
            let enriched_at: i64 = row.get(3)?;
            let status: String = row.get(4)?;
            let error: Option<String> = row.get(5)?;

            if let Ok(context) = serde_json::from_str::<SignalContext>(&context_json) {
                map.insert(
                    signal_id.clone(),
                    SignalContextRecord {
                        signal_id,
                        context_version,
                        enriched_at,
                        status,
                        error,
                        context,
                    },
                );
            }
        }

        Ok(map)
    }

    /// Fetch contexts for a specific set of signals.
    ///
    /// This avoids the ordering/limit mismatch of `get_all_contexts(limit)` when the caller is
    /// joining against a specific list of signals.
    pub fn get_contexts_for_signals(
        &self,
        signal_ids: &[String],
    ) -> Result<std::collections::HashMap<String, SignalContextRecord>> {
        if signal_ids.is_empty() {
            return Ok(std::collections::HashMap::new());
        }

        let conn = self.conn.lock();
        let placeholders = std::iter::repeat("?")
            .take(signal_ids.len())
            .collect::<Vec<_>>()
            .join(",");

        let sql = format!(
            "SELECT signal_id, context_version, context_json, enriched_at, status, error \
             FROM signal_context WHERE signal_id IN ({})",
            placeholders
        );

        let mut stmt = conn.prepare(&sql)?;
        let mut rows = stmt.query(params_from_iter(signal_ids.iter()))?;

        let mut map = std::collections::HashMap::new();
        while let Some(row) = rows.next()? {
            let signal_id: String = row.get(0)?;
            let context_version: i64 = row.get(1)?;
            let context_json: String = row.get(2)?;
            let enriched_at: i64 = row.get(3)?;
            let status: String = row.get(4)?;
            let error: Option<String> = row.get(5)?;

            if let Ok(context) = serde_json::from_str::<SignalContext>(&context_json) {
                map.insert(
                    signal_id.clone(),
                    SignalContextRecord {
                        signal_id,
                        context_version,
                        enriched_at,
                        status,
                        error,
                        context,
                    },
                );
            }
        }

        Ok(map)
    }

    /// Get cached JSON blob by key.
    pub fn get_cache(&self, cache_key: &str) -> Result<Option<(String, i64)>> {
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare_cached("SELECT cache_json, fetched_at FROM dome_cache WHERE cache_key = ?1")?;
        let mut rows = stmt.query([cache_key])?;
        let Some(row) = rows.next()? else {
            return Ok(None);
        };
        let json: String = row.get(0)?;
        let fetched_at: i64 = row.get(1)?;
        Ok(Some((json, fetched_at)))
    }

    /// Upsert cache JSON blob.
    pub fn upsert_cache(&self, cache_key: &str, cache_json: &str, fetched_at: i64) -> Result<()> {
        let conn = self.conn.lock();
        conn.execute(
            "INSERT INTO dome_cache (cache_key, cache_json, fetched_at) VALUES (?1, ?2, ?3) \
             ON CONFLICT(cache_key) DO UPDATE SET cache_json=excluded.cache_json, fetched_at=excluded.fetched_at",
            params![cache_key, cache_json, fetched_at],
        )?;
        Ok(())
    }
}

/// Database statistics
#[derive(Debug, Clone)]
pub struct DatabaseStats {
    pub total_signals: usize,
    pub total_signals_ever: usize,
    pub signals_by_source: Vec<(String, usize)>,
    pub avg_confidence: f64,
    pub high_confidence_count: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{SignalDetails, SignalType};

    fn create_test_signal(id: &str) -> MarketSignal {
        MarketSignal {
            id: id.to_string(),
            signal_type: SignalType::PriceDeviation {
                market_price: 0.55,
                fair_value: 0.50,
                deviation_pct: 10.0,
            },
            market_slug: "test-market".to_string(),
            confidence: 0.85,
            risk_level: "medium".to_string(),
            details: SignalDetails {
                market_id: "test_market_id".to_string(),
                market_title: "Test Market".to_string(),
                current_price: 0.55,
                volume_24h: 10000.0,
                liquidity: 50000.0,
                recommended_action: "BUY".to_string(),
                expiry_time: Some("2025-12-31T23:59:59Z".to_string()),
                observed_timestamp: None,
                signal_family: None,
                calibration_version: None,
                guardrail_flags: None,
                recommended_size: None,
            },
            detected_at: "2025-11-16T12:00:00Z".to_string(),
            source: "test".to_string(),
        }
    }

    fn create_test_signal_at(id: &str, detected_at: &str) -> MarketSignal {
        let mut s = create_test_signal(id);
        s.detected_at = detected_at.to_string();
        s
    }

    #[tokio::test]
    async fn test_db_storage_create() {
        let storage = DbSignalStorage::new(":memory:").expect("Failed to create database");
        assert_eq!(storage.len(), 0);
    }

    #[tokio::test]
    async fn test_db_storage_insert_and_retrieve() {
        let storage = DbSignalStorage::new(":memory:").expect("Failed to create database");

        let signal = create_test_signal("test_1");
        storage.store(&signal).await.expect("Failed to store");

        assert_eq!(storage.len(), 1);

        let retrieved = storage.get_recent(10).expect("Failed to retrieve");
        assert_eq!(retrieved.len(), 1);
        assert_eq!(retrieved[0].id, "test_1");
    }

    #[tokio::test]
    async fn test_db_storage_batch_insert() {
        let storage = DbSignalStorage::new(":memory:").expect("Failed to create database");

        let signals: Vec<_> = (0..100)
            .map(|i| create_test_signal(&format!("test_{}", i)))
            .collect();

        let inserted = storage
            .store_batch(&signals)
            .await
            .expect("Failed to batch insert");
        assert_eq!(inserted, 100);
        assert_eq!(storage.len(), 100);
    }

    #[tokio::test]
    async fn test_total_signals_ever() {
        let storage = DbSignalStorage::new(":memory:").expect("Failed to create database");

        for i in 0..5 {
            let signal = create_test_signal(&format!("test_{}", i));
            storage.store(&signal).await.expect("Failed to store");
        }

        let total = storage
            .get_total_signals_ever()
            .expect("Failed to get total");
        assert_eq!(total, 5);
    }

    #[tokio::test]
    async fn test_get_before_pagination() {
        let storage = DbSignalStorage::new(":memory:").expect("Failed to create database");

        let signals = vec![
            create_test_signal_at("a", "2025-11-16T12:00:00+00:00"),
            create_test_signal_at("b", "2025-11-16T12:00:00+00:00"),
            create_test_signal_at("c", "2025-11-16T11:59:00+00:00"),
            create_test_signal_at("d", "2025-11-16T11:58:00+00:00"),
        ];

        storage
            .store_batch(&signals)
            .await
            .expect("Failed to batch insert");

        let page1 = storage.get_recent(3).expect("Failed to retrieve");
        let page1_ids: Vec<_> = page1.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(page1_ids, vec!["a", "b", "c"]);

        let cursor = page1.last().unwrap();
        let page2 = storage
            .get_before(&cursor.detected_at, Some(&cursor.id), 3)
            .expect("Failed to paginate");
        let page2_ids: Vec<_> = page2.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(page2_ids, vec!["d"]);

        let tie_page = storage
            .get_before("2025-11-16T12:00:00+00:00", Some("a"), 10)
            .expect("Failed to paginate tie");
        let tie_ids: Vec<_> = tie_page.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(tie_ids, vec!["b", "c", "d"]);
    }
}

//! Chainlink Oracle Round Storage
//!
//! Persistent storage for historical Chainlink rounds with:
//! - SQLite backend for reliability
//! - Efficient time-range queries
//! - Arrival-time preservation for visibility semantics

use anyhow::{Context, Result};
use parking_lot::Mutex;
use rusqlite::{params, Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Arc;
use tracing::{debug, info};

use super::chainlink::{ChainlinkFeedConfig, ChainlinkRound};

// =============================================================================
// Configuration
// =============================================================================

/// Storage configuration.
#[derive(Debug, Clone)]
pub struct OracleStorageConfig {
    /// Path to SQLite database file.
    pub db_path: String,
    /// Maximum rounds to keep per feed (0 = unlimited).
    pub max_rounds_per_feed: usize,
    /// Enable WAL mode for better concurrency.
    pub wal_mode: bool,
}

impl Default for OracleStorageConfig {
    fn default() -> Self {
        Self {
            db_path: "chainlink_rounds.db".to_string(),
            max_rounds_per_feed: 0,
            wal_mode: true,
        }
    }
}

impl OracleStorageConfig {
    pub fn from_env() -> Self {
        let mut config = Self::default();

        if let Ok(path) = std::env::var("CHAINLINK_DB_PATH") {
            config.db_path = path;
        }

        if let Ok(max) = std::env::var("CHAINLINK_MAX_ROUNDS_PER_FEED") {
            if let Ok(n) = max.parse() {
                config.max_rounds_per_feed = n;
            }
        }

        config
    }
}

// =============================================================================
// Storage Schema
// =============================================================================

const SCHEMA_SQL: &str = r#"
-- Enable optimizations
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;
PRAGMA cache_size = -16000;
PRAGMA temp_store = MEMORY;

-- Main rounds table
CREATE TABLE IF NOT EXISTS chainlink_rounds (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    feed_id TEXT NOT NULL,
    round_id INTEGER NOT NULL,
    answer INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    answered_in_round INTEGER NOT NULL,
    started_at INTEGER NOT NULL,
    ingest_arrival_time_ns INTEGER NOT NULL,
    ingest_seq INTEGER NOT NULL,
    decimals INTEGER NOT NULL,
    asset_symbol TEXT NOT NULL,
    raw_source_hash TEXT,
    created_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now')),
    UNIQUE(feed_id, round_id)
);

-- Index for time-range queries (most common)
CREATE INDEX IF NOT EXISTS idx_chainlink_rounds_updated_at
    ON chainlink_rounds(feed_id, updated_at);

-- Index for round_id lookups
CREATE INDEX IF NOT EXISTS idx_chainlink_rounds_round_id
    ON chainlink_rounds(feed_id, round_id);

-- Index for asset symbol queries
CREATE INDEX IF NOT EXISTS idx_chainlink_rounds_asset
    ON chainlink_rounds(asset_symbol, updated_at);

-- Index by arrival time (for visibility queries)
CREATE INDEX IF NOT EXISTS idx_chainlink_rounds_arrival
    ON chainlink_rounds(feed_id, ingest_arrival_time_ns);

-- Feed configuration table
CREATE TABLE IF NOT EXISTS chainlink_feeds (
    feed_id TEXT PRIMARY KEY,
    chain_id INTEGER NOT NULL,
    feed_proxy_address TEXT NOT NULL,
    decimals INTEGER NOT NULL,
    asset_symbol TEXT NOT NULL,
    deviation_threshold REAL,
    heartbeat_secs INTEGER,
    created_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now')),
    updated_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now'))
);

-- Backfill state tracking
CREATE TABLE IF NOT EXISTS chainlink_backfill_state (
    feed_id TEXT PRIMARY KEY,
    oldest_round_id INTEGER NOT NULL,
    newest_round_id INTEGER NOT NULL,
    oldest_updated_at INTEGER NOT NULL,
    newest_updated_at INTEGER NOT NULL,
    total_rounds INTEGER NOT NULL,
    last_backfill_at INTEGER NOT NULL,
    is_complete INTEGER NOT NULL DEFAULT 0
);

-- Metadata
CREATE TABLE IF NOT EXISTS chainlink_metadata (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
) WITHOUT ROWID;
"#;

// =============================================================================
// Storage Implementation
// =============================================================================

/// Persistent storage for Chainlink oracle rounds.
pub struct OracleRoundStorage {
    conn: Arc<Mutex<Connection>>,
    config: OracleStorageConfig,
}

impl OracleRoundStorage {
    /// Open or create storage.
    pub fn open(config: OracleStorageConfig) -> Result<Self> {
        let path = Path::new(&config.db_path);

        // Create directory if needed
        if let Some(parent) = path.parent() {
            if !parent.exists() {
                std::fs::create_dir_all(parent)?;
            }
        }

        let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX;

        let conn = Connection::open_with_flags(&config.db_path, flags)
            .with_context(|| format!("Failed to open database: {}", config.db_path))?;

        // Initialize schema
        conn.execute_batch(SCHEMA_SQL)?;

        info!(path = %config.db_path, "Chainlink oracle storage opened");

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            config,
        })
    }

    /// Open in-memory storage (for testing).
    pub fn open_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(SCHEMA_SQL)?;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            config: OracleStorageConfig::default(),
        })
    }

    /// Store a single round (upsert).
    pub fn store_round(&self, round: &ChainlinkRound) -> Result<()> {
        let conn = self.conn.lock();
        conn.execute(
            r#"
            INSERT INTO chainlink_rounds (
                feed_id, round_id, answer, updated_at, answered_in_round,
                started_at, ingest_arrival_time_ns, ingest_seq, decimals,
                asset_symbol, raw_source_hash
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
            ON CONFLICT(feed_id, round_id) DO UPDATE SET
                answer = excluded.answer,
                updated_at = excluded.updated_at,
                ingest_arrival_time_ns = excluded.ingest_arrival_time_ns,
                ingest_seq = excluded.ingest_seq
            "#,
            params![
                round.feed_id,
                round.round_id as i64,
                round.answer as i64,
                round.updated_at as i64,
                round.answered_in_round as i64,
                round.started_at as i64,
                round.ingest_arrival_time_ns as i64,
                round.ingest_seq as i64,
                round.decimals as i32,
                round.asset_symbol,
                round.raw_source_hash,
            ],
        )?;
        Ok(())
    }

    /// Store multiple rounds in a batch.
    pub fn store_rounds(&self, rounds: &[ChainlinkRound]) -> Result<usize> {
        if rounds.is_empty() {
            return Ok(0);
        }

        let conn = self.conn.lock();
        conn.execute("BEGIN IMMEDIATE", [])?;

        let mut count = 0;
        for round in rounds {
            let result = conn.execute(
                r#"
                INSERT INTO chainlink_rounds (
                    feed_id, round_id, answer, updated_at, answered_in_round,
                    started_at, ingest_arrival_time_ns, ingest_seq, decimals,
                    asset_symbol, raw_source_hash
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
                ON CONFLICT(feed_id, round_id) DO NOTHING
                "#,
                params![
                    round.feed_id,
                    round.round_id as i64,
                    round.answer as i64,
                    round.updated_at as i64,
                    round.answered_in_round as i64,
                    round.started_at as i64,
                    round.ingest_arrival_time_ns as i64,
                    round.ingest_seq as i64,
                    round.decimals as i32,
                    round.asset_symbol,
                    round.raw_source_hash,
                ],
            );
            if result.is_ok() {
                count += 1;
            }
        }

        conn.execute("COMMIT", [])?;
        Ok(count)
    }

    /// Load rounds for a feed in a time range (by updated_at).
    pub fn load_rounds_in_range(
        &self,
        feed_id: &str,
        start_ts: u64,
        end_ts: u64,
    ) -> Result<Vec<ChainlinkRound>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            r#"
            SELECT feed_id, round_id, answer, updated_at, answered_in_round,
                   started_at, ingest_arrival_time_ns, ingest_seq, decimals,
                   asset_symbol, raw_source_hash
            FROM chainlink_rounds
            WHERE feed_id = ?1 AND updated_at >= ?2 AND updated_at <= ?3
            ORDER BY updated_at ASC, round_id ASC
            "#,
        )?;

        let rounds = stmt
            .query_map(params![feed_id, start_ts as i64, end_ts as i64], |row| {
                Ok(ChainlinkRound {
                    feed_id: row.get(0)?,
                    round_id: row.get::<_, i64>(1)? as u128,
                    answer: row.get::<_, i64>(2)? as i128,
                    updated_at: row.get::<_, i64>(3)? as u64,
                    answered_in_round: row.get::<_, i64>(4)? as u128,
                    started_at: row.get::<_, i64>(5)? as u64,
                    ingest_arrival_time_ns: row.get::<_, i64>(6)? as u64,
                    ingest_seq: row.get::<_, i64>(7)? as u64,
                    decimals: row.get::<_, i32>(8)? as u8,
                    asset_symbol: row.get(9)?,
                    raw_source_hash: row.get(10)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(rounds)
    }

    /// Load rounds for an asset symbol in a time range.
    pub fn load_rounds_by_asset(
        &self,
        asset: &str,
        start_ts: u64,
        end_ts: u64,
    ) -> Result<Vec<ChainlinkRound>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            r#"
            SELECT feed_id, round_id, answer, updated_at, answered_in_round,
                   started_at, ingest_arrival_time_ns, ingest_seq, decimals,
                   asset_symbol, raw_source_hash
            FROM chainlink_rounds
            WHERE asset_symbol = ?1 AND updated_at >= ?2 AND updated_at <= ?3
            ORDER BY updated_at ASC, round_id ASC
            "#,
        )?;

        let rounds = stmt
            .query_map(
                params![asset.to_uppercase(), start_ts as i64, end_ts as i64],
                |row| {
                    Ok(ChainlinkRound {
                        feed_id: row.get(0)?,
                        round_id: row.get::<_, i64>(1)? as u128,
                        answer: row.get::<_, i64>(2)? as i128,
                        updated_at: row.get::<_, i64>(3)? as u64,
                        answered_in_round: row.get::<_, i64>(4)? as u128,
                        started_at: row.get::<_, i64>(5)? as u64,
                        ingest_arrival_time_ns: row.get::<_, i64>(6)? as u64,
                        ingest_seq: row.get::<_, i64>(7)? as u64,
                        decimals: row.get::<_, i32>(8)? as u8,
                        asset_symbol: row.get(9)?,
                        raw_source_hash: row.get(10)?,
                    })
                },
            )?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(rounds)
    }

    /// Get the latest round for a feed.
    pub fn get_latest_round(&self, feed_id: &str) -> Result<Option<ChainlinkRound>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            r#"
            SELECT feed_id, round_id, answer, updated_at, answered_in_round,
                   started_at, ingest_arrival_time_ns, ingest_seq, decimals,
                   asset_symbol, raw_source_hash
            FROM chainlink_rounds
            WHERE feed_id = ?1
            ORDER BY round_id DESC
            LIMIT 1
            "#,
        )?;

        let round = stmt
            .query_row(params![feed_id], |row| {
                Ok(ChainlinkRound {
                    feed_id: row.get(0)?,
                    round_id: row.get::<_, i64>(1)? as u128,
                    answer: row.get::<_, i64>(2)? as i128,
                    updated_at: row.get::<_, i64>(3)? as u64,
                    answered_in_round: row.get::<_, i64>(4)? as u128,
                    started_at: row.get::<_, i64>(5)? as u64,
                    ingest_arrival_time_ns: row.get::<_, i64>(6)? as u64,
                    ingest_seq: row.get::<_, i64>(7)? as u64,
                    decimals: row.get::<_, i32>(8)? as u8,
                    asset_symbol: row.get(9)?,
                    raw_source_hash: row.get(10)?,
                })
            })
            .ok();

        Ok(round)
    }

    /// Get the round at or before a timestamp (for settlement).
    pub fn get_round_at_or_before(
        &self,
        feed_id: &str,
        target_ts: u64,
    ) -> Result<Option<ChainlinkRound>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            r#"
            SELECT feed_id, round_id, answer, updated_at, answered_in_round,
                   started_at, ingest_arrival_time_ns, ingest_seq, decimals,
                   asset_symbol, raw_source_hash
            FROM chainlink_rounds
            WHERE feed_id = ?1 AND updated_at <= ?2
            ORDER BY updated_at DESC, round_id DESC
            LIMIT 1
            "#,
        )?;

        let round = stmt
            .query_row(params![feed_id, target_ts as i64], |row| {
                Ok(ChainlinkRound {
                    feed_id: row.get(0)?,
                    round_id: row.get::<_, i64>(1)? as u128,
                    answer: row.get::<_, i64>(2)? as i128,
                    updated_at: row.get::<_, i64>(3)? as u64,
                    answered_in_round: row.get::<_, i64>(4)? as u128,
                    started_at: row.get::<_, i64>(5)? as u64,
                    ingest_arrival_time_ns: row.get::<_, i64>(6)? as u64,
                    ingest_seq: row.get::<_, i64>(7)? as u64,
                    decimals: row.get::<_, i32>(8)? as u8,
                    asset_symbol: row.get(9)?,
                    raw_source_hash: row.get(10)?,
                })
            })
            .ok();

        Ok(round)
    }

    /// Get the first round after a timestamp.
    pub fn get_round_first_after(
        &self,
        feed_id: &str,
        target_ts: u64,
    ) -> Result<Option<ChainlinkRound>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            r#"
            SELECT feed_id, round_id, answer, updated_at, answered_in_round,
                   started_at, ingest_arrival_time_ns, ingest_seq, decimals,
                   asset_symbol, raw_source_hash
            FROM chainlink_rounds
            WHERE feed_id = ?1 AND updated_at > ?2
            ORDER BY updated_at ASC, round_id ASC
            LIMIT 1
            "#,
        )?;

        let round = stmt
            .query_row(params![feed_id, target_ts as i64], |row| {
                Ok(ChainlinkRound {
                    feed_id: row.get(0)?,
                    round_id: row.get::<_, i64>(1)? as u128,
                    answer: row.get::<_, i64>(2)? as i128,
                    updated_at: row.get::<_, i64>(3)? as u64,
                    answered_in_round: row.get::<_, i64>(4)? as u128,
                    started_at: row.get::<_, i64>(5)? as u64,
                    ingest_arrival_time_ns: row.get::<_, i64>(6)? as u64,
                    ingest_seq: row.get::<_, i64>(7)? as u64,
                    decimals: row.get::<_, i32>(8)? as u8,
                    asset_symbol: row.get(9)?,
                    raw_source_hash: row.get(10)?,
                })
            })
            .ok();

        Ok(round)
    }

    /// Store feed configuration.
    pub fn store_feed_config(&self, config: &ChainlinkFeedConfig) -> Result<()> {
        let conn = self.conn.lock();
        conn.execute(
            r#"
            INSERT INTO chainlink_feeds (
                feed_id, chain_id, feed_proxy_address, decimals, asset_symbol,
                deviation_threshold, heartbeat_secs
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            ON CONFLICT(feed_id) DO UPDATE SET
                chain_id = excluded.chain_id,
                feed_proxy_address = excluded.feed_proxy_address,
                decimals = excluded.decimals,
                asset_symbol = excluded.asset_symbol,
                deviation_threshold = excluded.deviation_threshold,
                heartbeat_secs = excluded.heartbeat_secs,
                updated_at = strftime('%s', 'now')
            "#,
            params![
                config.feed_id(),
                config.chain_id as i64,
                config.feed_proxy_address,
                config.decimals as i32,
                config.asset_symbol,
                config.deviation_threshold,
                config.heartbeat_secs as i64,
            ],
        )?;
        Ok(())
    }

    /// Update backfill state.
    pub fn update_backfill_state(
        &self,
        feed_id: &str,
        oldest_round_id: u128,
        newest_round_id: u128,
        oldest_updated_at: u64,
        newest_updated_at: u64,
        total_rounds: usize,
        is_complete: bool,
    ) -> Result<()> {
        let conn = self.conn.lock();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        conn.execute(
            r#"
            INSERT INTO chainlink_backfill_state (
                feed_id, oldest_round_id, newest_round_id, oldest_updated_at,
                newest_updated_at, total_rounds, last_backfill_at, is_complete
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
            ON CONFLICT(feed_id) DO UPDATE SET
                oldest_round_id = excluded.oldest_round_id,
                newest_round_id = excluded.newest_round_id,
                oldest_updated_at = excluded.oldest_updated_at,
                newest_updated_at = excluded.newest_updated_at,
                total_rounds = excluded.total_rounds,
                last_backfill_at = excluded.last_backfill_at,
                is_complete = excluded.is_complete
            "#,
            params![
                feed_id,
                oldest_round_id as i64,
                newest_round_id as i64,
                oldest_updated_at as i64,
                newest_updated_at as i64,
                total_rounds as i64,
                now as i64,
                is_complete as i32,
            ],
        )?;
        Ok(())
    }

    /// Get backfill state.
    pub fn get_backfill_state(&self, feed_id: &str) -> Result<Option<BackfillState>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            r#"
            SELECT oldest_round_id, newest_round_id, oldest_updated_at,
                   newest_updated_at, total_rounds, last_backfill_at, is_complete
            FROM chainlink_backfill_state
            WHERE feed_id = ?1
            "#,
        )?;

        let state = stmt
            .query_row(params![feed_id], |row| {
                Ok(BackfillState {
                    feed_id: feed_id.to_string(),
                    oldest_round_id: row.get::<_, i64>(0)? as u128,
                    newest_round_id: row.get::<_, i64>(1)? as u128,
                    oldest_updated_at: row.get::<_, i64>(2)? as u64,
                    newest_updated_at: row.get::<_, i64>(3)? as u64,
                    total_rounds: row.get::<_, i64>(4)? as usize,
                    last_backfill_at: row.get::<_, i64>(5)? as u64,
                    is_complete: row.get::<_, i32>(6)? != 0,
                })
            })
            .ok();

        Ok(state)
    }

    /// Get total round count for a feed.
    pub fn count_rounds(&self, feed_id: &str) -> Result<usize> {
        let conn = self.conn.lock();
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM chainlink_rounds WHERE feed_id = ?1",
            params![feed_id],
            |row| row.get(0),
        )?;
        Ok(count as usize)
    }

    /// Get time coverage for a feed.
    pub fn get_time_coverage(&self, feed_id: &str) -> Result<Option<(u64, u64)>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            r#"
            SELECT MIN(updated_at), MAX(updated_at)
            FROM chainlink_rounds
            WHERE feed_id = ?1
            "#,
        )?;

        let result = stmt
            .query_row(params![feed_id], |row| {
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

    /// Prune old rounds if exceeding max.
    pub fn prune_old_rounds(&self, feed_id: &str, keep_count: usize) -> Result<usize> {
        if keep_count == 0 {
            return Ok(0);
        }

        let conn = self.conn.lock();
        let deleted = conn.execute(
            r#"
            DELETE FROM chainlink_rounds
            WHERE feed_id = ?1 AND id NOT IN (
                SELECT id FROM chainlink_rounds
                WHERE feed_id = ?1
                ORDER BY round_id DESC
                LIMIT ?2
            )
            "#,
            params![feed_id, keep_count as i64],
        )?;

        Ok(deleted)
    }
}

/// Backfill state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackfillState {
    pub feed_id: String,
    pub oldest_round_id: u128,
    pub newest_round_id: u128,
    pub oldest_updated_at: u64,
    pub newest_updated_at: u64,
    pub total_rounds: usize,
    pub last_backfill_at: u64,
    pub is_complete: bool,
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_round(round_id: u128, updated_at: u64, answer: i128) -> ChainlinkRound {
        ChainlinkRound {
            feed_id: "test_feed".to_string(),
            round_id,
            answer,
            updated_at,
            answered_in_round: round_id,
            started_at: updated_at,
            ingest_arrival_time_ns: updated_at * 1_000_000_000,
            ingest_seq: round_id as u64,
            decimals: 8,
            asset_symbol: "BTC".to_string(),
            raw_source_hash: None,
        }
    }

    #[test]
    fn test_storage_basic() {
        let storage = OracleRoundStorage::open_memory().unwrap();

        let round = make_test_round(1, 1000, 5000000000000);
        storage.store_round(&round).unwrap();

        let loaded = storage.get_latest_round("test_feed").unwrap().unwrap();
        assert_eq!(loaded.round_id, 1);
        assert_eq!(loaded.answer, 5000000000000);
    }

    #[test]
    fn test_storage_batch() {
        let storage = OracleRoundStorage::open_memory().unwrap();

        let rounds = vec![
            make_test_round(1, 1000, 5000000000000),
            make_test_round(2, 1100, 5010000000000),
            make_test_round(3, 1200, 5020000000000),
        ];

        let stored = storage.store_rounds(&rounds).unwrap();
        assert_eq!(stored, 3);

        let count = storage.count_rounds("test_feed").unwrap();
        assert_eq!(count, 3);
    }

    #[test]
    fn test_storage_round_at_or_before() {
        let storage = OracleRoundStorage::open_memory().unwrap();

        let rounds = vec![
            make_test_round(1, 1000, 5000000000000),
            make_test_round(2, 1100, 5010000000000),
            make_test_round(3, 1200, 5020000000000),
        ];
        storage.store_rounds(&rounds).unwrap();

        // Before first
        let r = storage.get_round_at_or_before("test_feed", 999).unwrap();
        assert!(r.is_none());

        // Exactly at first
        let r = storage
            .get_round_at_or_before("test_feed", 1000)
            .unwrap()
            .unwrap();
        assert_eq!(r.round_id, 1);

        // Between rounds
        let r = storage
            .get_round_at_or_before("test_feed", 1050)
            .unwrap()
            .unwrap();
        assert_eq!(r.round_id, 1);

        // At second
        let r = storage
            .get_round_at_or_before("test_feed", 1100)
            .unwrap()
            .unwrap();
        assert_eq!(r.round_id, 2);

        // After all
        let r = storage
            .get_round_at_or_before("test_feed", 2000)
            .unwrap()
            .unwrap();
        assert_eq!(r.round_id, 3);
    }

    #[test]
    fn test_storage_round_first_after() {
        let storage = OracleRoundStorage::open_memory().unwrap();

        let rounds = vec![
            make_test_round(1, 1000, 5000000000000),
            make_test_round(2, 1100, 5010000000000),
        ];
        storage.store_rounds(&rounds).unwrap();

        // Before first
        let r = storage
            .get_round_first_after("test_feed", 500)
            .unwrap()
            .unwrap();
        assert_eq!(r.round_id, 1);

        // After first, before second
        let r = storage
            .get_round_first_after("test_feed", 1000)
            .unwrap()
            .unwrap();
        assert_eq!(r.round_id, 2);

        // After all
        let r = storage.get_round_first_after("test_feed", 1100).unwrap();
        assert!(r.is_none());
    }

    #[test]
    fn test_storage_time_range() {
        let storage = OracleRoundStorage::open_memory().unwrap();

        let rounds = vec![
            make_test_round(1, 1000, 5000000000000),
            make_test_round(2, 1100, 5010000000000),
            make_test_round(3, 1200, 5020000000000),
            make_test_round(4, 1300, 5030000000000),
        ];
        storage.store_rounds(&rounds).unwrap();

        let loaded = storage
            .load_rounds_in_range("test_feed", 1050, 1250)
            .unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].round_id, 2);
        assert_eq!(loaded[1].round_id, 3);
    }

    #[test]
    fn test_storage_time_coverage() {
        let storage = OracleRoundStorage::open_memory().unwrap();

        let rounds = vec![
            make_test_round(1, 1000, 5000000000000),
            make_test_round(2, 1100, 5010000000000),
            make_test_round(3, 1200, 5020000000000),
        ];
        storage.store_rounds(&rounds).unwrap();

        let (min, max) = storage.get_time_coverage("test_feed").unwrap().unwrap();
        assert_eq!(min, 1000);
        assert_eq!(max, 1200);
    }
}

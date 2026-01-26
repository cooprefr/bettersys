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
    models::{MarketSignal, SignalContext, SignalContextRecord, SignalType},
    scrapers::dome_rest::DomeOrder,
};
use anyhow::{Context, Result};
use parking_lot::Mutex; // Faster than std::sync::Mutex
use rusqlite::{params, params_from_iter, Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::{debug, info, warn};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultLlmDecisionRow {
    pub decision_id: String,
    pub market_slug: String,
    pub created_at: i64,
    pub action: String,
    pub outcome_index: Option<i64>,
    pub outcome_text: Option<String>,
    pub p_true: Option<f64>,
    pub bid: Option<f64>,
    pub ask: Option<f64>,
    pub p_eff: Option<f64>,
    pub edge: Option<f64>,
    pub size_mult: Option<f64>,
    pub consensus_models: Option<String>,
    pub flags: Option<String>,
    pub rationale_hash: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultLlmModelRecordRow {
    pub id: String,
    pub decision_id: String,
    pub model: String,
    pub created_at: i64,
    pub parsed_ok: bool,
    pub action: Option<String>,
    pub outcome_index: Option<i64>,
    pub p_true: Option<f64>,
    pub uncertainty: Option<String>,
    pub size_mult: Option<f64>,
    pub flags: Option<String>,
    pub rationale_hash: Option<String>,
    pub raw_dsl: Option<String>,
    pub latency_ms: Option<i64>,
    pub prompt_tokens: Option<i64>,
    pub completion_tokens: Option<i64>,
    pub total_tokens: Option<i64>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultLlmUsageStats {
    pub day_start_ts: i64,
    pub calls_today: u32,
    pub tokens_today: u64,
    pub per_market_calls_today: Vec<(String, u32)>,
}

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

-- Full-text search index (FTS5) for robust market lookup
CREATE TABLE IF NOT EXISTS signal_search (
    signal_id TEXT NOT NULL UNIQUE,
    detected_at TEXT NOT NULL,
    market_slug TEXT NOT NULL,
    market_title TEXT,
    market_question TEXT,
    order_title TEXT,
    wallet_address TEXT,
    wallet_label TEXT,
    token_label TEXT,
    source TEXT,
    signal_type TEXT,
    updated_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now'))
);

CREATE INDEX IF NOT EXISTS idx_signal_search_detected_at
    ON signal_search(detected_at DESC);

CREATE VIRTUAL TABLE IF NOT EXISTS signal_search_fts USING fts5(
    market_slug,
    market_title,
    market_question,
    order_title,
    wallet_address,
    wallet_label,
    token_label,
    source,
    signal_type,
    content='signal_search',
    content_rowid='rowid',
    tokenize='unicode61 remove_diacritics 2'
);

CREATE TRIGGER IF NOT EXISTS signal_search_ai AFTER INSERT ON signal_search BEGIN
    INSERT INTO signal_search_fts(
        rowid,
        market_slug,
        market_title,
        market_question,
        order_title,
        wallet_address,
        wallet_label,
        token_label,
        source,
        signal_type
    ) VALUES (
        new.rowid,
        new.market_slug,
        new.market_title,
        new.market_question,
        new.order_title,
        new.wallet_address,
        new.wallet_label,
        new.token_label,
        new.source,
        new.signal_type
    );
END;

CREATE TRIGGER IF NOT EXISTS signal_search_ad AFTER DELETE ON signal_search BEGIN
    INSERT INTO signal_search_fts(
        signal_search_fts,
        rowid,
        market_slug,
        market_title,
        market_question,
        order_title,
        wallet_address,
        wallet_label,
        token_label,
        source,
        signal_type
    ) VALUES (
        'delete',
        old.rowid,
        old.market_slug,
        old.market_title,
        old.market_question,
        old.order_title,
        old.wallet_address,
        old.wallet_label,
        old.token_label,
        old.source,
        old.signal_type
    );
END;

CREATE TRIGGER IF NOT EXISTS signal_search_au AFTER UPDATE ON signal_search BEGIN
    INSERT INTO signal_search_fts(
        signal_search_fts,
        rowid,
        market_slug,
        market_title,
        market_question,
        order_title,
        wallet_address,
        wallet_label,
        token_label,
        source,
        signal_type
    ) VALUES (
        'delete',
        old.rowid,
        old.market_slug,
        old.market_title,
        old.market_question,
        old.order_title,
        old.wallet_address,
        old.wallet_label,
        old.token_label,
        old.source,
        old.signal_type
    );
    INSERT INTO signal_search_fts(
        rowid,
        market_slug,
        market_title,
        market_question,
        order_title,
        wallet_address,
        wallet_label,
        token_label,
        source,
        signal_type
    ) VALUES (
        new.rowid,
        new.market_slug,
        new.market_title,
        new.market_question,
        new.order_title,
        new.wallet_address,
        new.wallet_label,
        new.token_label,
        new.source,
        new.signal_type
    );
END;

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

-- Up/Down 15m settlement history (oracle/binance start/end + outcomes)
CREATE TABLE IF NOT EXISTS updown_15m_windows (
    market_slug TEXT PRIMARY KEY,
    asset TEXT NOT NULL,
    window_start_ts INTEGER NOT NULL,
    window_end_ts INTEGER NOT NULL,
    chainlink_start REAL,
    chainlink_end REAL,
    binance_start REAL,
    binance_end REAL,
    chainlink_outcome INTEGER,
    binance_outcome INTEGER,
    agreed INTEGER,
    divergence_usd REAL,
    divergence_bps REAL,
    recorded_at INTEGER NOT NULL
) WITHOUT ROWID;

CREATE INDEX IF NOT EXISTS idx_updown_15m_windows_asset_end
    ON updown_15m_windows(asset, window_end_ts DESC);

CREATE INDEX IF NOT EXISTS idx_updown_15m_windows_end
    ON updown_15m_windows(window_end_ts DESC);

-- Vault LONG engine: bounded LLM decision logs (small, auditable)
CREATE TABLE IF NOT EXISTS vault_llm_decisions (
    decision_id TEXT PRIMARY KEY,
    market_slug TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    action TEXT NOT NULL,
    outcome_index INTEGER,
    outcome_text TEXT,
    p_true REAL,
    bid REAL,
    ask REAL,
    p_eff REAL,
    edge REAL,
    size_mult REAL,
    consensus_models TEXT,
    flags TEXT,
    rationale_hash TEXT
) WITHOUT ROWID;

CREATE INDEX IF NOT EXISTS idx_vault_llm_decisions_market_created
    ON vault_llm_decisions(market_slug, created_at DESC);

CREATE TABLE IF NOT EXISTS vault_llm_model_records (
    id TEXT PRIMARY KEY,
    decision_id TEXT NOT NULL,
    model TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    parsed_ok INTEGER NOT NULL,
    action TEXT,
    outcome_index INTEGER,
    p_true REAL,
    uncertainty TEXT,
    size_mult REAL,
    flags TEXT,
    rationale_hash TEXT,
    raw_dsl TEXT,
    latency_ms INTEGER,
    prompt_tokens INTEGER,
    completion_tokens INTEGER,
    total_tokens INTEGER,
    error TEXT
) WITHOUT ROWID;

CREATE INDEX IF NOT EXISTS idx_vault_llm_model_records_decision
    ON vault_llm_model_records(decision_id);
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

        // Search index backfill state (best-effort).
        conn.execute(
            "INSERT OR IGNORE INTO metadata (key, value) VALUES ('search_backfill_done', '0')",
            [],
        )
        .ok();

        // Warm the search index with a small recent window so search works immediately.
        // Full backfill runs in the background.
        let indexed: i64 = conn
            .query_row("SELECT COUNT(*) FROM signal_search", [], |row| row.get(0))
            .unwrap_or(0);

        if indexed == 0 && count > 0 {
            const WARM_LIMIT: usize = 2_000;

            let warm_limit = (count as usize).min(WARM_LIMIT);
            if warm_limit > 0 {
                let mut stmt = conn.prepare_cached(
                    "SELECT id, signal_type, market_slug, confidence, risk_level, \
                            details_json, detected_at, source \
                     FROM signals \
                     ORDER BY detected_at DESC, id \
                     LIMIT ?1",
                )?;

                let warm_signals: Vec<MarketSignal> = stmt
                    .query_map([warm_limit], Self::row_to_signal)?
                    .filter_map(|r| r.ok())
                    .collect();

                if !warm_signals.is_empty() {
                    conn.execute("BEGIN IMMEDIATE", [])?;
                    for s in &warm_signals {
                        Self::upsert_signal_search_row(&conn, s)?;
                    }

                    if let Some(last) = warm_signals.last() {
                        let _ = Self::set_metadata(
                            &conn,
                            "search_backfill_cursor_detected_at",
                            &last.detected_at,
                        );
                        let _ = Self::set_metadata(&conn, "search_backfill_cursor_id", &last.id);
                    }

                    conn.execute("COMMIT", [])?;
                    info!(
                        "🔎 Search index warm-up: indexed {} recent signals",
                        warm_signals.len()
                    );
                }
            }
        }

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    #[inline]
    fn get_metadata(conn: &Connection, key: &str) -> Option<String> {
        let value: String = conn
            .query_row(
                "SELECT value FROM metadata WHERE key = ?1 LIMIT 1",
                [key],
                |row| row.get(0),
            )
            .ok()?;

        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    }

    #[inline]
    fn set_metadata(conn: &Connection, key: &str, value: &str) -> Result<()> {
        conn.execute(
            "INSERT INTO metadata (key, value) VALUES (?1, ?2) \
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn get_metadata_value(&self, key: &str) -> Result<Option<String>> {
        let conn = self.conn.lock();
        Ok(Self::get_metadata(&conn, key))
    }

    pub fn set_metadata_value(&self, key: &str, value: &str) -> Result<()> {
        let conn = self.conn.lock();
        Self::set_metadata(&conn, key, value)
    }

    fn extract_quoted_market_title(s: &str) -> Option<String> {
        let s = s.trim();
        if s.is_empty() {
            return None;
        }

        // Support strings like: "TRACKED WALLET ENTRY: ... on 'Market Title' by 0x..."
        let lower = s.to_ascii_lowercase();
        for (needle, quote) in [(" on '", '\''), (" on \"", '"')] {
            if let Some(pos) = lower.find(needle) {
                let start = pos + needle.len();
                if start >= s.len() {
                    continue;
                }
                let rest = &s[start..];
                if let Some(end) = rest.find(quote) {
                    let extracted = rest[..end].trim();
                    if !extracted.is_empty() {
                        return Some(extracted.to_string());
                    }
                }
            }
        }

        None
    }

    fn normalized_market_title_for_search(signal: &MarketSignal) -> Option<String> {
        let raw = signal.details.market_title.trim();
        if raw.is_empty() {
            return None;
        }

        if matches!(signal.signal_type, SignalType::TrackedWalletEntry { .. }) {
            let lower = raw.to_ascii_lowercase();
            let looks_like_headline = lower.starts_with("tracked wallet entry")
                || lower.starts_with("insider entry")
                || lower.starts_with("world class trader entry");
            if looks_like_headline {
                if let Some(extracted) = Self::extract_quoted_market_title(raw) {
                    return Some(extracted);
                }
            }
        }

        Some(raw.to_string())
    }

    #[inline]
    fn signal_type_name(st: &SignalType) -> &'static str {
        match st {
            SignalType::PriceDeviation { .. } => "PriceDeviation",
            SignalType::MarketExpiryEdge { .. } => "MarketExpiryEdge",
            SignalType::WhaleFollowing { .. } => "WhaleFollowing",
            SignalType::EliteWallet { .. } => "EliteWallet",
            SignalType::InsiderWallet { .. } => "InsiderWallet",
            SignalType::WhaleCluster { .. } => "WhaleCluster",
            SignalType::CrossPlatformArbitrage { .. } => "CrossPlatformArbitrage",
            SignalType::TrackedWalletEntry { .. } => "TrackedWalletEntry",
        }
    }

    #[inline]
    fn wallet_fields(st: &SignalType) -> (Option<String>, Option<String>, Option<String>) {
        match st {
            SignalType::TrackedWalletEntry {
                wallet_address,
                wallet_label,
                token_label,
                ..
            } => (
                Some(wallet_address.clone()),
                Some(wallet_label.clone()),
                token_label.clone(),
            ),
            SignalType::WhaleFollowing { whale_address, .. } => {
                (Some(whale_address.clone()), None, None)
            }
            SignalType::EliteWallet { wallet_address, .. } => {
                (Some(wallet_address.clone()), None, None)
            }
            SignalType::InsiderWallet { wallet_address, .. } => {
                (Some(wallet_address.clone()), None, None)
            }
            _ => (None, None, None),
        }
    }

    fn upsert_signal_search_row(conn: &Connection, signal: &MarketSignal) -> Result<()> {
        let market_title = Self::normalized_market_title_for_search(signal);
        let (wallet_address, wallet_label, token_label) = Self::wallet_fields(&signal.signal_type);
        let signal_type = Self::signal_type_name(&signal.signal_type);

        conn.execute(
            "INSERT INTO signal_search (
                signal_id,
                detected_at,
                market_slug,
                market_title,
                wallet_address,
                wallet_label,
                token_label,
                source,
                signal_type,
                updated_at
             ) VALUES (
                ?1,
                ?2,
                ?3,
                ?4,
                ?5,
                ?6,
                ?7,
                ?8,
                ?9,
                strftime('%s','now')
             )
             ON CONFLICT(signal_id) DO UPDATE SET
                detected_at=excluded.detected_at,
                market_slug=excluded.market_slug,
                market_title=COALESCE(excluded.market_title, market_title),
                wallet_address=COALESCE(excluded.wallet_address, wallet_address),
                wallet_label=COALESCE(excluded.wallet_label, wallet_label),
                token_label=COALESCE(excluded.token_label, token_label),
                source=excluded.source,
                signal_type=excluded.signal_type,
                updated_at=excluded.updated_at",
            params![
                signal.id,
                signal.detected_at,
                signal.market_slug,
                market_title,
                wallet_address,
                wallet_label,
                token_label,
                signal.source,
                signal_type,
            ],
        )?;

        Ok(())
    }

    fn get_signal_locked(conn: &Connection, signal_id: &str) -> Result<Option<MarketSignal>> {
        let mut stmt = conn.prepare_cached(
            "SELECT id, signal_type, market_slug, confidence, risk_level,
                    details_json, detected_at, source
             FROM signals
             WHERE id = ?1
             LIMIT 1",
        )?;

        let mut rows = stmt.query([signal_id])?;
        if let Some(row) = rows.next()? {
            Ok(Some(Self::row_to_signal(row)?))
        } else {
            Ok(None)
        }
    }

    fn update_signal_search_from_context(
        conn: &Connection,
        signal_id: &str,
        context: &SignalContext,
    ) -> Result<()> {
        let order_title = {
            let s = context.order.title.trim();
            if s.is_empty() {
                None
            } else {
                Some(s.to_string())
            }
        };

        let market_question: Option<String> = context
            .market
            .as_ref()
            .and_then(|m| m.get("question").and_then(|v| v.as_str()))
            .or_else(|| {
                context
                    .market
                    .as_ref()
                    .and_then(|m| m.get("title").and_then(|v| v.as_str()))
            })
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        let token_label = context
            .order
            .token_label
            .as_ref()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let wallet_address = {
            let s = context.order.user.trim();
            if s.is_empty() {
                None
            } else {
                Some(s.to_string())
            }
        };

        let market_slug = {
            let s = context.order.market_slug.trim();
            if s.is_empty() {
                None
            } else {
                Some(s.to_string())
            }
        };

        let changed = conn.execute(
            "UPDATE signal_search SET
                order_title=COALESCE(?1, order_title),
                market_question=COALESCE(?2, market_question),
                token_label=COALESCE(?3, token_label),
                wallet_address=COALESCE(?4, wallet_address),
                market_slug=COALESCE(?5, market_slug),
                updated_at=strftime('%s','now')
             WHERE signal_id = ?6",
            params![
                order_title,
                market_question,
                token_label,
                wallet_address,
                market_slug,
                signal_id
            ],
        )?;

        if changed == 0 {
            if let Some(signal) = Self::get_signal_locked(conn, signal_id)? {
                Self::upsert_signal_search_row(conn, &signal)?;
                let _ = conn.execute(
                    "UPDATE signal_search SET
                        order_title=COALESCE(?1, order_title),
                        market_question=COALESCE(?2, market_question),
                        token_label=COALESCE(?3, token_label),
                        wallet_address=COALESCE(?4, wallet_address),
                        market_slug=COALESCE(?5, market_slug),
                        updated_at=strftime('%s','now')
                     WHERE signal_id = ?6",
                    params![
                        order_title,
                        market_question,
                        token_label,
                        wallet_address,
                        market_slug,
                        signal_id
                    ],
                );
            }
        }

        Ok(())
    }

    /// Store a signal with optimized single-row insert
    #[inline]
    pub async fn store(&self, signal: &MarketSignal) -> Result<()> {
        let start = std::time::Instant::now();

        // Pre-serialize outside the lock
        let details_json = serde_json::to_string(&signal.details)?;
        let signal_type_json = serde_json::to_string(&signal.signal_type)?;

        // Track serialization cost
        let serialize_us = start.elapsed().as_micros() as u64;
        crate::latency::global_comprehensive()
            .serialization
            .record_encode(
                "signal",
                serialize_us,
                (details_json.len() + signal_type_json.len()) as u64,
                false,
            );

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

            if let Err(e) = Self::upsert_signal_search_row(&conn, signal) {
                warn!(
                    "failed to upsert signal_search row for {}: {}",
                    signal.id, e
                );
            }
        }

        // Record DB write latency
        let write_us = start.elapsed().as_micros() as u64;
        crate::latency::global_registry().record_span(crate::latency::LatencySpan::new(
            crate::latency::SpanType::DbWrite,
            write_us,
        ));
        crate::latency::global_comprehensive()
            .throughput
            .db_writes
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        // Record to CPU profiler for hot path tracking
        crate::performance::global_profiler()
            .cpu
            .record_span("db_signal_write", write_us);

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

            if changes > 0 {
                if let Err(e) = Self::upsert_signal_search_row(&conn, signal) {
                    warn!(
                        "failed to upsert signal_search row for {}: {}",
                        signal.id, e
                    );
                }
            }
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
        let start = std::time::Instant::now();
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

        // Record DB read latency
        crate::latency::global_registry().record_span(crate::latency::LatencySpan::new(
            crate::latency::SpanType::DbRead,
            start.elapsed().as_micros() as u64,
        ));
        crate::latency::global_comprehensive()
            .throughput
            .db_reads
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

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

    /// Fetch a single signal by id.
    #[inline]
    pub fn get_signal(&self, signal_id: &str) -> Result<Option<MarketSignal>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare_cached(
            "SELECT id, signal_type, market_slug, confidence, risk_level,
                    details_json, detected_at, source
             FROM signals
             WHERE id = ?1
             LIMIT 1",
        )?;

        let mut rows = stmt.query([signal_id])?;
        if let Some(row) = rows.next()? {
            Ok(Some(Self::row_to_signal(row)?))
        } else {
            Ok(None)
        }
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

    /// Full-text search over signals (FTS5-backed).
    ///
    /// Pagination matches `get_before`: ordering is (detected_at DESC, id ASC).
    pub fn search_signals_fts(
        &self,
        fts_query: &str,
        before_detected_at: Option<&str>,
        before_id: Option<&str>,
        limit: usize,
        exclude_updown: bool,
        min_confidence: Option<f64>,
    ) -> Result<Vec<MarketSignal>> {
        let q = fts_query.trim();
        if q.is_empty() {
            return Ok(Vec::new());
        }

        let conn = self.conn.lock();
        let exclude_flag: i64 = if exclude_updown { 1 } else { 0 };

        // NOTE: `before_id` is optional; when absent we must NOT include the tie-break clause.
        let mut stmt = conn.prepare_cached(
            "SELECT s.id, s.signal_type, s.market_slug, s.confidence, s.risk_level,
                    s.details_json, s.detected_at, s.source
             FROM signal_search_fts
             JOIN signal_search ss ON ss.rowid = signal_search_fts.rowid
             JOIN signals s ON s.id = ss.signal_id
             WHERE signal_search_fts MATCH ?1
               AND (?2 IS NULL OR s.confidence >= ?2)
               AND (?3 = 0 OR (s.market_slug NOT LIKE '%updown%' AND s.market_slug NOT LIKE '%up-or-down%' AND s.market_slug NOT LIKE '%up-down%'))
               AND (
                 ?4 IS NULL
                 OR s.detected_at < ?4
                 OR (s.detected_at = ?4 AND (?5 IS NOT NULL AND s.id > ?5))
               )
             ORDER BY s.detected_at DESC, s.id
             LIMIT ?6",
        )?;

        let signals: Vec<MarketSignal> = stmt
            .query_map(
                params![
                    q,
                    min_confidence,
                    exclude_flag,
                    before_detected_at,
                    before_id,
                    limit
                ],
                Self::row_to_signal,
            )?
            .filter_map(|r| r.ok())
            .collect();

        Ok(signals)
    }

    /// Ensure the search index has at least a small recent warm window indexed.
    ///
    /// This is a best-effort safety net so search doesn't appear "dead" before the
    /// background backfill finishes (or if the process was restarted mid-backfill).
    pub fn ensure_search_warm(&self, warm_limit: usize) -> Result<usize> {
        let warm_limit = warm_limit.clamp(1, 5_000);
        let conn = self.conn.lock();

        let indexed: i64 =
            conn.query_row("SELECT COUNT(*) FROM signal_search", [], |row| row.get(0))?;
        if indexed > 0 {
            return Ok(0);
        }

        let total: i64 = conn
            .query_row("SELECT COUNT(*) FROM signals", [], |row| row.get(0))
            .unwrap_or(0);
        if total <= 0 {
            return Ok(0);
        }

        let limit = (total as usize).min(warm_limit);
        let mut stmt = conn.prepare_cached(
            "SELECT id, signal_type, market_slug, confidence, risk_level, \
                    details_json, detected_at, source \
             FROM signals \
             ORDER BY detected_at DESC, id \
             LIMIT ?1",
        )?;

        let warm_signals: Vec<MarketSignal> = stmt
            .query_map([limit], Self::row_to_signal)?
            .filter_map(|r| r.ok())
            .collect();

        if warm_signals.is_empty() {
            return Ok(0);
        }

        conn.execute("BEGIN IMMEDIATE", [])?;
        for s in &warm_signals {
            Self::upsert_signal_search_row(&conn, s)?;
        }
        if let Some(last) = warm_signals.last() {
            let _ = Self::set_metadata(
                &conn,
                "search_backfill_cursor_detected_at",
                &last.detected_at,
            );
            let _ = Self::set_metadata(&conn, "search_backfill_cursor_id", &last.id);
        }
        conn.execute("COMMIT", [])?;

        Ok(warm_signals.len())
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

    /// Snapshot of search index readiness/backfill progress (best-effort).
    pub fn get_search_index_status(&self) -> SearchIndexStatus {
        let conn = self.conn.lock();

        let total_signals: i64 = conn
            .query_row("SELECT COUNT(*) FROM signals", [], |row| row.get(0))
            .unwrap_or(0);

        let indexed_rows_res = conn.query_row("SELECT COUNT(*) FROM signal_search", [], |row| {
            row.get::<_, i64>(0)
        });
        let (signal_search_exists, indexed_rows): (bool, i64) = match indexed_rows_res {
            Ok(v) => (true, v),
            Err(_) => (false, 0),
        };

        let fts_exists: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='signal_search_fts'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap_or(0)
            > 0;

        let schema_ready = signal_search_exists && fts_exists;

        let backfill_done =
            Self::get_metadata(&conn, "search_backfill_done").as_deref() == Some("1");
        let cursor_detected_at = Self::get_metadata(&conn, "search_backfill_cursor_detected_at");
        let cursor_id = Self::get_metadata(&conn, "search_backfill_cursor_id");

        SearchIndexStatus {
            schema_ready,
            backfill_done,
            total_signals: total_signals.max(0) as usize,
            indexed_rows: indexed_rows.max(0) as usize,
            cursor_detected_at,
            cursor_id,
        }
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

    /// Incrementally backfill the FTS search index.
    ///
    /// This is designed to run in a low-duty-cycle background task so search becomes robust over
    /// full history without blocking real-time ingestion.
    pub async fn backfill_search_index_step(&self, batch_size: usize) -> Result<usize> {
        let batch_size = batch_size.clamp(1, 5_000);
        let conn = self.conn.lock();

        if Self::get_metadata(&conn, "search_backfill_done").as_deref() == Some("1") {
            return Ok(0);
        }

        let cursor_detected_at = Self::get_metadata(&conn, "search_backfill_cursor_detected_at");
        let cursor_id = Self::get_metadata(&conn, "search_backfill_cursor_id");

        let batch: Vec<MarketSignal> = if let Some(dt) = cursor_detected_at.as_deref() {
            if let Some(id) = cursor_id.as_deref() {
                let mut stmt = conn.prepare_cached(
                    "SELECT id, signal_type, market_slug, confidence, risk_level,
                            details_json, detected_at, source
                     FROM signals
                     WHERE detected_at < ?1 OR (detected_at = ?1 AND id > ?2)
                     ORDER BY detected_at DESC, id
                     LIMIT ?3",
                )?;

                let batch: Vec<MarketSignal> = stmt
                    .query_map(params![dt, id, batch_size], Self::row_to_signal)?
                    .filter_map(|r| r.ok())
                    .collect();
                batch
            } else {
                let mut stmt = conn.prepare_cached(
                    "SELECT id, signal_type, market_slug, confidence, risk_level,
                            details_json, detected_at, source
                     FROM signals
                     WHERE detected_at < ?1
                     ORDER BY detected_at DESC, id
                     LIMIT ?2",
                )?;

                let batch: Vec<MarketSignal> = stmt
                    .query_map(params![dt, batch_size], Self::row_to_signal)?
                    .filter_map(|r| r.ok())
                    .collect();
                batch
            }
        } else {
            let mut stmt = conn.prepare_cached(
                "SELECT id, signal_type, market_slug, confidence, risk_level,
                        details_json, detected_at, source
                 FROM signals
                 ORDER BY detected_at DESC, id
                 LIMIT ?1",
            )?;

            let batch: Vec<MarketSignal> = stmt
                .query_map([batch_size], Self::row_to_signal)?
                .filter_map(|r| r.ok())
                .collect();
            batch
        };

        if batch.is_empty() {
            let _ = Self::set_metadata(&conn, "search_backfill_done", "1");
            info!("🔎 Search index backfill complete");
            return Ok(0);
        }

        conn.execute("BEGIN IMMEDIATE", [])?;

        for s in &batch {
            Self::upsert_signal_search_row(&conn, s)?;
        }

        // Opportunistically enrich indexed rows with any stored context.
        // SQLite defaults to 999 bound variables, so chunk IN queries conservatively.
        const MAX_VARS: usize = 900;
        let signal_ids: Vec<String> = batch.iter().map(|s| s.id.clone()).collect();
        for chunk in signal_ids.chunks(MAX_VARS) {
            if chunk.is_empty() {
                continue;
            }

            let placeholders: String = (0..chunk.len()).map(|_| "?").collect::<Vec<_>>().join(",");
            let sql = format!(
                "SELECT signal_id, context_json FROM signal_context WHERE signal_id IN ({})",
                placeholders
            );

            let mut stmt = conn.prepare_cached(&sql)?;
            let mut rows = stmt.query(params_from_iter(chunk.iter()))?;
            while let Some(row) = rows.next()? {
                let signal_id: String = row.get(0)?;
                let context_json: String = row.get(1)?;
                if let Ok(ctx) = serde_json::from_str::<SignalContext>(&context_json) {
                    if let Err(e) = Self::update_signal_search_from_context(&conn, &signal_id, &ctx)
                    {
                        warn!(
                            "search backfill: failed to apply context for {}: {}",
                            signal_id, e
                        );
                    }
                }
            }
        }

        if let Some(last) = batch.last() {
            let _ = Self::set_metadata(
                &conn,
                "search_backfill_cursor_detected_at",
                &last.detected_at,
            );
            let _ = Self::set_metadata(&conn, "search_backfill_cursor_id", &last.id);
        }

        conn.execute("COMMIT", [])?;

        Ok(batch.len())
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
        conn.execute("DELETE FROM signal_search", [])?;
        conn.execute("DELETE FROM dome_order_events", [])?;
        let _ = Self::set_metadata(&conn, "search_backfill_done", "0");
        let _ = Self::set_metadata(&conn, "search_backfill_cursor_detected_at", "");
        let _ = Self::set_metadata(&conn, "search_backfill_cursor_id", "");
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

        if let Err(e) = Self::update_signal_search_from_context(&conn, signal_id, context) {
            warn!(
                "failed to update signal_search from context for {}: {}",
                signal_id, e
            );
        }
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

    pub async fn insert_vault_llm_decision(
        &self,
        decision_id: &str,
        market_slug: &str,
        created_at: i64,
        action: &str,
        outcome_index: Option<i64>,
        outcome_text: Option<&str>,
        p_true: Option<f64>,
        bid: Option<f64>,
        ask: Option<f64>,
        p_eff: Option<f64>,
        edge: Option<f64>,
        size_mult: Option<f64>,
        consensus_models: Option<&str>,
        flags: Option<&str>,
        rationale_hash: Option<&str>,
    ) -> Result<()> {
        let conn = self.conn.lock();
        conn.execute(
            "INSERT OR REPLACE INTO vault_llm_decisions \
             (decision_id, market_slug, created_at, action, outcome_index, outcome_text, p_true, bid, ask, p_eff, edge, size_mult, consensus_models, flags, rationale_hash) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            params![
                decision_id,
                market_slug,
                created_at,
                action,
                outcome_index,
                outcome_text,
                p_true,
                bid,
                ask,
                p_eff,
                edge,
                size_mult,
                consensus_models,
                flags,
                rationale_hash,
            ],
        )?;
        Ok(())
    }

    pub async fn insert_vault_llm_model_record(
        &self,
        id: &str,
        decision_id: &str,
        model: &str,
        created_at: i64,
        parsed_ok: bool,
        action: Option<&str>,
        outcome_index: Option<i64>,
        p_true: Option<f64>,
        uncertainty: Option<&str>,
        size_mult: Option<f64>,
        flags: Option<&str>,
        rationale_hash: Option<&str>,
        raw_dsl: Option<&str>,
        latency_ms: Option<i64>,
        prompt_tokens: Option<i64>,
        completion_tokens: Option<i64>,
        total_tokens: Option<i64>,
        error: Option<&str>,
    ) -> Result<()> {
        let conn = self.conn.lock();
        conn.execute(
            "INSERT OR REPLACE INTO vault_llm_model_records \
             (id, decision_id, model, created_at, parsed_ok, action, outcome_index, p_true, uncertainty, size_mult, flags, rationale_hash, raw_dsl, latency_ms, prompt_tokens, completion_tokens, total_tokens, error) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)",
            params![
                id,
                decision_id,
                model,
                created_at,
                if parsed_ok { 1 } else { 0 },
                action,
                outcome_index,
                p_true,
                uncertainty,
                size_mult,
                flags,
                rationale_hash,
                raw_dsl,
                latency_ms,
                prompt_tokens,
                completion_tokens,
                total_tokens,
                error,
            ],
        )?;
        Ok(())
    }

    pub fn get_vault_llm_decisions(
        &self,
        limit: usize,
        market_slug: Option<&str>,
    ) -> Result<Vec<VaultLlmDecisionRow>> {
        let limit = limit.clamp(1, 500) as i64;
        let conn = self.conn.lock();

        let mut out: Vec<VaultLlmDecisionRow> = Vec::new();

        if let Some(slug) = market_slug {
            let slug = slug.trim().to_lowercase();
            let mut stmt = conn.prepare_cached(
                "SELECT decision_id, market_slug, created_at, action, outcome_index, outcome_text, p_true, bid, ask, p_eff, edge, size_mult, consensus_models, flags, rationale_hash \
                 FROM vault_llm_decisions WHERE market_slug = ?1 ORDER BY created_at DESC LIMIT ?2",
            )?;
            let rows = stmt.query_map(params![slug, limit], |row| {
                Ok(VaultLlmDecisionRow {
                    decision_id: row.get(0)?,
                    market_slug: row.get(1)?,
                    created_at: row.get(2)?,
                    action: row.get(3)?,
                    outcome_index: row.get(4)?,
                    outcome_text: row.get(5)?,
                    p_true: row.get(6)?,
                    bid: row.get(7)?,
                    ask: row.get(8)?,
                    p_eff: row.get(9)?,
                    edge: row.get(10)?,
                    size_mult: row.get(11)?,
                    consensus_models: row.get(12)?,
                    flags: row.get(13)?,
                    rationale_hash: row.get(14)?,
                })
            })?;
            for r in rows {
                if let Ok(v) = r {
                    out.push(v);
                }
            }
            return Ok(out);
        }

        let mut stmt = conn.prepare_cached(
            "SELECT decision_id, market_slug, created_at, action, outcome_index, outcome_text, p_true, bid, ask, p_eff, edge, size_mult, consensus_models, flags, rationale_hash \
             FROM vault_llm_decisions ORDER BY created_at DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit], |row| {
            Ok(VaultLlmDecisionRow {
                decision_id: row.get(0)?,
                market_slug: row.get(1)?,
                created_at: row.get(2)?,
                action: row.get(3)?,
                outcome_index: row.get(4)?,
                outcome_text: row.get(5)?,
                p_true: row.get(6)?,
                bid: row.get(7)?,
                ask: row.get(8)?,
                p_eff: row.get(9)?,
                edge: row.get(10)?,
                size_mult: row.get(11)?,
                consensus_models: row.get(12)?,
                flags: row.get(13)?,
                rationale_hash: row.get(14)?,
            })
        })?;
        for r in rows {
            if let Ok(v) = r {
                out.push(v);
            }
        }

        Ok(out)
    }

    pub fn get_vault_llm_model_records(
        &self,
        decision_id: &str,
        limit: usize,
    ) -> Result<Vec<VaultLlmModelRecordRow>> {
        let decision_id = decision_id.trim();
        if decision_id.is_empty() {
            return Ok(Vec::new());
        }
        let limit = limit.clamp(1, 100) as i64;
        let conn = self.conn.lock();

        let mut out: Vec<VaultLlmModelRecordRow> = Vec::new();
        let mut stmt = conn.prepare_cached(
            "SELECT id, decision_id, model, created_at, parsed_ok, action, outcome_index, p_true, uncertainty, size_mult, flags, rationale_hash, raw_dsl, latency_ms, prompt_tokens, completion_tokens, total_tokens, error \
             FROM vault_llm_model_records WHERE decision_id = ?1 ORDER BY created_at ASC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![decision_id, limit], |row| {
            let parsed_ok_int: i64 = row.get(4)?;
            Ok(VaultLlmModelRecordRow {
                id: row.get(0)?,
                decision_id: row.get(1)?,
                model: row.get(2)?,
                created_at: row.get(3)?,
                parsed_ok: parsed_ok_int != 0,
                action: row.get(5)?,
                outcome_index: row.get(6)?,
                p_true: row.get(7)?,
                uncertainty: row.get(8)?,
                size_mult: row.get(9)?,
                flags: row.get(10)?,
                rationale_hash: row.get(11)?,
                raw_dsl: row.get(12)?,
                latency_ms: row.get(13)?,
                prompt_tokens: row.get(14)?,
                completion_tokens: row.get(15)?,
                total_tokens: row.get(16)?,
                error: row.get(17)?,
            })
        })?;
        for r in rows {
            if let Ok(v) = r {
                out.push(v);
            }
        }

        Ok(out)
    }

    pub fn get_vault_llm_usage_today(&self, now_ts: i64) -> Result<VaultLlmUsageStats> {
        let day_start_ts = (now_ts / 86_400) * 86_400;
        let conn = self.conn.lock();

        let calls_today: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM vault_llm_model_records WHERE created_at >= ?1",
                params![day_start_ts],
                |row| row.get(0),
            )
            .unwrap_or(0);

        let tokens_today: i64 = conn
            .query_row(
                "SELECT COALESCE(SUM(COALESCE(total_tokens, 0)), 0) FROM vault_llm_model_records WHERE created_at >= ?1",
                params![day_start_ts],
                |row| row.get(0),
            )
            .unwrap_or(0);

        let mut per_market: Vec<(String, u32)> = Vec::new();
        if let Ok(mut stmt) = conn.prepare_cached(
            "SELECT d.market_slug, COUNT(*) AS c \
             FROM vault_llm_model_records m \
             JOIN vault_llm_decisions d ON m.decision_id = d.decision_id \
             WHERE m.created_at >= ?1 \
             GROUP BY d.market_slug \
             ORDER BY c DESC \
             LIMIT 10",
        ) {
            if let Ok(rows) = stmt.query_map(params![day_start_ts], |row| {
                let slug: String = row.get(0)?;
                let c: i64 = row.get(1)?;
                Ok((slug, c as u32))
            }) {
                for r in rows {
                    if let Ok(v) = r {
                        per_market.push(v);
                    }
                }
            }
        }

        Ok(VaultLlmUsageStats {
            day_start_ts,
            calls_today: calls_today.max(0) as u32,
            tokens_today: tokens_today.max(0) as u64,
            per_market_calls_today: per_market,
        })
    }

    /// Store a batch of Dome orders from API (for backtest data population)
    pub async fn store_dome_orders_batch(&self, orders: &[DomeOrder]) -> Result<usize> {
        if orders.is_empty() {
            return Ok(0);
        }

        let orders: Vec<DomeOrder> = orders.to_vec();
        let conn = self.conn.clone();

        tokio::task::spawn_blocking(move || {
            let conn = conn.lock();
            conn.execute("BEGIN TRANSACTION", [])?;

            let mut inserted = 0usize;
            for order in orders {
                // Legacy synthetic key used by earlier backfills.
                let legacy_order_hash = format!(
                    "api_{}_{}_{}_{}",
                    order.market_slug, order.timestamp, order.side, order.price
                );

                // Prefer Dome's stable order_hash (unique). Fallback to tx_hash, then a synthetic key.
                let order_hash = if !order.order_hash.trim().is_empty() {
                    order.order_hash.clone()
                } else if !order.tx_hash.trim().is_empty() {
                    format!("tx_{}", order.tx_hash)
                } else {
                    legacy_order_hash.clone()
                };

                // Serialize to JSON for storage
                let payload_json = serde_json::to_string(&order).unwrap_or_default();
                let received_at = chrono::Utc::now().timestamp();

                let result = conn.execute(
                    "INSERT OR IGNORE INTO dome_order_events \
                     (order_hash, tx_hash, user, market_slug, condition_id, token_id, timestamp, payload_json, received_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                    params![
                        order_hash,
                        &order.tx_hash,
                        &order.user,
                        order.market_slug,
                        order.condition_id,
                        order.token_id,
                        order.timestamp,
                        payload_json,
                        received_at,
                    ],
                );

                // If this order is being stored under its real Dome order_hash, delete any
                // legacy synthetic-key duplicate for the same underlying order.
                if !order.order_hash.trim().is_empty() {
                    let _ = conn.execute(
                        "DELETE FROM dome_order_events \
                         WHERE order_hash = ?1 AND json_extract(payload_json, '$.order_hash') = ?2",
                        params![legacy_order_hash, order.order_hash],
                    );
                }

                if result.is_ok() && result.unwrap() > 0 {
                    inserted += 1;
                }
            }

            conn.execute("COMMIT", [])?;
            Ok(inserted)
        })
        .await
        .map_err(|e| anyhow::anyhow!("Task join error: {}", e))?
    }

    /// Query dome_order_events for backtest
    pub async fn query_dome_orders_for_backtest(
        &self,
        where_clause: &str,
    ) -> Result<Vec<DomeOrderForBacktest>> {
        let where_clause = where_clause.to_string();
        let conn = self.conn.clone();

        tokio::task::spawn_blocking(move || {
            let conn = conn.lock();

            let query = format!(
                r#"
                SELECT 
                    timestamp,
                    market_slug,
                    json_extract(payload_json, '$.side') as side,
                    json_extract(payload_json, '$.token_label') as outcome,
                    json_extract(payload_json, '$.price') as price
                FROM dome_order_events
                {}
                ORDER BY timestamp ASC
                "#,
                where_clause
            );

            let mut stmt = conn.prepare(&query)?;
            let rows = stmt.query_map([], |row| {
                Ok(DomeOrderForBacktest {
                    timestamp: row.get(0)?,
                    market_slug: row.get::<_, String>(1).unwrap_or_default(),
                    side: row.get::<_, String>(2).unwrap_or_default(),
                    outcome: row.get::<_, String>(3).unwrap_or_default(),
                    price: row.get::<_, f64>(4).unwrap_or(0.5),
                })
            })?;

            let mut orders = Vec::new();
            for row in rows {
                if let Ok(order) = row {
                    orders.push(order);
                }
            }

            Ok(orders)
        })
        .await
        .map_err(|e| anyhow::anyhow!("Task join error: {}", e))?
    }

    pub fn upsert_updown_15m_window(
        &self,
        window: &crate::scrapers::oracle_comparison::WindowResolution,
    ) -> Result<()> {
        let market_slug = format!(
            "{}-updown-15m-{}",
            window.asset.to_ascii_lowercase(),
            window.window_start_ts
        );

        let chainlink_outcome: Option<i64> =
            window.chainlink_outcome.map(|b| if b { 1 } else { 0 });
        let binance_outcome: Option<i64> = window.binance_outcome.map(|b| if b { 1 } else { 0 });
        let agreed: Option<i64> = window.agreed.map(|b| if b { 1 } else { 0 });

        let conn = self.conn.lock();
        conn.execute(
            "INSERT INTO updown_15m_windows \
             (market_slug, asset, window_start_ts, window_end_ts, chainlink_start, chainlink_end, binance_start, binance_end, chainlink_outcome, binance_outcome, agreed, divergence_usd, divergence_bps, recorded_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14) \
             ON CONFLICT(market_slug) DO UPDATE SET \
                asset=excluded.asset, \
                window_start_ts=excluded.window_start_ts, \
                window_end_ts=excluded.window_end_ts, \
                chainlink_start=excluded.chainlink_start, \
                chainlink_end=excluded.chainlink_end, \
                binance_start=excluded.binance_start, \
                binance_end=excluded.binance_end, \
                chainlink_outcome=excluded.chainlink_outcome, \
                binance_outcome=excluded.binance_outcome, \
                agreed=excluded.agreed, \
                divergence_usd=excluded.divergence_usd, \
                divergence_bps=excluded.divergence_bps, \
                recorded_at=excluded.recorded_at",
            params![
                market_slug,
                window.asset.as_str(),
                window.window_start_ts,
                window.window_end_ts,
                window.chainlink_start,
                window.chainlink_end,
                window.binance_start,
                window.binance_end,
                chainlink_outcome,
                binance_outcome,
                agreed,
                window.divergence_usd,
                window.divergence_bps,
                window.recorded_at,
            ],
        )
        .context("upsert_updown_15m_window failed")?;

        Ok(())
    }

    pub fn get_updown_15m_windows(
        &self,
        asset: Option<&str>,
        limit: usize,
        before_end_ts: Option<i64>,
    ) -> Result<Vec<crate::scrapers::oracle_comparison::WindowResolution>> {
        let limit = limit.clamp(1, 10_000) as i64;
        let asset_norm: Option<String> = asset.map(|a| a.to_ascii_uppercase());
        let asset_param: Option<&str> = asset_norm.as_deref();

        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare_cached(
                "SELECT asset, window_start_ts, window_end_ts, chainlink_start, chainlink_end, binance_start, binance_end, chainlink_outcome, binance_outcome, agreed, divergence_usd, divergence_bps, recorded_at\
                 FROM updown_15m_windows\
                 WHERE (?1 IS NULL OR asset = ?1)\
                   AND (?2 IS NULL OR window_end_ts < ?2)\
                 ORDER BY window_end_ts DESC\
                 LIMIT ?3",
            )
            .context("prepare get_updown_15m_windows")?;

        let rows = stmt
            .query_map(params![asset_param, before_end_ts, limit], |row| {
                let asset: String = row.get(0)?;
                let window_start_ts: i64 = row.get(1)?;
                let window_end_ts: i64 = row.get(2)?;
                let chainlink_start: Option<f64> = row.get(3)?;
                let chainlink_end: Option<f64> = row.get(4)?;
                let binance_start: Option<f64> = row.get(5)?;
                let binance_end: Option<f64> = row.get(6)?;
                let chainlink_outcome: Option<i64> = row.get(7)?;
                let binance_outcome: Option<i64> = row.get(8)?;
                let agreed: Option<i64> = row.get(9)?;
                let divergence_usd: Option<f64> = row.get(10)?;
                let divergence_bps: Option<f64> = row.get(11)?;
                let recorded_at: i64 = row.get(12)?;

                Ok(crate::scrapers::oracle_comparison::WindowResolution {
                    asset,
                    window_start_ts,
                    window_end_ts,
                    chainlink_start,
                    chainlink_end,
                    binance_start,
                    binance_end,
                    chainlink_outcome: chainlink_outcome.map(|v| v != 0),
                    binance_outcome: binance_outcome.map(|v| v != 0),
                    agreed: agreed.map(|v| v != 0),
                    divergence_usd,
                    divergence_bps,
                    recorded_at,
                })
            })
            .context("query get_updown_15m_windows")?;

        let mut out: Vec<crate::scrapers::oracle_comparison::WindowResolution> = Vec::new();
        for row in rows {
            out.push(row?);
        }

        Ok(out)
    }
}

/// Order data for backtest
#[derive(Debug, Clone)]
pub struct DomeOrderForBacktest {
    pub timestamp: i64,
    pub market_slug: String,
    pub side: String,
    pub outcome: String,
    pub price: f64,
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

#[derive(Debug, Clone, Serialize)]
pub struct SearchIndexStatus {
    pub schema_ready: bool,
    pub backfill_done: bool,
    pub total_signals: usize,
    pub indexed_rows: usize,
    pub cursor_detected_at: Option<String>,
    pub cursor_id: Option<String>,
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

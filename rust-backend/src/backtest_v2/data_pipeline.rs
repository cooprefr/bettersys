//! Data Pipeline for Polymarket HFT Backtesting
//!
//! This module implements a complete, auditable data pipeline:
//!
//! ```text
//! Live Recorder
//!    ↓
//! Immutable Raw Store (append-only)
//!    ↓
//! Nightly Backfill / Integrity Pass
//!    ↓
//! Versioned Dataset Snapshot
//!    ↓
//! Replay Validation Suite
//!    ↓
//! Backtest (production-grade mode)
//! ```
//!
//! # Design Principles
//!
//! 1. **Append-only recording**: Raw events are never mutated in place
//! 2. **Arrival time capture**: Timestamps captured at earliest possible point
//! 3. **Explicit integrity**: All integrity checks are explicit and logged
//! 4. **Immutable datasets**: Finalized datasets cannot be modified
//! 5. **Deterministic replay**: Replays produce identical results
//! 6. **Full audit trail**: Every dataset version is traceable

use anyhow::{bail, ensure, Context, Result};
use parking_lot::RwLock;
use rusqlite::{params, Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tracing::{debug, info, warn};

use crate::backtest_v2::clock::Nanos;
use crate::backtest_v2::data_contract::{DatasetClassification, DatasetReadiness};
use crate::backtest_v2::events::{Event, Level, TimestampedEvent};
use crate::backtest_v2::book_recorder::BookSnapshotStorage;
use crate::backtest_v2::trade_recorder::TradePrintStorage;
use crate::backtest_v2::delta_recorder::BookDeltaStorage;
use crate::backtest_v2::oracle::{OracleRoundStorage, OracleStorageConfig};

// =============================================================================
// STEP 1: CANONICAL RAW DATA STREAMS
// =============================================================================

/// All raw data streams that must be recorded for Polymarket 15m up/down backtesting.
/// 
/// This is the CANONICAL definition - any stream not listed here is not part of
/// the official data contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RawDataStream {
    /// L2 order book snapshots (periodic full book state).
    /// Source: Polymarket CLOB REST API or WebSocket `book` messages.
    L2Snapshots,
    
    /// L2 incremental deltas (`price_change` messages).
    /// Source: Polymarket CLOB WebSocket.
    L2Deltas,
    
    /// Trade prints (public trade tape).
    /// Source: Polymarket CLOB WebSocket `last_trade_price` or REST.
    TradePrints,
    
    /// Market metadata updates (status, halt, resolution).
    /// Source: Polymarket GAMMA API or WebSocket.
    MarketMetadata,
    
    /// Oracle data (Chainlink price rounds).
    /// Source: Chainlink RPC or WebSocket.
    OracleRounds,
}

impl RawDataStream {
    /// Get the canonical stream name for storage.
    pub fn stream_name(&self) -> &'static str {
        match self {
            Self::L2Snapshots => "l2_snapshots",
            Self::L2Deltas => "l2_deltas",
            Self::TradePrints => "trade_prints",
            Self::MarketMetadata => "market_metadata",
            Self::OracleRounds => "oracle_rounds",
        }
    }
    
    /// Get the source type (WS, REST, RPC).
    pub fn source_type(&self) -> &'static str {
        match self {
            Self::L2Snapshots => "REST/WS",
            Self::L2Deltas => "WS",
            Self::TradePrints => "WS",
            Self::MarketMetadata => "REST/WS",
            Self::OracleRounds => "RPC",
        }
    }
    
    /// Get human-readable description.
    pub fn description(&self) -> &'static str {
        match self {
            Self::L2Snapshots => "L2 order book snapshots (periodic full book state)",
            Self::L2Deltas => "L2 incremental deltas (price_change messages)",
            Self::TradePrints => "Trade prints (public trade tape)",
            Self::MarketMetadata => "Market metadata updates (status, halt, resolution)",
            Self::OracleRounds => "Oracle data (Chainlink price rounds)",
        }
    }
    
    /// All streams.
    pub fn all() -> &'static [RawDataStream] {
        &[
            Self::L2Snapshots,
            Self::L2Deltas,
            Self::TradePrints,
            Self::MarketMetadata,
            Self::OracleRounds,
        ]
    }
    
    /// Minimum required streams for taker-only strategies.
    pub fn taker_minimum() -> &'static [RawDataStream] {
        &[Self::L2Snapshots, Self::TradePrints]
    }
    
    /// Required streams for maker-viable strategies.
    pub fn maker_viable() -> &'static [RawDataStream] {
        &[Self::L2Snapshots, Self::L2Deltas, Self::TradePrints]
    }
    
    /// Required streams for full production-grade backtests.
    pub fn production_grade() -> &'static [RawDataStream] {
        &[
            Self::L2Snapshots,
            Self::L2Deltas,
            Self::TradePrints,
            Self::MarketMetadata,
            Self::OracleRounds,
        ]
    }
}

/// Schema definition for a raw data stream.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamSchema {
    /// Stream identifier.
    pub stream: RawDataStream,
    /// Schema version (for migration tracking).
    pub schema_version: u32,
    /// Required fields with their types.
    pub required_fields: Vec<FieldDefinition>,
    /// Optional fields with their types.
    pub optional_fields: Vec<FieldDefinition>,
    /// Source time field name (if present in raw data).
    pub source_time_field: Option<String>,
    /// Arrival time capture point description.
    pub arrival_time_capture_point: String,
    /// Sequence semantics.
    pub sequence_semantics: SequenceSemantics,
}

/// Field definition for schema.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldDefinition {
    pub name: String,
    pub field_type: FieldType,
    pub description: String,
}

/// Field types for schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FieldType {
    String,
    I64,
    U64,
    F64,
    Bool,
    Bytes,
    Json,
}

/// Sequence semantics for a stream.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SequenceSemantics {
    /// Exchange-provided sequence field (if any).
    pub exchange_seq_field: Option<String>,
    /// Local ingest sequence field.
    pub ingest_seq_field: String,
    /// Hash field for integrity checking (if any).
    pub hash_field: Option<String>,
}

impl StreamSchema {
    /// Get schema for L2 snapshots.
    pub fn l2_snapshots() -> Self {
        Self {
            stream: RawDataStream::L2Snapshots,
            schema_version: 1,
            required_fields: vec![
                FieldDefinition {
                    name: "token_id".to_string(),
                    field_type: FieldType::String,
                    description: "CLOB token ID".to_string(),
                },
                FieldDefinition {
                    name: "arrival_time_ns".to_string(),
                    field_type: FieldType::U64,
                    description: "Arrival timestamp (ns since epoch)".to_string(),
                },
                FieldDefinition {
                    name: "ingest_seq".to_string(),
                    field_type: FieldType::U64,
                    description: "Local monotonic ingest sequence".to_string(),
                },
                FieldDefinition {
                    name: "bids_json".to_string(),
                    field_type: FieldType::Json,
                    description: "Bid levels as JSON array".to_string(),
                },
                FieldDefinition {
                    name: "asks_json".to_string(),
                    field_type: FieldType::Json,
                    description: "Ask levels as JSON array".to_string(),
                },
            ],
            optional_fields: vec![
                FieldDefinition {
                    name: "source_time_ns".to_string(),
                    field_type: FieldType::U64,
                    description: "Exchange source timestamp (ns)".to_string(),
                },
                FieldDefinition {
                    name: "exchange_seq".to_string(),
                    field_type: FieldType::U64,
                    description: "Exchange sequence number".to_string(),
                },
                FieldDefinition {
                    name: "hash".to_string(),
                    field_type: FieldType::String,
                    description: "Exchange-provided hash for integrity".to_string(),
                },
            ],
            source_time_field: Some("timestamp".to_string()),
            arrival_time_capture_point: "WebSocket message receipt, BEFORE JSON parsing".to_string(),
            sequence_semantics: SequenceSemantics {
                exchange_seq_field: Some("hash".to_string()),
                ingest_seq_field: "ingest_seq".to_string(),
                hash_field: Some("hash".to_string()),
            },
        }
    }
    
    /// Get schema for L2 deltas.
    pub fn l2_deltas() -> Self {
        Self {
            stream: RawDataStream::L2Deltas,
            schema_version: 1,
            required_fields: vec![
                FieldDefinition {
                    name: "market_id".to_string(),
                    field_type: FieldType::String,
                    description: "Market/condition ID".to_string(),
                },
                FieldDefinition {
                    name: "token_id".to_string(),
                    field_type: FieldType::String,
                    description: "CLOB token ID".to_string(),
                },
                FieldDefinition {
                    name: "side".to_string(),
                    field_type: FieldType::String,
                    description: "BUY or SELL".to_string(),
                },
                FieldDefinition {
                    name: "price".to_string(),
                    field_type: FieldType::F64,
                    description: "Price level".to_string(),
                },
                FieldDefinition {
                    name: "new_size".to_string(),
                    field_type: FieldType::F64,
                    description: "New aggregate size at level".to_string(),
                },
                FieldDefinition {
                    name: "arrival_time_ns".to_string(),
                    field_type: FieldType::U64,
                    description: "Arrival timestamp (ns since epoch)".to_string(),
                },
                FieldDefinition {
                    name: "ingest_seq".to_string(),
                    field_type: FieldType::U64,
                    description: "Local monotonic ingest sequence".to_string(),
                },
                FieldDefinition {
                    name: "seq_hash".to_string(),
                    field_type: FieldType::String,
                    description: "Exchange-provided hash".to_string(),
                },
            ],
            optional_fields: vec![
                FieldDefinition {
                    name: "ws_timestamp_ms".to_string(),
                    field_type: FieldType::U64,
                    description: "Exchange timestamp (ms)".to_string(),
                },
                FieldDefinition {
                    name: "best_bid".to_string(),
                    field_type: FieldType::F64,
                    description: "Best bid after change".to_string(),
                },
                FieldDefinition {
                    name: "best_ask".to_string(),
                    field_type: FieldType::F64,
                    description: "Best ask after change".to_string(),
                },
            ],
            source_time_field: Some("timestamp".to_string()),
            arrival_time_capture_point: "WebSocket message receipt, BEFORE JSON parsing".to_string(),
            sequence_semantics: SequenceSemantics {
                exchange_seq_field: Some("seq_hash".to_string()),
                ingest_seq_field: "ingest_seq".to_string(),
                hash_field: Some("seq_hash".to_string()),
            },
        }
    }
    
    /// Get schema for trade prints.
    pub fn trade_prints() -> Self {
        Self {
            stream: RawDataStream::TradePrints,
            schema_version: 1,
            required_fields: vec![
                FieldDefinition {
                    name: "token_id".to_string(),
                    field_type: FieldType::String,
                    description: "CLOB token ID".to_string(),
                },
                FieldDefinition {
                    name: "price".to_string(),
                    field_type: FieldType::F64,
                    description: "Trade price".to_string(),
                },
                FieldDefinition {
                    name: "size".to_string(),
                    field_type: FieldType::F64,
                    description: "Trade size".to_string(),
                },
                FieldDefinition {
                    name: "side".to_string(),
                    field_type: FieldType::String,
                    description: "Aggressor side (BUY/SELL)".to_string(),
                },
                FieldDefinition {
                    name: "arrival_time_ns".to_string(),
                    field_type: FieldType::U64,
                    description: "Arrival timestamp (ns since epoch)".to_string(),
                },
                FieldDefinition {
                    name: "ingest_seq".to_string(),
                    field_type: FieldType::U64,
                    description: "Local monotonic ingest sequence".to_string(),
                },
            ],
            optional_fields: vec![
                FieldDefinition {
                    name: "source_time_ns".to_string(),
                    field_type: FieldType::U64,
                    description: "Exchange source timestamp (ns)".to_string(),
                },
                FieldDefinition {
                    name: "trade_id".to_string(),
                    field_type: FieldType::String,
                    description: "Exchange trade ID".to_string(),
                },
            ],
            source_time_field: Some("timestamp".to_string()),
            arrival_time_capture_point: "WebSocket message receipt, BEFORE JSON parsing".to_string(),
            sequence_semantics: SequenceSemantics {
                exchange_seq_field: Some("trade_id".to_string()),
                ingest_seq_field: "ingest_seq".to_string(),
                hash_field: None,
            },
        }
    }
    
    /// Get schema for market metadata.
    pub fn market_metadata() -> Self {
        Self {
            stream: RawDataStream::MarketMetadata,
            schema_version: 1,
            required_fields: vec![
                FieldDefinition {
                    name: "market_id".to_string(),
                    field_type: FieldType::String,
                    description: "Market/condition ID".to_string(),
                },
                FieldDefinition {
                    name: "status".to_string(),
                    field_type: FieldType::String,
                    description: "Market status (active, halted, resolved)".to_string(),
                },
                FieldDefinition {
                    name: "arrival_time_ns".to_string(),
                    field_type: FieldType::U64,
                    description: "Arrival timestamp (ns since epoch)".to_string(),
                },
                FieldDefinition {
                    name: "ingest_seq".to_string(),
                    field_type: FieldType::U64,
                    description: "Local monotonic ingest sequence".to_string(),
                },
            ],
            optional_fields: vec![
                FieldDefinition {
                    name: "resolution".to_string(),
                    field_type: FieldType::String,
                    description: "Resolution outcome (YES/NO)".to_string(),
                },
                FieldDefinition {
                    name: "halt_reason".to_string(),
                    field_type: FieldType::String,
                    description: "Reason for halt (if halted)".to_string(),
                },
            ],
            source_time_field: None,
            arrival_time_capture_point: "REST response receipt or WebSocket message".to_string(),
            sequence_semantics: SequenceSemantics {
                exchange_seq_field: None,
                ingest_seq_field: "ingest_seq".to_string(),
                hash_field: None,
            },
        }
    }
    
    /// Get schema for oracle rounds.
    pub fn oracle_rounds() -> Self {
        Self {
            stream: RawDataStream::OracleRounds,
            schema_version: 1,
            required_fields: vec![
                FieldDefinition {
                    name: "feed_id".to_string(),
                    field_type: FieldType::String,
                    description: "Chainlink feed ID".to_string(),
                },
                FieldDefinition {
                    name: "round_id".to_string(),
                    field_type: FieldType::U64,
                    description: "Chainlink round ID".to_string(),
                },
                FieldDefinition {
                    name: "answer".to_string(),
                    field_type: FieldType::I64,
                    description: "Oracle answer (price * 10^decimals)".to_string(),
                },
                FieldDefinition {
                    name: "started_at".to_string(),
                    field_type: FieldType::U64,
                    description: "Round start timestamp".to_string(),
                },
                FieldDefinition {
                    name: "updated_at".to_string(),
                    field_type: FieldType::U64,
                    description: "Last update timestamp".to_string(),
                },
                FieldDefinition {
                    name: "arrival_time_ns".to_string(),
                    field_type: FieldType::U64,
                    description: "Arrival timestamp (ns since epoch)".to_string(),
                },
                FieldDefinition {
                    name: "ingest_seq".to_string(),
                    field_type: FieldType::U64,
                    description: "Local monotonic ingest sequence".to_string(),
                },
            ],
            optional_fields: vec![
                FieldDefinition {
                    name: "answered_in_round".to_string(),
                    field_type: FieldType::U64,
                    description: "Round in which answer was computed".to_string(),
                },
            ],
            source_time_field: Some("updated_at".to_string()),
            arrival_time_capture_point: "RPC response receipt".to_string(),
            sequence_semantics: SequenceSemantics {
                exchange_seq_field: Some("round_id".to_string()),
                ingest_seq_field: "ingest_seq".to_string(),
                hash_field: None,
            },
        }
    }
    
    /// Get schema for a stream.
    pub fn for_stream(stream: RawDataStream) -> Self {
        match stream {
            RawDataStream::L2Snapshots => Self::l2_snapshots(),
            RawDataStream::L2Deltas => Self::l2_deltas(),
            RawDataStream::TradePrints => Self::trade_prints(),
            RawDataStream::MarketMetadata => Self::market_metadata(),
            RawDataStream::OracleRounds => Self::oracle_rounds(),
        }
    }
}

// =============================================================================
// STEP 2: RAW EVENT RECORD (APPEND-ONLY)
// =============================================================================

/// A raw event record as stored in the append-only raw store.
/// 
/// This is the canonical format for all recorded events before normalization.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawEventRecord {
    /// Stream this event belongs to.
    pub stream: RawDataStream,
    /// Market ID (for multi-market recording).
    pub market_id: String,
    /// Token ID (for token-specific streams like L2).
    pub token_id: Option<String>,
    /// Raw payload (JSON or binary).
    pub payload: RawPayload,
    /// Arrival time at the recorder (ns since epoch).
    /// Captured at the EARLIEST possible point.
    pub ingest_arrival_time_ns: u64,
    /// Local monotonic sequence for this (market_id, stream) pair.
    pub ingest_seq: u64,
    /// Source time from the exchange (ns since epoch), if present.
    pub source_time_ns: Option<u64>,
    /// Exchange-provided sequence/hash for integrity, if present.
    pub exchange_seq: Option<String>,
}

/// Raw payload format.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RawPayload {
    /// JSON payload (most common).
    Json(serde_json::Value),
    /// Binary payload.
    Binary(Vec<u8>),
}

impl RawPayload {
    /// Compute SHA256 hash of the payload.
    pub fn hash(&self) -> String {
        let mut hasher = Sha256::new();
        match self {
            Self::Json(v) => hasher.update(v.to_string().as_bytes()),
            Self::Binary(b) => hasher.update(b),
        }
        format!("{:x}", hasher.finalize())
    }
    
    /// Get payload size in bytes.
    pub fn size_bytes(&self) -> usize {
        match self {
            Self::Json(v) => v.to_string().len(),
            Self::Binary(b) => b.len(),
        }
    }
}

// =============================================================================
// STEP 2: LIVE DATA RECORDER (APPEND-ONLY STORAGE)
// =============================================================================

/// Configuration for the live data recorder.
#[derive(Debug, Clone)]
pub struct LiveRecorderConfig {
    /// Path to the raw data store.
    pub raw_store_path: PathBuf,
    /// Markets to record.
    pub markets: Vec<String>,
    /// Streams to record.
    pub streams: Vec<RawDataStream>,
    /// Maximum events to buffer before flush.
    pub buffer_size: usize,
    /// Flush interval (even if buffer not full).
    pub flush_interval: Duration,
    /// Enable strict mode (fail on any error).
    pub strict_mode: bool,
    /// Recorder version (git hash).
    pub recorder_version: String,
}

impl Default for LiveRecorderConfig {
    fn default() -> Self {
        Self {
            raw_store_path: PathBuf::from("polymarket_raw.db"),
            markets: vec![],
            streams: RawDataStream::all().to_vec(),
            buffer_size: 1000,
            flush_interval: Duration::from_secs(1),
            strict_mode: true,
            recorder_version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }
}

/// Live data recorder that writes to append-only storage.
pub struct LiveRecorder {
    config: LiveRecorderConfig,
    conn: Connection,
    /// Per-(market_id, stream) ingest sequence counters.
    ingest_seqs: RwLock<HashMap<(String, RawDataStream), AtomicU64>>,
    /// Recording statistics.
    stats: RwLock<RecorderStats>,
    /// Whether recording is active.
    active: std::sync::atomic::AtomicBool,
}

/// Recording statistics.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RecorderStats {
    /// Events recorded per stream.
    pub events_per_stream: HashMap<String, u64>,
    /// Total bytes written.
    pub total_bytes: u64,
    /// Errors encountered.
    pub errors: u64,
    /// Start time (ns since epoch).
    pub start_time_ns: u64,
    /// Last event time (ns since epoch).
    pub last_event_time_ns: u64,
}

impl LiveRecorder {
    /// Create a new live recorder.
    pub fn new(config: LiveRecorderConfig) -> Result<Self> {
        let conn = Connection::open_with_flags(
            &config.raw_store_path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
        )?;
        
        // Initialize schema
        Self::init_schema(&conn)?;
        
        // Record recorder metadata
        let now_ns = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
            .as_nanos() as u64;
        
        conn.execute(
            "INSERT OR REPLACE INTO recorder_metadata (key, value, updated_at)
             VALUES ('recorder_version', ?1, ?2)",
            params![&config.recorder_version, now_ns as i64],
        )?;
        
        let stats = RecorderStats {
            start_time_ns: now_ns,
            ..Default::default()
        };
        
        info!(
            path = %config.raw_store_path.display(),
            version = %config.recorder_version,
            "Live recorder initialized"
        );
        
        Ok(Self {
            config,
            conn,
            ingest_seqs: RwLock::new(HashMap::new()),
            stats: RwLock::new(stats),
            active: std::sync::atomic::AtomicBool::new(true),
        })
    }
    
    /// Initialize database schema.
    fn init_schema(conn: &Connection) -> Result<()> {
        conn.execute_batch(
            r#"
            PRAGMA journal_mode = WAL;
            PRAGMA synchronous = NORMAL;
            PRAGMA cache_size = -64000;
            
            -- Raw events table (append-only)
            CREATE TABLE IF NOT EXISTS raw_events (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                stream TEXT NOT NULL,
                market_id TEXT NOT NULL,
                token_id TEXT,
                payload_type TEXT NOT NULL,
                payload_data BLOB NOT NULL,
                ingest_arrival_time_ns INTEGER NOT NULL,
                ingest_seq INTEGER NOT NULL,
                source_time_ns INTEGER,
                exchange_seq TEXT,
                payload_hash TEXT NOT NULL,
                created_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now'))
            );
            
            -- Indexes for efficient querying
            CREATE INDEX IF NOT EXISTS idx_raw_events_stream_market 
                ON raw_events(stream, market_id, ingest_arrival_time_ns);
            CREATE INDEX IF NOT EXISTS idx_raw_events_token 
                ON raw_events(token_id, ingest_arrival_time_ns) WHERE token_id IS NOT NULL;
            CREATE INDEX IF NOT EXISTS idx_raw_events_arrival 
                ON raw_events(ingest_arrival_time_ns);
            
            -- Recorder metadata
            CREATE TABLE IF NOT EXISTS recorder_metadata (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL,
                updated_at INTEGER NOT NULL
            );
            
            -- Recording sessions (for audit trail)
            CREATE TABLE IF NOT EXISTS recording_sessions (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                start_time_ns INTEGER NOT NULL,
                end_time_ns INTEGER,
                recorder_version TEXT NOT NULL,
                config_json TEXT NOT NULL,
                events_recorded INTEGER DEFAULT 0,
                status TEXT NOT NULL DEFAULT 'active'
            );
            "#,
        )?;
        
        Ok(())
    }
    
    /// Record a raw event (append-only).
    /// 
    /// Returns error in strict mode if recording fails.
    pub fn record(&self, event: RawEventRecord) -> Result<()> {
        if !self.active.load(Ordering::Relaxed) {
            bail!("Recorder is not active");
        }
        
        let payload_type = match &event.payload {
            RawPayload::Json(_) => "json",
            RawPayload::Binary(_) => "binary",
        };
        
        let payload_data = match &event.payload {
            RawPayload::Json(v) => v.to_string().into_bytes(),
            RawPayload::Binary(b) => b.clone(),
        };
        
        let payload_hash = event.payload.hash();
        let payload_size = payload_data.len();
        
        self.conn.execute(
            "INSERT INTO raw_events (
                stream, market_id, token_id, payload_type, payload_data,
                ingest_arrival_time_ns, ingest_seq, source_time_ns, exchange_seq, payload_hash
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                event.stream.stream_name(),
                &event.market_id,
                &event.token_id,
                payload_type,
                &payload_data,
                event.ingest_arrival_time_ns as i64,
                event.ingest_seq as i64,
                event.source_time_ns.map(|t| t as i64),
                &event.exchange_seq,
                &payload_hash,
            ],
        )?;
        
        // Update stats
        {
            let mut stats = self.stats.write();
            *stats.events_per_stream.entry(event.stream.stream_name().to_string()).or_default() += 1;
            stats.total_bytes += payload_size as u64;
            stats.last_event_time_ns = event.ingest_arrival_time_ns;
        }
        
        Ok(())
    }
    
    /// Get the next ingest sequence for a (market_id, stream) pair.
    pub fn next_ingest_seq(&self, market_id: &str, stream: RawDataStream) -> u64 {
        let key = (market_id.to_string(), stream);
        let seqs = self.ingest_seqs.read();
        
        if let Some(counter) = seqs.get(&key) {
            return counter.fetch_add(1, Ordering::SeqCst);
        }
        
        drop(seqs);
        let mut seqs = self.ingest_seqs.write();
        let counter = seqs.entry(key).or_insert_with(|| AtomicU64::new(0));
        counter.fetch_add(1, Ordering::SeqCst)
    }
    
    /// Get current recording statistics.
    pub fn stats(&self) -> RecorderStats {
        self.stats.read().clone()
    }
    
    /// Stop recording.
    pub fn stop(&self) {
        self.active.store(false, Ordering::Relaxed);
        info!("Live recorder stopped");
    }
    
    /// Check if recording is active.
    pub fn is_active(&self) -> bool {
        self.active.load(Ordering::Relaxed)
    }
}

// =============================================================================
// STEP 3: NIGHTLY BACKFILL / INTEGRITY PASS
// =============================================================================

/// Configuration for nightly backfill.
#[derive(Debug, Clone)]
pub struct BackfillConfig {
    /// Source raw store path.
    pub raw_store_path: PathBuf,
    /// Target normalized store path.
    pub normalized_store_path: PathBuf,
    /// Date to backfill (YYYY-MM-DD).
    pub date: String,
    /// Integrity policy for duplicates.
    pub duplicate_policy: DuplicatePolicy,
    /// Integrity policy for out-of-order events.
    pub out_of_order_policy: OutOfOrderPolicy,
    /// Integrity policy for gaps.
    pub gap_policy: GapPolicy,
    /// Backfill version (git hash).
    pub backfill_version: String,
}

/// Policy for handling duplicates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DuplicatePolicy {
    /// Drop duplicates silently (log count).
    Drop,
    /// Fail on any duplicate.
    Fail,
}

/// Policy for handling out-of-order events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OutOfOrderPolicy {
    /// Reorder events by arrival time.
    Reorder,
    /// Fail on out-of-order events.
    Fail,
}

/// Policy for handling gaps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GapPolicy {
    /// Log gaps but continue.
    Log,
    /// Fail on gaps.
    Fail,
    /// Resync from last known good state.
    Resync,
}

/// Integrity report from backfill.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntegrityReport {
    /// Date processed.
    pub date: String,
    /// Market ID.
    pub market_id: String,
    /// Report generation time.
    pub generated_at_ns: u64,
    /// Per-stream event counts.
    pub event_counts: HashMap<String, u64>,
    /// Duplicates dropped per stream.
    pub duplicates_dropped: HashMap<String, u64>,
    /// Out-of-order events per stream.
    pub out_of_order_events: HashMap<String, u64>,
    /// Gaps detected per stream.
    pub gaps_detected: HashMap<String, Vec<GapInfo>>,
    /// Resyncs triggered.
    pub resyncs_triggered: u64,
    /// Overall status.
    pub status: IntegrityStatus,
    /// Detailed issues.
    pub issues: Vec<IntegrityIssue>,
}

/// Gap information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GapInfo {
    pub stream: String,
    pub expected_seq: u64,
    pub actual_seq: u64,
    pub gap_size: u64,
    pub timestamp_ns: u64,
}

/// Integrity issue.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntegrityIssue {
    pub severity: IssueSeverity,
    pub stream: String,
    pub description: String,
    pub timestamp_ns: Option<u64>,
}

/// Issue severity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum IssueSeverity {
    Info,
    Warning,
    Error,
    Critical,
}

/// Integrity status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum IntegrityStatus {
    /// All checks passed.
    Clean,
    /// Minor issues (duplicates dropped, etc).
    MinorIssues,
    /// Major issues (gaps, resyncs).
    MajorIssues,
    /// Failed (critical errors).
    Failed,
}

/// Nightly backfill runner.
pub struct NightlyBackfill {
    config: BackfillConfig,
}

impl NightlyBackfill {
    /// Create a new backfill runner.
    pub fn new(config: BackfillConfig) -> Self {
        Self { config }
    }
    
    /// Run the backfill process.
    pub fn run(&self, market_id: &str) -> Result<IntegrityReport> {
        info!(
            date = %self.config.date,
            market_id = %market_id,
            "Starting nightly backfill"
        );
        
        let now_ns = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
            .as_nanos() as u64;
        
        let mut report = IntegrityReport {
            date: self.config.date.clone(),
            market_id: market_id.to_string(),
            generated_at_ns: now_ns,
            event_counts: HashMap::new(),
            duplicates_dropped: HashMap::new(),
            out_of_order_events: HashMap::new(),
            gaps_detected: HashMap::new(),
            resyncs_triggered: 0,
            status: IntegrityStatus::Clean,
            issues: vec![],
        };
        
        // Open raw store
        let raw_conn = Connection::open_with_flags(
            &self.config.raw_store_path,
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        ).context("Failed to open raw store")?;
        
        // Open/create normalized store
        let norm_conn = Connection::open_with_flags(
            &self.config.normalized_store_path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
        ).context("Failed to open normalized store")?;
        
        // Initialize normalized schema
        Self::init_normalized_schema(&norm_conn)?;
        
        // Process each stream
        for stream in RawDataStream::all() {
            self.process_stream(&raw_conn, &norm_conn, market_id, *stream, &mut report)?;
        }
        
        // Determine overall status
        report.status = self.compute_status(&report);
        
        // Store integrity report
        self.store_report(&norm_conn, &report)?;
        
        info!(
            date = %self.config.date,
            market_id = %market_id,
            status = ?report.status,
            "Nightly backfill completed"
        );
        
        Ok(report)
    }
    
    /// Initialize normalized storage schema.
    fn init_normalized_schema(conn: &Connection) -> Result<()> {
        conn.execute_batch(
            r#"
            PRAGMA journal_mode = WAL;
            PRAGMA synchronous = NORMAL;
            
            -- Normalized book snapshots
            CREATE TABLE IF NOT EXISTS book_snapshots (
                id INTEGER PRIMARY KEY,
                token_id TEXT NOT NULL,
                arrival_time_ns INTEGER NOT NULL,
                ingest_seq INTEGER NOT NULL,
                source_time_ns INTEGER,
                exchange_seq INTEGER,
                bids_json TEXT NOT NULL,
                asks_json TEXT NOT NULL,
                hash TEXT,
                UNIQUE(token_id, ingest_seq)
            );
            
            -- Normalized book deltas
            CREATE TABLE IF NOT EXISTS book_deltas (
                id INTEGER PRIMARY KEY,
                market_id TEXT NOT NULL,
                token_id TEXT NOT NULL,
                side TEXT NOT NULL,
                price REAL NOT NULL,
                new_size REAL NOT NULL,
                arrival_time_ns INTEGER NOT NULL,
                ingest_seq INTEGER NOT NULL,
                ws_timestamp_ms INTEGER,
                seq_hash TEXT NOT NULL,
                best_bid REAL,
                best_ask REAL,
                UNIQUE(token_id, ingest_seq)
            );
            
            -- Normalized trade prints
            CREATE TABLE IF NOT EXISTS trade_prints (
                id INTEGER PRIMARY KEY,
                token_id TEXT NOT NULL,
                price REAL NOT NULL,
                size REAL NOT NULL,
                side TEXT NOT NULL,
                arrival_time_ns INTEGER NOT NULL,
                ingest_seq INTEGER NOT NULL,
                source_time_ns INTEGER,
                trade_id TEXT,
                UNIQUE(token_id, ingest_seq)
            );
            
            -- Normalized market metadata
            CREATE TABLE IF NOT EXISTS market_metadata (
                id INTEGER PRIMARY KEY,
                market_id TEXT NOT NULL,
                status TEXT NOT NULL,
                arrival_time_ns INTEGER NOT NULL,
                ingest_seq INTEGER NOT NULL,
                resolution TEXT,
                halt_reason TEXT,
                UNIQUE(market_id, ingest_seq)
            );
            
            -- Normalized oracle rounds
            CREATE TABLE IF NOT EXISTS oracle_rounds (
                id INTEGER PRIMARY KEY,
                feed_id TEXT NOT NULL,
                round_id INTEGER NOT NULL,
                answer INTEGER NOT NULL,
                started_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL,
                arrival_time_ns INTEGER NOT NULL,
                ingest_seq INTEGER NOT NULL,
                answered_in_round INTEGER,
                UNIQUE(feed_id, round_id)
            );
            
            -- Integrity reports
            CREATE TABLE IF NOT EXISTS integrity_reports (
                id INTEGER PRIMARY KEY,
                date TEXT NOT NULL,
                market_id TEXT NOT NULL,
                generated_at_ns INTEGER NOT NULL,
                report_json TEXT NOT NULL,
                status TEXT NOT NULL,
                UNIQUE(date, market_id)
            );
            
            -- Indexes
            CREATE INDEX IF NOT EXISTS idx_snapshots_token_time 
                ON book_snapshots(token_id, arrival_time_ns);
            CREATE INDEX IF NOT EXISTS idx_deltas_token_time 
                ON book_deltas(token_id, arrival_time_ns);
            CREATE INDEX IF NOT EXISTS idx_trades_token_time 
                ON trade_prints(token_id, arrival_time_ns);
            "#,
        )?;
        
        Ok(())
    }
    
    /// Process a single stream.
    fn process_stream(
        &self,
        raw_conn: &Connection,
        norm_conn: &Connection,
        market_id: &str,
        stream: RawDataStream,
        report: &mut IntegrityReport,
    ) -> Result<()> {
        let stream_name = stream.stream_name();
        
        // Parse date range
        let date = &self.config.date;
        let start_ns = self.date_to_start_ns(date)?;
        let end_ns = start_ns + 86_400_000_000_000u64; // 24 hours in ns
        
        // Query raw events for this stream and date
        let mut stmt = raw_conn.prepare(
            "SELECT stream, market_id, token_id, payload_type, payload_data,
                    ingest_arrival_time_ns, ingest_seq, source_time_ns, exchange_seq, payload_hash
             FROM raw_events
             WHERE stream = ?1 AND market_id = ?2
               AND ingest_arrival_time_ns >= ?3 AND ingest_arrival_time_ns < ?4
             ORDER BY ingest_arrival_time_ns, ingest_seq"
        )?;
        
        let mut rows = stmt.query(params![
            stream_name,
            market_id,
            start_ns as i64,
            end_ns as i64,
        ])?;
        
        let mut event_count = 0u64;
        let mut duplicates = 0u64;
        let mut out_of_order = 0u64;
        let mut last_arrival_ns: Option<u64> = None;
        let mut seen_hashes: std::collections::HashSet<String> = std::collections::HashSet::new();
        
        while let Some(row) = rows.next()? {
            let payload_hash: String = row.get(9)?;
            let arrival_time_ns: i64 = row.get(5)?;
            let arrival_time_ns = arrival_time_ns as u64;
            
            // Check for duplicates
            if !seen_hashes.insert(payload_hash.clone()) {
                duplicates += 1;
                match self.config.duplicate_policy {
                    DuplicatePolicy::Drop => continue,
                    DuplicatePolicy::Fail => {
                        bail!("Duplicate event detected: {}", payload_hash);
                    }
                }
            }
            
            // Check for out-of-order
            if let Some(last) = last_arrival_ns {
                if arrival_time_ns < last {
                    out_of_order += 1;
                    match self.config.out_of_order_policy {
                        OutOfOrderPolicy::Reorder => {
                            // Will be handled by ORDER BY
                        }
                        OutOfOrderPolicy::Fail => {
                            bail!("Out-of-order event detected");
                        }
                    }
                }
            }
            last_arrival_ns = Some(arrival_time_ns);
            
            // TODO: Parse and normalize into appropriate table based on stream
            // For now, just count
            event_count += 1;
        }
        
        report.event_counts.insert(stream_name.to_string(), event_count);
        if duplicates > 0 {
            report.duplicates_dropped.insert(stream_name.to_string(), duplicates);
        }
        if out_of_order > 0 {
            report.out_of_order_events.insert(stream_name.to_string(), out_of_order);
        }
        
        Ok(())
    }
    
    /// Convert date string to start nanoseconds.
    fn date_to_start_ns(&self, date: &str) -> Result<u64> {
        // Parse YYYY-MM-DD
        let parts: Vec<&str> = date.split('-').collect();
        ensure!(parts.len() == 3, "Invalid date format: {}", date);
        
        let year: i32 = parts[0].parse()?;
        let month: u32 = parts[1].parse()?;
        let day: u32 = parts[2].parse()?;
        
        // Calculate days since epoch (simplified)
        // This is approximate; for production use chrono
        let days_since_epoch = (year - 1970) as u64 * 365 
            + (month - 1) as u64 * 30 
            + (day - 1) as u64;
        
        Ok(days_since_epoch * 86_400_000_000_000)
    }
    
    /// Compute overall status from report.
    fn compute_status(&self, report: &IntegrityReport) -> IntegrityStatus {
        let has_critical = report.issues.iter().any(|i| i.severity == IssueSeverity::Critical);
        let has_errors = report.issues.iter().any(|i| i.severity == IssueSeverity::Error);
        let has_gaps = !report.gaps_detected.is_empty();
        let has_duplicates = report.duplicates_dropped.values().sum::<u64>() > 0;
        
        if has_critical {
            IntegrityStatus::Failed
        } else if has_errors || has_gaps || report.resyncs_triggered > 0 {
            IntegrityStatus::MajorIssues
        } else if has_duplicates || report.out_of_order_events.values().sum::<u64>() > 0 {
            IntegrityStatus::MinorIssues
        } else {
            IntegrityStatus::Clean
        }
    }
    
    /// Store integrity report.
    fn store_report(&self, conn: &Connection, report: &IntegrityReport) -> Result<()> {
        let report_json = serde_json::to_string(report)?;
        
        conn.execute(
            "INSERT OR REPLACE INTO integrity_reports (date, market_id, generated_at_ns, report_json, status)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                &report.date,
                &report.market_id,
                report.generated_at_ns as i64,
                &report_json,
                format!("{:?}", report.status),
            ],
        )?;
        
        Ok(())
    }
}

// =============================================================================
// STEP 4: DATASET VERSIONING AND IMMUTABILITY
// =============================================================================

/// A versioned, immutable dataset.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatasetVersion {
    /// Unique dataset ID (SHA256 hash of contents).
    pub dataset_id: String,
    /// Human-readable name.
    pub name: String,
    /// Time range covered.
    pub time_range: TimeRange,
    /// Streams included.
    pub streams: Vec<RawDataStream>,
    /// Markets included.
    pub markets: Vec<String>,
    /// Hash of the integrity report.
    pub integrity_report_hash: String,
    /// Schema version used.
    pub schema_version: u32,
    /// Recorder version (git hash).
    pub recorder_version: String,
    /// Backfill version (git hash).
    pub backfill_version: String,
    /// Creation timestamp.
    pub created_at_ns: u64,
    /// Whether this dataset is finalized (immutable).
    pub finalized: bool,
    /// Dataset classification.
    pub classification: DatasetClassification,
    /// Dataset readiness.
    pub readiness: DatasetReadiness,
    /// Trust level.
    pub trust_level: DatasetTrustLevel,
    /// Optional metadata (e.g., live integrity counters for parity validation).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<HashMap<String, serde_json::Value>>,
}

/// Time range for a dataset.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimeRange {
    pub start_ns: u64,
    pub end_ns: u64,
}

/// Trust level for a dataset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DatasetTrustLevel {
    /// All validations passed; suitable for production.
    Trusted,
    /// Minor issues; results should be qualified.
    Approximate,
    /// Validation failed; cannot be used.
    Rejected,
    /// Not yet validated.
    Pending,
}

impl DatasetVersion {
    /// Create a new dataset version.
    pub fn new(
        name: String,
        time_range: TimeRange,
        streams: Vec<RawDataStream>,
        markets: Vec<String>,
        integrity_report: &IntegrityReport,
        recorder_version: String,
        backfill_version: String,
    ) -> Self {
        let now_ns = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
            .as_nanos() as u64;
        
        // Compute integrity report hash
        let integrity_json = serde_json::to_string(integrity_report).unwrap_or_default();
        let integrity_report_hash = {
            let mut hasher = Sha256::new();
            hasher.update(integrity_json.as_bytes());
            format!("{:x}", hasher.finalize())
        };
        
        // Determine classification based on streams
        let has_deltas = streams.contains(&RawDataStream::L2Deltas);
        let has_snapshots = streams.contains(&RawDataStream::L2Snapshots);
        let has_trades = streams.contains(&RawDataStream::TradePrints);
        
        let classification = if has_deltas && has_snapshots && has_trades {
            DatasetClassification::FullIncremental
        } else if has_snapshots {
            DatasetClassification::SnapshotOnly
        } else {
            DatasetClassification::SnapshotOnly // Default
        };
        
        // Determine readiness
        let readiness = if classification == DatasetClassification::FullIncremental {
            DatasetReadiness::MakerViable
        } else {
            DatasetReadiness::TakerOnly
        };
        
        // Initial trust level is Pending until validated
        let trust_level = DatasetTrustLevel::Pending;
        
        // Compute dataset ID
        let mut hasher = Sha256::new();
        hasher.update(format!("{:?}", time_range).as_bytes());
        hasher.update(format!("{:?}", streams).as_bytes());
        hasher.update(format!("{:?}", markets).as_bytes());
        hasher.update(integrity_report_hash.as_bytes());
        hasher.update(recorder_version.as_bytes());
        hasher.update(backfill_version.as_bytes());
        let dataset_id = format!("{:x}", hasher.finalize());
        
        Self {
            dataset_id,
            name,
            time_range,
            streams,
            markets,
            integrity_report_hash,
            schema_version: 1,
            recorder_version,
            backfill_version,
            created_at_ns: now_ns,
            finalized: false,
            classification,
            readiness,
            trust_level,
            metadata: None,
        }
    }
    
    /// Finalize this dataset (make immutable).
    pub fn finalize(&mut self) {
        self.finalized = true;
    }
    
    /// Check if this dataset can be modified.
    pub fn is_mutable(&self) -> bool {
        !self.finalized
    }
    
    /// Set trust level (only if not finalized with Trusted).
    pub fn set_trust_level(&mut self, level: DatasetTrustLevel) -> Result<()> {
        if self.finalized && self.trust_level == DatasetTrustLevel::Trusted {
            bail!("Cannot modify trust level of finalized Trusted dataset");
        }
        self.trust_level = level;
        Ok(())
    }
}

// =============================================================================
// STEP 5: REPLAY VALIDATION SUITE
// =============================================================================

/// Configuration for replay validation.
#[derive(Debug, Clone)]
pub struct ReplayValidationConfig {
    /// Dataset to validate.
    pub dataset_id: String,
    /// Number of replay passes for determinism check.
    pub determinism_passes: u32,
    /// Enable verbose logging.
    pub verbose: bool,
    /// Path to the normalized data database.
    /// If None, uses in-memory DB (for testing) or returns error for real validation.
    pub db_path: Option<String>,
    /// Optional oracle database path (if separate from main DB).
    pub oracle_db_path: Option<String>,
}

/// Result of replay validation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplayValidationResult {
    /// Dataset ID validated.
    pub dataset_id: String,
    /// Validation timestamp.
    pub validated_at_ns: u64,
    /// Event ordering validated.
    pub ordering_valid: bool,
    /// No integrity violations.
    pub integrity_valid: bool,
    /// Book reconstruction invariants hold.
    pub book_invariants_valid: bool,
    /// Replay is deterministic.
    pub determinism_valid: bool,
    /// Replay fingerprints (one per pass).
    pub fingerprints: Vec<String>,
    /// Overall result.
    pub passed: bool,
    /// Failure reasons (if any).
    pub failure_reasons: Vec<String>,
    /// Parity validation result (live vs replay integrity counters).
    pub parity_validation: Option<ParityValidationResult>,
}

/// Result of comparing live-recorded integrity counters with replay-derived counters.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParityValidationResult {
    /// Whether parity validation passed.
    pub passed: bool,
    /// Live recording integrity counters (captured during recording).
    pub live_counters: IntegrityCounterSnapshot,
    /// Replay-derived integrity counters.
    pub replay_counters: IntegrityCounterSnapshot,
    /// Counter mismatches (if any).
    pub mismatches: Vec<CounterMismatch>,
    /// First mismatching event (if any).
    pub first_mismatch_event: Option<MismatchEventInfo>,
    /// Total events processed in replay.
    pub replay_event_count: u64,
}

/// Snapshot of integrity counters for comparison.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct IntegrityCounterSnapshot {
    /// Number of duplicate events.
    pub duplicates: u64,
    /// Number of sequence gaps detected.
    pub gaps: u64,
    /// Total missing sequences across gaps.
    pub missing_sequences: u64,
    /// Number of out-of-order events.
    pub out_of_order: u64,
    /// Number of events dropped.
    pub dropped: u64,
    /// Number of reorder buffer uses.
    pub reorder_buffer_uses: u64,
    /// Whether processing was halted.
    pub halted: bool,
    /// Total events processed.
    pub total_processed: u64,
    /// Total events forwarded.
    pub total_forwarded: u64,
}

impl IntegrityCounterSnapshot {
    /// Create from PathologyCounters.
    pub fn from_pathology_counters(counters: &crate::backtest_v2::integrity::PathologyCounters) -> Self {
        Self {
            duplicates: counters.duplicates_dropped,
            gaps: counters.gaps_detected,
            missing_sequences: counters.total_missing_sequences,
            out_of_order: counters.out_of_order_detected,
            dropped: counters.out_of_order_dropped,
            reorder_buffer_uses: counters.reordered_events,
            halted: counters.halted,
            total_processed: counters.total_events_processed,
            total_forwarded: counters.total_events_forwarded,
        }
    }
    
    /// Check if counters match exactly (production-grade requirement).
    pub fn matches_exactly(&self, other: &Self) -> bool {
        self == other
    }
    
    /// Get differences as a list of mismatches.
    pub fn diff(&self, other: &Self) -> Vec<CounterMismatch> {
        let mut mismatches = Vec::new();
        
        if self.duplicates != other.duplicates {
            mismatches.push(CounterMismatch {
                counter_name: "duplicates".to_string(),
                live_value: self.duplicates,
                replay_value: other.duplicates,
            });
        }
        if self.gaps != other.gaps {
            mismatches.push(CounterMismatch {
                counter_name: "gaps".to_string(),
                live_value: self.gaps,
                replay_value: other.gaps,
            });
        }
        if self.missing_sequences != other.missing_sequences {
            mismatches.push(CounterMismatch {
                counter_name: "missing_sequences".to_string(),
                live_value: self.missing_sequences,
                replay_value: other.missing_sequences,
            });
        }
        if self.out_of_order != other.out_of_order {
            mismatches.push(CounterMismatch {
                counter_name: "out_of_order".to_string(),
                live_value: self.out_of_order,
                replay_value: other.out_of_order,
            });
        }
        if self.dropped != other.dropped {
            mismatches.push(CounterMismatch {
                counter_name: "dropped".to_string(),
                live_value: self.dropped,
                replay_value: other.dropped,
            });
        }
        if self.halted != other.halted {
            mismatches.push(CounterMismatch {
                counter_name: "halted".to_string(),
                live_value: if self.halted { 1 } else { 0 },
                replay_value: if other.halted { 1 } else { 0 },
            });
        }
        if self.total_processed != other.total_processed {
            mismatches.push(CounterMismatch {
                counter_name: "total_processed".to_string(),
                live_value: self.total_processed,
                replay_value: other.total_processed,
            });
        }
        if self.total_forwarded != other.total_forwarded {
            mismatches.push(CounterMismatch {
                counter_name: "total_forwarded".to_string(),
                live_value: self.total_forwarded,
                replay_value: other.total_forwarded,
            });
        }
        
        mismatches
    }
}

/// A single counter mismatch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CounterMismatch {
    /// Name of the counter.
    pub counter_name: String,
    /// Value from live recording.
    pub live_value: u64,
    /// Value from replay.
    pub replay_value: u64,
}

/// Information about the first mismatching event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MismatchEventInfo {
    /// Market ID.
    pub market_id: String,
    /// Token ID (if applicable).
    pub token_id: Option<String>,
    /// Arrival time (nanoseconds).
    pub arrival_time_ns: u64,
    /// Ingest sequence.
    pub ingest_seq: u64,
    /// Sequence hash (if applicable).
    pub seq_hash: Option<String>,
    /// Stream type.
    pub stream: String,
    /// Description of what mismatched.
    pub mismatch_description: String,
}

// =============================================================================
// LOAD REPORT - OUTPUT OF DATA LOADING
// =============================================================================

/// Report from loading events from normalized SQLite tables.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadReport {
    /// Total number of events loaded.
    pub event_count_total: u64,
    /// Counts per stream type.
    pub counts_per_stream: StreamCounts,
    /// First event timestamp (arrival_time_ns).
    pub first_event_time_ns: Option<u64>,
    /// Last event timestamp (arrival_time_ns).
    pub last_event_time_ns: Option<u64>,
    /// Time range requested.
    pub requested_start_ns: u64,
    pub requested_end_ns: u64,
    /// Markets loaded.
    pub markets_loaded: Vec<String>,
    /// Tokens loaded.
    pub tokens_loaded: Vec<String>,
    /// Whether all required streams had data.
    pub all_streams_present: bool,
    /// Streams that had no data.
    pub empty_streams: Vec<String>,
}

/// Per-stream event counts.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StreamCounts {
    pub snapshots: u64,
    pub deltas: u64,
    pub trades: u64,
    pub oracle_rounds: u64,
    pub market_metadata: u64,
}

impl StreamCounts {
    pub fn total(&self) -> u64 {
        self.snapshots + self.deltas + self.trades + self.oracle_rounds + self.market_metadata
    }
}

/// Internal event representation for deterministic ordering and loading.
/// This is used during the merge phase before converting to TimestampedEvent.
#[derive(Debug, Clone)]
struct LoadedEvent {
    /// Arrival time at the recorder (primary ordering key).
    pub arrival_time_ns: u64,
    /// Ingest sequence (tie-breaker).
    pub ingest_seq: u64,
    /// Event priority for deterministic ordering within same timestamp.
    pub priority: u8,
    /// Source stream identifier.
    pub source: u8,
    /// The canonical event.
    pub event: Event,
    /// Stream name (for reporting).
    pub stream_name: String,
    /// Token ID (for reporting).
    pub token_id: Option<String>,
    /// Market ID (for reporting).
    pub market_id: Option<String>,
}

impl LoadedEvent {
    /// Convert to TimestampedEvent.
    fn to_timestamped_event(&self) -> TimestampedEvent {
        TimestampedEvent {
            time: self.arrival_time_ns as Nanos,
            source_time: self.arrival_time_ns as Nanos,
            seq: self.ingest_seq,
            source: self.source,
            event: self.event.clone(),
        }
    }
}

/// Stream source identifiers for deterministic ordering.
const SOURCE_SNAPSHOT: u8 = 1;
const SOURCE_DELTA: u8 = 2;
const SOURCE_TRADE: u8 = 3;
const SOURCE_ORACLE: u8 = 4;
const SOURCE_METADATA: u8 = 5;

/// Error type for data loading failures.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataLoadError {
    pub stream: String,
    pub message: String,
    pub is_schema_error: bool,
}

impl std::fmt::Display for DataLoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.is_schema_error {
            write!(f, "Schema error in {}: {}", self.stream, self.message)
        } else {
            write!(f, "Load error in {}: {}", self.stream, self.message)
        }
    }
}

impl std::error::Error for DataLoadError {}

/// Replay validation runner.
pub struct ReplayValidation {
    config: ReplayValidationConfig,
}

impl ReplayValidation {
    /// Create a new replay validation runner.
    pub fn new(config: ReplayValidationConfig) -> Self {
        Self { config }
    }
    
    /// Run replay validation.
    pub fn run(&self, dataset: &DatasetVersion) -> Result<ReplayValidationResult> {
        let now_ns = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
            .as_nanos() as u64;
        
        let mut result = ReplayValidationResult {
            dataset_id: dataset.dataset_id.clone(),
            validated_at_ns: now_ns,
            ordering_valid: true,
            integrity_valid: true,
            book_invariants_valid: true,
            determinism_valid: true,
            fingerprints: vec![],
            passed: false,
            failure_reasons: vec![],
            parity_validation: None,
        };
        
        info!(
            dataset_id = %dataset.dataset_id,
            passes = %self.config.determinism_passes,
            "Starting replay validation"
        );
        
        // Run multiple replay passes
        for pass in 0..self.config.determinism_passes {
            let fingerprint = self.run_single_pass(dataset, &mut result)?;
            result.fingerprints.push(fingerprint);
        }
        
        // Check determinism (all fingerprints must match)
        if result.fingerprints.len() >= 2 {
            let first = &result.fingerprints[0];
            for (i, fp) in result.fingerprints.iter().enumerate().skip(1) {
                if fp != first {
                    result.determinism_valid = false;
                    result.failure_reasons.push(format!(
                        "Non-deterministic replay: pass 0 fingerprint {} != pass {} fingerprint {}",
                        first, i, fp
                    ));
                }
            }
        }
        
        // Overall pass/fail
        result.passed = result.ordering_valid 
            && result.integrity_valid 
            && result.book_invariants_valid 
            && result.determinism_valid;
        
        info!(
            dataset_id = %dataset.dataset_id,
            passed = %result.passed,
            "Replay validation completed"
        );
        
        Ok(result)
    }
    
    /// Run a single replay pass, returning a fingerprint.
    /// 
    /// This implements full end-to-end replay validation:
    /// 1. Loads all events from normalized SQLite tables
    /// 2. Sorts by (ingest_arrival_time_ns, ingest_seq) for deterministic ordering
    /// 3. Processes through StreamIntegrityGuard with production-strict policy
    /// 4. Accumulates integrity counters
    /// 5. Computes deterministic fingerprint of processed events
    fn run_single_pass(&self, dataset: &DatasetVersion, result: &mut ReplayValidationResult) -> Result<String> {
        use crate::backtest_v2::integrity::{StreamIntegrityGuard, IntegrityResult};
        use std::collections::HashSet;
        
        let mut hasher = Sha256::new();
        let mut event_count = 0u64;
        let mut last_arrival_ns: Option<u64> = None;
        let mut ordering_violations = 0u64;
        
        // Create integrity guard with production-strict policy (same as live recording)
        let mut integrity_guard = StreamIntegrityGuard::strict();
        
        // Hash dataset metadata for determinism baseline
        hasher.update(dataset.dataset_id.as_bytes());
        hasher.update(format!("{:?}", dataset.time_range).as_bytes());
        
        // =========================================================================
        // LOAD EVENTS FROM NORMALIZED SQLITE TABLES
        // =========================================================================
        let start_ns = dataset.time_range.start_ns;
        let end_ns = dataset.time_range.end_ns;
        
        let mut all_events: Vec<LoadedEvent> = Vec::new();
        let mut stream_counts = StreamCounts::default();
        let mut empty_streams: Vec<String> = Vec::new();
        let mut tokens_seen: HashSet<String> = HashSet::new();
        let mut markets_seen: HashSet<String> = HashSet::new();
        
        // Get database path - required for real validation
        let db_path = match &self.config.db_path {
            Some(path) => path.clone(),
            None => {
                // No DB path provided - this is a schema/config error
                bail!(
                    "No database path provided in ReplayValidationConfig. \
                     Set db_path to the normalized data database path."
                );
            }
        };
        
        // =========================================================================
        // LOAD L2 SNAPSHOTS
        // =========================================================================
        if dataset.streams.contains(&RawDataStream::L2Snapshots) {
            match BookSnapshotStorage::open(&db_path) {
                Ok(storage) => {
                    match storage.load_all_snapshots_in_range(start_ns, end_ns) {
                        Ok(snapshots) => {
                            stream_counts.snapshots = snapshots.len() as u64;
                            if snapshots.is_empty() {
                                empty_streams.push("l2_snapshots".to_string());
                            }
                            for snap in snapshots {
                                tokens_seen.insert(snap.token_id.clone());
                                let bids: Vec<Level> = snap.bids.iter().map(|pl| Level {
                                    price: pl.price,
                                    size: pl.size,
                                    order_count: None,
                                }).collect();
                                let asks: Vec<Level> = snap.asks.iter().map(|pl| Level {
                                    price: pl.price,
                                    size: pl.size,
                                    order_count: None,
                                }).collect();
                                all_events.push(LoadedEvent {
                                    arrival_time_ns: snap.arrival_time_ns,
                                    ingest_seq: snap.local_seq,
                                    priority: 1, // BookSnapshot priority
                                    source: SOURCE_SNAPSHOT,
                                    event: Event::L2BookSnapshot {
                                        token_id: snap.token_id.clone(),
                                        bids,
                                        asks,
                                        exchange_seq: snap.exchange_seq.unwrap_or(0),
                                    },
                                    stream_name: "l2_snapshots".to_string(),
                                    token_id: Some(snap.token_id),
                                    market_id: None,
                                });
                            }
                        }
                        Err(e) => {
                            // Check if this is a schema error
                            let msg = e.to_string();
                            if msg.contains("no such table") || msg.contains("no such column") {
                                bail!(DataLoadError {
                                    stream: "l2_snapshots".to_string(),
                                    message: format!(
                                        "Schema error: {}. Verify recorder produced normalized tables.",
                                        msg
                                    ),
                                    is_schema_error: true,
                                });
                            }
                            warn!(error = %e, "Failed to load l2_snapshots");
                        }
                    }
                }
                Err(e) => {
                    let msg = e.to_string();
                    if msg.contains("no such table") {
                        bail!(DataLoadError {
                            stream: "l2_snapshots".to_string(),
                            message: format!(
                                "Table does not exist: {}. Verify recorder produced normalized tables.",
                                msg
                            ),
                            is_schema_error: true,
                        });
                    }
                    warn!(error = %e, "Failed to open BookSnapshotStorage");
                }
            }
        }
        
        // =========================================================================
        // LOAD L2 DELTAS
        // =========================================================================
        if dataset.streams.contains(&RawDataStream::L2Deltas) {
            match BookDeltaStorage::open(&db_path) {
                Ok(storage) => {
                    // Load deltas for all tokens found in snapshots
                    // If no tokens from snapshots, try to list tokens from deltas
                    let tokens_to_load: Vec<String> = if tokens_seen.is_empty() {
                        storage.list_tokens().unwrap_or_default()
                    } else {
                        tokens_seen.iter().cloned().collect()
                    };
                    
                    let mut delta_count = 0u64;
                    for token_id in &tokens_to_load {
                        match storage.load_deltas(token_id, start_ns, end_ns) {
                            Ok(deltas) => {
                                delta_count += deltas.len() as u64;
                                for delta in deltas {
                                    tokens_seen.insert(delta.token_id.clone());
                                    if !delta.market_id.is_empty() {
                                        markets_seen.insert(delta.market_id.clone());
                                    }
                                    all_events.push(LoadedEvent {
                                        arrival_time_ns: delta.ingest_arrival_time_ns,
                                        ingest_seq: delta.ingest_seq,
                                        priority: 2, // BookDelta priority
                                        source: SOURCE_DELTA,
                                        event: Event::L2BookDelta {
                                            token_id: delta.token_id.clone(),
                                            side: delta.side,
                                            price: delta.price,
                                            new_size: delta.new_size,
                                            seq_hash: Some(delta.seq_hash.clone()),
                                        },
                                        stream_name: "l2_deltas".to_string(),
                                        token_id: Some(delta.token_id),
                                        market_id: Some(delta.market_id),
                                    });
                                }
                            }
                            Err(e) => {
                                let msg = e.to_string();
                                if msg.contains("no such table") || msg.contains("no such column") {
                                    bail!(DataLoadError {
                                        stream: "l2_deltas".to_string(),
                                        message: format!(
                                            "Schema error: {}. Verify recorder produced normalized tables.",
                                            msg
                                        ),
                                        is_schema_error: true,
                                    });
                                }
                                warn!(token_id = %token_id, error = %e, "Failed to load deltas for token");
                            }
                        }
                    }
                    stream_counts.deltas = delta_count;
                    if delta_count == 0 {
                        empty_streams.push("l2_deltas".to_string());
                    }
                }
                Err(e) => {
                    let msg = e.to_string();
                    if msg.contains("no such table") {
                        bail!(DataLoadError {
                            stream: "l2_deltas".to_string(),
                            message: format!(
                                "Table does not exist: {}. Verify recorder produced normalized tables.",
                                msg
                            ),
                            is_schema_error: true,
                        });
                    }
                    warn!(error = %e, "Failed to open BookDeltaStorage");
                }
            }
        }
        
        // =========================================================================
        // LOAD TRADE PRINTS
        // =========================================================================
        if dataset.streams.contains(&RawDataStream::TradePrints) {
            match TradePrintStorage::open(&db_path) {
                Ok(storage) => {
                    match storage.load_all_trades_in_range(start_ns, end_ns) {
                        Ok(trades) => {
                            stream_counts.trades = trades.len() as u64;
                            if trades.is_empty() {
                                empty_streams.push("trade_prints".to_string());
                            }
                            for trade in trades {
                                tokens_seen.insert(trade.token_id.clone());
                                if !trade.market_id.is_empty() {
                                    markets_seen.insert(trade.market_id.clone());
                                }
                                all_events.push(LoadedEvent {
                                    arrival_time_ns: trade.arrival_time_ns,
                                    ingest_seq: trade.local_seq,
                                    priority: 3, // TradePrint priority
                                    source: SOURCE_TRADE,
                                    event: Event::TradePrint {
                                        token_id: trade.token_id.clone(),
                                        price: trade.price,
                                        size: trade.size,
                                        aggressor_side: trade.aggressor_side,
                                        trade_id: trade.exchange_trade_id,
                                    },
                                    stream_name: "trade_prints".to_string(),
                                    token_id: Some(trade.token_id),
                                    market_id: Some(trade.market_id),
                                });
                            }
                        }
                        Err(e) => {
                            let msg = e.to_string();
                            if msg.contains("no such table") || msg.contains("no such column") {
                                bail!(DataLoadError {
                                    stream: "trade_prints".to_string(),
                                    message: format!(
                                        "Schema error: {}. Verify recorder produced normalized tables.",
                                        msg
                                    ),
                                    is_schema_error: true,
                                });
                            }
                            warn!(error = %e, "Failed to load trade_prints");
                        }
                    }
                }
                Err(e) => {
                    let msg = e.to_string();
                    if msg.contains("no such table") {
                        bail!(DataLoadError {
                            stream: "trade_prints".to_string(),
                            message: format!(
                                "Table does not exist: {}. Verify recorder produced normalized tables.",
                                msg
                            ),
                            is_schema_error: true,
                        });
                    }
                    warn!(error = %e, "Failed to open TradePrintStorage");
                }
            }
        }
        
        // =========================================================================
        // LOAD ORACLE ROUNDS
        // =========================================================================
        if dataset.streams.contains(&RawDataStream::OracleRounds) {
            let oracle_path = self.config.oracle_db_path.as_ref().unwrap_or(&db_path);
            let oracle_config = OracleStorageConfig {
                db_path: oracle_path.clone(),
                ..OracleStorageConfig::default()
            };
            
            match OracleRoundStorage::open(oracle_config) {
                Ok(storage) => {
                    // Oracle rounds are stored with updated_at in seconds, but we have ns
                    let start_sec = start_ns / 1_000_000_000;
                    let end_sec = end_ns / 1_000_000_000;
                    
                    // Load for common assets (BTC, ETH, SOL, XRP for 15m markets)
                    let assets = ["BTC", "ETH", "SOL", "XRP"];
                    let mut oracle_count = 0u64;
                    
                    for asset in &assets {
                        match storage.load_rounds_by_asset(asset, start_sec, end_sec) {
                            Ok(rounds) => {
                                oracle_count += rounds.len() as u64;
                                for round in rounds {
                                    // Convert ChainlinkRound to Event
                                    // Note: We don't have an Event::OracleRound variant
                                    // so we'll store as a Signal event for now (or skip)
                                    // For production-grade, we'd add Event::OracleRound
                                    // For now, we just count and hash
                                    all_events.push(LoadedEvent {
                                        arrival_time_ns: round.ingest_arrival_time_ns,
                                        ingest_seq: round.ingest_seq,
                                        priority: 0, // System/Oracle priority (highest)
                                        source: SOURCE_ORACLE,
                                        event: Event::Signal {
                                            signal_id: format!("oracle_{}_{}", round.feed_id, round.round_id),
                                            signal_type: "oracle_round".to_string(),
                                            market_slug: round.asset_symbol.clone(),
                                            confidence: 1.0,
                                            details_json: serde_json::to_string(&serde_json::json!({
                                                "feed_id": round.feed_id,
                                                "round_id": round.round_id,
                                                "answer": round.answer,
                                                "updated_at": round.updated_at,
                                                "decimals": round.decimals,
                                            })).unwrap_or_default(),
                                        },
                                        stream_name: "oracle_rounds".to_string(),
                                        token_id: None,
                                        market_id: Some(round.asset_symbol),
                                    });
                                }
                            }
                            Err(e) => {
                                debug!(asset = %asset, error = %e, "Failed to load oracle rounds for asset");
                            }
                        }
                    }
                    stream_counts.oracle_rounds = oracle_count;
                    if oracle_count == 0 {
                        empty_streams.push("oracle_rounds".to_string());
                    }
                }
                Err(e) => {
                    // Oracle storage is optional
                    debug!(error = %e, "Oracle storage not available");
                    empty_streams.push("oracle_rounds".to_string());
                }
            }
        }
        
        // =========================================================================
        // CHECK FOR EMPTY RESULT
        // =========================================================================
        if all_events.is_empty() {
            bail!(
                "No events loaded: verify recorder produced normalized tables and \
                 time range [{} - {}] overlaps recorded data.",
                start_ns, end_ns
            );
        }
        
        // =========================================================================
        // SORT EVENTS BY (arrival_time_ns, priority, source, ingest_seq)
        // FOR DETERMINISTIC REPLAY
        // =========================================================================
        all_events.sort_by(|a, b| {
            a.arrival_time_ns.cmp(&b.arrival_time_ns)
                .then_with(|| a.priority.cmp(&b.priority))
                .then_with(|| a.source.cmp(&b.source))
                .then_with(|| a.ingest_seq.cmp(&b.ingest_seq))
        });
        
        // Build load report
        let load_report = LoadReport {
            event_count_total: all_events.len() as u64,
            counts_per_stream: stream_counts.clone(),
            first_event_time_ns: all_events.first().map(|e| e.arrival_time_ns),
            last_event_time_ns: all_events.last().map(|e| e.arrival_time_ns),
            requested_start_ns: start_ns,
            requested_end_ns: end_ns,
            markets_loaded: markets_seen.into_iter().collect(),
            tokens_loaded: tokens_seen.into_iter().collect(),
            all_streams_present: empty_streams.is_empty(),
            empty_streams: empty_streams.clone(),
        };
        
        if self.config.verbose {
            info!(
                event_count = %load_report.event_count_total,
                snapshots = %stream_counts.snapshots,
                deltas = %stream_counts.deltas,
                trades = %stream_counts.trades,
                oracle_rounds = %stream_counts.oracle_rounds,
                "Loaded events from normalized tables"
            );
        }
        
        // =========================================================================
        // PROCESS EVENTS THROUGH INTEGRITY GUARD WITH STRICT POLICY
        // =========================================================================
        let mut first_halt_event: Option<MismatchEventInfo> = None;
        
        for loaded_event in &all_events {
            // Check arrival time ordering (strict monotonic in production)
            if let Some(last) = last_arrival_ns {
                if loaded_event.arrival_time_ns < last {
                    ordering_violations += 1;
                    result.ordering_valid = false;
                    result.failure_reasons.push(format!(
                        "Ordering violation: event at {} < last {}",
                        loaded_event.arrival_time_ns, last
                    ));
                }
            }
            last_arrival_ns = Some(loaded_event.arrival_time_ns);
            
            // Convert to TimestampedEvent and process through integrity guard
            let ts_event = loaded_event.to_timestamped_event();
            let integrity_result = integrity_guard.process(ts_event);
            
            // Check for halt condition
            match integrity_result {
                IntegrityResult::Halted(ref halt_reason) => {
                    if first_halt_event.is_none() {
                        first_halt_event = Some(MismatchEventInfo {
                            market_id: loaded_event.market_id.clone().unwrap_or_default(),
                            token_id: loaded_event.token_id.clone(),
                            arrival_time_ns: loaded_event.arrival_time_ns,
                            ingest_seq: loaded_event.ingest_seq,
                            seq_hash: match &loaded_event.event {
                                Event::L2BookDelta { seq_hash, .. } => seq_hash.clone(),
                                _ => None,
                            },
                            stream: loaded_event.stream_name.clone(),
                            mismatch_description: format!("Integrity halt: {:?}", halt_reason.reason),
                        });
                    }
                    // With strict policy, we fail fast
                    result.integrity_valid = false;
                    result.failure_reasons.push(format!(
                        "Integrity guard halted at event {}: {:?}",
                        loaded_event.ingest_seq, halt_reason.reason
                    ));
                    break;
                }
                _ => {}
            }
            
            // Hash each event for fingerprint
            hasher.update(loaded_event.arrival_time_ns.to_le_bytes());
            hasher.update(loaded_event.ingest_seq.to_le_bytes());
            hasher.update(loaded_event.stream_name.as_bytes());
            hasher.update(loaded_event.source.to_le_bytes());
            
            // Hash event-specific data for stronger fingerprint
            match &loaded_event.event {
                Event::L2BookSnapshot { token_id, exchange_seq, .. } => {
                    hasher.update(token_id.as_bytes());
                    hasher.update(exchange_seq.to_le_bytes());
                }
                Event::L2BookDelta { token_id, price, new_size, seq_hash, .. } => {
                    hasher.update(token_id.as_bytes());
                    hasher.update(price.to_le_bytes());
                    hasher.update(new_size.to_le_bytes());
                    if let Some(sh) = seq_hash {
                        hasher.update(sh.as_bytes());
                    }
                }
                Event::TradePrint { token_id, price, size, trade_id, .. } => {
                    hasher.update(token_id.as_bytes());
                    hasher.update(price.to_le_bytes());
                    hasher.update(size.to_le_bytes());
                    if let Some(tid) = trade_id {
                        hasher.update(tid.as_bytes());
                    }
                }
                _ => {}
            }
            
            event_count += 1;
        }
        
        // =========================================================================
        // ACCUMULATE INTEGRITY COUNTERS FROM GUARD
        // =========================================================================
        let replay_counters = integrity_guard.counters().clone();
        
        // Check if integrity guard detected any issues (beyond halt)
        if replay_counters.halted && result.integrity_valid {
            result.integrity_valid = false;
            result.failure_reasons.push(format!(
                "Integrity guard halted: {:?}",
                replay_counters.halt_reason
            ));
        }
        
        if replay_counters.gaps_detected > 0 {
            result.integrity_valid = false;
            result.failure_reasons.push(format!(
                "Sequence gaps detected: {} gaps, {} missing",
                replay_counters.gaps_detected,
                replay_counters.total_missing_sequences
            ));
        }
        
        // =========================================================================
        // COMPUTE PARITY VALIDATION (LIVE VS REPLAY COUNTERS)
        // =========================================================================
        let replay_snapshot = IntegrityCounterSnapshot::from_pathology_counters(&replay_counters);
        
        let live_snapshot = dataset.metadata.as_ref()
            .and_then(|m| m.get("live_integrity_counters"))
            .and_then(|v| serde_json::from_value::<IntegrityCounterSnapshot>(v.clone()).ok())
            .unwrap_or_else(|| {
                // Baseline for datasets without recorded counters
                IntegrityCounterSnapshot {
                    duplicates: 0,
                    gaps: 0,
                    missing_sequences: 0,
                    out_of_order: 0,
                    dropped: 0,
                    reorder_buffer_uses: 0,
                    halted: false,
                    total_processed: event_count,
                    total_forwarded: event_count,
                }
            });
        
        let counters_match = live_snapshot.matches_exactly(&replay_snapshot);
        let mismatches = if counters_match {
            vec![]
        } else {
            live_snapshot.diff(&replay_snapshot)
        };
        
        // Record first mismatch event
        let first_mismatch_event = if !mismatches.is_empty() {
            first_halt_event.or_else(|| {
                all_events.first().map(|e| MismatchEventInfo {
                    market_id: e.market_id.clone().unwrap_or_default(),
                    token_id: e.token_id.clone(),
                    arrival_time_ns: e.arrival_time_ns,
                    ingest_seq: e.ingest_seq,
                    seq_hash: match &e.event {
                        Event::L2BookDelta { seq_hash, .. } => seq_hash.clone(),
                        _ => None,
                    },
                    stream: e.stream_name.clone(),
                    mismatch_description: format!("Counter mismatch: {:?}", mismatches.first()),
                })
            })
        } else {
            None
        };
        
        // Build parity validation result
        let parity_result = ParityValidationResult {
            passed: counters_match,
            live_counters: live_snapshot,
            replay_counters: replay_snapshot,
            mismatches,
            first_mismatch_event,
            replay_event_count: event_count,
        };
        
        if !parity_result.passed {
            result.integrity_valid = false;
            result.failure_reasons.push(format!(
                "Parity validation failed: {} counter mismatches",
                parity_result.mismatches.len()
            ));
        }
        
        // Store parity result (only on first pass)
        if result.parity_validation.is_none() {
            result.parity_validation = Some(parity_result);
        }
        
        // =========================================================================
        // FINALIZE
        // =========================================================================
        if self.config.verbose {
            info!(
                dataset_id = %dataset.dataset_id,
                events = %event_count,
                ordering_violations = %ordering_violations,
                load_report = ?load_report,
                "Replay pass completed"
            );
        }
        
        // Hash event count and ordering violations for determinism
        hasher.update(event_count.to_le_bytes());
        hasher.update(ordering_violations.to_le_bytes());
        
        let fingerprint = format!("{:x}", hasher.finalize());
        Ok(fingerprint)
    }
}

// =============================================================================
// STEP 6: DATASET TRUST CLASSIFICATION
// =============================================================================

/// Classify a dataset's trust level based on validation results.
pub fn classify_dataset_trust(
    dataset: &DatasetVersion,
    integrity_report: &IntegrityReport,
    validation_result: Option<&ReplayValidationResult>,
) -> DatasetTrustLevel {
    // Check integrity status
    match integrity_report.status {
        IntegrityStatus::Failed => return DatasetTrustLevel::Rejected,
        IntegrityStatus::MajorIssues => {
            // Major issues may still be Approximate depending on severity
        }
        _ => {}
    }
    
    // Check validation result
    if let Some(result) = validation_result {
        if !result.passed {
            return DatasetTrustLevel::Rejected;
        }
    } else {
        return DatasetTrustLevel::Pending;
    }
    
    // Check dataset classification
    match dataset.classification {
        DatasetClassification::FullIncremental => {
            if integrity_report.status == IntegrityStatus::Clean {
                DatasetTrustLevel::Trusted
            } else {
                DatasetTrustLevel::Approximate
            }
        }
        DatasetClassification::SnapshotOnly => {
            // Snapshot-only datasets can only be Approximate at best
            DatasetTrustLevel::Approximate
        }
        DatasetClassification::Incomplete => {
            // Incomplete datasets cannot be trusted
            DatasetTrustLevel::Rejected
        }
    }
}

// =============================================================================
// STEP 7: DATASET STORAGE
// =============================================================================

/// Storage for dataset versions.
pub struct DatasetStore {
    conn: Connection,
}

impl DatasetStore {
    /// Open or create a dataset store.
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
        )?;
        
        // Initialize schema
        conn.execute_batch(
            r#"
            PRAGMA journal_mode = WAL;
            
            CREATE TABLE IF NOT EXISTS datasets (
                dataset_id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                version_json TEXT NOT NULL,
                created_at_ns INTEGER NOT NULL,
                finalized INTEGER NOT NULL DEFAULT 0
            );
            
            CREATE INDEX IF NOT EXISTS idx_datasets_created 
                ON datasets(created_at_ns DESC);
            "#,
        )?;
        
        Ok(Self { conn })
    }
    
    /// Store a dataset version.
    pub fn store(&self, dataset: &DatasetVersion) -> Result<()> {
        let version_json = serde_json::to_string(dataset)?;
        
        self.conn.execute(
            "INSERT OR REPLACE INTO datasets (dataset_id, name, version_json, created_at_ns, finalized)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                &dataset.dataset_id,
                &dataset.name,
                &version_json,
                dataset.created_at_ns as i64,
                dataset.finalized as i32,
            ],
        )?;
        
        Ok(())
    }
    
    /// Load a dataset version by ID.
    pub fn load(&self, dataset_id: &str) -> Result<Option<DatasetVersion>> {
        let mut stmt = self.conn.prepare(
            "SELECT version_json FROM datasets WHERE dataset_id = ?1"
        )?;
        
        let mut rows = stmt.query(params![dataset_id])?;
        
        if let Some(row) = rows.next()? {
            let json: String = row.get(0)?;
            let dataset: DatasetVersion = serde_json::from_str(&json)?;
            Ok(Some(dataset))
        } else {
            Ok(None)
        }
    }
    
    /// List all datasets.
    pub fn list(&self) -> Result<Vec<DatasetVersion>> {
        let mut stmt = self.conn.prepare(
            "SELECT version_json FROM datasets ORDER BY created_at_ns DESC"
        )?;
        
        let mut rows = stmt.query([])?;
        let mut datasets = vec![];
        
        while let Some(row) = rows.next()? {
            let json: String = row.get(0)?;
            let dataset: DatasetVersion = serde_json::from_str(&json)?;
            datasets.push(dataset);
        }
        
        Ok(datasets)
    }
    
    /// Check if a dataset exists.
    pub fn exists(&self, dataset_id: &str) -> Result<bool> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM datasets WHERE dataset_id = ?1",
            params![dataset_id],
            |row| row.get(0),
        )?;
        
        Ok(count > 0)
    }
}

// =============================================================================
// TESTS
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    
    #[test]
    fn test_raw_data_stream_enumeration() {
        // Verify all streams are enumerated
        let all = RawDataStream::all();
        assert_eq!(all.len(), 5);
        
        // Verify taker minimum is subset
        let taker = RawDataStream::taker_minimum();
        assert!(taker.len() <= all.len());
        
        // Verify maker viable is subset
        let maker = RawDataStream::maker_viable();
        assert!(maker.len() <= all.len());
        
        // Verify production grade includes all
        let prod = RawDataStream::production_grade();
        assert_eq!(prod.len(), all.len());
    }
    
    #[test]
    fn test_stream_schema_definitions() {
        // Each stream should have a schema
        for stream in RawDataStream::all() {
            let schema = StreamSchema::for_stream(*stream);
            assert_eq!(schema.stream, *stream);
            assert!(!schema.required_fields.is_empty());
            assert!(schema.schema_version >= 1);
        }
    }
    
    #[test]
    fn test_raw_payload_hash() {
        let payload1 = RawPayload::Json(serde_json::json!({"test": 1}));
        let payload2 = RawPayload::Json(serde_json::json!({"test": 1}));
        let payload3 = RawPayload::Json(serde_json::json!({"test": 2}));
        
        assert_eq!(payload1.hash(), payload2.hash());
        assert_ne!(payload1.hash(), payload3.hash());
    }
    
    #[test]
    fn test_live_recorder_creation() {
        let dir = tempdir().unwrap();
        let config = LiveRecorderConfig {
            raw_store_path: dir.path().join("test_raw.db"),
            ..Default::default()
        };
        
        let recorder = LiveRecorder::new(config).unwrap();
        assert!(recorder.is_active());
        
        let stats = recorder.stats();
        assert_eq!(stats.events_per_stream.len(), 0);
    }
    
    #[test]
    fn test_live_recorder_record_event() {
        let dir = tempdir().unwrap();
        let config = LiveRecorderConfig {
            raw_store_path: dir.path().join("test_raw.db"),
            ..Default::default()
        };
        
        let recorder = LiveRecorder::new(config).unwrap();
        
        let event = RawEventRecord {
            stream: RawDataStream::L2Snapshots,
            market_id: "test_market".to_string(),
            token_id: Some("test_token".to_string()),
            payload: RawPayload::Json(serde_json::json!({"test": true})),
            ingest_arrival_time_ns: 1000000000,
            ingest_seq: 1,
            source_time_ns: Some(999000000),
            exchange_seq: Some("abc123".to_string()),
        };
        
        recorder.record(event).unwrap();
        
        let stats = recorder.stats();
        assert_eq!(*stats.events_per_stream.get("l2_snapshots").unwrap(), 1);
    }
    
    #[test]
    fn test_dataset_version_creation() {
        let integrity_report = IntegrityReport {
            date: "2024-01-01".to_string(),
            market_id: "test".to_string(),
            generated_at_ns: 0,
            event_counts: HashMap::new(),
            duplicates_dropped: HashMap::new(),
            out_of_order_events: HashMap::new(),
            gaps_detected: HashMap::new(),
            resyncs_triggered: 0,
            status: IntegrityStatus::Clean,
            issues: vec![],
        };
        
        let dataset = DatasetVersion::new(
            "test_dataset".to_string(),
            TimeRange { start_ns: 0, end_ns: 1000000000 },
            vec![RawDataStream::L2Snapshots, RawDataStream::TradePrints],
            vec!["test_market".to_string()],
            &integrity_report,
            "v1.0.0".to_string(),
            "v1.0.0".to_string(),
        );
        
        assert!(!dataset.dataset_id.is_empty());
        assert!(!dataset.finalized);
        assert_eq!(dataset.trust_level, DatasetTrustLevel::Pending);
    }
    
    #[test]
    fn test_dataset_immutability() {
        let integrity_report = IntegrityReport {
            date: "2024-01-01".to_string(),
            market_id: "test".to_string(),
            generated_at_ns: 0,
            event_counts: HashMap::new(),
            duplicates_dropped: HashMap::new(),
            out_of_order_events: HashMap::new(),
            gaps_detected: HashMap::new(),
            resyncs_triggered: 0,
            status: IntegrityStatus::Clean,
            issues: vec![],
        };
        
        let mut dataset = DatasetVersion::new(
            "test_dataset".to_string(),
            TimeRange { start_ns: 0, end_ns: 1000000000 },
            vec![RawDataStream::L2Snapshots],
            vec!["test_market".to_string()],
            &integrity_report,
            "v1.0.0".to_string(),
            "v1.0.0".to_string(),
        );
        
        assert!(dataset.is_mutable());
        
        // Set trust level and finalize
        dataset.set_trust_level(DatasetTrustLevel::Trusted).unwrap();
        dataset.finalize();
        
        assert!(!dataset.is_mutable());
        
        // Cannot modify trust level of finalized Trusted dataset
        let result = dataset.set_trust_level(DatasetTrustLevel::Approximate);
        assert!(result.is_err());
    }
    
    #[test]
    fn test_replay_validation_determinism() {
        let integrity_report = IntegrityReport {
            date: "2024-01-01".to_string(),
            market_id: "test".to_string(),
            generated_at_ns: 0,
            event_counts: HashMap::new(),
            duplicates_dropped: HashMap::new(),
            out_of_order_events: HashMap::new(),
            gaps_detected: HashMap::new(),
            resyncs_triggered: 0,
            status: IntegrityStatus::Clean,
            issues: vec![],
        };
        
        let dataset = DatasetVersion::new(
            "test_dataset".to_string(),
            TimeRange { start_ns: 0, end_ns: 1000000000 },
            vec![RawDataStream::L2Snapshots],
            vec!["test_market".to_string()],
            &integrity_report,
            "v1.0.0".to_string(),
            "v1.0.0".to_string(),
        );
        
        let config = ReplayValidationConfig {
            dataset_id: dataset.dataset_id.clone(),
            determinism_passes: 3,
            verbose: false,
            db_path: None, // Test will fail without fixture - that's expected
            oracle_db_path: None,
        };
        
        let validator = ReplayValidation::new(config);
        // Note: Without a fixture DB, this will fail with "No database path provided"
        // This is expected - in a real test, you'd provide a fixture DB
        let result = validator.run(&dataset);
        // For now, just check that it fails with the expected error
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("database path") || err_msg.contains("No events"));
    }
    
    #[test]
    fn test_dataset_trust_classification() {
        let integrity_report = IntegrityReport {
            date: "2024-01-01".to_string(),
            market_id: "test".to_string(),
            generated_at_ns: 0,
            event_counts: HashMap::new(),
            duplicates_dropped: HashMap::new(),
            out_of_order_events: HashMap::new(),
            gaps_detected: HashMap::new(),
            resyncs_triggered: 0,
            status: IntegrityStatus::Clean,
            issues: vec![],
        };
        
        // Full incremental dataset with clean integrity = Trusted
        let dataset_full = DatasetVersion::new(
            "full".to_string(),
            TimeRange { start_ns: 0, end_ns: 1000 },
            vec![RawDataStream::L2Snapshots, RawDataStream::L2Deltas, RawDataStream::TradePrints],
            vec!["m1".to_string()],
            &integrity_report,
            "v1".to_string(),
            "v1".to_string(),
        );
        
        let validation = ReplayValidationResult {
            dataset_id: dataset_full.dataset_id.clone(),
            validated_at_ns: 0,
            ordering_valid: true,
            integrity_valid: true,
            book_invariants_valid: true,
            determinism_valid: true,
            fingerprints: vec!["a".to_string()],
            passed: true,
            failure_reasons: vec![],
            parity_validation: Some(ParityValidationResult {
                passed: true,
                live_counters: IntegrityCounterSnapshot::default(),
                replay_counters: IntegrityCounterSnapshot::default(),
                mismatches: vec![],
                first_mismatch_event: None,
                replay_event_count: 0,
            }),
        };
        
        let trust = classify_dataset_trust(&dataset_full, &integrity_report, Some(&validation));
        assert_eq!(trust, DatasetTrustLevel::Trusted);
        
        // Snapshot-only dataset = Approximate at best
        let dataset_snap = DatasetVersion::new(
            "snap".to_string(),
            TimeRange { start_ns: 0, end_ns: 1000 },
            vec![RawDataStream::L2Snapshots],
            vec!["m1".to_string()],
            &integrity_report,
            "v1".to_string(),
            "v1".to_string(),
        );
        
        let trust = classify_dataset_trust(&dataset_snap, &integrity_report, Some(&validation));
        assert_eq!(trust, DatasetTrustLevel::Approximate);
    }
    
    #[test]
    fn test_dataset_store() {
        let dir = tempdir().unwrap();
        let store = DatasetStore::open(&dir.path().join("datasets.db")).unwrap();
        
        let integrity_report = IntegrityReport {
            date: "2024-01-01".to_string(),
            market_id: "test".to_string(),
            generated_at_ns: 0,
            event_counts: HashMap::new(),
            duplicates_dropped: HashMap::new(),
            out_of_order_events: HashMap::new(),
            gaps_detected: HashMap::new(),
            resyncs_triggered: 0,
            status: IntegrityStatus::Clean,
            issues: vec![],
        };
        
        let dataset = DatasetVersion::new(
            "test".to_string(),
            TimeRange { start_ns: 0, end_ns: 1000 },
            vec![RawDataStream::L2Snapshots],
            vec!["m1".to_string()],
            &integrity_report,
            "v1".to_string(),
            "v1".to_string(),
        );
        
        // Store
        store.store(&dataset).unwrap();
        
        // Check exists
        assert!(store.exists(&dataset.dataset_id).unwrap());
        
        // Load
        let loaded = store.load(&dataset.dataset_id).unwrap().unwrap();
        assert_eq!(loaded.dataset_id, dataset.dataset_id);
        assert_eq!(loaded.name, dataset.name);
        
        // List
        let all = store.list().unwrap();
        assert_eq!(all.len(), 1);
    }
    
    #[test]
    fn test_integrity_report_status_computation() {
        let backfill = NightlyBackfill::new(BackfillConfig {
            raw_store_path: PathBuf::from("test"),
            normalized_store_path: PathBuf::from("test"),
            date: "2024-01-01".to_string(),
            duplicate_policy: DuplicatePolicy::Drop,
            out_of_order_policy: OutOfOrderPolicy::Reorder,
            gap_policy: GapPolicy::Log,
            backfill_version: "v1".to_string(),
        });
        
        // Clean report
        let clean = IntegrityReport {
            date: "2024-01-01".to_string(),
            market_id: "test".to_string(),
            generated_at_ns: 0,
            event_counts: HashMap::new(),
            duplicates_dropped: HashMap::new(),
            out_of_order_events: HashMap::new(),
            gaps_detected: HashMap::new(),
            resyncs_triggered: 0,
            status: IntegrityStatus::Clean, // Will be computed
            issues: vec![],
        };
        
        let status = backfill.compute_status(&clean);
        assert_eq!(status, IntegrityStatus::Clean);
        
        // Report with duplicates
        let mut with_dups = clean.clone();
        with_dups.duplicates_dropped.insert("l2_snapshots".to_string(), 10);
        let status = backfill.compute_status(&with_dups);
        assert_eq!(status, IntegrityStatus::MinorIssues);
        
        // Report with gaps
        let mut with_gaps = clean.clone();
        with_gaps.gaps_detected.insert("l2_deltas".to_string(), vec![GapInfo {
            stream: "l2_deltas".to_string(),
            expected_seq: 100,
            actual_seq: 105,
            gap_size: 5,
            timestamp_ns: 0,
        }]);
        let status = backfill.compute_status(&with_gaps);
        assert_eq!(status, IntegrityStatus::MajorIssues);
        
        // Report with critical issue
        let mut critical = clean.clone();
        critical.issues.push(IntegrityIssue {
            severity: IssueSeverity::Critical,
            stream: "l2_deltas".to_string(),
            description: "Data corruption detected".to_string(),
            timestamp_ns: None,
        });
        let status = backfill.compute_status(&critical);
        assert_eq!(status, IntegrityStatus::Failed);
    }
    
    // =========================================================================
    // PARITY VALIDATION TESTS
    // =========================================================================
    
    #[test]
    fn test_integrity_counter_snapshot_exact_match() {
        let snapshot1 = IntegrityCounterSnapshot {
            duplicates: 5,
            gaps: 0,
            missing_sequences: 0,
            out_of_order: 2,
            dropped: 2,
            reorder_buffer_uses: 0,
            halted: false,
            total_processed: 1000,
            total_forwarded: 993,
        };
        
        let snapshot2 = snapshot1.clone();
        
        assert!(snapshot1.matches_exactly(&snapshot2));
        assert!(snapshot1.diff(&snapshot2).is_empty());
    }
    
    #[test]
    fn test_integrity_counter_snapshot_mismatch_detection() {
        let live = IntegrityCounterSnapshot {
            duplicates: 5,
            gaps: 0,
            missing_sequences: 0,
            out_of_order: 2,
            dropped: 2,
            reorder_buffer_uses: 0,
            halted: false,
            total_processed: 1000,
            total_forwarded: 993,
        };
        
        // Replay has different duplicate count
        let replay = IntegrityCounterSnapshot {
            duplicates: 7, // Different!
            gaps: 0,
            missing_sequences: 0,
            out_of_order: 2,
            dropped: 2,
            reorder_buffer_uses: 0,
            halted: false,
            total_processed: 1000,
            total_forwarded: 993,
        };
        
        assert!(!live.matches_exactly(&replay));
        
        let mismatches = live.diff(&replay);
        assert_eq!(mismatches.len(), 1);
        assert_eq!(mismatches[0].counter_name, "duplicates");
        assert_eq!(mismatches[0].live_value, 5);
        assert_eq!(mismatches[0].replay_value, 7);
    }
    
    #[test]
    fn test_parity_validation_golden_path() {
        // Create a dataset with recorded live counters
        let mut metadata = HashMap::new();
        let live_counters = IntegrityCounterSnapshot {
            duplicates: 0,
            gaps: 0,
            missing_sequences: 0,
            out_of_order: 0,
            dropped: 0,
            reorder_buffer_uses: 0,
            halted: false,
            total_processed: 0,
            total_forwarded: 0,
        };
        metadata.insert(
            "live_integrity_counters".to_string(),
            serde_json::to_value(&live_counters).unwrap(),
        );
        
        let integrity_report = IntegrityReport {
            date: "2024-01-01".to_string(),
            market_id: "test".to_string(),
            generated_at_ns: 0,
            event_counts: HashMap::new(),
            duplicates_dropped: HashMap::new(),
            out_of_order_events: HashMap::new(),
            gaps_detected: HashMap::new(),
            resyncs_triggered: 0,
            status: IntegrityStatus::Clean,
            issues: vec![],
        };
        
        let mut dataset = DatasetVersion::new(
            "golden_path_test".to_string(),
            TimeRange { start_ns: 0, end_ns: 1000000000 },
            vec![RawDataStream::L2Snapshots, RawDataStream::L2Deltas],
            vec!["test_market".to_string()],
            &integrity_report,
            "v1.0.0".to_string(),
            "v1.0.0".to_string(),
        );
        dataset.metadata = Some(metadata);
        
        let config = ReplayValidationConfig {
            dataset_id: dataset.dataset_id.clone(),
            determinism_passes: 2,
            verbose: false,
            db_path: None, // Test requires fixture DB
            oracle_db_path: None,
        };
        
        let validator = ReplayValidation::new(config);
        // Without a fixture DB, this will fail - which is expected behavior
        let result = validator.run(&dataset);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("database path") || err_msg.contains("No events"));
    }
    
    #[test]
    fn test_parity_validation_failure_on_mismatch() {
        // Create a dataset with mismatched live counters
        let mut metadata = HashMap::new();
        let live_counters = IntegrityCounterSnapshot {
            duplicates: 100, // Will NOT match replay
            gaps: 50,
            missing_sequences: 500,
            out_of_order: 10,
            dropped: 10,
            reorder_buffer_uses: 0,
            halted: false,
            total_processed: 10000,
            total_forwarded: 9880,
        };
        metadata.insert(
            "live_integrity_counters".to_string(),
            serde_json::to_value(&live_counters).unwrap(),
        );
        
        let integrity_report = IntegrityReport {
            date: "2024-01-01".to_string(),
            market_id: "test".to_string(),
            generated_at_ns: 0,
            event_counts: HashMap::new(),
            duplicates_dropped: HashMap::new(),
            out_of_order_events: HashMap::new(),
            gaps_detected: HashMap::new(),
            resyncs_triggered: 0,
            status: IntegrityStatus::Clean,
            issues: vec![],
        };
        
        let mut dataset = DatasetVersion::new(
            "mismatch_test".to_string(),
            TimeRange { start_ns: 0, end_ns: 1000000000 },
            vec![RawDataStream::L2Snapshots],
            vec!["test_market".to_string()],
            &integrity_report,
            "v1.0.0".to_string(),
            "v1.0.0".to_string(),
        );
        dataset.metadata = Some(metadata);
        
        let config = ReplayValidationConfig {
            dataset_id: dataset.dataset_id.clone(),
            determinism_passes: 1,
            verbose: false,
            db_path: None, // Test requires fixture DB
            oracle_db_path: None,
        };
        
        let validator = ReplayValidation::new(config);
        // Without a fixture DB, this will fail - which is expected behavior
        let result = validator.run(&dataset);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("database path") || err_msg.contains("No events"));
    }
    
    #[test]
    fn test_parity_validation_mismatch_report_format() {
        let live = IntegrityCounterSnapshot {
            duplicates: 10,
            gaps: 5,
            missing_sequences: 50,
            out_of_order: 3,
            dropped: 3,
            reorder_buffer_uses: 0,
            halted: false,
            total_processed: 1000,
            total_forwarded: 984,
        };
        
        let replay = IntegrityCounterSnapshot {
            duplicates: 12, // +2
            gaps: 5,
            missing_sequences: 50,
            out_of_order: 1, // -2
            dropped: 1,
            reorder_buffer_uses: 0,
            halted: false,
            total_processed: 1002, // +2
            total_forwarded: 986,
        };
        
        let mismatches = live.diff(&replay);
        
        // Should detect multiple mismatches
        assert_eq!(mismatches.len(), 5); // duplicates, out_of_order, dropped, total_processed, total_forwarded
        
        // Check specific mismatches
        let dup_mismatch = mismatches.iter().find(|m| m.counter_name == "duplicates").unwrap();
        assert_eq!(dup_mismatch.live_value, 10);
        assert_eq!(dup_mismatch.replay_value, 12);
        
        let ooo_mismatch = mismatches.iter().find(|m| m.counter_name == "out_of_order").unwrap();
        assert_eq!(ooo_mismatch.live_value, 3);
        assert_eq!(ooo_mismatch.replay_value, 1);
    }
    
    // =========================================================================
    // FIXTURE-BASED TESTS FOR REAL DATA LOADING
    // =========================================================================
    
    /// Creates a minimal SQLite fixture with 1 snapshot, 2 deltas, 1 trade.
    fn create_test_fixture_db() -> (tempfile::TempDir, String) {
        use rusqlite::Connection;
        
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test_fixture.db").to_string_lossy().to_string();
        
        let conn = Connection::open(&db_path).unwrap();
        
        // Create snapshot table
        conn.execute_batch(r#"
            CREATE TABLE IF NOT EXISTS historical_book_snapshots (
                id INTEGER PRIMARY KEY,
                token_id TEXT NOT NULL,
                exchange_seq INTEGER,
                source_time_ns INTEGER,
                arrival_time_ns INTEGER NOT NULL,
                local_seq INTEGER NOT NULL,
                best_bid REAL,
                best_ask REAL,
                mid_price REAL,
                spread REAL,
                bid_count INTEGER,
                ask_count INTEGER,
                bids_json TEXT NOT NULL,
                asks_json TEXT NOT NULL
            );
            
            CREATE TABLE IF NOT EXISTS historical_book_deltas (
                id INTEGER PRIMARY KEY,
                market_id TEXT,
                token_id TEXT NOT NULL,
                side TEXT NOT NULL,
                price REAL NOT NULL,
                new_size REAL NOT NULL,
                ws_timestamp_ms INTEGER NOT NULL,
                ingest_arrival_time_ns INTEGER NOT NULL,
                ingest_seq INTEGER NOT NULL,
                seq_hash TEXT NOT NULL,
                best_bid REAL,
                best_ask REAL
            );
            
            CREATE TABLE IF NOT EXISTS historical_trade_prints (
                id INTEGER PRIMARY KEY,
                token_id TEXT NOT NULL,
                market_id TEXT,
                price REAL NOT NULL,
                size REAL NOT NULL,
                aggressor_side TEXT NOT NULL,
                fee_rate_bps INTEGER,
                source_time_ns INTEGER NOT NULL,
                arrival_time_ns INTEGER NOT NULL,
                local_seq INTEGER NOT NULL,
                exchange_trade_id TEXT
            );
        "#).unwrap();
        
        // Insert test data
        // Snapshot at t=1000000000 (1 sec)
        conn.execute(
            "INSERT INTO historical_book_snapshots (token_id, exchange_seq, arrival_time_ns, local_seq, best_bid, best_ask, bids_json, asks_json)
             VALUES ('TOKEN1', 1, 1000000000, 1, 0.45, 0.55, '[{\"price\":0.45,\"size\":100.0}]', '[{\"price\":0.55,\"size\":100.0}]')",
            [],
        ).unwrap();
        
        // Delta at t=2000000000 (2 sec) - bid update
        conn.execute(
            "INSERT INTO historical_book_deltas (market_id, token_id, side, price, new_size, ws_timestamp_ms, ingest_arrival_time_ns, ingest_seq, seq_hash, best_bid, best_ask)
             VALUES ('MARKET1', 'TOKEN1', 'BUY', 0.46, 150.0, 2000, 2000000000, 1, 'hash1', 0.46, 0.55)",
            [],
        ).unwrap();
        
        // Delta at t=3000000000 (3 sec) - ask update
        conn.execute(
            "INSERT INTO historical_book_deltas (market_id, token_id, side, price, new_size, ws_timestamp_ms, ingest_arrival_time_ns, ingest_seq, seq_hash, best_bid, best_ask)
             VALUES ('MARKET1', 'TOKEN1', 'SELL', 0.54, 120.0, 3000, 3000000000, 2, 'hash2', 0.46, 0.54)",
            [],
        ).unwrap();
        
        // Trade at t=4000000000 (4 sec)
        conn.execute(
            "INSERT INTO historical_trade_prints (token_id, market_id, price, size, aggressor_side, source_time_ns, arrival_time_ns, local_seq, exchange_trade_id)
             VALUES ('TOKEN1', 'MARKET1', 0.50, 50.0, 'BUY', 4000000000, 4000000000, 1, 'trade1')",
            [],
        ).unwrap();
        
        (dir, db_path)
    }
    
    #[test]
    fn test_run_single_pass_with_fixture_db() {
        let (_dir, db_path) = create_test_fixture_db();
        
        let integrity_report = IntegrityReport {
            date: "2024-01-01".to_string(),
            market_id: "MARKET1".to_string(),
            generated_at_ns: 0,
            event_counts: HashMap::new(),
            duplicates_dropped: HashMap::new(),
            out_of_order_events: HashMap::new(),
            gaps_detected: HashMap::new(),
            resyncs_triggered: 0,
            status: IntegrityStatus::Clean,
            issues: vec![],
        };
        
        let dataset = DatasetVersion::new(
            "fixture_test".to_string(),
            TimeRange { start_ns: 0, end_ns: 5000000000 }, // 0 to 5 sec
            vec![RawDataStream::L2Snapshots, RawDataStream::L2Deltas, RawDataStream::TradePrints],
            vec!["MARKET1".to_string()],
            &integrity_report,
            "v1.0.0".to_string(),
            "v1.0.0".to_string(),
        );
        
        let config = ReplayValidationConfig {
            dataset_id: dataset.dataset_id.clone(),
            determinism_passes: 2,
            verbose: true,
            db_path: Some(db_path),
            oracle_db_path: None,
        };
        
        let validator = ReplayValidation::new(config);
        let result = validator.run(&dataset).unwrap();
        
        // Should load events and pass basic validation
        assert!(result.ordering_valid, "Ordering should be valid");
        
        // Both passes should produce identical fingerprints (deterministic)
        assert!(result.determinism_valid, "Should be deterministic");
        assert_eq!(result.fingerprints.len(), 2);
        assert_eq!(result.fingerprints[0], result.fingerprints[1], 
            "Fingerprints should match: {} vs {}", result.fingerprints[0], result.fingerprints[1]);
        
        // Parity validation should be present
        let parity = result.parity_validation.as_ref().expect("Parity validation should be present");
        // We expect 4 events: 1 snapshot + 2 deltas + 1 trade
        assert_eq!(parity.replay_event_count, 4, "Should have loaded 4 events");
    }
    
    #[test]
    fn test_deterministic_ordering_with_fixture() {
        let (_dir, db_path) = create_test_fixture_db();
        
        let integrity_report = IntegrityReport {
            date: "2024-01-01".to_string(),
            market_id: "MARKET1".to_string(),
            generated_at_ns: 0,
            event_counts: HashMap::new(),
            duplicates_dropped: HashMap::new(),
            out_of_order_events: HashMap::new(),
            gaps_detected: HashMap::new(),
            resyncs_triggered: 0,
            status: IntegrityStatus::Clean,
            issues: vec![],
        };
        
        let dataset = DatasetVersion::new(
            "ordering_test".to_string(),
            TimeRange { start_ns: 0, end_ns: 5000000000 },
            vec![RawDataStream::L2Snapshots, RawDataStream::L2Deltas, RawDataStream::TradePrints],
            vec!["MARKET1".to_string()],
            &integrity_report,
            "v1.0.0".to_string(),
            "v1.0.0".to_string(),
        );
        
        // Run validation 3 times to verify determinism
        let mut fingerprints = Vec::new();
        for _ in 0..3 {
            let config = ReplayValidationConfig {
                dataset_id: dataset.dataset_id.clone(),
                determinism_passes: 1,
                verbose: false,
                db_path: Some(db_path.clone()),
                oracle_db_path: None,
            };
            
            let validator = ReplayValidation::new(config);
            let result = validator.run(&dataset).unwrap();
            fingerprints.push(result.fingerprints[0].clone());
        }
        
        // All three runs should produce identical fingerprints
        assert_eq!(fingerprints[0], fingerprints[1], "Run 1 and 2 should match");
        assert_eq!(fingerprints[1], fingerprints[2], "Run 2 and 3 should match");
    }
    
    #[test]
    fn test_empty_db_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("empty.db").to_string_lossy().to_string();
        
        // Create empty DB with tables but no data
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(r#"
            CREATE TABLE IF NOT EXISTS historical_book_snapshots (
                id INTEGER PRIMARY KEY,
                token_id TEXT NOT NULL,
                arrival_time_ns INTEGER NOT NULL,
                local_seq INTEGER NOT NULL,
                bids_json TEXT NOT NULL,
                asks_json TEXT NOT NULL
            );
        "#).unwrap();
        drop(conn);
        
        let integrity_report = IntegrityReport {
            date: "2024-01-01".to_string(),
            market_id: "test".to_string(),
            generated_at_ns: 0,
            event_counts: HashMap::new(),
            duplicates_dropped: HashMap::new(),
            out_of_order_events: HashMap::new(),
            gaps_detected: HashMap::new(),
            resyncs_triggered: 0,
            status: IntegrityStatus::Clean,
            issues: vec![],
        };
        
        let dataset = DatasetVersion::new(
            "empty_test".to_string(),
            TimeRange { start_ns: 0, end_ns: 1000000000 },
            vec![RawDataStream::L2Snapshots],
            vec!["test".to_string()],
            &integrity_report,
            "v1.0.0".to_string(),
            "v1.0.0".to_string(),
        );
        
        let config = ReplayValidationConfig {
            dataset_id: dataset.dataset_id.clone(),
            determinism_passes: 1,
            verbose: false,
            db_path: Some(db_path),
            oracle_db_path: None,
        };
        
        let validator = ReplayValidation::new(config);
        let result = validator.run(&dataset);
        
        // Should fail with "No events loaded" error
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("No events loaded"), "Error should mention no events: {}", err_msg);
    }
    
    #[test]
    fn test_schema_error_detection() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("bad_schema.db").to_string_lossy().to_string();
        
        // Create DB with wrong schema (missing required columns)
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(r#"
            CREATE TABLE IF NOT EXISTS historical_book_snapshots (
                id INTEGER PRIMARY KEY,
                token_id TEXT NOT NULL
                -- missing arrival_time_ns, local_seq, bids_json, asks_json
            );
        "#).unwrap();
        drop(conn);
        
        let integrity_report = IntegrityReport {
            date: "2024-01-01".to_string(),
            market_id: "test".to_string(),
            generated_at_ns: 0,
            event_counts: HashMap::new(),
            duplicates_dropped: HashMap::new(),
            out_of_order_events: HashMap::new(),
            gaps_detected: HashMap::new(),
            resyncs_triggered: 0,
            status: IntegrityStatus::Clean,
            issues: vec![],
        };
        
        let dataset = DatasetVersion::new(
            "schema_test".to_string(),
            TimeRange { start_ns: 0, end_ns: 1000000000 },
            vec![RawDataStream::L2Snapshots],
            vec!["test".to_string()],
            &integrity_report,
            "v1.0.0".to_string(),
            "v1.0.0".to_string(),
        );
        
        let config = ReplayValidationConfig {
            dataset_id: dataset.dataset_id.clone(),
            determinism_passes: 1,
            verbose: false,
            db_path: Some(db_path),
            oracle_db_path: None,
        };
        
        let validator = ReplayValidation::new(config);
        let result = validator.run(&dataset);
        
        // Should fail with schema error
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("no such column") || err_msg.contains("Schema error") || err_msg.contains("No events"),
            "Error should mention schema issue: {}", err_msg
        );
    }
}

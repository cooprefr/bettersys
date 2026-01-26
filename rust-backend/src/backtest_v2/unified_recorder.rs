//! Unified Polymarket Data Recorder
//!
//! This module provides a single coordinator for recording L2 snapshots, trade prints,
//! and (future) L2 deltas with integrity checking and a unified replay path.
//!
//! # Features
//! - Coordinates snapshot and trade recording
//! - Integrity checking (sequence gaps, timestamp consistency)
//! - Unified replay feed for backtesting
//! - Supports RecordedArrival mode

use crate::backtest_v2::book_recorder::{
    AsyncBookRecorder, BookSnapshotStorage, PriceLevel, RecordedBookSnapshot,
};
use crate::backtest_v2::clock::Nanos;
use crate::backtest_v2::data_contract::{
    ArrivalTimeSemantics, DatasetReadiness, DatasetReadinessClassifier, HistoricalDataContract,
    OrderBookHistory, TradeHistory,
};
use crate::backtest_v2::delta_recorder::BookDeltaStorage;
use crate::backtest_v2::events::{Event, Side, TimestampedEvent};
use crate::backtest_v2::trade_recorder::{AsyncTradeRecorder, RecordedTradePrint, TradePrintStorage};
use anyhow::Result;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use tracing::{debug, error, info, warn};

// =============================================================================
// CONFIGURATION
// =============================================================================

/// Configuration for the unified data recorder.
#[derive(Debug, Clone)]
pub struct UnifiedRecorderConfig {
    /// Path to the database file (single DB for all streams).
    pub db_path: String,
    /// Buffer size for async writers.
    pub buffer_size: usize,
    /// Enable integrity checking during recording.
    pub enable_integrity_checks: bool,
    /// Maximum allowed timestamp drift between arrival and source (ns).
    pub max_timestamp_drift_ns: u64,
    /// Enable verbose logging.
    pub verbose: bool,
}

impl Default for UnifiedRecorderConfig {
    fn default() -> Self {
        Self {
            db_path: "polymarket_historical.db".to_string(),
            buffer_size: 1000,
            enable_integrity_checks: true,
            max_timestamp_drift_ns: 5_000_000_000, // 5 seconds
            verbose: false,
        }
    }
}

// =============================================================================
// INTEGRITY TRACKING
// =============================================================================

/// Integrity counters for data recording.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RecorderIntegrity {
    /// Total snapshots recorded.
    pub snapshots_recorded: u64,
    /// Total trades recorded.
    pub trades_recorded: u64,
    /// Total deltas recorded.
    pub deltas_recorded: u64,
    /// Sequence gaps detected in snapshots.
    pub snapshot_seq_gaps: u64,
    /// Timestamp drift violations.
    pub timestamp_drift_violations: u64,
    /// Out-of-order arrivals.
    pub out_of_order_arrivals: u64,
    /// Total bytes written (approximate).
    pub bytes_written: u64,
}

/// Per-token integrity state.
#[derive(Debug, Clone, Default)]
struct TokenIntegrityState {
    last_snapshot_seq: Option<u64>,
    last_snapshot_arrival_ns: Option<Nanos>,
    last_trade_arrival_ns: Option<Nanos>,
    snapshot_count: u64,
    trade_count: u64,
}

// =============================================================================
// UNIFIED STORAGE
// =============================================================================

/// Unified storage for snapshots and trades.
pub struct UnifiedStorage {
    snapshot_storage: Arc<BookSnapshotStorage>,
    trade_storage: Arc<TradePrintStorage>,
    delta_storage: Option<Arc<BookDeltaStorage>>,
    integrity: RecorderIntegrity,
    token_states: HashMap<String, TokenIntegrityState>,
    config: UnifiedRecorderConfig,
}

impl UnifiedStorage {
    /// Open or create unified storage at the given path.
    pub fn open(config: UnifiedRecorderConfig) -> Result<Self> {
        let db_path = &config.db_path;
        
        // Open both storage systems (they can share a DB file or use separate tables)
        let snapshot_storage = Arc::new(BookSnapshotStorage::open(db_path)?);
        let trade_storage = Arc::new(TradePrintStorage::open(db_path)?);
        
        // Try to open delta storage (may not exist yet)
        let delta_storage = match BookDeltaStorage::open(db_path) {
            Ok(storage) => Some(Arc::new(storage)),
            Err(e) => {
                debug!(error = %e, "Delta storage not available (may need to be created)");
                None
            }
        };
        
        // Create metadata table for unified tracking
        let conn = Connection::open(db_path)?;
        conn.execute(
            "CREATE TABLE IF NOT EXISTS unified_metadata (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL,
                updated_at INTEGER NOT NULL
            )",
            [],
        )?;
        
        info!(
            db_path = %db_path,
            has_deltas = delta_storage.is_some(),
            "Unified storage opened"
        );
        
        Ok(Self {
            snapshot_storage,
            trade_storage,
            delta_storage,
            integrity: RecorderIntegrity::default(),
            token_states: HashMap::new(),
            config,
        })
    }
    
    /// Get a reference to the snapshot storage.
    pub fn snapshot_storage(&self) -> &Arc<BookSnapshotStorage> {
        &self.snapshot_storage
    }
    
    /// Get a reference to the trade storage.
    pub fn trade_storage(&self) -> &Arc<TradePrintStorage> {
        &self.trade_storage
    }
    
    /// Get current integrity counters.
    pub fn integrity(&self) -> &RecorderIntegrity {
        &self.integrity
    }
    
    /// Get the data contract this storage can satisfy.
    pub fn data_contract(&self) -> HistoricalDataContract {
        // Check what data we have based on token counts
        let has_snapshots = self.snapshot_storage.get_token_ids()
            .map(|ids| !ids.is_empty())
            .unwrap_or(false);
        let has_trades = self.trade_storage.get_token_ids()
            .map(|ids| !ids.is_empty())
            .unwrap_or(false);
        
        // Check for deltas - this upgrades us to FullIncremental
        let has_deltas = self.delta_storage.as_ref()
            .and_then(|storage| storage.total_delta_count().ok())
            .map(|count| count > 0)
            .unwrap_or(false);
        
        HistoricalDataContract {
            venue: "Polymarket".to_string(),
            market: "15m up/down".to_string(),
            orderbook: if has_deltas {
                // Deltas present = FULL_INCREMENTAL (MAKER_VIABLE pathway)
                OrderBookHistory::FullIncrementalL2DeltasWithExchangeSeq
            } else if has_snapshots {
                OrderBookHistory::PeriodicL2Snapshots
            } else {
                OrderBookHistory::None
            },
            trades: if has_trades {
                TradeHistory::TradePrints
            } else {
                TradeHistory::None
            },
            arrival_time: ArrivalTimeSemantics::RecordedArrival,
        }
    }
    
    /// Get a reference to the delta storage (if available).
    pub fn delta_storage(&self) -> Option<&Arc<BookDeltaStorage>> {
        self.delta_storage.as_ref()
    }
    
    /// Check if deltas are available for the given token.
    pub fn has_deltas_for_token(&self, token_id: &str) -> bool {
        self.delta_storage.as_ref()
            .and_then(|storage| storage.delta_count(token_id).ok())
            .map(|count| count > 0)
            .unwrap_or(false)
    }
    
    /// Get total delta count across all tokens.
    pub fn total_delta_count(&self) -> u64 {
        self.delta_storage.as_ref()
            .and_then(|storage| storage.total_delta_count().ok())
            .unwrap_or(0)
    }
    
    /// Get the dataset readiness classification.
    pub fn readiness(&self) -> DatasetReadiness {
        DatasetReadinessClassifier::new().classify_quick(&self.data_contract())
    }
}

// =============================================================================
// UNIFIED RECORDER
// =============================================================================

/// Unified asynchronous recorder that coordinates snapshot and trade recording.
pub struct UnifiedRecorder {
    storage: Arc<UnifiedStorage>,
    snapshot_recorder: AsyncBookRecorder,
    trade_recorder: AsyncTradeRecorder,
    config: UnifiedRecorderConfig,
    /// Per-token integrity state for checking.
    token_states: parking_lot::RwLock<HashMap<String, TokenIntegrityState>>,
    /// Global integrity counters.
    integrity: parking_lot::RwLock<RecorderIntegrity>,
}

impl UnifiedRecorder {
    /// Create and spawn a unified recorder.
    pub fn spawn(config: UnifiedRecorderConfig) -> Result<Arc<Self>> {
        let storage = Arc::new(UnifiedStorage::open(config.clone())?);
        
        let snapshot_recorder = AsyncBookRecorder::spawn(
            Arc::clone(storage.snapshot_storage()),
            config.buffer_size,
        );
        let trade_recorder = AsyncTradeRecorder::spawn(
            Arc::clone(storage.trade_storage()),
            config.buffer_size,
        );
        
        Ok(Arc::new(Self {
            storage,
            snapshot_recorder,
            trade_recorder,
            config,
            token_states: parking_lot::RwLock::new(HashMap::new()),
            integrity: parking_lot::RwLock::new(RecorderIntegrity::default()),
        }))
    }
    
    /// Record a book snapshot with integrity checking.
    pub fn record_snapshot(&self, snapshot: RecordedBookSnapshot) {
        if self.config.enable_integrity_checks {
            self.check_snapshot_integrity(&snapshot);
        }
        
        self.integrity.write().snapshots_recorded += 1;
        self.snapshot_recorder.record(snapshot);
    }
    
    /// Record a trade print with integrity checking.
    pub fn record_trade(&self, trade: RecordedTradePrint) {
        if self.config.enable_integrity_checks {
            self.check_trade_integrity(&trade);
        }
        
        self.integrity.write().trades_recorded += 1;
        self.trade_recorder.record(trade);
    }
    
    /// Check snapshot integrity.
    fn check_snapshot_integrity(&self, snapshot: &RecordedBookSnapshot) {
        let mut states = self.token_states.write();
        let mut integrity = self.integrity.write();
        
        let state = states.entry(snapshot.token_id.clone()).or_default();
        
        // Check sequence gap (only if we have sequence numbers)
        if let (Some(last_seq), Some(curr_seq)) = (state.last_snapshot_seq, snapshot.exchange_seq) {
            if curr_seq <= last_seq {
                if self.config.verbose {
                    warn!(
                        token_id = %snapshot.token_id,
                        last_seq = %last_seq,
                        new_seq = %curr_seq,
                        "Non-monotonic snapshot sequence"
                    );
                }
            } else if curr_seq > last_seq + 1 {
                integrity.snapshot_seq_gaps += 1;
                if self.config.verbose {
                    debug!(
                        token_id = %snapshot.token_id,
                        gap = %(curr_seq - last_seq - 1),
                        "Snapshot sequence gap detected"
                    );
                }
            }
        }
        
        // Check arrival time ordering
        if let Some(last_arrival) = state.last_snapshot_arrival_ns {
            if (snapshot.arrival_time_ns as i64) < last_arrival {
                integrity.out_of_order_arrivals += 1;
                if self.config.verbose {
                    warn!(
                        token_id = %snapshot.token_id,
                        "Out-of-order snapshot arrival"
                    );
                }
            }
        }
        
        // Check timestamp drift
        if let Some(source_ns) = snapshot.source_time_ns {
            let drift = snapshot.arrival_time_ns.saturating_sub(source_ns);
            if drift > self.config.max_timestamp_drift_ns {
                integrity.timestamp_drift_violations += 1;
                if self.config.verbose {
                    warn!(
                        token_id = %snapshot.token_id,
                        drift_ms = %(drift / 1_000_000),
                        "Excessive timestamp drift"
                    );
                }
            }
        }
        
        state.last_snapshot_seq = snapshot.exchange_seq;
        state.last_snapshot_arrival_ns = Some(snapshot.arrival_time_ns as i64);
        state.snapshot_count += 1;
    }
    
    /// Check trade integrity.
    fn check_trade_integrity(&self, trade: &RecordedTradePrint) {
        let mut states = self.token_states.write();
        let mut integrity = self.integrity.write();
        
        let state = states.entry(trade.token_id.clone()).or_default();
        
        // Check arrival time ordering
        if let Some(last_arrival) = state.last_trade_arrival_ns {
            if (trade.arrival_time_ns as i64) < last_arrival {
                integrity.out_of_order_arrivals += 1;
                if self.config.verbose {
                    warn!(
                        token_id = %trade.token_id,
                        "Out-of-order trade arrival"
                    );
                }
            }
        }
        
        // Check timestamp drift
        let drift = trade.arrival_time_ns.saturating_sub(trade.source_time_ns);
        if drift > self.config.max_timestamp_drift_ns {
            integrity.timestamp_drift_violations += 1;
            if self.config.verbose {
                warn!(
                    token_id = %trade.token_id,
                    drift_ms = %(drift / 1_000_000),
                    "Excessive timestamp drift for trade"
                );
            }
        }
        
        state.last_trade_arrival_ns = Some(trade.arrival_time_ns as i64);
        state.trade_count += 1;
    }
    
    /// Get current integrity counters.
    pub fn integrity(&self) -> RecorderIntegrity {
        self.integrity.read().clone()
    }
    
    /// Get recorder statistics.
    pub fn stats(&self) -> RecorderStats {
        let integrity = self.integrity.read().clone();
        
        RecorderStats {
            snapshots_recorded: integrity.snapshots_recorded,
            trades_recorded: integrity.trades_recorded,
            integrity,
        }
    }
    
    /// Get the underlying storage.
    pub fn storage(&self) -> &Arc<UnifiedStorage> {
        &self.storage
    }
}

/// Recorder statistics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecorderStats {
    pub snapshots_recorded: u64,
    pub trades_recorded: u64,
    pub integrity: RecorderIntegrity,
}

// =============================================================================
// UNIFIED REPLAY FEED
// =============================================================================

use crate::backtest_v2::delta_recorder::{L2BookDeltaRecord, DeltaReplayFeed};

/// Unified replay feed that merges snapshots, trades, and deltas by arrival time.
/// 
/// This feed ensures deterministic ordering by (arrival_time_ns, stream_priority, seq).
/// Stream priorities:
/// - Snapshots: 0 (highest - book state reset)
/// - Deltas: 1 (book state updates)
/// - Trades: 2 (queue consumption)
pub struct UnifiedReplayFeed {
    /// Pending snapshots (sorted by arrival_time_ns).
    snapshots: Vec<RecordedBookSnapshot>,
    /// Pending trades (sorted by arrival_time_ns).
    trades: Vec<RecordedTradePrint>,
    /// Pending deltas (sorted by arrival_time_ns, ingest_seq).
    deltas: Vec<L2BookDeltaRecord>,
    /// Current snapshot index.
    snapshot_idx: usize,
    /// Current trade index.
    trade_idx: usize,
    /// Current delta index.
    delta_idx: usize,
    /// Total events.
    total_events: usize,
    /// Whether deltas are available (impacts readiness).
    has_deltas: bool,
}

impl UnifiedReplayFeed {
    /// Load a unified feed from storage for a specific token and time range.
    /// Includes snapshots, trades, AND deltas for full incremental replay.
    pub fn from_storage(
        storage: &UnifiedStorage,
        token_id: &str,
        start_ns: Nanos,
        end_ns: Nanos,
    ) -> Result<Self> {
        let snapshots = storage.snapshot_storage()
            .load_snapshots_in_range(token_id, start_ns as u64, end_ns as u64)?;
        let trades = storage.trade_storage()
            .load_trades_in_range(token_id, start_ns as u64, end_ns as u64)?;
        
        // Load deltas if storage is available
        let (deltas, has_deltas) = if let Some(delta_storage) = storage.delta_storage() {
            let d = delta_storage.load_deltas(token_id, start_ns as u64, end_ns as u64)?;
            let has = !d.is_empty();
            (d, has)
        } else {
            (Vec::new(), false)
        };
        
        let total_events = snapshots.len() + trades.len() + deltas.len();
        
        info!(
            token_id = %token_id,
            snapshots = snapshots.len(),
            trades = trades.len(),
            deltas = deltas.len(),
            has_deltas = has_deltas,
            "Loaded unified replay feed"
        );
        
        Ok(Self {
            snapshots,
            trades,
            deltas,
            snapshot_idx: 0,
            trade_idx: 0,
            delta_idx: 0,
            total_events,
            has_deltas,
        })
    }
    
    /// Load all tokens for a time range.
    /// Includes snapshots, trades, AND deltas for full incremental replay.
    pub fn all_tokens(
        storage: &UnifiedStorage,
        start_ns: Nanos,
        end_ns: Nanos,
    ) -> Result<Self> {
        let snapshots = storage.snapshot_storage()
            .load_all_snapshots_in_range(start_ns as u64, end_ns as u64)?;
        let trades = storage.trade_storage()
            .load_all_trades_in_range(start_ns as u64, end_ns as u64)?;
        
        // Load deltas for all tokens if storage is available
        let (deltas, has_deltas) = if let Some(delta_storage) = storage.delta_storage() {
            // Get all tokens with deltas
            let tokens = delta_storage.list_tokens().unwrap_or_default();
            let mut all_deltas = Vec::new();
            for token_id in &tokens {
                let token_deltas = delta_storage.load_deltas(token_id, start_ns as u64, end_ns as u64)?;
                all_deltas.extend(token_deltas);
            }
            // Sort by arrival time, then ingest_seq for determinism
            all_deltas.sort_by(|a, b| {
                a.ingest_arrival_time_ns.cmp(&b.ingest_arrival_time_ns)
                    .then_with(|| a.ingest_seq.cmp(&b.ingest_seq))
            });
            let has = !all_deltas.is_empty();
            (all_deltas, has)
        } else {
            (Vec::new(), false)
        };
        
        let total_events = snapshots.len() + trades.len() + deltas.len();
        
        info!(
            snapshots = snapshots.len(),
            trades = trades.len(),
            deltas = deltas.len(),
            has_deltas = has_deltas,
            "Loaded unified replay feed for all tokens"
        );
        
        Ok(Self {
            snapshots,
            trades,
            deltas,
            snapshot_idx: 0,
            trade_idx: 0,
            delta_idx: 0,
            total_events,
            has_deltas,
        })
    }
    
    /// Check if this feed has delta data (required for maker viability).
    pub fn has_deltas(&self) -> bool {
        self.has_deltas
    }
    
    /// Get delta count.
    pub fn delta_count(&self) -> usize {
        self.deltas.len()
    }
    
    /// Get the next event (snapshot, delta, or trade) by arrival time.
    /// 
    /// Ordering priority when arrival times are equal:
    /// 1. Snapshots (stream=0) - book state reset must happen first
    /// 2. Deltas (stream=1) - book state updates
    /// 3. Trades (stream=2) - queue consumption happens after book updates
    pub fn next_event(&mut self) -> Option<TimestampedEvent> {
        let next_snapshot = self.snapshots.get(self.snapshot_idx);
        let next_trade = self.trades.get(self.trade_idx);
        let next_delta = self.deltas.get(self.delta_idx);
        
        // Get arrival times (using u64::MAX for exhausted streams)
        let snap_time = next_snapshot.map(|s| s.arrival_time_ns).unwrap_or(u64::MAX);
        let trade_time = next_trade.map(|t| t.arrival_time_ns).unwrap_or(u64::MAX);
        let delta_time = next_delta.map(|d| d.ingest_arrival_time_ns).unwrap_or(u64::MAX);
        
        // Find minimum time
        let min_time = snap_time.min(trade_time).min(delta_time);
        
        if min_time == u64::MAX {
            // All streams exhausted
            return None;
        }
        
        // Priority: snapshot > delta > trade when times are equal
        if snap_time == min_time {
            self.snapshot_idx += 1;
            Some(snapshot_to_event(next_snapshot.unwrap()))
        } else if delta_time == min_time {
            self.delta_idx += 1;
            Some(delta_to_event(next_delta.unwrap()))
        } else {
            self.trade_idx += 1;
            Some(trade_to_event(next_trade.unwrap()))
        }
    }
    
    /// Peek at the next event without consuming it.
    pub fn peek(&self) -> Option<Nanos> {
        let next_snapshot = self.snapshots.get(self.snapshot_idx);
        let next_trade = self.trades.get(self.trade_idx);
        let next_delta = self.deltas.get(self.delta_idx);
        
        let snap_time = next_snapshot.map(|s| s.arrival_time_ns).unwrap_or(u64::MAX);
        let trade_time = next_trade.map(|t| t.arrival_time_ns).unwrap_or(u64::MAX);
        let delta_time = next_delta.map(|d| d.ingest_arrival_time_ns).unwrap_or(u64::MAX);
        
        let min_time = snap_time.min(trade_time).min(delta_time);
        
        if min_time == u64::MAX {
            None
        } else {
            Some(min_time as Nanos)
        }
    }
    
    /// Get total event count.
    pub fn total_events(&self) -> usize {
        self.total_events
    }
    
    /// Get remaining events.
    pub fn remaining(&self) -> usize {
        (self.snapshots.len() - self.snapshot_idx) 
            + (self.trades.len() - self.trade_idx)
            + (self.deltas.len() - self.delta_idx)
    }
    
    /// Reset feed to the beginning for multiple passes.
    pub fn reset(&mut self) {
        self.snapshot_idx = 0;
        self.trade_idx = 0;
        self.delta_idx = 0;
    }
}

// Helper functions for converting recorded data to events

fn snapshot_to_event(s: &RecordedBookSnapshot) -> TimestampedEvent {
    use crate::backtest_v2::events::Level;
    
    TimestampedEvent {
        time: s.arrival_time_ns as Nanos,
        source_time: s.source_time_ns.unwrap_or(s.arrival_time_ns) as Nanos,
        seq: s.local_seq,
        source: 0, // L2 snapshot stream (highest priority)
        event: Event::L2BookSnapshot {
            token_id: s.token_id.clone(),
            bids: s.bids.iter().map(|l| Level { price: l.price, size: l.size, order_count: Some(1) }).collect(),
            asks: s.asks.iter().map(|l| Level { price: l.price, size: l.size, order_count: Some(1) }).collect(),
            exchange_seq: s.exchange_seq.unwrap_or(0),
        },
    }
}

fn delta_to_event(d: &L2BookDeltaRecord) -> TimestampedEvent {
    TimestampedEvent {
        time: d.ingest_arrival_time_ns as Nanos,
        source_time: d.source_time_as_nanos(),
        seq: d.ingest_seq,
        source: 1, // L2 delta stream (middle priority)
        event: Event::L2BookDelta {
            token_id: d.token_id.clone(),
            side: d.side,
            price: d.price,
            new_size: d.new_size,
            seq_hash: Some(d.seq_hash.clone()),
        },
    }
}

fn trade_to_event(t: &RecordedTradePrint) -> TimestampedEvent {
    TimestampedEvent {
        time: t.arrival_time_ns as Nanos,
        source_time: t.source_time_ns as Nanos,
        seq: t.local_seq,
        source: 2, // Trade print stream (lowest priority - processed after book updates)
        event: Event::TradePrint {
            token_id: t.token_id.clone(),
            price: t.price,
            size: t.size,
            aggressor_side: t.aggressor_side,
            trade_id: Some(t.local_seq.to_string()),
        },
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
    fn test_unified_storage_creation() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        
        let config = UnifiedRecorderConfig {
            db_path: db_path.to_string_lossy().to_string(),
            ..Default::default()
        };
        
        let storage = UnifiedStorage::open(config).unwrap();
        assert_eq!(storage.integrity().snapshots_recorded, 0);
        assert_eq!(storage.integrity().trades_recorded, 0);
    }
    
    #[tokio::test]
    async fn test_unified_recorder_spawn() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        
        let config = UnifiedRecorderConfig {
            db_path: db_path.to_string_lossy().to_string(),
            ..Default::default()
        };
        
        let recorder = UnifiedRecorder::spawn(config).unwrap();
        let stats = recorder.stats();
        
        assert_eq!(stats.snapshots_recorded, 0);
        assert_eq!(stats.trades_recorded, 0);
    }
    
    #[test]
    fn test_data_contract_generation() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        
        let config = UnifiedRecorderConfig {
            db_path: db_path.to_string_lossy().to_string(),
            ..Default::default()
        };
        
        let storage = UnifiedStorage::open(config).unwrap();
        let contract = storage.data_contract();
        
        // Empty storage should have no data
        assert_eq!(contract.orderbook, OrderBookHistory::None);
        assert_eq!(contract.trades, TradeHistory::None);
        assert_eq!(contract.arrival_time, ArrivalTimeSemantics::RecordedArrival);
    }
    
    #[test]
    fn test_readiness_classification() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        
        let config = UnifiedRecorderConfig {
            db_path: db_path.to_string_lossy().to_string(),
            ..Default::default()
        };
        
        let storage = UnifiedStorage::open(config).unwrap();
        let readiness = storage.readiness();
        
        // Empty storage should be NonRepresentative
        assert_eq!(readiness, DatasetReadiness::NonRepresentative);
    }
    
    #[tokio::test]
    async fn test_integrity_checking() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        
        let config = UnifiedRecorderConfig {
            db_path: db_path.to_string_lossy().to_string(),
            enable_integrity_checks: true,
            max_timestamp_drift_ns: 1_000_000_000, // 1 second
            ..Default::default()
        };
        
        let recorder = UnifiedRecorder::spawn(config).unwrap();
        
        // Record a snapshot
        let snapshot = RecordedBookSnapshot {
            token_id: "TOKEN1".to_string(),
            exchange_seq: Some(1),
            source_time_ns: Some(999_000_000),
            arrival_time_ns: 1_000_000_000,
            local_seq: 0,
            bids: vec![],
            asks: vec![],
            best_bid: None,
            best_ask: None,
            mid_price: None,
            spread: None,
        };
        recorder.record_snapshot(snapshot);
        
        // Record another with sequence gap
        let snapshot2 = RecordedBookSnapshot {
            token_id: "TOKEN1".to_string(),
            exchange_seq: Some(5), // Gap from 1 to 5
            source_time_ns: Some(1_999_000_000),
            arrival_time_ns: 2_000_000_000,
            local_seq: 1,
            bids: vec![],
            asks: vec![],
            best_bid: None,
            best_ask: None,
            mid_price: None,
            spread: None,
        };
        recorder.record_snapshot(snapshot2);
        
        let integrity = recorder.integrity();
        assert_eq!(integrity.snapshots_recorded, 2);
        assert_eq!(integrity.snapshot_seq_gaps, 1);
    }
    
    #[tokio::test]
    async fn test_timestamp_drift_detection() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        
        let config = UnifiedRecorderConfig {
            db_path: db_path.to_string_lossy().to_string(),
            enable_integrity_checks: true,
            max_timestamp_drift_ns: 100_000_000, // 100ms
            ..Default::default()
        };
        
        let recorder = UnifiedRecorder::spawn(config).unwrap();
        
        // Record a snapshot with excessive drift
        let snapshot = RecordedBookSnapshot {
            token_id: "TOKEN1".to_string(),
            exchange_seq: Some(1),
            source_time_ns: Some(100_000_000), // 900ms in the past
            arrival_time_ns: 1_000_000_000,
            local_seq: 0,
            bids: vec![],
            asks: vec![],
            best_bid: None,
            best_ask: None,
            mid_price: None,
            spread: None,
        };
        recorder.record_snapshot(snapshot);
        
        let integrity = recorder.integrity();
        assert_eq!(integrity.timestamp_drift_violations, 1);
    }
    
    #[test]
    fn test_data_contract_with_deltas() {
        use crate::backtest_v2::data_contract::{
            DatasetClassification, OrderBookHistory,
        };
        use crate::backtest_v2::delta_recorder::{BookDeltaStorage, L2BookDeltaRecord};
        
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test_deltas.db");
        
        // Create delta storage and add a delta
        let delta_storage = BookDeltaStorage::open(&db_path.to_string_lossy()).unwrap();
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
        delta_storage.store_delta(&delta).unwrap();
        
        // Now open unified storage - it should detect deltas
        let config = UnifiedRecorderConfig {
            db_path: db_path.to_string_lossy().to_string(),
            ..Default::default()
        };
        let storage = UnifiedStorage::open(config).unwrap();
        
        // Verify delta count
        assert_eq!(storage.total_delta_count(), 1);
        
        // Verify contract has FullIncremental orderbook
        let contract = storage.data_contract();
        assert_eq!(
            contract.orderbook,
            OrderBookHistory::FullIncrementalL2DeltasWithExchangeSeq
        );
        
        // Classification WITHOUT trades is Incomplete (need both for FullIncremental classification)
        // The key point is: deltas → FullIncrementalL2DeltasWithExchangeSeq orderbook type
        // To get FullIncremental classification, we also need trade prints
        let classification = contract.classify();
        assert_eq!(classification, DatasetClassification::Incomplete);
        
        // Note: To achieve MakerViable classification:
        // 1. Must have FullIncrementalL2DeltasWithExchangeSeq orderbook (✓ done via deltas)
        // 2. Must have TradePrints (needed for queue consumption tracking)
        // 3. Must have RecordedArrival timestamps (✓ always set)
    }
    
    /// CRITICAL TEST: Prove the readiness flip from TakerOnly → MakerViable
    /// when deltas + trades are present.
    #[test]
    fn test_readiness_flip_taker_to_maker() {
        use crate::backtest_v2::book_recorder::PriceLevel;
        use crate::backtest_v2::data_contract::{
            DatasetClassification, DatasetReadiness, DatasetReadinessClassifier, OrderBookHistory,
        };
        use crate::backtest_v2::delta_recorder::{BookDeltaStorage, L2BookDeltaRecord};
        use crate::backtest_v2::trade_recorder::{RecordedTradePrint, TradePrintStorage};
        use crate::backtest_v2::events::Side;
        
        // --- PHASE 1: Snapshots + Trades only → TakerOnly ---
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test_readiness_flip.db");
        
        // Create storage with snapshots and trades only (no deltas)
        let snapshot_storage = BookSnapshotStorage::open(&db_path.to_string_lossy()).unwrap();
        let trade_storage = TradePrintStorage::open(&db_path.to_string_lossy()).unwrap();
        
        // Add a snapshot
        let snapshot = RecordedBookSnapshot {
            token_id: "TOKEN1".to_string(),
            exchange_seq: Some(1),
            source_time_ns: Some(1000_000_000),
            arrival_time_ns: 1000_000_000,
            local_seq: 0,
            bids: vec![PriceLevel { price: 0.50, size: 100.0 }],
            asks: vec![PriceLevel { price: 0.52, size: 100.0 }],
            best_bid: Some(0.50),
            best_ask: Some(0.52),
            mid_price: Some(0.51),
            spread: Some(0.02),
        };
        snapshot_storage.store_snapshot(&snapshot).unwrap();
        
        // Add a trade
        let trade = RecordedTradePrint {
            token_id: "TOKEN1".to_string(),
            market_id: "MARKET1".to_string(),
            price: 0.51,
            size: 10.0,
            aggressor_side: Side::Buy,
            fee_rate_bps: Some(10),
            source_time_ns: 1001_000_000,
            arrival_time_ns: 1001_000_000,
            local_seq: 0,
            exchange_trade_id: Some("trade_1".to_string()),
        };
        trade_storage.store_trade(&trade).unwrap();
        
        // Open unified storage
        let config = UnifiedRecorderConfig {
            db_path: db_path.to_string_lossy().to_string(),
            ..Default::default()
        };
        let storage = UnifiedStorage::open(config.clone()).unwrap();
        
        // Check contract - should be PeriodicL2Snapshots (no deltas)
        let contract_phase1 = storage.data_contract();
        assert_eq!(
            contract_phase1.orderbook,
            OrderBookHistory::PeriodicL2Snapshots,
            "Phase 1: Without deltas, should be PeriodicL2Snapshots"
        );
        
        // Classification should be SnapshotOnly
        let classification_phase1 = contract_phase1.classify();
        assert_eq!(
            classification_phase1,
            DatasetClassification::SnapshotOnly,
            "Phase 1: Should be SnapshotOnly classification"
        );
        
        // Readiness should be TakerOnly
        let readiness_phase1 = storage.readiness();
        assert_eq!(
            readiness_phase1,
            DatasetReadiness::TakerOnly,
            "Phase 1: Should be TakerOnly readiness"
        );
        assert!(
            readiness_phase1.allows_taker(),
            "Phase 1: TakerOnly should allow taker strategies"
        );
        assert!(
            !readiness_phase1.allows_maker(),
            "Phase 1: TakerOnly should NOT allow maker strategies"
        );
        
        println!("=== PHASE 1 COMPLETE: TakerOnly (snapshots + trades) ===");
        println!("  Orderbook: {:?}", contract_phase1.orderbook);
        println!("  Classification: {:?}", classification_phase1);
        println!("  Readiness: {:?}", readiness_phase1);
        
        // --- PHASE 2: Add deltas → MakerViable ---
        // Note: We need to create a new delta storage (can't add to existing connection easily)
        let delta_storage = BookDeltaStorage::open(&db_path.to_string_lossy()).unwrap();
        
        // Add multiple deltas to the same token
        for i in 0..5 {
            let delta = L2BookDeltaRecord::from_price_change(
                "MARKET1".to_string(),
                "TOKEN1".to_string(),
                if i % 2 == 0 { "BUY" } else { "SELL" },
                0.50 + i as f64 * 0.01,
                100.0 + i as f64 * 10.0,
                1002_000_000 + i as u64,
                1002_000_000_000 + i as u64 * 1_000_000,
                i as u64,
                format!("hash_{}", i),
                Some(0.50),
                Some(0.52),
            );
            delta_storage.store_delta(&delta).unwrap();
        }
        
        // Re-open unified storage to pick up deltas
        let storage_phase2 = UnifiedStorage::open(config).unwrap();
        
        // Check contract - should now be FullIncrementalL2DeltasWithExchangeSeq
        let contract_phase2 = storage_phase2.data_contract();
        assert_eq!(
            contract_phase2.orderbook,
            OrderBookHistory::FullIncrementalL2DeltasWithExchangeSeq,
            "Phase 2: With deltas, should be FullIncrementalL2DeltasWithExchangeSeq"
        );
        
        // Classification should be FullIncremental (deltas + trades)
        let classification_phase2 = contract_phase2.classify();
        assert_eq!(
            classification_phase2,
            DatasetClassification::FullIncremental,
            "Phase 2: Should be FullIncremental classification"
        );
        
        // Readiness should be MakerViable!
        let readiness_phase2 = storage_phase2.readiness();
        assert_eq!(
            readiness_phase2,
            DatasetReadiness::MakerViable,
            "Phase 2: Should be MakerViable readiness"
        );
        assert!(
            readiness_phase2.allows_taker(),
            "Phase 2: MakerViable should allow taker strategies"
        );
        assert!(
            readiness_phase2.allows_maker(),
            "Phase 2: MakerViable SHOULD allow maker strategies"
        );
        
        println!("=== PHASE 2 COMPLETE: MakerViable (deltas + trades) ===");
        println!("  Orderbook: {:?}", contract_phase2.orderbook);
        println!("  Classification: {:?}", classification_phase2);
        println!("  Readiness: {:?}", readiness_phase2);
        println!("  Delta count: {}", storage_phase2.total_delta_count());
        
        // Verify the flip
        println!("\n=== READINESS FLIP VERIFIED ===");
        println!("  BEFORE (snapshots + trades): {:?}", readiness_phase1);
        println!("  AFTER (+ deltas):            {:?}", readiness_phase2);
        println!("  Flip successful: TakerOnly → MakerViable");
    }
    
    /// Test unified replay feed includes deltas in correct order.
    #[test]
    fn test_unified_feed_includes_deltas() {
        use crate::backtest_v2::book_recorder::PriceLevel;
        use crate::backtest_v2::delta_recorder::{BookDeltaStorage, L2BookDeltaRecord};
        use crate::backtest_v2::trade_recorder::{RecordedTradePrint, TradePrintStorage};
        use crate::backtest_v2::events::{Event, Side};
        
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test_unified_feed.db");
        
        // Create all storages
        let snapshot_storage = BookSnapshotStorage::open(&db_path.to_string_lossy()).unwrap();
        let trade_storage = TradePrintStorage::open(&db_path.to_string_lossy()).unwrap();
        let delta_storage = BookDeltaStorage::open(&db_path.to_string_lossy()).unwrap();
        
        // Timeline:
        // t=1000ns: Snapshot
        // t=1001ns: Delta 1
        // t=1002ns: Trade
        // t=1003ns: Delta 2
        
        // Add snapshot
        let snapshot = RecordedBookSnapshot {
            token_id: "TOKEN1".to_string(),
            exchange_seq: Some(1),
            source_time_ns: Some(1000),
            arrival_time_ns: 1000,
            local_seq: 0,
            bids: vec![PriceLevel { price: 0.50, size: 100.0 }],
            asks: vec![PriceLevel { price: 0.52, size: 100.0 }],
            best_bid: Some(0.50),
            best_ask: Some(0.52),
            mid_price: Some(0.51),
            spread: Some(0.02),
        };
        snapshot_storage.store_snapshot(&snapshot).unwrap();
        
        // Add delta 1
        let delta1 = L2BookDeltaRecord::from_price_change(
            "MARKET1".to_string(),
            "TOKEN1".to_string(),
            "BUY",
            0.50,
            110.0,
            1001,
            1001,
            0,
            "delta_hash_1".to_string(),
            Some(0.50),
            Some(0.52),
        );
        delta_storage.store_delta(&delta1).unwrap();
        
        // Add trade
        let trade = RecordedTradePrint {
            token_id: "TOKEN1".to_string(),
            market_id: "MARKET1".to_string(),
            price: 0.51,
            size: 5.0,
            aggressor_side: Side::Buy,
            fee_rate_bps: Some(10),
            source_time_ns: 1002,
            arrival_time_ns: 1002,
            local_seq: 0,
            exchange_trade_id: Some("trade_1".to_string()),
        };
        trade_storage.store_trade(&trade).unwrap();
        
        // Add delta 2
        let delta2 = L2BookDeltaRecord::from_price_change(
            "MARKET1".to_string(),
            "TOKEN1".to_string(),
            "SELL",
            0.52,
            90.0,
            1003,
            1003,
            1,
            "delta_hash_2".to_string(),
            Some(0.50),
            Some(0.52),
        );
        delta_storage.store_delta(&delta2).unwrap();
        
        // Open unified storage
        let config = UnifiedRecorderConfig {
            db_path: db_path.to_string_lossy().to_string(),
            ..Default::default()
        };
        let storage = UnifiedStorage::open(config).unwrap();
        
        // Check delta count
        assert_eq!(storage.total_delta_count(), 2);
        
        // Create replay feed
        let mut feed = UnifiedReplayFeed::from_storage(
            &storage,
            "TOKEN1",
            0,
            10000,
        ).unwrap();
        
        assert!(feed.has_deltas(), "Feed should have deltas");
        assert_eq!(feed.delta_count(), 2, "Should have 2 deltas");
        assert_eq!(feed.total_events(), 4, "Total events = 1 snapshot + 2 deltas + 1 trade");
        
        // Verify event order: Snapshot → Delta1 → Trade → Delta2
        let event1 = feed.next_event().unwrap();
        match event1.event {
            Event::L2BookSnapshot { .. } => println!("Event 1: L2BookSnapshot at t={}", event1.time),
            _ => panic!("Expected L2BookSnapshot first"),
        }
        
        let event2 = feed.next_event().unwrap();
        match event2.event {
            Event::L2BookDelta { new_size, .. } => {
                println!("Event 2: L2BookDelta at t={}, new_size={}", event2.time, new_size);
                assert_eq!(new_size, 110.0, "First delta should have new_size=110");
            }
            _ => panic!("Expected L2BookDelta second"),
        }
        
        let event3 = feed.next_event().unwrap();
        match event3.event {
            Event::TradePrint { size, .. } => {
                println!("Event 3: TradePrint at t={}, size={}", event3.time, size);
                assert_eq!(size, 5.0);
            }
            _ => panic!("Expected TradePrint third"),
        }
        
        let event4 = feed.next_event().unwrap();
        match event4.event {
            Event::L2BookDelta { new_size, .. } => {
                println!("Event 4: L2BookDelta at t={}, new_size={}", event4.time, new_size);
                assert_eq!(new_size, 90.0, "Second delta should have new_size=90");
            }
            _ => panic!("Expected L2BookDelta fourth"),
        }
        
        // No more events
        assert!(feed.next_event().is_none(), "Should have no more events");
        assert_eq!(feed.remaining(), 0);
        
        println!("\n=== UNIFIED FEED TEST PASSED ===");
        println!("  Events in correct order: Snapshot → Delta → Trade → Delta");
    }
}

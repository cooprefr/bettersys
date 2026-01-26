//! Trade Print Recorder
//!
//! Persists public trade prints received via Polymarket CLOB WebSocket
//! with high-resolution arrival timestamps for offline backtesting replay.
//!
//! Trade prints are essential for queue modeling because they tell us
//! when and how much of the queue at each price level was consumed.
//!
//! **CRITICAL**: Arrival time is captured at the EARLIEST possible point
//! (WebSocket message receipt, BEFORE JSON parsing) to minimize measurement noise.

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

use crate::backtest_v2::clock::Nanos;
use crate::backtest_v2::events::{Event, Side};

// =============================================================================
// Recorded Trade Print
// =============================================================================

/// A public trade print with arrival-time semantics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordedTradePrint {
    /// Token ID (clobTokenId / asset_id).
    pub token_id: String,
    /// Market ID (condition_id).
    pub market_id: String,
    /// Execution price.
    pub price: f64,
    /// Trade size.
    pub size: f64,
    /// Aggressor side (who crossed the spread: BUY = bought from asks, SELL = sold to bids).
    pub aggressor_side: Side,
    /// Fee rate in basis points (optional).
    pub fee_rate_bps: Option<i32>,
    /// Exchange source timestamp (from WebSocket message, as nanoseconds).
    pub source_time_ns: u64,
    /// Our arrival timestamp (nanoseconds since Unix epoch).
    /// Captured at WebSocket message receipt, BEFORE JSON parsing.
    pub arrival_time_ns: u64,
    /// Local monotonic sequence for ordering within same arrival_time.
    pub local_seq: u64,
    /// Exchange trade ID for deduplication (if available).
    pub exchange_trade_id: Option<String>,
}

impl RecordedTradePrint {
    /// Create from WebSocket `last_trade_price` message data.
    pub fn from_ws_trade(
        token_id: String,
        market_id: String,
        price: f64,
        size: f64,
        side_str: &str,
        fee_rate_bps: Option<i32>,
        source_time_ms: u64,
        arrival_time_ns: u64,
        local_seq: u64,
    ) -> Self {
        let aggressor_side = match side_str.to_uppercase().as_str() {
            "BUY" => Side::Buy,
            "SELL" => Side::Sell,
            _ => Side::Buy, // Default to buy if unknown
        };

        Self {
            token_id,
            market_id,
            price,
            size,
            aggressor_side,
            fee_rate_bps,
            source_time_ns: source_time_ms * 1_000_000, // ms to ns
            arrival_time_ns,
            local_seq,
            exchange_trade_id: None,
        }
    }

    /// Convert to backtest Event.
    pub fn to_event(&self) -> Event {
        Event::TradePrint {
            token_id: self.token_id.clone(),
            price: self.price,
            size: self.size,
            aggressor_side: self.aggressor_side,
            trade_id: self.exchange_trade_id.clone(),
        }
    }

    /// Get arrival time as Nanos for backtest clock.
    pub fn arrival_time_as_nanos(&self) -> Nanos {
        self.arrival_time_ns as Nanos
    }

    /// Check if this trade would consume liquidity from bids or asks.
    /// - BUY aggressor: consumes asks (lifts offers)
    /// - SELL aggressor: consumes bids (hits bids)
    pub fn consumes_side(&self) -> Side {
        self.aggressor_side.opposite()
    }
}

// =============================================================================
// Storage Schema
// =============================================================================

const TRADE_PRINT_SCHEMA: &str = r#"
-- Enable optimizations
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;
PRAGMA cache_size = -32000;
PRAGMA temp_store = MEMORY;

-- Historical trade prints table
CREATE TABLE IF NOT EXISTS historical_trade_prints (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    token_id TEXT NOT NULL,
    market_id TEXT NOT NULL,
    price REAL NOT NULL,
    size REAL NOT NULL,
    aggressor_side TEXT NOT NULL,
    fee_rate_bps INTEGER,
    source_time_ns INTEGER NOT NULL,
    arrival_time_ns INTEGER NOT NULL,
    local_seq INTEGER NOT NULL,
    exchange_trade_id TEXT,
    recorded_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now'))
);

-- Primary index: token + arrival time (most common query for replay)
CREATE INDEX IF NOT EXISTS idx_trade_prints_token_arrival
    ON historical_trade_prints(token_id, arrival_time_ns);

-- Index for time range queries across all tokens
CREATE INDEX IF NOT EXISTS idx_trade_prints_arrival
    ON historical_trade_prints(arrival_time_ns);

-- Index for market-based queries
CREATE INDEX IF NOT EXISTS idx_trade_prints_market_arrival
    ON historical_trade_prints(market_id, arrival_time_ns);

-- Index for deduplication by exchange trade ID
CREATE INDEX IF NOT EXISTS idx_trade_prints_exchange_id
    ON historical_trade_prints(exchange_trade_id) WHERE exchange_trade_id IS NOT NULL;

-- Stats table for per-token trade statistics
CREATE TABLE IF NOT EXISTS trade_print_stats (
    token_id TEXT PRIMARY KEY,
    first_arrival_ns INTEGER NOT NULL,
    last_arrival_ns INTEGER NOT NULL,
    total_trades INTEGER NOT NULL,
    total_volume REAL NOT NULL,
    updated_at INTEGER NOT NULL
) WITHOUT ROWID;
"#;

// =============================================================================
// Trade Print Storage
// =============================================================================

/// Persistent storage for historical trade prints.
pub struct TradePrintStorage {
    conn: Arc<Mutex<Connection>>,
    /// Local sequence counter for ordering.
    local_seq: AtomicU64,
    /// Stats counters.
    stats: TradeRecorderStats,
}

/// Recording statistics.
#[derive(Debug, Default)]
pub struct TradeRecorderStats {
    pub trades_recorded: AtomicU64,
    pub trades_skipped_duplicate: AtomicU64,
    pub trades_skipped_invalid: AtomicU64,
    pub batch_writes: AtomicU64,
    pub total_volume: AtomicU64, // Stored as size * 1_000_000 for precision
}

impl TradePrintStorage {
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
            .with_context(|| format!("Failed to open trade database: {}", db_path))?;

        // Initialize schema
        conn.execute_batch(TRADE_PRINT_SCHEMA)?;

        info!(path = %db_path, "Trade print storage opened");

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            local_seq: AtomicU64::new(0),
            stats: TradeRecorderStats::default(),
        })
    }

    /// Open in-memory storage (for testing).
    pub fn open_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(TRADE_PRINT_SCHEMA)?;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            local_seq: AtomicU64::new(0),
            stats: TradeRecorderStats::default(),
        })
    }

    /// Get next local sequence number.
    pub fn next_local_seq(&self) -> u64 {
        self.local_seq.fetch_add(1, Ordering::Relaxed)
    }

    /// Store a single trade print.
    pub fn store_trade(&self, trade: &RecordedTradePrint) -> Result<()> {
        // Skip invalid trades
        if trade.size <= 0.0 || trade.price <= 0.0 {
            self.stats.trades_skipped_invalid.fetch_add(1, Ordering::Relaxed);
            return Ok(());
        }

        let side_str = match trade.aggressor_side {
            Side::Buy => "BUY",
            Side::Sell => "SELL",
        };

        let conn = self.conn.lock();
        conn.execute(
            r#"
            INSERT INTO historical_trade_prints (
                token_id, market_id, price, size, aggressor_side, fee_rate_bps,
                source_time_ns, arrival_time_ns, local_seq, exchange_trade_id
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
            "#,
            params![
                trade.token_id,
                trade.market_id,
                trade.price,
                trade.size,
                side_str,
                trade.fee_rate_bps,
                trade.source_time_ns as i64,
                trade.arrival_time_ns as i64,
                trade.local_seq as i64,
                trade.exchange_trade_id,
            ],
        )?;

        self.stats.trades_recorded.fetch_add(1, Ordering::Relaxed);
        self.stats.total_volume.fetch_add(
            (trade.size * 1_000_000.0) as u64,
            Ordering::Relaxed,
        );

        Ok(())
    }

    /// Store multiple trade prints in a batch.
    pub fn store_batch(&self, trades: &[RecordedTradePrint]) -> Result<usize> {
        if trades.is_empty() {
            return Ok(0);
        }

        let conn = self.conn.lock();
        conn.execute("BEGIN IMMEDIATE", [])?;

        let mut count = 0;
        for trade in trades {
            if trade.size <= 0.0 || trade.price <= 0.0 {
                continue;
            }

            let side_str = match trade.aggressor_side {
                Side::Buy => "BUY",
                Side::Sell => "SELL",
            };

            let result = conn.execute(
                r#"
                INSERT INTO historical_trade_prints (
                    token_id, market_id, price, size, aggressor_side, fee_rate_bps,
                    source_time_ns, arrival_time_ns, local_seq, exchange_trade_id
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                "#,
                params![
                    trade.token_id,
                    trade.market_id,
                    trade.price,
                    trade.size,
                    side_str,
                    trade.fee_rate_bps,
                    trade.source_time_ns as i64,
                    trade.arrival_time_ns as i64,
                    trade.local_seq as i64,
                    trade.exchange_trade_id,
                ],
            );

            if result.is_ok() {
                count += 1;
            }
        }

        conn.execute("COMMIT", [])?;
        self.stats.batch_writes.fetch_add(1, Ordering::Relaxed);
        self.stats.trades_recorded.fetch_add(count, Ordering::Relaxed);

        Ok(count as usize)
    }

    /// Load trades for a token in a time range (by arrival_time).
    pub fn load_trades_in_range(
        &self,
        token_id: &str,
        start_arrival_ns: u64,
        end_arrival_ns: u64,
    ) -> Result<Vec<RecordedTradePrint>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            r#"
            SELECT token_id, market_id, price, size, aggressor_side, fee_rate_bps,
                   source_time_ns, arrival_time_ns, local_seq, exchange_trade_id
            FROM historical_trade_prints
            WHERE token_id = ?1 AND arrival_time_ns >= ?2 AND arrival_time_ns <= ?3
            ORDER BY arrival_time_ns ASC, local_seq ASC
            "#,
        )?;

        let trades = stmt
            .query_map(
                params![token_id, start_arrival_ns as i64, end_arrival_ns as i64],
                |row| {
                    let side_str: String = row.get(4)?;
                    let aggressor_side = match side_str.as_str() {
                        "BUY" => Side::Buy,
                        "SELL" => Side::Sell,
                        _ => Side::Buy,
                    };

                    Ok(RecordedTradePrint {
                        token_id: row.get(0)?,
                        market_id: row.get(1)?,
                        price: row.get(2)?,
                        size: row.get(3)?,
                        aggressor_side,
                        fee_rate_bps: row.get(5)?,
                        source_time_ns: row.get::<_, i64>(6)? as u64,
                        arrival_time_ns: row.get::<_, i64>(7)? as u64,
                        local_seq: row.get::<_, i64>(8)? as u64,
                        exchange_trade_id: row.get(9)?,
                    })
                },
            )?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(trades)
    }

    /// Load all trades in a time range (across all tokens).
    pub fn load_all_trades_in_range(
        &self,
        start_arrival_ns: u64,
        end_arrival_ns: u64,
    ) -> Result<Vec<RecordedTradePrint>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            r#"
            SELECT token_id, market_id, price, size, aggressor_side, fee_rate_bps,
                   source_time_ns, arrival_time_ns, local_seq, exchange_trade_id
            FROM historical_trade_prints
            WHERE arrival_time_ns >= ?1 AND arrival_time_ns <= ?2
            ORDER BY arrival_time_ns ASC, local_seq ASC
            "#,
        )?;

        let trades = stmt
            .query_map(params![start_arrival_ns as i64, end_arrival_ns as i64], |row| {
                let side_str: String = row.get(4)?;
                let aggressor_side = match side_str.as_str() {
                    "BUY" => Side::Buy,
                    "SELL" => Side::Sell,
                    _ => Side::Buy,
                };

                Ok(RecordedTradePrint {
                    token_id: row.get(0)?,
                    market_id: row.get(1)?,
                    price: row.get(2)?,
                    size: row.get(3)?,
                    aggressor_side,
                    fee_rate_bps: row.get(5)?,
                    source_time_ns: row.get::<_, i64>(6)? as u64,
                    arrival_time_ns: row.get::<_, i64>(7)? as u64,
                    local_seq: row.get::<_, i64>(8)? as u64,
                    exchange_trade_id: row.get(9)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(trades)
    }

    /// Get time coverage for a token.
    pub fn get_time_coverage(&self, token_id: &str) -> Result<Option<(u64, u64)>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            r#"
            SELECT MIN(arrival_time_ns), MAX(arrival_time_ns)
            FROM historical_trade_prints
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

    /// Count trades for a token.
    pub fn count_trades(&self, token_id: &str) -> Result<usize> {
        let conn = self.conn.lock();
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM historical_trade_prints WHERE token_id = ?1",
            params![token_id],
            |row| row.get(0),
        )?;
        Ok(count as usize)
    }

    /// Get total volume for a token.
    pub fn get_total_volume(&self, token_id: &str) -> Result<f64> {
        let conn = self.conn.lock();
        let volume: f64 = conn.query_row(
            "SELECT COALESCE(SUM(size), 0) FROM historical_trade_prints WHERE token_id = ?1",
            params![token_id],
            |row| row.get(0),
        )?;
        Ok(volume)
    }

    /// Get all unique token IDs in storage.
    pub fn get_token_ids(&self) -> Result<Vec<String>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT DISTINCT token_id FROM historical_trade_prints ORDER BY token_id",
        )?;
        let ids = stmt
            .query_map([], |row| row.get(0))?
            .collect::<Result<Vec<String>, _>>()?;
        Ok(ids)
    }

    /// Get recording statistics.
    pub fn stats(&self) -> &TradeRecorderStats {
        &self.stats
    }

    /// Update per-token statistics.
    pub fn update_token_stats(&self, token_id: &str) -> Result<()> {
        let conn = self.conn.lock();

        // Get aggregated stats
        let (first_arrival, last_arrival, total_trades, total_volume) = conn.query_row(
            r#"
            SELECT MIN(arrival_time_ns), MAX(arrival_time_ns), COUNT(*), COALESCE(SUM(size), 0)
            FROM historical_trade_prints
            WHERE token_id = ?1
            "#,
            params![token_id],
            |row| {
                Ok((
                    row.get::<_, Option<i64>>(0)?,
                    row.get::<_, Option<i64>>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, f64>(3)?,
                ))
            },
        )?;

        if first_arrival.is_none() {
            return Ok(()); // No data
        }

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        conn.execute(
            r#"
            INSERT INTO trade_print_stats (
                token_id, first_arrival_ns, last_arrival_ns, total_trades, total_volume, updated_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
            ON CONFLICT(token_id) DO UPDATE SET
                first_arrival_ns = excluded.first_arrival_ns,
                last_arrival_ns = excluded.last_arrival_ns,
                total_trades = excluded.total_trades,
                total_volume = excluded.total_volume,
                updated_at = excluded.updated_at
            "#,
            params![
                token_id,
                first_arrival.unwrap(),
                last_arrival.unwrap(),
                total_trades,
                total_volume,
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
pub enum TradeRecorderMessage {
    Trade(RecordedTradePrint),
    Flush,
    Shutdown,
}

/// Async trade print recorder with buffered writes.
pub struct AsyncTradeRecorder {
    tx: mpsc::Sender<TradeRecorderMessage>,
}

impl AsyncTradeRecorder {
    /// Spawn the async recorder.
    pub fn spawn(storage: Arc<TradePrintStorage>, buffer_size: usize) -> Self {
        let (tx, rx) = mpsc::channel(buffer_size);

        tokio::spawn(async move {
            Self::run_writer(storage, rx, buffer_size).await;
        });

        Self { tx }
    }

    /// Record a trade print (non-blocking).
    pub fn record(&self, trade: RecordedTradePrint) {
        let _ = self.tx.try_send(TradeRecorderMessage::Trade(trade));
    }

    /// Flush pending writes.
    pub async fn flush(&self) {
        let _ = self.tx.send(TradeRecorderMessage::Flush).await;
    }

    /// Shutdown the recorder.
    pub async fn shutdown(&self) {
        let _ = self.tx.send(TradeRecorderMessage::Shutdown).await;
    }

    async fn run_writer(
        storage: Arc<TradePrintStorage>,
        mut rx: mpsc::Receiver<TradeRecorderMessage>,
        buffer_size: usize,
    ) {
        let mut buffer: Vec<RecordedTradePrint> = Vec::with_capacity(buffer_size);
        let flush_interval = Duration::from_millis(100);
        let mut last_flush = std::time::Instant::now();

        loop {
            tokio::select! {
                msg = rx.recv() => {
                    match msg {
                        Some(TradeRecorderMessage::Trade(trade)) => {
                            buffer.push(trade);

                            // Flush if buffer is full or interval elapsed
                            if buffer.len() >= buffer_size || last_flush.elapsed() > flush_interval {
                                if let Err(e) = storage.store_batch(&buffer) {
                                    warn!(error = %e, "Failed to store trade prints");
                                }
                                buffer.clear();
                                last_flush = std::time::Instant::now();
                            }
                        }
                        Some(TradeRecorderMessage::Flush) => {
                            if !buffer.is_empty() {
                                if let Err(e) = storage.store_batch(&buffer) {
                                    warn!(error = %e, "Failed to store trade prints on flush");
                                }
                                buffer.clear();
                            }
                            last_flush = std::time::Instant::now();
                        }
                        Some(TradeRecorderMessage::Shutdown) | None => {
                            // Final flush before shutdown
                            if !buffer.is_empty() {
                                let _ = storage.store_batch(&buffer);
                            }
                            info!("Trade print recorder shutting down");
                            return;
                        }
                    }
                }
                _ = tokio::time::sleep(flush_interval) => {
                    // Periodic flush
                    if !buffer.is_empty() {
                        if let Err(e) = storage.store_batch(&buffer) {
                            warn!(error = %e, "Failed to store trade prints on timer");
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

/// Replay feed for loading recorded trade prints into backtests.
pub struct TradePrintFeed {
    trades: Vec<RecordedTradePrint>,
    current_index: usize,
}

impl TradePrintFeed {
    /// Create from stored trades.
    pub fn new(trades: Vec<RecordedTradePrint>) -> Self {
        Self {
            trades,
            current_index: 0,
        }
    }

    /// Load from storage for a token in a time range.
    pub fn from_storage(
        storage: &TradePrintStorage,
        token_id: &str,
        start_ns: u64,
        end_ns: u64,
    ) -> Result<Self> {
        let trades = storage.load_trades_in_range(token_id, start_ns, end_ns)?;
        Ok(Self::new(trades))
    }

    /// Load all tokens from storage in a time range.
    pub fn from_storage_all(
        storage: &TradePrintStorage,
        start_ns: u64,
        end_ns: u64,
    ) -> Result<Self> {
        let trades = storage.load_all_trades_in_range(start_ns, end_ns)?;
        Ok(Self::new(trades))
    }

    /// Get total number of trades.
    pub fn len(&self) -> usize {
        self.trades.len()
    }

    /// Check if empty.
    pub fn is_empty(&self) -> bool {
        self.trades.is_empty()
    }

    /// Get time range covered.
    pub fn time_range(&self) -> Option<(u64, u64)> {
        if self.trades.is_empty() {
            None
        } else {
            Some((
                self.trades.first().unwrap().arrival_time_ns,
                self.trades.last().unwrap().arrival_time_ns,
            ))
        }
    }

    /// Reset to beginning for new replay.
    pub fn reset(&mut self) {
        self.current_index = 0;
    }

    /// Get next trade in chronological order.
    pub fn next(&mut self) -> Option<&RecordedTradePrint> {
        if self.current_index < self.trades.len() {
            let trade = &self.trades[self.current_index];
            self.current_index += 1;
            Some(trade)
        } else {
            None
        }
    }

    /// Peek at next trade without advancing.
    pub fn peek(&self) -> Option<&RecordedTradePrint> {
        self.trades.get(self.current_index)
    }

    /// Get all trades.
    pub fn trades(&self) -> &[RecordedTradePrint] {
        &self.trades
    }

    /// Get total volume in feed.
    pub fn total_volume(&self) -> f64 {
        self.trades.iter().map(|t| t.size).sum()
    }
}

// =============================================================================
// Queue Consumption Helper
// =============================================================================

/// Helper for tracking how trades consume queue depth.
#[derive(Debug, Clone, Default)]
pub struct QueueConsumptionTracker {
    /// Map of token_id -> price_level -> cumulative consumed size
    consumed: std::collections::HashMap<String, std::collections::HashMap<i64, f64>>,
}

impl QueueConsumptionTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a trade's consumption of queue depth.
    pub fn record_trade(&mut self, trade: &RecordedTradePrint) {
        let token_consumed = self
            .consumed
            .entry(trade.token_id.clone())
            .or_insert_with(std::collections::HashMap::new);

        // Convert price to ticks (assuming 0.01 tick size)
        let price_ticks = (trade.price * 100.0).round() as i64;

        *token_consumed.entry(price_ticks).or_insert(0.0) += trade.size;
    }

    /// Get total consumed size at a price level.
    pub fn consumed_at_level(&self, token_id: &str, price_ticks: i64) -> f64 {
        self.consumed
            .get(token_id)
            .and_then(|m| m.get(&price_ticks))
            .copied()
            .unwrap_or(0.0)
    }

    /// Reset consumption tracking.
    pub fn reset(&mut self) {
        self.consumed.clear();
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_trade(token_id: &str, arrival_ns: u64, seq: u64, size: f64) -> RecordedTradePrint {
        RecordedTradePrint {
            token_id: token_id.to_string(),
            market_id: "0xmarket".to_string(),
            price: 0.50,
            size,
            aggressor_side: Side::Buy,
            fee_rate_bps: Some(0),
            source_time_ns: arrival_ns.saturating_sub(1_000_000),
            arrival_time_ns: arrival_ns,
            local_seq: seq,
            exchange_trade_id: None,
        }
    }

    #[test]
    fn test_trade_creation() {
        let trade = make_test_trade("TOKEN1", 1_000_000_000, 1, 100.0);

        assert_eq!(trade.token_id, "TOKEN1");
        assert_eq!(trade.size, 100.0);
        assert_eq!(trade.aggressor_side, Side::Buy);
        assert_eq!(trade.consumes_side(), Side::Sell); // Buy consumes asks
    }

    #[test]
    fn test_storage_basic() {
        let storage = TradePrintStorage::open_memory().unwrap();
        let trade = make_test_trade("TOKEN1", 1_000_000_000, 1, 100.0);

        storage.store_trade(&trade).unwrap();

        let count = storage.count_trades("TOKEN1").unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn test_storage_batch() {
        let storage = TradePrintStorage::open_memory().unwrap();

        let trades: Vec<_> = (1..=10)
            .map(|i| make_test_trade("TOKEN1", i * 1_000_000_000, i as u64, 10.0 * i as f64))
            .collect();

        let stored = storage.store_batch(&trades).unwrap();
        assert_eq!(stored, 10);

        let count = storage.count_trades("TOKEN1").unwrap();
        assert_eq!(count, 10);

        let volume = storage.get_total_volume("TOKEN1").unwrap();
        assert!((volume - 550.0).abs() < 0.01); // 10 + 20 + ... + 100 = 550
    }

    #[test]
    fn test_storage_load_range() {
        let storage = TradePrintStorage::open_memory().unwrap();

        // Store trades at 1s, 2s, 3s, 4s, 5s
        let trades: Vec<_> = (1..=5)
            .map(|i| make_test_trade("TOKEN1", i * 1_000_000_000_u64, i as u64, 100.0))
            .collect();
        storage.store_batch(&trades).unwrap();

        // Load 2s to 4s
        let loaded = storage
            .load_trades_in_range("TOKEN1", 2_000_000_000, 4_000_000_000)
            .unwrap();
        assert_eq!(loaded.len(), 3);
        assert_eq!(loaded[0].local_seq, 2);
        assert_eq!(loaded[2].local_seq, 4);
    }

    #[test]
    fn test_replay_feed() {
        let storage = TradePrintStorage::open_memory().unwrap();

        let trades: Vec<_> = (1..=5)
            .map(|i| make_test_trade("TOKEN1", i * 1_000_000_000_u64, i as u64, 100.0))
            .collect();
        storage.store_batch(&trades).unwrap();

        let mut feed =
            TradePrintFeed::from_storage(&storage, "TOKEN1", 0, i64::MAX as u64).unwrap();

        assert_eq!(feed.len(), 5);
        assert_eq!(feed.total_volume(), 500.0);

        // Iterate through
        let mut count = 0;
        while feed.next().is_some() {
            count += 1;
        }
        assert_eq!(count, 5);

        // Reset and iterate again
        feed.reset();
        let first = feed.next().unwrap();
        assert_eq!(first.local_seq, 1);
    }

    #[test]
    fn test_queue_consumption_tracker() {
        let mut tracker = QueueConsumptionTracker::new();

        // Simulate trades consuming the queue
        let trade1 = make_test_trade("TOKEN1", 1_000_000_000, 1, 50.0);
        let trade2 = make_test_trade("TOKEN1", 2_000_000_000, 2, 75.0);

        tracker.record_trade(&trade1);
        tracker.record_trade(&trade2);

        // Check consumption at price 0.50 (50 ticks)
        let consumed = tracker.consumed_at_level("TOKEN1", 50);
        assert!((consumed - 125.0).abs() < 0.01);
    }

    #[test]
    fn test_to_event() {
        let trade = make_test_trade("TOKEN1", 1_000_000_000, 1, 100.0);
        let event = trade.to_event();

        match event {
            Event::TradePrint {
                token_id,
                price,
                size,
                aggressor_side,
                ..
            } => {
                assert_eq!(token_id, "TOKEN1");
                assert_eq!(price, 0.50);
                assert_eq!(size, 100.0);
                assert_eq!(aggressor_side, Side::Buy);
            }
            _ => panic!("Expected TradePrint event"),
        }
    }
}

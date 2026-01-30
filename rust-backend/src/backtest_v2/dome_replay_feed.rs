//! Dome Replay Feed
//!
//! Adapter for reading dome_replay_data_v3.db (dome_orderbooks table)
//! and emitting L2BookSnapshot events compatible with the backtest engine.
//!
//! Converts timestamp_ms -> Nanos (multiply by 1_000_000).

use crate::backtest_v2::clock::{Nanos, NANOS_PER_MILLI};
use crate::backtest_v2::events::{Event, Level, TimestampedEvent};
use crate::backtest_v2::feed::MarketDataFeed;
use crate::backtest_v2::queue::StreamSource;
use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use serde::Deserialize;
use std::path::Path;

/// Raw depth JSON structure from dome_orderbooks.depth_json
#[derive(Debug, Deserialize)]
struct DepthJson {
    bids: Vec<[f64; 2]>, // [[price, size], ...]
    asks: Vec<[f64; 2]>,
}

/// A single orderbook snapshot from dome_orderbooks
#[derive(Debug, Clone)]
pub struct DomeOrderbookSnapshot {
    pub token_id: String,
    pub timestamp_ms: i64,
    pub best_bid: Option<f64>,
    pub best_bid_size: Option<f64>,
    pub best_ask: Option<f64>,
    pub best_ask_size: Option<f64>,
    pub bids: Vec<Level>,
    pub asks: Vec<Level>,
}

impl DomeOrderbookSnapshot {
    /// Convert timestamp_ms to Nanos
    #[inline]
    pub fn ingest_ts_ns(&self) -> Nanos {
        self.timestamp_ms * NANOS_PER_MILLI
    }

    /// Convert to TimestampedEvent (L2BookSnapshot)
    pub fn to_timestamped_event(&self, seq: u64) -> TimestampedEvent {
        let mut event = TimestampedEvent::new(
            self.ingest_ts_ns(),
            StreamSource::MarketData as u8,
            Event::L2BookSnapshot {
                token_id: self.token_id.clone(),
                bids: self.bids.clone(),
                asks: self.asks.clone(),
                exchange_seq: seq,
            },
        );
        event.seq = seq;
        event
    }
}

/// Dome replay feed that reads from dome_orderbooks table.
/// Implements MarketDataFeed trait for use with the backtest orchestrator.
pub struct DomeReplayFeed {
    /// Pre-loaded snapshots sorted by timestamp_ms ASC
    snapshots: Vec<DomeOrderbookSnapshot>,
    /// Current position in snapshot stream
    index: usize,
    /// Feed name for diagnostics
    name: String,
}

impl DomeReplayFeed {
    /// Open dome_replay DB and load snapshots for a token in time range.
    /// Uses inclusive start, exclusive end: [start_ns, end_ns)
    pub fn from_db(
        db_path: &str,
        token_id: &str,
        start_ns: Nanos,
        end_ns: Nanos,
    ) -> Result<Self> {
        let path = Path::new(db_path);
        if !path.exists() {
            anyhow::bail!("Dome replay DB not found: {}", db_path);
        }

        let conn = Connection::open(db_path)
            .with_context(|| format!("Failed to open dome replay DB: {}", db_path))?;

        // Convert Nanos bounds to milliseconds for query
        let start_ms = start_ns / NANOS_PER_MILLI;
        let end_ms = end_ns / NANOS_PER_MILLI;

        let mut stmt = conn.prepare(
            r#"
            SELECT token_id, timestamp_ms, best_bid, best_bid_size, best_ask, best_ask_size, depth_json
            FROM dome_orderbooks
            WHERE token_id = ?1 AND timestamp_ms >= ?2 AND timestamp_ms < ?3
            ORDER BY timestamp_ms ASC
            "#,
        )?;

        let rows = stmt.query_map(params![token_id, start_ms, end_ms], |row| {
            let token_id: String = row.get(0)?;
            let timestamp_ms: i64 = row.get(1)?;
            let best_bid: Option<f64> = row.get(2)?;
            let best_bid_size: Option<f64> = row.get(3)?;
            let best_ask: Option<f64> = row.get(4)?;
            let best_ask_size: Option<f64> = row.get(5)?;
            let depth_json: Option<String> = row.get(6)?;

            // Parse depth JSON into bids/asks levels
            let (bids, asks) = if let Some(json_str) = depth_json {
                match serde_json::from_str::<DepthJson>(&json_str) {
                    Ok(depth) => {
                        let bids: Vec<Level> = depth
                            .bids
                            .iter()
                            .map(|[price, size]| Level::new(*price, *size))
                            .collect();
                        let asks: Vec<Level> = depth
                            .asks
                            .iter()
                            .map(|[price, size]| Level::new(*price, *size))
                            .collect();
                        (bids, asks)
                    }
                    Err(_) => (Vec::new(), Vec::new()),
                }
            } else {
                // Fallback to best_bid/best_ask if no depth_json
                let mut bids = Vec::new();
                let mut asks = Vec::new();
                if let (Some(p), Some(s)) = (best_bid, best_bid_size) {
                    bids.push(Level::new(p, s));
                }
                if let (Some(p), Some(s)) = (best_ask, best_ask_size) {
                    asks.push(Level::new(p, s));
                }
                (bids, asks)
            };

            Ok(DomeOrderbookSnapshot {
                token_id,
                timestamp_ms,
                best_bid,
                best_bid_size,
                best_ask,
                best_ask_size,
                bids,
                asks,
            })
        })?;

        let mut snapshots = Vec::new();
        for row in rows {
            snapshots.push(row?);
        }

        Ok(Self {
            snapshots,
            index: 0,
            name: format!("DomeReplayFeed({})", token_id),
        })
    }

    /// Load all tokens in the time range (multi-token feed).
    pub fn from_db_all_tokens(
        db_path: &str,
        start_ns: Nanos,
        end_ns: Nanos,
    ) -> Result<Self> {
        let path = Path::new(db_path);
        if !path.exists() {
            anyhow::bail!("Dome replay DB not found: {}", db_path);
        }

        let conn = Connection::open(db_path)
            .with_context(|| format!("Failed to open dome replay DB: {}", db_path))?;

        let start_ms = start_ns / NANOS_PER_MILLI;
        let end_ms = end_ns / NANOS_PER_MILLI;

        let mut stmt = conn.prepare(
            r#"
            SELECT token_id, timestamp_ms, best_bid, best_bid_size, best_ask, best_ask_size, depth_json
            FROM dome_orderbooks
            WHERE timestamp_ms >= ?1 AND timestamp_ms < ?2
            ORDER BY timestamp_ms ASC
            "#,
        )?;

        let rows = stmt.query_map(params![start_ms, end_ms], |row| {
            let token_id: String = row.get(0)?;
            let timestamp_ms: i64 = row.get(1)?;
            let best_bid: Option<f64> = row.get(2)?;
            let best_bid_size: Option<f64> = row.get(3)?;
            let best_ask: Option<f64> = row.get(4)?;
            let best_ask_size: Option<f64> = row.get(5)?;
            let depth_json: Option<String> = row.get(6)?;

            let (bids, asks) = if let Some(json_str) = depth_json {
                match serde_json::from_str::<DepthJson>(&json_str) {
                    Ok(depth) => {
                        let bids: Vec<Level> = depth
                            .bids
                            .iter()
                            .map(|[price, size]| Level::new(*price, *size))
                            .collect();
                        let asks: Vec<Level> = depth
                            .asks
                            .iter()
                            .map(|[price, size]| Level::new(*price, *size))
                            .collect();
                        (bids, asks)
                    }
                    Err(_) => (Vec::new(), Vec::new()),
                }
            } else {
                let mut bids = Vec::new();
                let mut asks = Vec::new();
                if let (Some(p), Some(s)) = (best_bid, best_bid_size) {
                    bids.push(Level::new(p, s));
                }
                if let (Some(p), Some(s)) = (best_ask, best_ask_size) {
                    asks.push(Level::new(p, s));
                }
                (bids, asks)
            };

            Ok(DomeOrderbookSnapshot {
                token_id,
                timestamp_ms,
                best_bid,
                best_bid_size,
                best_ask,
                best_ask_size,
                bids,
                asks,
            })
        })?;

        let mut snapshots = Vec::new();
        for row in rows {
            snapshots.push(row?);
        }

        Ok(Self {
            snapshots,
            index: 0,
            name: "DomeReplayFeed(all)".to_string(),
        })
    }

    /// Get total snapshot count.
    pub fn len(&self) -> usize {
        self.snapshots.len()
    }

    /// Check if empty.
    pub fn is_empty(&self) -> bool {
        self.snapshots.is_empty()
    }

    /// Get first timestamp in feed (ms).
    pub fn first_ts_ms(&self) -> Option<i64> {
        self.snapshots.first().map(|s| s.timestamp_ms)
    }

    /// Get last timestamp in feed (ms).
    pub fn last_ts_ms(&self) -> Option<i64> {
        self.snapshots.last().map(|s| s.timestamp_ms)
    }

    /// Get distinct token IDs in the feed.
    pub fn token_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.snapshots.iter().map(|s| s.token_id.clone()).collect();
        ids.sort();
        ids.dedup();
        ids
    }
}

impl MarketDataFeed for DomeReplayFeed {
    fn next_event(&mut self) -> Option<TimestampedEvent> {
        if self.index < self.snapshots.len() {
            let snapshot = &self.snapshots[self.index];
            let event = snapshot.to_timestamped_event(self.index as u64);
            self.index += 1;
            Some(event)
        } else {
            None
        }
    }

    fn peek_time(&self) -> Option<Nanos> {
        self.snapshots.get(self.index).map(|s| s.ingest_ts_ns())
    }

    fn reset(&mut self) {
        self.index = 0;
    }

    fn remaining(&self) -> Option<usize> {
        Some(self.snapshots.len().saturating_sub(self.index))
    }

    fn name(&self) -> &str {
        &self.name
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_timestamp_conversion() {
        let snapshot = DomeOrderbookSnapshot {
            token_id: "test".to_string(),
            timestamp_ms: 1769413205000,
            best_bid: Some(0.5),
            best_bid_size: Some(100.0),
            best_ask: Some(0.51),
            best_ask_size: Some(100.0),
            bids: vec![Level::new(0.5, 100.0)],
            asks: vec![Level::new(0.51, 100.0)],
        };

        // 1769413205000 ms * 1_000_000 = 1769413205000000000000 ns
        assert_eq!(snapshot.ingest_ts_ns(), 1769413205000 * NANOS_PER_MILLI);
    }

    #[test]
    fn test_depth_json_parsing() {
        let json_str = r#"{"bids": [[0.5, 100.0], [0.49, 200.0]], "asks": [[0.51, 150.0]]}"#;
        let depth: DepthJson = serde_json::from_str(json_str).unwrap();
        
        assert_eq!(depth.bids.len(), 2);
        assert_eq!(depth.bids[0], [0.5, 100.0]);
        assert_eq!(depth.asks.len(), 1);
    }
}

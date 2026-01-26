//! Chainlink Price Feed Abstraction
//!
//! Production-grade Chainlink integration for settlement reference prices.
//! Supports live polling, historical backfill, and offline replay.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

use crate::backtest_v2::clock::Nanos;

// =============================================================================
// Configuration
// =============================================================================

/// Configuration for a single Chainlink feed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChainlinkFeedConfig {
    /// Chain ID (e.g., 137 for Polygon mainnet).
    pub chain_id: u64,
    /// AggregatorV3Interface proxy address.
    pub feed_proxy_address: String,
    /// Decimals for this feed (usually 8 for USD pairs).
    pub decimals: u8,
    /// Asset symbol (e.g., "BTC", "ETH").
    pub asset_symbol: String,
    /// Primary RPC endpoint (HTTP).
    pub rpc_endpoint: String,
    /// Optional WebSocket endpoint for subscription (if available).
    pub ws_endpoint: Option<String>,
    /// Polling interval in milliseconds for live mode.
    pub polling_interval_ms: u64,
    /// How far back to backfill (in seconds).
    pub backfill_range_secs: u64,
    /// Deviation threshold (from Chainlink docs).
    pub deviation_threshold: f64,
    /// Heartbeat interval (from Chainlink docs).
    pub heartbeat_secs: u64,
}

impl ChainlinkFeedConfig {
    /// BTC/USD on Polygon Mainnet.
    pub fn btc_usd_polygon() -> Self {
        Self {
            chain_id: 137,
            feed_proxy_address: "0xc907E116054Ad103354f2D350FD2514433D57F6f".to_string(),
            decimals: 8,
            asset_symbol: "BTC".to_string(),
            rpc_endpoint: String::new(), // Must be set from env
            ws_endpoint: None,
            polling_interval_ms: 1000,
            backfill_range_secs: 86400, // 1 day
            deviation_threshold: 0.001, // 0.1%
            heartbeat_secs: 2,
        }
    }

    /// ETH/USD on Polygon Mainnet.
    pub fn eth_usd_polygon() -> Self {
        Self {
            chain_id: 137,
            feed_proxy_address: "0xF9680D99D6C9589e2a93a78A04A279e509205945".to_string(),
            decimals: 8,
            asset_symbol: "ETH".to_string(),
            rpc_endpoint: String::new(),
            ws_endpoint: None,
            polling_interval_ms: 1000,
            backfill_range_secs: 86400,
            deviation_threshold: 0.001,
            heartbeat_secs: 2,
        }
    }

    /// SOL/USD on Polygon Mainnet.
    pub fn sol_usd_polygon() -> Self {
        Self {
            chain_id: 137,
            feed_proxy_address: "0x10C8264C0935b3B9870013e057f330Ff3e9C56dC".to_string(),
            decimals: 8,
            asset_symbol: "SOL".to_string(),
            rpc_endpoint: String::new(),
            ws_endpoint: None,
            polling_interval_ms: 1000,
            backfill_range_secs: 86400,
            deviation_threshold: 0.005, // 0.5%
            heartbeat_secs: 2,
        }
    }

    /// XRP/USD on Polygon Mainnet.
    pub fn xrp_usd_polygon() -> Self {
        Self {
            chain_id: 137,
            feed_proxy_address: "0x785ba89291f676b5386652eB12b30cF361020694".to_string(),
            decimals: 8,
            asset_symbol: "XRP".to_string(),
            rpc_endpoint: String::new(),
            ws_endpoint: None,
            polling_interval_ms: 1000,
            backfill_range_secs: 86400,
            deviation_threshold: 0.005,
            heartbeat_secs: 2,
        }
    }

    /// Generate a unique feed ID (hash of chain_id + proxy address).
    pub fn feed_id(&self) -> String {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut hasher = DefaultHasher::new();
        self.chain_id.hash(&mut hasher);
        self.feed_proxy_address.to_lowercase().hash(&mut hasher);
        format!("{:016x}", hasher.finish())
    }

    /// Load config from environment with asset-specific overrides.
    pub fn from_env(asset: &str) -> Option<Self> {
        let rpc_endpoint = std::env::var("POLYGON_RPC_URL")
            .or_else(|_| std::env::var("CHAINLINK_RPC_URL"))
            .ok()?;

        if rpc_endpoint.is_empty() {
            return None;
        }

        let mut config = match asset.to_uppercase().as_str() {
            "BTC" => Self::btc_usd_polygon(),
            "ETH" => Self::eth_usd_polygon(),
            "SOL" => Self::sol_usd_polygon(),
            "XRP" => Self::xrp_usd_polygon(),
            _ => return None,
        };

        config.rpc_endpoint = rpc_endpoint;

        // Override feed address if explicitly set
        let env_key = format!("CHAINLINK_{}_USD_ADDRESS", asset.to_uppercase());
        if let Ok(addr) = std::env::var(&env_key) {
            config.feed_proxy_address = addr;
        }

        // Override polling interval
        if let Ok(v) = std::env::var("CHAINLINK_POLL_INTERVAL_MS") {
            if let Ok(ms) = v.parse() {
                config.polling_interval_ms = ms;
            }
        }

        Some(config)
    }
}

// =============================================================================
// Chainlink Round Data
// =============================================================================

/// A single Chainlink price round with full metadata.
///
/// **Timestamp Semantics:**
/// - `updated_at`: Oracle source time (when Chainlink network agreed on price)
/// - `ingest_arrival_time_ns`: When WE observed/ingested this round (for visibility)
/// - `ingest_seq`: Local monotonic sequence for ordering
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChainlinkRound {
    /// Unique feed identifier (hash of chain_id + proxy).
    pub feed_id: String,
    /// Chainlink round ID (uint80, stored as u128 for safety).
    pub round_id: u128,
    /// Oracle answer (price with decimals, stored as i128 for precision).
    pub answer: i128,
    /// Oracle source time: when this round was updated on-chain.
    pub updated_at: u64,
    /// Round that this answer was computed in.
    pub answered_in_round: u128,
    /// When this round started.
    pub started_at: u64,
    /// OUR arrival time: when we ingested this round (nanoseconds).
    pub ingest_arrival_time_ns: u64,
    /// Local monotonic sequence for deterministic ordering.
    pub ingest_seq: u64,
    /// Decimals for this feed (for decoding answer to float).
    pub decimals: u8,
    /// Asset symbol (for convenience).
    pub asset_symbol: String,
    /// Optional raw source data hash for provenance.
    pub raw_source_hash: Option<String>,
}

impl ChainlinkRound {
    /// Decode the answer to a floating-point price.
    pub fn price(&self) -> f64 {
        let divisor = 10f64.powi(self.decimals as i32);
        (self.answer as f64) / divisor
    }

    /// Convert oracle timestamp to nanoseconds.
    pub fn updated_at_ns(&self) -> Nanos {
        (self.updated_at as i64) * 1_000_000_000
    }

    /// Check if this round is valid (non-zero answer and timestamps).
    pub fn is_valid(&self) -> bool {
        self.answer != 0 && self.updated_at != 0
    }

    /// Check if this round is stale (answered_in_round < round_id).
    pub fn is_stale(&self) -> bool {
        self.answered_in_round < self.round_id
    }
}

// =============================================================================
// Live Ingestor
// =============================================================================

/// Live Chainlink data ingestor with RPC polling.
pub struct ChainlinkIngestor {
    config: ChainlinkFeedConfig,
    client: reqwest::Client,
    /// Latest round seen.
    latest_round: Arc<RwLock<Option<ChainlinkRound>>>,
    /// Monotonic sequence counter.
    ingest_seq: AtomicU64,
    /// All rounds observed (for short-term lookback).
    rounds_cache: Arc<RwLock<BTreeMap<u128, ChainlinkRound>>>,
    /// Maximum cache size.
    max_cache_size: usize,
}

impl ChainlinkIngestor {
    pub fn new(config: ChainlinkFeedConfig) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .expect("failed to build HTTP client");

        Self {
            config,
            client,
            latest_round: Arc::new(RwLock::new(None)),
            ingest_seq: AtomicU64::new(0),
            rounds_cache: Arc::new(RwLock::new(BTreeMap::new())),
            max_cache_size: 10_000,
        }
    }

    /// Get the feed configuration.
    pub fn config(&self) -> &ChainlinkFeedConfig {
        &self.config
    }

    /// Fetch latest round data from Chainlink.
    pub async fn fetch_latest_round(&self) -> Result<ChainlinkRound> {
        let arrival_time_ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos() as u64;

        // latestRoundData() selector: 0xfeaf968c
        let call_data = "0xfeaf968c";

        let payload = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "eth_call",
            "params": [{
                "to": &self.config.feed_proxy_address,
                "data": call_data
            }, "latest"],
            "id": 1
        });

        let response: JsonRpcResponse = self
            .client
            .post(&self.config.rpc_endpoint)
            .json(&payload)
            .send()
            .await
            .context("RPC request failed")?
            .json()
            .await
            .context("failed to parse RPC response")?;

        if let Some(err) = response.error {
            return Err(anyhow::anyhow!("RPC error: {:?}", err));
        }

        let result = response
            .result
            .ok_or_else(|| anyhow::anyhow!("no result in RPC response"))?;

        let round = self.decode_round_data(&result, arrival_time_ns)?;
        Ok(round)
    }

    /// Fetch a specific round by ID.
    pub async fn fetch_round(&self, round_id: u128) -> Result<ChainlinkRound> {
        let arrival_time_ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos() as u64;

        // getRoundData(uint80) selector: 0x9a6fc8f5
        // Encode round_id as uint80 (right-padded to 32 bytes)
        let round_id_hex = format!("{:064x}", round_id);
        let call_data = format!("0x9a6fc8f5{}", round_id_hex);

        let payload = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "eth_call",
            "params": [{
                "to": &self.config.feed_proxy_address,
                "data": call_data
            }, "latest"],
            "id": 1
        });

        let response: JsonRpcResponse = self
            .client
            .post(&self.config.rpc_endpoint)
            .json(&payload)
            .send()
            .await
            .context("RPC request failed")?
            .json()
            .await
            .context("failed to parse RPC response")?;

        if let Some(err) = response.error {
            return Err(anyhow::anyhow!("RPC error: {:?}", err));
        }

        let result = response
            .result
            .ok_or_else(|| anyhow::anyhow!("no result in RPC response"))?;

        let round = self.decode_round_data(&result, arrival_time_ns)?;

        // Validate round_id matches
        if round.round_id != round_id {
            return Err(anyhow::anyhow!(
                "Round ID mismatch: requested {} got {}",
                round_id,
                round.round_id
            ));
        }

        Ok(round)
    }

    /// Decode ABI-encoded round data.
    fn decode_round_data(&self, hex_result: &str, arrival_time_ns: u64) -> Result<ChainlinkRound> {
        let bytes = hex::decode(hex_result.trim_start_matches("0x"))
            .context("failed to decode hex response")?;

        if bytes.len() < 160 {
            return Err(anyhow::anyhow!(
                "response too short: {} bytes, expected 160",
                bytes.len()
            ));
        }

        // Parse: roundId (32), answer (32), startedAt (32), updatedAt (32), answeredInRound (32)
        let round_id = decode_u128(&bytes[0..32]);
        let answer = decode_i128(&bytes[32..64]);
        let started_at = decode_u64(&bytes[64..96]);
        let updated_at = decode_u64(&bytes[96..128]);
        let answered_in_round = decode_u128(&bytes[128..160]);

        let seq = self.ingest_seq.fetch_add(1, Ordering::Relaxed);

        Ok(ChainlinkRound {
            feed_id: self.config.feed_id(),
            round_id,
            answer,
            updated_at,
            answered_in_round,
            started_at,
            ingest_arrival_time_ns: arrival_time_ns,
            ingest_seq: seq,
            decimals: self.config.decimals,
            asset_symbol: self.config.asset_symbol.clone(),
            raw_source_hash: None,
        })
    }

    /// Poll and update latest round. Returns the round if it's new.
    pub async fn poll(&self) -> Result<Option<ChainlinkRound>> {
        let round = self.fetch_latest_round().await?;

        let is_new = {
            let latest = self.latest_round.read().await;
            latest.as_ref().map(|r| r.round_id) != Some(round.round_id)
        };

        if is_new {
            // Update cache
            {
                let mut cache = self.rounds_cache.write().await;
                cache.insert(round.round_id, round.clone());

                // Prune cache if too large
                while cache.len() > self.max_cache_size {
                    if let Some((&oldest_key, _)) = cache.iter().next() {
                        cache.remove(&oldest_key);
                    }
                }
            }

            // Update latest
            {
                let mut latest = self.latest_round.write().await;
                *latest = Some(round.clone());
            }

            Ok(Some(round))
        } else {
            Ok(None)
        }
    }

    /// Get latest round (from cache).
    pub async fn get_latest(&self) -> Option<ChainlinkRound> {
        self.latest_round.read().await.clone()
    }

    /// Get a round by ID from cache.
    pub async fn get_round(&self, round_id: u128) -> Option<ChainlinkRound> {
        self.rounds_cache.read().await.get(&round_id).cloned()
    }

    /// Get the round closest to a target timestamp (updated_at).
    pub async fn get_round_at_or_before(&self, target_ts: u64) -> Option<ChainlinkRound> {
        let cache = self.rounds_cache.read().await;

        // Find the round with largest updated_at <= target_ts
        let mut best: Option<&ChainlinkRound> = None;
        for round in cache.values() {
            if round.updated_at <= target_ts {
                if best.map(|b| b.updated_at < round.updated_at).unwrap_or(true) {
                    best = Some(round);
                }
            }
        }

        best.cloned()
    }

    /// Backfill historical rounds by iterating backward from latest.
    pub async fn backfill(&self, from_round_id: u128, count: usize) -> Result<Vec<ChainlinkRound>> {
        let mut rounds = Vec::with_capacity(count);
        let mut current_id = from_round_id;

        for _ in 0..count {
            if current_id == 0 {
                break;
            }

            match self.fetch_round(current_id).await {
                Ok(round) => {
                    if !round.is_valid() {
                        // Round doesn't exist or is invalid
                        break;
                    }

                    // Add to cache
                    {
                        let mut cache = self.rounds_cache.write().await;
                        cache.insert(round.round_id, round.clone());
                    }

                    rounds.push(round);
                    current_id = current_id.saturating_sub(1);
                }
                Err(e) => {
                    warn!(round_id = current_id, error = %e, "Failed to fetch historical round");
                    break;
                }
            }

            // Rate limiting
            tokio::time::sleep(Duration::from_millis(50)).await;
        }

        Ok(rounds)
    }

    /// Get all cached rounds.
    pub async fn get_all_rounds(&self) -> Vec<ChainlinkRound> {
        self.rounds_cache.read().await.values().cloned().collect()
    }
}

// =============================================================================
// Replay Feed (for Backtesting)
// =============================================================================

/// Chainlink replay feed for offline backtesting.
/// Loads historical rounds and provides them in chronological order.
pub struct ChainlinkReplayFeed {
    /// All rounds sorted by (updated_at, round_id).
    rounds: Vec<ChainlinkRound>,
    /// Index for iteration.
    current_index: usize,
    /// Index by updated_at for fast lookup.
    by_timestamp: BTreeMap<u64, Vec<usize>>,
}

impl ChainlinkReplayFeed {
    /// Create a replay feed from stored rounds.
    pub fn new(mut rounds: Vec<ChainlinkRound>) -> Self {
        // Sort by (updated_at, round_id) for chronological replay
        rounds.sort_by(|a, b| {
            a.updated_at
                .cmp(&b.updated_at)
                .then(a.round_id.cmp(&b.round_id))
        });

        // Build timestamp index
        let mut by_timestamp: BTreeMap<u64, Vec<usize>> = BTreeMap::new();
        for (i, round) in rounds.iter().enumerate() {
            by_timestamp
                .entry(round.updated_at)
                .or_default()
                .push(i);
        }

        Self {
            rounds,
            current_index: 0,
            by_timestamp,
        }
    }

    /// Get total number of rounds.
    pub fn len(&self) -> usize {
        self.rounds.len()
    }

    /// Check if empty.
    pub fn is_empty(&self) -> bool {
        self.rounds.is_empty()
    }

    /// Get time range covered.
    pub fn time_range(&self) -> Option<(u64, u64)> {
        if self.rounds.is_empty() {
            None
        } else {
            Some((
                self.rounds.first().unwrap().updated_at,
                self.rounds.last().unwrap().updated_at,
            ))
        }
    }

    /// Reset to beginning for new replay.
    pub fn reset(&mut self) {
        self.current_index = 0;
    }

    /// Get next round in chronological order.
    pub fn next(&mut self) -> Option<&ChainlinkRound> {
        if self.current_index < self.rounds.len() {
            let round = &self.rounds[self.current_index];
            self.current_index += 1;
            Some(round)
        } else {
            None
        }
    }

    /// Peek at next round without advancing.
    pub fn peek(&self) -> Option<&ChainlinkRound> {
        self.rounds.get(self.current_index)
    }

    /// Get the last round at or before a timestamp.
    /// This is the key method for settlement reference lookup.
    pub fn round_at_or_before(&self, target_ts: u64) -> Option<&ChainlinkRound> {
        // Binary search for the last entry <= target_ts
        let mut best: Option<&ChainlinkRound> = None;

        for (ts, indices) in self.by_timestamp.range(..=target_ts).rev() {
            // Take the last round at this timestamp
            if let Some(&idx) = indices.last() {
                best = Some(&self.rounds[idx]);
                break;
            }
        }

        best
    }

    /// Get the first round after a timestamp.
    pub fn round_first_after(&self, target_ts: u64) -> Option<&ChainlinkRound> {
        for (ts, indices) in self.by_timestamp.range((target_ts + 1)..) {
            if let Some(&idx) = indices.first() {
                return Some(&self.rounds[idx]);
            }
        }
        None
    }

    /// Get round closest to a timestamp.
    pub fn round_closest(&self, target_ts: u64) -> Option<&ChainlinkRound> {
        let before = self.round_at_or_before(target_ts);
        let after = self.round_first_after(target_ts);

        match (before, after) {
            (Some(b), Some(a)) => {
                let diff_before = target_ts.saturating_sub(b.updated_at);
                let diff_after = a.updated_at.saturating_sub(target_ts);
                if diff_before <= diff_after {
                    Some(b)
                } else {
                    Some(a)
                }
            }
            (Some(b), None) => Some(b),
            (None, Some(a)) => Some(a),
            (None, None) => None,
        }
    }

    /// Get all rounds in a time range (inclusive).
    pub fn rounds_in_range(&self, start_ts: u64, end_ts: u64) -> Vec<&ChainlinkRound> {
        let mut result = Vec::new();
        for (_, indices) in self.by_timestamp.range(start_ts..=end_ts) {
            for &idx in indices {
                result.push(&self.rounds[idx]);
            }
        }
        result
    }
}

// =============================================================================
// Helper Functions
// =============================================================================

#[derive(Debug, Deserialize)]
struct JsonRpcResponse {
    result: Option<String>,
    error: Option<serde_json::Value>,
}

fn decode_u128(bytes: &[u8]) -> u128 {
    // Last 16 bytes of 32-byte slot
    if bytes.len() >= 32 {
        u128::from_be_bytes(bytes[16..32].try_into().unwrap_or([0; 16]))
    } else if bytes.len() >= 16 {
        u128::from_be_bytes(bytes[bytes.len() - 16..].try_into().unwrap_or([0; 16]))
    } else {
        0
    }
}

fn decode_i128(bytes: &[u8]) -> i128 {
    if bytes.len() >= 32 {
        i128::from_be_bytes(bytes[16..32].try_into().unwrap_or([0; 16]))
    } else if bytes.len() >= 16 {
        i128::from_be_bytes(bytes[bytes.len() - 16..].try_into().unwrap_or([0; 16]))
    } else {
        0
    }
}

fn decode_u64(bytes: &[u8]) -> u64 {
    if bytes.len() >= 32 {
        u64::from_be_bytes(bytes[24..32].try_into().unwrap_or([0; 8]))
    } else if bytes.len() >= 8 {
        u64::from_be_bytes(bytes[bytes.len() - 8..].try_into().unwrap_or([0; 8]))
    } else {
        0
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_chainlink_round_price() {
        let round = ChainlinkRound {
            feed_id: "test".to_string(),
            round_id: 1,
            answer: 5000000000000, // 50000.00000000
            updated_at: 1700000000,
            answered_in_round: 1,
            started_at: 1700000000,
            ingest_arrival_time_ns: 1700000000_000_000_000,
            ingest_seq: 0,
            decimals: 8,
            asset_symbol: "BTC".to_string(),
            raw_source_hash: None,
        };

        assert!((round.price() - 50000.0).abs() < 0.00001);
    }

    #[test]
    fn test_replay_feed_round_at_or_before() {
        let rounds = vec![
            ChainlinkRound {
                feed_id: "test".to_string(),
                round_id: 1,
                answer: 5000000000000,
                updated_at: 1000,
                answered_in_round: 1,
                started_at: 1000,
                ingest_arrival_time_ns: 1000_000_000_000,
                ingest_seq: 0,
                decimals: 8,
                asset_symbol: "BTC".to_string(),
                raw_source_hash: None,
            },
            ChainlinkRound {
                feed_id: "test".to_string(),
                round_id: 2,
                answer: 5010000000000,
                updated_at: 1100,
                answered_in_round: 2,
                started_at: 1100,
                ingest_arrival_time_ns: 1100_000_000_000,
                ingest_seq: 1,
                decimals: 8,
                asset_symbol: "BTC".to_string(),
                raw_source_hash: None,
            },
            ChainlinkRound {
                feed_id: "test".to_string(),
                round_id: 3,
                answer: 5020000000000,
                updated_at: 1200,
                answered_in_round: 3,
                started_at: 1200,
                ingest_arrival_time_ns: 1200_000_000_000,
                ingest_seq: 2,
                decimals: 8,
                asset_symbol: "BTC".to_string(),
                raw_source_hash: None,
            },
        ];

        let feed = ChainlinkReplayFeed::new(rounds);

        // Before first round
        assert!(feed.round_at_or_before(999).is_none());

        // Exactly at first round
        let r = feed.round_at_or_before(1000).unwrap();
        assert_eq!(r.round_id, 1);

        // Between rounds
        let r = feed.round_at_or_before(1050).unwrap();
        assert_eq!(r.round_id, 1);

        // At second round
        let r = feed.round_at_or_before(1100).unwrap();
        assert_eq!(r.round_id, 2);

        // After all rounds
        let r = feed.round_at_or_before(2000).unwrap();
        assert_eq!(r.round_id, 3);
    }

    #[test]
    fn test_replay_feed_round_first_after() {
        let rounds = vec![
            ChainlinkRound {
                feed_id: "test".to_string(),
                round_id: 1,
                answer: 5000000000000,
                updated_at: 1000,
                answered_in_round: 1,
                started_at: 1000,
                ingest_arrival_time_ns: 1000_000_000_000,
                ingest_seq: 0,
                decimals: 8,
                asset_symbol: "BTC".to_string(),
                raw_source_hash: None,
            },
            ChainlinkRound {
                feed_id: "test".to_string(),
                round_id: 2,
                answer: 5010000000000,
                updated_at: 1100,
                answered_in_round: 2,
                started_at: 1100,
                ingest_arrival_time_ns: 1100_000_000_000,
                ingest_seq: 1,
                decimals: 8,
                asset_symbol: "BTC".to_string(),
                raw_source_hash: None,
            },
        ];

        let feed = ChainlinkReplayFeed::new(rounds);

        // Before first round
        let r = feed.round_first_after(500).unwrap();
        assert_eq!(r.round_id, 1);

        // After first, before second
        let r = feed.round_first_after(1000).unwrap();
        assert_eq!(r.round_id, 2);

        // After all rounds
        assert!(feed.round_first_after(1100).is_none());
    }

    #[test]
    fn test_feed_config_feed_id() {
        let config1 = ChainlinkFeedConfig::btc_usd_polygon();
        let config2 = ChainlinkFeedConfig::eth_usd_polygon();

        // Different assets should have different feed IDs
        assert_ne!(config1.feed_id(), config2.feed_id());

        // Same config should produce same ID
        let config3 = ChainlinkFeedConfig::btc_usd_polygon();
        assert_eq!(config1.feed_id(), config3.feed_id());
    }
}

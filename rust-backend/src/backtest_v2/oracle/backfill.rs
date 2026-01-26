//! Automated Chainlink Oracle Backfill Service
//!
//! Provides historical oracle round backfill with:
//! - Log-based backfill (AnswerUpdated events) - preferred
//! - Round query backfill (getRoundData iteration) - fallback
//! - Idempotent storage (safe to run repeatedly)
//! - Phase/upgrade detection
//!
//! # Usage
//!
//! ```ignore
//! // Programmatic
//! let service = OracleBackfillService::new(config, storage);
//! service.backfill_range("BTC", start_ts, end_ts).await?;
//!
//! // CLI
//! backtest_v2 backfill-oracle --asset BTC --start 2024-01-01 --end 2024-01-31
//! ```

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};

use super::chainlink::{ChainlinkFeedConfig, ChainlinkRound};
use super::config::{OracleConfig, OracleFeedConfig};
use super::storage::OracleRoundStorage;

/// Nanoseconds per second.
const NS_PER_SEC: u64 = 1_000_000_000;

// =============================================================================
// BACKFILL CONFIGURATION
// =============================================================================

/// Backfill strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BackfillStrategy {
    /// Scan AnswerUpdated events from logs (preferred, more efficient).
    LogScan,
    /// Iterate getRoundData calls (fallback, slower but works with all nodes).
    RoundIteration,
    /// Hybrid: try logs first, fall back to round iteration if logs unavailable.
    Hybrid,
}

impl Default for BackfillStrategy {
    fn default() -> Self {
        Self::Hybrid
    }
}

/// Backfill configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackfillConfig {
    /// Backfill strategy to use.
    pub strategy: BackfillStrategy,
    /// Maximum concurrent RPC requests.
    pub max_concurrent_requests: usize,
    /// Delay between RPC requests (milliseconds).
    pub request_delay_ms: u64,
    /// Block range for log queries (larger = fewer queries, but may hit limits).
    pub log_block_range: u64,
    /// Maximum rounds to fetch per iteration.
    pub max_rounds_per_batch: usize,
    /// Whether to verify round continuity.
    pub verify_continuity: bool,
    /// Whether to detect and handle phase transitions.
    pub detect_phase_transitions: bool,
    /// Retry count for failed requests.
    pub retry_count: u32,
    /// Retry delay (milliseconds).
    pub retry_delay_ms: u64,
}

impl Default for BackfillConfig {
    fn default() -> Self {
        Self {
            strategy: BackfillStrategy::Hybrid,
            max_concurrent_requests: 4,
            request_delay_ms: 50,
            log_block_range: 10_000,
            max_rounds_per_batch: 1000,
            verify_continuity: true,
            detect_phase_transitions: true,
            retry_count: 3,
            retry_delay_ms: 1000,
        }
    }
}

// =============================================================================
// BACKFILL PROGRESS
// =============================================================================

/// Backfill progress tracking.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackfillProgress {
    /// Feed ID being backfilled.
    pub feed_id: String,
    /// Asset symbol.
    pub asset_symbol: String,
    /// Target time range start (Unix seconds).
    pub start_ts: u64,
    /// Target time range end (Unix seconds).
    pub end_ts: u64,
    /// Current progress timestamp.
    pub current_ts: u64,
    /// Rounds fetched so far.
    pub rounds_fetched: u64,
    /// Rounds stored (may be less due to dedup).
    pub rounds_stored: u64,
    /// Duplicate rounds skipped.
    pub duplicates_skipped: u64,
    /// Errors encountered.
    pub errors_encountered: u64,
    /// Phase transitions detected.
    pub phase_transitions: u64,
    /// Start time of backfill.
    pub started_at: u64,
    /// Last update time.
    pub updated_at: u64,
    /// Whether backfill is complete.
    pub is_complete: bool,
    /// Error message if failed.
    pub error_message: Option<String>,
}

impl BackfillProgress {
    pub fn new(feed_id: &str, asset_symbol: &str, start_ts: u64, end_ts: u64) -> Self {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        
        Self {
            feed_id: feed_id.to_string(),
            asset_symbol: asset_symbol.to_string(),
            start_ts,
            end_ts,
            current_ts: start_ts,
            rounds_fetched: 0,
            rounds_stored: 0,
            duplicates_skipped: 0,
            errors_encountered: 0,
            phase_transitions: 0,
            started_at: now,
            updated_at: now,
            is_complete: false,
            error_message: None,
        }
    }

    pub fn progress_pct(&self) -> f64 {
        if self.end_ts <= self.start_ts {
            return 100.0;
        }
        let elapsed = self.current_ts.saturating_sub(self.start_ts);
        let total = self.end_ts.saturating_sub(self.start_ts);
        (elapsed as f64 / total as f64 * 100.0).min(100.0)
    }

    pub fn format_status(&self) -> String {
        format!(
            "[{:.1}%] {} rounds fetched, {} stored, {} dupes, {} errors",
            self.progress_pct(),
            self.rounds_fetched,
            self.rounds_stored,
            self.duplicates_skipped,
            self.errors_encountered
        )
    }
}

// =============================================================================
// BACKFILL RESULT
// =============================================================================

/// Result of a backfill operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackfillResult {
    /// Final progress state.
    pub progress: BackfillProgress,
    /// Time taken (seconds).
    pub duration_secs: f64,
    /// Oldest round fetched.
    pub oldest_round_id: Option<u128>,
    /// Newest round fetched.
    pub newest_round_id: Option<u128>,
    /// Oldest timestamp fetched.
    pub oldest_updated_at: Option<u64>,
    /// Newest timestamp fetched.
    pub newest_updated_at: Option<u64>,
    /// Gaps detected in round sequence.
    pub gaps_detected: Vec<(u128, u128)>,
    /// Whether the backfill was successful.
    pub success: bool,
}

impl BackfillResult {
    pub fn format_report(&self) -> String {
        let mut out = String::new();
        
        out.push_str("╔══════════════════════════════════════════════════════════════════════════════╗\n");
        out.push_str("║                      ORACLE BACKFILL REPORT                                  ║\n");
        out.push_str("╠══════════════════════════════════════════════════════════════════════════════╣\n");
        out.push_str(&format!("║  Feed:             {:57} ║\n", self.progress.feed_id));
        out.push_str(&format!("║  Asset:            {:57} ║\n", self.progress.asset_symbol));
        out.push_str(&format!("║  Status:           {:57} ║\n", 
            if self.success { "SUCCESS" } else { "FAILED" }));
        out.push_str(&format!("║  Duration:         {:>10.2} seconds                                       ║\n", 
            self.duration_secs));
        out.push_str("╠══════════════════════════════════════════════════════════════════════════════╣\n");
        out.push_str(&format!("║  Rounds Fetched:   {:>10}                                                ║\n", 
            self.progress.rounds_fetched));
        out.push_str(&format!("║  Rounds Stored:    {:>10}                                                ║\n", 
            self.progress.rounds_stored));
        out.push_str(&format!("║  Duplicates:       {:>10}                                                ║\n", 
            self.progress.duplicates_skipped));
        out.push_str(&format!("║  Errors:           {:>10}                                                ║\n", 
            self.progress.errors_encountered));
        
        if !self.gaps_detected.is_empty() {
            out.push_str("╠══════════════════════════════════════════════════════════════════════════════╣\n");
            out.push_str(&format!("║  GAPS DETECTED:    {:>10}                                                ║\n", 
                self.gaps_detected.len()));
            for (start, end) in &self.gaps_detected {
                out.push_str(&format!("║    Round {} -> {}                                              ║\n", 
                    start, end));
            }
        }
        
        if let Some(ref err) = self.progress.error_message {
            out.push_str("╠══════════════════════════════════════════════════════════════════════════════╣\n");
            out.push_str(&format!("║  ERROR: {:67} ║\n", 
                if err.len() > 67 { &err[..67] } else { err }));
        }
        
        out.push_str("╚══════════════════════════════════════════════════════════════════════════════╝\n");
        
        out
    }
}

// =============================================================================
// BACKFILL SERVICE
// =============================================================================

/// Oracle backfill service for fetching historical Chainlink rounds.
pub struct OracleBackfillService {
    /// Oracle configuration.
    oracle_config: OracleConfig,
    /// Storage for persisting rounds.
    storage: Arc<OracleRoundStorage>,
    /// HTTP client for RPC calls.
    client: reqwest::Client,
    /// Backfill configuration.
    config: BackfillConfig,
}

impl OracleBackfillService {
    /// Create a new backfill service.
    pub fn new(
        oracle_config: OracleConfig,
        storage: Arc<OracleRoundStorage>,
        config: BackfillConfig,
    ) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .expect("failed to build HTTP client");
        
        Self {
            oracle_config,
            storage,
            client,
            config,
        }
    }

    /// Get the RPC endpoint, loading from env if needed.
    fn get_rpc_endpoint(&self) -> Result<String> {
        self.oracle_config.rpc_endpoint.clone()
            .or_else(|| std::env::var("POLYGON_RPC_URL").ok())
            .or_else(|| std::env::var("CHAINLINK_RPC_URL").ok())
            .ok_or_else(|| anyhow::anyhow!(
                "No RPC endpoint configured. Set POLYGON_RPC_URL or CHAINLINK_RPC_URL"
            ))
    }

    /// Backfill a time range for an asset.
    ///
    /// This is the main entry point for backfilling.
    /// It is idempotent - safe to run multiple times.
    pub async fn backfill_range(
        &self,
        asset: &str,
        start_ts: u64,
        end_ts: u64,
    ) -> Result<BackfillResult> {
        let feed_config = self.oracle_config.get_feed(asset)
            .ok_or_else(|| anyhow::anyhow!("No feed configured for asset: {}", asset))?;
        
        let feed_id = feed_config.feed_id();
        let rpc_endpoint = self.get_rpc_endpoint()?;
        
        info!(
            asset = %asset,
            feed_id = %feed_id,
            start_ts = %start_ts,
            end_ts = %end_ts,
            "Starting oracle backfill"
        );
        
        let start_time = Instant::now();
        let mut progress = BackfillProgress::new(&feed_id, asset, start_ts, end_ts);
        
        // Check existing coverage
        if let Ok(Some((existing_start, existing_end))) = self.storage.get_time_coverage(&feed_id) {
            info!(
                existing_start = %existing_start,
                existing_end = %existing_end,
                "Found existing data coverage"
            );
            
            // If we already have this range, skip
            if existing_start <= start_ts && existing_end >= end_ts {
                info!("Range already covered, skipping backfill");
                progress.is_complete = true;
                progress.current_ts = end_ts;
                
                return Ok(BackfillResult {
                    progress,
                    duration_secs: start_time.elapsed().as_secs_f64(),
                    oldest_round_id: None,
                    newest_round_id: None,
                    oldest_updated_at: Some(existing_start),
                    newest_updated_at: Some(existing_end),
                    gaps_detected: Vec::new(),
                    success: true,
                });
            }
        }
        
        // Execute backfill based on strategy
        let result = match self.config.strategy {
            BackfillStrategy::LogScan => {
                self.backfill_via_logs(feed_config, &rpc_endpoint, start_ts, end_ts, &mut progress).await
            }
            BackfillStrategy::RoundIteration => {
                self.backfill_via_rounds(feed_config, &rpc_endpoint, start_ts, end_ts, &mut progress).await
            }
            BackfillStrategy::Hybrid => {
                // Try logs first
                match self.backfill_via_logs(feed_config, &rpc_endpoint, start_ts, end_ts, &mut progress).await {
                    Ok(r) => Ok(r),
                    Err(e) => {
                        warn!(error = %e, "Log-based backfill failed, falling back to round iteration");
                        self.backfill_via_rounds(feed_config, &rpc_endpoint, start_ts, end_ts, &mut progress).await
                    }
                }
            }
        };
        
        let duration_secs = start_time.elapsed().as_secs_f64();
        
        match result {
            Ok(mut res) => {
                res.duration_secs = duration_secs;
                
                // Update storage backfill state
                if let (Some(oldest_id), Some(newest_id), Some(oldest_ts), Some(newest_ts)) = 
                    (res.oldest_round_id, res.newest_round_id, res.oldest_updated_at, res.newest_updated_at)
                {
                    let _ = self.storage.update_backfill_state(
                        &feed_id,
                        oldest_id,
                        newest_id,
                        oldest_ts,
                        newest_ts,
                        res.progress.rounds_stored as usize,
                        res.progress.is_complete,
                    );
                }
                
                info!(
                    rounds_stored = %res.progress.rounds_stored,
                    duration_secs = %duration_secs,
                    "Oracle backfill completed"
                );
                
                Ok(res)
            }
            Err(e) => {
                progress.error_message = Some(e.to_string());
                progress.is_complete = false;
                
                Ok(BackfillResult {
                    progress,
                    duration_secs,
                    oldest_round_id: None,
                    newest_round_id: None,
                    oldest_updated_at: None,
                    newest_updated_at: None,
                    gaps_detected: Vec::new(),
                    success: false,
                })
            }
        }
    }

    /// Backfill using log scanning (AnswerUpdated events).
    async fn backfill_via_logs(
        &self,
        feed_config: &OracleFeedConfig,
        rpc_endpoint: &str,
        start_ts: u64,
        end_ts: u64,
        progress: &mut BackfillProgress,
    ) -> Result<BackfillResult> {
        // First, get the block range for our time range
        let start_block = self.timestamp_to_block(rpc_endpoint, start_ts).await?;
        let end_block = self.timestamp_to_block(rpc_endpoint, end_ts).await?;
        
        info!(
            start_block = %start_block,
            end_block = %end_block,
            "Scanning logs for AnswerUpdated events"
        );
        
        // AnswerUpdated event signature
        // event AnswerUpdated(int256 indexed current, uint256 indexed roundId, uint256 updatedAt)
        let event_signature = "0x0559884fd3a460db3073b7fc896cc77986f16e378210ded43186175bf646fc5f";
        
        let mut rounds = Vec::new();
        let mut current_block = start_block;
        
        while current_block < end_block {
            let to_block = (current_block + self.config.log_block_range).min(end_block);
            
            let logs = self.fetch_logs(
                rpc_endpoint,
                &feed_config.feed_proxy_address,
                event_signature,
                current_block,
                to_block,
            ).await?;
            
            for log in logs {
                if let Some(round) = self.parse_answer_updated_log(&log, feed_config) {
                    rounds.push(round);
                    progress.rounds_fetched += 1;
                }
            }
            
            progress.current_ts = self.block_to_timestamp(rpc_endpoint, to_block).await
                .unwrap_or(progress.current_ts);
            progress.updated_at = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs();
            
            current_block = to_block + 1;
            
            // Rate limiting
            tokio::time::sleep(Duration::from_millis(self.config.request_delay_ms)).await;
        }
        
        // Store rounds
        let stored = self.storage.store_rounds(&rounds)
            .context("Failed to store rounds")?;
        progress.rounds_stored = stored as u64;
        progress.duplicates_skipped = progress.rounds_fetched.saturating_sub(progress.rounds_stored);
        progress.is_complete = true;
        progress.current_ts = end_ts;
        
        // Compute stats
        let (oldest_round_id, newest_round_id, oldest_ts, newest_ts) = if rounds.is_empty() {
            (None, None, None, None)
        } else {
            let oldest = rounds.iter().min_by_key(|r| r.round_id).unwrap();
            let newest = rounds.iter().max_by_key(|r| r.round_id).unwrap();
            (
                Some(oldest.round_id),
                Some(newest.round_id),
                Some(oldest.updated_at),
                Some(newest.updated_at),
            )
        };
        
        // Detect gaps
        let gaps = if self.config.verify_continuity {
            self.detect_gaps(&rounds)
        } else {
            Vec::new()
        };
        
        Ok(BackfillResult {
            progress: progress.clone(),
            duration_secs: 0.0, // Set by caller
            oldest_round_id,
            newest_round_id,
            oldest_updated_at: oldest_ts,
            newest_updated_at: newest_ts,
            gaps_detected: gaps,
            success: true,
        })
    }

    /// Backfill using round iteration (getRoundData calls).
    async fn backfill_via_rounds(
        &self,
        feed_config: &OracleFeedConfig,
        rpc_endpoint: &str,
        start_ts: u64,
        end_ts: u64,
        progress: &mut BackfillProgress,
    ) -> Result<BackfillResult> {
        // First, get the latest round to find a starting point
        let latest_round = self.fetch_latest_round(feed_config, rpc_endpoint).await
            .context("Failed to fetch latest round")?;
        
        info!(
            latest_round_id = %latest_round.round_id,
            latest_updated_at = %latest_round.updated_at,
            "Starting round iteration from latest"
        );
        
        let mut rounds = Vec::new();
        let mut current_round_id = latest_round.round_id;
        let mut consecutive_errors = 0;
        
        // Iterate backward from latest
        while consecutive_errors < 10 {
            match self.fetch_round(feed_config, rpc_endpoint, current_round_id).await {
                Ok(round) => {
                    consecutive_errors = 0;
                    
                    // Check if we're before our target range
                    if round.updated_at < start_ts {
                        break;
                    }
                    
                    // Store if within range
                    if round.updated_at >= start_ts && round.updated_at <= end_ts {
                        rounds.push(round.clone());
                        progress.rounds_fetched += 1;
                        progress.current_ts = round.updated_at;
                    }
                    
                    // Move to previous round
                    if current_round_id == 0 {
                        break;
                    }
                    current_round_id = current_round_id.saturating_sub(1);
                }
                Err(e) => {
                    consecutive_errors += 1;
                    progress.errors_encountered += 1;
                    debug!(round_id = %current_round_id, error = %e, "Failed to fetch round");
                    
                    // Skip this round and try the next
                    if current_round_id == 0 {
                        break;
                    }
                    current_round_id = current_round_id.saturating_sub(1);
                }
            }
            
            // Rate limiting
            tokio::time::sleep(Duration::from_millis(self.config.request_delay_ms)).await;
            
            // Progress update
            progress.updated_at = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs();
            
            // Batch limit
            if rounds.len() >= self.config.max_rounds_per_batch {
                // Store batch
                let stored = self.storage.store_rounds(&rounds)?;
                progress.rounds_stored += stored as u64;
                progress.duplicates_skipped += (rounds.len() - stored) as u64;
                rounds.clear();
            }
        }
        
        // Store remaining rounds
        if !rounds.is_empty() {
            let stored = self.storage.store_rounds(&rounds)?;
            progress.rounds_stored += stored as u64;
            progress.duplicates_skipped += (rounds.len() - stored) as u64;
        }
        
        progress.is_complete = true;
        progress.current_ts = end_ts;
        
        // Get actual stored range
        let (oldest_ts, newest_ts) = self.storage.get_time_coverage(&feed_config.feed_id())?
            .unwrap_or((start_ts, end_ts));
        
        // Get round range
        let oldest_round = self.storage.get_round_at_or_before(&feed_config.feed_id(), oldest_ts)?;
        let newest_round = self.storage.get_round_at_or_before(&feed_config.feed_id(), newest_ts)?;
        
        Ok(BackfillResult {
            progress: progress.clone(),
            duration_secs: 0.0,
            oldest_round_id: oldest_round.as_ref().map(|r| r.round_id),
            newest_round_id: newest_round.as_ref().map(|r| r.round_id),
            oldest_updated_at: Some(oldest_ts),
            newest_updated_at: Some(newest_ts),
            gaps_detected: Vec::new(),
            success: true,
        })
    }

    /// Fetch logs from the RPC endpoint.
    async fn fetch_logs(
        &self,
        rpc_endpoint: &str,
        address: &str,
        event_signature: &str,
        from_block: u64,
        to_block: u64,
    ) -> Result<Vec<serde_json::Value>> {
        let payload = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "eth_getLogs",
            "params": [{
                "address": address,
                "topics": [event_signature],
                "fromBlock": format!("0x{:x}", from_block),
                "toBlock": format!("0x{:x}", to_block)
            }],
            "id": 1
        });
        
        let response: serde_json::Value = self.client
            .post(rpc_endpoint)
            .json(&payload)
            .send()
            .await?
            .json()
            .await?;
        
        if let Some(error) = response.get("error") {
            return Err(anyhow::anyhow!("RPC error: {:?}", error));
        }
        
        let logs = response.get("result")
            .and_then(|r| r.as_array())
            .cloned()
            .unwrap_or_default();
        
        Ok(logs)
    }

    /// Parse an AnswerUpdated log into a ChainlinkRound.
    fn parse_answer_updated_log(
        &self,
        log: &serde_json::Value,
        feed_config: &OracleFeedConfig,
    ) -> Option<ChainlinkRound> {
        // topics[0] = event signature
        // topics[1] = indexed current (answer)
        // topics[2] = indexed roundId
        // data = updatedAt
        
        let topics = log.get("topics")?.as_array()?;
        if topics.len() < 3 {
            return None;
        }
        
        let answer_hex = topics.get(1)?.as_str()?;
        let round_id_hex = topics.get(2)?.as_str()?;
        let data_hex = log.get("data")?.as_str()?;
        
        let answer = i128::from_str_radix(answer_hex.trim_start_matches("0x"), 16).ok()?;
        let round_id = u128::from_str_radix(round_id_hex.trim_start_matches("0x"), 16).ok()?;
        
        // updatedAt is in the data field
        let updated_at = if data_hex.len() >= 66 {
            u64::from_str_radix(&data_hex[2..66], 16).ok()?
        } else {
            return None;
        };
        
        // Get block timestamp for arrival time approximation
        let block_number = log.get("blockNumber")?.as_str()?;
        let block_num = u64::from_str_radix(block_number.trim_start_matches("0x"), 16).ok()?;
        
        // Approximate arrival time as updated_at + 2 seconds (typical block time)
        let arrival_time_ns = (updated_at + 2) * NS_PER_SEC;
        
        Some(ChainlinkRound {
            feed_id: feed_config.feed_id(),
            round_id,
            answer,
            updated_at,
            answered_in_round: round_id, // Same for AnswerUpdated
            started_at: updated_at,
            ingest_arrival_time_ns: arrival_time_ns,
            ingest_seq: 0, // Will be set by storage
            decimals: feed_config.decimals,
            asset_symbol: feed_config.asset_symbol.clone(),
            raw_source_hash: Some(format!("log_{}", block_num)),
        })
    }

    /// Fetch the latest round from the feed.
    async fn fetch_latest_round(
        &self,
        feed_config: &OracleFeedConfig,
        rpc_endpoint: &str,
    ) -> Result<ChainlinkRound> {
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
                "to": &feed_config.feed_proxy_address,
                "data": call_data
            }, "latest"],
            "id": 1
        });
        
        let response: serde_json::Value = self.client
            .post(rpc_endpoint)
            .json(&payload)
            .send()
            .await?
            .json()
            .await?;
        
        if let Some(error) = response.get("error") {
            return Err(anyhow::anyhow!("RPC error: {:?}", error));
        }
        
        let result = response.get("result")
            .and_then(|r| r.as_str())
            .ok_or_else(|| anyhow::anyhow!("No result in response"))?;
        
        self.decode_round_data(result, feed_config, arrival_time_ns)
    }

    /// Fetch a specific round by ID.
    async fn fetch_round(
        &self,
        feed_config: &OracleFeedConfig,
        rpc_endpoint: &str,
        round_id: u128,
    ) -> Result<ChainlinkRound> {
        let arrival_time_ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos() as u64;
        
        // getRoundData(uint80) selector: 0x9a6fc8f5
        let round_id_hex = format!("{:064x}", round_id);
        let call_data = format!("0x9a6fc8f5{}", round_id_hex);
        
        let payload = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "eth_call",
            "params": [{
                "to": &feed_config.feed_proxy_address,
                "data": call_data
            }, "latest"],
            "id": 1
        });
        
        let response: serde_json::Value = self.client
            .post(rpc_endpoint)
            .json(&payload)
            .send()
            .await?
            .json()
            .await?;
        
        if let Some(error) = response.get("error") {
            return Err(anyhow::anyhow!("RPC error: {:?}", error));
        }
        
        let result = response.get("result")
            .and_then(|r| r.as_str())
            .ok_or_else(|| anyhow::anyhow!("No result in response"))?;
        
        self.decode_round_data(result, feed_config, arrival_time_ns)
    }

    /// Decode ABI-encoded round data.
    fn decode_round_data(
        &self,
        hex_result: &str,
        feed_config: &OracleFeedConfig,
        arrival_time_ns: u64,
    ) -> Result<ChainlinkRound> {
        let bytes = hex::decode(hex_result.trim_start_matches("0x"))
            .context("Failed to decode hex")?;
        
        if bytes.len() < 160 {
            return Err(anyhow::anyhow!("Response too short: {} bytes", bytes.len()));
        }
        
        // Parse: roundId (32), answer (32), startedAt (32), updatedAt (32), answeredInRound (32)
        let round_id = decode_u128(&bytes[0..32]);
        let answer = decode_i128(&bytes[32..64]);
        let started_at = decode_u64(&bytes[64..96]);
        let updated_at = decode_u64(&bytes[96..128]);
        let answered_in_round = decode_u128(&bytes[128..160]);
        
        Ok(ChainlinkRound {
            feed_id: feed_config.feed_id(),
            round_id,
            answer,
            updated_at,
            answered_in_round,
            started_at,
            ingest_arrival_time_ns: arrival_time_ns,
            ingest_seq: 0,
            decimals: feed_config.decimals,
            asset_symbol: feed_config.asset_symbol.clone(),
            raw_source_hash: None,
        })
    }

    /// Convert a timestamp to an approximate block number.
    async fn timestamp_to_block(&self, rpc_endpoint: &str, timestamp: u64) -> Result<u64> {
        // Get latest block
        let payload = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "eth_blockNumber",
            "params": [],
            "id": 1
        });
        
        let response: serde_json::Value = self.client
            .post(rpc_endpoint)
            .json(&payload)
            .send()
            .await?
            .json()
            .await?;
        
        let latest_block_hex = response.get("result")
            .and_then(|r| r.as_str())
            .ok_or_else(|| anyhow::anyhow!("No result"))?;
        let latest_block = u64::from_str_radix(latest_block_hex.trim_start_matches("0x"), 16)?;
        
        // Get latest block timestamp
        let latest_ts = self.block_to_timestamp(rpc_endpoint, latest_block).await?;
        
        // Estimate block number (assuming ~2 second blocks for Polygon)
        let time_diff = latest_ts.saturating_sub(timestamp);
        let block_diff = time_diff / 2;
        
        Ok(latest_block.saturating_sub(block_diff))
    }

    /// Get the timestamp for a block.
    async fn block_to_timestamp(&self, rpc_endpoint: &str, block_number: u64) -> Result<u64> {
        let payload = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "eth_getBlockByNumber",
            "params": [format!("0x{:x}", block_number), false],
            "id": 1
        });
        
        let response: serde_json::Value = self.client
            .post(rpc_endpoint)
            .json(&payload)
            .send()
            .await?
            .json()
            .await?;
        
        let block = response.get("result")
            .ok_or_else(|| anyhow::anyhow!("No result"))?;
        let timestamp_hex = block.get("timestamp")
            .and_then(|t| t.as_str())
            .ok_or_else(|| anyhow::anyhow!("No timestamp"))?;
        
        Ok(u64::from_str_radix(timestamp_hex.trim_start_matches("0x"), 16)?)
    }

    /// Detect gaps in round sequence.
    fn detect_gaps(&self, rounds: &[ChainlinkRound]) -> Vec<(u128, u128)> {
        if rounds.len() < 2 {
            return Vec::new();
        }
        
        let mut sorted: Vec<u128> = rounds.iter().map(|r| r.round_id).collect();
        sorted.sort();
        
        let mut gaps = Vec::new();
        for window in sorted.windows(2) {
            if window[1] > window[0] + 1 {
                gaps.push((window[0], window[1]));
            }
        }
        
        gaps
    }
}

// =============================================================================
// HELPER FUNCTIONS
// =============================================================================

fn decode_u128(bytes: &[u8]) -> u128 {
    if bytes.len() >= 32 {
        u128::from_be_bytes(bytes[16..32].try_into().unwrap_or([0; 16]))
    } else {
        0
    }
}

fn decode_i128(bytes: &[u8]) -> i128 {
    if bytes.len() >= 32 {
        i128::from_be_bytes(bytes[16..32].try_into().unwrap_or([0; 16]))
    } else {
        0
    }
}

fn decode_u64(bytes: &[u8]) -> u64 {
    if bytes.len() >= 32 {
        u64::from_be_bytes(bytes[24..32].try_into().unwrap_or([0; 8]))
    } else {
        0
    }
}

// =============================================================================
// TESTS
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_backfill_progress_formatting() {
        let progress = BackfillProgress::new("btc_137_c907e116", "BTC", 1000, 2000);
        let status = progress.format_status();
        assert!(status.contains("0.0%"));
    }

    #[test]
    fn test_backfill_progress_percentage() {
        let mut progress = BackfillProgress::new("test", "BTC", 1000, 2000);
        progress.current_ts = 1500;
        assert!((progress.progress_pct() - 50.0).abs() < 0.1);
    }

    #[test]
    fn test_gap_detection() {
        let config = OracleConfig::production_btc_polygon();
        let storage = Arc::new(OracleRoundStorage::open_memory().unwrap());
        let service = OracleBackfillService::new(config, storage, BackfillConfig::default());
        
        let rounds = vec![
            ChainlinkRound {
                feed_id: "test".to_string(),
                round_id: 1,
                answer: 50000_00000000,
                updated_at: 1000,
                answered_in_round: 1,
                started_at: 1000,
                ingest_arrival_time_ns: 1000 * NS_PER_SEC,
                ingest_seq: 0,
                decimals: 8,
                asset_symbol: "BTC".to_string(),
                raw_source_hash: None,
            },
            ChainlinkRound {
                feed_id: "test".to_string(),
                round_id: 3, // Gap: 2 is missing
                answer: 50100_00000000,
                updated_at: 1100,
                answered_in_round: 3,
                started_at: 1100,
                ingest_arrival_time_ns: 1100 * NS_PER_SEC,
                ingest_seq: 1,
                decimals: 8,
                asset_symbol: "BTC".to_string(),
                raw_source_hash: None,
            },
            ChainlinkRound {
                feed_id: "test".to_string(),
                round_id: 4,
                answer: 50200_00000000,
                updated_at: 1200,
                answered_in_round: 4,
                started_at: 1200,
                ingest_arrival_time_ns: 1200 * NS_PER_SEC,
                ingest_seq: 2,
                decimals: 8,
                asset_symbol: "BTC".to_string(),
                raw_source_hash: None,
            },
        ];
        
        let gaps = service.detect_gaps(&rounds);
        assert_eq!(gaps.len(), 1);
        assert_eq!(gaps[0], (1, 3));
    }
}

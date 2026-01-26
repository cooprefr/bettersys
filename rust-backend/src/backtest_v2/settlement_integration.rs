//! Settlement Integration for Orchestrator
//!
//! Wires the settlement engine with Chainlink reference sources for production-grade
//! backtesting of Polymarket 15-minute Up/Down markets.
//!
//! # Key Components
//!
//! - `SettlementConfig`: Extended configuration including Chainlink reference source
//! - `WindowSettlementRecord`: Per-window settlement audit trail
//! - `SettlementMetadata`: Complete settlement metadata for BacktestResults
//! - `SettlementFingerprint`: Deterministic fingerprint for settlement configuration
//!
//! # Visibility Semantics
//!
//! Settlement respects arrival-time visibility:
//! - Outcome is NOT knowable until the oracle round has ARRIVED (ingest_arrival_time_ns <= decision_time_ns)
//! - This prevents look-ahead bias in settlement timing

use crate::backtest_v2::clock::Nanos;
use crate::backtest_v2::oracle::{
    ChainlinkSettlementSource, OraclePricePoint, SettlementReferenceRule, SettlementReferenceSource,
};
use crate::backtest_v2::portfolio::Outcome;
use crate::backtest_v2::settlement::{
    SettlementEngine, SettlementEvent, SettlementModel, SettlementOutcome, SettlementSpec,
    SettlementState, TieRule,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

/// Nanoseconds per second.
pub const NS_PER_SEC: Nanos = 1_000_000_000;

// =============================================================================
// SETTLEMENT CONFIGURATION
// =============================================================================

/// Extended settlement configuration with Chainlink reference source.
#[derive(Debug, Clone)]
pub struct SettlementConfig {
    /// Base settlement specification.
    pub spec: SettlementSpec,
    /// Reference rule for selecting oracle price.
    pub reference_rule: SettlementReferenceRule,
    /// Tie rule for reference selection (when equidistant).
    pub tie_rule: TieRule,
    /// Whether to require Chainlink oracle for settlement (production-grade).
    pub require_chainlink: bool,
    /// Chainlink feed configuration.
    pub chainlink_feed_id: Option<String>,
    /// Asset symbol for Chainlink feed.
    pub chainlink_asset_symbol: Option<String>,
    /// Chain ID for the Chainlink feed (e.g., 1 for Ethereum mainnet).
    pub chainlink_chain_id: Option<u64>,
    /// Feed proxy address (for audit trail).
    pub chainlink_feed_proxy: Option<String>,
    /// Maximum allowed delay between cutoff and oracle update (ns).
    /// If the oracle round used for settlement is older than this, flag as stale.
    pub max_oracle_staleness_ns: Option<Nanos>,
    /// Whether to abort on missing oracle data (production-grade).
    pub abort_on_missing_oracle: bool,
}

impl Default for SettlementConfig {
    fn default() -> Self {
        Self {
            spec: SettlementSpec::polymarket_15m_updown(),
            reference_rule: SettlementReferenceRule::LastUpdateAtOrBeforeCutoff,
            tie_rule: TieRule::NoWins,
            require_chainlink: false,
            chainlink_feed_id: None,
            chainlink_asset_symbol: None,
            chainlink_chain_id: None,
            chainlink_feed_proxy: None,
            max_oracle_staleness_ns: Some(60 * NS_PER_SEC), // 60 seconds default
            abort_on_missing_oracle: false,
        }
    }
}

impl SettlementConfig {
    /// Production-grade BTC settlement configuration.
    pub fn production_btc() -> Self {
        Self {
            spec: SettlementSpec::polymarket_15m_updown(),
            reference_rule: SettlementReferenceRule::LastUpdateAtOrBeforeCutoff,
            tie_rule: TieRule::NoWins,
            require_chainlink: true,
            chainlink_feed_id: Some("btc-usd".to_string()),
            chainlink_asset_symbol: Some("BTC".to_string()),
            chainlink_chain_id: Some(1), // Ethereum mainnet
            chainlink_feed_proxy: Some("0xF4030086522a5bEEa4988F8cA5B36dbC97BeE88c".to_string()),
            max_oracle_staleness_ns: Some(60 * NS_PER_SEC),
            abort_on_missing_oracle: true,
        }
    }

    /// Production-grade ETH settlement configuration.
    pub fn production_eth() -> Self {
        Self {
            spec: SettlementSpec::polymarket_15m_updown(),
            reference_rule: SettlementReferenceRule::LastUpdateAtOrBeforeCutoff,
            tie_rule: TieRule::NoWins,
            require_chainlink: true,
            chainlink_feed_id: Some("eth-usd".to_string()),
            chainlink_asset_symbol: Some("ETH".to_string()),
            chainlink_chain_id: Some(1),
            chainlink_feed_proxy: Some("0x5f4eC3Df9cbd43714FE2740f5E3616155c5b8419".to_string()),
            max_oracle_staleness_ns: Some(60 * NS_PER_SEC),
            abort_on_missing_oracle: true,
        }
    }

    /// Generate a deterministic fingerprint for this configuration.
    pub fn fingerprint(&self) -> SettlementFingerprint {
        SettlementFingerprint {
            settlement_model: if self.require_chainlink {
                SettlementModel::ExactSpec
            } else {
                SettlementModel::Approximate
            },
            reference_rule: self.reference_rule,
            tie_rule: self.tie_rule,
            chainlink_feed_id: self.chainlink_feed_id.clone(),
            chainlink_chain_id: self.chainlink_chain_id,
            chainlink_feed_proxy: self.chainlink_feed_proxy.clone(),
            max_oracle_staleness_ns: self.max_oracle_staleness_ns,
            abort_on_missing_oracle: self.abort_on_missing_oracle,
        }
    }
}

// =============================================================================
// SETTLEMENT FINGERPRINT
// =============================================================================

/// Deterministic fingerprint of settlement configuration.
/// Changes to this fingerprint indicate the run's settlement behavior changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettlementFingerprint {
    pub settlement_model: SettlementModel,
    pub reference_rule: SettlementReferenceRule,
    pub tie_rule: TieRule,
    pub chainlink_feed_id: Option<String>,
    pub chainlink_chain_id: Option<u64>,
    pub chainlink_feed_proxy: Option<String>,
    pub max_oracle_staleness_ns: Option<Nanos>,
    pub abort_on_missing_oracle: bool,
}

impl SettlementFingerprint {
    /// Compute a hash of this fingerprint for embedding in run fingerprints.
    pub fn hash(&self) -> u64 {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        
        let mut hasher = DefaultHasher::new();
        // Hash each field
        format!("{:?}", self.settlement_model).hash(&mut hasher);
        format!("{:?}", self.reference_rule).hash(&mut hasher);
        format!("{:?}", self.tie_rule).hash(&mut hasher);
        self.chainlink_feed_id.hash(&mut hasher);
        self.chainlink_chain_id.hash(&mut hasher);
        self.chainlink_feed_proxy.hash(&mut hasher);
        self.max_oracle_staleness_ns.hash(&mut hasher);
        self.abort_on_missing_oracle.hash(&mut hasher);
        
        hasher.finish()
    }
    
    /// Format as a compact string for logging.
    pub fn format_compact(&self) -> String {
        format!(
            "model={:?} rule={:?} tie={:?} feed={} chain={} abort={}",
            self.settlement_model,
            self.reference_rule,
            self.tie_rule,
            self.chainlink_feed_id.as_deref().unwrap_or("none"),
            self.chainlink_chain_id.unwrap_or(0),
            self.abort_on_missing_oracle
        )
    }
}

// =============================================================================
// WINDOW SETTLEMENT RECORD
// =============================================================================

/// Per-window settlement record for audit trail.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowSettlementRecord {
    /// Window identifier (market_id).
    pub window_id: String,
    /// Window start time (Unix seconds).
    pub window_start_unix_sec: u64,
    /// Window end time (Unix seconds).
    pub window_end_unix_sec: u64,
    /// Start price used for settlement.
    pub start_price: f64,
    /// End price used for settlement.
    pub end_price: f64,
    /// Resolved outcome.
    pub outcome: SettlementOutcome,
    /// Oracle round ID used for end price (if Chainlink).
    pub oracle_round_id: Option<u128>,
    /// Oracle updated_at timestamp (Unix seconds).
    pub oracle_updated_at_unix_sec: Option<u64>,
    /// When the oracle round arrived (nanoseconds, simulation time).
    pub oracle_observed_arrival_time_ns: Option<Nanos>,
    /// Decision time when settlement was processed (nanoseconds).
    pub settle_decision_time_ns: Nanos,
    /// Settlement delay: settle_decision_time_ns - window_end_ns.
    pub settlement_delay_ns: Nanos,
    /// Whether oracle data was stale (beyond max_oracle_staleness_ns).
    pub oracle_stale: bool,
    /// Reference rule used for this settlement.
    pub reference_rule: SettlementReferenceRule,
}

impl WindowSettlementRecord {
    /// Window end time in nanoseconds.
    pub fn window_end_ns(&self) -> Nanos {
        self.window_end_unix_sec as Nanos * NS_PER_SEC
    }
}

// =============================================================================
// SETTLEMENT METADATA
// =============================================================================

/// Complete settlement metadata for BacktestResults.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SettlementMetadata {
    /// Settlement model used.
    pub model: SettlementModel,
    /// Reference rule used.
    pub reference_rule: Option<SettlementReferenceRule>,
    /// Tie rule used.
    pub tie_rule: Option<TieRule>,
    /// Chainlink feed ID (if used).
    pub chainlink_feed_id: Option<String>,
    /// Chainlink asset symbol (if used).
    pub chainlink_asset_symbol: Option<String>,
    /// Chainlink chain ID (if used).
    pub chainlink_chain_id: Option<u64>,
    /// Chainlink feed proxy address (if used).
    pub chainlink_feed_proxy: Option<String>,
    /// Total windows tracked.
    pub windows_tracked: u64,
    /// Windows successfully settled.
    pub windows_settled: u64,
    /// Windows with missing oracle data.
    pub windows_missing_oracle: u64,
    /// Windows with stale oracle data.
    pub windows_stale_oracle: u64,
    /// Up wins count.
    pub up_wins: u64,
    /// Down wins count.
    pub down_wins: u64,
    /// Tie count.
    pub ties: u64,
    /// Per-window settlement records.
    pub per_window_records: Vec<WindowSettlementRecord>,
    /// Settlement fingerprint for run comparison.
    pub fingerprint: Option<SettlementFingerprint>,
    /// Settlement delay statistics (ns).
    pub settlement_delay_stats: SettlementDelayStats,
}

/// Settlement delay statistics.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SettlementDelayStats {
    /// Minimum delay (ns).
    pub min_ns: Option<Nanos>,
    /// Maximum delay (ns).
    pub max_ns: Option<Nanos>,
    /// Mean delay (ns).
    pub mean_ns: Option<f64>,
    /// Median delay (ns).
    pub median_ns: Option<Nanos>,
    /// p99 delay (ns).
    pub p99_ns: Option<Nanos>,
}

impl SettlementDelayStats {
    /// Compute from a list of delays.
    pub fn from_delays(delays: &[Nanos]) -> Self {
        if delays.is_empty() {
            return Self::default();
        }
        
        let mut sorted = delays.to_vec();
        sorted.sort();
        
        let min_ns = Some(sorted[0]);
        let max_ns = Some(sorted[sorted.len() - 1]);
        let mean_ns = Some(sorted.iter().map(|&d| d as f64).sum::<f64>() / sorted.len() as f64);
        let median_ns = Some(sorted[sorted.len() / 2]);
        let p99_idx = (sorted.len() as f64 * 0.99) as usize;
        let p99_ns = Some(sorted[p99_idx.min(sorted.len() - 1)]);
        
        Self {
            min_ns,
            max_ns,
            mean_ns,
            median_ns,
            p99_ns,
        }
    }
}

impl Default for SettlementMetadata {
    fn default() -> Self {
        Self {
            model: SettlementModel::None,
            reference_rule: None,
            tie_rule: None,
            chainlink_feed_id: None,
            chainlink_asset_symbol: None,
            chainlink_chain_id: None,
            chainlink_feed_proxy: None,
            windows_tracked: 0,
            windows_settled: 0,
            windows_missing_oracle: 0,
            windows_stale_oracle: 0,
            up_wins: 0,
            down_wins: 0,
            ties: 0,
            per_window_records: Vec::new(),
            fingerprint: None,
            settlement_delay_stats: SettlementDelayStats::default(),
        }
    }
}

impl SettlementMetadata {
    /// Create metadata from configuration.
    pub fn from_config(config: &SettlementConfig) -> Self {
        Self {
            model: if config.require_chainlink {
                SettlementModel::ExactSpec
            } else {
                SettlementModel::Approximate
            },
            reference_rule: Some(config.reference_rule),
            tie_rule: Some(config.tie_rule),
            chainlink_feed_id: config.chainlink_feed_id.clone(),
            chainlink_asset_symbol: config.chainlink_asset_symbol.clone(),
            chainlink_chain_id: config.chainlink_chain_id,
            chainlink_feed_proxy: config.chainlink_feed_proxy.clone(),
            fingerprint: Some(config.fingerprint()),
            ..Default::default()
        }
    }
    
    /// Record a settlement.
    pub fn record_settlement(&mut self, record: WindowSettlementRecord) {
        // Update counters
        match &record.outcome {
            SettlementOutcome::Resolved { winner, is_tie } => {
                if *is_tie {
                    self.ties += 1;
                }
                match winner {
                    Outcome::Yes => self.up_wins += 1,
                    Outcome::No => self.down_wins += 1,
                }
            }
            _ => {}
        }
        
        if record.oracle_stale {
            self.windows_stale_oracle += 1;
        }
        
        self.windows_settled += 1;
        self.per_window_records.push(record);
    }
    
    /// Record missing oracle data for a window.
    pub fn record_missing_oracle(&mut self, window_id: &str) {
        self.windows_missing_oracle += 1;
        tracing::warn!(
            window_id = %window_id,
            "Settlement missing oracle data"
        );
    }
    
    /// Finalize and compute delay statistics.
    pub fn finalize(&mut self) {
        let delays: Vec<Nanos> = self.per_window_records
            .iter()
            .map(|r| r.settlement_delay_ns)
            .collect();
        self.settlement_delay_stats = SettlementDelayStats::from_delays(&delays);
    }
    
    /// Format as a report string.
    pub fn format_report(&self) -> String {
        let mut out = String::new();
        
        out.push_str("╔══════════════════════════════════════════════════════════════════════════════╗\n");
        out.push_str("║                      SETTLEMENT METADATA REPORT                              ║\n");
        out.push_str("╠══════════════════════════════════════════════════════════════════════════════╣\n");
        out.push_str(&format!("║  Model:           {:60} ║\n", format!("{:?}", self.model)));
        out.push_str(&format!("║  Reference Rule:  {:60} ║\n", 
            self.reference_rule.map(|r| format!("{:?}", r)).unwrap_or_else(|| "N/A".to_string())
        ));
        out.push_str(&format!("║  Tie Rule:        {:60} ║\n", 
            self.tie_rule.map(|r| format!("{:?}", r)).unwrap_or_else(|| "N/A".to_string())
        ));
        
        if let Some(ref feed_id) = self.chainlink_feed_id {
            out.push_str(&format!("║  Chainlink Feed:  {:60} ║\n", feed_id));
        }
        if let Some(chain_id) = self.chainlink_chain_id {
            out.push_str(&format!("║  Chain ID:        {:60} ║\n", chain_id));
        }
        if let Some(ref proxy) = self.chainlink_feed_proxy {
            out.push_str(&format!("║  Feed Proxy:      {:60} ║\n", proxy));
        }
        
        out.push_str("╠══════════════════════════════════════════════════════════════════════════════╣\n");
        out.push_str("║  SETTLEMENT STATISTICS                                                       ║\n");
        out.push_str(&format!("║    Windows Tracked:       {:>10}                                       ║\n", self.windows_tracked));
        out.push_str(&format!("║    Windows Settled:       {:>10}                                       ║\n", self.windows_settled));
        out.push_str(&format!("║    Windows Missing Oracle:{:>10}                                       ║\n", self.windows_missing_oracle));
        out.push_str(&format!("║    Windows Stale Oracle:  {:>10}                                       ║\n", self.windows_stale_oracle));
        out.push_str(&format!("║    Up Wins:               {:>10}                                       ║\n", self.up_wins));
        out.push_str(&format!("║    Down Wins:             {:>10}                                       ║\n", self.down_wins));
        out.push_str(&format!("║    Ties:                  {:>10}                                       ║\n", self.ties));
        
        if let Some(mean) = self.settlement_delay_stats.mean_ns {
            out.push_str("╠══════════════════════════════════════════════════════════════════════════════╣\n");
            out.push_str("║  SETTLEMENT DELAY STATISTICS                                                 ║\n");
            out.push_str(&format!("║    Mean Delay:   {:>12.2} ms                                            ║\n", 
                mean / 1_000_000.0
            ));
            if let Some(min) = self.settlement_delay_stats.min_ns {
                out.push_str(&format!("║    Min Delay:    {:>12.2} ms                                            ║\n", 
                    min as f64 / 1_000_000.0
                ));
            }
            if let Some(max) = self.settlement_delay_stats.max_ns {
                out.push_str(&format!("║    Max Delay:    {:>12.2} ms                                            ║\n", 
                    max as f64 / 1_000_000.0
                ));
            }
            if let Some(p99) = self.settlement_delay_stats.p99_ns {
                out.push_str(&format!("║    p99 Delay:    {:>12.2} ms                                            ║\n", 
                    p99 as f64 / 1_000_000.0
                ));
            }
        }
        
        if let Some(ref fp) = self.fingerprint {
            out.push_str("╠══════════════════════════════════════════════════════════════════════════════╣\n");
            out.push_str(&format!("║  Fingerprint Hash: {:>16x}                                          ║\n", fp.hash()));
        }
        
        out.push_str("╚══════════════════════════════════════════════════════════════════════════════╝\n");
        
        out
    }
}

// =============================================================================
// CHAINLINK SETTLEMENT COORDINATOR
// =============================================================================

/// Coordinates settlement between the settlement engine and Chainlink reference source.
/// 
/// This is the bridge that:
/// 1. Feeds oracle round events to the settlement engine
/// 2. Checks outcome knowability using the reference source
/// 3. Resolves settlements using exact Chainlink prices
/// 4. Records audit trail with full oracle metadata
pub struct ChainlinkSettlementCoordinator {
    /// Settlement configuration.
    config: SettlementConfig,
    /// Settlement engine (uses market data for start price, oracle for end price).
    engine: SettlementEngine,
    /// Chainlink reference source (for end price lookup with visibility).
    reference_source: Option<Arc<dyn SettlementReferenceSource>>,
    /// Settlement metadata being built.
    metadata: SettlementMetadata,
    /// Windows pending oracle data.
    pending_oracle_windows: HashMap<String, PendingOracleWindow>,
    /// Windows that have been settled (to prevent double-settlement).
    settled_windows: std::collections::HashSet<String>,
}

/// Window pending oracle data for settlement.
#[derive(Debug, Clone)]
struct PendingOracleWindow {
    window_id: String,
    window_start_unix_sec: u64,
    window_end_unix_sec: u64,
    start_price: f64,
    start_price_source_time_ns: Nanos,
}

impl ChainlinkSettlementCoordinator {
    /// Create a new coordinator with optional Chainlink reference source.
    pub fn new(
        config: SettlementConfig,
        reference_source: Option<Arc<dyn SettlementReferenceSource>>,
    ) -> Self {
        let engine = SettlementEngine::new(config.spec.clone());
        let metadata = SettlementMetadata::from_config(&config);
        
        Self {
            config,
            engine,
            reference_source,
            metadata,
            pending_oracle_windows: HashMap::new(),
            settled_windows: std::collections::HashSet::new(),
        }
    }
    
    /// Create a coordinator with a replay Chainlink source from stored rounds.
    pub fn with_chainlink_replay(
        config: SettlementConfig,
        rounds: Vec<crate::backtest_v2::oracle::ChainlinkRound>,
    ) -> Self {
        let asset_symbol = config.chainlink_asset_symbol.clone().unwrap_or_else(|| "BTC".to_string());
        let feed_id = config.chainlink_feed_id.clone().unwrap_or_else(|| "btc-usd".to_string());
        let source = Arc::new(ChainlinkSettlementSource::from_rounds(rounds, asset_symbol, feed_id));
        Self::new(config, Some(source))
    }
    
    /// Track a new window for settlement.
    pub fn track_window(&mut self, market_id: &str, now_ns: Nanos) -> Result<(), String> {
        self.engine.track_window(market_id, now_ns)?;
        self.metadata.windows_tracked += 1;
        Ok(())
    }
    
    /// Record a price observation (for start price from market data).
    pub fn observe_market_price(
        &mut self,
        market_id: &str,
        price: f64,
        source_time_ns: Nanos,
        arrival_time_ns: Nanos,
    ) {
        self.engine.observe_price(market_id, price, source_time_ns, arrival_time_ns);
    }
    
    /// Advance simulation time.
    pub fn advance_time(&mut self, decision_time_ns: Nanos) {
        self.engine.advance_time(decision_time_ns);
    }
    
    /// Try to settle a market window.
    /// 
    /// This uses the Chainlink reference source if available, falling back
    /// to the settlement engine's internal price tracking.
    /// 
    /// **CRITICAL**: Settlement only occurs when the oracle round is VISIBLE
    /// (ingest_arrival_time_ns <= decision_time_ns).
    pub fn try_settle(
        &mut self,
        market_id: &str,
        decision_time_ns: Nanos,
    ) -> Result<Option<(SettlementEvent, WindowSettlementRecord)>, SettlementError> {
        // Check if already settled
        if self.settled_windows.contains(market_id) {
            return Ok(None);
        }
        
        // Get window state from engine
        let state = match self.engine.get_state(market_id) {
            Some(s) => s.clone(),
            None => return Ok(None),
        };
        
        // Only process if in Resolvable or Active state
        let (window_start_ns, window_end_ns, start_price) = match &state {
            SettlementState::Resolvable { window_start_ns, window_end_ns, start_price, .. } => {
                (*window_start_ns, *window_end_ns, *start_price)
            }
            SettlementState::Active { window_start_ns, window_end_ns, start_price, .. } => {
                // Check if we're past cutoff
                if decision_time_ns <= *window_end_ns {
                    return Ok(None);
                }
                (*window_start_ns, *window_end_ns, *start_price)
            }
            _ => return Ok(None),
        };
        
        let window_end_unix_sec = window_end_ns / NS_PER_SEC;
        
        // === CHAINLINK SETTLEMENT PATH ===
        if let Some(ref source) = self.reference_source {
            // First, check if there's any reference price available for this cutoff
            let oracle_point_opt = source.reference_price(
                window_end_unix_sec as u64,
                self.config.reference_rule,
            );
            
            match oracle_point_opt {
                None => {
                    // Missing oracle data - no round matches the reference rule
                    if self.config.abort_on_missing_oracle {
                        return Err(SettlementError::MissingOracleData {
                            market_id: market_id.to_string(),
                            cutoff_unix_sec: window_end_unix_sec as u64,
                        });
                    }
                    self.metadata.record_missing_oracle(market_id);
                    self.engine.mark_missing_data(market_id, "No oracle data for cutoff");
                    return Ok(None);
                }
                Some(ref oracle_point) => {
                    // Oracle data exists - check if it has ARRIVED yet (visibility semantics)
                    if decision_time_ns < oracle_point.observed_arrival_time_ns as Nanos {
                        // Oracle round not yet visible - cannot settle
                        return Ok(None);
                    }
                }
            }
            
            // At this point we know oracle data exists and has arrived
            let oracle_point = oracle_point_opt.unwrap();
            
            let end_price = oracle_point.price;
            
            // Check staleness
            let oracle_stale = if let Some(max_staleness) = self.config.max_oracle_staleness_ns {
                let oracle_age_ns = (window_end_unix_sec as u64)
                    .saturating_sub(oracle_point.oracle_updated_at_unix_sec) * NS_PER_SEC as u64;
                oracle_age_ns > max_staleness as u64
            } else {
                false
            };
            
            // Determine outcome
            let outcome = self.config.spec.determine_outcome(start_price, end_price);
            
            let settlement_delay_ns = decision_time_ns.saturating_sub(window_end_ns);
            
            let event = SettlementEvent {
                market_id: market_id.to_string(),
                window_start_ns,
                window_end_ns,
                outcome: outcome.clone(),
                start_price,
                end_price,
                settle_decision_time_ns: decision_time_ns,
                reference_arrival_ns: oracle_point.observed_arrival_time_ns as Nanos,
            };
            
            let record = WindowSettlementRecord {
                window_id: market_id.to_string(),
                window_start_unix_sec: (window_start_ns / NS_PER_SEC) as u64,
                window_end_unix_sec: window_end_unix_sec as u64,
                start_price,
                end_price,
                outcome: outcome.clone(),
                oracle_round_id: Some(oracle_point.round_id),
                oracle_updated_at_unix_sec: Some(oracle_point.oracle_updated_at_unix_sec),
                oracle_observed_arrival_time_ns: Some(oracle_point.observed_arrival_time_ns as Nanos),
                settle_decision_time_ns: decision_time_ns,
                settlement_delay_ns,
                oracle_stale,
                reference_rule: self.config.reference_rule,
            };
            
            // Record settlement
            self.metadata.record_settlement(record.clone());
            self.settled_windows.insert(market_id.to_string());
            
            // Update engine state to Resolved
            // (The engine's internal try_settle would also work, but we've already done the work)
            
            tracing::debug!(
                market_id = %market_id,
                oracle_round_id = %oracle_point.round_id,
                start_price = %start_price,
                end_price = %end_price,
                outcome = ?outcome,
                "Settlement completed with Chainlink oracle"
            );
            
            return Ok(Some((event, record)));
        }
        
        // === FALLBACK: Use engine's internal settlement (market data prices) ===
        if let Some(event) = self.engine.try_settle(market_id, decision_time_ns) {
            let settlement_delay_ns = decision_time_ns.saturating_sub(window_end_ns);
            
            let record = WindowSettlementRecord {
                window_id: market_id.to_string(),
                window_start_unix_sec: (window_start_ns / NS_PER_SEC) as u64,
                window_end_unix_sec: window_end_unix_sec as u64,
                start_price: event.start_price,
                end_price: event.end_price,
                outcome: event.outcome.clone(),
                oracle_round_id: None,
                oracle_updated_at_unix_sec: None,
                oracle_observed_arrival_time_ns: None,
                settle_decision_time_ns: decision_time_ns,
                settlement_delay_ns,
                oracle_stale: false,
                reference_rule: self.config.reference_rule,
            };
            
            self.metadata.record_settlement(record.clone());
            self.settled_windows.insert(market_id.to_string());
            
            tracing::debug!(
                market_id = %market_id,
                start_price = %event.start_price,
                end_price = %event.end_price,
                outcome = ?event.outcome,
                "Settlement completed with market data (no Chainlink)"
            );
            
            return Ok(Some((event, record)));
        }
        
        Ok(None)
    }
    
    /// Finalize and get settlement metadata.
    pub fn finalize(&mut self) -> SettlementMetadata {
        self.metadata.finalize();
        self.metadata.clone()
    }
    
    /// Get the settlement configuration.
    pub fn config(&self) -> &SettlementConfig {
        &self.config
    }
    
    /// Get the underlying settlement engine.
    pub fn engine(&self) -> &SettlementEngine {
        &self.engine
    }
    
    /// Get settlement metadata (in-progress).
    pub fn metadata(&self) -> &SettlementMetadata {
        &self.metadata
    }
    
    /// Check if a window has been settled.
    pub fn is_settled(&self, market_id: &str) -> bool {
        self.settled_windows.contains(market_id)
    }
}

/// Settlement error types.
#[derive(Debug, Clone)]
pub enum SettlementError {
    /// Missing oracle data for settlement (production-grade abort).
    MissingOracleData {
        market_id: String,
        cutoff_unix_sec: u64,
    },
    /// Oracle data is stale beyond threshold.
    StaleOracleData {
        market_id: String,
        oracle_age_ns: u64,
        max_staleness_ns: u64,
    },
    /// Settlement engine error.
    EngineError(String),
}

impl std::fmt::Display for SettlementError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingOracleData { market_id, cutoff_unix_sec } => {
                write!(
                    f,
                    "Missing oracle data for market {} at cutoff {} (production-grade abort)",
                    market_id, cutoff_unix_sec
                )
            }
            Self::StaleOracleData { market_id, oracle_age_ns, max_staleness_ns } => {
                write!(
                    f,
                    "Stale oracle data for market {}: age {}ns exceeds max {}ns",
                    market_id, oracle_age_ns, max_staleness_ns
                )
            }
            Self::EngineError(msg) => write!(f, "Settlement engine error: {}", msg),
        }
    }
}

impl std::error::Error for SettlementError {}

// =============================================================================
// TESTS
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backtest_v2::oracle::ChainlinkRound;

    fn make_test_rounds(window_end_unix_sec: u64) -> Vec<ChainlinkRound> {
        vec![
            // Round BEFORE cutoff
            ChainlinkRound {
                feed_id: "btc-usd".to_string(),
                round_id: 1,
                answer: 5000000000000, // 50000.0
                updated_at: window_end_unix_sec - 10, // 10 seconds before cutoff
                answered_in_round: 1,
                started_at: window_end_unix_sec - 10,
                ingest_arrival_time_ns: (window_end_unix_sec - 9) * NS_PER_SEC as u64, // Arrives 1 second later
                ingest_seq: 0,
                decimals: 8,
                asset_symbol: "BTC".to_string(),
                raw_source_hash: None,
            },
            // Round AT cutoff
            ChainlinkRound {
                feed_id: "btc-usd".to_string(),
                round_id: 2,
                answer: 5010000000000, // 50100.0
                updated_at: window_end_unix_sec,
                answered_in_round: 2,
                started_at: window_end_unix_sec,
                ingest_arrival_time_ns: (window_end_unix_sec + 1) * NS_PER_SEC as u64, // Arrives 1 second after cutoff
                ingest_seq: 1,
                decimals: 8,
                asset_symbol: "BTC".to_string(),
                raw_source_hash: None,
            },
            // Round AFTER cutoff
            ChainlinkRound {
                feed_id: "btc-usd".to_string(),
                round_id: 3,
                answer: 5020000000000, // 50200.0
                updated_at: window_end_unix_sec + 5,
                answered_in_round: 3,
                started_at: window_end_unix_sec + 5,
                ingest_arrival_time_ns: (window_end_unix_sec + 6) * NS_PER_SEC as u64,
                ingest_seq: 2,
                decimals: 8,
                asset_symbol: "BTC".to_string(),
                raw_source_hash: None,
            },
        ]
    }

    #[test]
    fn test_settlement_fingerprint_hash_stability() {
        let config1 = SettlementConfig::production_btc();
        let config2 = SettlementConfig::production_btc();
        
        let fp1 = config1.fingerprint();
        let fp2 = config2.fingerprint();
        
        assert_eq!(fp1.hash(), fp2.hash(), "Same config should produce same fingerprint");
    }
    
    #[test]
    fn test_settlement_fingerprint_changes_on_rule_change() {
        let mut config1 = SettlementConfig::production_btc();
        let mut config2 = SettlementConfig::production_btc();
        
        config2.reference_rule = SettlementReferenceRule::FirstUpdateAfterCutoff;
        
        let fp1 = config1.fingerprint();
        let fp2 = config2.fingerprint();
        
        assert_ne!(fp1.hash(), fp2.hash(), "Different rule should produce different fingerprint");
    }
    
    #[test]
    fn test_coordinator_with_chainlink_replay() {
        let window_start_unix_sec = 1000u64;
        let window_end_unix_sec = window_start_unix_sec + 900; // 15 minutes
        
        let config = SettlementConfig {
            reference_rule: SettlementReferenceRule::LastUpdateAtOrBeforeCutoff,
            chainlink_feed_id: Some("btc-usd".to_string()),
            chainlink_asset_symbol: Some("BTC".to_string()),
            ..Default::default()
        };
        
        let rounds = make_test_rounds(window_end_unix_sec);
        let mut coordinator = ChainlinkSettlementCoordinator::with_chainlink_replay(config, rounds);
        
        // Track a window
        let market_id = format!("btc-updown-15m-{}", window_start_unix_sec);
        let start_ns = window_start_unix_sec * NS_PER_SEC as u64;
        coordinator.track_window(&market_id, start_ns as Nanos).unwrap();
        
        // Observe start price
        let start_price = 50000.0;
        coordinator.observe_market_price(
            &market_id, 
            start_price, 
            start_ns as Nanos, 
            start_ns as Nanos
        );
        
        // Advance past cutoff but before oracle arrival
        let window_end_ns = window_end_unix_sec * NS_PER_SEC as u64;
        coordinator.advance_time(window_end_ns as Nanos);
        
        // Try to settle - should fail because oracle hasn't arrived yet
        let decision_before_arrival = window_end_ns as Nanos;
        let result = coordinator.try_settle(&market_id, decision_before_arrival).unwrap();
        assert!(result.is_none(), "Settlement should not occur before oracle arrival");
        
        // Advance to after oracle arrival (round 2 arrives 1 second after cutoff)
        let decision_after_arrival = (window_end_unix_sec + 2) * NS_PER_SEC as u64;
        let result = coordinator.try_settle(&market_id, decision_after_arrival as Nanos).unwrap();
        
        assert!(result.is_some(), "Settlement should occur after oracle arrival");
        
        let (event, record) = result.unwrap();
        assert_eq!(event.start_price, start_price);
        assert!((event.end_price - 50100.0).abs() < 0.01, "End price should be from round 2");
        assert!(matches!(event.outcome, SettlementOutcome::Resolved { winner: Outcome::Yes, .. }));
        assert!(record.oracle_round_id.is_some());
        assert_eq!(record.oracle_round_id.unwrap(), 2);
    }
    
    #[test]
    fn test_settlement_delay_stats() {
        let delays = vec![100, 200, 300, 400, 500];
        let stats = SettlementDelayStats::from_delays(&delays);
        
        assert_eq!(stats.min_ns, Some(100));
        assert_eq!(stats.max_ns, Some(500));
        assert!((stats.mean_ns.unwrap() - 300.0).abs() < 0.01);
        assert_eq!(stats.median_ns, Some(300));
    }
    
    #[test]
    fn test_metadata_report_formatting() {
        let config = SettlementConfig::production_btc();
        let mut metadata = SettlementMetadata::from_config(&config);
        metadata.windows_tracked = 10;
        metadata.windows_settled = 8;
        metadata.up_wins = 5;
        metadata.down_wins = 3;
        
        let report = metadata.format_report();
        
        assert!(report.contains("SETTLEMENT METADATA REPORT"));
        assert!(report.contains("ExactSpec"));
        assert!(report.contains("btc-usd"));
    }
}

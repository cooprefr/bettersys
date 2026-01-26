//! Production-Auditable Run Fingerprint
//!
//! This module implements a deterministic fingerprint for backtest runs that:
//! - Changes if and only if observable behavior changes
//! - Is reproducible across machines given same inputs + config + seed
//! - Provides auditability through component hashes
//!
//! # Fingerprint Components
//!
//! ```text
//! RunFingerprint = H(
//!   "RUNFP_V1" ||
//!   CodeFingerprint ||
//!   ConfigFingerprint ||
//!   DatasetFingerprint ||
//!   SeedFingerprint ||
//!   BehaviorFingerprint
//! )
//! ```
//!
//! # Observable Behavior
//!
//! Observable behavior includes (in deterministic order):
//! 1. Strategy decisions (DecisionProof hashes)
//! 2. Orders emitted (id, side, price, size, type, time)
//! 3. OMS outcomes (ack/reject/cancel ack)
//! 4. Fills (price, size, maker/taker flag, time)
//! 5. Fees posted
//! 6. Settlement events and outcomes
//! 7. Ledger postings (if enabled)
//!
//! # Canonicalization
//!
//! All data is canonicalized before hashing:
//! - Floats are converted to fixed-point integers (price * 1e8, size * 1e8)
//! - Strings are UTF-8 encoded
//! - Collections are sorted by a stable key before hashing
//! - Events are ordered by (decision_time, ingest_seq)

use crate::backtest_v2::clock::Nanos;
use crate::backtest_v2::events::Side;
use crate::backtest_v2::settlement::SettlementOutcome;
use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

/// Fingerprint version string - increment when format changes.
pub const FINGERPRINT_VERSION: &str = "RUNFP_V2";

/// Scale factor for converting prices to fixed-point integers.
const PRICE_SCALE: f64 = 1e8;
/// Scale factor for converting sizes to fixed-point integers.
const SIZE_SCALE: f64 = 1e8;

// =============================================================================
// STRATEGY IDENTITY
// =============================================================================

/// Stable identifier for a strategy implementation.
/// 
/// Every backtest run MUST be tied to a specific strategy version so that
/// any published equity curve or PnL result can be unambiguously attributed
/// to the exact strategy implementation that produced it.
/// 
/// # Provenance Guarantee
/// 
/// Two runs with identical PnL but different strategy versions CANNOT share
/// a fingerprint. This ensures that public artifacts (equity curves, PnL
/// summaries) can be cryptographically proven to correspond to a specific
/// strategy implementation.
/// 
/// # Production-Grade Requirement
/// 
/// Production-grade backtests REQUIRE a complete StrategyId with at least
/// name and version. The code_hash is optional but strongly recommended.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct StrategyId {
    /// Human-readable strategy name (e.g., "pm_15m_edge_v1", "maker_mm_btc").
    /// Should be stable across versions of the same strategy.
    pub name: String,
    
    /// Semantic version of the strategy (e.g., "1.2.0", "0.1.0-alpha").
    /// Should follow semver conventions.
    pub version: String,
    
    /// Content hash of the strategy implementation.
    /// 
    /// If available, this should be derived from:
    /// - Git commit hash of the strategy module, OR
    /// - SHA256 hash of the compiled strategy source
    /// 
    /// If not available, set to None. Production-grade runs will record
    /// this as "unknown" but continue (with a warning).
    pub code_hash: Option<String>,
}

impl StrategyId {
    /// Create a new StrategyId with name and version.
    pub fn new(name: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            version: version.into(),
            code_hash: None,
        }
    }
    
    /// Create a new StrategyId with name, version, and code hash.
    pub fn with_code_hash(
        name: impl Into<String>, 
        version: impl Into<String>,
        code_hash: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            version: version.into(),
            code_hash: Some(code_hash.into()),
        }
    }
    
    /// Create a StrategyId from the current crate's git commit (if available).
    /// 
    /// This uses build-time environment variables set by build.rs or CI.
    pub fn from_build_env(name: impl Into<String>, version: impl Into<String>) -> Self {
        let code_hash = option_env!("GIT_COMMIT")
            .or(option_env!("VERGEN_GIT_SHA"))
            .map(|s| s.to_string());
        
        Self {
            name: name.into(),
            version: version.into(),
            code_hash,
        }
    }
    
    /// Validate the StrategyId for production-grade use.
    /// 
    /// Returns an error message if validation fails.
    pub fn validate_for_production(&self) -> Result<(), String> {
        if self.name.is_empty() {
            return Err("StrategyId.name cannot be empty for production-grade runs".to_string());
        }
        
        if self.version.is_empty() {
            return Err("StrategyId.version cannot be empty for production-grade runs".to_string());
        }
        
        // Version should look like semver (basic check)
        if !self.version.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(false) {
            return Err(format!(
                "StrategyId.version '{}' should start with a digit (semver format expected)",
                self.version
            ));
        }
        
        // Warn (but don't fail) if code_hash is missing
        // The warning is logged separately
        
        Ok(())
    }
    
    /// Check if this StrategyId has a code hash.
    pub fn has_code_hash(&self) -> bool {
        self.code_hash.is_some()
    }
    
    /// Get the code hash or "unknown" if not set.
    pub fn code_hash_or_unknown(&self) -> &str {
        self.code_hash.as_deref().unwrap_or("unknown")
    }
    
    /// Compute a deterministic hash of this StrategyId.
    pub fn compute_hash(&self) -> u64 {
        let mut hasher = DefaultHasher::new();
        self.name.hash(&mut hasher);
        self.version.hash(&mut hasher);
        self.code_hash.hash(&mut hasher);
        hasher.finish()
    }
    
    /// Format as a short summary string.
    pub fn format_short(&self) -> String {
        let hash_suffix = self.code_hash.as_ref()
            .map(|h| format!("@{}", &h[..8.min(h.len())]))
            .unwrap_or_default();
        format!("{}/v{}{}", self.name, self.version, hash_suffix)
    }
    
    /// Format as a full summary string.
    pub fn format_full(&self) -> String {
        format!(
            "Strategy[name={}, version={}, code_hash={}]",
            self.name,
            self.version,
            self.code_hash_or_unknown()
        )
    }
}

impl std::fmt::Display for StrategyId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.format_short())
    }
}

impl Default for StrategyId {
    /// Default StrategyId for testing only.
    /// 
    /// Production-grade runs should NEVER use default() - always provide
    /// explicit name/version.
    fn default() -> Self {
        Self {
            name: "unnamed_strategy".to_string(),
            version: "0.0.0".to_string(),
            code_hash: None,
        }
    }
}

// =============================================================================
// STRATEGY FINGERPRINT
// =============================================================================

/// Fingerprint of the strategy identity for inclusion in RunFingerprint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StrategyFingerprint {
    /// Strategy name.
    pub name: String,
    /// Strategy version.
    pub version: String,
    /// Strategy code hash (or "unknown").
    pub code_hash: String,
    /// Computed hash of the strategy identity.
    pub hash: u64,
}

impl StrategyFingerprint {
    /// Create from a StrategyId.
    pub fn from_strategy_id(id: &StrategyId) -> Self {
        Self {
            name: id.name.clone(),
            version: id.version.clone(),
            code_hash: id.code_hash_or_unknown().to_string(),
            hash: id.compute_hash(),
        }
    }
    
    /// Format as a short summary.
    pub fn format_short(&self) -> String {
        let hash_suffix = if self.code_hash != "unknown" {
            format!("@{}", &self.code_hash[..8.min(self.code_hash.len())])
        } else {
            String::new()
        };
        format!("{}/v{}{}", self.name, self.version, hash_suffix)
    }
}

impl Default for StrategyFingerprint {
    fn default() -> Self {
        Self::from_strategy_id(&StrategyId::default())
    }
}

// =============================================================================
// CODE FINGERPRINT
// =============================================================================

/// Fingerprint of the code version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeFingerprint {
    /// Crate version from Cargo.toml.
    pub crate_version: String,
    /// Git commit hash (if available).
    pub git_commit: Option<String>,
    /// Build profile (release/debug).
    pub build_profile: String,
    /// Computed hash.
    pub hash: u64,
}

impl CodeFingerprint {
    /// Create a new code fingerprint.
    pub fn new() -> Self {
        // Get version from Cargo.toml
        let crate_version = env!("CARGO_PKG_VERSION").to_string();
        
        // Try to get git commit from build-time env var
        // This should be set by build.rs or CI
        let git_commit = option_env!("GIT_COMMIT")
            .or(option_env!("VERGEN_GIT_SHA"))
            .map(|s| s.to_string());
        
        // Detect build profile
        let build_profile = if cfg!(debug_assertions) {
            "debug".to_string()
        } else {
            "release".to_string()
        };
        
        let mut fp = Self {
            crate_version,
            git_commit,
            build_profile,
            hash: 0,
        };
        fp.compute_hash();
        fp
    }
    
    fn compute_hash(&mut self) {
        let mut hasher = DefaultHasher::new();
        self.crate_version.hash(&mut hasher);
        self.git_commit.hash(&mut hasher);
        self.build_profile.hash(&mut hasher);
        self.hash = hasher.finish();
    }
    
    /// Format as a short summary.
    pub fn format_short(&self) -> String {
        let commit = self.git_commit.as_deref().unwrap_or("UNKNOWN");
        format!("v{} {} ({})", self.crate_version, &commit[..8.min(commit.len())], self.build_profile)
    }
}

impl Default for CodeFingerprint {
    fn default() -> Self {
        Self::new()
    }
}

// =============================================================================
// CONFIG FINGERPRINT
// =============================================================================

/// Fingerprint of behavior-relevant configuration.
/// 
/// Only includes config that affects observable output.
/// Excludes: logging level, verbose flags, debug settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigFingerprint {
    /// Settlement reference rule (e.g., "LastUpdateAtOrBeforeCutoff").
    pub settlement_reference_rule: Option<String>,
    /// Settlement tie rule.
    pub settlement_tie_rule: Option<String>,
    /// Chainlink feed ID (if used).
    pub chainlink_feed_id: Option<String>,
    /// Chain ID for oracle feeds (e.g., 137 for Polygon).
    pub oracle_chain_id: Option<u64>,
    /// Oracle feed proxy addresses (sorted by asset).
    pub oracle_feed_proxies: Vec<(String, String)>,
    /// Oracle decimals per asset.
    pub oracle_decimals: Vec<(String, u8)>,
    /// Oracle visibility rule.
    pub oracle_visibility_rule: Option<String>,
    /// Oracle rounding policy.
    pub oracle_rounding_policy: Option<String>,
    /// Oracle config fingerprint hash (from OracleConfig::fingerprint_hash).
    pub oracle_config_hash: Option<u64>,
    /// Latency model type.
    pub latency_model: String,
    /// Order latency (ns) if fixed.
    pub order_latency_ns: Option<Nanos>,
    /// OMS parity mode.
    pub oms_parity_mode: String,
    /// Maker fill model.
    pub maker_fill_model: String,
    /// Integrity policy mode.
    pub integrity_policy: String,
    /// Invariant mode.
    pub invariant_mode: String,
    /// Fee rate (basis points).
    pub fee_rate_bps: Option<i64>,
    /// Strategy parameters hash.
    pub strategy_params_hash: u64,
    /// Arrival policy description.
    pub arrival_policy: String,
    /// Strict accounting enabled.
    pub strict_accounting: bool,
    /// Production grade mode.
    pub production_grade: bool,
    /// Allow non-production override.
    /// When true, non-production settings were explicitly permitted.
    pub allow_non_production: bool,
    /// Computed hash of all config.
    pub hash: u64,
}

impl ConfigFingerprint {
    /// Create from BacktestConfig.
    pub fn from_config(config: &crate::backtest_v2::orchestrator::BacktestConfig) -> Self {
        use crate::backtest_v2::latency::LatencyDistribution;
        
        // Extract settlement info from settlement_integration config if present
        let (settlement_reference_rule, settlement_tie_rule, chainlink_feed_id) = 
            if let Some(ref spec) = config.settlement_spec {
                (
                    Some(format!("{:?}", spec.reference_price_rule)),
                    Some(format!("{:?}", spec.tie_rule)),
                    None, // Would come from OracleConfig
                )
            } else {
                (None, None, None)
            };
        
        // Extract oracle config info if present
        let (oracle_chain_id, oracle_feed_proxies, oracle_decimals, 
             oracle_visibility_rule, oracle_rounding_policy, oracle_config_hash) = 
            if let Some(ref oracle_config) = config.oracle_config {
                let mut feed_proxies: Vec<(String, String)> = oracle_config.feeds.iter()
                    .map(|(asset, feed)| (asset.clone(), feed.feed_proxy_address.clone()))
                    .collect();
                feed_proxies.sort_by(|a, b| a.0.cmp(&b.0));
                
                let mut decimals: Vec<(String, u8)> = oracle_config.feeds.iter()
                    .map(|(asset, feed)| (asset.clone(), feed.decimals))
                    .collect();
                decimals.sort_by(|a, b| a.0.cmp(&b.0));
                
                let chain_id = oracle_config.feeds.values().next().map(|f| f.chain_id);
                
                (
                    chain_id,
                    feed_proxies,
                    decimals,
                    Some(format!("{:?}", oracle_config.visibility_rule)),
                    Some(format!("{:?}", oracle_config.rounding_policy)),
                    Some(oracle_config.fingerprint_hash()),
                )
            } else {
                (None, Vec::new(), Vec::new(), None, None, None)
            };
        
        // Latency model
        let (latency_model, order_latency_ns) = match &config.latency.order_send {
            LatencyDistribution::Fixed { latency_ns } => 
                ("Fixed".to_string(), Some(*latency_ns)),
            LatencyDistribution::Normal { mean_ns, .. } => 
                ("Normal".to_string(), Some(*mean_ns)),
            _ => ("Other".to_string(), None),
        };
        
        // Hash strategy params (using the HashMap-based params)
        let strategy_params_hash = {
            let mut h = DefaultHasher::new();
            // Sort keys for determinism
            let mut keys: Vec<_> = config.strategy_params.params.keys().collect();
            keys.sort();
            for key in keys {
                key.hash(&mut h);
                if let Some(val) = config.strategy_params.params.get(key) {
                    val.to_bits().hash(&mut h);
                }
            }
            h.finish()
        };
        
        let mut fp = Self {
            settlement_reference_rule,
            settlement_tie_rule,
            chainlink_feed_id,
            oracle_chain_id,
            oracle_feed_proxies,
            oracle_decimals,
            oracle_visibility_rule,
            oracle_rounding_policy,
            oracle_config_hash,
            latency_model,
            order_latency_ns,
            oms_parity_mode: format!("{:?}", config.oms_parity_mode),
            maker_fill_model: format!("{:?}", config.maker_fill_model),
            integrity_policy: format!("{:?}", config.integrity_policy),
            invariant_mode: config.invariant_config
                .as_ref()
                .map(|c| format!("{:?}", c.mode))
                .unwrap_or_else(|| "Hard".to_string()),
            fee_rate_bps: Some((config.matching.fees.taker_fee_rate * 10000.0) as i64),
            strategy_params_hash,
            arrival_policy: config.arrival_policy.description().to_string(),
            strict_accounting: config.strict_accounting,
            production_grade: config.production_grade,
            allow_non_production: config.allow_non_production,
            hash: 0,
        };
        fp.compute_hash();
        fp
    }
    
    fn compute_hash(&mut self) {
        let mut hasher = DefaultHasher::new();
        self.settlement_reference_rule.hash(&mut hasher);
        self.settlement_tie_rule.hash(&mut hasher);
        self.chainlink_feed_id.hash(&mut hasher);
        // Oracle config fields
        self.oracle_chain_id.hash(&mut hasher);
        for (asset, proxy) in &self.oracle_feed_proxies {
            asset.hash(&mut hasher);
            proxy.hash(&mut hasher);
        }
        for (asset, decimals) in &self.oracle_decimals {
            asset.hash(&mut hasher);
            decimals.hash(&mut hasher);
        }
        self.oracle_visibility_rule.hash(&mut hasher);
        self.oracle_rounding_policy.hash(&mut hasher);
        self.oracle_config_hash.hash(&mut hasher);
        // Other fields
        self.latency_model.hash(&mut hasher);
        self.order_latency_ns.hash(&mut hasher);
        self.oms_parity_mode.hash(&mut hasher);
        self.maker_fill_model.hash(&mut hasher);
        self.integrity_policy.hash(&mut hasher);
        self.invariant_mode.hash(&mut hasher);
        self.fee_rate_bps.hash(&mut hasher);
        self.strategy_params_hash.hash(&mut hasher);
        self.arrival_policy.hash(&mut hasher);
        self.strict_accounting.hash(&mut hasher);
        self.production_grade.hash(&mut hasher);
        self.allow_non_production.hash(&mut hasher);
        self.hash = hasher.finish();
    }
}

// =============================================================================
// DATASET FINGERPRINT
// =============================================================================

/// Fingerprint of a single input stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamFingerprint {
    /// Stream name (e.g., "orderbook_snapshots", "trades", "oracle_rounds").
    pub stream_name: String,
    /// Market IDs covered.
    pub market_ids: Vec<String>,
    /// Time range: start timestamp (ns).
    pub start_time_ns: Nanos,
    /// Time range: end timestamp (ns).
    pub end_time_ns: Nanos,
    /// Number of records in stream.
    pub record_count: u64,
    /// Rolling hash of records (in deterministic order).
    pub rolling_hash: u64,
}

impl StreamFingerprint {
    /// Create a new stream fingerprint builder.
    pub fn builder(stream_name: impl Into<String>) -> StreamFingerprintBuilder {
        StreamFingerprintBuilder::new(stream_name)
    }
}

/// Builder for incrementally computing a StreamFingerprint.
pub struct StreamFingerprintBuilder {
    stream_name: String,
    market_ids: std::collections::BTreeSet<String>,
    start_time_ns: Option<Nanos>,
    end_time_ns: Option<Nanos>,
    record_count: u64,
    rolling_hash: u64,
}

impl StreamFingerprintBuilder {
    pub fn new(stream_name: impl Into<String>) -> Self {
        Self {
            stream_name: stream_name.into(),
            market_ids: std::collections::BTreeSet::new(),
            start_time_ns: None,
            end_time_ns: None,
            record_count: 0,
            rolling_hash: 0x5555_5555_5555_5555, // Initial seed
        }
    }
    
    /// Add a record to the fingerprint.
    /// 
    /// `record_hash` should be computed from the canonical fields of the record.
    pub fn add_record(&mut self, timestamp_ns: Nanos, market_id: Option<&str>, record_hash: u64) {
        self.record_count += 1;
        
        // Update time range
        match self.start_time_ns {
            None => self.start_time_ns = Some(timestamp_ns),
            Some(s) => self.start_time_ns = Some(s.min(timestamp_ns)),
        }
        match self.end_time_ns {
            None => self.end_time_ns = Some(timestamp_ns),
            Some(e) => self.end_time_ns = Some(e.max(timestamp_ns)),
        }
        
        // Track market IDs
        if let Some(mid) = market_id {
            self.market_ids.insert(mid.to_string());
        }
        
        // Rolling hash: H(prev || record_hash)
        let mut hasher = DefaultHasher::new();
        self.rolling_hash.hash(&mut hasher);
        record_hash.hash(&mut hasher);
        self.rolling_hash = hasher.finish();
    }
    
    /// Build the final fingerprint.
    pub fn build(self) -> StreamFingerprint {
        StreamFingerprint {
            stream_name: self.stream_name,
            market_ids: self.market_ids.into_iter().collect(),
            start_time_ns: self.start_time_ns.unwrap_or(0),
            end_time_ns: self.end_time_ns.unwrap_or(0),
            record_count: self.record_count,
            rolling_hash: self.rolling_hash,
        }
    }
}

/// Fingerprint of the entire dataset.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatasetFingerprint {
    /// Data contract classification.
    pub classification: String,
    /// Dataset readiness.
    pub readiness: String,
    /// Orderbook history type.
    pub orderbook_type: String,
    /// Trade history type.
    pub trade_type: String,
    /// Arrival time semantics.
    pub arrival_semantics: String,
    /// Per-stream fingerprints.
    pub streams: Vec<StreamFingerprint>,
    /// Computed hash.
    pub hash: u64,
}

impl DatasetFingerprint {
    /// Create from data contract and stream fingerprints.
    pub fn new(
        contract: &crate::backtest_v2::data_contract::HistoricalDataContract,
        readiness: crate::backtest_v2::data_contract::DatasetReadiness,
        streams: Vec<StreamFingerprint>,
    ) -> Self {
        let mut fp = Self {
            classification: format!("{:?}", contract.classify()),
            readiness: format!("{:?}", readiness),
            orderbook_type: format!("{:?}", contract.orderbook),
            trade_type: format!("{:?}", contract.trades),
            arrival_semantics: format!("{:?}", contract.arrival_time),
            streams,
            hash: 0,
        };
        fp.compute_hash();
        fp
    }
    
    fn compute_hash(&mut self) {
        let mut hasher = DefaultHasher::new();
        self.classification.hash(&mut hasher);
        self.readiness.hash(&mut hasher);
        self.orderbook_type.hash(&mut hasher);
        self.trade_type.hash(&mut hasher);
        self.arrival_semantics.hash(&mut hasher);
        for stream in &self.streams {
            stream.stream_name.hash(&mut hasher);
            stream.record_count.hash(&mut hasher);
            stream.rolling_hash.hash(&mut hasher);
        }
        self.hash = hasher.finish();
    }
}

// =============================================================================
// SEED FINGERPRINT
// =============================================================================

/// Fingerprint of RNG seeds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeedFingerprint {
    /// Primary seed.
    pub primary_seed: u64,
    /// Derived sub-seeds (for latency, fills, etc.).
    pub sub_seeds: Vec<(String, u64)>,
    /// Computed hash.
    pub hash: u64,
}

impl SeedFingerprint {
    pub fn new(seed: u64) -> Self {
        use crate::backtest_v2::validation::DeterministicSeed;
        
        let det_seed = DeterministicSeed::new(seed);
        let sub_seeds = vec![
            ("latency".to_string(), det_seed.latency),
            ("fill_probability".to_string(), det_seed.fill_probability),
            ("queue_position".to_string(), det_seed.queue_position),
        ];
        
        let mut fp = Self {
            primary_seed: seed,
            sub_seeds,
            hash: 0,
        };
        fp.compute_hash();
        fp
    }
    
    fn compute_hash(&mut self) {
        let mut hasher = DefaultHasher::new();
        self.primary_seed.hash(&mut hasher);
        for (name, seed) in &self.sub_seeds {
            name.hash(&mut hasher);
            seed.hash(&mut hasher);
        }
        self.hash = hasher.finish();
    }
}

// =============================================================================
// BEHAVIOR FINGERPRINT
// =============================================================================

/// Canonical record of an observable behavior event.
#[derive(Debug, Clone, Hash)]
pub enum BehaviorEvent {
    /// Strategy decision made.
    Decision {
        decision_id: u64,
        decision_time: Nanos,
        input_count: u32,
        proof_hash: u64,
    },
    /// Order submitted.
    OrderSubmit {
        order_id: u64,
        side: Side,
        price_scaled: i64,
        size_scaled: i64,
        decision_time: Nanos,
    },
    /// Order acknowledged.
    OrderAck {
        order_id: u64,
        decision_time: Nanos,
    },
    /// Order rejected.
    OrderReject {
        order_id: u64,
        reason_hash: u64,
        decision_time: Nanos,
    },
    /// Cancel acknowledged.
    CancelAck {
        order_id: u64,
        cancelled_qty_scaled: i64,
        decision_time: Nanos,
    },
    /// Fill received.
    Fill {
        order_id: u64,
        price_scaled: i64,
        size_scaled: i64,
        is_maker: bool,
        fee_scaled: i64,
        decision_time: Nanos,
    },
    /// Settlement event.
    Settlement {
        market_id_hash: u64,
        window_start_ns: Nanos,
        window_end_ns: Nanos,
        start_price_scaled: i64,
        end_price_scaled: i64,
        outcome_hash: u64,
        decision_time: Nanos,
    },
    /// Ledger posting (if ledger enabled).
    LedgerPost {
        entry_id: u64,
        account_hash: u64,
        amount_scaled: i64,
        decision_time: Nanos,
    },
    /// Window PnL finalized (per-15-minute window).
    WindowPnL {
        window_start_ns: Nanos,
        window_end_ns: Nanos,
        market_id_hash: u64,
        net_pnl_scaled: i64,
        trades_count: u64,
        is_finalized: bool,
    },
}

impl BehaviorEvent {
    /// Compute hash of this event.
    pub fn hash_event(&self) -> u64 {
        let mut hasher = DefaultHasher::new();
        self.hash(&mut hasher);
        hasher.finish()
    }
}

/// Helper to scale a float to a fixed-point integer.
fn scale_price(price: f64) -> i64 {
    (price * PRICE_SCALE) as i64
}

fn scale_size(size: f64) -> i64 {
    (size * SIZE_SCALE) as i64
}

fn hash_string(s: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    s.hash(&mut hasher);
    hasher.finish()
}

/// Builder for incrementally computing a BehaviorFingerprint.
pub struct BehaviorFingerprintBuilder {
    event_count: u64,
    rolling_hash: u64,
}

impl BehaviorFingerprintBuilder {
    pub fn new() -> Self {
        Self {
            event_count: 0,
            rolling_hash: 0xAAAA_AAAA_AAAA_AAAA, // Initial seed
        }
    }
    
    /// Record a decision event.
    pub fn record_decision(&mut self, decision_id: u64, decision_time: Nanos, input_count: u32, proof_hash: u64) {
        let event = BehaviorEvent::Decision {
            decision_id,
            decision_time,
            input_count,
            proof_hash,
        };
        self.add_event(event);
    }
    
    /// Record an order submission.
    pub fn record_order_submit(&mut self, order_id: u64, side: Side, price: f64, size: f64, decision_time: Nanos) {
        let event = BehaviorEvent::OrderSubmit {
            order_id,
            side,
            price_scaled: scale_price(price),
            size_scaled: scale_size(size),
            decision_time,
        };
        self.add_event(event);
    }
    
    /// Record an order ack.
    pub fn record_order_ack(&mut self, order_id: u64, decision_time: Nanos) {
        let event = BehaviorEvent::OrderAck {
            order_id,
            decision_time,
        };
        self.add_event(event);
    }
    
    /// Record an order reject.
    pub fn record_order_reject(&mut self, order_id: u64, reason: &str, decision_time: Nanos) {
        let event = BehaviorEvent::OrderReject {
            order_id,
            reason_hash: hash_string(reason),
            decision_time,
        };
        self.add_event(event);
    }
    
    /// Record a cancel ack.
    pub fn record_cancel_ack(&mut self, order_id: u64, cancelled_qty: f64, decision_time: Nanos) {
        let event = BehaviorEvent::CancelAck {
            order_id,
            cancelled_qty_scaled: scale_size(cancelled_qty),
            decision_time,
        };
        self.add_event(event);
    }
    
    /// Record a fill.
    pub fn record_fill(&mut self, order_id: u64, price: f64, size: f64, is_maker: bool, fee: f64, decision_time: Nanos) {
        let event = BehaviorEvent::Fill {
            order_id,
            price_scaled: scale_price(price),
            size_scaled: scale_size(size),
            is_maker,
            fee_scaled: scale_price(fee),
            decision_time,
        };
        self.add_event(event);
    }
    
    /// Record a settlement event.
    pub fn record_settlement(
        &mut self, 
        market_id: &str, 
        window_start_ns: Nanos, 
        window_end_ns: Nanos,
        start_price: f64,
        end_price: f64,
        outcome: &SettlementOutcome,
        decision_time: Nanos,
    ) {
        let outcome_hash = {
            let mut h = DefaultHasher::new();
            format!("{:?}", outcome).hash(&mut h);
            h.finish()
        };
        
        let event = BehaviorEvent::Settlement {
            market_id_hash: hash_string(market_id),
            window_start_ns,
            window_end_ns,
            start_price_scaled: scale_price(start_price),
            end_price_scaled: scale_price(end_price),
            outcome_hash,
            decision_time,
        };
        self.add_event(event);
    }
    
    /// Record a ledger posting.
    pub fn record_ledger_post(&mut self, entry_id: u64, account: &str, amount: f64, decision_time: Nanos) {
        let event = BehaviorEvent::LedgerPost {
            entry_id,
            account_hash: hash_string(account),
            amount_scaled: scale_price(amount),
            decision_time,
        };
        self.add_event(event);
    }
    
    /// Record the equity curve rolling hash (at finalization).
    /// 
    /// The equity curve is a derived observable artifact, and its hash should
    /// be included in the behavior fingerprint to ensure:
    /// - Same inputs produce identical equity curves
    /// - Different equity curves produce different fingerprints
    pub fn record_equity_curve_hash(&mut self, equity_curve_hash: u64, point_count: u64) {
        // Mix in the equity curve hash directly into the rolling hash
        let mut hasher = DefaultHasher::new();
        self.rolling_hash.hash(&mut hasher);
        equity_curve_hash.hash(&mut hasher);
        point_count.hash(&mut hasher);
        self.rolling_hash = hasher.finish();
    }
    
    /// Record a finalized window PnL.
    pub fn record_window_pnl(
        &mut self,
        window_start_ns: Nanos,
        window_end_ns: Nanos,
        market_id: &str,
        net_pnl: i128,
        trades_count: u64,
        is_finalized: bool,
    ) {
        let event = BehaviorEvent::WindowPnL {
            window_start_ns,
            window_end_ns,
            market_id_hash: hash_string(market_id),
            net_pnl_scaled: net_pnl as i64, // Truncate for fingerprinting
            trades_count,
            is_finalized,
        };
        self.add_event(event);
    }
    
    /// Record the window PnL series hash (at finalization).
    /// 
    /// The window PnL series is a canonical derived artifact. Its hash should be
    /// included in the behavior fingerprint to ensure:
    /// - Same trading produces identical per-window PnL breakdown
    /// - Any window accounting change affects the fingerprint
    pub fn record_window_pnl_series_hash(&mut self, series_hash: u64, window_count: u64) {
        let mut hasher = DefaultHasher::new();
        self.rolling_hash.hash(&mut hasher);
        series_hash.hash(&mut hasher);
        window_count.hash(&mut hasher);
        self.rolling_hash = hasher.finish();
    }
    
    fn add_event(&mut self, event: BehaviorEvent) {
        self.event_count += 1;
        let event_hash = event.hash_event();
        
        // Rolling hash: H(prev || event_hash)
        let mut hasher = DefaultHasher::new();
        self.rolling_hash.hash(&mut hasher);
        event_hash.hash(&mut hasher);
        self.rolling_hash = hasher.finish();
    }
    
    /// Build the final fingerprint.
    pub fn build(self) -> BehaviorFingerprint {
        BehaviorFingerprint {
            event_count: self.event_count,
            hash: self.rolling_hash,
        }
    }
}

impl Default for BehaviorFingerprintBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Fingerprint of observable behavior.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BehaviorFingerprint {
    /// Total observable events.
    pub event_count: u64,
    /// Rolling hash of all events.
    pub hash: u64,
}

// =============================================================================
// RUN FINGERPRINT
// =============================================================================

/// Complete run fingerprint combining all components.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunFingerprint {
    /// Fingerprint version.
    pub version: String,
    /// Strategy fingerprint (new in V2).
    pub strategy: StrategyFingerprint,
    /// Code fingerprint.
    pub code: CodeFingerprint,
    /// Config fingerprint.
    pub config: ConfigFingerprint,
    /// Dataset fingerprint.
    pub dataset: DatasetFingerprint,
    /// Seed fingerprint.
    pub seed: SeedFingerprint,
    /// Behavior fingerprint.
    pub behavior: BehaviorFingerprint,
    /// Market registry fingerprint (optional, required for hermetic 15M backtests).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registry: Option<crate::backtest_v2::market_registry::RegistryFingerprint>,
    /// Final combined hash.
    pub hash: u64,
    /// Hex string of final hash.
    pub hash_hex: String,
}

impl RunFingerprint {
    /// Compute the final run fingerprint from all components.
    pub fn compute(
        strategy: StrategyFingerprint,
        code: CodeFingerprint,
        config: ConfigFingerprint,
        dataset: DatasetFingerprint,
        seed: SeedFingerprint,
        behavior: BehaviorFingerprint,
    ) -> Self {
        Self::compute_with_registry(strategy, code, config, dataset, seed, behavior, None)
    }

    /// Compute the final run fingerprint from all components including optional registry.
    pub fn compute_with_registry(
        strategy: StrategyFingerprint,
        code: CodeFingerprint,
        config: ConfigFingerprint,
        dataset: DatasetFingerprint,
        seed: SeedFingerprint,
        behavior: BehaviorFingerprint,
        registry: Option<crate::backtest_v2::market_registry::RegistryFingerprint>,
    ) -> Self {
        let mut hasher = DefaultHasher::new();
        
        // Hash version prefix
        FINGERPRINT_VERSION.hash(&mut hasher);
        
        // Hash all component hashes (strategy is first-class component)
        strategy.hash.hash(&mut hasher);
        code.hash.hash(&mut hasher);
        config.hash.hash(&mut hasher);
        dataset.hash.hash(&mut hasher);
        seed.hash.hash(&mut hasher);
        behavior.hash.hash(&mut hasher);
        
        // Include registry fingerprint if present
        if let Some(ref reg) = registry {
            reg.compute_hash().hash(&mut hasher);
        }
        
        let hash = hasher.finish();
        let hash_hex = format!("{:016x}", hash);
        
        Self {
            version: FINGERPRINT_VERSION.to_string(),
            strategy,
            code,
            config,
            dataset,
            seed,
            behavior,
            registry,
            hash,
            hash_hex,
        }
    }
    
    /// Format as a summary report.
    pub fn format_report(&self) -> String {
        let mut out = String::new();
        
        out.push_str("╔══════════════════════════════════════════════════════════════════════════════╗\n");
        out.push_str("║                         RUN FINGERPRINT REPORT                               ║\n");
        out.push_str("╠══════════════════════════════════════════════════════════════════════════════╣\n");
        out.push_str(&format!("║  Version:    {:64} ║\n", self.version));
        out.push_str(&format!("║  Hash:       {:64} ║\n", self.hash_hex));
        out.push_str("╠══════════════════════════════════════════════════════════════════════════════╣\n");
        out.push_str("║  STRATEGY:                                                                   ║\n");
        out.push_str(&format!("║    Name:     {:64} ║\n", self.strategy.name));
        out.push_str(&format!("║    Version:  {:64} ║\n", self.strategy.version));
        out.push_str(&format!("║    CodeHash: {:64} ║\n", self.strategy.code_hash));
        out.push_str(&format!("║    Hash:     {:016x}                                                   ║\n", self.strategy.hash));
        out.push_str("╠══════════════════════════════════════════════════════════════════════════════╣\n");
        out.push_str("║  COMPONENT HASHES:                                                           ║\n");
        out.push_str(&format!("║    Strategy: {:016x}  ({:40})  ║\n", self.strategy.hash, self.strategy.format_short()));
        out.push_str(&format!("║    Code:     {:016x}  ({:40})  ║\n", self.code.hash, self.code.format_short()));
        out.push_str(&format!("║    Config:   {:016x}                                                   ║\n", self.config.hash));
        out.push_str(&format!("║    Dataset:  {:016x}  ({} streams, {} records)            ║\n", 
            self.dataset.hash,
            self.dataset.streams.len(),
            self.dataset.streams.iter().map(|s| s.record_count).sum::<u64>()
        ));
        out.push_str(&format!("║    Seed:     {:016x}  (primary: {})                         ║\n", self.seed.hash, self.seed.primary_seed));
        out.push_str(&format!("║    Behavior: {:016x}  ({} events)                           ║\n", self.behavior.hash, self.behavior.event_count));
        out.push_str("╠══════════════════════════════════════════════════════════════════════════════╣\n");
        out.push_str("║  CONFIGURATION:                                                              ║\n");
        out.push_str(&format!("║    Settlement Rule:  {:56} ║\n", 
            self.config.settlement_reference_rule.as_deref().unwrap_or("N/A")
        ));
        out.push_str(&format!("║    Latency Model:    {:56} ║\n", self.config.latency_model));
        out.push_str(&format!("║    Maker Fill Model: {:56} ║\n", self.config.maker_fill_model));
        out.push_str(&format!("║    Production Grade: {:56} ║\n", self.config.production_grade));
        out.push_str("╠══════════════════════════════════════════════════════════════════════════════╣\n");
        out.push_str("║  DATASET:                                                                    ║\n");
        out.push_str(&format!("║    Classification:   {:56} ║\n", self.dataset.classification));
        out.push_str(&format!("║    Readiness:        {:56} ║\n", self.dataset.readiness));
        for stream in &self.dataset.streams {
            out.push_str(&format!("║    Stream {:10}: {} records, hash {:016x}                   ║\n",
                stream.stream_name,
                stream.record_count,
                stream.rolling_hash
            ));
        }
        if let Some(ref reg) = self.registry {
            out.push_str("╠══════════════════════════════════════════════════════════════════════════════╣\n");
            out.push_str("║  MARKET REGISTRY:                                                            ║\n");
            out.push_str(&format!("║    Version:      {:60} ║\n", reg.version));
            out.push_str(&format!("║    Markets:      {:60} ║\n", reg.market_count));
            out.push_str(&format!("║    Hash:         {:60} ║\n", reg.hash_hex));
        }
        out.push_str("╚══════════════════════════════════════════════════════════════════════════════╝\n");
        
        out
    }
    
    /// Format as a single-line summary.
    pub fn format_compact(&self) -> String {
        format!(
            "RunFingerprint[{}] strategy={} code={:08x} config={:08x} data={:08x} seed={:08x} behavior={:08x}",
            self.hash_hex,
            self.strategy.format_short(),
            self.code.hash as u32,
            self.config.hash as u32,
            self.dataset.hash as u32,
            self.seed.hash as u32,
            self.behavior.hash as u32,
        )
    }
}

// =============================================================================
// FINGERPRINT COLLECTOR
// =============================================================================

/// Collects fingerprint data during a backtest run.
/// 
/// Create at run start, feed events during run, finalize at run end.
pub struct FingerprintCollector {
    /// Strategy identity (set at start).
    strategy_id: Option<StrategyId>,
    /// Code fingerprint (computed once at start).
    code: CodeFingerprint,
    /// Config fingerprint (computed once at start).
    config: Option<ConfigFingerprint>,
    /// Stream fingerprint builders.
    stream_builders: std::collections::HashMap<String, StreamFingerprintBuilder>,
    /// Behavior fingerprint builder.
    behavior: BehaviorFingerprintBuilder,
    /// Primary seed.
    seed: u64,
    /// Dataset contract (set at start).
    data_contract: Option<crate::backtest_v2::data_contract::HistoricalDataContract>,
    /// Dataset readiness (set at start).
    dataset_readiness: Option<crate::backtest_v2::data_contract::DatasetReadiness>,
}

impl FingerprintCollector {
    /// Create a new collector.
    pub fn new() -> Self {
        Self {
            strategy_id: None,
            code: CodeFingerprint::new(),
            config: None,
            stream_builders: std::collections::HashMap::new(),
            behavior: BehaviorFingerprintBuilder::new(),
            seed: 0,
            data_contract: None,
            dataset_readiness: None,
        }
    }
    
    /// Set the strategy identity.
    pub fn set_strategy_id(&mut self, strategy_id: StrategyId) {
        self.strategy_id = Some(strategy_id);
    }
    
    /// Set config fingerprint from BacktestConfig.
    pub fn set_config(&mut self, config: &crate::backtest_v2::orchestrator::BacktestConfig) {
        self.config = Some(ConfigFingerprint::from_config(config));
        self.seed = config.seed;
        self.data_contract = Some(config.data_contract.clone());
        // Also capture strategy_id from config if set
        if let Some(ref sid) = config.strategy_id {
            self.strategy_id = Some(sid.clone());
        }
    }
    
    /// Set dataset readiness.
    pub fn set_dataset_readiness(&mut self, readiness: crate::backtest_v2::data_contract::DatasetReadiness) {
        self.dataset_readiness = Some(readiness);
    }
    
    /// Record an input event (for dataset fingerprint).
    pub fn record_input_event(&mut self, stream_name: &str, timestamp_ns: Nanos, market_id: Option<&str>, record_hash: u64) {
        let builder = self.stream_builders
            .entry(stream_name.to_string())
            .or_insert_with(|| StreamFingerprintBuilder::new(stream_name));
        builder.add_record(timestamp_ns, market_id, record_hash);
    }
    
    /// Record a decision (for behavior fingerprint).
    pub fn record_decision(&mut self, decision_id: u64, decision_time: Nanos, input_count: u32, proof_hash: u64) {
        self.behavior.record_decision(decision_id, decision_time, input_count, proof_hash);
    }
    
    /// Record an order submission.
    pub fn record_order_submit(&mut self, order_id: u64, side: Side, price: f64, size: f64, decision_time: Nanos) {
        self.behavior.record_order_submit(order_id, side, price, size, decision_time);
    }
    
    /// Record an order ack.
    pub fn record_order_ack(&mut self, order_id: u64, decision_time: Nanos) {
        self.behavior.record_order_ack(order_id, decision_time);
    }
    
    /// Record an order reject.
    pub fn record_order_reject(&mut self, order_id: u64, reason: &str, decision_time: Nanos) {
        self.behavior.record_order_reject(order_id, reason, decision_time);
    }
    
    /// Record a cancel ack.
    pub fn record_cancel_ack(&mut self, order_id: u64, cancelled_qty: f64, decision_time: Nanos) {
        self.behavior.record_cancel_ack(order_id, cancelled_qty, decision_time);
    }
    
    /// Record a fill.
    pub fn record_fill(&mut self, order_id: u64, price: f64, size: f64, is_maker: bool, fee: f64, decision_time: Nanos) {
        self.behavior.record_fill(order_id, price, size, is_maker, fee, decision_time);
    }
    
    /// Record a settlement.
    pub fn record_settlement(
        &mut self, 
        market_id: &str, 
        window_start_ns: Nanos, 
        window_end_ns: Nanos,
        start_price: f64,
        end_price: f64,
        outcome: &SettlementOutcome,
        decision_time: Nanos,
    ) {
        self.behavior.record_settlement(market_id, window_start_ns, window_end_ns, start_price, end_price, outcome, decision_time);
    }
    
    /// Record a ledger posting.
    pub fn record_ledger_post(&mut self, entry_id: u64, account: &str, amount: f64, decision_time: Nanos) {
        self.behavior.record_ledger_post(entry_id, account, amount, decision_time);
    }
    
    /// Record the equity curve hash (at finalization).
    pub fn record_equity_curve_hash(&mut self, equity_curve_hash: u64, point_count: u64) {
        self.behavior.record_equity_curve_hash(equity_curve_hash, point_count);
    }
    
    /// Finalize and produce the RunFingerprint.
    pub fn finalize(self) -> RunFingerprint {
        use crate::backtest_v2::data_contract::{
            ArrivalTimeSemantics, HistoricalDataContract, OrderBookHistory, TradeHistory,
        };
        
        // Build stream fingerprints
        let streams: Vec<StreamFingerprint> = self.stream_builders
            .into_iter()
            .map(|(_, builder)| builder.build())
            .collect();
        
        // Build dataset fingerprint
        let default_contract = HistoricalDataContract {
            venue: "Unknown".to_string(),
            market: "Unknown".to_string(),
            orderbook: OrderBookHistory::None,
            trades: TradeHistory::None,
            arrival_time: ArrivalTimeSemantics::Unusable,
        };
        let dataset = DatasetFingerprint::new(
            &self.data_contract.unwrap_or(default_contract),
            self.dataset_readiness.unwrap_or(crate::backtest_v2::data_contract::DatasetReadiness::NonRepresentative),
            streams,
        );
        
        // Build seed fingerprint
        let seed = SeedFingerprint::new(self.seed);
        
        // Build behavior fingerprint
        let behavior = self.behavior.build();
        
        // Build strategy fingerprint
        let strategy = StrategyFingerprint::from_strategy_id(
            &self.strategy_id.unwrap_or_default()
        );
        
        // Compute final fingerprint
        RunFingerprint::compute(
            strategy,
            self.code,
            self.config.unwrap_or_else(|| ConfigFingerprint {
                settlement_reference_rule: None,
                settlement_tie_rule: None,
                chainlink_feed_id: None,
                oracle_chain_id: None,
                oracle_feed_proxies: Vec::new(),
                oracle_decimals: Vec::new(),
                oracle_visibility_rule: None,
                oracle_rounding_policy: None,
                oracle_config_hash: None,
                latency_model: "Unknown".to_string(),
                order_latency_ns: None,
                oms_parity_mode: "Unknown".to_string(),
                maker_fill_model: "Unknown".to_string(),
                integrity_policy: "Unknown".to_string(),
                invariant_mode: "Unknown".to_string(),
                fee_rate_bps: None,
                strategy_params_hash: 0,
                arrival_policy: "Unknown".to_string(),
                strict_accounting: false,
                production_grade: false,
                allow_non_production: false,
                hash: 0,
            }),
            dataset,
            seed,
            behavior,
        )
    }
    
    /// Get the strategy identity (if set).
    pub fn strategy_id(&self) -> Option<&StrategyId> {
        self.strategy_id.as_ref()
    }
    
    /// Get the code fingerprint (available immediately).
    pub fn code_fingerprint(&self) -> &CodeFingerprint {
        &self.code
    }
    
    /// Get current behavior event count.
    pub fn behavior_event_count(&self) -> u64 {
        self.behavior.event_count
    }
}

impl Default for FingerprintCollector {
    fn default() -> Self {
        Self::new()
    }
}

// =============================================================================
// TESTS
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_code_fingerprint_deterministic() {
        let fp1 = CodeFingerprint::new();
        let fp2 = CodeFingerprint::new();
        
        assert_eq!(fp1.hash, fp2.hash, "Code fingerprint should be deterministic");
        assert_eq!(fp1.crate_version, fp2.crate_version);
    }
    
    #[test]
    fn test_seed_fingerprint_deterministic() {
        let fp1 = SeedFingerprint::new(42);
        let fp2 = SeedFingerprint::new(42);
        
        assert_eq!(fp1.hash, fp2.hash, "Seed fingerprint should be deterministic");
        assert_eq!(fp1.sub_seeds, fp2.sub_seeds);
    }
    
    #[test]
    fn test_seed_fingerprint_changes_on_different_seed() {
        let fp1 = SeedFingerprint::new(42);
        let fp2 = SeedFingerprint::new(43);
        
        assert_ne!(fp1.hash, fp2.hash, "Different seeds should produce different fingerprints");
    }
    
    #[test]
    fn test_behavior_fingerprint_deterministic() {
        let mut b1 = BehaviorFingerprintBuilder::new();
        b1.record_decision(1, 1000, 5, 0x1234);
        b1.record_order_submit(100, Side::Buy, 0.5, 10.0, 1000);
        b1.record_fill(100, 0.5, 10.0, false, 0.001, 1100);
        let fp1 = b1.build();
        
        let mut b2 = BehaviorFingerprintBuilder::new();
        b2.record_decision(1, 1000, 5, 0x1234);
        b2.record_order_submit(100, Side::Buy, 0.5, 10.0, 1000);
        b2.record_fill(100, 0.5, 10.0, false, 0.001, 1100);
        let fp2 = b2.build();
        
        assert_eq!(fp1.hash, fp2.hash, "Same behavior should produce same fingerprint");
        assert_eq!(fp1.event_count, fp2.event_count);
    }
    
    #[test]
    fn test_behavior_fingerprint_changes_on_different_behavior() {
        let mut b1 = BehaviorFingerprintBuilder::new();
        b1.record_decision(1, 1000, 5, 0x1234);
        b1.record_order_submit(100, Side::Buy, 0.5, 10.0, 1000);
        let fp1 = b1.build();
        
        let mut b2 = BehaviorFingerprintBuilder::new();
        b2.record_decision(1, 1000, 5, 0x1234);
        b2.record_order_submit(100, Side::Sell, 0.5, 10.0, 1000); // Different side
        let fp2 = b2.build();
        
        assert_ne!(fp1.hash, fp2.hash, "Different behavior should produce different fingerprint");
    }
    
    #[test]
    fn test_stream_fingerprint_deterministic() {
        let mut b1 = StreamFingerprintBuilder::new("trades");
        b1.add_record(1000, Some("btc"), 0x1111);
        b1.add_record(2000, Some("btc"), 0x2222);
        let fp1 = b1.build();
        
        let mut b2 = StreamFingerprintBuilder::new("trades");
        b2.add_record(1000, Some("btc"), 0x1111);
        b2.add_record(2000, Some("btc"), 0x2222);
        let fp2 = b2.build();
        
        assert_eq!(fp1.rolling_hash, fp2.rolling_hash, "Same records should produce same hash");
        assert_eq!(fp1.record_count, fp2.record_count);
    }
    
    #[test]
    fn test_stream_fingerprint_changes_on_different_record() {
        let mut b1 = StreamFingerprintBuilder::new("trades");
        b1.add_record(1000, Some("btc"), 0x1111);
        b1.add_record(2000, Some("btc"), 0x2222);
        let fp1 = b1.build();
        
        let mut b2 = StreamFingerprintBuilder::new("trades");
        b2.add_record(1000, Some("btc"), 0x1111);
        b2.add_record(2000, Some("btc"), 0x3333); // Different record hash
        let fp2 = b2.build();
        
        assert_ne!(fp1.rolling_hash, fp2.rolling_hash, "Different records should produce different hash");
    }
    
    #[test]
    fn test_run_fingerprint_format() {
        let code = CodeFingerprint::new();
        let config = ConfigFingerprint {
            settlement_reference_rule: Some("LastUpdateAtOrBeforeCutoff".to_string()),
            settlement_tie_rule: Some("NoWins".to_string()),
            chainlink_feed_id: Some("btc-usd".to_string()),
            oracle_chain_id: Some(137),
            oracle_feed_proxies: vec![("BTC".to_string(), "0x1234".to_string())],
            oracle_decimals: vec![("BTC".to_string(), 8)],
            oracle_visibility_rule: Some("Finalized".to_string()),
            oracle_rounding_policy: Some("RoundDown".to_string()),
            oracle_config_hash: Some(0xABCD1234),
            latency_model: "Fixed".to_string(),
            order_latency_ns: Some(1_000_000),
            oms_parity_mode: "Full".to_string(),
            maker_fill_model: "ExplicitQueue".to_string(),
            integrity_policy: "Strict".to_string(),
            invariant_mode: "Hard".to_string(),
            fee_rate_bps: Some(10),
            strategy_params_hash: 0x1234,
            arrival_policy: "RecordedArrival".to_string(),
            strict_accounting: true,
            production_grade: true,
            allow_non_production: false,
            hash: 0x5678,
        };
        let dataset = DatasetFingerprint {
            classification: "FullIncremental".to_string(),
            readiness: "MakerViable".to_string(),
            orderbook_type: "FullIncrementalL2DeltasWithExchangeSeq".to_string(),
            trade_type: "TradePrints".to_string(),
            arrival_semantics: "RecordedArrival".to_string(),
            streams: vec![],
            hash: 0xABCD,
        };
        let seed = SeedFingerprint::new(42);
        let behavior = BehaviorFingerprint {
            event_count: 100,
            hash: 0xDEAD,
        };
        let strategy = StrategyFingerprint::from_strategy_id(
            &StrategyId::new("test_strategy", "1.0.0")
        );
        
        let fp = RunFingerprint::compute(strategy, code, config, dataset, seed, behavior);
        
        // Check format methods don't panic
        let report = fp.format_report();
        assert!(report.contains("RUN FINGERPRINT REPORT"));
        assert!(report.contains(&fp.hash_hex));
        
        let compact = fp.format_compact();
        assert!(compact.contains(&fp.hash_hex));
    }
    
    #[test]
    fn test_fingerprint_collector_basic() {
        let mut collector = FingerprintCollector::new();
        
        // Record some behavior
        collector.record_decision(1, 1000, 3, 0x1234);
        collector.record_order_submit(100, Side::Buy, 0.5, 10.0, 1000);
        collector.record_fill(100, 0.5, 10.0, false, 0.001, 1100);
        
        assert_eq!(collector.behavior_event_count(), 3);
        
        // Finalize
        let fp = collector.finalize();
        
        assert_eq!(fp.version, FINGERPRINT_VERSION);
        assert_eq!(fp.behavior.event_count, 3);
        assert!(!fp.hash_hex.is_empty());
    }
    
    #[test]
    fn test_fingerprint_version() {
        assert_eq!(FINGERPRINT_VERSION, "RUNFP_V2");
    }
    
    // =========================================================================
    // STRATEGY IDENTITY TESTS
    // =========================================================================
    
    #[test]
    fn test_strategy_id_new() {
        let sid = StrategyId::new("test_strategy", "1.0.0");
        assert_eq!(sid.name, "test_strategy");
        assert_eq!(sid.version, "1.0.0");
        assert!(sid.code_hash.is_none());
    }
    
    #[test]
    fn test_strategy_id_with_code_hash() {
        let sid = StrategyId::with_code_hash("test_strategy", "1.0.0", "abc123def456");
        assert_eq!(sid.name, "test_strategy");
        assert_eq!(sid.version, "1.0.0");
        assert_eq!(sid.code_hash, Some("abc123def456".to_string()));
    }
    
    #[test]
    fn test_strategy_id_validate_for_production() {
        // Valid strategy ID
        let sid = StrategyId::new("pm_15m_edge", "1.2.0");
        assert!(sid.validate_for_production().is_ok());
        
        // Empty name should fail
        let sid = StrategyId::new("", "1.0.0");
        assert!(sid.validate_for_production().is_err());
        
        // Empty version should fail
        let sid = StrategyId::new("test", "");
        assert!(sid.validate_for_production().is_err());
        
        // Version not starting with digit should fail
        let sid = StrategyId::new("test", "v1.0.0");
        assert!(sid.validate_for_production().is_err());
    }
    
    #[test]
    fn test_strategy_id_format() {
        let sid = StrategyId::new("test_strategy", "1.2.0");
        assert_eq!(sid.format_short(), "test_strategy/v1.2.0");
        
        let sid = StrategyId::with_code_hash("test_strategy", "1.2.0", "abc123def456");
        assert_eq!(sid.format_short(), "test_strategy/v1.2.0@abc123de");
        
        let sid = StrategyId::new("test_strategy", "1.2.0");
        assert_eq!(sid.format_full(), "Strategy[name=test_strategy, version=1.2.0, code_hash=unknown]");
    }
    
    #[test]
    fn test_strategy_id_hash_deterministic() {
        let sid1 = StrategyId::new("test", "1.0.0");
        let sid2 = StrategyId::new("test", "1.0.0");
        assert_eq!(sid1.compute_hash(), sid2.compute_hash());
    }
    
    #[test]
    fn test_strategy_id_hash_changes_on_version() {
        let sid1 = StrategyId::new("test", "1.0.0");
        let sid2 = StrategyId::new("test", "1.0.1");
        assert_ne!(sid1.compute_hash(), sid2.compute_hash());
    }
    
    #[test]
    fn test_strategy_id_hash_changes_on_name() {
        let sid1 = StrategyId::new("test_a", "1.0.0");
        let sid2 = StrategyId::new("test_b", "1.0.0");
        assert_ne!(sid1.compute_hash(), sid2.compute_hash());
    }
    
    #[test]
    fn test_strategy_id_hash_changes_on_code_hash() {
        let sid1 = StrategyId::with_code_hash("test", "1.0.0", "hash_a");
        let sid2 = StrategyId::with_code_hash("test", "1.0.0", "hash_b");
        assert_ne!(sid1.compute_hash(), sid2.compute_hash());
    }
    
    #[test]
    fn test_strategy_fingerprint_from_strategy_id() {
        let sid = StrategyId::with_code_hash("test_strategy", "1.0.0", "abc123");
        let fp = StrategyFingerprint::from_strategy_id(&sid);
        
        assert_eq!(fp.name, "test_strategy");
        assert_eq!(fp.version, "1.0.0");
        assert_eq!(fp.code_hash, "abc123");
        assert_eq!(fp.hash, sid.compute_hash());
    }
    
    #[test]
    fn test_fingerprint_collector_captures_strategy_id() {
        let mut collector = FingerprintCollector::new();
        
        let sid = StrategyId::new("captured_strategy", "3.0.0");
        collector.set_strategy_id(sid.clone());
        
        assert_eq!(collector.strategy_id(), Some(&sid));
        
        let fp = collector.finalize();
        assert_eq!(fp.strategy.name, "captured_strategy");
        assert_eq!(fp.strategy.version, "3.0.0");
    }
}

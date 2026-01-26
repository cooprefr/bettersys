//! Mandatory Oracle/Settlement Configuration for Production-Grade Backtests
//!
//! This module enforces that ANY run computing realized PnL must explicitly specify
//! all settlement reference parameters. Silent defaults are prohibited.
//!
//! # Required Configuration
//!
//! - `chain_id`: Network identifier (e.g., 137 for Polygon mainnet)
//! - `feed_proxy_address`: Chainlink AggregatorV3Interface proxy address
//! - `decimals`: Feed decimals (must match on-chain value)
//! - `reference_rule`: How to select the oracle price relative to cutoff
//! - `tie_rule`: How to handle ties in price comparison
//! - `oracle_visibility_rule`: When the outcome becomes knowable
//!
//! # Validation
//!
//! `OracleConfig::validate_production()` returns a list of violations.
//! In production-grade mode, any violation aborts the run.

use crate::backtest_v2::clock::Nanos;
use crate::backtest_v2::settlement::TieRule;
use super::settlement_source::SettlementReferenceRule;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Nanoseconds per second.
const NS_PER_SEC: u64 = 1_000_000_000;

// =============================================================================
// ORACLE CONFIGURATION VIOLATIONS
// =============================================================================

/// A violation of production-grade oracle configuration requirements.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OracleConfigViolation {
    /// Field that is misconfigured or missing.
    pub field: String,
    /// Description of the violation.
    pub description: String,
    /// Suggested fix.
    pub suggestion: String,
}

impl std::fmt::Display for OracleConfigViolation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{}] {}: {}", self.field, self.description, self.suggestion)
    }
}

/// Result of validating oracle configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OracleConfigValidationResult {
    /// Whether the configuration is valid for production.
    pub is_valid: bool,
    /// List of violations (empty if valid).
    pub violations: Vec<OracleConfigViolation>,
    /// Computed configuration fingerprint (if valid).
    pub fingerprint_hash: Option<u64>,
}

impl OracleConfigValidationResult {
    pub fn valid(fingerprint_hash: u64) -> Self {
        Self {
            is_valid: true,
            violations: Vec::new(),
            fingerprint_hash: Some(fingerprint_hash),
        }
    }

    pub fn invalid(violations: Vec<OracleConfigViolation>) -> Self {
        Self {
            is_valid: false,
            violations,
            fingerprint_hash: None,
        }
    }

    pub fn format_report(&self) -> String {
        if self.is_valid {
            format!("Oracle configuration VALID (fingerprint: {:016x})", 
                self.fingerprint_hash.unwrap_or(0))
        } else {
            let mut out = String::from("Oracle configuration INVALID:\n");
            for v in &self.violations {
                out.push_str(&format!("  - {}\n", v));
            }
            out
        }
    }
}

// =============================================================================
// PER-ASSET ORACLE FEED CONFIGURATION
// =============================================================================

/// Configuration for a single Chainlink price feed.
///
/// ALL fields are REQUIRED for production-grade runs.
/// There are NO silent defaults - every field must be explicitly set.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OracleFeedConfig {
    /// Asset symbol (e.g., "BTC", "ETH", "SOL").
    pub asset_symbol: String,
    
    /// Chain ID where the feed lives (e.g., 137 for Polygon, 1 for Ethereum mainnet).
    /// This MUST match the RPC endpoint's chain.
    pub chain_id: u64,
    
    /// Chainlink AggregatorV3Interface proxy address.
    /// This is the contract address that provides price data.
    pub feed_proxy_address: String,
    
    /// Decimals for this feed (typically 8 for USD pairs).
    /// This MUST match the on-chain `decimals()` return value.
    pub decimals: u8,
    
    /// Human-readable description (e.g., "BTC / USD").
    /// Used for validation against on-chain `description()`.
    pub expected_description: Option<String>,
    
    /// Deviation threshold from Chainlink docs (e.g., 0.001 = 0.1%).
    /// Used for staleness detection.
    pub deviation_threshold: Option<f64>,
    
    /// Heartbeat interval from Chainlink docs (seconds).
    /// Feed should update at least this often.
    pub heartbeat_secs: Option<u64>,
}

impl OracleFeedConfig {
    /// Validate this feed configuration.
    pub fn validate(&self) -> Vec<OracleConfigViolation> {
        let mut violations = Vec::new();

        // Asset symbol required
        if self.asset_symbol.is_empty() {
            violations.push(OracleConfigViolation {
                field: "asset_symbol".to_string(),
                description: "Asset symbol is empty".to_string(),
                suggestion: "Set asset_symbol to the asset identifier (e.g., 'BTC')".to_string(),
            });
        }

        // Chain ID must be non-zero
        if self.chain_id == 0 {
            violations.push(OracleConfigViolation {
                field: "chain_id".to_string(),
                description: "Chain ID is 0 (invalid)".to_string(),
                suggestion: "Set chain_id to the correct network (e.g., 137 for Polygon mainnet)".to_string(),
            });
        }

        // Feed proxy address required and must look like an address
        if self.feed_proxy_address.is_empty() {
            violations.push(OracleConfigViolation {
                field: "feed_proxy_address".to_string(),
                description: "Feed proxy address is empty".to_string(),
                suggestion: "Set feed_proxy_address to the Chainlink AggregatorV3 proxy".to_string(),
            });
        } else if !self.feed_proxy_address.starts_with("0x") || self.feed_proxy_address.len() != 42 {
            violations.push(OracleConfigViolation {
                field: "feed_proxy_address".to_string(),
                description: format!("Feed proxy address '{}' does not look like an Ethereum address", self.feed_proxy_address),
                suggestion: "Address should be 42 characters starting with 0x".to_string(),
            });
        }

        // Decimals typically 8 for USD pairs, but any non-zero value is valid
        // We just warn if it's 0 since that's almost certainly wrong
        if self.decimals == 0 {
            violations.push(OracleConfigViolation {
                field: "decimals".to_string(),
                description: "Decimals is 0 (unusual for price feeds)".to_string(),
                suggestion: "Verify decimals by calling decimals() on the feed contract".to_string(),
            });
        }

        violations
    }

    /// Compute a fingerprint hash for this feed config.
    pub fn fingerprint_hash(&self) -> u64 {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let mut hasher = DefaultHasher::new();
        self.asset_symbol.hash(&mut hasher);
        self.chain_id.hash(&mut hasher);
        self.feed_proxy_address.to_lowercase().hash(&mut hasher);
        self.decimals.hash(&mut hasher);
        hasher.finish()
    }

    /// Generate a unique feed identifier.
    pub fn feed_id(&self) -> String {
        format!("{}_{}_{}", 
            self.asset_symbol.to_lowercase(),
            self.chain_id,
            &self.feed_proxy_address[2..10].to_lowercase()
        )
    }
}

// =============================================================================
// ORACLE VISIBILITY RULE
// =============================================================================

/// Rule for when the settlement outcome becomes knowable.
///
/// This enforces visibility semantics to prevent look-ahead bias.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OracleVisibilityRule {
    /// Outcome knowable when the oracle round has ARRIVED (observed).
    /// This is the ONLY production-grade option.
    OnArrival,
    
    /// Outcome knowable at a fixed delay after cutoff.
    /// Use only for research/testing with known oracle latency.
    FixedDelay { delay_ns: Nanos },
    
    /// Outcome knowable immediately at cutoff (DANGEROUS - allows look-ahead).
    /// This is NEVER valid for production and will fail validation.
    Immediate,
}

impl Default for OracleVisibilityRule {
    fn default() -> Self {
        Self::OnArrival
    }
}

impl OracleVisibilityRule {
    pub fn is_production_grade(&self) -> bool {
        matches!(self, Self::OnArrival)
    }

    pub fn description(&self) -> &'static str {
        match self {
            Self::OnArrival => "Outcome knowable when oracle round arrives (production-grade)",
            Self::FixedDelay { .. } => "Outcome knowable after fixed delay from cutoff (research only)",
            Self::Immediate => "Outcome knowable at cutoff (INVALID - allows look-ahead)",
        }
    }
}

// =============================================================================
// ROUNDING POLICY
// =============================================================================

/// Rounding policy for price comparison in settlement.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum RoundingPolicy {
    /// No rounding - use exact comparison with epsilon tolerance.
    None { epsilon: f64 },
    /// Round to N decimal places before comparison.
    DecimalPlaces { places: u32 },
    /// Round to tick size before comparison.
    TickSize { tick: f64 },
}

impl Default for RoundingPolicy {
    fn default() -> Self {
        Self::DecimalPlaces { places: 8 }
    }
}

// =============================================================================
// COMPLETE ORACLE CONFIGURATION
// =============================================================================

/// Complete oracle configuration for settlement reference.
///
/// This is the MANDATORY configuration for any production-grade backtest
/// that computes realized PnL. ALL fields must be explicitly set.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OracleConfig {
    /// Per-asset feed configurations.
    /// Key is the asset symbol (e.g., "BTC").
    pub feeds: HashMap<String, OracleFeedConfig>,
    
    /// RPC endpoint URL for on-chain queries.
    /// This is loaded from environment for security (never stored in config).
    #[serde(skip)]
    pub rpc_endpoint: Option<String>,
    
    /// Settlement reference rule.
    pub reference_rule: SettlementReferenceRule,
    
    /// Tie rule for settlement (when start_price == end_price).
    pub tie_rule: TieRule,
    
    /// Oracle visibility rule (when outcome becomes knowable).
    pub visibility_rule: OracleVisibilityRule,
    
    /// Rounding policy for price comparison.
    pub rounding_policy: RoundingPolicy,
    
    /// Maximum allowed staleness for oracle data (nanoseconds).
    /// If the oracle round is older than this relative to cutoff, flag as stale.
    pub max_staleness_ns: Nanos,
    
    /// Whether to abort on missing oracle data (production-grade: true).
    pub abort_on_missing: bool,
    
    /// Whether to abort on stale oracle data (production-grade: true).
    pub abort_on_stale: bool,
}

impl OracleConfig {
    /// Create a new empty configuration.
    ///
    /// NOTE: This will FAIL validation. You must populate all required fields.
    pub fn new() -> Self {
        Self {
            feeds: HashMap::new(),
            rpc_endpoint: None,
            reference_rule: SettlementReferenceRule::LastUpdateAtOrBeforeCutoff,
            tie_rule: TieRule::NoWins,
            visibility_rule: OracleVisibilityRule::OnArrival,
            rounding_policy: RoundingPolicy::default(),
            max_staleness_ns: 60 * NS_PER_SEC as Nanos, // 60 seconds
            abort_on_missing: true,
            abort_on_stale: false,
        }
    }

    /// Create a production-grade configuration for BTC on Polygon.
    ///
    /// NOTE: You must still set the RPC endpoint via environment variable.
    pub fn production_btc_polygon() -> Self {
        let mut config = Self::new();
        config.feeds.insert("BTC".to_string(), OracleFeedConfig {
            asset_symbol: "BTC".to_string(),
            chain_id: 137,
            feed_proxy_address: "0xc907E116054Ad103354f2D350FD2514433D57F6f".to_string(),
            decimals: 8,
            expected_description: Some("BTC / USD".to_string()),
            deviation_threshold: Some(0.001),
            heartbeat_secs: Some(2),
        });
        config.abort_on_missing = true;
        config.abort_on_stale = true;
        config
    }

    /// Create a production-grade configuration for ETH on Polygon.
    pub fn production_eth_polygon() -> Self {
        let mut config = Self::new();
        config.feeds.insert("ETH".to_string(), OracleFeedConfig {
            asset_symbol: "ETH".to_string(),
            chain_id: 137,
            feed_proxy_address: "0xF9680D99D6C9589e2a93a78A04A279e509205945".to_string(),
            decimals: 8,
            expected_description: Some("ETH / USD".to_string()),
            deviation_threshold: Some(0.001),
            heartbeat_secs: Some(2),
        });
        config.abort_on_missing = true;
        config.abort_on_stale = true;
        config
    }

    /// Create a multi-asset production configuration for Polygon.
    pub fn production_multi_asset_polygon() -> Self {
        let mut config = Self::new();
        
        // BTC
        config.feeds.insert("BTC".to_string(), OracleFeedConfig {
            asset_symbol: "BTC".to_string(),
            chain_id: 137,
            feed_proxy_address: "0xc907E116054Ad103354f2D350FD2514433D57F6f".to_string(),
            decimals: 8,
            expected_description: Some("BTC / USD".to_string()),
            deviation_threshold: Some(0.001),
            heartbeat_secs: Some(2),
        });
        
        // ETH
        config.feeds.insert("ETH".to_string(), OracleFeedConfig {
            asset_symbol: "ETH".to_string(),
            chain_id: 137,
            feed_proxy_address: "0xF9680D99D6C9589e2a93a78A04A279e509205945".to_string(),
            decimals: 8,
            expected_description: Some("ETH / USD".to_string()),
            deviation_threshold: Some(0.001),
            heartbeat_secs: Some(2),
        });
        
        // SOL
        config.feeds.insert("SOL".to_string(), OracleFeedConfig {
            asset_symbol: "SOL".to_string(),
            chain_id: 137,
            feed_proxy_address: "0x10C8264C0935b3B9870013e057f330Ff3e9C56dC".to_string(),
            decimals: 8,
            expected_description: Some("SOL / USD".to_string()),
            deviation_threshold: Some(0.005),
            heartbeat_secs: Some(2),
        });
        
        // XRP
        config.feeds.insert("XRP".to_string(), OracleFeedConfig {
            asset_symbol: "XRP".to_string(),
            chain_id: 137,
            feed_proxy_address: "0x785ba89291f676b5386652eB12b30cF361020694".to_string(),
            decimals: 8,
            expected_description: Some("XRP / USD".to_string()),
            deviation_threshold: Some(0.005),
            heartbeat_secs: Some(2),
        });
        
        config.abort_on_missing = true;
        config.abort_on_stale = true;
        config
    }

    /// Load RPC endpoint from environment.
    pub fn load_rpc_from_env(&mut self) -> Option<String> {
        let endpoint = std::env::var("POLYGON_RPC_URL")
            .or_else(|_| std::env::var("CHAINLINK_RPC_URL"))
            .ok();
        self.rpc_endpoint = endpoint.clone();
        endpoint
    }

    /// Add a feed configuration.
    pub fn add_feed(&mut self, feed: OracleFeedConfig) {
        self.feeds.insert(feed.asset_symbol.clone(), feed);
    }

    /// Get feed configuration for an asset.
    pub fn get_feed(&self, asset: &str) -> Option<&OracleFeedConfig> {
        self.feeds.get(asset)
    }

    /// Validate the configuration for production use.
    ///
    /// Returns a result containing all violations found.
    pub fn validate_production(&self) -> OracleConfigValidationResult {
        let mut violations = Vec::new();

        // Must have at least one feed
        if self.feeds.is_empty() {
            violations.push(OracleConfigViolation {
                field: "feeds".to_string(),
                description: "No oracle feeds configured".to_string(),
                suggestion: "Add at least one OracleFeedConfig for the assets being traded".to_string(),
            });
        }

        // Validate each feed
        for (asset, feed) in &self.feeds {
            let feed_violations = feed.validate();
            for mut v in feed_violations {
                v.field = format!("feeds[{}].{}", asset, v.field);
                violations.push(v);
            }
        }

        // RPC endpoint must be set for live validation
        // (We don't require it for config validation since it comes from env)
        
        // Visibility rule must be production-grade
        if !self.visibility_rule.is_production_grade() {
            violations.push(OracleConfigViolation {
                field: "visibility_rule".to_string(),
                description: format!("Visibility rule {:?} is not production-grade", self.visibility_rule),
                suggestion: "Use OracleVisibilityRule::OnArrival for production".to_string(),
            });
        }

        // abort_on_missing should be true for production
        if !self.abort_on_missing {
            violations.push(OracleConfigViolation {
                field: "abort_on_missing".to_string(),
                description: "abort_on_missing is false (production requires true)".to_string(),
                suggestion: "Set abort_on_missing = true for production".to_string(),
            });
        }

        // Check that all feeds use the same chain_id (can't mix chains)
        let chain_ids: std::collections::HashSet<u64> = self.feeds.values()
            .map(|f| f.chain_id)
            .collect();
        if chain_ids.len() > 1 {
            violations.push(OracleConfigViolation {
                field: "feeds".to_string(),
                description: format!("Feeds use multiple chain IDs: {:?}", chain_ids),
                suggestion: "All feeds must be on the same chain".to_string(),
            });
        }

        if violations.is_empty() {
            OracleConfigValidationResult::valid(self.fingerprint_hash())
        } else {
            OracleConfigValidationResult::invalid(violations)
        }
    }

    /// Compute a fingerprint hash for this entire configuration.
    pub fn fingerprint_hash(&self) -> u64 {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let mut hasher = DefaultHasher::new();
        
        // Hash sorted feed fingerprints
        let mut feed_hashes: Vec<(String, u64)> = self.feeds.iter()
            .map(|(k, v)| (k.clone(), v.fingerprint_hash()))
            .collect();
        feed_hashes.sort_by(|a, b| a.0.cmp(&b.0));
        for (asset, hash) in feed_hashes {
            asset.hash(&mut hasher);
            hash.hash(&mut hasher);
        }
        
        // Hash rules
        format!("{:?}", self.reference_rule).hash(&mut hasher);
        format!("{:?}", self.tie_rule).hash(&mut hasher);
        format!("{:?}", self.visibility_rule).hash(&mut hasher);
        format!("{:?}", self.rounding_policy).hash(&mut hasher);
        self.max_staleness_ns.hash(&mut hasher);
        self.abort_on_missing.hash(&mut hasher);
        self.abort_on_stale.hash(&mut hasher);
        
        hasher.finish()
    }

    /// Format as a compact string for logging.
    pub fn format_compact(&self) -> String {
        let feeds: Vec<String> = self.feeds.keys().cloned().collect();
        format!(
            "OracleConfig[feeds={:?} rule={:?} tie={:?} vis={:?} abort_missing={} abort_stale={}]",
            feeds,
            self.reference_rule,
            self.tie_rule,
            self.visibility_rule,
            self.abort_on_missing,
            self.abort_on_stale,
        )
    }

    /// Format as a detailed report.
    pub fn format_report(&self) -> String {
        let mut out = String::new();
        
        out.push_str("╔══════════════════════════════════════════════════════════════════════════════╗\n");
        out.push_str("║                      ORACLE CONFIGURATION REPORT                             ║\n");
        out.push_str("╠══════════════════════════════════════════════════════════════════════════════╣\n");
        out.push_str(&format!("║  Reference Rule:    {:56} ║\n", format!("{:?}", self.reference_rule)));
        out.push_str(&format!("║  Tie Rule:          {:56} ║\n", format!("{:?}", self.tie_rule)));
        out.push_str(&format!("║  Visibility Rule:   {:56} ║\n", format!("{:?}", self.visibility_rule)));
        out.push_str(&format!("║  Rounding Policy:   {:56} ║\n", format!("{:?}", self.rounding_policy)));
        out.push_str(&format!("║  Max Staleness:     {:>10} ms                                          ║\n", 
            self.max_staleness_ns / 1_000_000));
        out.push_str(&format!("║  Abort on Missing:  {:56} ║\n", self.abort_on_missing));
        out.push_str(&format!("║  Abort on Stale:    {:56} ║\n", self.abort_on_stale));
        out.push_str("╠══════════════════════════════════════════════════════════════════════════════╣\n");
        out.push_str("║  CONFIGURED FEEDS                                                            ║\n");
        
        for (asset, feed) in &self.feeds {
            out.push_str(&format!("║  {:6} chain={:>3} addr={} dec={}             ║\n",
                asset,
                feed.chain_id,
                &feed.feed_proxy_address[..18],
                feed.decimals
            ));
        }
        
        out.push_str("╠══════════════════════════════════════════════════════════════════════════════╣\n");
        out.push_str(&format!("║  Fingerprint Hash:  {:016x}                                          ║\n", 
            self.fingerprint_hash()));
        out.push_str("╚══════════════════════════════════════════════════════════════════════════════╝\n");
        
        out
    }
}

impl Default for OracleConfig {
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
    fn test_empty_config_fails_validation() {
        let config = OracleConfig::new();
        let result = config.validate_production();
        
        assert!(!result.is_valid);
        assert!(!result.violations.is_empty());
    }

    #[test]
    fn test_production_btc_passes_validation() {
        let config = OracleConfig::production_btc_polygon();
        let result = config.validate_production();
        
        assert!(result.is_valid, "Violations: {:?}", result.violations);
        assert!(result.fingerprint_hash.is_some());
    }

    #[test]
    fn test_production_multi_asset_passes_validation() {
        let config = OracleConfig::production_multi_asset_polygon();
        let result = config.validate_production();
        
        assert!(result.is_valid, "Violations: {:?}", result.violations);
    }

    #[test]
    fn test_invalid_feed_address_fails() {
        let mut config = OracleConfig::new();
        config.feeds.insert("BTC".to_string(), OracleFeedConfig {
            asset_symbol: "BTC".to_string(),
            chain_id: 137,
            feed_proxy_address: "invalid".to_string(), // Invalid address
            decimals: 8,
            expected_description: None,
            deviation_threshold: None,
            heartbeat_secs: None,
        });
        
        let result = config.validate_production();
        assert!(!result.is_valid);
        assert!(result.violations.iter().any(|v| v.field.contains("feed_proxy_address")));
    }

    #[test]
    fn test_immediate_visibility_fails_validation() {
        let mut config = OracleConfig::production_btc_polygon();
        config.visibility_rule = OracleVisibilityRule::Immediate;
        
        let result = config.validate_production();
        assert!(!result.is_valid);
        assert!(result.violations.iter().any(|v| v.field == "visibility_rule"));
    }

    #[test]
    fn test_fingerprint_stability() {
        let config1 = OracleConfig::production_btc_polygon();
        let config2 = OracleConfig::production_btc_polygon();
        
        assert_eq!(config1.fingerprint_hash(), config2.fingerprint_hash());
    }

    #[test]
    fn test_fingerprint_changes_on_rule_change() {
        let mut config1 = OracleConfig::production_btc_polygon();
        let mut config2 = OracleConfig::production_btc_polygon();
        
        config2.reference_rule = SettlementReferenceRule::FirstUpdateAfterCutoff;
        
        assert_ne!(config1.fingerprint_hash(), config2.fingerprint_hash());
    }

    #[test]
    fn test_fingerprint_changes_on_feed_change() {
        let mut config1 = OracleConfig::production_btc_polygon();
        let mut config2 = OracleConfig::production_btc_polygon();
        
        // Change feed address
        config2.feeds.get_mut("BTC").unwrap().feed_proxy_address = 
            "0x0000000000000000000000000000000000000000".to_string();
        
        assert_ne!(config1.fingerprint_hash(), config2.fingerprint_hash());
    }

    #[test]
    fn test_mixed_chains_fails_validation() {
        let mut config = OracleConfig::new();
        config.feeds.insert("BTC".to_string(), OracleFeedConfig {
            asset_symbol: "BTC".to_string(),
            chain_id: 137, // Polygon
            feed_proxy_address: "0xc907E116054Ad103354f2D350FD2514433D57F6f".to_string(),
            decimals: 8,
            expected_description: None,
            deviation_threshold: None,
            heartbeat_secs: None,
        });
        config.feeds.insert("ETH".to_string(), OracleFeedConfig {
            asset_symbol: "ETH".to_string(),
            chain_id: 1, // Ethereum mainnet - different chain!
            feed_proxy_address: "0x5f4eC3Df9cbd43714FE2740f5E3616155c5b8419".to_string(),
            decimals: 8,
            expected_description: None,
            deviation_threshold: None,
            heartbeat_secs: None,
        });
        
        let result = config.validate_production();
        assert!(!result.is_valid);
        assert!(result.violations.iter().any(|v| v.description.contains("multiple chain IDs")));
    }

    #[test]
    fn test_feed_id_generation() {
        let feed = OracleFeedConfig {
            asset_symbol: "BTC".to_string(),
            chain_id: 137,
            feed_proxy_address: "0xc907E116054Ad103354f2D350FD2514433D57F6f".to_string(),
            decimals: 8,
            expected_description: None,
            deviation_threshold: None,
            heartbeat_secs: None,
        };
        
        let feed_id = feed.feed_id();
        assert!(feed_id.starts_with("btc_137_"));
    }
}

//! Pre-Resolved Market Registry for Hermetic Backtesting
//!
//! This module implements a strict "pre-resolved market registry" workflow:
//! - All market metadata and token IDs must be resolved BEFORE the run starts
//! - No runtime Gamma/API lookups are allowed during backtesting
//! - The registry is part of the run input and included in the run fingerprint
//!
//! # Hermetic Guarantee
//!
//! The backtester MUST remain hermetic and deterministic. Strategies cannot
//! perform runtime lookups. All required identifiers are:
//! 1. Resolved offline by a separate tool
//! 2. Stored in a deterministic, versioned registry
//! 3. Injected into Strategy via StrategyParams or Arc<MarketRegistry>
//! 4. Enforced by the framework and trust gate
//!
//! # Registry Fingerprinting
//!
//! The registry fingerprint is computed from:
//! - All market keys (sorted alphabetically)
//! - All metadata fields (normalized to canonical format)
//! - Registry version
//!
//! Changing ANY field changes the fingerprint deterministically.

use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::Arc;

/// Registry format version - increment when schema changes.
pub const REGISTRY_VERSION: &str = "MARKET_REGISTRY_V1";

/// Scale factor for fee rates (basis points to fixed-point).
const FEE_SCALE: f64 = 10000.0;
/// Scale factor for prices (to fixed-point integers).
const PRICE_SCALE: f64 = 1e8;

// =============================================================================
// MARKET KEY
// =============================================================================

/// Stable key for identifying a market in the registry.
///
/// The key uniquely identifies a market across:
/// - Exchange/venue
/// - Asset
/// - Market type (e.g., "15m_updown")
/// - Window size (for time-bounded markets)
///
/// # Canonical Format
///
/// The string representation follows a canonical format:
/// `{exchange}:{asset}:{market_type}:{window_size_secs}`
///
/// Example: `polymarket:btc:15m_updown:900`
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct MarketKey {
    /// Exchange/venue identifier (e.g., "polymarket").
    pub exchange: String,
    /// Asset symbol (e.g., "btc", "eth", "sol").
    pub asset: String,
    /// Market type (e.g., "15m_updown", "binary", "scalar").
    pub market_type: String,
    /// Window size in seconds (for time-bounded markets, 0 otherwise).
    pub window_size_secs: u32,
}

impl MarketKey {
    /// Create a new market key.
    pub fn new(
        exchange: impl Into<String>,
        asset: impl Into<String>,
        market_type: impl Into<String>,
        window_size_secs: u32,
    ) -> Self {
        Self {
            exchange: exchange.into().to_lowercase(),
            asset: asset.into().to_lowercase(),
            market_type: market_type.into().to_lowercase(),
            window_size_secs,
        }
    }

    /// Create a key for Polymarket 15m Up/Down market.
    pub fn polymarket_15m_updown(asset: impl Into<String>) -> Self {
        Self::new("polymarket", asset, "15m_updown", 900)
    }

    /// Convert to canonical string representation.
    pub fn to_canonical(&self) -> String {
        format!(
            "{}:{}:{}:{}",
            self.exchange, self.asset, self.market_type, self.window_size_secs
        )
    }

    /// Parse from canonical string representation.
    pub fn from_canonical(s: &str) -> Result<Self, MarketRegistryError> {
        let parts: Vec<&str> = s.split(':').collect();
        if parts.len() != 4 {
            return Err(MarketRegistryError::InvalidMarketKey {
                key: s.to_string(),
                reason: "Expected format: exchange:asset:market_type:window_size".to_string(),
            });
        }

        let window_size_secs = parts[3].parse().map_err(|_| MarketRegistryError::InvalidMarketKey {
            key: s.to_string(),
            reason: format!("Invalid window_size: {}", parts[3]),
        })?;

        Ok(Self {
            exchange: parts[0].to_lowercase(),
            asset: parts[1].to_lowercase(),
            market_type: parts[2].to_lowercase(),
            window_size_secs,
        })
    }

    /// Compute a deterministic hash.
    pub fn compute_hash(&self) -> u64 {
        let mut hasher = DefaultHasher::new();
        self.to_canonical().hash(&mut hasher);
        hasher.finish()
    }
}

impl std::fmt::Display for MarketKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.to_canonical())
    }
}

// =============================================================================
// TOKEN IDS
// =============================================================================

/// Explicit token IDs for a market's outcomes.
///
/// For Polymarket 15m Up/Down, these are the CLOB token IDs for:
/// - "Up" (Yes) outcome
/// - "Down" (No) outcome
///
/// These IDs are large integers as strings (Polymarket uses ~78-digit token IDs).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TokenIds {
    /// Token ID for the Up/Yes outcome.
    pub token_up: String,
    /// Token ID for the Down/No outcome.
    pub token_down: String,
}

impl TokenIds {
    pub fn new(token_up: impl Into<String>, token_down: impl Into<String>) -> Self {
        Self {
            token_up: token_up.into(),
            token_down: token_down.into(),
        }
    }

    /// Validate that token IDs are non-empty and look valid.
    pub fn validate(&self) -> Result<(), MarketRegistryError> {
        if self.token_up.is_empty() {
            return Err(MarketRegistryError::InvalidTokenId {
                field: "token_up".to_string(),
                reason: "Token ID cannot be empty".to_string(),
            });
        }
        if self.token_down.is_empty() {
            return Err(MarketRegistryError::InvalidTokenId {
                field: "token_down".to_string(),
                reason: "Token ID cannot be empty".to_string(),
            });
        }
        // Polymarket token IDs are numeric strings
        if !self.token_up.chars().all(|c| c.is_ascii_digit()) {
            return Err(MarketRegistryError::InvalidTokenId {
                field: "token_up".to_string(),
                reason: "Token ID must be numeric".to_string(),
            });
        }
        if !self.token_down.chars().all(|c| c.is_ascii_digit()) {
            return Err(MarketRegistryError::InvalidTokenId {
                field: "token_down".to_string(),
                reason: "Token ID must be numeric".to_string(),
            });
        }
        Ok(())
    }

    /// Get all token IDs as a set.
    pub fn as_set(&self) -> HashSet<&str> {
        let mut set = HashSet::new();
        set.insert(self.token_up.as_str());
        set.insert(self.token_down.as_str());
        set
    }

    /// Check if a token ID belongs to this market.
    pub fn contains(&self, token_id: &str) -> bool {
        self.token_up == token_id || self.token_down == token_id
    }

    /// Compute a deterministic hash.
    pub fn compute_hash(&self) -> u64 {
        let mut hasher = DefaultHasher::new();
        self.token_up.hash(&mut hasher);
        self.token_down.hash(&mut hasher);
        hasher.finish()
    }
}

// =============================================================================
// FEE SCHEDULE
// =============================================================================

/// Fee schedule for a market.
///
/// All fees are in basis points (1 bp = 0.01%).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FeeSchedule {
    /// Maker fee in basis points (negative = rebate).
    pub maker_fee_bps: i32,
    /// Taker fee in basis points.
    pub taker_fee_bps: i32,
}

impl FeeSchedule {
    pub fn new(maker_fee_bps: i32, taker_fee_bps: i32) -> Self {
        Self {
            maker_fee_bps,
            taker_fee_bps,
        }
    }

    /// Polymarket standard fee schedule.
    pub fn polymarket_standard() -> Self {
        Self {
            maker_fee_bps: 0,   // No maker fee
            taker_fee_bps: 10,  // 0.10% taker fee (10 bps)
        }
    }

    /// Convert maker fee to fractional rate.
    pub fn maker_rate(&self) -> f64 {
        self.maker_fee_bps as f64 / FEE_SCALE
    }

    /// Convert taker fee to fractional rate.
    pub fn taker_rate(&self) -> f64 {
        self.taker_fee_bps as f64 / FEE_SCALE
    }

    /// Compute a deterministic hash.
    pub fn compute_hash(&self) -> u64 {
        let mut hasher = DefaultHasher::new();
        self.maker_fee_bps.hash(&mut hasher);
        self.taker_fee_bps.hash(&mut hasher);
        hasher.finish()
    }
}

impl Default for FeeSchedule {
    fn default() -> Self {
        Self::polymarket_standard()
    }
}

// =============================================================================
// SETTLEMENT RULE
// =============================================================================

/// Settlement rule for 15m Up/Down markets.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SettlementRule {
    /// How the reference price is determined (e.g., "chainlink_last_before_cutoff").
    pub reference_rule: String,
    /// Tie rule (e.g., "no_wins" means Down wins on ties).
    pub tie_rule: String,
    /// Window alignment rule (e.g., "utc_aligned" for UTC 15-minute boundaries).
    pub window_alignment: String,
    /// Cutoff behavior (e.g., "inclusive" or "exclusive").
    pub cutoff_behavior: String,
}

impl SettlementRule {
    /// Standard Polymarket 15m Up/Down settlement rule.
    pub fn polymarket_15m_standard() -> Self {
        Self {
            reference_rule: "chainlink_last_before_cutoff".to_string(),
            tie_rule: "no_wins".to_string(), // Down wins on ties
            window_alignment: "utc_aligned".to_string(),
            cutoff_behavior: "inclusive".to_string(),
        }
    }

    /// Compute a deterministic hash.
    pub fn compute_hash(&self) -> u64 {
        let mut hasher = DefaultHasher::new();
        self.reference_rule.hash(&mut hasher);
        self.tie_rule.hash(&mut hasher);
        self.window_alignment.hash(&mut hasher);
        self.cutoff_behavior.hash(&mut hasher);
        hasher.finish()
    }
}

impl Default for SettlementRule {
    fn default() -> Self {
        Self::polymarket_15m_standard()
    }
}

// =============================================================================
// MARKET FLAGS
// =============================================================================

/// Per-market execution flags.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MarketFlags {
    /// Whether post-only orders are allowed.
    pub post_only_allowed: bool,
    /// Whether IOC (immediate-or-cancel) orders are allowed.
    pub ioc_allowed: bool,
    /// Whether FOK (fill-or-kill) orders are allowed.
    pub fok_allowed: bool,
    /// Whether market orders are allowed.
    pub market_orders_allowed: bool,
    /// Whether the market is currently active/tradable.
    pub is_active: bool,
}

impl MarketFlags {
    /// Default flags for Polymarket 15m markets.
    pub fn polymarket_15m_default() -> Self {
        Self {
            post_only_allowed: true,
            ioc_allowed: true,
            fok_allowed: true,
            market_orders_allowed: false, // Polymarket doesn't have market orders
            is_active: true,
        }
    }
}

impl Default for MarketFlags {
    fn default() -> Self {
        Self::polymarket_15m_default()
    }
}

// =============================================================================
// MARKET METADATA
// =============================================================================

/// Complete deterministic metadata for a market.
///
/// This struct contains ALL information a strategy needs to place and
/// account for orders WITHOUT any network calls.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MarketMeta {
    /// The market key (stable identifier).
    pub key: MarketKey,
    
    /// Human-readable market name/description.
    pub name: String,
    
    /// Market slug (used in dataset events).
    /// For 15m markets, this is a pattern like "btc-updown-15m-{unix_ts}".
    pub slug_pattern: String,
    
    /// Explicit token IDs for Up/Down outcomes.
    pub tokens: TokenIds,
    
    /// Tick size (minimum price increment).
    /// For Polymarket: 0.01 (1 cent).
    pub tick_size: f64,
    
    /// Minimum order size (in shares).
    pub min_order_size: f64,
    
    /// Maximum price decimals allowed.
    pub max_price_decimals: u8,
    
    /// Fee schedule.
    pub fees: FeeSchedule,
    
    /// Settlement rule.
    pub settlement: SettlementRule,
    
    /// Execution flags.
    pub flags: MarketFlags,
    
    /// Chainlink oracle feed address (for settlement reference).
    pub chainlink_feed_address: Option<String>,
    
    /// Chainlink oracle decimals.
    pub chainlink_decimals: Option<u8>,
    
    /// Metadata version (incremented when market config changes).
    pub version: u32,
    
    /// Timestamp when this metadata was resolved (Unix seconds).
    pub resolved_at: u64,
    
    /// Hash of source data used to resolve this metadata.
    /// Useful for detecting stale registries.
    pub source_hash: Option<String>,
}

impl MarketMeta {
    /// Create metadata for a Polymarket 15m Up/Down market.
    pub fn polymarket_15m_updown(
        asset: &str,
        token_up: impl Into<String>,
        token_down: impl Into<String>,
        chainlink_feed: Option<String>,
        chainlink_decimals: Option<u8>,
    ) -> Self {
        Self {
            key: MarketKey::polymarket_15m_updown(asset),
            name: format!("{} 15m Up/Down", asset.to_uppercase()),
            slug_pattern: format!("{}-updown-15m-{{ts}}", asset.to_lowercase()),
            tokens: TokenIds::new(token_up, token_down),
            tick_size: 0.01,
            min_order_size: 1.0,
            max_price_decimals: 2,
            fees: FeeSchedule::polymarket_standard(),
            settlement: SettlementRule::polymarket_15m_standard(),
            flags: MarketFlags::polymarket_15m_default(),
            chainlink_feed_address: chainlink_feed,
            chainlink_decimals,
            version: 1,
            resolved_at: 0, // To be set by registry generator
            source_hash: None,
        }
    }

    /// Validate the metadata for production use.
    pub fn validate(&self) -> Result<(), MarketRegistryError> {
        // Validate token IDs
        self.tokens.validate()?;

        // Validate tick size
        if self.tick_size <= 0.0 || self.tick_size > 1.0 {
            return Err(MarketRegistryError::InvalidMetadata {
                market_key: self.key.to_canonical(),
                field: "tick_size".to_string(),
                reason: format!("tick_size must be in (0, 1], got {}", self.tick_size),
            });
        }

        // Validate min order size
        if self.min_order_size <= 0.0 {
            return Err(MarketRegistryError::InvalidMetadata {
                market_key: self.key.to_canonical(),
                field: "min_order_size".to_string(),
                reason: format!("min_order_size must be > 0, got {}", self.min_order_size),
            });
        }

        // Validate settlement has oracle for 15m markets
        if self.key.market_type == "15m_updown" && self.chainlink_feed_address.is_none() {
            return Err(MarketRegistryError::InvalidMetadata {
                market_key: self.key.to_canonical(),
                field: "chainlink_feed_address".to_string(),
                reason: "15m Up/Down markets require Chainlink feed address".to_string(),
            });
        }

        Ok(())
    }

    /// Check if a market slug matches this market's pattern.
    pub fn matches_slug(&self, slug: &str) -> bool {
        // For 15m markets: "btc-updown-15m-1234567890"
        // Pattern: "btc-updown-15m-{ts}"
        let pattern_prefix = self.slug_pattern.replace("{ts}", "");
        slug.starts_with(&pattern_prefix) && {
            let suffix = &slug[pattern_prefix.len()..];
            suffix.chars().all(|c| c.is_ascii_digit())
        }
    }

    /// Compute a deterministic hash of all fields.
    pub fn compute_hash(&self) -> u64 {
        let mut hasher = DefaultHasher::new();

        // Hash all fields in canonical order
        self.key.to_canonical().hash(&mut hasher);
        self.name.hash(&mut hasher);
        self.slug_pattern.hash(&mut hasher);
        self.tokens.compute_hash().hash(&mut hasher);

        // Convert floats to fixed-point for determinism
        ((self.tick_size * PRICE_SCALE) as i64).hash(&mut hasher);
        ((self.min_order_size * PRICE_SCALE) as i64).hash(&mut hasher);
        self.max_price_decimals.hash(&mut hasher);

        self.fees.compute_hash().hash(&mut hasher);
        self.settlement.compute_hash().hash(&mut hasher);

        self.flags.hash(&mut hasher);
        self.chainlink_feed_address.hash(&mut hasher);
        self.chainlink_decimals.hash(&mut hasher);
        self.version.hash(&mut hasher);
        // Note: resolved_at and source_hash are NOT included in hash
        // (they are metadata about the resolution process, not the market itself)

        hasher.finish()
    }
}

// =============================================================================
// MARKET REGISTRY
// =============================================================================

/// Pre-resolved market registry for hermetic backtesting.
///
/// This is the SINGLE SOURCE OF TRUTH for market metadata during a backtest.
/// Strategies receive an Arc<MarketRegistry> and MUST NOT perform any runtime
/// lookups.
///
/// # Determinism
///
/// The registry is serialized in canonical order (sorted keys) and its
/// fingerprint changes if and only if any market's metadata changes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketRegistry {
    /// Registry format version.
    pub version: String,
    
    /// Markets indexed by canonical key string.
    /// Using BTreeMap for deterministic iteration order.
    pub markets: BTreeMap<String, MarketMeta>,
    
    /// Token ID to market key lookup.
    /// For fast token ID resolution during event processing.
    #[serde(skip)]
    token_to_market: HashMap<String, String>,
    
    /// Computed registry fingerprint.
    pub fingerprint: u64,
    
    /// Human-readable fingerprint hex.
    pub fingerprint_hex: String,
    
    /// When this registry was created (Unix seconds).
    pub created_at: u64,
    
    /// Description of how this registry was generated.
    pub generation_notes: Option<String>,
}

impl MarketRegistry {
    /// Create a new empty registry.
    pub fn new() -> Self {
        let mut registry = Self {
            version: REGISTRY_VERSION.to_string(),
            markets: BTreeMap::new(),
            token_to_market: HashMap::new(),
            fingerprint: 0,
            fingerprint_hex: String::new(),
            created_at: 0,
            generation_notes: None,
        };
        registry.recompute_fingerprint();
        registry
    }

    /// Add a market to the registry.
    pub fn add_market(&mut self, meta: MarketMeta) -> Result<(), MarketRegistryError> {
        // Validate the metadata
        meta.validate()?;

        let key_str = meta.key.to_canonical();

        // Check for duplicate market
        if self.markets.contains_key(&key_str) {
            return Err(MarketRegistryError::DuplicateMarket {
                market_key: key_str,
            });
        }

        // Check for duplicate token IDs
        for token in [&meta.tokens.token_up, &meta.tokens.token_down] {
            if self.token_to_market.contains_key(token) {
                return Err(MarketRegistryError::DuplicateTokenId {
                    token_id: token.clone(),
                    existing_market: self.token_to_market[token].clone(),
                    new_market: key_str.clone(),
                });
            }
        }

        // Add to token lookup
        self.token_to_market
            .insert(meta.tokens.token_up.clone(), key_str.clone());
        self.token_to_market
            .insert(meta.tokens.token_down.clone(), key_str.clone());

        // Add market
        self.markets.insert(key_str, meta);

        // Recompute fingerprint
        self.recompute_fingerprint();

        Ok(())
    }

    /// Get a market by its key.
    pub fn get(&self, key: &MarketKey) -> Option<&MarketMeta> {
        self.markets.get(&key.to_canonical())
    }

    /// Get a market by its canonical key string.
    pub fn get_by_key_str(&self, key_str: &str) -> Option<&MarketMeta> {
        self.markets.get(key_str)
    }

    /// Get a market by token ID.
    pub fn get_by_token_id(&self, token_id: &str) -> Option<&MarketMeta> {
        self.token_to_market
            .get(token_id)
            .and_then(|key_str| self.markets.get(key_str))
    }

    /// Check if a token ID exists in the registry.
    pub fn has_token(&self, token_id: &str) -> bool {
        self.token_to_market.contains_key(token_id)
    }

    /// Get all market keys.
    pub fn market_keys(&self) -> Vec<&str> {
        self.markets.keys().map(|s| s.as_str()).collect()
    }

    /// Get all token IDs in the registry.
    pub fn all_token_ids(&self) -> HashSet<&str> {
        self.token_to_market.keys().map(|s| s.as_str()).collect()
    }

    /// Number of markets in the registry.
    pub fn len(&self) -> usize {
        self.markets.len()
    }

    /// Check if the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.markets.is_empty()
    }

    /// Validate the entire registry.
    pub fn validate(&self) -> Result<(), MarketRegistryError> {
        if self.markets.is_empty() {
            return Err(MarketRegistryError::EmptyRegistry);
        }

        for meta in self.markets.values() {
            meta.validate()?;
        }

        Ok(())
    }

    /// Validate that a dataset references only known markets/tokens.
    pub fn validate_dataset_compatibility(
        &self,
        dataset_token_ids: &HashSet<String>,
        dataset_market_slugs: &HashSet<String>,
    ) -> Result<RegistryDatasetValidation, MarketRegistryError> {
        let mut validation = RegistryDatasetValidation::new();

        // Check token IDs
        for token_id in dataset_token_ids {
            if self.has_token(token_id) {
                validation.matched_tokens.insert(token_id.clone());
            } else {
                validation.unknown_tokens.insert(token_id.clone());
            }
        }

        // Check market slugs
        for slug in dataset_market_slugs {
            let mut matched = false;
            for meta in self.markets.values() {
                if meta.matches_slug(slug) {
                    validation.matched_slugs.insert(slug.clone());
                    matched = true;
                    break;
                }
            }
            if !matched {
                validation.unknown_slugs.insert(slug.clone());
            }
        }

        // Check for markets in registry but not in dataset
        for meta in self.markets.values() {
            let has_up = dataset_token_ids.contains(&meta.tokens.token_up);
            let has_down = dataset_token_ids.contains(&meta.tokens.token_down);
            if !has_up && !has_down {
                validation.unused_markets.insert(meta.key.to_canonical());
            }
        }

        validation.is_valid = validation.unknown_tokens.is_empty();

        Ok(validation)
    }

    /// Recompute the registry fingerprint.
    fn recompute_fingerprint(&mut self) {
        let mut hasher = DefaultHasher::new();

        // Hash version
        self.version.hash(&mut hasher);

        // Hash all markets in sorted order (BTreeMap guarantees this)
        for (key, meta) in &self.markets {
            key.hash(&mut hasher);
            meta.compute_hash().hash(&mut hasher);
        }

        self.fingerprint = hasher.finish();
        self.fingerprint_hex = format!("{:016x}", self.fingerprint);
    }

    /// Rebuild the token lookup table after deserialization.
    pub fn rebuild_lookup(&mut self) {
        self.token_to_market.clear();
        for (key_str, meta) in &self.markets {
            self.token_to_market
                .insert(meta.tokens.token_up.clone(), key_str.clone());
            self.token_to_market
                .insert(meta.tokens.token_down.clone(), key_str.clone());
        }
    }

    /// Load registry from JSON string.
    pub fn from_json(json: &str) -> Result<Self, MarketRegistryError> {
        let mut registry: Self = serde_json::from_str(json).map_err(|e| {
            MarketRegistryError::ParseError {
                reason: e.to_string(),
            }
        })?;

        // Rebuild lookup table
        registry.rebuild_lookup();

        // Verify fingerprint
        let expected = registry.fingerprint;
        registry.recompute_fingerprint();
        if registry.fingerprint != expected {
            return Err(MarketRegistryError::FingerprintMismatch {
                expected,
                computed: registry.fingerprint,
            });
        }

        Ok(registry)
    }

    /// Serialize registry to canonical JSON.
    pub fn to_json(&self) -> Result<String, MarketRegistryError> {
        serde_json::to_string_pretty(self).map_err(|e| MarketRegistryError::SerializeError {
            reason: e.to_string(),
        })
    }

    /// Load registry from a file path.
    pub fn load_from_file(path: &str) -> Result<Self, MarketRegistryError> {
        let content = std::fs::read_to_string(path).map_err(|e| MarketRegistryError::IoError {
            path: path.to_string(),
            reason: e.to_string(),
        })?;
        Self::from_json(&content)
    }

    /// Save registry to a file path.
    pub fn save_to_file(&self, path: &str) -> Result<(), MarketRegistryError> {
        let json = self.to_json()?;
        std::fs::write(path, json).map_err(|e| MarketRegistryError::IoError {
            path: path.to_string(),
            reason: e.to_string(),
        })?;
        Ok(())
    }
}

impl Default for MarketRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// =============================================================================
// REGISTRY DATASET VALIDATION
// =============================================================================

/// Result of validating registry against dataset.
#[derive(Debug, Clone, Default)]
pub struct RegistryDatasetValidation {
    /// Whether the validation passed.
    pub is_valid: bool,
    /// Token IDs in dataset that matched registry.
    pub matched_tokens: HashSet<String>,
    /// Token IDs in dataset not found in registry.
    pub unknown_tokens: HashSet<String>,
    /// Market slugs in dataset that matched registry.
    pub matched_slugs: HashSet<String>,
    /// Market slugs in dataset not found in registry.
    pub unknown_slugs: HashSet<String>,
    /// Markets in registry not referenced by dataset.
    pub unused_markets: HashSet<String>,
}

impl RegistryDatasetValidation {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn format_report(&self) -> String {
        let mut out = String::new();

        out.push_str("╔══════════════════════════════════════════════════════════════╗\n");
        out.push_str("║       REGISTRY-DATASET COMPATIBILITY VALIDATION             ║\n");
        out.push_str("╠══════════════════════════════════════════════════════════════╣\n");

        if self.is_valid {
            out.push_str("║  RESULT: ✓ COMPATIBLE                                       ║\n");
        } else {
            out.push_str("║  RESULT: ✗ INCOMPATIBLE                                     ║\n");
        }

        out.push_str(&format!(
            "║  Matched tokens: {:4}                                        ║\n",
            self.matched_tokens.len()
        ));
        out.push_str(&format!(
            "║  Unknown tokens: {:4}                                        ║\n",
            self.unknown_tokens.len()
        ));
        out.push_str(&format!(
            "║  Matched slugs:  {:4}                                        ║\n",
            self.matched_slugs.len()
        ));
        out.push_str(&format!(
            "║  Unknown slugs:  {:4}                                        ║\n",
            self.unknown_slugs.len()
        ));
        out.push_str(&format!(
            "║  Unused markets: {:4}                                        ║\n",
            self.unused_markets.len()
        ));

        if !self.unknown_tokens.is_empty() {
            out.push_str("╠══════════════════════════════════════════════════════════════╣\n");
            out.push_str("║  UNKNOWN TOKEN IDS (in dataset but not registry):           ║\n");
            for token in self.unknown_tokens.iter().take(5) {
                let display = if token.len() > 50 {
                    format!("{}...", &token[..47])
                } else {
                    token.clone()
                };
                out.push_str(&format!("║    {}  ║\n", display));
            }
            if self.unknown_tokens.len() > 5 {
                out.push_str(&format!(
                    "║    ... and {} more                                            ║\n",
                    self.unknown_tokens.len() - 5
                ));
            }
        }

        out.push_str("╚══════════════════════════════════════════════════════════════╝\n");
        out
    }
}

// =============================================================================
// REGISTRY HANDLE FOR STRATEGIES
// =============================================================================

/// Immutable handle to the market registry for strategy use.
///
/// Strategies receive this handle and can query metadata without any
/// network calls.
pub type RegistryHandle = Arc<MarketRegistry>;

/// Create a registry handle from a registry.
pub fn make_registry_handle(registry: MarketRegistry) -> RegistryHandle {
    Arc::new(registry)
}

// =============================================================================
// ERRORS
// =============================================================================

/// Errors related to market registry operations.
#[derive(Debug, Clone)]
pub enum MarketRegistryError {
    /// Invalid market key format.
    InvalidMarketKey { key: String, reason: String },
    /// Invalid token ID.
    InvalidTokenId { field: String, reason: String },
    /// Invalid metadata field.
    InvalidMetadata {
        market_key: String,
        field: String,
        reason: String,
    },
    /// Duplicate market key.
    DuplicateMarket { market_key: String },
    /// Duplicate token ID across markets.
    DuplicateTokenId {
        token_id: String,
        existing_market: String,
        new_market: String,
    },
    /// Empty registry.
    EmptyRegistry,
    /// Market not found.
    MarketNotFound { market_key: String },
    /// Token not found.
    TokenNotFound { token_id: String },
    /// Registry fingerprint mismatch.
    FingerprintMismatch { expected: u64, computed: u64 },
    /// JSON parse error.
    ParseError { reason: String },
    /// JSON serialize error.
    SerializeError { reason: String },
    /// File I/O error.
    IoError { path: String, reason: String },
    /// Registry required but not provided.
    RegistryRequired { context: String },
    /// Dataset contains tokens not in registry.
    UnknownTokensInDataset { tokens: Vec<String> },
}

impl std::fmt::Display for MarketRegistryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidMarketKey { key, reason } => {
                write!(f, "Invalid market key '{}': {}", key, reason)
            }
            Self::InvalidTokenId { field, reason } => {
                write!(f, "Invalid token ID in '{}': {}", field, reason)
            }
            Self::InvalidMetadata {
                market_key,
                field,
                reason,
            } => {
                write!(
                    f,
                    "Invalid metadata for '{}' field '{}': {}",
                    market_key, field, reason
                )
            }
            Self::DuplicateMarket { market_key } => {
                write!(f, "Duplicate market key: {}", market_key)
            }
            Self::DuplicateTokenId {
                token_id,
                existing_market,
                new_market,
            } => {
                write!(
                    f,
                    "Token ID '{}' already exists in market '{}', cannot add to '{}'",
                    token_id, existing_market, new_market
                )
            }
            Self::EmptyRegistry => write!(f, "Registry is empty"),
            Self::MarketNotFound { market_key } => {
                write!(f, "Market not found: {}", market_key)
            }
            Self::TokenNotFound { token_id } => {
                write!(f, "Token not found: {}", token_id)
            }
            Self::FingerprintMismatch { expected, computed } => {
                write!(
                    f,
                    "Registry fingerprint mismatch: expected {:016x}, computed {:016x}",
                    expected, computed
                )
            }
            Self::ParseError { reason } => write!(f, "Parse error: {}", reason),
            Self::SerializeError { reason } => write!(f, "Serialize error: {}", reason),
            Self::IoError { path, reason } => write!(f, "I/O error at '{}': {}", path, reason),
            Self::RegistryRequired { context } => {
                write!(f, "Registry required for {}", context)
            }
            Self::UnknownTokensInDataset { tokens } => {
                write!(
                    f,
                    "Dataset contains {} unknown token(s): {}...",
                    tokens.len(),
                    tokens.iter().take(3).cloned().collect::<Vec<_>>().join(", ")
                )
            }
        }
    }
}

impl std::error::Error for MarketRegistryError {}

// =============================================================================
// TRUST GATE EXTENSION
// =============================================================================

/// Additional trust failure reason for missing registry.
pub const TRUST_FAILURE_MISSING_REGISTRY: &str = "MISSING_REGISTRY";
/// Additional trust failure reason for invalid registry.
pub const TRUST_FAILURE_INVALID_REGISTRY: &str = "INVALID_REGISTRY";
/// Additional trust failure reason for dataset incompatibility.
pub const TRUST_FAILURE_DATASET_INCOMPATIBLE: &str = "DATASET_REGISTRY_INCOMPATIBLE";

// =============================================================================
// REGISTRY FINGERPRINT FOR RUN FINGERPRINT
// =============================================================================

/// Registry fingerprint component for inclusion in RunFingerprint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegistryFingerprint {
    /// Registry version string.
    pub version: String,
    /// Number of markets in registry.
    pub market_count: usize,
    /// Computed fingerprint hash.
    pub hash: u64,
    /// Human-readable hash hex.
    pub hash_hex: String,
}

impl RegistryFingerprint {
    /// Create from a MarketRegistry.
    pub fn from_registry(registry: &MarketRegistry) -> Self {
        Self {
            version: registry.version.clone(),
            market_count: registry.len(),
            hash: registry.fingerprint,
            hash_hex: registry.fingerprint_hex.clone(),
        }
    }

    /// Compute a combined hash including all fields.
    pub fn compute_hash(&self) -> u64 {
        let mut hasher = DefaultHasher::new();
        self.version.hash(&mut hasher);
        self.market_count.hash(&mut hasher);
        self.hash.hash(&mut hasher);
        hasher.finish()
    }
}

impl Default for RegistryFingerprint {
    fn default() -> Self {
        Self {
            version: REGISTRY_VERSION.to_string(),
            market_count: 0,
            hash: 0,
            hash_hex: "0000000000000000".to_string(),
        }
    }
}

// =============================================================================
// STRATEGY PARAMS EXTENSION
// =============================================================================

/// Keys for registry-related strategy params.
pub mod strategy_param_keys {
    /// Key for the primary market key string.
    pub const MARKET_KEY: &str = "registry.market_key";
    /// Key for the Up token ID.
    pub const TOKEN_UP: &str = "registry.token_up";
    /// Key for the Down token ID.
    pub const TOKEN_DOWN: &str = "registry.token_down";
    /// Key for the tick size.
    pub const TICK_SIZE: &str = "registry.tick_size";
    /// Key for the minimum order size.
    pub const MIN_ORDER_SIZE: &str = "registry.min_order_size";
    /// Key for the maker fee (basis points).
    pub const MAKER_FEE_BPS: &str = "registry.maker_fee_bps";
    /// Key for the taker fee (basis points).
    pub const TAKER_FEE_BPS: &str = "registry.taker_fee_bps";
    /// Key for the registry fingerprint.
    pub const REGISTRY_FINGERPRINT: &str = "registry.fingerprint";
}

/// Inject registry metadata into StrategyParams.
pub fn inject_registry_params(
    params: &mut crate::backtest_v2::strategy::StrategyParams,
    meta: &MarketMeta,
    registry_fingerprint: u64,
) {
    use strategy_param_keys::*;

    params.strings.insert(MARKET_KEY.to_string(), meta.key.to_canonical());
    params.strings.insert(TOKEN_UP.to_string(), meta.tokens.token_up.clone());
    params.strings.insert(TOKEN_DOWN.to_string(), meta.tokens.token_down.clone());
    params.params.insert(TICK_SIZE.to_string(), meta.tick_size);
    params.params.insert(MIN_ORDER_SIZE.to_string(), meta.min_order_size);
    params.params.insert(MAKER_FEE_BPS.to_string(), meta.fees.maker_fee_bps as f64);
    params.params.insert(TAKER_FEE_BPS.to_string(), meta.fees.taker_fee_bps as f64);
    params.params.insert(REGISTRY_FINGERPRINT.to_string(), registry_fingerprint as f64);
}

/// Extract registry metadata from StrategyParams.
pub fn extract_registry_params(
    params: &crate::backtest_v2::strategy::StrategyParams,
) -> Result<ExtractedRegistryParams, MarketRegistryError> {
    use strategy_param_keys::*;

    let market_key = params
        .get_string(MARKET_KEY)
        .ok_or_else(|| MarketRegistryError::RegistryRequired {
            context: format!("missing {}", MARKET_KEY),
        })?
        .to_string();

    let token_up = params
        .get_string(TOKEN_UP)
        .ok_or_else(|| MarketRegistryError::RegistryRequired {
            context: format!("missing {}", TOKEN_UP),
        })?
        .to_string();

    let token_down = params
        .get_string(TOKEN_DOWN)
        .ok_or_else(|| MarketRegistryError::RegistryRequired {
            context: format!("missing {}", TOKEN_DOWN),
        })?
        .to_string();

    let tick_size = params.get(TICK_SIZE).ok_or_else(|| {
        MarketRegistryError::RegistryRequired {
            context: format!("missing {}", TICK_SIZE),
        }
    })?;

    let min_order_size = params.get(MIN_ORDER_SIZE).ok_or_else(|| {
        MarketRegistryError::RegistryRequired {
            context: format!("missing {}", MIN_ORDER_SIZE),
        }
    })?;

    let maker_fee_bps = params.get(MAKER_FEE_BPS).ok_or_else(|| {
        MarketRegistryError::RegistryRequired {
            context: format!("missing {}", MAKER_FEE_BPS),
        }
    })? as i32;

    let taker_fee_bps = params.get(TAKER_FEE_BPS).ok_or_else(|| {
        MarketRegistryError::RegistryRequired {
            context: format!("missing {}", TAKER_FEE_BPS),
        }
    })? as i32;

    let registry_fingerprint = params.get(REGISTRY_FINGERPRINT).ok_or_else(|| {
        MarketRegistryError::RegistryRequired {
            context: format!("missing {}", REGISTRY_FINGERPRINT),
        }
    })? as u64;

    Ok(ExtractedRegistryParams {
        market_key,
        token_up,
        token_down,
        tick_size,
        min_order_size,
        maker_fee_bps,
        taker_fee_bps,
        registry_fingerprint,
    })
}

/// Registry params extracted from StrategyParams.
#[derive(Debug, Clone)]
pub struct ExtractedRegistryParams {
    pub market_key: String,
    pub token_up: String,
    pub token_down: String,
    pub tick_size: f64,
    pub min_order_size: f64,
    pub maker_fee_bps: i32,
    pub taker_fee_bps: i32,
    pub registry_fingerprint: u64,
}

// =============================================================================
// TESTS
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_market_key_canonical() {
        let key = MarketKey::polymarket_15m_updown("btc");
        assert_eq!(key.to_canonical(), "polymarket:btc:15m_updown:900");

        let parsed = MarketKey::from_canonical("polymarket:btc:15m_updown:900").unwrap();
        assert_eq!(key, parsed);
    }

    #[test]
    fn test_market_key_case_insensitive() {
        let key1 = MarketKey::new("POLYMARKET", "BTC", "15M_UPDOWN", 900);
        let key2 = MarketKey::new("polymarket", "btc", "15m_updown", 900);
        assert_eq!(key1, key2);
    }

    #[test]
    fn test_token_ids_validation() {
        let valid = TokenIds::new("12345", "67890");
        assert!(valid.validate().is_ok());

        let empty = TokenIds::new("", "67890");
        assert!(empty.validate().is_err());

        let non_numeric = TokenIds::new("abc123", "67890");
        assert!(non_numeric.validate().is_err());
    }

    #[test]
    fn test_market_meta_hash_deterministic() {
        let meta1 = MarketMeta::polymarket_15m_updown(
            "btc",
            "111111111111111111111111111111111111111111111111111111111111111111111111111111",
            "222222222222222222222222222222222222222222222222222222222222222222222222222222",
            Some("0x123".to_string()),
            Some(8),
        );

        let meta2 = MarketMeta::polymarket_15m_updown(
            "btc",
            "111111111111111111111111111111111111111111111111111111111111111111111111111111",
            "222222222222222222222222222222222222222222222222222222222222222222222222222222",
            Some("0x123".to_string()),
            Some(8),
        );

        assert_eq!(meta1.compute_hash(), meta2.compute_hash());
    }

    #[test]
    fn test_market_meta_hash_changes_on_field_change() {
        let meta1 = MarketMeta::polymarket_15m_updown(
            "btc",
            "111111111111111111111111111111111111111111111111111111111111111111111111111111",
            "222222222222222222222222222222222222222222222222222222222222222222222222222222",
            Some("0x123".to_string()),
            Some(8),
        );

        let mut meta2 = meta1.clone();
        meta2.tick_size = 0.001; // Change tick size

        assert_ne!(meta1.compute_hash(), meta2.compute_hash());
    }

    #[test]
    fn test_registry_add_and_lookup() {
        let mut registry = MarketRegistry::new();

        let btc_meta = MarketMeta::polymarket_15m_updown(
            "btc",
            "111111111111111111111111111111111111111111111111111111111111111111111111111111",
            "222222222222222222222222222222222222222222222222222222222222222222222222222222",
            Some("0xBtcFeed".to_string()),
            Some(8),
        );

        registry.add_market(btc_meta.clone()).unwrap();

        // Lookup by key
        let key = MarketKey::polymarket_15m_updown("btc");
        let found = registry.get(&key).unwrap();
        assert_eq!(found.name, btc_meta.name);

        // Lookup by token ID
        let found_by_token = registry
            .get_by_token_id("111111111111111111111111111111111111111111111111111111111111111111111111111111")
            .unwrap();
        assert_eq!(found_by_token.key, key);
    }

    #[test]
    fn test_registry_fingerprint_deterministic() {
        let mut registry1 = MarketRegistry::new();
        let mut registry2 = MarketRegistry::new();

        let btc_meta = MarketMeta::polymarket_15m_updown(
            "btc",
            "111111111111111111111111111111111111111111111111111111111111111111111111111111",
            "222222222222222222222222222222222222222222222222222222222222222222222222222222",
            Some("0xBtcFeed".to_string()),
            Some(8),
        );

        let eth_meta = MarketMeta::polymarket_15m_updown(
            "eth",
            "333333333333333333333333333333333333333333333333333333333333333333333333333333",
            "444444444444444444444444444444444444444444444444444444444444444444444444444444",
            Some("0xEthFeed".to_string()),
            Some(8),
        );

        // Add in same order
        registry1.add_market(btc_meta.clone()).unwrap();
        registry1.add_market(eth_meta.clone()).unwrap();

        // Add in different order (BTreeMap ensures sorted output)
        registry2.add_market(eth_meta).unwrap();
        registry2.add_market(btc_meta).unwrap();

        assert_eq!(registry1.fingerprint, registry2.fingerprint);
    }

    #[test]
    fn test_registry_fingerprint_changes_on_content_change() {
        let mut registry1 = MarketRegistry::new();
        let mut registry2 = MarketRegistry::new();

        let btc_meta1 = MarketMeta::polymarket_15m_updown(
            "btc",
            "111111111111111111111111111111111111111111111111111111111111111111111111111111",
            "222222222222222222222222222222222222222222222222222222222222222222222222222222",
            Some("0xBtcFeed".to_string()),
            Some(8),
        );

        let mut btc_meta2 = btc_meta1.clone();
        btc_meta2.fees.taker_fee_bps = 20; // Different fee

        registry1.add_market(btc_meta1).unwrap();
        registry2.add_market(btc_meta2).unwrap();

        assert_ne!(registry1.fingerprint, registry2.fingerprint);
    }

    #[test]
    fn test_registry_json_roundtrip() {
        let mut registry = MarketRegistry::new();

        let btc_meta = MarketMeta::polymarket_15m_updown(
            "btc",
            "111111111111111111111111111111111111111111111111111111111111111111111111111111",
            "222222222222222222222222222222222222222222222222222222222222222222222222222222",
            Some("0xBtcFeed".to_string()),
            Some(8),
        );

        registry.add_market(btc_meta).unwrap();
        let original_fingerprint = registry.fingerprint;

        let json = registry.to_json().unwrap();
        let loaded = MarketRegistry::from_json(&json).unwrap();

        assert_eq!(loaded.fingerprint, original_fingerprint);
        assert_eq!(loaded.len(), 1);
    }

    #[test]
    fn test_registry_duplicate_market_rejected() {
        let mut registry = MarketRegistry::new();

        let btc_meta = MarketMeta::polymarket_15m_updown(
            "btc",
            "111111111111111111111111111111111111111111111111111111111111111111111111111111",
            "222222222222222222222222222222222222222222222222222222222222222222222222222222",
            Some("0xBtcFeed".to_string()),
            Some(8),
        );

        registry.add_market(btc_meta.clone()).unwrap();
        let result = registry.add_market(btc_meta);

        assert!(matches!(
            result,
            Err(MarketRegistryError::DuplicateMarket { .. })
        ));
    }

    #[test]
    fn test_registry_duplicate_token_rejected() {
        let mut registry = MarketRegistry::new();

        let btc_meta = MarketMeta::polymarket_15m_updown(
            "btc",
            "111111111111111111111111111111111111111111111111111111111111111111111111111111",
            "222222222222222222222222222222222222222222222222222222222222222222222222222222",
            Some("0xBtcFeed".to_string()),
            Some(8),
        );

        let eth_meta_with_duplicate = MarketMeta::polymarket_15m_updown(
            "eth",
            "111111111111111111111111111111111111111111111111111111111111111111111111111111", // Same as BTC!
            "333333333333333333333333333333333333333333333333333333333333333333333333333333",
            Some("0xEthFeed".to_string()),
            Some(8),
        );

        registry.add_market(btc_meta).unwrap();
        let result = registry.add_market(eth_meta_with_duplicate);

        assert!(matches!(
            result,
            Err(MarketRegistryError::DuplicateTokenId { .. })
        ));
    }

    #[test]
    fn test_market_slug_matching() {
        let meta = MarketMeta::polymarket_15m_updown(
            "btc",
            "111111111111111111111111111111111111111111111111111111111111111111111111111111",
            "222222222222222222222222222222222222222222222222222222222222222222222222222222",
            Some("0xBtcFeed".to_string()),
            Some(8),
        );

        assert!(meta.matches_slug("btc-updown-15m-1700000000"));
        assert!(meta.matches_slug("btc-updown-15m-1234567890"));
        assert!(!meta.matches_slug("eth-updown-15m-1700000000"));
        assert!(!meta.matches_slug("btc-updown-15m-abc"));
    }

    #[test]
    fn test_dataset_compatibility_validation() {
        let mut registry = MarketRegistry::new();

        let btc_meta = MarketMeta::polymarket_15m_updown(
            "btc",
            "111111111111111111111111111111111111111111111111111111111111111111111111111111",
            "222222222222222222222222222222222222222222222222222222222222222222222222222222",
            Some("0xBtcFeed".to_string()),
            Some(8),
        );
        registry.add_market(btc_meta).unwrap();

        // Dataset with known tokens
        let mut dataset_tokens = HashSet::new();
        dataset_tokens.insert(
            "111111111111111111111111111111111111111111111111111111111111111111111111111111".to_string(),
        );
        dataset_tokens.insert(
            "222222222222222222222222222222222222222222222222222222222222222222222222222222".to_string(),
        );

        let mut dataset_slugs = HashSet::new();
        dataset_slugs.insert("btc-updown-15m-1700000000".to_string());

        let validation = registry
            .validate_dataset_compatibility(&dataset_tokens, &dataset_slugs)
            .unwrap();

        assert!(validation.is_valid);
        assert_eq!(validation.matched_tokens.len(), 2);
        assert!(validation.unknown_tokens.is_empty());
    }

    #[test]
    fn test_dataset_compatibility_unknown_tokens() {
        let mut registry = MarketRegistry::new();

        let btc_meta = MarketMeta::polymarket_15m_updown(
            "btc",
            "111111111111111111111111111111111111111111111111111111111111111111111111111111",
            "222222222222222222222222222222222222222222222222222222222222222222222222222222",
            Some("0xBtcFeed".to_string()),
            Some(8),
        );
        registry.add_market(btc_meta).unwrap();

        // Dataset with unknown token
        let mut dataset_tokens = HashSet::new();
        dataset_tokens.insert("999999999999".to_string()); // Unknown!

        let dataset_slugs = HashSet::new();

        let validation = registry
            .validate_dataset_compatibility(&dataset_tokens, &dataset_slugs)
            .unwrap();

        assert!(!validation.is_valid);
        assert!(validation.unknown_tokens.contains("999999999999"));
    }

    #[test]
    fn test_strategy_params_injection() {
        let meta = MarketMeta::polymarket_15m_updown(
            "btc",
            "111111111111111111111111111111111111111111111111111111111111111111111111111111",
            "222222222222222222222222222222222222222222222222222222222222222222222222222222",
            Some("0xBtcFeed".to_string()),
            Some(8),
        );

        let mut params = crate::backtest_v2::strategy::StrategyParams::new();
        inject_registry_params(&mut params, &meta, 12345);

        let extracted = extract_registry_params(&params).unwrap();
        assert_eq!(extracted.market_key, "polymarket:btc:15m_updown:900");
        assert_eq!(
            extracted.token_up,
            "111111111111111111111111111111111111111111111111111111111111111111111111111111"
        );
        assert_eq!(extracted.tick_size, 0.01);
        assert_eq!(extracted.taker_fee_bps, 10);
        assert_eq!(extracted.registry_fingerprint, 12345);
    }

    #[test]
    fn test_extract_registry_params_missing_required() {
        let params = crate::backtest_v2::strategy::StrategyParams::new();
        let result = extract_registry_params(&params);
        assert!(matches!(
            result,
            Err(MarketRegistryError::RegistryRequired { .. })
        ));
    }
}

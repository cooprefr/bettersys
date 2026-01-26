//! Settlement Reference Mapping for 15M Up/Down Markets
//!
//! This module defines the SINGLE SOURCE OF TRUTH for how Binance price events
//! are transformed into settlement reference prices for 15-minute Up/Down markets.
//!
//! # Design Principles
//!
//! 1. **Explicit**: Every decision (venue, price type, rounding) is explicitly defined
//! 2. **Versioned**: The mapping version is stored in datasets and run fingerprints
//! 3. **Deterministic**: Given identical inputs, produces identical outputs
//! 4. **Auditable**: Every reference tick records its provenance
//! 5. **Fail-Fast**: Missing/invalid data causes deterministic failures, not silent drift
//!
//! # Fixed-Point Arithmetic
//!
//! All prices use fixed-point representation to ensure deterministic rounding:
//! - Scale factor: 10^8 (8 decimal places, matching Binance precision)
//! - Rounding rule: Bankers rounding (round half to even)
//! - Mid calculation: (bid + ask + 1) / 2 in fixed-point (rounds up on 0.5)
//!
//! # Mapping Contract
//!
//! The SettlementReferenceMapping15m struct is stored in:
//! - Dataset metadata (at recording time)
//! - Run fingerprint (at backtest time)
//!
//! Backtests MUST match the dataset mapping or explicitly adopt it.

use crate::backtest_v2::clock::Nanos;
use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

/// Current version of the settlement reference mapping specification.
/// Increment this when the semantics change in a backward-incompatible way.
pub const SETTLEMENT_REFERENCE_MAPPING_VERSION: u32 = 1;

/// Fixed-point scale factor (10^8 for 8 decimal places).
pub const FP_SCALE: i64 = 100_000_000;

/// Nanoseconds per second.
pub const NS_PER_SEC: i64 = 1_000_000_000;

// =============================================================================
// ASSET DEFINITIONS
// =============================================================================

/// Assets supported by the 15M Up/Down settlement mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum UpDownAsset {
    BTC = 0,
    ETH = 1,
    SOL = 2,
    XRP = 3,
}

impl UpDownAsset {
    /// Get the Binance spot symbol for this asset.
    pub fn binance_spot_symbol(&self) -> &'static str {
        match self {
            Self::BTC => "BTCUSDT",
            Self::ETH => "ETHUSDT",
            Self::SOL => "SOLUSDT",
            Self::XRP => "XRPUSDT",
        }
    }

    /// Get the Binance futures symbol for this asset.
    pub fn binance_futures_symbol(&self) -> &'static str {
        match self {
            Self::BTC => "BTCUSDT",
            Self::ETH => "ETHUSDT",
            Self::SOL => "SOLUSDT",
            Self::XRP => "XRPUSDT",
        }
    }

    /// Parse asset from market slug prefix.
    pub fn from_slug_prefix(slug: &str) -> Option<Self> {
        let lower = slug.to_lowercase();
        if lower.starts_with("btc-") || lower.starts_with("btc_") {
            Some(Self::BTC)
        } else if lower.starts_with("eth-") || lower.starts_with("eth_") {
            Some(Self::ETH)
        } else if lower.starts_with("sol-") || lower.starts_with("sol_") {
            Some(Self::SOL)
        } else if lower.starts_with("xrp-") || lower.starts_with("xrp_") {
            Some(Self::XRP)
        } else {
            None
        }
    }

    /// All supported assets.
    pub const ALL: &'static [Self] = &[Self::BTC, Self::ETH, Self::SOL, Self::XRP];
}

impl std::fmt::Display for UpDownAsset {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BTC => write!(f, "BTC"),
            Self::ETH => write!(f, "ETH"),
            Self::SOL => write!(f, "SOL"),
            Self::XRP => write!(f, "XRP"),
        }
    }
}

// =============================================================================
// VENUE DEFINITIONS
// =============================================================================

/// Reference price venue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum ReferenceVenue {
    /// Binance spot market.
    BinanceSpot = 0,
    /// Binance USD-M futures market.
    BinanceFutures = 1,
}

impl ReferenceVenue {
    /// Get venue identifier string.
    pub fn id(&self) -> &'static str {
        match self {
            Self::BinanceSpot => "binance_spot",
            Self::BinanceFutures => "binance_futures",
        }
    }
}

impl std::fmt::Display for ReferenceVenue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.id())
    }
}

// =============================================================================
// PRICE KIND DEFINITIONS
// =============================================================================

/// Kind of price used for settlement reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum PriceKind {
    /// Mid price: (best_bid + best_ask) / 2
    Mid = 0,
    /// Last trade price.
    Last = 1,
    /// Mark price (futures only).
    Mark = 2,
}

impl PriceKind {
    /// Description for audit logs.
    pub fn description(&self) -> &'static str {
        match self {
            Self::Mid => "mid price from best bid/ask",
            Self::Last => "last trade price",
            Self::Mark => "mark price",
        }
    }
}

impl std::fmt::Display for PriceKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Mid => write!(f, "Mid"),
            Self::Last => write!(f, "Last"),
            Self::Mark => write!(f, "Mark"),
        }
    }
}

// =============================================================================
// INPUT STREAM KIND
// =============================================================================

/// The type of input stream from which reference prices are derived.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum InputStreamKind {
    /// Top-of-book updates (best bid/ask).
    BookTopOfBook = 0,
    /// Mark price stream (futures).
    MarkPriceStream = 1,
    /// Trade stream (last trade).
    TradeStream = 2,
}

impl InputStreamKind {
    /// Check if this stream kind can produce the given price kind.
    pub fn supports_price_kind(&self, kind: PriceKind) -> bool {
        match (self, kind) {
            (Self::BookTopOfBook, PriceKind::Mid) => true,
            (Self::BookTopOfBook, PriceKind::Last) => false, // TOB doesn't have trades
            (Self::BookTopOfBook, PriceKind::Mark) => false, // TOB doesn't have mark
            (Self::MarkPriceStream, PriceKind::Mark) => true,
            (Self::MarkPriceStream, _) => false,
            (Self::TradeStream, PriceKind::Last) => true,
            (Self::TradeStream, _) => false,
        }
    }
}

// =============================================================================
// ROUNDING RULE
// =============================================================================

/// Rounding rule for fixed-point calculations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RoundingRule {
    /// Round half away from zero (standard rounding).
    HalfAwayFromZero,
    /// Round half to even (bankers rounding) - deterministic for .5 cases.
    HalfToEven,
    /// Always round down (floor).
    Floor,
    /// Always round up (ceiling).
    Ceiling,
}

impl Default for RoundingRule {
    fn default() -> Self {
        Self::HalfToEven // Bankers rounding is most deterministic
    }
}

// =============================================================================
// FALLBACK REASON
// =============================================================================

/// Reason why a fallback was triggered.
///
/// Stable discriminants ensure consistent serialization across versions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum FallbackReason {
    /// Primary source had missing bid price.
    MissingBid = 0,
    /// Primary source had missing ask price.
    MissingAsk = 1,
    /// Primary source had crossed book (bid >= ask).
    CrossedBook = 2,
    /// Primary source data was stale beyond threshold.
    StaleData = 3,
    /// Primary source had zero or negative price.
    InvalidPrice = 4,
    /// Primary source price was outside plausible bounds.
    OutlierPrice = 5,
    /// Primary source stream was not available at sampling time.
    StreamUnavailable = 6,
    /// Carry-forward from last valid reference (explicit).
    CarryForward = 7,
}

impl FallbackReason {
    /// Human-readable description.
    pub fn description(&self) -> &'static str {
        match self {
            Self::MissingBid => "bid price missing",
            Self::MissingAsk => "ask price missing",
            Self::CrossedBook => "crossed book (bid >= ask)",
            Self::StaleData => "data stale beyond threshold",
            Self::InvalidPrice => "zero or negative price",
            Self::OutlierPrice => "price outside plausible bounds",
            Self::StreamUnavailable => "stream not available",
            Self::CarryForward => "carry-forward from last valid",
        }
    }
}

// =============================================================================
// FALLBACK STEP
// =============================================================================

/// A single step in the fallback chain.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FallbackStep {
    /// The price kind to try.
    pub price_kind: PriceKind,
    /// The input stream to use.
    pub input_stream: InputStreamKind,
    /// Venue to use (None = same as primary).
    pub venue: Option<ReferenceVenue>,
    /// Conditions that trigger progression to next fallback.
    pub trigger_conditions: Vec<FallbackReason>,
}

// =============================================================================
// TERMINAL FALLBACK ACTION
// =============================================================================

/// What to do when the entire fallback chain is exhausted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TerminalFallbackAction {
    /// Fail the tick emission (hard failure).
    Fail,
    /// Carry forward the last valid reference price strictly before this time.
    CarryForwardLastValid,
}

impl Default for TerminalFallbackAction {
    fn default() -> Self {
        Self::Fail // Safe default: fail explicitly
    }
}

// =============================================================================
// STALENESS THRESHOLDS
// =============================================================================

/// Staleness configuration for reference data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct StalenessConfig {
    /// Maximum age of book quote before considered stale (nanoseconds).
    pub max_quote_age_ns: i64,
    /// Maximum age of trade before considered stale (nanoseconds).
    pub max_trade_age_ns: i64,
    /// Maximum age of mark price before considered stale (nanoseconds).
    pub max_mark_age_ns: i64,
}

impl Default for StalenessConfig {
    fn default() -> Self {
        Self {
            max_quote_age_ns: 5 * NS_PER_SEC,  // 5 seconds
            max_trade_age_ns: 30 * NS_PER_SEC, // 30 seconds
            max_mark_age_ns: 10 * NS_PER_SEC,  // 10 seconds
        }
    }
}

// =============================================================================
// OUTLIER BOUNDS
// =============================================================================

/// Plausibility bounds for outlier detection.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct OutlierBounds {
    /// Minimum plausible price (USD).
    pub min_price: f64,
    /// Maximum plausible price (USD).
    pub max_price: f64,
    /// Maximum single-tick change (as fraction of price).
    pub max_tick_change_pct: f64,
}

impl Default for OutlierBounds {
    fn default() -> Self {
        Self {
            min_price: 0.0001,           // $0.0001 minimum
            max_price: 1_000_000_000.0,  // $1B maximum
            max_tick_change_pct: 0.50,   // 50% max single-tick change
        }
    }
}

impl OutlierBounds {
    /// Check if a price is within bounds.
    pub fn is_valid(&self, price_fp: i64) -> bool {
        let price = price_fp as f64 / FP_SCALE as f64;
        price >= self.min_price && price <= self.max_price
    }

    /// Check if a price change is within bounds.
    pub fn is_change_valid(&self, old_price_fp: i64, new_price_fp: i64) -> bool {
        if old_price_fp == 0 {
            return true; // No prior price to compare
        }
        let old = old_price_fp as f64;
        let new = new_price_fp as f64;
        let change_pct = ((new - old) / old).abs();
        change_pct <= self.max_tick_change_pct
    }
}

// =============================================================================
// SETTLEMENT REFERENCE MAPPING
// =============================================================================

/// Complete settlement reference mapping specification for 15M Up/Down markets.
///
/// This struct is the SINGLE SOURCE OF TRUTH for how Binance events become
/// settlement reference prices. It is stored in:
/// - Dataset metadata (when recording)
/// - Run fingerprint (when backtesting)
///
/// Changes to this mapping require a version increment.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SettlementReferenceMapping15m {
    /// Specification version (for compatibility checking).
    pub spec_version: u32,
    /// Primary reference venue.
    pub primary_venue: ReferenceVenue,
    /// Primary price kind.
    pub primary_price_kind: PriceKind,
    /// Primary input stream kind.
    pub primary_input_stream: InputStreamKind,
    /// Symbol mapping for each asset.
    pub symbol_mapping: SymbolMapping,
    /// Rounding rule for fixed-point calculations.
    pub rounding_rule: RoundingRule,
    /// Fallback chain (ordered, first match wins).
    pub fallback_chain: Vec<FallbackStep>,
    /// Terminal action when fallback chain exhausted.
    pub terminal_fallback: TerminalFallbackAction,
    /// Staleness thresholds.
    pub staleness_config: StalenessConfig,
    /// Outlier detection bounds (optional).
    pub outlier_bounds: Option<OutlierBounds>,
    /// Window boundary epsilon (nanoseconds) for sampling.
    /// Reference tick at time T is valid for window boundary if |T - boundary| <= epsilon.
    pub boundary_epsilon_ns: i64,
    /// Whether carry-forward is allowed for production-grade runs.
    pub allow_carry_forward_production: bool,
    /// Maximum carry-forward age (nanoseconds) before triggering failure.
    pub max_carry_forward_age_ns: i64,
}

/// Symbol mapping for all supported assets.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SymbolMapping {
    pub btc: String,
    pub eth: String,
    pub sol: String,
    pub xrp: String,
}

impl SymbolMapping {
    /// Get symbol for an asset.
    pub fn get(&self, asset: UpDownAsset) -> &str {
        match asset {
            UpDownAsset::BTC => &self.btc,
            UpDownAsset::ETH => &self.eth,
            UpDownAsset::SOL => &self.sol,
            UpDownAsset::XRP => &self.xrp,
        }
    }

    /// Check if all assets are covered.
    pub fn is_complete(&self) -> bool {
        !self.btc.is_empty() && !self.eth.is_empty() && 
        !self.sol.is_empty() && !self.xrp.is_empty()
    }
}

impl Default for SymbolMapping {
    fn default() -> Self {
        Self {
            btc: "BTCUSDT".to_string(),
            eth: "ETHUSDT".to_string(),
            sol: "SOLUSDT".to_string(),
            xrp: "XRPUSDT".to_string(),
        }
    }
}

impl Default for SettlementReferenceMapping15m {
    fn default() -> Self {
        Self::binance_spot_mid()
    }
}

impl SettlementReferenceMapping15m {
    /// Production-grade mapping: Binance spot mid-price.
    ///
    /// This is the canonical mapping for Polymarket 15M Up/Down settlement.
    pub fn binance_spot_mid() -> Self {
        Self {
            spec_version: SETTLEMENT_REFERENCE_MAPPING_VERSION,
            primary_venue: ReferenceVenue::BinanceSpot,
            primary_price_kind: PriceKind::Mid,
            primary_input_stream: InputStreamKind::BookTopOfBook,
            symbol_mapping: SymbolMapping::default(),
            rounding_rule: RoundingRule::HalfToEven,
            fallback_chain: vec![
                // Fallback 1: Last trade price if mid unavailable
                FallbackStep {
                    price_kind: PriceKind::Last,
                    input_stream: InputStreamKind::TradeStream,
                    venue: None, // Same venue
                    trigger_conditions: vec![
                        FallbackReason::MissingBid,
                        FallbackReason::MissingAsk,
                        FallbackReason::CrossedBook,
                    ],
                },
            ],
            terminal_fallback: TerminalFallbackAction::CarryForwardLastValid,
            staleness_config: StalenessConfig::default(),
            outlier_bounds: Some(OutlierBounds::default()),
            boundary_epsilon_ns: 100_000_000, // 100ms epsilon
            allow_carry_forward_production: false,
            max_carry_forward_age_ns: 60 * NS_PER_SEC, // 60 seconds max
        }
    }

    /// Alternative mapping: Binance spot last trade price.
    pub fn binance_spot_last() -> Self {
        Self {
            spec_version: SETTLEMENT_REFERENCE_MAPPING_VERSION,
            primary_venue: ReferenceVenue::BinanceSpot,
            primary_price_kind: PriceKind::Last,
            primary_input_stream: InputStreamKind::TradeStream,
            symbol_mapping: SymbolMapping::default(),
            rounding_rule: RoundingRule::HalfToEven,
            fallback_chain: vec![],
            terminal_fallback: TerminalFallbackAction::Fail,
            staleness_config: StalenessConfig::default(),
            outlier_bounds: Some(OutlierBounds::default()),
            boundary_epsilon_ns: 100_000_000,
            allow_carry_forward_production: false,
            max_carry_forward_age_ns: 60 * NS_PER_SEC,
        }
    }

    /// Alternative mapping: Binance futures mark price.
    pub fn binance_futures_mark() -> Self {
        Self {
            spec_version: SETTLEMENT_REFERENCE_MAPPING_VERSION,
            primary_venue: ReferenceVenue::BinanceFutures,
            primary_price_kind: PriceKind::Mark,
            primary_input_stream: InputStreamKind::MarkPriceStream,
            symbol_mapping: SymbolMapping::default(),
            rounding_rule: RoundingRule::HalfToEven,
            fallback_chain: vec![],
            terminal_fallback: TerminalFallbackAction::Fail,
            staleness_config: StalenessConfig::default(),
            outlier_bounds: Some(OutlierBounds::default()),
            boundary_epsilon_ns: 100_000_000,
            allow_carry_forward_production: false,
            max_carry_forward_age_ns: 60 * NS_PER_SEC,
        }
    }

    /// Compute a deterministic hash of this mapping for fingerprinting.
    pub fn fingerprint_hash(&self) -> u64 {
        let mut hasher = DefaultHasher::new();
        self.spec_version.hash(&mut hasher);
        (self.primary_venue as u8).hash(&mut hasher);
        (self.primary_price_kind as u8).hash(&mut hasher);
        (self.primary_input_stream as u8).hash(&mut hasher);
        self.symbol_mapping.hash(&mut hasher);
        self.rounding_rule.hash(&mut hasher);
        for step in &self.fallback_chain {
            (step.price_kind as u8).hash(&mut hasher);
            (step.input_stream as u8).hash(&mut hasher);
            step.venue.map(|v| v as u8).hash(&mut hasher);
            for cond in &step.trigger_conditions {
                (*cond as u8).hash(&mut hasher);
            }
        }
        (self.terminal_fallback as u8).hash(&mut hasher);
        self.staleness_config.max_quote_age_ns.hash(&mut hasher);
        self.boundary_epsilon_ns.hash(&mut hasher);
        self.allow_carry_forward_production.hash(&mut hasher);
        hasher.finish()
    }

    /// Format as compact string for logging.
    pub fn format_compact(&self) -> String {
        format!(
            "v{} {}:{} via {} fallback={} terminal={:?}",
            self.spec_version,
            self.primary_venue,
            self.primary_price_kind,
            self.primary_input_stream as u8,
            self.fallback_chain.len(),
            self.terminal_fallback,
        )
    }
}

// =============================================================================
// MAPPING INVARIANT VALIDATION
// =============================================================================

/// Result of mapping invariant validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MappingValidationResult {
    pub valid: bool,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

impl MappingValidationResult {
    fn new() -> Self {
        Self {
            valid: true,
            errors: Vec::new(),
            warnings: Vec::new(),
        }
    }

    fn error(&mut self, msg: String) {
        self.valid = false;
        self.errors.push(msg);
    }

    fn warn(&mut self, msg: String) {
        self.warnings.push(msg);
    }
}

impl SettlementReferenceMapping15m {
    /// Validate mapping invariants at startup.
    ///
    /// Returns errors if invariants are violated.
    pub fn validate(&self) -> MappingValidationResult {
        let mut result = MappingValidationResult::new();

        // Invariant 1: Exactly one primary source
        // (enforced by struct, but verify stream supports price kind)
        if !self.primary_input_stream.supports_price_kind(self.primary_price_kind) {
            result.error(format!(
                "Primary input stream {:?} does not support price kind {:?}",
                self.primary_input_stream, self.primary_price_kind
            ));
        }

        // Invariant 2: Fallback chain contains no cycles
        let mut seen_kinds = vec![(self.primary_price_kind, self.primary_input_stream)];
        for (i, step) in self.fallback_chain.iter().enumerate() {
            let key = (step.price_kind, step.input_stream);
            if seen_kinds.contains(&key) {
                result.error(format!(
                    "Fallback chain has cycle at index {}: {:?}",
                    i, key
                ));
            }
            seen_kinds.push(key);

            // Verify stream supports price kind
            if !step.input_stream.supports_price_kind(step.price_kind) {
                result.error(format!(
                    "Fallback step {} stream {:?} does not support price kind {:?}",
                    i, step.input_stream, step.price_kind
                ));
            }
        }

        // Invariant 3: Fallback chain ends with explicit action
        // (enforced by terminal_fallback field)

        // Invariant 4: Symbol mapping covers all assets
        if !self.symbol_mapping.is_complete() {
            result.error("Symbol mapping incomplete - all assets must be mapped".to_string());
        }

        // Invariant 5: Staleness thresholds are positive
        if self.staleness_config.max_quote_age_ns <= 0 {
            result.error("max_quote_age_ns must be positive".to_string());
        }
        if self.staleness_config.max_trade_age_ns <= 0 {
            result.error("max_trade_age_ns must be positive".to_string());
        }

        // Invariant 6: Boundary epsilon is non-negative
        if self.boundary_epsilon_ns < 0 {
            result.error("boundary_epsilon_ns must be non-negative".to_string());
        }

        // Invariant 7: If carry-forward not allowed for production, terminal must not be CarryForward
        if !self.allow_carry_forward_production {
            if self.terminal_fallback == TerminalFallbackAction::CarryForwardLastValid {
                result.warn(
                    "terminal_fallback is CarryForwardLastValid but carry-forward \
                     not allowed for production - will downgrade trust level".to_string()
                );
            }
        }

        // Invariant 8: Max carry-forward age is positive
        if self.max_carry_forward_age_ns <= 0 {
            result.error("max_carry_forward_age_ns must be positive".to_string());
        }

        // Invariant 9: Outlier bounds are valid if present
        if let Some(ref bounds) = self.outlier_bounds {
            if bounds.min_price < 0.0 {
                result.error("outlier_bounds.min_price must be non-negative".to_string());
            }
            if bounds.max_price <= bounds.min_price {
                result.error("outlier_bounds.max_price must be greater than min_price".to_string());
            }
            if bounds.max_tick_change_pct <= 0.0 || bounds.max_tick_change_pct > 10.0 {
                result.warn("outlier_bounds.max_tick_change_pct should be between 0 and 10".to_string());
            }
        }

        result
    }
}

// =============================================================================
// SETTLEMENT REFERENCE TICK
// =============================================================================

/// A single settlement reference tick with full provenance.
///
/// This is emitted by the transformation from Binance events and contains
/// all information needed to audit the settlement calculation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettlementReferenceTick {
    /// Mapping version that produced this tick.
    pub mapping_version: u32,
    /// Venue that provided the data.
    pub venue_id: ReferenceVenue,
    /// Symbol (e.g., "BTCUSDT").
    pub symbol: String,
    /// Asset this tick is for.
    pub asset: UpDownAsset,
    /// Price kind actually used (may differ from primary if fallback occurred).
    pub price_kind_used: PriceKind,
    /// Fixed-point price (scaled by FP_SCALE).
    pub price_fp: i64,
    /// Visible timestamp when this tick becomes observable.
    pub visible_ts_ns: Nanos,
    /// Ingest timestamp when the source event was recorded.
    pub ingest_ts_ns: Nanos,
    /// Exchange timestamp from venue (if available).
    pub exchange_ts_ns: Option<Nanos>,
    /// Source event sequence number (for correlation).
    pub source_seq: u64,
    /// Fallback reason if primary source was not used.
    pub fallback_reason: Option<FallbackReason>,
    /// Additional raw data for audit (bid/ask if mid, trade price if last).
    pub raw_bid_fp: Option<i64>,
    pub raw_ask_fp: Option<i64>,
    pub raw_trade_price_fp: Option<i64>,
}

impl SettlementReferenceTick {
    /// Get price as floating point.
    pub fn price(&self) -> f64 {
        self.price_fp as f64 / FP_SCALE as f64
    }

    /// Check if this tick used a fallback.
    pub fn used_fallback(&self) -> bool {
        self.fallback_reason.is_some()
    }

    /// Compute fingerprint for this tick.
    pub fn fingerprint(&self) -> u64 {
        let mut hasher = DefaultHasher::new();
        self.mapping_version.hash(&mut hasher);
        (self.venue_id as u8).hash(&mut hasher);
        self.symbol.hash(&mut hasher);
        (self.asset as u8).hash(&mut hasher);
        (self.price_kind_used as u8).hash(&mut hasher);
        self.price_fp.hash(&mut hasher);
        self.visible_ts_ns.hash(&mut hasher);
        self.source_seq.hash(&mut hasher);
        self.fallback_reason.map(|r| r as u8).hash(&mut hasher);
        hasher.finish()
    }
}

// =============================================================================
// BINANCE INPUT EVENTS
// =============================================================================

/// Raw Binance book top-of-book update for transformation.
#[derive(Debug, Clone)]
pub struct BinanceBookUpdate {
    pub symbol: String,
    pub bid_price: Option<f64>,
    pub bid_qty: Option<f64>,
    pub ask_price: Option<f64>,
    pub ask_qty: Option<f64>,
    pub exchange_ts_ns: Option<Nanos>,
    pub ingest_ts_ns: Nanos,
    pub visible_ts_ns: Nanos,
    pub source_seq: u64,
}

/// Raw Binance trade for transformation.
#[derive(Debug, Clone)]
pub struct BinanceTrade {
    pub symbol: String,
    pub price: f64,
    pub qty: f64,
    pub exchange_ts_ns: Option<Nanos>,
    pub ingest_ts_ns: Nanos,
    pub visible_ts_ns: Nanos,
    pub source_seq: u64,
}

/// Raw Binance mark price for transformation.
#[derive(Debug, Clone)]
pub struct BinanceMarkPrice {
    pub symbol: String,
    pub mark_price: f64,
    pub exchange_ts_ns: Option<Nanos>,
    pub ingest_ts_ns: Nanos,
    pub visible_ts_ns: Nanos,
    pub source_seq: u64,
}

// =============================================================================
// TRANSFORMATION ERROR
// =============================================================================

/// Error during reference tick transformation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TransformationError {
    /// No valid price could be derived (fallback exhausted).
    NoPriceAvailable {
        symbol: String,
        visible_ts_ns: Nanos,
        reason: String,
    },
    /// Price failed outlier check.
    OutlierPrice {
        symbol: String,
        price_fp: i64,
        reason: String,
    },
    /// Data was stale.
    StaleData {
        symbol: String,
        age_ns: i64,
        threshold_ns: i64,
    },
    /// Unknown symbol.
    UnknownSymbol {
        symbol: String,
    },
}

impl std::fmt::Display for TransformationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoPriceAvailable { symbol, visible_ts_ns, reason } => {
                write!(f, "No price available for {} at {}: {}", symbol, visible_ts_ns, reason)
            }
            Self::OutlierPrice { symbol, price_fp, reason } => {
                write!(f, "Outlier price for {} ({}): {}", symbol, price_fp, reason)
            }
            Self::StaleData { symbol, age_ns, threshold_ns } => {
                write!(f, "Stale data for {}: age={}ns > threshold={}ns", symbol, age_ns, threshold_ns)
            }
            Self::UnknownSymbol { symbol } => {
                write!(f, "Unknown symbol: {}", symbol)
            }
        }
    }
}

impl std::error::Error for TransformationError {}

// =============================================================================
// REFERENCE TICK TRANSFORMER
// =============================================================================

/// State for a single symbol's reference price tracking.
#[derive(Debug, Clone)]
struct SymbolState {
    /// Last valid reference tick for carry-forward.
    last_valid_tick: Option<SettlementReferenceTick>,
    /// Last book update.
    last_book: Option<BinanceBookUpdate>,
    /// Last trade.
    last_trade: Option<BinanceTrade>,
    /// Last mark price.
    last_mark: Option<BinanceMarkPrice>,
}

impl Default for SymbolState {
    fn default() -> Self {
        Self {
            last_valid_tick: None,
            last_book: None,
            last_trade: None,
            last_mark: None,
        }
    }
}

/// Transforms Binance events into settlement reference ticks.
///
/// This is the PURE, DETERMINISTIC transformation function that:
/// 1. Applies the mapping to select price kind and venue
/// 2. Computes fixed-point prices with specified rounding
/// 3. Applies fallback chain if primary source unavailable
/// 4. Records full provenance for each tick
pub struct ReferenceTickTransformer {
    mapping: SettlementReferenceMapping15m,
    /// Per-symbol state.
    states: std::collections::HashMap<String, SymbolState>,
    /// Statistics.
    pub stats: TransformerStats,
}

/// Transformation statistics.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TransformerStats {
    pub ticks_emitted: u64,
    pub ticks_from_primary: u64,
    pub ticks_from_fallback: u64,
    pub ticks_from_carry_forward: u64,
    pub ticks_failed: u64,
    pub outliers_rejected: u64,
    pub stale_data_rejected: u64,
}

impl ReferenceTickTransformer {
    /// Create a new transformer with the given mapping.
    pub fn new(mapping: SettlementReferenceMapping15m) -> Self {
        Self {
            mapping,
            states: std::collections::HashMap::new(),
            stats: TransformerStats::default(),
        }
    }

    /// Get the mapping.
    pub fn mapping(&self) -> &SettlementReferenceMapping15m {
        &self.mapping
    }

    /// Process a book update and potentially emit a reference tick.
    pub fn process_book_update(
        &mut self,
        update: BinanceBookUpdate,
    ) -> Result<Option<SettlementReferenceTick>, TransformationError> {
        let symbol = update.symbol.clone();
        let state = self.states.entry(symbol.clone()).or_default();
        state.last_book = Some(update.clone());

        // Only emit tick if this is the primary input stream
        if self.mapping.primary_input_stream == InputStreamKind::BookTopOfBook {
            self.try_emit_tick(&symbol, update.visible_ts_ns, update.ingest_ts_ns, 
                               update.exchange_ts_ns, update.source_seq)
        } else {
            Ok(None)
        }
    }

    /// Process a trade and potentially emit a reference tick.
    pub fn process_trade(
        &mut self,
        trade: BinanceTrade,
    ) -> Result<Option<SettlementReferenceTick>, TransformationError> {
        let symbol = trade.symbol.clone();
        let state = self.states.entry(symbol.clone()).or_default();
        state.last_trade = Some(trade.clone());

        // Only emit tick if this is the primary input stream
        if self.mapping.primary_input_stream == InputStreamKind::TradeStream {
            self.try_emit_tick(&symbol, trade.visible_ts_ns, trade.ingest_ts_ns,
                               trade.exchange_ts_ns, trade.source_seq)
        } else {
            Ok(None)
        }
    }

    /// Process a mark price and potentially emit a reference tick.
    pub fn process_mark_price(
        &mut self,
        mark: BinanceMarkPrice,
    ) -> Result<Option<SettlementReferenceTick>, TransformationError> {
        let symbol = mark.symbol.clone();
        let state = self.states.entry(symbol.clone()).or_default();
        state.last_mark = Some(mark.clone());

        // Only emit tick if this is the primary input stream
        if self.mapping.primary_input_stream == InputStreamKind::MarkPriceStream {
            self.try_emit_tick(&symbol, mark.visible_ts_ns, mark.ingest_ts_ns,
                               mark.exchange_ts_ns, mark.source_seq)
        } else {
            Ok(None)
        }
    }

    /// Try to emit a reference tick at the given time.
    fn try_emit_tick(
        &mut self,
        symbol: &str,
        visible_ts_ns: Nanos,
        ingest_ts_ns: Nanos,
        exchange_ts_ns: Option<Nanos>,
        source_seq: u64,
    ) -> Result<Option<SettlementReferenceTick>, TransformationError> {
        // Determine asset from symbol
        let asset = self.asset_from_symbol(symbol)?;
        
        let state = match self.states.get(symbol) {
            Some(s) => s.clone(),
            None => return Ok(None),
        };

        // Try primary source first
        let primary_result = self.try_price_from_kind(
            &state,
            self.mapping.primary_price_kind,
            self.mapping.primary_input_stream,
            visible_ts_ns,
        );

        match primary_result {
            Ok((price_fp, raw_bid, raw_ask, raw_trade)) => {
                // Validate against outlier bounds
                if let Some(ref bounds) = self.mapping.outlier_bounds {
                    if !bounds.is_valid(price_fp) {
                        self.stats.outliers_rejected += 1;
                        // Try fallback chain
                        return self.try_fallback_chain(
                            symbol, asset, &state, visible_ts_ns, ingest_ts_ns, 
                            exchange_ts_ns, source_seq, FallbackReason::OutlierPrice
                        );
                    }
                    // Check against last valid tick
                    if let Some(ref last) = state.last_valid_tick {
                        if !bounds.is_change_valid(last.price_fp, price_fp) {
                            self.stats.outliers_rejected += 1;
                            return self.try_fallback_chain(
                                symbol, asset, &state, visible_ts_ns, ingest_ts_ns,
                                exchange_ts_ns, source_seq, FallbackReason::OutlierPrice
                            );
                        }
                    }
                }

                let tick = SettlementReferenceTick {
                    mapping_version: self.mapping.spec_version,
                    venue_id: self.mapping.primary_venue,
                    symbol: symbol.to_string(),
                    asset,
                    price_kind_used: self.mapping.primary_price_kind,
                    price_fp,
                    visible_ts_ns,
                    ingest_ts_ns,
                    exchange_ts_ns,
                    source_seq,
                    fallback_reason: None,
                    raw_bid_fp: raw_bid,
                    raw_ask_fp: raw_ask,
                    raw_trade_price_fp: raw_trade,
                };

                // Update state
                let state = self.states.get_mut(symbol).unwrap();
                state.last_valid_tick = Some(tick.clone());

                self.stats.ticks_emitted += 1;
                self.stats.ticks_from_primary += 1;

                Ok(Some(tick))
            }
            Err(reason) => {
                self.try_fallback_chain(
                    symbol, asset, &state, visible_ts_ns, ingest_ts_ns,
                    exchange_ts_ns, source_seq, reason
                )
            }
        }
    }

    /// Try to get price from a specific kind/stream.
    fn try_price_from_kind(
        &self,
        state: &SymbolState,
        price_kind: PriceKind,
        input_stream: InputStreamKind,
        visible_ts_ns: Nanos,
    ) -> Result<(i64, Option<i64>, Option<i64>, Option<i64>), FallbackReason> {
        match (price_kind, input_stream) {
            (PriceKind::Mid, InputStreamKind::BookTopOfBook) => {
                let book = state.last_book.as_ref().ok_or(FallbackReason::StreamUnavailable)?;
                
                // Check staleness
                let age = visible_ts_ns - book.ingest_ts_ns;
                if age > self.mapping.staleness_config.max_quote_age_ns {
                    return Err(FallbackReason::StaleData);
                }

                let bid = book.bid_price.ok_or(FallbackReason::MissingBid)?;
                let ask = book.ask_price.ok_or(FallbackReason::MissingAsk)?;

                if bid <= 0.0 || ask <= 0.0 {
                    return Err(FallbackReason::InvalidPrice);
                }
                if bid >= ask {
                    return Err(FallbackReason::CrossedBook);
                }

                let bid_fp = float_to_fp(bid);
                let ask_fp = float_to_fp(ask);
                let mid_fp = compute_mid_fp(bid_fp, ask_fp, self.mapping.rounding_rule);

                Ok((mid_fp, Some(bid_fp), Some(ask_fp), None))
            }
            (PriceKind::Last, InputStreamKind::TradeStream) => {
                let trade = state.last_trade.as_ref().ok_or(FallbackReason::StreamUnavailable)?;

                let age = visible_ts_ns - trade.ingest_ts_ns;
                if age > self.mapping.staleness_config.max_trade_age_ns {
                    return Err(FallbackReason::StaleData);
                }

                if trade.price <= 0.0 {
                    return Err(FallbackReason::InvalidPrice);
                }

                let price_fp = float_to_fp(trade.price);
                Ok((price_fp, None, None, Some(price_fp)))
            }
            (PriceKind::Mark, InputStreamKind::MarkPriceStream) => {
                let mark = state.last_mark.as_ref().ok_or(FallbackReason::StreamUnavailable)?;

                let age = visible_ts_ns - mark.ingest_ts_ns;
                if age > self.mapping.staleness_config.max_mark_age_ns {
                    return Err(FallbackReason::StaleData);
                }

                if mark.mark_price <= 0.0 {
                    return Err(FallbackReason::InvalidPrice);
                }

                let price_fp = float_to_fp(mark.mark_price);
                Ok((price_fp, None, None, None))
            }
            _ => Err(FallbackReason::StreamUnavailable),
        }
    }

    /// Try the fallback chain.
    fn try_fallback_chain(
        &mut self,
        symbol: &str,
        asset: UpDownAsset,
        state: &SymbolState,
        visible_ts_ns: Nanos,
        ingest_ts_ns: Nanos,
        exchange_ts_ns: Option<Nanos>,
        source_seq: u64,
        initial_reason: FallbackReason,
    ) -> Result<Option<SettlementReferenceTick>, TransformationError> {
        // Try each fallback step
        for step in &self.mapping.fallback_chain {
            if step.trigger_conditions.contains(&initial_reason) {
                let result = self.try_price_from_kind(
                    state,
                    step.price_kind,
                    step.input_stream,
                    visible_ts_ns,
                );

                if let Ok((price_fp, raw_bid, raw_ask, raw_trade)) = result {
                    // Validate against outlier bounds
                    if let Some(ref bounds) = self.mapping.outlier_bounds {
                        if !bounds.is_valid(price_fp) {
                            continue; // Try next fallback
                        }
                    }

                    let venue = step.venue.unwrap_or(self.mapping.primary_venue);

                    let tick = SettlementReferenceTick {
                        mapping_version: self.mapping.spec_version,
                        venue_id: venue,
                        symbol: symbol.to_string(),
                        asset,
                        price_kind_used: step.price_kind,
                        price_fp,
                        visible_ts_ns,
                        ingest_ts_ns,
                        exchange_ts_ns,
                        source_seq,
                        fallback_reason: Some(initial_reason),
                        raw_bid_fp: raw_bid,
                        raw_ask_fp: raw_ask,
                        raw_trade_price_fp: raw_trade,
                    };

                    let state = self.states.get_mut(symbol).unwrap();
                    state.last_valid_tick = Some(tick.clone());

                    self.stats.ticks_emitted += 1;
                    self.stats.ticks_from_fallback += 1;

                    return Ok(Some(tick));
                }
            }
        }

        // Fallback chain exhausted - try terminal action
        match self.mapping.terminal_fallback {
            TerminalFallbackAction::CarryForwardLastValid => {
                if let Some(ref last) = state.last_valid_tick {
                    let age = visible_ts_ns - last.visible_ts_ns;
                    if age <= self.mapping.max_carry_forward_age_ns {
                        let tick = SettlementReferenceTick {
                            mapping_version: self.mapping.spec_version,
                            venue_id: last.venue_id,
                            symbol: symbol.to_string(),
                            asset,
                            price_kind_used: last.price_kind_used,
                            price_fp: last.price_fp,
                            visible_ts_ns,
                            ingest_ts_ns,
                            exchange_ts_ns,
                            source_seq,
                            fallback_reason: Some(FallbackReason::CarryForward),
                            raw_bid_fp: last.raw_bid_fp,
                            raw_ask_fp: last.raw_ask_fp,
                            raw_trade_price_fp: last.raw_trade_price_fp,
                        };

                        self.stats.ticks_emitted += 1;
                        self.stats.ticks_from_carry_forward += 1;

                        return Ok(Some(tick));
                    }
                }
                // Carry-forward not available or too old
                self.stats.ticks_failed += 1;
                Err(TransformationError::NoPriceAvailable {
                    symbol: symbol.to_string(),
                    visible_ts_ns,
                    reason: "carry-forward unavailable or too old".to_string(),
                })
            }
            TerminalFallbackAction::Fail => {
                self.stats.ticks_failed += 1;
                Err(TransformationError::NoPriceAvailable {
                    symbol: symbol.to_string(),
                    visible_ts_ns,
                    reason: format!("fallback chain exhausted (initial: {:?})", initial_reason),
                })
            }
        }
    }

    /// Determine asset from symbol.
    fn asset_from_symbol(&self, symbol: &str) -> Result<UpDownAsset, TransformationError> {
        if self.mapping.symbol_mapping.btc == symbol {
            Ok(UpDownAsset::BTC)
        } else if self.mapping.symbol_mapping.eth == symbol {
            Ok(UpDownAsset::ETH)
        } else if self.mapping.symbol_mapping.sol == symbol {
            Ok(UpDownAsset::SOL)
        } else if self.mapping.symbol_mapping.xrp == symbol {
            Ok(UpDownAsset::XRP)
        } else {
            Err(TransformationError::UnknownSymbol {
                symbol: symbol.to_string(),
            })
        }
    }

    /// Get the last valid tick for a symbol.
    pub fn last_valid_tick(&self, symbol: &str) -> Option<&SettlementReferenceTick> {
        self.states.get(symbol).and_then(|s| s.last_valid_tick.as_ref())
    }

    /// Reset state (for testing).
    pub fn reset(&mut self) {
        self.states.clear();
        self.stats = TransformerStats::default();
    }
}

// =============================================================================
// FIXED-POINT HELPERS
// =============================================================================

/// Convert floating-point price to fixed-point.
#[inline]
pub fn float_to_fp(price: f64) -> i64 {
    (price * FP_SCALE as f64).round() as i64
}

/// Convert fixed-point price to floating-point.
#[inline]
pub fn fp_to_float(price_fp: i64) -> f64 {
    price_fp as f64 / FP_SCALE as f64
}

/// Compute mid-price in fixed-point with specified rounding.
pub fn compute_mid_fp(bid_fp: i64, ask_fp: i64, rounding: RoundingRule) -> i64 {
    let sum = bid_fp + ask_fp;
    match rounding {
        RoundingRule::HalfAwayFromZero => {
            // Standard rounding: (sum + 1) / 2 rounds up on 0.5
            (sum + 1) / 2
        }
        RoundingRule::HalfToEven => {
            // Bankers rounding: round 0.5 to nearest even
            let half = sum / 2;
            let remainder = sum % 2;
            if remainder == 0 {
                half
            } else if half % 2 == 0 {
                half // Already even, round down
            } else {
                half + 1 // Odd, round up to even
            }
        }
        RoundingRule::Floor => {
            sum / 2
        }
        RoundingRule::Ceiling => {
            (sum + 1) / 2
        }
    }
}

// =============================================================================
// TESTS
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mapping_validation_valid() {
        let mapping = SettlementReferenceMapping15m::binance_spot_mid();
        let result = mapping.validate();
        assert!(result.valid, "Errors: {:?}", result.errors);
    }

    #[test]
    fn test_mapping_validation_invalid_stream() {
        let mut mapping = SettlementReferenceMapping15m::binance_spot_mid();
        // Set invalid combination: Mid from TradeStream
        mapping.primary_input_stream = InputStreamKind::TradeStream;
        
        let result = mapping.validate();
        assert!(!result.valid);
        assert!(result.errors.iter().any(|e| e.contains("does not support")));
    }

    #[test]
    fn test_mapping_validation_incomplete_symbols() {
        let mut mapping = SettlementReferenceMapping15m::binance_spot_mid();
        mapping.symbol_mapping.btc = String::new();
        
        let result = mapping.validate();
        assert!(!result.valid);
        assert!(result.errors.iter().any(|e| e.contains("incomplete")));
    }

    #[test]
    fn test_mapping_fingerprint_stability() {
        let mapping1 = SettlementReferenceMapping15m::binance_spot_mid();
        let mapping2 = SettlementReferenceMapping15m::binance_spot_mid();
        
        assert_eq!(mapping1.fingerprint_hash(), mapping2.fingerprint_hash());
    }

    #[test]
    fn test_mapping_fingerprint_changes() {
        let mapping1 = SettlementReferenceMapping15m::binance_spot_mid();
        let mut mapping2 = SettlementReferenceMapping15m::binance_spot_mid();
        mapping2.primary_price_kind = PriceKind::Last;
        
        assert_ne!(mapping1.fingerprint_hash(), mapping2.fingerprint_hash());
    }

    #[test]
    fn test_fixed_point_conversion() {
        let price = 45678.12345678;
        let fp = float_to_fp(price);
        let back = fp_to_float(fp);
        
        // Should be accurate to 8 decimal places
        assert!((price - back).abs() < 1e-8);
    }

    #[test]
    fn test_mid_price_bankers_rounding() {
        // Test case: bid=100, ask=101 -> mid should be 100.5 -> rounds to 100 (even)
        let bid_fp = 100 * FP_SCALE;
        let ask_fp = 101 * FP_SCALE;
        let mid = compute_mid_fp(bid_fp, ask_fp, RoundingRule::HalfToEven);
        
        // 100.5 with bankers rounding -> 100 (round to even)
        // Actually: (100 + 101) / 2 = 100.5 in integer arithmetic:
        // sum = 20100000000, half = 10050000000
        // 10050000000 / FP_SCALE = 100.5
        // With HalfToEven: 100.5 -> should round to 100 or 101?
        // In fixed-point: sum = 20100000000, sum/2 = 10050000000
        // remainder = 0 for this case (even sum), so mid = 10050000000
        assert_eq!(mid, 10050000000); // 100.5 in fixed-point
    }

    #[test]
    fn test_mid_price_floor_rounding() {
        let bid_fp = 100 * FP_SCALE;
        let ask_fp = 101 * FP_SCALE;
        let mid = compute_mid_fp(bid_fp, ask_fp, RoundingRule::Floor);
        
        assert_eq!(mid, 10050000000); // floor(100.5) = 100.5 (exact in FP)
    }

    #[test]
    fn test_transformer_mid_price_emission() {
        let mapping = SettlementReferenceMapping15m::binance_spot_mid();
        let mut transformer = ReferenceTickTransformer::new(mapping);

        let update = BinanceBookUpdate {
            symbol: "BTCUSDT".to_string(),
            bid_price: Some(45000.0),
            bid_qty: Some(1.0),
            ask_price: Some(45001.0),
            ask_qty: Some(1.0),
            exchange_ts_ns: Some(1000000000),
            ingest_ts_ns: 1000000000,
            visible_ts_ns: 1000100000,
            source_seq: 1,
        };

        let result = transformer.process_book_update(update).unwrap();
        assert!(result.is_some());
        
        let tick = result.unwrap();
        assert_eq!(tick.price_kind_used, PriceKind::Mid);
        assert!(tick.fallback_reason.is_none());
        assert_eq!(tick.asset, UpDownAsset::BTC);
        
        // Mid = (45000 + 45001) / 2 = 45000.5
        let expected_mid = (45000.0 + 45001.0) / 2.0;
        assert!((tick.price() - expected_mid).abs() < 1e-8);
    }

    #[test]
    fn test_transformer_fallback_on_missing_ask() {
        let mapping = SettlementReferenceMapping15m::binance_spot_mid();
        let mut transformer = ReferenceTickTransformer::new(mapping);

        // First, add a trade so fallback has something to use
        let trade = BinanceTrade {
            symbol: "BTCUSDT".to_string(),
            price: 45000.5,
            qty: 1.0,
            exchange_ts_ns: Some(999000000),
            ingest_ts_ns: 999000000,
            visible_ts_ns: 999100000,
            source_seq: 1,
        };
        transformer.process_trade(trade).unwrap();

        // Now send book update with missing ask
        let update = BinanceBookUpdate {
            symbol: "BTCUSDT".to_string(),
            bid_price: Some(45000.0),
            bid_qty: Some(1.0),
            ask_price: None, // Missing!
            ask_qty: None,
            exchange_ts_ns: Some(1000000000),
            ingest_ts_ns: 1000000000,
            visible_ts_ns: 1000100000,
            source_seq: 2,
        };

        let result = transformer.process_book_update(update).unwrap();
        assert!(result.is_some());
        
        let tick = result.unwrap();
        assert_eq!(tick.price_kind_used, PriceKind::Last);
        assert_eq!(tick.fallback_reason, Some(FallbackReason::MissingAsk));
    }

    #[test]
    fn test_transformer_fallback_on_crossed_book() {
        let mapping = SettlementReferenceMapping15m::binance_spot_mid();
        let mut transformer = ReferenceTickTransformer::new(mapping);

        // Add trade for fallback
        let trade = BinanceTrade {
            symbol: "BTCUSDT".to_string(),
            price: 45000.5,
            qty: 1.0,
            exchange_ts_ns: Some(999000000),
            ingest_ts_ns: 999000000,
            visible_ts_ns: 999100000,
            source_seq: 1,
        };
        transformer.process_trade(trade).unwrap();

        // Crossed book: bid > ask
        let update = BinanceBookUpdate {
            symbol: "BTCUSDT".to_string(),
            bid_price: Some(45001.0), // Higher than ask!
            bid_qty: Some(1.0),
            ask_price: Some(45000.0),
            ask_qty: Some(1.0),
            exchange_ts_ns: Some(1000000000),
            ingest_ts_ns: 1000000000,
            visible_ts_ns: 1000100000,
            source_seq: 2,
        };

        let result = transformer.process_book_update(update).unwrap();
        assert!(result.is_some());
        
        let tick = result.unwrap();
        assert_eq!(tick.fallback_reason, Some(FallbackReason::CrossedBook));
    }

    #[test]
    fn test_transformer_carry_forward() {
        let mut mapping = SettlementReferenceMapping15m::binance_spot_mid();
        mapping.fallback_chain.clear(); // No fallback steps
        mapping.terminal_fallback = TerminalFallbackAction::CarryForwardLastValid;
        
        let mut transformer = ReferenceTickTransformer::new(mapping);

        // First, a valid tick
        let update1 = BinanceBookUpdate {
            symbol: "BTCUSDT".to_string(),
            bid_price: Some(45000.0),
            bid_qty: Some(1.0),
            ask_price: Some(45001.0),
            ask_qty: Some(1.0),
            exchange_ts_ns: Some(1000000000),
            ingest_ts_ns: 1000000000,
            visible_ts_ns: 1000100000,
            source_seq: 1,
        };
        let tick1 = transformer.process_book_update(update1).unwrap().unwrap();

        // Now, invalid tick (missing ask)
        let update2 = BinanceBookUpdate {
            symbol: "BTCUSDT".to_string(),
            bid_price: Some(45000.0),
            bid_qty: Some(1.0),
            ask_price: None,
            ask_qty: None,
            exchange_ts_ns: Some(2000000000),
            ingest_ts_ns: 2000000000,
            visible_ts_ns: 2000100000,
            source_seq: 2,
        };
        let tick2 = transformer.process_book_update(update2).unwrap().unwrap();

        assert_eq!(tick2.fallback_reason, Some(FallbackReason::CarryForward));
        assert_eq!(tick2.price_fp, tick1.price_fp); // Same price as carry-forward
    }

    #[test]
    fn test_transformer_fail_no_fallback() {
        let mut mapping = SettlementReferenceMapping15m::binance_spot_mid();
        mapping.fallback_chain.clear();
        mapping.terminal_fallback = TerminalFallbackAction::Fail;
        
        let mut transformer = ReferenceTickTransformer::new(mapping);

        let update = BinanceBookUpdate {
            symbol: "BTCUSDT".to_string(),
            bid_price: Some(45000.0),
            bid_qty: Some(1.0),
            ask_price: None, // Missing!
            ask_qty: None,
            exchange_ts_ns: Some(1000000000),
            ingest_ts_ns: 1000000000,
            visible_ts_ns: 1000100000,
            source_seq: 1,
        };

        let result = transformer.process_book_update(update);
        assert!(result.is_err());
        
        match result.unwrap_err() {
            TransformationError::NoPriceAvailable { .. } => {}
            e => panic!("Unexpected error: {:?}", e),
        }
    }

    #[test]
    fn test_asset_from_slug_prefix() {
        assert_eq!(UpDownAsset::from_slug_prefix("btc-updown-15m-123"), Some(UpDownAsset::BTC));
        assert_eq!(UpDownAsset::from_slug_prefix("ETH-updown-15m-123"), Some(UpDownAsset::ETH));
        assert_eq!(UpDownAsset::from_slug_prefix("sol_something"), Some(UpDownAsset::SOL));
        assert_eq!(UpDownAsset::from_slug_prefix("xrp-test"), Some(UpDownAsset::XRP));
        assert_eq!(UpDownAsset::from_slug_prefix("unknown"), None);
    }

    #[test]
    fn test_tick_fingerprint_stability() {
        let tick = SettlementReferenceTick {
            mapping_version: 1,
            venue_id: ReferenceVenue::BinanceSpot,
            symbol: "BTCUSDT".to_string(),
            asset: UpDownAsset::BTC,
            price_kind_used: PriceKind::Mid,
            price_fp: 4500000000000,
            visible_ts_ns: 1000000000,
            ingest_ts_ns: 999000000,
            exchange_ts_ns: Some(998000000),
            source_seq: 42,
            fallback_reason: None,
            raw_bid_fp: Some(4499900000000),
            raw_ask_fp: Some(4500100000000),
            raw_trade_price_fp: None,
        };

        let fp1 = tick.fingerprint();
        let fp2 = tick.fingerprint();
        assert_eq!(fp1, fp2);
    }

    #[test]
    fn test_outlier_bounds() {
        let bounds = OutlierBounds::default();
        
        // Valid price
        assert!(bounds.is_valid(float_to_fp(45000.0)));
        
        // Invalid: zero
        assert!(!bounds.is_valid(0));
        
        // Invalid: negative
        assert!(!bounds.is_valid(-100));
        
        // Valid change
        assert!(bounds.is_change_valid(
            float_to_fp(45000.0),
            float_to_fp(45500.0) // ~1% change
        ));
        
        // Invalid change: >50%
        assert!(!bounds.is_change_valid(
            float_to_fp(45000.0),
            float_to_fp(70000.0) // ~56% change
        ));
    }

    #[test]
    fn test_staleness_detection() {
        let mapping = SettlementReferenceMapping15m::binance_spot_mid();
        let mut transformer = ReferenceTickTransformer::new(mapping);

        // Add trade for fallback
        let trade = BinanceTrade {
            symbol: "BTCUSDT".to_string(),
            price: 45000.5,
            qty: 1.0,
            exchange_ts_ns: Some(1000000000),
            ingest_ts_ns: 1000000000,
            visible_ts_ns: 1000100000,
            source_seq: 1,
        };
        transformer.process_trade(trade).unwrap();

        // Book update that's too old (6 seconds, threshold is 5)
        let update = BinanceBookUpdate {
            symbol: "BTCUSDT".to_string(),
            bid_price: Some(45000.0),
            bid_qty: Some(1.0),
            ask_price: Some(45001.0),
            ask_qty: Some(1.0),
            exchange_ts_ns: Some(1000000000),
            ingest_ts_ns: 1000000000, // 6 seconds before visible
            visible_ts_ns: 7000000000, // 7 seconds later
            source_seq: 2,
        };

        let result = transformer.process_book_update(update).unwrap();
        // Should fallback to trade due to stale book data
        assert!(result.is_some());
        let tick = result.unwrap();
        // Fallback triggered due to stale data
        assert!(tick.fallback_reason.is_some());
    }
}

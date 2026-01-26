//! Settlement Reference Replay for 15-Minute Up/Down Product
//!
//! This module implements deterministic, hermetic settlement using an explicitly
//! recorded and replayed reference price series. Settlement outcomes are computed
//! solely from the `SettlementReferenceTick` stream - never from execution books
//! or strategy signal feeds.
//!
//! # Design Principles
//!
//! 1. **Deterministic**: Given identical dataset and config, settlement produces
//!    bit-for-bit identical outcomes.
//!
//! 2. **Hermetic**: Settlement consumes ONLY the recorded reference stream.
//!    No queries to execution book, strategy feeds, or external systems.
//!
//! 3. **Auditable**: Every settlement decision logs the exact tick identifiers,
//!    timestamps, and spec version used for reproducibility.
//!
//! # Three-Timestamp Model for Reference Ticks
//!
//! Each reference tick carries:
//! - `exchange_ts_ns`: Venue-provided timestamp (optional)
//! - `ingest_ts_ns`: When the recorder captured this tick (required)
//! - `visible_ts_ns`: Computed via latency model (required for backtest)
//!
//! # Fixed-Point Price Representation
//!
//! Prices are stored as `i128` scaled by `PRICE_SCALE` (1e8) to avoid floating-point
//! non-determinism. All price comparisons use fixed-point arithmetic.

use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use crate::backtest_v2::clock::Nanos;

// =============================================================================
// FIXED-POINT PRICE TYPE
// =============================================================================

/// Scale factor for fixed-point prices (1e8 = 8 decimal places).
pub const PRICE_SCALE: i128 = 100_000_000;

/// Fixed-point price representation for deterministic arithmetic.
///
/// Stored as `i128` with 8 decimal places (same as Chainlink).
/// This avoids floating-point non-determinism in price comparisons.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[repr(transparent)]
pub struct PriceFixed(pub i128);

impl PriceFixed {
    /// Create from raw fixed-point value.
    #[inline]
    pub const fn from_raw(raw: i128) -> Self {
        Self(raw)
    }

    /// Create from floating-point (rounds to nearest).
    /// CAUTION: Use only for ingestion; prefer `from_raw` for replay.
    #[inline]
    pub fn from_f64(price: f64) -> Self {
        let scaled = (price * PRICE_SCALE as f64).round() as i128;
        Self(scaled)
    }

    /// Convert to floating-point (for display/logging only).
    #[inline]
    pub fn to_f64(self) -> f64 {
        self.0 as f64 / PRICE_SCALE as f64
    }

    /// Raw fixed-point value.
    #[inline]
    pub const fn raw(self) -> i128 {
        self.0
    }

    /// Compute mid-price from bid and ask (fixed-point).
    #[inline]
    pub fn mid(bid: PriceFixed, ask: PriceFixed) -> Self {
        // (bid + ask) / 2, rounding toward zero
        Self((bid.0 + ask.0) / 2)
    }

    /// Check if two prices are equal within a tolerance (in raw units).
    #[inline]
    pub fn eq_within(self, other: PriceFixed, tolerance: i128) -> bool {
        (self.0 - other.0).abs() <= tolerance
    }
}

impl Default for PriceFixed {
    fn default() -> Self {
        Self(0)
    }
}

impl std::fmt::Display for PriceFixed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:.8}", self.to_f64())
    }
}

// =============================================================================
// SETTLEMENT REFERENCE SPEC (CANONICAL CONTRACT)
// =============================================================================

/// Canonical settlement reference specification for the 15M Up/Down product.
///
/// This spec pins down EXACTLY how settlement reference prices are defined.
/// It is single-sourced and shared between strategy windowing and settlement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettlementReferenceSpec {
    /// Spec version (for auditing; increment on any semantic change).
    pub version: u32,

    /// Window duration in nanoseconds (15 minutes = 900_000_000_000 ns).
    pub window_duration_ns: Nanos,

    /// Reference venue identifier (e.g., "binance").
    pub reference_venue: String,

    /// Reference price type (Mid, Mark, Last, etc.).
    pub reference_price_type: ReferencePriceType,

    /// Rule for selecting the start reference price.
    pub sampling_rule_start: SamplingRule,

    /// Rule for selecting the end reference price.
    pub sampling_rule_end: SamplingRule,

    /// Tie-breaking rule when start_price == end_price.
    pub tie_rule: SettlementTieRule,

    /// Tolerance (in raw fixed-point units) for tie detection.
    /// Default: 0 (exact equality required for tie).
    pub tie_tolerance_raw: i128,

    /// Rounding rule for price comparison.
    pub rounding_rule: RoundingRule,
}

/// Reference price type to use for settlement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ReferencePriceType {
    /// Mid-price: (bid + ask) / 2
    Mid,
    /// Mark price (venue-computed fair value)
    Mark,
    /// Last trade price
    Last,
    /// Index price (composite of multiple venues)
    Index,
}

impl Default for ReferencePriceType {
    fn default() -> Self {
        Self::Mid
    }
}

/// Sampling rule for selecting a reference tick at window boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SamplingRule {
    /// First tick with visible_ts >= boundary.
    FirstAtOrAfter,

    /// First tick with visible_ts in [boundary, boundary + epsilon].
    FirstInWindow { epsilon_ns: Nanos },

    /// Last tick with visible_ts < boundary (carry-forward).
    LastBefore,

    /// Closest tick to boundary (ties go to earlier tick).
    ClosestToBoundary,

    /// Tick at exactly boundary (error if missing).
    ExactAtBoundary,
}

impl Default for SamplingRule {
    fn default() -> Self {
        // Default: first tick at or after boundary
        Self::FirstAtOrAfter
    }
}

/// Tie-breaking rule when start_price == end_price.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SettlementTieRule {
    /// Down (No) wins on tie - price must INCREASE for Up to win.
    DownWins,
    /// Up (Yes) wins on tie.
    UpWins,
    /// Tie is an error - market cannot settle.
    Invalid,
    /// 50/50 split settlement.
    Split,
}

impl Default for SettlementTieRule {
    fn default() -> Self {
        // Polymarket 15M: tie goes to Down (price did not go UP)
        Self::DownWins
    }
}

/// Rounding rule for price comparison.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RoundingRule {
    /// No rounding - use exact fixed-point comparison.
    None,
    /// Round to N decimal places before comparison.
    Decimals { places: u8 },
    /// Round to tick size before comparison.
    TickSize { tick_raw: i128 },
}

impl Default for RoundingRule {
    fn default() -> Self {
        // 8 decimal places (same as Chainlink/Binance)
        Self::Decimals { places: 8 }
    }
}

/// Nanoseconds per 15-minute window.
pub const NANOS_15_MIN: Nanos = 15 * 60 * 1_000_000_000;

impl Default for SettlementReferenceSpec {
    fn default() -> Self {
        Self::polymarket_15m_updown_v1()
    }
}

impl SettlementReferenceSpec {
    /// Canonical spec for Polymarket 15-minute Up/Down markets (version 1).
    ///
    /// Contract semantics:
    /// - Window: 15 minutes from slug-encoded start_ts
    /// - Reference: Binance spot mid-price
    /// - Start price: First mid tick with visible_ts >= window_start
    /// - End price: First mid tick with visible_ts >= window_end
    /// - Up wins if: end_price > start_price
    /// - Down wins if: end_price <= start_price (tie goes to Down)
    pub fn polymarket_15m_updown_v1() -> Self {
        Self {
            version: 1,
            window_duration_ns: NANOS_15_MIN,
            reference_venue: "binance".to_string(),
            reference_price_type: ReferencePriceType::Mid,
            sampling_rule_start: SamplingRule::FirstAtOrAfter,
            sampling_rule_end: SamplingRule::FirstAtOrAfter,
            tie_rule: SettlementTieRule::DownWins,
            tie_tolerance_raw: 0,
            rounding_rule: RoundingRule::Decimals { places: 8 },
        }
    }

    /// Compute a deterministic hash of this spec for fingerprinting.
    pub fn spec_hash(&self) -> u64 {
        let mut hasher = DefaultHasher::new();
        self.version.hash(&mut hasher);
        self.window_duration_ns.hash(&mut hasher);
        self.reference_venue.hash(&mut hasher);
        std::mem::discriminant(&self.reference_price_type).hash(&mut hasher);
        std::mem::discriminant(&self.sampling_rule_start).hash(&mut hasher);
        std::mem::discriminant(&self.sampling_rule_end).hash(&mut hasher);
        std::mem::discriminant(&self.tie_rule).hash(&mut hasher);
        self.tie_tolerance_raw.hash(&mut hasher);
        hasher.finish()
    }

    /// Apply rounding to a price.
    pub fn round_price(&self, price: PriceFixed) -> PriceFixed {
        match &self.rounding_rule {
            RoundingRule::None => price,
            RoundingRule::Decimals { places } => {
                // Round to N decimal places
                let divisor = 10i128.pow((8 - *places) as u32); // 8 is our base scale
                if divisor <= 1 {
                    price
                } else {
                    let rounded = (price.0 + divisor / 2) / divisor * divisor;
                    PriceFixed(rounded)
                }
            }
            RoundingRule::TickSize { tick_raw } => {
                if *tick_raw <= 0 {
                    price
                } else {
                    let rounded = (price.0 + tick_raw / 2) / tick_raw * tick_raw;
                    PriceFixed(rounded)
                }
            }
        }
    }

    /// Determine settlement outcome given start and end prices.
    pub fn determine_outcome(
        &self,
        start_price: PriceFixed,
        end_price: PriceFixed,
    ) -> SettlementOutcomeResult {
        let start_rounded = self.round_price(start_price);
        let end_rounded = self.round_price(end_price);

        // Check for tie (with tolerance)
        let is_tie = start_rounded.eq_within(end_rounded, self.tie_tolerance_raw);

        if is_tie {
            match self.tie_rule {
                SettlementTieRule::DownWins => SettlementOutcomeResult::Down { is_tie: true },
                SettlementTieRule::UpWins => SettlementOutcomeResult::Up { is_tie: true },
                SettlementTieRule::Invalid => SettlementOutcomeResult::Invalid {
                    reason: "Tie detected and tie_rule = Invalid".to_string(),
                },
                SettlementTieRule::Split => SettlementOutcomeResult::Split { share_value: 0.5 },
            }
        } else if end_rounded > start_rounded {
            SettlementOutcomeResult::Up { is_tie: false }
        } else {
            SettlementOutcomeResult::Down { is_tie: false }
        }
    }
}

/// Settlement outcome from reference price comparison.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SettlementOutcomeResult {
    /// Up (Yes) wins - price went up.
    Up { is_tie: bool },
    /// Down (No) wins - price went down or stayed flat.
    Down { is_tie: bool },
    /// Split settlement.
    Split { share_value: f64 },
    /// Invalid - cannot determine outcome.
    Invalid { reason: String },
}

impl SettlementOutcomeResult {
    pub fn is_up(&self) -> bool {
        matches!(self, Self::Up { .. })
    }

    pub fn is_down(&self) -> bool {
        matches!(self, Self::Down { .. })
    }

    pub fn is_tie(&self) -> bool {
        match self {
            Self::Up { is_tie } | Self::Down { is_tie } => *is_tie,
            _ => false,
        }
    }
}

// =============================================================================
// SETTLEMENT REFERENCE TICK (RECORDED STREAM)
// =============================================================================

/// A single reference tick from the settlement reference stream.
///
/// This is the atomic unit of the recorded reference series.
/// All prices are fixed-point to ensure determinism.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettlementReferenceTick {
    /// Stable sequence number (monotonic within stream, for tie-breaking).
    pub seq: u64,

    /// Venue identifier (e.g., "binance").
    pub venue_id: String,

    /// Asset symbol (e.g., "BTC", "ETH").
    pub symbol: String,

    /// Price type (Mid, Mark, Last, Index).
    pub price_type: ReferencePriceType,

    /// The reference price (fixed-point).
    pub price_fp: PriceFixed,

    /// For Mid price: the underlying bid (fixed-point).
    pub bid_fp: Option<PriceFixed>,

    /// For Mid price: the underlying ask (fixed-point).
    pub ask_fp: Option<PriceFixed>,

    /// Venue-provided timestamp (optional, may be missing or untrusted).
    pub exchange_ts_ns: Option<Nanos>,

    /// Local ingest timestamp (required for HFT-grade replay).
    pub ingest_ts_ns: Nanos,

    /// Visible timestamp (computed via latency model).
    pub visible_ts_ns: Nanos,
}

impl SettlementReferenceTick {
    /// Create a new reference tick from bid/ask (computes mid).
    pub fn from_bid_ask(
        seq: u64,
        venue_id: String,
        symbol: String,
        bid_fp: PriceFixed,
        ask_fp: PriceFixed,
        exchange_ts_ns: Option<Nanos>,
        ingest_ts_ns: Nanos,
        visible_ts_ns: Nanos,
    ) -> Self {
        let mid_fp = PriceFixed::mid(bid_fp, ask_fp);
        Self {
            seq,
            venue_id,
            symbol,
            price_type: ReferencePriceType::Mid,
            price_fp: mid_fp,
            bid_fp: Some(bid_fp),
            ask_fp: Some(ask_fp),
            exchange_ts_ns,
            ingest_ts_ns,
            visible_ts_ns,
        }
    }

    /// Create from a pre-computed price (Mark, Last, Index).
    pub fn from_price(
        seq: u64,
        venue_id: String,
        symbol: String,
        price_type: ReferencePriceType,
        price_fp: PriceFixed,
        exchange_ts_ns: Option<Nanos>,
        ingest_ts_ns: Nanos,
        visible_ts_ns: Nanos,
    ) -> Self {
        Self {
            seq,
            venue_id,
            symbol,
            price_type,
            price_fp,
            bid_fp: None,
            ask_fp: None,
            exchange_ts_ns,
            ingest_ts_ns,
            visible_ts_ns,
        }
    }

    /// Compute a stable fingerprint for this tick.
    pub fn fingerprint(&self) -> u64 {
        let mut hasher = DefaultHasher::new();
        self.seq.hash(&mut hasher);
        self.venue_id.hash(&mut hasher);
        self.symbol.hash(&mut hasher);
        self.price_fp.hash(&mut hasher);
        self.ingest_ts_ns.hash(&mut hasher);
        hasher.finish()
    }
}

/// Ordering for ticks: (visible_ts, seq) for deterministic ordering.
impl PartialOrd for SettlementReferenceTick {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for SettlementReferenceTick {
    fn cmp(&self, other: &Self) -> Ordering {
        self.visible_ts_ns
            .cmp(&other.visible_ts_ns)
            .then_with(|| self.seq.cmp(&other.seq))
    }
}

// =============================================================================
// SETTLEMENT REFERENCE PROVIDER TRAIT
// =============================================================================

/// Trait for providing settlement reference prices.
///
/// This is the ONLY interface settlement.rs should use to obtain
/// reference prices. Implementations must NOT query execution books
/// or strategy feeds.
pub trait SettlementReferenceProvider: Send + Sync {
    /// Get the settlement reference spec.
    fn spec(&self) -> &SettlementReferenceSpec;

    /// Sample the start reference price for a window.
    ///
    /// Returns the selected tick and its price, or None if no valid tick.
    fn sample_start_price(&self, window_start_ns: Nanos) -> Option<SampledReference>;

    /// Sample the end reference price for a window.
    ///
    /// Returns the selected tick and its price, or None if no valid tick.
    fn sample_end_price(&self, window_end_ns: Nanos) -> Option<SampledReference>;

    /// Check if the reference stream has sufficient coverage for a window.
    fn has_coverage(&self, window_start_ns: Nanos, window_end_ns: Nanos) -> bool;

    /// Get stream metadata for auditing.
    fn stream_metadata(&self) -> ReferenceStreamMetadata;
}

/// A sampled reference price with full provenance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SampledReference {
    /// The selected tick.
    pub tick: SettlementReferenceTick,
    /// The price to use (after any spec-mandated derivation).
    pub price: PriceFixed,
    /// Which sampling rule was applied.
    pub rule_applied: String,
    /// Distance from boundary in nanoseconds (for auditing).
    pub distance_from_boundary_ns: i64,
}

/// Metadata about the reference stream.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReferenceStreamMetadata {
    /// Venue identifier.
    pub venue_id: String,
    /// Asset symbol.
    pub symbol: String,
    /// Number of ticks in the stream.
    pub tick_count: usize,
    /// Earliest tick visible_ts.
    pub earliest_visible_ts_ns: Option<Nanos>,
    /// Latest tick visible_ts.
    pub latest_visible_ts_ns: Option<Nanos>,
    /// Average tick interval (nanoseconds).
    pub avg_interval_ns: Option<Nanos>,
}

// =============================================================================
// RECORDED REFERENCE STREAM PROVIDER
// =============================================================================

/// Settlement reference provider backed by a pre-recorded tick stream.
///
/// This is the standard implementation for backtests. It loads ticks from
/// the dataset and provides deterministic sampling.
pub struct RecordedReferenceStreamProvider {
    /// The settlement specification.
    spec: SettlementReferenceSpec,
    /// Sorted ticks (by visible_ts, then seq).
    ticks: Vec<SettlementReferenceTick>,
    /// Metadata about the stream.
    metadata: ReferenceStreamMetadata,
}

impl RecordedReferenceStreamProvider {
    /// Create from a vector of ticks (will be sorted).
    pub fn new(spec: SettlementReferenceSpec, mut ticks: Vec<SettlementReferenceTick>) -> Self {
        // Sort by (visible_ts, seq) for deterministic ordering
        ticks.sort();

        let metadata = Self::compute_metadata(&ticks, &spec);

        Self {
            spec,
            ticks,
            metadata,
        }
    }

    fn compute_metadata(
        ticks: &[SettlementReferenceTick],
        spec: &SettlementReferenceSpec,
    ) -> ReferenceStreamMetadata {
        let tick_count = ticks.len();
        let earliest = ticks.first().map(|t| t.visible_ts_ns);
        let latest = ticks.last().map(|t| t.visible_ts_ns);

        let avg_interval = if tick_count > 1 {
            let first_ts = ticks.first().map(|t| t.visible_ts_ns).unwrap_or(0);
            let last_ts = ticks.last().map(|t| t.visible_ts_ns).unwrap_or(0);
            Some((last_ts - first_ts) / (tick_count - 1) as i64)
        } else {
            None
        };

        // Get symbol from first tick or fallback
        let symbol = ticks
            .first()
            .map(|t| t.symbol.clone())
            .unwrap_or_else(|| "UNKNOWN".to_string());

        ReferenceStreamMetadata {
            venue_id: spec.reference_venue.clone(),
            symbol,
            tick_count,
            earliest_visible_ts_ns: earliest,
            latest_visible_ts_ns: latest,
            avg_interval_ns: avg_interval,
        }
    }

    /// Get number of ticks in the stream.
    pub fn len(&self) -> usize {
        self.ticks.len()
    }

    /// Check if the stream is empty.
    pub fn is_empty(&self) -> bool {
        self.ticks.is_empty()
    }

    /// Get a reference to all ticks (for debugging/auditing).
    pub fn ticks(&self) -> &[SettlementReferenceTick] {
        &self.ticks
    }

    /// Sample a tick according to a sampling rule at a boundary.
    fn sample_at_boundary(
        &self,
        boundary_ns: Nanos,
        rule: &SamplingRule,
    ) -> Option<SampledReference> {
        if self.ticks.is_empty() {
            return None;
        }

        match rule {
            SamplingRule::FirstAtOrAfter => {
                // Binary search for first tick with visible_ts >= boundary
                let idx = self
                    .ticks
                    .partition_point(|t| t.visible_ts_ns < boundary_ns);

                if idx < self.ticks.len() {
                    let tick = &self.ticks[idx];
                    Some(SampledReference {
                        tick: tick.clone(),
                        price: tick.price_fp,
                        rule_applied: "FirstAtOrAfter".to_string(),
                        distance_from_boundary_ns: tick.visible_ts_ns - boundary_ns,
                    })
                } else {
                    None
                }
            }

            SamplingRule::FirstInWindow { epsilon_ns } => {
                // First tick with visible_ts in [boundary, boundary + epsilon]
                let idx = self
                    .ticks
                    .partition_point(|t| t.visible_ts_ns < boundary_ns);

                if idx < self.ticks.len() {
                    let tick = &self.ticks[idx];
                    if tick.visible_ts_ns <= boundary_ns + epsilon_ns {
                        Some(SampledReference {
                            tick: tick.clone(),
                            price: tick.price_fp,
                            rule_applied: format!("FirstInWindow(epsilon={}ns)", epsilon_ns),
                            distance_from_boundary_ns: tick.visible_ts_ns - boundary_ns,
                        })
                    } else {
                        None
                    }
                } else {
                    None
                }
            }

            SamplingRule::LastBefore => {
                // Last tick with visible_ts < boundary
                let idx = self
                    .ticks
                    .partition_point(|t| t.visible_ts_ns < boundary_ns);

                if idx > 0 {
                    let tick = &self.ticks[idx - 1];
                    Some(SampledReference {
                        tick: tick.clone(),
                        price: tick.price_fp,
                        rule_applied: "LastBefore".to_string(),
                        distance_from_boundary_ns: tick.visible_ts_ns - boundary_ns,
                    })
                } else {
                    None
                }
            }

            SamplingRule::ClosestToBoundary => {
                let idx = self
                    .ticks
                    .partition_point(|t| t.visible_ts_ns < boundary_ns);

                let before = if idx > 0 {
                    Some(&self.ticks[idx - 1])
                } else {
                    None
                };
                let at_or_after = if idx < self.ticks.len() {
                    Some(&self.ticks[idx])
                } else {
                    None
                };

                match (before, at_or_after) {
                    (Some(b), Some(a)) => {
                        let dist_before = boundary_ns - b.visible_ts_ns;
                        let dist_after = a.visible_ts_ns - boundary_ns;

                        // Ties go to earlier tick
                        let tick = if dist_before <= dist_after { b } else { a };
                        let distance = if dist_before <= dist_after {
                            -(dist_before)
                        } else {
                            dist_after
                        };

                        Some(SampledReference {
                            tick: tick.clone(),
                            price: tick.price_fp,
                            rule_applied: "ClosestToBoundary".to_string(),
                            distance_from_boundary_ns: distance,
                        })
                    }
                    (Some(b), None) => Some(SampledReference {
                        tick: b.clone(),
                        price: b.price_fp,
                        rule_applied: "ClosestToBoundary".to_string(),
                        distance_from_boundary_ns: -(boundary_ns - b.visible_ts_ns),
                    }),
                    (None, Some(a)) => Some(SampledReference {
                        tick: a.clone(),
                        price: a.price_fp,
                        rule_applied: "ClosestToBoundary".to_string(),
                        distance_from_boundary_ns: a.visible_ts_ns - boundary_ns,
                    }),
                    (None, None) => None,
                }
            }

            SamplingRule::ExactAtBoundary => {
                // Must find tick at exactly boundary
                let idx = self
                    .ticks
                    .partition_point(|t| t.visible_ts_ns < boundary_ns);

                if idx < self.ticks.len() {
                    let tick = &self.ticks[idx];
                    if tick.visible_ts_ns == boundary_ns {
                        Some(SampledReference {
                            tick: tick.clone(),
                            price: tick.price_fp,
                            rule_applied: "ExactAtBoundary".to_string(),
                            distance_from_boundary_ns: 0,
                        })
                    } else {
                        None // Not exact
                    }
                } else {
                    None
                }
            }
        }
    }
}

impl SettlementReferenceProvider for RecordedReferenceStreamProvider {
    fn spec(&self) -> &SettlementReferenceSpec {
        &self.spec
    }

    fn sample_start_price(&self, window_start_ns: Nanos) -> Option<SampledReference> {
        self.sample_at_boundary(window_start_ns, &self.spec.sampling_rule_start)
    }

    fn sample_end_price(&self, window_end_ns: Nanos) -> Option<SampledReference> {
        self.sample_at_boundary(window_end_ns, &self.spec.sampling_rule_end)
    }

    fn has_coverage(&self, window_start_ns: Nanos, window_end_ns: Nanos) -> bool {
        // Must be able to sample both start and end
        self.sample_start_price(window_start_ns).is_some()
            && self.sample_end_price(window_end_ns).is_some()
    }

    fn stream_metadata(&self) -> ReferenceStreamMetadata {
        self.metadata.clone()
    }
}

// =============================================================================
// SETTLEMENT AUDIT RECORD
// =============================================================================

/// Complete audit record for a single window settlement.
///
/// This record contains all information needed to independently verify
/// that the settlement outcome was computed correctly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SettlementAuditRecord {
    /// Window index (for multi-window runs).
    pub window_index: u64,
    /// Window start time (nanoseconds).
    pub window_start_ns: Nanos,
    /// Window end time (nanoseconds).
    pub window_end_ns: Nanos,
    /// Spec version used for settlement.
    pub spec_version: u32,
    /// Spec hash for verification.
    pub spec_hash: u64,
    /// Start tick identifier (seq number).
    pub start_tick_seq: u64,
    /// Start tick visible_ts.
    pub start_tick_visible_ts_ns: Nanos,
    /// Start price (fixed-point).
    pub start_price_fp: PriceFixed,
    /// Start price (for display).
    pub start_price_f64: f64,
    /// End tick identifier (seq number).
    pub end_tick_seq: u64,
    /// End tick visible_ts.
    pub end_tick_visible_ts_ns: Nanos,
    /// End price (fixed-point).
    pub end_price_fp: PriceFixed,
    /// End price (for display).
    pub end_price_f64: f64,
    /// Settlement outcome.
    pub outcome: SettlementOutcomeResult,
    /// Decision timestamp (when settlement was computed).
    pub decision_ts_ns: Nanos,
}

impl SettlementAuditRecord {
    /// Compute a deterministic hash of this record.
    pub fn record_hash(&self) -> u64 {
        let mut hasher = DefaultHasher::new();
        self.window_index.hash(&mut hasher);
        self.window_start_ns.hash(&mut hasher);
        self.window_end_ns.hash(&mut hasher);
        self.spec_version.hash(&mut hasher);
        self.start_tick_seq.hash(&mut hasher);
        self.start_price_fp.hash(&mut hasher);
        self.end_tick_seq.hash(&mut hasher);
        self.end_price_fp.hash(&mut hasher);
        std::mem::discriminant(&self.outcome).hash(&mut hasher);
        hasher.finish()
    }

    /// Format as concise debug string.
    pub fn format_debug(&self) -> String {
        let winner = match &self.outcome {
            SettlementOutcomeResult::Up { is_tie } => {
                if *is_tie {
                    "UP (TIE)"
                } else {
                    "UP"
                }
            }
            SettlementOutcomeResult::Down { is_tie } => {
                if *is_tie {
                    "DOWN (TIE)"
                } else {
                    "DOWN"
                }
            }
            SettlementOutcomeResult::Split { .. } => "SPLIT",
            SettlementOutcomeResult::Invalid { .. } => "INVALID",
        };

        format!(
            "window[{}] start={} end={} | start_tick={} @ {} price={:.8} | end_tick={} @ {} price={:.8} | {}",
            self.window_index,
            self.window_start_ns,
            self.window_end_ns,
            self.start_tick_seq,
            self.start_tick_visible_ts_ns,
            self.start_price_f64,
            self.end_tick_seq,
            self.end_tick_visible_ts_ns,
            self.end_price_f64,
            winner,
        )
    }
}

// =============================================================================
// REFERENCE-BASED SETTLEMENT ENGINE
// =============================================================================

/// Settlement engine that uses ONLY the recorded reference stream.
///
/// This is the authoritative settlement implementation for 15M backtests.
/// It does NOT query execution books or strategy feeds.
pub struct ReferenceSettlementEngine<P: SettlementReferenceProvider> {
    provider: P,
    /// Audit records for all settled windows.
    audit_log: Vec<SettlementAuditRecord>,
    /// Statistics.
    stats: ReferenceSettlementStats,
}

/// Statistics for reference-based settlement.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReferenceSettlementStats {
    pub windows_settled: u64,
    pub up_wins: u64,
    pub down_wins: u64,
    pub ties: u64,
    pub missing_start: u64,
    pub missing_end: u64,
    pub invalid_outcomes: u64,
}

impl<P: SettlementReferenceProvider> ReferenceSettlementEngine<P> {
    /// Create a new engine with the given provider.
    pub fn new(provider: P) -> Self {
        Self {
            provider,
            audit_log: Vec::new(),
            stats: ReferenceSettlementStats::default(),
        }
    }

    /// Get the settlement specification.
    pub fn spec(&self) -> &SettlementReferenceSpec {
        self.provider.spec()
    }

    /// Settle a window and return the outcome.
    ///
    /// Returns `None` if the reference stream lacks coverage for this window.
    /// The run should be marked non-production-grade in that case.
    pub fn settle_window(
        &mut self,
        window_index: u64,
        window_start_ns: Nanos,
        decision_ts_ns: Nanos,
    ) -> Option<SettlementAuditRecord> {
        let spec = self.provider.spec();
        let window_end_ns = window_start_ns + spec.window_duration_ns;

        // Sample start and end prices
        let start_ref = match self.provider.sample_start_price(window_start_ns) {
            Some(r) => r,
            None => {
                self.stats.missing_start += 1;
                return None;
            }
        };

        let end_ref = match self.provider.sample_end_price(window_end_ns) {
            Some(r) => r,
            None => {
                self.stats.missing_end += 1;
                return None;
            }
        };

        // Determine outcome
        let outcome = spec.determine_outcome(start_ref.price, end_ref.price);

        // Update stats
        self.stats.windows_settled += 1;
        match &outcome {
            SettlementOutcomeResult::Up { is_tie } => {
                self.stats.up_wins += 1;
                if *is_tie {
                    self.stats.ties += 1;
                }
            }
            SettlementOutcomeResult::Down { is_tie } => {
                self.stats.down_wins += 1;
                if *is_tie {
                    self.stats.ties += 1;
                }
            }
            SettlementOutcomeResult::Invalid { .. } => {
                self.stats.invalid_outcomes += 1;
            }
            SettlementOutcomeResult::Split { .. } => {
                self.stats.ties += 1;
            }
        }

        // Create audit record
        let record = SettlementAuditRecord {
            window_index,
            window_start_ns,
            window_end_ns,
            spec_version: spec.version,
            spec_hash: spec.spec_hash(),
            start_tick_seq: start_ref.tick.seq,
            start_tick_visible_ts_ns: start_ref.tick.visible_ts_ns,
            start_price_fp: start_ref.price,
            start_price_f64: start_ref.price.to_f64(),
            end_tick_seq: end_ref.tick.seq,
            end_tick_visible_ts_ns: end_ref.tick.visible_ts_ns,
            end_price_fp: end_ref.price,
            end_price_f64: end_ref.price.to_f64(),
            outcome,
            decision_ts_ns,
        };

        self.audit_log.push(record.clone());
        Some(record)
    }

    /// Check if a window can be settled (has coverage).
    pub fn can_settle(&self, window_start_ns: Nanos) -> bool {
        let spec = self.provider.spec();
        let window_end_ns = window_start_ns + spec.window_duration_ns;
        self.provider.has_coverage(window_start_ns, window_end_ns)
    }

    /// Get all audit records.
    pub fn audit_log(&self) -> &[SettlementAuditRecord] {
        &self.audit_log
    }

    /// Get statistics.
    pub fn stats(&self) -> &ReferenceSettlementStats {
        &self.stats
    }

    /// Compute a hash of all audit records (for reproducibility check).
    pub fn audit_log_hash(&self) -> u64 {
        let mut hasher = DefaultHasher::new();
        for record in &self.audit_log {
            record.record_hash().hash(&mut hasher);
        }
        hasher.finish()
    }

    /// Get stream metadata.
    pub fn stream_metadata(&self) -> ReferenceStreamMetadata {
        self.provider.stream_metadata()
    }
}

// =============================================================================
// DATASET CLASSIFICATION EXTENSION
// =============================================================================

/// Settlement reference stream coverage classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SettlementReferenceCoverage {
    /// Full coverage - can sample start and end for all windows.
    Full,
    /// Partial coverage - some windows may not be settleable.
    Partial,
    /// No coverage - no reference ticks available.
    None,
}

impl SettlementReferenceCoverage {
    pub fn is_production_grade(&self) -> bool {
        matches!(self, Self::Full)
    }
}

/// Check settlement reference coverage for a time range.
pub fn classify_settlement_coverage<P: SettlementReferenceProvider>(
    provider: &P,
    start_ns: Nanos,
    end_ns: Nanos,
) -> SettlementReferenceCoverage {
    let spec = provider.spec();
    let window_duration = spec.window_duration_ns;

    // Calculate number of windows
    let num_windows = (end_ns - start_ns) / window_duration;
    if num_windows == 0 {
        return SettlementReferenceCoverage::None;
    }

    let mut covered = 0u64;
    let mut window_start = start_ns;

    while window_start < end_ns {
        let window_end = window_start + window_duration;
        if provider.has_coverage(window_start, window_end) {
            covered += 1;
        }
        window_start = window_end;
    }

    if covered == 0 {
        SettlementReferenceCoverage::None
    } else if covered == num_windows as u64 {
        SettlementReferenceCoverage::Full
    } else {
        SettlementReferenceCoverage::Partial
    }
}

// =============================================================================
// TRUST GATE EXTENSION
// =============================================================================

/// Trust failure reason for settlement reference issues.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SettlementReferenceFailure {
    /// No settlement reference stream in dataset.
    MissingReferenceStream,
    /// Reference stream has insufficient coverage.
    InsufficientCoverage {
        coverage: SettlementReferenceCoverage,
        windows_missing: u64,
    },
    /// Reference stream uses wrong venue/price type.
    SpecMismatch {
        expected_venue: String,
        actual_venue: String,
    },
}

impl std::fmt::Display for SettlementReferenceFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingReferenceStream => {
                write!(f, "MISSING_SETTLEMENT_REFERENCE_STREAM")
            }
            Self::InsufficientCoverage {
                coverage,
                windows_missing,
            } => {
                write!(
                    f,
                    "INSUFFICIENT_SETTLEMENT_COVERAGE (coverage={:?}, missing={})",
                    coverage, windows_missing
                )
            }
            Self::SpecMismatch {
                expected_venue,
                actual_venue,
            } => {
                write!(
                    f,
                    "SETTLEMENT_SPEC_MISMATCH (expected={}, actual={})",
                    expected_venue, actual_venue
                )
            }
        }
    }
}

// =============================================================================
// TESTS
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn make_tick(seq: u64, visible_ts_ns: Nanos, price: f64) -> SettlementReferenceTick {
        SettlementReferenceTick::from_price(
            seq,
            "binance".to_string(),
            "BTC".to_string(),
            ReferencePriceType::Mid,
            PriceFixed::from_f64(price),
            Some(visible_ts_ns - 100_000), // exchange_ts slightly before
            visible_ts_ns - 50_000,        // ingest_ts between exchange and visible
            visible_ts_ns,
        )
    }

    #[test]
    fn test_price_fixed_roundtrip() {
        let original = 50123.45678901;
        let fixed = PriceFixed::from_f64(original);
        let back = fixed.to_f64();
        assert!((original - back).abs() < 1e-8);
    }

    #[test]
    fn test_price_fixed_mid() {
        let bid = PriceFixed::from_f64(50000.0);
        let ask = PriceFixed::from_f64(50100.0);
        let mid = PriceFixed::mid(bid, ask);
        assert_eq!(mid.to_f64(), 50050.0);
    }

    #[test]
    fn test_spec_determine_outcome_up() {
        let spec = SettlementReferenceSpec::polymarket_15m_updown_v1();
        let start = PriceFixed::from_f64(50000.0);
        let end = PriceFixed::from_f64(50100.0);
        let outcome = spec.determine_outcome(start, end);
        assert!(matches!(
            outcome,
            SettlementOutcomeResult::Up { is_tie: false }
        ));
    }

    #[test]
    fn test_spec_determine_outcome_down() {
        let spec = SettlementReferenceSpec::polymarket_15m_updown_v1();
        let start = PriceFixed::from_f64(50100.0);
        let end = PriceFixed::from_f64(50000.0);
        let outcome = spec.determine_outcome(start, end);
        assert!(matches!(
            outcome,
            SettlementOutcomeResult::Down { is_tie: false }
        ));
    }

    #[test]
    fn test_spec_determine_outcome_tie_down_wins() {
        let spec = SettlementReferenceSpec::polymarket_15m_updown_v1();
        let price = PriceFixed::from_f64(50000.0);
        let outcome = spec.determine_outcome(price, price);
        assert!(matches!(
            outcome,
            SettlementOutcomeResult::Down { is_tie: true }
        ));
    }

    #[test]
    fn test_sampling_first_at_or_after() {
        let spec = SettlementReferenceSpec::polymarket_15m_updown_v1();
        let ticks = vec![
            make_tick(1, 1000, 50000.0),
            make_tick(2, 2000, 50100.0),
            make_tick(3, 3000, 50200.0),
        ];
        let provider = RecordedReferenceStreamProvider::new(spec, ticks);

        // Exact boundary
        let result = provider.sample_start_price(2000);
        assert!(result.is_some());
        assert_eq!(result.as_ref().unwrap().tick.seq, 2);

        // Between ticks
        let result = provider.sample_start_price(1500);
        assert!(result.is_some());
        assert_eq!(result.as_ref().unwrap().tick.seq, 2);

        // Before all ticks
        let result = provider.sample_start_price(500);
        assert!(result.is_some());
        assert_eq!(result.as_ref().unwrap().tick.seq, 1);

        // After all ticks
        let result = provider.sample_start_price(4000);
        assert!(result.is_none());
    }

    #[test]
    fn test_sampling_last_before() {
        let mut spec = SettlementReferenceSpec::polymarket_15m_updown_v1();
        spec.sampling_rule_end = SamplingRule::LastBefore;

        let ticks = vec![
            make_tick(1, 1000, 50000.0),
            make_tick(2, 2000, 50100.0),
            make_tick(3, 3000, 50200.0),
        ];
        let provider = RecordedReferenceStreamProvider::new(spec, ticks);

        // Should get tick before 2500
        let result = provider.sample_end_price(2500);
        assert!(result.is_some());
        assert_eq!(result.as_ref().unwrap().tick.seq, 2);

        // Should get tick before 1000 (none exists)
        let result = provider.sample_end_price(1000);
        assert!(result.is_none());
    }

    #[test]
    fn test_sampling_closest_to_boundary() {
        let mut spec = SettlementReferenceSpec::polymarket_15m_updown_v1();
        spec.sampling_rule_start = SamplingRule::ClosestToBoundary;

        let ticks = vec![
            make_tick(1, 1000, 50000.0),
            make_tick(2, 2000, 50100.0),
            make_tick(3, 3000, 50200.0),
        ];
        let provider = RecordedReferenceStreamProvider::new(spec, ticks);

        // Closer to 2000 than 1000
        let result = provider.sample_start_price(1600);
        assert!(result.is_some());
        assert_eq!(result.as_ref().unwrap().tick.seq, 2);

        // Closer to 1000 than 2000
        let result = provider.sample_start_price(1400);
        assert!(result.is_some());
        assert_eq!(result.as_ref().unwrap().tick.seq, 1);

        // Equidistant - should pick earlier (1000)
        let result = provider.sample_start_price(1500);
        assert!(result.is_some());
        assert_eq!(result.as_ref().unwrap().tick.seq, 1);
    }

    #[test]
    fn test_multiple_ticks_same_visible_ts() {
        let spec = SettlementReferenceSpec::polymarket_15m_updown_v1();

        // Multiple ticks at same visible_ts - should be ordered by seq
        let ticks = vec![
            make_tick(1, 1000, 50000.0),
            make_tick(2, 1000, 50050.0), // Same visible_ts, higher seq
            make_tick(3, 2000, 50100.0),
        ];
        let provider = RecordedReferenceStreamProvider::new(spec, ticks);

        // Should get seq=1 (first at visible_ts=1000)
        let result = provider.sample_start_price(1000);
        assert!(result.is_some());
        assert_eq!(result.as_ref().unwrap().tick.seq, 1);
    }

    #[test]
    fn test_settlement_engine_basic() {
        let spec = SettlementReferenceSpec::polymarket_15m_updown_v1();
        let window_duration = spec.window_duration_ns;
        let window_start: Nanos = 1000 * 1_000_000_000; // 1000 seconds in ns

        // Create ticks that span the window
        let ticks = vec![
            make_tick(1, window_start, 50000.0), // Start price
            make_tick(2, window_start + window_duration, 50100.0), // End price (Up wins)
        ];

        let provider = RecordedReferenceStreamProvider::new(spec, ticks);
        let mut engine = ReferenceSettlementEngine::new(provider);

        // Settle the window
        let decision_ts = window_start + window_duration + 1_000_000;
        let result = engine.settle_window(0, window_start, decision_ts);

        assert!(result.is_some());
        let record = result.unwrap();
        assert!(matches!(
            record.outcome,
            SettlementOutcomeResult::Up { is_tie: false }
        ));
        assert_eq!(record.start_tick_seq, 1);
        assert_eq!(record.end_tick_seq, 2);
        assert_eq!(engine.stats().up_wins, 1);
    }

    #[test]
    fn test_settlement_engine_missing_coverage() {
        let spec = SettlementReferenceSpec::polymarket_15m_updown_v1();
        let window_start: Nanos = 1000 * 1_000_000_000;

        // Only one tick - cannot cover window end
        let ticks = vec![make_tick(1, window_start, 50000.0)];

        let provider = RecordedReferenceStreamProvider::new(spec, ticks);
        let mut engine = ReferenceSettlementEngine::new(provider);

        let decision_ts = window_start + NANOS_15_MIN + 1_000_000;
        let result = engine.settle_window(0, window_start, decision_ts);

        assert!(result.is_none());
        assert_eq!(engine.stats().missing_end, 1);
    }

    #[test]
    fn test_audit_record_hash_determinism() {
        let spec = SettlementReferenceSpec::polymarket_15m_updown_v1();
        let window_start: Nanos = 1000 * 1_000_000_000;

        let ticks = vec![
            make_tick(1, window_start, 50000.0),
            make_tick(2, window_start + NANOS_15_MIN, 50100.0),
        ];

        let provider1 = RecordedReferenceStreamProvider::new(spec.clone(), ticks.clone());
        let provider2 = RecordedReferenceStreamProvider::new(spec, ticks);

        let mut engine1 = ReferenceSettlementEngine::new(provider1);
        let mut engine2 = ReferenceSettlementEngine::new(provider2);

        let decision_ts = window_start + NANOS_15_MIN + 1_000_000;
        let record1 = engine1.settle_window(0, window_start, decision_ts).unwrap();
        let record2 = engine2.settle_window(0, window_start, decision_ts).unwrap();

        // Hashes must be identical for identical inputs
        assert_eq!(record1.record_hash(), record2.record_hash());
        assert_eq!(engine1.audit_log_hash(), engine2.audit_log_hash());
    }

    #[test]
    fn test_coverage_classification() {
        let spec = SettlementReferenceSpec::polymarket_15m_updown_v1();
        let window_start: Nanos = 0;
        let window_end: Nanos = 2 * NANOS_15_MIN;

        // Full coverage - two windows, both covered
        let ticks = vec![
            make_tick(1, 0, 50000.0),
            make_tick(2, NANOS_15_MIN, 50100.0),
            make_tick(3, 2 * NANOS_15_MIN, 50200.0),
        ];
        let provider = RecordedReferenceStreamProvider::new(spec.clone(), ticks);
        let coverage = classify_settlement_coverage(&provider, window_start, window_end);
        assert_eq!(coverage, SettlementReferenceCoverage::Full);

        // Partial coverage - missing end tick
        let ticks = vec![
            make_tick(1, 0, 50000.0),
            make_tick(2, NANOS_15_MIN, 50100.0),
            // Missing tick at 2*NANOS_15_MIN
        ];
        let provider = RecordedReferenceStreamProvider::new(spec.clone(), ticks);
        let coverage = classify_settlement_coverage(&provider, window_start, window_end);
        assert_eq!(coverage, SettlementReferenceCoverage::Partial);

        // No coverage - empty stream
        let provider = RecordedReferenceStreamProvider::new(spec, vec![]);
        let coverage = classify_settlement_coverage(&provider, window_start, window_end);
        assert_eq!(coverage, SettlementReferenceCoverage::None);
    }
}

//! HFT-Grade Polymarket L2 Incremental Delta Model
//!
//! This module defines the canonical event types and book reconstruction logic for
//! deterministic replay of Polymarket CLOB L2 orderbook data.
//!
//! # Design Principles
//!
//! 1. **Monotonic Sequence Numbers**: Every delta carries a `seq` that must be strictly
//!    increasing within its scope (per-market or per-market-side).
//!
//! 2. **Deterministic Replay**: Given identical dataset files, replay produces identical
//!    book states at every point in simulated time.
//!
//! 3. **No Wall-Clock Time**: All timing is derived from recorded timestamps, never
//!    from system time during replay.
//!
//! 4. **Snapshot + Delta Contract**: Datasets begin with a full snapshot, followed by
//!    incremental deltas. Periodic snapshots enable gap healing and verification.
//!
//! 5. **Production-Grade Classification**: Trust gate only certifies runs where the
//!    L2 delta contract is fully satisfied.
//!
//! # Sequence Semantics
//!
//! Polymarket CLOB does not provide true exchange sequence numbers. Instead:
//! - Live recording assigns monotonic `ingest_seq` at arrival time
//! - The `seq_hash` field provides deduplication but not ordering
//! - Datasets using synthetic sequences are marked accordingly
//!
//! For production-grade backtests, we require:
//! - Real exchange sequences OR verified synthetic sequences
//! - No gaps in sequence numbers (or explicit gap healing via snapshot)
//! - Snapshot consistency validation

use crate::backtest_v2::clock::Nanos;
use crate::backtest_v2::events::{Event, Level, Side, TimestampedEvent};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::hash::{Hash, Hasher};

// =============================================================================
// SEQUENCE SCOPE
// =============================================================================

/// Defines the scope within which sequence numbers must be monotonic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SequenceScope {
    /// Single sequence per market (both bid/ask sides share one sequence).
    PerMarket,
    /// Separate sequences for bid and ask sides of each market.
    PerMarketSide,
}

impl Default for SequenceScope {
    fn default() -> Self {
        Self::PerMarket
    }
}

impl SequenceScope {
    /// Get the scope key for a given (market_id, side) pair.
    pub fn scope_key(&self, market_id: &str, side: Side) -> String {
        match self {
            Self::PerMarket => market_id.to_string(),
            Self::PerMarketSide => format!("{}:{:?}", market_id, side),
        }
    }
}

// =============================================================================
// SEQUENCE ORIGIN
// =============================================================================

/// Origin of sequence numbers in the dataset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SequenceOrigin {
    /// Sequence numbers are provided by the exchange (trusted).
    Exchange,
    /// Sequence numbers are synthesized from arrival order (less trusted).
    /// The dataset cannot be production-grade unless provably matches venue ordering.
    SyntheticFromArrival,
    /// Sequence numbers derived from exchange-provided hash (partial trust).
    DerivedFromHash,
    /// No meaningful sequence available.
    None,
}

impl Default for SequenceOrigin {
    fn default() -> Self {
        Self::SyntheticFromArrival
    }
}

impl SequenceOrigin {
    /// Check if this origin is production-grade.
    pub fn is_production_grade(&self) -> bool {
        matches!(self, Self::Exchange | Self::DerivedFromHash)
    }
}

// =============================================================================
// TICK SIZE AND PRICE CONVERSION
// =============================================================================

/// Fixed-point tick size for Polymarket (0.01 cents = 0.0001).
pub const POLYMARKET_TICK_SIZE: f64 = 0.0001;

/// Convert a floating-point price to tick units.
#[inline]
pub fn price_to_ticks(price: f64, tick_size: f64) -> i64 {
    (price / tick_size).round() as i64
}

/// Convert tick units back to floating-point price.
#[inline]
pub fn ticks_to_price(ticks: i64, tick_size: f64) -> f64 {
    ticks as f64 * tick_size
}

/// Canonical price level in tick space.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TickPriceLevel {
    /// Price in ticks (integer).
    pub price_ticks: i64,
    /// Size as fixed-point integer (size * 1e8).
    pub size_fp: i64,
}

impl TickPriceLevel {
    /// Size scale factor for fixed-point conversion.
    pub const SIZE_SCALE: f64 = 1e8;

    /// Create from floating-point values.
    pub fn from_float(price: f64, size: f64, tick_size: f64) -> Self {
        Self {
            price_ticks: price_to_ticks(price, tick_size),
            size_fp: (size * Self::SIZE_SCALE).round() as i64,
        }
    }

    /// Convert back to floating-point Level.
    pub fn to_level(&self, tick_size: f64) -> Level {
        Level {
            price: ticks_to_price(self.price_ticks, tick_size),
            size: self.size_fp as f64 / Self::SIZE_SCALE,
            order_count: None,
        }
    }

    /// Get size as float.
    pub fn size(&self) -> f64 {
        self.size_fp as f64 / Self::SIZE_SCALE
    }

    /// Check if this level has zero size (should be removed).
    pub fn is_empty(&self) -> bool {
        self.size_fp <= 0
    }
}

// =============================================================================
// EVENT TIME
// =============================================================================

/// Timestamp triplet for backtest events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EventTime {
    /// Exchange timestamp (if provided by venue).
    pub exchange_ts: Option<Nanos>,
    /// Ingest timestamp (when recorded by our system).
    pub ingest_ts: Nanos,
    /// Visible timestamp (when the strategy is allowed to see this event).
    /// Computed during replay based on latency model.
    pub visible_ts: Option<Nanos>,
}

impl EventTime {
    /// Create with ingest time only.
    pub fn ingest_only(ingest_ts: Nanos) -> Self {
        Self {
            exchange_ts: None,
            ingest_ts,
            visible_ts: None,
        }
    }

    /// Create with both exchange and ingest time.
    pub fn with_exchange(exchange_ts: Nanos, ingest_ts: Nanos) -> Self {
        Self {
            exchange_ts: Some(exchange_ts),
            ingest_ts,
            visible_ts: None,
        }
    }

    /// Get the canonical time for ordering (prefer ingest, fall back to exchange).
    pub fn canonical_time(&self) -> Nanos {
        self.visible_ts.unwrap_or(self.ingest_ts)
    }
}

// =============================================================================
// POLYMARKET L2 DELTA
// =============================================================================

/// Canonical Polymarket L2 incremental delta event.
///
/// Represents a single update to the orderbook at one price level.
/// The `size_delta` field can be:
/// - Absolute: new_size (with `is_absolute = true`)
/// - Relative: change in size (with `is_absolute = false`)
///
/// Polymarket CLOB uses absolute updates (new aggregate size at level).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PolymarketL2Delta {
    /// Market identifier (condition_id or token_id pair key).
    pub market_id: String,
    /// Token identifier (clobTokenId for outcome).
    pub token_id: String,
    /// Affected side (Buy = bid, Sell = ask).
    pub side: Side,
    /// Price level in ticks.
    pub price_ticks: i64,
    /// New size at this level (as fixed-point integer, size * 1e8).
    /// If `is_absolute`, this is the new total size.
    /// If not absolute, this is the delta to apply.
    pub size_fp: i64,
    /// Whether size_fp is absolute (new total) or relative (delta).
    pub is_absolute: bool,
    /// Sequence number within the scope (must be strictly increasing).
    pub seq: u64,
    /// Event timestamps.
    pub time: EventTime,
    /// Exchange-provided hash for integrity (if available).
    pub seq_hash: Option<String>,
}

impl PolymarketL2Delta {
    /// Create an absolute delta (new total size at level).
    pub fn absolute(
        market_id: String,
        token_id: String,
        side: Side,
        price_ticks: i64,
        size_fp: i64,
        seq: u64,
        time: EventTime,
        seq_hash: Option<String>,
    ) -> Self {
        Self {
            market_id,
            token_id,
            side,
            price_ticks,
            size_fp,
            is_absolute: true,
            seq,
            time,
            seq_hash,
        }
    }

    /// Create from floating-point values.
    pub fn from_float(
        market_id: String,
        token_id: String,
        side: Side,
        price: f64,
        new_size: f64,
        seq: u64,
        time: EventTime,
        seq_hash: Option<String>,
        tick_size: f64,
    ) -> Self {
        Self::absolute(
            market_id,
            token_id,
            side,
            price_to_ticks(price, tick_size),
            (new_size * TickPriceLevel::SIZE_SCALE).round() as i64,
            seq,
            time,
            seq_hash,
        )
    }

    /// Check if this delta removes the level (size becomes zero).
    pub fn is_level_removal(&self) -> bool {
        self.is_absolute && self.size_fp <= 0
    }

    /// Get price as float.
    pub fn price(&self, tick_size: f64) -> f64 {
        ticks_to_price(self.price_ticks, tick_size)
    }

    /// Get size as float.
    pub fn size(&self) -> f64 {
        self.size_fp as f64 / TickPriceLevel::SIZE_SCALE
    }

    /// Convert to backtest Event.
    pub fn to_event(&self, tick_size: f64) -> Event {
        Event::L2BookDelta {
            token_id: self.token_id.clone(),
            side: self.side,
            price: self.price(tick_size),
            new_size: self.size(),
            seq_hash: self.seq_hash.clone(),
        }
    }

    /// Convert to TimestampedEvent.
    pub fn to_timestamped_event(&self, tick_size: f64, source: u8) -> TimestampedEvent {
        TimestampedEvent {
            time: self.time.canonical_time(),
            source_time: self.time.exchange_ts.unwrap_or(self.time.ingest_ts),
            seq: self.seq,
            source,
            event: self.to_event(tick_size),
        }
    }
}

// =============================================================================
// POLYMARKET L2 SNAPSHOT
// =============================================================================

/// Full L2 orderbook snapshot.
///
/// Contains the complete state of both sides of the book at a point in time.
/// Used as the initial state and for periodic resync/verification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolymarketL2Snapshot {
    /// Market identifier.
    pub market_id: String,
    /// Token identifier.
    pub token_id: String,
    /// Sequence number (all subsequent deltas must have seq > seq_snapshot).
    pub seq_snapshot: u64,
    /// Bid levels sorted by price descending (best bid first).
    pub bids: Vec<TickPriceLevel>,
    /// Ask levels sorted by price ascending (best ask first).
    pub asks: Vec<TickPriceLevel>,
    /// Event timestamps.
    pub time: EventTime,
    /// Total bid depth (sum of all bid sizes).
    pub total_bid_depth_fp: i64,
    /// Total ask depth (sum of all ask sizes).
    pub total_ask_depth_fp: i64,
}

impl PolymarketL2Snapshot {
    /// Create from floating-point levels.
    pub fn from_levels(
        market_id: String,
        token_id: String,
        seq_snapshot: u64,
        bids: &[Level],
        asks: &[Level],
        time: EventTime,
        tick_size: f64,
    ) -> Self {
        let bids_ticks: Vec<TickPriceLevel> = bids
            .iter()
            .filter(|l| l.size > 0.0)
            .map(|l| TickPriceLevel::from_float(l.price, l.size, tick_size))
            .collect();

        let asks_ticks: Vec<TickPriceLevel> = asks
            .iter()
            .filter(|l| l.size > 0.0)
            .map(|l| TickPriceLevel::from_float(l.price, l.size, tick_size))
            .collect();

        let total_bid_depth_fp = bids_ticks.iter().map(|l| l.size_fp).sum();
        let total_ask_depth_fp = asks_ticks.iter().map(|l| l.size_fp).sum();

        Self {
            market_id,
            token_id,
            seq_snapshot,
            bids: bids_ticks,
            asks: asks_ticks,
            time,
            total_bid_depth_fp,
            total_ask_depth_fp,
        }
    }

    /// Convert to backtest Event.
    pub fn to_event(&self, tick_size: f64) -> Event {
        Event::L2BookSnapshot {
            token_id: self.token_id.clone(),
            bids: self.bids.iter().map(|l| l.to_level(tick_size)).collect(),
            asks: self.asks.iter().map(|l| l.to_level(tick_size)).collect(),
            exchange_seq: self.seq_snapshot,
        }
    }

    /// Convert to TimestampedEvent.
    pub fn to_timestamped_event(&self, tick_size: f64, source: u8) -> TimestampedEvent {
        TimestampedEvent {
            time: self.time.canonical_time(),
            source_time: self.time.exchange_ts.unwrap_or(self.time.ingest_ts),
            seq: self.seq_snapshot,
            source,
            event: self.to_event(tick_size),
        }
    }

    /// Get best bid price in ticks.
    pub fn best_bid_ticks(&self) -> Option<i64> {
        self.bids.first().map(|l| l.price_ticks)
    }

    /// Get best ask price in ticks.
    pub fn best_ask_ticks(&self) -> Option<i64> {
        self.asks.first().map(|l| l.price_ticks)
    }

    /// Get best bid price as float.
    pub fn best_bid(&self, tick_size: f64) -> Option<f64> {
        self.best_bid_ticks().map(|t| ticks_to_price(t, tick_size))
    }

    /// Get best ask price as float.
    pub fn best_ask(&self, tick_size: f64) -> Option<f64> {
        self.best_ask_ticks().map(|t| ticks_to_price(t, tick_size))
    }

    /// Check if book is crossed.
    pub fn is_crossed(&self) -> bool {
        match (self.best_bid_ticks(), self.best_ask_ticks()) {
            (Some(bid), Some(ask)) => bid >= ask,
            _ => false,
        }
    }

    /// Check if book is empty.
    pub fn is_empty(&self) -> bool {
        self.bids.is_empty() && self.asks.is_empty()
    }

    /// Compute a deterministic fingerprint of this snapshot.
    pub fn fingerprint(&self) -> BookFingerprint {
        BookFingerprint::from_snapshot(self)
    }
}

// =============================================================================
// BOOK FINGERPRINT
// =============================================================================

/// Deterministic hash of book state for reproducibility verification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BookFingerprint {
    /// Hash of the book state.
    pub hash: u64,
    /// Sequence number at which this fingerprint was computed.
    pub seq: u64,
    /// Number of bid levels.
    pub bid_levels: u32,
    /// Number of ask levels.
    pub ask_levels: u32,
}

impl BookFingerprint {
    /// Compute fingerprint from a snapshot.
    pub fn from_snapshot(snapshot: &PolymarketL2Snapshot) -> Self {
        use std::collections::hash_map::DefaultHasher;
        let mut hasher = DefaultHasher::new();

        // Hash market and token ID
        snapshot.market_id.hash(&mut hasher);
        snapshot.token_id.hash(&mut hasher);

        // Hash bids (sorted by price descending - already in that order)
        for level in &snapshot.bids {
            level.price_ticks.hash(&mut hasher);
            level.size_fp.hash(&mut hasher);
        }

        // Hash asks (sorted by price ascending - already in that order)
        for level in &snapshot.asks {
            level.price_ticks.hash(&mut hasher);
            level.size_fp.hash(&mut hasher);
        }

        Self {
            hash: hasher.finish(),
            seq: snapshot.seq_snapshot,
            bid_levels: snapshot.bids.len() as u32,
            ask_levels: snapshot.asks.len() as u32,
        }
    }

    /// Compute fingerprint from reconstructed book state.
    pub fn from_book_state(
        market_id: &str,
        token_id: &str,
        seq: u64,
        bids: &BTreeMap<i64, i64>,  // price_ticks -> size_fp
        asks: &BTreeMap<i64, i64>,
    ) -> Self {
        use std::collections::hash_map::DefaultHasher;
        let mut hasher = DefaultHasher::new();

        market_id.hash(&mut hasher);
        token_id.hash(&mut hasher);

        // Bids: iterate in reverse order (highest price first)
        for (&price_ticks, &size_fp) in bids.iter().rev() {
            if size_fp > 0 {
                price_ticks.hash(&mut hasher);
                size_fp.hash(&mut hasher);
            }
        }

        // Asks: iterate in natural order (lowest price first)
        for (&price_ticks, &size_fp) in asks.iter() {
            if size_fp > 0 {
                price_ticks.hash(&mut hasher);
                size_fp.hash(&mut hasher);
            }
        }

        Self {
            hash: hasher.finish(),
            seq,
            bid_levels: bids.values().filter(|&&s| s > 0).count() as u32,
            ask_levels: asks.values().filter(|&&s| s > 0).count() as u32,
        }
    }
}

// =============================================================================
// L2 DELTA CONTRACT
// =============================================================================

/// Enumeration of L2 delta contract requirements.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum L2DeltaContractRequirement {
    /// Initial snapshot exists for the market.
    InitialSnapshot,
    /// Sequences are monotonically increasing.
    MonotoneSeq,
    /// No gaps in sequence numbers (or gaps are healed).
    NoSeqGaps,
    /// Snapshot consistency verified.
    SnapshotConsistency,
    /// No negative sizes after applying deltas.
    NoNegativeSizes,
    /// Tick ordering is correct (bids descending, asks ascending).
    TickOrdering,
    /// Sequence numbers are from exchange (not synthetic).
    ExchangeSequence,
}

/// Result of L2 delta contract validation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct L2DeltaContractResult {
    /// Whether the contract is fully satisfied.
    pub satisfied: bool,
    /// Requirements that passed.
    pub passed: Vec<L2DeltaContractRequirement>,
    /// Requirements that failed with reasons.
    pub failed: Vec<(L2DeltaContractRequirement, String)>,
    /// Warnings (non-fatal issues).
    pub warnings: Vec<String>,
    /// Fingerprints at checkpoint sequences (for reproducibility verification).
    pub checkpoint_fingerprints: Vec<BookFingerprint>,
}

impl L2DeltaContractResult {
    /// Create a new empty result.
    pub fn new() -> Self {
        Self {
            satisfied: true,
            passed: Vec::new(),
            failed: Vec::new(),
            warnings: Vec::new(),
            checkpoint_fingerprints: Vec::new(),
        }
    }

    /// Mark a requirement as passed.
    pub fn pass(&mut self, req: L2DeltaContractRequirement) {
        self.passed.push(req);
    }

    /// Mark a requirement as failed.
    pub fn fail(&mut self, req: L2DeltaContractRequirement, reason: String) {
        self.satisfied = false;
        self.failed.push((req, reason));
    }

    /// Add a warning.
    pub fn warn(&mut self, msg: String) {
        self.warnings.push(msg);
    }

    /// Add a checkpoint fingerprint.
    pub fn add_fingerprint(&mut self, fp: BookFingerprint) {
        self.checkpoint_fingerprints.push(fp);
    }

    /// Check if a specific requirement passed.
    pub fn requirement_passed(&self, req: L2DeltaContractRequirement) -> bool {
        self.passed.contains(&req)
    }
}

impl Default for L2DeltaContractResult {
    fn default() -> Self {
        Self::new()
    }
}

// =============================================================================
// DETERMINISTIC BOOK STATE MACHINE
// =============================================================================

/// Reconstructed orderbook state machine for deterministic replay.
///
/// Maintains the full depth of the book in tick space and enforces
/// all invariants required for production-grade backtesting.
#[derive(Debug, Clone)]
pub struct DeterministicBook {
    /// Market identifier.
    pub market_id: String,
    /// Token identifier.
    pub token_id: String,
    /// Tick size for price conversion.
    pub tick_size: f64,
    /// Sequence scope for this book.
    pub seq_scope: SequenceScope,
    /// Last processed sequence number (per scope).
    last_seq: HashMap<String, u64>,
    /// Bid levels: price_ticks -> size_fp (sorted map, iterate in reverse for best bid).
    bids: BTreeMap<i64, i64>,
    /// Ask levels: price_ticks -> size_fp (sorted map, iterate naturally for best ask).
    asks: BTreeMap<i64, i64>,
    /// Total number of deltas applied.
    delta_count: u64,
    /// Total number of snapshots applied.
    snapshot_count: u64,
    /// Number of sequence gaps encountered.
    gap_count: u64,
    /// Whether a valid initial snapshot has been applied.
    has_initial_snapshot: bool,
    /// Gap policy: how to handle sequence gaps.
    gap_policy: GapPolicy,
}

/// Policy for handling sequence gaps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum GapPolicy {
    /// Hard fail on any gap.
    HardFail,
    /// Allow gaps if a snapshot immediately follows to heal.
    AllowIfHealed,
    /// Warn but continue (not production-grade).
    WarnAndContinue,
}

impl Default for GapPolicy {
    fn default() -> Self {
        Self::HardFail
    }
}

/// Error type for book operations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BookError {
    /// Delta sequence number is not monotonically increasing.
    NonMonotoneSeq { scope: String, expected_min: u64, actual: u64 },
    /// Gap in sequence numbers (missing deltas).
    SeqGap { scope: String, expected: u64, actual: u64, gap_size: u64 },
    /// Applying delta would result in negative size.
    NegativeSize { side: Side, price_ticks: i64, current: i64, delta: i64 },
    /// No initial snapshot provided.
    MissingInitialSnapshot,
    /// Book is crossed (best bid >= best ask).
    CrossedBook { best_bid_ticks: i64, best_ask_ticks: i64 },
    /// Snapshot consistency verification failed.
    SnapshotMismatch { expected_hash: u64, actual_hash: u64, seq: u64 },
    /// Generic error.
    Other(String),
}

impl std::fmt::Display for BookError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NonMonotoneSeq { scope, expected_min, actual } => {
                write!(f, "Non-monotone sequence in scope '{}': expected > {}, got {}", 
                       scope, expected_min, actual)
            }
            Self::SeqGap { scope, expected, actual, gap_size } => {
                write!(f, "Sequence gap in scope '{}': expected {}, got {} (gap={})",
                       scope, expected, actual, gap_size)
            }
            Self::NegativeSize { side, price_ticks, current, delta } => {
                write!(f, "Negative size on {:?} at ticks {}: current={}, delta={}",
                       side, price_ticks, current, delta)
            }
            Self::MissingInitialSnapshot => {
                write!(f, "No initial snapshot applied before deltas")
            }
            Self::CrossedBook { best_bid_ticks, best_ask_ticks } => {
                write!(f, "Crossed book: best_bid={} >= best_ask={}",
                       best_bid_ticks, best_ask_ticks)
            }
            Self::SnapshotMismatch { expected_hash, actual_hash, seq } => {
                write!(f, "Snapshot mismatch at seq {}: expected hash {:016x}, got {:016x}",
                       seq, expected_hash, actual_hash)
            }
            Self::Other(msg) => write!(f, "{}", msg),
        }
    }
}

impl std::error::Error for BookError {}

impl DeterministicBook {
    /// Create a new empty book.
    pub fn new(
        market_id: String,
        token_id: String,
        tick_size: f64,
        seq_scope: SequenceScope,
        gap_policy: GapPolicy,
    ) -> Self {
        Self {
            market_id,
            token_id,
            tick_size,
            seq_scope,
            last_seq: HashMap::new(),
            bids: BTreeMap::new(),
            asks: BTreeMap::new(),
            delta_count: 0,
            snapshot_count: 0,
            gap_count: 0,
            has_initial_snapshot: false,
            gap_policy,
        }
    }

    /// Apply a full snapshot (replaces entire book state).
    pub fn apply_snapshot(&mut self, snapshot: &PolymarketL2Snapshot) -> Result<(), BookError> {
        // Clear existing state
        self.bids.clear();
        self.asks.clear();

        // Apply bid levels
        for level in &snapshot.bids {
            if level.size_fp > 0 {
                self.bids.insert(level.price_ticks, level.size_fp);
            }
        }

        // Apply ask levels
        for level in &snapshot.asks {
            if level.size_fp > 0 {
                self.asks.insert(level.price_ticks, level.size_fp);
            }
        }

        // Update sequence tracking for all scopes
        let scope_key = self.seq_scope.scope_key(&snapshot.market_id, Side::Buy);
        self.last_seq.insert(scope_key.clone(), snapshot.seq_snapshot);
        
        if self.seq_scope == SequenceScope::PerMarketSide {
            let ask_scope = self.seq_scope.scope_key(&snapshot.market_id, Side::Sell);
            self.last_seq.insert(ask_scope, snapshot.seq_snapshot);
        }

        self.has_initial_snapshot = true;
        self.snapshot_count += 1;

        // Validate book is not crossed
        if self.is_crossed() {
            let best_bid = self.best_bid_ticks().unwrap_or(0);
            let best_ask = self.best_ask_ticks().unwrap_or(0);
            return Err(BookError::CrossedBook {
                best_bid_ticks: best_bid,
                best_ask_ticks: best_ask,
            });
        }

        Ok(())
    }

    /// Apply a single delta to the book.
    ///
    /// Returns Ok(()) if the delta was applied successfully, or Err if:
    /// - Sequence number is not monotonically increasing
    /// - Applying would result in negative size
    /// - Book becomes crossed (optionally)
    pub fn apply_delta(&mut self, delta: &PolymarketL2Delta) -> Result<(), BookError> {
        // Check for initial snapshot requirement
        if !self.has_initial_snapshot && self.gap_policy == GapPolicy::HardFail {
            return Err(BookError::MissingInitialSnapshot);
        }

        // Get scope key for sequence tracking
        let scope_key = self.seq_scope.scope_key(&delta.market_id, delta.side);

        // Check sequence monotonicity
        if let Some(&last) = self.last_seq.get(&scope_key) {
            if delta.seq <= last {
                return Err(BookError::NonMonotoneSeq {
                    scope: scope_key.clone(),
                    expected_min: last,
                    actual: delta.seq,
                });
            }

            // Check for gaps
            let expected = last + 1;
            if delta.seq > expected {
                let gap_size = delta.seq - expected;
                self.gap_count += 1;

                match self.gap_policy {
                    GapPolicy::HardFail => {
                        return Err(BookError::SeqGap {
                            scope: scope_key,
                            expected,
                            actual: delta.seq,
                            gap_size,
                        });
                    }
                    GapPolicy::AllowIfHealed => {
                        // Will be handled by subsequent snapshot
                        // For now, just track the gap
                    }
                    GapPolicy::WarnAndContinue => {
                        // Continue without error
                    }
                }
            }
        }

        // Apply the delta
        let levels = match delta.side {
            Side::Buy => &mut self.bids,
            Side::Sell => &mut self.asks,
        };

        if delta.is_absolute {
            // Absolute update: set new size directly
            if delta.size_fp <= 0 {
                levels.remove(&delta.price_ticks);
            } else {
                levels.insert(delta.price_ticks, delta.size_fp);
            }
        } else {
            // Relative update: apply delta to existing size
            let current = levels.get(&delta.price_ticks).copied().unwrap_or(0);
            let new_size = current + delta.size_fp;

            if new_size < 0 {
                return Err(BookError::NegativeSize {
                    side: delta.side,
                    price_ticks: delta.price_ticks,
                    current,
                    delta: delta.size_fp,
                });
            } else if new_size == 0 {
                levels.remove(&delta.price_ticks);
            } else {
                levels.insert(delta.price_ticks, new_size);
            }
        }

        // Update sequence tracking
        self.last_seq.insert(scope_key, delta.seq);
        self.delta_count += 1;

        Ok(())
    }

    /// Get best bid price in ticks.
    #[inline]
    pub fn best_bid_ticks(&self) -> Option<i64> {
        self.bids.keys().next_back().copied()
    }

    /// Get best ask price in ticks.
    #[inline]
    pub fn best_ask_ticks(&self) -> Option<i64> {
        self.asks.keys().next().copied()
    }

    /// Get best bid price as float.
    #[inline]
    pub fn best_bid(&self) -> Option<f64> {
        self.best_bid_ticks().map(|t| ticks_to_price(t, self.tick_size))
    }

    /// Get best ask price as float.
    #[inline]
    pub fn best_ask(&self) -> Option<f64> {
        self.best_ask_ticks().map(|t| ticks_to_price(t, self.tick_size))
    }

    /// Get mid price.
    #[inline]
    pub fn mid_price(&self) -> Option<f64> {
        match (self.best_bid(), self.best_ask()) {
            (Some(bid), Some(ask)) => Some((bid + ask) / 2.0),
            _ => None,
        }
    }

    /// Get spread in ticks.
    #[inline]
    pub fn spread_ticks(&self) -> Option<i64> {
        match (self.best_bid_ticks(), self.best_ask_ticks()) {
            (Some(bid), Some(ask)) => Some(ask - bid),
            _ => None,
        }
    }

    /// Check if book is crossed.
    #[inline]
    pub fn is_crossed(&self) -> bool {
        match (self.best_bid_ticks(), self.best_ask_ticks()) {
            (Some(bid), Some(ask)) => bid >= ask,
            _ => false,
        }
    }

    /// Check if book is empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.bids.is_empty() && self.asks.is_empty()
    }

    /// Get current sequence for a scope.
    pub fn current_seq(&self, scope_key: &str) -> Option<u64> {
        self.last_seq.get(scope_key).copied()
    }

    /// Get total delta count.
    pub fn delta_count(&self) -> u64 {
        self.delta_count
    }

    /// Get total snapshot count.
    pub fn snapshot_count(&self) -> u64 {
        self.snapshot_count
    }

    /// Get gap count.
    pub fn gap_count(&self) -> u64 {
        self.gap_count
    }

    /// Compute current fingerprint.
    pub fn fingerprint(&self) -> BookFingerprint {
        let seq = self.last_seq.values().max().copied().unwrap_or(0);
        BookFingerprint::from_book_state(
            &self.market_id,
            &self.token_id,
            seq,
            &self.bids,
            &self.asks,
        )
    }

    /// Verify that current state matches a snapshot at the same sequence.
    pub fn verify_snapshot(&self, snapshot: &PolymarketL2Snapshot) -> Result<(), BookError> {
        let current_fp = self.fingerprint();
        let expected_fp = snapshot.fingerprint();

        if current_fp.hash != expected_fp.hash {
            return Err(BookError::SnapshotMismatch {
                expected_hash: expected_fp.hash,
                actual_hash: current_fp.hash,
                seq: snapshot.seq_snapshot,
            });
        }

        Ok(())
    }

    /// Export current state as a snapshot.
    pub fn to_snapshot(&self, seq: u64, time: EventTime) -> PolymarketL2Snapshot {
        let bids: Vec<TickPriceLevel> = self.bids
            .iter()
            .rev()  // Highest price first
            .filter(|(_, &size)| size > 0)
            .map(|(&price_ticks, &size_fp)| TickPriceLevel { price_ticks, size_fp })
            .collect();

        let asks: Vec<TickPriceLevel> = self.asks
            .iter()  // Lowest price first
            .filter(|(_, &size)| size > 0)
            .map(|(&price_ticks, &size_fp)| TickPriceLevel { price_ticks, size_fp })
            .collect();

        let total_bid_depth_fp = bids.iter().map(|l| l.size_fp).sum();
        let total_ask_depth_fp = asks.iter().map(|l| l.size_fp).sum();

        PolymarketL2Snapshot {
            market_id: self.market_id.clone(),
            token_id: self.token_id.clone(),
            seq_snapshot: seq,
            bids,
            asks,
            time,
            total_bid_depth_fp,
            total_ask_depth_fp,
        }
    }

    /// Get top N bid levels.
    pub fn top_bids(&self, n: usize) -> Vec<Level> {
        self.bids
            .iter()
            .rev()
            .take(n)
            .map(|(&price_ticks, &size_fp)| Level {
                price: ticks_to_price(price_ticks, self.tick_size),
                size: size_fp as f64 / TickPriceLevel::SIZE_SCALE,
                order_count: None,
            })
            .collect()
    }

    /// Get top N ask levels.
    pub fn top_asks(&self, n: usize) -> Vec<Level> {
        self.asks
            .iter()
            .take(n)
            .map(|(&price_ticks, &size_fp)| Level {
                price: ticks_to_price(price_ticks, self.tick_size),
                size: size_fp as f64 / TickPriceLevel::SIZE_SCALE,
                order_count: None,
            })
            .collect()
    }

    /// Simulate market impact for a given order (walk the book).
    pub fn simulate_market_impact(&self, side: Side, size: f64) -> (f64, f64) {
        let size_fp = (size * TickPriceLevel::SIZE_SCALE).round() as i64;
        let levels = match side {
            Side::Buy => &self.asks,   // Buying crosses asks
            Side::Sell => &self.bids,  // Selling crosses bids
        };

        let mut remaining = size_fp;
        let mut total_cost_fp: i128 = 0;
        let mut total_filled_fp: i64 = 0;

        let iter: Box<dyn Iterator<Item = (&i64, &i64)>> = match side {
            Side::Buy => Box::new(levels.iter()),       // Lowest ask first
            Side::Sell => Box::new(levels.iter().rev()), // Highest bid first
        };

        for (&price_ticks, &level_size_fp) in iter {
            if remaining <= 0 {
                break;
            }
            let fill_size = remaining.min(level_size_fp);
            total_cost_fp += (fill_size as i128) * (price_ticks as i128);
            total_filled_fp += fill_size;
            remaining -= fill_size;
        }

        let avg_price = if total_filled_fp > 0 {
            (total_cost_fp as f64 / total_filled_fp as f64) * self.tick_size
        } else {
            0.0
        };

        let total_filled = total_filled_fp as f64 / TickPriceLevel::SIZE_SCALE;

        (avg_price, total_filled)
    }
}

// =============================================================================
// DATASET METADATA
// =============================================================================

/// Metadata describing an L2 delta dataset.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct L2DatasetMetadata {
    /// Dataset version.
    pub version: String,
    /// Market identifier.
    pub market_id: String,
    /// Token identifiers in this dataset.
    pub token_ids: Vec<String>,
    /// Tick size used for price encoding.
    pub tick_size: f64,
    /// Sequence scope (per-market or per-market-side).
    pub seq_scope: SequenceScope,
    /// Origin of sequence numbers.
    pub seq_origin: SequenceOrigin,
    /// Time range covered (start, end) in nanoseconds.
    pub time_range_ns: (Nanos, Nanos),
    /// Total number of snapshots.
    pub snapshot_count: u64,
    /// Total number of deltas.
    pub delta_count: u64,
    /// Whether the dataset has an initial snapshot for each token.
    pub has_initial_snapshots: bool,
    /// Sequence gaps detected (scope_key, gap_start, gap_end).
    pub sequence_gaps: Vec<(String, u64, u64)>,
    /// Checkpoint fingerprints for verification.
    pub checkpoint_fingerprints: Vec<BookFingerprint>,
    /// Recording timestamp.
    pub recorded_at: Nanos,
    /// Any warnings generated during recording.
    pub warnings: Vec<String>,
}

impl L2DatasetMetadata {
    /// Check if this dataset is production-grade.
    pub fn is_production_grade(&self) -> bool {
        self.seq_origin.is_production_grade()
            && self.has_initial_snapshots
            && self.sequence_gaps.is_empty()
    }

    /// Get a human-readable status summary.
    pub fn status_summary(&self) -> String {
        let grade = if self.is_production_grade() {
            "PRODUCTION_GRADE"
        } else {
            "NON_PRODUCTION"
        };
        
        format!(
            "{}: {} snapshots, {} deltas, {} gaps, seq_origin={:?}",
            grade,
            self.snapshot_count,
            self.delta_count,
            self.sequence_gaps.len(),
            self.seq_origin,
        )
    }
}

// =============================================================================
// TESTS
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn make_delta(
        seq: u64,
        side: Side,
        price_ticks: i64,
        size_fp: i64,
        ingest_ts: Nanos,
    ) -> PolymarketL2Delta {
        PolymarketL2Delta::absolute(
            "market1".to_string(),
            "token1".to_string(),
            side,
            price_ticks,
            size_fp,
            seq,
            EventTime::ingest_only(ingest_ts),
            None,
        )
    }

    fn make_snapshot(
        seq: u64,
        bids: Vec<(i64, i64)>,
        asks: Vec<(i64, i64)>,
        ingest_ts: Nanos,
    ) -> PolymarketL2Snapshot {
        PolymarketL2Snapshot {
            market_id: "market1".to_string(),
            token_id: "token1".to_string(),
            seq_snapshot: seq,
            bids: bids.into_iter().map(|(p, s)| TickPriceLevel { price_ticks: p, size_fp: s }).collect(),
            asks: asks.into_iter().map(|(p, s)| TickPriceLevel { price_ticks: p, size_fp: s }).collect(),
            time: EventTime::ingest_only(ingest_ts),
            total_bid_depth_fp: 0, // Will be recalculated if needed
            total_ask_depth_fp: 0,
        }
    }

    #[test]
    fn test_price_tick_conversion() {
        let tick_size = 0.0001;
        
        assert_eq!(price_to_ticks(0.55, tick_size), 5500);
        assert_eq!(price_to_ticks(0.4321, tick_size), 4321);
        assert_eq!(ticks_to_price(5500, tick_size), 0.55);
        assert_eq!(ticks_to_price(4321, tick_size), 0.4321);
    }

    #[test]
    fn test_tick_price_level() {
        let tick_size = 0.0001;
        let level = TickPriceLevel::from_float(0.55, 100.5, tick_size);
        
        assert_eq!(level.price_ticks, 5500);
        assert_eq!(level.size_fp, 10050000000);
        
        let back = level.to_level(tick_size);
        assert!((back.price - 0.55).abs() < 1e-9);
        assert!((back.size - 100.5).abs() < 1e-6);
    }

    #[test]
    fn test_deterministic_book_snapshot() {
        let mut book = DeterministicBook::new(
            "market1".to_string(),
            "token1".to_string(),
            0.0001,
            SequenceScope::PerMarket,
            GapPolicy::HardFail,
        );

        let snapshot = make_snapshot(
            1,
            vec![(4500, 1000_00000000), (4400, 2000_00000000)],  // Bids
            vec![(5500, 1500_00000000), (5600, 2500_00000000)],  // Asks
            1000000000,
        );

        book.apply_snapshot(&snapshot).unwrap();

        assert!(book.has_initial_snapshot);
        assert_eq!(book.best_bid_ticks(), Some(4500));
        assert_eq!(book.best_ask_ticks(), Some(5500));
        assert!(!book.is_crossed());
    }

    #[test]
    fn test_deterministic_book_delta() {
        let mut book = DeterministicBook::new(
            "market1".to_string(),
            "token1".to_string(),
            0.0001,
            SequenceScope::PerMarket,
            GapPolicy::HardFail,
        );

        // Apply initial snapshot
        let snapshot = make_snapshot(
            1,
            vec![(4500, 1000_00000000)],
            vec![(5500, 1500_00000000)],
            1000000000,
        );
        book.apply_snapshot(&snapshot).unwrap();

        // Apply delta: add new bid level
        let delta = make_delta(2, Side::Buy, 4600, 500_00000000, 1000001000);
        book.apply_delta(&delta).unwrap();

        assert_eq!(book.best_bid_ticks(), Some(4600)); // New best bid
        assert_eq!(book.delta_count(), 1);
    }

    #[test]
    fn test_monotone_sequence_enforcement() {
        let mut book = DeterministicBook::new(
            "market1".to_string(),
            "token1".to_string(),
            0.0001,
            SequenceScope::PerMarket,
            GapPolicy::HardFail,
        );

        let snapshot = make_snapshot(10, vec![(4500, 1000_00000000)], vec![(5500, 1500_00000000)], 1000000000);
        book.apply_snapshot(&snapshot).unwrap();

        // Try to apply delta with same sequence
        let delta = make_delta(10, Side::Buy, 4600, 500_00000000, 1000001000);
        let result = book.apply_delta(&delta);
        
        assert!(matches!(result, Err(BookError::NonMonotoneSeq { .. })));
    }

    #[test]
    fn test_sequence_gap_detection() {
        let mut book = DeterministicBook::new(
            "market1".to_string(),
            "token1".to_string(),
            0.0001,
            SequenceScope::PerMarket,
            GapPolicy::HardFail,
        );

        let snapshot = make_snapshot(1, vec![(4500, 1000_00000000)], vec![(5500, 1500_00000000)], 1000000000);
        book.apply_snapshot(&snapshot).unwrap();

        // Skip sequence 2, go directly to 5
        let delta = make_delta(5, Side::Buy, 4600, 500_00000000, 1000001000);
        let result = book.apply_delta(&delta);
        
        assert!(matches!(result, Err(BookError::SeqGap { gap_size: 3, .. })));
    }

    #[test]
    fn test_level_removal() {
        let mut book = DeterministicBook::new(
            "market1".to_string(),
            "token1".to_string(),
            0.0001,
            SequenceScope::PerMarket,
            GapPolicy::HardFail,
        );

        let snapshot = make_snapshot(1, vec![(4500, 1000_00000000)], vec![(5500, 1500_00000000)], 1000000000);
        book.apply_snapshot(&snapshot).unwrap();

        // Remove the bid level by setting size to 0
        let delta = make_delta(2, Side::Buy, 4500, 0, 1000001000);
        book.apply_delta(&delta).unwrap();

        assert_eq!(book.best_bid_ticks(), None); // Level removed
    }

    #[test]
    fn test_fingerprint_determinism() {
        let snapshot1 = make_snapshot(
            1,
            vec![(4500, 1000_00000000), (4400, 2000_00000000)],
            vec![(5500, 1500_00000000)],
            1000000000,
        );

        let snapshot2 = make_snapshot(
            1,
            vec![(4500, 1000_00000000), (4400, 2000_00000000)],
            vec![(5500, 1500_00000000)],
            2000000000, // Different time, same state
        );

        let fp1 = snapshot1.fingerprint();
        let fp2 = snapshot2.fingerprint();

        // Same book state should produce same hash
        assert_eq!(fp1.hash, fp2.hash);
    }

    #[test]
    fn test_fingerprint_sensitivity() {
        let snapshot1 = make_snapshot(
            1,
            vec![(4500, 1000_00000000)],
            vec![(5500, 1500_00000000)],
            1000000000,
        );

        let snapshot2 = make_snapshot(
            1,
            vec![(4500, 1000_00000001)], // Slightly different size
            vec![(5500, 1500_00000000)],
            1000000000,
        );

        let fp1 = snapshot1.fingerprint();
        let fp2 = snapshot2.fingerprint();

        // Different state should produce different hash
        assert_ne!(fp1.hash, fp2.hash);
    }

    #[test]
    fn test_snapshot_verification() {
        let mut book = DeterministicBook::new(
            "market1".to_string(),
            "token1".to_string(),
            0.0001,
            SequenceScope::PerMarket,
            GapPolicy::HardFail,
        );

        let snapshot = make_snapshot(
            1,
            vec![(4500, 1000_00000000), (4400, 2000_00000000)],
            vec![(5500, 1500_00000000)],
            1000000000,
        );

        book.apply_snapshot(&snapshot).unwrap();

        // Verification should pass
        book.verify_snapshot(&snapshot).unwrap();

        // Apply a delta
        let delta = make_delta(2, Side::Buy, 4600, 500_00000000, 1000001000);
        book.apply_delta(&delta).unwrap();

        // Verification against old snapshot should fail
        let result = book.verify_snapshot(&snapshot);
        assert!(matches!(result, Err(BookError::SnapshotMismatch { .. })));
    }

    #[test]
    fn test_crossed_book_detection() {
        let mut book = DeterministicBook::new(
            "market1".to_string(),
            "token1".to_string(),
            0.0001,
            SequenceScope::PerMarket,
            GapPolicy::HardFail,
        );

        // Create a crossed book (bid >= ask)
        let snapshot = make_snapshot(
            1,
            vec![(5500, 1000_00000000)], // Bid at 0.55
            vec![(5000, 1500_00000000)], // Ask at 0.50 (lower!)
            1000000000,
        );

        let result = book.apply_snapshot(&snapshot);
        assert!(matches!(result, Err(BookError::CrossedBook { .. })));
    }

    #[test]
    fn test_market_impact_simulation() {
        let mut book = DeterministicBook::new(
            "market1".to_string(),
            "token1".to_string(),
            0.0001,
            SequenceScope::PerMarket,
            GapPolicy::HardFail,
        );

        // 100 shares at 0.55, 200 shares at 0.56
        let snapshot = make_snapshot(
            1,
            vec![(4500, 1000_00000000)],
            vec![(5500, 100_00000000), (5600, 200_00000000)],
            1000000000,
        );
        book.apply_snapshot(&snapshot).unwrap();

        // Buy 150 shares
        let (avg_price, filled) = book.simulate_market_impact(Side::Buy, 150.0);

        assert!((filled - 150.0).abs() < 0.01);
        // Expected: 100 @ 0.55 + 50 @ 0.56 = (55 + 28) / 150 = 0.5533...
        let expected_avg = (100.0 * 0.55 + 50.0 * 0.56) / 150.0;
        assert!((avg_price - expected_avg).abs() < 0.0001);
    }

    #[test]
    fn test_per_market_side_sequence_scope() {
        let mut book = DeterministicBook::new(
            "market1".to_string(),
            "token1".to_string(),
            0.0001,
            SequenceScope::PerMarketSide,
            GapPolicy::HardFail,
        );

        let snapshot = make_snapshot(
            1,
            vec![(4500, 1000_00000000)],
            vec![(5500, 1500_00000000)],
            1000000000,
        );
        book.apply_snapshot(&snapshot).unwrap();

        // Both sides start at seq 1
        // Can apply bid delta at seq 2
        let bid_delta = make_delta(2, Side::Buy, 4600, 500_00000000, 1000001000);
        book.apply_delta(&bid_delta).unwrap();

        // Can ALSO apply ask delta at seq 2 (different scope)
        let ask_delta = make_delta(2, Side::Sell, 5400, 800_00000000, 1000002000);
        book.apply_delta(&ask_delta).unwrap();

        assert_eq!(book.delta_count(), 2);
    }

    #[test]
    fn test_l2_delta_contract_result() {
        let mut result = L2DeltaContractResult::new();
        
        result.pass(L2DeltaContractRequirement::InitialSnapshot);
        result.pass(L2DeltaContractRequirement::MonotoneSeq);
        result.fail(
            L2DeltaContractRequirement::ExchangeSequence,
            "Synthetic sequences used".to_string(),
        );
        result.warn("Dataset has 2 gaps that were healed by snapshots".to_string());

        assert!(!result.satisfied);
        assert_eq!(result.passed.len(), 2);
        assert_eq!(result.failed.len(), 1);
        assert_eq!(result.warnings.len(), 1);
    }
}

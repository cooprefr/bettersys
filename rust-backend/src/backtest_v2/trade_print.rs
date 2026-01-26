//! HFT-Grade Trade Print Recording and Replay
//!
//! This module implements the full Polymarket trade print stream for 15-minute
//! Up/Down strategy backtesting. Trade prints are essential for:
//!
//! 1. **Slippage modeling**: Attribute fill price deviations to observed market trades
//! 2. **Impact analysis**: Measure mid-move after fills using contemporaneous prints
//! 3. **Adverse selection**: Detect when fills precede adverse price movements
//! 4. **Queue consumption**: Track how much liquidity was consumed at each level
//!
//! # Why Full Prints Are Required for 15M Up/Down Backtests
//!
//! The 15M Up/Down strategy operates in markets with:
//! - Short windows (15 minutes) where every trade matters
//! - Binary outcomes where adverse selection is critical
//! - High-frequency trading where "last trade price" snapshots lose information
//!
//! Without full trade prints, we cannot:
//! - Distinguish between fills that preceded vs followed market trades
//! - Measure the actual spread-crossing activity during our order's lifetime
//! - Attribute slippage to genuine market impact vs coincidental price movement
//!
//! # Visible Time Governs Delivery
//!
//! Trade prints are delivered to strategies at their `visible_ts`, computed as:
//! ```text
//! visible_ts = ingest_ts + latency_model.polymarket_trade_delay_ns + jitter
//! ```
//!
//! Strategies NEVER see `exchange_ts` or `ingest_ts` directly. This enforces
//! realistic information arrival and prevents lookahead bias.
//!
//! # Determinism Contract
//!
//! Given identical:
//! - Dataset (trade prints)
//! - Latency model configuration
//! - Random seed (for jitter)
//!
//! The replay produces byte-identical event sequences across runs.

use crate::backtest_v2::event_time::{
    EventTime, FeedEvent, FeedEventPayload, FeedEventPriority, FeedSource, VisibleNanos,
};
use crate::backtest_v2::events::Side;
use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};

// =============================================================================
// CONSTANTS
// =============================================================================

/// Fixed-point scale for prices (8 decimal places for sub-cent precision).
pub const PRICE_SCALE: i64 = 100_000_000;

/// Fixed-point scale for sizes (8 decimal places for fractional shares).
pub const SIZE_SCALE: i64 = 100_000_000;

/// Default tick size for Polymarket (1 cent = 0.01).
pub const DEFAULT_TICK_SIZE: f64 = 0.01;

/// Maximum LRU cache size for deduplication per market.
pub const DEFAULT_DEDUP_CACHE_SIZE: usize = 10_000;

// =============================================================================
// TRADE ID SOURCE
// =============================================================================

/// Source of the trade identifier.
///
/// Determines the reliability of trade_id for deduplication and ordering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TradeIdSource {
    /// Native venue-provided unique trade ID.
    /// Highest trust - venue guarantees uniqueness.
    NativeVenueId,

    /// Composite key derived from venue fields.
    /// Medium trust - deterministic but may have edge cases.
    CompositeDerived,

    /// Hash-derived from immutable fields.
    /// Lower trust - collisions possible (though unlikely).
    HashDerived,

    /// Synthetic sequence generated at record time.
    /// Lowest trust - only guarantees arrival order, not venue order.
    Synthetic,
}

impl TradeIdSource {
    /// Whether this source provides reliable venue ordering.
    pub fn has_venue_ordering(&self) -> bool {
        matches!(self, Self::NativeVenueId | Self::CompositeDerived)
    }

    /// Whether this source should trigger trust downgrade for microstructure claims.
    pub fn requires_trust_downgrade(&self) -> bool {
        matches!(self, Self::Synthetic)
    }
}

// =============================================================================
// AGGRESSOR SIDE SOURCE
// =============================================================================

/// Source of the aggressor side information.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AggressorSideSource {
    /// Venue explicitly provides aggressor side.
    VenueProvided,

    /// Inferred from tick rule (trade price vs previous mid).
    InferredTickRule,

    /// Inferred from quote rule (trade price vs BBO at trade time).
    InferredQuoteRule,

    /// Unknown - could not determine aggressor.
    Unknown,
}

impl AggressorSideSource {
    /// Whether microstructure claims are valid with this source.
    pub fn supports_microstructure_claims(&self) -> bool {
        matches!(self, Self::VenueProvided | Self::InferredQuoteRule)
    }

    /// Description of inference method (if applicable).
    pub fn inference_rule(&self) -> Option<&'static str> {
        match self {
            Self::InferredTickRule => Some("tick_rule: buy if price > prev_price, sell if <"),
            Self::InferredQuoteRule => Some("quote_rule: buy if price >= ask, sell if <= bid"),
            _ => None,
        }
    }
}

// =============================================================================
// POLYMARKET TRADE PRINT (CANONICAL SCHEMA)
// =============================================================================

/// Canonical trade print event from Polymarket.
///
/// This struct represents a single executed match with a complete, stable schema
/// for HFT-grade backtesting. Every field is explicitly documented for auditability.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolymarketTradePrint {
    // -------------------------------------------------------------------------
    // MARKET IDENTIFICATION
    // -------------------------------------------------------------------------
    /// Market identifier (condition_id or canonical market key).
    /// Format: lowercase hex string for condition_id, or "btc-updown-15m-{window_start}".
    pub market_id: String,

    /// Token identifier (clobTokenId / asset_id).
    /// Large integer string representing the specific outcome token.
    pub token_id: String,

    /// Human-readable market slug (e.g., "btc-updown-15m-1705320000").
    pub market_slug: Option<String>,

    // -------------------------------------------------------------------------
    // TRADE IDENTIFICATION
    // -------------------------------------------------------------------------
    /// Unique trade identifier within the market.
    ///
    /// If venue provides a unique ID, use it directly.
    /// Otherwise, this is a deterministic hash of immutable fields.
    pub trade_id: String,

    /// Source of the trade_id.
    pub trade_id_source: TradeIdSource,

    /// Optional venue-provided match ID (if different from trade_id).
    pub match_id: Option<String>,

    /// Exchange sequence number (if venue provides).
    /// Guarantees ordering within a single market stream.
    pub trade_seq: Option<u64>,

    /// Synthetic sequence generated at record time.
    /// Used for deterministic ordering when trade_seq is unavailable.
    /// Strictly monotonic within (market_id, recorder_session).
    pub synthetic_trade_seq: u64,

    /// Whether trade_seq is synthetic (true) or native (false).
    pub sequence_is_synthetic: bool,

    // -------------------------------------------------------------------------
    // TRADE DATA
    // -------------------------------------------------------------------------
    /// Aggressor side (who crossed the spread).
    /// BUY = buyer initiated (lifted offers), SELL = seller initiated (hit bids).
    pub aggressor_side: Side,

    /// Source of aggressor side determination.
    pub aggressor_side_source: AggressorSideSource,

    /// Execution price in probability units (0.0 to 1.0).
    /// Stored as f64 but validated to be on tick grid.
    pub price: f64,

    /// Fixed-point price for deterministic hashing.
    /// Computed as: (price * PRICE_SCALE).round() as i64
    pub price_fixed: i64,

    /// Trade size in shares/contracts.
    pub size: f64,

    /// Fixed-point size for deterministic hashing.
    /// Computed as: (size * SIZE_SCALE).round() as i64
    pub size_fixed: i64,

    /// Fee rate in basis points (if available).
    pub fee_rate_bps: Option<i32>,

    // -------------------------------------------------------------------------
    // TIMESTAMPS (THREE-TIMESTAMP MODEL)
    // -------------------------------------------------------------------------
    /// Exchange timestamp in nanoseconds (if venue provides).
    /// May be None if venue only provides millisecond or no timestamp.
    pub exchange_ts_ns: Option<i64>,

    /// Ingest timestamp in nanoseconds (REQUIRED).
    /// Captured at the moment the recorder received the message.
    pub ingest_ts_ns: i64,

    /// Visible timestamp in nanoseconds.
    /// Computed by latency model: ingest_ts + delay + jitter.
    /// This is the ONLY time strategies should observe.
    pub visible_ts_ns: i64,

    // -------------------------------------------------------------------------
    // DATASET METADATA
    // -------------------------------------------------------------------------
    /// Local sequence within recorder session (for arrival ordering).
    pub local_seq: u64,

    /// Tick size for this market (for validation).
    pub tick_size: f64,

    /// Whether size is in shares (true) or contracts (false).
    pub size_unit_is_shares: bool,
}

impl PolymarketTradePrint {
    /// Create a new trade print with minimal required fields.
    pub fn new(
        market_id: String,
        token_id: String,
        aggressor_side: Side,
        price: f64,
        size: f64,
        ingest_ts_ns: i64,
    ) -> Self {
        let price_fixed = (price * PRICE_SCALE as f64).round() as i64;
        let size_fixed = (size * SIZE_SCALE as f64).round() as i64;

        Self {
            market_id,
            token_id,
            market_slug: None,
            trade_id: String::new(), // Must be set by builder
            trade_id_source: TradeIdSource::Synthetic,
            match_id: None,
            trade_seq: None,
            synthetic_trade_seq: 0,
            sequence_is_synthetic: true,
            aggressor_side,
            aggressor_side_source: AggressorSideSource::Unknown,
            price,
            price_fixed,
            size,
            size_fixed,
            fee_rate_bps: None,
            exchange_ts_ns: None,
            ingest_ts_ns,
            visible_ts_ns: ingest_ts_ns, // Default: zero latency
            local_seq: 0,
            tick_size: DEFAULT_TICK_SIZE,
            size_unit_is_shares: true,
        }
    }

    /// Compute deterministic hash-derived trade_id from immutable fields.
    pub fn compute_hash_trade_id(&self) -> String {
        let mut hasher = DefaultHasher::new();

        self.market_id.hash(&mut hasher);
        self.token_id.hash(&mut hasher);
        self.price_fixed.hash(&mut hasher);
        self.size_fixed.hash(&mut hasher);

        match self.aggressor_side {
            Side::Buy => "BUY".hash(&mut hasher),
            Side::Sell => "SELL".hash(&mut hasher),
        }

        if let Some(exchange_ts) = self.exchange_ts_ns {
            exchange_ts.hash(&mut hasher);
        }

        if let Some(ref match_id) = self.match_id {
            match_id.hash(&mut hasher);
        }

        format!("hash_{:016x}", hasher.finish())
    }

    /// Set trade_id from hash if not already set.
    pub fn ensure_trade_id(&mut self) {
        if self.trade_id.is_empty() {
            self.trade_id = self.compute_hash_trade_id();
            self.trade_id_source = TradeIdSource::HashDerived;
        }
    }

    /// Validate that price is on the tick grid.
    pub fn validate_price(&self) -> Result<(), TradePrintError> {
        if self.price < 0.0 || self.price > 1.0 {
            return Err(TradePrintError::InvalidPrice {
                price: self.price,
                reason: "price must be in [0.0, 1.0]",
            });
        }

        let ticks = self.price / self.tick_size;
        let rounded_ticks = ticks.round();
        let error = (ticks - rounded_ticks).abs();

        if error > 1e-9 {
            return Err(TradePrintError::PriceOffGrid {
                price: self.price,
                tick_size: self.tick_size,
                nearest_tick: rounded_ticks * self.tick_size,
            });
        }

        Ok(())
    }

    /// Validate that size is positive.
    pub fn validate_size(&self) -> Result<(), TradePrintError> {
        if self.size <= 0.0 {
            return Err(TradePrintError::InvalidSize {
                size: self.size,
                reason: "size must be positive",
            });
        }
        Ok(())
    }

    /// Validate all fields.
    pub fn validate(&self) -> Result<(), TradePrintError> {
        self.validate_price()?;
        self.validate_size()?;

        if self.trade_id.is_empty() {
            return Err(TradePrintError::MissingTradeId);
        }

        if self.ingest_ts_ns <= 0 {
            return Err(TradePrintError::InvalidTimestamp {
                field: "ingest_ts_ns",
                value: self.ingest_ts_ns,
            });
        }

        Ok(())
    }

    /// Convert to EventTime triple.
    pub fn to_event_time(&self) -> EventTime {
        EventTime::with_all(
            self.exchange_ts_ns,
            self.ingest_ts_ns,
            VisibleNanos::new(self.visible_ts_ns),
        )
    }

    /// Convert to FeedEvent for unified queue.
    pub fn to_feed_event(&self, dataset_seq: u64) -> FeedEvent {
        FeedEvent::new(
            self.to_event_time(),
            FeedSource::PolymarketTrade,
            FeedEventPriority::TradePrint,
            dataset_seq,
            FeedEventPayload::PolymarketTradePrint {
                token_id: self.token_id.clone(),
                market_slug: self.market_slug.clone().unwrap_or_default(),
                price: self.price,
                size: self.size,
                aggressor_side: match self.aggressor_side {
                    Side::Buy => crate::backtest_v2::event_time::BookSide::Ask, // Buy consumes asks
                    Side::Sell => crate::backtest_v2::event_time::BookSide::Bid, // Sell consumes bids
                },
                trade_id: Some(self.trade_id.clone()),
            },
        )
    }

    /// Which side of the book does this trade consume?
    /// BUY aggressor consumes asks (lifts offers).
    /// SELL aggressor consumes bids (hits bids).
    pub fn consumes_side(&self) -> Side {
        self.aggressor_side.opposite()
    }

    /// Notional value of the trade.
    pub fn notional(&self) -> f64 {
        self.price * self.size
    }

    /// Compute a deterministic fingerprint for replay validation.
    pub fn fingerprint(&self) -> u64 {
        let mut hasher = DefaultHasher::new();
        self.market_id.hash(&mut hasher);
        self.trade_id.hash(&mut hasher);
        self.price_fixed.hash(&mut hasher);
        self.size_fixed.hash(&mut hasher);
        self.visible_ts_ns.hash(&mut hasher);
        hasher.finish()
    }
}

// =============================================================================
// ERRORS
// =============================================================================

/// Errors related to trade print validation.
#[derive(Debug, Clone)]
pub enum TradePrintError {
    InvalidPrice {
        price: f64,
        reason: &'static str,
    },
    PriceOffGrid {
        price: f64,
        tick_size: f64,
        nearest_tick: f64,
    },
    InvalidSize {
        size: f64,
        reason: &'static str,
    },
    MissingTradeId,
    InvalidTimestamp {
        field: &'static str,
        value: i64,
    },
    DuplicateTradeId {
        market_id: String,
        trade_id: String,
    },
    SequenceViolation {
        market_id: String,
        expected_seq: u64,
        actual_seq: u64,
    },
}

impl std::fmt::Display for TradePrintError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidPrice { price, reason } => {
                write!(f, "invalid price {}: {}", price, reason)
            }
            Self::PriceOffGrid {
                price,
                tick_size,
                nearest_tick,
            } => {
                write!(
                    f,
                    "price {} is off tick grid (tick_size={}, nearest={})",
                    price, tick_size, nearest_tick
                )
            }
            Self::InvalidSize { size, reason } => {
                write!(f, "invalid size {}: {}", size, reason)
            }
            Self::MissingTradeId => write!(f, "trade_id is required"),
            Self::InvalidTimestamp { field, value } => {
                write!(f, "invalid timestamp {}: {}", field, value)
            }
            Self::DuplicateTradeId { market_id, trade_id } => {
                write!(
                    f,
                    "duplicate trade_id {} in market {}",
                    trade_id, market_id
                )
            }
            Self::SequenceViolation {
                market_id,
                expected_seq,
                actual_seq,
            } => {
                write!(
                    f,
                    "sequence violation in market {}: expected {}, got {}",
                    market_id, expected_seq, actual_seq
                )
            }
        }
    }
}

impl std::error::Error for TradePrintError {}

// =============================================================================
// DEDUPLICATION
// =============================================================================

/// Per-market deduplication state with bounded memory.
#[derive(Debug)]
pub struct MarketDedupState {
    /// Recently seen trade IDs (LRU cache).
    seen_ids: VecDeque<String>,
    /// Set for O(1) lookup.
    seen_set: HashSet<String>,
    /// Maximum cache size.
    max_size: usize,
    /// Counter for duplicates dropped.
    duplicates_dropped: u64,
}

impl MarketDedupState {
    pub fn new(max_size: usize) -> Self {
        Self {
            seen_ids: VecDeque::with_capacity(max_size),
            seen_set: HashSet::with_capacity(max_size),
            max_size,
            duplicates_dropped: 0,
        }
    }

    /// Check if a trade_id has been seen recently.
    /// Returns true if duplicate, false if new.
    pub fn check_and_insert(&mut self, trade_id: &str) -> bool {
        if self.seen_set.contains(trade_id) {
            self.duplicates_dropped += 1;
            return true; // Duplicate
        }

        // Evict oldest if at capacity
        if self.seen_ids.len() >= self.max_size {
            if let Some(old_id) = self.seen_ids.pop_front() {
                self.seen_set.remove(&old_id);
            }
        }

        // Insert new
        self.seen_ids.push_back(trade_id.to_string());
        self.seen_set.insert(trade_id.to_string());

        false // Not a duplicate
    }

    pub fn duplicates_dropped(&self) -> u64 {
        self.duplicates_dropped
    }
}

/// Trade print deduplicator with per-market state.
#[derive(Debug, Default)]
pub struct TradePrintDeduplicator {
    /// Per-market dedup state.
    markets: HashMap<String, MarketDedupState>,
    /// Default cache size for new markets.
    default_cache_size: usize,
    /// Total duplicates dropped across all markets.
    total_duplicates: AtomicU64,
}

impl TradePrintDeduplicator {
    pub fn new(default_cache_size: usize) -> Self {
        Self {
            markets: HashMap::new(),
            default_cache_size,
            total_duplicates: AtomicU64::new(0),
        }
    }

    /// Check if a trade is a duplicate. Returns true if duplicate.
    pub fn is_duplicate(&mut self, print: &PolymarketTradePrint) -> bool {
        let state = self
            .markets
            .entry(print.market_id.clone())
            .or_insert_with(|| MarketDedupState::new(self.default_cache_size));

        let is_dup = state.check_and_insert(&print.trade_id);
        if is_dup {
            self.total_duplicates.fetch_add(1, Ordering::Relaxed);
        }
        is_dup
    }

    pub fn total_duplicates(&self) -> u64 {
        self.total_duplicates.load(Ordering::Relaxed)
    }

    pub fn market_count(&self) -> usize {
        self.markets.len()
    }
}

// =============================================================================
// TRADE STREAM METADATA
// =============================================================================

/// Metadata about the trade stream for a market.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradeStreamMetadata {
    /// Market identifier.
    pub market_id: String,

    /// Encoding version for backward compatibility.
    pub encoding_version: u32,

    /// Tick size (e.g., 0.01 for 1 cent).
    pub tick_size: f64,

    /// Lot size (minimum trade size).
    pub lot_size: f64,

    /// Whether prices are in probability units (true) or cents (false).
    pub price_unit_is_probability: bool,

    /// Interpretation of size field.
    pub size_unit_is_shares: bool,

    /// Whether trade stream is present for this market.
    pub trade_stream_present: bool,

    /// Source of trade IDs.
    pub trade_id_source: TradeIdSource,

    /// Whether trade_seq is native (false) or synthetic (true).
    pub sequence_is_synthetic: bool,

    /// Source of aggressor side information.
    pub aggressor_side_source: AggressorSideSource,

    /// First trade timestamp in stream.
    pub first_trade_ts_ns: Option<i64>,

    /// Last trade timestamp in stream.
    pub last_trade_ts_ns: Option<i64>,

    /// Total trade count.
    pub trade_count: u64,

    /// Total volume traded.
    pub total_volume: f64,
}

impl Default for TradeStreamMetadata {
    fn default() -> Self {
        Self {
            market_id: String::new(),
            encoding_version: 1,
            tick_size: DEFAULT_TICK_SIZE,
            lot_size: 1.0,
            price_unit_is_probability: true,
            size_unit_is_shares: true,
            trade_stream_present: false,
            trade_id_source: TradeIdSource::Synthetic,
            sequence_is_synthetic: true,
            aggressor_side_source: AggressorSideSource::Unknown,
            first_trade_ts_ns: None,
            last_trade_ts_ns: None,
            trade_count: 0,
            total_volume: 0.0,
        }
    }
}

// =============================================================================
// TRADE PRINT BUILDER
// =============================================================================

/// Builder for constructing PolymarketTradePrint with validation.
#[derive(Debug, Default)]
pub struct TradePrintBuilder {
    market_id: Option<String>,
    token_id: Option<String>,
    market_slug: Option<String>,
    trade_id: Option<String>,
    trade_id_source: Option<TradeIdSource>,
    match_id: Option<String>,
    trade_seq: Option<u64>,
    aggressor_side: Option<Side>,
    aggressor_side_source: Option<AggressorSideSource>,
    price: Option<f64>,
    size: Option<f64>,
    fee_rate_bps: Option<i32>,
    exchange_ts_ns: Option<i64>,
    ingest_ts_ns: Option<i64>,
    tick_size: Option<f64>,
}

impl TradePrintBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn market_id(mut self, id: impl Into<String>) -> Self {
        self.market_id = Some(id.into());
        self
    }

    pub fn token_id(mut self, id: impl Into<String>) -> Self {
        self.token_id = Some(id.into());
        self
    }

    pub fn market_slug(mut self, slug: impl Into<String>) -> Self {
        self.market_slug = Some(slug.into());
        self
    }

    pub fn trade_id(mut self, id: impl Into<String>, source: TradeIdSource) -> Self {
        self.trade_id = Some(id.into());
        self.trade_id_source = Some(source);
        self
    }

    pub fn match_id(mut self, id: impl Into<String>) -> Self {
        self.match_id = Some(id.into());
        self
    }

    pub fn trade_seq(mut self, seq: u64) -> Self {
        self.trade_seq = Some(seq);
        self
    }

    pub fn aggressor_side(mut self, side: Side, source: AggressorSideSource) -> Self {
        self.aggressor_side = Some(side);
        self.aggressor_side_source = Some(source);
        self
    }

    pub fn price(mut self, price: f64) -> Self {
        self.price = Some(price);
        self
    }

    pub fn size(mut self, size: f64) -> Self {
        self.size = Some(size);
        self
    }

    pub fn fee_rate_bps(mut self, fee: i32) -> Self {
        self.fee_rate_bps = Some(fee);
        self
    }

    pub fn exchange_ts_ns(mut self, ts: i64) -> Self {
        self.exchange_ts_ns = Some(ts);
        self
    }

    pub fn ingest_ts_ns(mut self, ts: i64) -> Self {
        self.ingest_ts_ns = Some(ts);
        self
    }

    pub fn tick_size(mut self, ts: f64) -> Self {
        self.tick_size = Some(ts);
        self
    }

    pub fn build(self) -> Result<PolymarketTradePrint, String> {
        let market_id = self.market_id.ok_or("market_id is required")?;
        let token_id = self.token_id.ok_or("token_id is required")?;
        let aggressor_side = self.aggressor_side.ok_or("aggressor_side is required")?;
        let price = self.price.ok_or("price is required")?;
        let size = self.size.ok_or("size is required")?;
        let ingest_ts_ns = self.ingest_ts_ns.ok_or("ingest_ts_ns is required")?;

        let tick_size = self.tick_size.unwrap_or(DEFAULT_TICK_SIZE);
        let price_fixed = (price * PRICE_SCALE as f64).round() as i64;
        let size_fixed = (size * SIZE_SCALE as f64).round() as i64;

        let mut print = PolymarketTradePrint {
            market_id,
            token_id,
            market_slug: self.market_slug,
            trade_id: self.trade_id.unwrap_or_default(),
            trade_id_source: self.trade_id_source.unwrap_or(TradeIdSource::Synthetic),
            match_id: self.match_id,
            trade_seq: self.trade_seq,
            synthetic_trade_seq: 0,
            sequence_is_synthetic: self.trade_seq.is_none(),
            aggressor_side,
            aggressor_side_source: self
                .aggressor_side_source
                .unwrap_or(AggressorSideSource::Unknown),
            price,
            price_fixed,
            size,
            size_fixed,
            fee_rate_bps: self.fee_rate_bps,
            exchange_ts_ns: self.exchange_ts_ns,
            ingest_ts_ns,
            visible_ts_ns: ingest_ts_ns,
            local_seq: 0,
            tick_size,
            size_unit_is_shares: true,
        };

        // Ensure trade_id is set
        print.ensure_trade_id();

        Ok(print)
    }
}

// =============================================================================
// TRADE PRINT SEQUENCE TRACKER
// =============================================================================

/// Tracks sequence numbers per market for ordering validation.
#[derive(Debug, Default)]
pub struct TradeSequenceTracker {
    /// Per-market sequence state.
    sequences: HashMap<String, SequenceState>,
}

#[derive(Debug, Clone, Default)]
struct SequenceState {
    /// Last native trade_seq seen.
    last_native_seq: Option<u64>,
    /// Next synthetic sequence to assign.
    next_synthetic_seq: u64,
    /// Count of sequence gaps (for native seq).
    gaps_detected: u64,
}

impl TradeSequenceTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Assign synthetic sequence and validate native sequence if present.
    pub fn process(&mut self, print: &mut PolymarketTradePrint) -> Option<TradePrintError> {
        let state = self
            .sequences
            .entry(print.market_id.clone())
            .or_default();

        // Assign synthetic sequence (always)
        print.synthetic_trade_seq = state.next_synthetic_seq;
        state.next_synthetic_seq += 1;

        // Validate native sequence if present
        if let Some(native_seq) = print.trade_seq {
            if let Some(last_seq) = state.last_native_seq {
                if native_seq <= last_seq {
                    return Some(TradePrintError::SequenceViolation {
                        market_id: print.market_id.clone(),
                        expected_seq: last_seq + 1,
                        actual_seq: native_seq,
                    });
                }
                if native_seq > last_seq + 1 {
                    state.gaps_detected += 1;
                }
            }
            state.last_native_seq = Some(native_seq);
            print.sequence_is_synthetic = false;
        } else {
            print.sequence_is_synthetic = true;
        }

        None
    }

    pub fn gaps_detected(&self, market_id: &str) -> u64 {
        self.sequences
            .get(market_id)
            .map(|s| s.gaps_detected)
            .unwrap_or(0)
    }

    pub fn total_gaps(&self) -> u64 {
        self.sequences.values().map(|s| s.gaps_detected).sum()
    }
}

// =============================================================================
// TESTS
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_print(market_id: &str, trade_id: &str, price: f64, size: f64) -> PolymarketTradePrint {
        TradePrintBuilder::new()
            .market_id(market_id)
            .token_id("token_123")
            .trade_id(trade_id, TradeIdSource::NativeVenueId)
            .aggressor_side(Side::Buy, AggressorSideSource::VenueProvided)
            .price(price)
            .size(size)
            .ingest_ts_ns(1_000_000_000)
            .build()
            .unwrap()
    }

    #[test]
    fn test_trade_print_creation() {
        let print = make_test_print("market_1", "trade_001", 0.55, 100.0);

        assert_eq!(print.market_id, "market_1");
        assert_eq!(print.trade_id, "trade_001");
        assert_eq!(print.price, 0.55);
        assert_eq!(print.size, 100.0);
        assert_eq!(print.aggressor_side, Side::Buy);
        assert_eq!(print.consumes_side(), Side::Sell); // Buy consumes asks
    }

    #[test]
    fn test_fixed_point_conversion() {
        let print = make_test_print("market_1", "trade_001", 0.55, 123.456);

        assert_eq!(print.price_fixed, 55_000_000); // 0.55 * 10^8
        assert_eq!(print.size_fixed, 12_345_600_000); // 123.456 * 10^8
    }

    #[test]
    fn test_price_validation_on_grid() {
        let print = make_test_print("market_1", "trade_001", 0.55, 100.0);
        assert!(print.validate_price().is_ok());
    }

    #[test]
    fn test_price_validation_off_grid() {
        let mut print = make_test_print("market_1", "trade_001", 0.555, 100.0);
        print.tick_size = 0.01;
        // 0.555 is off the 0.01 grid
        assert!(print.validate_price().is_err());
    }

    #[test]
    fn test_price_validation_out_of_range() {
        let mut print = make_test_print("market_1", "trade_001", 1.5, 100.0);
        assert!(print.validate_price().is_err());

        print.price = -0.1;
        print.price_fixed = -10_000_000;
        assert!(print.validate_price().is_err());
    }

    #[test]
    fn test_hash_trade_id() {
        let mut print = PolymarketTradePrint::new(
            "market_1".to_string(),
            "token_1".to_string(),
            Side::Buy,
            0.50,
            100.0,
            1_000_000_000,
        );

        print.ensure_trade_id();
        assert!(print.trade_id.starts_with("hash_"));
        assert_eq!(print.trade_id_source, TradeIdSource::HashDerived);

        // Same inputs should produce same hash
        let mut print2 = PolymarketTradePrint::new(
            "market_1".to_string(),
            "token_1".to_string(),
            Side::Buy,
            0.50,
            100.0,
            1_000_000_000,
        );
        print2.ensure_trade_id();
        assert_eq!(print.trade_id, print2.trade_id);
    }

    #[test]
    fn test_deduplicator() {
        let mut dedup = TradePrintDeduplicator::new(100);

        let print1 = make_test_print("market_1", "trade_001", 0.50, 100.0);
        let print2 = make_test_print("market_1", "trade_002", 0.51, 50.0);

        assert!(!dedup.is_duplicate(&print1)); // First time - not duplicate
        assert!(dedup.is_duplicate(&print1)); // Second time - duplicate
        assert!(!dedup.is_duplicate(&print2)); // Different trade - not duplicate

        assert_eq!(dedup.total_duplicates(), 1);
    }

    #[test]
    fn test_deduplicator_lru_eviction() {
        let mut dedup = TradePrintDeduplicator::new(3);

        // Insert 4 trades
        for i in 1..=4 {
            let print = make_test_print("market_1", &format!("trade_{:03}", i), 0.50, 100.0);
            assert!(!dedup.is_duplicate(&print));
        }

        // trade_001 should have been evicted
        let print1 = make_test_print("market_1", "trade_001", 0.50, 100.0);
        assert!(!dedup.is_duplicate(&print1)); // Was evicted, so not a duplicate

        // trade_004 should still be cached
        let print4 = make_test_print("market_1", "trade_004", 0.50, 100.0);
        assert!(dedup.is_duplicate(&print4)); // Still cached
    }

    #[test]
    fn test_sequence_tracker() {
        let mut tracker = TradeSequenceTracker::new();

        let mut print1 = make_test_print("market_1", "trade_001", 0.50, 100.0);
        print1.trade_seq = Some(1);

        let mut print2 = make_test_print("market_1", "trade_002", 0.51, 50.0);
        print2.trade_seq = Some(2);

        assert!(tracker.process(&mut print1).is_none());
        assert!(tracker.process(&mut print2).is_none());

        assert_eq!(print1.synthetic_trade_seq, 0);
        assert_eq!(print2.synthetic_trade_seq, 1);
        assert!(!print1.sequence_is_synthetic);
        assert!(!print2.sequence_is_synthetic);
    }

    #[test]
    fn test_sequence_tracker_gap_detection() {
        let mut tracker = TradeSequenceTracker::new();

        let mut print1 = make_test_print("market_1", "trade_001", 0.50, 100.0);
        print1.trade_seq = Some(1);

        let mut print3 = make_test_print("market_1", "trade_003", 0.51, 50.0);
        print3.trade_seq = Some(3); // Gap: missing seq=2

        assert!(tracker.process(&mut print1).is_none());
        assert!(tracker.process(&mut print3).is_none());

        assert_eq!(tracker.gaps_detected("market_1"), 1);
    }

    #[test]
    fn test_sequence_tracker_out_of_order() {
        let mut tracker = TradeSequenceTracker::new();

        let mut print2 = make_test_print("market_1", "trade_002", 0.50, 100.0);
        print2.trade_seq = Some(2);

        let mut print1 = make_test_print("market_1", "trade_001", 0.51, 50.0);
        print1.trade_seq = Some(1); // Out of order (1 after 2)

        assert!(tracker.process(&mut print2).is_none());
        assert!(tracker.process(&mut print1).is_some()); // Should error
    }

    #[test]
    fn test_to_feed_event() {
        let print = make_test_print("market_1", "trade_001", 0.55, 100.0);
        let event = print.to_feed_event(42);

        assert_eq!(event.dataset_seq, 42);
        assert_eq!(event.source, FeedSource::PolymarketTrade);
        assert_eq!(event.priority, FeedEventPriority::TradePrint);

        if let FeedEventPayload::PolymarketTradePrint {
            token_id,
            price,
            size,
            ..
        } = &event.payload
        {
            assert_eq!(token_id, "token_123");
            assert_eq!(*price, 0.55);
            assert_eq!(*size, 100.0);
        } else {
            panic!("Wrong payload type");
        }
    }

    #[test]
    fn test_trade_id_source_properties() {
        assert!(TradeIdSource::NativeVenueId.has_venue_ordering());
        assert!(TradeIdSource::CompositeDerived.has_venue_ordering());
        assert!(!TradeIdSource::HashDerived.has_venue_ordering());
        assert!(!TradeIdSource::Synthetic.has_venue_ordering());

        assert!(!TradeIdSource::NativeVenueId.requires_trust_downgrade());
        assert!(TradeIdSource::Synthetic.requires_trust_downgrade());
    }

    #[test]
    fn test_aggressor_side_source_properties() {
        assert!(AggressorSideSource::VenueProvided.supports_microstructure_claims());
        assert!(AggressorSideSource::InferredQuoteRule.supports_microstructure_claims());
        assert!(!AggressorSideSource::InferredTickRule.supports_microstructure_claims());
        assert!(!AggressorSideSource::Unknown.supports_microstructure_claims());

        assert!(AggressorSideSource::InferredTickRule.inference_rule().is_some());
        assert!(AggressorSideSource::VenueProvided.inference_rule().is_none());
    }

    #[test]
    fn test_fingerprint_determinism() {
        let print1 = make_test_print("market_1", "trade_001", 0.55, 100.0);
        let print2 = make_test_print("market_1", "trade_001", 0.55, 100.0);

        assert_eq!(print1.fingerprint(), print2.fingerprint());
    }

    #[test]
    fn test_notional_calculation() {
        let print = make_test_print("market_1", "trade_001", 0.50, 200.0);
        assert!((print.notional() - 100.0).abs() < 1e-9);
    }
}

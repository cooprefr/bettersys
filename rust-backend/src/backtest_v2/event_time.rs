//! Event Time Model for 15-Minute Up/Down Strategy Backtesting
//!
//! # Three-Timestamp Model
//!
//! Every incoming event from any feed carries three distinct timestamps:
//!
//! 1. **`exchange_ts_ns`** (optional): The venue-provided timestamp
//!    - Binance: `event_time` from WebSocket messages
//!    - Polymarket: on-chain or API-provided timestamp (if present)
//!    - May be missing or unreliable for some event types
//!
//! 2. **`ingest_ts_ns`** (required): Local time when the recorder captured the event
//!    - Historical "arrival at our system" time
//!    - Part of the dataset schema, required for HFT-grade replay
//!    - Used as the basis for computing visible_ts
//!
//! 3. **`visible_ts_ns`** (required): Time at which the backtest makes the event visible
//!    - Computed as: `ingest_ts + latency_model.delay + jitter`
//!    - The ONLY time the SimClock advances to
//!    - The ONLY time strategies are allowed to observe
//!
//! # Determinism Contract
//!
//! - `visible_ts` is computed deterministically from `ingest_ts` + latency model
//! - Jitter (if any) is a pure function of (seed, event fingerprint)
//! - Replay produces identical results given identical dataset + seed
//!
//! # Strategy Isolation
//!
//! Strategies MUST NOT access `exchange_ts` or `ingest_ts` directly.
//! The only time exposed via `StrategyContext` is `visible_ts`.
//! This is enforced at the type level where possible.

use crate::backtest_v2::clock::Nanos;
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

/// Nanosecond constants for convenience.
pub const NS_PER_US: i64 = 1_000;
pub const NS_PER_MS: i64 = 1_000_000;
pub const NS_PER_SEC: i64 = 1_000_000_000;
pub const NANOS_15_MIN: i64 = 15 * 60 * NS_PER_SEC;

// =============================================================================
// STRONGLY-TYPED TIMESTAMP WRAPPERS
// =============================================================================

/// Visible time in nanoseconds - the ONLY time strategies should see.
///
/// This wrapper enforces at the type level that strategies cannot
/// accidentally use exchange or ingest timestamps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[repr(transparent)]
pub struct VisibleNanos(pub i64);

impl VisibleNanos {
    #[inline]
    pub const fn new(ns: i64) -> Self {
        Self(ns)
    }

    #[inline]
    pub const fn as_nanos(self) -> i64 {
        self.0
    }

    #[inline]
    pub fn as_secs(self) -> i64 {
        self.0 / NS_PER_SEC
    }

    #[inline]
    pub fn as_millis(self) -> i64 {
        self.0 / NS_PER_MS
    }

    /// Compute the 15-minute window start for this visible time.
    #[inline]
    pub fn window_start(self) -> VisibleNanos {
        VisibleNanos((self.0 / NANOS_15_MIN) * NANOS_15_MIN)
    }

    /// Compute the 15-minute window end for this visible time.
    #[inline]
    pub fn window_end(self) -> VisibleNanos {
        VisibleNanos(self.window_start().0 + NANOS_15_MIN)
    }

    /// Remaining time in the current window (in seconds).
    #[inline]
    pub fn remaining_secs(self) -> f64 {
        let remaining_ns = self.window_end().0 - self.0;
        (remaining_ns.max(0) as f64) / (NS_PER_SEC as f64)
    }
}

impl Default for VisibleNanos {
    fn default() -> Self {
        Self(0)
    }
}

impl std::fmt::Display for VisibleNanos {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let secs = self.0 / NS_PER_SEC;
        let nanos = self.0 % NS_PER_SEC;
        write!(f, "{}.{:09}s", secs, nanos)
    }
}

impl From<VisibleNanos> for Nanos {
    fn from(v: VisibleNanos) -> Nanos {
        v.0
    }
}

// =============================================================================
// EVENT TIME TRIPLE
// =============================================================================

/// Complete timestamp triple for any backtest event.
///
/// This struct carries all three timestamps required for HFT-grade backtesting.
/// It is the canonical representation for event timing in the unified feed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventTime {
    /// Venue-provided timestamp (if available).
    /// - Binance: `event_time` from WS messages
    /// - Polymarket: on-chain timestamp or API timestamp
    /// - `None` if the venue does not provide a reliable timestamp
    pub exchange_ts: Option<Nanos>,

    /// Local ingest timestamp when the recorder captured this event.
    /// This is REQUIRED for HFT-grade backtesting.
    /// For legacy datasets, this may be synthetically derived.
    pub ingest_ts: Nanos,

    /// Visible timestamp - when the strategy may observe this event.
    /// Computed as: `ingest_ts + latency_delay + jitter`
    /// This is the ONLY time exposed to strategy code.
    pub visible_ts: VisibleNanos,
}

impl EventTime {
    /// Create a new EventTime with all timestamps set to the same value.
    /// Useful for zero-latency testing.
    pub fn instant(ts: Nanos) -> Self {
        Self {
            exchange_ts: Some(ts),
            ingest_ts: ts,
            visible_ts: VisibleNanos(ts),
        }
    }

    /// Create a new EventTime with synthetic ingest (exchange_ts derived).
    /// Marks that this event lacks true ingest timestamp.
    pub fn synthetic_ingest(exchange_ts: Nanos) -> Self {
        Self {
            exchange_ts: Some(exchange_ts),
            ingest_ts: exchange_ts, // Synthetic: use exchange as ingest
            visible_ts: VisibleNanos(exchange_ts),
        }
    }

    /// Create EventTime with all three timestamps explicitly provided.
    pub fn with_all(exchange_ts: Option<Nanos>, ingest_ts: Nanos, visible_ts: VisibleNanos) -> Self {
        Self {
            exchange_ts,
            ingest_ts,
            visible_ts,
        }
    }

    /// Validate that visible_ts >= ingest_ts (no negative delays).
    pub fn validate(&self) -> Result<(), EventTimeError> {
        if self.visible_ts.0 < self.ingest_ts {
            return Err(EventTimeError::NegativeDelay {
                ingest_ts: self.ingest_ts,
                visible_ts: self.visible_ts,
            });
        }
        Ok(())
    }

    /// Get the latency from ingest to visible (in nanoseconds).
    pub fn latency_ns(&self) -> i64 {
        self.visible_ts.0 - self.ingest_ts
    }
}

impl Default for EventTime {
    fn default() -> Self {
        Self {
            exchange_ts: None,
            ingest_ts: 0,
            visible_ts: VisibleNanos(0),
        }
    }
}

/// Errors related to event time validation.
#[derive(Debug, Clone)]
pub enum EventTimeError {
    /// visible_ts < ingest_ts (negative delay not allowed)
    NegativeDelay {
        ingest_ts: Nanos,
        visible_ts: VisibleNanos,
    },
    /// Missing required ingest timestamp
    MissingIngestTs,
    /// Invalid timestamp value
    InvalidTimestamp { field: &'static str, value: Nanos },
}

impl std::fmt::Display for EventTimeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NegativeDelay {
                ingest_ts,
                visible_ts,
            } => {
                write!(
                    f,
                    "negative delay: visible_ts ({}) < ingest_ts ({})",
                    visible_ts, ingest_ts
                )
            }
            Self::MissingIngestTs => write!(f, "missing required ingest_ts"),
            Self::InvalidTimestamp { field, value } => {
                write!(f, "invalid timestamp for {}: {}", field, value)
            }
        }
    }
}

impl std::error::Error for EventTimeError {}

// =============================================================================
// FEED SOURCE IDENTIFICATION
// =============================================================================

/// Identifies the source feed for an event (for deterministic tie-breaking).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum FeedSource {
    /// Binance price feed (BTC/ETH/SOL/XRP mid prices)
    Binance = 0,
    /// Polymarket L2 book updates
    PolymarketBook = 1,
    /// Polymarket trade prints
    PolymarketTrade = 2,
    /// Chainlink oracle updates (for settlement)
    ChainlinkOracle = 3,
    /// Internal timer events
    Timer = 4,
    /// Order management events (fills, acks, rejects)
    OrderManagement = 5,
}

impl FeedSource {
    pub fn as_u8(self) -> u8 {
        self as u8
    }
}

// =============================================================================
// UNIFIED FEED EVENT
// =============================================================================

/// Event priority class for deterministic ordering within same timestamp.
/// Lower value = higher priority (processed first).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum FeedEventPriority {
    /// System events (halts, resolutions) - highest priority
    System = 0,
    /// Reference price updates (Binance mid) - needed before book updates
    ReferencePrice = 1,
    /// Book snapshots
    BookSnapshot = 2,
    /// Book deltas
    BookDelta = 3,
    /// Trade prints
    TradePrint = 4,
    /// Order acknowledgments
    OrderAck = 5,
    /// Order fills
    Fill = 6,
    /// Order rejects
    OrderReject = 7,
    /// Timer events
    Timer = 8,
}

/// Unified feed event for 15M Up/Down strategy backtesting.
///
/// Every event carries:
/// - `EventTime`: the three-timestamp triple
/// - `FeedSource`: identifies the originating feed
/// - `priority`: for deterministic ordering
/// - `seq`: dataset sequence number for tie-breaking
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedEvent {
    /// Event timing (exchange, ingest, visible timestamps)
    pub time: EventTime,
    /// Source feed identifier
    pub source: FeedSource,
    /// Priority class for ordering
    pub priority: FeedEventPriority,
    /// Dataset sequence number (for deterministic tie-breaking)
    pub dataset_seq: u64,
    /// The actual event payload
    pub payload: FeedEventPayload,
}

impl FeedEvent {
    pub fn new(
        time: EventTime,
        source: FeedSource,
        priority: FeedEventPriority,
        dataset_seq: u64,
        payload: FeedEventPayload,
    ) -> Self {
        Self {
            time,
            source,
            priority,
            dataset_seq,
            payload,
        }
    }

    /// Get the visible timestamp (the only time strategies should use).
    #[inline]
    pub fn visible_ts(&self) -> VisibleNanos {
        self.time.visible_ts
    }

    /// Compute a fingerprint for this event (for jitter computation).
    pub fn fingerprint(&self) -> u64 {
        let mut hasher = DefaultHasher::new();
        self.source.hash(&mut hasher);
        self.priority.hash(&mut hasher);
        self.dataset_seq.hash(&mut hasher);
        self.time.ingest_ts.hash(&mut hasher);
        hasher.finish()
    }
}

/// Ordering for FeedEvent: (visible_ts, priority, source, dataset_seq)
impl PartialEq for FeedEvent {
    fn eq(&self, other: &Self) -> bool {
        self.time.visible_ts == other.time.visible_ts
            && self.priority == other.priority
            && self.source == other.source
            && self.dataset_seq == other.dataset_seq
    }
}

impl Eq for FeedEvent {}

impl PartialOrd for FeedEvent {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for FeedEvent {
    fn cmp(&self, other: &Self) -> Ordering {
        // Primary: visible_ts (earlier first)
        self.time
            .visible_ts
            .cmp(&other.time.visible_ts)
            // Secondary: priority (lower = higher priority)
            .then_with(|| self.priority.cmp(&other.priority))
            // Tertiary: source (Binance before Polymarket)
            .then_with(|| self.source.cmp(&other.source))
            // Quaternary: dataset sequence (deterministic tie-break)
            .then_with(|| self.dataset_seq.cmp(&other.dataset_seq))
    }
}

/// Event payloads for the 15M Up/Down strategy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FeedEventPayload {
    /// Binance mid-price update for a symbol.
    BinanceMidPriceUpdate {
        symbol: String,
        mid_price: f64,
        bid: f64,
        ask: f64,
    },

    /// Full L2 book snapshot for a Polymarket token.
    PolymarketBookSnapshot {
        token_id: String,
        market_slug: String,
        bids: Vec<PriceLevel>,
        asks: Vec<PriceLevel>,
        exchange_seq: u64,
    },

    /// Incremental L2 book delta for a Polymarket token.
    PolymarketBookDelta {
        token_id: String,
        market_slug: String,
        side: BookSide,
        price: f64,
        new_size: f64,
        exchange_seq: u64,
    },

    /// Trade print from Polymarket.
    PolymarketTradePrint {
        token_id: String,
        market_slug: String,
        price: f64,
        size: f64,
        aggressor_side: BookSide,
        trade_id: Option<String>,
    },

    /// Chainlink oracle round update.
    ChainlinkRoundUpdate {
        asset: String,
        round_id: u64,
        answer: i128,
        decimals: u8,
        started_at: u64,
        updated_at: u64,
    },

    /// Market status change (halt, close, resolution).
    MarketStatusChange {
        token_id: String,
        new_status: MarketStatus,
        reason: Option<String>,
    },

    /// Market resolution (settlement).
    MarketResolution {
        token_id: String,
        market_slug: String,
        outcome: ResolutionOutcome,
        settlement_price: f64,
    },

    /// Timer event for scheduled callbacks.
    Timer { timer_id: u64, payload: Option<String> },

    /// Fill notification (from OMS).
    Fill {
        order_id: u64,
        price: f64,
        size: f64,
        is_maker: bool,
        leaves_qty: f64,
        fee: f64,
    },

    /// Order acknowledgment (from OMS).
    OrderAck {
        order_id: u64,
        client_order_id: Option<String>,
    },

    /// Order rejection (from OMS).
    OrderReject {
        order_id: u64,
        client_order_id: Option<String>,
        reason: String,
    },

    /// Cancel acknowledgment (from OMS).
    CancelAck { order_id: u64, cancelled_qty: f64 },
}

/// Price level in an order book.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PriceLevel {
    pub price: f64,
    pub size: f64,
    pub order_count: Option<u32>,
}

impl PriceLevel {
    pub fn new(price: f64, size: f64) -> Self {
        Self {
            price,
            size,
            order_count: None,
        }
    }
}

/// Book side (bid/ask).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BookSide {
    Bid,
    Ask,
}

/// Market status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MarketStatus {
    Open,
    Halted,
    Closed,
}

/// Resolution outcome.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ResolutionOutcome {
    Yes,
    No,
    Tie,
    Voided,
}

// =============================================================================
// BACKTEST LATENCY MODEL
// =============================================================================

/// Configuration for deterministic latency modeling.
///
/// This model computes `visible_ts` from `ingest_ts` with configurable delays
/// and optional deterministic jitter per feed and event class.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BacktestLatencyModel {
    /// Delay for Binance price updates (ns).
    pub binance_price_delay_ns: i64,
    /// Delay for Polymarket book updates (ns).
    pub polymarket_book_delay_ns: i64,
    /// Delay for Polymarket trade prints (ns).
    pub polymarket_trade_delay_ns: i64,
    /// Delay for Chainlink oracle updates (ns).
    pub chainlink_oracle_delay_ns: i64,
    /// Delay for internal timer events (ns).
    pub timer_delay_ns: i64,
    /// Delay for OMS events (fills, acks, rejects).
    pub oms_delay_ns: i64,

    /// Enable deterministic jitter (adds noise based on event fingerprint).
    pub jitter_enabled: bool,
    /// Maximum jitter amplitude (ns). Jitter is in range [0, jitter_max_ns].
    pub jitter_max_ns: i64,
    /// Seed for jitter RNG. Jitter is pure function of (seed, event fingerprint).
    pub jitter_seed: u64,
}

impl Default for BacktestLatencyModel {
    fn default() -> Self {
        Self {
            // Conservative defaults: ~100us for colocated systems
            binance_price_delay_ns: 100 * NS_PER_US,
            polymarket_book_delay_ns: 150 * NS_PER_US,
            polymarket_trade_delay_ns: 150 * NS_PER_US,
            chainlink_oracle_delay_ns: 1 * NS_PER_MS,
            timer_delay_ns: 0,
            oms_delay_ns: 50 * NS_PER_US,
            jitter_enabled: false,
            jitter_max_ns: 0,
            jitter_seed: 42,
        }
    }
}

impl BacktestLatencyModel {
    /// Create a zero-latency model (for debugging/unit tests).
    pub fn zero() -> Self {
        Self {
            binance_price_delay_ns: 0,
            polymarket_book_delay_ns: 0,
            polymarket_trade_delay_ns: 0,
            chainlink_oracle_delay_ns: 0,
            timer_delay_ns: 0,
            oms_delay_ns: 0,
            jitter_enabled: false,
            jitter_max_ns: 0,
            jitter_seed: 0,
        }
    }

    /// Create a realistic latency model with jitter.
    pub fn realistic_with_jitter(seed: u64) -> Self {
        Self {
            binance_price_delay_ns: 100 * NS_PER_US,
            polymarket_book_delay_ns: 200 * NS_PER_US,
            polymarket_trade_delay_ns: 200 * NS_PER_US,
            chainlink_oracle_delay_ns: 2 * NS_PER_MS,
            timer_delay_ns: 0,
            oms_delay_ns: 100 * NS_PER_US,
            jitter_enabled: true,
            jitter_max_ns: 50 * NS_PER_US,
            jitter_seed: seed,
        }
    }

    /// Get the base delay for a given source and priority.
    pub fn base_delay(&self, source: FeedSource, priority: FeedEventPriority) -> i64 {
        match source {
            FeedSource::Binance => self.binance_price_delay_ns,
            FeedSource::PolymarketBook => self.polymarket_book_delay_ns,
            FeedSource::PolymarketTrade => self.polymarket_trade_delay_ns,
            FeedSource::ChainlinkOracle => self.chainlink_oracle_delay_ns,
            FeedSource::Timer => self.timer_delay_ns,
            FeedSource::OrderManagement => match priority {
                FeedEventPriority::Fill
                | FeedEventPriority::OrderAck
                | FeedEventPriority::OrderReject => self.oms_delay_ns,
                _ => self.oms_delay_ns,
            },
        }
    }

    /// Compute deterministic jitter for an event.
    /// Jitter is a pure function of (seed, event_fingerprint).
    pub fn compute_jitter(&self, event_fingerprint: u64) -> i64 {
        if !self.jitter_enabled || self.jitter_max_ns == 0 {
            return 0;
        }

        // Combine seed and fingerprint to get a deterministic value
        let combined = self.jitter_seed.wrapping_add(event_fingerprint);
        let mut hasher = DefaultHasher::new();
        combined.hash(&mut hasher);
        let hash = hasher.finish();

        // Map hash to [0, jitter_max_ns]
        (hash % (self.jitter_max_ns as u64 + 1)) as i64
    }

    /// Compute visible_ts for an event.
    pub fn compute_visible_ts(
        &self,
        ingest_ts: Nanos,
        source: FeedSource,
        priority: FeedEventPriority,
        event_fingerprint: u64,
    ) -> VisibleNanos {
        let base_delay = self.base_delay(source, priority);
        let jitter = self.compute_jitter(event_fingerprint);
        VisibleNanos(ingest_ts + base_delay + jitter)
    }
}

// =============================================================================
// LATENCY MODEL APPLIER
// =============================================================================

/// Applies the latency model to events during ingestion.
///
/// This is the SINGLE PLACE where `visible_ts` is computed.
/// All events entering the unified feed must pass through this applier.
pub struct LatencyModelApplier {
    model: BacktestLatencyModel,
    events_processed: u64,
    total_latency_ns: i64,
    max_latency_ns: i64,
}

impl LatencyModelApplier {
    pub fn new(model: BacktestLatencyModel) -> Self {
        Self {
            model,
            events_processed: 0,
            total_latency_ns: 0,
            max_latency_ns: 0,
        }
    }

    /// Apply the latency model to compute visible_ts for an event.
    pub fn apply(&mut self, event: &mut FeedEvent) {
        let fingerprint = event.fingerprint();
        let visible_ts =
            self.model
                .compute_visible_ts(event.time.ingest_ts, event.source, event.priority, fingerprint);

        event.time.visible_ts = visible_ts;

        // Track statistics
        let latency = visible_ts.0 - event.time.ingest_ts;
        self.events_processed += 1;
        self.total_latency_ns += latency;
        self.max_latency_ns = self.max_latency_ns.max(latency);
    }

    /// Apply the latency model and return a new event with computed visible_ts.
    pub fn apply_and_create(
        &mut self,
        exchange_ts: Option<Nanos>,
        ingest_ts: Nanos,
        source: FeedSource,
        priority: FeedEventPriority,
        dataset_seq: u64,
        payload: FeedEventPayload,
    ) -> FeedEvent {
        // Create a placeholder event to get fingerprint
        let mut event = FeedEvent {
            time: EventTime {
                exchange_ts,
                ingest_ts,
                visible_ts: VisibleNanos(0),
            },
            source,
            priority,
            dataset_seq,
            payload,
        };

        // Apply latency model
        self.apply(&mut event);
        event
    }

    /// Get average latency in nanoseconds.
    pub fn avg_latency_ns(&self) -> f64 {
        if self.events_processed == 0 {
            0.0
        } else {
            self.total_latency_ns as f64 / self.events_processed as f64
        }
    }

    /// Get statistics.
    pub fn stats(&self) -> LatencyApplierStats {
        LatencyApplierStats {
            events_processed: self.events_processed,
            avg_latency_ns: self.avg_latency_ns(),
            max_latency_ns: self.max_latency_ns,
        }
    }
}

/// Statistics from the latency model applier.
#[derive(Debug, Clone)]
pub struct LatencyApplierStats {
    pub events_processed: u64,
    pub avg_latency_ns: f64,
    pub max_latency_ns: i64,
}

// =============================================================================
// DATASET INGEST TIMESTAMP FLAGS
// =============================================================================

/// Flags indicating the quality of ingest timestamps in a dataset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum IngestTimestampQuality {
    /// True nanosecond ingest timestamps recorded at capture time.
    /// Full HFT-grade replay is supported.
    TrueNanosecond,

    /// Millisecond-precision ingest timestamps.
    /// Acceptable for 15M strategy, may lose sub-ms ordering.
    Millisecond,

    /// Ingest timestamps synthetically derived from exchange timestamps.
    /// NOT suitable for HFT-grade claims; marked as "synthetic ingest".
    SyntheticFromExchange,

    /// No ingest timestamps available.
    /// Dataset must be rejected for 15M production-grade runs.
    Missing,
}

impl IngestTimestampQuality {
    /// Check if this quality level supports HFT-grade backtesting.
    pub fn is_hft_grade(&self) -> bool {
        matches!(self, Self::TrueNanosecond)
    }

    /// Check if this quality level is acceptable for 15M strategy.
    pub fn is_15m_acceptable(&self) -> bool {
        matches!(self, Self::TrueNanosecond | Self::Millisecond)
    }

    /// Check if production-grade runs should be rejected.
    pub fn reject_for_production(&self) -> bool {
        matches!(self, Self::SyntheticFromExchange | Self::Missing)
    }
}

// =============================================================================
// 15M WINDOW SEMANTICS (TIED TO VISIBLE TIME)
// =============================================================================

/// 15-minute window boundaries and P_start tracking.
///
/// All window semantics are defined purely in terms of visible time.
/// This struct provides the canonical implementation for the 15M Up/Down strategy.
#[derive(Debug, Clone)]
pub struct Window15M {
    /// Window start (aligned to 15-minute boundary in visible time).
    pub window_start: VisibleNanos,
    /// Window end (window_start + 15 minutes).
    pub window_end: VisibleNanos,
    /// P_start: Binance mid price observed at the first update whose
    /// visible_ts >= window_start. None until first price arrives.
    pub p_start: Option<f64>,
    /// Visible time when P_start was observed.
    pub p_start_visible_ts: Option<VisibleNanos>,
    /// Most recent Binance mid price observed (P_now).
    pub p_now: Option<f64>,
    /// Visible time when P_now was last updated.
    pub p_now_visible_ts: Option<VisibleNanos>,
}

impl Window15M {
    /// Create a new 15M window for the given visible time.
    pub fn for_visible_time(visible_ts: VisibleNanos) -> Self {
        let window_start = visible_ts.window_start();
        let window_end = visible_ts.window_end();
        Self {
            window_start,
            window_end,
            p_start: None,
            p_start_visible_ts: None,
            p_now: None,
            p_now_visible_ts: None,
        }
    }

    /// Update with a Binance mid price observation at the given visible time.
    pub fn update_price(&mut self, visible_ts: VisibleNanos, mid_price: f64) {
        // Update P_now unconditionally
        self.p_now = Some(mid_price);
        self.p_now_visible_ts = Some(visible_ts);

        // Set P_start if this is the first observation >= window_start
        if self.p_start.is_none() && visible_ts >= self.window_start {
            self.p_start = Some(mid_price);
            self.p_start_visible_ts = Some(visible_ts);
        }
    }

    /// Carry forward P_start from a previous window (if no new observation arrived).
    pub fn carry_forward_p_start(&mut self, previous_p_now: f64, previous_visible_ts: VisibleNanos) {
        if self.p_start.is_none() {
            self.p_start = Some(previous_p_now);
            self.p_start_visible_ts = Some(previous_visible_ts);
        }
    }

    /// Remaining time in the window (in seconds).
    pub fn remaining_secs(&self, visible_ts: VisibleNanos) -> f64 {
        let remaining_ns = self.window_end.0 - visible_ts.0;
        (remaining_ns.max(0) as f64) / (NS_PER_SEC as f64)
    }

    /// Check if a visible time is within this window.
    pub fn contains(&self, visible_ts: VisibleNanos) -> bool {
        visible_ts >= self.window_start && visible_ts < self.window_end
    }

    /// Check if the window has ended at the given visible time.
    pub fn has_ended(&self, visible_ts: VisibleNanos) -> bool {
        visible_ts >= self.window_end
    }
}

// =============================================================================
// INVARIANT CHECKS
// =============================================================================

/// Validate that visible_ts is monotone non-decreasing within a stream.
pub fn check_visible_monotone(prev: Option<VisibleNanos>, curr: VisibleNanos) -> Result<(), EventTimeError> {
    if let Some(prev_ts) = prev {
        if curr < prev_ts {
            return Err(EventTimeError::InvalidTimestamp {
                field: "visible_ts",
                value: curr.0,
            });
        }
    }
    Ok(())
}

/// Validate that visible_ts >= ingest_ts (no negative delays).
pub fn check_no_negative_delay(event_time: &EventTime) -> Result<(), EventTimeError> {
    if event_time.visible_ts.0 < event_time.ingest_ts {
        return Err(EventTimeError::NegativeDelay {
            ingest_ts: event_time.ingest_ts,
            visible_ts: event_time.visible_ts,
        });
    }
    Ok(())
}

// =============================================================================
// TESTS
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_visible_nanos_window_boundaries() {
        // 15 minutes = 900 seconds = 900_000_000_000 ns
        let ts = VisibleNanos(1000 * NS_PER_SEC + 123456);

        let window_start = ts.window_start();
        let window_end = ts.window_end();

        // Window start should be aligned to 15-minute boundary
        assert_eq!(window_start.0, 900 * NS_PER_SEC);
        assert_eq!(window_end.0, 1800 * NS_PER_SEC);
    }

    #[test]
    fn test_visible_nanos_remaining_secs() {
        // At the start of a window
        let window_start = VisibleNanos(900 * NS_PER_SEC);
        assert!((window_start.remaining_secs() - 900.0).abs() < 0.001);

        // Halfway through
        let halfway = VisibleNanos(900 * NS_PER_SEC + 450 * NS_PER_SEC);
        assert!((halfway.remaining_secs() - 450.0).abs() < 0.001);

        // At the end
        let window_end = VisibleNanos(1800 * NS_PER_SEC);
        assert!(window_end.remaining_secs() < 0.001);
    }

    #[test]
    fn test_event_time_instant() {
        let ts = 1234567890 * NS_PER_SEC;
        let et = EventTime::instant(ts);

        assert_eq!(et.exchange_ts, Some(ts));
        assert_eq!(et.ingest_ts, ts);
        assert_eq!(et.visible_ts.0, ts);
        assert!(et.validate().is_ok());
    }

    #[test]
    fn test_event_time_negative_delay_rejected() {
        let et = EventTime {
            exchange_ts: Some(100),
            ingest_ts: 100,
            visible_ts: VisibleNanos(50), // visible < ingest = negative delay
        };

        assert!(et.validate().is_err());
    }

    #[test]
    fn test_latency_model_zero() {
        let model = BacktestLatencyModel::zero();
        let visible = model.compute_visible_ts(
            1000 * NS_PER_SEC,
            FeedSource::Binance,
            FeedEventPriority::ReferencePrice,
            12345,
        );
        assert_eq!(visible.0, 1000 * NS_PER_SEC);
    }

    #[test]
    fn test_latency_model_with_delay() {
        let model = BacktestLatencyModel::default();
        let ingest_ts = 1000 * NS_PER_SEC;
        let visible = model.compute_visible_ts(
            ingest_ts,
            FeedSource::Binance,
            FeedEventPriority::ReferencePrice,
            12345,
        );

        // Should add the Binance delay
        assert_eq!(visible.0, ingest_ts + model.binance_price_delay_ns);
    }

    #[test]
    fn test_latency_model_jitter_deterministic() {
        let model = BacktestLatencyModel::realistic_with_jitter(42);

        let jitter1 = model.compute_jitter(12345);
        let jitter2 = model.compute_jitter(12345);
        let jitter3 = model.compute_jitter(99999);

        // Same fingerprint = same jitter
        assert_eq!(jitter1, jitter2);
        // Different fingerprint = different jitter (with high probability)
        assert_ne!(jitter1, jitter3);
    }

    #[test]
    fn test_feed_event_ordering() {
        let e1 = FeedEvent {
            time: EventTime::with_all(Some(100), 100, VisibleNanos(100)),
            source: FeedSource::Binance,
            priority: FeedEventPriority::ReferencePrice,
            dataset_seq: 1,
            payload: FeedEventPayload::BinanceMidPriceUpdate {
                symbol: "BTC".into(),
                mid_price: 50000.0,
                bid: 49999.0,
                ask: 50001.0,
            },
        };

        let e2 = FeedEvent {
            time: EventTime::with_all(Some(100), 100, VisibleNanos(100)),
            source: FeedSource::PolymarketBook,
            priority: FeedEventPriority::BookDelta,
            dataset_seq: 2,
            payload: FeedEventPayload::PolymarketBookDelta {
                token_id: "token1".into(),
                market_slug: "btc-updown-15m-1000".into(),
                side: BookSide::Bid,
                price: 0.5,
                new_size: 100.0,
                exchange_seq: 1,
            },
        };

        // Same visible_ts, but e1 has higher priority (ReferencePrice < BookDelta)
        assert!(e1 < e2);
    }

    #[test]
    fn test_feed_event_ordering_by_source() {
        let e1 = FeedEvent {
            time: EventTime::with_all(Some(100), 100, VisibleNanos(100)),
            source: FeedSource::Binance,
            priority: FeedEventPriority::ReferencePrice,
            dataset_seq: 1,
            payload: FeedEventPayload::Timer {
                timer_id: 1,
                payload: None,
            },
        };

        let e2 = FeedEvent {
            time: EventTime::with_all(Some(100), 100, VisibleNanos(100)),
            source: FeedSource::PolymarketBook,
            priority: FeedEventPriority::ReferencePrice, // Same priority
            dataset_seq: 1,
            payload: FeedEventPayload::Timer {
                timer_id: 2,
                payload: None,
            },
        };

        // Same visible_ts and priority, but e1 has lower source (Binance < PolymarketBook)
        assert!(e1 < e2);
    }

    #[test]
    fn test_window_15m_basic() {
        let visible_ts = VisibleNanos(950 * NS_PER_SEC); // Within first window after 900s
        let window = Window15M::for_visible_time(visible_ts);

        assert_eq!(window.window_start.0, 900 * NS_PER_SEC);
        assert_eq!(window.window_end.0, 1800 * NS_PER_SEC);
        assert!(window.p_start.is_none());
    }

    #[test]
    fn test_window_15m_price_update() {
        let mut window = Window15M::for_visible_time(VisibleNanos(900 * NS_PER_SEC));

        // First price update at window start
        window.update_price(VisibleNanos(900 * NS_PER_SEC), 50000.0);
        assert_eq!(window.p_start, Some(50000.0));
        assert_eq!(window.p_now, Some(50000.0));

        // Second price update
        window.update_price(VisibleNanos(910 * NS_PER_SEC), 50100.0);
        assert_eq!(window.p_start, Some(50000.0)); // P_start unchanged
        assert_eq!(window.p_now, Some(50100.0));
    }

    #[test]
    fn test_window_15m_carry_forward() {
        let mut window = Window15M::for_visible_time(VisibleNanos(1800 * NS_PER_SEC)); // Second window

        // No price update yet, carry forward from previous window
        window.carry_forward_p_start(50000.0, VisibleNanos(1799 * NS_PER_SEC));
        assert_eq!(window.p_start, Some(50000.0));

        // New update should not overwrite carried-forward P_start
        window.update_price(VisibleNanos(1805 * NS_PER_SEC), 50050.0);
        assert_eq!(window.p_start, Some(50000.0)); // Still the carried value
        assert_eq!(window.p_now, Some(50050.0));
    }

    #[test]
    fn test_ingest_timestamp_quality() {
        assert!(IngestTimestampQuality::TrueNanosecond.is_hft_grade());
        assert!(!IngestTimestampQuality::Millisecond.is_hft_grade());

        assert!(IngestTimestampQuality::TrueNanosecond.is_15m_acceptable());
        assert!(IngestTimestampQuality::Millisecond.is_15m_acceptable());
        assert!(!IngestTimestampQuality::SyntheticFromExchange.is_15m_acceptable());

        assert!(IngestTimestampQuality::SyntheticFromExchange.reject_for_production());
        assert!(IngestTimestampQuality::Missing.reject_for_production());
    }

    #[test]
    fn test_visible_monotone_check() {
        assert!(check_visible_monotone(None, VisibleNanos(100)).is_ok());
        assert!(check_visible_monotone(Some(VisibleNanos(100)), VisibleNanos(100)).is_ok());
        assert!(check_visible_monotone(Some(VisibleNanos(100)), VisibleNanos(200)).is_ok());
        assert!(check_visible_monotone(Some(VisibleNanos(200)), VisibleNanos(100)).is_err());
    }
}

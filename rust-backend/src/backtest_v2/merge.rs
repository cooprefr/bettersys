//! Explicit Dual-Feed Merge Layer for 15-Minute Up/Down Strategy Backtesting
//!
//! # Purpose
//!
//! This module provides a deterministic k-way merge layer that unifies events from
//! multiple data feeds (Binance price updates, Polymarket L2 book updates, etc.)
//! into a single ordered event queue. The merged queue is the **single source of truth**
//! for simulation time advancement.
//!
//! # Why Explicit Merging is Required for 15M Strategy
//!
//! The 15M Up/Down strategy requires alignment between:
//! 1. **Binance-derived probabilities**: `P_up` computed from BTC/ETH/SOL/XRP mid prices
//! 2. **Polymarket-execution prices**: Best bid/ask from L2 book for order placement
//!
//! Without explicit merging, per-feed processing could:
//! - Introduce non-deterministic event ordering across runs
//! - Allow look-ahead bias if Binance updates "see" future Polymarket prices
//! - Make the alignment of signal (Binance) and execution (Polymarket) unauditable
//!
//! # Ordering Key (Total Order)
//!
//! Events are ordered by a 5-level deterministic key:
//! 1. **Primary**: `visible_ts` (nanoseconds) - when strategy may observe the event
//! 2. **Secondary**: `priority_class` (u8) - event type priority
//! 3. **Tertiary**: `source_id` (u8) - feed source ordinal
//! 4. **Quaternary**: `per_source_seq` (u64) - dataset sequence number
//! 5. **Quinary**: `fingerprint` (u64) - deterministic hash (last resort)
//!
//! # Visibility Model
//!
//! `visible_ts` is the **only** time exposed to strategies. It is computed as:
//! ```text
//! visible_ts = ingest_ts + latency_delay + deterministic_jitter(event_fingerprint, seed)
//! ```
//!
//! Strategies CANNOT observe `exchange_ts` or `ingest_ts`. This is enforced at the
//! type level through `StrategyEventView`.
//!
//! # Determinism Contract
//!
//! Given the same:
//! - Dataset (ordered input events)
//! - Latency model configuration
//! - RNG seed
//!
//! The merge layer produces **identical** output sequences across runs.
//! This is verified by computing a run hash over the ordered sequence.

use crate::backtest_v2::clock::Nanos;
use crate::backtest_v2::event_time::{
    EventTime, FeedEvent, FeedEventPayload, FeedEventPriority, FeedSource,
    LatencyModelApplier, VisibleNanos,
};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::hash::{Hash, Hasher};

// =============================================================================
// ORDERING KEY DEFINITION
// =============================================================================

/// Complete ordering key for deterministic event ordering.
///
/// This is a 5-tuple that provides a total order over all events:
/// 1. visible_ts: Primary ordering by when strategy observes the event
/// 2. priority_class: Secondary by event type priority
/// 3. source_id: Tertiary by feed source
/// 4. per_source_seq: Quaternary by dataset sequence
/// 5. fingerprint: Quinary fallback for events without sequence
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct OrderingKey {
    pub visible_ts: i64,
    pub priority_class: u8,
    pub source_id: u8,
    pub per_source_seq: u64,
    pub fingerprint: u64,
}

impl OrderingKey {
    /// Create an ordering key from a FeedEvent.
    pub fn from_event(event: &FeedEvent) -> Self {
        Self {
            visible_ts: event.time.visible_ts.0,
            priority_class: event.priority as u8,
            source_id: event.source as u8,
            per_source_seq: event.dataset_seq,
            fingerprint: event.fingerprint(),
        }
    }
}

impl PartialOrd for OrderingKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for OrderingKey {
    fn cmp(&self, other: &Self) -> Ordering {
        // Primary: visible_ts (earlier first)
        self.visible_ts
            .cmp(&other.visible_ts)
            // Secondary: priority_class (lower = higher priority)
            .then_with(|| self.priority_class.cmp(&other.priority_class))
            // Tertiary: source_id (lower = higher priority: Binance < Polymarket)
            .then_with(|| self.source_id.cmp(&other.source_id))
            // Quaternary: per_source_seq (earlier in dataset first)
            .then_with(|| self.per_source_seq.cmp(&other.per_source_seq))
            // Quinary: fingerprint (deterministic fallback)
            .then_with(|| self.fingerprint.cmp(&other.fingerprint))
    }
}

/// Compute ordering key from a FeedEvent.
///
/// This is the canonical function that defines event ordering.
/// All merge operations use this key for deterministic ordering.
#[inline]
pub fn ordering_key(event: &FeedEvent) -> (i64, u8, u8, u64, u64) {
    (
        event.time.visible_ts.0,
        event.priority as u8,
        event.source as u8,
        event.dataset_seq,
        event.fingerprint(),
    )
}

// =============================================================================
// PRIORITY CLASS MAPPING
// =============================================================================

/// Priority class for deterministic tie-breaking.
///
/// Events at the same `visible_ts` are ordered by priority class.
/// Lower value = higher priority (processed first).
///
/// # Priority Policy (Documented and Stable)
///
/// The ordering reflects causality requirements for the 15M strategy:
///
/// 1. **System events** (halts, resolutions): Must be processed first as they
///    affect market state and may invalidate pending orders.
///
/// 2. **Reference prices** (Binance mid): Needed before book updates so that
///    `P_up` computation uses the correct reference price.
///
/// 3. **Book snapshots**: Full state before deltas for consistent reconstruction.
///
/// 4. **Book deltas**: Incremental updates after snapshots.
///
/// 5. **Trade prints**: Public trades after book state is established.
///
/// 6. **OMS events** (fills, acks, rejects): Order lifecycle events.
///
/// 7. **Timer events**: Scheduled callbacks (lowest priority).
///
/// This ordering ensures that when evaluating edge at time T:
/// - Binance P_now is from the most recent update at or before T
/// - Polymarket book is from the most recent snapshot/delta at or before T
/// - OMS state reflects fills that occurred before T
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum PriorityClass {
    /// System events (halts, resolutions) - highest priority
    System = 0,
    /// Binance reference price updates
    BinancePrice = 1,
    /// Polymarket L2 book snapshots
    PolymarketSnapshot = 2,
    /// Polymarket L2 book deltas
    PolymarketDelta = 3,
    /// Polymarket trade prints
    PolymarketTrade = 4,
    /// Chainlink oracle updates
    ChainlinkOracle = 5,
    /// Order acknowledgments
    OrderAck = 6,
    /// Order fills
    Fill = 7,
    /// Order rejects
    OrderReject = 8,
    /// Cancel acknowledgments
    CancelAck = 9,
    /// Timer events (lowest priority)
    Timer = 10,
    /// Internal backtest events
    Internal = 11,
}

impl PriorityClass {
    /// Map a FeedEventPriority to a PriorityClass.
    pub fn from_feed_priority(priority: FeedEventPriority, source: FeedSource) -> Self {
        match (priority, source) {
            (FeedEventPriority::System, _) => Self::System,
            (FeedEventPriority::ReferencePrice, FeedSource::Binance) => Self::BinancePrice,
            (FeedEventPriority::BookSnapshot, _) => Self::PolymarketSnapshot,
            (FeedEventPriority::BookDelta, _) => Self::PolymarketDelta,
            (FeedEventPriority::TradePrint, _) => Self::PolymarketTrade,
            (FeedEventPriority::OrderAck, _) => Self::OrderAck,
            (FeedEventPriority::Fill, _) => Self::Fill,
            (FeedEventPriority::OrderReject, _) => Self::OrderReject,
            (FeedEventPriority::Timer, _) => Self::Timer,
            (_, FeedSource::ChainlinkOracle) => Self::ChainlinkOracle,
            _ => Self::Internal,
        }
    }

    /// Get the raw priority value.
    pub fn as_u8(self) -> u8 {
        self as u8
    }
}

// =============================================================================
// SOURCE ID MAPPING
// =============================================================================

/// Source ID for deterministic tie-breaking.
///
/// Events from different sources at the same `visible_ts` and `priority_class`
/// are ordered by source ID. Lower value = higher priority.
///
/// # Source Priority Policy (Documented and Stable)
///
/// The ordering reflects the data flow for the 15M strategy:
///
/// 1. **InternalSim** (0): Internal simulation events (system)
/// 2. **Binance** (1): Reference price source
/// 3. **Polymarket** (2): Execution venue
/// 4. **Chainlink** (3): Settlement oracle
/// 5. **Timer** (4): Scheduled callbacks
/// 6. **OrderManagement** (5): OMS events
/// 7. **StrategySim** (6): Strategy-generated events
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum SourceId {
    InternalSim = 0,
    Binance = 1,
    Polymarket = 2,
    Chainlink = 3,
    Timer = 4,
    OrderManagement = 5,
    StrategySim = 6,
}

impl From<FeedSource> for SourceId {
    fn from(source: FeedSource) -> Self {
        match source {
            FeedSource::Binance => Self::Binance,
            FeedSource::PolymarketBook => Self::Polymarket,
            FeedSource::PolymarketTrade => Self::Polymarket,
            FeedSource::ChainlinkOracle => Self::Chainlink,
            FeedSource::Timer => Self::Timer,
            FeedSource::OrderManagement => Self::OrderManagement,
        }
    }
}

impl SourceId {
    pub fn as_u8(self) -> u8 {
        self as u8
    }
}

// =============================================================================
// MERGEABLE EVENT WRAPPER
// =============================================================================

/// Wrapper for events in the merge heap.
///
/// Uses `Reverse` semantics for min-heap behavior with BinaryHeap.
#[derive(Debug, Clone)]
struct MergeEntry {
    key: OrderingKey,
    event: FeedEvent,
}

impl PartialEq for MergeEntry {
    fn eq(&self, other: &Self) -> bool {
        self.key == other.key
    }
}

impl Eq for MergeEntry {}

impl PartialOrd for MergeEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for MergeEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        // Reverse ordering for min-heap behavior
        other.key.cmp(&self.key)
    }
}

// =============================================================================
// FEED ADAPTER TRAIT
// =============================================================================

/// Trait for dataset readers that produce FeedEvent streams.
///
/// Adapters are **pure**: no network, no wall-clock, deterministic parsing only.
/// Each adapter reads from a pre-recorded dataset and yields events in dataset order.
pub trait FeedAdapter: Send {
    /// Get the next event from the dataset.
    ///
    /// Returns `None` when the dataset is exhausted.
    fn next_event(&mut self) -> Option<FeedEvent>;

    /// Peek at the next event without consuming it.
    fn peek(&self) -> Option<&FeedEvent>;

    /// Reset the adapter to the beginning of the dataset.
    fn reset(&mut self);

    /// Get the source identifier for this adapter.
    fn source(&self) -> FeedSource;

    /// Get the adapter name for logging.
    fn name(&self) -> &str;

    /// Number of events remaining (if known).
    fn remaining(&self) -> Option<usize> {
        None
    }
}

// =============================================================================
// BINANCE ADAPTER
// =============================================================================

/// Binance feed adapter for price/volatility updates.
///
/// Reads recorded Binance events from a dataset and produces
/// `BinanceMidPriceUpdate` events with proper EventTime.
pub struct BinanceAdapter {
    events: Vec<FeedEvent>,
    index: usize,
    name: String,
}

impl BinanceAdapter {
    /// Create a new Binance adapter from raw price records.
    pub fn new(name: impl Into<String>, events: Vec<FeedEvent>) -> Self {
        Self {
            events,
            index: 0,
            name: name.into(),
        }
    }

    /// Create from raw Binance price records.
    ///
    /// Converts raw price records into FeedEvents with proper timestamps.
    pub fn from_raw_records(
        name: impl Into<String>,
        records: Vec<BinanceRawRecord>,
        latency_applier: &mut LatencyModelApplier,
    ) -> Self {
        let events: Vec<FeedEvent> = records
            .into_iter()
            .enumerate()
            .map(|(seq, record)| {
                latency_applier.apply_and_create(
                    record.exchange_ts,
                    record.ingest_ts,
                    FeedSource::Binance,
                    FeedEventPriority::ReferencePrice,
                    seq as u64,
                    FeedEventPayload::BinanceMidPriceUpdate {
                        symbol: record.symbol,
                        mid_price: record.mid_price,
                        bid: record.bid,
                        ask: record.ask,
                    },
                )
            })
            .collect();

        Self::new(name, events)
    }

    pub fn len(&self) -> usize {
        self.events.len()
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }
}

impl FeedAdapter for BinanceAdapter {
    fn next_event(&mut self) -> Option<FeedEvent> {
        if self.index < self.events.len() {
            let event = self.events[self.index].clone();
            self.index += 1;
            Some(event)
        } else {
            None
        }
    }

    fn peek(&self) -> Option<&FeedEvent> {
        self.events.get(self.index)
    }

    fn reset(&mut self) {
        self.index = 0;
    }

    fn source(&self) -> FeedSource {
        FeedSource::Binance
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn remaining(&self) -> Option<usize> {
        Some(self.events.len().saturating_sub(self.index))
    }
}

/// Raw Binance price record from dataset.
#[derive(Debug, Clone)]
pub struct BinanceRawRecord {
    pub exchange_ts: Option<Nanos>,
    pub ingest_ts: Nanos,
    pub symbol: String,
    pub mid_price: f64,
    pub bid: f64,
    pub ask: f64,
}

// =============================================================================
// POLYMARKET ADAPTER
// =============================================================================

/// Polymarket feed adapter for L2 book and trade events.
///
/// Reads recorded Polymarket events from a dataset and produces
/// L2 snapshots, deltas, and trade prints with proper EventTime.
pub struct PolymarketAdapter {
    events: Vec<FeedEvent>,
    index: usize,
    name: String,
}

impl PolymarketAdapter {
    /// Create a new Polymarket adapter from pre-built events.
    pub fn new(name: impl Into<String>, events: Vec<FeedEvent>) -> Self {
        Self {
            events,
            index: 0,
            name: name.into(),
        }
    }

    /// Create from raw Polymarket records.
    pub fn from_raw_records(
        name: impl Into<String>,
        records: Vec<PolymarketRawRecord>,
        latency_applier: &mut LatencyModelApplier,
    ) -> Self {
        let events: Vec<FeedEvent> = records
            .into_iter()
            .enumerate()
            .map(|(seq, record)| {
                // Determine source first before consuming event_type
                let source = match &record.event_type {
                    PolymarketEventType::TradePrint { .. } => FeedSource::PolymarketTrade,
                    _ => FeedSource::PolymarketBook,
                };

                let (priority, payload) = match record.event_type {
                    PolymarketEventType::L2Snapshot { bids, asks, exchange_seq } => (
                        FeedEventPriority::BookSnapshot,
                        FeedEventPayload::PolymarketBookSnapshot {
                            token_id: record.token_id.clone(),
                            market_slug: record.market_slug.clone(),
                            bids,
                            asks,
                            exchange_seq,
                        },
                    ),
                    PolymarketEventType::L2Delta { side, price, new_size, exchange_seq } => (
                        FeedEventPriority::BookDelta,
                        FeedEventPayload::PolymarketBookDelta {
                            token_id: record.token_id.clone(),
                            market_slug: record.market_slug.clone(),
                            side,
                            price,
                            new_size,
                            exchange_seq,
                        },
                    ),
                    PolymarketEventType::TradePrint { price, size, aggressor_side, trade_id } => (
                        FeedEventPriority::TradePrint,
                        FeedEventPayload::PolymarketTradePrint {
                            token_id: record.token_id.clone(),
                            market_slug: record.market_slug.clone(),
                            price,
                            size,
                            aggressor_side,
                            trade_id,
                        },
                    ),
                };

                latency_applier.apply_and_create(
                    record.exchange_ts,
                    record.ingest_ts,
                    source,
                    priority,
                    seq as u64,
                    payload,
                )
            })
            .collect();

        Self::new(name, events)
    }

    pub fn len(&self) -> usize {
        self.events.len()
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }
}

impl FeedAdapter for PolymarketAdapter {
    fn next_event(&mut self) -> Option<FeedEvent> {
        if self.index < self.events.len() {
            let event = self.events[self.index].clone();
            self.index += 1;
            Some(event)
        } else {
            None
        }
    }

    fn peek(&self) -> Option<&FeedEvent> {
        self.events.get(self.index)
    }

    fn reset(&mut self) {
        self.index = 0;
    }

    fn source(&self) -> FeedSource {
        FeedSource::PolymarketBook
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn remaining(&self) -> Option<usize> {
        Some(self.events.len().saturating_sub(self.index))
    }
}

/// Raw Polymarket record from dataset.
#[derive(Debug, Clone)]
pub struct PolymarketRawRecord {
    pub exchange_ts: Option<Nanos>,
    pub ingest_ts: Nanos,
    pub token_id: String,
    pub market_slug: String,
    pub event_type: PolymarketEventType,
}

/// Polymarket event types.
#[derive(Debug, Clone)]
pub enum PolymarketEventType {
    L2Snapshot {
        bids: Vec<crate::backtest_v2::event_time::PriceLevel>,
        asks: Vec<crate::backtest_v2::event_time::PriceLevel>,
        exchange_seq: u64,
    },
    L2Delta {
        side: crate::backtest_v2::event_time::BookSide,
        price: f64,
        new_size: f64,
        exchange_seq: u64,
    },
    TradePrint {
        price: f64,
        size: f64,
        aggressor_side: crate::backtest_v2::event_time::BookSide,
        trade_id: Option<String>,
    },
}

// =============================================================================
// TIMER ADAPTER
// =============================================================================

/// Timer adapter for scheduled callback events.
pub struct TimerAdapter {
    events: Vec<FeedEvent>,
    index: usize,
}

impl TimerAdapter {
    pub fn new(events: Vec<FeedEvent>) -> Self {
        Self { events, index: 0 }
    }

    /// Schedule a new timer event.
    pub fn schedule(&mut self, visible_ts: VisibleNanos, timer_id: u64, payload: Option<String>) {
        let event = FeedEvent::new(
            EventTime::with_all(None, visible_ts.0, visible_ts),
            FeedSource::Timer,
            FeedEventPriority::Timer,
            timer_id,
            FeedEventPayload::Timer { timer_id, payload },
        );
        self.events.push(event);
        // Re-sort to maintain ordering
        self.events.sort_by_key(|e| e.time.visible_ts);
    }
}

impl FeedAdapter for TimerAdapter {
    fn next_event(&mut self) -> Option<FeedEvent> {
        if self.index < self.events.len() {
            let event = self.events[self.index].clone();
            self.index += 1;
            Some(event)
        } else {
            None
        }
    }

    fn peek(&self) -> Option<&FeedEvent> {
        self.events.get(self.index)
    }

    fn reset(&mut self) {
        self.index = 0;
    }

    fn source(&self) -> FeedSource {
        FeedSource::Timer
    }

    fn name(&self) -> &str {
        "timer"
    }
}

// =============================================================================
// OMS ADAPTER
// =============================================================================

/// OMS adapter for order lifecycle events (fills, acks, rejects).
///
/// Events are pushed dynamically by the simulation as orders are processed.
pub struct OmsAdapter {
    events: Vec<FeedEvent>,
    index: usize,
    next_seq: u64,
}

impl OmsAdapter {
    pub fn new() -> Self {
        Self {
            events: Vec::new(),
            index: 0,
            next_seq: 0,
        }
    }

    /// Push a fill notification.
    pub fn push_fill(
        &mut self,
        visible_ts: VisibleNanos,
        order_id: u64,
        price: f64,
        size: f64,
        is_maker: bool,
        leaves_qty: f64,
        fee: f64,
    ) {
        let seq = self.next_seq;
        self.next_seq += 1;

        let event = FeedEvent::new(
            EventTime::with_all(None, visible_ts.0, visible_ts),
            FeedSource::OrderManagement,
            FeedEventPriority::Fill,
            seq,
            FeedEventPayload::Fill {
                order_id,
                price,
                size,
                is_maker,
                leaves_qty,
                fee,
            },
        );
        self.events.push(event);
    }

    /// Push an order acknowledgment.
    pub fn push_ack(&mut self, visible_ts: VisibleNanos, order_id: u64, client_order_id: Option<String>) {
        let seq = self.next_seq;
        self.next_seq += 1;

        let event = FeedEvent::new(
            EventTime::with_all(None, visible_ts.0, visible_ts),
            FeedSource::OrderManagement,
            FeedEventPriority::OrderAck,
            seq,
            FeedEventPayload::OrderAck {
                order_id,
                client_order_id,
            },
        );
        self.events.push(event);
    }

    /// Push an order rejection.
    pub fn push_reject(&mut self, visible_ts: VisibleNanos, order_id: u64, reason: String) {
        let seq = self.next_seq;
        self.next_seq += 1;

        let event = FeedEvent::new(
            EventTime::with_all(None, visible_ts.0, visible_ts),
            FeedSource::OrderManagement,
            FeedEventPriority::OrderReject,
            seq,
            FeedEventPayload::OrderReject {
                order_id,
                client_order_id: None,
                reason,
            },
        );
        self.events.push(event);
    }

    /// Push a cancel acknowledgment.
    pub fn push_cancel_ack(&mut self, visible_ts: VisibleNanos, order_id: u64, cancelled_qty: f64) {
        let seq = self.next_seq;
        self.next_seq += 1;

        let event = FeedEvent::new(
            EventTime::with_all(None, visible_ts.0, visible_ts),
            FeedSource::OrderManagement,
            FeedEventPriority::Timer, // CancelAck uses timer priority (low)
            seq,
            FeedEventPayload::CancelAck {
                order_id,
                cancelled_qty,
            },
        );
        self.events.push(event);
    }
}

impl Default for OmsAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl FeedAdapter for OmsAdapter {
    fn next_event(&mut self) -> Option<FeedEvent> {
        if self.index < self.events.len() {
            let event = self.events[self.index].clone();
            self.index += 1;
            Some(event)
        } else {
            None
        }
    }

    fn peek(&self) -> Option<&FeedEvent> {
        self.events.get(self.index)
    }

    fn reset(&mut self) {
        self.index = 0;
    }

    fn source(&self) -> FeedSource {
        FeedSource::OrderManagement
    }

    fn name(&self) -> &str {
        "oms"
    }
}

// =============================================================================
// K-WAY MERGE LAYER
// =============================================================================

/// Deterministic k-way merge layer for multiple feed sources.
///
/// This is the **single source of truth** for event ordering in the backtest.
/// All events from all feeds pass through this merge layer.
///
/// # Determinism Guarantees
///
/// - Events are ordered by the 5-level ordering key
/// - No heap iteration order dependencies (explicit key comparison)
/// - No HashMap iteration (no HashMaps used in ordering)
/// - Same output given same input and seed
pub struct FeedMerger {
    /// Adapters for each feed source
    adapters: Vec<Box<dyn FeedAdapter>>,
    /// Binary heap for k-way merge (min-heap via Reverse)
    heap: BinaryHeap<MergeEntry>,
    /// Last visible_ts popped (for monotonicity validation)
    last_visible_ts: Option<VisibleNanos>,
    /// Statistics
    stats: FeedMergerStats,
    /// Enable strict monotonicity checking
    strict_mode: bool,
    /// Event log for debugging (behind flag)
    event_log: Option<Vec<EventLogEntry>>,
    /// Run hash state
    run_hash_state: RunHashState,
}

/// Statistics for the feed merger.
#[derive(Debug, Clone, Default)]
pub struct FeedMergerStats {
    /// Total events merged
    pub total_merged: u64,
    /// Events by source
    pub by_source: [u64; 8],
    /// Events by priority
    pub by_priority: [u64; 12],
    /// Tie-breaks by priority class
    pub tiebreaks_by_priority: u64,
    /// Tie-breaks by source
    pub tiebreaks_by_source: u64,
    /// Tie-breaks by sequence
    pub tiebreaks_by_seq: u64,
    /// Tie-breaks by fingerprint
    pub tiebreaks_by_fingerprint: u64,
    /// Max queue depth
    pub max_depth: usize,
}

/// Event log entry for debugging.
#[derive(Debug, Clone)]
pub struct EventLogEntry {
    pub visible_ts: i64,
    pub event_type: String,
    pub source: FeedSource,
    pub dataset_seq: u64,
    pub exchange_ts: Option<i64>,
    pub ingest_ts: i64,
}

impl EventLogEntry {
    fn from_event(event: &FeedEvent) -> Self {
        Self {
            visible_ts: event.time.visible_ts.0,
            event_type: Self::event_type_name(&event.payload),
            source: event.source,
            dataset_seq: event.dataset_seq,
            exchange_ts: event.time.exchange_ts,
            ingest_ts: event.time.ingest_ts,
        }
    }

    fn event_type_name(payload: &FeedEventPayload) -> String {
        match payload {
            FeedEventPayload::BinanceMidPriceUpdate { symbol, .. } => {
                format!("BinanceMid({})", symbol)
            }
            FeedEventPayload::PolymarketBookSnapshot { market_slug, .. } => {
                format!("PMSnapshot({})", market_slug)
            }
            FeedEventPayload::PolymarketBookDelta { market_slug, .. } => {
                format!("PMDelta({})", market_slug)
            }
            FeedEventPayload::PolymarketTradePrint { market_slug, .. } => {
                format!("PMTrade({})", market_slug)
            }
            FeedEventPayload::ChainlinkRoundUpdate { asset, .. } => {
                format!("Chainlink({})", asset)
            }
            FeedEventPayload::MarketStatusChange { token_id, .. } => {
                format!("StatusChange({})", token_id)
            }
            FeedEventPayload::MarketResolution { market_slug, .. } => {
                format!("Resolution({})", market_slug)
            }
            FeedEventPayload::Timer { timer_id, .. } => format!("Timer({})", timer_id),
            FeedEventPayload::Fill { order_id, .. } => format!("Fill({})", order_id),
            FeedEventPayload::OrderAck { order_id, .. } => format!("Ack({})", order_id),
            FeedEventPayload::OrderReject { order_id, .. } => format!("Reject({})", order_id),
            FeedEventPayload::CancelAck { order_id, .. } => format!("CancelAck({})", order_id),
        }
    }
}

/// Run hash state for determinism verification.
#[derive(Debug, Clone)]
struct RunHashState {
    hasher: u64,
    events_hashed: u64,
}

impl Default for RunHashState {
    fn default() -> Self {
        Self {
            hasher: 0xcbf29ce484222325, // FNV-1a offset basis
            events_hashed: 0,
        }
    }
}

impl RunHashState {
    fn update(&mut self, event: &FeedEvent) {
        // FNV-1a hash update
        let key = ordering_key(event);
        let bytes = [
            key.0.to_le_bytes().as_slice(),
            &[key.1],
            &[key.2],
            key.3.to_le_bytes().as_slice(),
            key.4.to_le_bytes().as_slice(),
        ]
        .concat();

        for byte in bytes {
            self.hasher ^= byte as u64;
            self.hasher = self.hasher.wrapping_mul(0x100000001b3);
        }
        self.events_hashed += 1;
    }

    fn finalize(&self) -> u64 {
        self.hasher
    }
}

/// Configuration for the feed merger.
#[derive(Debug, Clone)]
pub struct FeedMergerConfig {
    /// Enable strict monotonicity checking
    pub strict_mode: bool,
    /// Enable event logging (expensive)
    pub enable_logging: bool,
    /// Initial heap capacity
    pub initial_capacity: usize,
}

impl Default for FeedMergerConfig {
    fn default() -> Self {
        Self {
            strict_mode: true,
            enable_logging: false,
            initial_capacity: 1024,
        }
    }
}

impl FeedMerger {
    /// Create a new feed merger with the given configuration.
    pub fn new(config: FeedMergerConfig) -> Self {
        Self {
            adapters: Vec::new(),
            heap: BinaryHeap::with_capacity(config.initial_capacity),
            last_visible_ts: None,
            stats: FeedMergerStats::default(),
            strict_mode: config.strict_mode,
            event_log: if config.enable_logging {
                Some(Vec::new())
            } else {
                None
            },
            run_hash_state: RunHashState::default(),
        }
    }

    /// Add a feed adapter to the merger.
    pub fn add_adapter(&mut self, adapter: Box<dyn FeedAdapter>) {
        self.adapters.push(adapter);
    }

    /// Prime the merge heap by pulling one event from each adapter.
    pub fn prime(&mut self) {
        for adapter in &mut self.adapters {
            if let Some(event) = adapter.next_event() {
                let key = OrderingKey::from_event(&event);
                self.heap.push(MergeEntry { key, event });
            }
        }
        self.stats.max_depth = self.stats.max_depth.max(self.heap.len());
    }

    /// Pop the next event in merged visible-time order.
    ///
    /// Returns `None` when all feeds are exhausted.
    pub fn pop(&mut self) -> Option<FeedEvent> {
        // Pop the minimum event from the heap
        let entry = self.heap.pop()?;
        let event = entry.event;

        // Validate monotonicity
        if let Some(last_ts) = self.last_visible_ts {
            if event.time.visible_ts < last_ts {
                if self.strict_mode {
                    panic!(
                        "FeedMerger: visible_ts monotonicity violation: {:?} < {:?}",
                        event.time.visible_ts, last_ts
                    );
                }
            }
        }
        self.last_visible_ts = Some(event.time.visible_ts);

        // Update run hash
        self.run_hash_state.update(&event);

        // Log if enabled
        if let Some(log) = &mut self.event_log {
            log.push(EventLogEntry::from_event(&event));
        }

        // Update statistics
        self.stats.total_merged += 1;
        self.stats.by_source[event.source as usize] += 1;
        self.stats.by_priority[event.priority as usize] += 1;

        // Refill from the source adapter that provided this event
        let source_idx = self
            .adapters
            .iter()
            .position(|a| a.source() == event.source);

        if let Some(idx) = source_idx {
            if let Some(next_event) = self.adapters[idx].next_event() {
                let key = OrderingKey::from_event(&next_event);
                self.heap.push(MergeEntry {
                    key,
                    event: next_event,
                });
                self.stats.max_depth = self.stats.max_depth.max(self.heap.len());
            }
        }

        Some(event)
    }

    /// Peek at the next event without consuming it.
    pub fn peek(&self) -> Option<&FeedEvent> {
        self.heap.peek().map(|e| &e.event)
    }

    /// Peek at the visible_ts of the next event.
    pub fn peek_visible_ts(&self) -> Option<VisibleNanos> {
        self.heap.peek().map(|e| e.event.time.visible_ts)
    }

    /// Check if all feeds are exhausted.
    pub fn is_exhausted(&self) -> bool {
        self.heap.is_empty()
    }

    /// Get merger statistics.
    pub fn stats(&self) -> &FeedMergerStats {
        &self.stats
    }

    /// Get the event log (if logging is enabled).
    pub fn event_log(&self) -> Option<&[EventLogEntry]> {
        self.event_log.as_deref()
    }

    /// Compute the run hash for determinism verification.
    ///
    /// Two runs with identical inputs and seeds should produce identical run hashes.
    pub fn run_hash(&self) -> u64 {
        self.run_hash_state.finalize()
    }

    /// Reset the merger for a new run.
    pub fn reset(&mut self) {
        self.heap.clear();
        self.last_visible_ts = None;
        self.stats = FeedMergerStats::default();
        self.run_hash_state = RunHashState::default();
        if let Some(log) = &mut self.event_log {
            log.clear();
        }
        for adapter in &mut self.adapters {
            adapter.reset();
        }
    }

    /// Drain all events up to (and including) a given visible time.
    pub fn drain_until(&mut self, cutoff: VisibleNanos) -> Vec<FeedEvent> {
        let mut events = Vec::new();
        while let Some(ts) = self.peek_visible_ts() {
            if ts > cutoff {
                break;
            }
            if let Some(event) = self.pop() {
                events.push(event);
            }
        }
        events
    }
}

// =============================================================================
// MERGE ITERATOR
// =============================================================================

/// Iterator adapter for FeedMerger.
pub struct MergeIterator<'a> {
    merger: &'a mut FeedMerger,
}

impl<'a> Iterator for MergeIterator<'a> {
    type Item = FeedEvent;

    fn next(&mut self) -> Option<Self::Item> {
        self.merger.pop()
    }
}

impl FeedMerger {
    /// Get an iterator over merged events.
    pub fn iter(&mut self) -> MergeIterator<'_> {
        MergeIterator { merger: self }
    }
}

// =============================================================================
// RUN FINGERPRINT
// =============================================================================

/// Fingerprint for a complete backtest run.
///
/// This captures the full ordered sequence in a single hash for quick comparison.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunFingerprint {
    /// Hash of the ordered event sequence
    pub sequence_hash: u64,
    /// Total events in the run
    pub total_events: u64,
    /// First event visible_ts
    pub first_visible_ts: Option<i64>,
    /// Last event visible_ts
    pub last_visible_ts: Option<i64>,
}

impl RunFingerprint {
    /// Compute fingerprint from a completed merger.
    pub fn from_merger(merger: &FeedMerger) -> Self {
        Self {
            sequence_hash: merger.run_hash(),
            total_events: merger.stats.total_merged,
            first_visible_ts: merger.event_log.as_ref().and_then(|l| l.first().map(|e| e.visible_ts)),
            last_visible_ts: merger.last_visible_ts.map(|t| t.0),
        }
    }
}

// =============================================================================
// TESTS
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backtest_v2::event_time::{BookSide, PriceLevel, NS_PER_MS};

    fn make_binance_event(ingest_ts: i64, seq: u64, symbol: &str, mid: f64) -> FeedEvent {
        FeedEvent::new(
            EventTime::with_all(Some(ingest_ts), ingest_ts, VisibleNanos(ingest_ts)),
            FeedSource::Binance,
            FeedEventPriority::ReferencePrice,
            seq,
            FeedEventPayload::BinanceMidPriceUpdate {
                symbol: symbol.to_string(),
                mid_price: mid,
                bid: mid - 0.5,
                ask: mid + 0.5,
            },
        )
    }

    fn make_polymarket_delta(
        ingest_ts: i64,
        seq: u64,
        market: &str,
        price: f64,
    ) -> FeedEvent {
        FeedEvent::new(
            EventTime::with_all(Some(ingest_ts), ingest_ts, VisibleNanos(ingest_ts)),
            FeedSource::PolymarketBook,
            FeedEventPriority::BookDelta,
            seq,
            FeedEventPayload::PolymarketBookDelta {
                token_id: "token1".to_string(),
                market_slug: market.to_string(),
                side: BookSide::Bid,
                price,
                new_size: 100.0,
                exchange_seq: seq,
            },
        )
    }

    #[test]
    fn test_ordering_key_visible_ts_primary() {
        let e1 = make_binance_event(1000, 1, "BTC", 50000.0);
        let e2 = make_binance_event(2000, 2, "BTC", 50100.0);

        let k1 = OrderingKey::from_event(&e1);
        let k2 = OrderingKey::from_event(&e2);

        assert!(k1 < k2, "Earlier visible_ts should come first");
    }

    #[test]
    fn test_ordering_key_priority_secondary() {
        // Same visible_ts, different priorities
        let e1 = FeedEvent::new(
            EventTime::with_all(Some(1000), 1000, VisibleNanos(1000)),
            FeedSource::Binance,
            FeedEventPriority::ReferencePrice, // Priority 1
            1,
            FeedEventPayload::Timer { timer_id: 1, payload: None },
        );
        let e2 = FeedEvent::new(
            EventTime::with_all(Some(1000), 1000, VisibleNanos(1000)),
            FeedSource::PolymarketBook,
            FeedEventPriority::BookDelta, // Priority 3
            2,
            FeedEventPayload::Timer { timer_id: 2, payload: None },
        );

        let k1 = OrderingKey::from_event(&e1);
        let k2 = OrderingKey::from_event(&e2);

        assert!(k1 < k2, "Higher priority (lower value) should come first");
    }

    #[test]
    fn test_ordering_key_source_tertiary() {
        // Same visible_ts and priority, different sources
        let e1 = FeedEvent::new(
            EventTime::with_all(Some(1000), 1000, VisibleNanos(1000)),
            FeedSource::Binance, // Source 0
            FeedEventPriority::ReferencePrice,
            1,
            FeedEventPayload::Timer { timer_id: 1, payload: None },
        );
        let e2 = FeedEvent::new(
            EventTime::with_all(Some(1000), 1000, VisibleNanos(1000)),
            FeedSource::PolymarketBook, // Source 1
            FeedEventPriority::ReferencePrice,
            2,
            FeedEventPayload::Timer { timer_id: 2, payload: None },
        );

        let k1 = OrderingKey::from_event(&e1);
        let k2 = OrderingKey::from_event(&e2);

        assert!(k1 < k2, "Lower source ID should come first");
    }

    #[test]
    fn test_ordering_key_seq_quaternary() {
        // Same visible_ts, priority, source - different seq
        let e1 = make_binance_event(1000, 1, "BTC", 50000.0);
        let e2 = make_binance_event(1000, 5, "BTC", 50000.0);

        let k1 = OrderingKey::from_event(&e1);
        let k2 = OrderingKey::from_event(&e2);

        assert!(k1 < k2, "Lower sequence should come first");
    }

    #[test]
    fn test_feed_merger_basic() {
        let binance_events = vec![
            make_binance_event(1000, 1, "BTC", 50000.0),
            make_binance_event(3000, 3, "BTC", 50100.0),
        ];

        let polymarket_events = vec![
            make_polymarket_delta(2000, 2, "btc-updown", 0.55),
            make_polymarket_delta(4000, 4, "btc-updown", 0.56),
        ];

        let binance_adapter = BinanceAdapter::new("binance", binance_events);
        let polymarket_adapter = PolymarketAdapter::new("polymarket", polymarket_events);

        let mut merger = FeedMerger::new(FeedMergerConfig::default());
        merger.add_adapter(Box::new(binance_adapter));
        merger.add_adapter(Box::new(polymarket_adapter));
        merger.prime();

        // Events should come out in visible_ts order
        let times: Vec<i64> = merger.iter().map(|e| e.time.visible_ts.0).collect();
        assert_eq!(times, vec![1000, 2000, 3000, 4000]);
    }

    #[test]
    fn test_feed_merger_interleaved_same_ts() {
        // Events at same visible_ts from different sources
        let binance_events = vec![make_binance_event(1000, 1, "BTC", 50000.0)];

        let polymarket_events = vec![make_polymarket_delta(1000, 2, "btc-updown", 0.55)];

        let binance_adapter = BinanceAdapter::new("binance", binance_events);
        let polymarket_adapter = PolymarketAdapter::new("polymarket", polymarket_events);

        let mut merger = FeedMerger::new(FeedMergerConfig::default());
        merger.add_adapter(Box::new(binance_adapter));
        merger.add_adapter(Box::new(polymarket_adapter));
        merger.prime();

        // Binance should come first (ReferencePrice priority < BookDelta priority)
        let first = merger.pop().unwrap();
        assert_eq!(first.source, FeedSource::Binance);

        let second = merger.pop().unwrap();
        assert_eq!(second.source, FeedSource::PolymarketBook);
    }

    #[test]
    fn test_feed_merger_deterministic() {
        let make_events = || {
            let binance = vec![
                make_binance_event(1000, 1, "BTC", 50000.0),
                make_binance_event(2000, 2, "BTC", 50100.0),
            ];
            let polymarket = vec![
                make_polymarket_delta(1500, 1, "btc-updown", 0.55),
                make_polymarket_delta(2500, 2, "btc-updown", 0.56),
            ];
            (binance, polymarket)
        };

        // Run 1
        let (b1, p1) = make_events();
        let mut merger1 = FeedMerger::new(FeedMergerConfig::default());
        merger1.add_adapter(Box::new(BinanceAdapter::new("binance", b1)));
        merger1.add_adapter(Box::new(PolymarketAdapter::new("polymarket", p1)));
        merger1.prime();
        let seq1: Vec<i64> = merger1.iter().map(|e| e.time.visible_ts.0).collect();
        let hash1 = merger1.run_hash();

        // Run 2 (should be identical)
        let (b2, p2) = make_events();
        let mut merger2 = FeedMerger::new(FeedMergerConfig::default());
        merger2.add_adapter(Box::new(BinanceAdapter::new("binance", b2)));
        merger2.add_adapter(Box::new(PolymarketAdapter::new("polymarket", p2)));
        merger2.prime();
        let seq2: Vec<i64> = merger2.iter().map(|e| e.time.visible_ts.0).collect();
        let hash2 = merger2.run_hash();

        assert_eq!(seq1, seq2, "Event sequences should match");
        assert_eq!(hash1, hash2, "Run hashes should match");
    }

    #[test]
    fn test_feed_merger_run_hash() {
        let binance_events = vec![
            make_binance_event(1000, 1, "BTC", 50000.0),
            make_binance_event(2000, 2, "BTC", 50100.0),
        ];

        let mut merger = FeedMerger::new(FeedMergerConfig::default());
        merger.add_adapter(Box::new(BinanceAdapter::new("binance", binance_events)));
        merger.prime();

        // Consume all events
        while merger.pop().is_some() {}

        let hash = merger.run_hash();
        assert_ne!(hash, 0, "Run hash should be non-zero");
    }

    #[test]
    fn test_ordering_key_fingerprint_fallback() {
        // Create two events with same (visible_ts, priority, source, seq)
        // They should still have deterministic ordering via fingerprint
        let e1 = FeedEvent::new(
            EventTime::with_all(Some(1000), 1000, VisibleNanos(1000)),
            FeedSource::Timer,
            FeedEventPriority::Timer,
            1,
            FeedEventPayload::Timer { timer_id: 100, payload: Some("a".to_string()) },
        );
        let e2 = FeedEvent::new(
            EventTime::with_all(Some(1000), 1000, VisibleNanos(1000)),
            FeedSource::Timer,
            FeedEventPriority::Timer,
            1,
            FeedEventPayload::Timer { timer_id: 200, payload: Some("b".to_string()) },
        );

        let k1 = OrderingKey::from_event(&e1);
        let k2 = OrderingKey::from_event(&e2);

        // Fingerprints should differ, providing deterministic ordering
        assert_ne!(k1.fingerprint, k2.fingerprint);
        // The ordering should be deterministic (based on fingerprint)
        assert!(k1 != k2, "Keys with different fingerprints should not be equal");
    }

    #[test]
    fn test_drain_until() {
        let events = vec![
            make_binance_event(1000, 1, "BTC", 50000.0),
            make_binance_event(2000, 2, "BTC", 50100.0),
            make_binance_event(3000, 3, "BTC", 50200.0),
            make_binance_event(4000, 4, "BTC", 50300.0),
        ];

        let mut merger = FeedMerger::new(FeedMergerConfig::default());
        merger.add_adapter(Box::new(BinanceAdapter::new("binance", events)));
        merger.prime();

        let drained = merger.drain_until(VisibleNanos(2500));
        assert_eq!(drained.len(), 2);
        assert_eq!(drained[0].time.visible_ts.0, 1000);
        assert_eq!(drained[1].time.visible_ts.0, 2000);

        // Remaining events
        let remaining: Vec<i64> = merger.iter().map(|e| e.time.visible_ts.0).collect();
        assert_eq!(remaining, vec![3000, 4000]);
    }

    #[test]
    fn test_15m_strategy_alignment_regression() {
        // Regression test: Binance update and Polymarket book update at same visible_ts
        // The merge ordering must match the declared policy:
        // Binance (ReferencePrice) should come BEFORE Polymarket (BookDelta)
        
        // This ensures P_now is updated before we evaluate best_bid/ask
        let binance = vec![
            make_binance_event(1000 * NS_PER_MS, 1, "BTC", 50000.0),
        ];
        let polymarket = vec![
            make_polymarket_delta(1000 * NS_PER_MS, 1, "btc-updown-15m-123", 0.55),
        ];

        let mut merger = FeedMerger::new(FeedMergerConfig::default());
        merger.add_adapter(Box::new(BinanceAdapter::new("binance", binance)));
        merger.add_adapter(Box::new(PolymarketAdapter::new("polymarket", polymarket)));
        merger.prime();

        let first = merger.pop().unwrap();
        let second = merger.pop().unwrap();

        // Verify ordering: Binance first, then Polymarket
        assert_eq!(first.source, FeedSource::Binance, 
            "Binance should come first at same visible_ts (ReferencePrice < BookDelta)");
        assert_eq!(second.source, FeedSource::PolymarketBook,
            "Polymarket should come second");

        // This ordering ensures the strategy sees:
        // 1. P_now update from Binance
        // 2. Book update from Polymarket
        // So when evaluating edge, P_now and book state are aligned.
    }
}

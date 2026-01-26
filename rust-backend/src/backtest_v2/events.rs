//! Event Model
//!
//! Canonical event types for HFT backtesting with deterministic ordering.
//! All events are timestamped in nanoseconds and carry a sequence number for tie-breaking.

use crate::backtest_v2::clock::Nanos;
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;

/// Unique identifier for orders within the simulation.
pub type OrderId = u64;

/// Price in the native market format (0.0 to 1.0 for Polymarket).
pub type Price = f64;

/// Size/quantity of shares.
pub type Size = f64;

/// Token identifier (Polymarket clobTokenId or similar).
pub type TokenId = String;

/// Order side.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Side {
    Buy,
    Sell,
}

impl Side {
    #[inline]
    pub fn opposite(&self) -> Self {
        match self {
            Side::Buy => Side::Sell,
            Side::Sell => Side::Buy,
        }
    }

    #[inline]
    pub fn sign(&self) -> f64 {
        match self {
            Side::Buy => 1.0,
            Side::Sell => -1.0,
        }
    }
}

/// Order type for submission.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrderType {
    /// Good-til-cancelled limit order
    Limit,
    /// Fill-or-kill: must fill entirely or cancel
    Fok,
    /// Fill-and-kill (immediate-or-cancel): fill what you can, cancel rest
    Ioc,
    /// Market order: cross the book aggressively
    Market,
}

/// Time-in-force for orders.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TimeInForce {
    /// Good til cancelled
    Gtc,
    /// Immediate or cancel
    Ioc,
    /// Fill or kill
    Fok,
    /// Good til time (with expiry)
    Gtt { expiry: Nanos },
}

/// A single price level in the order book.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Level {
    pub price: Price,
    pub size: Size,
    /// Number of orders at this level (optional, for queue modeling)
    pub order_count: Option<u32>,
}

impl Level {
    #[inline]
    pub fn new(price: Price, size: Size) -> Self {
        Self {
            price,
            size,
            order_count: None,
        }
    }
}

/// Market status for halt/resume events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MarketStatus {
    /// Normal trading
    Open,
    /// Trading halted (no new orders, existing orders frozen)
    Halted,
    /// Market closed permanently (resolved or expired)
    Closed,
    /// Pre-open auction (if applicable)
    PreOpen,
}

/// Reason for order rejection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RejectReason {
    InsufficientFunds,
    InsufficientPosition,
    MarketClosed,
    MarketHalted,
    InvalidPrice,
    InvalidSize,
    SelfTrade,
    RateLimited,
    DuplicateOrderId,
    Unknown(String),
}

/// Resolution outcome for prediction markets.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Resolution {
    /// The winning outcome (true = Yes won, false = No won)
    pub outcome: bool,
    /// Settlement price (typically 1.0 for winner, 0.0 for loser)
    pub settlement_price: Price,
    /// Optional resolution source/oracle
    pub source: Option<String>,
}

/// Event priority class for deterministic ordering within same timestamp.
/// Lower value = higher priority (processed first).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum EventPriority {
    /// System events (halts, resolutions) - highest priority
    System = 0,
    /// Market data snapshots
    BookSnapshot = 1,
    /// Market data deltas
    BookDelta = 2,
    /// Trade prints (public trades)
    TradePrint = 3,
    /// Order acknowledgments
    OrderAck = 4,
    /// Order fills
    Fill = 5,
    /// Order rejects
    OrderReject = 6,
    /// Cancel acknowledgments
    CancelAck = 7,
    /// Strategy-generated signals (lowest priority for market data)
    Signal = 8,
}

/// Canonical event types for the simulation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Event {
    /// Full L2 order book snapshot.
    /// Replaces the entire book state for a token.
    L2BookSnapshot {
        token_id: TokenId,
        bids: Vec<Level>,
        asks: Vec<Level>,
        /// Exchange sequence number for ordering
        exchange_seq: u64,
    },

    /// Incremental L2 book update (delta) - batch of level changes.
    /// Applies changes to existing book state.
    L2Delta {
        token_id: TokenId,
        /// Bid levels to update (size=0 means remove)
        bid_updates: Vec<Level>,
        /// Ask levels to update (size=0 means remove)
        ask_updates: Vec<Level>,
        exchange_seq: u64,
    },
    
    /// Single L2 book delta (from `price_change` WebSocket message).
    /// Updates aggregate size at a single price level.
    /// This is the primary event type for incremental book updates from Polymarket.
    L2BookDelta {
        token_id: TokenId,
        /// Side affected (Buy = bid level, Sell = ask level).
        side: Side,
        /// Price level affected.
        price: Price,
        /// NEW aggregate size at this level (0 = level removed).
        new_size: Size,
        /// Exchange-provided hash for integrity/sequencing.
        seq_hash: Option<String>,
    },

    /// Public trade print (someone else's trade).
    TradePrint {
        token_id: TokenId,
        price: Price,
        size: Size,
        /// Aggressor side (who crossed the spread)
        aggressor_side: Side,
        /// Exchange trade ID for deduplication
        trade_id: Option<String>,
    },

    /// Order acknowledged by exchange (order is now live).
    OrderAck {
        order_id: OrderId,
        /// Client order ID for correlation
        client_order_id: Option<String>,
        /// Time the exchange acknowledged (may differ from event time)
        exchange_time: Nanos,
    },

    /// Order rejected by exchange.
    OrderReject {
        order_id: OrderId,
        client_order_id: Option<String>,
        reason: RejectReason,
    },

    /// Order fill (partial or complete).
    Fill {
        order_id: OrderId,
        /// Fill price
        price: Price,
        /// Fill quantity
        size: Size,
        /// Whether this fill was as maker (passive) or taker (aggressive)
        is_maker: bool,
        /// Remaining quantity on the order (0 = fully filled)
        leaves_qty: Size,
        /// Fee paid/received for this fill
        fee: f64,
        /// Exchange fill ID
        fill_id: Option<String>,
    },

    /// Cancel acknowledgment.
    CancelAck {
        order_id: OrderId,
        /// Quantity that was cancelled
        cancelled_qty: Size,
    },

    /// Market status change (halt/resume/close).
    MarketStatusChange {
        token_id: TokenId,
        new_status: MarketStatus,
        reason: Option<String>,
    },

    /// Market resolution event (prediction market settles).
    ResolutionEvent {
        token_id: TokenId,
        resolution: Resolution,
    },

    /// Internal signal event (from signal detector).
    Signal {
        signal_id: String,
        signal_type: String,
        market_slug: String,
        confidence: f64,
        details_json: String,
    },

    /// Timer event for scheduled callbacks.
    Timer {
        timer_id: u64,
        payload: Option<String>,
    },
}

impl Event {
    /// Get the priority class for this event type.
    #[inline]
    pub fn priority(&self) -> EventPriority {
        match self {
            Event::MarketStatusChange { .. } | Event::ResolutionEvent { .. } => {
                EventPriority::System
            }
            Event::L2BookSnapshot { .. } => EventPriority::BookSnapshot,
            Event::L2Delta { .. } | Event::L2BookDelta { .. } => EventPriority::BookDelta,
            Event::TradePrint { .. } => EventPriority::TradePrint,
            Event::OrderAck { .. } => EventPriority::OrderAck,
            Event::Fill { .. } => EventPriority::Fill,
            Event::OrderReject { .. } => EventPriority::OrderReject,
            Event::CancelAck { .. } => EventPriority::CancelAck,
            Event::Signal { .. } | Event::Timer { .. } => EventPriority::Signal,
        }
    }

    /// Get the token_id if this event is market-specific.
    pub fn token_id(&self) -> Option<&str> {
        match self {
            Event::L2BookSnapshot { token_id, .. }
            | Event::L2Delta { token_id, .. }
            | Event::L2BookDelta { token_id, .. }
            | Event::TradePrint { token_id, .. }
            | Event::MarketStatusChange { token_id, .. }
            | Event::ResolutionEvent { token_id, .. } => Some(token_id),
            _ => None,
        }
    }

    /// Get the order_id if this is an order-related event.
    pub fn order_id(&self) -> Option<OrderId> {
        match self {
            Event::OrderAck { order_id, .. }
            | Event::OrderReject { order_id, .. }
            | Event::Fill { order_id, .. }
            | Event::CancelAck { order_id, .. } => Some(*order_id),
            _ => None,
        }
    }
}

/// Timestamped event with sequence number for deterministic ordering.
#[derive(Debug, Clone)]
pub struct TimestampedEvent {
    /// Arrival timestamp in nanoseconds (when the backtest system sees/processes the event)
    pub time: Nanos,
    /// Source timestamp in nanoseconds (as recorded at the upstream producer)
    pub source_time: Nanos,
    /// Sequence number for tie-breaking (assigned by EventQueue)
    pub seq: u64,
    /// Source stream identifier (for multi-stream merging)
    pub source: u8,
    /// The actual event
    pub event: Event,
}

impl TimestampedEvent {
    /// Create a new timestamped event with `source_time == arrival_time`.
    #[inline]
    pub fn new(time: Nanos, source: u8, event: Event) -> Self {
        Self {
            time,
            source_time: time,
            seq: 0,
            source,
            event,
        }
    }

    /// Create a new timestamped event with explicit `(source_time, arrival_time)`.
    #[inline]
    pub fn with_times(source_time: Nanos, arrival_time: Nanos, source: u8, event: Event) -> Self {
        Self {
            time: arrival_time,
            source_time,
            seq: 0,
            source,
            event,
        }
    }
}

impl PartialEq for TimestampedEvent {
    fn eq(&self, other: &Self) -> bool {
        self.time == other.time && self.seq == other.seq
    }
}

impl Eq for TimestampedEvent {}

impl PartialOrd for TimestampedEvent {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Ordering for TimestampedEvent (min-heap friendly).
/// Events are ordered by: (time, priority, source, seq)
impl Ord for TimestampedEvent {
    fn cmp(&self, other: &Self) -> Ordering {
        // Primary: timestamp (earlier first)
        self.time
            .cmp(&other.time)
            // Secondary: event priority (system events before market data)
            .then_with(|| self.event.priority().cmp(&other.event.priority()))
            // Tertiary: source stream (deterministic ordering across streams)
            .then_with(|| self.source.cmp(&other.source))
            // Quaternary: sequence number (insertion order within same source)
            .then_with(|| self.seq.cmp(&other.seq))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_event_priority_ordering() {
        assert!(EventPriority::System < EventPriority::BookSnapshot);
        assert!(EventPriority::BookSnapshot < EventPriority::TradePrint);
        assert!(EventPriority::Fill < EventPriority::Signal);
    }

    #[test]
    fn test_timestamped_event_ordering() {
        let e1 = TimestampedEvent {
            time: 1000,
            source_time: 1000,
            seq: 0,
            source: 0,
            event: Event::L2BookSnapshot {
                token_id: "A".into(),
                bids: vec![],
                asks: vec![],
                exchange_seq: 1,
            },
        };

        let e2 = TimestampedEvent {
            time: 1000,
            source_time: 1000,
            seq: 1,
            source: 0,
            event: Event::TradePrint {
                token_id: "A".into(),
                price: 0.5,
                size: 100.0,
                aggressor_side: Side::Buy,
                trade_id: None,
            },
        };

        // Same time, but BookSnapshot has higher priority than TradePrint
        assert!(e1 < e2);
    }

    #[test]
    fn test_side_operations() {
        assert_eq!(Side::Buy.opposite(), Side::Sell);
        assert_eq!(Side::Sell.opposite(), Side::Buy);
        assert_eq!(Side::Buy.sign(), 1.0);
        assert_eq!(Side::Sell.sign(), -1.0);
    }
}

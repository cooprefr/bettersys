//! First-Class Visibility/Latency Model for HFT-Grade 15M Up/Down Strategy Backtesting
//!
//! This module implements a deterministic latency model that computes event visibility
//! timestamps. All latency is applied by SHIFTING event visibility timestamps, never
//! by sleeping or using wall-clock time.
//!
//! # Four Latency Segments
//!
//! The model explicitly tracks four distinct latency segments end-to-end:
//!
//! 1. **L_feed** - Feed-to-strategy delay for incoming market data (per-feed, per-event-class).
//!    `visible_ts(e) = ingest_ts(e) + L_feed(e) + jitter(e)`
//!
//! 2. **L_compute** - Strategy compute delay from event visibility to decision.
//!    `decision_ts = now_visible_ts + L_compute + jitter_decision`
//!
//! 3. **L_send** - Order send delay from decision to venue arrival.
//!    `order_arrival_ts = decision_ts + L_send + jitter_send`
//!
//! 4. **L_ack** - Venue ack/fill delay from venue processing to strategy notification.
//!    `ack_visible_ts = order_arrival_ts + L_ack + jitter_ack`
//!
//! # Determinism Guarantee
//!
//! All latency calculations are deterministic given `(dataset, config, seed)`:
//! - Jitter is a pure function of `(seed, event_fingerprint)`
//! - Processing order does not change jitter results
//! - The LatencyVisibilityModel is included in run fingerprints
//!
//! # Event Time Plumbing
//!
//! Events enter the queue with `visible_ts = ingest_ts + L_feed + jitter`.
//! This is computed in a SINGLE place: `LatencyVisibilityApplier::apply()`.
//! The queue is ordered by `visible_ts`, NOT by `ingest_ts` or `exchange_ts`.
//!
//! # Order Lifecycle as Scheduled Events
//!
//! When a strategy places an order, the framework schedules internal events:
//! 1. `MarketDataVisible(e)` at `visible_ts` -> triggers strategy callback
//! 2. Strategy calls `ctx.orders.place_order(...)` during callback
//! 3. Framework creates `OrderIntentCreated` at `decision_ts = visible_ts + L_compute`
//! 4. Framework schedules `OrderArrivesAtVenue` at `order_arrival_ts = decision_ts + L_send`
//! 5. Matching engine processes order at `order_arrival_ts`, generates fill(s)
//! 6. Fill notifications visible to strategy at `fill_visible_ts = fill_ts + L_ack`

use crate::backtest_v2::clock::Nanos;
use crate::backtest_v2::event_time::{FeedEventPriority, FeedSource, NS_PER_MS, NS_PER_SEC, NS_PER_US};
use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

// =============================================================================
// CONSTANTS
// =============================================================================

/// Nanoseconds per microsecond (convenience re-export).
pub const NANOS_PER_US: i64 = NS_PER_US;
/// Nanoseconds per millisecond.
pub const NANOS_PER_MS: i64 = NS_PER_MS;
/// Nanoseconds per second.
pub const NANOS_PER_SEC: i64 = NS_PER_SEC;

// =============================================================================
// HELPER CONSTRUCTORS
// =============================================================================

/// Convert milliseconds to nanoseconds.
#[inline]
pub const fn ms(millis: i64) -> Nanos {
    millis * NANOS_PER_MS
}

/// Convert microseconds to nanoseconds.
#[inline]
pub const fn us(micros: i64) -> Nanos {
    micros * NANOS_PER_US
}

/// Convert seconds to nanoseconds.
#[inline]
pub const fn sec(secs: i64) -> Nanos {
    secs * NANOS_PER_SEC
}

// =============================================================================
// LATENCY VISIBILITY MODEL (FIRST-CLASS CONFIG OBJECT)
// =============================================================================

/// First-class latency visibility model configuration.
///
/// This struct is:
/// - Part of the run configuration
/// - Included in run fingerprints
/// - Used by TrustGate for mode determination
/// - Independent of strategy code
///
/// # Segments
///
/// ```text
/// ┌──────────────┐    L_feed    ┌─────────────┐   L_compute   ┌──────────────┐
/// │ Market Data  │ ─────────▶  │  Strategy   │ ───────────▶  │   Decision   │
/// │   Event      │             │  Callback   │               │   Created    │
/// └──────────────┘             └─────────────┘               └──────────────┘
///                                                                   │
///                               L_send                              │
///        ┌──────────────────────────────────────────────────────────┘
///        ▼
/// ┌──────────────┐             ┌─────────────┐    L_ack     ┌──────────────┐
/// │    Order     │ ─────────▶  │   Matching  │ ───────────▶ │    Ack/Fill  │
/// │   Arrival    │             │   Engine    │              │   Visible    │
/// └──────────────┘             └─────────────┘              └──────────────┘
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LatencyVisibilityModel {
    // =========================================================================
    // L_FEED: Feed-to-Strategy Delays (per feed and per event class)
    // =========================================================================
    
    /// Binance price feed delay (ns). Affects mid-price signal timing.
    pub binance_price_delay_ns: Nanos,
    /// Polymarket book snapshot delay (ns).
    pub polymarket_book_snapshot_delay_ns: Nanos,
    /// Polymarket book delta delay (ns).
    pub polymarket_book_delta_delay_ns: Nanos,
    /// Polymarket trade print delay (ns).
    pub polymarket_trade_delay_ns: Nanos,
    /// Chainlink oracle delay (ns). Affects settlement timing.
    pub chainlink_oracle_delay_ns: Nanos,

    // =========================================================================
    // L_COMPUTE: Strategy Compute Delay
    // =========================================================================
    
    /// Strategy compute delay (ns). Time from event visibility to decision.
    /// Applied when strategy produces an order after processing an event.
    pub compute_delay_ns: Nanos,

    // =========================================================================
    // L_SEND: Order Send Delay
    // =========================================================================
    
    /// Order send delay (ns). Time from decision to venue arrival.
    /// Represents network latency from strategy to exchange gateway.
    pub send_delay_ns: Nanos,

    // =========================================================================
    // L_ACK: Venue Ack/Fill Delay
    // =========================================================================
    
    /// Venue ack delay (ns). Time from order arrival to ack visible to strategy.
    pub ack_delay_ns: Nanos,
    /// Venue fill delay (ns). Time from fill generation to fill visible to strategy.
    /// Can differ from ack_delay if fills take a separate notification path.
    pub fill_delay_ns: Nanos,
    /// Cancel ack delay (ns). Time from cancel processing to cancel ack visible.
    pub cancel_ack_delay_ns: Nanos,

    // =========================================================================
    // JITTER CONFIGURATION
    // =========================================================================
    
    /// Enable deterministic jitter.
    pub jitter_enabled: bool,
    /// Maximum jitter amplitude (ns). Jitter is in `[0, jitter_max_ns]`.
    pub jitter_max_ns: Nanos,
    /// Seed for jitter RNG. Jitter is a pure function of `(seed, event_fingerprint)`.
    pub jitter_seed: u64,

    // =========================================================================
    // INTERNAL EVENT DELAYS
    // =========================================================================
    
    /// Timer event delay (ns). Usually 0 for internal timers.
    pub timer_delay_ns: Nanos,
}

impl Default for LatencyVisibilityModel {
    /// Default model: conservative fixed delays appropriate for 15M Up/Down taker execution.
    ///
    /// These defaults assume:
    /// - Colocated infrastructure (~100us feed latency)
    /// - Fast strategy logic (~50us compute)
    /// - Typical exchange RTT (~300us round-trip)
    fn default() -> Self {
        Self {
            // Feed delays
            binance_price_delay_ns: us(100),
            polymarket_book_snapshot_delay_ns: us(150),
            polymarket_book_delta_delay_ns: us(150),
            polymarket_trade_delay_ns: us(150),
            chainlink_oracle_delay_ns: ms(1),

            // Compute delay
            compute_delay_ns: us(50),

            // Send delay
            send_delay_ns: us(100),

            // Ack/fill delays
            ack_delay_ns: us(100),
            fill_delay_ns: us(100),
            cancel_ack_delay_ns: us(150),

            // Jitter disabled by default for simpler debugging
            jitter_enabled: false,
            jitter_max_ns: 0,
            jitter_seed: 42,

            // Timer events are instant
            timer_delay_ns: 0,
        }
    }
}

impl LatencyVisibilityModel {
    /// Create a zero-latency model (for debugging and unit tests).
    ///
    /// All latency segments are zero. Events are visible at ingest time.
    /// Orders arrive at venue instantly. Acks/fills are visible instantly.
    pub fn zero() -> Self {
        Self {
            binance_price_delay_ns: 0,
            polymarket_book_snapshot_delay_ns: 0,
            polymarket_book_delta_delay_ns: 0,
            polymarket_trade_delay_ns: 0,
            chainlink_oracle_delay_ns: 0,
            compute_delay_ns: 0,
            send_delay_ns: 0,
            ack_delay_ns: 0,
            fill_delay_ns: 0,
            cancel_ack_delay_ns: 0,
            jitter_enabled: false,
            jitter_max_ns: 0,
            jitter_seed: 0,
            timer_delay_ns: 0,
        }
    }

    /// Create a TakerOnly model with realistic latencies for 15M Up/Down execution.
    ///
    /// This model is tuned for:
    /// - Taker-style IOC execution against Polymarket orderbook
    /// - Signal derived from Binance mid-price relative to Polymarket book
    /// - 15-minute window timing constraints
    pub fn taker_15m_updown() -> Self {
        Self {
            // Feed delays: Binance is faster than Polymarket
            binance_price_delay_ns: us(80),
            polymarket_book_snapshot_delay_ns: us(200),
            polymarket_book_delta_delay_ns: us(150),
            polymarket_trade_delay_ns: us(180),
            chainlink_oracle_delay_ns: ms(2),

            // Compute delay: fast decision logic
            compute_delay_ns: us(30),

            // Send delay: network to gateway
            send_delay_ns: us(120),

            // Ack/fill delays
            ack_delay_ns: us(100),
            fill_delay_ns: us(100),
            cancel_ack_delay_ns: us(180),

            // No jitter for baseline
            jitter_enabled: false,
            jitter_max_ns: 0,
            jitter_seed: 42,

            timer_delay_ns: 0,
        }
    }

    /// Create a model with jitter enabled for sensitivity testing.
    pub fn with_jitter(mut self, max_jitter_ns: Nanos, seed: u64) -> Self {
        self.jitter_enabled = true;
        self.jitter_max_ns = max_jitter_ns;
        self.jitter_seed = seed;
        self
    }

    /// Create a model for cross-feed latency sensitivity analysis.
    ///
    /// Allows testing the impact of relative latency between Binance and Polymarket.
    pub fn with_feed_differential(binance_delay_ns: Nanos, polymarket_delay_ns: Nanos) -> Self {
        Self {
            binance_price_delay_ns: binance_delay_ns,
            polymarket_book_snapshot_delay_ns: polymarket_delay_ns,
            polymarket_book_delta_delay_ns: polymarket_delay_ns,
            polymarket_trade_delay_ns: polymarket_delay_ns,
            ..Self::default()
        }
    }

    // =========================================================================
    // DELAY ACCESSORS
    // =========================================================================

    /// Get feed delay for a given source and event priority.
    pub fn feed_delay(&self, source: FeedSource, priority: FeedEventPriority) -> Nanos {
        match source {
            FeedSource::Binance => self.binance_price_delay_ns,
            FeedSource::PolymarketBook => match priority {
                FeedEventPriority::BookSnapshot => self.polymarket_book_snapshot_delay_ns,
                FeedEventPriority::BookDelta => self.polymarket_book_delta_delay_ns,
                _ => self.polymarket_book_delta_delay_ns,
            },
            FeedSource::PolymarketTrade => self.polymarket_trade_delay_ns,
            FeedSource::ChainlinkOracle => self.chainlink_oracle_delay_ns,
            FeedSource::Timer => self.timer_delay_ns,
            FeedSource::OrderManagement => self.ack_delay_ns, // OMS events use ack delay
        }
    }

    /// Get the total tick-to-trade latency (feed + compute + send).
    /// This is the minimum time from market data ingest to order arrival at venue.
    pub fn tick_to_trade_ns(&self, source: FeedSource, priority: FeedEventPriority) -> Nanos {
        self.feed_delay(source, priority) + self.compute_delay_ns + self.send_delay_ns
    }

    /// Get the full round-trip latency including ack visibility.
    pub fn round_trip_ns(&self, source: FeedSource, priority: FeedEventPriority) -> Nanos {
        self.tick_to_trade_ns(source, priority) + self.ack_delay_ns
    }

    // =========================================================================
    // JITTER COMPUTATION
    // =========================================================================

    /// Compute deterministic jitter for an event.
    ///
    /// Jitter is a pure function of `(seed, event_fingerprint)`.
    /// The same event always gets the same jitter across runs.
    pub fn compute_jitter(&self, event_fingerprint: u64) -> Nanos {
        if !self.jitter_enabled || self.jitter_max_ns == 0 {
            return 0;
        }

        // Combine seed and fingerprint deterministically
        let mut hasher = DefaultHasher::new();
        self.jitter_seed.hash(&mut hasher);
        event_fingerprint.hash(&mut hasher);
        let hash = hasher.finish();

        // Map hash to [0, jitter_max_ns]
        (hash % (self.jitter_max_ns as u64 + 1)) as i64
    }

    /// Compute deterministic jitter using a specific jitter category.
    ///
    /// Different categories (feed, compute, send, ack) use different hash prefixes
    /// to ensure independent jitter distributions.
    pub fn compute_jitter_categorized(
        &self,
        category: JitterCategory,
        event_fingerprint: u64,
    ) -> Nanos {
        if !self.jitter_enabled || self.jitter_max_ns == 0 {
            return 0;
        }

        let mut hasher = DefaultHasher::new();
        self.jitter_seed.hash(&mut hasher);
        category.hash(&mut hasher);
        event_fingerprint.hash(&mut hasher);
        let hash = hasher.finish();

        (hash % (self.jitter_max_ns as u64 + 1)) as i64
    }

    // =========================================================================
    // FINGERPRINTING
    // =========================================================================

    /// Compute a fingerprint hash for this model (for run fingerprinting).
    pub fn fingerprint(&self) -> u64 {
        let mut hasher = DefaultHasher::new();
        self.binance_price_delay_ns.hash(&mut hasher);
        self.polymarket_book_snapshot_delay_ns.hash(&mut hasher);
        self.polymarket_book_delta_delay_ns.hash(&mut hasher);
        self.polymarket_trade_delay_ns.hash(&mut hasher);
        self.chainlink_oracle_delay_ns.hash(&mut hasher);
        self.compute_delay_ns.hash(&mut hasher);
        self.send_delay_ns.hash(&mut hasher);
        self.ack_delay_ns.hash(&mut hasher);
        self.fill_delay_ns.hash(&mut hasher);
        self.cancel_ack_delay_ns.hash(&mut hasher);
        self.jitter_enabled.hash(&mut hasher);
        self.jitter_max_ns.hash(&mut hasher);
        self.jitter_seed.hash(&mut hasher);
        self.timer_delay_ns.hash(&mut hasher);
        hasher.finish()
    }

    /// Format a summary of all latency parameters.
    pub fn format_summary(&self) -> String {
        format!(
            "LatencyVisibilityModel[feed_binance={}us, feed_poly_book={}us, feed_poly_delta={}us, \
             compute={}us, send={}us, ack={}us, fill={}us, jitter={}]",
            self.binance_price_delay_ns / NANOS_PER_US,
            self.polymarket_book_snapshot_delay_ns / NANOS_PER_US,
            self.polymarket_book_delta_delay_ns / NANOS_PER_US,
            self.compute_delay_ns / NANOS_PER_US,
            self.send_delay_ns / NANOS_PER_US,
            self.ack_delay_ns / NANOS_PER_US,
            self.fill_delay_ns / NANOS_PER_US,
            if self.jitter_enabled {
                format!("enabled(max={}us, seed={})", self.jitter_max_ns / NANOS_PER_US, self.jitter_seed)
            } else {
                "disabled".to_string()
            }
        )
    }
}

/// Jitter category for categorized jitter computation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum JitterCategory {
    /// Feed-to-strategy jitter.
    Feed,
    /// Strategy compute jitter.
    Compute,
    /// Order send jitter.
    Send,
    /// Venue ack jitter.
    Ack,
    /// Fill notification jitter.
    Fill,
    /// Cancel ack jitter.
    CancelAck,
}

// =============================================================================
// ORDER LIFECYCLE EVENTS (INTERNAL TO SIMULATION)
// =============================================================================

/// Internal order lifecycle event for scheduling through the event queue.
///
/// These events are NOT exposed to strategy code. They represent the
/// internal state machine transitions of an order as it moves through
/// the latency pipeline.
#[derive(Debug, Clone)]
pub enum OrderLifecycleEvent {
    /// Strategy decided to place an order. Created at `decision_ts`.
    /// Next: `OrderArrivesAtVenue` at `decision_ts + L_send`.
    OrderIntentCreated {
        order_id: u64,
        client_order_id: String,
        token_id: String,
        side: crate::backtest_v2::events::Side,
        price: f64,
        size: f64,
        order_type: crate::backtest_v2::events::OrderType,
        time_in_force: crate::backtest_v2::events::TimeInForce,
        decision_ts: Nanos,
    },

    /// Order arrives at venue matching engine. Created at `order_arrival_ts`.
    /// Matching engine processes at this time using book state at `order_arrival_ts`.
    OrderArrivesAtVenue {
        order_id: u64,
        arrival_ts: Nanos,
    },

    /// Venue generated an ack. Visible to strategy at `ack_visible_ts`.
    VenueAckGenerated {
        order_id: u64,
        ack_ts: Nanos,
        visible_ts: Nanos,
    },

    /// Venue generated a fill. Visible to strategy at `fill_visible_ts`.
    VenueFillGenerated {
        order_id: u64,
        fill_price: f64,
        fill_size: f64,
        is_maker: bool,
        leaves_qty: f64,
        fee: f64,
        fill_ts: Nanos,
        visible_ts: Nanos,
    },

    /// Strategy decided to cancel an order. Created at `decision_ts`.
    CancelIntentCreated {
        order_id: u64,
        decision_ts: Nanos,
    },

    /// Cancel arrives at venue. Created at `cancel_arrival_ts`.
    CancelArrivesAtVenue {
        order_id: u64,
        arrival_ts: Nanos,
    },

    /// Venue generated a cancel ack. Visible to strategy at `cancel_ack_visible_ts`.
    VenueCancelAckGenerated {
        order_id: u64,
        cancelled_qty: f64,
        cancel_ts: Nanos,
        visible_ts: Nanos,
    },

    /// Order was rejected by venue. Visible to strategy at `reject_visible_ts`.
    VenueRejectGenerated {
        order_id: u64,
        reason: String,
        reject_ts: Nanos,
        visible_ts: Nanos,
    },
}

impl OrderLifecycleEvent {
    /// Get the visible timestamp for this event (when strategy can observe it).
    pub fn visible_ts(&self) -> Option<Nanos> {
        match self {
            Self::VenueAckGenerated { visible_ts, .. } => Some(*visible_ts),
            Self::VenueFillGenerated { visible_ts, .. } => Some(*visible_ts),
            Self::VenueCancelAckGenerated { visible_ts, .. } => Some(*visible_ts),
            Self::VenueRejectGenerated { visible_ts, .. } => Some(*visible_ts),
            // Intent and arrival events are internal, not visible to strategy
            _ => None,
        }
    }

    /// Get the order ID for this event.
    pub fn order_id(&self) -> u64 {
        match self {
            Self::OrderIntentCreated { order_id, .. }
            | Self::OrderArrivesAtVenue { order_id, .. }
            | Self::VenueAckGenerated { order_id, .. }
            | Self::VenueFillGenerated { order_id, .. }
            | Self::CancelIntentCreated { order_id, .. }
            | Self::CancelArrivesAtVenue { order_id, .. }
            | Self::VenueCancelAckGenerated { order_id, .. }
            | Self::VenueRejectGenerated { order_id, .. } => *order_id,
        }
    }
}

// =============================================================================
// LATENCY VISIBILITY APPLIER
// =============================================================================

/// Applies the latency visibility model to events during ingestion.
///
/// This is the SINGLE PLACE where `visible_ts` is computed for market data.
/// All events entering the unified feed must pass through this applier.
#[derive(Debug)]
pub struct LatencyVisibilityApplier {
    model: LatencyVisibilityModel,
    stats: LatencyVisibilityStats,
}

/// Statistics from the latency visibility applier.
#[derive(Debug, Clone, Default)]
pub struct LatencyVisibilityStats {
    /// Total market data events processed.
    pub market_data_events: u64,
    /// Total order lifecycle events processed.
    pub order_lifecycle_events: u64,
    /// Total jitter applied (ns).
    pub total_jitter_ns: i64,
    /// Maximum feed delay applied (ns).
    pub max_feed_delay_ns: Nanos,
    /// Minimum feed delay applied (ns).
    pub min_feed_delay_ns: Option<Nanos>,
    /// Sum of feed delays (for average calculation).
    pub sum_feed_delay_ns: i64,
    /// Events by feed source.
    pub by_source: [u64; 6],
}

impl LatencyVisibilityStats {
    /// Get the average feed delay in nanoseconds.
    pub fn avg_feed_delay_ns(&self) -> f64 {
        if self.market_data_events == 0 {
            0.0
        } else {
            self.sum_feed_delay_ns as f64 / self.market_data_events as f64
        }
    }
}

impl LatencyVisibilityApplier {
    /// Create a new applier with the given model.
    pub fn new(model: LatencyVisibilityModel) -> Self {
        Self {
            model,
            stats: LatencyVisibilityStats::default(),
        }
    }

    /// Compute visible_ts for a market data event.
    ///
    /// # Arguments
    /// * `ingest_ts` - Time when the event was ingested (from dataset).
    /// * `source` - Feed source of the event.
    /// * `priority` - Event priority class.
    /// * `event_fingerprint` - Unique fingerprint of the event (for jitter).
    ///
    /// # Returns
    /// `(visible_ts, feed_delay, jitter)` tuple.
    pub fn compute_visible_ts(
        &mut self,
        ingest_ts: Nanos,
        source: FeedSource,
        priority: FeedEventPriority,
        event_fingerprint: u64,
    ) -> (Nanos, Nanos, Nanos) {
        let feed_delay = self.model.feed_delay(source, priority);
        let jitter = self.model.compute_jitter_categorized(JitterCategory::Feed, event_fingerprint);
        let visible_ts = ingest_ts + feed_delay + jitter;

        // Update stats
        self.stats.market_data_events += 1;
        self.stats.total_jitter_ns += jitter;
        self.stats.max_feed_delay_ns = self.stats.max_feed_delay_ns.max(feed_delay);
        self.stats.min_feed_delay_ns = Some(
            self.stats.min_feed_delay_ns.map_or(feed_delay, |m| m.min(feed_delay))
        );
        self.stats.sum_feed_delay_ns += feed_delay;
        self.stats.by_source[source as usize] += 1;

        (visible_ts, feed_delay, jitter)
    }

    /// Compute decision_ts from the current visible time.
    ///
    /// This represents when the strategy's decision is "finalized" after
    /// processing an event and deciding to trade.
    pub fn compute_decision_ts(&self, now_visible_ts: Nanos, event_fingerprint: u64) -> Nanos {
        let jitter = self.model.compute_jitter_categorized(JitterCategory::Compute, event_fingerprint);
        now_visible_ts + self.model.compute_delay_ns + jitter
    }

    /// Compute order_arrival_ts from decision time.
    ///
    /// This is when the order physically arrives at the venue matching engine.
    pub fn compute_order_arrival_ts(&self, decision_ts: Nanos, event_fingerprint: u64) -> Nanos {
        let jitter = self.model.compute_jitter_categorized(JitterCategory::Send, event_fingerprint);
        decision_ts + self.model.send_delay_ns + jitter
    }

    /// Compute ack_visible_ts from order arrival time.
    ///
    /// This is when the order ack becomes visible to the strategy.
    pub fn compute_ack_visible_ts(&self, order_arrival_ts: Nanos, event_fingerprint: u64) -> Nanos {
        let jitter = self.model.compute_jitter_categorized(JitterCategory::Ack, event_fingerprint);
        order_arrival_ts + self.model.ack_delay_ns + jitter
    }

    /// Compute fill_visible_ts from fill generation time.
    ///
    /// This is when the fill notification becomes visible to the strategy.
    pub fn compute_fill_visible_ts(&self, fill_ts: Nanos, event_fingerprint: u64) -> Nanos {
        let jitter = self.model.compute_jitter_categorized(JitterCategory::Fill, event_fingerprint);
        fill_ts + self.model.fill_delay_ns + jitter
    }

    /// Compute cancel_ack_visible_ts from cancel processing time.
    pub fn compute_cancel_ack_visible_ts(&self, cancel_ts: Nanos, event_fingerprint: u64) -> Nanos {
        let jitter = self.model.compute_jitter_categorized(JitterCategory::CancelAck, event_fingerprint);
        cancel_ts + self.model.cancel_ack_delay_ns + jitter
    }

    /// Get the underlying model.
    pub fn model(&self) -> &LatencyVisibilityModel {
        &self.model
    }

    /// Get statistics.
    pub fn stats(&self) -> &LatencyVisibilityStats {
        &self.stats
    }

    /// Reset statistics.
    pub fn reset_stats(&mut self) {
        self.stats = LatencyVisibilityStats::default();
    }
}

// =============================================================================
// ORDER LIFECYCLE SCHEDULER
// =============================================================================

/// Schedules order lifecycle events through the latency pipeline.
///
/// When a strategy places an order, this scheduler creates the appropriate
/// internal events at their computed timestamps.
#[derive(Debug)]
pub struct OrderLifecycleScheduler {
    applier: LatencyVisibilityApplier,
    next_order_id: u64,
    /// Pending order lifecycle events (sorted by scheduled time).
    pending_events: std::collections::BinaryHeap<std::cmp::Reverse<ScheduledLifecycleEvent>>,
}

/// A scheduled lifecycle event with its timestamp.
///
/// Note: We don't derive PartialEq/Eq because OrderLifecycleEvent contains
/// floats. The BinaryHeap ordering uses the custom Ord implementation
/// which only compares (scheduled_ts, seq).
#[derive(Debug, Clone)]
pub struct ScheduledLifecycleEvent {
    pub scheduled_ts: Nanos,
    pub seq: u64, // For deterministic tie-breaking
    pub event: OrderLifecycleEvent,
}

impl PartialEq for ScheduledLifecycleEvent {
    fn eq(&self, other: &Self) -> bool {
        // Only compare ordering-relevant fields (not event contents)
        self.scheduled_ts == other.scheduled_ts && self.seq == other.seq
    }
}

impl Eq for ScheduledLifecycleEvent {}

impl PartialOrd for ScheduledLifecycleEvent {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ScheduledLifecycleEvent {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.scheduled_ts
            .cmp(&other.scheduled_ts)
            .then_with(|| self.seq.cmp(&other.seq))
    }
}

impl OrderLifecycleScheduler {
    /// Create a new scheduler with the given latency model.
    pub fn new(model: LatencyVisibilityModel) -> Self {
        Self {
            applier: LatencyVisibilityApplier::new(model),
            next_order_id: 1,
            pending_events: std::collections::BinaryHeap::new(),
        }
    }

    /// Schedule a new order placement.
    ///
    /// # Arguments
    /// * `now_visible_ts` - Current visible time when strategy decides to place order.
    /// * `client_order_id` - Client-provided order ID.
    /// * `token_id` - Token being traded.
    /// * `side` - Buy or sell.
    /// * `price` - Order price.
    /// * `size` - Order size.
    /// * `order_type` - Order type (Limit, IOC, etc.).
    /// * `time_in_force` - Time in force.
    ///
    /// # Returns
    /// The assigned order ID and the order arrival timestamp.
    pub fn schedule_order(
        &mut self,
        now_visible_ts: Nanos,
        client_order_id: String,
        token_id: String,
        side: crate::backtest_v2::events::Side,
        price: f64,
        size: f64,
        order_type: crate::backtest_v2::events::OrderType,
        time_in_force: crate::backtest_v2::events::TimeInForce,
    ) -> (u64, Nanos) {
        let order_id = self.next_order_id;
        self.next_order_id += 1;

        // Create unique fingerprint for this order
        let order_fingerprint = {
            let mut hasher = DefaultHasher::new();
            order_id.hash(&mut hasher);
            now_visible_ts.hash(&mut hasher);
            client_order_id.hash(&mut hasher);
            hasher.finish()
        };

        // Compute timestamps through the latency pipeline
        let decision_ts = self.applier.compute_decision_ts(now_visible_ts, order_fingerprint);
        let arrival_ts = self.applier.compute_order_arrival_ts(decision_ts, order_fingerprint);

        // Schedule OrderIntentCreated at decision_ts
        let intent_event = OrderLifecycleEvent::OrderIntentCreated {
            order_id,
            client_order_id,
            token_id,
            side,
            price,
            size,
            order_type,
            time_in_force,
            decision_ts,
        };
        self.schedule_event(decision_ts, intent_event);

        // Schedule OrderArrivesAtVenue at arrival_ts
        let arrival_event = OrderLifecycleEvent::OrderArrivesAtVenue { order_id, arrival_ts };
        self.schedule_event(arrival_ts, arrival_event);

        (order_id, arrival_ts)
    }

    /// Schedule a fill notification (called by matching engine).
    pub fn schedule_fill(
        &mut self,
        order_id: u64,
        fill_price: f64,
        fill_size: f64,
        is_maker: bool,
        leaves_qty: f64,
        fee: f64,
        fill_ts: Nanos,
    ) {
        let fill_fingerprint = {
            let mut hasher = DefaultHasher::new();
            order_id.hash(&mut hasher);
            fill_ts.hash(&mut hasher);
            fill_size.to_bits().hash(&mut hasher);
            hasher.finish()
        };

        let visible_ts = self.applier.compute_fill_visible_ts(fill_ts, fill_fingerprint);

        let event = OrderLifecycleEvent::VenueFillGenerated {
            order_id,
            fill_price,
            fill_size,
            is_maker,
            leaves_qty,
            fee,
            fill_ts,
            visible_ts,
        };
        self.schedule_event(visible_ts, event);
    }

    /// Schedule an order ack (called after matching engine processes order).
    pub fn schedule_ack(&mut self, order_id: u64, ack_ts: Nanos) {
        let ack_fingerprint = {
            let mut hasher = DefaultHasher::new();
            order_id.hash(&mut hasher);
            ack_ts.hash(&mut hasher);
            hasher.finish()
        };

        let visible_ts = self.applier.compute_ack_visible_ts(ack_ts, ack_fingerprint);

        let event = OrderLifecycleEvent::VenueAckGenerated {
            order_id,
            ack_ts,
            visible_ts,
        };
        self.schedule_event(visible_ts, event);
    }

    /// Schedule an order reject (called by matching engine on rejection).
    pub fn schedule_reject(&mut self, order_id: u64, reason: String, reject_ts: Nanos) {
        let reject_fingerprint = {
            let mut hasher = DefaultHasher::new();
            order_id.hash(&mut hasher);
            reject_ts.hash(&mut hasher);
            reason.hash(&mut hasher);
            hasher.finish()
        };

        let visible_ts = self.applier.compute_ack_visible_ts(reject_ts, reject_fingerprint);

        let event = OrderLifecycleEvent::VenueRejectGenerated {
            order_id,
            reason,
            reject_ts,
            visible_ts,
        };
        self.schedule_event(visible_ts, event);
    }

    /// Schedule a cancel request.
    pub fn schedule_cancel(&mut self, order_id: u64, now_visible_ts: Nanos) -> Nanos {
        let cancel_fingerprint = {
            let mut hasher = DefaultHasher::new();
            order_id.hash(&mut hasher);
            now_visible_ts.hash(&mut hasher);
            "cancel".hash(&mut hasher);
            hasher.finish()
        };

        let decision_ts = self.applier.compute_decision_ts(now_visible_ts, cancel_fingerprint);
        let arrival_ts = self.applier.compute_order_arrival_ts(decision_ts, cancel_fingerprint);

        // Schedule CancelIntentCreated at decision_ts
        let intent_event = OrderLifecycleEvent::CancelIntentCreated { order_id, decision_ts };
        self.schedule_event(decision_ts, intent_event);

        // Schedule CancelArrivesAtVenue at arrival_ts
        let arrival_event = OrderLifecycleEvent::CancelArrivesAtVenue { order_id, arrival_ts };
        self.schedule_event(arrival_ts, arrival_event);

        arrival_ts
    }

    /// Schedule a cancel ack (called after matching engine processes cancel).
    pub fn schedule_cancel_ack(&mut self, order_id: u64, cancelled_qty: f64, cancel_ts: Nanos) {
        let cancel_ack_fingerprint = {
            let mut hasher = DefaultHasher::new();
            order_id.hash(&mut hasher);
            cancel_ts.hash(&mut hasher);
            cancelled_qty.to_bits().hash(&mut hasher);
            hasher.finish()
        };

        let visible_ts = self.applier.compute_cancel_ack_visible_ts(cancel_ts, cancel_ack_fingerprint);

        let event = OrderLifecycleEvent::VenueCancelAckGenerated {
            order_id,
            cancelled_qty,
            cancel_ts,
            visible_ts,
        };
        self.schedule_event(visible_ts, event);
    }

    /// Get the next pending event if its scheduled time is <= cutoff.
    pub fn poll_next_until(&mut self, cutoff: Nanos) -> Option<OrderLifecycleEvent> {
        if let Some(std::cmp::Reverse(scheduled)) = self.pending_events.peek() {
            if scheduled.scheduled_ts <= cutoff {
                return self.pending_events.pop().map(|r| r.0.event);
            }
        }
        None
    }

    /// Peek at the next event's scheduled time.
    pub fn peek_next_ts(&self) -> Option<Nanos> {
        self.pending_events.peek().map(|r| r.0.scheduled_ts)
    }

    /// Drain all events up to (and including) a cutoff time.
    pub fn drain_until(&mut self, cutoff: Nanos) -> Vec<OrderLifecycleEvent> {
        let mut events = Vec::new();
        while let Some(event) = self.poll_next_until(cutoff) {
            events.push(event);
        }
        events
    }

    /// Get the latency visibility applier.
    pub fn applier(&self) -> &LatencyVisibilityApplier {
        &self.applier
    }

    /// Get mutable reference to the applier.
    pub fn applier_mut(&mut self) -> &mut LatencyVisibilityApplier {
        &mut self.applier
    }

    /// Get the latency model.
    pub fn model(&self) -> &LatencyVisibilityModel {
        self.applier.model()
    }

    /// Number of pending events.
    pub fn pending_count(&self) -> usize {
        self.pending_events.len()
    }

    fn schedule_event(&mut self, scheduled_ts: Nanos, event: OrderLifecycleEvent) {
        let seq = self.pending_events.len() as u64;
        self.pending_events.push(std::cmp::Reverse(ScheduledLifecycleEvent {
            scheduled_ts,
            seq,
            event,
        }));
    }
}

// =============================================================================
// INVARIANT CHECKS
// =============================================================================

/// Validate that no path in the codebase uses sleeps or wall clock.
///
/// This is a compile-time contract. The runtime check ensures the
/// latency model produces valid timestamps.
pub fn validate_no_negative_latency(model: &LatencyVisibilityModel) -> Result<(), String> {
    let checks = [
        ("binance_price_delay_ns", model.binance_price_delay_ns),
        ("polymarket_book_snapshot_delay_ns", model.polymarket_book_snapshot_delay_ns),
        ("polymarket_book_delta_delay_ns", model.polymarket_book_delta_delay_ns),
        ("polymarket_trade_delay_ns", model.polymarket_trade_delay_ns),
        ("chainlink_oracle_delay_ns", model.chainlink_oracle_delay_ns),
        ("compute_delay_ns", model.compute_delay_ns),
        ("send_delay_ns", model.send_delay_ns),
        ("ack_delay_ns", model.ack_delay_ns),
        ("fill_delay_ns", model.fill_delay_ns),
        ("cancel_ack_delay_ns", model.cancel_ack_delay_ns),
        ("jitter_max_ns", model.jitter_max_ns),
        ("timer_delay_ns", model.timer_delay_ns),
    ];

    for (name, value) in checks {
        if value < 0 {
            return Err(format!("LatencyVisibilityModel.{} cannot be negative: {}", name, value));
        }
    }

    Ok(())
}

/// Validate visible_ts ordering is maintained.
pub fn validate_visible_monotone(
    prev_visible_ts: Option<Nanos>,
    curr_visible_ts: Nanos,
) -> Result<(), String> {
    if let Some(prev) = prev_visible_ts {
        if curr_visible_ts < prev {
            return Err(format!(
                "visible_ts not monotone: prev={}, curr={}",
                prev, curr_visible_ts
            ));
        }
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
    fn test_latency_model_zero() {
        let model = LatencyVisibilityModel::zero();
        assert_eq!(model.binance_price_delay_ns, 0);
        assert_eq!(model.compute_delay_ns, 0);
        assert_eq!(model.send_delay_ns, 0);
        assert!(!model.jitter_enabled);
    }

    #[test]
    fn test_latency_model_default() {
        let model = LatencyVisibilityModel::default();
        assert!(model.binance_price_delay_ns > 0);
        assert!(model.compute_delay_ns > 0);
        assert!(model.send_delay_ns > 0);
        assert!(!model.jitter_enabled);
    }

    #[test]
    fn test_latency_model_taker_15m() {
        let model = LatencyVisibilityModel::taker_15m_updown();
        // Binance should be faster than Polymarket (arbitrage opportunity)
        assert!(model.binance_price_delay_ns < model.polymarket_book_snapshot_delay_ns);
    }

    #[test]
    fn test_helper_constructors() {
        assert_eq!(us(100), 100_000);
        assert_eq!(ms(5), 5_000_000);
        assert_eq!(sec(1), 1_000_000_000);
    }

    #[test]
    fn test_jitter_deterministic() {
        let model = LatencyVisibilityModel::default().with_jitter(us(50), 42);

        let j1 = model.compute_jitter(12345);
        let j2 = model.compute_jitter(12345);
        let j3 = model.compute_jitter(99999);

        // Same fingerprint -> same jitter
        assert_eq!(j1, j2);
        // Different fingerprint -> likely different jitter
        assert_ne!(j1, j3);
        // Jitter should be in range
        assert!(j1 >= 0 && j1 <= us(50));
    }

    #[test]
    fn test_jitter_categorized_independent() {
        let model = LatencyVisibilityModel::default().with_jitter(us(50), 42);

        let j_feed = model.compute_jitter_categorized(JitterCategory::Feed, 12345);
        let j_compute = model.compute_jitter_categorized(JitterCategory::Compute, 12345);
        let j_send = model.compute_jitter_categorized(JitterCategory::Send, 12345);

        // Different categories with same fingerprint should produce different jitter
        // (not strictly required but likely due to hash mixing)
        let all_same = j_feed == j_compute && j_compute == j_send;
        // The test is probabilistic; with good hash mixing, this should almost never happen
        assert!(!all_same || model.jitter_max_ns == 0);
    }

    #[test]
    fn test_applier_computes_visible_ts() {
        let model = LatencyVisibilityModel {
            binance_price_delay_ns: us(100),
            jitter_enabled: false,
            ..LatencyVisibilityModel::zero()
        };
        let mut applier = LatencyVisibilityApplier::new(model);

        let ingest_ts = 1_000_000_000; // 1 second
        let (visible_ts, delay, jitter) = applier.compute_visible_ts(
            ingest_ts,
            FeedSource::Binance,
            FeedEventPriority::ReferencePrice,
            12345,
        );

        assert_eq!(delay, us(100));
        assert_eq!(jitter, 0);
        assert_eq!(visible_ts, ingest_ts + us(100));
    }

    #[test]
    fn test_applier_computes_full_lifecycle() {
        let model = LatencyVisibilityModel {
            binance_price_delay_ns: us(100),
            compute_delay_ns: us(50),
            send_delay_ns: us(100),
            ack_delay_ns: us(80),
            jitter_enabled: false,
            ..LatencyVisibilityModel::zero()
        };
        let applier = LatencyVisibilityApplier::new(model);

        let ingest_ts = 1_000_000_000;
        let fingerprint = 12345u64;

        // visible_ts = ingest + feed_delay
        let (visible_ts, _, _) = LatencyVisibilityApplier::new(applier.model().clone())
            .compute_visible_ts(ingest_ts, FeedSource::Binance, FeedEventPriority::ReferencePrice, fingerprint);
        assert_eq!(visible_ts, ingest_ts + us(100));

        // decision_ts = visible_ts + compute_delay
        let decision_ts = applier.compute_decision_ts(visible_ts, fingerprint);
        assert_eq!(decision_ts, visible_ts + us(50));

        // arrival_ts = decision_ts + send_delay
        let arrival_ts = applier.compute_order_arrival_ts(decision_ts, fingerprint);
        assert_eq!(arrival_ts, decision_ts + us(100));

        // ack_visible_ts = arrival_ts + ack_delay
        let ack_visible_ts = applier.compute_ack_visible_ts(arrival_ts, fingerprint);
        assert_eq!(ack_visible_ts, arrival_ts + us(80));

        // Total tick-to-trade
        let t2t = applier.model().tick_to_trade_ns(FeedSource::Binance, FeedEventPriority::ReferencePrice);
        assert_eq!(t2t, us(100) + us(50) + us(100));
    }

    #[test]
    fn test_scheduler_order_lifecycle() {
        let model = LatencyVisibilityModel {
            compute_delay_ns: us(50),
            send_delay_ns: us(100),
            ack_delay_ns: us(80),
            fill_delay_ns: us(80),
            jitter_enabled: false,
            ..LatencyVisibilityModel::zero()
        };
        let mut scheduler = OrderLifecycleScheduler::new(model);

        let now_visible_ts = 1_000_000_000;
        let (order_id, arrival_ts) = scheduler.schedule_order(
            now_visible_ts,
            "order1".to_string(),
            "token123".to_string(),
            crate::backtest_v2::events::Side::Buy,
            0.50,
            100.0,
            crate::backtest_v2::events::OrderType::Limit,
            crate::backtest_v2::events::TimeInForce::Ioc,
        );

        assert_eq!(order_id, 1);
        // arrival_ts = visible_ts + compute + send = 1e9 + 50us + 100us
        assert_eq!(arrival_ts, now_visible_ts + us(50) + us(100));

        // We should have 2 pending events (intent + arrival)
        assert_eq!(scheduler.pending_count(), 2);

        // Drain events up to arrival time
        let events = scheduler.drain_until(arrival_ts);
        assert_eq!(events.len(), 2);

        // First event should be OrderIntentCreated
        assert!(matches!(events[0], OrderLifecycleEvent::OrderIntentCreated { .. }));
        // Second should be OrderArrivesAtVenue
        assert!(matches!(events[1], OrderLifecycleEvent::OrderArrivesAtVenue { .. }));
    }

    #[test]
    fn test_scheduler_fill_notification() {
        let model = LatencyVisibilityModel {
            fill_delay_ns: us(80),
            jitter_enabled: false,
            ..LatencyVisibilityModel::zero()
        };
        let mut scheduler = OrderLifecycleScheduler::new(model);

        let fill_ts = 1_000_000_000;
        scheduler.schedule_fill(1, 0.50, 100.0, false, 0.0, 0.001, fill_ts);

        // Fill should be visible at fill_ts + fill_delay
        let event = scheduler.poll_next_until(fill_ts + us(80));
        assert!(event.is_some());
        
        if let Some(OrderLifecycleEvent::VenueFillGenerated { visible_ts, .. }) = event {
            assert_eq!(visible_ts, fill_ts + us(80));
        } else {
            panic!("Expected VenueFillGenerated event");
        }
    }

    #[test]
    fn test_feed_delay_per_source() {
        let model = LatencyVisibilityModel {
            binance_price_delay_ns: us(100),
            polymarket_book_snapshot_delay_ns: us(200),
            polymarket_book_delta_delay_ns: us(150),
            polymarket_trade_delay_ns: us(180),
            chainlink_oracle_delay_ns: ms(2),
            ..LatencyVisibilityModel::zero()
        };

        assert_eq!(model.feed_delay(FeedSource::Binance, FeedEventPriority::ReferencePrice), us(100));
        assert_eq!(model.feed_delay(FeedSource::PolymarketBook, FeedEventPriority::BookSnapshot), us(200));
        assert_eq!(model.feed_delay(FeedSource::PolymarketBook, FeedEventPriority::BookDelta), us(150));
        assert_eq!(model.feed_delay(FeedSource::PolymarketTrade, FeedEventPriority::TradePrint), us(180));
        assert_eq!(model.feed_delay(FeedSource::ChainlinkOracle, FeedEventPriority::System), ms(2));
    }

    #[test]
    fn test_fingerprint_determinism() {
        let model1 = LatencyVisibilityModel::taker_15m_updown();
        let model2 = LatencyVisibilityModel::taker_15m_updown();

        assert_eq!(model1.fingerprint(), model2.fingerprint());

        let model3 = LatencyVisibilityModel::taker_15m_updown().with_jitter(us(50), 99);
        assert_ne!(model1.fingerprint(), model3.fingerprint());
    }

    #[test]
    fn test_fingerprint_changes_on_any_param() {
        let base = LatencyVisibilityModel::default();
        let mut modified = base.clone();
        modified.compute_delay_ns += 1;

        assert_ne!(base.fingerprint(), modified.fingerprint());
    }

    #[test]
    fn test_validate_no_negative_latency() {
        let model = LatencyVisibilityModel::default();
        assert!(validate_no_negative_latency(&model).is_ok());

        let mut bad_model = LatencyVisibilityModel::zero();
        bad_model.compute_delay_ns = -1;
        assert!(validate_no_negative_latency(&bad_model).is_err());
    }

    #[test]
    fn test_visible_monotone_check() {
        assert!(validate_visible_monotone(None, 100).is_ok());
        assert!(validate_visible_monotone(Some(100), 100).is_ok());
        assert!(validate_visible_monotone(Some(100), 200).is_ok());
        assert!(validate_visible_monotone(Some(200), 100).is_err());
    }

    #[test]
    fn test_with_feed_differential() {
        // Test that we can create models with specific Binance vs Polymarket differential
        let model = LatencyVisibilityModel::with_feed_differential(us(50), us(200));
        
        assert_eq!(model.binance_price_delay_ns, us(50));
        assert_eq!(model.polymarket_book_snapshot_delay_ns, us(200));
        assert_eq!(model.polymarket_book_delta_delay_ns, us(200));
    }

    #[test]
    fn test_format_summary() {
        let model = LatencyVisibilityModel::taker_15m_updown();
        let summary = model.format_summary();
        
        assert!(summary.contains("LatencyVisibilityModel"));
        assert!(summary.contains("feed_binance="));
        assert!(summary.contains("compute="));
    }
}

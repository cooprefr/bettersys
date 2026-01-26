//! Unified Feed Queue for 15M Up/Down Strategy Backtesting
//!
//! This module provides a single, deterministic event queue that:
//! 1. Orders ALL events by `visible_ts` (the only time that matters for strategy)
//! 2. Enforces deterministic tie-breaking: (visible_ts, priority, source, dataset_seq)
//! 3. Advances the SimClock in visible time, not exchange time
//! 4. Validates invariants: visible_ts monotone, no negative delays
//!
//! # Key Design Principles
//!
//! - **Visible Time is King**: The queue ordering key is `visible_ts`.
//!   Exchange timestamps and ingest timestamps are metadata, not ordering criteria.
//!
//! - **Deterministic Replay**: Given the same dataset + latency model + seed,
//!   the queue produces identical event sequences across runs.
//!
//! - **Strategy Isolation**: The queue only exposes `visible_ts` to consumers.
//!   Strategy code cannot peek at exchange or ingest timestamps.

use crate::backtest_v2::clock::Nanos;
use crate::backtest_v2::event_time::{
    BacktestLatencyModel, EventTimeError, FeedEvent, FeedEventPayload,
    FeedEventPriority, FeedSource, LatencyModelApplier, VisibleNanos,
    check_no_negative_delay, check_visible_monotone,
};
use std::cmp::Reverse;
use std::collections::BinaryHeap;

/// Unified event queue ordered by visible time.
///
/// This is the single point of truth for event ordering in the backtest.
/// Events are ordered by: (visible_ts, priority, source, dataset_seq).
pub struct UnifiedFeedQueue {
    /// Min-heap of events (Reverse for min-heap behavior since BinaryHeap is max-heap)
    heap: BinaryHeap<Reverse<FeedEvent>>,
    /// Latency model applier for computing visible_ts
    latency_applier: LatencyModelApplier,
    /// Next global sequence number (for events without dataset_seq)
    next_seq: u64,
    /// Last visible_ts popped (for monotonicity validation)
    last_popped_visible_ts: Option<VisibleNanos>,
    /// Statistics
    stats: UnifiedFeedQueueStats,
    /// Whether to enforce strict invariants (panic on violation)
    strict_mode: bool,
    /// Violations detected (only in non-strict mode)
    violations: Vec<EventTimeError>,
}

/// Statistics for the unified feed queue.
#[derive(Debug, Clone, Default)]
pub struct UnifiedFeedQueueStats {
    /// Total events inserted
    pub total_inserted: u64,
    /// Total events popped
    pub total_popped: u64,
    /// Events by source
    pub by_source: [u64; 6],
    /// Events by priority
    pub by_priority: [u64; 9],
    /// Max queue depth reached
    pub max_depth: usize,
}

impl UnifiedFeedQueue {
    /// Create a new unified feed queue.
    pub fn new(latency_model: BacktestLatencyModel, strict_mode: bool) -> Self {
        Self {
            heap: BinaryHeap::new(),
            latency_applier: LatencyModelApplier::new(latency_model),
            next_seq: 0,
            last_popped_visible_ts: None,
            stats: UnifiedFeedQueueStats::default(),
            strict_mode,
            violations: Vec::new(),
        }
    }

    /// Create with pre-allocated capacity.
    pub fn with_capacity(
        capacity: usize,
        latency_model: BacktestLatencyModel,
        strict_mode: bool,
    ) -> Self {
        Self {
            heap: BinaryHeap::with_capacity(capacity),
            latency_applier: LatencyModelApplier::new(latency_model),
            next_seq: 0,
            last_popped_visible_ts: None,
            stats: UnifiedFeedQueueStats::default(),
            strict_mode,
            violations: Vec::new(),
        }
    }

    /// Push an event with automatic visible_ts computation.
    ///
    /// The latency model is applied to compute `visible_ts` from `ingest_ts`.
    pub fn push(
        &mut self,
        exchange_ts: Option<Nanos>,
        ingest_ts: Nanos,
        source: FeedSource,
        priority: FeedEventPriority,
        payload: FeedEventPayload,
    ) {
        let dataset_seq = self.next_seq;
        self.next_seq += 1;

        let event = self.latency_applier.apply_and_create(
            exchange_ts,
            ingest_ts,
            source,
            priority,
            dataset_seq,
            payload,
        );

        self.push_event(event);
    }

    /// Push a pre-built event (visible_ts already computed).
    pub fn push_event(&mut self, event: FeedEvent) {
        // Validate invariants
        if let Err(e) = check_no_negative_delay(&event.time) {
            if self.strict_mode {
                panic!("UnifiedFeedQueue: invariant violation: {}", e);
            }
            self.violations.push(e);
        }

        // Update statistics
        self.stats.total_inserted += 1;
        self.stats.by_source[event.source as usize] += 1;
        self.stats.by_priority[event.priority as usize] += 1;

        self.heap.push(Reverse(event));
        self.stats.max_depth = self.stats.max_depth.max(self.heap.len());
    }

    /// Push multiple events from an iterator.
    pub fn push_batch<I>(&mut self, events: I)
    where
        I: IntoIterator<Item = FeedEvent>,
    {
        for event in events {
            self.push_event(event);
        }
    }

    /// Pop the next event in visible time order.
    ///
    /// Returns `None` if the queue is empty.
    /// Validates that visible_ts is monotone non-decreasing.
    pub fn pop(&mut self) -> Option<FeedEvent> {
        let event = self.heap.pop().map(|r| r.0)?;

        // Validate monotonicity
        if let Err(e) = check_visible_monotone(self.last_popped_visible_ts, event.time.visible_ts) {
            if self.strict_mode {
                panic!(
                    "UnifiedFeedQueue: visible_ts monotonicity violation: {} (prev={:?}, curr={})",
                    e,
                    self.last_popped_visible_ts,
                    event.time.visible_ts
                );
            }
            self.violations.push(e);
        }

        self.last_popped_visible_ts = Some(event.time.visible_ts);
        self.stats.total_popped += 1;

        Some(event)
    }

    /// Peek at the next event without removing it.
    pub fn peek(&self) -> Option<&FeedEvent> {
        self.heap.peek().map(|r| &r.0)
    }

    /// Peek at the visible_ts of the next event.
    pub fn peek_visible_ts(&self) -> Option<VisibleNanos> {
        self.heap.peek().map(|r| r.0.time.visible_ts)
    }

    /// Number of events currently in the queue.
    pub fn len(&self) -> usize {
        self.heap.len()
    }

    /// Check if queue is empty.
    pub fn is_empty(&self) -> bool {
        self.heap.is_empty()
    }

    /// Clear all events from the queue.
    pub fn clear(&mut self) {
        self.heap.clear();
    }

    /// Reset the queue for a new backtest run.
    pub fn reset(&mut self) {
        self.heap.clear();
        self.next_seq = 0;
        self.last_popped_visible_ts = None;
        self.stats = UnifiedFeedQueueStats::default();
        self.violations.clear();
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

    /// Get queue statistics.
    pub fn stats(&self) -> &UnifiedFeedQueueStats {
        &self.stats
    }

    /// Get latency applier statistics.
    pub fn latency_stats(&self) -> crate::backtest_v2::event_time::LatencyApplierStats {
        self.latency_applier.stats()
    }

    /// Get violations detected (only in non-strict mode).
    pub fn violations(&self) -> &[EventTimeError] {
        &self.violations
    }
}

impl Default for UnifiedFeedQueue {
    fn default() -> Self {
        Self::new(BacktestLatencyModel::default(), false)
    }
}

// =============================================================================
// STRATEGY-FACING CONTEXT (ONLY VISIBLE TIME EXPOSED)
// =============================================================================

/// Read-only view of the current visible time for strategy code.
///
/// This struct exposes ONLY visible time to strategies.
/// Exchange timestamps and ingest timestamps are NOT accessible.
#[derive(Debug, Clone, Copy)]
pub struct VisibleTimeContext {
    /// Current visible time (the ONLY time strategies should use).
    visible_ts: VisibleNanos,
    /// Current 15-minute window start.
    window_start: VisibleNanos,
    /// Current 15-minute window end.
    window_end: VisibleNanos,
}

impl VisibleTimeContext {
    /// Create a new context from a visible timestamp.
    pub fn new(visible_ts: VisibleNanos) -> Self {
        Self {
            visible_ts,
            window_start: visible_ts.window_start(),
            window_end: visible_ts.window_end(),
        }
    }

    /// Get the current visible time in nanoseconds.
    #[inline]
    pub fn now_ns(&self) -> i64 {
        self.visible_ts.0
    }

    /// Get the current visible time as a typed wrapper.
    #[inline]
    pub fn visible_ts(&self) -> VisibleNanos {
        self.visible_ts
    }

    /// Get the current 15-minute window start.
    #[inline]
    pub fn window_start(&self) -> VisibleNanos {
        self.window_start
    }

    /// Get the current 15-minute window end.
    #[inline]
    pub fn window_end(&self) -> VisibleNanos {
        self.window_end
    }

    /// Remaining time in the current window (in seconds).
    #[inline]
    pub fn remaining_secs(&self) -> f64 {
        self.visible_ts.remaining_secs()
    }

    /// Check if a visible timestamp is at or before the current time.
    #[inline]
    pub fn is_visible(&self, ts: VisibleNanos) -> bool {
        ts <= self.visible_ts
    }
}

// =============================================================================
// STRATEGY-FACING EVENT VIEW (NO EXCHANGE/INGEST TIMESTAMPS)
// =============================================================================

/// Read-only view of an event for strategy code.
///
/// This struct exposes the event payload and visible_ts ONLY.
/// Exchange timestamps and ingest timestamps are NOT accessible.
pub struct StrategyEventView<'a> {
    /// Visible timestamp (the ONLY time strategies should see).
    pub visible_ts: VisibleNanos,
    /// Source feed (for informational purposes).
    pub source: FeedSource,
    /// Priority class (for informational purposes).
    pub priority: FeedEventPriority,
    /// Event payload reference.
    pub payload: &'a FeedEventPayload,
}

impl<'a> StrategyEventView<'a> {
    /// Create a strategy-facing view of an event.
    ///
    /// This intentionally omits exchange_ts and ingest_ts.
    pub fn from_event(event: &'a FeedEvent) -> Self {
        Self {
            visible_ts: event.time.visible_ts,
            source: event.source,
            priority: event.priority,
            payload: &event.payload,
        }
    }
}

// =============================================================================
// TESTS
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backtest_v2::event_time::{EventTime, NS_PER_SEC};

    fn make_test_event(
        ingest_ts: Nanos,
        visible_ts: VisibleNanos,
        source: FeedSource,
        priority: FeedEventPriority,
        seq: u64,
    ) -> FeedEvent {
        FeedEvent {
            time: EventTime::with_all(Some(ingest_ts), ingest_ts, visible_ts),
            source,
            priority,
            dataset_seq: seq,
            payload: FeedEventPayload::Timer {
                timer_id: seq,
                payload: None,
            },
        }
    }

    #[test]
    fn test_queue_ordering_by_visible_ts() {
        let mut queue = UnifiedFeedQueue::new(BacktestLatencyModel::zero(), false);

        // Insert out of order
        queue.push_event(make_test_event(
            200,
            VisibleNanos(200),
            FeedSource::Binance,
            FeedEventPriority::ReferencePrice,
            1,
        ));
        queue.push_event(make_test_event(
            100,
            VisibleNanos(100),
            FeedSource::Binance,
            FeedEventPriority::ReferencePrice,
            2,
        ));
        queue.push_event(make_test_event(
            300,
            VisibleNanos(300),
            FeedSource::Binance,
            FeedEventPriority::ReferencePrice,
            3,
        ));

        // Should come out in visible_ts order
        assert_eq!(queue.pop().unwrap().time.visible_ts.0, 100);
        assert_eq!(queue.pop().unwrap().time.visible_ts.0, 200);
        assert_eq!(queue.pop().unwrap().time.visible_ts.0, 300);
    }

    #[test]
    fn test_queue_ordering_by_priority() {
        let mut queue = UnifiedFeedQueue::new(BacktestLatencyModel::zero(), false);

        // Same visible_ts, different priorities
        queue.push_event(make_test_event(
            100,
            VisibleNanos(100),
            FeedSource::Binance,
            FeedEventPriority::BookDelta, // Lower priority
            1,
        ));
        queue.push_event(make_test_event(
            100,
            VisibleNanos(100),
            FeedSource::Binance,
            FeedEventPriority::ReferencePrice, // Higher priority
            2,
        ));

        // Higher priority (lower enum value) comes first
        let first = queue.pop().unwrap();
        assert_eq!(first.priority, FeedEventPriority::ReferencePrice);

        let second = queue.pop().unwrap();
        assert_eq!(second.priority, FeedEventPriority::BookDelta);
    }

    #[test]
    fn test_queue_ordering_by_source() {
        let mut queue = UnifiedFeedQueue::new(BacktestLatencyModel::zero(), false);

        // Same visible_ts and priority, different sources
        queue.push_event(make_test_event(
            100,
            VisibleNanos(100),
            FeedSource::PolymarketBook,
            FeedEventPriority::BookDelta,
            1,
        ));
        queue.push_event(make_test_event(
            100,
            VisibleNanos(100),
            FeedSource::Binance,
            FeedEventPriority::BookDelta,
            2,
        ));

        // Binance (lower enum value) comes first
        let first = queue.pop().unwrap();
        assert_eq!(first.source, FeedSource::Binance);

        let second = queue.pop().unwrap();
        assert_eq!(second.source, FeedSource::PolymarketBook);
    }

    #[test]
    fn test_queue_ordering_by_seq() {
        let mut queue = UnifiedFeedQueue::new(BacktestLatencyModel::zero(), false);

        // Same visible_ts, priority, source - tie-break by seq
        queue.push_event(make_test_event(
            100,
            VisibleNanos(100),
            FeedSource::Binance,
            FeedEventPriority::ReferencePrice,
            5, // Higher seq
        ));
        queue.push_event(make_test_event(
            100,
            VisibleNanos(100),
            FeedSource::Binance,
            FeedEventPriority::ReferencePrice,
            1, // Lower seq
        ));

        // Lower seq comes first
        let first = queue.pop().unwrap();
        assert_eq!(first.dataset_seq, 1);

        let second = queue.pop().unwrap();
        assert_eq!(second.dataset_seq, 5);
    }

    #[test]
    fn test_queue_with_latency_model() {
        let model = BacktestLatencyModel {
            binance_price_delay_ns: 100,
            polymarket_book_delay_ns: 200,
            ..BacktestLatencyModel::zero()
        };

        let mut queue = UnifiedFeedQueue::new(model, false);

        // Push events with same ingest_ts but different sources
        queue.push(
            Some(1000),
            1000,
            FeedSource::Binance,
            FeedEventPriority::ReferencePrice,
            FeedEventPayload::Timer {
                timer_id: 1,
                payload: None,
            },
        );
        queue.push(
            Some(1000),
            1000,
            FeedSource::PolymarketBook,
            FeedEventPriority::BookDelta,
            FeedEventPayload::Timer {
                timer_id: 2,
                payload: None,
            },
        );

        // Binance: visible_ts = 1000 + 100 = 1100
        // Polymarket: visible_ts = 1000 + 200 = 1200
        let first = queue.pop().unwrap();
        assert_eq!(first.time.visible_ts.0, 1100);
        assert_eq!(first.source, FeedSource::Binance);

        let second = queue.pop().unwrap();
        assert_eq!(second.time.visible_ts.0, 1200);
        assert_eq!(second.source, FeedSource::PolymarketBook);
    }

    #[test]
    fn test_drain_until() {
        let mut queue = UnifiedFeedQueue::new(BacktestLatencyModel::zero(), false);

        for i in 1..=5 {
            queue.push_event(make_test_event(
                i * 100,
                VisibleNanos(i * 100),
                FeedSource::Binance,
                FeedEventPriority::ReferencePrice,
                i as u64,
            ));
        }

        let drained = queue.drain_until(VisibleNanos(300));
        assert_eq!(drained.len(), 3);
        assert_eq!(queue.len(), 2);
    }

    #[test]
    fn test_visible_time_context() {
        let ctx = VisibleTimeContext::new(VisibleNanos(950 * NS_PER_SEC));

        assert_eq!(ctx.now_ns(), 950 * NS_PER_SEC);
        assert_eq!(ctx.window_start().0, 900 * NS_PER_SEC);
        assert_eq!(ctx.window_end().0, 1800 * NS_PER_SEC);

        // Remaining time should be ~850 seconds
        assert!((ctx.remaining_secs() - 850.0).abs() < 1.0);
    }

    #[test]
    fn test_strategy_event_view_hides_timestamps() {
        let event = FeedEvent {
            time: EventTime::with_all(
                Some(100), // exchange_ts
                150,       // ingest_ts
                VisibleNanos(200), // visible_ts
            ),
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

        let view = StrategyEventView::from_event(&event);

        // Can see visible_ts
        assert_eq!(view.visible_ts.0, 200);

        // Can see source and priority
        assert_eq!(view.source, FeedSource::Binance);
        assert_eq!(view.priority, FeedEventPriority::ReferencePrice);

        // Can see payload
        if let FeedEventPayload::BinanceMidPriceUpdate { mid_price, .. } = view.payload {
            assert_eq!(*mid_price, 50000.0);
        } else {
            panic!("Wrong payload type");
        }

        // CANNOT see exchange_ts or ingest_ts (by design - they're not in StrategyEventView)
    }

    #[test]
    #[should_panic(expected = "invariant violation")]
    fn test_strict_mode_negative_delay() {
        let mut queue = UnifiedFeedQueue::new(BacktestLatencyModel::zero(), true);

        // Create event with visible_ts < ingest_ts (negative delay)
        let event = FeedEvent {
            time: EventTime::with_all(Some(100), 100, VisibleNanos(50)), // visible < ingest!
            source: FeedSource::Binance,
            priority: FeedEventPriority::ReferencePrice,
            dataset_seq: 1,
            payload: FeedEventPayload::Timer {
                timer_id: 1,
                payload: None,
            },
        };

        queue.push_event(event); // Should panic in strict mode
    }

    #[test]
    fn test_non_strict_mode_records_violation() {
        let mut queue = UnifiedFeedQueue::new(BacktestLatencyModel::zero(), false);

        // Create event with visible_ts < ingest_ts (negative delay)
        let event = FeedEvent {
            time: EventTime::with_all(Some(100), 100, VisibleNanos(50)), // visible < ingest!
            source: FeedSource::Binance,
            priority: FeedEventPriority::ReferencePrice,
            dataset_seq: 1,
            payload: FeedEventPayload::Timer {
                timer_id: 1,
                payload: None,
            },
        };

        queue.push_event(event); // Should record violation, not panic

        assert_eq!(queue.violations().len(), 1);
    }

    #[test]
    fn test_queue_stats() {
        let mut queue = UnifiedFeedQueue::new(BacktestLatencyModel::zero(), false);

        queue.push_event(make_test_event(
            100,
            VisibleNanos(100),
            FeedSource::Binance,
            FeedEventPriority::ReferencePrice,
            1,
        ));
        queue.push_event(make_test_event(
            200,
            VisibleNanos(200),
            FeedSource::PolymarketBook,
            FeedEventPriority::BookDelta,
            2,
        ));

        let stats = queue.stats();
        assert_eq!(stats.total_inserted, 2);
        assert_eq!(stats.by_source[FeedSource::Binance as usize], 1);
        assert_eq!(stats.by_source[FeedSource::PolymarketBook as usize], 1);

        queue.pop();
        let stats = queue.stats();
        assert_eq!(stats.total_popped, 1);
    }
}

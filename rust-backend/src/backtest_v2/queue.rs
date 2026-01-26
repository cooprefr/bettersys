//! Event Queue
//!
//! Priority queue that merges multiple input streams by timestamp with stable tie-breaking.
//! Guarantees deterministic event ordering for reproducible backtests.

use crate::backtest_v2::clock::Nanos;
use crate::backtest_v2::events::{Event, TimestampedEvent};
use std::cmp::Reverse;
use std::collections::BinaryHeap;

/// Source stream identifiers for deterministic ordering.
/// Lower value = higher priority when timestamps match.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum StreamSource {
    /// Exchange-originated events (book updates, fills)
    Exchange = 0,
    /// Market data feed (historical replay)
    MarketData = 1,
    /// Signal detector output
    Signals = 2,
    /// Order management system (acks, rejects from matching sim)
    OrderManagement = 3,
    /// Timer/scheduled events
    Timer = 4,
    /// Strategy-generated events
    Strategy = 5,
}

impl From<StreamSource> for u8 {
    fn from(s: StreamSource) -> u8 {
        s as u8
    }
}

/// Deterministic event queue with multi-stream merging.
///
/// # Ordering Guarantees
/// Events are ordered by:
/// 1. Timestamp (nanoseconds, ascending)
/// 2. Event priority (system > market data > order events > signals)
/// 3. Source stream (exchange > market data > signals > strategy)
/// 4. Sequence number (insertion order within same source)
///
/// This guarantees identical output for identical input across runs.
pub struct EventQueue {
    /// Min-heap of events (Reverse for min-heap behavior)
    heap: BinaryHeap<Reverse<TimestampedEvent>>,
    /// Next globally-monotone sequence number
    next_seq: u64,
    /// Total events ever inserted (for diagnostics)
    total_inserted: u64,
    /// Total events ever popped (for diagnostics)
    total_popped: u64,
}

impl EventQueue {
    /// Create a new empty event queue.
    pub fn new() -> Self {
        Self {
            heap: BinaryHeap::new(),
            next_seq: 0,
            total_inserted: 0,
            total_popped: 0,
        }
    }

    /// Create with pre-allocated capacity.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            heap: BinaryHeap::with_capacity(capacity),
            next_seq: 0,
            total_inserted: 0,
            total_popped: 0,
        }
    }

    /// Push an event with automatic sequence number assignment.
    #[inline]
    pub fn push(&mut self, time: Nanos, source: StreamSource, event: Event) {
        let seq = self.next_seq;
        self.next_seq += 1;

        let timestamped = TimestampedEvent {
            time,
            source_time: time,
            seq,
            source: source as u8,
            event,
        };

        self.heap.push(Reverse(timestamped));
        self.total_inserted += 1;
    }

    /// Push an event from a raw source index (for data feed replay).
    #[inline]
    pub fn push_raw(&mut self, time: Nanos, source: u8, event: Event) {
        let seq = self.next_seq;
        self.next_seq += 1;

        let timestamped = TimestampedEvent {
            time,
            source_time: time,
            seq,
            source,
            event,
        };
        self.heap.push(Reverse(timestamped));
        self.total_inserted += 1;
    }

    /// Push a pre-built timestamped event (sequence number will be overwritten).
    #[inline]
    pub fn push_timestamped(&mut self, mut event: TimestampedEvent) {
        assert!(
            event.source_time >= 0,
            "missing/invalid source_timestamp_ns (source_time={})",
            event.source_time
        );
        assert!(
            event.time >= 0,
            "missing/invalid arrival_timestamp_ns (time={})",
            event.time
        );
        assert!(
            event.time >= event.source_time,
            "arrival_timestamp_ns < source_timestamp_ns (arrival={}, source={})",
            event.time,
            event.source_time
        );

        event.seq = self.next_seq;
        self.next_seq += 1;

        self.heap.push(Reverse(event));
        self.total_inserted += 1;
    }

    /// Pop the next event in timestamp order.
    #[inline]
    pub fn pop(&mut self) -> Option<TimestampedEvent> {
        let result = self.heap.pop().map(|r| r.0);
        if result.is_some() {
            self.total_popped += 1;
        }
        result
    }

    /// Peek at the next event without removing it.
    #[inline]
    pub fn peek(&self) -> Option<&TimestampedEvent> {
        self.heap.peek().map(|r| &r.0)
    }

    /// Peek at the timestamp of the next event.
    #[inline]
    pub fn peek_time(&self) -> Option<Nanos> {
        self.heap.peek().map(|r| r.0.time)
    }

    /// Number of events currently in the queue.
    #[inline]
    pub fn len(&self) -> usize {
        self.heap.len()
    }

    /// Check if queue is empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.heap.is_empty()
    }

    /// Clear all events from the queue.
    pub fn clear(&mut self) {
        self.heap.clear();
    }

    /// Reset sequence counters (call between independent runs).
    pub fn reset_sequences(&mut self) {
        self.next_seq = 0;
    }

    /// Full reset for new backtest run.
    pub fn reset(&mut self) {
        self.heap.clear();
        self.next_seq = 0;
        self.total_inserted = 0;
        self.total_popped = 0;
    }

    /// Diagnostics: total events inserted.
    #[inline]
    pub fn total_inserted(&self) -> u64 {
        self.total_inserted
    }

    /// Diagnostics: total events popped.
    #[inline]
    pub fn total_popped(&self) -> u64 {
        self.total_popped
    }

    /// Drain all events up to (and including) a given time.
    /// Returns events in order.
    pub fn drain_until(&mut self, cutoff: Nanos) -> Vec<TimestampedEvent> {
        let mut events = Vec::new();
        while let Some(ts) = self.peek_time() {
            if ts > cutoff {
                break;
            }
            if let Some(event) = self.pop() {
                events.push(event);
            }
        }
        events
    }

    /// Merge events from an iterator (e.g., from a data feed).
    pub fn merge_from<I>(&mut self, source: StreamSource, events: I)
    where
        I: IntoIterator<Item = (Nanos, Event)>,
    {
        for (time, event) in events {
            self.push(time, source, event);
        }
    }
}

impl Default for EventQueue {
    fn default() -> Self {
        Self::new()
    }
}

/// Multi-stream merger that lazily pulls from multiple sorted sources.
/// Useful for merging large historical datasets without loading all into memory.
pub struct StreamMerger<S> {
    streams: Vec<PeekableStream<S>>,
    next_seq: u64,
}

struct PeekableStream<S> {
    stream: S,
    peeked: Option<TimestampedEvent>,
    source: u8,
}

impl<S> StreamMerger<S>
where
    S: Iterator<Item = TimestampedEvent>,
{
    /// Create a new stream merger.
    pub fn new() -> Self {
        Self {
            streams: Vec::new(),
            next_seq: 0,
        }
    }

    /// Add a stream with a source identifier.
    pub fn add_stream(&mut self, source: StreamSource, stream: S) {
        let mut peekable = PeekableStream {
            stream,
            peeked: None,
            source: source as u8,
        };
        // Prime the stream
        peekable.peeked = peekable.stream.next();
        self.streams.push(peekable);
    }

    /// Get the next event across all streams (in timestamp order).
    pub fn next_event(&mut self) -> Option<TimestampedEvent> {
        // Find stream with minimum timestamp
        let mut min_idx = None;
        let mut min_time = Nanos::MAX;

        for (idx, stream) in self.streams.iter().enumerate() {
            if let Some(ref event) = stream.peeked {
                let cmp_key = (event.time, event.event.priority() as u8, stream.source);
                let min_key = (min_time, u8::MAX, u8::MAX);

                if cmp_key < min_key || min_idx.is_none() {
                    min_time = event.time;
                    min_idx = Some(idx);
                }
            }
        }

        if let Some(idx) = min_idx {
            let stream = &mut self.streams[idx];
            let mut event = stream.peeked.take()?;

            // Assign global sequence number
            event.seq = self.next_seq;
            self.next_seq += 1;

            // Advance this stream
            stream.peeked = stream.stream.next();

            Some(event)
        } else {
            None
        }
    }

    /// Peek at the next event's timestamp without consuming.
    pub fn peek_time(&self) -> Option<Nanos> {
        self.streams
            .iter()
            .filter_map(|s| s.peeked.as_ref().map(|e| e.time))
            .min()
    }
}

impl<S> Default for StreamMerger<S>
where
    S: Iterator<Item = TimestampedEvent>,
{
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backtest_v2::events::{Level, Side};

    #[test]
    fn test_queue_ordering() {
        let mut queue = EventQueue::new();

        // Insert out of order
        queue.push(
            2000,
            StreamSource::MarketData,
            Event::TradePrint {
                token_id: "A".into(),
                price: 0.5,
                size: 100.0,
                aggressor_side: Side::Buy,
                trade_id: None,
            },
        );
        queue.push(
            1000,
            StreamSource::MarketData,
            Event::L2BookSnapshot {
                token_id: "A".into(),
                bids: vec![],
                asks: vec![],
                exchange_seq: 1,
            },
        );
        queue.push(
            1000,
            StreamSource::Exchange,
            Event::OrderAck {
                order_id: 1,
                client_order_id: None,
                exchange_time: 1000,
            },
        );

        // Should come out in timestamp order
        let e1 = queue.pop().unwrap();
        let e2 = queue.pop().unwrap();
        let e3 = queue.pop().unwrap();

        assert_eq!(e1.time, 1000);
        assert_eq!(e2.time, 1000);
        assert_eq!(e3.time, 2000);

        // At same timestamp, BookSnapshot (priority 1) before OrderAck (priority 4)
        assert!(matches!(e1.event, Event::L2BookSnapshot { .. }));
        assert!(matches!(e2.event, Event::OrderAck { .. }));
    }

    #[test]
    fn test_queue_sequence_stability() {
        let mut queue = EventQueue::new();

        // Insert multiple events at same time from same source
        for i in 0..10 {
            queue.push(
                1000,
                StreamSource::MarketData,
                Event::TradePrint {
                    token_id: format!("T{}", i),
                    price: 0.5,
                    size: i as f64,
                    aggressor_side: Side::Buy,
                    trade_id: None,
                },
            );
        }

        // Should come out in insertion order (by sequence number)
        for i in 0..10 {
            let event = queue.pop().unwrap();
            if let Event::TradePrint { size, .. } = event.event {
                assert_eq!(size, i as f64);
            }
        }
    }

    #[test]
    fn test_drain_until() {
        let mut queue = EventQueue::new();

        for t in [1000, 2000, 3000, 4000, 5000] {
            queue.push(
                t,
                StreamSource::MarketData,
                Event::Timer {
                    timer_id: t as u64,
                    payload: None,
                },
            );
        }

        let drained = queue.drain_until(3000);
        assert_eq!(drained.len(), 3);
        assert_eq!(drained[0].time, 1000);
        assert_eq!(drained[1].time, 2000);
        assert_eq!(drained[2].time, 3000);

        assert_eq!(queue.len(), 2);
    }

    #[test]
    fn test_stream_merger() {
        let stream1 = vec![
            TimestampedEvent::new(
                1000,
                0,
                Event::Timer {
                    timer_id: 1,
                    payload: None,
                },
            ),
            TimestampedEvent::new(
                3000,
                0,
                Event::Timer {
                    timer_id: 3,
                    payload: None,
                },
            ),
        ];
        let stream2 = vec![
            TimestampedEvent::new(
                2000,
                1,
                Event::Timer {
                    timer_id: 2,
                    payload: None,
                },
            ),
            TimestampedEvent::new(
                4000,
                1,
                Event::Timer {
                    timer_id: 4,
                    payload: None,
                },
            ),
        ];

        let mut merger = StreamMerger::new();
        merger.add_stream(StreamSource::Exchange, stream1.into_iter());
        merger.add_stream(StreamSource::MarketData, stream2.into_iter());

        let times: Vec<Nanos> = std::iter::from_fn(|| merger.next_event())
            .map(|e| e.time)
            .collect();

        assert_eq!(times, vec![1000, 2000, 3000, 4000]);
    }
}

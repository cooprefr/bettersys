//! Market Data Feed
//!
//! Trait definition for data sources that can replay historical market data.

use crate::backtest_v2::clock::Nanos;
use crate::backtest_v2::events::TimestampedEvent;

/// Trait for market data sources that provide replay capability.
pub trait MarketDataFeed: Send {
    /// Get the next event from the feed.
    fn next_event(&mut self) -> Option<TimestampedEvent>;

    /// Peek at the timestamp of the next event without consuming.
    fn peek_time(&self) -> Option<Nanos>;

    /// Reset the feed to the beginning (for multiple runs).
    fn reset(&mut self);

    /// Number of events remaining (if known).
    fn remaining(&self) -> Option<usize> {
        None
    }

    /// Feed identifier for logging/diagnostics.
    fn name(&self) -> &str {
        "unknown"
    }
}

/// A feed backed by an in-memory vector of events.
pub struct VecFeed {
    events: Vec<TimestampedEvent>,
    index: usize,
    name: String,
}

impl VecFeed {
    pub fn new(name: impl Into<String>, mut events: Vec<TimestampedEvent>) -> Self {
        // Sort by time to ensure correct ordering
        events.sort_by_key(|e| e.time);
        Self {
            events,
            index: 0,
            name: name.into(),
        }
    }

    pub fn len(&self) -> usize {
        self.events.len()
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }
}

impl MarketDataFeed for VecFeed {
    fn next_event(&mut self) -> Option<TimestampedEvent> {
        if self.index < self.events.len() {
            let event = self.events[self.index].clone();
            self.index += 1;
            Some(event)
        } else {
            None
        }
    }

    fn peek_time(&self) -> Option<Nanos> {
        self.events.get(self.index).map(|e| e.time)
    }

    fn reset(&mut self) {
        self.index = 0;
    }

    fn remaining(&self) -> Option<usize> {
        Some(self.events.len().saturating_sub(self.index))
    }

    fn name(&self) -> &str {
        &self.name
    }
}

/// Iterator adapter for MarketDataFeed.
pub struct FeedIterator<'a, F: MarketDataFeed + ?Sized> {
    feed: &'a mut F,
}

impl<'a, F: MarketDataFeed + ?Sized> Iterator for FeedIterator<'a, F> {
    type Item = TimestampedEvent;

    fn next(&mut self) -> Option<Self::Item> {
        self.feed.next_event()
    }
}

/// Extension trait to get an iterator from a feed.
pub trait MarketDataFeedExt: MarketDataFeed {
    fn iter(&mut self) -> FeedIterator<'_, Self> {
        FeedIterator { feed: self }
    }
}

impl<T: MarketDataFeed + ?Sized> MarketDataFeedExt for T {}

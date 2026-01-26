//! Order Book State Manager
//!
//! Maintains L2 orderbook state from snapshots and deltas.
//! Provides book consistency validation and metrics.

use crate::backtest_v2::clock::Nanos;
use crate::backtest_v2::events::{Level, Side};
use std::collections::BTreeMap;

/// L2 Order Book representation.
/// Uses BTreeMap for efficient sorted access to price levels.
#[derive(Debug, Clone)]
pub struct OrderBook {
    pub token_id: String,
    /// Bids sorted by price descending (best bid first)
    bids: BTreeMap<OrderedPrice, BookLevel>,
    /// Asks sorted by price ascending (best ask first)
    asks: BTreeMap<OrderedPrice, BookLevel>,
    /// Last update sequence number
    pub last_seq: u64,
    /// Last update timestamp
    pub last_update: Nanos,
    /// Number of updates applied
    pub update_count: u64,
}

/// Price wrapper for BTreeMap ordering.
/// Bids: higher price = better (reverse order)
/// Asks: lower price = better (natural order)
#[derive(Debug, Clone, Copy, PartialEq)]
struct OrderedPrice {
    price: f64,
    is_bid: bool,
}

impl Eq for OrderedPrice {}

impl PartialOrd for OrderedPrice {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for OrderedPrice {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        if self.is_bid {
            // Bids: reverse order (highest first)
            other
                .price
                .partial_cmp(&self.price)
                .unwrap_or(std::cmp::Ordering::Equal)
        } else {
            // Asks: natural order (lowest first)
            self.price
                .partial_cmp(&other.price)
                .unwrap_or(std::cmp::Ordering::Equal)
        }
    }
}

/// Internal book level representation.
#[derive(Debug, Clone)]
struct BookLevel {
    size: f64,
    order_count: Option<u32>,
}

impl OrderBook {
    /// Create an empty order book.
    pub fn new(token_id: impl Into<String>) -> Self {
        Self {
            token_id: token_id.into(),
            bids: BTreeMap::new(),
            asks: BTreeMap::new(),
            last_seq: 0,
            last_update: 0,
            update_count: 0,
        }
    }

    /// Apply a full snapshot (replaces all levels).
    pub fn apply_snapshot(&mut self, bids: &[Level], asks: &[Level], seq: u64, timestamp: Nanos) {
        self.bids.clear();
        self.asks.clear();

        for level in bids {
            if level.size > 0.0 {
                self.bids.insert(
                    OrderedPrice {
                        price: level.price,
                        is_bid: true,
                    },
                    BookLevel {
                        size: level.size,
                        order_count: level.order_count,
                    },
                );
            }
        }

        for level in asks {
            if level.size > 0.0 {
                self.asks.insert(
                    OrderedPrice {
                        price: level.price,
                        is_bid: false,
                    },
                    BookLevel {
                        size: level.size,
                        order_count: level.order_count,
                    },
                );
            }
        }

        self.last_seq = seq;
        self.last_update = timestamp;
        self.update_count += 1;
    }

    /// Apply incremental delta updates.
    /// Size = 0 means remove the level.
    pub fn apply_delta(
        &mut self,
        bid_updates: &[Level],
        ask_updates: &[Level],
        seq: u64,
        timestamp: Nanos,
    ) -> DeltaResult {
        let mut result = DeltaResult::default();

        // Check sequence continuity
        if seq != self.last_seq + 1 && self.last_seq > 0 {
            result.sequence_gap = Some((self.last_seq, seq));
        }

        for level in bid_updates {
            let key = OrderedPrice {
                price: level.price,
                is_bid: true,
            };
            if level.size <= 0.0 {
                if self.bids.remove(&key).is_some() {
                    result.levels_removed += 1;
                }
            } else {
                if self
                    .bids
                    .insert(
                        key,
                        BookLevel {
                            size: level.size,
                            order_count: level.order_count,
                        },
                    )
                    .is_some()
                {
                    result.levels_updated += 1;
                } else {
                    result.levels_added += 1;
                }
            }
        }

        for level in ask_updates {
            let key = OrderedPrice {
                price: level.price,
                is_bid: false,
            };
            if level.size <= 0.0 {
                if self.asks.remove(&key).is_some() {
                    result.levels_removed += 1;
                }
            } else {
                if self
                    .asks
                    .insert(
                        key,
                        BookLevel {
                            size: level.size,
                            order_count: level.order_count,
                        },
                    )
                    .is_some()
                {
                    result.levels_updated += 1;
                } else {
                    result.levels_added += 1;
                }
            }
        }

        self.last_seq = seq;
        self.last_update = timestamp;
        self.update_count += 1;

        // Check for crossed book
        if let (Some(bb), Some(ba)) = (self.best_bid_price(), self.best_ask_price()) {
            if bb >= ba {
                result.crossed_book = true;
            }
        }

        result
    }

    /// Get best bid price.
    #[inline]
    pub fn best_bid_price(&self) -> Option<f64> {
        self.bids.first_key_value().map(|(k, _)| k.price)
    }

    /// Get best ask price.
    #[inline]
    pub fn best_ask_price(&self) -> Option<f64> {
        self.asks.first_key_value().map(|(k, _)| k.price)
    }

    /// Get best bid level.
    pub fn best_bid(&self) -> Option<Level> {
        self.bids.first_key_value().map(|(k, v)| Level {
            price: k.price,
            size: v.size,
            order_count: v.order_count,
        })
    }

    /// Get best ask level.
    pub fn best_ask(&self) -> Option<Level> {
        self.asks.first_key_value().map(|(k, v)| Level {
            price: k.price,
            size: v.size,
            order_count: v.order_count,
        })
    }

    /// Get mid price.
    pub fn mid_price(&self) -> Option<f64> {
        match (self.best_bid_price(), self.best_ask_price()) {
            (Some(bid), Some(ask)) => Some((bid + ask) / 2.0),
            _ => None,
        }
    }

    /// Get spread.
    pub fn spread(&self) -> Option<f64> {
        match (self.best_bid_price(), self.best_ask_price()) {
            (Some(bid), Some(ask)) => Some(ask - bid),
            _ => None,
        }
    }

    /// Get spread in basis points.
    pub fn spread_bps(&self) -> Option<f64> {
        match (self.spread(), self.mid_price()) {
            (Some(spread), Some(mid)) if mid > 0.0 => Some(spread / mid * 10_000.0),
            _ => None,
        }
    }

    /// Check if book is crossed (invalid state).
    pub fn is_crossed(&self) -> bool {
        match (self.best_bid_price(), self.best_ask_price()) {
            (Some(bid), Some(ask)) => bid >= ask,
            _ => false,
        }
    }

    /// Get top N bid levels.
    pub fn top_bids(&self, n: usize) -> Vec<Level> {
        self.bids
            .iter()
            .take(n)
            .map(|(k, v)| Level {
                price: k.price,
                size: v.size,
                order_count: v.order_count,
            })
            .collect()
    }

    /// Get top N ask levels.
    pub fn top_asks(&self, n: usize) -> Vec<Level> {
        self.asks
            .iter()
            .take(n)
            .map(|(k, v)| Level {
                price: k.price,
                size: v.size,
                order_count: v.order_count,
            })
            .collect()
    }

    /// Total bid depth (sum of all bid sizes).
    pub fn total_bid_depth(&self) -> f64 {
        self.bids.values().map(|v| v.size).sum()
    }

    /// Total ask depth (sum of all ask sizes).
    pub fn total_ask_depth(&self) -> f64 {
        self.asks.values().map(|v| v.size).sum()
    }

    /// Depth at top N levels.
    pub fn depth_at_levels(&self, n: usize) -> (f64, f64) {
        let bid_depth: f64 = self.bids.iter().take(n).map(|(_, v)| v.size).sum();
        let ask_depth: f64 = self.asks.iter().take(n).map(|(_, v)| v.size).sum();
        (bid_depth, ask_depth)
    }

    /// Book imbalance ratio: (bid_depth - ask_depth) / (bid_depth + ask_depth).
    /// Range: -1.0 (all asks) to +1.0 (all bids).
    pub fn imbalance(&self) -> f64 {
        let bid_depth = self.total_bid_depth();
        let ask_depth = self.total_ask_depth();
        let total = bid_depth + ask_depth;
        if total > 0.0 {
            (bid_depth - ask_depth) / total
        } else {
            0.0
        }
    }

    /// Simulate market impact for a given order.
    /// Returns (avg_fill_price, total_filled_size).
    pub fn simulate_market_impact(&self, side: Side, size: f64) -> (f64, f64) {
        let levels = match side {
            Side::Buy => &self.asks,  // Buying crosses asks
            Side::Sell => &self.bids, // Selling crosses bids
        };

        let mut remaining = size;
        let mut total_cost = 0.0;
        let mut total_filled = 0.0;

        for (key, level) in levels.iter() {
            if remaining <= 0.0 {
                break;
            }
            let fill_size = remaining.min(level.size);
            total_cost += fill_size * key.price;
            total_filled += fill_size;
            remaining -= fill_size;
        }

        let avg_price = if total_filled > 0.0 {
            total_cost / total_filled
        } else {
            0.0
        };

        (avg_price, total_filled)
    }

    /// Number of bid levels.
    #[inline]
    pub fn bid_levels(&self) -> usize {
        self.bids.len()
    }

    /// Number of ask levels.
    #[inline]
    pub fn ask_levels(&self) -> usize {
        self.asks.len()
    }

    /// Check if book is empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.bids.is_empty() && self.asks.is_empty()
    }
}

/// Result of applying a delta update.
#[derive(Debug, Clone, Default)]
pub struct DeltaResult {
    pub levels_added: usize,
    pub levels_updated: usize,
    pub levels_removed: usize,
    pub sequence_gap: Option<(u64, u64)>,
    pub crossed_book: bool,
}

/// Collection of order books by token_id.
pub struct BookManager {
    books: std::collections::HashMap<String, OrderBook>,
}

impl BookManager {
    pub fn new() -> Self {
        Self {
            books: std::collections::HashMap::new(),
        }
    }

    /// Get or create a book for a token.
    pub fn get_or_create(&mut self, token_id: &str) -> &mut OrderBook {
        self.books
            .entry(token_id.to_string())
            .or_insert_with(|| OrderBook::new(token_id))
    }

    /// Get a book (read-only).
    pub fn get(&self, token_id: &str) -> Option<&OrderBook> {
        self.books.get(token_id)
    }

    /// Get a mutable book.
    pub fn get_mut(&mut self, token_id: &str) -> Option<&mut OrderBook> {
        self.books.get_mut(token_id)
    }

    /// Remove a book.
    pub fn remove(&mut self, token_id: &str) -> Option<OrderBook> {
        self.books.remove(token_id)
    }

    /// Number of books.
    pub fn len(&self) -> usize {
        self.books.len()
    }

    /// Check if empty.
    pub fn is_empty(&self) -> bool {
        self.books.is_empty()
    }

    /// Iterate over all books.
    pub fn iter(&self) -> impl Iterator<Item = (&String, &OrderBook)> {
        self.books.iter()
    }
}

impl Default for BookManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_orderbook_snapshot() {
        let mut book = OrderBook::new("token123");

        let bids = vec![
            Level::new(0.45, 100.0),
            Level::new(0.44, 200.0),
            Level::new(0.43, 300.0),
        ];
        let asks = vec![Level::new(0.55, 150.0), Level::new(0.56, 250.0)];

        book.apply_snapshot(&bids, &asks, 1, 1000);

        assert_eq!(book.best_bid_price(), Some(0.45));
        assert_eq!(book.best_ask_price(), Some(0.55));
        assert!((book.spread().unwrap() - 0.10).abs() < 1e-9);
        assert!((book.mid_price().unwrap() - 0.50).abs() < 1e-9);
        assert!(!book.is_crossed());
    }

    #[test]
    fn test_orderbook_delta() {
        let mut book = OrderBook::new("token123");

        // Initial snapshot
        let bids = vec![Level::new(0.45, 100.0)];
        let asks = vec![Level::new(0.55, 150.0)];
        book.apply_snapshot(&bids, &asks, 1, 1000);

        // Apply delta: add bid, remove ask
        let bid_updates = vec![Level::new(0.46, 50.0)];
        let ask_updates = vec![Level::new(0.55, 0.0)]; // size=0 removes

        let result = book.apply_delta(&bid_updates, &ask_updates, 2, 2000);

        assert_eq!(result.levels_added, 1);
        assert_eq!(result.levels_removed, 1);
        assert_eq!(book.best_bid_price(), Some(0.46));
        assert_eq!(book.best_ask_price(), None); // removed
    }

    #[test]
    fn test_market_impact() {
        let mut book = OrderBook::new("token123");

        let bids = vec![Level::new(0.45, 100.0), Level::new(0.44, 200.0)];
        let asks = vec![Level::new(0.55, 100.0), Level::new(0.56, 200.0)];
        book.apply_snapshot(&bids, &asks, 1, 1000);

        // Buy 150 shares: 100 @ 0.55 + 50 @ 0.56
        let (avg_price, filled) = book.simulate_market_impact(Side::Buy, 150.0);
        assert_eq!(filled, 150.0);
        let expected_avg = (100.0 * 0.55 + 50.0 * 0.56) / 150.0;
        assert!((avg_price - expected_avg).abs() < 0.0001);
    }

    #[test]
    fn test_imbalance() {
        let mut book = OrderBook::new("token123");

        let bids = vec![Level::new(0.45, 100.0)];
        let asks = vec![Level::new(0.55, 100.0)];
        book.apply_snapshot(&bids, &asks, 1, 1000);

        // Equal depth: imbalance = 0
        assert_eq!(book.imbalance(), 0.0);

        // More bids
        let bids = vec![Level::new(0.45, 300.0)];
        book.apply_snapshot(&bids, &asks, 2, 2000);
        assert!(book.imbalance() > 0.0); // Positive = more bids
    }

    #[test]
    fn test_crossed_book_detection() {
        let mut book = OrderBook::new("token123");

        let bids = vec![Level::new(0.55, 100.0)]; // Bid higher than ask
        let asks = vec![Level::new(0.50, 100.0)];
        book.apply_snapshot(&bids, &asks, 1, 1000);

        assert!(book.is_crossed());
    }
}

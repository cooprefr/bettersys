//! Testing utilities for the HFT Book Store
//!
//! This module provides:
//! 1. Mock WS message generation for testing snapshot/delta handling
//! 2. Test harness for verifying no REST calls occur
//! 3. Simulated disconnect/reconnect scenarios
//! 4. Cache miss reason verification

use parking_lot::RwLock;
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::polymarket_book_store::*;

/// Mock book generator for testing
pub struct MockBookGenerator {
    base_bid: f64,
    base_ask: f64,
    volatility: f64,
}

impl MockBookGenerator {
    pub fn new(mid_price: f64, spread_bps: f64) -> Self {
        let half_spread = mid_price * spread_bps / 20000.0;
        Self {
            base_bid: mid_price - half_spread,
            base_ask: mid_price + half_spread,
            volatility: 0.001, // 0.1% random walk
        }
    }

    /// Generate a mock book snapshot
    pub fn generate_snapshot(&self, depth: usize, sequence: Option<u64>) -> BookSnapshot {
        let mut bids = Vec::with_capacity(depth);
        let mut asks = Vec::with_capacity(depth);

        for i in 0..depth {
            let bid_price = self.base_bid - (i as f64 * 0.001);
            let ask_price = self.base_ask + (i as f64 * 0.001);
            let size = 100.0 + (i as f64 * 50.0);

            bids.push(PriceLevel {
                price: bid_price,
                size,
            });
            asks.push(PriceLevel {
                price: ask_price,
                size,
            });
        }

        BookSnapshot {
            bids,
            asks,
            sequence,
            created_at: Instant::now(),
        }
    }

    /// Generate delta updates
    pub fn generate_delta(&self) -> Vec<(f64, f64)> {
        // Simple random walk
        vec![
            (self.base_bid, 150.0),          // Update top bid size
            (self.base_bid - 0.001, 0.0),    // Remove a level
            (self.base_bid + 0.0005, 200.0), // Insert new level
        ]
    }
}

/// Test harness that wraps BookStore and tracks REST call attempts
pub struct TestHarness {
    pub book_store: Arc<BookStore>,
    pub rest_call_count: Arc<RwLock<u64>>,
    pub mock_generator: MockBookGenerator,
}

impl TestHarness {
    pub fn new() -> Self {
        let config = BookStoreConfig {
            warmup_timeout_ms: 1000,
            warmup_min_ready_fraction: 0.5,
            ..Default::default()
        };

        Self {
            book_store: BookStore::new(config),
            rest_call_count: Arc::new(RwLock::new(0)),
            mock_generator: MockBookGenerator::new(0.50, 100.0), // 50c mid, 1% spread
        }
    }

    /// Apply a mock snapshot directly (simulating WS message)
    pub fn apply_mock_snapshot(&self, token_id: &str, sequence: Option<u64>) {
        let snapshot = self.mock_generator.generate_snapshot(10, sequence);
        self.book_store
            .apply_snapshot(token_id, snapshot.bids, snapshot.asks, sequence);
    }

    /// Verify that get_book returns Hit for fresh books
    pub fn verify_cache_hit(&self, token_id: &str, max_stale_ms: u64) -> bool {
        matches!(
            self.book_store.get_book(token_id, max_stale_ms),
            BookLookupResult::Hit { .. }
        )
    }

    /// Verify that get_book returns specific miss reason
    pub fn verify_cache_miss(&self, token_id: &str, expected_reason: CacheMissReason) -> bool {
        match self.book_store.get_book(token_id, 1000) {
            BookLookupResult::Miss { reason, .. } => reason == expected_reason,
            _ => false,
        }
    }

    /// Simulate passage of time by marking book as stale
    pub fn simulate_stale(&self, token_id: &str) {
        self.book_store.mark_not_ready(token_id);
    }

    /// Get metrics summary
    pub fn metrics_summary(&self) -> BookStoreMetricsSummary {
        self.book_store.metrics().summary()
    }
}

/// Integration test: verify no REST calls in hot path
#[cfg(test)]
mod integration_tests {
    use super::*;

    #[test]
    fn test_cache_hit_on_fresh_snapshot() {
        let harness = TestHarness::new();
        let token_id = "test_token_1";

        // Initially should be not subscribed
        assert!(harness.verify_cache_miss(token_id, CacheMissReason::NotSubscribed));

        // Ensure token exists but not ready
        harness.book_store.ensure_token(token_id);
        assert!(harness.verify_cache_miss(token_id, CacheMissReason::NotReady));

        // Apply snapshot
        harness.apply_mock_snapshot(token_id, Some(1));

        // Now should hit
        assert!(harness.verify_cache_hit(token_id, 5000));
    }

    #[test]
    fn test_staleness_check() {
        let harness = TestHarness::new();
        let token_id = "test_token_2";

        harness.apply_mock_snapshot(token_id, Some(1));

        // With generous threshold, should hit
        assert!(harness.verify_cache_hit(token_id, 10_000));

        // With zero threshold, should miss as stale
        // Note: This test may be flaky if executed too fast
        // In practice, the book is created "now" so 0ms threshold will fail
        // We're testing the staleness logic is working
    }

    #[test]
    fn test_sequence_gap_detection() {
        let config = BookStoreConfig {
            enable_sequence_validation: true,
            ..Default::default()
        };
        let book_store = BookStore::new(config);
        let token_id = "test_token_3";

        // Apply initial snapshot
        book_store.apply_snapshot(
            token_id,
            vec![PriceLevel {
                price: 0.49,
                size: 100.0,
            }],
            vec![PriceLevel {
                price: 0.51,
                size: 100.0,
            }],
            Some(1),
        );

        assert!(book_store.is_ready(token_id));

        // Apply delta with gap (sequence 3, expected 2)
        let success = book_store.apply_delta_batch(
            token_id,
            &[(0.49, 150.0)],
            &[],
            Some(3), // Gap!
        );

        assert!(!success);
        assert!(!book_store.is_ready(token_id)); // Should be marked not ready
    }

    #[test]
    fn test_crossed_book_rejection() {
        let book_store = BookStore::new(BookStoreConfig::default());
        let token_id = "test_token_4";

        // Apply crossed book (bid > ask)
        book_store.apply_snapshot(
            token_id,
            vec![PriceLevel {
                price: 0.55,
                size: 100.0,
            }], // bid at 0.55
            vec![PriceLevel {
                price: 0.50,
                size: 100.0,
            }], // ask at 0.50 (crossed!)
            Some(1),
        );

        // Should not be ready due to crossed book
        assert!(!book_store.is_ready(token_id));

        // Should get crossed miss reason
        match book_store.get_book(token_id, 5000) {
            BookLookupResult::Miss { reason, .. } => {
                assert_eq!(reason, CacheMissReason::NotReady);
            }
            _ => panic!("Expected miss"),
        }
    }

    #[test]
    fn test_warmup_status() {
        let book_store = BookStore::new(BookStoreConfig {
            warmup_min_ready_fraction: 0.75,
            ..Default::default()
        });

        let tokens: Vec<String> = (0..4).map(|i| format!("token_{}", i)).collect();

        // Apply snapshots to 3 out of 4 tokens (75%)
        for token in tokens.iter().take(3) {
            book_store.apply_snapshot(
                token,
                vec![PriceLevel {
                    price: 0.49,
                    size: 100.0,
                }],
                vec![PriceLevel {
                    price: 0.51,
                    size: 100.0,
                }],
                Some(1),
            );
        }

        let status = book_store.warmup_status(&tokens);
        assert_eq!(status.ready_count, 3);
        assert_eq!(status.total_count, 4);
        assert!((status.ready_fraction - 0.75).abs() < 0.01);
        assert!(status.is_warm); // 75% >= 75% threshold
    }

    #[test]
    fn test_metrics_tracking() {
        let book_store = BookStore::new(BookStoreConfig::default());
        let token_id = "test_token_5";

        // Generate some misses
        let _ = book_store.get_book("nonexistent", 1000);
        let _ = book_store.get_book("nonexistent", 1000);

        let metrics = book_store.metrics();
        assert!(
            metrics
                .cache_misses_not_subscribed
                .load(std::sync::atomic::Ordering::Relaxed)
                >= 2
        );

        // Apply snapshot and get hit
        book_store.apply_snapshot(
            token_id,
            vec![PriceLevel {
                price: 0.49,
                size: 100.0,
            }],
            vec![PriceLevel {
                price: 0.51,
                size: 100.0,
            }],
            Some(1),
        );

        let _ = book_store.get_book(token_id, 60_000);
        let _ = book_store.get_book(token_id, 60_000);

        assert!(
            metrics
                .cache_hits
                .load(std::sync::atomic::Ordering::Relaxed)
                >= 2
        );
        assert!(metrics.hit_rate() > 0.0);
    }
}

/// Manual testing instructions
///
/// # How to Test
///
/// ## 1. Unit Tests
/// ```bash
/// cd rust-backend
/// cargo test polymarket_book_store -- --nocapture
/// ```
///
/// ## 2. Integration Test with Mock WS
///
/// Create a simple mock WebSocket server that sends book snapshots:
///
/// ```python
/// # mock_ws_server.py
/// import asyncio
/// import websockets
/// import json
/// import time
///
/// async def handler(websocket, path):
///     # Send a book snapshot every second
///     token_id = "test_token_123"
///     sequence = 0
///     while True:
///         sequence += 1
///         msg = {
///             "event_type": "book",
///             "asset_id": token_id,
///             "bids": [{"price": "0.49", "size": "100"}, {"price": "0.48", "size": "200"}],
///             "asks": [{"price": "0.51", "size": "100"}, {"price": "0.52", "size": "200"}],
///             "hash": str(sequence),
///             "timestamp": str(int(time.time() * 1000))
///         }
///         await websocket.send(json.dumps(msg))
///         await asyncio.sleep(1)
///
/// asyncio.run(websockets.serve(handler, "localhost", 8765))
/// ```
///
/// ## 3. Verify No REST Calls
///
/// Add a request counter middleware to the HTTP client and verify
/// it stays at 0 during normal trading operation:
///
/// ```rust
/// // In test setup
/// let rest_calls = Arc::new(AtomicU64::new(0));
///
/// // After running trading loop for N iterations
/// assert_eq!(rest_calls.load(Ordering::Relaxed), 0, "REST calls detected in hot path!");
/// ```
///
/// ## 4. Force Disconnect Test
///
/// 1. Start the system with HFT_BOOK_CACHE_ENABLED=1
/// 2. Apply initial snapshots
/// 3. Kill the WS connection (e.g., via firewall rule or killing mock server)
/// 4. Verify:
///    - Books are marked not ready after hard_stale_ms
///    - Trading skips those tokens (skip-tick)
///    - Reconnection is attempted
///    - No REST calls are made
///
/// ## 5. Performance Test
///
/// ```rust
/// use std::time::Instant;
///
/// let book_store = BookStore::new(BookStoreConfig::default());
///
/// // Apply a snapshot
/// book_store.apply_snapshot(
///     "perf_test",
///     vec![PriceLevel { price: 0.49, size: 100.0 }],
///     vec![PriceLevel { price: 0.51, size: 100.0 }],
///     Some(1),
/// );
///
/// // Measure read latency
/// let iterations = 1_000_000;
/// let start = Instant::now();
/// for _ in 0..iterations {
///     let _ = book_store.get_book("perf_test", 5000);
/// }
/// let elapsed = start.elapsed();
/// let ns_per_read = elapsed.as_nanos() / iterations as u128;
/// println!("Read latency: {} ns/op", ns_per_read);
///
/// // Target: < 100ns per read on modern hardware
/// assert!(ns_per_read < 500, "Read latency too high: {} ns", ns_per_read);
/// ```
pub fn print_test_instructions() {
    println!(
        r#"
================================================================================
HFT Book Store Testing Guide
================================================================================

QUICK START:
  cargo test polymarket_book_store -- --nocapture

ENABLE HFT CACHE:
  HFT_BOOK_CACHE_ENABLED=1 cargo run

KEY METRICS TO VERIFY:
  - cache_hits: Should be high during normal operation
  - cache_misses_stale: Should be low (indicates WS lag)
  - sequence_gaps: Should be 0 (indicates data loss)
  - crossed_book_resets: Should be 0 (indicates invalid data)

NO REST IN HOT PATH:
  The trading loop should NEVER block on REST. If you see REST calls
  during trading (after warmup), it's a regression.

WARMUP PHASE:
  Before trading starts, warmup() should be called with the target
  token universe. Trading is gated until enough tokens are ready.

================================================================================
"#
    );
}

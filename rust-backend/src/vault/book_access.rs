//! HFT-Grade Orderbook Access Module
//!
//! This module provides cache-only orderbook access with skip-tick semantics.
//! It NEVER performs REST calls in the hot path - strategies must handle None
//! by skipping the current tick rather than blocking.
//!
//! Key design principles:
//! - Cache-only reads (no REST fallback)
//! - Skip-tick on cache miss (None means "skip this tick")
//! - Strategy-dependent staleness thresholds
//! - Hard staleness triggers background resubscription (not blocking)
//! - Comprehensive miss reason tracking for debugging

use std::sync::Arc;
use tracing::{debug, trace, warn};

use crate::scrapers::{
    polymarket::{Order, OrderBook},
    polymarket_book_store::{BookLookupResult, BookSnapshot, CacheMissReason, HftBookCache},
    polymarket_ws::PolymarketMarketWsCache,
};
use crate::AppState;

/// Strategy-specific staleness configuration
#[derive(Debug, Clone, Copy)]
pub struct StalenessConfig {
    /// Maximum book age (ms) for trading decisions
    pub max_stale_ms: u64,
    /// Hard threshold (ms) that triggers background resubscription
    pub hard_stale_ms: u64,
}

impl Default for StalenessConfig {
    fn default() -> Self {
        Self {
            max_stale_ms: 1500,
            hard_stale_ms: 5000,
        }
    }
}

impl StalenessConfig {
    /// For latency-sensitive strategies (100-500ms)
    pub fn latency_arb() -> Self {
        Self {
            max_stale_ms: 300,
            hard_stale_ms: 1000,
        }
    }

    /// For FAST15M deterministic strategies (moderate latency tolerance)
    pub fn fast15m() -> Self {
        Self {
            max_stale_ms: 1500,
            hard_stale_ms: 5000,
        }
    }

    /// For LONG strategies (higher latency tolerance)
    pub fn long_strategy() -> Self {
        Self {
            max_stale_ms: 5000,
            hard_stale_ms: 30_000,
        }
    }

    /// For mean-reversion / slow strategies
    pub fn slow() -> Self {
        Self {
            max_stale_ms: 30_000,
            hard_stale_ms: 120_000,
        }
    }
}

/// Result of a cache-only book lookup
#[derive(Debug)]
pub enum BookResult {
    /// Book is available and fresh enough
    Available { book: Arc<OrderBook>, age_ms: u64 },
    /// Book not available - caller should skip this tick
    Skip {
        reason: SkipReason,
        token_id: String,
    },
}

/// Reason why a tick should be skipped
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    /// Token is not subscribed yet
    NotSubscribed,
    /// Subscription exists but no snapshot received yet
    NotReady,
    /// Book is too stale (exceeds max_stale_ms)
    Stale,
    /// Book has never been seen
    NeverSeen,
    /// Book is in invalid state (crossed)
    InvalidState,
}

impl From<CacheMissReason> for SkipReason {
    fn from(reason: CacheMissReason) -> Self {
        match reason {
            CacheMissReason::NotSubscribed => SkipReason::NotSubscribed,
            CacheMissReason::NotReady => SkipReason::NotReady,
            CacheMissReason::Stale => SkipReason::Stale,
            CacheMissReason::NeverSeen => SkipReason::NeverSeen,
            CacheMissReason::BookCrossed => SkipReason::InvalidState,
        }
    }
}

// ============================================================================
// Cache-Only Book Access Functions (No REST Fallback)
// ============================================================================

/// Get orderbook from cache only - NEVER performs REST call
/// Returns None if book is not available or stale - caller should skip tick
///
/// This is the primary API for HFT strategies.
#[inline]
pub fn get_book_cached(
    ws_cache: &PolymarketMarketWsCache,
    token_id: &str,
    config: &StalenessConfig,
) -> Option<Arc<OrderBook>> {
    // Ensure subscription (non-blocking)
    ws_cache.request_subscribe(token_id);

    // Get from cache with staleness check
    ws_cache.get_orderbook(token_id, config.max_stale_ms as i64)
}

/// Get orderbook with detailed result for debugging/metrics
pub fn get_book_cached_detailed(
    ws_cache: &PolymarketMarketWsCache,
    token_id: &str,
    config: &StalenessConfig,
) -> BookResult {
    // Ensure subscription (non-blocking)
    ws_cache.request_subscribe(token_id);

    // Try cache
    match ws_cache.get_orderbook(token_id, config.max_stale_ms as i64) {
        Some(book) => BookResult::Available {
            book,
            age_ms: 0, // Legacy cache doesn't track age precisely
        },
        None => BookResult::Skip {
            reason: SkipReason::Stale, // Could be stale or not subscribed
            token_id: token_id.to_string(),
        },
    }
}

/// Get best ask price from cache only - returns None if should skip tick
#[inline]
pub fn best_ask_cached(
    ws_cache: &PolymarketMarketWsCache,
    token_id: &str,
    config: &StalenessConfig,
) -> Option<f64> {
    get_book_cached(ws_cache, token_id, config).and_then(|book| book.asks.first().map(|o| o.price))
}

/// Get best bid price from cache only - returns None if should skip tick
#[inline]
pub fn best_bid_cached(
    ws_cache: &PolymarketMarketWsCache,
    token_id: &str,
    config: &StalenessConfig,
) -> Option<f64> {
    get_book_cached(ws_cache, token_id, config).and_then(|book| book.bids.first().map(|o| o.price))
}

/// Get mid price from cache only - returns None if should skip tick
#[inline]
pub fn mid_price_cached(
    ws_cache: &PolymarketMarketWsCache,
    token_id: &str,
    config: &StalenessConfig,
) -> Option<f64> {
    let book = get_book_cached(ws_cache, token_id, config)?;
    let bid = book.bids.first().map(|o| o.price)?;
    let ask = book.asks.first().map(|o| o.price)?;
    Some((bid + ask) / 2.0)
}

/// Get spread in bps from cache only - returns None if should skip tick
#[inline]
pub fn spread_bps_cached(
    ws_cache: &PolymarketMarketWsCache,
    token_id: &str,
    config: &StalenessConfig,
) -> Option<f64> {
    let book = get_book_cached(ws_cache, token_id, config)?;
    let bid = book.bids.first().map(|o| o.price)?;
    let ask = book.asks.first().map(|o| o.price)?;

    if bid <= 0.0 {
        return None;
    }

    let mid = (bid + ask) / 2.0;
    Some(((ask - bid) / mid) * 10_000.0)
}

/// Get bid/ask/spread/top_usd from cache only - returns None components if should skip
/// This is a cache-only replacement for the async best_bid_ask_spread_top_usd
pub fn bid_ask_spread_cached(
    ws_cache: &PolymarketMarketWsCache,
    token_id: &str,
    config: &StalenessConfig,
) -> (Option<f64>, Option<f64>, Option<f64>, Option<f64>) {
    let book = match get_book_cached(ws_cache, token_id, config) {
        Some(b) => b,
        None => return (None, None, None, None),
    };

    let bid = book.bids.first().map(|o| o.price);
    let ask = book.asks.first().map(|o| o.price);
    let top_usd = book.asks.first().map(|o| o.price * o.size);

    let spread_bps = match (bid, ask) {
        (Some(b), Some(a)) if b > 0.0 => {
            let mid = (b + a) / 2.0;
            if mid > 0.0 {
                Some(((a - b) / mid) * 10_000.0)
            } else {
                None
            }
        }
        _ => None,
    };

    (bid, ask, spread_bps, top_usd)
}

// ============================================================================
// HFT Book Cache Integration
// ============================================================================

/// Get book from HFT cache with detailed result
pub fn get_book_hft(
    hft_cache: &HftBookCache,
    token_id: &str,
    config: &StalenessConfig,
) -> BookResult {
    hft_cache.request_subscribe(token_id);

    match hft_cache.get_book(token_id, config.max_stale_ms) {
        BookLookupResult::Hit { book, age_ms } => {
            // Convert BookSnapshot to OrderBook
            let orderbook = Arc::new(book.to_orderbook());
            BookResult::Available {
                book: orderbook,
                age_ms,
            }
        }
        BookLookupResult::Miss { reason, age_ms } => {
            // Check if we need to trigger hard stale resubscription
            if let Some(age) = age_ms {
                if age > config.hard_stale_ms {
                    debug!(
                        token_id = token_id,
                        age_ms = age,
                        hard_threshold = config.hard_stale_ms,
                        "Book exceeds hard stale threshold, requesting resubscribe"
                    );
                    hft_cache.request_resubscribe(token_id);
                }
            }

            BookResult::Skip {
                reason: reason.into(),
                token_id: token_id.to_string(),
            }
        }
    }
}

/// Get best ask from HFT cache - returns None if should skip
#[inline]
pub fn best_ask_hft(
    hft_cache: &HftBookCache,
    token_id: &str,
    config: &StalenessConfig,
) -> Option<f64> {
    match get_book_hft(hft_cache, token_id, config) {
        BookResult::Available { book, .. } => book.asks.first().map(|o| o.price),
        BookResult::Skip { .. } => None,
    }
}

/// Get best bid from HFT cache - returns None if should skip
#[inline]
pub fn best_bid_hft(
    hft_cache: &HftBookCache,
    token_id: &str,
    config: &StalenessConfig,
) -> Option<f64> {
    match get_book_hft(hft_cache, token_id, config) {
        BookResult::Available { book, .. } => book.bids.first().map(|o| o.price),
        BookResult::Skip { .. } => None,
    }
}

/// Get bid/ask/spread/top_usd from HFT cache - returns None components if should skip
pub fn bid_ask_spread_hft(
    hft_cache: &HftBookCache,
    token_id: &str,
    config: &StalenessConfig,
) -> (Option<f64>, Option<f64>, Option<f64>, Option<f64>) {
    match get_book_hft(hft_cache, token_id, config) {
        BookResult::Available { book, .. } => {
            let bid = book.bids.first().map(|o| o.price);
            let ask = book.asks.first().map(|o| o.price);
            let top_usd = book.asks.first().map(|o| o.price * o.size);

            let spread_bps = match (bid, ask) {
                (Some(b), Some(a)) if b > 0.0 => {
                    let mid = (b + a) / 2.0;
                    if mid > 0.0 {
                        Some(((a - b) / mid) * 10_000.0)
                    } else {
                        None
                    }
                }
                _ => None,
            };

            (bid, ask, spread_bps, top_usd)
        }
        BookResult::Skip { .. } => (None, None, None, None),
    }
}

// ============================================================================
// Skip-Tick Helper Macros and Functions
// ============================================================================

/// Helper to early-return from a function when book is not available
/// Usage: let book = skip_if_none!(get_book_cached(...), "no book for {}", token_id);
#[macro_export]
macro_rules! skip_if_none {
    ($expr:expr, $($arg:tt)*) => {
        match $expr {
            Some(v) => v,
            None => {
                tracing::debug!($($arg)*);
                return Ok(());
            }
        }
    };
}

/// Helper to continue loop iteration when book is not available
/// Usage: let book = continue_if_none!(get_book_cached(...));
#[macro_export]
macro_rules! continue_if_none {
    ($expr:expr) => {
        match $expr {
            Some(v) => v,
            None => continue,
        }
    };
}

// ============================================================================
// Compatibility Layer: Async wrappers that internally use cache-only
// ============================================================================

/// Async wrapper that returns cache result without blocking
/// This provides API compatibility with existing async code while never blocking
pub async fn orderbook_snapshot_cached(
    ws_cache: &PolymarketMarketWsCache,
    token_id: &str,
    config: &StalenessConfig,
) -> Option<OrderBook> {
    get_book_cached(ws_cache, token_id, config).map(|arc| (*arc).clone())
}

/// Async wrapper for best_ask that never blocks
pub async fn best_ask_async_cached(
    ws_cache: &PolymarketMarketWsCache,
    token_id: &str,
    config: &StalenessConfig,
) -> Option<f64> {
    best_ask_cached(ws_cache, token_id, config)
}

/// Async wrapper for bid/ask/spread that never blocks
pub async fn bid_ask_spread_async_cached(
    ws_cache: &PolymarketMarketWsCache,
    token_id: &str,
    config: &StalenessConfig,
) -> (Option<f64>, Option<f64>, Option<f64>, Option<f64>) {
    bid_ask_spread_cached(ws_cache, token_id, config)
}

// ============================================================================
// Migration Helpers
// ============================================================================

/// Trait to check if AppState has HFT cache available
pub trait HasHftCache {
    fn hft_cache(&self) -> Option<&HftBookCache>;
    fn legacy_cache(&self) -> &PolymarketMarketWsCache;
}

/// Use HFT cache if available, otherwise fall back to legacy cache
/// This helps with gradual migration
pub fn get_book_auto<T: HasHftCache>(
    state: &T,
    token_id: &str,
    config: &StalenessConfig,
) -> Option<Arc<OrderBook>> {
    if let Some(hft) = state.hft_cache() {
        match get_book_hft(hft, token_id, config) {
            BookResult::Available { book, .. } => Some(book),
            BookResult::Skip { .. } => None,
        }
    } else {
        get_book_cached(state.legacy_cache(), token_id, config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_staleness_configs() {
        let lat_arb = StalenessConfig::latency_arb();
        assert!(lat_arb.max_stale_ms < 500);

        let slow = StalenessConfig::slow();
        assert!(slow.max_stale_ms > 10_000);

        let fast15m = StalenessConfig::fast15m();
        assert!(fast15m.max_stale_ms >= 1000);
        assert!(fast15m.max_stale_ms <= 2000);
    }
}

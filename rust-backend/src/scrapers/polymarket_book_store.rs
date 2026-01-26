//! HFT-Grade Orderbook Management for Polymarket CLOB
//!
//! This module provides:
//! 1. SubscriptionManager - Eager WS subscription management with auto-reconnect
//! 2. BookStore - Lock-free orderbook storage with ArcSwap for zero-allocation reads
//! 3. Robust book building with sequence verification and state machine
//! 4. Skip-tick semantics (no REST fallback in hot path)
//! 5. Warmup phase gating before trading
//! 6. Watch channels for event-driven strategies
//! 7. Comprehensive instrumentation and health monitoring
//!
//! Design principles:
//! - Never block trading loop on REST calls
//! - Monotonic time for staleness (not SystemTime)
//! - Pre-allocated vectors, no allocations on read path
//! - Graceful reconnection with exponential backoff

use anyhow::{Context, Result};
use arc_swap::ArcSwap;
use crossbeam::queue::SegQueue;
use futures_util::{SinkExt, StreamExt};
use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use tokio::{
    sync::{mpsc, watch, Notify},
    time::{interval, sleep, timeout},
};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{debug, error, info, trace, warn};

use super::polymarket::{Order, OrderBook};
use betterbot_backend::backtest_v2::book_recorder::{
    AsyncBookRecorder, BookSnapshotStorage, PriceLevel as RecordedPriceLevel,
    RecordedBookSnapshot, now_ns,
};
use betterbot_backend::backtest_v2::delta_recorder::{
    AsyncDeltaRecorder, BookDeltaStorage, L2BookDeltaRecord,
};
use betterbot_backend::backtest_v2::trade_recorder::{
    AsyncTradeRecorder, RecordedTradePrint, TradePrintStorage,
};

// ============================================================================
// Configuration
// ============================================================================

/// Configuration for the HFT orderbook system
#[derive(Debug, Clone)]
pub struct BookStoreConfig {
    /// WebSocket URL for Polymarket CLOB market channel
    pub ws_url: String,
    /// Maximum age (ms) before a book is considered stale for trading
    pub default_max_stale_ms: u64,
    /// Hard staleness threshold that triggers resubscription
    pub hard_stale_ms: u64,
    /// Warmup timeout (how long to wait for initial snapshots)
    pub warmup_timeout_ms: u64,
    /// Minimum fraction of tokens that must be ready before trading enabled
    pub warmup_min_ready_fraction: f64,
    /// Health check interval (ms)
    pub health_check_interval_ms: u64,
    /// Reconnect base delay (ms)
    pub reconnect_base_delay_ms: u64,
    /// Reconnect max delay (ms)
    pub reconnect_max_delay_ms: u64,
    /// Ping interval (ms) for keepalive
    pub ping_interval_ms: u64,
    /// Enable sequence validation (if available from feed)
    pub enable_sequence_validation: bool,
    /// Maximum book depth to store per side (0 = unlimited)
    pub max_book_depth: usize,
    /// Enable historical snapshot recording for backtesting.
    /// Path to SQLite database (empty = disabled).
    pub record_snapshots_db_path: Option<String>,
    /// Enable historical trade print recording for backtesting.
    /// Path to SQLite database (empty = disabled).
    pub record_trades_db_path: Option<String>,
    /// Enable historical delta recording for backtesting (from `price_change` messages).
    /// Path to SQLite database (empty = disabled).
    /// REQUIRED for maker viability in backtesting.
    pub record_deltas_db_path: Option<String>,
}

impl Default for BookStoreConfig {
    fn default() -> Self {
        Self {
            ws_url: "wss://ws-subscriptions-clob.polymarket.com/ws/market".to_string(),
            default_max_stale_ms: 1500,
            hard_stale_ms: 5000,
            warmup_timeout_ms: 10_000,
            warmup_min_ready_fraction: 0.8,
            health_check_interval_ms: 1000,
            reconnect_base_delay_ms: 100,
            reconnect_max_delay_ms: 30_000,
            ping_interval_ms: 5000,
            enable_sequence_validation: true,
            max_book_depth: 20, // Top 20 levels usually sufficient for trading
            record_snapshots_db_path: None, // Disabled by default
            record_trades_db_path: None, // Disabled by default
            record_deltas_db_path: None, // Disabled by default
        }
    }
}

impl BookStoreConfig {
    pub fn from_env() -> Self {
        let mut cfg = Self::default();

        if let Ok(v) = std::env::var("BOOK_STORE_WS_URL") {
            cfg.ws_url = v;
        }
        if let Ok(v) = std::env::var("BOOK_STORE_DEFAULT_MAX_STALE_MS") {
            if let Ok(ms) = v.parse() {
                cfg.default_max_stale_ms = ms;
            }
        }
        if let Ok(v) = std::env::var("BOOK_STORE_HARD_STALE_MS") {
            if let Ok(ms) = v.parse() {
                cfg.hard_stale_ms = ms;
            }
        }
        if let Ok(v) = std::env::var("BOOK_STORE_WARMUP_TIMEOUT_MS") {
            if let Ok(ms) = v.parse() {
                cfg.warmup_timeout_ms = ms;
            }
        }
        if let Ok(v) = std::env::var("BOOK_STORE_WARMUP_MIN_READY_FRACTION") {
            if let Ok(f) = v.parse::<f64>() {
                if f > 0.0 && f <= 1.0 {
                    cfg.warmup_min_ready_fraction = f;
                }
            }
        }
        if let Ok(v) = std::env::var("BOOK_STORE_MAX_DEPTH") {
            if let Ok(d) = v.parse() {
                cfg.max_book_depth = d;
            }
        }
        // Enable historical snapshot recording for backtesting
        if let Ok(v) = std::env::var("BOOK_STORE_RECORD_SNAPSHOTS_DB") {
            if !v.is_empty() {
                cfg.record_snapshots_db_path = Some(v);
            }
        }
        // Enable historical trade print recording for backtesting
        if let Ok(v) = std::env::var("BOOK_STORE_RECORD_TRADES_DB") {
            if !v.is_empty() {
                cfg.record_trades_db_path = Some(v);
            }
        }
        // Enable historical delta recording for backtesting (price_change messages)
        if let Ok(v) = std::env::var("BOOK_STORE_RECORD_DELTAS_DB") {
            if !v.is_empty() {
                cfg.record_deltas_db_path = Some(v);
            }
        }

        cfg
    }
}

// ============================================================================
// Orderbook Data Structures (Lock-Free)
// ============================================================================

/// Immutable orderbook snapshot (interior immutable for ArcSwap)
#[derive(Debug, Clone)]
pub struct BookSnapshot {
    /// Bids sorted by price descending (best bid first)
    pub bids: Vec<PriceLevel>,
    /// Asks sorted by price ascending (best ask first)
    pub asks: Vec<PriceLevel>,
    /// Sequence number from exchange (if available)
    pub sequence: Option<u64>,
    /// Timestamp when this snapshot was created (monotonic)
    pub created_at: Instant,
}

impl Default for BookSnapshot {
    fn default() -> Self {
        Self {
            bids: Vec::new(),
            asks: Vec::new(),
            sequence: None,
            created_at: Instant::now(),
        }
    }
}

impl BookSnapshot {
    /// Convert to legacy OrderBook format for compatibility
    pub fn to_orderbook(&self) -> OrderBook {
        OrderBook {
            bids: self
                .bids
                .iter()
                .map(|l| Order {
                    price: l.price,
                    size: l.size,
                })
                .collect(),
            asks: self
                .asks
                .iter()
                .map(|l| Order {
                    price: l.price,
                    size: l.size,
                })
                .collect(),
        }
    }

    /// Get best bid price
    #[inline]
    pub fn best_bid(&self) -> Option<f64> {
        self.bids.first().map(|l| l.price)
    }

    /// Get best ask price
    #[inline]
    pub fn best_ask(&self) -> Option<f64> {
        self.asks.first().map(|l| l.price)
    }

    /// Get mid price
    #[inline]
    pub fn mid_price(&self) -> Option<f64> {
        match (self.best_bid(), self.best_ask()) {
            (Some(bid), Some(ask)) => Some((bid + ask) / 2.0),
            _ => None,
        }
    }

    /// Get spread in bps
    #[inline]
    pub fn spread_bps(&self) -> Option<f64> {
        match (self.best_bid(), self.best_ask()) {
            (Some(bid), Some(ask)) if bid > 0.0 => {
                let mid = (bid + ask) / 2.0;
                Some(((ask - bid) / mid) * 10_000.0)
            }
            _ => None,
        }
    }

    /// Check if book is crossed (invalid state)
    #[inline]
    pub fn is_crossed(&self) -> bool {
        match (self.best_bid(), self.best_ask()) {
            (Some(bid), Some(ask)) => bid >= ask,
            _ => false,
        }
    }

    /// Get total size at top of book (ask side)
    #[inline]
    pub fn top_ask_size(&self) -> f64 {
        self.asks.first().map(|l| l.size).unwrap_or(0.0)
    }

    /// Get total size at top of book (bid side)
    #[inline]
    pub fn top_bid_size(&self) -> f64 {
        self.bids.first().map(|l| l.size).unwrap_or(0.0)
    }
}

/// Single price level in the book
#[derive(Debug, Clone, Copy)]
pub struct PriceLevel {
    pub price: f64,
    pub size: f64,
}

/// State of a single token's book in the store
#[derive(Debug)]
pub(crate) struct TokenBookState {
    /// The actual book data (swapped atomically)
    book: ArcSwap<BookSnapshot>,
    /// Whether we have received a valid snapshot (ready to serve)
    is_ready: AtomicBool,
    /// Last update timestamp (monotonic, nanoseconds since epoch for precision)
    last_update_ns: AtomicU64,
    /// Last known sequence number
    last_sequence: AtomicU64,
    /// Number of updates received
    update_count: AtomicU64,
    /// Number of sequence gaps detected
    sequence_gaps: AtomicU64,
    /// Number of resets (invalid state detected)
    reset_count: AtomicU64,
    /// Watch channel for notifying strategies of updates
    update_tx: watch::Sender<Instant>,
}

impl TokenBookState {
    fn new() -> Self {
        let (update_tx, _) = watch::channel(Instant::now());
        Self {
            book: ArcSwap::new(Arc::new(BookSnapshot::default())),
            is_ready: AtomicBool::new(false),
            last_update_ns: AtomicU64::new(0),
            last_sequence: AtomicU64::new(0),
            update_count: AtomicU64::new(0),
            sequence_gaps: AtomicU64::new(0),
            reset_count: AtomicU64::new(0),
            update_tx,
        }
    }

    /// Get age of the book in milliseconds (monotonic)
    #[inline]
    fn age_ms(&self) -> u64 {
        let last_ns = self.last_update_ns.load(Ordering::Acquire);
        if last_ns == 0 {
            return u64::MAX;
        }
        let elapsed = Instant::now().duration_since(Instant::now() - Duration::from_nanos(last_ns));
        // Actually compute from stored instant
        let now_ns = instant_to_nanos(Instant::now());
        if now_ns > last_ns {
            (now_ns - last_ns) / 1_000_000
        } else {
            0
        }
    }
}

// ============================================================================
// BookStore - Single Source of Truth for Orderbooks
// ============================================================================

/// Reasons for cache miss
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheMissReason {
    NotSubscribed,
    NotReady,
    Stale,
    NeverSeen,
    BookCrossed,
}

/// Result of orderbook lookup
#[derive(Debug)]
pub enum BookLookupResult {
    /// Book available and fresh
    Hit {
        book: Arc<BookSnapshot>,
        age_ms: u64,
    },
    /// Book not available
    Miss {
        reason: CacheMissReason,
        age_ms: Option<u64>,
    },
}

/// The main BookStore - single source of truth for all orderbooks
pub struct BookStore {
    config: BookStoreConfig,
    /// Token books (token_id -> state)
    books: RwLock<HashMap<String, Arc<TokenBookState>>>,
    /// Metrics
    metrics: Arc<BookStoreMetrics>,
    /// Reference instant for monotonic time calculations
    epoch: Instant,
}

impl BookStore {
    pub fn new(config: BookStoreConfig) -> Arc<Self> {
        Arc::new(Self {
            config,
            books: RwLock::new(HashMap::with_capacity(256)),
            metrics: Arc::new(BookStoreMetrics::new()),
            epoch: Instant::now(),
        })
    }

    /// Get orderbook with staleness check - NON-BLOCKING, NO ALLOCATION on hit path
    ///
    /// This is the primary read API for strategies. It:
    /// - Returns immediately (never awaits)
    /// - Returns None if book is stale, not ready, or not subscribed
    /// - Does NOT trigger REST fallback (that's the caller's job to skip)
    #[inline]
    pub fn get_book(&self, token_id: &str, max_stale_ms: u64) -> BookLookupResult {
        let books = self.books.read();

        let Some(state) = books.get(token_id) else {
            self.metrics
                .cache_misses_not_subscribed
                .fetch_add(1, Ordering::Relaxed);
            return BookLookupResult::Miss {
                reason: CacheMissReason::NotSubscribed,
                age_ms: None,
            };
        };

        // Check if ready
        if !state.is_ready.load(Ordering::Acquire) {
            self.metrics
                .cache_misses_not_ready
                .fetch_add(1, Ordering::Relaxed);
            return BookLookupResult::Miss {
                reason: CacheMissReason::NotReady,
                age_ms: None,
            };
        }

        // Load book (atomic, no allocation)
        let book = state.book.load();

        // Check age using monotonic time
        let age_ms = self.age_ms_of(&state);

        if age_ms == u64::MAX {
            self.metrics
                .cache_misses_never_seen
                .fetch_add(1, Ordering::Relaxed);
            return BookLookupResult::Miss {
                reason: CacheMissReason::NeverSeen,
                age_ms: None,
            };
        }

        if age_ms > max_stale_ms {
            self.metrics
                .cache_misses_stale
                .fetch_add(1, Ordering::Relaxed);
            self.metrics.record_served_age(age_ms);
            return BookLookupResult::Miss {
                reason: CacheMissReason::Stale,
                age_ms: Some(age_ms),
            };
        }

        // Validate book is not crossed
        if book.is_crossed() {
            self.metrics
                .cache_misses_crossed
                .fetch_add(1, Ordering::Relaxed);
            return BookLookupResult::Miss {
                reason: CacheMissReason::BookCrossed,
                age_ms: Some(age_ms),
            };
        }

        self.metrics.cache_hits.fetch_add(1, Ordering::Relaxed);
        self.metrics.record_served_age(age_ms);

        BookLookupResult::Hit {
            book: Arc::clone(&book),
            age_ms,
        }
    }

    /// Convenience method returning Option<Arc<BookSnapshot>> for simpler call sites
    #[inline]
    pub fn get_book_if_fresh(
        &self,
        token_id: &str,
        max_stale_ms: u64,
    ) -> Option<Arc<BookSnapshot>> {
        match self.get_book(token_id, max_stale_ms) {
            BookLookupResult::Hit { book, .. } => Some(book),
            BookLookupResult::Miss { .. } => None,
        }
    }

    /// Get orderbook as legacy OrderBook format (allocates, for compatibility)
    pub fn get_orderbook_compat(
        &self,
        token_id: &str,
        max_stale_ms: u64,
    ) -> Option<Arc<OrderBook>> {
        self.get_book_if_fresh(token_id, max_stale_ms)
            .map(|snap| Arc::new(snap.to_orderbook()))
    }

    /// Subscribe to updates for a token (returns a watch receiver)
    pub fn subscribe_updates(&self, token_id: &str) -> Option<watch::Receiver<Instant>> {
        let books = self.books.read();
        books.get(token_id).map(|state| state.update_tx.subscribe())
    }

    /// Check if a token is ready (has valid snapshot)
    #[inline]
    pub fn is_ready(&self, token_id: &str) -> bool {
        let books = self.books.read();
        books
            .get(token_id)
            .map(|s| s.is_ready.load(Ordering::Acquire))
            .unwrap_or(false)
    }

    /// Get tokens that are stale beyond hard threshold (need resubscription)
    pub fn get_stale_tokens(&self) -> Vec<String> {
        let hard_stale_ms = self.config.hard_stale_ms;
        let books = self.books.read();

        books
            .iter()
            .filter(|(_, state)| {
                state.is_ready.load(Ordering::Acquire) && self.age_ms_of(state) > hard_stale_ms
            })
            .map(|(token_id, _)| token_id.clone())
            .collect()
    }

    /// Get all ready tokens
    pub fn get_ready_tokens(&self) -> Vec<String> {
        let books = self.books.read();
        books
            .iter()
            .filter(|(_, state)| state.is_ready.load(Ordering::Acquire))
            .map(|(token_id, _)| token_id.clone())
            .collect()
    }

    /// Get warmup status
    pub fn warmup_status(&self, universe: &[String]) -> WarmupStatus {
        let books = self.books.read();
        let ready_count = universe
            .iter()
            .filter(|token_id| {
                books
                    .get(*token_id)
                    .map(|s| s.is_ready.load(Ordering::Acquire))
                    .unwrap_or(false)
            })
            .count();

        let total = universe.len();
        let ready_fraction = if total > 0 {
            ready_count as f64 / total as f64
        } else {
            1.0
        };

        let not_ready: Vec<String> = universe
            .iter()
            .filter(|token_id| {
                !books
                    .get(*token_id)
                    .map(|s| s.is_ready.load(Ordering::Acquire))
                    .unwrap_or(false)
            })
            .cloned()
            .collect();

        WarmupStatus {
            ready_count,
            total_count: total,
            ready_fraction,
            not_ready_tokens: not_ready,
            is_warm: ready_fraction >= self.config.warmup_min_ready_fraction,
        }
    }

    /// Get metrics snapshot
    pub fn metrics(&self) -> &Arc<BookStoreMetrics> {
        &self.metrics
    }

    /// Internal: ensure token state exists (called by SubscriptionManager)
    pub(crate) fn ensure_token(&self, token_id: &str) -> Arc<TokenBookState> {
        // Fast path: check if exists
        {
            let books = self.books.read();
            if let Some(state) = books.get(token_id) {
                return Arc::clone(state);
            }
        }

        // Slow path: insert
        let mut books = self.books.write();
        books
            .entry(token_id.to_string())
            .or_insert_with(|| Arc::new(TokenBookState::new()))
            .clone()
    }

    /// Internal: apply a full snapshot (called by WS consumer)
    pub(crate) fn apply_snapshot(
        &self,
        token_id: &str,
        bids: Vec<PriceLevel>,
        asks: Vec<PriceLevel>,
        sequence: Option<u64>,
    ) {
        let state = self.ensure_token(token_id);

        let now = Instant::now();
        let now_ns = instant_to_nanos(now);

        let snapshot = BookSnapshot {
            bids,
            asks,
            sequence,
            created_at: now,
        };

        // Validate not crossed
        if snapshot.is_crossed() {
            warn!(
                token_id = token_id,
                best_bid = ?snapshot.best_bid(),
                best_ask = ?snapshot.best_ask(),
                "Received crossed book snapshot, marking not ready"
            );
            state.is_ready.store(false, Ordering::Release);
            state.reset_count.fetch_add(1, Ordering::Relaxed);
            self.metrics
                .snapshot_rejects
                .fetch_add(1, Ordering::Relaxed);
            return;
        }

        // Atomic swap
        state.book.store(Arc::new(snapshot));
        state.last_update_ns.store(now_ns, Ordering::Release);
        if let Some(seq) = sequence {
            state.last_sequence.store(seq, Ordering::Release);
        }
        state.is_ready.store(true, Ordering::Release);
        state.update_count.fetch_add(1, Ordering::Relaxed);

        // Notify watchers
        let _ = state.update_tx.send(now);

        self.metrics
            .snapshots_applied
            .fetch_add(1, Ordering::Relaxed);

        trace!(
            token_id = token_id,
            sequence = ?sequence,
            bids = state.book.load().bids.len(),
            asks = state.book.load().asks.len(),
            "Applied book snapshot"
        );
    }

    /// Internal: apply a batch delta update (called by WS consumer)
    pub(crate) fn apply_delta_batch(
        &self,
        token_id: &str,
        bid_updates: &[(f64, f64)], // (price, new_size)
        ask_updates: &[(f64, f64)],
        sequence: Option<u64>,
    ) -> bool {
        let books = self.books.read();
        let Some(state) = books.get(token_id) else {
            return false;
        };

        // Must have snapshot first
        if !state.is_ready.load(Ordering::Acquire) {
            debug!(
                token_id = token_id,
                "Delta received before snapshot, ignoring"
            );
            self.metrics
                .deltas_before_snapshot
                .fetch_add(1, Ordering::Relaxed);
            return false;
        }

        // Sequence validation
        if self.config.enable_sequence_validation {
            if let Some(new_seq) = sequence {
                let last_seq = state.last_sequence.load(Ordering::Acquire);
                if last_seq > 0 && new_seq != last_seq + 1 {
                    warn!(
                        token_id = token_id,
                        expected = last_seq + 1,
                        got = new_seq,
                        "Sequence gap detected, triggering reset"
                    );
                    state.is_ready.store(false, Ordering::Release);
                    state.sequence_gaps.fetch_add(1, Ordering::Relaxed);
                    self.metrics.sequence_gaps.fetch_add(1, Ordering::Relaxed);
                    return false;
                }
            }
        }

        // Load current book and apply updates
        let current = state.book.load();
        let mut new_bids = current.bids.clone();
        let mut new_asks = current.asks.clone();

        // Apply bid updates
        for &(price, size) in bid_updates {
            apply_level_update(&mut new_bids, price, size, true);
        }

        // Apply ask updates
        for &(price, size) in ask_updates {
            apply_level_update(&mut new_asks, price, size, false);
        }

        // Truncate to max depth
        if self.config.max_book_depth > 0 {
            new_bids.truncate(self.config.max_book_depth);
            new_asks.truncate(self.config.max_book_depth);
        }

        let now = Instant::now();
        let now_ns = instant_to_nanos(now);

        let snapshot = BookSnapshot {
            bids: new_bids,
            asks: new_asks,
            sequence,
            created_at: now,
        };

        // Validate not crossed
        if snapshot.is_crossed() {
            warn!(
                token_id = token_id,
                best_bid = ?snapshot.best_bid(),
                best_ask = ?snapshot.best_ask(),
                "Delta resulted in crossed book, triggering reset"
            );
            state.is_ready.store(false, Ordering::Release);
            state.reset_count.fetch_add(1, Ordering::Relaxed);
            self.metrics
                .crossed_book_resets
                .fetch_add(1, Ordering::Relaxed);
            return false;
        }

        // Atomic swap
        state.book.store(Arc::new(snapshot));
        state.last_update_ns.store(now_ns, Ordering::Release);
        if let Some(seq) = sequence {
            state.last_sequence.store(seq, Ordering::Release);
        }
        state.update_count.fetch_add(1, Ordering::Relaxed);

        // Notify watchers
        let _ = state.update_tx.send(now);

        self.metrics.deltas_applied.fetch_add(1, Ordering::Relaxed);

        true
    }

    /// Internal: apply a single-level delta from price_change message (called by WS consumer)
    /// 
    /// This is the primary entry point for `price_change` messages.
    /// - side: "BUY" for bids, "SELL" for asks
    /// - price: the price level affected
    /// - new_size: the NEW aggregate size at this level (0 = remove)
    pub(crate) fn apply_delta(&self, token_id: &str, side: &str, price: f64, new_size: f64) -> bool {
        let books = self.books.read();
        let Some(state) = books.get(token_id) else {
            return false;
        };

        // Must have snapshot first
        if !state.is_ready.load(Ordering::Acquire) {
            debug!(
                token_id = token_id,
                "Delta received before snapshot, ignoring"
            );
            self.metrics
                .deltas_before_snapshot
                .fetch_add(1, Ordering::Relaxed);
            return false;
        }

        // Determine side
        let is_bid = side.to_uppercase() == "BUY";

        // Load current book and apply update
        let current = state.book.load();
        let mut new_bids = current.bids.clone();
        let mut new_asks = current.asks.clone();

        if is_bid {
            apply_level_update(&mut new_bids, price, new_size, true);
        } else {
            apply_level_update(&mut new_asks, price, new_size, false);
        }

        // Truncate to max depth
        if self.config.max_book_depth > 0 {
            new_bids.truncate(self.config.max_book_depth);
            new_asks.truncate(self.config.max_book_depth);
        }

        let now = Instant::now();
        let now_ns = instant_to_nanos(now);

        let snapshot = BookSnapshot {
            bids: new_bids,
            asks: new_asks,
            sequence: current.sequence, // Preserve existing sequence
            created_at: now,
        };

        // Validate not crossed
        if snapshot.is_crossed() {
            warn!(
                token_id = token_id,
                best_bid = ?snapshot.best_bid(),
                best_ask = ?snapshot.best_ask(),
                "Single delta resulted in crossed book, ignoring"
            );
            self.metrics
                .crossed_book_resets
                .fetch_add(1, Ordering::Relaxed);
            return false;
        }

        // Atomic swap
        state.book.store(Arc::new(snapshot));
        state.last_update_ns.store(now_ns, Ordering::Release);
        state.update_count.fetch_add(1, Ordering::Relaxed);

        // Notify watchers
        let _ = state.update_tx.send(now);

        self.metrics.deltas_applied.fetch_add(1, Ordering::Relaxed);

        true
    }

    /// Internal: mark token as not ready (triggers resubscription)
    pub(crate) fn mark_not_ready(&self, token_id: &str) {
        let books = self.books.read();
        if let Some(state) = books.get(token_id) {
            state.is_ready.store(false, Ordering::Release);
        }
    }

    /// Helper to compute age in ms using monotonic time
    #[inline]
    fn age_ms_of(&self, state: &TokenBookState) -> u64 {
        let last_ns = state.last_update_ns.load(Ordering::Acquire);
        if last_ns == 0 {
            // `instant_to_nanos()` uses a process-local epoch set on first call, which can
            // legitimately yield 0ns for the first snapshot. If the book is marked ready,
            // treat this as "fresh" rather than "never seen".
            return if state.is_ready.load(Ordering::Acquire) {
                0
            } else {
                u64::MAX
            };
        }
        let now_ns = instant_to_nanos(Instant::now());
        if now_ns > last_ns {
            (now_ns - last_ns) / 1_000_000
        } else {
            0
        }
    }
}

/// Apply a price level update to a sorted book side
fn apply_level_update(levels: &mut Vec<PriceLevel>, price: f64, size: f64, is_bid: bool) {
    // Find position
    let pos = if is_bid {
        // Bids sorted descending
        levels.iter().position(|l| l.price <= price)
    } else {
        // Asks sorted ascending
        levels.iter().position(|l| l.price >= price)
    };

    match pos {
        Some(i) if levels[i].price == price => {
            if size <= 0.0 {
                // Remove level
                levels.remove(i);
            } else {
                // Update size
                levels[i].size = size;
            }
        }
        Some(i) if size > 0.0 => {
            // Insert new level
            levels.insert(i, PriceLevel { price, size });
        }
        None if size > 0.0 => {
            // Append at end
            levels.push(PriceLevel { price, size });
        }
        _ => {}
    }
}

// ============================================================================
// Warmup Status
// ============================================================================

#[derive(Debug, Clone)]
pub struct WarmupStatus {
    pub ready_count: usize,
    pub total_count: usize,
    pub ready_fraction: f64,
    pub not_ready_tokens: Vec<String>,
    pub is_warm: bool,
}

// ============================================================================
// SubscriptionManager - Eager Subscription Management
// ============================================================================

/// Commands for the subscription manager
#[derive(Debug)]
enum SubCommand {
    /// Set the target universe (diff and subscribe/unsubscribe)
    SetUniverse(Vec<String>),
    /// Ensure a single token is subscribed (safety net)
    EnsureSubscribed(String),
    /// Force resubscribe for a token (e.g., after detecting staleness)
    Resubscribe(String),
    /// Shutdown
    Shutdown,
}

/// Events emitted by the subscription manager
#[derive(Debug, Clone)]
pub enum SubEvent {
    /// Connection established
    Connected,
    /// Connection lost
    Disconnected { reason: String },
    /// Token became ready
    TokenReady { token_id: String },
    /// Token became stale
    TokenStale { token_id: String },
    /// Health check result
    HealthCheck {
        ready_count: usize,
        stale_count: usize,
    },
}

/// Manages WebSocket subscriptions for Polymarket orderbooks
pub struct SubscriptionManager {
    config: BookStoreConfig,
    book_store: Arc<BookStore>,
    cmd_tx: mpsc::Sender<SubCommand>,
    event_rx: Mutex<Option<mpsc::Receiver<SubEvent>>>,
    /// Current target universe
    universe: RwLock<HashSet<String>>,
    /// Connection state
    is_connected: AtomicBool,
    /// Shutdown flag
    shutdown: AtomicBool,
    /// Metrics
    metrics: Arc<SubscriptionMetrics>,
    /// Optional snapshot recorder for historical persistence
    recorder: Option<AsyncBookRecorder>,
    /// Local sequence counter for recorded snapshots
    recorder_seq: AtomicU64,
    /// Optional trade print recorder for historical persistence
    trade_recorder: Option<AsyncTradeRecorder>,
    /// Local sequence counter for recorded trades
    trade_recorder_seq: AtomicU64,
    /// Optional delta recorder for historical persistence (from price_change messages)
    delta_recorder: Option<AsyncDeltaRecorder>,
    /// Per-token sequence counters for recorded deltas (using Mutex<HashMap>)
    delta_recorder_seq: Mutex<std::collections::HashMap<String, AtomicU64>>,
}

impl SubscriptionManager {
    /// Create and spawn the subscription manager
    pub fn spawn(config: BookStoreConfig, book_store: Arc<BookStore>) -> Arc<Self> {
        let (cmd_tx, cmd_rx) = mpsc::channel::<SubCommand>(1024);
        let (event_tx, event_rx) = mpsc::channel::<SubEvent>(256);

        // Initialize optional snapshot recorder for backtesting persistence
        let recorder = config.record_snapshots_db_path.as_ref().and_then(|db_path| {
            match BookSnapshotStorage::open(db_path) {
                Ok(storage) => {
                    info!(path = %db_path, "Historical snapshot recording enabled");
                    Some(AsyncBookRecorder::spawn(Arc::new(storage), 1000))
                }
                Err(e) => {
                    error!(error = %e, path = %db_path, "Failed to open snapshot database, recording disabled");
                    None
                }
            }
        });

        // Initialize optional trade recorder for backtesting persistence
        let trade_recorder = config.record_trades_db_path.as_ref().and_then(|db_path| {
            match TradePrintStorage::open(db_path) {
                Ok(storage) => {
                    info!(path = %db_path, "Historical trade recording enabled");
                    Some(AsyncTradeRecorder::spawn(Arc::new(storage), 1000))
                }
                Err(e) => {
                    error!(error = %e, path = %db_path, "Failed to open trade database, recording disabled");
                    None
                }
            }
        });

        // Initialize optional delta recorder for backtesting persistence (price_change messages)
        // CRITICAL: This enables MAKER VIABILITY in backtesting
        let delta_recorder = config.record_deltas_db_path.as_ref().and_then(|db_path| {
            match BookDeltaStorage::open(db_path) {
                Ok(storage) => {
                    info!(path = %db_path, "Historical delta recording enabled (MAKER VIABILITY)");
                    Some(AsyncDeltaRecorder::spawn(Arc::new(storage), 500))
                }
                Err(e) => {
                    error!(error = %e, path = %db_path, "Failed to open delta database, recording disabled");
                    None
                }
            }
        });

        let manager = Arc::new(Self {
            config: config.clone(),
            book_store: book_store.clone(),
            cmd_tx,
            event_rx: Mutex::new(Some(event_rx)),
            universe: RwLock::new(HashSet::new()),
            is_connected: AtomicBool::new(false),
            shutdown: AtomicBool::new(false),
            metrics: Arc::new(SubscriptionMetrics::new()),
            recorder,
            recorder_seq: AtomicU64::new(0),
            trade_recorder,
            trade_recorder_seq: AtomicU64::new(0),
            delta_recorder,
            delta_recorder_seq: Mutex::new(std::collections::HashMap::new()),
        });

        // Spawn main WS connection task
        let manager_clone = Arc::clone(&manager);
        let config_clone = config.clone();
        let book_store_clone = book_store.clone();
        tokio::spawn(async move {
            manager_clone
                .run_ws_loop(cmd_rx, event_tx, config_clone, book_store_clone)
                .await;
        });

        // Spawn health check task
        let manager_health = Arc::clone(&manager);
        tokio::spawn(async move {
            manager_health.run_health_loop().await;
        });

        manager
    }

    /// Set the target universe of tokens to subscribe
    pub async fn set_universe(&self, tokens: Vec<String>) {
        // Update local state
        {
            let mut universe = self.universe.write();
            universe.clear();
            for t in &tokens {
                universe.insert(t.clone());
            }
        }

        // Send command to WS task
        let _ = self.cmd_tx.send(SubCommand::SetUniverse(tokens)).await;
    }

    /// Ensure a token is subscribed (non-blocking, for safety)
    pub fn ensure_subscribed(&self, token_id: &str) {
        let token = token_id.to_string();

        // Add to universe
        {
            let mut universe = self.universe.write();
            universe.insert(token.clone());
        }

        // Ensure book state exists
        self.book_store.ensure_token(token_id);

        // Send command (non-blocking)
        let _ = self.cmd_tx.try_send(SubCommand::EnsureSubscribed(token));
    }

    /// Request resubscription for a stale token
    pub fn request_resubscribe(&self, token_id: &str) {
        let _ = self
            .cmd_tx
            .try_send(SubCommand::Resubscribe(token_id.to_string()));
    }

    /// Take the event receiver (can only be called once)
    pub fn take_event_receiver(&self) -> Option<mpsc::Receiver<SubEvent>> {
        self.event_rx.lock().take()
    }

    /// Check if connected
    pub fn is_connected(&self) -> bool {
        self.is_connected.load(Ordering::Acquire)
    }

    /// Get metrics
    pub fn metrics(&self) -> &Arc<SubscriptionMetrics> {
        &self.metrics
    }

    /// Shutdown the manager
    pub async fn shutdown(&self) {
        self.shutdown.store(true, Ordering::Release);
        let _ = self.cmd_tx.send(SubCommand::Shutdown).await;
    }

    /// Main WebSocket loop
    async fn run_ws_loop(
        &self,
        mut cmd_rx: mpsc::Receiver<SubCommand>,
        event_tx: mpsc::Sender<SubEvent>,
        config: BookStoreConfig,
        book_store: Arc<BookStore>,
    ) {
        let mut reconnect_delay = Duration::from_millis(config.reconnect_base_delay_ms);
        let max_reconnect_delay = Duration::from_millis(config.reconnect_max_delay_ms);
        let mut subscribed_tokens: HashSet<String> = HashSet::new();

        loop {
            if self.shutdown.load(Ordering::Acquire) {
                info!("SubscriptionManager shutting down");
                return;
            }

            // Collect pending commands before connecting
            let mut pending_universe: Option<Vec<String>> = None;
            let mut pending_ensures: Vec<String> = Vec::new();

            // Drain any pending commands
            while let Ok(cmd) = cmd_rx.try_recv() {
                match cmd {
                    SubCommand::SetUniverse(tokens) => pending_universe = Some(tokens),
                    SubCommand::EnsureSubscribed(token) => pending_ensures.push(token),
                    SubCommand::Resubscribe(token) => pending_ensures.push(token),
                    SubCommand::Shutdown => return,
                }
            }

            // Connect
            info!(url = %config.ws_url, "Connecting to Polymarket market WS");

            match self
                .connect_and_stream(
                    &mut cmd_rx,
                    &event_tx,
                    &config,
                    &book_store,
                    &mut subscribed_tokens,
                    pending_universe,
                    pending_ensures,
                )
                .await
            {
                Ok(_) => {
                    reconnect_delay = Duration::from_millis(config.reconnect_base_delay_ms);
                }
                Err(e) => {
                    self.is_connected.store(false, Ordering::Release);
                    let _ = event_tx
                        .send(SubEvent::Disconnected {
                            reason: e.to_string(),
                        })
                        .await;

                    warn!(
                        error = %e,
                        delay_ms = reconnect_delay.as_millis(),
                        "Polymarket WS disconnected, reconnecting"
                    );

                    self.metrics.reconnects.fetch_add(1, Ordering::Relaxed);

                    sleep(reconnect_delay).await;
                    reconnect_delay = (reconnect_delay * 2).min(max_reconnect_delay);

                    // Clear subscribed set on disconnect (will resubscribe on reconnect)
                    subscribed_tokens.clear();
                }
            }
        }
    }

    async fn connect_and_stream(
        &self,
        cmd_rx: &mut mpsc::Receiver<SubCommand>,
        event_tx: &mpsc::Sender<SubEvent>,
        config: &BookStoreConfig,
        book_store: &Arc<BookStore>,
        subscribed_tokens: &mut HashSet<String>,
        initial_universe: Option<Vec<String>>,
        initial_ensures: Vec<String>,
    ) -> Result<()> {
        let (ws_stream, resp) = connect_async(&config.ws_url)
            .await
            .context("Failed to connect to Polymarket market WS")?;

        info!(
            status = %resp.status(),
            "Connected to Polymarket market WS"
        );

        self.is_connected.store(true, Ordering::Release);
        let _ = event_tx.send(SubEvent::Connected).await;

        let (mut write, mut read) = ws_stream.split();

        // Subscribe to initial universe
        let mut target_universe: HashSet<String> = self.universe.read().clone();

        if let Some(tokens) = initial_universe {
            target_universe = tokens.into_iter().collect();
        }
        for token in initial_ensures {
            target_universe.insert(token);
        }

        // Subscribe to all tokens in universe
        if !target_universe.is_empty() {
            let tokens_to_sub: Vec<_> = target_universe
                .iter()
                .filter(|t| !subscribed_tokens.contains(*t))
                .cloned()
                .collect();

            if !tokens_to_sub.is_empty() {
                let sub_msg = serde_json::json!({
                    "type": "market",
                    "assets_ids": tokens_to_sub,
                });
                write
                    .send(Message::Text(sub_msg.to_string()))
                    .await
                    .context("Failed to send initial subscription")?;

                for t in &tokens_to_sub {
                    subscribed_tokens.insert(t.clone());
                    book_store.ensure_token(t);
                }

                self.metrics
                    .subscriptions
                    .fetch_add(tokens_to_sub.len() as u64, Ordering::Relaxed);

                info!(
                    count = tokens_to_sub.len(),
                    "Sent initial market subscriptions"
                );
            }
        }

        let mut ping_interval = interval(Duration::from_millis(config.ping_interval_ms));
        ping_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                // Ping/keepalive
                _ = ping_interval.tick() => {
                    write.send(Message::Text("PING".to_string())).await
                        .context("Failed to send ping")?;
                }

                // Commands
                cmd = cmd_rx.recv() => {
                    let Some(cmd) = cmd else {
                        return Ok(());
                    };

                    match cmd {
                        SubCommand::SetUniverse(tokens) => {
                            let new_universe: HashSet<String> = tokens.into_iter().collect();

                            // Unsubscribe from removed tokens
                            let to_unsub: Vec<_> = subscribed_tokens.iter()
                                .filter(|t| !new_universe.contains(*t))
                                .cloned()
                                .collect();

                            if !to_unsub.is_empty() {
                                // Polymarket doesn't have explicit unsubscribe, but we track locally
                                for t in &to_unsub {
                                    subscribed_tokens.remove(t);
                                }
                                debug!(count = to_unsub.len(), "Removed tokens from subscription set");
                            }

                            // Subscribe to new tokens
                            let to_sub: Vec<_> = new_universe.iter()
                                .filter(|t| !subscribed_tokens.contains(*t))
                                .cloned()
                                .collect();

                            if !to_sub.is_empty() {
                                let sub_msg = serde_json::json!({
                                    "assets_ids": &to_sub,
                                    "operation": "subscribe",
                                });
                                write.send(Message::Text(sub_msg.to_string())).await
                                    .context("Failed to send subscription")?;

                                for t in &to_sub {
                                    subscribed_tokens.insert(t.clone());
                                    book_store.ensure_token(t);
                                }

                                self.metrics.subscriptions.fetch_add(to_sub.len() as u64, Ordering::Relaxed);

                                debug!(count = to_sub.len(), "Subscribed to new tokens");
                            }

                            target_universe = new_universe;
                        }

                        SubCommand::EnsureSubscribed(token) => {
                            if !subscribed_tokens.contains(&token) {
                                let sub_msg = serde_json::json!({
                                    "assets_ids": [&token],
                                    "operation": "subscribe",
                                });
                                write.send(Message::Text(sub_msg.to_string())).await
                                    .context("Failed to send subscription")?;

                                subscribed_tokens.insert(token.clone());
                                book_store.ensure_token(&token);

                                self.metrics.subscriptions.fetch_add(1, Ordering::Relaxed);

                                trace!(token_id = &token, "Ensured token subscription");
                            }
                        }

                        SubCommand::Resubscribe(token) => {
                            // Mark not ready to trigger fresh snapshot
                            book_store.mark_not_ready(&token);

                            // Resubscribe
                            let sub_msg = serde_json::json!({
                                "assets_ids": [&token],
                                "operation": "subscribe",
                            });
                            write.send(Message::Text(sub_msg.to_string())).await
                                .context("Failed to send resubscription")?;

                            subscribed_tokens.insert(token.clone());
                            self.metrics.resubscriptions.fetch_add(1, Ordering::Relaxed);

                            debug!(token_id = &token, "Resubscribed to token");
                        }

                        SubCommand::Shutdown => {
                            return Ok(());
                        }
                    }
                }

                // Incoming messages
                msg = read.next() => {
                    // Capture arrival time IMMEDIATELY - BEFORE any JSON parsing
                    let arrival_time_ns = now_ns();

                    let Some(msg) = msg else {
                        return Err(anyhow::anyhow!("WebSocket stream ended"));
                    };

                    match msg {
                        Ok(Message::Text(text)) => {
                            self.handle_message(&text, book_store, event_tx, arrival_time_ns).await;
                        }
                        Ok(Message::Ping(data)) => {
                            write.send(Message::Pong(data)).await?;
                        }
                        Ok(Message::Close(frame)) => {
                            debug!(?frame, "WebSocket close frame received");
                            return Err(anyhow::anyhow!("WebSocket closed by server"));
                        }
                        Ok(_) => {}
                        Err(e) => {
                            return Err(anyhow::anyhow!("WebSocket error: {}", e));
                        }
                    }
                }
            }
        }
    }

    async fn handle_message(
        &self,
        text: &str,
        book_store: &Arc<BookStore>,
        event_tx: &mpsc::Sender<SubEvent>,
        arrival_time_ns: u64,
    ) {
        // Ignore PONG
        if text.eq_ignore_ascii_case("PONG") {
            return;
        }

        let json: serde_json::Value = match serde_json::from_str(text) {
            Ok(v) => v,
            Err(_) => return,
        };

        let event_type = json
            .get("event_type")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        match event_type {
            "book" => {
                // Full book snapshot
                let msg: WsBookMsg = match serde_json::from_value(json) {
                    Ok(m) => m,
                    Err(e) => {
                        debug!(error = %e, "Failed to parse book message");
                        return;
                    }
                };

                let bids: Vec<PriceLevel> = msg
                    .bids
                    .iter()
                    .map(|o| PriceLevel {
                        price: o.price,
                        size: o.size,
                    })
                    .collect();
                let asks: Vec<PriceLevel> = msg
                    .asks
                    .iter()
                    .map(|o| PriceLevel {
                        price: o.price,
                        size: o.size,
                    })
                    .collect();

                let sequence = msg.hash.and_then(|h| h.parse::<u64>().ok());

                // Parse exchange source timestamp (ISO string to nanoseconds)
                let source_time_ns = msg.timestamp.as_ref().and_then(|ts| {
                    chrono::DateTime::parse_from_rfc3339(ts)
                        .ok()
                        .map(|dt| dt.timestamp_nanos_opt().unwrap_or(0) as u64)
                });

                // Record snapshot for historical persistence (if enabled)
                if let Some(ref recorder) = self.recorder {
                    let local_seq = self.recorder_seq.fetch_add(1, Ordering::Relaxed);
                    let recorded_bids: Vec<RecordedPriceLevel> = msg
                        .bids
                        .iter()
                        .map(|o| RecordedPriceLevel {
                            price: o.price,
                            size: o.size,
                        })
                        .collect();
                    let recorded_asks: Vec<RecordedPriceLevel> = msg
                        .asks
                        .iter()
                        .map(|o| RecordedPriceLevel {
                            price: o.price,
                            size: o.size,
                        })
                        .collect();

                    let snapshot = RecordedBookSnapshot::from_ws_message(
                        msg.asset_id.clone(),
                        recorded_bids,
                        recorded_asks,
                        sequence,
                        source_time_ns,
                        arrival_time_ns,
                        local_seq,
                    );

                    // Non-blocking send to recorder
                    recorder.record(snapshot);
                }

                let was_ready = book_store.is_ready(&msg.asset_id);
                book_store.apply_snapshot(&msg.asset_id, bids, asks, sequence);

                if !was_ready && book_store.is_ready(&msg.asset_id) {
                    let _ = event_tx
                        .send(SubEvent::TokenReady {
                            token_id: msg.asset_id,
                        })
                        .await;
                }

                self.metrics
                    .messages_received
                    .fetch_add(1, Ordering::Relaxed);
            }
            "price_change" => {
                // L2 Book Delta - incremental update to a single price level
                // CRITICAL: This enables MAKER VIABILITY in backtesting
                //
                // Message schema:
                // {
                //   "event_type": "price_change",
                //   "market": "0x...",
                //   "timestamp": "1757908892351",
                //   "price_changes": [{
                //     "asset_id": "...",
                //     "price": "0.5",
                //     "size": "200",      // NEW aggregate size at level
                //     "side": "BUY",
                //     "hash": "56621a...",
                //     "best_bid": "0.5",
                //     "best_ask": "1"
                //   }]
                // }
                
                let market_id = json
                    .get("market")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let ws_timestamp_ms: u64 = json
                    .get("timestamp")
                    .and_then(|v| v.as_str())
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0);
                
                // Process each price change in the array
                if let Some(price_changes) = json.get("price_changes").and_then(|v| v.as_array()) {
                    for pc in price_changes {
                        let asset_id = pc.get("asset_id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        let price_str = pc.get("price")
                            .and_then(|v| v.as_str())
                            .unwrap_or("0");
                        let size_str = pc.get("size")
                            .and_then(|v| v.as_str())
                            .unwrap_or("0");
                        let side = pc.get("side")
                            .and_then(|v| v.as_str())
                            .unwrap_or("BUY");
                        let seq_hash = pc.get("hash")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let best_bid: Option<f64> = pc.get("best_bid")
                            .and_then(|v| v.as_str())
                            .and_then(|s| s.parse().ok());
                        let best_ask: Option<f64> = pc.get("best_ask")
                            .and_then(|v| v.as_str())
                            .and_then(|s| s.parse().ok());
                        
                        let price: f64 = price_str.parse().unwrap_or(0.0);
                        let new_size: f64 = size_str.parse().unwrap_or(0.0);
                        
                        // Skip invalid deltas
                        if asset_id.is_empty() || price <= 0.0 {
                            continue;
                        }
                        
                        // Apply delta to in-memory book state
                        book_store.apply_delta(asset_id, side, price, new_size);
                        
                        // Record delta for historical persistence (if enabled)
                        if let Some(ref delta_recorder) = self.delta_recorder {
                            // Get per-token sequence counter
                            let local_seq = {
                                let mut seq_map = self.delta_recorder_seq.lock();
                                let counter = seq_map.entry(asset_id.to_string())
                                    .or_insert_with(|| AtomicU64::new(0));
                                counter.fetch_add(1, Ordering::Relaxed)
                            };
                            
                            let delta = L2BookDeltaRecord::from_price_change(
                                market_id.clone(),
                                asset_id.to_string(),
                                side,
                                price,
                                new_size,
                                ws_timestamp_ms,
                                arrival_time_ns,
                                local_seq,
                                seq_hash,
                                best_bid,
                                best_ask,
                            );
                            
                            // Non-blocking send to recorder
                            delta_recorder.record(delta);
                        }
                    }
                }
                
                self.metrics
                    .messages_received
                    .fetch_add(1, Ordering::Relaxed);
            }
            "last_trade_price" => {
                // Public trade print - a maker and taker order matched
                // This is critical for queue modeling in backtesting
                let asset_id = json
                    .get("asset_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let market_id = json
                    .get("market")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let price: f64 = json
                    .get("price")
                    .and_then(|v| v.as_str())
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0.0);
                let size: f64 = json
                    .get("size")
                    .and_then(|v| v.as_str())
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0.0);
                let side = json
                    .get("side")
                    .and_then(|v| v.as_str())
                    .unwrap_or("BUY");
                let fee_rate_bps: Option<i32> = json
                    .get("fee_rate_bps")
                    .and_then(|v| v.as_str())
                    .and_then(|s| s.parse().ok());
                let source_time_ms: u64 = json
                    .get("timestamp")
                    .and_then(|v| v.as_str())
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0);

                // Record trade for historical persistence (if enabled)
                if let Some(ref trade_recorder) = self.trade_recorder {
                    if !asset_id.is_empty() && price > 0.0 && size > 0.0 {
                        let local_seq = self.trade_recorder_seq.fetch_add(1, Ordering::Relaxed);
                        let trade = RecordedTradePrint::from_ws_trade(
                            asset_id.to_string(),
                            market_id.to_string(),
                            price,
                            size,
                            side,
                            fee_rate_bps,
                            source_time_ms,
                            arrival_time_ns,
                            local_seq,
                        );
                        // Non-blocking send to recorder
                        trade_recorder.record(trade);
                    }
                }

                self.metrics
                    .messages_received
                    .fetch_add(1, Ordering::Relaxed);
            }
            _ => {
                // Ignore other message types
            }
        }
    }

    /// Health check loop
    async fn run_health_loop(&self) {
        let mut interval = interval(Duration::from_millis(self.config.health_check_interval_ms));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            interval.tick().await;

            if self.shutdown.load(Ordering::Acquire) {
                return;
            }

            // Check for stale tokens
            let stale_tokens = self.book_store.get_stale_tokens();

            for token_id in stale_tokens {
                debug!(
                    token_id = &token_id,
                    "Token is stale, requesting resubscribe"
                );
                self.request_resubscribe(&token_id);
            }

            // Update metrics
            let ready_tokens = self.book_store.get_ready_tokens();
            self.metrics
                .ready_token_count
                .store(ready_tokens.len() as u64, Ordering::Relaxed);
        }
    }
}

// ============================================================================
// WS Message Types
// ============================================================================

#[derive(Debug, Deserialize)]
struct WsBookMsg {
    pub event_type: String,
    #[serde(rename = "asset_id")]
    pub asset_id: String,
    #[serde(default)]
    pub bids: Vec<Order>,
    #[serde(default)]
    pub asks: Vec<Order>,
    #[serde(default)]
    pub hash: Option<String>,
    #[serde(default)]
    pub timestamp: Option<String>,
}

// ============================================================================
// Metrics
// ============================================================================

/// Metrics for the BookStore
#[derive(Debug)]
pub struct BookStoreMetrics {
    pub cache_hits: AtomicU64,
    pub cache_misses_not_subscribed: AtomicU64,
    pub cache_misses_not_ready: AtomicU64,
    pub cache_misses_stale: AtomicU64,
    pub cache_misses_never_seen: AtomicU64,
    pub cache_misses_crossed: AtomicU64,
    pub snapshots_applied: AtomicU64,
    pub snapshot_rejects: AtomicU64,
    pub deltas_applied: AtomicU64,
    pub deltas_before_snapshot: AtomicU64,
    pub sequence_gaps: AtomicU64,
    pub crossed_book_resets: AtomicU64,
    /// Running sum of served book ages (for mean calculation)
    served_age_sum_ms: AtomicU64,
    served_age_count: AtomicU64,
    /// Histogram buckets for age distribution (0-10ms, 10-50ms, 50-100ms, etc.)
    age_histogram: [AtomicU64; 8],
}

impl BookStoreMetrics {
    fn new() -> Self {
        Self {
            cache_hits: AtomicU64::new(0),
            cache_misses_not_subscribed: AtomicU64::new(0),
            cache_misses_not_ready: AtomicU64::new(0),
            cache_misses_stale: AtomicU64::new(0),
            cache_misses_never_seen: AtomicU64::new(0),
            cache_misses_crossed: AtomicU64::new(0),
            snapshots_applied: AtomicU64::new(0),
            snapshot_rejects: AtomicU64::new(0),
            deltas_applied: AtomicU64::new(0),
            deltas_before_snapshot: AtomicU64::new(0),
            sequence_gaps: AtomicU64::new(0),
            crossed_book_resets: AtomicU64::new(0),
            served_age_sum_ms: AtomicU64::new(0),
            served_age_count: AtomicU64::new(0),
            age_histogram: Default::default(),
        }
    }

    fn record_served_age(&self, age_ms: u64) {
        self.served_age_sum_ms.fetch_add(age_ms, Ordering::Relaxed);
        self.served_age_count.fetch_add(1, Ordering::Relaxed);

        // Histogram buckets: 0-10, 10-50, 50-100, 100-200, 200-500, 500-1000, 1000-2000, 2000+
        let bucket = match age_ms {
            0..=10 => 0,
            11..=50 => 1,
            51..=100 => 2,
            101..=200 => 3,
            201..=500 => 4,
            501..=1000 => 5,
            1001..=2000 => 6,
            _ => 7,
        };
        self.age_histogram[bucket].fetch_add(1, Ordering::Relaxed);
    }

    /// Get cache hit rate
    pub fn hit_rate(&self) -> f64 {
        let hits = self.cache_hits.load(Ordering::Relaxed);
        let misses = self.total_misses();
        let total = hits + misses;
        if total == 0 {
            1.0
        } else {
            hits as f64 / total as f64
        }
    }

    /// Get total cache misses
    pub fn total_misses(&self) -> u64 {
        self.cache_misses_not_subscribed.load(Ordering::Relaxed)
            + self.cache_misses_not_ready.load(Ordering::Relaxed)
            + self.cache_misses_stale.load(Ordering::Relaxed)
            + self.cache_misses_never_seen.load(Ordering::Relaxed)
            + self.cache_misses_crossed.load(Ordering::Relaxed)
    }

    /// Get mean served age in ms
    pub fn mean_served_age_ms(&self) -> f64 {
        let sum = self.served_age_sum_ms.load(Ordering::Relaxed);
        let count = self.served_age_count.load(Ordering::Relaxed);
        if count == 0 {
            0.0
        } else {
            sum as f64 / count as f64
        }
    }

    /// Get summary for logging
    pub fn summary(&self) -> BookStoreMetricsSummary {
        BookStoreMetricsSummary {
            cache_hits: self.cache_hits.load(Ordering::Relaxed),
            cache_misses: self.total_misses(),
            hit_rate: self.hit_rate(),
            mean_age_ms: self.mean_served_age_ms(),
            snapshots_applied: self.snapshots_applied.load(Ordering::Relaxed),
            sequence_gaps: self.sequence_gaps.load(Ordering::Relaxed),
            crossed_resets: self.crossed_book_resets.load(Ordering::Relaxed),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct BookStoreMetricsSummary {
    pub cache_hits: u64,
    pub cache_misses: u64,
    pub hit_rate: f64,
    pub mean_age_ms: f64,
    pub snapshots_applied: u64,
    pub sequence_gaps: u64,
    pub crossed_resets: u64,
}

/// Metrics for the SubscriptionManager
#[derive(Debug)]
pub struct SubscriptionMetrics {
    pub subscriptions: AtomicU64,
    pub resubscriptions: AtomicU64,
    pub reconnects: AtomicU64,
    pub messages_received: AtomicU64,
    pub ready_token_count: AtomicU64,
}

impl SubscriptionMetrics {
    fn new() -> Self {
        Self {
            subscriptions: AtomicU64::new(0),
            resubscriptions: AtomicU64::new(0),
            reconnects: AtomicU64::new(0),
            messages_received: AtomicU64::new(0),
            ready_token_count: AtomicU64::new(0),
        }
    }
}

// ============================================================================
// Warmup Manager
// ============================================================================

/// Manages the warmup phase before trading
pub struct WarmupManager {
    config: BookStoreConfig,
    book_store: Arc<BookStore>,
    subscription_manager: Arc<SubscriptionManager>,
    /// Whether warmup is complete
    is_warm: AtomicBool,
    /// Tokens that failed to warm (disabled for session)
    disabled_tokens: RwLock<HashSet<String>>,
}

impl WarmupManager {
    pub fn new(
        config: BookStoreConfig,
        book_store: Arc<BookStore>,
        subscription_manager: Arc<SubscriptionManager>,
    ) -> Arc<Self> {
        Arc::new(Self {
            config,
            book_store,
            subscription_manager,
            is_warm: AtomicBool::new(false),
            disabled_tokens: RwLock::new(HashSet::new()),
        })
    }

    /// Run warmup phase for a set of tokens
    /// Returns true if warmup succeeded (enough tokens ready)
    pub async fn warmup(&self, tokens: &[String]) -> Result<bool> {
        if tokens.is_empty() {
            self.is_warm.store(true, Ordering::Release);
            return Ok(true);
        }

        info!(
            count = tokens.len(),
            timeout_ms = self.config.warmup_timeout_ms,
            "Starting orderbook warmup phase"
        );

        // Set universe (triggers subscriptions)
        self.subscription_manager
            .set_universe(tokens.to_vec())
            .await;

        // Wait for warmup with timeout
        let deadline = Instant::now() + Duration::from_millis(self.config.warmup_timeout_ms);
        let check_interval = Duration::from_millis(100);

        loop {
            let status = self.book_store.warmup_status(tokens);

            if status.is_warm {
                info!(
                    ready = status.ready_count,
                    total = status.total_count,
                    "Warmup complete"
                );
                self.is_warm.store(true, Ordering::Release);
                return Ok(true);
            }

            if Instant::now() >= deadline {
                // Timeout - disable tokens that aren't ready
                let mut disabled = self.disabled_tokens.write();
                for token_id in &status.not_ready_tokens {
                    disabled.insert(token_id.clone());
                    warn!(
                        token_id = token_id,
                        "Token failed to warm, disabled for session"
                    );
                }

                if status.ready_fraction >= self.config.warmup_min_ready_fraction {
                    info!(
                        ready = status.ready_count,
                        total = status.total_count,
                        disabled = status.not_ready_tokens.len(),
                        "Warmup partial success (met minimum threshold)"
                    );
                    self.is_warm.store(true, Ordering::Release);
                    return Ok(true);
                } else {
                    warn!(
                        ready = status.ready_count,
                        total = status.total_count,
                        required_fraction = self.config.warmup_min_ready_fraction,
                        "Warmup failed (below minimum threshold)"
                    );
                    return Ok(false);
                }
            }

            sleep(check_interval).await;
        }
    }

    /// Check if trading is allowed (warmup complete)
    #[inline]
    pub fn is_trading_allowed(&self) -> bool {
        self.is_warm.load(Ordering::Acquire)
    }

    /// Check if a specific token is disabled
    #[inline]
    pub fn is_token_disabled(&self, token_id: &str) -> bool {
        self.disabled_tokens.read().contains(token_id)
    }

    /// Get list of disabled tokens
    pub fn disabled_tokens(&self) -> Vec<String> {
        self.disabled_tokens.read().iter().cloned().collect()
    }
}

// ============================================================================
// Utility Functions
// ============================================================================

/// Convert Instant to nanoseconds (relative to process start)
#[inline]
fn instant_to_nanos(instant: Instant) -> u64 {
    static EPOCH: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    let epoch = EPOCH.get_or_init(Instant::now);
    instant.duration_since(*epoch).as_nanos() as u64
}

// ============================================================================
// Integration: HFT BookCache (drop-in replacement for PolymarketMarketWsCache)
// ============================================================================

/// HFT-grade book cache that can be used as a drop-in replacement for PolymarketMarketWsCache
/// This provides the same API but with the HFT-grade implementation underneath
pub struct HftBookCache {
    config: BookStoreConfig,
    book_store: Arc<BookStore>,
    subscription_manager: Arc<SubscriptionManager>,
    warmup_manager: Arc<WarmupManager>,
}

impl HftBookCache {
    /// Spawn the HFT book cache with default configuration
    pub fn spawn() -> Arc<Self> {
        Self::spawn_with_config(BookStoreConfig::from_env())
    }

    /// Spawn with custom configuration
    pub fn spawn_with_config(config: BookStoreConfig) -> Arc<Self> {
        let book_store = BookStore::new(config.clone());
        let subscription_manager = SubscriptionManager::spawn(config.clone(), book_store.clone());
        let warmup_manager = WarmupManager::new(
            config.clone(),
            book_store.clone(),
            subscription_manager.clone(),
        );

        Arc::new(Self {
            config,
            book_store,
            subscription_manager,
            warmup_manager,
        })
    }

    /// Request subscription to a token_id (non-blocking, backward compatible)
    pub fn request_subscribe(&self, token_id: &str) {
        if token_id.trim().is_empty() {
            return;
        }
        self.subscription_manager.ensure_subscribed(token_id.trim());
    }

    /// Get cached orderbook (backward compatible API)
    /// Returns None if stale or not ready - NEVER blocks for REST
    pub fn get_orderbook(&self, token_id: &str, max_age_ms: i64) -> Option<Arc<OrderBook>> {
        let max_stale = if max_age_ms <= 0 {
            self.config.default_max_stale_ms
        } else {
            max_age_ms as u64
        };

        self.book_store.get_orderbook_compat(token_id, max_stale)
    }

    /// Get book with detailed result (new HFT API)
    pub fn get_book(&self, token_id: &str, max_stale_ms: u64) -> BookLookupResult {
        self.book_store.get_book(token_id, max_stale_ms)
    }

    /// Get book snapshot if fresh
    pub fn get_book_if_fresh(
        &self,
        token_id: &str,
        max_stale_ms: u64,
    ) -> Option<Arc<BookSnapshot>> {
        self.book_store.get_book_if_fresh(token_id, max_stale_ms)
    }

    /// Set the trading universe (eager subscription)
    pub async fn set_universe(&self, tokens: Vec<String>) {
        self.subscription_manager.set_universe(tokens).await;
    }

    /// Run warmup phase
    pub async fn warmup(&self, tokens: &[String]) -> Result<bool> {
        self.warmup_manager.warmup(tokens).await
    }

    /// Check if trading is allowed
    pub fn is_trading_allowed(&self) -> bool {
        self.warmup_manager.is_trading_allowed()
    }

    /// Check if a token is disabled
    pub fn is_token_disabled(&self, token_id: &str) -> bool {
        self.warmup_manager.is_token_disabled(token_id)
    }

    /// Get warmup status
    pub fn warmup_status(&self, universe: &[String]) -> WarmupStatus {
        self.book_store.warmup_status(universe)
    }

    /// Get BookStore metrics
    pub fn book_metrics(&self) -> &Arc<BookStoreMetrics> {
        self.book_store.metrics()
    }

    /// Get SubscriptionManager metrics
    pub fn subscription_metrics(&self) -> &Arc<SubscriptionMetrics> {
        self.subscription_manager.metrics()
    }

    /// Subscribe to book updates for a token
    pub fn subscribe_updates(&self, token_id: &str) -> Option<watch::Receiver<Instant>> {
        self.book_store.subscribe_updates(token_id)
    }

    /// Check if connected to WS
    pub fn is_connected(&self) -> bool {
        self.subscription_manager.is_connected()
    }

    /// Request resubscription for a stale token
    pub fn request_resubscribe(&self, token_id: &str) {
        self.subscription_manager.request_resubscribe(token_id);
    }

    /// Get book store reference (for advanced use)
    pub fn book_store(&self) -> &Arc<BookStore> {
        &self.book_store
    }

    /// Get subscription manager reference (for advanced use)
    pub fn subscription_manager(&self) -> &Arc<SubscriptionManager> {
        &self.subscription_manager
    }

    /// Shutdown the cache
    pub async fn shutdown(&self) {
        self.subscription_manager.shutdown().await;
    }

    /// Print health summary
    pub fn health_summary(&self) -> String {
        let book_metrics = self.book_store.metrics().summary();
        let sub_metrics = self.subscription_manager.metrics();

        format!(
            "BookStore: hits={} misses={} hit_rate={:.2}% mean_age={:.1}ms | \
             Subs: count={} reconnects={} msgs={} ready={}",
            book_metrics.cache_hits,
            book_metrics.cache_misses,
            book_metrics.hit_rate * 100.0,
            book_metrics.mean_age_ms,
            sub_metrics.subscriptions.load(Ordering::Relaxed),
            sub_metrics.reconnects.load(Ordering::Relaxed),
            sub_metrics.messages_received.load(Ordering::Relaxed),
            sub_metrics.ready_token_count.load(Ordering::Relaxed),
        )
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_apply_level_update_bid() {
        let mut levels = vec![
            PriceLevel {
                price: 0.55,
                size: 100.0,
            },
            PriceLevel {
                price: 0.50,
                size: 50.0,
            },
        ];

        // Update existing
        apply_level_update(&mut levels, 0.55, 150.0, true);
        assert_eq!(levels[0].size, 150.0);

        // Insert new higher price
        apply_level_update(&mut levels, 0.60, 200.0, true);
        assert_eq!(levels[0].price, 0.60);
        assert_eq!(levels.len(), 3);

        // Remove level
        apply_level_update(&mut levels, 0.50, 0.0, true);
        assert_eq!(levels.len(), 2);
    }

    #[test]
    fn test_apply_level_update_ask() {
        let mut levels = vec![
            PriceLevel {
                price: 0.60,
                size: 100.0,
            },
            PriceLevel {
                price: 0.65,
                size: 50.0,
            },
        ];

        // Insert new lower price
        apply_level_update(&mut levels, 0.55, 200.0, false);
        assert_eq!(levels[0].price, 0.55);
        assert_eq!(levels.len(), 3);
    }

    #[test]
    fn test_book_snapshot_metrics() {
        let snapshot = BookSnapshot {
            bids: vec![PriceLevel {
                price: 0.48,
                size: 100.0,
            }],
            asks: vec![PriceLevel {
                price: 0.52,
                size: 100.0,
            }],
            sequence: Some(1),
            created_at: Instant::now(),
        };

        assert_eq!(snapshot.best_bid(), Some(0.48));
        assert_eq!(snapshot.best_ask(), Some(0.52));
        assert!((snapshot.mid_price().unwrap() - 0.50).abs() < 0.001);
        assert!((snapshot.spread_bps().unwrap() - 800.0).abs() < 1.0); // 4% spread = 400bps
        assert!(!snapshot.is_crossed());
    }

    #[test]
    fn test_crossed_book_detection() {
        let crossed = BookSnapshot {
            bids: vec![PriceLevel {
                price: 0.55,
                size: 100.0,
            }],
            asks: vec![PriceLevel {
                price: 0.50,
                size: 100.0,
            }],
            sequence: None,
            created_at: Instant::now(),
        };

        assert!(crossed.is_crossed());
    }
}

//! Deterministic 15-Minute Window Semantics
//!
//! This module provides the **single source of truth** for 15-minute window boundary
//! computation in the backtest_v2 engine. All window-related calculations MUST use
//! the functions in this module to ensure consistency across:
//! - Signal generation
//! - Probability computation
//! - Order timing decisions
//! - Settlement alignment
//!
//! # Canonical Rule
//!
//! For any simulation time `t` (visible time, in nanoseconds):
//! - `window_index = floor_div(t, W)` where `W = 15 minutes in nanoseconds`
//! - `window_start = window_index * W`
//! - `window_end = window_start + W`
//!
//! The window is **half-open**: `[window_start, window_end)`.
//! - Events with `visible_ts == window_start` belong to this window.
//! - Events with `visible_ts == window_end` belong to the NEXT window.
//!
//! # Hermetic Guarantee
//!
//! - Window boundaries are computed ONLY from `visible_ts` (simulation clock).
//! - Exchange timestamps are NEVER used for window computation.
//! - Ingest timestamps are NEVER used for window computation.
//! - Wall-clock time is NEVER used.
//!
//! # Single Source of Truth
//!
//! The constant `WINDOW_DURATION_NS` is defined ONLY here. All other modules
//! that need the 15-minute duration MUST import it from this module.

use crate::backtest_v2::clock::Nanos;
use crate::backtest_v2::event_time::VisibleNanos;
use serde::{Deserialize, Serialize};

// =============================================================================
// CONSTANTS - SINGLE SOURCE OF TRUTH
// =============================================================================

/// Nanoseconds per second.
pub const NS_PER_SEC: i64 = 1_000_000_000;

/// Nanoseconds per minute.
pub const NS_PER_MIN: i64 = 60 * NS_PER_SEC;

/// Window duration in seconds (15 minutes).
pub const WINDOW_DURATION_SECS: i64 = 15 * 60;

/// Window duration in nanoseconds (15 minutes).
/// This is THE canonical constant for window duration.
/// All settlement, strategy, and accounting code MUST use this constant.
pub const WINDOW_DURATION_NS: i64 = WINDOW_DURATION_SECS * NS_PER_SEC;

// =============================================================================
// CANONICAL WINDOW BOUNDARY COMPUTATION
// =============================================================================

/// Compute the 15-minute window boundaries for a given simulation time.
///
/// This is THE canonical function for window boundary computation.
/// All code that needs window boundaries MUST call this function.
///
/// # Arguments
///
/// * `t` - Simulation time (visible_ts) in nanoseconds. MUST NOT be negative.
///
/// # Returns
///
/// A tuple of `(window_start_ns, window_end_ns, window_index)`:
/// - `window_start_ns`: The start of the window containing `t` (inclusive).
/// - `window_end_ns`: The end of the window (exclusive).
/// - `window_index`: The zero-based index of the window (for tracking).
///
/// # Half-Open Interval Semantics
///
/// The window is `[window_start, window_end)`:
/// - `t == window_start` → belongs to this window
/// - `t == window_end` → belongs to the NEXT window (index + 1)
///
/// # Example
///
/// ```ignore
/// let (start, end, idx) = window_bounds_15m(900_000_000_000); // t = 900s
/// assert_eq!(start, 900_000_000_000); // 15m boundary (900s = 15 * 60)
/// assert_eq!(end, 1_800_000_000_000); // 30m boundary
/// assert_eq!(idx, 1); // Second window (0-indexed)
/// ```
///
/// # Panics
///
/// Panics if `t < 0`. Negative simulation times are not supported.
#[inline]
pub fn window_bounds_15m(t: Nanos) -> (Nanos, Nanos, i64) {
    debug_assert!(t >= 0, "window_bounds_15m: negative time t={} not supported", t);
    
    // Integer division for window index (floor division for non-negative t)
    let window_index = t / WINDOW_DURATION_NS;
    let window_start = window_index * WINDOW_DURATION_NS;
    let window_end = window_start + WINDOW_DURATION_NS;
    
    (window_start, window_end, window_index)
}

/// Compute window boundaries from a `VisibleNanos` timestamp.
///
/// This is a convenience wrapper around `window_bounds_15m` that accepts
/// the strongly-typed `VisibleNanos` wrapper used in the event model.
#[inline]
pub fn window_bounds_from_visible(visible_ts: VisibleNanos) -> WindowBounds {
    let (window_start, window_end, window_index) = window_bounds_15m(visible_ts.0);
    WindowBounds {
        window_start: VisibleNanos(window_start),
        window_end: VisibleNanos(window_end),
        window_index,
    }
}

/// Structured representation of window boundaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct WindowBounds {
    /// Start of the window (inclusive).
    pub window_start: VisibleNanos,
    /// End of the window (exclusive).
    pub window_end: VisibleNanos,
    /// Zero-based window index.
    pub window_index: i64,
}

impl WindowBounds {
    /// Create new bounds from a visible timestamp.
    #[inline]
    pub fn from_visible_ts(visible_ts: VisibleNanos) -> Self {
        window_bounds_from_visible(visible_ts)
    }
    
    /// Create new bounds from a raw nanosecond timestamp.
    #[inline]
    pub fn from_nanos(t: Nanos) -> Self {
        let (window_start, window_end, window_index) = window_bounds_15m(t);
        Self {
            window_start: VisibleNanos(window_start),
            window_end: VisibleNanos(window_end),
            window_index,
        }
    }
    
    /// Check if a timestamp is within this window (half-open: [start, end)).
    #[inline]
    pub fn contains(&self, t: VisibleNanos) -> bool {
        t >= self.window_start && t < self.window_end
    }
    
    /// Compute remaining time from `t` to window end (in seconds).
    /// Returns 0.0 if `t >= window_end`.
    #[inline]
    pub fn remaining_secs(&self, t: VisibleNanos) -> f64 {
        let remaining_ns = (self.window_end.0 - t.0).max(0);
        remaining_ns as f64 / NS_PER_SEC as f64
    }
    
    /// Compute remaining time from `t` to window end (in nanoseconds).
    /// Returns 0 if `t >= window_end`.
    #[inline]
    pub fn remaining_ns(&self, t: VisibleNanos) -> i64 {
        (self.window_end.0 - t.0).max(0)
    }
    
    /// Get the window duration in nanoseconds.
    #[inline]
    pub const fn duration_ns(&self) -> i64 {
        WINDOW_DURATION_NS
    }
    
    /// Get the window duration in seconds.
    #[inline]
    pub const fn duration_secs(&self) -> i64 {
        WINDOW_DURATION_SECS
    }
}

// =============================================================================
// WINDOW INDEX COMPUTATION
// =============================================================================

/// Compute the window index for a given timestamp.
///
/// This is a pure function that computes the zero-based window index.
/// Window 0 covers `[0, W)`, window 1 covers `[W, 2W)`, etc.
#[inline]
pub fn window_index(t: Nanos) -> i64 {
    debug_assert!(t >= 0, "window_index: negative time t={} not supported", t);
    t / WINDOW_DURATION_NS
}

/// Compute the window start time for a given window index.
#[inline]
pub fn window_start_from_index(index: i64) -> Nanos {
    debug_assert!(index >= 0, "window_start_from_index: negative index {} not supported", index);
    index * WINDOW_DURATION_NS
}

/// Compute the window end time for a given window index.
#[inline]
pub fn window_end_from_index(index: i64) -> Nanos {
    debug_assert!(index >= 0, "window_end_from_index: negative index {} not supported", index);
    (index + 1) * WINDOW_DURATION_NS
}

// =============================================================================
// ROLLOVER DETECTION
// =============================================================================

/// Check if two timestamps belong to different windows.
///
/// This is the canonical way to detect a window rollover.
/// Returns `true` if `t1` and `t2` have different window indices.
#[inline]
pub fn is_different_window(t1: Nanos, t2: Nanos) -> bool {
    window_index(t1) != window_index(t2)
}

/// Check if `t2` is in a later window than `t1`.
///
/// Returns `true` if `window_index(t2) > window_index(t1)`.
#[inline]
pub fn is_later_window(t1: Nanos, t2: Nanos) -> bool {
    window_index(t2) > window_index(t1)
}

// =============================================================================
// ALIGNMENT UTILITIES
// =============================================================================

/// Align a timestamp to the start of its containing window.
///
/// Equivalent to `window_bounds_15m(t).0`.
#[inline]
pub fn align_to_window_start(t: Nanos) -> Nanos {
    (t / WINDOW_DURATION_NS) * WINDOW_DURATION_NS
}

/// Align a timestamp to the end of its containing window.
///
/// Returns the exclusive end time of the window containing `t`.
#[inline]
pub fn align_to_window_end(t: Nanos) -> Nanos {
    align_to_window_start(t) + WINDOW_DURATION_NS
}

// =============================================================================
// REMAINING TIME COMPUTATION
// =============================================================================

/// Compute remaining time in the current window (in seconds).
///
/// This is the canonical `t_rem` computation for the 15M strategy.
/// Returns `max(0, window_end - t)` converted to seconds.
///
/// # Note
///
/// If `t >= window_end`, this returns 0.0. The caller should detect rollover
/// BEFORE computing remaining time to avoid getting stale values.
#[inline]
pub fn remaining_time_secs(t: Nanos) -> f64 {
    let (_, window_end, _) = window_bounds_15m(t);
    let remaining_ns = (window_end - t).max(0);
    remaining_ns as f64 / NS_PER_SEC as f64
}

/// Compute remaining time in the current window (in nanoseconds).
///
/// Returns `max(0, window_end - t)`.
#[inline]
pub fn remaining_time_ns(t: Nanos) -> i64 {
    let (_, window_end, _) = window_bounds_15m(t);
    (window_end - t).max(0)
}

// =============================================================================
// P_START DETERMINATION CONFIGURATION
// =============================================================================

/// Configuration for P_start (reference price at window start) determination.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PStartConfig {
    /// Whether to allow carry-forward of P_start from a previous window.
    ///
    /// If `true` and no Binance mid-price arrives in `[window_start, t)`:
    /// - Use the last Binance mid-price with `visible_ts < window_start`.
    /// - Record that carry-forward occurred in logs/metrics.
    ///
    /// If `false` (production-grade default):
    /// - Strategy is NOT allowed to trade if no P_start is available.
    /// - This ensures the contract's start price is well-defined.
    pub allow_carry_forward: bool,
    
    /// Log level for carry-forward events.
    pub log_carry_forward: bool,
}

impl Default for PStartConfig {
    fn default() -> Self {
        Self {
            allow_carry_forward: false,  // Production-grade: no carry-forward
            log_carry_forward: true,
        }
    }
}

impl PStartConfig {
    /// Production-grade configuration: no carry-forward allowed.
    pub fn production() -> Self {
        Self {
            allow_carry_forward: false,
            log_carry_forward: true,
        }
    }
    
    /// Research-grade configuration: carry-forward allowed.
    pub fn research() -> Self {
        Self {
            allow_carry_forward: true,
            log_carry_forward: true,
        }
    }
}

// =============================================================================
// PER-WINDOW STATE MACHINE
// =============================================================================

/// State of P_start for a window.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum StartPriceState {
    /// No start price observed yet in this window.
    /// Contains the previous window's final price if available for carry-forward.
    Pending { carried_price: Option<f64> },
    
    /// Start price was directly observed in this window.
    /// The price is the first Binance mid-price with `visible_ts >= window_start`.
    Observed {
        price: f64,
        observed_at: VisibleNanos,
    },
    
    /// Start price was carried forward from the previous window.
    /// The price is the last Binance mid-price with `visible_ts < window_start`.
    CarriedForward {
        price: f64,
        original_observation_ts: VisibleNanos,
    },
}

impl StartPriceState {
    /// Get the P_start value if available.
    pub fn price(&self) -> Option<f64> {
        match self {
            StartPriceState::Pending { .. } => None,
            StartPriceState::Observed { price, .. } => Some(*price),
            StartPriceState::CarriedForward { price, .. } => Some(*price),
        }
    }
    
    /// Check if a direct observation has been made.
    pub fn is_observed(&self) -> bool {
        matches!(self, StartPriceState::Observed { .. })
    }
    
    /// Check if the price was carried forward.
    pub fn is_carried_forward(&self) -> bool {
        matches!(self, StartPriceState::CarriedForward { .. })
    }
    
    /// Check if P_start is available (observed or carried forward).
    pub fn is_available(&self) -> bool {
        self.price().is_some()
    }
}

/// Per-window state for the 15M Up/Down strategy.
///
/// This struct tracks all per-window state that must be reset at window boundaries.
/// The state machine resets exactly once per window transition, BEFORE processing
/// the triggering event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowState {
    /// Current window index.
    pub window_index: i64,
    /// Window start time (nanoseconds).
    pub window_start: VisibleNanos,
    /// Window end time (nanoseconds).
    pub window_end: VisibleNanos,
    /// Start price state (P_start).
    pub start_price_state: StartPriceState,
    /// Most recent Binance mid-price (P_now).
    pub p_now: Option<f64>,
    /// Timestamp when P_now was last updated.
    pub p_now_ts: Option<VisibleNanos>,
    /// Whether the strategy has traded in this window.
    pub has_traded: bool,
    /// Number of trades in this window.
    pub trade_count: u32,
    /// Number of Binance price updates in this window.
    pub price_update_count: u32,
    /// Number of Polymarket book updates in this window.
    pub book_update_count: u32,
}

impl WindowState {
    /// Create initial state for a given window.
    pub fn new(visible_ts: VisibleNanos) -> Self {
        let bounds = WindowBounds::from_visible_ts(visible_ts);
        Self {
            window_index: bounds.window_index,
            window_start: bounds.window_start,
            window_end: bounds.window_end,
            start_price_state: StartPriceState::Pending { carried_price: None },
            p_now: None,
            p_now_ts: None,
            has_traded: false,
            trade_count: 0,
            price_update_count: 0,
            book_update_count: 0,
        }
    }
    
    /// Check if the given timestamp requires a window rollover.
    ///
    /// Returns `true` if `visible_ts` belongs to a different window than the current state.
    #[inline]
    pub fn needs_rollover(&self, visible_ts: VisibleNanos) -> bool {
        let new_index = window_index(visible_ts.0);
        new_index != self.window_index
    }
    
    /// Perform a window rollover to the window containing `visible_ts`.
    ///
    /// This resets all per-window state except for the carried price
    /// which is preserved for potential carry-forward.
    pub fn rollover_to(&mut self, visible_ts: VisibleNanos) {
        let bounds = WindowBounds::from_visible_ts(visible_ts);
        
        // Preserve the last P_now for potential carry-forward
        let carried_price = self.p_now;
        let carried_ts = self.p_now_ts;
        
        // Reset window state
        self.window_index = bounds.window_index;
        self.window_start = bounds.window_start;
        self.window_end = bounds.window_end;
        self.start_price_state = StartPriceState::Pending { carried_price };
        self.p_now = None;  // Will be set by first price update
        self.p_now_ts = None;
        self.has_traded = false;
        self.trade_count = 0;
        self.price_update_count = 0;
        self.book_update_count = 0;
        
        // If we have a carried price and it's from before this window, store it
        if let (Some(price), Some(ts)) = (carried_price, carried_ts) {
            if ts < self.window_start {
                // This is a valid carry-forward candidate
                self.start_price_state = StartPriceState::Pending {
                    carried_price: Some(price),
                };
            }
        }
    }
    
    /// Process a Binance mid-price update.
    ///
    /// This updates P_now and potentially sets P_start if this is the first
    /// observation in the current window.
    ///
    /// # Arguments
    ///
    /// * `visible_ts` - The visible timestamp of the price update.
    /// * `mid_price` - The Binance mid-price.
    ///
    /// # Panics
    ///
    /// Debug-panics if `visible_ts` is outside the current window.
    pub fn update_price(&mut self, visible_ts: VisibleNanos, mid_price: f64) {
        debug_assert!(
            self.window_start <= visible_ts && visible_ts < self.window_end,
            "update_price called with out-of-window timestamp: {:?} not in [{:?}, {:?})",
            visible_ts, self.window_start, self.window_end
        );
        
        // Update P_now unconditionally
        self.p_now = Some(mid_price);
        self.p_now_ts = Some(visible_ts);
        self.price_update_count += 1;
        
        // Set P_start if this is the first observation in the window
        if matches!(self.start_price_state, StartPriceState::Pending { .. }) {
            self.start_price_state = StartPriceState::Observed {
                price: mid_price,
                observed_at: visible_ts,
            };
        }
    }
    
    /// Apply carry-forward for P_start if configured and needed.
    ///
    /// This should be called after rollover if `config.allow_carry_forward` is true
    /// and no direct P_start observation has been made yet.
    ///
    /// Returns `true` if carry-forward was applied.
    pub fn apply_carry_forward(&mut self, _config: &PStartConfig) -> bool {
        if let StartPriceState::Pending { carried_price: Some(price) } = &self.start_price_state {
            // We have a carried price candidate
            let price = *price;
            self.start_price_state = StartPriceState::CarriedForward {
                price,
                original_observation_ts: self.window_start,  // Approximate
            };
            true
        } else {
            false
        }
    }
    
    /// Get the remaining time in seconds.
    #[inline]
    pub fn remaining_secs(&self, visible_ts: VisibleNanos) -> f64 {
        let remaining_ns = (self.window_end.0 - visible_ts.0).max(0);
        remaining_ns as f64 / NS_PER_SEC as f64
    }
    
    /// Check if trading is allowed (P_start is available).
    #[inline]
    pub fn can_trade(&self) -> bool {
        self.start_price_state.is_available()
    }
    
    /// Record that a trade was executed.
    pub fn record_trade(&mut self) {
        self.has_traded = true;
        self.trade_count += 1;
    }
    
    /// Record a book update.
    pub fn record_book_update(&mut self) {
        self.book_update_count += 1;
    }
}

// =============================================================================
// STRATEGY CONTEXT EXTENSION
// =============================================================================

/// Window-aware context for strategy decision-making.
///
/// This struct provides a read-only view of window state for strategies.
/// It enforces that the strategy only uses `visible_ts` for all time-based decisions.
#[derive(Debug, Clone, Copy)]
pub struct WindowContext {
    /// Current simulation time (visible_ts).
    pub now: VisibleNanos,
    /// Window start (inclusive).
    pub window_start: VisibleNanos,
    /// Window end (exclusive).
    pub window_end: VisibleNanos,
    /// Window index.
    pub window_index: i64,
    /// Remaining time in seconds.
    pub remaining_secs: f64,
    /// P_start (if available).
    pub p_start: Option<f64>,
    /// P_now (most recent price).
    pub p_now: Option<f64>,
    /// Whether trading is allowed (P_start available).
    pub can_trade: bool,
}

impl WindowContext {
    /// Create a window context from WindowState and current time.
    pub fn from_state(state: &WindowState, now: VisibleNanos) -> Self {
        Self {
            now,
            window_start: state.window_start,
            window_end: state.window_end,
            window_index: state.window_index,
            remaining_secs: state.remaining_secs(now),
            p_start: state.start_price_state.price(),
            p_now: state.p_now,
            can_trade: state.can_trade(),
        }
    }
}

// =============================================================================
// TESTS
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // -------------------------------------------------------------------------
    // Basic boundary tests
    // -------------------------------------------------------------------------

    #[test]
    fn test_window_bounds_t_equals_zero() {
        let (start, end, idx) = window_bounds_15m(0);
        assert_eq!(start, 0);
        assert_eq!(end, WINDOW_DURATION_NS);
        assert_eq!(idx, 0);
    }

    #[test]
    fn test_window_bounds_t_equals_w_minus_1ns() {
        // Just before the first window ends
        let t = WINDOW_DURATION_NS - 1;
        let (start, end, idx) = window_bounds_15m(t);
        assert_eq!(start, 0);
        assert_eq!(end, WINDOW_DURATION_NS);
        assert_eq!(idx, 0);
    }

    #[test]
    fn test_window_bounds_t_equals_w() {
        // Exactly at the first window boundary
        let t = WINDOW_DURATION_NS;
        let (start, end, idx) = window_bounds_15m(t);
        // t == W belongs to the SECOND window (half-open semantics)
        assert_eq!(start, WINDOW_DURATION_NS);
        assert_eq!(end, 2 * WINDOW_DURATION_NS);
        assert_eq!(idx, 1);
    }

    #[test]
    fn test_window_bounds_t_equals_2w() {
        let t = 2 * WINDOW_DURATION_NS;
        let (start, end, idx) = window_bounds_15m(t);
        assert_eq!(start, 2 * WINDOW_DURATION_NS);
        assert_eq!(end, 3 * WINDOW_DURATION_NS);
        assert_eq!(idx, 2);
    }

    #[test]
    fn test_window_bounds_representative_mid_run() {
        // 1000 seconds into the simulation
        let t = 1000 * NS_PER_SEC;
        let (start, end, idx) = window_bounds_15m(t);
        // Window 1 is [900s, 1800s) = [900*1e9, 1800*1e9)
        assert_eq!(start, 900 * NS_PER_SEC);
        assert_eq!(end, 1800 * NS_PER_SEC);
        assert_eq!(idx, 1);
    }

    // -------------------------------------------------------------------------
    // Property tests
    // -------------------------------------------------------------------------

    #[test]
    fn test_property_window_contains_t() {
        // For any t, window_start <= t < window_end
        for t in [0, 1, 100, 899 * NS_PER_SEC, 900 * NS_PER_SEC, 901 * NS_PER_SEC,
                  1000 * NS_PER_SEC, 1799 * NS_PER_SEC, 1800 * NS_PER_SEC,
                  10000 * NS_PER_SEC, 86400 * NS_PER_SEC] {
            let (start, end, _) = window_bounds_15m(t);
            assert!(
                start <= t && t < end,
                "Property violated for t={}: start={}, end={}",
                t, start, end
            );
        }
    }

    #[test]
    fn test_property_consecutive_windows_no_gap() {
        // For consecutive windows, end[i] == start[i+1]
        for idx in 0..100 {
            let start_i = window_start_from_index(idx);
            let end_i = window_end_from_index(idx);
            let start_next = window_start_from_index(idx + 1);
            assert_eq!(end_i, start_next, "Gap between window {} and {}", idx, idx + 1);
        }
    }

    #[test]
    fn test_property_window_duration_constant() {
        // All windows have the same duration
        for idx in 0..100 {
            let start = window_start_from_index(idx);
            let end = window_end_from_index(idx);
            assert_eq!(end - start, WINDOW_DURATION_NS, "Window {} has wrong duration", idx);
        }
    }

    // -------------------------------------------------------------------------
    // Half-open semantics tests
    // -------------------------------------------------------------------------

    #[test]
    fn test_half_open_at_exact_boundary() {
        // Event at exactly W belongs to window 1, not window 0
        let t = WINDOW_DURATION_NS;
        let (start, _, idx) = window_bounds_15m(t);
        assert_eq!(idx, 1, "Event at W should be in window 1");
        assert_eq!(start, WINDOW_DURATION_NS, "Window 1 starts at W");
    }

    #[test]
    fn test_half_open_just_before_boundary() {
        // Event at W - 1 belongs to window 0
        let t = WINDOW_DURATION_NS - 1;
        let (_, _, idx) = window_bounds_15m(t);
        assert_eq!(idx, 0, "Event at W-1 should be in window 0");
    }

    // -------------------------------------------------------------------------
    // WindowBounds tests
    // -------------------------------------------------------------------------

    #[test]
    fn test_window_bounds_contains() {
        let bounds = WindowBounds::from_nanos(1000 * NS_PER_SEC);
        // Window is [900, 1800)
        assert!(bounds.contains(VisibleNanos(900 * NS_PER_SEC)));
        assert!(bounds.contains(VisibleNanos(1000 * NS_PER_SEC)));
        assert!(bounds.contains(VisibleNanos(1799 * NS_PER_SEC)));
        assert!(!bounds.contains(VisibleNanos(1800 * NS_PER_SEC))); // Half-open
        assert!(!bounds.contains(VisibleNanos(899 * NS_PER_SEC)));
    }

    #[test]
    fn test_window_bounds_remaining() {
        let bounds = WindowBounds::from_nanos(900 * NS_PER_SEC); // Window [900, 1800)
        
        // At window start: 900 seconds remaining
        let rem = bounds.remaining_secs(VisibleNanos(900 * NS_PER_SEC));
        assert!((rem - 900.0).abs() < 0.001);
        
        // Halfway through: 450 seconds remaining
        let rem = bounds.remaining_secs(VisibleNanos(1350 * NS_PER_SEC));
        assert!((rem - 450.0).abs() < 0.001);
        
        // At window end: 0 seconds remaining
        let rem = bounds.remaining_secs(VisibleNanos(1800 * NS_PER_SEC));
        assert_eq!(rem, 0.0);
        
        // Past window end: still 0
        let rem = bounds.remaining_secs(VisibleNanos(2000 * NS_PER_SEC));
        assert_eq!(rem, 0.0);
    }

    // -------------------------------------------------------------------------
    // Rollover detection tests
    // -------------------------------------------------------------------------

    #[test]
    fn test_is_different_window() {
        assert!(!is_different_window(0, 100));
        assert!(!is_different_window(0, WINDOW_DURATION_NS - 1));
        assert!(is_different_window(0, WINDOW_DURATION_NS));
        assert!(is_different_window(WINDOW_DURATION_NS - 1, WINDOW_DURATION_NS));
    }

    #[test]
    fn test_is_later_window() {
        assert!(!is_later_window(0, 0));
        assert!(!is_later_window(WINDOW_DURATION_NS, 0));
        assert!(is_later_window(0, WINDOW_DURATION_NS));
        assert!(is_later_window(0, 2 * WINDOW_DURATION_NS));
    }

    // -------------------------------------------------------------------------
    // WindowState rollover tests
    // -------------------------------------------------------------------------

    #[test]
    fn test_window_state_initial() {
        let state = WindowState::new(VisibleNanos(1000 * NS_PER_SEC));
        assert_eq!(state.window_index, 1);
        assert_eq!(state.window_start, VisibleNanos(900 * NS_PER_SEC));
        assert_eq!(state.window_end, VisibleNanos(1800 * NS_PER_SEC));
        assert!(!state.can_trade());
    }

    #[test]
    fn test_window_state_price_update_sets_p_start() {
        let mut state = WindowState::new(VisibleNanos(900 * NS_PER_SEC));
        assert!(state.start_price_state.price().is_none());
        
        state.update_price(VisibleNanos(905 * NS_PER_SEC), 50000.0);
        
        assert!(state.start_price_state.is_observed());
        assert_eq!(state.start_price_state.price(), Some(50000.0));
        assert!(state.can_trade());
    }

    #[test]
    fn test_window_state_rollover_resets_counters() {
        let mut state = WindowState::new(VisibleNanos(900 * NS_PER_SEC));
        state.update_price(VisibleNanos(950 * NS_PER_SEC), 50000.0);
        state.record_trade();
        state.record_book_update();
        
        assert!(state.has_traded);
        assert_eq!(state.trade_count, 1);
        assert_eq!(state.price_update_count, 1);
        assert_eq!(state.book_update_count, 1);
        
        // Rollover to next window
        state.rollover_to(VisibleNanos(1800 * NS_PER_SEC));
        
        assert_eq!(state.window_index, 2);
        assert!(!state.has_traded);
        assert_eq!(state.trade_count, 0);
        assert_eq!(state.price_update_count, 0);
        assert_eq!(state.book_update_count, 0);
    }

    #[test]
    fn test_window_state_rollover_preserves_p_now_for_carry_forward() {
        let mut state = WindowState::new(VisibleNanos(900 * NS_PER_SEC));
        state.update_price(VisibleNanos(1700 * NS_PER_SEC), 51000.0);
        
        // Rollover to next window
        state.rollover_to(VisibleNanos(1800 * NS_PER_SEC));
        
        // P_start should be pending with carried price
        if let StartPriceState::Pending { carried_price } = &state.start_price_state {
            assert_eq!(*carried_price, Some(51000.0));
        } else {
            panic!("Expected Pending state with carried price");
        }
    }

    #[test]
    fn test_window_state_apply_carry_forward() {
        let mut state = WindowState::new(VisibleNanos(900 * NS_PER_SEC));
        state.update_price(VisibleNanos(1700 * NS_PER_SEC), 51000.0);
        state.rollover_to(VisibleNanos(1800 * NS_PER_SEC));
        
        let config = PStartConfig::research();
        let applied = state.apply_carry_forward(&config);
        
        assert!(applied);
        assert!(state.start_price_state.is_carried_forward());
        assert_eq!(state.start_price_state.price(), Some(51000.0));
        assert!(state.can_trade());
    }

    #[test]
    fn test_window_state_needs_rollover() {
        let state = WindowState::new(VisibleNanos(900 * NS_PER_SEC));
        
        // Same window
        assert!(!state.needs_rollover(VisibleNanos(900 * NS_PER_SEC)));
        assert!(!state.needs_rollover(VisibleNanos(1000 * NS_PER_SEC)));
        assert!(!state.needs_rollover(VisibleNanos(1799 * NS_PER_SEC)));
        
        // Next window (half-open semantics)
        assert!(state.needs_rollover(VisibleNanos(1800 * NS_PER_SEC)));
        assert!(state.needs_rollover(VisibleNanos(1801 * NS_PER_SEC)));
    }

    // -------------------------------------------------------------------------
    // Sequence of events rollover test
    // -------------------------------------------------------------------------

    #[test]
    fn test_rollover_sequence_of_events() {
        // Simulate a sequence of events straddling a window boundary
        let events = vec![
            (890 * NS_PER_SEC, "price_890"),   // Window 0
            (899 * NS_PER_SEC, "price_899"),   // Window 0 (just before rollover)
            (900 * NS_PER_SEC, "price_900"),   // Window 1 (exactly at boundary)
            (901 * NS_PER_SEC, "price_901"),   // Window 1
        ];
        
        let mut state = WindowState::new(VisibleNanos(events[0].0));
        let mut rollovers = vec![];
        
        for (t, label) in &events {
            let visible_ts = VisibleNanos(*t);
            if state.needs_rollover(visible_ts) {
                state.rollover_to(visible_ts);
                rollovers.push(*label);
            }
            state.update_price(visible_ts, 50000.0);
        }
        
        // Should have rolled over exactly once, at "price_900"
        assert_eq!(rollovers, vec!["price_900"]);
        
        // Final state should be in window 1
        assert_eq!(state.window_index, 1);
    }

    // -------------------------------------------------------------------------
    // Invariant: strategy time comes from ctx.now
    // -------------------------------------------------------------------------

    #[test]
    fn test_window_context_uses_visible_time_only() {
        let mut state = WindowState::new(VisibleNanos(1000 * NS_PER_SEC));
        state.update_price(VisibleNanos(1000 * NS_PER_SEC), 50000.0);
        
        let ctx = WindowContext::from_state(&state, VisibleNanos(1100 * NS_PER_SEC));
        
        // All times in context should be derived from visible_ts
        assert_eq!(ctx.now.0, 1100 * NS_PER_SEC);
        assert_eq!(ctx.window_start.0, 900 * NS_PER_SEC);
        assert_eq!(ctx.window_end.0, 1800 * NS_PER_SEC);
        // remaining_secs should be computed from visible_ts
        assert!((ctx.remaining_secs - 700.0).abs() < 0.001);
    }
}

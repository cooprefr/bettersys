//! Slippage and Impact Attribution
//!
//! Framework for attributing fill price deviations to observed trade prints.
//! Provides diagnostic metrics for analyzing execution quality against market activity.
//!
//! # Attribution Model
//!
//! When a fill occurs at time T with price P:
//! 1. Find nearby trade prints in window [T - Δ, T + Δ]
//! 2. Compute mid-move diagnostics: how did mid price move around the fill?
//! 3. Track trade volume during fill window
//! 4. Detect adverse selection: did fill precede adverse price movement?
//!
//! # IMPORTANT: Diagnostics Only
//!
//! These metrics are DIAGNOSTICS - they help analyze execution quality
//! but DO NOT change matching outcomes. The execution model is deterministic
//! and independent of these attribution metrics unless explicitly configured.

use crate::backtest_v2::events::Side;
use crate::backtest_v2::trade_print::PolymarketTradePrint;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, VecDeque};

// =============================================================================
// CONSTANTS
// =============================================================================

/// Default window for nearby prints (1 second in nanoseconds).
pub const DEFAULT_NEARBY_WINDOW_NS: i64 = 1_000_000_000;

/// Default horizons for mid-move analysis (in nanoseconds).
pub const DEFAULT_MID_MOVE_HORIZONS_NS: &[i64] = &[
    100_000_000,    // 100ms
    500_000_000,    // 500ms
    1_000_000_000,  // 1s
    5_000_000_000,  // 5s
];

/// Maximum trade prints to retain in rolling buffer per market.
pub const DEFAULT_BUFFER_SIZE: usize = 10_000;

// =============================================================================
// NEARBY PRINT CONTEXT
// =============================================================================

/// Context about trade prints near a fill.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NearbyPrintContext {
    /// Number of prints before the fill (in window).
    pub prints_before: usize,
    /// Number of prints after the fill (in window).
    pub prints_after: usize,
    /// Total volume traded before fill (in window).
    pub volume_before: f64,
    /// Total volume traded after fill (in window).
    pub volume_after: f64,
    /// Total notional traded before fill.
    pub notional_before: f64,
    /// Total notional traded after fill.
    pub notional_after: f64,
    /// Volume-weighted average price of prints before fill.
    pub vwap_before: Option<f64>,
    /// Volume-weighted average price of prints after fill.
    pub vwap_after: Option<f64>,
    /// First print timestamp in window.
    pub first_print_ts: Option<i64>,
    /// Last print timestamp in window.
    pub last_print_ts: Option<i64>,
    /// Fill timestamp (visible_ts).
    pub fill_ts: i64,
    /// Window size used (nanoseconds).
    pub window_ns: i64,
}

impl Default for NearbyPrintContext {
    fn default() -> Self {
        Self {
            prints_before: 0,
            prints_after: 0,
            volume_before: 0.0,
            volume_after: 0.0,
            notional_before: 0.0,
            notional_after: 0.0,
            vwap_before: None,
            vwap_after: None,
            first_print_ts: None,
            last_print_ts: None,
            fill_ts: 0,
            window_ns: DEFAULT_NEARBY_WINDOW_NS,
        }
    }
}

impl NearbyPrintContext {
    /// Total prints in window.
    pub fn total_prints(&self) -> usize {
        self.prints_before + self.prints_after
    }

    /// Total volume in window.
    pub fn total_volume(&self) -> f64 {
        self.volume_before + self.volume_after
    }

    /// Total notional in window.
    pub fn total_notional(&self) -> f64 {
        self.notional_before + self.notional_after
    }

    /// Whether any prints were observed.
    pub fn has_prints(&self) -> bool {
        self.total_prints() > 0
    }
}

// =============================================================================
// MID MOVE METRICS
// =============================================================================

/// Mid-price movement metrics around a fill.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MidMoveMetrics {
    /// Mid price at fill time.
    pub mid_at_fill: Option<f64>,
    /// Mid price changes at various horizons after fill.
    /// Key: horizon in nanoseconds, Value: (mid_after, move_bps)
    pub mid_after_horizons: Vec<MidMoveAtHorizon>,
    /// Whether adverse selection was detected.
    /// True if price moved against us after fill.
    pub adverse_selection_detected: bool,
    /// Severity of adverse selection (in basis points).
    pub adverse_selection_bps: Option<f64>,
    /// The horizon at which worst adverse move occurred.
    pub worst_adverse_horizon_ns: Option<i64>,
}

/// Mid-price move at a specific horizon.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MidMoveAtHorizon {
    /// Horizon in nanoseconds.
    pub horizon_ns: i64,
    /// Mid price at this horizon (if available).
    pub mid_price: Option<f64>,
    /// Price move in basis points (vs mid at fill).
    pub move_bps: Option<f64>,
    /// Whether this move is adverse for the fill side.
    pub is_adverse: bool,
}

impl Default for MidMoveMetrics {
    fn default() -> Self {
        Self {
            mid_at_fill: None,
            mid_after_horizons: Vec::new(),
            adverse_selection_detected: false,
            adverse_selection_bps: None,
            worst_adverse_horizon_ns: None,
        }
    }
}

// =============================================================================
// FILL ATTRIBUTION REPORT
// =============================================================================

/// Complete attribution report for a single fill.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FillAttributionReport {
    /// Fill identifier.
    pub fill_id: String,
    /// Order ID.
    pub order_id: u64,
    /// Market ID.
    pub market_id: String,
    /// Fill side.
    pub side: Side,
    /// Fill price.
    pub fill_price: f64,
    /// Fill size.
    pub fill_size: f64,
    /// Fill timestamp (visible_ts).
    pub fill_ts: i64,
    /// Whether this was a maker fill.
    pub is_maker: bool,
    /// Nearby print context.
    pub nearby_prints: NearbyPrintContext,
    /// Mid-move metrics.
    pub mid_move: MidMoveMetrics,
    /// Slippage from mid at fill time (in price units).
    pub slippage_price: Option<f64>,
    /// Slippage from mid at fill time (in basis points).
    pub slippage_bps: Option<f64>,
    /// Trade volume during fill window.
    pub trade_volume_during_fill: f64,
    /// Whether attribution is complete (all data available).
    pub attribution_complete: bool,
    /// Reasons for incomplete attribution.
    pub incomplete_reasons: Vec<String>,
}

impl FillAttributionReport {
    /// Create an incomplete report (data not available).
    pub fn incomplete(
        fill_id: String,
        order_id: u64,
        market_id: String,
        side: Side,
        fill_price: f64,
        fill_size: f64,
        fill_ts: i64,
        is_maker: bool,
        reason: &str,
    ) -> Self {
        Self {
            fill_id,
            order_id,
            market_id,
            side,
            fill_price,
            fill_size,
            fill_ts,
            is_maker,
            nearby_prints: NearbyPrintContext::default(),
            mid_move: MidMoveMetrics::default(),
            slippage_price: None,
            slippage_bps: None,
            trade_volume_during_fill: 0.0,
            attribution_complete: false,
            incomplete_reasons: vec![reason.to_string()],
        }
    }

    /// Whether this fill experienced adverse selection.
    pub fn has_adverse_selection(&self) -> bool {
        self.mid_move.adverse_selection_detected
    }

    /// Worst adverse move in basis points.
    pub fn worst_adverse_bps(&self) -> Option<f64> {
        self.mid_move.adverse_selection_bps
    }
}

// =============================================================================
// TRADE PRINT BUFFER
// =============================================================================

/// Rolling buffer of recent trade prints for attribution.
#[derive(Debug)]
pub struct TradePrintBuffer {
    /// Per-market buffers, ordered by visible_ts.
    markets: BTreeMap<String, VecDeque<PrintSnapshot>>,
    /// Maximum buffer size per market.
    max_size: usize,
}

/// Snapshot of a trade print for attribution.
#[derive(Debug, Clone)]
struct PrintSnapshot {
    visible_ts_ns: i64,
    price: f64,
    size: f64,
    aggressor_side: Side,
}

impl TradePrintBuffer {
    pub fn new(max_size: usize) -> Self {
        Self {
            markets: BTreeMap::new(),
            max_size,
        }
    }

    /// Add a trade print to the buffer.
    pub fn push(&mut self, print: &PolymarketTradePrint) {
        let buffer = self
            .markets
            .entry(print.market_id.clone())
            .or_insert_with(VecDeque::new);

        // Add new print
        buffer.push_back(PrintSnapshot {
            visible_ts_ns: print.visible_ts_ns,
            price: print.price,
            size: print.size,
            aggressor_side: print.aggressor_side,
        });

        // Evict old prints if over capacity
        while buffer.len() > self.max_size {
            buffer.pop_front();
        }
    }

    /// Get prints in a time window around a fill.
    pub fn get_prints_in_window(
        &self,
        market_id: &str,
        center_ts: i64,
        window_ns: i64,
    ) -> Vec<&PrintSnapshot> {
        let start_ts = center_ts - window_ns;
        let end_ts = center_ts + window_ns;

        self.markets
            .get(market_id)
            .map(|buffer| {
                buffer
                    .iter()
                    .filter(|p| p.visible_ts_ns >= start_ts && p.visible_ts_ns <= end_ts)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Clear all buffers.
    pub fn clear(&mut self) {
        self.markets.clear();
    }

    /// Total prints across all markets.
    pub fn total_prints(&self) -> usize {
        self.markets.values().map(|b| b.len()).sum()
    }
}

// =============================================================================
// MID PRICE TRACKER
// =============================================================================

/// Tracks mid prices over time for attribution.
#[derive(Debug)]
pub struct MidPriceTracker {
    /// Per-market mid price history, ordered by time.
    markets: BTreeMap<String, VecDeque<MidPricePoint>>,
    /// Maximum history size per market.
    max_size: usize,
}

#[derive(Debug, Clone)]
struct MidPricePoint {
    visible_ts_ns: i64,
    mid_price: f64,
}

impl MidPriceTracker {
    pub fn new(max_size: usize) -> Self {
        Self {
            markets: BTreeMap::new(),
            max_size,
        }
    }

    /// Record a mid price observation.
    pub fn record(&mut self, market_id: &str, visible_ts_ns: i64, mid_price: f64) {
        let history = self
            .markets
            .entry(market_id.to_string())
            .or_insert_with(VecDeque::new);

        history.push_back(MidPricePoint {
            visible_ts_ns,
            mid_price,
        });

        while history.len() > self.max_size {
            history.pop_front();
        }
    }

    /// Get mid price at or before a given timestamp.
    pub fn get_mid_at(&self, market_id: &str, ts: i64) -> Option<f64> {
        self.markets.get(market_id).and_then(|history| {
            history
                .iter()
                .filter(|p| p.visible_ts_ns <= ts)
                .last()
                .map(|p| p.mid_price)
        })
    }

    /// Get mid price at or after a given timestamp.
    pub fn get_mid_after(&self, market_id: &str, ts: i64) -> Option<f64> {
        self.markets.get(market_id).and_then(|history| {
            history
                .iter()
                .filter(|p| p.visible_ts_ns >= ts)
                .next()
                .map(|p| p.mid_price)
        })
    }

    /// Get mid prices at multiple horizons after a timestamp.
    pub fn get_mids_at_horizons(
        &self,
        market_id: &str,
        base_ts: i64,
        horizons_ns: &[i64],
    ) -> Vec<(i64, Option<f64>)> {
        horizons_ns
            .iter()
            .map(|h| (*h, self.get_mid_after(market_id, base_ts + h)))
            .collect()
    }

    /// Clear all history.
    pub fn clear(&mut self) {
        self.markets.clear();
    }
}

// =============================================================================
// ATTRIBUTION ENGINE
// =============================================================================

/// Configuration for attribution engine.
#[derive(Debug, Clone)]
pub struct AttributionConfig {
    /// Enable attribution (can be disabled for performance).
    pub enabled: bool,
    /// Window for nearby print search (nanoseconds).
    pub nearby_window_ns: i64,
    /// Horizons for mid-move analysis (nanoseconds).
    pub mid_move_horizons_ns: Vec<i64>,
    /// Buffer size for trade prints per market.
    pub print_buffer_size: usize,
    /// Buffer size for mid price history per market.
    pub mid_buffer_size: usize,
    /// Threshold for adverse selection detection (basis points).
    pub adverse_selection_threshold_bps: f64,
}

impl Default for AttributionConfig {
    fn default() -> Self {
        Self {
            enabled: false, // Off by default for performance
            nearby_window_ns: DEFAULT_NEARBY_WINDOW_NS,
            mid_move_horizons_ns: DEFAULT_MID_MOVE_HORIZONS_NS.to_vec(),
            print_buffer_size: DEFAULT_BUFFER_SIZE,
            mid_buffer_size: DEFAULT_BUFFER_SIZE,
            adverse_selection_threshold_bps: 10.0,
        }
    }
}

/// Attribution engine for tracking and analyzing fills against market activity.
pub struct AttributionEngine {
    config: AttributionConfig,
    print_buffer: TradePrintBuffer,
    mid_tracker: MidPriceTracker,
    /// Statistics
    fills_attributed: u64,
    fills_skipped: u64,
    adverse_selection_count: u64,
}

impl AttributionEngine {
    pub fn new(config: AttributionConfig) -> Self {
        let print_buffer = TradePrintBuffer::new(config.print_buffer_size);
        let mid_tracker = MidPriceTracker::new(config.mid_buffer_size);

        Self {
            config,
            print_buffer,
            mid_tracker,
            fills_attributed: 0,
            fills_skipped: 0,
            adverse_selection_count: 0,
        }
    }

    /// Check if attribution is enabled.
    pub fn is_enabled(&self) -> bool {
        self.config.enabled
    }

    /// Process a trade print (add to buffer).
    pub fn on_trade_print(&mut self, print: &PolymarketTradePrint) {
        if !self.config.enabled {
            return;
        }
        self.print_buffer.push(print);
    }

    /// Record a mid price update.
    pub fn on_mid_update(&mut self, market_id: &str, visible_ts_ns: i64, mid_price: f64) {
        if !self.config.enabled {
            return;
        }
        self.mid_tracker.record(market_id, visible_ts_ns, mid_price);
    }

    /// Attribute a fill to nearby market activity.
    pub fn attribute_fill(
        &mut self,
        fill_id: String,
        order_id: u64,
        market_id: &str,
        side: Side,
        fill_price: f64,
        fill_size: f64,
        fill_ts: i64,
        is_maker: bool,
    ) -> FillAttributionReport {
        if !self.config.enabled {
            self.fills_skipped += 1;
            return FillAttributionReport::incomplete(
                fill_id,
                order_id,
                market_id.to_string(),
                side,
                fill_price,
                fill_size,
                fill_ts,
                is_maker,
                "Attribution disabled",
            );
        }

        // Get nearby prints
        let nearby_context = self.compute_nearby_context(market_id, fill_ts);

        // Get mid-move metrics
        let mid_move = self.compute_mid_move(market_id, fill_ts, side);

        // Compute slippage
        let (slippage_price, slippage_bps) = if let Some(mid) = mid_move.mid_at_fill {
            let slip_price = match side {
                Side::Buy => fill_price - mid,
                Side::Sell => mid - fill_price,
            };
            let slip_bps = if mid > 0.0 {
                (slip_price / mid) * 10_000.0
            } else {
                0.0
            };
            (Some(slip_price), Some(slip_bps))
        } else {
            (None, None)
        };

        // Determine completeness
        let mut incomplete_reasons = Vec::new();
        if mid_move.mid_at_fill.is_none() {
            incomplete_reasons.push("Mid price at fill not available".to_string());
        }
        if !nearby_context.has_prints() {
            incomplete_reasons.push("No nearby trade prints".to_string());
        }

        let attribution_complete = incomplete_reasons.is_empty();

        // Track adverse selection
        if mid_move.adverse_selection_detected {
            self.adverse_selection_count += 1;
        }

        self.fills_attributed += 1;

        let trade_volume = nearby_context.total_volume();

        FillAttributionReport {
            fill_id,
            order_id,
            market_id: market_id.to_string(),
            side,
            fill_price,
            fill_size,
            fill_ts,
            is_maker,
            nearby_prints: nearby_context,
            mid_move,
            slippage_price,
            slippage_bps,
            trade_volume_during_fill: trade_volume,
            attribution_complete,
            incomplete_reasons,
        }
    }

    fn compute_nearby_context(&self, market_id: &str, fill_ts: i64) -> NearbyPrintContext {
        let prints = self.print_buffer.get_prints_in_window(
            market_id,
            fill_ts,
            self.config.nearby_window_ns,
        );

        let mut ctx = NearbyPrintContext {
            fill_ts,
            window_ns: self.config.nearby_window_ns,
            ..Default::default()
        };

        let mut notional_before = 0.0;
        let mut notional_after = 0.0;

        for print in &prints {
            let notional = print.price * print.size;

            if print.visible_ts_ns < fill_ts {
                ctx.prints_before += 1;
                ctx.volume_before += print.size;
                notional_before += notional;
            } else {
                ctx.prints_after += 1;
                ctx.volume_after += print.size;
                notional_after += notional;
            }

            if ctx.first_print_ts.is_none() || print.visible_ts_ns < ctx.first_print_ts.unwrap() {
                ctx.first_print_ts = Some(print.visible_ts_ns);
            }
            if ctx.last_print_ts.is_none() || print.visible_ts_ns > ctx.last_print_ts.unwrap() {
                ctx.last_print_ts = Some(print.visible_ts_ns);
            }
        }

        ctx.notional_before = notional_before;
        ctx.notional_after = notional_after;

        if ctx.volume_before > 0.0 {
            ctx.vwap_before = Some(notional_before / ctx.volume_before);
        }
        if ctx.volume_after > 0.0 {
            ctx.vwap_after = Some(notional_after / ctx.volume_after);
        }

        ctx
    }

    fn compute_mid_move(&self, market_id: &str, fill_ts: i64, fill_side: Side) -> MidMoveMetrics {
        let mid_at_fill = self.mid_tracker.get_mid_at(market_id, fill_ts);

        let horizons_with_mids = self.mid_tracker.get_mids_at_horizons(
            market_id,
            fill_ts,
            &self.config.mid_move_horizons_ns,
        );

        let mut metrics = MidMoveMetrics {
            mid_at_fill,
            ..Default::default()
        };

        let mut worst_adverse_bps: Option<f64> = None;
        let mut worst_adverse_horizon: Option<i64> = None;

        for (horizon_ns, mid_after) in horizons_with_mids {
            let (move_bps, is_adverse) = match (mid_at_fill, mid_after) {
                (Some(mid_fill), Some(mid_h)) if mid_fill > 0.0 => {
                    let move_bps = ((mid_h - mid_fill) / mid_fill) * 10_000.0;
                    // Adverse for buy: price went up after we bought
                    // Adverse for sell: price went down after we sold
                    let is_adverse = match fill_side {
                        Side::Buy => move_bps < -self.config.adverse_selection_threshold_bps,
                        Side::Sell => move_bps > self.config.adverse_selection_threshold_bps,
                    };
                    (Some(move_bps), is_adverse)
                }
                _ => (None, false),
            };

            metrics.mid_after_horizons.push(MidMoveAtHorizon {
                horizon_ns,
                mid_price: mid_after,
                move_bps,
                is_adverse,
            });

            if is_adverse {
                metrics.adverse_selection_detected = true;
                let adverse_magnitude = move_bps.map(|m| m.abs());
                if adverse_magnitude > worst_adverse_bps {
                    worst_adverse_bps = adverse_magnitude;
                    worst_adverse_horizon = Some(horizon_ns);
                }
            }
        }

        metrics.adverse_selection_bps = worst_adverse_bps;
        metrics.worst_adverse_horizon_ns = worst_adverse_horizon;

        metrics
    }

    /// Get attribution statistics.
    pub fn stats(&self) -> AttributionStats {
        AttributionStats {
            enabled: self.config.enabled,
            fills_attributed: self.fills_attributed,
            fills_skipped: self.fills_skipped,
            adverse_selection_count: self.adverse_selection_count,
            print_buffer_size: self.print_buffer.total_prints(),
        }
    }

    /// Reset the engine for a new run.
    pub fn reset(&mut self) {
        self.print_buffer.clear();
        self.mid_tracker.clear();
        self.fills_attributed = 0;
        self.fills_skipped = 0;
        self.adverse_selection_count = 0;
    }
}

/// Attribution statistics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttributionStats {
    pub enabled: bool,
    pub fills_attributed: u64,
    pub fills_skipped: u64,
    pub adverse_selection_count: u64,
    pub print_buffer_size: usize,
}

// =============================================================================
// TESTS
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backtest_v2::trade_print::{TradePrintBuilder, TradeIdSource, AggressorSideSource};

    fn make_print(market_id: &str, visible_ts: i64, price: f64, size: f64) -> PolymarketTradePrint {
        TradePrintBuilder::new()
            .market_id(market_id)
            .token_id("token")
            .trade_id(format!("trade_{}", visible_ts), TradeIdSource::NativeVenueId)
            .aggressor_side(Side::Buy, AggressorSideSource::VenueProvided)
            .price(price)
            .size(size)
            .ingest_ts_ns(visible_ts - 1000)
            .build()
            .map(|mut p| {
                p.visible_ts_ns = visible_ts;
                p
            })
            .unwrap()
    }

    #[test]
    fn test_print_buffer() {
        let mut buffer = TradePrintBuffer::new(100);

        buffer.push(&make_print("market_1", 1_000_000_000, 0.50, 100.0));
        buffer.push(&make_print("market_1", 2_000_000_000, 0.51, 50.0));
        buffer.push(&make_print("market_1", 3_000_000_000, 0.52, 75.0));

        let prints = buffer.get_prints_in_window("market_1", 2_000_000_000, 1_500_000_000);
        assert_eq!(prints.len(), 3);
    }

    #[test]
    fn test_mid_tracker() {
        let mut tracker = MidPriceTracker::new(100);

        tracker.record("market_1", 1_000_000_000, 0.50);
        tracker.record("market_1", 2_000_000_000, 0.51);
        tracker.record("market_1", 3_000_000_000, 0.52);

        assert_eq!(tracker.get_mid_at("market_1", 2_000_000_000), Some(0.51));
        assert_eq!(tracker.get_mid_at("market_1", 1_500_000_000), Some(0.50));
        assert_eq!(tracker.get_mid_after("market_1", 2_500_000_000), Some(0.52));
    }

    #[test]
    fn test_attribution_disabled() {
        let config = AttributionConfig::default(); // disabled by default
        let mut engine = AttributionEngine::new(config);

        let report = engine.attribute_fill(
            "fill_1".to_string(),
            1,
            "market_1",
            Side::Buy,
            0.51,
            100.0,
            2_000_000_000,
            false,
        );

        assert!(!report.attribution_complete);
        assert!(report.incomplete_reasons.iter().any(|r| r.contains("disabled")));
    }

    #[test]
    fn test_attribution_with_prints() {
        let config = AttributionConfig {
            enabled: true,
            ..Default::default()
        };
        let mut engine = AttributionEngine::new(config);

        // Add trade prints
        engine.on_trade_print(&make_print("market_1", 1_800_000_000, 0.50, 100.0));
        engine.on_trade_print(&make_print("market_1", 1_900_000_000, 0.505, 50.0));
        engine.on_trade_print(&make_print("market_1", 2_100_000_000, 0.51, 75.0));

        // Add mid prices
        engine.on_mid_update("market_1", 1_800_000_000, 0.50);
        engine.on_mid_update("market_1", 2_000_000_000, 0.505);
        engine.on_mid_update("market_1", 2_100_000_000, 0.51);

        let report = engine.attribute_fill(
            "fill_1".to_string(),
            1,
            "market_1",
            Side::Buy,
            0.505,
            100.0,
            2_000_000_000,
            false,
        );

        assert!(report.nearby_prints.has_prints());
        assert_eq!(report.nearby_prints.prints_before, 2);
        assert_eq!(report.nearby_prints.prints_after, 1);
        assert!(report.mid_move.mid_at_fill.is_some());
    }

    #[test]
    fn test_adverse_selection_detection() {
        let config = AttributionConfig {
            enabled: true,
            mid_move_horizons_ns: vec![1_000_000_000],
            adverse_selection_threshold_bps: 10.0,
            ..Default::default()
        };
        let mut engine = AttributionEngine::new(config);

        // Mid moves against buyer after fill
        engine.on_mid_update("market_1", 2_000_000_000, 0.50);
        engine.on_mid_update("market_1", 3_000_000_000, 0.49); // Price dropped (adverse for buyer)

        let report = engine.attribute_fill(
            "fill_1".to_string(),
            1,
            "market_1",
            Side::Buy,
            0.50,
            100.0,
            2_000_000_000,
            false,
        );

        // -200 bps move should trigger adverse selection
        assert!(report.mid_move.adverse_selection_detected);
        assert!(report.mid_move.adverse_selection_bps.is_some());
    }

    #[test]
    fn test_slippage_calculation() {
        let config = AttributionConfig {
            enabled: true,
            ..Default::default()
        };
        let mut engine = AttributionEngine::new(config);

        engine.on_mid_update("market_1", 2_000_000_000, 0.50);

        // Buy at 0.51 when mid is 0.50 = positive slippage (paid more)
        let report = engine.attribute_fill(
            "fill_1".to_string(),
            1,
            "market_1",
            Side::Buy,
            0.51,
            100.0,
            2_000_000_000,
            false,
        );

        assert!(report.slippage_price.is_some());
        assert!((report.slippage_price.unwrap() - 0.01).abs() < 1e-9);
        // 0.01 / 0.50 * 10000 = 200 bps
        assert!((report.slippage_bps.unwrap() - 200.0).abs() < 1.0);
    }

    #[test]
    fn test_nearby_context_vwap() {
        let mut buffer = TradePrintBuffer::new(100);

        // Two prints: 100 @ 0.50 and 200 @ 0.60
        let mut p1 = make_print("market_1", 1_900_000_000, 0.50, 100.0);
        let mut p2 = make_print("market_1", 1_950_000_000, 0.60, 200.0);

        buffer.push(&p1);
        buffer.push(&p2);

        let prints = buffer.get_prints_in_window("market_1", 2_000_000_000, 1_000_000_000);
        assert_eq!(prints.len(), 2);

        // VWAP = (100*0.50 + 200*0.60) / 300 = 170/300 = 0.5667
        let total_notional: f64 = prints.iter().map(|p| p.price * p.size).sum();
        let total_volume: f64 = prints.iter().map(|p| p.size).sum();
        let vwap = total_notional / total_volume;

        assert!((vwap - 0.5667).abs() < 0.001);
    }
}

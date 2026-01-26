//! Canonical Time-Indexed Equity Curve
//!
//! This module exposes a reproducible equity curve derived from the ledger state.
//! The equity curve reflects the true economic state of the backtest over time.
//!
//! # Design Principles
//!
//! 1. **Ledger-derived**: Equity is computed from the same accounting primitives
//!    that enforce `strict_accounting`. No ad-hoc PnL accumulation.
//! 2. **Economically meaningful**: Points are recorded only when economic state changes
//!    (fills, fees, settlements), not on every event.
//! 3. **Fixed-point**: Uses the same fixed-point arithmetic as the ledger (AMOUNT_SCALE).
//! 4. **Deterministic**: Same inputs produce identical equity curves.
//! 5. **Time-indexed**: Strictly increasing by `time_ns`.
//!
//! # Equity Calculation
//!
//! ```text
//! Equity = Cash + MarkedPositionValue
//! ```
//!
//! Where:
//! - Cash = ledger cash balance
//! - MarkedPositionValue = sum of (position_qty * settlement_price) for all positions
//!   (using $1.00 for positions, as they settle at $0 or $1)

use crate::backtest_v2::clock::Nanos;
use crate::backtest_v2::ledger::{Amount, Ledger, AMOUNT_SCALE, from_amount, to_amount};
use crate::backtest_v2::portfolio::Outcome;
use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

// =============================================================================
// EQUITY POINT
// =============================================================================

/// A single point on the equity curve.
///
/// All values are in fixed-point using AMOUNT_SCALE for consistency with the ledger.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EquityPoint {
    /// Simulation time (nanoseconds) when this observation was recorded.
    pub time_ns: Nanos,
    
    /// Total equity value at this time.
    /// Equity = cash_balance + position_value
    pub equity_value: Amount,
    
    /// Cash balance component.
    pub cash_balance: Amount,
    
    /// Mark-to-market position value component.
    /// For binary markets, this is sum of (qty * mid_price) in fixed-point.
    pub position_value: Amount,
    
    /// Optional: drawdown from peak equity so far.
    /// Computed incrementally during recording.
    pub drawdown_value: Amount,
    
    /// Optional: drawdown as a percentage of peak (scaled by 10000 for 0.01% precision).
    /// E.g., 500 = 5.00% drawdown
    pub drawdown_bps: i64,
}

impl EquityPoint {
    /// Create a new equity point.
    pub fn new(
        time_ns: Nanos,
        equity_value: Amount,
        cash_balance: Amount,
        position_value: Amount,
    ) -> Self {
        Self {
            time_ns,
            equity_value,
            cash_balance,
            position_value,
            drawdown_value: 0,
            drawdown_bps: 0,
        }
    }
    
    /// Create a new equity point with drawdown information.
    pub fn with_drawdown(
        time_ns: Nanos,
        equity_value: Amount,
        cash_balance: Amount,
        position_value: Amount,
        drawdown_value: Amount,
        drawdown_bps: i64,
    ) -> Self {
        Self {
            time_ns,
            equity_value,
            cash_balance,
            position_value,
            drawdown_value,
            drawdown_bps,
        }
    }
    
    /// Get equity as f64.
    pub fn equity_f64(&self) -> f64 {
        from_amount(self.equity_value)
    }
    
    /// Get cash as f64.
    pub fn cash_f64(&self) -> f64 {
        from_amount(self.cash_balance)
    }
    
    /// Get position value as f64.
    pub fn position_value_f64(&self) -> f64 {
        from_amount(self.position_value)
    }
    
    /// Get drawdown as f64.
    pub fn drawdown_f64(&self) -> f64 {
        from_amount(self.drawdown_value)
    }
    
    /// Get drawdown percentage.
    pub fn drawdown_pct(&self) -> f64 {
        self.drawdown_bps as f64 / 10000.0
    }
}

// =============================================================================
// EQUITY CURVE
// =============================================================================

/// A canonical, time-indexed equity curve.
///
/// Invariants:
/// - Points are strictly increasing by `time_ns`.
/// - First point is the initial equity (at initial deposit).
/// - Last point equity equals `BacktestResults.final_equity`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EquityCurve {
    /// Ordered equity points, strictly increasing by time_ns.
    points: Vec<EquityPoint>,
    
    /// Peak equity seen so far (for drawdown calculation).
    peak_equity: Amount,
    
    /// Rolling hash for fingerprinting (updated on each point).
    rolling_hash: u64,
}

impl EquityCurve {
    /// Initial seed for rolling hash.
    const HASH_SEED: u64 = 0xECEC_ECEC_ECEC_ECEC;
    
    /// Create a new empty equity curve.
    pub fn new() -> Self {
        Self {
            points: Vec::new(),
            peak_equity: 0,
            rolling_hash: Self::HASH_SEED,
        }
    }
    
    /// Create with pre-allocated capacity.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            points: Vec::with_capacity(capacity),
            peak_equity: 0,
            rolling_hash: Self::HASH_SEED,
        }
    }
    
    /// Record an equity point.
    ///
    /// # Panics
    ///
    /// Panics if `time_ns` is not strictly greater than the last recorded time.
    /// In production-grade mode, this would abort the backtest.
    pub fn record(&mut self, time_ns: Nanos, equity_value: Amount, cash_balance: Amount, position_value: Amount) {
        // Enforce time monotonicity
        if let Some(last) = self.points.last() {
            assert!(
                time_ns > last.time_ns,
                "EquityCurve time_ns must be strictly increasing: {} <= {}",
                time_ns,
                last.time_ns
            );
        }
        
        // Update peak equity
        if equity_value > self.peak_equity {
            self.peak_equity = equity_value;
        }
        
        // Calculate drawdown
        let drawdown_value = self.peak_equity - equity_value;
        let drawdown_bps = if self.peak_equity > 0 {
            (drawdown_value * 10000 / self.peak_equity) as i64
        } else {
            0
        };
        
        let point = EquityPoint::with_drawdown(
            time_ns,
            equity_value,
            cash_balance,
            position_value,
            drawdown_value,
            drawdown_bps,
        );
        
        // Update rolling hash
        self.update_hash(&point);
        
        self.points.push(point);
    }
    
    /// Record an equity point, allowing non-strictly-increasing time.
    /// Returns false if the time was rejected (not strictly greater).
    pub fn try_record(&mut self, time_ns: Nanos, equity_value: Amount, cash_balance: Amount, position_value: Amount) -> bool {
        if let Some(last) = self.points.last() {
            if time_ns <= last.time_ns {
                return false;
            }
        }
        self.record(time_ns, equity_value, cash_balance, position_value);
        true
    }
    
    fn update_hash(&mut self, point: &EquityPoint) {
        let mut hasher = DefaultHasher::new();
        self.rolling_hash.hash(&mut hasher);
        point.time_ns.hash(&mut hasher);
        point.equity_value.hash(&mut hasher);
        self.rolling_hash = hasher.finish();
    }
    
    /// Get all points.
    pub fn points(&self) -> &[EquityPoint] {
        &self.points
    }
    
    /// Get the number of points.
    pub fn len(&self) -> usize {
        self.points.len()
    }
    
    /// Check if empty.
    pub fn is_empty(&self) -> bool {
        self.points.is_empty()
    }
    
    /// Get the first point.
    pub fn first(&self) -> Option<&EquityPoint> {
        self.points.first()
    }
    
    /// Get the last point.
    pub fn last(&self) -> Option<&EquityPoint> {
        self.points.last()
    }
    
    /// Get the initial equity (first point).
    pub fn initial_equity(&self) -> Option<Amount> {
        self.first().map(|p| p.equity_value)
    }
    
    /// Get the final equity (last point).
    pub fn final_equity(&self) -> Option<Amount> {
        self.last().map(|p| p.equity_value)
    }
    
    /// Get the peak equity.
    pub fn peak_equity(&self) -> Amount {
        self.peak_equity
    }
    
    /// Get the maximum drawdown in fixed-point.
    pub fn max_drawdown(&self) -> Amount {
        self.points.iter().map(|p| p.drawdown_value).max().unwrap_or(0)
    }
    
    /// Get the maximum drawdown in basis points.
    pub fn max_drawdown_bps(&self) -> i64 {
        self.points.iter().map(|p| p.drawdown_bps).max().unwrap_or(0)
    }
    
    /// Get the rolling hash for fingerprinting.
    pub fn rolling_hash(&self) -> u64 {
        self.rolling_hash
    }
    
    /// Verify time monotonicity invariant.
    pub fn verify_monotonicity(&self) -> bool {
        for window in self.points.windows(2) {
            if window[0].time_ns >= window[1].time_ns {
                return false;
            }
        }
        true
    }
    
    /// Verify that final equity matches expected value.
    pub fn verify_final_equity(&self, expected: Amount, tolerance: Amount) -> bool {
        if let Some(last) = self.last() {
            (last.equity_value - expected).abs() <= tolerance
        } else {
            false
        }
    }
    
    /// Convert to f64 points for plotting.
    pub fn to_plot_data(&self) -> Vec<(f64, f64)> {
        self.points
            .iter()
            .map(|p| (p.time_ns as f64 / 1e9, p.equity_f64()))
            .collect()
    }
    
    /// Compute equity returns between points.
    pub fn returns(&self) -> Vec<f64> {
        if self.points.len() < 2 {
            return Vec::new();
        }
        
        self.points
            .windows(2)
            .map(|w| {
                let prev = from_amount(w[0].equity_value);
                let curr = from_amount(w[1].equity_value);
                if prev.abs() > 1e-10 {
                    (curr - prev) / prev
                } else {
                    0.0
                }
            })
            .collect()
    }
}

// =============================================================================
// EQUITY RECORDER
// =============================================================================

/// Reason for an equity observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EquityObservationTrigger {
    /// Initial deposit / start of run.
    InitialDeposit,
    /// A fill occurred.
    Fill,
    /// A fee was posted.
    Fee,
    /// A settlement occurred.
    Settlement,
    /// End of run finalization.
    Finalization,
}

/// Records equity observations at economically meaningful times.
///
/// The recorder is fed by the orchestrator when:
/// - Initial deposit is made
/// - After each fill (and fee) is posted to the ledger
/// - After each settlement is posted to the ledger
/// - At end-of-run finalization
pub struct EquityRecorder {
    /// The accumulated equity curve.
    curve: EquityCurve,
    
    /// Last recorded time (to skip duplicate observations at same time).
    last_time_ns: Nanos,
    
    /// Track number of observations by trigger type.
    trigger_counts: [u64; 5],
}

impl EquityRecorder {
    /// Create a new recorder.
    pub fn new() -> Self {
        Self {
            curve: EquityCurve::with_capacity(10_000),
            last_time_ns: 0,
            trigger_counts: [0; 5],
        }
    }
    
    /// Create with custom capacity.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            curve: EquityCurve::with_capacity(capacity),
            last_time_ns: 0,
            trigger_counts: [0; 5],
        }
    }
    
    /// Record an equity observation from ledger state.
    ///
    /// # Arguments
    ///
    /// * `time_ns` - The simulation time of the observation.
    /// * `ledger` - The ledger to read state from.
    /// * `mid_prices` - Map of market_id to mid price for position valuation.
    /// * `trigger` - What caused this observation.
    ///
    /// # Returns
    ///
    /// `true` if the observation was recorded, `false` if skipped (same time).
    pub fn observe(
        &mut self,
        time_ns: Nanos,
        ledger: &Ledger,
        mid_prices: &std::collections::HashMap<String, f64>,
        trigger: EquityObservationTrigger,
    ) -> bool {
        // Skip if we already have an observation at this time
        // (multiple events can occur at the same nanosecond)
        if time_ns <= self.last_time_ns && !self.curve.is_empty() {
            return false;
        }
        
        // Compute cash balance from ledger
        let cash = ledger.get_balance(&crate::backtest_v2::ledger::LedgerAccount::Cash);
        
        // Compute position value (mark-to-market)
        let mut position_value: Amount = 0;
        
        // Iterate through all markets in mid_prices
        for (market_id, mid) in mid_prices {
            let yes_qty = ledger.position_qty(market_id, Outcome::Yes);
            let no_qty = ledger.position_qty(market_id, Outcome::No);
            
            // Position value = yes_qty * mid + no_qty * (1 - mid)
            // Convert to fixed-point
            let yes_value = to_amount(yes_qty * *mid);
            let no_value = to_amount(no_qty * (1.0 - *mid));
            position_value += yes_value + no_value;
        }
        
        // Total equity
        let equity_value = cash + position_value;
        
        // Record the point
        self.curve.record(time_ns, equity_value, cash, position_value);
        self.last_time_ns = time_ns;
        
        // Update trigger count
        let idx = match trigger {
            EquityObservationTrigger::InitialDeposit => 0,
            EquityObservationTrigger::Fill => 1,
            EquityObservationTrigger::Fee => 2,
            EquityObservationTrigger::Settlement => 3,
            EquityObservationTrigger::Finalization => 4,
        };
        self.trigger_counts[idx] += 1;
        
        true
    }
    
    /// Record an equity observation with explicit values (for testing or non-ledger use).
    pub fn observe_explicit(
        &mut self,
        time_ns: Nanos,
        equity_value: Amount,
        cash_balance: Amount,
        position_value: Amount,
        trigger: EquityObservationTrigger,
    ) -> bool {
        if time_ns <= self.last_time_ns && !self.curve.is_empty() {
            return false;
        }
        
        self.curve.record(time_ns, equity_value, cash_balance, position_value);
        self.last_time_ns = time_ns;
        
        let idx = match trigger {
            EquityObservationTrigger::InitialDeposit => 0,
            EquityObservationTrigger::Fill => 1,
            EquityObservationTrigger::Fee => 2,
            EquityObservationTrigger::Settlement => 3,
            EquityObservationTrigger::Finalization => 4,
        };
        self.trigger_counts[idx] += 1;
        
        true
    }
    
    /// Get the recorded equity curve.
    pub fn curve(&self) -> &EquityCurve {
        &self.curve
    }
    
    /// Take ownership of the equity curve.
    pub fn into_curve(self) -> EquityCurve {
        self.curve
    }
    
    /// Get the last recorded time.
    pub fn last_time_ns(&self) -> Nanos {
        self.last_time_ns
    }
    
    /// Get statistics about observation triggers.
    pub fn stats(&self) -> EquityRecorderStats {
        EquityRecorderStats {
            total_observations: self.curve.len() as u64,
            initial_deposits: self.trigger_counts[0],
            fills: self.trigger_counts[1],
            fees: self.trigger_counts[2],
            settlements: self.trigger_counts[3],
            finalizations: self.trigger_counts[4],
        }
    }
}

impl Default for EquityRecorder {
    fn default() -> Self {
        Self::new()
    }
}

/// Statistics about equity observations.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EquityRecorderStats {
    pub total_observations: u64,
    pub initial_deposits: u64,
    pub fills: u64,
    pub fees: u64,
    pub settlements: u64,
    pub finalizations: u64,
}

// =============================================================================
// EQUITY CURVE SUMMARY
// =============================================================================

/// Summary statistics for an equity curve.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EquityCurveSummary {
    /// Number of points in the curve.
    pub point_count: usize,
    
    /// Initial equity (f64).
    pub initial_equity: f64,
    
    /// Final equity (f64).
    pub final_equity: f64,
    
    /// Peak equity (f64).
    pub peak_equity: f64,
    
    /// Total return (final / initial - 1).
    pub total_return: f64,
    
    /// Maximum drawdown (f64).
    pub max_drawdown: f64,
    
    /// Maximum drawdown percentage.
    pub max_drawdown_pct: f64,
    
    /// Rolling hash for fingerprinting.
    pub rolling_hash: u64,
    
    /// Start time (ns).
    pub start_time_ns: Nanos,
    
    /// End time (ns).
    pub end_time_ns: Nanos,
}

impl EquityCurveSummary {
    /// Create summary from an equity curve.
    pub fn from_curve(curve: &EquityCurve) -> Self {
        let initial = curve.initial_equity().unwrap_or(0);
        let final_eq = curve.final_equity().unwrap_or(0);
        let peak = curve.peak_equity();
        
        let initial_f64 = from_amount(initial);
        let final_f64 = from_amount(final_eq);
        let peak_f64 = from_amount(peak);
        
        let total_return = if initial_f64.abs() > 1e-10 {
            (final_f64 - initial_f64) / initial_f64
        } else {
            0.0
        };
        
        let max_dd = from_amount(curve.max_drawdown());
        let max_dd_pct = curve.max_drawdown_bps() as f64 / 10000.0;
        
        Self {
            point_count: curve.len(),
            initial_equity: initial_f64,
            final_equity: final_f64,
            peak_equity: peak_f64,
            total_return,
            max_drawdown: max_dd,
            max_drawdown_pct: max_dd_pct,
            rolling_hash: curve.rolling_hash(),
            start_time_ns: curve.first().map(|p| p.time_ns).unwrap_or(0),
            end_time_ns: curve.last().map(|p| p.time_ns).unwrap_or(0),
        }
    }
}

// =============================================================================
// TESTS
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_equity_point_creation() {
        let point = EquityPoint::new(
            1_000_000_000, // 1 second
            to_amount(10000.0),
            to_amount(9000.0),
            to_amount(1000.0),
        );
        
        assert_eq!(point.time_ns, 1_000_000_000);
        assert!((point.equity_f64() - 10000.0).abs() < 0.01);
        assert!((point.cash_f64() - 9000.0).abs() < 0.01);
        assert!((point.position_value_f64() - 1000.0).abs() < 0.01);
    }
    
    #[test]
    fn test_equity_curve_basic() {
        let mut curve = EquityCurve::new();
        
        curve.record(1000, to_amount(10000.0), to_amount(10000.0), 0);
        curve.record(2000, to_amount(10100.0), to_amount(9500.0), to_amount(600.0));
        curve.record(3000, to_amount(10050.0), to_amount(9450.0), to_amount(600.0));
        
        assert_eq!(curve.len(), 3);
        assert!(curve.verify_monotonicity());
        
        // Peak should be 10100
        assert!((from_amount(curve.peak_equity()) - 10100.0).abs() < 0.01);
        
        // Max drawdown should be 50 (10100 - 10050)
        assert!((from_amount(curve.max_drawdown()) - 50.0).abs() < 0.01);
    }
    
    #[test]
    fn test_equity_curve_monotonicity_enforced() {
        let mut curve = EquityCurve::new();
        
        curve.record(1000, to_amount(10000.0), to_amount(10000.0), 0);
        
        // This should return false (same time)
        let result = curve.try_record(1000, to_amount(10100.0), to_amount(10100.0), 0);
        assert!(!result);
        
        // This should return false (earlier time)
        let result = curve.try_record(500, to_amount(10100.0), to_amount(10100.0), 0);
        assert!(!result);
        
        // This should succeed
        let result = curve.try_record(2000, to_amount(10100.0), to_amount(10100.0), 0);
        assert!(result);
        
        assert_eq!(curve.len(), 2);
    }
    
    #[test]
    #[should_panic(expected = "strictly increasing")]
    fn test_equity_curve_panics_on_non_monotonic() {
        let mut curve = EquityCurve::new();
        
        curve.record(2000, to_amount(10000.0), to_amount(10000.0), 0);
        curve.record(1000, to_amount(10100.0), to_amount(10100.0), 0); // Should panic
    }
    
    #[test]
    fn test_equity_curve_rolling_hash_deterministic() {
        let mut curve1 = EquityCurve::new();
        curve1.record(1000, to_amount(10000.0), to_amount(10000.0), 0);
        curve1.record(2000, to_amount(10100.0), to_amount(10100.0), 0);
        
        let mut curve2 = EquityCurve::new();
        curve2.record(1000, to_amount(10000.0), to_amount(10000.0), 0);
        curve2.record(2000, to_amount(10100.0), to_amount(10100.0), 0);
        
        assert_eq!(curve1.rolling_hash(), curve2.rolling_hash());
    }
    
    #[test]
    fn test_equity_curve_rolling_hash_changes_on_different_data() {
        let mut curve1 = EquityCurve::new();
        curve1.record(1000, to_amount(10000.0), to_amount(10000.0), 0);
        curve1.record(2000, to_amount(10100.0), to_amount(10100.0), 0);
        
        let mut curve2 = EquityCurve::new();
        curve2.record(1000, to_amount(10000.0), to_amount(10000.0), 0);
        curve2.record(2000, to_amount(10200.0), to_amount(10200.0), 0); // Different equity
        
        assert_ne!(curve1.rolling_hash(), curve2.rolling_hash());
    }
    
    #[test]
    fn test_equity_recorder_basic() {
        let mut recorder = EquityRecorder::new();
        
        recorder.observe_explicit(
            1000,
            to_amount(10000.0),
            to_amount(10000.0),
            0,
            EquityObservationTrigger::InitialDeposit,
        );
        
        recorder.observe_explicit(
            2000,
            to_amount(9950.0),
            to_amount(9400.0),
            to_amount(550.0),
            EquityObservationTrigger::Fill,
        );
        
        recorder.observe_explicit(
            2001, // Just after fill for fee
            to_amount(9940.0),
            to_amount(9390.0),
            to_amount(550.0),
            EquityObservationTrigger::Fee,
        );
        
        let stats = recorder.stats();
        assert_eq!(stats.total_observations, 3);
        assert_eq!(stats.initial_deposits, 1);
        assert_eq!(stats.fills, 1);
        assert_eq!(stats.fees, 1);
        
        let curve = recorder.curve();
        assert_eq!(curve.len(), 3);
        assert!(curve.verify_monotonicity());
    }
    
    #[test]
    fn test_equity_recorder_skips_duplicate_time() {
        let mut recorder = EquityRecorder::new();
        
        recorder.observe_explicit(
            1000,
            to_amount(10000.0),
            to_amount(10000.0),
            0,
            EquityObservationTrigger::InitialDeposit,
        );
        
        // Same time - should be skipped
        let result = recorder.observe_explicit(
            1000,
            to_amount(10100.0),
            to_amount(10100.0),
            0,
            EquityObservationTrigger::Fill,
        );
        
        assert!(!result);
        assert_eq!(recorder.curve().len(), 1);
    }
    
    #[test]
    fn test_equity_curve_summary() {
        let mut curve = EquityCurve::new();
        
        curve.record(1000, to_amount(10000.0), to_amount(10000.0), 0);
        curve.record(2000, to_amount(10500.0), to_amount(10000.0), to_amount(500.0));
        curve.record(3000, to_amount(10200.0), to_amount(9700.0), to_amount(500.0));
        curve.record(4000, to_amount(10800.0), to_amount(10300.0), to_amount(500.0));
        
        let summary = EquityCurveSummary::from_curve(&curve);
        
        assert_eq!(summary.point_count, 4);
        assert!((summary.initial_equity - 10000.0).abs() < 0.01);
        assert!((summary.final_equity - 10800.0).abs() < 0.01);
        assert!((summary.peak_equity - 10800.0).abs() < 0.01);
        assert!((summary.total_return - 0.08).abs() < 0.001);
        
        // Max drawdown was 300 (10500 -> 10200)
        assert!((summary.max_drawdown - 300.0).abs() < 0.01);
    }
    
    #[test]
    fn test_equity_curve_returns() {
        let mut curve = EquityCurve::new();
        
        curve.record(1000, to_amount(10000.0), to_amount(10000.0), 0);
        curve.record(2000, to_amount(10100.0), to_amount(10100.0), 0);
        curve.record(3000, to_amount(10302.0), to_amount(10302.0), 0);
        
        let returns = curve.returns();
        
        assert_eq!(returns.len(), 2);
        assert!((returns[0] - 0.01).abs() < 0.0001); // 1% return
        assert!((returns[1] - 0.02).abs() < 0.0001); // 2% return
    }
}

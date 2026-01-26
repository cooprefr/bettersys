//! Honesty Metrics for Backtest Results
//!
//! This module computes normalized returns and fee impact ratios that prevent
//! misleading PnL screenshots by surfacing how much of performance is fee-driven
//! and how results scale per window.
//!
//! # Design Principles
//!
//! 1. **Derived from Canonical Sources**: All metrics are computed deterministically
//!    from the ledger and WindowPnLSeries - no ad-hoc counters.
//!
//! 2. **Fixed-Point Arithmetic**: Uses the same Amount type (i128) as the ledger
//!    to avoid floating-point drift. Ratios are computed in fixed-point and only
//!    converted to f64 at the serialization boundary.
//!
//! 3. **Explicit Zero-Guards**: Division by zero is never silent. All ratio fields
//!    use Option<RatioValue> that is None when the denominator is zero.
//!
//! 4. **Identity Enforcement**: The fundamental identity `net_pnl = gross_pnl - fees`
//!    is verified before computing metrics. Violations abort in production mode.
//!
//! # Metric Definitions
//!
//! - `net_over_gross_ratio`: Fraction of gross PnL retained after fees
//! - `fees_over_gross_ratio`: Fraction of gross PnL consumed by fees
//! - `net_pnl_per_window`: Average net PnL per settled window
//! - `net_return_per_notional`: Net PnL per unit of notional traded (optional)

use crate::backtest_v2::ledger::{Amount, AMOUNT_SCALE, from_amount, to_amount};
use crate::backtest_v2::window_pnl::WindowPnLSeries;
use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

/// Scale factor for ratio fixed-point representation.
/// Ratios are stored as i128 with this scale (1.0 = RATIO_SCALE).
pub const RATIO_SCALE: i128 = 1_000_000_000; // 9 decimal places for precision

/// A ratio value with explicit undefined handling.
///
/// Ratios that would require division by zero are represented as `None`.
/// When Some, the value is stored in fixed-point (RATIO_SCALE).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RatioValue {
    /// Fixed-point ratio value (1.0 = RATIO_SCALE).
    pub fixed_point: i128,
    /// Original numerator (for auditability).
    pub numerator: Amount,
    /// Original denominator (for auditability).
    pub denominator: Amount,
}

impl RatioValue {
    /// Create a new ratio value from numerator and denominator.
    /// Returns None if denominator is zero.
    pub fn new(numerator: Amount, denominator: Amount) -> Option<Self> {
        if denominator == 0 {
            return None;
        }
        // Compute ratio in fixed-point: (num * RATIO_SCALE) / denom
        // Use i128 multiplication, then division
        let fixed_point = (numerator as i128)
            .saturating_mul(RATIO_SCALE)
            .checked_div(denominator as i128)?;
        
        Some(Self {
            fixed_point,
            numerator,
            denominator,
        })
    }
    
    /// Get the ratio as f64.
    pub fn as_f64(&self) -> f64 {
        self.fixed_point as f64 / RATIO_SCALE as f64
    }
    
    /// Get the ratio as a percentage (0-100 scale).
    pub fn as_percentage(&self) -> f64 {
        self.as_f64() * 100.0
    }
}

/// Per-window average with explicit undefined handling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PerWindowValue {
    /// Total value (fixed-point Amount).
    pub total: Amount,
    /// Window count (denominator).
    pub windows: u64,
    /// Average per window (fixed-point Amount).
    pub average: Amount,
}

impl PerWindowValue {
    /// Create a new per-window value.
    /// Returns None if windows is zero.
    pub fn new(total: Amount, windows: u64) -> Option<Self> {
        if windows == 0 {
            return None;
        }
        let average = total / windows as i128;
        Some(Self {
            total,
            windows,
            average,
        })
    }
    
    /// Get the average as f64.
    pub fn average_f64(&self) -> f64 {
        from_amount(self.average)
    }
    
    /// Get the total as f64.
    pub fn total_f64(&self) -> f64 {
        from_amount(self.total)
    }
}

/// Honesty metrics computed from canonical accounting outputs.
///
/// These metrics prevent misleading PnL screenshots by providing:
/// 1. Fee impact ratios (how much of gross PnL goes to fees)
/// 2. Normalized returns (net PnL per window)
/// 3. Optional notional-based returns (if notional is well-defined)
///
/// All values use fixed-point arithmetic for determinism.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HonestyMetrics {
    // =========================================================================
    // PRIMARY ACCOUNTING TOTALS (from WindowPnLSeries)
    // =========================================================================
    
    /// Total gross PnL before fees (fixed-point Amount).
    /// Definition: sum(trading_pnl) + sum(settlement_transfer)
    pub total_gross_pnl: Amount,
    
    /// Total fees paid (fixed-point Amount).
    /// Always non-negative.
    pub total_fees: Amount,
    
    /// Total net PnL after fees (fixed-point Amount).
    /// Identity: total_net_pnl = total_gross_pnl - total_fees
    pub total_net_pnl: Amount,
    
    /// Total settlement transfers (fixed-point Amount).
    /// Positive = received from winning positions.
    pub total_settlement: Amount,
    
    // =========================================================================
    // WINDOW STATISTICS
    // =========================================================================
    
    /// Number of windows with trades (active windows).
    pub windows_traded: u64,
    
    /// Number of finalized (settled) windows.
    pub windows_finalized: u64,
    
    /// Total trades across all windows.
    pub total_trades: u64,
    
    // =========================================================================
    // RATIO METRICS (with explicit zero-guards)
    // =========================================================================
    
    /// Net PnL / Gross PnL ratio.
    /// Measures fraction of gross PnL retained after fees.
    /// None if gross_pnl == 0.
    pub net_over_gross_ratio: Option<RatioValue>,
    
    /// Fees / Gross PnL ratio.
    /// Measures fraction of gross PnL consumed by fees.
    /// None if gross_pnl == 0.
    pub fees_over_gross_ratio: Option<RatioValue>,
    
    /// Net PnL per window.
    /// None if windows_traded == 0.
    pub net_pnl_per_window: Option<PerWindowValue>,
    
    /// Gross PnL per window.
    /// None if windows_traded == 0.
    pub gross_pnl_per_window: Option<PerWindowValue>,
    
    /// Fees per window.
    /// None if windows_traded == 0.
    pub fees_per_window: Option<PerWindowValue>,
    
    // =========================================================================
    // OPTIONAL NOTIONAL-BASED METRICS
    // =========================================================================
    
    /// Total notional traded (turnover).
    /// This is sum(|fill_price * fill_size|) over all fills.
    /// Set to None if notional tracking is not enabled or undefined.
    pub total_notional_traded: Option<Amount>,
    
    /// Net return per notional traded.
    /// Definition: total_net_pnl / total_notional_traded
    /// None if notional is undefined or zero.
    pub net_return_per_notional: Option<RatioValue>,
    
    /// Whether notional is well-defined in this backtest.
    pub notional_defined: bool,
    
    /// Reason why notional is undefined (if applicable).
    pub notional_undefined_reason: Option<String>,
    
    // =========================================================================
    // DISTRIBUTION STATISTICS (optional)
    // =========================================================================
    
    /// Window net PnL distribution statistics.
    pub window_pnl_stats: Option<DistributionStats>,
    
    // =========================================================================
    // IDENTITY VERIFICATION
    // =========================================================================
    
    /// Whether the fundamental identity holds: net = gross - fees
    pub identity_verified: bool,
    
    /// Identity verification error (if any).
    pub identity_error: Option<String>,
    
    /// Fingerprint hash of these metrics (for reproducibility).
    pub metrics_hash: u64,
}

/// Distribution statistics for window PnL values.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DistributionStats {
    /// Number of samples.
    pub count: u64,
    /// Mean value (fixed-point).
    pub mean: Amount,
    /// Median value (fixed-point).
    pub median: Amount,
    /// 5th percentile (fixed-point).
    pub p05: Amount,
    /// 95th percentile (fixed-point).
    pub p95: Amount,
    /// Minimum value (fixed-point).
    pub min: Amount,
    /// Maximum value (fixed-point).
    pub max: Amount,
    /// Standard deviation (fixed-point, scaled by AMOUNT_SCALE).
    pub std_dev: Amount,
}

impl DistributionStats {
    /// Compute distribution statistics from a slice of Amount values.
    pub fn from_values(values: &[Amount]) -> Option<Self> {
        if values.is_empty() {
            return None;
        }
        
        let n = values.len() as i128;
        let count = values.len() as u64;
        
        // Compute mean
        let sum: i128 = values.iter().sum();
        let mean = sum / n;
        
        // Sort for percentiles
        let mut sorted = values.to_vec();
        sorted.sort();
        
        let min = sorted[0];
        let max = sorted[sorted.len() - 1];
        let median = sorted[sorted.len() / 2];
        
        // Percentiles
        let p05_idx = (sorted.len() * 5 / 100).max(0).min(sorted.len() - 1);
        let p95_idx = (sorted.len() * 95 / 100).min(sorted.len() - 1);
        let p05 = sorted[p05_idx];
        let p95 = sorted[p95_idx];
        
        // Variance and std dev
        let variance: i128 = values.iter()
            .map(|&x| {
                let diff = x - mean;
                diff.saturating_mul(diff) / AMOUNT_SCALE // Scale down to avoid overflow
            })
            .sum::<i128>() / n;
        
        // std_dev in Amount scale
        let std_dev = (variance as f64).sqrt() as i128;
        
        Some(Self {
            count,
            mean,
            median,
            p05,
            p95,
            min,
            max,
            std_dev,
        })
    }
    
    /// Get mean as f64.
    pub fn mean_f64(&self) -> f64 {
        from_amount(self.mean)
    }
    
    /// Get std_dev as f64.
    pub fn std_dev_f64(&self) -> f64 {
        from_amount(self.std_dev)
    }
}

/// Error type for honesty metrics computation.
#[derive(Debug, Clone)]
pub enum HonestyMetricsError {
    /// Fundamental accounting identity does not hold.
    IdentityViolation {
        expected_net: Amount,
        actual_net: Amount,
        gross: Amount,
        fees: Amount,
    },
    /// WindowPnLSeries validation failed.
    WindowSeriesInvalid(String),
}

impl std::fmt::Display for HonestyMetricsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::IdentityViolation { expected_net, actual_net, gross, fees } => {
                write!(
                    f,
                    "Accounting identity violation: expected net={} ({:.8}), actual net={} ({:.8}), gross={} ({:.8}), fees={} ({:.8})",
                    expected_net, from_amount(*expected_net),
                    actual_net, from_amount(*actual_net),
                    gross, from_amount(*gross),
                    fees, from_amount(*fees)
                )
            }
            Self::WindowSeriesInvalid(msg) => {
                write!(f, "WindowPnLSeries invalid: {}", msg)
            }
        }
    }
}

impl std::error::Error for HonestyMetricsError {}

impl HonestyMetrics {
    /// Compute honesty metrics from a finalized WindowPnLSeries.
    ///
    /// # Arguments
    /// * `series` - The finalized window PnL series
    /// * `total_notional` - Optional total notional traded (if well-defined in codebase)
    /// * `production_grade` - If true, abort on identity violations
    ///
    /// # Returns
    /// * `Ok(HonestyMetrics)` - Successfully computed metrics
    /// * `Err(HonestyMetricsError)` - Identity violation (in production_grade mode)
    pub fn from_window_series(
        series: &WindowPnLSeries,
        total_notional: Option<Amount>,
        production_grade: bool,
    ) -> Result<Self, HonestyMetricsError> {
        // Extract totals from series (these are the canonical values)
        let total_gross_pnl = series.total_gross_pnl;
        let total_fees = series.total_fees;
        let total_net_pnl = series.total_net_pnl;
        let total_settlement = series.total_settlement;
        
        // Verify fundamental identity: net_pnl = gross_pnl - fees + settlement
        // Note: In WindowPnL, gross_pnl is trading PnL before fees/settlement
        // The identity is: net = gross - fees + settlement
        let expected_net = total_gross_pnl - total_fees + total_settlement;
        let identity_verified = expected_net == total_net_pnl;
        let identity_error = if !identity_verified {
            Some(format!(
                "Identity mismatch: expected {} ({:.8}), actual {} ({:.8})",
                expected_net, from_amount(expected_net),
                total_net_pnl, from_amount(total_net_pnl)
            ))
        } else {
            None
        };
        
        // In production-grade mode, identity violations are fatal
        if production_grade && !identity_verified {
            return Err(HonestyMetricsError::IdentityViolation {
                expected_net,
                actual_net: total_net_pnl,
                gross: total_gross_pnl,
                fees: total_fees,
            });
        }
        
        // For ratio computation, use gross_with_settlement = gross + settlement
        // This is the "true gross" before fees
        let gross_for_ratios = total_gross_pnl + total_settlement;
        
        // Compute ratios with zero-guards
        let net_over_gross_ratio = RatioValue::new(total_net_pnl, gross_for_ratios);
        let fees_over_gross_ratio = RatioValue::new(total_fees, gross_for_ratios);
        
        // Window statistics
        let windows_traded = series.active_windows;
        let windows_finalized = series.finalized_count;
        let total_trades = series.total_trades;
        
        // Per-window metrics
        let net_pnl_per_window = PerWindowValue::new(total_net_pnl, windows_traded);
        let gross_pnl_per_window = PerWindowValue::new(gross_for_ratios, windows_traded);
        let fees_per_window = PerWindowValue::new(total_fees, windows_traded);
        
        // Notional-based metrics
        let (notional_defined, notional_undefined_reason, net_return_per_notional) = 
            match total_notional {
                Some(notional) if notional > 0 => {
                    let ratio = RatioValue::new(total_net_pnl, notional);
                    (true, None, ratio)
                }
                Some(notional) if notional == 0 => {
                    (true, Some("total_notional_traded is zero".to_string()), None)
                }
                Some(_notional) => {
                    // Negative notional shouldn't happen but handle defensively
                    (true, Some("total_notional_traded is negative (invalid)".to_string()), None)
                }
                None => {
                    (false, Some("notional base not defined canonically in codebase".to_string()), None)
                }
            };
        
        // Compute window PnL distribution statistics
        let window_pnl_stats = if !series.windows.is_empty() {
            let net_pnls: Vec<Amount> = series.windows.iter().map(|w| w.net_pnl).collect();
            DistributionStats::from_values(&net_pnls)
        } else {
            None
        };
        
        // Build the metrics struct
        let mut metrics = Self {
            total_gross_pnl,
            total_fees,
            total_net_pnl,
            total_settlement,
            windows_traded,
            windows_finalized,
            total_trades,
            net_over_gross_ratio,
            fees_over_gross_ratio,
            net_pnl_per_window,
            gross_pnl_per_window,
            fees_per_window,
            total_notional_traded: total_notional,
            net_return_per_notional,
            notional_defined,
            notional_undefined_reason,
            window_pnl_stats,
            identity_verified,
            identity_error,
            metrics_hash: 0, // Computed below
        };
        
        // Compute fingerprint hash
        metrics.metrics_hash = metrics.compute_hash();
        
        Ok(metrics)
    }
    
    /// Compute a deterministic hash of these metrics.
    pub fn compute_hash(&self) -> u64 {
        let mut hasher = DefaultHasher::new();
        
        // Hash primary accounting totals
        self.total_gross_pnl.hash(&mut hasher);
        self.total_fees.hash(&mut hasher);
        self.total_net_pnl.hash(&mut hasher);
        self.total_settlement.hash(&mut hasher);
        
        // Hash window statistics
        self.windows_traded.hash(&mut hasher);
        self.windows_finalized.hash(&mut hasher);
        self.total_trades.hash(&mut hasher);
        
        // Hash notional if defined
        if let Some(notional) = self.total_notional_traded {
            notional.hash(&mut hasher);
        }
        
        // Hash identity verification
        self.identity_verified.hash(&mut hasher);
        
        hasher.finish()
    }
    
    // =========================================================================
    // CONVENIENCE ACCESSORS (f64 conversions at boundary)
    // =========================================================================
    
    /// Get total gross PnL as f64.
    pub fn total_gross_pnl_f64(&self) -> f64 {
        from_amount(self.total_gross_pnl)
    }
    
    /// Get total fees as f64.
    pub fn total_fees_f64(&self) -> f64 {
        from_amount(self.total_fees)
    }
    
    /// Get total net PnL as f64.
    pub fn total_net_pnl_f64(&self) -> f64 {
        from_amount(self.total_net_pnl)
    }
    
    /// Get total settlement as f64.
    pub fn total_settlement_f64(&self) -> f64 {
        from_amount(self.total_settlement)
    }
    
    /// Get net-over-gross ratio as f64 (or None).
    pub fn net_over_gross_f64(&self) -> Option<f64> {
        self.net_over_gross_ratio.map(|r| r.as_f64())
    }
    
    /// Get fees-over-gross ratio as f64 (or None).
    pub fn fees_over_gross_f64(&self) -> Option<f64> {
        self.fees_over_gross_ratio.map(|r| r.as_f64())
    }
    
    /// Get average net PnL per window as f64 (or None).
    pub fn net_pnl_per_window_f64(&self) -> Option<f64> {
        self.net_pnl_per_window.map(|v| v.average_f64())
    }
    
    /// Get net return per notional as f64 (or None).
    pub fn net_return_per_notional_f64(&self) -> Option<f64> {
        self.net_return_per_notional.map(|r| r.as_f64())
    }
    
    // =========================================================================
    // FORMATTING
    // =========================================================================
    
    /// Format a summary report.
    pub fn format_summary(&self) -> String {
        let mut out = String::new();
        
        out.push_str("╔══════════════════════════════════════════════════════════════╗\n");
        out.push_str("║                    HONESTY METRICS                           ║\n");
        out.push_str("╠══════════════════════════════════════════════════════════════╣\n");
        
        // Primary totals
        out.push_str(&format!(
            "║  Total Gross PnL:        ${:>12.2}                       ║\n",
            self.total_gross_pnl_f64()
        ));
        out.push_str(&format!(
            "║  Total Fees:             ${:>12.2}                       ║\n",
            self.total_fees_f64()
        ));
        out.push_str(&format!(
            "║  Total Settlement:       ${:>12.2}                       ║\n",
            self.total_settlement_f64()
        ));
        out.push_str(&format!(
            "║  Total Net PnL:          ${:>12.2}                       ║\n",
            self.total_net_pnl_f64()
        ));
        
        out.push_str("╠══════════════════════════════════════════════════════════════╣\n");
        
        // Ratios
        match self.net_over_gross_ratio {
            Some(r) => out.push_str(&format!(
                "║  Net/Gross Ratio:        {:>12.2}%                       ║\n",
                r.as_percentage()
            )),
            None => out.push_str(
                "║  Net/Gross Ratio:        UNDEFINED (gross=0)               ║\n"
            ),
        }
        
        match self.fees_over_gross_ratio {
            Some(r) => out.push_str(&format!(
                "║  Fees/Gross Ratio:       {:>12.2}%                       ║\n",
                r.as_percentage()
            )),
            None => out.push_str(
                "║  Fees/Gross Ratio:       UNDEFINED (gross=0)               ║\n"
            ),
        }
        
        out.push_str("╠══════════════════════════════════════════════════════════════╣\n");
        
        // Per-window
        out.push_str(&format!(
            "║  Windows Traded:         {:>12}                         ║\n",
            self.windows_traded
        ));
        
        match &self.net_pnl_per_window {
            Some(v) => out.push_str(&format!(
                "║  Avg Net PnL/Window:     ${:>12.4}                       ║\n",
                v.average_f64()
            )),
            None => out.push_str(
                "║  Avg Net PnL/Window:     UNDEFINED (no windows)            ║\n"
            ),
        }
        
        out.push_str("╠══════════════════════════════════════════════════════════════╣\n");
        
        // Notional
        if self.notional_defined {
            if let Some(notional) = self.total_notional_traded {
                out.push_str(&format!(
                    "║  Total Notional:         ${:>12.2}                       ║\n",
                    from_amount(notional)
                ));
            }
            match self.net_return_per_notional {
                Some(r) => out.push_str(&format!(
                    "║  Net Return/Notional:    {:>12.4}%                       ║\n",
                    r.as_percentage()
                )),
                None => out.push_str(
                    "║  Net Return/Notional:    UNDEFINED (notional=0)           ║\n"
                ),
            }
        } else {
            out.push_str(&format!(
                "║  Notional:               NOT DEFINED                       ║\n"
            ));
            if let Some(reason) = &self.notional_undefined_reason {
                let display = if reason.len() > 45 {
                    format!("{}...", &reason[..42])
                } else {
                    reason.clone()
                };
                out.push_str(&format!(
                    "║    Reason: {:47} ║\n",
                    display
                ));
            }
        }
        
        out.push_str("╠══════════════════════════════════════════════════════════════╣\n");
        
        // Identity verification
        let check = if self.identity_verified { "✓" } else { "✗" };
        out.push_str(&format!(
            "║  [{}] Identity Verified (net = gross - fees + settle)       ║\n",
            check
        ));
        if let Some(err) = &self.identity_error {
            let display = if err.len() > 53 {
                format!("{}...", &err[..50])
            } else {
                err.clone()
            };
            out.push_str(&format!(
                "║      Error: {:48}║\n",
                display
            ));
        }
        
        out.push_str(&format!(
            "║  Metrics Hash: {:016x}                             ║\n",
            self.metrics_hash
        ));
        
        out.push_str("╚══════════════════════════════════════════════════════════════╝\n");
        
        out
    }
    
    /// Format as a compact one-line summary.
    pub fn format_compact(&self) -> String {
        let net_gross = self.net_over_gross_ratio
            .map(|r| format!("{:.1}%", r.as_percentage()))
            .unwrap_or_else(|| "N/A".to_string());
        
        let fee_gross = self.fees_over_gross_ratio
            .map(|r| format!("{:.1}%", r.as_percentage()))
            .unwrap_or_else(|| "N/A".to_string());
        
        let per_win = self.net_pnl_per_window
            .map(|v| format!("${:.2}", v.average_f64()))
            .unwrap_or_else(|| "N/A".to_string());
        
        format!(
            "net=${:.2} net/gross={} fee/gross={} per_window={} windows={} verified={}",
            self.total_net_pnl_f64(),
            net_gross,
            fee_gross,
            per_win,
            self.windows_traded,
            self.identity_verified
        )
    }
}

impl Default for HonestyMetrics {
    fn default() -> Self {
        Self {
            total_gross_pnl: 0,
            total_fees: 0,
            total_net_pnl: 0,
            total_settlement: 0,
            windows_traded: 0,
            windows_finalized: 0,
            total_trades: 0,
            net_over_gross_ratio: None,
            fees_over_gross_ratio: None,
            net_pnl_per_window: None,
            gross_pnl_per_window: None,
            fees_per_window: None,
            total_notional_traded: None,
            net_return_per_notional: None,
            notional_defined: false,
            notional_undefined_reason: Some("not computed".to_string()),
            window_pnl_stats: None,
            identity_verified: false,
            identity_error: Some("not computed".to_string()),
            metrics_hash: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backtest_v2::window_pnl::{WindowPnL, WindowPnLSeries};
    use crate::backtest_v2::settlement::NS_PER_SEC;
    
    fn make_test_series() -> WindowPnLSeries {
        let mut series = WindowPnLSeries::new();
        
        // Window 1: gross=10, fees=1, settlement=5 -> net=14
        let mut w1 = WindowPnL::new(1000 * NS_PER_SEC, "test-market-1000".to_string());
        w1.gross_pnl = to_amount(10.0);
        w1.fees = to_amount(1.0);
        w1.settlement_transfer = to_amount(5.0);
        w1.net_pnl = w1.gross_pnl - w1.fees + w1.settlement_transfer;
        w1.trades_count = 5;
        w1.is_finalized = true;
        
        // Window 2: gross=-5, fees=0.5, settlement=0 -> net=-5.5
        let mut w2 = WindowPnL::new(2000 * NS_PER_SEC, "test-market-2000".to_string());
        w2.gross_pnl = to_amount(-5.0);
        w2.fees = to_amount(0.5);
        w2.settlement_transfer = to_amount(0.0);
        w2.net_pnl = w2.gross_pnl - w2.fees + w2.settlement_transfer;
        w2.trades_count = 3;
        w2.is_finalized = true;
        
        // Window 3: gross=2, fees=0.2, settlement=3 -> net=4.8
        let mut w3 = WindowPnL::new(3000 * NS_PER_SEC, "test-market-3000".to_string());
        w3.gross_pnl = to_amount(2.0);
        w3.fees = to_amount(0.2);
        w3.settlement_transfer = to_amount(3.0);
        w3.net_pnl = w3.gross_pnl - w3.fees + w3.settlement_transfer;
        w3.trades_count = 2;
        w3.is_finalized = true;
        
        series.add_window(w1);
        series.add_window(w2);
        series.add_window(w3);
        
        series
    }
    
    #[test]
    fn test_honesty_metrics_basic() {
        let series = make_test_series();
        let metrics = HonestyMetrics::from_window_series(&series, None, false).unwrap();
        
        // Totals: gross=7, fees=1.7, settlement=8 -> net=13.3
        assert!((metrics.total_gross_pnl_f64() - 7.0).abs() < 0.01);
        assert!((metrics.total_fees_f64() - 1.7).abs() < 0.01);
        assert!((metrics.total_settlement_f64() - 8.0).abs() < 0.01);
        assert!((metrics.total_net_pnl_f64() - 13.3).abs() < 0.01);
        
        assert!(metrics.identity_verified);
        assert!(metrics.identity_error.is_none());
    }
    
    #[test]
    fn test_honesty_metrics_ratios() {
        let series = make_test_series();
        let metrics = HonestyMetrics::from_window_series(&series, None, false).unwrap();
        
        // gross_for_ratios = gross + settlement = 7 + 8 = 15
        // net/gross = 13.3 / 15 = 0.8867
        // fees/gross = 1.7 / 15 = 0.1133
        
        let net_gross = metrics.net_over_gross_f64().unwrap();
        let fees_gross = metrics.fees_over_gross_f64().unwrap();
        
        assert!((net_gross - 0.8867).abs() < 0.01);
        assert!((fees_gross - 0.1133).abs() < 0.01);
        
        // These should sum to approximately 1.0
        assert!((net_gross + fees_gross - 1.0).abs() < 0.01);
    }
    
    #[test]
    fn test_honesty_metrics_per_window() {
        let series = make_test_series();
        let metrics = HonestyMetrics::from_window_series(&series, None, false).unwrap();
        
        assert_eq!(metrics.windows_traded, 3);
        
        // Avg net per window = 13.3 / 3 = 4.43
        let per_window = metrics.net_pnl_per_window_f64().unwrap();
        assert!((per_window - 4.43).abs() < 0.1);
    }
    
    #[test]
    fn test_honesty_metrics_zero_gross() {
        let mut series = WindowPnLSeries::new();
        
        // Window with zero gross and zero fees
        let mut w1 = WindowPnL::new(1000 * NS_PER_SEC, "test-market".to_string());
        w1.gross_pnl = to_amount(0.0);
        w1.fees = to_amount(0.0);
        w1.settlement_transfer = to_amount(0.0);
        w1.net_pnl = to_amount(0.0);
        w1.trades_count = 0;
        w1.is_finalized = true;
        
        series.add_window(w1);
        
        let metrics = HonestyMetrics::from_window_series(&series, None, false).unwrap();
        
        // Ratios should be None (undefined) when gross=0
        assert!(metrics.net_over_gross_ratio.is_none());
        assert!(metrics.fees_over_gross_ratio.is_none());
        assert!(metrics.identity_verified);
    }
    
    #[test]
    fn test_honesty_metrics_no_windows() {
        let series = WindowPnLSeries::new();
        let metrics = HonestyMetrics::from_window_series(&series, None, false).unwrap();
        
        assert_eq!(metrics.windows_traded, 0);
        assert!(metrics.net_pnl_per_window.is_none());
        assert!(metrics.identity_verified);
    }
    
    #[test]
    fn test_honesty_metrics_with_notional() {
        let series = make_test_series();
        let total_notional = to_amount(1000.0);
        
        let metrics = HonestyMetrics::from_window_series(&series, Some(total_notional), false).unwrap();
        
        assert!(metrics.notional_defined);
        assert!(metrics.notional_undefined_reason.is_none());
        
        // net_return/notional = 13.3 / 1000 = 0.0133
        let return_per_notional = metrics.net_return_per_notional_f64().unwrap();
        assert!((return_per_notional - 0.0133).abs() < 0.001);
    }
    
    #[test]
    fn test_honesty_metrics_notional_undefined() {
        let series = make_test_series();
        let metrics = HonestyMetrics::from_window_series(&series, None, false).unwrap();
        
        assert!(!metrics.notional_defined);
        assert!(metrics.notional_undefined_reason.is_some());
        assert!(metrics.net_return_per_notional.is_none());
    }
    
    #[test]
    fn test_honesty_metrics_identity_violation() {
        let mut series = WindowPnLSeries::new();
        
        // Create a window with inconsistent values
        let mut w1 = WindowPnL::new(1000 * NS_PER_SEC, "test-market".to_string());
        w1.gross_pnl = to_amount(10.0);
        w1.fees = to_amount(1.0);
        w1.settlement_transfer = to_amount(5.0);
        // Intentionally wrong net_pnl (should be 14.0)
        w1.net_pnl = to_amount(20.0);
        w1.is_finalized = true;
        
        series.add_window(w1);
        
        // Non-production mode: should succeed but flag violation
        let metrics = HonestyMetrics::from_window_series(&series, None, false).unwrap();
        assert!(!metrics.identity_verified);
        assert!(metrics.identity_error.is_some());
        
        // Production mode: should fail
        let result = HonestyMetrics::from_window_series(&series, None, true);
        assert!(result.is_err());
        match result.unwrap_err() {
            HonestyMetricsError::IdentityViolation { .. } => (),
            _ => panic!("Expected IdentityViolation error"),
        }
    }
    
    #[test]
    fn test_honesty_metrics_determinism() {
        let series = make_test_series();
        
        let m1 = HonestyMetrics::from_window_series(&series, None, false).unwrap();
        let m2 = HonestyMetrics::from_window_series(&series, None, false).unwrap();
        
        assert_eq!(m1.metrics_hash, m2.metrics_hash);
        assert_eq!(m1.total_gross_pnl, m2.total_gross_pnl);
        assert_eq!(m1.total_fees, m2.total_fees);
        assert_eq!(m1.total_net_pnl, m2.total_net_pnl);
    }
    
    #[test]
    fn test_ratio_value() {
        // Normal case
        let r = RatioValue::new(to_amount(75.0), to_amount(100.0)).unwrap();
        assert!((r.as_f64() - 0.75).abs() < 0.001);
        assert!((r.as_percentage() - 75.0).abs() < 0.1);
        
        // Zero denominator
        let r_zero = RatioValue::new(to_amount(10.0), to_amount(0.0));
        assert!(r_zero.is_none());
        
        // Negative ratio (loss)
        let r_neg = RatioValue::new(to_amount(-50.0), to_amount(100.0)).unwrap();
        assert!((r_neg.as_f64() - (-0.50)).abs() < 0.001);
    }
    
    #[test]
    fn test_per_window_value() {
        let pw = PerWindowValue::new(to_amount(100.0), 10).unwrap();
        assert!((pw.average_f64() - 10.0).abs() < 0.001);
        assert!((pw.total_f64() - 100.0).abs() < 0.001);
        
        // Zero windows
        let pw_zero = PerWindowValue::new(to_amount(100.0), 0);
        assert!(pw_zero.is_none());
    }
    
    #[test]
    fn test_distribution_stats() {
        let values = vec![
            to_amount(10.0),
            to_amount(20.0),
            to_amount(30.0),
            to_amount(40.0),
            to_amount(50.0),
        ];
        
        let stats = DistributionStats::from_values(&values).unwrap();
        
        assert_eq!(stats.count, 5);
        assert!((stats.mean_f64() - 30.0).abs() < 0.01);
        assert!((from_amount(stats.median) - 30.0).abs() < 0.01);
        assert!((from_amount(stats.min) - 10.0).abs() < 0.01);
        assert!((from_amount(stats.max) - 50.0).abs() < 0.01);
    }
    
    #[test]
    fn test_format_summary() {
        let series = make_test_series();
        let metrics = HonestyMetrics::from_window_series(&series, None, false).unwrap();
        
        let summary = metrics.format_summary();
        assert!(summary.contains("HONESTY METRICS"));
        assert!(summary.contains("Net/Gross Ratio"));
        assert!(summary.contains("Identity Verified"));
    }
}

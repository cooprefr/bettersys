//! Basis Diagnostics: Chainlink vs Binance
//!
//! Quantifies the mismatch between Binance mid-price (used for signals) and
//! Chainlink oracle price (used for settlement). This is critical for:
//! - Understanding signal-to-settlement basis risk
//! - Identifying regimes where Binance-based strategies may fail
//! - Validating that Binance is a reasonable predictor

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use super::chainlink::ChainlinkRound;
use super::settlement_source::{OraclePricePoint, SettlementReferenceRule, SettlementReferenceSource};
use crate::backtest_v2::clock::Nanos;

// =============================================================================
// Window Basis Record
// =============================================================================

/// Basis record for a single 15-minute window.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowBasis {
    /// Window start timestamp (Unix seconds).
    pub window_start_ts: u64,
    /// Window end timestamp (Unix seconds).
    pub window_end_ts: u64,
    /// Asset symbol.
    pub asset_symbol: String,

    /// Binance mid-price at cutoff (if available).
    pub binance_mid_at_cutoff: Option<f64>,
    /// Chainlink reference price used for settlement.
    pub chainlink_settlement_price: Option<f64>,
    /// Chainlink round ID used.
    pub chainlink_round_id: Option<u128>,
    /// Chainlink updated_at (may differ from cutoff).
    pub chainlink_updated_at: Option<u64>,

    /// Basis = Binance - Chainlink (in USD).
    pub basis_usd: Option<f64>,
    /// Basis in basis points.
    pub basis_bps: Option<f64>,

    /// Whether Binance and Chainlink agree on direction (up/down).
    /// Computed using start and end prices.
    pub direction_agrees: Option<bool>,

    /// Binance start price (for direction comparison).
    pub binance_start: Option<f64>,
    /// Binance end price.
    pub binance_end: Option<f64>,
    /// Chainlink start price.
    pub chainlink_start: Option<f64>,
    /// Chainlink end price (settlement).
    pub chainlink_end: Option<f64>,
}

impl WindowBasis {
    pub fn new(window_start_ts: u64, window_end_ts: u64, asset_symbol: String) -> Self {
        Self {
            window_start_ts,
            window_end_ts,
            asset_symbol,
            binance_mid_at_cutoff: None,
            chainlink_settlement_price: None,
            chainlink_round_id: None,
            chainlink_updated_at: None,
            basis_usd: None,
            basis_bps: None,
            direction_agrees: None,
            binance_start: None,
            binance_end: None,
            chainlink_start: None,
            chainlink_end: None,
        }
    }

    /// Compute derived fields (basis, direction agreement).
    pub fn finalize(&mut self) {
        // Compute basis if both prices available
        if let (Some(bn), Some(cl)) = (self.binance_mid_at_cutoff, self.chainlink_settlement_price) {
            self.basis_usd = Some(bn - cl);
            if cl.abs() > 0.0 {
                self.basis_bps = Some(((bn - cl) / cl) * 10_000.0);
            }
        }

        // Compute direction agreement
        if let (Some(bn_start), Some(bn_end), Some(cl_start), Some(cl_end)) = (
            self.binance_start,
            self.binance_end,
            self.chainlink_start,
            self.chainlink_end,
        ) {
            let bn_up = bn_end > bn_start;
            let cl_up = cl_end > cl_start;
            self.direction_agrees = Some(bn_up == cl_up);
        }
    }
}

// =============================================================================
// Basis Statistics
// =============================================================================

/// Aggregate statistics for basis analysis.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BasisStats {
    /// Number of windows analyzed.
    pub window_count: usize,
    /// Windows with complete data.
    pub windows_with_data: usize,

    /// Mean basis (USD).
    pub mean_basis_usd: Option<f64>,
    /// Median basis (USD).
    pub median_basis_usd: Option<f64>,
    /// Std deviation of basis (USD).
    pub std_basis_usd: Option<f64>,
    /// Max absolute basis (USD).
    pub max_abs_basis_usd: Option<f64>,

    /// Mean basis (bps).
    pub mean_basis_bps: Option<f64>,
    /// Median basis (bps).
    pub median_basis_bps: Option<f64>,
    /// Std deviation of basis (bps).
    pub std_basis_bps: Option<f64>,
    /// P95 absolute basis (bps).
    pub p95_abs_basis_bps: Option<f64>,
    /// P99 absolute basis (bps).
    pub p99_abs_basis_bps: Option<f64>,
    /// Max absolute basis (bps).
    pub max_abs_basis_bps: Option<f64>,

    /// Direction agreement rate (%).
    pub direction_agreement_rate: Option<f64>,
    /// Windows where direction disagreed.
    pub direction_disagreements: usize,
}

impl BasisStats {
    /// Compute statistics from window records.
    pub fn from_windows(windows: &[WindowBasis]) -> Self {
        let mut stats = Self::default();
        stats.window_count = windows.len();

        let basis_usd: Vec<f64> = windows
            .iter()
            .filter_map(|w| w.basis_usd)
            .collect();

        let basis_bps: Vec<f64> = windows
            .iter()
            .filter_map(|w| w.basis_bps)
            .collect();

        let direction_agrees: Vec<bool> = windows
            .iter()
            .filter_map(|w| w.direction_agrees)
            .collect();

        stats.windows_with_data = basis_usd.len();

        if !basis_usd.is_empty() {
            let n = basis_usd.len() as f64;

            // Mean
            let sum: f64 = basis_usd.iter().sum();
            stats.mean_basis_usd = Some(sum / n);

            // Median
            let mut sorted = basis_usd.clone();
            sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            stats.median_basis_usd = Some(percentile(&sorted, 50.0));

            // Std dev
            let mean = stats.mean_basis_usd.unwrap();
            let variance: f64 = basis_usd.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / n;
            stats.std_basis_usd = Some(variance.sqrt());

            // Max abs
            stats.max_abs_basis_usd = basis_usd.iter().map(|x| x.abs()).reduce(f64::max);
        }

        if !basis_bps.is_empty() {
            let n = basis_bps.len() as f64;

            // Mean
            let sum: f64 = basis_bps.iter().sum();
            stats.mean_basis_bps = Some(sum / n);

            // Median
            let mut sorted = basis_bps.clone();
            sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            stats.median_basis_bps = Some(percentile(&sorted, 50.0));

            // Std dev
            let mean = stats.mean_basis_bps.unwrap();
            let variance: f64 = basis_bps.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / n;
            stats.std_basis_bps = Some(variance.sqrt());

            // Percentiles of abs basis
            let mut abs_sorted: Vec<f64> = basis_bps.iter().map(|x| x.abs()).collect();
            abs_sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            stats.p95_abs_basis_bps = Some(percentile(&abs_sorted, 95.0));
            stats.p99_abs_basis_bps = Some(percentile(&abs_sorted, 99.0));
            stats.max_abs_basis_bps = abs_sorted.last().copied();
        }

        if !direction_agrees.is_empty() {
            let agrees: usize = direction_agrees.iter().filter(|&&x| x).count();
            stats.direction_disagreements = direction_agrees.len() - agrees;
            stats.direction_agreement_rate =
                Some((agrees as f64 / direction_agrees.len() as f64) * 100.0);
        }

        stats
    }
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let idx = ((p / 100.0) * (sorted.len() - 1) as f64).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

// =============================================================================
// Basis Diagnostics Engine
// =============================================================================

/// Engine for computing basis diagnostics across windows.
pub struct BasisDiagnostics {
    windows: Vec<WindowBasis>,
    /// Stratification by volatility regime (optional).
    regime_windows: HashMap<String, Vec<WindowBasis>>,
}

impl BasisDiagnostics {
    pub fn new() -> Self {
        Self {
            windows: Vec::new(),
            regime_windows: HashMap::new(),
        }
    }

    /// Record a window's basis data.
    pub fn record_window(&mut self, window: WindowBasis) {
        self.windows.push(window);
    }

    /// Record a window with volatility regime stratification.
    pub fn record_window_with_regime(&mut self, window: WindowBasis, regime: &str) {
        self.regime_windows
            .entry(regime.to_string())
            .or_default()
            .push(window.clone());
        self.windows.push(window);
    }

    /// Get overall statistics.
    pub fn overall_stats(&self) -> BasisStats {
        BasisStats::from_windows(&self.windows)
    }

    /// Get statistics per regime.
    pub fn stats_by_regime(&self) -> HashMap<String, BasisStats> {
        self.regime_windows
            .iter()
            .map(|(regime, windows)| (regime.clone(), BasisStats::from_windows(windows)))
            .collect()
    }

    /// Get all recorded windows.
    pub fn windows(&self) -> &[WindowBasis] {
        &self.windows
    }

    /// Generate a summary report string.
    pub fn summary_report(&self) -> String {
        let stats = self.overall_stats();

        let mut report = String::new();
        report.push_str("╔═══════════════════════════════════════════════════════════════╗\n");
        report.push_str("║           BASIS DIAGNOSTICS: Chainlink vs Binance             ║\n");
        report.push_str("╠═══════════════════════════════════════════════════════════════╣\n");

        report.push_str(&format!(
            "║ Windows Analyzed: {:>10}                                  ║\n",
            stats.window_count
        ));
        report.push_str(&format!(
            "║ Windows with Data: {:>9}                                  ║\n",
            stats.windows_with_data
        ));
        report.push_str("╠═══════════════════════════════════════════════════════════════╣\n");

        if let Some(mean) = stats.mean_basis_bps {
            report.push_str(&format!(
                "║ Mean Basis:       {:>+10.2} bps                              ║\n",
                mean
            ));
        }
        if let Some(median) = stats.median_basis_bps {
            report.push_str(&format!(
                "║ Median Basis:     {:>+10.2} bps                              ║\n",
                median
            ));
        }
        if let Some(std) = stats.std_basis_bps {
            report.push_str(&format!(
                "║ Std Dev:          {:>10.2} bps                              ║\n",
                std
            ));
        }
        if let Some(p95) = stats.p95_abs_basis_bps {
            report.push_str(&format!(
                "║ P95 |Basis|:      {:>10.2} bps                              ║\n",
                p95
            ));
        }
        if let Some(p99) = stats.p99_abs_basis_bps {
            report.push_str(&format!(
                "║ P99 |Basis|:      {:>10.2} bps                              ║\n",
                p99
            ));
        }
        if let Some(max) = stats.max_abs_basis_bps {
            report.push_str(&format!(
                "║ Max |Basis|:      {:>10.2} bps                              ║\n",
                max
            ));
        }

        report.push_str("╠═══════════════════════════════════════════════════════════════╣\n");

        if let Some(rate) = stats.direction_agreement_rate {
            report.push_str(&format!(
                "║ Direction Agreement: {:>7.1}%                                ║\n",
                rate
            ));
            report.push_str(&format!(
                "║ Direction Disagreements: {:>5}                                ║\n",
                stats.direction_disagreements
            ));
        }

        report.push_str("╚═══════════════════════════════════════════════════════════════╝\n");

        // Add regime breakdown if available
        if !self.regime_windows.is_empty() {
            report.push_str("\n╔═══════════════════════════════════════════════════════════════╗\n");
            report.push_str("║                    STATS BY VOLATILITY REGIME                 ║\n");
            report.push_str("╠═══════════════════════════════════════════════════════════════╣\n");

            for (regime, windows) in &self.regime_windows {
                let regime_stats = BasisStats::from_windows(windows);
                report.push_str(&format!(
                    "║ {:<15} N={:<5} Mean={:>+6.1}bps P99={:>6.1}bps Agr={:>5.1}%  ║\n",
                    regime,
                    regime_stats.windows_with_data,
                    regime_stats.mean_basis_bps.unwrap_or(0.0),
                    regime_stats.p99_abs_basis_bps.unwrap_or(0.0),
                    regime_stats.direction_agreement_rate.unwrap_or(0.0),
                ));
            }

            report.push_str("╚═══════════════════════════════════════════════════════════════╝\n");
        }

        report
    }

    /// Print summary to tracing log.
    pub fn log_summary(&self) {
        let report = self.summary_report();
        for line in report.lines() {
            tracing::info!("{}", line);
        }
    }

    /// Reset diagnostics.
    pub fn reset(&mut self) {
        self.windows.clear();
        self.regime_windows.clear();
    }
}

impl Default for BasisDiagnostics {
    fn default() -> Self {
        Self::new()
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_window_basis_finalize() {
        let mut window = WindowBasis::new(1000, 1900, "BTC".to_string());
        window.binance_mid_at_cutoff = Some(50100.0);
        window.chainlink_settlement_price = Some(50000.0);
        window.binance_start = Some(49900.0);
        window.binance_end = Some(50100.0);
        window.chainlink_start = Some(49900.0);
        window.chainlink_end = Some(50000.0);
        window.finalize();

        assert!((window.basis_usd.unwrap() - 100.0).abs() < 0.01);
        assert!((window.basis_bps.unwrap() - 20.0).abs() < 0.1);
        assert_eq!(window.direction_agrees, Some(true)); // Both went up
    }

    #[test]
    fn test_direction_disagreement() {
        let mut window = WindowBasis::new(1000, 1900, "BTC".to_string());
        window.binance_start = Some(50000.0);
        window.binance_end = Some(50100.0); // UP
        window.chainlink_start = Some(50000.0);
        window.chainlink_end = Some(49900.0); // DOWN
        window.finalize();

        assert_eq!(window.direction_agrees, Some(false));
    }

    #[test]
    fn test_basis_stats() {
        let windows = vec![
            {
                let mut w = WindowBasis::new(1000, 1900, "BTC".to_string());
                w.basis_usd = Some(10.0); // Need basis_usd for windows_with_data
                w.basis_bps = Some(10.0);
                w.direction_agrees = Some(true);
                w
            },
            {
                let mut w = WindowBasis::new(1900, 2800, "BTC".to_string());
                w.basis_usd = Some(-20.0);
                w.basis_bps = Some(-20.0);
                w.direction_agrees = Some(true);
                w
            },
            {
                let mut w = WindowBasis::new(2800, 3700, "BTC".to_string());
                w.basis_usd = Some(30.0);
                w.basis_bps = Some(30.0);
                w.direction_agrees = Some(false);
                w
            },
        ];

        let stats = BasisStats::from_windows(&windows);

        assert_eq!(stats.window_count, 3);
        assert_eq!(stats.windows_with_data, 3);
        assert!((stats.mean_basis_bps.unwrap() - 6.67).abs() < 0.1);
        assert_eq!(stats.direction_disagreements, 1);
        assert!((stats.direction_agreement_rate.unwrap() - 66.67).abs() < 0.1);
    }

    #[test]
    fn test_diagnostics_summary() {
        let mut diag = BasisDiagnostics::new();

        for i in 0..10 {
            let mut window = WindowBasis::new(i * 900, (i + 1) * 900, "BTC".to_string());
            window.basis_bps = Some((i as f64 - 5.0) * 10.0);
            window.direction_agrees = Some(i % 3 != 0);
            diag.record_window(window);
        }

        let stats = diag.overall_stats();
        assert_eq!(stats.window_count, 10);

        let report = diag.summary_report();
        assert!(report.contains("BASIS DIAGNOSTICS"));
        assert!(report.contains("Windows Analyzed"));
    }
}

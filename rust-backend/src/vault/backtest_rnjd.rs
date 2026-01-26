//! Backtest framework for RN-JD model validation
//!
//! This module provides infrastructure for collecting and analyzing
//! backtest data to compare the RN-JD enhanced model against the
//! legacy driftless lognormal approach.
//!
//! Key metrics tracked:
//! - Prediction accuracy (directional)
//! - Brier score (calibration)
//! - Position sizing differences
//! - Jump regime detection effectiveness

use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

/// A single backtest observation record
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BacktestRecord {
    /// Unix timestamp of the observation
    pub timestamp: i64,
    /// Market identifier
    pub market_slug: String,
    /// Underlying price at market start
    pub p_start: f64,
    /// Current underlying price
    pub p_now: f64,
    /// Current market mid price (probability)
    pub market_mid: f64,
    /// Price volatility (annualized)
    pub sigma_price: f64,
    /// Time remaining to expiry (seconds)
    pub t_rem_secs: f64,

    // Model outputs
    /// Original driftless lognormal p_up estimate
    pub p_up_old: f64,
    /// RN-JD enhanced p_up estimate
    pub p_up_rnjd: f64,
    /// Belief volatility estimate
    pub sigma_b: f64,
    /// RN drift correction applied
    pub drift_correction: f64,
    /// Whether jump regime was detected
    pub jump_regime: bool,

    // Position sizing
    /// Kelly position size with old model
    pub kelly_old: f64,
    /// Kelly position size with RN-JD model
    pub kelly_rnjd: f64,

    // Outcome (filled in after resolution)
    /// Whether this market has been resolved
    pub resolved: bool,
    /// Actual outcome (true = Up won)
    pub outcome_up: Option<bool>,
}

/// Performance metrics comparing models
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BacktestMetrics {
    /// Total number of resolved records
    pub total_resolved: usize,
    /// Accuracy of old model (fraction correct)
    pub old_accuracy: f64,
    /// Accuracy of RN-JD model (fraction correct)
    pub rnjd_accuracy: f64,
    /// Brier score for old model (lower is better)
    pub old_brier_score: f64,
    /// Brier score for RN-JD model (lower is better)
    pub rnjd_brier_score: f64,
    /// Average position size ratio (rnjd/old)
    pub avg_position_ratio: f64,
    /// Number of trades skipped due to jump regime
    pub jump_regime_skips: usize,
}

/// Collector for backtest records with metrics calculation
#[derive(Debug)]
pub struct BacktestCollector {
    records: VecDeque<BacktestRecord>,
    max_records: usize,
}

impl Default for BacktestCollector {
    fn default() -> Self {
        Self::new(10_000)
    }
}

impl BacktestCollector {
    /// Create a new collector with specified maximum records
    pub fn new(max_records: usize) -> Self {
        Self {
            records: VecDeque::new(),
            max_records,
        }
    }

    /// Add a new record to the collector
    pub fn add_record(&mut self, record: BacktestRecord) {
        self.records.push_back(record);
        while self.records.len() > self.max_records {
            self.records.pop_front();
        }
    }

    /// Get all records as a slice
    pub fn records(&self) -> &VecDeque<BacktestRecord> {
        &self.records
    }

    /// Get the most recent N records
    pub fn recent_records(&self, limit: usize) -> Vec<&BacktestRecord> {
        self.records.iter().rev().take(limit).collect()
    }

    /// Get number of records
    pub fn len(&self) -> usize {
        self.records.len()
    }

    /// Check if collector is empty
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// Mark records as resolved with actual outcome
    pub fn resolve_market(&mut self, market_slug: &str, outcome_up: bool) {
        for record in &mut self.records {
            if record.market_slug == market_slug && !record.resolved {
                record.resolved = true;
                record.outcome_up = Some(outcome_up);
            }
        }
    }

    /// Calculate performance metrics from resolved records
    pub fn calculate_metrics(&self) -> BacktestMetrics {
        let resolved: Vec<_> = self
            .records
            .iter()
            .filter(|r| r.resolved && r.outcome_up.is_some())
            .collect();

        if resolved.is_empty() {
            return BacktestMetrics::default();
        }

        let mut old_correct = 0usize;
        let mut rnjd_correct = 0usize;
        let mut old_brier = 0.0;
        let mut rnjd_brier = 0.0;
        let mut position_ratio_sum = 0.0;
        let mut position_ratio_count = 0usize;
        let mut jump_skips = 0usize;

        for r in &resolved {
            let outcome: f64 = if r.outcome_up.unwrap() { 1.0 } else { 0.0 };

            // Old model prediction
            let old_pred: f64 = if r.p_up_old > 0.5 { 1.0 } else { 0.0 };
            if (old_pred - outcome).abs() < 0.01 {
                old_correct += 1;
            }
            old_brier += (r.p_up_old - outcome).powi(2);

            // RN-JD prediction
            let rnjd_pred: f64 = if r.p_up_rnjd > 0.5 { 1.0 } else { 0.0 };
            if (rnjd_pred - outcome).abs() < 0.01 {
                rnjd_correct += 1;
            }
            rnjd_brier += (r.p_up_rnjd - outcome).powi(2);

            // Position sizing comparison
            if r.kelly_old > 0.0 {
                position_ratio_sum += r.kelly_rnjd / r.kelly_old;
                position_ratio_count += 1;
            }

            if r.jump_regime {
                jump_skips += 1;
            }
        }

        let n = resolved.len() as f64;

        BacktestMetrics {
            total_resolved: resolved.len(),
            old_accuracy: old_correct as f64 / n,
            rnjd_accuracy: rnjd_correct as f64 / n,
            old_brier_score: old_brier / n,
            rnjd_brier_score: rnjd_brier / n,
            avg_position_ratio: if position_ratio_count > 0 {
                position_ratio_sum / position_ratio_count as f64
            } else {
                1.0
            },
            jump_regime_skips: jump_skips,
        }
    }

    /// Export all records as JSON string
    pub fn export_json(&self) -> String {
        serde_json::to_string_pretty(&self.records).unwrap_or_default()
    }

    /// Clear all records
    pub fn clear(&mut self) {
        self.records.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_collector_basic() {
        let mut collector = BacktestCollector::new(100);
        assert!(collector.is_empty());

        let record = BacktestRecord {
            timestamp: 1700000000,
            market_slug: "btc-updown-15m-test".to_string(),
            p_start: 50000.0,
            p_now: 50100.0,
            market_mid: 0.52,
            sigma_price: 0.20,
            t_rem_secs: 600.0,
            p_up_old: 0.55,
            p_up_rnjd: 0.54,
            sigma_b: 2.0,
            drift_correction: 0.001,
            jump_regime: false,
            kelly_old: 100.0,
            kelly_rnjd: 95.0,
            resolved: false,
            outcome_up: None,
        };

        collector.add_record(record);
        assert_eq!(collector.len(), 1);
    }

    #[test]
    fn test_collector_max_records() {
        let mut collector = BacktestCollector::new(3);

        for i in 0..5 {
            collector.add_record(BacktestRecord {
                timestamp: 1700000000 + i,
                market_slug: format!("test-{}", i),
                p_start: 100.0,
                p_now: 101.0,
                market_mid: 0.5,
                sigma_price: 0.2,
                t_rem_secs: 600.0,
                p_up_old: 0.5,
                p_up_rnjd: 0.5,
                sigma_b: 2.0,
                drift_correction: 0.0,
                jump_regime: false,
                kelly_old: 100.0,
                kelly_rnjd: 100.0,
                resolved: false,
                outcome_up: None,
            });
        }

        assert_eq!(collector.len(), 3);
    }

    #[test]
    fn test_resolve_market() {
        let mut collector = BacktestCollector::new(100);

        collector.add_record(BacktestRecord {
            timestamp: 1700000000,
            market_slug: "btc-updown-15m-test".to_string(),
            p_start: 100.0,
            p_now: 101.0,
            market_mid: 0.55,
            sigma_price: 0.2,
            t_rem_secs: 600.0,
            p_up_old: 0.6,
            p_up_rnjd: 0.58,
            sigma_b: 2.0,
            drift_correction: 0.001,
            jump_regime: false,
            kelly_old: 100.0,
            kelly_rnjd: 95.0,
            resolved: false,
            outcome_up: None,
        });

        collector.resolve_market("btc-updown-15m-test", true);

        let record = collector.records().back().unwrap();
        assert!(record.resolved);
        assert_eq!(record.outcome_up, Some(true));
    }

    #[test]
    fn test_calculate_metrics() {
        let mut collector = BacktestCollector::new(100);

        // Add some resolved records
        for i in 0..10 {
            let outcome_up = i % 2 == 0;
            collector.add_record(BacktestRecord {
                timestamp: 1700000000 + i,
                market_slug: format!("test-{}", i),
                p_start: 100.0,
                p_now: 101.0,
                market_mid: 0.5,
                sigma_price: 0.2,
                t_rem_secs: 600.0,
                // Old model gets it right half the time
                p_up_old: if i % 4 < 2 { 0.6 } else { 0.4 },
                // RN-JD model is slightly better
                p_up_rnjd: if outcome_up { 0.6 } else { 0.4 },
                sigma_b: 2.0,
                drift_correction: 0.001,
                jump_regime: i == 5,
                kelly_old: 100.0,
                kelly_rnjd: 90.0,
                resolved: true,
                outcome_up: Some(outcome_up),
            });
        }

        let metrics = collector.calculate_metrics();
        assert_eq!(metrics.total_resolved, 10);
        assert!(metrics.rnjd_accuracy >= metrics.old_accuracy);
        assert!(metrics.rnjd_brier_score <= metrics.old_brier_score + 0.01);
        assert_eq!(metrics.jump_regime_skips, 1);
    }
}

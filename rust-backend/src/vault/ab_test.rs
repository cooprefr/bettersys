//! A/B testing framework for RN-JD vs legacy model
//!
//! This module provides infrastructure for comparing the performance
//! of the RN-JD enhanced model against the legacy driftless lognormal
//! approach in a controlled experiment.
//!
//! Features:
//! - Per-market random assignment to model variant
//! - Trade-level P&L tracking per variant
//! - Summary statistics for comparison

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Model variant for A/B testing
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ModelVariant {
    /// Original driftless lognormal model
    Legacy,
    /// RN-JD enhanced model with belief volatility
    RnjdEnhanced,
}

/// Configuration for A/B testing
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ABTestConfig {
    /// Whether A/B test is enabled
    pub enabled: bool,
    /// Probability of using RN-JD (0.5 = 50/50 split)
    pub rnjd_probability: f64,
}

impl Default for ABTestConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            rnjd_probability: 0.5,
        }
    }
}

/// Summary statistics for A/B test
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ABTestSummary {
    /// Whether A/B test is currently enabled
    pub enabled: bool,
    /// Total trades with legacy model
    pub legacy_trades: usize,
    /// Total P&L with legacy model
    pub legacy_pnl: f64,
    /// Average P&L per trade with legacy model
    pub legacy_avg_pnl: f64,
    /// Total trades with RN-JD model
    pub rnjd_trades: usize,
    /// Total P&L with RN-JD model
    pub rnjd_pnl: f64,
    /// Average P&L per trade with RN-JD model
    pub rnjd_avg_pnl: f64,
    /// Number of markets assigned to each variant
    pub market_assignments: HashMap<String, String>,
}

/// Tracker for A/B test state and results
#[derive(Debug)]
pub struct ABTestTracker {
    config: ABTestConfig,
    /// market_slug -> assigned variant
    assignments: HashMap<String, ModelVariant>,
    /// Stats for legacy model
    legacy_trades: usize,
    legacy_pnl: f64,
    /// Stats for RN-JD model
    rnjd_trades: usize,
    rnjd_pnl: f64,
    /// Simple RNG state for reproducibility
    rng_state: u64,
}

impl Default for ABTestTracker {
    fn default() -> Self {
        Self::new(ABTestConfig::default())
    }
}

impl ABTestTracker {
    /// Create a new tracker with the given configuration
    pub fn new(config: ABTestConfig) -> Self {
        Self {
            config,
            assignments: HashMap::new(),
            legacy_trades: 0,
            legacy_pnl: 0.0,
            rnjd_trades: 0,
            rnjd_pnl: 0.0,
            rng_state: 42, // Seed for reproducibility
        }
    }

    /// Simple LCG random number generator (0.0 to 1.0)
    fn next_random(&mut self) -> f64 {
        self.rng_state = self.rng_state.wrapping_mul(1103515245).wrapping_add(12345);
        ((self.rng_state >> 16) & 0x7FFF) as f64 / 32767.0
    }

    /// Get or assign variant for a market
    ///
    /// If A/B testing is disabled, always returns RnjdEnhanced.
    /// Otherwise, randomly assigns a variant on first call for each market.
    pub fn get_variant(&mut self, market_slug: &str) -> ModelVariant {
        if !self.config.enabled {
            return ModelVariant::RnjdEnhanced;
        }

        if let Some(&variant) = self.assignments.get(market_slug) {
            return variant;
        }

        // Generate random assignment
        let variant = if self.next_random() < self.config.rnjd_probability {
            ModelVariant::RnjdEnhanced
        } else {
            ModelVariant::Legacy
        };

        self.assignments.insert(market_slug.to_string(), variant);
        variant
    }

    /// Check variant for a market without assigning
    pub fn peek_variant(&self, market_slug: &str) -> Option<ModelVariant> {
        self.assignments.get(market_slug).copied()
    }

    /// Record a trade result for a variant
    pub fn record_result(&mut self, variant: ModelVariant, pnl: f64) {
        match variant {
            ModelVariant::Legacy => {
                self.legacy_trades += 1;
                self.legacy_pnl += pnl;
            }
            ModelVariant::RnjdEnhanced => {
                self.rnjd_trades += 1;
                self.rnjd_pnl += pnl;
            }
        }
    }

    /// Get summary statistics
    pub fn summary(&self) -> ABTestSummary {
        let market_assignments: HashMap<String, String> = self
            .assignments
            .iter()
            .map(|(k, v)| {
                (
                    k.clone(),
                    match v {
                        ModelVariant::Legacy => "legacy".to_string(),
                        ModelVariant::RnjdEnhanced => "rnjd".to_string(),
                    },
                )
            })
            .collect();

        ABTestSummary {
            enabled: self.config.enabled,
            legacy_trades: self.legacy_trades,
            legacy_pnl: self.legacy_pnl,
            legacy_avg_pnl: if self.legacy_trades > 0 {
                self.legacy_pnl / self.legacy_trades as f64
            } else {
                0.0
            },
            rnjd_trades: self.rnjd_trades,
            rnjd_pnl: self.rnjd_pnl,
            rnjd_avg_pnl: if self.rnjd_trades > 0 {
                self.rnjd_pnl / self.rnjd_trades as f64
            } else {
                0.0
            },
            market_assignments,
        }
    }

    /// Reset all statistics (but keep assignments)
    pub fn reset_stats(&mut self) {
        self.legacy_trades = 0;
        self.legacy_pnl = 0.0;
        self.rnjd_trades = 0;
        self.rnjd_pnl = 0.0;
    }

    /// Clear all assignments and stats
    pub fn clear(&mut self) {
        self.assignments.clear();
        self.reset_stats();
    }

    /// Get the configuration
    pub fn config(&self) -> &ABTestConfig {
        &self.config
    }

    /// Update configuration (clears assignments if probability changes)
    pub fn update_config(&mut self, config: ABTestConfig) {
        if (config.rnjd_probability - self.config.rnjd_probability).abs() > 0.001 {
            self.assignments.clear();
        }
        self.config = config;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_disabled_returns_rnjd() {
        let mut tracker = ABTestTracker::new(ABTestConfig {
            enabled: false,
            rnjd_probability: 0.5,
        });

        let variant = tracker.get_variant("test-market");
        assert_eq!(variant, ModelVariant::RnjdEnhanced);
    }

    #[test]
    fn test_consistent_assignment() {
        let mut tracker = ABTestTracker::new(ABTestConfig {
            enabled: true,
            rnjd_probability: 0.5,
        });

        let v1 = tracker.get_variant("test-market");
        let v2 = tracker.get_variant("test-market");
        let v3 = tracker.get_variant("test-market");

        assert_eq!(v1, v2);
        assert_eq!(v2, v3);
    }

    #[test]
    fn test_record_result() {
        let mut tracker = ABTestTracker::new(ABTestConfig::default());

        tracker.record_result(ModelVariant::Legacy, 10.0);
        tracker.record_result(ModelVariant::Legacy, -5.0);
        tracker.record_result(ModelVariant::RnjdEnhanced, 15.0);

        let summary = tracker.summary();
        assert_eq!(summary.legacy_trades, 2);
        assert!((summary.legacy_pnl - 5.0).abs() < 0.01);
        assert_eq!(summary.rnjd_trades, 1);
        assert!((summary.rnjd_pnl - 15.0).abs() < 0.01);
    }

    #[test]
    fn test_distribution() {
        let mut tracker = ABTestTracker::new(ABTestConfig {
            enabled: true,
            rnjd_probability: 0.5,
        });

        let mut rnjd_count = 0;
        let mut legacy_count = 0;

        for i in 0..100 {
            match tracker.get_variant(&format!("market-{}", i)) {
                ModelVariant::RnjdEnhanced => rnjd_count += 1,
                ModelVariant::Legacy => legacy_count += 1,
            }
        }

        // With 50/50 split, we expect roughly equal distribution
        // Allow for some variance (30-70 range)
        assert!(rnjd_count >= 30 && rnjd_count <= 70);
        assert!(legacy_count >= 30 && legacy_count <= 70);
    }

    #[test]
    fn test_summary() {
        let mut tracker = ABTestTracker::new(ABTestConfig {
            enabled: true,
            rnjd_probability: 0.5,
        });

        tracker.get_variant("market-a");
        tracker.get_variant("market-b");
        tracker.record_result(ModelVariant::RnjdEnhanced, 100.0);

        let summary = tracker.summary();
        assert!(summary.enabled);
        assert_eq!(summary.market_assignments.len(), 2);
    }
}

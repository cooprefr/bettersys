//! Snapshot Sufficiency Analysis for Queue Modeling
//!
//! This module analyzes whether periodic L2 snapshots are sufficient for
//! conservative queue position modeling. Key insight:
//!
//! **Queue modeling with snapshots requires knowing:**
//! 1. Snapshot inter-arrival times (to bound how much queue could have changed)
//! 2. Typical order flow at each price level (to estimate queue consumption rate)
//! 3. Whether we can conservatively bound "queue ahead" between snapshots
//!
//! **Conservative model possibilities:**
//! - `Conservative`: Can bound queue_ahead pessimistically (always assume worst case)
//! - `Impossible`: Snapshot gaps are too large to bound queue movement
//!
//! **Critical thresholds:**
//! - For 15m Up/Down markets with ~60s market lifetime, snapshots every 100ms
//!   might allow conservative bounding, but 5s gaps make it impossible.

use crate::backtest_v2::book_recorder::{BookSnapshotStorage, RecordedBookSnapshot};
use crate::backtest_v2::clock::Nanos;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// =============================================================================
// Inter-Arrival Statistics
// =============================================================================

/// Statistics for snapshot inter-arrival times.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InterArrivalStats {
    /// Number of inter-arrival samples.
    pub sample_count: usize,
    /// Minimum inter-arrival time (nanoseconds).
    pub min_ns: u64,
    /// Maximum inter-arrival time (nanoseconds).
    pub max_ns: u64,
    /// Mean inter-arrival time (nanoseconds).
    pub mean_ns: f64,
    /// Median inter-arrival time (nanoseconds).
    pub median_ns: u64,
    /// 95th percentile inter-arrival time (nanoseconds).
    pub p95_ns: u64,
    /// 99th percentile inter-arrival time (nanoseconds).
    pub p99_ns: u64,
    /// Standard deviation (nanoseconds).
    pub std_dev_ns: f64,
}

impl InterArrivalStats {
    /// Compute from a sorted list of inter-arrival times.
    pub fn from_sorted(inter_arrivals: &[u64]) -> Option<Self> {
        if inter_arrivals.is_empty() {
            return None;
        }

        let n = inter_arrivals.len();
        let sum: u64 = inter_arrivals.iter().sum();
        let mean = sum as f64 / n as f64;

        let variance: f64 = inter_arrivals
            .iter()
            .map(|&x| {
                let diff = x as f64 - mean;
                diff * diff
            })
            .sum::<f64>()
            / n as f64;
        let std_dev = variance.sqrt();

        Some(Self {
            sample_count: n,
            min_ns: inter_arrivals[0],
            max_ns: inter_arrivals[n - 1],
            mean_ns: mean,
            median_ns: inter_arrivals[n / 2],
            p95_ns: inter_arrivals[(n as f64 * 0.95) as usize].min(inter_arrivals[n - 1]),
            p99_ns: inter_arrivals[(n as f64 * 0.99) as usize].min(inter_arrivals[n - 1]),
            std_dev_ns: std_dev,
        })
    }

    /// Convert nanoseconds to human-readable string.
    fn format_duration(ns: u64) -> String {
        if ns < 1_000 {
            format!("{}ns", ns)
        } else if ns < 1_000_000 {
            format!("{:.1}us", ns as f64 / 1_000.0)
        } else if ns < 1_000_000_000 {
            format!("{:.1}ms", ns as f64 / 1_000_000.0)
        } else {
            format!("{:.2}s", ns as f64 / 1_000_000_000.0)
        }
    }

    /// Get a human-readable summary.
    pub fn summary(&self) -> String {
        format!(
            "n={}, min={}, median={}, p95={}, p99={}, max={}",
            self.sample_count,
            Self::format_duration(self.min_ns),
            Self::format_duration(self.median_ns),
            Self::format_duration(self.p95_ns),
            Self::format_duration(self.p99_ns),
            Self::format_duration(self.max_ns),
        )
    }
}

// =============================================================================
// Queue Modeling Capability
// =============================================================================

/// Classification of queue modeling capability based on snapshot data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum QueueModelingCapability {
    /// Can model queue position conservatively.
    ///
    /// Requires:
    /// - Snapshot inter-arrival P99 < threshold (e.g., 500ms)
    /// - Can pessimistically bound queue_ahead between snapshots
    /// - Fills only credited when conservative bound allows
    Conservative,

    /// Cannot model queue position - gaps too large.
    ///
    /// Reasons:
    /// - Snapshot inter-arrival P99 > threshold
    /// - Queue could change arbitrarily between snapshots
    /// - Cannot bound fills conservatively
    Impossible,
}

impl QueueModelingCapability {
    /// Check if maker fills can be trusted.
    pub fn supports_maker_fills(&self) -> bool {
        matches!(self, Self::Conservative)
    }

    /// Get explanation of capability.
    pub fn explanation(&self) -> &'static str {
        match self {
            Self::Conservative => {
                "Snapshot frequency allows conservative queue bounding. \
                 Maker fills can be credited when pessimistic queue_ahead is exhausted."
            }
            Self::Impossible => {
                "Snapshot gaps are too large to bound queue movement. \
                 Maker fills cannot be credited because queue position is unknown."
            }
        }
    }
}

// =============================================================================
// Sufficiency Thresholds
// =============================================================================

/// Thresholds for determining snapshot sufficiency.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SufficiencyThresholds {
    /// Maximum P99 inter-arrival time for Conservative capability (nanoseconds).
    /// Default: 500ms (0.5 second)
    pub max_p99_inter_arrival_ns: u64,

    /// Maximum P95 inter-arrival time (secondary check).
    /// Default: 200ms
    pub max_p95_inter_arrival_ns: u64,

    /// Minimum sample count for reliable statistics.
    /// Default: 100
    pub min_sample_count: usize,

    /// Maximum allowed gap (absolute worst case).
    /// Default: 2 seconds
    pub max_gap_ns: u64,
}

impl Default for SufficiencyThresholds {
    fn default() -> Self {
        Self {
            // For 15m markets, we need fairly tight snapshot bounds.
            // At P99=500ms, we can pessimistically assume full queue turnover
            // within that window and still make progress.
            max_p99_inter_arrival_ns: 500_000_000, // 500ms
            max_p95_inter_arrival_ns: 200_000_000, // 200ms
            min_sample_count: 100,
            max_gap_ns: 2_000_000_000, // 2s absolute max
        }
    }
}

impl SufficiencyThresholds {
    /// Strict thresholds for production-grade backtesting.
    pub fn strict() -> Self {
        Self {
            max_p99_inter_arrival_ns: 100_000_000,  // 100ms
            max_p95_inter_arrival_ns: 50_000_000,   // 50ms
            min_sample_count: 1000,
            max_gap_ns: 500_000_000, // 500ms
        }
    }

    /// Relaxed thresholds for exploratory analysis.
    pub fn relaxed() -> Self {
        Self {
            max_p99_inter_arrival_ns: 2_000_000_000, // 2s
            max_p95_inter_arrival_ns: 1_000_000_000, // 1s
            min_sample_count: 50,
            max_gap_ns: 5_000_000_000, // 5s
        }
    }
}

// =============================================================================
// Token Snapshot Analysis
// =============================================================================

/// Analysis result for a single token.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenSnapshotAnalysis {
    /// Token ID.
    pub token_id: String,
    /// Total snapshots analyzed.
    pub snapshot_count: usize,
    /// Time range covered (start, end) in nanoseconds.
    pub time_range_ns: (u64, u64),
    /// Inter-arrival statistics.
    pub inter_arrival: Option<InterArrivalStats>,
    /// Sequence gaps detected.
    pub sequence_gaps: usize,
    /// Queue modeling capability assessment.
    pub capability: QueueModelingCapability,
    /// Reasons for capability classification.
    pub reasons: Vec<String>,
}

impl TokenSnapshotAnalysis {
    /// Get coverage duration in seconds.
    pub fn coverage_seconds(&self) -> f64 {
        (self.time_range_ns.1 - self.time_range_ns.0) as f64 / 1_000_000_000.0
    }

    /// Get average snapshots per second.
    pub fn snapshots_per_second(&self) -> f64 {
        let coverage = self.coverage_seconds();
        if coverage > 0.0 {
            self.snapshot_count as f64 / coverage
        } else {
            0.0
        }
    }
}

// =============================================================================
// Snapshot Frequency Analyzer
// =============================================================================

/// Analyzes snapshot frequency to determine queue modeling capability.
pub struct SnapshotFrequencyAnalyzer {
    thresholds: SufficiencyThresholds,
}

impl SnapshotFrequencyAnalyzer {
    /// Create with default thresholds.
    pub fn new() -> Self {
        Self {
            thresholds: SufficiencyThresholds::default(),
        }
    }

    /// Create with custom thresholds.
    pub fn with_thresholds(thresholds: SufficiencyThresholds) -> Self {
        Self { thresholds }
    }

    /// Analyze snapshots for a single token.
    pub fn analyze_token(&self, snapshots: &[RecordedBookSnapshot]) -> TokenSnapshotAnalysis {
        if snapshots.is_empty() {
            return TokenSnapshotAnalysis {
                token_id: String::new(),
                snapshot_count: 0,
                time_range_ns: (0, 0),
                inter_arrival: None,
                sequence_gaps: 0,
                capability: QueueModelingCapability::Impossible,
                reasons: vec!["No snapshots available".to_string()],
            };
        }

        let token_id = snapshots[0].token_id.clone();
        let start_ns = snapshots.first().unwrap().arrival_time_ns;
        let end_ns = snapshots.last().unwrap().arrival_time_ns;

        // Compute inter-arrival times
        let mut inter_arrivals: Vec<u64> = snapshots
            .windows(2)
            .map(|w| w[1].arrival_time_ns.saturating_sub(w[0].arrival_time_ns))
            .collect();
        inter_arrivals.sort_unstable();

        let inter_arrival = InterArrivalStats::from_sorted(&inter_arrivals);

        // Count sequence gaps
        let sequence_gaps = self.count_sequence_gaps(snapshots);

        // Determine capability
        let (capability, reasons) = self.assess_capability(&inter_arrival, snapshots.len());

        TokenSnapshotAnalysis {
            token_id,
            snapshot_count: snapshots.len(),
            time_range_ns: (start_ns, end_ns),
            inter_arrival,
            sequence_gaps,
            capability,
            reasons,
        }
    }

    /// Analyze snapshots from storage.
    pub fn analyze_from_storage(
        &self,
        storage: &BookSnapshotStorage,
        token_id: &str,
        start_ns: u64,
        end_ns: u64,
    ) -> Result<TokenSnapshotAnalysis> {
        let snapshots = storage.load_snapshots_in_range(token_id, start_ns, end_ns)?;
        Ok(self.analyze_token(&snapshots))
    }

    /// Analyze all tokens in storage.
    pub fn analyze_all_tokens(
        &self,
        storage: &BookSnapshotStorage,
        start_ns: u64,
        end_ns: u64,
    ) -> Result<HashMap<String, TokenSnapshotAnalysis>> {
        let token_ids = storage.get_token_ids()?;
        let mut results = HashMap::new();

        for token_id in token_ids {
            let analysis = self.analyze_from_storage(storage, &token_id, start_ns, end_ns)?;
            results.insert(token_id, analysis);
        }

        Ok(results)
    }

    fn count_sequence_gaps(&self, snapshots: &[RecordedBookSnapshot]) -> usize {
        let mut gaps = 0;
        let mut prev_seq: Option<u64> = None;

        for snap in snapshots {
            if let Some(seq) = snap.exchange_seq {
                if let Some(prev) = prev_seq {
                    if seq > prev + 1 {
                        gaps += 1;
                    }
                }
                prev_seq = Some(seq);
            }
        }

        gaps
    }

    fn assess_capability(
        &self,
        stats: &Option<InterArrivalStats>,
        sample_count: usize,
    ) -> (QueueModelingCapability, Vec<String>) {
        let mut reasons = Vec::new();

        // Check sample count
        if sample_count < self.thresholds.min_sample_count {
            reasons.push(format!(
                "Insufficient samples: {} < {} required",
                sample_count, self.thresholds.min_sample_count
            ));
            return (QueueModelingCapability::Impossible, reasons);
        }

        let stats = match stats {
            Some(s) => s,
            None => {
                reasons.push("Could not compute inter-arrival statistics".to_string());
                return (QueueModelingCapability::Impossible, reasons);
            }
        };

        // Check P99 threshold
        if stats.p99_ns > self.thresholds.max_p99_inter_arrival_ns {
            reasons.push(format!(
                "P99 inter-arrival {} > {} threshold",
                InterArrivalStats::format_duration(stats.p99_ns),
                InterArrivalStats::format_duration(self.thresholds.max_p99_inter_arrival_ns),
            ));
        }

        // Check P95 threshold
        if stats.p95_ns > self.thresholds.max_p95_inter_arrival_ns {
            reasons.push(format!(
                "P95 inter-arrival {} > {} threshold",
                InterArrivalStats::format_duration(stats.p95_ns),
                InterArrivalStats::format_duration(self.thresholds.max_p95_inter_arrival_ns),
            ));
        }

        // Check absolute max gap
        if stats.max_ns > self.thresholds.max_gap_ns {
            reasons.push(format!(
                "Max gap {} > {} absolute threshold",
                InterArrivalStats::format_duration(stats.max_ns),
                InterArrivalStats::format_duration(self.thresholds.max_gap_ns),
            ));
        }

        // Determine capability
        if reasons.is_empty() {
            reasons.push(format!(
                "P99={} within threshold, conservative queue bounding possible",
                InterArrivalStats::format_duration(stats.p99_ns),
            ));
            (QueueModelingCapability::Conservative, reasons)
        } else {
            (QueueModelingCapability::Impossible, reasons)
        }
    }
}

impl Default for SnapshotFrequencyAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

// =============================================================================
// Aggregate Report
// =============================================================================

/// Aggregate analysis report across multiple tokens.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotSufficiencyReport {
    /// Thresholds used for analysis.
    pub thresholds: SufficiencyThresholds,
    /// Total tokens analyzed.
    pub tokens_analyzed: usize,
    /// Tokens with Conservative capability.
    pub tokens_conservative: usize,
    /// Tokens with Impossible capability.
    pub tokens_impossible: usize,
    /// Per-token analyses.
    pub token_analyses: HashMap<String, TokenSnapshotAnalysis>,
    /// Overall capability (worst case across all tokens).
    pub overall_capability: QueueModelingCapability,
    /// Aggregate inter-arrival stats (across all tokens).
    pub aggregate_inter_arrival: Option<InterArrivalStats>,
}

impl SnapshotSufficiencyReport {
    /// Generate report from per-token analyses.
    pub fn from_analyses(
        thresholds: SufficiencyThresholds,
        analyses: HashMap<String, TokenSnapshotAnalysis>,
    ) -> Self {
        let tokens_analyzed = analyses.len();
        let tokens_conservative = analyses
            .values()
            .filter(|a| a.capability == QueueModelingCapability::Conservative)
            .count();
        let tokens_impossible = analyses
            .values()
            .filter(|a| a.capability == QueueModelingCapability::Impossible)
            .count();

        // Overall capability is worst case
        let overall_capability = if tokens_impossible > 0 {
            QueueModelingCapability::Impossible
        } else if tokens_conservative > 0 {
            QueueModelingCapability::Conservative
        } else {
            QueueModelingCapability::Impossible
        };

        // Aggregate inter-arrival stats
        let mut all_inter_arrivals: Vec<u64> = Vec::new();
        for analysis in analyses.values() {
            if let Some(stats) = &analysis.inter_arrival {
                // We don't have the raw data, but we can note the range
                // For a proper aggregate, we'd need the raw inter-arrival times
            }
        }

        Self {
            thresholds,
            tokens_analyzed,
            tokens_conservative,
            tokens_impossible,
            token_analyses: analyses,
            overall_capability,
            aggregate_inter_arrival: None, // Would require raw data
        }
    }

    /// Get a summary string.
    pub fn summary(&self) -> String {
        format!(
            "Snapshot Sufficiency Report:\n\
             - Tokens analyzed: {}\n\
             - Conservative: {} ({:.1}%)\n\
             - Impossible: {} ({:.1}%)\n\
             - Overall: {:?}",
            self.tokens_analyzed,
            self.tokens_conservative,
            100.0 * self.tokens_conservative as f64 / self.tokens_analyzed.max(1) as f64,
            self.tokens_impossible,
            100.0 * self.tokens_impossible as f64 / self.tokens_analyzed.max(1) as f64,
            self.overall_capability,
        )
    }

    /// Check if the dataset supports conservative queue modeling.
    pub fn supports_conservative_queue_modeling(&self) -> bool {
        self.overall_capability == QueueModelingCapability::Conservative
    }
}

// =============================================================================
// Conservative Queue Bound Model
// =============================================================================

/// Conservative queue bound estimation between snapshots.
///
/// When we only have periodic snapshots (not full deltas), we cannot know
/// the exact queue position. However, we can compute a *pessimistic* bound:
///
/// ```text
/// conservative_queue_ahead = max(
///     size_at_level_before_our_order in prev_snapshot,
///     size_at_level_before_our_order in next_snapshot
/// )
/// ```
///
/// This is pessimistic because:
/// - We assume NO fills happened to consume queue (worst case for us)
/// - We assume new orders arrived at front of queue (worst case for us)
///
/// A fill is only credited when:
/// 1. Trade consumes our price level
/// 2. Trade size > conservative_queue_ahead
/// 3. We can prove our order would have been at front
#[derive(Debug, Clone)]
pub struct ConservativeQueueBound {
    /// Our order ID.
    pub order_id: u64,
    /// Price level (ticks).
    pub price_ticks: i64,
    /// Conservative queue ahead estimate.
    pub queue_ahead_bound: f64,
    /// Last snapshot time used.
    pub snapshot_time_ns: u64,
    /// Next snapshot time (if known).
    pub next_snapshot_time_ns: Option<u64>,
    /// Size at level in current snapshot.
    pub size_at_level: f64,
}

impl ConservativeQueueBound {
    /// Check if a trade of given size would reach our order (conservatively).
    pub fn would_fill(&self, trade_size: f64) -> bool {
        trade_size > self.queue_ahead_bound
    }

    /// Compute fill amount (conservatively).
    pub fn fill_amount(&self, trade_size: f64, our_size: f64) -> f64 {
        if trade_size <= self.queue_ahead_bound {
            0.0
        } else {
            (trade_size - self.queue_ahead_bound).min(our_size)
        }
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backtest_v2::book_recorder::PriceLevel;

    fn make_snapshot(token_id: &str, arrival_ns: u64, seq: u64) -> RecordedBookSnapshot {
        RecordedBookSnapshot {
            token_id: token_id.to_string(),
            exchange_seq: Some(seq),
            source_time_ns: Some(arrival_ns.saturating_sub(1_000_000)),
            arrival_time_ns: arrival_ns,
            local_seq: seq,
            bids: vec![PriceLevel { price: 0.50, size: 100.0 }],
            asks: vec![PriceLevel { price: 0.51, size: 100.0 }],
            best_bid: Some(0.50),
            best_ask: Some(0.51),
            mid_price: Some(0.505),
            spread: Some(0.01),
        }
    }

    #[test]
    fn test_inter_arrival_stats() {
        let arrivals = vec![10, 20, 30, 50, 100];
        let stats = InterArrivalStats::from_sorted(&arrivals).unwrap();

        assert_eq!(stats.sample_count, 5);
        assert_eq!(stats.min_ns, 10);
        assert_eq!(stats.max_ns, 100);
        assert_eq!(stats.median_ns, 30);
    }

    #[test]
    fn test_analyzer_conservative() {
        let analyzer = SnapshotFrequencyAnalyzer::with_thresholds(SufficiencyThresholds {
            max_p99_inter_arrival_ns: 1_000_000_000, // 1s
            max_p95_inter_arrival_ns: 500_000_000,   // 500ms
            min_sample_count: 10,
            max_gap_ns: 2_000_000_000, // 2s
        });

        // Create snapshots every 100ms for 10 seconds
        let snapshots: Vec<_> = (0..100)
            .map(|i| make_snapshot("TOKEN1", i * 100_000_000, i as u64))
            .collect();

        let analysis = analyzer.analyze_token(&snapshots);

        assert_eq!(analysis.capability, QueueModelingCapability::Conservative);
        assert_eq!(analysis.snapshot_count, 100);
        assert!(analysis.inter_arrival.is_some());

        let stats = analysis.inter_arrival.unwrap();
        assert_eq!(stats.min_ns, 100_000_000); // 100ms
        assert_eq!(stats.max_ns, 100_000_000); // 100ms (uniform)
    }

    #[test]
    fn test_analyzer_impossible_large_gaps() {
        let analyzer = SnapshotFrequencyAnalyzer::with_thresholds(SufficiencyThresholds {
            max_p99_inter_arrival_ns: 500_000_000, // 500ms
            max_p95_inter_arrival_ns: 200_000_000, // 200ms
            min_sample_count: 10,
            max_gap_ns: 1_000_000_000, // 1s
        });

        // Create snapshots with large gaps (2 seconds)
        let snapshots: Vec<_> = (0..20)
            .map(|i| make_snapshot("TOKEN1", i * 2_000_000_000, i as u64))
            .collect();

        let analysis = analyzer.analyze_token(&snapshots);

        assert_eq!(analysis.capability, QueueModelingCapability::Impossible);
        assert!(!analysis.reasons.is_empty());
    }

    #[test]
    fn test_analyzer_insufficient_samples() {
        let analyzer = SnapshotFrequencyAnalyzer::with_thresholds(SufficiencyThresholds {
            min_sample_count: 100,
            ..Default::default()
        });

        let snapshots: Vec<_> = (0..10)
            .map(|i| make_snapshot("TOKEN1", i * 100_000_000, i as u64))
            .collect();

        let analysis = analyzer.analyze_token(&snapshots);

        assert_eq!(analysis.capability, QueueModelingCapability::Impossible);
        assert!(analysis.reasons[0].contains("Insufficient samples"));
    }

    #[test]
    fn test_conservative_queue_bound_fill() {
        let bound = ConservativeQueueBound {
            order_id: 1,
            price_ticks: 50,
            queue_ahead_bound: 100.0,
            snapshot_time_ns: 1_000_000_000,
            next_snapshot_time_ns: Some(1_100_000_000),
            size_at_level: 150.0,
        };

        // Trade smaller than queue_ahead - no fill
        assert!(!bound.would_fill(50.0));
        assert_eq!(bound.fill_amount(50.0, 25.0), 0.0);

        // Trade exactly at queue_ahead - no fill (conservative)
        assert!(!bound.would_fill(100.0));
        assert_eq!(bound.fill_amount(100.0, 25.0), 0.0);

        // Trade larger than queue_ahead - fill
        assert!(bound.would_fill(150.0));
        assert_eq!(bound.fill_amount(150.0, 25.0), 25.0); // Fill capped at our_size

        // Large trade
        assert_eq!(bound.fill_amount(200.0, 25.0), 25.0);
    }

    #[test]
    fn test_report_generation() {
        let mut analyses = HashMap::new();

        // Add a conservative token
        analyses.insert(
            "TOKEN1".to_string(),
            TokenSnapshotAnalysis {
                token_id: "TOKEN1".to_string(),
                snapshot_count: 1000,
                time_range_ns: (0, 100_000_000_000),
                inter_arrival: Some(InterArrivalStats {
                    sample_count: 999,
                    min_ns: 100_000_000,
                    max_ns: 150_000_000,
                    mean_ns: 100_000_000.0,
                    median_ns: 100_000_000,
                    p95_ns: 120_000_000,
                    p99_ns: 140_000_000,
                    std_dev_ns: 10_000_000.0,
                }),
                sequence_gaps: 0,
                capability: QueueModelingCapability::Conservative,
                reasons: vec!["Within thresholds".to_string()],
            },
        );

        // Add an impossible token
        analyses.insert(
            "TOKEN2".to_string(),
            TokenSnapshotAnalysis {
                token_id: "TOKEN2".to_string(),
                snapshot_count: 100,
                time_range_ns: (0, 100_000_000_000),
                inter_arrival: Some(InterArrivalStats {
                    sample_count: 99,
                    min_ns: 500_000_000,
                    max_ns: 5_000_000_000,
                    mean_ns: 1_000_000_000.0,
                    median_ns: 1_000_000_000,
                    p95_ns: 2_000_000_000,
                    p99_ns: 4_000_000_000,
                    std_dev_ns: 1_000_000_000.0,
                }),
                sequence_gaps: 5,
                capability: QueueModelingCapability::Impossible,
                reasons: vec!["Gaps too large".to_string()],
            },
        );

        let report = SnapshotSufficiencyReport::from_analyses(
            SufficiencyThresholds::default(),
            analyses,
        );

        assert_eq!(report.tokens_analyzed, 2);
        assert_eq!(report.tokens_conservative, 1);
        assert_eq!(report.tokens_impossible, 1);
        assert_eq!(report.overall_capability, QueueModelingCapability::Impossible);
        assert!(!report.supports_conservative_queue_modeling());
    }
}

//! Settlement Reference Source Trait and Implementations
//!
//! Provides the canonical interface for settlement price lookup with:
//! - Configurable reference rules (at-or-before, first-after, closest)
//! - Arrival-time visibility semantics
//! - Support for both live and replay modes

use serde::{Deserialize, Serialize};
use std::sync::Arc;

use super::chainlink::{ChainlinkReplayFeed, ChainlinkRound};
use super::storage::OracleRoundStorage;
use crate::backtest_v2::clock::Nanos;

// =============================================================================
// Settlement Reference Rule
// =============================================================================

/// Rule for selecting the reference price relative to the cutoff time.
///
/// **CRITICAL**: This must match the actual Polymarket settlement semantics.
/// Different markets may use different rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SettlementReferenceRule {
    /// Use the last oracle update at or before the cutoff.
    /// This is the most conservative choice for "price at cutoff".
    LastUpdateAtOrBeforeCutoff,

    /// Use the first oracle update strictly after the cutoff.
    /// Used if settlement uses "first available price after window ends".
    FirstUpdateAfterCutoff,

    /// Use the closest oracle update to the cutoff (tie goes to before).
    /// Used if settlement uses "nearest price to cutoff".
    ClosestToCutoff,

    /// Use the closest oracle update (tie goes to after).
    ClosestToCutoffTieAfter,
}

impl Default for SettlementReferenceRule {
    fn default() -> Self {
        // Default to most conservative: last update at or before
        Self::LastUpdateAtOrBeforeCutoff
    }
}

// =============================================================================
// Oracle Price Point
// =============================================================================

/// A price point from the oracle with full metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OraclePricePoint {
    /// Price value (already decoded from fixed-point).
    pub price: f64,
    /// Oracle updated_at timestamp (Unix seconds).
    pub oracle_updated_at_unix_sec: u64,
    /// When we observed/arrived at this data (nanoseconds).
    pub observed_arrival_time_ns: u64,
    /// Chainlink round ID.
    pub round_id: u128,
    /// Asset symbol.
    pub asset_symbol: String,
    /// Feed ID.
    pub feed_id: String,
}

impl From<&ChainlinkRound> for OraclePricePoint {
    fn from(round: &ChainlinkRound) -> Self {
        Self {
            price: round.price(),
            oracle_updated_at_unix_sec: round.updated_at,
            observed_arrival_time_ns: round.ingest_arrival_time_ns,
            round_id: round.round_id,
            asset_symbol: round.asset_symbol.clone(),
            feed_id: round.feed_id.clone(),
        }
    }
}

// =============================================================================
// Settlement Reference Source Trait
// =============================================================================

/// Trait for settlement reference price lookup.
///
/// Implementations provide price lookup with:
/// - Time-based queries relative to cutoff
/// - Visibility checks based on arrival time
/// - Support for different reference rules
pub trait SettlementReferenceSource: Send + Sync {
    /// Get the reference price for settlement at the given cutoff.
    ///
    /// The `cutoff_time_unix_sec` is the window end time (source time).
    /// The `rule` determines which oracle update to select.
    fn reference_price(
        &self,
        cutoff_time_unix_sec: u64,
        rule: SettlementReferenceRule,
    ) -> Option<OraclePricePoint>;

    /// Get the reference price at or before the cutoff.
    fn reference_price_at_or_before(
        &self,
        cutoff_time_unix_sec: u64,
    ) -> Option<OraclePricePoint> {
        self.reference_price(cutoff_time_unix_sec, SettlementReferenceRule::LastUpdateAtOrBeforeCutoff)
    }

    /// Get the first reference price after the cutoff.
    fn reference_price_first_after(
        &self,
        cutoff_time_unix_sec: u64,
    ) -> Option<OraclePricePoint> {
        self.reference_price(cutoff_time_unix_sec, SettlementReferenceRule::FirstUpdateAfterCutoff)
    }

    /// Check if the settlement outcome is knowable at the given decision time.
    ///
    /// The outcome is NOT knowable until the oracle round used for reference
    /// has been OBSERVED (arrival_time <= decision_time).
    ///
    /// This enforces visibility semantics to prevent look-ahead bias.
    fn is_outcome_knowable(
        &self,
        decision_time_ns: u64,
        cutoff_unix_sec: u64,
        rule: SettlementReferenceRule,
    ) -> bool {
        if let Some(price_point) = self.reference_price(cutoff_unix_sec, rule) {
            // Outcome is knowable when the reference price has ARRIVED
            decision_time_ns >= price_point.observed_arrival_time_ns
        } else {
            // No reference price available - cannot settle
            false
        }
    }

    /// Get the asset symbol this source provides prices for.
    fn asset_symbol(&self) -> &str;

    /// Get the feed ID.
    fn feed_id(&self) -> &str;
}

// =============================================================================
// Chainlink Settlement Source (Replay Mode)
// =============================================================================

/// Chainlink settlement source using a replay feed (for backtesting).
pub struct ChainlinkSettlementSource {
    feed: ChainlinkReplayFeed,
    asset_symbol: String,
    feed_id: String,
}

impl ChainlinkSettlementSource {
    /// Create from a replay feed.
    pub fn new(feed: ChainlinkReplayFeed, asset_symbol: String, feed_id: String) -> Self {
        Self {
            feed,
            asset_symbol,
            feed_id,
        }
    }

    /// Create from stored rounds.
    pub fn from_rounds(rounds: Vec<ChainlinkRound>, asset_symbol: String, feed_id: String) -> Self {
        let feed = ChainlinkReplayFeed::new(rounds);
        Self::new(feed, asset_symbol, feed_id)
    }

    /// Create from storage for a time range.
    pub fn from_storage(
        storage: &OracleRoundStorage,
        feed_id: &str,
        asset_symbol: &str,
        start_ts: u64,
        end_ts: u64,
    ) -> anyhow::Result<Self> {
        let rounds = storage.load_rounds_in_range(feed_id, start_ts, end_ts)?;
        Ok(Self::from_rounds(
            rounds,
            asset_symbol.to_string(),
            feed_id.to_string(),
        ))
    }

    /// Get number of rounds loaded.
    pub fn round_count(&self) -> usize {
        self.feed.len()
    }

    /// Get time coverage.
    pub fn time_range(&self) -> Option<(u64, u64)> {
        self.feed.time_range()
    }

    /// Get underlying replay feed (for advanced queries).
    pub fn replay_feed(&self) -> &ChainlinkReplayFeed {
        &self.feed
    }
}

impl SettlementReferenceSource for ChainlinkSettlementSource {
    fn reference_price(
        &self,
        cutoff_time_unix_sec: u64,
        rule: SettlementReferenceRule,
    ) -> Option<OraclePricePoint> {
        let round = match rule {
            SettlementReferenceRule::LastUpdateAtOrBeforeCutoff => {
                self.feed.round_at_or_before(cutoff_time_unix_sec)
            }
            SettlementReferenceRule::FirstUpdateAfterCutoff => {
                self.feed.round_first_after(cutoff_time_unix_sec)
            }
            SettlementReferenceRule::ClosestToCutoff => {
                self.feed.round_closest(cutoff_time_unix_sec)
            }
            SettlementReferenceRule::ClosestToCutoffTieAfter => {
                // Closest with tie going to after
                let before = self.feed.round_at_or_before(cutoff_time_unix_sec);
                let after = self.feed.round_first_after(cutoff_time_unix_sec);
                
                match (before, after) {
                    (Some(b), Some(a)) => {
                        let diff_before = cutoff_time_unix_sec.saturating_sub(b.updated_at);
                        let diff_after = a.updated_at.saturating_sub(cutoff_time_unix_sec);
                        if diff_after <= diff_before {
                            Some(a)
                        } else {
                            Some(b)
                        }
                    }
                    (Some(b), None) => Some(b),
                    (None, Some(a)) => Some(a),
                    (None, None) => None,
                }
            }
        };

        round.map(OraclePricePoint::from)
    }

    fn asset_symbol(&self) -> &str {
        &self.asset_symbol
    }

    fn feed_id(&self) -> &str {
        &self.feed_id
    }
}

// =============================================================================
// Live Chainlink Settlement Source
// =============================================================================

/// Chainlink settlement source using live storage (for production).
pub struct LiveChainlinkSettlementSource {
    storage: Arc<OracleRoundStorage>,
    feed_id: String,
    asset_symbol: String,
}

impl LiveChainlinkSettlementSource {
    pub fn new(storage: Arc<OracleRoundStorage>, feed_id: String, asset_symbol: String) -> Self {
        Self {
            storage,
            feed_id,
            asset_symbol,
        }
    }
}

impl SettlementReferenceSource for LiveChainlinkSettlementSource {
    fn reference_price(
        &self,
        cutoff_time_unix_sec: u64,
        rule: SettlementReferenceRule,
    ) -> Option<OraclePricePoint> {
        let round = match rule {
            SettlementReferenceRule::LastUpdateAtOrBeforeCutoff => {
                self.storage
                    .get_round_at_or_before(&self.feed_id, cutoff_time_unix_sec)
                    .ok()?
            }
            SettlementReferenceRule::FirstUpdateAfterCutoff => {
                self.storage
                    .get_round_first_after(&self.feed_id, cutoff_time_unix_sec)
                    .ok()?
            }
            SettlementReferenceRule::ClosestToCutoff
            | SettlementReferenceRule::ClosestToCutoffTieAfter => {
                let before = self.storage
                    .get_round_at_or_before(&self.feed_id, cutoff_time_unix_sec)
                    .ok()?;
                let after = self.storage
                    .get_round_first_after(&self.feed_id, cutoff_time_unix_sec)
                    .ok()?;

                let tie_to_after = rule == SettlementReferenceRule::ClosestToCutoffTieAfter;

                match (before, after) {
                    (Some(b), Some(a)) => {
                        let diff_before = cutoff_time_unix_sec.saturating_sub(b.updated_at);
                        let diff_after = a.updated_at.saturating_sub(cutoff_time_unix_sec);
                        if tie_to_after {
                            if diff_after <= diff_before {
                                Some(a)
                            } else {
                                Some(b)
                            }
                        } else {
                            if diff_before <= diff_after {
                                Some(b)
                            } else {
                                Some(a)
                            }
                        }
                    }
                    (Some(b), None) => Some(b),
                    (None, Some(a)) => Some(a),
                    (None, None) => None,
                }
            }
        };

        round.map(|r| OraclePricePoint::from(&r))
    }

    fn asset_symbol(&self) -> &str {
        &self.asset_symbol
    }

    fn feed_id(&self) -> &str {
        &self.feed_id
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_rounds() -> Vec<ChainlinkRound> {
        vec![
            ChainlinkRound {
                feed_id: "test".to_string(),
                round_id: 1,
                answer: 5000000000000, // 50000.0
                updated_at: 1000,
                answered_in_round: 1,
                started_at: 1000,
                ingest_arrival_time_ns: 1000_500_000_000, // Arrives 500ms after update
                ingest_seq: 0,
                decimals: 8,
                asset_symbol: "BTC".to_string(),
                raw_source_hash: None,
            },
            ChainlinkRound {
                feed_id: "test".to_string(),
                round_id: 2,
                answer: 5010000000000, // 50100.0
                updated_at: 1100,
                answered_in_round: 2,
                started_at: 1100,
                ingest_arrival_time_ns: 1100_500_000_000,
                ingest_seq: 1,
                decimals: 8,
                asset_symbol: "BTC".to_string(),
                raw_source_hash: None,
            },
            ChainlinkRound {
                feed_id: "test".to_string(),
                round_id: 3,
                answer: 5020000000000, // 50200.0
                updated_at: 1200,
                answered_in_round: 3,
                started_at: 1200,
                ingest_arrival_time_ns: 1200_500_000_000,
                ingest_seq: 2,
                decimals: 8,
                asset_symbol: "BTC".to_string(),
                raw_source_hash: None,
            },
        ]
    }

    #[test]
    fn test_settlement_source_at_or_before() {
        let source = ChainlinkSettlementSource::from_rounds(
            make_test_rounds(),
            "BTC".to_string(),
            "test".to_string(),
        );

        // Exactly at round 2
        let p = source.reference_price_at_or_before(1100).unwrap();
        assert!((p.price - 50100.0).abs() < 0.01);
        assert_eq!(p.round_id, 2);

        // Between rounds
        let p = source.reference_price_at_or_before(1150).unwrap();
        assert_eq!(p.round_id, 2);

        // Before first round
        assert!(source.reference_price_at_or_before(999).is_none());
    }

    #[test]
    fn test_settlement_source_first_after() {
        let source = ChainlinkSettlementSource::from_rounds(
            make_test_rounds(),
            "BTC".to_string(),
            "test".to_string(),
        );

        // After round 1
        let p = source.reference_price_first_after(1000).unwrap();
        assert_eq!(p.round_id, 2);

        // After last round
        assert!(source.reference_price_first_after(1200).is_none());
    }

    #[test]
    fn test_outcome_knowable() {
        let source = ChainlinkSettlementSource::from_rounds(
            make_test_rounds(),
            "BTC".to_string(),
            "test".to_string(),
        );

        let cutoff = 1100;

        // Decision time before arrival - NOT knowable
        let decision_before = 1100_000_000_000; // 1100 seconds in ns
        assert!(!source.is_outcome_knowable(
            decision_before,
            cutoff,
            SettlementReferenceRule::LastUpdateAtOrBeforeCutoff
        ));

        // Decision time after arrival - IS knowable
        let decision_after = 1100_600_000_000; // 600ms after round 2's arrival
        assert!(source.is_outcome_knowable(
            decision_after,
            cutoff,
            SettlementReferenceRule::LastUpdateAtOrBeforeCutoff
        ));
    }

    #[test]
    fn test_closest_rule() {
        let source = ChainlinkSettlementSource::from_rounds(
            make_test_rounds(),
            "BTC".to_string(),
            "test".to_string(),
        );

        // Cutoff at 1040: closer to 1000 than 1100
        let p = source
            .reference_price(1040, SettlementReferenceRule::ClosestToCutoff)
            .unwrap();
        assert_eq!(p.round_id, 1);

        // Cutoff at 1060: closer to 1100 than 1000
        let p = source
            .reference_price(1060, SettlementReferenceRule::ClosestToCutoff)
            .unwrap();
        assert_eq!(p.round_id, 2);

        // Cutoff at 1050: equidistant, tie goes to before
        let p = source
            .reference_price(1050, SettlementReferenceRule::ClosestToCutoff)
            .unwrap();
        assert_eq!(p.round_id, 1);

        // Cutoff at 1050 with tie-to-after
        let p = source
            .reference_price(1050, SettlementReferenceRule::ClosestToCutoffTieAfter)
            .unwrap();
        assert_eq!(p.round_id, 2);
    }
}

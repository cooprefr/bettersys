//! Settlement Integration Tests
//!
//! Tests for boundary conditions, knowability, and orchestrator integration
//! of settlement with Chainlink reference sources.

use crate::backtest_v2::clock::Nanos;
use crate::backtest_v2::oracle::{ChainlinkRound, ChainlinkSettlementSource, SettlementReferenceRule, SettlementReferenceSource};
use crate::backtest_v2::portfolio::Outcome;
use crate::backtest_v2::settlement::{SettlementOutcome, SettlementSpec, TieRule};
use crate::backtest_v2::settlement_integration::{
    ChainlinkSettlementCoordinator, SettlementConfig, SettlementError, SettlementFingerprint,
    SettlementMetadata, WindowSettlementRecord, NS_PER_SEC,
};
use std::sync::Arc;

// =============================================================================
// TEST HELPERS
// =============================================================================

/// Create a Chainlink round at a specific time.
fn make_round(
    round_id: u128,
    updated_at_unix_sec: u64,
    price_scaled: i128,
    arrival_delay_sec: u64,
) -> ChainlinkRound {
    ChainlinkRound {
        feed_id: "btc-usd".to_string(),
        round_id,
        answer: price_scaled,
        updated_at: updated_at_unix_sec,
        answered_in_round: round_id,
        started_at: updated_at_unix_sec,
        ingest_arrival_time_ns: (updated_at_unix_sec + arrival_delay_sec) * NS_PER_SEC as u64,
        ingest_seq: round_id as u64,
        decimals: 8,
        asset_symbol: "BTC".to_string(),
        raw_source_hash: None,
    }
}

fn make_config_with_rule(rule: SettlementReferenceRule) -> SettlementConfig {
    SettlementConfig {
        spec: SettlementSpec::polymarket_15m_updown(),
        reference_rule: rule,
        tie_rule: TieRule::NoWins,
        require_chainlink: true,
        chainlink_feed_id: Some("btc-usd".to_string()),
        chainlink_asset_symbol: Some("BTC".to_string()),
        chainlink_chain_id: Some(1),
        chainlink_feed_proxy: None,
        max_oracle_staleness_ns: Some(120 * NS_PER_SEC), // 2 minutes
        abort_on_missing_oracle: true,
    }
}

// =============================================================================
// BOUNDARY TESTS: CUTOFF ± ε
// =============================================================================

/// Test: Round at T-1 is selected by LastUpdateAtOrBeforeCutoff
#[test]
fn test_boundary_round_before_cutoff_selected_by_at_or_before_rule() {
    let cutoff = 2000u64; // Window ends at T=2000
    
    let rounds = vec![
        make_round(1, cutoff - 10, 5000000000000, 1), // T-10 (before cutoff)
        make_round(2, cutoff - 1, 5010000000000, 1),  // T-1 (just before cutoff)
        make_round(3, cutoff + 1, 5020000000000, 1),  // T+1 (just after cutoff)
    ];
    
    let source = ChainlinkSettlementSource::from_rounds(rounds, "BTC".to_string(), "btc-usd".to_string());
    
    // LastUpdateAtOrBeforeCutoff should select round 2 (T-1)
    let price = source.reference_price(cutoff, SettlementReferenceRule::LastUpdateAtOrBeforeCutoff).unwrap();
    assert_eq!(price.round_id, 2, "Should select round at T-1");
}

/// Test: Round exactly at T is selected by LastUpdateAtOrBeforeCutoff
#[test]
fn test_boundary_round_exactly_at_cutoff_selected_by_at_or_before_rule() {
    let cutoff = 2000u64;
    
    let rounds = vec![
        make_round(1, cutoff - 10, 5000000000000, 1),
        make_round(2, cutoff, 5010000000000, 1),      // Exactly at cutoff
        make_round(3, cutoff + 1, 5020000000000, 1),
    ];
    
    let source = ChainlinkSettlementSource::from_rounds(rounds, "BTC".to_string(), "btc-usd".to_string());
    
    // LastUpdateAtOrBeforeCutoff should select round 2 (exactly at T)
    let price = source.reference_price(cutoff, SettlementReferenceRule::LastUpdateAtOrBeforeCutoff).unwrap();
    assert_eq!(price.round_id, 2, "Should select round exactly at cutoff");
}

/// Test: Round at T+1 is NOT selected by LastUpdateAtOrBeforeCutoff
#[test]
fn test_boundary_round_after_cutoff_not_selected_by_at_or_before_rule() {
    let cutoff = 2000u64;
    
    let rounds = vec![
        make_round(1, cutoff - 10, 5000000000000, 1),
        make_round(2, cutoff + 1, 5010000000000, 1),  // T+1 (after cutoff)
    ];
    
    let source = ChainlinkSettlementSource::from_rounds(rounds, "BTC".to_string(), "btc-usd".to_string());
    
    // LastUpdateAtOrBeforeCutoff should select round 1 (last before cutoff)
    let price = source.reference_price(cutoff, SettlementReferenceRule::LastUpdateAtOrBeforeCutoff).unwrap();
    assert_eq!(price.round_id, 1, "Should NOT select round after cutoff");
}

/// Test: FirstUpdateAfterCutoff selects T+1 but NOT T
#[test]
fn test_boundary_first_after_rule_selects_strictly_after() {
    let cutoff = 2000u64;
    
    let rounds = vec![
        make_round(1, cutoff - 1, 5000000000000, 1),  // T-1
        make_round(2, cutoff, 5010000000000, 1),      // Exactly at T
        make_round(3, cutoff + 1, 5020000000000, 1),  // T+1
    ];
    
    let source = ChainlinkSettlementSource::from_rounds(rounds, "BTC".to_string(), "btc-usd".to_string());
    
    // FirstUpdateAfterCutoff should select round 3 (T+1), not round 2 (T)
    let price = source.reference_price(cutoff, SettlementReferenceRule::FirstUpdateAfterCutoff).unwrap();
    assert_eq!(price.round_id, 3, "FirstUpdateAfterCutoff should select strictly after cutoff");
}

/// Test: ClosestToCutoff with equidistant rounds (tie goes to before)
#[test]
fn test_boundary_closest_tie_goes_to_before() {
    let cutoff = 2000u64;
    
    let rounds = vec![
        make_round(1, cutoff - 5, 5000000000000, 1),  // 5 seconds before
        make_round(2, cutoff + 5, 5010000000000, 1),  // 5 seconds after (equidistant)
    ];
    
    let source = ChainlinkSettlementSource::from_rounds(rounds, "BTC".to_string(), "btc-usd".to_string());
    
    // ClosestToCutoff with tie should go to before
    let price = source.reference_price(cutoff, SettlementReferenceRule::ClosestToCutoff).unwrap();
    assert_eq!(price.round_id, 1, "Tie should go to before");
}

/// Test: ClosestToCutoffTieAfter with equidistant rounds (tie goes to after)
#[test]
fn test_boundary_closest_tie_goes_to_after() {
    let cutoff = 2000u64;
    
    let rounds = vec![
        make_round(1, cutoff - 5, 5000000000000, 1),  // 5 seconds before
        make_round(2, cutoff + 5, 5010000000000, 1),  // 5 seconds after (equidistant)
    ];
    
    let source = ChainlinkSettlementSource::from_rounds(rounds, "BTC".to_string(), "btc-usd".to_string());
    
    // ClosestToCutoffTieAfter with tie should go to after
    let price = source.reference_price(cutoff, SettlementReferenceRule::ClosestToCutoffTieAfter).unwrap();
    assert_eq!(price.round_id, 2, "Tie should go to after");
}

/// Test: Closest selects nearer round when not equidistant
#[test]
fn test_boundary_closest_selects_nearer() {
    let cutoff = 2000u64;
    
    let rounds = vec![
        make_round(1, cutoff - 10, 5000000000000, 1), // 10 seconds before
        make_round(2, cutoff + 3, 5010000000000, 1),  // 3 seconds after (closer)
    ];
    
    let source = ChainlinkSettlementSource::from_rounds(rounds, "BTC".to_string(), "btc-usd".to_string());
    
    let price = source.reference_price(cutoff, SettlementReferenceRule::ClosestToCutoff).unwrap();
    assert_eq!(price.round_id, 2, "Should select closer round");
}

// =============================================================================
// KNOWABILITY TESTS
// =============================================================================

/// Test: Settlement does NOT occur until oracle round has ARRIVED
#[test]
fn test_knowability_settlement_blocked_before_arrival() {
    let window_start = 1000u64;
    let window_end = window_start + 900; // 15 minutes
    
    // Round updated_at is at cutoff, but arrival is 5 seconds later
    let rounds = vec![
        make_round(1, window_start, 5000000000000, 1),  // Start price
        make_round(2, window_end, 5010000000000, 5),    // End price arrives 5s late
    ];
    
    let config = make_config_with_rule(SettlementReferenceRule::LastUpdateAtOrBeforeCutoff);
    let mut coordinator = ChainlinkSettlementCoordinator::with_chainlink_replay(config, rounds);
    
    let market_id = format!("btc-updown-15m-{}", window_start);
    let start_ns = (window_start * NS_PER_SEC as u64) as Nanos;
    coordinator.track_window(&market_id, start_ns).unwrap();
    coordinator.observe_market_price(&market_id, 50000.0, start_ns, start_ns);
    
    let window_end_ns = (window_end * NS_PER_SEC as u64) as Nanos;
    coordinator.advance_time(window_end_ns);
    
    // Decision time at cutoff + 1 second (before oracle arrival at cutoff + 5s)
    let decision_before_arrival = ((window_end + 1) * NS_PER_SEC as u64) as Nanos;
    let result = coordinator.try_settle(&market_id, decision_before_arrival).unwrap();
    assert!(result.is_none(), "Settlement should be blocked before oracle arrival");
    
    // Decision time at cutoff + 5 seconds (at oracle arrival)
    let decision_at_arrival = ((window_end + 5) * NS_PER_SEC as u64) as Nanos;
    let result = coordinator.try_settle(&market_id, decision_at_arrival).unwrap();
    assert!(result.is_some(), "Settlement should occur at oracle arrival");
}

/// Test: is_outcome_knowable returns false before arrival
#[test]
fn test_knowability_is_outcome_knowable_semantics() {
    let cutoff = 2000u64;
    
    // Round arrives 10 seconds after being updated
    let rounds = vec![
        make_round(1, cutoff, 5010000000000, 10), // updated_at=2000, arrives at 2010
    ];
    
    let source = ChainlinkSettlementSource::from_rounds(rounds, "BTC".to_string(), "btc-usd".to_string());
    
    // Decision at cutoff + 5s (before arrival)
    let decision_before = (cutoff + 5) * NS_PER_SEC as u64;
    assert!(
        !source.is_outcome_knowable(decision_before, cutoff, SettlementReferenceRule::LastUpdateAtOrBeforeCutoff),
        "Outcome should NOT be knowable before arrival"
    );
    
    // Decision at cutoff + 10s (at arrival)
    let decision_at = (cutoff + 10) * NS_PER_SEC as u64;
    assert!(
        source.is_outcome_knowable(decision_at, cutoff, SettlementReferenceRule::LastUpdateAtOrBeforeCutoff),
        "Outcome should be knowable at arrival"
    );
    
    // Decision at cutoff + 15s (after arrival)
    let decision_after = (cutoff + 15) * NS_PER_SEC as u64;
    assert!(
        source.is_outcome_knowable(decision_after, cutoff, SettlementReferenceRule::LastUpdateAtOrBeforeCutoff),
        "Outcome should be knowable after arrival"
    );
}

/// Test: Different rounds have different arrival times
#[test]
fn test_knowability_per_round_arrival_times() {
    let cutoff = 2000u64;
    
    let rounds = vec![
        make_round(1, cutoff - 5, 5000000000000, 2),  // Updated T-5, arrives T-3
        make_round(2, cutoff, 5010000000000, 8),      // Updated T, arrives T+8
    ];
    
    let source = ChainlinkSettlementSource::from_rounds(rounds, "BTC".to_string(), "btc-usd".to_string());
    
    // Get the round that would be used for LastUpdateAtOrBeforeCutoff (round 2)
    let price = source.reference_price(cutoff, SettlementReferenceRule::LastUpdateAtOrBeforeCutoff).unwrap();
    assert_eq!(price.round_id, 2);
    
    // Outcome is knowable when round 2 has arrived (T+8)
    let decision_t_plus_7 = (cutoff + 7) * NS_PER_SEC as u64;
    assert!(
        !source.is_outcome_knowable(decision_t_plus_7, cutoff, SettlementReferenceRule::LastUpdateAtOrBeforeCutoff),
        "Outcome not knowable before round 2 arrival"
    );
    
    let decision_t_plus_8 = (cutoff + 8) * NS_PER_SEC as u64;
    assert!(
        source.is_outcome_knowable(decision_t_plus_8, cutoff, SettlementReferenceRule::LastUpdateAtOrBeforeCutoff),
        "Outcome knowable after round 2 arrival"
    );
}

// =============================================================================
// MISSING DATA TESTS
// =============================================================================

/// Test: Missing oracle data aborts when abort_on_missing_oracle = true
#[test]
fn test_missing_oracle_aborts_in_production_mode() {
    let window_start = 1000u64;
    let window_end = window_start + 900;
    
    // No rounds at all - truly missing oracle data
    let rounds = vec![];
    
    let config = SettlementConfig {
        require_chainlink: true,
        abort_on_missing_oracle: true,
        ..make_config_with_rule(SettlementReferenceRule::LastUpdateAtOrBeforeCutoff)
    };
    
    let mut coordinator = ChainlinkSettlementCoordinator::with_chainlink_replay(config, rounds);
    
    let market_id = format!("btc-updown-15m-{}", window_start);
    let start_ns = (window_start * NS_PER_SEC as u64) as Nanos;
    coordinator.track_window(&market_id, start_ns).unwrap();
    // Use observe_market_price for start price (from market data, not oracle)
    coordinator.observe_market_price(&market_id, 50000.0, start_ns, start_ns);
    
    let window_end_ns = (window_end * NS_PER_SEC as u64) as Nanos;
    coordinator.advance_time(window_end_ns);
    
    // Try to settle - should error due to missing oracle
    let decision_time = (window_end + 10) * NS_PER_SEC as u64;
    let result = coordinator.try_settle(&market_id, decision_time as Nanos);
    
    assert!(result.is_err(), "Should error with missing oracle data");
    match result.unwrap_err() {
        SettlementError::MissingOracleData { market_id: _, cutoff_unix_sec } => {
            assert_eq!(cutoff_unix_sec, window_end);
        }
        other => panic!("Wrong error type: {:?}", other),
    }
}

/// Test: Missing oracle records in metadata when abort = false
#[test]
fn test_missing_oracle_records_in_metadata() {
    let window_start = 1000u64;
    let window_end = window_start + 900;
    
    // No rounds at all
    let rounds = vec![];
    
    let config = SettlementConfig {
        require_chainlink: true,
        abort_on_missing_oracle: false, // Don't abort, just record
        ..make_config_with_rule(SettlementReferenceRule::LastUpdateAtOrBeforeCutoff)
    };
    
    let mut coordinator = ChainlinkSettlementCoordinator::with_chainlink_replay(config, rounds);
    
    let market_id = format!("btc-updown-15m-{}", window_start);
    let start_ns = (window_start * NS_PER_SEC as u64) as Nanos;
    coordinator.track_window(&market_id, start_ns).unwrap();
    coordinator.observe_market_price(&market_id, 50000.0, start_ns, start_ns);
    
    let window_end_ns = (window_end * NS_PER_SEC as u64) as Nanos;
    coordinator.advance_time(window_end_ns);
    
    // Try to settle - should not error but should record missing
    let decision_time = (window_end + 10) * NS_PER_SEC as u64;
    let result = coordinator.try_settle(&market_id, decision_time as Nanos).unwrap();
    
    assert!(result.is_none(), "Should not settle without oracle data");
    
    let metadata = coordinator.metadata();
    assert_eq!(metadata.windows_missing_oracle, 1, "Should record missing oracle");
}

// =============================================================================
// DETERMINISM TESTS
// =============================================================================

/// Test: Same inputs produce same settlement fingerprint
#[test]
fn test_deterministic_fingerprint() {
    let config1 = make_config_with_rule(SettlementReferenceRule::LastUpdateAtOrBeforeCutoff);
    let config2 = make_config_with_rule(SettlementReferenceRule::LastUpdateAtOrBeforeCutoff);
    
    let fp1 = config1.fingerprint();
    let fp2 = config2.fingerprint();
    
    assert_eq!(fp1.hash(), fp2.hash(), "Same config should produce same hash");
    assert_eq!(fp1, fp2, "Fingerprints should be equal");
}

/// Test: Different rules produce different fingerprints
#[test]
fn test_fingerprint_changes_on_rule_change() {
    let config1 = make_config_with_rule(SettlementReferenceRule::LastUpdateAtOrBeforeCutoff);
    let config2 = make_config_with_rule(SettlementReferenceRule::FirstUpdateAfterCutoff);
    
    let fp1 = config1.fingerprint();
    let fp2 = config2.fingerprint();
    
    assert_ne!(fp1.hash(), fp2.hash(), "Different rules should produce different hashes");
}

/// Test: Same settlement scenario produces identical results
#[test]
fn test_settlement_determinism() {
    let window_start = 1000u64;
    let window_end = window_start + 900;
    
    let rounds = vec![
        make_round(1, window_start, 5000000000000, 1),  // 50000.0 at start
        make_round(2, window_end, 5010000000000, 1),    // 50100.0 at end (UP)
    ];
    
    let config = make_config_with_rule(SettlementReferenceRule::LastUpdateAtOrBeforeCutoff);
    
    // Run 1
    let mut coord1 = ChainlinkSettlementCoordinator::with_chainlink_replay(config.clone(), rounds.clone());
    let market_id = format!("btc-updown-15m-{}", window_start);
    let start_ns = (window_start * NS_PER_SEC as u64) as Nanos;
    coord1.track_window(&market_id, start_ns).unwrap();
    coord1.observe_market_price(&market_id, 50000.0, start_ns, start_ns);
    coord1.advance_time((window_end * NS_PER_SEC as u64) as Nanos);
    let decision = (window_end + 5) * NS_PER_SEC as u64;
    let result1 = coord1.try_settle(&market_id, decision as Nanos).unwrap().unwrap();
    
    // Run 2 (identical)
    let mut coord2 = ChainlinkSettlementCoordinator::with_chainlink_replay(config, rounds);
    coord2.track_window(&market_id, start_ns).unwrap();
    coord2.observe_market_price(&market_id, 50000.0, start_ns, start_ns);
    coord2.advance_time((window_end * NS_PER_SEC as u64) as Nanos);
    let result2 = coord2.try_settle(&market_id, decision as Nanos).unwrap().unwrap();
    
    // Results should be identical
    assert_eq!(result1.0.start_price, result2.0.start_price);
    assert_eq!(result1.0.end_price, result2.0.end_price);
    assert_eq!(result1.0.outcome, result2.0.outcome);
    assert_eq!(result1.1.oracle_round_id, result2.1.oracle_round_id);
}

// =============================================================================
// OUTCOME TESTS
// =============================================================================

/// Test: Price up produces Yes (Up) wins
#[test]
fn test_outcome_up_wins_when_price_increases() {
    let window_start = 1000u64;
    let window_end = window_start + 900;
    
    let rounds = vec![
        make_round(1, window_start, 5000000000000, 1),  // 50000.0
        make_round(2, window_end, 5100000000000, 1),    // 51000.0 (UP by 1000)
    ];
    
    let config = make_config_with_rule(SettlementReferenceRule::LastUpdateAtOrBeforeCutoff);
    let mut coordinator = ChainlinkSettlementCoordinator::with_chainlink_replay(config, rounds);
    
    let market_id = format!("btc-updown-15m-{}", window_start);
    let start_ns = (window_start * NS_PER_SEC as u64) as Nanos;
    coordinator.track_window(&market_id, start_ns).unwrap();
    coordinator.observe_market_price(&market_id, 50000.0, start_ns, start_ns);
    coordinator.advance_time((window_end * NS_PER_SEC as u64) as Nanos);
    
    let decision = (window_end + 5) * NS_PER_SEC as u64;
    let (event, record) = coordinator.try_settle(&market_id, decision as Nanos).unwrap().unwrap();
    
    assert!(matches!(event.outcome, SettlementOutcome::Resolved { winner: Outcome::Yes, is_tie: false }));
    assert!(record.end_price > record.start_price);
}

/// Test: Price down produces No (Down) wins
#[test]
fn test_outcome_down_wins_when_price_decreases() {
    let window_start = 1000u64;
    let window_end = window_start + 900;
    
    let rounds = vec![
        make_round(1, window_start, 5100000000000, 1),  // 51000.0
        make_round(2, window_end, 5000000000000, 1),    // 50000.0 (DOWN by 1000)
    ];
    
    let config = make_config_with_rule(SettlementReferenceRule::LastUpdateAtOrBeforeCutoff);
    let mut coordinator = ChainlinkSettlementCoordinator::with_chainlink_replay(config, rounds);
    
    let market_id = format!("btc-updown-15m-{}", window_start);
    let start_ns = (window_start * NS_PER_SEC as u64) as Nanos;
    coordinator.track_window(&market_id, start_ns).unwrap();
    coordinator.observe_market_price(&market_id, 51000.0, start_ns, start_ns);
    coordinator.advance_time((window_end * NS_PER_SEC as u64) as Nanos);
    
    let decision = (window_end + 5) * NS_PER_SEC as u64;
    let (event, _record) = coordinator.try_settle(&market_id, decision as Nanos).unwrap().unwrap();
    
    assert!(matches!(event.outcome, SettlementOutcome::Resolved { winner: Outcome::No, is_tie: false }));
}

/// Test: Tie (same price) produces No wins with TieRule::NoWins
#[test]
fn test_outcome_tie_goes_to_no_with_no_wins_rule() {
    let window_start = 1000u64;
    let window_end = window_start + 900;
    
    let rounds = vec![
        make_round(1, window_start, 5000000000000, 1),  // 50000.0
        make_round(2, window_end, 5000000000000, 1),    // 50000.0 (SAME = TIE)
    ];
    
    let mut config = make_config_with_rule(SettlementReferenceRule::LastUpdateAtOrBeforeCutoff);
    config.tie_rule = TieRule::NoWins;
    
    let mut coordinator = ChainlinkSettlementCoordinator::with_chainlink_replay(config, rounds);
    
    let market_id = format!("btc-updown-15m-{}", window_start);
    let start_ns = (window_start * NS_PER_SEC as u64) as Nanos;
    coordinator.track_window(&market_id, start_ns).unwrap();
    coordinator.observe_market_price(&market_id, 50000.0, start_ns, start_ns);
    coordinator.advance_time((window_end * NS_PER_SEC as u64) as Nanos);
    
    let decision = (window_end + 5) * NS_PER_SEC as u64;
    let (event, _record) = coordinator.try_settle(&market_id, decision as Nanos).unwrap().unwrap();
    
    assert!(matches!(event.outcome, SettlementOutcome::Resolved { winner: Outcome::No, is_tie: true }));
}

// =============================================================================
// IDEMPOTENCY TESTS
// =============================================================================

/// Test: Settlement is idempotent (multiple calls after settlement are no-ops)
#[test]
fn test_settlement_idempotency() {
    let window_start = 1000u64;
    let window_end = window_start + 900;
    
    let rounds = vec![
        make_round(1, window_start, 5000000000000, 1),
        make_round(2, window_end, 5010000000000, 1),
    ];
    
    let config = make_config_with_rule(SettlementReferenceRule::LastUpdateAtOrBeforeCutoff);
    let mut coordinator = ChainlinkSettlementCoordinator::with_chainlink_replay(config, rounds);
    
    let market_id = format!("btc-updown-15m-{}", window_start);
    let start_ns = (window_start * NS_PER_SEC as u64) as Nanos;
    coordinator.track_window(&market_id, start_ns).unwrap();
    coordinator.observe_market_price(&market_id, 50000.0, start_ns, start_ns);
    coordinator.advance_time((window_end * NS_PER_SEC as u64) as Nanos);
    
    let decision = (window_end + 5) * NS_PER_SEC as u64;
    
    // First settle - should succeed
    let result1 = coordinator.try_settle(&market_id, decision as Nanos).unwrap();
    assert!(result1.is_some(), "First settlement should succeed");
    
    // Second settle - should be no-op
    let result2 = coordinator.try_settle(&market_id, (decision + 1000) as Nanos).unwrap();
    assert!(result2.is_none(), "Second settlement should be no-op");
    
    // Metadata should only show one settlement
    let metadata = coordinator.metadata();
    assert_eq!(metadata.windows_settled, 1);
}

// =============================================================================
// DELAY STATISTICS TESTS
// =============================================================================

/// Test: Settlement delay statistics are computed correctly
#[test]
fn test_settlement_delay_statistics() {
    use crate::backtest_v2::settlement_integration::SettlementDelayStats;
    
    let delays = vec![100, 200, 300, 400, 500, 600, 700, 800, 900, 1000];
    let stats = SettlementDelayStats::from_delays(&delays);
    
    assert_eq!(stats.min_ns, Some(100));
    assert_eq!(stats.max_ns, Some(1000));
    assert!((stats.mean_ns.unwrap() - 550.0).abs() < 0.01);
    assert_eq!(stats.median_ns, Some(600)); // 10 elements, median is [5] = 600
}

/// Test: Empty delays produce default stats
#[test]
fn test_settlement_delay_stats_empty() {
    use crate::backtest_v2::settlement_integration::SettlementDelayStats;
    
    let stats = SettlementDelayStats::from_delays(&[]);
    
    assert!(stats.min_ns.is_none());
    assert!(stats.max_ns.is_none());
    assert!(stats.mean_ns.is_none());
}

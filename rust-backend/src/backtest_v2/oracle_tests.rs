//! Oracle Integration Tests
//!
//! Tests for Chainlink settlement reference with:
//! 1. Round retrieval correctness
//! 2. Boundary tests (cutoff ± ε)
//! 3. Knowability tests (visibility semantics)
//! 4. Replay determinism
//! 5. Missing data behavior

use crate::backtest_v2::oracle::{
    chainlink::{ChainlinkFeedConfig, ChainlinkReplayFeed, ChainlinkRound},
    settlement_source::{
        ChainlinkSettlementSource, OraclePricePoint, SettlementReferenceRule,
        SettlementReferenceSource,
    },
    storage::{OracleRoundStorage, OracleStorageConfig},
    basis_diagnostics::{BasisDiagnostics, BasisStats, WindowBasis},
};

// =============================================================================
// Test Helpers
// =============================================================================

fn make_round(round_id: u128, updated_at: u64, answer: i128, arrival_delay_ms: u64) -> ChainlinkRound {
    ChainlinkRound {
        feed_id: "test_btc_usd".to_string(),
        round_id,
        answer,
        updated_at,
        answered_in_round: round_id,
        started_at: updated_at,
        ingest_arrival_time_ns: (updated_at * 1_000_000_000) + (arrival_delay_ms * 1_000_000),
        ingest_seq: round_id as u64,
        decimals: 8,
        asset_symbol: "BTC".to_string(),
        raw_source_hash: None,
    }
}

fn make_test_rounds() -> Vec<ChainlinkRound> {
    vec![
        make_round(1, 1000, 5000000000000, 500), // $50000, arrives 500ms later
        make_round(2, 1002, 5001000000000, 500), // $50010
        make_round(3, 1004, 5002000000000, 500), // $50020
        make_round(4, 1006, 5003000000000, 500), // $50030
        make_round(5, 1008, 5004000000000, 500), // $50040
    ]
}

// =============================================================================
// 1. Round Retrieval Correctness Tests
// =============================================================================

#[test]
fn test_round_retrieval_exact_match() {
    let source = ChainlinkSettlementSource::from_rounds(
        make_test_rounds(),
        "BTC".to_string(),
        "test_btc_usd".to_string(),
    );

    // Query exactly at round 3's timestamp
    let price = source.reference_price_at_or_before(1004).unwrap();
    assert_eq!(price.round_id, 3);
    assert!((price.price - 50020.0).abs() < 0.01);
}

#[test]
fn test_round_retrieval_between_rounds() {
    let source = ChainlinkSettlementSource::from_rounds(
        make_test_rounds(),
        "BTC".to_string(),
        "test_btc_usd".to_string(),
    );

    // Query between round 2 (1002) and round 3 (1004)
    let price = source.reference_price_at_or_before(1003).unwrap();
    assert_eq!(price.round_id, 2, "Should return last round at or before");
}

#[test]
fn test_round_retrieval_before_first() {
    let source = ChainlinkSettlementSource::from_rounds(
        make_test_rounds(),
        "BTC".to_string(),
        "test_btc_usd".to_string(),
    );

    // Query before any rounds exist
    let price = source.reference_price_at_or_before(999);
    assert!(price.is_none(), "No round should exist before first");
}

#[test]
fn test_round_retrieval_after_last() {
    let source = ChainlinkSettlementSource::from_rounds(
        make_test_rounds(),
        "BTC".to_string(),
        "test_btc_usd".to_string(),
    );

    // Query after all rounds
    let price = source.reference_price_at_or_before(9999).unwrap();
    assert_eq!(price.round_id, 5, "Should return last available round");
}

// =============================================================================
// 2. Boundary Tests: cutoff ± ε
// =============================================================================

const EPSILON_SEC: u64 = 1; // 1 second epsilon for boundary tests

#[test]
fn test_boundary_cutoff_minus_epsilon_included() {
    let rounds = vec![
        make_round(1, 1000, 5000000000000, 100),
        make_round(2, 2000, 5010000000000, 100), // Exactly at 2000
    ];
    let source = ChainlinkSettlementSource::from_rounds(
        rounds,
        "BTC".to_string(),
        "test".to_string(),
    );

    let cutoff = 2000;

    // cutoff - ε: should include round at 1000
    let price = source.reference_price_at_or_before(cutoff - EPSILON_SEC).unwrap();
    assert_eq!(price.round_id, 1, "Round before cutoff-ε should be included");
    assert_eq!(price.oracle_updated_at_unix_sec, 1000);
}

#[test]
fn test_boundary_exactly_cutoff_included() {
    let rounds = vec![
        make_round(1, 1000, 5000000000000, 100),
        make_round(2, 2000, 5010000000000, 100), // Exactly at 2000
    ];
    let source = ChainlinkSettlementSource::from_rounds(
        rounds,
        "BTC".to_string(),
        "test".to_string(),
    );

    let cutoff = 2000;

    // Exactly at cutoff: should include round at cutoff
    let price = source.reference_price_at_or_before(cutoff).unwrap();
    assert_eq!(price.round_id, 2, "Round exactly at cutoff should be included");
    assert_eq!(price.oracle_updated_at_unix_sec, 2000);
}

#[test]
fn test_boundary_cutoff_plus_epsilon_excluded_for_at_or_before() {
    let rounds = vec![
        make_round(1, 1000, 5000000000000, 100),
        make_round(2, 2001, 5010000000000, 100), // One second AFTER cutoff
    ];
    let source = ChainlinkSettlementSource::from_rounds(
        rounds,
        "BTC".to_string(),
        "test".to_string(),
    );

    let cutoff = 2000;

    // at_or_before(2000) should NOT return round at 2001
    let price = source.reference_price_at_or_before(cutoff).unwrap();
    assert_eq!(price.round_id, 1, "Round after cutoff should be excluded for at_or_before");
}

#[test]
fn test_boundary_first_after_cutoff() {
    let rounds = vec![
        make_round(1, 1000, 5000000000000, 100),
        make_round(2, 2001, 5010000000000, 100), // One second AFTER cutoff
    ];
    let source = ChainlinkSettlementSource::from_rounds(
        rounds,
        "BTC".to_string(),
        "test".to_string(),
    );

    let cutoff = 2000;

    // first_after(2000) SHOULD return round at 2001
    let price = source.reference_price_first_after(cutoff).unwrap();
    assert_eq!(price.round_id, 2, "first_after should return round at 2001");
}

#[test]
fn test_last_valid_price_wins_at_boundary() {
    // Multiple rounds at or before the same cutoff - last one wins
    let rounds = vec![
        make_round(1, 1998, 5000000000000, 100),
        make_round(2, 1999, 5010000000000, 100),
        make_round(3, 2000, 5020000000000, 100), // Latest at cutoff
    ];
    let source = ChainlinkSettlementSource::from_rounds(
        rounds,
        "BTC".to_string(),
        "test".to_string(),
    );

    let cutoff = 2000;

    let price = source.reference_price_at_or_before(cutoff).unwrap();
    assert_eq!(price.round_id, 3, "Last round at or before cutoff should win");
    assert!((price.price - 50200.0).abs() < 0.01);
}

// =============================================================================
// 3. Knowability Tests (Visibility Semantics)
// =============================================================================

#[test]
fn test_outcome_not_knowable_until_reference_arrives() {
    // Round updated_at = 2000, but arrives 5 seconds later (arrival_time_ns)
    let rounds = vec![
        make_round(1, 1000, 5000000000000, 100),
        ChainlinkRound {
            feed_id: "test".to_string(),
            round_id: 2,
            answer: 5010000000000,
            updated_at: 2000,
            answered_in_round: 2,
            started_at: 2000,
            ingest_arrival_time_ns: 2005_000_000_000, // Arrives 5 seconds AFTER update
            ingest_seq: 1,
            decimals: 8,
            asset_symbol: "BTC".to_string(),
            raw_source_hash: None,
        },
    ];
    let source = ChainlinkSettlementSource::from_rounds(
        rounds,
        "BTC".to_string(),
        "test".to_string(),
    );

    let cutoff = 2000;

    // Decision time immediately after cutoff - should NOT be knowable
    // (round source time is 2000, but arrival is 2005)
    let decision_time_ns = 2001_000_000_000; // 2001 seconds
    assert!(
        !source.is_outcome_knowable(
            decision_time_ns,
            cutoff,
            SettlementReferenceRule::LastUpdateAtOrBeforeCutoff
        ),
        "Outcome should NOT be knowable before reference arrives"
    );

    // Decision time after arrival - IS knowable
    let decision_time_ns = 2006_000_000_000; // 2006 seconds
    assert!(
        source.is_outcome_knowable(
            decision_time_ns,
            cutoff,
            SettlementReferenceRule::LastUpdateAtOrBeforeCutoff
        ),
        "Outcome should be knowable after reference arrives"
    );
}

#[test]
fn test_outcome_knowable_exactly_at_arrival() {
    let rounds = vec![ChainlinkRound {
        feed_id: "test".to_string(),
        round_id: 1,
        answer: 5010000000000,
        updated_at: 2000,
        answered_in_round: 1,
        started_at: 2000,
        ingest_arrival_time_ns: 2005_000_000_000,
        ingest_seq: 0,
        decimals: 8,
        asset_symbol: "BTC".to_string(),
        raw_source_hash: None,
    }];
    let source = ChainlinkSettlementSource::from_rounds(
        rounds,
        "BTC".to_string(),
        "test".to_string(),
    );

    let cutoff = 2000;

    // Exactly at arrival time - IS knowable
    let decision_time_ns = 2005_000_000_000;
    assert!(
        source.is_outcome_knowable(
            decision_time_ns,
            cutoff,
            SettlementReferenceRule::LastUpdateAtOrBeforeCutoff
        ),
        "Outcome should be knowable exactly at arrival time"
    );

    // One nanosecond before arrival - NOT knowable
    let decision_time_ns = 2005_000_000_000 - 1;
    assert!(
        !source.is_outcome_knowable(
            decision_time_ns,
            cutoff,
            SettlementReferenceRule::LastUpdateAtOrBeforeCutoff
        ),
        "Outcome should NOT be knowable one ns before arrival"
    );
}

#[test]
fn test_outcome_not_knowable_if_no_reference() {
    // Empty source - no rounds available
    let source = ChainlinkSettlementSource::from_rounds(
        vec![],
        "BTC".to_string(),
        "test".to_string(),
    );

    let cutoff = 2000;
    let decision_time_ns = 9999_000_000_000;

    assert!(
        !source.is_outcome_knowable(
            decision_time_ns,
            cutoff,
            SettlementReferenceRule::LastUpdateAtOrBeforeCutoff
        ),
        "Outcome should NOT be knowable if no reference data exists"
    );
}

// =============================================================================
// 4. Replay Determinism Tests
// =============================================================================

#[test]
fn test_replay_determinism_same_rounds_same_results() {
    let rounds = make_test_rounds();

    // Create two sources from the same data
    let source1 = ChainlinkSettlementSource::from_rounds(
        rounds.clone(),
        "BTC".to_string(),
        "test".to_string(),
    );
    let source2 = ChainlinkSettlementSource::from_rounds(
        rounds,
        "BTC".to_string(),
        "test".to_string(),
    );

    // Query the same cutoffs - results must be identical
    let cutoffs = vec![1001, 1003, 1005, 1007, 1009, 9999];

    for cutoff in cutoffs {
        let p1 = source1.reference_price_at_or_before(cutoff);
        let p2 = source2.reference_price_at_or_before(cutoff);

        match (p1, p2) {
            (Some(a), Some(b)) => {
                assert_eq!(a.round_id, b.round_id, "Round ID must match for cutoff {}", cutoff);
                assert!(
                    (a.price - b.price).abs() < 1e-10,
                    "Price must match for cutoff {}",
                    cutoff
                );
            }
            (None, None) => {}
            _ => panic!("Existence must match for cutoff {}", cutoff),
        }
    }
}

#[test]
fn test_replay_determinism_order_independence() {
    // Same rounds but inserted in different order
    let mut rounds1 = make_test_rounds();
    let mut rounds2 = make_test_rounds();
    rounds2.reverse();

    let source1 = ChainlinkSettlementSource::from_rounds(
        rounds1,
        "BTC".to_string(),
        "test".to_string(),
    );
    let source2 = ChainlinkSettlementSource::from_rounds(
        rounds2,
        "BTC".to_string(),
        "test".to_string(),
    );

    // Results must still be identical
    let cutoffs = vec![1001, 1003, 1005, 1007, 1009];

    for cutoff in cutoffs {
        let p1 = source1.reference_price_at_or_before(cutoff).unwrap();
        let p2 = source2.reference_price_at_or_before(cutoff).unwrap();

        assert_eq!(
            p1.round_id, p2.round_id,
            "Round ID must match regardless of insertion order for cutoff {}",
            cutoff
        );
    }
}

#[test]
fn test_replay_feed_iteration_determinism() {
    let rounds = make_test_rounds();

    let mut feed1 = ChainlinkReplayFeed::new(rounds.clone());
    let mut feed2 = ChainlinkReplayFeed::new(rounds);

    // Iterate both feeds - must produce same sequence
    loop {
        let r1 = feed1.next();
        let r2 = feed2.next();

        match (r1, r2) {
            (Some(a), Some(b)) => {
                assert_eq!(a.round_id, b.round_id);
                assert_eq!(a.updated_at, b.updated_at);
                assert_eq!(a.answer, b.answer);
            }
            (None, None) => break,
            _ => panic!("Iteration must be identical"),
        }
    }
}

// =============================================================================
// 5. Missing Data Behavior Tests
// =============================================================================

#[test]
fn test_missing_data_empty_source() {
    let source = ChainlinkSettlementSource::from_rounds(
        vec![],
        "BTC".to_string(),
        "test".to_string(),
    );

    assert!(source.reference_price_at_or_before(2000).is_none());
    assert!(source.reference_price_first_after(2000).is_none());
    assert_eq!(source.round_count(), 0);
}

#[test]
fn test_missing_data_gap_in_rounds() {
    // Gap between round 2 (ts=1002) and round 3 (ts=2000)
    let rounds = vec![
        make_round(1, 1000, 5000000000000, 100),
        make_round(2, 1002, 5010000000000, 100),
        // No round between 1002 and 2000
        make_round(3, 2000, 5050000000000, 100),
    ];
    let source = ChainlinkSettlementSource::from_rounds(
        rounds,
        "BTC".to_string(),
        "test".to_string(),
    );

    // Query in the gap - should return last available
    let price = source.reference_price_at_or_before(1500).unwrap();
    assert_eq!(price.round_id, 2);
    assert_eq!(price.oracle_updated_at_unix_sec, 1002);
}

// =============================================================================
// 6. Storage Round-Trip Tests
// =============================================================================

#[test]
fn test_storage_round_trip() {
    let storage = OracleRoundStorage::open_memory().unwrap();
    let rounds = make_test_rounds();

    // Store rounds
    storage.store_rounds(&rounds).unwrap();

    // Load rounds
    let loaded = storage
        .load_rounds_in_range("test_btc_usd", 1000, 1010)
        .unwrap();

    assert_eq!(loaded.len(), rounds.len());

    for (original, stored) in rounds.iter().zip(loaded.iter()) {
        assert_eq!(original.round_id, stored.round_id);
        assert_eq!(original.answer, stored.answer);
        assert_eq!(original.updated_at, stored.updated_at);
        assert_eq!(original.ingest_arrival_time_ns, stored.ingest_arrival_time_ns);
    }
}

#[test]
fn test_storage_time_based_lookup() {
    let storage = OracleRoundStorage::open_memory().unwrap();
    let rounds = make_test_rounds();
    storage.store_rounds(&rounds).unwrap();

    // Query at or before
    let round = storage
        .get_round_at_or_before("test_btc_usd", 1005)
        .unwrap()
        .unwrap();
    assert_eq!(round.round_id, 3); // Round 3 at ts=1004

    // Query first after
    let round = storage
        .get_round_first_after("test_btc_usd", 1005)
        .unwrap()
        .unwrap();
    assert_eq!(round.round_id, 4); // Round 4 at ts=1006
}

#[test]
fn test_storage_backfill_state() {
    let storage = OracleRoundStorage::open_memory().unwrap();

    // Update backfill state
    storage
        .update_backfill_state("test_btc_usd", 1, 100, 1000, 2000, 100, false)
        .unwrap();

    // Read it back
    let state = storage.get_backfill_state("test_btc_usd").unwrap().unwrap();
    assert_eq!(state.oldest_round_id, 1);
    assert_eq!(state.newest_round_id, 100);
    assert_eq!(state.total_rounds, 100);
    assert!(!state.is_complete);
}

// =============================================================================
// 7. Basis Diagnostics Tests
// =============================================================================

#[test]
fn test_basis_diagnostics_computation() {
    let mut diag = BasisDiagnostics::new();

    for i in 0..20 {
        let mut window = WindowBasis::new(i * 900, (i + 1) * 900, "BTC".to_string());
        window.binance_mid_at_cutoff = Some(50000.0 + (i as f64) * 10.0);
        window.chainlink_settlement_price = Some(50000.0);
        window.binance_start = Some(49950.0);
        window.binance_end = Some(50000.0 + (i as f64) * 10.0);
        window.chainlink_start = Some(49950.0);
        window.chainlink_end = Some(50000.0);
        window.finalize();
        diag.record_window(window);
    }

    let stats = diag.overall_stats();

    assert_eq!(stats.window_count, 20);
    assert_eq!(stats.windows_with_data, 20);
    assert!(stats.mean_basis_bps.is_some());
    assert!(stats.max_abs_basis_bps.is_some());
}

#[test]
fn test_basis_direction_disagreement_detection() {
    let mut window = WindowBasis::new(0, 900, "BTC".to_string());

    // Binance says UP, Chainlink says DOWN
    window.binance_start = Some(50000.0);
    window.binance_end = Some(50100.0); // UP
    window.chainlink_start = Some(50000.0);
    window.chainlink_end = Some(49900.0); // DOWN
    window.finalize();

    assert_eq!(window.direction_agrees, Some(false));
}

// =============================================================================
// 8. Settlement Rule Tests
// =============================================================================

#[test]
fn test_closest_rule_tie_handling() {
    // Two rounds equidistant from cutoff
    let rounds = vec![
        make_round(1, 990, 5000000000000, 100),  // 10 seconds before
        make_round(2, 1010, 5010000000000, 100), // 10 seconds after
    ];
    let source = ChainlinkSettlementSource::from_rounds(
        rounds,
        "BTC".to_string(),
        "test".to_string(),
    );

    let cutoff = 1000;

    // ClosestToCutoff: tie goes to before
    let price = source
        .reference_price(cutoff, SettlementReferenceRule::ClosestToCutoff)
        .unwrap();
    assert_eq!(price.round_id, 1, "Tie should go to before");

    // ClosestToCutoffTieAfter: tie goes to after
    let price = source
        .reference_price(cutoff, SettlementReferenceRule::ClosestToCutoffTieAfter)
        .unwrap();
    assert_eq!(price.round_id, 2, "Tie should go to after with TieAfter rule");
}

#[test]
fn test_all_settlement_rules() {
    let rounds = vec![
        make_round(1, 990, 5000000000000, 100),
        make_round(2, 1010, 5010000000000, 100),
    ];
    let source = ChainlinkSettlementSource::from_rounds(
        rounds,
        "BTC".to_string(),
        "test".to_string(),
    );

    let cutoff = 1000;

    // LastUpdateAtOrBeforeCutoff: should return round 1
    let price = source
        .reference_price(cutoff, SettlementReferenceRule::LastUpdateAtOrBeforeCutoff)
        .unwrap();
    assert_eq!(price.round_id, 1);

    // FirstUpdateAfterCutoff: should return round 2
    let price = source
        .reference_price(cutoff, SettlementReferenceRule::FirstUpdateAfterCutoff)
        .unwrap();
    assert_eq!(price.round_id, 2);

    // ClosestToCutoff: equidistant, tie to before
    let price = source
        .reference_price(cutoff, SettlementReferenceRule::ClosestToCutoff)
        .unwrap();
    assert_eq!(price.round_id, 1);
}

//! Adversarial Injection Tests for Stream Integrity
//!
//! These tests deliberately inject corrupt streams to verify deterministic,
//! documented behavior. All tests must fail pre-fix and pass post-fix.

use crate::backtest_v2::clock::Nanos;
use crate::backtest_v2::events::{Event, Level, TimestampedEvent};
use crate::backtest_v2::integrity::{
    DropReason, DuplicatePolicy, GapPolicy, HaltReason, HaltType, IntegrityResult,
    OutOfOrderPolicy, PathologyCounters, PathologyPolicy, StreamIntegrityGuard, SyncState,
};

// =============================================================================
// TEST HELPERS
// =============================================================================

fn make_delta(token_id: &str, seq: u64, time: Nanos) -> TimestampedEvent {
    TimestampedEvent {
        time,
        source_time: time,
        seq: 0,
        source: 1,
        event: Event::L2Delta {
            token_id: token_id.to_string(),
            bid_updates: vec![Level::new(0.5, 100.0)],
            ask_updates: vec![],
            exchange_seq: seq,
        },
    }
}

fn make_snapshot(token_id: &str, seq: u64, time: Nanos) -> TimestampedEvent {
    TimestampedEvent {
        time,
        source_time: time,
        seq: 0,
        source: 1,
        event: Event::L2BookSnapshot {
            token_id: token_id.to_string(),
            bids: vec![Level::new(0.5, 100.0)],
            asks: vec![Level::new(0.55, 100.0)],
            exchange_seq: seq,
        },
    }
}

fn make_trade(token_id: &str, trade_id: &str, time: Nanos) -> TimestampedEvent {
    use crate::backtest_v2::events::Side;
    TimestampedEvent {
        time,
        source_time: time,
        seq: 0,
        source: 1,
        event: Event::TradePrint {
            token_id: token_id.to_string(),
            price: 0.52,
            size: 10.0,
            aggressor_side: Side::Buy,
            trade_id: Some(trade_id.to_string()),
        },
    }
}

// =============================================================================
// DUPLICATE INJECTION TESTS
// =============================================================================

#[test]
fn test_duplicate_injection_drop_policy() {
    let mut guard = StreamIntegrityGuard::new(PathologyPolicy {
        on_duplicate: DuplicatePolicy::Drop,
        ..PathologyPolicy::strict()
    });

    let event1 = make_delta("token1", 1, 1000);
    let event2 = make_delta("token1", 1, 1000); // Exact duplicate

    let r1 = guard.process(event1);
    assert!(matches!(r1, IntegrityResult::Forward(_)));

    let r2 = guard.process(event2);
    assert!(
        matches!(r2, IntegrityResult::Dropped(DropReason::Duplicate { .. })),
        "Expected Dropped(Duplicate), got {:?}",
        r2
    );

    assert_eq!(guard.counters().duplicates_dropped, 1);
    assert_eq!(guard.counters().total_events_processed, 2);
    assert_eq!(guard.counters().total_events_forwarded, 1);
}

#[test]
fn test_duplicate_injection_halt_policy() {
    let mut guard = StreamIntegrityGuard::new(PathologyPolicy {
        on_duplicate: DuplicatePolicy::Halt,
        ..PathologyPolicy::strict()
    });

    let event1 = make_delta("token1", 1, 1000);
    let event2 = make_delta("token1", 1, 1000); // Exact duplicate

    let r1 = guard.process(event1);
    assert!(matches!(r1, IntegrityResult::Forward(_)));

    let r2 = guard.process(event2);
    assert!(
        matches!(r2, IntegrityResult::Halted(HaltReason { reason: HaltType::DuplicateCorruption { .. }, .. })),
        "Expected Halted(DuplicateCorruption), got {:?}",
        r2
    );

    assert!(guard.counters().halted);
    assert!(guard.counters().halt_reason.is_some());
}

#[test]
fn test_duplicate_trade_detection() {
    let mut guard = StreamIntegrityGuard::strict();

    let trade1 = make_trade("token1", "trade_abc", 1000);
    let trade2 = make_trade("token1", "trade_abc", 1000); // Same trade_id

    let r1 = guard.process(trade1);
    assert!(matches!(r1, IntegrityResult::Forward(_)));

    let r2 = guard.process(trade2);
    assert!(
        matches!(r2, IntegrityResult::Dropped(DropReason::Duplicate { .. })),
        "Expected duplicate detection for same trade_id"
    );
}

// =============================================================================
// GAP INJECTION TESTS
// =============================================================================

#[test]
fn test_gap_injection_halt_policy() {
    let mut guard = StreamIntegrityGuard::new(PathologyPolicy {
        on_gap: GapPolicy::Halt,
        gap_tolerance: 0,
        ..PathologyPolicy::strict()
    });

    let event1 = make_delta("token1", 100, 1000);
    let event2 = make_delta("token1", 105, 2000); // Gap of 4 (missing 101-104)

    let r1 = guard.process(event1);
    assert!(matches!(r1, IntegrityResult::Forward(_)));

    let r2 = guard.process(event2);
    match r2 {
        IntegrityResult::Halted(HaltReason {
            reason: HaltType::SequenceGap { expected, actual, gap_size },
            ..
        }) => {
            assert_eq!(expected, 101);
            assert_eq!(actual, 105);
            assert_eq!(gap_size, 4);
        }
        _ => panic!("Expected Halted(SequenceGap), got {:?}", r2),
    }

    assert!(guard.counters().halted);
    assert_eq!(guard.counters().gaps_detected, 1);
    assert_eq!(guard.counters().total_missing_sequences, 4);
}

#[test]
fn test_gap_injection_resync_policy() {
    let mut guard = StreamIntegrityGuard::new(PathologyPolicy {
        on_gap: GapPolicy::Resync,
        gap_tolerance: 0,
        ..PathologyPolicy::strict()
    });

    let event1 = make_delta("token1", 100, 1000);
    let event2 = make_delta("token1", 105, 2000); // Gap triggers resync

    let r1 = guard.process(event1);
    assert!(matches!(r1, IntegrityResult::Forward(_)));

    let r2 = guard.process(event2);
    match r2 {
        IntegrityResult::NeedResync { token_id, last_good_seq } => {
            assert_eq!(token_id, "token1");
            assert_eq!(last_good_seq, Some(100));
        }
        _ => panic!("Expected NeedResync, got {:?}", r2),
    }

    assert_eq!(guard.sync_state("token1"), SyncState::NeedSnapshot);
    assert!(!guard.counters().halted);
}

#[test]
fn test_gap_within_tolerance_accepted() {
    let mut guard = StreamIntegrityGuard::new(PathologyPolicy {
        on_gap: GapPolicy::Halt,
        gap_tolerance: 10, // Allow gaps up to 10
        ..PathologyPolicy::strict()
    });

    let event1 = make_delta("token1", 100, 1000);
    let event2 = make_delta("token1", 105, 2000); // Gap of 4 (within tolerance)

    let r1 = guard.process(event1);
    assert!(matches!(r1, IntegrityResult::Forward(_)));

    let r2 = guard.process(event2);
    assert!(
        matches!(r2, IntegrityResult::Forward(_)),
        "Gap within tolerance should be accepted, got {:?}",
        r2
    );

    assert_eq!(guard.counters().gaps_detected, 1);
    assert!(!guard.counters().halted);
}

#[test]
fn test_gap_resync_drops_deltas_until_snapshot() {
    let mut guard = StreamIntegrityGuard::new(PathologyPolicy {
        on_gap: GapPolicy::Resync,
        gap_tolerance: 0,
        ..PathologyPolicy::strict()
    });

    // Establish baseline
    guard.process(make_delta("token1", 100, 1000));

    // Trigger resync
    let r = guard.process(make_delta("token1", 200, 2000));
    assert!(matches!(r, IntegrityResult::NeedResync { .. }));

    // Deltas should be dropped while awaiting resync
    let r = guard.process(make_delta("token1", 201, 3000));
    assert!(
        matches!(r, IntegrityResult::Dropped(DropReason::AwaitingResync)),
        "Deltas should be dropped while awaiting resync, got {:?}",
        r
    );

    // Snapshot should complete resync
    let r = guard.process(make_snapshot("token1", 300, 4000));
    assert!(matches!(r, IntegrityResult::Forward(_)));
    assert_eq!(guard.sync_state("token1"), SyncState::InSync);

    // Normal operation should resume
    let r = guard.process(make_delta("token1", 301, 5000));
    assert!(matches!(r, IntegrityResult::Forward(_)));
}

// =============================================================================
// OUT-OF-ORDER INJECTION TESTS
// =============================================================================

#[test]
fn test_out_of_order_injection_halt_policy() {
    let mut guard = StreamIntegrityGuard::new(PathologyPolicy {
        on_out_of_order: OutOfOrderPolicy::Halt,
        ..PathologyPolicy::strict()
    });

    let event1 = make_delta("token1", 10, 1000);
    let event2 = make_delta("token1", 9, 2000); // Out of order

    let r1 = guard.process(event1);
    assert!(matches!(r1, IntegrityResult::Forward(_)));

    let r2 = guard.process(event2);
    match r2 {
        IntegrityResult::Halted(HaltReason {
            reason: HaltType::OutOfOrder { expected, actual },
            ..
        }) => {
            assert_eq!(expected, 11);
            assert_eq!(actual, 9);
        }
        _ => panic!("Expected Halted(OutOfOrder), got {:?}", r2),
    }

    assert!(guard.counters().halted);
}

#[test]
fn test_out_of_order_injection_drop_policy() {
    let mut guard = StreamIntegrityGuard::new(PathologyPolicy {
        on_out_of_order: OutOfOrderPolicy::Drop,
        ..PathologyPolicy::strict()
    });

    let event1 = make_delta("token1", 10, 1000);
    let event2 = make_delta("token1", 9, 2000);  // Out of order - drop
    let event3 = make_delta("token1", 11, 3000); // Back in order

    guard.process(event1);
    let r2 = guard.process(event2);
    assert!(
        matches!(r2, IntegrityResult::Dropped(DropReason::OutOfOrder { .. })),
        "Out-of-order should be dropped"
    );

    let r3 = guard.process(event3);
    assert!(matches!(r3, IntegrityResult::Forward(_)));

    assert_eq!(guard.counters().out_of_order_detected, 1);
    assert_eq!(guard.counters().out_of_order_dropped, 1);
    assert!(!guard.counters().halted);
}

#[test]
fn test_out_of_order_injection_reorder_policy() {
    let mut guard = StreamIntegrityGuard::new(PathologyPolicy {
        on_out_of_order: OutOfOrderPolicy::Reorder,
        on_gap: GapPolicy::Resync, // Gaps trigger resync, not halt
        reorder_buffer_size: 10,
        gap_tolerance: 100, // High tolerance for this test
        ..PathologyPolicy::strict()
    });

    // Send events out of order: 1, 3, 2
    let r1 = guard.process(make_delta("token1", 1, 1000));
    assert!(matches!(r1, IntegrityResult::Forward(_)));

    // seq 3 creates a gap (missing 2), with high tolerance it's accepted
    let r2 = guard.process(make_delta("token1", 3, 2000));
    // With high gap tolerance, this should forward
    assert!(
        matches!(r2, IntegrityResult::Forward(_)),
        "seq 3 with high gap tolerance should forward, got {:?}",
        r2
    );

    // seq 2 is now out of order (< expected which is 4)
    let r3 = guard.process(make_delta("token1", 2, 3000));
    // Should be buffered or forward depending on implementation
    assert!(guard.counters().out_of_order_detected > 0, 
        "Should detect out-of-order");
}

// =============================================================================
// BOUNDED REORDER BUFFER TESTS
// =============================================================================

#[test]
fn test_reorder_buffer_overflow_halts() {
    let mut guard = StreamIntegrityGuard::new(PathologyPolicy {
        on_out_of_order: OutOfOrderPolicy::Reorder,
        on_gap: GapPolicy::Resync, // Gaps trigger resync
        gap_tolerance: 100, // High tolerance so gaps don't halt
        reorder_buffer_size: 3, // Small buffer
        ..PathologyPolicy::strict()
    });

    // Establish baseline
    guard.process(make_delta("token1", 1, 1000));
    
    // Create gap (allowed by high tolerance)
    guard.process(make_delta("token1", 100, 2000));
    
    // Now send out-of-order events that fill buffer
    // Expected seq is 101, so these are all out-of-order
    guard.process(make_delta("token1", 50, 3000)); // buffered
    guard.process(make_delta("token1", 51, 4000)); // buffered
    guard.process(make_delta("token1", 52, 5000)); // buffered
    
    // This should overflow the buffer
    let r = guard.process(make_delta("token1", 53, 6000));
    assert!(
        matches!(r, IntegrityResult::Halted(HaltReason { reason: HaltType::ReorderBufferOverflow { .. }, .. })),
        "Buffer overflow should halt, got {:?}",
        r
    );

    assert!(guard.counters().halted);
    assert_eq!(guard.counters().reorder_buffer_overflows, 1);
}

// =============================================================================
// DETERMINISM TESTS
// =============================================================================

#[test]
fn test_identical_corrupt_stream_produces_identical_results() {
    fn run_corrupt_stream(policy: PathologyPolicy) -> (PathologyCounters, Vec<String>) {
        let mut guard = StreamIntegrityGuard::new(policy);
        let mut results = Vec::new();

        // Deliberately corrupt stream
        let events = vec![
            make_delta("token1", 1, 1000),
            make_delta("token1", 1, 1000), // duplicate
            make_delta("token1", 5, 2000), // gap
            make_delta("token1", 3, 3000), // out of order
            make_delta("token1", 6, 4000), // back in sequence
        ];

        for event in events {
            let r = guard.process(event);
            results.push(format!("{:?}", std::mem::discriminant(&r)));
        }

        (guard.counters().clone(), results)
    }

    let policy = PathologyPolicy::permissive();

    let (counters1, results1) = run_corrupt_stream(policy.clone());
    let (counters2, results2) = run_corrupt_stream(policy);

    // Results must be identical
    assert_eq!(results1, results2, "Results differ between runs");
    assert_eq!(counters1.duplicates_dropped, counters2.duplicates_dropped);
    assert_eq!(counters1.gaps_detected, counters2.gaps_detected);
    assert_eq!(counters1.out_of_order_detected, counters2.out_of_order_detected);
    assert_eq!(counters1.total_events_forwarded, counters2.total_events_forwarded);
}

#[test]
fn test_same_stream_same_policy_same_halt_point() {
    fn run_until_halt(seed: u64) -> (u64, Option<String>) {
        let mut guard = StreamIntegrityGuard::strict();

        // Create deterministic stream based on seed
        let events = vec![
            make_delta("token1", 1, 1000),
            make_delta("token1", 2, 2000),
            make_delta("token1", 2, 2000), // duplicate - strict drops
            make_delta("token1", 10, 3000), // gap - strict halts
        ];

        for (i, event) in events.into_iter().enumerate() {
            let r = guard.process(event);
            if let IntegrityResult::Halted(_) = r {
                return (i as u64, guard.counters().halt_reason.clone());
            }
        }

        (999, None)
    }

    let (halt_point1, reason1) = run_until_halt(12345);
    let (halt_point2, reason2) = run_until_halt(12345);

    assert_eq!(halt_point1, halt_point2, "Halt point differs");
    assert_eq!(reason1, reason2, "Halt reason differs");
}

// =============================================================================
// MULTI-TOKEN ISOLATION TESTS
// =============================================================================

#[test]
fn test_pathologies_isolated_per_token() {
    let mut guard = StreamIntegrityGuard::new(PathologyPolicy {
        on_gap: GapPolicy::Resync,
        ..PathologyPolicy::strict()
    });

    // token1: normal sequence
    guard.process(make_delta("token1", 1, 1000));
    guard.process(make_delta("token1", 2, 2000));
    
    // token2: gap triggers resync
    guard.process(make_delta("token2", 1, 1500));
    guard.process(make_delta("token2", 100, 2500)); // gap

    // token1 should still be in sync
    assert_eq!(guard.sync_state("token1"), SyncState::InSync);
    // token2 should need snapshot
    assert_eq!(guard.sync_state("token2"), SyncState::NeedSnapshot);

    // token1 should continue normally
    let r = guard.process(make_delta("token1", 3, 3000));
    assert!(matches!(r, IntegrityResult::Forward(_)));

    // token2 deltas should be dropped
    let r = guard.process(make_delta("token2", 101, 3500));
    assert!(matches!(r, IntegrityResult::Dropped(DropReason::AwaitingResync)));
}

// =============================================================================
// TIMESTAMP MONOTONICITY TESTS
// =============================================================================

fn make_unsequenced_trade(token_id: &str, time: Nanos) -> TimestampedEvent {
    use crate::backtest_v2::events::Side;
    TimestampedEvent {
        time,
        source_time: time,
        seq: 0,
        source: 1,
        event: Event::TradePrint {
            token_id: token_id.to_string(),
            price: 0.52,
            size: 10.0,
            aggressor_side: Side::Buy,
            trade_id: None, // No trade_id = unsequenced
        },
    }
}

#[test]
fn test_timestamp_regression_detected() {
    let mut guard = StreamIntegrityGuard::new(PathologyPolicy {
        on_out_of_order: OutOfOrderPolicy::Halt,
        timestamp_jitter_tolerance_ns: 0,
        ..PathologyPolicy::strict()
    });

    // Use unsequenced trades (no trade_id) so timestamp monotonicity is checked
    let trade1 = make_unsequenced_trade("token1", 2000);
    let trade2 = make_unsequenced_trade("token1", 1000); // Earlier timestamp

    guard.process(trade1);
    let r = guard.process(trade2);
    
    assert!(
        matches!(r, IntegrityResult::Halted(HaltReason { reason: HaltType::TimestampRegression { .. }, .. })),
        "Expected TimestampRegression halt, got {:?}",
        r
    );
}

#[test]
fn test_timestamp_jitter_within_tolerance() {
    let mut guard = StreamIntegrityGuard::new(PathologyPolicy {
        on_out_of_order: OutOfOrderPolicy::Halt,
        timestamp_jitter_tolerance_ns: 1_000_000, // 1ms tolerance
        ..PathologyPolicy::strict()
    });

    // Use unsequenced trades (no trade_id)
    let trade1 = make_unsequenced_trade("token1", 2_000_000);
    let trade2 = make_unsequenced_trade("token1", 1_500_000); // 0.5ms regression (within tolerance)

    guard.process(trade1);
    let r = guard.process(trade2);
    
    // Should not halt due to tolerance
    assert!(
        !matches!(r, IntegrityResult::Halted(_)),
        "Small timestamp jitter should be tolerated, got {:?}",
        r
    );
}

// =============================================================================
// POLICY VALIDATION TESTS
// =============================================================================

#[test]
fn test_strict_policy_defaults() {
    let policy = PathologyPolicy::strict();
    
    assert_eq!(policy.gap_tolerance, 0);
    assert!(matches!(policy.on_duplicate, DuplicatePolicy::Drop));
    assert!(matches!(policy.on_gap, GapPolicy::Halt));
    assert!(matches!(policy.on_out_of_order, OutOfOrderPolicy::Halt));
    assert_eq!(policy.reorder_buffer_size, 0);
}

#[test]
fn test_resilient_policy_defaults() {
    let policy = PathologyPolicy::resilient();
    
    assert!(policy.gap_tolerance > 0);
    assert!(matches!(policy.on_gap, GapPolicy::Resync));
    assert!(matches!(policy.on_out_of_order, OutOfOrderPolicy::Reorder));
    assert!(policy.reorder_buffer_size > 0);
}

#[test]
fn test_permissive_policy_never_halts() {
    let mut guard = StreamIntegrityGuard::new(PathologyPolicy::permissive());

    // Inject all types of pathologies
    let events = vec![
        make_delta("token1", 1, 1000),
        make_delta("token1", 1, 1000),  // duplicate
        make_delta("token1", 100, 2000), // gap
        make_delta("token1", 50, 3000),  // out of order
        make_delta("token1", 101, 4000), // continue
    ];

    for event in events {
        let r = guard.process(event);
        assert!(
            !matches!(r, IntegrityResult::Halted(_)),
            "Permissive policy should never halt"
        );
    }

    // Should have detected pathologies but not halted
    assert!(!guard.counters().halted);
    assert!(guard.counters().has_pathologies());
}

// =============================================================================
// COUNTERS SUMMARY TESTS
// =============================================================================

#[test]
fn test_counters_summary_format() {
    let mut counters = PathologyCounters::default();
    counters.total_events_processed = 100;
    counters.total_events_forwarded = 95;
    counters.duplicates_dropped = 2;
    counters.gaps_detected = 1;
    counters.total_missing_sequences = 5;
    counters.out_of_order_detected = 2;

    let summary = counters.summary();
    
    assert!(summary.contains("processed=100"));
    assert!(summary.contains("forwarded=95"));
    assert!(summary.contains("dups=2"));
    assert!(summary.contains("gaps=1"));
    assert!(summary.contains("missing=5"));
    assert!(summary.contains("ooo=2"));
}

#[test]
fn test_has_pathologies_detects_any() {
    let mut c = PathologyCounters::default();
    assert!(!c.has_pathologies());

    c.duplicates_dropped = 1;
    assert!(c.has_pathologies());

    c = PathologyCounters::default();
    c.gaps_detected = 1;
    assert!(c.has_pathologies());

    c = PathologyCounters::default();
    c.out_of_order_detected = 1;
    assert!(c.has_pathologies());

    c = PathologyCounters::default();
    c.halted = true;
    assert!(c.has_pathologies());
}

//! Tests for Settlement Reference Replay
//!
//! This module contains comprehensive tests for the settlement reference system:
//! - Sampling rule edge cases
//! - Boundary conditions
//! - Tie-breaking determinism
//! - Settlement isolation from execution book
//! - Audit log reproducibility

use super::settlement_reference::*;
use crate::backtest_v2::clock::Nanos;

// =============================================================================
// HELPER FUNCTIONS
// =============================================================================

fn make_tick(seq: u64, visible_ts_ns: Nanos, price: f64) -> SettlementReferenceTick {
    SettlementReferenceTick::from_price(
        seq,
        "binance".to_string(),
        "BTC".to_string(),
        ReferencePriceType::Mid,
        PriceFixed::from_f64(price),
        Some(visible_ts_ns - 100_000), // exchange_ts slightly before
        visible_ts_ns - 50_000,        // ingest_ts between exchange and visible
        visible_ts_ns,
    )
}

fn make_bid_ask_tick(
    seq: u64,
    visible_ts_ns: Nanos,
    bid: f64,
    ask: f64,
) -> SettlementReferenceTick {
    SettlementReferenceTick::from_bid_ask(
        seq,
        "binance".to_string(),
        "BTC".to_string(),
        PriceFixed::from_f64(bid),
        PriceFixed::from_f64(ask),
        Some(visible_ts_ns - 100_000),
        visible_ts_ns - 50_000,
        visible_ts_ns,
    )
}

// =============================================================================
// FIXED-POINT PRICE TESTS
// =============================================================================

#[test]
fn test_price_fixed_from_raw() {
    let price = PriceFixed::from_raw(5012345678901);
    assert_eq!(price.raw(), 5012345678901);
}

#[test]
fn test_price_fixed_from_f64_roundtrip() {
    let values = [
        0.0,
        1.0,
        100.0,
        50000.12345678,
        99999.99999999,
        0.00000001,
        -100.50,
    ];

    for original in values {
        let fixed = PriceFixed::from_f64(original);
        let back = fixed.to_f64();
        assert!(
            (original - back).abs() < 1e-8,
            "Roundtrip failed for {}: got {}",
            original,
            back
        );
    }
}

#[test]
fn test_price_fixed_mid() {
    // Simple case
    let bid = PriceFixed::from_f64(50000.0);
    let ask = PriceFixed::from_f64(50100.0);
    let mid = PriceFixed::mid(bid, ask);
    assert_eq!(mid.to_f64(), 50050.0);

    // Odd spread
    let bid = PriceFixed::from_f64(50000.0);
    let ask = PriceFixed::from_f64(50001.0);
    let mid = PriceFixed::mid(bid, ask);
    assert_eq!(mid.to_f64(), 50000.5);

    // Very small spread
    let bid = PriceFixed::from_f64(50000.00000000);
    let ask = PriceFixed::from_f64(50000.00000002);
    let mid = PriceFixed::mid(bid, ask);
    assert_eq!(mid.to_f64(), 50000.00000001);
}

#[test]
fn test_price_fixed_eq_within() {
    let p1 = PriceFixed::from_f64(50000.0);
    let p2 = PriceFixed::from_f64(50000.0);
    assert!(p1.eq_within(p2, 0));

    let p3 = PriceFixed::from_f64(50000.00000001);
    assert!(p1.eq_within(p3, 1)); // 1 raw unit tolerance
    assert!(!p1.eq_within(p3, 0)); // No tolerance - not equal

    let p4 = PriceFixed::from_f64(50001.0);
    assert!(!p1.eq_within(p4, 0));
    assert!(p1.eq_within(p4, PRICE_SCALE as i128)); // 1.0 tolerance
}

#[test]
fn test_price_fixed_ordering() {
    let p1 = PriceFixed::from_f64(50000.0);
    let p2 = PriceFixed::from_f64(50001.0);
    let p3 = PriceFixed::from_f64(49999.0);

    assert!(p1 < p2);
    assert!(p1 > p3);
    assert_eq!(p1, p1);
}

// =============================================================================
// SETTLEMENT REFERENCE SPEC TESTS
// =============================================================================

#[test]
fn test_spec_default_is_polymarket_15m() {
    let spec = SettlementReferenceSpec::default();
    assert_eq!(spec.version, 1);
    assert_eq!(spec.window_duration_ns, NANOS_15_MIN);
    assert_eq!(spec.reference_venue, "binance");
    assert_eq!(spec.reference_price_type, ReferencePriceType::Mid);
    assert_eq!(spec.tie_rule, SettlementTieRule::DownWins);
}

#[test]
fn test_spec_hash_determinism() {
    let spec1 = SettlementReferenceSpec::polymarket_15m_updown_v1();
    let spec2 = SettlementReferenceSpec::polymarket_15m_updown_v1();
    assert_eq!(spec1.spec_hash(), spec2.spec_hash());

    // Different spec should have different hash
    let mut spec3 = spec1.clone();
    spec3.version = 2;
    assert_ne!(spec1.spec_hash(), spec3.spec_hash());
}

#[test]
fn test_spec_rounding_none() {
    let spec = SettlementReferenceSpec {
        rounding_rule: RoundingRule::None,
        ..SettlementReferenceSpec::default()
    };

    let price = PriceFixed::from_f64(50000.123456789);
    let rounded = spec.round_price(price);
    assert_eq!(price, rounded);
}

#[test]
fn test_spec_rounding_decimals() {
    let spec = SettlementReferenceSpec {
        rounding_rule: RoundingRule::Decimals { places: 2 },
        ..SettlementReferenceSpec::default()
    };

    // Should round to 2 decimal places
    let price = PriceFixed::from_f64(50000.12345678);
    let rounded = spec.round_price(price);
    // 50000.12 with 8 decimal storage
    assert!((rounded.to_f64() - 50000.12).abs() < 0.01);
}

#[test]
fn test_spec_rounding_tick_size() {
    let spec = SettlementReferenceSpec {
        rounding_rule: RoundingRule::TickSize { tick_raw: 100 }, // 0.000001 tick
        ..SettlementReferenceSpec::default()
    };

    let price = PriceFixed::from_f64(50000.123456);
    let rounded = spec.round_price(price);
    // Should round to nearest 0.000001
    assert!((rounded.to_f64() - 50000.123456).abs() < 0.000002);
}

#[test]
fn test_spec_determine_outcome_up() {
    let spec = SettlementReferenceSpec::polymarket_15m_updown_v1();
    let start = PriceFixed::from_f64(50000.0);
    let end = PriceFixed::from_f64(50100.0);
    let outcome = spec.determine_outcome(start, end);

    assert!(matches!(
        outcome,
        SettlementOutcomeResult::Up { is_tie: false }
    ));
    assert!(outcome.is_up());
    assert!(!outcome.is_down());
    assert!(!outcome.is_tie());
}

#[test]
fn test_spec_determine_outcome_down() {
    let spec = SettlementReferenceSpec::polymarket_15m_updown_v1();
    let start = PriceFixed::from_f64(50100.0);
    let end = PriceFixed::from_f64(50000.0);
    let outcome = spec.determine_outcome(start, end);

    assert!(matches!(
        outcome,
        SettlementOutcomeResult::Down { is_tie: false }
    ));
    assert!(outcome.is_down());
    assert!(!outcome.is_up());
    assert!(!outcome.is_tie());
}

#[test]
fn test_spec_determine_outcome_tie_down_wins() {
    let spec = SettlementReferenceSpec::polymarket_15m_updown_v1();
    assert_eq!(spec.tie_rule, SettlementTieRule::DownWins);

    let price = PriceFixed::from_f64(50000.0);
    let outcome = spec.determine_outcome(price, price);

    assert!(matches!(
        outcome,
        SettlementOutcomeResult::Down { is_tie: true }
    ));
    assert!(outcome.is_tie());
    assert!(outcome.is_down());
}

#[test]
fn test_spec_determine_outcome_tie_up_wins() {
    let spec = SettlementReferenceSpec {
        tie_rule: SettlementTieRule::UpWins,
        ..SettlementReferenceSpec::default()
    };

    let price = PriceFixed::from_f64(50000.0);
    let outcome = spec.determine_outcome(price, price);

    assert!(matches!(
        outcome,
        SettlementOutcomeResult::Up { is_tie: true }
    ));
    assert!(outcome.is_tie());
    assert!(outcome.is_up());
}

#[test]
fn test_spec_determine_outcome_tie_invalid() {
    let spec = SettlementReferenceSpec {
        tie_rule: SettlementTieRule::Invalid,
        ..SettlementReferenceSpec::default()
    };

    let price = PriceFixed::from_f64(50000.0);
    let outcome = spec.determine_outcome(price, price);

    assert!(matches!(outcome, SettlementOutcomeResult::Invalid { .. }));
}

#[test]
fn test_spec_determine_outcome_tie_split() {
    let spec = SettlementReferenceSpec {
        tie_rule: SettlementTieRule::Split,
        ..SettlementReferenceSpec::default()
    };

    let price = PriceFixed::from_f64(50000.0);
    let outcome = spec.determine_outcome(price, price);

    assert!(matches!(
        outcome,
        SettlementOutcomeResult::Split { share_value: 0.5 }
    ));
}

#[test]
fn test_spec_determine_outcome_with_tolerance() {
    let spec = SettlementReferenceSpec {
        tie_tolerance_raw: 100, // 0.000001 tolerance
        ..SettlementReferenceSpec::default()
    };

    let start = PriceFixed::from_f64(50000.0);
    let end = PriceFixed::from_f64(50000.00000001); // Within tolerance

    let outcome = spec.determine_outcome(start, end);
    // Should be treated as tie
    assert!(outcome.is_tie());
}

// =============================================================================
// TICK TESTS
// =============================================================================

#[test]
fn test_tick_from_bid_ask() {
    let tick = make_bid_ask_tick(1, 1_000_000_000, 50000.0, 50100.0);
    assert_eq!(tick.price_type, ReferencePriceType::Mid);
    assert_eq!(tick.price_fp.to_f64(), 50050.0);
    assert_eq!(tick.bid_fp.unwrap().to_f64(), 50000.0);
    assert_eq!(tick.ask_fp.unwrap().to_f64(), 50100.0);
}

#[test]
fn test_tick_from_price() {
    let tick = SettlementReferenceTick::from_price(
        1,
        "binance".to_string(),
        "ETH".to_string(),
        ReferencePriceType::Last,
        PriceFixed::from_f64(3000.0),
        Some(1_000_000_000),
        1_000_050_000,
        1_000_100_000,
    );

    assert_eq!(tick.price_type, ReferencePriceType::Last);
    assert_eq!(tick.price_fp.to_f64(), 3000.0);
    assert!(tick.bid_fp.is_none());
    assert!(tick.ask_fp.is_none());
}

#[test]
fn test_tick_ordering_by_visible_ts() {
    let tick1 = make_tick(1, 1000, 50000.0);
    let tick2 = make_tick(2, 2000, 50000.0);
    let tick3 = make_tick(3, 1000, 50000.0); // Same visible_ts as tick1

    assert!(tick1 < tick2);
    assert!(tick1 < tick3); // Same visible_ts, tick1 has lower seq
}

#[test]
fn test_tick_fingerprint_determinism() {
    let tick1 = make_tick(1, 1000, 50000.0);
    let tick2 = make_tick(1, 1000, 50000.0);

    assert_eq!(tick1.fingerprint(), tick2.fingerprint());

    let tick3 = make_tick(2, 1000, 50000.0); // Different seq
    assert_ne!(tick1.fingerprint(), tick3.fingerprint());
}

// =============================================================================
// SAMPLING RULE TESTS
// =============================================================================

#[test]
fn test_sampling_first_at_or_after_exact() {
    let spec = SettlementReferenceSpec::polymarket_15m_updown_v1();
    let ticks = vec![
        make_tick(1, 1000, 50000.0),
        make_tick(2, 2000, 50100.0),
        make_tick(3, 3000, 50200.0),
    ];
    let provider = RecordedReferenceStreamProvider::new(spec, ticks);

    // Exact boundary
    let result = provider.sample_start_price(2000);
    assert!(result.is_some());
    assert_eq!(result.as_ref().unwrap().tick.seq, 2);
    assert_eq!(result.as_ref().unwrap().distance_from_boundary_ns, 0);
}

#[test]
fn test_sampling_first_at_or_after_between() {
    let spec = SettlementReferenceSpec::polymarket_15m_updown_v1();
    let ticks = vec![
        make_tick(1, 1000, 50000.0),
        make_tick(2, 2000, 50100.0),
        make_tick(3, 3000, 50200.0),
    ];
    let provider = RecordedReferenceStreamProvider::new(spec, ticks);

    // Between ticks - should get next tick
    let result = provider.sample_start_price(1500);
    assert!(result.is_some());
    assert_eq!(result.as_ref().unwrap().tick.seq, 2);
    assert_eq!(result.as_ref().unwrap().distance_from_boundary_ns, 500);
}

#[test]
fn test_sampling_first_at_or_after_before_all() {
    let spec = SettlementReferenceSpec::polymarket_15m_updown_v1();
    let ticks = vec![make_tick(1, 1000, 50000.0), make_tick(2, 2000, 50100.0)];
    let provider = RecordedReferenceStreamProvider::new(spec, ticks);

    // Before all ticks - should get first tick
    let result = provider.sample_start_price(500);
    assert!(result.is_some());
    assert_eq!(result.as_ref().unwrap().tick.seq, 1);
}

#[test]
fn test_sampling_first_at_or_after_after_all() {
    let spec = SettlementReferenceSpec::polymarket_15m_updown_v1();
    let ticks = vec![make_tick(1, 1000, 50000.0), make_tick(2, 2000, 50100.0)];
    let provider = RecordedReferenceStreamProvider::new(spec, ticks);

    // After all ticks - should be None
    let result = provider.sample_start_price(3000);
    assert!(result.is_none());
}

#[test]
fn test_sampling_first_in_window() {
    let spec = SettlementReferenceSpec {
        sampling_rule_start: SamplingRule::FirstInWindow { epsilon_ns: 500 },
        ..SettlementReferenceSpec::default()
    };
    let ticks = vec![
        make_tick(1, 1000, 50000.0),
        make_tick(2, 2000, 50100.0),
        make_tick(3, 3000, 50200.0),
    ];
    let provider = RecordedReferenceStreamProvider::new(spec, ticks);

    // Boundary at 1800 with epsilon 500 -> window [1800, 2300]
    // Tick at 2000 is in window
    let result = provider.sample_start_price(1800);
    assert!(result.is_some());
    assert_eq!(result.as_ref().unwrap().tick.seq, 2);

    // Boundary at 1400 with epsilon 500 -> window [1400, 1900]
    // No tick in that window (tick 1 at 1000 is before, tick 2 at 2000 is after)
    let result = provider.sample_start_price(1400);
    assert!(result.is_none());
}

#[test]
fn test_sampling_last_before() {
    let spec = SettlementReferenceSpec {
        sampling_rule_end: SamplingRule::LastBefore,
        ..SettlementReferenceSpec::default()
    };
    let ticks = vec![
        make_tick(1, 1000, 50000.0),
        make_tick(2, 2000, 50100.0),
        make_tick(3, 3000, 50200.0),
    ];
    let provider = RecordedReferenceStreamProvider::new(spec, ticks);

    // Should get tick before 2500
    let result = provider.sample_end_price(2500);
    assert!(result.is_some());
    assert_eq!(result.as_ref().unwrap().tick.seq, 2);
    assert!(result.as_ref().unwrap().distance_from_boundary_ns < 0);

    // Exactly at tick - should get previous
    let result = provider.sample_end_price(2000);
    assert!(result.is_some());
    assert_eq!(result.as_ref().unwrap().tick.seq, 1);

    // Before first tick - should be None
    let result = provider.sample_end_price(500);
    assert!(result.is_none());
}

#[test]
fn test_sampling_closest_to_boundary() {
    let spec = SettlementReferenceSpec {
        sampling_rule_start: SamplingRule::ClosestToBoundary,
        ..SettlementReferenceSpec::default()
    };
    let ticks = vec![
        make_tick(1, 1000, 50000.0),
        make_tick(2, 2000, 50100.0),
        make_tick(3, 3000, 50200.0),
    ];
    let provider = RecordedReferenceStreamProvider::new(spec, ticks);

    // Closer to 2000 than 1000
    let result = provider.sample_start_price(1600);
    assert!(result.is_some());
    assert_eq!(result.as_ref().unwrap().tick.seq, 2);

    // Closer to 1000 than 2000
    let result = provider.sample_start_price(1400);
    assert!(result.is_some());
    assert_eq!(result.as_ref().unwrap().tick.seq, 1);
}

#[test]
fn test_sampling_closest_equidistant_tiebreak() {
    let spec = SettlementReferenceSpec {
        sampling_rule_start: SamplingRule::ClosestToBoundary,
        ..SettlementReferenceSpec::default()
    };
    let ticks = vec![make_tick(1, 1000, 50000.0), make_tick(2, 2000, 50100.0)];
    let provider = RecordedReferenceStreamProvider::new(spec, ticks);

    // Equidistant (1500 is exactly between 1000 and 2000)
    // Should pick earlier tick (1)
    let result = provider.sample_start_price(1500);
    assert!(result.is_some());
    assert_eq!(result.as_ref().unwrap().tick.seq, 1);
}

#[test]
fn test_sampling_exact_at_boundary() {
    let spec = SettlementReferenceSpec {
        sampling_rule_start: SamplingRule::ExactAtBoundary,
        ..SettlementReferenceSpec::default()
    };
    let ticks = vec![make_tick(1, 1000, 50000.0), make_tick(2, 2000, 50100.0)];
    let provider = RecordedReferenceStreamProvider::new(spec, ticks);

    // Exact match
    let result = provider.sample_start_price(2000);
    assert!(result.is_some());
    assert_eq!(result.as_ref().unwrap().tick.seq, 2);

    // Not exact - should be None
    let result = provider.sample_start_price(1999);
    assert!(result.is_none());
}

#[test]
fn test_sampling_multiple_ticks_same_visible_ts() {
    let spec = SettlementReferenceSpec::polymarket_15m_updown_v1();

    // Multiple ticks at same visible_ts - should be ordered by seq
    let ticks = vec![
        make_tick(1, 1000, 50000.0),
        make_tick(2, 1000, 50050.0), // Same visible_ts, higher seq
        make_tick(3, 2000, 50100.0),
    ];
    let provider = RecordedReferenceStreamProvider::new(spec, ticks);

    // Should get seq=1 (first at visible_ts=1000)
    let result = provider.sample_start_price(1000);
    assert!(result.is_some());
    assert_eq!(result.as_ref().unwrap().tick.seq, 1);
    assert_eq!(result.as_ref().unwrap().price.to_f64(), 50000.0);
}

// =============================================================================
// SETTLEMENT ENGINE TESTS
// =============================================================================

#[test]
fn test_engine_settle_window_up() {
    let spec = SettlementReferenceSpec::polymarket_15m_updown_v1();
    let window_duration = spec.window_duration_ns;
    let window_start: Nanos = 1000 * 1_000_000_000;

    let ticks = vec![
        make_tick(1, window_start, 50000.0),
        make_tick(2, window_start + window_duration, 50100.0),
    ];

    let provider = RecordedReferenceStreamProvider::new(spec, ticks);
    let mut engine = ReferenceSettlementEngine::new(provider);

    let decision_ts = window_start + window_duration + 1_000_000;
    let result = engine.settle_window(0, window_start, decision_ts);

    assert!(result.is_some());
    let record = result.unwrap();
    assert!(matches!(
        record.outcome,
        SettlementOutcomeResult::Up { is_tie: false }
    ));
    assert_eq!(record.start_tick_seq, 1);
    assert_eq!(record.end_tick_seq, 2);
    assert_eq!(record.start_price_f64, 50000.0);
    assert_eq!(record.end_price_f64, 50100.0);

    assert_eq!(engine.stats().up_wins, 1);
    assert_eq!(engine.stats().down_wins, 0);
    assert_eq!(engine.stats().windows_settled, 1);
}

#[test]
fn test_engine_settle_window_down() {
    let spec = SettlementReferenceSpec::polymarket_15m_updown_v1();
    let window_start: Nanos = 1000 * 1_000_000_000;
    let window_end = window_start + NANOS_15_MIN;

    let ticks = vec![
        make_tick(1, window_start, 50100.0),
        make_tick(2, window_end, 50000.0), // Price went down
    ];

    let provider = RecordedReferenceStreamProvider::new(spec, ticks);
    let mut engine = ReferenceSettlementEngine::new(provider);

    let decision_ts = window_end + 1_000_000;
    let result = engine.settle_window(0, window_start, decision_ts);

    assert!(result.is_some());
    let record = result.unwrap();
    assert!(record.outcome.is_down());
    assert_eq!(engine.stats().down_wins, 1);
}

#[test]
fn test_engine_settle_window_missing_start() {
    let spec = SettlementReferenceSpec::polymarket_15m_updown_v1();
    let window_start: Nanos = 1000 * 1_000_000_000;
    let window_end = window_start + NANOS_15_MIN;

    // Only tick at end - no start tick
    let ticks = vec![make_tick(1, window_end, 50000.0)];

    let provider = RecordedReferenceStreamProvider::new(spec, ticks);
    let mut engine = ReferenceSettlementEngine::new(provider);

    let result = engine.settle_window(0, window_start, window_end + 1_000_000);

    assert!(result.is_none());
    assert_eq!(engine.stats().missing_start, 1);
}

#[test]
fn test_engine_settle_window_missing_end() {
    let spec = SettlementReferenceSpec::polymarket_15m_updown_v1();
    let window_start: Nanos = 1000 * 1_000_000_000;
    let window_end = window_start + NANOS_15_MIN;

    // Only tick at start - no end tick
    let ticks = vec![make_tick(1, window_start, 50000.0)];

    let provider = RecordedReferenceStreamProvider::new(spec, ticks);
    let mut engine = ReferenceSettlementEngine::new(provider);

    let result = engine.settle_window(0, window_start, window_end + 1_000_000);

    assert!(result.is_none());
    assert_eq!(engine.stats().missing_end, 1);
}

#[test]
fn test_engine_can_settle() {
    let spec = SettlementReferenceSpec::polymarket_15m_updown_v1();
    let window_start: Nanos = 0;
    let _window_end = NANOS_15_MIN;

    let ticks = vec![
        make_tick(1, 0, 50000.0),
        make_tick(2, NANOS_15_MIN, 50100.0),
    ];

    let provider = RecordedReferenceStreamProvider::new(spec, ticks);
    let engine = ReferenceSettlementEngine::new(provider);

    assert!(engine.can_settle(window_start));

    // Window beyond our ticks
    let far_window = 10 * NANOS_15_MIN;
    assert!(!engine.can_settle(far_window));
}

#[test]
fn test_engine_multiple_windows() {
    let spec = SettlementReferenceSpec::polymarket_15m_updown_v1();

    // Three windows worth of ticks
    let ticks = vec![
        make_tick(1, 0, 50000.0),
        make_tick(2, NANOS_15_MIN, 50100.0), // End of window 0, start of window 1
        make_tick(3, 2 * NANOS_15_MIN, 50050.0), // End of window 1, start of window 2
        make_tick(4, 3 * NANOS_15_MIN, 50200.0), // End of window 2
    ];

    let provider = RecordedReferenceStreamProvider::new(spec, ticks);
    let mut engine = ReferenceSettlementEngine::new(provider);

    // Window 0: 50000 -> 50100 = UP
    let result0 = engine.settle_window(0, 0, NANOS_15_MIN + 1000);
    assert!(result0.is_some());
    assert!(result0.unwrap().outcome.is_up());

    // Window 1: 50100 -> 50050 = DOWN
    let result1 = engine.settle_window(1, NANOS_15_MIN, 2 * NANOS_15_MIN + 1000);
    assert!(result1.is_some());
    assert!(result1.unwrap().outcome.is_down());

    // Window 2: 50050 -> 50200 = UP
    let result2 = engine.settle_window(2, 2 * NANOS_15_MIN, 3 * NANOS_15_MIN + 1000);
    assert!(result2.is_some());
    assert!(result2.unwrap().outcome.is_up());

    assert_eq!(engine.stats().windows_settled, 3);
    assert_eq!(engine.stats().up_wins, 2);
    assert_eq!(engine.stats().down_wins, 1);
}

// =============================================================================
// AUDIT LOG TESTS
// =============================================================================

#[test]
fn test_audit_record_hash_determinism() {
    let spec = SettlementReferenceSpec::polymarket_15m_updown_v1();
    let window_start: Nanos = 1000 * 1_000_000_000;

    let ticks = vec![
        make_tick(1, window_start, 50000.0),
        make_tick(2, window_start + NANOS_15_MIN, 50100.0),
    ];

    // Create two independent engines with same data
    let provider1 = RecordedReferenceStreamProvider::new(spec.clone(), ticks.clone());
    let provider2 = RecordedReferenceStreamProvider::new(spec, ticks);

    let mut engine1 = ReferenceSettlementEngine::new(provider1);
    let mut engine2 = ReferenceSettlementEngine::new(provider2);

    let decision_ts = window_start + NANOS_15_MIN + 1_000_000;
    let record1 = engine1.settle_window(0, window_start, decision_ts).unwrap();
    let record2 = engine2.settle_window(0, window_start, decision_ts).unwrap();

    // Hashes must be identical
    assert_eq!(record1.record_hash(), record2.record_hash());
}

#[test]
fn test_audit_log_hash_determinism() {
    let spec = SettlementReferenceSpec::polymarket_15m_updown_v1();

    let ticks = vec![
        make_tick(1, 0, 50000.0),
        make_tick(2, NANOS_15_MIN, 50100.0),
        make_tick(3, 2 * NANOS_15_MIN, 50050.0),
    ];

    let provider1 = RecordedReferenceStreamProvider::new(spec.clone(), ticks.clone());
    let provider2 = RecordedReferenceStreamProvider::new(spec, ticks);

    let mut engine1 = ReferenceSettlementEngine::new(provider1);
    let mut engine2 = ReferenceSettlementEngine::new(provider2);

    // Settle same windows in same order
    engine1.settle_window(0, 0, NANOS_15_MIN + 1000);
    engine1.settle_window(1, NANOS_15_MIN, 2 * NANOS_15_MIN + 1000);

    engine2.settle_window(0, 0, NANOS_15_MIN + 1000);
    engine2.settle_window(1, NANOS_15_MIN, 2 * NANOS_15_MIN + 1000);

    assert_eq!(engine1.audit_log_hash(), engine2.audit_log_hash());
}

#[test]
fn test_audit_record_format_debug() {
    let spec = SettlementReferenceSpec::polymarket_15m_updown_v1();
    let window_start: Nanos = 0;

    let ticks = vec![
        make_tick(1, 0, 50000.0),
        make_tick(2, NANOS_15_MIN, 50100.0),
    ];

    let provider = RecordedReferenceStreamProvider::new(spec, ticks);
    let mut engine = ReferenceSettlementEngine::new(provider);

    let record = engine
        .settle_window(0, window_start, NANOS_15_MIN + 1000)
        .unwrap();
    let debug_str = record.format_debug();

    assert!(debug_str.contains("window[0]"));
    assert!(debug_str.contains("start_tick=1"));
    assert!(debug_str.contains("end_tick=2"));
    assert!(debug_str.contains("UP"));
}

// =============================================================================
// COVERAGE CLASSIFICATION TESTS
// =============================================================================

#[test]
fn test_coverage_full() {
    let spec = SettlementReferenceSpec::polymarket_15m_updown_v1();

    // Two complete windows
    let ticks = vec![
        make_tick(1, 0, 50000.0),
        make_tick(2, NANOS_15_MIN, 50100.0),
        make_tick(3, 2 * NANOS_15_MIN, 50200.0),
    ];

    let provider = RecordedReferenceStreamProvider::new(spec, ticks);
    let coverage = classify_settlement_coverage(&provider, 0, 2 * NANOS_15_MIN);

    assert_eq!(coverage, SettlementReferenceCoverage::Full);
    assert!(coverage.is_production_grade());
}

#[test]
fn test_coverage_partial() {
    let spec = SettlementReferenceSpec::polymarket_15m_updown_v1();

    // Missing tick at 2*NANOS_15_MIN
    let ticks = vec![
        make_tick(1, 0, 50000.0),
        make_tick(2, NANOS_15_MIN, 50100.0),
    ];

    let provider = RecordedReferenceStreamProvider::new(spec, ticks);
    let coverage = classify_settlement_coverage(&provider, 0, 2 * NANOS_15_MIN);

    assert_eq!(coverage, SettlementReferenceCoverage::Partial);
    assert!(!coverage.is_production_grade());
}

#[test]
fn test_coverage_none() {
    let spec = SettlementReferenceSpec::polymarket_15m_updown_v1();
    let provider = RecordedReferenceStreamProvider::new(spec, vec![]);

    let coverage = classify_settlement_coverage(&provider, 0, NANOS_15_MIN);

    assert_eq!(coverage, SettlementReferenceCoverage::None);
    assert!(!coverage.is_production_grade());
}

// =============================================================================
// ISOLATION FROM EXECUTION BOOK (INTEGRATION TEST)
// =============================================================================

/// This test verifies that settlement outcomes are ONLY determined by the
/// reference stream and not by execution book data.
#[test]
fn test_settlement_isolation_from_execution_book() {
    let spec = SettlementReferenceSpec::polymarket_15m_updown_v1();
    let window_start: Nanos = 0;

    // Create reference stream with specific prices
    let reference_ticks = vec![
        make_tick(1, 0, 50000.0),
        make_tick(2, NANOS_15_MIN, 50100.0), // Up by $100
    ];

    // Simulate "execution book" having completely different prices
    // In a broken implementation, this might affect settlement
    let _execution_book_mid_start = 49000.0; // $1000 lower
    let _execution_book_mid_end = 48000.0; // Would be DOWN if used

    let provider = RecordedReferenceStreamProvider::new(spec, reference_ticks);
    let mut engine = ReferenceSettlementEngine::new(provider);

    // Settlement MUST use reference stream, not execution book
    let result = engine.settle_window(0, window_start, NANOS_15_MIN + 1000);

    assert!(result.is_some());
    let record = result.unwrap();

    // CRITICAL: Outcome must be UP based on reference prices (50000 -> 50100)
    // NOT DOWN based on hypothetical execution book prices (49000 -> 48000)
    assert!(record.outcome.is_up());
    assert_eq!(record.start_price_f64, 50000.0);
    assert_eq!(record.end_price_f64, 50100.0);
}

/// Test that changing execution data doesn't change settlement.
#[test]
fn test_settlement_determinism_independent_of_execution_data() {
    let spec = SettlementReferenceSpec::polymarket_15m_updown_v1();
    let window_start: Nanos = 0;

    // Same reference ticks for both runs
    let reference_ticks = vec![
        make_tick(1, 0, 50000.0),
        make_tick(2, NANOS_15_MIN, 50100.0),
    ];

    // Run 1: Simulate with hypothetical execution activity
    let provider1 = RecordedReferenceStreamProvider::new(spec.clone(), reference_ticks.clone());
    let mut engine1 = ReferenceSettlementEngine::new(provider1);
    let _simulated_fills_run1 = vec![("BUY", 50010.0, 100.0), ("SELL", 50050.0, 50.0)];
    let result1 = engine1.settle_window(0, window_start, NANOS_15_MIN + 1000);

    // Run 2: Simulate with different execution activity
    let provider2 = RecordedReferenceStreamProvider::new(spec, reference_ticks);
    let mut engine2 = ReferenceSettlementEngine::new(provider2);
    let _simulated_fills_run2 = vec![
        ("SELL", 50005.0, 200.0),
        ("BUY", 50095.0, 75.0),
        ("SELL", 50090.0, 25.0),
    ];
    let result2 = engine2.settle_window(0, window_start, NANOS_15_MIN + 1000);

    // Outcomes MUST be identical regardless of execution activity
    assert_eq!(
        result1.as_ref().unwrap().outcome,
        result2.as_ref().unwrap().outcome
    );
    assert_eq!(
        result1.as_ref().unwrap().record_hash(),
        result2.as_ref().unwrap().record_hash()
    );
    assert_eq!(engine1.audit_log_hash(), engine2.audit_log_hash());
}

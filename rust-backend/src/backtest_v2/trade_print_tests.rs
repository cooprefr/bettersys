//! Trade Print Invariants and Tests
//!
//! Comprehensive tests for HFT-grade trade print recording and replay.

use crate::backtest_v2::events::Side;
use crate::backtest_v2::trade_print::{
    AggressorSideSource, PolymarketTradePrint, TradePrintBuilder, TradePrintDeduplicator,
    TradeIdSource, TradeSequenceTracker, TradePrintError,
};
use crate::backtest_v2::trade_print_storage::{
    TradePrintFullStorage, TradePrintReplayFeed,
};
use crate::backtest_v2::trade_print_attribution::{
    AttributionConfig, AttributionEngine,
};

// =============================================================================
// INVARIANT: Unique (market_id, trade_id)
// =============================================================================

#[test]
fn test_invariant_unique_market_trade_id() {
    let storage = TradePrintFullStorage::open_memory().unwrap();

    // Create two prints with same (market_id, trade_id)
    let mut print1 = TradePrintBuilder::new()
        .market_id("market_1")
        .token_id("token_1")
        .trade_id("trade_001", TradeIdSource::NativeVenueId)
        .aggressor_side(Side::Buy, AggressorSideSource::VenueProvided)
        .price(0.50)
        .size(100.0)
        .ingest_ts_ns(1_000_000_000)
        .build()
        .unwrap();

    let mut print2 = TradePrintBuilder::new()
        .market_id("market_1")
        .token_id("token_1")
        .trade_id("trade_001", TradeIdSource::NativeVenueId) // Same trade_id
        .aggressor_side(Side::Sell, AggressorSideSource::VenueProvided)
        .price(0.51)
        .size(50.0)
        .ingest_ts_ns(2_000_000_000)
        .build()
        .unwrap();

    // First should succeed
    assert!(storage.store(&mut print1).unwrap());

    // Second should be detected as duplicate
    assert!(!storage.store(&mut print2).unwrap());

    // Only one print should be stored
    assert_eq!(storage.count_prints("market_1").unwrap(), 1);
}

// =============================================================================
// INVARIANT: Non-negative size
// =============================================================================

#[test]
fn test_invariant_non_negative_size() {
    let result = TradePrintBuilder::new()
        .market_id("market_1")
        .token_id("token_1")
        .trade_id("trade_001", TradeIdSource::NativeVenueId)
        .aggressor_side(Side::Buy, AggressorSideSource::VenueProvided)
        .price(0.50)
        .size(-100.0) // Invalid: negative size
        .ingest_ts_ns(1_000_000_000)
        .build();

    assert!(result.is_ok()); // Builder doesn't validate

    let print = result.unwrap();
    assert!(print.validate_size().is_err()); // Validation catches it
}

#[test]
fn test_invariant_zero_size_rejected() {
    let mut print = TradePrintBuilder::new()
        .market_id("market_1")
        .token_id("token_1")
        .trade_id("trade_001", TradeIdSource::NativeVenueId)
        .aggressor_side(Side::Buy, AggressorSideSource::VenueProvided)
        .price(0.50)
        .size(0.0) // Invalid: zero size
        .ingest_ts_ns(1_000_000_000)
        .build()
        .unwrap();

    let storage = TradePrintFullStorage::open_memory().unwrap();
    assert!(!storage.store(&mut print).unwrap()); // Should be rejected

    assert_eq!(storage.stats().prints_skipped_invalid, 1);
}

// =============================================================================
// INVARIANT: Price in valid tick grid
// =============================================================================

#[test]
fn test_invariant_price_on_tick_grid() {
    // Valid prices (on 0.01 tick grid)
    let valid_prices = [0.00, 0.01, 0.50, 0.99, 1.00];

    for price in valid_prices {
        let print = TradePrintBuilder::new()
            .market_id("market_1")
            .token_id("token_1")
            .trade_id(format!("trade_{}", (price * 100.0) as i32), TradeIdSource::NativeVenueId)
            .aggressor_side(Side::Buy, AggressorSideSource::VenueProvided)
            .price(price)
            .size(100.0)
            .ingest_ts_ns(1_000_000_000)
            .tick_size(0.01)
            .build()
            .unwrap();

        assert!(
            print.validate_price().is_ok(),
            "Price {} should be valid on 0.01 tick grid",
            price
        );
    }
}

#[test]
fn test_invariant_price_off_tick_grid() {
    // Invalid prices (off 0.01 tick grid)
    let invalid_prices = [0.015, 0.505, 0.123, 0.999];

    for price in invalid_prices {
        let mut print = TradePrintBuilder::new()
            .market_id("market_1")
            .token_id("token_1")
            .trade_id("trade_001", TradeIdSource::NativeVenueId)
            .aggressor_side(Side::Buy, AggressorSideSource::VenueProvided)
            .price(price)
            .size(100.0)
            .ingest_ts_ns(1_000_000_000)
            .tick_size(0.01)
            .build()
            .unwrap();

        print.tick_size = 0.01;

        assert!(
            print.validate_price().is_err(),
            "Price {} should be invalid on 0.01 tick grid",
            price
        );
    }
}

#[test]
fn test_invariant_price_out_of_range() {
    // Prices outside [0.0, 1.0] for probability markets
    let invalid_prices = [-0.01, 1.01, 2.0, -0.5];

    for price in invalid_prices {
        let print = TradePrintBuilder::new()
            .market_id("market_1")
            .token_id("token_1")
            .trade_id("trade_001", TradeIdSource::NativeVenueId)
            .aggressor_side(Side::Buy, AggressorSideSource::VenueProvided)
            .price(price)
            .size(100.0)
            .ingest_ts_ns(1_000_000_000)
            .build()
            .unwrap();

        assert!(
            print.validate_price().is_err(),
            "Price {} should be out of range",
            price
        );
    }
}

// =============================================================================
// INVARIANT: Monotone trade_seq if declared
// =============================================================================

#[test]
fn test_invariant_monotone_trade_seq() {
    let mut tracker = TradeSequenceTracker::new();

    // Create prints with monotonically increasing trade_seq
    let mut prints: Vec<PolymarketTradePrint> = (1..=5)
        .map(|i| {
            let mut p = TradePrintBuilder::new()
                .market_id("market_1")
                .token_id("token_1")
                .trade_id(format!("trade_{:03}", i), TradeIdSource::NativeVenueId)
                .trade_seq(i as u64)
                .aggressor_side(Side::Buy, AggressorSideSource::VenueProvided)
                .price(0.50)
                .size(100.0)
                .ingest_ts_ns(i * 1_000_000_000)
                .build()
                .unwrap();
            p.trade_seq = Some(i as u64);
            p
        })
        .collect();

    // Process all - should have no errors
    for print in &mut prints {
        let result = tracker.process(print);
        assert!(
            result.is_none(),
            "Monotonic sequence should not produce errors"
        );
    }

    assert_eq!(tracker.total_gaps(), 0);
}

#[test]
fn test_invariant_non_monotone_trade_seq_error() {
    let mut tracker = TradeSequenceTracker::new();

    // First print with seq=5
    let mut print1 = TradePrintBuilder::new()
        .market_id("market_1")
        .token_id("token_1")
        .trade_id("trade_001", TradeIdSource::NativeVenueId)
        .aggressor_side(Side::Buy, AggressorSideSource::VenueProvided)
        .price(0.50)
        .size(100.0)
        .ingest_ts_ns(1_000_000_000)
        .build()
        .unwrap();
    print1.trade_seq = Some(5);

    // Second print with seq=3 (out of order)
    let mut print2 = TradePrintBuilder::new()
        .market_id("market_1")
        .token_id("token_1")
        .trade_id("trade_002", TradeIdSource::NativeVenueId)
        .aggressor_side(Side::Buy, AggressorSideSource::VenueProvided)
        .price(0.51)
        .size(50.0)
        .ingest_ts_ns(2_000_000_000)
        .build()
        .unwrap();
    print2.trade_seq = Some(3);

    assert!(tracker.process(&mut print1).is_none());

    // Second should produce sequence violation
    let result = tracker.process(&mut print2);
    assert!(result.is_some(), "Out-of-order sequence should produce error");

    if let Some(TradePrintError::SequenceViolation { expected_seq, actual_seq, .. }) = result {
        assert_eq!(expected_seq, 6); // Expected next seq after 5
        assert_eq!(actual_seq, 3);
    }
}

#[test]
fn test_invariant_sequence_gap_detection() {
    let mut tracker = TradeSequenceTracker::new();

    // seq=1
    let mut print1 = TradePrintBuilder::new()
        .market_id("market_1")
        .token_id("token_1")
        .trade_id("trade_001", TradeIdSource::NativeVenueId)
        .aggressor_side(Side::Buy, AggressorSideSource::VenueProvided)
        .price(0.50)
        .size(100.0)
        .ingest_ts_ns(1_000_000_000)
        .build()
        .unwrap();
    print1.trade_seq = Some(1);

    // seq=5 (gap: missing 2,3,4)
    let mut print2 = TradePrintBuilder::new()
        .market_id("market_1")
        .token_id("token_1")
        .trade_id("trade_002", TradeIdSource::NativeVenueId)
        .aggressor_side(Side::Buy, AggressorSideSource::VenueProvided)
        .price(0.51)
        .size(50.0)
        .ingest_ts_ns(2_000_000_000)
        .build()
        .unwrap();
    print2.trade_seq = Some(5);

    tracker.process(&mut print1);
    tracker.process(&mut print2); // No error - gaps are logged but allowed

    assert_eq!(tracker.gaps_detected("market_1"), 1);
}

// =============================================================================
// INVARIANT: Deterministic replay hash stability
// =============================================================================

#[test]
fn test_invariant_replay_fingerprint_determinism() {
    // Create identical datasets
    let create_prints = || {
        (1..=10)
            .map(|i| {
                TradePrintBuilder::new()
                    .market_id("market_1")
                    .token_id("token_1")
                    .trade_id(format!("trade_{:03}", i), TradeIdSource::NativeVenueId)
                    .aggressor_side(if i % 2 == 0 { Side::Buy } else { Side::Sell }, AggressorSideSource::VenueProvided)
                    .price(0.50 + (i as f64 * 0.01))
                    .size(100.0 * i as f64)
                    .ingest_ts_ns(i * 1_000_000_000)
                    .build()
                    .map(|mut p| {
                        p.visible_ts_ns = i * 1_000_000_000 + 100_000; // Add latency
                        p
                    })
                    .unwrap()
            })
            .collect::<Vec<_>>()
    };

    let prints1 = create_prints();
    let prints2 = create_prints();

    let feed1 = TradePrintReplayFeed::new(prints1);
    let feed2 = TradePrintReplayFeed::new(prints2);

    assert_eq!(
        feed1.fingerprint(),
        feed2.fingerprint(),
        "Identical datasets must produce identical fingerprints"
    );
}

#[test]
fn test_invariant_different_data_different_fingerprint() {
    let prints1: Vec<_> = (1..=5)
        .map(|i| {
            TradePrintBuilder::new()
                .market_id("market_1")
                .token_id("token_1")
                .trade_id(format!("trade_{:03}", i), TradeIdSource::NativeVenueId)
                .aggressor_side(Side::Buy, AggressorSideSource::VenueProvided)
                .price(0.50)
                .size(100.0)
                .ingest_ts_ns(i * 1_000_000_000)
                .build()
                .unwrap()
        })
        .collect();

    let prints2: Vec<_> = (1..=5)
        .map(|i| {
            TradePrintBuilder::new()
                .market_id("market_1")
                .token_id("token_1")
                .trade_id(format!("trade_{:03}", i), TradeIdSource::NativeVenueId)
                .aggressor_side(Side::Buy, AggressorSideSource::VenueProvided)
                .price(0.51) // Different price
                .size(100.0)
                .ingest_ts_ns(i * 1_000_000_000)
                .build()
                .unwrap()
        })
        .collect();

    let feed1 = TradePrintReplayFeed::new(prints1);
    let feed2 = TradePrintReplayFeed::new(prints2);

    assert_ne!(
        feed1.fingerprint(),
        feed2.fingerprint(),
        "Different datasets must produce different fingerprints"
    );
}

// =============================================================================
// INVARIANT: Stable ordering under equal visible_ts
// =============================================================================

#[test]
fn test_invariant_stable_ordering_equal_visible_ts() {
    let storage = TradePrintFullStorage::open_memory().unwrap();

    // Create prints with same visible_ts but different synthetic_trade_seq
    let mut prints: Vec<PolymarketTradePrint> = (1..=5)
        .map(|i| {
            let mut p = TradePrintBuilder::new()
                .market_id("market_1")
                .token_id("token_1")
                .trade_id(format!("trade_{:03}", i), TradeIdSource::NativeVenueId)
                .aggressor_side(Side::Buy, AggressorSideSource::VenueProvided)
                .price(0.50 + (i as f64 * 0.01))
                .size(100.0)
                .ingest_ts_ns(1_000_000_000) // Same time
                .build()
                .unwrap();
            p.visible_ts_ns = 1_000_000_100; // Same visible_ts for all
            p
        })
        .collect();

    storage.store_batch(&mut prints).unwrap();

    // Load and verify ordering by synthetic_trade_seq
    let loaded = storage
        .load_by_visible_ts("market_1", 0, i64::MAX)
        .unwrap();

    assert_eq!(loaded.len(), 5);

    for i in 1..loaded.len() {
        assert!(
            loaded[i].synthetic_trade_seq > loaded[i - 1].synthetic_trade_seq,
            "Prints with same visible_ts must be ordered by synthetic_trade_seq"
        );
    }
}

// =============================================================================
// FIXED-POINT CONVERSION TESTS
// =============================================================================

#[test]
fn test_fixed_point_price_conversion() {
    let test_cases = [
        (0.01, 1_000_000i64),
        (0.50, 50_000_000i64),
        (0.99, 99_000_000i64),
        (1.00, 100_000_000i64),
        (0.001, 100_000i64),
    ];

    for (price, expected_fixed) in test_cases {
        let print = TradePrintBuilder::new()
            .market_id("market_1")
            .token_id("token_1")
            .trade_id("trade_001", TradeIdSource::NativeVenueId)
            .aggressor_side(Side::Buy, AggressorSideSource::VenueProvided)
            .price(price)
            .size(100.0)
            .ingest_ts_ns(1_000_000_000)
            .build()
            .unwrap();

        assert_eq!(
            print.price_fixed, expected_fixed,
            "Price {} should convert to fixed-point {}",
            price, expected_fixed
        );
    }
}

#[test]
fn test_fixed_point_size_conversion() {
    let test_cases = [
        (1.0, 100_000_000i64),
        (100.0, 10_000_000_000i64),
        (0.001, 100_000i64),
        (123.456, 12_345_600_000i64),
    ];

    for (size, expected_fixed) in test_cases {
        let print = TradePrintBuilder::new()
            .market_id("market_1")
            .token_id("token_1")
            .trade_id("trade_001", TradeIdSource::NativeVenueId)
            .aggressor_side(Side::Buy, AggressorSideSource::VenueProvided)
            .price(0.50)
            .size(size)
            .ingest_ts_ns(1_000_000_000)
            .build()
            .unwrap();

        assert_eq!(
            print.size_fixed, expected_fixed,
            "Size {} should convert to fixed-point {}",
            size, expected_fixed
        );
    }
}

// =============================================================================
// DEDUPLICATION TESTS
// =============================================================================

#[test]
fn test_deduplication_across_markets() {
    let mut dedup = TradePrintDeduplicator::new(100);

    // Same trade_id but different markets
    let print1 = TradePrintBuilder::new()
        .market_id("market_1")
        .token_id("token_1")
        .trade_id("trade_001", TradeIdSource::NativeVenueId)
        .aggressor_side(Side::Buy, AggressorSideSource::VenueProvided)
        .price(0.50)
        .size(100.0)
        .ingest_ts_ns(1_000_000_000)
        .build()
        .unwrap();

    let print2 = TradePrintBuilder::new()
        .market_id("market_2") // Different market
        .token_id("token_1")
        .trade_id("trade_001", TradeIdSource::NativeVenueId) // Same trade_id
        .aggressor_side(Side::Buy, AggressorSideSource::VenueProvided)
        .price(0.50)
        .size(100.0)
        .ingest_ts_ns(1_000_000_000)
        .build()
        .unwrap();

    // Both should be unique (different markets)
    assert!(!dedup.is_duplicate(&print1));
    assert!(!dedup.is_duplicate(&print2));
}

// =============================================================================
// CLASSIFICATION DOWNGRADE TESTS
// =============================================================================

#[test]
fn test_trade_id_source_trust_downgrade() {
    assert!(
        !TradeIdSource::NativeVenueId.requires_trust_downgrade(),
        "NativeVenueId should not require downgrade"
    );
    assert!(
        !TradeIdSource::CompositeDerived.requires_trust_downgrade(),
        "CompositeDerived should not require downgrade"
    );
    assert!(
        !TradeIdSource::HashDerived.requires_trust_downgrade(),
        "HashDerived should not require downgrade"
    );
    assert!(
        TradeIdSource::Synthetic.requires_trust_downgrade(),
        "Synthetic MUST require trust downgrade"
    );
}

#[test]
fn test_aggressor_side_microstructure_claims() {
    assert!(
        AggressorSideSource::VenueProvided.supports_microstructure_claims(),
        "VenueProvided should support microstructure claims"
    );
    assert!(
        AggressorSideSource::InferredQuoteRule.supports_microstructure_claims(),
        "InferredQuoteRule should support microstructure claims"
    );
    assert!(
        !AggressorSideSource::InferredTickRule.supports_microstructure_claims(),
        "InferredTickRule should NOT support microstructure claims"
    );
    assert!(
        !AggressorSideSource::Unknown.supports_microstructure_claims(),
        "Unknown should NOT support microstructure claims"
    );
}

// =============================================================================
// ATTRIBUTION ENGINE TESTS
// =============================================================================

#[test]
fn test_attribution_disabled_by_default() {
    let config = AttributionConfig::default();
    assert!(!config.enabled, "Attribution should be disabled by default for performance");
}

#[test]
fn test_attribution_skipped_when_disabled() {
    let config = AttributionConfig::default(); // disabled
    let mut engine = AttributionEngine::new(config);

    let report = engine.attribute_fill(
        "fill_1".to_string(),
        1,
        "market_1",
        Side::Buy,
        0.50,
        100.0,
        1_000_000_000,
        false,
    );

    assert!(!report.attribution_complete);
    assert!(report.incomplete_reasons.iter().any(|r| r.contains("disabled")));
    assert_eq!(engine.stats().fills_skipped, 1);
}

#[test]
fn test_attribution_no_prints_incomplete() {
    let config = AttributionConfig {
        enabled: true,
        ..Default::default()
    };
    let mut engine = AttributionEngine::new(config);

    // No prints added
    let report = engine.attribute_fill(
        "fill_1".to_string(),
        1,
        "market_1",
        Side::Buy,
        0.50,
        100.0,
        1_000_000_000,
        false,
    );

    assert!(!report.attribution_complete);
    assert!(report.incomplete_reasons.iter().any(|r| r.contains("No nearby trade prints")));
}

// =============================================================================
// STORAGE PERSISTENCE TESTS
// =============================================================================

#[test]
fn test_storage_roundtrip() {
    let storage = TradePrintFullStorage::open_memory().unwrap();

    let original = TradePrintBuilder::new()
        .market_id("market_1")
        .token_id("token_123")
        .market_slug("btc-updown-15m-1705320000")
        .trade_id("trade_001", TradeIdSource::NativeVenueId)
        .match_id("match_001")
        .trade_seq(42)
        .aggressor_side(Side::Buy, AggressorSideSource::VenueProvided)
        .price(0.55)
        .size(123.456)
        .fee_rate_bps(10)
        .exchange_ts_ns(999_000_000)
        .ingest_ts_ns(1_000_000_000)
        .build()
        .unwrap();

    let mut print = original.clone();
    print.visible_ts_ns = 1_000_100_000;

    storage.store(&mut print).unwrap();

    let loaded = storage
        .load_by_visible_ts("market_1", 0, i64::MAX)
        .unwrap();

    assert_eq!(loaded.len(), 1);

    let loaded_print = &loaded[0];
    assert_eq!(loaded_print.market_id, "market_1");
    assert_eq!(loaded_print.token_id, "token_123");
    assert_eq!(loaded_print.market_slug, Some("btc-updown-15m-1705320000".to_string()));
    assert_eq!(loaded_print.trade_id, "trade_001");
    assert_eq!(loaded_print.match_id, Some("match_001".to_string()));
    assert_eq!(loaded_print.trade_seq, Some(42));
    assert_eq!(loaded_print.aggressor_side, Side::Buy);
    assert!((loaded_print.price - 0.55).abs() < 1e-9);
    assert!((loaded_print.size - 123.456).abs() < 1e-9);
    assert_eq!(loaded_print.fee_rate_bps, Some(10));
    assert_eq!(loaded_print.exchange_ts_ns, Some(999_000_000));
    assert_eq!(loaded_print.ingest_ts_ns, 1_000_000_000);
}

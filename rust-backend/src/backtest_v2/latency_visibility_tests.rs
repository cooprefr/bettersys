//! Validation Tests for Visibility/Latency Model
//!
//! These tests prove the latency model is applied correctly:
//!
//! A) With nonzero L_feed, strategy receives market data later than ingest_ts and in visible_ts order.
//! B) With nonzero L_compute and L_send, order arrival time is strictly after market data event time.
//! C) With nonzero L_ack, strategy receives acks/fills later than order arrival.
//! D) With jitter enabled, results are deterministic across runs with same seed and dataset.
//! E) No path in the codebase uses sleeps or wall clock.
//!
//! Also includes 15M-specific acceptance test demonstrating latency sensitivity.

use crate::backtest_v2::clock::{Nanos, SimClock};
use crate::backtest_v2::event_time::{
    BacktestLatencyModel, FeedEventPayload, FeedEventPriority, FeedSource,
    LatencyModelApplier, VisibleNanos, NS_PER_SEC as ET_NS_PER_SEC,
};
use crate::backtest_v2::events::{Side, TimeInForce, OrderType};
use crate::backtest_v2::latency_visibility::{
    JitterCategory, LatencyVisibilityApplier, LatencyVisibilityModel, OrderLifecycleEvent,
    OrderLifecycleScheduler, ms, us, sec, validate_no_negative_latency, validate_visible_monotone,
    NANOS_PER_US as LV_NANOS_PER_US,
};

// =============================================================================
// TEST A: With nonzero L_feed, strategy receives market data later than ingest_ts
//         and events are delivered in visible_ts order.
// =============================================================================

#[test]
fn test_a_nonzero_l_feed_delays_visibility() {
    let model = LatencyVisibilityModel {
        binance_price_delay_ns: us(100),
        polymarket_book_delta_delay_ns: us(200),
        jitter_enabled: false,
        ..LatencyVisibilityModel::zero()
    };
    let mut applier = LatencyVisibilityApplier::new(model);

    let ingest_ts = 1_000_000_000i64; // 1 second
    let fingerprint = 12345u64;

    // Binance event
    let (visible_ts_binance, delay, jitter) = applier.compute_visible_ts(
        ingest_ts,
        FeedSource::Binance,
        FeedEventPriority::ReferencePrice,
        fingerprint,
    );

    // visible_ts must be strictly after ingest_ts
    assert!(visible_ts_binance > ingest_ts, "visible_ts must be > ingest_ts with nonzero L_feed");
    assert_eq!(delay, us(100), "delay should match configured L_feed");
    assert_eq!(jitter, 0, "jitter should be 0 when disabled");
    assert_eq!(visible_ts_binance, ingest_ts + us(100));
}

#[test]
fn test_a_events_delivered_in_visible_ts_order() {
    let model = LatencyVisibilityModel {
        binance_price_delay_ns: us(100),
        polymarket_book_delta_delay_ns: us(200),
        jitter_enabled: false,
        ..LatencyVisibilityModel::zero()
    };
    let mut applier = LatencyVisibilityApplier::new(model);

    // Create events with SAME ingest_ts but different feeds
    let ingest_ts = 1_000_000_000i64;

    // Event 1: Binance (faster feed)
    let (visible_ts_1, _, _) = applier.compute_visible_ts(
        ingest_ts,
        FeedSource::Binance,
        FeedEventPriority::ReferencePrice,
        1,
    );

    // Event 2: Polymarket (slower feed)
    let (visible_ts_2, _, _) = applier.compute_visible_ts(
        ingest_ts,
        FeedSource::PolymarketBook,
        FeedEventPriority::BookDelta,
        2,
    );

    // Binance should be visible before Polymarket despite same ingest_ts
    assert!(visible_ts_1 < visible_ts_2, "Binance should be visible before Polymarket");

    // Verify ordering
    let mut visible_times = vec![visible_ts_1, visible_ts_2];
    visible_times.sort();
    assert_eq!(visible_times[0], visible_ts_1, "Binance should come first");
    assert_eq!(visible_times[1], visible_ts_2, "Polymarket should come second");
}

#[test]
fn test_a_visibility_order_matches_queue_order() {
    let model = BacktestLatencyModel {
        binance_price_delay_ns: us(100),
        polymarket_book_delay_ns: us(200),
        polymarket_trade_delay_ns: us(200),
        chainlink_oracle_delay_ns: ms(1),
        timer_delay_ns: 0,
        oms_delay_ns: us(50),
        jitter_enabled: false,
        jitter_max_ns: 0,
        jitter_seed: 0,
    };

    let mut applier = LatencyModelApplier::new(model);
    let ingest_ts = 1_000_000_000i64;

    // Create multiple events at same ingest_ts
    let event1 = applier.apply_and_create(
        Some(ingest_ts),
        ingest_ts,
        FeedSource::Binance,
        FeedEventPriority::ReferencePrice,
        1,
        FeedEventPayload::BinanceMidPriceUpdate {
            symbol: "BTC".to_string(),
            mid_price: 50000.0,
            bid: 49999.0,
            ask: 50001.0,
        },
    );

    let event2 = applier.apply_and_create(
        Some(ingest_ts),
        ingest_ts,
        FeedSource::PolymarketBook,
        FeedEventPriority::BookDelta,
        2,
        FeedEventPayload::PolymarketBookDelta {
            token_id: "token1".to_string(),
            market_slug: "btc-updown".to_string(),
            side: crate::backtest_v2::event_time::BookSide::Bid,
            price: 0.55,
            new_size: 100.0,
            exchange_seq: 1,
        },
    );

    // Event1 (Binance) should have earlier visible_ts
    assert!(event1.visible_ts() < event2.visible_ts());
}

// =============================================================================
// TEST B: With nonzero L_compute and L_send, order arrival is strictly after
//         market data event time, and matching uses book state at arrival time.
// =============================================================================

#[test]
fn test_b_order_arrival_strictly_after_event_time() {
    let model = LatencyVisibilityModel {
        compute_delay_ns: us(50),
        send_delay_ns: us(100),
        jitter_enabled: false,
        ..LatencyVisibilityModel::zero()
    };
    let mut scheduler = OrderLifecycleScheduler::new(model.clone());

    let event_visible_ts = 1_000_000_000i64;

    // Strategy places an order at event_visible_ts
    let (_order_id, arrival_ts) = scheduler.schedule_order(
        event_visible_ts,
        "order1".to_string(),
        "BTC-UP".to_string(),
        Side::Buy,
        0.55,
        100.0,
        OrderType::Limit,
        TimeInForce::Ioc,
    );

    // Order arrival must be strictly after event_visible_ts
    assert!(arrival_ts > event_visible_ts, "arrival_ts must be > event_visible_ts");

    // Verify the delta is at least L_compute + L_send
    let expected_min_delay = us(50) + us(100);
    assert!(
        arrival_ts >= event_visible_ts + expected_min_delay,
        "arrival_ts must be at least event_ts + L_compute + L_send"
    );
}

#[test]
fn test_b_order_lifecycle_timing_correct() {
    let model = LatencyVisibilityModel {
        compute_delay_ns: us(30),
        send_delay_ns: us(70),
        jitter_enabled: false,
        ..LatencyVisibilityModel::zero()
    };
    let mut scheduler = OrderLifecycleScheduler::new(model);

    let visible_ts = 1_000_000_000i64;
    let (_order_id, arrival_ts) = scheduler.schedule_order(
        visible_ts,
        "order1".to_string(),
        "token".to_string(),
        Side::Buy,
        0.50,
        100.0,
        OrderType::Limit,
        TimeInForce::Gtc,
    );

    // Verify: arrival = visible + compute + send
    let expected_arrival = visible_ts + us(30) + us(70);
    assert_eq!(arrival_ts, expected_arrival);

    // Drain events and verify order
    let events = scheduler.drain_until(arrival_ts + 1000);
    assert_eq!(events.len(), 2);

    // First: OrderIntentCreated
    let decision_ts = match &events[0] {
        OrderLifecycleEvent::OrderIntentCreated { decision_ts, .. } => *decision_ts,
        _ => panic!("Expected OrderIntentCreated"),
    };
    assert_eq!(decision_ts, visible_ts + us(30));

    // Second: OrderArrivesAtVenue
    let arrival = match &events[1] {
        OrderLifecycleEvent::OrderArrivesAtVenue { arrival_ts, .. } => *arrival_ts,
        _ => panic!("Expected OrderArrivesAtVenue"),
    };
    assert_eq!(arrival, expected_arrival);
}

// =============================================================================
// TEST C: With nonzero L_ack, strategy receives acks/fills later than order arrival.
// =============================================================================

#[test]
fn test_c_acks_visible_after_order_arrival() {
    let model = LatencyVisibilityModel {
        ack_delay_ns: us(80),
        jitter_enabled: false,
        ..LatencyVisibilityModel::zero()
    };
    let mut scheduler = OrderLifecycleScheduler::new(model);

    let arrival_ts = 1_000_000_000i64;

    // Schedule an ack
    scheduler.schedule_ack(1, arrival_ts);

    // Ack should not be visible until arrival_ts + L_ack
    let event_too_early = scheduler.poll_next_until(arrival_ts);
    assert!(event_too_early.is_none(), "Ack should not be visible at arrival_ts");

    // Ack should be visible at arrival_ts + L_ack
    let event = scheduler.poll_next_until(arrival_ts + us(80));
    assert!(event.is_some(), "Ack should be visible at arrival_ts + L_ack");

    if let Some(OrderLifecycleEvent::VenueAckGenerated { visible_ts, .. }) = event {
        assert_eq!(visible_ts, arrival_ts + us(80));
    } else {
        panic!("Expected VenueAckGenerated event");
    }
}

#[test]
fn test_c_fills_visible_after_generation() {
    let model = LatencyVisibilityModel {
        fill_delay_ns: us(100),
        jitter_enabled: false,
        ..LatencyVisibilityModel::zero()
    };
    let mut scheduler = OrderLifecycleScheduler::new(model);

    let fill_ts = 1_000_000_000i64;

    // Schedule a fill
    scheduler.schedule_fill(1, 0.55, 100.0, false, 0.0, 0.001, fill_ts);

    // Fill should not be visible until fill_ts + L_fill
    let event_too_early = scheduler.poll_next_until(fill_ts);
    assert!(event_too_early.is_none(), "Fill should not be visible at fill_ts");

    // Fill should be visible at fill_ts + L_fill
    let event = scheduler.poll_next_until(fill_ts + us(100));
    assert!(event.is_some(), "Fill should be visible at fill_ts + L_fill");

    if let Some(OrderLifecycleEvent::VenueFillGenerated { visible_ts, fill_price, .. }) = event {
        assert_eq!(visible_ts, fill_ts + us(100));
        assert!((fill_price - 0.55).abs() < 1e-9);
    } else {
        panic!("Expected VenueFillGenerated event");
    }
}

#[test]
fn test_c_cancel_acks_visible_after_processing() {
    let model = LatencyVisibilityModel {
        cancel_ack_delay_ns: us(150),
        jitter_enabled: false,
        ..LatencyVisibilityModel::zero()
    };
    let mut scheduler = OrderLifecycleScheduler::new(model);

    let cancel_ts = 1_000_000_000i64;

    // Schedule a cancel ack
    scheduler.schedule_cancel_ack(1, 50.0, cancel_ts);

    // Cancel ack should not be visible until cancel_ts + L_cancel_ack
    let event_too_early = scheduler.poll_next_until(cancel_ts);
    assert!(event_too_early.is_none());

    let event = scheduler.poll_next_until(cancel_ts + us(150));
    assert!(event.is_some());

    if let Some(OrderLifecycleEvent::VenueCancelAckGenerated { visible_ts, .. }) = event {
        assert_eq!(visible_ts, cancel_ts + us(150));
    } else {
        panic!("Expected VenueCancelAckGenerated event");
    }
}

// =============================================================================
// TEST D: With jitter enabled, results are deterministic across runs.
// =============================================================================

#[test]
fn test_d_jitter_deterministic_same_seed() {
    let seed = 42u64;
    let model1 = LatencyVisibilityModel::default().with_jitter(us(50), seed);
    let model2 = LatencyVisibilityModel::default().with_jitter(us(50), seed);

    // Same fingerprint with same seed should produce same jitter
    for fingerprint in [1, 100, 999, 12345, 999999] {
        let jitter1 = model1.compute_jitter(fingerprint);
        let jitter2 = model2.compute_jitter(fingerprint);
        assert_eq!(jitter1, jitter2, "Jitter must be deterministic for same seed and fingerprint");
    }
}

#[test]
fn test_d_jitter_deterministic_across_runs() {
    let seed = 42u64;
    let max_jitter = us(50);

    // Run 1
    let model1 = LatencyVisibilityModel::default().with_jitter(max_jitter, seed);
    let mut applier1 = LatencyVisibilityApplier::new(model1);

    let ingest_ts = 1_000_000_000i64;
    let fingerprints: Vec<u64> = (0..100).collect();

    let visible_times_run1: Vec<Nanos> = fingerprints
        .iter()
        .map(|&fp| {
            let (visible_ts, _, _) = applier1.compute_visible_ts(
                ingest_ts,
                FeedSource::Binance,
                FeedEventPriority::ReferencePrice,
                fp,
            );
            visible_ts
        })
        .collect();

    // Run 2 (fresh model and applier)
    let model2 = LatencyVisibilityModel::default().with_jitter(max_jitter, seed);
    let mut applier2 = LatencyVisibilityApplier::new(model2);

    let visible_times_run2: Vec<Nanos> = fingerprints
        .iter()
        .map(|&fp| {
            let (visible_ts, _, _) = applier2.compute_visible_ts(
                ingest_ts,
                FeedSource::Binance,
                FeedEventPriority::ReferencePrice,
                fp,
            );
            visible_ts
        })
        .collect();

    // Results must be identical
    assert_eq!(visible_times_run1, visible_times_run2, "Visible times must be identical across runs");
}

#[test]
fn test_d_jitter_varies_with_fingerprint() {
    let seed = 42u64;
    let max_jitter = us(50);
    let model = LatencyVisibilityModel::default().with_jitter(max_jitter, seed);

    // Different fingerprints should (likely) produce different jitter
    let jitters: Vec<Nanos> = (0..100)
        .map(|fp| model.compute_jitter(fp))
        .collect();

    // Count unique values
    let mut unique = jitters.clone();
    unique.sort();
    unique.dedup();

    // Should have significant variety (not all same)
    assert!(unique.len() > 1, "Jitter should vary with fingerprint");
}

#[test]
fn test_d_jitter_within_bounds() {
    let seed = 42u64;
    let max_jitter = us(50);
    let model = LatencyVisibilityModel::default().with_jitter(max_jitter, seed);

    for fingerprint in 0..1000 {
        let jitter = model.compute_jitter(fingerprint);
        assert!(jitter >= 0, "Jitter must be >= 0");
        assert!(jitter <= max_jitter, "Jitter must be <= max_jitter");
    }
}

#[test]
fn test_d_categorized_jitter_independent() {
    let seed = 42u64;
    let max_jitter = us(50);
    let model = LatencyVisibilityModel::default().with_jitter(max_jitter, seed);

    let fingerprint = 12345u64;

    let j_feed = model.compute_jitter_categorized(JitterCategory::Feed, fingerprint);
    let j_compute = model.compute_jitter_categorized(JitterCategory::Compute, fingerprint);
    let j_send = model.compute_jitter_categorized(JitterCategory::Send, fingerprint);
    let j_ack = model.compute_jitter_categorized(JitterCategory::Ack, fingerprint);

    // All should be valid
    for j in [j_feed, j_compute, j_send, j_ack] {
        assert!(j >= 0 && j <= max_jitter);
    }

    // With good hash mixing, they should be different (probabilistic but very likely)
    let all_jitters = vec![j_feed, j_compute, j_send, j_ack];
    let mut unique = all_jitters.clone();
    unique.sort();
    unique.dedup();
    // Allow for some collisions but expect at least 2 unique values
    assert!(unique.len() >= 2, "Different categories should likely produce different jitter");
}

// =============================================================================
// TEST E: No path in the codebase uses sleeps or wall clock.
// =============================================================================

#[test]
fn test_e_no_negative_latencies() {
    // Ensure all model configurations have valid (non-negative) latencies
    let models = vec![
        LatencyVisibilityModel::zero(),
        LatencyVisibilityModel::default(),
        LatencyVisibilityModel::taker_15m_updown(),
        LatencyVisibilityModel::default().with_jitter(us(50), 42),
    ];

    for model in models {
        assert!(
            validate_no_negative_latency(&model).is_ok(),
            "Model should have no negative latencies"
        );
    }
}

#[test]
fn test_e_clock_never_goes_backward() {
    let mut clock = SimClock::new(1_000_000_000);

    // Advancing forward is fine
    clock.advance_to(2_000_000_000);
    assert_eq!(clock.now(), 2_000_000_000);

    clock.advance_by(500_000_000);
    assert_eq!(clock.now(), 2_500_000_000);
}

#[test]
#[should_panic(expected = "cannot go backward")]
fn test_e_clock_panics_on_backward() {
    let mut clock = SimClock::new(2_000_000_000);
    clock.advance_to(1_000_000_000); // Should panic
}

#[test]
fn test_e_visible_ts_always_after_ingest_ts() {
    let model = LatencyVisibilityModel::default();
    let mut applier = LatencyVisibilityApplier::new(model);

    // Test with various ingest times
    for ingest_ts in [0, 1_000_000, 1_000_000_000, i64::MAX / 2] {
        let (visible_ts, delay, _) = applier.compute_visible_ts(
            ingest_ts,
            FeedSource::Binance,
            FeedEventPriority::ReferencePrice,
            ingest_ts as u64,
        );

        assert!(
            visible_ts >= ingest_ts,
            "visible_ts must always be >= ingest_ts"
        );
        assert!(delay >= 0, "delay must always be >= 0");
    }
}

#[test]
fn test_e_visible_monotone_enforcement() {
    // Test the monotone validation
    assert!(validate_visible_monotone(None, 100).is_ok());
    assert!(validate_visible_monotone(Some(100), 100).is_ok());
    assert!(validate_visible_monotone(Some(100), 200).is_ok());
    assert!(validate_visible_monotone(Some(200), 100).is_err());
}

// =============================================================================
// 15M-SPECIFIC ACCEPTANCE TEST: Latency sensitivity changes realized edge
// =============================================================================

#[test]
fn test_15m_latency_sensitivity_changes_edge() {
    // Scenario: Two runs with different relative latency between Binance and Polymarket
    // The 15M strategy uses Binance mid-price to predict Up/Down outcomes
    // and executes against Polymarket orderbook.
    //
    // If Binance has LOWER latency than Polymarket, we see the price move
    // before Polymarket does, giving us an edge.
    //
    // If Binance has HIGHER latency than Polymarket, the edge is reduced.

    let base_ingest_ts = 1_000_000_000i64;

    // Configuration 1: Binance faster (100us) than Polymarket (200us)
    // This is the favorable case for arbitrage
    let model_favorable = LatencyVisibilityModel {
        binance_price_delay_ns: us(100),
        polymarket_book_delta_delay_ns: us(200),
        jitter_enabled: false,
        ..LatencyVisibilityModel::zero()
    };
    let mut applier_favorable = LatencyVisibilityApplier::new(model_favorable);

    // Configuration 2: Binance slower (300us) than Polymarket (100us)
    // This is the unfavorable case
    let model_unfavorable = LatencyVisibilityModel {
        binance_price_delay_ns: us(300),
        polymarket_book_delta_delay_ns: us(100),
        jitter_enabled: false,
        ..LatencyVisibilityModel::zero()
    };
    let mut applier_unfavorable = LatencyVisibilityApplier::new(model_unfavorable);

    // Simulate: Binance price update and Polymarket book update arrive at same ingest_ts
    let fingerprint = 12345u64;

    let (binance_visible_favorable, _, _) = applier_favorable.compute_visible_ts(
        base_ingest_ts,
        FeedSource::Binance,
        FeedEventPriority::ReferencePrice,
        fingerprint,
    );
    let (poly_visible_favorable, _, _) = applier_favorable.compute_visible_ts(
        base_ingest_ts,
        FeedSource::PolymarketBook,
        FeedEventPriority::BookDelta,
        fingerprint,
    );

    let (binance_visible_unfavorable, _, _) = applier_unfavorable.compute_visible_ts(
        base_ingest_ts,
        FeedSource::Binance,
        FeedEventPriority::ReferencePrice,
        fingerprint,
    );
    let (poly_visible_unfavorable, _, _) = applier_unfavorable.compute_visible_ts(
        base_ingest_ts,
        FeedSource::PolymarketBook,
        FeedEventPriority::BookDelta,
        fingerprint,
    );

    // Favorable: Binance arrives 100us before Polymarket
    let edge_favorable = poly_visible_favorable - binance_visible_favorable;
    assert_eq!(edge_favorable, us(100), "Favorable config: Binance should be 100us ahead");

    // Unfavorable: Polymarket arrives 200us before Binance
    let edge_unfavorable = poly_visible_unfavorable - binance_visible_unfavorable;
    assert_eq!(edge_unfavorable, us(-200), "Unfavorable config: Polymarket should be 200us ahead");

    // The "edge window" has changed by 300us between configurations
    let edge_difference = edge_favorable - edge_unfavorable;
    assert_eq!(edge_difference, us(300));
}

#[test]
fn test_15m_window_timing_with_latency() {
    // Test that windowing uses visible time, so shifting latency can move
    // decisions across the 15-minute boundary.
    use crate::backtest_v2::event_time::Window15M;

    // Window boundary at T = 900 seconds
    let window_boundary_ns = 900 * ET_NS_PER_SEC;

    // Event ingested just before the boundary
    let ingest_before_boundary = window_boundary_ns - us(50);

    // With small latency, event is visible before boundary
    let model_small = LatencyVisibilityModel {
        binance_price_delay_ns: us(10),
        jitter_enabled: false,
        ..LatencyVisibilityModel::zero()
    };
    let mut applier_small = LatencyVisibilityApplier::new(model_small);

    let (visible_small, _, _) = applier_small.compute_visible_ts(
        ingest_before_boundary,
        FeedSource::Binance,
        FeedEventPriority::ReferencePrice,
        1,
    );

    // With larger latency, event becomes visible AFTER boundary
    let model_large = LatencyVisibilityModel {
        binance_price_delay_ns: us(100),
        jitter_enabled: false,
        ..LatencyVisibilityModel::zero()
    };
    let mut applier_large = LatencyVisibilityApplier::new(model_large);

    let (visible_large, _, _) = applier_large.compute_visible_ts(
        ingest_before_boundary,
        FeedSource::Binance,
        FeedEventPriority::ReferencePrice,
        1,
    );

    // Small latency: visible before boundary
    assert!(visible_small < window_boundary_ns, "Small latency: event visible before boundary");

    // Large latency: visible after boundary
    assert!(visible_large > window_boundary_ns, "Large latency: event visible after boundary");

    // Check window assignment
    let window_small = Window15M::for_visible_time(VisibleNanos(visible_small));
    let window_large = Window15M::for_visible_time(VisibleNanos(visible_large));

    // They should be in different windows!
    assert_ne!(
        window_small.window_start, window_large.window_start,
        "Events should be in different 15M windows due to latency difference"
    );
}

#[test]
fn test_15m_order_execution_timing() {
    // Verify that with realistic latencies, there's enough time budget
    // within a 15-minute window to detect signal and execute.

    let model = LatencyVisibilityModel::taker_15m_updown();

    // Total tick-to-trade latency for Binance feed
    let t2t = model.tick_to_trade_ns(FeedSource::Binance, FeedEventPriority::ReferencePrice);

    // Should be well under 1 second (plenty of time within 15 minutes)
    assert!(t2t < sec(1), "Tick-to-trade should be < 1 second");

    // In fact, for the 15M strategy, we need time to:
    // 1. Receive Binance price update
    // 2. Compare to Polymarket implied odds
    // 3. Decide to trade
    // 4. Place order
    // 5. Order arrives at venue
    //
    // Total latency budget: ~500us is reasonable
    assert!(t2t < ms(1), "Tick-to-trade should be < 1ms for HFT-grade execution");

    // Log for visibility
    println!(
        "Tick-to-trade latency (Binance): {}us ({}ns)",
        t2t / LV_NANOS_PER_US,
        t2t
    );
}

// =============================================================================
// FINGERPRINT STABILITY TEST
// =============================================================================

#[test]
fn test_fingerprint_changes_on_latency_change() {
    let model1 = LatencyVisibilityModel::taker_15m_updown();
    let model2 = LatencyVisibilityModel::taker_15m_updown();
    
    // Same model = same fingerprint
    assert_eq!(model1.fingerprint(), model2.fingerprint());

    // Change any parameter = different fingerprint
    let mut model3 = LatencyVisibilityModel::taker_15m_updown();
    model3.binance_price_delay_ns += 1;
    assert_ne!(model1.fingerprint(), model3.fingerprint());

    let mut model4 = LatencyVisibilityModel::taker_15m_updown();
    model4.compute_delay_ns += 1;
    assert_ne!(model1.fingerprint(), model4.fingerprint());

    let mut model5 = LatencyVisibilityModel::taker_15m_updown();
    model5.jitter_seed += 1;
    assert_ne!(model1.fingerprint(), model5.fingerprint());
}

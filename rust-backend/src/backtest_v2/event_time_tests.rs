//! Tests for Event Time Model
//!
//! These tests verify:
//! 1. Three-timestamp model correctness
//! 2. Deterministic replay
//! 3. Strategy time isolation
//! 4. 15M window semantics

use super::event_time::*;
use super::unified_feed::*;

// =============================================================================
// TIME-LEAK TEST: Strategy cannot access exchange or ingest timestamps
// =============================================================================

/// Dummy strategy that attempts to read exchange time from event payload.
/// This test verifies that the `StrategyEventView` API does NOT expose
/// `exchange_ts` or `ingest_ts`.
#[test]
fn test_strategy_cannot_access_exchange_time() {
    let event = FeedEvent {
        time: EventTime::with_all(
            Some(100), // exchange_ts - should be hidden
            150,       // ingest_ts - should be hidden
            VisibleNanos(200), // visible_ts - the only accessible time
        ),
        source: FeedSource::Binance,
        priority: FeedEventPriority::ReferencePrice,
        dataset_seq: 1,
        payload: FeedEventPayload::BinanceMidPriceUpdate {
            symbol: "BTC".into(),
            mid_price: 50000.0,
            bid: 49999.0,
            ask: 50001.0,
        },
    };

    let view = StrategyEventView::from_event(&event);

    // Strategy can access visible_ts
    assert_eq!(view.visible_ts.0, 200);

    // Strategy CANNOT access exchange_ts or ingest_ts through the view
    // (They are simply not part of the StrategyEventView struct)
    // This is compile-time enforcement: the fields don't exist.
    
    // Verify the payload is accessible
    if let FeedEventPayload::BinanceMidPriceUpdate { mid_price, .. } = view.payload {
        assert_eq!(*mid_price, 50000.0);
    } else {
        panic!("Wrong payload type");
    }
}

// =============================================================================
// DETERMINISM TESTS
// =============================================================================

#[test]
fn test_latency_model_deterministic_across_runs() {
    let model = BacktestLatencyModel::realistic_with_jitter(42);

    // Run 1
    let mut results1 = Vec::new();
    for fingerprint in 0..100 {
        let visible = model.compute_visible_ts(
            1000 * NS_PER_SEC,
            FeedSource::Binance,
            FeedEventPriority::ReferencePrice,
            fingerprint,
        );
        results1.push(visible);
    }

    // Run 2 with same seed
    let model2 = BacktestLatencyModel::realistic_with_jitter(42);
    let mut results2 = Vec::new();
    for fingerprint in 0..100 {
        let visible = model2.compute_visible_ts(
            1000 * NS_PER_SEC,
            FeedSource::Binance,
            FeedEventPriority::ReferencePrice,
            fingerprint,
        );
        results2.push(visible);
    }

    // Results must be identical
    assert_eq!(results1, results2);
}

#[test]
fn test_queue_deterministic_ordering() {
    fn run_queue(seed: u64) -> Vec<(VisibleNanos, FeedSource, u64)> {
        let model = BacktestLatencyModel::realistic_with_jitter(seed);
        let mut queue = UnifiedFeedQueue::new(model, false);

        // Push events in a specific order
        for i in 0..10 {
            queue.push(
                Some(1000 + i),
                1000 + i,
                FeedSource::Binance,
                FeedEventPriority::ReferencePrice,
                FeedEventPayload::Timer {
                    timer_id: i as u64,
                    payload: None,
                },
            );
        }

        // Drain and collect
        let mut results = Vec::new();
        while let Some(event) = queue.pop() {
            results.push((event.time.visible_ts, event.source, event.dataset_seq));
        }
        results
    }

    // Same seed should produce identical results
    let run1 = run_queue(42);
    let run2 = run_queue(42);
    assert_eq!(run1, run2);

    // Different seed should produce different results (with high probability)
    let run3 = run_queue(99);
    assert_ne!(run1, run3);
}

// =============================================================================
// VISIBLE TIME MONOTONICITY TESTS
// =============================================================================

#[test]
fn test_visible_monotone_within_feed() {
    let model = BacktestLatencyModel::default();
    let mut queue = UnifiedFeedQueue::new(model, false);

    // Push events with increasing ingest times
    for i in 0..100 {
        queue.push(
            Some(i * NS_PER_MS),
            i * NS_PER_MS,
            FeedSource::Binance,
            FeedEventPriority::ReferencePrice,
            FeedEventPayload::Timer {
                timer_id: i as u64,
                payload: None,
            },
        );
    }

    // Pop and verify monotonicity
    let mut prev_visible_ts: Option<VisibleNanos> = None;
    while let Some(event) = queue.pop() {
        if let Some(prev) = prev_visible_ts {
            assert!(
                event.time.visible_ts >= prev,
                "visible_ts must be monotone: prev={}, curr={}",
                prev,
                event.time.visible_ts
            );
        }
        prev_visible_ts = Some(event.time.visible_ts);
    }
}

// =============================================================================
// 15M WINDOW SEMANTICS TESTS
// =============================================================================

#[test]
fn test_window_boundary_alignment() {
    // Test various times and their window boundaries
    let test_cases = vec![
        // (visible_ts_secs, expected_window_start_secs, expected_window_end_secs)
        (0, 0, 900),
        (1, 0, 900),
        (899, 0, 900),
        (900, 900, 1800),
        (901, 900, 1800),
        (1799, 900, 1800),
        (1800, 1800, 2700),
    ];

    for (ts_secs, expected_start, expected_end) in test_cases {
        let visible_ts = VisibleNanos(ts_secs * NS_PER_SEC);
        assert_eq!(
            visible_ts.window_start().0,
            expected_start * NS_PER_SEC,
            "window_start failed for ts={}s",
            ts_secs
        );
        assert_eq!(
            visible_ts.window_end().0,
            expected_end * NS_PER_SEC,
            "window_end failed for ts={}s",
            ts_secs
        );
    }
}

#[test]
fn test_window_remaining_secs() {
    // At window start
    let window_start = VisibleNanos(900 * NS_PER_SEC);
    assert!((window_start.remaining_secs() - 900.0).abs() < 0.001);

    // Halfway through
    let halfway = VisibleNanos(1350 * NS_PER_SEC);
    assert!((halfway.remaining_secs() - 450.0).abs() < 0.001);

    // Just before end
    let just_before = VisibleNanos(1799 * NS_PER_SEC + 500 * NS_PER_MS);
    assert!(just_before.remaining_secs() < 1.0);
    assert!(just_before.remaining_secs() > 0.0);

    // At window end
    let window_end = VisibleNanos(1800 * NS_PER_SEC);
    assert!(window_end.remaining_secs() < 0.001); // Should be ~0, in next window
}

#[test]
fn test_window_15m_p_start_tracking() {
    let mut window = Window15M::for_visible_time(VisibleNanos(900 * NS_PER_SEC));

    assert!(window.p_start.is_none());
    assert!(window.p_now.is_none());

    // First price update at exactly window start
    window.update_price(VisibleNanos(900 * NS_PER_SEC), 50000.0);
    assert_eq!(window.p_start, Some(50000.0));
    assert_eq!(window.p_now, Some(50000.0));

    // Second price update - P_start should not change
    window.update_price(VisibleNanos(905 * NS_PER_SEC), 50100.0);
    assert_eq!(window.p_start, Some(50000.0)); // Unchanged
    assert_eq!(window.p_now, Some(50100.0)); // Updated

    // Third price update
    window.update_price(VisibleNanos(910 * NS_PER_SEC), 49900.0);
    assert_eq!(window.p_start, Some(50000.0)); // Still unchanged
    assert_eq!(window.p_now, Some(49900.0));
}

#[test]
fn test_window_15m_carry_forward() {
    // First window ends, we had a price
    let mut window1 = Window15M::for_visible_time(VisibleNanos(900 * NS_PER_SEC));
    window1.update_price(VisibleNanos(1700 * NS_PER_SEC), 50000.0);

    // Second window starts, no price yet
    let mut window2 = Window15M::for_visible_time(VisibleNanos(1800 * NS_PER_SEC));
    assert!(window2.p_start.is_none());

    // Carry forward from previous window
    window2.carry_forward_p_start(
        window1.p_now.unwrap(),
        window1.p_now_visible_ts.unwrap(),
    );
    assert_eq!(window2.p_start, Some(50000.0));
    assert_eq!(window2.p_start_visible_ts, Some(VisibleNanos(1700 * NS_PER_SEC)));

    // New price arrives but P_start already set (from carry-forward)
    window2.update_price(VisibleNanos(1805 * NS_PER_SEC), 50050.0);
    assert_eq!(window2.p_start, Some(50000.0)); // Still the carried value
    assert_eq!(window2.p_now, Some(50050.0));
}

#[test]
fn test_window_15m_contains() {
    let window = Window15M::for_visible_time(VisibleNanos(1000 * NS_PER_SEC));

    // Window is [900, 1800)
    assert!(!window.contains(VisibleNanos(899 * NS_PER_SEC)));
    assert!(window.contains(VisibleNanos(900 * NS_PER_SEC)));
    assert!(window.contains(VisibleNanos(1000 * NS_PER_SEC)));
    assert!(window.contains(VisibleNanos(1799 * NS_PER_SEC)));
    assert!(!window.contains(VisibleNanos(1800 * NS_PER_SEC)));
}

#[test]
fn test_window_15m_has_ended() {
    let window = Window15M::for_visible_time(VisibleNanos(1000 * NS_PER_SEC));

    assert!(!window.has_ended(VisibleNanos(1799 * NS_PER_SEC)));
    assert!(window.has_ended(VisibleNanos(1800 * NS_PER_SEC)));
    assert!(window.has_ended(VisibleNanos(2000 * NS_PER_SEC)));
}

// =============================================================================
// INVARIANT TESTS
// =============================================================================

#[test]
fn test_no_negative_delay_invariant() {
    // Valid: visible_ts >= ingest_ts
    let valid = EventTime::with_all(Some(100), 100, VisibleNanos(150));
    assert!(check_no_negative_delay(&valid).is_ok());

    // Valid: visible_ts == ingest_ts (zero delay)
    let zero_delay = EventTime::with_all(Some(100), 100, VisibleNanos(100));
    assert!(check_no_negative_delay(&zero_delay).is_ok());

    // Invalid: visible_ts < ingest_ts (negative delay)
    let negative = EventTime::with_all(Some(100), 100, VisibleNanos(50));
    assert!(check_no_negative_delay(&negative).is_err());
}

#[test]
fn test_visible_monotone_invariant() {
    assert!(check_visible_monotone(None, VisibleNanos(100)).is_ok());
    assert!(check_visible_monotone(Some(VisibleNanos(100)), VisibleNanos(100)).is_ok());
    assert!(check_visible_monotone(Some(VisibleNanos(100)), VisibleNanos(200)).is_ok());
    assert!(check_visible_monotone(Some(VisibleNanos(200)), VisibleNanos(100)).is_err());
}

// =============================================================================
// INGEST TIMESTAMP QUALITY TESTS
// =============================================================================

#[test]
fn test_ingest_timestamp_quality_classification() {
    assert!(IngestTimestampQuality::TrueNanosecond.is_hft_grade());
    assert!(!IngestTimestampQuality::Millisecond.is_hft_grade());
    assert!(!IngestTimestampQuality::SyntheticFromExchange.is_hft_grade());
    assert!(!IngestTimestampQuality::Missing.is_hft_grade());

    assert!(IngestTimestampQuality::TrueNanosecond.is_15m_acceptable());
    assert!(IngestTimestampQuality::Millisecond.is_15m_acceptable());
    assert!(!IngestTimestampQuality::SyntheticFromExchange.is_15m_acceptable());
    assert!(!IngestTimestampQuality::Missing.is_15m_acceptable());

    assert!(!IngestTimestampQuality::TrueNanosecond.reject_for_production());
    assert!(!IngestTimestampQuality::Millisecond.reject_for_production());
    assert!(IngestTimestampQuality::SyntheticFromExchange.reject_for_production());
    assert!(IngestTimestampQuality::Missing.reject_for_production());
}

// =============================================================================
// FEED EVENT ORDERING TESTS
// =============================================================================

#[test]
fn test_feed_event_complete_ordering() {
    // Test the full ordering: (visible_ts, priority, source, dataset_seq)
    let events = vec![
        // Different visible_ts
        (VisibleNanos(200), FeedEventPriority::ReferencePrice, FeedSource::Binance, 1u64),
        (VisibleNanos(100), FeedEventPriority::ReferencePrice, FeedSource::Binance, 2),
        // Same visible_ts, different priority
        (VisibleNanos(100), FeedEventPriority::BookDelta, FeedSource::Binance, 3),
        // Same visible_ts and priority, different source
        (VisibleNanos(100), FeedEventPriority::ReferencePrice, FeedSource::PolymarketBook, 4),
        // Same everything except seq
        (VisibleNanos(100), FeedEventPriority::ReferencePrice, FeedSource::Binance, 5),
    ];

    let mut feed_events: Vec<FeedEvent> = events
        .iter()
        .map(|(visible_ts, priority, source, seq)| {
            FeedEvent {
                time: EventTime::with_all(Some(100), 100, *visible_ts),
                source: *source,
                priority: *priority,
                dataset_seq: *seq,
                payload: FeedEventPayload::Timer {
                    timer_id: *seq,
                    payload: None,
                },
            }
        })
        .collect();

    feed_events.sort();

    // Expected order:
    // 1. visible_ts=100, priority=ReferencePrice, source=Binance, seq=2
    // 2. visible_ts=100, priority=ReferencePrice, source=Binance, seq=5
    // 3. visible_ts=100, priority=ReferencePrice, source=PolymarketBook, seq=4
    // 4. visible_ts=100, priority=BookDelta, source=Binance, seq=3
    // 5. visible_ts=200, priority=ReferencePrice, source=Binance, seq=1

    assert_eq!(feed_events[0].dataset_seq, 2);
    assert_eq!(feed_events[1].dataset_seq, 5);
    assert_eq!(feed_events[2].dataset_seq, 4);
    assert_eq!(feed_events[3].dataset_seq, 3);
    assert_eq!(feed_events[4].dataset_seq, 1);
}

// =============================================================================
// VISIBLE TIME CONTEXT TESTS
// =============================================================================

#[test]
fn test_visible_time_context_provides_only_visible_time() {
    let ctx = VisibleTimeContext::new(VisibleNanos(1000 * NS_PER_SEC));

    // Can access visible time
    assert_eq!(ctx.now_ns(), 1000 * NS_PER_SEC);
    assert_eq!(ctx.visible_ts().0, 1000 * NS_PER_SEC);

    // Can access derived window information
    assert_eq!(ctx.window_start().0, 900 * NS_PER_SEC);
    assert_eq!(ctx.window_end().0, 1800 * NS_PER_SEC);
    assert!((ctx.remaining_secs() - 800.0).abs() < 1.0);

    // Can check visibility
    assert!(ctx.is_visible(VisibleNanos(999 * NS_PER_SEC)));
    assert!(ctx.is_visible(VisibleNanos(1000 * NS_PER_SEC)));
    assert!(!ctx.is_visible(VisibleNanos(1001 * NS_PER_SEC)));

    // CANNOT access exchange_ts or ingest_ts (they don't exist in VisibleTimeContext)
}

// =============================================================================
// INTEGRATION TEST: FULL BACKTEST FLOW
// =============================================================================

#[test]
fn test_integration_backtest_flow() {
    // Simulate a mini-backtest flow with the event time model

    // 1. Create latency model
    let model = BacktestLatencyModel::default();

    // 2. Create event queue
    let mut queue = UnifiedFeedQueue::new(model, true);

    // 3. Simulate Binance price updates
    let binance_ingest_times = vec![
        900 * NS_PER_SEC + 50 * NS_PER_MS,  // Just after window start
        905 * NS_PER_SEC,
        910 * NS_PER_SEC,
    ];

    for (i, ingest_ts) in binance_ingest_times.iter().enumerate() {
        queue.push(
            Some(*ingest_ts),
            *ingest_ts,
            FeedSource::Binance,
            FeedEventPriority::ReferencePrice,
            FeedEventPayload::BinanceMidPriceUpdate {
                symbol: "BTC".into(),
                mid_price: 50000.0 + (i as f64) * 10.0,
                bid: 49999.0,
                ask: 50001.0,
            },
        );
    }

    // 4. Simulate Polymarket book updates
    let pm_ingest_times = vec![
        900 * NS_PER_SEC + 100 * NS_PER_MS,
        906 * NS_PER_SEC,
    ];

    for (i, ingest_ts) in pm_ingest_times.iter().enumerate() {
        queue.push(
            Some(*ingest_ts),
            *ingest_ts,
            FeedSource::PolymarketBook,
            FeedEventPriority::BookDelta,
            FeedEventPayload::PolymarketBookDelta {
                token_id: "token1".into(),
                market_slug: "btc-updown-15m-900".into(),
                side: BookSide::Bid,
                price: 0.5,
                new_size: 100.0 + (i as f64) * 50.0,
                exchange_seq: i as u64 + 1,
            },
        );
    }

    // 5. Process events and track 15M window state
    let mut window = Window15M::for_visible_time(VisibleNanos(900 * NS_PER_SEC));
    let mut events_processed = 0;

    while let Some(event) = queue.pop() {
        events_processed += 1;

        // Strategy only sees visible_ts through VisibleTimeContext
        let ctx = VisibleTimeContext::new(event.time.visible_ts);

        // Track Binance prices for 15M window
        if let FeedEventPayload::BinanceMidPriceUpdate { mid_price, .. } = &event.payload {
            window.update_price(ctx.visible_ts(), *mid_price);
        }

        // Verify strategy cannot see exchange or ingest timestamps
        let _view = StrategyEventView::from_event(&event);
        // view.time.exchange_ts - would not compile (field doesn't exist)
        // view.time.ingest_ts - would not compile (field doesn't exist)
    }

    // 6. Verify results
    assert_eq!(events_processed, 5);
    assert_eq!(window.p_start, Some(50000.0)); // First Binance price in window
    assert_eq!(window.p_now, Some(50020.0)); // Last Binance price
}

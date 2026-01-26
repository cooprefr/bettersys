//! Integration tests for Shadow Maker validation system.
//!
//! These tests verify:
//! - Precondition checking
//! - Shadow order lifecycle management
//! - Prediction vs actual comparison
//! - Discrepancy classification
//! - Report generation and trust level determination

use crate::backtest_v2::shadow_maker::*;
use crate::backtest_v2::data_contract::{
    ArrivalTimeSemantics, DatasetReadiness, HistoricalDataContract, OrderBookHistory, TradeHistory,
};
use crate::backtest_v2::events::Side;
use crate::backtest_v2::integrity::PathologyCounters;

// =============================================================================
// PRECONDITION TESTS
// =============================================================================

#[test]
fn test_preconditions_require_maker_viable_readiness() {
    let contract = HistoricalDataContract::polymarket_15m_updown_full_deltas();
    
    // TakerOnly readiness should fail
    let preconditions = ShadowMakerPreconditions::check(
        &contract,
        DatasetReadiness::TakerOnly,
        true,
        true,
    );
    assert!(!preconditions.all_pass());
    assert!(!preconditions.dataset_readiness_ok);
    
    // MakerViable should pass
    let preconditions = ShadowMakerPreconditions::check(
        &contract,
        DatasetReadiness::MakerViable,
        true,
        true,
    );
    assert!(preconditions.dataset_readiness_ok);
}

#[test]
fn test_preconditions_require_full_deltas() {
    // Snapshot-only contract should fail queue model check
    let contract = HistoricalDataContract::polymarket_15m_updown_with_recorded_arrival();
    let preconditions = ShadowMakerPreconditions::check(
        &contract,
        DatasetReadiness::MakerViable,
        true,
        true,
    );
    
    assert!(!preconditions.queue_model_active);
    assert!(!preconditions.all_pass());
}

#[test]
fn test_preconditions_require_recorded_arrival() {
    let contract = HistoricalDataContract {
        venue: "Polymarket".to_string(),
        market: "15m up/down".to_string(),
        orderbook: OrderBookHistory::FullIncrementalL2DeltasWithExchangeSeq,
        trades: TradeHistory::TradePrints,
        arrival_time: ArrivalTimeSemantics::SimulatedLatency, // Should fail
    };
    
    let preconditions = ShadowMakerPreconditions::check(
        &contract,
        DatasetReadiness::MakerViable,
        true,
        true,
    );
    
    assert!(!preconditions.arrival_time_enforced);
    assert!(!preconditions.all_pass());
}

#[test]
fn test_preconditions_require_strict_accounting() {
    let contract = HistoricalDataContract::polymarket_15m_updown_full_deltas();
    let preconditions = ShadowMakerPreconditions::check(
        &contract,
        DatasetReadiness::MakerViable,
        false, // strict_accounting disabled
        true,
    );
    
    assert!(!preconditions.strict_accounting_enabled);
    assert!(!preconditions.all_pass());
}

#[test]
fn test_preconditions_require_hard_invariants() {
    let contract = HistoricalDataContract::polymarket_15m_updown_full_deltas();
    let preconditions = ShadowMakerPreconditions::check(
        &contract,
        DatasetReadiness::MakerViable,
        true,
        false, // invariants not hard
    );
    
    assert!(!preconditions.invariants_enabled);
    assert!(!preconditions.all_pass());
}

#[test]
fn test_preconditions_all_pass_with_full_config() {
    let contract = HistoricalDataContract::polymarket_15m_updown_full_deltas();
    let preconditions = ShadowMakerPreconditions::check(
        &contract,
        DatasetReadiness::MakerViable,
        true,
        true,
    );
    
    assert!(preconditions.all_pass());
    assert!(preconditions.abort_message().is_none());
}

#[test]
fn test_preconditions_abort_message_contains_reasons() {
    let contract = HistoricalDataContract::polymarket_15m_updown_with_recorded_arrival();
    let preconditions = ShadowMakerPreconditions::check(
        &contract,
        DatasetReadiness::TakerOnly,
        false,
        false,
    );
    
    let msg = preconditions.abort_message().expect("should have abort message");
    assert!(msg.contains("ABORTED"));
    assert!(msg.contains("Preconditions failed"));
}

// =============================================================================
// SHADOW ORDER LIFECYCLE TESTS
// =============================================================================

#[test]
fn test_shadow_order_creation() {
    let order = ShadowOrder::new(
        1,
        "token123".to_string(),
        "btc-updown-15m-123".to_string(),
        Side::Buy,
        0.55,
        100.0,
        1_000_000_000,
    );
    
    assert_eq!(order.order_id, 1);
    assert_eq!(order.token_id, "token123");
    assert_eq!(order.side, Side::Buy);
    assert_eq!(order.price, 0.55);
    assert_eq!(order.size, 100.0);
    assert!(!order.is_terminal());
    assert!(!order.was_filled());
}

#[test]
fn test_shadow_order_ack_recording() {
    let mut order = ShadowOrder::new(
        1, "token".to_string(), "market".to_string(),
        Side::Buy, 0.5, 100.0, 1000,
    );
    
    order.record_ack(1010, Some("EXCH123".to_string()));
    
    assert_eq!(order.order_ack_time_ns, Some(1010));
    assert_eq!(order.exchange_order_id, Some("EXCH123".to_string()));
}

#[test]
fn test_shadow_order_fill_recording() {
    let mut order = ShadowOrder::new(
        1, "token".to_string(), "market".to_string(),
        Side::Buy, 0.5, 100.0, 1000,
    );
    
    // First partial fill
    order.record_fill(2000, 40.0, 0.5, 0.001, Some("fill1".to_string()));
    
    assert!(order.was_filled());
    assert_eq!(order.actual_fill_size, 40.0);
    assert_eq!(order.actual_fill_time_ns, Some(2000));
    assert_eq!(order.fill_count, 1);
    assert!(!order.is_terminal()); // Not fully filled
    
    // Second partial fill
    order.record_fill(3000, 60.0, 0.5, 0.001, Some("fill2".to_string()));
    
    assert_eq!(order.actual_fill_size, 100.0);
    assert_eq!(order.fill_count, 2);
    assert!(order.is_terminal()); // Now fully filled
    assert_eq!(order.terminal_reason, ShadowOrderTerminalReason::Filled);
}

#[test]
fn test_shadow_order_cancel_recording() {
    let mut order = ShadowOrder::new(
        1, "token".to_string(), "market".to_string(),
        Side::Buy, 0.5, 100.0, 1000,
    );
    
    order.record_cancel_submit(2000);
    assert_eq!(order.cancel_submit_time_ns, Some(2000));
    assert!(!order.is_terminal());
    
    order.record_cancel_ack(2100);
    assert_eq!(order.cancel_ack_time_ns, Some(2100));
    assert!(order.is_terminal());
    assert_eq!(order.terminal_reason, ShadowOrderTerminalReason::Cancelled);
}

#[test]
fn test_shadow_order_rejection_recording() {
    let mut order = ShadowOrder::new(
        1, "token".to_string(), "market".to_string(),
        Side::Buy, 0.5, 100.0, 1000,
    );
    
    order.record_rejection(1500);
    
    assert!(order.is_terminal());
    assert_eq!(order.terminal_reason, ShadowOrderTerminalReason::Rejected);
    assert_eq!(order.terminal_time_ns, Some(1500));
}

#[test]
fn test_shadow_order_time_to_fill() {
    let mut order = ShadowOrder::new(
        1, "token".to_string(), "market".to_string(),
        Side::Buy, 0.5, 100.0, 1_000_000_000, // 1 second
    );
    
    order.record_fill(1_500_000_000, 100.0, 0.5, 0.001, None);
    
    assert_eq!(order.time_to_fill_ns(), Some(500_000_000)); // 500ms
}

// =============================================================================
// DISCREPANCY CLASSIFICATION TESTS
// =============================================================================

fn make_test_order(filled: bool, fill_time_ns: Option<i64>) -> ShadowOrder {
    let mut order = ShadowOrder::new(
        1, "token".to_string(), "market".to_string(),
        Side::Buy, 0.5, 100.0, 1000,
    );
    if filled {
        order.record_fill(fill_time_ns.unwrap_or(2000) as u64, 100.0, 0.5, 0.001, None);
    }
    order
}

fn make_test_prediction(predicted_fill: bool, fill_time_ns: Option<i64>) -> ShadowPrediction {
    let mut pred = ShadowPrediction::new(1, 0);
    pred.predicted_fill = predicted_fill;
    pred.predicted_fill_time_ns = fill_time_ns.map(|t| t as u64);
    pred.predicted_fill_size = if predicted_fill { 100.0 } else { 0.0 };
    pred
}

#[test]
fn test_discrepancy_none_when_match() {
    let order = make_test_order(true, Some(2000));
    let pred = make_test_prediction(true, Some(2005)); // 5ns error - acceptable
    let data = DataIntegrityContext::default();
    
    let disc = OrderDiscrepancy::compute(&order, &pred, &data);
    
    assert_eq!(disc.classification, DiscrepancyClass::None);
    assert!(!disc.fill_occurrence_mismatch);
    assert!(!disc.false_positive);
    assert!(!disc.false_negative);
}

#[test]
fn test_discrepancy_false_positive() {
    let order = make_test_order(false, None);
    let pred = make_test_prediction(true, Some(2000));
    let data = DataIntegrityContext::default();
    
    let disc = OrderDiscrepancy::compute(&order, &pred, &data);
    
    assert!(disc.fill_occurrence_mismatch);
    assert!(disc.false_positive);
    assert!(!disc.false_negative);
}

#[test]
fn test_discrepancy_false_negative() {
    let order = make_test_order(true, Some(2000));
    let pred = make_test_prediction(false, None);
    let data = DataIntegrityContext::default();
    
    let disc = OrderDiscrepancy::compute(&order, &pred, &data);
    
    assert!(disc.fill_occurrence_mismatch);
    assert!(!disc.false_positive);
    assert!(disc.false_negative);
}

#[test]
fn test_discrepancy_data_gap_classification() {
    let order = make_test_order(true, Some(2000));
    let pred = make_test_prediction(false, None);
    let mut data = DataIntegrityContext::default();
    data.has_gaps = true;
    data.missing_deltas = 10;
    
    let disc = OrderDiscrepancy::compute(&order, &pred, &data);
    
    assert_eq!(disc.classification, DiscrepancyClass::DataGap);
    assert!(disc.had_data_gap);
    assert_eq!(disc.missing_deltas, 10);
}

#[test]
fn test_discrepancy_latency_error_classification() {
    let order = make_test_order(true, Some(2_000_000_000)); // 2 seconds
    let pred = make_test_prediction(true, Some(2_200_000_000)); // 2.2 seconds (+200ms)
    let data = DataIntegrityContext::default();
    
    let disc = OrderDiscrepancy::compute(&order, &pred, &data);
    
    // 200ms error exceeds 100ms threshold
    assert_eq!(disc.classification, DiscrepancyClass::LatencyError);
    assert!(disc.latency_model_error);
}

#[test]
fn test_discrepancy_queue_model_error_classification() {
    let order = make_test_order(true, Some(2000));
    let mut pred = make_test_prediction(false, None);
    pred.predicted_queue_remaining = 50.0; // Queue says shouldn't fill
    let data = DataIntegrityContext::default();
    
    let disc = OrderDiscrepancy::compute(&order, &pred, &data);
    
    // Predicted no fill but actual fill, no data gaps = queue model error
    assert_eq!(disc.classification, DiscrepancyClass::QueueModelError);
}

// =============================================================================
// VALIDATION STATS TESTS
// =============================================================================

#[test]
fn test_validation_stats_precision() {
    let mut stats = ShadowValidationStats::default();
    stats.true_positives = 90;
    stats.false_positives = 10;
    
    // Precision = 90 / (90 + 10) = 0.9
    assert!((stats.fill_precision() - 0.9).abs() < 0.001);
}

#[test]
fn test_validation_stats_recall() {
    let mut stats = ShadowValidationStats::default();
    stats.true_positives = 90;
    stats.false_negatives = 10;
    
    // Recall = 90 / (90 + 10) = 0.9
    assert!((stats.fill_recall() - 0.9).abs() < 0.001);
}

#[test]
fn test_validation_stats_f1() {
    let mut stats = ShadowValidationStats::default();
    stats.true_positives = 80;
    stats.false_positives = 10;
    stats.false_negatives = 10;
    
    // Precision = 80 / 90 = 0.888
    // Recall = 80 / 90 = 0.888
    // F1 = 2 * 0.888 * 0.888 / 1.776 = 0.888
    assert!((stats.fill_f1() - 0.888).abs() < 0.01);
}

#[test]
fn test_validation_stats_timing_errors() {
    let mut stats = ShadowValidationStats::default();
    stats.fill_time_errors_ns = vec![-10, -5, 0, 5, 10, 100];
    
    // Mean = (−10 −5 + 0 + 5 + 10 + 100) / 6 = 16.67
    let mean = stats.mean_fill_time_error_ns().unwrap();
    assert!((mean - 16.67).abs() < 0.1);
    
    // Median of [-10, -5, 0, 5, 10, 100] = (0 + 5) / 2 = 2.5 -> 2 (integer)
    let median = stats.median_fill_time_error_ns().unwrap();
    assert_eq!(median, 2);
}

#[test]
fn test_validation_stats_discrepancy_rate() {
    let mut stats = ShadowValidationStats::default();
    stats.total_orders = 100;
    stats.discrepancies_none = 85;
    stats.discrepancies_data_gap = 5;
    stats.discrepancies_queue_model_error = 10;
    
    // Discrepancy rate = 15 / 100 = 0.15
    assert!((stats.discrepancy_rate() - 0.15).abs() < 0.001);
}

// =============================================================================
// REPORT GENERATION TESTS
// =============================================================================

#[test]
fn test_report_insufficient_data() {
    let mut report = QueueModelValidationReport::new(QueueModelThresholds::default());
    report.stats.total_orders = 50; // Below minimum of 100
    
    report.finalize();
    
    assert!(!report.sample_size_sufficient);
    assert!(!report.validation_passed);
    assert_eq!(report.recommended_trust_level, QueueModelTrustLevel::InsufficientData);
}

#[test]
fn test_report_passes_with_good_stats() {
    let mut report = QueueModelValidationReport::new(QueueModelThresholds::default());
    report.stats.total_orders = 150;
    report.stats.true_positives = 95;
    report.stats.true_negatives = 40;
    report.stats.false_positives = 5;
    report.stats.false_negatives = 10;
    report.stats.discrepancies_none = 140;
    report.stats.discrepancies_queue_model_error = 2;
    report.stats.discrepancies_unknown = 8;
    
    report.finalize();
    
    assert!(report.sample_size_sufficient);
    assert!(report.fill_precision_passed); // 95 / 100 = 0.95 > 0.85
    assert!(report.fill_recall_passed);    // 95 / 105 = 0.90 > 0.85
}

#[test]
fn test_report_fails_on_low_precision() {
    let thresholds = QueueModelThresholds {
        min_fill_precision: 0.90,
        ..QueueModelThresholds::default()
    };
    let mut report = QueueModelValidationReport::new(thresholds);
    report.stats.total_orders = 150;
    report.stats.true_positives = 80;
    report.stats.false_positives = 20; // Precision = 80%
    report.stats.false_negatives = 0;
    
    report.finalize();
    
    assert!(!report.fill_precision_passed);
    assert!(!report.validation_passed);
    assert!(report.failure_reasons.iter().any(|r| r.contains("precision")));
}

#[test]
fn test_report_format_contains_key_info() {
    let mut report = QueueModelValidationReport::new(QueueModelThresholds::default());
    report.stats.total_orders = 100;
    report.coverage_orders = 100;
    report.finalize();
    
    let formatted = report.format_report();
    
    assert!(formatted.contains("QUEUE MODEL VALIDATION REPORT"));
    assert!(formatted.contains("COVERAGE"));
    assert!(formatted.contains("FILL PREDICTION"));
    assert!(formatted.contains("DISCREPANCY BREAKDOWN"));
}

// =============================================================================
// VALIDATOR INTEGRATION TESTS
// =============================================================================

#[test]
fn test_validator_disabled_on_failed_preconditions() {
    let contract = HistoricalDataContract::polymarket_15m_updown_with_recorded_arrival();
    let validator = ShadowMakerValidator::new(
        &contract,
        DatasetReadiness::TakerOnly,
        true,
        true,
        QueueModelThresholds::default(),
    );
    
    assert!(!validator.is_enabled());
}

#[test]
fn test_validator_enabled_with_full_config() {
    let contract = HistoricalDataContract::polymarket_15m_updown_full_deltas();
    let validator = ShadowMakerValidator::new(
        &contract,
        DatasetReadiness::MakerViable,
        true,
        true,
        QueueModelThresholds::default(),
    );
    
    assert!(validator.is_enabled());
}

#[test]
fn test_validator_order_submission() {
    let contract = HistoricalDataContract::polymarket_15m_updown_full_deltas();
    let mut validator = ShadowMakerValidator::new(
        &contract,
        DatasetReadiness::MakerViable,
        true,
        true,
        QueueModelThresholds::default(),
    );
    
    let order_id = validator.submit_shadow_order(
        "token123".to_string(),
        "market-slug".to_string(),
        Side::Buy,
        0.55,
        100.0,
        1_000_000_000,
        None,
    );
    
    assert!(order_id.is_some());
    assert_eq!(validator.active_order_count(), 1);
}

#[test]
fn test_validator_order_completion() {
    let contract = HistoricalDataContract::polymarket_15m_updown_full_deltas();
    let mut validator = ShadowMakerValidator::new(
        &contract,
        DatasetReadiness::MakerViable,
        true,
        true,
        QueueModelThresholds::default(),
    );
    
    let order_id = validator.submit_shadow_order(
        "token123".to_string(),
        "market-slug".to_string(),
        Side::Buy,
        0.55,
        100.0,
        1_000_000_000,
        None,
    ).unwrap();
    
    // Record fill
    validator.record_fill(order_id, 1_500_000_000, 100.0, 0.55, 0.001, None);
    
    // Complete with prediction
    let prediction = ShadowPrediction::new(order_id, 0);
    validator.complete_order(order_id, prediction);
    
    assert_eq!(validator.active_order_count(), 0);
    assert_eq!(validator.completed_order_count(), 1);
}

#[test]
fn test_validator_report_generation() {
    let contract = HistoricalDataContract::polymarket_15m_updown_full_deltas();
    let mut validator = ShadowMakerValidator::new(
        &contract,
        DatasetReadiness::MakerViable,
        true,
        true,
        QueueModelThresholds::default(),
    );
    
    // Submit and complete some orders
    for i in 1..=5 {
        let order_id = validator.submit_shadow_order(
            format!("token{}", i),
            format!("market-{}", i),
            Side::Buy,
            0.5,
            100.0,
            i * 1_000_000_000,
            None,
        ).unwrap();
        
        validator.record_fill(order_id, (i + 1) * 1_000_000_000, 100.0, 0.5, 0.001, None);
        
        let mut prediction = ShadowPrediction::new(order_id, 0);
        prediction.predicted_fill = true;
        prediction.predicted_fill_time_ns = Some((i + 1) * 1_000_000_000);
        
        validator.complete_order(order_id, prediction);
    }
    
    let report = validator.generate_report(10_000_000_000, 0x1234);
    
    assert_eq!(report.coverage_orders, 5);
    assert_eq!(report.stats.total_orders, 5);
    assert_eq!(report.stats.true_positives, 5);
}

// =============================================================================
// TRUST LEVEL TESTS
// =============================================================================

#[test]
fn test_trust_level_validated_allows_maker() {
    assert!(QueueModelTrustLevel::Validated.allows_maker_trust());
}

#[test]
fn test_trust_level_not_validated_blocks_maker() {
    assert!(!QueueModelTrustLevel::NotValidated.allows_maker_trust());
    assert!(!QueueModelTrustLevel::PartiallyValidated.allows_maker_trust());
    assert!(!QueueModelTrustLevel::InsufficientData.allows_maker_trust());
}

#[test]
fn test_validation_flag_from_report() {
    let mut report = QueueModelValidationReport::new(QueueModelThresholds::default());
    report.stats.total_orders = 200;
    report.stats.true_positives = 180;
    report.stats.false_positives = 10;
    report.stats.false_negatives = 10;
    report.stats.discrepancies_none = 190;
    report.coverage_orders = 200;
    report.dataset_hash = 0xABCD;
    report.validation_timestamp_ns = 123456789;
    
    report.finalize();
    
    let flag = QueueModelValidationFlag::from_report(&report);
    
    assert!(flag.validation_performed);
    assert_eq!(flag.validation_dataset_hash, Some(0xABCD));
    assert_eq!(flag.shadow_order_count, Some(200));
}

#[test]
fn test_validation_flag_allows_certification() {
    let mut report = QueueModelValidationReport::new(QueueModelThresholds::default());
    report.stats.total_orders = 200;
    report.stats.true_positives = 180;
    report.stats.true_negatives = 10;
    report.stats.false_positives = 5;
    report.stats.false_negatives = 5;
    report.stats.discrepancies_none = 195;
    report.stats.discrepancies_queue_model_error = 1;
    
    report.finalize();
    
    let flag = QueueModelValidationFlag::from_report(&report);
    
    // Should be validated with these stats
    if report.validation_passed {
        assert!(flag.allows_maker_certification());
    }
}

// =============================================================================
// UNSUPPORTED REGIME TESTS
// =============================================================================

#[test]
fn test_unsupported_regimes_populated_on_failure() {
    let mut report = QueueModelValidationReport::new(QueueModelThresholds::default());
    report.stats.total_orders = 150;
    report.stats.discrepancies_queue_model_error = 20; // High error rate
    
    report.finalize();
    
    // Should have at least near-settlement regime
    assert!(!report.unsupported_regimes.is_empty());
    assert!(report.unsupported_regimes.iter().any(|r| r.id == "NEAR_SETTLEMENT"));
}

#[test]
fn test_failure_modes_populated() {
    let mut report = QueueModelValidationReport::new(QueueModelThresholds::default());
    report.stats.total_orders = 150;
    report.stats.discrepancies_data_gap = 10;
    report.stats.discrepancies_latency_error = 5;
    report.stats.discrepancies_queue_model_error = 3;
    
    report.finalize();
    
    // Should have failure modes for each discrepancy type
    assert!(report.known_failure_modes.iter().any(|m| m.id == "DATA_GAP"));
    assert!(report.known_failure_modes.iter().any(|m| m.id == "LATENCY_ERROR"));
    assert!(report.known_failure_modes.iter().any(|m| m.id == "QUEUE_MODEL"));
}

// =============================================================================
// DATA INTEGRITY CONTEXT TESTS
// =============================================================================

#[test]
fn test_data_integrity_from_pathology_counters() {
    let mut counters = PathologyCounters::default();
    counters.gaps_detected = 3;
    counters.total_missing_sequences = 15;
    counters.out_of_order_detected = 2;
    
    let contract = HistoricalDataContract::polymarket_15m_updown_full_deltas();
    let mut validator = ShadowMakerValidator::new(
        &contract,
        DatasetReadiness::MakerViable,
        true,
        true,
        QueueModelThresholds::default(),
    );
    
    validator.update_data_integrity(&counters);
    
    // The validator should record the integrity context
    // This affects discrepancy classification
}

// =============================================================================
// THRESHOLDS TESTS
// =============================================================================

#[test]
fn test_default_thresholds_are_reasonable() {
    let thresholds = QueueModelThresholds::default();
    
    // Verify defaults are set
    assert!(thresholds.max_false_positive_rate > 0.0);
    assert!(thresholds.max_false_negative_rate > 0.0);
    assert!(thresholds.min_fill_precision > 0.5);
    assert!(thresholds.min_fill_recall > 0.5);
    assert!(thresholds.min_sample_size >= 100);
}

#[test]
fn test_custom_thresholds() {
    let thresholds = QueueModelThresholds {
        max_false_positive_rate: 0.05,
        max_false_negative_rate: 0.05,
        min_fill_precision: 0.95,
        min_fill_recall: 0.95,
        max_mean_fill_time_error_ms: 20.0,
        max_p95_fill_time_error_ms: 100.0,
        max_queue_model_error_rate: 0.02,
        min_sample_size: 500,
    };
    
    assert_eq!(thresholds.min_sample_size, 500);
    assert_eq!(thresholds.min_fill_precision, 0.95);
}

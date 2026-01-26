//! Sensitivity Analysis Tests
//!
//! Tests for the sensitivity analysis framework, including:
//! - Deterministic sweep execution
//! - Fragility detection under known conditions
//! - Trust recommendation logic
//! - Sweep configuration generation

use crate::backtest_v2::latency::{LatencyConfig, LatencyDistribution, NS_PER_MS};
use crate::backtest_v2::orchestrator::MakerFillModel;
use crate::backtest_v2::sensitivity::*;

// =============================================================================
// LATENCY SWEEP TESTS
// =============================================================================

#[test]
fn test_latency_sweep_config_default_values() {
    let config = LatencySweepConfig::default();
    
    // Must include zero (baseline)
    assert!(config.values_ms.contains(&0.0), "Must include 0ms baseline");
    
    // Must include realistic values for Polymarket
    assert!(config.values_ms.iter().any(|&v| v >= 50.0 && v <= 100.0),
        "Must include 50-100ms range");
    
    // Must include high latency for stress testing
    assert!(config.values_ms.iter().any(|&v| v >= 500.0),
        "Must include 500ms+ for stress testing");
}

#[test]
fn test_latency_sweep_config_polymarket_standard() {
    let config = LatencySweepConfig::polymarket_standard();
    
    // Sub-second trading focus
    assert!(config.values_ms.iter().all(|&v| v <= 1000.0),
        "Polymarket standard should focus on sub-second latencies");
    
    assert!(config.values_ms.contains(&0.0), "Must include baseline");
    assert!(config.values_ms.contains(&50.0), "Must include 50ms");
}

#[test]
fn test_latency_sweep_config_hft_fine() {
    let config = LatencySweepConfig::hft_fine();
    
    // Fine granularity for HFT
    assert!(config.values_ms.len() >= 5, "HFT config needs fine granularity");
    assert!(config.values_ms.iter().any(|&v| v > 0.0 && v < 10.0),
        "HFT config needs sub-10ms values");
}

#[test]
fn test_latency_config_generation_fixed() {
    let sweep = LatencySweepConfig {
        component: LatencyComponent::EndToEnd,
        values_ms: vec![0.0, 100.0],
        use_fixed: true,
    };
    let base = LatencyConfig::default();
    
    // Generate config for 100ms
    let config = sweep.config_for_value(100.0, &base);
    
    // All components should be fixed at 100ms
    match &config.market_data {
        LatencyDistribution::Fixed { latency_ns } => {
            assert_eq!(*latency_ns, 100 * NS_PER_MS);
        }
        _ => panic!("Expected Fixed distribution for use_fixed=true"),
    }
    
    match &config.order_send {
        LatencyDistribution::Fixed { latency_ns } => {
            assert_eq!(*latency_ns, 100 * NS_PER_MS);
        }
        _ => panic!("Expected Fixed distribution"),
    }
}

#[test]
fn test_latency_config_generation_normal() {
    let sweep = LatencySweepConfig {
        component: LatencyComponent::EndToEnd,
        values_ms: vec![100.0],
        use_fixed: false,
    };
    let base = LatencyConfig::default();
    
    let config = sweep.config_for_value(100.0, &base);
    
    // Should use Normal distribution with 20% std dev
    match &config.market_data {
        LatencyDistribution::Normal { mean_ns, std_ns, .. } => {
            assert_eq!(*mean_ns, 100 * NS_PER_MS);
            assert_eq!(*std_ns, 20 * NS_PER_MS); // 20% of 100ms
        }
        _ => panic!("Expected Normal distribution for use_fixed=false"),
    }
}

#[test]
fn test_latency_config_generation_single_component() {
    let sweep = LatencySweepConfig {
        component: LatencyComponent::MarketData,
        values_ms: vec![50.0],
        use_fixed: true,
    };
    // Use a base config with fixed latencies for easier comparison
    let base = LatencyConfig {
        market_data: LatencyDistribution::Fixed { latency_ns: 100 * NS_PER_MS },
        decision: LatencyDistribution::Fixed { latency_ns: 10 * NS_PER_MS },
        order_send: LatencyDistribution::Fixed { latency_ns: 200 * NS_PER_MS },
        venue_process: LatencyDistribution::Fixed { latency_ns: 100 * NS_PER_MS },
        cancel_process: LatencyDistribution::Fixed { latency_ns: 150 * NS_PER_MS },
        fill_report: LatencyDistribution::Fixed { latency_ns: 100 * NS_PER_MS },
    };
    
    let config = sweep.config_for_value(50.0, &base);
    
    // Only market_data should be modified
    match &config.market_data {
        LatencyDistribution::Fixed { latency_ns } => {
            assert_eq!(*latency_ns, 50 * NS_PER_MS);
        }
        _ => panic!("Market data should be fixed at 50ms"),
    }
    
    // Other components should be unchanged from base
    match &config.order_send {
        LatencyDistribution::Fixed { latency_ns } => {
            assert_eq!(*latency_ns, 200 * NS_PER_MS, "Order send should be unchanged");
        }
        _ => panic!("Order send should remain Fixed from base config"),
    }
}

#[test]
fn test_latency_component_descriptions() {
    // All components should have non-empty descriptions
    let components = [
        LatencyComponent::EndToEnd,
        LatencyComponent::MarketData,
        LatencyComponent::Decision,
        LatencyComponent::OrderSend,
        LatencyComponent::VenueProcess,
        LatencyComponent::CancelProcess,
        LatencyComponent::FillReport,
    ];
    
    for component in &components {
        let desc = component.description();
        assert!(!desc.is_empty(), "Component {:?} needs description", component);
    }
}

// =============================================================================
// SAMPLING SWEEP TESTS
// =============================================================================

#[test]
fn test_sampling_sweep_config_default() {
    let config = SamplingSweepConfig::default();
    
    // Must include baseline (EveryUpdate)
    assert!(config.regimes.contains(&SamplingRegime::EveryUpdate),
        "Must include EveryUpdate baseline");
    
    // Must include degraded regimes
    assert!(config.regimes.iter().any(|r| matches!(r, SamplingRegime::Periodic { .. })),
        "Must include Periodic regimes");
}

#[test]
fn test_sampling_regime_delta_support() {
    assert!(SamplingRegime::EveryUpdate.supports_deltas());
    assert!(SamplingRegime::Periodic { interval_ms: 100 }.supports_deltas());
    assert!(!SamplingRegime::TopOfBookOnly.supports_deltas());
    assert!(!SamplingRegime::SnapshotOnly { interval_ms: 1000 }.supports_deltas());
}

#[test]
fn test_sampling_regime_descriptions() {
    let regimes = [
        SamplingRegime::EveryUpdate,
        SamplingRegime::Periodic { interval_ms: 100 },
        SamplingRegime::TopOfBookOnly,
        SamplingRegime::SnapshotOnly { interval_ms: 1000 },
    ];
    
    for regime in &regimes {
        let desc = regime.description();
        assert!(!desc.is_empty(), "Regime {:?} needs description", regime);
    }
}

// =============================================================================
// EXECUTION SWEEP TESTS
// =============================================================================

#[test]
fn test_execution_sweep_config_default() {
    let config = ExecutionSweepConfig::default();
    
    // Must include production-valid models
    assert!(config.queue_models.iter().any(|m| m.is_valid_for_production()),
        "Must include production-valid queue models");
    
    // Must include explicit FIFO
    assert!(config.queue_models.contains(&QueueModelAssumption::ExplicitFifo),
        "Must include ExplicitFifo baseline");
}

#[test]
fn test_queue_model_production_validity() {
    // Production-valid models
    assert!(QueueModelAssumption::ExplicitFifo.is_valid_for_production());
    assert!(QueueModelAssumption::Conservative { extra_ahead_pct: 25 }.is_valid_for_production());
    assert!(QueueModelAssumption::MakerDisabled.is_valid_for_production());
    
    // NOT production-valid
    assert!(!QueueModelAssumption::Optimistic.is_valid_for_production());
    // Note: PessimisticLevelClear may or may not be production-valid depending on use case
}

#[test]
fn test_queue_model_to_maker_fill_model() {
    assert_eq!(
        QueueModelAssumption::ExplicitFifo.to_maker_fill_model(),
        MakerFillModel::ExplicitQueue
    );
    assert_eq!(
        QueueModelAssumption::Optimistic.to_maker_fill_model(),
        MakerFillModel::Optimistic
    );
    assert_eq!(
        QueueModelAssumption::MakerDisabled.to_maker_fill_model(),
        MakerFillModel::MakerDisabled
    );
}

#[test]
fn test_cancel_latency_assumptions() {
    let assumptions = [
        CancelLatencyAssumption::Zero,
        CancelLatencyAssumption::Fixed { latency_ms: 50 },
        CancelLatencyAssumption::SameAsOrderSend,
    ];
    
    for assumption in &assumptions {
        let desc = assumption.description();
        assert!(!desc.is_empty(), "Assumption {:?} needs description", assumption);
    }
}

// =============================================================================
// FRAGILITY DETECTION TESTS
// =============================================================================

#[test]
fn test_fragility_detection_latency_degradation() {
    let detector = FragilityDetector::new(FragilityThresholds {
        latency_pnl_drop_pct: 50.0,
        latency_increase_ms: 50.0,
        ..Default::default()
    });
    
    // Create results where PnL drops 60% at 50ms (exceeds 50% threshold)
    let results = LatencySweepResults {
        config: LatencySweepConfig::default(),
        points: vec![
            SweepPointMetrics {
                parameter_value: 0.0,
                pnl_after_fees: 1000.0,
                ..Default::default()
            },
            SweepPointMetrics {
                parameter_value: 50.0,
                pnl_after_fees: 400.0, // 60% drop
                ..Default::default()
            },
        ],
        baseline_index: 0,
        profitability_flip_index: None,
        max_profitable_latency_ms: None,
    };
    
    let (fragile, reason) = detector.detect_latency_fragility(&results);
    assert!(fragile, "Should detect latency fragility");
    assert!(reason.is_some(), "Should provide reason");
    assert!(reason.unwrap().contains("60.0%"), "Reason should mention drop percentage");
}

#[test]
fn test_fragility_detection_profitability_flip() {
    // Use thresholds that won't trigger on PnL drop percentage
    let detector = FragilityDetector::new(FragilityThresholds {
        latency_pnl_drop_pct: 200.0, // Very high threshold so only profitability flip triggers
        latency_increase_ms: 50.0,
        ..Default::default()
    });
    
    // Create results where strategy flips from profitable to unprofitable
    // with a smaller percentage drop that won't trigger the pnl_drop threshold
    let results = LatencySweepResults {
        config: LatencySweepConfig::default(),
        points: vec![
            SweepPointMetrics {
                parameter_value: 0.0,
                pnl_after_fees: 100.0, // Profitable
                ..Default::default()
            },
            SweepPointMetrics {
                parameter_value: 50.0,
                pnl_after_fees: -50.0, // Unprofitable (150% drop, but threshold is 200%)
                ..Default::default()
            },
        ],
        baseline_index: 0,
        profitability_flip_index: None,
        max_profitable_latency_ms: None,
    };
    
    let (fragile, reason) = detector.detect_latency_fragility(&results);
    assert!(fragile, "Should detect profitability flip");
    assert!(reason.is_some());
    let reason_text = reason.unwrap();
    // When profitability flips, the reason should mention it
    assert!(reason_text.contains("profitable") || reason_text.contains("flips") || reason_text.contains("PnL"), 
        "Reason should indicate fragility, got: {}", reason_text);
}

#[test]
fn test_fragility_detection_no_fragility() {
    let detector = FragilityDetector::new(FragilityThresholds::default());
    
    // Create results where PnL drops only 20% (below 50% threshold)
    let results = LatencySweepResults {
        config: LatencySweepConfig::default(),
        points: vec![
            SweepPointMetrics {
                parameter_value: 0.0,
                pnl_after_fees: 1000.0,
                ..Default::default()
            },
            SweepPointMetrics {
                parameter_value: 50.0,
                pnl_after_fees: 800.0, // Only 20% drop
                ..Default::default()
            },
        ],
        baseline_index: 0,
        profitability_flip_index: None,
        max_profitable_latency_ms: None,
    };
    
    let (fragile, reason) = detector.detect_latency_fragility(&results);
    assert!(!fragile, "Should not detect fragility for small degradation");
    assert!(reason.is_none());
}

#[test]
fn test_fragility_detection_sampling() {
    let detector = FragilityDetector::new(FragilityThresholds {
        sampling_fill_rate_drop_pct: 30.0,
        ..Default::default()
    });
    
    // Create results where fill rate drops significantly
    let results = SamplingSweepResults {
        config: SamplingSweepConfig::default(),
        points: vec![
            (SamplingRegime::EveryUpdate, SweepPointMetrics {
                fill_rate: 0.8,
                pnl_after_fees: 1000.0,
                ..Default::default()
            }),
            (SamplingRegime::Periodic { interval_ms: 1000 }, SweepPointMetrics {
                fill_rate: 0.4, // 50% drop
                pnl_after_fees: 500.0,
                ..Default::default()
            }),
        ],
        baseline_regime: SamplingRegime::EveryUpdate,
    };
    
    let (fragile, reason) = detector.detect_sampling_fragility(&results);
    assert!(fragile, "Should detect sampling fragility");
    assert!(reason.is_some());
}

#[test]
fn test_fragility_detection_execution() {
    let detector = FragilityDetector::new(FragilityThresholds {
        execution_pnl_drop_pct: 75.0,
        ..Default::default()
    });
    
    // Create results where PnL collapses under conservative model
    let results = ExecutionSweepResults {
        config: ExecutionSweepConfig::default(),
        queue_model_results: vec![
            (QueueModelAssumption::ExplicitFifo, SweepPointMetrics {
                pnl_after_fees: 1000.0,
                maker_fills: 100,
                ..Default::default()
            }),
            (QueueModelAssumption::Conservative { extra_ahead_pct: 50 }, SweepPointMetrics {
                pnl_after_fees: 200.0, // 80% drop
                maker_fills: 20,
                ..Default::default()
            }),
        ],
        cancel_latency_results: vec![],
        baseline_queue_model: QueueModelAssumption::ExplicitFifo,
    };
    
    let (fragile, reason) = detector.detect_execution_fragility(&results);
    assert!(fragile, "Should detect execution fragility");
    assert!(reason.is_some());
}

#[test]
fn test_fragility_detection_ignores_optimistic() {
    let detector = FragilityDetector::new(FragilityThresholds::default());
    
    // Results should NOT be fragile just because optimistic mode has better PnL
    let results = ExecutionSweepResults {
        config: ExecutionSweepConfig::default(),
        queue_model_results: vec![
            (QueueModelAssumption::ExplicitFifo, SweepPointMetrics {
                pnl_after_fees: 500.0,
                ..Default::default()
            }),
            (QueueModelAssumption::Optimistic, SweepPointMetrics {
                pnl_after_fees: 2000.0, // Much higher - but this is expected
                ..Default::default()
            }),
        ],
        cancel_latency_results: vec![],
        baseline_queue_model: QueueModelAssumption::ExplicitFifo,
    };
    
    let (fragile, _) = detector.detect_execution_fragility(&results);
    assert!(!fragile, "Should not flag fragility based on optimistic mode comparison");
}

#[test]
fn test_fragility_flags_combined() {
    let detector = FragilityDetector::new(FragilityThresholds::default());
    
    // Create a fragile latency sweep
    let latency_results = LatencySweepResults {
        config: LatencySweepConfig::default(),
        points: vec![
            SweepPointMetrics {
                parameter_value: 0.0,
                pnl_after_fees: 1000.0,
                ..Default::default()
            },
            SweepPointMetrics {
                parameter_value: 50.0,
                pnl_after_fees: -100.0, // Flips to unprofitable
                ..Default::default()
            },
        ],
        baseline_index: 0,
        profitability_flip_index: None,
        max_profitable_latency_ms: None,
    };
    
    let flags = detector.detect_all(Some(&latency_results), None, None);
    
    assert!(flags.latency_fragile);
    assert!(!flags.sampling_fragile); // Not tested
    assert!(!flags.execution_fragile); // Not tested
    assert!(flags.is_fragile());
    assert!(!flags.is_trustworthy());
}

// =============================================================================
// TRUST RECOMMENDATION TESTS
// =============================================================================

#[test]
fn test_trust_recommendation_no_sensitivity() {
    let mut report = SensitivityReport::default();
    assert_eq!(report.trust_recommendation, TrustRecommendation::UntrustedNoSensitivity);
    
    // Even after computing, should remain untrusted without sensitivity
    report.compute_trust_recommendation();
    assert_eq!(report.trust_recommendation, TrustRecommendation::UntrustedNoSensitivity);
}

#[test]
fn test_trust_recommendation_trusted() {
    let mut report = SensitivityReport {
        sensitivity_run: true,
        fragility: FragilityFlags::default(), // No fragility
        ..Default::default()
    };
    
    report.compute_trust_recommendation();
    assert_eq!(report.trust_recommendation, TrustRecommendation::Trusted);
    assert!(report.trust_recommendation.is_trustworthy());
}

#[test]
fn test_trust_recommendation_caution_fragile() {
    let mut report = SensitivityReport {
        sensitivity_run: true,
        fragility: FragilityFlags {
            latency_fragile: true,
            latency_fragility_reason: Some("test".to_string()),
            ..Default::default()
        },
        ..Default::default()
    };
    
    report.compute_trust_recommendation();
    assert_eq!(report.trust_recommendation, TrustRecommendation::CautionFragile);
    assert!(!report.trust_recommendation.is_trustworthy());
}

#[test]
fn test_trust_recommendation_untrusted_optimistic() {
    let mut report = SensitivityReport {
        sensitivity_run: true,
        fragility: FragilityFlags {
            requires_optimistic_assumptions: true,
            ..Default::default()
        },
        ..Default::default()
    };
    
    report.compute_trust_recommendation();
    assert_eq!(report.trust_recommendation, TrustRecommendation::UntrustedOptimistic);
}

#[test]
fn test_trust_recommendation_optimistic_takes_precedence() {
    let mut report = SensitivityReport {
        sensitivity_run: true,
        fragility: FragilityFlags {
            latency_fragile: true,
            requires_optimistic_assumptions: true,
            ..Default::default()
        },
        ..Default::default()
    };
    
    report.compute_trust_recommendation();
    // Optimistic dependency is worse than fragility
    assert_eq!(report.trust_recommendation, TrustRecommendation::UntrustedOptimistic);
}

// =============================================================================
// SWEEP POINT METRICS TESTS
// =============================================================================

#[test]
fn test_sweep_point_metrics_pnl_change() {
    let baseline = SweepPointMetrics {
        pnl_after_fees: 1000.0,
        ..Default::default()
    };
    
    let better = SweepPointMetrics {
        pnl_after_fees: 1200.0,
        ..Default::default()
    };
    
    let worse = SweepPointMetrics {
        pnl_after_fees: 500.0,
        ..Default::default()
    };
    
    assert_eq!(better.pnl_change_from(&baseline), 20.0); // +20%
    assert_eq!(worse.pnl_change_from(&baseline), -50.0); // -50%
}

#[test]
fn test_sweep_point_metrics_fill_rate_change() {
    let baseline = SweepPointMetrics {
        fill_rate: 0.8,
        ..Default::default()
    };
    
    let degraded = SweepPointMetrics {
        fill_rate: 0.4,
        ..Default::default()
    };
    
    assert_eq!(degraded.fill_rate_change_from(&baseline), -50.0);
}

#[test]
fn test_sweep_point_metrics_zero_baseline() {
    let baseline = SweepPointMetrics {
        pnl_after_fees: 0.0,
        fill_rate: 0.0,
        ..Default::default()
    };
    
    let other = SweepPointMetrics {
        pnl_after_fees: 100.0,
        fill_rate: 0.5,
        ..Default::default()
    };
    
    // Should handle zero baseline gracefully
    assert_eq!(other.pnl_change_from(&baseline), 0.0);
    assert_eq!(other.fill_rate_change_from(&baseline), 0.0);
}

#[test]
fn test_sweep_point_metrics_is_profitable() {
    assert!(SweepPointMetrics { pnl_after_fees: 100.0, ..Default::default() }.is_profitable());
    assert!(SweepPointMetrics { pnl_after_fees: 0.01, ..Default::default() }.is_profitable());
    assert!(!SweepPointMetrics { pnl_after_fees: 0.0, ..Default::default() }.is_profitable());
    assert!(!SweepPointMetrics { pnl_after_fees: -100.0, ..Default::default() }.is_profitable());
}

// =============================================================================
// LATENCY SWEEP RESULTS TESTS
// =============================================================================

#[test]
fn test_latency_sweep_results_pnl_degradation() {
    let results = LatencySweepResults {
        config: LatencySweepConfig::default(),
        points: vec![
            SweepPointMetrics { parameter_value: 0.0, pnl_after_fees: 1000.0, ..Default::default() },
            SweepPointMetrics { parameter_value: 50.0, pnl_after_fees: 800.0, ..Default::default() },
            SweepPointMetrics { parameter_value: 100.0, pnl_after_fees: 500.0, ..Default::default() },
        ],
        baseline_index: 0,
        profitability_flip_index: None,
        max_profitable_latency_ms: None,
    };
    
    let degradation = results.pnl_degradation();
    assert_eq!(degradation.len(), 3);
    assert_eq!(degradation[0], 0.0); // Baseline
    assert_eq!(degradation[1], -20.0); // 800 vs 1000 = -20%
    assert_eq!(degradation[2], -50.0); // 500 vs 1000 = -50%
}

#[test]
fn test_latency_sweep_results_latency_at_pnl_drop() {
    let results = LatencySweepResults {
        config: LatencySweepConfig::default(),
        points: vec![
            SweepPointMetrics { parameter_value: 0.0, pnl_after_fees: 1000.0, ..Default::default() },
            SweepPointMetrics { parameter_value: 25.0, pnl_after_fees: 900.0, ..Default::default() },
            SweepPointMetrics { parameter_value: 50.0, pnl_after_fees: 700.0, ..Default::default() },
            SweepPointMetrics { parameter_value: 100.0, pnl_after_fees: 400.0, ..Default::default() },
        ],
        baseline_index: 0,
        profitability_flip_index: None,
        max_profitable_latency_ms: None,
    };
    
    // Find latency at 30% PnL drop
    let latency = results.latency_at_pnl_drop(30.0);
    assert_eq!(latency, Some(50.0)); // 700/1000 = 30% drop at 50ms
    
    // Find latency at 60% PnL drop
    let latency = results.latency_at_pnl_drop(60.0);
    assert_eq!(latency, Some(100.0)); // 400/1000 = 60% drop at 100ms
    
    // No drop reaches 90%
    let latency = results.latency_at_pnl_drop(90.0);
    assert_eq!(latency, None);
}

// =============================================================================
// EXECUTION SWEEP RESULTS TESTS
// =============================================================================

#[test]
fn test_execution_sweep_results_maker_pnl_collapses() {
    // Scenario 1: Maker PnL collapses under conservative model
    let results = ExecutionSweepResults {
        config: ExecutionSweepConfig::default(),
        queue_model_results: vec![
            (QueueModelAssumption::ExplicitFifo, SweepPointMetrics {
                pnl_after_fees: 1000.0,
                maker_fills: 100,
                ..Default::default()
            }),
            (QueueModelAssumption::Conservative { extra_ahead_pct: 50 }, SweepPointMetrics {
                pnl_after_fees: -50.0, // Unprofitable
                maker_fills: 0,
                ..Default::default()
            }),
        ],
        cancel_latency_results: vec![],
        baseline_queue_model: QueueModelAssumption::ExplicitFifo,
    };
    
    assert!(results.maker_pnl_collapses());
    
    // Scenario 2: Maker PnL does not collapse
    let results2 = ExecutionSweepResults {
        config: ExecutionSweepConfig::default(),
        queue_model_results: vec![
            (QueueModelAssumption::ExplicitFifo, SweepPointMetrics {
                pnl_after_fees: 1000.0,
                maker_fills: 100,
                ..Default::default()
            }),
            (QueueModelAssumption::Conservative { extra_ahead_pct: 50 }, SweepPointMetrics {
                pnl_after_fees: 700.0, // Still profitable
                maker_fills: 70,
                ..Default::default()
            }),
        ],
        cancel_latency_results: vec![],
        baseline_queue_model: QueueModelAssumption::ExplicitFifo,
    };
    
    assert!(!results2.maker_pnl_collapses());
}

// =============================================================================
// SENSITIVITY CONFIG TESTS
// =============================================================================

#[test]
fn test_sensitivity_config_default_disabled() {
    let config = SensitivityConfig::default();
    assert!(!config.enabled, "Sensitivity should be disabled by default");
    assert!(!config.strict_sensitivity, "Strict sensitivity should be disabled by default");
}

#[test]
fn test_sensitivity_config_production_validation() {
    let config = SensitivityConfig::production_validation();
    assert!(config.enabled);
    assert!(config.strict_sensitivity, "Production validation should require sensitivity");
}

#[test]
fn test_sensitivity_config_quick() {
    let config = SensitivityConfig::quick();
    assert!(config.enabled);
    assert!(!config.strict_sensitivity);
    
    // Quick config should have fewer sweep points
    assert!(config.latency_sweep.values_ms.len() <= 5);
    assert!(config.sampling_sweep.regimes.len() <= 3);
}

// =============================================================================
// SENSITIVITY REPORT FORMAT TESTS
// =============================================================================

#[test]
fn test_sensitivity_report_format_text() {
    let report = SensitivityReport {
        sensitivity_run: true,
        latency_sweep: Some(LatencySweepResults {
            config: LatencySweepConfig::default(),
            points: vec![
                SweepPointMetrics {
                    parameter_value: 0.0,
                    pnl_after_fees: 1000.0,
                    total_fills: 100,
                    fill_rate: 0.8,
                    sharpe_ratio: Some(2.5),
                    ..Default::default()
                },
                SweepPointMetrics {
                    parameter_value: 50.0,
                    pnl_after_fees: 800.0,
                    total_fills: 80,
                    fill_rate: 0.7,
                    sharpe_ratio: Some(2.0),
                    ..Default::default()
                },
            ],
            baseline_index: 0,
            profitability_flip_index: None,
            max_profitable_latency_ms: Some(100.0),
        }),
        sampling_sweep: None,
        execution_sweep: None,
        fragility: FragilityFlags::default(),
        trust_recommendation: TrustRecommendation::Trusted,
    };
    
    let text = report.format_text();
    
    // Check key sections are present
    assert!(text.contains("SENSITIVITY ANALYSIS REPORT"));
    assert!(text.contains("Sensitivity Run: YES"));
    assert!(text.contains("Trust Recommendation"));
    assert!(text.contains("LATENCY SWEEP"));
    assert!(text.contains("FRAGILITY FLAGS"));
}

// =============================================================================
// DETERMINISM TESTS
// =============================================================================

#[test]
fn test_latency_config_generation_deterministic() {
    let sweep = LatencySweepConfig {
        component: LatencyComponent::EndToEnd,
        values_ms: vec![50.0],
        use_fixed: true,
    };
    let base = LatencyConfig::default();
    
    // Generate same config twice
    let config1 = sweep.config_for_value(50.0, &base);
    let config2 = sweep.config_for_value(50.0, &base);
    
    // Should produce identical results
    match (&config1.market_data, &config2.market_data) {
        (LatencyDistribution::Fixed { latency_ns: ns1 }, 
         LatencyDistribution::Fixed { latency_ns: ns2 }) => {
            assert_eq!(ns1, ns2);
        }
        _ => panic!("Expected identical Fixed distributions"),
    }
}

#[test]
fn test_fragility_detection_deterministic() {
    let detector = FragilityDetector::new(FragilityThresholds::default());
    
    let results = LatencySweepResults {
        config: LatencySweepConfig::default(),
        points: vec![
            SweepPointMetrics {
                parameter_value: 0.0,
                pnl_after_fees: 1000.0,
                ..Default::default()
            },
            SweepPointMetrics {
                parameter_value: 50.0,
                pnl_after_fees: 400.0,
                ..Default::default()
            },
        ],
        baseline_index: 0,
        profitability_flip_index: None,
        max_profitable_latency_ms: None,
    };
    
    // Run detection twice
    let (fragile1, reason1) = detector.detect_latency_fragility(&results);
    let (fragile2, reason2) = detector.detect_latency_fragility(&results);
    
    // Should produce identical results
    assert_eq!(fragile1, fragile2);
    assert_eq!(reason1, reason2);
}

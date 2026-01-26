//! Maker Validation Ladder Tests
//!
//! These tests verify:
//! 1. ConservativeMaker is strictly harder than NeutralMaker
//! 2. Relaxing assumptions cannot improve results unrealistically
//! 3. Deterministic outputs across runs
//! 4. Fragility detection works correctly
//! 5. Survival criteria are enforced properly

use crate::backtest_v2::maker_validation::{
    ConservativeConfig, MakerExecutionProfile, MakerFragilityFlags, MakerProfileConfigs,
    MakerSurvivalCriteria, MakerSurvivalStatus, MakerValidationConfig, MakerValidationResult,
    MakerValidationRunner, MeasuredLiveConfig, NeutralConfig, ProfileMetrics,
};
use crate::backtest_v2::latency::LatencyConfig;
use std::collections::HashMap;

// =============================================================================
// TEST 1: ConservativeMaker is strictly harder than NeutralMaker
// =============================================================================

#[test]
fn test_conservative_has_higher_latency_than_neutral() {
    let configs = MakerProfileConfigs::default();
    
    let conservative = configs.latency_config_for(MakerExecutionProfile::ConservativeMaker);
    let neutral = configs.latency_config_for(MakerExecutionProfile::NeutralMaker);
    
    // Conservative should have >= latency on all components
    assert!(
        conservative.order_latency_ns() >= neutral.order_latency_ns(),
        "Conservative order latency ({}) should be >= neutral ({})",
        conservative.order_latency_ns(),
        neutral.order_latency_ns()
    );
    
    assert!(
        conservative.cancel_latency_ns() >= neutral.cancel_latency_ns(),
        "Conservative cancel latency ({}) should be >= neutral ({})",
        conservative.cancel_latency_ns(),
        neutral.cancel_latency_ns()
    );
}

#[test]
fn test_conservative_has_larger_queue_ahead_multiplier() {
    let configs = MakerProfileConfigs::default();
    
    let conservative_mult = configs.queue_ahead_multiplier(MakerExecutionProfile::ConservativeMaker);
    let neutral_mult = configs.queue_ahead_multiplier(MakerExecutionProfile::NeutralMaker);
    let live_mult = configs.queue_ahead_multiplier(MakerExecutionProfile::MeasuredLiveMaker);
    
    // Conservative assumes more queue ahead
    assert!(
        conservative_mult > neutral_mult,
        "Conservative multiplier ({}) should be > neutral ({})",
        conservative_mult,
        neutral_mult
    );
    
    // Neutral and live should be equal (no pessimism)
    assert_eq!(neutral_mult, 1.0);
    assert_eq!(live_mult, 1.0);
}

#[test]
fn test_profile_ordering_is_strict() {
    // Profile ordering: Conservative < Neutral < MeasuredLive
    let profiles = MakerExecutionProfile::all_ordered();
    
    for i in 0..profiles.len() - 1 {
        let current = profiles[i];
        let next = profiles[i + 1];
        
        assert!(
            current.is_stricter_than(next),
            "{:?} should be stricter than {:?}",
            current,
            next
        );
        
        assert!(
            !next.is_stricter_than(current),
            "{:?} should NOT be stricter than {:?}",
            next,
            current
        );
    }
}

// =============================================================================
// TEST 2: Relaxing assumptions cannot improve results unrealistically
// =============================================================================

#[test]
fn test_suspicious_improvement_is_detected() {
    let runner = MakerValidationRunner::new(MakerValidationConfig::default());
    
    let mut metrics = HashMap::new();
    
    // Conservative: modest profit
    metrics.insert(MakerExecutionProfile::ConservativeMaker, ProfileMetrics {
        profile: Some(MakerExecutionProfile::ConservativeMaker),
        net_pnl: 10.0,
        maker_pnl: 8.0,
        taker_pnl: 2.0,
        total_fills: 20,
        maker_fills: 15,
        taker_fills: 5,
        maker_fill_rate: 0.5,
        max_drawdown: 0.1,
        ..Default::default()
    });
    
    // Neutral: unrealistically better (10x improvement)
    metrics.insert(MakerExecutionProfile::NeutralMaker, ProfileMetrics {
        profile: Some(MakerExecutionProfile::NeutralMaker),
        net_pnl: 1000.0,  // 100x improvement
        maker_pnl: 900.0,
        taker_pnl: 100.0,
        total_fills: 100,
        maker_fills: 90,
        taker_fills: 10,
        maker_fill_rate: 0.9,
        max_drawdown: 0.02,
        ..Default::default()
    });
    
    let result = runner.analyze(metrics);
    
    // Should detect suspicious improvement
    assert!(result.fragility.improves_when_relaxed);
    assert_eq!(result.survival_status, MakerSurvivalStatus::Fail);
}

#[test]
fn test_moderate_improvement_is_allowed() {
    let runner = MakerValidationRunner::new(MakerValidationConfig::default());
    
    let mut metrics = HashMap::new();
    
    // Conservative: good profit
    metrics.insert(MakerExecutionProfile::ConservativeMaker, ProfileMetrics {
        profile: Some(MakerExecutionProfile::ConservativeMaker),
        net_pnl: 100.0,
        maker_fills: 30,
        taker_fills: 20,
        total_fills: 50,
        maker_fill_rate: 0.6,
        max_drawdown: 0.1,
        ..Default::default()
    });
    
    // Neutral: modestly better (50% improvement - within threshold)
    metrics.insert(MakerExecutionProfile::NeutralMaker, ProfileMetrics {
        profile: Some(MakerExecutionProfile::NeutralMaker),
        net_pnl: 150.0,  // 50% improvement
        maker_fills: 35,
        taker_fills: 20,
        total_fills: 55,
        maker_fill_rate: 0.65,
        max_drawdown: 0.08,
        ..Default::default()
    });
    
    let result = runner.analyze(metrics);
    
    // Should NOT be flagged as suspicious
    assert!(!result.fragility.improves_when_relaxed);
    assert_eq!(result.survival_status, MakerSurvivalStatus::Pass);
}

// =============================================================================
// TEST 3: Deterministic outputs across runs
// =============================================================================

#[test]
fn test_validation_is_deterministic() {
    let config = MakerValidationConfig::default();
    
    let mut metrics1 = HashMap::new();
    metrics1.insert(MakerExecutionProfile::ConservativeMaker, ProfileMetrics {
        profile: Some(MakerExecutionProfile::ConservativeMaker),
        net_pnl: 75.0,
        maker_fills: 25,
        taker_fills: 15,
        total_fills: 40,
        maker_fill_rate: 0.5,
        max_drawdown: 0.12,
        ..Default::default()
    });
    
    // Run 1
    let runner1 = MakerValidationRunner::new(config.clone());
    let result1 = runner1.analyze(metrics1.clone());
    
    // Run 2 (same inputs)
    let runner2 = MakerValidationRunner::new(config);
    let result2 = runner2.analyze(metrics1);
    
    // Results should be identical
    assert_eq!(result1.survival_status, result2.survival_status);
    assert_eq!(result1.fragility.is_fragile(), result2.fragility.is_fragile());
    assert_eq!(result1.failure_reasons, result2.failure_reasons);
}

#[test]
fn test_latency_config_is_deterministic() {
    let configs = MakerProfileConfigs::default();
    
    // Generate configs twice
    let latency1 = configs.latency_config_for(MakerExecutionProfile::ConservativeMaker);
    let latency2 = configs.latency_config_for(MakerExecutionProfile::ConservativeMaker);
    
    // Should be identical
    assert_eq!(latency1.order_latency_ns(), latency2.order_latency_ns());
    assert_eq!(latency1.cancel_latency_ns(), latency2.cancel_latency_ns());
}

// =============================================================================
// TEST 4: Fragility detection works correctly
// =============================================================================

#[test]
fn test_conservative_fragility_flag() {
    let runner = MakerValidationRunner::new(MakerValidationConfig::default());
    
    let mut metrics = HashMap::new();
    metrics.insert(MakerExecutionProfile::ConservativeMaker, ProfileMetrics {
        profile: Some(MakerExecutionProfile::ConservativeMaker),
        net_pnl: -100.0,  // Negative PnL
        maker_fills: 10,
        total_fills: 20,
        maker_fill_rate: 0.5,
        max_drawdown: 0.3,
        ..Default::default()
    });
    
    let result = runner.analyze(metrics);
    
    assert!(result.fragility.fragile_conservative);
    assert!(result.fragility.conservative_reason.is_some());
    assert!(result.fragility.is_fragile());
}

#[test]
fn test_sign_flip_fragility_flag() {
    let mut config = MakerValidationConfig::default();
    config.criteria.allow_sign_flip = false;
    
    let runner = MakerValidationRunner::new(config);
    
    let mut metrics = HashMap::new();
    metrics.insert(MakerExecutionProfile::ConservativeMaker, ProfileMetrics {
        profile: Some(MakerExecutionProfile::ConservativeMaker),
        net_pnl: 100.0,  // Positive
        maker_fills: 20,
        total_fills: 30,
        maker_fill_rate: 0.6,
        max_drawdown: 0.1,
        ..Default::default()
    });
    metrics.insert(MakerExecutionProfile::NeutralMaker, ProfileMetrics {
        profile: Some(MakerExecutionProfile::NeutralMaker),
        net_pnl: -50.0,  // Negative (sign flip!)
        maker_fills: 25,
        total_fills: 35,
        maker_fill_rate: 0.7,
        max_drawdown: 0.2,
        ..Default::default()
    });
    
    let result = runner.analyze(metrics);
    
    assert!(result.fragility.sign_flip_detected);
    assert!(result.fragility.sign_flip_profiles.is_some());
    assert_eq!(result.survival_status, MakerSurvivalStatus::Fail);
}

#[test]
fn test_latency_fragility_flag() {
    let runner = MakerValidationRunner::new(MakerValidationConfig::default());
    
    let mut metrics = HashMap::new();
    metrics.insert(MakerExecutionProfile::ConservativeMaker, ProfileMetrics {
        profile: Some(MakerExecutionProfile::ConservativeMaker),
        net_pnl: 100.0,
        maker_fills: 20,
        total_fills: 30,
        maker_fill_rate: 0.6,
        max_drawdown: 0.1,
        ..Default::default()
    });
    metrics.insert(MakerExecutionProfile::NeutralMaker, ProfileMetrics {
        profile: Some(MakerExecutionProfile::NeutralMaker),
        net_pnl: 150.0,
        maker_fills: 25,
        total_fills: 35,
        maker_fill_rate: 0.7,
        max_drawdown: 0.08,
        ..Default::default()
    });
    metrics.insert(MakerExecutionProfile::MeasuredLiveMaker, ProfileMetrics {
        profile: Some(MakerExecutionProfile::MeasuredLiveMaker),
        net_pnl: 20.0,  // 87% degradation from neutral (150 -> 20)
        maker_fills: 10,
        total_fills: 20,
        maker_fill_rate: 0.4,
        max_drawdown: 0.25,
        ..Default::default()
    });
    
    let result = runner.analyze(metrics);
    
    // Default threshold is 50% degradation, so 87% should trigger
    assert!(result.fragility.fragile_latency);
    assert!(result.fragility.latency_reason.is_some());
}

#[test]
fn test_all_fragility_reasons_collected() {
    let mut flags = MakerFragilityFlags::default();
    flags.fragile_conservative = true;
    flags.conservative_reason = Some("PnL below threshold".to_string());
    flags.fragile_latency = true;
    flags.latency_reason = Some("High latency sensitivity".to_string());
    flags.sign_flip_detected = true;
    flags.sign_flip_profiles = Some((
        MakerExecutionProfile::ConservativeMaker,
        MakerExecutionProfile::NeutralMaker,
    ));
    
    let reasons = flags.all_reasons();
    
    assert_eq!(reasons.len(), 3);
    assert!(reasons.iter().any(|r| r.contains("Conservative")));
    assert!(reasons.iter().any(|r| r.contains("Latency")));
    assert!(reasons.iter().any(|r| r.contains("Sign flip")));
}

// =============================================================================
// TEST 5: Survival criteria are enforced properly
// =============================================================================

#[test]
fn test_min_conservative_pnl_criterion() {
    let mut config = MakerValidationConfig::default();
    config.criteria.min_conservative_pnl = 50.0;  // Require $50 minimum
    
    let runner = MakerValidationRunner::new(config);
    
    // Test case 1: Below threshold
    let mut metrics1 = HashMap::new();
    metrics1.insert(MakerExecutionProfile::ConservativeMaker, ProfileMetrics {
        profile: Some(MakerExecutionProfile::ConservativeMaker),
        net_pnl: 30.0,  // Below threshold
        maker_fills: 10,
        total_fills: 20,
        maker_fill_rate: 0.5,
        max_drawdown: 0.1,
        ..Default::default()
    });
    
    let result1 = runner.analyze(metrics1);
    assert_eq!(result1.survival_status, MakerSurvivalStatus::Fail);
    
    // Test case 2: Above threshold
    let mut metrics2 = HashMap::new();
    metrics2.insert(MakerExecutionProfile::ConservativeMaker, ProfileMetrics {
        profile: Some(MakerExecutionProfile::ConservativeMaker),
        net_pnl: 60.0,  // Above threshold
        maker_fills: 10,
        total_fills: 20,
        maker_fill_rate: 0.5,
        max_drawdown: 0.1,
        ..Default::default()
    });
    
    let result2 = runner.analyze(metrics2);
    assert_eq!(result2.survival_status, MakerSurvivalStatus::Pass);
}

#[test]
fn test_max_drawdown_criterion() {
    let mut config = MakerValidationConfig::default();
    config.criteria.max_conservative_drawdown_pct = 20.0;  // 20% max
    
    let runner = MakerValidationRunner::new(config);
    
    let mut metrics = HashMap::new();
    metrics.insert(MakerExecutionProfile::ConservativeMaker, ProfileMetrics {
        profile: Some(MakerExecutionProfile::ConservativeMaker),
        net_pnl: 100.0,
        maker_fills: 20,
        total_fills: 30,
        maker_fill_rate: 0.6,
        max_drawdown: 0.35,  // 35% > 20% threshold
        ..Default::default()
    });
    
    let result = runner.analyze(metrics);
    
    assert_eq!(result.survival_status, MakerSurvivalStatus::Fail);
    assert!(result.failure_reasons.iter().any(|r| r.contains("drawdown")));
}

#[test]
fn test_min_maker_fill_rate_criterion() {
    let mut config = MakerValidationConfig::default();
    config.criteria.min_conservative_maker_fill_rate = 0.2;  // 20% min
    
    let runner = MakerValidationRunner::new(config);
    
    let mut metrics = HashMap::new();
    metrics.insert(MakerExecutionProfile::ConservativeMaker, ProfileMetrics {
        profile: Some(MakerExecutionProfile::ConservativeMaker),
        net_pnl: 100.0,
        maker_fills: 5,
        total_fills: 100,
        maker_fill_rate: 0.05,  // 5% < 20% threshold
        max_drawdown: 0.1,
        ..Default::default()
    });
    
    let result = runner.analyze(metrics);
    
    assert_eq!(result.survival_status, MakerSurvivalStatus::Fail);
    assert!(result.failure_reasons.iter().any(|r| r.contains("fill rate")));
}

#[test]
fn test_require_profitable_all_profiles() {
    let mut config = MakerValidationConfig::default();
    config.criteria.require_profitable_all_profiles = true;
    config.criteria.allow_sign_flip = true;  // Allow sign flip to isolate this criterion
    
    let runner = MakerValidationRunner::new(config);
    
    let mut metrics = HashMap::new();
    metrics.insert(MakerExecutionProfile::ConservativeMaker, ProfileMetrics {
        profile: Some(MakerExecutionProfile::ConservativeMaker),
        net_pnl: 100.0,
        maker_fills: 20,
        total_fills: 30,
        maker_fill_rate: 0.6,
        max_drawdown: 0.1,
        ..Default::default()
    });
    metrics.insert(MakerExecutionProfile::NeutralMaker, ProfileMetrics {
        profile: Some(MakerExecutionProfile::NeutralMaker),
        net_pnl: -10.0,  // Not profitable
        maker_fills: 25,
        total_fills: 35,
        maker_fill_rate: 0.7,
        max_drawdown: 0.15,
        ..Default::default()
    });
    
    let result = runner.analyze(metrics);
    
    assert_eq!(result.survival_status, MakerSurvivalStatus::Fail);
    assert!(result.failure_reasons.iter().any(|r| r.contains("not profitable")));
}

#[test]
fn test_strict_criteria_is_stricter() {
    let default_criteria = MakerSurvivalCriteria::default();
    let strict_criteria = MakerSurvivalCriteria::strict();
    
    // Strict should have higher thresholds
    assert!(
        strict_criteria.min_conservative_maker_fill_rate >= default_criteria.min_conservative_maker_fill_rate
    );
    assert!(
        strict_criteria.max_conservative_drawdown_pct <= default_criteria.max_conservative_drawdown_pct
    );
    assert!(
        strict_criteria.max_neutral_to_live_degradation_pct <= default_criteria.max_neutral_to_live_degradation_pct
    );
}

#[test]
fn test_lenient_criteria_is_more_permissive() {
    let default_criteria = MakerSurvivalCriteria::default();
    let lenient_criteria = MakerSurvivalCriteria::lenient();
    
    // Lenient should allow more
    assert!(lenient_criteria.min_conservative_pnl < default_criteria.min_conservative_pnl);
    assert!(lenient_criteria.max_conservative_drawdown_pct > default_criteria.max_conservative_drawdown_pct);
    assert!(lenient_criteria.allow_sign_flip);
}

// =============================================================================
// TEST 6: Edge cases and boundary conditions
// =============================================================================

#[test]
fn test_zero_pnl_passes_default_criteria() {
    let runner = MakerValidationRunner::new(MakerValidationConfig::default());
    
    let mut metrics = HashMap::new();
    metrics.insert(MakerExecutionProfile::ConservativeMaker, ProfileMetrics {
        profile: Some(MakerExecutionProfile::ConservativeMaker),
        net_pnl: 0.0,  // Exactly zero
        maker_fills: 20,
        total_fills: 30,
        maker_fill_rate: 0.6,
        max_drawdown: 0.1,
        ..Default::default()
    });
    
    let result = runner.analyze(metrics);
    
    // Zero PnL should pass (default threshold is 0.0)
    assert_eq!(result.survival_status, MakerSurvivalStatus::Pass);
}

#[test]
fn test_pnl_change_calculation_from_zero_baseline() {
    let baseline = ProfileMetrics {
        net_pnl: 0.0,
        ..Default::default()
    };
    
    let positive = ProfileMetrics {
        net_pnl: 100.0,
        ..Default::default()
    };
    
    let negative = ProfileMetrics {
        net_pnl: -50.0,
        ..Default::default()
    };
    
    // Change from zero baseline
    let change_pos = positive.pnl_change_pct_from(&baseline);
    let change_neg = negative.pnl_change_pct_from(&baseline);
    
    // With zero baseline, positive becomes infinity, negative becomes neg infinity
    assert!(change_pos.is_infinite() && change_pos > 0.0);
    assert!(change_neg.is_infinite() && change_neg < 0.0);
}

#[test]
fn test_empty_profile_set_is_not_applicable() {
    let runner = MakerValidationRunner::new(MakerValidationConfig::default());
    let result = runner.analyze(HashMap::new());
    
    assert_eq!(result.survival_status, MakerSurvivalStatus::NotApplicable);
    assert!(!result.is_maker_viable());
}

#[test]
fn test_taker_only_strategy_is_not_applicable_for_maker_validation() {
    let runner = MakerValidationRunner::new(MakerValidationConfig::default());
    
    let mut metrics = HashMap::new();
    metrics.insert(MakerExecutionProfile::ConservativeMaker, ProfileMetrics {
        profile: Some(MakerExecutionProfile::ConservativeMaker),
        net_pnl: 100.0,
        maker_fills: 0,  // No maker fills
        taker_fills: 50,
        total_fills: 50,
        maker_fill_rate: 0.0,
        max_drawdown: 0.1,
        ..Default::default()
    });
    
    let result = runner.analyze(metrics);
    
    assert_eq!(result.survival_status, MakerSurvivalStatus::NotApplicable);
}

// =============================================================================
// TEST 7: Report formatting
// =============================================================================

#[test]
fn test_report_contains_all_sections() {
    let runner = MakerValidationRunner::new(MakerValidationConfig::default());
    
    let mut metrics = HashMap::new();
    metrics.insert(MakerExecutionProfile::ConservativeMaker, ProfileMetrics {
        profile: Some(MakerExecutionProfile::ConservativeMaker),
        net_pnl: 100.0,
        maker_pnl: 60.0,
        taker_pnl: 40.0,
        maker_fills: 30,
        taker_fills: 20,
        total_fills: 50,
        maker_fill_rate: 0.6,
        max_drawdown: 0.1,
        ..Default::default()
    });
    metrics.insert(MakerExecutionProfile::NeutralMaker, ProfileMetrics {
        profile: Some(MakerExecutionProfile::NeutralMaker),
        net_pnl: 120.0,
        maker_pnl: 75.0,
        taker_pnl: 45.0,
        maker_fills: 35,
        taker_fills: 20,
        total_fills: 55,
        maker_fill_rate: 0.65,
        max_drawdown: 0.08,
        ..Default::default()
    });
    
    let result = runner.analyze(metrics);
    let report = result.format_report();
    
    // Check all sections present
    assert!(report.contains("MAKER VALIDATION LADDER"));
    assert!(report.contains("SURVIVAL STATUS"));
    assert!(report.contains("MAKER VIABLE"));
    assert!(report.contains("PROFILE RESULTS"));
    assert!(report.contains("Conservative"));
    assert!(report.contains("Neutral"));
}

#[test]
fn test_report_shows_failure_reasons_when_failed() {
    let runner = MakerValidationRunner::new(MakerValidationConfig::default());
    
    let mut metrics = HashMap::new();
    metrics.insert(MakerExecutionProfile::ConservativeMaker, ProfileMetrics {
        profile: Some(MakerExecutionProfile::ConservativeMaker),
        net_pnl: -50.0,  // Will fail
        maker_fills: 10,
        total_fills: 20,
        maker_fill_rate: 0.5,
        max_drawdown: 0.1,
        ..Default::default()
    });
    
    let result = runner.analyze(metrics);
    let report = result.format_report();
    
    assert!(report.contains("FAILURE REASONS"));
    assert!(report.contains("PnL"));
}

#[test]
fn test_report_shows_fragility_when_fragile() {
    let mut config = MakerValidationConfig::default();
    config.criteria.allow_sign_flip = false;
    
    let runner = MakerValidationRunner::new(config);
    
    let mut metrics = HashMap::new();
    metrics.insert(MakerExecutionProfile::ConservativeMaker, ProfileMetrics {
        profile: Some(MakerExecutionProfile::ConservativeMaker),
        net_pnl: 100.0,
        maker_fills: 20,
        total_fills: 30,
        maker_fill_rate: 0.6,
        max_drawdown: 0.1,
        ..Default::default()
    });
    metrics.insert(MakerExecutionProfile::NeutralMaker, ProfileMetrics {
        profile: Some(MakerExecutionProfile::NeutralMaker),
        net_pnl: -50.0,  // Sign flip
        maker_fills: 25,
        total_fills: 35,
        maker_fill_rate: 0.7,
        max_drawdown: 0.2,
        ..Default::default()
    });
    
    let result = runner.analyze(metrics);
    let report = result.format_report();
    
    assert!(report.contains("FRAGILITY FLAGS"));
    assert!(report.contains("Sign flip"));
}

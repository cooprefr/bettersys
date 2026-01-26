//! Trust Gate Tests
//!
//! Comprehensive tests verifying the trust workflow is properly locked down.

use crate::backtest_v2::data_contract::DatasetReadiness;
use crate::backtest_v2::fingerprint::{
    BehaviorFingerprint, CodeFingerprint, ConfigFingerprint, DatasetFingerprint, RunFingerprint,
    SeedFingerprint, StrategyFingerprint,
};
use crate::backtest_v2::gate_suite::{
    GateMetrics, GateSuiteConfig, GateSuiteReport, GateTestResult, TrustLevel as GateTrustLevel,
};
use crate::backtest_v2::invariants::InvariantMode;
use crate::backtest_v2::orchestrator::{BacktestConfig, BacktestResults, MakerFillModel};
use crate::backtest_v2::sensitivity::{FragilityFlags, SensitivityReport, TrustRecommendation};
use crate::backtest_v2::settlement::SettlementModel;
use crate::backtest_v2::sim_adapter::{OmsParityMode, OmsParityStats};
use crate::backtest_v2::trust_gate::{TrustDecision, TrustFailureReason, TrustGate};

// =============================================================================
// TEST HELPERS
// =============================================================================

fn make_production_config() -> BacktestConfig {
    BacktestConfig::production_grade_15m_updown()
}

fn make_production_results() -> BacktestResults {
    let mut results = BacktestResults::default();
    results.production_grade = true;
    results.strict_accounting_enabled = true;
    results.first_accounting_violation = None;
    results.invariant_mode = InvariantMode::Hard;
    results.invariant_violations_detected = 0;
    results.settlement_model = SettlementModel::ExactSpec;
    results.oms_parity = Some(OmsParityStats {
        mode: OmsParityMode::Full,
        valid_for_production: true,
        ..Default::default()
    });
    results.dataset_readiness = DatasetReadiness::MakerViable;
    results.maker_fills_valid = true;
    results.maker_fills = 0;
    results
}

fn make_passing_gate_suite_report() -> GateSuiteReport {
    GateSuiteReport {
        passed: true,
        gates: vec![
            GateTestResult {
                name: "Gate A: Zero-Edge Matching".to_string(),
                passed: true,
                failure_reason: None,
                metrics: GateMetrics::default(),
                failed_seeds: vec![],
                execution_ms: 100,
            },
            GateTestResult {
                name: "Gate B: Martingale Price Path".to_string(),
                passed: true,
                failure_reason: None,
                metrics: GateMetrics::default(),
                failed_seeds: vec![],
                execution_ms: 100,
            },
            GateTestResult {
                name: "Gate C: Signal Inversion Symmetry".to_string(),
                passed: true,
                failure_reason: None,
                metrics: GateMetrics::default(),
                failed_seeds: vec![],
                execution_ms: 100,
            },
        ],
        trust_level: GateTrustLevel::Trusted,
        config: GateSuiteConfig::default(),
        total_execution_ms: 300,
        timestamp: 0,
    }
}

fn make_failing_gate_suite_report() -> GateSuiteReport {
    GateSuiteReport {
        passed: false,
        gates: vec![
            GateTestResult {
                name: "Gate A: Zero-Edge Matching".to_string(),
                passed: false,
                failure_reason: Some("Mean PnL before fees $5.50 exceeds tolerance $0.50".to_string()),
                metrics: GateMetrics {
                    pnl_before_fees: 5.50,
                    ..Default::default()
                },
                failed_seeds: vec![42, 43, 44],
                execution_ms: 100,
            },
        ],
        trust_level: GateTrustLevel::Untrusted {
            reasons: vec![crate::backtest_v2::gate_suite::GateFailureReason::new(
                "Gate A",
                "Mean PnL before fees exceeds tolerance",
                "pnl_before_fees",
                "5.50",
                "< 0.50",
            )],
        },
        config: GateSuiteConfig::default(),
        total_execution_ms: 100,
        timestamp: 0,
    }
}

fn make_passing_sensitivity_report() -> SensitivityReport {
    SensitivityReport {
        sensitivity_run: true,
        latency_sweep: None,
        sampling_sweep: None,
        execution_sweep: None,
        fragility: FragilityFlags::default(),
        trust_recommendation: TrustRecommendation::Trusted,
    }
}

fn make_fragile_sensitivity_report() -> SensitivityReport {
    SensitivityReport {
        sensitivity_run: true,
        latency_sweep: None,
        sampling_sweep: None,
        execution_sweep: None,
        fragility: FragilityFlags {
            latency_fragile: true,
            latency_fragility_reason: Some("PnL drops 80% at 50ms latency".to_string()),
            sampling_fragile: false,
            sampling_fragility_reason: None,
            execution_fragile: false,
            execution_fragility_reason: None,
            requires_optimistic_assumptions: false,
            fragility_score: 0.33,
        },
        trust_recommendation: TrustRecommendation::CautionFragile,
    }
}

fn make_valid_fingerprint() -> RunFingerprint {
    use crate::backtest_v2::fingerprint::StrategyFingerprint;
    
    RunFingerprint {
        version: "RUNFP_V2".to_string(),
        strategy: StrategyFingerprint {
            name: "test_strategy".to_string(),
            version: "1.0.0".to_string(),
            code_hash: "abc123def456".to_string(),
            hash: 0x1111_2222_3333_4444,
        },
        code: CodeFingerprint {
            crate_version: "1.0.0".to_string(),
            git_commit: Some("abc123def456".to_string()),
            build_profile: "release".to_string(),
            hash: 0x1234_5678_9ABC_DEF0,
        },
        config: ConfigFingerprint {
            settlement_reference_rule: Some("LastUpdateAtOrBeforeCutoff".to_string()),
            settlement_tie_rule: Some("NoWins".to_string()),
            chainlink_feed_id: None,
            oracle_chain_id: Some(137),
            oracle_feed_proxies: vec![],
            oracle_decimals: vec![],
            oracle_visibility_rule: None,
            oracle_rounding_policy: None,
            oracle_config_hash: None,
            latency_model: "Fixed".to_string(),
            order_latency_ns: Some(1_000_000),
            oms_parity_mode: "Full".to_string(),
            maker_fill_model: "ExplicitQueue".to_string(),
            integrity_policy: "Strict".to_string(),
            invariant_mode: "Hard".to_string(),
            fee_rate_bps: Some(10),
            strategy_params_hash: 0xABCD_1234,
            arrival_policy: "RecordedArrival".to_string(),
            strict_accounting: true,
            production_grade: true,
            allow_non_production: false,
            hash: 0xFEDC_BA98_7654_3210,
        },
        dataset: DatasetFingerprint {
            classification: "FullIncremental".to_string(),
            readiness: "MakerViable".to_string(),
            orderbook_type: "FullIncrementalL2DeltasWithExchangeSeq".to_string(),
            trade_type: "TradePrints".to_string(),
            arrival_semantics: "RecordedArrival".to_string(),
            streams: vec![],
            hash: 0x1111_2222_3333_4444,
        },
        seed: SeedFingerprint {
            primary_seed: 42,
            sub_seeds: vec![
                ("latency".to_string(), 0x1000),
                ("fill_probability".to_string(), 0x2000),
                ("queue_position".to_string(), 0x3000),
            ],
            hash: 0x5555_6666_7777_8888,
        },
        behavior: BehaviorFingerprint {
            event_count: 1000,
            hash: 0x9999_AAAA_BBBB_CCCC,
        },
        registry: None,
        hash: 0xDDDD_EEEE_FFFF_0000,
        hash_hex: "ddddeeee ffff0000".to_string(),
    }
}

// =============================================================================
// CORE TRUST GATE TESTS
// =============================================================================

#[test]
fn test_gate_suite_skipped_results_in_untrusted_missing_gate_suite() {
    let config = make_production_config();
    let results = make_production_results();
    let sensitivity = make_passing_sensitivity_report();
    let fingerprint = make_valid_fingerprint();

    let decision = TrustGate::evaluate(
        &config,
        &results,
        None, // Gate suite not run
        Some(&sensitivity),
        Some(&fingerprint),
    );

    assert!(
        !decision.is_trusted(),
        "Expected Untrusted when GateSuite is missing"
    );
    assert!(
        decision
            .failure_reasons()
            .iter()
            .any(|r| matches!(r, TrustFailureReason::MissingGateSuite)),
        "Expected MissingGateSuite failure reason"
    );
}

#[test]
fn test_gate_suite_run_but_failed_results_in_untrusted_gate_suite_failed() {
    let config = make_production_config();
    let results = make_production_results();
    let gate_report = make_failing_gate_suite_report();
    let sensitivity = make_passing_sensitivity_report();
    let fingerprint = make_valid_fingerprint();

    let decision = TrustGate::evaluate(
        &config,
        &results,
        Some(&gate_report),
        Some(&sensitivity),
        Some(&fingerprint),
    );

    assert!(
        !decision.is_trusted(),
        "Expected Untrusted when GateSuite failed"
    );
    
    let has_gate_failed = decision.failure_reasons().iter().any(|r| {
        matches!(r, TrustFailureReason::GateSuiteFailed { failed_gates, .. } 
            if !failed_gates.is_empty())
    });
    assert!(has_gate_failed, "Expected GateSuiteFailed failure reason with failed gates");
}

#[test]
fn test_sensitivity_missing_results_in_untrusted_missing_sensitivity() {
    let config = make_production_config();
    let results = make_production_results();
    let gate_report = make_passing_gate_suite_report();
    let fingerprint = make_valid_fingerprint();

    let decision = TrustGate::evaluate(
        &config,
        &results,
        Some(&gate_report),
        None, // Sensitivity not run
        Some(&fingerprint),
    );

    assert!(
        !decision.is_trusted(),
        "Expected Untrusted when sensitivity is missing"
    );
    assert!(
        decision
            .failure_reasons()
            .iter()
            .any(|r| matches!(r, TrustFailureReason::MissingSensitivityAnalysis)),
        "Expected MissingSensitivityAnalysis failure reason"
    );
}

#[test]
fn test_sensitivity_fragile_results_in_untrusted_sensitivity_outside_tolerance() {
    let config = make_production_config();
    let results = make_production_results();
    let gate_report = make_passing_gate_suite_report();
    let sensitivity = make_fragile_sensitivity_report();
    let fingerprint = make_valid_fingerprint();

    let decision = TrustGate::evaluate(
        &config,
        &results,
        Some(&gate_report),
        Some(&sensitivity),
        Some(&fingerprint),
    );

    assert!(
        !decision.is_trusted(),
        "Expected Untrusted when sensitivity shows fragility"
    );
    
    let has_sensitivity_failure = decision.failure_reasons().iter().any(|r| {
        matches!(r, TrustFailureReason::SensitivityOutsideTolerance { dimension, .. } 
            if dimension == "latency")
    });
    assert!(
        has_sensitivity_failure,
        "Expected SensitivityOutsideTolerance with latency dimension"
    );
}

#[test]
fn test_fingerprint_missing_results_in_untrusted_missing_fingerprint() {
    let config = make_production_config();
    let results = make_production_results();
    let gate_report = make_passing_gate_suite_report();
    let sensitivity = make_passing_sensitivity_report();

    let decision = TrustGate::evaluate(
        &config,
        &results,
        Some(&gate_report),
        Some(&sensitivity),
        None, // Fingerprint not computed
    );

    assert!(
        !decision.is_trusted(),
        "Expected Untrusted when fingerprint is missing"
    );
    assert!(
        decision
            .failure_reasons()
            .iter()
            .any(|r| matches!(r, TrustFailureReason::MissingRunFingerprint)),
        "Expected MissingRunFingerprint failure reason"
    );
}

#[test]
fn test_all_artifacts_present_and_passing_results_in_trusted() {
    let config = make_production_config();
    let results = make_production_results();
    let gate_report = make_passing_gate_suite_report();
    let sensitivity = make_passing_sensitivity_report();
    let fingerprint = make_valid_fingerprint();

    let decision = TrustGate::evaluate(
        &config,
        &results,
        Some(&gate_report),
        Some(&sensitivity),
        Some(&fingerprint),
    );

    assert!(
        decision.is_trusted(),
        "Expected Trusted when all requirements are met. Failures: {:?}",
        decision.failure_reasons()
    );
}

#[test]
fn test_attempt_to_label_trusted_without_passing_fails() {
    let config = make_production_config();
    let results = make_production_results();

    // Try to claim trust without any validation artifacts
    let decision = TrustGate::evaluate(&config, &results, None, None, None);

    assert!(!decision.is_trusted(), "Must not be trusted without artifacts");
    assert!(
        decision.failure_count() >= 3,
        "Expected at least 3 failure reasons (gate suite, sensitivity, fingerprint)"
    );
}

// =============================================================================
// PRODUCTION GRADE TESTS
// =============================================================================

#[test]
fn test_production_grade_disabled_results_in_untrusted() {
    let mut config = make_production_config();
    config.production_grade = false;
    config.allow_non_production = true;

    let results = make_production_results();
    let gate_report = make_passing_gate_suite_report();
    let sensitivity = make_passing_sensitivity_report();
    let fingerprint = make_valid_fingerprint();

    let decision = TrustGate::evaluate(
        &config,
        &results,
        Some(&gate_report),
        Some(&sensitivity),
        Some(&fingerprint),
    );

    assert!(
        !decision.is_trusted(),
        "Expected Untrusted when production_grade is false"
    );
    assert!(
        decision
            .failure_reasons()
            .iter()
            .any(|r| matches!(r, TrustFailureReason::ProductionGradeDisabled)),
        "Expected ProductionGradeDisabled failure reason"
    );
}

// =============================================================================
// DATASET READINESS TESTS
// =============================================================================

#[test]
fn test_dataset_non_representative_results_in_untrusted() {
    let config = make_production_config();
    let mut results = make_production_results();
    results.dataset_readiness = DatasetReadiness::NonRepresentative;

    let gate_report = make_passing_gate_suite_report();
    let sensitivity = make_passing_sensitivity_report();
    let fingerprint = make_valid_fingerprint();

    let decision = TrustGate::evaluate(
        &config,
        &results,
        Some(&gate_report),
        Some(&sensitivity),
        Some(&fingerprint),
    );

    assert!(
        !decision.is_trusted(),
        "Expected Untrusted when dataset is NonRepresentative"
    );
    assert!(
        decision.failure_reasons().iter().any(|r| {
            matches!(r, TrustFailureReason::DatasetNotCompatibleWithStrategy { .. })
        }),
        "Expected DatasetNotCompatibleWithStrategy failure reason"
    );
}

#[test]
fn test_maker_fills_with_taker_only_dataset_results_in_untrusted() {
    let mut config = make_production_config();
    config.maker_fill_model = MakerFillModel::ExplicitQueue;

    let mut results = make_production_results();
    results.dataset_readiness = DatasetReadiness::TakerOnly;
    results.maker_fills = 100; // Has maker fills
    results.maker_fills_valid = true; // Claims they're valid

    let gate_report = make_passing_gate_suite_report();
    let sensitivity = make_passing_sensitivity_report();
    let fingerprint = make_valid_fingerprint();

    let decision = TrustGate::evaluate(
        &config,
        &results,
        Some(&gate_report),
        Some(&sensitivity),
        Some(&fingerprint),
    );

    assert!(
        !decision.is_trusted(),
        "Expected Untrusted when maker fills occur with TakerOnly dataset"
    );
}

// =============================================================================
// ACCOUNTING TESTS
// =============================================================================

#[test]
fn test_strict_accounting_disabled_results_in_untrusted() {
    let mut config = make_production_config();
    config.strict_accounting = false;
    config.allow_non_production = true;
    config.production_grade = false; // Must be false if strict_accounting is false

    let mut results = make_production_results();
    results.strict_accounting_enabled = false;

    let gate_report = make_passing_gate_suite_report();
    let sensitivity = make_passing_sensitivity_report();
    let fingerprint = make_valid_fingerprint();

    let decision = TrustGate::evaluate(
        &config,
        &results,
        Some(&gate_report),
        Some(&sensitivity),
        Some(&fingerprint),
    );

    assert!(!decision.is_trusted());
    assert!(
        decision
            .failure_reasons()
            .iter()
            .any(|r| matches!(r, TrustFailureReason::StrictAccountingDisabled)),
        "Expected StrictAccountingDisabled failure reason"
    );
}

#[test]
fn test_accounting_violation_detected_results_in_untrusted() {
    let config = make_production_config();
    let mut results = make_production_results();
    results.first_accounting_violation = Some("Cash balance went negative: -$100".to_string());

    let gate_report = make_passing_gate_suite_report();
    let sensitivity = make_passing_sensitivity_report();
    let fingerprint = make_valid_fingerprint();

    let decision = TrustGate::evaluate(
        &config,
        &results,
        Some(&gate_report),
        Some(&sensitivity),
        Some(&fingerprint),
    );

    assert!(!decision.is_trusted());
    assert!(
        decision.failure_reasons().iter().any(|r| {
            matches!(r, TrustFailureReason::AccountingViolationDetected { violation } 
                if violation.contains("negative"))
        }),
        "Expected AccountingViolationDetected failure reason"
    );
}

// =============================================================================
// INVARIANT TESTS
// =============================================================================

#[test]
fn test_invariant_mode_not_hard_results_in_untrusted() {
    let config = make_production_config();
    let mut results = make_production_results();
    results.invariant_mode = InvariantMode::Soft;

    let gate_report = make_passing_gate_suite_report();
    let sensitivity = make_passing_sensitivity_report();
    let fingerprint = make_valid_fingerprint();

    let decision = TrustGate::evaluate(
        &config,
        &results,
        Some(&gate_report),
        Some(&sensitivity),
        Some(&fingerprint),
    );

    assert!(!decision.is_trusted());
    assert!(
        decision.failure_reasons().iter().any(|r| {
            matches!(r, TrustFailureReason::InvariantModeNotHard { actual_mode } 
                if actual_mode.contains("Soft"))
        }),
        "Expected InvariantModeNotHard failure reason"
    );
}

#[test]
fn test_invariant_violation_detected_results_in_untrusted() {
    let config = make_production_config();
    let mut results = make_production_results();
    results.invariant_violations_detected = 3;
    results.first_invariant_violation = Some("Time went backwards".to_string());

    let gate_report = make_passing_gate_suite_report();
    let sensitivity = make_passing_sensitivity_report();
    let fingerprint = make_valid_fingerprint();

    let decision = TrustGate::evaluate(
        &config,
        &results,
        Some(&gate_report),
        Some(&sensitivity),
        Some(&fingerprint),
    );

    assert!(!decision.is_trusted());
    assert!(
        decision.failure_reasons().iter().any(|r| {
            matches!(r, TrustFailureReason::InvariantViolationDetected { count, .. } 
                if *count == 3)
        }),
        "Expected InvariantViolationDetected failure reason with count=3"
    );
}

// =============================================================================
// HERMETIC MODE TESTS
// =============================================================================

#[test]
fn test_hermetic_mode_disabled_results_in_untrusted() {
    let mut config = make_production_config();
    config.hermetic_config.enabled = false;
    config.allow_non_production = true;
    config.production_grade = false; // Must be false if hermetic is disabled

    let results = make_production_results();
    let gate_report = make_passing_gate_suite_report();
    let sensitivity = make_passing_sensitivity_report();
    let fingerprint = make_valid_fingerprint();

    let decision = TrustGate::evaluate(
        &config,
        &results,
        Some(&gate_report),
        Some(&sensitivity),
        Some(&fingerprint),
    );

    assert!(!decision.is_trusted());
    assert!(
        decision
            .failure_reasons()
            .iter()
            .any(|r| matches!(r, TrustFailureReason::HermeticModeDisabled)),
        "Expected HermeticModeDisabled failure reason"
    );
}

// =============================================================================
// SETTLEMENT MODEL TESTS
// =============================================================================

#[test]
fn test_settlement_model_not_exact_results_in_untrusted() {
    let config = make_production_config();
    let mut results = make_production_results();
    results.settlement_model = SettlementModel::None;

    let gate_report = make_passing_gate_suite_report();
    let sensitivity = make_passing_sensitivity_report();
    let fingerprint = make_valid_fingerprint();

    let decision = TrustGate::evaluate(
        &config,
        &results,
        Some(&gate_report),
        Some(&sensitivity),
        Some(&fingerprint),
    );

    assert!(!decision.is_trusted());
    assert!(
        decision.failure_reasons().iter().any(|r| {
            matches!(r, TrustFailureReason::SettlementModelNotExact { .. })
        }),
        "Expected SettlementModelNotExact failure reason"
    );
}

// =============================================================================
// OMS PARITY TESTS
// =============================================================================

#[test]
fn test_oms_parity_not_full_results_in_untrusted() {
    let config = make_production_config();
    let mut results = make_production_results();
    results.oms_parity = Some(OmsParityStats {
        mode: OmsParityMode::Relaxed,
        valid_for_production: false,
        ..Default::default()
    });

    let gate_report = make_passing_gate_suite_report();
    let sensitivity = make_passing_sensitivity_report();
    let fingerprint = make_valid_fingerprint();

    let decision = TrustGate::evaluate(
        &config,
        &results,
        Some(&gate_report),
        Some(&sensitivity),
        Some(&fingerprint),
    );

    assert!(!decision.is_trusted());
    assert!(
        decision.failure_reasons().iter().any(|r| {
            matches!(r, TrustFailureReason::OmsParityModeNotFull { .. })
        }),
        "Expected OmsParityModeNotFull failure reason"
    );
}

// =============================================================================
// MAKER FILLS TESTS
// =============================================================================

#[test]
fn test_maker_fills_invalid_results_in_untrusted() {
    let config = make_production_config();
    let mut results = make_production_results();
    results.maker_fills = 50;
    results.maker_fills_valid = false;

    let gate_report = make_passing_gate_suite_report();
    let sensitivity = make_passing_sensitivity_report();
    let fingerprint = make_valid_fingerprint();

    let decision = TrustGate::evaluate(
        &config,
        &results,
        Some(&gate_report),
        Some(&sensitivity),
        Some(&fingerprint),
    );

    assert!(!decision.is_trusted());
    assert!(
        decision
            .failure_reasons()
            .iter()
            .any(|r| matches!(r, TrustFailureReason::MakerFillsInvalid { .. })),
        "Expected MakerFillsInvalid failure reason"
    );
}

// =============================================================================
// MULTIPLE FAILURES TESTS
// =============================================================================

#[test]
fn test_multiple_failures_all_reported() {
    let mut config = make_production_config();
    config.production_grade = false;
    config.strict_accounting = false;
    config.hermetic_config.enabled = false;
    config.allow_non_production = true;

    let mut results = make_production_results();
    results.strict_accounting_enabled = false;
    results.invariant_mode = InvariantMode::Off;

    // No artifacts provided
    let decision = TrustGate::evaluate(&config, &results, None, None, None);

    assert!(!decision.is_trusted());

    // Should have many failure reasons
    let count = decision.failure_count();
    assert!(
        count >= 5,
        "Expected at least 5 failure reasons, got {}",
        count
    );

    // Verify specific failures are present
    let reasons = decision.failure_reasons();
    assert!(reasons.iter().any(|r| matches!(r, TrustFailureReason::ProductionGradeDisabled)));
    assert!(reasons.iter().any(|r| matches!(r, TrustFailureReason::MissingGateSuite)));
    assert!(reasons.iter().any(|r| matches!(r, TrustFailureReason::MissingSensitivityAnalysis)));
    assert!(reasons.iter().any(|r| matches!(r, TrustFailureReason::MissingRunFingerprint)));
}

// =============================================================================
// REQUIRE_TRUSTED TESTS
// =============================================================================

#[test]
fn test_require_trusted_succeeds_when_trusted() {
    let config = make_production_config();
    let results = make_production_results();
    let gate_report = make_passing_gate_suite_report();
    let sensitivity = make_passing_sensitivity_report();
    let fingerprint = make_valid_fingerprint();

    let result = TrustGate::require_trusted(
        &config,
        &results,
        Some(&gate_report),
        Some(&sensitivity),
        Some(&fingerprint),
    );

    assert!(result.is_ok(), "Expected Ok when all requirements met");
}

#[test]
fn test_require_trusted_fails_when_untrusted() {
    let config = make_production_config();
    let results = make_production_results();

    let result = TrustGate::require_trusted(&config, &results, None, None, None);

    assert!(result.is_err(), "Expected Err when requirements not met");

    let err = result.unwrap_err();
    assert!(
        err.to_string().contains("Trust gate failed"),
        "Error message should indicate trust gate failure"
    );
}

// =============================================================================
// FORMAT TESTS
// =============================================================================

#[test]
fn test_trust_decision_format_report_trusted() {
    let decision = TrustDecision::Trusted;
    let report = decision.format_report();

    assert!(report.contains("TRUSTED"));
    assert!(report.contains("All trust requirements satisfied"));
}

#[test]
fn test_trust_decision_format_report_untrusted() {
    let decision = TrustDecision::Untrusted {
        reasons: vec![
            TrustFailureReason::MissingGateSuite,
            TrustFailureReason::ProductionGradeDisabled,
        ],
    };
    let report = decision.format_report();

    assert!(report.contains("UNTRUSTED"));
    assert!(report.contains("MISSING_GATE_SUITE"));
    assert!(report.contains("PRODUCTION_GRADE_DISABLED"));
}

#[test]
fn test_trust_failure_reason_codes_are_unique() {
    use std::collections::HashSet;

    let all_reasons = vec![
        TrustFailureReason::MissingGateSuite,
        TrustFailureReason::GateSuiteFailed {
            failed_gates: vec![],
            trust_level: "".to_string(),
        },
        TrustFailureReason::GateSuiteBypassed,
        TrustFailureReason::MissingSensitivityAnalysis,
        TrustFailureReason::SensitivityOutsideTolerance {
            dimension: "".to_string(),
            reason: "".to_string(),
        },
        TrustFailureReason::MissingRunFingerprint,
        TrustFailureReason::IncompleteRunFingerprint {
            missing_components: vec![],
        },
        TrustFailureReason::FingerprintNotReproducible {
            reason: "".to_string(),
        },
        TrustFailureReason::ProductionGradeDisabled,
        TrustFailureReason::DatasetNotCompatibleWithStrategy {
            dataset_readiness: "".to_string(),
            strategy_type: "".to_string(),
            reason: "".to_string(),
        },
        TrustFailureReason::StrictAccountingDisabled,
        TrustFailureReason::AccountingViolationDetected {
            violation: "".to_string(),
        },
        TrustFailureReason::InvariantModeNotHard {
            actual_mode: "".to_string(),
        },
        TrustFailureReason::InvariantViolationDetected {
            count: 0,
            first_violation: "".to_string(),
        },
        TrustFailureReason::HermeticModeDisabled,
        TrustFailureReason::SettlementModelNotExact {
            actual_model: "".to_string(),
        },
        TrustFailureReason::OmsParityModeNotFull {
            actual_mode: "".to_string(),
        },
        TrustFailureReason::MakerFillsInvalid {
            reason: "".to_string(),
        },
    ];

    let codes: Vec<&str> = all_reasons.iter().map(|r| r.code()).collect();
    let unique_codes: HashSet<&str> = codes.iter().cloned().collect();

    assert_eq!(
        codes.len(),
        unique_codes.len(),
        "All failure reason codes must be unique"
    );
}

// =============================================================================
// QUICK CHECK TESTS
// =============================================================================

#[test]
fn test_quick_check_returns_true_for_production_config() {
    let config = make_production_config();
    assert!(TrustGate::quick_check(&config));
}

#[test]
fn test_quick_check_returns_false_for_non_production_config() {
    let config = BacktestConfig::research_mode();
    assert!(!TrustGate::quick_check(&config));
}

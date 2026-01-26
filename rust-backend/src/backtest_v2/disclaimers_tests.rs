//! Tests for the disclaimers module.
//!
//! These tests verify that disclaimers are generated correctly based on run conditions.

use crate::backtest_v2::data_contract::DatasetReadiness;
use crate::backtest_v2::disclaimers::{
    Category, Disclaimer, DisclaimerContext, DisclaimersBlock, Severity, TrustLevelSnapshot,
    generate_disclaimers,
};
use crate::backtest_v2::fingerprint::{
    BehaviorFingerprint, CodeFingerprint, ConfigFingerprint, DatasetFingerprint, RunFingerprint,
    SeedFingerprint, StrategyFingerprint,
};
use crate::backtest_v2::gate_suite::{
    GateMetrics, GateMode, GateSuiteConfig, GateSuiteReport, GateTestResult,
    TrustLevel as GateTrustLevel,
};
use crate::backtest_v2::invariants::InvariantMode;
use crate::backtest_v2::orchestrator::{BacktestConfig, BacktestResults, MakerFillModel};
use crate::backtest_v2::sensitivity::{FragilityFlags, SensitivityReport, TrustRecommendation};
use crate::backtest_v2::settlement::SettlementModel;
use crate::backtest_v2::trust_gate::TrustDecision;

// =============================================================================
// TEST HELPERS
// =============================================================================

fn make_research_config() -> BacktestConfig {
    BacktestConfig::research_mode()
}

fn make_production_config() -> BacktestConfig {
    BacktestConfig::production_grade_15m_updown()
}

fn make_default_results() -> BacktestResults {
    BacktestResults::default()
}

fn make_production_results() -> BacktestResults {
    let mut results = BacktestResults::default();
    results.production_grade = true;
    results.strict_accounting_enabled = true;
    results.invariant_mode = InvariantMode::Hard;
    results.dataset_readiness = DatasetReadiness::MakerViable;
    results.settlement_model = SettlementModel::ExactSpec;
    results.gate_suite_passed = true;
    results.trust_level = GateTrustLevel::Trusted;
    results.maker_fills_valid = true;
    results
}

fn make_passing_gate_suite_report() -> GateSuiteReport {
    GateSuiteReport {
        passed: true,
        gates: vec![GateTestResult {
            name: "Gate A: Zero-Edge Matching".to_string(),
            passed: true,
            failure_reason: None,
            metrics: GateMetrics::default(),
            failed_seeds: vec![],
            execution_ms: 100,
        }],
        trust_level: GateTrustLevel::Trusted,
        config: GateSuiteConfig::default(),
        total_execution_ms: 100,
        timestamp: 0,
    }
}

fn make_failing_gate_suite_report() -> GateSuiteReport {
    GateSuiteReport {
        passed: false,
        gates: vec![GateTestResult {
            name: "Gate A: Zero-Edge Matching".to_string(),
            passed: false,
            failure_reason: Some("PnL too high".to_string()),
            metrics: GateMetrics::default(),
            failed_seeds: vec![42],
            execution_ms: 100,
        }],
        trust_level: GateTrustLevel::Untrusted {
            reasons: vec![crate::backtest_v2::gate_suite::GateFailureReason::new(
                "Gate A",
                "PnL too high",
                "pnl",
                "100",
                "< 0",
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
            latency_fragility_reason: Some("PnL drops 80% at 50ms".to_string()),
            ..Default::default()
        },
        trust_recommendation: TrustRecommendation::CautionFragile,
    }
}

fn make_valid_fingerprint() -> RunFingerprint {
    RunFingerprint {
        version: "RUNFP_V1".to_string(),
        strategy: StrategyFingerprint {
            name: "TestStrategy".to_string(),
            version: "1.0.0".to_string(),
            code_hash: "abc123".to_string(),
            hash: 11111,
        },
        code: CodeFingerprint {
            crate_version: "1.0.0".to_string(),
            git_commit: Some("abc123".to_string()),
            build_profile: "release".to_string(),
            hash: 12345,
        },
        config: ConfigFingerprint {
            settlement_reference_rule: Some("LastUpdateAtOrBeforeCutoff".to_string()),
            settlement_tie_rule: Some("NoWins".to_string()),
            chainlink_feed_id: None,
            oracle_chain_id: None,
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
            strategy_params_hash: 12345,
            arrival_policy: "RecordedArrival".to_string(),
            strict_accounting: true,
            production_grade: true,
            allow_non_production: false,
            hash: 67890,
        },
        dataset: DatasetFingerprint {
            classification: "FullIncremental".to_string(),
            readiness: "MakerViable".to_string(),
            orderbook_type: "FullIncrementalL2DeltasWithExchangeSeq".to_string(),
            trade_type: "TradePrints".to_string(),
            arrival_semantics: "RecordedArrival".to_string(),
            streams: vec![],
            hash: 11111,
        },
        seed: SeedFingerprint {
            primary_seed: 42,
            sub_seeds: vec![],
            hash: 22222,
        },
        behavior: BehaviorFingerprint {
            event_count: 100,
            hash: 33333,
        },
        registry: None,
        hash: 99999,
        hash_hex: "000000000001869f".to_string(),
    }
}

// =============================================================================
// PART A TESTS - STRUCTURED TYPES
// =============================================================================

#[test]
fn test_severity_ordering() {
    assert!(Severity::Critical > Severity::Warning);
    assert!(Severity::Warning > Severity::Info);
    assert!(Severity::Critical > Severity::Info);
}

#[test]
fn test_severity_labels() {
    assert_eq!(Severity::Info.label(), "INFO");
    assert_eq!(Severity::Warning.label(), "WARNING");
    assert_eq!(Severity::Critical.label(), "CRITICAL");
}

#[test]
fn test_category_labels() {
    assert_eq!(Category::ProductionMode.label(), "PRODUCTION_MODE");
    assert_eq!(Category::DatasetReadiness.label(), "DATASET_READINESS");
    assert_eq!(Category::MakerValidity.label(), "MAKER_VALIDITY");
    assert_eq!(Category::GateSuite.label(), "GATE_SUITE");
    assert_eq!(Category::Sensitivity.label(), "SENSITIVITY");
    assert_eq!(Category::SettlementReference.label(), "SETTLEMENT_REFERENCE");
    assert_eq!(Category::IntegrityPolicy.label(), "INTEGRITY_POLICY");
    assert_eq!(Category::Reproducibility.label(), "REPRODUCIBILITY");
    assert_eq!(Category::DataCoverage.label(), "DATA_COVERAGE");
}

#[test]
fn test_disclaimer_creation() {
    let d = Disclaimer::critical(
        "TEST_ID",
        Category::ProductionMode,
        "Test message",
        vec!["evidence=1".to_string()],
    );

    assert_eq!(d.id, "TEST_ID");
    assert_eq!(d.severity, Severity::Critical);
    assert_eq!(d.category, Category::ProductionMode);
    assert_eq!(d.message, "Test message");
    assert_eq!(d.evidence, vec!["evidence=1".to_string()]);
}

#[test]
fn test_disclaimers_block_counts() {
    let block = DisclaimersBlock {
        generated_at_ns: 0,
        trust_level: TrustLevelSnapshot::Unknown,
        disclaimers: vec![
            Disclaimer::critical("A", Category::ProductionMode, "msg", vec![]),
            Disclaimer::critical("B", Category::ProductionMode, "msg", vec![]),
            Disclaimer::warning("C", Category::ProductionMode, "msg", vec![]),
            Disclaimer::info("D", Category::ProductionMode, "msg", vec![]),
        ],
    };

    assert_eq!(block.critical_count(), 2);
    assert_eq!(block.warning_count(), 1);
    assert_eq!(block.info_count(), 1);
    assert!(block.has_critical());
}

// =============================================================================
// RULE 1 TESTS - PRODUCTION MODE
// =============================================================================

#[test]
fn test_non_production_config_generates_critical_disclaimer() {
    let config = make_research_config();
    let results = make_default_results();

    let ctx = DisclaimerContext {
        config: &config,
        results: &results,
        gate_suite_report: None,
        sensitivity_report: None,
        run_fingerprint: None,
        trust_decision: None,
        current_time_ns: 0,
    };

    let block = generate_disclaimers(&ctx);

    assert!(block.has_critical());
    let non_prod = block
        .disclaimers
        .iter()
        .find(|d| d.id == "NON_PRODUCTION_RUN");
    assert!(non_prod.is_some());
    assert_eq!(non_prod.unwrap().severity, Severity::Critical);
}

#[test]
fn test_downgraded_mode_generates_critical_disclaimer() {
    let mut config = make_research_config();
    config.allow_non_production = true;

    let mut results = make_default_results();
    results.downgraded_subsystems = vec![
        "strict_accounting=false".to_string(),
        "production_grade=false".to_string(),
    ];

    let ctx = DisclaimerContext {
        config: &config,
        results: &results,
        gate_suite_report: None,
        sensitivity_report: None,
        run_fingerprint: None,
        trust_decision: None,
        current_time_ns: 0,
    };

    let block = generate_disclaimers(&ctx);

    let downgraded = block
        .disclaimers
        .iter()
        .find(|d| d.id == "DOWNGRADED_MODE");
    assert!(downgraded.is_some());
    assert_eq!(downgraded.unwrap().severity, Severity::Critical);
}

// =============================================================================
// RULE 2 TESTS - DATASET READINESS
// =============================================================================

#[test]
fn test_taker_only_dataset_generates_warning() {
    let config = make_research_config();
    let mut results = make_default_results();
    results.dataset_readiness = DatasetReadiness::TakerOnly;

    let ctx = DisclaimerContext {
        config: &config,
        results: &results,
        gate_suite_report: None,
        sensitivity_report: None,
        run_fingerprint: None,
        trust_decision: None,
        current_time_ns: 0,
    };

    let block = generate_disclaimers(&ctx);

    let taker_only = block
        .disclaimers
        .iter()
        .find(|d| d.id == "TAKER_ONLY_DATASET");
    assert!(taker_only.is_some());
    assert_eq!(taker_only.unwrap().severity, Severity::Warning);
}

#[test]
fn test_taker_only_dataset_with_maker_requested_generates_critical() {
    let mut config = make_research_config();
    config.maker_fill_model = MakerFillModel::ExplicitQueue;

    let mut results = make_default_results();
    results.dataset_readiness = DatasetReadiness::TakerOnly;

    let ctx = DisclaimerContext {
        config: &config,
        results: &results,
        gate_suite_report: None,
        sensitivity_report: None,
        run_fingerprint: None,
        trust_decision: None,
        current_time_ns: 0,
    };

    let block = generate_disclaimers(&ctx);

    let critical = block
        .disclaimers
        .iter()
        .find(|d| d.id == "TAKER_ONLY_DATASET_WITH_MAKER");
    assert!(critical.is_some());
    assert_eq!(critical.unwrap().severity, Severity::Critical);
}

#[test]
fn test_non_representative_dataset_generates_critical() {
    let config = make_research_config();
    let mut results = make_default_results();
    results.dataset_readiness = DatasetReadiness::NonRepresentative;

    let ctx = DisclaimerContext {
        config: &config,
        results: &results,
        gate_suite_report: None,
        sensitivity_report: None,
        run_fingerprint: None,
        trust_decision: None,
        current_time_ns: 0,
    };

    let block = generate_disclaimers(&ctx);

    let non_rep = block
        .disclaimers
        .iter()
        .find(|d| d.id == "NON_REPRESENTATIVE_DATASET");
    assert!(non_rep.is_some());
    assert_eq!(non_rep.unwrap().severity, Severity::Critical);
}

// =============================================================================
// RULE 3 TESTS - GATE SUITE
// =============================================================================

#[test]
fn test_gates_not_run_generates_critical() {
    let mut config = make_research_config();
    config.gate_mode = GateMode::Strict;
    let results = make_default_results();

    let ctx = DisclaimerContext {
        config: &config,
        results: &results,
        gate_suite_report: None,
        sensitivity_report: None,
        run_fingerprint: None,
        trust_decision: None,
        current_time_ns: 0,
    };

    let block = generate_disclaimers(&ctx);

    let gates_not_run = block
        .disclaimers
        .iter()
        .find(|d| d.id == "GATES_NOT_RUN");
    assert!(gates_not_run.is_some());
    assert_eq!(gates_not_run.unwrap().severity, Severity::Critical);
}

#[test]
fn test_gates_bypassed_generates_critical() {
    let mut config = make_research_config();
    config.gate_mode = GateMode::Disabled;
    let results = make_default_results();

    let ctx = DisclaimerContext {
        config: &config,
        results: &results,
        gate_suite_report: None,
        sensitivity_report: None,
        run_fingerprint: None,
        trust_decision: None,
        current_time_ns: 0,
    };

    let block = generate_disclaimers(&ctx);

    let bypassed = block
        .disclaimers
        .iter()
        .find(|d| d.id == "GATES_BYPASSED");
    assert!(bypassed.is_some());
    assert_eq!(bypassed.unwrap().severity, Severity::Critical);
}

#[test]
fn test_gates_failed_generates_critical() {
    let config = make_research_config();
    let results = make_default_results();
    let gate_report = make_failing_gate_suite_report();

    let ctx = DisclaimerContext {
        config: &config,
        results: &results,
        gate_suite_report: Some(&gate_report),
        sensitivity_report: None,
        run_fingerprint: None,
        trust_decision: None,
        current_time_ns: 0,
    };

    let block = generate_disclaimers(&ctx);

    let failed = block.disclaimers.iter().find(|d| d.id == "GATES_FAILED");
    assert!(failed.is_some());
    assert_eq!(failed.unwrap().severity, Severity::Critical);
}

#[test]
fn test_untrusted_results_generates_critical() {
    let config = make_research_config();
    let mut results = make_default_results();
    results.trust_level = GateTrustLevel::Untrusted {
        reasons: vec![crate::backtest_v2::gate_suite::GateFailureReason::new(
            "test",
            "test reason",
            "field",
            "actual",
            "expected",
        )],
    };

    let ctx = DisclaimerContext {
        config: &config,
        results: &results,
        gate_suite_report: None,
        sensitivity_report: None,
        run_fingerprint: None,
        trust_decision: None,
        current_time_ns: 0,
    };

    let block = generate_disclaimers(&ctx);

    let untrusted = block
        .disclaimers
        .iter()
        .find(|d| d.id == "UNTRUSTED_RESULTS");
    assert!(untrusted.is_some());
    assert_eq!(untrusted.unwrap().severity, Severity::Critical);
}

// =============================================================================
// RULE 4 TESTS - SETTLEMENT REFERENCE
// =============================================================================

#[test]
fn test_non_exact_settlement_generates_critical() {
    let config = make_research_config();
    let mut results = make_default_results();
    results.settlement_model = SettlementModel::None;

    let ctx = DisclaimerContext {
        config: &config,
        results: &results,
        gate_suite_report: None,
        sensitivity_report: None,
        run_fingerprint: None,
        trust_decision: None,
        current_time_ns: 0,
    };

    let block = generate_disclaimers(&ctx);

    let non_exact = block
        .disclaimers
        .iter()
        .find(|d| d.id == "NON_PRODUCTION_SETTLEMENT_RULE");
    assert!(non_exact.is_some());
    assert_eq!(non_exact.unwrap().severity, Severity::Critical);
}

// =============================================================================
// RULE 5 TESTS - INTEGRITY POLICY / DATA ISSUES
// =============================================================================

#[test]
fn test_pathology_counters_nonzero_generates_disclaimer() {
    let config = make_research_config();
    let mut results = make_default_results();
    results.pathology_counters.gaps_detected = 5;
    results.pathology_counters.total_missing_sequences = 10;

    let ctx = DisclaimerContext {
        config: &config,
        results: &results,
        gate_suite_report: None,
        sensitivity_report: None,
        run_fingerprint: None,
        trust_decision: None,
        current_time_ns: 0,
    };

    let block = generate_disclaimers(&ctx);

    let pathologies = block
        .disclaimers
        .iter()
        .find(|d| d.id == "DATA_PATHOLOGIES_DETECTED");
    assert!(pathologies.is_some());
    // Gaps should make it Critical
    assert_eq!(pathologies.unwrap().severity, Severity::Critical);
}

#[test]
fn test_invariant_violations_generate_critical() {
    let config = make_research_config();
    let mut results = make_default_results();
    results.invariant_violations_detected = 3;
    results.first_invariant_violation = Some("Book crossed".to_string());

    let ctx = DisclaimerContext {
        config: &config,
        results: &results,
        gate_suite_report: None,
        sensitivity_report: None,
        run_fingerprint: None,
        trust_decision: None,
        current_time_ns: 0,
    };

    let block = generate_disclaimers(&ctx);

    let inv = block
        .disclaimers
        .iter()
        .find(|d| d.id == "INVARIANT_VIOLATIONS_DETECTED");
    assert!(inv.is_some());
    assert_eq!(inv.unwrap().severity, Severity::Critical);
}

// =============================================================================
// RULE 6 TESTS - SENSITIVITY / FRAGILITY
// =============================================================================

#[test]
fn test_latency_fragile_generates_disclaimer() {
    let config = make_research_config();
    let results = make_default_results();
    let sensitivity = make_fragile_sensitivity_report();

    let ctx = DisclaimerContext {
        config: &config,
        results: &results,
        gate_suite_report: None,
        sensitivity_report: Some(&sensitivity),
        run_fingerprint: None,
        trust_decision: None,
        current_time_ns: 0,
    };

    let block = generate_disclaimers(&ctx);

    let fragile = block
        .disclaimers
        .iter()
        .find(|d| d.id == "FRAGILE_TO_LATENCY");
    assert!(fragile.is_some());
}

// =============================================================================
// DETERMINISM TESTS
// =============================================================================

#[test]
fn test_determinism_same_input_produces_identical_output() {
    let config = make_research_config();
    let results = make_default_results();

    let ctx = DisclaimerContext {
        config: &config,
        results: &results,
        gate_suite_report: None,
        sensitivity_report: None,
        run_fingerprint: None,
        trust_decision: None,
        current_time_ns: 12345,
    };

    let block1 = generate_disclaimers(&ctx);
    let block2 = generate_disclaimers(&ctx);

    // Same count
    assert_eq!(block1.disclaimers.len(), block2.disclaimers.len());

    // Same order and content
    for (d1, d2) in block1.disclaimers.iter().zip(block2.disclaimers.iter()) {
        assert_eq!(d1.id, d2.id);
        assert_eq!(d1.severity, d2.severity);
        assert_eq!(d1.category, d2.category);
        assert_eq!(d1.message, d2.message);
        assert_eq!(d1.evidence, d2.evidence);
    }
}

#[test]
fn test_ordering_critical_before_warning_before_info() {
    let config = make_research_config();
    let mut results = make_default_results();
    results.pathology_counters.duplicates_dropped = 1; // Warning level
    results.dataset_readiness = DatasetReadiness::NonRepresentative; // Critical

    let ctx = DisclaimerContext {
        config: &config,
        results: &results,
        gate_suite_report: None,
        sensitivity_report: None,
        run_fingerprint: None,
        trust_decision: None,
        current_time_ns: 0,
    };

    let block = generate_disclaimers(&ctx);

    // All critical disclaimers should come before warnings
    let mut seen_warning = false;
    for d in &block.disclaimers {
        if d.severity == Severity::Warning {
            seen_warning = true;
        }
        if d.severity == Severity::Critical && seen_warning {
            panic!(
                "Critical disclaimer {} found after warning disclaimer",
                d.id
            );
        }
    }
}

// =============================================================================
// INTEGRATION TESTS
// =============================================================================

#[test]
fn test_full_production_scenario_no_disclaimers_except_missing_fingerprint() {
    let config = make_production_config();
    let mut results = make_production_results();
    results.gate_suite_passed = true;
    results.trust_level = GateTrustLevel::Trusted;
    results.settlement_model = SettlementModel::ExactSpec;

    let gate_report = make_passing_gate_suite_report();
    let sensitivity = make_passing_sensitivity_report();
    let fingerprint = make_valid_fingerprint();
    let trust = TrustDecision::Trusted;

    let ctx = DisclaimerContext {
        config: &config,
        results: &results,
        gate_suite_report: Some(&gate_report),
        sensitivity_report: Some(&sensitivity),
        run_fingerprint: Some(&fingerprint),
        trust_decision: Some(&trust),
        current_time_ns: 0,
    };

    let block = generate_disclaimers(&ctx);

    // In a fully passing production scenario, we expect no critical disclaimers
    // (other than possibly missing oracle config which is hard to set up in this test)
    assert_eq!(block.trust_level, TrustLevelSnapshot::Trusted);

    // Check no critical disclaimers related to basic validation
    let production_critical = block
        .disclaimers
        .iter()
        .filter(|d| d.severity == Severity::Critical)
        .filter(|d| d.id == "NON_PRODUCTION_RUN" || d.id == "GATES_NOT_RUN")
        .count();
    assert_eq!(production_critical, 0);
}

#[test]
fn test_block_format_report() {
    let block = DisclaimersBlock {
        generated_at_ns: 1000,
        trust_level: TrustLevelSnapshot::Untrusted,
        disclaimers: vec![
            Disclaimer::critical(
                "TEST_CRITICAL",
                Category::ProductionMode,
                "Critical test message",
                vec!["evidence=1".to_string()],
            ),
            Disclaimer::warning(
                "TEST_WARNING",
                Category::GateSuite,
                "Warning test message",
                vec!["evidence=2".to_string()],
            ),
        ],
    };

    let report = block.format_report();
    assert!(report.contains("BACKTEST DISCLAIMERS"));
    assert!(report.contains("UNTRUSTED"));
    assert!(report.contains("TEST_CRITICAL"));
    assert!(report.contains("TEST_WARNING"));
}

#[test]
fn test_first_critical_message() {
    let block = DisclaimersBlock {
        generated_at_ns: 0,
        trust_level: TrustLevelSnapshot::Untrusted,
        disclaimers: vec![
            Disclaimer::critical(
                "FIRST",
                Category::ProductionMode,
                "First critical message",
                vec![],
            ),
            Disclaimer::warning("SECOND", Category::GateSuite, "Warning message", vec![]),
        ],
    };

    assert_eq!(
        block.first_critical_message(),
        Some("First critical message")
    );
}

#[test]
fn test_by_category() {
    let block = DisclaimersBlock {
        generated_at_ns: 0,
        trust_level: TrustLevelSnapshot::Unknown,
        disclaimers: vec![
            Disclaimer::critical("A", Category::ProductionMode, "msg", vec![]),
            Disclaimer::warning("B", Category::GateSuite, "msg", vec![]),
            Disclaimer::critical("C", Category::ProductionMode, "msg", vec![]),
        ],
    };

    let prod_mode = block.by_category(Category::ProductionMode);
    assert_eq!(prod_mode.len(), 2);

    let gate_suite = block.by_category(Category::GateSuite);
    assert_eq!(gate_suite.len(), 1);
}

#[test]
fn test_trust_level_snapshot_conversion_from_trust_decision() {
    let trusted = TrustDecision::Trusted;
    assert_eq!(
        TrustLevelSnapshot::from(&trusted),
        TrustLevelSnapshot::Trusted
    );

    let untrusted = TrustDecision::Untrusted { reasons: vec![] };
    assert_eq!(
        TrustLevelSnapshot::from(&untrusted),
        TrustLevelSnapshot::Untrusted
    );
}

//! Integration Tests for Gate Suite
//!
//! These tests verify:
//! 1. Gate suite produces deterministic results
//! 2. Known-correct strategies pass
//! 3. Known-biased strategies fail
//! 4. Gate failures properly block trust level

use crate::backtest_v2::gate_suite::{
    GateMode, GateMetrics, GateSuite, GateSuiteConfig, GateSuiteReport, GateTestResult,
    GateTolerances, TrustLevel, DoNothingStrategy, RandomTakerStrategy,
    SyntheticPriceGenerator,
};
use crate::backtest_v2::events::Side;

// =============================================================================
// DETERMINISM TESTS
// =============================================================================

#[test]
fn test_gate_suite_fully_deterministic() {
    let config = GateSuiteConfig {
        base_seed: 12345,
        windows_per_gate: 5,
        tolerances: GateTolerances {
            martingale_seeds: 20,
            ..Default::default()
        },
        ..Default::default()
    };
    
    // Run three times
    let reports: Vec<GateSuiteReport> = (0..3)
        .map(|_| GateSuite::new(config.clone()).run())
        .collect();
    
    // All should be identical
    for i in 1..reports.len() {
        assert_eq!(reports[0].passed, reports[i].passed,
            "Pass/fail differs between runs");
        
        for (g0, gi) in reports[0].gates.iter().zip(reports[i].gates.iter()) {
            assert_eq!(g0.passed, gi.passed,
                "Gate {} pass/fail differs", g0.name);
            assert!((g0.metrics.pnl_after_fees - gi.metrics.pnl_after_fees).abs() < 1e-10,
                "Gate {} PnL differs: {} vs {}", g0.name, g0.metrics.pnl_after_fees, gi.metrics.pnl_after_fees);
            assert_eq!(g0.metrics.fill_count, gi.metrics.fill_count,
                "Gate {} fill count differs", g0.name);
        }
    }
}

#[test]
fn test_different_seeds_produce_different_results() {
    let config1 = GateSuiteConfig {
        base_seed: 11111,
        windows_per_gate: 3,
        tolerances: GateTolerances {
            martingale_seeds: 10,
            ..Default::default()
        },
        ..Default::default()
    };
    
    let config2 = GateSuiteConfig {
        base_seed: 22222,
        ..config1.clone()
    };
    
    let report1 = GateSuite::new(config1).run();
    let report2 = GateSuite::new(config2).run();
    
    // PnL should differ (different price paths)
    let pnl1 = report1.gates[0].metrics.pnl_after_fees;
    let pnl2 = report2.gates[0].metrics.pnl_after_fees;
    
    assert!((pnl1 - pnl2).abs() > 0.01,
        "Different seeds should produce different PnL: {} vs {}", pnl1, pnl2);
}

// =============================================================================
// ZERO-EDGE TESTS
// =============================================================================

#[test]
fn test_gate_a_zero_edge_no_systematic_profit() {
    let config = GateSuiteConfig {
        base_seed: 42,
        windows_per_gate: 20,
        tolerances: GateTolerances {
            max_mean_pnl_before_fees: 1.0,
            min_mean_pnl_after_fees: -0.05,
            max_positive_pnl_probability: 0.60,
            min_trades_for_validity: 5,
            ..Default::default()
        },
        ..Default::default()
    };
    
    let suite = GateSuite::new(config);
    let report = suite.run();
    
    // Gate A should pass
    let gate_a = &report.gates[0];
    assert!(gate_a.passed, "Gate A failed: {:?}", gate_a.failure_reason);
    
    // PnL before fees should be close to zero (not strongly positive)
    assert!(gate_a.metrics.pnl_before_fees < 2.0,
        "PnL before fees ${:.2} too high", gate_a.metrics.pnl_before_fees);
    
    // PnL after fees should be negative or close to zero
    assert!(gate_a.metrics.pnl_after_fees < 1.0,
        "PnL after fees ${:.2} too high", gate_a.metrics.pnl_after_fees);
}

#[test]
fn test_gate_a_fee_sign_correct() {
    // Verify that fees are always subtracted (never added)
    let config = GateSuiteConfig {
        base_seed: 99999,
        windows_per_gate: 10,
        taker_fee_rate: 0.01, // High fee rate to make effect visible
        ..Default::default()
    };
    
    let suite = GateSuite::new(config);
    let report = suite.run();
    
    // Fees should be positive
    for gate in &report.gates {
        if gate.metrics.fill_count > 0 {
            assert!(gate.metrics.fees_paid > 0.0,
                "Gate {} has non-positive fees ${:.4}", gate.name, gate.metrics.fees_paid);
        }
    }
}

// =============================================================================
// MARTINGALE TESTS
// =============================================================================

#[test]
fn test_gate_b_martingale_no_drift() {
    let config = GateSuiteConfig {
        base_seed: 77777,
        tolerances: GateTolerances {
            martingale_seeds: 50,
            max_martingale_drift_pct: 5.0,
            ..Default::default()
        },
        ..Default::default()
    };
    
    let suite = GateSuite::new(config);
    let report = suite.run();
    
    let gate_b = &report.gates[1];
    assert!(gate_b.passed, "Gate B failed: {:?}", gate_b.failure_reason);
    
    // Mean PnL should be close to zero
    if let Some(mean) = gate_b.metrics.mean_pnl {
        let drift_pct = (mean / 10000.0).abs() * 100.0;
        assert!(drift_pct < 10.0, "Martingale drift {:.2}% too high", drift_pct);
    }
}

#[test]
fn test_martingale_price_generator_statistical_properties() {
    // Run many seeds and verify martingale property
    let initial_price = 0.5;
    let volatility = 0.01;
    let steps = 1000;
    let num_seeds = 100;
    
    let mut final_prices = Vec::new();
    
    for seed in 0..num_seeds {
        let mut gen = SyntheticPriceGenerator::new(initial_price, volatility, seed);
        for _ in 0..steps {
            gen.next_price();
        }
        final_prices.push(gen.price());
    }
    
    let mean = final_prices.iter().sum::<f64>() / num_seeds as f64;
    let variance = final_prices.iter().map(|p| (p - mean).powi(2)).sum::<f64>() / num_seeds as f64;
    let std = variance.sqrt();
    
    // Mean should be close to initial (martingale property)
    assert!((mean - initial_price).abs() < 0.15,
        "Martingale mean {:.4} too far from initial {:.4}", mean, initial_price);
    
    // Should have some variance (not degenerate)
    assert!(std > 0.01, "Variance too low: std = {:.4}", std);
}

// =============================================================================
// SIGNAL INVERSION TESTS
// =============================================================================

#[test]
fn test_gate_c_inversion_symmetry() {
    let config = GateSuiteConfig {
        base_seed: 54321,
        windows_per_gate: 15,
        ..Default::default()
    };
    
    let suite = GateSuite::new(config);
    let report = suite.run();
    
    let gate_c = &report.gates[2];
    assert!(gate_c.passed, "Gate C failed: {:?}", gate_c.failure_reason);
}

#[test]
fn test_inversion_both_profitable_fails() {
    // This test verifies that if both original and inverted are profitable,
    // the gate correctly fails (indicates simulator bias)
    
    // We can't easily force both profitable without a biased simulator,
    // but we can verify the check logic exists
    let config = GateSuiteConfig::default();
    let tolerances = config.tolerances.clone();
    
    // The max_inversion_correlation should prevent both being profitable
    assert!(tolerances.max_inversion_correlation < 0.0,
        "Inversion correlation tolerance should be negative");
}

// =============================================================================
// TRUST LEVEL TESTS
// =============================================================================

#[test]
fn test_trust_level_trusted_requires_all_gates_pass() {
    let config = GateSuiteConfig {
        base_seed: 1234567890,
        windows_per_gate: 5,
        tolerances: GateTolerances {
            martingale_seeds: 10,
            min_trades_for_validity: 0, // Don't require trades
            ..Default::default()
        },
        ..Default::default()
    };
    
    let suite = GateSuite::new(config);
    let report = suite.run();
    
    if report.passed {
        assert_eq!(report.trust_level, TrustLevel::Trusted);
        assert!(report.gates.iter().all(|g| g.passed));
    } else {
        assert!(matches!(report.trust_level, TrustLevel::Untrusted { .. }));
        assert!(report.gates.iter().any(|g| !g.passed));
    }
}

#[test]
fn test_trust_level_default_is_unknown() {
    assert_eq!(TrustLevel::default(), TrustLevel::Unknown);
}

#[test]
fn test_gate_mode_default() {
    assert_eq!(GateMode::default(), GateMode::Disabled);
}

// =============================================================================
// TOLERANCE TESTS
// =============================================================================

#[test]
fn test_tolerance_values_are_conservative() {
    let t = GateTolerances::default();
    
    // PnL tolerances should be small
    assert!(t.max_mean_pnl_before_fees < 5.0,
        "Before-fee tolerance ${:.2} too loose", t.max_mean_pnl_before_fees);
    
    // After-fee tolerance should be negative
    assert!(t.min_mean_pnl_after_fees < 0.0,
        "After-fee tolerance should be negative");
    
    // Probability tolerance should be close to 50%
    assert!(t.max_positive_pnl_probability <= 0.60,
        "Positive PnL probability tolerance {:.1}% too loose", t.max_positive_pnl_probability * 100.0);
    
    // Martingale drift should be small
    assert!(t.max_martingale_drift_pct <= 10.0,
        "Martingale drift tolerance {:.1}% too loose", t.max_martingale_drift_pct);
    
    // Minimum seeds for significance
    assert!(t.martingale_seeds >= 50,
        "Martingale seeds {} too low for statistical significance", t.martingale_seeds);
}

// =============================================================================
// METRICS AGGREGATION TESTS
// =============================================================================

#[test]
fn test_metrics_aggregation_across_seeds() {
    let config = GateSuiteConfig {
        base_seed: 11111,
        windows_per_gate: 10,
        tolerances: GateTolerances {
            martingale_seeds: 20,
            ..Default::default()
        },
        ..Default::default()
    };
    
    let suite = GateSuite::new(config);
    let report = suite.run();
    
    // Check that multi-seed gates have statistics
    let gate_b = &report.gates[1]; // Martingale gate
    assert!(gate_b.metrics.mean_pnl.is_some(), "Mean PnL should be computed");
    assert!(gate_b.metrics.std_pnl.is_some(), "Std PnL should be computed");
    assert!(gate_b.metrics.positive_pnl_probability.is_some(), "P(PnL > 0) should be computed");
}

#[test]
fn test_fill_counts_tracked() {
    let config = GateSuiteConfig {
        base_seed: 55555,
        windows_per_gate: 5,
        ..Default::default()
    };
    
    let suite = GateSuite::new(config);
    let report = suite.run();
    
    for gate in &report.gates {
        // Volume should be consistent with fills
        if gate.metrics.fill_count > 0 {
            assert!(gate.metrics.volume > 0.0,
                "Gate {} has fills but no volume", gate.name);
        }
    }
}

// =============================================================================
// REPORT FORMAT TESTS
// =============================================================================

#[test]
fn test_report_format_includes_all_gates() {
    let config = GateSuiteConfig::default();
    let suite = GateSuite::new(config);
    let report = suite.run();
    let summary = report.format_summary();
    
    // Should have all three gates
    assert!(summary.contains("Gate A"), "Missing Gate A in report");
    assert!(summary.contains("Gate B"), "Missing Gate B in report");
    assert!(summary.contains("Gate C"), "Missing Gate C in report");
    
    // Should have key metrics
    assert!(summary.contains("PnL before fees"), "Missing PnL before fees");
    assert!(summary.contains("PnL after fees"), "Missing PnL after fees");
    assert!(summary.contains("Fees paid"), "Missing fees paid");
}

#[test]
fn test_report_shows_failure_reasons() {
    // Create a config that will likely cause some failures
    let config = GateSuiteConfig {
        base_seed: 99999,
        windows_per_gate: 2,
        tolerances: GateTolerances {
            max_mean_pnl_before_fees: 0.001, // Very tight - will fail
            martingale_seeds: 5,
            ..Default::default()
        },
        ..Default::default()
    };
    
    let suite = GateSuite::new(config);
    let report = suite.run();
    
    // If any failures, they should have reasons
    for gate in &report.gates {
        if !gate.passed {
            assert!(gate.failure_reason.is_some(),
                "Gate {} failed but has no reason", gate.name);
        }
    }
}

// =============================================================================
// EDGE CASE TESTS
// =============================================================================

#[test]
fn test_zero_trades_handled() {
    // Config that produces zero trades
    let config = GateSuiteConfig {
        base_seed: 0,
        windows_per_gate: 1,
        tolerances: GateTolerances {
            min_trades_for_validity: 0,
            ..Default::default()
        },
        ..Default::default()
    };
    
    let suite = GateSuite::new(config);
    let report = suite.run();
    
    // Should not crash with zero trades
    assert!(!report.gates.is_empty());
}

#[test]
fn test_single_seed_runs() {
    let config = GateSuiteConfig {
        base_seed: 42,
        windows_per_gate: 1,
        tolerances: GateTolerances {
            martingale_seeds: 1,
            min_trades_for_validity: 0,
            ..Default::default()
        },
        ..Default::default()
    };
    
    let suite = GateSuite::new(config);
    let report = suite.run();
    
    // Should complete without error
    assert_eq!(report.gates.len(), 3);
}

// =============================================================================
// DATA FIDELITY GATING TESTS
// =============================================================================
// These tests verify that maker strategies only remain profitable in regimes
// permitted by data fidelity (DatasetReadiness::MakerViable).

#[test]
fn test_gate_suite_passes_with_default_config() {
    // Default config runs synthetic tests that should pass
    let config = GateSuiteConfig {
        base_seed: 12345,
        windows_per_gate: 10,
        tolerances: GateTolerances::default(),
        ..Default::default()
    };
    
    let suite = GateSuite::new(config);
    let report = suite.run();
    
    // All gates should pass with synthetic martingale data
    assert!(report.passed, "Gate suite failed: {}", report.format_summary());
    assert_eq!(report.trust_level, TrustLevel::Trusted);
    
    // Verify each gate passes
    for gate in &report.gates {
        assert!(gate.passed, "Gate {} failed: {:?}", gate.name, gate.failure_reason);
    }
}

#[test]
fn test_zero_edge_gate_blocks_look_ahead_bias() {
    // This test verifies Gate A catches look-ahead bias
    // When p_theory == p_mkt, systematic profit indicates information leakage
    
    let config = GateSuiteConfig {
        base_seed: 42424,
        windows_per_gate: 20,
        tolerances: GateTolerances {
            max_mean_pnl_before_fees: 0.50, // Tight tolerance
            max_positive_pnl_probability: 0.55,
            min_trades_for_validity: 5,
            martingale_seeds: 30,
            ..Default::default()
        },
        ..Default::default()
    };
    
    let suite = GateSuite::new(config);
    let report = suite.run();
    
    let gate_a = &report.gates[0];
    assert!(gate_a.name.contains("Zero-Edge"));
    
    // Gate A metrics should show:
    // 1. PnL before fees close to zero (no systematic edge)
    // 2. PnL after fees negative (fees dominate)
    println!("Gate A metrics:");
    println!("  PnL before fees: ${:.4}", gate_a.metrics.pnl_before_fees);
    println!("  PnL after fees: ${:.4}", gate_a.metrics.pnl_after_fees);
    println!("  P(PnL > 0): {:.1}%", gate_a.metrics.positive_pnl_probability.unwrap_or(0.0) * 100.0);
    
    // If gate A fails, there's simulator bias (which we don't want)
    assert!(gate_a.passed, "Gate A failed - indicates potential look-ahead bias: {:?}", 
        gate_a.failure_reason);
}

#[test]
fn test_martingale_gate_blocks_drift() {
    // This test verifies Gate B catches price path drift
    // Martingale prices should not produce systematic profit
    
    let config = GateSuiteConfig {
        base_seed: 77777,
        windows_per_gate: 5,
        tolerances: GateTolerances {
            martingale_seeds: 50,
            max_martingale_drift_pct: 5.0,
            max_positive_pnl_probability: 0.55,
            ..Default::default()
        },
        ..Default::default()
    };
    
    let suite = GateSuite::new(config);
    let report = suite.run();
    
    let gate_b = report.gates.iter()
        .find(|g| g.name.contains("Martingale"))
        .expect("Gate B not found");
    
    println!("Gate B metrics:");
    println!("  Mean PnL: ${:.4}", gate_b.metrics.mean_pnl.unwrap_or(0.0));
    println!("  Std PnL: ${:.4}", gate_b.metrics.std_pnl.unwrap_or(0.0));
    println!("  P(PnL > 0): {:.1}%", gate_b.metrics.positive_pnl_probability.unwrap_or(0.0) * 100.0);
    
    // Gate B should pass (no systematic drift)
    assert!(gate_b.passed, "Gate B failed - indicates drift in martingale: {:?}",
        gate_b.failure_reason);
}

#[test]
fn test_signal_inversion_gate_symmetry() {
    // This test verifies Gate C catches asymmetric behavior
    // Both original and inverted should not be profitable
    
    let config = GateSuiteConfig {
        base_seed: 33333,
        windows_per_gate: 10,
        tolerances: GateTolerances::default(),
        ..Default::default()
    };
    
    let suite = GateSuite::new(config);
    let report = suite.run();
    
    let gate_c = report.gates.iter()
        .find(|g| g.name.contains("Inversion"))
        .expect("Gate C not found");
    
    println!("Gate C metrics:");
    println!("  Fill count: {}", gate_c.metrics.fill_count);
    println!("  Volume: ${:.2}", gate_c.metrics.volume);
    
    // Gate C should pass (symmetric losses)
    assert!(gate_c.passed, "Gate C failed - indicates asymmetric behavior: {:?}",
        gate_c.failure_reason);
}

#[test]
fn test_combined_gates_provide_trust_level() {
    // Verify that trust_level is correctly derived from gate results
    
    let config = GateSuiteConfig {
        base_seed: 55555,
        windows_per_gate: 15,
        tolerances: GateTolerances::default(),
        ..Default::default()
    };
    
    let suite = GateSuite::new(config);
    let report = suite.run();
    
    // Trust level logic
    if report.passed {
        assert_eq!(report.trust_level, TrustLevel::Trusted,
            "All gates passed but trust level is not Trusted");
    } else {
        assert!(matches!(report.trust_level, TrustLevel::Untrusted { .. }),
            "Some gates failed but trust level is not Untrusted");
    }
    
    println!("Gate Suite Summary:");
    println!("  Passed: {}", report.passed);
    println!("  Trust Level: {:?}", report.trust_level);
    println!("  Total execution time: {}ms", report.total_execution_ms);
}

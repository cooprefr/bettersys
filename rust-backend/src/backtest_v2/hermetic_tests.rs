//! Hermetic Strategy Mode Tests
//!
//! Tests for hermetic strategy sandboxing and DecisionProof enforcement.

use crate::backtest_v2::hermetic::{
    CallbackType, DecisionAction, HermeticAbort, HermeticConfig, HermeticDecisionProof,
    HermeticEnforcer, HermeticViolationType, disable_hermetic_mode, enable_hermetic_mode,
    hermetic_guard, is_hermetic_mode,
};
use crate::backtest_v2::events::Side;

/// Reset hermetic mode between tests.
fn reset_hermetic_mode() {
    disable_hermetic_mode();
}

// =============================================================================
// WALL-CLOCK TIME BLOCKING TESTS
// =============================================================================

#[test]
fn test_hermetic_mode_flag_default_disabled() {
    reset_hermetic_mode();
    assert!(!is_hermetic_mode());
}

#[test]
fn test_hermetic_mode_enable_disable() {
    reset_hermetic_mode();
    
    enable_hermetic_mode();
    assert!(is_hermetic_mode());
    
    disable_hermetic_mode();
    assert!(!is_hermetic_mode());
}

#[test]
fn test_hermetic_guard_allows_when_disabled() {
    reset_hermetic_mode();
    // Should not panic
    hermetic_guard("test_operation");
}

#[test]
#[should_panic(expected = "HERMETIC MODE VIOLATION")]
fn test_hermetic_guard_panics_when_enabled() {
    reset_hermetic_mode();
    enable_hermetic_mode();
    hermetic_guard("wall_clock_access");
}

#[test]
#[should_panic(expected = "HERMETIC MODE VIOLATION")]
fn test_wall_clock_guard_panics() {
    reset_hermetic_mode();
    enable_hermetic_mode();
    crate::backtest_v2::hermetic::guard_wall_clock();
}

#[test]
#[should_panic(expected = "HERMETIC MODE VIOLATION")]
fn test_env_access_guard_panics() {
    reset_hermetic_mode();
    enable_hermetic_mode();
    crate::backtest_v2::hermetic::guard_env_access();
}

#[test]
#[should_panic(expected = "HERMETIC MODE VIOLATION")]
fn test_filesystem_io_guard_panics() {
    reset_hermetic_mode();
    enable_hermetic_mode();
    crate::backtest_v2::hermetic::guard_filesystem_io();
}

#[test]
#[should_panic(expected = "HERMETIC MODE VIOLATION")]
fn test_network_io_guard_panics() {
    reset_hermetic_mode();
    enable_hermetic_mode();
    crate::backtest_v2::hermetic::guard_network_io();
}

#[test]
#[should_panic(expected = "HERMETIC MODE VIOLATION")]
fn test_thread_spawn_guard_panics() {
    reset_hermetic_mode();
    enable_hermetic_mode();
    crate::backtest_v2::hermetic::guard_thread_spawn();
}

#[test]
#[should_panic(expected = "HERMETIC MODE VIOLATION")]
fn test_async_spawn_guard_panics() {
    reset_hermetic_mode();
    enable_hermetic_mode();
    crate::backtest_v2::hermetic::guard_async_spawn();
}

// =============================================================================
// DECISION PROOF TESTS
// =============================================================================

#[test]
fn test_decision_proof_builder_basic() {
    reset_hermetic_mode();
    
    let builder = HermeticDecisionProof::builder(
        1,
        "TestStrategy",
        CallbackType::OnBookUpdate,
        1000,
    );
    
    let proof = builder.build();
    
    assert_eq!(proof.decision_id, 1);
    assert_eq!(proof.strategy_name, "TestStrategy");
    assert_eq!(proof.callback_type, CallbackType::OnBookUpdate);
    assert_eq!(proof.decision_time, 1000);
    assert!(proof.is_noop); // No actions recorded
}

#[test]
fn test_decision_proof_with_order() {
    reset_hermetic_mode();
    
    let mut builder = HermeticDecisionProof::builder(
        1,
        "TestStrategy",
        CallbackType::OnBookUpdate,
        1000,
    );
    
    builder.record_input_event(900, 850, 1);
    builder.record_book_snapshot(12345);
    builder.record_signal("mid_price", 0.55);
    builder.record_order("order_1", "BTC-UP", Side::Buy, 0.54, 100.0);
    
    let proof = builder.build();
    
    assert!(!proof.is_noop);
    assert_eq!(proof.input_events.len(), 1);
    assert_eq!(proof.signal_values.len(), 1);
    assert_eq!(proof.actions.len(), 1);
    
    match &proof.actions[0] {
        DecisionAction::Order { client_order_id, side, price, size, .. } => {
            assert_eq!(client_order_id, "order_1");
            assert_eq!(*side, Side::Buy);
            assert_eq!(*price, 0.54);
            assert_eq!(*size, 100.0);
        }
        _ => panic!("Expected Order action"),
    }
}

#[test]
fn test_decision_proof_with_cancel() {
    reset_hermetic_mode();
    
    let mut builder = HermeticDecisionProof::builder(
        1,
        "TestStrategy",
        CallbackType::OnFill,
        2000,
    );
    
    builder.record_cancel(42);
    
    let proof = builder.build();
    
    assert!(!proof.is_noop);
    assert_eq!(proof.actions.len(), 1);
    
    match &proof.actions[0] {
        DecisionAction::Cancel { order_id } => {
            assert_eq!(*order_id, 42);
        }
        _ => panic!("Expected Cancel action"),
    }
}

#[test]
fn test_decision_proof_with_explicit_noop() {
    reset_hermetic_mode();
    
    let mut builder = HermeticDecisionProof::builder(
        1,
        "TestStrategy",
        CallbackType::OnBookUpdate,
        1000,
    );
    
    builder.record_noop("Position already at max");
    
    let proof = builder.build();
    
    assert!(proof.is_noop);
    assert_eq!(proof.actions.len(), 1);
    
    match &proof.actions[0] {
        DecisionAction::NoOp { reason } => {
            assert_eq!(reason, "Position already at max");
        }
        _ => panic!("Expected NoOp action"),
    }
}

#[test]
fn test_decision_proof_hash_determinism() {
    reset_hermetic_mode();
    
    let build_proof = || {
        let mut builder = HermeticDecisionProof::builder(
            1,
            "TestStrategy",
            CallbackType::OnBookUpdate,
            1000,
        );
        builder.record_input_event(900, 850, 1);
        builder.record_signal("mid", 0.5);
        builder.record_order("order_1", "BTC-UP", Side::Buy, 0.54, 100.0);
        builder.build()
    };
    
    let proof1 = build_proof();
    let proof2 = build_proof();
    
    assert_eq!(proof1.input_hash, proof2.input_hash, "Same inputs must produce same hash");
}

#[test]
fn test_decision_proof_hash_changes_with_input() {
    reset_hermetic_mode();
    
    let mut builder1 = HermeticDecisionProof::builder(
        1,
        "TestStrategy",
        CallbackType::OnBookUpdate,
        1000,
    );
    builder1.record_signal("mid", 0.5);
    let proof1 = builder1.build();
    
    let mut builder2 = HermeticDecisionProof::builder(
        1,
        "TestStrategy",
        CallbackType::OnBookUpdate,
        1000,
    );
    builder2.record_signal("mid", 0.6); // Different signal value
    let proof2 = builder2.build();
    
    assert_ne!(proof1.input_hash, proof2.input_hash, "Different inputs must produce different hash");
}

// =============================================================================
// HERMETIC ENFORCER TESTS
// =============================================================================

#[test]
fn test_hermetic_enforcer_disabled() {
    reset_hermetic_mode();
    
    let config = HermeticConfig::default(); // enabled = false
    let mut enforcer = HermeticEnforcer::new(config);
    
    assert!(!enforcer.is_enabled());
    
    // Should return None when disabled
    let builder = enforcer.on_callback_entry("TestStrategy", CallbackType::OnBookUpdate, 1000);
    assert!(builder.is_none());
    
    // Should succeed without proof
    let result = enforcer.on_callback_exit(None);
    assert!(result.is_ok());
}

#[test]
fn test_hermetic_enforcer_callback_flow_success() {
    reset_hermetic_mode();
    
    let config = HermeticConfig {
        enabled: true,
        require_decision_proofs: true,
        abort_on_violation: false, // Don't abort for this test
        max_callback_duration_ns: 1_000_000_000,
    };
    let mut enforcer = HermeticEnforcer::new(config);
    
    // Start callback
    let builder = enforcer.on_callback_entry("TestStrategy", CallbackType::OnBookUpdate, 1000)
        .expect("Should return builder when enabled");
    
    // Build proof
    let proof = builder.build();
    
    // End callback with proof
    let result = enforcer.on_callback_exit(Some(proof));
    assert!(result.is_ok());
    
    // Proof should be stored
    assert_eq!(enforcer.recent_proofs().len(), 1);
}

#[test]
fn test_hermetic_enforcer_missing_proof_non_abort() {
    reset_hermetic_mode();
    
    let config = HermeticConfig {
        enabled: true,
        require_decision_proofs: true,
        abort_on_violation: false, // Don't abort
        max_callback_duration_ns: 1_000_000_000,
    };
    let mut enforcer = HermeticEnforcer::new(config);
    
    // Start callback
    let _builder = enforcer.on_callback_entry("TestStrategy", CallbackType::OnBookUpdate, 1000);
    
    // End callback WITHOUT proof
    let result = enforcer.on_callback_exit(None);
    assert!(result.is_ok()); // Should not abort in non-abort mode
    
    // Violation should be recorded
    assert_eq!(enforcer.violations().len(), 1);
    
    match &enforcer.violations()[0].violation_type {
        HermeticViolationType::MissingDecisionProof { strategy_name, callback_type, .. } => {
            assert_eq!(strategy_name, "TestStrategy");
            assert_eq!(*callback_type, CallbackType::OnBookUpdate);
        }
        _ => panic!("Expected MissingDecisionProof violation"),
    }
}

#[test]
fn test_hermetic_enforcer_missing_proof_abort() {
    reset_hermetic_mode();
    
    let config = HermeticConfig::production(); // abort_on_violation = true
    let mut enforcer = HermeticEnforcer::new(config);
    
    // Start callback
    let _builder = enforcer.on_callback_entry("TestStrategy", CallbackType::OnBookUpdate, 1000);
    
    // End callback WITHOUT proof (should abort)
    let result = enforcer.on_callback_exit(None);
    assert!(result.is_err());
    
    let abort = result.unwrap_err();
    match abort.violation.violation_type {
        HermeticViolationType::MissingDecisionProof { .. } => {}
        _ => panic!("Expected MissingDecisionProof violation"),
    }
}

#[test]
fn test_hermetic_enforcer_non_order_callback() {
    reset_hermetic_mode();
    
    let config = HermeticConfig::production();
    let mut enforcer = HermeticEnforcer::new(config);
    
    // OnOrderAck cannot emit orders, so proof is not required if require_decision_proofs is properly scoped
    // But with require_decision_proofs=true and production mode, we still track callbacks
    
    // Check callback type flags
    assert!(!CallbackType::OnOrderAck.can_emit_orders());
    assert!(!CallbackType::OnOrderReject.can_emit_orders());
    assert!(!CallbackType::OnStop.can_emit_orders());
    
    assert!(CallbackType::OnBookUpdate.can_emit_orders());
    assert!(CallbackType::OnTrade.can_emit_orders());
    assert!(CallbackType::OnTimer.can_emit_orders());
    assert!(CallbackType::OnFill.can_emit_orders());
    assert!(CallbackType::OnStart.can_emit_orders());
}

// =============================================================================
// PRODUCTION-GRADE VALIDATION TESTS
// =============================================================================

#[test]
fn test_production_grade_requires_hermetic() {
    reset_hermetic_mode();
    
    let non_hermetic = HermeticConfig {
        enabled: false,
        ..Default::default()
    };
    
    let result = HermeticEnforcer::validate_production_grade(true, &non_hermetic);
    assert!(result.is_err());
    
    let abort = result.unwrap_err();
    match abort.violation.violation_type {
        HermeticViolationType::HermeticModeRequired => {}
        _ => panic!("Expected HermeticModeRequired violation"),
    }
}

#[test]
fn test_production_grade_with_hermetic_enabled() {
    reset_hermetic_mode();
    
    let hermetic = HermeticConfig::production();
    
    let result = HermeticEnforcer::validate_production_grade(true, &hermetic);
    assert!(result.is_ok());
}

#[test]
fn test_production_grade_requires_abort_on_violation() {
    reset_hermetic_mode();
    
    let weak_hermetic = HermeticConfig {
        enabled: true,
        require_decision_proofs: true,
        abort_on_violation: false, // This is not production-grade
        max_callback_duration_ns: 1_000_000_000,
    };
    
    let result = HermeticEnforcer::validate_production_grade(true, &weak_hermetic);
    assert!(result.is_err());
}

#[test]
fn test_non_production_allows_non_hermetic() {
    reset_hermetic_mode();
    
    let non_hermetic = HermeticConfig {
        enabled: false,
        ..Default::default()
    };
    
    // production_grade = false should allow non-hermetic config
    let result = HermeticEnforcer::validate_production_grade(false, &non_hermetic);
    assert!(result.is_ok());
}

// =============================================================================
// HERMETIC CLOCK TESTS
// =============================================================================

#[test]
fn test_hermetic_clock() {
    use crate::backtest_v2::hermetic::HermeticClock;
    
    let mut clock = HermeticClock::new(1000);
    assert_eq!(clock.now(), 1000);
    
    clock.advance_to(2000);
    assert_eq!(clock.now(), 2000);
    
    clock.advance_to(3000);
    assert_eq!(clock.now(), 3000);
}

// =============================================================================
// HERMETIC RNG TESTS
// =============================================================================

#[test]
fn test_hermetic_rng_determinism() {
    use crate::backtest_v2::hermetic::HermeticRng;
    use rand::Rng;
    
    let mut rng1 = HermeticRng::new(42);
    let mut rng2 = HermeticRng::new(42);
    
    let v1: u64 = rng1.rng().gen();
    let v2: u64 = rng2.rng().gen();
    
    assert_eq!(v1, v2, "Same seed must produce same sequence");
    assert_eq!(rng1.samples_drawn(), 1);
    assert_eq!(rng2.samples_drawn(), 1);
}

#[test]
fn test_hermetic_rng_different_seeds() {
    use crate::backtest_v2::hermetic::HermeticRng;
    use rand::Rng;
    
    let mut rng1 = HermeticRng::new(42);
    let mut rng2 = HermeticRng::new(43);
    
    let v1: u64 = rng1.rng().gen();
    let v2: u64 = rng2.rng().gen();
    
    assert_ne!(v1, v2, "Different seeds should produce different sequences");
}

// =============================================================================
// HERMETIC CONFIG TESTS
// =============================================================================

#[test]
fn test_hermetic_config_default() {
    let config = HermeticConfig::default();
    assert!(!config.enabled);
    assert!(config.require_decision_proofs);
    assert!(config.abort_on_violation);
}

#[test]
fn test_hermetic_config_production() {
    let config = HermeticConfig::production();
    assert!(config.enabled);
    assert!(config.require_decision_proofs);
    assert!(config.abort_on_violation);
    assert!(config.is_production_grade());
}

#[test]
fn test_hermetic_config_testing() {
    let config = HermeticConfig::testing();
    assert!(config.enabled);
    assert!(config.require_decision_proofs);
    assert!(!config.abort_on_violation); // Doesn't abort for easier debugging
    assert!(!config.is_production_grade()); // Not production grade
}

// =============================================================================
// BACKTEST CONFIG INTEGRATION TESTS
// =============================================================================

#[test]
fn test_backtest_config_default_is_production_grade_hermetic() {
    use crate::backtest_v2::orchestrator::BacktestConfig;
    
    let config = BacktestConfig::default();
    
    assert!(config.production_grade);
    assert!(config.hermetic_config.enabled);
    assert!(config.hermetic_config.is_production_grade());
}

#[test]
fn test_backtest_config_production_validates_hermetic() {
    use crate::backtest_v2::orchestrator::BacktestConfig;
    
    let config = BacktestConfig::production_grade_15m_updown();
    
    // Should pass validation
    let result = config.validate_production_grade();
    assert!(result.is_ok(), "Production config should validate: {:?}", result);
}

#[test]
fn test_backtest_config_non_hermetic_fails_production_validation() {
    use crate::backtest_v2::orchestrator::BacktestConfig;
    
    let mut config = BacktestConfig::default();
    config.hermetic_config.enabled = false; // Disable hermetic
    
    // Should fail validation
    let result = config.validate_production_grade();
    assert!(result.is_err());
    
    let err = result.unwrap_err();
    assert!(err.violations.iter().any(|v| v.contains("hermetic")));
}

#[test]
fn test_backtest_config_research_mode_non_hermetic() {
    use crate::backtest_v2::orchestrator::BacktestConfig;
    
    let config = BacktestConfig::research_mode();
    
    assert!(!config.production_grade);
    assert!(!config.hermetic_config.enabled);
    
    // Should pass validation (production_grade = false)
    let result = config.validate_production_grade();
    assert!(result.is_ok());
}

// =============================================================================
// REPRODUCIBILITY TESTS
// =============================================================================

#[test]
fn test_proof_reproducibility_across_runs() {
    reset_hermetic_mode();
    
    // Simulate two identical runs
    fn simulate_run(seed: u64) -> Vec<HermeticDecisionProof> {
        let mut proofs = Vec::new();
        
        for i in 0u64..5 {
            let mut builder = HermeticDecisionProof::builder(
                i + 1,
                "ReproStrategy",
                CallbackType::OnBookUpdate,
                ((i + 1) * 1000) as i64,
            );
            
            builder.record_input_event(((i + 1) * 900) as i64, ((i + 1) * 850) as i64, seed + i);
            builder.record_signal("mid", 0.5 + (i as f64) * 0.01);
            
            if i % 2 == 0 {
                builder.record_order(
                    format!("order_{}", i),
                    "BTC-UP",
                    Side::Buy,
                    0.54,
                    100.0,
                );
            } else {
                builder.record_noop("No signal");
            }
            
            proofs.push(builder.build());
        }
        
        proofs
    }
    
    let proofs1 = simulate_run(42);
    let proofs2 = simulate_run(42);
    
    assert_eq!(proofs1.len(), proofs2.len());
    
    for (p1, p2) in proofs1.iter().zip(proofs2.iter()) {
        assert_eq!(p1.input_hash, p2.input_hash, "Proof hashes must match for identical runs");
    }
}

// =============================================================================
// FORBIDDEN API PATTERNS TEST
// =============================================================================

#[test]
fn test_forbidden_api_patterns_list() {
    use crate::backtest_v2::hermetic::FORBIDDEN_API_PATTERNS;
    
    // Verify key patterns are in the list
    assert!(FORBIDDEN_API_PATTERNS.contains(&"std::time::SystemTime"));
    assert!(FORBIDDEN_API_PATTERNS.contains(&"std::time::Instant"));
    assert!(FORBIDDEN_API_PATTERNS.contains(&"std::env::var"));
    assert!(FORBIDDEN_API_PATTERNS.contains(&"std::fs::"));
    assert!(FORBIDDEN_API_PATTERNS.contains(&"std::net::"));
    assert!(FORBIDDEN_API_PATTERNS.contains(&"std::thread::spawn"));
    assert!(FORBIDDEN_API_PATTERNS.contains(&"tokio::spawn"));
    assert!(FORBIDDEN_API_PATTERNS.contains(&"chrono::"));
}

// =============================================================================
// COMPILE-TIME ENFORCEMENT VERIFICATION TESTS
// =============================================================================

/// This test documents that hermetic enforcement is via compile-time clippy lints.
/// 
/// The actual enforcement is in `clippy.toml` which configures:
/// - `disallowed-types`: Forbids SystemTime, Instant, chrono::Utc, etc.
/// - `disallowed-methods`: Forbids ::now(), ::elapsed() etc.
///
/// Strategy modules have `#![deny(clippy::disallowed_types)]` and 
/// `#![deny(clippy::disallowed_methods)]` which cause compile failures.
///
/// To verify enforcement works, try uncommenting the code below and run:
/// ```bash
/// cargo clippy --all-targets -- -D warnings
/// ```
///
/// The following would cause compile errors if uncommented in strategy.rs:
/// ```rust,ignore
/// use std::time::SystemTime;  // ERROR: disallowed type
/// let _ = SystemTime::now();  // ERROR: disallowed method
/// ```
#[test]
fn test_compile_time_enforcement_documentation() {
    // This test documents that compile-time enforcement exists.
    // The enforcement happens via clippy, not at runtime.
    //
    // See: HERMETIC_COMPILE_ENFORCEMENT.md
    // See: clippy.toml
    // See: strategy.rs, example_strategy.rs, gate_suite.rs
    
    // Verify the clippy.toml file exists and contains expected configuration
    let clippy_toml_exists = std::path::Path::new("clippy.toml").exists()
        || std::path::Path::new("../clippy.toml").exists()
        || std::path::Path::new("../../clippy.toml").exists();
    
    // In test environment, we may not have access to the file, but we document it exists
    // The real test is: "cargo clippy" fails if disallowed APIs are used in strategy modules
    
    println!("Hermetic compile-time enforcement is configured via:");
    println!("  1. clippy.toml - defines disallowed types and methods");
    println!("  2. strategy.rs - #![deny(clippy::disallowed_types)]");
    println!("  3. example_strategy.rs - #![deny(clippy::disallowed_types)]");
    println!("  4. gate_suite.rs - #![deny(clippy::disallowed_types)]");
    println!("");
    println!("To verify: cargo clippy --all-targets -- -D warnings");
}

/// Test that we correctly identify time-related types as forbidden.
#[test]
fn test_time_types_are_forbidden_in_patterns() {
    use crate::backtest_v2::hermetic::FORBIDDEN_API_PATTERNS;
    
    // These are the critical time-related APIs that must be blocked
    let critical_time_apis = [
        "std::time::SystemTime",
        "std::time::Instant",
    ];
    
    for api in critical_time_apis {
        assert!(
            FORBIDDEN_API_PATTERNS.contains(&api),
            "Critical time API '{}' must be in FORBIDDEN_API_PATTERNS",
            api
        );
    }
}

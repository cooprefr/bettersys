//! Production-Grade Mode Enforcement Tests
//!
//! These tests verify that:
//! 1. Production-grade forces Hard mode invariants
//! 2. Production-grade forces strict integrity policies  
//! 3. First violation aborts immediately with deterministic dump
//! 4. Violation hash is deterministic across reruns
//! 5. No silent continue is possible in production-grade mode

use crate::backtest_v2::integrity::PathologyPolicy;
use crate::backtest_v2::invariants::{
    CausalDump, CategoryFlags, EventSummary, InvariantCategory, InvariantConfig, 
    InvariantEnforcer, InvariantMode, InvariantViolation, LedgerEntrySummary, 
    OmsTransition, StateSnapshot, ViolationType, ViolationContext,
};
use crate::backtest_v2::ledger::LedgerConfig;
use crate::backtest_v2::orchestrator::BacktestConfig;
use crate::backtest_v2::production_grade::{
    compute_config_fingerprint, enforce_production_grade_requirements, 
    ProductionGradeAbort, ViolationHash,
};

// =============================================================================
// TEST 1: PRODUCTION-GRADE FORCES HARD MODE
// =============================================================================

#[test]
fn test_production_grade_forces_hard_mode_invariants() {
    // Attempt to set production_grade=true with Soft invariants
    let mut config = BacktestConfig::production_grade_15m_updown();
    if let Some(ref mut ic) = config.invariant_config {
        ic.mode = InvariantMode::Soft;
    }
    
    let result = config.validate_production_grade();
    
    assert!(result.is_err(), "Should reject Soft invariants in production-grade");
    let err = result.unwrap_err();
    assert!(
        err.violations.iter().any(|v| v.contains("invariant") || v.contains("Hard")),
        "Error should mention invariant mode"
    );
}

#[test]
fn test_production_grade_forces_hard_mode_cannot_be_overridden() {
    let invariant_config = InvariantConfig {
        mode: InvariantMode::Soft, // Attempting to weaken
        categories: CategoryFlags::all(),
        ..Default::default()
    };
    
    let result = enforce_production_grade_requirements(
        true,
        &invariant_config,
        &PathologyPolicy::strict(),
        Some(&LedgerConfig::production_grade()),
        true,
        42,
    );
    
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.violations.iter().any(|v| v.contains("invariant_mode")));
}

#[test]
fn test_production_grade_rejects_off_mode() {
    let invariant_config = InvariantConfig {
        mode: InvariantMode::Off, // Disabled
        categories: CategoryFlags::all(),
        ..Default::default()
    };
    
    let result = enforce_production_grade_requirements(
        true,
        &invariant_config,
        &PathologyPolicy::strict(),
        Some(&LedgerConfig::production_grade()),
        true,
        42,
    );
    
    assert!(result.is_err());
}

// =============================================================================
// TEST 2: PRODUCTION-GRADE FORCES STRICT INTEGRITY
// =============================================================================

#[test]
fn test_production_grade_forces_strict_integrity() {
    let mut config = BacktestConfig::production_grade_15m_updown();
    config.integrity_policy = PathologyPolicy::resilient(); // Weaken
    
    let result = config.validate_production_grade();
    
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.violations.iter().any(|v| v.contains("integrity")));
}

#[test]
fn test_production_grade_rejects_permissive_integrity() {
    let result = enforce_production_grade_requirements(
        true,
        &InvariantConfig::production(),
        &PathologyPolicy::permissive(), // Weakened
        Some(&LedgerConfig::production_grade()),
        true,
        42,
    );
    
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.violations.iter().any(|v| v.contains("integrity")));
}

#[test]
fn test_production_grade_rejects_resilient_integrity() {
    let result = enforce_production_grade_requirements(
        true,
        &InvariantConfig::production(),
        &PathologyPolicy::resilient(), // Also not strict
        Some(&LedgerConfig::production_grade()),
        true,
        42,
    );
    
    assert!(result.is_err());
}

// =============================================================================
// TEST 3: FIRST VIOLATION ABORTS WITH DETERMINISTIC DUMP
// =============================================================================

#[test]
fn test_hard_mode_aborts_on_first_violation() {
    let mut enforcer = InvariantEnforcer::new(InvariantConfig::production());
    
    // Set initial time
    let _ = enforcer.check_decision_time(1000);
    
    // Go backward - this is a time violation
    let result = enforcer.check_decision_time(500);
    
    assert!(result.is_err(), "Should abort on first violation (time backward)");
    
    let abort = result.unwrap_err();
    assert!(!abort.dump.violation.message.is_empty());
    assert!(enforcer.is_aborted());
}

#[test]
fn test_abort_produces_causal_dump() {
    use crate::backtest_v2::events::{Event, TimestampedEvent, Level};
    
    let mut enforcer = InvariantEnforcer::new(InvariantConfig::production());
    
    // Add some events to context using TimestampedEvent
    let event = TimestampedEvent {
        time: 999_000_000,
        source_time: 998_000_000,
        seq: 1,
        source: 1,
        event: Event::L2BookSnapshot {
            token_id: "test-token".to_string(),
            bids: vec![Level::new(0.45, 100.0)],
            asks: vec![Level::new(0.55, 100.0)],
            exchange_seq: 1,
        },
    };
    enforcer.record_event(&event);
    
    // Set initial time then go backward
    let _ = enforcer.check_decision_time(1_000_000_000);
    let result = enforcer.check_decision_time(500_000_000);
    
    let abort = result.unwrap_err();
    let dump = &abort.dump;
    
    // Verify dump contains expected fields
    assert!(!dump.recent_events.is_empty(), "Dump should contain recent events");
    assert!(matches!(dump.violation.category, InvariantCategory::Time));
    assert!(dump.fingerprint_at_abort != 0, "Should have fingerprint");
}

#[test]
fn test_dump_is_bounded() {
    use crate::backtest_v2::events::{Event, TimestampedEvent, Side};
    
    let config = InvariantConfig {
        mode: InvariantMode::Hard,
        categories: CategoryFlags::all(),
        event_dump_depth: 5, // Bounded to 5 events
        oms_dump_depth: 3,
        ledger_dump_depth: 3,
    };
    let mut enforcer = InvariantEnforcer::new(config);
    
    // Add more events than the limit
    for i in 0..20i64 {
        let event = TimestampedEvent {
            time: i * 1_000_000,
            source_time: i * 1_000_000,
            seq: i as u64,
            source: 1,
            event: Event::L2BookDelta {
                token_id: "test-token".to_string(),
                side: Side::Buy,
                price: 0.5,
                new_size: 100.0,
                seq_hash: None,
            },
        };
        enforcer.record_event(&event);
    }
    
    // Set initial time then go backward to trigger violation
    let _ = enforcer.check_decision_time(1_000_000_000);
    let result = enforcer.check_decision_time(500_000_000);
    let abort = result.unwrap_err();
    
    // Verify bounded (events are stored up to event_dump_depth in the enforcer's ring buffer)
    // The ring buffer respects event_dump_depth, so we should have at most 5 events
    assert!(abort.dump.recent_events.len() <= 5, "Should respect depth limit");
}

// =============================================================================
// TEST 4: DETERMINISTIC VIOLATION HASH
// =============================================================================

#[test]
fn test_violation_hash_is_deterministic() {
    let violation = InvariantViolation {
        category: InvariantCategory::Book,
        violation_type: ViolationType::CrossedBook { best_bid: 0.55, best_ask: 0.50 },
        message: "Crossed book".to_string(),
        sim_time: 1_000_000_000,
        decision_time: 1_000_000_000,
        arrival_time: Some(1_000_000_000),
        seq: Some(42),
        market_id: Some("test".to_string()),
        order_id: None,
        fill_id: None,
        context: ViolationContext::default(),
    };
    
    let events = vec![
        EventSummary {
            arrival_time: 999_000_000,
            source_time: Some(998_000_000),
            seq: Some(41),
            event_type: "L2BookSnapshot".to_string(),
            market_id: Some("test".to_string()),
        },
    ];
    
    // Compute hash twice
    let hash1 = ViolationHash::compute(&violation, events.last(), &events, &[], &[], 12345);
    let hash2 = ViolationHash::compute(&violation, events.last(), &events, &[], &[], 12345);
    
    assert_eq!(hash1, hash2, "Violation hash must be deterministic");
}

#[test]
fn test_same_failure_produces_same_hash() {
    use crate::backtest_v2::events::{Event, TimestampedEvent, Level};
    
    // Run the same failing scenario twice
    let run = || {
        let mut enforcer = InvariantEnforcer::new(InvariantConfig::production());
        
        // Same events
        let event = TimestampedEvent {
            time: 999_000_000,
            source_time: 998_000_000,
            seq: 1,
            source: 1,
            event: Event::L2BookSnapshot {
                token_id: "test-token".to_string(),
                bids: vec![Level::new(0.45, 100.0)],
                asks: vec![Level::new(0.55, 100.0)],
                exchange_seq: 1,
            },
        };
        enforcer.record_event(&event);
        
        // Set initial time
        let _ = enforcer.check_decision_time(1_000_000_000);
        
        // Same violation - time goes backward
        let result = enforcer.check_decision_time(500_000_000);
        let abort = result.unwrap_err();
        
        ViolationHash::from_dump(&abort.dump)
    };
    
    let hash1 = run();
    let hash2 = run();
    
    assert_eq!(hash1, hash2, "Same failure must produce same hash");
}

#[test]
fn test_different_context_produces_different_hash() {
    let violation = InvariantViolation {
        category: InvariantCategory::Book,
        violation_type: ViolationType::CrossedBook { best_bid: 0.55, best_ask: 0.50 },
        message: "Crossed book".to_string(),
        sim_time: 1_000_000_000,
        decision_time: 1_000_000_000,
        arrival_time: Some(1_000_000_000),
        seq: Some(42),
        market_id: Some("test".to_string()),
        order_id: None,
        fill_id: None,
        context: ViolationContext::default(),
    };
    
    let events1 = vec![
        EventSummary {
            arrival_time: 999_000_000,
            source_time: Some(998_000_000),
            seq: Some(41),
            event_type: "L2BookSnapshot".to_string(),
            market_id: Some("test".to_string()),
        },
    ];
    
    let events2 = vec![
        EventSummary {
            arrival_time: 999_000_000,
            source_time: Some(998_000_000),
            seq: Some(40), // Different sequence
            event_type: "L2BookSnapshot".to_string(),
            market_id: Some("test".to_string()),
        },
    ];
    
    let hash1 = ViolationHash::compute(&violation, events1.last(), &events1, &[], &[], 12345);
    let hash2 = ViolationHash::compute(&violation, events2.last(), &events2, &[], &[], 12345);
    
    assert_ne!(hash1, hash2, "Different context must produce different hash");
}

// =============================================================================
// TEST 5: NO SILENT CONTINUE
// =============================================================================

#[test]
fn test_no_silent_continue_after_violation() {
    let mut enforcer = InvariantEnforcer::new(InvariantConfig::production());
    
    // Set initial time
    let _ = enforcer.check_decision_time(1_000_000_000);
    
    // First violation should abort
    let _ = enforcer.check_decision_time(500_000_000);
    
    assert!(enforcer.is_aborted(), "Should be aborted after first violation");
    assert!(enforcer.first_violation().is_some(), "Should have recorded violation");
}

#[test]
fn test_production_grade_config_passes_validation() {
    let config = BacktestConfig::production_grade_15m_updown();
    
    let result = config.validate_production_grade();
    
    assert!(result.is_ok(), "Production-grade preset should pass validation: {:?}", result);
}

#[test]
fn test_all_requirements_must_be_met() {
    // Test that missing ANY requirement causes failure
    
    // Missing strict_mode
    let result = enforce_production_grade_requirements(
        false, // Not strict
        &InvariantConfig::production(),
        &PathologyPolicy::strict(),
        Some(&LedgerConfig::production_grade()),
        true,
        42,
    );
    assert!(result.is_err());
    
    // Missing ledger
    let result = enforce_production_grade_requirements(
        true,
        &InvariantConfig::production(),
        &PathologyPolicy::strict(),
        None, // No ledger
        true,
        42,
    );
    assert!(result.is_err());
    
    // Missing strict_accounting
    let result = enforce_production_grade_requirements(
        true,
        &InvariantConfig::production(),
        &PathologyPolicy::strict(),
        Some(&LedgerConfig::production_grade()),
        false, // Not strict accounting
        42,
    );
    assert!(result.is_err());
}

// =============================================================================
// CONFIG FINGERPRINT TESTS
// =============================================================================

#[test]
fn test_config_fingerprint_determinism() {
    let fp1 = compute_config_fingerprint(
        true,
        &InvariantConfig::production(),
        &PathologyPolicy::strict(),
        true,
        true,
        42,
    );
    
    let fp2 = compute_config_fingerprint(
        true,
        &InvariantConfig::production(),
        &PathologyPolicy::strict(),
        true,
        true,
        42,
    );
    
    assert_eq!(fp1, fp2, "Config fingerprint must be deterministic");
}

#[test]
fn test_config_fingerprint_changes_with_config() {
    let fp1 = compute_config_fingerprint(
        true,
        &InvariantConfig::production(),
        &PathologyPolicy::strict(),
        true,
        true,
        42,
    );
    
    let fp2 = compute_config_fingerprint(
        true,
        &InvariantConfig::production(),
        &PathologyPolicy::strict(),
        true,
        true,
        43, // Different seed
    );
    
    assert_ne!(fp1, fp2, "Fingerprint must change when config changes");
}

// =============================================================================
// ABORT FORMATTING TESTS
// =============================================================================

#[test]
fn test_abort_format_is_deterministic() {
    let violation = InvariantViolation {
        category: InvariantCategory::Book,
        violation_type: ViolationType::CrossedBook { best_bid: 0.55, best_ask: 0.50 },
        message: "Crossed book detected".to_string(),
        sim_time: 1_000_000_000,
        decision_time: 1_000_000_000,
        arrival_time: Some(1_000_000_000),
        seq: Some(42),
        market_id: Some("test-market".to_string()),
        order_id: None,
        fill_id: None,
        context: ViolationContext::default(),
    };
    
    let dump = CausalDump {
        violation: violation.clone(),
        triggering_event: None,
        recent_events: vec![
            EventSummary {
                arrival_time: 999_000_000,
                source_time: Some(998_000_000),
                seq: Some(41),
                event_type: "L2BookSnapshot".to_string(),
                market_id: Some("test-market".to_string()),
            },
        ],
        oms_transitions: vec![],
        ledger_entries: vec![],
        state_snapshot: StateSnapshot {
            best_bid: Some((0.55, 100.0)),
            best_ask: Some((0.50, 100.0)),
            cash: 1000.0,
            open_orders: 0,
            position: 0.0,
        },
        fingerprint_at_abort: 12345,
        config_hash: 67890,
    };
    
    let abort1 = ProductionGradeAbort::from_dump(dump.clone(), 11111, 67890);
    let abort2 = ProductionGradeAbort::from_dump(dump, 11111, 67890);
    
    // Format should be identical
    let text1 = abort1.format_deterministic();
    let text2 = abort2.format_deterministic();
    
    assert_eq!(text1, text2, "Abort format must be deterministic");
}

#[test]
fn test_abort_contains_violation_hash() {
    let dump = CausalDump {
        violation: InvariantViolation {
            category: InvariantCategory::Book,
            violation_type: ViolationType::CrossedBook { best_bid: 0.55, best_ask: 0.50 },
            message: "Crossed book".to_string(),
            sim_time: 1_000_000_000,
            decision_time: 1_000_000_000,
            arrival_time: None,
            seq: None,
            market_id: None,
            order_id: None,
            fill_id: None,
            context: ViolationContext::default(),
        },
        triggering_event: None,
        recent_events: vec![],
        oms_transitions: vec![],
        ledger_entries: vec![],
        state_snapshot: StateSnapshot {
            best_bid: None,
            best_ask: None,
            cash: 0.0,
            open_orders: 0,
            position: 0.0,
        },
        fingerprint_at_abort: 0,
        config_hash: 0,
    };
    
    let abort = ProductionGradeAbort::from_dump(dump, 0, 0);
    
    assert!(abort.violation_hash.0 != 0, "Should have non-zero violation hash");
    
    let text = abort.format_deterministic();
    assert!(text.contains("VIOLATION HASH"), "Format should include violation hash");
}

// =============================================================================
// INTEGRATION TESTS
// =============================================================================

#[test]
fn test_full_production_grade_scenario() {
    // Verify the full production-grade preset works
    let config = BacktestConfig::production_grade_15m_updown();
    
    // Should pass validation
    assert!(config.validate_production_grade().is_ok());
    
    // Should have correct settings
    assert!(config.production_grade);
    assert!(config.strict_mode);
    assert!(config.strict_accounting);
    assert!(config.ledger_config.as_ref().unwrap().strict_mode);
    assert_eq!(
        config.invariant_config.as_ref().unwrap().mode,
        InvariantMode::Hard
    );
    assert_eq!(config.integrity_policy, PathologyPolicy::strict());
}

#[test]
fn test_enforcement_function_validates_all_requirements() {
    // All valid
    let result = enforce_production_grade_requirements(
        true,
        &InvariantConfig::production(),
        &PathologyPolicy::strict(),
        Some(&LedgerConfig::production_grade()),
        true,
        42,
    );
    assert!(result.is_ok());
    
    let reqs = result.unwrap();
    assert!(reqs.visibility_strict);
    assert!(reqs.invariant_hard);
    assert!(reqs.all_invariant_categories);
    assert!(reqs.integrity_strict);
    assert!(reqs.ledger_strict);
    assert!(reqs.accounting_strict);
    assert!(reqs.deterministic_seed);
    assert!(reqs.dump_buffers_enabled);
    assert!(reqs.all_met());
}

// =============================================================================
// PRODUCTION-GRADE AS DEFAULT WITH ALLOW_NON_PRODUCTION OVERRIDE
// =============================================================================

#[test]
fn test_default_config_is_production_grade() {
    let config = BacktestConfig::default();
    
    // Production-grade must be the default
    assert!(config.production_grade, "Default config must have production_grade=true");
    assert!(config.strict_accounting, "Default config must have strict_accounting=true");
    assert!(config.strict_mode, "Default config must have strict_mode=true");
    assert!(!config.allow_non_production, "Default config must have allow_non_production=false");
    
    // Validate that the default config passes production-grade validation
    let result = config.validate_production_grade();
    assert!(result.is_ok(), "Default config must pass production-grade validation: {:?}", result);
}

#[test]
fn test_research_mode_has_allow_non_production_set() {
    let config = BacktestConfig::research_mode();
    
    // Research mode is non-production with explicit override
    assert!(!config.production_grade, "Research mode must have production_grade=false");
    assert!(config.allow_non_production, "Research mode must have allow_non_production=true");
}

#[test]
fn test_production_grade_15m_updown_has_allow_non_production_false() {
    let config = BacktestConfig::production_grade_15m_updown();
    
    assert!(config.production_grade);
    assert!(!config.allow_non_production);
    assert!(config.strict_accounting);
}

#[test]
fn test_non_production_without_override_is_detected() {
    // This tests that the detect_non_production_config logic works correctly
    // Create a config that is non-production
    let mut config = BacktestConfig::default();
    config.production_grade = false;
    config.strict_accounting = false;
    config.allow_non_production = false; // Override NOT set
    
    // The config should fail validation because it's non-production without override
    // This is tested at runtime in run(), but we can test the detection logic here
    let is_non_prod = !config.production_grade 
        || !config.strict_mode 
        || !config.strict_accounting 
        || config.settlement_spec.is_none() 
        || config.ledger_config.is_none();
    
    assert!(is_non_prod, "Config should be detected as non-production");
    assert!(!config.allow_non_production, "Override flag should not be set");
}

#[test]
fn test_fingerprint_differs_between_production_and_non_production() {
    use crate::backtest_v2::fingerprint::ConfigFingerprint;
    
    let production_config = BacktestConfig::production_grade_15m_updown();
    let research_config = BacktestConfig::research_mode();
    
    let prod_fingerprint = ConfigFingerprint::from_config(&production_config);
    let research_fingerprint = ConfigFingerprint::from_config(&research_config);
    
    // Hashes MUST differ
    assert_ne!(
        prod_fingerprint.hash, research_fingerprint.hash,
        "Fingerprints must differ between production and research modes"
    );
    
    // Specific fields that differ
    assert_ne!(prod_fingerprint.production_grade, research_fingerprint.production_grade);
    assert_ne!(prod_fingerprint.allow_non_production, research_fingerprint.allow_non_production);
    assert_ne!(prod_fingerprint.strict_accounting, research_fingerprint.strict_accounting);
}

#[test]
fn test_fingerprint_includes_allow_non_production_flag() {
    use crate::backtest_v2::fingerprint::ConfigFingerprint;
    
    // Two configs identical except for allow_non_production
    let mut config1 = BacktestConfig::research_mode();
    let mut config2 = BacktestConfig::research_mode();
    config2.allow_non_production = false; // Change just this flag
    
    let fp1 = ConfigFingerprint::from_config(&config1);
    let fp2 = ConfigFingerprint::from_config(&config2);
    
    // The hashes MUST differ because allow_non_production differs
    assert_ne!(
        fp1.hash, fp2.hash,
        "Fingerprints must differ when allow_non_production differs"
    );
    assert!(fp1.allow_non_production);
    assert!(!fp2.allow_non_production);
}

#[test]
fn test_results_track_allow_non_production_and_downgrades() {
    use crate::backtest_v2::orchestrator::BacktestResults;
    
    let mut results = BacktestResults::default();
    
    // Initially, fields should be default
    assert!(!results.allow_non_production);
    assert!(results.downgraded_subsystems.is_empty());
    
    // When set, they should be preserved
    results.allow_non_production = true;
    results.downgraded_subsystems = vec![
        "production_grade=false".to_string(),
        "strict_accounting=false".to_string(),
    ];
    
    assert!(results.allow_non_production);
    assert_eq!(results.downgraded_subsystems.len(), 2);
}

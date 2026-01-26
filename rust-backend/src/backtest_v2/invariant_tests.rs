//! Adversarial Invariant Tests
//!
//! Tests that deliberately inject violations to verify:
//! - Hard mode aborts correctly
//! - Causal dumps are bounded and deterministic
//! - Each invariant category is checked

use crate::backtest_v2::invariants::*;
use crate::backtest_v2::matching::{LimitOrderBook, MatchingConfig};
use crate::backtest_v2::oms::OrderState;

fn make_test_book() -> LimitOrderBook {
    LimitOrderBook::new("test_token", MatchingConfig::default())
}

// =============================================================================
// TIME INVARIANT TESTS
// =============================================================================

#[test]
fn test_time_decision_backward_hard_mode_aborts() {
    let config = InvariantConfig {
        mode: InvariantMode::Hard,
        ..Default::default()
    };
    let mut enforcer = InvariantEnforcer::new(config);

    // Set initial time
    let _ = enforcer.check_decision_time(1000);

    // Go backward - should abort
    let result = enforcer.check_decision_time(500);

    assert!(result.is_err(), "Hard mode should abort on time violation");
    let abort = result.unwrap_err();
    assert!(matches!(
        abort.dump.violation.violation_type,
        ViolationType::DecisionTimeBackward { old: 1000, new: 500 }
    ));
    assert!(abort.dump.format_text().contains("DecisionTimeBackward"));
}

#[test]
fn test_time_decision_backward_soft_mode_continues() {
    let config = InvariantConfig {
        mode: InvariantMode::Soft,
        ..Default::default()
    };
    let mut enforcer = InvariantEnforcer::new(config);

    let _ = enforcer.check_decision_time(1000);
    let result = enforcer.check_decision_time(500);

    assert!(result.is_ok(), "Soft mode should continue");
    assert!(enforcer.counters().has_violations());
    assert_eq!(enforcer.counters().time_violations, 1);
}

#[test]
fn test_visibility_arrival_after_decision() {
    let config = InvariantConfig::production();
    let mut enforcer = InvariantEnforcer::new(config);

    // arrival_time > decision_time is a look-ahead bug
    let result = enforcer.check_visibility(2000, 1000);

    assert!(result.is_err());
    let abort = result.unwrap_err();
    assert!(matches!(
        abort.dump.violation.violation_type,
        ViolationType::ArrivalAfterDecision { arrival: 2000, decision: 1000 }
    ));
}

#[test]
fn test_event_ordering_timestamp_regression() {
    let config = InvariantConfig::production();
    let mut enforcer = InvariantEnforcer::new(config);

    // First event
    let _ = enforcer.check_event_ordering("market1", 1000, Some(1));

    // Second event with earlier arrival time
    let result = enforcer.check_event_ordering("market1", 500, Some(2));

    assert!(result.is_err());
    let abort = result.unwrap_err();
    assert!(matches!(
        abort.dump.violation.violation_type,
        ViolationType::TimestampRegression { .. }
    ));
}

#[test]
fn test_event_ordering_sequence_not_increasing() {
    let config = InvariantConfig::production();
    let mut enforcer = InvariantEnforcer::new(config);

    let _ = enforcer.check_event_ordering("market1", 1000, Some(10));

    // Same or lower sequence number
    let result = enforcer.check_event_ordering("market1", 2000, Some(10));

    assert!(result.is_err());
    let abort = result.unwrap_err();
    assert!(matches!(
        abort.dump.violation.violation_type,
        ViolationType::EventOrderingViolation { expected_seq: 11, actual_seq: 10 }
    ));
}

// =============================================================================
// BOOK INVARIANT TESTS
// =============================================================================

#[test]
fn test_book_valid_state_passes() {
    let config = InvariantConfig::production();
    let mut enforcer = InvariantEnforcer::new(config);

    // Empty book is valid - no crossed book, no negative sizes, no invalid prices
    let book = make_test_book();

    let result = enforcer.check_book(&book, 1000);

    assert!(result.is_ok());
    assert_eq!(enforcer.counters().book_checks, 1);
    assert_eq!(enforcer.counters().book_violations, 0);
}

// =============================================================================
// OMS INVARIANT TESTS
// =============================================================================

#[test]
fn test_oms_illegal_state_transition() {
    let config = InvariantConfig::production();
    let mut enforcer = InvariantEnforcer::new(config);

    enforcer.register_order(1, 100.0);

    // Skip Sent state and go directly to Live - illegal
    let result = enforcer.check_order_transition(1, OrderState::Live, 100, "direct_ack");

    assert!(result.is_err());
    let abort = result.unwrap_err();
    assert!(matches!(
        abort.dump.violation.violation_type,
        ViolationType::IllegalStateTransition { .. }
    ));
}

#[test]
fn test_oms_fill_before_ack() {
    let config = InvariantConfig::production();
    let mut enforcer = InvariantEnforcer::new(config);

    enforcer.register_order(1, 100.0);

    // Try to check fill while order is still New (not acked)
    let result = enforcer.check_fill_order_state(1, 100);

    assert!(result.is_err());
    let abort = result.unwrap_err();
    assert!(matches!(
        abort.dump.violation.violation_type,
        ViolationType::FillBeforeAck { order_id: 1 }
    ));
}

#[test]
fn test_oms_fill_after_terminal() {
    let config = InvariantConfig::production();
    let mut enforcer = InvariantEnforcer::new(config);

    enforcer.register_order(1, 100.0);

    // Normal flow to Done (rejection path: New -> PendingAck -> Done)
    let _ = enforcer.check_order_transition(1, OrderState::PendingAck, 100, "send");
    let _ = enforcer.check_order_transition(1, OrderState::Done, 200, "reject");

    // Now try to fill a rejected order
    let result = enforcer.check_fill_order_state(1, 300);

    assert!(result.is_err());
    let abort = result.unwrap_err();
    assert!(matches!(
        abort.dump.violation.violation_type,
        ViolationType::FillAfterTerminal { order_id: 1, .. }
    ));
}

#[test]
fn test_oms_unknown_order() {
    let config = InvariantConfig::production();
    let mut enforcer = InvariantEnforcer::new(config);

    // Try to check fill for non-existent order
    let result = enforcer.check_fill_order_state(999, 100);

    assert!(result.is_err());
    let abort = result.unwrap_err();
    assert!(matches!(
        abort.dump.violation.violation_type,
        ViolationType::UnknownOrderId { order_id: 999 }
    ));
}

#[test]
fn test_oms_valid_lifecycle() {
    let config = InvariantConfig::production();
    let mut enforcer = InvariantEnforcer::new(config);

    enforcer.register_order(1, 100.0);

    // Valid lifecycle: New -> PendingAck -> Live -> PartiallyFilled -> Done
    assert!(enforcer.check_order_transition(1, OrderState::PendingAck, 100, "send").is_ok());
    assert!(enforcer.check_order_transition(1, OrderState::Live, 200, "ack").is_ok());
    assert!(enforcer.check_fill_order_state(1, 300).is_ok());
    assert!(enforcer.check_order_transition(1, OrderState::PartiallyFilled, 300, "fill").is_ok());
    assert!(enforcer.check_order_transition(1, OrderState::Done, 400, "fill_complete").is_ok());

    assert_eq!(enforcer.counters().oms_violations, 0);
}

// =============================================================================
// FILL INVARIANT TESTS
// =============================================================================

#[test]
fn test_fill_overfill() {
    let config = InvariantConfig::production();
    let mut enforcer = InvariantEnforcer::new(config);
    let book = make_test_book();

    enforcer.register_order(1, 100.0);

    // First fill OK
    assert!(enforcer.check_fill(1, 50.0, 0.50, true, &book, 100).is_ok());

    // Overfill
    let result = enforcer.check_fill(1, 60.0, 0.50, true, &book, 200);

    assert!(result.is_err());
    let abort = result.unwrap_err();
    assert!(matches!(
        abort.dump.violation.violation_type,
        ViolationType::OverFill { order_id: 1, .. }
    ));
}

#[test]
fn test_fill_negative_size() {
    let config = InvariantConfig::production();
    let mut enforcer = InvariantEnforcer::new(config);
    let book = make_test_book();

    enforcer.register_order(1, 100.0);

    let result = enforcer.check_fill(1, -10.0, 0.50, true, &book, 100);

    assert!(result.is_err());
    let abort = result.unwrap_err();
    assert!(matches!(
        abort.dump.violation.violation_type,
        ViolationType::NegativeFillSize { order_id: 1, size } if size == -10.0
    ));
}

#[test]
fn test_fill_nan() {
    let config = InvariantConfig::production();
    let mut enforcer = InvariantEnforcer::new(config);
    let book = make_test_book();

    enforcer.register_order(1, 100.0);

    let result = enforcer.check_fill(1, f64::NAN, 0.50, true, &book, 100);

    assert!(result.is_err());
    let abort = result.unwrap_err();
    assert!(matches!(
        abort.dump.violation.violation_type,
        ViolationType::FillNaN { order_id: 1 }
    ));
}

#[test]
fn test_fill_valid() {
    let config = InvariantConfig::production();
    let mut enforcer = InvariantEnforcer::new(config);
    let book = make_test_book();

    enforcer.register_order(1, 100.0);

    // Partial fill
    assert!(enforcer.check_fill(1, 30.0, 0.50, true, &book, 100).is_ok());
    // Another partial
    assert!(enforcer.check_fill(1, 40.0, 0.50, true, &book, 200).is_ok());
    // Final fill (exactly fills the order)
    assert!(enforcer.check_fill(1, 30.0, 0.50, true, &book, 300).is_ok());

    assert_eq!(enforcer.counters().fill_violations, 0);
}

// =============================================================================
// ACCOUNTING INVARIANT TESTS
// =============================================================================

#[test]
fn test_accounting_unbalanced_entry() {
    let config = InvariantConfig::production();
    let mut enforcer = InvariantEnforcer::new(config);

    let result = enforcer.check_accounting_balance(100, 90, 1000);

    assert!(result.is_err());
    let abort = result.unwrap_err();
    assert!(matches!(
        abort.dump.violation.violation_type,
        ViolationType::UnbalancedEntry { debits: 100, credits: 90 }
    ));
}

#[test]
fn test_accounting_negative_cash() {
    let config = InvariantConfig::production();
    let mut enforcer = InvariantEnforcer::new(config);

    let result = enforcer.check_cash(-100.0, 1000);

    assert!(result.is_err());
    let abort = result.unwrap_err();
    assert!(matches!(
        abort.dump.violation.violation_type,
        ViolationType::NegativeCash { cash } if cash == -100.0
    ));
}

#[test]
fn test_accounting_duplicate_settlement() {
    let config = InvariantConfig::production();
    let mut enforcer = InvariantEnforcer::new(config);

    // First settlement OK
    assert!(enforcer.check_settlement("market1", 1000).is_ok());

    // Duplicate settlement
    let result = enforcer.check_settlement("market1", 2000);

    assert!(result.is_err());
    let abort = result.unwrap_err();
    assert!(matches!(
        abort.dump.violation.violation_type,
        ViolationType::DuplicateSettlement { .. }
    ));
}

#[test]
fn test_accounting_valid() {
    let config = InvariantConfig::production();
    let mut enforcer = InvariantEnforcer::new(config);

    assert!(enforcer.check_accounting_balance(100, 100, 1000).is_ok());
    assert!(enforcer.check_cash(1000.0, 1000).is_ok());
    assert!(enforcer.check_settlement("market1", 1000).is_ok());
    assert!(enforcer.check_settlement("market2", 2000).is_ok());

    assert_eq!(enforcer.counters().accounting_violations, 0);
}

// =============================================================================
// CATEGORY FILTERING TESTS
// =============================================================================

#[test]
fn test_disabled_category_not_checked() {
    let mut categories = CategoryFlags::all();
    categories.disable(InvariantCategory::Time);

    let config = InvariantConfig {
        mode: InvariantMode::Hard,
        categories,
        ..Default::default()
    };
    let mut enforcer = InvariantEnforcer::new(config);

    // Set time, then go backward
    enforcer.last_decision_time = 1000;

    // Should NOT abort because Time category is disabled
    let result = enforcer.check_decision_time(500);
    assert!(result.is_ok());
    assert_eq!(enforcer.counters().time_checks, 0);
}

#[test]
fn test_enabled_category_checked() {
    let categories = CategoryFlags::from_categories(&[InvariantCategory::Time]);

    let config = InvariantConfig {
        mode: InvariantMode::Hard,
        categories,
        ..Default::default()
    };
    let mut enforcer = InvariantEnforcer::new(config);

    enforcer.last_decision_time = 1000;

    // Should abort because Time is enabled
    let result = enforcer.check_decision_time(500);
    assert!(result.is_err());
}

// =============================================================================
// CAUSAL DUMP TESTS
// =============================================================================

#[test]
fn test_causal_dump_bounded_size() {
    let config = InvariantConfig {
        mode: InvariantMode::Hard,
        event_dump_depth: 5,
        oms_dump_depth: 3,
        ledger_dump_depth: 2,
        ..Default::default()
    };
    let mut enforcer = InvariantEnforcer::new(config);

    // Record more events than dump depth using TimestampedEvent
    use crate::backtest_v2::events::{Event, Side, TimestampedEvent};
    for i in 0..20 {
        let event = TimestampedEvent {
            time: i * 100,
            source_time: i * 100,
            seq: i as u64,
            source: 0,
            event: Event::TradePrint {
                token_id: "test_token".to_string(),
                price: 0.50,
                size: 10.0,
                aggressor_side: Side::Buy,
                trade_id: None,
            },
        };
        enforcer.record_event(&event);
    }

    // Cause violation
    enforcer.last_decision_time = 1000;
    let result = enforcer.check_decision_time(500);

    assert!(result.is_err());
    let dump = result.unwrap_err().dump;

    // Dump should be bounded
    assert!(dump.recent_events.len() <= 5);
}

#[test]
fn test_causal_dump_deterministic() {
    // Run twice with same inputs, dumps should be identical
    let run = || {
        let config = InvariantConfig::production();
        let mut enforcer = InvariantEnforcer::new(config);

        enforcer.register_order(1, 100.0);
        enforcer.last_decision_time = 1000;

        let result = enforcer.check_decision_time(500);
        result.unwrap_err().dump
    };

    let dump1 = run();
    let dump2 = run();

    // Check determinism of key fields
    assert_eq!(dump1.violation.sim_time, dump2.violation.sim_time);
    assert_eq!(dump1.config_hash, dump2.config_hash);
    assert_eq!(
        format!("{:?}", dump1.violation.violation_type),
        format!("{:?}", dump2.violation.violation_type)
    );
}

#[test]
fn test_causal_dump_format_text_contains_required_info() {
    let config = InvariantConfig::production();
    let mut enforcer = InvariantEnforcer::new(config);

    enforcer.last_decision_time = 1000;
    let result = enforcer.check_decision_time(500);

    let text = result.unwrap_err().dump.format_text();

    assert!(text.contains("INVARIANT VIOLATION"));
    assert!(text.contains("Category:"));
    assert!(text.contains("Message:"));
    assert!(text.contains("State Snapshot"));
    assert!(text.contains("Fingerprint at abort:"));
    assert!(text.contains("Config hash:"));
}

// =============================================================================
// PRODUCTION REQUIREMENTS TESTS
// =============================================================================

#[test]
fn test_production_requirements_soft_mode_fails() {
    let config = InvariantConfig {
        mode: InvariantMode::Soft,
        categories: CategoryFlags::all(),
        ..Default::default()
    };

    let reqs = ProductionGradeRequirements::check(&config);

    assert!(!reqs.is_satisfied());
    assert!(!reqs.invariant_mode_hard);
    assert!(reqs.all_categories_enabled);
}

#[test]
fn test_production_requirements_missing_categories_fails() {
    let mut categories = CategoryFlags::all();
    categories.disable(InvariantCategory::Accounting);

    let config = InvariantConfig {
        mode: InvariantMode::Hard,
        categories,
        ..Default::default()
    };

    let reqs = ProductionGradeRequirements::check(&config);

    assert!(!reqs.is_satisfied());
    assert!(reqs.invariant_mode_hard);
    assert!(!reqs.all_categories_enabled);
}

#[test]
fn test_production_requirements_satisfied() {
    let config = InvariantConfig::production();
    let reqs = ProductionGradeRequirements::check(&config);

    assert!(reqs.is_satisfied());
    assert!(reqs.unsatisfied_reasons().is_empty());
}

// =============================================================================
// CONTEXT RECORDING TESTS
// =============================================================================

#[test]
fn test_oms_transitions_recorded() {
    let config = InvariantConfig {
        mode: InvariantMode::Soft, // Soft so we can see transitions after violation
        ..Default::default()
    };
    let mut enforcer = InvariantEnforcer::new(config);

    enforcer.register_order(1, 100.0);
    let _ = enforcer.check_order_transition(1, OrderState::PendingAck, 100, "send");
    let _ = enforcer.check_order_transition(1, OrderState::Live, 200, "ack");

    // Check transitions are recorded
    let _context = enforcer.first_violation();
    
    // Even without violation, transitions should be recorded
    // We need a violation to see context, so cause one
    enforcer.last_decision_time = 1000;
    let _ = enforcer.check_decision_time(500);

    let violation = enforcer.first_violation().unwrap();
    // Context should have recent OMS transitions
    assert!(violation.context.oms_transitions.len() >= 2);
}

// =============================================================================
// COUNTER TESTS
// =============================================================================

#[test]
fn test_counters_track_all_categories() {
    let config = InvariantConfig {
        mode: InvariantMode::Soft,
        ..Default::default()
    };
    let mut enforcer = InvariantEnforcer::new(config);

    // Perform checks in each category
    let _ = enforcer.check_decision_time(100);
    
    let book = make_test_book();
    let _ = enforcer.check_book(&book, 100);
    
    enforcer.register_order(1, 100.0);
    let _ = enforcer.check_order_transition(1, OrderState::PendingAck, 100, "send");
    
    let _ = enforcer.check_fill(1, 10.0, 0.50, true, &book, 100);
    let _ = enforcer.check_accounting_balance(100, 100, 100);

    assert_eq!(enforcer.counters().time_checks, 1);
    assert_eq!(enforcer.counters().book_checks, 1);
    assert_eq!(enforcer.counters().oms_checks, 1);
    assert_eq!(enforcer.counters().fill_checks, 1);
    assert_eq!(enforcer.counters().accounting_checks, 1);
    assert_eq!(enforcer.counters().total_checks, 5);
}

#[test]
fn test_counters_summary_format() {
    let config = InvariantConfig::default();
    let enforcer = InvariantEnforcer::new(config);

    let summary = enforcer.counters().summary();
    
    assert!(summary.contains("Checks:"));
    assert!(summary.contains("Violations:"));
    assert!(summary.contains("T:"));
    assert!(summary.contains("B:"));
    assert!(summary.contains("O:"));
    assert!(summary.contains("F:"));
    assert!(summary.contains("A:"));
}

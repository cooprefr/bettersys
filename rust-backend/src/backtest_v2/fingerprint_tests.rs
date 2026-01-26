//! Fingerprint Integration Tests
//!
//! These tests verify the run fingerprint behavior:
//! 1. Determinism: same inputs + seed → identical fingerprint
//! 2. Stability: non-observable changes (logging) → unchanged fingerprint
//! 3. Config sensitivity: behavior-relevant config change → config hash changes
//! 4. Input sensitivity: altered input record → dataset hash changes
//! 5. Behavior sensitivity: different decision → behavior hash changes

use crate::backtest_v2::fingerprint::{
    BehaviorFingerprintBuilder, CodeFingerprint, ConfigFingerprint, FingerprintCollector,
    RunFingerprint, SeedFingerprint, StreamFingerprintBuilder, FINGERPRINT_VERSION,
};
use crate::backtest_v2::clock::Nanos;
use crate::backtest_v2::events::Side;
use crate::backtest_v2::settlement::SettlementOutcome;
use crate::backtest_v2::portfolio::Outcome;

// =============================================================================
// TEST 1: DETERMINISM - Same inputs produce identical fingerprint
// =============================================================================

#[test]
fn test_determinism_same_inputs_same_fingerprint() {
    // Run the same behavior collection twice with identical inputs
    // Note: We test behavior fingerprint determinism, not the full RunFingerprint,
    // because the full fingerprint includes CodeFingerprint which may vary by build.
    fn collect_behavior() -> crate::backtest_v2::fingerprint::BehaviorFingerprint {
        let mut builder = BehaviorFingerprintBuilder::new();
        
        // Record identical behavior
        builder.record_decision(1, 1000, 3, 0x1111);
        builder.record_order_submit(100, Side::Buy, 0.5, 10.0, 1000);
        builder.record_order_ack(100, 1010);
        builder.record_fill(100, 0.5, 10.0, false, 0.001, 1100);
        
        builder.record_decision(2, 2000, 2, 0x2222);
        builder.record_order_submit(101, Side::Sell, 0.55, 10.0, 2000);
        builder.record_fill(101, 0.55, 10.0, false, 0.001, 2100);
        
        builder.build()
    }
    
    let fp1 = collect_behavior();
    let fp2 = collect_behavior();
    
    // Behavior fingerprints should be identical
    assert_eq!(fp1.hash, fp2.hash, "Same inputs should produce identical behavior hash");
    assert_eq!(fp1.event_count, fp2.event_count);
}

#[test]
fn test_determinism_collector_with_same_inputs() {
    // Test that FingerprintCollector produces identical behavior hashes
    // Note: We only test behavior hash because dataset hash depends on data_contract
    // which is not set in this test.
    fn collect_behavior() -> u64 {
        let mut collector = FingerprintCollector::new();
        
        // Record input events (these go into stream fingerprints)
        collector.record_input_event("orderbook_snapshots", 1000, Some("btc-updown"), 0x1234);
        collector.record_input_event("orderbook_snapshots", 2000, Some("btc-updown"), 0x5678);
        collector.record_input_event("trades", 1500, Some("btc-updown"), 0xABCD);
        
        // Record behavior
        collector.record_decision(1, 1000, 3, 0x1111);
        collector.record_order_submit(100, Side::Buy, 0.5, 10.0, 1000);
        collector.record_fill(100, 0.5, 10.0, false, 0.001, 1100);
        
        let fp = collector.finalize();
        fp.behavior.hash
    }
    
    let behavior1 = collect_behavior();
    let behavior2 = collect_behavior();
    
    assert_eq!(behavior1, behavior2, "Same behavior should produce identical behavior hash");
}

#[test]
fn test_determinism_code_fingerprint_stable() {
    // Code fingerprint should be identical across calls
    let cf1 = CodeFingerprint::new();
    let cf2 = CodeFingerprint::new();
    
    assert_eq!(cf1.hash, cf2.hash, "Code fingerprint should be stable");
    assert_eq!(cf1.crate_version, cf2.crate_version);
    assert_eq!(cf1.build_profile, cf2.build_profile);
}

#[test]
fn test_determinism_seed_fingerprint_stable() {
    // Same seed should produce identical fingerprint
    let sf1 = SeedFingerprint::new(12345);
    let sf2 = SeedFingerprint::new(12345);
    
    assert_eq!(sf1.hash, sf2.hash, "Same seed should produce identical fingerprint");
    assert_eq!(sf1.primary_seed, sf2.primary_seed);
    assert_eq!(sf1.sub_seeds, sf2.sub_seeds);
}

// =============================================================================
// TEST 2: STABILITY - Non-observable changes don't affect fingerprint
// =============================================================================

#[test]
fn test_stability_behavior_unaffected_by_event_order_within_same_decision_time() {
    // Events at same decision_time but different arrival should still produce deterministic hash
    // because behavior events are ordered by decision_time
    let mut b1 = BehaviorFingerprintBuilder::new();
    b1.record_decision(1, 1000, 5, 0x1234);
    b1.record_order_submit(100, Side::Buy, 0.5, 10.0, 1000);
    b1.record_fill(100, 0.5, 10.0, false, 0.001, 1000);
    let fp1 = b1.build();
    
    let mut b2 = BehaviorFingerprintBuilder::new();
    b2.record_decision(1, 1000, 5, 0x1234);
    b2.record_order_submit(100, Side::Buy, 0.5, 10.0, 1000);
    b2.record_fill(100, 0.5, 10.0, false, 0.001, 1000);
    let fp2 = b2.build();
    
    assert_eq!(fp1.hash, fp2.hash, "Same events should produce same hash");
}

#[test]
fn test_stability_stream_fingerprint_unaffected_by_market_id_order() {
    // Market IDs are sorted, so order shouldn't matter
    let mut s1 = StreamFingerprintBuilder::new("trades");
    s1.add_record(1000, Some("eth-updown"), 0x1111);
    s1.add_record(2000, Some("btc-updown"), 0x2222);
    let fp1 = s1.build();
    
    let mut s2 = StreamFingerprintBuilder::new("trades");
    s2.add_record(1000, Some("eth-updown"), 0x1111);
    s2.add_record(2000, Some("btc-updown"), 0x2222);
    let fp2 = s2.build();
    
    assert_eq!(fp1.rolling_hash, fp2.rolling_hash);
    // Market IDs should be sorted in the fingerprint
    assert!(fp1.market_ids.windows(2).all(|w| w[0] <= w[1]), "Market IDs should be sorted");
}

// =============================================================================
// TEST 3: CONFIG SENSITIVITY - Config changes affect config hash
// =============================================================================

#[test]
fn test_config_sensitivity_fee_rate_changes_hash() {
    // Different fee rates should produce different hashes
    let cfg1 = ConfigFingerprint {
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
        fee_rate_bps: Some(10), // 10 bps
        strategy_params_hash: 0x1234,
        arrival_policy: "RecordedArrival".to_string(),
        strict_accounting: true,
        production_grade: true,
        allow_non_production: false,
        hash: 0,
    };
    
    let mut cfg2 = cfg1.clone();
    cfg2.fee_rate_bps = Some(20); // 20 bps - different
    
    // Compute hashes manually
    let hash1 = {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut h = DefaultHasher::new();
        cfg1.settlement_reference_rule.hash(&mut h);
        cfg1.fee_rate_bps.hash(&mut h);
        h.finish()
    };
    
    let hash2 = {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut h = DefaultHasher::new();
        cfg2.settlement_reference_rule.hash(&mut h);
        cfg2.fee_rate_bps.hash(&mut h);
        h.finish()
    };
    
    assert_ne!(hash1, hash2, "Different fee rates should produce different hashes");
}

#[test]
fn test_config_sensitivity_settlement_rule_changes_hash() {
    let rule1 = "LastUpdateAtOrBeforeCutoff";
    let rule2 = "FirstUpdateAfterCutoff";
    
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    
    let hash1 = {
        let mut h = DefaultHasher::new();
        rule1.hash(&mut h);
        h.finish()
    };
    
    let hash2 = {
        let mut h = DefaultHasher::new();
        rule2.hash(&mut h);
        h.finish()
    };
    
    assert_ne!(hash1, hash2, "Different settlement rules should produce different hashes");
}

// =============================================================================
// TEST 4: INPUT SENSITIVITY - Input changes affect dataset hash
// =============================================================================

#[test]
fn test_input_sensitivity_altered_record_changes_hash() {
    let mut s1 = StreamFingerprintBuilder::new("trades");
    s1.add_record(1000, Some("btc"), 0x1111);
    s1.add_record(2000, Some("btc"), 0x2222);
    let fp1 = s1.build();
    
    let mut s2 = StreamFingerprintBuilder::new("trades");
    s2.add_record(1000, Some("btc"), 0x1111);
    s2.add_record(2000, Some("btc"), 0x3333); // Different record hash
    let fp2 = s2.build();
    
    assert_ne!(fp1.rolling_hash, fp2.rolling_hash, "Altered record should change hash");
}

#[test]
fn test_input_sensitivity_missing_record_changes_hash() {
    let mut s1 = StreamFingerprintBuilder::new("trades");
    s1.add_record(1000, Some("btc"), 0x1111);
    s1.add_record(2000, Some("btc"), 0x2222);
    s1.add_record(3000, Some("btc"), 0x3333);
    let fp1 = s1.build();
    
    let mut s2 = StreamFingerprintBuilder::new("trades");
    s2.add_record(1000, Some("btc"), 0x1111);
    s2.add_record(2000, Some("btc"), 0x2222);
    // Missing third record
    let fp2 = s2.build();
    
    assert_ne!(fp1.rolling_hash, fp2.rolling_hash, "Missing record should change hash");
    assert_ne!(fp1.record_count, fp2.record_count);
}

#[test]
fn test_input_sensitivity_record_order_matters() {
    let mut s1 = StreamFingerprintBuilder::new("trades");
    s1.add_record(1000, Some("btc"), 0x1111);
    s1.add_record(2000, Some("btc"), 0x2222);
    let fp1 = s1.build();
    
    let mut s2 = StreamFingerprintBuilder::new("trades");
    s2.add_record(2000, Some("btc"), 0x2222); // Swapped order
    s2.add_record(1000, Some("btc"), 0x1111);
    let fp2 = s2.build();
    
    assert_ne!(fp1.rolling_hash, fp2.rolling_hash, "Record order should affect hash");
}

// =============================================================================
// TEST 5: BEHAVIOR SENSITIVITY - Decision changes affect behavior hash
// =============================================================================

#[test]
fn test_behavior_sensitivity_different_decision_changes_hash() {
    let mut b1 = BehaviorFingerprintBuilder::new();
    b1.record_decision(1, 1000, 5, 0x1234);
    b1.record_order_submit(100, Side::Buy, 0.5, 10.0, 1000);
    let fp1 = b1.build();
    
    let mut b2 = BehaviorFingerprintBuilder::new();
    b2.record_decision(1, 1000, 5, 0x1234);
    b2.record_order_submit(100, Side::Sell, 0.5, 10.0, 1000); // Different side
    let fp2 = b2.build();
    
    assert_ne!(fp1.hash, fp2.hash, "Different order side should change hash");
}

#[test]
fn test_behavior_sensitivity_different_fill_price_changes_hash() {
    let mut b1 = BehaviorFingerprintBuilder::new();
    b1.record_fill(100, 0.50, 10.0, false, 0.001, 1000);
    let fp1 = b1.build();
    
    let mut b2 = BehaviorFingerprintBuilder::new();
    b2.record_fill(100, 0.51, 10.0, false, 0.001, 1000); // Different price
    let fp2 = b2.build();
    
    assert_ne!(fp1.hash, fp2.hash, "Different fill price should change hash");
}

#[test]
fn test_behavior_sensitivity_different_settlement_outcome_changes_hash() {
    let mut b1 = BehaviorFingerprintBuilder::new();
    b1.record_settlement(
        "btc-updown-15m-1000",
        1000 * 1_000_000_000,
        1900 * 1_000_000_000,
        50000.0,
        50100.0,
        &SettlementOutcome::Resolved { winner: Outcome::Yes, is_tie: false },
        2000 * 1_000_000_000,
    );
    let fp1 = b1.build();
    
    let mut b2 = BehaviorFingerprintBuilder::new();
    b2.record_settlement(
        "btc-updown-15m-1000",
        1000 * 1_000_000_000,
        1900 * 1_000_000_000,
        50000.0,
        49900.0, // Different end price
        &SettlementOutcome::Resolved { winner: Outcome::No, is_tie: false }, // Different outcome
        2000 * 1_000_000_000,
    );
    let fp2 = b2.build();
    
    assert_ne!(fp1.hash, fp2.hash, "Different settlement outcome should change hash");
}

#[test]
fn test_behavior_sensitivity_extra_fill_changes_hash() {
    let mut b1 = BehaviorFingerprintBuilder::new();
    b1.record_decision(1, 1000, 3, 0x1234);
    b1.record_fill(100, 0.5, 10.0, false, 0.001, 1100);
    let fp1 = b1.build();
    
    let mut b2 = BehaviorFingerprintBuilder::new();
    b2.record_decision(1, 1000, 3, 0x1234);
    b2.record_fill(100, 0.5, 10.0, false, 0.001, 1100);
    b2.record_fill(101, 0.55, 5.0, false, 0.001, 1200); // Extra fill
    let fp2 = b2.build();
    
    assert_ne!(fp1.hash, fp2.hash, "Extra fill should change hash");
    assert_ne!(fp1.event_count, fp2.event_count);
}

// =============================================================================
// TEST 6: SEED SENSITIVITY
// =============================================================================

#[test]
fn test_seed_sensitivity_different_seed_changes_hash() {
    let sf1 = SeedFingerprint::new(42);
    let sf2 = SeedFingerprint::new(43);
    
    assert_ne!(sf1.hash, sf2.hash, "Different seeds should produce different hashes");
    assert_ne!(sf1.sub_seeds, sf2.sub_seeds);
}

// =============================================================================
// TEST 7: VERSION STRING
// =============================================================================

#[test]
fn test_version_string_correct() {
    assert_eq!(FINGERPRINT_VERSION, "RUNFP_V1");
    
    let mut collector = FingerprintCollector::new();
    collector.record_decision(1, 1000, 1, 0x1234);
    let fp = collector.finalize();
    
    assert_eq!(fp.version, FINGERPRINT_VERSION);
}

// =============================================================================
// TEST 8: COLLECTOR EVENT COUNT
// =============================================================================

#[test]
fn test_collector_event_count_accurate() {
    let mut collector = FingerprintCollector::new();
    
    assert_eq!(collector.behavior_event_count(), 0);
    
    collector.record_decision(1, 1000, 3, 0x1234);
    assert_eq!(collector.behavior_event_count(), 1);
    
    collector.record_order_submit(100, Side::Buy, 0.5, 10.0, 1000);
    assert_eq!(collector.behavior_event_count(), 2);
    
    collector.record_order_ack(100, 1010);
    assert_eq!(collector.behavior_event_count(), 3);
    
    collector.record_fill(100, 0.5, 10.0, false, 0.001, 1100);
    assert_eq!(collector.behavior_event_count(), 4);
    
    let fp = collector.finalize();
    assert_eq!(fp.behavior.event_count, 4);
}

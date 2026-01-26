//! Comprehensive Tests for Oracle Configuration and Settlement
//!
//! Tests cover:
//! 1. Config validation: missing rule/feed/decimals causes startup failure in production_grade
//! 2. Feed validation: wrong chain_id fails, non-contract address fails, decimals mismatch fails
//! 3. Backfill idempotency: run backfill twice -> no duplicate rounds
//! 4. Settlement selection correctness: boundary tests for reference rule
//! 5. Fingerprint sensitivity: changing reference rule or feed address changes RunFingerprint

#[cfg(test)]
mod tests {
    use super::super::*;

    // =========================================================================
    // CONFIG VALIDATION TESTS
    // =========================================================================

    #[test]
    fn test_empty_config_fails_production_validation() {
        let config = OracleConfig::new();
        let result = config.validate_production();
        
        assert!(!result.is_valid, "Empty config should fail validation");
        assert!(!result.violations.is_empty(), "Should have violations");
        
        // Should specifically fail on missing feeds
        let has_feed_violation = result.violations.iter()
            .any(|v| v.field == "feeds" && v.description.contains("No oracle feeds"));
        assert!(has_feed_violation, "Should have 'no feeds' violation");
    }

    #[test]
    fn test_missing_feed_proxy_address_fails() {
        let mut config = OracleConfig::new();
        config.feeds.insert("BTC".to_string(), OracleFeedConfig {
            asset_symbol: "BTC".to_string(),
            chain_id: 137,
            feed_proxy_address: String::new(), // Empty address
            decimals: 8,
            expected_description: None,
            deviation_threshold: None,
            heartbeat_secs: None,
        });
        
        let result = config.validate_production();
        assert!(!result.is_valid);
        assert!(result.violations.iter().any(|v| 
            v.field.contains("feed_proxy_address") && 
            v.description.contains("empty")
        ));
    }

    #[test]
    fn test_invalid_address_format_fails() {
        let mut config = OracleConfig::new();
        config.feeds.insert("BTC".to_string(), OracleFeedConfig {
            asset_symbol: "BTC".to_string(),
            chain_id: 137,
            feed_proxy_address: "not_a_valid_address".to_string(),
            decimals: 8,
            expected_description: None,
            deviation_threshold: None,
            heartbeat_secs: None,
        });
        
        let result = config.validate_production();
        assert!(!result.is_valid);
        assert!(result.violations.iter().any(|v| 
            v.field.contains("feed_proxy_address") && 
            v.description.contains("does not look like")
        ));
    }

    #[test]
    fn test_zero_chain_id_fails() {
        let mut config = OracleConfig::new();
        config.feeds.insert("BTC".to_string(), OracleFeedConfig {
            asset_symbol: "BTC".to_string(),
            chain_id: 0, // Invalid
            feed_proxy_address: "0xc907E116054Ad103354f2D350FD2514433D57F6f".to_string(),
            decimals: 8,
            expected_description: None,
            deviation_threshold: None,
            heartbeat_secs: None,
        });
        
        let result = config.validate_production();
        assert!(!result.is_valid);
        assert!(result.violations.iter().any(|v| 
            v.field.contains("chain_id") && 
            v.description.contains("0")
        ));
    }

    #[test]
    fn test_immediate_visibility_fails_production() {
        let mut config = OracleConfig::production_btc_polygon();
        config.visibility_rule = OracleVisibilityRule::Immediate;
        
        let result = config.validate_production();
        assert!(!result.is_valid);
        assert!(result.violations.iter().any(|v| 
            v.field == "visibility_rule" && 
            v.description.contains("not production-grade")
        ));
    }

    #[test]
    fn test_abort_on_missing_false_fails_production() {
        let mut config = OracleConfig::production_btc_polygon();
        config.abort_on_missing = false;
        
        let result = config.validate_production();
        assert!(!result.is_valid);
        assert!(result.violations.iter().any(|v| 
            v.field == "abort_on_missing"
        ));
    }

    #[test]
    fn test_mixed_chain_ids_fails() {
        let mut config = OracleConfig::new();
        config.feeds.insert("BTC".to_string(), OracleFeedConfig {
            asset_symbol: "BTC".to_string(),
            chain_id: 137, // Polygon
            feed_proxy_address: "0xc907E116054Ad103354f2D350FD2514433D57F6f".to_string(),
            decimals: 8,
            expected_description: None,
            deviation_threshold: None,
            heartbeat_secs: None,
        });
        config.feeds.insert("ETH".to_string(), OracleFeedConfig {
            asset_symbol: "ETH".to_string(),
            chain_id: 1, // Mainnet - different!
            feed_proxy_address: "0x5f4eC3Df9cbd43714FE2740f5E3616155c5b8419".to_string(),
            decimals: 8,
            expected_description: None,
            deviation_threshold: None,
            heartbeat_secs: None,
        });
        
        let result = config.validate_production();
        assert!(!result.is_valid);
        assert!(result.violations.iter().any(|v| 
            v.description.contains("multiple chain IDs")
        ));
    }

    #[test]
    fn test_valid_production_config_passes() {
        let config = OracleConfig::production_btc_polygon();
        let result = config.validate_production();
        
        assert!(result.is_valid, "Production BTC config should pass: {:?}", result.violations);
        assert!(result.fingerprint_hash.is_some());
    }

    #[test]
    fn test_multi_asset_production_config_passes() {
        let config = OracleConfig::production_multi_asset_polygon();
        let result = config.validate_production();
        
        assert!(result.is_valid, "Multi-asset config should pass: {:?}", result.violations);
        assert_eq!(config.feeds.len(), 4); // BTC, ETH, SOL, XRP
    }

    // =========================================================================
    // FINGERPRINT SENSITIVITY TESTS
    // =========================================================================

    #[test]
    fn test_fingerprint_is_deterministic() {
        let config1 = OracleConfig::production_btc_polygon();
        let config2 = OracleConfig::production_btc_polygon();
        
        assert_eq!(config1.fingerprint_hash(), config2.fingerprint_hash(),
            "Same config should produce same fingerprint");
    }

    #[test]
    fn test_fingerprint_changes_on_reference_rule_change() {
        let mut config1 = OracleConfig::production_btc_polygon();
        let mut config2 = OracleConfig::production_btc_polygon();
        
        config2.reference_rule = SettlementReferenceRule::FirstUpdateAfterCutoff;
        
        assert_ne!(config1.fingerprint_hash(), config2.fingerprint_hash(),
            "Different reference rule should produce different fingerprint");
    }

    #[test]
    fn test_fingerprint_changes_on_feed_address_change() {
        let mut config1 = OracleConfig::production_btc_polygon();
        let mut config2 = OracleConfig::production_btc_polygon();
        
        // Change feed address
        config2.feeds.get_mut("BTC").unwrap().feed_proxy_address = 
            "0x0000000000000000000000000000000000000001".to_string();
        
        assert_ne!(config1.fingerprint_hash(), config2.fingerprint_hash(),
            "Different feed address should produce different fingerprint");
    }

    #[test]
    fn test_fingerprint_changes_on_decimals_change() {
        let mut config1 = OracleConfig::production_btc_polygon();
        let mut config2 = OracleConfig::production_btc_polygon();
        
        config2.feeds.get_mut("BTC").unwrap().decimals = 6;
        
        assert_ne!(config1.fingerprint_hash(), config2.fingerprint_hash(),
            "Different decimals should produce different fingerprint");
    }

    #[test]
    fn test_fingerprint_changes_on_tie_rule_change() {
        let mut config1 = OracleConfig::production_btc_polygon();
        let mut config2 = OracleConfig::production_btc_polygon();
        
        config2.tie_rule = crate::backtest_v2::settlement::TieRule::YesWins;
        
        assert_ne!(config1.fingerprint_hash(), config2.fingerprint_hash(),
            "Different tie rule should produce different fingerprint");
    }

    #[test]
    fn test_fingerprint_changes_on_visibility_rule_change() {
        let mut config1 = OracleConfig::production_btc_polygon();
        let mut config2 = OracleConfig::production_btc_polygon();
        
        config2.visibility_rule = OracleVisibilityRule::FixedDelay { delay_ns: 5_000_000_000 };
        
        assert_ne!(config1.fingerprint_hash(), config2.fingerprint_hash(),
            "Different visibility rule should produce different fingerprint");
    }

    // =========================================================================
    // FEED ID GENERATION TESTS
    // =========================================================================

    #[test]
    fn test_feed_id_is_deterministic() {
        let feed1 = OracleFeedConfig {
            asset_symbol: "BTC".to_string(),
            chain_id: 137,
            feed_proxy_address: "0xc907E116054Ad103354f2D350FD2514433D57F6f".to_string(),
            decimals: 8,
            expected_description: None,
            deviation_threshold: None,
            heartbeat_secs: None,
        };
        let feed2 = OracleFeedConfig {
            asset_symbol: "BTC".to_string(),
            chain_id: 137,
            feed_proxy_address: "0xc907E116054Ad103354f2D350FD2514433D57F6f".to_string(),
            decimals: 8,
            expected_description: None,
            deviation_threshold: None,
            heartbeat_secs: None,
        };
        
        assert_eq!(feed1.feed_id(), feed2.feed_id());
    }

    #[test]
    fn test_feed_id_includes_asset_chain_address() {
        let feed = OracleFeedConfig {
            asset_symbol: "BTC".to_string(),
            chain_id: 137,
            feed_proxy_address: "0xc907E116054Ad103354f2D350FD2514433D57F6f".to_string(),
            decimals: 8,
            expected_description: None,
            deviation_threshold: None,
            heartbeat_secs: None,
        };
        
        let id = feed.feed_id();
        assert!(id.contains("btc"), "Feed ID should contain asset: {}", id);
        assert!(id.contains("137"), "Feed ID should contain chain: {}", id);
    }

    // =========================================================================
    // STORAGE IDEMPOTENCY TESTS
    // =========================================================================

    #[test]
    fn test_storage_idempotency() {
        let storage = OracleRoundStorage::open_memory().unwrap();
        
        let round = ChainlinkRound {
            feed_id: "test_feed".to_string(),
            round_id: 100,
            answer: 50000_00000000,
            updated_at: 1700000000,
            answered_in_round: 100,
            started_at: 1700000000,
            ingest_arrival_time_ns: 1700000000_000_000_000,
            ingest_seq: 0,
            decimals: 8,
            asset_symbol: "BTC".to_string(),
            raw_source_hash: None,
        };
        
        // Store once
        storage.store_round(&round).unwrap();
        let count1 = storage.count_rounds("test_feed").unwrap();
        
        // Store again (same round)
        storage.store_round(&round).unwrap();
        let count2 = storage.count_rounds("test_feed").unwrap();
        
        assert_eq!(count1, count2, "Storing same round twice should not duplicate");
        assert_eq!(count1, 1);
    }

    #[test]
    fn test_batch_storage_idempotency() {
        let storage = OracleRoundStorage::open_memory().unwrap();
        
        let rounds = vec![
            ChainlinkRound {
                feed_id: "test_feed".to_string(),
                round_id: 1,
                answer: 50000_00000000,
                updated_at: 1000,
                answered_in_round: 1,
                started_at: 1000,
                ingest_arrival_time_ns: 1000_000_000_000,
                ingest_seq: 0,
                decimals: 8,
                asset_symbol: "BTC".to_string(),
                raw_source_hash: None,
            },
            ChainlinkRound {
                feed_id: "test_feed".to_string(),
                round_id: 2,
                answer: 50100_00000000,
                updated_at: 1100,
                answered_in_round: 2,
                started_at: 1100,
                ingest_arrival_time_ns: 1100_000_000_000,
                ingest_seq: 1,
                decimals: 8,
                asset_symbol: "BTC".to_string(),
                raw_source_hash: None,
            },
        ];
        
        // Store batch
        let stored1 = storage.store_rounds(&rounds).unwrap();
        let count1 = storage.count_rounds("test_feed").unwrap();
        
        // Store same batch again
        let stored2 = storage.store_rounds(&rounds).unwrap();
        let count2 = storage.count_rounds("test_feed").unwrap();
        
        assert_eq!(count1, count2, "Re-storing same batch should not duplicate");
        assert_eq!(count1, 2);
        assert!(stored2 <= stored1, "Second store should store fewer or equal rounds");
    }

    // =========================================================================
    // SETTLEMENT SELECTION BOUNDARY TESTS
    // =========================================================================

    #[test]
    fn test_settlement_at_or_before_cutoff() {
        let rounds = vec![
            ChainlinkRound {
                feed_id: "test".to_string(),
                round_id: 1,
                answer: 50000_00000000,
                updated_at: 1000,
                answered_in_round: 1,
                started_at: 1000,
                ingest_arrival_time_ns: 1000_500_000_000,
                ingest_seq: 0,
                decimals: 8,
                asset_symbol: "BTC".to_string(),
                raw_source_hash: None,
            },
            ChainlinkRound {
                feed_id: "test".to_string(),
                round_id: 2,
                answer: 50100_00000000,
                updated_at: 1100,
                answered_in_round: 2,
                started_at: 1100,
                ingest_arrival_time_ns: 1100_500_000_000,
                ingest_seq: 1,
                decimals: 8,
                asset_symbol: "BTC".to_string(),
                raw_source_hash: None,
            },
        ];
        
        let source = ChainlinkSettlementSource::from_rounds(
            rounds,
            "BTC".to_string(),
            "test".to_string(),
        );
        
        // Cutoff at 1050 - should get round 1 (at_or_before)
        let price = source.reference_price_at_or_before(1050).unwrap();
        assert_eq!(price.round_id, 1);
        
        // Cutoff at 1100 - should get round 2 (exactly at)
        let price = source.reference_price_at_or_before(1100).unwrap();
        assert_eq!(price.round_id, 2);
        
        // Cutoff at 999 - should get None (before all)
        assert!(source.reference_price_at_or_before(999).is_none());
    }

    #[test]
    fn test_settlement_first_after_cutoff() {
        let rounds = vec![
            ChainlinkRound {
                feed_id: "test".to_string(),
                round_id: 1,
                answer: 50000_00000000,
                updated_at: 1000,
                answered_in_round: 1,
                started_at: 1000,
                ingest_arrival_time_ns: 1000_500_000_000,
                ingest_seq: 0,
                decimals: 8,
                asset_symbol: "BTC".to_string(),
                raw_source_hash: None,
            },
            ChainlinkRound {
                feed_id: "test".to_string(),
                round_id: 2,
                answer: 50100_00000000,
                updated_at: 1100,
                answered_in_round: 2,
                started_at: 1100,
                ingest_arrival_time_ns: 1100_500_000_000,
                ingest_seq: 1,
                decimals: 8,
                asset_symbol: "BTC".to_string(),
                raw_source_hash: None,
            },
        ];
        
        let source = ChainlinkSettlementSource::from_rounds(
            rounds,
            "BTC".to_string(),
            "test".to_string(),
        );
        
        // Cutoff at 999 - should get round 1 (first after)
        let price = source.reference_price_first_after(999).unwrap();
        assert_eq!(price.round_id, 1);
        
        // Cutoff at 1000 - should get round 2 (first strictly after)
        let price = source.reference_price_first_after(1000).unwrap();
        assert_eq!(price.round_id, 2);
        
        // Cutoff at 1100 - should get None (after all)
        assert!(source.reference_price_first_after(1100).is_none());
    }

    #[test]
    fn test_settlement_boundary_minus_epsilon() {
        let rounds = vec![
            ChainlinkRound {
                feed_id: "test".to_string(),
                round_id: 1,
                answer: 50000_00000000,
                updated_at: 1000,
                answered_in_round: 1,
                started_at: 1000,
                ingest_arrival_time_ns: 1000_500_000_000,
                ingest_seq: 0,
                decimals: 8,
                asset_symbol: "BTC".to_string(),
                raw_source_hash: None,
            },
        ];
        
        let source = ChainlinkSettlementSource::from_rounds(
            rounds,
            "BTC".to_string(),
            "test".to_string(),
        );
        
        // Just before the round - should not find
        assert!(source.reference_price_at_or_before(999).is_none());
        
        // Exactly at - should find
        assert!(source.reference_price_at_or_before(1000).is_some());
    }

    // =========================================================================
    // VISIBILITY SEMANTICS TESTS
    // =========================================================================

    #[test]
    fn test_outcome_knowable_respects_arrival_time() {
        let rounds = vec![
            ChainlinkRound {
                feed_id: "test".to_string(),
                round_id: 1,
                answer: 50000_00000000,
                updated_at: 1000, // Oracle source time
                answered_in_round: 1,
                started_at: 1000,
                ingest_arrival_time_ns: 1005_000_000_000, // Arrives 5 seconds later
                ingest_seq: 0,
                decimals: 8,
                asset_symbol: "BTC".to_string(),
                raw_source_hash: None,
            },
        ];
        
        let source = ChainlinkSettlementSource::from_rounds(
            rounds,
            "BTC".to_string(),
            "test".to_string(),
        );
        
        let cutoff = 1000;
        
        // Decision time before arrival - NOT knowable
        let decision_before = 1002_000_000_000; // 1002 seconds (before 1005)
        assert!(!source.is_outcome_knowable(
            decision_before,
            cutoff,
            SettlementReferenceRule::LastUpdateAtOrBeforeCutoff
        ), "Outcome should not be knowable before arrival");
        
        // Decision time after arrival - IS knowable
        let decision_after = 1006_000_000_000; // 1006 seconds (after 1005)
        assert!(source.is_outcome_knowable(
            decision_after,
            cutoff,
            SettlementReferenceRule::LastUpdateAtOrBeforeCutoff
        ), "Outcome should be knowable after arrival");
    }
}

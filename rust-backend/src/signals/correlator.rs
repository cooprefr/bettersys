//! Multi-Signal Correlation Engine
//! Mission: Detect patterns and correlations across multiple signal types
//! Philosophy: Multiple confirming signals > single signal

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{debug, info};

use crate::models::MarketSignal;
use crate::signals::db_storage::DbSignalStorage;

/// Pattern types detected by correlator
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum PatternType {
    /// Whale trade + arbitrage opportunity on same market
    WhaleArbitrageAlignment,
    /// Multiple whales buying same market
    MultiWhaleConsensus,
    /// Similar to historically profitable pattern
    HistoricalRepeat,
    /// Unusual volume spike detected
    VolumeSpike,
    /// Custom pattern
    Custom(String),
}

/// Composite signal combining multiple individual signals
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompositeSignal {
    pub id: String,
    pub market_slug: String,
    pub component_signals: Vec<String>, // Signal IDs
    pub composite_confidence: f64,
    pub correlation_score: f64,
    pub pattern_type: PatternType,
    pub expected_return: f64,
    pub risk_score: f64,
    pub detected_at: String,
    pub description: String,
}

/// Configuration for signal correlation
#[derive(Debug, Clone)]
pub struct CorrelatorConfig {
    pub min_correlation: f64, // Minimum correlation score (0.0-1.0)
    pub lookback_hours: i64,  // How far back to look for signals
    pub min_signals: usize,   // Minimum signals required for pattern
}

impl Default for CorrelatorConfig {
    fn default() -> Self {
        Self {
            min_correlation: 0.6,
            lookback_hours: 24,
            min_signals: 2,
        }
    }
}

/// Multi-signal correlation engine
pub struct SignalCorrelator {
    storage: Arc<DbSignalStorage>,
    config: CorrelatorConfig,
}

impl SignalCorrelator {
    /// Create new signal correlator
    pub fn new(storage: Arc<DbSignalStorage>, config: CorrelatorConfig) -> Self {
        Self { storage, config }
    }

    /// Analyze correlations across all recent signals
    pub async fn analyze_correlations(&self) -> Result<Vec<CompositeSignal>> {
        info!(
            "🔗 Analyzing signal correlations (lookback: {}h)",
            self.config.lookback_hours
        );

        // Get recent signals
        let signals = self
            .storage
            .get_recent(1000)
            .context("Failed to fetch recent signals")?;

        if signals.len() < self.config.min_signals {
            debug!("Insufficient signals for correlation analysis");
            return Ok(Vec::new());
        }

        // Group signals by market
        let mut by_market: HashMap<String, Vec<MarketSignal>> = HashMap::new();
        for signal in signals {
            by_market
                .entry(signal.market_slug.clone())
                .or_insert_with(Vec::new)
                .push(signal);
        }

        let mut composite_signals = Vec::new();

        // Detect patterns in each market
        for (market_slug, market_signals) in by_market {
            if market_signals.len() < self.config.min_signals {
                continue;
            }

            // Check for whale-arbitrage alignment
            if let Some(composite) =
                self.detect_whale_arbitrage_alignment(&market_slug, &market_signals)
            {
                composite_signals.push(composite);
            }

            // Check for multi-whale consensus
            if let Some(composite) =
                self.detect_multi_whale_consensus(&market_slug, &market_signals)
            {
                composite_signals.push(composite);
            }

            // Check for volume spikes
            if let Some(composite) = self.detect_volume_spike(&market_slug, &market_signals) {
                composite_signals.push(composite);
            }
        }

        info!("✅ Found {} composite signals", composite_signals.len());
        Ok(composite_signals)
    }

    /// Find aligned signals for a specific market
    pub async fn find_aligned_signals(&self, market_slug: &str) -> Result<Option<CompositeSignal>> {
        debug!("🎯 Checking alignment for market: {}", market_slug);

        let signals = self
            .storage
            .get_by_market(market_slug, 50)
            .context("Failed to fetch signals for market")?;

        if signals.len() < self.config.min_signals {
            return Ok(None);
        }

        // Try each pattern type
        if let Some(composite) = self.detect_whale_arbitrage_alignment(market_slug, &signals) {
            return Ok(Some(composite));
        }

        if let Some(composite) = self.detect_multi_whale_consensus(market_slug, &signals) {
            return Ok(Some(composite));
        }

        Ok(None)
    }

    /// Detect whale trade + arbitrage opportunity alignment
    fn detect_whale_arbitrage_alignment(
        &self,
        market_slug: &str,
        signals: &[MarketSignal],
    ) -> Option<CompositeSignal> {
        // Look for both whale trades and arbitrage signals
        let whale_signals: Vec<&MarketSignal> = signals
            .iter()
            .filter(|s| {
                matches!(
                    s.signal_type,
                    crate::models::SignalType::WhaleFollowing { .. }
                        | crate::models::SignalType::EliteWallet { .. }
                        | crate::models::SignalType::InsiderWallet { .. }
                        | crate::models::SignalType::TrackedWalletEntry { .. }
                )
            })
            .collect();

        let arb_signals: Vec<&MarketSignal> = signals
            .iter()
            .filter(|s| {
                matches!(
                    s.signal_type,
                    crate::models::SignalType::CrossPlatformArbitrage { .. }
                )
            })
            .collect();

        if whale_signals.is_empty() || arb_signals.is_empty() {
            return None;
        }

        // Calculate composite confidence (weighted average)
        let whale_conf: f64 =
            whale_signals.iter().map(|s| s.confidence).sum::<f64>() / whale_signals.len() as f64;
        let arb_conf: f64 =
            arb_signals.iter().map(|s| s.confidence).sum::<f64>() / arb_signals.len() as f64;

        let composite_confidence = (whale_conf * 0.6 + arb_conf * 0.4).min(0.99);
        let correlation_score = 0.85; // High correlation when both present

        // Estimate expected return from arbitrage signals
        let expected_return = arb_signals
            .iter()
            .filter_map(|s| {
                if let crate::models::SignalType::CrossPlatformArbitrage { spread_pct, .. } =
                    &s.signal_type
                {
                    Some(*spread_pct)
                } else {
                    None
                }
            })
            .max_by(|a, b| a.partial_cmp(b).unwrap())
            .unwrap_or(0.0);

        let component_ids: Vec<String> = whale_signals
            .iter()
            .chain(arb_signals.iter())
            .map(|s| s.id.clone())
            .collect();

        Some(CompositeSignal {
            id: format!("composite_{}", chrono::Utc::now().timestamp_millis()),
            market_slug: market_slug.to_string(),
            component_signals: component_ids.clone(),
            composite_confidence,
            correlation_score,
            pattern_type: PatternType::WhaleArbitrageAlignment,
            expected_return,
            risk_score: 1.0 - composite_confidence,
            detected_at: chrono::Utc::now().to_rfc3339(),
            description: format!(
                "STRONG SIGNAL: {} whale trades + {} arbitrage opportunities aligned on '{}' (confidence: {:.1}%, expected return: {:.2}%)",
                whale_signals.len(),
                arb_signals.len(),
                market_slug,
                composite_confidence * 100.0,
                expected_return * 100.0
            ),
        })
    }

    /// Detect multiple whales buying the same market
    fn detect_multi_whale_consensus(
        &self,
        market_slug: &str,
        signals: &[MarketSignal],
    ) -> Option<CompositeSignal> {
        let whale_signals: Vec<&MarketSignal> = signals
            .iter()
            .filter(|s| {
                matches!(
                    s.signal_type,
                    crate::models::SignalType::WhaleFollowing { .. }
                        | crate::models::SignalType::EliteWallet { .. }
                        | crate::models::SignalType::InsiderWallet { .. }
                        | crate::models::SignalType::TrackedWalletEntry { .. }
                )
            })
            .collect();

        if whale_signals.len() < 2 {
            return None;
        }

        // Calculate composite confidence (higher when more whales agree)
        let avg_confidence: f64 =
            whale_signals.iter().map(|s| s.confidence).sum::<f64>() / whale_signals.len() as f64;

        // Boost confidence based on number of whales
        let consensus_boost = ((whale_signals.len() - 1) as f64 * 0.05).min(0.15);
        let composite_confidence = (avg_confidence + consensus_boost).min(0.99);

        // Correlation score based on how closely they agree
        let correlation_score = if whale_signals.len() >= 3 { 0.90 } else { 0.75 };

        // Estimate expected return (conservative)
        let expected_return = 0.05; // 5% estimated for consensus trades

        let component_ids: Vec<String> = whale_signals.iter().map(|s| s.id.clone()).collect();

        Some(CompositeSignal {
            id: format!("composite_{}", chrono::Utc::now().timestamp_millis()),
            market_slug: market_slug.to_string(),
            component_signals: component_ids,
            composite_confidence,
            correlation_score,
            pattern_type: PatternType::MultiWhaleConsensus,
            expected_return,
            risk_score: 1.0 - composite_confidence,
            detected_at: chrono::Utc::now().to_rfc3339(),
            description: format!(
                "MULTI-WHALE CONSENSUS: {} elite traders buying '{}' (confidence: {:.1}%)",
                whale_signals.len(),
                market_slug,
                composite_confidence * 100.0
            ),
        })
    }

    /// Detect unusual volume spikes
    fn detect_volume_spike(
        &self,
        market_slug: &str,
        signals: &[MarketSignal],
    ) -> Option<CompositeSignal> {
        // Count recent signals as proxy for volume
        let recent_count = signals.len();

        // Only trigger if we see unusual activity (>5 signals in lookback)
        if recent_count < 5 {
            return None;
        }

        let avg_confidence: f64 =
            signals.iter().map(|s| s.confidence).sum::<f64>() / signals.len() as f64;

        let composite_confidence = (avg_confidence + 0.05).min(0.95);
        let correlation_score = 0.70;
        let expected_return = 0.03; // 3% estimated for volume spikes

        let component_ids: Vec<String> = signals.iter().map(|s| s.id.clone()).collect();

        Some(CompositeSignal {
            id: format!("composite_{}", chrono::Utc::now().timestamp_millis()),
            market_slug: market_slug.to_string(),
            component_signals: component_ids,
            composite_confidence,
            correlation_score,
            pattern_type: PatternType::VolumeSpike,
            expected_return,
            risk_score: 1.0 - composite_confidence,
            detected_at: chrono::Utc::now().to_rfc3339(),
            description: format!(
                "VOLUME SPIKE: {} signals detected on '{}' in {}h window (confidence: {:.1}%)",
                recent_count,
                market_slug,
                self.config.lookback_hours,
                composite_confidence * 100.0
            ),
        })
    }

    /// Calculate composite confidence from multiple signals
    pub fn calculate_composite_confidence(&self, signals: &[MarketSignal]) -> f64 {
        if signals.is_empty() {
            return 0.0;
        }

        // Weighted average with diminishing returns for more signals
        let sum: f64 = signals.iter().map(|s| s.confidence).sum();
        let avg = sum / signals.len() as f64;

        // Boost for multiple confirming signals (up to 15% boost)
        let consensus_boost = ((signals.len() - 1) as f64 * 0.03).min(0.15);

        (avg + consensus_boost).min(0.99)
    }

    /// Detect all patterns for a given set of signals
    pub async fn detect_patterns(&self) -> Result<Vec<CompositeSignal>> {
        self.analyze_correlations().await
    }
}

#[cfg(test)]
#[allow(dead_code, unused_imports, unused_variables)]
mod tests {
    use super::*;
    use crate::models::{SignalDetails, SignalType};

    fn create_test_signal(market: &str, signal_type: SignalType, confidence: f64) -> MarketSignal {
        MarketSignal {
            id: format!("sig_{}", chrono::Utc::now().timestamp_millis()),
            signal_type,
            market_slug: market.to_string(),
            confidence,
            risk_level: "medium".to_string(),
            details: SignalDetails {
                market_id: format!("{}-id", market),
                market_title: format!("Test signal for {}", market),
                current_price: 0.5,
                volume_24h: 1000.0,
                liquidity: 5000.0,
                recommended_action: "HOLD".to_string(),
                expiry_time: None,
                observed_timestamp: None,
                signal_family: None,
                calibration_version: None,
                guardrail_flags: None,
                recommended_size: None,
            },
            detected_at: chrono::Utc::now().to_rfc3339(),
            source: "test".to_string(),
        }
    }

    #[test]
    fn test_composite_confidence_calculation() {
        let storage = Arc::new(DbSignalStorage::new(":memory:").unwrap());
        let correlator = SignalCorrelator::new(storage, CorrelatorConfig::default());

        let signals = vec![
            create_test_signal(
                "test-market",
                SignalType::WhaleFollowing {
                    whale_address: "0x123".to_string(),
                    position_size: 1000.0,
                    confidence_score: 0.80,
                },
                0.80,
            ),
            create_test_signal(
                "test-market",
                SignalType::WhaleFollowing {
                    whale_address: "0x456".to_string(),
                    position_size: 1500.0,
                    confidence_score: 0.85,
                },
                0.85,
            ),
        ];

        let confidence = correlator.calculate_composite_confidence(&signals);

        // Should be average (0.825) + small boost (~0.03) = ~0.855
        assert!(confidence > 0.84 && confidence < 0.87);
    }

    #[test]
    fn test_pattern_detection_whale_arbitrage() {
        let storage = Arc::new(DbSignalStorage::new(":memory:").unwrap());
        let correlator = SignalCorrelator::new(storage, CorrelatorConfig::default());

        let signals = vec![
            create_test_signal(
                "test-market",
                SignalType::WhaleFollowing {
                    whale_address: "0x123".to_string(),
                    position_size: 5000.0,
                    confidence_score: 0.85,
                },
                0.85,
            ),
            create_test_signal(
                "test-market",
                SignalType::CrossPlatformArbitrage {
                    polymarket_price: 0.52,
                    kalshi_price: Some(0.47),
                    spread_pct: 0.05,
                },
                0.75,
            ),
        ];

        let composite = correlator.detect_whale_arbitrage_alignment("test-market", &signals);

        assert!(composite.is_some());
        let comp = composite.unwrap();
        assert_eq!(comp.pattern_type, PatternType::WhaleArbitrageAlignment);
        assert!(comp.composite_confidence > 0.75);
        assert_eq!(comp.component_signals.len(), 2);
    }

    #[test]
    fn test_pattern_detection_multi_whale() {
        let storage = Arc::new(DbSignalStorage::new(":memory:").unwrap());
        let correlator = SignalCorrelator::new(storage, CorrelatorConfig::default());

        let signals = vec![
            create_test_signal(
                "test-market",
                SignalType::WhaleFollowing {
                    whale_address: "0x111".to_string(),
                    position_size: 3000.0,
                    confidence_score: 0.80,
                },
                0.80,
            ),
            create_test_signal(
                "test-market",
                SignalType::WhaleFollowing {
                    whale_address: "0x222".to_string(),
                    position_size: 4000.0,
                    confidence_score: 0.85,
                },
                0.85,
            ),
            create_test_signal(
                "test-market",
                SignalType::WhaleFollowing {
                    whale_address: "0x333".to_string(),
                    position_size: 2500.0,
                    confidence_score: 0.82,
                },
                0.82,
            ),
        ];

        let composite = correlator.detect_multi_whale_consensus("test-market", &signals);

        assert!(composite.is_some());
        let comp = composite.unwrap();
        assert_eq!(comp.pattern_type, PatternType::MultiWhaleConsensus);
        assert!(comp.composite_confidence > 0.85); // Boosted by consensus
        assert_eq!(comp.component_signals.len(), 3);
    }
}

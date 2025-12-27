//! Signal Detection Engine
//! Pilot in Command: Alpha Discovery
//! Mission: Detect market inefficiencies faster than the speed of light
//!
//! Optimizations:
//! - Pre-allocated vectors with capacity hints
//! - Inline hot functions
//! - Reduced string allocations

use crate::models::{MarketSignal, PolymarketEvent, SignalDetails, SignalType};
use chrono::Utc;

pub struct SignalDetector {
    confidence_threshold: f64,
}

impl SignalDetector {
    #[inline]
    pub fn new() -> Self {
        Self {
            confidence_threshold: 0.6,
        }
    }

    /// Detect all signals from market data
    #[inline]
    pub async fn detect_all(&self, events: &[PolymarketEvent]) -> Vec<MarketSignal> {
        // Pre-allocate with reasonable capacity
        let mut signals = Vec::with_capacity(events.len() * 2);

        for event in events {
            // Detect price deviation signals
            if let Some(signal) = self.detect_price_deviation(event) {
                signals.push(signal);
            }

            // Detect market expiry edge
            if let Some(signal) = self.detect_expiry_edge(event) {
                signals.push(signal);
            }

            // Detect volume anomalies
            if let Some(signal) = self.detect_volume_spike(event) {
                signals.push(signal);
            }
        }

        signals
    }

    fn detect_price_deviation(&self, event: &PolymarketEvent) -> Option<MarketSignal> {
        for market in &event.markets {
            if market.outcome_prices.len() >= 2 {
                let yes_price = market.outcome_prices[0];
                let no_price = market.outcome_prices[1];

                // Check if prices don't sum to ~1.0 (arbitrage opportunity)
                let sum = yes_price + no_price;
                let deviation = (sum - 1.0).abs();

                if deviation > 0.02 {
                    return Some(MarketSignal {
                        id: format!("dev_{}", market.id),
                        signal_type: SignalType::PriceDeviation {
                            market_price: yes_price,
                            fair_value: 1.0 - no_price,
                            deviation_pct: deviation * 100.0,
                        },
                        market_slug: event.slug.clone(),
                        confidence: 0.8 + deviation.min(0.1),
                        risk_level: if deviation > 0.05 { "low" } else { "medium" }.to_string(),
                        details: SignalDetails {
                            market_id: market.id.clone(),
                            market_title: market.question.clone(),
                            current_price: yes_price,
                            volume_24h: market.volume.unwrap_or(0.0),
                            liquidity: market.liquidity.unwrap_or(0.0),
                            recommended_action: if sum < 1.0 { "BUY_BOTH" } else { "SELL_BOTH" }
                                .to_string(),
                            expiry_time: event.end_date_iso.clone(),
                            observed_timestamp: None,
                            signal_family: None,
                            calibration_version: None,
                            guardrail_flags: None,
                            recommended_size: None,
                        },
                        detected_at: Utc::now().to_rfc3339(),
                        source: "detector".to_string(),
                    });
                }
            }
        }
        None
    }

    fn detect_expiry_edge(&self, event: &PolymarketEvent) -> Option<MarketSignal> {
        if let Some(end_date) = &event.end_date_iso {
            if let Ok(expiry) = chrono::DateTime::parse_from_rfc3339(end_date) {
                let now = Utc::now();
                let hours_to_expiry = (expiry.timestamp() - now.timestamp()) as f64 / 3600.0;

                // Detect opportunities near expiry
                if hours_to_expiry > 0.0 && hours_to_expiry < 48.0 {
                    let volume_spike = event.volume.unwrap_or(0.0) / 100000.0;

                    if volume_spike > 1.0 {
                        return Some(MarketSignal {
                            id: format!("exp_{}", event.id),
                            signal_type: SignalType::MarketExpiryEdge {
                                hours_to_expiry,
                                volume_spike,
                            },
                            market_slug: event.slug.clone(),
                            confidence: (0.5 + (48.0 - hours_to_expiry) / 96.0).min(0.9),
                            risk_level: if hours_to_expiry < 12.0 {
                                "high"
                            } else {
                                "medium"
                            }
                            .to_string(),
                            details: SignalDetails {
                                market_id: event.id.clone(),
                                market_title: event.title.clone(),
                                current_price: 0.0,
                                volume_24h: event.volume.unwrap_or(0.0),
                                liquidity: event.liquidity.unwrap_or(0.0),
                                recommended_action: "MONITOR".to_string(),
                                expiry_time: Some(end_date.clone()),
                                observed_timestamp: None,
                                signal_family: None,
                                calibration_version: None,
                                guardrail_flags: None,
                                recommended_size: None,
                            },
                            detected_at: Utc::now().to_rfc3339(),
                            source: "detector".to_string(),
                        });
                    }
                }
            }
        }
        None
    }

    fn detect_volume_spike(&self, event: &PolymarketEvent) -> Option<MarketSignal> {
        let volume = event.volume.unwrap_or(0.0);
        let liquidity = event.liquidity.unwrap_or(0.0);

        // Detect unusual volume/liquidity ratio
        if volume > 0.0 && liquidity > 0.0 {
            let ratio = volume / liquidity;

            if ratio > 5.0 {
                return Some(MarketSignal {
                    id: format!("vol_{}", event.id),
                    signal_type: SignalType::PriceDeviation {
                        market_price: 0.5,
                        fair_value: 0.5,
                        deviation_pct: ratio,
                    },
                    market_slug: event.slug.clone(),
                    confidence: (0.6 + ratio / 20.0).min(0.95),
                    risk_level: "medium".to_string(),
                    details: SignalDetails {
                        market_id: event.id.clone(),
                        market_title: event.title.clone(),
                        current_price: 0.5,
                        volume_24h: volume,
                        liquidity,
                        recommended_action: "ANALYZE".to_string(),
                        expiry_time: event.end_date_iso.clone(),
                        observed_timestamp: None,
                        signal_family: None,
                        calibration_version: None,
                        guardrail_flags: None,
                        recommended_size: None,
                    },
                    detected_at: Utc::now().to_rfc3339(),
                    source: "detector".to_string(),
                });
            }
        }
        None
    }

    /// Detect tracked wallet entries from Dome API orders
    ///
    /// Mission-critical: No missed entries. Each order represents alpha.
    pub fn detect_trader_entry(
        &self,
        orders: &[crate::scrapers::dome_tracker::DomeOrder],
        wallet_address: &str,
        wallet_label: &str,
    ) -> Vec<MarketSignal> {
        let mut signals = Vec::new();

        for order in orders {
            // Calculate position value
            let position_value = order.shares_normalized * order.price;

            // Filter: only positions >= $1 (captures all meaningful WebSocket trades)
            // Small trades from active wallets provide real-time signal flow
            if position_value < 1.0 {
                continue;
            }

            // Format description based on wallet type
            let side_upper = order.side.to_uppercase();
            let description = if wallet_label.starts_with("insider") {
                let category = wallet_label
                    .split('_')
                    .nth(1)
                    .unwrap_or("unknown")
                    .to_uppercase();

                format!(
                    "INSIDER ENTRY [{}]: ~${:.0} {} on '{}' ({}) by {} @ {:.3}",
                    category,
                    position_value,
                    side_upper,
                    order.title,
                    order.market_slug,
                    &wallet_address[..10], // Truncate address for display
                    order.price
                )
            } else if wallet_label == "world_class" {
                format!(
                    "WORLD CLASS TRADER ENTRY: ~${:.0} {} on '{}' by {} @ {:.3}",
                    position_value,
                    side_upper,
                    order.title,
                    &wallet_address[..10],
                    order.price
                )
            } else {
                format!(
                    "TRACKED WALLET ENTRY: ~${:.0} {} on '{}' by {}",
                    position_value,
                    side_upper,
                    order.title,
                    &wallet_address[..10]
                )
            };

            // Confidence scoring based on insider category and position size
            // Base confidence by category (verified insiders have higher base)
            let base_confidence = match wallet_label {
                "insider_politics" => 0.90, // Highest - political insiders are most reliable
                "insider_finance" => 0.88,  // Financial sector insiders
                "insider_tech" => 0.87,     // Tech sector insiders
                "insider_crypto" => 0.86,   // Crypto market insiders
                "insider_sports" => 0.85,   // Sports betting insiders
                "insider_entertainment" => 0.84, // Entertainment industry insiders
                "high_frequency_test" => 0.50, // Test wallet - low confidence
                _ => 0.80,                  // Other tracked wallets
            };

            // Size bonus: larger positions = higher confidence (max +5%)
            let size_bonus = (position_value / 10000.0).min(5.0) / 100.0;
            let confidence = (base_confidence + size_bonus).min(0.95);

            // Risk level based on category and size
            let risk_level = if wallet_label == "high_frequency_test" {
                "high" // Test wallet is high risk
            } else if position_value > 50000.0 {
                "low" // Large positions from verified insiders = low risk
            } else if wallet_label.starts_with("insider_") {
                "low" // All verified insiders = low risk
            } else {
                "medium"
            };

            let stable_id = order
                .order_hash
                .clone()
                .or_else(|| order.tx_hash.clone())
                .unwrap_or_else(|| format!("{}_{}", wallet_address, order.timestamp));

            let recommended_action = if side_upper == "SELL" {
                "FOLLOW_SELL"
            } else {
                "FOLLOW_BUY"
            };

            signals.push(MarketSignal {
                id: format!("dome_order_{}", stable_id),
                signal_type: SignalType::TrackedWalletEntry {
                    wallet_address: wallet_address.to_string(),
                    wallet_label: wallet_label.to_string(),
                    position_value_usd: position_value,
                    order_count: 1,
                    token_label: order.token_label.clone(), // "Up", "Down", "Yes", "No"
                },
                market_slug: order.market_slug.clone(),
                confidence,
                risk_level: risk_level.to_string(),
                details: SignalDetails {
                    market_id: order.token_id.clone(),
                    // `market_title` should be the actual market title/question, not the signal headline.
                    // The frontend can render the headline from `signal_type`.
                    market_title: order.title.clone(),
                    current_price: order.price,
                    volume_24h: position_value,
                    liquidity: 0.0, // Not available from orders endpoint
                    recommended_action: recommended_action.to_string(),
                    expiry_time: None,
                    observed_timestamp: None,
                    signal_family: None,
                    calibration_version: None,
                    guardrail_flags: None,
                    recommended_size: None,
                },
                detected_at: Utc::now().to_rfc3339(),
                source: "dome".to_string(),
            });
        }

        signals
    }
}

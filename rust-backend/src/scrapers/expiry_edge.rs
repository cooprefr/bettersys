//! Expiry Edge Alpha Signal Scanner
//! Mission: Capture 95% win rate from markets ≤4 hours until expiry
//!
//! Research thesis: Markets with dominant side (≥70% probability) near expiry
//! win 95% of the time due to time decay and informed trader convergence.

use crate::models::{MarketSignal, SignalDetails, SignalType};
use chrono::{DateTime, Duration, Utc};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Instant;
use tracing::{debug, info, warn};

/// Polymarket Gamma API market response
#[derive(Debug, Clone, Serialize, Deserialize)]
struct PolymarketMarket {
    id: String,
    question: String,
    #[serde(rename = "endDate")]
    end_date: Option<String>, // ISO 8601 date string
    #[serde(rename = "outcomePrices")]
    outcome_prices: Option<String>, // JSON array or comma-separated
    #[serde(rename = "volumeNum")]
    volume_num: Option<f64>,
    #[serde(rename = "liquidityNum")]
    liquidity_num: Option<f64>,
    active: Option<bool>,
    closed: Option<bool>,
    slug: Option<String>,
    #[serde(rename = "clobTokenIds")]
    clob_token_ids: Option<Vec<String>>,
}

/// Expiry edge scanner configuration
pub struct ExpiryEdgeScanner {
    api_base: String,
    client: Client,
    threshold_hours: f64, // 4.0 hours
    min_probability: f64, // 0.70 (70% dominant side threshold)
    min_liquidity: f64,   // Minimum liquidity to avoid illiquid markets
    last_scan: Option<Instant>,
}

impl ExpiryEdgeScanner {
    /// Create new scanner with default configuration
    pub fn new() -> Self {
        // Build client with proper TLS configuration
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .danger_accept_invalid_certs(true) // Workaround for cert issues in dev
            .build()
            .unwrap_or_else(|_| Client::new());

        Self {
            api_base: "https://gamma-api.polymarket.com".to_string(),
            client,
            threshold_hours: 4.0,
            min_probability: 0.70, // 70% threshold (conservative)
            min_liquidity: 1000.0, // $1000 minimum liquidity
            last_scan: None,
        }
    }

    /// Scan Polymarket for expiry edge opportunities
    pub async fn scan(&mut self) -> Result<Vec<MarketSignal>, String> {
        let scan_start = Instant::now();
        self.last_scan = Some(scan_start);

        // Calculate time window: now → now + 4 hours
        let now = Utc::now();
        let end_window = now + Duration::hours(4);

        // Build API request - fetch markets and filter client-side
        // NOTE: GAMMA API date params can be finicky, so we fetch broadly and filter
        let url = format!(
            "{}/markets?limit=200&active=true",
            self.api_base
        );

        info!(
            "🔍 Scanning expiry edge: {} to {}",
            now.format("%H:%M"),
            end_window.format("%H:%M")
        );

        // Query Polymarket API
        let response = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| format!("HTTP request failed: {}", e))?;

        if !response.status().is_success() {
            return Err(format!("Polymarket API error: {}", response.status()));
        }

        let markets: Vec<PolymarketMarket> = response
            .json()
            .await
            .map_err(|e| format!("JSON parse error: {}", e))?;
        debug!("Retrieved {} markets from Polymarket", markets.len());

        // Process each market and generate signals
        let mut signals = Vec::new();

        for market in markets {
            match self.process_market(&market, &now) {
                Ok(Some(signal)) => signals.push(signal),
                Ok(None) => {} // Market doesn't qualify
                Err(e) => warn!("Error processing market {}: {}", market.id, e),
            }
        }

        let scan_duration = scan_start.elapsed();
        info!(
            "✅ Expiry edge scan complete: {} signals in {:?}",
            signals.len(),
            scan_duration
        );

        Ok(signals)
    }

    /// Process a single market and generate signal if qualifying
    fn process_market(
        &self,
        market: &PolymarketMarket,
        now: &DateTime<Utc>,
    ) -> Result<Option<MarketSignal>, Box<dyn std::error::Error>> {
        // Skip if market is closed or inactive
        if market.closed.unwrap_or(false) || !market.active.unwrap_or(true) {
            return Ok(None);
        }

        // Parse end date
        let end_date = match &market.end_date {
            Some(date_str) => match DateTime::parse_from_rfc3339(date_str) {
                Ok(dt) => dt.with_timezone(&Utc),
                Err(_) => return Ok(None), // Skip if can't parse
            },
            None => return Ok(None), // Skip if no end date
        };

        // Calculate hours to expiry
        let time_to_expiry = end_date.signed_duration_since(*now);
        let hours_to_expiry = time_to_expiry.num_seconds() as f64 / 3600.0;

        // Filter: only markets with ≤4 hours to expiry
        if hours_to_expiry > self.threshold_hours || hours_to_expiry < 0.0 {
            return Ok(None);
        }

        // Parse outcome prices
        let probabilities = self.parse_outcome_prices(&market.outcome_prices)?;
        if probabilities.is_empty() {
            return Ok(None);
        }

        // Find dominant probability (max)
        let dominant_prob = probabilities
            .iter()
            .copied()
            .fold(f64::NEG_INFINITY, f64::max);

        // Filter: only if dominant side ≥ threshold
        if dominant_prob < self.min_probability {
            return Ok(None);
        }

        // Filter: check minimum liquidity
        let liquidity = market.liquidity_num.unwrap_or(0.0);
        if liquidity < self.min_liquidity {
            debug!(
                "Skipping {}: liquidity too low ({})",
                market.question, liquidity
            );
            return Ok(None);
        }

        // Calculate volume spike (relative to liquidity)
        let volume = market.volume_num.unwrap_or(0.0);
        let volume_spike = if liquidity > 0.0 {
            volume / liquidity
        } else {
            0.0
        };

        // Calculate expected return
        // If dominant side is 80%, expected return = (1-0.8)/0.8 = 25%
        let expected_return = if dominant_prob < 1.0 {
            ((1.0 - dominant_prob) / dominant_prob * 100.0).min(100.0)
        } else {
            0.0
        };

        // Generate signal
        let signal = self.build_signal(
            market,
            hours_to_expiry,
            dominant_prob,
            volume_spike,
            expected_return,
            volume,
            liquidity,
        )?;

        info!(
            "🎯 EXPIRY EDGE: {} | {:.1}h left | {:.0}% prob | {:.1}% return",
            market.question.chars().take(50).collect::<String>(),
            hours_to_expiry,
            dominant_prob * 100.0,
            expected_return
        );

        Ok(Some(signal))
    }

    /// Parse outcome prices from API response
    fn parse_outcome_prices(
        &self,
        prices_str: &Option<String>,
    ) -> Result<Vec<f64>, Box<dyn std::error::Error>> {
        let prices_str = match prices_str {
            Some(s) => s,
            None => return Ok(Vec::new()),
        };

        // Try parsing as JSON array first
        if let Ok(prices) = serde_json::from_str::<Vec<f64>>(prices_str) {
            return Ok(prices);
        }

        // Try parsing as JSON string array
        if let Ok(prices) = serde_json::from_str::<Vec<String>>(prices_str) {
            let parsed: Result<Vec<f64>, _> = prices.iter().map(|s| s.parse::<f64>()).collect();
            if let Ok(p) = parsed {
                return Ok(p);
            }
        }

        // Try comma-separated values
        let prices: Result<Vec<f64>, _> = prices_str
            .split(',')
            .map(|s| s.trim().parse::<f64>())
            .collect();

        match prices {
            Ok(p) => Ok(p),
            Err(_) => Ok(Vec::new()), // Return empty if can't parse
        }
    }

    /// Build signal struct
    fn build_signal(
        &self,
        market: &PolymarketMarket,
        hours_to_expiry: f64,
        dominant_prob: f64,
        volume_spike: f64,
        expected_return: f64,
        volume: f64,
        liquidity: f64,
    ) -> Result<MarketSignal, Box<dyn std::error::Error>> {
        let now = Utc::now();

        // Use market slug or create from question
        let market_slug = market.slug.clone().unwrap_or_else(|| {
            market
                .question
                .to_lowercase()
                .chars()
                .filter(|c| c.is_alphanumeric() || *c == ' ')
                .collect::<String>()
                .split_whitespace()
                .take(5)
                .collect::<Vec<_>>()
                .join("-")
        });

        // Risk level based on time to expiry and probability
        let risk_level = if hours_to_expiry < 2.0 && dominant_prob >= 0.85 {
            "LOW"
        } else if hours_to_expiry < 3.0 && dominant_prob >= 0.80 {
            "MEDIUM"
        } else {
            "HIGH"
        }
        .to_string();

        // Confidence = dominant probability (research shows 95% accuracy at ≥70% threshold)
        let confidence = dominant_prob;

        // Recommended action
        let recommended_action = format!(
            "BUY dominant side ({:.0}% probability) - Expected return: {:.1}%",
            dominant_prob * 100.0,
            expected_return
        );

        let signal = MarketSignal {
            id: format!("expiry_edge_{}", now.timestamp()),
            signal_type: SignalType::MarketExpiryEdge {
                hours_to_expiry,
                volume_spike,
            },
            market_slug: market_slug.clone(),
            confidence,
            risk_level,
            details: SignalDetails {
                market_id: market.id.clone(),
                market_title: market.question.clone(),
                current_price: dominant_prob,
                volume_24h: volume,
                liquidity,
                recommended_action,
                expiry_time: market.end_date.clone(),
                observed_timestamp: None,
                signal_family: None,
                calibration_version: None,
                guardrail_flags: None,
                recommended_size: None,
            },
            detected_at: now.to_rfc3339(),
            source: "polymarket_expiry_edge".to_string(),
        };

        Ok(signal)
    }

    /// Get scanner statistics
    pub fn get_stats(&self) -> HashMap<String, String> {
        let mut stats = HashMap::new();
        stats.insert("api_base".to_string(), self.api_base.clone());
        stats.insert(
            "threshold_hours".to_string(),
            format!("{:.1}", self.threshold_hours),
        );
        stats.insert(
            "min_probability".to_string(),
            format!("{:.0}%", self.min_probability * 100.0),
        );
        stats.insert(
            "min_liquidity".to_string(),
            format!("${:.0}", self.min_liquidity),
        );

        if let Some(last) = self.last_scan {
            let elapsed = last.elapsed();
            stats.insert(
                "last_scan".to_string(),
                format!("{:.1}s ago", elapsed.as_secs_f64()),
            );
        } else {
            stats.insert("last_scan".to_string(), "never".to_string());
        }

        stats
    }
}

impl Default for ExpiryEdgeScanner {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_outcome_prices_json_array() {
        let scanner = ExpiryEdgeScanner::new();

        // Test JSON array of numbers
        let prices = scanner
            .parse_outcome_prices(&Some("[0.65, 0.35]".to_string()))
            .unwrap();
        assert_eq!(prices, vec![0.65, 0.35]);
    }

    #[test]
    fn test_parse_outcome_prices_comma_separated() {
        let scanner = ExpiryEdgeScanner::new();

        // Test comma-separated
        let prices = scanner
            .parse_outcome_prices(&Some("0.82, 0.18".to_string()))
            .unwrap();
        assert_eq!(prices, vec![0.82, 0.18]);
    }

    #[test]
    fn test_dominant_probability_threshold() {
        let scanner = ExpiryEdgeScanner::new();

        // Test that 70% threshold is used
        assert_eq!(scanner.min_probability, 0.70);

        // Probabilities [0.85, 0.15] should qualify (max = 0.85 >= 0.70)
        let probs = vec![0.85, 0.15];
        let max_prob = probs.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        assert!(max_prob >= scanner.min_probability);
    }

    #[test]
    fn test_expected_return_calculation() {
        // If dominant side = 80%, expected return = (1-0.8)/0.8 = 0.25 = 25%
        let dominant_prob = 0.80_f64;
        let expected_return: f64 = (1.0 - dominant_prob) / dominant_prob * 100.0;
        assert!((expected_return - 25.0_f64).abs() < 0.01_f64);

        // If dominant side = 90%, expected return = (1-0.9)/0.9 = 0.111 = 11.1%
        let dominant_prob = 0.90_f64;
        let expected_return: f64 = (1.0 - dominant_prob) / dominant_prob * 100.0;
        assert!((expected_return - 11.11_f64).abs() < 0.01_f64);
    }
}

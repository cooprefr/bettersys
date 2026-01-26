//! DomeAPI Real-Time Polling System
//! Mission: Get REAL signals from tracked wallets via REST API
//! Philosophy: Reliability > WebSocket. If WebSocket fails, REST always works.
//!
//! Optimizations:
//! - Connection pooling via reqwest Client
//! - Pre-allocated vectors
//! - Efficient string handling

use anyhow::{Context, Result};
use chrono::Utc;
use parking_lot::Mutex; // Faster than tokio::sync::Mutex for short critical sections
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::time::{sleep, Duration};
use tracing::{debug, info, warn};

use crate::models::{MarketSignal, SignalDetails, SignalType};

const DOME_API_BASE: &str = "https://api.domeapi.io/v1/polymarket";

/// Order data from Dome API
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DomeOrder {
    pub token_id: String,
    #[serde(default)]
    pub token_label: Option<String>, // "Up", "Down", "Yes", "No" - outcome label
    pub side: String, // "BUY" or "SELL"
    pub market_slug: String,
    pub condition_id: String,
    pub shares: Option<f64>,
    pub shares_normalized: f64,
    pub price: f64,
    pub tx_hash: String,
    pub title: String,
    pub timestamp: i64,
    pub order_hash: String,
    pub user: String,
}

/// Orders response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrdersResponse {
    pub orders: Vec<DomeOrder>,
    pub pagination: Option<Pagination>,
}

/// Activity data
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActivityItem {
    pub token_id: String,
    pub side: String, // "REDEEM", "MERGE", "SPLIT"
    pub market_slug: String,
    pub condition_id: String,
    pub shares: f64,
    pub shares_normalized: f64,
    pub price: f64,
    pub tx_hash: String,
    pub title: String,
    pub timestamp: i64,
    pub order_hash: String,
    pub user: String,
}

/// Activity response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActivityResponse {
    pub activities: Vec<ActivityItem>,
    pub pagination: Option<Pagination>,
}

/// Pagination info
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pagination {
    pub limit: i32,
    pub offset: i32,
    pub total: Option<i32>,
    pub count: Option<i32>,
    pub has_more: bool,
}

/// Real-time polling client with connection pooling
#[derive(Clone)]
pub struct DomeRealtimeClient {
    client: Client,
    tracked_wallets: HashMap<String, String>, // address -> label
    last_poll: Arc<Mutex<HashMap<String, i64>>>, // wallet -> last timestamp
}

impl DomeRealtimeClient {
    pub fn new(api_key: String, tracked_wallets: HashMap<String, String>) -> Self {
        // Create client with connection pooling and keep-alive
        let client = Client::builder()
            .timeout(Duration::from_secs(30))
            .pool_max_idle_per_host(10)
            .pool_idle_timeout(Duration::from_secs(90))
            .tcp_keepalive(Duration::from_secs(60))
            .default_headers({
                let mut headers = reqwest::header::HeaderMap::new();
                headers.insert(
                    reqwest::header::AUTHORIZATION,
                    format!("Bearer {}", api_key).parse().unwrap(),
                );
                headers
            })
            .build()
            .unwrap();

        let wallet_count = tracked_wallets.len();
        Self {
            client,
            tracked_wallets,
            last_poll: Arc::new(Mutex::new(HashMap::with_capacity(wallet_count))),
        }
    }

    /// Poll all tracked wallets for recent orders
    pub async fn poll_all_wallets(&mut self) -> Result<Vec<MarketSignal>> {
        let (signals, _orders) = self.poll_all_wallets_with_orders().await?;
        Ok(signals)
    }

    /// Poll all tracked wallets and return both signals AND raw orders (for enrichment)
    pub async fn poll_all_wallets_with_orders(
        &mut self,
    ) -> Result<(Vec<MarketSignal>, Vec<(DomeOrder, String)>)> {
        let now = Utc::now().timestamp();
        let wallet_count = self.tracked_wallets.len();

        // Pre-allocate with estimated capacity
        let mut all_signals = Vec::with_capacity(wallet_count * 5);
        let mut all_orders = Vec::with_capacity(wallet_count * 5);

        debug!("🔄 Polling {} tracked wallets for orders...", wallet_count);

        for (wallet_address, wallet_label) in self.tracked_wallets.iter() {
            // Get last poll time using parking_lot (no await needed)
            let last_poll_time = {
                let last_polls = self.last_poll.lock();
                last_polls
                    .get(wallet_address)
                    .copied()
                    .unwrap_or(now - 3600)
            };

            // Poll orders for this wallet
            match self
                .poll_wallet_orders_with_raw(wallet_address, wallet_label, last_poll_time)
                .await
            {
                Ok((signals, orders)) => {
                    if !signals.is_empty() {
                        info!(
                            "✅ Found {} signals from {} [{}]",
                            signals.len(),
                            &wallet_address[..10],
                            wallet_label
                        );
                        all_signals.extend(signals);
                        all_orders.extend(orders);
                    }
                }
                Err(e) => {
                    warn!("Failed to poll wallet {}: {}", &wallet_address[..10], e);
                }
            }

            // Update last poll time
            {
                let mut last_polls = self.last_poll.lock();
                last_polls.insert(wallet_address.clone(), now);
            }

            // Rate limit: 20ms between requests (optimized for speed)
            // DomeAPI allows high-frequency polling with proper auth
            sleep(Duration::from_millis(20)).await;
        }

        if !all_signals.is_empty() {
            info!("🎯 Total signals found: {}", all_signals.len());
        }

        Ok((all_signals, all_orders))
    }

    /// Poll orders for a single wallet
    async fn poll_wallet_orders(
        &self,
        wallet_address: &str,
        wallet_label: &str,
        since_timestamp: i64,
    ) -> Result<Vec<MarketSignal>> {
        let url = format!(
            "{}/orders?user={}&start_time={}&limit=100",
            DOME_API_BASE, wallet_address, since_timestamp
        );

        debug!("Polling: {} since {}", wallet_address, since_timestamp);

        let response = self
            .client
            .get(&url)
            .send()
            .await
            .context("Failed to send request")?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(anyhow::anyhow!("API error {}: {}", status, text));
        }

        let orders_response: OrdersResponse = response
            .json()
            .await
            .context("Failed to parse orders response")?;

        // Convert orders to signals
        let signals: Vec<MarketSignal> = orders_response
            .orders
            .into_iter()
            .filter(|order| order.timestamp > since_timestamp) // Only new orders
            .map(|order| {
                let position_value = order.shares_normalized * order.price;

                MarketSignal {
                    id: format!("dome_order_{}", order.order_hash),
                    signal_type: SignalType::TrackedWalletEntry {
                        wallet_address: wallet_address.to_string(),
                        wallet_label: wallet_label.to_string(),
                        position_value_usd: position_value,
                        order_count: 1,
                        token_label: order.token_label.clone(), // "Up", "Down", "Yes", "No"
                    },
                    market_slug: order.market_slug.clone(),
                    confidence: calculate_confidence(wallet_label, position_value),
                    risk_level: calculate_risk_level(wallet_label, position_value),
                    details: SignalDetails {
                        market_id: order.condition_id,
                        market_title: order.title,
                        current_price: order.price,
                        volume_24h: order.shares_normalized,
                        liquidity: 0.0,
                        recommended_action: format!("{} (follow {})", order.side, wallet_label),
                        expiry_time: None,
                        observed_timestamp: Some(Utc::now().to_rfc3339()),
                        signal_family: Some("tracked_wallet".to_string()),
                        calibration_version: Some("v1.0".to_string()),
                        guardrail_flags: None,
                        recommended_size: Some(position_value * 0.1), // 10% of whale position
                    },
                    detected_at: Utc::now().to_rfc3339(),
                    source: "dome_rest".to_string(),
                }
            })
            .collect();

        Ok(signals)
    }

    /// Poll orders for a single wallet and return both signals AND raw orders
    async fn poll_wallet_orders_with_raw(
        &self,
        wallet_address: &str,
        wallet_label: &str,
        since_timestamp: i64,
    ) -> Result<(Vec<MarketSignal>, Vec<(DomeOrder, String)>)> {
        let url = format!(
            "{}/orders?user={}&start_time={}&limit=100",
            DOME_API_BASE, wallet_address, since_timestamp
        );

        debug!("Polling: {} since {}", wallet_address, since_timestamp);

        let response = self
            .client
            .get(&url)
            .send()
            .await
            .context("Failed to send request")?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(anyhow::anyhow!("API error {}: {}", status, text));
        }

        let orders_response: OrdersResponse = response
            .json()
            .await
            .context("Failed to parse orders response")?;

        // Filter and convert orders
        let filtered_orders: Vec<DomeOrder> = orders_response
            .orders
            .into_iter()
            .filter(|order| order.timestamp > since_timestamp)
            .collect();

        // Create signals from orders
        let signals: Vec<MarketSignal> = filtered_orders
            .iter()
            .map(|order| {
                let position_value = order.shares_normalized * order.price;

                MarketSignal {
                    id: format!("dome_order_{}", order.order_hash),
                    signal_type: SignalType::TrackedWalletEntry {
                        wallet_address: wallet_address.to_string(),
                        wallet_label: wallet_label.to_string(),
                        position_value_usd: position_value,
                        order_count: 1,
                        token_label: order.token_label.clone(),
                    },
                    market_slug: order.market_slug.clone(),
                    confidence: calculate_confidence(wallet_label, position_value),
                    risk_level: calculate_risk_level(wallet_label, position_value),
                    details: SignalDetails {
                        market_id: order.condition_id.clone(),
                        market_title: order.title.clone(),
                        current_price: order.price,
                        volume_24h: order.shares_normalized,
                        liquidity: 0.0,
                        recommended_action: format!("{} (follow {})", order.side, wallet_label),
                        expiry_time: None,
                        observed_timestamp: Some(Utc::now().to_rfc3339()),
                        signal_family: Some("tracked_wallet".to_string()),
                        calibration_version: Some("v1.0".to_string()),
                        guardrail_flags: None,
                        recommended_size: Some(position_value * 0.1),
                    },
                    detected_at: Utc::now().to_rfc3339(),
                    source: "dome_rest".to_string(),
                }
            })
            .collect();

        // Return orders with wallet label for enrichment
        let orders_with_label: Vec<(DomeOrder, String)> = filtered_orders
            .into_iter()
            .map(|order| (order, wallet_label.to_string()))
            .collect();

        Ok((signals, orders_with_label))
    }

    /// Poll activity (REDEEM, MERGE, SPLIT) for a wallet
    pub async fn poll_wallet_activity(
        &self,
        wallet_address: &str,
        since_timestamp: i64,
    ) -> Result<Vec<ActivityItem>> {
        let url = format!(
            "{}/activity?user={}&start_time={}&limit=100",
            DOME_API_BASE, wallet_address, since_timestamp
        );

        let response = self
            .client
            .get(&url)
            .send()
            .await
            .context("Failed to send request")?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(anyhow::anyhow!("API error {}: {}", status, text));
        }

        let activity_response: ActivityResponse = response
            .json()
            .await
            .context("Failed to parse activity response")?;

        Ok(activity_response.activities)
    }
}

/// Calculate confidence using quantitative factors
///
/// Confidence Formula:
/// Base (wallet tier) + Position Size Factor + Conviction Factor
///
/// Philosophy:
/// - Micro bets (<$1) = low conviction = low confidence (max 55%)
/// - Small bets ($1-$10) = testing waters = moderate confidence (max 65%)
/// - Medium bets ($10-$100) = decent conviction = good confidence (max 75%)
/// - Large bets ($100-$1000) = high conviction = high confidence (max 85%)
/// - Whale bets ($1000+) = maximum conviction = highest confidence (max 95%)
///
/// Wallet tier provides base multiplier:
/// - insider_sports: 1.0x (proven edge in sports)
/// - insider_politics: 0.95x (good but more volatile)
/// - world_class: 0.85x (good overall, but diverse)
/// - insider_other: 0.80x (mixed bag)
/// - unknown: 0.70x
fn calculate_confidence(wallet_label: &str, position_value: f64) -> f64 {
    // Step 1: Position size determines the BASE confidence ceiling
    // This is the most important factor - "skin in the game"
    let (size_base, size_ceiling) = match position_value {
        v if v < 0.10 => (0.20, 0.35),    // Dust: basically noise
        v if v < 1.0 => (0.30, 0.55),     // Micro: testing, low conviction
        v if v < 10.0 => (0.45, 0.65),    // Small: some conviction
        v if v < 100.0 => (0.55, 0.75),   // Medium: decent conviction
        v if v < 500.0 => (0.65, 0.82),   // Solid: good conviction
        v if v < 1000.0 => (0.70, 0.85),  // Large: high conviction
        v if v < 5000.0 => (0.75, 0.90),  // Very large: very high conviction
        v if v < 10000.0 => (0.80, 0.92), // Whale: whale conviction
        _ => (0.85, 0.95),                // Mega whale: maximum conviction
    };

    // Step 2: Wallet tier provides a multiplier within the range
    // All insiders get full multiplier (proven track records)
    let wallet_multiplier = match wallet_label {
        label if label.starts_with("insider_") => 1.0, // All insiders get full multiplier
        "world_class" => 0.95,                         // World class very close to insiders
        _ => 0.80,                                     // Unknown wallets get penalty
    };

    // Step 3: Calculate final confidence
    // confidence = base + (ceiling - base) * multiplier
    let range = size_ceiling - size_base;
    let confidence = size_base + (range * wallet_multiplier);

    // Step 4: Apply logarithmic scaling for position value bonus
    // Large positions get slight additional boost (diminishing returns)
    let log_bonus = if position_value > 100.0 {
        (position_value.log10() - 2.0) * 0.02 // +0.02 per order of magnitude above $100
    } else {
        0.0
    };

    // Final confidence with ceiling cap
    f64::min(confidence + log_bonus, size_ceiling)
}

/// Calculate risk level based on wallet type and position
/// Risk is inverse of confidence - micro bets are HIGH risk
fn calculate_risk_level(wallet_label: &str, position_value: f64) -> String {
    // Calculate confidence first to determine risk
    let confidence = calculate_confidence(wallet_label, position_value);

    match confidence {
        c if c >= 0.80 => "low".to_string(),
        c if c >= 0.65 => "medium".to_string(),
        c if c >= 0.50 => "high".to_string(),
        _ => "very_high".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_dome_realtime_poll() {
        if let Ok(api_key) = std::env::var("DOME_API_KEY") {
            let mut wallets = HashMap::new();
            wallets.insert(
                "0xcacf2bf1906bb3c74a0e0453bfb91f1374e335ff".to_string(),
                "world_class".to_string(),
            );

            let mut client = DomeRealtimeClient::new(api_key, wallets);
            let signals = client.poll_all_wallets().await;

            assert!(signals.is_ok());
            println!("Found {} signals", signals.unwrap().len());
        }
    }
}

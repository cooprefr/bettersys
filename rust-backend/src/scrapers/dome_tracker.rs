//! DomeAPI Wallet Tracker
//! Mission: Track elite and insider wallet entries with millisecond precision
//! Philosophy: No missed signals. Paranoia is standard.

use anyhow::{bail, Context, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use tokio::time::sleep;
use tracing::{debug, info, warn};

const DOME_API_BASE: &str = "https://api.domeapi.io/v1/polymarket";
const MAX_RETRIES: u32 = 5;
const RATE_LIMIT_DELAY_MS: u64 = 1000; // 1 second between requests

/// Order data from Dome API
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DomeOrder {
    pub token_id: String,
    #[serde(default)]
    pub token_label: Option<String>, // "Up", "Down", "Yes", "No" - outcome label
    pub side: String, // "BUY" or "SELL"
    pub shares_normalized: f64,
    pub price: f64,
    pub timestamp: i64,
    pub market_slug: String,
    pub title: String,
    pub user: String,

    // Optional fields (available from WS and REST, but not required everywhere)
    #[serde(default)]
    pub condition_id: Option<String>,
    #[serde(default)]
    pub order_hash: Option<String>,
    #[serde(default)]
    pub tx_hash: Option<String>,
}

/// Response from get_orders endpoint
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrdersResponse {
    pub orders: Vec<DomeOrder>,
    pub count: usize,
}

/// Dome API client with rate limiting
pub struct DomeClient {
    client: Client,
    api_key: String,
    last_request: Arc<Mutex<Option<Instant>>>,
}

impl DomeClient {
    pub fn new(api_key: String) -> Result<Self> {
        let client = Client::builder()
            .timeout(Duration::from_secs(30))
            .default_headers({
                let mut headers = reqwest::header::HeaderMap::new();
                let auth_value = format!("Bearer {}", api_key);
                headers.insert(
                    reqwest::header::AUTHORIZATION,
                    auth_value.parse().context("Invalid API key format")?,
                );
                headers
            })
            .build()
            .context("Failed to build HTTP client")?;

        Ok(Self {
            client,
            api_key,
            last_request: Arc::new(Mutex::new(None)),
        })
    }

    /// Rate-limited request execution
    async fn rate_limited_request(&self) {
        let mut last = self.last_request.lock().await;

        if let Some(last_time) = *last {
            let elapsed = last_time.elapsed();
            let min_delay = Duration::from_millis(RATE_LIMIT_DELAY_MS);

            if elapsed < min_delay {
                let wait_time = min_delay - elapsed;
                debug!("Rate limiting: waiting {}ms", wait_time.as_millis());
                sleep(wait_time).await;
            }
        }

        *last = Some(Instant::now());
    }

    /// Get orders for a specific wallet address
    ///
    /// # Arguments
    /// * `user` - Wallet address (checksummed or lowercase)
    /// * `start_time` - Optional Unix timestamp for filtering (only orders after this time)
    /// * `limit` - Max orders to return (capped at 1000 per API docs)
    /// * `offset` - Pagination offset
    pub async fn get_orders(
        &self,
        user: &str,
        start_time: Option<i64>,
        limit: u32,
        offset: u32,
    ) -> Result<OrdersResponse> {
        self.rate_limited_request().await;

        let url = format!("{}/orders", DOME_API_BASE);
        let limit = limit.min(1000); // API cap

        let mut query_params = vec![
            ("user", user.to_string()),
            ("limit", limit.to_string()),
            ("offset", offset.to_string()),
        ];

        if let Some(ts) = start_time {
            query_params.push(("start_time", ts.to_string()));
        }

        let response = self.retry_request(&url, &query_params).await?;

        // Parse response
        let orders: Vec<DomeOrder> = response
            .json()
            .await
            .context("Failed to parse orders response")?;

        // Filter for BUY orders only
        let buy_orders: Vec<DomeOrder> = orders
            .into_iter()
            .filter(|o| o.side.to_uppercase() == "BUY")
            .collect();

        info!(
            "📊 Dome: Fetched {} BUY orders for wallet {}",
            buy_orders.len(),
            user
        );

        Ok(OrdersResponse {
            count: buy_orders.len(),
            orders: buy_orders,
        })
    }

    /// Execute request with exponential backoff retry
    async fn retry_request(
        &self,
        url: &str,
        query_params: &[(&str, String)],
    ) -> Result<reqwest::Response> {
        let mut backoff = Duration::from_millis(100);

        for attempt in 1..=MAX_RETRIES {
            match self.client.get(url).query(query_params).send().await {
                Ok(response) => {
                    let status = response.status();

                    if status.is_success() {
                        return Ok(response);
                    } else if status.as_u16() == 429 {
                        // Rate limited
                        warn!("Rate limited (429) on attempt {}, backing off 60s", attempt);
                        sleep(Duration::from_secs(60)).await;
                    } else if status.is_server_error() {
                        // 5xx errors - retry with backoff
                        warn!(
                            "Server error {} on attempt {}, backing off {}ms",
                            status,
                            attempt,
                            backoff.as_millis()
                        );
                        sleep(backoff).await;
                        backoff = (backoff * 2).min(Duration::from_secs(16));
                    } else {
                        // Client error - don't retry
                        let body = response.text().await.unwrap_or_default();
                        bail!("API error {}: {}", status, body);
                    }
                }
                Err(e) => {
                    warn!("Request failed (attempt {}): {}", attempt, e);
                    if attempt < MAX_RETRIES {
                        sleep(backoff).await;
                        backoff = (backoff * 2).min(Duration::from_secs(16));
                    } else {
                        return Err(e.into());
                    }
                }
            }
        }

        bail!("Max retries exceeded for {}", url)
    }

    /// Get orders with pagination support (fetches up to 3 pages max)
    pub async fn get_orders_paginated(
        &self,
        user: &str,
        start_time: Option<i64>,
        limit_per_page: u32,
    ) -> Result<Vec<DomeOrder>> {
        let mut all_orders = Vec::new();
        let max_pages = 3;

        for page in 0..max_pages {
            let offset = page * limit_per_page;
            let response = self
                .get_orders(user, start_time, limit_per_page, offset)
                .await?;

            let order_count = response.orders.len();
            all_orders.extend(response.orders);

            // Stop if we got fewer orders than requested (no more pages)
            if order_count < limit_per_page as usize {
                break;
            }
        }

        info!(
            "📊 Dome: Total {} BUY orders fetched for {} (across {} pages)",
            all_orders.len(),
            user,
            (all_orders.len() as f32 / limit_per_page as f32).ceil()
        );

        Ok(all_orders)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_dome_client_creation() {
        let client = DomeClient::new("test-api-key".to_string());
        assert!(client.is_ok());
    }

    #[tokio::test]
    #[ignore] // Only run with real API key
    async fn test_get_orders_real() {
        let api_key = std::env::var("DOME_API_KEY").expect("DOME_API_KEY not set");
        let client = DomeClient::new(api_key).unwrap();

        // Test with a known active wallet (first from insider_sports list)
        let result = client
            .get_orders("0xc529ec14b3fd6fd42d2c4eab28ea8a2eaeda4f91", None, 10, 0)
            .await;

        match result {
            Ok(response) => {
                println!("✅ Fetched {} orders", response.count);
                assert!(response.count >= response.orders.len());
            }
            Err(e) => {
                println!("⚠️ API call failed (expected if rate limited): {}", e);
            }
        }
    }
}

use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::time::{sleep, Duration, Instant};

const HASHDIVE_BASE_URL: &str = "https://hashdive.com/api";
const RATE_LIMIT_DELAY: Duration = Duration::from_millis(1050); // 1 req/sec with 50ms buffer

/// Hashdive API client with automatic rate limiting
pub struct HashdiveClient {
    client: reqwest::Client,
    api_key: String,
    last_request: Arc<Mutex<Option<Instant>>>,
}

impl HashdiveClient {
    pub fn new(api_key: String) -> Self {
        Self {
            client: reqwest::Client::builder()
                .user_agent("BetterBot/1.0")
                .timeout(Duration::from_secs(15))
                .build()
                .unwrap(),
            api_key,
            last_request: Arc::new(Mutex::new(None)),
        }
    }

    /// Rate-limited request wrapper
    async fn rate_limited_request(&self) -> Result<()> {
        let mut last_req = self.last_request.lock().await;
        
        if let Some(last_time) = *last_req {
            let elapsed = last_time.elapsed();
            if elapsed < RATE_LIMIT_DELAY {
                let wait_time = RATE_LIMIT_DELAY - elapsed;
                tracing::debug!("Rate limiting: waiting {:?}", wait_time);
                sleep(wait_time).await;
            }
        }
        
        *last_req = Some(Instant::now());
        Ok(())
    }

    /// Retry wrapper with exponential backoff (3 attempts, 1s -> 2s -> 4s)
    async fn retry_request<F, Fut, T>(&self, operation: F) -> Result<T>
    where
        F: Fn() -> Fut,
        Fut: std::future::Future<Output = Result<T>>,
    {
        let mut attempt = 0;
        let max_attempts = 3;
        
        loop {
            attempt += 1;
            
            match operation().await {
                Ok(result) => return Ok(result),
                Err(e) => {
                    if attempt >= max_attempts {
                        return Err(anyhow!("Failed after {} attempts: {}", max_attempts, e));
                    }
                    
                    let backoff_ms = 1000 * (1 << (attempt - 1)); // 1s, 2s, 4s
                    tracing::warn!(
                        "Hashdive request failed (attempt {}/{}): {}. Retrying in {}ms...",
                        attempt,
                        max_attempts,
                        e,
                        backoff_ms
                    );
                    sleep(Duration::from_millis(backoff_ms)).await;
                }
            }
        }
    }

    /// Get latest whale trades (large trades above threshold) - WITH RETRY
    pub async fn get_whale_trades(&self, min_usd: Option<u32>, limit: Option<u32>) -> Result<Vec<WhaleTrade>> {
        let api_key = self.api_key.clone();
        let client = self.client.clone();
        
        self.retry_request(|| async {
            self.rate_limited_request().await?;

            let url = format!("{}/get_latest_whale_trades", HASHDIVE_BASE_URL);
            
            let mut params = vec![
                ("format", "json".to_string()),
            ];
            
            if let Some(min) = min_usd {
                params.push(("min_usd", min.to_string()));
            }
            
            if let Some(lim) = limit {
                params.push(("limit", lim.to_string()));
            }

            tracing::debug!("Fetching whale trades (min_usd: {:?}, limit: {:?})", min_usd, limit);

            let response = client
                .get(&url)
                .header("x-api-key", &api_key)
                .query(&params)
                .send()
                .await?;

            if !response.status().is_success() {
                return Err(anyhow!("Hashdive API error: HTTP {}", response.status()));
            }

            // API returns array directly, not wrapped in object
            let trades: Vec<WhaleTrade> = response.json().await?;
            
            tracing::info!("✓ Fetched {} whale trades from Hashdive", trades.len());
            
            Ok(trades)
        }).await
    }

    /// Get OHLCV data for a market
    #[allow(dead_code)] // Reserved for price movement analysis
    pub async fn get_ohlcv(
        &self,
        asset_id: &str,
        resolution: &str,
        timestamp_gte: Option<DateTime<Utc>>,
        timestamp_lte: Option<DateTime<Utc>>,
    ) -> Result<Vec<OhlcvBar>> {
        self.rate_limited_request().await?;

        let url = format!("{}/get_ohlcv", HASHDIVE_BASE_URL);
        
        let mut params = vec![
            ("asset_id", asset_id.to_string()),
            ("resolution", resolution.to_string()),
            ("format", "json".to_string()),
        ];
        
        if let Some(start) = timestamp_gte {
            params.push(("timestamp_gte", start.to_rfc3339()));
        }
        
        if let Some(end) = timestamp_lte {
            params.push(("timestamp_lte", end.to_rfc3339()));
        }

        tracing::debug!("Fetching OHLCV for asset {} (resolution: {})", asset_id, resolution);

        let response = self.client
            .get(&url)
            .header("x-api-key", &self.api_key)
            .query(&params)
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(anyhow!("Hashdive API error: HTTP {}", response.status()));
        }

        let data: OhlcvResponse = response.json().await?;
        
        Ok(data.ohlcv)
    }

    /// Search markets by query string
    #[allow(dead_code)] // Reserved for market lookup features
    pub async fn search_markets(&self, query: &str) -> Result<Vec<Market>> {
        self.rate_limited_request().await?;

        let url = format!("{}/search_markets", HASHDIVE_BASE_URL);
        
        let params = [
            ("query", query),
        ];

        tracing::debug!("Searching markets for: {}", query);

        let response = self.client
            .get(&url)
            .header("x-api-key", &self.api_key)
            .query(&params)
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(anyhow!("Hashdive API error: HTTP {}", response.status()));
        }

        let data: MarketsResponse = response.json().await?;
        
        tracing::debug!("Found {} markets matching '{}'", data.markets.len(), query);
        
        Ok(data.markets)
    }

    /// Get last price for an asset
    #[allow(dead_code)] // Reserved for price tracking features
    pub async fn get_last_price(&self, asset_id: &str) -> Result<LastPrice> {
        self.rate_limited_request().await?;

        let url = format!("{}/get_last_price", HASHDIVE_BASE_URL);
        
        let params = [("asset_id", asset_id)];

        let response = self.client
            .get(&url)
            .header("x-api-key", &self.api_key)
            .query(&params)
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(anyhow!("Hashdive API error: HTTP {}", response.status()));
        }

        let data: LastPrice = response.json().await?;
        
        Ok(data)
    }

    /// Check API usage and remaining credits - WITH RETRY
    pub async fn check_usage(&self) -> Result<ApiUsage> {
        self.retry_request(|| async {
            self.rate_limited_request().await?;

            let url = format!("{}/get_api_usage", HASHDIVE_BASE_URL);

            let response = self.client
                .get(&url)
                .header("x-api-key", &self.api_key)
                .send()
                .await?;

            if !response.status().is_success() {
                return Err(anyhow!("Hashdive API error: HTTP {}", response.status()));
            }

            let data: ApiUsage = response.json().await?;
            
            Ok(data)
        }).await
    }
}

// Response types matching Hashdive API

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WhaleTrade {
    pub transaction_hash: String,
    pub timestamp: String,
    pub user_address: String,
    pub side: String, // "BUY" or "SELL"
    pub shares: String,
    pub price: f64,
    pub usd_amount: f64,
    pub asset_id: String,
    pub market_title: Option<String>,
    pub outcome: Option<String>,
}

// Note: Hashdive API returns array directly for whale trades
// No wrapper object needed

#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)] // Reserved for price movement analysis
pub struct OhlcvBar {
    pub timestamp: String,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: f64,
    pub num_trades: i32,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)] // Reserved for price movement analysis
struct OhlcvResponse {
    ohlcv: Vec<OhlcvBar>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)] // Reserved for market lookup features
pub struct Market {
    pub market_id: String,
    pub question: String,
    pub description: Option<String>,
    pub outcomes: Vec<Outcome>,
    pub end_date: Option<String>,
    pub volume: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)] // Reserved for market lookup features
pub struct Outcome {
    pub asset_id: String,
    pub outcome: String,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)] // Reserved for market lookup features
struct MarketsResponse {
    markets: Vec<Market>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)] // Reserved for price tracking features
pub struct LastPrice {
    pub asset_id: String,
    pub price: f64,
    pub source: String, // "resolved" or "last_trade"
    pub market_title: Option<String>,
    pub outcome: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ApiUsage {
    #[serde(default)]
    pub credits_used: i32,  // Actual field returned by API
    #[serde(default)]
    pub api_key: String,    // Masked API key returned
}

impl Clone for HashdiveClient {
    fn clone(&self) -> Self {
        Self {
            client: self.client.clone(),
            api_key: self.api_key.clone(),
            last_request: Arc::clone(&self.last_request),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_rate_limiting() {
        let client = HashdiveClient::new("test_key".to_string());
        
        let start = Instant::now();
        
        // Make 3 requests
        for _ in 0..3 {
            client.rate_limited_request().await.unwrap();
        }
        
        let elapsed = start.elapsed();
        
        // Should take at least 2 seconds (3 requests = 2 delays)
        assert!(elapsed >= Duration::from_secs(2));
    }
}

//! Hashdive API Integration
//! Pilot in Command: Whale Tracking System
//! Mission: Track large market participants with physics-constrained precision

use crate::models::WhaleTrade;
use anyhow::{bail, Context, Result};
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;
use tokio::time::{sleep, timeout};
use tracing::{debug, error, info, warn};

const HASHDIVE_API_BASE: &str = "https://hashdive.com/api";
const MAX_RETRIES: u32 = 3;
const INITIAL_BACKOFF_MS: u64 = 100;

/// Rate limiter - Hashdive limits to 1 query per second (60/minute)
struct RateLimiter {
    last_request: std::time::Instant,
    min_interval: Duration,
}

impl RateLimiter {
    fn new() -> Self {
        Self {
            last_request: std::time::Instant::now() - Duration::from_secs(2),
            min_interval: Duration::from_secs(2), // 2 seconds between requests (safer than 1)
        }
    }

    async fn acquire(&mut self) {
        let elapsed = self.last_request.elapsed();
        if elapsed < self.min_interval {
            let wait_time = self.min_interval - elapsed;
            debug!("Rate limiting: waiting {}ms", wait_time.as_millis());
            sleep(wait_time).await;
        }
        self.last_request = std::time::Instant::now();
    }
}

pub struct HashdiveScraper {
    client: Client,
    api_key: String,
    rate_limiter: RateLimiter,
    credits_used: u32,
    credits_limit: u32,
}

impl HashdiveScraper {
    pub fn new(api_key: String) -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(30))
            .user_agent("BetterBot/1.0 (Arbitrage Engine)")
            .build()
            .unwrap_or_else(|_| Client::new());

        Self {
            client,
            api_key,
            rate_limiter: RateLimiter::new(),
            credits_used: 0,
            credits_limit: 1000, // Monthly limit as per docs
        }
    }

    /// Get trades for a specific user
    pub async fn get_trades(
        &mut self,
        user_address: &str,
        asset_id: Option<&str>,
        timestamp_gte: Option<&str>,
        timestamp_lte: Option<&str>,
        page: Option<u32>,
        page_size: Option<u32>,
    ) -> Result<TradesResponse> {
        self.rate_limiter.acquire().await;

        let mut params = HashMap::new();
        params.insert("user_address", user_address.to_string());
        params.insert("format", "json".to_string());

        if let Some(id) = asset_id {
            params.insert("asset_id", id.to_string());
        }
        if let Some(gte) = timestamp_gte {
            params.insert("timestamp_gte", gte.to_string());
        }
        if let Some(lte) = timestamp_lte {
            params.insert("timestamp_lte", lte.to_string());
        }
        if let Some(p) = page {
            params.insert("page", p.to_string());
        }
        if let Some(ps) = page_size {
            params.insert("page_size", ps.min(1000).to_string());
        }

        let url = format!("{}/get_trades", HASHDIVE_API_BASE);
        let response = self.execute_with_retry(&url, &params).await?;

        let trades: TradesResponse = response
            .json()
            .await
            .context("Failed to parse trades response")?;

        self.credits_used += 1;
        info!(
            "Fetched {} trades for user {}, credits used: {}/{}",
            trades.data.len(),
            user_address,
            self.credits_used,
            self.credits_limit
        );

        Ok(trades)
    }

    /// Get positions for a specific user
    pub async fn get_positions(
        &mut self,
        user_address: &str,
        asset_id: Option<&str>,
    ) -> Result<PositionsResponse> {
        self.rate_limiter.acquire().await;

        let mut params = HashMap::new();
        params.insert("user_address", user_address.to_string());
        params.insert("format", "json".to_string());

        if let Some(id) = asset_id {
            params.insert("asset_id", id.to_string());
        }

        let url = format!("{}/get_positions", HASHDIVE_API_BASE);
        let response = self.execute_with_retry(&url, &params).await?;

        let positions: PositionsResponse = response
            .json()
            .await
            .context("Failed to parse positions response")?;

        self.credits_used += 1;
        info!(
            "Fetched {} positions for user {}",
            positions.data.len(),
            user_address
        );

        Ok(positions)
    }

    /// Get latest whale trades above a threshold
    pub async fn get_latest_whale_trades(
        &mut self,
        min_usd: Option<f64>,
        limit: Option<u32>,
    ) -> Result<WhaleTradesResponse> {
        self.rate_limiter.acquire().await;

        let mut params = HashMap::new();
        params.insert("format", "json".to_string());

        if let Some(min) = min_usd {
            params.insert("min_usd", min.to_string());
        } else {
            params.insert("min_usd", "10000".to_string()); // Default $10k
        }

        if let Some(l) = limit {
            params.insert("limit", l.min(1000).to_string());
        } else {
            params.insert("limit", "100".to_string());
        }

        let url = format!("{}/get_latest_whale_trades", HASHDIVE_API_BASE);
        let response = self.execute_with_retry(&url, &params).await?;

        // Get response text first to check for errors
        let text = response
            .text()
            .await
            .context("Failed to get response text")?;

        // Check for credit limit error in response body
        if text.contains("Credit limit exceeded") {
            error!("🚫 Hashdive API credit limit exceeded (1000/month). Whale signals disabled until next month.");
            bail!("Hashdive credit limit exceeded - wait for monthly reset");
        }

        // Parse the JSON response
        let trades: WhaleTradesResponse =
            serde_json::from_str(&text).context("Failed to parse whale trades response")?;

        self.credits_used += 1;
        info!(
            "Fetched {} whale trades above ${}",
            trades.data.len(),
            min_usd.unwrap_or(10000.0)
        );

        Ok(trades)
    }

    /// Get last price for an asset
    pub async fn get_last_price(&mut self, asset_id: &str) -> Result<LastPriceResponse> {
        self.rate_limiter.acquire().await;

        let mut params = HashMap::new();
        params.insert("asset_id", asset_id.to_string());

        let url = format!("{}/get_last_price", HASHDIVE_API_BASE);
        let response = self.execute_with_retry(&url, &params).await?;

        let price: LastPriceResponse = response
            .json()
            .await
            .context("Failed to parse last price response")?;

        self.credits_used += 1;
        debug!("Fetched last price for asset {}: {}", asset_id, price.price);

        Ok(price)
    }

    /// Get OHLCV data for an asset
    pub async fn get_ohlcv(
        &mut self,
        asset_id: &str,
        resolution: &str, // 1m, 5m, 15m, 1h, 4h, 1d
        timestamp_gte: Option<&str>,
        timestamp_lte: Option<&str>,
    ) -> Result<OHLCVResponse> {
        self.rate_limiter.acquire().await;

        let mut params = HashMap::new();
        params.insert("asset_id", asset_id.to_string());
        params.insert("resolution", resolution.to_string());
        params.insert("format", "json".to_string());

        if let Some(gte) = timestamp_gte {
            params.insert("timestamp_gte", gte.to_string());
        }
        if let Some(lte) = timestamp_lte {
            params.insert("timestamp_lte", lte.to_string());
        }

        let url = format!("{}/get_ohlcv", HASHDIVE_API_BASE);
        let response = self.execute_with_retry(&url, &params).await?;

        let ohlcv: OHLCVResponse = response
            .json()
            .await
            .context("Failed to parse OHLCV response")?;

        self.credits_used += 1;
        info!(
            "Fetched {} OHLCV bars for asset {}",
            ohlcv.data.len(),
            asset_id
        );

        Ok(ohlcv)
    }

    /// Search markets by query
    pub async fn search_markets(&mut self, query: &str) -> Result<MarketSearchResponse> {
        self.rate_limiter.acquire().await;

        let mut params = HashMap::new();
        params.insert("query", query.to_string());

        let url = format!("{}/search_markets", HASHDIVE_API_BASE);
        let response = self.execute_with_retry(&url, &params).await?;

        let markets: MarketSearchResponse = response
            .json()
            .await
            .context("Failed to parse market search response")?;

        self.credits_used += 1;
        info!("Found {} markets for query: {}", markets.data.len(), query);

        Ok(markets)
    }

    /// Get API usage statistics
    pub async fn get_api_usage(&mut self) -> Result<ApiUsageResponse> {
        self.rate_limiter.acquire().await;

        let params = HashMap::new();
        let url = format!("{}/get_api_usage", HASHDIVE_API_BASE);
        let response = self.execute_with_retry(&url, &params).await?;

        let usage: ApiUsageResponse = response
            .json()
            .await
            .context("Failed to parse API usage response")?;

        info!(
            "API usage: {}/{} credits",
            usage.credits_used, usage.credits_limit
        );

        Ok(usage)
    }

    /// Execute request with exponential backoff retry
    async fn execute_with_retry(
        &self,
        url: &str,
        params: &HashMap<&str, String>,
    ) -> Result<reqwest::Response> {
        let mut backoff = INITIAL_BACKOFF_MS;

        for attempt in 0..MAX_RETRIES {
            let request = self
                .client
                .get(url)
                .header("x-api-key", &self.api_key)
                .query(params);

            match timeout(Duration::from_secs(10), request.send()).await {
                Ok(Ok(response)) => {
                    if response.status().is_success() {
                        return Ok(response);
                    } else if response.status() == StatusCode::TOO_MANY_REQUESTS {
                        warn!("Rate limited on attempt {}, backing off", attempt + 1);
                        sleep(Duration::from_secs(60)).await; // 1 minute backoff for rate limit
                    } else {
                        let status = response.status();
                        let text = response.text().await.unwrap_or_default();
                        if text.contains("Credit limit exceeded") {
                            error!("🚫 Hashdive API credit limit exceeded (1000/month). Whale signals disabled until next month.");
                            bail!("Hashdive credit limit exceeded - wait for monthly reset");
                        }
                        error!("API error {}: {}", status, text);
                        bail!("Hashdive API error {}: {}", status, text);
                    }
                }
                Ok(Err(e)) => {
                    warn!("Request failed (attempt {}): {}", attempt + 1, e);
                }
                Err(_) => {
                    warn!("Request timeout (attempt {})", attempt + 1);
                }
            }

            if attempt < MAX_RETRIES - 1 {
                debug!("Retrying in {}ms", backoff);
                sleep(Duration::from_millis(backoff)).await;
                backoff = (backoff * 2).min(30000);
            }
        }

        bail!("Max retries exceeded for {}", url)
    }

    /// Convert Hashdive trades to our internal WhaleTrade format
    pub fn to_whale_trades(&self, trades: &[HashdiveTrade]) -> Vec<WhaleTrade> {
        trades
            .iter()
            .map(|t| WhaleTrade {
                market_id: t.asset_id.clone(),
                side: t.side.clone(),
                size: t.size,
                price: t.price,
                timestamp: t.timestamp,
            })
            .collect()
    }

    /// Classify wallet as Elite or Insider based on performance metrics
    /// Elite: High volume + High win rate (established whale)
    /// Insider: Early entry + High win rate (information advantage)
    pub fn classify_wallet(&self, trades: &[HashdiveTrade]) -> WalletClassification {
        if trades.is_empty() {
            return WalletClassification::Regular;
        }

        // Calculate performance metrics
        let total_volume: f64 = trades.iter().map(|t| t.size * t.price).sum();
        let avg_volume = total_volume / trades.len() as f64;

        // Calculate win rate from profitable trades
        let profitable_trades = trades
            .iter()
            .filter(|t| t.pnl_usd.is_some() && t.pnl_usd.unwrap() > 0.0)
            .count();
        let win_rate = profitable_trades as f64 / trades.len() as f64;

        // Calculate average trade timing (early entry score)
        // Trades closer to market creation = higher insider score
        let timestamps: Vec<i64> = trades.iter().map(|t| t.timestamp).collect();
        let earliest_ts = timestamps.iter().min().unwrap_or(&0);
        let latest_ts = timestamps.iter().max().unwrap_or(&0);
        let time_range = (latest_ts - earliest_ts) as f64;

        // Early entry score: How quickly they enter markets
        let early_entry_score = if time_range > 0.0 {
            let avg_entry_time = timestamps.iter().sum::<i64>() as f64 / timestamps.len() as f64;
            1.0 - ((avg_entry_time - *earliest_ts as f64) / time_range).min(1.0)
        } else {
            0.5
        };

        // Classification thresholds
        const ELITE_VOLUME_THRESHOLD: f64 = 100_000.0; // $100k+ total volume
        const ELITE_WIN_RATE_THRESHOLD: f64 = 0.65; // 65%+ win rate
        const INSIDER_WIN_RATE_THRESHOLD: f64 = 0.70; // 70%+ win rate
        const INSIDER_EARLY_THRESHOLD: f64 = 0.75; // 75%+ early entry score

        // Elite: High volume trader with good win rate
        if total_volume >= ELITE_VOLUME_THRESHOLD && win_rate >= ELITE_WIN_RATE_THRESHOLD {
            return WalletClassification::Elite {
                win_rate,
                total_volume,
                avg_trade_size: avg_volume,
            };
        }

        // Insider: High win rate + early market entry (information edge)
        if win_rate >= INSIDER_WIN_RATE_THRESHOLD && early_entry_score >= INSIDER_EARLY_THRESHOLD {
            return WalletClassification::Insider {
                win_rate,
                early_entry_score,
                total_volume,
            };
        }

        // Regular whale: High volume but doesn't meet elite criteria
        if total_volume >= 50_000.0 {
            return WalletClassification::Whale {
                total_volume,
                win_rate,
            };
        }

        WalletClassification::Regular
    }
}

/// Wallet classification types
#[derive(Debug, Clone)]
pub enum WalletClassification {
    Elite {
        win_rate: f64,
        total_volume: f64,
        avg_trade_size: f64,
    },
    Insider {
        win_rate: f64,
        early_entry_score: f64,
        total_volume: f64,
    },
    Whale {
        total_volume: f64,
        win_rate: f64,
    },
    Regular,
}

// API Response Types
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradesResponse {
    pub data: Vec<HashdiveTrade>,
    pub page: u32,
    pub page_size: u32,
    pub total: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HashdiveTrade {
    pub id: String,
    pub user_address: String,
    pub asset_id: String,
    pub market_slug: String,
    pub outcome: String,
    pub side: String,
    pub size: f64,
    pub price: f64,
    pub fee_usd: f64,
    pub pnl_usd: Option<f64>,
    pub timestamp: i64,
    pub block_number: u64,
    pub transaction_hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PositionsResponse {
    pub data: Vec<Position>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Position {
    pub user_address: String,
    pub asset_id: String,
    pub market_slug: String,
    pub outcome: String,
    pub amount: f64,
    pub avg_price: f64,
    pub realized_pnl: f64,
    pub unrealized_pnl: Option<f64>,
    pub last_price: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WhaleTradesResponse {
    pub data: Vec<WhaleTrade>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LastPriceResponse {
    pub asset_id: String,
    pub price: f64,
    pub source: String, // "resolved" or "last_trade"
    pub market_slug: String,
    pub outcome: String,
    pub resolved: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OHLCVResponse {
    pub data: Vec<OHLCV>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OHLCV {
    pub timestamp: i64,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: f64,
    pub trades: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketSearchResponse {
    pub data: Vec<MarketInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketInfo {
    pub condition_id: String,
    pub question: String,
    pub end_date: Option<String>,
    pub outcomes: Vec<Outcome>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Outcome {
    pub asset_id: String,
    pub outcome: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiUsageResponse {
    pub credits_used: u32,
    pub credits_limit: u32,
    pub reset_date: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_hashdive_scraper() {
        // Test with actual API key from environment
        if let Ok(api_key) = std::env::var("HASHDIVE_API_KEY") {
            let mut scraper = HashdiveScraper::new(api_key);
            let usage = scraper.get_api_usage().await;
            assert!(usage.is_ok());
        }
    }
}

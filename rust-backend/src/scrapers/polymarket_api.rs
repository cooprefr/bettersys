//! Polymarket CLOB & Data API Integration
//! Pilot in Command: Market Data Acquisition
//! Mission: Extract market inefficiencies with physics-constrained speed

use crate::models::{Market, PolymarketEvent};
use anyhow::{bail, Context, Result};
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;
use tokio::time::{sleep, timeout};
use tracing::{debug, error, info, warn};

const CLOB_API_BASE: &str = "https://clob.polymarket.com";
const DATA_API_BASE: &str = "https://data-api.polymarket.com";
const GAMMA_API_BASE: &str = "https://gamma-api.polymarket.com";

const MAX_RETRIES: u32 = 3;
const INITIAL_BACKOFF_MS: u64 = 100;

/// Rate limiter to respect API limits
struct RateLimiter {
    requests_per_10s: u32,
    current_requests: u32,
    window_start: std::time::Instant,
}

impl RateLimiter {
    fn new(requests_per_10s: u32) -> Self {
        Self {
            requests_per_10s,
            current_requests: 0,
            window_start: std::time::Instant::now(),
        }
    }

    async fn acquire(&mut self) {
        let elapsed = self.window_start.elapsed();

        // Reset window if 10 seconds have passed
        if elapsed >= Duration::from_secs(10) {
            self.current_requests = 0;
            self.window_start = std::time::Instant::now();
        }

        // If we've hit the limit, wait for the window to reset
        if self.current_requests >= self.requests_per_10s {
            let wait_time = Duration::from_secs(10) - elapsed;
            if wait_time > Duration::ZERO {
                debug!("Rate limiting: waiting {}ms", wait_time.as_millis());
                sleep(wait_time).await;
                self.current_requests = 0;
                self.window_start = std::time::Instant::now();
            }
        }

        self.current_requests += 1;
    }
}

pub struct PolymarketScraper {
    client: Client,
    clob_limiter: RateLimiter,
    data_limiter: RateLimiter,
    gamma_limiter: RateLimiter,
}

impl PolymarketScraper {
    pub fn new() -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(30))
            .user_agent("BetterBot/1.0 (Arbitrage Engine)")
            .build()
            .expect("Failed to create HTTP client");

        Self {
            client,
            clob_limiter: RateLimiter::new(500), // 5000/10s general, be conservative
            data_limiter: RateLimiter::new(20),  // 200/10s
            gamma_limiter: RateLimiter::new(75), // 750/10s
        }
    }

    /// Fetch active markets from CLOB API
    pub async fn fetch_markets(&mut self) -> Result<Vec<CLOBMarket>> {
        self.clob_limiter.acquire().await;

        let url = format!("{}/markets", CLOB_API_BASE);
        let response = self.execute_with_retry(&url, None).await?;

        let markets: Vec<CLOBMarket> = response
            .json()
            .await
            .context("Failed to parse markets response")?;

        info!("Fetched {} markets from CLOB", markets.len());
        Ok(markets)
    }

    /// Fetch detailed market info from GAMMA API
    pub async fn fetch_gamma_markets(
        &mut self,
        limit: usize,
        offset: usize,
    ) -> Result<GammaMarketsResponse> {
        self.gamma_limiter.acquire().await;

        let url = format!("{}/markets", GAMMA_API_BASE);
        let mut params = HashMap::new();
        params.insert("limit", limit.to_string());
        params.insert("offset", offset.to_string());
        // Note: GAMMA API doesn't support 'active' parameter - removed to fix 422 error

        let response = self.execute_with_retry(&url, Some(&params)).await?;

        // The GAMMA API returns an array of markets directly
        let markets_data: Vec<GammaMarket> = response
            .json()
            .await
            .context("Failed to parse GAMMA markets")?;

        let markets = GammaMarketsResponse {
            data: markets_data,
            next_cursor: None,
        };

        info!("Fetched {} GAMMA markets", markets.data.len());
        Ok(markets)
    }

    /// Fetch order book for a specific market
    pub async fn fetch_orderbook(&mut self, token_id: &str) -> Result<OrderBook> {
        self.clob_limiter.acquire().await;

        let url = format!("{}/book", CLOB_API_BASE);
        let mut params = HashMap::new();
        params.insert("token_id", token_id.to_string());

        let response = self.execute_with_retry(&url, Some(&params)).await?;

        let orderbook: OrderBook = response.json().await.context("Failed to parse orderbook")?;

        debug!(
            "Fetched orderbook for token {}: {} bids, {} asks",
            token_id,
            orderbook.bids.len(),
            orderbook.asks.len()
        );
        Ok(orderbook)
    }

    /// Fetch multiple order books in one call
    pub async fn fetch_orderbooks(&mut self, token_ids: Vec<String>) -> Result<Vec<OrderBook>> {
        self.clob_limiter.acquire().await;

        let url = format!("{}/books", CLOB_API_BASE);
        let token_ids_str = token_ids.join(",");
        let mut params = HashMap::new();
        params.insert("token_ids", token_ids_str);

        let response = self.execute_with_retry(&url, Some(&params)).await?;

        let orderbooks: Vec<OrderBook> = response
            .json()
            .await
            .context("Failed to parse orderbooks")?;

        info!("Fetched {} orderbooks", orderbooks.len());
        Ok(orderbooks)
    }

    /// Fetch current price for a market
    pub async fn fetch_price(&mut self, token_id: &str) -> Result<PriceInfo> {
        self.clob_limiter.acquire().await;

        let url = format!("{}/price", CLOB_API_BASE);
        let mut params = HashMap::new();
        params.insert("token_id", token_id.to_string());

        let response = self.execute_with_retry(&url, Some(&params)).await?;

        let price: PriceInfo = response.json().await.context("Failed to parse price")?;

        Ok(price)
    }

    /// Fetch recent trades
    pub async fn fetch_trades(
        &mut self,
        market: Option<String>,
        limit: Option<usize>,
    ) -> Result<Vec<Trade>> {
        self.data_limiter.acquire().await;

        let url = format!("{}/trades", DATA_API_BASE);
        let mut params = HashMap::new();

        if let Some(m) = market {
            params.insert("market", m);
        }
        if let Some(l) = limit {
            params.insert("limit", l.to_string());
        }

        let response = self.execute_with_retry(&url, Some(&params)).await?;

        let trades: Vec<Trade> = response.json().await.context("Failed to parse trades")?;

        info!("Fetched {} trades", trades.len());
        Ok(trades)
    }

    /// Execute request with exponential backoff retry
    async fn execute_with_retry(
        &self,
        url: &str,
        params: Option<&HashMap<&str, String>>,
    ) -> Result<reqwest::Response> {
        let mut backoff = INITIAL_BACKOFF_MS;

        for attempt in 0..MAX_RETRIES {
            let mut request = self.client.get(url);

            if let Some(p) = params {
                request = request.query(p);
            }

            match timeout(Duration::from_secs(10), request.send()).await {
                Ok(Ok(response)) => {
                    if response.status().is_success() {
                        return Ok(response);
                    } else if response.status() == StatusCode::TOO_MANY_REQUESTS {
                        warn!("Rate limited on attempt {}, backing off", attempt + 1);
                        sleep(Duration::from_millis(backoff * 10)).await;
                    } else {
                        let status = response.status();
                        let text = response.text().await.unwrap_or_default();
                        error!("API error {}: {}", status, text);
                        bail!("API error {}: {}", status, text);
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

    /// Convert GAMMA markets to our internal PolymarketEvent format
    pub fn gamma_to_events(&self, gamma: GammaMarketsResponse) -> Vec<PolymarketEvent> {
        gamma
            .data
            .into_iter()
            .map(|m| PolymarketEvent {
                id: m.condition_id.clone(),
                slug: m.slug.clone(),
                title: m.question,
                description: m.description,
                end_date_iso: m.end_date_iso,
                volume: m.volume,
                liquidity: m.liquidity,
                markets: vec![Market {
                    id: m.condition_id,
                    question: m.question_id,
                    outcome_prices: m.outcome_prices.unwrap_or_default(),
                    volume: m.volume,
                    liquidity: m.liquidity,
                }],
            })
            .collect()
    }
}

// CLOB API Types
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CLOBMarket {
    #[serde(rename = "conditionId")]
    pub condition_id: String,
    pub tokens: Vec<Token>,
    pub rewards: MarketRewards,
    #[serde(rename = "spreadData")]
    pub spread_data: Option<SpreadData>,
    #[serde(rename = "acceptingOrders")]
    pub accepting_orders: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Token {
    #[serde(rename = "tokenId")]
    pub token_id: String,
    pub outcome: String,
    pub price: f64,
    pub winner: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketRewards {
    pub min: f64,
    pub max: f64,
    #[serde(rename = "minSize")]
    pub min_size: f64,
    #[serde(rename = "maxSpread")]
    pub max_spread: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpreadData {
    pub spread: f64,
    pub bid: f64,
    pub ask: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderBook {
    pub market: String,
    pub asset_id: String,
    pub bids: Vec<Order>,
    pub asks: Vec<Order>,
    pub timestamp: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Order {
    pub price: String,
    pub size: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PriceInfo {
    pub price: f64,
    pub bid: f64,
    pub ask: f64,
    pub spread: f64,
}

// GAMMA API Types
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GammaMarketsResponse {
    pub data: Vec<GammaMarket>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GammaMarket {
    pub id: String,
    pub condition_id: String,
    pub question_id: String,
    pub slug: String,
    pub question: String,
    pub description: Option<String>,
    pub end_date_iso: Option<String>,
    pub volume: Option<f64>,
    pub liquidity: Option<f64>,
    pub outcome_prices: Option<Vec<f64>>,
    pub closed: bool,
    pub active: bool,
}

// Data API Types
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Trade {
    pub id: String,
    pub market: String,
    pub asset_id: String,
    pub side: String,
    pub size: f64,
    pub price: f64,
    pub fee: f64,
    pub trader: String,
    pub timestamp: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_polymarket_scraper() {
        let mut scraper = PolymarketScraper::new();
        // Test with real API if needed
        // let markets = scraper.fetch_markets().await;
        // assert!(markets.is_ok());
    }
}

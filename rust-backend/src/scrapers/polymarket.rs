use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::time::Duration;

#[allow(dead_code)] // Reserved for future direct API integration
const POLYMARKET_API_BASE: &str = "https://gamma-api.polymarket.com";
#[allow(dead_code)] // Reserved for future order book features
const CLOB_API_BASE: &str = "https://clob.polymarket.com";

/// Polymarket API client (public endpoints, no auth required)
pub struct PolymarketClient {
    client: reqwest::Client,
}

impl PolymarketClient {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::builder()
                .user_agent("BetterBot/1.0")
                .timeout(Duration::from_secs(15))
                .build()
                .unwrap(),
        }
    }

    /// Get events (markets grouped by event) - CRITICAL for signals 4 & 6
    pub async fn get_events(&self, limit: Option<u32>, closed: bool) -> Result<Vec<PolymarketEvent>> {
        let url = format!("{}/events", POLYMARKET_API_BASE);
        
        let mut params = vec![
            ("closed", closed.to_string()),
        ];
        
        if let Some(lim) = limit {
            params.push(("limit", lim.to_string()));
        }

        tracing::debug!("Fetching Polymarket events (limit: {:?}, closed: {})", limit, closed);

        let response = self.client
            .get(&url)
            .query(&params)
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(anyhow!("Polymarket events API error: HTTP {}", response.status()));
        }

        let events: Vec<PolymarketEvent> = response.json().await?;
        
        tracing::info!("✓ Fetched {} events from Polymarket", events.len());
        
        Ok(events)
    }

    /// Get active markets
    #[allow(dead_code)] // Reserved for Day 3+ market integration
    pub async fn get_markets(&self, limit: Option<u32>, offset: Option<u32>) -> Result<Vec<PolymarketMarket>> {
        let url = format!("{}/markets", POLYMARKET_API_BASE);
        
        let mut params = vec![];
        
        if let Some(lim) = limit {
            params.push(("limit", lim.to_string()));
        }
        
        if let Some(off) = offset {
            params.push(("offset", off.to_string()));
        }

        // Filter for active markets only
        params.push(("closed", "false".to_string()));

        tracing::debug!("Fetching Polymarket markets (limit: {:?}, offset: {:?})", limit, offset);

        let response = self.client
            .get(&url)
            .query(&params)
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(anyhow!("Polymarket API error: HTTP {}", response.status()));
        }

        let markets: Vec<PolymarketMarket> = response.json().await?;
        
        tracing::info!("✓ Fetched {} active markets from Polymarket", markets.len());
        
        Ok(markets)
    }

    /// Get specific market by condition ID
    #[allow(dead_code)] // Reserved for Day 3+ market integration
    pub async fn get_market(&self, condition_id: &str) -> Result<PolymarketMarket> {
        let url = format!("{}/markets/{}", POLYMARKET_API_BASE, condition_id);

        let response = self.client
            .get(&url)
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(anyhow!("Polymarket API error: HTTP {}", response.status()));
        }

        let market: PolymarketMarket = response.json().await?;
        
        Ok(market)
    }

    /// Get market by slug (URL-friendly name)
    #[allow(dead_code)] // Reserved for Day 3+ market integration
    pub async fn get_market_by_slug(&self, slug: &str) -> Result<PolymarketMarket> {
        let url = format!("{}/markets", POLYMARKET_API_BASE);
        
        let params = [
            ("slug", slug),
        ];

        let response = self.client
            .get(&url)
            .query(&params)
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(anyhow!("Polymarket API error: HTTP {}", response.status()));
        }

        let markets: Vec<PolymarketMarket> = response.json().await?;
        
        markets.into_iter().next()
            .ok_or_else(|| anyhow!("Market not found: {}", slug))
    }

    /// Get orderbook snapshot for a token
    #[allow(dead_code)] // Reserved for Day 3+ orderbook features
    pub async fn get_orderbook(&self, token_id: &str) -> Result<Orderbook> {
        let url = format!("{}/book", CLOB_API_BASE);
        
        let params = [
            ("token_id", token_id),
        ];

        let response = self.client
            .get(&url)
            .query(&params)
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(anyhow!("CLOB API error: HTTP {}", response.status()));
        }

        let orderbook: Orderbook = response.json().await?;
        
        Ok(orderbook)
    }

    /// Get recent trades for a token
    #[allow(dead_code)] // Reserved for Day 3+ trade tracking
    pub async fn get_trades(&self, token_id: &str, limit: Option<u32>) -> Result<Vec<Trade>> {
        let url = format!("{}/trades", CLOB_API_BASE);
        
        let mut params = vec![
            ("token_id", token_id.to_string()),
        ];
        
        if let Some(lim) = limit {
            params.push(("limit", lim.to_string()));
        }

        let response = self.client
            .get(&url)
            .query(&params)
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(anyhow!("CLOB API error: HTTP {}", response.status()));
        }

        let trades: Vec<Trade> = response.json().await?;
        
        Ok(trades)
    }

    /// Search markets by keyword (client-side filtering)
    #[allow(dead_code)] // Reserved for Day 3+ market search
    pub async fn search_markets(&self, query: &str) -> Result<Vec<PolymarketMarket>> {
        let markets = self.get_markets(Some(100), None).await?;
        
        let query_lower = query.to_lowercase();
        
        let filtered: Vec<PolymarketMarket> = markets
            .into_iter()
            .filter(|m| {
                m.question.to_lowercase().contains(&query_lower) ||
                m.description.as_ref()
                    .map(|d| d.to_lowercase().contains(&query_lower))
                    .unwrap_or(false)
            })
            .collect();
        
        tracing::debug!("Found {} markets matching '{}'", filtered.len(), query);
        
        Ok(filtered)
    }
}

// Polymarket API response types

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolymarketEvent {
    pub id: String,
    pub slug: String,
    pub title: String,
    pub description: Option<String>,
    #[serde(rename = "endDate")]
    pub end_date: Option<String>,
    pub active: bool,
    pub closed: bool,
    pub volume: Option<String>,
    #[serde(rename = "volume24hr")]
    pub volume_24hr: Option<String>,
    #[serde(rename = "volume1wk")]
    pub volume_1wk: Option<String>,
    #[serde(rename = "volume1mo")]
    pub volume_1mo: Option<String>,
    pub liquidity: Option<String>,
    pub markets: Vec<Market>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Market {
    pub id: String,
    pub question: String,
    pub outcomes: String, // JSON string like "[\"Yes\", \"No\"]"
    #[serde(rename = "outcomePrices")]
    pub outcome_prices: String, // JSON string like "[\"0.55\", \"0.45\"]"
    pub volume: Option<String>,
    #[serde(rename = "endDate")]
    pub end_date: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)] // Reserved for Day 3+ market integration
pub struct PolymarketMarket {
    pub condition_id: String,
    pub question: String,
    pub description: Option<String>,
    pub market_slug: String,
    pub end_date_iso: Option<String>,
    pub game_start_time: Option<String>,
    pub tokens: Vec<Token>,
    pub volume: Option<String>,
    pub liquidity: Option<String>,
    pub active: bool,
    pub closed: bool,
    pub archived: bool,
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)] // Reserved for Day 3+ market integration
pub struct Token {
    pub token_id: String,
    pub outcome: String,
    pub price: Option<String>,
    pub winner: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)] // Reserved for Day 3+ orderbook features
pub struct Orderbook {
    pub market: String,
    pub asset_id: String,
    pub bids: Vec<Order>,
    pub asks: Vec<Order>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)] // Reserved for Day 3+ orderbook features
pub struct Order {
    pub price: String,
    pub size: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)] // Reserved for Day 3+ trade tracking
pub struct Trade {
    pub id: String,
    pub market: String,
    pub asset_id: String,
    pub side: String, // "BUY" or "SELL"
    pub size: String,
    pub price: String,
    pub timestamp: i64,
}

impl Clone for PolymarketClient {
    fn clone(&self) -> Self {
        Self {
            client: self.client.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_get_markets() {
        let client = PolymarketClient::new();
        
        // This will hit the real API
        let result = client.get_markets(Some(5), None).await;
        
        match result {
            Ok(markets) => {
                println!("Fetched {} markets", markets.len());
                assert!(markets.len() <= 5);
            }
            Err(e) => {
                println!("API call failed (expected in CI): {}", e);
            }
        }
    }
}

//! Polymarket Data Scraper
//! Pilot in Command: Market Data Acquisition
//! Mission: Extract market inefficiencies from Polymarket

use crate::models::PolymarketEvent;
use anyhow::{Context, Result};
use reqwest::Client;
use serde::{Deserialize, Deserializer, Serialize};
use tracing::info;

const POLYMARKET_API_BASE: &str = "https://clob.polymarket.com";

pub struct PolymarketScraper {
    client: Client,
}

impl PolymarketScraper {
    pub fn new() -> Self {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("Failed to create HTTP client");

        Self { client }
    }

    pub async fn fetch_active_markets(
        &mut self,
        limit: Option<usize>,
        offset: Option<usize>,
    ) -> Result<Vec<PolymarketEvent>> {
        let limit = limit.unwrap_or(100);
        let offset = offset.unwrap_or(0);

        info!("Fetching active markets from Polymarket");

        // For now, return empty vec - would implement actual API call
        // let url = format!("{}/markets", POLYMARKET_API_BASE);
        // let response = self.client.get(&url)...

        Ok(Vec::new())
    }

    pub async fn fetch_market_orderbook(&mut self, token_id: &str) -> Result<OrderBook> {
        let url = format!("{}/book", POLYMARKET_API_BASE);

        let response = self
            .client
            .get(&url)
            .query(&[("token_id", token_id)])
            .send()
            .await
            .context("Failed to fetch orderbook")?;

        let orderbook: OrderBook = response.json().await.context("Failed to parse orderbook")?;

        Ok(orderbook)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderBook {
    pub bids: Vec<Order>,
    pub asks: Vec<Order>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Order {
    #[serde(deserialize_with = "de_f64")]
    pub price: f64,
    #[serde(deserialize_with = "de_f64")]
    pub size: f64,
}

fn de_f64<'de, D>(deserializer: D) -> Result<f64, D::Error>
where
    D: Deserializer<'de>,
{
    let v = serde_json::Value::deserialize(deserializer)?;
    match v {
        serde_json::Value::Number(n) => n
            .as_f64()
            .ok_or_else(|| serde::de::Error::custom("invalid number")),
        serde_json::Value::String(s) => s
            .parse::<f64>()
            .map_err(|_| serde::de::Error::custom("invalid float string")),
        _ => Err(serde::de::Error::custom("expected string or number")),
    }
}

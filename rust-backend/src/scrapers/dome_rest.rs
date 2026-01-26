//! Dome REST API Client
//!
//! Used for enrichment only (WebSocket remains the primary real-time feed).

use anyhow::{Context, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::Duration;

const DOME_API_BASE: &str = "https://api.domeapi.io/v1";

#[derive(Clone)]
pub struct DomeRestClient {
    client: Client,
    base_url: String,
}

impl DomeRestClient {
    pub fn new(api_key: String) -> Result<Self> {
        let client = Client::builder()
            .timeout(Duration::from_secs(30))
            .pool_max_idle_per_host(10)
            .pool_idle_timeout(Duration::from_secs(90))
            .tcp_keepalive(Duration::from_secs(60))
            .default_headers({
                let mut headers = reqwest::header::HeaderMap::new();
                headers.insert(
                    reqwest::header::AUTHORIZATION,
                    format!("Bearer {}", api_key)
                        .parse()
                        .context("Invalid DOME api key")?,
                );
                headers
            })
            .build()
            .context("Failed to build DomeRestClient")?;

        Ok(Self {
            client,
            base_url: DOME_API_BASE.to_string(),
        })
    }

    #[inline]
    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    pub async fn get_markets_by_slug(
        &self,
        market_slug: &str,
        limit: Option<u32>,
    ) -> Result<MarketsResponse> {
        let url = self.url("/polymarket/markets");
        let mut qp: Vec<(String, String)> = Vec::with_capacity(4);
        qp.push(("market_slug".to_string(), market_slug.to_string()));
        if let Some(l) = limit {
            qp.push(("limit".to_string(), l.to_string()));
        }

        let resp = self
            .client
            .get(url)
            .query(&qp)
            .send()
            .await
            .context("GET /polymarket/markets failed")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow::anyhow!(
                "GET /polymarket/markets {}: {}",
                status,
                text
            ));
        }

        resp.json::<MarketsResponse>()
            .await
            .context("Failed to parse markets response")
    }

    /// Get market by condition_id for resolution lookup
    pub async fn get_market_by_condition_id(
        &self,
        condition_id: &str,
    ) -> Result<Option<DomeMarket>> {
        let url = self.url("/polymarket/markets");
        let qp = [("condition_id", condition_id), ("limit", "1")];

        let resp = self
            .client
            .get(url)
            .query(&qp)
            .send()
            .await
            .context("GET /polymarket/markets by condition_id failed")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow::anyhow!(
                "GET /polymarket/markets?condition_id={} {}: {}",
                condition_id,
                status,
                text
            ));
        }

        let markets_resp = resp
            .json::<MarketsResponse>()
            .await
            .context("Failed to parse markets response")?;

        Ok(markets_resp.markets.into_iter().next())
    }

    /// Search markets with pagination - returns all markets matching a pattern
    pub async fn search_markets(
        &self,
        status: Option<&str>,
        limit: u32,
        offset: u32,
    ) -> Result<MarketsResponse> {
        let url = self.url("/polymarket/markets");
        let mut qp: Vec<(String, String)> = Vec::with_capacity(4);
        if let Some(s) = status {
            qp.push(("status".to_string(), s.to_string()));
        }
        qp.push(("limit".to_string(), limit.to_string()));
        qp.push(("offset".to_string(), offset.to_string()));

        let resp = self
            .client
            .get(&url)
            .query(&qp)
            .send()
            .await
            .context("GET /polymarket/markets search failed")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow::anyhow!(
                "GET /polymarket/markets search {}: {}",
                status,
                text
            ));
        }

        resp.json::<MarketsResponse>()
            .await
            .context("Failed to parse markets response")
    }

    /// Fetch all orders for a specific market slug with pagination
    pub async fn get_all_orders_for_market(
        &self,
        market_slug: &str,
        start_time: Option<i64>,
        end_time: Option<i64>,
    ) -> Result<Vec<DomeOrder>> {
        let mut all_orders: Vec<DomeOrder> = Vec::new();
        let limit = 1000u32;
        let mut pagination_key: Option<String> = None;

        for _page in 0..200 {
            let resp = self
                .get_orders_with_pagination_key(
                    OrdersFilter {
                        market_slug: Some(market_slug.to_string()),
                        condition_id: None,
                        token_id: None,
                        user: None,
                    },
                    start_time,
                    end_time,
                    Some(limit),
                    None,
                    pagination_key.clone(),
                )
                .await?;

            let count = resp.orders.len();
            all_orders.extend(resp.orders);

            pagination_key = resp.pagination.and_then(|p| p.pagination_key);
            if count < limit as usize || pagination_key.is_none() {
                break;
            }

            tokio::time::sleep(Duration::from_millis(25)).await;
        }

        Ok(all_orders)
    }

    pub async fn get_orders(
        &self,
        filter: OrdersFilter,
        start_time: Option<i64>,
        end_time: Option<i64>,
        limit: Option<u32>,
        offset: Option<u32>,
    ) -> Result<OrdersResponse> {
        self.get_orders_with_pagination_key(filter, start_time, end_time, limit, offset, None)
            .await
    }

    pub async fn get_orders_with_pagination_key(
        &self,
        filter: OrdersFilter,
        start_time: Option<i64>,
        end_time: Option<i64>,
        limit: Option<u32>,
        offset: Option<u32>,
        pagination_key: Option<String>,
    ) -> Result<OrdersResponse> {
        let url = self.url("/polymarket/orders");
        let mut qp: Vec<(String, String)> = Vec::with_capacity(10);

        if let Some(market_slug) = filter.market_slug {
            qp.push(("market_slug".to_string(), market_slug));
        }
        if let Some(condition_id) = filter.condition_id {
            qp.push(("condition_id".to_string(), condition_id));
        }
        if let Some(token_id) = filter.token_id {
            qp.push(("token_id".to_string(), token_id));
        }
        if let Some(user) = filter.user {
            qp.push(("user".to_string(), user));
        }
        if let Some(s) = start_time {
            qp.push(("start_time".to_string(), s.to_string()));
        }
        if let Some(e) = end_time {
            qp.push(("end_time".to_string(), e.to_string()));
        }
        if let Some(l) = limit {
            qp.push(("limit".to_string(), l.to_string()));
        }
        // Use pagination_key if provided, otherwise use offset
        if let Some(pk) = pagination_key {
            qp.push(("pagination_key".to_string(), pk));
        } else if let Some(o) = offset {
            qp.push(("offset".to_string(), o.to_string()));
        }

        let resp = self
            .client
            .get(url)
            .query(&qp)
            .send()
            .await
            .context("GET /polymarket/orders failed")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow::anyhow!(
                "GET /polymarket/orders {}: {}",
                status,
                text
            ));
        }

        resp.json::<OrdersResponse>()
            .await
            .context("Failed to parse orders response")
    }

    pub async fn get_orderbooks(
        &self,
        token_id: &str,
        start_time_ms: i64,
        end_time_ms: i64,
        limit: Option<u32>,
        pagination_key: Option<String>,
    ) -> Result<OrderbooksResponse> {
        let url = self.url("/polymarket/orderbooks");
        let mut qp: Vec<(String, String)> = Vec::with_capacity(6);
        qp.push(("token_id".to_string(), token_id.to_string()));
        qp.push(("start_time".to_string(), start_time_ms.to_string()));
        qp.push(("end_time".to_string(), end_time_ms.to_string()));
        if let Some(l) = limit {
            qp.push(("limit".to_string(), l.to_string()));
        }
        if let Some(k) = pagination_key {
            qp.push(("pagination_key".to_string(), k));
        }

        let resp = self
            .client
            .get(url)
            .query(&qp)
            .send()
            .await
            .context("GET /polymarket/orderbooks failed")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow::anyhow!(
                "GET /polymarket/orderbooks {}: {}",
                status,
                text
            ));
        }

        resp.json::<OrderbooksResponse>()
            .await
            .context("Failed to parse orderbooks response")
    }

    pub async fn get_activity(
        &self,
        user: &str,
        start_time: Option<i64>,
        end_time: Option<i64>,
        market_slug: Option<String>,
        condition_id: Option<String>,
        limit: Option<u32>,
        offset: Option<u32>,
    ) -> Result<ActivityResponse> {
        let url = self.url("/polymarket/activity");
        let mut qp: Vec<(String, String)> = Vec::with_capacity(8);
        qp.push(("user".to_string(), user.to_string()));
        if let Some(s) = start_time {
            qp.push(("start_time".to_string(), s.to_string()));
        }
        if let Some(e) = end_time {
            qp.push(("end_time".to_string(), e.to_string()));
        }
        if let Some(ms) = market_slug {
            qp.push(("market_slug".to_string(), ms));
        }
        if let Some(cid) = condition_id {
            qp.push(("condition_id".to_string(), cid));
        }
        if let Some(l) = limit {
            qp.push(("limit".to_string(), l.to_string()));
        }
        if let Some(o) = offset {
            qp.push(("offset".to_string(), o.to_string()));
        }

        let resp = self
            .client
            .get(url)
            .query(&qp)
            .send()
            .await
            .context("GET /polymarket/activity failed")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow::anyhow!(
                "GET /polymarket/activity {}: {}",
                status,
                text
            ));
        }

        resp.json::<ActivityResponse>()
            .await
            .context("Failed to parse activity response")
    }

    pub async fn get_market_price(
        &self,
        token_id: &str,
        at_time: Option<i64>,
    ) -> Result<MarketPriceResponse> {
        let url = self.url(&format!("/polymarket/market-price/{}", token_id));
        let mut req = self.client.get(url);
        if let Some(ts) = at_time {
            req = req.query(&[("at_time", ts)]);
        }
        let resp = req
            .send()
            .await
            .context("GET /polymarket/market-price failed")?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow::anyhow!(
                "GET /polymarket/market-price/{} {}: {}",
                token_id,
                status,
                text
            ));
        }
        resp.json::<MarketPriceResponse>()
            .await
            .context("Failed to parse market price response")
    }

    pub async fn get_candlesticks_raw(
        &self,
        condition_id: &str,
        start_time: i64,
        end_time: i64,
        interval: Option<i64>,
    ) -> Result<serde_json::Value> {
        let url = self.url(&format!("/polymarket/candlesticks/{}", condition_id));
        let mut qp: Vec<(String, String)> = Vec::with_capacity(4);
        qp.push(("start_time".to_string(), start_time.to_string()));
        qp.push(("end_time".to_string(), end_time.to_string()));
        if let Some(i) = interval {
            qp.push(("interval".to_string(), i.to_string()));
        }

        let resp = self
            .client
            .get(url)
            .query(&qp)
            .send()
            .await
            .context("GET /polymarket/candlesticks failed")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow::anyhow!(
                "GET /polymarket/candlesticks {}: {}",
                status,
                text
            ));
        }

        resp.json::<serde_json::Value>()
            .await
            .context("Failed to parse candlesticks response")
    }

    pub async fn get_wallet(
        &self,
        eoa: Option<&str>,
        proxy: Option<&str>,
    ) -> Result<WalletResponse> {
        let url = self.url("/polymarket/wallet");
        let mut qp: Vec<(String, String)> = Vec::with_capacity(1);
        if let Some(e) = eoa {
            qp.push(("eoa".to_string(), e.to_string()));
        }
        if let Some(p) = proxy {
            qp.push(("proxy".to_string(), p.to_string()));
        }
        let resp = self
            .client
            .get(url)
            .query(&qp)
            .send()
            .await
            .context("GET /polymarket/wallet failed")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow::anyhow!(
                "GET /polymarket/wallet {}: {}",
                status,
                text
            ));
        }

        resp.json::<WalletResponse>()
            .await
            .context("Failed to parse wallet response")
    }

    pub async fn get_wallet_pnl(
        &self,
        wallet_address: &str,
        granularity: WalletPnlGranularity,
        start_time: Option<i64>,
        end_time: Option<i64>,
    ) -> Result<WalletPnlResponse> {
        let url = self.url(&format!("/polymarket/wallet/pnl/{}", wallet_address));
        let mut qp: Vec<(String, String)> = Vec::with_capacity(4);
        qp.push(("granularity".to_string(), granularity.as_str().to_string()));
        if let Some(s) = start_time {
            qp.push(("start_time".to_string(), s.to_string()));
        }
        if let Some(e) = end_time {
            qp.push(("end_time".to_string(), e.to_string()));
        }
        let resp = self
            .client
            .get(url)
            .query(&qp)
            .send()
            .await
            .context("GET /polymarket/wallet/pnl failed")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow::anyhow!(
                "GET /polymarket/wallet/pnl/{} {}: {}",
                wallet_address,
                status,
                text
            ));
        }

        resp.json::<WalletPnlResponse>()
            .await
            .context("Failed to parse wallet pnl response")
    }
}

#[derive(Debug, Clone, Default)]
pub struct OrdersFilter {
    pub market_slug: Option<String>,
    pub condition_id: Option<String>,
    pub token_id: Option<String>,
    pub user: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketsResponse {
    pub markets: Vec<DomeMarket>,
    pub pagination: Option<Pagination>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DomeMarket {
    pub market_slug: String,
    pub condition_id: String,
    pub title: String,
    pub start_time: Option<i64>,
    pub end_time: Option<i64>,
    pub completed_time: Option<i64>,
    pub close_time: Option<i64>,
    pub game_start_time: Option<String>,
    pub tags: Option<Vec<String>>,
    pub volume_total: Option<f64>,
    pub volume_1_week: Option<f64>,
    pub volume_1_month: Option<f64>,
    pub volume_1_year: Option<f64>,
    pub resolution_source: Option<String>,
    pub image: Option<String>,
    pub side_a: Option<MarketSide>,
    pub side_b: Option<MarketSide>,
    pub winning_side: Option<String>,
    pub status: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketSide {
    pub id: String,
    pub label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pagination {
    pub limit: Option<i64>,
    pub offset: Option<i64>,
    pub total: Option<i64>,
    pub count: Option<i64>,
    pub has_more: Option<bool>,
    pub pagination_key: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrdersResponse {
    pub orders: Vec<DomeOrder>,
    pub pagination: Option<Pagination>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DomeOrder {
    pub token_id: String,
    #[serde(default)]
    pub token_label: Option<String>,
    pub side: String,
    pub market_slug: String,
    pub condition_id: String,
    #[serde(default)]
    pub shares: Option<f64>,
    pub shares_normalized: f64,
    pub price: f64,
    pub tx_hash: String,
    pub title: String,
    pub timestamp: i64,
    pub order_hash: String,
    pub user: String,
    #[serde(default)]
    pub taker: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderbooksResponse {
    pub snapshots: Vec<OrderbookSnapshot>,
    pub pagination: Option<Pagination>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderbookSnapshot {
    pub asks: Vec<OrderbookLevel>,
    pub bids: Vec<OrderbookLevel>,
    pub hash: Option<String>,
    #[serde(rename = "minOrderSize")]
    pub min_order_size: Option<String>,
    #[serde(rename = "negRisk")]
    pub neg_risk: Option<bool>,
    #[serde(rename = "assetId")]
    pub asset_id: Option<String>,
    pub timestamp: i64,
    #[serde(rename = "tickSize")]
    pub tick_size: Option<String>,
    #[serde(rename = "indexedAt")]
    pub indexed_at: Option<i64>,
    pub market: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderbookLevel {
    pub size: String,
    pub price: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActivityResponse {
    pub activities: Vec<ActivityItem>,
    pub pagination: Option<Pagination>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActivityItem {
    pub token_id: String,
    pub side: String,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketPriceResponse {
    pub price: f64,
    pub at_time: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WalletResponse {
    pub eoa: String,
    pub proxy: String,
    pub wallet_type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WalletPnlResponse {
    pub granularity: String,
    pub start_time: Option<i64>,
    pub end_time: Option<i64>,
    // Dome currently returns `wallet_addr` (docs show `wallet_address`). Support both.
    #[serde(alias = "wallet_addr")]
    pub wallet_address: String,
    #[serde(default)]
    pub pnl_over_time: Vec<WalletPnlPoint>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WalletPnlPoint {
    pub timestamp: i64,
    pub pnl_to_date: f64,
}

#[derive(Debug, Clone, Copy)]
pub enum WalletPnlGranularity {
    Day,
    Week,
    Month,
    Year,
    All,
}

impl WalletPnlGranularity {
    pub fn as_str(&self) -> &'static str {
        match self {
            WalletPnlGranularity::Day => "day",
            WalletPnlGranularity::Week => "week",
            WalletPnlGranularity::Month => "month",
            WalletPnlGranularity::Year => "year",
            WalletPnlGranularity::All => "all",
        }
    }
}

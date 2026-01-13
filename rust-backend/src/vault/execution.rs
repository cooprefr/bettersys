use anyhow::{anyhow, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum OrderSide {
    Buy,
    Sell,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum TimeInForce {
    Gtc,
    Ioc,
    Fok,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderRequest {
    pub client_order_id: String,
    pub token_id: String,
    pub side: OrderSide,
    /// Limit price (0..1) for Polymarket binary outcome shares.
    pub price: f64,
    /// Notional USDC to spend (BUY) or receive target (SELL). For now we treat this as "spend".
    pub notional_usdc: f64,
    pub tif: TimeInForce,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub market_slug: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderAck {
    pub order_id: String,
    pub filled_notional_usdc: f64,
    pub filled_price: f64,
    pub filled_at: i64,
}

#[async_trait::async_trait]
pub trait ExecutionAdapter: Send + Sync {
    async fn place_order(&self, req: OrderRequest) -> Result<OrderAck>;
}

#[derive(Debug, Clone)]
pub struct PaperExecutionAdapter;

#[async_trait::async_trait]
impl ExecutionAdapter for PaperExecutionAdapter {
    async fn place_order(&self, req: OrderRequest) -> Result<OrderAck> {
        if !(req.price.is_finite() && req.price > 0.0 && req.price < 1.0) {
            return Err(anyhow!("invalid price"));
        }
        if !(req.notional_usdc.is_finite() && req.notional_usdc > 0.0) {
            return Err(anyhow!("invalid notional"));
        }

        Ok(OrderAck {
            order_id: format!("paper:{}", req.client_order_id),
            filled_notional_usdc: req.notional_usdc,
            filled_price: req.price,
            filled_at: Utc::now().timestamp(),
        })
    }
}

#[derive(Debug, Clone)]
pub struct DomeExecutionAdapter {
    pub router_url: String,
    pub api_key: String,
}

impl DomeExecutionAdapter {
    pub fn from_env() -> Option<Self> {
        let router_url = std::env::var("DOME_ROUTER_URL").ok()?;
        let api_key = std::env::var("DOME_ROUTER_API_KEY")
            .or_else(|_| std::env::var("DOME_API_KEY"))
            .ok()?;

        if router_url.trim().is_empty() || api_key.trim().is_empty() {
            return None;
        }

        Some(Self {
            router_url,
            api_key,
        })
    }
}

#[async_trait::async_trait]
impl ExecutionAdapter for DomeExecutionAdapter {
    async fn place_order(&self, _req: OrderRequest) -> Result<OrderAck> {
        Err(anyhow!(
            "Dome router execution not configured (endpoint + payload pending)"
        ))
    }
}

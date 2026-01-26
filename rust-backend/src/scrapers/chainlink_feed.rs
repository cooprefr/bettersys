//! Chainlink Price Feed for Polymarket 15m Up/Down Settlement
//!
//! CRITICAL: Polymarket 15m markets settle using Chainlink oracle prices, NOT Binance spot.
//! The Chainlink feed updates on:
//! - Deviation threshold: 0.1% (BTC), 0.1% (ETH), varies for others
//! - Heartbeat: 2 seconds (frequent updates)
//!
//! However, during fast moves the Chainlink price can LAG Binance by seconds,
//! which can flip an up/down outcome. This feed tracks both to detect divergence.

use anyhow::{Context, Result};
use chrono::Utc;
use parking_lot::RwLock;
use reqwest::Client;
use serde::Deserialize;
use std::{collections::HashMap, sync::Arc, time::Duration};
use tokio::sync::broadcast;
use tracing::{debug, info, warn};

/// Chainlink feed contract addresses on Polygon Mainnet
pub mod polygon_feeds {
    /// BTC/USD: 0xc907E116054Ad103354f2D350FD2514433D57F6f
    pub const BTC_USD: &str = "0xc907E116054Ad103354f2D350FD2514433D57F6f";
    /// ETH/USD: 0xF9680D99D6C9589e2a93a78A04A279e509205945
    pub const ETH_USD: &str = "0xF9680D99D6C9589e2a93a78A04A279e509205945";
    /// SOL/USD: 0x10C8264C0935b3B9870013e057f330Ff3e9C56dC (if available)
    pub const SOL_USD: &str = "0x10C8264C0935b3B9870013e057f330Ff3e9C56dC";
    /// XRP/USD: 0x785ba89291f676b5386652eB12b30cF361020694 (if available)
    pub const XRP_USD: &str = "0x785ba89291f676b5386652eB12b30cF361020694";
}

/// Deviation thresholds (from Chainlink docs)
pub mod deviation_thresholds {
    pub const BTC_USD: f64 = 0.001; // 0.1%
    pub const ETH_USD: f64 = 0.001; // 0.1%
    pub const SOL_USD: f64 = 0.005; // 0.5% (less liquid)
    pub const XRP_USD: f64 = 0.005; // 0.5%
}

/// Price observation with timestamp and source
#[derive(Debug, Clone)]
pub struct PriceObservation {
    pub price: f64,
    pub timestamp_ms: i64,
    pub source: PriceSource,
    pub round_id: Option<u128>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PriceSource {
    Chainlink,
    Binance,
}

/// Oracle lag detection result
#[derive(Debug, Clone)]
pub struct OracleLagAnalysis {
    pub binance_price: f64,
    pub chainlink_price: f64,
    pub divergence_bps: f64,
    pub chainlink_age_ms: i64,
    pub is_stale: bool,
    pub is_dangerous_regime: bool,
}

impl OracleLagAnalysis {
    /// Returns true if we should NOT trade due to oracle uncertainty
    pub fn should_skip_trade(&self) -> bool {
        // Skip if Chainlink is stale (>5 seconds old)
        if self.is_stale {
            return true;
        }
        // Skip if divergence > 50bps (0.5%) - oracle might flip outcome
        if self.divergence_bps.abs() > 50.0 {
            return true;
        }
        // Skip in dangerous regime (high vol + divergence)
        if self.is_dangerous_regime {
            return true;
        }
        false
    }
}

/// Cached price state per asset
#[derive(Debug, Clone, Default)]
pub struct AssetPriceState {
    pub chainlink: Option<PriceObservation>,
    pub binance: Option<PriceObservation>,
    pub window_start_chainlink: Option<f64>,
    pub window_start_binance: Option<f64>,
}

/// Chainlink price feed manager
pub struct ChainlinkFeed {
    client: Client,
    rpc_url: String,
    prices: Arc<RwLock<HashMap<String, AssetPriceState>>>,
    update_tx: broadcast::Sender<ChainlinkUpdate>,
}

#[derive(Debug, Clone)]
pub struct ChainlinkUpdate {
    pub asset: String,
    pub price: f64,
    pub round_id: u128,
    pub updated_at: i64,
}

/// Response from eth_call to Chainlink aggregator
#[derive(Debug, Deserialize)]
struct JsonRpcResponse {
    result: Option<String>,
    error: Option<serde_json::Value>,
}

impl ChainlinkFeed {
    pub fn new(rpc_url: String) -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .expect("failed to build HTTP client");

        let (update_tx, _) = broadcast::channel(256);

        Self {
            client,
            rpc_url,
            prices: Arc::new(RwLock::new(HashMap::new())),
            update_tx,
        }
    }

    pub fn from_env() -> Option<Self> {
        let rpc_url = std::env::var("POLYGON_RPC_URL")
            .or_else(|_| std::env::var("CHAINLINK_RPC_URL"))
            .ok()?;

        if rpc_url.is_empty() {
            return None;
        }

        Some(Self::new(rpc_url))
    }

    /// Get the contract address for an asset
    fn get_feed_address(asset: &str) -> Option<&'static str> {
        match asset.to_uppercase().as_str() {
            "BTC" => Some(polygon_feeds::BTC_USD),
            "ETH" => Some(polygon_feeds::ETH_USD),
            "SOL" => Some(polygon_feeds::SOL_USD),
            "XRP" => Some(polygon_feeds::XRP_USD),
            _ => None,
        }
    }

    /// Fetch latest round data from Chainlink aggregator
    /// Calls `latestRoundData()` which returns (roundId, answer, startedAt, updatedAt, answeredInRound)
    pub async fn fetch_price(&self, asset: &str) -> Result<PriceObservation> {
        let feed_address = Self::get_feed_address(asset)
            .ok_or_else(|| anyhow::anyhow!("unknown asset: {}", asset))?;

        // latestRoundData() selector: 0xfeaf968c
        let call_data = "0xfeaf968c";

        let payload = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "eth_call",
            "params": [{
                "to": feed_address,
                "data": call_data
            }, "latest"],
            "id": 1
        });

        let response: JsonRpcResponse = self
            .client
            .post(&self.rpc_url)
            .json(&payload)
            .send()
            .await
            .context("RPC request failed")?
            .json()
            .await
            .context("failed to parse RPC response")?;

        if let Some(err) = response.error {
            return Err(anyhow::anyhow!("RPC error: {:?}", err));
        }

        let result = response
            .result
            .ok_or_else(|| anyhow::anyhow!("no result in RPC response"))?;

        // Decode the response (5 x uint256 = 160 bytes = 320 hex chars + 0x prefix)
        let bytes = hex::decode(result.trim_start_matches("0x"))
            .context("failed to decode hex response")?;

        if bytes.len() < 160 {
            return Err(anyhow::anyhow!("response too short: {} bytes", bytes.len()));
        }

        // Parse: roundId (32 bytes), answer (32 bytes), startedAt (32 bytes), updatedAt (32 bytes), answeredInRound (32 bytes)
        let round_id = u128::from_be_bytes(bytes[16..32].try_into().unwrap_or([0; 16]));
        let answer = i128::from_be_bytes(bytes[48..64].try_into().unwrap_or([0; 16]));
        let updated_at = i64::from_be_bytes(bytes[112..120].try_into().unwrap_or([0; 8]));

        // Chainlink prices have 8 decimals for USD pairs
        let price = (answer as f64) / 1e8;

        let obs = PriceObservation {
            price,
            timestamp_ms: updated_at * 1000,
            source: PriceSource::Chainlink,
            round_id: Some(round_id),
        };

        // Update cache
        {
            let mut prices = self.prices.write();
            let state = prices.entry(asset.to_uppercase()).or_default();
            state.chainlink = Some(obs.clone());
        }

        // Broadcast update
        let _ = self.update_tx.send(ChainlinkUpdate {
            asset: asset.to_uppercase(),
            price,
            round_id,
            updated_at,
        });

        Ok(obs)
    }

    /// Update Binance price in cache (called from BinancePriceFeed)
    pub fn update_binance_price(&self, asset: &str, price: f64) {
        let mut prices = self.prices.write();
        let state = prices.entry(asset.to_uppercase()).or_default();
        state.binance = Some(PriceObservation {
            price,
            timestamp_ms: Utc::now().timestamp_millis(),
            source: PriceSource::Binance,
            round_id: None,
        });
    }

    /// Record window start prices (called at beginning of 15m window)
    pub fn record_window_start(&self, asset: &str) {
        let mut prices = self.prices.write();
        if let Some(state) = prices.get_mut(&asset.to_uppercase()) {
            state.window_start_chainlink = state.chainlink.as_ref().map(|p| p.price);
            state.window_start_binance = state.binance.as_ref().map(|p| p.price);
        }
    }

    /// Get current price state for an asset
    pub fn get_state(&self, asset: &str) -> Option<AssetPriceState> {
        self.prices.read().get(&asset.to_uppercase()).cloned()
    }

    /// Analyze oracle lag and divergence
    pub fn analyze_lag(&self, asset: &str) -> Option<OracleLagAnalysis> {
        let prices = self.prices.read();
        let state = prices.get(&asset.to_uppercase())?;

        let chainlink = state.chainlink.as_ref()?;
        let binance = state.binance.as_ref()?;

        let now_ms = Utc::now().timestamp_millis();
        let chainlink_age_ms = now_ms - chainlink.timestamp_ms;

        let divergence_bps = ((binance.price - chainlink.price) / chainlink.price) * 10000.0;

        // Stale if > 5 seconds old
        let is_stale = chainlink_age_ms > 5000;

        // Dangerous regime: divergence > 20bps AND chainlink > 2s old
        let is_dangerous_regime = divergence_bps.abs() > 20.0 && chainlink_age_ms > 2000;

        Some(OracleLagAnalysis {
            binance_price: binance.price,
            chainlink_price: chainlink.price,
            divergence_bps,
            chainlink_age_ms,
            is_stale,
            is_dangerous_regime,
        })
    }

    /// Calculate p_up using CHAINLINK prices (the settlement source)
    /// This is the correct price to use for settlement prediction
    pub fn chainlink_p_up(&self, asset: &str, sigma_annual: f64, tte_years: f64) -> Option<f64> {
        let prices = self.prices.read();
        let state = prices.get(&asset.to_uppercase())?;

        let current = state.chainlink.as_ref()?.price;
        let strike = state.window_start_chainlink?;

        if strike <= 0.0 || current <= 0.0 || tte_years <= 0.0 || sigma_annual <= 0.0 {
            return Some(if current >= strike { 1.0 } else { 0.0 });
        }

        let log_moneyness = (current / strike).ln();
        let vol_sqrt_t = sigma_annual * tte_years.sqrt();
        let d = log_moneyness / vol_sqrt_t;

        // Standard normal CDF approximation
        Some(normal_cdf(d))
    }

    /// Subscribe to price updates
    pub fn subscribe(&self) -> broadcast::Receiver<ChainlinkUpdate> {
        self.update_tx.subscribe()
    }
}

/// Standard normal CDF approximation (Abramowitz and Stegun)
fn normal_cdf(x: f64) -> f64 {
    const A1: f64 = 0.254829592;
    const A2: f64 = -0.284496736;
    const A3: f64 = 1.421413741;
    const A4: f64 = -1.453152027;
    const A5: f64 = 1.061405429;
    const P: f64 = 0.3275911;

    let sign = if x < 0.0 { -1.0 } else { 1.0 };
    let x = x.abs();

    let t = 1.0 / (1.0 + P * x);
    let y = 1.0 - (((((A5 * t + A4) * t) + A3) * t + A2) * t + A1) * t * (-x * x / 2.0).exp();

    0.5 * (1.0 + sign * y)
}

/// Spawn background polling loop for Chainlink prices
/// Also feeds the oracle comparison tracker for settlement analysis
pub async fn spawn_chainlink_poller(feed: Arc<ChainlinkFeed>, poll_interval_ms: u64) -> Result<()> {
    use crate::scrapers::oracle_comparison::global_oracle_tracker;

    let assets = ["BTC", "ETH", "SOL", "XRP"];
    let oracle_tracker = global_oracle_tracker();

    loop {
        for asset in &assets {
            // Fetch and record Chainlink price
            match feed.fetch_price(asset).await {
                Ok(obs) => {
                    debug!(
                        asset = %asset,
                        price = %obs.price,
                        round_id = ?obs.round_id,
                        "Chainlink price updated"
                    );
                    // Feed to oracle comparison tracker
                    oracle_tracker.record_chainlink(asset, obs.price, obs.timestamp_ms / 1000);
                }
                Err(e) => {
                    warn!(asset = %asset, error = %e, "Chainlink fetch failed");
                }
            }
        }

        tokio::time::sleep(Duration::from_millis(poll_interval_ms)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normal_cdf() {
        assert!((normal_cdf(0.0) - 0.5).abs() < 0.001);
        assert!((normal_cdf(1.96) - 0.975).abs() < 0.01);
        assert!((normal_cdf(-1.96) - 0.025).abs() < 0.01);
    }
}

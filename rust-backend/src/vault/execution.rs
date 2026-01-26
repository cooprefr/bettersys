use anyhow::{anyhow, Context, Result};
use base64::{
    engine::general_purpose::{STANDARD as BASE64, URL_SAFE, URL_SAFE_NO_PAD},
    Engine,
};
use chrono::Utc;
use hmac::{Hmac, Mac};
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use tokio::time::{sleep, Duration};
use tracing::{debug, info, warn};

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
    /// Fees paid in USDC
    #[serde(default)]
    pub fees_usdc: f64,
    /// Slippage from requested price in bps
    #[serde(default)]
    pub slippage_bps: f64,
    /// Simulated latency in ms
    #[serde(default)]
    pub latency_ms: u64,
}

#[async_trait::async_trait]
pub trait ExecutionAdapter: Send + Sync {
    async fn place_order(&self, req: OrderRequest) -> Result<OrderAck>;
}

/// Paper execution configuration for realistic simulation
#[derive(Debug, Clone)]
pub struct PaperExecutionConfig {
    /// Base latency in ms (will add random jitter)
    pub base_latency_ms: u64,
    /// Max additional random latency in ms
    pub latency_jitter_ms: u64,
    /// Slippage in bps per $1000 notional (market impact)
    pub slippage_bps_per_1k: f64,
    /// Base slippage in bps (spread crossing)
    pub base_slippage_bps: f64,
    /// Fee rate (Polymarket is ~0.5% taker)
    pub fee_rate: f64,
    /// Probability of partial fill (0.0 to 1.0)
    pub partial_fill_prob: f64,
    /// Min fill ratio when partial fill occurs
    pub min_fill_ratio: f64,
    /// Probability of order rejection (0.0 to 1.0)
    pub reject_prob: f64,
}

impl Default for PaperExecutionConfig {
    fn default() -> Self {
        Self {
            base_latency_ms: 150,      // 150ms base
            latency_jitter_ms: 200,    // +0-200ms random
            slippage_bps_per_1k: 15.0, // 15bps per $1k (market impact)
            base_slippage_bps: 10.0,   // 10bps base (half-spread)
            fee_rate: 0.005,           // 0.5% taker fee
            partial_fill_prob: 0.15,   // 15% chance of partial fill
            min_fill_ratio: 0.4,       // At least 40% fills when partial
            reject_prob: 0.02,         // 2% random rejection
        }
    }
}

impl PaperExecutionConfig {
    pub fn from_env() -> Self {
        let mut config = Self::default();

        if let Ok(v) = std::env::var("PAPER_BASE_LATENCY_MS") {
            if let Ok(ms) = v.parse() {
                config.base_latency_ms = ms;
            }
        }
        if let Ok(v) = std::env::var("PAPER_LATENCY_JITTER_MS") {
            if let Ok(ms) = v.parse() {
                config.latency_jitter_ms = ms;
            }
        }
        if let Ok(v) = std::env::var("PAPER_SLIPPAGE_BPS_PER_1K") {
            if let Ok(bps) = v.parse() {
                config.slippage_bps_per_1k = bps;
            }
        }
        if let Ok(v) = std::env::var("PAPER_BASE_SLIPPAGE_BPS") {
            if let Ok(bps) = v.parse() {
                config.base_slippage_bps = bps;
            }
        }
        if let Ok(v) = std::env::var("PAPER_FEE_RATE") {
            if let Ok(rate) = v.parse() {
                config.fee_rate = rate;
            }
        }
        if let Ok(v) = std::env::var("PAPER_PARTIAL_FILL_PROB") {
            if let Ok(prob) = v.parse() {
                config.partial_fill_prob = prob;
            }
        }
        if let Ok(v) = std::env::var("PAPER_REJECT_PROB") {
            if let Ok(prob) = v.parse() {
                config.reject_prob = prob;
            }
        }

        config
    }
}

#[derive(Debug, Clone)]
pub struct PaperExecutionAdapter {
    pub config: PaperExecutionConfig,
}

impl Default for PaperExecutionAdapter {
    fn default() -> Self {
        Self {
            config: PaperExecutionConfig::from_env(),
        }
    }
}

impl PaperExecutionAdapter {
    pub fn new(config: PaperExecutionConfig) -> Self {
        Self { config }
    }
}

#[async_trait::async_trait]
impl ExecutionAdapter for PaperExecutionAdapter {
    async fn place_order(&self, req: OrderRequest) -> Result<OrderAck> {
        let mut rng = StdRng::from_entropy();

        // Validate inputs
        if !(req.price.is_finite() && req.price > 0.0 && req.price < 1.0) {
            return Err(anyhow!("invalid price"));
        }
        if !(req.notional_usdc.is_finite() && req.notional_usdc > 0.0) {
            return Err(anyhow!("invalid notional"));
        }

        // Simulate network + matching latency
        let jitter: u64 = rng.gen_range(0..=self.config.latency_jitter_ms);
        let total_latency_ms = self.config.base_latency_ms + jitter;
        sleep(Duration::from_millis(total_latency_ms)).await;

        // Random rejection (simulates network errors, invalid token, etc.)
        if rng.gen::<f64>() < self.config.reject_prob {
            return Err(anyhow!("order rejected (simulated)"));
        }

        // Calculate slippage: base + market impact based on size
        let size_factor = req.notional_usdc / 1000.0; // per $1k
        let total_slippage_bps =
            self.config.base_slippage_bps + (self.config.slippage_bps_per_1k * size_factor);

        // Apply slippage to price (adverse for trader)
        let slippage_multiplier = total_slippage_bps / 10000.0;
        let filled_price = match req.side {
            OrderSide::Buy => (req.price * (1.0 + slippage_multiplier)).min(0.99),
            OrderSide::Sell => (req.price * (1.0 - slippage_multiplier)).max(0.01),
        };

        // Determine fill ratio (partial fills)
        let fill_ratio = if rng.gen::<f64>() < self.config.partial_fill_prob {
            // Partial fill
            rng.gen_range(self.config.min_fill_ratio..1.0)
        } else {
            1.0
        };

        // For FOK orders, reject if partial
        if req.tif == TimeInForce::Fok && fill_ratio < 1.0 {
            return Err(anyhow!("FOK order could not be fully filled"));
        }

        let filled_notional = req.notional_usdc * fill_ratio;

        // Calculate fees
        let fees_usdc = filled_notional * self.config.fee_rate;

        Ok(OrderAck {
            order_id: format!("paper:{}", req.client_order_id),
            filled_notional_usdc: filled_notional,
            filled_price,
            filled_at: Utc::now().timestamp(),
            fees_usdc,
            slippage_bps: total_slippage_bps,
            latency_ms: total_latency_ms,
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

// ============================================================================
// Polymarket CLOB Execution Adapter (LIVE TRADING)
// ============================================================================

type HmacSha256 = Hmac<Sha256>;

/// Polymarket CLOB API credentials (builder credentials)
#[derive(Debug, Clone)]
pub struct PolymarketClobCredentials {
    pub api_key: String,
    pub secret: String,
    pub passphrase: String,
}

impl PolymarketClobCredentials {
    pub fn from_env() -> Option<Self> {
        let api_key = std::env::var("POLYMARKET_CLOB_API_KEY").ok()?;
        let secret = std::env::var("POLYMARKET_CLOB_SECRET").ok()?;
        let passphrase = std::env::var("POLYMARKET_CLOB_PASSPHRASE").ok()?;

        if api_key.is_empty() || secret.is_empty() || passphrase.is_empty() {
            return None;
        }

        Some(Self {
            api_key,
            secret,
            passphrase,
        })
    }
}

/// Live execution adapter for Polymarket CLOB
#[derive(Clone)]
pub struct PolymarketClobAdapter {
    client: Client,
    creds: PolymarketClobCredentials,
    host: String,
}

impl std::fmt::Debug for PolymarketClobAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PolymarketClobAdapter")
            .field("host", &self.host)
            .field("api_key", &"[REDACTED]")
            .finish()
    }
}

/// Order payload for Polymarket CLOB API
#[derive(Debug, Serialize)]
struct ClobOrderPayload {
    #[serde(rename = "tokenID")]
    token_id: String,
    price: String,
    size: String,
    side: String,
    #[serde(rename = "orderType", skip_serializing_if = "Option::is_none")]
    order_type: Option<String>,
    #[serde(rename = "timeInForce", skip_serializing_if = "Option::is_none")]
    time_in_force: Option<String>,
}

/// Response from Polymarket CLOB order endpoint
#[derive(Debug, Deserialize)]
struct ClobOrderResponse {
    #[serde(rename = "orderID", alias = "orderId", alias = "order_id")]
    order_id: Option<String>,
    #[serde(default)]
    success: bool,
    #[serde(default)]
    status: Option<String>,
    #[serde(rename = "errorMsg", alias = "error", default)]
    error_msg: Option<String>,
    // Fill info (may come from different response shapes)
    #[serde(rename = "filledSize", alias = "filled_size", default)]
    filled_size: Option<String>,
    #[serde(rename = "avgPrice", alias = "avg_price", default)]
    avg_price: Option<String>,
}

/// Account balance response from Polymarket
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PolymarketBalance {
    #[serde(default)]
    pub balance: f64,
    #[serde(rename = "allowance", default)]
    pub allowance: f64,
}

/// Position from Polymarket
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PolymarketPosition {
    #[serde(rename = "asset_id", alias = "assetId", alias = "token_id", default)]
    pub token_id: String,
    #[serde(default)]
    pub size: f64,
    #[serde(rename = "avgPrice", alias = "avg_price", default)]
    pub avg_price: f64,
    #[serde(rename = "marketSlug", alias = "market_slug", default)]
    pub market_slug: Option<String>,
    #[serde(default)]
    pub outcome: Option<String>,
    #[serde(rename = "curPrice", alias = "cur_price", default)]
    pub current_price: Option<f64>,
}

/// Account info with balance and positions
#[derive(Debug, Clone, Serialize)]
pub struct PolymarketAccountInfo {
    pub balance_usdc: f64,
    pub positions: Vec<PolymarketPosition>,
    pub positions_value_usdc: f64,
    pub total_value_usdc: f64,
    pub fetched_at: i64,
}

impl PolymarketClobAdapter {
    pub const CLOB_HOST: &'static str = "https://clob.polymarket.com";
    pub const DATA_API_HOST: &'static str = "https://data-api.polymarket.com";

    pub fn new(creds: PolymarketClobCredentials) -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .expect("failed to build HTTP client");

        Self {
            client,
            creds,
            host: Self::CLOB_HOST.to_string(),
        }
    }

    pub fn from_env() -> Option<Self> {
        let creds = PolymarketClobCredentials::from_env();
        if creds.is_none() {
            warn!("PolymarketClobAdapter::from_env() - credentials not found");
            return None;
        }
        info!("PolymarketClobAdapter::from_env() - initialized with CLOB credentials");
        Some(Self::new(creds.unwrap()))
    }

    /// Get the wallet address associated with this API key
    pub fn wallet_address(&self) -> Option<String> {
        // The API key is typically the wallet address for Polymarket
        // Or we need to fetch it from an endpoint
        std::env::var("POLYMARKET_WALLET_ADDRESS").ok()
    }

    /// Fetch account balance from Polymarket CLOB
    /// Uses the authenticated /balance-allowance endpoint
    pub async fn get_balance(&self) -> Result<f64> {
        // The CLOB uses /balance-allowance with signature_type param
        // signature_type: 0 = EOA, 1 = POLY_GNOSIS_SAFE, 2 = POLY_PROXY
        let path = "/balance-allowance?signature_type=2";
        let method = "GET";
        let body = "";

        let headers = self.auth_headers(method, path, body)?;
        let url = format!("{}{}", self.host, path);

        info!(url = %url, "fetching balance from CLOB");

        let mut request = self.client.get(&url);
        for (key, value) in headers {
            request = request.header(&key, &value);
        }

        let response = request.send().await.context("balance request failed")?;
        let status = response.status();

        let resp_text = response.text().await.unwrap_or_default();
        info!(status = %status, response = %resp_text, "balance API response");

        if !status.is_success() {
            return Err(anyhow!(
                "balance request failed ({}): {}",
                status,
                resp_text
            ));
        }

        // Response format: {"balance": "123.45", "allowance": "999999..."}
        if let Ok(map) =
            serde_json::from_str::<std::collections::HashMap<String, serde_json::Value>>(&resp_text)
        {
            // Look for balance field
            if let Some(bal_val) = map.get("balance") {
                if let Some(bal) = bal_val.as_f64() {
                    return Ok(bal);
                }
                if let Some(bal_str) = bal_val.as_str() {
                    if let Ok(bal) = bal_str.parse::<f64>() {
                        // Balance is in wei (6 decimals for USDC)
                        return Ok(bal / 1_000_000.0);
                    }
                }
            }
        }

        // Try parsing as object with balance field
        if let Ok(bal) = serde_json::from_str::<PolymarketBalance>(&resp_text) {
            return Ok(bal.balance);
        }

        warn!(response = %resp_text, "could not parse balance response");
        Ok(0.0)
    }

    /// Fetch positions from Polymarket Data API
    pub async fn get_positions(&self) -> Result<Vec<PolymarketPosition>> {
        let wallet = self
            .wallet_address()
            .ok_or_else(|| anyhow!("POLYMARKET_WALLET_ADDRESS not set"))?;

        // Use CLOB API for positions (more accurate than data API)
        // Try /positions endpoint first
        let path = format!("/positions?address={}", wallet);
        let method = "GET";
        let body = "";

        let headers = self.auth_headers(method, &path, body)?;
        let url = format!("{}{}", self.host, path);

        debug!(url = %url, "fetching positions");

        let mut request = self.client.get(&url).timeout(Duration::from_secs(10));
        for (key, value) in headers {
            request = request.header(&key, &value);
        }

        let response = request.send().await;

        if let Ok(resp) = response {
            let status = resp.status();
            let resp_text = resp.text().await.unwrap_or_default();
            info!(status = %status, response_len = %resp_text.len(), "positions API response");

            if status.is_success() && !resp_text.is_empty() && resp_text != "[]" {
                if let Ok(positions) = serde_json::from_str::<Vec<PolymarketPosition>>(&resp_text) {
                    return Ok(positions);
                }
            }
        }

        // Fallback to Data API
        let url = format!("{}/positions?user={}", Self::DATA_API_HOST, wallet);

        let response = self
            .client
            .get(&url)
            .timeout(Duration::from_secs(10))
            .send()
            .await
            .context("positions request failed")?;

        let status = response.status();
        if !status.is_success() {
            let error_text = response.text().await.unwrap_or_default();
            return Err(anyhow!(
                "positions request failed ({}): {}",
                status,
                error_text
            ));
        }

        let resp_text = response.text().await?;
        debug!(response = %resp_text, "positions response");

        // Try parsing as array of positions
        let positions: Vec<PolymarketPosition> =
            serde_json::from_str(&resp_text).context("failed to parse positions")?;

        Ok(positions)
    }

    /// Fetch full account info (balance + positions)
    pub async fn get_account_info(&self) -> Result<PolymarketAccountInfo> {
        info!("get_account_info: fetching balance...");
        let balance = match self.get_balance().await {
            Ok(b) => {
                info!(balance = %b, "get_account_info: balance fetched");
                b
            }
            Err(e) => {
                warn!(error = %e, "get_account_info: balance fetch failed");
                0.0
            }
        };
        info!("get_account_info: fetching positions...");
        let positions = self.get_positions().await.unwrap_or_default();
        info!(num_positions = %positions.len(), "get_account_info: positions fetched");

        let positions_value: f64 = positions
            .iter()
            .map(|p| {
                let price = p.current_price.unwrap_or(p.avg_price);
                p.size * price
            })
            .sum();

        Ok(PolymarketAccountInfo {
            balance_usdc: balance,
            positions,
            positions_value_usdc: positions_value,
            total_value_usdc: balance + positions_value,
            fetched_at: Utc::now().timestamp(),
        })
    }

    /// Generate HMAC signature for Polymarket L2 authentication
    fn sign_request(&self, method: &str, path: &str, body: &str, timestamp: i64) -> Result<String> {
        // Message format: timestamp + method + path + body
        let message = format!("{}{}{}{}", timestamp, method, path, body);
        debug!(message = %message, timestamp = %timestamp, method = %method, path = %path, "signing request");

        // Decode base64 secret (try URL-safe first, then standard)
        let secret_bytes = URL_SAFE
            .decode(&self.creds.secret)
            .or_else(|_| URL_SAFE_NO_PAD.decode(&self.creds.secret))
            .or_else(|_| BASE64.decode(&self.creds.secret))
            .context("failed to decode CLOB secret")?;

        // Create HMAC-SHA256
        let mut mac = HmacSha256::new_from_slice(&secret_bytes)
            .map_err(|e| anyhow!("HMAC key error: {}", e))?;
        mac.update(message.as_bytes());

        // URL-safe Base64 encode the signature (same as Python's urlsafe_b64encode)
        let signature = URL_SAFE.encode(mac.finalize().into_bytes());
        Ok(signature)
    }

    /// Build authenticated request headers
    fn auth_headers(&self, method: &str, path: &str, body: &str) -> Result<Vec<(String, String)>> {
        // Timestamp in SECONDS (not milliseconds)
        let timestamp = Utc::now().timestamp();
        let signature = self.sign_request(method, path, body, timestamp)?;

        // Get wallet address
        let wallet = self.wallet_address().unwrap_or_default();

        Ok(vec![
            ("POLY_ADDRESS".to_string(), wallet),
            ("POLY_API_KEY".to_string(), self.creds.api_key.clone()),
            ("POLY_SIGNATURE".to_string(), signature),
            ("POLY_TIMESTAMP".to_string(), timestamp.to_string()),
            ("POLY_PASSPHRASE".to_string(), self.creds.passphrase.clone()),
        ])
    }
}

#[async_trait::async_trait]
impl ExecutionAdapter for PolymarketClobAdapter {
    async fn place_order(&self, req: OrderRequest) -> Result<OrderAck> {
        let start = std::time::Instant::now();

        // Validate inputs
        if !(req.price.is_finite() && req.price > 0.0 && req.price < 1.0) {
            return Err(anyhow!("invalid price: {}", req.price));
        }
        if !(req.notional_usdc.is_finite() && req.notional_usdc > 0.0) {
            return Err(anyhow!("invalid notional: {}", req.notional_usdc));
        }

        // Calculate size (shares) from notional
        let size = req.notional_usdc / req.price;

        // Build order payload
        let side_str = match req.side {
            OrderSide::Buy => "BUY",
            OrderSide::Sell => "SELL",
        };

        let tif_str = match req.tif {
            TimeInForce::Gtc => "GTC",
            TimeInForce::Ioc => "IOC",
            TimeInForce::Fok => "FOK",
        };

        let payload = ClobOrderPayload {
            token_id: req.token_id.clone(),
            price: format!("{:.4}", req.price),
            size: format!("{:.6}", size),
            side: side_str.to_string(),
            order_type: Some("LIMIT".to_string()),
            time_in_force: Some(tif_str.to_string()),
        };

        let body = serde_json::to_string(&payload).context("failed to serialize order")?;
        let path = "/order";
        let method = "POST";

        // Build auth headers
        let headers = self.auth_headers(method, path, &body)?;

        debug!(
            token_id = %req.token_id,
            side = %side_str,
            price = %req.price,
            size = %size,
            notional = %req.notional_usdc,
            "CLOB order submission"
        );

        // Send request
        let url = format!("{}{}", self.host, path);
        let mut request = self
            .client
            .post(&url)
            .header("Content-Type", "application/json");

        for (key, value) in headers {
            request = request.header(&key, &value);
        }

        let response = request
            .body(body.clone())
            .send()
            .await
            .context("CLOB request failed")?;

        let status = response.status();
        let latency_ms = start.elapsed().as_millis() as u64;

        if !status.is_success() {
            let error_text = response.text().await.unwrap_or_default();
            warn!(
                status = %status,
                error = %error_text,
                latency_ms = %latency_ms,
                "CLOB order rejected"
            );
            return Err(anyhow!("CLOB order rejected ({}): {}", status, error_text));
        }

        let resp_text = response.text().await.context("failed to read response")?;
        let resp: ClobOrderResponse =
            serde_json::from_str(&resp_text).context("failed to parse CLOB response")?;

        if let Some(err) = resp.error_msg {
            if !err.is_empty() {
                return Err(anyhow!("CLOB error: {}", err));
            }
        }

        let order_id = resp
            .order_id
            .unwrap_or_else(|| format!("clob:{}", req.client_order_id));

        // Parse fill info if available, otherwise assume full fill at limit price
        let filled_size: f64 = resp
            .filled_size
            .and_then(|s| s.parse().ok())
            .unwrap_or(size);
        let filled_price: f64 = resp
            .avg_price
            .and_then(|s| s.parse().ok())
            .unwrap_or(req.price);
        let filled_notional = filled_size * filled_price;

        // Polymarket taker fee is ~0.5%
        let fees_usdc = filled_notional * 0.005;

        info!(
            order_id = %order_id,
            filled_size = %filled_size,
            filled_price = %filled_price,
            filled_notional = %filled_notional,
            latency_ms = %latency_ms,
            "CLOB order filled"
        );

        Ok(OrderAck {
            order_id,
            filled_notional_usdc: filled_notional,
            filled_price,
            filled_at: Utc::now().timestamp(),
            fees_usdc,
            slippage_bps: 0.0, // Would need pre-trade quote to calculate
            latency_ms,
        })
    }
}

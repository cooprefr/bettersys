//! Chainlink Feed Validation
//!
//! Validates feed addresses against chain/network at startup:
//! - RPC endpoint chain_id matches configured chain_id
//! - Feed proxy address is a contract (code size > 0)
//! - decimals() matches configured value
//! - description() matches expected asset
//! - latestRoundData() returns sane values
//!
//! Validation is MANDATORY for production-grade runs.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tracing::{debug, info, warn};

use super::config::{OracleConfig, OracleFeedConfig};

// =============================================================================
// VALIDATION RESULTS
// =============================================================================

/// Result of validating a single feed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedValidationResult {
    /// Feed ID.
    pub feed_id: String,
    /// Asset symbol.
    pub asset_symbol: String,
    /// Whether validation passed.
    pub passed: bool,
    /// Chain ID from RPC endpoint.
    pub rpc_chain_id: Option<u64>,
    /// Configured chain ID.
    pub configured_chain_id: u64,
    /// Whether the address has code (is a contract).
    pub is_contract: Option<bool>,
    /// On-chain decimals.
    pub onchain_decimals: Option<u8>,
    /// Configured decimals.
    pub configured_decimals: u8,
    /// On-chain description.
    pub onchain_description: Option<String>,
    /// Expected description.
    pub expected_description: Option<String>,
    /// Latest round answer (sanity check).
    pub latest_answer: Option<f64>,
    /// Latest round updated_at.
    pub latest_updated_at: Option<u64>,
    /// Validation errors.
    pub errors: Vec<String>,
    /// Validation warnings.
    pub warnings: Vec<String>,
}

impl FeedValidationResult {
    pub fn new(feed_config: &OracleFeedConfig) -> Self {
        Self {
            feed_id: feed_config.feed_id(),
            asset_symbol: feed_config.asset_symbol.clone(),
            passed: false,
            rpc_chain_id: None,
            configured_chain_id: feed_config.chain_id,
            is_contract: None,
            onchain_decimals: None,
            configured_decimals: feed_config.decimals,
            onchain_description: None,
            expected_description: feed_config.expected_description.clone(),
            latest_answer: None,
            latest_updated_at: None,
            errors: Vec::new(),
            warnings: Vec::new(),
        }
    }

    pub fn add_error(&mut self, error: impl Into<String>) {
        self.errors.push(error.into());
        self.passed = false;
    }

    pub fn add_warning(&mut self, warning: impl Into<String>) {
        self.warnings.push(warning.into());
    }
}

/// Result of validating all feeds.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OracleValidationResult {
    /// Whether all validations passed.
    pub all_passed: bool,
    /// Per-feed results.
    pub feed_results: Vec<FeedValidationResult>,
    /// RPC endpoint used.
    pub rpc_endpoint: String,
    /// Validation timestamp.
    pub validated_at: u64,
    /// Total validation time (ms).
    pub validation_time_ms: u64,
}

impl OracleValidationResult {
    pub fn format_report(&self) -> String {
        let mut out = String::new();
        
        out.push_str("╔══════════════════════════════════════════════════════════════════════════════╗\n");
        out.push_str("║                    ORACLE FEED VALIDATION REPORT                             ║\n");
        out.push_str("╠══════════════════════════════════════════════════════════════════════════════╣\n");
        out.push_str(&format!("║  Status:           {:57} ║\n", 
            if self.all_passed { "PASS" } else { "FAIL" }));
        out.push_str(&format!("║  Validation Time:  {:>10} ms                                           ║\n", 
            self.validation_time_ms));
        out.push_str(&format!("║  Feeds Validated:  {:>10}                                                ║\n", 
            self.feed_results.len()));
        
        for result in &self.feed_results {
            out.push_str("╠══════════════════════════════════════════════════════════════════════════════╣\n");
            out.push_str(&format!("║  Feed: {} ({})                                                    ║\n",
                result.asset_symbol, 
                if result.passed { "PASS" } else { "FAIL" }));
            out.push_str(&format!("║    Chain ID:   config={} rpc={}                                     ║\n",
                result.configured_chain_id,
                result.rpc_chain_id.map(|c| c.to_string()).unwrap_or_else(|| "?".to_string())
            ));
            out.push_str(&format!("║    Decimals:   config={} onchain={}                                   ║\n",
                result.configured_decimals,
                result.onchain_decimals.map(|d| d.to_string()).unwrap_or_else(|| "?".to_string())
            ));
            out.push_str(&format!("║    Is Contract: {}                                                      ║\n",
                result.is_contract.map(|b| if b { "YES" } else { "NO" }).unwrap_or("?")
            ));
            
            if let Some(answer) = result.latest_answer {
                out.push_str(&format!("║    Latest Price: ${:.2}                                               ║\n", answer));
            }
            
            for err in &result.errors {
                let display = if err.len() > 60 { &err[..60] } else { err };
                out.push_str(&format!("║    ERROR: {:64} ║\n", display));
            }
            for warn in &result.warnings {
                let display = if warn.len() > 60 { &warn[..60] } else { warn };
                out.push_str(&format!("║    WARN:  {:64} ║\n", display));
            }
        }
        
        out.push_str("╚══════════════════════════════════════════════════════════════════════════════╝\n");
        
        out
    }
}

// =============================================================================
// FEED VALIDATOR
// =============================================================================

/// Validates Chainlink feeds against the actual chain.
pub struct OracleFeedValidator {
    /// HTTP client for RPC calls.
    client: reqwest::Client,
    /// Cache of validation results (feed_id -> result).
    cache: std::collections::HashMap<String, FeedValidationResult>,
    /// Whether to use cache.
    use_cache: bool,
}

impl OracleFeedValidator {
    /// Create a new validator.
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .expect("failed to build HTTP client");
        
        Self {
            client,
            cache: std::collections::HashMap::new(),
            use_cache: true,
        }
    }

    /// Disable caching (for testing).
    pub fn without_cache(mut self) -> Self {
        self.use_cache = false;
        self
    }

    /// Validate all feeds in an oracle configuration.
    pub async fn validate_all(&mut self, config: &OracleConfig) -> Result<OracleValidationResult> {
        let rpc_endpoint = config.rpc_endpoint.clone()
            .or_else(|| std::env::var("POLYGON_RPC_URL").ok())
            .or_else(|| std::env::var("CHAINLINK_RPC_URL").ok())
            .ok_or_else(|| anyhow::anyhow!(
                "No RPC endpoint. Set POLYGON_RPC_URL or CHAINLINK_RPC_URL"
            ))?;
        
        let start = std::time::Instant::now();
        let mut feed_results = Vec::new();
        
        // First, validate chain ID from RPC
        let rpc_chain_id = self.get_chain_id(&rpc_endpoint).await?;
        
        for (asset, feed_config) in &config.feeds {
            // Check cache
            if self.use_cache {
                if let Some(cached) = self.cache.get(&feed_config.feed_id()) {
                    feed_results.push(cached.clone());
                    continue;
                }
            }
            
            let result = self.validate_feed(feed_config, &rpc_endpoint, rpc_chain_id).await;
            
            if self.use_cache {
                self.cache.insert(feed_config.feed_id(), result.clone());
            }
            
            feed_results.push(result);
        }
        
        let all_passed = feed_results.iter().all(|r| r.passed);
        let validation_time_ms = start.elapsed().as_millis() as u64;
        
        let validated_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        
        Ok(OracleValidationResult {
            all_passed,
            feed_results,
            rpc_endpoint,
            validated_at,
            validation_time_ms,
        })
    }

    /// Validate a single feed.
    pub async fn validate_feed(
        &self,
        feed_config: &OracleFeedConfig,
        rpc_endpoint: &str,
        rpc_chain_id: u64,
    ) -> FeedValidationResult {
        let mut result = FeedValidationResult::new(feed_config);
        result.rpc_chain_id = Some(rpc_chain_id);
        result.passed = true; // Assume pass, set to false on any error

        // 1. Chain ID match
        if rpc_chain_id != feed_config.chain_id {
            result.add_error(format!(
                "Chain ID mismatch: configured {} but RPC reports {}",
                feed_config.chain_id, rpc_chain_id
            ));
        }

        // 2. Check if address is a contract
        match self.get_code(&rpc_endpoint, &feed_config.feed_proxy_address).await {
            Ok(code) => {
                let is_contract = code.len() > 2; // "0x" means no code
                result.is_contract = Some(is_contract);
                if !is_contract {
                    result.add_error("Address is not a contract (no code)");
                }
            }
            Err(e) => {
                result.add_error(format!("Failed to check contract code: {}", e));
            }
        }

        // 3. Check decimals()
        match self.call_decimals(&rpc_endpoint, &feed_config.feed_proxy_address).await {
            Ok(decimals) => {
                result.onchain_decimals = Some(decimals);
                if decimals != feed_config.decimals {
                    result.add_error(format!(
                        "Decimals mismatch: configured {} but on-chain is {}",
                        feed_config.decimals, decimals
                    ));
                }
            }
            Err(e) => {
                result.add_error(format!("Failed to call decimals(): {}", e));
            }
        }

        // 4. Check description()
        match self.call_description(&rpc_endpoint, &feed_config.feed_proxy_address).await {
            Ok(description) => {
                result.onchain_description = Some(description.clone());
                
                // If expected description is set, check it
                if let Some(ref expected) = feed_config.expected_description {
                    if !description.contains(expected) && !expected.contains(&description) {
                        result.add_warning(format!(
                            "Description mismatch: expected '{}' but got '{}'",
                            expected, description
                        ));
                    }
                }
                
                // Sanity check: description should contain asset symbol
                if !description.to_uppercase().contains(&feed_config.asset_symbol.to_uppercase()) {
                    result.add_warning(format!(
                        "Description '{}' doesn't contain asset symbol '{}'",
                        description, feed_config.asset_symbol
                    ));
                }
            }
            Err(e) => {
                // Description is optional, just warn
                result.add_warning(format!("Failed to call description(): {}", e));
            }
        }

        // 5. Check latestRoundData() for sanity
        match self.call_latest_round_data(&rpc_endpoint, &feed_config.feed_proxy_address).await {
            Ok((answer, updated_at)) => {
                let price = (answer as f64) / 10f64.powi(feed_config.decimals as i32);
                result.latest_answer = Some(price);
                result.latest_updated_at = Some(updated_at);
                
                // Sanity checks
                if answer == 0 {
                    result.add_error("latestRoundData() returned answer = 0");
                }
                if updated_at == 0 {
                    result.add_error("latestRoundData() returned updatedAt = 0");
                }
                
                // Check for stale data (> 1 day old)
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs();
                if now > updated_at + 86400 {
                    result.add_warning(format!(
                        "Latest data is stale: {} seconds old",
                        now - updated_at
                    ));
                }
                
                // Price sanity check for known assets
                match feed_config.asset_symbol.as_str() {
                    "BTC" if price < 1000.0 || price > 1_000_000.0 => {
                        result.add_warning(format!("BTC price ${:.2} seems unusual", price));
                    }
                    "ETH" if price < 100.0 || price > 100_000.0 => {
                        result.add_warning(format!("ETH price ${:.2} seems unusual", price));
                    }
                    _ => {}
                }
            }
            Err(e) => {
                result.add_error(format!("Failed to call latestRoundData(): {}", e));
            }
        }

        result
    }

    /// Get chain ID from RPC endpoint.
    async fn get_chain_id(&self, rpc_endpoint: &str) -> Result<u64> {
        let payload = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "eth_chainId",
            "params": [],
            "id": 1
        });
        
        let response: serde_json::Value = self.client
            .post(rpc_endpoint)
            .json(&payload)
            .send()
            .await
            .context("RPC request failed")?
            .json()
            .await
            .context("Failed to parse response")?;
        
        if let Some(error) = response.get("error") {
            return Err(anyhow::anyhow!("RPC error: {:?}", error));
        }
        
        let chain_id_hex = response.get("result")
            .and_then(|r| r.as_str())
            .ok_or_else(|| anyhow::anyhow!("No result in response"))?;
        
        let chain_id = u64::from_str_radix(chain_id_hex.trim_start_matches("0x"), 16)
            .context("Failed to parse chain ID")?;
        
        Ok(chain_id)
    }

    /// Get code at an address.
    async fn get_code(&self, rpc_endpoint: &str, address: &str) -> Result<String> {
        let payload = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "eth_getCode",
            "params": [address, "latest"],
            "id": 1
        });
        
        let response: serde_json::Value = self.client
            .post(rpc_endpoint)
            .json(&payload)
            .send()
            .await?
            .json()
            .await?;
        
        if let Some(error) = response.get("error") {
            return Err(anyhow::anyhow!("RPC error: {:?}", error));
        }
        
        let code = response.get("result")
            .and_then(|r| r.as_str())
            .unwrap_or("0x")
            .to_string();
        
        Ok(code)
    }

    /// Call decimals() on the feed.
    async fn call_decimals(&self, rpc_endpoint: &str, address: &str) -> Result<u8> {
        // decimals() selector: 0x313ce567
        let call_data = "0x313ce567";
        
        let payload = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "eth_call",
            "params": [{
                "to": address,
                "data": call_data
            }, "latest"],
            "id": 1
        });
        
        let response: serde_json::Value = self.client
            .post(rpc_endpoint)
            .json(&payload)
            .send()
            .await?
            .json()
            .await?;
        
        if let Some(error) = response.get("error") {
            return Err(anyhow::anyhow!("RPC error: {:?}", error));
        }
        
        let result = response.get("result")
            .and_then(|r| r.as_str())
            .ok_or_else(|| anyhow::anyhow!("No result"))?;
        
        let bytes = hex::decode(result.trim_start_matches("0x"))?;
        if bytes.len() < 32 {
            return Err(anyhow::anyhow!("Invalid response length"));
        }
        
        Ok(bytes[31])
    }

    /// Call description() on the feed.
    async fn call_description(&self, rpc_endpoint: &str, address: &str) -> Result<String> {
        // description() selector: 0x7284e416
        let call_data = "0x7284e416";
        
        let payload = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "eth_call",
            "params": [{
                "to": address,
                "data": call_data
            }, "latest"],
            "id": 1
        });
        
        let response: serde_json::Value = self.client
            .post(rpc_endpoint)
            .json(&payload)
            .send()
            .await?
            .json()
            .await?;
        
        if let Some(error) = response.get("error") {
            return Err(anyhow::anyhow!("RPC error: {:?}", error));
        }
        
        let result = response.get("result")
            .and_then(|r| r.as_str())
            .ok_or_else(|| anyhow::anyhow!("No result"))?;
        
        // Decode string from ABI encoding
        let bytes = hex::decode(result.trim_start_matches("0x"))?;
        if bytes.len() < 64 {
            return Err(anyhow::anyhow!("Invalid response length"));
        }
        
        // String offset is at bytes[0..32], length at [offset..offset+32], data after
        let offset = u64::from_be_bytes(bytes[24..32].try_into().unwrap_or([0; 8])) as usize;
        if bytes.len() < offset + 32 {
            return Err(anyhow::anyhow!("Invalid string offset"));
        }
        
        let len = u64::from_be_bytes(bytes[offset + 24..offset + 32].try_into().unwrap_or([0; 8])) as usize;
        if bytes.len() < offset + 32 + len {
            return Err(anyhow::anyhow!("Invalid string length"));
        }
        
        let description = String::from_utf8_lossy(&bytes[offset + 32..offset + 32 + len]).to_string();
        
        Ok(description)
    }

    /// Call latestRoundData() on the feed.
    async fn call_latest_round_data(&self, rpc_endpoint: &str, address: &str) -> Result<(i128, u64)> {
        // latestRoundData() selector: 0xfeaf968c
        let call_data = "0xfeaf968c";
        
        let payload = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "eth_call",
            "params": [{
                "to": address,
                "data": call_data
            }, "latest"],
            "id": 1
        });
        
        let response: serde_json::Value = self.client
            .post(rpc_endpoint)
            .json(&payload)
            .send()
            .await?
            .json()
            .await?;
        
        if let Some(error) = response.get("error") {
            return Err(anyhow::anyhow!("RPC error: {:?}", error));
        }
        
        let result = response.get("result")
            .and_then(|r| r.as_str())
            .ok_or_else(|| anyhow::anyhow!("No result"))?;
        
        let bytes = hex::decode(result.trim_start_matches("0x"))?;
        if bytes.len() < 160 {
            return Err(anyhow::anyhow!("Invalid response length: {}", bytes.len()));
        }
        
        // Parse: roundId (32), answer (32), startedAt (32), updatedAt (32), answeredInRound (32)
        let answer = i128::from_be_bytes(bytes[48..64].try_into().unwrap_or([0; 16]));
        let updated_at = u64::from_be_bytes(bytes[120..128].try_into().unwrap_or([0; 8]));
        
        Ok((answer, updated_at))
    }

    /// Clear the validation cache.
    pub fn clear_cache(&mut self) {
        self.cache.clear();
    }
}

impl Default for OracleFeedValidator {
    fn default() -> Self {
        Self::new()
    }
}

// =============================================================================
// PRODUCTION VALIDATION GATE
// =============================================================================

/// Validates oracle configuration and aborts on failure in production mode.
///
/// Call this at the start of any production-grade backtest to ensure
/// oracle configuration is correct before processing any data.
pub async fn validate_oracle_config_for_production(
    config: &OracleConfig,
    allow_non_production: bool,
) -> Result<OracleValidationResult> {
    // First, validate config structure
    let config_validation = config.validate_production();
    if !config_validation.is_valid {
        if !allow_non_production {
            return Err(anyhow::anyhow!(
                "Oracle configuration validation failed:\n{}",
                config_validation.format_report()
            ));
        }
        warn!("Oracle configuration invalid (non-production mode):\n{}", 
            config_validation.format_report());
    }

    // Then, validate against chain
    let mut validator = OracleFeedValidator::new();
    let result = validator.validate_all(config).await?;
    
    if !result.all_passed {
        if !allow_non_production {
            return Err(anyhow::anyhow!(
                "Oracle feed validation failed:\n{}",
                result.format_report()
            ));
        }
        warn!("Oracle feed validation failed (non-production mode):\n{}", 
            result.format_report());
    }
    
    info!(
        all_passed = %result.all_passed,
        feeds_validated = %result.feed_results.len(),
        validation_time_ms = %result.validation_time_ms,
        "Oracle validation completed"
    );
    
    Ok(result)
}

// =============================================================================
// TESTS
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_feed_validation_result_creation() {
        let feed_config = OracleFeedConfig {
            asset_symbol: "BTC".to_string(),
            chain_id: 137,
            feed_proxy_address: "0xc907E116054Ad103354f2D350FD2514433D57F6f".to_string(),
            decimals: 8,
            expected_description: Some("BTC / USD".to_string()),
            deviation_threshold: None,
            heartbeat_secs: None,
        };
        
        let result = FeedValidationResult::new(&feed_config);
        assert!(!result.passed); // Default is false until validated
        assert_eq!(result.configured_decimals, 8);
        assert_eq!(result.configured_chain_id, 137);
    }

    #[test]
    fn test_validation_result_errors() {
        let feed_config = OracleFeedConfig {
            asset_symbol: "BTC".to_string(),
            chain_id: 137,
            feed_proxy_address: "0xc907E116054Ad103354f2D350FD2514433D57F6f".to_string(),
            decimals: 8,
            expected_description: None,
            deviation_threshold: None,
            heartbeat_secs: None,
        };
        
        let mut result = FeedValidationResult::new(&feed_config);
        result.passed = true;
        
        result.add_error("Test error");
        
        assert!(!result.passed);
        assert_eq!(result.errors.len(), 1);
    }

    #[test]
    fn test_report_formatting() {
        let result = OracleValidationResult {
            all_passed: true,
            feed_results: vec![],
            rpc_endpoint: "https://example.com".to_string(),
            validated_at: 1000,
            validation_time_ms: 500,
        };
        
        let report = result.format_report();
        assert!(report.contains("ORACLE FEED VALIDATION REPORT"));
        assert!(report.contains("PASS"));
    }
}

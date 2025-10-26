use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Signal types
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignalType {
    InsiderEdge,
    Arbitrage,
    WhaleCluster,
    PriceDeviation,
    ExpiryEdge,
}

impl SignalType {
    pub fn as_str(&self) -> &str {
        match self {
            SignalType::InsiderEdge => "insider_edge",
            SignalType::Arbitrage => "arbitrage",
            SignalType::WhaleCluster => "whale_cluster",
            SignalType::PriceDeviation => "price_deviation",
            SignalType::ExpiryEdge => "expiry_edge",
        }
    }
}

/// A trading signal
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Signal {
    pub id: Option<i64>,
    pub signal_type: SignalType,
    pub source: String,
    pub market_name: Option<String>,
    pub description: String,
    pub confidence: f32,
    pub metadata: Option<String>, // JSON string
    pub created_at: DateTime<Utc>,
}

impl Signal {
    pub fn new(
        signal_type: SignalType,
        source: String,
        description: String,
        confidence: f32,
    ) -> Self {
        Self {
            id: None,
            signal_type,
            source,
            market_name: None,
            description,
            confidence,
            metadata: None,
            created_at: Utc::now(),
        }
    }

    pub fn with_market(mut self, market_name: String) -> Self {
        self.market_name = Some(market_name);
        self
    }

    pub fn with_metadata(mut self, metadata: String) -> Self {
        self.metadata = Some(metadata);
        self
    }
}

/// Application configuration
#[derive(Debug, Clone)]
pub struct Config {
    pub database_path: String,
    pub port: u16,
    pub twitter_accounts: Vec<String>,
    pub twitter_scrape_interval: u64,
    pub hashdive_scrape_interval: u64,
    pub hashdive_api_key: Option<String>,
    pub hashdive_whale_min_usd: u32,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        dotenv::dotenv().ok();

        let database_path = std::env::var("DATABASE_PATH")
            .unwrap_or_else(|_| "./betterbot.db".to_string());

        let port = std::env::var("PORT")
            .unwrap_or_else(|_| "8080".to_string())
            .parse()
            .unwrap_or(8080);

        let twitter_accounts = std::env::var("TWITTER_ACCOUNTS")
            .unwrap_or_else(|_| {
                "PolyWhaleWatch,PolymarketWhale,PolyInsider_,PloyPulseBot,PredWhales,polyalerthub"
                    .to_string()
            })
            .split(',')
            .map(|s| s.trim().to_string())
            .collect();

        let twitter_scrape_interval = std::env::var("TWITTER_SCRAPE_INTERVAL")
            .unwrap_or_else(|_| "30".to_string())
            .parse()
            .unwrap_or(30);

        let hashdive_scrape_interval = std::env::var("HASHDIVE_SCRAPE_INTERVAL")
            .unwrap_or_else(|_| "120".to_string())
            .parse()
            .unwrap_or(120);

        let hashdive_api_key = std::env::var("HASHDIVE_API_KEY").ok();

        let hashdive_whale_min_usd = std::env::var("HASHDIVE_WHALE_MIN_USD")
            .unwrap_or_else(|_| "10000".to_string())
            .parse()
            .unwrap_or(10000);

        Ok(Self {
            database_path,
            port,
            twitter_accounts,
            twitter_scrape_interval,
            hashdive_scrape_interval,
            hashdive_api_key,
            hashdive_whale_min_usd,
        })
    }
}

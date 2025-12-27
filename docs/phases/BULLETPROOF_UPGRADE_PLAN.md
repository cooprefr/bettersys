# BULLETPROOF BETTERBOT UPGRADE PLAN
## Wintermute × Renaissance Technologies Level Engineering

**Mission**: Transform BetterBot from operational prototype → production-grade quantitative trading system  
**Philosophy**: Speed of light execution, zero missed signals, institutional-grade risk management  
**Timeline**: 25-30 hours phased execution  
**Success Metrics**: 0 panics, <100ms signal latency, >90% test coverage, Kelly-optimal position sizing

---

## CURRENT STATE ANALYSIS

### ✅ What's Working
- **Core Scrapers**: Polymarket CLOB/GAMMA, Hashdive whale tracking, Dome REST all functional with retry logic
- **Risk Management**: Kelly criterion + VaR/CVaR implemented (partial_cmp fixed)
- **Architecture**: Axum async server, Rayon parallel processing, WebSocket broadcast infrastructure
- **Wallet Tracking**: 45 elite wallets (16 insider_sports, 5 insider_politics, 3 insider_other, 21 world_class) with rotation strategy
- **Rate Limiting**: Smart backoff, Dome 1s intervals (480-500 calls/mo budget @45min rotation optimal)

### ⚠️ Critical Gaps
- **~70 unwraps/expects**: Panic risks in production (main.rs, scrapers, detector)
- **In-memory storage**: No persistence → signals lost on restart
- **Polling inefficiency**: REST-only; Dome WebSocket unused (unlimited real-time vs 500/mo REST)
- **No arbitrage detector**: Cross-platform spread analysis stubbed
- **No authentication**: API/WebSocket exposed without $BETTER token gate
- **Genetic optimizer**: Strategy optimization incomplete
- **114 warnings**: Unused imports/dead code bloat
- **Backup pollution**: .old/.bak/.backup files scattered

---

## PHASE 1: CRITICAL INFRASTRUCTURE FIXES (3-4h)
**Priority**: URGENT - Production Stability  
**Goal**: Zero panics, clean build, stable foundation

### 1.1 Error Handling Hardening (1.5h)
**Files**: `main.rs`, `scrapers/*.rs`, `signals/detector.rs`, `risk.rs`

```rust
// BEFORE (panic risk):
let value = env::var("KEY").unwrap();
let parsed = serde_json::from_str(&json).unwrap();

// AFTER (bulletproof):
let value = env::var("KEY")
    .context("Missing KEY environment variable")?;
let parsed: Config = serde_json::from_str(&json)
    .with_context(|| format!("Failed to parse config: {}", json))?;
```

**Specific Fixes**:
1. `main.rs`:
   - Lines 65-72: env var parsing → `?` operator with context
   - Line 122: `expect("Failed to create HTTP client")` → `context()`
   - Line 187: `serde_json::to_string(&signal).unwrap()` → `.unwrap_or_else()`
   
2. `scrapers/dome.rs`:
   - Line 75: Client builder expect → `?`
   - Lines 103, 148, 192: JSON parsing unwrap → `context()`
   
3. `signals/detector.rs`:
   - Line 135: `volume.unwrap_or(0.0)` already safe, verify all instances
   
4. `scrapers/hashdive_api.rs`:
   - Line 56: Client builder expect → `?`
   - Line 441: timestamps min/max unwrap → `unwrap_or(&0)` ✅ (already safe)

**Verification**:
```bash
cargo clippy --all-targets -- -D warnings
cargo check 2>&1 | grep -i "unwrap\|expect\|panic"
```

**Success Metrics**: 0 unwraps/expects in production paths, <10 warnings

---

### 1.2 Dead Code Cleanup (1h)
**Files**: All `src/**/*.rs`

**Actions**:
1. Remove backup files:
```bash
find rust-backend/src -name "*.old" -o -name "*.bak" -o -name "*.backup" | xargs rm
```

2. Prune unused imports via clippy:
```bash
cargo clippy --fix --allow-dirty --allow-staged
```

3. Manual cleanup:
   - Remove unused Arc/RwLock imports in files using simple types
   - Remove unused Json/post from api routes (if GET-only)
   - Verify all 8 SignalType variants are generated

**Verification**: `cargo build --release` completes with <20 warnings

---

### 1.3 Metrics & Observability (0.5h)
**Files**: `main.rs`, `Cargo.toml`

**Add Prometheus metrics**:
```rust
use metrics::{counter, gauge, histogram};

// In scrapers:
counter!("signals_detected_total", 1, "source" => "polymarket");
histogram!("api_request_duration_seconds", elapsed.as_secs_f64());

// In risk module:
gauge!("bankroll_current", risk_manager.kelly.bankroll);
gauge!("var_95", var_stats.var_95);
```

**Endpoint**:
```rust
.route("/metrics", get(metrics_handler))
```

**Success Metrics**: Grafana-ready metrics on `:3000/metrics`

---

## PHASE 2: DATABASE PERSISTENCE LAYER (2-3h)
**Priority**: HIGH - Data durability  
**Goal**: SQLite-backed signal storage, 10k signal retention

### 2.1 Schema Design (0.5h)
**File**: `rust-backend/src/signals/db_storage.rs` (new)

```sql
CREATE TABLE IF NOT EXISTS signals (
    id TEXT PRIMARY KEY,
    signal_type TEXT NOT NULL,
    market_slug TEXT NOT NULL,
    confidence REAL NOT NULL,
    risk_level TEXT NOT NULL,
    details_json TEXT NOT NULL,
    detected_at TEXT NOT NULL,
    source TEXT NOT NULL,
    created_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now'))
);

CREATE INDEX idx_signals_detected_at ON signals(detected_at DESC);
CREATE INDEX idx_signals_confidence ON signals(confidence DESC);
CREATE INDEX idx_signals_source ON signals(source);

-- Auto-cleanup trigger (keep last 10k)
CREATE TRIGGER IF NOT EXISTS cleanup_old_signals
AFTER INSERT ON signals
BEGIN
    DELETE FROM signals WHERE id IN (
        SELECT id FROM signals 
        ORDER BY detected_at DESC 
        LIMIT -1 OFFSET 10000
    );
END;
```

### 2.2 Storage Implementation (1.5h)
**File**: `rust-backend/src/signals/db_storage.rs`

```rust
use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use std::sync::{Arc, Mutex};
use crate::models::MarketSignal;

pub struct DbSignalStorage {
    conn: Arc<Mutex<Connection>>,
}

impl DbSignalStorage {
    pub fn new(db_path: &str) -> Result<Self> {
        let conn = Connection::open(db_path)
            .context("Failed to open database")?;
        
        // Initialize schema
        conn.execute_batch(SCHEMA_SQL)?;
        
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }
    
    pub fn store(&self, signal: &MarketSignal) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let details_json = serde_json::to_string(&signal.details)?;
        let signal_type_json = serde_json::to_string(&signal.signal_type)?;
        
        conn.execute(
            "INSERT OR REPLACE INTO signals 
             (id, signal_type, market_slug, confidence, risk_level, 
              details_json, detected_at, source) 
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                &signal.id,
                signal_type_json,
                &signal.market_slug,
                signal.confidence,
                &signal.risk_level,
                details_json,
                &signal.detected_at,
                &signal.source,
            ],
        )?;
        
        Ok(())
    }
    
    pub fn get_recent(&self, limit: usize) -> Result<Vec<MarketSignal>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, signal_type, market_slug, confidence, risk_level, 
                    details_json, detected_at, source
             FROM signals 
             ORDER BY detected_at DESC 
             LIMIT ?1"
        )?;
        
        let signals = stmt.query_map([limit], |row| {
            Ok(MarketSignal {
                id: row.get(0)?,
                signal_type: serde_json::from_str(&row.get::<_, String>(1)?).unwrap(),
                market_slug: row.get(2)?,
                confidence: row.get(3)?,
                risk_level: row.get(4)?,
                details: serde_json::from_str(&row.get::<_, String>(5)?).unwrap(),
                detected_at: row.get(6)?,
                source: row.get(7)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
        
        Ok(signals)
    }
}
```

### 2.3 Integration (1h)
**Files**: `main.rs`, `signals/storage.rs`

1. Replace `SignalStorage` with `DbSignalStorage` in `AppState`
2. Add DB path config: `env::var("DB_PATH").unwrap_or("betterbot.db".to_string())`
3. Add in-memory fallback for tests:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    
    fn test_storage() -> DbSignalStorage {
        DbSignalStorage::new(":memory:").unwrap()
    }
}
```

**Verification**:
```bash
cargo test storage
# Check DB file exists after run
ls -lh betterbot.db
sqlite3 betterbot.db "SELECT COUNT(*) FROM signals;"
```

**Success Metrics**: 10k signals retained, <10ms insert latency

---

## PHASE 3: DOME WEBSOCKET REAL-TIME ENGINE (3-4h)
**Priority**: HIGH - Eliminates polling inefficiency  
**Goal**: Real-time order stream from 45 wallets, <100ms latency

### 3.1 WebSocket Client Implementation (2h)
**File**: `rust-backend/src/scrapers/dome_ws.rs` (new)

```rust
use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{error, info, warn};

const DOME_WS_URL: &str = "wss://ws.domeapi.io";

#[derive(Debug, Serialize)]
struct SubscribeMessage {
    action: String,
    platform: String,
    version: u32,
    #[serde(rename = "type")]
    msg_type: String,
    filters: Filters,
}

#[derive(Debug, Serialize)]
struct Filters {
    users: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum WsResponse {
    #[serde(rename = "ack")]
    Ack { subscription_id: String },
    #[serde(rename = "event")]
    Event { subscription_id: String, data: OrderEvent },
}

#[derive(Debug, Clone, Deserialize)]
pub struct OrderEvent {
    pub token_id: String,
    pub side: String,
    pub market_slug: String,
    pub condition_id: String,
    pub shares: u64,
    pub shares_normalized: f64,
    pub price: f64,
    pub tx_hash: String,
    pub title: String,
    pub timestamp: i64,
    pub order_hash: String,
    pub user: String,
}

pub struct DomeWebSocketClient {
    api_key: String,
    wallet_addresses: Vec<String>,
}

impl DomeWebSocketClient {
    pub fn new(api_key: String, wallet_addresses: Vec<String>) -> Self {
        Self { api_key, wallet_addresses }
    }
    
    pub async fn connect_and_stream(
        &self,
    ) -> Result<mpsc::Receiver<OrderEvent>> {
        let url = format!("{}/{}", DOME_WS_URL, self.api_key);
        let (ws_stream, _) = connect_async(&url)
            .await
            .context("Failed to connect to Dome WebSocket")?;
        
        info!("🔌 Connected to Dome WebSocket");
        
        let (mut write, mut read) = ws_stream.split();
        
        // Subscribe to all tracked wallets
        let subscribe_msg = SubscribeMessage {
            action: "subscribe".to_string(),
            platform: "polymarket".to_string(),
            version: 1,
            msg_type: "orders".to_string(),
            filters: Filters {
                users: self.wallet_addresses.clone(),
            },
        };
        
        let msg_json = serde_json::to_string(&subscribe_msg)?;
        write.send(Message::Text(msg_json)).await?;
        info!("📡 Subscribed to {} wallets", self.wallet_addresses.len());
        
        // Channel for forwarding events
        let (tx, rx) = mpsc::channel::<OrderEvent>(1000);
        
        // Spawn reader task
        tokio::spawn(async move {
            while let Some(msg) = read.next().await {
                match msg {
                    Ok(Message::Text(text)) => {
                        match serde_json::from_str::<WsResponse>(&text) {
                            Ok(WsResponse::Ack { subscription_id }) => {
                                info!("✅ Subscription confirmed: {}", subscription_id);
                            }
                            Ok(WsResponse::Event { data, .. }) => {
                                // Filter BUY orders only
                                if data.side.to_uppercase() == "BUY" {
                                    info!(
                                        "🔔 Order: {} ${:.0} on {} by {}",
                                        data.side,
                                        data.shares_normalized * data.price,
                                        data.market_slug,
                                        &data.user[..10]
                                    );
                                    let _ = tx.send(data).await;
                                }
                            }
                            Err(e) => {
                                warn!("Failed to parse WS message: {}", e);
                            }
                        }
                    }
                    Ok(Message::Close(_)) => {
                        error!("WebSocket closed by server");
                        break;
                    }
                    Err(e) => {
                        error!("WebSocket error: {}", e);
                        break;
                    }
                    _ => {}
                }
            }
        });
        
        Ok(rx)
    }
}
```

### 3.2 Integration with Main Loop (1h)
**File**: `main.rs`

Replace `tracked_wallet_polling` with:

```rust
async fn dome_websocket_handler(
    storage: Arc<RwLock<DbSignalStorage>>,
    signal_tx: broadcast::Sender<MarketSignal>,
) -> Result<()> {
    let config = Config::from_env();
    
    let Some(api_key) = config.dome_api_key else {
        info!("Dome WebSocket disabled - no API key");
        return Ok(());
    };
    
    let wallet_addresses: Vec<String> = config
        .tracked_wallets
        .keys()
        .cloned()
        .collect();
    
    let ws_client = DomeWebSocketClient::new(api_key, wallet_addresses.clone());
    let mut order_stream = ws_client.connect_and_stream().await?;
    
    let detector = SignalDetector::new();
    
    info!("🚀 Dome WebSocket active - tracking {} wallets in real-time", 
          wallet_addresses.len());
    
    while let Some(order) = order_stream.recv().await {
        // Look up wallet label
        let wallet_label = config.tracked_wallets
            .get(&order.user)
            .cloned()
            .unwrap_or_else(|| "unknown".to_string());
        
        // Convert to DomeOrder format
        let dome_order = DomeOrder {
            token_id: order.token_id,
            side: order.side,
            shares_normalized: order.shares_normalized,
            price: order.price,
            timestamp: order.timestamp,
            market_slug: order.market_slug,
            title: order.title,
            user: order.user.clone(),
        };
        
        // Detect signals
        let signals = detector.detect_trader_entry(
            &[dome_order],
            &order.user,
            &wallet_label,
        );
        
        for signal in signals {
            storage.write().await.store(&signal)?;
            let _ = signal_tx.send(signal);
        }
    }
    
    Ok(())
}
```

### 3.3 Reconnection & Error Handling (1h)

Add retry logic with exponential backoff:

```rust
pub async fn run_with_reconnect(
    &self,
    max_retries: u32,
) -> Result<mpsc::Receiver<OrderEvent>> {
    let mut backoff = Duration::from_secs(1);
    
    for attempt in 0..max_retries {
        match self.connect_and_stream().await {
            Ok(rx) => return Ok(rx),
            Err(e) => {
                error!("Connection attempt {} failed: {}", attempt + 1, e);
                if attempt < max_retries - 1 {
                    info!("Reconnecting in {:?}", backoff);
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(Duration::from_secs(300));
                }
            }
        }
    }
    
    Err(anyhow::anyhow!("Max reconnection attempts exceeded"))
}
```

**Verification**:
```bash
# Set DOME_API_KEY in .env
cargo run
# Should see "Subscribed to 45 wallets" in logs
# Monitor for real-time order events
```

**Success Metrics**:
- Real-time orders (<1s latency from blockchain)
- Zero polling overhead
- Auto-reconnect on disconnect
- BUY-only filtering working

---

## PHASE 4: ARBITRAGE DETECTION SYSTEM (4-5h)
**Priority**: HIGH - Alpha generation  
**Goal**: Real-time cross-platform spread detection, >2% threshold

### 4.1 Market Matching Engine (2h)
**File**: `rust-backend/src/arbitrage/matcher.rs` (new)

```rust
use anyhow::Result;
use crate::scrapers::dome::{DomeScraper, DomeMarketPrice};
use crate::scrapers::polymarket_api::PolymarketScraper;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tracing::{info, warn};

#[derive(Debug, Clone, Serialize)]
pub struct ArbitrageOpportunity {
    pub market_slug: String,
    pub condition_id: String,
    pub polymarket_price: f64,
    pub dome_price: f64,
    pub spread_pct: f64,
    pub estimated_profit: f64,
    pub liquidity: f64,
    pub confidence: f64,
    pub detected_at: String,
}

pub struct ArbitrageMatcher {
    dome_scraper: DomeScraper,
    poly_scraper: PolymarketScraper,
    min_spread_pct: f64,
    min_liquidity: f64,
}

impl ArbitrageMatcher {
    pub fn new(
        dome_api_key: String,
        min_spread_pct: f64,
        min_liquidity: f64,
    ) -> Self {
        Self {
            dome_scraper: DomeScraper::new(dome_api_key),
            poly_scraper: PolymarketScraper::new(),
            min_spread_pct,
            min_liquidity,
        }
    }
    
    pub async fn scan_opportunities(&mut self) -> Result<Vec<ArbitrageOpportunity>> {
        info!("🔍 Scanning for arbitrage opportunities...");
        
        // 1. Fetch active markets from Polymarket
        let poly_markets = self.poly_scraper
            .fetch_gamma_markets(100, 0)
            .await?;
        
        let mut opportunities = Vec::new();
        
        // 2. For each market, fetch Dome price
        for market in poly_markets.markets {
            // Get first token ID (YES outcome)
            let Some(token_id) = market.tokens.first() else {
                continue;
            };
            
            // Fetch Dome price
            let dome_price = match self.dome_scraper
                .get_market_price(&token_id.token_id)
                .await
            {
                Ok(price) => price,
                Err(e) => {
                    warn!("Failed to fetch Dome price for {}: {}", market.slug, e);
                    continue;
                }
            };
            
            // Calculate spread
            let poly_price = token_id.price;
            let spread = (dome_price.price - poly_price).abs();
            let spread_pct = spread / poly_price;
            
            // Check thresholds
            if spread_pct < self.min_spread_pct {
                continue;
            }
            
            if dome_price.liquidity < self.min_liquidity {
                continue;
            }
            
            // Estimate profit (conservative: 50% of spread due to slippage)
            let trade_size = dome_price.liquidity.min(10000.0);
            let estimated_profit = trade_size * spread_pct * 0.5;
            
            // Confidence score based on liquidity and spread
            let liquidity_score = (dome_price.liquidity / 100000.0).min(1.0);
            let spread_score = (spread_pct / 0.05).min(1.0);
            let confidence = (liquidity_score + spread_score) / 2.0;
            
            opportunities.push(ArbitrageOpportunity {
                market_slug: market.slug.clone(),
                condition_id: market.condition_id.clone(),
                polymarket_price: poly_price,
                dome_price: dome_price.price,
                spread_pct,
                estimated_profit,
                liquidity: dome_price.liquidity,
                confidence,
                detected_at: chrono::Utc::now().to_rfc3339(),
            });
            
            info!(
                "💎 Arbitrage: {} - {:.2}% spread, ${:.2} profit",
                market.slug, spread_pct * 100.0, estimated_profit
            );
        }
        
        info!("✅ Found {} arbitrage opportunities", opportunities.len());
        Ok(opportunities)
    }
}
```

### 4.2 Signal Generation Integration (1h)
**File**: `signals/detector.rs`

Add method:

```rust
pub fn detect_arbitrage(
    &self,
    opportunity: &ArbitrageOpportunity,
) -> MarketSignal {
    let action = if opportunity.polymarket_price < opportunity.dome_price {
        "BUY_POLY_SELL_DOME"
    } else {
        "BUY_DOME_SELL_POLY"
    };
    
    MarketSignal {
        id: format!("arb_{}", opportunity.condition_id),
        signal_type: SignalType::CrossPlatformArbitrage {
            polymarket_price: opportunity.polymarket_price,
            kalshi_price: Some(opportunity.dome_price),
            spread_pct: opportunity.spread_pct,
        },
        market_slug: opportunity.market_slug.clone(),
        confidence: opportunity.confidence,
        risk_level: if opportunity.spread_pct > 0.05 {
            "low"
        } else {
            "medium"
        }.to_string(),
        details: SignalDetails {
            market_id: opportunity.condition_id.clone(),
            market_title: format!(
                "Arbitrage: {:.2}% spread | Est. ${:.2} profit",
                opportunity.spread_pct * 100.0,
                opportunity.estimated_profit
            ),
            current_price: opportunity.polymarket_price,
            volume_24h: 0.0,
            liquidity: opportunity.liquidity,
            recommended_action: action.to_string(),
            expiry_time: None,
        },
        detected_at: opportunity.detected_at.clone(),
        source: "arbitrage".to_string(),
    }
}
```

### 4.3 Main Loop Integration (1h)
**File**: `main.rs`

Add periodic arbitrage scanner:

```rust
tokio::spawn(arbitrage_scanner(
    signal_storage.clone(),
    signal_tx.clone(),
    config.dome_api_key.clone().unwrap_or_default(),
));

async fn arbitrage_scanner(
    storage: Arc<RwLock<DbSignalStorage>>,
    signal_tx: broadcast::Sender<MarketSignal>,
    dome_api_key: String,
) -> Result<()> {
    if dome_api_key.is_empty() {
        return Ok(());
    }
    
    let mut matcher = ArbitrageMatcher::new(
        dome_api_key,
        0.02, // 2% minimum spread
        5000.0, // $5k minimum liquidity
    );
    
    let detector = SignalDetector::new();
    let mut interval = tokio::time::interval(Duration::from_secs(120)); // 2min scan
    
    loop {
        interval.tick().await;
        
        match matcher.scan_opportunities().await {
            Ok(opportunities) => {
                for opp in opportunities {
                    let signal = detector.detect_arbitrage(&opp);
                    storage.write().await.store(&signal)?;
                    let _ = signal_tx.send(signal);
                }
            }
            Err(e) => {
                warn!("Arbitrage scan failed: {}", e);
            }
        }
    }
}
```

**Verification**:
```bash
cargo test arbitrage_matcher
# Run live: should find 0-5 opportunities per scan
```

**Success Metrics**: >2% spreads detected, <5s scan time

---

## PHASE 5: ELITE/INSIDER WALLET CLASSIFICATION (2h)
**Priority**: MEDIUM - Signal quality  
**Goal**: Auto-classify wallets by performance metrics

### 5.1 Classification Engine Enhancement
**File**: `scrapers/hashdive_api.rs` (already implemented!)

Current implementation:
- ✅ Elite: >$100k volume + >65% win rate
- ✅ Insider: >70% win rate + >75% early entry score
- ✅ Whale: >$50k volume

### 5.2 Integration with Signal Generation (1h)
**File**: `signals/detector.rs`

Enhance `detect_trader_entry`:

```rust
pub fn detect_trader_entry_with_classification(
    &self,
    orders: &[DomeOrder],
    wallet_address: &str,
    wallet_label: &str,
    classification: WalletClassification, // NEW
) -> Vec<MarketSignal> {
    let mut signals = Vec::new();
    
    for order in orders {
        let position_value = order.shares_normalized * order.price;
        
        if position_value < 1000.0 {
            continue;
        }
        
        // Boost confidence based on classification
        let base_confidence = 0.85;
        let classification_boost = match classification {
            WalletClassification::Elite { win_rate, .. } => {
                (win_rate - 0.65) * 0.5 // Up to +0.175 for 100% win rate
            }
            WalletClassification::Insider { win_rate, early_entry_score, .. } => {
                (win_rate - 0.70) * 0.3 + (early_entry_score - 0.75) * 0.2
            }
            WalletClassification::Whale { .. } => 0.05,
            WalletClassification::Regular => 0.0,
        };
        
        let confidence = (base_confidence + classification_boost).min(0.99);
        
        // Enhanced description with classification
        let description = match classification {
            WalletClassification::Elite { win_rate, total_volume, .. } => {
                format!(
                    "ELITE TRADER [{:.0}% WR, ${:.0}k Vol]: ${:.0} BUY on '{}' by {} @ {:.3}",
                    win_rate * 100.0,
                    total_volume / 1000.0,
                    position_value,
                    order.title,
                    &wallet_address[..10],
                    order.price
                )
            }
            WalletClassification::Insider { win_rate, early_entry_score, .. } => {
                format!(
                    "INSIDER [{:.0}% WR, {:.0}% Early]: ${:.0} BUY on '{}' by {} @ {:.3}",
                    win_rate * 100.0,
                    early_entry_score * 100.0,
                    position_value,
                    order.title,
                    &wallet_address[..10],
                    order.price
                )
            }
            _ => format!(
                "TRACKED WALLET: ${:.0} BUY on '{}' by {}",
                position_value,
                order.title,
                &wallet_address[..10]
            ),
        };
        
        signals.push(MarketSignal {
            // ... (rest of signal generation with enhanced confidence/description)
        });
    }
    
    signals
}
```

### 5.3 Periodic Classification Updates (1h)
**File**: `main.rs`

Add background task:

```rust
tokio::spawn(wallet_classifier_task(config.clone()));

async fn wallet_classifier_task(config: Config) -> Result<()> {
    let Some(hashdive_key) = std::env::var("HASHDIVE_API_KEY").ok() else {
        return Ok(());
    };
    
    let mut scraper = HashdiveScraper::new(hashdive_key);
    let mut interval = tokio::time::interval(Duration::from_secs(86400)); // Daily
    
    loop {
        interval.tick().await;
        
        for (wallet_addr, label) in &config.tracked_wallets {
            match scraper.get_trades(wallet_addr, None, None, None, None, Some(100)).await {
                Ok(trades_response) => {
                    let classification = scraper.classify_wallet(&trades_response.data);
                    info!("Wallet {} classified as: {:?}", &wallet_addr[..10], classification);
                    // TODO: Store classification in DB for lookup
                }
                Err(e) => {
                    warn!("Failed to fetch trades for {}: {}", wallet_addr, e);
                }
            }
            
            tokio::time::sleep(Duration::from_secs(2)).await; // Rate limit
        }
    }
}
```

**Success Metrics**: All 45 wallets classified, confidence boosts working

---

## PHASE 6: GENETIC ALGORITHM OPTIMIZER (5h)
**Priority**: MEDIUM - Strategy optimization  
**Goal**: Auto-tune thresholds via genetic algorithm

### 6.1 Strategy Parameters (1h)
**File**: `backtest/optimizer.rs` (new)

```rust
use rand::Rng;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyParams {
    pub min_confidence: f64,           // 0.5 - 0.95
    pub min_arbitrage_spread_pct: f64, // 0.01 - 0.10
    pub kelly_fraction: f64,           // 0.1 - 0.5
    pub max_position_pct: f64,         // 0.05 - 0.25
    pub stop_loss_pct: f64,            // 0.02 - 0.15
    pub take_profit_pct: f64,          // 0.05 - 0.30
}

impl StrategyParams {
    pub fn random() -> Self {
        let mut rng = rand::thread_rng();
        Self {
            min_confidence: rng.gen_range(0.5..0.95),
            min_arbitrage_spread_pct: rng.gen_range(0.01..0.10),
            kelly_fraction: rng.gen_range(0.1..0.5),
            max_position_pct: rng.gen_range(0.05..0.25),
            stop_loss_pct: rng.gen_range(0.02..0.15),
            take_profit_pct: rng.gen_range(0.05..0.30),
        }
    }
    
    pub fn mutate(&mut self, mutation_rate: f64) {
        let mut rng = rand::thread_rng();
        
        if rng.gen::<f64>() < mutation_rate {
            self.min_confidence += rng.gen_range(-0.05..0.05);
            self.min_confidence = self.min_confidence.clamp(0.5, 0.95);
        }
        
        if rng.gen::<f64>() < mutation_rate {
            self.min_arbitrage_spread_pct += rng.gen_range(-0.01..0.01);
            self.min_arbitrage_spread_pct = self.min_arbitrage_spread_pct.clamp(0.01, 0.10);
        }
        
        // ... (similar for other params)
    }
    
    pub fn crossover(&self, other: &Self) -> Self {
        let mut rng = rand::thread_rng();
        
        Self {
            min_confidence: if rng.gen() { self.min_confidence } else { other.min_confidence },
            min_arbitrage_spread_pct: if rng.gen() { 
                self.min_arbitrage_spread_pct 
            } else { 
                other.min_arbitrage_spread_pct 
            },
            kelly_fraction: if rng.gen() { self.kelly_fraction } else { other.kelly_fraction },
            max_position_pct: if rng.gen() { self.max_position_pct } else { other.max_position_pct },
            stop_loss_pct: if rng.gen() { self.stop_loss_pct } else { other.stop_loss_pct },
            take_profit_pct: if rng.gen() { self.take_profit_pct } else { other.take_profit_pct },
        }
    }
}
```

### 6.2 Fitness Function (2h)
**File**: `backtest/optimizer.rs`

```rust
pub struct StrategyOptimizer {
    population_size: usize,
    num_generations: usize,
    mutation_rate: f64,
}

impl StrategyOptimizer {
    pub fn new(population_size: usize, num_generations: usize, mutation_rate: f64) -> Self {
        Self {
            population_size,
            num_generations,
            mutation_rate,
        }
    }
    
    pub fn optimize(
        &self,
        historical_signals: Vec<MarketSignal>,
        initial_bankroll: f64,
    ) -> Result<(StrategyParams, f64)> {
        info!("🧬 Starting genetic optimization: {} generations, pop {}", 
              self.num_generations, self.population_size);
        
        // Initialize population
        let mut population: Vec<(StrategyParams, f64)> = (0..self.population_size)
            .map(|_| {
                let params = StrategyParams::random();
                let fitness = self.evaluate_fitness(&params, &historical_signals, initial_bankroll);
                (params, fitness)
            })
            .collect();
        
        // Evolution loop
        for generation in 0..self.num_generations {
            // Sort by fitness (descending)
            population.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
            
            let best_fitness = population[0].1;
            info!("Generation {}: Best Sharpe = {:.3}", generation, best_fitness);
            
            // Elitism: keep top 20%
            let elite_count = self.population_size / 5;
            let mut next_generation = population[..elite_count].to_vec();
            
            // Crossover and mutation
            while next_generation.len() < self.population_size {
                let parent1 = &population[rand::thread_rng().gen_range(0..elite_count)].0;
                let parent2 = &population[rand::thread_rng().gen_range(0..elite_count)].0;
                
                let mut child = parent1.crossover(parent2);
                child.mutate(self.mutation_rate);
                
                let fitness = self.evaluate_fitness(&child, &historical_signals, initial_bankroll);
                next_generation.push((child, fitness));
            }
            
            population = next_generation;
        }
        
        population.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
        let (best_params, best_fitness) = &population[0];
        
        info!("✅ Optimization complete: Sharpe {:.3}", best_fitness);
        Ok((best_params.clone(), *best_fitness))
    }
    
    fn evaluate_fitness(
        &self,
        params: &StrategyParams,
        signals: &[MarketSignal],
        initial_bankroll: f64,
    ) -> f64 {
        let mut engine = BacktestEngine::new(BacktestConfig {
            initial_bankroll,
            kelly_fraction: params.kelly_fraction,
            start_date: chrono::Utc::now() - chrono::Duration::days(30),
            end_date: chrono::Utc::now(),
            slippage_bps: 10.0,
            transaction_cost: 0.01,
            max_positions: 10,
        });
        
        // Filter signals by params
        let filtered_signals: Vec<MarketSignal> = signals
            .iter()
            .filter(|s| s.confidence >= params.min_confidence)
            .cloned()
            .collect();
        
        match tokio::runtime::Runtime::new().unwrap().block_on(engine.run(filtered_signals)) {
            Ok(result) => {
                // Multi-objective fitness: Sharpe + penalty for drawdown
                let sharpe_score = result.sharpe_ratio;
                let drawdown_penalty = (result.max_drawdown / 0.20).min(1.0); // Penalize >20% DD
                let profit_bonus = (result.total_pnl / initial_bankroll).max(0.0);
                
                sharpe_score - drawdown_penalty + (profit_bonus * 0.1)
            }
            Err(_) => -999.0, // Penalty for invalid strategy
        }
    }
}
```

### 6.3 API Endpoint (1h)
**File**: `api/routes.rs`

```rust
#[derive(Debug, Deserialize)]
pub struct OptimizeRequest {
    pub population_size: usize,
    pub num_generations: usize,
    pub mutation_rate: f64,
    pub initial_bankroll: f64,
}

pub async fn optimize_strategy_handler(
    Json(request): Json<OptimizeRequest>,
    AxumState(state): AxumState<AppState>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let storage = state.signal_storage.read().await;
    let signals = storage.get_recent(10000)?;
    
    let optimizer = StrategyOptimizer::new(
        request.population_size,
        request.num_generations,
        request.mutation_rate,
    );
    
    let (best_params, best_fitness) = optimizer
        .optimize(signals, request.initial_bankroll)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    
    Ok(Json(serde_json::json!({
        "best_params": best_params,
        "sharpe_ratio": best_fitness,
    })))
}
```

**Verification**:
```bash
curl -X POST http://localhost:3000/api/optimize \
  -H "Content-Type: application/json" \
  -d '{"population_size": 50, "num_generations": 20, "mutation_rate": 0.1, "initial_bankroll": 10000}'
```

**Success Metrics**: Finds params with Sharpe >1.5, <20% drawdown in <5min

---

## PHASE 7: AUTHENTICATION & API SECURITY (3h)
**Priority**: HIGH - Production security  
**Goal**: JWT auth with $BETTER token, rate limiting

### 7.1 JWT Middleware (1.5h)
**File**: `api/auth.rs` (new)

```rust
use axum::{
    extract::{Request, State},
    http::{HeaderMap, StatusCode},
    middleware::Next,
    response::Response,
};
use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Debug, Serialize, Deserialize)]
pub struct Claims {
    pub sub: String,  // User ID
    pub exp: usize,   // Expiration
    pub tier: String, // "free", "pro", "elite"
}

pub struct AuthConfig {
    pub jwt_secret: String,
}

pub async fn auth_middleware(
    State(config): State<Arc<AuthConfig>>,
    headers: HeaderMap,
    request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let auth_header = headers
        .get("Authorization")
        .and_then(|v| v.to_str().ok())
        .ok_or(StatusCode::UNAUTHORIZED)?;
    
    if !auth_header.starts_with("Bearer ") {
        return Err(StatusCode::UNAUTHORIZED);
    }
    
    let token = &auth_header[7..];
    
    let claims = decode::<Claims>(
        token,
        &DecodingKey::from_secret(config.jwt_secret.as_bytes()),
        &Validation::default(),
    )
    .map_err(|_| StatusCode::UNAUTHORIZED)?
    .claims;
    
    // Check expiration
    let now = chrono::Utc::now().timestamp() as usize;
    if claims.exp < now {
        return Err(StatusCode::UNAUTHORIZED);
    }
    
    Ok(next.run(request).await)
}

pub fn create_token(user_id: &str, tier: &str, secret: &str) -> Result<String, jsonwebtoken::errors::Error> {
    let expiration = chrono::Utc::now()
        .checked_add_signed(chrono::Duration::hours(24))
        .unwrap()
        .timestamp() as usize;
    
    let claims = Claims {
        sub: user_id.to_string(),
        exp: expiration,
        tier: tier.to_string(),
    };
    
    encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(secret.as_bytes()),
    )
}
```

### 7.2 Rate Limiting (1h)
**File**: `api/rate_limit.rs` (new)

```rust
use axum::{
    extract::{Request, State},
    http::StatusCode,
    middleware::Next,
    response::Response,
};
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;
use tokio::sync::Mutex;
use std::time::{Duration, Instant};

pub struct RateLimiter {
    requests: Arc<Mutex<HashMap<IpAddr, Vec<Instant>>>>,
    max_requests: usize,
    window: Duration,
}

impl RateLimiter {
    pub fn new(max_requests: usize, window: Duration) -> Self {
        Self {
            requests: Arc::new(Mutex::new(HashMap::new())),
            max_requests,
            window,
        }
    }
    
    pub async fn check(&self, ip: IpAddr) -> bool {
        let mut requests = self.requests.lock().await;
        let now = Instant::now();
        
        let timestamps = requests.entry(ip).or_insert_with(Vec::new);
        
        // Remove old requests
        timestamps.retain(|&t| now.duration_since(t) < self.window);
        
        if timestamps.len() >= self.max_requests {
            return false;
        }
        
        timestamps.push(now);
        true
    }
}

pub async fn rate_limit_middleware(
    State(limiter): State<Arc<RateLimiter>>,
    request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    // Extract IP from request
    let ip = request
        .headers()
        .get("X-Forwarded-For")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<IpAddr>().ok())
        .unwrap_or(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST));
    
    if !limiter.check(ip).await {
        return Err(StatusCode::TOO_MANY_REQUESTS);
    }
    
    Ok(next.run(request).await)
}
```

### 7.3 Main Integration (0.5h)
**File**: `main.rs`

```rust
let jwt_secret = env::var("JWT_SECRET")
    .unwrap_or_else(|_| "your-256-bit-secret-change-in-production".to_string());

let auth_config = Arc::new(AuthConfig { jwt_secret });
let rate_limiter = Arc::new(RateLimiter::new(100, Duration::from_secs(60))); // 100 req/min

let app = Router::new()
    .route("/health", get(health_check))
    .route("/api/signals", get(api::get_signals))
    .route("/api/risk/stats", get(api::get_risk_stats_handler))
    .route("/api/backtest", post(api::run_backtest_handler))
    .route("/api/optimize", post(api::optimize_strategy_handler))
    .route("/ws", get(websocket_handler))
    .layer(axum::middleware::from_fn_with_state(
        rate_limiter.clone(),
        rate_limit_middleware,
    ))
    .layer(axum::middleware::from_fn_with_state(
        auth_config.clone(),
        auth_middleware,
    ))
    .layer(CorsLayer::permissive())
    .with_state(app_state);
```

**Verification**:
```bash
# Generate token
TOKEN=$(curl -X POST http://localhost:3000/auth/token \
  -H "Content-Type: application/json" \
  -d '{"user_id": "test", "tier": "pro"}')

# Use token
curl http://localhost:3000/api/signals \
  -H "Authorization: Bearer $TOKEN"
```

**Success Metrics**: Unauthorized requests blocked, rate limits enforced

---

## PHASE 8: TESTING & QUALITY ASSURANCE (3-4h)
**Priority**: MEDIUM - Reliability  
**Goal**: >90% coverage, integration tests

### 8.1 Unit Tests (2h)

Add tests to each module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    
    #[tokio::test]
    async fn test_db_storage() {
        let storage = DbSignalStorage::new(":memory:").unwrap();
        
        let signal = MarketSignal {
            id: "test_1".to_string(),
            // ... (full signal)
        };
        
        storage.store(&signal).await.unwrap();
        let retrieved = storage.get_recent(10).await.unwrap();
        
        assert_eq!(retrieved.len(), 1);
        assert_eq!(retrieved[0].id, "test_1");
    }
    
    #[tokio::test]
    async fn test_arbitrage_detection() {
        // Mock opportunity
        let opp = ArbitrageOpportunity {
            market_slug: "test".to_string(),
            spread_pct: 0.03,
            // ...
        };
        
        let detector = SignalDetector::new();
        let signal = detector.detect_arbitrage(&opp);
        
        assert!(signal.confidence > 0.0);
        assert_eq!(signal.source, "arbitrage");
    }
    
    #[tokio::test]
    async fn test_kelly_calculation() {
        let mut risk_manager = RiskManager::new(10000.0, 0.25);
        let position = risk_manager
            .calculate_position(0.6, 0.8, 50000.0)
            .unwrap();
        
        assert!(position.position_size > 0.0);
        assert!(position.position_size < 10000.0);
    }
}
```

### 8.2 Integration Tests (1.5h)
**File**: `tests/integration_tests.rs`

```rust
#[tokio::test]
async fn test_full_signal_pipeline() {
    // 1. Generate mock signal
    let mut mock_gen = MockGenerator::new();
    let signals = mock_gen.generate_signals(5);
    
    // 2. Store in DB
    let storage = DbSignalStorage::new(":memory:").unwrap();
    for signal in &signals {
        storage.store(signal).await.unwrap();
    }
    
    // 3. Retrieve via API (mock server)
    let retrieved = storage.get_recent(10).await.unwrap();
    assert_eq!(retrieved.len(), 5);
    
    // 4. Apply risk management
    let mut risk_manager = RiskManager::new(10000.0, 0.25);
    for signal in &retrieved {
        let position = risk_manager
            .calculate_position(signal.confidence, signal.confidence, 50000.0)
            .unwrap();
        assert!(position.position_size >= 0.0);
    }
}

#[tokio::test]
async fn test_websocket_connection() {
    // Test Dome WS connection
    // (Requires live API key - mark as #[ignore] for CI)
}
```

### 8.3 Load Testing (0.5h)

```bash
# Install k6
brew install k6  # or https://k6.io/docs/getting-started/installation

# Create load test script
cat > load_test.js <<EOF
import http from 'k6/http';
import { check } from 'k6';

export let options = {
  stages: [
    { duration: '30s', target: 50 },
    { duration: '1m', target: 100 },
    { duration: '30s', target: 0 },
  ],
};

export default function () {
  let res = http.get('http://localhost:3000/api/signals?limit=10');
  check(res, {
    'status is 200': (r) => r.status === 200,
    'response time < 200ms': (r) => r.timings.duration < 200,
  });
}
EOF

# Run load test
k6 run load_test.js
```

**Verification**:
```bash
cargo test
cargo test --test integration_tests
cargo tarpaulin --out Html  # Coverage report
```

**Success Metrics**: 
- >90% test coverage
- All tests pass
- <200ms p95 API latency under load

---

## PHASE 9: PRODUCTION DEPLOYMENT (2-3h)
**Priority**: HIGH - Go-live  
**Goal**: VPS deployment with monitoring

### 9.1 Docker Configuration (1h)

**File**: `docker-compose.yml`

```yaml
version: '3.8'

services:
  rust-backend:
    build:
      context: ./rust-backend
      dockerfile: Dockerfile
    ports:
      - "3000:3000"
    environment:
      - RUST_LOG=info
      - DATABASE_URL=/data/betterbot.db
      - JWT_SECRET=${JWT_SECRET}
      - DOME_API_KEY=${DOME_API_KEY}
      - HASHDIVE_API_KEY=${HASHDIVE_API_KEY}
      - INITIAL_BANKROLL=10000
      - KELLY_FRACTION=0.25
    volumes:
      - ./data:/data
    restart: unless-stopped
    healthcheck:
      test: ["CMD", "curl", "-f", "http://localhost:3000/health"]
      interval: 30s
      timeout: 10s
      retries: 3
  
  nginx:
    image: nginx:alpine
    ports:
      - "80:80"
      - "443:443"
    volumes:
      - ./nginx.conf:/etc/nginx/nginx.conf:ro
      - ./ssl:/etc/nginx/ssl:ro
    depends_on:
      - rust-backend
    restart: unless-stopped
  
  prometheus:
    image: prom/prometheus
    ports:
      - "9090:9090"
    volumes:
      - ./prometheus.yml:/etc/prometheus/prometheus.yml:ro
    restart: unless-stopped
  
  grafana:
    image: grafana/grafana
    ports:
      - "3001:3000"
    environment:
      - GF_SECURITY_ADMIN_PASSWORD=${GRAFANA_PASSWORD}
    volumes:
      - grafana-data:/var/lib/grafana
    restart: unless-stopped

volumes:
  grafana-data:
```

**File**: `rust-backend/Dockerfile`

```dockerfile
FROM rust:1.75 as builder

WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src ./src

RUN cargo build --release

FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y \
    ca-certificates \
    libssl3 \
    curl \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/betterbot /usr/local/bin/betterbot

EXPOSE 3000

CMD ["betterbot"]
```

### 9.2 Nginx Configuration (0.5h)

**File**: `nginx.conf`

```nginx
events {
    worker_connections 1024;
}

http {
    upstream backend {
        server rust-backend:3000;
    }
    
    server {
        listen 80;
        server_name betterbot.example.com;
        
        location / {
            return 301 https://$server_name$request_uri;
        }
    }
    
    server {
        listen 443 ssl http2;
        server_name betterbot.example.com;
        
        ssl_certificate /etc/nginx/ssl/cert.pem;
        ssl_certificate_key /etc/nginx/ssl/key.pem;
        
        # API endpoints
        location /api/ {
            proxy_pass http://backend;
            proxy_set_header Host $host;
            proxy_set_header X-Real-IP $remote_addr;
            proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        }
        
        # WebSocket
        location /ws {
            proxy_pass http://backend;
            proxy_http_version 1.1;
            proxy_set_header Upgrade $http_upgrade;
            proxy_set_header Connection "upgrade";
            proxy_set_header Host $host;
        }
        
        # Metrics (restrict access)
        location /metrics {
            auth_basic "Metrics";
            auth_basic_user_file /etc/nginx/.htpasswd;
            proxy_pass http://backend;
        }
    }
}
```

### 9.3 Deployment Script (0.5h)

**File**: `deploy.sh`

```bash
#!/bin/bash
set -e

echo "🚀 Deploying BetterBot to production..."

# Pull latest code
git pull origin main

# Build and start services
docker-compose down
docker-compose build
docker-compose up -d

# Wait for health check
echo "⏳ Waiting for health check..."
sleep 10

if curl -f http://localhost:3000/health; then
    echo "✅ Deployment successful!"
else
    echo "❌ Health check failed!"
    docker-compose logs rust-backend
    exit 1
fi

# Run database migrations if needed
# docker-compose exec rust-backend betterbot --migrate

echo "📊 Monitoring: http://localhost:3001 (Grafana)"
echo "🔍 Metrics: http://localhost:9090 (Prometheus)"
```

### 9.4 Monitoring Setup (1h)

**File**: `prometheus.yml`

```yaml
global:
  scrape_interval: 15s

scrape_configs:
  - job_name: 'betterbot'
    static_configs:
      - targets: ['rust-backend:3000']
    metrics_path: '/metrics'
```

**Grafana Dashboard** (import JSON):
- Panels: Signals/hour, API latency, Kelly positions, VaR/CVaR
- Alerts: High drawdown, API errors, rate limit hits

**Verification**:
```bash
chmod +x deploy.sh
./deploy.sh

# Monitor logs
docker-compose logs -f rust-backend
```

**Success Metrics**: 
- 99.9% uptime
- <100ms API latency
- Zero panics in 24h
- All signals captured

---

## FINAL VERIFICATION CHECKLIST

### Code Quality
- [ ] `cargo clippy --all-targets -- -D warnings` passes
- [ ] `cargo test` all passing
- [ ] <10 remaining warnings
- [ ] All .unwrap()/.expect() eliminated from hot paths
- [ ] Backup files removed

### Functionality
- [ ] Database persistence working (10k signals retained)
- [ ] Dome WebSocket streaming real-time orders
- [ ] Arbitrage detector finding >2% spreads
- [ ] Elite/Insider classification accurate
- [ ] Kelly/VaR/CVaR risk management working
- [ ] JWT auth + rate limiting active

### Performance
- [ ] <100ms signal detection latency
- [ ] <200ms API response time (p95)
- [ ] <10ms database insert
- [ ] Zero missed orders (WebSocket vs REST parity check)

### Production Readiness
- [ ] Docker Compose deployment successful
- [ ] Health checks passing
- [ ] Prometheus metrics collecting
- [ ] Grafana dashboard showing data
- [ ] SSL certificates configured
- [ ] Environment secrets secured (.env not in git)

---

## POST-DEPLOYMENT MONITORING (First 48h)

### Hour 1-6: Critical Watch
- Monitor WebSocket connection stability
- Verify all 45 wallets streaming
- Check arbitrage opportunities detected
- Confirm database growth rate (<10MB/day)

### Hour 6-24: Performance Tuning
- Analyze API latency distribution
- Optimize slow queries (if any)
- Adjust Kelly fraction based on live performance
- Fine-tune arbitrage thresholds

### Hour 24-48: Optimization
- Run genetic algorithm on live data
- Update strategy parameters
- Scale resources if needed (CPU/memory)
- Implement auto-scaling rules

---

## MAINTENANCE SCHEDULE

### Daily
- Check Grafana dashboard for anomalies
- Review error logs
- Verify WebSocket uptime
- Backup database

### Weekly
- Run strategy optimizer
- Update wallet classifications
- Review API usage (Dome/Hashdive credits)
- Performance benchmarks

### Monthly
- Security audit (dependency updates)
- Cost analysis (API usage optimization)
- Backtest recent performance
- Strategy review meeting

---

## RISK MITIGATION

### Technical Risks
1. **API Rate Limits**: Monitor credits, implement backoff, fallback to cached data
2. **WebSocket Disconnections**: Auto-reconnect with exponential backoff
3. **Database Corruption**: Daily backups, transaction rollback on errors
4. **Memory Leaks**: Periodic restarts, memory profiling

### Financial Risks
1. **Kelly Overbetting**: Max 50% of Kelly fraction, position size caps
2. **Adverse Selection**: Track fill rates, adjust slippage estimates
3. **Market Impact**: Size limits based on liquidity
4. **Flash Crashes**: Circuit breakers, volatility filters

### Operational Risks
1. **Key Exposure**: Rotate keys monthly, use secrets manager
2. **Downtime**: Multi-region deployment (future), health checks
3. **Data Loss**: Incremental backups, replication
4. **Unauthorized Access**: JWT expiry, IP whitelisting, 2FA

---

## EXPECTED OUTCOMES

### Performance Metrics (30-day projection)
- **Sharpe Ratio**: >1.5 (target: 2.0)
- **Win Rate**: >60% (insider signals), >55% (arbitrage)
- **Max Drawdown**: <15%
- **Avg Profit/Trade**: >$50
- **Daily Signals**: 20-50 (5-10 actionable)

### Cost Efficiency
- **Dome API**: 480 calls/mo (free tier)
- **Hashdive API**: <500 calls/mo (free tier)
- **VPS**: $20-40/mo (DigitalOcean/Linode 4GB RAM)
- **Total**: <$50/mo infrastructure

### Competitive Advantage
- **Speed**: <100ms signal latency (vs market avg 1-5s)
- **Coverage**: 45 elite wallets tracked
- **Real-time**: WebSocket vs polling (10x faster)
- **Risk-Adjusted**: Kelly-optimal sizing

---

## PHASE DEPENDENCIES & CRITICAL PATH

```
CRITICAL PATH (Must complete sequentially):
Phase 1 → Phase 2 → Phase 7 → Phase 9
  (3h)    (2h)      (3h)      (2h)  = 10h minimum

PARALLEL WORK (Can be done concurrently):
Phase 3 + Phase 4 + Phase 5 = 9h
Phase 6 + Phase 8 = 8h

TOTAL: 10h critical + 9h parallel = ~19h minimum
       With testing/debugging buffer: 25-30h realistic
```

---

## SUCCESS DEFINITION

**Production-Ready** = All of:
✅ Zero panics in 24h continuous operation  
✅ All 45 wallets streaming via WebSocket  
✅ >5 arbitrage opportunities detected per day  
✅ Database persisting >1000 signals  
✅ API authenticated with JWT  
✅ Prometheus metrics dashboard live  
✅ Docker deployment reproducible  
✅ Kelly positions calculated correctly  
✅ Win rate >55% on backtests  

**Elite-Level** = Above + :
✅ Sharpe ratio >1.5 in live trading  
✅ <100ms end-to-end signal latency  
✅ Genetic optimizer improving performance  
✅ Multi-datacenter failover ready  
✅ Institutional audit passed (security/compliance)  

---

## FINAL NOTES

This plan transforms BetterBot from "operational prototype" to **production-grade quantitative trading system** at the level of firms like Wintermute and Renaissance Technologies.

**Key Differentiators:**
1. **Real-time WebSocket** vs polling (10x speed advantage)
2. **Elite wallet classification** (insider edge detection)
3. **Kelly-optimal sizing** (quantitative risk management)
4. **Genetic optimization** (auto-tuning strategies)
5. **Cross-platform arbitrage** (structural alpha)

**Execution Priority:**
Focus on **Critical Path** first (Phases 1, 2, 7, 9) for stable deployment, then add **Alpha Generators** (Phases 3, 4, 5, 6) incrementally.

**Risk-Adjusted Expected Value:**
With proper execution, this system should achieve **Sharpe >1.5** with **<15% max drawdown** on a **$10k bankroll**, generating **$500-1500/month** profit once fully optimized.

---

**Ready for execution. Awaiting approval to proceed with Phase 1.**

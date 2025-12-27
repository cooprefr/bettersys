# Phase 4: Arbitrage Detection System - Implementation Plan

**Objective**: Build production-grade cross-platform arbitrage detection engine  
**Priority**: HIGH (Core profit generation)  
**Estimated Duration**: 4-5 hours

---

## Mission

Create a real-time arbitrage detection system that:
1. Monitors prices across Polymarket and Kalshi
2. Detects profitable arbitrage opportunities (price mismatches)
3. Calculates net profit after fees and slippage
4. Applies risk-adjusted position sizing
5. Generates actionable signals with execution plans

---

## Current State Analysis

### Existing Code
- ✅ `dome.rs`: Basic arbitrage structures defined
- ✅ `models.rs`: `CrossPlatformArbitrage` signal type
- ✅ Dome API client for market matching
- ❌ **Missing**: Actual arbitrage detection logic
- ❌ **Missing**: Fee calculations
- ❌ **Missing**: Profitability analysis

### Signal Type (Already Defined)
```rust
CrossPlatformArbitrage {
    polymarket_price: f64,
    kalshi_price: Option<f64>,
    spread_pct: f64,
}
```

---

## Phase 4 Implementation Strategy

### 4.1 Arbitrage Engine Core

**New File**: `rust-backend/src/arbitrage/engine.rs`

#### Components

```rust
pub struct ArbitrageEngine {
    polymarket_client: PolymarketScraper,
    dome_client: DomeScraper,
    risk_manager: Arc<RwLock<RiskManager>>,
}

impl ArbitrageEngine {
    // Core arbitrage detection
    pub async fn scan_opportunities(&mut self) -> Result<Vec<ArbitrageOpportunity>>;
    
    // Fee-adjusted profitability
    fn calculate_net_profit(&self, opp: &ArbitrageOpportunity) -> f64;
    
    // Risk-adjusted sizing
    fn calculate_position_size(&self, opp: &ArbitrageOpportunity) -> f64;
    
    // Execution plan
    fn generate_execution_plan(&self, opp: &ArbitrageOpportunity) -> ExecutionPlan;
}
```

#### Fee Structure

**Polymarket (CLOB)**:
- Maker fee: 0% (free liquidity provision)
- Taker fee: 2% (on shares bought/sold)
- Gas costs: $0.10-$0.50 per transaction (Polygon)

**Kalshi**:
- Trading fee: 7% (on profits)
- No taker/maker distinction
- Settlement fee: included

**Fee Calculation**:
```rust
pub struct FeeStructure {
    pub polymarket_taker_fee: f64,  // 0.02 (2%)
    pub polymarket_gas_usd: f64,     // 0.30 (avg)
    pub kalshi_fee: f64,             // 0.07 (7%)
}

impl FeeStructure {
    pub fn calculate_total_fees(&self, trade_size_usd: f64) -> f64 {
        let poly_fee = trade_size_usd * self.polymarket_taker_fee;
        let kalshi_fee = trade_size_usd * self.kalshi_fee;
        poly_fee + kalshi_fee + self.polymarket_gas_usd
    }
}
```

#### Arbitrage Detection Logic

**Step 1: Market Matching**
- Use Dome API to find equivalent markets
- Match by event, question, and resolution criteria
- Handle slight question wording differences

**Step 2: Price Comparison**
```rust
// Polymarket: YES price = 0.65
// Kalshi: YES price = 0.58
// Spread = 0.65 - 0.58 = 0.07 (7%)

// Trade:
// - Buy YES on Kalshi @ 0.58
// - Sell YES on Polymarket @ 0.65
// - Gross profit: 0.07 per share = 7%
```

**Step 3: Fee Adjustment**
```rust
let gross_profit = poly_price - kalshi_price;
let fees = calculate_total_fees(trade_size);
let net_profit = gross_profit - fees;
let net_profit_pct = net_profit / kalshi_price;

// Minimum profitable spread: 3% after fees
const MIN_PROFITABLE_SPREAD: f64 = 0.03;
```

**Step 4: Risk Scoring**
```rust
pub fn calculate_arbitrage_confidence(
    spread_pct: f64,
    poly_liquidity: f64,
    kalshi_volume: f64,
    time_to_expiry_hours: f64,
) -> f64 {
    let mut confidence = 0.5;
    
    // Higher spread = higher confidence
    if spread_pct > 0.05 { confidence += 0.2; }
    
    // Good liquidity = higher confidence
    if poly_liquidity > 50000.0 { confidence += 0.15; }
    
    // Volume confirmation
    if kalshi_volume > 10000.0 { confidence += 0.10; }
    
    // Time decay penalty
    if time_to_expiry_hours < 24.0 {
        confidence -= 0.15; // Risky close to expiry
    }
    
    confidence.min(0.95).max(0.3)
}
```

---

### 4.2 Market Matching Algorithm

**Dome API Endpoint**: `/v1/matching-markets`

**Request**:
```rust
pub async fn find_matching_markets(
    &mut self,
    polymarket_slug: &str,
) -> Result<Vec<MatchedMarket>> {
    let url = format!("{}/v1/matching-markets", DOME_API_BASE);
    
    let response = self.client
        .get(&url)
        .header("X-API-Key", &self.api_key)
        .query(&[("polymarket_slug", polymarket_slug)])
        .send()
        .await?;
    
    let matches: Vec<MatchedMarket> = response.json().await?;
    Ok(matches)
}
```

**Response Structure**:
```json
{
    "polymarket_slug": "trump-wins-2024",
    "matches": [
        {
            "platform": "kalshi",
            "market_ticker": "PRES-2024",
            "confidence": 0.95,
            "polymarket_price": 0.65,
            "kalshi_price": 0.58
        }
    ]
}
```

---

### 4.3 Execution Planning

**Two-Leg Arbitrage**:

```rust
pub struct ExecutionPlan {
    pub leg1: TradeLeg,  // Buy on cheaper platform
    pub leg2: TradeLeg,  // Sell on expensive platform
    pub expected_profit_usd: f64,
    pub risk_score: f64,
    pub instructions: String,
}

pub struct TradeLeg {
    pub platform: String,     // "polymarket" or "kalshi"
    pub action: String,       // "BUY" or "SELL"
    pub outcome: String,      // "YES" or "NO"
    pub shares: f64,
    pub price: f64,
    pub total_cost_usd: f64,
}

impl ArbitrageEngine {
    pub fn generate_execution_plan(
        &self,
        opp: &ArbitrageOpportunity,
        max_position_usd: f64,
    ) -> ExecutionPlan {
        // Leg 1: Buy on cheaper platform
        let leg1 = TradeLeg {
            platform: opp.cheaper_platform.clone(),
            action: "BUY".to_string(),
            outcome: "YES".to_string(),
            shares: max_position_usd / opp.cheaper_price,
            price: opp.cheaper_price,
            total_cost_usd: max_position_usd,
        };
        
        // Leg 2: Sell on expensive platform
        let leg2 = TradeLeg {
            platform: opp.expensive_platform.clone(),
            action: "SELL".to_string(),
            outcome: "YES".to_string(),
            shares: leg1.shares,
            price: opp.expensive_price,
            total_cost_usd: leg1.shares * opp.expensive_price,
        };
        
        let expected_profit = leg2.total_cost_usd - leg1.total_cost_usd;
        let expected_profit_after_fees = expected_profit - self.fees.calculate_total_fees(max_position_usd);
        
        ExecutionPlan {
            leg1,
            leg2,
            expected_profit_usd: expected_profit_after_fees,
            risk_score: opp.confidence,
            instructions: format!(
                "1. Buy {} shares on {} @ ${:.3}\n2. Sell {} shares on {} @ ${:.3}\n3. Net profit: ${:.2}",
                leg1.shares, leg1.platform, leg1.price,
                leg2.shares, leg2.platform, leg2.price,
                expected_profit_after_fees
            ),
        }
    }
}
```

---

### 4.4 Real-Time Monitoring

**Integration with Main Loop**:

```rust
// In main.rs - parallel_data_collection()
tokio::spawn(arbitrage_scanning_loop(
    signal_storage.clone(),
    signal_tx.clone(),
    risk_manager.clone(),
));

async fn arbitrage_scanning_loop(
    storage: Arc<DbSignalStorage>,
    signal_tx: broadcast::Sender<MarketSignal>,
    risk_manager: Arc<RwLock<RiskManager>>,
) -> Result<()> {
    info!("🎯 Starting arbitrage detection loop");
    
    let mut engine = ArbitrageEngine::new(
        PolymarketScraper::new(),
        DomeScraper::new(env::var("DOME_API_KEY").unwrap_or_default()),
        risk_manager,
    );
    
    let mut interval = interval(Duration::from_secs(10)); // Check every 10 seconds
    
    loop {
        interval.tick().await;
        
        match engine.scan_opportunities().await {
            Ok(opportunities) => {
                info!("💎 Found {} arbitrage opportunities", opportunities.len());
                
                for opp in opportunities {
                    // Convert to MarketSignal
                    let signal = MarketSignal {
                        id: format!("arb_{}_{}", opp.polymarket_market, chrono::Utc::now().timestamp()),
                        signal_type: SignalType::CrossPlatformArbitrage {
                            polymarket_price: opp.polymarket_price,
                            kalshi_price: opp.kalshi_price,
                            spread_pct: opp.spread_pct,
                        },
                        market_slug: opp.polymarket_market,
                        confidence: opp.confidence,
                        risk_level: if opp.confidence > 0.8 { "low" } else { "medium" }.to_string(),
                        details: SignalDetails {
                            market_id: opp.polymarket_market.clone(),
                            market_title: format!("Arbitrage: {:.1}% spread", opp.spread_pct * 100.0),
                            current_price: opp.polymarket_price,
                            volume_24h: 0.0,
                            liquidity: 0.0,
                            recommended_action: format!(
                                "Buy Kalshi @ {:.3}, Sell Polymarket @ {:.3}",
                                opp.kalshi_price.unwrap_or(0.0),
                                opp.polymarket_price
                            ),
                            expiry_time: None,
                        },
                        detected_at: chrono::Utc::now().to_rfc3339(),
                        source: "arbitrage_engine".to_string(),
                    };
                    
                    // Store and broadcast
                    if let Err(e) = storage.store(&signal).await {
                        warn!("Failed to store arbitrage signal: {}", e);
                    }
                    let _ = signal_tx.send(signal);
                }
            }
            Err(e) => {
                warn!("Arbitrage scan error: {}", e);
            }
        }
    }
}
```

---

## Implementation Steps

### Step 1: Create Arbitrage Module (2 hours)
1. Create `rust-backend/src/arbitrage/mod.rs`
2. Create `rust-backend/src/arbitrage/engine.rs`
3. Create `rust-backend/src/arbitrage/fees.rs`
4. Implement core detection logic

### Step 2: Integrate with Main (1 hour)
1. Update `main.rs` to spawn arbitrage loop
2. Connect to existing signal storage
3. Add configuration for scan intervals

### Step 3: Testing & Validation (1 hour)
1. Unit tests for fee calculations
2. Integration tests for arbitrage detection
3. Mock data tests for edge cases

### Step 4: Documentation (30 min)
1. Create PHASE_4_COMPLETE.md
2. Document arbitrage strategies
3. Add usage examples

---

## Success Metrics

| Metric | Target | Measurement |
|--------|--------|-------------|
| Detection Speed | <15 seconds | Time from price change to signal |
| False Positives | <5% | Invalid arbitrage alerts |
| Profitable Spreads | >3% after fees | Net profit threshold |
| Execution Plans | 100% | All signals have plans |

---

## Risk Management

### Position Sizing
```rust
// Kelly Criterion for arbitrage
pub fn calculate_arbitrage_position(
    &self,
    spread_pct: f64,
    confidence: f64,
    bankroll: f64,
) -> f64 {
    // Conservative sizing: 5-10% of bankroll per arb
    let kelly_fraction = 0.25; // 25% Kelly (conservative)
    let edge = spread_pct * confidence;
    let position = bankroll * kelly_fraction * edge;
    
    // Cap at 10% of bankroll per opportunity
    position.min(bankroll * 0.10)
}
```

### Slippage Protection
```rust
const MAX_SLIPPAGE_PCT: f64 = 0.01; // 1% max slippage
const MIN_LIQUIDITY_USD: f64 = 50000.0; // Minimum liquidity required
```

---

## Next Phase

**Phase 5: Advanced Signal Detection**
- Multi-signal correlation
- Machine learning for pattern recognition
- Historical performance tracking

---

## Notes

- Arbitrage opportunities typically last 30-300 seconds
- Need fast execution (Phase 3 WebSockets are critical)
- Most profitable spreads: 3-8% after fees
- High-confidence arbs (>0.9): rare but highly profitable

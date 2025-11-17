# 🤖 AGENTS.MD - Complete System Guide for AI Agents

**Last Updated:** November 17, 2025  
**Purpose:** Comprehensive technical reference for AI agents working on BetterBot  
**Target Audience:** Future AI assistants, developers, and system architects

---

## 📚 TABLE OF CONTENTS

1. [System Overview](#system-overview)
2. [Critical Lessons Learned](#critical-lessons-learned)
3. [Backend Architecture](#backend-architecture)
4. [API Integration Details](#api-integration-details)
5. [Frontend Architecture](#frontend-architecture)
6. [Data Flow & Processing](#data-flow--processing)
7. [Common Pitfalls & Solutions](#common-pitfalls--solutions)
8. [Testing & Verification](#testing--verification)
9. [Deployment Checklist](#deployment-checklist)
10. [Quick Reference](#quick-reference)

---

## 🔑 SECRETS & ENVIRONMENT (DO NOT COMMIT SECRETS)

Status: Keys previously appeared in this document. Treat as a security incident. Rotate all affected credentials immediately and remove any history containing secrets.

Immediate actions (required):
- Rotate Hashdive and Dome API credentials in the provider dashboards.
- Purge secrets from git history (use git filter-repo or BFG) and force-push if this repo is remote.
- Add or validate secret scanning in CI (e.g., gitleaks) and a pre-commit hook.

How to configure (sanitized):

1) rust-backend/.env (create locally; never commit)
```bash
# Server
PORT=8080
RUST_LOG=info,betterbot=debug

# Databases
DATABASE_PATH=./betterbot_signals.db
AUTH_DB_PATH=./betterbot_auth.db

# External APIs (set via environment; do not place secrets in files)
# export HASHDIVE_API_KEY=...   # set in shell/CI secret manager
# export DOME_API_KEY=...       # set in shell/CI secret manager
HASHDIVE_WHALE_MIN_USD=10000

# Source kill-switches (optional; defaults shown)
POLYMARKET_ENABLED=true
POLYMARKET_FAILURE_THRESHOLD=3
POLYMARKET_LATENCY_P95_MS=5000
HASHDIVE_ENABLED=true
HASHDIVE_FAILURE_THRESHOLD=3
HASHDIVE_LATENCY_P95_MS=10000
DOME_ENABLED=true
DOME_FAILURE_THRESHOLD=3
DOME_LATENCY_P95_MS=8000

# Optional services
TWITTER_PYTHON_SERVICE_URL=http://localhost:8081

# Polling and limits
TWITTER_SCRAPE_INTERVAL=30
HASHDIVE_SCRAPE_INTERVAL=2700
```

2) frontend/.env (dev only; never commit production values)
```bash
VITE_API_URL=http://localhost:3000
VITE_WS_URL=ws://localhost:3000/ws
```

3) Provide templates (tracked) and keep real env files untracked
- Track: `rust-backend/.env.example` and `frontend/.env.example` with placeholders only.
- Ensure `.gitignore` includes `.env`, `frontend/.env`.

Auth header usage (no secrets in docs):
- Hashdive REST: header `x-api-key: ${HASHDIVE_API_KEY}`
- Dome REST: header `Authorization: Bearer ${DOME_API_KEY}`
- Dome WS: `wss://ws.domeapi.io/${DOME_API_KEY}`

Operational note:
- For any configuration change, restart backend: `cd rust-backend && cargo run`

---

## 1. SYSTEM OVERVIEW

### What is BetterBot?

BetterBot is a production-grade quantitative trading system for Polymarket prediction markets featuring:
- **Backend:** Rust (Axum framework)
- **Frontend:** React + TypeScript (Vite + TailwindCSS)
- **Real-time:** WebSocket streaming
- **Security:** JWT authentication with bcrypt
- **Databases:** SQLite (signals + auth)
- **APIs:** Hashdive, DomeAPI, Polymarket GAMMA

Current state highlights (implemented):
- Data source kill-switches with p95 latency monitoring and failure thresholds (main.rs)
- Institutional-grade risk guardrails incl. fractional Kelly cap (≤0.20), drawdown throttle (8%/4%), regime risk factor, isotonic calibration; guardrail flags propagated to frontend (risk.rs + main.rs)
- Database-backed signal storage with cleanup and indices; attribution fields stored (signals/db_storage.rs)
- Walk-forward backtesting with embargo/leakage controls and hygiene checks (backtest.rs)
- Real-time tracked wallet streaming via Dome WebSocket with auto-reconnect (scrapers/dome_websocket.rs + main.rs)

### Architecture at a Glance

```
┌─────────────────────────────────────────────────────────┐
│                    BETTERBOT SYSTEM                      │
├─────────────────────────────────────────────────────────┤
│                                                          │
│  ┌──────────────┐         ┌──────────────┐             │
│  │   Frontend    │◄───WS──►│   Backend     │            │
│  │  React + TS   │         │  Rust + Axum  │            │
│  │  localhost    │         │  localhost    │            │
│  │    :5173      │         │    :3000      │            │
│  └──────────────┘         └──────────────┘             │
│         ▲                        │                       │
│         │                        ▼                       │
│         │             ┌────────────────────┐            │
│         │             │  SQLite Databases  │            │
│         │             │  - Signals         │            │
│         │             │  - Authentication  │            │
│         │             └────────────────────┘            │
│         │                        │                       │
│         │                        ▼                       │
│         │             ┌────────────────────┐            │
│         └─────────────│   External APIs    │            │
│                       │  - Hashdive        │            │
│                       │  - DomeAPI         │            │
│                       │  - Polymarket      │            │
│                       └────────────────────┘            │
│                                                          │
└─────────────────────────────────────────────────────────┘
```

---

## 2. CRITICAL LESSONS LEARNED

### 🚨 LESSON 1: Type System Mismatches (Backend ↔ Frontend)

**PROBLEM:**  
Rust backend sends tagged enum unions, but TypeScript frontend expected simple strings.

**EXAMPLE:**

**Backend (Rust):**
```rust
#[derive(Serialize)]
#[serde(tag = "type")]
pub enum SignalType {
    WhaleFollowing {
        whale_address: String,
        position_size: f64,
        confidence_score: f64,
    },
    // ...
}
```

**What Backend Sends:**
```json
{
  "signal_type": {
    "type": "WhaleFollowing",
    "whale_address": "0xabc...",
    "position_size": 50000.0,
    "confidence_score": 0.85
  }
}
```

**What Frontend Initially Expected:**
```typescript
// WRONG
signal_type: "WhaleFollowing" // Simple string
```

**SOLUTION:**

```typescript
// CORRECT - Match Rust's tagged union
export type SignalType =
  | { type: 'WhaleFollowing'; whale_address: string; position_size: number; confidence_score: number }
  | { type: 'EliteWallet'; wallet_address: string; win_rate: number; total_volume: number; position_size: number }
  // ...

// Access the type:
if (signal.signal_type.type === 'WhaleFollowing') {
  console.log(signal.signal_type.whale_address); // Correct
}
```

**KEY TAKEAWAY:** Always check Rust's serde serialization output format. Use `#[serde(tag = "type")]` for tagged unions.

---

### 🚨 LESSON 2: API Authentication Formats

**PROBLEM:**  
Different APIs use different authentication mechanisms. Mixing them causes silent failures.

**API AUTH MATRIX:**

| API | Header | Format | Example |
|-----|--------|--------|---------|
| **Hashdive** | `x-api-key` | Direct key | `x-api-key: abc123` |
| **DomeAPI REST** | `Authorization` | Bearer token | `Authorization: Bearer ${DOME_API_KEY}` |
| **DomeAPI WebSocket** | URL path | API key in path | `wss://ws.domeapi.io/${DOME_API_KEY}` |
| **Polymarket** | None | Public API | No auth needed |

**CRITICAL IMPLEMENTATION:**

```rust
// Hashdive - x-api-key header
.header("x-api-key", &self.api_key)

// DomeAPI REST - Bearer token
.header("Authorization", format!("Bearer {}", api_key))

// DomeAPI WebSocket - URL path
let ws_url = format!("wss://ws.domeapi.io/{}", api_key);
```

**COMMON MISTAKE:**
```rust
// WRONG - Using X-API-Key for DomeAPI (doesn't work!)
.header("X-API-Key", &api_key)
```

**KEY TAKEAWAY:** Never assume auth headers. Always verify API documentation for exact format.

---

### 🚨 LESSON 3: Rate Limiting & Credit Management

**PROBLEM:**  
Hashdive has 1000 monthly credits. Aggressive polling exhausts credits quickly.

**CREDIT CALCULATIONS:**

| Polling Interval | Requests/Day | Requests/Month | Status |
|-----------------|--------------|----------------|---------|
| 30 seconds | 2,880 | 86,400 | ❌ Exhausts in hours |
| 5 minutes | 288 | 8,640 | ❌ Exhausts in 3 days |
| 15 minutes | 96 | 2,880 | ❌ Exhausts in 10 days |
| **45 minutes** | **32** | **960** | ✅ Under 1000 limit |
| 1 hour | 24 | 720 | ✅ Safe with buffer |

**SOLUTION:**

```rust
// CORRECT - 45-minute polling
let mut interval_timer = interval(Duration::from_secs(2700)); // 45 minutes

// Also implement rate limiting per request
struct RateLimiter {
    last_request: Instant,
    min_interval: Duration, // 1 second for Hashdive
}
```

**KEY TAKEAWAY:** Calculate API usage math BEFORE implementing. Use timers strategically.

---

### 🚨 LESSON 4: WebSocket Reconnection Logic

**PROBLEM:**  
DomeAPI WebSocket connection can fail. Without proper reconnection, system stops receiving updates.

**CORRECT IMPLEMENTATION:**

```rust
pub async fn run(&self) -> Result<()> {
    let mut reconnect_delay = Duration::from_secs(1);
    let max_reconnect_delay = Duration::from_secs(60);
    
    loop {
        match self.connect_and_stream().await {
            Ok(_) => {
                info!("WebSocket connection closed gracefully");
                reconnect_delay = Duration::from_secs(1);
            }
            Err(e) => {
                error!("WebSocket error: {}", e);
                warn!("Reconnecting in {:?}...", reconnect_delay);
                sleep(reconnect_delay).await;
                
                // Exponential backoff up to 60 seconds
                reconnect_delay = (reconnect_delay * 2).min(max_reconnect_delay);
            }
        }
    }
}
```

**KEY ELEMENTS:**
1. **Infinite loop** - Never give up reconnecting
2. **Exponential backoff** - Start at 1s, double each retry, cap at 60s
3. **Reset on success** - Go back to 1s delay after successful connection

**KEY TAKEAWAY:** WebSockets WILL disconnect. Plan for it with exponential backoff.

---

### 🚨 LESSON 5: Mock Data Fallback Strategy

**PROBLEM:**  
During development, APIs might not be configured. System needs graceful degradation.

**CORRECT APPROACH:**

```rust
// Collect all real API signals
let mut all_signals = Vec::new();

if let Ok(Ok(signals)) = poly_result {
    all_signals.extend(signals);
}
if let Ok(Ok(signals)) = hash_result {
    all_signals.extend(signals);
}

// ONLY use mock if NO real signals
if all_signals.is_empty() {
    info!("⚠️  No real signals detected, generating mock signals for testing");
    all_signals.extend(mock_gen.generate_signals(10));
}
```

**KEY POINTS:**
- Mock is **fallback**, not default
- Always try real APIs first
- Log clearly when using mock data
- Make it easy to disable mock in production

**KEY TAKEAWAY:** Graceful degradation allows development without all APIs configured.

---

### 🚨 LESSON 6: Login Response Structure

**PROBLEM:**  
Frontend needed username to display, but initial login response didn't include user object.

**WRONG:**
```rust
// Missing user object
pub struct LoginResponse {
    pub token: String,
    pub expires_in: usize,
    pub role: UserRole,
}
```

**CORRECT:**
```rust
pub struct LoginResponse {
    pub token: String,
    pub expires_in: usize,
    pub role: UserRole,
    pub user: UserResponse, // ✅ Include sanitized user object
}

pub struct UserResponse {
    pub id: String,
    pub username: String,
    pub role: UserRole,
    pub created_at: String,
    // NO password_hash!
}
```

**KEY TAKEAWAY:** Login responses should include user data needed by frontend. Never send password hashes.

---

## 3. BACKEND ARCHITECTURE

### Directory Structure

```
rust-backend/
├── src/
│   ├── main.rs                 # Entry point, server setup
│   ├── models.rs               # Core data structures
│   ├── api/                    # REST API endpoints
│   │   ├── mod.rs
│   │   ├── signals.rs          # Signal endpoints
│   │   └── risk.rs             # Risk management endpoints
│   ├── auth/                   # Authentication system
│   │   ├── mod.rs
│   │   ├── models.rs           # User, Claims, etc.
│   │   ├── jwt.rs              # JWT token handling
│   │   ├── user_store.rs       # User database
│   │   ├── middleware.rs       # Auth middleware
│   │   └── api.rs              # Login/logout endpoints
│   ├── arbitrage/              # Arbitrage detection
│   │   ├── mod.rs
│   │   ├── engine.rs
│   │   └── fees.rs
│   ├── signals/                # Signal detection & storage
│   │   ├── mod.rs
│   │   ├── detector.rs         # Pattern detection
│   │   ├── db_storage.rs       # SQLite persistence
│   │   └── correlation.rs      # Multi-signal correlation
│   ├── scrapers/               # External API integrations
│   │   ├── mod.rs
│   │   ├── hashdive_api.rs     # Hashdive integration
│   │   ├── dome.rs             # DomeAPI REST
│   │   ├── dome_tracker.rs     # DomeAPI client
│   │   ├── dome_websocket.rs   # DomeAPI WebSocket
│   │   ├── polymarket_api.rs   # Polymarket GAMMA
│   │   ├── expiry_edge.rs      # Expiry edge scanner
│   │   └── mock_generator.rs   # Mock data for testing
│   ├── risk.rs                 # Risk management
│   └── backtest.rs             # Backtesting engine
├── Cargo.toml                  # Dependencies
└── .env                        # Environment variables
```

### Key Components

#### 1. Main Server (`main.rs`)

**Responsibilities:**
- Initialize databases (signals + auth)
- Spawn background tasks (API polling, WebSocket streaming)
- Set up HTTP server with Axum
- Configure CORS and middleware

**Critical Sections:**

```rust
// Polling tasks
tokio::spawn(parallel_data_collection(...));  // 45-min intervals
tokio::spawn(tracked_wallet_polling(...));     // WebSocket streaming
tokio::spawn(expiry_edge_polling(...));        // 60-sec intervals
```

#### 2. Signal Types (`models.rs`)

**The Core Enum:**

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]  // ⚠️ CRITICAL: Creates tagged union
pub enum SignalType {
    PriceDeviation { market_price: f64, fair_value: f64, deviation_pct: f64 },
    MarketExpiryEdge { hours_to_expiry: f64, volume_spike: f64 },
    WhaleFollowing { whale_address: String, position_size: f64, confidence_score: f64 },
    EliteWallet { wallet_address: String, win_rate: f64, total_volume: f64, position_size: f64 },
    InsiderWallet { wallet_address: String, early_entry_score: f64, win_rate: f64, position_size: f64 },
    WhaleCluster { cluster_count: usize, total_volume: f64, consensus_direction: String },
    CrossPlatformArbitrage { polymarket_price: f64, kalshi_price: Option<f64>, spread_pct: f64 },
    TrackedWalletEntry { wallet_address: String, wallet_label: String, position_value_usd: f64, order_count: usize },
}
```

**Why `#[serde(tag = "type")]`?**
- Creates JSON with `"type"` field
- Allows TypeScript discriminated unions
- Enables type-safe pattern matching

---

## 4. API INTEGRATION DETAILS

### 4.1 HASHDIVE API

**Base URL:** `https://hashdive.com/api`  
**Authentication:** `x-api-key` header  
**Rate Limit:** 1 request per second  
**Monthly Credits:** 1000  
**Data Update Frequency:** Every 1 minute  

#### Key Endpoints

##### `/get_latest_whale_trades`

**Purpose:** Fetch recent large trades (whale activity)

**Request:**
```rust
let url = format!("{}/get_latest_whale_trades", HASHDIVE_API_BASE);
let mut params = HashMap::new();
params.insert("min_usd", "20000".to_string());  // Minimum trade size
params.insert("limit", "50".to_string());        // Max results
params.insert("format", "json".to_string());

let response = self.client
    .get(&url)
    .header("x-api-key", &self.api_key)
    .query(&params)
    .send()
    .await?;
```

**Response Structure:**
```json
{
  "data": [
    {
      "user_address": "0xabc123...",
      "asset_id": "77874905...",
      "side": "BUY",
      "size": 25000.50,
      "price": 0.54,
      "timestamp": 1700000000,
      "market_slug": "will-bitcoin-reach-100k",
      "market_title": "Will Bitcoin reach $100k by 2025?"
    }
  ],
  "count": 15
}
```

**Processing Example:**
```rust
match scraper.get_latest_whale_trades(Some(20000.0), Some(50)).await {
    Ok(whale_response) => {
        let signals: Vec<MarketSignal> = whale_response
            .data
            .into_iter()
            .filter(|trade| trade.size > 10000.0)  // Additional filter
            .map(|trade| MarketSignal {
                id: format!("whale_{}", trade.timestamp),
                signal_type: SignalType::WhaleFollowing {
                    whale_address: trade.user_address.clone(),
                    position_size: trade.size,
                    confidence_score: (trade.size / 100000.0).min(0.99),
                },
                market_slug: trade.market_slug,
                confidence: (trade.size / 50000.0).min(0.95),
                // ... rest of signal
            })
            .collect();
    }
}
```

##### `/get_trades`

**Purpose:** Get trades for specific wallet

**Request:**
```rust
let mut params = HashMap::new();
params.insert("user_address", wallet_address.to_string());
params.insert("page", "1".to_string());
params.insert("page_size", "100".to_string());
```

**Use Case:** Wallet classification (Elite/Insider/Whale)

##### `/get_positions`

**Purpose:** Current positions for wallet

**Note:** Position data for inactive users may be archived. Always check response.

#### Wallet Classification Logic

```rust
pub fn classify_wallet(&self, wallet_data: &WalletData) -> WalletTier {
    let volume = wallet_data.total_volume;
    let win_rate = wallet_data.win_rate;
    let early_entry = wallet_data.early_entry_score;
    
    if volume > 100_000.0 && win_rate > 0.65 {
        WalletTier::Elite  // 👑
    } else if win_rate > 0.70 && early_entry > 0.75 {
        WalletTier::Insider  // 🎯
    } else if volume > 50_000.0 {
        WalletTier::Whale  // 🐋
    } else {
        WalletTier::Regular
    }
}
```

---

### 4.2 DOME API (REST)

**Base URL:** `https://api.domeapi.io/v1/polymarket`  
**Authentication:** `Authorization: Bearer ${API_KEY}`  
**Rate Limit:** Varies by tier (Free: 100/day, Dev: 10,000/day)  

#### Key Endpoints

##### `/wallet`

**Purpose:** Convert between EOA and proxy wallet addresses

**Request:**
```rust
let url = format!("{}/v1/polymarket/wallet", DOME_API_BASE);
let response = self.client
    .get(&url)
    .header("Authorization", format!("Bearer {}", api_key))
    .query(&[("eoa", wallet_address)])
    .send()
    .await?;
```

**Response:**
```json
{
  "eoa": "<eoa_address>",
  "proxy": "<proxy_address>",
  "wallet_type": "safe"
}
```

##### `/orders`

**Purpose:** Get historical orders for wallet

**Request:**
```rust
let url = format!("{}/v1/polymarket/orders", DOME_API_BASE);
let response = self.client
    .get(&url)
    .header("Authorization", format!("Bearer {}", api_key))
    .query(&[
        ("user", wallet_address),
        ("limit", "100"),
        ("offset", "0")
    ])
    .send()
    .await?;
```

**Response:**
```json
{
  "orders": [
    {
      "token_id": "57564352641769637...",
      "side": "BUY",
      "shares_normalized": 5.0,
      "price": 0.54,
      "timestamp": 1700000000,
      "market_slug": "btc-updown-15m",
      "title": "Bitcoin Up or Down",
      "user": "0x6031b6eed1c97..."
    }
  ],
  "count": 42
}
```

---

### 4.3 DOME API (WebSocket)

**URL:** `wss://ws.domeapi.io/${API_KEY}`  
**Purpose:** Real-time order streaming (30-90x faster than polling)  

#### Connection Setup

```rust
pub struct DomeWebSocketClient {
    api_key: String,
    tracked_wallets: Vec<String>,
    order_tx: mpsc::UnboundedSender<WsOrderData>,
}

impl DomeWebSocketClient {
    pub fn new(
        api_key: String,
        tracked_wallets: Vec<String>,
    ) -> (Self, mpsc::UnboundedReceiver<WsOrderData>) {
        let (order_tx, order_rx) = mpsc::unbounded_channel();
        let client = Self { api_key, tracked_wallets, order_tx };
        (client, order_rx)
    }
}
```

#### Subscription Message

**Send after connection:**
```json
{
  "action": "subscribe",
  "platform": "polymarket",
  "version": 1,
  "type": "orders",
  "filters": {
    "users": [
      "<wallet_address_a>",
      "<wallet_address_b>"
    ]
  }
}
```

#### Order Update Messages

**Receive continuously:**
```json
{
  "type": "event",
  "subscription_id": "sub_m58zfduokmd",
  "data": {
    "token_id": "57564352641769637...",
    "side": "BUY",
    "market_slug": "btc-updown-15m-1762755300",
    "condition_id": "0x592b8a416cbe36...",
    "shares": 5000000,
    "shares_normalized": 5.0,
    "price": 0.54,
    "tx_hash": "<tx_hash>",
    "title": "Bitcoin Up or Down - Nov 10, 1:15AM",
    "timestamp": 1762755335,
    "order_hash": "<order_hash>",
    "user": "0x6031b6eed1c97..."
  }
}
```

#### Processing Pipeline

```rust
async fn tracked_wallet_polling(...) -> Result<()> {
    // Create WebSocket client
    let (ws_client, mut order_rx) = DomeWebSocketClient::new(
        dome_api_key,
        tracked_wallets.clone(),
    );
    
    // Spawn reconnection loop
    tokio::spawn(async move {
        ws_client.run().await
    });
    
    // Process orders as they arrive
    while let Some(order) = order_rx.recv().await {
        let wallet_label = wallet_labels.get(&order.user)
            .unwrap_or(&"unknown".to_string());
        
        info!(
            "💰 REALTIME: {} [{}] {} {} @ ${:.3}",
            &order.user[..10],
            wallet_label,
            order.side,
            order.shares_normalized,
            order.price
        );
        
        // Convert to signal and broadcast
        let signals = detector.detect_trader_entry(&order, wallet_label);
        for signal in signals {
            storage.store(&signal).await?;
            signal_tx.send(signal)?;
        }
    }
}
```

#### Unsubscribe

```json
{
  "action": "unsubscribe",
  "version": 1,
  "subscription_id": "sub_m58zfduokmd"
}
```

---

### 4.4 POLYMARKET API (GAMMA)

**Base URL:** `https://gamma-api.polymarket.com`  
**Authentication:** None (public API)  
**Rate Limit:** 750 requests per 10 seconds  

#### Key Endpoint: `/markets`

**Purpose:** Fetch market data with prices and metadata

**Request:**
```rust
let url = format!("{}/markets", GAMMA_API_BASE);
let mut params = HashMap::new();
params.insert("limit", "100".to_string());
params.insert("offset", "0".to_string());
// NOTE: Do NOT send "active" parameter - causes 422 error

let response = self.client
    .get(&url)
    .query(&params)
    .send()
    .await?;
```

**Response:**
```json
[
  {
    "id": "market_id_123",
    "condition_id": "0xabc123...",
    "question_id": "q_456",
    "slug": "will-bitcoin-reach-100k-by-2025",
    "question": "Will Bitcoin reach $100K by end of 2025?",
    "description": "Resolves YES if...",
    "end_date_iso": "2025-12-31T23:59:59Z",
    "volume": 125000.50,
    "liquidity": 50000.0,
    "outcome_prices": [0.65, 0.35],  // [YES, NO]
    "closed": false,
    "active": true
  }
]
```

**CRITICAL:** The API returns an **array directly**, not `{ data: [...] }`.

**Processing:**
```rust
pub async fn fetch_gamma_markets(&mut self, limit: usize, offset: usize) -> Result<Vec<GammaMarket>> {
    self.gamma_limiter.acquire().await;
    
    let url = format!("{}/markets", GAMMA_API_BASE);
    let mut params = HashMap::new();
    params.insert("limit", limit.to_string());
    params.insert("offset", offset.to_string());
    // ⚠️ DO NOT ADD: params.insert("active", "true"); // Causes 422 error!
    
    let response = self.execute_with_retry(&url, Some(&params)).await?;
    
    // Direct array deserialization
    let markets: Vec<GammaMarket> = response.json().await
        .context("Failed to parse GAMMA markets")?;
    
    Ok(markets)
}
```

---

## 5. FRONTEND ARCHITECTURE

### Directory Structure

```
frontend/
├── src/
│   ├── main.tsx                    # Entry point
│   ├── App.tsx                     # Main app component
│   ├── components/
│   │   ├── auth/
│   │   │   ├── LoginPage.tsx       # Login screen
│   │   │   └── AuthGuard.tsx       # Protected route wrapper
│   │   ├── effects/
│   │   │   ├── CRTEffect.tsx       # Retro CRT filter
│   │   │   ├── Scanlines.tsx       # Terminal scanlines
│   │   │   └── GlitchText.tsx      # Glitch animation
│   │   ├── layout/
│   │   │   ├── AppShell.tsx        # Main layout
│   │   │   └── StatusBar.tsx       # Bottom status bar
│   │   └── Terminal/
│   │       ├── SignalCard.tsx      # Individual signal display
│   │       ├── SignalFeed.tsx      # Signal stream
│   │       └── TerminalHeader.tsx  # Top header with stats
│   ├── hooks/
│   │   ├── useAuth.ts              # Authentication hook
│   │   ├── useSignals.ts           # Signal fetching
│   │   └── useWebSocket.ts         # WebSocket connection
│   ├── services/
│   │   ├── api.ts                  # REST API client
│   │   └── websocket.ts            # WebSocket client
│   ├── stores/
│   │   ├── authStore.ts            # Zustand auth store
│   │   └── signalStore.ts          # Zustand signal store
│   ├── types/
│   │   ├── auth.ts                 # Auth type definitions
│   │   └── signal.ts               # Signal type definitions
│   └── utils/
│       └── formatters.ts           # Display formatters
├── index.html
├── vite.config.ts
├── tailwind.config.js
└── package.json
```

### Key Type Definitions

#### Signal Types (`types/signal.ts`)

**MUST MATCH BACKEND EXACTLY:**

```typescript
export type SignalTypeVariant =
  | 'PriceDeviation'
  | 'MarketExpiryEdge'
  | 'WhaleFollowing'
  | 'EliteWallet'
  | 'InsiderWallet'
  | 'WhaleCluster'
  | 'CrossPlatformArbitrage'
  | 'TrackedWalletEntry';

export type SignalType =
  | { type: 'PriceDeviation'; market_price: number; fair_value: number; deviation_pct: number }
  | { type: 'MarketExpiryEdge'; hours_to_expiry: number; volume_spike: number }
  | { type: 'WhaleFollowing'; whale_address: string; position_size: number; confidence_score: number }
  | { type: 'EliteWallet'; wallet_address: string; win_rate: number; total_volume: number; position_size: number }
  | { type: 'InsiderWallet'; wallet_address: string; early_entry_score: number; win_rate: number; position_size: number }
  | { type: 'WhaleCluster'; cluster_count: number; total_volume: number; consensus_direction: string }
  | { type: 'CrossPlatformArbitrage'; polymarket_price: number; kalshi_price?: number; spread_pct: number }
  | { type: 'TrackedWalletEntry'; wallet_address: string; wallet_label: string; position_value_usd: number; order_count: number };

export interface Signal {
  id: string;
  signal_type: SignalType;  // ⚠️ Tagged union, not string!
  market_slug: string;
  confidence: number;
  detected_at: string;
  details: SignalDetails;
  source: string;
}

// Matches backend SignalDetails enrichment
export interface SignalDetails {
  market_title: string;
  current_price: number;
  volume_24h: number;
  liquidity?: number;
  recommended_action: string;
  position_size?: number;
  entry_price?: number;
  wallet_address?: string;
  wallet_tier?: string;
  win_rate?: number;
  spread?: number;
  expected_profit?: number;
  time_to_expiry?: string;
  dominant_side?: string;
  dominant_percentage?: number;
  expiry_time?: string | null;
  observed_timestamp?: string;
  signal_family?: string;
  calibration_version?: string;
  guardrail_flags?: string[];
  recommended_size?: number;
}
```

### WebSocket Connection (`services/websocket.ts`)

```typescript
export class WebSocketClient {
  private ws: WebSocket | null = null;
  private reconnectAttempts = 0;
  private maxReconnectAttempts = 10;
  private reconnectDelay = 1000; // Start at 1 second

  connect(url: string, onMessage: (signal: Signal) => void) {
    this.ws = new WebSocket(url);

    this.ws.onopen = () => {
      console.log('✅ WebSocket connected');
      this.reconnectAttempts = 0;
      this.reconnectDelay = 1000;
    };

    this.ws.onmessage = (event) => {
      try {
        const signal = JSON.parse(event.data) as Signal;
        onMessage(signal);
      } catch (error) {
        console.error('Failed to parse WebSocket message:', error);
      }
    };

    this.ws.onerror = (error) => {
      console.error('WebSocket error:', error);
    };

    this.ws.onclose = () => {
      console.log('WebSocket disconnected');
      this.reconnect(url, onMessage);
    };
  }

  private reconnect(url: string, onMessage: (signal: Signal) => void) {
    if (this.reconnectAttempts >= this.maxReconnectAttempts) {
      console.error('Max reconnection attempts reached');
      return;
    }

    this.reconnectAttempts++;
    console.log(`Reconnecting in ${this.reconnectDelay}ms (attempt ${this.reconnectAttempts})`);

    setTimeout(() => {
      this.connect(url, onMessage);
      this.reconnectDelay = Math.min(this.reconnectDelay * 2, 30000); // Cap at 30s
    }, this.reconnectDelay);
  }

  disconnect() {
    if (this.ws) {
      this.ws.close();
      this.ws = null;
    }
  }
}
```

---

## 6. DATA FLOW & PROCESSING

### Complete Signal Lifecycle

```
┌──────────────────────────────────────────────────────────────┐
│                    SIGNAL LIFECYCLE                           │
└──────────────────────────────────────────────────────────────┘

1. EXTERNAL API CALL
   │
   ├─► Hashdive: /get_latest_whale_trades
   ├─► DomeAPI: WebSocket stream
   └─► Polymarket: /markets
   │
   ▼

2. API RESPONSE PARSING
   │
   ├─► Parse JSON response
   ├─► Extract relevant fields
   └─► Handle errors gracefully
   │
   ▼

3. SIGNAL DETECTION
   │
   ├─► detector.detect_whale_activity()
   ├─► detector.detect_expiry_edge()
   └─► detector.detect_trader_entry()
   │
   ▼

4. SIGNAL CONSTRUCTION
   │
   ├─► Create SignalType enum
   ├─► Calculate confidence score
   └─► Add market metadata
   │
   ▼

5. RISK FILTERING
   │
   ├─► risk_manager.calculate_position() with calibration + guardrails
   ├─► Fractional Kelly (cap 0.20), regime scaling, drawdown throttle
   └─► Guardrail flags + calibrated confidence embedded in details
   │
   ▼

6. DATABASE STORAGE
   │
   ├─► SQLite INSERT
   ├─► Auto-increment ID
   └─► Timestamp recording
   │
   ▼

7. BROADCAST (WebSocket)
   │
   ├─► signal_tx.send()
   ├─► Broadcast to all clients
   └─► JSON serialization
   │
   ▼

8. FRONTEND RECEPTION
   │
   ├─► WebSocket onmessage
   ├─► JSON.parse()
   └─► Type validation
   │
   ▼

9. STATE UPDATE
   │
   ├─► Zustand store update
   ├─► React re-render
   └─► UI update (< 100ms)
```

### Example: Whale Trade Signal

**1. API Response (Hashdive):**
```json
{
  "data": [{
    "user_address": "<wallet_address>",
    "asset_id": "<asset_id>",
    "side": "BUY",
    "size": 50000.0,
    "price": 0.62,
    "timestamp": 1700000000
  }]
}
```

**2. Backend Processing:**
```rust
// Parse and create signal
let signal = MarketSignal {
    id: format!("whale_{}", trade.timestamp),
    signal_type: SignalType::WhaleFollowing {
        whale_address: trade.user_address.clone(),
        position_size: trade.size,
        confidence_score: (trade.size / 100000.0).min(0.99),
    },
    market_slug: "will-bitcoin-reach-100k-by-2025".to_string(),
    confidence: (trade.size / 50000.0).min(0.95),
    risk_level: "low".to_string(),
    details: SignalDetails {
        market_id: trade.asset_id,
        market_title: "Will Bitcoin reach $100K by 2025?".to_string(),
        current_price: trade.price,
        volume_24h: trade.size,
        liquidity: 0.0,
        recommended_action: "FOLLOW_BUY".to_string(),
        expiry_time: None,
        observed_timestamp: None,
        signal_family: None,
        calibration_version: None,
        guardrail_flags: None,
        recommended_size: None,
    },
    detected_at: chrono::Utc::now().to_rfc3339(),
    source: "hashdive".to_string(),
};

// Store in database
storage.store(&signal).await?;

// Broadcast via WebSocket
signal_tx.send(signal)?;
```

**3. JSON Serialization (Sent to Frontend):**
```json
{
  "id": "whale_1700000000",
  "signal_type": {
    "type": "WhaleFollowing",
    "whale_address": "<wallet_address>",
    "position_size": 50000.0,
    "confidence_score": 0.5
  },
  "market_slug": "will-bitcoin-reach-100k-by-2025",
  "confidence": 0.95,
  "risk_level": "low",
  "details": {
    "market_id": "77874905...",
    "market_title": "Will Bitcoin reach $100K by 2025?",
    "current_price": 0.62,
    "volume_24h": 50000.0,
    "liquidity": 0.0,
    "recommended_action": "FOLLOW_BUY",
    "expiry_time": null
  },
  "detected_at": "2025-11-16T20:00:00Z",
  "source": "hashdive"
}
```

**4. Frontend Display:**
```typescript
export const SignalCard: React.FC<{ signal: Signal }> = ({ signal }) => {
  // Type-safe access
  if (signal.signal_type.type === 'WhaleFollowing') {
    return (
      <div>
        <span>🐋 WHALE FOLLOWING</span>
        <div>
          Whale: {signal.signal_type.whale_address.slice(0, 10)}...
        </div>
        <div>
          Position: ${(signal.signal_type.position_size / 1000).toFixed(1)}K
        </div>
        <div>
          Confidence: {(signal.confidence * 100).toFixed(1)}%
        </div>
      </div>
    );
  }
};
```

---

## 7. COMMON PITFALLS & SOLUTIONS

### Pitfall #1: Forgetting to Start Both Servers

**SYMPTOM:** Frontend shows "DISCONNECTED" status

**SOLUTION:**
```bash
# Terminal 1 - Backend
cd rust-backend
cargo run

# Terminal 2 - Frontend
cd frontend
npm run dev
```

**Check:**
- Backend: http://localhost:3000/health
- Frontend: http://localhost:5173

---

### Pitfall #2: Invalid API Keys

**SYMPTOM:** Mock signals appear, no real data

**CHECK:**
```bash
cd rust-backend
cat .env | grep API_KEY
```

**SOLUTION:**
```bash
echo "HASHDIVE_API_KEY=<YOUR_HASHDIVE_API_KEY>" >> .env
echo "DOME_API_KEY=<YOUR_DOME_API_KEY>" >> .env
```

---

### Pitfall #3: Database Locked Error

**SYMPTOM:** `database is locked` error in backend logs

**CAUSE:** Multiple processes accessing SQLite simultaneously

**SOLUTION:**
```rust
// Add to db_storage.rs
let conn = Connection::open_with_flags(
    path,
    OpenFlags::SQLITE_OPEN_READ_WRITE |
    OpenFlags::SQLITE_OPEN_CREATE |
    OpenFlags::SQLITE_OPEN_NO_MUTEX  // Allow concurrent access
)?;

conn.execute("PRAGMA journal_mode=WAL", [])?;  // Write-Ahead Logging
```

---

### Pitfall #4: CORS Errors

**SYMPTOM:** Browser console shows CORS errors

**CAUSE:** Frontend and backend on different origins

**SOLUTION (Backend):**
```rust
use tower_http::cors::CorsLayer;

let app = Router::new()
    // ... routes ...
    .layer(CorsLayer::permissive());  // Allow all origins (dev only)
```

**PRODUCTION:**
```rust
use tower_http::cors::{CorsLayer, Any};

let cors = CorsLayer::new()
    .allow_origin("https://yourdomain.com".parse::<HeaderValue>().unwrap())
    .allow_methods([Method::GET, Method::POST])
    .allow_headers([AUTHORIZATION, CONTENT_TYPE]);

let app = Router::new()
    .layer(cors);
```

---

### Pitfall #5: Memory Leaks in WebSocket

**SYMPTOM:** Memory usage grows over time

**CAUSE:** Not cleaning up closed connections

**SOLUTION:**
```rust
async fn handle_socket(mut socket: WebSocket, state: AppState) {
    let mut rx = state.signal_broadcast.subscribe();

    loop {
        tokio::select! {
            Ok(signal) = rx.recv() => {
                let msg = serde_json::to_string(&signal).unwrap();
                if socket.send(Message::Text(msg)).await.is_err() {
                    // Connection closed, break loop to clean up
                    break;
                }
            }
            Some(Ok(Message::Close(_))) = socket.recv() => {
                // Client closed connection
                break;
            }
        }
    }
    
    // Cleanup happens here automatically
    drop(rx);
}
```

---

### Pitfall #6: Incorrect TypeScript Signal Access

**WRONG:**
```typescript
// ❌ Treating as string
if (signal.signal_type === 'WhaleFollowing') {
  // This will never be true!
}
```

**CORRECT:**
```typescript
// ✅ Access .type property
if (signal.signal_type.type === 'WhaleFollowing') {
  console.log(signal.signal_type.whale_address);  // Works!
}
```

---

## 8. TESTING & VERIFICATION

### Backend Health Checks

```bash
# 1. Server running?
curl http://localhost:3000/health

# Expected: "🚀 BetterBot Operational..."

# 2. Signals endpoint
curl http://localhost:3000/api/signals | jq .

# Expected: {"signals": [...], "count": 10}

# 3. Risk stats
curl http://localhost:3000/api/risk/stats | jq .

# Expected: {"current_balance": 10000.0, ...}

# 4. Login
curl -X POST http://localhost:3000/api/auth/login \
  -H "Content-Type: application/json" \
  -d '{"username":"admin","password":"admin123"}' | jq .

# Expected: {"token": "...", "user": {...}}
```

### Frontend Tests

```bash
# 1. Dev server running?
curl http://localhost:5173

# 2. Check browser console for errors
# Open: http://localhost:5173
# F12 → Console → Should see:
#   "✅ WebSocket connected"
#   "Logged in as admin"

# 3. Network tab
# F12 → Network → WS → Should see:
#   Connection to ws://localhost:3000/ws
#   Messages flowing
```

### Database Inspection

```bash
cd rust-backend

# Signals database
sqlite3 betterbot_signals.db "SELECT COUNT(*) FROM signals;"
sqlite3 betterbot_signals.db "SELECT * FROM signals LIMIT 5;"

# Auth database
sqlite3 betterbot_auth.db "SELECT username, role FROM users;"
```

---

## 9. DEPLOYMENT CHECKLIST

### Pre-Deployment

- [ ] All environment variables set
- [ ] API keys validated
- [ ] Database migrations run
- [ ] Frontend built (`npm run build`)
- [ ] Backend compiled in release mode (`cargo build --release`)
- [ ] CORS configured for production domain
- [ ] Default admin password changed
- [ ] JWT secret changed from default
- [ ] Rate limiting configured
- [ ] Logging level set appropriately
- [ ] Health check endpoint tested
- [ ] WebSocket endpoint tested
- [ ] SSL/TLS certificates installed

### Production Environment Variables

```bash
# rust-backend/.env
RUST_LOG=info
DATABASE_PATH=./betterbot_signals.db
AUTH_DB_PATH=./betterbot_auth.db
JWT_SECRET=<random-64-char-string>
HASHDIVE_API_KEY=<your-key>
DOME_API_KEY=<your-key>
INITIAL_BANKROLL=10000
KELLY_FRACTION=0.25
```

### Monitoring

**Logs to Watch:**
- Connection count
- Signal processing rate
- API error rates
- Database size growth
- Memory usage
- WebSocket reconnection frequency

**Alerts to Set:**
- Backend down (health check fails)
- Database errors
- API rate limit exceeded
- Memory usage > 80%
- Disk space < 20%

---

## 10. QUICK REFERENCE

### Start Commands

```bash
# Backend
cd rust-backend
cargo run

# Frontend
cd frontend
npm run dev
```

### Default Credentials

```
Username: admin
Password: admin123
```

**⚠️ CHANGE IN PRODUCTION!**

### API Endpoints

```
GET  /health                    - Health check
GET  /api/signals               - Get recent signals (optional: ?limit=, ?min_confidence=)
GET  /api/risk/stats            - Risk management stats (VaR/CVaR, bankroll, kelly)
POST /api/auth/login            - Login
GET  /ws                        - WebSocket endpoint
```

### File Locations

```
Backend:
  Code:      rust-backend/src/
  Database:  rust-backend/betterbot_signals.db
  Auth DB:   rust-backend/betterbot_auth.db
  Env:       rust-backend/.env
  
Frontend:
  Code:      frontend/src/
  Build:     frontend/dist/
  Env:       frontend/.env
```

### Port Reference

```
Backend:  3000
Frontend: 5173 (dev), 4173 (preview)
```

### Key Dependencies

**Backend:**
- axum (HTTP server)
- tokio (async runtime)
- rusqlite (database)
- serde_json (JSON)
- jsonwebtoken (JWT)
- bcrypt (password hashing)
- reqwest (HTTP client)
- tokio-tungstenite (WebSocket)

**Frontend:**
- react + typescript
- vite (build tool)
- tailwindcss (styling)
- zustand (state management)
- date-fns (date formatting)

---

## 🎯 SUMMARY FOR AGENTS

### Top 10 Things to Remember

1. **Type Mismatches:** Backend sends tagged unions, frontend must match exactly
2. **Auth Headers:** Hashdive uses `x-api-key`, DomeAPI uses `Authorization: Bearer`
3. **Rate Limits:** Hashdive has 1000 monthly credits - poll every 45 minutes
4. **WebSocket Reconnection:** Always implement exponential backoff
5. **Mock Fallback:** Only use when NO real signals available
6. **Login Response:** Must include user object for frontend display
7. **Polymarket 422:** Never send `active` parameter to GAMMA API
8. **Database Locking:** Use WAL mode for concurrent access
9. **CORS:** Configure properly for production domains
10. **Signal Flow:** API → Parse → Detect → Filter → Store → Broadcast → Display

### Most Common Mistakes

1. Using `X-API-Key` for DomeAPI (should be Bearer token)
2. Aggressive polling exhausting API credits
3. Forgetting WebSocket reconnection logic
4. Type mismatch between backend Rust enum and frontend TypeScript
5. Not handling API errors gracefully
6. Forgetting to start both backend AND frontend
7. Using default JWT secret in production
8. Not validating API responses before processing
9. Incorrect signal type checking in frontend (string vs object)
10. Database locked errors from concurrent access

---

## 🧠 INSTITUTIONAL‑GRADE QUANT ENHANCEMENTS

This section documents the institutional‑grade capabilities implemented across the system.

1) Data quality and gating
- Implemented: Source kill‑switches with env toggles and p95 latency monitors; trips on consecutive failures or SLO breach. (main.rs)
- Implemented: Data quality gate for stale/outlier drops and cooldowns. (signals/quality.rs + signals/detector.rs)
- Implemented: Staggered async polling (implicit jitter). (main.rs)

2) Signal research hygiene
- Implemented: Rolling walk‑forward with test embargo; no random shuffle. (backtest.rs)
- Implemented: Leakage controls, monotonic timestamp assertions, hygiene tracking. (backtest.rs)
- Implemented: Confidence calibration (isotonic) per family; calibration_version persisted. (risk.rs + main.rs + signals/db_storage.rs)

3) Portfolio construction and risk
- Implemented: Fractional Kelly capped at 0.20; regime risk factor scaling. (risk.rs)
- Implemented: Drawdown throttle (8% trigger / 4% release). (risk.rs)
- Implemented: Correlation awareness and de‑duplication in composite analysis. (signals/correlator.rs)
- Implemented: Per‑market sizing returned via details.recommended_size; hooks for theme caps. (risk.rs)

4) Execution realism (backtests and live)
- Implemented: Slippage/transaction costs; partial fill simulator; cooldown windows. (backtest.rs)
- Implemented: Live path embeds calibrated confidence and guardrail flags. (main.rs)

5) Monitoring and SLOs
- Implemented: Source‑level latency monitors with p95 enforcement; structured logs. (main.rs)
- Implemented: Risk stats API (VaR/CVaR, bankroll, kelly fraction, sample size). (GET /api/risk/stats)
- In progress: Attribution by family (db stores family + calibration; surface via API).

6) Security & secrets
- Implemented: No hardcoded API keys; env‑only; fail‑closed on missing keys. (main.rs + scrapers/*)
- Recommended: CI secret scanning and pre‑commit hooks (gitleaks). Provide .env.example only.

7) Remaining enhancements (targeted)
- Add `/api/signals/stats` by family for attribution dashboards.
- Expose theme‑level exposure caps via API/UI controls.
- Export Prometheus metrics (latency, throughput, error rates).

---

## 📞 NEED HELP?

This document should answer 95% of questions. For the remaining 5%:

1. Check backend logs: `rust-backend/target/debug/betterbot`
2. Check browser console: F12 → Console tab
3. Verify API responses: curl commands in Testing section
4. Review phase documentation: `docs/phases/PHASE_*_COMPLETE.md`
5. Check API documentation directly:
   - Hashdive: https://hashdive.com/API_documentation
   - DomeAPI: https://docs.domeapi.io/
   - Polymarket: https://gamma-api.polymarket.com/

---

**Last Updated:** November 17, 2025  
**Maintainers:** AI Agents & Contributors  
**License:** Proprietary  

**🚀 Happy Building!**

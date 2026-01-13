# AGENTS.MD - Complete Technical System Reference

**Last Updated:** January 13, 2026  
**Purpose:** Comprehensive technical reference for AI agents working on BetterBot  
**Status:** Signal pipeline + pooled vault (paper) operational; live execution/on-chain settlement still pending

---

## Table of Contents

1. [System Architecture Overview](#1-system-architecture-overview)
2. [Backend - Rust Architecture](#2-backend---rust-architecture)
3. [Frontend - React/TypeScript Architecture](#3-frontend---reacttypescript-architecture)
4. [External API Integration Guide](#4-external-api-integration-guide)
5. [Data Flow & Signal Pipeline](#5-data-flow--signal-pipeline)
6. [Database Schema & Optimization](#6-database-schema--optimization)
7. [Authentication System](#7-authentication-system)
8. [Risk Management & Kelly Criterion](#8-risk-management--kelly-criterion)
9. [Environment Configuration](#9-environment-configuration)
10. [Performance Optimizations](#10-performance-optimizations)

---

## 1. System Architecture Overview

BetterBot is a quantitative trading signal platform for Polymarket prediction markets. It tracks elite wallet activity, detects market inefficiencies, and provides real-time trading signals.

### Technology Stack

| Layer | Technology | Purpose |
|-------|------------|---------|
| Backend | Rust + Axum | High-performance async API server |
| Database | SQLite (WAL mode) | Signal persistence, 10M+ capacity |
| Frontend | React 18 + TypeScript | Terminal-style UI |
| State | Zustand | Lightweight state management |
| Styling | TailwindCSS | AMOLED-black theme |
| Real-time | WebSocket + REST polling | Hybrid signal delivery |

### Core Data Sources

| Source | Purpose | Rate Limit |
|--------|---------|------------|
| Polymarket GAMMA | Market data, events, prices | Generous |
| Polymarket CLOB | Orderbook snapshots (bid/ask/depth) + execution surface | Generous (cache + rate-limit) |
| Hashdive | Whale trades ($10k+) | 1000/month |
| DomeAPI REST | Wallet activity history | 100ms delay |
| DomeAPI WebSocket | Real-time wallet orders | Streaming |

### Current Status (Vault / Phase 8)

Implemented (paper trading + accounting-only APIs):
- **FAST15M engine:** deterministic BTC/ETH/SOL/XRP 15m Up/Down markets (Binance mid via `barter-data`, conservative `p_up` model, fractional Kelly sizing, Polymarket WS/REST orderbooks).
- **LONG engine (BRAID-bounded LLM):** OpenRouter `chat/completions` with **scout-first gating** + **3-of-4 consensus**; deterministic admissibility + cost-adjusted edge + conservative sizing; global/day budgets and per-market cadence.
- **Pooled vault accounting:** share-based NAV with `deposit()` / `withdraw()` and persistence.
- **UX:** 15m signals persist **lite** context only; live 15m enrichments are fetched on-demand; wallet analytics can be bulk-primed.

Not implemented yet (current focus):
- **Live execution** (`DomeExecutionAdapter`) once router endpoint + payload are available (idempotent client_order_id, cancel/replace, reconciliation).
- **On-chain settlement / real custody** for deposits/withdrawals (current APIs are accounting-only).
- **Multi-account routing** (multiple Polymarket accounts / sub-allocations) and full position valuation/resolution.

---

## 2. Backend - Rust Architecture

### Directory Structure

```
rust-backend/src/
├── main.rs           # Entry point, server setup, polling loops
├── models.rs         # Core data structures
├── risk.rs           # Kelly criterion position sizing
├── backtest.rs       # Historical signal analysis
├── api/              # HTTP REST endpoints
├── auth/             # JWT authentication
├── scrapers/         # External API integrations
├── signals/          # Signal detection & storage
├── vault/            # Auto-trading infrastructure
└── arbitrage/        # Cross-platform arbitrage
```

### File-by-File Reference

#### `main.rs`
**Purpose:** Application entry point, async runtime orchestration

**Key Components:**
- `AppState` - Shared state (storage, risk manager, broadcast channel, HTTP client, Polymarket WS cache, Binance price feed, pooled vault)
- `DataSourceKillSwitch` - Health monitoring with auto-disable
- `parallel_data_collection()` - 45-minute Polymarket/Hashdive/Dome polling
- `tracked_wallet_polling()` - Dome WebSocket + REST for tracked wallets (≈357 base + CSV/manual extensions; see `Config::from_env()`)
- `expiry_edge_polling()` - 60-second market expiry scanner
- `wallet_analytics_polling()` - Warms wallet analytics caches for recently-active wallets
- `storage_pruning_polling()` - Prunes old `dome_order_events` to bound DB growth
- `websocket_handler()` - Client signal streaming

**Concurrency Model:**
```rust
// Uses parking_lot::RwLock instead of tokio::RwLock for faster short critical sections
let risk_manager: Arc<ParkingRwLock<RiskManager>>

// Batch processing with pre-allocated vectors
let processed_signals: Vec<MarketSignal> = qualified_signals
    .par_iter()  // Rayon parallel iterator
    .filter_map(|signal| { ... })
    .collect();
```

---

#### `models.rs`
**Purpose:** Core data types + enrichment context + runtime config.

**Key points:**
- `MarketSignal.source` defaults to `"detector"` (older signals may have `"polymarket"|"hashdive"|"dome"`).
- `SignalType` is `#[serde(tag = "type")]` (frontend relies on the tagged union shape).
- `TrackedWalletEntry` includes an optional `token_label` ("Yes/No", "Up/Down") which the UI uses for outcome semantics.

**Tracked wallets (current behavior):**
- Base list lives in `Config::default_tracked_wallets()` (≈357 curated wallets + `high_frequency_test`).
- `Config::from_env()` *also* merges:
  - repo-root `more_insiders.csv` (override path via `MORE_INSIDERS_CSV_PATH`)
  - a small manual list embedded in `models.rs`
  - merge uses `entry().or_insert()` so existing classifications are not overridden
- In practice this yields ~384 tracked wallets depending on duplicates.
- Full override: `TRACKED_WALLETS_JSON` (JSON map `{ "0x...": "label" }`) is applied first, then CSV/manual are merged on top.

**Label taxonomy (used across backend + UI):**
`insider_crypto`, `insider_finance`, `insider_politics`, `insider_tech`, `insider_entertainment`, `insider_sports`, `insider_other`, `high_frequency_test`.

---

#### `risk.rs`
**Purpose:** Risk management and position sizing

**Kelly Criterion Implementation:**
```rust
pub struct RiskManager {
    pub kelly: KellyCalculator,
    pub var: VaRCalculator,
    calibration: HashMap<String, f64>,  // signal_family -> historical accuracy
}

pub struct RiskInput {
    pub market_probability: f64,
    pub signal_confidence: f64,
    pub market_liquidity: f64,
    pub signal_family: String,
    pub regime_risk: Option<f64>,
}

pub struct RiskOutput {
    pub position_size: f64,
    pub calibrated_confidence: f64,
    pub calibration_version: String,
    pub guardrail_flags: Vec<String>,
}
```

**Guardrails:**
- Max bet: 5% of bankroll
- Min liquidity: $1,000
- Max leverage: 2x
- Signal family calibration adjustments

---

### API Module (`api/`)

#### `simple.rs` - Main API handlers
```rust
// GET /api/signals?limit=100
pub async fn get_signals_simple(
    Query(params): Query<SignalQuery>,
    AxumState(state): AxumState<AppState>,
) -> Json<SignalResponse>

// GET /api/signals/search?q=...&limit=...&before=...&before_id=...
pub async fn get_signals_search(
    Query(params): Query<SignalSearchQuery>,
    AxumState(state): AxumState<AppState>,
) -> Result<Json<SignalResponse>, (StatusCode, String)>

// GET /api/signals/search/status
pub async fn get_signals_search_status(
    AxumState(state): AxumState<AppState>,
) -> Json<SignalSearchStatusResponse>

// GET /api/signals/stats
pub async fn get_signal_stats(
    AxumState(state): AxumState<AppState>,
) -> Json<SignalStatsResponse>

// GET /api/risk/stats
pub async fn get_risk_stats_simple(
    AxumState(state): AxumState<AppState>,
) -> Json<RiskStatsResponse>

// GET /api/vault/state
pub async fn get_vault_state(
    AxumState(state): AxumState<AppState>,
) -> Json<VaultStateResponse>

// POST /api/vault/deposit
pub async fn post_vault_deposit(
    AxumState(state): AxumState<AppState>,
    AxumJson(req): AxumJson<VaultDepositRequest>,
) -> Result<Json<VaultDepositResponse>, StatusCode>

// POST /api/vault/withdraw
pub async fn post_vault_withdraw(
    AxumState(state): AxumState<AppState>,
    AxumJson(req): AxumJson<VaultWithdrawRequest>,
) -> Result<Json<VaultWithdrawResponse>, StatusCode>

// GET /api/signals/enrich?signal_id=...&levels=10&fresh=true
pub async fn get_signal_enrich(
    Query(params): Query<SignalEnrichQuery>,
    AxumState(state): AxumState<AppState>,
) -> Result<Json<SignalEnrichResponse>, StatusCode>

// POST /api/wallet/analytics/prime
pub async fn post_wallet_analytics_prime(
    AxumState(state): AxumState<AppState>,
    AxumJson(req): AxumJson<WalletAnalyticsPrimeRequest>,
) -> Result<Json<WalletAnalyticsPrimeResponse>, StatusCode>
```

`GET /api/signals` notes:
- Default returns **lite** `SignalContext` for each signal (keeps payloads small).
- Pass `full_context=true` to return full context blobs (debugging only).

#### Routes (wired in `main.rs`)
```
GET  /health              - Health check
POST /api/auth/login      - JWT login
POST /api/auth/privy      - Privy login (optional)
GET  /api/auth/me         - Current user (protected)
GET  /api/signals         - Signal list (protected)
GET  /api/signals/search  - Full-history search (FTS5) (protected)
GET  /api/signals/search/status - Search index status / backfill progress (protected)
GET  /api/signals/context - Per-signal enrichment blob (protected)
GET  /api/signals/enrich  - Ephemeral 15m Up/Down enrichment (protected)
GET  /api/signals/stats   - Signal statistics (protected)
GET  /api/market/snapshot - Orderbook snapshot + depth/imbalance (protected)
GET  /api/wallet/analytics - Wallet + copy-trade analytics (protected)
POST /api/wallet/analytics/prime - Bulk pre-warm wallet analytics caches (protected)
GET  /api/vault/state     - Pooled vault state (accounting-only) (protected)
POST /api/vault/deposit   - Mint shares for deposit (accounting-only) (protected)
POST /api/vault/withdraw  - Burn shares for withdrawal (accounting-only) (protected)
POST /api/trade/order     - One-click trade (feature-flagged; paper/live) (protected)
GET  /api/risk/stats      - Risk statistics (protected)
GET  /ws                  - WebSocket upgrade (protected)
```

---

### Scrapers Module (`scrapers/`)

#### `dome_websocket.rs` - Real-time wallet streaming
**Purpose:** Sub-second latency order updates

```rust
pub struct DomeWebSocketClient {
    api_key: String,
    tracked_wallets: Vec<String>,
    order_tx: mpsc::UnboundedSender<WsOrderData>,
}

// Connection URL format
const DOME_WS_BASE: &str = "wss://ws.domeapi.io";
let ws_url = format!("{}/{}", DOME_WS_BASE, self.api_key);

// Subscription message format
#[derive(Serialize)]
pub struct WsSubscribeMessage {
    pub action: String,        // "subscribe"
    pub platform: String,      // "polymarket"
    pub version: i32,          // 1
    #[serde(rename = "type")]
    pub msg_type: String,      // "orders"
    pub filters: WsFilters,    // { users: ["0x..."] }
}

// Incoming order event format
#[derive(Deserialize)]
pub struct WsOrderUpdate {
    #[serde(rename = "type")]
    pub msg_type: String,           // "event"
    pub subscription_id: String,    // "sub_m58zfduokmd"
    pub data: WsOrderData,
}

pub struct WsOrderData {
    pub token_id: String,
    pub side: String,               // "BUY" or "SELL"
    pub market_slug: String,
    pub shares_normalized: f64,     // Actual share count
    pub price: f64,
    pub timestamp: i64,
    pub user: String,               // Wallet address
}
```

**Auto-reconnect with exponential backoff:**
```rust
loop {
    match self.connect_and_stream().await {
        Ok(_) => reconnect_delay = Duration::from_secs(1),
        Err(e) => {
            sleep(reconnect_delay).await;
            reconnect_delay = (reconnect_delay * 2).min(Duration::from_secs(60));
        }
    }
}
```

**CRITICAL - TLS Configuration:**
```toml
# Cargo.toml - REQUIRED for WebSocket SSL
tokio-tungstenite = { version = "0.24", features = ["native-tls"] }
```

---

#### `dome_realtime.rs` - REST polling fallback
**Purpose:** Backup when WebSocket fails, incremental polling

```rust
pub struct DomeRealtimeClient {
    client: Client,
    tracked_wallets: HashMap<String, String>,
    last_poll: Arc<Mutex<HashMap<String, i64>>>,  // Per-wallet timestamps
}

// Optimized client setup
let client = Client::builder()
    .timeout(Duration::from_secs(30))
    .pool_max_idle_per_host(10)
    .pool_idle_timeout(Duration::from_secs(90))
    .tcp_keepalive(Duration::from_secs(60))
    .build();

// Incremental polling - only fetch new orders
let url = format!(
    "https://api.domeapi.io/v1/polymarket/orders?user={}&start_time={}&limit=100",
    wallet_address,
    last_poll_timestamp
);
```

**Hybrid Loop Pattern (main.rs):**
```rust
loop {
    tokio::select! {
        // WebSocket message - instant
        Some(order) = order_rx.recv() => {
            let signals = detector.detect_trader_entry(&[order], &wallet, &label);
            storage.store_batch(&signals).await?;
            signal_tx.send(signal)?;
        }
        
        // REST fallback - every 60s
        _ = poll_interval.tick() => {
            let signals = rest_client.poll_all_wallets().await?;
            storage.store_batch(&signals).await?;
        }
    }
}
```

---

#### `hashdive_api.rs` - Whale trade data
**Purpose:** Large position tracking ($10k+ trades)

```rust
pub struct HashdiveScraper {
    client: Client,
    api_key: String,
    rate_limiter: RateLimiter,  // 2s between requests
    credits_used: u32,
    credits_limit: u32,         // 1000/month
}

// API endpoint
const HASHDIVE_API_BASE: &str = "https://hashdive.com/api";

// Request with authentication
let response = self.client
    .get(&format!("{}/get_latest_whale_trades", HASHDIVE_API_BASE))
    .header("x-api-key", &self.api_key)
    .query(&[("min_usd", "10000"), ("limit", "50")])
    .send()
    .await?;

// Rate limiting: 45-minute intervals = ~960 requests/month
let mut interval_timer = interval(Duration::from_secs(2700));
```

**Wallet Classification:**
```rust
pub enum WalletClassification {
    Elite { win_rate: f64, total_volume: f64, avg_trade_size: f64 },
    Insider { win_rate: f64, early_entry_score: f64, total_volume: f64 },
    Whale { total_volume: f64, win_rate: f64 },
    Regular,
}

// Thresholds
const ELITE_VOLUME_THRESHOLD: f64 = 100_000.0;    // $100k+ volume
const ELITE_WIN_RATE_THRESHOLD: f64 = 0.65;       // 65%+ win rate
const INSIDER_WIN_RATE_THRESHOLD: f64 = 0.70;     // 70%+ win rate
const INSIDER_EARLY_THRESHOLD: f64 = 0.75;        // 75%+ early entry
```

---

#### `polymarket_api.rs` - Market data
**Purpose:** Event/market data from GAMMA API

```rust
const GAMMA_API_BASE: &str = "https://gamma-api.polymarket.com";

// Fetch markets with pagination
pub async fn fetch_gamma_markets(&mut self, limit: u32, offset: u32) -> Result<GammaResponse> {
    let url = format!("{}/markets?limit={}&offset={}", GAMMA_API_BASE, limit, offset);
    self.client.get(&url).send().await?.json().await
}

// Convert to internal event structure
pub fn gamma_to_events(&self, response: GammaResponse) -> Vec<PolymarketEvent>
```

---

#### `expiry_edge.rs` - Near-expiry scanner
**Purpose:** 95% win rate on high-probability expiring markets

```rust
pub struct ExpiryEdgeScanner {
    max_hours: f64,           // 4.0 hours
    min_probability: f64,     // 0.80 (80%)
    min_volume: f64,          // $10,000
}

// Scan every 60 seconds for markets ≤4 hours from expiry
// with >80% probability and sufficient volume
```

---

### Signals Module (`signals/`)

#### `db_storage.rs` - SQLite persistence
**Purpose:** High-performance signal storage (10M+ capacity)

```rust
pub struct DbSignalStorage {
    conn: Arc<Mutex<Connection>>,  // parking_lot::Mutex for speed
}

// Optimized schema
const SCHEMA_SQL: &str = r#"
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;
PRAGMA cache_size = -64000;          -- 64MB cache
PRAGMA mmap_size = 268435456;        -- 256MB memory-mapped I/O

CREATE TABLE signals (
    id TEXT PRIMARY KEY,
    signal_type TEXT NOT NULL,
    market_slug TEXT NOT NULL,
    confidence REAL NOT NULL,
    risk_level TEXT NOT NULL,
    details_json TEXT NOT NULL,
    detected_at TEXT NOT NULL,
    source TEXT NOT NULL
) WITHOUT ROWID;  -- Clustered on PRIMARY KEY

CREATE TABLE metadata (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
) WITHOUT ROWID;

-- Covering indexes for common queries
CREATE INDEX idx_signals_recent ON signals(detected_at DESC, ...);
CREATE INDEX idx_signals_high_conf ON signals(detected_at DESC) WHERE confidence >= 0.7;
"#;

// Batch insert for performance
pub async fn store_batch(&self, signals: &[MarketSignal]) -> Result<usize> {
    let conn = self.conn.lock();
    conn.execute("BEGIN IMMEDIATE", [])?;
    
    for signal in signals {
        conn.execute("INSERT OR IGNORE INTO signals ...", params![...])?;
    }
    
    conn.execute("COMMIT", [])?;
}
```

---

#### `detector.rs` - Signal detection
**Purpose:** Convert raw data to actionable signals

```rust
pub struct SignalDetector {
    confidence_threshold: f64,  // 0.6
}

impl SignalDetector {
    // Detect from Polymarket events
    pub async fn detect_all(&self, events: &[PolymarketEvent]) -> Vec<MarketSignal>;
    
    // Detect from wallet orders (Dome API)
    pub fn detect_trader_entry(
        &self,
        orders: &[DomeOrder],
        wallet_address: &str,
        wallet_label: &str,
    ) -> Vec<MarketSignal>;
}
```

**Confidence Calculation:**
- Base: 85% for tracked wallets
- Size bonus: Up to +10% for large positions ($10k+ = max bonus)
- Capped at 95%

---

#### `quality.rs` - Signal filtering
**Purpose:** Drop low-quality signals before storage

```rust
pub struct SignalQualityGate {
    max_age: Duration,         // 3 seconds
    zscore_threshold: f64,     // 8.0 standard deviations
}

// Filters:
// 1. Signals older than 3 seconds (stale)
// 2. Confidence below threshold (low quality)
// 3. Corroboration check (multiple sources = higher trust)
```

---

### Vault Module (`vault/`)

**Phase 8 (Option A): pooled vault + automated trading (paper-first).**

Key files:
- `engine.rs` - Orchestrates FAST15M + LONG engine loops, ingests wallet-entry signals, enforces cadence/budgets, and places orders via `ExecutionAdapter`.
- `updown15m.rs` - 15m market slug parsing + driftless lognormal `p_up` + shrink-to-half conservatism.
- `llm.rs` - Bounded Decision DSL parser + OpenRouter client (env-keyed; no secrets in code).
- `execution.rs` - `ExecutionAdapter` + `PaperExecutionAdapter` (fills instantly at limit price) + `DomeExecutionAdapter` placeholder.
- `paper_ledger.rs` - Cash + positions ledger; supports BUY/SELL (paper).
- `pool.rs` - Share accounting (`deposit`/`withdraw`) + approximate NAV.
- `vault_db.rs` - SQLite persistence for vault state + per-wallet shares (separate DB from `betterbot_signals.db`).

#### `kelly.rs` - Position sizing
**Purpose:** Optimal bet sizing using Kelly Criterion

```rust
pub fn calculate_kelly_position(
    confidence: f64,      // Our probability estimate
    market_price: f64,    // Implied probability (market price)
    params: &KellyParams,
) -> KellyResult {
    // Edge = our confidence - market price
    let edge = confidence - market_price;
    
    // Kelly formula: f* = (p * odds - q) / odds
    let odds = (1.0 / market_price) - 1.0;
    let p = confidence;
    let q = 1.0 - p;
    let full_kelly = (p * odds - q) / odds;
    
    // Apply fractional Kelly (0.25x default)
    let actual_fraction = full_kelly * params.kelly_fraction;
    
    // Cap at max position (10% of bankroll)
    let position_usd = params.bankroll * actual_fraction.min(params.max_position_pct);
    
    KellyResult { position_size_usd: position_usd, ... }
}
```

---

## 3. Frontend - React/TypeScript Architecture

### Directory Structure

```
frontend/src/
├── main.tsx              # React entry point
├── App.tsx               # Root component
├── components/
│   ├── auth/             # Login, auth guard
│   ├── layout/           # App shell, status bar
│   └── terminal/         # Signal feed, cards, header
├── hooks/                # Custom React hooks
├── services/             # API & WebSocket clients
├── stores/               # Zustand state management
├── types/                # TypeScript definitions
└── utils/                # Formatting utilities
```

### File-by-File Reference

#### `hooks/useSignals.ts` - Signal fetching
**Purpose:** Polling with concurrent request prevention + adaptive polling when WS is healthy

```typescript
export const useSignals = (opts?: { wsConnected?: boolean }) => {
  const isMounted = useRef(true);
  const isLoadingRef = useRef(false);

  const loadSignals = useCallback(async () => {
    if (isLoadingRef.current) return;  // Prevent concurrent requests
    isLoadingRef.current = true;

    const response = await api.getSignals({ limit: 500 });
    setSignals(response.signals); // IMPORTANT: merges-by-id to hydrate WS replay
    
    isLoadingRef.current = false;
  }, []);

  useEffect(() => {
    // When WS is connected, REST becomes a safety net (reduce thrash)
    const pollMs = opts?.wsConnected ? 5000 : 500;
    const signalInterval = setInterval(loadSignals, pollMs);
    const statsInterval = setInterval(loadStats, 10000);     // 10s for stats
    return () => { clearInterval(...) };
  }, [loadSignals, opts?.wsConnected]);
};
```

---

#### `services/api.ts` - REST client
**Purpose:** HTTP API with cached headers

```typescript
class ApiClient {
  private token: string | null = null;
  private cachedHeaders: Record<string, string> | null = null;

  private getHeaders(): Record<string, string> {
    if (this.cachedHeaders) return this.cachedHeaders;
    const token = this.getToken();
    this.cachedHeaders = {
      'Content-Type': 'application/json',
      ...(token ? { 'Authorization': `Bearer ${token}` } : {}),
    };
    return this.cachedHeaders;
  }

  async getSignals(params?: { limit?: number }): Promise<{ signals: Signal[] }> {
    return this.fetch('/api/signals' + queryString);
  }
}
```

Notes:
- `fetch()` supports an external `AbortSignal` and distinguishes timeout vs abort; errors include HTTP status + response body when available.
- Full-history search endpoints:
  - `searchSignals()` → `GET /api/signals/search`
  - `searchSignalsStatus()` → `GET /api/signals/search/status` (short timeout, used for index/backfill progress)

---

#### `services/websocket.ts` - WebSocket client
**Purpose:** Real-time signal streaming with auto-reconnect

```typescript
export class WebSocketClient {
  private ws: WebSocket | null = null;
  private reconnectAttempts = 0;
  private maxReconnectAttempts = 5;

  connect() {
    const token = localStorage.getItem('betterbot_token');
    const wsUrl = token ? `${WS_URL}?token=${token}` : WS_URL;
    
    this.ws = new WebSocket(wsUrl);
    
    this.ws.onmessage = (event) => {
      const message = JSON.parse(event.data);
      if (message.type === 'signal') {
        this.emit('signal', message.data);
      }
    };
    
    this.ws.onclose = () => this.attemptReconnect();
  }

  private attemptReconnect() {
    if (this.reconnectAttempts < this.maxReconnectAttempts) {
      setTimeout(() => this.connect(), 1000 * ++this.reconnectAttempts);
    }
  }
}
```

---

#### `stores/signalStore.ts` - State management
**Purpose:** Merge-by-id hydration (REST + WS), batched WS replay, 24h retention

```typescript
export const useSignalStore = create<SignalStore>((set) => ({
  signals: [],

  // WS replay can deliver signals before REST hydration; always MERGE by id.
  // Context merge is context_version-aware.
  setSignals: (signals) => set((state) => {
    const byId = new Map(state.signals.map(s => [s.id, s]));
    for (const s of signals) {
      const existing = byId.get(s.id);
      byId.set(s.id, existing ? mergeSignal(existing, s) : s);
    }
    return { signals: trimSignalsToWindow(sortSignalsNewestFirst([...byId.values()])) };
  }),

  // Batch insert for WS replay to reduce UI thrash
  addSignals: (signals) => set((state) => { ...merge+trim... }),
}));
```

---

#### `components/terminal/TerminalHeader.tsx` - Terminal header + navigation
**Purpose:** Make the terminal feel like a complete dApp shell: centered brand, consistent spacing, and quick access to views/controls.

Key behavior:
- 2-row layout: **logo row** (centered, extra breathing room) + **controls row** (nav, stats, latency, user/exit).
- Logo scaled up (~30%) and typography/spacing tuned for a less “clinical dashboard” feel.

---

#### `components/terminal/SignalSearch.tsx` - Always-visible search bar
**Purpose:** One search entry point for both server (full-history) and local (loaded window) search.

Key behavior:
- Input + clear button + match counter.
- Optional banner messaging for server/indexing issues.
- Shows `LOCAL` badge when in fallback mode.

#### `components/terminal/SignalFeed.tsx` - Feed + Inspector orchestration
**Purpose:** Dense horizontal feed with a right-side Inspector drawer (no vertical expanding cards)

**Key behavior:**
- Feed renders `SignalCardCompact` rows.
- Search is always visible via `SignalSearch`.
  - **Server mode:** queries `/api/signals/search` (FTS5, full-history) with pagination.
  - **Local mode:** substring match over the loaded feed window (up to 24h) when server search is unavailable; auto-heals back to server when `/api/signals/search/status` indicates the schema is ready.
  - Uses debounce + AbortController cancellation to prevent request storms.
- 24h scrollback: scrolling near the bottom pages older signals via `GET /api/signals?before=<detected_at>&before_id=<id>`.
- REST hydration uses **lite** signal context by default; `GET /api/signals?full_context=true` is for debugging only (can be very large).
- Filters are resilient to Up/Down dominance: if filters produce 0 matches in the currently loaded window, the feed auto-pages backward (up to 24h) until matches appear or the 24h cutoff is reached.
- Clicking `[OPEN]`/`[DETAILS]`/`[BOOK]`/`[TRADE]` opens the Inspector drawer to that tab.
- Inspector is fixed-width with internal scroll (higher information density).

---

#### `components/terminal/SignalCardCompact.tsx` - Primary signal row
**Purpose:** HFT-style dense, horizontal card with semantic metric colors + explicit `[TRADE]` CTA

- Uses `formatDelta(bps, usd)` to display `Δ: +XXbps / +$0.0X`.
- Uses `metricColorClass(value)` for PnL/Δ/Sharpe/ROE coloring.

---

#### `components/terminal/SignalInspectorDrawer.tsx` - Right-side inspector (DETAILS/PERFORMANCE/BOOK/TRADE)
**Purpose:** Progressive disclosure without layout thrash; “never blank” panels.

**Reliability contract:**
- Always renders a bounded container: skeleton → live/cached/stale/error.
- PERFORMANCE tab loads wallet analytics (realized curve + copy curve). It supports:
  - `friction_mode`: OPT/BASE/PESS
  - `copy_model`: SCALED (default) / MTM (slower, more realistic)
  - longer timeouts + “still computing… retry” UX for cold caches
- Prefetches wallet analytics on drawer open (and the feed may `cached_only=true` prefetch for hot wallets).
- Prefetches book snapshot on BOOK/TRADE tabs.
- In-flight de-dupe + TTL caches to avoid request storms.

**TRADE tab (feature-flagged):**
- UI for side (BUY/SELL), notional, order type (GTC/FAK/FOK), price mode (JOIN/CROSS/CUSTOM), ARM→SUBMIT.
- Requires `VITE_ENABLE_TRADING=true` to enable the UI.

---

#### `components/terminal/SignalCard.tsx` - Legacy full card
Still present in the repo, but the primary UX is now `SignalCardCompact` + `SignalInspectorDrawer`.

---

## 4. External API Integration Guide

### DomeAPI WebSocket - Primary Real-time Feed

**Connection:**
```
URL: wss://ws.domeapi.io/${DOME_API_KEY}
Auth: (WS) token in URL path; some deployments may also accept `Authorization: Bearer ...`
Protocol: WSS (TLS required)
```

**Subscribe to wallet orders:**
```json
{
  "action": "subscribe",
  "platform": "polymarket",
  "version": 1,
  "type": "orders",
  "filters": {
    "users": ["0x1234...", "0x5678..."]
  }
}
```

**Incoming order event:**
```json
{
  "type": "event",
  "subscription_id": "sub_m58zfduokmd",
  "data": {
    "token_id": "57564352641769637...",
    "side": "BUY",
    "market_slug": "btc-updown-15m-1762755300",
    "shares": 5000000,
    "shares_normalized": 5.0,
    "price": 0.54,
    "timestamp": 1762755335,
    "user": "0x6031b6eed..."
  }
}
```

### DomeAPI REST - Fallback Polling

**Endpoint:** `https://api.domeapi.io/v1/polymarket/orders`

**Request:**
```bash
curl -H "Authorization: Bearer ${DOME_API_KEY}" \
  "https://api.domeapi.io/v1/polymarket/orders?user=0x1234...&start_time=1700000000&limit=100"
```

**Response:**
```json
{
  "orders": [
    {
      "token_id": "12345...",
      "side": "BUY",
      "market_slug": "will-bitcoin-reach-100k",
      "shares_normalized": 100.5,
      "price": 0.65,
      "timestamp": 1700000123,
      "user": "0x1234..."
    }
  ]
}
```

### DomeAPI REST Enrichment (Signal Context)

**Goal:** Keep Dome WebSocket as the *primary* low-latency feed, but asynchronously enrich each tracked-wallet order signal with extra context from Dome REST endpoints (market metadata, prices, orderbook snapshots, activity, candles, wallet mapping, wallet PnL).

#### High-level behavior

1. A tracked-wallet order arrives via Dome WebSocket.
2. BetterBot immediately emits a normal signal event to the frontend (no waiting).
3. A background enrichment worker fetches REST context and emits a follow-up `signal_context` event that the frontend merges into the existing signal card.

#### WebSocket server message format

The backend now broadcasts a tagged union (`WsServerEvent`) over `/ws`:

```json
{ "type": "signal", "data": { /* MarketSignal */ } }
```

```json
{ "type": "signal_context", "data": { /* SignalContextUpdate */ } }
```

`SignalContextUpdate` fields:
- `signal_id`: matches the original `MarketSignal.id`
- `context_version`: monotonically increasing version per signal
- `enriched_at`: unix seconds
- `status`: `ok | partial | failed`
- `context`: `SignalContext`

#### Stable signal IDs for Dome tracked-wallet orders

Tracked-wallet order signals use a stable id so enrichment can be joined deterministically:

```
dome_order_{order_hash}   (fallback: tx_hash)
```

#### Storage

SQLite tables added in `signals/db_storage.rs`:
- `signal_context` (upserted JSON blob keyed by `signal_id`)
- `dome_order_events` (lossless raw WS payloads keyed by `order_hash`)
- `dome_cache` (DB-backed cache for market/wallet/PnL payloads)

API endpoint:
- `GET /api/signals/context?signal_id=...` (returns stored `SignalContextRecord`)

#### Enrichment pipeline implementation

Files:
- `rust-backend/src/scrapers/dome_rest.rs` — typed REST client for enrichment endpoints
- `rust-backend/src/signals/enrichment.rs` — bounded queue + worker pool; parallel REST fetches; DB persistence; WS broadcast

Concurrency + rate limiting controls:
- Global request semaphore + separate semaphore for heavy endpoints (orderbooks/candles)
- DB-backed caching TTLs:
  - markets: 30 min
  - wallet mapping: 24 h
  - wallet PnL: 1 h

Enrichment tuning env vars (optional):
```bash
DOME_ENRICH_WORKERS=2
DOME_ENRICH_QUEUE_SIZE=2000
DOME_ENRICH_MAX_CONCURRENT_REQUESTS=8
DOME_ENRICH_MAX_CONCURRENT_HEAVY_REQUESTS=2
```

### Wallet Analytics + Copy Curves (cached + SWR)

**Goal:** fast, defensible wallet performance + copy curves without blocking the HFT path.

**Curves:**
1. **Wallet (realized)**: Dome `GET /polymarket/wallet/pnl/{wallet}` (`granularity=day`), normalized so the first day starts at `0`.
2. **Copy curve** (choose via `copy_model`):
   - `scaled` (default): scales the wallet realized curve to the follower’s fixed sizing and subtracts execution costs derived from local order flow.
   - `mtm`: trade-replay backtest with daily mark-to-market using price history (slower; more realistic).

#### API endpoint

`GET /api/wallet/analytics?wallet_address=0x...&friction_mode=base&copy_model=scaled&force=false&cached_only=false`

Query params:
- `friction_mode`: `optimistic|base|pessimistic`
- `copy_model`: `scaled|mtm`
- `cached_only=true` → returns **204** when missing (used for safe background prefetch)
- `force=true` bypasses the TTL (use sparingly)

#### Storage & caching

- SQLite cache table: `dome_cache`
- Cache key: `wallet_analytics_v5:{wallet}:{friction_mode}:{copy_model}`
- TTL: **900s**
- SWR: if cached exists and `force=false`, serve immediately; stale entries are refreshed in the background. On upstream failures: fall back to stale cache.

#### Execution costs (“Friction”)

- UI label: **Execution Costs (assumed)**
- Costs are computed as `friction_pct × traded_notional` across BUY+SELL fills.
- Costs are already subtracted from the copy curve and also surfaced as `copy_total_friction_usd`.

#### Metrics (per curve)

- `*_roe_pct`: `total_pnl_usd / denom_usd × 100`
- `*_win_rate`: fraction of positive daily deltas
- `*_profit_factor`: gross_profit / gross_loss (capped)
- Sharpe uses daily bucketing + missing-day fill + winsorization for stability.

#### Frontend rendering notes

PERFORMANCE tab shows wallet + copy curves with axis min/max + date labels, plus stats tiles (ROE%, WR, Profit Factor).

#### Background refresh + pruning

- `wallet_analytics_polling` periodically warms **scaled** analytics for recently-active wallets for OPT/BASE/PESS.
- `storage_pruning_polling` prunes old `dome_order_events` (`DOME_ORDER_EVENTS_RETENTION_DAYS`, `STORAGE_PRUNE_POLL_SECS`) to keep the DB bounded.

### Market Snapshot / Orderbook (Now) - CLOB Token ID Pitfall

Orderbook snapshots are fetched via backend:

- `GET /api/market/snapshot?token_id=<clobTokenId>`
  - or `GET /api/market/snapshot?market_slug=<slug>&outcome=<Up|Down|Yes|No>`

**Critical pitfall:** Polymarket CLOB `/book` expects an outcome-level **`clobTokenId`** (large integer string).

However:
- Dome order payloads include:
  - `condition_id` = Polymarket conditionId (0x… hex)
  - `token_id` = often a numeric string (outcome token id)
- Many internal code paths historically tried to feed `condition_id` into CLOB `/book` → guaranteed failure.

**Solution used here:** when only `(market_slug, outcome)` are available, backend queries Gamma to resolve the correct `clobTokenId` and caches the slug→token mapping.

Gamma parsing quirk:
- `clobTokenIds` sometimes arrives as a JSON array, sometimes as a JSON-string containing an array. Code must handle both.

### DomeAPI Quirks (docs can be wrong)

- `GET /polymarket/wallet/pnl/{wallet}`: Dome may return `wallet_addr` instead of `wallet_address`. Use serde alias.
- `GET /polymarket/activity`: `token_id` is often an empty string; use `condition_id`/`market_slug` for joins.
- Dome upstream instability: intermittent `502 Bad Gateway` happens; always timebox requests and serve stale cache.

### Debugging playbook (prevents “stuck loading” regressions)

1) Confirm backend is actually running (port conflicts are common):

```bash
lsof -nP -iTCP:3000 -sTCP:LISTEN
curl -s http://localhost:3000/health
```

2) If PERFORMANCE/BOOK panels spin forever:
- Frontend has request timeouts + in-flight TTL to avoid infinite spinners.
- Check backend logs (terminal output or `.runlogs/backend.log` if you start via scripts) for upstream 502/timeouts.

3) If signal titles look corrupted:
- `SignalDetails.market_title` must always be the **market title/question**, never a signal headline.
- Backend normalizes legacy tracked-wallet titles at the `/api/signals` boundary.

4) If filters show nothing (common with Up/Down floods):
- Recent signals may be ~100% Up/Down; the filtered view can be empty.
- The feed will automatically page older history (up to 24h) to find matches.
- If it still shows `NO MATCHING SIGNALS (24H)`, there truly were no matches in the last 24 hours.

### Hashdive API - Whale Tracking

**Endpoint:** `https://hashdive.com/api`

**Authentication:** Header `x-api-key: {API_KEY}`

**Get whale trades:**
```bash
curl -H "x-api-key: ${API_KEY}" \
  "https://hashdive.com/api/get_latest_whale_trades?min_usd=10000&limit=50"
```

**Rate Limits:**
- 1000 requests/month total
- 2 seconds between requests recommended
- Poll every 45 minutes = ~960 requests/month

### Polymarket GAMMA API - Market Data

**Endpoint:** `https://gamma-api.polymarket.com`

**No authentication required**

**Note:** If Gamma requests fail with TLS errors like `invalid peer certificate: NotValidForName`, enrichment will fall back to Dome. This is typically an environment/network trust issue (MITM/cert mismatch).

**Get markets:**
```bash
curl "https://gamma-api.polymarket.com/markets?limit=100&offset=0"
```

---

## 5. Data Flow & Signal Pipeline

```
┌─────────────────────────────────────────────────────────────────┐
│                        DATA SOURCES                              │
├─────────────────────────────────────────────────────────────────┤
│ • Polymarket GAMMA API (45-min polling)                         │
│ • Hashdive API (45-min polling, 1000/month limit)               │
│ • DomeAPI WebSocket (real-time streaming)                       │
│ • DomeAPI REST (30-second fallback polling)                     │
│ • Expiry Edge Scanner (60-second polling)                       │
└──────────────────────────┬──────────────────────────────────────┘
                           ↓
┌─────────────────────────────────────────────────────────────────┐
│                    SIGNAL DETECTION                              │
├─────────────────────────────────────────────────────────────────┤
│ SignalDetector::detect_all() - Price deviations, expiry edges   │
│ SignalDetector::detect_trader_entry() - Wallet order signals    │
└──────────────────────────┬──────────────────────────────────────┘
                           ↓
┌─────────────────────────────────────────────────────────────────┐
│                     QUALITY GATE                                 │
├─────────────────────────────────────────────────────────────────┤
│ Filter: Age < 3 seconds                                          │
│ Filter: Confidence threshold (z-score based)                     │
│ Filter: Source corroboration                                     │
│ ~30% of signals dropped                                          │
└──────────────────────────┬──────────────────────────────────────┘
                           ↓
┌─────────────────────────────────────────────────────────────────┐
│                    RISK MANAGEMENT                               │
├─────────────────────────────────────────────────────────────────┤
│ Kelly Criterion position sizing                                  │
│ Signal family calibration                                        │
│ Guardrails: max 5% bankroll, min $1000 liquidity                │
└──────────────────────────┬──────────────────────────────────────┘
                           ↓
┌─────────────────────────────────────────────────────────────────┐
│                  STORAGE & BROADCAST                             │
├─────────────────────────────────────────────────────────────────┤
│ SQLite (WAL mode, batch inserts)                                 │
│ tokio::broadcast channel → WebSocket clients                    │
└──────────────────────────┬──────────────────────────────────────┘
                           ↓
┌─────────────────────────────────────────────────────────────────┐
│                       FRONTEND                                   │
├─────────────────────────────────────────────────────────────────┤
│ REST polling (adaptive: 500ms when WS down, 5s when WS healthy)  │
│ WebSocket stream (instant updates)                               │
│ Zustand store (smart merge, dedup, sort)                        │
│ Memoized React components (prevent flicker)                      │
└─────────────────────────────────────────────────────────────────┘
```

---

## 6. Database Schema & Optimization

### Signals Table

```sql
CREATE TABLE signals (
    id TEXT PRIMARY KEY,
    signal_type TEXT NOT NULL,       -- JSON serialized SignalType
    market_slug TEXT NOT NULL,
    confidence REAL NOT NULL,
    risk_level TEXT NOT NULL,
    details_json TEXT NOT NULL,      -- JSON serialized SignalDetails
    detected_at TEXT NOT NULL,
    source TEXT NOT NULL,
    created_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now'))
) WITHOUT ROWID;  -- Clustered on PRIMARY KEY
```

### Performance Settings

```sql
PRAGMA journal_mode = WAL;           -- Concurrent reads during writes
PRAGMA synchronous = NORMAL;         -- Faster than FULL, safe with WAL
PRAGMA cache_size = -64000;          -- 64MB page cache
PRAGMA temp_store = MEMORY;          -- Temp tables in RAM
PRAGMA mmap_size = 268435456;        -- 256MB memory-mapped I/O
```

### Indexes

```sql
-- Covering index for recent signals (most common query)
CREATE INDEX idx_signals_recent ON signals(
    detected_at DESC, id, signal_type, market_slug, confidence, risk_level, details_json, source
);

-- Partial index for high-confidence signals
CREATE INDEX idx_signals_high_conf ON signals(detected_at DESC) WHERE confidence >= 0.7;

-- Source filtering
CREATE INDEX idx_signals_source ON signals(source, detected_at DESC);

-- Market filtering
CREATE INDEX idx_signals_market ON signals(market_slug, detected_at DESC);
```

### Full-history search index (SQLite FTS5)

BetterBot maintains an **FTS5-backed full-history search** over signals so the terminal search can find markets beyond the currently loaded window.

High-level schema (see `signals/db_storage.rs`):

```sql
-- Content table (canonical columns; rowid is used as the FTS content_rowid)
CREATE TABLE IF NOT EXISTS signal_search (
  signal_id TEXT NOT NULL UNIQUE,
  detected_at TEXT NOT NULL,
  market_slug TEXT NOT NULL,
  market_title TEXT,
  order_title TEXT,
  market_question TEXT,
  wallet_address TEXT,
  wallet_label TEXT,
  token_label TEXT,
  source TEXT,
  signal_type TEXT,
  updated_at INTEGER NOT NULL
);

-- FTS virtual table + sync triggers
CREATE VIRTUAL TABLE IF NOT EXISTS signal_search_fts USING fts5(
  market_slug,
  market_title,
  order_title,
  market_question,
  wallet_address,
  wallet_label,
  token_label,
  source,
  signal_type,
  content='signal_search',
  content_rowid='rowid',
  tokenize='unicode61 remove_diacritics 2'
);
```

Indexing strategy:
- **Warm-up:** on startup (and on-demand via `ensure_search_warm()`), index the most recent N signals so search never looks “dead”.
- **Incremental backfill:** a low-duty-cycle task pages backward through history, tracking cursor metadata:
  - `search_backfill_cursor_detected_at`
  - `search_backfill_cursor_id`
  - `search_backfill_done`

Reliability notes:
- SQLite has a default **999 bind variable limit**; chunk `IN (...)` enrichment lookups (we use 900).
- Avoid Rust string `\\` line continuations in SQL literals: they can concatenate tokens (e.g., `SETdetected_at`) and cause SQLite syntax errors.

---

### Additional tables in `betterbot_signals.db` (high-level)

- `signal_context` - per-signal enrichment payloads (**15m Up/Down signals store lite context only**; full enrichment is fetched on-demand).
- `dome_order_events` - raw Dome wallet order events (lossless).
- `dome_cache` - small JSON cache (also used for Gamma lookups).
- `vault_llm_decisions` / `vault_llm_model_records` - compact audit log for LONG (LLM) decisions and per-model parses.

### Separate vault DB: `betterbot_vault.db`

The pooled vault accounting state is stored separately (see `VAULT_DB_PATH`):
- `vault_state` (singleton row: cash_usdc, total_shares, updated_at)
- `vault_user_shares` (wallet_address → shares)

## 7. Authentication System

### JWT Flow

1. **Login:** `POST /api/auth/login` with `{ username, password }`
2. **Response:** `{ token: "eyJ...", user: { id, username } }`
3. **Storage:** Token stored in `localStorage['betterbot_token']`
4. **Protected routes:** Header `Authorization: Bearer {token}`
5. **Validation:** `GET /api/auth/me` returns current user

### Auth Database Schema

```sql
CREATE TABLE users (
    id INTEGER PRIMARY KEY,
    username TEXT UNIQUE NOT NULL,
    password_hash TEXT NOT NULL,     -- bcrypt, cost 12
    created_at TEXT NOT NULL
);

CREATE TABLE sessions (
    id INTEGER PRIMARY KEY,
    user_id INTEGER NOT NULL,
    token_hash TEXT NOT NULL,
    expires_at TEXT NOT NULL,
    FOREIGN KEY (user_id) REFERENCES users(id)
);
```

---

## 8. Risk Management & Kelly Criterion

### Position Sizing Formula

```
Edge = Confidence - Market_Price
Odds = (1 / Market_Price) - 1
Full_Kelly = (Confidence × Odds - (1 - Confidence)) / Odds
Actual_Kelly = Full_Kelly × 0.25  (quarter Kelly)
Position = min(Actual_Kelly, 0.10) × Bankroll
```

### Example

```
Signal: 90% confidence, market price 65%
Edge = 0.90 - 0.65 = 0.25 (25% edge)
Odds = (1/0.65) - 1 = 0.538
Full_Kelly = (0.90 × 0.538 - 0.10) / 0.538 = 0.714
Quarter_Kelly = 0.714 × 0.25 = 0.178
Capped = min(0.178, 0.10) = 0.10
Position = 0.10 × $10,000 = $1,000
```

---

## 9. Environment Configuration

### Backend `.env`

```bash
# Server (backend currently binds to port 3000)
RUST_LOG=info,betterbot=debug

# Databases
# These paths are resolved relative to `rust-backend/` (not your current shell cwd)
DATABASE_PATH=betterbot_signals.db
AUTH_DB_PATH=betterbot_auth.db

# API Keys / Tokens (NEVER COMMIT)
HASHDIVE_API_KEY=your_key_here

# Dome uses a BEARER TOKEN (sent as: Authorization: Bearer <token>)
# Supported env var names:
# - DOME_API_KEY (preferred)
# - DOME_BEARER_TOKEN (alias)
# - DOME_TOKEN (alias)
DOME_API_KEY=your_token_here

# Risk Management
INITIAL_BANKROLL=10000
KELLY_FRACTION=0.25

# JWT
JWT_SECRET=minimum-32-characters-change-in-production

# Polling / background jobs (optional)
POLL_INTERVAL_SECS=2700               # main polling loop (default 2700 = 45m)
WALLET_ANALYTICS_POLL_SECS=3600       # wallet analytics warmer
STORAGE_PRUNE_POLL_SECS=86400         # dome_order_events pruning sweep
DOME_ORDER_EVENTS_RETENTION_DAYS=365  # min enforced in code

# Tracked wallets (optional)
# MORE_INSIDERS_CSV_PATH=/abs/path/to/more_insiders.csv
# TRACKED_WALLETS_JSON='{"0xabc...": "insider_other"}'

# Binance price feed (FAST15M)
BINANCE_ENABLED=true

# Vault DB (separate from signals DB)
VAULT_DB_PATH=betterbot_vault.db

# Vault engine master switches (default is OFF)
VAULT_ENGINE_ENABLED=false
VAULT_ENGINE_PAPER=true

# FAST15M (deterministic Up/Down) tuning
UPDOWN15M_POLL_MS=2000
UPDOWN15M_MIN_EDGE=0.01
UPDOWN15M_KELLY_FRACTION=0.05
UPDOWN15M_MAX_POSITION_PCT=0.01
UPDOWN15M_SHRINK=0.35
UPDOWN15M_COOLDOWN_SEC=30

# LONG engine (BRAID-bounded LLM) switches + budgets
VAULT_LLM_ENABLED=false
OPENROUTER_API_KEY=your_key_here   # ROTATE if ever pasted in chat/logs
# Optional OpenRouter headers:
# OPENROUTER_HTTP_REFERER=https://your.domain
# OPENROUTER_APP_TITLE=BetterBot

# Models (comma-separated; first is scout; must be 4)
# VAULT_LLM_MODELS=x-ai/grok-4.1-thinking,google/gemini-3.0-high-think,openai/gpt-5.2-extra-high-thinking,anthropic/opus-4.5-thinking

# LONG tuning (defaults exist; override as needed)
VAULT_LLM_MIN_EDGE=0.02
VAULT_LLM_POLL_MS=5000
VAULT_LLM_MIN_INFER_INTERVAL_SEC=60
VAULT_LLM_COOLDOWN_SEC=300
VAULT_LLM_MAX_CALLS_PER_DAY=200
VAULT_LLM_MAX_CALLS_PER_MARKET_PER_DAY=30
VAULT_LLM_MAX_TOKENS_PER_DAY=300000
VAULT_LLM_TIMEOUT_SEC=20
VAULT_LLM_MAX_TOKENS=220
VAULT_LLM_TEMPERATURE=0.15

# LONG admissibility (defaults exist; override as needed)
VAULT_LLM_MAX_TTE_DAYS=240
VAULT_LLM_MAX_SPREAD_BPS=500
VAULT_LLM_MIN_TOP_OF_BOOK_USD=250
```

### IMPORTANT: Preventing "no signals" / "missing DOME token" regressions

- `.env` is gitignored; keep the Dome bearer token there (never in tracked files).
- The backend loads `.env` from both `rust-backend/.env` and repo-root `../.env` (relative to `CARGO_MANIFEST_DIR`) so running via `cargo run --manifest-path ...` from another working directory still picks up secrets.
- The backend DB path is read from `DB_PATH` or `DATABASE_PATH`; if neither is set it defaults to `rust-backend/betterbot_signals.db` to avoid silently creating an empty DB in the wrong directory.

### Frontend `.env`

```bash
VITE_API_URL=http://localhost:3000
VITE_WS_URL=ws://localhost:3000/ws

# Optional: Privy login (if empty, Privy UI is disabled and the app falls back to password login)
VITE_PRIVY_APP_ID=

# Feature flags
VITE_ENABLE_TRADING=false

# Optional: WebSocket latency ping cadence (ms). Lower = more updates, higher = less overhead.
VITE_WS_PING_MS=100
```

### Trading flags (backend)

```bash
# Feature flag for /api/trade/order
ENABLE_TRADING=false

# paper|live (NOTE: live mode is currently NOT wired; it will return NOT_IMPLEMENTED)
TRADING_MODE=paper
```

### LIVE trading prerequisites (NOT implemented yet)

The current codebase ships a **trade UI + API contract**, but it does **not** yet execute real orders.
When an agent wires live execution, these are the inputs you will need (store as secrets; never commit):

- **Polymarket-funded account (“funder address”)**: the on-chain address that actually holds your funds on Polymarket.
- **A signer** that can produce the required signatures (Privy wallet or equivalent).
- **Privy server credentials** (to verify sessions and request signatures from Privy on behalf of a logged-in user).
- **Enclave credentials** (only if you enable MagicSpend++ / unified deposit abstraction).

For vault live execution specifically, we also need:
- **Dome router endpoint + payload contract** (to implement `DomeExecutionAdapter`).
- A clear **idempotency + cancel/replace** story to avoid duplicate orders.

Agents should define explicit env var names for these when implementing (e.g. `PRIVY_APP_ID`, `PRIVY_SERVER_AUTH_KEY`, `ENCLAVE_API_KEY`) and document them here.

### Secret hygiene (2025-12-21)

- No Dome/Hashdive/Polymarket/Privy/Enclave secrets are hardcoded in tracked source.
- Secrets are expected via `.env`/runtime env vars only.

---

## 10. Performance Optimizations

### Backend Optimizations

| Optimization | Location | Impact |
|-------------|----------|--------|
| `parking_lot::RwLock` | main.rs | 2-5x faster locking |
| WAL mode + 64MB cache | db_storage.rs | 10x write throughput |
| Batch inserts | db_storage.rs | 100x faster bulk storage |
| Connection pooling | scrapers | Reduced TCP overhead |
| Pre-allocated vectors | All files | Fewer allocations |
| `#[inline]` hints | Hot functions | Better inlining |

### Frontend Optimizations

| Optimization | Location | Impact |
|-------------|----------|--------|
| 0.5s polling | useSignals.ts | More frequent UI refresh |
| WS ping 100ms (configurable) | services/websocket.ts | Higher-frequency latency sampling |
| Debug logging gated to dev | services/websocket.ts, stores/signalStore.ts | Less console overhead |
| Cached headers | api.ts | Fewer object allocations |
| `useCallback` + `useRef` | hooks | Proper memoization |
| `React.memo` | SignalCard | Prevents re-renders |
| `performance.now()` | useSignals.ts | More precise timing |

### Measured Results

- **API latency:** 8-9ms average (sub-10ms achieved)
- **Database:** Scales to 10M+ signals
- **Frontend:** 50% reduction in polling load
- **Memory:** Stable under continuous operation

---

## Quick Reference

### Start Development

```bash
# Backend
cd rust-backend
cargo run --release

# Frontend
cd frontend
npm run dev
```

### Test API

```bash
# Health check
curl http://localhost:3000/health

# Login
curl -X POST http://localhost:3000/api/auth/login \
  -H "Content-Type: application/json" \
  -d '{"username":"admin","password":"admin123"}'

# Get signals (with token)
curl http://localhost:3000/api/signals \
  -H "Authorization: Bearer YOUR_TOKEN"
```

### WebSocket Test

```javascript
const ws = new WebSocket('ws://localhost:3000/ws?token=YOUR_TOKEN');
ws.onmessage = (e) => console.log(JSON.parse(e.data));
```

---

## Session Learnings (2026-01-05)

- **Privy blank screen:** An empty `VITE_PRIVY_APP_ID` can crash the React mount; gate `PrivyProvider` behind a `PRIVY_ENABLED` flag.
- **Signal context payloads:** Returning full context for large lists can exceed tens of MB and break browser hydration; default to **lite context** and fetch full context on-demand.
- **Wallet analytics reliability:** Keep `/api/wallet/analytics` fast via caching + SWR; warm OPT/BASE/PESS; add `cached_only=true` (returns **204** when cold) for safe background prefetch.
- **Copy curve realism:** Realized-only copy curves go flat; default to `copy_model=scaled` (scaled wallet pnl net execution costs) and offer `copy_model=mtm` (trade replay + daily MTM) for a more defensible backtest.
- **Frontend UX:** Wallet analytics needs longer timeouts + “still computing… retry” behavior; label friction as **Execution Costs (assumed)** and rename **PF → Profit Factor**.

## Session Learnings (2026-01-12)

- **15m context bloat:** Persist only lite context for `*-updown-15m-*` and fetch live book/price via `GET /api/signals/enrich`.
- **LLM key hygiene:** Any OpenRouter key pasted in chat should be treated as compromised; load via `OPENROUTER_API_KEY` env var only and never log it.
- **Cost control:** Scout-first + 3-of-4 consensus dramatically reduces inference spend while keeping a hard permission gate.

## Session Learnings (2026-01-13)

- **Full-history search (FTS5):** Added `/api/signals/search` backed by SQLite FTS5 (`signal_search` + `signal_search_fts` + triggers) with warm-up indexing + incremental backfill.
- **Search never “dead”:** Added `/api/signals/search/status` and on-demand warm indexing (`ensure_search_warm`) so the UI can show indexing progress and still return recent matches.
- **Hybrid UX:** Terminal search is always visible; it falls back to local (loaded-window) search on 404/501 or repeated 5xx, and auto-heals back to server mode when the backend is ready.
- **Pitfalls:** Vite may bind `localhost` (IPv6) on macOS so `127.0.0.1:5173` can fail; and Rust SQL strings with `\\` line continuations can concatenate tokens and break SQLite parsing.

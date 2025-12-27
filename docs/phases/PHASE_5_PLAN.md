# Phase 5 Plan: Advanced Signal Detection System

**Priority:** HIGH - Intelligence Layer  
**Estimated Time:** 4-6 hours  
**Goal:** Multi-signal correlation, ML pattern recognition, comprehensive API endpoints

---

## Overview

Phase 5 builds the **intelligence layer** on top of our bulletproof foundation:
- Phase 1: ✅ Stable error handling
- Phase 2: ✅ Persistent storage
- Phase 3: ✅ Real-time data streaming
- Phase 4: ✅ Core profit generation engine
- **Phase 5: Advanced Signal Detection & API** ← NOW

---

## Objectives

### 1. Multi-Signal Correlation Analysis (2h)
Build a system that combines multiple signal types for higher confidence:
- Whale trades + arbitrage opportunities
- Historical performance patterns
- Market momentum indicators
- Volume and liquidity analysis

### 2. Signal Scoring & Ranking (1h)
Implement sophisticated scoring beyond simple confidence:
- Composite scores from multiple factors
- Time-decay for signal freshness
- Historical win rate weighting
- Risk-adjusted scoring

### 3. Historical Performance Tracking (1.5h)
Track and analyze signal performance over time:
- Trade execution tracking
- P&L per signal type
- Win rate by confidence bucket
- Sharpe ratio by strategy

### 4. API Endpoints (1.5h)
Expose all functionality through REST API:
- Signal retrieval and filtering
- Arbitrage opportunities
- Performance analytics
- Real-time WebSocket notifications

---

## Implementation Plan

### 5.1 Multi-Signal Correlation Engine (2h)

**File:** `rust-backend/src/signals/correlator.rs` (new)

#### Features:
1. **Composite Signal Generation**
   - Combine whale trades with arbitrage opportunities
   - Detect when multiple signals align on same market
   - Boost confidence when signals correlate

2. **Pattern Recognition**
   - Identify recurring profitable patterns
   - Track whale behavior patterns
   - Detect market regime changes

3. **Signal Aggregation**
   - Aggregate signals by market
   - Calculate composite confidence scores
   - Generate meta-signals from patterns

#### Key Structures:
```rust
pub struct CompositeSignal {
    pub market_slug: String,
    pub component_signals: Vec<MarketSignal>,
    pub composite_confidence: f64,
    pub correlation_score: f64,
    pub pattern_type: PatternType,
    pub expected_return: f64,
    pub risk_score: f64,
}

pub enum PatternType {
    WhaleArbitrageAlignment,     // Whale + arbitrage on same market
    MultiWhaleConsensus,         // Multiple whales same direction
    HistoricalRepeat,            // Similar to past winner
    VolumeSpike,                 // Unusual volume increase
    Custom(String),
}

pub struct SignalCorrelator {
    signal_storage: Arc<DbSignalStorage>,
    min_correlation: f64,
    lookback_hours: i64,
}
```

#### Methods:
```rust
impl SignalCorrelator {
    pub async fn analyze_correlations(&self) -> Result<Vec<CompositeSignal>>;
    pub async fn find_aligned_signals(&self, market_slug: &str) -> Result<Option<CompositeSignal>>;
    pub async fn detect_patterns(&self) -> Result<Vec<CompositeSignal>>;
    pub fn calculate_composite_confidence(&self, signals: &[MarketSignal]) -> f64;
}
```

---

### 5.2 Advanced Signal Scoring System (1h)

**File:** `rust-backend/src/signals/scoring.rs` (new)

#### Features:
1. **Multi-Factor Scoring**
   - Base confidence from detector
   - Historical performance boost
   - Time decay factor
   - Volume/liquidity weighting
   - Cross-signal correlation boost

2. **Risk-Adjusted Scores**
   - Sharpe ratio consideration
   - Maximum drawdown factor
   - Win rate weighting
   - Volatility adjustment

3. **Dynamic Thresholds**
   - Adaptive minimum confidence based on recent performance
   - Market regime-specific thresholds

#### Key Structures:
```rust
pub struct SignalScore {
    pub signal_id: String,
    pub raw_confidence: f64,
    pub adjusted_score: f64,
    pub time_decay_factor: f64,
    pub historical_boost: f64,
    pub correlation_boost: f64,
    pub risk_penalty: f64,
    pub final_score: f64,
    pub ranking: usize,
}

pub struct SignalScorer {
    decay_rate: f64,              // How fast signals decay (default: 0.1/hour)
    historical_weight: f64,       // Weight of historical performance (0-1)
    correlation_weight: f64,      // Weight of signal correlation (0-1)
    risk_weight: f64,            // Weight of risk factors (0-1)
}
```

#### Methods:
```rust
impl SignalScorer {
    pub fn score_signal(&self, signal: &MarketSignal, context: &ScoringContext) -> SignalScore;
    pub fn rank_signals(&self, signals: Vec<MarketSignal>) -> Vec<(MarketSignal, SignalScore)>;
    pub fn calculate_time_decay(&self, detected_at: &str) -> f64;
    pub fn calculate_historical_boost(&self, signal_type: &SignalType) -> f64;
}
```

---

### 5.3 Historical Performance Tracking (1.5h)

**File:** `rust-backend/src/analytics/performance.rs` (new)

#### Database Schema Extension:
```sql
-- Trade executions
CREATE TABLE IF NOT EXISTS trade_executions (
    id TEXT PRIMARY KEY,
    signal_id TEXT NOT NULL,
    market_slug TEXT NOT NULL,
    entry_price REAL NOT NULL,
    entry_time TEXT NOT NULL,
    exit_price REAL,
    exit_time TEXT,
    shares REAL NOT NULL,
    pnl REAL,
    pnl_pct REAL,
    outcome TEXT, -- 'win', 'loss', 'open'
    FOREIGN KEY (signal_id) REFERENCES signals(id)
);

-- Signal performance stats
CREATE TABLE IF NOT EXISTS signal_performance (
    signal_type TEXT PRIMARY KEY,
    total_trades INTEGER DEFAULT 0,
    wins INTEGER DEFAULT 0,
    losses INTEGER DEFAULT 0,
    total_pnl REAL DEFAULT 0,
    avg_pnl REAL DEFAULT 0,
    sharpe_ratio REAL DEFAULT 0,
    max_drawdown REAL DEFAULT 0,
    last_updated TEXT
);

-- Performance by confidence bucket
CREATE TABLE IF NOT EXISTS confidence_buckets (
    bucket_min REAL,
    bucket_max REAL,
    total_trades INTEGER DEFAULT 0,
    wins INTEGER DEFAULT 0,
    avg_pnl REAL DEFAULT 0,
    PRIMARY KEY (bucket_min, bucket_max)
);
```

#### Key Structures:
```rust
pub struct TradeExecution {
    pub id: String,
    pub signal_id: String,
    pub market_slug: String,
    pub entry_price: f64,
    pub entry_time: String,
    pub exit_price: Option<f64>,
    pub exit_time: Option<String>,
    pub shares: f64,
    pub pnl: Option<f64>,
    pub pnl_pct: Option<f64>,
    pub outcome: TradeOutcome,
}

pub enum TradeOutcome {
    Open,
    Win,
    Loss,
}

pub struct PerformanceMetrics {
    pub signal_type: SignalType,
    pub total_trades: u32,
    pub win_rate: f64,
    pub total_pnl: f64,
    pub avg_pnl: f64,
    pub sharpe_ratio: f64,
    pub max_drawdown: f64,
    pub profit_factor: f64,
}

pub struct PerformanceTracker {
    db_path: String,
    conn: Arc<Mutex<Connection>>,
}
```

#### Methods:
```rust
impl PerformanceTracker {
    pub async fn record_trade(&self, trade: TradeExecution) -> Result<()>;
    pub async fn close_trade(&self, trade_id: &str, exit_price: f64) -> Result<()>;
    pub async fn get_metrics(&self, signal_type: SignalType) -> Result<PerformanceMetrics>;
    pub async fn get_metrics_by_confidence(&self, min: f64, max: f64) -> Result<PerformanceMetrics>;
    pub async fn calculate_sharpe_ratio(&self, signal_type: SignalType) -> Result<f64>;
    pub async fn get_open_trades(&self) -> Result<Vec<TradeExecution>>;
}
```

---

### 5.4 Comprehensive API Endpoints (1.5h)

**File:** `rust-backend/src/api/signals_api.rs` (new)

#### Endpoint List:

```rust
// Signal Management
GET  /api/v1/signals                    // List recent signals
GET  /api/v1/signals/:id                // Get specific signal
GET  /api/v1/signals/market/:slug       // Signals for market
GET  /api/v1/signals/composite          // Composite signals

// Arbitrage Opportunities
GET  /api/v1/arbitrage/opportunities    // Current opportunities
GET  /api/v1/arbitrage/scan             // Trigger new scan
POST /api/v1/arbitrage/execute          // Execute arbitrage (future)

// Performance Analytics
GET  /api/v1/analytics/performance      // Overall performance
GET  /api/v1/analytics/by-signal-type   // Performance by type
GET  /api/v1/analytics/by-confidence    // Performance by confidence
GET  /api/v1/analytics/trades           // Trade history
GET  /api/v1/analytics/open-positions   // Current open trades

// WebSocket
WS   /api/v1/ws/signals                 // Real-time signal stream
WS   /api/v1/ws/arbitrage               // Real-time arbitrage alerts
```

#### Request/Response Types:
```rust
// GET /api/v1/signals
#[derive(Serialize)]
pub struct SignalsResponse {
    pub signals: Vec<EnrichedSignal>,
    pub total: usize,
    pub page: usize,
    pub per_page: usize,
}

#[derive(Serialize)]
pub struct EnrichedSignal {
    #[serde(flatten)]
    pub signal: MarketSignal,
    pub score: SignalScore,
    pub correlations: Vec<String>,  // Related signal IDs
    pub historical_performance: Option<PerformanceMetrics>,
}

// GET /api/v1/arbitrage/opportunities
#[derive(Serialize)]
pub struct ArbitrageResponse {
    pub opportunities: Vec<EnrichedArbitrage>,
    pub scan_time: String,
    pub count: usize,
}

#[derive(Serialize)]
pub struct EnrichedArbitrage {
    #[serde(flatten)]
    pub opportunity: ArbitrageOpportunity,
    pub execution_plan: ExecutionPlan,
    pub risk_assessment: RiskAssessment,
}

// GET /api/v1/analytics/performance
#[derive(Serialize)]
pub struct PerformanceResponse {
    pub overall: PerformanceMetrics,
    pub by_signal_type: HashMap<String, PerformanceMetrics>,
    pub by_confidence: Vec<ConfidenceBucket>,
    pub recent_trades: Vec<TradeExecution>,
}
```

---

### 5.5 WebSocket Notification System (1h)

**File:** `rust-backend/src/api/websocket.rs` (new)

#### Features:
1. **Real-Time Signal Streaming**
   - Push new signals as they're detected
   - Push composite signals when patterns emerge
   - Filter by confidence threshold

2. **Arbitrage Alerts**
   - Instant notification of profitable spreads
   - Execution plan included
   - Risk warnings if applicable

3. **Performance Updates**
   - Trade execution confirmations
   - P&L updates for open positions
   - Milestone notifications (e.g., "10 wins in a row")

#### Implementation:
```rust
pub struct WebSocketManager {
    clients: Arc<RwLock<HashMap<String, WebSocketClient>>>,
    signal_rx: broadcast::Receiver<MarketSignal>,
    arbitrage_rx: broadcast::Receiver<ArbitrageOpportunity>,
}

#[derive(Debug, Clone, Serialize)]
pub enum WebSocketMessage {
    NewSignal(EnrichedSignal),
    CompositeSignal(CompositeSignal),
    ArbitrageOpportunity(EnrichedArbitrage),
    TradeExecution(TradeExecution),
    PerformanceUpdate(PerformanceMetrics),
    Alert { level: String, message: String },
}

impl WebSocketManager {
    pub async fn broadcast(&self, message: WebSocketMessage) -> Result<()>;
    pub async fn send_to_client(&self, client_id: &str, message: WebSocketMessage) -> Result<()>;
    pub async fn handle_connection(&self, ws: WebSocket) -> Result<()>;
}
```

---

## Integration Points

### With Phase 4 (Arbitrage)
- Use `ArbitrageEngine::scan_opportunities()` for API endpoint
- Generate `ExecutionPlan` for each opportunity
- Track arbitrage-based trades in performance system

### With Phase 3 (WebSocket)
- Feed real-time orders into signal correlator
- Trigger composite signal detection on new data
- Broadcast signals via WebSocket API

### With Phase 2 (Database)
- Store composite signals in extended schema
- Track trade executions and performance
- Query historical data for pattern recognition

---

## Success Criteria

| Metric | Target | How to Verify |
|--------|--------|---------------|
| **Composite Signals** | 2-5 per hour | Monitor `/api/v1/signals/composite` |
| **API Response Time** | <100ms | Load test with `wrk` |
| **WebSocket Latency** | <50ms | Measure push-to-receive time |
| **Performance Tracking** | 100% trades logged | Check `trade_executions` table |
| **Pattern Detection** | >3 patterns recognized | Review pattern types in logs |
| **Historical Accuracy** | Sharpe >1.5 on backtest | Run performance analyzer |

---

## Testing Strategy

### 1. Unit Tests
- Signal correlator logic
- Scoring algorithm accuracy
- Performance calculations
- Time decay functions

### 2. Integration Tests
- API endpoint responses
- WebSocket connections
- Database persistence
- Real-time updates

### 3. Load Tests
- 1000 concurrent WebSocket clients
- 100 requests/sec on API
- Sustained signal generation

### 4. Validation Tests
- Historical backtest validation
- Score calibration vs actual outcomes
- Correlation accuracy verification

---

## File Structure

```
rust-backend/src/
├── signals/
│   ├── mod.rs (updated)
│   ├── db_storage.rs (existing)
│   ├── detector.rs (existing)
│   ├── correlator.rs (NEW - 300 lines)
│   └── scoring.rs (NEW - 200 lines)
├── analytics/
│   ├── mod.rs (NEW)
│   └── performance.rs (NEW - 400 lines)
├── api/
│   ├── mod.rs (updated)
│   ├── routes.rs (existing)
│   ├── simple.rs (existing)
│   ├── signals_api.rs (NEW - 350 lines)
│   └── websocket.rs (NEW - 250 lines)
└── main.rs (updated - add new modules)
```

**Total New Code:** ~1500 lines

---

## Execution Timeline

### Hour 1-2: Core Intelligence
- ✅ Create `signals/correlator.rs`
- ✅ Implement pattern detection
- ✅ Build composite signal generation

### Hour 2-3: Scoring & Performance
- ✅ Create `signals/scoring.rs`
- ✅ Create `analytics/performance.rs`
- ✅ Implement database schema updates

### Hour 3-4: API Layer
- ✅ Create `api/signals_api.rs`
- ✅ Implement all REST endpoints
- ✅ Add request/response types

### Hour 4-5: WebSocket System
- ✅ Create `api/websocket.rs`
- ✅ Implement real-time broadcasting
- ✅ Add client management

### Hour 5-6: Testing & Documentation
- ✅ Write unit tests
- ✅ Integration testing
- ✅ Create Phase 5 completion docs

---

## Risk Mitigation

### Performance Risks
- **Risk:** Correlation analysis too slow
- **Mitigation:** Cache recent signals, limit lookback window
- **Fallback:** Disable correlator if latency >500ms

### Data Quality Risks
- **Risk:** Insufficient historical data
- **Mitigation:** Start with 30-day minimum, expand gradually
- **Fallback:** Use confidence-only scoring if no history

### API Scalability Risks
- **Risk:** Too many WebSocket connections
- **Mitigation:** Connection pooling, rate limiting
- **Fallback:** Fall back to polling for excess clients

---

## Phase 5 Deliverables

1. ✅ **Code:**
   - 5 new modules (~1500 lines)
   - 10+ API endpoints
   - WebSocket notification system
   - Performance tracking database

2. ✅ **Tests:**
   - Unit tests for each module
   - Integration tests for API
   - Load tests for WebSocket

3. ✅ **Documentation:**
   - PHASE_5_COMPLETE.md
   - PHASE_5_SUMMARY.md
   - API documentation
   - WebSocket protocol spec

---

## Ready to Build! 🚀

This plan transforms BetterBot from a data collector into an **intelligent trading system** with:
- Multi-signal intelligence
- Historical learning
- Production-grade APIs
- Real-time notifications

**Let's build the intelligence layer!**

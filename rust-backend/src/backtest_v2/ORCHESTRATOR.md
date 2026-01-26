# Backtest Orchestrator (`orchestrator.rs`)

**File Location:** `rust-backend/src/backtest_v2/orchestrator.rs`  
**Lines of Code:** ~5,700+  
**Last Updated:** 2026-01-25

---

## Table of Contents

1. [Overview](#1-overview)
2. [Core Concepts](#2-core-concepts)
3. [Data Structures](#3-data-structures)
4. [Operating Modes](#4-operating-modes)
5. [Configuration](#5-configuration)
6. [Main Event Loop](#6-main-event-loop)
7. [Subsystems](#7-subsystems)
8. [Results & Reporting](#8-results--reporting)
9. [Testing](#9-testing)
10. [Function Reference](#10-function-reference)

---

## 1. Overview

The **BacktestOrchestrator** is the central engine that wires together all backtest components:

- **Strategy** - User-defined trading logic
- **SimulatedOrderSender** - Simulated OMS/execution adapter
- **BookManager** - L2 orderbook state maintenance
- **QueuePositionModel** - Passive fill queue tracking
- **SettlementEngine** - 15-minute window resolution
- **Ledger** - Double-entry accounting
- **InvariantEnforcer** - Correctness checks
- **MakerFillGate** - Maker fill validation choke point

### Key Responsibilities

1. **Event Replay** - Process historical market data events in order
2. **Time Semantics** - Enforce visibility rules (`arrival_time <= decision_time`)
3. **Execution Simulation** - Handle order placement, fills, cancels
4. **Accounting** - Track PnL, positions, fees via double-entry ledger
5. **Settlement** - Resolve 15-minute prediction market windows
6. **Validation** - Run gate suite, sensitivity analysis, invariant checks
7. **Reporting** - Generate truthfulness certificates, fingerprints, disclaimers

### Import Dependencies

```rust
use crate::backtest_v2::book::BookManager;
use crate::backtest_v2::clock::SimClock;
use crate::backtest_v2::data_contract::{DataContractValidator, DatasetReadiness, ...};
use crate::backtest_v2::events::{Event, Level, Side, TimestampedEvent};
use crate::backtest_v2::feed::MarketDataFeed;
use crate::backtest_v2::latency::LatencyConfig;
use crate::backtest_v2::matching::MatchingConfig;
use crate::backtest_v2::oms::VenueConstraints;
use crate::backtest_v2::queue::EventQueue;
use crate::backtest_v2::sim_adapter::SimulatedOrderSender;
use crate::backtest_v2::strategy::{Strategy, StrategyContext, ...};
use crate::backtest_v2::queue_model::QueuePositionModel;
use crate::backtest_v2::maker_fill_gate::MakerFillGate;
use crate::backtest_v2::visibility::{DecisionProof, VisibilityWatermark, ...};
```

---

## 2. Core Concepts

### 2.1 Time Semantics

The orchestrator enforces **strict visibility rules** to prevent look-ahead bias:

| Timestamp | Description |
|-----------|-------------|
| `source_time` | Original timestamp from upstream feed (may be untrusted) |
| `arrival_time` | When the simulator "sees" the event (used for visibility) |
| `decision_time` | Current SimClock time when strategy is invoked |

**Hard Invariant:** `arrival_time <= decision_time` for all events the strategy can read.

### 2.2 Truth Boundary Enforcement

The orchestrator automatically classifies runs based on data quality and configuration:

- **Production-Grade** - All invariants enforced, results are deployable
- **Research-Grade** - Results are indicative only
- **Taker-Only** - Maker fills disabled, aggressive execution only

### 2.3 Single Source of Truth

When `strict_accounting=true` (implied by `production_grade=true`):

- The **Ledger** is the ONLY pathway for cash, positions, fees, PnL
- The adapter's position tracking is derived-only
- First accounting violation aborts immediately

---

## 3. Data Structures

### 3.1 BacktestOperatingMode (enum)

Declares what claims are valid from a backtest run.

```rust
pub enum BacktestOperatingMode {
    TakerOnly,        // Only aggressive execution valid
    ResearchGrade,    // Indicative results only
    ProductionGrade,  // Full fidelity, all claims valid
}
```

**Key Methods:**
- `description() -> &'static str` - Human-readable description
- `allowed_claims() -> &'static [&'static str]` - Valid claims list
- `prohibited_claims() -> &'static [&'static str]` - Invalid claims list
- `allows_maker_fills() -> bool` - Whether maker fills are allowed
- `is_production_deployable() -> bool` - Whether results can be acted upon

### 3.2 MakerFillModel (enum)

Determines how passive order fills are validated.

```rust
pub enum MakerFillModel {
    ExplicitQueue,  // Queue position tracking required (production-grade)
    MakerDisabled,  // Taker-only execution (safe default)
    Optimistic,     // Fills at price match (INVALID for production)
}
```

**Default:** `MakerDisabled` (prevents silently incorrect fills)

### 3.3 BacktestConfig (struct)

Main configuration for the orchestrator. **Production-grade is the DEFAULT.**

```rust
pub struct BacktestConfig {
    // Core settings
    pub matching: MatchingConfig,
    pub latency: LatencyConfig,
    pub strategy_params: StrategyParams,
    pub trader_id: String,
    pub seed: u64,
    
    // Data contract
    pub data_contract: HistoricalDataContract,
    pub arrival_policy: SimArrivalPolicy,
    
    // Enforcement modes
    pub strict_mode: bool,                    // Visibility enforcement
    pub production_grade: bool,               // Full production requirements
    pub strict_accounting: bool,              // Ledger-only accounting
    pub allow_non_production: bool,           // Explicit downgrade opt-in
    
    // Execution models
    pub maker_fill_model: MakerFillModel,
    pub oms_parity_mode: OmsParityMode,
    pub venue_constraints: VenueConstraints,
    
    // Subsystem configs
    pub gate_mode: GateMode,
    pub integrity_policy: PathologyPolicy,
    pub sensitivity: SensitivityConfig,
    pub settlement_spec: Option<SettlementSpec>,
    pub oracle_config: Option<OracleConfig>,
    pub ledger_config: Option<LedgerConfig>,
    pub invariant_config: Option<InvariantConfig>,
    pub hermetic_config: HermeticConfig,
    pub strategy_id: Option<StrategyId>,
    
    // Limits
    pub max_events: u64,
    pub verbose: bool,
}
```

**Factory Methods:**
- `Default::default()` - Production-grade configuration
- `production_grade_15m_updown()` - Production config for Polymarket 15m markets
- `research_mode()` - Non-production with explicit `allow_non_production=true`
- `test_config()` - Unit test configuration (permissive)

**Validation:**
- `validate_production_grade() -> Result<(), ProductionGradeViolation>` - Check all requirements

### 3.4 BacktestResults (struct)

Comprehensive results from a backtest run (~100 fields).

**Core Metrics:**
```rust
pub operating_mode: BacktestOperatingMode,
pub events_processed: u64,
pub final_pnl: f64,
pub final_position_value: f64,
pub total_fills: u64,
pub total_volume: f64,
pub total_fees: f64,
pub sharpe_ratio: Option<f64>,
pub max_drawdown: f64,
pub win_rate: f64,
pub avg_fill_price: f64,
pub duration_ns: Nanos,
pub wall_clock_ms: u64,
```

**Execution Breakdown:**
```rust
pub maker_fills: u64,
pub taker_fills: u64,
pub maker_fills_blocked: u64,
pub maker_fill_model: MakerFillModel,
pub effective_maker_model: MakerFillModel,
pub maker_fills_valid: bool,
pub maker_auto_disabled: bool,
pub cancel_fill_races: u64,
pub cancel_fill_races_fill_won: u64,
pub queue_stats: Option<QueueStats>,
pub maker_fill_gate_stats: Option<MakerFillGateStats>,
```

**Validation & Trust:**
```rust
pub gate_suite_passed: bool,
pub gate_suite_report: Option<GateSuiteReport>,
pub trust_level: TrustLevel,
pub trust_decision: Option<TrustDecision>,
pub truthfulness: TruthfulnessSummary,
pub production_grade: bool,
pub production_grade_violations: Vec<String>,
```

**Accounting:**
```rust
pub settlement_model: SettlementModel,
pub settlement_stats: Option<SettlementStats>,
pub accounting_mode: AccountingMode,
pub strict_accounting_enabled: bool,
pub first_accounting_violation: Option<String>,
pub total_ledger_entries: u64,
pub window_pnl: Option<WindowPnLSeries>,
pub equity_curve: Option<EquityCurve>,
pub final_equity: Option<Amount>,
pub honesty_metrics: Option<HonestyMetrics>,
```

**Provenance:**
```rust
pub run_fingerprint: Option<RunFingerprint>,
pub strategy_id: Option<StrategyId>,
pub disclaimers: Option<DisclaimersBlock>,
```

### 3.5 TruthfulnessSummary (struct)

Comprehensive trust certificate.

```rust
pub struct TruthfulnessSummary {
    pub verdict: TrustVerdict,           // Trusted/Untrusted/Inconclusive
    pub production_grade: bool,
    pub settlement_exact: bool,
    pub ledger_enforced: bool,
    pub invariants_hard: bool,
    pub maker_valid: bool,
    pub data_classification: DatasetClassification,
    pub gates_passed: bool,
    pub sensitivity_fragilities: Vec<String>,
    pub oms_parity_mode: OmsParityMode,
    pub oms_parity_valid: bool,
    pub untrusted_reasons: Vec<String>,
    pub generated_at: i64,
}
```

**Methods:**
- `from_results(results: &BacktestResults) -> Self` - Build from results
- `is_trusted() -> bool` - Quick trust check
- `format_certificate() -> String` - Human-readable certificate
- `format_compact() -> String` - One-line summary

### 3.6 BacktestOrchestrator (struct)

The main orchestrator struct.

```rust
pub struct BacktestOrchestrator {
    // Configuration
    config: BacktestConfig,
    
    // Time management
    clock: SimClock,
    event_queue: EventQueue,
    
    // Execution
    adapter: SimulatedOrderSender,
    
    // Book state
    book_manager: BookManager,
    queue_model: QueuePositionModel,
    last_mid: HashMap<String, f64>,
    
    // Accounting
    ledger: Option<Ledger>,
    settlement_engine: Option<SettlementEngine>,
    settlement_realized_pnl: f64,
    
    // Validation
    invariant_enforcer: InvariantEnforcer,
    maker_fill_gate: MakerFillGate,
    integrity_guard: StreamIntegrityGuard,
    hermetic_enforcer: HermeticEnforcer,
    data_validator: DataContractValidator,
    
    // Visibility
    visibility: VisibilityWatermark,
    decision_proofs: DecisionProofBuffer,
    current_proof: Option<DecisionProof>,
    
    // State tracking
    results: BacktestResults,
    pnl_history: Vec<f64>,
    tracked_markets: HashSet<String>,
    pending_settlements: Vec<SettlementEvent>,
    pending_cancels: HashMap<u64, Nanos>,
    
    // Fingerprinting
    fingerprint_collector: FingerprintCollector,
    
    // Window accounting
    window_accounting: Option<WindowAccountingEngine>,
    equity_recorder: Option<EquityRecorder>,
    
    // Runtime state
    effective_maker_model: MakerFillModel,
    dataset_readiness: Option<DatasetReadiness>,
    maker_paths_enabled: bool,
    next_fill_id: u64,
    next_settlement_id: u64,
}
```

---

## 4. Operating Modes

### 4.1 Mode Determination

Operating mode is determined **once at startup** and cannot be changed:

```rust
pub fn determine_operating_mode(
    config: &BacktestConfig,
    data_classification: DatasetClassification,
) -> BacktestOperatingMode
```

**Rules (in order):**

1. If `production_grade=true` AND all requirements met → `ProductionGrade`
2. If `maker_fill_model=MakerDisabled` → `TakerOnly`
3. If data doesn't support queue modeling → `TakerOnly` (auto-downgrade)
4. If `maker_fill_model=Optimistic` → `ResearchGrade`
5. If `ExplicitQueue` + full data but not production_grade → `ResearchGrade`
6. Otherwise → `TakerOnly` (safest default)

### 4.2 Mode Banner

The orchestrator prints a startup banner showing the mode:

```
╔══════════════════════════════════════════════════════════════════════════════╗
║                      BACKTEST OPERATING MODE                                 ║
╠══════════════════════════════════════════════════════════════════════════════╣
║  MODE: PRODUCTION-GRADE: Full fidelity, all claims valid                     ║
╠══════════════════════════════════════════════════════════════════════════════╣
║  ALLOWED CLAIMS:                                                             ║
║    ✓ Taker PnL (aggressive execution)                                        ║
║    ✓ Maker PnL (passive execution)                                           ║
║    ✓ Queue position tracking                                                 ║
║    ✓ Cancel-fill race outcomes                                               ║
║    ✓ Sharpe ratio (production-grade)                                         ║
║    ✓ All execution metrics                                                   ║
╠══════════════════════════════════════════════════════════════════════════════╣
║  PROHIBITED CLAIMS:                                                          ║
║    (none - all claims allowed)                                               ║
╚══════════════════════════════════════════════════════════════════════════════╝
```

### 4.3 Hard Enforcement

If user explicitly requests maker fills but mode is TakerOnly:

```rust
if config.maker_fill_model == MakerFillModel::ExplicitQueue 
    && operating_mode == BacktestOperatingMode::TakerOnly 
{
    anyhow::bail!("BACKTEST ABORTED: Maker fills requested but operating mode is TAKER-ONLY...");
}
```

---

## 5. Configuration

### 5.1 Production-Grade Requirements

For `production_grade=true`, ALL of these must be satisfied:

| Requirement | Field | Value |
|-------------|-------|-------|
| Visibility | `strict_mode` | `true` |
| Arrival Policy | `arrival_policy` | Must be production-grade |
| OMS Parity | `oms_parity_mode` | `Full` |
| Integrity | `integrity_policy` | `strict()` |
| Gates | `gate_mode` | `Strict` |
| Sensitivity | `sensitivity.enabled` | `true` |
| Settlement | `settlement_spec` | Must be `Some(...)` |
| Ledger | `ledger_config` | Must be `Some(...)` with `strict_mode=true` |
| Invariants | `invariant_config.mode` | `Hard` |
| Data Contract | `data_contract.orderbook` | `FullIncrementalL2DeltasWithExchangeSeq` |
| Maker Model | `maker_fill_model` | Not `Optimistic` |
| Accounting | `strict_accounting` | `true` |
| Oracle | `oracle_config` | Must be `Some(...)` and valid |
| Hermetic | `hermetic_config.enabled` | `true` |
| Strategy ID | `strategy_id` | Must be `Some(...)` and valid |

### 5.2 Non-Production Override

To run with downgrades, you MUST explicitly set:

```rust
BacktestConfig {
    allow_non_production: true,
    ..BacktestConfig::research_mode()
}
```

Without this flag, non-production configs abort with detailed error.

### 5.3 Configuration Validation

```rust
impl BacktestConfig {
    pub fn validate_production_grade(&self) -> Result<(), ProductionGradeViolation> {
        // Checks all 16+ requirements
        // Returns detailed violation list if any fail
    }
}
```

---

## 6. Main Event Loop

### 6.1 Entry Points

```rust
impl BacktestOrchestrator {
    pub fn new(config: BacktestConfig) -> Self { ... }
    
    pub fn try_new(config: BacktestConfig) -> Result<Self> {
        // Validates production-grade requirements upfront
        config.validate_production_grade()?;
        Ok(Self::new(config))
    }
    
    pub fn load_feed<F: MarketDataFeed>(&mut self, feed: &mut F) -> Result<()> {
        // Load events from feed into event_queue
        // Validates data contract in production-grade mode
    }
    
    pub fn run(&mut self, strategy: &mut dyn Strategy) -> Result<BacktestResults> {
        // Main event loop
    }
}
```

### 6.2 Run Method Structure

The `run()` method (~600 lines) follows this structure:

```
1. NON-PRODUCTION GATING
   - Check if config is non-production
   - Abort if allow_non_production=false
   - Emit warnings if allowed

2. DATASET CLASSIFICATION
   - Classify data contract (FullIncremental/SnapshotOnly/Incomplete)
   - Determine operating mode
   - Print operating mode banner

3. DATASET READINESS GATING
   - Classify readiness (MakerViable/TakerOnly/NonRepresentative)
   - Abort if NonRepresentative
   - Configure maker fill gate

4. PRODUCTION-GRADE VALIDATION
   - Validate all requirements
   - Abort on any failure

5. INITIALIZATION
   - Set up clock, visibility watermark
   - Call strategy.on_start()
   - Record initial equity observation

6. MAIN EVENT LOOP
   while events_processed < max_events:
       a. Process adapter-generated events
       b. Check and fire timers
       c. Get next event from queue
       d. Stream integrity check
       e. Advance clock and visibility
       f. Invariant checks
       g. Settlement engine advance
       h. Process pending settlements
       i. Dispatch event to strategy
       j. Abort on invariant/accounting violations

7. FINALIZATION
   - Call strategy.on_stop()
   - Calculate final results
   - Run gate suite
   - Run sensitivity sweeps
   - Trust gate evaluation
   - Generate truthfulness certificate
   - Generate disclaimers block
   - Return results
```

### 6.3 Event Dispatch

The `dispatch_event()` method handles all event types:

```rust
fn dispatch_event(&mut self, strategy: &mut dyn Strategy, event: &TimestampedEvent) {
    match &event.event {
        Event::L2BookSnapshot { .. } => {
            // Update BookManager
            // Check book invariants
            // Track mid price
            // Feed settlement engine
            // Call strategy.on_book_update()
        }
        
        Event::L2Delta { .. } => {
            // Apply delta to BookManager
            // Check invariants
            // Feed queue model
            // Track mid price
            // Call strategy.on_book_update()
        }
        
        Event::L2BookDelta { .. } => {
            // Single-level delta (from price_change messages)
            // Enable maker viability
            // Update BookManager and queue model
        }
        
        Event::TradePrint { .. } => {
            // Feed settlement engine
            // Call strategy.on_trade()
        }
        
        Event::OrderAck { .. } => {
            // Call strategy.on_order_ack()
        }
        
        Event::OrderReject { .. } => {
            // Call strategy.on_order_reject()
        }
        
        Event::Fill { .. } => {
            // MAKER FILL: Route through MakerFillGate
            // TAKER FILL: Always allowed
            // Post to ledger
            // Track in window accounting
            // Record equity observation
            // Check invariants
            // Call strategy.on_fill()
        }
        
        Event::CancelAck { .. } => {
            // Process cancel
            // Call strategy.on_cancel_ack()
        }
        
        Event::Timer { .. } => {
            // Call strategy.on_timer()
        }
    }
}
```

### 6.4 Fill Processing

Fills have special handling based on maker/taker:

```rust
// TAKER FILLS: Always allowed
self.results.taker_fills += 1;
should_process_fill = true;

// MAKER FILLS: Must pass through MakerFillGate
let candidate = MakerFillCandidate { ... };
let queue_proof = self.queue_model.get_position(order_id).map(|pos| QueueProof::new(...));
let cancel_proof = CancelRaceProof::...;

match self.maker_fill_gate.validate_or_reject(candidate, queue_proof, cancel_proof) {
    Ok(_admitted_fill) => {
        self.results.maker_fills += 1;
        should_process_fill = true;
    }
    Err(reason) => {
        self.results.maker_fills_blocked += 1;
        should_process_fill = false;  // DO NOT credit PnL
    }
}
```

---

## 7. Subsystems

### 7.1 BookManager

Maintains authoritative L2 book state:

```rust
book_manager: BookManager
```

**Used for:**
- Execution simulation (fill price determination)
- Queue position tracking inputs
- Book invariant enforcement (crossed book, monotonic levels)

### 7.2 QueuePositionModel

Tracks queue positions for passive fills:

```rust
queue_model: QueuePositionModel
```

**Fed by:**
- L2Delta events
- L2BookDelta events (single-level deltas)

### 7.3 SettlementEngine

Handles 15-minute window resolution:

```rust
settlement_engine: Option<SettlementEngine>
```

**Responsibilities:**
- Track window start/end times
- Observe prices during window
- Compute settlement outcomes (Winner, Split, Invalid)
- Feed oracle prices

### 7.4 Ledger

Double-entry accounting (SOLE SOURCE OF TRUTH when enabled):

```rust
ledger: Option<Ledger>
```

**Accounts:**
- Cash
- Positions (by market/outcome)
- Fees
- Realized PnL

### 7.5 InvariantEnforcer

Mandatory invariant checking:

```rust
invariant_enforcer: InvariantEnforcer  // Never Option, always present
```

**Invariant Categories:**
- Time (monotonicity)
- Book (crossed detection, sequence gaps)
- OMS (order state)
- Fills (order must exist, price validity)
- Accounting (cash non-negative, balance)
- Settlement (no duplicates)

### 7.6 MakerFillGate

Single choke point for maker fill validation:

```rust
maker_fill_gate: MakerFillGate
```

**Requires:**
- `QueueProof` - Proves queue position was consumed
- `CancelRaceProof` - Proves order was live at fill time

### 7.7 IntegrityGuard

Stream integrity enforcement:

```rust
integrity_guard: StreamIntegrityGuard
```

**Detects:**
- Duplicates (same event twice)
- Gaps (missing sequence numbers)
- Out-of-order events

**Actions (based on policy):**
- Drop duplicates
- Halt on gaps (production)
- Reorder if possible

### 7.8 HermeticEnforcer

Strategy sandboxing:

```rust
hermetic_enforcer: HermeticEnforcer
```

**Enforces (production mode):**
- No wall-clock time access
- No environment variables
- No filesystem/network I/O
- No thread spawning
- DecisionProof for every callback

### 7.9 WindowAccountingEngine

Per-15-minute window PnL tracking:

```rust
window_accounting: Option<WindowAccountingEngine>
```

**Tracks per window:**
- Trades count
- Volume
- Fees
- Net PnL

### 7.10 EquityRecorder

Canonical equity curve:

```rust
equity_recorder: Option<EquityRecorder>
```

**Records observations at:**
- Initial deposit
- Fills
- Fees
- Settlements
- Finalization

---

## 8. Results & Reporting

### 8.1 Gate Suite

```rust
fn run_gate_suite(&mut self) -> Result<()>
```

Tests:
- Zero-edge test (should break even with no edge)
- Martingale test (no pattern exploitation)
- Signal-inversion test (reverse signals should lose)

### 8.2 Sensitivity Analysis

```rust
fn run_sensitivity_sweeps(&mut self) -> Result<()>
```

Tests sensitivity to:
- Latency variations
- Sampling rate
- Execution assumptions

### 8.3 Trust Gate

The TrustGate is the **SOLE PATHWAY** for establishing trust:

```rust
let trust_decision = TrustGate::evaluate(
    &self.config,
    &self.results,
    self.results.gate_suite_report.as_ref(),
    Some(&self.results.sensitivity_report),
    Some(&run_fingerprint),
);
```

### 8.4 Disclaimers

Generated programmatically from run conditions:

```rust
let disclaimers_block = generate_disclaimers(&disclaimer_ctx);
```

Includes all caveats that must accompany published results.

### 8.5 Finalize Results

```rust
fn finalize_results(&mut self, wall_start: Instant, duration_ns: Nanos)
```

Computes:
- Final PnL (from ledger or adapter)
- Position value (mark-to-market)
- Sharpe ratio
- Max drawdown
- Win rate
- OMS parity stats
- Invariant stats
- Maker fill gate stats
- Window PnL series
- Equity curve
- Honesty metrics

---

## 9. Testing

### 9.1 Test Infrastructure

```rust
// Helper to create book events
fn make_book_event(time: Nanos, mid: f64) -> TimestampedEvent

// NoOp strategy for testing
struct NoOpStrategy;
impl Strategy for NoOpStrategy { ... }
```

### 9.2 Test Categories

**Basic Tests:**
- `test_backtest_orchestrator` - Basic functionality
- `test_empty_backtest` - No events
- `test_production_grade_validation_*` - Config validation

**Operating Mode Tests:**
- `test_operating_mode_default_is_taker_only`
- `test_operating_mode_explicit_queue_*`
- `test_operating_mode_production_grade_*`
- `test_operating_mode_optimistic_*`

**Bypass Prevention Tests:**
- `test_verification_optimistic_maker_rejected_in_production`
- `test_verification_relaxed_oms_rejected_in_production`
- `test_verification_permissive_gates_rejected_in_production`
- `test_verification_disabled_gates_rejected_in_production`
- `test_verification_disabled_sensitivity_rejected_in_production`
- `test_verification_snapshot_data_rejected_for_makers_in_production`
- `test_verification_incomplete_data_always_rejected_in_production`
- `test_verification_no_certificate_bypass_in_production`

**Integration Tests:**
- `test_integration_noop_strategy_completes`
- `test_integration_book_snapshot_updates_book_manager`
- `test_integration_l2_delta_updates_book_manager`
- `test_integration_trade_print_feeds_settlement_engine`
- `test_integration_invariant_checks_run`
- `test_integration_ledger_posts_fill`
- `test_integration_run_fingerprint_generated`
- `test_integration_determinism_same_seed`

---

## 10. Function Reference

### Public Methods

| Method | Description |
|--------|-------------|
| `new(config)` | Create orchestrator |
| `try_new(config)` | Create with production validation |
| `load_feed(feed)` | Load events from data feed |
| `run(strategy)` | Run the backtest |
| `results()` | Get results reference |
| `adapter()` | Get adapter reference |
| `settlement_engine()` | Get settlement engine |
| `settlement_realized_pnl()` | Get settlement-based PnL |
| `ledger()` | Get ledger reference |
| `invariant_enforcer()` | Get invariant enforcer |
| `effective_maker_model()` | Get effective maker model |
| `maker_paths_enabled()` | Check if maker paths enabled |
| `dataset_readiness()` | Get dataset readiness |

### Private Methods

| Method | Description |
|--------|-------------|
| `detect_non_production_config()` | Check for downgrades |
| `list_downgraded_subsystems()` | List all downgrades |
| `dispatch_event(strategy, event)` | Handle one event |
| `process_pending_settlements()` | Process settlements |
| `run_gate_suite()` | Run gate tests |
| `run_sensitivity_sweeps()` | Run sensitivity |
| `finalize_results(wall_start, duration)` | Compute final results |
| `extract_market_id(token_id)` | Parse market from token |

### Free Functions

| Function | Description |
|----------|-------------|
| `determine_operating_mode(config, classification)` | Determine mode |
| `format_operating_mode_banner(mode)` | Format startup banner |

---

## Quick Reference: Adding New Features

### To add a new event type:

1. Add variant to `Event` enum in `events.rs`
2. Add handling in `dispatch_event()` match arm
3. Add any invariant checks needed
4. Update fingerprint collector if needed

### To add a new invariant:

1. Add check method to `InvariantEnforcer`
2. Call it in appropriate place in event loop
3. Add test for violation detection

### To add a new production requirement:

1. Add field to `BacktestConfig`
2. Add check in `validate_production_grade()`
3. Add enforcement in `run()` if needed
4. Update `BacktestConfig::production_grade_15m_updown()`
5. Add bypass prevention test

### To modify accounting:

1. All changes MUST go through `Ledger` when `strict_accounting=true`
2. Update `finalize_results()` to use ledger values
3. Ensure window accounting is updated
4. Ensure equity recorder is updated

---

## Changelog Notes

This file documents `orchestrator.rs` as of 2026-01-25. Key features:

- Production-grade as default (non-production requires explicit opt-in)
- Operating mode truth boundaries
- MakerFillGate as single choke point
- Stream integrity enforcement
- Window PnL and equity curve tracking
- Honesty metrics computation
- Trust gate evaluation
- Disclaimers generation
- Run fingerprinting for reproducibility

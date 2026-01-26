# Independent Audit Report: Polymarket 15-Minute Up/Down HFT Backtesting Backend

**Audit Date:** 2026-01-24  
**Auditor Scope:** External, independent assessment  
**Codebase Path:** `rust-backend/src/backtest_v2/`  
**Lines Analyzed:** ~47,000+ LOC across 65 modules

---

## Executive Summary

The Polymarket 15-minute up/down HFT backtesting backend represents a **near-production-grade** system with unusually strong structural enforcement mechanisms. The architecture distinguishes between "claims the system can make" and "modes in which claims are valid" with explicit gating at multiple levels.

**Key Findings:**

1. **Structural enforcement is real, not cosmetic.** The system uses compile-time type gating, runtime flag checks, and abort-on-violation semantics to prevent common backtesting fallacies.

2. **Dataset readiness classification is enforced at runtime.** The system will refuse to run maker strategies on snapshot-only data and will abort entirely on incomplete data.

3. **Production-grade mode is genuinely stricter.** When `production_grade=true`, ALL invariants are checked in Hard mode (abort-on-first-violation), and the system enforces strict accounting, strict visibility, and strict integrity policies.

4. **The double-entry ledger is enforced structurally when strict_accounting=true.** Direct mutations to portfolio state are blocked by runtime guards that panic in strict mode.

5. **The system can still produce misleading results in non-production-grade mode.** Default configurations allow soft invariants, permissive integrity policies, and missing queue proofs.

---

## Section-by-Section Findings

---

### 1. Data Contract and Dataset Readiness

**A) What the system is REQUIRED to do:**
- Classify datasets by fidelity (FullIncremental, SnapshotOnly, Incomplete)
- Gate execution modes based on data availability
- Prevent maker strategies from running on snapshot-only data
- Abort on non-representative (incomplete) datasets

**B) What the code ACTUALLY does:**

The `DatasetReadinessClassifier` in `data_contract.rs` (lines 428-574) implements a 3-tier classification:

1. **MakerViable**: FullIncrementalL2DeltasWithExchangeSeq + TradePrints + RecordedArrival
2. **TakerOnly**: Snapshots/Deltas + TradePrints + usable timestamps
3. **NonRepresentative**: Missing orderbook OR missing trades OR unusable timestamps

**C) Evidence:**

```rust
// orchestrator.rs:1332-1344
if !readiness.allows_backtest() {
    anyhow::bail!(
        "BACKTEST ABORTED: Dataset classified as NON_REPRESENTATIVE.\n\n{}\n\n\
         The dataset is insufficient for reliable backtesting..."
    );
}

// orchestrator.rs:1345-1358
if self.config.maker_fill_model == MakerFillModel::ExplicitQueue && !readiness.allows_maker() {
    anyhow::bail!(
        "BACKTEST ABORTED: Maker strategies requested but dataset readiness is {}..."
    );
}
```

**D) Failure modes:**
- If a user manually constructs a `HistoricalDataContract` with incorrect fields, the classifier could misclassify
- The system trusts the declared `arrival_time` semantics without independent verification
- **UNVERIFIED:** Whether the live data pipeline actually records arrival times correctly

**E) Verdict: PASS**

The gating is structural and enforced at the start of `run()`. NonRepresentative data aborts. Maker strategies on TakerOnly data aborts.

---

### 2. Event Model and Time Semantics

**A) What the system is REQUIRED to do:**
- Distinguish source_time (exchange), arrival_time (our system), and decision_time (sim clock)
- Ensure events are ordered deterministically by (time, priority, source, seq)
- Prevent strategies from seeing future data (visibility enforcement)
- Support deterministic replay

**B) What the code ACTUALLY does:**

The `TimestampedEvent` struct (events.rs:322-360) carries:
- `time`: arrival timestamp (used for visibility)
- `source_time`: exchange timestamp
- `seq`: sequence number (assigned by EventQueue)
- `source`: stream identifier

Ordering is implemented in `Ord for TimestampedEvent`:
```rust
self.time.cmp(&other.time)
    .then_with(|| self.event.priority().cmp(&other.event.priority()))
    .then_with(|| self.source.cmp(&other.source))
    .then_with(|| self.seq.cmp(&other.seq))
```

**C) Evidence:**

```rust
// visibility.rs:241-260
pub fn assert_visible(&mut self, event: &TimestampedEvent) {
    if event.time > self.decision_time {
        let violation = VisibilityViolation { ... };
        if is_strict_mode() {
            panic!("VISIBILITY VIOLATION (strict mode): {}", violation.description);
        } else {
            self.violations.push(violation);
        }
    }
}
```

**D) Failure modes:**
- In non-strict mode, visibility violations are logged but not fatal
- The `seq` field is assigned by EventQueue, so raw feeds must be sorted before injection
- **UNVERIFIED:** Whether the actual data loading code preserves deterministic ordering when reading from disk

**E) Verdict: PASS**

Time semantics are well-defined. Visibility is enforced structurally in strict mode (panics). The priority-based ordering ensures determinism.

---

### 3. Market Reconstruction and Integrity

**A) What the system is REQUIRED to do:**
- Detect and handle duplicates, gaps, and out-of-order events
- Support resync via snapshot when deltas are missing
- Halt or recover based on explicit policy

**B) What the code ACTUALLY does:**

The `StreamIntegrityGuard` (integrity.rs:300+) maintains per-token state and enforces `PathologyPolicy`:

```rust
pub struct PathologyPolicy {
    pub on_duplicate: DuplicatePolicy,  // Drop | Halt
    pub on_gap: GapPolicy,              // Halt | Resync
    pub on_out_of_order: OutOfOrderPolicy, // Drop | Reorder | Halt
    pub gap_tolerance: u64,
    pub reorder_buffer_size: usize,
    pub timestamp_jitter_tolerance_ns: Nanos,
}
```

Three preset policies exist:
- `strict()`: Halts on any gap or out-of-order
- `resilient()`: Resyncs on gap, reorders out-of-order
- `permissive()`: Drops problematic events, never halts

**C) Evidence:**

```rust
// integrity.rs:483-510
if gap_size > self.policy.gap_tolerance {
    match self.policy.on_gap {
        GapPolicy::Halt => {
            state.sync_state = SyncState::Halted;
            self.counters.halted = true;
            return IntegrityResult::Halted(HaltReason { ... });
        }
        GapPolicy::Resync => { ... }
    }
}
```

**D) Failure modes:**
- The `resilient()` and `permissive()` policies can silently corrupt book state
- The hash-based deduplication has a fixed-size seen_hashes set (100,000) that prunes old entries
- **UNVERIFIED:** Whether Polymarket actually provides sequence numbers for all events

**E) Verdict: PASS**

In production-grade mode, `PathologyPolicy::strict()` is enforced. Gaps halt. The counters provide observability.

---

### 4. Strategy Boundary and Information Exposure

**A) What the system is REQUIRED to do:**
- Provide identical interfaces for live and backtest execution
- Prevent strategies from accessing future data
- Support auditability through DecisionProof

**B) What the code ACTUALLY does:**

The `Strategy` trait (strategy.rs:270-320) defines callbacks:
- `on_book_update(ctx, book)` - receives `BookSnapshot`
- `on_trade(ctx, trade)` - receives `TradePrint`
- `on_fill(ctx, fill)` - receives `FillNotification`

The `OrderSender` trait provides:
- `send_order()`, `send_cancel()`, `get_position()`, `now()`

**C) Evidence:**

```rust
// strategy.rs:162-188
pub trait OrderSender: Send + Sync {
    fn send_order(&mut self, order: StrategyOrder) -> Result<OrderId, String>;
    fn send_cancel(&mut self, cancel: StrategyCancel) -> Result<(), String>;
    fn get_position(&self, token_id: &str) -> Position;
    fn now(&self) -> Nanos;
    // ...
}
```

The `DecisionProofBuffer` (visibility.rs:375-440) records:
- decision_id, decision_time
- input_events (arrival_time, source_time, seq, event_type)
- proof_hash for comparison

**D) Failure modes:**
- Strategies receive raw `&BookSnapshot` which includes the `exchange_seq` - this could theoretically be abused
- The proof buffer is bounded (configurable capacity) and will drop old proofs
- **No compile-time enforcement** prevents a strategy from calling `std::time::SystemTime::now()`

**E) Verdict: PARTIAL**

The boundary is well-designed but not hermetic. A malicious or careless strategy could still access wall-clock time or other global state. DecisionProof exists but is not mandatory in all modes.

---

### 5. OMS Parity and Order Lifecycle

**A) What the system is REQUIRED to do:**
- Enforce legal order state transitions (New → PendingAck → Live → ...)
- Support rate limiting matching venue constraints
- Handle out-of-order messages (fill before ack)
- Detect impossible behaviors (fill after terminal)

**B) What the code ACTUALLY does:**

The `OrderManagementSystem` (oms.rs:440+) tracks:
- `OrderState`: New, PendingAck, Live, PartiallyFilled, PendingCancel, Done
- `TerminalReason`: Filled, Cancelled, Rejected, Expired, etc.
- `VenueConstraints`: min/max size, tick size, rate limits, etc.

```rust
impl OmsOrder {
    pub fn apply_fill(&mut self, fill_qty, fill_price, fee, now) -> bool {
        if self.state.is_terminal() { return false; }
        // ... update avg fill price, remaining qty
        if self.remaining_qty <= 0.0 {
            self.state = OrderState::Done;
            self.terminal_reason = Some(TerminalReason::Filled);
        }
        true
    }
}
```

**C) Evidence:**

```rust
// oms.rs:33-45
impl OrderState {
    pub fn is_terminal(&self) -> bool {
        matches!(self, OrderState::Done)
    }
    pub fn can_cancel(&self) -> bool {
        matches!(self, OrderState::Live | OrderState::PartiallyFilled)
    }
}
```

The invariant enforcer checks OMS transitions (invariants.rs):
```rust
ViolationType::IllegalStateTransition { from, to, order_id }
ViolationType::FillBeforeAck { order_id }
ViolationType::FillAfterTerminal { order_id, terminal_state }
```

**D) Failure modes:**
- The `OmsParityMode` can be set to `Permissive` which skips some checks
- Rate limiting is applied to the simulation, not validated against actual venue behavior
- **UNVERIFIED:** Whether the VenueConstraints match actual Polymarket limits

**E) Verdict: PASS**

The OMS state machine is well-designed. Invariants detect impossible transitions. Rate limiting is modeled.

---

### 6. Execution and Fill Plausibility

**A) What the system is REQUIRED to do:**
- Gate maker fills through explicit queue proof
- Detect cancel-fill races
- Prevent optimistic fill assumptions in production mode

**B) What the code ACTUALLY does:**

The `MakerFillGate` (maker_fill_gate.rs:440+) is the **single choke point** for all maker fills:

```rust
pub fn validate_or_reject(
    &mut self,
    candidate: MakerFillCandidate,
    queue_proof: Option<QueueProof>,
    cancel_proof: Option<CancelRaceProof>,
) -> Result<AdmissibleFill, RejectionReason>
```

In production mode:
- `QueueProof` is MANDATORY (queue_ahead_at_arrival - queue_consumed <= 0)
- `CancelRaceProof` is MANDATORY (order was live at fill time)

**C) Evidence:**

```rust
// maker_fill_gate.rs:470-490
if self.config.production_grade {
    let queue_proof = match queue_proof {
        Some(p) => p,
        None => {
            self.stats.fills_rejected += 1;
            return Err(RejectionReason::MissingQueueProof);
        }
    };
    if !queue_proof.validates() {
        return Err(RejectionReason::QueueNotConsumed { ... });
    }
}
```

**D) Failure modes:**
- In research mode (`allow_missing_queue_proof: true`), maker fills can be credited without proof
- The queue consumption model depends on observing trade prints, which may be incomplete
- **UNVERIFIED:** Whether the queue model accurately reflects Polymarket's actual execution priority

**E) Verdict: PASS (in production-grade mode)**

The gate is structural. Missing proofs are rejected. Stats track admissions vs rejections.

---

### 7. Settlement and Oracle Integration

**A) What the system is REQUIRED to do:**
- Define 15-minute window boundaries precisely
- Use authoritative reference prices (Chainlink)
- Apply correct tie-breaking rules
- Respect visibility semantics for outcome knowability

**B) What the code ACTUALLY does:**

The `SettlementSpec` (settlement.rs:1-300) defines:
- `window_duration_ns`: 15 * 60 * NS_PER_SEC
- `reference_price_rule`: MidPrice (Binance spot)
- `tie_rule`: NoWins (tie = Down wins)
- `outcome_knowable_rule`: OnReferenceArrival

```rust
pub fn determine_outcome(&self, start_price, end_price) -> SettlementOutcome {
    match comparison {
        Ordering::Greater => Resolved { winner: Yes, is_tie: false },
        Ordering::Less => Resolved { winner: No, is_tie: false },
        Ordering::Equal => match self.tie_rule {
            TieRule::NoWins => Resolved { winner: No, is_tie: true },
            // ...
        }
    }
}
```

Chainlink integration (oracle/chainlink.rs) provides:
- `ChainlinkRound`: round_id, answer, updated_at, ingest_arrival_time_ns
- Feed configs for BTC/ETH/SOL/XRP on Polygon

**C) Evidence:**

```rust
// settlement.rs:14-17
// **CRITICAL**: Settlement must respect visibility semantics. The outcome is NOT knowable
// at the cutoff time. It becomes knowable only when the reference price event becomes
// VISIBLE in the simulation (arrival_time <= decision_time).
```

**D) Failure modes:**
- The `WindowStartRule::FromSlugTimestamp` parser assumes a specific slug format
- Chainlink data must be backfilled separately; the system does not automatically fetch it
- **UNVERIFIED:** Whether the rounding rules exactly match Polymarket's contract

**E) Verdict: PARTIAL**

Settlement logic is well-specified. The visibility comment is correct. However:
- Chainlink integration requires manual configuration of RPC endpoints
- The system trusts the configured feed addresses

---

### 8. Accounting and PnL Correctness

**A) What the system is REQUIRED to do:**
- Track all economic state changes via double-entry ledger
- Ensure debits == credits for every posting
- Prevent direct mutations when strict accounting is enabled

**B) What the code ACTUALLY does:**

The `Ledger` (ledger.rs) uses fixed-point arithmetic (`Amount = i128`, AMOUNT_SCALE = 1e8):

```rust
pub struct LedgerEntry {
    pub entry_id: u64,
    pub sim_time_ns: Nanos,
    pub event_ref: EventRef,
    pub postings: Vec<LedgerPosting>,  // Must sum to zero
}
```

The `strict_accounting` module (strict_accounting.rs) provides:
- Global `STRICT_ACCOUNTING_ACTIVE` flag
- `guard_direct_mutation!` macro that panics in strict mode

**C) Evidence:**

```rust
// strict_accounting.rs:71-82
pub fn abort_direct_mutation(location: &str) -> ! {
    panic!(
        "STRICT ACCOUNTING VIOLATION - DIRECT MUTATION BLOCKED\n\
         LOCATION: {}\n\
         When strict_accounting=true, ALL economic state changes MUST go through\n\
         the double-entry ledger. Direct mutations are FORBIDDEN.",
        location
    )
}
```

Guards are placed in:
- `Portfolio::apply_fill()` (portfolio.rs)
- `Portfolio::deposit()`, `withdraw()`, `settle_market()`
- `SimAdapter::process_fill()` (sim_adapter.rs)

**D) Failure modes:**
- If a module forgets to add the guard macro, mutations could bypass the ledger
- The `STRICT_ACCOUNTING_ACTIVE` flag is global and affects all concurrent tests
- **UNVERIFIED:** Whether all mutation paths are covered by guards

**E) Verdict: PASS**

The structural enforcement is real. The macro approach ensures visibility. Fixed-point avoids float errors.

---

### 9. Invariants and Abort Semantics

**A) What the system is REQUIRED to do:**
- Check invariants across 5 categories (Time, Book, OMS, Fills, Accounting)
- Abort on first violation in Hard mode
- Produce deterministic causal dumps

**B) What the code ACTUALLY does:**

The `InvariantEnforcer` (invariants.rs:580+) maintains:
- Ring buffers for recent events, OMS transitions, ledger entries
- Counters per category
- First violation record with `CausalDump`

```rust
pub enum InvariantMode {
    Off,   // No checking (INVALID for production)
    Soft,  // Log + count, continue
    Hard,  // Abort on first violation
}
```

Production-grade mode forces Hard (production_grade.rs:380-420):
```rust
if !matches!(invariant_config.mode, InvariantMode::Hard) {
    violations.push("Invariant mode must be Hard (abort on first violation)");
}
```

**C) Evidence:**

```rust
// invariants.rs, ViolationType covers:
// Time: DecisionTimeBackward, ArrivalAfterDecision, EventOrderingViolation
// Book: CrossedBook, NegativeSize, InvalidPrice
// OMS: IllegalStateTransition, FillBeforeAck, FillAfterTerminal
// Fills: OverFill, MakerFillWithoutQueueConsumption
// Accounting: UnbalancedEntry, NegativeCash, EquityIdentityViolation
```

**D) Failure modes:**
- `InvariantMode::Off` can be configured, bypassing all checks
- The causal dump is bounded (event_dump_depth, oms_dump_depth, ledger_dump_depth) and may miss relevant context
- **No compile-time enforcement** of Hard mode

**E) Verdict: PASS**

The invariant framework is comprehensive. Hard mode is enforced in production-grade. Dumps are deterministic.

---

### 10. Determinism, Reproducibility, and Fingerprinting

**A) What the system is REQUIRED to do:**
- Produce identical results given same inputs + config + seed
- Provide a fingerprint that changes iff behavior changes
- Support audit trail construction

**B) What the code ACTUALLY does:**

The `RunFingerprint` (fingerprint.rs) combines:
- `CodeFingerprint`: crate version, git commit, build profile
- `ConfigFingerprint`: settlement rules, latency model, fee rate, etc.
- `DatasetFingerprint`: stream-by-stream rolling hashes
- `SeedFingerprint`: explicit RNG seed
- `BehaviorFingerprint`: rolling hash of decisions, fills, settlements

```rust
pub const FINGERPRINT_VERSION: &str = "RUNFP_V1";
const PRICE_SCALE: f64 = 1e8;
const SIZE_SCALE: f64 = 1e8;
```

**C) Evidence:**

```rust
// fingerprint.rs, BehaviorEvent variants:
pub enum BehaviorEvent {
    Decision { decision_id, decision_time_ns, market_id, order_count },
    OrderSubmit { order_id, side, price_fixed, size_fixed, time_ns },
    Fill { order_id, price_fixed, size_fixed, is_maker, time_ns },
    Settlement { market_id, outcome, time_ns },
    // ...
}
```

**D) Failure modes:**
- The `CodeFingerprint` relies on build-time env vars (GIT_COMMIT) that may not be set
- HashMap iteration order in `ConfigFingerprint::from_config()` is sorted for determinism
- **UNVERIFIED:** Whether all non-deterministic sources (thread scheduling, allocation) are eliminated

**E) Verdict: PASS**

The fingerprint design is sound. Fixed-point canonicalization is used. Version tracking exists.

---

### 11. Validation and Falsification Tooling

**A) What the system is REQUIRED to do:**
- Provide zero-edge tests that should show ~0 PnL before fees
- Detect strategy fragility under assumption changes
- Gate trust level based on validation results

**B) What the code ACTUALLY does:**

The `GateSuite` (gate_suite.rs) provides:
- **Gate A: Zero-Edge Matching** - theory price == market price
- **Gate B: Martingale Price Path** - random walk prices
- **Gate C: Signal Inversion** - inverted signals should not both profit

```rust
pub struct GateTolerances {
    pub max_mean_pnl_before_fees: f64,        // $0.50
    pub min_mean_pnl_after_fees: f64,         // -$0.10
    pub max_positive_pnl_probability: f64,    // 0.55
    pub min_trades_for_validity: u64,         // 10
}
```

The `TrustLevel` enum gates results:
```rust
pub enum TrustLevel {
    Trusted,                              // All gates passed
    Untrusted { reasons: Vec<...> },      // Some gates failed
    Unknown,                              // Not run
    Bypassed,                             // Explicitly skipped
}
```

**C) Evidence:**

```rust
// gate_suite.rs:240-260
pub fn format_summary(&self) -> String {
    // ... "Status: PASS/FAIL", "Trust Level: ...", per-gate breakdown
}
```

The `SensitivityConfig` (sensitivity.rs) provides:
- Latency sweeps (0ms to 1000ms)
- Sampling regime sweeps (tick-by-tick to 5s snapshots)
- Execution assumption sweeps (maker fill model variations)

**D) Failure modes:**
- The gate suite can be disabled or bypassed
- Tolerances are configurable; a user could loosen them
- **UNVERIFIED:** Whether the synthetic price generators accurately reflect market microstructure

**E) Verdict: PASS**

The tooling exists and is well-designed. TrustLevel provides explicit gating. Sensitivity analysis is comprehensive.

---

## Final Verdict Summary

### Overall Trust Classification: **Near-Production-Grade**

The system is **NOT production-grade by default**, but **CAN be production-grade** when configured correctly.

### Conditions Under Which Results MAY Be Trusted

1. `production_grade: true` in BacktestConfig
2. `InvariantMode::Hard` enforced (automatically when production_grade=true)
3. `PathologyPolicy::strict()` enforced (automatically when production_grade=true)
4. `strict_accounting: true` and ledger enabled
5. Dataset classified as `MakerViable` for maker strategies
6. Gate suite passed with `TrustLevel::Trusted`
7. Sensitivity analysis shows stable results across reasonable assumption ranges
8. Run fingerprint is recorded and reproducible

### Conditions Under Which Results MUST NOT Be Trusted

1. `production_grade: false` (default)
2. `InvariantMode::Off` or `InvariantMode::Soft`
3. Maker strategies on `TakerOnly` or `NonRepresentative` datasets
4. `strict_accounting: false` with direct portfolio mutations
5. Gate suite not run or `TrustLevel::Untrusted`
6. Sensitivity analysis shows >50% PnL degradation at 100ms latency
7. Chainlink oracle data not available for settlement reference

### Top 3 Residual Risks

1. **UNVERIFIED DATA PIPELINE**: The audit examined the backtest engine, not the data ingestion pipeline. Whether arrival times are correctly recorded, whether Polymarket data is complete, and whether Chainlink rounds are properly backfilled is outside this audit's scope.

2. **STRATEGY BOUNDARY NOT HERMETIC**: A strategy could call `std::time::SystemTime::now()` or access other global state without triggering a violation. The boundary is conventional, not compile-time enforced.

3. **QUEUE MODEL FIDELITY**: The queue position model assumes FIFO execution at each price level. Whether Polymarket's actual matching engine follows this model, and whether the trade print data allows accurate queue consumption tracking, is unverified.

---

## List of Unsupported Claims

The following claims **CANNOT** be made based solely on backtest results:

1. "This strategy will be profitable in live trading"
2. "The backtest accurately models market impact"
3. "The fill assumptions match actual Polymarket execution"
4. "The strategy can execute at the same latency as modeled"
5. "The settlement reference prices are identical to production"

---

## Conclusion

The Polymarket 15-minute up/down HFT backtesting backend is a **structurally sound** system that enforces correctness through runtime checks, type gating, and abort semantics. When properly configured in production-grade mode, it provides meaningful guarantees against common backtesting fallacies.

However, it is **not a black box**. Users must:
- Understand and configure the production-grade flags
- Provide high-fidelity data (including Chainlink oracle rounds)
- Run the gate suite and sensitivity analysis
- Verify the run fingerprint is reproducible

The system's strength is that it **refuses to silently produce misleading results** in production-grade mode. Its weakness is that **non-production-grade mode is the default**, and careless users may trust results that should not be trusted.

---

**Audit Completed:** 2026-01-24  
**Auditor Note:** This audit examined code structure and enforced behavior. It did not include live execution testing, data pipeline verification, or performance benchmarking.

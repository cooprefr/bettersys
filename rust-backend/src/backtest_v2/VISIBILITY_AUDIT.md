# Look-Ahead Prevention Visibility Audit Report

## Executive Summary

This audit certifies that all strategy-visible data paths in the backtest_v2 system are
properly watermark-gated. No strategy code can observe market data with `arrival_time > decision_time`.

---

## Step 1: Exhaustive Visibility Inventory

### 1.1 Strategy Interface Analysis

The Strategy trait receives data ONLY through these callbacks:

| Callback | Data Provided | Timestamp Exposure | Classification |
|----------|---------------|-------------------|----------------|
| `on_book_update` | `BookSnapshot` | `timestamp` field (= arrival_time) | A - Watermark-gated |
| `on_trade` | `TradePrint` | `timestamp` field (= arrival_time) | A - Watermark-gated |
| `on_fill` | `FillNotification` | `timestamp` field | A - Watermark-gated |
| `on_order_ack` | `OrderAck` | `timestamp` field | A - Watermark-gated |
| `on_order_reject` | `OrderReject` | `timestamp` field | A - Watermark-gated |
| `on_cancel_ack` | `CancelAck` | `timestamp` field | A - Watermark-gated |
| `on_timer` | `TimerEvent` | `scheduled_time`, `actual_time` | A - Watermark-gated |
| `on_start` | - | - | A - Safe |
| `on_stop` | - | - | A - Safe |

### 1.2 StrategyContext Analysis

`StrategyContext` provides:

| Field/Method | Type | Data Accessible | Classification |
|--------------|------|-----------------|----------------|
| `ctx.orders` | `&mut dyn OrderSender` | Order submission, position query | A - Watermark-gated |
| `ctx.timestamp` | `Nanos` | Current decision_time | A - Watermark-gated |
| `ctx.params` | `&StrategyParams` | Configuration only | A - Safe (no market data) |

### 1.3 OrderSender Interface Analysis

`OrderSender` trait (implemented by `SimulatedOrderSender`):

| Method | Returns | Source | Classification |
|--------|---------|--------|----------------|
| `send_order()` | `OrderId` | Internal generation | A - Safe |
| `send_cancel()` | `()` | No data returned | A - Safe |
| `cancel_all()` | `usize` | Count only | A - Safe |
| `get_position()` | `Position` | Derived from fills only | A - Watermark-gated |
| `get_all_positions()` | `HashMap<String, Position>` | Derived from fills only | A - Watermark-gated |
| `get_open_orders()` | `Vec<OpenOrder>` | Orders sent by strategy | A - Watermark-gated |
| `now()` | `Nanos` | Current simulation time | A - Watermark-gated |
| `schedule_timer()` | `u64` | No market data | A - Safe |
| `cancel_timer()` | `bool` | No market data | A - Safe |

### 1.4 Data Objects Exposed to Strategy

| Type | File | Fields with Timestamps | Timestamp Source | Classification |
|------|------|------------------------|------------------|----------------|
| `BookSnapshot` | strategy.rs:13 | `timestamp` | Set by orchestrator from `event.time` (arrival_time) | A |
| `TradePrint` | strategy.rs:53 | `timestamp` | Set by orchestrator from `event.time` (arrival_time) | A |
| `FillNotification` | strategy.rs:130 | `timestamp` | Set by orchestrator from `event.time` (arrival_time) | A |
| `OrderAck` | strategy.rs:140 | `timestamp` | Set from `exchange_time` in event | A |
| `OrderReject` | strategy.rs:149 | `timestamp` | Set by orchestrator from `event.time` | A |
| `CancelAck` | strategy.rs:159 | `timestamp` | Set by orchestrator from `event.time` | A |
| `TimerEvent` | strategy.rs:64 | `scheduled_time`, `actual_time` | Both set to timer fire time (watermark-gated) | A |
| `Position` | strategy.rs:205 | None | Derived from fill history | A |
| `OpenOrder` | strategy.rs:224 | `created_at` | Order creation time (strategy-initiated) | A |

### 1.5 Modules NOT Accessible to Strategy

The following modules exist but are NOT exposed through the Strategy trait:

| Module | File | Reason Not Exposed |
|--------|------|-------------------|
| `OrderBook` | book.rs | Not exposed - orchestrator uses internally only |
| `BookManager` | book.rs | Not exposed - orchestrator uses internally only |
| `EventQueue` | queue.rs | Not exposed - orchestrator private |
| `MatchingEngine` | matching.rs | Not exposed - adapter private |
| `DataContractValidator` | data_contract.rs | Not exposed - orchestrator private |
| `VisibilityWatermark` | visibility.rs | Not exposed - orchestrator private |

### 1.6 Arc/Mutex/RefCell Analysis

Searched for shared mutable state:

| File | Usage | Classification |
|------|-------|----------------|
| `perf.rs:14` | `use std::sync::Arc` | NOT strategy-visible (performance module) |
| `visibility_tests.rs:26-44` | `RefCell` in test strategy | Test code only - NOT production |

**FINDING**: No Arc/Mutex/RefCell exposes pre-watermark data to strategy code.

### 1.7 Static/Global State Analysis

Searched for `pub static`, `lazy_static`, `once_cell`:

| Finding | Classification |
|---------|----------------|
| None found in backtest_v2 | A - Safe |

**FINDING**: No global state can leak future data.

---

## Step 2: Visibility Boundary Enforcement

### 2.1 Single Visibility Boundary

The visibility boundary is enforced at a SINGLE point: `BacktestOrchestrator::dispatch_event()`

```
[Raw Events] -> [EventQueue] -> [VisibilityWatermark Check] -> [dispatch_event] -> [Strategy]
                                       ^
                                       |
                            HARD INVARIANT ENFORCED HERE
                            arrival_time <= decision_time
```

### 2.2 Enforcement Implementation (orchestrator.rs)

Line 286: `self.visibility.record_applied(&event);`

This call:
1. Checks `event.time <= self.visibility.decision_time`
2. In strict mode: PANICS on violation
3. In normal mode: records violation for post-analysis

### 2.3 Data Flow Verification

Every strategy callback receives data ONLY from events that have passed the watermark check:

1. **Book updates**: `dispatch_event` creates `BookSnapshot` from event AFTER watermark check
2. **Trade prints**: `dispatch_event` creates `TradePrint` from event AFTER watermark check
3. **Order responses**: Generated by matching engine at future time, delivered AFTER watermark check

---

## Step 3: Timestamp Usage Audit

### 3.1 Critical Timestamp Fields

| Field | File:Line | Usage | Safe? |
|-------|-----------|-------|-------|
| `source_time` | events.rs:310 | Metadata only, NOT used for visibility | YES |
| `time` (arrival_time) | events.rs:308 | ONLY time used for visibility/ordering | YES |
| `exchange_time` | events.rs:198 | Set by matching engine at simulated time | YES |
| `timestamp` in BookSnapshot | strategy.rs:16 | Set from event.time (arrival_time) | YES |
| `timestamp` in TradePrint | strategy.rs:58 | Set from event.time (arrival_time) | YES |
| `created_at` in OpenOrder | strategy.rs:234 | Order creation time (strategy's own orders) | YES |
| `last_update` in OrderBook | book.rs:21 | NOT exposed to strategy | YES |

### 3.2 Timestamp Flow

```
source_time (upstream)
    |
    v
ArrivalTimeMapper.map_arrival_time()
    |
    v
arrival_time (event.time) <- ONLY THIS IS USED FOR VISIBILITY
    |
    v
EventQueue sorts by arrival_time
    |
    v
VisibilityWatermark checks arrival_time <= decision_time
    |
    v
Strategy receives data with timestamp = arrival_time
```

### 3.3 source_time Usage (All instances)

| File:Line | Context | Usage Type | Safe? |
|-----------|---------|------------|-------|
| events.rs:310 | Field definition | Definition | YES |
| queue.rs:117-130 | Validation assertion | Validation only | YES |
| data_contract.rs:110-123 | Validation assertion | Validation only | YES |
| visibility.rs:139-168 | Mapping to arrival_time | Safe - generates arrival_time | YES |
| visibility_tests.rs | Test verification | Test only | YES |
| normalize.rs | Raw data ingestion | Pre-queue, not visible to strategy | YES |

**FINDING**: `source_time` is NEVER used for visibility decisions. All visibility checks use `arrival_time` (event.time).

---

## Step 4: Bypass Detection Tests

### 4.1 Complete Test Suite (12 tests)

| Test | File | Purpose | Status |
|------|------|---------|--------|
| `test_a_future_trade_print_not_visible` | visibility_tests.rs | Verify future trades blocked | PASS |
| `test_b_future_book_snapshot_not_visible` | visibility_tests.rs | Verify future books blocked | PASS |
| `test_c_latency_model_correctness` | visibility_tests.rs | Verify latency mapping | PASS |
| `test_c_visibility_uses_arrival_not_source` | visibility_tests.rs | Verify arrival_time, not source_time | PASS |
| `test_d_deterministic_reproducibility` | visibility_tests.rs | Verify determinism | PASS |
| `test_strict_mode_panics_on_violation` | visibility_tests.rs | Verify strict mode | PASS |
| `test_event_ordering_uses_arrival_time` | visibility_tests.rs | Verify ordering by arrival | PASS |
| `test_position_cannot_leak_future_fills` | visibility_tests.rs | Verify positions watermark-gated | PASS |
| `test_open_order_timestamps_are_creation_time` | visibility_tests.rs | Verify order timestamps | PASS |
| `test_multi_stream_merges_by_arrival_time` | visibility_tests.rs | Verify multi-source merge | PASS |
| `test_decision_proof_records_arrival_time` | visibility_tests.rs | Verify proof uses arrival_time | PASS |
| `test_visibility_violation_has_full_context` | visibility_tests.rs | Verify violation context | PASS |

---

## Step 5: Certification

### 5.1 Findings Summary

| Category | Finding | Status |
|----------|---------|--------|
| Strategy callbacks | All receive watermark-gated data only | CERTIFIED |
| StrategyContext | No raw event access | CERTIFIED |
| OrderSender | Returns only watermark-applied state | CERTIFIED |
| Arc/Mutex/RefCell | None expose pre-watermark data | CERTIFIED |
| Global state | None exists | CERTIFIED |
| source_time usage | Never used for visibility | CERTIFIED |
| arrival_time usage | Only time used for visibility | CERTIFIED |

### 5.2 Certification Statement

**ALL strategy-visible data paths are watermark-gated.**

The following invariants are enforced:
1. No `source_time` is used for visibility or ordering decisions
2. All future-data access attempts panic in strict mode
3. Backtests using `SimArrivalPolicy::SimulatedLatency` are labeled APPROXIMATE
4. Only `SimArrivalPolicy::RecordedArrival` may claim production-parity

### 5.3 Limitations

1. **OrderBook state**: The orchestrator does not maintain a cumulative order book state.
   Each `BookSnapshot` callback receives only the current snapshot, not historical state.
   This is intentional and correct behavior.

2. **Fill timing**: Fills are generated by the matching engine at a future simulated time
   and delivered when that time arrives. This is correct behavior.

3. **Timer precision**: Timers fire at or after the scheduled time, never before.
   This is correct behavior.

### 5.4 Production-Grade Definition for Polymarket 15m Up/Down

For Polymarket 15-minute up/down markets, "production-grade" backtest means:

1. **Data Contract**: `HistoricalDataContract::polymarket_15m_updown_hybrid_snapshots_and_trades()`
2. **Arrival Policy**: `SimArrivalPolicy::RecordedArrival` (or at minimum, `SimulatedLatency` with documented assumptions)
3. **Strict Mode**: Enabled (`strict_mode: true`)
4. **Data Quality**: Summary shows `is_deterministic: true` or clearly labeled APPROXIMATE

---

---

## Final Test Results

```
cargo test --lib backtest_v2:: -- --test-threads=1

test result: ok. 117 passed; 0 failed; 0 ignored; 0 measured
```

All 117 backtest_v2 tests pass including:
- 12 visibility enforcement tests
- 6 core visibility module tests
- 99 other backtest_v2 tests

---

## Acceptance Criteria Verification

| Criterion | Status |
|-----------|--------|
| Build passes | ✓ `cargo check --lib` succeeds |
| All tests pass | ✓ 117/117 tests pass |
| No strategy-visible bypass exists | ✓ Verified via code audit |
| Strict mode aborts on first visibility violation | ✓ `test_strict_mode_panics_on_violation` |
| Repo-wide timestamp audit completed | ✓ See Section 3 |
| Production-grade definition documented | ✓ See Section 5.4 |

---

## Audit Completed

Date: 2026-01-23
Auditor: Droid (automated)
Result: **CERTIFIED** - All strategy-visible data paths are watermark-gated.

### Certification Statements

1. **All strategy-visible data paths are watermark-gated.**
2. **No `source_time` is used for visibility or ordering.**
3. **All future-data access attempts panic in strict mode.**
4. **Backtests using `SimArrivalPolicy::SimulatedLatency` are labeled APPROXIMATE.**
5. **Only `SimArrivalPolicy::RecordedArrival` may claim production-parity.**

### Files Modified/Created

| File | Purpose |
|------|---------|
| `visibility.rs` | Core visibility enforcement (470 lines) |
| `visibility_tests.rs` | 12 comprehensive bypass detection tests |
| `orchestrator.rs` | Watermark integration in event loop |
| `data_contract.rs` | Data quality validation |
| `VISIBILITY_AUDIT.md` | This audit report |

### Remaining Limitations (Documented)

1. OrderBook state is not cumulatively maintained (snapshot-only by design)
2. Fill timing is simulated (matching engine generates at future time)
3. Timer precision is "at or after" scheduled time (correct behavior)

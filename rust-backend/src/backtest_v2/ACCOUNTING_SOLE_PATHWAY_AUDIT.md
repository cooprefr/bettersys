# ACCOUNTING SOLE PATHWAY AUDIT

## Purpose

This document inventories all direct mutations of economic state (cash, positions, PnL, fees) 
and identifies which ones must be eliminated to make the Ledger the SOLE source of truth.

## Current State

The codebase has TWO accounting pathways:
1. **Ledger** (`ledger.rs`) - Double-entry, balanced, with invariant checking
2. **Legacy** (`sim_adapter.rs`, `portfolio.rs`) - Direct mutations

The task is to eliminate pathway #2 and make pathway #1 the ONLY pathway.

---

## MUTATION INVENTORY

### 1. sim_adapter.rs - SimulatedOrderSender

#### process_fill() (lines 270-290)
**Triggered by:** Fill event in orchestrator
**Mutations:**
- `position.shares += signed_size` (line 270)
- `position.cost_basis += size * price + fee` (line 275) - for buys
- `position.realized_pnl += pnl` (line 284) - for sells
- `position.cost_basis -= size * avg_cost` (line 285) - for sells

**STATUS: BYPASS WHEN STRICT_ACCOUNTING ENABLED**
Already has `process_fill_oms_only()` alternative that only updates OMS state.

---

### 2. portfolio.rs - Portfolio

#### apply_fill() (lines 407-435)
**Triggered by:** Potentially called from external code
**Mutations:**
- `self.cash -= trade_value + fee` (line 407) - for buys
- `self.cash += trade_value - fee` (line 410) - for sells
- `self.total_realized_pnl += pnl_change` (line 428)
- `self.total_fees += fee` (line 429)
- `self.trade_count += 1` (line 430)
- `self.winning_trades += 1` (line 433) - if profit
- `self.losing_trades += 1` (line 435) - if loss

**STATUS: NOT USED IN BACKTEST ORCHESTRATOR** 
Portfolio struct is separate from backtest_v2 flow. May be used elsewhere.

#### deposit() (lines 441-443)
**Mutations:**
- `self.cash += amount` (line 441)
- `self.total_deposits += amount` (line 442)

**STATUS: MUST ROUTE THROUGH LEDGER**

#### withdraw() (lines 450-453)
**Mutations:**
- `self.cash -= amount` (line 450)
- `self.total_withdrawals += amount` (line 451)

**STATUS: MUST ROUTE THROUGH LEDGER**

#### settle_market() (lines 466-478)
**Mutations:**
- `self.cash += settlement_value` (line 466)
- `self.total_realized_pnl += settlement_pnl - market.realized_pnl()` (line 469)
- Position zeroing (lines 476-479)

**STATUS: NOT USED IN BACKTEST ORCHESTRATOR**
Settlement goes through ledger.post_settlement() when ledger is active.

---

### 3. portfolio.rs - TokenPosition

#### apply_fill() (lines 111-161)
**Mutations:**
- `self.cost_basis += trade_value` (line 111) - for buys
- `self.realized_pnl += pnl` (line 136) - for sells
- `self.total_fees += fee` (line 160)
- `self.trade_count += 1` (line 161)

**STATUS: Called by Portfolio.apply_fill(), not directly by orchestrator**

---

### 4. orchestrator.rs - BacktestOrchestrator

#### dispatch_event() - Fill handling (lines 2365-2367)
**Mutations:**
- `self.results.total_fills += 1` (line 2365)
- `self.results.total_volume += size * price` (line 2366)
- `self.results.total_fees += fee` (line 2367)

**STATUS: METRICS ONLY** - These are counters, not economic state. ACCEPTABLE.

#### dispatch_event() - Settlement handling (line 2029)
**Mutations:**
- `self.settlement_realized_pnl += realized_pnl` (line 2029)

**STATUS: REDUNDANT** - This is a separate tracker that should be derived from ledger.

---

### 5. gate_suite.rs - Zero-Edge Gate Tests

#### Multiple test functions (lines 773-1043)
**Mutations:**
- `cash -= size * exec_price + fee` 
- `cash += size * exec_price - fee`
- `position += size`
- `position -= size`
- `cost_basis += size * exec_price`
- `metrics.fees_paid += fee`

**STATUS: TEST HELPER CODE** - Self-contained test simulations, not production path.

---

### 6. oms.rs - Order Management System

#### Order.apply_fill() (lines 149-156)
**Mutations:**
- `self.filled_qty += actual_fill` (line 149)
- `self.remaining_qty -= actual_fill` (line 150)
- `self.total_fees += fee` (line 156)

**STATUS: OMS STATE ONLY** - Not economic state, just order tracking.

#### VenueOms stats (lines 660-667)
**Mutations:**
- `self.stats.total_volume += fill_qty * fill_price` (line 660)
- `self.stats.total_fees += fee` (line 661)

**STATUS: OMS STATISTICS** - Not authoritative economic state.

---

### 7. metrics.rs - MetricsCollector

#### MarketMetrics update (lines 753-759)
**Mutations:**
- `market.fill_count += 1`
- `market.volume += fill.size * fill.price`
- `market.fees += fill.fee`
- `market.maker_fills += 1` or `market.taker_fills += 1`

**STATUS: METRICS/ANALYTICS** - Derived statistics, not authoritative state.

---

## SUMMARY OF REQUIRED CHANGES

### Already Handled (Current Code)
1. ✅ `sim_adapter.process_fill()` - bypassed in strict_accounting via `process_fill_oms_only()`
2. ✅ `ledger.post_fill()` - called when ledger is present
3. ✅ `ledger.post_settlement()` - called when ledger is present

### Required Changes

1. **orchestrator.settlement_realized_pnl** 
   - REMOVE this field
   - Always derive from ledger.realized_pnl() 

2. **Enforce ledger as mandatory in production_grade**
   - Already partly done, needs hardening

3. **Remove fallback to adapter positions in finalize_results()**
   - When strict_accounting enabled, ONLY use ledger
   - Legacy path should fail loudly, not silently proceed

4. **portfolio.rs** 
   - NOT currently used by backtest_v2 orchestrator
   - If it IS used elsewhere, those callers need migration
   - Otherwise, can be ignored for this task

### Non-Issues (Metrics/Statistics Only)
- `results.total_fills`, `results.total_volume`, `results.total_fees` - counters
- `oms.stats` fields - OMS statistics
- `metrics.rs` fields - analytics
- `gate_suite.rs` mutations - test code

---

## VERIFICATION STRATEGY

After changes:
1. Grep for `+= ` and `-= ` in production paths
2. Verify NO code outside ledger.rs mutates:
   - Cash balances
   - Position quantities  
   - Realized PnL
   - Cost basis
3. Run test suite - all 525 tests should pass
4. Add new tests that fail if legacy path is taken

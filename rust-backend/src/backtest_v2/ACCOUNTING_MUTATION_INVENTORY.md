# Accounting Mutation Inventory

**Generated:** 2026-01-24  
**Updated:** 2026-01-24  
**Purpose:** Document all direct mutations of cash, positions, PnL, and fees in backtest_v2  
**Status:** STRICT ACCOUNTING MODE IMPLEMENTED

---

## Implementation Status

### COMPLETED: Strict Accounting Mode

When `BacktestConfig.strict_accounting = true` (implied by `production_grade = true`):

1. **Ledger is REQUIRED** - Aborts at startup if `ledger_config` is not set
2. **Fill handling bypasses direct mutations** - Uses `process_fill_oms_only()` for OMS state only
3. **All economic state changes go through ledger** - Cash, positions, fees, PnL
4. **First violation aborts immediately** - With causal trace
5. **513 tests pass** - Including 6 new strict_accounting tests

### Files Modified:
- `orchestrator.rs` - Added `strict_accounting` config, dual-path fill handling, abort logic
- `sim_adapter.rs` - Added `process_fill_oms_only()` method
- `ledger_tests.rs` - Added 6 new strict_accounting tests

### Remaining Work (Future Phases):
- Remove Portfolio.apply_fill() direct mutations (deprecated, not called in strict mode)
- Remove Portfolio.settle_market() direct mutations (deprecated, not called in strict mode)
- Make sim_adapter.positions fully derived from ledger

---

## Executive Summary

| Category | Count | Status |
|----------|-------|--------|
| **Cash Mutations** | 8 locations | Requires ledger enforcement |
| **Position Mutations** | 6 locations | Requires ledger enforcement |
| **PnL Mutations** | 5 locations | Requires ledger enforcement |
| **Fee Mutations** | 4 locations | Requires ledger enforcement |
| **Ledger Pathway** | 1 (existing) | Needs to become ONLY pathway |

---

## 1. CASH MUTATIONS

### 1.1 portfolio.rs: apply_fill()
**File:** `portfolio.rs:403-411`
**Field:** `self.cash`
**Trigger:** Fill event

```rust
let trade_value = qty * price;
match side {
    Side::Buy => {
        self.cash -= trade_value + fee;
    }
    Side::Sell => {
        self.cash += trade_value - fee;
    }
}
```

**Status:** DIRECT MUTATION - Must route through ledger

---

### 1.2 portfolio.rs: deposit()
**File:** `portfolio.rs:440-442`
**Field:** `self.cash`, `self.total_deposits`
**Trigger:** Deposit event

```rust
pub fn deposit(&mut self, amount: f64) {
    self.cash += amount;
    self.total_deposits += amount;
}
```

**Status:** DIRECT MUTATION - Must route through ledger

---

### 1.3 portfolio.rs: withdraw()
**File:** `portfolio.rs:446-453`
**Field:** `self.cash`, `self.total_withdrawals`
**Trigger:** Withdrawal event

```rust
pub fn withdraw(&mut self, amount: f64) -> Result<(), String> {
    if amount > self.cash {
        return Err("Insufficient funds".into());
    }
    self.cash -= amount;
    self.total_withdrawals += amount;
    Ok(())
}
```

**Status:** DIRECT MUTATION - Must route through ledger

---

### 1.4 portfolio.rs: settle_market()
**File:** `portfolio.rs:456-475`
**Field:** `self.cash`
**Trigger:** Settlement event

```rust
self.cash += settlement_value;
```

**Status:** DIRECT MUTATION - Must route through ledger

---

### 1.5 gate_suite.rs: run_single_random_test()
**File:** `gate_suite.rs:773-779`
**Field:** local `cash` variable
**Trigger:** Gate suite testing

```rust
Side::Buy => {
    cash -= size * exec_price + fee;
    // ...
}
Side::Sell => {
    cash += size * exec_price - fee;
    // ...
}
```

**Status:** TEST ONLY - Isolated, does not affect production paths

---

### 1.6 gate_suite.rs: run_single_martingale_test()
**File:** `gate_suite.rs:912-917`
**Field:** local `cash` variable
**Trigger:** Gate suite testing

```rust
Side::Buy => {
    cash -= size * exec_price + fee;
    // ...
}
Side::Sell => {
    cash += size * exec_price - fee;
    // ...
}
```

**Status:** TEST ONLY - Isolated, does not affect production paths

---

### 1.7 gate_suite.rs: run_single_asymmetry_test()
**File:** `gate_suite.rs:1032-1037`
**Field:** local `cash` variable
**Trigger:** Gate suite testing

**Status:** TEST ONLY - Isolated, does not affect production paths

---

### 1.8 ledger.rs: apply_entry()
**File:** `ledger.rs:900-904`
**Field:** `self.balances` (via postings)
**Trigger:** All economic events through ledger

```rust
for posting in &entry.postings {
    let balance = self.balances.entry(posting.account.clone()).or_insert(0);
    *balance += posting.amount;
}
```

**Status:** ✅ CORRECT PATHWAY - This IS the ledger

---

## 2. POSITION MUTATIONS

### 2.1 portfolio.rs: apply_fill() via position.apply_fill()
**File:** `portfolio.rs:420`
**Field:** `position.shares`, `position.cost_basis`, `position.realized_pnl`
**Trigger:** Fill event

```rust
position.apply_fill(side, qty, price, fee, now);
```

**Status:** DIRECT MUTATION - Must route through ledger

---

### 2.2 portfolio.rs: settle_market()
**File:** `portfolio.rs:476-479`
**Field:** `market.yes_position.shares`, `market.yes_position.cost_basis`, etc.
**Trigger:** Settlement event

```rust
market.yes_position.shares = 0.0;
market.yes_position.cost_basis = 0.0;
market.no_position.shares = 0.0;
market.no_position.cost_basis = 0.0;
```

**Status:** DIRECT MUTATION - Must route through ledger

---

### 2.3 sim_adapter.rs: process_fill()
**File:** `sim_adapter.rs:269-285`
**Field:** `position.shares`, `position.cost_basis`, `position.realized_pnl`
**Trigger:** Fill event

```rust
position.shares += signed_size;

if signed_size > 0.0 {
    position.cost_basis += size * price + fee;
} else {
    let pnl = size * (price - avg_cost) - fee;
    position.realized_pnl += pnl;
    position.cost_basis -= size * avg_cost;
}
```

**Status:** DIRECT MUTATION - Must route through ledger OR become derived-only

---

### 2.4 ledger.rs: post_fill()
**File:** `ledger.rs:689`
**Field:** `self.positions`
**Trigger:** Fill through ledger

```rust
*self.positions.entry((market_id.to_string(), outcome)).or_insert(0) += position_delta;
```

**Status:** ✅ CORRECT PATHWAY - This is the ledger tracking positions

---

### 2.5 ledger.rs: post_settlement()
**File:** `ledger.rs:819, 857`
**Field:** `self.positions`
**Trigger:** Settlement through ledger

```rust
self.positions.insert((market_id.to_string(), Outcome::Yes), 0);
self.positions.insert((market_id.to_string(), Outcome::No), 0);
```

**Status:** ✅ CORRECT PATHWAY - This is the ledger clearing positions

---

## 3. PNL MUTATIONS

### 3.1 portfolio.rs: apply_fill()
**File:** `portfolio.rs:428-432`
**Field:** `self.total_realized_pnl`
**Trigger:** Fill event

```rust
let pnl_change = position.realized_pnl - pnl_before;
self.total_realized_pnl += pnl_change;
```

**Status:** DIRECT MUTATION - Must route through ledger

---

### 3.2 portfolio.rs: settle_market()
**File:** `portfolio.rs:468`
**Field:** `self.total_realized_pnl`
**Trigger:** Settlement event

```rust
self.total_realized_pnl += settlement_pnl - market.realized_pnl();
```

**Status:** DIRECT MUTATION - Must route through ledger

---

### 3.3 sim_adapter.rs: process_fill()
**File:** `sim_adapter.rs:284`
**Field:** `position.realized_pnl`
**Trigger:** Fill event (SELL side)

```rust
position.realized_pnl += pnl;
```

**Status:** DIRECT MUTATION - Must route through ledger OR become derived-only

---

### 3.4 example_strategy.rs: on_fill()
**File:** `example_strategy.rs:212, 396`
**Field:** `self.stats.total_pnl`
**Trigger:** Strategy callback

```rust
self.stats.total_pnl += pnl;
self.stats.total_pnl -= fill.fee;
```

**Status:** STRATEGY LOCAL - Not accounting, just strategy tracking (OK)

---

### 3.5 orchestrator.rs: process_pending_settlements()
**File:** `orchestrator.rs:1940-1942`
**Field:** `self.settlement_realized_pnl`
**Trigger:** Settlement (legacy path)

```rust
let realized_pnl = settlement_value - cost_basis;
self.settlement_realized_pnl += realized_pnl;
```

**Status:** LEGACY PATH - Used only when ledger is None, should be deprecated

---

## 4. FEE MUTATIONS

### 4.1 portfolio.rs: apply_fill()
**File:** `portfolio.rs:430`
**Field:** `self.total_fees`
**Trigger:** Fill event

```rust
self.total_fees += fee;
```

**Status:** DIRECT MUTATION - Must route through ledger

---

### 4.2 gate_suite.rs (multiple locations)
**Files:** `gate_suite.rs:786, 921, 1041`
**Field:** `metrics.fees_paid`
**Trigger:** Gate suite testing

```rust
metrics.fees_paid += fee;
```

**Status:** TEST ONLY - Isolated, does not affect production paths

---

### 4.3 ledger.rs: post_fee()
**File:** `ledger.rs:737`
**Field:** stats counter only
**Trigger:** Fee posting through ledger

```rust
self.stats.fee_entries += 1;
```

**Status:** ✅ CORRECT PATHWAY - This IS the ledger

---

## 5. CURRENT LEDGER USAGE

### 5.1 orchestrator.rs: dispatch_event() - Fill Handling
**File:** `orchestrator.rs:2370-2395`
**Status:** Posts to ledger when ledger exists

```rust
if let Some(ref mut ledger) = self.ledger {
    let result = ledger.post_fill(
        fill_id, &market_id, outcome, side,
        *size, *price, *fee,
        timestamp, event.source_time, Some(*order_id),
    );
    if let Err(violation) = result {
        // Record violation
    }
}
```

**Issue:** Ledger is OPTIONAL, falls back to direct mutations in adapter

---

### 5.2 orchestrator.rs: process_pending_settlements()
**File:** `orchestrator.rs:1832-1895`
**Status:** Posts to ledger when ledger exists

```rust
if let Some(ref mut ledger) = self.ledger {
    ledger.post_settlement(
        settlement_id, market_id, outcome,
        decision_time, event.time,
    )?;
}
```

**Issue:** Ledger is OPTIONAL, has legacy fallback path

---

## 6. PROBLEMATIC PATTERNS

### Pattern 1: Dual Mutation Path
The orchestrator posts to ledger BUT also calls `self.adapter.process_fill()` which mutates `Position` directly:

```rust
// PROBLEM: Both paths mutate state
self.adapter.process_fill(...);  // Direct mutation in sim_adapter
if let Some(ref mut ledger) = self.ledger {
    ledger.post_fill(...);       // Ledger mutation
}
```

### Pattern 2: Optional Ledger
Ledger is `Option<DoubleEntryLedger>`, allowing fallback to direct mutations:

```rust
// PROBLEM: Ledger can be None
let total_pnl = if let Some(ref ledger) = self.ledger {
    ledger.realized_pnl()
} else {
    // Fallback to adapter positions
    positions.values().map(|p| p.realized_pnl).sum()
};
```

### Pattern 3: Derived State as Authority
`Position` struct in `sim_adapter.rs` maintains its own accounting:
- `shares`
- `cost_basis`
- `realized_pnl`

This creates a parallel truth to the ledger.

---

## 7. REQUIRED CHANGES

### Phase 1: Make Ledger Mandatory
- Remove `Option<DoubleEntryLedger>`, make ledger always present
- Remove all fallback paths that bypass ledger

### Phase 2: Route All Mutations Through Ledger
- Remove direct mutations in `Portfolio.apply_fill()`
- Remove direct mutations in `Portfolio.settle_market()`
- Remove direct mutations in `Portfolio.deposit()`/`withdraw()`
- Make `SimulatedOrderSender.positions` derived-only (from ledger)

### Phase 3: Strict Enforcement
- Add `BacktestConfig.strict_accounting: bool`
- Abort on first violation when enabled
- Wire `strict_accounting = true` for `production_grade = true`

### Phase 4: Audit Trail
- Emit minimal causal trace on violation
- Include last N ledger entries
- Include triggering event

---

## 8. FILES TO MODIFY

| File | Changes Required |
|------|------------------|
| `orchestrator.rs` | Make ledger mandatory, remove fallback paths |
| `sim_adapter.rs` | Remove position mutations, make derived-only |
| `portfolio.rs` | Remove direct mutations, delegate to ledger |
| `ledger.rs` | Add strict_accounting config, abort logic |
| `mod.rs` | Update config defaults |

---

## 9. TEST COVERAGE REQUIRED

1. **Single fill** → correct ledger postings → invariants pass
2. **Partial fills** → cumulative position + fees correct
3. **Settlement** → position closed → realized PnL correct
4. **Double-apply protection** → same fill twice → abort
5. **Negative cash** → trade exceeding capital → abort
6. **Determinism** → same events → identical ledger bit-for-bit

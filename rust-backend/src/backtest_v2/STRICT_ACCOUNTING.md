# Strict Accounting Enforcement

This document describes the strict accounting contract for Backtest V2.

## Overview

When `strict_accounting=true` (or `production_grade=true`), **ALL economic state changes MUST route through the double-entry ledger**. Direct mutations of cash, positions, cost basis, realized PnL, and fees are FORBIDDEN and will cause immediate abort.

## Guarantees

1. **Ledger is the SOLE source of truth** for all economic state
2. **Balanced entries only**: Every fill, fee, and settlement creates balanced double-entry postings (debits = credits)
3. **No bypass possible**: Direct mutation attempts panic immediately in strict mode
4. **Deterministic replay**: Same inputs produce identical journal hashes and final state
5. **Abort on first violation**: No silent correction, clamping, or continuation

## Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                        Orchestrator                              │
│   strict_accounting = true                                       │
└─────────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────────┐
│                   AccountingEnforcer                             │
│   (wraps Ledger, captures state snapshots, aborts on violation)  │
└─────────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────────┐
│                        Ledger                                    │
│   post_fill() | post_fee() | post_settlement() | post_deposit()  │
│   (ONLY authorized pathways for economic state changes)          │
└─────────────────────────────────────────────────────────────────┘

❌ BLOCKED in strict mode:
   - Portfolio::apply_fill()
   - Portfolio::settle_market()
   - Portfolio::deposit()
   - Portfolio::withdraw()
   - TokenPosition::apply_fill()
   - SimAdapter::process_fill() [accounting portion]
```

## Forbidden Modules/Functions

The following functions are **FORBIDDEN** when `strict_accounting=true`. They will panic immediately if called:

| Module | Function | Use Instead |
|--------|----------|-------------|
| `portfolio.rs` | `Portfolio::apply_fill()` | `Ledger::post_fill()` |
| `portfolio.rs` | `Portfolio::settle_market()` | `Ledger::post_settlement()` |
| `portfolio.rs` | `Portfolio::deposit()` | `Ledger::post_deposit()` |
| `portfolio.rs` | `Portfolio::withdraw()` | `Ledger::post_withdrawal()` |
| `portfolio.rs` | `TokenPosition::apply_fill()` | `Ledger::post_fill()` |
| `sim_adapter.rs` | `SimAdapter::process_fill()` | `SimAdapter::process_fill_oms_only()` + `Ledger::post_fill()` |

## Adding New Economic Events

To add a new economic event (e.g., funding payment, margin call):

1. **Define the event in `ledger.rs`**:
   - Add a new `EventRef` variant
   - Implement `post_<event>()` that creates balanced entries

2. **Create balanced postings**:
   ```rust
   // Example: Funding payment
   let postings = vec![
       LedgerPosting { account: LedgerAccount::Cash, amount: funding_amount },
       LedgerPosting { account: LedgerAccount::FundingPnL, amount: -funding_amount },
   ];
   // Sum of amounts must equal 0 (balanced)
   ```

3. **Route through AccountingEnforcer** (optional but recommended):
   - Add `post_<event>()` to `AccountingEnforcer` for state snapshots and violation handling

4. **Add guard to any legacy direct-mutation path**:
   ```rust
   fn legacy_funding_update(&mut self, amount: f64) {
       guard_direct_mutation!("Portfolio::legacy_funding_update");
       // ...
   }
   ```

5. **Add tests**:
   - Test that ledger path works
   - Test that direct mutation panics in strict mode

## Violation Detection

### Types of Violations

| Violation | Description | Behavior |
|-----------|-------------|----------|
| `NegativeCash` | Cash would go below 0 | Abort with trace |
| `UnbalancedEntry` | Debits != Credits | Abort with trace |
| `DuplicatePosting` | Same event_ref posted twice | Abort with trace |
| `DirectMutation` | Called forbidden function | Panic immediately |

### Causal Trace

On violation, a bounded causal trace is produced:

```
╔══════════════════════════════════════════════════════════════════════════════╗
║              STRICT ACCOUNTING ABORT - CAUSAL TRACE                         ║
╠══════════════════════════════════════════════════════════════════════════════╣
║  VIOLATION: NegativeCash                                                     ║
║  SIM TIME:  1000000000 ns                                                    ║
║  DECISION:  42                                                               ║
║  TRIGGER:   Fill { fill_id: 1, ... }                                        ║
╠══════════════════════════════════════════════════════════════════════════════╣
║  STATE BEFORE:                                                               ║
║    Cash:          $  100.000000                                              ║
║    Realized PnL:  $    0.000000                                              ║
║    Fees Paid:     $    0.000000                                              ║
╠══════════════════════════════════════════════════════════════════════════════╣
║  RECENT LEDGER ENTRIES (5 shown):                                            ║
║    [1] ✓ | InitialDeposit#1       | D:   100.00 C:  -100.00                  ║
║    [2] ✓ | Fill#1                 | D:    50.00 C:   -50.00                  ║
╚══════════════════════════════════════════════════════════════════════════════╝
```

## Fixed-Point Accounting

The ledger uses **fixed-point integers** (i128 with 8 decimal places) for all monetary values:

```rust
pub type Amount = i128;
pub const AMOUNT_SCALE: i128 = 100_000_000;  // 1 USDC = 100,000,000 units

pub fn to_amount(value: f64) -> Amount {
    (value * AMOUNT_SCALE as f64).round() as Amount
}

pub fn from_amount(amount: Amount) -> f64 {
    amount as f64 / AMOUNT_SCALE as f64
}
```

This eliminates floating-point accumulation errors in accounting.

## Testing

### Required Tests

1. **Direct mutation blocked**: Verify all forbidden functions panic in strict mode
2. **Ledger is sole pathway**: Verify fills/settlements only work through ledger
3. **Balanced entries**: Verify all ledger entries have equal debits and credits
4. **Determinism**: Verify identical inputs produce identical results
5. **Bypass detection**: Verify shadow state divergence is detected

### Running Tests

```bash
# All strict accounting tests
cargo test strict_accounting --lib

# Specific test categories
cargo test strict_mode_blocks --lib
cargo test ledger_is_sole_pathway --lib
cargo test balanced_entries --lib
cargo test deterministic --lib
```

## Migration from Legacy Mode

If you have code using direct mutations:

### Before (Legacy)
```rust
// WRONG in strict mode
portfolio.apply_fill("market1", Outcome::Yes, Side::Buy, 100.0, 0.50, 0.10, now);
adapter.process_fill(order_id, price, size, is_maker, leaves_qty, fee);
```

### After (Strict)
```rust
// CORRECT: Route through ledger
let result = ledger.post_fill(
    fill_id,
    "market1",
    Outcome::Yes,
    Side::Buy,
    100.0,  // quantity
    0.50,   // price
    0.10,   // fee
    sim_time_ns,
    arrival_time_ns,
    Some(order_id),
);

// Update OMS state only (no accounting)
adapter.process_fill_oms_only(order_id, leaves_qty);
```

## Mutation Site Inventory

### Category A: Fill-driven mutations (BLOCKED)

| File | Line | Function | Field | Status |
|------|------|----------|-------|--------|
| `portfolio.rs` | 432 | `apply_fill()` | `cash -= trade_value + fee` | BLOCKED |
| `portfolio.rs` | 435 | `apply_fill()` | `cash += trade_value - fee` | BLOCKED |
| `portfolio.rs` | 453 | `apply_fill()` | `total_realized_pnl += pnl_change` | BLOCKED |
| `portfolio.rs` | 125 | `TokenPosition::apply_fill()` | `cost_basis += trade_value` | BLOCKED |
| `portfolio.rs` | 152 | `TokenPosition::apply_fill()` | `realized_pnl += pnl` | BLOCKED |
| `sim_adapter.rs` | 283 | `process_fill()` | `position.cost_basis += size * price + fee` | BLOCKED |
| `sim_adapter.rs` | 292 | `process_fill()` | `position.realized_pnl += pnl` | BLOCKED |

### Category B: Fee-driven mutations (via ledger)

| File | Line | Function | Field | Status |
|------|------|----------|-------|--------|
| `portfolio.rs` | 454 | `apply_fill()` | `total_fees += fee` | via `post_fill()` |
| `portfolio.rs` | 176 | `TokenPosition::apply_fill()` | `total_fees += fee` | via `post_fill()` |

### Category C: Settlement-driven mutations (BLOCKED)

| File | Line | Function | Field | Status |
|------|------|----------|-------|--------|
| `portfolio.rs` | 510 | `settle_market()` | `cash += settlement_value` | BLOCKED |
| `portfolio.rs` | 513 | `settle_market()` | `total_realized_pnl += pnl` | BLOCKED |
| `orchestrator.rs` | 2030 | `process_settlements()` | `settlement_realized_pnl += realized_pnl` | via `post_settlement()` |

### Category D: Transfer mutations (BLOCKED)

| File | Line | Function | Field | Status |
|------|------|----------|-------|--------|
| `portfolio.rs` | 475 | `deposit()` | `cash += amount` | BLOCKED |
| `portfolio.rs` | 490 | `withdraw()` | `cash -= amount` | BLOCKED |

### Category E: Derived/MTM updates (ALLOWED)

These are read-only derivations from ledger state:
- `unrealized_pnl` computation
- `equity` calculation
- Mark-to-market updates

### Category F: Metrics/Bookkeeping (ALLOWED)

These do not affect economic state:
- `orchestrator.rs`: `total_fees` counter in results
- `oms.rs`: OMS statistics
- `metrics.rs`: Performance metrics

## Debugging

### "STRICT ACCOUNTING VIOLATION - DIRECT MUTATION BLOCKED"

This panic means you called a forbidden function in strict mode.

**Fix**: Route the operation through the ledger:
```rust
// Instead of: portfolio.apply_fill(...)
// Use:
ledger.post_fill(fill_id, market_id, outcome, side, qty, price, fee, sim_time, arrival_time, order_id)
```

### "ACCOUNTING ABORT: NegativeCash"

The operation would cause cash to go negative.

**Fix**: Either:
1. Ensure sufficient cash before the operation
2. Set `allow_negative_cash: true` in `LedgerConfig` (not recommended for production)

### Verification Commands

```bash
# Check for direct mutations in strict-mode code paths
grep -rn "guard_direct_mutation" src/backtest_v2/

# Verify all fills go through ledger
grep -rn "post_fill" src/backtest_v2/orchestrator.rs

# Check for unauthorized cash mutations
grep -rn "\.cash\s*[+\-]?=" src/backtest_v2/ | grep -v guard_direct_mutation
```

## Acceptance Criteria

For a backtest run to be valid with `strict_accounting=true`:

- [ ] Every fill posts balanced ledger entries
- [ ] Every fee is recorded via ledger
- [ ] Every settlement posts balanced ledger entries
- [ ] No direct cash/position/PnL mutations occurred
- [ ] Zero `bypass_violations` in `StrictAccountingState`
- [ ] Zero `direct_mutation_attempts` (global counter)
- [ ] Deterministic journal hash matches expected value
- [ ] Final state equals ledger-derived state

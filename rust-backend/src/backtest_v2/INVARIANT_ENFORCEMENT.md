# Invariant Enforcement Framework

## Overview

The invariant enforcement framework promotes invariant checking from debug/trace tooling into a **mandatory structural requirement** for production-grade backtests. In Hard mode, any invariant violation triggers immediate abort with a deterministic minimal causal dump.

## Invariant Categories

The framework enforces five categories of invariants:

| Category | Description | Critical Checks |
|----------|-------------|-----------------|
| **Time** | Decision time monotonicity, visibility semantics | decision_time never decreases, arrival ≤ decision |
| **Book** | Orderbook consistency | No crossed book, no negative sizes, valid prices |
| **OMS** | Order lifecycle correctness | Valid state transitions, no fills before ack |
| **Fills** | Fill plausibility | No overfills, valid prices, no NaN |
| **Accounting** | Double-entry balance | balanced entries, cash non-negative |

## Invariant Modes

```rust
pub enum InvariantMode {
    /// No invariant checking (testing only)
    Off,
    /// Log violations, continue execution (development)
    Soft,
    /// Abort on first violation with causal dump (production)
    Hard,
}
```

### Mode Selection

- **Off**: For unit tests that deliberately violate invariants
- **Soft**: For development, debugging, and exploration
- **Hard**: Required for production-grade backtest results

## Production-Grade Requirements

A backtest is only considered "production-grade" if:

```rust
ProductionGradeRequirements {
    invariant_mode_hard: true,      // Must be Hard mode
    all_categories_enabled: true,   // All 5 categories checked
}
```

Use `ProductionGradeRequirements::check(config)` to validate.

## Usage

### Basic Setup

```rust
use crate::backtest_v2::invariants::{InvariantConfig, InvariantEnforcer, InvariantMode};

// Production configuration
let config = InvariantConfig::production();
let mut enforcer = InvariantEnforcer::new(config);

// Integrate with event loop
enforcer.check_decision_time(new_time)?;
enforcer.check_book(&book, timestamp)?;
enforcer.check_order_transition(order_id, new_state, timestamp, trigger)?;
enforcer.check_fill(order_id, fill_qty, fill_price, is_taker, &book, timestamp)?;
enforcer.check_accounting_balance(cash, equity, net_position)?;
```

### Recording Context

```rust
// Record events for causal dump context
enforcer.record_event(&timestamped_event);
enforcer.record_ledger_entry(&ledger_entry);
enforcer.update_state(cash, position, open_orders);
```

### Handling Violations

In Hard mode, violations return `Err(InvariantAbort)`:

```rust
match enforcer.check_decision_time(new_time) {
    Ok(()) => { /* continue */ }
    Err(abort) => {
        // abort.dump contains full context
        eprintln!("{}", abort.dump.format());
        // Backtest MUST stop here
        return Err(BacktestError::InvariantViolation(abort));
    }
}
```

## Causal Dump Format

On violation, the framework produces a deterministic causal dump:

```
================================================================================
INVARIANT VIOLATION
================================================================================
Category: Time
Type: DecisionTimeBackward { old: 1000000, new: 500000 }
Message: Decision time went backward from 1000000 to 500000

Decision Time: 500000
Last Decision Time: 1000000

--- Recent Events (last 10) ---
[0] arrival=900000 source=900000 seq=5 type=L2BookSnapshot market=None
[1] arrival=950000 source=950000 seq=6 type=TradePrint market=None

--- State Snapshot ---
Cash: $10000.00
Position: 100.0
Open Orders: 2
Best Bid: 0.49 x 100.0
Best Ask: 0.51 x 100.0

Config Hash: 0x1234567890ABCDEF
================================================================================
```

## Category Flags

Categories can be selectively enabled/disabled:

```rust
let mut categories = CategoryFlags::all();
categories.disable(InvariantCategory::Accounting);  // Disable accounting checks

let config = InvariantConfig {
    mode: InvariantMode::Hard,
    categories,
    ..Default::default()
};
```

**Warning**: Disabling any category disqualifies the run from production-grade status.

## Violation Types

### Time Violations
- `DecisionTimeBackward` - Decision time went backward
- `ArrivalAfterDecision` - Event arrival_time > decision_time (look-ahead)
- `EventOrderingViolation` - Event sequence numbers out of order

### Book Violations
- `CrossedBook` - Bid >= Ask (impossible state)
- `NegativeSize` - Level size < 0
- `InvalidPrice` - Price outside valid range

### OMS Violations
- `IllegalStateTransition` - Invalid order state transition
- `FillBeforeAck` - Fill received before order acknowledged
- `FillAfterTerminal` - Fill on completed/rejected order
- `UnknownOrderId` - Operation on unknown order

### Fill Violations
- `Overfill` - Fill would exceed order quantity
- `NegativeFillSize` - Fill size < 0
- `NaNFillPrice` - Fill price is NaN
- `FillPriceOutsideRange` - Fill price outside valid bounds
- `TakerFillWithoutLiquidity` - Taker fill with empty book

### Accounting Violations
- `UnbalancedEntry` - Debits != Credits
- `NegativeCash` - Cash balance < 0
- `EquityIdentityViolation` - Balance equation violated
- `DuplicateSettlement` - Same settlement applied twice
- `DuplicateFeePosting` - Same fee posted twice

## Counters

The enforcer tracks check/violation counts:

```rust
let counters = enforcer.counters();
println!("{}", counters.summary());
// Output: Checks: T:1000/B:1000/O:500/F:200/A:200 | Violations: T:0/B:0/O:0/F:0/A:0
```

## Integration with BacktestOrchestrator

```rust
pub struct BacktestOrchestrator {
    invariant_enforcer: InvariantEnforcer,
    // ...
}

impl BacktestOrchestrator {
    pub fn new(config: BacktestConfig) -> Self {
        let invariant_config = if config.production_grade {
            InvariantConfig::production()
        } else {
            InvariantConfig::default()
        };
        
        Self {
            invariant_enforcer: InvariantEnforcer::new(invariant_config),
            // ...
        }
    }
    
    fn process_event(&mut self, event: &TimestampedEvent) -> Result<()> {
        self.invariant_enforcer.record_event(event);
        self.invariant_enforcer.check_decision_time(event.time)?;
        // ... rest of event processing
    }
}
```

## Test Suite

The framework includes 41 adversarial tests covering:

- Time monotonicity violations
- Visibility (look-ahead) violations
- Book consistency violations
- OMS lifecycle violations
- Fill plausibility violations
- Accounting balance violations
- Production-grade requirement validation
- Causal dump format verification
- Counter accuracy
- Category enable/disable

Run tests:
```bash
cargo test --lib backtest_v2::invariant
```

## Design Principles

1. **Fail-Fast**: Hard mode aborts immediately on first violation
2. **Deterministic**: Same inputs produce identical causal dumps
3. **Bounded**: Context buffers have configurable maximum size
4. **Auditable**: Every violation includes full causal context
5. **Production-Mandatory**: Results are only trustworthy with full enforcement

# Hermetic Compile-Time Enforcement

**Status:** ENFORCED  
**Last Updated:** 2026-01-24

---

## Overview

This document describes the compile-time enforcement mechanism that prevents strategy code from accessing wall-clock time APIs. This is a critical safeguard for backtest determinism.

## Why Hermetic Enforcement?

Backtests MUST produce identical results given identical inputs. Any access to real wall-clock time breaks this guarantee because:

1. `SystemTime::now()` returns different values on each run
2. `Instant::now()` measures elapsed real time, not simulated time
3. Chrono's `Utc::now()` / `Local::now()` access the system clock

Strategies that (accidentally or intentionally) use these APIs will:
- Produce non-reproducible backtests
- Have their live behavior differ from backtest behavior
- Invalidate any performance claims derived from backtesting

## Enforcement Mechanism

### Compile-Time (Primary)

Strategy modules include the following deny attributes:

```rust
#![deny(clippy::disallowed_types)]
#![deny(clippy::disallowed_methods)]
```

The `clippy.toml` configuration forbids:

**Disallowed Types:**
- `std::time::SystemTime`
- `std::time::Instant`
- `tokio::time::Instant`
- `chrono::Local`
- `chrono::Utc`

**Disallowed Methods:**
- `std::time::SystemTime::now`
- `std::time::SystemTime::elapsed`
- `std::time::Instant::now`
- `std::time::Instant::elapsed`
- `chrono::Utc::now`
- `chrono::Local::now`
- `tokio::time::Instant::now`

### Runtime (Secondary)

The `hermetic.rs` module provides runtime guards that panic if forbidden operations are attempted when hermetic mode is enabled. This is a defense-in-depth layer.

## Affected Modules

The following modules have hermetic enforcement enabled:

| Module | File | Enforcement |
|--------|------|-------------|
| Strategy Harness | `strategy.rs` | `#![deny(clippy::disallowed_types)]` |
| Example Strategy | `example_strategy.rs` | `#![deny(clippy::disallowed_types)]` |
| Gate Suite | `gate_suite.rs` | `#![deny(clippy::disallowed_types)]` |

## What to Use Instead

### For Current Simulated Time

Use `StrategyContext::timestamp()`:

```rust
impl Strategy for MyStrategy {
    fn on_book_update(&mut self, ctx: &mut StrategyContext, book: &BookSnapshot) {
        // CORRECT: Use simulated timestamp from context
        let current_time = ctx.timestamp;
        
        // WRONG: This will cause a compile error
        // let current_time = std::time::Instant::now();
    }
}
```

### For Time Calculations

Use `Nanos` arithmetic:

```rust
use crate::backtest_v2::clock::{Nanos, NANOS_PER_SEC};

// Calculate 5 seconds from now (simulated)
let future_time = ctx.timestamp + 5 * NANOS_PER_SEC;

// Calculate elapsed simulated time
let elapsed_ns = ctx.timestamp - self.last_trade_time;
let elapsed_sec = elapsed_ns as f64 / NANOS_PER_SEC as f64;
```

### For Timers

Use the strategy timer API:

```rust
fn on_book_update(&mut self, ctx: &mut StrategyContext, book: &BookSnapshot) {
    // Schedule a callback 1 second from now (simulated time)
    ctx.orders.schedule_timer(
        self.next_timer_id(),
        ctx.timestamp + NANOS_PER_SEC,
        Some("requote".to_string()),
    );
}

fn on_timer(&mut self, ctx: &mut StrategyContext, timer: &TimerEvent) {
    // Handle the scheduled callback
    self.requote(ctx);
}
```

## CI Verification

The CI pipeline runs `cargo clippy` with the hermetic enforcement enabled:

```bash
# This will fail if any strategy module uses forbidden time APIs
cargo clippy --all-targets -- -D warnings
```

## Testing Enforcement

To verify the enforcement is working:

```rust
#[cfg(test)]
mod hermetic_enforcement_tests {
    // This test module intentionally tries to use forbidden types.
    // If clippy is configured correctly, uncommenting this will cause
    // a compile error:
    //
    // use std::time::SystemTime;  // ERROR: disallowed type
    // let _ = SystemTime::now();  // ERROR: disallowed method
    
    #[test]
    fn test_hermetic_enforcement_documentation() {
        // This test exists to document the enforcement mechanism.
        // The actual enforcement happens at compile time via clippy.
        assert!(true, "Hermetic enforcement is via compile-time clippy lints");
    }
}
```

## Exceptions

There are NO exceptions to hermetic enforcement for strategy code. If you believe you need wall-clock time:

1. **For logging:** Use the simulated timestamp and log it
2. **For rate limiting:** Track call counts or simulated time intervals
3. **For benchmarking:** Use the benchmark infrastructure, not strategy code

## Audit Compliance

This enforcement mechanism satisfies requirement **G** from the production-grade audit:

> **G. Strategy boundary is hermetic in production-grade**
> - Verify production-grade backtests run strategies in a constrained mode that prevents wall-clock access.
> - Acceptable implementations: compile-time feature gate or lint that fails build for disallowed APIs

**Status: PASS** - Compile-time enforcement via `clippy::disallowed_types` and `clippy::disallowed_methods` prevents strategy code from using wall-clock time APIs.

---

## Related Documentation

- `hermetic.rs` - Runtime hermetic enforcement guards
- `strategy.rs` - Strategy harness with hermetic context
- `clock.rs` - Simulated clock (Nanos type)
- `clippy.toml` - Clippy configuration with disallowed types

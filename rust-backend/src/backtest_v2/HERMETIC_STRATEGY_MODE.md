# Hermetic Strategy Mode

## Overview

Hermetic Strategy Mode is a **mandatory correctness hardening layer** for production-grade backtests. When enabled (`hermetic_strategy=true`), it enforces strict sandboxing of strategy code to guarantee:

1. **Deterministic execution** - identical inputs produce identical outputs
2. **No look-ahead bias** - strategies cannot access future information
3. **Full auditability** - every decision is recorded with a cryptographic proof
4. **Reproducibility** - backtests can be exactly replayed

## Requirements

When `production_grade=true`, the following are **NON-NEGOTIABLE**:

### What Strategies MUST NOT Do

| Forbidden Operation | Why It's Forbidden |
|---------------------|-------------------|
| Access wall-clock time (`std::time::SystemTime`, `Instant`) | Non-deterministic; differs between runs |
| Access environment variables (`std::env::var`) | External state; non-reproducible |
| Perform filesystem I/O (`std::fs::*`) | Side effects; non-deterministic |
| Perform network I/O (`std::net::*`) | External state; latency varies |
| Spawn threads (`std::thread::spawn`) | Non-deterministic ordering |
| Spawn async tasks (`tokio::spawn`) | Non-deterministic scheduling |
| Use randomness outside provided RNG | Non-reproducible |
| Access global/static mutable state | Shared mutation; race conditions |

### What Strategies MUST Do

| Required Behavior | How It's Enforced |
|-------------------|------------------|
| Derive ALL time from `StrategyContext::now()` | Runtime guard panics on wall-clock access |
| Derive ALL data from visibility-gated inputs | Visibility watermark enforcement |
| Produce a `HermeticDecisionProof` for EVERY callback | Builder drop panics if not finalized |
| Be reproducible bit-for-bit | Proof hashes verified across runs |

## Enforcement Layers

### 1. Compile-Time Lint (CI Gate)

The following patterns are deny-listed for `strategy/` modules:

```toml
# clippy.toml / CI script
disallowed-methods = [
    "std::time::SystemTime",
    "std::time::Instant",
    "std::time::UNIX_EPOCH",
    "chrono::",
    "tokio::time::",
    "std::env::var",
    "std::env::vars",
    "std::env::set_var",
    "std::env::remove_var",
    "std::fs::",
    "std::net::",
    "std::thread::spawn",
    "std::thread::Builder",
    "tokio::spawn",
    "tokio::task::spawn",
    "async_std::task::spawn",
]
```

### 2. Runtime Guards (Backstop)

When `hermetic_mode` is enabled globally:

```rust
// Any call to guard_* functions panics immediately
guard_wall_clock();  // Panics: "HERMETIC MODE VIOLATION: wall_clock_time_access"
guard_env_access();  // Panics: "HERMETIC MODE VIOLATION: environment_variable_access"
guard_filesystem_io();
guard_network_io();
guard_thread_spawn();
guard_async_spawn();
```

### 3. DecisionProof Enforcement

Every strategy callback that can emit orders MUST produce a `HermeticDecisionProof`:

```rust
// In hermetic mode, the builder panics on drop if not finalized
let mut builder = HermeticDecisionProof::builder(
    decision_id,
    "MyStrategy",
    CallbackType::OnBookUpdate,
    ctx.now(),
);

builder.record_input_event(arrival_time, source_time, seq);
builder.record_book_snapshot(book_hash);
builder.record_signal("edge", edge_value);

// If placing an order
builder.record_order(client_order_id, token_id, side, price, size);

// Or if explicitly doing nothing
builder.record_noop("Position at max, no action");

// MUST call build() before the callback returns
let proof = builder.build();
```

If a strategy callback returns without calling `build()`, the Drop impl panics:

```
HERMETIC MODE VIOLATION: HermeticDecisionProofBuilder dropped without calling build().
Strategy 'MyStrategy' callback OnBookUpdate at time 1234567890 did not produce a DecisionProof.
```

## Configuration

### Production-Grade (Default)

```rust
let config = BacktestConfig::default();
// config.hermetic_config.enabled = true
// config.hermetic_config.require_decision_proofs = true
// config.hermetic_config.abort_on_violation = true
```

### Research Mode (Explicit Opt-In)

```rust
let config = BacktestConfig::research_mode();
// config.hermetic_config.enabled = false
// config.allow_non_production = true  // Required!
```

### Custom Configuration

```rust
let hermetic_config = HermeticConfig {
    enabled: true,
    require_decision_proofs: true,
    abort_on_violation: true,  // false for debugging
    max_callback_duration_ns: 1_000_000_000,  // 1 second timeout
};
```

## HermeticDecisionProof Structure

```rust
pub struct HermeticDecisionProof {
    pub decision_id: u64,
    pub strategy_name: String,
    pub callback_type: CallbackType,
    pub decision_time: Nanos,
    pub input_events: Vec<InputEventId>,
    pub book_snapshot_hash: u64,
    pub signal_values: Vec<(String, f64)>,
    pub input_hash: u64,  // Deterministic hash of all inputs
    pub actions: Vec<DecisionAction>,
    pub is_noop: bool,
}
```

The `input_hash` is computed deterministically from all inputs and is **identical across runs** with the same inputs.

## Callback Types

| Callback | Can Emit Orders | Requires Proof |
|----------|-----------------|----------------|
| `OnBookUpdate` | Yes | **Yes** |
| `OnTrade` | Yes | **Yes** |
| `OnTimer` | Yes | **Yes** |
| `OnFill` | Yes | **Yes** |
| `OnCancelAck` | Yes | **Yes** |
| `OnStart` | Yes | **Yes** |
| `OnOrderAck` | No | No |
| `OnOrderReject` | No | No |
| `OnStop` | No | No |

## Hermetic Clock and RNG

### HermeticClock

Strategies MUST use the provided hermetic clock, not system time:

```rust
// CORRECT - uses simulated time
let now = ctx.timestamp;

// WRONG - panics in hermetic mode
let now = std::time::SystemTime::now();  // PANIC!
```

### HermeticRng

Strategies MUST use the provided seeded RNG:

```rust
// CORRECT - deterministic
let value: f64 = hermetic_rng.rng().gen();

// WRONG - non-reproducible
let value: f64 = rand::random();  // Non-deterministic!
```

## Error Handling

### Missing DecisionProof (Abort Mode)

```
╔══════════════════════════════════════════════════════════════════╗
║            HERMETIC STRATEGY MODE ABORT                          ║
╠══════════════════════════════════════════════════════════════════╣
║  Strategy 'MyStrategy' callback OnBookUpdate at time 1234567890  ║
║  did not produce a DecisionProof                                 ║
║  Decision Time: 1234567890 ns                                    ║
╠══════════════════════════════════════════════════════════════════╣
║  VIOLATION TYPE: MissingDecisionProof                            ║
╚══════════════════════════════════════════════════════════════════╝
```

### Forbidden API Access

```
HERMETIC MODE VIOLATION: Forbidden operation 'wall_clock_time_access' attempted in hermetic mode.
Strategy code MUST NOT access wall-clock time, environment, I/O, or threading.
```

## Marker Trait

Strategies can optionally implement `HermeticStrategy` to declare hermetic compatibility:

```rust
pub trait HermeticStrategy: Strategy {
    fn is_hermetic(&self) -> bool { true }
}

impl HermeticStrategy for MyStrategy {}
```

## Validation at Startup

The orchestrator validates hermetic requirements at startup:

```rust
// In BacktestConfig::validate_production_grade()
if !self.hermetic_config.enabled {
    violations.push("hermetic_config.enabled must be true in production-grade mode");
}
if !self.hermetic_config.is_production_grade() {
    violations.push("hermetic_config must be production-grade");
}
```

## Known Limitations

1. **Full sandboxing not achievable in pure Rust** - We rely on runtime guards + lints rather than true process isolation. Malicious code could bypass guards.

2. **Lint enforcement requires CI integration** - The compile-time checks are not enforced by the Rust compiler directly.

3. **Global state access not fully prevented** - We cannot prevent all `static mut` access without more invasive changes.

## Why This Is Required

Production-grade backtests make **claims about live trading performance**. These claims are only valid if:

1. The backtest is **deterministic** - running it twice produces identical results
2. The backtest is **reproducible** - another party can verify the results
3. The backtest is **auditable** - every decision can be traced to its inputs
4. The backtest has **no look-ahead** - strategies only see past data

Hermetic mode enforces all four properties through a combination of:
- Configuration validation (abort on invalid config)
- Runtime guards (panic on forbidden operations)
- DecisionProof requirements (panic on missing proofs)
- Deterministic hashing (verify identical behavior)

Without these guarantees, backtest results cannot be trusted for production deployment decisions.

## Summary

| Property | Guarantee | Enforcement |
|----------|-----------|-------------|
| Determinism | Same inputs → same outputs | Seeded RNG, no wall-clock |
| Reproducibility | Results verifiable by others | Proof hashes, fingerprints |
| Auditability | Every decision traceable | DecisionProof for all callbacks |
| No Look-Ahead | Only past data visible | Visibility watermark |
| Sandboxing | No external dependencies | Runtime guards, lint rules |

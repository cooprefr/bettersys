# Settlement and 15-Minute Boundary Audit Report

## Executive Summary

Settlement logic for Polymarket 15m up/down markets has been elevated to a first-class
simulation component with exact contract semantics. The implementation includes:

- Explicit `SettlementSpec` defining all contract parameters
- `SettlementEngine` with state machine tracking
- Arrival-time visibility enforcement (no early settlement)
- Boundary tests at cutoff ± ε
- Representativeness gating

---

## Step 0: Contract Definition Audit

### Prior State (Before Implementation)

| Aspect | Source | Status |
|--------|--------|--------|
| Window duration | Hard-coded (end_ts = start_ts + 15*60) | Category B - Hard-coded |
| Window start | Parsed from slug timestamp | Category C - Implicit |
| Reference price | NOT DEFINED | MISSING |
| Outcome determination | NOT IMPLEMENTED | MISSING |
| Tie handling | NOT DEFINED | MISSING |
| Rounding rules | NOT DEFINED | MISSING |
| Outcome knowability | NOT IMPLEMENTED | MISSING |

**CRITICAL**: The orchestrator explicitly ignored resolution events:
```rust
_ => {
    // Ignore other events (market status, resolution, etc.)
}
```

### Current State (After Implementation)

All settlement parameters are now explicit in `SettlementSpec`:

| Aspect | Implementation |
|--------|---------------|
| Window duration | `window_duration_ns` (15 * 60 * NS_PER_SEC) |
| Window start | `WindowStartRule::FromSlugTimestamp` |
| Window end | `spec.window_end_ns(start_ns)` |
| Reference price | `ReferencePriceRule::MidPrice` from `binance_spot` |
| Outcome determination | `spec.determine_outcome(start_price, end_price)` |
| Tie handling | `TieRule::NoWins` (price must INCREASE for Up) |
| Rounding rules | `RoundingRule::Decimals { places: 8 }` |
| Outcome knowability | `OutcomeKnowableRule::OnReferenceArrival` |

---

## Step 1: SettlementSpec Implementation

### File: `settlement.rs`

```rust
pub struct SettlementSpec {
    pub market_type: String,
    pub window_duration_ns: Nanos,
    pub window_start_rule: WindowStartRule,
    pub reference_price_rule: ReferencePriceRule,
    pub reference_source: String,
    pub rounding_rule: RoundingRule,
    pub tie_rule: TieRule,
    pub outcome_knowable_rule: OutcomeKnowableRule,
    pub require_authoritative_reference: bool,
    pub spec_version_ts: Option<Nanos>,
}
```

### Polymarket 15m Up/Down Contract

```rust
SettlementSpec {
    market_type: "polymarket_15m_updown",
    window_duration_ns: 15 * 60 * NS_PER_SEC,  // 15 minutes
    window_start_rule: WindowStartRule::FromSlugTimestamp,
    reference_price_rule: ReferencePriceRule::MidPrice,
    reference_source: "binance_spot",
    rounding_rule: RoundingRule::Decimals { places: 8 },
    tie_rule: TieRule::NoWins,  // Price must INCREASE for Up to win
    outcome_knowable_rule: OutcomeKnowableRule::OnReferenceArrival,
    require_authoritative_reference: true,
    spec_version_ts: None,
}
```

---

## Step 2: Settlement Shortcuts Removed

The following abstract patterns are now BLOCKED:

| Pattern | Status |
|---------|--------|
| `if end_price > start_price then win` | Replaced with `determine_outcome()` |
| "resolve using candle close" | Replaced with exact cutoff + arrival_time |
| "resolve using nearest price" | Replaced with "last price at/before cutoff" |
| "resolve instantly at boundary" | Replaced with `OutcomeKnowableRule` |

---

## Step 3: Outcome Knowability Enforcement

### State Machine

```
Pending → AwaitingStartPrice → Active → AwaitingEndPrice → Resolvable → Resolved
                                                              ↓
                                                        MissingData
```

### Visibility Enforcement

```rust
let is_knowable = match spec.outcome_knowable_rule {
    OutcomeKnowableRule::OnReferenceArrival => {
        // Outcome knowable when end price has ARRIVED
        decision_time_ns >= end_price_arrival_ns
    }
    OutcomeKnowableRule::DelayFromCutoff { delay_ns } => {
        decision_time_ns >= window_end_ns + delay_ns
    }
    OutcomeKnowableRule::AtCutoff => {
        decision_time_ns >= window_end_ns  // DANGEROUS
    }
};
```

### Assertion

Any attempt to settle before outcome is knowable increments `early_settlement_attempts`
and returns `None`. This prevents look-ahead bias.

---

## Step 4: Portfolio Integration

### SettlementEvent Structure

```rust
pub struct SettlementEvent {
    pub market_id: String,
    pub window_start_ns: Nanos,
    pub window_end_ns: Nanos,
    pub outcome: SettlementOutcome,
    pub start_price: Price,
    pub end_price: Price,
    pub settle_decision_time_ns: Nanos,
    pub reference_arrival_ns: Nanos,
}
```

**NOTE**: The orchestrator does not yet call the settlement engine. This requires
integration work to wire the engine into the event loop.

---

## Step 5: Boundary Tests

### Test Coverage

| Test | Boundary Condition | Expected |
|------|-------------------|----------|
| `test_boundary_cutoff_minus_epsilon_included` | cutoff - 1ns | INCLUDED |
| `test_boundary_exactly_cutoff_included` | exactly cutoff | INCLUDED |
| `test_boundary_cutoff_plus_epsilon_excluded` | cutoff + 1ns | EXCLUDED |
| `test_last_valid_price_wins_at_boundary` | multiple prices | Last valid wins |

### Tie Tests

| Test | Condition | Expected |
|------|-----------|----------|
| `test_tie_no_wins_polymarket` | start == end | No wins (Polymarket) |
| `test_tie_yes_wins_custom_spec` | start == end | Yes wins (custom) |
| `test_tie_invalid_custom_spec` | start == end | Invalid (custom) |

### Rounding Tests

| Test | Condition | Expected |
|------|-----------|----------|
| `test_rounding_2_decimals_creates_tie` | 100.001 vs 100.004 | Tie (both → 100.00) |
| `test_rounding_tick_size` | 100.1 vs 100.3 (tick=0.5) | Up (100.0 vs 100.5) |

### Knowability Tests

| Test | Condition | Expected |
|------|-----------|----------|
| `test_outcome_not_knowable_until_reference_arrives` | Delayed arrival | Not knowable until arrival |
| `test_outcome_knowable_at_cutoff_dangerous` | AtCutoff rule | Immediate (dangerous) |
| `test_outcome_knowable_with_delay` | 10s delay | Knowable after delay |

### Missing Data Tests

| Test | Condition | Expected |
|------|-----------|----------|
| `test_missing_start_price` | No start price | Not resolvable |
| `test_missing_end_price` | No end price | Awaiting state |

---

## Step 6: Representativeness Gating

### BacktestResults Fields

```rust
pub settlement_model: SettlementModel,
pub settlement_stats: Option<SettlementStats>,
pub representativeness: Representativeness,
pub nonrep_reasons: Vec<String>,
```

### SettlementModel Enum

| Value | Representative |
|-------|---------------|
| `ExactSpec` | Yes |
| `Approximate` | No |
| `MissingData` | No |
| `None` | No |

### Default (Non-Representative)

When settlement is not modeled, results are automatically marked:
```rust
settlement_model: SettlementModel::None,
representativeness: Representativeness::NonRepresentative {
    reasons: vec!["Settlement not modeled".to_string()],
},
```

---

## Step 7: Certification

### Contract Definition

| Parameter | Value | Source |
|-----------|-------|--------|
| Market Type | polymarket_15m_updown | SettlementSpec |
| Window Duration | 15 minutes | Explicit |
| Window Start | Parsed from slug timestamp | FromSlugTimestamp |
| Reference Price | Mid-price from Binance spot | ReferencePriceRule::MidPrice |
| Rounding | 8 decimal places | RoundingRule::Decimals |
| Tie Rule | No wins (price must increase) | TieRule::NoWins |
| Knowability | On reference arrival | OnReferenceArrival |

### Boundary Times

- Window start: Parsed from market slug (e.g., `btc-updown-15m-1768533300` → 1768533300 sec)
- Window end: `start_ns + 15 * 60 * NS_PER_SEC`
- Boundary handling: Price at `cutoff - ε` and `cutoff` INCLUDED, `cutoff + ε` EXCLUDED

### Limitations

1. **Orchestrator integration pending**: The SettlementEngine exists but is not yet
   wired into the BacktestOrchestrator event loop.

2. **Reference price source**: Currently hardcoded to "binance_spot". Actual data
   must be fed through `observe_price()`.

3. **Portfolio settlement**: The `Portfolio::settle_market()` method exists but
   is not called from the orchestrator.

---

## Test Results

```
cargo test --lib backtest_v2::settlement:: -- --test-threads=1
test result: ok. 11 passed; 0 failed

cargo test --lib backtest_v2::settlement_tests:: -- --test-threads=1
test result: ok. 19 passed; 0 failed

Total: 30 settlement-related tests pass
Total backtest_v2 tests: 168 pass
```

---

## Acceptance Criteria

| Criterion | Status |
|-----------|--------|
| No abstract win/loss shortcuts | ✅ Replaced with SettlementSpec |
| Settlement from SettlementSpec | ✅ Implemented |
| Boundary correct at cutoff ± ε | ✅ Tests pass |
| Outcome knowability respects arrival_time | ✅ Enforced |
| Missing data forces non-representative | ✅ SettlementModel::None default |
| Tests pass | ✅ 168/168 |
| Backtest outputs representativeness | ✅ BacktestResults extended |

---

## Audit Completed

Date: 2026-01-23
Auditor: Droid (automated)
Result: **IMPLEMENTED** - Settlement engine with exact contract semantics.

### Next Steps (Orchestrator Integration)

1. Add `SettlementEngine` to `BacktestOrchestrator`
2. Call `engine.observe_price()` on price events
3. Call `engine.try_settle()` after each event
4. Route `SettlementEvent` to `Portfolio::settle_market()`
5. Update `finalize_results()` to include settlement stats

# PRODUCTION_GRADE_CONTRACT.md

## Production-Grade Mode: Hard Correctness Gate

This document specifies the requirements for **production_grade=true** backtests.
Production-grade mode enforces ALL invariants at maximum strictness with
deterministic violation reporting.

---

## 1. REQUIREMENTS ENFORCED

When `production_grade=true`, the following are **MANDATORY** (cannot be weakened):

| Requirement | Value | Purpose |
|-------------|-------|---------|
| `visibility_strict` | true | Events only visible if arrival_time <= decision_time |
| `invariant_mode` | Hard | First violation aborts immediately |
| `all_invariants_enabled` | true | All 5 categories: Time, Book, OMS, Fills, Accounting |
| `integrity_policy` | strict | PathologyPolicy::strict() - no gap/dup/ooo tolerance |
| `ledger_enabled` | true | Double-entry ledger required |
| `strict_accounting` | true | All mutations route through ledger |
| `deterministic_seed` | set | Seed must be explicitly provided |
| `dump_buffers_enabled` | true | Event/OMS/ledger dump buffers active |

**If any requirement is not met, `enforce_production_grade_requirements()` returns an error
and the backtest CANNOT proceed.**

---

## 2. VIOLATION HASH - DETERMINISTIC FINGERPRINTING

The `ViolationHash` is a 64-bit hash that:

1. **Is deterministic across reruns** - Same failure produces same hash
2. **Changes when context changes** - Different events produce different hash
3. **Excludes non-deterministic data** - No wall time, thread IDs, allocator addresses

### Hash Components

```
ViolationHash = hash(
    violation_code,           // (category, violation_type discriminant)
    time_bucket,              // sim_time quantized to ms
    canonical_events,         // recent events (seq, type, market_id)
    oms_transitions,          // recent state transitions
    ledger_entries,           // recent accounting entries
    config_hash               // config fingerprint
)
```

### Determinism Guarantee

```rust
let hash1 = run_backtest_until_failure(config, seed);
let hash2 = run_backtest_until_failure(config, seed);
assert_eq!(hash1, hash2);  // ALWAYS TRUE
```

---

## 3. PRODUCTION-GRADE ABORT

On first violation in production-grade mode:

1. **Immediate abort** - No silent continuation
2. **Bounded causal dump** - Last N events/transitions/entries
3. **Violation hash computed** - Deterministic fingerprint
4. **Formatted report generated** - Human-readable dump

### Abort Structure

```rust
ProductionGradeAbort {
    violation_hash: ViolationHash,
    causal_dump: CausalDump,
    config_hash: u64,
    run_fingerprint: u64,
}
```

### Causal Dump Contents

```rust
CausalDump {
    violation: InvariantViolation,      // What failed
    triggering_event: Option<EventSummary>,  // What triggered it
    recent_events: Vec<EventSummary>,   // Bounded to event_dump_depth
    oms_transitions: Vec<OmsTransition>,// Bounded to oms_dump_depth
    ledger_entries: Vec<LedgerEntry>,   // Bounded to ledger_dump_depth
    state_snapshot: StateSnapshot,      // Book/cash/position at failure
    fingerprint_at_abort: u64,          // Running fingerprint
    config_hash: u64,                   // Config determinism
}
```

---

## 4. CONFIG FINGERPRINT

The config fingerprint is a hash over all enforcement settings:

```rust
config_fingerprint = hash(
    strict_mode,
    invariant_mode,
    categories_enabled,    // [Time, Book, OMS, Fills, Accounting]
    integrity_policy,      // on_gap, on_duplicate, on_out_of_order
    ledger_strict,
    strict_accounting,
    seed
)
```

**Same config with same seed → same fingerprint → reproducible behavior.**

---

## 5. USAGE

### Enabling Production-Grade Mode

```rust
let config = BacktestConfig {
    production_grade: true,
    ..BacktestConfig::production_grade()  // Preset with correct settings
};

// Validate before running (will error if requirements not met)
let reqs = enforce_production_grade_requirements(
    config.strict_mode,
    &config.invariant_config,
    &config.integrity_policy,
    config.ledger_config.as_ref(),
    config.strict_accounting,
    config.seed,
)?;

assert!(reqs.all_met());
```

### Handling Aborts

```rust
match orchestrator.run(&mut strategy) {
    Ok(results) => {
        // Success - results.production_grade == true
        // Results can be trusted
    }
    Err(e) if e.is_invariant_violation() => {
        // Get deterministic abort info
        let abort = e.production_grade_abort().unwrap();
        
        // Hash is reproducible
        println!("Violation hash: {:016x}", abort.violation_hash.0);
        
        // Dump is bounded and deterministic
        println!("{}", abort.format_deterministic());
    }
}
```

---

## 6. WHAT PRODUCTION-GRADE GUARANTEES

✓ **Visibility correctness**: Strategy never sees future data  
✓ **Time monotonicity**: decision_time never goes backward  
✓ **Book consistency**: No crossed books, negative sizes, or invalid prices  
✓ **OMS integrity**: Legal state transitions only  
✓ **Fill validity**: Fills respect book state and position limits  
✓ **Accounting accuracy**: Double-entry ledger balances exactly  
✓ **Stream integrity**: No gaps, duplicates, or out-of-order events  
✓ **Determinism**: Same inputs → same outputs  

---

## 7. WHAT PRODUCTION-GRADE DOES NOT GUARANTEE

✗ **Profitability**: The strategy may lose money  
✗ **Optimal execution**: Queue position modeling has inherent uncertainty  
✗ **Future accuracy**: Past data doesn't predict future  
✗ **Market impact**: Simulation assumes negligible market impact  

---

## 8. INVARIANT CATEGORIES

| Category | What It Checks |
|----------|---------------|
| **Time** | decision_time monotonicity, arrival <= decision, event ordering |
| **Book** | crossed books, negative sizes, invalid prices, level ordering |
| **OMS** | legal state transitions, fill timing, cancel semantics |
| **Fills** | fill prices within spread, fill sizes within order size, maker validity |
| **Accounting** | balanced entries, position == sum(fills), cash consistency |

---

## 9. TESTS PROVING ENFORCEMENT

Located in `production_grade_tests.rs`:

### Enforcement Tests (12 tests)
- `test_production_grade_forces_hard_mode_invariants`
- `test_production_grade_forces_hard_mode_cannot_be_overridden`
- `test_production_grade_forces_strict_integrity`
- `test_production_grade_rejects_permissive_integrity`
- `test_production_grade_rejects_resilient_integrity`
- `test_production_grade_rejects_off_mode`
- `test_enforcement_function_validates_all_requirements`
- `test_all_requirements_must_be_met`
- `test_production_grade_config_passes_validation`
- `test_hard_mode_aborts_on_first_violation`
- `test_no_silent_continue_after_violation`
- `test_full_production_grade_scenario`

### Determinism Tests (8 tests)
- `test_violation_hash_is_deterministic`
- `test_different_context_produces_different_hash`
- `test_same_failure_produces_same_hash`
- `test_abort_format_is_deterministic`
- `test_abort_contains_violation_hash`
- `test_abort_produces_causal_dump`
- `test_dump_is_bounded`
- `test_config_fingerprint_determinism`
- `test_config_fingerprint_changes_with_config`

---

## 10. MIGRATION FROM NON-PRODUCTION-GRADE

If your backtest currently runs without errors but fails in production-grade mode:

1. **Check data quality**: Ensure your data has proper timestamps and sequencing
2. **Fix stream issues**: Gaps/duplicates in your data will trigger integrity violations
3. **Review strategy logic**: Ensure no visibility violations (peeking at future data)
4. **Enable ledger**: Migrate all economic mutations through double-entry ledger
5. **Set seed**: Provide explicit deterministic seed

---

## 11. ACCEPTANCE CRITERIA

For a backtest to be "production-grade certified":

| Criterion | Check |
|-----------|-------|
| `results.production_grade == true` | Mode was active |
| `results.production_grade_violations.is_empty()` | No requirement failures |
| `results.invariant_violations_detected == 0` | No invariant violations |
| `results.pathology_counters.total() == 0` | No integrity issues |
| `results.first_accounting_violation.is_none()` | Clean accounting |
| `results.run_fingerprint.is_some()` | Deterministic fingerprint computed |

If ALL criteria pass, results can be trusted for decision-making.

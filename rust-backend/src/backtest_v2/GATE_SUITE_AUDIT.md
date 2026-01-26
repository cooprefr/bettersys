# Gate Suite Audit Document

## 1. Purpose

This document certifies that the backtest_v2 adversarial gate suite correctly validates backtester integrity by testing for biases, look-ahead leakage, and incorrect fee handling under controlled zero-edge scenarios.

**Core Principle:** A correct backtester should produce ~0 PnL before fees and <0 PnL after fees when no informational edge exists.

## 2. Gate Tests Implemented

### Gate A: Zero-Edge Matching

**Objective:** Verify the backtester produces no systematic profit when `p_theory == p_mkt`.

**Mechanism:**
1. Generate martingale price paths (zero drift)
2. Execute random trades uniformly distributed between buys and sells
3. Measure PnL distribution across multiple seeds

**Pass Criteria:**
- `mean_pnl_before_fees < $0.50` (no systematic profit)
- `mean_pnl_after_fees < $-0.10` if trades occur (fees must reduce returns)
- `P(PnL > 0) < 55%` (consistent with random outcomes)

**What It Detects:**
- Look-ahead bias (future prices visible)
- Optimistic fill assumptions (fills at better prices than available)
- Systematic execution bias (one side favored)

### Gate B: Martingale Price Path

**Objective:** Verify the backtester doesn't inject drift into price series.

**Mechanism:**
1. Generate 100 independent martingale price paths
2. Run identical strategy on each path
3. Measure equity curve drift across seeds

**Pass Criteria:**
- `|mean_equity_drift| < 5%` of initial capital
- `P(PnL > 0) < 55%`

**What It Detects:**
- Price drift injection
- Non-stationarity in fill prices
- Bid-ask asymmetry

### Gate C: Signal Inversion Symmetry

**Objective:** Verify inverted signals don't both produce profits.

**Mechanism:**
1. Run random strategy in original direction
2. Run same strategy with inverted buy/sell signals
3. Compare PnL distributions

**Pass Criteria:**
- At most one direction can be profitable
- Combined PnL should approximate `-2 × fees`

**What It Detects:**
- Execution bias (fills favoring one side)
- Timestamp asymmetry
- Queue position bias

## 3. Synthetic Price Generator

The gate suite uses a deterministic martingale price generator:

```rust
pub struct SyntheticPriceGenerator {
    current_price: f64,
    volatility: f64,
    rng_state: u64,  // LCG PRNG for determinism
}
```

**Properties:**
- Zero drift (E[Pt+1] = Pt)
- Constant volatility per step
- Deterministic given seed
- Bounded to (0.01, 0.99) probability range

**RNG:** Linear Congruential Generator (LCG) with constants from MMIX:
- `state = state * 6364136223846793005 + 1442695040888963407`
- Yields uniform [0, 1) via bit extraction

## 4. Tolerances

Default tolerances are intentionally strict:

| Tolerance | Value | Rationale |
|-----------|-------|-----------|
| `max_mean_pnl_before_fees` | $0.50 | Tick discretization noise |
| `min_mean_pnl_after_fees` | -$0.10 | Any trading must cost |
| `max_positive_pnl_probability` | 55% | Variance around 50% |
| `min_trades_for_validity` | 10 | Statistical significance |
| `martingale_seeds` | 100 | Sufficient samples |
| `max_martingale_drift_pct` | 5% | Small expected deviation |

## 5. Integration with Backtest Framework

### BacktestConfig Extension

```rust
pub struct BacktestConfig {
    // ... existing fields ...
    pub gate_mode: GateMode,
}

pub enum GateMode {
    Disabled,    // Gates not run; trust_level = Bypassed
    Permissive,  // Run gates but allow failures
    Strict,      // Abort if gates fail
}
```

### BacktestResults Extension

```rust
pub struct BacktestResults {
    // ... existing fields ...
    pub gate_suite_passed: bool,
    pub gate_failures: Vec<(String, String)>,
    pub trust_level: TrustLevel,
}

pub enum TrustLevel {
    Trusted,    // All gates passed
    Untrusted,  // Some gates failed
    Unknown,    // Gates not run
    Bypassed,   // Gates explicitly skipped
}
```

## 6. Test Coverage

### Unit Tests (6)

| Test | Description |
|------|-------------|
| `test_synthetic_price_generator_martingale` | Verifies zero drift |
| `test_gate_suite_do_nothing_passes` | Baseline validation |
| `test_gate_tolerances_are_strict` | Tolerance bounds |
| `test_gate_suite_deterministic` | Reproducibility |
| `test_gate_report_format` | Output formatting |
| `test_trust_level_enum` | Enum semantics |

### Integration Tests (18)

| Test | Description |
|------|-------------|
| `test_gate_suite_fully_deterministic` | Multi-run reproducibility |
| `test_different_seeds_produce_different_results` | Seed sensitivity |
| `test_gate_a_zero_edge_no_systematic_profit` | Gate A correctness |
| `test_gate_a_fee_sign_correct` | Fee deduction |
| `test_gate_b_martingale_no_drift` | Gate B correctness |
| `test_martingale_price_generator_statistical_properties` | Generator properties |
| `test_gate_c_inversion_symmetry` | Gate C correctness |
| `test_inversion_both_profitable_fails` | Bias detection |
| `test_trust_level_trusted_requires_all_gates_pass` | Trust logic |
| `test_trust_level_default_is_unknown` | Default state |
| `test_gate_mode_default` | Mode default |
| `test_tolerance_values_are_conservative` | Tolerance bounds |
| `test_metrics_aggregation_across_seeds` | Statistic computation |
| `test_fill_counts_tracked` | Volume tracking |
| `test_report_format_includes_all_gates` | Report completeness |
| `test_report_shows_failure_reasons` | Failure reporting |
| `test_zero_trades_handled` | Edge case |
| `test_single_seed_runs` | Minimal config |

## 7. Usage Examples

### Running Gates Before Trusting Results

```rust
use backtest_v2::{GateSuite, GateSuiteConfig, TrustLevel};

fn validate_backtest_results(results: &BacktestResults) -> bool {
    // Run gate suite
    let suite = GateSuite::new(GateSuiteConfig::default());
    let report = suite.run();
    
    if !report.passed {
        eprintln!("GATE SUITE FAILED:");
        eprintln!("{}", report.format_summary());
        return false;
    }
    
    // Only trust positive results if gates pass
    if results.total_pnl > 0.0 && report.trust_level != TrustLevel::Trusted {
        eprintln!("Positive PnL but trust_level != Trusted");
        return false;
    }
    
    true
}
```

### Custom Tolerances for Specific Strategies

```rust
let tolerances = GateTolerances {
    max_mean_pnl_before_fees: 1.0,  // Slightly looser for noisy data
    martingale_seeds: 200,           // More seeds for high variance
    ..Default::default()
};

let config = GateSuiteConfig {
    tolerances,
    base_seed: 12345,  // Reproducible
    ..Default::default()
};

let report = GateSuite::new(config).run();
```

## 8. Certification

| Criterion | Status | Evidence |
|-----------|--------|----------|
| Zero-edge produces ~0 PnL | PASS | Gate A test suite |
| Martingale has no drift | PASS | Gate B test suite |
| Signal inversion symmetric | PASS | Gate C test suite |
| Fees always subtracted | PASS | `test_gate_a_fee_sign_correct` |
| Deterministic execution | PASS | `test_gate_suite_fully_deterministic` |
| Conservative tolerances | PASS | `test_tolerance_values_are_conservative` |

**All 24 gate tests pass.**

---

*Audit Date: 2026-01-23*  
*Module: `backtest_v2/gate_suite.rs`*  
*Test Module: `backtest_v2/gate_suite_tests.rs`*  
*Total Lines: ~1100*

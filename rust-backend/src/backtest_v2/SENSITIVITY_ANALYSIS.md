# Sensitivity Analysis Framework

## Overview

This document describes the sensitivity analysis framework for quantifying how backtest results degrade under varying assumptions. The framework ensures that positive results are not artifacts of optimistic configurations.

## Core Principles

1. **No Hidden Optimism**: All assumptions are explicit and parameterized
2. **Sweep-Based Validation**: Results must be stable across reasonable ranges
3. **Fragility Detection**: Automatic flagging of assumption-sensitive strategies
4. **Trust Gating**: Positive results without sensitivity analysis are untrusted

## Latency Components

The framework identifies and parameterizes all latency sources:

| Component | Description | Default Sweep (ms) |
|-----------|-------------|-------------------|
| `EndToEnd` | All components scaled together | 0, 25, 50, 100, 250, 500, 1000 |
| `MarketData` | Exchange → strategy data feed | Component-specific |
| `Decision` | Signal computation time | Component-specific |
| `OrderSend` | Strategy → gateway | Component-specific |
| `VenueProcess` | Gateway → exchange matching | Component-specific |
| `CancelProcess` | Cancel message processing | Component-specific |
| `FillReport` | Exchange → strategy fill notification | Component-specific |

### Latency Sweep Configurations

```rust
// Standard Polymarket sweep
LatencySweepConfig::polymarket_standard()  // 0, 10, 25, 50, 100, 250, 500ms

// HFT fine-grained sweep
LatencySweepConfig::hft_fine()  // 0, 1, 2, 5, 10, 20, 50, 100ms

// Slower strategy sweep
LatencySweepConfig::slow_strategy()  // 0, 100, 500, 1000, 2000, 5000ms
```

## Sampling Frequency Sensitivity

Tests how results degrade as market data fidelity decreases:

| Regime | Description |
|--------|-------------|
| `EveryUpdate` | Tick-by-tick (baseline) |
| `Periodic { interval_ms }` | Snapshots at fixed intervals |
| `TopOfBookOnly` | L1 data only, no depth |
| `SnapshotOnly { interval_ms }` | No delta updates |

## Execution Assumption Sensitivity

### Queue Model Assumptions

| Model | Description | Production Valid |
|-------|-------------|------------------|
| `ExplicitFifo` | Explicit FIFO queue tracking | Yes |
| `Optimistic` | Fill on price touch | No |
| `Conservative { extra_ahead_pct }` | Assume extra queue ahead | Yes |
| `PessimisticLevelClear` | Level must clear for fill | Yes |
| `MakerDisabled` | Taker-only trading | Yes |

### Cancel Latency Assumptions

| Assumption | Description |
|------------|-------------|
| `Zero` | Instant cancels (optimistic) |
| `Fixed { latency_ms }` | Fixed cancel delay |
| `SameAsOrderSend` | Cancel latency = order send latency |

## Fragility Detection

Automatic detection of assumption-sensitive strategies using configurable thresholds:

```rust
FragilityThresholds {
    latency_pnl_drop_pct: 50.0,      // 50% PnL drop = fragile
    latency_increase_ms: 50.0,       // at 50ms latency increase
    sampling_fill_rate_drop_pct: 30.0, // 30% fill rate drop = fragile
    execution_pnl_drop_pct: 75.0,    // 75% PnL drop = fragile
}
```

### Fragility Flags

- `latency_fragile`: PnL drops significantly with modest latency increase
- `sampling_fragile`: Fill rate collapses under degraded sampling
- `execution_fragile`: PnL disappears under conservative queue model
- `requires_optimistic_assumptions`: Profitability depends on optimistic mode

## Trust Recommendations

| Recommendation | Meaning |
|----------------|---------|
| `Trusted` | Results stable across variations |
| `CautionFragile` | Results sensitive to assumptions |
| `UntrustedOptimistic` | Profitability requires optimistic assumptions |
| `UntrustedNoSensitivity` | Sensitivity analysis not run |
| `Invalid` | Configuration errors |

## Configuration

### BacktestConfig Integration

```rust
BacktestConfig {
    // ... other fields ...
    sensitivity: SensitivityConfig {
        enabled: true,
        latency_sweep: LatencySweepConfig::polymarket_standard(),
        sampling_sweep: SamplingSweepConfig::standard(),
        execution_sweep: ExecutionSweepConfig::default(),
        fragility_thresholds: FragilityThresholds::default(),
        strict_sensitivity: true,  // Abort if sensitivity not run
    },
}
```

### Preset Configurations

```rust
// Production validation (recommended)
SensitivityConfig::production_validation()

// Quick development checks
SensitivityConfig::quick()
```

## BacktestResults Integration

Results include:

```rust
BacktestResults {
    // ... other fields ...
    sensitivity_report: SensitivityReport {
        sensitivity_run: true,
        latency_sweep: Some(LatencySweepResults { ... }),
        sampling_sweep: Some(SamplingSweepResults { ... }),
        execution_sweep: Some(ExecutionSweepResults { ... }),
        fragility: FragilityFlags { ... },
        trust_recommendation: TrustRecommendation::Trusted,
    },
}
```

## Report Format

The `SensitivityReport::format_text()` method produces:

```
=== SENSITIVITY ANALYSIS REPORT ===

Sensitivity Run: YES
Trust Recommendation: Trusted
  Results stable across assumption variations - trusted

--- FRAGILITY FLAGS ---
Latency Fragile: NO 
Sampling Fragile: NO 
Execution Fragile: NO 
Requires Optimistic Assumptions: NO
Fragility Score: 0.00

--- LATENCY SWEEP ---
Component: EndToEnd
Latency(ms) | PnL After Fees | Fills | Fill Rate | Sharpe
-----------------------------------------------------------------
       0.0 |        1000.00 |   100 |      80.0% |   2.50
      50.0 |         950.00 |    95 |      78.0% |   2.40
     100.0 |         900.00 |    90 |      75.0% |   2.30

--- EXECUTION ASSUMPTION SWEEP ---
Queue Model                      | PnL After Fees | Maker Fills | Taker Fills
-------------------------------------------------------------------------------
Explicit FIFO queue              |        1000.00 |          60 |          40
Conservative (+25% queue ahead)  |         900.00 |          50 |          50
Maker fills disabled             |         700.00 |           0 |         100

===================================
```

## Trust Gating Rules

For production relevance, backtests MUST include:

1. At least one latency sweep (varying end-to-end or component latencies)
2. At least one sampling degradation run (reduced data fidelity)
3. At least one conservative execution mode run (non-optimistic queue model)

If `strict_sensitivity: true` is set and sensitivity is not run:
- Results are marked `UntrustedNoSensitivity`
- Trust claims are blocked

## Best Practices

1. **Always run sensitivity for positive results**: Profitable backtests must demonstrate stability
2. **Use conservative baselines**: Start with `ExplicitFifo`, not `Optimistic`
3. **Check maker dependency**: If PnL collapses under `MakerDisabled`, strategy is queue-dependent
4. **Monitor fragility scores**: Higher scores indicate more assumption-sensitive strategies
5. **Document sensitivity**: Include sweep results in strategy reports

## Module Location

- Core module: `backtest_v2/sensitivity.rs`
- Tests: `backtest_v2/sensitivity_tests.rs`
- Total tests: 46

## Integration Checklist

- [x] Latency components explicitly parameterized
- [x] Latency sweep configurations defined
- [x] Sampling sweep configurations defined
- [x] Execution sweep configurations defined
- [x] Fragility detection implemented
- [x] Trust recommendation logic implemented
- [x] BacktestConfig.sensitivity field added
- [x] BacktestResults.sensitivity_report field added
- [x] Text report formatting implemented
- [x] 46 tests passing

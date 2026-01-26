# Implementation Plan: RN-JD Kernel for 15m Up/Down Markets

## Executive Summary

Apply the paper's Risk-Neutral Jump-Diffusion (RN-JD) framework to improve the FAST15M engine's pricing model for BTC/ETH/SOL/XRP 15-minute Up/Down markets on Polymarket.

---

## Current State (FAST15M Engine)

From `AGENTS.md` and `updown15m.rs`:
- Uses **driftless lognormal** model for `p_up` estimation
- Conservative shrink-to-half (`UPDOWN15M_SHRINK=0.35`)
- Fetches Binance mid-price via `barter-data`
- Fractional Kelly sizing (`UPDOWN15M_KELLY_FRACTION=0.05`)
- Fixed minimum edge threshold (`UPDOWN15M_MIN_EDGE=0.01`)

---

## What the Paper Adds

### 1. Theoretically Grounded Drift
**Current:** Assumes `p_up ≈ 0.5` with ad-hoc adjustments  
**Paper:** Drift is *pinned* by the martingale constraint:
```rust
// Pure diffusion case
let mu = -0.5 * sigma_b.powi(2) * (1.0 - 2.0 * p);
```
This isn't a modeling choice—it's an arbitrage constraint.

### 2. Belief Volatility (`σ_b`) as Primary Parameter
**Current:** Uses realized price volatility from Binance  
**Paper:** Use **log-odds volatility** of the prediction market itself:
```rust
let x = (p / (1.0 - p)).ln();  // logit transform
let sigma_b = estimate_realized_vol_logodds(&historical_x);
```

### 3. Jump Detection for News
15m markets are sensitive to sudden moves. The paper's jump-diffusion component:
```rust
// Detect if recent move was a jump vs diffusion
let is_jump = abs_log_odds_move > 3.0 * sigma_b * dt.sqrt();
```

---

## Implementation Phases

### Phase 1: Enhanced `p_up` Estimation (Low Risk)

**File:** `rust-backend/src/vault/updown15m.rs`

```rust
/// RN-JD adjusted probability estimate
pub fn estimate_p_up_rnjd(
    current_price: f64,
    historical_prices: &[f64],  // Binance 1m candles
    market_p: f64,              // Current Polymarket mid
    time_to_expiry_mins: f64,
) -> f64 {
    // 1. Estimate realized vol in log-price space
    let sigma_price = realized_vol_log_returns(historical_prices);
    
    // 2. Convert to belief volatility via chain rule
    // σ_b ≈ σ_price * |dp/dS| / p(1-p) for price-linked events
    let p = market_p.clamp(0.01, 0.99);
    let sigma_b = sigma_price / (p * (1.0 - p));
    
    // 3. Apply RN drift correction
    // For short horizons, the correction is small but systematic
    let dt = time_to_expiry_mins / (252.0 * 24.0 * 60.0);  // annualized
    let drift_correction = -0.5 * sigma_b.powi(2) * (1.0 - 2.0 * p) * dt;
    
    // 4. Estimate raw p_up from price distribution
    let log_return = (current_price / historical_prices.last().unwrap()).ln();
    let raw_p_up = normal_cdf(log_return / (sigma_price * dt.sqrt()));
    
    // 5. Apply martingale-consistent adjustment
    let adjusted_p_up = (raw_p_up + drift_correction).clamp(0.01, 0.99);
    
    adjusted_p_up
}
```

**Risk:** Low. This is a refinement of existing logic.  
**Effort:** 1-2 days.

---

### Phase 2: Belief Volatility Surface (Medium Risk)

**New file:** `rust-backend/src/vault/belief_vol.rs`

Track and store `σ_b` estimates per market slug:

```rust
pub struct BeliefVolSurface {
    /// slug -> (timestamp, sigma_b, sample_count)
    cache: HashMap<String, (i64, f64, usize)>,
    /// Minimum samples before trusting estimate
    min_samples: usize,
}

impl BeliefVolSurface {
    pub fn update(&mut self, slug: &str, p_t: f64, dt_secs: f64) {
        // Track log-odds increments
        let x_t = logit(p_t);
        // Exponential moving average of squared increments
        // ...
    }
    
    pub fn get_sigma_b(&self, slug: &str) -> Option<f64> {
        self.cache.get(slug).map(|(_, sigma, _)| *sigma)
    }
}
```

**Risk:** Medium. Requires historical data accumulation.  
**Effort:** 3-5 days.

---

### Phase 3: Jump Detection (Medium Risk)

Detect when market moves are jumps vs diffusion:

```rust
/// Returns (is_jump, estimated_jump_size)
pub fn detect_jump(
    delta_x: f64,        // log-odds change
    dt: f64,             // time delta (annualized)
    sigma_b: f64,        // belief volatility
    threshold: f64,      // typically 3.0
) -> (bool, f64) {
    let diffusion_std = sigma_b * dt.sqrt();
    let z_score = delta_x.abs() / diffusion_std;
    
    if z_score > threshold {
        (true, delta_x)  // Jump detected
    } else {
        (false, 0.0)
    }
}
```

**Application:** 
- Widen required edge after detected jumps (news absorption period)
- Skip trading during high jump-intensity periods

**Risk:** Medium. False positives could cause missed opportunities.  
**Effort:** 2-3 days.

---

### Phase 4: Adaptive Kelly with Belief Vol (Low-Medium Risk)

Current Kelly uses fixed `confidence` input. Enhance with `σ_b`:

```rust
pub fn kelly_with_belief_vol(
    our_p: f64,          // Our probability estimate
    market_p: f64,       // Market price
    sigma_b: f64,        // Belief volatility
    time_to_expiry: f64, // In years
) -> f64 {
    let edge = our_p - market_p;
    
    // Adjust for expected vol during holding period
    // Higher σ_b → more uncertainty → smaller position
    let vol_penalty = sigma_b * time_to_expiry.sqrt();
    let adjusted_edge = (edge - vol_penalty).max(0.0);
    
    // Standard Kelly
    let odds = (1.0 / market_p) - 1.0;
    let kelly = (our_p * odds - (1.0 - our_p)) / odds;
    
    // Scale by edge confidence
    kelly * (adjusted_edge / edge.abs().max(0.001))
}
```

**Risk:** Low. Strictly more conservative than current approach.  
**Effort:** 1 day.

---

## What NOT to Implement (Yet)

### Derivative Layer
The paper's variance swaps, correlation swaps, etc. require:
- Active counterparties (market doesn't exist yet)
- OTC infrastructure
- Regulatory clarity

**Recommendation:** Document as future opportunity but don't build.

### Multi-Event Correlation
Co-jump detection and correlation surfaces require:
- Simultaneous tracking of multiple markets
- Significant historical data
- Complex EM calibration

**Recommendation:** Phase 5+, after core RN-JD is validated.

---

## Risks & Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| σ_b estimation noise (short history) | High | Medium | Use priors, blend with price vol |
| False jump detection | Medium | Low | Conservative threshold (3σ+) |
| Model misspecification | Medium | Medium | A/B test against current engine |
| Overfitting to paper's synthetic tests | Medium | Medium | Validate on live 15m data first |
| Latency from extra computation | Low | Low | Pre-compute, cache aggressively |

---

## Validation Plan

### Backtest Framework
1. Collect 30 days of 15m Up/Down market data (prices + outcomes)
2. Run current FAST15M model → compute edge accuracy, PnL
3. Run RN-JD enhanced model → same metrics
4. Compare Sharpe, win rate, drawdown

### Paper Trading A/B
1. Deploy RN-JD model alongside current model
2. Both generate signals; only one executes (random selection)
3. Track virtual PnL for both
4. Statistical significance test after N trades

---

## Plausibility Assessment

### Why This Should Work
1. **15m markets are efficient** → martingale assumption is reasonable
2. **Short horizon** → drift correction is small but systematic
3. **Price-linked** → Binance vol translates to belief vol
4. **High frequency** → enough data to estimate σ_b

### Why It Might Not
1. **15m is too short** for vol to matter much (σ_b * √dt is tiny)
2. **Polymarket orderbook noise** may dominate signal
3. **Jump detection** may trigger too often on microstructure artifacts

### Verdict: **Worth Implementing Phase 1-2**
The drift correction and belief-vol concepts are theoretically sound. Phase 1 is low-risk and should improve edge estimation slightly. Phase 2 provides useful diagnostics even if not directly profitable.

---

## Timeline Estimate

| Phase | Days | Dependencies |
|-------|------|--------------|
| Phase 1: Enhanced p_up | 1-2 | None |
| Phase 2: Belief Vol Surface | 3-5 | Phase 1 |
| Phase 3: Jump Detection | 2-3 | Phase 2 |
| Phase 4: Adaptive Kelly | 1 | Phase 2 |
| Backtest + Validation | 3-5 | All phases |
| **Total** | **10-16 days** | |

---

## Code Locations

```
rust-backend/src/vault/
├── updown15m.rs          # Modify: add RN-JD p_up estimation
├── belief_vol.rs         # New: belief volatility tracking
├── kelly.rs              # Modify: add vol-adjusted Kelly
└── engine.rs             # Modify: integrate new components

rust-backend/src/signals/prediction_market_pricing/
├── overview.md           # This overview
├── implementation_plan_15m.md  # This plan
└── 2510.15205v1.pdf      # Original paper
```

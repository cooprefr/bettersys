# Paper Overview: Toward Black-Scholes for Prediction Markets

**Title:** Toward Black-Scholes for Prediction Markets: A Unified Kernel and Market Maker's Handbook  
**Author:** Shaw Dalen (Daedalus Research Team)  
**arXiv:** 2510.15205v1 [cs.CE] - October 2025

---

## Core Problem

Prediction markets (Polymarket, Kalshi, etc.) lack a unifying **stochastic kernel**—the equivalent of Black-Scholes for options. Without this:
- Market makers cannot isolate "belief risk" (level, volatility, jumps, correlation)
- No standardized tools for quoting, hedging, or pricing derivatives on event probabilities
- Spreads widen around news; inventory near 0/1 boundaries becomes unmanageable

---

## Proposed Solution: RN-JD (Risk-Neutral Jump-Diffusion) Kernel

### The Core Model

Transform probability `p_t ∈ (0,1)` to **log-odds** `x_t = logit(p_t) = log(p_t / (1 - p_t))`, then model as:

```
dx_t = μ(t, x_t) dt + σ_b(t, x_t) dW_t + ∫ z Ñ(dt, dz)
```

Where:
- `σ_b` = **belief volatility** (how fast log-odds move)
- `W_t` = Brownian motion (Q-measure)
- `Ñ` = compensated jump measure (news shocks)
- `p_t = S(x_t) = 1 / (1 + e^(-x_t))` (sigmoid)

### Key Insight: Martingale Constraint

Since `p_t` is the risk-neutral price of a binary contract paying $1 if event occurs:
- `{p_t}` must be a **Q-martingale**
- This pins down the drift `μ` via Itô-Lévy calculus
- No free parameters in drift—it's determined by `σ_b`, jump intensity, and jump moments

---

## Tradable Risk Factors

| Factor | Description | Trading Instrument |
|--------|-------------|-------------------|
| **Belief Level** | Current `p_t` | Base contract (long/short) |
| **Belief Volatility** | `σ_b` - how fast log-odds move | Belief-variance swaps |
| **Jump Intensity** | `λ` - news shock arrival rate | Jump risk premium |
| **Jump Moments** | Mean/variance of jump sizes | Tail risk instruments |
| **Cross-Event Correlation** | Diffusive `ρ_ij` | Correlation swaps |
| **Co-Jumps** | Common jumps across events | Conditional baskets |

---

## Derivative Layer (Analogous to Options)

### 1. Belief-Variance Swaps
Exchange realized quadratic variation of `x_t` (or `p_t`) for a fixed strike. Lets makers hedge volatility exposure.

### 2. Correlation/Covariance Swaps
Hedge baskets of related events (e.g., multiple election races). Offset diffusive correlation and co-jumps.

### 3. Corridor Variance
Focus hedging on the "swing zone" `p ∈ [0.3, 0.7]` where belief moves matter most.

### 4. First-Passage Notes
Transfer threshold-crossing risk ("Will `p` breach 0.8 before time T?"). Critical when quotes cluster near boundaries.

---

## Calibration Pipeline

1. **Filter microstructure noise** from mid/bid-ask/trade streams (state-space methods)
2. **Separate diffusion from jumps** via EM algorithm
3. **Enforce RN drift** constraint
4. **Co-jump detection** across related events
5. **Output:** Stable belief-volatility surface suitable for quoting

---

## Why This Matters

### The Black-Scholes Lesson
BS wasn't "true"—it was **standardized**. It coordinated quoting/hedging around a small number of state variables (σ), enabling:
- Implied volatility as a common language
- Greeks for risk management
- Deep derivative ecosystem

### For Prediction Markets
A shared belief-variance surface would:
- Let makers quote tighter spreads while hedging vol/jump risk
- Enable calendar hedges (between maturities)
- Support cross-event hedges (correlated races)
- Concentrate liquidity around a few quoted risk factors

---

## Validation Results

From the paper's experiments:
- RN-JD model achieves **sub-1% RMSE** on synthetic RN-consistent paths
- On real event data: outperforms naive/AR(1)/GARCH baselines
- Validated causal calibration (no look-ahead) and economic interpretability

---

## Key Equations Reference

**Logit transform:**
```
x_t = log(p_t / (1 - p_t))
p_t = 1 / (1 + e^(-x_t))
```

**Sigmoid derivatives:**
```
S'(x) = p(1-p)
S''(x) = p(1-p)(1-2p)
```

**RN drift constraint (simplified for pure diffusion):**
```
μ = -½ σ_b² (1 - 2p)
```

**Multi-event model:**
```
dx_t^i = μ^i dt + σ_b^i dW_t^i + ∫ z^i Ñ^i(dt, dz)
Corr(dW^i, dW^j) = ρ_ij
```

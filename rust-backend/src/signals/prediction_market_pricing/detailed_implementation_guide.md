# Detailed Implementation Guide: RN-JD Kernel for 15m Up/Down Markets

## Master Prompt Index

This document contains **47 sequential prompts** organized into phases. Each prompt is self-contained and can be given to an AI assistant to implement that specific piece.

---

## Prerequisites Checklist

Before starting, ensure:
- [ ] Rust backend compiles (`cargo build --release`)
- [ ] Binance price feed is working (`BINANCE_ENABLED=true`)
- [ ] FAST15M engine runs in paper mode
- [ ] At least 24h of 15m market data in `betterbot_signals.db`

---

# PHASE 0: Foundation & Data Structures (Day 1)

## Prompt 0.1: Create the belief_vol module skeleton

```
Create a new file `rust-backend/src/vault/belief_vol.rs` with the following structure:

1. Module documentation explaining the RN-JD belief volatility concept
2. A `BeliefVolEstimate` struct with fields:
   - `sigma_b: f64` (belief volatility in log-odds space)
   - `sample_count: usize`
   - `last_updated: i64` (unix timestamp)
   - `confidence: f64` (0-1, how reliable the estimate is)
3. A `BeliefVolConfig` struct with fields:
   - `min_samples: usize` (default 30)
   - `ema_alpha: f64` (default 0.1 for exponential moving average)
   - `max_age_secs: i64` (default 3600)
   - `prior_sigma_b: f64` (default 2.0, fallback value)
4. Empty impl blocks for both structs
5. Unit test module skeleton

Do NOT implement any methods yet - just the structure.
```

## Prompt 0.2: Add logit/sigmoid utility functions

```
In `rust-backend/src/vault/belief_vol.rs`, add these utility functions below the struct definitions:

1. `pub fn logit(p: f64) -> f64`
   - Returns log(p / (1-p))
   - Clamp input to (0.0001, 0.9999) to avoid infinities
   - Add docstring explaining this is the log-odds transform

2. `pub fn sigmoid(x: f64) -> f64`
   - Returns 1 / (1 + exp(-x))
   - This is the inverse of logit
   - Add docstring

3. `pub fn sigmoid_derivative(p: f64) -> f64`
   - Returns p * (1 - p)
   - This is S'(x) where p = S(x)
   - Used for chain rule conversions

4. `pub fn sigmoid_second_derivative(p: f64) -> f64`
   - Returns p * (1 - p) * (1 - 2*p)
   - This is S''(x)
   - Used for Itô correction term

Add unit tests for each function:
- logit(0.5) should be 0.0
- sigmoid(0.0) should be 0.5
- logit(sigmoid(x)) should return x for x in [-5, 5]
- sigmoid_derivative(0.5) should be 0.25
```

## Prompt 0.3: Register belief_vol module

```
Modify `rust-backend/src/vault/mod.rs` to:

1. Add `pub mod belief_vol;`
2. Add to the pub use statement:
   - `belief_vol::{logit, sigmoid, sigmoid_derivative, sigmoid_second_derivative, BeliefVolEstimate, BeliefVolConfig}`

Then run `cargo check` to ensure it compiles.
```

## Prompt 0.4: Create LogOddsIncrement struct

```
In `rust-backend/src/vault/belief_vol.rs`, add a struct to track log-odds increments:

```rust
/// A single observation of log-odds change
#[derive(Debug, Clone, Copy)]
pub struct LogOddsIncrement {
    pub timestamp: i64,
    pub x_before: f64,      // logit(p_before)
    pub x_after: f64,       // logit(p_after)
    pub dt_secs: f64,       // time delta in seconds
}

impl LogOddsIncrement {
    pub fn new(p_before: f64, p_after: f64, dt_secs: f64, timestamp: i64) -> Self {
        Self {
            timestamp,
            x_before: logit(p_before),
            x_after: logit(p_after),
            dt_secs,
        }
    }
    
    /// Returns the log-odds change
    pub fn delta_x(&self) -> f64 {
        self.x_after - self.x_before
    }
    
    /// Returns annualized squared increment (for variance estimation)
    pub fn annualized_sq_increment(&self) -> f64 {
        if self.dt_secs <= 0.0 {
            return 0.0;
        }
        let dt_years = self.dt_secs / (365.25 * 24.0 * 3600.0);
        let dx = self.delta_x();
        (dx * dx) / dt_years
    }
}
```

Add tests:
- Verify delta_x() calculation
- Verify annualized_sq_increment() with known values
```

## Prompt 0.5: Create BeliefVolTracker struct

```
In `rust-backend/src/vault/belief_vol.rs`, add the main tracker struct:

```rust
use std::collections::{HashMap, VecDeque};

/// Tracks belief volatility estimates per market
pub struct BeliefVolTracker {
    config: BeliefVolConfig,
    /// market_slug -> recent increments
    history: HashMap<String, VecDeque<LogOddsIncrement>>,
    /// market_slug -> current estimate
    estimates: HashMap<String, BeliefVolEstimate>,
    /// Maximum history entries per market
    max_history: usize,
}

impl BeliefVolTracker {
    pub fn new(config: BeliefVolConfig) -> Self {
        Self {
            config,
            history: HashMap::new(),
            estimates: HashMap::new(),
            max_history: 1000,
        }
    }
    
    pub fn with_max_history(mut self, max: usize) -> Self {
        self.max_history = max;
        self
    }
    
    /// Get current sigma_b estimate for a market, or prior if unavailable
    pub fn get_sigma_b(&self, market_slug: &str) -> f64 {
        self.estimates
            .get(market_slug)
            .filter(|e| e.sample_count >= self.config.min_samples)
            .map(|e| e.sigma_b)
            .unwrap_or(self.config.prior_sigma_b)
    }
    
    /// Get full estimate if available
    pub fn get_estimate(&self, market_slug: &str) -> Option<&BeliefVolEstimate> {
        self.estimates.get(market_slug)
    }
    
    /// Check if we have a reliable estimate
    pub fn has_reliable_estimate(&self, market_slug: &str) -> bool {
        self.estimates
            .get(market_slug)
            .map(|e| e.sample_count >= self.config.min_samples && e.confidence > 0.5)
            .unwrap_or(false)
    }
}
```
```

---

# PHASE 1: Belief Volatility Estimation (Days 2-4)

## Prompt 1.1: Implement record_observation method

```
In `rust-backend/src/vault/belief_vol.rs`, add to the `BeliefVolTracker` impl:

```rust
/// Record a new price observation for a market
pub fn record_observation(
    &mut self,
    market_slug: &str,
    p_now: f64,
    timestamp: i64,
) {
    let slug = market_slug.to_lowercase();
    
    // Get or create history for this market
    let history = self.history.entry(slug.clone()).or_insert_with(VecDeque::new);
    
    // If we have previous observation, create increment
    if let Some(last) = history.back() {
        let dt_secs = (timestamp - last.timestamp) as f64;
        
        // Only record if reasonable time delta (1s to 1h)
        if dt_secs >= 1.0 && dt_secs <= 3600.0 {
            let p_before = sigmoid(last.x_after);
            let increment = LogOddsIncrement::new(p_before, p_now, dt_secs, timestamp);
            
            history.push_back(increment);
            
            // Trim history
            while history.len() > self.max_history {
                history.pop_front();
            }
            
            // Update estimate
            self.update_estimate(&slug, timestamp);
        }
    } else {
        // First observation - just record the logit value
        let fake_increment = LogOddsIncrement {
            timestamp,
            x_before: logit(p_now),
            x_after: logit(p_now),
            dt_secs: 0.0,
        };
        history.push_back(fake_increment);
    }
}
```

Add a test that:
1. Creates a tracker
2. Records 50 observations with p values varying around 0.5
3. Verifies history length is correct
```

## Prompt 1.2: Implement update_estimate method (EMA approach)

```
In `rust-backend/src/vault/belief_vol.rs`, add the private method:

```rust
impl BeliefVolTracker {
    /// Update sigma_b estimate using exponential moving average of squared increments
    fn update_estimate(&mut self, market_slug: &str, now_ts: i64) {
        let Some(history) = self.history.get(market_slug) else {
            return;
        };
        
        if history.len() < 2 {
            return;
        }
        
        // Calculate EMA of annualized variance
        let alpha = self.config.ema_alpha;
        let mut ema_var: Option<f64> = None;
        let mut count = 0usize;
        
        for inc in history.iter().skip(1) {  // Skip first (fake) entry
            if inc.dt_secs <= 0.0 {
                continue;
            }
            
            let sq_inc = inc.annualized_sq_increment();
            
            match ema_var {
                Some(prev) => {
                    ema_var = Some(alpha * sq_inc + (1.0 - alpha) * prev);
                }
                None => {
                    ema_var = Some(sq_inc);
                }
            }
            count += 1;
        }
        
        if let Some(var) = ema_var {
            let sigma_b = var.sqrt().max(0.01);  // Floor at 1% annualized
            
            // Confidence based on sample count
            let confidence = (count as f64 / self.config.min_samples as f64).min(1.0);
            
            self.estimates.insert(
                market_slug.to_string(),
                BeliefVolEstimate {
                    sigma_b,
                    sample_count: count,
                    last_updated: now_ts,
                    confidence,
                },
            );
        }
    }
}
```

Add tests:
1. Feed constant p=0.5 -> sigma_b should be small
2. Feed alternating p=0.4, p=0.6 -> sigma_b should be larger
3. More samples -> higher confidence
```

## Prompt 1.3: Implement realized_vol_from_history method

```
In `rust-backend/src/vault/belief_vol.rs`, add an alternative estimation method:

```rust
impl BeliefVolTracker {
    /// Calculate realized volatility using simple sum of squared increments
    /// (no EMA, just raw calculation over a window)
    pub fn realized_vol_window(
        &self,
        market_slug: &str,
        window_secs: i64,
        now_ts: i64,
    ) -> Option<f64> {
        let history = self.history.get(market_slug)?;
        
        let cutoff = now_ts - window_secs;
        
        let mut sum_sq = 0.0;
        let mut total_dt = 0.0;
        let mut count = 0;
        
        for inc in history.iter().rev() {
            if inc.timestamp < cutoff {
                break;
            }
            if inc.dt_secs <= 0.0 {
                continue;
            }
            
            let dx = inc.delta_x();
            sum_sq += dx * dx;
            total_dt += inc.dt_secs;
            count += 1;
        }
        
        if count < 5 || total_dt < 60.0 {
            return None;  // Not enough data
        }
        
        // Annualize: variance per year
        let secs_per_year = 365.25 * 24.0 * 3600.0;
        let annualized_var = sum_sq * (secs_per_year / total_dt);
        
        Some(annualized_var.sqrt())
    }
}
```

Add test comparing EMA vs realized_vol_window on same data.
```

## Prompt 1.4: Add jump detection to BeliefVolTracker

```
In `rust-backend/src/vault/belief_vol.rs`, add jump detection:

```rust
/// Result of jump detection
#[derive(Debug, Clone, Copy)]
pub struct JumpDetectionResult {
    pub is_jump: bool,
    pub z_score: f64,
    pub jump_size: f64,
    pub threshold_used: f64,
}

impl BeliefVolTracker {
    /// Detect if the most recent move was a jump (news shock)
    pub fn detect_recent_jump(
        &self,
        market_slug: &str,
        threshold_sigma: f64,  // typically 3.0
    ) -> Option<JumpDetectionResult> {
        let history = self.history.get(market_slug)?;
        let estimate = self.estimates.get(market_slug)?;
        
        // Get most recent increment
        let recent = history.back()?;
        if recent.dt_secs <= 0.0 {
            return None;
        }
        
        let dx = recent.delta_x();
        let dt_years = recent.dt_secs / (365.25 * 24.0 * 3600.0);
        
        // Expected std dev for this time interval
        let expected_std = estimate.sigma_b * dt_years.sqrt();
        
        if expected_std <= 0.0 {
            return None;
        }
        
        let z_score = dx.abs() / expected_std;
        let is_jump = z_score > threshold_sigma;
        
        Some(JumpDetectionResult {
            is_jump,
            z_score,
            jump_size: if is_jump { dx } else { 0.0 },
            threshold_used: threshold_sigma,
        })
    }
    
    /// Count jumps in recent history
    pub fn count_recent_jumps(
        &self,
        market_slug: &str,
        window_secs: i64,
        now_ts: i64,
        threshold_sigma: f64,
    ) -> usize {
        let Some(history) = self.history.get(market_slug) else {
            return 0;
        };
        let Some(estimate) = self.estimates.get(market_slug) else {
            return 0;
        };
        
        let cutoff = now_ts - window_secs;
        let mut jump_count = 0;
        
        for inc in history.iter().rev() {
            if inc.timestamp < cutoff {
                break;
            }
            if inc.dt_secs <= 0.0 {
                continue;
            }
            
            let dt_years = inc.dt_secs / (365.25 * 24.0 * 3600.0);
            let expected_std = estimate.sigma_b * dt_years.sqrt();
            
            if expected_std > 0.0 {
                let z = inc.delta_x().abs() / expected_std;
                if z > threshold_sigma {
                    jump_count += 1;
                }
            }
        }
        
        jump_count
    }
}
```

Add tests with synthetic data containing known jumps.
```

## Prompt 1.5: Add serialization for persistence

```
In `rust-backend/src/vault/belief_vol.rs`, add serde support:

1. Add `use serde::{Serialize, Deserialize};` at top
2. Add `#[derive(Serialize, Deserialize)]` to:
   - BeliefVolEstimate
   - BeliefVolConfig
   - LogOddsIncrement
3. Add methods to BeliefVolTracker:

```rust
impl BeliefVolTracker {
    /// Export all estimates as JSON-serializable map
    pub fn export_estimates(&self) -> HashMap<String, BeliefVolEstimate> {
        self.estimates.clone()
    }
    
    /// Import estimates (e.g., on startup from DB)
    pub fn import_estimates(&mut self, estimates: HashMap<String, BeliefVolEstimate>) {
        for (slug, est) in estimates {
            self.estimates.insert(slug, est);
        }
    }
    
    /// Get summary stats for logging
    pub fn summary(&self) -> BeliefVolSummary {
        BeliefVolSummary {
            markets_tracked: self.estimates.len(),
            reliable_estimates: self.estimates.values()
                .filter(|e| e.sample_count >= self.config.min_samples)
                .count(),
            avg_sigma_b: if self.estimates.is_empty() {
                self.config.prior_sigma_b
            } else {
                self.estimates.values().map(|e| e.sigma_b).sum::<f64>() 
                    / self.estimates.len() as f64
            },
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BeliefVolSummary {
    pub markets_tracked: usize,
    pub reliable_estimates: usize,
    pub avg_sigma_b: f64,
}
```
```

## Prompt 1.6: Integration test with real-ish data

```
In `rust-backend/src/vault/belief_vol.rs`, add an integration test:

```rust
#[cfg(test)]
mod integration_tests {
    use super::*;
    
    /// Simulate a 15m market session
    #[test]
    fn test_15m_session_simulation() {
        let config = BeliefVolConfig::default();
        let mut tracker = BeliefVolTracker::new(config);
        
        let slug = "btc-updown-15m-test";
        let mut ts = 1700000000i64;
        let mut p = 0.50f64;
        
        // Simulate 100 observations over 15 minutes
        // Price follows a random walk (ish)
        let mut rng_seed = 42u64;
        
        for _ in 0..100 {
            // Simple LCG for reproducibility
            rng_seed = rng_seed.wrapping_mul(1103515245).wrapping_add(12345);
            let rand_unit = (rng_seed % 1000) as f64 / 1000.0;  // 0 to 1
            
            // Random walk in logit space
            let x = logit(p);
            let dx = (rand_unit - 0.5) * 0.1;  // Small random change
            p = sigmoid(x + dx);
            
            tracker.record_observation(slug, p, ts);
            ts += 9;  // ~9 seconds between observations
        }
        
        // Should have estimate now
        assert!(tracker.has_reliable_estimate(slug));
        
        let sigma_b = tracker.get_sigma_b(slug);
        println!("Estimated sigma_b: {:.4}", sigma_b);
        
        // Should be positive and reasonable
        assert!(sigma_b > 0.0);
        assert!(sigma_b < 100.0);  // Sanity check
        
        // Test realized vol window
        let rv = tracker.realized_vol_window(slug, 600, ts);
        assert!(rv.is_some());
        println!("Realized vol (10m window): {:.4}", rv.unwrap());
    }
}
```
```

---

# PHASE 2: RN-JD Enhanced p_up Estimation (Days 5-7)

## Prompt 2.1: Create rnjd module skeleton

```
Create a new file `rust-backend/src/vault/rnjd.rs`:

```rust
//! Risk-Neutral Jump-Diffusion (RN-JD) pricing for prediction markets
//!
//! Based on: "Toward Black-Scholes for Prediction Markets" (arXiv:2510.15205)
//!
//! Key insight: The traded probability p_t must be a Q-martingale,
//! which pins down the drift via Itô-Lévy calculus.

use crate::vault::belief_vol::{logit, sigmoid, sigmoid_derivative, sigmoid_second_derivative};

/// Parameters for RN-JD model
#[derive(Debug, Clone)]
pub struct RnjdParams {
    /// Belief volatility (annualized, in log-odds space)
    pub sigma_b: f64,
    /// Jump intensity (expected jumps per year)
    pub lambda: f64,
    /// Mean jump size in log-odds
    pub mu_j: f64,
    /// Jump size std dev
    pub sigma_j: f64,
}

impl Default for RnjdParams {
    fn default() -> Self {
        Self {
            sigma_b: 2.0,    // Moderate belief vol
            lambda: 0.0,     // No jumps by default (pure diffusion)
            mu_j: 0.0,
            sigma_j: 0.1,
        }
    }
}

/// Result of RN-JD probability estimation
#[derive(Debug, Clone)]
pub struct RnjdEstimate {
    /// Estimated probability of "Up" outcome
    pub p_up: f64,
    /// The RN drift correction applied
    pub drift_correction: f64,
    /// Raw estimate before correction
    pub p_up_raw: f64,
    /// Confidence in estimate (0-1)
    pub confidence: f64,
    /// Diagnostic: was jump regime detected?
    pub jump_regime: bool,
}
```

Register in `mod.rs`:
```rust
pub mod rnjd;
pub use rnjd::{RnjdParams, RnjdEstimate};
```
```

## Prompt 2.2: Implement RN drift calculation

```
In `rust-backend/src/vault/rnjd.rs`, add the core drift function:

```rust
/// Calculate the risk-neutral drift for log-odds x_t
///
/// From the paper's Eq. (3):
/// μ(t, x) = -½ σ_b² S''(x)/S'(x) - jump_compensation
///
/// For pure diffusion, this simplifies to:
/// μ = -½ σ_b² (1 - 2p)
///
/// The drift is DETERMINED by no-arbitrage, not a free parameter.
pub fn rn_drift(p: f64, params: &RnjdParams) -> f64 {
    let p_clamped = p.clamp(0.001, 0.999);
    
    // Pure diffusion component
    // S'(x) = p(1-p), S''(x) = p(1-p)(1-2p)
    // S''/S' = (1 - 2p)
    let diffusion_drift = -0.5 * params.sigma_b.powi(2) * (1.0 - 2.0 * p_clamped);
    
    // Jump compensation (if jumps are modeled)
    // This ensures E^Q[dp_t] = 0
    let jump_compensation = if params.lambda > 0.0 {
        // Expected jump contribution to p
        // Simplified: assume small jumps, linearize
        params.lambda * params.mu_j * sigmoid_derivative(logit(p_clamped))
    } else {
        0.0
    };
    
    diffusion_drift - jump_compensation
}

/// Calculate expected change in p over time dt (in years)
pub fn expected_dp(p: f64, dt_years: f64, params: &RnjdParams) -> f64 {
    // For a martingale, E[dp] = 0 under Q
    // But we track the drift in x, then transform
    let x = logit(p);
    let mu_x = rn_drift(p, params);
    
    // Approximate: dp ≈ S'(x) * dx = p(1-p) * μ_x * dt
    sigmoid_derivative(x) * mu_x * dt_years
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_rn_drift_at_half() {
        let params = RnjdParams::default();
        // At p = 0.5, (1 - 2p) = 0, so drift should be 0
        let drift = rn_drift(0.5, &params);
        assert!(drift.abs() < 0.001);
    }
    
    #[test]
    fn test_rn_drift_asymmetry() {
        let params = RnjdParams { sigma_b: 1.0, ..Default::default() };
        
        // At p < 0.5, drift should be positive (pushing toward 0.5)
        let drift_low = rn_drift(0.3, &params);
        // At p > 0.5, drift should be negative (pushing toward 0.5)
        let drift_high = rn_drift(0.7, &params);
        
        assert!(drift_low > 0.0);
        assert!(drift_high < 0.0);
        // Should be symmetric
        assert!((drift_low + drift_high).abs() < 0.001);
    }
}
```
```

## Prompt 2.3: Implement p_up estimation with RN-JD correction

```
In `rust-backend/src/vault/rnjd.rs`, add the main estimation function:

```rust
use statrs::distribution::{ContinuousCDF, Normal};

/// Estimate probability of "Up" using RN-JD model
///
/// This replaces the simple `p_up_driftless_lognormal` with a more
/// theoretically grounded approach.
///
/// # Arguments
/// * `p_start` - Price at market start
/// * `p_now` - Current underlying price  
/// * `market_p` - Current market probability (mid price)
/// * `sigma_price` - Realized price volatility (annualized)
/// * `t_rem_secs` - Time remaining to expiry in seconds
/// * `params` - RN-JD parameters (primarily sigma_b)
pub fn estimate_p_up_rnjd(
    p_start: f64,
    p_now: f64,
    market_p: f64,
    sigma_price: f64,
    t_rem_secs: f64,
    params: &RnjdParams,
) -> Option<RnjdEstimate> {
    // Validate inputs
    if !(p_start > 0.0 && p_now > 0.0) {
        return None;
    }
    if !(sigma_price > 0.0 && sigma_price.is_finite()) {
        return None;
    }
    if !(t_rem_secs > 0.0 && t_rem_secs.is_finite()) {
        return None;
    }
    
    let market_p = market_p.clamp(0.01, 0.99);
    let t_years = t_rem_secs / (365.25 * 24.0 * 3600.0);
    
    // Step 1: Raw probability from price return distribution
    let log_return = (p_now / p_start).ln();
    let std_dev = sigma_price * t_years.sqrt();
    
    if std_dev <= 0.0 {
        return None;
    }
    
    let n = Normal::new(0.0, 1.0).ok()?;
    let z = log_return / std_dev;
    let p_up_raw = n.cdf(z).clamp(0.001, 0.999);
    
    // Step 2: Calculate RN drift correction
    // The market price should already reflect this, but we use it
    // to adjust our estimate toward the martingale-consistent value
    let drift = rn_drift(market_p, params);
    let drift_correction = drift * t_years;
    
    // Step 3: Apply correction
    // Convert to logit space, apply, convert back
    let x_raw = logit(p_up_raw);
    let x_adjusted = x_raw + drift_correction;
    let p_up = sigmoid(x_adjusted).clamp(0.001, 0.999);
    
    // Step 4: Confidence based on:
    // - How close market_p is to 0.5 (more uncertain at extremes)
    // - Time remaining (more uncertain with more time)
    // - Volatility regime
    let time_factor = (-t_rem_secs / 900.0).exp();  // Decays over 15 min
    let extremity_penalty = 4.0 * market_p * (1.0 - market_p);  // Max at 0.5
    let confidence = (time_factor * extremity_penalty).clamp(0.1, 0.95);
    
    Some(RnjdEstimate {
        p_up,
        drift_correction,
        p_up_raw,
        confidence,
        jump_regime: false,  // TODO: implement
    })
}
```

Add tests comparing with the original `p_up_driftless_lognormal`.
```

## Prompt 2.4: Convert price volatility to belief volatility

```
In `rust-backend/src/vault/rnjd.rs`, add conversion function:

```rust
/// Convert price volatility to belief volatility
///
/// For price-linked events (like Up/Down), the relationship is:
/// σ_b ≈ σ_price / p(1-p)
///
/// This comes from the chain rule: if p is linked to price via some
/// function, the volatility transforms accordingly.
///
/// For 15m Up/Down markets, this is an approximation since the
/// actual payoff is binary, not continuous.
pub fn price_vol_to_belief_vol(sigma_price: f64, market_p: f64) -> f64 {
    let p = market_p.clamp(0.05, 0.95);  // Avoid extreme values
    let sensitivity = p * (1.0 - p);
    
    if sensitivity < 0.01 {
        return sigma_price * 10.0;  // Cap at 10x when near boundaries
    }
    
    (sigma_price / sensitivity).min(50.0)  // Cap at 50 annualized
}

/// Blend our sigma_b estimate with price-derived estimate
pub fn blend_sigma_b(
    tracked_sigma_b: Option<f64>,
    price_sigma_b: f64,
    blend_weight: f64,  // Weight on tracked (0-1)
) -> f64 {
    match tracked_sigma_b {
        Some(tracked) => {
            blend_weight * tracked + (1.0 - blend_weight) * price_sigma_b
        }
        None => price_sigma_b,
    }
}

#[cfg(test)]
mod conversion_tests {
    use super::*;
    
    #[test]
    fn test_price_to_belief_vol() {
        // At p=0.5, sensitivity = 0.25, so sigma_b = 4 * sigma_price
        let sigma_b = price_vol_to_belief_vol(0.20, 0.5);
        assert!((sigma_b - 0.80).abs() < 0.01);
        
        // At p=0.1, sensitivity = 0.09, sigma_b is higher
        let sigma_b_extreme = price_vol_to_belief_vol(0.20, 0.1);
        assert!(sigma_b_extreme > sigma_b);
    }
}
```
```

## Prompt 2.5: Add jump regime detection

```
In `rust-backend/src/vault/rnjd.rs`, add:

```rust
/// Detect if we're in a high-jump regime (news absorption period)
#[derive(Debug, Clone)]
pub struct JumpRegimeDetector {
    /// Recent jump count threshold
    pub jump_count_threshold: usize,
    /// Window for counting jumps (seconds)
    pub window_secs: i64,
    /// Z-score threshold for jump detection
    pub z_threshold: f64,
    /// Cooldown after detected jump regime (seconds)
    pub cooldown_secs: i64,
}

impl Default for JumpRegimeDetector {
    fn default() -> Self {
        Self {
            jump_count_threshold: 2,
            window_secs: 300,  // 5 minutes
            z_threshold: 3.0,
            cooldown_secs: 60,
        }
    }
}

impl JumpRegimeDetector {
    /// Check if current regime suggests elevated jump risk
    pub fn is_jump_regime(
        &self,
        recent_jump_count: usize,
        last_jump_ts: Option<i64>,
        now_ts: i64,
    ) -> bool {
        // High recent jump count
        if recent_jump_count >= self.jump_count_threshold {
            return true;
        }
        
        // Recently had a jump (cooldown)
        if let Some(ts) = last_jump_ts {
            if now_ts - ts < self.cooldown_secs {
                return true;
            }
        }
        
        false
    }
    
    /// Suggested edge multiplier in jump regime
    pub fn edge_multiplier(&self, in_jump_regime: bool) -> f64 {
        if in_jump_regime {
            0.5  // Require 2x edge to trade during jump regime
        } else {
            1.0
        }
    }
}
```
```

## Prompt 2.6: Create unified estimate_p_up_enhanced function

```
In `rust-backend/src/vault/rnjd.rs`, create the main entry point that combines everything:

```rust
use crate::vault::belief_vol::BeliefVolTracker;

/// Enhanced p_up estimation combining all components
///
/// This is the main function to call from the FAST15M engine.
pub fn estimate_p_up_enhanced(
    p_start: f64,
    p_now: f64,
    market_p: f64,
    sigma_price: f64,
    t_rem_secs: f64,
    belief_tracker: Option<&BeliefVolTracker>,
    market_slug: &str,
    now_ts: i64,
) -> Option<RnjdEstimate> {
    // Step 1: Get or estimate sigma_b
    let price_derived_sigma_b = price_vol_to_belief_vol(sigma_price, market_p);
    
    let sigma_b = match belief_tracker {
        Some(tracker) if tracker.has_reliable_estimate(market_slug) => {
            // Blend tracked with price-derived (70% tracked, 30% price)
            blend_sigma_b(Some(tracker.get_sigma_b(market_slug)), price_derived_sigma_b, 0.7)
        }
        _ => price_derived_sigma_b,
    };
    
    // Step 2: Check for jump regime
    let (jump_regime, jump_count) = match belief_tracker {
        Some(tracker) => {
            let count = tracker.count_recent_jumps(market_slug, 300, now_ts, 3.0);
            (count >= 2, count)
        }
        None => (false, 0),
    };
    
    // Step 3: Build params and estimate
    let params = RnjdParams {
        sigma_b,
        lambda: if jump_regime { 10.0 } else { 0.0 },  // Elevated during jump regime
        ..Default::default()
    };
    
    let mut estimate = estimate_p_up_rnjd(
        p_start,
        p_now,
        market_p,
        sigma_price,
        t_rem_secs,
        &params,
    )?;
    
    estimate.jump_regime = jump_regime;
    
    // Reduce confidence if in jump regime
    if jump_regime {
        estimate.confidence *= 0.7;
    }
    
    Some(estimate)
}
```
```

---

# PHASE 3: Integration with FAST15M Engine (Days 8-10)

## Prompt 3.1: Add BeliefVolTracker to AppState

```
Modify `rust-backend/src/main.rs`:

1. Add import:
```rust
use crate::vault::belief_vol::{BeliefVolTracker, BeliefVolConfig};
```

2. Add to AppState struct:
```rust
pub struct AppState {
    // ... existing fields ...
    pub belief_vol_tracker: Arc<parking_lot::RwLock<BeliefVolTracker>>,
}
```

3. Initialize in main():
```rust
let belief_vol_config = BeliefVolConfig::default();
let belief_vol_tracker = Arc::new(parking_lot::RwLock::new(
    BeliefVolTracker::new(belief_vol_config)
));
```

4. Add to AppState construction.

Run `cargo check` to find and fix any compilation errors.
```

## Prompt 3.2: Feed market prices to BeliefVolTracker

```
In `rust-backend/src/vault/engine.rs`, modify the FAST15M polling loop to record observations:

Find the section where market prices are fetched and add:

```rust
// After fetching orderbook/market price
if let Some(market_mid) = orderbook.mid_price() {
    let belief_tracker = state.belief_vol_tracker.write();
    belief_tracker.record_observation(
        &market_slug,
        market_mid,
        Utc::now().timestamp(),
    );
    drop(belief_tracker);
}
```

This should be added in the `run_updown_loop` or equivalent function where
the engine iterates over 15m markets.
```

## Prompt 3.3: Replace p_up calculation in engine

```
In `rust-backend/src/vault/engine.rs`, find where `p_up_driftless_lognormal` is called and replace with the enhanced version.

Current code likely looks like:
```rust
let p_up = p_up_driftless_lognormal(
    start_price,
    current_price,
    sigma,
    t_remaining,
)?;
let p_up_shrunk = shrink_to_half(p_up, cfg.updown_shrink_to_half);
```

Replace with:
```rust
use crate::vault::rnjd::estimate_p_up_enhanced;

// Get belief tracker reference
let belief_tracker = state.belief_vol_tracker.read();

let estimate = estimate_p_up_enhanced(
    start_price,
    current_price,
    market_mid,  // Current market price
    sigma,
    t_remaining,
    Some(&*belief_tracker),
    &market_slug,
    Utc::now().timestamp(),
);
drop(belief_tracker);

let Some(est) = estimate else {
    debug!("RN-JD estimation failed for {}", market_slug);
    continue;
};

// Apply shrink (conservative adjustment)
let p_up_shrunk = shrink_to_half(est.p_up, cfg.updown_shrink_to_half);

// Log diagnostics
debug!(
    "RN-JD estimate: p_up={:.4}, raw={:.4}, drift_corr={:.6}, jump_regime={}",
    est.p_up, est.p_up_raw, est.drift_correction, est.jump_regime
);
```
```

## Prompt 3.4: Add jump regime handling

```
In `rust-backend/src/vault/engine.rs`, after the estimate is computed, add jump regime handling:

```rust
// After getting estimate
let effective_min_edge = if est.jump_regime {
    // Require 2x edge during jump regime
    cfg.updown_min_edge * 2.0
} else {
    cfg.updown_min_edge
};

// Modify the edge check
let edge = (p_up_shrunk - market_mid).abs();
if edge < effective_min_edge {
    debug!(
        "Edge {:.4} below threshold {:.4} (jump_regime={})",
        edge, effective_min_edge, est.jump_regime
    );
    continue;
}
```

Also add logging when jump regime is detected:
```rust
if est.jump_regime {
    info!(
        "Jump regime detected for {}: requiring min_edge={:.4}",
        market_slug, effective_min_edge
    );
}
```
```

## Prompt 3.5: Add belief vol to signal context

```
When creating signal context for 15m Up/Down signals, include belief vol info.

In `rust-backend/src/vault/engine.rs`, find where signals are created and add to the context:

```rust
// When building signal details/context
let belief_tracker = state.belief_vol_tracker.read();
let sigma_b = belief_tracker.get_sigma_b(&market_slug);
let has_reliable_vol = belief_tracker.has_reliable_estimate(&market_slug);
drop(belief_tracker);

// Add to signal context/details (adjust struct as needed)
// This might go in SignalDetails or a new field
let rnjd_context = serde_json::json!({
    "sigma_b": sigma_b,
    "sigma_b_reliable": has_reliable_vol,
    "drift_correction": est.drift_correction,
    "jump_regime": est.jump_regime,
    "p_up_raw": est.p_up_raw,
    "p_up_adjusted": est.p_up,
});
```
```

## Prompt 3.6: Add API endpoint for belief vol stats

```
In `rust-backend/src/api/simple.rs`, add a new endpoint:

```rust
#[derive(Serialize)]
pub struct BeliefVolStatsResponse {
    pub markets_tracked: usize,
    pub reliable_estimates: usize,
    pub avg_sigma_b: f64,
    pub estimates: HashMap<String, BeliefVolEstimateDto>,
}

#[derive(Serialize)]
pub struct BeliefVolEstimateDto {
    pub sigma_b: f64,
    pub sample_count: usize,
    pub confidence: f64,
    pub last_updated: i64,
}

pub async fn get_belief_vol_stats(
    AxumState(state): AxumState<AppState>,
) -> Json<BeliefVolStatsResponse> {
    let tracker = state.belief_vol_tracker.read();
    let summary = tracker.summary();
    let estimates = tracker.export_estimates();
    
    let estimates_dto: HashMap<String, BeliefVolEstimateDto> = estimates
        .into_iter()
        .map(|(k, v)| (k, BeliefVolEstimateDto {
            sigma_b: v.sigma_b,
            sample_count: v.sample_count,
            confidence: v.confidence,
            last_updated: v.last_updated,
        }))
        .collect();
    
    Json(BeliefVolStatsResponse {
        markets_tracked: summary.markets_tracked,
        reliable_estimates: summary.reliable_estimates,
        avg_sigma_b: summary.avg_sigma_b,
        estimates: estimates_dto,
    })
}
```

Add route in `main.rs`:
```rust
.route("/api/belief-vol/stats", get(get_belief_vol_stats))
```
```

---

# PHASE 4: Vol-Adjusted Kelly (Day 11)

## Prompt 4.1: Add vol-adjusted Kelly function

```
In `rust-backend/src/vault/kelly.rs`, add a new function:

```rust
/// Kelly with belief volatility adjustment
///
/// Higher sigma_b means more uncertainty, so we reduce position size.
pub fn kelly_with_belief_vol(
    confidence: f64,
    market_price: f64,
    sigma_b: f64,
    t_years: f64,
    params: &KellyParams,
) -> KellyResult {
    // First calculate standard Kelly
    let mut result = calculate_kelly_position(confidence, market_price, params);
    
    if !result.should_trade {
        return result;
    }
    
    // Apply volatility penalty
    // Expected movement in p over holding period
    let p = market_price.clamp(0.01, 0.99);
    let sensitivity = p * (1.0 - p);
    let expected_p_move = sigma_b * t_years.sqrt() * sensitivity;
    
    // If expected movement is large relative to edge, reduce position
    let edge_safety_ratio = result.edge / (expected_p_move + 0.001);
    
    let vol_multiplier = if edge_safety_ratio < 1.0 {
        // Edge is smaller than expected vol - very risky
        edge_safety_ratio.max(0.1)
    } else if edge_safety_ratio < 2.0 {
        // Edge is 1-2x expected vol - reduce somewhat
        0.5 + 0.25 * edge_safety_ratio
    } else {
        // Edge is 2x+ expected vol - full position
        1.0
    };
    
    result.position_size_usd *= vol_multiplier;
    result.actual_fraction *= vol_multiplier;
    
    // Re-check minimum
    if result.position_size_usd < params.min_position_usd {
        result.should_trade = false;
        result.skip_reason = Some(format!(
            "Vol-adjusted position ${:.2} below min (vol_mult={:.2})",
            result.position_size_usd, vol_multiplier
        ));
    }
    
    result
}
```
```

## Prompt 4.2: Integrate vol-adjusted Kelly in engine

```
In `rust-backend/src/vault/engine.rs`, replace the Kelly calculation:

Find where `calculate_kelly_position` is called for 15m markets and replace:

```rust
// Old:
let kelly = calculate_kelly_position(p_up_shrunk, market_mid, &kelly_params);

// New:
use crate::vault::kelly::kelly_with_belief_vol;

let belief_tracker = state.belief_vol_tracker.read();
let sigma_b = belief_tracker.get_sigma_b(&market_slug);
drop(belief_tracker);

let t_years = t_remaining / (365.25 * 24.0 * 3600.0);
let kelly = kelly_with_belief_vol(
    p_up_shrunk,
    market_mid,
    sigma_b,
    t_years,
    &kelly_params,
);

if !kelly.should_trade {
    debug!("Kelly skip: {:?}", kelly.skip_reason);
    continue;
}
```
```

## Prompt 4.3: Add tests for vol-adjusted Kelly

```
In `rust-backend/src/vault/kelly.rs`, add tests:

```rust
#[cfg(test)]
mod vol_kelly_tests {
    use super::*;
    
    #[test]
    fn test_vol_reduces_position() {
        let params = KellyParams {
            bankroll: 10000.0,
            kelly_fraction: 0.25,
            max_position_pct: 0.10,
            min_position_usd: 1.0,
        };
        
        // Same inputs, different sigma_b
        let low_vol = kelly_with_belief_vol(0.60, 0.50, 1.0, 0.001, &params);
        let high_vol = kelly_with_belief_vol(0.60, 0.50, 10.0, 0.001, &params);
        
        // Higher vol should result in smaller position
        assert!(high_vol.position_size_usd <= low_vol.position_size_usd);
    }
    
    #[test]
    fn test_vol_can_skip_trade() {
        let params = KellyParams {
            bankroll: 1000.0,
            kelly_fraction: 0.25,
            max_position_pct: 0.10,
            min_position_usd: 10.0,
        };
        
        // Very high vol relative to small edge
        let result = kelly_with_belief_vol(0.52, 0.50, 20.0, 0.01, &params);
        
        // Should skip due to vol adjustment
        if result.position_size_usd < params.min_position_usd {
            assert!(!result.should_trade);
        }
    }
}
```
```

---

# PHASE 5: Testing & Validation (Days 12-16)

## Prompt 5.1: Create backtest data collector

```
Create `rust-backend/src/vault/backtest_rnjd.rs`:

```rust
//! Backtest framework for RN-JD model validation

use std::collections::HashMap;
use serde::{Serialize, Deserialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BacktestRecord {
    pub timestamp: i64,
    pub market_slug: String,
    pub p_start: f64,
    pub p_now: f64,
    pub market_mid: f64,
    pub sigma_price: f64,
    pub t_rem_secs: f64,
    
    // Model outputs
    pub p_up_old: f64,      // Original driftless lognormal
    pub p_up_rnjd: f64,     // RN-JD estimate
    pub sigma_b: f64,
    pub drift_correction: f64,
    pub jump_regime: bool,
    
    // Position sizing
    pub kelly_old: f64,
    pub kelly_rnjd: f64,
    
    // Outcome (filled in after resolution)
    pub resolved: bool,
    pub outcome_up: Option<bool>,
}

#[derive(Debug, Default)]
pub struct BacktestCollector {
    records: Vec<BacktestRecord>,
    max_records: usize,
}

impl BacktestCollector {
    pub fn new(max_records: usize) -> Self {
        Self {
            records: Vec::new(),
            max_records,
        }
    }
    
    pub fn add_record(&mut self, record: BacktestRecord) {
        self.records.push(record);
        if self.records.len() > self.max_records {
            self.records.remove(0);
        }
    }
    
    pub fn export(&self) -> &[BacktestRecord] {
        &self.records
    }
    
    pub fn export_json(&self) -> String {
        serde_json::to_string_pretty(&self.records).unwrap_or_default()
    }
}
```

Register in `mod.rs`.
```

## Prompt 5.2: Add backtest recording to engine

```
In `rust-backend/src/vault/engine.rs`:

1. Add BacktestCollector to engine state or AppState
2. Record each decision point:

```rust
// After calculating both old and new estimates
if let Some(collector) = &state.backtest_collector {
    let record = BacktestRecord {
        timestamp: Utc::now().timestamp(),
        market_slug: market_slug.clone(),
        p_start: start_price,
        p_now: current_price,
        market_mid,
        sigma_price: sigma,
        t_rem_secs: t_remaining,
        p_up_old: p_up_old,  // Calculate with old method
        p_up_rnjd: est.p_up,
        sigma_b,
        drift_correction: est.drift_correction,
        jump_regime: est.jump_regime,
        kelly_old: kelly_old.position_size_usd,
        kelly_rnjd: kelly_new.position_size_usd,
        resolved: false,
        outcome_up: None,
    };
    
    let mut collector = collector.write();
    collector.add_record(record);
}
```
```

## Prompt 5.3: Add backtest resolution

```
Create a function to resolve backtest records after market expiry:

```rust
impl BacktestCollector {
    /// Mark records as resolved with actual outcome
    pub fn resolve_market(&mut self, market_slug: &str, outcome_up: bool) {
        for record in &mut self.records {
            if record.market_slug == market_slug && !record.resolved {
                record.resolved = true;
                record.outcome_up = Some(outcome_up);
            }
        }
    }
    
    /// Calculate performance metrics
    pub fn calculate_metrics(&self) -> BacktestMetrics {
        let resolved: Vec<_> = self.records.iter()
            .filter(|r| r.resolved && r.outcome_up.is_some())
            .collect();
        
        if resolved.is_empty() {
            return BacktestMetrics::default();
        }
        
        // Calculate accuracy for old vs new model
        let mut old_correct = 0;
        let mut rnjd_correct = 0;
        let mut old_brier = 0.0;
        let mut rnjd_brier = 0.0;
        
        for r in &resolved {
            let outcome = if r.outcome_up.unwrap() { 1.0 } else { 0.0 };
            
            // Old model prediction
            let old_pred = if r.p_up_old > 0.5 { 1.0 } else { 0.0 };
            if (old_pred - outcome).abs() < 0.01 {
                old_correct += 1;
            }
            old_brier += (r.p_up_old - outcome).powi(2);
            
            // RN-JD prediction
            let rnjd_pred = if r.p_up_rnjd > 0.5 { 1.0 } else { 0.0 };
            if (rnjd_pred - outcome).abs() < 0.01 {
                rnjd_correct += 1;
            }
            rnjd_brier += (r.p_up_rnjd - outcome).powi(2);
        }
        
        let n = resolved.len() as f64;
        
        BacktestMetrics {
            total_resolved: resolved.len(),
            old_accuracy: old_correct as f64 / n,
            rnjd_accuracy: rnjd_correct as f64 / n,
            old_brier_score: old_brier / n,
            rnjd_brier_score: rnjd_brier / n,
        }
    }
}

#[derive(Debug, Default, Serialize)]
pub struct BacktestMetrics {
    pub total_resolved: usize,
    pub old_accuracy: f64,
    pub rnjd_accuracy: f64,
    pub old_brier_score: f64,
    pub rnjd_brier_score: f64,
}
```
```

## Prompt 5.4: Add backtest API endpoints

```
In `rust-backend/src/api/simple.rs`, add endpoints:

```rust
/// Get backtest statistics
pub async fn get_backtest_stats(
    AxumState(state): AxumState<AppState>,
) -> Json<BacktestMetrics> {
    let collector = state.backtest_collector.read();
    Json(collector.calculate_metrics())
}

/// Export backtest records
pub async fn get_backtest_records(
    Query(params): Query<BacktestQuery>,
    AxumState(state): AxumState<AppState>,
) -> Json<Vec<BacktestRecord>> {
    let collector = state.backtest_collector.read();
    let records = collector.export();
    
    let limit = params.limit.unwrap_or(100).min(1000);
    Json(records.iter().rev().take(limit).cloned().collect())
}

#[derive(Deserialize)]
pub struct BacktestQuery {
    pub limit: Option<usize>,
}
```

Add routes:
```rust
.route("/api/backtest/stats", get(get_backtest_stats))
.route("/api/backtest/records", get(get_backtest_records))
```
```

## Prompt 5.5: Create A/B test framework

```
Create `rust-backend/src/vault/ab_test.rs`:

```rust
//! A/B testing framework for RN-JD vs legacy model

use rand::Rng;
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ModelVariant {
    Legacy,
    RnjdEnhanced,
}

#[derive(Debug)]
pub struct ABTestConfig {
    /// Probability of using RN-JD (0.5 = 50/50 split)
    pub rnjd_probability: f64,
    /// Whether A/B test is enabled
    pub enabled: bool,
}

impl Default for ABTestConfig {
    fn default() -> Self {
        Self {
            rnjd_probability: 0.5,
            enabled: false,
        }
    }
}

#[derive(Debug, Default)]
pub struct ABTestTracker {
    config: ABTestConfig,
    /// market_slug -> assigned variant
    assignments: HashMap<String, ModelVariant>,
    /// Stats per variant
    legacy_trades: usize,
    legacy_pnl: f64,
    rnjd_trades: usize,
    rnjd_pnl: f64,
}

impl ABTestTracker {
    pub fn new(config: ABTestConfig) -> Self {
        Self {
            config,
            ..Default::default()
        }
    }
    
    /// Get variant assignment for a market
    pub fn get_variant(&mut self, market_slug: &str) -> ModelVariant {
        if !self.config.enabled {
            return ModelVariant::RnjdEnhanced;  // Default to new
        }
        
        *self.assignments.entry(market_slug.to_string()).or_insert_with(|| {
            if rand::thread_rng().gen::<f64>() < self.config.rnjd_probability {
                ModelVariant::RnjdEnhanced
            } else {
                ModelVariant::Legacy
            }
        })
    }
    
    /// Record trade result
    pub fn record_result(&mut self, variant: ModelVariant, pnl: f64) {
        match variant {
            ModelVariant::Legacy => {
                self.legacy_trades += 1;
                self.legacy_pnl += pnl;
            }
            ModelVariant::RnjdEnhanced => {
                self.rnjd_trades += 1;
                self.rnjd_pnl += pnl;
            }
        }
    }
    
    /// Get A/B test summary
    pub fn summary(&self) -> ABTestSummary {
        ABTestSummary {
            enabled: self.config.enabled,
            legacy_trades: self.legacy_trades,
            legacy_pnl: self.legacy_pnl,
            legacy_avg_pnl: if self.legacy_trades > 0 {
                self.legacy_pnl / self.legacy_trades as f64
            } else { 0.0 },
            rnjd_trades: self.rnjd_trades,
            rnjd_pnl: self.rnjd_pnl,
            rnjd_avg_pnl: if self.rnjd_trades > 0 {
                self.rnjd_pnl / self.rnjd_trades as f64
            } else { 0.0 },
        }
    }
}

#[derive(Debug, Serialize)]
pub struct ABTestSummary {
    pub enabled: bool,
    pub legacy_trades: usize,
    pub legacy_pnl: f64,
    pub legacy_avg_pnl: f64,
    pub rnjd_trades: usize,
    pub rnjd_pnl: f64,
    pub rnjd_avg_pnl: f64,
}
```
```

## Prompt 5.6: Add environment config for A/B testing

```
In `rust-backend/src/vault/engine.rs`, add to VaultEngineConfig:

```rust
// A/B test config
pub ab_test_enabled: bool,
pub ab_test_rnjd_probability: f64,
```

In the config loading (VaultEngineConfig::from_env or similar):
```rust
ab_test_enabled: env::var("AB_TEST_ENABLED")
    .map(|s| s == "true")
    .unwrap_or(false),
ab_test_rnjd_probability: env::var("AB_TEST_RNJD_PROB")
    .ok()
    .and_then(|s| s.parse().ok())
    .unwrap_or(0.5),
```

Document in `.env.example`:
```bash
# A/B Testing
AB_TEST_ENABLED=false
AB_TEST_RNJD_PROB=0.5
```
```

## Prompt 5.7: Final integration and cleanup

```
Review and complete the integration:

1. Ensure all new modules are properly exported in mod.rs files
2. Add comprehensive docstrings to public functions
3. Run `cargo clippy` and fix all warnings
4. Run `cargo test` and ensure all tests pass
5. Update AGENTS.md with new components:
   - belief_vol module
   - rnjd module  
   - backtest_rnjd module
   - ab_test module
6. Add env vars to the documentation

Create a checklist in comments at the top of engine.rs:
```rust
// RN-JD Integration Checklist:
// [x] BeliefVolTracker initialized in AppState
// [x] Observations recorded in polling loop
// [x] estimate_p_up_enhanced called instead of old function
// [x] Jump regime handling added
// [x] Vol-adjusted Kelly integrated
// [x] Backtest recording enabled
// [x] API endpoints added
```
```

---

# Troubleshooting Prompts

## Prompt T.1: Debug compilation errors

```
I'm getting compilation errors after implementing the RN-JD changes. 
Please help me fix them:

[paste error output here]

The relevant files are:
- rust-backend/src/vault/belief_vol.rs
- rust-backend/src/vault/rnjd.rs
- rust-backend/src/vault/engine.rs

Common issues to check:
1. Missing imports (use statements)
2. Lifetime issues with references
3. Missing trait implementations (Clone, Debug, Serialize)
4. Type mismatches between f64 and Option<f64>
```

## Prompt T.2: Debug runtime panics

```
The RN-JD code is panicking at runtime. Here's the error:

[paste panic output]

Please help diagnose:
1. Check for division by zero in logit/sigmoid
2. Check for invalid inputs (NaN, infinity)
3. Check for unwrap() on None values
4. Add defensive clamping where needed
```

## Prompt T.3: Performance issues

```
The FAST15M engine is running slower after adding RN-JD. 
Profile and optimize:

1. Check if BeliefVolTracker lock contention is high
2. Consider using read-write lock more efficiently
3. Cache sigma_b lookups per loop iteration
4. Reduce allocations in hot path
```

---

# Alternative Approaches

## Alternative A: Simpler Implementation (If time constrained)

If the full implementation is too complex, implement only:

1. **Phase 0**: Basic utility functions (logit, sigmoid)
2. **Phase 2.2**: RN drift calculation only
3. **Phase 2.3**: Simple p_up adjustment

Skip: BeliefVolTracker, jump detection, vol-adjusted Kelly, backtest framework

This gives ~50% of the theoretical benefit with ~20% of the effort.

## Alternative B: External Computation

If Rust complexity is too high:

1. Implement belief vol estimation in Python
2. Expose via a sidecar service
3. Call from Rust via HTTP

Pros: Easier debugging, can use numpy/scipy
Cons: Latency, deployment complexity

## Alternative C: Configuration-Only Approach

If code changes are risky:

1. Add new config parameters for manual sigma_b input
2. Tune via environment variables
3. No automatic estimation, just manual calibration

Pros: Zero code risk
Cons: Requires manual tuning per market

---

# Success Criteria

After full implementation, verify:

- [ ] `cargo test` passes with >95% coverage on new code
- [ ] Belief vol estimates stabilize after ~50 observations
- [ ] Jump detection triggers on >3σ moves
- [ ] Vol-adjusted Kelly reduces position in high-vol regimes
- [ ] Backtest shows RN-JD Brier score ≤ legacy Brier score
- [ ] No latency regression (engine cycle time <100ms)
- [ ] A/B test framework correctly assigns variants

---

# Appendix: Mathematical Reference

## Logit Transform
```
x = logit(p) = ln(p / (1-p))
p = sigmoid(x) = 1 / (1 + e^(-x))
```

## Sigmoid Derivatives
```
S'(x) = p(1-p)
S''(x) = p(1-p)(1-2p)
```

## RN Drift (Pure Diffusion)
```
μ(t, x) = -½ σ_b² (1 - 2p)
```

## Martingale Condition
```
E^Q[dp_t] = 0
⟹ drift is fully determined by σ_b and jump parameters
```

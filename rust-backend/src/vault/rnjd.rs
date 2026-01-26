//! Risk-Neutral Jump-Diffusion (RN-JD) pricing for prediction markets
//!
//! Based on: "Toward Black-Scholes for Prediction Markets" (arXiv:2510.15205)
//! Shaw Dalen, Daedalus Research Team, October 2025
//!
//! Key insight: The traded probability p_t must be a Q-martingale,
//! which pins down the drift via Itô-Lévy calculus.
//!
//! This module provides:
//! - Risk-neutral drift calculation for probability dynamics
//! - Enhanced p_up estimation with belief volatility correction
//! - Jump regime detection and handling
//! - Price-to-belief volatility conversion

use serde::{Deserialize, Serialize};
use statrs::distribution::{ContinuousCDF, Normal};

use crate::vault::belief_vol::{logit, sigmoid, sigmoid_derivative, BeliefVolTracker};

// ============================================================================
// Core Types
// ============================================================================

/// Parameters for RN-JD model
#[derive(Debug, Clone, Serialize, Deserialize)]
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
            sigma_b: 2.0, // Moderate belief vol
            lambda: 0.0,  // No jumps by default (pure diffusion)
            mu_j: 0.0,
            sigma_j: 0.1,
        }
    }
}

/// Result of RN-JD probability estimation
#[derive(Debug, Clone, Serialize, Deserialize)]
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

// ============================================================================
// Risk-Neutral Drift Calculation
// ============================================================================

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
#[allow(dead_code)]
pub fn expected_dp(p: f64, dt_years: f64, params: &RnjdParams) -> f64 {
    // For a martingale, E[dp] = 0 under Q
    // But we track the drift in x, then transform
    let x = logit(p);
    let mu_x = rn_drift(p, params);

    // Approximate: dp ≈ S'(x) * dx = p(1-p) * μ_x * dt
    sigmoid_derivative(x) * mu_x * dt_years
}

// ============================================================================
// p_up Estimation with RN-JD Correction
// ============================================================================

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
    let time_factor = (-t_rem_secs / 900.0).exp(); // Decays over 15 min
    let extremity_penalty = 4.0 * market_p * (1.0 - market_p); // Max at 0.5
    let confidence = (time_factor * extremity_penalty).clamp(0.1, 0.95);

    Some(RnjdEstimate {
        p_up,
        drift_correction,
        p_up_raw,
        confidence,
        jump_regime: false,
    })
}

// ============================================================================
// Price Volatility to Belief Volatility Conversion
// ============================================================================

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
    let p = market_p.clamp(0.05, 0.95); // Avoid extreme values
    let sensitivity = p * (1.0 - p);

    if sensitivity < 0.01 {
        return sigma_price * 10.0; // Cap at 10x when near boundaries
    }

    (sigma_price / sensitivity).min(50.0) // Cap at 50 annualized
}

/// Blend our sigma_b estimate with price-derived estimate
pub fn blend_sigma_b(
    tracked_sigma_b: Option<f64>,
    price_sigma_b: f64,
    blend_weight: f64, // Weight on tracked (0-1)
) -> f64 {
    match tracked_sigma_b {
        Some(tracked) => blend_weight * tracked + (1.0 - blend_weight) * price_sigma_b,
        None => price_sigma_b,
    }
}

// ============================================================================
// Jump Regime Detection
// ============================================================================

/// Detect if we're in a high-jump regime (news absorption period)
#[derive(Debug, Clone, Serialize, Deserialize)]
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
            window_secs: 300, // 5 minutes
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
            0.5 // Require 2x edge to trade during jump regime
        } else {
            1.0
        }
    }
}

// ============================================================================
// Unified Enhanced p_up Estimation
// ============================================================================

/// Enhanced p_up estimation combining all components
///
/// This is the main function to call from the FAST15M engine.
///
/// # Arguments
/// * `p_start` - Price at market start
/// * `p_now` - Current underlying price
/// * `market_p` - Current market probability (mid price)
/// * `sigma_price` - Realized price volatility (annualized)
/// * `t_rem_secs` - Time remaining to expiry in seconds
/// * `belief_tracker` - Optional tracker for market-specific sigma_b
/// * `market_slug` - Market identifier for tracker lookup
/// * `now_ts` - Current unix timestamp
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
            blend_sigma_b(
                Some(tracker.get_sigma_b(market_slug)),
                price_derived_sigma_b,
                0.7,
            )
        }
        _ => price_derived_sigma_b,
    };

    // Step 2: Check for jump regime
    let jump_regime = match belief_tracker {
        Some(tracker) => {
            let count = tracker.count_recent_jumps(market_slug, 300, now_ts, 3.0);
            count >= 2
        }
        None => false,
    };

    // Step 3: Build params and estimate
    let params = RnjdParams {
        sigma_b,
        lambda: if jump_regime { 10.0 } else { 0.0 }, // Elevated during jump regime
        ..Default::default()
    };

    let mut estimate =
        estimate_p_up_rnjd(p_start, p_now, market_p, sigma_price, t_rem_secs, &params)?;

    estimate.jump_regime = jump_regime;

    // Reduce confidence if in jump regime
    if jump_regime {
        estimate.confidence *= 0.7;
    }

    Some(estimate)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ========================================================================
    // RN Drift Tests
    // ========================================================================

    #[test]
    fn test_rn_drift_at_half() {
        let params = RnjdParams::default();
        // At p = 0.5, (1 - 2p) = 0, so drift should be 0
        let drift = rn_drift(0.5, &params);
        assert!(
            drift.abs() < 0.001,
            "Drift at p=0.5 should be ~0, got {}",
            drift
        );
    }

    #[test]
    fn test_rn_drift_asymmetry() {
        let params = RnjdParams {
            sigma_b: 1.0,
            ..Default::default()
        };

        // Under Q, p should be a martingale; this requires a drift in log-odds x.
        // For pure diffusion, μ_x = -½ σ_b² (1 - 2p), which is negative for p < 0.5
        // and positive for p > 0.5.
        let drift_low = rn_drift(0.3, &params);
        // At p > 0.5, drift should be positive
        let drift_high = rn_drift(0.7, &params);

        assert!(drift_low < 0.0, "Drift at p=0.3 should be negative");
        assert!(drift_high > 0.0, "Drift at p=0.7 should be positive");
        // Should be symmetric
        assert!(
            (drift_low + drift_high).abs() < 0.001,
            "Drift should be symmetric"
        );
    }

    #[test]
    fn test_rn_drift_scales_with_sigma() {
        let params_low = RnjdParams {
            sigma_b: 1.0,
            ..Default::default()
        };
        let params_high = RnjdParams {
            sigma_b: 2.0,
            ..Default::default()
        };

        let drift_low = rn_drift(0.3, &params_low).abs();
        let drift_high = rn_drift(0.3, &params_high).abs();

        // Drift scales with sigma_b^2
        let expected_ratio = 4.0; // (2/1)^2
        let actual_ratio = drift_high / drift_low;
        assert!(
            (actual_ratio - expected_ratio).abs() < 0.1,
            "Drift should scale with sigma_b^2: expected {}, got {}",
            expected_ratio,
            actual_ratio
        );
    }

    // ========================================================================
    // p_up Estimation Tests
    // ========================================================================

    #[test]
    fn test_estimate_p_up_rnjd_basic() {
        let params = RnjdParams::default();

        // Price unchanged, should be near 0.5
        let est = estimate_p_up_rnjd(100.0, 100.0, 0.5, 0.20, 600.0, &params);
        assert!(est.is_some());
        let e = est.unwrap();
        assert!(
            (e.p_up - 0.5).abs() < 0.1,
            "Unchanged price should give p_up near 0.5, got {}",
            e.p_up
        );
    }

    #[test]
    fn test_estimate_p_up_rnjd_price_up() {
        let params = RnjdParams::default();

        // Price up 1%
        let est = estimate_p_up_rnjd(100.0, 101.0, 0.5, 0.20, 600.0, &params);
        assert!(est.is_some());
        let e = est.unwrap();
        assert!(
            e.p_up > 0.5,
            "Price up should give p_up > 0.5, got {}",
            e.p_up
        );
    }

    #[test]
    fn test_estimate_p_up_rnjd_price_down() {
        let params = RnjdParams::default();

        // Price down 1%
        let est = estimate_p_up_rnjd(100.0, 99.0, 0.5, 0.20, 600.0, &params);
        assert!(est.is_some());
        let e = est.unwrap();
        assert!(
            e.p_up < 0.5,
            "Price down should give p_up < 0.5, got {}",
            e.p_up
        );
    }

    #[test]
    fn test_estimate_p_up_rnjd_invalid_inputs() {
        let params = RnjdParams::default();

        // Invalid p_start
        assert!(estimate_p_up_rnjd(0.0, 100.0, 0.5, 0.20, 600.0, &params).is_none());
        assert!(estimate_p_up_rnjd(-1.0, 100.0, 0.5, 0.20, 600.0, &params).is_none());

        // Invalid sigma
        assert!(estimate_p_up_rnjd(100.0, 100.0, 0.5, 0.0, 600.0, &params).is_none());
        assert!(estimate_p_up_rnjd(100.0, 100.0, 0.5, -0.1, 600.0, &params).is_none());

        // Invalid time
        assert!(estimate_p_up_rnjd(100.0, 100.0, 0.5, 0.20, 0.0, &params).is_none());
        assert!(estimate_p_up_rnjd(100.0, 100.0, 0.5, 0.20, -1.0, &params).is_none());
    }

    #[test]
    fn test_estimate_p_up_rnjd_drift_correction() {
        // With extreme market_p, drift correction should be noticeable
        let params = RnjdParams {
            sigma_b: 3.0, // High sigma_b for visible effect
            ..Default::default()
        };

        let est = estimate_p_up_rnjd(100.0, 100.0, 0.2, 0.20, 600.0, &params);
        assert!(est.is_some());
        let e = est.unwrap();

        // Drift correction should be non-zero for extreme market_p
        // At p=0.2, (1-2p) = 0.6, so positive drift
        assert!(
            e.drift_correction.abs() > 0.0,
            "Should have drift correction"
        );
    }

    // ========================================================================
    // Price to Belief Vol Conversion Tests
    // ========================================================================

    #[test]
    fn test_price_to_belief_vol_at_half() {
        // At p=0.5, sensitivity = 0.25, so sigma_b = 4 * sigma_price
        let sigma_b = price_vol_to_belief_vol(0.20, 0.5);
        assert!(
            (sigma_b - 0.80).abs() < 0.01,
            "At p=0.5: expected 0.80, got {}",
            sigma_b
        );
    }

    #[test]
    fn test_price_to_belief_vol_extreme() {
        // At p=0.1, sensitivity = 0.09, sigma_b is higher
        let sigma_b_mid = price_vol_to_belief_vol(0.20, 0.5);
        let sigma_b_extreme = price_vol_to_belief_vol(0.20, 0.1);
        assert!(
            sigma_b_extreme > sigma_b_mid,
            "Extreme p should have higher sigma_b"
        );
    }

    #[test]
    fn test_price_to_belief_vol_capped() {
        // Very extreme p should be capped
        let sigma_b = price_vol_to_belief_vol(10.0, 0.5);
        assert!(sigma_b <= 50.0, "sigma_b should be capped at 50");
    }

    #[test]
    fn test_blend_sigma_b() {
        let blended = blend_sigma_b(Some(2.0), 4.0, 0.7);
        let expected = 0.7 * 2.0 + 0.3 * 4.0; // 1.4 + 1.2 = 2.6
        assert!(
            (blended - expected).abs() < 0.01,
            "Expected {}, got {}",
            expected,
            blended
        );

        // No tracked value
        let blended_none = blend_sigma_b(None, 4.0, 0.7);
        assert!((blended_none - 4.0).abs() < 0.01);
    }

    // ========================================================================
    // Jump Regime Detector Tests
    // ========================================================================

    #[test]
    fn test_jump_regime_detector_default() {
        let detector = JumpRegimeDetector::default();
        assert_eq!(detector.jump_count_threshold, 2);
        assert_eq!(detector.window_secs, 300);
    }

    #[test]
    fn test_jump_regime_high_count() {
        let detector = JumpRegimeDetector::default();

        // Below threshold
        assert!(!detector.is_jump_regime(1, None, 1000));

        // At threshold
        assert!(detector.is_jump_regime(2, None, 1000));

        // Above threshold
        assert!(detector.is_jump_regime(5, None, 1000));
    }

    #[test]
    fn test_jump_regime_cooldown() {
        let detector = JumpRegimeDetector {
            cooldown_secs: 60,
            ..Default::default()
        };

        // Within cooldown
        assert!(detector.is_jump_regime(0, Some(950), 1000));

        // After cooldown
        assert!(!detector.is_jump_regime(0, Some(900), 1000));
    }

    #[test]
    fn test_jump_regime_edge_multiplier() {
        let detector = JumpRegimeDetector::default();

        assert!((detector.edge_multiplier(false) - 1.0).abs() < 0.01);
        assert!((detector.edge_multiplier(true) - 0.5).abs() < 0.01);
    }

    // ========================================================================
    // Enhanced Estimation Tests
    // ========================================================================

    #[test]
    fn test_estimate_p_up_enhanced_no_tracker() {
        let est = estimate_p_up_enhanced(
            100.0,
            101.0,
            0.5,
            0.20,
            600.0,
            None,
            "btc-updown-15m",
            1700000000,
        );
        assert!(est.is_some());
        let e = est.unwrap();
        assert!(e.p_up > 0.5);
        assert!(!e.jump_regime);
    }

    #[test]
    fn test_estimate_p_up_enhanced_with_tracker() {
        use crate::vault::belief_vol::{BeliefVolConfig, BeliefVolTracker};

        let config = BeliefVolConfig {
            min_samples: 5,
            ..Default::default()
        };
        let mut tracker = BeliefVolTracker::new(config);

        // Build up some history
        let mut ts = 1700000000i64;
        for i in 0..20 {
            let p = 0.5 + 0.05 * ((i as f64 * 0.5).sin());
            tracker.record_observation("btc-updown-15m", p, ts);
            ts += 10;
        }

        let est = estimate_p_up_enhanced(
            100.0,
            101.0,
            0.5,
            0.20,
            600.0,
            Some(&tracker),
            "btc-updown-15m",
            ts,
        );
        assert!(est.is_some());
        let e = est.unwrap();
        assert!(e.p_up > 0.5);
    }

    #[test]
    fn test_estimate_confidence_decay() {
        let params = RnjdParams::default();

        // Short time remaining -> high confidence
        let est_short = estimate_p_up_rnjd(100.0, 100.0, 0.5, 0.20, 60.0, &params).unwrap();

        // Long time remaining -> lower confidence
        let est_long = estimate_p_up_rnjd(100.0, 100.0, 0.5, 0.20, 900.0, &params).unwrap();

        assert!(
            est_short.confidence > est_long.confidence,
            "Shorter time should have higher confidence: {} vs {}",
            est_short.confidence,
            est_long.confidence
        );
    }
}

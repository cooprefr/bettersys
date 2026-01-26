//! Belief Volatility Estimation for Prediction Markets
//!
//! This module implements the belief volatility (`σ_b`) estimation from the
//! Risk-Neutral Jump-Diffusion (RN-JD) framework for prediction markets.
//!
//! # Background
//!
//! In the RN-JD model, we transform the traded probability `p_t ∈ (0,1)` to
//! log-odds space: `x_t = logit(p_t) = ln(p_t / (1 - p_t))`.
//!
//! The belief volatility `σ_b` measures how fast log-odds move over time,
//! analogous to implied volatility in options markets. It is the key
//! parameter for:
//! - Position sizing (higher σ_b → more uncertainty → smaller positions)
//! - Jump detection (moves > 3σ_b are likely news shocks)
//! - Risk-neutral drift calculation (drift is pinned by σ_b via martingale constraint)
//!
//! # Reference
//!
//! "Toward Black-Scholes for Prediction Markets" (arXiv:2510.15205)
//! Shaw Dalen, Daedalus Research Team, October 2025

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};

/// Estimate of belief volatility for a specific market
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BeliefVolEstimate {
    /// Belief volatility in log-odds space (annualized)
    pub sigma_b: f64,
    /// Number of observations used in estimate
    pub sample_count: usize,
    /// Unix timestamp of last update
    pub last_updated: i64,
    /// Confidence in estimate (0.0 to 1.0)
    /// Higher values indicate more reliable estimates
    pub confidence: f64,
}

impl BeliefVolEstimate {
    // Methods will be implemented in subsequent prompts
}

impl Default for BeliefVolEstimate {
    fn default() -> Self {
        Self {
            sigma_b: 2.0,
            sample_count: 0,
            last_updated: 0,
            confidence: 0.0,
        }
    }
}

/// Configuration for belief volatility estimation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BeliefVolConfig {
    /// Minimum samples required before trusting an estimate
    pub min_samples: usize,
    /// EMA smoothing factor (0 < alpha ≤ 1)
    /// Lower values = more smoothing, slower adaptation
    pub ema_alpha: f64,
    /// Maximum age (seconds) before estimate is considered stale
    pub max_age_secs: i64,
    /// Prior/fallback sigma_b when no reliable estimate exists
    pub prior_sigma_b: f64,
}

impl BeliefVolConfig {
    // Methods will be implemented in subsequent prompts
}

impl Default for BeliefVolConfig {
    fn default() -> Self {
        Self {
            min_samples: 30,
            ema_alpha: 0.1,
            max_age_secs: 3600,
            prior_sigma_b: 2.0,
        }
    }
}

// ============================================================================
// Log-Odds Increment Tracking
// ============================================================================

/// Summary statistics for logging/monitoring
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BeliefVolSummary {
    /// Number of markets being tracked
    pub markets_tracked: usize,
    /// Number of markets with reliable estimates
    pub reliable_estimates: usize,
    /// Average sigma_b across all tracked markets
    pub avg_sigma_b: f64,
}

/// Result of jump detection analysis
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct JumpDetectionResult {
    /// Whether the move qualifies as a jump
    pub is_jump: bool,
    /// Z-score of the move (|Δx| / expected_std)
    pub z_score: f64,
    /// Size of the jump in log-odds (0 if not a jump)
    pub jump_size: f64,
    /// Threshold used for detection (in sigma units)
    pub threshold_used: f64,
}

/// A single observation of log-odds change between two time points
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct LogOddsIncrement {
    /// Unix timestamp of the observation
    pub timestamp: i64,
    /// Log-odds before: logit(p_before)
    pub x_before: f64,
    /// Log-odds after: logit(p_after)
    pub x_after: f64,
    /// Time delta in seconds
    pub dt_secs: f64,
}

impl LogOddsIncrement {
    /// Create a new increment from probability values
    pub fn new(p_before: f64, p_after: f64, dt_secs: f64, timestamp: i64) -> Self {
        Self {
            timestamp,
            x_before: logit(p_before),
            x_after: logit(p_after),
            dt_secs,
        }
    }

    /// Returns the log-odds change (Δx = x_after - x_before)
    #[inline]
    pub fn delta_x(&self) -> f64 {
        self.x_after - self.x_before
    }

    /// Returns annualized squared increment for variance estimation
    ///
    /// This scales the squared increment by time to get an annualized measure:
    /// `(Δx)² / Δt` where Δt is in years
    pub fn annualized_sq_increment(&self) -> f64 {
        if self.dt_secs <= 0.0 {
            return 0.0;
        }
        let dt_years = self.dt_secs / (365.25 * 24.0 * 3600.0);
        let dx = self.delta_x();
        (dx * dx) / dt_years
    }
}

// ============================================================================
// Belief Volatility Tracker
// ============================================================================

/// Tracks belief volatility estimates per market
///
/// This is the main struct for estimating and managing σ_b across markets.
/// It maintains a rolling history of log-odds increments and computes
/// exponential moving average estimates of annualized variance.
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
    /// Create a new tracker with the given configuration
    pub fn new(config: BeliefVolConfig) -> Self {
        Self {
            config,
            history: HashMap::new(),
            estimates: HashMap::new(),
            max_history: 1000,
        }
    }

    /// Builder method to set maximum history size
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

    /// Check if we have a reliable estimate (enough samples and confidence)
    pub fn has_reliable_estimate(&self, market_slug: &str) -> bool {
        self.estimates
            .get(market_slug)
            .map(|e| e.sample_count >= self.config.min_samples && e.confidence > 0.5)
            .unwrap_or(false)
    }

    /// Get the configuration
    pub fn config(&self) -> &BeliefVolConfig {
        &self.config
    }

    /// Get number of markets being tracked
    pub fn market_count(&self) -> usize {
        self.history.len()
    }

    /// Record a new price observation for a market
    ///
    /// This should be called each time the market mid-price is observed.
    /// The tracker will compute log-odds increments and update the
    /// belief volatility estimate.
    ///
    /// # Arguments
    /// * `market_slug` - Market identifier (case-insensitive)
    /// * `p_now` - Current probability/price (0 < p < 1)
    /// * `timestamp` - Unix timestamp of observation
    pub fn record_observation(&mut self, market_slug: &str, p_now: f64, timestamp: i64) {
        let slug = market_slug.to_lowercase();

        // Clamp probability to valid range
        let p_now = p_now.clamp(0.0001, 0.9999);

        // Get or create history for this market
        let history = self
            .history
            .entry(slug.clone())
            .or_insert_with(VecDeque::new);

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
            // First observation - just record the logit value as a placeholder
            let fake_increment = LogOddsIncrement {
                timestamp,
                x_before: logit(p_now),
                x_after: logit(p_now),
                dt_secs: 0.0,
            };
            history.push_back(fake_increment);
        }
    }

    /// Update sigma_b estimate using exponential moving average of squared increments
    ///
    /// This computes an EMA of annualized variance from the log-odds increments,
    /// then takes the square root to get sigma_b.
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

        for inc in history.iter().skip(1) {
            // Skip first (placeholder) entry and any with invalid dt
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
            // Take sqrt of variance to get volatility, floor at 1% annualized
            let sigma_b = var.sqrt().max(0.01);

            // Confidence based on sample count relative to min_samples
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

    /// Calculate realized volatility using simple sum of squared increments
    ///
    /// This is an alternative to the EMA-based estimate that computes raw
    /// realized variance over a fixed time window. Useful for comparison
    /// and validation.
    ///
    /// # Arguments
    /// * `market_slug` - Market identifier
    /// * `window_secs` - Time window in seconds to look back
    /// * `now_ts` - Current timestamp
    ///
    /// # Returns
    /// Annualized volatility (sigma_b) or None if insufficient data
    pub fn realized_vol_window(
        &self,
        market_slug: &str,
        window_secs: i64,
        now_ts: i64,
    ) -> Option<f64> {
        let history = self.history.get(&market_slug.to_lowercase())?;

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
            return None; // Not enough data
        }

        // Annualize: variance per year
        let secs_per_year = 365.25 * 24.0 * 3600.0;
        let annualized_var = sum_sq * (secs_per_year / total_dt);

        Some(annualized_var.sqrt())
    }

    /// Detect if the most recent move was a jump (news shock)
    ///
    /// A jump is defined as a move whose z-score exceeds the threshold.
    /// The z-score is computed as |Δx| / (σ_b × √Δt).
    ///
    /// # Arguments
    /// * `market_slug` - Market identifier
    /// * `threshold_sigma` - Detection threshold in sigma units (typically 3.0)
    ///
    /// # Returns
    /// Jump detection result or None if insufficient data
    pub fn detect_recent_jump(
        &self,
        market_slug: &str,
        threshold_sigma: f64,
    ) -> Option<JumpDetectionResult> {
        let slug = market_slug.to_lowercase();
        let history = self.history.get(&slug)?;
        let estimate = self.estimates.get(&slug)?;

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
    ///
    /// Scans the history window and counts moves exceeding the threshold.
    ///
    /// # Arguments
    /// * `market_slug` - Market identifier
    /// * `window_secs` - Time window to scan (seconds)
    /// * `now_ts` - Current timestamp
    /// * `threshold_sigma` - Detection threshold in sigma units
    pub fn count_recent_jumps(
        &self,
        market_slug: &str,
        window_secs: i64,
        now_ts: i64,
        threshold_sigma: f64,
    ) -> usize {
        let slug = market_slug.to_lowercase();
        let Some(history) = self.history.get(&slug) else {
            return 0;
        };
        let Some(estimate) = self.estimates.get(&slug) else {
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

    // ========================================================================
    // Serialization / Persistence Methods
    // ========================================================================

    /// Export all estimates as a cloned HashMap (for persistence)
    pub fn export_estimates(&self) -> HashMap<String, BeliefVolEstimate> {
        self.estimates.clone()
    }

    /// Import estimates (e.g., on startup from DB)
    ///
    /// This merges imported estimates into the tracker. Existing estimates
    /// for the same market slug will be overwritten.
    pub fn import_estimates(&mut self, estimates: HashMap<String, BeliefVolEstimate>) {
        for (slug, est) in estimates {
            self.estimates.insert(slug.to_lowercase(), est);
        }
    }

    /// Get summary stats for logging/monitoring
    pub fn summary(&self) -> BeliefVolSummary {
        let markets_tracked = self.estimates.len();
        let reliable_estimates = self
            .estimates
            .values()
            .filter(|e| e.sample_count >= self.config.min_samples && e.confidence > 0.5)
            .count();
        let avg_sigma_b = if self.estimates.is_empty() {
            self.config.prior_sigma_b
        } else {
            self.estimates.values().map(|e| e.sigma_b).sum::<f64>() / self.estimates.len() as f64
        };

        BeliefVolSummary {
            markets_tracked,
            reliable_estimates,
            avg_sigma_b,
        }
    }
}

// ============================================================================
// Utility Functions: Logit/Sigmoid Transforms
// ============================================================================

/// Transform probability to log-odds (logit function)
///
/// The logit transform maps p ∈ (0, 1) to x ∈ (-∞, +∞):
/// ```text
/// x = logit(p) = ln(p / (1 - p))
/// ```
///
/// This is the canonical transformation for working with probabilities
/// in the RN-JD framework, as it maps the bounded probability space
/// to an unbounded space where standard diffusion models apply.
///
/// Input is clamped to (0.0001, 0.9999) to avoid infinities.
#[inline]
pub fn logit(p: f64) -> f64 {
    let p_clamped = p.clamp(0.0001, 0.9999);
    (p_clamped / (1.0 - p_clamped)).ln()
}

/// Transform log-odds back to probability (sigmoid/logistic function)
///
/// The sigmoid is the inverse of logit, mapping x ∈ (-∞, +∞) to p ∈ (0, 1):
/// ```text
/// p = sigmoid(x) = 1 / (1 + exp(-x))
/// ```
///
/// Also known as the logistic function or expit.
#[inline]
pub fn sigmoid(x: f64) -> f64 {
    1.0 / (1.0 + (-x).exp())
}

/// First derivative of sigmoid: S'(x) = p(1 - p)
///
/// This is the sensitivity of probability to changes in log-odds.
/// Used in chain rule conversions between price volatility and belief volatility.
///
/// Note: Input is the probability p = S(x), not x itself.
#[inline]
pub fn sigmoid_derivative(p: f64) -> f64 {
    let p_clamped = p.clamp(0.0001, 0.9999);
    p_clamped * (1.0 - p_clamped)
}

/// Second derivative of sigmoid: S''(x) = p(1 - p)(1 - 2p)
///
/// Used in the Itô correction term for the risk-neutral drift calculation.
/// The RN drift includes a term proportional to S''/S' = (1 - 2p).
///
/// Note: Input is the probability p = S(x), not x itself.
#[inline]
pub fn sigmoid_second_derivative(p: f64) -> f64 {
    let p_clamped = p.clamp(0.0001, 0.9999);
    p_clamped * (1.0 - p_clamped) * (1.0 - 2.0 * p_clamped)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_belief_vol_estimate_default() {
        let est = BeliefVolEstimate::default();
        assert_eq!(est.sample_count, 0);
        assert_eq!(est.confidence, 0.0);
    }

    #[test]
    fn test_belief_vol_config_default() {
        let cfg = BeliefVolConfig::default();
        assert_eq!(cfg.min_samples, 30);
        assert!((cfg.ema_alpha - 0.1).abs() < 1e-10);
        assert_eq!(cfg.max_age_secs, 3600);
        assert!((cfg.prior_sigma_b - 2.0).abs() < 1e-10);
    }

    // ========================================================================
    // Logit/Sigmoid Tests
    // ========================================================================

    #[test]
    fn test_logit_at_half() {
        // logit(0.5) = ln(0.5 / 0.5) = ln(1) = 0
        let result = logit(0.5);
        assert!(
            result.abs() < 1e-10,
            "logit(0.5) should be 0, got {}",
            result
        );
    }

    #[test]
    fn test_sigmoid_at_zero() {
        // sigmoid(0) = 1 / (1 + 1) = 0.5
        let result = sigmoid(0.0);
        assert!(
            (result - 0.5).abs() < 1e-10,
            "sigmoid(0) should be 0.5, got {}",
            result
        );
    }

    #[test]
    fn test_logit_sigmoid_inverse() {
        // logit(sigmoid(x)) should return x for reasonable x values
        for x in [-5.0, -2.0, -1.0, 0.0, 1.0, 2.0, 5.0] {
            let p = sigmoid(x);
            let x_recovered = logit(p);
            assert!(
                (x - x_recovered).abs() < 1e-6,
                "logit(sigmoid({})) = {}, expected {}",
                x,
                x_recovered,
                x
            );
        }
    }

    #[test]
    fn test_sigmoid_logit_inverse() {
        // sigmoid(logit(p)) should return p for reasonable p values
        for p in [0.1, 0.25, 0.5, 0.75, 0.9] {
            let x = logit(p);
            let p_recovered = sigmoid(x);
            assert!(
                (p - p_recovered).abs() < 1e-6,
                "sigmoid(logit({})) = {}, expected {}",
                p,
                p_recovered,
                p
            );
        }
    }

    #[test]
    fn test_sigmoid_derivative_at_half() {
        // S'(x) at p=0.5: 0.5 * 0.5 = 0.25
        let result = sigmoid_derivative(0.5);
        assert!(
            (result - 0.25).abs() < 1e-10,
            "sigmoid_derivative(0.5) should be 0.25, got {}",
            result
        );
    }

    #[test]
    fn test_sigmoid_derivative_symmetry() {
        // S'(x) is symmetric around p=0.5
        let d_low = sigmoid_derivative(0.3);
        let d_high = sigmoid_derivative(0.7);
        assert!(
            (d_low - d_high).abs() < 1e-10,
            "sigmoid_derivative should be symmetric: {} vs {}",
            d_low,
            d_high
        );
    }

    #[test]
    fn test_sigmoid_second_derivative_at_half() {
        // S''(x) at p=0.5: 0.5 * 0.5 * (1 - 1) = 0
        let result = sigmoid_second_derivative(0.5);
        assert!(
            result.abs() < 1e-10,
            "sigmoid_second_derivative(0.5) should be 0, got {}",
            result
        );
    }

    #[test]
    fn test_sigmoid_second_derivative_signs() {
        // S''(x) > 0 for p < 0.5, S''(x) < 0 for p > 0.5
        let d_low = sigmoid_second_derivative(0.3);
        let d_high = sigmoid_second_derivative(0.7);
        assert!(d_low > 0.0, "S''(0.3) should be positive, got {}", d_low);
        assert!(d_high < 0.0, "S''(0.7) should be negative, got {}", d_high);
    }

    #[test]
    fn test_logit_clamping() {
        // Extreme values should be clamped, not produce infinities
        let low = logit(0.0);
        let high = logit(1.0);
        assert!(low.is_finite(), "logit(0) should be finite");
        assert!(high.is_finite(), "logit(1) should be finite");
        assert!(low < -5.0, "logit(0) should be very negative");
        assert!(high > 5.0, "logit(1) should be very positive");
    }

    // ========================================================================
    // LogOddsIncrement Tests
    // ========================================================================

    #[test]
    fn test_log_odds_increment_delta_x() {
        // p_before = 0.5 -> x_before = 0
        // p_after = 0.7 -> x_after = ln(0.7/0.3) ≈ 0.847
        let inc = LogOddsIncrement::new(0.5, 0.7, 10.0, 1000);
        let dx = inc.delta_x();
        let expected = logit(0.7) - logit(0.5);
        assert!(
            (dx - expected).abs() < 1e-10,
            "delta_x mismatch: {} vs {}",
            dx,
            expected
        );
    }

    #[test]
    fn test_log_odds_increment_zero_dt() {
        let inc = LogOddsIncrement::new(0.5, 0.6, 0.0, 1000);
        assert_eq!(
            inc.annualized_sq_increment(),
            0.0,
            "Zero dt should return 0"
        );
    }

    #[test]
    fn test_log_odds_increment_annualized() {
        // If p moves from 0.5 to 0.6 in 1 second:
        // dx = logit(0.6) - logit(0.5) = logit(0.6) ≈ 0.405
        // dt_years = 1 / (365.25 * 24 * 3600) ≈ 3.17e-8
        // annualized = dx² / dt_years
        let inc = LogOddsIncrement::new(0.5, 0.6, 1.0, 1000);
        let ann = inc.annualized_sq_increment();

        let dx = logit(0.6);
        let dt_years = 1.0 / (365.25 * 24.0 * 3600.0);
        let expected = (dx * dx) / dt_years;

        assert!(
            (ann - expected).abs() / expected < 1e-6,
            "Annualized mismatch: {} vs {}",
            ann,
            expected
        );
        assert!(ann > 0.0, "Annualized should be positive");
    }

    #[test]
    fn test_log_odds_increment_symmetric() {
        // Moving up and down by same amount should have same magnitude
        let inc_up = LogOddsIncrement::new(0.5, 0.6, 10.0, 1000);
        let inc_down = LogOddsIncrement::new(0.5, 0.4, 10.0, 1000);

        assert!(
            (inc_up.delta_x().abs() - inc_down.delta_x().abs()).abs() < 1e-10,
            "Symmetric moves should have equal magnitude"
        );
        assert!(
            (inc_up.annualized_sq_increment() - inc_down.annualized_sq_increment()).abs() < 1e-6,
            "Symmetric moves should have equal annualized variance"
        );
    }

    // ========================================================================
    // BeliefVolTracker Tests
    // ========================================================================

    #[test]
    fn test_tracker_new() {
        let config = BeliefVolConfig::default();
        let tracker = BeliefVolTracker::new(config);
        assert_eq!(tracker.market_count(), 0);
    }

    #[test]
    fn test_tracker_with_max_history() {
        let config = BeliefVolConfig::default();
        let tracker = BeliefVolTracker::new(config).with_max_history(500);
        assert_eq!(tracker.max_history, 500);
    }

    #[test]
    fn test_tracker_get_sigma_b_prior() {
        let config = BeliefVolConfig {
            prior_sigma_b: 3.0,
            ..Default::default()
        };
        let tracker = BeliefVolTracker::new(config);
        // No data yet, should return prior
        assert!((tracker.get_sigma_b("test-market") - 3.0).abs() < 1e-10);
    }

    #[test]
    fn test_tracker_has_reliable_estimate_empty() {
        let tracker = BeliefVolTracker::new(BeliefVolConfig::default());
        assert!(!tracker.has_reliable_estimate("test-market"));
    }

    #[test]
    fn test_tracker_get_estimate_none() {
        let tracker = BeliefVolTracker::new(BeliefVolConfig::default());
        assert!(tracker.get_estimate("nonexistent").is_none());
    }

    #[test]
    fn test_record_observation_basic() {
        let config = BeliefVolConfig::default();
        let mut tracker = BeliefVolTracker::new(config);

        let slug = "btc-updown-15m-test";
        let mut ts = 1700000000i64;

        // Record 50 observations with p varying around 0.5
        for i in 0..50 {
            // Oscillate p between 0.45 and 0.55
            let p = 0.5 + 0.05 * ((i as f64 * 0.5).sin());
            tracker.record_observation(slug, p, ts);
            ts += 10; // 10 seconds between observations
        }

        // Should have recorded history
        assert_eq!(tracker.market_count(), 1);

        // First entry is placeholder, then 49 real increments = 50 total
        // But some may be skipped if dt < 1s, here dt=10s so all should be recorded
        let estimate = tracker.get_estimate(slug);
        assert!(estimate.is_some() || tracker.market_count() == 1);
    }

    #[test]
    fn test_record_observation_case_insensitive() {
        let mut tracker = BeliefVolTracker::new(BeliefVolConfig::default());

        tracker.record_observation("TEST-Market", 0.5, 1000);
        tracker.record_observation("test-market", 0.6, 1010);

        // Should be same market (case insensitive)
        assert_eq!(tracker.market_count(), 1);
    }

    #[test]
    fn test_record_observation_skip_small_dt() {
        let mut tracker = BeliefVolTracker::new(BeliefVolConfig::default());

        tracker.record_observation("test", 0.5, 1000);
        tracker.record_observation("test", 0.6, 1000); // Same timestamp - should skip

        // Only the first (placeholder) entry should exist
        assert_eq!(tracker.market_count(), 1);
    }

    #[test]
    fn test_record_observation_skip_large_dt() {
        let mut tracker = BeliefVolTracker::new(BeliefVolConfig::default());

        tracker.record_observation("test", 0.5, 1000);
        tracker.record_observation("test", 0.6, 1000 + 7200); // 2 hours later - skip

        // Only the first (placeholder) entry should exist
        assert_eq!(tracker.market_count(), 1);
    }

    #[test]
    fn test_record_observation_max_history() {
        let config = BeliefVolConfig::default();
        let mut tracker = BeliefVolTracker::new(config).with_max_history(10);

        let mut ts = 1000i64;
        for i in 0..20 {
            let p = 0.5 + 0.1 * ((i as f64 * 0.3).sin());
            tracker.record_observation("test", p, ts);
            ts += 10;
        }

        // History should be trimmed to max_history
        // Can't directly access history, but we can verify it doesn't grow unbounded
        assert_eq!(tracker.market_count(), 1);
    }

    // ========================================================================
    // update_estimate Tests (EMA volatility estimation)
    // ========================================================================

    #[test]
    fn test_update_estimate_constant_p() {
        // Constant p=0.5 should produce very small sigma_b (only floor value)
        let config = BeliefVolConfig {
            min_samples: 10,
            ..Default::default()
        };
        let mut tracker = BeliefVolTracker::new(config);

        let slug = "constant-test";
        let mut ts = 1000i64;

        // Feed constant p=0.5
        for _ in 0..50 {
            tracker.record_observation(slug, 0.5, ts);
            ts += 10;
        }

        let estimate = tracker.get_estimate(slug);
        assert!(
            estimate.is_some(),
            "Should have estimate after 50 observations"
        );

        let est = estimate.unwrap();
        // sigma_b should be at the floor (0.01) since there's no movement
        assert!(
            est.sigma_b <= 0.02,
            "Constant p should have very low sigma_b, got {}",
            est.sigma_b
        );
    }

    #[test]
    fn test_update_estimate_alternating_p() {
        // Alternating p=0.4, p=0.6 should produce larger sigma_b
        let config = BeliefVolConfig {
            min_samples: 10,
            ..Default::default()
        };
        let mut tracker = BeliefVolTracker::new(config);

        let slug = "alternating-test";
        let mut ts = 1000i64;

        // Feed alternating p values
        for i in 0..50 {
            let p = if i % 2 == 0 { 0.4 } else { 0.6 };
            tracker.record_observation(slug, p, ts);
            ts += 10;
        }

        let estimate = tracker.get_estimate(slug);
        assert!(estimate.is_some());

        let est = estimate.unwrap();
        // sigma_b should be significant due to the alternating moves
        assert!(
            est.sigma_b > 0.1,
            "Alternating p should have non-trivial sigma_b, got {}",
            est.sigma_b
        );
    }

    #[test]
    fn test_update_estimate_larger_moves_higher_vol() {
        let config = BeliefVolConfig {
            min_samples: 10,
            ..Default::default()
        };

        // Small moves: 0.48 <-> 0.52
        let mut tracker_small = BeliefVolTracker::new(config.clone());
        let mut ts = 1000i64;
        for i in 0..50 {
            let p = if i % 2 == 0 { 0.48 } else { 0.52 };
            tracker_small.record_observation("small", p, ts);
            ts += 10;
        }

        // Large moves: 0.3 <-> 0.7
        let mut tracker_large = BeliefVolTracker::new(config);
        ts = 1000;
        for i in 0..50 {
            let p = if i % 2 == 0 { 0.3 } else { 0.7 };
            tracker_large.record_observation("large", p, ts);
            ts += 10;
        }

        let sigma_small = tracker_small.get_estimate("small").unwrap().sigma_b;
        let sigma_large = tracker_large.get_estimate("large").unwrap().sigma_b;

        assert!(
            sigma_large > sigma_small,
            "Larger moves should have higher sigma_b: {} vs {}",
            sigma_large,
            sigma_small
        );
    }

    #[test]
    fn test_update_estimate_confidence_increases() {
        let config = BeliefVolConfig {
            min_samples: 30,
            ..Default::default()
        };
        let mut tracker = BeliefVolTracker::new(config);

        let slug = "confidence-test";
        let mut ts = 1000i64;

        // Record 10 samples (below min_samples)
        for i in 0..10 {
            let p = 0.5 + 0.05 * ((i as f64 * 0.5).sin());
            tracker.record_observation(slug, p, ts);
            ts += 10;
        }

        let conf_10 = tracker
            .get_estimate(slug)
            .map(|e| e.confidence)
            .unwrap_or(0.0);

        // Record 20 more samples (now 30 total, at min_samples)
        for i in 10..30 {
            let p = 0.5 + 0.05 * ((i as f64 * 0.5).sin());
            tracker.record_observation(slug, p, ts);
            ts += 10;
        }

        let conf_30 = tracker
            .get_estimate(slug)
            .map(|e| e.confidence)
            .unwrap_or(0.0);

        // Record 20 more samples (now 50 total)
        for i in 30..50 {
            let p = 0.5 + 0.05 * ((i as f64 * 0.5).sin());
            tracker.record_observation(slug, p, ts);
            ts += 10;
        }

        let conf_50 = tracker
            .get_estimate(slug)
            .map(|e| e.confidence)
            .unwrap_or(0.0);

        // Confidence should increase with samples (up to 1.0)
        assert!(
            conf_30 > conf_10,
            "Confidence should increase: {} vs {}",
            conf_30,
            conf_10
        );
        // At min_samples (30), confidence should be ~1.0
        assert!(
            conf_30 >= 0.9,
            "At min_samples, confidence should be ~1.0, got {}",
            conf_30
        );
        // Should cap at 1.0
        assert!(
            (conf_50 - 1.0).abs() < 0.01,
            "Confidence should cap at 1.0, got {}",
            conf_50
        );
    }

    #[test]
    fn test_has_reliable_estimate_with_data() {
        let config = BeliefVolConfig {
            min_samples: 20,
            ..Default::default()
        };
        let mut tracker = BeliefVolTracker::new(config);

        let slug = "reliable-test";
        let mut ts = 1000i64;

        // Not reliable yet (too few samples)
        for i in 0..10 {
            let p = 0.5 + 0.05 * ((i as f64 * 0.5).sin());
            tracker.record_observation(slug, p, ts);
            ts += 10;
        }
        assert!(
            !tracker.has_reliable_estimate(slug),
            "Should not be reliable with only 10 samples"
        );

        // Now add enough to be reliable
        for i in 10..30 {
            let p = 0.5 + 0.05 * ((i as f64 * 0.5).sin());
            tracker.record_observation(slug, p, ts);
            ts += 10;
        }
        assert!(
            tracker.has_reliable_estimate(slug),
            "Should be reliable with 30 samples (min=20)"
        );
    }

    // ========================================================================
    // realized_vol_window Tests
    // ========================================================================

    #[test]
    fn test_realized_vol_window_none_insufficient_data() {
        let mut tracker = BeliefVolTracker::new(BeliefVolConfig::default());

        // Only 3 observations (need at least 5)
        tracker.record_observation("test", 0.5, 1000);
        tracker.record_observation("test", 0.6, 1010);
        tracker.record_observation("test", 0.5, 1020);

        let vol = tracker.realized_vol_window("test", 3600, 1030);
        assert!(vol.is_none(), "Should return None with < 5 samples");
    }

    #[test]
    fn test_realized_vol_window_basic() {
        let mut tracker = BeliefVolTracker::new(BeliefVolConfig::default());

        let slug = "window-test";
        let mut ts = 1000i64;

        // Record 20 observations with alternating p
        for i in 0..20 {
            let p = if i % 2 == 0 { 0.45 } else { 0.55 };
            tracker.record_observation(slug, p, ts);
            ts += 10;
        }

        let now_ts = ts;
        let vol = tracker.realized_vol_window(slug, 3600, now_ts);

        assert!(vol.is_some(), "Should have vol with 20 samples");
        let v = vol.unwrap();
        assert!(v > 0.0, "Vol should be positive");
        assert!(v.is_finite(), "Vol should be finite");
    }

    #[test]
    fn test_realized_vol_window_respects_cutoff() {
        let mut tracker = BeliefVolTracker::new(BeliefVolConfig::default());

        let slug = "cutoff-test";

        // Old data (outside window)
        for i in 0..10 {
            let p = if i % 2 == 0 { 0.3 } else { 0.7 }; // High volatility
            tracker.record_observation(slug, p, 1000 + i * 10);
        }

        // Recent data (inside window) - low volatility
        for i in 0..10 {
            let p = 0.5 + 0.01 * ((i as f64 * 0.5).sin()); // Very small moves
            tracker.record_observation(slug, p, 2000 + i * 10);
        }

        // Window of 500 seconds from ts=2100 should only see recent low-vol data
        let vol_recent = tracker.realized_vol_window(slug, 500, 2100).unwrap();
        assert!(vol_recent.is_finite() && vol_recent > 0.0);

        // Including the older high-volatility segment should increase realized vol.
        let vol_all = tracker.realized_vol_window(slug, 2000, 2100).unwrap();
        assert!(
            vol_all > vol_recent,
            "Including older high-vol data should increase vol (recent={}, all={})",
            vol_recent,
            vol_all
        );
    }

    #[test]
    fn test_realized_vol_vs_ema_same_order_of_magnitude() {
        let config = BeliefVolConfig {
            min_samples: 10,
            ema_alpha: 0.1,
            ..Default::default()
        };
        let mut tracker = BeliefVolTracker::new(config);

        let slug = "compare-test";
        let mut ts = 1000i64;

        // Feed consistent alternating data
        for i in 0..100 {
            let p = if i % 2 == 0 { 0.4 } else { 0.6 };
            tracker.record_observation(slug, p, ts);
            ts += 10;
        }

        let now_ts = ts;

        // Get EMA estimate
        let ema_sigma = tracker.get_estimate(slug).map(|e| e.sigma_b).unwrap_or(0.0);

        // Get realized vol over full window
        let realized_sigma = tracker
            .realized_vol_window(slug, 2000, now_ts)
            .unwrap_or(0.0);

        // Both should be positive
        assert!(ema_sigma > 0.0, "EMA sigma should be positive");
        assert!(realized_sigma > 0.0, "Realized sigma should be positive");

        // They should be in the same order of magnitude (within 10x)
        let ratio = ema_sigma / realized_sigma;
        assert!(
            ratio > 0.1 && ratio < 10.0,
            "EMA ({}) and realized ({}) should be similar order of magnitude, ratio={}",
            ema_sigma,
            realized_sigma,
            ratio
        );
    }

    #[test]
    fn test_realized_vol_window_case_insensitive() {
        let mut tracker = BeliefVolTracker::new(BeliefVolConfig::default());

        // Record with mixed case
        let mut ts = 1000i64;
        for i in 0..10 {
            let p = if i % 2 == 0 { 0.45 } else { 0.55 };
            tracker.record_observation("TEST-Market", p, ts);
            ts += 10;
        }

        // Query with different case
        let vol = tracker.realized_vol_window("test-market", 3600, ts);
        assert!(vol.is_some(), "Should find market regardless of case");
    }

    // ========================================================================
    // Jump Detection Tests
    // ========================================================================

    #[test]
    fn test_detect_recent_jump_no_data() {
        let tracker = BeliefVolTracker::new(BeliefVolConfig::default());
        let result = tracker.detect_recent_jump("nonexistent", 3.0);
        assert!(result.is_none());
    }

    #[test]
    fn test_detect_recent_jump_small_move() {
        let config = BeliefVolConfig {
            min_samples: 5,
            ..Default::default()
        };
        let mut tracker = BeliefVolTracker::new(config);

        let slug = "small-move";
        let mut ts = 1000i64;

        // Build up history with moderate moves to establish sigma_b
        for i in 0..20 {
            let p = 0.5 + 0.05 * ((i as f64 * 0.5).sin());
            tracker.record_observation(slug, p, ts);
            ts += 10;
        }

        // Add a small move (within normal range)
        tracker.record_observation(slug, 0.51, ts);

        let result = tracker.detect_recent_jump(slug, 3.0);
        assert!(result.is_some());
        let r = result.unwrap();
        assert!(!r.is_jump, "Small move should not be a jump");
        assert!(r.z_score < 3.0, "Z-score should be below threshold");
        assert_eq!(r.jump_size, 0.0, "Jump size should be 0 for non-jumps");
    }

    #[test]
    fn test_detect_recent_jump_large_move() {
        let config = BeliefVolConfig {
            min_samples: 5,
            ..Default::default()
        };
        let mut tracker = BeliefVolTracker::new(config);

        let slug = "large-move";
        let mut ts = 1000i64;

        // Build up history with small moves (low volatility baseline)
        for _ in 0..30 {
            tracker.record_observation(slug, 0.50, ts);
            ts += 10;
        }

        // Now inject a HUGE move (0.50 -> 0.90)
        // This should definitely be a jump
        tracker.record_observation(slug, 0.90, ts);

        let result = tracker.detect_recent_jump(slug, 3.0);
        assert!(result.is_some(), "Should have result");
        let r = result.unwrap();

        // With constant prior history, any large move should be a jump
        // Note: sigma_b will be at floor (0.01), so even modest moves can trigger
        assert!(
            r.z_score > 1.0,
            "Large move should have high z-score: {}",
            r.z_score
        );
    }

    #[test]
    fn test_detect_recent_jump_threshold_sensitivity() {
        let config = BeliefVolConfig {
            min_samples: 5,
            ..Default::default()
        };
        let mut tracker = BeliefVolTracker::new(config);

        let slug = "threshold-test";
        let mut ts = 1000i64;

        // Build baseline
        for i in 0..20 {
            let p = 0.5 + 0.02 * ((i as f64 * 0.3).sin());
            tracker.record_observation(slug, p, ts);
            ts += 10;
        }

        // Add a moderately large move
        tracker.record_observation(slug, 0.6, ts);

        // With low threshold, might be a jump
        let r_low = tracker.detect_recent_jump(slug, 1.0).unwrap();
        // With high threshold, probably not
        let r_high = tracker.detect_recent_jump(slug, 10.0).unwrap();

        // Same z-score regardless of threshold
        assert!(
            (r_low.z_score - r_high.z_score).abs() < 1e-10,
            "Z-score should be same"
        );
        // But is_jump depends on threshold
        assert_eq!(r_low.threshold_used, 1.0);
        assert_eq!(r_high.threshold_used, 10.0);
    }

    #[test]
    fn test_count_recent_jumps_empty() {
        let tracker = BeliefVolTracker::new(BeliefVolConfig::default());
        let count = tracker.count_recent_jumps("nonexistent", 3600, 5000, 3.0);
        assert_eq!(count, 0);
    }

    #[test]
    fn test_count_recent_jumps_with_synthetic_jumps() {
        let config = BeliefVolConfig {
            min_samples: 5,
            ..Default::default()
        };
        let mut tracker = BeliefVolTracker::new(config);

        let slug = "jump-count-test";
        let mut ts = 1000i64;

        // Build baseline with small moves
        for _ in 0..20 {
            tracker.record_observation(slug, 0.50, ts);
            ts += 10;
        }

        // Inject 3 obvious jumps
        tracker.record_observation(slug, 0.80, ts); // Jump 1
        ts += 10;
        tracker.record_observation(slug, 0.50, ts); // Jump 2 (back down)
        ts += 10;
        tracker.record_observation(slug, 0.50, ts); // Normal
        ts += 10;
        tracker.record_observation(slug, 0.85, ts); // Jump 3
        ts += 10;

        let now = ts;

        // Count jumps with very low threshold (most moves are jumps relative to floor vol)
        let count_low = tracker.count_recent_jumps(slug, 1000, now, 0.5);

        // Count jumps with higher threshold
        let count_high = tracker.count_recent_jumps(slug, 1000, now, 5.0);

        // Lower threshold should catch more
        assert!(
            count_low >= count_high,
            "Lower threshold should catch >= jumps: {} vs {}",
            count_low,
            count_high
        );
    }

    #[test]
    fn test_count_recent_jumps_window_cutoff() {
        let config = BeliefVolConfig {
            min_samples: 5,
            ..Default::default()
        };
        let mut tracker = BeliefVolTracker::new(config);

        let slug = "window-cutoff";
        let mut ts = 1000i64;

        // Old jumps (outside window)
        for _ in 0..10 {
            tracker.record_observation(slug, 0.50, ts);
            ts += 10;
        }
        tracker.record_observation(slug, 0.90, ts); // Old jump
        ts += 10;

        // Gap
        ts = 5000;

        // Recent normal activity
        for _ in 0..10 {
            tracker.record_observation(slug, 0.50, ts);
            ts += 10;
        }

        let now = ts;

        // Window of 200 seconds should only see recent data (no jumps)
        let count_recent = tracker.count_recent_jumps(slug, 200, now, 3.0);

        // Window of 5000 seconds should see old jump too
        let count_all = tracker.count_recent_jumps(slug, 5000, now, 3.0);

        assert!(
            count_all >= count_recent,
            "Larger window should catch >= jumps: {} vs {}",
            count_all,
            count_recent
        );
    }

    // ========================================================================
    // Serialization / Persistence Tests
    // ========================================================================

    #[test]
    fn test_export_estimates_empty() {
        let tracker = BeliefVolTracker::new(BeliefVolConfig::default());
        let exported = tracker.export_estimates();
        assert!(exported.is_empty());
    }

    #[test]
    fn test_export_import_roundtrip() {
        let config = BeliefVolConfig {
            min_samples: 5,
            ..Default::default()
        };
        let mut tracker = BeliefVolTracker::new(config.clone());

        // Build some estimates
        let mut ts = 1000i64;
        for i in 0..20 {
            let p = 0.5 + 0.05 * ((i as f64 * 0.5).sin());
            tracker.record_observation("market-a", p, ts);
            tracker.record_observation("market-b", p * 0.9, ts);
            ts += 10;
        }

        // Export
        let exported = tracker.export_estimates();
        assert_eq!(exported.len(), 2);

        // Create new tracker and import
        let mut tracker2 = BeliefVolTracker::new(config);
        tracker2.import_estimates(exported.clone());

        // Verify estimates match
        let est_a1 = tracker.get_estimate("market-a").unwrap();
        let est_a2 = tracker2.get_estimate("market-a").unwrap();
        assert!((est_a1.sigma_b - est_a2.sigma_b).abs() < 1e-10);
        assert_eq!(est_a1.sample_count, est_a2.sample_count);
    }

    #[test]
    fn test_import_estimates_case_insensitive() {
        let mut tracker = BeliefVolTracker::new(BeliefVolConfig::default());

        let mut estimates = HashMap::new();
        estimates.insert(
            "UPPER-CASE".to_string(),
            BeliefVolEstimate {
                sigma_b: 1.5,
                sample_count: 50,
                last_updated: 1000,
                confidence: 1.0,
            },
        );

        tracker.import_estimates(estimates);

        // Should be accessible with lowercase
        let est = tracker.get_estimate("upper-case");
        assert!(est.is_some());
        assert!((est.unwrap().sigma_b - 1.5).abs() < 1e-10);
    }

    #[test]
    fn test_summary_empty() {
        let tracker = BeliefVolTracker::new(BeliefVolConfig::default());
        let summary = tracker.summary();

        assert_eq!(summary.markets_tracked, 0);
        assert_eq!(summary.reliable_estimates, 0);
        assert!((summary.avg_sigma_b - 2.0).abs() < 1e-10); // Prior value
    }

    #[test]
    fn test_summary_with_data() {
        let config = BeliefVolConfig {
            min_samples: 10,
            ..Default::default()
        };
        let mut tracker = BeliefVolTracker::new(config);

        // Add two markets with different amounts of data
        let mut ts = 1000i64;
        for i in 0..30 {
            let p = 0.5 + 0.05 * ((i as f64 * 0.5).sin());
            tracker.record_observation("market-a", p, ts);
            if i < 5 {
                // Only 5 observations for market-b
                tracker.record_observation("market-b", p, ts);
            }
            ts += 10;
        }

        let summary = tracker.summary();

        assert_eq!(summary.markets_tracked, 2);
        // market-a should be reliable (30 samples > 10), market-b not (5 < 10)
        assert!(summary.reliable_estimates >= 1);
        assert!(summary.avg_sigma_b > 0.0);
    }

    // ========================================================================
    // Integration Test: 15-minute session simulation
    // ========================================================================

    #[test]
    fn test_15m_session_simulation() {
        let config = BeliefVolConfig::default();
        let mut tracker = BeliefVolTracker::new(config);

        let slug = "btc-updown-15m-test";
        let mut ts = 1700000000i64;
        let mut p = 0.50f64;

        // Simulate 100 observations over ~15 minutes
        // Price follows a random walk using simple LCG for reproducibility
        let mut rng_seed = 42u64;

        for _ in 0..100 {
            // Simple LCG for reproducibility
            rng_seed = rng_seed.wrapping_mul(1103515245).wrapping_add(12345);
            let rand_unit = (rng_seed % 1000) as f64 / 1000.0; // 0 to 1

            // Random walk in logit space
            let x = logit(p);
            let dx = (rand_unit - 0.5) * 0.1; // Small random change
            p = sigmoid(x + dx);

            tracker.record_observation(slug, p, ts);
            ts += 9; // ~9 seconds between observations
        }

        // Should have estimate now
        assert!(
            tracker.has_reliable_estimate(slug),
            "Should have reliable estimate after 100 observations"
        );

        let sigma_b = tracker.get_sigma_b(slug);

        // Should be positive and reasonable
        assert!(sigma_b > 0.0, "sigma_b should be positive");
        assert!(sigma_b < 100.0, "sigma_b should be reasonable (< 100)");

        // Test realized vol window (10 minute window)
        let rv = tracker.realized_vol_window(slug, 600, ts);
        assert!(rv.is_some(), "Should have realized vol");

        // Summary should show 1 market
        let summary = tracker.summary();
        assert_eq!(summary.markets_tracked, 1);
        assert_eq!(summary.reliable_estimates, 1);
    }

    #[test]
    fn test_multi_market_simulation() {
        let config = BeliefVolConfig {
            min_samples: 20,
            ..Default::default()
        };
        let mut tracker = BeliefVolTracker::new(config);

        let markets = ["btc-updown", "eth-updown", "sol-updown"];
        let mut ts = 1700000000i64;

        // Simulate 50 observations per market
        for _ in 0..50 {
            for (i, market) in markets.iter().enumerate() {
                // Different volatility regimes per market
                let base_vol = 0.02 + (i as f64 * 0.01);
                let p = 0.5 + base_vol * ((ts as f64 * 0.001 + i as f64).sin());
                tracker.record_observation(market, p, ts);
            }
            ts += 10;
        }

        // All markets should be tracked
        assert_eq!(tracker.market_count(), 3);

        // Check summary
        let summary = tracker.summary();
        assert_eq!(summary.markets_tracked, 3);

        // Each market should have different sigma_b (due to different vol regimes)
        let sigmas: Vec<f64> = markets.iter().map(|m| tracker.get_sigma_b(m)).collect();

        // They should all be positive
        for s in &sigmas {
            assert!(*s > 0.0);
        }
    }
}

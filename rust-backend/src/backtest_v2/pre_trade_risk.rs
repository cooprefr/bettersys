//! Pre-Trade Risk Controls
//!
//! Implements risk checks that run BEFORE order submission.
//! All checks are deterministic and logged to DecisionProof.
//!
//! # Design Principles
//!
//! 1. **Pre-trade, not post-trade**: Reject trades before submission, don't "fix" after fills
//! 2. **Deterministic**: Same inputs always produce same decision
//! 3. **Logged**: Every check result is recorded in DecisionProof
//! 4. **Conservative**: When in doubt, reject the trade

use crate::backtest_v2::basis_signal::BasisObservation;
use crate::backtest_v2::clock::Nanos;
use crate::backtest_v2::events::Side;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// =============================================================================
// CONFIGURATION
// =============================================================================

/// Configuration for pre-trade risk controls.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreTradeRiskConfig {
    /// Maximum inventory per market (in position units).
    pub max_inventory_per_market: f64,
    /// Maximum inventory as a function of time to settlement.
    pub max_inventory_per_time_to_settle: MaxInventoryByTime,
    /// Maximum total exposure across all markets.
    pub max_total_exposure: f64,
    /// Minimum required edge after execution cost.
    pub min_edge_after_cost: f64,
    /// Minimum basis confidence to trade.
    pub min_basis_confidence: f64,
    /// Maximum Chainlink staleness (seconds).
    pub max_chainlink_staleness_sec: f64,
    /// Minimum time to boundary for new positions (seconds).
    pub min_time_to_boundary_sec: f64,
    /// Enable hard stops (reject all trades if limits breached).
    pub enable_hard_stops: bool,
}

impl Default for PreTradeRiskConfig {
    fn default() -> Self {
        Self {
            max_inventory_per_market: 100.0,
            max_inventory_per_time_to_settle: MaxInventoryByTime::default(),
            max_total_exposure: 1000.0,
            min_edge_after_cost: 0.001, // 10 bps
            min_basis_confidence: 0.5,
            max_chainlink_staleness_sec: 120.0, // 2 minutes
            min_time_to_boundary_sec: 10.0, // 10 seconds
            enable_hard_stops: true,
        }
    }
}

impl PreTradeRiskConfig {
    /// Conservative configuration for production.
    pub fn conservative() -> Self {
        Self {
            max_inventory_per_market: 50.0,
            max_inventory_per_time_to_settle: MaxInventoryByTime::conservative(),
            max_total_exposure: 500.0,
            min_edge_after_cost: 0.002, // 20 bps
            min_basis_confidence: 0.6,
            max_chainlink_staleness_sec: 60.0,
            min_time_to_boundary_sec: 30.0,
            enable_hard_stops: true,
        }
    }
}

/// Maximum inventory as a function of time to settlement.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MaxInventoryByTime {
    /// Inventory limit when >5 minutes to settle.
    pub over_5min: f64,
    /// Inventory limit when 2-5 minutes to settle.
    pub between_2_5min: f64,
    /// Inventory limit when 1-2 minutes to settle.
    pub between_1_2min: f64,
    /// Inventory limit when <1 minute to settle.
    pub under_1min: f64,
}

impl Default for MaxInventoryByTime {
    fn default() -> Self {
        Self {
            over_5min: 100.0,
            between_2_5min: 75.0,
            between_1_2min: 50.0,
            under_1min: 25.0,
        }
    }
}

impl MaxInventoryByTime {
    /// Conservative limits.
    pub fn conservative() -> Self {
        Self {
            over_5min: 50.0,
            between_2_5min: 30.0,
            between_1_2min: 15.0,
            under_1min: 5.0,
        }
    }
    
    /// Get limit for given time to settlement.
    pub fn limit_for_time(&self, time_to_settle_sec: f64) -> f64 {
        if time_to_settle_sec > 300.0 {
            self.over_5min
        } else if time_to_settle_sec > 120.0 {
            self.between_2_5min
        } else if time_to_settle_sec > 60.0 {
            self.between_1_2min
        } else {
            self.under_1min
        }
    }
}

// =============================================================================
// RISK CHECK RESULT
// =============================================================================

/// Result of a single risk check.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskCheckResult {
    /// Name of the check.
    pub check_name: String,
    /// Whether the check passed.
    pub passed: bool,
    /// Observed value.
    pub observed_value: f64,
    /// Limit/threshold.
    pub limit: f64,
    /// Human-readable message.
    pub message: String,
}

impl RiskCheckResult {
    /// Create a passing check.
    pub fn pass(check_name: impl Into<String>, observed: f64, limit: f64) -> Self {
        Self {
            check_name: check_name.into(),
            passed: true,
            observed_value: observed,
            limit,
            message: format!("PASS: {:.4} within limit {:.4}", observed, limit),
        }
    }
    
    /// Create a failing check.
    pub fn fail(check_name: impl Into<String>, observed: f64, limit: f64, reason: impl Into<String>) -> Self {
        Self {
            check_name: check_name.into(),
            passed: false,
            observed_value: observed,
            limit,
            message: format!("FAIL: {}", reason.into()),
        }
    }
}

/// Aggregate result of all risk checks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreTradeRiskResult {
    /// Individual check results.
    pub checks: Vec<RiskCheckResult>,
    /// Whether all checks passed.
    pub all_passed: bool,
    /// First failure reason (if any).
    pub first_failure: Option<String>,
    /// Recommended position size (may be reduced from requested).
    pub recommended_size: f64,
    /// Original requested size.
    pub requested_size: f64,
    /// Risk-adjusted expected edge.
    pub adjusted_edge: f64,
}

impl PreTradeRiskResult {
    /// Create a result from check results.
    pub fn from_checks(
        checks: Vec<RiskCheckResult>,
        requested_size: f64,
        adjusted_edge: f64,
    ) -> Self {
        let all_passed = checks.iter().all(|c| c.passed);
        let first_failure = checks.iter()
            .find(|c| !c.passed)
            .map(|c| c.message.clone());
        
        let recommended_size = if all_passed {
            requested_size
        } else {
            0.0
        };
        
        Self {
            checks,
            all_passed,
            first_failure,
            recommended_size,
            requested_size,
            adjusted_edge,
        }
    }
}

// =============================================================================
// ORDER REQUEST
// =============================================================================

/// A trade order request to be risk-checked.
#[derive(Debug, Clone)]
pub struct OrderRequest {
    /// Market ID.
    pub market_id: String,
    /// Token ID.
    pub token_id: String,
    /// Order side.
    pub side: Side,
    /// Requested size.
    pub size: f64,
    /// Limit price.
    pub price: f64,
    /// Expected execution cost (spread + fees).
    pub expected_execution_cost: f64,
    /// Expected payoff if trade succeeds.
    pub expected_payoff: f64,
    /// Current basis observation.
    pub basis: Option<BasisObservation>,
    /// Basis confidence.
    pub basis_confidence: f64,
    /// Time to window boundary (seconds).
    pub time_to_boundary_sec: Option<f64>,
    /// Decision timestamp.
    pub decision_time_ns: Nanos,
}

// =============================================================================
// PRE-TRADE RISK CONTROLLER
// =============================================================================

/// The main pre-trade risk controller.
pub struct PreTradeRiskController {
    config: PreTradeRiskConfig,
    /// Current inventory per market/token.
    inventory: HashMap<String, f64>,
    /// Total exposure.
    total_exposure: f64,
    /// Decision log.
    decisions: Vec<PreTradeRiskResult>,
}

impl PreTradeRiskController {
    /// Create a new risk controller.
    pub fn new(config: PreTradeRiskConfig) -> Self {
        Self {
            config,
            inventory: HashMap::new(),
            total_exposure: 0.0,
            decisions: vec![],
        }
    }
    
    /// Check an order request against all risk controls.
    pub fn check(&self, request: &OrderRequest) -> PreTradeRiskResult {
        let mut checks = vec![];
        
        // 1. Maximum inventory per market
        checks.push(self.check_max_inventory(request));
        
        // 2. Inventory by time to settle
        checks.push(self.check_inventory_by_time(request));
        
        // 3. Total exposure
        checks.push(self.check_total_exposure(request));
        
        // 4. Minimum edge
        checks.push(self.check_min_edge(request));
        
        // 5. Basis confidence
        checks.push(self.check_basis_confidence(request));
        
        // 6. Chainlink staleness
        checks.push(self.check_chainlink_staleness(request));
        
        // 7. Time to boundary
        checks.push(self.check_time_to_boundary(request));
        
        // Calculate adjusted edge
        let gross_edge = request.expected_payoff - request.expected_execution_cost;
        let adjusted_edge = gross_edge * request.basis_confidence;
        
        PreTradeRiskResult::from_checks(checks, request.size, adjusted_edge)
    }
    
    /// Check and potentially execute (update state) if all checks pass.
    /// Returns (result, log_entries) where log_entries can be added to decision metadata.
    pub fn check_and_execute(&mut self, request: &OrderRequest) -> (PreTradeRiskResult, Vec<(String, String)>) {
        let result = self.check(request);
        
        // Build log entries
        let mut log_entries = vec![];
        for check in &result.checks {
            log_entries.push((
                format!("risk_{}", check.check_name),
                format!("{}: {:.4} vs limit {:.4}", 
                    if check.passed { "PASS" } else { "FAIL" },
                    check.observed_value,
                    check.limit
                ),
            ));
        }
        
        log_entries.push((
            "risk_all_passed".to_string(),
            result.all_passed.to_string(),
        ));
        
        if let Some(ref failure) = result.first_failure {
            log_entries.push((
                "risk_first_failure".to_string(),
                failure.clone(),
            ));
        }
        
        // Update state if passed
        if result.all_passed {
            let delta = match request.side {
                Side::Buy => request.size,
                Side::Sell => -request.size,
            };
            
            *self.inventory.entry(request.token_id.clone()).or_default() += delta;
            self.total_exposure += request.size * request.price;
        }
        
        self.decisions.push(result.clone());
        (result, log_entries)
    }
    
    // =========================================================================
    // Individual check implementations
    // =========================================================================
    
    fn check_max_inventory(&self, request: &OrderRequest) -> RiskCheckResult {
        let current = self.inventory.get(&request.token_id).copied().unwrap_or(0.0);
        let proposed = match request.side {
            Side::Buy => current + request.size,
            Side::Sell => current - request.size,
        };
        
        let limit = self.config.max_inventory_per_market;
        
        if proposed.abs() <= limit {
            RiskCheckResult::pass("max_inventory_market", proposed.abs(), limit)
        } else {
            RiskCheckResult::fail(
                "max_inventory_market",
                proposed.abs(),
                limit,
                format!("Inventory {} would exceed max {}", proposed.abs(), limit),
            )
        }
    }
    
    fn check_inventory_by_time(&self, request: &OrderRequest) -> RiskCheckResult {
        let time_to_settle = request.time_to_boundary_sec.unwrap_or(f64::INFINITY);
        let limit = self.config.max_inventory_per_time_to_settle.limit_for_time(time_to_settle);
        
        let current = self.inventory.get(&request.token_id).copied().unwrap_or(0.0);
        let proposed = match request.side {
            Side::Buy => current + request.size,
            Side::Sell => current - request.size,
        };
        
        if proposed.abs() <= limit {
            RiskCheckResult::pass("inventory_by_time", proposed.abs(), limit)
        } else {
            RiskCheckResult::fail(
                "inventory_by_time",
                proposed.abs(),
                limit,
                format!(
                    "Inventory {} exceeds time-based limit {} with {:.0}s to settle",
                    proposed.abs(), limit, time_to_settle
                ),
            )
        }
    }
    
    fn check_total_exposure(&self, request: &OrderRequest) -> RiskCheckResult {
        let proposed_exposure = self.total_exposure + request.size * request.price;
        let limit = self.config.max_total_exposure;
        
        if proposed_exposure <= limit {
            RiskCheckResult::pass("total_exposure", proposed_exposure, limit)
        } else {
            RiskCheckResult::fail(
                "total_exposure",
                proposed_exposure,
                limit,
                format!("Total exposure {} would exceed max {}", proposed_exposure, limit),
            )
        }
    }
    
    fn check_min_edge(&self, request: &OrderRequest) -> RiskCheckResult {
        let edge = request.expected_payoff - request.expected_execution_cost;
        let limit = self.config.min_edge_after_cost;
        
        if edge >= limit {
            RiskCheckResult::pass("min_edge", edge, limit)
        } else {
            RiskCheckResult::fail(
                "min_edge",
                edge,
                limit,
                format!("Expected edge {:.4} below minimum {:.4}", edge, limit),
            )
        }
    }
    
    fn check_basis_confidence(&self, request: &OrderRequest) -> RiskCheckResult {
        let confidence = request.basis_confidence;
        let limit = self.config.min_basis_confidence;
        
        if confidence >= limit {
            RiskCheckResult::pass("basis_confidence", confidence, limit)
        } else {
            RiskCheckResult::fail(
                "basis_confidence",
                confidence,
                limit,
                format!("Basis confidence {:.2} below minimum {:.2}", confidence, limit),
            )
        }
    }
    
    fn check_chainlink_staleness(&self, request: &OrderRequest) -> RiskCheckResult {
        let staleness = request.basis
            .as_ref()
            .map(|b| b.chainlink_staleness_sec)
            .unwrap_or(f64::INFINITY);
        let limit = self.config.max_chainlink_staleness_sec;
        
        if staleness <= limit {
            RiskCheckResult::pass("chainlink_staleness", staleness, limit)
        } else {
            RiskCheckResult::fail(
                "chainlink_staleness",
                staleness,
                limit,
                format!("Chainlink data {:.0}s stale, max allowed {:.0}s", staleness, limit),
            )
        }
    }
    
    fn check_time_to_boundary(&self, request: &OrderRequest) -> RiskCheckResult {
        let time_to_boundary = request.time_to_boundary_sec.unwrap_or(f64::INFINITY);
        let limit = self.config.min_time_to_boundary_sec;
        
        if time_to_boundary >= limit {
            RiskCheckResult::pass("time_to_boundary", time_to_boundary, limit)
        } else {
            RiskCheckResult::fail(
                "time_to_boundary",
                time_to_boundary,
                limit,
                format!("Only {:.0}s to boundary, min required {:.0}s", time_to_boundary, limit),
            )
        }
    }
    
    // =========================================================================
    // State management
    // =========================================================================
    
    /// Update inventory after a fill (external update).
    pub fn record_fill(&mut self, token_id: &str, side: Side, size: f64, price: f64) {
        let delta = match side {
            Side::Buy => size,
            Side::Sell => -size,
        };
        
        *self.inventory.entry(token_id.to_string()).or_default() += delta;
        // Note: Don't double-count exposure since check_and_execute already added it
    }
    
    /// Update inventory on settlement/expiry.
    pub fn record_settlement(&mut self, token_id: &str, _final_value: f64) {
        let _position = self.inventory.remove(token_id).unwrap_or(0.0);
        // total_exposure will be recalculated if needed
    }
    
    /// Get current inventory for a token.
    pub fn inventory(&self, token_id: &str) -> f64 {
        self.inventory.get(token_id).copied().unwrap_or(0.0)
    }
    
    /// Get total exposure.
    pub fn total_exposure(&self) -> f64 {
        self.total_exposure
    }
    
    /// Get all decisions.
    pub fn decisions(&self) -> &[PreTradeRiskResult] {
        &self.decisions
    }
    
    /// Reset state (for new backtest window).
    pub fn reset(&mut self) {
        self.inventory.clear();
        self.total_exposure = 0.0;
        self.decisions.clear();
    }
    
    /// Generate summary statistics.
    pub fn summary(&self) -> PreTradeRiskSummary {
        let total_decisions = self.decisions.len();
        let passed = self.decisions.iter().filter(|d| d.all_passed).count();
        let rejected = total_decisions - passed;
        
        // Count rejections by check type
        let mut rejections_by_check: HashMap<String, u32> = HashMap::new();
        for decision in &self.decisions {
            for check in &decision.checks {
                if !check.passed {
                    *rejections_by_check.entry(check.check_name.clone()).or_default() += 1;
                }
            }
        }
        
        PreTradeRiskSummary {
            total_decisions,
            passed,
            rejected,
            pass_rate: if total_decisions > 0 { passed as f64 / total_decisions as f64 } else { 0.0 },
            rejections_by_check,
        }
    }
}

/// Summary of risk control statistics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreTradeRiskSummary {
    pub total_decisions: usize,
    pub passed: usize,
    pub rejected: usize,
    pub pass_rate: f64,
    pub rejections_by_check: HashMap<String, u32>,
}

// =============================================================================
// TESTS
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    
    fn make_request(size: f64, price: f64) -> OrderRequest {
        // Create a basis observation that won't fail staleness check
        let basis = BasisObservation {
            decision_time_ns: 1_000_000_000,
            binance_mid: 100.05,
            chainlink_price: 100.00,
            basis: 0.05,
            basis_bps: 5.0,
            chainlink_round_id: 1,
            chainlink_updated_at_unix_sec: 999, // 1 second stale (decision_time is 1000)
            chainlink_staleness_sec: 1.0,
            time_to_boundary_sec: Some(300.0),
            window_cutoff_unix_sec: Some(1300),
        };
        
        OrderRequest {
            market_id: "test_market".to_string(),
            token_id: "test_token".to_string(),
            side: Side::Buy,
            size,
            price,
            expected_execution_cost: 0.001,
            expected_payoff: 0.01,
            basis: Some(basis),
            basis_confidence: 0.8,
            time_to_boundary_sec: Some(300.0),
            decision_time_ns: 1_000_000_000,
        }
    }
    
    #[test]
    fn test_check_passes_within_limits() {
        let config = PreTradeRiskConfig::default();
        let controller = PreTradeRiskController::new(config);
        
        let request = make_request(10.0, 0.50);
        let result = controller.check(&request);
        
        assert!(result.all_passed, "Should pass within limits: {:?}", result.first_failure);
        assert_eq!(result.recommended_size, 10.0);
    }
    
    #[test]
    fn test_check_fails_over_inventory_limit() {
        let mut config = PreTradeRiskConfig::default();
        config.max_inventory_per_market = 5.0;
        
        let controller = PreTradeRiskController::new(config);
        let request = make_request(10.0, 0.50);
        let result = controller.check(&request);
        
        assert!(!result.all_passed);
        assert!(result.first_failure.is_some());
        // Check that at least one inventory check failed
        let has_inventory_failure = result.checks.iter()
            .any(|c| !c.passed && c.check_name.contains("inventory"));
        assert!(has_inventory_failure, "Expected an inventory check to fail, got: {:?}", 
            result.first_failure);
    }
    
    #[test]
    fn test_check_fails_low_edge() {
        let mut config = PreTradeRiskConfig::default();
        config.min_edge_after_cost = 0.1; // 10%
        
        let controller = PreTradeRiskController::new(config);
        let mut request = make_request(10.0, 0.50);
        request.expected_payoff = 0.01;
        request.expected_execution_cost = 0.005;
        
        let result = controller.check(&request);
        
        assert!(!result.all_passed);
        assert!(result.first_failure.unwrap().contains("edge"));
    }
    
    #[test]
    fn test_check_fails_low_confidence() {
        let mut config = PreTradeRiskConfig::default();
        config.min_basis_confidence = 0.9;
        
        let controller = PreTradeRiskController::new(config);
        let mut request = make_request(10.0, 0.50);
        request.basis_confidence = 0.5;
        
        let result = controller.check(&request);
        
        assert!(!result.all_passed);
        assert!(result.first_failure.unwrap().contains("confidence"));
    }
    
    #[test]
    fn test_inventory_by_time() {
        let config = PreTradeRiskConfig::default();
        let controller = PreTradeRiskController::new(config);
        
        // Long time to settle - higher limit
        let mut request = make_request(80.0, 0.50);
        request.time_to_boundary_sec = Some(400.0); // Over 5 min
        let result = controller.check(&request);
        assert!(result.all_passed, "Should pass with high limit: {:?}", result.first_failure);
        
        // Short time to settle - lower limit
        let mut request2 = make_request(80.0, 0.50);
        request2.time_to_boundary_sec = Some(30.0); // Under 1 min
        let result2 = controller.check(&request2);
        assert!(!result2.all_passed, "Should fail with low limit");
    }
    
    #[test]
    fn test_state_updates_on_execute() {
        let config = PreTradeRiskConfig::default();
        let mut controller = PreTradeRiskController::new(config);
        
        let request = make_request(10.0, 0.50);
        let (result, log_entries) = controller.check_and_execute(&request);
        
        assert!(result.all_passed);
        assert_eq!(controller.inventory("test_token"), 10.0);
        assert_eq!(controller.total_exposure(), 5.0); // 10 * 0.50
        assert!(!log_entries.is_empty());
    }
    
    #[test]
    fn test_summary_statistics() {
        let config = PreTradeRiskConfig::default();
        let mut controller = PreTradeRiskController::new(config);
        
        // Execute some trades
        for _ in 0..5 {
            let request = make_request(5.0, 0.50);
            controller.check_and_execute(&request);
        }
        
        let summary = controller.summary();
        assert_eq!(summary.total_decisions, 5);
        assert!(summary.pass_rate > 0.0);
    }
    
    #[test]
    fn test_time_to_boundary_check() {
        let mut config = PreTradeRiskConfig::default();
        config.min_time_to_boundary_sec = 60.0;
        
        let controller = PreTradeRiskController::new(config);
        
        // Enough time
        let mut request = make_request(10.0, 0.50);
        request.time_to_boundary_sec = Some(120.0);
        let result = controller.check(&request);
        assert!(result.checks.iter().find(|c| c.check_name == "time_to_boundary").unwrap().passed);
        
        // Not enough time
        let mut request2 = make_request(10.0, 0.50);
        request2.time_to_boundary_sec = Some(30.0);
        let result2 = controller.check(&request2);
        assert!(!result2.checks.iter().find(|c| c.check_name == "time_to_boundary").unwrap().passed);
    }
}

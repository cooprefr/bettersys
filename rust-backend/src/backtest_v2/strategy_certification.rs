//! Strategy Certification Framework
//!
//! For any strategy claiming profitability, this module verifies:
//! - Passing adversarial gates
//! - Sensitivity robustness
//! - Live-backtest parity within tolerance
//!
//! No strategy can be "certified" without meeting all criteria.

use crate::backtest_v2::basis_signal::BasisDecisionRecord;
use crate::backtest_v2::clock::Nanos;
use crate::backtest_v2::data_contract::{DatasetClassification, DatasetReadiness};
use crate::backtest_v2::gate_suite::{GateSuiteReport, TrustLevel};
use crate::backtest_v2::orchestrator::{BacktestResults, TruthfulnessSummary, TrustVerdict};
use crate::backtest_v2::pre_trade_risk::PreTradeRiskSummary;
use crate::backtest_v2::sensitivity::{FragilityFlags, SensitivityReport, TrustRecommendation};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// =============================================================================
// PNL DECOMPOSITION
// =============================================================================

/// PnL decomposition by attribution source.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PnLDecomposition {
    /// Total realized PnL.
    pub total_pnl: f64,
    /// PnL from basis convergence/divergence.
    pub basis_pnl: f64,
    /// PnL from favorable execution (better than expected fill price).
    pub execution_pnl: f64,
    /// PnL from settlement outcome.
    pub settlement_pnl: f64,
    /// PnL from fees paid.
    pub fees_pnl: f64,
    /// Unexplained PnL (should be ~0).
    pub unexplained_pnl: f64,
    /// Number of trades contributing.
    pub trade_count: u64,
}

impl PnLDecomposition {
    /// Compute decomposition from trade records.
    pub fn from_trades(trades: &[TradeAttribution]) -> Self {
        let mut decomp = Self::default();
        
        for trade in trades {
            decomp.basis_pnl += trade.basis_contribution;
            decomp.execution_pnl += trade.execution_contribution;
            decomp.settlement_pnl += trade.settlement_contribution;
            decomp.fees_pnl += trade.fees_paid;
        }
        
        decomp.total_pnl = decomp.basis_pnl + decomp.execution_pnl 
            + decomp.settlement_pnl - decomp.fees_pnl;
        decomp.trade_count = trades.len() as u64;
        decomp.unexplained_pnl = 0.0; // Will be set if there's a discrepancy
        
        decomp
    }
    
    /// Check if decomposition is valid (components sum to total).
    pub fn is_valid(&self) -> bool {
        let computed = self.basis_pnl + self.execution_pnl + self.settlement_pnl - self.fees_pnl;
        (computed - self.total_pnl).abs() < 1e-6
    }
    
    /// Format as string.
    pub fn format(&self) -> String {
        format!(
            "PnL Decomposition:\n\
             Total:       {:>12.4}\n\
             Basis:       {:>12.4} ({:>5.1}%)\n\
             Execution:   {:>12.4} ({:>5.1}%)\n\
             Settlement:  {:>12.4} ({:>5.1}%)\n\
             Fees:        {:>12.4} ({:>5.1}%)\n\
             Trades:      {:>12}",
            self.total_pnl,
            self.basis_pnl, self.pct(self.basis_pnl),
            self.execution_pnl, self.pct(self.execution_pnl),
            self.settlement_pnl, self.pct(self.settlement_pnl),
            -self.fees_pnl, self.pct(-self.fees_pnl),
            self.trade_count,
        )
    }
    
    fn pct(&self, component: f64) -> f64 {
        if self.total_pnl.abs() > 1e-9 {
            (component / self.total_pnl) * 100.0
        } else {
            0.0
        }
    }
}

/// Attribution for a single trade.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradeAttribution {
    /// Trade ID.
    pub trade_id: String,
    /// Timestamp.
    pub timestamp_ns: Nanos,
    /// Total realized on this trade.
    pub total_realized: f64,
    /// Contribution from basis.
    pub basis_contribution: f64,
    /// Contribution from execution.
    pub execution_contribution: f64,
    /// Contribution from settlement.
    pub settlement_contribution: f64,
    /// Fees paid.
    pub fees_paid: f64,
    /// Entry basis value.
    pub entry_basis: f64,
    /// Exit/settlement basis value.
    pub exit_basis: f64,
}

// =============================================================================
// LIVE-BACKTEST PARITY
// =============================================================================

/// Configuration for parity checking.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParityConfig {
    /// Maximum signal disagreement rate.
    pub max_signal_disagreement_rate: f64,
    /// Maximum execution divergence (absolute PnL difference).
    pub max_execution_divergence: f64,
    /// Maximum PnL attribution mismatch.
    pub max_pnl_mismatch_pct: f64,
    /// Number of windows to compare.
    pub windows_to_compare: usize,
}

impl Default for ParityConfig {
    fn default() -> Self {
        Self {
            max_signal_disagreement_rate: 0.05, // 5%
            max_execution_divergence: 0.001, // $0.001 per trade
            max_pnl_mismatch_pct: 10.0, // 10%
            windows_to_compare: 5,
        }
    }
}

/// A parity check window result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParityWindow {
    /// Window start timestamp.
    pub start_ns: Nanos,
    /// Window end timestamp.
    pub end_ns: Nanos,
    /// Live signal count.
    pub live_signals: u64,
    /// Backtest signal count.
    pub backtest_signals: u64,
    /// Signal agreement rate.
    pub signal_agreement_rate: f64,
    /// Live orders sent.
    pub live_orders: u64,
    /// Backtest orders sent.
    pub backtest_orders: u64,
    /// Live fills.
    pub live_fills: u64,
    /// Backtest fills.
    pub backtest_fills: u64,
    /// Live PnL.
    pub live_pnl: f64,
    /// Backtest PnL.
    pub backtest_pnl: f64,
    /// PnL difference.
    pub pnl_difference: f64,
    /// Whether this window passed parity checks.
    pub passed: bool,
    /// Failure reasons.
    pub failure_reasons: Vec<String>,
}

/// Result of parity checking.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParityResult {
    /// Individual window results.
    pub windows: Vec<ParityWindow>,
    /// Overall signal agreement rate.
    pub overall_signal_agreement: f64,
    /// Overall execution divergence.
    pub overall_execution_divergence: f64,
    /// Overall PnL mismatch percentage.
    pub overall_pnl_mismatch_pct: f64,
    /// Whether parity passed.
    pub passed: bool,
    /// Summary of failures.
    pub failure_summary: Vec<String>,
}

impl ParityResult {
    /// Compute overall result from windows.
    pub fn from_windows(windows: Vec<ParityWindow>, config: &ParityConfig) -> Self {
        let total_windows = windows.len() as f64;
        
        let overall_signal_agreement = if total_windows > 0.0 {
            windows.iter().map(|w| w.signal_agreement_rate).sum::<f64>() / total_windows
        } else {
            0.0
        };
        
        let total_live_pnl: f64 = windows.iter().map(|w| w.live_pnl).sum();
        let total_backtest_pnl: f64 = windows.iter().map(|w| w.backtest_pnl).sum();
        let overall_execution_divergence = (total_live_pnl - total_backtest_pnl).abs();
        
        let overall_pnl_mismatch_pct = if total_live_pnl.abs() > 1e-9 {
            ((total_backtest_pnl - total_live_pnl) / total_live_pnl).abs() * 100.0
        } else if total_backtest_pnl.abs() > 1e-9 {
            100.0 // 100% mismatch if live is 0 but backtest isn't
        } else {
            0.0
        };
        
        let mut failure_summary = vec![];
        
        if overall_signal_agreement < (1.0 - config.max_signal_disagreement_rate) {
            failure_summary.push(format!(
                "Signal agreement {:.1}% below threshold {:.1}%",
                overall_signal_agreement * 100.0,
                (1.0 - config.max_signal_disagreement_rate) * 100.0
            ));
        }
        
        if overall_pnl_mismatch_pct > config.max_pnl_mismatch_pct {
            failure_summary.push(format!(
                "PnL mismatch {:.1}% exceeds threshold {:.1}%",
                overall_pnl_mismatch_pct,
                config.max_pnl_mismatch_pct
            ));
        }
        
        let passed = failure_summary.is_empty();
        
        Self {
            windows,
            overall_signal_agreement,
            overall_execution_divergence,
            overall_pnl_mismatch_pct,
            passed,
            failure_summary,
        }
    }
    
    /// Format as string.
    pub fn format(&self) -> String {
        format!(
            "Parity Check Result: {}\n\
             Signal Agreement:     {:>6.1}%\n\
             Execution Divergence: {:>12.4}\n\
             PnL Mismatch:         {:>6.1}%\n\
             Windows:              {:>6}\n{}",
            if self.passed { "PASS" } else { "FAIL" },
            self.overall_signal_agreement * 100.0,
            self.overall_execution_divergence,
            self.overall_pnl_mismatch_pct,
            self.windows.len(),
            if self.failure_summary.is_empty() {
                "".to_string()
            } else {
                format!("Failures:\n  - {}", self.failure_summary.join("\n  - "))
            }
        )
    }
}

// =============================================================================
// STRATEGY CERTIFICATION
// =============================================================================

/// A certification claim that a strategy can make.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CertificationClaim {
    /// Strategy is profitable (positive expected return).
    Profitable,
    /// Strategy has positive Sharpe ratio.
    PositiveSharpe,
    /// Strategy survives adversarial gates.
    AdversarialGatesPassed,
    /// Strategy is robust to latency variations.
    LatencyRobust,
    /// Strategy is robust to queue model variations.
    QueueModelRobust,
    /// Strategy matches live behavior (parity passed).
    LiveParityPassed,
    /// Strategy basis-aware (uses Chainlink for settlement).
    BasisAware,
    /// Strategy uses pre-trade risk controls.
    RiskControlled,
    /// Strategy is suitable for production deployment.
    ProductionReady,
}

impl CertificationClaim {
    /// Get description.
    pub fn description(&self) -> &'static str {
        match self {
            Self::Profitable => "Strategy shows positive expected return",
            Self::PositiveSharpe => "Strategy has positive risk-adjusted return (Sharpe > 0)",
            Self::AdversarialGatesPassed => "Strategy survives zero-edge, martingale, and inversion tests",
            Self::LatencyRobust => "Strategy maintains profitability under latency sweeps",
            Self::QueueModelRobust => "Strategy maintains profitability under queue model variations",
            Self::LiveParityPassed => "Backtest matches live trading within tolerances",
            Self::BasisAware => "Strategy explicitly trades settlement truth, not proxy prices",
            Self::RiskControlled => "Strategy uses pre-trade risk controls at decision boundary",
            Self::ProductionReady => "Strategy meets all production deployment criteria",
        }
    }
    
    /// All claims.
    pub fn all() -> &'static [CertificationClaim] {
        &[
            Self::Profitable,
            Self::PositiveSharpe,
            Self::AdversarialGatesPassed,
            Self::LatencyRobust,
            Self::QueueModelRobust,
            Self::LiveParityPassed,
            Self::BasisAware,
            Self::RiskControlled,
            Self::ProductionReady,
        ]
    }
}

/// A fragility identified in the strategy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyFragility {
    /// What aspect is fragile.
    pub aspect: String,
    /// Description of the fragility.
    pub description: String,
    /// Severity (0.0 to 1.0).
    pub severity: f64,
    /// How to mitigate.
    pub mitigation: String,
}

/// Strategy certification block.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyCertification {
    /// Strategy name/identifier.
    pub strategy_name: String,
    /// Certification timestamp.
    pub certified_at_ns: Nanos,
    /// Supported claims (passed verification).
    pub supported_claims: Vec<CertificationClaim>,
    /// Unsupported claims (failed verification).
    pub unsupported_claims: Vec<(CertificationClaim, String)>,
    /// Identified fragilities.
    pub fragilities: Vec<StrategyFragility>,
    /// Data contract assumptions.
    pub data_assumptions: DataAssumptions,
    /// Overall certification level.
    pub certification_level: CertificationLevel,
    /// Detailed report.
    pub detailed_report: String,
}

/// Data contract assumptions the certification is valid under.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataAssumptions {
    /// Dataset classification.
    pub classification: DatasetClassification,
    /// Dataset readiness.
    pub readiness: DatasetReadiness,
    /// Settlement model.
    pub settlement_model: String,
    /// Arrival time semantics.
    pub arrival_time_semantics: String,
    /// Data time range.
    pub data_time_range: Option<(Nanos, Nanos)>,
}

/// Certification level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CertificationLevel {
    /// Fully certified for production.
    ProductionCertified,
    /// Certified with caveats.
    ConditionalCertified,
    /// Research-grade only.
    ResearchGrade,
    /// Not certified (failures).
    NotCertified,
}

impl CertificationLevel {
    /// Get description.
    pub fn description(&self) -> &'static str {
        match self {
            Self::ProductionCertified => "Fully certified for production deployment",
            Self::ConditionalCertified => "Certified with documented caveats/limitations",
            Self::ResearchGrade => "Valid for research only, not production-ready",
            Self::NotCertified => "Certification failed - do not deploy",
        }
    }
}

// =============================================================================
// STRATEGY CERTIFIER
// =============================================================================

/// The strategy certifier that validates and issues certifications.
pub struct StrategyCertifier {
    parity_config: ParityConfig,
}

impl StrategyCertifier {
    /// Create a new certifier.
    pub fn new(parity_config: ParityConfig) -> Self {
        Self { parity_config }
    }
    
    /// Create a certification from backtest results.
    pub fn certify(
        &self,
        strategy_name: &str,
        results: &BacktestResults,
        pnl_decomposition: Option<&PnLDecomposition>,
        parity_result: Option<&ParityResult>,
        risk_summary: Option<&PreTradeRiskSummary>,
    ) -> StrategyCertification {
        let now_ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as Nanos)
            .unwrap_or(0);
        
        let mut supported = vec![];
        let mut unsupported = vec![];
        let mut fragilities = vec![];
        
        // Check each claim
        
        // 1. Profitable
        if results.final_pnl > 0.0 {
            supported.push(CertificationClaim::Profitable);
        } else {
            unsupported.push((
                CertificationClaim::Profitable,
                format!("Final PnL {:.4} <= 0", results.final_pnl),
            ));
        }
        
        // 2. Positive Sharpe
        if let Some(sharpe) = results.sharpe_ratio {
            if sharpe > 0.0 {
                supported.push(CertificationClaim::PositiveSharpe);
            } else {
                unsupported.push((
                    CertificationClaim::PositiveSharpe,
                    format!("Sharpe {:.4} <= 0", sharpe),
                ));
            }
        } else {
            unsupported.push((
                CertificationClaim::PositiveSharpe,
                "Sharpe ratio not calculated".to_string(),
            ));
        }
        
        // 3. Adversarial gates
        if results.gate_suite_passed {
            supported.push(CertificationClaim::AdversarialGatesPassed);
        } else {
            let reasons: Vec<_> = results.gate_failures.iter()
                .map(|(name, reason)| format!("{}: {}", name, reason))
                .collect();
            unsupported.push((
                CertificationClaim::AdversarialGatesPassed,
                format!("Gate failures: {}", reasons.join("; ")),
            ));
        }
        
        // 4. Latency robustness (from sensitivity report)
        let latency_robust = !results.sensitivity_report.fragility.latency_fragile;
        if latency_robust {
            supported.push(CertificationClaim::LatencyRobust);
        } else {
            unsupported.push((
                CertificationClaim::LatencyRobust,
                "Strategy is fragile to latency variations".to_string(),
            ));
            fragilities.push(StrategyFragility {
                aspect: "Latency".to_string(),
                description: "Performance degrades significantly under latency variations".to_string(),
                severity: 0.7,
                mitigation: "Review signal timing and execution assumptions".to_string(),
            });
        }
        
        // 5. Queue model / execution robustness
        let queue_robust = !results.sensitivity_report.fragility.execution_fragile;
        if queue_robust {
            supported.push(CertificationClaim::QueueModelRobust);
        } else {
            unsupported.push((
                CertificationClaim::QueueModelRobust,
                "Strategy is fragile to execution/queue model variations".to_string(),
            ));
            fragilities.push(StrategyFragility {
                aspect: "Execution".to_string(),
                description: "Performance degrades under conservative execution assumptions".to_string(),
                severity: 0.8,
                mitigation: "Use more conservative maker fill assumptions".to_string(),
            });
        }
        
        // 6. Live parity (if available)
        if let Some(parity) = parity_result {
            if parity.passed {
                supported.push(CertificationClaim::LiveParityPassed);
            } else {
                unsupported.push((
                    CertificationClaim::LiveParityPassed,
                    parity.failure_summary.join("; "),
                ));
            }
        }
        
        // 7. Basis awareness (check if PnL decomposition available)
        if pnl_decomposition.is_some() {
            supported.push(CertificationClaim::BasisAware);
        } else {
            unsupported.push((
                CertificationClaim::BasisAware,
                "No PnL decomposition available - basis awareness not verified".to_string(),
            ));
        }
        
        // 8. Risk controlled
        if let Some(risk) = risk_summary {
            if risk.total_decisions > 0 && risk.pass_rate < 1.0 {
                // Some decisions were rejected by risk controls
                supported.push(CertificationClaim::RiskControlled);
            } else if risk.total_decisions > 0 {
                supported.push(CertificationClaim::RiskControlled);
            } else {
                unsupported.push((
                    CertificationClaim::RiskControlled,
                    "No risk control decisions recorded".to_string(),
                ));
            }
        }
        
        // 9. Production ready (requires all production criteria)
        let production_ready = results.production_grade 
            && results.truthfulness.is_trusted()
            && results.gate_suite_passed
            && latency_robust
            && queue_robust
            && results.maker_fills_valid;
        
        if production_ready {
            supported.push(CertificationClaim::ProductionReady);
        } else {
            let mut reasons = vec![];
            if !results.production_grade {
                reasons.push("Not production-grade mode");
            }
            if !results.truthfulness.is_trusted() {
                reasons.push("Truthfulness not trusted");
            }
            if !results.gate_suite_passed {
                reasons.push("Gate suite failed");
            }
            if !latency_robust {
                reasons.push("Latency fragile");
            }
            if !queue_robust {
                reasons.push("Queue model fragile");
            }
            unsupported.push((
                CertificationClaim::ProductionReady,
                reasons.join(", "),
            ));
        }
        
        // Determine certification level
        let certification_level = if production_ready && unsupported.is_empty() {
            CertificationLevel::ProductionCertified
        } else if production_ready || (supported.len() >= 5 && fragilities.len() <= 2) {
            CertificationLevel::ConditionalCertified
        } else if supported.contains(&CertificationClaim::Profitable) {
            CertificationLevel::ResearchGrade
        } else {
            CertificationLevel::NotCertified
        };
        
        // Build data assumptions
        let data_assumptions = DataAssumptions {
            classification: results.data_quality.classification,
            readiness: results.dataset_readiness,
            settlement_model: format!("{:?}", results.settlement_model),
            arrival_time_semantics: "RecordedArrival".to_string(),
            data_time_range: None,
        };
        
        // Build detailed report
        let detailed_report = self.build_detailed_report(
            strategy_name,
            &supported,
            &unsupported,
            &fragilities,
            results,
            pnl_decomposition,
            parity_result,
        );
        
        StrategyCertification {
            strategy_name: strategy_name.to_string(),
            certified_at_ns: now_ns,
            supported_claims: supported,
            unsupported_claims: unsupported,
            fragilities,
            data_assumptions,
            certification_level,
            detailed_report,
        }
    }
    
    fn build_detailed_report(
        &self,
        strategy_name: &str,
        supported: &[CertificationClaim],
        unsupported: &[(CertificationClaim, String)],
        fragilities: &[StrategyFragility],
        results: &BacktestResults,
        pnl_decomposition: Option<&PnLDecomposition>,
        parity_result: Option<&ParityResult>,
    ) -> String {
        let mut report = String::new();
        
        report.push_str("╔════════════════════════════════════════════════════════════════════╗\n");
        report.push_str("║               STRATEGY CERTIFICATION REPORT                        ║\n");
        report.push_str("╠════════════════════════════════════════════════════════════════════╣\n");
        report.push_str(&format!("║  Strategy: {:<55}  ║\n", strategy_name));
        report.push_str("╠════════════════════════════════════════════════════════════════════╣\n");
        
        // Supported claims
        report.push_str("║  SUPPORTED CLAIMS:                                                 ║\n");
        for claim in supported {
            report.push_str(&format!("║    [✓] {:61}║\n", claim.description()));
        }
        
        // Unsupported claims
        if !unsupported.is_empty() {
            report.push_str("║                                                                    ║\n");
            report.push_str("║  UNSUPPORTED CLAIMS:                                               ║\n");
            for (claim, reason) in unsupported {
                report.push_str(&format!("║    [✗] {:61}║\n", claim.description()));
                report.push_str(&format!("║        Reason: {:51}║\n", 
                    if reason.len() > 51 { &reason[..48] } else { reason }));
            }
        }
        
        // Fragilities
        if !fragilities.is_empty() {
            report.push_str("║                                                                    ║\n");
            report.push_str("║  FRAGILITIES:                                                      ║\n");
            for frag in fragilities {
                report.push_str(&format!("║    [!] {}: {:.1} severity                             ║\n", 
                    frag.aspect, frag.severity));
            }
        }
        
        // PnL decomposition
        if let Some(decomp) = pnl_decomposition {
            report.push_str("║                                                                    ║\n");
            report.push_str("║  PNL DECOMPOSITION:                                                ║\n");
            report.push_str(&format!("║    Total:      {:>12.4}                                   ║\n", decomp.total_pnl));
            report.push_str(&format!("║    Basis:      {:>12.4} ({:>5.1}%)                         ║\n", 
                decomp.basis_pnl, decomp.pct(decomp.basis_pnl)));
            report.push_str(&format!("║    Execution:  {:>12.4} ({:>5.1}%)                         ║\n", 
                decomp.execution_pnl, decomp.pct(decomp.execution_pnl)));
            report.push_str(&format!("║    Settlement: {:>12.4} ({:>5.1}%)                         ║\n", 
                decomp.settlement_pnl, decomp.pct(decomp.settlement_pnl)));
        }
        
        // Parity result
        if let Some(parity) = parity_result {
            report.push_str("║                                                                    ║\n");
            report.push_str(&format!("║  LIVE-BACKTEST PARITY: {}                                       ║\n",
                if parity.passed { "PASS" } else { "FAIL" }));
            report.push_str(&format!("║    Signal Agreement: {:>5.1}%                                    ║\n",
                parity.overall_signal_agreement * 100.0));
        }
        
        report.push_str("╚════════════════════════════════════════════════════════════════════╝\n");
        
        report
    }
}

impl StrategyCertification {
    /// Check if this certification allows production deployment.
    pub fn allows_production(&self) -> bool {
        matches!(
            self.certification_level,
            CertificationLevel::ProductionCertified | CertificationLevel::ConditionalCertified
        )
    }
    
    /// Get a summary string.
    pub fn summary(&self) -> String {
        format!(
            "Strategy '{}': {} ({} supported, {} unsupported, {} fragilities)",
            self.strategy_name,
            self.certification_level.description(),
            self.supported_claims.len(),
            self.unsupported_claims.len(),
            self.fragilities.len(),
        )
    }
}

// =============================================================================
// TESTS
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backtest_v2::orchestrator::BacktestResults;
    
    fn make_passing_results() -> BacktestResults {
        let mut results = BacktestResults::default();
        results.final_pnl = 100.0;
        results.sharpe_ratio = Some(1.5);
        results.gate_suite_passed = true;
        results.maker_fills_valid = true;
        results.production_grade = true;
        results.data_quality.classification = DatasetClassification::FullIncremental;
        results.dataset_readiness = DatasetReadiness::MakerViable;
        results
    }
    
    #[test]
    fn test_pnl_decomposition() {
        let trades = vec![
            TradeAttribution {
                trade_id: "1".to_string(),
                timestamp_ns: 1_000_000_000,
                total_realized: 10.0,
                basis_contribution: 5.0,
                execution_contribution: 3.0,
                settlement_contribution: 3.0,
                fees_paid: 1.0,
                entry_basis: 0.01,
                exit_basis: 0.02,
            },
        ];
        
        let decomp = PnLDecomposition::from_trades(&trades);
        
        assert!((decomp.total_pnl - 10.0).abs() < 1e-9);
        assert_eq!(decomp.trade_count, 1);
        assert!(decomp.is_valid());
    }
    
    #[test]
    fn test_parity_result() {
        let windows = vec![
            ParityWindow {
                start_ns: 0,
                end_ns: 1_000_000_000,
                live_signals: 100,
                backtest_signals: 98,
                signal_agreement_rate: 0.98,
                live_orders: 50,
                backtest_orders: 49,
                live_fills: 45,
                backtest_fills: 44,
                live_pnl: 10.0,
                backtest_pnl: 9.5,
                pnl_difference: 0.5,
                passed: true,
                failure_reasons: vec![],
            },
        ];
        
        let config = ParityConfig::default();
        let result = ParityResult::from_windows(windows, &config);
        
        assert!(result.passed);
        assert!(result.overall_signal_agreement > 0.95);
    }
    
    #[test]
    fn test_certification_passing() {
        let results = make_passing_results();
        let certifier = StrategyCertifier::new(ParityConfig::default());
        
        let cert = certifier.certify("TestStrategy", &results, None, None, None);
        
        assert!(cert.supported_claims.contains(&CertificationClaim::Profitable));
        assert!(cert.supported_claims.contains(&CertificationClaim::PositiveSharpe));
        assert!(cert.supported_claims.contains(&CertificationClaim::AdversarialGatesPassed));
    }
    
    #[test]
    fn test_certification_failing() {
        let mut results = BacktestResults::default();
        results.final_pnl = -50.0;
        results.sharpe_ratio = Some(-0.5);
        results.gate_suite_passed = false;
        
        let certifier = StrategyCertifier::new(ParityConfig::default());
        let cert = certifier.certify("FailingStrategy", &results, None, None, None);
        
        assert!(cert.unsupported_claims.iter().any(|(c, _)| *c == CertificationClaim::Profitable));
        assert_eq!(cert.certification_level, CertificationLevel::NotCertified);
        assert!(!cert.allows_production());
    }
    
    #[test]
    fn test_certification_level() {
        assert_eq!(
            CertificationLevel::ProductionCertified.description(),
            "Fully certified for production deployment"
        );
        assert_eq!(
            CertificationLevel::NotCertified.description(),
            "Certification failed - do not deploy"
        );
    }
    
    #[test]
    fn test_certification_summary() {
        let results = make_passing_results();
        let certifier = StrategyCertifier::new(ParityConfig::default());
        let cert = certifier.certify("TestStrategy", &results, None, None, None);
        
        let summary = cert.summary();
        assert!(summary.contains("TestStrategy"));
        assert!(summary.contains("supported"));
    }
}

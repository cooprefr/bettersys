//! Risk Management Module
//! Pilot in Command: Risk Engine
//! Mission: Institutional-grade guardrails, calibrated confidence, and drawdown awareness

use anyhow::Result;
use serde::{Deserialize, Serialize};
use statrs::statistics::Statistics;
use std::collections::{HashMap, VecDeque};
use std::ops::Range;

const MAX_KELLY_CAP: f64 = 0.20;
const DRAWNDOWN_THROTTLE_TRIGGER: f64 = 0.08;
const DRAWNDOWN_THROTTLE_RELEASE: f64 = 0.04;

/// Kelly Criterion Calculator for optimal position sizing
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KellyCalculator {
    /// Fractional Kelly multiplier for safety (0.25-0.5x)
    pub fraction: f64,
    /// Bankroll available for trading
    pub bankroll: f64,
    /// Historical win rates
    win_history: VecDeque<bool>,
    /// Maximum history size
    max_history: usize,
}

impl KellyCalculator {
    pub fn new(bankroll: f64, fraction: f64) -> Self {
        Self {
            fraction: fraction.clamp(0.1, 0.5), // Safety bounds
            bankroll,
            win_history: VecDeque::with_capacity(1000),
            max_history: 1000,
        }
    }

    /// Compute the raw Kelly fraction (before safety caps or additional guardrails)
    pub fn raw_fraction(&self, win_probability: f64) -> f64 {
        let p = win_probability.clamp(0.001, 0.999);
        let q = 1.0 - p;
        let b = (1.0 / p) - 1.0;
        if b <= 0.0 {
            return 0.0;
        }
        ((b * p - q) / b).max(0.0)
    }

    pub fn update_history(&mut self, won: bool) {
        if self.win_history.len() >= self.max_history {
            self.win_history.pop_front();
        }
        self.win_history.push_back(won);
    }

    pub fn get_win_rate(&self) -> f64 {
        if self.win_history.is_empty() {
            return 0.5; // Default assumption
        }
        let wins = self.win_history.iter().filter(|&&w| w).count() as f64;
        wins / self.win_history.len() as f64
    }

    pub fn apply_pnl(&mut self, pnl: f64) {
        self.bankroll = (self.bankroll + pnl).max(0.0);
    }
}

/// Value at Risk (VaR) Calculator using historical simulation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaRCalculator {
    /// Historical PnL data
    historical_pnl: VecDeque<f64>,
    /// Confidence level (e.g., 0.95 for 95% VaR)
    confidence_level: f64,
    /// Maximum history size
    max_history: usize,
}

impl VaRCalculator {
    pub fn new(confidence_level: f64) -> Self {
        Self {
            historical_pnl: VecDeque::with_capacity(10000),
            confidence_level: confidence_level.clamp(0.9, 0.99),
            max_history: 10000,
        }
    }

    /// Add a new PnL observation
    pub fn add_pnl(&mut self, pnl: f64) {
        if self.historical_pnl.len() >= self.max_history {
            self.historical_pnl.pop_front();
        }
        self.historical_pnl.push_back(pnl);
    }

    /// Calculate VaR at specified confidence level
    pub fn calculate_var(&self) -> Result<f64> {
        if self.historical_pnl.len() < 100 {
            return Ok(0.0); // Not enough data
        }

        let mut sorted_pnl: Vec<f64> = self.historical_pnl.iter().copied().collect();
        sorted_pnl.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

        let index = ((1.0 - self.confidence_level) * sorted_pnl.len() as f64) as usize;
        Ok(-sorted_pnl[index]) // VaR is typically reported as positive
    }

    /// Calculate Conditional VaR (CVaR) - average of losses beyond VaR
    pub fn calculate_cvar(&self) -> Result<f64> {
        if self.historical_pnl.len() < 100 {
            return Ok(0.0); // Not enough data
        }

        let mut sorted_pnl: Vec<f64> = self.historical_pnl.iter().copied().collect();
        sorted_pnl.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

        let var_index = ((1.0 - self.confidence_level) * sorted_pnl.len() as f64) as usize;

        // Calculate average of all losses worse than VaR
        let tail_losses: Vec<f64> = sorted_pnl[..=var_index].to_vec();
        if tail_losses.is_empty() {
            return Ok(0.0);
        }

        let cvar = tail_losses.iter().sum::<f64>() / tail_losses.len() as f64;
        Ok(-cvar) // CVaR is typically reported as positive
    }

    /// Get current statistics
    pub fn get_stats(&self) -> RiskStats {
        RiskStats {
            var_95: self.calculate_var().unwrap_or(0.0),
            cvar_95: self.calculate_cvar().unwrap_or(0.0),
            sample_size: self.historical_pnl.len(),
            max_loss: if self.historical_pnl.is_empty() {
                0.0
            } else {
                self.historical_pnl
                    .iter()
                    .copied()
                    .fold(f64::INFINITY, f64::min)
            },
            max_gain: if self.historical_pnl.is_empty() {
                0.0
            } else {
                self.historical_pnl
                    .iter()
                    .copied()
                    .fold(f64::NEG_INFINITY, f64::max)
            },
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskStats {
    pub var_95: f64,
    pub cvar_95: f64,
    pub sample_size: usize,
    pub max_loss: f64,
    pub max_gain: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalibrationSummary {
    pub signal_family: String,
    pub version: String,
    pub sample_size: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuardrailState {
    pub bankroll: f64,
    pub base_fraction: f64,
    pub kelly_cap: f64,
    pub regime_risk: f64,
    pub drawdown_pct: f64,
    pub max_drawdown_pct: f64,
    pub drawdown_throttle_active: bool,
    pub calibration_versions: Vec<CalibrationSummary>,
}

#[derive(Debug, Clone)]
struct CalibrationBin {
    range: Range<f64>,
    wins: u32,
    total: u32,
}

impl CalibrationBin {
    fn new(lower: f64, upper: f64) -> Self {
        Self {
            range: lower..upper,
            wins: 0,
            total: 0,
        }
    }

    fn contains(&self, value: f64) -> bool {
        value >= self.range.start && value < self.range.end
    }

    fn observe(&mut self, won: bool) {
        self.total += 1;
        if won {
            self.wins += 1;
        }
    }

    fn calibrated_probability(&self, fallback: f64) -> f64 {
        if self.total < 5 {
            return fallback;
        }
        (self.wins as f64 / self.total as f64).clamp(0.01, 0.99)
    }
}

#[derive(Debug, Clone)]
struct CalibrationModel {
    version: String,
    bins: Vec<CalibrationBin>,
    sample_size: u32,
}

impl CalibrationModel {
    fn new(version: &str, bin_count: usize) -> Self {
        let step = 1.0 / bin_count as f64;
        let mut bins = Vec::with_capacity(bin_count);
        for i in 0..bin_count {
            let lower = i as f64 * step;
            let upper = if i == bin_count - 1 {
                1.0 + f64::EPSILON
            } else {
                (i + 1) as f64 * step
            };
            bins.push(CalibrationBin::new(lower, upper));
        }
        Self {
            version: version.to_string(),
            bins,
            sample_size: 0,
        }
    }

    fn calibrate(&self, raw: f64) -> f64 {
        let fallback = raw.clamp(0.01, 0.99);
        self.bins
            .iter()
            .find(|bin| bin.contains(raw))
            .map(|bin| bin.calibrated_probability(fallback))
            .unwrap_or(fallback)
    }

    fn observe(&mut self, raw: f64, won: bool) {
        if let Some(bin) = self.bins.iter_mut().find(|b| b.contains(raw)) {
            bin.observe(won);
            self.sample_size += 1;
            if self.sample_size % 250 == 0 {
                self.version = format!("iso-v1-{}", self.sample_size);
            }
        }
    }

    fn summary(&self, family: &str) -> CalibrationSummary {
        CalibrationSummary {
            signal_family: family.to_string(),
            version: self.version.clone(),
            sample_size: self.sample_size,
        }
    }
}

#[derive(Debug, Default)]
struct CalibrationRegistry {
    models: HashMap<String, CalibrationModel>,
}

impl CalibrationRegistry {
    fn ensure_model(&mut self, family: &str) -> &mut CalibrationModel {
        self.models
            .entry(family.to_string())
            // Increased resolution to 100 bins (1% steps) for "Analog" risk scoring
            .or_insert_with(|| CalibrationModel::new("iso-v1", 100))
    }

    fn calibrate(&mut self, family: &str, raw: f64) -> CalibrationResult {
        let model = self.ensure_model(family);
        let calibrated = model.calibrate(raw);
        CalibrationResult {
            calibrated,
            version: model.version.clone(),
        }
    }

    fn observe(&mut self, family: &str, raw: f64, won: bool) {
        let model = self.ensure_model(family);
        model.observe(raw, won);
    }

    fn summaries(&self) -> Vec<CalibrationSummary> {
        let mut summaries: Vec<_> = self
            .models
            .iter()
            .map(|(family, model)| model.summary(family))
            .collect();
        summaries.sort_by(|a, b| a.signal_family.cmp(&b.signal_family));
        summaries
    }
}

#[derive(Debug, Clone)]
struct DrawdownMonitor {
    equity: f64,
    peak: f64,
    max_drawdown: f64,
    current_drawdown: f64,
    throttle_active: bool,
}

impl DrawdownMonitor {
    fn new(initial_equity: f64) -> Self {
        Self {
            equity: initial_equity,
            peak: initial_equity,
            max_drawdown: 0.0,
            current_drawdown: 0.0,
            throttle_active: false,
        }
    }

    fn record(&mut self, equity: f64) {
        self.equity = equity.max(0.0);
        if self.equity > self.peak {
            self.peak = self.equity;
        }
        if self.peak > 0.0 {
            self.current_drawdown = ((self.peak - self.equity) / self.peak).clamp(0.0, 1.0);
            if self.current_drawdown > self.max_drawdown {
                self.max_drawdown = self.current_drawdown;
            }
        }

        if self.current_drawdown >= DRAWNDOWN_THROTTLE_TRIGGER {
            self.throttle_active = true;
        } else if self.current_drawdown <= DRAWNDOWN_THROTTLE_RELEASE {
            self.throttle_active = false;
        }
    }

    fn multiplier(&self) -> f64 {
        if self.throttle_active {
            0.5
        } else {
            1.0
        }
    }

    fn drawdown_pct(&self) -> f64 {
        self.current_drawdown
    }

    fn max_drawdown_pct(&self) -> f64 {
        self.max_drawdown
    }

    fn is_throttled(&self) -> bool {
        self.throttle_active
    }
}

#[derive(Debug, Clone)]
pub struct CalibrationResult {
    pub calibrated: f64,
    pub version: String,
}

#[derive(Debug, Clone)]
pub struct RiskInput {
    pub market_probability: f64,
    pub signal_confidence: f64,
    pub market_liquidity: f64,
    pub signal_family: String,
    pub regime_risk: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct TradeTelemetry<'a> {
    pub pnl: f64,
    pub won: bool,
    pub position_size: f64,
    pub signal_family: &'a str,
    pub raw_confidence: f64,
}

/// Integrated Risk Manager
pub struct RiskManager {
    pub kelly: KellyCalculator,
    pub var: VaRCalculator,
    pub var_99: VaRCalculator,
    regime_risk: f64,
    drawdown: DrawdownMonitor,
    calibration: CalibrationRegistry,
}

impl RiskManager {
    pub fn new(bankroll: f64, kelly_fraction: f64) -> Self {
        Self {
            kelly: KellyCalculator::new(bankroll, kelly_fraction),
            var: VaRCalculator::new(0.95),
            var_99: VaRCalculator::new(0.99),
            regime_risk: 1.0,
            drawdown: DrawdownMonitor::new(bankroll),
            calibration: CalibrationRegistry::default(),
        }
    }

    /// Calculate risk-adjusted position size
    pub fn calculate_position(&mut self, input: RiskInput) -> Result<PositionRecommendation> {
        let calibration = self
            .calibration
            .calibrate(&input.signal_family, input.signal_confidence);
        let calibrated_confidence = calibration.calibrated;

        let mut probability = input.market_probability.clamp(0.001, 0.999);
        if (calibrated_confidence - probability).abs() > 0.05 {
            probability = (probability * 0.7 + calibrated_confidence * 0.3).clamp(0.001, 0.999);
        }

        let raw_fraction = self.kelly.raw_fraction(probability);
        let capped_fraction = raw_fraction.min(MAX_KELLY_CAP);
        let fractional = capped_fraction * self.kelly.fraction;

        let requested_regime = input.regime_risk.unwrap_or(1.0).clamp(0.3, 1.0);
        let effective_regime = (self.regime_risk * requested_regime).clamp(0.3, 1.0);

        let liquidity_factor = (input.market_liquidity / 100_000.0).clamp(0.1, 1.0);
        let drawdown_multiplier = self.drawdown.multiplier();

        let mut guardrail_flags = Vec::new();
        if raw_fraction > MAX_KELLY_CAP {
            guardrail_flags.push("kelly_cap".to_string());
        }
        if effective_regime < 1.0 {
            guardrail_flags.push("regime_risk".to_string());
        }
        if drawdown_multiplier < 1.0 {
            guardrail_flags.push("drawdown_throttle".to_string());
        }
        if liquidity_factor < 1.0 {
            guardrail_flags.push("liquidity".to_string());
        }

        let effective_fraction =
            fractional * effective_regime * liquidity_factor * drawdown_multiplier;
        let position_size = self.kelly.bankroll * effective_fraction;

        let var_95 = self.var.calculate_var()?;
        let cvar_95 = self.var.calculate_cvar()?;
        let risk_level = self.classify_risk(position_size, var_95);

        Ok(PositionRecommendation {
            position_size,
            kelly_fraction_raw: raw_fraction,
            kelly_fraction_capped: capped_fraction,
            kelly_fraction_effective: effective_fraction,
            var_95,
            cvar_95,
            risk_level,
            confidence: input.signal_confidence,
            calibrated_confidence,
            calibration_version: calibration.version,
            regime_risk: effective_regime,
            liquidity_factor,
            drawdown_multiplier,
            guardrail_flags,
        })
    }

    fn classify_risk(&self, position: f64, var: f64) -> RiskLevel {
        let bankroll = self.kelly.bankroll;
        let position_pct = if bankroll > 0.0 {
            position / bankroll
        } else {
            0.0
        };
        let var_pct = if bankroll > 0.0 { var / bankroll } else { 0.0 };

        if position_pct > 0.1 || var_pct > 0.05 {
            RiskLevel::High
        } else if position_pct > 0.05 || var_pct > 0.03 {
            RiskLevel::Medium
        } else {
            RiskLevel::Low
        }
    }

    pub fn update_trade_outcome(&mut self, telemetry: TradeTelemetry<'_>) {
        self.var.add_pnl(telemetry.pnl);
        self.var_99.add_pnl(telemetry.pnl);
        self.kelly.update_history(telemetry.won);
        self.kelly.apply_pnl(telemetry.pnl);
        self.drawdown.record(self.kelly.bankroll);
        self.calibration.observe(
            telemetry.signal_family,
            telemetry.raw_confidence,
            telemetry.won,
        );
    }

    pub fn guardrail_state(&self) -> GuardrailState {
        GuardrailState {
            bankroll: self.kelly.bankroll,
            base_fraction: self.kelly.fraction,
            kelly_cap: MAX_KELLY_CAP,
            regime_risk: self.regime_risk,
            drawdown_pct: self.drawdown.drawdown_pct(),
            max_drawdown_pct: self.drawdown.max_drawdown_pct(),
            drawdown_throttle_active: self.drawdown.is_throttled(),
            calibration_versions: self.calibration.summaries(),
        }
    }

    pub fn set_regime_risk(&mut self, value: f64) {
        self.regime_risk = value.clamp(0.3, 1.0);
    }

    pub fn regime_risk(&self) -> f64 {
        self.regime_risk
    }

    /// Get current bankroll
    pub fn get_current_bankroll(&self) -> f64 {
        self.kelly.bankroll
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PositionRecommendation {
    pub position_size: f64,
    pub kelly_fraction_raw: f64,
    pub kelly_fraction_capped: f64,
    pub kelly_fraction_effective: f64,
    pub var_95: f64,
    pub cvar_95: f64,
    pub risk_level: RiskLevel,
    pub confidence: f64,
    pub calibrated_confidence: f64,
    pub calibration_version: String,
    pub regime_risk: f64,
    pub liquidity_factor: f64,
    pub drawdown_multiplier: f64,
    pub guardrail_flags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RiskLevel {
    Low,
    Medium,
    High,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_kelly_fraction_cap() {
        let mut manager = RiskManager::new(10_000.0, 0.5);
        let input = RiskInput {
            market_probability: 0.65,
            signal_confidence: 0.9,
            market_liquidity: 200_000.0,
            signal_family: "test".to_string(),
            regime_risk: Some(1.0),
        };

        let rec = manager.calculate_position(input).expect("calculation");
        assert!(rec.kelly_fraction_capped <= MAX_KELLY_CAP + 1e-6);
        assert!(rec.position_size >= 0.0);
    }

    #[test]
    fn test_drawdown_throttle() {
        let mut manager = RiskManager::new(10_000.0, 0.5);
        manager.drawdown.record(8_500.0); // 15% drawdown triggers throttle
        let input = RiskInput {
            market_probability: 0.6,
            signal_confidence: 0.7,
            market_liquidity: 100_000.0,
            signal_family: "test".to_string(),
            regime_risk: Some(1.0),
        };
        let rec = manager.calculate_position(input).expect("calculation");
        assert!(rec
            .guardrail_flags
            .contains(&"drawdown_throttle".to_string()));
    }

    #[test]
    fn test_calibration_updates_version() {
        let mut manager = RiskManager::new(10_000.0, 0.25);
        for _ in 0..10 {
            manager.update_trade_outcome(TradeTelemetry {
                pnl: 10.0,
                won: true,
                position_size: 100.0,
                signal_family: "test_family",
                raw_confidence: 0.8,
            });
        }

        let state = manager.guardrail_state();
        let summary = state
            .calibration_versions
            .iter()
            .find(|s| s.signal_family == "test_family")
            .unwrap();
        assert!(summary.sample_size > 0);
    }
}

//! Backtesting Framework
//! Pilot in Command: Historical Analysis Engine
//! Mission: Simulate strategies on past data with institutional-grade research hygiene

use crate::models::{MarketSignal, SignalType};
use crate::risk::{RiskInput, RiskManager, TradeTelemetry};
use anyhow::{ensure, Result};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BacktestConfig {
    pub initial_bankroll: f64,
    pub kelly_fraction: f64,
    pub start_date: DateTime<Utc>,
    pub end_date: DateTime<Utc>,
    pub slippage_bps: f64, // Basis points
    pub transaction_cost: f64,
    pub max_positions: usize,
    /// Rolling history window (days) used to train/calibrate before each trade
    pub walk_forward_window_days: i64,
    /// Test segment length (days) that advances the walk-forward cursor
    pub test_window_days: i64,
    /// Embargo gap (hours) applied after the latest training observation
    pub embargo_hours: i64,
    /// Minimum number of training examples required before live trading
    pub min_training_signals: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Trade {
    pub id: String,
    pub signal_type: SignalType,
    pub entry_time: DateTime<Utc>,
    pub exit_time: Option<DateTime<Utc>>,
    pub entry_price: f64,
    pub exit_price: Option<f64>,
    pub position_size: f64,
    pub pnl: Option<f64>,
    pub status: TradeStatus,
    pub calibrated_confidence: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TradeStatus {
    Open,
    Closed,
    Expired,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BacktestResult {
    pub total_pnl: f64,
    pub win_rate: f64,
    pub sharpe_ratio: f64,
    pub max_drawdown: f64,
    pub total_trades: usize,
    pub winning_trades: usize,
    pub losing_trades: usize,
    pub avg_win: f64,
    pub avg_loss: f64,
    pub profit_factor: f64,
    pub trades: Vec<Trade>,
    pub equity_curve: Vec<(DateTime<Utc>, f64)>,
    pub skipped_due_to_history: usize,
    pub skipped_due_to_embargo: usize,
    pub skipped_due_to_outlier: usize,
    pub skipped_due_to_leakage: usize,
    pub training_windows_processed: usize,
}

pub struct BacktestEngine {
    config: BacktestConfig,
    risk_manager: RiskManager,
    trades: Vec<Trade>,
    open_positions: HashMap<String, Trade>,
    equity_curve: Vec<(DateTime<Utc>, f64)>,
    current_equity: f64,
    hygiene_state: ResearchHygieneState,
}

impl BacktestEngine {
    pub fn new(config: BacktestConfig) -> Self {
        let risk_manager = RiskManager::new(config.initial_bankroll, config.kelly_fraction);

        let mut equity_curve = Vec::with_capacity(1024);
        equity_curve.push((config.start_date, config.initial_bankroll));

        Self {
            current_equity: config.initial_bankroll,
            trades: Vec::new(),
            open_positions: HashMap::new(),
            equity_curve,
            risk_manager,
            hygiene_state: ResearchHygieneState::default(),
            config,
        }
    }

    /// Run backtest on historical signals with walk-forward validation and embargo
    pub async fn run(&mut self, signals: Vec<MarketSignal>) -> Result<BacktestResult> {
        tracing::info!(
            "Starting backtest from {} to {}",
            self.config.start_date,
            self.config.end_date
        );

        let prepared = self.prepare_signals(signals)?;
        let mut training_buffer: VecDeque<TimedSignal> = VecDeque::new();

        for timed in prepared.into_iter() {
            self.prune_training_buffer(&mut training_buffer, timed.detected_at);
            self.ensure_monotonic(&timed)?;

            let mut replay = timed.clone();

            if training_buffer.len() < self.config.min_training_signals {
                self.hygiene_state.skipped_due_to_history += 1;
                training_buffer.push_back(replay);
                continue;
            }

            if self.should_embargo(&training_buffer, timed.detected_at) {
                self.hygiene_state.skipped_due_to_embargo += 1;
                training_buffer.push_back(replay);
                continue;
            }

            let stats = TrainingStats::from_buffer(&training_buffer);
            self.hygiene_state.update_from_stats(&stats);

            match self.leakage_permitted(&timed, &stats) {
                Ok(true) => {}
                Ok(false) => {
                    self.hygiene_state.skipped_due_to_leakage += 1;
                    training_buffer.push_back(replay);
                    continue;
                }
                Err(err) => {
                    tracing::warn!("Leakage guard triggered: {}", err);
                    self.hygiene_state.skipped_due_to_leakage += 1;
                    training_buffer.push_back(replay);
                    continue;
                }
            }

            if self.is_confidence_outlier(&timed, &stats) {
                self.hygiene_state.skipped_due_to_outlier += 1;
                training_buffer.push_back(replay);
                continue;
            }

            self.process_signal(&timed.signal, &stats).await?;
            training_buffer.push_back(replay);
        }

        self.close_all_positions()?;
        let result = self.calculate_results()?;

        tracing::info!(
            "Backtest complete: {:.2}% win rate, {:.2} Sharpe, PnL ${:.2} (skipped history: {}, embargo: {}, outliers: {}, leakage: {})",
            result.win_rate * 100.0,
            result.sharpe_ratio,
            result.total_pnl,
            result.skipped_due_to_history,
            result.skipped_due_to_embargo,
            result.skipped_due_to_outlier,
            result.skipped_due_to_leakage
        );

        Ok(result)
    }

    fn prepare_signals(&self, signals: Vec<MarketSignal>) -> Result<Vec<TimedSignal>> {
        let mut prepared: Vec<TimedSignal> = signals
            .into_iter()
            .filter_map(|signal| {
                DateTime::parse_from_rfc3339(&signal.detected_at)
                    .map(|dt| (signal, dt.with_timezone(&Utc)))
                    .ok()
            })
            .filter(|(_, detected_at)| {
                *detected_at >= self.config.start_date && *detected_at <= self.config.end_date
            })
            .map(|(signal, detected_at)| {
                let observed_at = signal
                    .details
                    .observed_timestamp
                    .as_deref()
                    .and_then(|ts| DateTime::parse_from_rfc3339(ts).ok())
                    .map(|dt| dt.with_timezone(&Utc));

                TimedSignal {
                    signal,
                    detected_at,
                    observed_at,
                }
            })
            .collect();

        prepared.sort_by(|a, b| a.detected_at.cmp(&b.detected_at));
        Ok(prepared)
    }

    fn ensure_monotonic(&mut self, timed: &TimedSignal) -> Result<()> {
        if let Some(previous) = self.hygiene_state.last_detected {
            ensure!(
                timed.detected_at >= previous,
                "Signals not sorted by detection time ({} < {})",
                timed.detected_at,
                previous
            );
        }
        self.hygiene_state.last_detected = Some(timed.detected_at);
        Ok(())
    }

    fn should_embargo(&self, buffer: &VecDeque<TimedSignal>, current_time: DateTime<Utc>) -> bool {
        if buffer.is_empty() || self.config.embargo_hours <= 0 {
            return false;
        }

        let embargo = Duration::hours(self.config.embargo_hours.max(0));
        if let Some(last_training) = buffer.back() {
            current_time.signed_duration_since(last_training.detected_at) < embargo
        } else {
            false
        }
    }

    fn prune_training_buffer(
        &self,
        buffer: &mut VecDeque<TimedSignal>,
        current_time: DateTime<Utc>,
    ) {
        if self.config.walk_forward_window_days <= 0 {
            return;
        }
        let window = Duration::days(self.config.walk_forward_window_days);
        while let Some(front) = buffer.front() {
            if current_time.signed_duration_since(front.detected_at) > window {
                buffer.pop_front();
            } else {
                break;
            }
        }
    }

    async fn process_signal(&mut self, signal: &MarketSignal, stats: &TrainingStats) -> Result<()> {
        self.check_exit_conditions(signal)?;

        if self.open_positions.len() >= self.config.max_positions {
            return Ok(());
        }

        let raw_confidence = signal.confidence;
        let calibrated_confidence = self.calibrate_confidence(raw_confidence, stats);

        let liquidity = stats.estimated_liquidity().max(1_000.0);
        let signal_family = signal.signal_family();
        let risk_input = RiskInput {
            market_probability: raw_confidence,
            signal_confidence: calibrated_confidence,
            market_liquidity: liquidity,
            signal_family,
            regime_risk: None,
        };
        let position_rec = self.risk_manager.calculate_position(risk_input)?;

        if position_rec.position_size <= 0.0 {
            return Ok(());
        }

        let slippage = self.config.slippage_bps / 10_000.0;
        let entry_price = (position_rec.calibrated_confidence * (1.0 + slippage)).clamp(0.0, 1.0);

        let entry_time = DateTime::parse_from_rfc3339(&signal.detected_at)?.with_timezone(&Utc);

        let trade = Trade {
            id: format!("{}_{}", signal.market_slug, signal.detected_at),
            signal_type: signal.signal_type.clone(),
            entry_time,
            exit_time: None,
            entry_price,
            exit_price: None,
            position_size: position_rec.position_size,
            pnl: None,
            status: TradeStatus::Open,
            calibrated_confidence: position_rec.calibrated_confidence,
        };

        let cost = position_rec.position_size + self.config.transaction_cost;
        if cost > self.current_equity {
            return Ok(());
        }

        self.current_equity -= cost;
        self.open_positions.insert(trade.id.clone(), trade);

        Ok(())
    }

    fn check_exit_conditions(&mut self, signal: &MarketSignal) -> Result<()> {
        let mut closed_trades = Vec::new();

        for (id, trade) in self.open_positions.iter_mut() {
            let should_exit = match &trade.signal_type {
                SignalType::MarketExpiryEdge { .. } => true,
                SignalType::PriceDeviation { .. } => {
                    signal.confidence < 0.55 && signal.confidence > 0.45
                }
                _ => false,
            };

            if should_exit {
                let slippage = self.config.slippage_bps / 10_000.0;
                let exit_price = (signal.confidence * (1.0 - slippage)).clamp(0.0, 1.0);
                let pnl = (exit_price - trade.entry_price) * trade.position_size
                    - self.config.transaction_cost;

                trade.exit_time =
                    Some(DateTime::parse_from_rfc3339(&signal.detected_at)?.with_timezone(&Utc));
                trade.exit_price = Some(exit_price);
                trade.pnl = Some(pnl);
                trade.status = if pnl >= 0.0 {
                    TradeStatus::Closed
                } else {
                    TradeStatus::Expired
                };

                self.current_equity += trade.position_size + pnl;
                let family = trade.signal_type.family();
                self.risk_manager.update_trade_outcome(TradeTelemetry {
                    pnl,
                    won: pnl >= 0.0,
                    position_size: trade.position_size,
                    signal_family: family,
                    raw_confidence: trade.calibrated_confidence,
                });
                closed_trades.push((id.clone(), trade.clone()));
            }
        }

        for (id, trade) in closed_trades {
            self.open_positions.remove(&id);
            self.trades.push(trade);
        }

        if let Ok(dt) = DateTime::parse_from_rfc3339(&signal.detected_at) {
            self.equity_curve
                .push((dt.with_timezone(&Utc), self.current_equity));
        }

        Ok(())
    }

    fn close_all_positions(&mut self) -> Result<()> {
        for (_id, mut trade) in self.open_positions.drain() {
            trade.exit_time = Some(self.config.end_date);
            trade.exit_price = Some(trade.entry_price);
            trade.pnl = Some(-self.config.transaction_cost);
            trade.status = TradeStatus::Expired;
            self.trades.push(trade);
        }
        Ok(())
    }

    fn calculate_results(&self) -> Result<BacktestResult> {
        let total_trades = self.trades.len();
        let winning_trades = self
            .trades
            .iter()
            .filter(|t| t.pnl.unwrap_or(0.0) > 0.0)
            .count();
        let losing_trades = self
            .trades
            .iter()
            .filter(|t| t.pnl.unwrap_or(0.0) <= 0.0)
            .count();

        let total_pnl: f64 = self.trades.iter().map(|t| t.pnl.unwrap_or(0.0)).sum();

        let win_rate = if total_trades > 0 {
            winning_trades as f64 / total_trades as f64
        } else {
            0.0
        };

        let wins: Vec<f64> = self
            .trades
            .iter()
            .filter_map(|t| t.pnl.filter(|&pnl| pnl > 0.0))
            .collect();
        let losses: Vec<f64> = self
            .trades
            .iter()
            .filter_map(|t| t.pnl.filter(|&pnl| pnl < 0.0))
            .map(|pnl| pnl.abs())
            .collect();

        let avg_win = if !wins.is_empty() {
            wins.iter().sum::<f64>() / wins.len() as f64
        } else {
            0.0
        };
        let avg_loss = if !losses.is_empty() {
            losses.iter().sum::<f64>() / losses.len() as f64
        } else {
            0.0
        };

        let profit_factor = if avg_loss > f64::EPSILON {
            (avg_win * winning_trades as f64) / (avg_loss * losing_trades.max(1) as f64)
        } else {
            f64::INFINITY
        };

        let returns = self.calculate_returns();
        let sharpe_ratio = self.calculate_sharpe(&returns);
        let max_drawdown = self.calculate_max_drawdown();

        Ok(BacktestResult {
            total_pnl,
            win_rate,
            sharpe_ratio,
            max_drawdown,
            total_trades,
            winning_trades,
            losing_trades,
            avg_win,
            avg_loss,
            profit_factor,
            trades: self.trades.clone(),
            equity_curve: self.equity_curve.clone(),
            skipped_due_to_history: self.hygiene_state.skipped_due_to_history,
            skipped_due_to_embargo: self.hygiene_state.skipped_due_to_embargo,
            skipped_due_to_outlier: self.hygiene_state.skipped_due_to_outlier,
            skipped_due_to_leakage: self.hygiene_state.skipped_due_to_leakage,
            training_windows_processed: self.hygiene_state.training_windows_processed,
        })
    }

    fn calculate_returns(&self) -> Vec<f64> {
        let mut returns = Vec::new();
        for window in self.equity_curve.windows(2) {
            let prev_equity = window[0].1;
            let curr_equity = window[1].1;
            if prev_equity.abs() > f64::EPSILON {
                returns.push((curr_equity - prev_equity) / prev_equity);
            }
        }
        returns
    }

    fn calculate_sharpe(&self, returns: &[f64]) -> f64 {
        if returns.is_empty() {
            return 0.0;
        }

        let mean_return = returns.iter().sum::<f64>() / returns.len() as f64;
        let variance = returns
            .iter()
            .map(|r| (r - mean_return).powi(2))
            .sum::<f64>()
            / returns.len() as f64;

        let std_dev = variance.sqrt();
        if std_dev > f64::EPSILON {
            mean_return * 252.0_f64.sqrt() / std_dev
        } else {
            0.0
        }
    }

    fn calculate_max_drawdown(&self) -> f64 {
        if self.equity_curve.is_empty() {
            return 0.0;
        }

        let mut peak = self.equity_curve[0].1;
        let mut max_drawdown = 0.0;
        for (_, equity) in &self.equity_curve {
            if *equity > peak {
                peak = *equity;
            }
            if peak > 0.0 {
                let drawdown = (peak - equity) / peak;
                if drawdown > max_drawdown {
                    max_drawdown = drawdown;
                }
            }
        }
        max_drawdown
    }

    fn calibrate_confidence(&self, raw: f64, stats: &TrainingStats) -> f64 {
        let mut min_bound = 0.05;
        let mut max_bound = 0.95;

        if stats.std_confidence > 1e-6 {
            min_bound = (stats.mean_confidence - 3.0 * stats.std_confidence).clamp(0.05, 0.95);
            max_bound = (stats.mean_confidence + 3.0 * stats.std_confidence).clamp(0.05, 0.95);
        }

        raw.clamp(min_bound, max_bound)
    }

    fn is_confidence_outlier(&self, timed: &TimedSignal, stats: &TrainingStats) -> bool {
        if stats.std_confidence < 1e-6 {
            return false;
        }

        let z_score =
            (timed.signal.confidence - stats.mean_confidence) / stats.std_confidence.max(1e-6);
        z_score.abs() > 4.0
    }

    fn leakage_permitted(&mut self, timed: &TimedSignal, stats: &TrainingStats) -> Result<bool> {
        if let Some(observed_at) = timed.observed_at {
            if observed_at > timed.detected_at {
                return Ok(false);
            }

            if let Some(latest_training_obs) = stats.latest_observed {
                if observed_at
                    < latest_training_obs - Duration::hours(self.config.embargo_hours.max(0))
                {
                    return Ok(false);
                }
            }

            self.hygiene_state.last_observed = Some(observed_at);
        }

        Ok(true)
    }
}

#[derive(Clone)]
struct TimedSignal {
    signal: MarketSignal,
    detected_at: DateTime<Utc>,
    observed_at: Option<DateTime<Utc>>,
}

struct TrainingStats {
    mean_confidence: f64,
    std_confidence: f64,
    count: usize,
    latest_detected: DateTime<Utc>,
    latest_observed: Option<DateTime<Utc>>,
    avg_liquidity: f64,
}

impl TrainingStats {
    fn from_buffer(buffer: &VecDeque<TimedSignal>) -> Self {
        let count = buffer.len().max(1);
        let mut sum_confidence = 0.0;
        let mut sum_confidence_sq = 0.0;
        let mut latest_detected = buffer
            .back()
            .map(|s| s.detected_at)
            .unwrap_or_else(Utc::now);
        let mut latest_observed: Option<DateTime<Utc>> = None;
        let mut sum_liquidity = 0.0;

        for timed in buffer.iter() {
            let confidence = timed.signal.confidence;
            sum_confidence += confidence;
            sum_confidence_sq += confidence * confidence;
            if timed.detected_at > latest_detected {
                latest_detected = timed.detected_at;
            }
            if let Some(observed) = timed.observed_at {
                let next = match latest_observed {
                    Some(curr) => curr.max(observed),
                    None => observed,
                };
                latest_observed = Some(next);
            }
            sum_liquidity += timed.signal.details.liquidity;
        }

        let mean_confidence = sum_confidence / count as f64;
        let variance = if count > 1 {
            (sum_confidence_sq / count as f64) - mean_confidence.powi(2)
        } else {
            0.0
        };

        let std_confidence = variance.max(0.0).sqrt();
        let avg_liquidity = sum_liquidity / count as f64;

        Self {
            mean_confidence,
            std_confidence,
            count,
            latest_detected,
            latest_observed,
            avg_liquidity,
        }
    }

    fn estimated_liquidity(&self) -> f64 {
        self.avg_liquidity.max(0.0)
    }
}

#[derive(Default)]
struct ResearchHygieneState {
    last_detected: Option<DateTime<Utc>>,
    last_observed: Option<DateTime<Utc>>,
    skipped_due_to_history: usize,
    skipped_due_to_embargo: usize,
    skipped_due_to_outlier: usize,
    skipped_due_to_leakage: usize,
    training_windows_processed: usize,
}

impl ResearchHygieneState {
    fn update_from_stats(&mut self, stats: &TrainingStats) {
        self.training_windows_processed += 1;
        if let Some(observed) = stats.latest_observed {
            self.last_observed = Some(
                self.last_observed
                    .map_or(observed, |curr| curr.max(observed)),
            );
        }
    }
}

/// Strategy optimizer using genetic algorithms (placeholder)
pub struct StrategyOptimizer {
    population_size: usize,
    generations: usize,
    mutation_rate: f64,
}

impl StrategyOptimizer {
    pub fn new() -> Self {
        Self {
            population_size: 50,
            generations: 100,
            mutation_rate: 0.1,
        }
    }

    pub async fn optimize(
        &self,
        _signals: Vec<MarketSignal>,
        base_config: BacktestConfig,
    ) -> Result<BacktestConfig> {
        tracing::info!(
            "Starting strategy optimization with {} generations",
            self.generations
        );
        Ok(base_config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_backtest_engine_walk_forward_setup() {
        let config = BacktestConfig {
            initial_bankroll: 10_000.0,
            kelly_fraction: 0.25,
            start_date: Utc::now() - Duration::days(60),
            end_date: Utc::now(),
            slippage_bps: 10.0,
            transaction_cost: 1.0,
            max_positions: 5,
            walk_forward_window_days: 30,
            test_window_days: 7,
            embargo_hours: 12,
            min_training_signals: 10,
        };

        let mut engine = BacktestEngine::new(config);
        let signals: Vec<MarketSignal> = Vec::new();
        let result = engine.run(signals).await.unwrap();
        assert_eq!(result.total_trades, 0);
        assert_eq!(result.training_windows_processed, 0);
        assert_eq!(result.skipped_due_to_history, 0);
    }
}

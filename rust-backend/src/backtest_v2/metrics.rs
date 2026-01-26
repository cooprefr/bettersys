//! Evaluation Metrics and Reporting
//!
//! Computes comprehensive backtest metrics including latency, fill rates,
//! adverse selection, slippage, and tail risk measures.

use crate::backtest_v2::clock::Nanos;
use crate::backtest_v2::events::{OrderId, Price, Side, Size};
use crate::backtest_v2::portfolio::{MarketId, Outcome, Portfolio, TokenId};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

// ============================================================================
// Latency Metrics
// ============================================================================

/// Latency sample for a single operation.
#[derive(Debug, Clone, Copy)]
pub struct LatencySample {
    pub timestamp: Nanos,
    pub latency_ns: Nanos,
}

/// Latency tracker for a specific stage.
#[derive(Debug, Clone, Default)]
pub struct LatencyTracker {
    samples: Vec<Nanos>,
}

impl LatencyTracker {
    pub fn record(&mut self, latency_ns: Nanos) {
        self.samples.push(latency_ns);
    }

    pub fn count(&self) -> usize {
        self.samples.len()
    }

    pub fn percentiles(&self) -> LatencyPercentiles {
        if self.samples.is_empty() {
            return LatencyPercentiles::default();
        }

        let mut sorted = self.samples.clone();
        sorted.sort_unstable();
        let n = sorted.len();

        LatencyPercentiles {
            count: n,
            min: sorted[0],
            p50: sorted[n / 2],
            p90: sorted[(n * 90) / 100],
            p95: sorted[(n * 95) / 100],
            p99: sorted[(n * 99) / 100],
            max: sorted[n - 1],
            mean: sorted.iter().sum::<i64>() / n as i64,
        }
    }
}

/// Latency percentiles.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LatencyPercentiles {
    pub count: usize,
    pub min: Nanos,
    pub p50: Nanos,
    pub p90: Nanos,
    pub p95: Nanos,
    pub p99: Nanos,
    pub max: Nanos,
    pub mean: Nanos,
}

impl LatencyPercentiles {
    pub fn to_micros(&self) -> LatencyPercentilesUs {
        LatencyPercentilesUs {
            count: self.count,
            min: self.min as f64 / 1_000.0,
            p50: self.p50 as f64 / 1_000.0,
            p90: self.p90 as f64 / 1_000.0,
            p95: self.p95 as f64 / 1_000.0,
            p99: self.p99 as f64 / 1_000.0,
            max: self.max as f64 / 1_000.0,
            mean: self.mean as f64 / 1_000.0,
        }
    }
}

/// Latency percentiles in microseconds for display.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LatencyPercentilesUs {
    pub count: usize,
    pub min: f64,
    pub p50: f64,
    pub p90: f64,
    pub p95: f64,
    pub p99: f64,
    pub max: f64,
    pub mean: f64,
}

/// Multi-stage latency metrics.
#[derive(Debug, Clone, Default)]
pub struct LatencyMetrics {
    /// Order submission to ack.
    pub order_to_ack: LatencyTracker,
    /// Order ack to first fill.
    pub ack_to_fill: LatencyTracker,
    /// Cancel request to ack.
    pub cancel_to_ack: LatencyTracker,
    /// Market data processing.
    pub market_data: LatencyTracker,
    /// Strategy computation time.
    pub strategy: LatencyTracker,
    /// Total round-trip (order to fill).
    pub round_trip: LatencyTracker,
}

impl LatencyMetrics {
    pub fn summary(&self) -> LatencyMetricsSummary {
        LatencyMetricsSummary {
            order_to_ack: self.order_to_ack.percentiles().to_micros(),
            ack_to_fill: self.ack_to_fill.percentiles().to_micros(),
            cancel_to_ack: self.cancel_to_ack.percentiles().to_micros(),
            market_data: self.market_data.percentiles().to_micros(),
            strategy: self.strategy.percentiles().to_micros(),
            round_trip: self.round_trip.percentiles().to_micros(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LatencyMetricsSummary {
    pub order_to_ack: LatencyPercentilesUs,
    pub ack_to_fill: LatencyPercentilesUs,
    pub cancel_to_ack: LatencyPercentilesUs,
    pub market_data: LatencyPercentilesUs,
    pub strategy: LatencyPercentilesUs,
    pub round_trip: LatencyPercentilesUs,
}

// ============================================================================
// Fill Metrics
// ============================================================================

/// Fill record for analysis.
#[derive(Debug, Clone)]
pub struct FillRecord {
    pub order_id: OrderId,
    pub market_id: MarketId,
    pub outcome: Outcome,
    pub side: Side,
    pub price: Price,
    pub size: Size,
    pub is_maker: bool,
    pub fee: f64,
    pub timestamp: Nanos,
    pub order_sent_at: Nanos,
    pub order_acked_at: Option<Nanos>,
    pub mid_at_fill: Option<Price>,
    pub mid_at_order: Option<Price>,
    /// Price at +Δt after fill (for adverse selection).
    pub price_after: HashMap<Nanos, Price>,
}

impl FillRecord {
    /// Slippage from mid price at order time.
    pub fn slippage_from_order_mid(&self) -> Option<f64> {
        self.mid_at_order.map(|mid| match self.side {
            Side::Buy => self.price - mid,
            Side::Sell => mid - self.price,
        })
    }

    /// Slippage from mid price at fill time.
    pub fn slippage_from_fill_mid(&self) -> Option<f64> {
        self.mid_at_fill.map(|mid| match self.side {
            Side::Buy => self.price - mid,
            Side::Sell => mid - self.price,
        })
    }

    /// Time from order to fill.
    pub fn time_to_fill(&self) -> Nanos {
        self.timestamp - self.order_sent_at
    }

    /// Time in queue (ack to fill).
    pub fn time_in_queue(&self) -> Option<Nanos> {
        self.order_acked_at.map(|ack| self.timestamp - ack)
    }
}

/// Fill metrics aggregator.
#[derive(Debug, Clone, Default)]
pub struct FillMetrics {
    fills: Vec<FillRecord>,
    orders_sent: u64,
    orders_filled: u64,
    orders_partially_filled: u64,
    orders_cancelled: u64,
    orders_rejected: u64,
    cancels_sent: u64,
    cancels_succeeded: u64,
    cancel_fill_races: u64,
    cancel_fill_race_fills: u64,
}

impl FillMetrics {
    pub fn record_fill(&mut self, fill: FillRecord) {
        self.fills.push(fill);
    }

    pub fn record_order_sent(&mut self) {
        self.orders_sent += 1;
    }

    pub fn record_order_filled(&mut self) {
        self.orders_filled += 1;
    }

    pub fn record_order_partially_filled(&mut self) {
        self.orders_partially_filled += 1;
    }

    pub fn record_order_cancelled(&mut self) {
        self.orders_cancelled += 1;
    }

    pub fn record_order_rejected(&mut self) {
        self.orders_rejected += 1;
    }

    pub fn record_cancel_sent(&mut self) {
        self.cancels_sent += 1;
    }

    pub fn record_cancel_succeeded(&mut self) {
        self.cancels_succeeded += 1;
    }

    pub fn record_cancel_fill_race(&mut self, filled: bool) {
        self.cancel_fill_races += 1;
        if filled {
            self.cancel_fill_race_fills += 1;
        }
    }

    pub fn fill_rate(&self) -> f64 {
        if self.orders_sent == 0 {
            return 0.0;
        }
        (self.orders_filled + self.orders_partially_filled) as f64 / self.orders_sent as f64
    }

    pub fn cancel_rate(&self) -> f64 {
        if self.orders_sent == 0 {
            return 0.0;
        }
        self.orders_cancelled as f64 / self.orders_sent as f64
    }

    pub fn reject_rate(&self) -> f64 {
        if self.orders_sent == 0 {
            return 0.0;
        }
        self.orders_rejected as f64 / self.orders_sent as f64
    }

    pub fn cancel_success_rate(&self) -> f64 {
        if self.cancels_sent == 0 {
            return 0.0;
        }
        self.cancels_succeeded as f64 / self.cancels_sent as f64
    }

    pub fn cancel_fill_race_rate(&self) -> f64 {
        if self.cancels_sent == 0 {
            return 0.0;
        }
        self.cancel_fill_races as f64 / self.cancels_sent as f64
    }

    pub fn maker_taker_mix(&self) -> (f64, f64) {
        if self.fills.is_empty() {
            return (0.0, 0.0);
        }
        let maker_count = self.fills.iter().filter(|f| f.is_maker).count();
        let taker_count = self.fills.len() - maker_count;
        let total = self.fills.len() as f64;
        (maker_count as f64 / total, taker_count as f64 / total)
    }

    pub fn maker_taker_volume(&self) -> (f64, f64) {
        let mut maker_vol = 0.0;
        let mut taker_vol = 0.0;
        for fill in &self.fills {
            let notional = fill.size * fill.price;
            if fill.is_maker {
                maker_vol += notional;
            } else {
                taker_vol += notional;
            }
        }
        (maker_vol, taker_vol)
    }

    pub fn total_volume(&self) -> f64 {
        self.fills.iter().map(|f| f.size * f.price).sum()
    }

    pub fn total_fees(&self) -> f64 {
        self.fills.iter().map(|f| f.fee).sum()
    }

    pub fn time_in_queue_percentiles(&self) -> LatencyPercentiles {
        let mut tracker = LatencyTracker::default();
        for fill in &self.fills {
            if let Some(tiq) = fill.time_in_queue() {
                tracker.record(tiq);
            }
        }
        tracker.percentiles()
    }

    pub fn summary(&self) -> FillMetricsSummary {
        let (maker_pct, taker_pct) = self.maker_taker_mix();
        let (maker_vol, taker_vol) = self.maker_taker_volume();

        FillMetricsSummary {
            orders_sent: self.orders_sent,
            orders_filled: self.orders_filled,
            orders_partially_filled: self.orders_partially_filled,
            orders_cancelled: self.orders_cancelled,
            orders_rejected: self.orders_rejected,
            fill_rate: self.fill_rate(),
            cancel_rate: self.cancel_rate(),
            reject_rate: self.reject_rate(),
            cancel_success_rate: self.cancel_success_rate(),
            cancel_fill_race_rate: self.cancel_fill_race_rate(),
            cancel_fill_races: self.cancel_fill_races,
            cancel_fill_race_fills: self.cancel_fill_race_fills,
            maker_pct,
            taker_pct,
            maker_volume: maker_vol,
            taker_volume: taker_vol,
            total_volume: self.total_volume(),
            total_fees: self.total_fees(),
            fill_count: self.fills.len(),
            time_in_queue: self.time_in_queue_percentiles().to_micros(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FillMetricsSummary {
    pub orders_sent: u64,
    pub orders_filled: u64,
    pub orders_partially_filled: u64,
    pub orders_cancelled: u64,
    pub orders_rejected: u64,
    pub fill_rate: f64,
    pub cancel_rate: f64,
    pub reject_rate: f64,
    pub cancel_success_rate: f64,
    pub cancel_fill_race_rate: f64,
    pub cancel_fill_races: u64,
    pub cancel_fill_race_fills: u64,
    pub maker_pct: f64,
    pub taker_pct: f64,
    pub maker_volume: f64,
    pub taker_volume: f64,
    pub total_volume: f64,
    pub total_fees: f64,
    pub fill_count: usize,
    pub time_in_queue: LatencyPercentilesUs,
}

// ============================================================================
// Slippage Metrics
// ============================================================================

/// Slippage tracker.
#[derive(Debug, Clone, Default)]
pub struct SlippageMetrics {
    samples: Vec<f64>,
    by_side: HashMap<Side, Vec<f64>>,
}

impl SlippageMetrics {
    pub fn record(&mut self, side: Side, slippage: f64) {
        self.samples.push(slippage);
        self.by_side.entry(side).or_default().push(slippage);
    }

    pub fn from_fills(fills: &[FillRecord]) -> Self {
        let mut metrics = Self::default();
        for fill in fills {
            if let Some(slip) = fill.slippage_from_fill_mid() {
                metrics.record(fill.side, slip);
            }
        }
        metrics
    }

    pub fn summary(&self) -> SlippageSummary {
        SlippageSummary {
            overall: Self::compute_stats(&self.samples),
            buy: Self::compute_stats(self.by_side.get(&Side::Buy).unwrap_or(&vec![])),
            sell: Self::compute_stats(self.by_side.get(&Side::Sell).unwrap_or(&vec![])),
        }
    }

    fn compute_stats(samples: &[f64]) -> SlippageStats {
        if samples.is_empty() {
            return SlippageStats::default();
        }

        let n = samples.len();
        let mean = samples.iter().sum::<f64>() / n as f64;
        let variance = samples.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / n as f64;
        let std_dev = variance.sqrt();

        let mut sorted = samples.to_vec();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());

        SlippageStats {
            count: n,
            mean,
            std_dev,
            min: sorted[0],
            max: sorted[n - 1],
            median: sorted[n / 2],
            total: samples.iter().sum(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SlippageSummary {
    pub overall: SlippageStats,
    pub buy: SlippageStats,
    pub sell: SlippageStats,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SlippageStats {
    pub count: usize,
    pub mean: f64,
    pub std_dev: f64,
    pub min: f64,
    pub max: f64,
    pub median: f64,
    pub total: f64,
}

// ============================================================================
// Adverse Selection Metrics
// ============================================================================

/// Adverse selection analysis (PnL at +Δt after fill).
#[derive(Debug, Clone, Default)]
pub struct AdverseSelectionMetrics {
    /// PnL samples at different time horizons (ns -> samples).
    pnl_by_horizon: BTreeMap<Nanos, Vec<f64>>,
}

impl AdverseSelectionMetrics {
    /// Standard horizons to track.
    pub fn standard_horizons() -> Vec<Nanos> {
        vec![
            100_000_000,     // 100ms
            500_000_000,     // 500ms
            1_000_000_000,   // 1s
            5_000_000_000,   // 5s
            10_000_000_000,  // 10s
            60_000_000_000,  // 1min
            300_000_000_000, // 5min
        ]
    }

    pub fn record(&mut self, fill: &FillRecord) {
        for (&horizon, &price_after) in &fill.price_after {
            // Calculate PnL: price movement from fill
            let pnl = match fill.side {
                Side::Buy => price_after - fill.price,
                Side::Sell => fill.price - price_after,
            };
            self.pnl_by_horizon
                .entry(horizon)
                .or_default()
                .push(pnl * fill.size);
        }
    }

    pub fn summary(&self) -> Vec<AdverseSelectionAtHorizon> {
        let mut result = Vec::new();
        for (&horizon, samples) in &self.pnl_by_horizon {
            if samples.is_empty() {
                continue;
            }

            let n = samples.len();
            let mean = samples.iter().sum::<f64>() / n as f64;
            let positive = samples.iter().filter(|&&x| x > 0.0).count();

            result.push(AdverseSelectionAtHorizon {
                horizon_ns: horizon,
                horizon_label: format_duration(horizon),
                sample_count: n,
                mean_pnl: mean,
                total_pnl: samples.iter().sum(),
                win_rate: positive as f64 / n as f64,
            });
        }
        result
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdverseSelectionAtHorizon {
    pub horizon_ns: Nanos,
    pub horizon_label: String,
    pub sample_count: usize,
    pub mean_pnl: f64,
    pub total_pnl: f64,
    pub win_rate: f64,
}

// ============================================================================
// Tail Risk Metrics
// ============================================================================

/// PnL sample for tail risk analysis.
#[derive(Debug, Clone, Copy)]
pub struct PnlSample {
    pub timestamp: Nanos,
    pub pnl: f64,
    pub equity: f64,
}

/// Tail risk tracker.
#[derive(Debug, Clone, Default)]
pub struct TailRiskMetrics {
    /// Equity curve samples.
    equity_curve: Vec<PnlSample>,
    /// Rolling window PnL (window_ns -> worst samples).
    rolling_worst: BTreeMap<Nanos, Vec<f64>>,
}

impl TailRiskMetrics {
    pub fn record_equity(&mut self, timestamp: Nanos, equity: f64) {
        let pnl = if let Some(last) = self.equity_curve.last() {
            equity - last.equity
        } else {
            0.0
        };
        self.equity_curve.push(PnlSample {
            timestamp,
            pnl,
            equity,
        });
    }

    /// Compute rolling worst PnL for given window sizes.
    pub fn compute_rolling_worst(&mut self, window_sizes_ns: &[Nanos]) {
        for &window in window_sizes_ns {
            let mut worst_pnls = Vec::new();

            for i in 0..self.equity_curve.len() {
                let start_time = self.equity_curve[i].timestamp;
                let end_time = start_time + window;

                // Find cumulative PnL over window
                let start_equity = self.equity_curve[i].equity;
                let mut min_equity = start_equity;

                for j in i..self.equity_curve.len() {
                    if self.equity_curve[j].timestamp > end_time {
                        break;
                    }
                    min_equity = min_equity.min(self.equity_curve[j].equity);
                }

                worst_pnls.push(min_equity - start_equity);
            }

            self.rolling_worst.insert(window, worst_pnls);
        }
    }

    pub fn summary(&self) -> TailRiskSummary {
        // Standard windows: 1min, 5min, 15min, 1hour
        let windows = vec![
            60_000_000_000i64,
            300_000_000_000,
            900_000_000_000,
            3_600_000_000_000,
        ];

        let mut worst_by_window = Vec::new();
        for window in windows {
            if let Some(pnls) = self.rolling_worst.get(&window) {
                if let Some(&worst) = pnls.iter().min_by(|a, b| a.partial_cmp(b).unwrap()) {
                    worst_by_window.push(WorstPnlAtWindow {
                        window_ns: window,
                        window_label: format_duration(window),
                        worst_pnl: worst,
                    });
                }
            }
        }

        // Calculate max drawdown
        let mut peak = f64::MIN;
        let mut max_drawdown = 0.0;
        let mut max_drawdown_pct = 0.0;
        for sample in &self.equity_curve {
            if sample.equity > peak {
                peak = sample.equity;
            }
            let dd = peak - sample.equity;
            if dd > max_drawdown {
                max_drawdown = dd;
                max_drawdown_pct = if peak > 0.0 { dd / peak } else { 0.0 };
            }
        }

        // Calculate Calmar ratio
        let total_return = if let (Some(first), Some(last)) =
            (self.equity_curve.first(), self.equity_curve.last())
        {
            (last.equity - first.equity) / first.equity
        } else {
            0.0
        };
        let calmar = if max_drawdown_pct > 0.0 {
            total_return / max_drawdown_pct
        } else {
            0.0
        };

        TailRiskSummary {
            worst_by_window,
            max_drawdown_usd: max_drawdown,
            max_drawdown_pct,
            calmar_ratio: calmar,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TailRiskSummary {
    pub worst_by_window: Vec<WorstPnlAtWindow>,
    pub max_drawdown_usd: f64,
    pub max_drawdown_pct: f64,
    pub calmar_ratio: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorstPnlAtWindow {
    pub window_ns: Nanos,
    pub window_label: String,
    pub worst_pnl: f64,
}

// ============================================================================
// Per-Market Metrics
// ============================================================================

/// Per-market breakdown.
#[derive(Debug, Clone, Default)]
pub struct MarketMetrics {
    pub market_id: MarketId,
    pub fill_count: usize,
    pub volume: f64,
    pub fees: f64,
    pub realized_pnl: f64,
    pub unrealized_pnl: f64,
    pub maker_fills: usize,
    pub taker_fills: usize,
    pub avg_slippage: f64,
    pub trade_count: u64,
}

impl MarketMetrics {
    pub fn total_pnl(&self) -> f64 {
        self.realized_pnl + self.unrealized_pnl
    }

    pub fn net_pnl(&self) -> f64 {
        self.total_pnl() - self.fees
    }

    pub fn maker_ratio(&self) -> f64 {
        let total = self.maker_fills + self.taker_fills;
        if total == 0 {
            return 0.0;
        }
        self.maker_fills as f64 / total as f64
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MarketMetricsSummary {
    pub market_id: String,
    pub fill_count: usize,
    pub volume: f64,
    pub fees: f64,
    pub realized_pnl: f64,
    pub unrealized_pnl: f64,
    pub total_pnl: f64,
    pub net_pnl: f64,
    pub maker_ratio: f64,
    pub avg_slippage: f64,
}

// ============================================================================
// Comprehensive Metrics Collector
// ============================================================================

/// Main metrics collector for backtest.
#[derive(Debug, Default)]
pub struct MetricsCollector {
    pub latency: LatencyMetrics,
    pub fills: FillMetrics,
    pub slippage: SlippageMetrics,
    pub adverse_selection: AdverseSelectionMetrics,
    pub tail_risk: TailRiskMetrics,
    pub by_market: HashMap<MarketId, MarketMetrics>,
    /// Backtest metadata.
    pub start_time: Nanos,
    pub end_time: Nanos,
    pub initial_equity: f64,
    pub final_equity: f64,
    pub strategy_name: String,
}

impl MetricsCollector {
    pub fn new(strategy_name: impl Into<String>, initial_equity: f64, start_time: Nanos) -> Self {
        Self {
            strategy_name: strategy_name.into(),
            initial_equity,
            start_time,
            ..Default::default()
        }
    }

    /// Record a fill.
    pub fn record_fill(&mut self, fill: FillRecord) {
        // Update market metrics
        let market = self.by_market.entry(fill.market_id.clone()).or_default();
        market.market_id = fill.market_id.clone();
        market.fill_count += 1;
        market.volume += fill.size * fill.price;
        market.fees += fill.fee;
        if fill.is_maker {
            market.maker_fills += 1;
        } else {
            market.taker_fills += 1;
        }
        if let Some(slip) = fill.slippage_from_fill_mid() {
            market.avg_slippage = (market.avg_slippage * (market.fill_count - 1) as f64 + slip)
                / market.fill_count as f64;
        }

        // Update slippage metrics
        if let Some(slip) = fill.slippage_from_fill_mid() {
            self.slippage.record(fill.side, slip);
        }

        // Update adverse selection
        self.adverse_selection.record(&fill);

        // Update fill metrics
        self.fills.record_fill(fill);
    }

    /// Record equity sample.
    pub fn record_equity(&mut self, timestamp: Nanos, equity: f64) {
        self.tail_risk.record_equity(timestamp, equity);
    }

    /// Finalize metrics at end of backtest.
    pub fn finalize(&mut self, end_time: Nanos, final_equity: f64, portfolio: &Portfolio) {
        self.end_time = end_time;
        self.final_equity = final_equity;

        // Update market PnL from portfolio
        for (market_id, market_pos) in &portfolio.markets {
            if let Some(metrics) = self.by_market.get_mut(market_id) {
                metrics.realized_pnl = market_pos.realized_pnl();
            }
        }

        // Compute tail risk rolling windows
        self.tail_risk.compute_rolling_worst(&[
            60_000_000_000,
            300_000_000_000,
            900_000_000_000,
            3_600_000_000_000,
        ]);
    }

    /// Generate full report.
    pub fn report(&self) -> BacktestReport {
        let duration_ns = self.end_time - self.start_time;
        let duration_hours = duration_ns as f64 / 3_600_000_000_000.0;
        let total_return = (self.final_equity - self.initial_equity) / self.initial_equity;

        // Calculate Sharpe (simplified - daily returns)
        let sharpe = self.calculate_sharpe();

        // Calculate turnover
        let turnover = self.fills.total_volume() / self.initial_equity;

        BacktestReport {
            strategy_name: self.strategy_name.clone(),
            start_time: self.start_time,
            end_time: self.end_time,
            duration_hours,
            initial_equity: self.initial_equity,
            final_equity: self.final_equity,
            total_return,
            total_return_pct: total_return * 100.0,
            sharpe_ratio: sharpe,
            turnover,
            latency: self.latency.summary(),
            fills: self.fills.summary(),
            slippage: self.slippage.summary(),
            adverse_selection: self.adverse_selection.summary(),
            tail_risk: self.tail_risk.summary(),
            by_market: self
                .by_market
                .values()
                .map(|m| MarketMetricsSummary {
                    market_id: m.market_id.clone(),
                    fill_count: m.fill_count,
                    volume: m.volume,
                    fees: m.fees,
                    realized_pnl: m.realized_pnl,
                    unrealized_pnl: m.unrealized_pnl,
                    total_pnl: m.total_pnl(),
                    net_pnl: m.net_pnl(),
                    maker_ratio: m.maker_ratio(),
                    avg_slippage: m.avg_slippage,
                })
                .collect(),
        }
    }

    fn calculate_sharpe(&self) -> f64 {
        // Group equity samples by day and calculate daily returns
        let equity_samples = &self.tail_risk.equity_curve;
        if equity_samples.len() < 2 {
            return 0.0;
        }

        // Calculate returns between samples
        let mut returns = Vec::new();
        for i in 1..equity_samples.len() {
            let ret = (equity_samples[i].equity - equity_samples[i - 1].equity)
                / equity_samples[i - 1].equity;
            returns.push(ret);
        }

        if returns.is_empty() {
            return 0.0;
        }

        let mean = returns.iter().sum::<f64>() / returns.len() as f64;
        let variance =
            returns.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / returns.len() as f64;
        let std_dev = variance.sqrt();

        if std_dev < 1e-9 {
            return 0.0;
        }

        // Annualize assuming samples are at regular intervals
        // This is simplified; proper Sharpe would need time-weighted returns
        mean / std_dev * (252.0_f64).sqrt()
    }
}

// ============================================================================
// Report Structures
// ============================================================================

/// Complete backtest report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BacktestReport {
    pub strategy_name: String,
    pub start_time: Nanos,
    pub end_time: Nanos,
    pub duration_hours: f64,
    pub initial_equity: f64,
    pub final_equity: f64,
    pub total_return: f64,
    pub total_return_pct: f64,
    pub sharpe_ratio: f64,
    pub turnover: f64,
    pub latency: LatencyMetricsSummary,
    pub fills: FillMetricsSummary,
    pub slippage: SlippageSummary,
    pub adverse_selection: Vec<AdverseSelectionAtHorizon>,
    pub tail_risk: TailRiskSummary,
    pub by_market: Vec<MarketMetricsSummary>,
}

impl BacktestReport {
    /// Export to JSON.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }

    /// Export to JSON file.
    pub fn to_json_file(&self, path: &str) -> std::io::Result<()> {
        let json = self
            .to_json()
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        std::fs::write(path, json)
    }

    /// Generate terminal summary.
    pub fn terminal_summary(&self) -> String {
        let mut s = String::new();
        s.push_str(&format!("\n{}\n", "=".repeat(72)));
        s.push_str(&format!("BACKTEST REPORT: {}\n", self.strategy_name));
        s.push_str(&format!("{}\n\n", "=".repeat(72)));

        // Performance
        s.push_str("PERFORMANCE\n");
        s.push_str(&format!(
            "  Initial Equity:    ${:>12.2}\n",
            self.initial_equity
        ));
        s.push_str(&format!(
            "  Final Equity:      ${:>12.2}\n",
            self.final_equity
        ));
        s.push_str(&format!(
            "  Total Return:      {:>12.2}%\n",
            self.total_return_pct
        ));
        s.push_str(&format!(
            "  Sharpe Ratio:      {:>12.2}\n",
            self.sharpe_ratio
        ));
        s.push_str(&format!("  Turnover:          {:>12.2}x\n", self.turnover));
        s.push_str(&format!(
            "  Duration:          {:>12.1} hours\n\n",
            self.duration_hours
        ));

        // Fill Stats
        s.push_str("FILLS\n");
        s.push_str(&format!(
            "  Orders Sent:       {:>12}\n",
            self.fills.orders_sent
        ));
        s.push_str(&format!(
            "  Fill Rate:         {:>12.1}%\n",
            self.fills.fill_rate * 100.0
        ));
        s.push_str(&format!(
            "  Cancel Rate:       {:>12.1}%\n",
            self.fills.cancel_rate * 100.0
        ));
        s.push_str(&format!(
            "  Maker/Taker:       {:>5.1}% / {:.1}%\n",
            self.fills.maker_pct * 100.0,
            self.fills.taker_pct * 100.0
        ));
        s.push_str(&format!(
            "  Total Volume:      ${:>12.2}\n",
            self.fills.total_volume
        ));
        s.push_str(&format!(
            "  Total Fees:        ${:>12.2}\n",
            self.fills.total_fees
        ));
        s.push_str(&format!(
            "  Cancel-Fill Races: {:>12} ({:.1}%)\n\n",
            self.fills.cancel_fill_races,
            self.fills.cancel_fill_race_rate * 100.0
        ));

        // Latency
        s.push_str("LATENCY (microseconds)\n");
        s.push_str(&format!(
            "  Order->Ack:        p50={:>8.1}  p99={:>8.1}\n",
            self.latency.order_to_ack.p50, self.latency.order_to_ack.p99
        ));
        s.push_str(&format!(
            "  Ack->Fill:         p50={:>8.1}  p99={:>8.1}\n",
            self.latency.ack_to_fill.p50, self.latency.ack_to_fill.p99
        ));
        s.push_str(&format!(
            "  Time in Queue:     p50={:>8.1}  p99={:>8.1}\n\n",
            self.fills.time_in_queue.p50, self.fills.time_in_queue.p99
        ));

        // Slippage
        s.push_str("SLIPPAGE\n");
        s.push_str(&format!(
            "  Mean:              {:>12.4}\n",
            self.slippage.overall.mean
        ));
        s.push_str(&format!(
            "  Total:             ${:>12.2}\n\n",
            self.slippage.overall.total
        ));

        // Tail Risk
        s.push_str("TAIL RISK\n");
        s.push_str(&format!(
            "  Max Drawdown:      {:>12.2}% (${:.2})\n",
            self.tail_risk.max_drawdown_pct * 100.0,
            self.tail_risk.max_drawdown_usd
        ));
        s.push_str(&format!(
            "  Calmar Ratio:      {:>12.2}\n",
            self.tail_risk.calmar_ratio
        ));
        for w in &self.tail_risk.worst_by_window {
            s.push_str(&format!(
                "  Worst {} PnL:  ${:>12.2}\n",
                w.window_label, w.worst_pnl
            ));
        }
        s.push_str("\n");

        // Adverse Selection
        if !self.adverse_selection.is_empty() {
            s.push_str("ADVERSE SELECTION (mean PnL after fill)\n");
            for as_row in &self.adverse_selection {
                s.push_str(&format!(
                    "  +{:>6}:          ${:>12.4}  (win rate: {:.1}%)\n",
                    as_row.horizon_label,
                    as_row.mean_pnl,
                    as_row.win_rate * 100.0
                ));
            }
            s.push_str("\n");
        }

        // Per-Market
        if !self.by_market.is_empty() {
            s.push_str("PER-MARKET BREAKDOWN\n");
            s.push_str(&format!(
                "  {:20} {:>8} {:>12} {:>12} {:>8}\n",
                "Market", "Fills", "Volume", "Net PnL", "Maker%"
            ));
            s.push_str(&format!("  {}\n", "-".repeat(64)));
            for m in &self.by_market {
                let market_id_display = if m.market_id.len() > 20 {
                    format!("{}...", &m.market_id[..17])
                } else {
                    m.market_id.clone()
                };
                s.push_str(&format!(
                    "  {:20} {:>8} ${:>11.2} ${:>11.2} {:>7.1}%\n",
                    market_id_display,
                    m.fill_count,
                    m.volume,
                    m.net_pnl,
                    m.maker_ratio * 100.0
                ));
            }
        }

        s.push_str(&format!("\n{}\n", "=".repeat(72)));
        s
    }
}

// ============================================================================
// Helpers
// ============================================================================

fn format_duration(ns: Nanos) -> String {
    if ns >= 3_600_000_000_000 {
        format!("{}h", ns / 3_600_000_000_000)
    } else if ns >= 60_000_000_000 {
        format!("{}m", ns / 60_000_000_000)
    } else if ns >= 1_000_000_000 {
        format!("{}s", ns / 1_000_000_000)
    } else if ns >= 1_000_000 {
        format!("{}ms", ns / 1_000_000)
    } else if ns >= 1_000 {
        format!("{}us", ns / 1_000)
    } else {
        format!("{}ns", ns)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_latency_percentiles() {
        let mut tracker = LatencyTracker::default();
        for i in 1..=100 {
            tracker.record(i * 1000); // 1-100 microseconds
        }

        let p = tracker.percentiles();
        assert_eq!(p.count, 100);
        assert_eq!(p.min, 1000);
        assert_eq!(p.max, 100_000);
        // p50 uses simple indexing: sorted[n/2] for even n.
        assert_eq!(p.p50, 51_000);
    }

    #[test]
    fn test_slippage_stats() {
        let mut slippage = SlippageMetrics::default();
        slippage.record(Side::Buy, 0.001);
        slippage.record(Side::Buy, 0.002);
        slippage.record(Side::Sell, -0.001);

        let summary = slippage.summary();
        assert_eq!(summary.overall.count, 3);
        assert!((summary.overall.mean - 0.000667).abs() < 0.0001);
    }

    #[test]
    fn test_format_duration() {
        assert_eq!(format_duration(100_000_000), "100ms");
        assert_eq!(format_duration(1_000_000_000), "1s");
        assert_eq!(format_duration(60_000_000_000), "1m");
        assert_eq!(format_duration(3_600_000_000_000), "1h");
    }

    #[test]
    fn test_report_generation() {
        let mut collector = MetricsCollector::new("TestStrategy", 10_000.0, 0);

        // Record some equity samples
        collector.record_equity(0, 10_000.0);
        collector.record_equity(1_000_000_000, 10_100.0);
        collector.record_equity(2_000_000_000, 10_050.0);
        collector.record_equity(3_000_000_000, 10_200.0);

        // Finalize
        let portfolio = crate::backtest_v2::Portfolio::new(10_000.0);
        collector.finalize(3_000_000_000, 10_200.0, &portfolio);

        let report = collector.report();
        assert_eq!(report.strategy_name, "TestStrategy");
        assert_eq!(report.initial_equity, 10_000.0);
        assert_eq!(report.final_equity, 10_200.0);
        assert!((report.total_return - 0.02).abs() < 0.001);

        // Test terminal summary
        let summary = report.terminal_summary();
        assert!(summary.contains("TestStrategy"));
        assert!(summary.contains("10200.00"));
    }
}

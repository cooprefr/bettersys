//! Basis Signal Module
//!
//! Computes and tracks the Binance–Chainlink basis as a first-class signal.
//!
//! # Definition
//!
//! ```text
//! basis_t = Binance_mid(t) - Chainlink_reference_price(t)
//! ```
//!
//! # Key Properties
//!
//! 1. **Arrival-time visibility**: Uses only data visible at decision_time
//! 2. **Settlement-aware**: References Chainlink price per settlement rules
//! 3. **Logged**: Every trade records basis value in DecisionProof
//! 4. **Regime-aware**: Tracks mean, volatility, and regime classification

use crate::backtest_v2::clock::Nanos;
use crate::backtest_v2::oracle::{ChainlinkRound, SettlementReferenceRule};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

// =============================================================================
// CONSTANTS
// =============================================================================

/// Default lookback window for basis statistics (in observations).
pub const DEFAULT_LOOKBACK_OBSERVATIONS: usize = 100;

/// Default lookback window for basis statistics (in nanoseconds).
pub const DEFAULT_LOOKBACK_NS: u64 = 15 * 60 * 1_000_000_000; // 15 minutes

/// Minimum observations required for regime classification.
pub const MIN_OBSERVATIONS_FOR_REGIME: usize = 10;

// =============================================================================
// BASIS VALUE
// =============================================================================

/// A single basis observation with full context.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BasisObservation {
    /// Decision time (arrival time when this observation was computed).
    pub decision_time_ns: Nanos,
    /// Binance mid price (visible at decision_time).
    pub binance_mid: f64,
    /// Chainlink reference price (visible at decision_time per settlement rules).
    pub chainlink_price: f64,
    /// Computed basis = binance_mid - chainlink_price.
    pub basis: f64,
    /// Basis in basis points (bps).
    pub basis_bps: f64,
    /// Chainlink round ID used.
    pub chainlink_round_id: u128,
    /// Chainlink update timestamp (Unix seconds).
    pub chainlink_updated_at_unix_sec: u64,
    /// Time since last Chainlink update (seconds).
    pub chainlink_staleness_sec: f64,
    /// Market window cutoff time (if applicable).
    pub window_cutoff_unix_sec: Option<u64>,
    /// Time remaining until window cutoff (seconds).
    pub time_to_boundary_sec: Option<f64>,
}

impl BasisObservation {
    /// Create a new basis observation.
    pub fn new(
        decision_time_ns: Nanos,
        binance_mid: f64,
        chainlink_price: f64,
        chainlink_round_id: u128,
        chainlink_updated_at_unix_sec: u64,
        window_cutoff_unix_sec: Option<u64>,
    ) -> Self {
        let basis = binance_mid - chainlink_price;
        let basis_bps = if chainlink_price > 0.0 {
            (basis / chainlink_price) * 10_000.0
        } else {
            0.0
        };
        
        // Compute staleness
        let decision_time_sec = decision_time_ns as f64 / 1_000_000_000.0;
        let chainlink_staleness_sec = decision_time_sec - chainlink_updated_at_unix_sec as f64;
        
        // Compute time to boundary
        let time_to_boundary_sec = window_cutoff_unix_sec.map(|cutoff| {
            cutoff as f64 - decision_time_sec
        });
        
        Self {
            decision_time_ns,
            binance_mid,
            chainlink_price,
            basis,
            basis_bps,
            chainlink_round_id,
            chainlink_updated_at_unix_sec,
            chainlink_staleness_sec,
            time_to_boundary_sec,
            window_cutoff_unix_sec,
        }
    }
    
    /// Check if basis is positive (Binance > Chainlink).
    pub fn is_positive(&self) -> bool {
        self.basis > 0.0
    }
    
    /// Check if basis is significant (> 10 bps).
    pub fn is_significant(&self) -> bool {
        self.basis_bps.abs() > 10.0
    }
    
    /// Check if Chainlink data is stale (> 60 seconds).
    pub fn is_chainlink_stale(&self) -> bool {
        self.chainlink_staleness_sec > 60.0
    }
    
    /// Check if we're near the window boundary (< 30 seconds).
    pub fn is_near_boundary(&self) -> bool {
        self.time_to_boundary_sec.map(|t| t < 30.0).unwrap_or(false)
    }
}

// =============================================================================
// BASIS STATISTICS
// =============================================================================

/// Rolling statistics for basis observations.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BasisStats {
    /// Number of observations in the window.
    pub count: usize,
    /// Mean basis (in price units).
    pub mean: f64,
    /// Mean basis (in bps).
    pub mean_bps: f64,
    /// Standard deviation of basis.
    pub std_dev: f64,
    /// Standard deviation of basis (in bps).
    pub std_dev_bps: f64,
    /// Minimum basis observed.
    pub min: f64,
    /// Maximum basis observed.
    pub max: f64,
    /// Current z-score of basis relative to rolling mean.
    pub z_score: f64,
    /// Autocorrelation (lag-1).
    pub autocorr_lag1: f64,
}

impl BasisStats {
    /// Compute statistics from observations.
    pub fn from_observations(observations: &[BasisObservation]) -> Self {
        if observations.is_empty() {
            return Self::default();
        }
        
        let n = observations.len();
        let bases: Vec<f64> = observations.iter().map(|o| o.basis).collect();
        let bases_bps: Vec<f64> = observations.iter().map(|o| o.basis_bps).collect();
        
        // Mean
        let mean = bases.iter().sum::<f64>() / n as f64;
        let mean_bps = bases_bps.iter().sum::<f64>() / n as f64;
        
        // Variance and std dev
        let variance = if n > 1 {
            bases.iter().map(|b| (b - mean).powi(2)).sum::<f64>() / (n - 1) as f64
        } else {
            0.0
        };
        let std_dev = variance.sqrt();
        
        let variance_bps = if n > 1 {
            bases_bps.iter().map(|b| (b - mean_bps).powi(2)).sum::<f64>() / (n - 1) as f64
        } else {
            0.0
        };
        let std_dev_bps = variance_bps.sqrt();
        
        // Min/max
        let min = bases.iter().cloned().fold(f64::INFINITY, f64::min);
        let max = bases.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        
        // Z-score of current observation
        let z_score = if std_dev > 0.0 && !observations.is_empty() {
            (observations.last().unwrap().basis - mean) / std_dev
        } else {
            0.0
        };
        
        // Autocorrelation (lag-1)
        let autocorr_lag1 = if n > 2 && std_dev > 0.0 {
            let mean_shifted = bases[..n-1].iter().sum::<f64>() / (n - 1) as f64;
            let cov: f64 = bases[1..].iter().zip(bases[..n-1].iter())
                .map(|(b1, b0)| (b1 - mean) * (b0 - mean_shifted))
                .sum::<f64>() / (n - 2) as f64;
            cov / variance
        } else {
            0.0
        };
        
        Self {
            count: n,
            mean,
            mean_bps,
            std_dev,
            std_dev_bps,
            min,
            max,
            z_score,
            autocorr_lag1,
        }
    }
}

// =============================================================================
// BASIS REGIME
// =============================================================================

/// Regime classification based on basis behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BasisRegime {
    /// Basis is stable around zero (mean-reverting).
    Stable,
    /// Basis is persistently positive (Binance premium).
    BinancePremium,
    /// Basis is persistently negative (Chainlink premium).
    ChainlinkPremium,
    /// Basis is volatile with high standard deviation.
    Volatile,
    /// Basis is trending (high autocorrelation).
    Trending,
    /// Insufficient data for classification.
    Unknown,
}

impl BasisRegime {
    /// Classify regime from statistics.
    pub fn classify(stats: &BasisStats) -> Self {
        if stats.count < MIN_OBSERVATIONS_FOR_REGIME {
            return Self::Unknown;
        }
        
        // Thresholds
        const STABLE_MEAN_BPS: f64 = 5.0;      // ±5 bps
        const PREMIUM_MEAN_BPS: f64 = 15.0;    // ±15 bps
        const VOLATILE_STD_BPS: f64 = 20.0;    // >20 bps std dev
        const TRENDING_AUTOCORR: f64 = 0.5;    // >0.5 autocorr
        
        // Check for trending first
        if stats.autocorr_lag1.abs() > TRENDING_AUTOCORR {
            return Self::Trending;
        }
        
        // Check for volatility
        if stats.std_dev_bps > VOLATILE_STD_BPS {
            return Self::Volatile;
        }
        
        // Check for persistent premium/discount
        if stats.mean_bps > PREMIUM_MEAN_BPS {
            return Self::BinancePremium;
        }
        if stats.mean_bps < -PREMIUM_MEAN_BPS {
            return Self::ChainlinkPremium;
        }
        
        // Check for stability
        if stats.mean_bps.abs() < STABLE_MEAN_BPS && stats.std_dev_bps < VOLATILE_STD_BPS / 2.0 {
            return Self::Stable;
        }
        
        Self::Unknown
    }
    
    /// Get a description of the regime.
    pub fn description(&self) -> &'static str {
        match self {
            Self::Stable => "Basis stable around zero, mean-reverting",
            Self::BinancePremium => "Persistent Binance premium over Chainlink",
            Self::ChainlinkPremium => "Persistent Chainlink premium over Binance",
            Self::Volatile => "High basis volatility, unstable",
            Self::Trending => "Basis trending (high autocorrelation)",
            Self::Unknown => "Insufficient data for regime classification",
        }
    }
    
    /// Whether this regime is favorable for trading.
    pub fn is_favorable(&self) -> bool {
        matches!(self, Self::Stable | Self::BinancePremium | Self::ChainlinkPremium)
    }
}

// =============================================================================
// BASIS SIGNAL
// =============================================================================

/// Configuration for the basis signal module.
#[derive(Debug, Clone)]
pub struct BasisSignalConfig {
    /// Maximum observations to keep in history.
    pub max_history: usize,
    /// Lookback window for statistics (nanoseconds).
    pub lookback_ns: u64,
    /// Settlement reference rule for Chainlink price.
    pub settlement_rule: SettlementReferenceRule,
    /// Minimum confidence threshold.
    pub min_confidence: f64,
}

impl Default for BasisSignalConfig {
    fn default() -> Self {
        Self {
            max_history: DEFAULT_LOOKBACK_OBSERVATIONS,
            lookback_ns: DEFAULT_LOOKBACK_NS,
            settlement_rule: SettlementReferenceRule::LastUpdateAtOrBeforeCutoff,
            min_confidence: 0.5,
        }
    }
}

/// The main basis signal tracker.
pub struct BasisSignal {
    config: BasisSignalConfig,
    /// History of basis observations.
    history: VecDeque<BasisObservation>,
    /// Current statistics.
    current_stats: BasisStats,
    /// Current regime classification.
    current_regime: BasisRegime,
    /// Last computed basis (for quick access).
    last_basis: Option<BasisObservation>,
}

impl BasisSignal {
    /// Create a new basis signal tracker.
    pub fn new(config: BasisSignalConfig) -> Self {
        Self {
            config,
            history: VecDeque::with_capacity(DEFAULT_LOOKBACK_OBSERVATIONS),
            current_stats: BasisStats::default(),
            current_regime: BasisRegime::Unknown,
            last_basis: None,
        }
    }
    
    /// Update with a new observation.
    pub fn update(
        &mut self,
        decision_time_ns: Nanos,
        binance_mid: f64,
        chainlink_round: &ChainlinkRound,
        window_cutoff_unix_sec: Option<u64>,
    ) {
        let observation = BasisObservation::new(
            decision_time_ns,
            binance_mid,
            chainlink_round.price(),
            chainlink_round.round_id,
            chainlink_round.updated_at,
            window_cutoff_unix_sec,
        );
        
        // Add to history
        self.history.push_back(observation.clone());
        
        // Trim to max history
        while self.history.len() > self.config.max_history {
            self.history.pop_front();
        }
        
        // Also trim by time window
        let cutoff_ns = decision_time_ns.saturating_sub(self.config.lookback_ns as Nanos);
        while self.history.front().map(|o| o.decision_time_ns < cutoff_ns).unwrap_or(false) {
            self.history.pop_front();
        }
        
        // Update statistics
        let observations: Vec<_> = self.history.iter().cloned().collect();
        self.current_stats = BasisStats::from_observations(&observations);
        
        // Update regime
        self.current_regime = BasisRegime::classify(&self.current_stats);
        
        // Store last basis
        self.last_basis = Some(observation);
    }
    
    /// Get the current basis (if available).
    pub fn current_basis(&self) -> Option<&BasisObservation> {
        self.last_basis.as_ref()
    }
    
    /// Get current statistics.
    pub fn stats(&self) -> &BasisStats {
        &self.current_stats
    }
    
    /// Get current regime classification.
    pub fn regime(&self) -> BasisRegime {
        self.current_regime
    }
    
    /// Compute signal confidence based on regime and data quality.
    pub fn confidence(&self) -> f64 {
        if self.history.len() < MIN_OBSERVATIONS_FOR_REGIME {
            return 0.0;
        }
        
        let base_confidence = match self.current_regime {
            BasisRegime::Stable => 0.9,
            BasisRegime::BinancePremium | BasisRegime::ChainlinkPremium => 0.8,
            BasisRegime::Volatile => 0.5,
            BasisRegime::Trending => 0.6,
            BasisRegime::Unknown => 0.3,
        };
        
        // Adjust for data freshness
        let freshness_penalty: f64 = if let Some(ref basis) = self.last_basis {
            if basis.is_chainlink_stale() {
                0.2
            } else {
                0.0
            }
        } else {
            0.3
        };
        
        (base_confidence - freshness_penalty).max(0.0)
    }
    
    /// Check if basis signal is reliable enough for trading.
    pub fn is_reliable(&self) -> bool {
        self.confidence() >= self.config.min_confidence
    }
    
    /// Compute expected basis at settlement (mean-reversion assumption).
    /// 
    /// Returns the expected basis value at the window cutoff,
    /// assuming mean reversion to historical mean.
    pub fn expected_settlement_basis(&self) -> Option<f64> {
        let current = self.last_basis.as_ref()?;
        let time_to_boundary = current.time_to_boundary_sec?;
        
        if time_to_boundary <= 0.0 {
            return Some(current.basis);
        }
        
        // Simple mean-reversion model:
        // E[basis_T] = mean + (current - mean) * exp(-lambda * time_to_boundary)
        // Using lambda = 1/300 (5-minute half-life)
        let lambda = 1.0 / 300.0;
        let decay = (-lambda * time_to_boundary).exp();
        
        Some(self.current_stats.mean + (current.basis - self.current_stats.mean) * decay)
    }
    
    /// Generate a trade signal based on basis.
    pub fn trade_signal(&self) -> Option<BasisTradeSignal> {
        let current = self.last_basis.as_ref()?;
        
        if !self.is_reliable() {
            return None;
        }
        
        // Signal based on expected basis convergence
        let expected_basis = self.expected_settlement_basis()?;
        let basis_change = expected_basis - current.basis;
        
        // Direction: if basis expected to increase, Binance will be relatively cheaper
        // (buy on Binance, or for 15m updown: basis affects Up vs Down pricing)
        let direction = if basis_change > 0.0 {
            TradeDirection::Long
        } else {
            TradeDirection::Short
        };
        
        // Strength based on expected change magnitude
        let strength = (basis_change.abs() / self.current_stats.std_dev.max(0.0001)).min(3.0) / 3.0;
        
        Some(BasisTradeSignal {
            direction,
            strength,
            current_basis: current.clone(),
            expected_basis,
            confidence: self.confidence(),
            regime: self.current_regime,
        })
    }
    
    /// Get basis summary for logging (can be added to decision metadata).
    pub fn summary_for_logging(&self, label: &str) -> Vec<(String, String)> {
        let mut entries = vec![];
        if let Some(ref basis) = self.last_basis {
            entries.push((format!("{}_basis", label), format!("{:.4}", basis.basis)));
            entries.push((format!("{}_basis_bps", label), format!("{:.2}", basis.basis_bps)));
            entries.push((format!("{}_chainlink_price", label), format!("{:.6}", basis.chainlink_price)));
            entries.push((format!("{}_binance_mid", label), format!("{:.6}", basis.binance_mid)));
            entries.push((format!("{}_regime", label), format!("{:?}", self.current_regime)));
            entries.push((format!("{}_confidence", label), format!("{:.2}", self.confidence())));
        }
        entries
    }
}

// =============================================================================
// TRADE SIGNAL
// =============================================================================

/// Direction of a trade signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TradeDirection {
    Long,
    Short,
}

/// A trade signal derived from basis analysis.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BasisTradeSignal {
    /// Trade direction.
    pub direction: TradeDirection,
    /// Signal strength (0.0 to 1.0).
    pub strength: f64,
    /// Current basis observation.
    pub current_basis: BasisObservation,
    /// Expected basis at settlement.
    pub expected_basis: f64,
    /// Signal confidence.
    pub confidence: f64,
    /// Current regime.
    pub regime: BasisRegime,
}

impl BasisTradeSignal {
    /// Expected profit from basis convergence (in price units).
    pub fn expected_basis_profit(&self) -> f64 {
        self.expected_basis - self.current_basis.basis
    }
    
    /// Check if signal is strong enough to trade.
    pub fn is_actionable(&self, min_strength: f64) -> bool {
        self.strength >= min_strength && self.confidence >= 0.5
    }
}

// =============================================================================
// DECISION RECORD
// =============================================================================

/// A complete record of a basis-aware trade decision.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BasisDecisionRecord {
    /// Decision timestamp.
    pub decision_time_ns: Nanos,
    /// Signal that triggered the decision.
    pub signal: BasisTradeSignal,
    /// Expected payoff vs settlement.
    pub expected_payoff: f64,
    /// Expected execution cost.
    pub expected_execution_cost: f64,
    /// Net expected edge (payoff - cost).
    pub net_expected_edge: f64,
    /// Whether the decision was to trade.
    pub traded: bool,
    /// Reason if not traded.
    pub rejection_reason: Option<String>,
}

impl BasisDecisionRecord {
    /// Create a new decision record.
    pub fn new(
        decision_time_ns: Nanos,
        signal: BasisTradeSignal,
        expected_payoff: f64,
        expected_execution_cost: f64,
    ) -> Self {
        let net_expected_edge = expected_payoff - expected_execution_cost;
        Self {
            decision_time_ns,
            signal,
            expected_payoff,
            expected_execution_cost,
            net_expected_edge,
            traded: net_expected_edge > 0.0,
            rejection_reason: if net_expected_edge <= 0.0 {
                Some(format!("Net edge {} <= 0", net_expected_edge))
            } else {
                None
            },
        }
    }
    
    /// Mark as rejected with reason.
    pub fn reject(mut self, reason: String) -> Self {
        self.traded = false;
        self.rejection_reason = Some(reason);
        self
    }
}

// =============================================================================
// TESTS
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    
    fn make_chainlink_round(price: f64, updated_at: u64, round_id: u128) -> ChainlinkRound {
        ChainlinkRound {
            feed_id: "btc_usd".to_string(),
            round_id,
            answer: (price * 1e8) as i128,
            started_at: updated_at,
            updated_at,
            answered_in_round: round_id,
            decimals: 8,
            asset_symbol: "BTC".to_string(),
            ingest_arrival_time_ns: updated_at * 1_000_000_000,
            ingest_seq: round_id as u64,
            raw_source_hash: None,
        }
    }
    
    #[test]
    fn test_basis_observation() {
        let decision_time_ns = 1000_000_000_000i64; // 1000 seconds
        let binance_mid = 100.05;
        let chainlink_price = 100.00;
        
        let obs = BasisObservation::new(
            decision_time_ns,
            binance_mid,
            chainlink_price,
            123,
            990, // 10 seconds stale
            Some(1015), // 15 seconds to boundary
        );
        
        assert!((obs.basis - 0.05).abs() < 1e-9);
        assert!((obs.basis_bps - 5.0).abs() < 0.1);
        assert!(obs.is_positive());
        assert!(!obs.is_significant()); // 5 bps < 10 bps
        assert!(!obs.is_chainlink_stale()); // 10 sec < 60 sec
        assert!(obs.is_near_boundary()); // 15 sec < 30 sec
    }
    
    #[test]
    fn test_basis_stats() {
        let observations = vec![
            BasisObservation::new(1_000_000_000, 100.05, 100.00, 1, 1, None),
            BasisObservation::new(2_000_000_000, 100.10, 100.00, 2, 2, None),
            BasisObservation::new(3_000_000_000, 99.95, 100.00, 3, 3, None),
            BasisObservation::new(4_000_000_000, 100.00, 100.00, 4, 4, None),
        ];
        
        let stats = BasisStats::from_observations(&observations);
        
        assert_eq!(stats.count, 4);
        assert!((stats.mean - 0.025).abs() < 0.01);
        assert!(stats.std_dev > 0.0);
        assert!((stats.min - (-0.05)).abs() < 1e-9);
        assert!((stats.max - 0.10).abs() < 1e-9);
    }
    
    #[test]
    fn test_basis_regime_classification() {
        // Stable regime
        let stable_stats = BasisStats {
            count: 20,
            mean_bps: 2.0,
            std_dev_bps: 5.0,
            autocorr_lag1: 0.1,
            ..Default::default()
        };
        assert_eq!(BasisRegime::classify(&stable_stats), BasisRegime::Stable);
        
        // Binance premium
        let premium_stats = BasisStats {
            count: 20,
            mean_bps: 25.0,
            std_dev_bps: 10.0,
            autocorr_lag1: 0.1,
            ..Default::default()
        };
        assert_eq!(BasisRegime::classify(&premium_stats), BasisRegime::BinancePremium);
        
        // Volatile
        let volatile_stats = BasisStats {
            count: 20,
            mean_bps: 5.0,
            std_dev_bps: 30.0,
            autocorr_lag1: 0.1,
            ..Default::default()
        };
        assert_eq!(BasisRegime::classify(&volatile_stats), BasisRegime::Volatile);
        
        // Trending
        let trending_stats = BasisStats {
            count: 20,
            mean_bps: 5.0,
            std_dev_bps: 10.0,
            autocorr_lag1: 0.7,
            ..Default::default()
        };
        assert_eq!(BasisRegime::classify(&trending_stats), BasisRegime::Trending);
        
        // Unknown (insufficient data)
        let insufficient_stats = BasisStats {
            count: 5,
            ..Default::default()
        };
        assert_eq!(BasisRegime::classify(&insufficient_stats), BasisRegime::Unknown);
    }
    
    #[test]
    fn test_basis_signal_update() {
        let config = BasisSignalConfig::default();
        let mut signal = BasisSignal::new(config);
        
        // Add observations
        for i in 0..15 {
            let round = make_chainlink_round(100.0, 1000 + i as u64, i as u128);
            let decision_time = (1000 + i as i64) * 1_000_000_000;
            signal.update(
                decision_time,
                100.0 + (i as f64 % 3.0) * 0.01, // Varying Binance price
                &round,
                Some(1020),
            );
        }
        
        assert!(signal.current_basis().is_some());
        assert_eq!(signal.stats().count, 15);
        assert_ne!(signal.regime(), BasisRegime::Unknown);
    }
    
    #[test]
    fn test_trade_signal_generation() {
        let config = BasisSignalConfig::default();
        let mut signal = BasisSignal::new(config);
        
        // Add enough observations for reliability
        for i in 0..20 {
            let round = make_chainlink_round(100.0, 1000 + i as u64, i as u128);
            let decision_time = (1000 + i as i64) * 1_000_000_000;
            signal.update(
                decision_time,
                100.05, // Consistent Binance premium
                &round,
                Some(1100), // 80 seconds to boundary
            );
        }
        
        let trade_signal = signal.trade_signal();
        assert!(trade_signal.is_some());
        
        let ts = trade_signal.unwrap();
        assert!(ts.confidence > 0.5);
    }
    
    #[test]
    fn test_expected_settlement_basis() {
        let config = BasisSignalConfig::default();
        let mut signal = BasisSignal::new(config);
        
        // Add observations with positive basis
        for i in 0..20 {
            let round = make_chainlink_round(100.0, 1000 + i as u64, i as u128);
            let decision_time = (1000 + i as i64) * 1_000_000_000;
            signal.update(
                decision_time,
                100.10, // 10 cent premium
                &round,
                Some(1300), // 300 seconds to boundary
            );
        }
        
        let expected = signal.expected_settlement_basis();
        assert!(expected.is_some());
        
        // Should decay towards mean
        let exp_val = expected.unwrap();
        let current = signal.current_basis().unwrap().basis;
        let mean = signal.stats().mean;
        
        // Expected should be between current and mean
        if current > mean {
            assert!(exp_val <= current && exp_val >= mean);
        } else {
            assert!(exp_val >= current && exp_val <= mean);
        }
    }
    
    #[test]
    fn test_decision_record() {
        let signal = BasisTradeSignal {
            direction: TradeDirection::Long,
            strength: 0.7,
            current_basis: BasisObservation::new(
                1_000_000_000,
                100.05,
                100.00,
                1,
                1,
                Some(1015),
            ),
            expected_basis: 0.02,
            confidence: 0.8,
            regime: BasisRegime::Stable,
        };
        
        // Profitable trade
        let record = BasisDecisionRecord::new(
            1_000_000_000,
            signal.clone(),
            0.10, // expected payoff
            0.02, // execution cost
        );
        assert!(record.traded);
        assert!((record.net_expected_edge - 0.08).abs() < 1e-9);
        
        // Unprofitable trade
        let record2 = BasisDecisionRecord::new(
            1_000_000_000,
            signal,
            0.01, // expected payoff
            0.02, // execution cost
        );
        assert!(!record2.traded);
        assert!(record2.rejection_reason.is_some());
    }
}

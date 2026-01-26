//! Vault Module - User Deposits & Automated Trading
//!
//! This module handles:
//! 1. User wallet registration and deposits
//! 2. Fractional Kelly criterion position sizing
//! 3. Automated trade execution based on signals
//!
//! Architecture:
//! - Users deposit USDC or BETTER tokens
//! - Each signal triggers Kelly-optimal position sizing
//! - Trades are executed via DomeAPI or direct Polymarket CLOB

pub mod ab_test;
pub mod backtest_rnjd;
pub mod belief_vol;
pub mod book_access; // HFT-grade cache-only book access (no REST in hot path)
pub mod engine;
pub mod execution;
pub mod fast15m_reactive;
pub mod hft_paper_strategy; // HFT paper trading with RN-JD core (DEPRECATED - use unified_15m_strategy)
pub mod kelly;
pub mod latency_arb;
pub mod llm;
pub mod orderflow_paper;
pub mod paper_ledger;
pub mod pool;
pub mod rnjd;
pub mod trade_executor;
pub mod unified_15m_strategy; // PRODUCTION: Unified 15M strategy for live trading
pub mod updown15m;
pub mod user_accounts;
pub mod vault_db;

pub use ab_test::{ABTestConfig, ABTestSummary, ABTestTracker, ModelVariant};
pub use backtest_rnjd::{BacktestCollector, BacktestMetrics, BacktestRecord};
pub use belief_vol::{
    logit, sigmoid, sigmoid_derivative, sigmoid_second_derivative, BeliefVolConfig,
    BeliefVolEstimate, BeliefVolSummary, BeliefVolTracker, JumpDetectionResult, LogOddsIncrement,
};
pub use book_access::{
    best_ask_cached, best_ask_hft, best_bid_cached, best_bid_hft, bid_ask_spread_cached,
    bid_ask_spread_hft, get_book_auto, get_book_cached, get_book_hft, BookResult, HasHftCache,
    SkipReason, StalenessConfig,
};
pub use engine::*;
pub use execution::*;
pub use fast15m_reactive::*;
pub use hft_paper_strategy::{
    spawn_hft_paper_strategy, HftPaperMetrics, HftPaperMetricsSummary, HftPaperStrategyConfig,
    HftPaperTrade,
};
pub use kelly::*;
pub use latency_arb::*;
pub use llm::*;
pub use orderflow_paper::{
    spawn_orderflow_paper_engine, OrderflowPaperConfig, OrderflowPaperMetrics,
    OrderflowPaperMetricsSummary,
};
pub use paper_ledger::*;
pub use pool::*;
pub use rnjd::{
    estimate_p_up_enhanced, estimate_p_up_rnjd, price_vol_to_belief_vol, rn_drift,
    JumpRegimeDetector, RnjdEstimate, RnjdParams,
};
pub use trade_executor::*;
pub use unified_15m_strategy::{
    ExitReason, MetricsSummary, OpenPosition, PositionSide, StrategyMetrics, TradeRecord,
    Unified15mConfig, Unified15mStrategy,
};
pub use updown15m::*;
pub use user_accounts::*;
pub use vault_db::*;

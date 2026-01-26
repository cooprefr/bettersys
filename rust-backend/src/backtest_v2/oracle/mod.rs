//! Oracle Module for Canonical Settlement Reference
//!
//! This module provides production-grade Chainlink integration for Polymarket
//! 15-minute up/down market settlement. It handles:
//!
//! - **Mandatory configuration**: All settlement parameters must be explicitly set
//! - **Feed validation**: Validates feed addresses against chain/network at startup
//! - **Automated backfill**: Historical oracle rounds via logs or round iteration
//! - **Persistent storage**: SQLite with arrival-time semantics
//! - **Deterministic replay**: For backtesting with visibility enforcement
//!
//! **CRITICAL**: Settlement MUST use Chainlink oracle prices, NOT Binance spot.
//! Binance is only used for strategy signal generation and basis diagnostics.
//!
//! # Production-Grade Requirements
//!
//! For production-grade backtests, you MUST:
//! 1. Configure `OracleConfig` with all required fields (no silent defaults)
//! 2. Call `validate_oracle_config_for_production()` at startup
//! 3. Run backfill for the required time range
//! 4. Include oracle config in the run fingerprint

pub mod chainlink;
pub mod storage;
pub mod settlement_source;
pub mod basis_diagnostics;
pub mod config;
pub mod backfill;
pub mod validation;

pub use chainlink::{
    ChainlinkFeedConfig, ChainlinkRound, ChainlinkIngestor, ChainlinkReplayFeed,
};
pub use storage::{OracleRoundStorage, OracleStorageConfig, BackfillState};
pub use settlement_source::{
    SettlementReferenceSource, SettlementReferenceRule, OraclePricePoint,
    ChainlinkSettlementSource, LiveChainlinkSettlementSource,
};
pub use config::{
    OracleConfig, OracleFeedConfig, OracleVisibilityRule, RoundingPolicy,
    OracleConfigViolation, OracleConfigValidationResult,
};
pub use backfill::{
    OracleBackfillService, BackfillConfig, BackfillStrategy,
    BackfillProgress, BackfillResult,
};
pub use validation::{
    OracleFeedValidator, FeedValidationResult, OracleValidationResult,
    validate_oracle_config_for_production,
};

#[cfg(test)]
mod tests;
pub use basis_diagnostics::{BasisDiagnostics, BasisStats, WindowBasis};

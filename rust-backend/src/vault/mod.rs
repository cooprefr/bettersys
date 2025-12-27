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

pub mod kelly;
pub mod trade_executor;
pub mod user_accounts;

pub use kelly::*;
pub use trade_executor::*;
pub use user_accounts::*;

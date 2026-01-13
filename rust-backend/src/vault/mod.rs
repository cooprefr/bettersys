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

pub mod engine;
pub mod execution;
pub mod kelly;
pub mod llm;
pub mod paper_ledger;
pub mod pool;
pub mod trade_executor;
pub mod updown15m;
pub mod user_accounts;
pub mod vault_db;

pub use engine::*;
pub use execution::*;
pub use kelly::*;
pub use llm::*;
pub use paper_ledger::*;
pub use pool::*;
pub use trade_executor::*;
pub use updown15m::*;
pub use user_accounts::*;
pub use vault_db::*;

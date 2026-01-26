//! BetterBot Backend Library
//!
//! Exposes core modules for use by binaries and tests.
//! Note: Most modules depend on AppState from main.rs
//! Only standalone modules are exported here.

pub mod backtest_v2;
pub mod performance;

// Re-export latency at crate root for compatibility
pub use performance::latency;

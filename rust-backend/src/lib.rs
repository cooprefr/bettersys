//! BetterBot Backend Library
//!
//! Exposes core modules for use by binaries and tests.
//! Note: Most modules depend on AppState from main.rs
//! Only standalone modules are exported here.

pub mod backtest_v2;
pub mod edge;
pub mod performance;
pub mod route_quality;

// Re-export latency at crate root for compatibility
pub use performance::latency;

// Re-export edge types for convenience
pub use edge::{
    EdgeFallbackController, EdgeReceiver, EdgeReceiverClient, EdgeReceiverClientConfig,
    EdgeReceiverConfig, EdgeTick, SymbolId,
};

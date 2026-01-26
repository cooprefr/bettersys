//! Compatibility shim: expose `crate::latency::*` for the binary crate.
//!
//! The implementation lives in `crate::performance::latency`.

pub use crate::performance::latency::*;

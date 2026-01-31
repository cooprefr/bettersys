//! Edge Receiver Module
//!
//! Two-tier architecture for low-latency Binance market data:
//! - Edge receiver (ap-southeast-1): Connects to Binance, normalizes, forwards
//! - Engine client (eu-west-1): Receives binary stream, handles loss/reorder

pub mod client;
pub mod receiver;
pub mod wire;

pub use client::{EdgeFallbackController, EdgeReceiverClient, EdgeReceiverClientConfig};
pub use receiver::{EdgeReceiver, EdgeReceiverConfig};
pub use wire::{EdgeFlags, EdgeTick, SymbolId, EDGE_TICK_SIZE};

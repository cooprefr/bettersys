pub mod binance_arb_feed; // Binance arb feed with L1 + trades for 15M monitoring
pub mod binance_price_feed; // Binance spot L1 mid-price feed (barter-data)
pub mod chainlink_feed;
pub mod dome;
pub mod dome_realtime; // Real-time REST polling (reliable fallback)
pub mod dome_rest; // REST client for enrichment and analytics
pub mod dome_tracker;
pub mod dome_websocket; // Real-time WebSocket client
pub mod expiry_edge; // Expiry edge alpha signal
pub mod hashdive;
pub mod hashdive_api;
pub mod oracle_comparison; // Chainlink vs Binance oracle comparison for 15m markets
pub mod polymarket;
pub mod polymarket_api;
pub mod polymarket_book_store; // HFT-grade orderbook store (no REST in hot path)
#[cfg(test)]
pub mod polymarket_book_store_test; // Testing utilities
pub mod polymarket_gamma;
pub mod polymarket_ws;

// Re-export price update event for reactive consumers
pub use binance_price_feed::PriceUpdateEvent;

// Re-export HFT book cache types for convenience
pub use polymarket_book_store::{
    BookLookupResult, BookSnapshot, BookStore, BookStoreConfig, BookStoreMetrics, CacheMissReason,
    HftBookCache, PriceLevel, SubscriptionManager, WarmupManager, WarmupStatus,
};

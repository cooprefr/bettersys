//! Strategy Harness
//!
//! Production-identical interfaces for strategies that work in both live and backtest modes.
//! The same strategy code runs unchanged - only the adapters are swapped via feature flags.
//!
//! # Hermetic Boundary Enforcement
//!
//! This module enforces compile-time hermetic boundaries. Strategy code MUST NOT
//! access wall-clock time APIs. The following are FORBIDDEN:
//! - `std::time::SystemTime`
//! - `std::time::Instant`
//! - `tokio::time::Instant`
//! - `chrono::Utc::now()` / `chrono::Local::now()`
//!
//! Use `StrategyContext::timestamp()` for all time-related operations.
//!
//! See: HERMETIC_COMPILE_ENFORCEMENT.md

// =============================================================================
// HERMETIC BOUNDARY: COMPILE-TIME ENFORCEMENT
// =============================================================================
// The following deny attributes enforce that strategy code cannot access
// wall-clock time APIs at compile time. Any attempt to use forbidden types
// or methods will result in a compilation error.
#![deny(clippy::disallowed_types)]
#![deny(clippy::disallowed_methods)]

use crate::backtest_v2::clock::Nanos;
use crate::backtest_v2::events::{Level, OrderId, OrderType, Price, Side, Size, TimeInForce};
use std::collections::HashMap;

/// Book snapshot provided to strategies.
#[derive(Debug, Clone)]
pub struct BookSnapshot {
    pub token_id: String,
    pub bids: Vec<Level>,
    pub asks: Vec<Level>,
    pub timestamp: Nanos,
    pub exchange_seq: u64,
}

impl BookSnapshot {
    pub fn best_bid(&self) -> Option<&Level> {
        self.bids.first()
    }

    pub fn best_ask(&self) -> Option<&Level> {
        self.asks.first()
    }

    pub fn mid_price(&self) -> Option<Price> {
        match (self.best_bid(), self.best_ask()) {
            (Some(bid), Some(ask)) => Some((bid.price + ask.price) / 2.0),
            _ => None,
        }
    }

    pub fn spread(&self) -> Option<Price> {
        match (self.best_bid(), self.best_ask()) {
            (Some(bid), Some(ask)) => Some(ask.price - bid.price),
            _ => None,
        }
    }

    pub fn spread_bps(&self) -> Option<f64> {
        match (self.spread(), self.mid_price()) {
            (Some(spread), Some(mid)) if mid > 0.0 => Some(spread / mid * 10_000.0),
            _ => None,
        }
    }
}

/// Trade print provided to strategies.
#[derive(Debug, Clone)]
pub struct TradePrint {
    pub token_id: String,
    pub price: Price,
    pub size: Size,
    pub aggressor_side: Side,
    pub timestamp: Nanos,
    pub trade_id: Option<String>,
}

/// Timer event for scheduled callbacks.
#[derive(Debug, Clone)]
pub struct TimerEvent {
    pub timer_id: u64,
    pub scheduled_time: Nanos,
    pub actual_time: Nanos,
    pub payload: Option<String>,
}

/// Order request sent by strategy.
#[derive(Debug, Clone)]
pub struct StrategyOrder {
    pub client_order_id: String,
    pub token_id: String,
    pub side: Side,
    pub price: Price,
    pub size: Size,
    pub order_type: OrderType,
    pub time_in_force: TimeInForce,
    pub post_only: bool,
    pub reduce_only: bool,
}

impl StrategyOrder {
    pub fn limit(
        client_order_id: impl Into<String>,
        token_id: impl Into<String>,
        side: Side,
        price: Price,
        size: Size,
    ) -> Self {
        Self {
            client_order_id: client_order_id.into(),
            token_id: token_id.into(),
            side,
            price,
            size,
            order_type: OrderType::Limit,
            time_in_force: TimeInForce::Gtc,
            post_only: false,
            reduce_only: false,
        }
    }

    pub fn post_only(mut self) -> Self {
        self.post_only = true;
        self
    }

    pub fn ioc(mut self) -> Self {
        self.time_in_force = TimeInForce::Ioc;
        self
    }

    pub fn fok(mut self) -> Self {
        self.time_in_force = TimeInForce::Fok;
        self
    }
}

/// Cancel request sent by strategy.
#[derive(Debug, Clone)]
pub struct StrategyCancel {
    pub order_id: OrderId,
    pub client_order_id: Option<String>,
}

/// Fill notification received by strategy.
#[derive(Debug, Clone)]
pub struct FillNotification {
    pub order_id: OrderId,
    pub client_order_id: Option<String>,
    pub price: Price,
    pub size: Size,
    pub is_maker: bool,
    pub leaves_qty: Size,
    pub fee: f64,
    pub timestamp: Nanos,
}

/// Order acknowledgment received by strategy.
#[derive(Debug, Clone)]
pub struct OrderAck {
    pub order_id: OrderId,
    pub client_order_id: Option<String>,
    pub timestamp: Nanos,
}

/// Order rejection received by strategy.
#[derive(Debug, Clone)]
pub struct OrderReject {
    pub order_id: OrderId,
    pub client_order_id: Option<String>,
    pub reason: String,
    pub timestamp: Nanos,
}

/// Cancel acknowledgment received by strategy.
#[derive(Debug, Clone)]
pub struct CancelAck {
    pub order_id: OrderId,
    pub cancelled_qty: Size,
    pub timestamp: Nanos,
}

/// Order sender interface - same API for live and backtest.
///
/// In production: sends to exchange via gateway
/// In backtest: sends to matching simulator
pub trait OrderSender: Send + Sync {
    /// Submit a new order.
    fn send_order(&mut self, order: StrategyOrder) -> Result<OrderId, String>;

    /// Cancel an existing order.
    fn send_cancel(&mut self, cancel: StrategyCancel) -> Result<(), String>;

    /// Cancel all orders for a token.
    fn cancel_all(&mut self, token_id: &str) -> Result<usize, String>;

    /// Get current position for a token.
    fn get_position(&self, token_id: &str) -> Position;

    /// Get all positions.
    fn get_all_positions(&self) -> HashMap<String, Position>;

    /// Get open orders.
    fn get_open_orders(&self) -> Vec<OpenOrder>;

    /// Get current time (simulation or real).
    fn now(&self) -> Nanos;

    /// Schedule a timer callback.
    fn schedule_timer(&mut self, delay_ns: Nanos, payload: Option<String>) -> u64;

    /// Cancel a timer.
    fn cancel_timer(&mut self, timer_id: u64) -> bool;
}

/// Position information.
#[derive(Debug, Clone, Default)]
pub struct Position {
    pub token_id: String,
    pub shares: Size,
    pub cost_basis: f64,
    pub realized_pnl: f64,
    pub unrealized_pnl: f64,
}

impl Position {
    pub fn is_flat(&self) -> bool {
        self.shares.abs() < 1e-9
    }

    pub fn is_long(&self) -> bool {
        self.shares > 1e-9
    }

    pub fn is_short(&self) -> bool {
        self.shares < -1e-9
    }
}

/// Open order information.
#[derive(Debug, Clone)]
pub struct OpenOrder {
    pub order_id: OrderId,
    pub client_order_id: String,
    pub token_id: String,
    pub side: Side,
    pub price: Price,
    pub original_size: Size,
    pub remaining_size: Size,
    pub created_at: Nanos,
}

/// Strategy context provided on each callback.
pub struct StrategyContext<'a> {
    /// Order sender for submitting orders.
    pub orders: &'a mut dyn OrderSender,
    /// Current simulation/real time.
    pub timestamp: Nanos,
    /// Strategy parameters (read-only).
    pub params: &'a StrategyParams,
}

/// Strategy parameters (loaded from config).
#[derive(Debug, Clone, Default)]
pub struct StrategyParams {
    pub params: HashMap<String, f64>,
    pub strings: HashMap<String, String>,
}

impl StrategyParams {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_param(mut self, key: impl Into<String>, value: f64) -> Self {
        self.params.insert(key.into(), value);
        self
    }

    pub fn with_string(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.strings.insert(key.into(), value.into());
        self
    }

    pub fn get(&self, key: &str) -> Option<f64> {
        self.params.get(key).copied()
    }

    pub fn get_or(&self, key: &str, default: f64) -> f64 {
        self.params.get(key).copied().unwrap_or(default)
    }

    pub fn get_string(&self, key: &str) -> Option<&str> {
        self.strings.get(key).map(|s| s.as_str())
    }
}

/// The core strategy trait - implement this for your trading logic.
///
/// This trait is identical for live and backtest execution.
/// The same implementation runs in both modes without modification.
pub trait Strategy: Send {
    /// Called on each book update.
    fn on_book_update(&mut self, ctx: &mut StrategyContext, book: &BookSnapshot);

    /// Called on each trade print.
    fn on_trade(&mut self, ctx: &mut StrategyContext, trade: &TradePrint);

    /// Called when a timer fires.
    fn on_timer(&mut self, ctx: &mut StrategyContext, timer: &TimerEvent);

    /// Called when an order is acknowledged.
    fn on_order_ack(&mut self, ctx: &mut StrategyContext, ack: &OrderAck);

    /// Called when an order is rejected.
    fn on_order_reject(&mut self, ctx: &mut StrategyContext, reject: &OrderReject);

    /// Called when an order is filled.
    fn on_fill(&mut self, ctx: &mut StrategyContext, fill: &FillNotification);

    /// Called when a cancel is acknowledged.
    fn on_cancel_ack(&mut self, ctx: &mut StrategyContext, ack: &CancelAck);

    /// Called once at strategy startup.
    fn on_start(&mut self, _ctx: &mut StrategyContext) {}

    /// Called once at strategy shutdown.
    fn on_stop(&mut self, _ctx: &mut StrategyContext) {}

    /// Strategy name for logging.
    fn name(&self) -> &str;

    /// Get strategy state for persistence (optional).
    fn get_state(&self) -> Option<String> {
        None
    }

    /// Restore strategy state (optional).
    fn restore_state(&mut self, _state: &str) -> Result<(), String> {
        Ok(())
    }
}

/// Strategy factory for creating strategy instances.
pub trait StrategyFactory: Send + Sync {
    fn create(&self, params: StrategyParams) -> Box<dyn Strategy>;
    fn name(&self) -> &str;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_book_snapshot() {
        let book = BookSnapshot {
            token_id: "token123".into(),
            bids: vec![Level::new(0.45, 100.0), Level::new(0.44, 200.0)],
            asks: vec![Level::new(0.55, 150.0), Level::new(0.56, 250.0)],
            timestamp: 1000,
            exchange_seq: 1,
        };

        assert_eq!(book.best_bid().unwrap().price, 0.45);
        assert_eq!(book.best_ask().unwrap().price, 0.55);
        assert!((book.mid_price().unwrap() - 0.50).abs() < 1e-9);
        assert!((book.spread().unwrap() - 0.10).abs() < 1e-9);
    }

    #[test]
    fn test_strategy_order_builder() {
        let order = StrategyOrder::limit("order1", "token123", Side::Buy, 0.50, 100.0)
            .post_only()
            .ioc();

        assert_eq!(order.client_order_id, "order1");
        assert!(order.post_only);
        assert!(matches!(order.time_in_force, TimeInForce::Ioc));
    }

    #[test]
    fn test_position() {
        let mut pos = Position::default();
        assert!(pos.is_flat());

        pos.shares = 100.0;
        assert!(pos.is_long());
        assert!(!pos.is_short());

        pos.shares = -50.0;
        assert!(pos.is_short());
        assert!(!pos.is_long());
    }

    #[test]
    fn test_strategy_params() {
        let params = StrategyParams::new()
            .with_param("max_position", 1000.0)
            .with_param("spread_threshold", 0.02)
            .with_string("token", "BTC-UPDOWN");

        assert_eq!(params.get("max_position"), Some(1000.0));
        assert_eq!(params.get_or("missing", 42.0), 42.0);
        assert_eq!(params.get_string("token"), Some("BTC-UPDOWN"));
    }
}

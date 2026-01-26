//! Example Strategy
//!
//! A minimal market-making strategy that demonstrates the strategy harness.
//! This same code runs in both live and backtest modes.
//!
//! # Hermetic Boundary Enforcement
//!
//! This module is subject to compile-time hermetic enforcement.
//! Wall-clock time APIs (`SystemTime`, `Instant`) are FORBIDDEN.
//! Use `StrategyContext::timestamp()` for all time-related operations.
//!
//! See: HERMETIC_COMPILE_ENFORCEMENT.md

// =============================================================================
// HERMETIC BOUNDARY: COMPILE-TIME ENFORCEMENT
// =============================================================================
#![deny(clippy::disallowed_types)]
#![deny(clippy::disallowed_methods)]

use crate::backtest_v2::clock::{Nanos, NANOS_PER_SEC};
use crate::backtest_v2::events::Side;
use crate::backtest_v2::strategy::{
    BookSnapshot, CancelAck, FillNotification, OpenOrder, OrderAck, OrderReject, Position,
    Strategy, StrategyContext, StrategyOrder, StrategyParams, TimerEvent, TradePrint,
};
use std::collections::HashMap;

/// Simple market-making strategy.
///
/// Maintains two-sided quotes around mid-price with configurable spread.
/// Demonstrates all strategy callbacks and order management.
pub struct MarketMakerStrategy {
    /// Strategy name.
    name: String,
    /// Token to trade.
    token_id: String,
    /// Quote spread (half-spread on each side).
    half_spread: f64,
    /// Quote size.
    quote_size: f64,
    /// Maximum position.
    max_position: f64,
    /// Requote interval.
    requote_interval_ns: Nanos,
    /// Current bid order ID.
    bid_order_id: Option<u64>,
    /// Current ask order ID.
    ask_order_id: Option<u64>,
    /// Last mid price.
    last_mid: Option<f64>,
    /// Order counter for client IDs.
    order_counter: u64,
    /// Statistics.
    pub stats: MarketMakerStats,
}

#[derive(Debug, Clone, Default)]
pub struct MarketMakerStats {
    pub quotes_sent: u64,
    pub fills_received: u64,
    pub total_volume: f64,
    pub total_pnl: f64,
    pub rejects: u64,
}

impl MarketMakerStrategy {
    pub fn new(params: &StrategyParams) -> Self {
        Self {
            name: "MarketMaker".into(),
            token_id: params
                .get_string("token_id")
                .unwrap_or("default")
                .to_string(),
            half_spread: params.get_or("half_spread", 0.02),
            quote_size: params.get_or("quote_size", 100.0),
            max_position: params.get_or("max_position", 500.0),
            requote_interval_ns: (params.get_or("requote_interval_sec", 1.0) * NANOS_PER_SEC as f64)
                as Nanos,
            bid_order_id: None,
            ask_order_id: None,
            last_mid: None,
            order_counter: 0,
            stats: MarketMakerStats::default(),
        }
    }

    fn generate_client_id(&mut self, prefix: &str) -> String {
        self.order_counter += 1;
        format!("{}_{}", prefix, self.order_counter)
    }

    fn update_quotes(&mut self, ctx: &mut StrategyContext, mid_price: f64) {
        let position = ctx.orders.get_position(&self.token_id);

        // Cancel existing orders if price moved significantly
        if let Some(last) = self.last_mid {
            if (mid_price - last).abs() > self.half_spread * 0.5 {
                self.cancel_all_quotes(ctx);
            }
        }

        self.last_mid = Some(mid_price);

        // Calculate skewed quotes based on position
        let position_skew = position.shares / self.max_position;
        let bid_skew = -position_skew * self.half_spread * 0.5;
        let ask_skew = position_skew * self.half_spread * 0.5;

        let bid_price = (mid_price - self.half_spread + bid_skew).max(0.01);
        let ask_price = (mid_price + self.half_spread + ask_skew).min(0.99);

        // Send bid if we don't have one and position allows
        if self.bid_order_id.is_none() && position.shares < self.max_position {
            let size = (self.max_position - position.shares).min(self.quote_size);
            if size > 0.0 {
                let order = StrategyOrder::limit(
                    self.generate_client_id("bid"),
                    &self.token_id,
                    Side::Buy,
                    bid_price,
                    size,
                )
                .post_only();

                if let Ok(id) = ctx.orders.send_order(order) {
                    self.bid_order_id = Some(id);
                    self.stats.quotes_sent += 1;
                }
            }
        }

        // Send ask if we don't have one and position allows
        if self.ask_order_id.is_none() && position.shares > -self.max_position {
            let size = (self.max_position + position.shares).min(self.quote_size);
            if size > 0.0 {
                let order = StrategyOrder::limit(
                    self.generate_client_id("ask"),
                    &self.token_id,
                    Side::Sell,
                    ask_price,
                    size,
                )
                .post_only();

                if let Ok(id) = ctx.orders.send_order(order) {
                    self.ask_order_id = Some(id);
                    self.stats.quotes_sent += 1;
                }
            }
        }
    }

    fn cancel_all_quotes(&mut self, ctx: &mut StrategyContext) {
        if let Some(bid_id) = self.bid_order_id.take() {
            let _ = ctx
                .orders
                .send_cancel(crate::backtest_v2::strategy::StrategyCancel {
                    order_id: bid_id,
                    client_order_id: None,
                });
        }
        if let Some(ask_id) = self.ask_order_id.take() {
            let _ = ctx
                .orders
                .send_cancel(crate::backtest_v2::strategy::StrategyCancel {
                    order_id: ask_id,
                    client_order_id: None,
                });
        }
    }
}

impl Strategy for MarketMakerStrategy {
    fn on_book_update(&mut self, ctx: &mut StrategyContext, book: &BookSnapshot) {
        if book.token_id != self.token_id {
            return;
        }

        if let Some(mid) = book.mid_price() {
            self.update_quotes(ctx, mid);
        }
    }

    fn on_trade(&mut self, _ctx: &mut StrategyContext, _trade: &TradePrint) {
        // Could update fair value model here
    }

    fn on_timer(&mut self, ctx: &mut StrategyContext, _timer: &TimerEvent) {
        // Periodic requote check
        if let Some(mid) = self.last_mid {
            self.update_quotes(ctx, mid);
        }

        // Schedule next timer
        ctx.orders
            .schedule_timer(self.requote_interval_ns, Some("requote".into()));
    }

    fn on_order_ack(&mut self, _ctx: &mut StrategyContext, _ack: &OrderAck) {
        // Order confirmed, nothing to do
    }

    fn on_order_reject(&mut self, _ctx: &mut StrategyContext, reject: &OrderReject) {
        self.stats.rejects += 1;

        // Clear our tracking if the rejected order was ours
        // (In production, would match by client_order_id)
        if Some(reject.order_id) == self.bid_order_id {
            self.bid_order_id = None;
        }
        if Some(reject.order_id) == self.ask_order_id {
            self.ask_order_id = None;
        }
    }

    fn on_fill(&mut self, ctx: &mut StrategyContext, fill: &FillNotification) {
        self.stats.fills_received += 1;
        self.stats.total_volume += fill.size * fill.price;

        // Update PnL
        let pnl = if fill.is_maker {
            -fill.fee // Maker rebate
        } else {
            -fill.fee // Taker fee
        };
        self.stats.total_pnl += pnl;

        // Clear order ID if fully filled
        if fill.leaves_qty <= 0.0 {
            if Some(fill.order_id) == self.bid_order_id {
                self.bid_order_id = None;
            }
            if Some(fill.order_id) == self.ask_order_id {
                self.ask_order_id = None;
            }
        }

        // Immediately update quotes after fill
        if let Some(mid) = self.last_mid {
            self.update_quotes(ctx, mid);
        }
    }

    fn on_cancel_ack(&mut self, _ctx: &mut StrategyContext, ack: &CancelAck) {
        if Some(ack.order_id) == self.bid_order_id {
            self.bid_order_id = None;
        }
        if Some(ack.order_id) == self.ask_order_id {
            self.ask_order_id = None;
        }
    }

    fn on_start(&mut self, ctx: &mut StrategyContext) {
        // Schedule first requote timer
        ctx.orders
            .schedule_timer(self.requote_interval_ns, Some("requote".into()));
    }

    fn on_stop(&mut self, ctx: &mut StrategyContext) {
        self.cancel_all_quotes(ctx);
    }

    fn name(&self) -> &str {
        &self.name
    }
}

/// Simple momentum strategy example.
///
/// Trades in direction of recent price movement.
pub struct MomentumStrategy {
    name: String,
    token_id: String,
    /// Lookback period for momentum calculation.
    lookback: usize,
    /// Entry threshold (price change %).
    threshold: f64,
    /// Position size.
    position_size: f64,
    /// Recent prices.
    prices: Vec<f64>,
    /// Current position.
    current_order_id: Option<u64>,
    order_counter: u64,
    pub stats: MomentumStats,
}

#[derive(Debug, Clone, Default)]
pub struct MomentumStats {
    pub signals: u64,
    pub trades: u64,
    pub total_pnl: f64,
}

impl MomentumStrategy {
    pub fn new(params: &StrategyParams) -> Self {
        Self {
            name: "Momentum".into(),
            token_id: params
                .get_string("token_id")
                .unwrap_or("default")
                .to_string(),
            lookback: params.get_or("lookback", 10.0) as usize,
            threshold: params.get_or("threshold", 0.02),
            position_size: params.get_or("position_size", 100.0),
            prices: Vec::with_capacity(100),
            current_order_id: None,
            order_counter: 0,
            stats: MomentumStats::default(),
        }
    }

    fn generate_client_id(&mut self) -> String {
        self.order_counter += 1;
        format!("mom_{}", self.order_counter)
    }

    fn calculate_momentum(&self) -> Option<f64> {
        if self.prices.len() < self.lookback {
            return None;
        }

        let recent = &self.prices[self.prices.len() - self.lookback..];
        let first = recent.first()?;
        let last = recent.last()?;

        if *first > 0.0 {
            Some((last - first) / first)
        } else {
            None
        }
    }
}

impl Strategy for MomentumStrategy {
    fn on_book_update(&mut self, ctx: &mut StrategyContext, book: &BookSnapshot) {
        if book.token_id != self.token_id {
            return;
        }

        let Some(mid) = book.mid_price() else {
            return;
        };

        // Record price
        self.prices.push(mid);
        if self.prices.len() > self.lookback * 2 {
            self.prices.remove(0);
        }

        // Calculate momentum
        let Some(momentum) = self.calculate_momentum() else {
            return;
        };

        // Check for signal
        let position = ctx.orders.get_position(&self.token_id);

        if momentum > self.threshold && position.shares <= 0.0 {
            // Bullish signal - go long
            self.stats.signals += 1;

            if self.current_order_id.is_none() {
                let order = StrategyOrder::limit(
                    self.generate_client_id(),
                    &self.token_id,
                    Side::Buy,
                    mid * 1.001, // Slightly aggressive
                    self.position_size,
                )
                .ioc();

                if let Ok(id) = ctx.orders.send_order(order) {
                    self.current_order_id = Some(id);
                }
            }
        } else if momentum < -self.threshold && position.shares >= 0.0 {
            // Bearish signal - go short
            self.stats.signals += 1;

            if self.current_order_id.is_none() {
                let order = StrategyOrder::limit(
                    self.generate_client_id(),
                    &self.token_id,
                    Side::Sell,
                    mid * 0.999, // Slightly aggressive
                    self.position_size,
                )
                .ioc();

                if let Ok(id) = ctx.orders.send_order(order) {
                    self.current_order_id = Some(id);
                }
            }
        }
    }

    fn on_trade(&mut self, _ctx: &mut StrategyContext, _trade: &TradePrint) {}
    fn on_timer(&mut self, _ctx: &mut StrategyContext, _timer: &TimerEvent) {}
    fn on_order_ack(&mut self, _ctx: &mut StrategyContext, _ack: &OrderAck) {}

    fn on_order_reject(&mut self, _ctx: &mut StrategyContext, reject: &OrderReject) {
        if Some(reject.order_id) == self.current_order_id {
            self.current_order_id = None;
        }
    }

    fn on_fill(&mut self, _ctx: &mut StrategyContext, fill: &FillNotification) {
        self.stats.trades += 1;
        self.stats.total_pnl -= fill.fee;

        if fill.leaves_qty <= 0.0 {
            self.current_order_id = None;
        }
    }

    fn on_cancel_ack(&mut self, _ctx: &mut StrategyContext, ack: &CancelAck) {
        if Some(ack.order_id) == self.current_order_id {
            self.current_order_id = None;
        }
    }

    fn name(&self) -> &str {
        &self.name
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_market_maker_creation() {
        let params = StrategyParams::new()
            .with_string("token_id", "BTC-UPDOWN")
            .with_param("half_spread", 0.01)
            .with_param("quote_size", 50.0);

        let strategy = MarketMakerStrategy::new(&params);
        assert_eq!(strategy.token_id, "BTC-UPDOWN");
        assert_eq!(strategy.half_spread, 0.01);
        assert_eq!(strategy.quote_size, 50.0);
    }

    #[test]
    fn test_momentum_creation() {
        let params = StrategyParams::new()
            .with_string("token_id", "ETH-UPDOWN")
            .with_param("lookback", 20.0)
            .with_param("threshold", 0.03);

        let strategy = MomentumStrategy::new(&params);
        assert_eq!(strategy.token_id, "ETH-UPDOWN");
        assert_eq!(strategy.lookback, 20);
        assert_eq!(strategy.threshold, 0.03);
    }
}

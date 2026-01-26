//! Simulated Adapter
//!
//! OrderSender implementation for backtesting that routes to the matching simulator.
//! NOW WITH OMS PARITY: Uses OrderManagementSystem for validation, rate limiting,
//! and state tracking to ensure backtest behavior matches live trading.
//!
//! # Strict Accounting Mode
//!
//! When `strict_accounting=true`, the `process_fill()` method is FORBIDDEN.
//! Use `process_fill_oms_only()` for OMS state updates, and route all economic
//! state changes through the double-entry ledger.

use crate::backtest_v2::clock::Nanos;
use crate::guard_direct_mutation;
use crate::backtest_v2::events::{Event, OrderId, OrderType, Side, Size, TimeInForce, TimestampedEvent};
use crate::backtest_v2::latency::{LatencyConfig, LatencySampler};
use crate::backtest_v2::matching::{
    CancelRequest, LimitOrderBook, MatchingConfig, MatchingEngine, OrderRequest,
};
use crate::backtest_v2::oms::{MarketStatus, OrderManagementSystem, OmsStats, VenueConstraints};
use crate::backtest_v2::queue::StreamSource;
use crate::backtest_v2::strategy::{
    OpenOrder, OrderSender, Position, StrategyCancel, StrategyOrder,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// OMS parity mode for backtest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OmsParityMode {
    /// Full OMS parity: all validation, rate limits, and state tracking enforced.
    /// This is the ONLY production-valid mode.
    Full,
    /// Relaxed mode: OMS validation enabled but does not block.
    /// Results are marked INVALID for production use.
    /// Useful for debugging strategy logic without OMS constraints.
    Relaxed,
    /// Bypass mode: OMS completely bypassed (legacy behavior).
    /// Results are marked INVALID. Use only for testing the matching engine.
    Bypass,
}

impl Default for OmsParityMode {
    fn default() -> Self {
        Self::Full
    }
}

impl OmsParityMode {
    /// Whether this mode produces valid results for production use.
    pub fn is_valid_for_production(&self) -> bool {
        matches!(self, Self::Full)
    }

    pub fn description(&self) -> &'static str {
        match self {
            Self::Full => "Full OMS parity - production-grade validation and rate limiting",
            Self::Relaxed => "Relaxed OMS - validation logged but not enforced (INVALID for production)",
            Self::Bypass => "OMS bypass - legacy mode, no validation (INVALID for production)",
        }
    }
}

/// OMS parity statistics for backtest results.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OmsParityStats {
    /// OMS parity mode used.
    pub mode: OmsParityMode,
    /// Whether results are valid for production use.
    pub valid_for_production: bool,
    /// Orders that would have been rejected by OMS in live.
    pub would_reject_count: u64,
    /// Orders rejected by rate limiting.
    pub rate_limited_orders: u64,
    /// Cancels rejected by rate limiting.
    pub rate_limited_cancels: u64,
    /// Validation failures (price, size, market status, etc.).
    pub validation_failures: u64,
    /// Duplicate client order IDs attempted.
    pub duplicate_client_ids: u64,
    /// Orders on non-open markets.
    pub market_status_rejects: u64,
    /// Full OMS statistics (if available).
    pub oms_stats: Option<OmsStats>,
}

/// Simulated order sender for backtesting.
/// Now with OMS parity enforcement for production-grade backtests.
pub struct SimulatedOrderSender {
    /// Current simulation time.
    current_time: Nanos,
    /// Matching engine.
    matching: MatchingEngine,
    /// Latency sampler.
    latency: LatencySampler,
    /// Strategy's trader ID for matching.
    trader_id: String,
    /// OMS for validation, rate limiting, and state tracking.
    oms: OrderManagementSystem,
    /// OMS parity mode.
    oms_parity_mode: OmsParityMode,
    /// OMS parity statistics.
    oms_parity_stats: OmsParityStats,
    /// Positions by token.
    positions: HashMap<String, Position>,
    /// Open orders (maintained for compatibility).
    open_orders: HashMap<OrderId, OpenOrderInternal>,
    /// Pending events to deliver.
    pending_events: Vec<TimestampedEvent>,
    /// Next timer ID.
    next_timer_id: u64,
    /// Scheduled timers.
    timers: HashMap<u64, ScheduledTimer>,
}

#[derive(Debug, Clone)]
struct OpenOrderInternal {
    order_id: OrderId,
    client_order_id: String,
    token_id: String,
    side: Side,
    price: f64,
    original_size: Size,
    remaining_size: Size,
    created_at: Nanos,
}

#[derive(Debug, Clone)]
pub struct ScheduledTimer {
    pub timer_id: u64,
    pub fire_time: Nanos,
    pub payload: Option<String>,
}

impl SimulatedOrderSender {
    /// Create a new simulated order sender with full OMS parity (default).
    pub fn new(
        matching_config: MatchingConfig,
        latency_config: LatencyConfig,
        trader_id: impl Into<String>,
        seed: u64,
    ) -> Self {
        Self::with_oms_parity(
            matching_config,
            latency_config,
            trader_id,
            seed,
            VenueConstraints::polymarket(),
            OmsParityMode::Full,
        )
    }

    /// Create with custom venue constraints and OMS parity mode.
    pub fn with_oms_parity(
        matching_config: MatchingConfig,
        latency_config: LatencyConfig,
        trader_id: impl Into<String>,
        seed: u64,
        venue_constraints: VenueConstraints,
        oms_parity_mode: OmsParityMode,
    ) -> Self {
        let oms = OrderManagementSystem::new(venue_constraints);
        Self {
            current_time: 0,
            matching: MatchingEngine::new(matching_config),
            latency: LatencySampler::new(latency_config, seed),
            trader_id: trader_id.into(),
            oms,
            oms_parity_mode,
            oms_parity_stats: OmsParityStats {
                mode: oms_parity_mode,
                valid_for_production: oms_parity_mode.is_valid_for_production(),
                ..Default::default()
            },
            positions: HashMap::new(),
            open_orders: HashMap::new(),
            pending_events: Vec::new(),
            next_timer_id: 1,
            timers: HashMap::new(),
        }
    }

    /// Create with legacy bypass mode (for tests that need no OMS validation).
    pub fn new_bypass(
        matching_config: MatchingConfig,
        latency_config: LatencyConfig,
        trader_id: impl Into<String>,
        seed: u64,
    ) -> Self {
        Self::with_oms_parity(
            matching_config,
            latency_config,
            trader_id,
            seed,
            VenueConstraints::default(),
            OmsParityMode::Bypass,
        )
    }

    /// Get OMS parity statistics.
    pub fn oms_parity_stats(&self) -> &OmsParityStats {
        &self.oms_parity_stats
    }

    /// Get OMS statistics.
    pub fn oms_stats(&self) -> OmsStats {
        self.oms.stats.clone()
    }

    /// Set market status for a token.
    pub fn set_market_status(&mut self, token_id: &str, status: MarketStatus) {
        self.oms.set_market_status(token_id, status);
    }

    /// Get market status for a token.
    pub fn get_market_status(&self, token_id: &str) -> MarketStatus {
        self.oms.get_market_status(token_id)
    }

    /// Set current simulation time.
    pub fn set_time(&mut self, time: Nanos) {
        self.current_time = time;
    }

    /// Get pending events and clear the queue.
    pub fn take_pending_events(&mut self) -> Vec<TimestampedEvent> {
        std::mem::take(&mut self.pending_events)
    }

    /// Get matching engine reference.
    pub fn matching_engine(&self) -> &MatchingEngine {
        &self.matching
    }

    /// Get mutable matching engine reference.
    pub fn matching_engine_mut(&mut self) -> &mut MatchingEngine {
        &mut self.matching
    }

    /// Process a fill event (update positions and OMS state).
    /// 
    /// # Strict Accounting Mode
    /// 
    /// This method is FORBIDDEN when `strict_accounting=true`.
    /// Use `process_fill_oms_only()` for OMS state, and route economic
    /// state changes through `Ledger::post_fill()`.
    pub fn process_fill(
        &mut self,
        order_id: OrderId,
        price: f64,
        size: Size,
        is_maker: bool,
        leaves_qty: Size,
        fee: f64,
    ) {
        guard_direct_mutation!("SimAdapter::process_fill");
        
        // Notify OMS of fill (for state tracking)
        self.oms.on_fill(order_id, size, price, fee, self.current_time);

        // Get the open order
        let Some(order) = self.open_orders.get_mut(&order_id) else {
            return;
        };

        // Update remaining size
        order.remaining_size = leaves_qty;

        // Update position
        let position = self
            .positions
            .entry(order.token_id.clone())
            .or_insert_with(|| Position {
                token_id: order.token_id.clone(),
                ..Default::default()
            });

        let signed_size = match order.side {
            Side::Buy => size,
            Side::Sell => -size,
        };

        // Update shares
        let old_shares = position.shares;
        position.shares += signed_size;

        // Update cost basis (simplified)
        if signed_size > 0.0 {
            // Buying: add to cost basis
            position.cost_basis += size * price + fee;
        } else {
            // Selling: realize PnL
            let avg_cost = if old_shares.abs() > 0.0 {
                position.cost_basis / old_shares.abs()
            } else {
                price
            };
            let pnl = size * (price - avg_cost) - fee;
            position.realized_pnl += pnl;
            position.cost_basis -= size * avg_cost;
        }

        // Remove order if fully filled
        if leaves_qty <= 0.0 {
            self.open_orders.remove(&order_id);
        }
    }

    /// Process a fill event - OMS state ONLY (for strict_accounting mode).
    /// 
    /// In strict_accounting mode, position/PnL changes go EXCLUSIVELY through the ledger.
    /// This method only updates OMS state and open order tracking.
    /// 
    /// DO NOT call this for position accounting - use the Ledger for that.
    pub fn process_fill_oms_only(
        &mut self,
        order_id: OrderId,
        leaves_qty: Size,
    ) {
        // Notify OMS of fill (for state tracking)
        // Note: We pass dummy values for price/size/fee since we only care about OMS state
        self.oms.on_fill(order_id, 0.0, 0.0, 0.0, self.current_time);

        // Get the open order and update remaining size
        if let Some(order) = self.open_orders.get_mut(&order_id) {
            order.remaining_size = leaves_qty;
        }

        // Remove order if fully filled
        if leaves_qty <= 0.0 {
            self.open_orders.remove(&order_id);
        }
    }

    /// Process a cancel ack (remove from open orders and update OMS).
    pub fn process_cancel_ack(&mut self, order_id: OrderId, cancelled_qty: Size) {
        // Notify OMS of cancel
        self.oms.on_cancel_ack(order_id, cancelled_qty, self.current_time);
        self.open_orders.remove(&order_id);
    }

    /// Process an order ack (update OMS state).
    pub fn process_order_ack(&mut self, order_id: OrderId) {
        self.oms.on_order_ack(order_id, self.current_time);
    }

    /// Process an order reject (update OMS state and remove from open orders).
    pub fn process_order_reject(&mut self, order_id: OrderId, reason: &str) {
        self.oms.on_order_reject(order_id, reason.to_string(), self.current_time);
        self.open_orders.remove(&order_id);
    }

    /// Check and fire timers.
    pub fn check_timers(&mut self) -> Vec<ScheduledTimer> {
        let current = self.current_time;
        let mut fired = Vec::new();

        self.timers.retain(|_, timer| {
            if timer.fire_time <= current {
                fired.push(timer.clone());
                false
            } else {
                true
            }
        });

        fired
    }

    /// Get latency sampler for external use.
    pub fn latency_sampler(&mut self) -> &mut LatencySampler {
        &mut self.latency
    }
}

impl OrderSender for SimulatedOrderSender {
    fn send_order(&mut self, order: StrategyOrder) -> Result<OrderId, String> {
        // === OMS PARITY: Create order through OMS for validation ===
        let oms_result = if self.oms_parity_mode != OmsParityMode::Bypass {
            self.oms.create_order(
                order.client_order_id.clone(),
                order.token_id.clone(),
                order.side,
                order.order_type,
                order.time_in_force,
                order.price,
                order.size,
                order.post_only,
                order.reduce_only,
                self.current_time,
            )
        } else {
            // Bypass mode: generate our own order ID
            Ok(self.oms.stats.orders_created + 1)
        };

        let order_id = match oms_result {
            Ok(id) => id,
            Err(e) => {
                // Track validation failure
                self.oms_parity_stats.would_reject_count += 1;
                self.oms_parity_stats.validation_failures += 1;
                
                match self.oms_parity_mode {
                    OmsParityMode::Full => {
                        // Full parity: reject the order
                        return Err(format!("OMS validation failed: {}", e.message));
                    }
                    OmsParityMode::Relaxed | OmsParityMode::Bypass => {
                        // Relaxed/Bypass: log but continue (results marked invalid)
                        self.oms_parity_stats.valid_for_production = false;
                        // Generate fallback order ID
                        self.oms.stats.orders_created + 1
                    }
                }
            }
        };

        // === OMS PARITY: Send order through OMS for rate limiting ===
        if self.oms_parity_mode != OmsParityMode::Bypass {
            if let Err(e) = self.oms.send_order(order_id, self.current_time) {
                self.oms_parity_stats.would_reject_count += 1;
                if e.message.contains("rate limit") {
                    self.oms_parity_stats.rate_limited_orders += 1;
                }
                
                match self.oms_parity_mode {
                    OmsParityMode::Full => {
                        return Err(format!("OMS send failed: {}", e.message));
                    }
                    OmsParityMode::Relaxed | OmsParityMode::Bypass => {
                        self.oms_parity_stats.valid_for_production = false;
                    }
                }
            }
        }

        // Sample latencies
        let order_send_latency = self.latency.sample_order_send();
        let venue_latency = self.latency.sample_venue_process();
        let total_latency = order_send_latency + venue_latency;

        // Create matching request
        let matching_req = OrderRequest {
            client_order_id: order.client_order_id.clone(),
            token_id: order.token_id.clone(),
            side: order.side,
            price: order.price,
            size: order.size,
            order_type: order.order_type,
            time_in_force: order.time_in_force,
            trader_id: self.trader_id.clone(),
            post_only: order.post_only,
            reduce_only: order.reduce_only,
        };

        // Submit to matching engine (at future time)
        let submit_time = self.current_time + total_latency;
        let events = self.matching.submit_order(matching_req, submit_time);

        // Track open order
        self.open_orders.insert(
            order_id,
            OpenOrderInternal {
                order_id,
                client_order_id: order.client_order_id,
                token_id: order.token_id,
                side: order.side,
                price: order.price,
                original_size: order.size,
                remaining_size: order.size,
                created_at: self.current_time,
            },
        );

        // Queue events for delivery
        self.pending_events.extend(events);

        Ok(order_id)
    }

    fn send_cancel(&mut self, cancel: StrategyCancel) -> Result<(), String> {
        let order_id = cancel.order_id;

        // Get token ID from open orders
        let Some(order) = self.open_orders.get(&order_id) else {
            return Err("Order not found".into());
        };
        let token_id = order.token_id.clone();

        // === OMS PARITY: Request cancel through OMS for rate limiting ===
        if self.oms_parity_mode != OmsParityMode::Bypass {
            if let Err(e) = self.oms.request_cancel(order_id, self.current_time) {
                self.oms_parity_stats.would_reject_count += 1;
                if e.message.contains("rate limit") {
                    self.oms_parity_stats.rate_limited_cancels += 1;
                }
                
                match self.oms_parity_mode {
                    OmsParityMode::Full => {
                        return Err(format!("OMS cancel failed: {}", e.message));
                    }
                    OmsParityMode::Relaxed | OmsParityMode::Bypass => {
                        self.oms_parity_stats.valid_for_production = false;
                    }
                }
            }
        }

        // Sample cancel latency
        let cancel_latency = self.latency.sample_cancel();
        let cancel_time = self.current_time + cancel_latency;

        // Submit cancel to matching engine
        let cancel_req = CancelRequest {
            order_id,
            client_order_id: cancel.client_order_id,
        };

        let events = self
            .matching
            .cancel_order(&token_id, cancel_req, cancel_time);
        self.pending_events.extend(events);

        Ok(())
    }

    fn cancel_all(&mut self, token_id: &str) -> Result<usize, String> {
        let orders_to_cancel: Vec<OrderId> = self
            .open_orders
            .values()
            .filter(|o| o.token_id == token_id)
            .map(|o| o.order_id)
            .collect();

        let count = orders_to_cancel.len();

        for order_id in orders_to_cancel {
            let _ = self.send_cancel(StrategyCancel {
                order_id,
                client_order_id: None,
            });
        }

        Ok(count)
    }

    fn get_position(&self, token_id: &str) -> Position {
        self.positions
            .get(token_id)
            .cloned()
            .unwrap_or_else(|| Position {
                token_id: token_id.to_string(),
                ..Default::default()
            })
    }

    fn get_all_positions(&self) -> HashMap<String, Position> {
        self.positions.clone()
    }

    fn get_open_orders(&self) -> Vec<OpenOrder> {
        self.open_orders
            .values()
            .map(|o| OpenOrder {
                order_id: o.order_id,
                client_order_id: o.client_order_id.clone(),
                token_id: o.token_id.clone(),
                side: o.side,
                price: o.price,
                original_size: o.original_size,
                remaining_size: o.remaining_size,
                created_at: o.created_at,
            })
            .collect()
    }

    fn now(&self) -> Nanos {
        self.current_time
    }

    fn schedule_timer(&mut self, delay_ns: Nanos, payload: Option<String>) -> u64 {
        let timer_id = self.next_timer_id;
        self.next_timer_id += 1;

        self.timers.insert(
            timer_id,
            ScheduledTimer {
                timer_id,
                fire_time: self.current_time + delay_ns,
                payload,
            },
        );

        timer_id
    }

    fn cancel_timer(&mut self, timer_id: u64) -> bool {
        self.timers.remove(&timer_id).is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backtest_v2::events::TimeInForce;

    #[test]
    fn test_simulated_order_sender() {
        // Use bypass mode for basic matching engine tests
        let mut sender = SimulatedOrderSender::new_bypass(
            MatchingConfig::default(),
            LatencyConfig::default(),
            "test_trader",
            42,
        );

        sender.set_time(1_000_000_000);

        // Send an order
        let order = StrategyOrder::limit("order1", "token123", Side::Buy, 0.50, 100.0);

        let order_id = sender.send_order(order).unwrap();
        assert_eq!(order_id, 1);

        // Should have open order
        let open = sender.get_open_orders();
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].client_order_id, "order1");
    }

    #[test]
    fn test_position_tracking() {
        // Use bypass mode for basic matching engine tests
        let mut sender = SimulatedOrderSender::new_bypass(
            MatchingConfig::default(),
            LatencyConfig::default(),
            "test_trader",
            42,
        );

        sender.set_time(1_000_000_000);

        // Send buy order
        let order = StrategyOrder::limit("order1", "token123", Side::Buy, 0.50, 100.0);
        let order_id = sender.send_order(order).unwrap();

        // Simulate fill
        sender.process_fill(order_id, 0.50, 100.0, false, 0.0, 0.05);

        // Check position
        let pos = sender.get_position("token123");
        assert_eq!(pos.shares, 100.0);
    }

    #[test]
    fn test_timer_scheduling() {
        // Use bypass mode for basic matching engine tests
        let mut sender = SimulatedOrderSender::new_bypass(
            MatchingConfig::default(),
            LatencyConfig::default(),
            "test_trader",
            42,
        );

        sender.set_time(1_000_000_000);

        // Schedule timer for 100ms later
        let timer_id = sender.schedule_timer(100_000_000, Some("test".into()));
        assert_eq!(timer_id, 1);

        // Timer shouldn't fire yet
        let fired = sender.check_timers();
        assert!(fired.is_empty());

        // Advance time
        sender.set_time(1_100_000_000);

        // Timer should fire now
        let fired = sender.check_timers();
        assert_eq!(fired.len(), 1);
        assert_eq!(fired[0].payload, Some("test".into()));
    }

    #[test]
    fn test_oms_parity_full_mode_validates() {
        // Full OMS parity mode should enforce validation
        let mut sender = SimulatedOrderSender::new(
            MatchingConfig::default(),
            LatencyConfig::default(),
            "test_trader",
            42,
        );

        sender.set_time(1_000_000_000);

        // Order with invalid price (outside 0.01-0.99 range)
        let order = StrategyOrder::limit("order1", "token123", Side::Buy, 1.50, 100.0);
        let result = sender.send_order(order);
        
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("validation failed"));
    }

    #[test]
    fn test_oms_parity_relaxed_mode_allows_but_marks_invalid() {
        // Relaxed mode should allow but mark results invalid
        let mut sender = SimulatedOrderSender::with_oms_parity(
            MatchingConfig::default(),
            LatencyConfig::default(),
            "test_trader",
            42,
            VenueConstraints::polymarket(),
            OmsParityMode::Relaxed,
        );

        sender.set_time(1_000_000_000);

        // Order with invalid price
        let order = StrategyOrder::limit("order1", "token123", Side::Buy, 1.50, 100.0);
        let result = sender.send_order(order);
        
        // Should succeed in relaxed mode
        assert!(result.is_ok());
        
        // But results should be marked invalid
        assert!(!sender.oms_parity_stats().valid_for_production);
        assert!(sender.oms_parity_stats().validation_failures > 0);
    }

    #[test]
    fn test_oms_parity_stats() {
        let sender = SimulatedOrderSender::new(
            MatchingConfig::default(),
            LatencyConfig::default(),
            "test_trader",
            42,
        );

        let stats = sender.oms_parity_stats();
        assert_eq!(stats.mode, OmsParityMode::Full);
        assert!(stats.valid_for_production);
    }
}

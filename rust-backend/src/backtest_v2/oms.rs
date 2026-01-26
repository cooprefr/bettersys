//! Order Management System
//!
//! Realistic OMS with state machine, venue constraints, rate limiting, and throttling.
//! Handles out-of-order messages, rejects, partial fills, and cancel races.

use crate::backtest_v2::clock::Nanos;
use crate::backtest_v2::events::{
    OrderId, OrderType, Price, RejectReason, Side, Size, TimeInForce,
};
use crate::backtest_v2::matching::PriceTicks;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};

/// Order state machine states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderState {
    /// Order created but not yet sent.
    New,
    /// Order sent, waiting for ack from venue.
    PendingAck,
    /// Order acknowledged and live on the book.
    Live,
    /// Order partially filled and still live.
    PartiallyFilled,
    /// Cancel request sent, waiting for ack.
    PendingCancel,
    /// Order is done (filled, cancelled, rejected, or expired).
    Done,
}

impl OrderState {
    pub fn is_terminal(&self) -> bool {
        matches!(self, OrderState::Done)
    }

    pub fn is_active(&self) -> bool {
        matches!(
            self,
            OrderState::PendingAck
                | OrderState::Live
                | OrderState::PartiallyFilled
                | OrderState::PendingCancel
        )
    }

    pub fn can_cancel(&self) -> bool {
        matches!(self, OrderState::Live | OrderState::PartiallyFilled)
    }
}

/// Terminal state reason.
#[derive(Debug, Clone, PartialEq)]
pub enum TerminalReason {
    Filled,
    Cancelled,
    Rejected { reason: String },
    Expired,
    CancelRejected,
    MarketHalted,
    MarketResolved,
}

/// Order record in the OMS.
#[derive(Debug, Clone)]
pub struct OmsOrder {
    pub order_id: OrderId,
    pub client_order_id: String,
    pub token_id: String,
    pub side: Side,
    pub order_type: OrderType,
    pub time_in_force: TimeInForce,
    pub price: Price,
    pub original_qty: Size,
    pub filled_qty: Size,
    pub remaining_qty: Size,
    pub avg_fill_price: Price,
    pub total_fees: f64,
    pub state: OrderState,
    pub terminal_reason: Option<TerminalReason>,
    /// Timestamps
    pub created_at: Nanos,
    pub sent_at: Option<Nanos>,
    pub acked_at: Option<Nanos>,
    pub last_fill_at: Option<Nanos>,
    pub done_at: Option<Nanos>,
    /// Flags
    pub post_only: bool,
    pub reduce_only: bool,
    /// Cancel tracking
    pub cancel_sent_at: Option<Nanos>,
    pub cancel_request_id: Option<u64>,
}

impl OmsOrder {
    pub fn new(
        order_id: OrderId,
        client_order_id: String,
        token_id: String,
        side: Side,
        order_type: OrderType,
        time_in_force: TimeInForce,
        price: Price,
        qty: Size,
        post_only: bool,
        reduce_only: bool,
        created_at: Nanos,
    ) -> Self {
        Self {
            order_id,
            client_order_id,
            token_id,
            side,
            order_type,
            time_in_force,
            price,
            original_qty: qty,
            filled_qty: 0.0,
            remaining_qty: qty,
            avg_fill_price: 0.0,
            total_fees: 0.0,
            state: OrderState::New,
            terminal_reason: None,
            created_at,
            sent_at: None,
            acked_at: None,
            last_fill_at: None,
            done_at: None,
            post_only,
            reduce_only,
            cancel_sent_at: None,
            cancel_request_id: None,
        }
    }

    /// Apply a fill to this order.
    pub fn apply_fill(&mut self, fill_qty: Size, fill_price: Price, fee: f64, now: Nanos) -> bool {
        if self.state.is_terminal() {
            return false;
        }

        let actual_fill = fill_qty.min(self.remaining_qty);
        if actual_fill <= 0.0 {
            return false;
        }

        // Update average fill price
        let old_value = self.avg_fill_price * self.filled_qty;
        let new_value = old_value + fill_price * actual_fill;
        self.filled_qty += actual_fill;
        self.remaining_qty -= actual_fill;
        self.avg_fill_price = if self.filled_qty > 0.0 {
            new_value / self.filled_qty
        } else {
            0.0
        };
        self.total_fees += fee;
        self.last_fill_at = Some(now);

        // Update state
        if self.remaining_qty <= 0.0 {
            self.state = OrderState::Done;
            self.terminal_reason = Some(TerminalReason::Filled);
            self.done_at = Some(now);
        } else if self.state == OrderState::Live {
            self.state = OrderState::PartiallyFilled;
        }

        true
    }

    /// Transition to acked state.
    pub fn ack(&mut self, now: Nanos) -> bool {
        if self.state != OrderState::PendingAck {
            return false;
        }
        self.state = OrderState::Live;
        self.acked_at = Some(now);
        true
    }

    /// Transition to rejected state.
    pub fn reject(&mut self, reason: String, now: Nanos) -> bool {
        if self.state.is_terminal() {
            return false;
        }
        self.state = OrderState::Done;
        self.terminal_reason = Some(TerminalReason::Rejected { reason });
        self.done_at = Some(now);
        true
    }

    /// Request cancel.
    pub fn request_cancel(&mut self, request_id: u64, now: Nanos) -> bool {
        if !self.state.can_cancel() {
            return false;
        }
        self.state = OrderState::PendingCancel;
        self.cancel_sent_at = Some(now);
        self.cancel_request_id = Some(request_id);
        true
    }

    /// Cancel acknowledged.
    pub fn cancel_ack(&mut self, cancelled_qty: Size, now: Nanos) -> bool {
        if self.state != OrderState::PendingCancel
            && self.state != OrderState::Live
            && self.state != OrderState::PartiallyFilled
        {
            return false;
        }
        self.remaining_qty = 0.0;
        self.state = OrderState::Done;
        self.terminal_reason = Some(TerminalReason::Cancelled);
        self.done_at = Some(now);
        true
    }

    /// Cancel rejected.
    pub fn cancel_reject(&mut self, now: Nanos) -> bool {
        if self.state != OrderState::PendingCancel {
            return false;
        }
        // Revert to previous state
        if self.filled_qty > 0.0 {
            self.state = OrderState::PartiallyFilled;
        } else {
            self.state = OrderState::Live;
        }
        self.cancel_sent_at = None;
        self.cancel_request_id = None;
        true
    }

    /// Mark as sent.
    pub fn mark_sent(&mut self, now: Nanos) -> bool {
        if self.state != OrderState::New {
            return false;
        }
        self.state = OrderState::PendingAck;
        self.sent_at = Some(now);
        true
    }
}

/// Venue constraints configuration.
#[derive(Debug, Clone)]
pub struct VenueConstraints {
    /// Minimum order size.
    pub min_order_size: Size,
    /// Maximum order size.
    pub max_order_size: Size,
    /// Price tick size (e.g., 0.01 for cents).
    pub tick_size: Price,
    /// Minimum price.
    pub min_price: Price,
    /// Maximum price.
    pub max_price: Price,
    /// Maximum open orders per token.
    pub max_open_orders_per_token: usize,
    /// Maximum total open orders.
    pub max_total_open_orders: usize,
    /// Maximum order rate (orders per second).
    pub max_orders_per_second: u32,
    /// Maximum cancel rate (cancels per second).
    pub max_cancels_per_second: u32,
    /// Post-only allowed.
    pub post_only_allowed: bool,
    /// Reduce-only allowed.
    pub reduce_only_allowed: bool,
    /// Order types allowed.
    pub allowed_order_types: Vec<OrderType>,
    /// Time-in-force types allowed.
    pub allowed_tif: Vec<TimeInForce>,
}

impl Default for VenueConstraints {
    fn default() -> Self {
        Self {
            min_order_size: 1.0,
            max_order_size: 1_000_000.0,
            tick_size: 0.01,
            min_price: 0.01,
            max_price: 0.99,
            max_open_orders_per_token: 100,
            max_total_open_orders: 500,
            max_orders_per_second: 10,
            max_cancels_per_second: 20,
            post_only_allowed: true,
            reduce_only_allowed: true,
            allowed_order_types: vec![
                OrderType::Limit,
                OrderType::Market,
                OrderType::Ioc,
                OrderType::Fok,
            ],
            allowed_tif: vec![TimeInForce::Gtc, TimeInForce::Ioc, TimeInForce::Fok],
        }
    }
}

impl VenueConstraints {
    /// Polymarket-like constraints.
    pub fn polymarket() -> Self {
        Self {
            min_order_size: 1.0,
            max_order_size: 100_000.0,
            tick_size: 0.01,
            min_price: 0.01,
            max_price: 0.99,
            max_open_orders_per_token: 50,
            max_total_open_orders: 200,
            max_orders_per_second: 5,
            max_cancels_per_second: 10,
            post_only_allowed: true,
            reduce_only_allowed: false,
            allowed_order_types: vec![OrderType::Limit],
            allowed_tif: vec![TimeInForce::Gtc, TimeInForce::Ioc, TimeInForce::Fok],
        }
    }
}

/// Market status for a token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarketStatus {
    /// Normal trading.
    Open,
    /// Trading halted.
    Halted,
    /// Market is resolving (no new orders).
    Resolving,
    /// Market closed/resolved.
    Closed,
}

/// Rate limiter with sliding window.
#[derive(Debug)]
pub struct RateLimiter {
    /// Window size in nanoseconds.
    window_ns: Nanos,
    /// Maximum events per window.
    max_events: u32,
    /// Timestamps of events in the window.
    events: VecDeque<Nanos>,
    /// Total events processed.
    total_events: u64,
    /// Events dropped due to rate limiting.
    dropped_events: u64,
}

impl RateLimiter {
    pub fn new(max_per_second: u32) -> Self {
        Self {
            window_ns: 1_000_000_000, // 1 second
            max_events: max_per_second,
            events: VecDeque::with_capacity(max_per_second as usize * 2),
            total_events: 0,
            dropped_events: 0,
        }
    }

    /// Try to consume a rate limit slot. Returns true if allowed.
    pub fn try_acquire(&mut self, now: Nanos) -> bool {
        // Remove expired events
        let cutoff = now - self.window_ns;
        while let Some(&front) = self.events.front() {
            if front < cutoff {
                self.events.pop_front();
            } else {
                break;
            }
        }

        self.total_events += 1;

        if self.events.len() >= self.max_events as usize {
            self.dropped_events += 1;
            false
        } else {
            self.events.push_back(now);
            true
        }
    }

    /// Current usage (0.0 to 1.0).
    pub fn usage(&self) -> f64 {
        self.events.len() as f64 / self.max_events as f64
    }

    /// Events dropped due to rate limiting.
    pub fn dropped(&self) -> u64 {
        self.dropped_events
    }

    /// Reset the rate limiter.
    pub fn reset(&mut self) {
        self.events.clear();
        self.total_events = 0;
        self.dropped_events = 0;
    }
}

/// Validation result.
#[derive(Debug, Clone)]
pub enum ValidationResult {
    Valid,
    Invalid { reason: RejectReason },
}

/// Order validation error.
#[derive(Debug, Clone)]
pub struct ValidationError {
    pub reason: RejectReason,
    pub message: String,
}

/// Order Management System.
pub struct OrderManagementSystem {
    /// Venue constraints.
    constraints: VenueConstraints,
    /// All orders by order_id.
    orders: HashMap<OrderId, OmsOrder>,
    /// Client order ID to order ID mapping.
    client_to_order: HashMap<String, OrderId>,
    /// Open orders by token.
    open_orders_by_token: HashMap<String, Vec<OrderId>>,
    /// Market status by token.
    market_status: HashMap<String, MarketStatus>,
    /// Order rate limiter.
    order_rate_limiter: RateLimiter,
    /// Cancel rate limiter.
    cancel_rate_limiter: RateLimiter,
    /// Next order ID.
    next_order_id: OrderId,
    /// Next cancel request ID.
    next_cancel_id: u64,
    /// Statistics.
    pub stats: OmsStats,
    /// Out-of-order message buffer.
    pending_messages: HashMap<OrderId, Vec<PendingMessage>>,
}

/// Pending message for out-of-order handling.
#[derive(Debug, Clone)]
enum PendingMessage {
    Fill {
        qty: Size,
        price: Price,
        fee: f64,
        time: Nanos,
    },
    CancelAck {
        cancelled_qty: Size,
        time: Nanos,
    },
}

/// OMS statistics.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OmsStats {
    pub orders_created: u64,
    pub orders_sent: u64,
    pub orders_acked: u64,
    pub orders_rejected: u64,
    pub orders_filled: u64,
    pub orders_partially_filled: u64,
    pub orders_cancelled: u64,
    pub cancels_rejected: u64,
    pub rate_limited_orders: u64,
    pub rate_limited_cancels: u64,
    pub validation_failures: u64,
    pub out_of_order_messages: u64,
    pub total_volume: f64,
    pub total_fees: f64,
}

impl OrderManagementSystem {
    pub fn new(constraints: VenueConstraints) -> Self {
        let order_rate = constraints.max_orders_per_second;
        let cancel_rate = constraints.max_cancels_per_second;

        Self {
            constraints,
            orders: HashMap::new(),
            client_to_order: HashMap::new(),
            open_orders_by_token: HashMap::new(),
            market_status: HashMap::new(),
            order_rate_limiter: RateLimiter::new(order_rate),
            cancel_rate_limiter: RateLimiter::new(cancel_rate),
            next_order_id: 1,
            next_cancel_id: 1,
            stats: OmsStats::default(),
            pending_messages: HashMap::new(),
        }
    }

    /// Create a new order (pre-validation).
    pub fn create_order(
        &mut self,
        client_order_id: String,
        token_id: String,
        side: Side,
        order_type: OrderType,
        time_in_force: TimeInForce,
        price: Price,
        qty: Size,
        post_only: bool,
        reduce_only: bool,
        now: Nanos,
    ) -> Result<OrderId, ValidationError> {
        // Validate before creating
        self.validate_order(
            &token_id,
            side,
            order_type,
            time_in_force,
            price,
            qty,
            post_only,
            reduce_only,
            now,
        )?;

        // Check for duplicate client order ID
        if self.client_to_order.contains_key(&client_order_id) {
            return Err(ValidationError {
                reason: RejectReason::DuplicateOrderId,
                message: "Duplicate client order ID".into(),
            });
        }

        let order_id = self.next_order_id;
        self.next_order_id += 1;

        let order = OmsOrder::new(
            order_id,
            client_order_id.clone(),
            token_id,
            side,
            order_type,
            time_in_force,
            price,
            qty,
            post_only,
            reduce_only,
            now,
        );

        self.client_to_order.insert(client_order_id, order_id);
        self.orders.insert(order_id, order);
        self.stats.orders_created += 1;

        Ok(order_id)
    }

    /// Send an order to the venue (rate limited).
    pub fn send_order(&mut self, order_id: OrderId, now: Nanos) -> Result<(), ValidationError> {
        // Rate limit check
        if !self.order_rate_limiter.try_acquire(now) {
            self.stats.rate_limited_orders += 1;
            return Err(ValidationError {
                reason: RejectReason::RateLimited,
                message: "Order rate limit exceeded".into(),
            });
        }

        let order = self.orders.get_mut(&order_id).ok_or(ValidationError {
            reason: RejectReason::Unknown("Order not found".into()),
            message: "Order not found".into(),
        })?;

        if !order.mark_sent(now) {
            return Err(ValidationError {
                reason: RejectReason::Unknown("Invalid order state for send".into()),
                message: format!("Cannot send order in state {:?}", order.state),
            });
        }

        // Track open order
        self.open_orders_by_token
            .entry(order.token_id.clone())
            .or_insert_with(Vec::new)
            .push(order_id);

        self.stats.orders_sent += 1;
        Ok(())
    }

    /// Handle order acknowledgment from venue.
    pub fn on_order_ack(&mut self, order_id: OrderId, now: Nanos) -> bool {
        if let Some(order) = self.orders.get_mut(&order_id) {
            if order.ack(now) {
                self.stats.orders_acked += 1;

                // Process any pending messages
                self.process_pending_messages(order_id, now);
                return true;
            }
        }
        false
    }

    /// Handle order rejection from venue.
    pub fn on_order_reject(&mut self, order_id: OrderId, reason: String, now: Nanos) -> bool {
        if let Some(order) = self.orders.get_mut(&order_id) {
            let token_id = order.token_id.clone();
            if order.reject(reason, now) {
                self.stats.orders_rejected += 1;
                self.remove_from_open_orders(&token_id, order_id);
                return true;
            }
        }
        false
    }

    /// Handle fill from venue.
    pub fn on_fill(
        &mut self,
        order_id: OrderId,
        fill_qty: Size,
        fill_price: Price,
        fee: f64,
        now: Nanos,
    ) -> bool {
        // Check if order exists
        let Some(order) = self.orders.get_mut(&order_id) else {
            // Store for out-of-order processing
            self.stats.out_of_order_messages += 1;
            self.pending_messages
                .entry(order_id)
                .or_insert_with(Vec::new)
                .push(PendingMessage::Fill {
                    qty: fill_qty,
                    price: fill_price,
                    fee,
                    time: now,
                });
            return false;
        };

        // Check if order is in a state to receive fills
        if order.state == OrderState::PendingAck {
            // Store for later
            self.stats.out_of_order_messages += 1;
            self.pending_messages
                .entry(order_id)
                .or_insert_with(Vec::new)
                .push(PendingMessage::Fill {
                    qty: fill_qty,
                    price: fill_price,
                    fee,
                    time: now,
                });
            return false;
        }

        let token_id = order.token_id.clone();
        let was_partial = order.filled_qty > 0.0;

        if order.apply_fill(fill_qty, fill_price, fee, now) {
            self.stats.total_volume += fill_qty * fill_price;
            self.stats.total_fees += fee;

            if order.state == OrderState::Done {
                self.stats.orders_filled += 1;
                self.remove_from_open_orders(&token_id, order_id);
            } else if !was_partial {
                self.stats.orders_partially_filled += 1;
            }
            return true;
        }
        false
    }

    /// Request to cancel an order.
    pub fn request_cancel(
        &mut self,
        order_id: OrderId,
        now: Nanos,
    ) -> Result<u64, ValidationError> {
        // Rate limit check
        if !self.cancel_rate_limiter.try_acquire(now) {
            self.stats.rate_limited_cancels += 1;
            return Err(ValidationError {
                reason: RejectReason::RateLimited,
                message: "Cancel rate limit exceeded".into(),
            });
        }

        let order = self.orders.get_mut(&order_id).ok_or(ValidationError {
            reason: RejectReason::Unknown("Order not found".into()),
            message: "Order not found".into(),
        })?;

        let cancel_id = self.next_cancel_id;
        self.next_cancel_id += 1;

        if !order.request_cancel(cancel_id, now) {
            return Err(ValidationError {
                reason: RejectReason::Unknown("Cannot cancel order".into()),
                message: format!("Cannot cancel order in state {:?}", order.state),
            });
        }

        Ok(cancel_id)
    }

    /// Handle cancel acknowledgment from venue.
    pub fn on_cancel_ack(&mut self, order_id: OrderId, cancelled_qty: Size, now: Nanos) -> bool {
        // Check if order exists
        let Some(order) = self.orders.get_mut(&order_id) else {
            self.stats.out_of_order_messages += 1;
            self.pending_messages
                .entry(order_id)
                .or_insert_with(Vec::new)
                .push(PendingMessage::CancelAck {
                    cancelled_qty,
                    time: now,
                });
            return false;
        };

        let token_id = order.token_id.clone();

        if order.cancel_ack(cancelled_qty, now) {
            self.stats.orders_cancelled += 1;
            self.remove_from_open_orders(&token_id, order_id);
            return true;
        }
        false
    }

    /// Handle cancel rejection from venue.
    pub fn on_cancel_reject(&mut self, order_id: OrderId, now: Nanos) -> bool {
        if let Some(order) = self.orders.get_mut(&order_id) {
            if order.cancel_reject(now) {
                self.stats.cancels_rejected += 1;
                return true;
            }
        }
        false
    }

    /// Set market status for a token.
    pub fn set_market_status(&mut self, token_id: &str, status: MarketStatus) {
        self.market_status.insert(token_id.to_string(), status);

        // If halted or resolving, mark all open orders
        if status == MarketStatus::Halted
            || status == MarketStatus::Resolving
            || status == MarketStatus::Closed
        {
            if let Some(order_ids) = self.open_orders_by_token.get(token_id).cloned() {
                for order_id in order_ids {
                    if let Some(order) = self.orders.get_mut(&order_id) {
                        if !order.state.is_terminal() {
                            order.state = OrderState::Done;
                            order.terminal_reason = Some(match status {
                                MarketStatus::Halted => TerminalReason::MarketHalted,
                                MarketStatus::Resolving | MarketStatus::Closed => {
                                    TerminalReason::MarketResolved
                                }
                                _ => TerminalReason::Cancelled,
                            });
                        }
                    }
                }
            }
            self.open_orders_by_token.remove(token_id);
        }
    }

    /// Get market status for a token.
    pub fn get_market_status(&self, token_id: &str) -> MarketStatus {
        self.market_status
            .get(token_id)
            .copied()
            .unwrap_or(MarketStatus::Open)
    }

    /// Get an order by ID.
    pub fn get_order(&self, order_id: OrderId) -> Option<&OmsOrder> {
        self.orders.get(&order_id)
    }

    /// Get an order by client order ID.
    pub fn get_order_by_client_id(&self, client_order_id: &str) -> Option<&OmsOrder> {
        self.client_to_order
            .get(client_order_id)
            .and_then(|id| self.orders.get(id))
    }

    /// Get all open orders for a token.
    pub fn get_open_orders(&self, token_id: &str) -> Vec<&OmsOrder> {
        self.open_orders_by_token
            .get(token_id)
            .map(|ids| {
                ids.iter()
                    .filter_map(|id| self.orders.get(id))
                    .filter(|o| o.state.is_active())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Get all orders.
    pub fn get_all_orders(&self) -> impl Iterator<Item = &OmsOrder> {
        self.orders.values()
    }

    /// Get total open order count.
    pub fn open_order_count(&self) -> usize {
        self.orders.values().filter(|o| o.state.is_active()).count()
    }

    /// Get open order count for a token.
    pub fn open_order_count_for_token(&self, token_id: &str) -> usize {
        self.open_orders_by_token
            .get(token_id)
            .map(|ids| {
                ids.iter()
                    .filter(|id| {
                        self.orders
                            .get(id)
                            .map(|o| o.state.is_active())
                            .unwrap_or(false)
                    })
                    .count()
            })
            .unwrap_or(0)
    }

    /// Cancel all open orders for a token.
    pub fn cancel_all(&mut self, token_id: &str, now: Nanos) -> Vec<OrderId> {
        let order_ids: Vec<OrderId> = self
            .open_orders_by_token
            .get(token_id)
            .cloned()
            .unwrap_or_default();

        let mut cancelled = Vec::new();
        for order_id in order_ids {
            if self.request_cancel(order_id, now).is_ok() {
                cancelled.push(order_id);
            }
        }
        cancelled
    }

    /// Reset the OMS.
    pub fn reset(&mut self) {
        self.orders.clear();
        self.client_to_order.clear();
        self.open_orders_by_token.clear();
        self.market_status.clear();
        self.order_rate_limiter.reset();
        self.cancel_rate_limiter.reset();
        self.next_order_id = 1;
        self.next_cancel_id = 1;
        self.stats = OmsStats::default();
        self.pending_messages.clear();
    }

    // === Private methods ===

    fn validate_order(
        &mut self,
        token_id: &str,
        _side: Side,
        order_type: OrderType,
        time_in_force: TimeInForce,
        price: Price,
        qty: Size,
        post_only: bool,
        reduce_only: bool,
        _now: Nanos,
    ) -> Result<(), ValidationError> {
        // Market status check
        let status = self.get_market_status(token_id);
        if status != MarketStatus::Open {
            self.stats.validation_failures += 1;
            return Err(ValidationError {
                reason: RejectReason::MarketClosed,
                message: format!("Market is {:?}", status),
            });
        }

        // Size validation
        if qty < self.constraints.min_order_size {
            self.stats.validation_failures += 1;
            return Err(ValidationError {
                reason: RejectReason::InvalidSize,
                message: format!(
                    "Size {} below minimum {}",
                    qty, self.constraints.min_order_size
                ),
            });
        }
        if qty > self.constraints.max_order_size {
            self.stats.validation_failures += 1;
            return Err(ValidationError {
                reason: RejectReason::InvalidSize,
                message: format!(
                    "Size {} above maximum {}",
                    qty, self.constraints.max_order_size
                ),
            });
        }

        // Price validation
        if price < self.constraints.min_price || price > self.constraints.max_price {
            self.stats.validation_failures += 1;
            return Err(ValidationError {
                reason: RejectReason::InvalidPrice,
                message: format!(
                    "Price {} outside range [{}, {}]",
                    price, self.constraints.min_price, self.constraints.max_price
                ),
            });
        }

        // Tick size validation
        let ticks = (price / self.constraints.tick_size).round();
        let rounded_price = ticks * self.constraints.tick_size;
        if (price - rounded_price).abs() > 1e-9 {
            self.stats.validation_failures += 1;
            return Err(ValidationError {
                reason: RejectReason::InvalidPrice,
                message: format!(
                    "Price {} not on tick size {}",
                    price, self.constraints.tick_size
                ),
            });
        }

        // Order type validation
        if !self.constraints.allowed_order_types.contains(&order_type) {
            self.stats.validation_failures += 1;
            return Err(ValidationError {
                reason: RejectReason::Unknown(format!("Order type {:?} not allowed", order_type)),
                message: format!("Order type {:?} not allowed", order_type),
            });
        }

        // TIF validation
        if !self.constraints.allowed_tif.contains(&time_in_force) {
            self.stats.validation_failures += 1;
            return Err(ValidationError {
                reason: RejectReason::Unknown(format!("TIF {:?} not allowed", time_in_force)),
                message: format!("Time in force {:?} not allowed", time_in_force),
            });
        }

        // Post-only validation
        if post_only && !self.constraints.post_only_allowed {
            self.stats.validation_failures += 1;
            return Err(ValidationError {
                reason: RejectReason::Unknown("Post-only not allowed".into()),
                message: "Post-only orders not allowed".into(),
            });
        }

        // Reduce-only validation
        if reduce_only && !self.constraints.reduce_only_allowed {
            self.stats.validation_failures += 1;
            return Err(ValidationError {
                reason: RejectReason::Unknown("Reduce-only not allowed".into()),
                message: "Reduce-only orders not allowed".into(),
            });
        }

        // Open orders limit per token
        let token_open = self.open_order_count_for_token(token_id);
        if token_open >= self.constraints.max_open_orders_per_token {
            self.stats.validation_failures += 1;
            return Err(ValidationError {
                reason: RejectReason::Unknown("Too many open orders for token".into()),
                message: format!(
                    "Max {} open orders per token exceeded",
                    self.constraints.max_open_orders_per_token
                ),
            });
        }

        // Total open orders limit
        let total_open = self.open_order_count();
        if total_open >= self.constraints.max_total_open_orders {
            self.stats.validation_failures += 1;
            return Err(ValidationError {
                reason: RejectReason::Unknown("Too many total open orders".into()),
                message: format!(
                    "Max {} total open orders exceeded",
                    self.constraints.max_total_open_orders
                ),
            });
        }

        Ok(())
    }

    fn remove_from_open_orders(&mut self, token_id: &str, order_id: OrderId) {
        if let Some(orders) = self.open_orders_by_token.get_mut(token_id) {
            orders.retain(|&id| id != order_id);
        }
    }

    fn process_pending_messages(&mut self, order_id: OrderId, now: Nanos) {
        if let Some(messages) = self.pending_messages.remove(&order_id) {
            for msg in messages {
                match msg {
                    PendingMessage::Fill {
                        qty,
                        price,
                        fee,
                        time,
                    } => {
                        self.on_fill(order_id, qty, price, fee, time);
                    }
                    PendingMessage::CancelAck {
                        cancelled_qty,
                        time,
                    } => {
                        self.on_cancel_ack(order_id, cancelled_qty, time);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_oms() -> OrderManagementSystem {
        OrderManagementSystem::new(VenueConstraints::default())
    }

    #[test]
    fn test_order_lifecycle() {
        let mut oms = create_oms();
        let now = 1_000_000_000i64;

        // Create order
        let order_id = oms
            .create_order(
                "order1".into(),
                "token123".into(),
                Side::Buy,
                OrderType::Limit,
                TimeInForce::Gtc,
                0.50,
                100.0,
                false,
                false,
                now,
            )
            .unwrap();

        assert_eq!(oms.get_order(order_id).unwrap().state, OrderState::New);

        // Send order
        oms.send_order(order_id, now + 1000).unwrap();
        assert_eq!(
            oms.get_order(order_id).unwrap().state,
            OrderState::PendingAck
        );

        // Ack order
        oms.on_order_ack(order_id, now + 2000);
        assert_eq!(oms.get_order(order_id).unwrap().state, OrderState::Live);

        // Partial fill
        oms.on_fill(order_id, 50.0, 0.50, 0.025, now + 3000);
        assert_eq!(
            oms.get_order(order_id).unwrap().state,
            OrderState::PartiallyFilled
        );
        assert_eq!(oms.get_order(order_id).unwrap().filled_qty, 50.0);

        // Complete fill
        oms.on_fill(order_id, 50.0, 0.50, 0.025, now + 4000);
        assert_eq!(oms.get_order(order_id).unwrap().state, OrderState::Done);
        assert!(matches!(
            oms.get_order(order_id).unwrap().terminal_reason,
            Some(TerminalReason::Filled)
        ));
    }

    #[test]
    fn test_order_cancel() {
        let mut oms = create_oms();
        let now = 1_000_000_000i64;

        let order_id = oms
            .create_order(
                "order1".into(),
                "token123".into(),
                Side::Buy,
                OrderType::Limit,
                TimeInForce::Gtc,
                0.50,
                100.0,
                false,
                false,
                now,
            )
            .unwrap();

        oms.send_order(order_id, now + 1000).unwrap();
        oms.on_order_ack(order_id, now + 2000);

        // Request cancel
        let cancel_id = oms.request_cancel(order_id, now + 3000).unwrap();
        assert_eq!(
            oms.get_order(order_id).unwrap().state,
            OrderState::PendingCancel
        );

        // Cancel ack
        oms.on_cancel_ack(order_id, 100.0, now + 4000);
        assert_eq!(oms.get_order(order_id).unwrap().state, OrderState::Done);
        assert!(matches!(
            oms.get_order(order_id).unwrap().terminal_reason,
            Some(TerminalReason::Cancelled)
        ));
    }

    #[test]
    fn test_order_rejection() {
        let mut oms = create_oms();
        let now = 1_000_000_000i64;

        let order_id = oms
            .create_order(
                "order1".into(),
                "token123".into(),
                Side::Buy,
                OrderType::Limit,
                TimeInForce::Gtc,
                0.50,
                100.0,
                false,
                false,
                now,
            )
            .unwrap();

        oms.send_order(order_id, now + 1000).unwrap();
        oms.on_order_reject(order_id, "Insufficient funds".into(), now + 2000);

        assert_eq!(oms.get_order(order_id).unwrap().state, OrderState::Done);
        assert!(matches!(
            oms.get_order(order_id).unwrap().terminal_reason,
            Some(TerminalReason::Rejected { .. })
        ));
    }

    #[test]
    fn test_validation_size() {
        let mut oms = create_oms();
        let now = 1_000_000_000i64;

        // Too small
        let result = oms.create_order(
            "order1".into(),
            "token123".into(),
            Side::Buy,
            OrderType::Limit,
            TimeInForce::Gtc,
            0.50,
            0.1, // Below minimum
            false,
            false,
            now,
        );
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err().reason,
            RejectReason::InvalidSize
        ));
    }

    #[test]
    fn test_validation_price() {
        let mut oms = create_oms();
        let now = 1_000_000_000i64;

        // Price outside range
        let result = oms.create_order(
            "order1".into(),
            "token123".into(),
            Side::Buy,
            OrderType::Limit,
            TimeInForce::Gtc,
            1.50, // Above max
            100.0,
            false,
            false,
            now,
        );
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err().reason,
            RejectReason::InvalidPrice
        ));
    }

    #[test]
    fn test_rate_limiting() {
        let mut oms = OrderManagementSystem::new(VenueConstraints {
            max_orders_per_second: 2,
            ..Default::default()
        });
        let now = 1_000_000_000i64;

        // First two should succeed
        for i in 0..2 {
            let order_id = oms
                .create_order(
                    format!("order{}", i),
                    "token123".into(),
                    Side::Buy,
                    OrderType::Limit,
                    TimeInForce::Gtc,
                    0.50,
                    100.0,
                    false,
                    false,
                    now,
                )
                .unwrap();
            oms.send_order(order_id, now).unwrap();
        }

        // Third should be rate limited
        let order_id = oms
            .create_order(
                "order2".into(),
                "token123".into(),
                Side::Buy,
                OrderType::Limit,
                TimeInForce::Gtc,
                0.50,
                100.0,
                false,
                false,
                now,
            )
            .unwrap();
        let result = oms.send_order(order_id, now);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err().reason,
            RejectReason::RateLimited
        ));
    }

    #[test]
    fn test_market_halt() {
        let mut oms = create_oms();
        let now = 1_000_000_000i64;

        // Create and send order
        let order_id = oms
            .create_order(
                "order1".into(),
                "token123".into(),
                Side::Buy,
                OrderType::Limit,
                TimeInForce::Gtc,
                0.50,
                100.0,
                false,
                false,
                now,
            )
            .unwrap();
        oms.send_order(order_id, now).unwrap();
        oms.on_order_ack(order_id, now + 1000);

        // Halt market
        oms.set_market_status("token123", MarketStatus::Halted);

        // Order should be terminated
        assert_eq!(oms.get_order(order_id).unwrap().state, OrderState::Done);
        assert!(matches!(
            oms.get_order(order_id).unwrap().terminal_reason,
            Some(TerminalReason::MarketHalted)
        ));

        // Can't create new orders
        let result = oms.create_order(
            "order2".into(),
            "token123".into(),
            Side::Buy,
            OrderType::Limit,
            TimeInForce::Gtc,
            0.50,
            100.0,
            false,
            false,
            now + 2000,
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_out_of_order_fill() {
        let mut oms = create_oms();
        let now = 1_000_000_000i64;

        let order_id = oms
            .create_order(
                "order1".into(),
                "token123".into(),
                Side::Buy,
                OrderType::Limit,
                TimeInForce::Gtc,
                0.50,
                100.0,
                false,
                false,
                now,
            )
            .unwrap();
        oms.send_order(order_id, now).unwrap();

        // Fill arrives before ack (out of order)
        let filled = oms.on_fill(order_id, 50.0, 0.50, 0.025, now + 1000);
        assert!(!filled); // Should not process yet

        // Now ack arrives
        oms.on_order_ack(order_id, now + 2000);

        // Fill should have been applied
        assert_eq!(oms.get_order(order_id).unwrap().filled_qty, 50.0);
    }

    #[test]
    fn test_duplicate_client_order_id() {
        let mut oms = create_oms();
        let now = 1_000_000_000i64;

        // First order
        oms.create_order(
            "order1".into(),
            "token123".into(),
            Side::Buy,
            OrderType::Limit,
            TimeInForce::Gtc,
            0.50,
            100.0,
            false,
            false,
            now,
        )
        .unwrap();

        // Duplicate
        let result = oms.create_order(
            "order1".into(),
            "token123".into(),
            Side::Buy,
            OrderType::Limit,
            TimeInForce::Gtc,
            0.50,
            100.0,
            false,
            false,
            now,
        );
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err().reason,
            RejectReason::DuplicateOrderId
        ));
    }
}

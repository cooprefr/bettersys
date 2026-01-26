//! Limit Order Book Simulator
//!
//! Full CLOB simulator for binary outcome tokens with realistic exchange behavior.
//! Supports FIFO matching, partial fills, self-trade prevention, and maker/taker fees.

use crate::backtest_v2::clock::Nanos;
use crate::backtest_v2::events::{
    Event, OrderId, OrderType, Price, RejectReason, Side, Size, TimeInForce, TimestampedEvent,
};
use crate::backtest_v2::queue::StreamSource;
use std::collections::{BTreeMap, HashMap, VecDeque};

/// Price tick size for binary outcome markets.
/// Polymarket uses 0.01 (1 cent) ticks in the 0-1 range.
pub const DEFAULT_TICK_SIZE: f64 = 0.01;
pub const MIN_PRICE: f64 = 0.01;
pub const MAX_PRICE: f64 = 0.99;

/// Discrete price level (integer ticks for deterministic matching).
pub type PriceTicks = u32;

/// Convert price to ticks.
#[inline]
pub fn price_to_ticks(price: f64, tick_size: f64) -> PriceTicks {
    ((price / tick_size).round() as u32).clamp(1, 99)
}

/// Convert ticks to price.
#[inline]
pub fn ticks_to_price(ticks: PriceTicks, tick_size: f64) -> f64 {
    (ticks as f64 * tick_size).clamp(MIN_PRICE, MAX_PRICE)
}

/// Fee configuration for the matching engine.
#[derive(Debug, Clone)]
pub struct FeeConfig {
    /// Maker fee (negative = rebate). Typically -0.0001 to 0.001.
    pub maker_fee_rate: f64,
    /// Taker fee. Typically 0.001 to 0.003.
    pub taker_fee_rate: f64,
}

impl Default for FeeConfig {
    fn default() -> Self {
        Self {
            maker_fee_rate: 0.0,   // Polymarket: no maker fee
            taker_fee_rate: 0.001, // 10 bps taker fee
        }
    }
}

/// Matching engine configuration.
#[derive(Debug, Clone)]
pub struct MatchingConfig {
    pub tick_size: f64,
    pub fees: FeeConfig,
    /// Enable self-trade prevention.
    pub self_trade_prevention: bool,
    /// Self-trade prevention mode.
    pub stp_mode: SelfTradeMode,
    /// Minimum order size.
    pub min_order_size: f64,
    /// Maximum order size.
    pub max_order_size: f64,
    /// Simulated latency for order acknowledgment (nanos).
    pub ack_latency_ns: Nanos,
}

impl Default for MatchingConfig {
    fn default() -> Self {
        Self {
            tick_size: DEFAULT_TICK_SIZE,
            fees: FeeConfig::default(),
            self_trade_prevention: true,
            stp_mode: SelfTradeMode::CancelNewest,
            min_order_size: 1.0,
            max_order_size: 1_000_000.0,
            ack_latency_ns: 1_000_000, // 1ms default
        }
    }
}

/// Self-trade prevention modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelfTradeMode {
    /// Cancel the incoming (newest) order.
    CancelNewest,
    /// Cancel the resting (oldest) order.
    CancelOldest,
    /// Cancel both orders.
    CancelBoth,
    /// Decrement and cancel (reduce qty, cancel if zero).
    DecrementAndCancel,
}

/// Order submission request.
#[derive(Debug, Clone)]
pub struct OrderRequest {
    pub client_order_id: String,
    pub token_id: String,
    pub side: Side,
    pub price: Price,
    pub size: Size,
    pub order_type: OrderType,
    pub time_in_force: TimeInForce,
    /// Trader ID for self-trade prevention.
    pub trader_id: String,
    /// Post-only flag (reject if would cross).
    pub post_only: bool,
    /// Reduce-only flag (can only reduce position).
    pub reduce_only: bool,
}

/// Cancel request.
#[derive(Debug, Clone)]
pub struct CancelRequest {
    pub order_id: OrderId,
    pub client_order_id: Option<String>,
}

/// Internal order representation on the book.
#[derive(Debug, Clone)]
struct BookOrder {
    order_id: OrderId,
    client_order_id: String,
    trader_id: String,
    side: Side,
    price_ticks: PriceTicks,
    original_size: Size,
    remaining_size: Size,
    #[allow(dead_code)]
    post_only: bool,
    #[allow(dead_code)]
    created_at: Nanos,
    #[allow(dead_code)]
    time_in_force: TimeInForce,
}

/// A single price level with FIFO queue.
#[derive(Debug, Clone, Default)]
struct PriceLevel {
    orders: VecDeque<BookOrder>,
    total_size: Size,
}

impl PriceLevel {
    fn add_order(&mut self, order: BookOrder) {
        self.total_size += order.remaining_size;
        self.orders.push_back(order);
    }

    fn remove_front(&mut self) -> Option<BookOrder> {
        if let Some(order) = self.orders.pop_front() {
            self.total_size -= order.remaining_size;
            Some(order)
        } else {
            None
        }
    }

    fn reduce_front(&mut self, fill_size: Size) -> Option<Size> {
        if let Some(front) = self.orders.front_mut() {
            let actual_fill = fill_size.min(front.remaining_size);
            front.remaining_size -= actual_fill;
            self.total_size -= actual_fill;

            if front.remaining_size <= 0.0 {
                self.orders.pop_front();
            }
            Some(actual_fill)
        } else {
            None
        }
    }

    fn is_empty(&self) -> bool {
        self.orders.is_empty()
    }

    fn front(&self) -> Option<&BookOrder> {
        self.orders.front()
    }

    fn remove_order(&mut self, order_id: OrderId) -> Option<BookOrder> {
        if let Some(pos) = self.orders.iter().position(|o| o.order_id == order_id) {
            let order = self.orders.remove(pos)?;
            self.total_size -= order.remaining_size;
            Some(order)
        } else {
            None
        }
    }
}

/// The limit order book for a single token.
pub struct LimitOrderBook {
    pub token_id: String,
    config: MatchingConfig,
    /// Bids: sorted by price descending (best bid = highest)
    bids: BTreeMap<PriceTicks, PriceLevel>,
    /// Asks: sorted by price ascending (best ask = lowest)
    asks: BTreeMap<PriceTicks, PriceLevel>,
    /// Order lookup by order_id
    orders: HashMap<OrderId, OrderLocation>,
    /// Next order ID
    next_order_id: OrderId,
    /// Next fill ID
    next_fill_id: u64,
    /// Statistics
    pub stats: MatchingStats,
}

/// Location of an order in the book.
#[derive(Debug, Clone)]
struct OrderLocation {
    side: Side,
    price_ticks: PriceTicks,
}

/// Matching statistics.
#[derive(Debug, Clone, Default)]
pub struct MatchingStats {
    pub orders_submitted: u64,
    pub orders_accepted: u64,
    pub orders_rejected: u64,
    pub orders_cancelled: u64,
    pub fills: u64,
    pub total_volume: f64,
    pub self_trades_prevented: u64,
    pub post_only_rejections: u64,
}

/// Fill instruction generated during matching.
#[derive(Debug)]
struct FillInstruction {
    taker_order_id: OrderId,
    maker_order_id: OrderId,
    price_ticks: PriceTicks,
    fill_size: Size,
    taker_leaves: Size,
    maker_leaves: Size,
}

/// Action to take during matching.
#[derive(Debug)]
enum MatchAction {
    Fill(FillInstruction),
    CancelIncoming {
        order_id: OrderId,
        qty: Size,
    },
    CancelResting {
        order_id: OrderId,
        qty: Size,
    },
    CancelBoth {
        incoming_id: OrderId,
        incoming_qty: Size,
        resting_id: OrderId,
        resting_qty: Size,
    },
}

impl LimitOrderBook {
    pub fn new(token_id: impl Into<String>, config: MatchingConfig) -> Self {
        Self {
            token_id: token_id.into(),
            config,
            bids: BTreeMap::new(),
            asks: BTreeMap::new(),
            orders: HashMap::new(),
            next_order_id: 1,
            next_fill_id: 1,
            stats: MatchingStats::default(),
        }
    }

    /// Submit a new order. Returns events to emit.
    pub fn submit_order(&mut self, req: OrderRequest, now: Nanos) -> Vec<TimestampedEvent> {
        self.stats.orders_submitted += 1;
        let mut events = Vec::new();

        // Validate order
        if let Some(reject_reason) = self.validate_order(&req) {
            self.stats.orders_rejected += 1;
            events.push(self.make_reject_event(0, Some(req.client_order_id), reject_reason, now));
            return events;
        }

        let order_id = self.next_order_id;
        self.next_order_id += 1;

        let price_ticks = price_to_ticks(req.price, self.config.tick_size);
        let ack_time = now + self.config.ack_latency_ns;

        // Check post-only
        if req.post_only && self.would_cross(req.side, price_ticks) {
            self.stats.orders_rejected += 1;
            self.stats.post_only_rejections += 1;
            events.push(self.make_reject_event(
                order_id,
                Some(req.client_order_id),
                RejectReason::Unknown("Post-only order would cross".into()),
                now,
            ));
            return events;
        }

        // Create internal order
        let mut order = BookOrder {
            order_id,
            client_order_id: req.client_order_id.clone(),
            trader_id: req.trader_id.clone(),
            side: req.side,
            price_ticks,
            original_size: req.size,
            remaining_size: req.size,
            post_only: req.post_only,
            created_at: now,
            time_in_force: req.time_in_force,
        };

        // Emit ack
        self.stats.orders_accepted += 1;
        events.push(TimestampedEvent::new(
            ack_time,
            StreamSource::OrderManagement as u8,
            Event::OrderAck {
                order_id,
                client_order_id: Some(req.client_order_id.clone()),
                exchange_time: ack_time,
            },
        ));

        // Attempt matching
        let fill_events = self.match_order(&mut order, now);
        events.extend(fill_events);

        // Handle remaining quantity
        if order.remaining_size > 0.0 {
            match req.time_in_force {
                TimeInForce::Gtc | TimeInForce::Gtt { .. } => {
                    self.add_to_book(order);
                }
                TimeInForce::Ioc | TimeInForce::Fok => {
                    events.push(self.make_cancel_ack_event(order_id, order.remaining_size, now));
                }
            }
        }

        events
    }

    /// Cancel an order.
    pub fn cancel_order(&mut self, req: CancelRequest, now: Nanos) -> Vec<TimestampedEvent> {
        let mut events = Vec::new();

        let Some(location) = self.orders.remove(&req.order_id) else {
            events.push(self.make_reject_event(
                req.order_id,
                req.client_order_id,
                RejectReason::Unknown("Order not found".into()),
                now,
            ));
            return events;
        };

        let book = match location.side {
            Side::Buy => &mut self.bids,
            Side::Sell => &mut self.asks,
        };

        let mut cancelled_qty = 0.0;
        let mut remove_level = false;

        if let Some(level) = book.get_mut(&location.price_ticks) {
            if let Some(order) = level.remove_order(req.order_id) {
                cancelled_qty = order.remaining_size;
                remove_level = level.is_empty();
            }
        }

        if remove_level {
            book.remove(&location.price_ticks);
        }

        self.stats.orders_cancelled += 1;
        events.push(self.make_cancel_ack_event(req.order_id, cancelled_qty, now));

        events
    }

    /// Get best bid price.
    pub fn best_bid(&self) -> Option<(Price, Size)> {
        self.bids.last_key_value().map(|(&ticks, level)| {
            (
                ticks_to_price(ticks, self.config.tick_size),
                level.total_size,
            )
        })
    }

    /// Get best ask price.
    pub fn best_ask(&self) -> Option<(Price, Size)> {
        self.asks.first_key_value().map(|(&ticks, level)| {
            (
                ticks_to_price(ticks, self.config.tick_size),
                level.total_size,
            )
        })
    }

    /// Get mid price.
    pub fn mid_price(&self) -> Option<Price> {
        match (self.best_bid(), self.best_ask()) {
            (Some((bid, _)), Some((ask, _))) => Some((bid + ask) / 2.0),
            _ => None,
        }
    }

    /// Get spread.
    pub fn spread(&self) -> Option<Price> {
        match (self.best_bid(), self.best_ask()) {
            (Some((bid, _)), Some((ask, _))) => Some(ask - bid),
            _ => None,
        }
    }

    /// Check if book is crossed.
    pub fn is_crossed(&self) -> bool {
        match (self.bids.last_key_value(), self.asks.first_key_value()) {
            (Some((&bid_ticks, _)), Some((&ask_ticks, _))) => bid_ticks >= ask_ticks,
            _ => false,
        }
    }

    /// Get top of book levels.
    pub fn top_of_book(&self, depth: usize) -> (Vec<(Price, Size)>, Vec<(Price, Size)>) {
        let bids: Vec<_> = self
            .bids
            .iter()
            .rev()
            .take(depth)
            .map(|(&ticks, level)| {
                (
                    ticks_to_price(ticks, self.config.tick_size),
                    level.total_size,
                )
            })
            .collect();

        let asks: Vec<_> = self
            .asks
            .iter()
            .take(depth)
            .map(|(&ticks, level)| {
                (
                    ticks_to_price(ticks, self.config.tick_size),
                    level.total_size,
                )
            })
            .collect();

        (bids, asks)
    }

    /// Number of orders on the book.
    pub fn order_count(&self) -> usize {
        self.orders.len()
    }

    // === Private methods ===

    fn validate_order(&self, req: &OrderRequest) -> Option<RejectReason> {
        let price_ticks = price_to_ticks(req.price, self.config.tick_size);
        if price_ticks < 1 || price_ticks > 99 {
            return Some(RejectReason::InvalidPrice);
        }

        if req.size < self.config.min_order_size {
            return Some(RejectReason::InvalidSize);
        }
        if req.size > self.config.max_order_size {
            return Some(RejectReason::InvalidSize);
        }

        if matches!(req.time_in_force, TimeInForce::Fok) {
            let available = self.available_liquidity(req.side, price_ticks);
            if available < req.size {
                return Some(RejectReason::Unknown("FOK cannot be fully filled".into()));
            }
        }

        None
    }

    fn would_cross(&self, side: Side, price_ticks: PriceTicks) -> bool {
        match side {
            Side::Buy => self
                .asks
                .first_key_value()
                .map(|(&ask, _)| price_ticks >= ask)
                .unwrap_or(false),
            Side::Sell => self
                .bids
                .last_key_value()
                .map(|(&bid, _)| price_ticks <= bid)
                .unwrap_or(false),
        }
    }

    fn available_liquidity(&self, side: Side, limit_ticks: PriceTicks) -> Size {
        match side {
            Side::Buy => self
                .asks
                .iter()
                .take_while(|(&ticks, _)| ticks <= limit_ticks)
                .map(|(_, level)| level.total_size)
                .sum(),
            Side::Sell => self
                .bids
                .iter()
                .rev()
                .take_while(|(&ticks, _)| ticks >= limit_ticks)
                .map(|(_, level)| level.total_size)
                .sum(),
        }
    }

    fn match_order(&mut self, order: &mut BookOrder, now: Nanos) -> Vec<TimestampedEvent> {
        // Phase 1: Collect match actions without borrowing self mutably
        let actions = self.collect_match_actions(order);

        // Phase 2: Apply actions and generate events
        let mut events = Vec::new();

        for action in actions {
            match action {
                MatchAction::Fill(fill) => {
                    events.extend(self.apply_fill(fill, now));
                }
                MatchAction::CancelIncoming { order_id, qty } => {
                    self.stats.self_trades_prevented += 1;
                    events.push(self.make_cancel_ack_event(order_id, qty, now));
                }
                MatchAction::CancelResting { order_id, qty } => {
                    self.stats.self_trades_prevented += 1;
                    self.orders.remove(&order_id);
                    events.push(self.make_cancel_ack_event(order_id, qty, now));
                }
                MatchAction::CancelBoth {
                    incoming_id,
                    incoming_qty,
                    resting_id,
                    resting_qty,
                } => {
                    self.stats.self_trades_prevented += 1;
                    self.orders.remove(&resting_id);
                    events.push(self.make_cancel_ack_event(resting_id, resting_qty, now));
                    events.push(self.make_cancel_ack_event(incoming_id, incoming_qty, now));
                }
            }
        }

        events
    }

    fn collect_match_actions(&mut self, order: &mut BookOrder) -> Vec<MatchAction> {
        let mut actions = Vec::new();
        let tick_size = self.config.tick_size;
        let stp_enabled = self.config.self_trade_prevention;
        let stp_mode = self.config.stp_mode;
        let maker_fee_rate = self.config.fees.maker_fee_rate;
        let taker_fee_rate = self.config.fees.taker_fee_rate;
        let _ = (maker_fee_rate, taker_fee_rate); // Will use in apply_fill

        // Get contra book
        let contra_book = match order.side {
            Side::Buy => &mut self.asks,
            Side::Sell => &mut self.bids,
        };

        // Collect matchable levels
        let matchable_levels: Vec<PriceTicks> = match order.side {
            Side::Buy => contra_book
                .keys()
                .take_while(|&&ask| ask <= order.price_ticks)
                .copied()
                .collect(),
            Side::Sell => contra_book
                .keys()
                .rev()
                .take_while(|&&bid| bid >= order.price_ticks)
                .copied()
                .collect(),
        };

        let mut levels_to_remove = Vec::new();

        for level_ticks in matchable_levels {
            if order.remaining_size <= 0.0 {
                break;
            }

            let Some(level) = contra_book.get_mut(&level_ticks) else {
                continue;
            };

            while order.remaining_size > 0.0 && !level.is_empty() {
                let resting = level.front().unwrap();

                // Self-trade prevention check
                if stp_enabled && resting.trader_id == order.trader_id {
                    match stp_mode {
                        SelfTradeMode::CancelNewest => {
                            let qty = order.remaining_size;
                            order.remaining_size = 0.0;
                            actions.push(MatchAction::CancelIncoming {
                                order_id: order.order_id,
                                qty,
                            });
                            break;
                        }
                        SelfTradeMode::CancelOldest => {
                            let resting_id = resting.order_id;
                            let resting_qty = resting.remaining_size;
                            level.remove_front();
                            actions.push(MatchAction::CancelResting {
                                order_id: resting_id,
                                qty: resting_qty,
                            });
                            continue;
                        }
                        SelfTradeMode::CancelBoth => {
                            let resting_id = resting.order_id;
                            let resting_qty = resting.remaining_size;
                            let incoming_qty = order.remaining_size;
                            level.remove_front();
                            order.remaining_size = 0.0;
                            actions.push(MatchAction::CancelBoth {
                                incoming_id: order.order_id,
                                incoming_qty,
                                resting_id,
                                resting_qty,
                            });
                            break;
                        }
                        SelfTradeMode::DecrementAndCancel => {
                            let resting_qty = resting.remaining_size;
                            let decrement = order.remaining_size.min(resting_qty);
                            level.reduce_front(decrement);
                            order.remaining_size -= decrement;
                            continue;
                        }
                    }
                }

                // Normal fill
                let fill_size = order.remaining_size.min(resting.remaining_size);
                let resting_id = resting.order_id;
                let resting_remaining = resting.remaining_size - fill_size;
                let taker_remaining = order.remaining_size - fill_size;

                actions.push(MatchAction::Fill(FillInstruction {
                    taker_order_id: order.order_id,
                    maker_order_id: resting_id,
                    price_ticks: level_ticks,
                    fill_size,
                    taker_leaves: taker_remaining,
                    maker_leaves: resting_remaining,
                }));

                order.remaining_size -= fill_size;
                level.reduce_front(fill_size);

                if resting_remaining <= 0.0 {
                    // Will remove from orders map in apply phase
                }
            }

            if level.is_empty() {
                levels_to_remove.push(level_ticks);
            }
        }

        // Remove empty levels
        for ticks in levels_to_remove {
            contra_book.remove(&ticks);
        }

        actions
    }

    fn apply_fill(&mut self, fill: FillInstruction, now: Nanos) -> Vec<TimestampedEvent> {
        let fill_price = ticks_to_price(fill.price_ticks, self.config.tick_size);
        let taker_fee = fill.fill_size * fill_price * self.config.fees.taker_fee_rate;
        let maker_fee = fill.fill_size * fill_price * self.config.fees.maker_fee_rate;

        let fill_id = format!("fill_{}", self.next_fill_id);
        self.next_fill_id += 1;

        self.stats.fills += 1;
        self.stats.total_volume += fill.fill_size * fill_price;

        if fill.maker_leaves <= 0.0 {
            self.orders.remove(&fill.maker_order_id);
        }

        let ack_time = now + self.config.ack_latency_ns;

        vec![
            TimestampedEvent::with_times(
                now,
                ack_time,
                StreamSource::OrderManagement as u8,
                Event::Fill {
                    order_id: fill.taker_order_id,
                    price: fill_price,
                    size: fill.fill_size,
                    is_maker: false,
                    leaves_qty: fill.taker_leaves,
                    fee: taker_fee,
                    fill_id: Some(fill_id.clone()),
                },
            ),
            TimestampedEvent::with_times(
                now,
                ack_time,
                StreamSource::OrderManagement as u8,
                Event::Fill {
                    order_id: fill.maker_order_id,
                    price: fill_price,
                    size: fill.fill_size,
                    is_maker: true,
                    leaves_qty: fill.maker_leaves,
                    fee: maker_fee,
                    fill_id: Some(format!("{}_maker", fill_id)),
                },
            ),
        ]
    }

    fn add_to_book(&mut self, order: BookOrder) {
        let side = order.side;
        let price_ticks = order.price_ticks;
        let order_id = order.order_id;

        self.orders
            .insert(order_id, OrderLocation { side, price_ticks });

        let book = match side {
            Side::Buy => &mut self.bids,
            Side::Sell => &mut self.asks,
        };

        book.entry(price_ticks)
            .or_insert_with(PriceLevel::default)
            .add_order(order);
    }

    fn make_reject_event(
        &self,
        order_id: OrderId,
        client_order_id: Option<String>,
        reason: RejectReason,
        now: Nanos,
    ) -> TimestampedEvent {
        TimestampedEvent::with_times(
            now,
            now + self.config.ack_latency_ns,
            StreamSource::OrderManagement as u8,
            Event::OrderReject {
                order_id,
                client_order_id,
                reason,
            },
        )
    }

    fn make_cancel_ack_event(
        &self,
        order_id: OrderId,
        cancelled_qty: Size,
        now: Nanos,
    ) -> TimestampedEvent {
        TimestampedEvent::with_times(
            now,
            now + self.config.ack_latency_ns,
            StreamSource::OrderManagement as u8,
            Event::CancelAck {
                order_id,
                cancelled_qty,
            },
        )
    }
}

/// Multi-token matching engine manager.
pub struct MatchingEngine {
    books: HashMap<String, LimitOrderBook>,
    config: MatchingConfig,
    /// Aggregate statistics
    pub total_stats: MatchingStats,
}

impl MatchingEngine {
    pub fn new(config: MatchingConfig) -> Self {
        Self {
            books: HashMap::new(),
            config,
            total_stats: MatchingStats::default(),
        }
    }

    /// Get or create a book for a token.
    pub fn get_or_create_book(&mut self, token_id: &str) -> &mut LimitOrderBook {
        let config = self.config.clone();
        self.books
            .entry(token_id.to_string())
            .or_insert_with(|| LimitOrderBook::new(token_id, config))
    }

    /// Submit an order.
    pub fn submit_order(&mut self, req: OrderRequest, now: Nanos) -> Vec<TimestampedEvent> {
        let token_id = req.token_id.clone();
        let book = self.get_or_create_book(&token_id);
        let events = book.submit_order(req, now);
        self.update_total_stats();
        events
    }

    /// Cancel an order.
    pub fn cancel_order(
        &mut self,
        token_id: &str,
        req: CancelRequest,
        now: Nanos,
    ) -> Vec<TimestampedEvent> {
        if let Some(book) = self.books.get_mut(token_id) {
            let events = book.cancel_order(req, now);
            self.update_total_stats();
            events
        } else {
            vec![TimestampedEvent::new(
                now,
                StreamSource::OrderManagement as u8,
                Event::OrderReject {
                    order_id: req.order_id,
                    client_order_id: req.client_order_id,
                    reason: RejectReason::Unknown("Token not found".into()),
                },
            )]
        }
    }

    /// Get a book (read-only).
    pub fn get_book(&self, token_id: &str) -> Option<&LimitOrderBook> {
        self.books.get(token_id)
    }

    /// Iterate over all books.
    pub fn iter_books(&self) -> impl Iterator<Item = (&String, &LimitOrderBook)> {
        self.books.iter()
    }

    fn update_total_stats(&mut self) {
        self.total_stats = MatchingStats::default();
        for book in self.books.values() {
            self.total_stats.orders_submitted += book.stats.orders_submitted;
            self.total_stats.orders_accepted += book.stats.orders_accepted;
            self.total_stats.orders_rejected += book.stats.orders_rejected;
            self.total_stats.orders_cancelled += book.stats.orders_cancelled;
            self.total_stats.fills += book.stats.fills;
            self.total_stats.total_volume += book.stats.total_volume;
            self.total_stats.self_trades_prevented += book.stats.self_trades_prevented;
            self.total_stats.post_only_rejections += book.stats.post_only_rejections;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_order(side: Side, price: f64, size: f64, trader: &str) -> OrderRequest {
        OrderRequest {
            client_order_id: format!("order_{}_{}", trader, price),
            token_id: "token123".into(),
            side,
            price,
            size,
            order_type: OrderType::Limit,
            time_in_force: TimeInForce::Gtc,
            trader_id: trader.into(),
            post_only: false,
            reduce_only: false,
        }
    }

    #[test]
    fn test_simple_limit_order() {
        let config = MatchingConfig::default();
        let mut book = LimitOrderBook::new("token123", config);

        let req = make_order(Side::Buy, 0.45, 100.0, "trader1");
        let events = book.submit_order(req, 1000);

        assert!(events
            .iter()
            .any(|e| matches!(e.event, Event::OrderAck { .. })));
        assert_eq!(book.order_count(), 1);
        assert_eq!(book.best_bid(), Some((0.45, 100.0)));
    }

    #[test]
    fn test_crossing_orders_match() {
        let config = MatchingConfig::default();
        let mut book = LimitOrderBook::new("token123", config);

        let sell = make_order(Side::Sell, 0.50, 100.0, "trader1");
        book.submit_order(sell, 1000);

        let buy = make_order(Side::Buy, 0.50, 50.0, "trader2");
        let events = book.submit_order(buy, 2000);

        let fills: Vec<_> = events
            .iter()
            .filter(|e| matches!(e.event, Event::Fill { .. }))
            .collect();

        assert_eq!(fills.len(), 2);
        assert_eq!(book.best_ask(), Some((0.50, 50.0)));
    }

    #[test]
    fn test_partial_fill() {
        let config = MatchingConfig::default();
        let mut book = LimitOrderBook::new("token123", config);

        let sell = make_order(Side::Sell, 0.50, 50.0, "trader1");
        book.submit_order(sell, 1000);

        let buy = make_order(Side::Buy, 0.50, 100.0, "trader2");
        let events = book.submit_order(buy, 2000);

        let taker_fill = events
            .iter()
            .find(|e| matches!(e.event, Event::Fill { is_maker: false, size, .. } if size == 50.0));
        assert!(taker_fill.is_some());

        assert_eq!(book.best_bid(), Some((0.50, 50.0)));
        assert_eq!(book.best_ask(), None);
    }

    #[test]
    fn test_self_trade_prevention() {
        let config = MatchingConfig {
            self_trade_prevention: true,
            stp_mode: SelfTradeMode::CancelNewest,
            ..Default::default()
        };
        let mut book = LimitOrderBook::new("token123", config);

        let sell = make_order(Side::Sell, 0.50, 100.0, "trader1");
        book.submit_order(sell, 1000);

        let buy = make_order(Side::Buy, 0.50, 50.0, "trader1");
        let events = book.submit_order(buy, 2000);

        let cancel = events
            .iter()
            .any(|e| matches!(e.event, Event::CancelAck { .. }));
        assert!(cancel);
        assert_eq!(book.best_ask(), Some((0.50, 100.0)));
        assert_eq!(book.stats.self_trades_prevented, 1);
    }

    #[test]
    fn test_post_only_rejection() {
        let config = MatchingConfig::default();
        let mut book = LimitOrderBook::new("token123", config);

        let sell = make_order(Side::Sell, 0.50, 100.0, "trader1");
        book.submit_order(sell, 1000);

        let buy = OrderRequest {
            post_only: true,
            ..make_order(Side::Buy, 0.50, 50.0, "trader2")
        };
        let events = book.submit_order(buy, 2000);

        let rejected = events
            .iter()
            .any(|e| matches!(e.event, Event::OrderReject { .. }));
        assert!(rejected);
        assert_eq!(book.stats.post_only_rejections, 1);
    }

    #[test]
    fn test_cancel_order() {
        let config = MatchingConfig::default();
        let mut book = LimitOrderBook::new("token123", config);

        let req = make_order(Side::Buy, 0.45, 100.0, "trader1");
        book.submit_order(req, 1000);

        let order_id = 1;

        let cancel_req = CancelRequest {
            order_id,
            client_order_id: None,
        };
        let events = book.cancel_order(cancel_req, 2000);

        let cancel_ack = events.iter().find(
            |e| matches!(e.event, Event::CancelAck { cancelled_qty, .. } if cancelled_qty == 100.0),
        );
        assert!(cancel_ack.is_some());
        assert_eq!(book.order_count(), 0);
    }

    #[test]
    fn test_ioc_order() {
        let config = MatchingConfig::default();
        let mut book = LimitOrderBook::new("token123", config);

        let sell = make_order(Side::Sell, 0.50, 50.0, "trader1");
        book.submit_order(sell, 1000);

        let buy = OrderRequest {
            time_in_force: TimeInForce::Ioc,
            ..make_order(Side::Buy, 0.50, 100.0, "trader2")
        };
        let events = book.submit_order(buy, 2000);

        let fill = events
            .iter()
            .any(|e| matches!(e.event, Event::Fill { is_maker: false, size, .. } if size == 50.0));
        let cancel = events.iter().any(
            |e| matches!(e.event, Event::CancelAck { cancelled_qty, .. } if cancelled_qty == 50.0),
        );

        assert!(fill);
        assert!(cancel);
        assert_eq!(book.order_count(), 0);
    }

    #[test]
    fn test_price_ticks_conversion() {
        assert_eq!(price_to_ticks(0.45, 0.01), 45);
        assert_eq!(price_to_ticks(0.999, 0.01), 99);
        assert_eq!(price_to_ticks(0.001, 0.01), 1);

        assert_eq!(ticks_to_price(45, 0.01), 0.45);
        assert_eq!(ticks_to_price(99, 0.01), 0.99);
    }

    #[test]
    fn test_maker_taker_fees() {
        let config = MatchingConfig {
            fees: FeeConfig {
                maker_fee_rate: -0.0001,
                taker_fee_rate: 0.001,
            },
            ..Default::default()
        };
        let mut book = LimitOrderBook::new("token123", config);

        let sell = make_order(Side::Sell, 0.50, 100.0, "trader1");
        book.submit_order(sell, 1000);

        let buy = make_order(Side::Buy, 0.50, 100.0, "trader2");
        let events = book.submit_order(buy, 2000);

        for e in events {
            if let Event::Fill {
                is_maker,
                fee,
                size,
                price,
                ..
            } = e.event
            {
                let notional = size * price;
                if is_maker {
                    assert!((fee - notional * -0.0001).abs() < 0.0001);
                } else {
                    assert!((fee - notional * 0.001).abs() < 0.0001);
                }
            }
        }
    }
}

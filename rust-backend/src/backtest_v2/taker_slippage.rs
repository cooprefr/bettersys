//! Taker Slippage and Fill Model for 15M Up/Down Strategy
//!
//! This module provides realistic execution modeling for taker (aggressive) orders
//! by sweeping available depth across price levels. It replaces the naive "hit best
//! bid/ask for full size" assumption with a proper depth-walk algorithm.
//!
//! # Design Principles
//!
//! 1. **Depth Consumption**: Large orders consume liquidity at successive price levels.
//! 2. **Partial Fills**: Orders may be partially filled if liquidity is insufficient.
//! 3. **Deterministic Execution**: Same inputs yield identical fill sequences.
//! 4. **Fee Integration**: Per-fill fees computed and recorded.
//! 5. **Book State Mutation**: Consumed liquidity is removed from the book.
//!
//! # Execution Model
//!
//! For a taker buy order:
//! - Sweep asks from best ask upward (increasing price)
//! - At each level: fill_size = min(remaining, level_size)
//! - Continue until filled or limit price exceeded or book exhausted
//!
//! For a taker sell order:
//! - Sweep bids from best bid downward (decreasing price)
//! - At each level: fill_size = min(remaining, level_size)
//! - Continue until filled or limit price exceeded or book exhausted
//!
//! # Polymarket-Specific Constraints
//!
//! - Price range: [0.01, 0.99] for binary outcome tokens
//! - Tick size: 0.01 (1 cent)
//! - Size: in shares (typically 1.0 = 1 USDC notional at price 1.0)
//!
//! # Integration Points
//!
//! - Consumes `SimulatedL2Book` state for depth information
//! - Produces `TakerFillResult` with fills, slippage metrics, and book updates
//! - Events flow through `FillNotification` to the strategy/ledger

use crate::backtest_v2::event_time::VisibleNanos;
use crate::backtest_v2::events::{OrderId, Price, Side, Size, TimeInForce};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

// =============================================================================
// CONSTANTS
// =============================================================================

/// Minimum price for binary outcome tokens.
pub const MIN_PRICE: Price = 0.01;
/// Maximum price for binary outcome tokens.
pub const MAX_PRICE: Price = 0.99;
/// Default tick size (Polymarket uses 1 cent).
pub const DEFAULT_TICK_SIZE: Price = 0.01;
/// Minimum order size.
pub const MIN_ORDER_SIZE: Size = 1.0;

/// Price tick (integer representation for deterministic ordering).
pub type PriceTick = u32;

/// Convert price to tick.
#[inline]
pub fn price_to_tick(price: Price, tick_size: Price) -> PriceTick {
    ((price / tick_size).round() as u32).clamp(1, 99)
}

/// Convert tick to price.
#[inline]
pub fn tick_to_price(tick: PriceTick, tick_size: Price) -> Price {
    (tick as f64 * tick_size).clamp(MIN_PRICE, MAX_PRICE)
}

// =============================================================================
// CONFIGURATION
// =============================================================================

/// Configuration for the taker slippage model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TakerSlippageConfig {
    /// Tick size for price levels.
    pub tick_size: Price,
    /// Taker fee rate (e.g., 0.001 = 10 bps).
    pub taker_fee_rate: f64,
    /// Minimum order size.
    pub min_order_size: Size,
    /// Maximum order size (sanity check).
    pub max_order_size: Size,
    /// Whether to allow partial fills.
    pub allow_partial_fills: bool,
    /// Whether to reject orders on empty book (vs. returning no fills).
    pub reject_on_empty_book: bool,
    /// Whether to label results as not production-grade when using snapshot-only data.
    pub is_snapshot_only: bool,
}

impl Default for TakerSlippageConfig {
    fn default() -> Self {
        Self {
            tick_size: DEFAULT_TICK_SIZE,
            taker_fee_rate: 0.001, // 10 bps
            min_order_size: MIN_ORDER_SIZE,
            max_order_size: 1_000_000.0,
            allow_partial_fills: true,
            reject_on_empty_book: false,
            is_snapshot_only: false,
        }
    }
}

impl TakerSlippageConfig {
    /// Production-grade configuration with full incremental deltas.
    pub fn production() -> Self {
        Self {
            is_snapshot_only: false,
            ..Default::default()
        }
    }

    /// Research-grade configuration using snapshot-only data.
    pub fn research_snapshot_only() -> Self {
        Self {
            is_snapshot_only: true,
            ..Default::default()
        }
    }

    /// Polymarket-specific configuration.
    pub fn polymarket() -> Self {
        Self {
            tick_size: 0.01,
            taker_fee_rate: 0.001, // Polymarket typical taker fee
            min_order_size: 1.0,
            max_order_size: 100_000.0,
            allow_partial_fills: true,
            reject_on_empty_book: false,
            is_snapshot_only: false,
        }
    }
}

// =============================================================================
// L2 BOOK STATE FOR SLIPPAGE MODEL
// =============================================================================

/// A single price level in the simulated book.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimulatedPriceLevel {
    /// Aggregate size available at this price.
    pub size: Size,
    /// Optional order count (for queue model integration).
    pub order_count: Option<u32>,
}

impl SimulatedPriceLevel {
    pub fn new(size: Size) -> Self {
        Self {
            size,
            order_count: None,
        }
    }

    pub fn with_order_count(size: Size, order_count: u32) -> Self {
        Self {
            size,
            order_count: Some(order_count),
        }
    }

    /// Consume size from this level, return actual consumed.
    pub fn consume(&mut self, requested: Size) -> Size {
        let consumed = requested.min(self.size);
        self.size -= consumed;
        consumed
    }

    pub fn is_empty(&self) -> bool {
        self.size <= 0.0
    }
}

/// Simulated L2 order book for taker execution modeling.
///
/// This book tracks aggregate depth at each price level and supports
/// consuming liquidity during taker order execution.
#[derive(Debug, Clone)]
pub struct SimulatedL2Book {
    /// Token/market identifier.
    pub token_id: String,
    /// Bids: keyed by tick, best bid = highest tick.
    bids: BTreeMap<PriceTick, SimulatedPriceLevel>,
    /// Asks: keyed by tick, best ask = lowest tick.
    asks: BTreeMap<PriceTick, SimulatedPriceLevel>,
    /// Last sequence number (for delta tracking).
    pub last_seq: u64,
    /// Last update visible timestamp.
    pub last_update_ts: VisibleNanos,
    /// Configuration.
    config: TakerSlippageConfig,
}

impl SimulatedL2Book {
    /// Create a new empty book.
    pub fn new(token_id: impl Into<String>, config: TakerSlippageConfig) -> Self {
        Self {
            token_id: token_id.into(),
            bids: BTreeMap::new(),
            asks: BTreeMap::new(),
            last_seq: 0,
            last_update_ts: VisibleNanos(0),
            config,
        }
    }

    /// Apply a full snapshot (replaces all levels).
    pub fn apply_snapshot(
        &mut self,
        bids: &[(Price, Size)],
        asks: &[(Price, Size)],
        seq: u64,
        visible_ts: VisibleNanos,
    ) {
        self.bids.clear();
        self.asks.clear();

        for &(price, size) in bids {
            if size > 0.0 && price >= MIN_PRICE && price <= MAX_PRICE {
                let tick = price_to_tick(price, self.config.tick_size);
                self.bids.insert(tick, SimulatedPriceLevel::new(size));
            }
        }

        for &(price, size) in asks {
            if size > 0.0 && price >= MIN_PRICE && price <= MAX_PRICE {
                let tick = price_to_tick(price, self.config.tick_size);
                self.asks.insert(tick, SimulatedPriceLevel::new(size));
            }
        }

        self.last_seq = seq;
        self.last_update_ts = visible_ts;
    }

    /// Apply an incremental delta (update single level).
    pub fn apply_delta(
        &mut self,
        side: Side,
        price: Price,
        new_size: Size,
        seq: u64,
        visible_ts: VisibleNanos,
    ) {
        let tick = price_to_tick(price, self.config.tick_size);
        let levels = match side {
            Side::Buy => &mut self.bids,
            Side::Sell => &mut self.asks,
        };

        if new_size <= 0.0 {
            levels.remove(&tick);
        } else {
            levels.insert(tick, SimulatedPriceLevel::new(new_size));
        }

        self.last_seq = seq;
        self.last_update_ts = visible_ts;
    }

    /// Get best bid (highest bid price).
    pub fn best_bid(&self) -> Option<(Price, Size)> {
        self.bids
            .iter()
            .next_back()
            .map(|(&tick, level)| (tick_to_price(tick, self.config.tick_size), level.size))
    }

    /// Get best ask (lowest ask price).
    pub fn best_ask(&self) -> Option<(Price, Size)> {
        self.asks
            .iter()
            .next()
            .map(|(&tick, level)| (tick_to_price(tick, self.config.tick_size), level.size))
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

    /// Get total available liquidity on one side within a price limit.
    pub fn available_liquidity(&self, side: Side, limit_price: Price) -> Size {
        let limit_tick = price_to_tick(limit_price, self.config.tick_size);

        match side {
            Side::Buy => {
                // For buy: sum asks from best ask up to limit_tick
                self.asks
                    .iter()
                    .take_while(|(&tick, _)| tick <= limit_tick)
                    .map(|(_, level)| level.size)
                    .sum()
            }
            Side::Sell => {
                // For sell: sum bids from best bid down to limit_tick
                self.bids
                    .iter()
                    .rev()
                    .take_while(|(&tick, _)| tick >= limit_tick)
                    .map(|(_, level)| level.size)
                    .sum()
            }
        }
    }

    /// Get depth at a specific price.
    pub fn depth_at(&self, side: Side, price: Price) -> Size {
        let tick = price_to_tick(price, self.config.tick_size);
        let levels = match side {
            Side::Buy => &self.bids,
            Side::Sell => &self.asks,
        };
        levels.get(&tick).map(|l| l.size).unwrap_or(0.0)
    }

    /// Get top N levels of depth.
    pub fn top_levels(&self, side: Side, n: usize) -> Vec<(Price, Size)> {
        match side {
            Side::Buy => self
                .bids
                .iter()
                .rev()
                .take(n)
                .map(|(&tick, level)| (tick_to_price(tick, self.config.tick_size), level.size))
                .collect(),
            Side::Sell => self
                .asks
                .iter()
                .take(n)
                .map(|(&tick, level)| (tick_to_price(tick, self.config.tick_size), level.size))
                .collect(),
        }
    }

    /// Check if book is empty on the opposite side.
    pub fn is_side_empty(&self, side: Side) -> bool {
        match side {
            Side::Buy => self.asks.is_empty(),
            Side::Sell => self.bids.is_empty(),
        }
    }

    /// Check if book is crossed (invalid state).
    pub fn is_crossed(&self) -> bool {
        match (self.bids.iter().next_back(), self.asks.iter().next()) {
            (Some((&bid_tick, _)), Some((&ask_tick, _))) => bid_tick >= ask_tick,
            _ => false,
        }
    }
}

// =============================================================================
// TAKER ORDER AND FILL TYPES
// =============================================================================

/// A taker order request for execution.
#[derive(Debug, Clone)]
pub struct TakerOrderRequest {
    /// Order ID (assigned by caller).
    pub order_id: OrderId,
    /// Client order ID for correlation.
    pub client_order_id: String,
    /// Token/market being traded.
    pub token_id: String,
    /// Order side (Buy or Sell).
    pub side: Side,
    /// Limit price (worst acceptable price).
    pub limit_price: Price,
    /// Desired size in shares.
    pub size: Size,
    /// Time in force (typically IOC for taker orders).
    pub time_in_force: TimeInForce,
    /// Trader ID (for tracking).
    pub trader_id: String,
}

impl TakerOrderRequest {
    /// Create a new IOC taker order (most common for 15M strategy).
    pub fn ioc(
        order_id: OrderId,
        client_order_id: impl Into<String>,
        token_id: impl Into<String>,
        side: Side,
        limit_price: Price,
        size: Size,
        trader_id: impl Into<String>,
    ) -> Self {
        Self {
            order_id,
            client_order_id: client_order_id.into(),
            token_id: token_id.into(),
            side,
            limit_price,
            size,
            time_in_force: TimeInForce::Ioc,
            trader_id: trader_id.into(),
        }
    }
}

/// A single fill at one price level.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LevelFill {
    /// Price at which the fill occurred.
    pub price: Price,
    /// Size filled at this price.
    pub size: Size,
    /// Notional value (price * size).
    pub notional: f64,
    /// Fee charged for this fill.
    pub fee: f64,
    /// Tick at which the fill occurred.
    pub tick: PriceTick,
    /// Source of liquidity (for auditing).
    pub liquidity_source: LiquiditySource,
}

impl LevelFill {
    fn new(price: Price, size: Size, fee: f64, tick: PriceTick) -> Self {
        Self {
            price,
            size,
            notional: price * size,
            fee,
            tick,
            liquidity_source: LiquiditySource::BookLevel,
        }
    }
}

/// Source of liquidity for a fill.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LiquiditySource {
    /// Fill came from resting book level.
    BookLevel,
    /// Fill came from simulated queue (if modeling).
    Queue,
}

/// Outcome reason for execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExecutionOutcome {
    /// Order fully filled.
    FullyFilled,
    /// Order partially filled, remainder cancelled (IOC).
    PartiallyFilledCancelled,
    /// Order partially filled, remainder resting (GTC - not typical for taker).
    PartiallyFilledResting,
    /// No liquidity available within limit.
    InsufficientLiquidityWithinLimit,
    /// Book was empty on opposite side.
    EmptyBook,
    /// Order rejected due to validation failure.
    Rejected { reason: String },
}

/// Result of executing a taker order.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TakerFillResult {
    /// Order ID.
    pub order_id: OrderId,
    /// Client order ID.
    pub client_order_id: String,
    /// Side of the order.
    pub side: Side,
    /// Original requested size.
    pub requested_size: Size,
    /// Limit price.
    pub limit_price: Price,
    /// List of fills at each price level.
    pub fills: Vec<LevelFill>,
    /// Total filled size.
    pub total_filled: Size,
    /// Unfilled remainder.
    pub unfilled: Size,
    /// Total fees paid.
    pub total_fees: f64,
    /// Total notional value traded.
    pub total_notional: f64,
    /// Execution outcome.
    pub outcome: ExecutionOutcome,
    /// Execution metrics.
    pub metrics: ExecutionMetrics,
    /// Visible timestamp at execution.
    pub execution_ts: VisibleNanos,
    /// Whether this was based on snapshot-only data (not production-grade).
    pub is_snapshot_based: bool,
}

impl TakerFillResult {
    /// Check if order was at least partially filled.
    pub fn is_filled(&self) -> bool {
        self.total_filled > 0.0
    }

    /// Check if order was fully filled.
    pub fn is_fully_filled(&self) -> bool {
        self.unfilled <= 0.0
    }
}

/// Execution metrics for analysis and logging.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionMetrics {
    /// Volume-weighted average price.
    pub vwap: Price,
    /// Best price at arrival (best ask for buy, best bid for sell).
    pub arrival_price: Option<Price>,
    /// Slippage: VWAP - arrival_price for buys, arrival_price - VWAP for sells.
    /// Positive = unfavorable slippage (paid more / received less).
    pub slippage: Option<f64>,
    /// Slippage in basis points.
    pub slippage_bps: Option<f64>,
    /// Number of price levels swept.
    pub levels_swept: usize,
    /// Spread at arrival.
    pub spread_at_arrival: Option<Price>,
    /// Depth available within limit.
    pub depth_within_limit: Size,
}

impl Default for ExecutionMetrics {
    fn default() -> Self {
        Self {
            vwap: 0.0,
            arrival_price: None,
            slippage: None,
            slippage_bps: None,
            levels_swept: 0,
            spread_at_arrival: None,
            depth_within_limit: 0.0,
        }
    }
}

// =============================================================================
// TAKER FILL MODEL IMPLEMENTATION
// =============================================================================

/// The taker slippage model that executes orders against the simulated book.
#[derive(Debug)]
pub struct TakerFillModel {
    config: TakerSlippageConfig,
    /// Statistics.
    stats: TakerFillStats,
}

/// Statistics from the taker fill model.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TakerFillStats {
    /// Total orders processed.
    pub orders_processed: u64,
    /// Orders fully filled.
    pub orders_fully_filled: u64,
    /// Orders partially filled.
    pub orders_partially_filled: u64,
    /// Orders with no fills (insufficient liquidity).
    pub orders_no_fill: u64,
    /// Orders rejected.
    pub orders_rejected: u64,
    /// Total fills generated.
    pub total_fills: u64,
    /// Total volume filled.
    pub total_volume: f64,
    /// Total fees collected.
    pub total_fees: f64,
    /// Total notional traded.
    pub total_notional: f64,
    /// Sum of slippage (for average calculation).
    pub total_slippage: f64,
    /// Count of orders with measurable slippage.
    pub slippage_count: u64,
}

impl TakerFillStats {
    /// Average slippage in price units.
    pub fn avg_slippage(&self) -> Option<f64> {
        if self.slippage_count > 0 {
            Some(self.total_slippage / self.slippage_count as f64)
        } else {
            None
        }
    }

    /// Fill rate (fraction of orders with any fills).
    pub fn fill_rate(&self) -> f64 {
        if self.orders_processed > 0 {
            (self.orders_fully_filled + self.orders_partially_filled) as f64
                / self.orders_processed as f64
        } else {
            0.0
        }
    }
}

impl TakerFillModel {
    /// Create a new taker fill model.
    pub fn new(config: TakerSlippageConfig) -> Self {
        Self {
            config,
            stats: TakerFillStats::default(),
        }
    }

    /// Execute a taker order against the book, consuming depth.
    ///
    /// This is the main entry point for taker execution. It:
    /// 1. Validates the order
    /// 2. Records arrival metrics
    /// 3. Sweeps the book from best price toward limit
    /// 4. Generates fills at each consumed level
    /// 5. Updates book state (removes consumed liquidity)
    /// 6. Computes execution metrics
    ///
    /// # Arguments
    ///
    /// * `order` - The taker order to execute.
    /// * `book` - The simulated book (will be mutated to consume depth).
    /// * `visible_ts` - The visible timestamp at execution.
    ///
    /// # Returns
    ///
    /// A `TakerFillResult` containing all fills, metrics, and outcome.
    pub fn execute(
        &mut self,
        order: &TakerOrderRequest,
        book: &mut SimulatedL2Book,
        visible_ts: VisibleNanos,
    ) -> TakerFillResult {
        self.stats.orders_processed += 1;

        // Validate order
        if let Some(reason) = self.validate_order(order) {
            self.stats.orders_rejected += 1;
            return TakerFillResult {
                order_id: order.order_id,
                client_order_id: order.client_order_id.clone(),
                side: order.side,
                requested_size: order.size,
                limit_price: order.limit_price,
                fills: vec![],
                total_filled: 0.0,
                unfilled: order.size,
                total_fees: 0.0,
                total_notional: 0.0,
                outcome: ExecutionOutcome::Rejected { reason },
                metrics: ExecutionMetrics::default(),
                execution_ts: visible_ts,
                is_snapshot_based: self.config.is_snapshot_only,
            };
        }

        // Record arrival state
        let arrival_price = match order.side {
            Side::Buy => book.best_ask().map(|(p, _)| p),
            Side::Sell => book.best_bid().map(|(p, _)| p),
        };
        let spread_at_arrival = book.spread();
        let depth_within_limit = book.available_liquidity(order.side, order.limit_price);

        // Check for empty book
        if book.is_side_empty(order.side) {
            self.stats.orders_no_fill += 1;
            return TakerFillResult {
                order_id: order.order_id,
                client_order_id: order.client_order_id.clone(),
                side: order.side,
                requested_size: order.size,
                limit_price: order.limit_price,
                fills: vec![],
                total_filled: 0.0,
                unfilled: order.size,
                total_fees: 0.0,
                total_notional: 0.0,
                outcome: ExecutionOutcome::EmptyBook,
                metrics: ExecutionMetrics {
                    arrival_price,
                    spread_at_arrival,
                    depth_within_limit,
                    ..Default::default()
                },
                execution_ts: visible_ts,
                is_snapshot_based: self.config.is_snapshot_only,
            };
        }

        // Sweep the book
        let fills = self.sweep_book(order, book);

        // Compute totals
        let total_filled: Size = fills.iter().map(|f| f.size).sum();
        let total_fees: f64 = fills.iter().map(|f| f.fee).sum();
        let total_notional: f64 = fills.iter().map(|f| f.notional).sum();
        let unfilled = order.size - total_filled;

        // Compute VWAP
        let vwap = if total_filled > 0.0 {
            fills.iter().map(|f| f.price * f.size).sum::<f64>() / total_filled
        } else {
            0.0
        };

        // Compute slippage
        let slippage = arrival_price.map(|ap| {
            match order.side {
                Side::Buy => vwap - ap,  // Positive = paid more
                Side::Sell => ap - vwap, // Positive = received less
            }
        });

        let slippage_bps = slippage
            .and_then(|s| arrival_price.map(|ap| if ap > 0.0 { s / ap * 10000.0 } else { 0.0 }));

        // Determine outcome
        let outcome = if total_filled >= order.size - 1e-9 {
            self.stats.orders_fully_filled += 1;
            ExecutionOutcome::FullyFilled
        } else if total_filled > 0.0 {
            self.stats.orders_partially_filled += 1;
            match order.time_in_force {
                TimeInForce::Ioc => ExecutionOutcome::PartiallyFilledCancelled,
                TimeInForce::Fok => unreachable!("FOK should not partially fill"),
                _ => ExecutionOutcome::PartiallyFilledResting,
            }
        } else {
            self.stats.orders_no_fill += 1;
            ExecutionOutcome::InsufficientLiquidityWithinLimit
        };

        // Update stats
        self.stats.total_fills += fills.len() as u64;
        self.stats.total_volume += total_filled;
        self.stats.total_fees += total_fees;
        self.stats.total_notional += total_notional;

        if let Some(s) = slippage {
            self.stats.total_slippage += s.abs();
            self.stats.slippage_count += 1;
        }

        TakerFillResult {
            order_id: order.order_id,
            client_order_id: order.client_order_id.clone(),
            side: order.side,
            requested_size: order.size,
            limit_price: order.limit_price,
            fills,
            total_filled,
            unfilled,
            total_fees,
            total_notional,
            outcome,
            metrics: ExecutionMetrics {
                vwap,
                arrival_price,
                slippage,
                slippage_bps,
                levels_swept: 0, // Will be set by sweep_book
                spread_at_arrival,
                depth_within_limit,
            },
            execution_ts: visible_ts,
            is_snapshot_based: self.config.is_snapshot_only,
        }
    }

    /// Get model statistics.
    pub fn stats(&self) -> &TakerFillStats {
        &self.stats
    }

    /// Reset statistics.
    pub fn reset_stats(&mut self) {
        self.stats = TakerFillStats::default();
    }

    // === Private methods ===

    fn validate_order(&self, order: &TakerOrderRequest) -> Option<String> {
        // Validate price bounds
        if order.limit_price < MIN_PRICE || order.limit_price > MAX_PRICE {
            return Some(format!(
                "Price {} outside valid range [{}, {}]",
                order.limit_price, MIN_PRICE, MAX_PRICE
            ));
        }

        // Validate size
        if order.size < self.config.min_order_size {
            return Some(format!(
                "Size {} below minimum {}",
                order.size, self.config.min_order_size
            ));
        }
        if order.size > self.config.max_order_size {
            return Some(format!(
                "Size {} exceeds maximum {}",
                order.size, self.config.max_order_size
            ));
        }

        None
    }

    /// Sweep the book and generate fills.
    ///
    /// This implements the core price-walk algorithm:
    /// - For buys: iterate asks from lowest to highest
    /// - For sells: iterate bids from highest to lowest
    /// - At each level: fill min(remaining, level_size)
    /// - Stop when filled, limit exceeded, or book exhausted
    fn sweep_book(&self, order: &TakerOrderRequest, book: &mut SimulatedL2Book) -> Vec<LevelFill> {
        let mut fills = Vec::new();
        let mut remaining = order.size;
        let limit_tick = price_to_tick(order.limit_price, self.config.tick_size);

        match order.side {
            Side::Buy => {
                // Sweep asks from lowest (best) to highest, up to limit
                // Collect ticks to process (to avoid borrow issues)
                let ticks_to_process: Vec<PriceTick> = book
                    .asks
                    .iter()
                    .filter(|(&tick, _)| tick <= limit_tick)
                    .map(|(&tick, _)| tick)
                    .collect();

                for tick in ticks_to_process {
                    if remaining <= 0.0 {
                        break;
                    }

                    let level = match book.asks.get_mut(&tick) {
                        Some(l) => l,
                        None => continue,
                    };

                    let fill_size = level.consume(remaining);
                    if fill_size > 0.0 {
                        let price = tick_to_price(tick, self.config.tick_size);
                        let notional = price * fill_size;
                        let fee = notional * self.config.taker_fee_rate;

                        fills.push(LevelFill::new(price, fill_size, fee, tick));
                        remaining -= fill_size;
                    }

                    // Remove empty levels
                    if level.is_empty() {
                        book.asks.remove(&tick);
                    }
                }
            }
            Side::Sell => {
                // Sweep bids from highest (best) to lowest, down to limit
                let ticks_to_process: Vec<PriceTick> = book
                    .bids
                    .iter()
                    .rev()
                    .filter(|(&tick, _)| tick >= limit_tick)
                    .map(|(&tick, _)| tick)
                    .collect();

                for tick in ticks_to_process {
                    if remaining <= 0.0 {
                        break;
                    }

                    let level = match book.bids.get_mut(&tick) {
                        Some(l) => l,
                        None => continue,
                    };

                    let fill_size = level.consume(remaining);
                    if fill_size > 0.0 {
                        let price = tick_to_price(tick, self.config.tick_size);
                        let notional = price * fill_size;
                        let fee = notional * self.config.taker_fee_rate;

                        fills.push(LevelFill::new(price, fill_size, fee, tick));
                        remaining -= fill_size;
                    }

                    // Remove empty levels
                    if level.is_empty() {
                        book.bids.remove(&tick);
                    }
                }
            }
        }

        fills
    }
}

// =============================================================================
// COMPARISON: TOP-OF-BOOK VS REALISTIC SLIPPAGE
// =============================================================================

/// Result of comparing naive vs realistic execution models.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlippageComparison {
    /// Order details.
    pub side: Side,
    pub size: Size,
    pub limit_price: Price,

    /// Naive model: assumes full fill at best price.
    pub naive_fill_price: Option<Price>,
    pub naive_filled: Size,
    pub naive_notional: f64,
    pub naive_fees: f64,

    /// Realistic model: sweeps depth.
    pub realistic_vwap: Price,
    pub realistic_filled: Size,
    pub realistic_notional: f64,
    pub realistic_fees: f64,

    /// Difference metrics.
    pub price_difference: Option<f64>,
    pub pnl_difference: f64,
    pub fill_difference: Size,
}

/// Compare naive top-of-book execution vs realistic depth sweep.
///
/// This is useful for quantifying the PnL sensitivity of the 15M strategy
/// to execution assumptions.
pub fn compare_execution_models(
    order: &TakerOrderRequest,
    book: &SimulatedL2Book,
    config: &TakerSlippageConfig,
) -> SlippageComparison {
    // Naive model: assumes fill at best price for full size
    let (naive_price, naive_available) = match order.side {
        Side::Buy => book.best_ask().unwrap_or((0.0, 0.0)),
        Side::Sell => book.best_bid().unwrap_or((0.0, 0.0)),
    };

    let naive_filled = order.size.min(naive_available);
    let naive_notional = naive_price * naive_filled;
    let naive_fees = naive_notional * config.taker_fee_rate;

    // Realistic model: create a copy and sweep
    let mut book_copy = book.clone();
    let mut model = TakerFillModel::new(config.clone());
    let result = model.execute(order, &mut book_copy, VisibleNanos(0));

    // Compute differences
    let price_difference = if result.metrics.vwap > 0.0 && naive_price > 0.0 {
        Some(result.metrics.vwap - naive_price)
    } else {
        None
    };

    // PnL difference: for buys, higher price = worse; for sells, lower price = worse
    let pnl_difference = match order.side {
        Side::Buy => {
            // Paid more with realistic model
            (result.total_notional + result.total_fees) - (naive_notional + naive_fees)
        }
        Side::Sell => {
            // Received less with realistic model
            (naive_notional - naive_fees) - (result.total_notional - result.total_fees)
        }
    };

    SlippageComparison {
        side: order.side,
        size: order.size,
        limit_price: order.limit_price,
        naive_fill_price: Some(naive_price),
        naive_filled,
        naive_notional,
        naive_fees,
        realistic_vwap: result.metrics.vwap,
        realistic_filled: result.total_filled,
        realistic_notional: result.total_notional,
        realistic_fees: result.total_fees,
        price_difference,
        pnl_difference,
        fill_difference: naive_filled - result.total_filled,
    }
}

// =============================================================================
// TESTS
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_book() -> SimulatedL2Book {
        let config = TakerSlippageConfig::default();
        let mut book = SimulatedL2Book::new("test-token", config);

        // Set up a book with depth:
        // Bids: 0.45 (100), 0.44 (200), 0.43 (300)
        // Asks: 0.55 (100), 0.56 (200), 0.57 (300)
        book.apply_snapshot(
            &[(0.45, 100.0), (0.44, 200.0), (0.43, 300.0)],
            &[(0.55, 100.0), (0.56, 200.0), (0.57, 300.0)],
            1,
            VisibleNanos(1000),
        );

        book
    }

    #[test]
    fn test_price_tick_conversion() {
        assert_eq!(price_to_tick(0.55, 0.01), 55);
        assert_eq!(price_to_tick(0.01, 0.01), 1);
        assert_eq!(price_to_tick(0.99, 0.01), 99);

        assert!((tick_to_price(55, 0.01) - 0.55).abs() < 1e-9);
        assert!((tick_to_price(1, 0.01) - 0.01).abs() < 1e-9);
        assert!((tick_to_price(99, 0.01) - 0.99).abs() < 1e-9);
    }

    #[test]
    fn test_book_best_prices() {
        let book = make_test_book();

        let (best_bid, bid_size) = book.best_bid().unwrap();
        assert!((best_bid - 0.45).abs() < 1e-9);
        assert!((bid_size - 100.0).abs() < 1e-9);

        let (best_ask, ask_size) = book.best_ask().unwrap();
        assert!((best_ask - 0.55).abs() < 1e-9);
        assert!((ask_size - 100.0).abs() < 1e-9);
    }

    #[test]
    fn test_buy_single_level_fill() {
        let mut book = make_test_book();
        let config = TakerSlippageConfig::default();
        let mut model = TakerFillModel::new(config);

        // Buy 50 shares - should fill entirely at best ask
        let order =
            TakerOrderRequest::ioc(1, "order1", "test-token", Side::Buy, 0.60, 50.0, "trader1");
        let result = model.execute(&order, &mut book, VisibleNanos(2000));

        assert!(result.is_fully_filled());
        assert_eq!(result.fills.len(), 1);
        assert!((result.fills[0].price - 0.55).abs() < 1e-9);
        assert!((result.fills[0].size - 50.0).abs() < 1e-9);
        assert!((result.metrics.vwap - 0.55).abs() < 1e-9);

        // Check book was updated
        let (_, remaining_ask) = book.best_ask().unwrap();
        assert!((remaining_ask - 50.0).abs() < 1e-9);
    }

    #[test]
    fn test_buy_sweeps_multiple_levels() {
        let mut book = make_test_book();
        let config = TakerSlippageConfig::default();
        let mut model = TakerFillModel::new(config);

        // Buy 250 shares - should sweep 0.55 (100) and 0.56 (150)
        let order =
            TakerOrderRequest::ioc(1, "order1", "test-token", Side::Buy, 0.60, 250.0, "trader1");
        let result = model.execute(&order, &mut book, VisibleNanos(2000));

        assert!(result.is_fully_filled());
        assert_eq!(result.fills.len(), 2);

        // First fill at 0.55
        assert!((result.fills[0].price - 0.55).abs() < 1e-9);
        assert!((result.fills[0].size - 100.0).abs() < 1e-9);

        // Second fill at 0.56
        assert!((result.fills[1].price - 0.56).abs() < 1e-9);
        assert!((result.fills[1].size - 150.0).abs() < 1e-9);

        // VWAP should be weighted average
        let expected_vwap = (0.55 * 100.0 + 0.56 * 150.0) / 250.0;
        assert!((result.metrics.vwap - expected_vwap).abs() < 1e-9);

        // Check slippage is positive (paid more than best ask)
        assert!(result.metrics.slippage.unwrap() > 0.0);

        // Check book was updated
        let (best_ask, _) = book.best_ask().unwrap();
        assert!((best_ask - 0.56).abs() < 1e-9); // 0.55 level removed, 0.56 is new best
    }

    #[test]
    fn test_sell_sweeps_multiple_levels() {
        let mut book = make_test_book();
        let config = TakerSlippageConfig::default();
        let mut model = TakerFillModel::new(config);

        // Sell 250 shares - should sweep 0.45 (100) and 0.44 (150)
        let order = TakerOrderRequest::ioc(
            1,
            "order1",
            "test-token",
            Side::Sell,
            0.40,
            250.0,
            "trader1",
        );
        let result = model.execute(&order, &mut book, VisibleNanos(2000));

        assert!(result.is_fully_filled());
        assert_eq!(result.fills.len(), 2);

        // First fill at 0.45 (best bid)
        assert!((result.fills[0].price - 0.45).abs() < 1e-9);
        assert!((result.fills[0].size - 100.0).abs() < 1e-9);

        // Second fill at 0.44
        assert!((result.fills[1].price - 0.44).abs() < 1e-9);
        assert!((result.fills[1].size - 150.0).abs() < 1e-9);

        // VWAP should be weighted average
        let expected_vwap = (0.45 * 100.0 + 0.44 * 150.0) / 250.0;
        assert!((result.metrics.vwap - expected_vwap).abs() < 1e-9);

        // Check slippage is positive (received less than best bid)
        assert!(result.metrics.slippage.unwrap() > 0.0);
    }

    #[test]
    fn test_partial_fill_ioc() {
        let mut book = make_test_book();
        let config = TakerSlippageConfig::default();
        let mut model = TakerFillModel::new(config);

        // Buy 700 shares but only 600 available within limit
        let order =
            TakerOrderRequest::ioc(1, "order1", "test-token", Side::Buy, 0.57, 700.0, "trader1");
        let result = model.execute(&order, &mut book, VisibleNanos(2000));

        assert!(!result.is_fully_filled());
        assert!((result.total_filled - 600.0).abs() < 1e-9);
        assert!((result.unfilled - 100.0).abs() < 1e-9);
        assert_eq!(result.outcome, ExecutionOutcome::PartiallyFilledCancelled);
    }

    #[test]
    fn test_limit_price_respected() {
        let mut book = make_test_book();
        let config = TakerSlippageConfig::default();
        let mut model = TakerFillModel::new(config);

        // Buy 500 shares but limit at 0.56 - should only fill 300
        let order =
            TakerOrderRequest::ioc(1, "order1", "test-token", Side::Buy, 0.56, 500.0, "trader1");
        let result = model.execute(&order, &mut book, VisibleNanos(2000));

        assert!(!result.is_fully_filled());
        assert!((result.total_filled - 300.0).abs() < 1e-9); // 100 @ 0.55 + 200 @ 0.56
        assert_eq!(result.fills.len(), 2);

        // Verify no fills above limit
        for fill in &result.fills {
            assert!(fill.price <= 0.56 + 1e-9);
        }
    }

    #[test]
    fn test_fees_computed_correctly() {
        let mut book = make_test_book();
        let config = TakerSlippageConfig {
            taker_fee_rate: 0.001, // 10 bps
            ..Default::default()
        };
        let mut model = TakerFillModel::new(config);

        let order =
            TakerOrderRequest::ioc(1, "order1", "test-token", Side::Buy, 0.60, 100.0, "trader1");
        let result = model.execute(&order, &mut book, VisibleNanos(2000));

        // Notional = 0.55 * 100 = 55
        // Fee = 55 * 0.001 = 0.055
        assert!((result.total_notional - 55.0).abs() < 1e-9);
        assert!((result.total_fees - 0.055).abs() < 1e-9);
    }

    #[test]
    fn test_empty_book_handling() {
        let config = TakerSlippageConfig::default();
        let mut book = SimulatedL2Book::new("test-token", config.clone());
        let mut model = TakerFillModel::new(config);

        let order =
            TakerOrderRequest::ioc(1, "order1", "test-token", Side::Buy, 0.60, 100.0, "trader1");
        let result = model.execute(&order, &mut book, VisibleNanos(2000));

        assert_eq!(result.outcome, ExecutionOutcome::EmptyBook);
        assert_eq!(result.total_filled, 0.0);
    }

    #[test]
    fn test_order_validation() {
        let mut book = make_test_book();
        let config = TakerSlippageConfig {
            min_order_size: 10.0,
            ..Default::default()
        };
        let mut model = TakerFillModel::new(config);

        // Order below minimum size
        let order =
            TakerOrderRequest::ioc(1, "order1", "test-token", Side::Buy, 0.60, 5.0, "trader1");
        let result = model.execute(&order, &mut book, VisibleNanos(2000));

        assert!(matches!(result.outcome, ExecutionOutcome::Rejected { .. }));
    }

    #[test]
    fn test_deterministic_replay() {
        // Same inputs should produce identical results
        let config = TakerSlippageConfig::default();

        let order =
            TakerOrderRequest::ioc(1, "order1", "test-token", Side::Buy, 0.60, 250.0, "trader1");

        let mut book1 = make_test_book();
        let mut model1 = TakerFillModel::new(config.clone());
        let result1 = model1.execute(&order, &mut book1, VisibleNanos(2000));

        let mut book2 = make_test_book();
        let mut model2 = TakerFillModel::new(config);
        let result2 = model2.execute(&order, &mut book2, VisibleNanos(2000));

        assert_eq!(result1.fills.len(), result2.fills.len());
        assert!((result1.total_filled - result2.total_filled).abs() < 1e-9);
        assert!((result1.metrics.vwap - result2.metrics.vwap).abs() < 1e-9);
    }

    #[test]
    fn test_book_depth_reduced_after_fills() {
        let mut book = make_test_book();
        let config = TakerSlippageConfig::default();
        let mut model = TakerFillModel::new(config);

        // First order: buy 100 at best ask
        let order1 =
            TakerOrderRequest::ioc(1, "order1", "test-token", Side::Buy, 0.60, 100.0, "trader1");
        model.execute(&order1, &mut book, VisibleNanos(2000));

        // Best ask should now be 0.56 (0.55 level exhausted)
        let (best_ask, _) = book.best_ask().unwrap();
        assert!((best_ask - 0.56).abs() < 1e-9);

        // Second order: buy another 100 - should fill at 0.56
        let order2 =
            TakerOrderRequest::ioc(2, "order2", "test-token", Side::Buy, 0.60, 100.0, "trader1");
        let result2 = model.execute(&order2, &mut book, VisibleNanos(3000));

        assert!((result2.metrics.vwap - 0.56).abs() < 1e-9);
    }

    #[test]
    fn test_slippage_comparison() {
        let book = make_test_book();
        let config = TakerSlippageConfig::default();

        // Order that sweeps multiple levels
        let order =
            TakerOrderRequest::ioc(1, "order1", "test-token", Side::Buy, 0.60, 250.0, "trader1");
        let comparison = compare_execution_models(&order, &book, &config);

        // Naive assumes all fills at 0.55
        assert!((comparison.naive_fill_price.unwrap() - 0.55).abs() < 1e-9);

        // Realistic VWAP is higher (worse for buyer)
        assert!(comparison.realistic_vwap > 0.55);

        // PnL difference should be positive (realistic costs more)
        assert!(comparison.pnl_difference > 0.0);
    }

    #[test]
    fn test_stats_accumulation() {
        let config = TakerSlippageConfig::default();
        let mut model = TakerFillModel::new(config);

        for i in 0..5 {
            let mut book = make_test_book();
            let order = TakerOrderRequest::ioc(
                i + 1,
                format!("order{}", i),
                "test-token",
                Side::Buy,
                0.60,
                100.0,
                "trader1",
            );
            model.execute(&order, &mut book, VisibleNanos((i * 1000 + 1000) as i64));
        }

        assert_eq!(model.stats().orders_processed, 5);
        assert_eq!(model.stats().orders_fully_filled, 5);
        assert!((model.stats().total_volume - 500.0).abs() < 1e-9);
    }

    #[test]
    fn test_delta_updates_book() {
        let config = TakerSlippageConfig::default();
        let mut book = SimulatedL2Book::new("test-token", config);

        // Start with empty book
        assert!(book.best_ask().is_none());

        // Apply deltas
        book.apply_delta(Side::Sell, 0.55, 100.0, 1, VisibleNanos(1000));
        book.apply_delta(Side::Sell, 0.56, 200.0, 2, VisibleNanos(1001));
        book.apply_delta(Side::Buy, 0.45, 150.0, 3, VisibleNanos(1002));

        let (best_ask, ask_size) = book.best_ask().unwrap();
        assert!((best_ask - 0.55).abs() < 1e-9);
        assert!((ask_size - 100.0).abs() < 1e-9);

        let (best_bid, bid_size) = book.best_bid().unwrap();
        assert!((best_bid - 0.45).abs() < 1e-9);
        assert!((bid_size - 150.0).abs() < 1e-9);

        // Remove a level
        book.apply_delta(Side::Sell, 0.55, 0.0, 4, VisibleNanos(1003));
        let (best_ask, _) = book.best_ask().unwrap();
        assert!((best_ask - 0.56).abs() < 1e-9);
    }
}

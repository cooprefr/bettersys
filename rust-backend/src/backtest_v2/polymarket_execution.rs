//! Polymarket Backtest Execution Adapter
//!
//! Production-grade execution adapter that enforces real Polymarket CLOB constraints
//! and produces deterministic execution outcomes for HFT-grade backtesting.
//!
//! # Design Principles
//!
//! 1. **Deterministic**: Same dataset + seed + config => same fills/fees/events
//! 2. **Hermetic**: No wall-clock, no external I/O, no async
//! 3. **Synchronous**: All execution logic is event-driven via visibility timestamps
//! 4. **Venue-Accurate**: Enforces tick size, fees, TIF, post-only, IOC, GTC
//!
//! # Rejection Philosophy
//!
//! For HFT-grade correctness, we default to REJECT off-tick prices rather than
//! silently rounding, because rounding changes strategy intent. The strategy
//! should send already-ticked prices.
//!
//! # Integration
//!
//! This adapter implements the OrderSender trait and sits between the Strategy
//! and the underlying MatchingEngine. Orders flow:
//!
//! Strategy -> PolymarketExecutionAdapter -> VenueSpec Validation -> MatchingEngine
//!                                        -> Latency Scheduling -> Event Queue

use crate::backtest_v2::clock::Nanos;
use crate::backtest_v2::events::{
    Event, Level, OrderId, OrderType, RejectReason, Side, TimeInForce, TimestampedEvent,
};
use crate::backtest_v2::matching::{
    price_to_ticks, ticks_to_price, FeeConfig, MatchingConfig, PriceTicks,
    SelfTradeMode as MatchingSelfTradeMode,
};
use crate::backtest_v2::oms::MarketStatus;
use crate::backtest_v2::queue::StreamSource;
use crate::backtest_v2::strategy::{OpenOrder, OrderSender, Position, StrategyCancel, StrategyOrder};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

// =============================================================================
// SELF-TRADE PREVENTION MODE
// =============================================================================

/// Self-trade prevention modes (local definition to avoid Serialize/Deserialize issues).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum SelfTradeMode {
    /// Cancel the incoming (newest) order.
    #[default]
    CancelNewest,
    /// Cancel the resting (oldest) order.
    CancelOldest,
    /// Cancel both orders.
    CancelBoth,
    /// Decrement and cancel (reduce qty, cancel if zero).
    DecrementAndCancel,
}

// =============================================================================
// CONSTANTS
// =============================================================================

/// Fixed-point scale for prices (8 decimals, matching ledger)
pub const PRICE_SCALE: i64 = 100_000_000;

/// Fixed-point scale for sizes (8 decimals)
pub const SIZE_SCALE: i64 = 100_000_000;

/// Default tick size for Polymarket binary outcome tokens
pub const POLYMARKET_TICK_SIZE: f64 = 0.01;

/// Default minimum price (1 cent)
pub const POLYMARKET_MIN_PRICE: f64 = 0.01;

/// Default maximum price (99 cents for standard markets)
pub const POLYMARKET_MAX_PRICE: f64 = 0.99;

/// Default taker fee (Polymarket: varies, typically 0-2%)
pub const DEFAULT_TAKER_FEE_BPS: i32 = 0;

/// Default maker fee (Polymarket: typically 0 or rebate)
pub const DEFAULT_MAKER_FEE_BPS: i32 = 0;

// =============================================================================
// REJECTION CODES
// =============================================================================

/// Explicit rejection codes for order validation failures.
/// These map to how the live Polymarket system would behave.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum RejectionCode {
    /// Price is not on the tick grid
    OffTickPrice { price: i64, tick_size: i64 },
    /// Price is below minimum
    PriceBelowMin { price: f64, min: f64 },
    /// Price is above maximum
    PriceAboveMax { price: f64, max: f64 },
    /// Size is below minimum
    SizeBelowMin { size: f64, min: f64 },
    /// Notional value is below minimum
    NotionalBelowMin { notional: f64, min: f64 },
    /// Unsupported time-in-force
    UnsupportedTif { tif: String },
    /// Post-only order would cross (take liquidity)
    PostOnlyWouldCross { price: f64, best_contra: f64 },
    /// Book is empty (no price discovery) for post-only
    PostOnlyNoBook,
    /// Self-trade would occur
    SelfTrade { resting_order_id: OrderId },
    /// Market is not open for trading
    MarketNotOpen { status: String },
    /// Order type not supported
    UnsupportedOrderType { order_type: String },
    /// Rate limit exceeded
    RateLimitExceeded,
    /// Duplicate client order ID
    DuplicateClientOrderId { client_order_id: String },
    /// Invalid token ID
    InvalidTokenId { token_id: String },
    /// Reduce-only would increase position
    ReduceOnlyWouldIncrease,
    /// Insufficient funds for order
    InsufficientFunds { required: f64, available: f64 },
    /// Generic validation failure
    ValidationFailed { reason: String },
}

impl RejectionCode {
    pub fn to_reject_reason(&self) -> RejectReason {
        match self {
            Self::OffTickPrice { .. } => RejectReason::InvalidPrice,
            Self::PriceBelowMin { .. } => RejectReason::InvalidPrice,
            Self::PriceAboveMax { .. } => RejectReason::InvalidPrice,
            Self::SizeBelowMin { .. } => RejectReason::InvalidSize,
            Self::NotionalBelowMin { .. } => RejectReason::InvalidSize,
            Self::UnsupportedTif { .. } => RejectReason::Unknown("Unsupported TIF".into()),
            Self::PostOnlyWouldCross { .. } => {
                RejectReason::Unknown("Post-only would cross".into())
            }
            Self::PostOnlyNoBook => RejectReason::Unknown("Post-only no price discovery".into()),
            Self::SelfTrade { .. } => RejectReason::SelfTrade,
            Self::MarketNotOpen { .. } => RejectReason::MarketHalted,
            Self::UnsupportedOrderType { .. } => {
                RejectReason::Unknown("Unsupported order type".into())
            }
            Self::RateLimitExceeded => RejectReason::RateLimited,
            Self::DuplicateClientOrderId { .. } => RejectReason::DuplicateOrderId,
            Self::InvalidTokenId { .. } => RejectReason::Unknown("Invalid token".into()),
            Self::ReduceOnlyWouldIncrease => RejectReason::Unknown("Reduce-only violated".into()),
            Self::InsufficientFunds { .. } => RejectReason::InsufficientFunds,
            Self::ValidationFailed { reason } => RejectReason::Unknown(reason.clone()),
        }
    }

    pub fn message(&self) -> String {
        match self {
            Self::OffTickPrice { price, tick_size } => {
                format!(
                    "Price {} is not on tick grid (tick_size={})",
                    from_price_fixed(*price),
                    from_price_fixed(*tick_size)
                )
            }
            Self::PriceBelowMin { price, min } => {
                format!("Price {} is below minimum {}", price, min)
            }
            Self::PriceAboveMax { price, max } => {
                format!("Price {} is above maximum {}", price, max)
            }
            Self::SizeBelowMin { size, min } => {
                format!("Size {} is below minimum {}", size, min)
            }
            Self::NotionalBelowMin { notional, min } => {
                format!("Notional {} is below minimum {}", notional, min)
            }
            Self::UnsupportedTif { tif } => {
                format!("Time-in-force '{}' is not supported", tif)
            }
            Self::PostOnlyWouldCross { price, best_contra } => {
                format!(
                    "Post-only order at {} would cross best contra at {}",
                    price, best_contra
                )
            }
            Self::PostOnlyNoBook => {
                "Post-only rejected: no price discovery (empty book)".to_string()
            }
            Self::SelfTrade { resting_order_id } => {
                format!("Order would self-trade with resting order {}", resting_order_id)
            }
            Self::MarketNotOpen { status } => {
                format!("Market is not open for trading (status: {})", status)
            }
            Self::UnsupportedOrderType { order_type } => {
                format!("Order type '{}' is not supported", order_type)
            }
            Self::RateLimitExceeded => "Rate limit exceeded".to_string(),
            Self::DuplicateClientOrderId { client_order_id } => {
                format!("Duplicate client order ID: {}", client_order_id)
            }
            Self::InvalidTokenId { token_id } => {
                format!("Invalid token ID: {}", token_id)
            }
            Self::ReduceOnlyWouldIncrease => {
                "Reduce-only order would increase position".to_string()
            }
            Self::InsufficientFunds { required, available } => {
                format!(
                    "Insufficient funds: required {}, available {}",
                    required, available
                )
            }
            Self::ValidationFailed { reason } => reason.clone(),
        }
    }
}

// =============================================================================
// FIXED-POINT HELPERS
// =============================================================================

/// Convert f64 price to fixed-point (8 decimals)
#[inline]
pub fn to_price_fixed(price: f64) -> i64 {
    (price * PRICE_SCALE as f64).round() as i64
}

/// Convert fixed-point to f64 price
#[inline]
pub fn from_price_fixed(price: i64) -> f64 {
    price as f64 / PRICE_SCALE as f64
}

/// Convert f64 size to fixed-point (8 decimals)
#[inline]
pub fn to_size_fixed(size: f64) -> i64 {
    (size * SIZE_SCALE as f64).round() as i64
}

/// Convert fixed-point to f64 size
#[inline]
pub fn from_size_fixed(size: i64) -> f64 {
    size as f64 / SIZE_SCALE as f64
}

// =============================================================================
// PRICE ROUNDING POLICY
// =============================================================================

/// Price rounding policy for order validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PriceRoundingPolicy {
    /// Reject orders with off-tick prices (HFT-grade default)
    Reject,
    /// Round to nearest tick
    RoundNearest,
    /// Round toward more conservative price (buy down, sell up)
    RoundConservative,
    /// Round toward more aggressive price (buy up, sell down)
    RoundAggressive,
}

impl Default for PriceRoundingPolicy {
    fn default() -> Self {
        Self::Reject
    }
}

impl PriceRoundingPolicy {
    /// Apply rounding policy to a price.
    /// Returns None if policy is Reject and price is off-tick.
    pub fn apply(&self, price: f64, tick_size: f64, side: Side) -> Option<f64> {
        let ticks = price / tick_size;
        let rounded_ticks = ticks.round();
        let is_on_tick = (ticks - rounded_ticks).abs() < 1e-9;

        match self {
            Self::Reject => {
                if is_on_tick {
                    Some(rounded_ticks * tick_size)
                } else {
                    None
                }
            }
            Self::RoundNearest => Some(rounded_ticks * tick_size),
            Self::RoundConservative => {
                let rounded = match side {
                    Side::Buy => ticks.floor() * tick_size, // Round down for buys
                    Side::Sell => ticks.ceil() * tick_size, // Round up for sells
                };
                Some(rounded)
            }
            Self::RoundAggressive => {
                let rounded = match side {
                    Side::Buy => ticks.ceil() * tick_size, // Round up for buys
                    Side::Sell => ticks.floor() * tick_size, // Round down for sells
                };
                Some(rounded)
            }
        }
    }
}

// =============================================================================
// POLYMARKET VENUE SPECIFICATION
// =============================================================================

/// Complete venue specification for Polymarket binary outcome markets.
/// This struct fully specifies trading constraints per market.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolymarketVenueSpec {
    // === Price Constraints ===
    /// Tick size (e.g., 0.01 for 1 cent)
    pub tick_size: f64,
    /// Minimum price (typically 0.01)
    pub price_min: f64,
    /// Maximum price (typically 0.99, or 1.00 if allowed)
    pub price_max: f64,
    /// Price rounding policy
    pub price_rounding: PriceRoundingPolicy,

    // === Size Constraints ===
    /// Minimum order size in shares
    pub min_order_size: f64,
    /// Maximum order size in shares
    pub max_order_size: f64,
    /// Size step (if applicable, 0.0 means any size >= min)
    pub size_step: f64,
    /// Minimum notional value (price * size)
    pub min_notional: f64,

    // === Fee Schedule ===
    /// Maker fee in basis points (negative = rebate)
    pub maker_fee_bps: i32,
    /// Taker fee in basis points
    pub taker_fee_bps: i32,

    // === Time-in-Force Support ===
    /// GTC (Good-til-Cancelled) allowed
    pub supports_gtc: bool,
    /// IOC (Immediate-or-Cancel) allowed
    pub supports_ioc: bool,
    /// FOK (Fill-or-Kill) allowed
    pub supports_fok: bool,
    /// Post-only orders allowed
    pub supports_post_only: bool,
    /// Reduce-only orders allowed
    pub supports_reduce_only: bool,

    // === Post-Only Behavior ===
    /// Behavior when post-only order would cross an empty book
    pub post_only_empty_book_behavior: PostOnlyEmptyBookBehavior,

    // === Self-Trade Prevention ===
    /// Self-trade prevention mode
    pub stp_mode: SelfTradeMode,

    // === Latency Configuration ===
    /// Order submission to ack delay (nanoseconds)
    pub submit_to_ack_delay_ns: Nanos,
    /// Order submission to fill delay (nanoseconds)
    pub submit_to_fill_delay_ns: Nanos,
    /// Cancel request to ack delay (nanoseconds)
    pub cancel_to_ack_delay_ns: Nanos,

    // === Market Identification ===
    /// Market ID (condition_id or similar)
    pub market_id: Option<String>,
    /// Token ID for this outcome
    pub token_id: Option<String>,
}

/// Behavior when post-only order is submitted to an empty book.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PostOnlyEmptyBookBehavior {
    /// Reject the order (default - no price discovery)
    Reject,
    /// Accept and park as a resting order
    Accept,
}

impl Default for PostOnlyEmptyBookBehavior {
    fn default() -> Self {
        Self::Reject
    }
}

impl Default for PolymarketVenueSpec {
    fn default() -> Self {
        Self::polymarket_binary()
    }
}

impl PolymarketVenueSpec {
    /// Standard Polymarket binary outcome token configuration.
    pub fn polymarket_binary() -> Self {
        Self {
            tick_size: POLYMARKET_TICK_SIZE,
            price_min: POLYMARKET_MIN_PRICE,
            price_max: POLYMARKET_MAX_PRICE,
            price_rounding: PriceRoundingPolicy::Reject,

            min_order_size: 1.0,
            max_order_size: 100_000.0,
            size_step: 0.0, // Any size >= min
            min_notional: 0.0, // No minimum notional

            maker_fee_bps: DEFAULT_MAKER_FEE_BPS,
            taker_fee_bps: DEFAULT_TAKER_FEE_BPS,

            supports_gtc: true,
            supports_ioc: true,
            supports_fok: true,
            supports_post_only: true,
            supports_reduce_only: false,

            post_only_empty_book_behavior: PostOnlyEmptyBookBehavior::Reject,

            stp_mode: SelfTradeMode::CancelNewest,

            submit_to_ack_delay_ns: 0,
            submit_to_fill_delay_ns: 0,
            cancel_to_ack_delay_ns: 0,

            market_id: None,
            token_id: None,
        }
    }

    /// Configuration with realistic latencies for simulation.
    pub fn with_realistic_latency() -> Self {
        let mut spec = Self::polymarket_binary();
        spec.submit_to_ack_delay_ns = 10_000_000; // 10ms
        spec.submit_to_fill_delay_ns = 15_000_000; // 15ms
        spec.cancel_to_ack_delay_ns = 10_000_000; // 10ms
        spec
    }

    /// Strict mode for HFT testing (reject off-tick, minimal fees).
    pub fn strict_hft() -> Self {
        let mut spec = Self::polymarket_binary();
        spec.price_rounding = PriceRoundingPolicy::Reject;
        spec.min_order_size = 10.0; // Higher minimum for HFT
        spec.min_notional = 1.0; // At least $1 notional
        spec
    }

    /// Create from a config map (for runtime configuration).
    pub fn from_config(config: &HashMap<String, String>) -> Self {
        let mut spec = Self::polymarket_binary();

        if let Some(v) = config.get("tick_size") {
            spec.tick_size = v.parse().unwrap_or(spec.tick_size);
        }
        if let Some(v) = config.get("price_min") {
            spec.price_min = v.parse().unwrap_or(spec.price_min);
        }
        if let Some(v) = config.get("price_max") {
            spec.price_max = v.parse().unwrap_or(spec.price_max);
        }
        if let Some(v) = config.get("min_order_size") {
            spec.min_order_size = v.parse().unwrap_or(spec.min_order_size);
        }
        if let Some(v) = config.get("max_order_size") {
            spec.max_order_size = v.parse().unwrap_or(spec.max_order_size);
        }
        if let Some(v) = config.get("min_notional") {
            spec.min_notional = v.parse().unwrap_or(spec.min_notional);
        }
        if let Some(v) = config.get("maker_fee_bps") {
            spec.maker_fee_bps = v.parse().unwrap_or(spec.maker_fee_bps);
        }
        if let Some(v) = config.get("taker_fee_bps") {
            spec.taker_fee_bps = v.parse().unwrap_or(spec.taker_fee_bps);
        }
        if let Some(v) = config.get("submit_to_ack_delay_ns") {
            spec.submit_to_ack_delay_ns = v.parse().unwrap_or(spec.submit_to_ack_delay_ns);
        }
        if let Some(v) = config.get("submit_to_fill_delay_ns") {
            spec.submit_to_fill_delay_ns = v.parse().unwrap_or(spec.submit_to_fill_delay_ns);
        }
        if let Some(v) = config.get("cancel_to_ack_delay_ns") {
            spec.cancel_to_ack_delay_ns = v.parse().unwrap_or(spec.cancel_to_ack_delay_ns);
        }

        spec
    }

    /// Convert to matching engine config.
    pub fn to_matching_config(&self) -> MatchingConfig {
        let stp_mode = match self.stp_mode {
            SelfTradeMode::CancelNewest => MatchingSelfTradeMode::CancelNewest,
            SelfTradeMode::CancelOldest => MatchingSelfTradeMode::CancelOldest,
            SelfTradeMode::CancelBoth => MatchingSelfTradeMode::CancelBoth,
            SelfTradeMode::DecrementAndCancel => MatchingSelfTradeMode::DecrementAndCancel,
        };
        MatchingConfig {
            tick_size: self.tick_size,
            fees: FeeConfig {
                maker_fee_rate: self.maker_fee_bps as f64 / 10_000.0,
                taker_fee_rate: self.taker_fee_bps as f64 / 10_000.0,
            },
            self_trade_prevention: true,
            stp_mode,
            min_order_size: self.min_order_size,
            max_order_size: self.max_order_size,
            ack_latency_ns: self.submit_to_ack_delay_ns,
        }
    }

    /// Validate a price against this spec.
    pub fn validate_price(&self, price: f64, side: Side) -> Result<f64, RejectionCode> {
        // Check bounds first
        if price < self.price_min {
            return Err(RejectionCode::PriceBelowMin {
                price,
                min: self.price_min,
            });
        }
        if price > self.price_max {
            return Err(RejectionCode::PriceAboveMax {
                price,
                max: self.price_max,
            });
        }

        // Apply rounding policy
        match self.price_rounding.apply(price, self.tick_size, side) {
            Some(rounded) => Ok(rounded),
            None => Err(RejectionCode::OffTickPrice {
                price: to_price_fixed(price),
                tick_size: to_price_fixed(self.tick_size),
            }),
        }
    }

    /// Validate order size against this spec.
    pub fn validate_size(&self, size: f64, price: f64) -> Result<(), RejectionCode> {
        if size < self.min_order_size {
            return Err(RejectionCode::SizeBelowMin {
                size,
                min: self.min_order_size,
            });
        }
        if size > self.max_order_size {
            return Err(RejectionCode::SizeBelowMin {
                size,
                min: self.max_order_size,
            });
        }

        // Check notional
        let notional = price * size;
        if notional < self.min_notional {
            return Err(RejectionCode::NotionalBelowMin {
                notional,
                min: self.min_notional,
            });
        }

        Ok(())
    }

    /// Validate time-in-force against this spec.
    pub fn validate_tif(&self, tif: TimeInForce) -> Result<(), RejectionCode> {
        let supported = match tif {
            TimeInForce::Gtc => self.supports_gtc,
            TimeInForce::Ioc => self.supports_ioc,
            TimeInForce::Fok => self.supports_fok,
            TimeInForce::Gtt { .. } => self.supports_gtc, // Treat GTT as GTC variant
        };

        if supported {
            Ok(())
        } else {
            Err(RejectionCode::UnsupportedTif {
                tif: format!("{:?}", tif),
            })
        }
    }

    /// Calculate fee for a fill.
    pub fn calculate_fee(&self, price: f64, size: f64, is_maker: bool) -> f64 {
        let notional = price * size;
        let fee_bps = if is_maker {
            self.maker_fee_bps
        } else {
            self.taker_fee_bps
        };
        notional * (fee_bps as f64 / 10_000.0)
    }

}

// =============================================================================
// ORDER VALIDATION RESULT
// =============================================================================

/// Result of order validation.
#[derive(Debug, Clone)]
pub struct ValidationResult {
    /// Whether the order passed validation
    pub valid: bool,
    /// Rejection code if validation failed
    pub rejection: Option<RejectionCode>,
    /// Validated/rounded price (may differ from submitted if rounding applied)
    pub validated_price: Option<f64>,
    /// Warnings (non-fatal issues)
    pub warnings: Vec<String>,
}

impl ValidationResult {
    pub fn ok(price: f64) -> Self {
        Self {
            valid: true,
            rejection: None,
            validated_price: Some(price),
            warnings: Vec::new(),
        }
    }

    pub fn reject(code: RejectionCode) -> Self {
        Self {
            valid: false,
            rejection: Some(code),
            validated_price: None,
            warnings: Vec::new(),
        }
    }
}

// =============================================================================
// EXECUTION ADAPTER STATISTICS
// =============================================================================

/// Statistics for the execution adapter.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExecutionAdapterStats {
    /// Total orders submitted
    pub orders_submitted: u64,
    /// Orders accepted (passed validation)
    pub orders_accepted: u64,
    /// Orders rejected at validation
    pub orders_rejected: u64,
    /// Orders fully filled
    pub orders_filled: u64,
    /// Orders partially filled
    pub orders_partial_filled: u64,
    /// Orders cancelled
    pub orders_cancelled: u64,
    /// Total fills generated
    pub fills_generated: u64,
    /// Total volume traded (notional)
    pub volume_notional: f64,
    /// Total fees paid (positive = paid, negative = rebate)
    pub fees_paid: f64,
    /// Post-only rejections
    pub post_only_rejections: u64,
    /// Off-tick rejections
    pub off_tick_rejections: u64,
    /// Self-trade preventions
    pub self_trade_preventions: u64,
    /// IOC cancellations (unfilled remainder)
    pub ioc_cancellations: u64,
}

// =============================================================================
// PENDING EXECUTION EVENT
// =============================================================================

/// An execution event pending delivery at a scheduled time.
#[derive(Debug, Clone)]
pub struct PendingExecutionEvent {
    /// Scheduled delivery time (visible timestamp)
    pub delivery_time: Nanos,
    /// The event to deliver
    pub event: ExecutionEvent,
    /// Sequence number for deterministic ordering at same timestamp
    pub seq: u64,
}

impl PartialEq for PendingExecutionEvent {
    fn eq(&self, other: &Self) -> bool {
        self.delivery_time == other.delivery_time && self.seq == other.seq
    }
}

impl Eq for PendingExecutionEvent {}

impl PartialOrd for PendingExecutionEvent {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for PendingExecutionEvent {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        match self.delivery_time.cmp(&other.delivery_time) {
            std::cmp::Ordering::Equal => self.seq.cmp(&other.seq),
            ord => ord,
        }
    }
}

/// Execution events that can be delivered to strategies.
#[derive(Debug, Clone)]
pub enum ExecutionEvent {
    /// Order acknowledged
    Ack {
        order_id: OrderId,
        client_order_id: Option<String>,
    },
    /// Order rejected
    Reject {
        order_id: OrderId,
        client_order_id: Option<String>,
        code: RejectionCode,
    },
    /// Fill notification
    Fill {
        order_id: OrderId,
        client_order_id: Option<String>,
        price: f64,
        size: f64,
        is_maker: bool,
        leaves_qty: f64,
        fee: f64,
        fill_id: u64,
    },
    /// Cancel acknowledged
    CancelAck {
        order_id: OrderId,
        cancelled_qty: f64,
    },
}

// =============================================================================
// INTERNAL ORDER STATE
// =============================================================================

/// Internal order tracking for the execution adapter.
#[derive(Debug, Clone)]
struct InternalOrder {
    order_id: OrderId,
    client_order_id: String,
    token_id: String,
    side: Side,
    price: f64,
    original_size: f64,
    remaining_size: f64,
    filled_size: f64,
    order_type: OrderType,
    time_in_force: TimeInForce,
    post_only: bool,
    reduce_only: bool,
    trader_id: String,
    created_at: Nanos,
    state: InternalOrderState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InternalOrderState {
    PendingAck,
    Live,
    PartiallyFilled,
    PendingCancel,
    Done,
}

// =============================================================================
// POLYMARKET BACKTEST EXECUTION ADAPTER
// =============================================================================

/// Polymarket-compliant backtest execution adapter.
///
/// This adapter enforces real Polymarket CLOB constraints and produces
/// deterministic execution outcomes for HFT-grade backtesting.
pub struct PolymarketBacktestExecutionAdapter {
    /// Venue specification
    spec: PolymarketVenueSpec,
    /// Current simulation time
    current_time: Nanos,
    /// Trader ID for self-trade prevention
    trader_id: String,
    /// Quote books for liquidity tracking (one per token)
    quote_books: HashMap<String, SimpleQuoteBook>,
    /// Internal order tracking
    orders: HashMap<OrderId, InternalOrder>,
    /// Client order ID to order ID mapping
    client_order_ids: HashMap<String, OrderId>,
    /// Position tracking (for reduce-only validation)
    positions: HashMap<String, f64>,
    /// Pending events queue (sorted by delivery time)
    pending_events: BTreeMap<(Nanos, u64), PendingExecutionEvent>,
    /// Next order ID
    next_order_id: OrderId,
    /// Next fill ID
    next_fill_id: u64,
    /// Next event sequence
    next_event_seq: u64,
    /// Statistics
    stats: ExecutionAdapterStats,
    /// Market status per token
    market_status: HashMap<String, MarketStatus>,
    /// Fill records (for ledger integration - drain after processing)
    fill_records: Vec<FillRecord>,
}

/// Fill record for ledger integration.
#[derive(Debug, Clone)]
pub struct FillRecord {
    pub fill_id: u64,
    pub order_id: OrderId,
    pub client_order_id: String,
    pub token_id: String,
    pub market_id: Option<String>,
    pub side: Side,
    pub price: f64,
    pub size: f64,
    pub is_maker: bool,
    pub fee: f64,
    pub leaves_qty: f64,
    pub timestamp: Nanos,
}

/// Open order information returned by `open_orders()`.
#[derive(Debug, Clone)]
pub struct OpenOrderInfo {
    pub order_id: OrderId,
    pub client_order_id: String,
    pub token_id: String,
    pub side: Side,
    pub price: f64,
    pub original_size: f64,
    pub remaining_size: f64,
    pub filled_size: f64,
    pub created_at: Nanos,
}

impl PolymarketBacktestExecutionAdapter {
    /// Create a new execution adapter with default Polymarket spec.
    pub fn new(trader_id: impl Into<String>) -> Self {
        Self::with_spec(PolymarketVenueSpec::polymarket_binary(), trader_id)
    }

    /// Create with custom venue specification.
    pub fn with_spec(spec: PolymarketVenueSpec, trader_id: impl Into<String>) -> Self {
        Self {
            spec,
            current_time: 0,
            trader_id: trader_id.into(),
            quote_books: HashMap::new(),
            orders: HashMap::new(),
            client_order_ids: HashMap::new(),
            positions: HashMap::new(),
            pending_events: BTreeMap::new(),
            next_order_id: 1,
            next_fill_id: 1,
            next_event_seq: 1,
            stats: ExecutionAdapterStats::default(),
            market_status: HashMap::new(),
            fill_records: Vec::new(),
        }
    }

    /// Drain fill records (for ledger integration).
    /// Returns all accumulated fill records and clears the internal buffer.
    pub fn drain_fill_records(&mut self) -> Vec<FillRecord> {
        std::mem::take(&mut self.fill_records)
    }

    /// Get fill records without draining.
    pub fn fill_records(&self) -> &[FillRecord] {
        &self.fill_records
    }

    /// Get venue specification.
    pub fn spec(&self) -> &PolymarketVenueSpec {
        &self.spec
    }

    /// Get mutable venue specification.
    pub fn spec_mut(&mut self) -> &mut PolymarketVenueSpec {
        &mut self.spec
    }

    /// Get statistics.
    pub fn stats(&self) -> &ExecutionAdapterStats {
        &self.stats
    }

    /// Set current simulation time.
    pub fn set_time(&mut self, time: Nanos) {
        self.current_time = time;
    }

    /// Get current simulation time.
    pub fn current_time(&self) -> Nanos {
        self.current_time
    }

    /// Set market status for a token.
    pub fn set_market_status(&mut self, token_id: &str, status: MarketStatus) {
        self.market_status.insert(token_id.to_string(), status);
    }

    /// Get market status for a token.
    pub fn get_market_status(&self, token_id: &str) -> MarketStatus {
        self.market_status
            .get(token_id)
            .copied()
            .unwrap_or(MarketStatus::Open)
    }

    /// Update position (for reduce-only validation).
    pub fn update_position(&mut self, token_id: &str, delta: f64) {
        let pos = self.positions.entry(token_id.to_string()).or_insert(0.0);
        *pos += delta;
    }

    /// Get position for a token.
    pub fn get_position(&self, token_id: &str) -> f64 {
        self.positions.get(token_id).copied().unwrap_or(0.0)
    }

    /// Apply an L2 book snapshot.
    pub fn apply_book_snapshot(
        &mut self,
        token_id: &str,
        bids: &[Level],
        asks: &[Level],
        _exchange_seq: u64,
    ) {
        let book = self.get_or_create_book(token_id);

        // Convert to internal format and apply
        for bid in bids {
            book.apply_update(Side::Buy, bid.price, bid.size);
        }
        for ask in asks {
            book.apply_update(Side::Sell, ask.price, ask.size);
        }
    }

    /// Apply an L2 book delta.
    pub fn apply_book_delta(
        &mut self,
        token_id: &str,
        side: Side,
        price: f64,
        new_size: f64,
    ) {
        let book = self.get_or_create_book(token_id);
        book.apply_update(side, price, new_size);
    }

    /// Get best bid for a token.
    pub fn best_bid(&self, token_id: &str) -> Option<(f64, f64)> {
        self.quote_books
            .get(token_id)
            .and_then(|b| b.best_bid())
    }

    /// Get best ask for a token.
    pub fn best_ask(&self, token_id: &str) -> Option<(f64, f64)> {
        self.quote_books
            .get(token_id)
            .and_then(|b| b.best_ask())
    }

    /// Get open orders, optionally filtered by token.
    pub fn open_orders(&self, token_id: Option<&str>) -> Vec<OpenOrderInfo> {
        self.orders
            .values()
            .filter(|o| {
                // Must be in an active state
                let is_active = matches!(
                    o.state,
                    InternalOrderState::Live | InternalOrderState::PartiallyFilled
                );
                if !is_active {
                    return false;
                }
                // Filter by token if specified
                if let Some(tid) = token_id {
                    o.token_id == tid
                } else {
                    true
                }
            })
            .map(|o| OpenOrderInfo {
                order_id: o.order_id,
                client_order_id: o.client_order_id.clone(),
                token_id: o.token_id.clone(),
                side: o.side,
                price: o.price,
                original_size: o.original_size,
                remaining_size: o.remaining_size,
                filled_size: o.filled_size,
                created_at: o.created_at,
            })
            .collect()
    }

    /// Drain pending events up to the given time.
    /// Returns events in deterministic order.
    pub fn drain_events_until(&mut self, until_time: Nanos) -> Vec<ExecutionEvent> {
        let mut events = Vec::new();

        // Collect keys to remove
        let keys_to_drain: Vec<_> = self
            .pending_events
            .range(..=(until_time, u64::MAX))
            .map(|(k, _)| *k)
            .collect();

        for key in keys_to_drain {
            if let Some(pending) = self.pending_events.remove(&key) {
                events.push(pending.event);
            }
        }

        events
    }

    /// Validate an order against venue spec.
    pub fn validate_order(&self, order: &StrategyOrder) -> ValidationResult {
        // Check market status
        let status = self.get_market_status(&order.token_id);
        if status != MarketStatus::Open {
            return ValidationResult::reject(RejectionCode::MarketNotOpen {
                status: format!("{:?}", status),
            });
        }

        // Check for duplicate client order ID
        if self.client_order_ids.contains_key(&order.client_order_id) {
            return ValidationResult::reject(RejectionCode::DuplicateClientOrderId {
                client_order_id: order.client_order_id.clone(),
            });
        }

        // Validate price (with rounding policy)
        let validated_price = match self.spec.validate_price(order.price, order.side) {
            Ok(p) => p,
            Err(code) => {
                if matches!(code, RejectionCode::OffTickPrice { .. }) {
                    self.stats.off_tick_rejections;
                }
                return ValidationResult::reject(code);
            }
        };

        // Validate size
        if let Err(code) = self.spec.validate_size(order.size, validated_price) {
            return ValidationResult::reject(code);
        }

        // Validate time-in-force
        if let Err(code) = self.spec.validate_tif(order.time_in_force) {
            return ValidationResult::reject(code);
        }

        // Validate post-only
        if order.post_only && !self.spec.supports_post_only {
            return ValidationResult::reject(RejectionCode::UnsupportedTif {
                tif: "post_only".to_string(),
            });
        }

        // Validate reduce-only
        if order.reduce_only {
            if !self.spec.supports_reduce_only {
                return ValidationResult::reject(RejectionCode::UnsupportedTif {
                    tif: "reduce_only".to_string(),
                });
            }

            // Check if order would reduce position
            let current_pos = self.get_position(&order.token_id);
            let would_increase = match order.side {
                Side::Buy => current_pos >= 0.0,
                Side::Sell => current_pos <= 0.0,
            };

            if would_increase {
                return ValidationResult::reject(RejectionCode::ReduceOnlyWouldIncrease);
            }
        }

        // Check post-only would cross
        if order.post_only {
            if let Some(best_contra) = self.get_best_contra_price(&order.token_id, order.side) {
                let would_cross = match order.side {
                    Side::Buy => validated_price >= best_contra,
                    Side::Sell => validated_price <= best_contra,
                };

                if would_cross {
                    return ValidationResult::reject(RejectionCode::PostOnlyWouldCross {
                        price: validated_price,
                        best_contra,
                    });
                }
            } else {
                // Empty book case
                match self.spec.post_only_empty_book_behavior {
                    PostOnlyEmptyBookBehavior::Reject => {
                        return ValidationResult::reject(RejectionCode::PostOnlyNoBook);
                    }
                    PostOnlyEmptyBookBehavior::Accept => {
                        // Allow through
                    }
                }
            }
        }

        ValidationResult::ok(validated_price)
    }

    /// Submit an order.
    pub fn submit_order(&mut self, order: StrategyOrder) -> Result<OrderId, RejectionCode> {
        self.stats.orders_submitted += 1;

        // Validate
        let validation = self.validate_order(&order);
        if !validation.valid {
            self.stats.orders_rejected += 1;
            if let Some(ref code) = validation.rejection {
                if matches!(code, RejectionCode::PostOnlyWouldCross { .. }) {
                    self.stats.post_only_rejections += 1;
                }
            }
            return Err(validation.rejection.unwrap());
        }

        let validated_price = validation.validated_price.unwrap();
        let order_id = self.next_order_id;
        self.next_order_id += 1;

        // Create internal order
        let internal_order = InternalOrder {
            order_id,
            client_order_id: order.client_order_id.clone(),
            token_id: order.token_id.clone(),
            side: order.side,
            price: validated_price,
            original_size: order.size,
            remaining_size: order.size,
            filled_size: 0.0,
            order_type: order.order_type,
            time_in_force: order.time_in_force,
            post_only: order.post_only,
            reduce_only: order.reduce_only,
            trader_id: self.trader_id.clone(),
            created_at: self.current_time,
            state: InternalOrderState::PendingAck,
        };

        self.orders.insert(order_id, internal_order);
        self.client_order_ids
            .insert(order.client_order_id.clone(), order_id);
        self.stats.orders_accepted += 1;

        // Schedule ack event
        let ack_time = self.current_time + self.spec.submit_to_ack_delay_ns;
        self.schedule_event(
            ack_time,
            ExecutionEvent::Ack {
                order_id,
                client_order_id: Some(order.client_order_id.clone()),
            },
        );

        // Attempt matching
        self.try_match_order(order_id);

        Ok(order_id)
    }

    /// Cancel an order.
    pub fn cancel_order(&mut self, cancel: StrategyCancel) -> Result<(), RejectionCode> {
        let order = match self.orders.get_mut(&cancel.order_id) {
            Some(o) => o,
            None => {
                return Err(RejectionCode::ValidationFailed {
                    reason: "Order not found".to_string(),
                });
            }
        };

        // Check if order can be cancelled
        match order.state {
            InternalOrderState::Live | InternalOrderState::PartiallyFilled => {
                order.state = InternalOrderState::PendingCancel;
            }
            InternalOrderState::PendingAck => {
                // Can't cancel until acked
                return Err(RejectionCode::ValidationFailed {
                    reason: "Cannot cancel order pending ack".to_string(),
                });
            }
            _ => {
                return Err(RejectionCode::ValidationFailed {
                    reason: "Order not active".to_string(),
                });
            }
        }

        // Schedule cancel ack
        let cancel_time = self.current_time + self.spec.cancel_to_ack_delay_ns;
        let cancelled_qty = order.remaining_size;

        self.schedule_event(
            cancel_time,
            ExecutionEvent::CancelAck {
                order_id: cancel.order_id,
                cancelled_qty,
            },
        );

        // Finalize order
        let order = self.orders.get_mut(&cancel.order_id).unwrap();
        order.state = InternalOrderState::Done;
        order.remaining_size = 0.0;

        // Note: We don't need to remove from external matching engine since we
        // track resting orders internally in this simplified implementation.

        self.stats.orders_cancelled += 1;

        Ok(())
    }

    /// Convert to events for the backtest event queue.
    pub fn to_timestamped_events(&self) -> Vec<TimestampedEvent> {
        let mut events = Vec::new();

        for pending in self.pending_events.values() {
            let event = match &pending.event {
                ExecutionEvent::Ack {
                    order_id,
                    client_order_id,
                } => Event::OrderAck {
                    order_id: *order_id,
                    client_order_id: client_order_id.clone(),
                    exchange_time: pending.delivery_time,
                },
                ExecutionEvent::Reject {
                    order_id,
                    client_order_id,
                    code,
                } => Event::OrderReject {
                    order_id: *order_id,
                    client_order_id: client_order_id.clone(),
                    reason: code.to_reject_reason(),
                },
                ExecutionEvent::Fill {
                    order_id,
                    price,
                    size,
                    is_maker,
                    leaves_qty,
                    fee,
                    fill_id,
                    ..
                } => Event::Fill {
                    order_id: *order_id,
                    price: *price,
                    size: *size,
                    is_maker: *is_maker,
                    leaves_qty: *leaves_qty,
                    fee: *fee,
                    fill_id: Some(format!("fill_{}", fill_id)),
                },
                ExecutionEvent::CancelAck {
                    order_id,
                    cancelled_qty,
                } => Event::CancelAck {
                    order_id: *order_id,
                    cancelled_qty: *cancelled_qty,
                },
            };

            events.push(TimestampedEvent::new(
                pending.delivery_time,
                StreamSource::OrderManagement as u8,
                event,
            ));
        }

        events
    }

    // === Private Methods ===

    fn get_or_create_book(&mut self, token_id: &str) -> &mut SimpleQuoteBook {
        if !self.quote_books.contains_key(token_id) {
            let book = SimpleQuoteBook::new(self.spec.tick_size);
            self.quote_books.insert(token_id.to_string(), book);
        }
        self.quote_books.get_mut(token_id).unwrap()
    }

    fn get_best_contra_price(&self, token_id: &str, side: Side) -> Option<f64> {
        let book = self.quote_books.get(token_id)?;
        match side {
            Side::Buy => book.best_ask().map(|(p, _)| p),
            Side::Sell => book.best_bid().map(|(p, _)| p),
        }
    }

    fn schedule_event(&mut self, time: Nanos, event: ExecutionEvent) {
        let seq = self.next_event_seq;
        self.next_event_seq += 1;

        let pending = PendingExecutionEvent {
            delivery_time: time,
            event,
            seq,
        };

        self.pending_events.insert((time, seq), pending);
    }

    fn try_match_order(&mut self, order_id: OrderId) {
        let order = match self.orders.get(&order_id) {
            Some(o) => o.clone(),
            None => return,
        };

        // Post-only orders don't match immediately
        if order.post_only {
            // Add to book as resting order
            self.add_resting_order(order_id);
            return;
        }

        // Try to match against available liquidity
        let book = self.get_or_create_book(&order.token_id);

        let available = match order.side {
            Side::Buy => book.best_ask(),
            Side::Sell => book.best_bid(),
        };

        if let Some((best_price, available_size)) = available {
            // Check if order would cross
            let would_cross = match order.side {
                Side::Buy => order.price >= best_price,
                Side::Sell => order.price <= best_price,
            };

            if would_cross {
                self.execute_taker_match(order_id, best_price, available_size);
            } else {
                // Order doesn't cross - add to book or cancel based on TIF
                match order.time_in_force {
                    TimeInForce::Gtc | TimeInForce::Gtt { .. } => {
                        self.add_resting_order(order_id);
                    }
                    TimeInForce::Ioc | TimeInForce::Fok => {
                        // Cancel the order
                        self.cancel_unfilled_ioc(order_id);
                    }
                }
            }
        } else {
            // Empty book
            match order.time_in_force {
                TimeInForce::Gtc | TimeInForce::Gtt { .. } => {
                    self.add_resting_order(order_id);
                }
                TimeInForce::Ioc | TimeInForce::Fok => {
                    // Cancel the order
                    self.cancel_unfilled_ioc(order_id);
                }
            }
        }
    }

    fn execute_taker_match(&mut self, order_id: OrderId, match_price: f64, available_size: f64) {
        // Extract order data we need before mutating
        let (fill_size, fee, client_order_id, token_id, side, remaining_after, tif) = {
            let order = match self.orders.get(&order_id) {
                Some(o) => o,
                None => return,
            };
            let fill_size = order.remaining_size.min(available_size);
            let is_maker = false;
            let fee = self.spec.calculate_fee(match_price, fill_size, is_maker);
            let remaining_after = order.remaining_size - fill_size;
            (
                fill_size,
                fee,
                order.client_order_id.clone(),
                order.token_id.clone(),
                order.side,
                remaining_after,
                order.time_in_force,
            )
        };

        // Now mutate the order
        if let Some(order) = self.orders.get_mut(&order_id) {
            order.filled_size += fill_size;
            order.remaining_size = remaining_after;
        }

        let is_maker = false;
        let fill_time = self.current_time + self.spec.submit_to_fill_delay_ns;
        let fill_id = self.next_fill_id;
        self.next_fill_id += 1;

        // Schedule fill event
        let seq = self.next_event_seq;
        self.next_event_seq += 1;
        let pending = PendingExecutionEvent {
            delivery_time: fill_time,
            event: ExecutionEvent::Fill {
                order_id,
                client_order_id: Some(client_order_id.clone()),
                price: match_price,
                size: fill_size,
                is_maker,
                leaves_qty: remaining_after,
                fee,
                fill_id,
            },
            seq,
        };
        self.pending_events.insert((fill_time, seq), pending);

        // Update statistics
        self.stats.fills_generated += 1;
        self.stats.volume_notional += match_price * fill_size;
        self.stats.fees_paid += fee;

        // Update position
        let position_delta = match side {
            Side::Buy => fill_size,
            Side::Sell => -fill_size,
        };
        let pos = self.positions.entry(token_id.clone()).or_insert(0.0);
        *pos += position_delta;

        // Store fill record for ledger integration
        let fill_record = FillRecord {
            fill_id,
            order_id,
            client_order_id,
            token_id: token_id.clone(),
            market_id: self.spec.market_id.clone(),
            side,
            price: match_price,
            size: fill_size,
            is_maker,
            fee,
            leaves_qty: remaining_after,
            timestamp: fill_time,
        };
        self.fill_records.push(fill_record);

        // Update book (remove consumed liquidity)
        if let Some(book) = self.quote_books.get_mut(&token_id) {
            let contra_side = side.opposite();
            let new_size = (available_size - fill_size).max(0.0);
            book.apply_update(contra_side, match_price, new_size);
        }

        // Handle remaining quantity
        if remaining_after > 0.0 {
            if let Some(order) = self.orders.get_mut(&order_id) {
                order.state = InternalOrderState::PartiallyFilled;
            }
            self.stats.orders_partial_filled += 1;

            match tif {
                TimeInForce::Ioc | TimeInForce::Fok => {
                    // Cancel remainder
                    self.cancel_unfilled_ioc(order_id);
                }
                TimeInForce::Gtc | TimeInForce::Gtt { .. } => {
                    // Add remainder to book
                    self.add_resting_order(order_id);
                }
            }
        } else {
            if let Some(order) = self.orders.get_mut(&order_id) {
                order.state = InternalOrderState::Done;
            }
            self.stats.orders_filled += 1;
        }
    }

    fn add_resting_order(&mut self, order_id: OrderId) {
        // Mark order as live (resting on book)
        // Note: For a full implementation, we'd add to a resting order queue
        // and match incoming market data against it. For this simplified version,
        // resting orders are tracked but only fill against IOC/market orders.
        if let Some(order) = self.orders.get_mut(&order_id) {
            order.state = InternalOrderState::Live;
        }
    }

    fn cancel_unfilled_ioc(&mut self, order_id: OrderId) {
        let order = match self.orders.get_mut(&order_id) {
            Some(o) => o,
            None => return,
        };

        let cancelled_qty = order.remaining_size;
        order.remaining_size = 0.0;
        order.state = InternalOrderState::Done;

        // Schedule cancel ack
        let cancel_time = self.current_time + self.spec.cancel_to_ack_delay_ns;
        self.schedule_event(
            cancel_time,
            ExecutionEvent::CancelAck {
                order_id,
                cancelled_qty,
            },
        );

        self.stats.ioc_cancellations += 1;
    }
}

// =============================================================================
// ORDERSENDER IMPLEMENTATION
// =============================================================================

impl OrderSender for PolymarketBacktestExecutionAdapter {
    fn send_order(&mut self, order: StrategyOrder) -> Result<OrderId, String> {
        self.submit_order(order)
            .map_err(|code| code.message())
    }

    fn send_cancel(&mut self, cancel: StrategyCancel) -> Result<(), String> {
        self.cancel_order(cancel)
            .map_err(|code| code.message())
    }

    fn cancel_all(&mut self, token_id: &str) -> Result<usize, String> {
        let orders_to_cancel: Vec<OrderId> = self
            .orders
            .iter()
            .filter(|(_, o)| {
                o.token_id == token_id
                    && matches!(
                        o.state,
                        InternalOrderState::Live | InternalOrderState::PartiallyFilled
                    )
            })
            .map(|(id, _)| *id)
            .collect();

        let mut cancelled_count = 0;
        for order_id in orders_to_cancel {
            if self
                .cancel_order(StrategyCancel {
                    order_id,
                    client_order_id: None,
                })
                .is_ok()
            {
                cancelled_count += 1;
            }
        }

        Ok(cancelled_count)
    }

    fn get_position(&self, token_id: &str) -> Position {
        let shares = self.positions.get(token_id).copied().unwrap_or(0.0);
        Position {
            token_id: token_id.to_string(),
            shares,
            cost_basis: 0.0, // Not tracked here - use ledger
            realized_pnl: 0.0,
            unrealized_pnl: 0.0,
        }
    }

    fn get_all_positions(&self) -> HashMap<String, Position> {
        self.positions
            .iter()
            .map(|(token_id, &shares)| {
                (
                    token_id.clone(),
                    Position {
                        token_id: token_id.clone(),
                        shares,
                        cost_basis: 0.0,
                        realized_pnl: 0.0,
                        unrealized_pnl: 0.0,
                    },
                )
            })
            .collect()
    }

    fn get_open_orders(&self) -> Vec<OpenOrder> {
        self.orders
            .values()
            .filter(|o| {
                matches!(
                    o.state,
                    InternalOrderState::Live | InternalOrderState::PartiallyFilled
                )
            })
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

    fn schedule_timer(&mut self, _delay_ns: Nanos, _payload: Option<String>) -> u64 {
        // Not implemented in this adapter - use orchestrator timers
        0
    }

    fn cancel_timer(&mut self, _timer_id: u64) -> bool {
        false
    }
}

// =============================================================================
// SIMPLE QUOTE BOOK FOR LIQUIDITY TRACKING
// =============================================================================

/// Simple order book for tracking quote liquidity.
/// This is separate from the matching engine's book and is used purely
/// for determining available liquidity at submission time.
#[derive(Debug, Clone, Default)]
struct SimpleQuoteBook {
    /// Bids by price ticks (descending order for best bid = highest)
    bids: BTreeMap<PriceTicks, f64>,
    /// Asks by price ticks (ascending order for best ask = lowest)
    asks: BTreeMap<PriceTicks, f64>,
    /// Tick size
    tick_size: f64,
}

impl SimpleQuoteBook {
    fn new(tick_size: f64) -> Self {
        Self {
            bids: BTreeMap::new(),
            asks: BTreeMap::new(),
            tick_size,
        }
    }

    /// Apply a quote update (set size at price level).
    fn apply_update(&mut self, side: Side, price: f64, new_size: f64) {
        let price_ticks = price_to_ticks(price, self.tick_size);
        let book = match side {
            Side::Buy => &mut self.bids,
            Side::Sell => &mut self.asks,
        };

        if new_size <= 0.0 {
            book.remove(&price_ticks);
        } else {
            book.insert(price_ticks, new_size);
        }
    }

    /// Get best bid (price, size).
    fn best_bid(&self) -> Option<(f64, f64)> {
        self.bids.iter().next_back().map(|(&ticks, &size)| {
            (ticks_to_price(ticks, self.tick_size), size)
        })
    }

    /// Get best ask (price, size).
    fn best_ask(&self) -> Option<(f64, f64)> {
        self.asks.iter().next().map(|(&ticks, &size)| {
            (ticks_to_price(ticks, self.tick_size), size)
        })
    }
}

// =============================================================================
// TESTS
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_order(
        token_id: &str,
        side: Side,
        price: f64,
        size: f64,
        tif: TimeInForce,
    ) -> StrategyOrder {
        StrategyOrder {
            client_order_id: format!("test_{}", rand::random::<u32>()),
            token_id: token_id.to_string(),
            side,
            price,
            size,
            order_type: OrderType::Limit,
            time_in_force: tif,
            post_only: false,
            reduce_only: false,
        }
    }

    #[test]
    fn test_venue_spec_defaults() {
        let spec = PolymarketVenueSpec::polymarket_binary();
        assert_eq!(spec.tick_size, 0.01);
        assert_eq!(spec.price_min, 0.01);
        assert_eq!(spec.price_max, 0.99);
        assert_eq!(spec.min_order_size, 1.0);
        assert!(spec.supports_gtc);
        assert!(spec.supports_ioc);
        assert!(spec.supports_post_only);
    }

    #[test]
    fn test_price_validation_on_tick() {
        let spec = PolymarketVenueSpec::polymarket_binary();
        
        // On-tick prices should pass
        assert!(spec.validate_price(0.50, Side::Buy).is_ok());
        assert!(spec.validate_price(0.01, Side::Buy).is_ok());
        assert!(spec.validate_price(0.99, Side::Sell).is_ok());
    }

    #[test]
    fn test_price_validation_off_tick() {
        let spec = PolymarketVenueSpec::polymarket_binary();
        
        // Off-tick prices should be rejected (default policy is Reject)
        let result = spec.validate_price(0.505, Side::Buy);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), RejectionCode::OffTickPrice { .. }));
    }

    #[test]
    fn test_price_validation_out_of_bounds() {
        let spec = PolymarketVenueSpec::polymarket_binary();
        
        // Below minimum
        let result = spec.validate_price(0.001, Side::Buy);
        assert!(matches!(result.unwrap_err(), RejectionCode::PriceBelowMin { .. }));
        
        // Above maximum
        let result = spec.validate_price(1.00, Side::Sell);
        assert!(matches!(result.unwrap_err(), RejectionCode::PriceAboveMax { .. }));
    }

    #[test]
    fn test_size_validation() {
        let spec = PolymarketVenueSpec::polymarket_binary();
        
        // Valid size
        assert!(spec.validate_size(10.0, 0.50).is_ok());
        
        // Below minimum
        let result = spec.validate_size(0.5, 0.50);
        assert!(matches!(result.unwrap_err(), RejectionCode::SizeBelowMin { .. }));
    }

    #[test]
    fn test_fee_calculation() {
        let mut spec = PolymarketVenueSpec::polymarket_binary();
        spec.taker_fee_bps = 100; // 1%
        spec.maker_fee_bps = -20; // -0.2% (rebate)
        
        let taker_fee = spec.calculate_fee(0.50, 100.0, false);
        assert!((taker_fee - 0.50).abs() < 0.0001); // 50 * 0.01 = 0.50
        
        let maker_fee = spec.calculate_fee(0.50, 100.0, true);
        assert!((maker_fee - (-0.10)).abs() < 0.0001); // 50 * -0.002 = -0.10 (rebate)
    }

    #[test]
    fn test_post_only_rejection_on_cross() {
        let mut adapter = PolymarketBacktestExecutionAdapter::new("test_trader");
        
        // Set up book with ask at 0.50
        adapter.apply_book_delta("token_1", Side::Sell, 0.50, 100.0);
        
        // Post-only buy at 0.50 should be rejected (would cross)
        let order = StrategyOrder {
            client_order_id: "po_1".to_string(),
            token_id: "token_1".to_string(),
            side: Side::Buy,
            price: 0.50,
            size: 10.0,
            order_type: OrderType::Limit,
            time_in_force: TimeInForce::Gtc,
            post_only: true,
            reduce_only: false,
        };
        
        let result = adapter.submit_order(order);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), RejectionCode::PostOnlyWouldCross { .. }));
    }

    #[test]
    fn test_ioc_partial_fill_then_cancel() {
        let mut adapter = PolymarketBacktestExecutionAdapter::new("test_trader");
        
        // Set up book with limited liquidity
        adapter.apply_book_delta("token_1", Side::Sell, 0.50, 50.0);
        
        // IOC buy for 100 shares at 0.50
        let order = StrategyOrder {
            client_order_id: "ioc_1".to_string(),
            token_id: "token_1".to_string(),
            side: Side::Buy,
            price: 0.50,
            size: 100.0,
            order_type: OrderType::Ioc,
            time_in_force: TimeInForce::Ioc,
            post_only: false,
            reduce_only: false,
        };
        
        let result = adapter.submit_order(order);
        assert!(result.is_ok());
        
        // Drain events
        let events = adapter.drain_events_until(i64::MAX);
        
        // Should have: ack, fill (50), cancel ack (50)
        let fill_count = events.iter().filter(|e| matches!(e, ExecutionEvent::Fill { .. })).count();
        let cancel_count = events.iter().filter(|e| matches!(e, ExecutionEvent::CancelAck { .. })).count();
        
        assert_eq!(fill_count, 1);
        assert_eq!(cancel_count, 1);
        
        // Verify fill was for 50 shares
        for event in &events {
            if let ExecutionEvent::Fill { size, leaves_qty, .. } = event {
                assert_eq!(*size, 50.0);
                assert_eq!(*leaves_qty, 50.0);
            }
        }
    }

    #[test]
    fn test_deterministic_execution() {
        // Run twice with same inputs, verify same outputs
        let run = || {
            let mut adapter = PolymarketBacktestExecutionAdapter::new("test_trader");
            adapter.apply_book_delta("token_1", Side::Sell, 0.50, 100.0);
            
            let order = make_test_order("token_1", Side::Buy, 0.50, 50.0, TimeInForce::Ioc);
            let _ = adapter.submit_order(order);
            
            let events = adapter.drain_events_until(i64::MAX);
            (adapter.stats().clone(), events.len())
        };
        
        let (stats1, events1) = run();
        let (stats2, events2) = run();
        
        assert_eq!(stats1.orders_submitted, stats2.orders_submitted);
        assert_eq!(stats1.fills_generated, stats2.fills_generated);
        assert_eq!(events1, events2);
    }

    #[test]
    fn test_fee_accounting() {
        let mut spec = PolymarketVenueSpec::polymarket_binary();
        spec.taker_fee_bps = 50; // 0.5%
        
        let mut adapter = PolymarketBacktestExecutionAdapter::with_spec(spec, "test_trader");
        adapter.apply_book_delta("token_1", Side::Sell, 0.50, 100.0);
        
        let order = make_test_order("token_1", Side::Buy, 0.50, 100.0, TimeInForce::Ioc);
        let _ = adapter.submit_order(order);
        
        // Fee should be 50 * 0.005 = 0.25
        assert!((adapter.stats().fees_paid - 0.25).abs() < 0.0001);
    }

    #[test]
    fn test_price_rounding_conservative() {
        let mut spec = PolymarketVenueSpec::polymarket_binary();
        spec.price_rounding = PriceRoundingPolicy::RoundConservative;
        
        // Buy order at 0.505 should round down to 0.50
        let result = spec.validate_price(0.505, Side::Buy);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 0.50);
        
        // Sell order at 0.505 should round up to 0.51
        let result = spec.validate_price(0.505, Side::Sell);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 0.51);
    }

    #[test]
    fn test_gtc_resting_order() {
        let mut adapter = PolymarketBacktestExecutionAdapter::new("test_trader");
        
        // Submit a GTC buy order with no matching liquidity
        let order = make_test_order("token_1", Side::Buy, 0.40, 100.0, TimeInForce::Gtc);
        let result = adapter.submit_order(order);
        assert!(result.is_ok());
        
        // Order should be resting
        let open: Vec<_> = adapter.get_open_orders()
            .into_iter()
            .filter(|o| o.token_id == "token_1")
            .collect();
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].remaining_size, 100.0);
    }

    #[test]
    fn test_market_status_rejection() {
        let mut adapter = PolymarketBacktestExecutionAdapter::new("test_trader");
        adapter.set_market_status("token_1", MarketStatus::Halted);
        
        let order = make_test_order("token_1", Side::Buy, 0.50, 100.0, TimeInForce::Gtc);
        let result = adapter.submit_order(order);
        
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), RejectionCode::MarketNotOpen { .. }));
    }

    #[test]
    fn test_duplicate_client_order_id() {
        let mut adapter = PolymarketBacktestExecutionAdapter::new("test_trader");
        
        let order1 = StrategyOrder {
            client_order_id: "dup_id".to_string(),
            token_id: "token_1".to_string(),
            side: Side::Buy,
            price: 0.40,
            size: 100.0,
            order_type: OrderType::Limit,
            time_in_force: TimeInForce::Gtc,
            post_only: false,
            reduce_only: false,
        };
        
        // First order should succeed
        assert!(adapter.submit_order(order1.clone()).is_ok());
        
        // Second order with same client_order_id should fail
        let result = adapter.submit_order(order1);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), RejectionCode::DuplicateClientOrderId { .. }));
    }
}

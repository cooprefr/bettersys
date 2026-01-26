//! Risk Management and Position Sizing
//!
//! Configurable limits, fractional-Kelly sizing, and pre-trade risk checks.
//! Logs all blocked orders with reasons.

use crate::backtest_v2::clock::Nanos;
use crate::backtest_v2::events::{OrderId, Price, Side, Size};
use crate::backtest_v2::oms::OrderManagementSystem;
use crate::backtest_v2::portfolio::{MarketId, Outcome, Portfolio, TokenId};
use std::collections::HashMap;

/// Risk limits configuration.
#[derive(Debug, Clone)]
pub struct RiskLimits {
    /// Maximum gross exposure as multiple of equity.
    pub max_gross_exposure_mult: f64,
    /// Maximum position in any single market (USD notional).
    pub max_market_position_usd: f64,
    /// Maximum position per market as fraction of equity.
    pub max_market_position_pct: f64,
    /// Maximum single order size (shares).
    pub max_order_size: Size,
    /// Maximum single order notional (USD).
    pub max_order_notional: f64,
    /// Maximum outstanding orders (across all markets).
    pub max_outstanding_orders: usize,
    /// Maximum outstanding orders per market.
    pub max_outstanding_orders_per_market: usize,
    /// Drawdown stop: halt trading if drawdown exceeds this.
    pub max_drawdown_pct: f64,
    /// Minimum cash balance to maintain.
    pub min_cash_balance: f64,
    /// Minimum cash as fraction of equity.
    pub min_cash_pct: f64,
    /// Maximum loss per day (USD).
    pub max_daily_loss: f64,
    /// Maximum number of trades per day.
    pub max_trades_per_day: u64,
    /// Cooldown after hitting a limit (nanoseconds).
    pub cooldown_ns: Nanos,
}

impl Default for RiskLimits {
    fn default() -> Self {
        Self {
            max_gross_exposure_mult: 3.0,
            max_market_position_usd: 10_000.0,
            max_market_position_pct: 0.25,
            max_order_size: 1_000.0,
            max_order_notional: 1_000.0,
            max_outstanding_orders: 50,
            max_outstanding_orders_per_market: 10,
            max_drawdown_pct: 0.20,
            min_cash_balance: 100.0,
            min_cash_pct: 0.05,
            max_daily_loss: 500.0,
            max_trades_per_day: 100,
            cooldown_ns: 60_000_000_000, // 1 minute
        }
    }
}

impl RiskLimits {
    /// Conservative limits for small accounts.
    pub fn conservative() -> Self {
        Self {
            max_gross_exposure_mult: 1.5,
            max_market_position_usd: 2_000.0,
            max_market_position_pct: 0.10,
            max_order_size: 200.0,
            max_order_notional: 200.0,
            max_outstanding_orders: 20,
            max_outstanding_orders_per_market: 5,
            max_drawdown_pct: 0.10,
            min_cash_balance: 200.0,
            min_cash_pct: 0.10,
            max_daily_loss: 200.0,
            max_trades_per_day: 50,
            cooldown_ns: 120_000_000_000,
        }
    }

    /// Aggressive limits for larger accounts.
    pub fn aggressive() -> Self {
        Self {
            max_gross_exposure_mult: 5.0,
            max_market_position_usd: 50_000.0,
            max_market_position_pct: 0.40,
            max_order_size: 5_000.0,
            max_order_notional: 5_000.0,
            max_outstanding_orders: 100,
            max_outstanding_orders_per_market: 20,
            max_drawdown_pct: 0.30,
            min_cash_balance: 500.0,
            min_cash_pct: 0.03,
            max_daily_loss: 2_000.0,
            max_trades_per_day: 500,
            cooldown_ns: 30_000_000_000,
        }
    }
}

/// Fractional Kelly sizing parameters.
#[derive(Debug, Clone)]
pub struct KellyParams {
    /// Kelly fraction (0.25 = quarter Kelly, typical for safety).
    pub kelly_fraction: f64,
    /// Maximum position size as fraction of bankroll.
    pub max_position_pct: f64,
    /// Minimum edge required to take a position.
    pub min_edge: f64,
    /// Maximum edge to cap (prevents oversizing on extreme edges).
    pub max_edge_cap: f64,
    /// Confidence adjustment factor (multiply edge by this).
    pub confidence_factor: f64,
    /// Use volatility scaling.
    pub vol_scale: bool,
    /// Target volatility for scaling.
    pub target_vol: f64,
}

impl Default for KellyParams {
    fn default() -> Self {
        Self {
            kelly_fraction: 0.25,
            max_position_pct: 0.10,
            min_edge: 0.01,
            max_edge_cap: 0.30,
            confidence_factor: 1.0,
            vol_scale: false,
            target_vol: 0.02,
        }
    }
}

impl KellyParams {
    /// Very conservative (1/8 Kelly).
    pub fn conservative() -> Self {
        Self {
            kelly_fraction: 0.125,
            max_position_pct: 0.05,
            min_edge: 0.02,
            max_edge_cap: 0.20,
            confidence_factor: 0.8,
            vol_scale: false,
            target_vol: 0.02,
        }
    }

    /// Moderate (1/4 Kelly).
    pub fn moderate() -> Self {
        Self::default()
    }

    /// Aggressive (1/2 Kelly).
    pub fn aggressive() -> Self {
        Self {
            kelly_fraction: 0.5,
            max_position_pct: 0.20,
            min_edge: 0.005,
            max_edge_cap: 0.40,
            confidence_factor: 1.0,
            vol_scale: false,
            target_vol: 0.02,
        }
    }
}

/// Kelly sizing calculator.
#[derive(Debug, Clone)]
pub struct KellySizer {
    pub params: KellyParams,
}

impl KellySizer {
    pub fn new(params: KellyParams) -> Self {
        Self { params }
    }

    /// Calculate optimal position size using Kelly criterion.
    ///
    /// # Arguments
    /// * `estimated_prob` - Our estimated probability of winning
    /// * `market_price` - Current market price (implied probability)
    /// * `bankroll` - Total available capital
    /// * `current_vol` - Current realized volatility (optional)
    pub fn calculate_size(
        &self,
        estimated_prob: f64,
        market_price: Price,
        bankroll: f64,
        current_vol: Option<f64>,
    ) -> KellyResult {
        // Calculate edge
        let raw_edge = estimated_prob - market_price;
        let adjusted_edge = raw_edge * self.params.confidence_factor;

        // Check minimum edge
        if adjusted_edge < self.params.min_edge {
            return KellyResult {
                recommended_size: 0.0,
                kelly_fraction: 0.0,
                full_kelly: 0.0,
                edge: adjusted_edge,
                blocked: true,
                block_reason: Some(format!(
                    "Edge {:.4} below minimum {:.4}",
                    adjusted_edge, self.params.min_edge
                )),
            };
        }

        // Cap edge
        let capped_edge = adjusted_edge.min(self.params.max_edge_cap);

        // Calculate full Kelly fraction
        // f* = (bp - q) / b where b = odds, p = prob of win, q = 1-p
        // For binary markets: f* = (p - market_price) / (1 - market_price)
        // Simplified: f* = edge / (1 - market_price)
        let full_kelly = if market_price < 0.99 {
            capped_edge / (1.0 - market_price)
        } else {
            0.0
        };

        // Apply fractional Kelly
        let mut kelly_fraction = full_kelly * self.params.kelly_fraction;

        // Apply volatility scaling if enabled
        if self.params.vol_scale {
            if let Some(vol) = current_vol {
                if vol > 0.0 {
                    let vol_scalar = (self.params.target_vol / vol).min(2.0).max(0.25);
                    kelly_fraction *= vol_scalar;
                }
            }
        }

        // Cap at maximum position
        kelly_fraction = kelly_fraction.min(self.params.max_position_pct);

        // Calculate USD size
        let recommended_size = (bankroll * kelly_fraction).max(0.0);

        KellyResult {
            recommended_size,
            kelly_fraction,
            full_kelly,
            edge: adjusted_edge,
            blocked: false,
            block_reason: None,
        }
    }

    /// Calculate size for a specific side.
    pub fn calculate_side_size(
        &self,
        side: Side,
        our_fair_value: Price,
        market_bid: Price,
        market_ask: Price,
        bankroll: f64,
    ) -> KellyResult {
        match side {
            Side::Buy => {
                // Buying: we think fair > ask, edge = fair - ask
                let edge = our_fair_value - market_ask;
                self.calculate_size(our_fair_value, market_ask, bankroll, None)
            }
            Side::Sell => {
                // Selling: we think fair < bid, edge = bid - fair
                let edge = market_bid - our_fair_value;
                // For selling, we use complement probability
                let complement_fair = 1.0 - our_fair_value;
                let complement_market = 1.0 - market_bid;
                self.calculate_size(complement_fair, complement_market, bankroll, None)
            }
        }
    }
}

/// Result of Kelly calculation.
#[derive(Debug, Clone)]
pub struct KellyResult {
    /// Recommended position size in USD.
    pub recommended_size: f64,
    /// Fractional Kelly used.
    pub kelly_fraction: f64,
    /// Full (uncapped) Kelly fraction.
    pub full_kelly: f64,
    /// Calculated edge.
    pub edge: f64,
    /// Whether sizing was blocked.
    pub blocked: bool,
    /// Reason for blocking.
    pub block_reason: Option<String>,
}

/// Reason an order was blocked.
#[derive(Debug, Clone)]
pub enum BlockReason {
    /// Drawdown limit exceeded.
    DrawdownStop { current: f64, limit: f64 },
    /// Maximum gross exposure exceeded.
    GrossExposure { current: f64, limit: f64 },
    /// Maximum market position exceeded.
    MarketPosition {
        market_id: String,
        current: f64,
        limit: f64,
    },
    /// Maximum order size exceeded.
    OrderSize { requested: f64, limit: f64 },
    /// Maximum order notional exceeded.
    OrderNotional { requested: f64, limit: f64 },
    /// Too many outstanding orders.
    OutstandingOrders { current: usize, limit: usize },
    /// Too many orders in market.
    MarketOrders {
        market_id: String,
        current: usize,
        limit: usize,
    },
    /// Insufficient cash.
    InsufficientCash { required: f64, available: f64 },
    /// Below minimum cash.
    MinCash { current: f64, required: f64 },
    /// Daily loss limit.
    DailyLoss { current: f64, limit: f64 },
    /// Daily trade limit.
    DailyTrades { current: u64, limit: u64 },
    /// Cooldown active.
    Cooldown { remaining_ns: Nanos },
    /// Edge too small.
    InsufficientEdge { edge: f64, min_edge: f64 },
    /// Market halted.
    MarketHalted { market_id: String },
    /// Custom reason.
    Custom(String),
}

impl std::fmt::Display for BlockReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BlockReason::DrawdownStop { current, limit } => {
                write!(
                    f,
                    "Drawdown stop: {:.2}% > {:.2}% limit",
                    current * 100.0,
                    limit * 100.0
                )
            }
            BlockReason::GrossExposure { current, limit } => {
                write!(f, "Gross exposure ${:.2} > ${:.2} limit", current, limit)
            }
            BlockReason::MarketPosition {
                market_id,
                current,
                limit,
            } => {
                write!(
                    f,
                    "Market {} position ${:.2} > ${:.2} limit",
                    market_id, current, limit
                )
            }
            BlockReason::OrderSize { requested, limit } => {
                write!(f, "Order size {:.2} > {:.2} limit", requested, limit)
            }
            BlockReason::OrderNotional { requested, limit } => {
                write!(f, "Order notional ${:.2} > ${:.2} limit", requested, limit)
            }
            BlockReason::OutstandingOrders { current, limit } => {
                write!(f, "Outstanding orders {} > {} limit", current, limit)
            }
            BlockReason::MarketOrders {
                market_id,
                current,
                limit,
            } => {
                write!(
                    f,
                    "Market {} orders {} > {} limit",
                    market_id, current, limit
                )
            }
            BlockReason::InsufficientCash {
                required,
                available,
            } => {
                write!(
                    f,
                    "Insufficient cash: need ${:.2}, have ${:.2}",
                    required, available
                )
            }
            BlockReason::MinCash { current, required } => {
                write!(
                    f,
                    "Below min cash: ${:.2} < ${:.2} required",
                    current, required
                )
            }
            BlockReason::DailyLoss { current, limit } => {
                write!(f, "Daily loss ${:.2} > ${:.2} limit", current, limit)
            }
            BlockReason::DailyTrades { current, limit } => {
                write!(f, "Daily trades {} > {} limit", current, limit)
            }
            BlockReason::Cooldown { remaining_ns } => {
                write!(
                    f,
                    "Cooldown active: {}ms remaining",
                    remaining_ns / 1_000_000
                )
            }
            BlockReason::InsufficientEdge { edge, min_edge } => {
                write!(f, "Edge {:.4} < {:.4} minimum", edge, min_edge)
            }
            BlockReason::MarketHalted { market_id } => {
                write!(f, "Market {} is halted", market_id)
            }
            BlockReason::Custom(reason) => write!(f, "{}", reason),
        }
    }
}

/// Risk check result.
#[derive(Debug, Clone)]
pub enum RiskCheckResult {
    /// Order approved.
    Approved,
    /// Order blocked with reason.
    Blocked(BlockReason),
    /// Order approved but size reduced.
    SizeReduced {
        original: Size,
        reduced: Size,
        reason: String,
    },
}

impl RiskCheckResult {
    pub fn is_approved(&self) -> bool {
        matches!(
            self,
            RiskCheckResult::Approved | RiskCheckResult::SizeReduced { .. }
        )
    }

    pub fn is_blocked(&self) -> bool {
        matches!(self, RiskCheckResult::Blocked(_))
    }
}

/// Risk manager state.
#[derive(Debug, Clone, Default)]
pub struct RiskState {
    /// Cooldown expiry timestamp.
    pub cooldown_until: Nanos,
    /// Daily loss tracking.
    pub daily_loss: f64,
    /// Daily trade count.
    pub daily_trades: u64,
    /// Current day start timestamp.
    pub day_start: Nanos,
    /// Blocked order log.
    pub blocked_orders: Vec<BlockedOrder>,
    /// Statistics.
    pub stats: RiskStats,
}

/// Record of a blocked order.
#[derive(Debug, Clone)]
pub struct BlockedOrder {
    pub timestamp: Nanos,
    pub market_id: String,
    pub side: Side,
    pub size: Size,
    pub price: Price,
    pub reason: BlockReason,
}

/// Risk statistics.
#[derive(Debug, Clone, Default)]
pub struct RiskStats {
    pub orders_checked: u64,
    pub orders_approved: u64,
    pub orders_blocked: u64,
    pub orders_size_reduced: u64,
    pub blocks_by_drawdown: u64,
    pub blocks_by_exposure: u64,
    pub blocks_by_position: u64,
    pub blocks_by_size: u64,
    pub blocks_by_cash: u64,
    pub blocks_by_daily_limit: u64,
    pub blocks_by_cooldown: u64,
    pub total_size_reduction: f64,
}

/// Risk manager for backtest.
pub struct RiskManager {
    pub limits: RiskLimits,
    pub kelly: KellySizer,
    pub state: RiskState,
    /// Enable logging of blocked orders.
    pub log_blocks: bool,
}

impl RiskManager {
    pub fn new(limits: RiskLimits, kelly_params: KellyParams) -> Self {
        Self {
            limits,
            kelly: KellySizer::new(kelly_params),
            state: RiskState::default(),
            log_blocks: true,
        }
    }

    /// Check if an order passes all risk limits.
    pub fn check_order(
        &mut self,
        market_id: &str,
        outcome: Outcome,
        side: Side,
        size: Size,
        price: Price,
        portfolio: &Portfolio,
        oms: &OrderManagementSystem,
        prices: &HashMap<TokenId, Price>,
        now: Nanos,
    ) -> RiskCheckResult {
        self.state.stats.orders_checked += 1;

        // Reset daily counters if new day
        self.maybe_reset_daily(now);

        // Check cooldown
        if now < self.state.cooldown_until {
            let reason = BlockReason::Cooldown {
                remaining_ns: self.state.cooldown_until - now,
            };
            return self.block_order(market_id, side, size, price, reason, now);
        }

        // Calculate equity
        let equity = portfolio.equity(prices);

        // Check drawdown
        let drawdown = portfolio.drawdown(prices);
        if drawdown > self.limits.max_drawdown_pct {
            self.activate_cooldown(now);
            let reason = BlockReason::DrawdownStop {
                current: drawdown,
                limit: self.limits.max_drawdown_pct,
            };
            self.state.stats.blocks_by_drawdown += 1;
            return self.block_order(market_id, side, size, price, reason, now);
        }

        // Check daily loss
        if self.state.daily_loss > self.limits.max_daily_loss {
            let reason = BlockReason::DailyLoss {
                current: self.state.daily_loss,
                limit: self.limits.max_daily_loss,
            };
            self.state.stats.blocks_by_daily_limit += 1;
            return self.block_order(market_id, side, size, price, reason, now);
        }

        // Check daily trades
        if self.state.daily_trades >= self.limits.max_trades_per_day {
            let reason = BlockReason::DailyTrades {
                current: self.state.daily_trades,
                limit: self.limits.max_trades_per_day,
            };
            self.state.stats.blocks_by_daily_limit += 1;
            return self.block_order(market_id, side, size, price, reason, now);
        }

        // Check order size
        if size > self.limits.max_order_size {
            let reason = BlockReason::OrderSize {
                requested: size,
                limit: self.limits.max_order_size,
            };
            self.state.stats.blocks_by_size += 1;
            return self.block_order(market_id, side, size, price, reason, now);
        }

        // Check order notional
        let notional = size * price;
        if notional > self.limits.max_order_notional {
            let reason = BlockReason::OrderNotional {
                requested: notional,
                limit: self.limits.max_order_notional,
            };
            self.state.stats.blocks_by_size += 1;
            return self.block_order(market_id, side, size, price, reason, now);
        }

        // Check gross exposure
        let current_gross = portfolio.gross_exposure();
        let max_gross = equity * self.limits.max_gross_exposure_mult;
        if current_gross + size > max_gross {
            let reason = BlockReason::GrossExposure {
                current: current_gross + size,
                limit: max_gross,
            };
            self.state.stats.blocks_by_exposure += 1;
            return self.block_order(market_id, side, size, price, reason, now);
        }

        // Check market position
        if let Some(market) = portfolio.get_market(market_id) {
            let market_pos = market.gross_position();
            let max_market_usd = self
                .limits
                .max_market_position_usd
                .min(equity * self.limits.max_market_position_pct);
            if market_pos + size > max_market_usd {
                let reason = BlockReason::MarketPosition {
                    market_id: market_id.to_string(),
                    current: market_pos + size,
                    limit: max_market_usd,
                };
                self.state.stats.blocks_by_position += 1;
                return self.block_order(market_id, side, size, price, reason, now);
            }
        }

        // Check outstanding orders
        let total_orders = oms.open_order_count();
        if total_orders >= self.limits.max_outstanding_orders {
            let reason = BlockReason::OutstandingOrders {
                current: total_orders,
                limit: self.limits.max_outstanding_orders,
            };
            return self.block_order(market_id, side, size, price, reason, now);
        }

        // Check market orders
        let market_orders = oms.open_order_count_for_token(market_id);
        if market_orders >= self.limits.max_outstanding_orders_per_market {
            let reason = BlockReason::MarketOrders {
                market_id: market_id.to_string(),
                current: market_orders,
                limit: self.limits.max_outstanding_orders_per_market,
            };
            return self.block_order(market_id, side, size, price, reason, now);
        }

        // Check cash for buys
        if side == Side::Buy {
            let required_cash = notional;
            let min_cash = self
                .limits
                .min_cash_balance
                .max(equity * self.limits.min_cash_pct);
            let available = portfolio.cash - min_cash;

            if available < required_cash {
                let reason = BlockReason::InsufficientCash {
                    required: required_cash,
                    available,
                };
                self.state.stats.blocks_by_cash += 1;
                return self.block_order(market_id, side, size, price, reason, now);
            }

            // Check if this would put us below min cash
            if portfolio.cash - required_cash < min_cash {
                let reason = BlockReason::MinCash {
                    current: portfolio.cash - required_cash,
                    required: min_cash,
                };
                self.state.stats.blocks_by_cash += 1;
                return self.block_order(market_id, side, size, price, reason, now);
            }
        }

        // All checks passed
        self.state.stats.orders_approved += 1;
        RiskCheckResult::Approved
    }

    /// Calculate optimal size using Kelly criterion.
    pub fn calculate_kelly_size(
        &self,
        estimated_prob: f64,
        market_price: Price,
        portfolio: &Portfolio,
        prices: &HashMap<TokenId, Price>,
    ) -> KellyResult {
        let bankroll = portfolio.equity(prices);
        self.kelly
            .calculate_size(estimated_prob, market_price, bankroll, None)
    }

    /// Check and potentially reduce order size to fit limits.
    pub fn check_and_size_order(
        &mut self,
        market_id: &str,
        outcome: Outcome,
        side: Side,
        requested_size: Size,
        price: Price,
        portfolio: &Portfolio,
        oms: &OrderManagementSystem,
        prices: &HashMap<TokenId, Price>,
        now: Nanos,
    ) -> RiskCheckResult {
        // First check with requested size
        let result = self.check_order(
            market_id,
            outcome,
            side,
            requested_size,
            price,
            portfolio,
            oms,
            prices,
            now,
        );

        // If blocked for size reasons, try to find acceptable size
        if let RiskCheckResult::Blocked(ref reason) = result {
            match reason {
                BlockReason::OrderSize { limit, .. } | BlockReason::OrderNotional { limit, .. } => {
                    let max_size = if matches!(reason, BlockReason::OrderNotional { .. }) {
                        limit / price
                    } else {
                        *limit
                    };

                    if max_size > 1.0 {
                        let reduced_result = self.check_order(
                            market_id, outcome, side, max_size, price, portfolio, oms, prices, now,
                        );

                        if reduced_result.is_approved() {
                            self.state.stats.orders_size_reduced += 1;
                            self.state.stats.total_size_reduction += requested_size - max_size;
                            return RiskCheckResult::SizeReduced {
                                original: requested_size,
                                reduced: max_size,
                                reason: format!("{}", reason),
                            };
                        }
                    }
                }
                BlockReason::GrossExposure { current, limit } => {
                    let available = limit - (current - requested_size);
                    if available > 1.0 {
                        let reduced_result = self.check_order(
                            market_id, outcome, side, available, price, portfolio, oms, prices, now,
                        );

                        if reduced_result.is_approved() {
                            self.state.stats.orders_size_reduced += 1;
                            self.state.stats.total_size_reduction += requested_size - available;
                            return RiskCheckResult::SizeReduced {
                                original: requested_size,
                                reduced: available,
                                reason: format!("{}", reason),
                            };
                        }
                    }
                }
                BlockReason::InsufficientCash { available, .. } => {
                    let max_size = available / price;
                    if max_size > 1.0 {
                        let reduced_result = self.check_order(
                            market_id, outcome, side, max_size, price, portfolio, oms, prices, now,
                        );

                        if reduced_result.is_approved() {
                            self.state.stats.orders_size_reduced += 1;
                            self.state.stats.total_size_reduction += requested_size - max_size;
                            return RiskCheckResult::SizeReduced {
                                original: requested_size,
                                reduced: max_size,
                                reason: format!("{}", reason),
                            };
                        }
                    }
                }
                _ => {}
            }
        }

        result
    }

    /// Record a realized loss (call after fills).
    pub fn record_loss(&mut self, loss: f64) {
        if loss > 0.0 {
            self.state.daily_loss += loss;
        }
    }

    /// Record a trade (call after fills).
    pub fn record_trade(&mut self) {
        self.state.daily_trades += 1;
    }

    /// Activate cooldown period.
    pub fn activate_cooldown(&mut self, now: Nanos) {
        self.state.cooldown_until = now + self.limits.cooldown_ns;
        self.state.stats.blocks_by_cooldown += 1;
    }

    /// Get blocked orders log.
    pub fn get_blocked_orders(&self) -> &[BlockedOrder] {
        &self.state.blocked_orders
    }

    /// Clear blocked orders log.
    pub fn clear_blocked_orders(&mut self) {
        self.state.blocked_orders.clear();
    }

    /// Reset the risk manager state.
    pub fn reset(&mut self) {
        self.state = RiskState::default();
    }

    // Private helpers

    fn block_order(
        &mut self,
        market_id: &str,
        side: Side,
        size: Size,
        price: Price,
        reason: BlockReason,
        now: Nanos,
    ) -> RiskCheckResult {
        self.state.stats.orders_blocked += 1;

        if self.log_blocks {
            let blocked = BlockedOrder {
                timestamp: now,
                market_id: market_id.to_string(),
                side,
                size,
                price,
                reason: reason.clone(),
            };

            // Log to stderr in debug builds
            #[cfg(debug_assertions)]
            eprintln!(
                "[RISK] Order blocked: {} {} {:.2} @ {:.4} - {}",
                market_id,
                match side {
                    Side::Buy => "BUY",
                    Side::Sell => "SELL",
                },
                size,
                price,
                reason
            );

            self.state.blocked_orders.push(blocked);
        }

        RiskCheckResult::Blocked(reason)
    }

    fn maybe_reset_daily(&mut self, now: Nanos) {
        const DAY_NS: Nanos = 86_400_000_000_000; // 24 hours

        if now - self.state.day_start >= DAY_NS {
            self.state.daily_loss = 0.0;
            self.state.daily_trades = 0;
            self.state.day_start = now;
        }
    }
}

/// Builder for RiskManager.
pub struct RiskManagerBuilder {
    limits: RiskLimits,
    kelly_params: KellyParams,
    log_blocks: bool,
}

impl Default for RiskManagerBuilder {
    fn default() -> Self {
        Self {
            limits: RiskLimits::default(),
            kelly_params: KellyParams::default(),
            log_blocks: true,
        }
    }
}

impl RiskManagerBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn limits(mut self, limits: RiskLimits) -> Self {
        self.limits = limits;
        self
    }

    pub fn kelly(mut self, params: KellyParams) -> Self {
        self.kelly_params = params;
        self
    }

    pub fn log_blocks(mut self, enabled: bool) -> Self {
        self.log_blocks = enabled;
        self
    }

    pub fn max_gross_exposure(mut self, mult: f64) -> Self {
        self.limits.max_gross_exposure_mult = mult;
        self
    }

    pub fn max_market_position(mut self, usd: f64, pct: f64) -> Self {
        self.limits.max_market_position_usd = usd;
        self.limits.max_market_position_pct = pct;
        self
    }

    pub fn max_order_size(mut self, size: Size) -> Self {
        self.limits.max_order_size = size;
        self
    }

    pub fn max_order_notional(mut self, notional: f64) -> Self {
        self.limits.max_order_notional = notional;
        self
    }

    pub fn max_outstanding_orders(mut self, total: usize, per_market: usize) -> Self {
        self.limits.max_outstanding_orders = total;
        self.limits.max_outstanding_orders_per_market = per_market;
        self
    }

    pub fn max_drawdown(mut self, pct: f64) -> Self {
        self.limits.max_drawdown_pct = pct;
        self
    }

    pub fn kelly_fraction(mut self, fraction: f64) -> Self {
        self.kelly_params.kelly_fraction = fraction;
        self
    }

    pub fn min_edge(mut self, edge: f64) -> Self {
        self.kelly_params.min_edge = edge;
        self
    }

    pub fn build(self) -> RiskManager {
        let mut rm = RiskManager::new(self.limits, self.kelly_params);
        rm.log_blocks = self.log_blocks;
        rm
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_portfolio(cash: f64) -> Portfolio {
        Portfolio::new(cash)
    }

    fn make_oms() -> OrderManagementSystem {
        OrderManagementSystem::new(crate::backtest_v2::oms::VenueConstraints::default())
    }

    #[test]
    fn test_kelly_sizing() {
        let kelly = KellySizer::new(KellyParams::default());

        // Strong edge: 60% prob, 50% market price
        let result = kelly.calculate_size(0.60, 0.50, 10_000.0, None);

        assert!(!result.blocked);
        assert!(result.edge > 0.0);
        assert!(result.recommended_size > 0.0);
        assert!(result.recommended_size <= 10_000.0 * 0.10); // Max 10%
    }

    #[test]
    fn test_kelly_no_edge() {
        let kelly = KellySizer::new(KellyParams::default());

        // No edge: 50% prob, 50% market price
        let result = kelly.calculate_size(0.50, 0.50, 10_000.0, None);

        assert!(result.blocked);
        assert_eq!(result.recommended_size, 0.0);
    }

    #[test]
    fn test_kelly_negative_edge() {
        let kelly = KellySizer::new(KellyParams::default());

        // Negative edge: 40% prob, 50% market price
        let result = kelly.calculate_size(0.40, 0.50, 10_000.0, None);

        assert!(result.blocked);
        assert!(result.edge < 0.0);
    }

    #[test]
    fn test_risk_manager_drawdown_stop() {
        let mut rm = RiskManagerBuilder::new()
            .max_drawdown(0.10)
            .log_blocks(false)
            .build();

        let mut portfolio = make_portfolio(10_000.0);
        let oms = make_oms();
        let prices = HashMap::new();

        // Simulate drawdown by reducing cash
        portfolio.cash = 8_500.0; // 15% loss
        portfolio.equity_high_watermark = 10_000.0;

        let result = rm.check_order(
            "market1",
            Outcome::Yes,
            Side::Buy,
            100.0,
            0.50,
            &portfolio,
            &oms,
            &prices,
            1_000_000_000,
        );

        assert!(matches!(
            result,
            RiskCheckResult::Blocked(BlockReason::DrawdownStop { .. })
        ));
    }

    #[test]
    fn test_risk_manager_order_size() {
        let mut rm = RiskManagerBuilder::new()
            .max_order_size(100.0)
            .log_blocks(false)
            .build();

        let portfolio = make_portfolio(10_000.0);
        let oms = make_oms();
        let prices = HashMap::new();

        let result = rm.check_order(
            "market1",
            Outcome::Yes,
            Side::Buy,
            200.0, // Too large
            0.50,
            &portfolio,
            &oms,
            &prices,
            1_000_000_000,
        );

        assert!(matches!(
            result,
            RiskCheckResult::Blocked(BlockReason::OrderSize { .. })
        ));
    }

    #[test]
    fn test_risk_manager_approved() {
        let mut rm = RiskManagerBuilder::new().log_blocks(false).build();

        let portfolio = make_portfolio(10_000.0);
        let oms = make_oms();
        let prices = HashMap::new();

        let result = rm.check_order(
            "market1",
            Outcome::Yes,
            Side::Buy,
            50.0,
            0.50,
            &portfolio,
            &oms,
            &prices,
            1_000_000_000,
        );

        assert!(matches!(result, RiskCheckResult::Approved));
    }

    #[test]
    fn test_risk_manager_size_reduction() {
        let mut rm = RiskManagerBuilder::new()
            .max_order_size(100.0)
            .log_blocks(false)
            .build();

        let portfolio = make_portfolio(10_000.0);
        let oms = make_oms();
        let prices = HashMap::new();

        let result = rm.check_and_size_order(
            "market1",
            Outcome::Yes,
            Side::Buy,
            200.0, // Too large, should be reduced
            0.50,
            &portfolio,
            &oms,
            &prices,
            1_000_000_000,
        );

        assert!(matches!(
            result,
            RiskCheckResult::SizeReduced { reduced: 100.0, .. }
        ));
    }

    #[test]
    fn test_daily_limits_reset() {
        let mut rm = RiskManagerBuilder::new().log_blocks(false).build();

        rm.state.daily_loss = 1000.0;
        rm.state.daily_trades = 50;
        rm.state.day_start = 0;

        // Advance by more than a day
        let now = 100_000_000_000_000i64; // > 24 hours in ns
        rm.maybe_reset_daily(now);

        assert_eq!(rm.state.daily_loss, 0.0);
        assert_eq!(rm.state.daily_trades, 0);
    }
}

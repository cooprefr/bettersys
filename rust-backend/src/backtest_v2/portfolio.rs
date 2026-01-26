//! Portfolio, Accounting, and Settlement
//!
//! Tracks positions, PnL, fees, and handles binary outcome settlement.
//! Supports YES/NO complement coupling for risk management.
//!
//! # Strict Accounting Mode
//!
//! When `strict_accounting=true`, the direct mutation methods in this module
//! are FORBIDDEN. All economic state changes MUST go through the double-entry
//! ledger. The `guard_direct_mutation!` macro enforces this at runtime.
//!
//! ## Forbidden methods in strict mode:
//! - `Position::apply_fill()` - Use `Ledger::post_fill()` instead
//! - `Portfolio::apply_fill()` - Use `Ledger::post_fill()` instead
//! - `Portfolio::settle_market()` - Use `Ledger::post_settlement()` instead
//! - `Portfolio::deposit()` - Use `Ledger::post_deposit()` instead
//! - `Portfolio::withdraw()` - Use `Ledger::post_withdrawal()` instead

use crate::backtest_v2::clock::Nanos;
use crate::backtest_v2::events::{Price, Side, Size};
use crate::guard_direct_mutation;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Outcome type for binary markets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum Outcome {
    #[default]
    Yes,
    No,
}

impl Outcome {
    pub fn complement(&self) -> Self {
        match self {
            Outcome::Yes => Outcome::No,
            Outcome::No => Outcome::Yes,
        }
    }

    pub fn settlement_value(&self, winner: Outcome) -> Price {
        if *self == winner {
            1.0
        } else {
            0.0
        }
    }
}

/// Unique identifier for a market.
pub type MarketId = String;

/// Unique identifier for an outcome token within a market.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub struct TokenId {
    pub market_id: MarketId,
    pub outcome: Outcome,
}

impl TokenId {
    pub fn new(market_id: impl Into<String>, outcome: Outcome) -> Self {
        Self {
            market_id: market_id.into(),
            outcome,
        }
    }

    pub fn complement(&self) -> Self {
        Self {
            market_id: self.market_id.clone(),
            outcome: self.outcome.complement(),
        }
    }
}

/// Position in a single outcome token.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TokenPosition {
    /// Token identifier.
    pub token_id: TokenId,
    /// Number of shares held (positive = long, negative = short).
    pub shares: Size,
    /// Total cost basis (what we paid to acquire).
    pub cost_basis: f64,
    /// Average entry price.
    pub avg_entry_price: Price,
    /// Realized PnL (from closed portions).
    pub realized_pnl: f64,
    /// Total fees paid.
    pub total_fees: f64,
    /// Number of trades.
    pub trade_count: u64,
    /// Last trade timestamp.
    pub last_trade_at: Option<Nanos>,
}

impl TokenPosition {
    pub fn new(token_id: TokenId) -> Self {
        Self {
            token_id,
            shares: 0.0,
            cost_basis: 0.0,
            avg_entry_price: 0.0,
            realized_pnl: 0.0,
            total_fees: 0.0,
            trade_count: 0,
            last_trade_at: None,
        }
    }

    /// Apply a fill to this position.
    /// 
    /// # Strict Accounting Mode
    /// 
    /// This method is FORBIDDEN when `strict_accounting=true`.
    /// Use `Ledger::post_fill()` instead to ensure double-entry accounting.
    pub fn apply_fill(&mut self, side: Side, qty: Size, price: Price, fee: f64, now: Nanos) {
        guard_direct_mutation!("TokenPosition::apply_fill");
        
        let signed_qty = match side {
            Side::Buy => qty,
            Side::Sell => -qty,
        };
        let trade_value = qty * price;

        let old_shares = self.shares;
        let new_shares = old_shares + signed_qty;

        // Determine if this is opening, closing, or flipping
        if old_shares.signum() == signed_qty.signum() || old_shares.abs() < 1e-9 {
            // Opening or adding to position
            self.cost_basis += trade_value;
            self.shares = new_shares;

            // Update average entry price
            if self.shares.abs() > 1e-9 {
                self.avg_entry_price = self.cost_basis / self.shares.abs();
            }
        } else {
            // Closing (partially or fully) or flipping
            let closing_qty = qty.min(old_shares.abs());
            let opening_qty = qty - closing_qty;

            // Realize PnL on closing portion
            if closing_qty > 0.0 {
                let exit_value = closing_qty * price;
                let entry_value = closing_qty * self.avg_entry_price;

                let pnl = if old_shares > 0.0 {
                    // Was long, selling
                    exit_value - entry_value
                } else {
                    // Was short, buying
                    entry_value - exit_value
                };

                self.realized_pnl += pnl;

                // Reduce cost basis proportionally
                let ratio = closing_qty / old_shares.abs();
                self.cost_basis *= 1.0 - ratio;
            }

            // Handle any opening portion (position flip)
            if opening_qty > 0.0 {
                self.cost_basis = opening_qty * price;
                self.avg_entry_price = price;
            }

            self.shares = new_shares;

            // Recalculate avg entry if position remains
            if self.shares.abs() > 1e-9 && self.cost_basis > 1e-9 {
                self.avg_entry_price = self.cost_basis / self.shares.abs();
            } else if self.shares.abs() < 1e-9 {
                self.avg_entry_price = 0.0;
                self.cost_basis = 0.0;
            }
        }

        self.total_fees += fee;
        self.trade_count += 1;
        self.last_trade_at = Some(now);
    }

    /// Calculate unrealized PnL at a given mark price.
    pub fn unrealized_pnl(&self, mark_price: Price) -> f64 {
        if self.shares.abs() < 1e-9 {
            return 0.0;
        }

        let mark_value = self.shares.abs() * mark_price;

        if self.shares > 0.0 {
            // Long position
            mark_value - self.cost_basis
        } else {
            // Short position
            self.cost_basis - mark_value
        }
    }

    /// Total PnL (realized + unrealized).
    pub fn total_pnl(&self, mark_price: Price) -> f64 {
        self.realized_pnl + self.unrealized_pnl(mark_price)
    }

    /// Net PnL after fees.
    pub fn net_pnl(&self, mark_price: Price) -> f64 {
        self.total_pnl(mark_price) - self.total_fees
    }

    /// Check if position is flat.
    pub fn is_flat(&self) -> bool {
        self.shares.abs() < 1e-9
    }

    /// Check if position is long.
    pub fn is_long(&self) -> bool {
        self.shares > 1e-9
    }

    /// Check if position is short.
    pub fn is_short(&self) -> bool {
        self.shares < -1e-9
    }

    /// Settlement value at resolution.
    pub fn settlement_value(&self, winner: Outcome) -> f64 {
        let payoff_per_share = self.token_id.outcome.settlement_value(winner);
        self.shares * payoff_per_share
    }

    /// Terminal PnL at settlement.
    pub fn settlement_pnl(&self, winner: Outcome) -> f64 {
        let settlement = self.settlement_value(winner);
        let pnl = settlement - self.cost_basis + self.realized_pnl;
        pnl - self.total_fees
    }
}

/// Market-level position (YES + NO combined).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketPosition {
    pub market_id: MarketId,
    pub yes_position: TokenPosition,
    pub no_position: TokenPosition,
    /// Market metadata
    pub market_title: Option<String>,
    pub created_at: Option<Nanos>,
    pub resolved_at: Option<Nanos>,
    pub resolution: Option<Outcome>,
}

impl MarketPosition {
    pub fn new(market_id: impl Into<String>) -> Self {
        let market_id = market_id.into();
        Self {
            yes_position: TokenPosition::new(TokenId::new(&market_id, Outcome::Yes)),
            no_position: TokenPosition::new(TokenId::new(&market_id, Outcome::No)),
            market_id,
            market_title: None,
            created_at: None,
            resolved_at: None,
            resolution: None,
        }
    }

    /// Get position for an outcome.
    pub fn get_position(&self, outcome: Outcome) -> &TokenPosition {
        match outcome {
            Outcome::Yes => &self.yes_position,
            Outcome::No => &self.no_position,
        }
    }

    /// Get mutable position for an outcome.
    pub fn get_position_mut(&mut self, outcome: Outcome) -> &mut TokenPosition {
        match outcome {
            Outcome::Yes => &mut self.yes_position,
            Outcome::No => &mut self.no_position,
        }
    }

    /// Net shares exposure (YES - NO).
    /// Positive = net long YES, negative = net long NO.
    pub fn net_exposure(&self) -> Size {
        self.yes_position.shares - self.no_position.shares
    }

    /// Gross position (|YES| + |NO|).
    pub fn gross_position(&self) -> Size {
        self.yes_position.shares.abs() + self.no_position.shares.abs()
    }

    /// Combined realized PnL.
    pub fn realized_pnl(&self) -> f64 {
        self.yes_position.realized_pnl + self.no_position.realized_pnl
    }

    /// Combined unrealized PnL.
    pub fn unrealized_pnl(&self, yes_price: Price, no_price: Price) -> f64 {
        self.yes_position.unrealized_pnl(yes_price) + self.no_position.unrealized_pnl(no_price)
    }

    /// Combined total fees.
    pub fn total_fees(&self) -> f64 {
        self.yes_position.total_fees + self.no_position.total_fees
    }

    /// Check if market is fully hedged (equal YES and NO).
    pub fn is_hedged(&self) -> bool {
        (self.yes_position.shares - self.no_position.shares).abs() < 1e-9
    }

    /// Hedged amount (min of YES and NO positions).
    pub fn hedged_amount(&self) -> Size {
        if self.yes_position.shares > 0.0 && self.no_position.shares > 0.0 {
            self.yes_position.shares.min(self.no_position.shares)
        } else {
            0.0
        }
    }

    /// Settlement value at resolution.
    pub fn settlement_value(&self, winner: Outcome) -> f64 {
        self.yes_position.settlement_value(winner) + self.no_position.settlement_value(winner)
    }

    /// Settlement PnL at resolution.
    pub fn settlement_pnl(&self, winner: Outcome) -> f64 {
        self.yes_position.settlement_pnl(winner) + self.no_position.settlement_pnl(winner)
    }

    /// Worst-case settlement exposure (max loss across outcomes).
    pub fn worst_case_exposure(&self) -> f64 {
        let pnl_if_yes = self.settlement_pnl(Outcome::Yes);
        let pnl_if_no = self.settlement_pnl(Outcome::No);
        pnl_if_yes.min(pnl_if_no)
    }

    /// Best-case settlement (max gain across outcomes).
    pub fn best_case_exposure(&self) -> f64 {
        let pnl_if_yes = self.settlement_pnl(Outcome::Yes);
        let pnl_if_no = self.settlement_pnl(Outcome::No);
        pnl_if_yes.max(pnl_if_no)
    }

    /// Apply resolution.
    pub fn resolve(&mut self, winner: Outcome, now: Nanos) {
        self.resolution = Some(winner);
        self.resolved_at = Some(now);
    }
}

/// Portfolio-level accounting.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Portfolio {
    /// Cash balance (USDC or equivalent).
    pub cash: f64,
    /// Initial cash for reference.
    pub initial_cash: f64,
    /// Market positions by market_id.
    pub markets: HashMap<MarketId, MarketPosition>,
    /// Total fees paid.
    pub total_fees: f64,
    /// Total realized PnL.
    pub total_realized_pnl: f64,
    /// Total deposits.
    pub total_deposits: f64,
    /// Total withdrawals.
    pub total_withdrawals: f64,
    /// Equity high watermark (for drawdown).
    pub equity_high_watermark: f64,
    /// Equity curve samples.
    pub equity_curve: Vec<(Nanos, f64)>,
    /// Trade history summary.
    pub trade_count: u64,
    pub winning_trades: u64,
    pub losing_trades: u64,
}

impl Portfolio {
    pub fn new(initial_cash: f64) -> Self {
        Self {
            cash: initial_cash,
            initial_cash,
            markets: HashMap::new(),
            total_fees: 0.0,
            total_realized_pnl: 0.0,
            total_deposits: initial_cash,
            total_withdrawals: 0.0,
            equity_high_watermark: initial_cash,
            equity_curve: vec![],
            trade_count: 0,
            winning_trades: 0,
            losing_trades: 0,
        }
    }

    /// Get or create a market position.
    pub fn get_or_create_market(&mut self, market_id: &str) -> &mut MarketPosition {
        self.markets
            .entry(market_id.to_string())
            .or_insert_with(|| MarketPosition::new(market_id))
    }

    /// Get a market position.
    pub fn get_market(&self, market_id: &str) -> Option<&MarketPosition> {
        self.markets.get(market_id)
    }

    /// Apply a fill to the portfolio.
    /// 
    /// # Strict Accounting Mode
    /// 
    /// This method is FORBIDDEN when `strict_accounting=true`.
    /// Use `Ledger::post_fill()` instead to ensure double-entry accounting.
    pub fn apply_fill(
        &mut self,
        market_id: &str,
        outcome: Outcome,
        side: Side,
        qty: Size,
        price: Price,
        fee: f64,
        now: Nanos,
    ) {
        guard_direct_mutation!("Portfolio::apply_fill");
        
        // Update cash first (before borrowing markets)
        let trade_value = qty * price;
        match side {
            Side::Buy => {
                self.cash -= trade_value + fee;
            }
            Side::Sell => {
                self.cash += trade_value - fee;
            }
        }

        // Get or create market position and apply fill
        let market = self.get_or_create_market(market_id);
        let position = market.get_position_mut(outcome);

        // Track PnL before fill
        let pnl_before = position.realized_pnl;

        // Apply fill to position
        position.apply_fill(side, qty, price, fee, now);

        // Track realized PnL change
        let pnl_change = position.realized_pnl - pnl_before;

        // Update portfolio stats
        self.total_realized_pnl += pnl_change;
        self.total_fees += fee;
        self.trade_count += 1;

        if pnl_change > 0.0 {
            self.winning_trades += 1;
        } else if pnl_change < 0.0 {
            self.losing_trades += 1;
        }
    }

    /// Deposit cash.
    /// 
    /// # Strict Accounting Mode
    /// 
    /// This method is FORBIDDEN when `strict_accounting=true`.
    /// Use `Ledger::post_deposit()` instead.
    pub fn deposit(&mut self, amount: f64) {
        guard_direct_mutation!("Portfolio::deposit");
        self.cash += amount;
        self.total_deposits += amount;
    }

    /// Withdraw cash.
    /// 
    /// # Strict Accounting Mode
    /// 
    /// This method is FORBIDDEN when `strict_accounting=true`.
    /// Use `Ledger::post_withdrawal()` instead.
    pub fn withdraw(&mut self, amount: f64) -> Result<(), String> {
        guard_direct_mutation!("Portfolio::withdraw");
        if amount > self.cash {
            return Err("Insufficient funds".into());
        }
        self.cash -= amount;
        self.total_withdrawals += amount;
        Ok(())
    }

    /// Settle a market at resolution.
    /// 
    /// # Strict Accounting Mode
    /// 
    /// This method is FORBIDDEN when `strict_accounting=true`.
    /// Use `Ledger::post_settlement()` instead.
    pub fn settle_market(&mut self, market_id: &str, winner: Outcome, now: Nanos) -> f64 {
        guard_direct_mutation!("Portfolio::settle_market");
        
        let Some(market) = self.markets.get_mut(market_id) else {
            return 0.0;
        };

        // Calculate settlement value
        let settlement_value = market.settlement_value(winner);
        let settlement_pnl = market.settlement_pnl(winner);

        // Add settlement to cash
        self.cash += settlement_value;

        // Update realized PnL
        self.total_realized_pnl += settlement_pnl - market.realized_pnl();

        // Mark as resolved
        market.resolve(winner, now);

        // Zero out positions
        market.yes_position.shares = 0.0;
        market.yes_position.cost_basis = 0.0;
        market.no_position.shares = 0.0;
        market.no_position.cost_basis = 0.0;

        settlement_pnl
    }

    /// Calculate total equity (cash + mark-to-market positions).
    pub fn equity(&self, prices: &HashMap<TokenId, Price>) -> f64 {
        let mut total = self.cash;

        for market in self.markets.values() {
            // Mark YES position
            if let Some(&yes_price) = prices.get(&market.yes_position.token_id) {
                total += market.yes_position.unrealized_pnl(yes_price);
            }
            // Mark NO position
            if let Some(&no_price) = prices.get(&market.no_position.token_id) {
                total += market.no_position.unrealized_pnl(no_price);
            }
        }

        total
    }

    /// Record equity sample for curve.
    pub fn record_equity(&mut self, now: Nanos, prices: &HashMap<TokenId, Price>) {
        let eq = self.equity(prices);
        self.equity_curve.push((now, eq));
        self.equity_high_watermark = self.equity_high_watermark.max(eq);
    }

    /// Current drawdown from high watermark.
    pub fn drawdown(&self, prices: &HashMap<TokenId, Price>) -> f64 {
        let eq = self.equity(prices);
        if self.equity_high_watermark > 0.0 {
            (self.equity_high_watermark - eq) / self.equity_high_watermark
        } else {
            0.0
        }
    }

    /// Worst-case equity across all markets.
    pub fn worst_case_equity(&self) -> f64 {
        let mut worst_case = self.cash;

        for market in self.markets.values() {
            worst_case += market.worst_case_exposure();
        }

        worst_case
    }

    /// Best-case equity across all markets.
    pub fn best_case_equity(&self) -> f64 {
        let mut best_case = self.cash;

        for market in self.markets.values() {
            best_case += market.best_case_exposure();
        }

        best_case
    }

    /// Total gross exposure.
    pub fn gross_exposure(&self) -> f64 {
        self.markets.values().map(|m| m.gross_position()).sum()
    }

    /// Total net exposure.
    pub fn net_exposure(&self) -> f64 {
        self.markets.values().map(|m| m.net_exposure().abs()).sum()
    }

    /// Win rate.
    pub fn win_rate(&self) -> f64 {
        let total = self.winning_trades + self.losing_trades;
        if total > 0 {
            self.winning_trades as f64 / total as f64
        } else {
            0.0
        }
    }

    /// Return on initial capital.
    pub fn roi(&self, prices: &HashMap<TokenId, Price>) -> f64 {
        let eq = self.equity(prices);
        if self.initial_cash > 0.0 {
            (eq - self.initial_cash) / self.initial_cash
        } else {
            0.0
        }
    }

    /// Generate portfolio summary.
    pub fn summary(&self, prices: &HashMap<TokenId, Price>) -> PortfolioSummary {
        let equity = self.equity(prices);
        let max_drawdown = self.calculate_max_drawdown();

        PortfolioSummary {
            cash: self.cash,
            equity,
            initial_cash: self.initial_cash,
            total_realized_pnl: self.total_realized_pnl,
            total_unrealized_pnl: equity - self.cash - self.total_realized_pnl,
            total_fees: self.total_fees,
            trade_count: self.trade_count,
            win_rate: self.win_rate(),
            roi: self.roi(prices),
            max_drawdown,
            worst_case_equity: self.worst_case_equity(),
            best_case_equity: self.best_case_equity(),
            gross_exposure: self.gross_exposure(),
            net_exposure: self.net_exposure(),
            active_markets: self.markets.len(),
        }
    }

    fn calculate_max_drawdown(&self) -> f64 {
        let mut max_dd: f64 = 0.0;
        let mut peak: f64 = 0.0;

        for &(_, eq) in &self.equity_curve {
            if eq > peak {
                peak = eq;
            }
            if peak > 0.0 {
                let dd = (peak - eq) / peak;
                max_dd = max_dd.max(dd);
            }
        }

        max_dd
    }
}

/// Portfolio summary for reporting.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortfolioSummary {
    pub cash: f64,
    pub equity: f64,
    pub initial_cash: f64,
    pub total_realized_pnl: f64,
    pub total_unrealized_pnl: f64,
    pub total_fees: f64,
    pub trade_count: u64,
    pub win_rate: f64,
    pub roi: f64,
    pub max_drawdown: f64,
    pub worst_case_equity: f64,
    pub best_case_equity: f64,
    pub gross_exposure: f64,
    pub net_exposure: f64,
    pub active_markets: usize,
}

/// Margin/risk constraints for position sizing.
#[derive(Debug, Clone)]
pub struct RiskConstraints {
    /// Maximum gross exposure as fraction of equity.
    pub max_gross_exposure: f64,
    /// Maximum position in any single market.
    pub max_market_exposure: f64,
    /// Maximum worst-case loss as fraction of equity.
    pub max_worst_case_loss: f64,
    /// Minimum cash buffer.
    pub min_cash_buffer: f64,
    /// Maximum concentration in any market.
    pub max_concentration: f64,
}

impl Default for RiskConstraints {
    fn default() -> Self {
        Self {
            max_gross_exposure: 5.0,  // 5x equity
            max_market_exposure: 1.0, // 100% of equity per market
            max_worst_case_loss: 0.5, // Max 50% loss
            min_cash_buffer: 0.1,     // 10% cash minimum
            max_concentration: 0.25,  // Max 25% in one market
        }
    }
}

impl RiskConstraints {
    /// Check if a proposed trade passes risk constraints.
    pub fn check_trade(
        &self,
        portfolio: &Portfolio,
        market_id: &str,
        outcome: Outcome,
        side: Side,
        qty: Size,
        price: Price,
        prices: &HashMap<TokenId, Price>,
    ) -> Result<(), RiskViolation> {
        let equity = portfolio.equity(prices);
        if equity <= 0.0 {
            return Err(RiskViolation::InsufficientEquity);
        }

        // Simulate the trade
        let trade_value = qty * price;
        let new_cash = match side {
            Side::Buy => portfolio.cash - trade_value,
            Side::Sell => portfolio.cash + trade_value,
        };

        // Cash buffer check
        if new_cash < equity * self.min_cash_buffer {
            return Err(RiskViolation::CashBufferViolation {
                required: equity * self.min_cash_buffer,
                actual: new_cash,
            });
        }

        // Gross exposure check
        let new_gross = portfolio.gross_exposure() + qty;
        if new_gross > equity * self.max_gross_exposure {
            return Err(RiskViolation::GrossExposureViolation {
                max: equity * self.max_gross_exposure,
                actual: new_gross,
            });
        }

        // Market concentration check
        if let Some(market) = portfolio.markets.get(market_id) {
            let market_exposure = market.gross_position() + qty;
            if market_exposure > equity * self.max_concentration {
                return Err(RiskViolation::ConcentrationViolation {
                    market_id: market_id.to_string(),
                    max: equity * self.max_concentration,
                    actual: market_exposure,
                });
            }
        }

        Ok(())
    }
}

/// Risk constraint violation.
#[derive(Debug, Clone)]
pub enum RiskViolation {
    InsufficientEquity,
    CashBufferViolation {
        required: f64,
        actual: f64,
    },
    GrossExposureViolation {
        max: f64,
        actual: f64,
    },
    ConcentrationViolation {
        market_id: String,
        max: f64,
        actual: f64,
    },
    WorstCaseLossViolation {
        max: f64,
        actual: f64,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_token_position_buy() {
        let mut pos = TokenPosition::new(TokenId::new("market1", Outcome::Yes));

        // Buy 100 @ 0.50
        pos.apply_fill(Side::Buy, 100.0, 0.50, 0.05, 1000);

        assert_eq!(pos.shares, 100.0);
        assert_eq!(pos.cost_basis, 50.0);
        assert_eq!(pos.avg_entry_price, 0.50);
        assert_eq!(pos.total_fees, 0.05);
    }

    #[test]
    fn test_token_position_sell_with_pnl() {
        let mut pos = TokenPosition::new(TokenId::new("market1", Outcome::Yes));

        // Buy 100 @ 0.50
        pos.apply_fill(Side::Buy, 100.0, 0.50, 0.0, 1000);

        // Sell 50 @ 0.60 (profit)
        pos.apply_fill(Side::Sell, 50.0, 0.60, 0.0, 2000);

        assert_eq!(pos.shares, 50.0);
        assert_eq!(pos.realized_pnl, 5.0); // (0.60 - 0.50) * 50
        assert_eq!(pos.cost_basis, 25.0); // Remaining 50 @ 0.50
    }

    #[test]
    fn test_token_position_settlement() {
        let mut pos = TokenPosition::new(TokenId::new("market1", Outcome::Yes));

        // Buy 100 YES @ 0.40
        pos.apply_fill(Side::Buy, 100.0, 0.40, 1.0, 1000);

        // If YES wins: 100 * 1.0 = 100
        let settlement_yes = pos.settlement_value(Outcome::Yes);
        assert_eq!(settlement_yes, 100.0);

        // PnL: 100 - 40 - 1 = 59
        let pnl_yes = pos.settlement_pnl(Outcome::Yes);
        assert_eq!(pnl_yes, 59.0);

        // If NO wins: 100 * 0.0 = 0
        let settlement_no = pos.settlement_value(Outcome::No);
        assert_eq!(settlement_no, 0.0);

        // PnL: 0 - 40 - 1 = -41
        let pnl_no = pos.settlement_pnl(Outcome::No);
        assert_eq!(pnl_no, -41.0);
    }

    #[test]
    fn test_market_position_hedged() {
        let mut market = MarketPosition::new("market1");

        // Buy 100 YES @ 0.50
        market
            .yes_position
            .apply_fill(Side::Buy, 100.0, 0.50, 0.0, 1000);

        // Buy 100 NO @ 0.50
        market
            .no_position
            .apply_fill(Side::Buy, 100.0, 0.50, 0.0, 2000);

        assert!(market.is_hedged());
        assert_eq!(market.hedged_amount(), 100.0);

        // Hedged position: both outcomes pay 100, cost 100
        // Settlement value always 100 regardless of outcome
        assert_eq!(market.settlement_value(Outcome::Yes), 100.0);
        assert_eq!(market.settlement_value(Outcome::No), 100.0);
    }

    #[test]
    fn test_market_worst_case_exposure() {
        let mut market = MarketPosition::new("market1");

        // Long YES only: worst case is NO wins
        market
            .yes_position
            .apply_fill(Side::Buy, 100.0, 0.60, 2.0, 1000);

        let worst = market.worst_case_exposure();
        // If NO wins: 0 - 60 - 2 = -62
        assert_eq!(worst, -62.0);

        let best = market.best_case_exposure();
        // If YES wins: 100 - 60 - 2 = 38
        assert_eq!(best, 38.0);
    }

    #[test]
    fn test_portfolio_fill() {
        let mut portfolio = Portfolio::new(1000.0);

        // Buy 100 YES @ 0.50 with $0.50 fee
        portfolio.apply_fill("market1", Outcome::Yes, Side::Buy, 100.0, 0.50, 0.50, 1000);

        assert_eq!(portfolio.cash, 1000.0 - 50.0 - 0.50);
        assert_eq!(portfolio.total_fees, 0.50);
        assert_eq!(portfolio.trade_count, 1);

        let market = portfolio.get_market("market1").unwrap();
        assert_eq!(market.yes_position.shares, 100.0);
    }

    #[test]
    fn test_portfolio_settlement() {
        let mut portfolio = Portfolio::new(1000.0);

        // Buy 100 YES @ 0.40
        portfolio.apply_fill("market1", Outcome::Yes, Side::Buy, 100.0, 0.40, 0.0, 1000);

        // Cash: 1000 - 40 = 960
        assert_eq!(portfolio.cash, 960.0);

        // Settle with YES winning
        let pnl = portfolio.settle_market("market1", Outcome::Yes, 2000);

        // Settlement: 100 shares * 1.0 = 100
        // Cash: 960 + 100 = 1060
        assert_eq!(portfolio.cash, 1060.0);

        // PnL: 100 - 40 = 60
        assert_eq!(pnl, 60.0);
    }

    #[test]
    fn test_portfolio_equity() {
        let mut portfolio = Portfolio::new(1000.0);

        // Buy 100 YES @ 0.50
        portfolio.apply_fill("market1", Outcome::Yes, Side::Buy, 100.0, 0.50, 0.0, 1000);

        // Cash: 950, Position: 100 YES
        let mut prices = HashMap::new();
        let token_id = TokenId::new("market1", Outcome::Yes);

        // If YES is now worth 0.60
        prices.insert(token_id, 0.60);

        // Equity: 950 + (100 * 0.60 - 50) = 950 + 10 = 960
        // Wait, let me recalculate...
        // Cash = 950, unrealized = (60 - 50) = 10
        // Equity = 950 + 10 = 960
        let equity = portfolio.equity(&prices);
        assert_eq!(equity, 960.0);
    }

    #[test]
    fn test_risk_constraints() {
        let portfolio = Portfolio::new(1000.0);
        let constraints = RiskConstraints::default();
        let prices = HashMap::new();

        // Try to buy way too much
        let result = constraints.check_trade(
            &portfolio,
            "market1",
            Outcome::Yes,
            Side::Buy,
            10000.0, // Way too much
            0.50,
            &prices,
        );

        assert!(result.is_err());
    }
}

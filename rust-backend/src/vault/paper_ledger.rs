use std::collections::HashMap;

#[derive(Debug, Clone, Default)]
pub struct VaultPaperLedger {
    pub cash_usdc: f64,
    pub positions: HashMap<String, VaultPaperPosition>,
    /// Total fees paid (for tracking)
    pub total_fees_usdc: f64,
    /// Total slippage cost (for tracking)
    pub total_slippage_usdc: f64,
    /// Number of trades executed
    pub trade_count: u64,
    /// Number of rejected orders
    pub reject_count: u64,
    /// Number of partial fills
    pub partial_fill_count: u64,
}

#[derive(Debug, Clone)]
pub struct VaultPaperPosition {
    pub token_id: String,
    pub outcome: String,
    pub shares: f64,
    pub cost_usdc: f64,
    pub avg_price: f64,
}

impl VaultPaperLedger {
    /// Apply a buy order with fees deducted from cash
    /// Returns actual shares acquired
    pub fn apply_buy(
        &mut self,
        token_id: &str,
        outcome: &str,
        price: f64,
        notional: f64,
        fees: f64,
    ) -> f64 {
        if !(price > 0.0 && price < 1.0) {
            return 0.0;
        }
        if !(notional > 0.0) {
            return 0.0;
        }

        let shares = notional / price;
        let total_cost = notional + fees;

        // Deduct notional + fees from cash
        self.cash_usdc = (self.cash_usdc - total_cost).max(0.0);
        self.total_fees_usdc += fees;
        self.trade_count += 1;

        let entry = self
            .positions
            .entry(token_id.to_string())
            .or_insert_with(|| VaultPaperPosition {
                token_id: token_id.to_string(),
                outcome: outcome.to_string(),
                shares: 0.0,
                cost_usdc: 0.0,
                avg_price: price,
            });

        let new_cost = entry.cost_usdc + notional; // Cost basis excludes fees
        let new_shares = entry.shares + shares;
        entry.cost_usdc = new_cost;
        entry.shares = new_shares;
        entry.avg_price = if new_shares > 0.0 {
            new_cost / new_shares
        } else {
            price
        };

        shares
    }

    /// Apply a sell order with fees deducted from proceeds
    /// Returns actual shares sold
    pub fn apply_sell(&mut self, token_id: &str, price: f64, notional: f64, fees: f64) -> f64 {
        if !(price > 0.0 && price < 1.0) {
            return 0.0;
        }
        if !(notional > 0.0) {
            return 0.0;
        }

        let Some(pos) = self.positions.get_mut(token_id) else {
            return 0.0;
        };
        if !(pos.shares > 0.0) {
            return 0.0;
        }

        let target_shares = notional / price;
        let shares_sold = target_shares.min(pos.shares);
        if !(shares_sold > 0.0) {
            return 0.0;
        }

        let notional_received = shares_sold * price;
        let cost_reduced = pos.avg_price * shares_sold;

        pos.shares = (pos.shares - shares_sold).max(0.0);
        pos.cost_usdc = (pos.cost_usdc - cost_reduced).max(0.0);

        // Credit proceeds minus fees
        self.cash_usdc += (notional_received - fees).max(0.0);
        self.total_fees_usdc += fees;
        self.trade_count += 1;

        if pos.shares <= 1e-9 {
            self.positions.remove(token_id);
        } else if pos.shares > 0.0 {
            pos.avg_price = if pos.shares > 0.0 {
                (pos.cost_usdc / pos.shares).max(1e-9)
            } else {
                pos.avg_price
            };
        }

        shares_sold
    }

    /// Record a rejected order
    pub fn record_reject(&mut self) {
        self.reject_count += 1;
    }

    /// Record a partial fill
    pub fn record_partial_fill(&mut self) {
        self.partial_fill_count += 1;
    }

    /// Record slippage cost for tracking
    pub fn record_slippage(&mut self, slippage_usdc: f64) {
        self.total_slippage_usdc += slippage_usdc;
    }

    /// Get execution stats summary
    pub fn execution_stats(&self) -> ExecutionStats {
        ExecutionStats {
            trade_count: self.trade_count,
            reject_count: self.reject_count,
            partial_fill_count: self.partial_fill_count,
            total_fees_usdc: self.total_fees_usdc,
            total_slippage_usdc: self.total_slippage_usdc,
            reject_rate: if self.trade_count + self.reject_count > 0 {
                self.reject_count as f64 / (self.trade_count + self.reject_count) as f64
            } else {
                0.0
            },
            partial_fill_rate: if self.trade_count > 0 {
                self.partial_fill_count as f64 / self.trade_count as f64
            } else {
                0.0
            },
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ExecutionStats {
    pub trade_count: u64,
    pub reject_count: u64,
    pub partial_fill_count: u64,
    pub total_fees_usdc: f64,
    pub total_slippage_usdc: f64,
    pub reject_rate: f64,
    pub partial_fill_rate: f64,
}

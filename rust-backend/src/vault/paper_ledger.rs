use std::collections::HashMap;

#[derive(Debug, Clone, Default)]
pub struct VaultPaperLedger {
    pub cash_usdc: f64,
    pub positions: HashMap<String, VaultPaperPosition>,
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
    pub fn apply_buy(&mut self, token_id: &str, outcome: &str, price: f64, notional: f64) {
        if !(price > 0.0 && price < 1.0) {
            return;
        }
        if !(notional > 0.0) {
            return;
        }

        let shares = notional / price;
        self.cash_usdc = (self.cash_usdc - notional).max(0.0);

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

        let new_cost = entry.cost_usdc + notional;
        let new_shares = entry.shares + shares;
        entry.cost_usdc = new_cost;
        entry.shares = new_shares;
        entry.avg_price = if new_shares > 0.0 {
            new_cost / new_shares
        } else {
            price
        };
    }

    pub fn apply_sell(&mut self, token_id: &str, price: f64, notional: f64) {
        if !(price > 0.0 && price < 1.0) {
            return;
        }
        if !(notional > 0.0) {
            return;
        }

        let Some(pos) = self.positions.get_mut(token_id) else {
            return;
        };
        if !(pos.shares > 0.0) {
            return;
        }

        let target_shares = notional / price;
        let shares_sold = target_shares.min(pos.shares);
        if !(shares_sold > 0.0) {
            return;
        }

        let notional_received = shares_sold * price;
        let cost_reduced = pos.avg_price * shares_sold;

        pos.shares = (pos.shares - shares_sold).max(0.0);
        pos.cost_usdc = (pos.cost_usdc - cost_reduced).max(0.0);

        self.cash_usdc += notional_received;

        if pos.shares <= 1e-9 {
            self.positions.remove(token_id);
        } else if pos.shares > 0.0 {
            pos.avg_price = if pos.shares > 0.0 {
                (pos.cost_usdc / pos.shares).max(1e-9)
            } else {
                pos.avg_price
            };
        }
    }
}

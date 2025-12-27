//! Fee Calculation for Cross-Platform Arbitrage
//! Mission: Accurate profit calculation after all fees
//! Philosophy: A profitable trade on paper must be profitable in reality

use serde::{Deserialize, Serialize};

/// Fee structure for cross-platform arbitrage
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeeStructure {
    /// Polymarket taker fee (2%)
    pub polymarket_taker_fee: f64,

    /// Polymarket gas cost (average on Polygon)
    pub polymarket_gas_usd: f64,

    /// Kalshi trading fee (7% on profits)
    pub kalshi_fee: f64,

    /// Minimum slippage buffer (1%)
    pub slippage_buffer: f64,
}

impl Default for FeeStructure {
    fn default() -> Self {
        Self {
            polymarket_taker_fee: 0.02, // 2%
            polymarket_gas_usd: 0.30,   // $0.30 avg gas
            kalshi_fee: 0.07,           // 7%
            slippage_buffer: 0.01,      // 1%
        }
    }
}

/// Fee calculator for arbitrage opportunities
pub struct FeeCalculator {
    fees: FeeStructure,
}

impl FeeCalculator {
    pub fn new(fees: FeeStructure) -> Self {
        Self { fees }
    }

    pub fn default() -> Self {
        Self {
            fees: FeeStructure::default(),
        }
    }

    /// Calculate total fees for a two-leg arbitrage trade
    ///
    /// # Arguments
    /// * `buy_price` - Price on platform where we buy (cheaper)
    /// * `sell_price` - Price on platform where we sell (expensive)
    /// * `shares` - Number of shares to trade
    ///
    /// # Returns
    /// Total fees in USD
    pub fn calculate_total_fees(&self, buy_price: f64, sell_price: f64, shares: f64) -> f64 {
        // Leg 1: Buy shares (Kalshi typically)
        let buy_cost = buy_price * shares;
        let buy_fees = buy_cost * self.fees.kalshi_fee;

        // Leg 2: Sell shares (Polymarket typically)
        let sell_revenue = sell_price * shares;
        let sell_fees = sell_revenue * self.fees.polymarket_taker_fee;

        // Gas costs (Polymarket on Polygon)
        let gas_fees = self.fees.polymarket_gas_usd;

        // Total fees
        buy_fees + sell_fees + gas_fees
    }

    /// Calculate net profit after all fees
    ///
    /// # Arguments
    /// * `buy_price` - Price on cheaper platform
    /// * `sell_price` - Price on expensive platform
    /// * `shares` - Number of shares
    ///
    /// # Returns
    /// (gross_profit, total_fees, net_profit, net_profit_pct)
    pub fn calculate_net_profit(
        &self,
        buy_price: f64,
        sell_price: f64,
        shares: f64,
    ) -> (f64, f64, f64, f64) {
        let buy_cost = buy_price * shares;
        let sell_revenue = sell_price * shares;
        let gross_profit = sell_revenue - buy_cost;

        let total_fees = self.calculate_total_fees(buy_price, sell_price, shares);
        let net_profit = gross_profit - total_fees;
        let net_profit_pct = net_profit / buy_cost;

        (gross_profit, total_fees, net_profit, net_profit_pct)
    }

    /// Check if arbitrage is profitable after fees
    ///
    /// # Arguments
    /// * `buy_price` - Price on cheaper platform
    /// * `sell_price` - Price on expensive platform
    /// * `min_profit_pct` - Minimum acceptable profit percentage (e.g., 0.03 for 3%)
    ///
    /// # Returns
    /// true if profitable, false otherwise
    pub fn is_profitable(&self, buy_price: f64, sell_price: f64, min_profit_pct: f64) -> bool {
        // Calculate for 100 shares as a baseline
        let shares = 100.0;
        let (_, _, _, net_profit_pct) = self.calculate_net_profit(buy_price, sell_price, shares);

        net_profit_pct >= min_profit_pct
    }

    /// Calculate maximum position size given bankroll and risk parameters
    ///
    /// # Arguments
    /// * `bankroll` - Available trading capital
    /// * `confidence` - Confidence score (0.0-1.0)
    /// * `spread_pct` - Spread percentage (e.g., 0.05 for 5%)
    /// * `kelly_fraction` - Kelly fraction multiplier (e.g., 0.25 for 25% Kelly)
    ///
    /// # Returns
    /// Maximum position size in USD
    pub fn calculate_position_size(
        &self,
        bankroll: f64,
        confidence: f64,
        spread_pct: f64,
        kelly_fraction: f64,
    ) -> f64 {
        // Kelly Criterion: f = (bp - q) / b
        // Where: f = fraction of bankroll to bet
        //        b = odds (spread)
        //        p = probability of success (confidence)
        //        q = 1 - p

        let edge = spread_pct * confidence;
        let kelly_position = bankroll * kelly_fraction * edge;

        // Cap at 10% of bankroll for safety
        let max_position = bankroll * 0.10;

        kelly_position.min(max_position)
    }

    /// Estimate execution time based on liquidity
    ///
    /// # Arguments
    /// * `position_size_usd` - Desired position size
    /// * `liquidity_usd` - Available liquidity
    ///
    /// # Returns
    /// Estimated seconds to execute
    pub fn estimate_execution_time(&self, position_size_usd: f64, liquidity_usd: f64) -> f64 {
        if liquidity_usd == 0.0 {
            return 999.0; // Effectively impossible
        }

        let liquidity_ratio = position_size_usd / liquidity_usd;

        if liquidity_ratio < 0.01 {
            5.0 // <1% of liquidity: fast execution
        } else if liquidity_ratio < 0.05 {
            15.0 // 1-5%: moderate execution
        } else if liquidity_ratio < 0.10 {
            45.0 // 5-10%: slow execution
        } else {
            120.0 // >10%: very slow, high slippage risk
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fee_calculation() {
        let calculator = FeeCalculator::default();

        // Example: Buy at 0.58, sell at 0.65, 100 shares
        let buy_price = 0.58;
        let sell_price = 0.65;
        let shares = 100.0;

        let (gross, fees, net, net_pct) =
            calculator.calculate_net_profit(buy_price, sell_price, shares);

        // Gross profit = (0.65 - 0.58) * 100 = 7.0
        assert!((gross - 7.0).abs() < 0.01);

        // Fees should be positive
        assert!(fees > 0.0);

        // Net profit should be less than gross
        assert!(net < gross);

        // Net profit percentage should be reasonable
        assert!(net_pct > 0.0 && net_pct < 1.0);

        println!("Gross profit: ${:.2}", gross);
        println!("Total fees: ${:.2}", fees);
        println!("Net profit: ${:.2}", net);
        println!("Net profit %: {:.2}%", net_pct * 100.0);
    }

    #[test]
    fn test_profitability_check() {
        let calculator = FeeCalculator::default();

        // 7% spread - should be profitable under realistic fee structure
        assert!(calculator.is_profitable(0.58, 0.65, 0.02));

        // 2% spread - should NOT be profitable after fees
        assert!(!calculator.is_profitable(0.58, 0.60, 0.03));
    }

    #[test]
    fn test_position_sizing() {
        let calculator = FeeCalculator::default();

        let bankroll = 10000.0;
        let confidence = 0.85;
        let spread_pct = 0.07;
        let kelly_fraction = 0.25;

        let position =
            calculator.calculate_position_size(bankroll, confidence, spread_pct, kelly_fraction);

        // Should be less than max (10% of bankroll)
        assert!(position <= bankroll * 0.10);

        // Should be positive
        assert!(position > 0.0);

        println!("Recommended position: ${:.2}", position);
    }

    #[test]
    fn test_execution_time_estimation() {
        let calculator = FeeCalculator::default();

        // Small position relative to liquidity
        let time1 = calculator.estimate_execution_time(1000.0, 200000.0);
        assert_eq!(time1, 5.0); // Fast

        // Large position relative to liquidity
        let time2 = calculator.estimate_execution_time(15000.0, 100000.0);
        assert!(time2 > 45.0); // Slow
    }
}

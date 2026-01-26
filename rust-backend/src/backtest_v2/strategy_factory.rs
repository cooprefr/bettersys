//! Strategy Factory
//!
//! Maps strategy names to Strategy implementations for the backtest runner.
//!
//! # Supported Strategies
//!
//! ## Testing / Validation
//! - `noop` - Never trades, used for smoke tests and validation
//! - `random_taker` - Random taker strategy for gate suite tests
//!
//! ## Example Strategies (from example_strategy.rs)
//! - `market_maker` - Two-sided market making around mid-price
//! - `momentum` - Momentum-following strategy based on short-term price trends

use crate::backtest_v2::example_strategy::{MarketMakerStrategy, MomentumStrategy};
use crate::backtest_v2::strategy::{
    BookSnapshot, CancelAck, FillNotification, OrderAck, OrderReject, Strategy, StrategyContext,
    StrategyParams, TimerEvent, TradePrint,
};
use std::collections::HashMap;

/// Registry of available strategies with their descriptions.
pub fn available_strategies() -> HashMap<&'static str, &'static str> {
    let mut map = HashMap::new();
    // Testing / validation
    map.insert("noop", "No-op strategy that never trades (smoke test)");
    map.insert("random_taker", "Random taker strategy for gate suite tests");
    // Example strategies
    map.insert("market_maker", "Two-sided market making around mid-price");
    map.insert("momentum", "Momentum-following strategy based on short-term price trends");
    map
}

/// Create a strategy by name.
///
/// # Arguments
/// * `name` - Strategy name (case-insensitive)
/// * `params` - Strategy parameters
///
/// # Returns
/// * `Ok(Box<dyn Strategy>)` on success
/// * `Err(String)` with available strategy names on failure
pub fn make_strategy(name: &str, params: &StrategyParams) -> Result<Box<dyn Strategy>, String> {
    let name_lower = name.to_lowercase();
    
    match name_lower.as_str() {
        // Testing / validation strategies
        "noop" | "no-op" | "no_op" => {
            Ok(Box::new(NoOpStrategy::new(params)))
        }
        "random_taker" | "random-taker" => {
            Ok(Box::new(RandomTakerStrategy::new(params)))
        }
        // Example strategies from example_strategy.rs
        "market_maker" | "market-maker" | "mm" => {
            Ok(Box::new(MarketMakerStrategy::new(params)))
        }
        "momentum" | "momo" => {
            Ok(Box::new(MomentumStrategy::new(params)))
        }
        _ => {
            let available: Vec<_> = available_strategies().keys().copied().collect();
            Err(format!(
                "Unknown strategy: '{}'. Available strategies: {}",
                name,
                available.join(", ")
            ))
        }
    }
}

// =============================================================================
// NO-OP STRATEGY
// =============================================================================

/// A strategy that never trades.
///
/// Useful for:
/// - Smoke testing the backtest infrastructure
/// - Validating data pipelines
/// - Baseline PnL should be exactly 0 (no fees)
pub struct NoOpStrategy {
    name: String,
    book_updates: u64,
    trade_prints: u64,
}

impl NoOpStrategy {
    pub fn new(_params: &StrategyParams) -> Self {
        Self {
            name: "NoOp".to_string(),
            book_updates: 0,
            trade_prints: 0,
        }
    }
}

impl Strategy for NoOpStrategy {
    fn on_book_update(&mut self, _ctx: &mut StrategyContext, _book: &BookSnapshot) {
        self.book_updates += 1;
    }

    fn on_trade(&mut self, _ctx: &mut StrategyContext, _trade: &TradePrint) {
        self.trade_prints += 1;
    }

    fn on_timer(&mut self, _ctx: &mut StrategyContext, _timer: &TimerEvent) {}

    fn on_order_ack(&mut self, _ctx: &mut StrategyContext, _ack: &OrderAck) {}

    fn on_order_reject(&mut self, _ctx: &mut StrategyContext, _reject: &OrderReject) {}

    fn on_fill(&mut self, _ctx: &mut StrategyContext, _fill: &FillNotification) {}

    fn on_cancel_ack(&mut self, _ctx: &mut StrategyContext, _ack: &CancelAck) {}

    fn on_start(&mut self, _ctx: &mut StrategyContext) {}

    fn on_stop(&mut self, _ctx: &mut StrategyContext) {}

    fn name(&self) -> &str {
        &self.name
    }
}

// =============================================================================
// RANDOM TAKER STRATEGY
// =============================================================================

/// A strategy that randomly takes liquidity.
///
/// Used for gate suite tests to verify the backtester produces expected
/// outcomes in zero-edge regimes.
pub struct RandomTakerStrategy {
    name: String,
    seed: u64,
    trade_probability: f64,
    max_position: f64,
    clip_size: f64,
    rng_state: u64,
    position: f64,
    order_counter: u64,
}

impl RandomTakerStrategy {
    pub fn new(params: &StrategyParams) -> Self {
        let seed = params.get_or("seed", 42.0) as u64;
        Self {
            name: "RandomTaker".to_string(),
            seed,
            trade_probability: params.get_or("trade_probability", 0.01),
            max_position: params.get_or("max_position", 1000.0),
            clip_size: params.get_or("clip_size", 10.0),
            rng_state: seed,
            position: 0.0,
            order_counter: 0,
        }
    }

    fn next_random(&mut self) -> f64 {
        // Simple xorshift64 PRNG
        self.rng_state ^= self.rng_state << 13;
        self.rng_state ^= self.rng_state >> 7;
        self.rng_state ^= self.rng_state << 17;
        (self.rng_state as f64) / (u64::MAX as f64)
    }

    fn generate_client_id(&mut self) -> String {
        self.order_counter += 1;
        format!("random_taker_{}", self.order_counter)
    }
}

impl Strategy for RandomTakerStrategy {
    fn on_book_update(&mut self, ctx: &mut StrategyContext, book: &BookSnapshot) {
        // Random chance to trade
        if self.next_random() > self.trade_probability {
            return;
        }

        // Check position limits
        if self.position.abs() >= self.max_position {
            return;
        }

        // Need both bid and ask to cross
        let (best_bid, best_ask) = match (book.best_bid(), book.best_ask()) {
            (Some(b), Some(a)) => (b, a),
            _ => return,
        };

        // Random side
        let is_buy = self.next_random() > 0.5;

        let (side, price) = if is_buy {
            (crate::backtest_v2::events::Side::Buy, best_ask.price)
        } else {
            (crate::backtest_v2::events::Side::Sell, best_bid.price)
        };

        let order = crate::backtest_v2::strategy::StrategyOrder::limit(
            self.generate_client_id(),
            &book.token_id,
            side,
            price,
            self.clip_size,
        )
        .ioc();

        let _ = ctx.orders.send_order(order);
    }

    fn on_trade(&mut self, _ctx: &mut StrategyContext, _trade: &TradePrint) {}

    fn on_timer(&mut self, _ctx: &mut StrategyContext, _timer: &TimerEvent) {}

    fn on_order_ack(&mut self, _ctx: &mut StrategyContext, _ack: &OrderAck) {}

    fn on_order_reject(&mut self, _ctx: &mut StrategyContext, _reject: &OrderReject) {}

    fn on_fill(&mut self, _ctx: &mut StrategyContext, fill: &FillNotification) {
        if fill.is_maker {
            // Shouldn't happen with IOC, but track anyway
        }
        // Track position (simplified - doesn't handle partial fills properly)
        self.position += fill.size;
    }

    fn on_cancel_ack(&mut self, _ctx: &mut StrategyContext, _ack: &CancelAck) {}

    fn on_start(&mut self, _ctx: &mut StrategyContext) {
        self.rng_state = self.seed;
        self.position = 0.0;
        self.order_counter = 0;
    }

    fn on_stop(&mut self, _ctx: &mut StrategyContext) {}

    fn name(&self) -> &str {
        &self.name
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_available_strategies() {
        let strategies = available_strategies();
        assert!(strategies.contains_key("noop"));
        assert!(strategies.contains_key("random_taker"));
        assert!(strategies.contains_key("market_maker"));
        assert!(strategies.contains_key("momentum"));
    }

    #[test]
    fn test_make_strategy_noop() {
        let params = StrategyParams::default();
        let strategy = make_strategy("noop", &params);
        assert!(strategy.is_ok());
        assert_eq!(strategy.unwrap().name(), "NoOp");
    }

    #[test]
    fn test_make_strategy_unknown() {
        let params = StrategyParams::default();
        let result = make_strategy("unknown_strategy", &params);
        assert!(result.is_err());
        match result {
            Ok(_) => panic!("Expected error"),
            Err(err) => {
                assert!(err.contains("Unknown strategy"));
                assert!(err.contains("noop"));
            }
        }
    }

    #[test]
    fn test_noop_strategy() {
        let params = StrategyParams::default();
        let mut strategy = NoOpStrategy::new(&params);
        assert_eq!(strategy.name(), "NoOp");
        assert_eq!(strategy.book_updates, 0);
    }

    #[test]
    fn test_random_taker_determinism() {
        let params = StrategyParams::new().with_param("seed", 12345.0);
        let mut s1 = RandomTakerStrategy::new(&params);
        let mut s2 = RandomTakerStrategy::new(&params);
        
        // Same seed should produce same sequence
        for _ in 0..10 {
            assert_eq!(s1.next_random(), s2.next_random());
        }
    }
}

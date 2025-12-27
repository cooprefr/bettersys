//! Kelly Criterion Position Sizing
//!
//! The Kelly Criterion determines the optimal fraction of bankroll to bet.
//! Formula: f* = (bp - q) / b
//! Where:
//!   f* = fraction of bankroll to bet
//!   b = odds received on the bet (decimal odds - 1)
//!   p = probability of winning
//!   q = probability of losing (1 - p)
//!
//! For Polymarket:
//! - We use confidence as our edge estimate
//! - Price gives us implied probability
//! - Edge = confidence - implied_probability
//!
//! We use FRACTIONAL Kelly (typically 0.25x) to reduce volatility

use serde::{Deserialize, Serialize};

/// Kelly calculation parameters
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KellyParams {
    /// User's total bankroll (in USD)
    pub bankroll: f64,
    /// Fractional Kelly multiplier (0.25 = quarter Kelly, safer)
    pub kelly_fraction: f64,
    /// Maximum single position size as % of bankroll
    pub max_position_pct: f64,
    /// Minimum position size in USD
    pub min_position_usd: f64,
}

impl Default for KellyParams {
    fn default() -> Self {
        Self {
            bankroll: 1000.0,
            kelly_fraction: 0.25,   // Quarter Kelly - conservative
            max_position_pct: 0.10, // Max 10% on any single trade
            min_position_usd: 1.0,  // Minimum $1 trade
        }
    }
}

/// Result of Kelly calculation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KellyResult {
    /// Recommended position size in USD
    pub position_size_usd: f64,
    /// Kelly fraction before applying fractional multiplier
    pub full_kelly_fraction: f64,
    /// Actual fraction used (after applying kelly_fraction multiplier)
    pub actual_fraction: f64,
    /// Expected edge (confidence - implied prob)
    pub edge: f64,
    /// Whether this trade should be taken
    pub should_trade: bool,
    /// Reason if not trading
    pub skip_reason: Option<String>,
}

/// Calculate optimal position size using fractional Kelly criterion
///
/// # Arguments
/// * `confidence` - Our confidence in the outcome (0.0 to 1.0)
/// * `market_price` - Current market price (implied probability)
/// * `params` - Kelly calculation parameters
///
/// # Returns
/// * `KellyResult` - Contains position size and reasoning
pub fn calculate_kelly_position(
    confidence: f64,
    market_price: f64,
    params: &KellyParams,
) -> KellyResult {
    // Validate inputs
    if confidence <= 0.0 || confidence >= 1.0 {
        return KellyResult {
            position_size_usd: 0.0,
            full_kelly_fraction: 0.0,
            actual_fraction: 0.0,
            edge: 0.0,
            should_trade: false,
            skip_reason: Some("Invalid confidence value".to_string()),
        };
    }

    if market_price <= 0.0 || market_price >= 1.0 {
        return KellyResult {
            position_size_usd: 0.0,
            full_kelly_fraction: 0.0,
            actual_fraction: 0.0,
            edge: 0.0,
            should_trade: false,
            skip_reason: Some("Invalid market price".to_string()),
        };
    }

    // Calculate edge
    // Our confidence is our estimate of true probability
    // Market price is implied probability
    let edge = confidence - market_price;

    // If no edge or negative edge, don't trade
    if edge <= 0.0 {
        return KellyResult {
            position_size_usd: 0.0,
            full_kelly_fraction: 0.0,
            actual_fraction: 0.0,
            edge,
            should_trade: false,
            skip_reason: Some(format!(
                "No edge: confidence {:.1}% <= market {:.1}%",
                confidence * 100.0,
                market_price * 100.0
            )),
        };
    }

    // Kelly formula for binary outcomes:
    // f* = (p * (b + 1) - 1) / b
    // Where b = (1/price) - 1 (decimal odds minus 1)
    // Simplified for binary: f* = p - q/b = p - (1-p)/(1/price - 1)
    //
    // Alternative formulation:
    // f* = (edge) / (1 - price)
    // This is the fraction of bankroll to bet on YES at price `price`

    let odds = (1.0 / market_price) - 1.0; // Decimal odds - 1
    let p = confidence; // Our probability estimate
    let q = 1.0 - p; // Probability of losing

    // Kelly formula
    let full_kelly = (p * odds - q) / odds;

    // Clamp to valid range
    let full_kelly = full_kelly.max(0.0).min(1.0);

    // Apply fractional Kelly
    let actual_fraction = full_kelly * params.kelly_fraction;

    // Apply max position constraint
    let capped_fraction = actual_fraction.min(params.max_position_pct);

    // Calculate USD amount
    let position_usd = params.bankroll * capped_fraction;

    // Check minimum position size
    if position_usd < params.min_position_usd {
        return KellyResult {
            position_size_usd: 0.0,
            full_kelly_fraction: full_kelly,
            actual_fraction: capped_fraction,
            edge,
            should_trade: false,
            skip_reason: Some(format!(
                "Position ${:.2} below minimum ${:.2}",
                position_usd, params.min_position_usd
            )),
        };
    }

    KellyResult {
        position_size_usd: position_usd,
        full_kelly_fraction: full_kelly,
        actual_fraction: capped_fraction,
        edge,
        should_trade: true,
        skip_reason: None,
    }
}

/// Calculate Kelly for a specific signal
pub fn kelly_for_signal(
    signal_confidence: f64,
    signal_price: f64,
    user_bankroll: f64,
    kelly_fraction: f64,
) -> KellyResult {
    let params = KellyParams {
        bankroll: user_bankroll,
        kelly_fraction,
        max_position_pct: 0.10,
        min_position_usd: 1.0,
    };

    calculate_kelly_position(signal_confidence, signal_price, &params)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_kelly_with_edge() {
        let params = KellyParams {
            bankroll: 10000.0,
            kelly_fraction: 0.25,
            max_position_pct: 0.10,
            min_position_usd: 1.0,
        };

        // Confidence 60%, market price 50% => edge of 10%
        let result = calculate_kelly_position(0.60, 0.50, &params);

        assert!(result.should_trade);
        assert!(result.edge > 0.0);
        assert!(result.position_size_usd > 0.0);
        println!(
            "Position: ${:.2}, Edge: {:.1}%",
            result.position_size_usd,
            result.edge * 100.0
        );
    }

    #[test]
    fn test_kelly_no_edge() {
        let params = KellyParams::default();

        // Confidence 40%, market price 50% => negative edge
        let result = calculate_kelly_position(0.40, 0.50, &params);

        assert!(!result.should_trade);
        assert!(result.edge < 0.0);
    }

    #[test]
    fn test_kelly_high_confidence() {
        let params = KellyParams {
            bankroll: 10000.0,
            kelly_fraction: 0.25,
            max_position_pct: 0.10,
            min_position_usd: 1.0,
        };

        // Confidence 90%, market price 50% => large edge
        let result = calculate_kelly_position(0.90, 0.50, &params);

        assert!(result.should_trade);
        assert!(result.position_size_usd <= params.bankroll * params.max_position_pct);
        println!("High conf position: ${:.2}", result.position_size_usd);
    }
}

//! Polymarket 15-Minute Up/Down Market Fee Schedule
//!
//! Implements the exact fee structure for 15M crypto markets as specified by Polymarket.
//! Fees were introduced on January 6, 2025 - trades before this date have zero fees.
//!
//! Fee structure is based on a lookup table with linear interpolation for prices
//! between the defined tiers.

use crate::backtest_v2::clock::Nanos;

/// January 6, 2025 00:00:00 UTC in nanoseconds since Unix epoch.
/// Fees are only applied on or after this timestamp.
pub const FEE_START_TIMESTAMP_NS: i64 = 1_736_121_600_000_000_000;

/// January 6, 2025 00:00:00 UTC in seconds since Unix epoch.
pub const FEE_START_TIMESTAMP_SECS: i64 = 1_736_121_600;

/// Fee lookup table: (price, fee_per_share)
/// These are the exact values from Polymarket's fee schedule for 15M Up/Down markets.
const FEE_TABLE: [(f64, f64); 12] = [
    (0.01, 0.0000),
    (0.05, 0.0006),
    (0.10, 0.0020),
    (0.20, 0.0064),
    (0.30, 0.0110),
    (0.40, 0.0144),
    (0.50, 0.0156),
    (0.60, 0.0144),
    (0.70, 0.0110),
    (0.80, 0.0064),
    (0.90, 0.0020),
    (0.99, 0.0000),
];

/// Calculate the fee per share for a given price using the Polymarket 15M fee schedule.
/// Uses linear interpolation between defined price tiers.
///
/// # Arguments
/// * `price` - The execution price (0.01 to 0.99)
///
/// # Returns
/// Fee per share in USD
#[inline]
pub fn fee_per_share_15m(price: f64) -> f64 {
    // Clamp price to valid range
    let price = price.clamp(0.01, 0.99);

    // Find the two surrounding price tiers
    let mut lower_idx = 0;
    for (i, &(p, _)) in FEE_TABLE.iter().enumerate() {
        if p <= price {
            lower_idx = i;
        } else {
            break;
        }
    }

    let upper_idx = (lower_idx + 1).min(FEE_TABLE.len() - 1);

    // If exact match or at boundary, return directly
    if lower_idx == upper_idx {
        return FEE_TABLE[lower_idx].1;
    }

    let (lower_price, lower_fee) = FEE_TABLE[lower_idx];
    let (upper_price, upper_fee) = FEE_TABLE[upper_idx];

    // Exact match check
    if (price - lower_price).abs() < 1e-9 {
        return lower_fee;
    }
    if (price - upper_price).abs() < 1e-9 {
        return upper_fee;
    }

    // Linear interpolation
    let t = (price - lower_price) / (upper_price - lower_price);
    lower_fee + t * (upper_fee - lower_fee)
}

/// Calculate the total fee for a trade on a 15M Up/Down market.
///
/// # Arguments
/// * `price` - The execution price (0.01 to 0.99)
/// * `size` - The number of shares
/// * `timestamp_ns` - The trade timestamp in nanoseconds since Unix epoch
///
/// # Returns
/// Total fee in USD. Returns 0.0 if the trade occurred before January 6, 2025.
#[inline]
pub fn calculate_fee_15m(price: f64, size: f64, timestamp_ns: i64) -> f64 {
    // No fees before January 6, 2025
    if timestamp_ns < FEE_START_TIMESTAMP_NS {
        return 0.0;
    }

    fee_per_share_15m(price) * size
}

/// Calculate the total fee for a trade using seconds timestamp.
///
/// # Arguments
/// * `price` - The execution price (0.01 to 0.99)
/// * `size` - The number of shares
/// * `timestamp_secs` - The trade timestamp in seconds since Unix epoch
///
/// # Returns
/// Total fee in USD. Returns 0.0 if the trade occurred before January 6, 2025.
#[inline]
pub fn calculate_fee_15m_secs(price: f64, size: f64, timestamp_secs: i64) -> f64 {
    // No fees before January 6, 2025
    if timestamp_secs < FEE_START_TIMESTAMP_SECS {
        return 0.0;
    }

    fee_per_share_15m(price) * size
}

/// Check if fees are applicable for a given timestamp.
#[inline]
pub fn fees_enabled(timestamp_ns: i64) -> bool {
    timestamp_ns >= FEE_START_TIMESTAMP_NS
}

/// Check if fees are applicable for a given timestamp (seconds).
#[inline]
pub fn fees_enabled_secs(timestamp_secs: i64) -> bool {
    timestamp_secs >= FEE_START_TIMESTAMP_SECS
}

// =============================================================================
// TESTS
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fee_per_share_exact_values() {
        // Test exact values from the fee table
        assert!((fee_per_share_15m(0.01) - 0.0000).abs() < 1e-9);
        assert!((fee_per_share_15m(0.05) - 0.0006).abs() < 1e-9);
        assert!((fee_per_share_15m(0.10) - 0.0020).abs() < 1e-9);
        assert!((fee_per_share_15m(0.20) - 0.0064).abs() < 1e-9);
        assert!((fee_per_share_15m(0.30) - 0.0110).abs() < 1e-9);
        assert!((fee_per_share_15m(0.40) - 0.0144).abs() < 1e-9);
        assert!((fee_per_share_15m(0.50) - 0.0156).abs() < 1e-9);
        assert!((fee_per_share_15m(0.60) - 0.0144).abs() < 1e-9);
        assert!((fee_per_share_15m(0.70) - 0.0110).abs() < 1e-9);
        assert!((fee_per_share_15m(0.80) - 0.0064).abs() < 1e-9);
        assert!((fee_per_share_15m(0.90) - 0.0020).abs() < 1e-9);
        assert!((fee_per_share_15m(0.99) - 0.0000).abs() < 1e-9);
    }

    #[test]
    fn test_fee_per_share_interpolation() {
        // Test interpolation between 0.40 and 0.50
        // At 0.45: should be halfway between 0.0144 and 0.0156 = 0.0150
        let fee_045 = fee_per_share_15m(0.45);
        assert!((fee_045 - 0.0150).abs() < 1e-6);

        // Test interpolation between 0.10 and 0.20
        // At 0.15: should be halfway between 0.0020 and 0.0064 = 0.0042
        let fee_015 = fee_per_share_15m(0.15);
        assert!((fee_015 - 0.0042).abs() < 1e-6);
    }

    #[test]
    fn test_fee_100_shares() {
        // Verify the fee_100_shares values from the spec
        assert!((fee_per_share_15m(0.01) * 100.0 - 0.0).abs() < 0.01);
        assert!((fee_per_share_15m(0.05) * 100.0 - 0.06).abs() < 0.01);
        assert!((fee_per_share_15m(0.10) * 100.0 - 0.20).abs() < 0.01);
        assert!((fee_per_share_15m(0.50) * 100.0 - 1.56).abs() < 0.01);
        assert!((fee_per_share_15m(0.99) * 100.0 - 0.0).abs() < 0.01);
    }

    #[test]
    fn test_calculate_fee_before_jan6() {
        // January 5, 2025 23:59:59 UTC
        let before_fees_ns: i64 = 1_736_121_599_000_000_000;
        
        // Should be zero regardless of price/size
        assert_eq!(calculate_fee_15m(0.50, 100.0, before_fees_ns), 0.0);
        assert_eq!(calculate_fee_15m(0.30, 1000.0, before_fees_ns), 0.0);
    }

    #[test]
    fn test_calculate_fee_after_jan6() {
        // January 6, 2025 00:00:00 UTC (exactly at cutoff)
        let at_fees_ns: i64 = FEE_START_TIMESTAMP_NS;
        
        // Should apply fees
        let fee = calculate_fee_15m(0.50, 100.0, at_fees_ns);
        assert!((fee - 1.56).abs() < 0.01);

        // January 7, 2025
        let after_fees_ns: i64 = 1_736_208_000_000_000_000;
        let fee2 = calculate_fee_15m(0.50, 100.0, after_fees_ns);
        assert!((fee2 - 1.56).abs() < 0.01);
    }

    #[test]
    fn test_fees_enabled() {
        // Before Jan 6
        assert!(!fees_enabled(FEE_START_TIMESTAMP_NS - 1));
        // At Jan 6
        assert!(fees_enabled(FEE_START_TIMESTAMP_NS));
        // After Jan 6
        assert!(fees_enabled(FEE_START_TIMESTAMP_NS + 1_000_000_000));
    }

    #[test]
    fn test_fee_symmetry() {
        // Fee at 0.30 should equal fee at 0.70 (symmetric around 0.50)
        assert!((fee_per_share_15m(0.30) - fee_per_share_15m(0.70)).abs() < 1e-9);
        // Fee at 0.40 should equal fee at 0.60
        assert!((fee_per_share_15m(0.40) - fee_per_share_15m(0.60)).abs() < 1e-9);
        // Fee at 0.20 should equal fee at 0.80
        assert!((fee_per_share_15m(0.20) - fee_per_share_15m(0.80)).abs() < 1e-9);
    }

    #[test]
    fn test_price_clamping() {
        // Prices outside valid range should be clamped
        assert!((fee_per_share_15m(0.001) - fee_per_share_15m(0.01)).abs() < 1e-9);
        assert!((fee_per_share_15m(1.50) - fee_per_share_15m(0.99)).abs() < 1e-9);
    }
}

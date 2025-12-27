//! Arbitrage Detection Engine
//! Mission: Find and quantify cross-platform price mismatches in real-time
//! Philosophy: Speed + accuracy = profit

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

use crate::arbitrage::fees::FeeCalculator;
use crate::risk::RiskManager;
use crate::scrapers::{dome::DomeScraper, polymarket_api::PolymarketScraper};

/// Minimum profitable spread after fees (3%)
const MIN_PROFITABLE_SPREAD: f64 = 0.03;

/// Minimum liquidity required for arbitrage ($50k)
const MIN_LIQUIDITY_USD: f64 = 50000.0;

/// Maximum execution time in seconds (5 minutes)
const MAX_EXECUTION_TIME_SECS: f64 = 300.0;

/// Arbitrage opportunity detected by the engine
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArbitrageOpportunity {
    pub id: String,
    pub polymarket_market: String,
    pub kalshi_market: Option<String>,
    pub polymarket_price: f64,
    pub kalshi_price: Option<f64>,
    pub spread_pct: f64,
    pub gross_profit_per_share: f64,
    pub net_profit_per_share: f64,
    pub confidence: f64,
    pub polymarket_liquidity: f64,
    pub kalshi_volume: f64,
    pub estimated_execution_time_secs: f64,
    pub detected_at: String,
}

/// Trade leg for execution plan
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradeLeg {
    pub platform: String,
    pub action: String,  // "BUY" or "SELL"
    pub outcome: String, // "YES" or "NO"
    pub shares: f64,
    pub price: f64,
    pub total_cost_usd: f64,
}

/// Complete execution plan for arbitrage
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionPlan {
    pub opportunity_id: String,
    pub leg1: TradeLeg,
    pub leg2: TradeLeg,
    pub expected_profit_usd: f64,
    pub expected_profit_pct: f64,
    pub risk_score: f64,
    pub execution_steps: Vec<String>,
}

/// Arbitrage detection engine
pub struct ArbitrageEngine {
    polymarket_scraper: PolymarketScraper,
    dome_scraper: DomeScraper,
    fee_calculator: FeeCalculator,
    risk_manager: Arc<RwLock<RiskManager>>,
    min_spread: f64,
}

impl ArbitrageEngine {
    /// Create new arbitrage engine
    pub fn new(
        polymarket_scraper: PolymarketScraper,
        dome_scraper: DomeScraper,
        risk_manager: Arc<RwLock<RiskManager>>,
    ) -> Self {
        Self {
            polymarket_scraper,
            dome_scraper,
            fee_calculator: FeeCalculator::default(),
            risk_manager,
            min_spread: MIN_PROFITABLE_SPREAD,
        }
    }

    /// Scan for cross-platform arbitrage opportunities
    ///
    /// This is the main entry point for arbitrage detection.
    /// It fetches markets from both platforms and identifies profitable spreads.
    pub async fn scan_opportunities(&mut self) -> Result<Vec<ArbitrageOpportunity>> {
        info!("🎯 Scanning for arbitrage opportunities");

        // Use the existing scan_arbitrage_opportunities method from DomeScraper
        // This method is already defined but needs proper implementation
        let dome_opportunities = self
            .dome_scraper
            .scan_arbitrage_opportunities(self.min_spread)
            .await
            .context("Failed to scan Dome API for arbitrage")?;

        debug!(
            "Found {} potential opportunities from Dome API",
            dome_opportunities.len()
        );

        // Filter and enhance opportunities
        let mut validated_opportunities = Vec::new();

        for dome_opp in dome_opportunities {
            match self.validate_and_enhance_opportunity(dome_opp).await {
                Ok(Some(opp)) => validated_opportunities.push(opp),
                Ok(None) => {} // Filtered out
                Err(e) => warn!("Failed to validate opportunity: {}", e),
            }
        }

        info!(
            "✅ Found {} validated arbitrage opportunities",
            validated_opportunities.len()
        );

        Ok(validated_opportunities)
    }

    /// Validate and enhance an arbitrage opportunity
    async fn validate_and_enhance_opportunity(
        &self,
        dome_opp: crate::scrapers::dome::ArbitrageOpportunity,
    ) -> Result<Option<ArbitrageOpportunity>> {
        // Check if spread meets minimum threshold
        if dome_opp.spread_pct < self.min_spread {
            debug!(
                "Spread {:.2}% below minimum {:.2}%",
                dome_opp.spread_pct * 100.0,
                self.min_spread * 100.0
            );
            return Ok(None);
        }

        // Calculate fee-adjusted profit
        // Assuming we trade 100 shares as baseline
        let shares = 100.0;
        let buy_price = dome_opp.spread_pct; // This is placeholder - would come from API
        let sell_price = buy_price + dome_opp.spread_pct;

        let (gross_profit, total_fees, net_profit, net_profit_pct) = self
            .fee_calculator
            .calculate_net_profit(buy_price, sell_price, shares);

        // Check if still profitable after fees
        if net_profit_pct < 0.0 {
            debug!("Opportunity not profitable after fees");
            return Ok(None);
        }

        let opportunity = ArbitrageOpportunity {
            id: format!("arb_{}", chrono::Utc::now().timestamp_millis()),
            polymarket_market: dome_opp.polymarket_market,
            kalshi_market: dome_opp.kalshi_market,
            polymarket_price: 0.0, // Would be fetched from actual market data
            kalshi_price: None,
            spread_pct: dome_opp.spread_pct,
            gross_profit_per_share: gross_profit / shares,
            net_profit_per_share: net_profit / shares,
            confidence: dome_opp.confidence,
            polymarket_liquidity: 0.0, // Would be fetched
            kalshi_volume: 0.0,        // Would be fetched
            estimated_execution_time_secs: self
                .fee_calculator
                .estimate_execution_time(1000.0, MIN_LIQUIDITY_USD),
            detected_at: chrono::Utc::now().to_rfc3339(),
        };

        Ok(Some(opportunity))
    }

    /// Calculate confidence score for arbitrage opportunity
    ///
    /// Factors:
    /// - Spread size (higher = more confident)
    /// - Liquidity (higher = more confident)
    /// - Volume (higher = more confident)
    /// - Time to expiry (closer = less confident)
    pub fn calculate_confidence(
        &self,
        spread_pct: f64,
        liquidity: f64,
        volume: f64,
        time_to_expiry_hours: Option<f64>,
    ) -> f64 {
        let mut confidence = 0.5;

        // Spread contribution (0-0.25)
        if spread_pct > 0.10 {
            confidence += 0.25;
        } else if spread_pct > 0.05 {
            confidence += 0.20;
        } else if spread_pct > 0.03 {
            confidence += 0.10;
        }

        // Liquidity contribution (0-0.20)
        if liquidity > 100000.0 {
            confidence += 0.20;
        } else if liquidity > 50000.0 {
            confidence += 0.15;
        } else if liquidity > 25000.0 {
            confidence += 0.10;
        }

        // Volume contribution (0-0.15)
        if volume > 50000.0 {
            confidence += 0.15;
        } else if volume > 10000.0 {
            confidence += 0.10;
        } else if volume > 5000.0 {
            confidence += 0.05;
        }

        // Time to expiry penalty (0-0.15)
        if let Some(hours) = time_to_expiry_hours {
            if hours < 6.0 {
                confidence -= 0.15; // Very risky
            } else if hours < 24.0 {
                confidence -= 0.10; // Risky
            } else if hours < 72.0 {
                confidence -= 0.05; // Slight risk
            }
        }

        // Clamp to valid range
        f64::max(0.3, f64::min(confidence, 0.95))
    }

    /// Generate execution plan for an arbitrage opportunity
    ///
    /// Creates a step-by-step plan with position sizing and expected profit
    pub async fn generate_execution_plan(
        &self,
        opportunity: &ArbitrageOpportunity,
    ) -> Result<ExecutionPlan> {
        // Get current bankroll from risk manager
        let risk_mgr = self.risk_manager.read().await;
        let bankroll = risk_mgr.get_current_bankroll();

        // Calculate position size
        let position_size_usd = self.fee_calculator.calculate_position_size(
            bankroll,
            opportunity.confidence,
            opportunity.spread_pct,
            0.25, // 25% Kelly
        );

        // Calculate shares based on position size
        // Assuming we buy on the cheaper platform
        let buy_price = opportunity
            .polymarket_price
            .min(opportunity.kalshi_price.unwrap_or(1.0));
        let sell_price = f64::max(
            opportunity.polymarket_price,
            opportunity.kalshi_price.unwrap_or(0.0),
        );
        let shares = position_size_usd / buy_price;

        // Leg 1: Buy on cheaper platform
        let (cheaper_platform, expensive_platform) =
            if opportunity.polymarket_price < opportunity.kalshi_price.unwrap_or(1.0) {
                ("Polymarket", "Kalshi")
            } else {
                ("Kalshi", "Polymarket")
            };

        let leg1 = TradeLeg {
            platform: cheaper_platform.to_string(),
            action: "BUY".to_string(),
            outcome: "YES".to_string(),
            shares,
            price: buy_price,
            total_cost_usd: shares * buy_price,
        };

        // Leg 2: Sell on expensive platform
        let leg2 = TradeLeg {
            platform: expensive_platform.to_string(),
            action: "SELL".to_string(),
            outcome: "YES".to_string(),
            shares,
            price: sell_price,
            total_cost_usd: shares * sell_price,
        };

        // Calculate expected profit
        let (_, _, net_profit, net_profit_pct) = self
            .fee_calculator
            .calculate_net_profit(buy_price, sell_price, shares);

        // Generate step-by-step instructions
        let execution_steps = vec![
            format!("1. Check current bankroll: ${:.2}", bankroll),
            format!(
                "2. Allocate ${:.2} ({:.1}% of bankroll)",
                position_size_usd,
                (position_size_usd / bankroll) * 100.0
            ),
            format!(
                "3. BUY {:.0} shares on {} @ ${:.4} = ${:.2}",
                leg1.shares, leg1.platform, leg1.price, leg1.total_cost_usd
            ),
            format!(
                "4. SELL {:.0} shares on {} @ ${:.4} = ${:.2}",
                leg2.shares, leg2.platform, leg2.price, leg2.total_cost_usd
            ),
            format!(
                "5. Expected net profit: ${:.2} ({:.2}%)",
                net_profit,
                net_profit_pct * 100.0
            ),
            format!("6. Update bankroll to: ${:.2}", bankroll + net_profit),
        ];

        Ok(ExecutionPlan {
            opportunity_id: opportunity.id.clone(),
            leg1,
            leg2,
            expected_profit_usd: net_profit,
            expected_profit_pct: net_profit_pct,
            risk_score: opportunity.confidence,
            execution_steps,
        })
    }

    /// Quick check if a price spread is worth investigating
    pub fn is_worth_investigating(&self, poly_price: f64, kalshi_price: f64) -> bool {
        let spread = (poly_price - kalshi_price).abs();
        let spread_pct = spread / poly_price.min(kalshi_price);

        spread_pct >= self.min_spread
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_confidence_calculation() {
        let risk_manager = Arc::new(RwLock::new(RiskManager::new(10000.0, 0.25)));
        let engine = ArbitrageEngine::new(
            PolymarketScraper::new(),
            DomeScraper::new("test_key".to_string()),
            risk_manager,
        );

        // High confidence scenario
        let confidence_high = engine.calculate_confidence(
            0.12,        // 12% spread
            150000.0,    // High liquidity
            60000.0,     // High volume
            Some(168.0), // 1 week to expiry
        );
        assert!(confidence_high > 0.80);

        // Low confidence scenario
        let confidence_low = engine.calculate_confidence(
            0.04,      // 4% spread
            30000.0,   // Low liquidity
            3000.0,    // Low volume
            Some(5.0), // 5 hours to expiry
        );
        assert!(confidence_low < 0.60);

        println!("High confidence: {:.2}", confidence_high);
        println!("Low confidence: {:.2}", confidence_low);
    }

    #[test]
    fn test_worth_investigating() {
        let risk_manager = Arc::new(RwLock::new(RiskManager::new(10000.0, 0.25)));
        let engine = ArbitrageEngine::new(
            PolymarketScraper::new(),
            DomeScraper::new("test_key".to_string()),
            risk_manager,
        );

        // 7% spread - worth it
        assert!(engine.is_worth_investigating(0.65, 0.58));

        // 2% spread - not worth it
        assert!(!engine.is_worth_investigating(0.60, 0.59));
    }
}

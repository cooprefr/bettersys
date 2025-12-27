//! Trade Executor
//!
//! Executes trades on Polymarket via DomeAPI or direct CLOB
//! Uses Kelly criterion for position sizing

use anyhow::Result;
use tracing::{error, info, warn};

use super::kelly::{calculate_kelly_position, KellyParams, KellyResult};
use super::user_accounts::{TradeRecord, UserAccount, UserAccountsDB};
use crate::models::MarketSignal;

/// Trade execution result
#[derive(Debug, Clone)]
pub struct TradeExecutionResult {
    pub success: bool,
    pub order_id: Option<String>,
    pub filled_size: f64,
    pub filled_price: f64,
    pub error: Option<String>,
}

/// Trade executor for automated Kelly-based trading
pub struct TradeExecutor {
    accounts_db: UserAccountsDB,
    dome_api_key: String,
    dry_run: bool, // If true, don't actually execute trades
}

impl TradeExecutor {
    pub fn new(accounts_db: UserAccountsDB, dome_api_key: String, dry_run: bool) -> Self {
        Self {
            accounts_db,
            dome_api_key,
            dry_run,
        }
    }

    /// Process a new signal for all auto-trade users
    pub async fn process_signal(&self, signal: &MarketSignal) -> Result<Vec<TradeExecutionResult>> {
        let mut results = Vec::new();

        // Get all users with auto-trade enabled
        let users = self.accounts_db.get_auto_trade_users().await?;

        if users.is_empty() {
            return Ok(results);
        }

        info!(
            "Processing signal {} for {} auto-trade users",
            signal.id,
            users.len()
        );

        for user in users {
            match self.execute_for_user(&user, signal).await {
                Ok(result) => {
                    if result.success {
                        info!(
                            "Trade executed for user {}: ${:.2} at {:.4}",
                            user.wallet_address, result.filled_size, result.filled_price
                        );
                    }
                    results.push(result);
                }
                Err(e) => {
                    error!("Trade failed for user {}: {}", user.wallet_address, e);
                    results.push(TradeExecutionResult {
                        success: false,
                        order_id: None,
                        filled_size: 0.0,
                        filled_price: 0.0,
                        error: Some(e.to_string()),
                    });
                }
            }
        }

        Ok(results)
    }

    /// Execute trade for a single user based on signal
    async fn execute_for_user(
        &self,
        user: &UserAccount,
        signal: &MarketSignal,
    ) -> Result<TradeExecutionResult> {
        // Calculate Kelly position
        let kelly_params = KellyParams {
            bankroll: user.balance_usdc,
            kelly_fraction: user.kelly_fraction,
            max_position_pct: user.max_position_pct,
            min_position_usd: 1.0,
        };

        let kelly_result = calculate_kelly_position(
            signal.confidence,
            signal.details.current_price,
            &kelly_params,
        );

        // Check if we should trade
        if !kelly_result.should_trade {
            return Ok(TradeExecutionResult {
                success: false,
                order_id: None,
                filled_size: 0.0,
                filled_price: 0.0,
                error: kelly_result.skip_reason,
            });
        }

        info!(
            "Kelly recommends ${:.2} position for user {} (edge: {:.1}%, fraction: {:.1}%)",
            kelly_result.position_size_usd,
            user.wallet_address,
            kelly_result.edge * 100.0,
            kelly_result.actual_fraction * 100.0
        );

        // Determine trade direction
        let (side, outcome) = self.determine_trade_direction(signal);

        // Record trade intent
        let trade_record = TradeRecord {
            id: 0, // Will be set by DB
            user_id: user.id,
            signal_id: signal.id.clone(),
            market_slug: signal.market_slug.clone(),
            side: side.clone(),
            outcome: outcome.clone(),
            entry_price: signal.details.current_price,
            position_size: kelly_result.position_size_usd,
            kelly_fraction_used: kelly_result.actual_fraction,
            status: "pending".to_string(),
            exit_price: None,
            pnl: None,
            created_at: chrono::Utc::now(),
            closed_at: None,
        };

        let trade_id = self.accounts_db.record_trade(&trade_record).await?;

        // Execute trade (or simulate in dry run)
        if self.dry_run {
            warn!(
                "DRY RUN: Would execute {} {} {} shares at ${:.4}",
                side, kelly_result.position_size_usd, outcome, signal.details.current_price
            );

            return Ok(TradeExecutionResult {
                success: true,
                order_id: Some(format!("dry_run_{}", trade_id)),
                filled_size: kelly_result.position_size_usd,
                filled_price: signal.details.current_price,
                error: None,
            });
        }

        // TODO: Actual trade execution via DomeAPI
        // This requires:
        // 1. User to have signed authorization for the trading bot
        // 2. DomeAPI trading endpoint integration
        // 3. Order signing and submission

        // For now, return as if pending execution
        Ok(TradeExecutionResult {
            success: true,
            order_id: Some(format!("pending_{}", trade_id)),
            filled_size: kelly_result.position_size_usd,
            filled_price: signal.details.current_price,
            error: Some(
                "Trade execution not yet implemented - pending DomeAPI integration".to_string(),
            ),
        })
    }

    /// Determine trade direction from signal
    fn determine_trade_direction(&self, signal: &MarketSignal) -> (String, String) {
        let action = signal.details.recommended_action.to_uppercase();

        // Parse action to get side (BUY/SELL) and outcome (YES/NO)
        let side = if action.contains("BUY") {
            "BUY"
        } else if action.contains("SELL") {
            "SELL"
        } else {
            "BUY" // Default to BUY
        };

        // Infer YES/NO from price (price >= 0.5 means YES shares are favored)
        let outcome = if signal.details.current_price >= 0.5 {
            "YES"
        } else {
            "NO"
        };

        (side.to_string(), outcome.to_string())
    }
}

/// Configuration for trade executor
#[derive(Debug, Clone)]
pub struct TradeExecutorConfig {
    pub dome_api_key: String,
    pub accounts_db_path: String,
    pub dry_run: bool,
    pub default_kelly_fraction: f64,
    pub max_position_pct: f64,
    pub min_position_usd: f64,
}

impl Default for TradeExecutorConfig {
    fn default() -> Self {
        Self {
            dome_api_key: String::new(),
            accounts_db_path: "betterbot_accounts.db".to_string(),
            dry_run: true, // Safe default
            default_kelly_fraction: 0.25,
            max_position_pct: 0.10,
            min_position_usd: 1.0,
        }
    }
}

//! Latency Arbitrage Backtest - REAL DATA ONLY
//!
//! Backtests the latency arbitrage strategy using actual Polymarket order data
//! from the signals database. No synthetic data.
//!
//! Usage:
//!   cargo run --bin latency_arb_backtest_real -- [OPTIONS]
//!
//! Options:
//!   --bankroll <USD>     Initial bankroll (default: 10000)
//!   --min-edge <PCT>     Minimum effective edge (default: 0.01)
//!   --kelly <FRAC>       Kelly fraction (default: 0.10)
//!   --max-pos <PCT>      Max position size as % of bankroll (default: 0.05)
//!   --fee <PCT>          Fee rate (default: 0.02)
//!   --db <PATH>          Database path (default: betterbot_signals.db)
//!   --asset <NAME>       Filter: btc, eth, all (default: all)

use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::env;

/// Real order from signal_context with entry/exit pricing
#[derive(Debug, Clone)]
struct RealOrder {
    signal_id: String,
    timestamp: i64,
    market_slug: String,
    user: String,
    side: String,            // BUY or SELL
    outcome: String,         // Up, Down, Yes, No
    order_price: f64,        // Price they paid
    entry_mid: f64,          // Market mid at entry time
    exit_price: Option<f64>, // Price shortly after (for PnL calc)
    shares: f64,
}

/// Backtest configuration
#[derive(Debug, Clone)]
struct Config {
    bankroll: f64,
    min_edge: f64,
    kelly_fraction: f64,
    max_position_pct: f64,
    fee_rate: f64,
}

/// Per-market state
#[derive(Debug, Clone, Default)]
struct MarketState {
    position: f64,   // Shares held
    avg_entry: f64,  // Average entry price
    total_cost: f64, // Total cost basis
}

/// Backtest results
#[derive(Debug, Clone, Default)]
struct Results {
    total_orders: u64,
    opportunities: u64, // Orders where we detected edge
    trades_taken: u64,
    total_volume: f64,
    total_fees: f64,
    realized_pnl: f64,
    unrealized_pnl: f64,
    gross_profit: f64,
    gross_loss: f64,
    wins: u64,
    losses: u64,
    max_drawdown: f64,
    peak_equity: f64,
    avg_edge: f64,
    edge_sum: f64,
}

impl Results {
    fn win_rate(&self) -> f64 {
        let total = self.wins + self.losses;
        if total == 0 {
            0.0
        } else {
            self.wins as f64 / total as f64
        }
    }

    fn profit_factor(&self) -> f64 {
        if self.gross_loss.abs() < 0.01 {
            if self.gross_profit > 0.0 {
                f64::INFINITY
            } else {
                0.0
            }
        } else {
            self.gross_profit / self.gross_loss.abs()
        }
    }
}

fn main() {
    let args: Vec<String> = env::args().collect();

    let config = Config {
        bankroll: parse_arg(&args, "--bankroll", 10_000.0),
        min_edge: parse_arg(&args, "--min-edge", 0.01),
        kelly_fraction: parse_arg(&args, "--kelly", 0.10),
        max_position_pct: parse_arg(&args, "--max-pos", 0.05),
        fee_rate: parse_arg(&args, "--fee", 0.02),
    };

    let db_path = parse_str_arg(&args, "--db", "betterbot_signals.db");
    let asset_filter = parse_str_arg(&args, "--asset", "all");

    println!("=== Latency Arbitrage Backtest (REAL DATA) ===");
    println!("Bankroll:      ${:.0}", config.bankroll);
    println!("Min Edge:      {:.1}%", config.min_edge * 100.0);
    println!("Kelly Frac:    {:.0}%", config.kelly_fraction * 100.0);
    println!("Max Position:  {:.1}%", config.max_position_pct * 100.0);
    println!("Fee Rate:      {:.1}%", config.fee_rate * 100.0);
    println!("Database:      {}", db_path);
    println!("Asset Filter:  {}", asset_filter);
    println!();

    // Load real orders from database
    let orders = load_real_orders(db_path, asset_filter);
    if orders.is_empty() {
        eprintln!("No orders loaded!");
        return;
    }

    println!("Loaded {} real orders", orders.len());
    println!();

    // Run backtest
    let results = run_backtest(&config, &orders);

    // Print results
    print_results(&config, &results);
}

fn load_real_orders(db_path: &str, asset_filter: &str) -> Vec<RealOrder> {
    let conn = match Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_ONLY) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Failed to open database: {}", e);
            return vec![];
        }
    };

    // Use dome_order_events directly - it's indexed and fast
    let where_clause = match asset_filter {
        "btc" => "WHERE market_slug LIKE 'btc-updown-15m%'",
        "eth" => "WHERE market_slug LIKE 'eth-updown-15m%'",
        "sol" => "WHERE market_slug LIKE 'sol-updown-15m%'",
        _ => "WHERE market_slug LIKE '%-updown-15m%'", // all 15m markets
    };

    let query = format!(
        r#"
        SELECT 
            order_hash,
            timestamp,
            market_slug,
            user,
            json_extract(payload_json, '$.side') as side,
            json_extract(payload_json, '$.token_label') as outcome,
            json_extract(payload_json, '$.price') as order_price,
            json_extract(payload_json, '$.shares_normalized') as shares
        FROM dome_order_events
        {}
        ORDER BY timestamp ASC
    "#,
        where_clause
    );

    let mut stmt = match conn.prepare(&query) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Failed to prepare query: {}", e);
            return vec![];
        }
    };

    let orders: Vec<RealOrder> = stmt
        .query_map([], |row| {
            let order_price: f64 = row.get::<_, f64>(6).unwrap_or(0.5);
            Ok(RealOrder {
                signal_id: row.get(0)?,
                timestamp: row.get::<_, i64>(1).unwrap_or(0),
                market_slug: row.get::<_, String>(2).unwrap_or_default(),
                user: row.get::<_, String>(3).unwrap_or_default(),
                side: row.get::<_, String>(4).unwrap_or_default(),
                outcome: row.get::<_, String>(5).unwrap_or_default(),
                order_price,
                entry_mid: order_price, // Use order price as mid estimate
                exit_price: None,       // Will compute from subsequent orders
                shares: row.get::<_, f64>(7).unwrap_or(0.0),
            })
        })
        .ok()
        .map(|iter| iter.filter_map(|r| r.ok()).collect())
        .unwrap_or_default();

    // Print some stats about the data
    if !orders.is_empty() {
        let btc_count = orders
            .iter()
            .filter(|o| o.market_slug.starts_with("btc-"))
            .count();
        let eth_count = orders
            .iter()
            .filter(|o| o.market_slug.starts_with("eth-"))
            .count();
        let unique_markets: std::collections::HashSet<_> =
            orders.iter().map(|o| &o.market_slug).collect();
        let unique_users: std::collections::HashSet<_> = orders.iter().map(|o| &o.user).collect();

        println!("Data Summary:");
        println!("  BTC orders:      {}", btc_count);
        println!("  ETH orders:      {}", eth_count);
        println!("  Unique markets:  {}", unique_markets.len());
        println!("  Unique traders:  {}", unique_users.len());

        // Date range
        if let (Some(first), Some(last)) = (orders.first(), orders.last()) {
            println!(
                "  Date range:      {} to {}",
                timestamp_to_date(first.timestamp),
                timestamp_to_date(last.timestamp)
            );
        }
    }

    orders
}

fn run_backtest(config: &Config, orders: &[RealOrder]) -> Results {
    let mut results = Results::default();
    let mut equity = config.bankroll;
    results.peak_equity = equity;

    // Group orders by market to compute price evolution
    let mut market_prices: HashMap<String, Vec<(i64, f64, String)>> = HashMap::new();
    for order in orders {
        market_prices
            .entry(order.market_slug.clone())
            .or_default()
            .push((order.timestamp, order.order_price, order.outcome.clone()));
    }

    // Build a price history for each market/outcome to estimate "fair value"
    // Fair value = rolling average of recent trade prices
    let mut fair_values: HashMap<(String, String), f64> = HashMap::new();
    let mut price_history: HashMap<(String, String), Vec<f64>> = HashMap::new();

    for (i, order) in orders.iter().enumerate() {
        results.total_orders += 1;

        if order.order_price <= 0.01 || order.order_price >= 0.99 {
            continue;
        }

        let key = (order.market_slug.clone(), order.outcome.clone());

        // Get current fair value estimate (rolling avg of last N prices)
        let history = price_history.entry(key.clone()).or_insert_with(Vec::new);
        let fair_value = if history.len() >= 3 {
            history.iter().rev().take(10).sum::<f64>() / history.len().min(10) as f64
        } else {
            order.order_price // Not enough history, use order price
        };

        // Update price history
        history.push(order.order_price);
        if history.len() > 50 {
            history.remove(0);
        }
        fair_values.insert(key.clone(), order.order_price);

        // Compute edge: deviation from fair value
        // If we BUY below fair value, we have positive edge
        // If we SELL above fair value, we have positive edge
        let raw_edge = if order.side == "BUY" {
            fair_value - order.order_price
        } else {
            order.order_price - fair_value
        };

        // Effective edge after round-trip fees
        let effective_edge = raw_edge - config.fee_rate * 2.0;

        if effective_edge.abs() < 0.001 {
            continue;
        }

        results.opportunities += 1;

        // Only trade if edge exceeds threshold
        if effective_edge < config.min_edge {
            continue;
        }

        results.edge_sum += effective_edge;

        // Find exit price for PnL calculation (look ahead)
        let exit_price = find_exit_price(orders, i, &order.market_slug, &order.outcome);
        let exit_price = exit_price.unwrap_or(order.order_price);

        // Kelly sizing based on edge
        // Kelly fraction: edge / (1 - p) where p is probability (price)
        let odds = (1.0 / order.order_price.max(0.01)) - 1.0;
        let kelly_bet = (effective_edge * odds).max(0.0);
        let position_frac = (kelly_bet * config.kelly_fraction).min(config.max_position_pct);
        let position_usd = (position_frac * equity).min(500.0); // Cap single trade at $500

        if position_usd < 5.0 || !position_usd.is_finite() {
            continue;
        }

        // Compute trade size in USD and resulting shares
        let shares = position_usd / order.order_price.max(0.01);
        let cost = position_usd; // We spend position_usd
        let entry_fee = cost * config.fee_rate;

        // Exit value based on price change
        let exit_value = shares * exit_price;
        let exit_fee = exit_value * config.fee_rate;
        let total_fees = entry_fee + exit_fee;

        // Sanity check on values
        if !cost.is_finite() || !exit_value.is_finite() || cost > 10000.0 || exit_value > 20000.0 {
            continue;
        }

        results.trades_taken += 1;
        results.total_volume += cost;
        results.total_fees += total_fees;

        // PnL calculation
        let trade_pnl = if order.side == "BUY" {
            exit_value - cost - total_fees
        } else {
            cost - exit_value - total_fees // Short: profit from price decrease
        };

        // Sanity check PnL
        if !trade_pnl.is_finite() || trade_pnl.abs() > 1000.0 {
            continue;
        }

        results.realized_pnl += trade_pnl;

        if trade_pnl > 0.0 {
            results.gross_profit += trade_pnl;
            results.wins += 1;
        } else {
            results.gross_loss += trade_pnl.abs();
            results.losses += 1;
        }

        equity += trade_pnl;

        // Track drawdown
        if equity > results.peak_equity {
            results.peak_equity = equity;
        }
        let drawdown = results.peak_equity - equity;
        if drawdown > results.max_drawdown {
            results.max_drawdown = drawdown;
        }
    }

    if results.opportunities > 0 {
        results.avg_edge = results.edge_sum / results.opportunities as f64;
    }

    results
}

/// Find exit price by looking at subsequent orders in the same market/outcome
fn find_exit_price(
    orders: &[RealOrder],
    start_idx: usize,
    market: &str,
    outcome: &str,
) -> Option<f64> {
    let entry_order = &orders[start_idx];
    let entry_time = entry_order.timestamp;

    // Look for orders 1-15 minutes later in the same market
    for order in orders.iter().skip(start_idx + 1) {
        if order.market_slug != market {
            continue;
        }
        if order.outcome != outcome {
            continue;
        }

        let time_diff = order.timestamp - entry_time;

        // Exit window: 1 minute to 15 minutes after entry
        if time_diff >= 60 && time_diff <= 900 {
            return Some(order.order_price);
        }

        // Stop looking if too far ahead
        if time_diff > 1800 {
            break;
        }
    }

    None
}

fn print_results(config: &Config, results: &Results) {
    println!();
    println!("=== Backtest Results ===");
    println!();

    println!("Order Analysis:");
    println!("  Total orders scanned:  {}", results.total_orders);
    println!(
        "  Opportunities found:   {} ({:.1}%)",
        results.opportunities,
        100.0 * results.opportunities as f64 / results.total_orders.max(1) as f64
    );
    println!("  Trades executed:       {}", results.trades_taken);
    println!("  Average edge:          {:.2}%", results.avg_edge * 100.0);
    println!();

    println!("Performance:");
    println!("  Total volume:          ${:.2}", results.total_volume);
    println!("  Total fees:            ${:.2}", results.total_fees);
    println!("  Realized PnL:          ${:.2}", results.realized_pnl);
    println!("  Gross profit:          ${:.2}", results.gross_profit);
    println!("  Gross loss:            ${:.2}", results.gross_loss);
    println!();

    println!("Statistics:");
    println!(
        "  Win rate:              {:.1}% ({}/{})",
        results.win_rate() * 100.0,
        results.wins,
        results.wins + results.losses
    );
    println!("  Profit factor:         {:.2}", results.profit_factor());
    println!("  Max drawdown:          ${:.2}", results.max_drawdown);
    println!();

    let roi = results.realized_pnl / config.bankroll * 100.0;
    println!("=== Summary ===");
    println!("  Return on Investment:  {:.2}%", roi);
    if results.trades_taken > 0 {
        println!(
            "  Avg PnL per trade:     ${:.2}",
            results.realized_pnl / results.trades_taken as f64
        );
        println!(
            "  Avg trade size:        ${:.2}",
            results.total_volume / results.trades_taken as f64
        );
    }
}

fn parse_arg(args: &[String], flag: &str, default: f64) -> f64 {
    for i in 0..args.len().saturating_sub(1) {
        if args[i] == flag {
            if let Ok(v) = args[i + 1].parse::<f64>() {
                return v;
            }
        }
    }
    default
}

fn parse_str_arg<'a>(args: &'a [String], flag: &str, default: &'a str) -> &'a str {
    for i in 0..args.len().saturating_sub(1) {
        if args[i] == flag {
            return &args[i + 1];
        }
    }
    default
}

fn timestamp_to_date(ts: i64) -> String {
    use std::time::{Duration, UNIX_EPOCH};
    let d = UNIX_EPOCH + Duration::from_secs(ts as u64);
    format!("{:?}", d).chars().take(20).collect()
}

use crate::models::{Signal, SignalType};
use crate::scrapers::hashdive::{WhaleTrade, OhlcvBar};
use crate::scrapers::polymarket::PolymarketEvent;
use chrono::{DateTime, Utc, Duration as ChronoDuration};
use std::collections::HashMap;

/// Detect whale trade signals from Hashdive data
pub fn detect_whale_trade_signal(trade: &WhaleTrade) -> Signal {
    let market_name = trade.market_title.clone()
        .unwrap_or_else(|| "Unknown market".to_string());
    
    let outcome = trade.outcome.clone()
        .unwrap_or_else(|| "Unknown".to_string());
    
    let description = format!(
        "{} whale trade: ${:.0} {} on '{}' outcome ({:.2}% price)",
        trade.side,
        trade.usd_amount,
        trade.side.to_lowercase(),
        outcome,
        trade.price * 100.0
    );
    
    // Higher amounts = higher confidence
    let confidence = match trade.usd_amount {
        x if x >= 100_000.0 => 95.0,
        x if x >= 50_000.0 => 85.0,
        x if x >= 25_000.0 => 75.0,
        x if x >= 10_000.0 => 65.0,
        _ => 55.0,
    };
    
    tracing::info!(
        "🐋 Whale trade detected: ${:.0} {} on {} (confidence: {:.0}%)",
        trade.usd_amount,
        trade.side,
        market_name,
        confidence
    );
    
    Signal::new(
        SignalType::InsiderEdge,
        "Hashdive".to_string(),
        description,
        confidence,
    )
    .with_market(market_name)
    .with_metadata(serde_json::to_string(&serde_json::json!({
        "transaction_hash": trade.transaction_hash,
        "user_address": trade.user_address,
        "asset_id": trade.asset_id,
        "usd_amount": trade.usd_amount,
        "price": trade.price,
        "side": trade.side,
    })).unwrap())
}

/// Detect price movement signals from OHLCV data
/// Returns a signal if there's significant price movement
#[allow(dead_code)] // Reserved for Day 3+ price movement features
pub fn detect_price_movement_signal(
    bars: &[OhlcvBar],
    asset_id: &str,
    market_name: &str,
) -> Option<Signal> {
    if bars.len() < 2 {
        return None;
    }
    
    // Compare last bar to previous bar
    let latest = &bars[bars.len() - 1];
    let previous = &bars[bars.len() - 2];
    
    let price_change = ((latest.close - previous.close) / previous.close) * 100.0;
    let volume_change = ((latest.volume - previous.volume) / previous.volume) * 100.0;
    
    // Significant if: price moved >5% OR (price moved >3% AND volume increased >50%)
    let is_significant = price_change.abs() > 5.0 || 
        (price_change.abs() > 3.0 && volume_change > 50.0);
    
    if !is_significant {
        return None;
    }
    
    let direction = if price_change > 0.0 { "UP" } else { "DOWN" };
    
    let description = format!(
        "Price moved {} {:.1}% (from {:.2}% to {:.2}%) with {:.1}% volume increase on '{}'",
        direction,
        price_change.abs(),
        previous.close * 100.0,
        latest.close * 100.0,
        volume_change,
        market_name
    );
    
    // Confidence based on magnitude
    let confidence = ((price_change.abs() * 10.0 + volume_change / 2.0).min(95.0)) as f32;
    
    tracing::info!(
        "📈 Price movement detected: {} {:.1}% on {} (confidence: {:.0}%)",
        direction,
        price_change.abs(),
        market_name,
        confidence
    );
    
    Some(
        Signal::new(
            SignalType::Arbitrage, // Price movements = potential arbitrage
            "Hashdive-OHLCV".to_string(),
            description,
            confidence,
        )
        .with_market(market_name.to_string())
        .with_metadata(serde_json::to_string(&serde_json::json!({
            "asset_id": asset_id,
            "price_change_pct": price_change,
            "volume_change_pct": volume_change,
            "open": latest.open,
            "high": latest.high,
            "low": latest.low,
            "close": latest.close,
            "volume": latest.volume,
        })).unwrap())
    )
}

/// Signal 4: Price Deviation (Binary Arbitrage)
/// Detects when Yes + No prices don't equal $1.00 (pure arbitrage opportunity)
pub fn detect_price_deviation(event: &PolymarketEvent) -> Vec<Signal> {
    let mut signals = Vec::new();
    
    for market in &event.markets {
        // Parse outcome prices
        let prices: Result<Vec<f64>, _> = serde_json::from_str::<Vec<String>>(&market.outcome_prices)
            .unwrap_or_default()
            .iter()
            .map(|p| p.parse::<f64>())
            .collect();
        
        let prices = match prices {
            Ok(p) if p.len() >= 2 => p,
            _ => continue,
        };
        
        let price_yes = prices[0];
        let price_no = prices[1];
        let total = price_yes + price_no;
        let deviation = (1.0 - total).abs();
        let deviation_pct = deviation * 100.0;
        
        // Only signal if deviation > 2%
        if deviation_pct < 2.0 {
            continue;
        }
        
        let confidence = (30.0 + deviation_pct * 20.0).min(95.0) as f32;
        
        let action = if total < 0.98 {
            "BUY BOTH"
        } else {
            "SELL BOTH"
        };
        
        let profit_pct = (deviation / total) * 100.0;
        
        let description = format!(
            "ARBITRAGE: {:.1}% price deviation on '{}' | {} for {:.1}% profit | Yes=${:.3} No=${:.3}",
            deviation_pct,
            event.title,
            action,
            profit_pct,
            price_yes,
            price_no
        );
        
        tracing::info!(
            "💎 Price deviation detected: {:.1}% on {} (confidence: {:.0}%)",
            deviation_pct,
            event.title,
            confidence
        );
        
        signals.push(
            Signal::new(
                SignalType::PriceDeviation,
                "Polymarket-Deviation".to_string(),
                description,
                confidence,
            )
            .with_market(event.title.clone())
            .with_metadata(serde_json::to_string(&serde_json::json!({
                "price_yes": price_yes,
                "price_no": price_no,
                "total": total,
                "deviation_pct": deviation_pct,
                "profit_pct": profit_pct,
                "action": action,
                "market_slug": event.slug,
            })).unwrap())
        );
    }
    
    signals
}

/// Signal 5: Whale Cluster (Smart Money Consensus)
/// Detects when 3+ whales trade the same direction on same market within 1 hour
pub fn detect_whale_cluster(trades: &[WhaleTrade], time_window_hours: i64) -> Vec<Signal> {
    let now = Utc::now();
    let cutoff = now - ChronoDuration::hours(time_window_hours);
    
    // Group by asset_id and direction
    let mut clusters: HashMap<(String, String), Vec<&WhaleTrade>> = HashMap::new();
    
    for trade in trades {
        if trade.usd_amount < 10_000.0 {
            continue; // Only whales ($10k+)
        }
        
        // Parse timestamp and filter by time window
        let trade_time = match DateTime::parse_from_rfc3339(&trade.timestamp) {
            Ok(dt) => dt.with_timezone(&Utc),
            Err(_) => continue, // Skip if can't parse timestamp
        };
        
        if trade_time < cutoff {
            continue; // Outside time window
        }
        
        let key = (trade.asset_id.clone(), trade.side.clone());
        clusters.entry(key).or_insert_with(Vec::new).push(trade);
    }
    
    let mut signals = Vec::new();
    
    for ((asset_id, direction), cluster_trades) in clusters {
        let whale_count = cluster_trades.len();
        
        if whale_count < 3 {
            continue; // Need 3+ whales
        }
        
        let confidence = match whale_count {
            3 => 70.0,
            4 => 80.0,
            5 => 90.0,
            _ => 95.0,
        };
        
        let total_volume: f64 = cluster_trades.iter()
            .map(|t| t.usd_amount)
            .sum();
        
        let market_name = cluster_trades[0].market_title
            .clone()
            .unwrap_or_else(|| "Unknown".to_string());
        
        let description = format!(
            "WHALE CLUSTER: {} whales {} ${:.0} total on '{}' (within {}hr)",
            whale_count,
            direction.to_lowercase(),
            total_volume,
            market_name,
            time_window_hours
        );
        
        tracing::info!(
            "🎯 Whale cluster detected: {} whales {} ${:.0} on {} (confidence: {:.0}%)",
            whale_count,
            direction,
            total_volume,
            market_name,
            confidence
        );
        
        signals.push(
            Signal::new(
                SignalType::WhaleCluster,
                "Hashdive-Cluster".to_string(),
                description,
                confidence,
            )
            .with_market(market_name)
            .with_metadata(serde_json::to_string(&serde_json::json!({
                "whale_count": whale_count,
                "total_volume": total_volume,
                "direction": direction,
                "asset_id": asset_id,
                "time_window_hours": time_window_hours,
            })).unwrap())
        );
    }
    
    signals
}

/// Signal 6: Market Expiry Edge
/// Detects markets closing within 4 hours and recommends 10% portfolio bet on dominant side
pub fn detect_market_expiry_edge(events: &[PolymarketEvent]) -> Vec<Signal> {
    let now = Utc::now();
    let cutoff = now + ChronoDuration::hours(4);
    
    let mut signals = Vec::new();
    
    for event in events {
        if event.closed {
            continue; // Skip already closed markets
        }
        
        // Parse end date
        let end_date = match &event.end_date {
            Some(date_str) => match DateTime::parse_from_rfc3339(date_str) {
                Ok(dt) => dt.with_timezone(&Utc),
                Err(_) => continue,
            },
            None => continue,
        };
        
        // Check if market expires within 4 hours
        if end_date > cutoff {
            continue; // Too far away
        }
        
        if end_date < now {
            continue; // Already expired
        }
        
        // Find dominant outcome (highest price)
        for market in &event.markets {
            let prices: Result<Vec<f64>, _> = serde_json::from_str::<Vec<String>>(&market.outcome_prices)
                .unwrap_or_default()
                .iter()
                .map(|p| p.parse::<f64>())
                .collect();
            
            let prices = match prices {
                Ok(p) if p.len() >= 2 => p,
                _ => continue,
            };
            
            let outcomes: Vec<String> = serde_json::from_str(&market.outcomes)
                .unwrap_or_default();
            
            if outcomes.len() != prices.len() {
                continue;
            }
            
            // Find dominant outcome
            let (dominant_idx, dominant_price) = prices.iter()
                .enumerate()
                .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap())
                .unwrap();
            
            let dominant_outcome = &outcomes[dominant_idx];
            let dominant_pct = dominant_price * 100.0;
            
            // Only signal if dominant side >= 60% (clear leader)
            if dominant_pct < 60.0 {
                continue;
            }
            
            let hours_left = (end_date - now).num_minutes() as f64 / 60.0;
            
            let description = format!(
                "EXPIRY EDGE: '{}' closes in {:.1}hrs | '{}' @ {:.1}% is dominant | Recommend 10% portfolio bet on '{}' (95% historical accuracy)",
                event.title,
                hours_left,
                dominant_outcome,
                dominant_pct,
                dominant_outcome
            );
            
            tracing::info!(
                "⏰ Market expiry edge detected: {} closes in {:.1}hrs, '{}' @ {:.1}%",
                event.title,
                hours_left,
                dominant_outcome,
                dominant_pct
            );
            
            signals.push(
                Signal::new(
                    SignalType::ExpiryEdge,
                    "Polymarket-Expiry".to_string(),
                    description,
                    95.0, // 95% confidence based on historical analysis
                )
                .with_market(event.title.clone())
                .with_metadata(serde_json::to_string(&serde_json::json!({
                    "dominant_outcome": dominant_outcome,
                    "dominant_price": dominant_price,
                    "dominant_pct": dominant_pct,
                    "hours_left": hours_left,
                    "recommendation": "10% portfolio bet",
                    "market_slug": event.slug,
                    "end_date": end_date.to_rfc3339(),
                })).unwrap())
            );
        }
    }
    
    signals
}

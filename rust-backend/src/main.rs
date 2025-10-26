mod api;
mod models;
mod scrapers;
mod signals;

use anyhow::Result;
use models::Config;
use scrapers::{HashdiveClient, PolymarketClient};
use signals::{detect_whale_trade_signal, detect_whale_cluster, detect_price_deviation, detect_market_expiry_edge, Database};
use std::sync::Arc;
use std::time::Duration;
use tokio::time;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "betterbot=debug,info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    tracing::info!("🚀 BetterBot v2 starting up...");

    // Load configuration
    let config = Config::from_env()?;
    tracing::info!("📋 Config loaded:");
    tracing::info!("  - Database: {}", config.database_path);
    tracing::info!("  - API Port: {}", config.port);
    tracing::info!("  - Twitter accounts: {:?}", config.twitter_accounts);
    tracing::info!("  - Twitter scrape interval: {}s", config.twitter_scrape_interval);
    tracing::info!("  - Hashdive scrape interval: {}s", config.hashdive_scrape_interval);
    tracing::info!("  - Hashdive API: {}", if config.hashdive_api_key.is_some() { "✓ Configured" } else { "✗ Not configured" });

    // Initialize database
    let db = Arc::new(Database::new(&config.database_path)?);
    let signal_count = db.count_signals()?;
    tracing::info!("💾 Database ready ({} signals stored)", signal_count);

    // Initialize scrapers
    let polymarket_client = PolymarketClient::new();
    tracing::info!("📊 Polymarket client initialized");

    let hashdive_client = if let Some(api_key) = &config.hashdive_api_key {
        let client = HashdiveClient::new(api_key.clone());
        
        // Try to check API usage, but DON'T fail init if it errors (resilience)
        match client.check_usage().await {
            Ok(usage) => {
                let remaining = 1000 - usage.credits_used; // Free tier has 1000 credits/month
                tracing::info!(
                    "🔑 Hashdive API connected: {} credits used, ~{} remaining (1000/month limit)",
                    usage.credits_used,
                    remaining
                );
            }
            Err(e) => {
                // Log warning but proceed - usage endpoint may be unstable (beta API)
                tracing::warn!("⚠️  Could not check Hashdive usage ({}), but client initialized. Will track usage in-loop.", e);
            }
        }
        
        // Return client regardless of usage check - let scraping attempts fail/retry independently
        Some(client)
    } else {
        tracing::warn!("⚠️  Hashdive API key not configured. Set HASHDIVE_API_KEY to enable whale tracking.");
        None
    };

    // Start API server in background
    let api_db = Arc::clone(&db);
    let api_port = config.port;
    tokio::spawn(async move {
        let app = api::create_router(api_db);
        
        let listener = match tokio::net::TcpListener::bind(format!("0.0.0.0:{}", api_port)).await {
            Ok(l) => l,
            Err(e) => {
                tracing::error!("Failed to bind API server: {}", e);
                return;
            }
        };
        
        tracing::info!("🌐 API server listening on http://0.0.0.0:{}", api_port);
        tracing::info!("  - Health: http://localhost:{}/health", api_port);
        tracing::info!("  - Signals: http://localhost:{}/api/signals", api_port);
        tracing::info!("  - Stats: http://localhost:{}/api/stats", api_port);
        
        if let Err(e) = axum::serve(listener, app).await {
            tracing::error!("API server error: {}", e);
        }
    });

    // Start background scraping loops
    tracing::info!("🔄 Starting scraping loops...");
    tracing::info!("💡 Bot running on Hashdive whale trades + Polymarket signals");

    // Hashdive + Polymarket scraping loop
    // Polymarket is always available (no auth), so we always have at least one source
    let hashdive_handle = if let Some(client) = hashdive_client {
        let hashdive_db = Arc::clone(&db);
        let hashdive_config = config.clone();
        
        Some(tokio::spawn(async move {
            hashdive_loop(client, polymarket_client, hashdive_db, hashdive_config).await;
        }))
    } else {
        // No Hashdive, but Polymarket still works - run Polymarket-only loop
        tracing::info!("💡 Running Polymarket-only mode (no Hashdive API key)");
        let poly_db = Arc::clone(&db);
        let poly_config = config.clone();
        
        Some(tokio::spawn(async move {
            polymarket_only_loop(polymarket_client, poly_db, poly_config).await;
        }))
    };

    // Wait for scraping task
    if let Some(handle) = hashdive_handle {
        handle.await?;
    }

    Ok(())
}

/// Hashdive + Polymarket scraping loop with all 6 signal types
async fn hashdive_loop(
    hashdive_client: HashdiveClient,
    polymarket_client: PolymarketClient,
    db: Arc<Database>,
    config: Config
) {
    let mut interval = time::interval(Duration::from_secs(config.hashdive_scrape_interval));
    
    loop {
        interval.tick().await;

        tracing::debug!("⏱️  Scrape cycle starting (Hashdive + Polymarket)...");

        // Fetch whale trades from Hashdive
        let whale_trades = match hashdive_client.get_whale_trades(
            Some(config.hashdive_whale_min_usd),
            Some(100) // Get top 100 recent whale trades for cluster analysis
        ).await {
            Ok(trades) => {
                if !trades.is_empty() {
                    tracing::info!("🐋 Fetched {} whale trades", trades.len());
                }
                trades
            }
            Err(e) => {
                tracing::error!("Hashdive API error: {}", e);
                vec![]
            }
        };

        let mut total_signals = 0;

        // Signal 1: Whale Following (individual large trades)
        for trade in &whale_trades {
            let signal = detect_whale_trade_signal(trade);
            
            if db.signal_exists_recently(&signal.description, 24).unwrap_or(false) {
                continue;
            }

            match db.insert_signal(&signal) {
                Ok(id) if id > 0 => {
                    total_signals += 1;
                    tracing::info!(
                        "🐋 Whale signal #{}: ${:.0} {}",
                        id,
                        trade.usd_amount,
                        trade.side
                    );
                }
                Err(e) => tracing::error!("Failed to store whale signal: {}", e),
                _ => {}
            }
        }

        // Signal 5: Whale Cluster (3+ whales same direction within 1 hour)
        let cluster_signals = detect_whale_cluster(&whale_trades, 1);
        for signal in cluster_signals {
            if db.signal_exists_recently(&signal.description, 24).unwrap_or(false) {
                continue;
            }

            match db.insert_signal(&signal) {
                Ok(id) if id > 0 => {
                    total_signals += 1;
                }
                Err(e) => tracing::error!("Failed to store cluster signal: {}", e),
                _ => {}
            }
        }

        // Fetch Polymarket events for signals 4 & 6 (CRITICAL ARB SIGNALS)
        match polymarket_client.get_events(Some(50), false).await {
            Ok(events) => {
                tracing::debug!("📊 Fetched {} Polymarket events", events.len());

                // Signal 4: Price Deviation (Binary Arbitrage)
                for event in &events {
                    let deviation_signals = detect_price_deviation(event);
                    for signal in deviation_signals {
                        if db.signal_exists_recently(&signal.description, 24).unwrap_or(false) {
                            continue;
                        }

                        match db.insert_signal(&signal) {
                            Ok(id) if id > 0 => {
                                total_signals += 1;
                            }
                            Err(e) => tracing::error!("Failed to store deviation signal: {}", e),
                            _ => {}
                        }
                    }
                }

                // Signal 6: Market Expiry Edge
                let expiry_signals = detect_market_expiry_edge(&events);
                for signal in expiry_signals {
                    if db.signal_exists_recently(&signal.description, 24).unwrap_or(false) {
                        continue;
                    }

                    match db.insert_signal(&signal) {
                        Ok(id) if id > 0 => {
                            total_signals += 1;
                        }
                        Err(e) => tracing::error!("Failed to store expiry signal: {}", e),
                        _ => {}
                    }
                }
            }
            Err(e) => {
                tracing::error!("Polymarket API error: {}", e);
            }
        }

        if total_signals > 0 {
            tracing::info!("✅ Generated {} signals this cycle", total_signals);
        }

        // Check Hashdive API usage
        match hashdive_client.check_usage().await {
            Ok(usage) => {
                let remaining = 1000 - usage.credits_used;
                if remaining < 100 {
                    tracing::warn!(
                        "⚠️  Low Hashdive API credits: {} used, {} remaining",
                        usage.credits_used,
                        remaining
                    );
                } else {
                    tracing::debug!(
                        "Hashdive API: {} used, {} remaining",
                        usage.credits_used,
                        remaining
                    );
                }
            }
            Err(e) => {
                tracing::debug!("Could not check Hashdive API usage: {}", e);
            }
        }
    }
}

/// Polymarket-only loop (fallback when Hashdive unavailable)
async fn polymarket_only_loop(
    polymarket_client: PolymarketClient,
    db: Arc<Database>,
    config: Config
) {
    let mut interval = time::interval(Duration::from_secs(config.hashdive_scrape_interval));
    
    loop {
        interval.tick().await;

        tracing::debug!("⏱️  Polymarket-only scrape cycle starting...");

        let mut total_signals = 0;

        // Fetch Polymarket events for signals 4 & 6
        match polymarket_client.get_events(Some(50), false).await {
            Ok(events) => {
                tracing::debug!("📊 Fetched {} Polymarket events", events.len());

                // Signal 4: Price Deviation (Binary Arbitrage)
                for event in &events {
                    let deviation_signals = detect_price_deviation(event);
                    for signal in deviation_signals {
                        if db.signal_exists_recently(&signal.description, 24).unwrap_or(false) {
                            continue;
                        }

                        match db.insert_signal(&signal) {
                            Ok(id) if id > 0 => {
                                total_signals += 1;
                            }
                            Err(e) => tracing::error!("Failed to store deviation signal: {}", e),
                            _ => {}
                        }
                    }
                }

                // Signal 6: Market Expiry Edge
                let expiry_signals = detect_market_expiry_edge(&events);
                for signal in expiry_signals {
                    if db.signal_exists_recently(&signal.description, 24).unwrap_or(false) {
                        continue;
                    }

                    match db.insert_signal(&signal) {
                        Ok(id) if id > 0 => {
                            total_signals += 1;
                        }
                        Err(e) => tracing::error!("Failed to store expiry signal: {}", e),
                        _ => {}
                    }
                }
            }
            Err(e) => {
                tracing::error!("Polymarket API error: {}", e);
            }
        }

        if total_signals > 0 {
            tracing::info!("✅ Generated {} Polymarket signals this cycle", total_signals);
        }
    }
}

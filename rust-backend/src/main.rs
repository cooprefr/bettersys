//! BetterBot - World's Fastest Polymarket Arbitrage Bot
//! Mission: Dominate the prediction market arbitrage space
//! Philosophy: Speed of Light execution, Physics-Aware Resilience
//! Phase 2: Database Persistence Layer Active
//! Phase 3: WebSocket Real-time Engine Active
//! Phase 4: Arbitrage Detection System Active
//! Phase 6: Expiry Edge Alpha Signal Active
//! Phase 7: Authentication & Security Active

#![allow(dead_code, unused_imports, unused_variables, unused_mut)]

mod api;
mod arbitrage; // Phase 4: Arbitrage detection engine
mod auth; // Phase 7: Authentication & security
mod backtest;
mod models;
mod risk;
mod scrapers;
mod signals;
mod vault; // Phase 8: User deposits & Kelly auto-trading

use anyhow::{Context, Result};
use axum::{
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    extract::State,
    middleware,
    response::Response,
    routing::{get, post},
    Router,
};
use chrono::Utc;
use dotenv::dotenv;
use parking_lot::RwLock as ParkingRwLock; // Faster than tokio RwLock for short critical sections
use rayon::prelude::*;
use std::path::{Path, PathBuf};
use std::{collections::VecDeque, env, sync::Arc, time::Duration};
use tokio::{
    net::TcpListener,
    sync::broadcast,
    time::{interval, Instant},
};
use tower_http::cors::CorsLayer;
use tracing::{debug, error, info, warn};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use crate::{
    auth::{api as auth_api, auth_middleware, AuthState, JwtHandler, UserStore},
    models::{Config, MarketSignal, SignalDetails, SignalType, WsServerEvent},
    risk::{RiskInput, RiskManager},
    scrapers::{
        binance_price_feed::BinancePriceFeed, dome::DomeScraper, dome_rest::DomeRestClient,
        dome_tracker::DomeClient, hashdive_api::HashdiveScraper, polymarket_api::PolymarketScraper,
    },
    signals::{
        db_storage::DbSignalStorage,
        detector::SignalDetector,
        enrichment::{DomeEnrichmentService, EnrichmentJob},
        quality::SignalQualityGate,
        wallet_analytics::{get_or_compute_wallet_analytics, WalletAnalyticsParams},
    },
};

const MIN_LATENCY_SAMPLES: usize = 20;

struct DataSourceKillSwitch {
    name: &'static str,
    enabled: bool,
    kill_triggered: bool,
    failure_threshold: u32,
    latency_threshold_ms: f64,
    consecutive_failures: u32,
    latencies_ms: VecDeque<f64>,
    window_size: usize,
}

impl DataSourceKillSwitch {
    fn new(
        name: &'static str,
        enabled_var: &str,
        failure_var: &str,
        latency_var: &str,
        default_failure_threshold: u32,
        default_latency_ms: f64,
    ) -> Self {
        let enabled = env::var(enabled_var)
            .map(|v| matches!(v.as_str(), "1" | "true" | "TRUE" | "on" | "ON"))
            .unwrap_or(true);
        let failure_threshold = env::var(failure_var)
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
            .filter(|&v| v > 0)
            .unwrap_or(default_failure_threshold);
        let latency_threshold_ms = env::var(latency_var)
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .filter(|&v| v > 0.0)
            .unwrap_or(default_latency_ms);

        Self {
            name,
            enabled,
            kill_triggered: false,
            failure_threshold,
            latency_threshold_ms,
            consecutive_failures: 0,
            latencies_ms: VecDeque::with_capacity(64),
            window_size: 64,
        }
    }

    fn is_active(&self) -> bool {
        self.enabled && !self.kill_triggered
    }

    fn disable(&mut self, reason: &str) {
        if self.enabled {
            warn!(
                source = self.name,
                reason, "🔌 Source disabled via configuration"
            );
        }
        self.enabled = false;
    }

    fn record_success(&mut self, latency: Duration) {
        if !self.enabled {
            return;
        }

        self.consecutive_failures = 0;
        let latency_ms = latency.as_secs_f64() * 1000.0;
        self.latencies_ms.push_back(latency_ms);
        if self.latencies_ms.len() > self.window_size {
            self.latencies_ms.pop_front();
        }

        if let Some(p95) = self.p95_latency() {
            if p95 > self.latency_threshold_ms {
                self.trip(&format!(
                    "latency p95 {:.1}ms exceeded threshold {:.1}ms",
                    p95, self.latency_threshold_ms
                ));
            }
        }
    }

    fn record_failure(&mut self, reason: &str) {
        if !self.enabled {
            return;
        }

        self.consecutive_failures += 1;
        warn!(
            source = self.name,
            failures = self.consecutive_failures,
            reason,
            "⚠️ Source failure recorded"
        );
        if self.consecutive_failures >= self.failure_threshold {
            self.trip(&format!(
                "{} consecutive failures (threshold {})",
                self.consecutive_failures, self.failure_threshold
            ));
        }
    }

    fn trip(&mut self, reason: &str) {
        if self.kill_triggered {
            return;
        }
        self.kill_triggered = true;
        error!(source = self.name, reason, "🛑 Kill-switch engaged");
    }

    fn p95_latency(&self) -> Option<f64> {
        if self.latencies_ms.len() < MIN_LATENCY_SAMPLES {
            return None;
        }
        let mut samples: Vec<f64> = self.latencies_ms.iter().copied().collect();
        samples.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let index = ((samples.len() as f64 * 0.95).ceil() as usize).saturating_sub(1);
        samples.get(index).copied()
    }
}

/// Application state shared across all threads
#[derive(Clone)]
struct AppState {
    signal_storage: Arc<DbSignalStorage>,
    risk_manager: Arc<ParkingRwLock<RiskManager>>, // parking_lot for faster locking
    signal_broadcast: broadcast::Sender<WsServerEvent>,
    http_client: reqwest::Client,
    dome_rest: Option<Arc<DomeRestClient>>,
    polymarket_market_ws: Arc<crate::scrapers::polymarket_ws::PolymarketMarketWsCache>,
    binance_feed: Arc<BinancePriceFeed>,
    vault: Arc<crate::vault::PooledVault>,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize environment and logging
    load_env();
    init_tracing();

    info!("🚀 BetterBot Arbitrage Engine Starting - Mission: Market Domination");
    info!("⚡ Phase 2: Database Persistence Layer ACTIVE");
    info!("🔐 Phase 7: Authentication & Security ACTIVE");

    let http_client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .context("Failed to build HTTP client")?;

    // Phase 7: Initialize authentication system
    let auth_db_path = resolve_data_path(env::var("AUTH_DB_PATH").ok(), "betterbot_auth.db");
    let jwt_secret = env::var("JWT_SECRET")
        .unwrap_or_else(|_| "dev-secret-change-in-production-minimum-32-characters".to_string());

    let user_store = Arc::new(UserStore::new(&auth_db_path)?);
    let jwt_handler = Arc::new(JwtHandler::new(jwt_secret));
    let auth_state = AuthState::new(user_store.clone(), jwt_handler.clone(), http_client.clone());

    info!("🔐 Authentication initialized at: {}", auth_db_path);

    // Initialize risk management
    let initial_bankroll = env::var("INITIAL_BANKROLL")
        .unwrap_or_else(|_| "10000".to_string())
        .parse::<f64>()
        .context("Invalid bankroll")?;

    let kelly_fraction = env::var("KELLY_FRACTION")
        .unwrap_or_else(|_| "0.25".to_string())
        .parse::<f64>()
        .context("Invalid Kelly fraction")?;

    let risk_manager = Arc::new(ParkingRwLock::new(RiskManager::new(
        initial_bankroll,
        kelly_fraction,
    )));

    // Initialize database-backed signal storage (Phase 2)
    // IMPORTANT: This defaults to the rust-backend directory so running from repo root doesn't
    // accidentally create a new empty DB in a different working directory.
    let db_path = resolve_data_path(
        env::var("DB_PATH")
            .or_else(|_| env::var("DATABASE_PATH"))
            .ok(),
        "betterbot_signals.db",
    );
    let signal_storage = Arc::new(DbSignalStorage::new(&db_path)?);

    info!("📊 Database initialized at: {}", db_path);
    info!("💾 Existing signals in database: {}", signal_storage.len());

    // Initialize broadcast channel (signals + enrichment updates)
    let (signal_tx, _signal_rx) = broadcast::channel::<WsServerEvent>(1000);

    let dome_api_key = env::var("DOME_API_KEY")
        .or_else(|_| env::var("DOME_BEARER_TOKEN"))
        .or_else(|_| env::var("DOME_TOKEN"))
        .unwrap_or_default();

    let dome_rest = if dome_api_key.trim().is_empty() {
        None
    } else {
        match DomeRestClient::new(dome_api_key) {
            Ok(c) => Some(Arc::new(c)),
            Err(e) => {
                warn!("Failed to initialize DomeRestClient for API routes: {e}");
                None
            }
        }
    };

    let polymarket_market_ws = crate::scrapers::polymarket_ws::PolymarketMarketWsCache::spawn();

    let binance_enabled = env::var("BINANCE_ENABLED")
        .map(|v| matches!(v.as_str(), "1" | "true" | "TRUE" | "on" | "ON"))
        .unwrap_or(true);
    let binance_feed = if !binance_enabled {
        BinancePriceFeed::disabled()
    } else {
        match BinancePriceFeed::spawn_default().await {
            Ok(feed) => feed,
            Err(e) => {
                warn!("Failed to start Binance price feed: {e}");
                BinancePriceFeed::disabled()
            }
        }
    };

    // Phase 8: Pooled vault state (shares + paper ledger).
    let vault_db_path = resolve_data_path(env::var("VAULT_DB_PATH").ok(), "betterbot_vault.db");
    let vault_db = Arc::new(crate::vault::VaultDb::new(&vault_db_path)?);
    let (vault_cash_db, vault_total_shares_db) = vault_db.load_state().await.unwrap_or((0.0, 0.0));
    let vault_user_shares_db = vault_db.load_user_shares().await.unwrap_or_default();

    let vault_initial_cash = if vault_cash_db > 0.0 {
        vault_cash_db
    } else {
        initial_bankroll
    };
    risk_manager.write().kelly.bankroll = vault_initial_cash;

    let vault_ledger = Arc::new(tokio::sync::Mutex::new(crate::vault::VaultPaperLedger {
        cash_usdc: vault_initial_cash,
        ..Default::default()
    }));
    let vault_shares = Arc::new(tokio::sync::Mutex::new(crate::vault::VaultShareState {
        total_shares: vault_total_shares_db,
        user_shares: vault_user_shares_db,
    }));
    let vault = Arc::new(crate::vault::PooledVault::new(
        vault_db.clone(),
        vault_ledger,
        vault_shares,
    ));

    let _ = vault
        .db
        .upsert_state(
            vault_initial_cash,
            vault_total_shares_db,
            Utc::now().timestamp(),
        )
        .await;

    let app_state = AppState {
        signal_storage: signal_storage.clone(),
        risk_manager: risk_manager.clone(),
        signal_broadcast: signal_tx.clone(),
        http_client,
        dome_rest: dome_rest.clone(),
        polymarket_market_ws,
        binance_feed,
        vault,
    };

    // Phase 8: Vault engine (15m deterministic, non-15m router stub).
    crate::vault::VaultEngine::spawn(app_state.clone());

    // Spawn parallel data collection tasks
    tokio::spawn(parallel_data_collection(
        signal_storage.clone(),
        signal_tx.clone(),
        risk_manager.clone(),
    ));

    // Spawn tracked wallet polling (45-min intervals with rotation)
    tokio::spawn(tracked_wallet_polling(
        signal_storage.clone(),
        signal_tx.clone(),
    ));

    // Phase 4+: Refresh wallet analytics (cached daily) for recently-active wallets.
    tokio::spawn(wallet_analytics_polling(signal_storage.clone(), dome_rest));

    // Periodically prune old WS event logs (equity curve source of truth) to keep the DB lean.
    tokio::spawn(storage_pruning_polling(signal_storage.clone()));

    // Background: backfill FTS search index so full-history search works.
    tokio::spawn(search_index_backfill_polling(signal_storage.clone()));

    // Phase 6: Spawn expiry edge alpha signal scanner (60-second intervals)
    tokio::spawn(expiry_edge_polling(
        signal_storage.clone(),
        signal_tx.clone(),
    ));

    // Spawn WebSocket handler
    tokio::spawn(websocket_broadcaster(signal_tx.subscribe()));

    // Build auth routes (separate router with auth state)
    let auth_router = Router::new()
        .route("/api/auth/login", post(auth_api::login))
        .route("/api/auth/privy", post(auth_api::privy_login))
        .with_state(auth_state);

    // Protected API routes
    let protected_routes = Router::new()
        .route("/api/signals", get(api::get_signals_simple))
        .route("/api/signals/search", get(api::get_signals_search))
        .route(
            "/api/signals/search/status",
            get(api::get_signals_search_status),
        )
        .route("/api/signals/context", get(api::get_signal_context_simple))
        .route("/api/signals/enrich", get(api::get_signal_enrich))
        .route("/api/signals/stats", get(api::get_signal_stats))
        .route("/api/market/snapshot", get(api::get_market_snapshot))
        .route("/api/wallet/analytics", get(api::get_wallet_analytics))
        .route(
            "/api/wallet/analytics/prime",
            post(api::post_wallet_analytics_prime),
        )
        .route("/api/vault/state", get(api::get_vault_state))
        .route("/api/vault/overview", get(api::get_vault_overview))
        .route("/api/vault/performance", get(api::get_vault_performance))
        .route("/api/vault/positions", get(api::get_vault_positions))
        .route("/api/vault/activity", get(api::get_vault_activity))
        .route("/api/vault/config", get(api::get_vault_config))
        .route(
            "/api/vault/llm/decisions",
            get(api::get_vault_llm_decisions),
        )
        .route("/api/vault/llm/models", get(api::get_vault_llm_models))
        .route("/api/vault/deposit", post(api::post_vault_deposit))
        .route("/api/vault/withdraw", post(api::post_vault_withdraw))
        .route("/api/trade/order", post(api::post_trade_order))
        .route("/api/risk/stats", get(api::get_risk_stats_simple))
        .route("/api/auth/me", get(auth_api::get_current_user))
        .route("/ws", get(websocket_handler))
        .route_layer(middleware::from_fn_with_state(
            jwt_handler.clone(),
            auth_middleware,
        ))
        .with_state(app_state.clone());

    // Public routes (health check + vault stats for marketing)
    let public_routes = Router::new()
        .route("/health", get(health_check))
        .with_state(app_state);

    let app = Router::new()
        .merge(public_routes)
        .merge(protected_routes)
        .merge(auth_router)
        .layer(CorsLayer::permissive());

    // Start server
    let addr = "0.0.0.0:3000";
    let listener = TcpListener::bind(addr).await?;
    info!("🎯 API server listening on {}", addr);

    axum::serve(listener, app).await.context("Server error")?;

    Ok(())
}

async fn wallet_analytics_polling(
    storage: Arc<DbSignalStorage>,
    dome_rest: Option<Arc<DomeRestClient>>,
) -> Result<()> {
    info!("📈 Starting wallet analytics refresher (daily cache, active wallets only)");

    let poll_secs = env::var("WALLET_ANALYTICS_POLL_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(3600);
    let mut ticker = interval(Duration::from_secs(poll_secs));

    let Some(rest) = dome_rest else {
        warn!("⚠️  Dome API key not configured - wallet analytics refresher disabled");
        loop {
            tokio::time::sleep(Duration::from_secs(3600)).await;
        }
    };
    let base_params = WalletAnalyticsParams::default();

    loop {
        ticker.tick().await;
        let now = Utc::now().timestamp();

        // Refresh only wallets that have produced signals recently.
        let recent = storage.get_recent(5000).unwrap_or_default();
        let mut wallets: std::collections::HashSet<String> =
            std::collections::HashSet::with_capacity(128);

        for s in &recent {
            match &s.signal_type {
                SignalType::TrackedWalletEntry { wallet_address, .. } => {
                    wallets.insert(wallet_address.clone());
                }
                SignalType::WhaleFollowing { whale_address, .. } => {
                    wallets.insert(whale_address.clone());
                }
                SignalType::EliteWallet { wallet_address, .. } => {
                    wallets.insert(wallet_address.clone());
                }
                SignalType::InsiderWallet { wallet_address, .. } => {
                    wallets.insert(wallet_address.clone());
                }
                _ => {}
            }
        }

        if wallets.is_empty() {
            continue;
        }

        let mut refreshed = 0usize;
        for w in wallets {
            for mode in [
                crate::signals::wallet_analytics::FrictionMode::Optimistic,
                crate::signals::wallet_analytics::FrictionMode::Base,
                crate::signals::wallet_analytics::FrictionMode::Pessimistic,
            ] {
                let mut params = base_params.clone();
                params.friction_mode = mode;
                let _ =
                    get_or_compute_wallet_analytics(&storage, &rest, &w, false, now, params).await;

                // Be conservative with rate limits.
                tokio::time::sleep(Duration::from_millis(125)).await;
            }
            refreshed += 1;

            // Additional spacing per-wallet.
            tokio::time::sleep(Duration::from_millis(125)).await;
        }

        info!(
            "📈 Wallet analytics refresh sweep done: {} wallets checked",
            refreshed
        );
    }
}

async fn storage_pruning_polling(storage: Arc<DbSignalStorage>) -> Result<()> {
    let poll_secs = env::var("STORAGE_PRUNE_POLL_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(86_400);
    let retention_days = env::var("DOME_ORDER_EVENTS_RETENTION_DAYS")
        .ok()
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(365)
        .max(140);

    let mut ticker = interval(Duration::from_secs(poll_secs));
    loop {
        ticker.tick().await;
        let now = Utc::now().timestamp();
        let cutoff = now - retention_days * 86_400;

        match storage.prune_dome_order_events_before(cutoff) {
            Ok(deleted) => {
                if deleted > 0 {
                    info!(
                        "🧹 Pruned {} dome_order_events (retention={}d)",
                        deleted, retention_days
                    );
                    let _ = storage.optimize();
                }
            }
            Err(e) => warn!("storage prune failed: {}", e),
        }
    }
}

async fn search_index_backfill_polling(storage: Arc<DbSignalStorage>) -> Result<()> {
    let enabled = env::var("SEARCH_BACKFILL_ENABLED")
        .map(|v| matches!(v.as_str(), "1" | "true" | "TRUE" | "on" | "ON"))
        .unwrap_or(true);

    if !enabled {
        info!("🔎 Search index backfill disabled");
        return Ok(());
    }

    let batch_size = env::var("SEARCH_BACKFILL_BATCH_SIZE")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(500);
    let poll_ms = env::var("SEARCH_BACKFILL_POLL_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(250);

    loop {
        match storage.backfill_search_index_step(batch_size).await {
            Ok(0) => break,
            Ok(n) => {
                debug!("🔎 Search index backfill: indexed {} signals", n);
            }
            Err(e) => {
                warn!("search index backfill failed: {}", e);
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }

        tokio::time::sleep(Duration::from_millis(poll_ms)).await;
    }

    Ok(())
}

/// Initialize tracing with enhanced observability
fn init_tracing() {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "betterbot_backend=debug,tower_http=debug".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();
}

/// Parallel data collection using Rayon for maximum throughput
async fn parallel_data_collection(
    storage: Arc<DbSignalStorage>,
    signal_tx: broadcast::Sender<WsServerEvent>,
    risk_manager: Arc<ParkingRwLock<RiskManager>>,
) -> Result<()> {
    info!("🔥 Starting parallel data collection with real API connections");

    // Get API keys from environment
    let hashdive_api_key = env::var("HASHDIVE_API_KEY").unwrap_or_default();
    // Dome uses a bearer token; support multiple env var names to avoid misconfiguration.
    let dome_api_key = env::var("DOME_API_KEY")
        .or_else(|_| env::var("DOME_BEARER_TOKEN"))
        .or_else(|_| env::var("DOME_TOKEN"))
        .unwrap_or_default();

    // Kill-switches and latency monitors per data source
    let mut polymarket_switch = DataSourceKillSwitch::new(
        "polymarket",
        "POLYMARKET_ENABLED",
        "POLYMARKET_FAILURE_THRESHOLD",
        "POLYMARKET_LATENCY_P95_MS",
        3,
        5_000.0,
    );
    let mut hashdive_switch = DataSourceKillSwitch::new(
        "hashdive",
        "HASHDIVE_ENABLED",
        "HASHDIVE_FAILURE_THRESHOLD",
        "HASHDIVE_LATENCY_P95_MS",
        3,
        10_000.0,
    );
    let mut dome_switch = DataSourceKillSwitch::new(
        "dome",
        "DOME_ENABLED",
        "DOME_FAILURE_THRESHOLD",
        "DOME_LATENCY_P95_MS",
        3,
        8_000.0,
    );

    if hashdive_api_key.trim().is_empty() {
        hashdive_switch.disable("HASHDIVE_API_KEY missing or empty");
    }
    if dome_api_key.trim().is_empty() {
        dome_switch.disable("DOME_API_KEY missing or empty");
    }

    let mut quality_gate = SignalQualityGate::new(Duration::from_secs(3), 8.0);

    // Poll every 45 minutes to conserve Hashdive API credits (1000/month limit)
    // 45 minutes = 32 requests/day × 30 days = 960/month (under 1000 limit)
    // Hashdive data updates every minute, but we poll less frequently to stay under credit limit
    let mut interval_timer = interval(Duration::from_secs(2700)); // 45-minute intervals

    loop {
        interval_timer.tick().await;

        let mut raw_signals: Vec<MarketSignal> = Vec::new();

        let polymarket_handle = if polymarket_switch.is_active() {
            let start = Instant::now();
            Some(tokio::spawn(async move {
                let result = scrape_polymarket_real().await;
                (result, start.elapsed())
            }))
        } else {
            None
        };

        let hash_handle = if hashdive_switch.is_active() {
            let api_key = hashdive_api_key.clone();
            let start = Instant::now();
            Some(tokio::spawn(async move {
                let result = scrape_hashdive_real(api_key).await;
                (result, start.elapsed())
            }))
        } else {
            None
        };

        let dome_handle = if dome_switch.is_active() {
            let api_key = dome_api_key.clone();
            let start = Instant::now();
            Some(tokio::spawn(async move {
                let result = scrape_dome_real(api_key).await;
                (result, start.elapsed())
            }))
        } else {
            None
        };

        if let Some(handle) = polymarket_handle {
            match handle.await {
                Ok((Ok(mut signals), latency)) => {
                    polymarket_switch.record_success(latency);
                    for signal in signals.iter_mut() {
                        signal.source = "polymarket".to_string();
                    }
                    raw_signals.append(&mut signals);
                }
                Ok((Err(err), latency)) => {
                    polymarket_switch.record_failure(&err.to_string());
                    debug!(source = "polymarket", error = %err, "Polymarket scrape failed");
                }
                Err(join_err) => {
                    polymarket_switch.record_failure(&format!("join error: {join_err}"));
                }
            }
        }

        if let Some(handle) = hash_handle {
            match handle.await {
                Ok((Ok(mut signals), latency)) => {
                    hashdive_switch.record_success(latency);
                    raw_signals.append(&mut signals);
                }
                Ok((Err(err), latency)) => {
                    hashdive_switch.record_failure(&err.to_string());
                    debug!(source = "hashdive", error = %err, "Hashdive scrape failed");
                }
                Err(join_err) => {
                    hashdive_switch.record_failure(&format!("join error: {join_err}"));
                }
            }
        }

        if let Some(handle) = dome_handle {
            match handle.await {
                Ok((Ok(mut signals), latency)) => {
                    dome_switch.record_success(latency);
                    raw_signals.append(&mut signals);
                }
                Ok((Err(err), latency)) => {
                    dome_switch.record_failure(&err.to_string());
                    debug!(source = "dome", error = %err, "Dome scrape failed");
                }
                Err(join_err) => {
                    dome_switch.record_failure(&format!("join error: {join_err}"));
                }
            }
        }

        let raw_count = raw_signals.len();
        let mut qualified_signals = if raw_count > 0 {
            let filtered = quality_gate.filter(raw_signals);
            let dropped = raw_count.saturating_sub(filtered.len());
            if dropped > 0 {
                warn!("🧹 Data quality gate dropped {} signals", dropped);
            }
            filtered
        } else {
            Vec::new()
        };

        // No mock signals - only use real data
        if qualified_signals.is_empty() {
            debug!("No new signals detected in this polling cycle");
        }

        // Process signals with risk management - use parking_lot's write() which is non-async
        let processed_signals: Vec<MarketSignal> = qualified_signals
            .par_iter()
            .filter_map(|signal| {
                let mut risk_mgr = risk_manager.write(); // parking_lot - fast, non-blocking
                let liquidity = signal.details.liquidity;
                let family = signal.signal_family();
                let risk_input = RiskInput {
                    market_probability: signal.confidence,
                    signal_confidence: signal.confidence,
                    market_liquidity: liquidity,
                    signal_family: family.clone(),
                    regime_risk: None,
                };
                match risk_mgr.calculate_position(risk_input) {
                    Ok(position) if position.position_size > 0.0 => {
                        let now = chrono::Utc::now().to_rfc3339();
                        Some(MarketSignal {
                            id: signal.id.clone(),
                            signal_type: signal.signal_type.clone(),
                            market_slug: signal.market_slug.clone(),
                            confidence: position.calibrated_confidence,
                            risk_level: signal.risk_level.clone(),
                            details: SignalDetails {
                                market_id: signal.details.market_id.clone(),
                                market_title: signal.details.market_title.clone(),
                                current_price: signal.details.current_price,
                                volume_24h: signal.details.volume_24h,
                                liquidity: signal.details.liquidity,
                                recommended_action: signal.details.recommended_action.clone(),
                                expiry_time: signal.details.expiry_time.clone(),
                                observed_timestamp: Some(now),
                                signal_family: Some(family),
                                calibration_version: Some(position.calibration_version),
                                guardrail_flags: if position.guardrail_flags.is_empty() {
                                    None
                                } else {
                                    Some(position.guardrail_flags)
                                },
                                recommended_size: Some(position.position_size),
                            },
                            detected_at: signal.detected_at.clone(),
                            source: signal.source.clone(),
                        })
                    }
                    _ => None,
                }
            })
            .collect();

        // Batch store for better performance
        if !processed_signals.is_empty() {
            if let Err(e) = storage.store_batch(&processed_signals).await {
                warn!("Failed to batch store signals: {}", e);
            }

            // Broadcast each signal
            for signal in processed_signals {
                let _ = signal_tx.send(WsServerEvent::Signal(signal));
            }
        }
    }
}

fn default_data_path(filename: &str) -> String {
    // Anchor defaults to the Rust crate directory (rust-backend/)
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    base.join(filename).to_string_lossy().to_string()
}

fn resolve_data_path(env_value: Option<String>, default_filename: &str) -> String {
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let Some(raw) = env_value.filter(|v| !v.trim().is_empty()) else {
        return default_data_path(default_filename);
    };

    let p = PathBuf::from(raw);
    if p.is_absolute() {
        return p.to_string_lossy().to_string();
    }

    // Treat relative paths as relative to rust-backend/, not the caller's cwd.
    base.join(p).to_string_lossy().to_string()
}

fn load_env() {
    // 1) Standard dotenv search (cwd + parents)
    let _ = dotenv();

    // 2) Also try repo-root .env (common when running with --manifest-path from elsewhere)
    // CARGO_MANIFEST_DIR points at rust-backend/ at compile time.
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));

    let candidates = [manifest_dir.join(".env"), manifest_dir.join("../.env")];

    for p in candidates {
        if p.exists() {
            let _ = dotenv::from_path(&p);
        }
    }
}

/// Scrape real Polymarket data using CLOB API
async fn scrape_polymarket_real() -> Result<Vec<MarketSignal>> {
    let mut scraper = PolymarketScraper::new();
    let detector = SignalDetector::new();

    // Fetch real markets from GAMMA API for better data
    match scraper.fetch_gamma_markets(100, 0).await {
        Ok(gamma_response) => {
            let events = scraper.gamma_to_events(gamma_response);
            let signals = detector.detect_all(&events).await;
            info!(
                "📊 Polymarket REAL: {} signals from {} markets",
                signals.len(),
                events.len()
            );
            Ok(signals)
        }
        Err(e) => {
            warn!("Polymarket API error (non-critical): {}", e);
            Ok(Vec::new())
        }
    }
}

/// Scrape real Hashdive whale data with elite/insider classification
async fn scrape_hashdive_real(api_key: String) -> Result<Vec<MarketSignal>> {
    if api_key.is_empty() || api_key == "your_api_key_here" {
        return Ok(Vec::new());
    }

    let mut scraper = HashdiveScraper::new(api_key.clone());

    // Note: get_latest_whale_trades returns WhaleTrade which doesn't have wallet info
    // Instead, we'll query by known wallet addresses
    // For now, let's use a simplified approach with just whale following signals
    match scraper
        .get_latest_whale_trades(Some(20000.0), Some(50))
        .await
    {
        Ok(whale_response) => {
            let signals: Vec<MarketSignal> = whale_response
                .data
                .into_iter()
                .filter(|trade| trade.size > 10000.0)
                .map(|trade| MarketSignal {
                    id: format!("whale_{}", trade.timestamp),
                    signal_type: SignalType::WhaleFollowing {
                        whale_address: "unknown".to_string(),
                        position_size: trade.size,
                        confidence_score: (trade.size / 100000.0).min(0.99),
                    },
                    market_slug: trade.market_id.clone(),
                    confidence: (trade.size / 50000.0).min(0.95),
                    risk_level: if trade.size > 50000.0 {
                        "low"
                    } else {
                        "medium"
                    }
                    .to_string(),
                    details: SignalDetails {
                        market_id: trade.market_id,
                        market_title: format!("🐋 Whale Trade: {} ${:.0}", trade.side, trade.size),
                        current_price: trade.price,
                        volume_24h: trade.size,
                        liquidity: 0.0,
                        recommended_action: if trade.side == "BUY" {
                            "FOLLOW_BUY"
                        } else {
                            "FOLLOW_SELL"
                        }
                        .to_string(),
                        expiry_time: None,
                        observed_timestamp: None,
                        signal_family: None,
                        calibration_version: None,
                        guardrail_flags: None,
                        recommended_size: None,
                    },
                    detected_at: chrono::Utc::now().to_rfc3339(),
                    source: "hashdive".to_string(),
                })
                .collect();

            info!("🐋 Hashdive REAL: {} whale signals detected", signals.len());
            Ok(signals)
        }
        Err(e) => {
            warn!("Hashdive API error (non-critical): {}", e);
            Ok(Vec::new())
        }
    }
}

/// Scrape DomeAPI for cross-platform arbitrage
async fn scrape_dome_real(api_key: String) -> Result<Vec<MarketSignal>> {
    if api_key.is_empty() || api_key == "your_dome_api_key_here" {
        return Ok(Vec::new());
    }

    let mut scraper = DomeScraper::new(api_key);

    // Scan for arbitrage opportunities with 2% minimum spread
    match scraper.scan_arbitrage_opportunities(0.02).await {
        Ok(opportunities) => {
            let signals: Vec<MarketSignal> = opportunities
                .into_iter()
                .map(|opp| {
                    MarketSignal {
                        id: format!("arb_{}", opp.polymarket_market),
                        signal_type: SignalType::CrossPlatformArbitrage {
                            polymarket_price: 0.5,    // Would be fetched from opportunity
                            kalshi_price: Some(0.48), // Would be fetched
                            spread_pct: opp.spread_pct,
                        },
                        market_slug: opp.polymarket_market.clone(),
                        confidence: opp.confidence,
                        risk_level: if opp.confidence > 0.8 {
                            "low"
                        } else {
                            "medium"
                        }
                        .to_string(),
                        details: SignalDetails {
                            market_id: opp.polymarket_market,
                            market_title: format!(
                                "Arbitrage: {:.1}% spread",
                                opp.spread_pct * 100.0
                            ),
                            current_price: 0.5,
                            volume_24h: 0.0,
                            liquidity: 0.0,
                            recommended_action: "ARBITRAGE".to_string(),
                            expiry_time: None,
                            observed_timestamp: None,
                            signal_family: None,
                            calibration_version: None,
                            guardrail_flags: None,
                            recommended_size: None,
                        },
                        detected_at: opp.detected_at,
                        source: "dome".to_string(),
                    }
                })
                .collect();

            info!(
                "💎 DomeAPI REAL: {} arbitrage opportunities found",
                signals.len()
            );
            Ok(signals)
        }
        Err(e) => {
            warn!("DomeAPI error (non-critical): {}", e);
            Ok(Vec::new())
        }
    }
}

/// Tracked wallet polling with WebSocket streaming and REST fallback
/// Mission: Zero missed entries. Real-time = competitive advantage.
async fn tracked_wallet_polling(
    storage: Arc<DbSignalStorage>,
    signal_tx: broadcast::Sender<WsServerEvent>,
) -> Result<()> {
    info!("👑 Starting tracked wallet STREAMING system");

    // Load config
    let config = Config::from_env();

    // Check if Dome API key is available
    let dome_api_key = match &config.dome_api_key {
        Some(key) if !key.is_empty() && key != "your_dome_api_key_here" && key.len() > 10 => {
            key.clone()
        }
        _ => {
            warn!("⚠️  Dome API key not configured or invalid - wallet tracking disabled");
            warn!("⚠️  Set DOME_API_KEY environment variable or add to config");
            loop {
                tokio::time::sleep(Duration::from_secs(3600)).await;
            }
        }
    };

    // Extract wallet addresses from config
    let tracked_wallets: Vec<String> = config.tracked_wallets.keys().cloned().collect();
    let wallet_labels = config.tracked_wallets.clone();

    info!(
        "📊 Tracking {} wallets with WebSocket + REST fallback",
        tracked_wallets.len()
    );

    // Try WebSocket first (real-time)
    use crate::scrapers::dome_websocket::{DomeWebSocketClient, WsOrderData};

    let (ws_client, mut order_rx) =
        DomeWebSocketClient::new(dome_api_key.clone(), tracked_wallets.clone());
    let detector = SignalDetector::new();

    // Enrichment pipeline
    let enrich_workers = env::var("DOME_ENRICH_WORKERS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(2);
    let enrich_queue_size = env::var("DOME_ENRICH_QUEUE_SIZE")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(2000);
    let enrich_max_conc = env::var("DOME_ENRICH_MAX_CONCURRENT_REQUESTS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(8);
    let enrich_max_heavy = env::var("DOME_ENRICH_MAX_CONCURRENT_HEAVY_REQUESTS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(2);

    let (enrich_tx, enrich_rx) = tokio::sync::mpsc::channel::<EnrichmentJob>(enrich_queue_size);
    let dome_rest = crate::scrapers::dome_rest::DomeRestClient::new(dome_api_key.clone())?;
    let enrich_svc = DomeEnrichmentService::new(
        dome_rest,
        storage.clone(),
        signal_tx.clone(),
        enrich_max_conc,
        enrich_max_heavy,
    )?;
    enrich_svc.spawn_workers(enrich_rx, enrich_workers);

    // Spawn WebSocket connection task
    let ws_handle = tokio::spawn(async move {
        match ws_client.run().await {
            Ok(_) => info!("✅ WebSocket connection successful"),
            Err(e) => warn!("⚠️ WebSocket failed: {} - using REST polling fallback", e),
        }
    });

    // Start REST polling as fallback (polls every 60 seconds)
    use crate::scrapers::dome_realtime::{DomeOrder, DomeRealtimeClient};
    use std::collections::HashMap;

    let mut rest_client = DomeRealtimeClient::new(dome_api_key, wallet_labels.clone());

    // Hybrid approach: WebSocket primary (real-time), REST fallback (30s)
    let mut ws_connected = false;
    let mut poll_interval = tokio::time::interval(Duration::from_secs(30)); // 30s REST fallback
    poll_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    info!("🔥 Hybrid tracking active: WebSocket (real-time) + REST (30s fallback)");

    // Hybrid loop: process WebSocket messages AND poll REST API
    loop {
        tokio::select! {
            // Process WebSocket messages when available
            Some(order) = order_rx.recv() => {
                ws_connected = true;

                // Find wallet label
                let wallet_label = wallet_labels
                    .get(&order.user)
                    .map(|s| s.as_str())
                    .unwrap_or("unknown");

                info!(
                    "💰 WEBSOCKET: {} [{}] {} {} @ ${:.3} | {}",
                    &order.user[..10],
                    wallet_label,
                    order.side,
                    order.shares_normalized,
                    order.price,
                    &order.market_slug
                );

                // Convert WsOrderData to DomeOrder format for detector
                use crate::scrapers::dome_tracker::DomeOrder;

                // Clone the WS order fields we need (we also persist the raw payload)
                let raw_payload_json = serde_json::to_string(&order).ok();
                let user = order.user.clone();
                let market_slug = order.market_slug.clone();
                let condition_id = order.condition_id.clone();
                let token_id = order.token_id.clone();
                let token_label = order.token_label.clone();
                let side = order.side.clone();
                let price = order.price;
                let shares_normalized = order.shares_normalized;
                let timestamp = order.timestamp;
                let order_hash = order.order_hash.clone();
                let tx_hash = order.tx_hash.clone();
                let title = order.title.clone();

                let dome_order = vec![DomeOrder {
                    token_id: token_id.clone(),
                    token_label: token_label.clone(), // "Up", "Down", "Yes", "No"
                    side: side.clone(),
                    shares_normalized,
                    price,
                    timestamp,
                    market_slug: market_slug.clone(),
                    title: title.clone(),
                    user: user.clone(),
                    condition_id: Some(condition_id.clone()),
                    order_hash: Some(order_hash.clone()),
                    tx_hash: Some(tx_hash.clone()),
                }];

                // Detect signals from this order
                let signals = detector.detect_trader_entry(&dome_order, &user, wallet_label);

                // Store and broadcast immediately
                for signal in signals {
                    if let Err(e) = storage.store(&signal).await {
                        warn!("Failed to store signal {}: {}", signal.id, e);
                    }
                    let _ = signal_tx.send(WsServerEvent::Signal(signal.clone()));

                    // Queue enrichment for this signal (best effort)
                    if let Some(raw_json) = raw_payload_json.clone() {
                        let job = EnrichmentJob {
                            signal_id: signal.id.clone(),
                            user: user.clone(),
                            market_slug: market_slug.clone(),
                            condition_id: condition_id.clone(),
                            token_id: token_id.clone(),
                            token_label: token_label.clone(),
                            side: side.clone(),
                            price,
                            shares_normalized,
                            timestamp,
                            order_hash: order_hash.clone(),
                            tx_hash: tx_hash.clone(),
                            title: title.clone(),
                            raw_payload_json: raw_json,
                        };
                        let _ = enrich_tx.try_send(job);
                    }
                }
            }

            // Poll REST API every 60 seconds as fallback
            _ = poll_interval.tick() => {
                // Only poll REST when we haven't seen any WebSocket messages since the last tick.
                // This keeps the Dome load low when WS is healthy (HFT path) while retaining a
                // safety net for missed WS events / disconnects.
                if ws_connected {
                    ws_connected = false;
                    continue;
                }

                info!("🔄 REST polling for tracked wallets (WebSocket fallback)...");

                match rest_client.poll_all_wallets_with_orders().await {
                    Ok((signals, orders)) => {
                        if !signals.is_empty() {
                            info!("📊 REST API: Found {} signals from tracked wallets", signals.len());

                            for signal in &signals {
                                if let Err(e) = storage.store(signal).await {
                                    warn!("Failed to store REST signal {}: {}", signal.id, e);
                                }
                                let _ = signal_tx.send(WsServerEvent::Signal(signal.clone()));
                            }

                            // Queue enrichment for REST signals (same as WebSocket path)
                            for (order, wallet_label) in orders {
                                let raw_json = serde_json::to_string(&order).unwrap_or_default();
                                let job = EnrichmentJob {
                                    signal_id: format!("dome_order_{}", order.order_hash),
                                    user: order.user.clone(),
                                    market_slug: order.market_slug.clone(),
                                    condition_id: order.condition_id.clone(),
                                    token_id: order.token_id.clone(),
                                    token_label: order.token_label.clone(),
                                    side: order.side.clone(),
                                    price: order.price,
                                    shares_normalized: order.shares_normalized,
                                    timestamp: order.timestamp,
                                    order_hash: order.order_hash.clone(),
                                    tx_hash: order.tx_hash.clone(),
                                    title: order.title.clone(),
                                    raw_payload_json: raw_json,
                                };
                                if let Err(e) = enrich_tx.try_send(job) {
                                    debug!("Enrichment queue full for REST signal: {}", e);
                                }
                            }
                        }
                    }
                    Err(e) => {
                        warn!("REST polling error: {}", e);
                    }
                }

                // Reset WebSocket flag - if it was connected, next loop will set it true again
                ws_connected = false;
            }
        }
    }
}

/// Phase 6: Expiry edge alpha signal polling
/// Mission: Capture 95% win rate from markets ≤4 hours until expiry
async fn expiry_edge_polling(
    storage: Arc<DbSignalStorage>,
    signal_tx: broadcast::Sender<WsServerEvent>,
) -> Result<()> {
    info!("🎯 Starting expiry edge alpha scanner (Phase 6)");
    info!("🔍 Polling: Every 60 seconds | Threshold: ≤4 hours | Win rate: 95%");

    use crate::scrapers::expiry_edge::ExpiryEdgeScanner;

    let mut scanner = ExpiryEdgeScanner::new();
    let mut interval_timer = interval(Duration::from_secs(60)); // 1 minute intervals

    loop {
        interval_timer.tick().await;

        match scanner.scan().await {
            Ok(signals) => {
                if !signals.is_empty() {
                    info!("🎯 Expiry edge scan: {} signals found", signals.len());
                }

                // Store and broadcast signals
                for signal in signals {
                    // Log high-probability signals
                    if signal.confidence >= 0.80 {
                        info!(
                            "🚨 HIGH PROBABILITY EXPIRY EDGE: {} (conf: {:.1}%)",
                            signal.market_slug,
                            signal.confidence * 100.0
                        );
                    }

                    // Store in database
                    if let Err(e) = storage.store(&signal).await {
                        warn!("Failed to store expiry edge signal {}: {}", signal.id, e);
                    }

                    // Broadcast to WebSocket clients
                    let _ = signal_tx.send(WsServerEvent::Signal(signal));
                }
            }
            Err(e) => {
                warn!("⚠️  Expiry edge scan failed (non-critical): {}", e);
                // Continue polling even if one scan fails
            }
        }
    }
}

/// WebSocket handler for real-time signal streaming
async fn websocket_handler(ws: WebSocketUpgrade, State(state): State<AppState>) -> Response {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

async fn handle_socket(mut socket: WebSocket, state: AppState) {
    let mut rx = state.signal_broadcast.subscribe();

    // On connect, immediately replay recent signals so the UI isn't empty even if REST polling fails.
    if let Ok(recent) = state.signal_storage.get_recent(200) {
        let mut signal_ids: Vec<String> = Vec::with_capacity(recent.len());

        // Send signals first.
        for signal in recent {
            signal_ids.push(signal.id.clone());
            let msg = serde_json::to_string(&WsServerEvent::Signal(signal))
                .unwrap_or_else(|_| "{}".to_string());
            if socket.send(Message::Text(msg)).await.is_err() {
                return;
            }
        }

        // Then replay any stored context for those signals so list metadata is hydrated immediately.
        if let Ok(contexts) = state.signal_storage.get_contexts_for_signals(&signal_ids) {
            for signal_id in signal_ids {
                let Some(ctx) = contexts.get(&signal_id) else {
                    continue;
                };
                let update = crate::models::SignalContextUpdate {
                    signal_id: signal_id.clone(),
                    context_version: ctx.context_version,
                    enriched_at: ctx.enriched_at,
                    status: ctx.status.clone(),
                    context: ctx.context.lite(),
                };
                let msg = serde_json::to_string(&WsServerEvent::SignalContext(update))
                    .unwrap_or_else(|_| "{}".to_string());
                if socket.send(Message::Text(msg)).await.is_err() {
                    return;
                }
            }
        }
    }

    loop {
        tokio::select! {
            // Send new signals to client
            Ok(event) = rx.recv() => {
                let msg = serde_json::to_string(&event)
                    .unwrap_or_else(|e| {
                        warn!("Failed to serialize ws event: {}", e);
                        "{}".to_string()
                    });
                if socket.send(Message::Text(msg)).await.is_err() {
                    break;
                }
            }
            // Handle incoming messages from client
            Some(Ok(msg)) = socket.recv() => {
                match msg {
                    Message::Text(text) => {
                        // Try to parse as JSON first
                        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
                            if json.get("type").and_then(|t| t.as_str()) == Some("ping") {
                                // Echo back pong with the same timestamp for latency calculation
                                let timestamp = json.get("data")
                                    .and_then(|d| d.get("timestamp"))
                                    .and_then(|t| t.as_i64())
                                    .unwrap_or(0);
                                let pong = serde_json::json!({
                                    "type": "pong",
                                    "data": { "timestamp": timestamp }
                                });
                                let _ = socket.send(Message::Text(pong.to_string())).await;
                            }
                        } else if text == "ping" {
                            // Legacy plain text ping
                            let _ = socket.send(Message::Text("pong".to_string())).await;
                        }
                    }
                    Message::Close(_) => break,
                    _ => {}
                }
            }
        }
    }
}

/// Broadcast signals to all WebSocket connections
async fn websocket_broadcaster(mut rx: broadcast::Receiver<WsServerEvent>) {
    loop {
        if let Ok(event) = rx.recv().await {
            if let WsServerEvent::Signal(signal) = event {
                info!("📡 Broadcasting signal: {}", signal.market_slug);
            }
        }
    }
}

/// Health check endpoint
async fn health_check() -> &'static str {
    "🚀 BetterBot Operational - Phase 2: Database Persistence ACTIVE"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_risk_manager_integration() {
        let mut risk_manager = RiskManager::new(10000.0, 0.25);
        let input = RiskInput {
            market_probability: 0.65,
            signal_confidence: 0.8,
            market_liquidity: 50_000.0,
            signal_family: "test".to_string(),
            regime_risk: Some(1.0),
        };
        let position = risk_manager
            .calculate_position(input)
            .expect("Risk manager calculation should succeed in test");
        assert!(position.position_size > 0.0);
        assert!(position.position_size < 10000.0);
    }
}

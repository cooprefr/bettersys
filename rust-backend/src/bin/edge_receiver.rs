//! Edge Receiver Binary
//!
//! Runs in ap-southeast-1 (Singapore) to receive Binance market data
//! and forward it to the trading engine in eu-west-1.
//!
//! Usage:
//!   edge_receiver --forward-host 10.0.1.100 --forward-port 19876
//!
//! Environment:
//!   EDGE_SYMBOLS - Comma-separated symbols (default: BTCUSDT,ETHUSDT,SOLUSDT,XRPUSDT)
//!   EDGE_FORWARD_HOST - Destination host
//!   EDGE_FORWARD_PORT - Destination port (default: 19876)
//!   EDGE_HEARTBEAT_MS - Heartbeat interval (default: 100)
//!   EDGE_PIN_CORE - CPU core to pin to (optional)

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use axum::{routing::get, Json, Router};
use clap::Parser;
use tracing::{info, Level};
use tracing_subscriber::EnvFilter;

// Import from the library
use betterbot_backend::edge::receiver::{EdgeReceiver, EdgeReceiverConfig};

#[derive(Parser, Debug)]
#[command(name = "edge_receiver")]
#[command(about = "Binance Edge Receiver - Forward market data to trading engine")]
struct Args {
    /// Symbols to subscribe to (comma-separated)
    #[arg(long, env = "EDGE_SYMBOLS", default_value = "BTCUSDT,ETHUSDT,SOLUSDT,XRPUSDT")]
    symbols: String,

    /// Binance WebSocket URL
    #[arg(long, env = "EDGE_BINANCE_WS_URL", default_value = "wss://stream.binance.com:9443/ws")]
    binance_ws_url: String,

    /// Forward destination host
    #[arg(long, env = "EDGE_FORWARD_HOST", default_value = "127.0.0.1")]
    forward_host: String,

    /// Forward destination port
    #[arg(long, env = "EDGE_FORWARD_PORT", default_value = "19876")]
    forward_port: u16,

    /// Heartbeat interval in milliseconds
    #[arg(long, env = "EDGE_HEARTBEAT_MS", default_value = "100")]
    heartbeat_ms: u64,

    /// Stale threshold in milliseconds
    #[arg(long, env = "EDGE_STALE_MS", default_value = "100")]
    stale_ms: u64,

    /// CPU core to pin to (optional)
    #[arg(long, env = "EDGE_PIN_CORE")]
    pin_core: Option<usize>,

    /// Metrics HTTP port (optional)
    #[arg(long, env = "EDGE_METRICS_PORT", default_value = "9090")]
    metrics_port: u16,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::from_default_env()
                .add_directive(Level::INFO.into())
                .add_directive("edge_receiver=debug".parse().unwrap()),
        )
        .init();

    let args = Args::parse();

    info!("Starting Edge Receiver");
    info!("  Symbols: {}", args.symbols);
    info!("  Binance WS: {}", args.binance_ws_url);
    info!("  Forward to: {}:{}", args.forward_host, args.forward_port);
    info!("  Heartbeat: {}ms", args.heartbeat_ms);
    info!("  Pin core: {:?}", args.pin_core);

    let symbols: Vec<String> = args
        .symbols
        .split(',')
        .map(|s| s.trim().to_uppercase())
        .collect();

    let forward_addr: SocketAddr = format!("{}:{}", args.forward_host, args.forward_port).parse()?;

    let config = EdgeReceiverConfig {
        symbols,
        binance_ws_url: args.binance_ws_url,
        forward_addr,
        heartbeat_interval: Duration::from_millis(args.heartbeat_ms),
        stale_threshold: Duration::from_millis(args.stale_ms),
        pin_to_core: args.pin_core,
    };

    let receiver = EdgeReceiver::new(config);

    // Start metrics server (receiver is Arc, so we can share stats)
    let receiver_for_metrics = receiver.clone();
    let metrics_port = args.metrics_port;
    tokio::spawn(async move {
        start_metrics_server(metrics_port, receiver_for_metrics).await;
    });

    // Handle shutdown
    let receiver_clone = receiver.clone();
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        info!("Shutdown signal received");
        receiver_clone.stop();
    });

    // Run the receiver
    receiver.run().await?;

    info!("Edge receiver stopped");
    Ok(())
}

async fn start_metrics_server(port: u16, receiver: Arc<EdgeReceiver>) {
    let app = Router::new()
        .route("/health", get(|| async { "OK" }))
        .route(
            "/metrics",
            get({
                let receiver = receiver.clone();
                move || {
                    let stats = receiver.stats().snapshot();
                    async move { Json(stats) }
                }
            }),
        );

    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{}", port))
        .await
        .expect("Failed to bind metrics port");

    info!("Metrics server listening on port {}", port);
    axum::serve(listener, app).await.ok();
}

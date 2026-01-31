//! Route Quality Monitor Service
//!
//! Standalone binary for continuous route quality monitoring to Binance endpoints.
//! Exposes Prometheus metrics on HTTP and triggers mitigations via callbacks.
//!
//! Usage:
//!   route_quality_monitor --config config.toml --metrics-port 9090
//!
//! Environment Variables:
//!   ROUTE_QUALITY_CONFIG_PATH - Path to TOML config file
//!   ROUTE_QUALITY_METRICS_PORT - Prometheus metrics port (default: 9090)
//!   ROUTE_QUALITY_LOG_LEVEL - Log level (default: info)

use std::net::SocketAddr;
use std::sync::Arc;

use axum::{routing::get, Router};
use clap::Parser;
use tokio::sync::mpsc;
use tracing::{info, Level};
use tracing_subscriber::FmtSubscriber;

use betterbot_backend::route_quality::{
    BaselineCalculator, MitigationAction, MitigationController, MitigationEvent,
    RouteQualityConfig, RouteQualityMetrics, RouteQualityProber,
};

#[derive(Parser, Debug)]
#[command(name = "route_quality_monitor")]
#[command(about = "Continuous route quality monitoring for market data endpoints")]
struct Args {
    /// Path to TOML configuration file
    #[arg(short, long)]
    config: Option<String>,

    /// Prometheus metrics port
    #[arg(short, long, default_value = "9090")]
    metrics_port: u16,

    /// Log level (trace, debug, info, warn, error)
    #[arg(short, long, default_value = "info")]
    log_level: String,

    /// Webhook URL for mitigation events (optional)
    #[arg(long)]
    webhook_url: Option<String>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    // Initialize logging
    let level = match args.log_level.to_lowercase().as_str() {
        "trace" => Level::TRACE,
        "debug" => Level::DEBUG,
        "info" => Level::INFO,
        "warn" => Level::WARN,
        "error" => Level::ERROR,
        _ => Level::INFO,
    };

    let subscriber = FmtSubscriber::builder()
        .with_max_level(level)
        .with_target(true)
        .with_thread_ids(true)
        .with_file(true)
        .with_line_number(true)
        .finish();
    tracing::subscriber::set_global_default(subscriber)?;

    info!("Starting Route Quality Monitor");

    // Load configuration
    let config = if let Some(config_path) = &args.config {
        info!("Loading config from {}", config_path);
        let content = tokio::fs::read_to_string(config_path).await?;
        toml::from_str(&content)?
    } else {
        info!("Using default configuration");
        RouteQualityConfig::default()
    };

    info!(
        "Monitoring {} endpoints",
        config.endpoints.len()
    );
    for ep in &config.endpoints {
        info!("  - {} ({}:{})", ep.name, ep.host, ep.port);
    }

    // Create shared metrics
    let metrics = Arc::new(RouteQualityMetrics::new());

    // Create mitigation channel
    let (mitigation_tx, mitigation_rx) = mpsc::channel::<MitigationAction>(100);

    // Create baseline calculator
    let baseline_calculator = Arc::new(BaselineCalculator::new(
        config.baseline.clone(),
        metrics.clone(),
    ));

    // Create prober
    let prober = RouteQualityProber::new(config.clone(), metrics.clone(), mitigation_tx);

    // Create mitigation controller
    let mut mitigation_controller =
        MitigationController::new(config.clone(), metrics.clone(), mitigation_rx);

    // Set up mitigation callback
    let webhook_url = args.webhook_url.clone();
    mitigation_controller.set_callback(move |event| {
        handle_mitigation_event(event, webhook_url.as_deref());
    });

    // Create HTTP server for Prometheus metrics
    let metrics_for_handler = metrics.clone();
    let app = Router::new()
        .route("/metrics", get(move || {
            let m = metrics_for_handler.clone();
            async move { m.to_prometheus() }
        }))
        .route("/health", get(|| async { "OK" }))
        .route("/ready", get(|| async { "OK" }));

    let addr = SocketAddr::from(([0, 0, 0, 0], args.metrics_port));
    info!("Prometheus metrics available at http://{}/metrics", addr);

    // Spawn HTTP server
    let http_server = tokio::spawn(async move {
        let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
        axum::serve(listener, app).await.unwrap();
    });

    // Spawn baseline recalculator
    let baseline_calc = baseline_calculator.clone();
    let baseline_task = tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
        loop {
            interval.tick().await;
            baseline_calc.maybe_recalculate();
        }
    });

    // Spawn mitigation controller
    let mitigation_task = tokio::spawn(async move {
        mitigation_controller.run().await;
    });

    // Run prober (main loop)
    info!("Starting probe loop");
    tokio::select! {
        _ = prober.run() => {
            info!("Prober exited");
        }
        _ = http_server => {
            info!("HTTP server exited");
        }
        _ = baseline_task => {
            info!("Baseline calculator exited");
        }
        _ = mitigation_task => {
            info!("Mitigation controller exited");
        }
        _ = tokio::signal::ctrl_c() => {
            info!("Received Ctrl+C, shutting down");
        }
    }

    Ok(())
}

/// Handle mitigation events (log and optionally send to webhook)
fn handle_mitigation_event(event: MitigationEvent, webhook_url: Option<&str>) {
    match &event {
        MitigationEvent::DnsRefreshed { endpoint, new_ips } => {
            info!(
                "MITIGATION: DNS refreshed for {} -> {:?}",
                endpoint, new_ips
            );
        }
        MitigationEvent::ConnectionRefreshed { endpoint } => {
            info!("MITIGATION: Connections refreshed for {}", endpoint);
        }
        MitigationEvent::FailoverExecuted { from, to, reason } => {
            info!(
                "MITIGATION: Failover {} -> {} (reason: {})",
                from, to, reason
            );
        }
        MitigationEvent::FailbackExecuted { to } => {
            info!("MITIGATION: Failback to {}", to);
        }
        MitigationEvent::CircuitOpened { endpoint } => {
            info!("MITIGATION: Circuit opened for {}", endpoint);
        }
        MitigationEvent::CircuitClosed { endpoint } => {
            info!("MITIGATION: Circuit closed for {}", endpoint);
        }
    }

    // Send to webhook if configured
    if let Some(url) = webhook_url {
        let url = url.to_string();
        let event_json = serde_json::to_string(&event_to_json(&event)).ok();

        if let Some(json) = event_json {
            tokio::spawn(async move {
                let client = reqwest::Client::new();
                let _ = client
                    .post(&url)
                    .header("Content-Type", "application/json")
                    .body(json)
                    .send()
                    .await;
            });
        }
    }
}

/// Convert mitigation event to JSON-serializable format
fn event_to_json(event: &MitigationEvent) -> serde_json::Value {
    match event {
        MitigationEvent::DnsRefreshed { endpoint, new_ips } => {
            serde_json::json!({
                "type": "dns_refreshed",
                "endpoint": endpoint,
                "new_ips": new_ips.iter().map(|ip| ip.to_string()).collect::<Vec<_>>(),
                "timestamp": chrono::Utc::now().to_rfc3339()
            })
        }
        MitigationEvent::ConnectionRefreshed { endpoint } => {
            serde_json::json!({
                "type": "connection_refreshed",
                "endpoint": endpoint,
                "timestamp": chrono::Utc::now().to_rfc3339()
            })
        }
        MitigationEvent::FailoverExecuted { from, to, reason } => {
            serde_json::json!({
                "type": "failover",
                "from": from,
                "to": to,
                "reason": reason,
                "timestamp": chrono::Utc::now().to_rfc3339()
            })
        }
        MitigationEvent::FailbackExecuted { to } => {
            serde_json::json!({
                "type": "failback",
                "to": to,
                "timestamp": chrono::Utc::now().to_rfc3339()
            })
        }
        MitigationEvent::CircuitOpened { endpoint } => {
            serde_json::json!({
                "type": "circuit_opened",
                "endpoint": endpoint,
                "timestamp": chrono::Utc::now().to_rfc3339()
            })
        }
        MitigationEvent::CircuitClosed { endpoint } => {
            serde_json::json!({
                "type": "circuit_closed",
                "endpoint": endpoint,
                "timestamp": chrono::Utc::now().to_rfc3339()
            })
        }
    }
}

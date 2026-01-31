//! Binance Latency Probe
//!
//! Measures one-way WebSocket latency from Binance market data feeds.
//! Exposes metrics via HTTP for collection by the sweep orchestrator.

use anyhow::{Context, Result};
use axum::{routing::get, Json, Router};
use barter_data::{
    exchange::binance::spot::BinanceSpot,
    streams::{reconnect::Event as ReconnectEvent, Streams},
    subscription::book::OrderBooksL1,
};
use barter_instrument::instrument::market_data::{
    kind::MarketDataInstrumentKind, MarketDataInstrument,
};
use chrono::{DateTime, Utc};
use clap::Parser;
use futures_util::StreamExt;
use parking_lot::RwLock;
use serde::Serialize;
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use tracing::{info, warn};

/// Binance Latency Probe for region/instance benchmarking
#[derive(Parser, Debug, Clone)]
#[command(author, version, about)]
struct Args {
    /// Region identifier (for tagging)
    #[arg(long, env = "PROBE_REGION")]
    region: String,

    /// Instance family (for tagging)
    #[arg(long, env = "PROBE_INSTANCE_FAMILY")]
    instance_family: String,

    /// Experiment ID
    #[arg(long, env = "PROBE_EXPERIMENT_ID")]
    experiment_id: String,

    /// Warmup duration in seconds
    #[arg(long, env = "PROBE_WARMUP_SEC", default_value = "300")]
    warmup_sec: u64,

    /// Metrics HTTP port
    #[arg(long, env = "PROBE_METRICS_PORT", default_value = "9090")]
    metrics_port: u16,

    /// Symbols to track (comma-separated)
    #[arg(long, default_value = "BTCUSDT,ETHUSDT,SOLUSDT,XRPUSDT")]
    symbols: String,
}

// === Latency Histogram ===

/// Logarithmic bucket boundaries in microseconds (1us to 10s)
static BUCKET_BOUNDS_US: &[u64] = &[
    1, 2, 5, 10, 20, 50, 100, 200, 500,
    1_000, 2_000, 5_000, 10_000,
    20_000, 50_000, 100_000,
    200_000, 500_000, 1_000_000,
    2_000_000, 5_000_000, 10_000_000,
    u64::MAX,
];

#[derive(Debug)]
struct LatencyHistogram {
    buckets: Vec<AtomicU64>,
    count: AtomicU64,
    sum_us: AtomicU64,
    min_us: AtomicU64,
    max_us: AtomicU64,
    // Welford's online algorithm for variance
    m2: RwLock<f64>,
    mean: RwLock<f64>,
}

impl LatencyHistogram {
    fn new() -> Self {
        Self {
            buckets: (0..BUCKET_BOUNDS_US.len())
                .map(|_| AtomicU64::new(0))
                .collect(),
            count: AtomicU64::new(0),
            sum_us: AtomicU64::new(0),
            min_us: AtomicU64::new(u64::MAX),
            max_us: AtomicU64::new(0),
            m2: RwLock::new(0.0),
            mean: RwLock::new(0.0),
        }
    }

    #[inline]
    fn record(&self, latency_us: u64) {
        let n = self.count.fetch_add(1, Ordering::Relaxed) + 1;
        self.sum_us.fetch_add(latency_us, Ordering::Relaxed);

        // Update min atomically
        loop {
            let current_min = self.min_us.load(Ordering::Relaxed);
            if latency_us >= current_min {
                break;
            }
            if self
                .min_us
                .compare_exchange_weak(
                    current_min,
                    latency_us,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                )
                .is_ok()
            {
                break;
            }
        }

        // Update max atomically
        loop {
            let current_max = self.max_us.load(Ordering::Relaxed);
            if latency_us <= current_max {
                break;
            }
            if self
                .max_us
                .compare_exchange_weak(
                    current_max,
                    latency_us,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                )
                .is_ok()
            {
                break;
            }
        }

        // Bucket update (binary search would be faster but this is simple)
        let idx = BUCKET_BOUNDS_US
            .iter()
            .position(|&bound| latency_us <= bound)
            .unwrap_or(BUCKET_BOUNDS_US.len() - 1);
        self.buckets[idx].fetch_add(1, Ordering::Relaxed);

        // Welford's online variance (requires lock for accuracy)
        let x = latency_us as f64;
        let mut mean = self.mean.write();
        let mut m2 = self.m2.write();
        let delta = x - *mean;
        *mean += delta / n as f64;
        let delta2 = x - *mean;
        *m2 += delta * delta2;
    }

    fn percentile(&self, p: f64) -> u64 {
        let count = self.count.load(Ordering::Relaxed);
        if count == 0 {
            return 0;
        }

        let target = ((p / 100.0) * count as f64).ceil() as u64;
        let mut cumulative = 0u64;

        for (i, bucket) in self.buckets.iter().enumerate() {
            cumulative += bucket.load(Ordering::Relaxed);
            if cumulative >= target {
                return BUCKET_BOUNDS_US[i];
            }
        }

        self.max_us.load(Ordering::Relaxed)
    }

    fn variance(&self) -> f64 {
        let n = self.count.load(Ordering::Relaxed);
        if n < 2 {
            return 0.0;
        }
        *self.m2.read() / (n - 1) as f64
    }

    fn std_dev(&self) -> f64 {
        self.variance().sqrt()
    }

    fn cv(&self) -> f64 {
        let mean = *self.mean.read();
        if mean == 0.0 {
            return 0.0;
        }
        self.std_dev() / mean
    }

    fn summary(&self) -> HistogramSummary {
        let count = self.count.load(Ordering::Relaxed);
        HistogramSummary {
            count,
            min_us: if count == 0 {
                0
            } else {
                self.min_us.load(Ordering::Relaxed)
            },
            max_us: self.max_us.load(Ordering::Relaxed),
            mean_us: *self.mean.read(),
            std_dev_us: self.std_dev(),
            cv: self.cv(),
            p50_us: self.percentile(50.0),
            p90_us: self.percentile(90.0),
            p95_us: self.percentile(95.0),
            p99_us: self.percentile(99.0),
            p999_us: self.percentile(99.9),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct HistogramSummary {
    count: u64,
    min_us: u64,
    max_us: u64,
    mean_us: f64,
    std_dev_us: f64,
    cv: f64,
    p50_us: u64,
    p90_us: u64,
    p95_us: u64,
    p99_us: u64,
    p999_us: u64,
}

// === Probe State ===

struct ProbeState {
    args: Args,
    start_time: Instant,
    warmup_complete: AtomicU64,
    histograms: HashMap<String, Arc<LatencyHistogram>>,
    aggregate: Arc<LatencyHistogram>,
    reconnect_count: AtomicU64,
    message_count: AtomicU64,
    error_count: AtomicU64,
    recent_samples: RwLock<Vec<LatencySample>>,
}

#[derive(Debug, Clone, Serialize)]
struct LatencySample {
    timestamp: DateTime<Utc>,
    symbol: String,
    exchange_ts_ms: i64,
    receive_ts_ms: i64,
    latency_us: u64,
}

impl ProbeState {
    fn new(args: Args) -> Self {
        let symbols: Vec<String> = args
            .symbols
            .split(',')
            .map(|s| s.trim().to_uppercase())
            .collect();

        let histograms: HashMap<String, Arc<LatencyHistogram>> = symbols
            .iter()
            .map(|s| (s.clone(), Arc::new(LatencyHistogram::new())))
            .collect();

        Self {
            args,
            start_time: Instant::now(),
            warmup_complete: AtomicU64::new(0),
            histograms,
            aggregate: Arc::new(LatencyHistogram::new()),
            reconnect_count: AtomicU64::new(0),
            message_count: AtomicU64::new(0),
            error_count: AtomicU64::new(0),
            recent_samples: RwLock::new(Vec::with_capacity(1000)),
        }
    }

    fn is_warmed_up(&self) -> bool {
        self.warmup_complete.load(Ordering::Relaxed) > 0
    }

    fn mark_warmup_complete(&self) {
        let now = Utc::now().timestamp() as u64;
        self.warmup_complete.store(now, Ordering::Relaxed);
        info!("Warmup complete at {}", now);
    }

    fn record_latency(&self, symbol: &str, exchange_ts_ms: i64, receive_ts_ms: i64) {
        self.message_count.fetch_add(1, Ordering::Relaxed);

        // Skip recording until warmup complete
        if !self.is_warmed_up() {
            return;
        }

        // Calculate one-way latency (exchange → local)
        let latency_us = if receive_ts_ms > exchange_ts_ms {
            ((receive_ts_ms - exchange_ts_ms) * 1000) as u64
        } else {
            // Clock skew - record as 0
            0
        };

        // Record to symbol histogram
        if let Some(hist) = self.histograms.get(symbol) {
            hist.record(latency_us);
        }

        // Record to aggregate
        self.aggregate.record(latency_us);

        // Store recent sample (ring buffer)
        let sample = LatencySample {
            timestamp: Utc::now(),
            symbol: symbol.to_string(),
            exchange_ts_ms,
            receive_ts_ms,
            latency_us,
        };

        let mut recent = self.recent_samples.write();
        if recent.len() >= 1000 {
            recent.remove(0);
        }
        recent.push(sample);
    }
}

// === HTTP API ===

#[derive(Serialize)]
struct MetricsResponse {
    probe_info: ProbeInfo,
    aggregate: HistogramSummary,
    per_symbol: HashMap<String, HistogramSummary>,
    counters: Counters,
    recent_samples: Vec<LatencySample>,
    uptime_sec: u64,
    warmup_complete: bool,
}

#[derive(Serialize)]
struct ProbeInfo {
    region: String,
    instance_family: String,
    experiment_id: String,
    timestamp: DateTime<Utc>,
}

#[derive(Serialize)]
struct Counters {
    messages: u64,
    reconnects: u64,
    errors: u64,
}

async fn get_metrics(
    axum::extract::State(state): axum::extract::State<Arc<ProbeState>>,
) -> Json<MetricsResponse> {
    let per_symbol: HashMap<String, HistogramSummary> = state
        .histograms
        .iter()
        .map(|(k, v)| (k.clone(), v.summary()))
        .collect();

    Json(MetricsResponse {
        probe_info: ProbeInfo {
            region: state.args.region.clone(),
            instance_family: state.args.instance_family.clone(),
            experiment_id: state.args.experiment_id.clone(),
            timestamp: Utc::now(),
        },
        aggregate: state.aggregate.summary(),
        per_symbol,
        counters: Counters {
            messages: state.message_count.load(Ordering::Relaxed),
            reconnects: state.reconnect_count.load(Ordering::Relaxed),
            errors: state.error_count.load(Ordering::Relaxed),
        },
        recent_samples: state.recent_samples.read().clone(),
        uptime_sec: state.start_time.elapsed().as_secs(),
        warmup_complete: state.is_warmed_up(),
    })
}

async fn health() -> &'static str {
    "OK"
}

// === Main ===

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing with JSON format for structured logging
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("binance_latency_probe=info".parse().unwrap()),
        )
        .json()
        .init();

    let args = Args::parse();
    info!(?args, "Starting Binance latency probe");

    let state = Arc::new(ProbeState::new(args.clone()));

    // Start metrics server
    let metrics_state = state.clone();
    let metrics_port = args.metrics_port;
    tokio::spawn(async move {
        let app = Router::new()
            .route("/metrics", get(get_metrics))
            .route("/health", get(health))
            .with_state(metrics_state);

        let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{}", metrics_port))
            .await
            .expect("Failed to bind metrics port");

        info!("Metrics server listening on port {}", metrics_port);
        axum::serve(listener, app)
            .await
            .expect("Metrics server failed");
    });

    // Warmup timer
    let warmup_state = state.clone();
    let warmup_sec = args.warmup_sec;
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(warmup_sec)).await;
        warmup_state.mark_warmup_complete();
    });

    // Initialize Binance streams
    let streams = init_streams().await?;
    let mut joined = streams.select_all();

    while let Some(event) = joined.next().await {
        match event {
            ReconnectEvent::Reconnecting(exchange) => {
                warn!(?exchange, "WebSocket reconnecting");
                state.reconnect_count.fetch_add(1, Ordering::Relaxed);
            }
            ReconnectEvent::Item(result) => match result {
                Ok(market_event) => {
                    let receive_ts_ms = Utc::now().timestamp_millis();
                    let exchange_ts_ms = market_event.time_exchange.timestamp_millis();

                    let symbol = format!(
                        "{}{}",
                        market_event.instrument.base, market_event.instrument.quote
                    )
                    .to_uppercase();

                    state.record_latency(&symbol, exchange_ts_ms, receive_ts_ms);
                }
                Err(e) => {
                    warn!(error = %e, "Stream error");
                    state.error_count.fetch_add(1, Ordering::Relaxed);
                }
            },
        }
    }

    Ok(())
}

async fn init_streams() -> Result<
    Streams<
        barter_data::streams::consumer::MarketStreamResult<
            MarketDataInstrument,
            barter_data::subscription::book::OrderBookL1,
        >,
    >,
> {
    Streams::<OrderBooksL1>::builder()
        .subscribe([
            (
                BinanceSpot::default(),
                "btc",
                "usdt",
                MarketDataInstrumentKind::Spot,
                OrderBooksL1,
            ),
            (
                BinanceSpot::default(),
                "eth",
                "usdt",
                MarketDataInstrumentKind::Spot,
                OrderBooksL1,
            ),
            (
                BinanceSpot::default(),
                "sol",
                "usdt",
                MarketDataInstrumentKind::Spot,
                OrderBooksL1,
            ),
            (
                BinanceSpot::default(),
                "xrp",
                "usdt",
                MarketDataInstrumentKind::Spot,
                OrderBooksL1,
            ),
        ])
        .init()
        .await
        .context("Failed to init Binance streams")
}

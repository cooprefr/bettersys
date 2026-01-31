//! Async Metrics Collection for BinanceBookTickerFeed
//!
//! Collects and exports metrics OFF the hot path.
//! Periodically samples the feed metrics and logs/exports them.

use std::sync::Arc;
use std::time::Duration;
use tokio::time::interval;

use super::binance_book_ticker::{BinanceBookTickerFeed, GapEvent, Symbol};

/// Metrics snapshot for export
#[derive(Debug, Clone, serde::Serialize)]
pub struct BookTickerMetricsSnapshot {
    pub timestamp_ms: i64,
    
    // Decode latency
    pub decode_latency_mean_us: f64,
    pub decode_latency_max_us: f64,
    pub decode_latency_count: u64,
    
    // Jitter
    pub jitter_mean_us: f64,
    pub jitter_max_us: f64,
    
    // Per-symbol staleness
    pub btcusdt_staleness_ms: u64,
    pub ethusdt_staleness_ms: u64,
    pub solusdt_staleness_ms: u64,
    pub xrpusdt_staleness_ms: u64,
    
    // Counters
    pub messages_received: u64,
    pub parse_errors: u64,
    pub gaps_total: u64,
    pub reconnects: u64,
    
    // Connection state
    pub connected: bool,
}

impl BookTickerMetricsSnapshot {
    pub fn from_feed(feed: &BinanceBookTickerFeed) -> Self {
        let metrics = feed.metrics();
        let now_ms = chrono::Utc::now().timestamp_millis();
        
        Self {
            timestamp_ms: now_ms,
            decode_latency_mean_us: metrics.decode_latency_mean_ns() / 1000.0,
            decode_latency_max_us: metrics.decode_latency_max_ns.load(std::sync::atomic::Ordering::Relaxed) as f64 / 1000.0,
            decode_latency_count: metrics.decode_latency_count.load(std::sync::atomic::Ordering::Relaxed),
            jitter_mean_us: metrics.jitter_mean_ns() / 1000.0,
            jitter_max_us: metrics.jitter_max_ns.load(std::sync::atomic::Ordering::Relaxed) as f64 / 1000.0,
            btcusdt_staleness_ms: feed.time_since_update_ns(Symbol::BtcUsdt) / 1_000_000,
            ethusdt_staleness_ms: feed.time_since_update_ns(Symbol::EthUsdt) / 1_000_000,
            solusdt_staleness_ms: feed.time_since_update_ns(Symbol::SolUsdt) / 1_000_000,
            xrpusdt_staleness_ms: feed.time_since_update_ns(Symbol::XrpUsdt) / 1_000_000,
            messages_received: metrics.messages_received.load(std::sync::atomic::Ordering::Relaxed),
            parse_errors: metrics.parse_errors.load(std::sync::atomic::Ordering::Relaxed),
            gaps_total: metrics.gaps_total.load(std::sync::atomic::Ordering::Relaxed),
            reconnects: metrics.reconnects.load(std::sync::atomic::Ordering::Relaxed),
            connected: feed.is_connected(),
        }
    }
}

/// Async metrics collector task
pub struct MetricsCollector {
    feed: Arc<BinanceBookTickerFeed>,
    collection_interval: Duration,
    export_tx: Option<tokio::sync::mpsc::Sender<BookTickerMetricsSnapshot>>,
}

impl MetricsCollector {
    pub fn new(
        feed: Arc<BinanceBookTickerFeed>,
        collection_interval: Duration,
    ) -> Self {
        Self {
            feed,
            collection_interval,
            export_tx: None,
        }
    }
    
    pub fn with_export_channel(
        mut self,
        tx: tokio::sync::mpsc::Sender<BookTickerMetricsSnapshot>,
    ) -> Self {
        self.export_tx = Some(tx);
        self
    }
    
    /// Run the metrics collection loop (spawn this as a background task)
    pub async fn run(self) {
        let mut ticker = interval(self.collection_interval);
        
        loop {
            ticker.tick().await;
            
            let snapshot = BookTickerMetricsSnapshot::from_feed(&self.feed);
            
            // Export via channel if configured
            if let Some(ref tx) = self.export_tx {
                let _ = tx.try_send(snapshot.clone());
            }
            
            // Record to global latency registry (for dashboard integration)
            {
                let decode_us = snapshot.decode_latency_mean_us as u64;
                if decode_us > 0 {
                    crate::latency::global_registry().record_span(
                        crate::latency::LatencySpan::new(
                            crate::latency::SpanType::BinanceWs,
                            decode_us,
                        ),
                    );
                }
            }
            
            // Log periodically (every 10th collection)
            static LOG_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let count = LOG_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if count % 10 == 0 {
                tracing::debug!(
                    decode_mean_us = %format!("{:.1}", snapshot.decode_latency_mean_us),
                    decode_max_us = %format!("{:.1}", snapshot.decode_latency_max_us),
                    jitter_mean_us = %format!("{:.1}", snapshot.jitter_mean_us),
                    messages = snapshot.messages_received,
                    gaps = snapshot.gaps_total,
                    connected = snapshot.connected,
                    "binance_book_ticker metrics"
                );
            }
        }
    }
}

/// Gap event handler task
pub struct GapHandler {
    gap_rx: tokio::sync::mpsc::UnboundedReceiver<GapEvent>,
}

impl GapHandler {
    pub fn new(gap_rx: tokio::sync::mpsc::UnboundedReceiver<GapEvent>) -> Self {
        Self { gap_rx }
    }
    
    /// Run the gap handler loop (logs gaps without blocking hot path)
    pub async fn run(mut self) {
        while let Some(gap) = self.gap_rx.recv().await {
            // Log gap event
            tracing::warn!(
                symbol = %gap.symbol.as_str(),
                expected = gap.expected,
                received = gap.received,
                gap_size = gap.gap_size,
                "binance sequence gap detected"
            );
            
            // Record to comprehensive metrics (gaps are tracked via record_message with sequence)
            // The gap is already detected and logged; we record it to the integrity tracker
            crate::latency::global_comprehensive()
                .md_integrity
                .record_message("binance_book_ticker", Some(gap.received), None);
        }
    }
}

/// Spawn all background tasks for the feed
pub fn spawn_metrics_tasks(
    feed: Arc<BinanceBookTickerFeed>,
    gap_rx: tokio::sync::mpsc::UnboundedReceiver<GapEvent>,
    metrics_interval: Duration,
) -> tokio::sync::mpsc::Receiver<BookTickerMetricsSnapshot> {
    let (metrics_tx, metrics_rx) = tokio::sync::mpsc::channel(100);
    
    // Spawn metrics collector
    let collector = MetricsCollector::new(feed, metrics_interval)
        .with_export_channel(metrics_tx);
    tokio::spawn(collector.run());
    
    // Spawn gap handler
    let gap_handler = GapHandler::new(gap_rx);
    tokio::spawn(gap_handler.run());
    
    metrics_rx
}

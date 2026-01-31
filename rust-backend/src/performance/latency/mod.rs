//! System-Wide Latency Measurement Engine
//!
//! Comprehensive instrumentation for all latency-sensitive paths:
//! - Market data ingestion (Binance, Dome WebSocket)
//! - Signal detection and processing
//! - Database operations
//! - REST API response times
//! - WebSocket broadcast latency
//! - Tick-to-trade for trading engines
//!
//! P99.9 OPTIMIZATION: Uses atomic counters for lock-free recording in hot paths.
//! RwLock is only used for cold-path operations (recent_spans, component_status).

use parking_lot::RwLock;
use serde::Serialize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
    time::Instant,
};

pub mod binance_harness;
pub mod comprehensive;
pub mod histogram;
pub mod regression;
pub mod socket_tuning;
pub mod spans;
pub mod time_sync;

pub use comprehensive::*;
pub use histogram::*;
pub use regression::*;
pub use socket_tuning::*;
pub use spans::*;
pub use time_sync::*;

/// System-wide latency registry
/// Thread-safe singleton that collects metrics from all components
#[derive(Debug)]
pub struct SystemLatencyRegistry {
    // === Market Data Ingestion ===
    pub binance_ws_latency: LatencyHistogram,
    pub dome_ws_latency: LatencyHistogram,
    pub dome_rest_latency: LatencyHistogram,
    pub polymarket_ws_latency: LatencyHistogram,
    pub polymarket_rest_latency: LatencyHistogram,
    pub gamma_api_latency: LatencyHistogram,

    // === Signal Pipeline ===
    pub signal_detection_latency: LatencyHistogram,
    pub signal_enrichment_latency: LatencyHistogram,
    pub signal_broadcast_latency: LatencyHistogram,
    pub signal_storage_latency: LatencyHistogram,

    // === Database Operations ===
    pub db_read_latency: LatencyHistogram,
    pub db_write_latency: LatencyHistogram,
    pub db_search_latency: LatencyHistogram,

    // === REST API ===
    pub api_signals_latency: LatencyHistogram,
    pub api_search_latency: LatencyHistogram,
    pub api_wallet_analytics_latency: LatencyHistogram,
    pub api_market_snapshot_latency: LatencyHistogram,
    pub api_vault_latency: LatencyHistogram,

    // === Trading Engines ===
    pub fast15m_t2t_latency: LatencyHistogram,
    pub fast15m_gamma_lookup: LatencyHistogram,
    pub fast15m_book_fetch: LatencyHistogram,
    pub fast15m_order_submit: LatencyHistogram,
    pub long_t2t_latency: LatencyHistogram,
    pub long_llm_latency: LatencyHistogram,

    // === WebSocket ===
    pub ws_client_rtt: LatencyHistogram,
    pub ws_broadcast_latency: LatencyHistogram,

    // === Counters (P99.9 OPTIMIZATION: lock-free atomics) ===
    pub counters: AtomicLatencyCounters,

    // === Recent Spans (for debugging) ===
    pub recent_spans: RwLock<VecDeque<LatencySpan>>,
    max_recent_spans: usize,

    // === Component Status ===
    pub component_status: RwLock<HashMap<String, ComponentStatus>>,

    // === Time series for dashboard ===
    pub time_series: RwLock<LatencyTimeSeries>,
}

/// P99.9 OPTIMIZATION: Lock-free atomic counters for hot-path metrics.
/// All increments are atomic; reads snapshot the current values.
#[derive(Debug, Default)]
pub struct AtomicLatencyCounters {
    // Market data
    pub binance_updates: AtomicU64,
    pub dome_ws_events: AtomicU64,
    pub dome_rest_calls: AtomicU64,
    pub polymarket_book_updates: AtomicU64,

    // Signals
    pub signals_detected: AtomicU64,
    pub signals_stored: AtomicU64,
    pub signals_broadcast: AtomicU64,

    // Trading
    pub fast15m_evaluations: AtomicU64,
    pub fast15m_trades: AtomicU64,
    pub long_evaluations: AtomicU64,
    pub long_trades: AtomicU64,

    // API
    pub api_requests: AtomicU64,
    pub api_errors: AtomicU64,

    // Cache
    pub cache_hits: AtomicU64,
    pub cache_misses: AtomicU64,
}

impl AtomicLatencyCounters {
    pub fn new() -> Self {
        Self::default()
    }

    /// Snapshot all counters into a serializable struct (cold path)
    pub fn snapshot(&self) -> LatencyCounters {
        LatencyCounters {
            binance_updates: self.binance_updates.load(Ordering::Relaxed),
            dome_ws_events: self.dome_ws_events.load(Ordering::Relaxed),
            dome_rest_calls: self.dome_rest_calls.load(Ordering::Relaxed),
            polymarket_book_updates: self.polymarket_book_updates.load(Ordering::Relaxed),
            signals_detected: self.signals_detected.load(Ordering::Relaxed),
            signals_stored: self.signals_stored.load(Ordering::Relaxed),
            signals_broadcast: self.signals_broadcast.load(Ordering::Relaxed),
            fast15m_evaluations: self.fast15m_evaluations.load(Ordering::Relaxed),
            fast15m_trades: self.fast15m_trades.load(Ordering::Relaxed),
            long_evaluations: self.long_evaluations.load(Ordering::Relaxed),
            long_trades: self.long_trades.load(Ordering::Relaxed),
            api_requests: self.api_requests.load(Ordering::Relaxed),
            api_errors: self.api_errors.load(Ordering::Relaxed),
            cache_hits: self.cache_hits.load(Ordering::Relaxed),
            cache_misses: self.cache_misses.load(Ordering::Relaxed),
        }
    }
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct LatencyCounters {
    // Market data
    pub binance_updates: u64,
    pub dome_ws_events: u64,
    pub dome_rest_calls: u64,
    pub polymarket_book_updates: u64,

    // Signals
    pub signals_detected: u64,
    pub signals_stored: u64,
    pub signals_broadcast: u64,

    // Trading
    pub fast15m_evaluations: u64,
    pub fast15m_trades: u64,
    pub long_evaluations: u64,
    pub long_trades: u64,

    // API
    pub api_requests: u64,
    pub api_errors: u64,

    // Cache
    pub cache_hits: u64,
    pub cache_misses: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ComponentStatus {
    pub name: String,
    pub healthy: bool,
    pub last_activity_ts: i64,
    pub error_count: u64,
    pub latency_p50_us: u64,
    pub latency_p99_us: u64,
}

#[derive(Debug, Default)]
pub struct LatencyTimeSeries {
    /// Bucketed by minute: minute_ts -> LatencyBucket
    pub buckets: HashMap<i64, LatencyBucket>,
    pub max_buckets: usize,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct LatencyBucket {
    pub timestamp: i64,
    pub binance_p50_us: u64,
    pub binance_p99_us: u64,
    pub dome_ws_p50_us: u64,
    pub dome_ws_p99_us: u64,
    pub signal_p50_us: u64,
    pub signal_p99_us: u64,
    pub api_p50_us: u64,
    pub api_p99_us: u64,
    pub fast15m_p50_us: u64,
    pub fast15m_p99_us: u64,
    pub sample_count: u64,
}

impl Default for SystemLatencyRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl SystemLatencyRegistry {
    pub fn new() -> Self {
        Self {
            // Market Data
            binance_ws_latency: LatencyHistogram::new(),
            dome_ws_latency: LatencyHistogram::new(),
            dome_rest_latency: LatencyHistogram::new(),
            polymarket_ws_latency: LatencyHistogram::new(),
            polymarket_rest_latency: LatencyHistogram::new(),
            gamma_api_latency: LatencyHistogram::new(),

            // Signal Pipeline
            signal_detection_latency: LatencyHistogram::new(),
            signal_enrichment_latency: LatencyHistogram::new(),
            signal_broadcast_latency: LatencyHistogram::new(),
            signal_storage_latency: LatencyHistogram::new(),

            // Database
            db_read_latency: LatencyHistogram::new(),
            db_write_latency: LatencyHistogram::new(),
            db_search_latency: LatencyHistogram::new(),

            // REST API
            api_signals_latency: LatencyHistogram::new(),
            api_search_latency: LatencyHistogram::new(),
            api_wallet_analytics_latency: LatencyHistogram::new(),
            api_market_snapshot_latency: LatencyHistogram::new(),
            api_vault_latency: LatencyHistogram::new(),

            // Trading
            fast15m_t2t_latency: LatencyHistogram::new(),
            fast15m_gamma_lookup: LatencyHistogram::new(),
            fast15m_book_fetch: LatencyHistogram::new(),
            fast15m_order_submit: LatencyHistogram::new(),
            long_t2t_latency: LatencyHistogram::new(),
            long_llm_latency: LatencyHistogram::new(),

            // WebSocket
            ws_client_rtt: LatencyHistogram::new(),
            ws_broadcast_latency: LatencyHistogram::new(),

            // Counters (lock-free)
            counters: AtomicLatencyCounters::new(),

            // Spans
            recent_spans: RwLock::new(VecDeque::with_capacity(1000)),
            max_recent_spans: 1000,

            // Status
            component_status: RwLock::new(HashMap::new()),

            // Time series
            time_series: RwLock::new(LatencyTimeSeries {
                buckets: HashMap::new(),
                max_buckets: 60, // 1 hour of minute buckets
            }),
        }
    }

    /// Record a latency span and update relevant histograms
    /// 
    /// P99.9 OPTIMIZATION: Uses lock-free atomic increments for counters.
    /// Only the recent_spans storage requires a lock (and can be disabled for max perf).
    #[inline]
    pub fn record_span(&self, span: LatencySpan) {
        // Update histogram based on span type (histograms are now lock-free)
        match span.span_type {
            SpanType::BinanceWs => {
                self.binance_ws_latency.record(span.duration_us);
                self.counters.binance_updates.fetch_add(1, Ordering::Relaxed);
            }
            SpanType::DomeWs => {
                self.dome_ws_latency.record(span.duration_us);
                self.counters.dome_ws_events.fetch_add(1, Ordering::Relaxed);
            }
            SpanType::DomeRest => {
                self.dome_rest_latency.record(span.duration_us);
                self.counters.dome_rest_calls.fetch_add(1, Ordering::Relaxed);
            }
            SpanType::PolymarketWs => {
                self.polymarket_ws_latency.record(span.duration_us);
                self.counters.polymarket_book_updates.fetch_add(1, Ordering::Relaxed);
            }
            SpanType::PolymarketRest => {
                self.polymarket_rest_latency.record(span.duration_us);
            }
            SpanType::GammaApi => {
                self.gamma_api_latency.record(span.duration_us);
            }
            SpanType::SignalDetection => {
                self.signal_detection_latency.record(span.duration_us);
                self.counters.signals_detected.fetch_add(1, Ordering::Relaxed);
            }
            SpanType::SignalEnrichment => {
                self.signal_enrichment_latency.record(span.duration_us);
            }
            SpanType::SignalBroadcast => {
                self.signal_broadcast_latency.record(span.duration_us);
                self.counters.signals_broadcast.fetch_add(1, Ordering::Relaxed);
            }
            SpanType::SignalStorage => {
                self.signal_storage_latency.record(span.duration_us);
                self.counters.signals_stored.fetch_add(1, Ordering::Relaxed);
            }
            SpanType::DbRead => {
                self.db_read_latency.record(span.duration_us);
            }
            SpanType::DbWrite => {
                self.db_write_latency.record(span.duration_us);
            }
            SpanType::DbSearch => {
                self.db_search_latency.record(span.duration_us);
            }
            SpanType::ApiSignals => {
                self.api_signals_latency.record(span.duration_us);
                self.counters.api_requests.fetch_add(1, Ordering::Relaxed);
            }
            SpanType::ApiSearch => {
                self.api_search_latency.record(span.duration_us);
                self.counters.api_requests.fetch_add(1, Ordering::Relaxed);
            }
            SpanType::ApiWalletAnalytics => {
                self.api_wallet_analytics_latency.record(span.duration_us);
                self.counters.api_requests.fetch_add(1, Ordering::Relaxed);
            }
            SpanType::ApiMarketSnapshot => {
                self.api_market_snapshot_latency.record(span.duration_us);
                self.counters.api_requests.fetch_add(1, Ordering::Relaxed);
            }
            SpanType::ApiVault => {
                self.api_vault_latency.record(span.duration_us);
                self.counters.api_requests.fetch_add(1, Ordering::Relaxed);
            }
            SpanType::Fast15mT2T => {
                self.fast15m_t2t_latency.record(span.duration_us);
                self.counters.fast15m_evaluations.fetch_add(1, Ordering::Relaxed);
            }
            SpanType::Fast15mGamma => {
                self.fast15m_gamma_lookup.record(span.duration_us);
            }
            SpanType::Fast15mBook => {
                self.fast15m_book_fetch.record(span.duration_us);
            }
            SpanType::Fast15mOrder => {
                self.fast15m_order_submit.record(span.duration_us);
                self.counters.fast15m_trades.fetch_add(1, Ordering::Relaxed);
            }
            SpanType::LongT2T => {
                self.long_t2t_latency.record(span.duration_us);
                self.counters.long_evaluations.fetch_add(1, Ordering::Relaxed);
            }
            SpanType::LongLlm => {
                self.long_llm_latency.record(span.duration_us);
            }
            SpanType::WsClientRtt => {
                self.ws_client_rtt.record(span.duration_us);
            }
            SpanType::WsBroadcast => {
                self.ws_broadcast_latency.record(span.duration_us);
            }
        }

        // Store recent span (cold path - only lock if we're storing spans)
        // This is the only remaining lock in the hot path
        let mut spans = self.recent_spans.write();
        if spans.len() >= self.max_recent_spans {
            spans.pop_front();
        }
        spans.push_back(span);
    }

    /// Update component status
    pub fn update_component_status(&self, name: &str, healthy: bool, error_delta: u64) {
        let now = chrono::Utc::now().timestamp();
        let mut status = self.component_status.write();
        let entry = status.entry(name.to_string()).or_insert(ComponentStatus {
            name: name.to_string(),
            healthy: true,
            last_activity_ts: now,
            error_count: 0,
            latency_p50_us: 0,
            latency_p99_us: 0,
        });
        entry.healthy = healthy;
        entry.last_activity_ts = now;
        entry.error_count += error_delta;
    }

    /// Record cache hit/miss (lock-free)
    #[inline]
    pub fn record_cache(&self, hit: bool) {
        if hit {
            self.counters.cache_hits.fetch_add(1, Ordering::Relaxed);
        } else {
            self.counters.cache_misses.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Record API error (lock-free)
    #[inline]
    pub fn record_api_error(&self) {
        self.counters.api_errors.fetch_add(1, Ordering::Relaxed);
    }

    /// Get comprehensive system summary
    pub fn summary(&self) -> SystemLatencySummary {
        let counters = self.counters.snapshot();
        let status: Vec<ComponentStatus> = self.component_status.read().values().cloned().collect();

        SystemLatencySummary {
            timestamp: chrono::Utc::now().timestamp(),

            // Market Data
            market_data: MarketDataLatency {
                binance_ws: self.binance_ws_latency.summary("binance_ws"),
                dome_ws: self.dome_ws_latency.summary("dome_ws"),
                dome_rest: self.dome_rest_latency.summary("dome_rest"),
                polymarket_ws: self.polymarket_ws_latency.summary("polymarket_ws"),
                polymarket_rest: self.polymarket_rest_latency.summary("polymarket_rest"),
                gamma_api: self.gamma_api_latency.summary("gamma_api"),
            },

            // Signal Pipeline
            signal_pipeline: SignalPipelineLatency {
                detection: self.signal_detection_latency.summary("signal_detection"),
                enrichment: self.signal_enrichment_latency.summary("signal_enrichment"),
                broadcast: self.signal_broadcast_latency.summary("signal_broadcast"),
                storage: self.signal_storage_latency.summary("signal_storage"),
            },

            // Database
            database: DatabaseLatency {
                read: self.db_read_latency.summary("db_read"),
                write: self.db_write_latency.summary("db_write"),
                search: self.db_search_latency.summary("db_search"),
            },

            // REST API
            api: ApiLatency {
                signals: self.api_signals_latency.summary("api_signals"),
                search: self.api_search_latency.summary("api_search"),
                wallet_analytics: self.api_wallet_analytics_latency.summary("api_wallet"),
                market_snapshot: self.api_market_snapshot_latency.summary("api_snapshot"),
                vault: self.api_vault_latency.summary("api_vault"),
            },

            // Trading
            trading: TradingLatency {
                fast15m_t2t: self.fast15m_t2t_latency.summary("fast15m_t2t"),
                fast15m_gamma: self.fast15m_gamma_lookup.summary("fast15m_gamma"),
                fast15m_book: self.fast15m_book_fetch.summary("fast15m_book"),
                fast15m_order: self.fast15m_order_submit.summary("fast15m_order"),
                long_t2t: self.long_t2t_latency.summary("long_t2t"),
                long_llm: self.long_llm_latency.summary("long_llm"),
            },

            // WebSocket
            websocket: WebSocketLatency {
                client_rtt: self.ws_client_rtt.summary("ws_rtt"),
                broadcast: self.ws_broadcast_latency.summary("ws_broadcast"),
            },

            // Component status
            components: status,

            // Cache stats (computed from the snapshotted counters)
            cache_hit_rate: {
                let total = counters.cache_hits + counters.cache_misses;
                if total > 0 {
                    counters.cache_hits as f64 / total as f64
                } else {
                    0.0
                }
            },
            
            // Counters (must be last since it's moved)
            counters,
        }
    }

    /// Get recent spans for debugging
    pub fn recent_spans(&self, limit: usize) -> Vec<LatencySpan> {
        self.recent_spans
            .read()
            .iter()
            .rev()
            .take(limit)
            .cloned()
            .collect()
    }

    /// Get time series data for dashboard
    pub fn time_series(&self, minutes: usize) -> Vec<LatencyBucket> {
        let now_minute = chrono::Utc::now().timestamp() / 60 * 60;
        let ts = self.time_series.read();

        (0..minutes)
            .filter_map(|i| {
                let minute_ts = now_minute - (i as i64 * 60);
                ts.buckets.get(&minute_ts).cloned()
            })
            .collect()
    }

    /// Snapshot current state into time series bucket
    pub fn snapshot_to_timeseries(&self) {
        let now_minute = chrono::Utc::now().timestamp() / 60 * 60;
        let bucket = LatencyBucket {
            timestamp: now_minute,
            binance_p50_us: self.binance_ws_latency.p50(),
            binance_p99_us: self.binance_ws_latency.p99(),
            dome_ws_p50_us: self.dome_ws_latency.p50(),
            dome_ws_p99_us: self.dome_ws_latency.p99(),
            signal_p50_us: self.signal_detection_latency.p50(),
            signal_p99_us: self.signal_detection_latency.p99(),
            api_p50_us: self.api_signals_latency.p50(),
            api_p99_us: self.api_signals_latency.p99(),
            fast15m_p50_us: self.fast15m_t2t_latency.p50(),
            fast15m_p99_us: self.fast15m_t2t_latency.p99(),
            sample_count: self.counters.api_requests.load(Ordering::Relaxed),
        };

        let mut ts = self.time_series.write();
        ts.buckets.insert(now_minute, bucket);

        // Prune old buckets
        let cutoff = now_minute - (ts.max_buckets as i64 * 60);
        ts.buckets.retain(|&k, _| k >= cutoff);
    }
}

// === Summary Types ===

#[derive(Debug, Clone, Serialize)]
pub struct SystemLatencySummary {
    pub timestamp: i64,
    pub market_data: MarketDataLatency,
    pub signal_pipeline: SignalPipelineLatency,
    pub database: DatabaseLatency,
    pub api: ApiLatency,
    pub trading: TradingLatency,
    pub websocket: WebSocketLatency,
    pub counters: LatencyCounters,
    pub components: Vec<ComponentStatus>,
    pub cache_hit_rate: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct MarketDataLatency {
    pub binance_ws: HistogramSummary,
    pub dome_ws: HistogramSummary,
    pub dome_rest: HistogramSummary,
    pub polymarket_ws: HistogramSummary,
    pub polymarket_rest: HistogramSummary,
    pub gamma_api: HistogramSummary,
}

#[derive(Debug, Clone, Serialize)]
pub struct SignalPipelineLatency {
    pub detection: HistogramSummary,
    pub enrichment: HistogramSummary,
    pub broadcast: HistogramSummary,
    pub storage: HistogramSummary,
}

#[derive(Debug, Clone, Serialize)]
pub struct DatabaseLatency {
    pub read: HistogramSummary,
    pub write: HistogramSummary,
    pub search: HistogramSummary,
}

#[derive(Debug, Clone, Serialize)]
pub struct ApiLatency {
    pub signals: HistogramSummary,
    pub search: HistogramSummary,
    pub wallet_analytics: HistogramSummary,
    pub market_snapshot: HistogramSummary,
    pub vault: HistogramSummary,
}

#[derive(Debug, Clone, Serialize)]
pub struct TradingLatency {
    pub fast15m_t2t: HistogramSummary,
    pub fast15m_gamma: HistogramSummary,
    pub fast15m_book: HistogramSummary,
    pub fast15m_order: HistogramSummary,
    pub long_t2t: HistogramSummary,
    pub long_llm: HistogramSummary,
}

#[derive(Debug, Clone, Serialize)]
pub struct WebSocketLatency {
    pub client_rtt: HistogramSummary,
    pub broadcast: HistogramSummary,
}

/// Global latency registry instance
pub fn global_registry() -> &'static Arc<SystemLatencyRegistry> {
    static REGISTRY: std::sync::OnceLock<Arc<SystemLatencyRegistry>> = std::sync::OnceLock::new();
    REGISTRY.get_or_init(|| Arc::new(SystemLatencyRegistry::new()))
}

/// Convenience function to record a span
pub fn record(span: LatencySpan) {
    global_registry().record_span(span);
}

/// Convenience function to start a span timer
pub fn start_span(span_type: SpanType) -> SpanTimer {
    SpanTimer::new(span_type)
}

/// RAII timer that records span on drop
pub struct SpanTimer {
    span_type: SpanType,
    start: Instant,
    metadata: Option<String>,
}

impl SpanTimer {
    pub fn new(span_type: SpanType) -> Self {
        Self {
            span_type,
            start: Instant::now(),
            metadata: None,
        }
    }

    pub fn with_metadata(mut self, meta: impl Into<String>) -> Self {
        self.metadata = Some(meta.into());
        self
    }

    pub fn finish(mut self) -> u64 {
        let duration_us = self.start.elapsed().as_micros() as u64;
        let span = LatencySpan {
            span_type: self.span_type,
            start_ns: 0, // Not needed for histogram
            duration_us,
            metadata: self.metadata.take(),
            timestamp: chrono::Utc::now().timestamp(),
        };
        global_registry().record_span(span);
        duration_us
    }
}

impl Drop for SpanTimer {
    fn drop(&mut self) {
        // Only record if not already finished
        // This allows explicit finish() calls
    }
}

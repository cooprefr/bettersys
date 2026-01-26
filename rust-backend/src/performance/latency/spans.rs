//! Latency span types for detailed tracing

use serde::Serialize;

/// Type of latency span
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SpanType {
    // Market Data
    BinanceWs,
    DomeWs,
    DomeRest,
    PolymarketWs,
    PolymarketRest,
    GammaApi,

    // Signal Pipeline
    SignalDetection,
    SignalEnrichment,
    SignalBroadcast,
    SignalStorage,

    // Database
    DbRead,
    DbWrite,
    DbSearch,

    // REST API
    ApiSignals,
    ApiSearch,
    ApiWalletAnalytics,
    ApiMarketSnapshot,
    ApiVault,

    // Trading Engines
    Fast15mT2T,
    Fast15mGamma,
    Fast15mBook,
    Fast15mOrder,
    LongT2T,
    LongLlm,

    // WebSocket
    WsClientRtt,
    WsBroadcast,
}

impl SpanType {
    pub fn as_str(&self) -> &'static str {
        match self {
            SpanType::BinanceWs => "binance_ws",
            SpanType::DomeWs => "dome_ws",
            SpanType::DomeRest => "dome_rest",
            SpanType::PolymarketWs => "polymarket_ws",
            SpanType::PolymarketRest => "polymarket_rest",
            SpanType::GammaApi => "gamma_api",
            SpanType::SignalDetection => "signal_detection",
            SpanType::SignalEnrichment => "signal_enrichment",
            SpanType::SignalBroadcast => "signal_broadcast",
            SpanType::SignalStorage => "signal_storage",
            SpanType::DbRead => "db_read",
            SpanType::DbWrite => "db_write",
            SpanType::DbSearch => "db_search",
            SpanType::ApiSignals => "api_signals",
            SpanType::ApiSearch => "api_search",
            SpanType::ApiWalletAnalytics => "api_wallet_analytics",
            SpanType::ApiMarketSnapshot => "api_market_snapshot",
            SpanType::ApiVault => "api_vault",
            SpanType::Fast15mT2T => "fast15m_t2t",
            SpanType::Fast15mGamma => "fast15m_gamma",
            SpanType::Fast15mBook => "fast15m_book",
            SpanType::Fast15mOrder => "fast15m_order",
            SpanType::LongT2T => "long_t2t",
            SpanType::LongLlm => "long_llm",
            SpanType::WsClientRtt => "ws_client_rtt",
            SpanType::WsBroadcast => "ws_broadcast",
        }
    }

    pub fn category(&self) -> &'static str {
        match self {
            SpanType::BinanceWs
            | SpanType::DomeWs
            | SpanType::DomeRest
            | SpanType::PolymarketWs
            | SpanType::PolymarketRest
            | SpanType::GammaApi => "market_data",
            SpanType::SignalDetection
            | SpanType::SignalEnrichment
            | SpanType::SignalBroadcast
            | SpanType::SignalStorage => "signal_pipeline",
            SpanType::DbRead | SpanType::DbWrite | SpanType::DbSearch => "database",
            SpanType::ApiSignals
            | SpanType::ApiSearch
            | SpanType::ApiWalletAnalytics
            | SpanType::ApiMarketSnapshot
            | SpanType::ApiVault => "api",
            SpanType::Fast15mT2T
            | SpanType::Fast15mGamma
            | SpanType::Fast15mBook
            | SpanType::Fast15mOrder
            | SpanType::LongT2T
            | SpanType::LongLlm => "trading",
            SpanType::WsClientRtt | SpanType::WsBroadcast => "websocket",
        }
    }
}

/// A single latency measurement span
#[derive(Debug, Clone, Serialize)]
pub struct LatencySpan {
    pub span_type: SpanType,
    pub start_ns: u64,
    pub duration_us: u64,
    pub metadata: Option<String>,
    pub timestamp: i64,
}

impl LatencySpan {
    pub fn new(span_type: SpanType, duration_us: u64) -> Self {
        Self {
            span_type,
            start_ns: 0,
            duration_us,
            metadata: None,
            timestamp: chrono::Utc::now().timestamp(),
        }
    }

    pub fn with_metadata(mut self, meta: impl Into<String>) -> Self {
        self.metadata = Some(meta.into());
        self
    }
}

/// Builder for creating spans with timing
pub struct SpanBuilder {
    span_type: SpanType,
    start: std::time::Instant,
    metadata: Option<String>,
}

impl SpanBuilder {
    pub fn start(span_type: SpanType) -> Self {
        Self {
            span_type,
            start: std::time::Instant::now(),
            metadata: None,
        }
    }

    pub fn with_metadata(mut self, meta: impl Into<String>) -> Self {
        self.metadata = Some(meta.into());
        self
    }

    pub fn finish(self) -> LatencySpan {
        LatencySpan {
            span_type: self.span_type,
            start_ns: 0,
            duration_us: self.start.elapsed().as_micros() as u64,
            metadata: self.metadata,
            timestamp: chrono::Utc::now().timestamp(),
        }
    }

    pub fn finish_and_record(self) -> u64 {
        let span = self.finish();
        let duration = span.duration_us;
        crate::latency::global_registry().record_span(span);
        duration
    }
}

/// Macro for easy span measurement
#[macro_export]
macro_rules! measure_latency {
    ($span_type:expr, $block:expr) => {{
        let _span = $crate::latency::SpanBuilder::start($span_type);
        let result = $block;
        _span.finish_and_record();
        result
    }};
    ($span_type:expr, $meta:expr, $block:expr) => {{
        let _span = $crate::latency::SpanBuilder::start($span_type).with_metadata($meta);
        let result = $block;
        _span.finish_and_record();
        result
    }};
}

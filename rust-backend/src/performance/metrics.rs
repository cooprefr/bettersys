//! Unified Metrics Collection
//!
//! Aggregates all performance metrics into a single interface
//! for the data ingestion pipeline and trading engine.

use serde::Serialize;
use std::time::Instant;

use super::{
    cpu::CpuSnapshot, io::IoSnapshot, memory::MemorySnapshot, throughput::ThroughputSnapshot,
    PipelineSnapshot,
};

/// Complete performance metrics for the trading engine
#[derive(Debug, Clone, Serialize)]
pub struct PerformanceMetrics {
    pub timestamp: i64,
    pub uptime_secs: f64,

    // Core metrics
    pub memory: MemorySnapshot,
    pub cpu: CpuSnapshot,
    pub io: IoSnapshot,
    pub throughput: ThroughputSnapshot,

    // Pipeline-specific
    pub pipeline: PipelineSnapshot,

    // Summary scores
    pub health: HealthScore,
}

/// Health score for quick assessment
#[derive(Debug, Clone, Serialize)]
pub struct HealthScore {
    /// Overall health 0-100
    pub overall: u8,
    /// Memory pressure (0 = good, 100 = critical)
    pub memory_pressure: u8,
    /// CPU pressure
    pub cpu_pressure: u8,
    /// Latency score (0 = fast, 100 = slow)
    pub latency_score: u8,
    /// Error rate score (0 = no errors, 100 = high errors)
    pub error_rate: u8,
    /// Throughput score (100 = meeting targets, 0 = far below)
    pub throughput_score: u8,
    /// Issues detected
    pub issues: Vec<String>,
}

impl HealthScore {
    pub fn compute(
        memory: &MemorySnapshot,
        cpu: &CpuSnapshot,
        io: &IoSnapshot,
        throughput: &ThroughputSnapshot,
        pipeline: &PipelineSnapshot,
    ) -> Self {
        let mut issues = Vec::new();

        // Memory pressure (based on allocation rate and peak)
        let memory_pressure = if memory.peak_heap_bytes > 1024 * 1024 * 1024 {
            issues.push("High memory usage (>1GB)".to_string());
            80
        } else if memory.peak_heap_bytes > 512 * 1024 * 1024 {
            50
        } else {
            20
        };

        // CPU pressure
        let cpu_pressure = if cpu.cpu_utilization_pct > 80.0 {
            issues.push(format!(
                "High CPU utilization: {:.1}%",
                cpu.cpu_utilization_pct
            ));
            80
        } else if cpu.cpu_utilization_pct > 50.0 {
            50
        } else {
            20
        };

        // Latency score (based on SQLite and network latency)
        let max_sqlite_latency = io.sqlite.max_latency_us;
        let latency_score = if max_sqlite_latency > 100_000 {
            issues.push(format!(
                "High SQLite latency: {}ms",
                max_sqlite_latency / 1000
            ));
            80
        } else if max_sqlite_latency > 10_000 {
            50
        } else {
            20
        };

        // Error rate
        let total_errors =
            pipeline.dome_ws.errors + pipeline.dome_rest.errors + pipeline.signal_detection.errors;
        let total_events = pipeline.dome_ws.events_processed
            + pipeline.dome_rest.events_processed
            + pipeline.signal_detection.events_processed;

        let error_rate = if total_events > 0 {
            let rate = (total_errors as f64 / total_events as f64) * 100.0;
            if rate > 5.0 {
                issues.push(format!("High error rate: {:.1}%", rate));
                80
            } else if rate > 1.0 {
                50
            } else {
                20
            }
        } else {
            20
        };

        // Throughput score (are we processing at expected rates?)
        let throughput_score = if throughput.recent_rates.binance_per_sec < 0.5 {
            issues.push("Low Binance update rate".to_string());
            40
        } else {
            80
        };

        // Overall health (weighted average)
        let overall = 100
            - ((memory_pressure as u16 * 2
                + cpu_pressure as u16 * 2
                + latency_score as u16 * 3
                + error_rate as u16 * 3
                + (100 - throughput_score) as u16 * 2)
                / 12) as u8;

        Self {
            overall,
            memory_pressure,
            cpu_pressure,
            latency_score,
            error_rate,
            throughput_score,
            issues,
        }
    }
}

/// Data ingestion pipeline latency breakdown
#[derive(Debug, Clone, Serialize)]
pub struct PipelineLatencyBreakdown {
    /// Total tick-to-trade latency (p50, p99, p999)
    pub total_t2t_p50_us: u64,
    pub total_t2t_p99_us: u64,
    pub total_t2t_p999_us: u64,

    /// Component breakdown
    pub binance_receive_us: u64,
    pub price_processing_us: u64,
    pub gamma_lookup_us: u64,
    pub orderbook_fetch_us: u64,
    pub kelly_calculation_us: u64,
    pub order_submission_us: u64,
    pub ledger_update_us: u64,

    /// Percentage of time in each component
    pub pct_binance: f64,
    pub pct_gamma: f64,
    pub pct_orderbook: f64,
    pub pct_kelly: f64,
    pub pct_submission: f64,
    pub pct_other: f64,
}

impl PipelineLatencyBreakdown {
    pub fn from_pipeline(pipeline: &PipelineSnapshot) -> Self {
        let binance = pipeline.binance_feed.p50_us();
        let gamma = pipeline.fast15m_engine.p50_us(); // Simplified
        let total = binance + gamma;

        let pct = |v: u64| -> f64 {
            if total > 0 {
                (v as f64 / total as f64) * 100.0
            } else {
                0.0
            }
        };

        Self {
            total_t2t_p50_us: pipeline.fast15m_engine.p50_us(),
            total_t2t_p99_us: pipeline.fast15m_engine.p99_us(),
            total_t2t_p999_us: pipeline.fast15m_engine.p999_us(),
            binance_receive_us: binance,
            price_processing_us: 0,
            gamma_lookup_us: gamma,
            orderbook_fetch_us: 0,
            kelly_calculation_us: 0,
            order_submission_us: 0,
            ledger_update_us: 0,
            pct_binance: pct(binance),
            pct_gamma: pct(gamma),
            pct_orderbook: 0.0,
            pct_kelly: 0.0,
            pct_submission: 0.0,
            pct_other: 0.0,
        }
    }
}

/// Trading engine performance summary
#[derive(Debug, Clone, Serialize)]
pub struct TradingEngineSummary {
    pub fast15m: EngineMetrics,
    pub long: EngineMetrics,
}

#[derive(Debug, Clone, Serialize)]
pub struct EngineMetrics {
    pub enabled: bool,
    pub evaluations: u64,
    pub trades: u64,
    pub success_rate: f64,
    pub avg_latency_us: f64,
    pub p99_latency_us: u64,
    pub cache_hit_rate: f64,
}

impl TradingEngineSummary {
    pub fn from_pipeline(pipeline: &PipelineSnapshot) -> Self {
        Self {
            fast15m: EngineMetrics {
                enabled: true,
                evaluations: pipeline.fast15m_engine.events_processed,
                trades: 0, // Would need separate counter
                success_rate: if pipeline.fast15m_engine.events_processed > 0 {
                    1.0 - (pipeline.fast15m_engine.errors as f64
                        / pipeline.fast15m_engine.events_processed as f64)
                } else {
                    1.0
                },
                avg_latency_us: pipeline.fast15m_engine.mean_latency_us(),
                p99_latency_us: pipeline.fast15m_engine.p99_us(),
                cache_hit_rate: 0.0, // Would need cache tracking
            },
            long: EngineMetrics {
                enabled: true,
                evaluations: pipeline.long_engine.events_processed,
                trades: 0,
                success_rate: if pipeline.long_engine.events_processed > 0 {
                    1.0 - (pipeline.long_engine.errors as f64
                        / pipeline.long_engine.events_processed as f64)
                } else {
                    1.0
                },
                avg_latency_us: pipeline.long_engine.mean_latency_us(),
                p99_latency_us: pipeline.long_engine.p99_us(),
                cache_hit_rate: 0.0,
            },
        }
    }
}

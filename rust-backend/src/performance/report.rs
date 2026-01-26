//! Performance Report Generation
//!
//! Generates comprehensive performance reports for analysis.

use serde::Serialize;

use super::{
    cpu::CpuSnapshot,
    io::IoSnapshot,
    memory::MemorySnapshot,
    metrics::{HealthScore, PipelineLatencyBreakdown, TradingEngineSummary},
    throughput::ThroughputSnapshot,
    PipelineSnapshot,
};

/// Full performance report
#[derive(Debug, Clone, Serialize)]
pub struct PerformanceReport {
    pub timestamp: i64,
    pub uptime_secs: f64,
    pub memory: MemorySnapshot,
    pub cpu: CpuSnapshot,
    pub io: IoSnapshot,
    pub throughput: ThroughputSnapshot,
    pub pipeline: PipelineSnapshot,
}

impl PerformanceReport {
    /// Generate health score from this report
    pub fn health_score(&self) -> HealthScore {
        HealthScore::compute(
            &self.memory,
            &self.cpu,
            &self.io,
            &self.throughput,
            &self.pipeline,
        )
    }

    /// Generate latency breakdown
    pub fn latency_breakdown(&self) -> PipelineLatencyBreakdown {
        PipelineLatencyBreakdown::from_pipeline(&self.pipeline)
    }

    /// Generate trading engine summary
    pub fn trading_summary(&self) -> TradingEngineSummary {
        TradingEngineSummary::from_pipeline(&self.pipeline)
    }

    /// Generate executive summary (text)
    pub fn executive_summary(&self) -> String {
        let health = self.health_score();
        let mut summary = String::new();

        summary.push_str(&format!(
            "=== BetterSys Performance Report ===\n\
             Timestamp: {}\n\
             Uptime: {:.1}s\n\n",
            chrono::DateTime::from_timestamp(self.timestamp, 0)
                .map(|dt| dt.to_rfc3339())
                .unwrap_or_else(|| "unknown".to_string()),
            self.uptime_secs
        ));

        summary.push_str(&format!("HEALTH SCORE: {}/100\n", health.overall));

        if !health.issues.is_empty() {
            summary.push_str("\nIssues Detected:\n");
            for issue in &health.issues {
                summary.push_str(&format!("  - {}\n", issue));
            }
        }

        summary.push_str(&format!(
            "\nMEMORY:\n\
             - Current heap: {} bytes\n\
             - Peak heap: {} bytes\n\
             - Allocations: {}\n",
            self.memory.heap_bytes, self.memory.peak_heap_bytes, self.memory.total_allocations,
        ));

        summary.push_str(&format!(
            "\nCPU:\n\
             - Utilization: {:.1}%\n\
             - Total CPU time: {}μs\n\
             - Active spans: {}\n",
            self.cpu.cpu_utilization_pct, self.cpu.total_cpu_us, self.cpu.span_count,
        ));

        summary.push_str(&format!(
            "\nTHROUGHPUT (lifetime):\n\
             - Binance updates/s: {:.2}\n\
             - Dome WS events/s: {:.2}\n\
             - Signals/s: {:.4}\n\
             - API requests/s: {:.2}\n\
             - Trades/s: {:.4}\n",
            self.throughput.lifetime_rates.binance_per_sec,
            self.throughput.lifetime_rates.dome_ws_per_sec,
            self.throughput.lifetime_rates.signals_per_sec,
            self.throughput.lifetime_rates.api_per_sec,
            self.throughput.lifetime_rates.trades_per_sec,
        ));

        summary.push_str(&format!(
            "\nPIPELINE LATENCY:\n\
             - Binance feed p50: {}μs, p99: {}μs\n\
             - Dome WS p50: {}μs, p99: {}μs\n\
             - Signal detection p50: {}μs, p99: {}μs\n\
             - FAST15M engine p50: {}μs, p99: {}μs\n",
            self.pipeline.binance_feed.p50_us(),
            self.pipeline.binance_feed.p99_us(),
            self.pipeline.dome_ws.p50_us(),
            self.pipeline.dome_ws.p99_us(),
            self.pipeline.signal_detection.p50_us(),
            self.pipeline.signal_detection.p99_us(),
            self.pipeline.fast15m_engine.p50_us(),
            self.pipeline.fast15m_engine.p99_us(),
        ));

        summary.push_str(&format!(
            "\nSQLITE:\n\
             - Selects: {} (avg {}μs)\n\
             - Inserts: {} (avg {}μs)\n\
             - Max latency: {}μs\n",
            self.io.sqlite.selects,
            self.io.sqlite.select_avg_latency_us,
            self.io.sqlite.inserts,
            self.io.sqlite.insert_avg_latency_us,
            self.io.sqlite.max_latency_us,
        ));

        summary
    }

    /// Export as JSON
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }

    /// Export as compact JSON
    pub fn to_json_compact(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }
}

/// Summary report for quick checks
#[derive(Debug, Clone, Serialize)]
pub struct QuickReport {
    pub timestamp: i64,
    pub health_score: u8,
    pub issues_count: usize,
    pub memory_mb: f64,
    pub cpu_pct: f64,
    pub binance_rate: f64,
    pub signal_rate: f64,
    pub api_rate: f64,
    pub fast15m_p99_us: u64,
    pub sqlite_max_us: u64,
}

impl QuickReport {
    pub fn from_full(report: &PerformanceReport) -> Self {
        let health = report.health_score();
        Self {
            timestamp: report.timestamp,
            health_score: health.overall,
            issues_count: health.issues.len(),
            memory_mb: report.memory.heap_bytes as f64 / (1024.0 * 1024.0),
            cpu_pct: report.cpu.cpu_utilization_pct,
            binance_rate: report.throughput.lifetime_rates.binance_per_sec,
            signal_rate: report.throughput.lifetime_rates.signals_per_sec,
            api_rate: report.throughput.lifetime_rates.api_per_sec,
            fast15m_p99_us: report.pipeline.fast15m_engine.p99_us(),
            sqlite_max_us: report.io.sqlite.max_latency_us,
        }
    }
}

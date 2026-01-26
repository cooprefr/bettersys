//! Performance + Determinism Benchmark Suite for backtest_v2
//!
//! Measures:
//! - Hot-path performance (throughput, latency per event, replay speed)
//! - Memory growth and buffer sizes
//! - Determinism under load (identical inputs => identical RunFingerprint)
//!
//! Does NOT modify strategy logic. Benchmarks run in release mode.

use crate::backtest_v2::clock::Nanos;
use crate::backtest_v2::events::{Event, Level, Side, TimestampedEvent};
use crate::backtest_v2::integrity::PathologyPolicy;
use crate::backtest_v2::invariants::InvariantMode;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Instant;

// =============================================================================
// BENCHMARK CONFIGURATION
// =============================================================================

/// Benchmark scenario size preset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScenarioSize {
    /// 1 market, 1 hour data - sanity check
    Small,
    /// 10 markets, 24 hours data - typical research
    Medium,
    /// 50 markets, 7 days data - stress test
    Large,
    /// Maximum available markets for a day with all event types
    Stress,
}

impl ScenarioSize {
    pub fn description(&self) -> &'static str {
        match self {
            Self::Small => "Small (1 market, 1 hour): sanity check",
            Self::Medium => "Medium (10 markets, 24 hours): typical research",
            Self::Large => "Large (50 markets, 7 days): stress test",
            Self::Stress => "Stress (max markets, full day): capacity test",
        }
    }

    /// Default number of markets for this scenario size.
    pub fn default_markets(&self) -> usize {
        match self {
            Self::Small => 1,
            Self::Medium => 10,
            Self::Large => 50,
            Self::Stress => 200, // Approximate max available
        }
    }

    /// Default time range duration in nanoseconds.
    /// Note: For benchmarking practicality, durations are kept reasonable
    /// while still exercising the system at scale.
    pub fn default_duration_ns(&self) -> Nanos {
        use crate::backtest_v2::clock::NANOS_PER_SEC;
        match self {
            Self::Small => 60 * 60 * NANOS_PER_SEC,      // 1 hour
            Self::Medium => 24 * 60 * 60 * NANOS_PER_SEC, // 24 hours
            Self::Large => 24 * 60 * 60 * NANOS_PER_SEC,  // 24 hours (with 50 markets)
            Self::Stress => 4 * 60 * 60 * NANOS_PER_SEC,  // 4 hours (with 200 markets)
        }
    }
}

/// Benchmark scenario configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchmarkScenario {
    /// Scenario name.
    pub name: String,
    /// Scenario size preset.
    pub size: ScenarioSize,
    /// Number of markets to simulate.
    pub num_markets: usize,
    /// Simulated time range start (ns).
    pub start_time_ns: Nanos,
    /// Simulated time range end (ns).
    pub end_time_ns: Nanos,
    /// Execution mode: taker-only vs maker-enabled.
    pub maker_enabled: bool,
    /// Production-grade mode.
    pub production_grade: bool,
    /// Strict accounting mode.
    pub strict_accounting: bool,
    /// Invariant mode.
    pub invariant_mode: InvariantMode,
    /// Integrity policy.
    pub integrity_policy: PathologyPolicy,
    /// Random seed for deterministic data generation.
    pub seed: u64,
    /// Events per second to generate (for synthetic data).
    pub events_per_second: u64,
    /// Enable delta events (L2BookDelta) in addition to snapshots.
    pub include_deltas: bool,
    /// Enable trade prints.
    pub include_trades: bool,
}

impl BenchmarkScenario {
    /// Create a scenario from a size preset.
    pub fn from_size(size: ScenarioSize, seed: u64) -> Self {
        let (production_grade, maker_enabled, events_per_second) = match size {
            ScenarioSize::Small => (false, false, 100),   // 100 events/sec
            ScenarioSize::Medium => (false, false, 10),   // 10 events/sec (more realistic)
            ScenarioSize::Large => (true, true, 5),       // 5 events/sec
            ScenarioSize::Stress => (true, true, 2),      // 2 events/sec
        };

        Self {
            name: format!("{:?}", size),
            size,
            num_markets: size.default_markets(),
            start_time_ns: 0,
            end_time_ns: size.default_duration_ns(),
            maker_enabled,
            production_grade,
            strict_accounting: production_grade,
            invariant_mode: if production_grade {
                InvariantMode::Hard
            } else {
                InvariantMode::Soft
            },
            integrity_policy: if production_grade {
                PathologyPolicy::strict()
            } else {
                PathologyPolicy::default()
            },
            seed,
            events_per_second,
            include_deltas: production_grade,
            include_trades: true,
        }
    }

    /// Total simulated duration in nanoseconds.
    pub fn duration_ns(&self) -> Nanos {
        self.end_time_ns - self.start_time_ns
    }

    /// Estimated total events (rough estimate).
    pub fn estimated_events(&self) -> u64 {
        let duration_secs = self.duration_ns() as f64 / 1_000_000_000.0;
        let events_per_market = (duration_secs * self.events_per_second as f64) as u64;
        events_per_market * self.num_markets as u64
    }
}

// =============================================================================
// STAGE TIMING (Internal Instrumentation)
// =============================================================================

/// Profiling stage for hot-path timing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BenchmarkStage {
    /// Event decode/parse (if applicable).
    EventDecode,
    /// Event queue insertion.
    QueueInsert,
    /// Orchestrator dispatch.
    Dispatch,
    /// Book update apply (snapshot + delta).
    BookUpdate,
    /// Strategy callback invocation.
    StrategyCallback,
    /// OMS state transitions.
    OmsTransition,
    /// Fill generation + MakerFillGate checks.
    FillGeneration,
    /// Ledger posting.
    LedgerPosting,
    /// Invariant checks.
    InvariantCheck,
    /// Fingerprint recording.
    FingerprintRecord,
    /// Total (full event processing).
    Total,
}

impl BenchmarkStage {
    pub fn all() -> &'static [BenchmarkStage] {
        &[
            Self::EventDecode,
            Self::QueueInsert,
            Self::Dispatch,
            Self::BookUpdate,
            Self::StrategyCallback,
            Self::OmsTransition,
            Self::FillGeneration,
            Self::LedgerPosting,
            Self::InvariantCheck,
            Self::FingerprintRecord,
            Self::Total,
        ]
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::EventDecode => "event_decode",
            Self::QueueInsert => "queue_insert",
            Self::Dispatch => "dispatch",
            Self::BookUpdate => "book_update",
            Self::StrategyCallback => "strategy_callback",
            Self::OmsTransition => "oms_transition",
            Self::FillGeneration => "fill_generation",
            Self::LedgerPosting => "ledger_posting",
            Self::InvariantCheck => "invariant_check",
            Self::FingerprintRecord => "fingerprint_record",
            Self::Total => "total",
        }
    }
}

/// Accumulated timing statistics for a stage.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StageTiming {
    pub count: u64,
    pub total_ns: u64,
    pub min_ns: u64,
    pub max_ns: u64,
}

impl StageTiming {
    pub fn record(&mut self, duration_ns: u64) {
        self.count += 1;
        self.total_ns += duration_ns;
        if self.count == 1 {
            self.min_ns = duration_ns;
            self.max_ns = duration_ns;
        } else {
            self.min_ns = self.min_ns.min(duration_ns);
            self.max_ns = self.max_ns.max(duration_ns);
        }
    }

    pub fn mean_ns(&self) -> f64 {
        if self.count > 0 {
            self.total_ns as f64 / self.count as f64
        } else {
            0.0
        }
    }

    pub fn percentage_of(&self, total_ns: u64) -> f64 {
        if total_ns > 0 {
            (self.total_ns as f64 / total_ns as f64) * 100.0
        } else {
            0.0
        }
    }
}

/// Stage-level profiler for benchmark instrumentation.
#[derive(Debug, Clone, Default)]
pub struct StageProfiler {
    pub enabled: bool,
    pub stages: HashMap<BenchmarkStage, StageTiming>,
}

impl StageProfiler {
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            stages: HashMap::new(),
        }
    }

    /// Record a stage duration.
    #[inline]
    pub fn record(&mut self, stage: BenchmarkStage, duration_ns: u64) {
        if !self.enabled {
            return;
        }
        self.stages.entry(stage).or_default().record(duration_ns);
    }

    /// Start a scoped timer that records on drop.
    #[inline]
    pub fn time(&mut self, stage: BenchmarkStage) -> StageTimer<'_> {
        StageTimer {
            profiler: self,
            stage,
            start: Instant::now(),
        }
    }

    /// Get total time across all stages.
    pub fn total_ns(&self) -> u64 {
        self.stages
            .get(&BenchmarkStage::Total)
            .map(|s| s.total_ns)
            .unwrap_or(0)
    }

    /// Generate stage breakdown report.
    pub fn breakdown(&self) -> Vec<StageBreakdown> {
        let total = self.total_ns();
        let mut breakdown: Vec<_> = self
            .stages
            .iter()
            .map(|(stage, timing)| StageBreakdown {
                stage: *stage,
                name: stage.name().to_string(),
                count: timing.count,
                total_ns: timing.total_ns,
                mean_ns: timing.mean_ns(),
                min_ns: timing.min_ns,
                max_ns: timing.max_ns,
                percentage: timing.percentage_of(total),
            })
            .collect();
        breakdown.sort_by(|a, b| b.total_ns.cmp(&a.total_ns));
        breakdown
    }
}

pub struct StageTimer<'a> {
    profiler: &'a mut StageProfiler,
    stage: BenchmarkStage,
    start: Instant,
}

impl<'a> Drop for StageTimer<'a> {
    fn drop(&mut self) {
        let elapsed = self.start.elapsed().as_nanos() as u64;
        self.profiler.record(self.stage, elapsed);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StageBreakdown {
    pub stage: BenchmarkStage,
    pub name: String,
    pub count: u64,
    pub total_ns: u64,
    pub mean_ns: f64,
    pub min_ns: u64,
    pub max_ns: u64,
    pub percentage: f64,
}

// =============================================================================
// MEMORY PROFILING
// =============================================================================

/// Memory usage sample.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MemorySample {
    /// Sample timestamp (wall clock, relative to start).
    pub elapsed_ms: u64,
    /// Simulated time at sample.
    pub sim_time_ns: Nanos,
    /// Events processed so far.
    pub events_processed: u64,
    /// Approximate RSS (if available).
    pub rss_bytes: Option<u64>,
    /// EventQueue length.
    pub event_queue_len: usize,
    /// DecisionProofBuffer length.
    pub decision_proof_buffer_len: usize,
    /// OMS order map size.
    pub oms_order_count: usize,
    /// Book state total levels (sum across all tokens).
    pub book_total_levels: usize,
    /// Ledger entries count.
    pub ledger_entries_count: u64,
    /// Fingerprint behavior event count.
    pub fingerprint_event_count: u64,
}

/// Memory profiler that samples at intervals.
#[derive(Debug, Clone, Default)]
pub struct MemoryProfiler {
    pub samples: Vec<MemorySample>,
    pub sample_interval_ms: u64,
    last_sample_time: Option<Instant>,
    start_time: Option<Instant>,
}

impl MemoryProfiler {
    pub fn new(sample_interval_ms: u64) -> Self {
        Self {
            samples: Vec::new(),
            sample_interval_ms,
            last_sample_time: None,
            start_time: None,
        }
    }

    pub fn start(&mut self) {
        self.start_time = Some(Instant::now());
        self.last_sample_time = Some(Instant::now());
    }

    /// Check if a sample should be taken.
    pub fn should_sample(&self) -> bool {
        if let Some(last) = self.last_sample_time {
            last.elapsed().as_millis() >= self.sample_interval_ms as u128
        } else {
            true
        }
    }

    /// Record a sample.
    pub fn record(&mut self, sample: MemorySample) {
        self.samples.push(sample);
        self.last_sample_time = Some(Instant::now());
    }

    /// Get elapsed ms since start.
    pub fn elapsed_ms(&self) -> u64 {
        self.start_time
            .map(|s| s.elapsed().as_millis() as u64)
            .unwrap_or(0)
    }

    /// Peak memory sample.
    pub fn peak(&self) -> Option<&MemorySample> {
        self.samples.iter().max_by_key(|s| {
            s.rss_bytes.unwrap_or(0)
                + s.event_queue_len as u64
                + s.oms_order_count as u64
                + s.book_total_levels as u64
        })
    }

    /// Detect monotonic growth (potential leak).
    pub fn detect_growth(&self) -> Option<GrowthReport> {
        if self.samples.len() < 3 {
            return None;
        }

        // Split into thirds and compare
        let n = self.samples.len();
        let first_third_avg = self.samples[..n / 3]
            .iter()
            .map(|s| s.event_queue_len + s.oms_order_count + s.book_total_levels)
            .sum::<usize>() as f64
            / (n / 3) as f64;

        let last_third_avg = self.samples[2 * n / 3..]
            .iter()
            .map(|s| s.event_queue_len + s.oms_order_count + s.book_total_levels)
            .sum::<usize>() as f64
            / (n - 2 * n / 3) as f64;

        let growth_ratio = if first_third_avg > 0.0 {
            last_third_avg / first_third_avg
        } else {
            1.0
        };

        Some(GrowthReport {
            first_third_avg,
            last_third_avg,
            growth_ratio,
            is_monotonic_growth: growth_ratio > 1.5, // 50% growth is suspicious
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GrowthReport {
    pub first_third_avg: f64,
    pub last_third_avg: f64,
    pub growth_ratio: f64,
    pub is_monotonic_growth: bool,
}

// =============================================================================
// DETERMINISM VERIFICATION
// =============================================================================

/// Determinism test result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeterminismResult {
    /// Scenario name.
    pub scenario: String,
    /// Number of runs compared.
    pub num_runs: usize,
    /// Whether all runs produced identical fingerprints.
    pub deterministic: bool,
    /// Run fingerprints (should all be identical if deterministic).
    pub fingerprints: Vec<String>,
    /// Final PnL from each run.
    pub final_pnls: Vec<f64>,
    /// Order counts from each run.
    pub order_counts: Vec<u64>,
    /// Fill counts from each run.
    pub fill_counts: Vec<u64>,
    /// First differing behavior event index (if not deterministic).
    pub first_diff_index: Option<u64>,
    /// First differing behavior event hashes (if not deterministic).
    pub first_diff_hashes: Option<(u64, u64)>,
    /// Error message if determinism failed.
    pub error: Option<String>,
}

impl DeterminismResult {
    pub fn success(scenario: String, fingerprints: Vec<String>, pnls: Vec<f64>, orders: Vec<u64>, fills: Vec<u64>) -> Self {
        Self {
            scenario,
            num_runs: fingerprints.len(),
            deterministic: fingerprints.windows(2).all(|w| w[0] == w[1]),
            fingerprints,
            final_pnls: pnls,
            order_counts: orders,
            fill_counts: fills,
            first_diff_index: None,
            first_diff_hashes: None,
            error: None,
        }
    }

    pub fn failure(scenario: String, error: String) -> Self {
        Self {
            scenario,
            num_runs: 0,
            deterministic: false,
            fingerprints: Vec::new(),
            final_pnls: Vec::new(),
            order_counts: Vec::new(),
            fill_counts: Vec::new(),
            first_diff_index: None,
            first_diff_hashes: None,
            error: Some(error),
        }
    }
}

// =============================================================================
// BENCHMARK RESULTS
// =============================================================================

/// Complete benchmark results for a scenario.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchmarkResults {
    /// Scenario configuration.
    pub scenario: BenchmarkScenario,
    /// Total events processed.
    pub events_processed: u64,
    /// Total simulated time covered (ns).
    pub sim_duration_ns: Nanos,
    /// Wall clock time (ms).
    pub wall_time_ms: u64,
    /// Events per second.
    pub events_per_sec: f64,
    /// Nanoseconds per event (overall).
    pub ns_per_event: f64,
    /// Replay speed ratio: (simulated duration) / (wall duration).
    pub replay_speed_ratio: f64,
    /// Stage timing breakdown.
    pub stage_breakdown: Vec<StageBreakdown>,
    /// Memory statistics.
    pub memory_stats: MemoryStats,
    /// Determinism result (if verified).
    pub determinism: Option<DeterminismResult>,
    /// Run fingerprint.
    pub fingerprint: Option<String>,
    /// Performance targets comparison.
    pub targets: TargetComparison,
    /// Timestamp when benchmark was run.
    pub timestamp: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MemoryStats {
    /// Peak memory sample.
    pub peak_event_queue_len: usize,
    pub peak_oms_orders: usize,
    pub peak_book_levels: usize,
    pub peak_ledger_entries: u64,
    pub peak_rss_mb: Option<f64>,
    /// Start/mid/end samples.
    pub start_event_queue_len: usize,
    pub mid_event_queue_len: usize,
    pub end_event_queue_len: usize,
    /// Growth detection.
    pub growth_report: Option<GrowthReport>,
}

/// Performance target comparison.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TargetComparison {
    /// Minimum replay speed ratio target.
    pub min_replay_speed: f64,
    /// Actual replay speed.
    pub actual_replay_speed: f64,
    /// Replay speed target met.
    pub replay_speed_met: bool,
    /// Minimum events per second target.
    pub min_events_per_sec: f64,
    /// Actual events per second.
    pub actual_events_per_sec: f64,
    /// Throughput target met.
    pub throughput_met: bool,
    /// Memory growth must be bounded.
    pub memory_bounded: bool,
    /// Determinism must be exact.
    pub determinism_exact: bool,
    /// All targets met.
    pub all_targets_met: bool,
}

impl TargetComparison {
    pub fn evaluate(
        replay_speed: f64,
        events_per_sec: f64,
        memory_growth: Option<&GrowthReport>,
        determinism: Option<&DeterminismResult>,
        targets: &PerformanceTargets,
    ) -> Self {
        let replay_speed_met = replay_speed >= targets.min_replay_speed;
        let throughput_met = events_per_sec >= targets.min_events_per_sec;
        let memory_bounded = memory_growth
            .map(|g| !g.is_monotonic_growth)
            .unwrap_or(true);
        let determinism_exact = determinism
            .map(|d| d.deterministic)
            .unwrap_or(true);

        Self {
            min_replay_speed: targets.min_replay_speed,
            actual_replay_speed: replay_speed,
            replay_speed_met,
            min_events_per_sec: targets.min_events_per_sec,
            actual_events_per_sec: events_per_sec,
            throughput_met,
            memory_bounded,
            determinism_exact,
            all_targets_met: replay_speed_met && throughput_met && memory_bounded && determinism_exact,
        }
    }
}

/// Configurable performance targets.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PerformanceTargets {
    /// Minimum replay speed ratio (simulated time / wall time).
    pub min_replay_speed: f64,
    /// Minimum events processed per second.
    pub min_events_per_sec: f64,
}

impl Default for PerformanceTargets {
    fn default() -> Self {
        Self {
            min_replay_speed: 10.0,      // At least 10x faster than real-time
            min_events_per_sec: 10_000.0, // At least 10k events/sec
        }
    }
}

impl PerformanceTargets {
    pub fn for_scenario(size: ScenarioSize) -> Self {
        match size {
            ScenarioSize::Small => Self {
                min_replay_speed: 100.0,
                min_events_per_sec: 50_000.0,
            },
            ScenarioSize::Medium => Self {
                min_replay_speed: 50.0,
                min_events_per_sec: 30_000.0,
            },
            ScenarioSize::Large => Self {
                min_replay_speed: 10.0,
                min_events_per_sec: 10_000.0,
            },
            ScenarioSize::Stress => Self {
                min_replay_speed: 5.0,
                min_events_per_sec: 5_000.0,
            },
        }
    }
}

// =============================================================================
// SYNTHETIC DATA GENERATION
// =============================================================================

/// Generate synthetic events for benchmarking.
pub struct SyntheticDataGenerator {
    seed: u64,
    rng: rand_chacha::ChaCha8Rng,
}

impl SyntheticDataGenerator {
    pub fn new(seed: u64) -> Self {
        use rand::SeedableRng;
        Self {
            seed,
            rng: rand_chacha::ChaCha8Rng::seed_from_u64(seed),
        }
    }

    /// Generate events for a scenario.
    pub fn generate(&mut self, scenario: &BenchmarkScenario) -> Vec<TimestampedEvent> {
        use crate::backtest_v2::queue::StreamSource;
        use rand::Rng;

        let mut events = Vec::new();
        let duration_ns = scenario.duration_ns();
        let events_per_market = scenario.estimated_events() / scenario.num_markets.max(1) as u64;
        let interval_ns = if events_per_market > 0 {
            duration_ns / events_per_market as i64
        } else {
            1_000_000_000 // 1 second default
        };

        let mut seq = 0u64;

        for market_idx in 0..scenario.num_markets {
            let token_id = format!("token_{}", market_idx);
            let mut current_time = scenario.start_time_ns;
            let mut exchange_seq = 0u64;

            // Initial snapshot
            events.push(TimestampedEvent {
                time: current_time,
                source_time: current_time,
                seq: {
                    seq += 1;
                    seq
                },
                source: StreamSource::MarketData as u8,
                event: Event::L2BookSnapshot {
                    token_id: token_id.clone(),
                    bids: self.generate_levels(5, 0.45, 0.50, true),
                    asks: self.generate_levels(5, 0.50, 0.55, false),
                    exchange_seq: {
                        exchange_seq += 1;
                        exchange_seq
                    },
                },
            });

            while current_time < scenario.end_time_ns {
                current_time += interval_ns;
                if current_time >= scenario.end_time_ns {
                    break;
                }

                // Generate event type based on scenario config
                let event_type: u8 = self.rng.gen_range(0..100);

                let event = if scenario.include_deltas && event_type < 60 {
                    // 60% L2BookDelta
                    Event::L2BookDelta {
                        token_id: token_id.clone(),
                        side: if self.rng.gen_bool(0.5) {
                            Side::Buy
                        } else {
                            Side::Sell
                        },
                        price: 0.45 + self.rng.gen::<f64>() * 0.10,
                        new_size: self.rng.gen::<f64>() * 1000.0,
                        seq_hash: Some(format!("hash_{}", exchange_seq)),
                    }
                } else if scenario.include_trades && event_type < 90 {
                    // 30% TradePrint
                    Event::TradePrint {
                        token_id: token_id.clone(),
                        price: 0.48 + self.rng.gen::<f64>() * 0.04,
                        size: 10.0 + self.rng.gen::<f64>() * 100.0,
                        aggressor_side: if self.rng.gen_bool(0.5) {
                            Side::Buy
                        } else {
                            Side::Sell
                        },
                        trade_id: Some(format!("trade_{}", seq)),
                    }
                } else {
                    // 10% L2BookSnapshot
                    exchange_seq += 1;
                    Event::L2BookSnapshot {
                        token_id: token_id.clone(),
                        bids: self.generate_levels(5, 0.45, 0.50, true),
                        asks: self.generate_levels(5, 0.50, 0.55, false),
                        exchange_seq,
                    }
                };

                events.push(TimestampedEvent {
                    time: current_time,
                    source_time: current_time,
                    seq: {
                        seq += 1;
                        seq
                    },
                    source: StreamSource::MarketData as u8,
                    event,
                });
            }
        }

        // Sort by time, then by seq for determinism
        events.sort_by(|a, b| a.time.cmp(&b.time).then_with(|| a.seq.cmp(&b.seq)));
        events
    }

    fn generate_levels(&mut self, count: usize, min_price: f64, max_price: f64, is_bid: bool) -> Vec<Level> {
        use rand::Rng;
        
        let mut levels = Vec::with_capacity(count);
        let price_step = (max_price - min_price) / count as f64;

        for i in 0..count {
            let price = if is_bid {
                max_price - i as f64 * price_step
            } else {
                min_price + i as f64 * price_step
            };
            let size = 100.0 + self.rng.gen::<f64>() * 900.0;
            levels.push(Level {
                price,
                size,
                order_count: Some(self.rng.gen_range(1..10)),
            });
        }
        levels
    }
}

// =============================================================================
// BENCHMARK RUNNER
// =============================================================================

/// Configuration for the benchmark suite.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchmarkSuiteConfig {
    /// Scenarios to run.
    pub scenarios: Vec<BenchmarkScenario>,
    /// Whether to verify determinism (runs each scenario twice).
    pub verify_determinism: bool,
    /// Number of runs for determinism verification.
    pub determinism_runs: usize,
    /// Whether to profile stages.
    pub profile_stages: bool,
    /// Memory sampling interval (ms).
    pub memory_sample_interval_ms: u64,
    /// Performance targets.
    pub targets: PerformanceTargets,
    /// Output directory for results.
    pub output_dir: String,
}

impl Default for BenchmarkSuiteConfig {
    fn default() -> Self {
        Self {
            scenarios: vec![
                BenchmarkScenario::from_size(ScenarioSize::Small, 42),
                BenchmarkScenario::from_size(ScenarioSize::Medium, 42),
            ],
            verify_determinism: true,
            determinism_runs: 2,
            profile_stages: true,
            memory_sample_interval_ms: 100,
            targets: PerformanceTargets::default(),
            output_dir: ".".to_string(),
        }
    }
}

impl BenchmarkSuiteConfig {
    /// Create config with all scenario sizes.
    pub fn full_suite(seed: u64) -> Self {
        Self {
            scenarios: vec![
                BenchmarkScenario::from_size(ScenarioSize::Small, seed),
                BenchmarkScenario::from_size(ScenarioSize::Medium, seed),
                BenchmarkScenario::from_size(ScenarioSize::Large, seed),
                BenchmarkScenario::from_size(ScenarioSize::Stress, seed),
            ],
            verify_determinism: true,
            determinism_runs: 2,
            profile_stages: true,
            memory_sample_interval_ms: 100,
            targets: PerformanceTargets::default(),
            output_dir: ".".to_string(),
        }
    }
}

/// Complete benchmark suite report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchmarkSuiteReport {
    /// Individual scenario results.
    pub results: Vec<BenchmarkResults>,
    /// Overall summary.
    pub summary: SuiteSummary,
    /// Timestamp.
    pub timestamp: String,
    /// Configuration used.
    pub config: BenchmarkSuiteConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SuiteSummary {
    /// Total scenarios run.
    pub scenarios_run: usize,
    /// Scenarios that met all targets.
    pub scenarios_passed: usize,
    /// Fastest scenario (events/sec).
    pub fastest_scenario: Option<String>,
    pub fastest_events_per_sec: f64,
    /// Slowest scenario (events/sec).
    pub slowest_scenario: Option<String>,
    pub slowest_events_per_sec: f64,
    /// Bottleneck stage (most time spent).
    pub bottleneck_stage: Option<String>,
    pub bottleneck_percentage: f64,
    /// Memory hotspot (largest buffer).
    pub memory_hotspot: Option<String>,
    /// Determinism status.
    pub all_deterministic: bool,
    /// Overall pass/fail.
    pub overall_pass: bool,
}

impl BenchmarkSuiteReport {
    /// Generate markdown report.
    pub fn to_markdown(&self) -> String {
        let mut md = String::new();

        md.push_str("# Backtest_v2 Performance & Determinism Benchmark Report\n\n");
        md.push_str(&format!("**Generated:** {}\n\n", self.timestamp));

        // Summary
        md.push_str("## Summary\n\n");
        md.push_str(&format!(
            "| Metric | Value |\n|--------|-------|\n"
        ));
        md.push_str(&format!(
            "| Scenarios Run | {} |\n",
            self.summary.scenarios_run
        ));
        md.push_str(&format!(
            "| Scenarios Passed | {} |\n",
            self.summary.scenarios_passed
        ));
        md.push_str(&format!(
            "| Overall Status | {} |\n",
            if self.summary.overall_pass {
                "PASS"
            } else {
                "FAIL"
            }
        ));
        md.push_str(&format!(
            "| Determinism | {} |\n",
            if self.summary.all_deterministic {
                "EXACT"
            } else {
                "FAILED"
            }
        ));

        if let Some(ref stage) = self.summary.bottleneck_stage {
            md.push_str(&format!(
                "| Bottleneck Stage | {} ({:.1}%) |\n",
                stage, self.summary.bottleneck_percentage
            ));
        }

        md.push_str("\n");

        // Per-scenario results
        md.push_str("## Scenario Results\n\n");

        for result in &self.results {
            md.push_str(&format!("### {}\n\n", result.scenario.name));
            md.push_str(&format!(
                "**Size:** {} markets, {:.1} hours\n\n",
                result.scenario.num_markets,
                result.scenario.duration_ns() as f64 / 3_600_000_000_000.0
            ));

            md.push_str("| Metric | Value | Target | Status |\n");
            md.push_str("|--------|-------|--------|--------|\n");
            md.push_str(&format!(
                "| Events Processed | {} | - | - |\n",
                result.events_processed
            ));
            md.push_str(&format!(
                "| Events/sec | {:.0} | {:.0} | {} |\n",
                result.events_per_sec,
                result.targets.min_events_per_sec,
                if result.targets.throughput_met {
                    "PASS"
                } else {
                    "FAIL"
                }
            ));
            md.push_str(&format!(
                "| Replay Speed | {:.1}x | {:.1}x | {} |\n",
                result.targets.actual_replay_speed,
                result.targets.min_replay_speed,
                if result.targets.replay_speed_met {
                    "PASS"
                } else {
                    "FAIL"
                }
            ));
            md.push_str(&format!(
                "| ns/event | {:.0} | - | - |\n",
                result.ns_per_event
            ));
            md.push_str(&format!(
                "| Memory Bounded | {} | Yes | {} |\n",
                if result.targets.memory_bounded {
                    "Yes"
                } else {
                    "No"
                },
                if result.targets.memory_bounded {
                    "PASS"
                } else {
                    "FAIL"
                }
            ));
            md.push_str(&format!(
                "| Determinism | {} | Exact | {} |\n",
                if result.targets.determinism_exact {
                    "Exact"
                } else {
                    "FAILED"
                },
                if result.targets.determinism_exact {
                    "PASS"
                } else {
                    "FAIL"
                }
            ));

            md.push_str("\n");

            // Stage breakdown
            if !result.stage_breakdown.is_empty() {
                md.push_str("**Stage Breakdown:**\n\n");
                md.push_str("| Stage | Count | Total (ms) | Mean (us) | % |\n");
                md.push_str("|-------|-------|-----------|-----------|---|\n");
                for stage in &result.stage_breakdown {
                    md.push_str(&format!(
                        "| {} | {} | {:.1} | {:.2} | {:.1}% |\n",
                        stage.name,
                        stage.count,
                        stage.total_ns as f64 / 1_000_000.0,
                        stage.mean_ns / 1_000.0,
                        stage.percentage
                    ));
                }
                md.push_str("\n");
            }

            // Memory stats
            md.push_str("**Memory:**\n\n");
            md.push_str(&format!(
                "- Peak Event Queue: {} items\n",
                result.memory_stats.peak_event_queue_len
            ));
            md.push_str(&format!(
                "- Peak OMS Orders: {}\n",
                result.memory_stats.peak_oms_orders
            ));
            md.push_str(&format!(
                "- Peak Book Levels: {}\n",
                result.memory_stats.peak_book_levels
            ));
            if let Some(rss) = result.memory_stats.peak_rss_mb {
                md.push_str(&format!("- Peak RSS: {:.1} MB\n", rss));
            }
            md.push_str("\n");

            // Fingerprint
            if let Some(ref fp) = result.fingerprint {
                md.push_str(&format!("**Fingerprint:** `{}`\n\n", fp));
            }

            md.push_str("---\n\n");
        }

        md
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_scenario_sizes() {
        for size in [
            ScenarioSize::Small,
            ScenarioSize::Medium,
            ScenarioSize::Large,
            ScenarioSize::Stress,
        ] {
            let scenario = BenchmarkScenario::from_size(size, 42);
            assert!(scenario.num_markets > 0);
            assert!(scenario.duration_ns() > 0);
            assert!(scenario.estimated_events() > 0);
        }
    }

    #[test]
    fn test_stage_profiler() {
        let mut profiler = StageProfiler::new(true);
        profiler.record(BenchmarkStage::BookUpdate, 1000);
        profiler.record(BenchmarkStage::BookUpdate, 2000);
        profiler.record(BenchmarkStage::Total, 3000);

        let breakdown = profiler.breakdown();
        assert!(!breakdown.is_empty());

        let book_update = breakdown.iter().find(|s| s.stage == BenchmarkStage::BookUpdate);
        assert!(book_update.is_some());
        assert_eq!(book_update.unwrap().count, 2);
    }

    #[test]
    fn test_synthetic_data_generation() {
        let scenario = BenchmarkScenario::from_size(ScenarioSize::Small, 42);
        let mut gen = SyntheticDataGenerator::new(scenario.seed);
        let events = gen.generate(&scenario);

        assert!(!events.is_empty());

        // Verify determinism - generate again with same seed
        let mut gen2 = SyntheticDataGenerator::new(scenario.seed);
        let events2 = gen2.generate(&scenario);

        assert_eq!(events.len(), events2.len());
        for (e1, e2) in events.iter().zip(events2.iter()) {
            assert_eq!(e1.time, e2.time);
            assert_eq!(e1.seq, e2.seq);
        }
    }

    #[test]
    fn test_memory_profiler() {
        let mut profiler = MemoryProfiler::new(10);
        profiler.start();

        for i in 0..10 {
            profiler.record(MemorySample {
                elapsed_ms: i * 10,
                sim_time_ns: i as i64 * 1_000_000_000,
                events_processed: i * 100,
                rss_bytes: Some(100_000_000 + i * 1_000_000),
                event_queue_len: 100 + i as usize * 10,
                decision_proof_buffer_len: 50,
                oms_order_count: 10 + i as usize,
                book_total_levels: 500 + i as usize * 5,
                ledger_entries_count: i,
                fingerprint_event_count: i * 10,
            });
        }

        let peak = profiler.peak();
        assert!(peak.is_some());

        let growth = profiler.detect_growth();
        assert!(growth.is_some());
    }

    #[test]
    fn test_target_comparison() {
        let targets = PerformanceTargets::default();
        let comparison = TargetComparison::evaluate(
            20.0,     // replay speed
            50_000.0, // events/sec
            None,     // no growth report
            None,     // no determinism result
            &targets,
        );

        assert!(comparison.replay_speed_met);
        assert!(comparison.throughput_met);
        assert!(comparison.all_targets_met);
    }
}

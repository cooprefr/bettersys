//! Performance Optimization Module
//!
//! Parallelization, zero-allocation hot paths, benchmarks, and profiling.

use crate::backtest_v2::clock::Nanos;
use crate::backtest_v2::events::{Event, Price, Side, Size, TimestampedEvent};
use crate::backtest_v2::matching::{LimitOrderBook, MatchingConfig};
use crate::backtest_v2::portfolio::MarketId;
use crate::backtest_v2::validation::DeterministicSeed;
use serde::{Deserialize, Serialize};
use std::cell::UnsafeCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

// ============================================================================
// Object Pools for Zero-Allocation Hot Path
// ============================================================================

/// Pre-allocated event buffer to avoid allocations in hot loop.
pub struct EventPool {
    events: Vec<TimestampedEvent>,
    capacity: usize,
    cursor: usize,
}

impl EventPool {
    pub fn new(capacity: usize) -> Self {
        Self {
            events: Vec::with_capacity(capacity),
            capacity,
            cursor: 0,
        }
    }

    /// Reset pool for reuse (no deallocation).
    #[inline]
    pub fn reset(&mut self) {
        self.events.clear();
        self.cursor = 0;
    }

    /// Push an event, reusing existing capacity.
    #[inline]
    pub fn push(&mut self, event: TimestampedEvent) {
        if self.events.len() < self.capacity {
            self.events.push(event);
        }
        // Silently drop if at capacity (production would handle differently)
    }

    /// Drain events.
    #[inline]
    pub fn drain(&mut self) -> std::vec::Drain<'_, TimestampedEvent> {
        self.events.drain(..)
    }

    /// Get current count.
    #[inline]
    pub fn len(&self) -> usize {
        self.events.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }
}

/// Pre-allocated price level buffer.
pub struct LevelPool {
    bids: Vec<(Price, Size)>,
    asks: Vec<(Price, Size)>,
    capacity: usize,
}

impl LevelPool {
    pub fn new(capacity: usize) -> Self {
        Self {
            bids: Vec::with_capacity(capacity),
            asks: Vec::with_capacity(capacity),
            capacity,
        }
    }

    #[inline]
    pub fn reset(&mut self) {
        self.bids.clear();
        self.asks.clear();
    }

    #[inline]
    pub fn push_bid(&mut self, price: Price, size: Size) {
        if self.bids.len() < self.capacity {
            self.bids.push((price, size));
        }
    }

    #[inline]
    pub fn push_ask(&mut self, price: Price, size: Size) {
        if self.asks.len() < self.capacity {
            self.asks.push((price, size));
        }
    }

    pub fn bids(&self) -> &[(Price, Size)] {
        &self.bids
    }

    pub fn asks(&self) -> &[(Price, Size)] {
        &self.asks
    }
}

/// Arena allocator for temporary strings in hot path.
pub struct StringArena {
    buffer: Vec<u8>,
    cursor: usize,
}

impl StringArena {
    pub fn new(capacity: usize) -> Self {
        Self {
            buffer: vec![0u8; capacity],
            cursor: 0,
        }
    }

    #[inline]
    pub fn reset(&mut self) {
        self.cursor = 0;
    }

    /// Allocate a string slice (returns None if full).
    #[inline]
    pub fn alloc(&mut self, s: &str) -> Option<&str> {
        let bytes = s.as_bytes();
        if self.cursor + bytes.len() > self.buffer.len() {
            return None;
        }
        let start = self.cursor;
        self.buffer[start..start + bytes.len()].copy_from_slice(bytes);
        self.cursor += bytes.len();
        // SAFETY: We just copied valid UTF-8 bytes
        Some(unsafe { std::str::from_utf8_unchecked(&self.buffer[start..self.cursor]) })
    }

    pub fn used(&self) -> usize {
        self.cursor
    }
}

// ============================================================================
// Per-Market Isolated State (for parallelization)
// ============================================================================

/// Isolated market state that can be processed in parallel.
pub struct MarketState {
    pub market_id: MarketId,
    pub book: LimitOrderBook,
    pub matching_config: MatchingConfig,
    pub seed: DeterministicSeed,
    pub event_pool: EventPool,
    pub level_pool: LevelPool,
    /// Events processed.
    pub events_processed: u64,
    /// Last event timestamp.
    pub last_timestamp: Nanos,
}

impl MarketState {
    pub fn new(market_id: impl Into<String>, config: MatchingConfig, seed: u64) -> Self {
        let market_id = market_id.into();
        Self {
            book: LimitOrderBook::new(&market_id, config.clone()),
            market_id,
            matching_config: config,
            seed: DeterministicSeed::new(seed),
            event_pool: EventPool::new(1024),
            level_pool: LevelPool::new(100),
            events_processed: 0,
            last_timestamp: 0,
        }
    }

    /// Reset for reuse.
    pub fn reset(&mut self) {
        self.book = LimitOrderBook::new(&self.market_id, self.matching_config.clone());
        self.event_pool.reset();
        self.level_pool.reset();
        self.events_processed = 0;
        self.last_timestamp = 0;
    }
}

// ============================================================================
// Parallel Market Processor
// ============================================================================

/// Configuration for parallel processing.
#[derive(Debug, Clone)]
pub struct ParallelConfig {
    /// Number of worker threads (0 = use all cores).
    pub num_threads: usize,
    /// Batch size for processing.
    pub batch_size: usize,
    /// Enable work stealing.
    pub work_stealing: bool,
}

impl Default for ParallelConfig {
    fn default() -> Self {
        Self {
            num_threads: 0, // Auto-detect
            batch_size: 1000,
            work_stealing: true,
        }
    }
}

/// Parallel backtest processor.
/// Processes multiple markets in parallel while maintaining per-market determinism.
pub struct ParallelProcessor {
    config: ParallelConfig,
    markets: Vec<MarketState>,
    /// Global event counter for ordering.
    global_event_counter: AtomicU64,
    /// Profiler.
    profiler: Option<Profiler>,
}

impl ParallelProcessor {
    pub fn new(config: ParallelConfig) -> Self {
        Self {
            config,
            markets: Vec::new(),
            global_event_counter: AtomicU64::new(0),
            profiler: None,
        }
    }

    /// Add a market.
    pub fn add_market(
        &mut self,
        market_id: impl Into<String>,
        matching_config: MatchingConfig,
        seed: u64,
    ) {
        self.markets
            .push(MarketState::new(market_id, matching_config, seed));
    }

    /// Enable profiling.
    pub fn enable_profiling(&mut self) {
        self.profiler = Some(Profiler::new());
    }

    /// Get profiler results.
    pub fn profiler(&self) -> Option<&Profiler> {
        self.profiler.as_ref()
    }

    /// Process events for all markets in parallel.
    /// Events are partitioned by market_id and processed independently.
    pub fn process_batch(&mut self, events: &[MarketEvent]) -> ProcessingResult {
        let start = Instant::now();
        let mut total_events = 0u64;

        // Partition events by market
        let mut by_market: HashMap<&str, Vec<&MarketEvent>> = HashMap::new();
        for event in events {
            by_market.entry(&event.market_id).or_default().push(event);
        }

        // Process each market (could be parallelized with rayon)
        // For now, sequential but isolated for determinism demonstration
        for market in &mut self.markets {
            if let Some(market_events) = by_market.get(market.market_id.as_str()) {
                let count = Self::process_market_events_static(market, market_events);
                total_events += count;
                self.global_event_counter
                    .fetch_add(count, Ordering::Relaxed);
            }
        }

        let elapsed = start.elapsed();

        if let Some(ref mut profiler) = self.profiler {
            profiler.record_batch(total_events, elapsed.as_nanos() as u64);
        }

        ProcessingResult {
            events_processed: total_events,
            elapsed_ns: elapsed.as_nanos() as u64,
            events_per_second: if elapsed.as_secs_f64() > 0.0 {
                total_events as f64 / elapsed.as_secs_f64()
            } else {
                0.0
            },
        }
    }

    /// Process events for a single market (maintains determinism).
    fn process_market_events_static(market: &mut MarketState, events: &[&MarketEvent]) -> u64 {
        market.event_pool.reset();
        let mut count = 0u64;

        for event in events {
            // Process event (simplified - real implementation would handle full event types)
            market.last_timestamp = event.timestamp;
            market.events_processed += 1;
            count += 1;
        }

        count
    }

    /// Run parallel processing with Rayon (if available).
    #[cfg(feature = "rayon")]
    pub fn process_batch_parallel(&mut self, events: &[MarketEvent]) -> ProcessingResult {
        use rayon::prelude::*;

        let start = Instant::now();

        // Partition events by market
        let mut by_market: HashMap<String, Vec<MarketEvent>> = HashMap::new();
        for event in events {
            by_market
                .entry(event.market_id.clone())
                .or_default()
                .push(event.clone());
        }

        // Process in parallel
        let results: Vec<u64> = self
            .markets
            .par_iter_mut()
            .map(|market| {
                let mut count = 0u64;
                if let Some(events) = by_market.get(&market.market_id) {
                    market.event_pool.reset();
                    for event in events {
                        market.last_timestamp = event.timestamp;
                        market.events_processed += 1;
                        count += 1;
                    }
                }
                count
            })
            .collect();

        let total_events: u64 = results.iter().sum();
        let elapsed = start.elapsed();

        if let Some(ref mut profiler) = self.profiler {
            profiler.record_batch(total_events, elapsed.as_nanos() as u64);
        }

        ProcessingResult {
            events_processed: total_events,
            elapsed_ns: elapsed.as_nanos() as u64,
            events_per_second: if elapsed.as_secs_f64() > 0.0 {
                total_events as f64 / elapsed.as_secs_f64()
            } else {
                0.0
            },
        }
    }

    /// Get total events processed.
    pub fn total_events(&self) -> u64 {
        self.global_event_counter.load(Ordering::Relaxed)
    }

    /// Get memory usage estimate.
    pub fn memory_usage(&self) -> MemoryUsage {
        let mut total_bytes = 0usize;
        let mut per_market = Vec::new();

        for market in &self.markets {
            let book_estimate =
                std::mem::size_of::<LimitOrderBook>() + 1000 * std::mem::size_of::<(Price, Size)>(); // Rough estimate
            let pool_estimate = market.event_pool.capacity
                * std::mem::size_of::<TimestampedEvent>()
                + market.level_pool.capacity * 2 * std::mem::size_of::<(Price, Size)>();
            let market_total = book_estimate + pool_estimate;

            total_bytes += market_total;
            per_market.push((market.market_id.clone(), market_total));
        }

        MemoryUsage {
            total_bytes,
            per_market,
            pool_overhead: self.markets.len()
                * (std::mem::size_of::<EventPool>() + std::mem::size_of::<LevelPool>()),
        }
    }
}

/// Simple market event for testing.
#[derive(Debug, Clone)]
pub struct MarketEvent {
    pub market_id: String,
    pub timestamp: Nanos,
    pub event_type: MarketEventType,
}

#[derive(Debug, Clone)]
pub enum MarketEventType {
    BookUpdate {
        bids: Vec<(Price, Size)>,
        asks: Vec<(Price, Size)>,
    },
    Trade {
        price: Price,
        size: Size,
        side: Side,
    },
    OrderAck {
        order_id: u64,
    },
    Fill {
        order_id: u64,
        price: Price,
        size: Size,
    },
}

/// Processing result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessingResult {
    pub events_processed: u64,
    pub elapsed_ns: u64,
    pub events_per_second: f64,
}

/// Memory usage breakdown.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryUsage {
    pub total_bytes: usize,
    pub per_market: Vec<(String, usize)>,
    pub pool_overhead: usize,
}

impl MemoryUsage {
    pub fn total_mb(&self) -> f64 {
        self.total_bytes as f64 / (1024.0 * 1024.0)
    }
}

// ============================================================================
// Profiler
// ============================================================================

/// Profiling stage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProfileStage {
    EventParsing,
    BookUpdate,
    Matching,
    FillProcessing,
    StrategyCallback,
    RiskCheck,
    PortfolioUpdate,
    MetricsCollection,
    Total,
}

impl ProfileStage {
    pub fn all() -> &'static [ProfileStage] {
        &[
            ProfileStage::EventParsing,
            ProfileStage::BookUpdate,
            ProfileStage::Matching,
            ProfileStage::FillProcessing,
            ProfileStage::StrategyCallback,
            ProfileStage::RiskCheck,
            ProfileStage::PortfolioUpdate,
            ProfileStage::MetricsCollection,
            ProfileStage::Total,
        ]
    }

    pub fn name(&self) -> &'static str {
        match self {
            ProfileStage::EventParsing => "event_parsing",
            ProfileStage::BookUpdate => "book_update",
            ProfileStage::Matching => "matching",
            ProfileStage::FillProcessing => "fill_processing",
            ProfileStage::StrategyCallback => "strategy_callback",
            ProfileStage::RiskCheck => "risk_check",
            ProfileStage::PortfolioUpdate => "portfolio_update",
            ProfileStage::MetricsCollection => "metrics_collection",
            ProfileStage::Total => "total",
        }
    }
}

/// Profile sample.
#[derive(Debug, Clone, Default)]
pub struct ProfileSample {
    pub count: u64,
    pub total_ns: u64,
    pub min_ns: u64,
    pub max_ns: u64,
}

impl ProfileSample {
    #[inline]
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

    pub fn mean_us(&self) -> f64 {
        self.mean_ns() / 1000.0
    }
}

/// Profiler for replay analysis.
#[derive(Debug, Default)]
pub struct Profiler {
    samples: HashMap<ProfileStage, ProfileSample>,
    batch_samples: Vec<(u64, u64)>, // (events, duration_ns)
    start_time: Option<Instant>,
    enabled: bool,
}

impl Profiler {
    pub fn new() -> Self {
        Self {
            samples: HashMap::new(),
            batch_samples: Vec::new(),
            start_time: None,
            enabled: true,
        }
    }

    /// Start profiling.
    pub fn start(&mut self) {
        self.start_time = Some(Instant::now());
        self.enabled = true;
    }

    /// Stop profiling.
    pub fn stop(&mut self) {
        self.enabled = false;
    }

    /// Record a stage duration.
    #[inline]
    pub fn record(&mut self, stage: ProfileStage, duration_ns: u64) {
        if !self.enabled {
            return;
        }
        self.samples.entry(stage).or_default().record(duration_ns);
    }

    /// Record a batch.
    pub fn record_batch(&mut self, events: u64, duration_ns: u64) {
        if !self.enabled {
            return;
        }
        self.batch_samples.push((events, duration_ns));
        self.samples
            .entry(ProfileStage::Total)
            .or_default()
            .record(duration_ns);
    }

    /// Scoped profiling helper.
    #[inline]
    pub fn scope(&mut self, stage: ProfileStage) -> ProfileScope<'_> {
        ProfileScope {
            profiler: self,
            stage,
            start: Instant::now(),
        }
    }

    /// Get sample for a stage.
    pub fn get(&self, stage: ProfileStage) -> Option<&ProfileSample> {
        self.samples.get(&stage)
    }

    /// Generate report.
    pub fn report(&self) -> ProfileReport {
        let mut stages = Vec::new();

        for stage in ProfileStage::all() {
            if let Some(sample) = self.samples.get(stage) {
                stages.push(ProfileStageReport {
                    name: stage.name().to_string(),
                    count: sample.count,
                    total_us: sample.total_ns as f64 / 1000.0,
                    mean_us: sample.mean_us(),
                    min_us: sample.min_ns as f64 / 1000.0,
                    max_us: sample.max_ns as f64 / 1000.0,
                });
            }
        }

        // Calculate events per second
        let total_events: u64 = self.batch_samples.iter().map(|(e, _)| e).sum();
        let total_duration_ns: u64 = self.batch_samples.iter().map(|(_, d)| d).sum();
        let events_per_second = if total_duration_ns > 0 {
            total_events as f64 / (total_duration_ns as f64 / 1_000_000_000.0)
        } else {
            0.0
        };

        // Identify bottleneck
        let bottleneck = stages
            .iter()
            .filter(|s| s.name != "total")
            .max_by(|a, b| a.total_us.partial_cmp(&b.total_us).unwrap())
            .map(|s| s.name.clone());

        ProfileReport {
            stages,
            total_events,
            total_duration_us: total_duration_ns as f64 / 1000.0,
            events_per_second,
            bottleneck,
        }
    }

    /// Terminal summary.
    pub fn terminal_summary(&self) -> String {
        let report = self.report();
        let mut s = String::new();

        s.push_str("\n=== PROFILE REPORT ===\n\n");
        s.push_str(&format!("Total Events:    {:>12}\n", report.total_events));
        s.push_str(&format!(
            "Total Duration:  {:>12.2} ms\n",
            report.total_duration_us / 1000.0
        ));
        s.push_str(&format!(
            "Events/Second:   {:>12.0}\n",
            report.events_per_second
        ));

        if let Some(ref bottleneck) = report.bottleneck {
            s.push_str(&format!("Bottleneck:      {:>12}\n", bottleneck));
        }

        s.push_str("\nBreakdown by Stage:\n");
        s.push_str(&format!(
            "{:20} {:>10} {:>12} {:>10} {:>10}\n",
            "Stage", "Count", "Total(us)", "Mean(us)", "Max(us)"
        ));
        s.push_str(&format!("{}\n", "-".repeat(64)));

        for stage in &report.stages {
            s.push_str(&format!(
                "{:20} {:>10} {:>12.1} {:>10.2} {:>10.1}\n",
                stage.name, stage.count, stage.total_us, stage.mean_us, stage.max_us
            ));
        }

        s
    }
}

/// Scoped profiling RAII guard.
pub struct ProfileScope<'a> {
    profiler: &'a mut Profiler,
    stage: ProfileStage,
    start: Instant,
}

impl<'a> Drop for ProfileScope<'a> {
    fn drop(&mut self) {
        let duration = self.start.elapsed().as_nanos() as u64;
        self.profiler.record(self.stage, duration);
    }
}

/// Profile report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileReport {
    pub stages: Vec<ProfileStageReport>,
    pub total_events: u64,
    pub total_duration_us: f64,
    pub events_per_second: f64,
    pub bottleneck: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileStageReport {
    pub name: String,
    pub count: u64,
    pub total_us: f64,
    pub mean_us: f64,
    pub min_us: f64,
    pub max_us: f64,
}

// ============================================================================
// Benchmarks
// ============================================================================

/// Benchmark configuration.
#[derive(Debug, Clone)]
pub struct BenchmarkConfig {
    pub num_markets: usize,
    pub events_per_market: usize,
    pub warmup_events: usize,
    pub iterations: usize,
}

impl Default for BenchmarkConfig {
    fn default() -> Self {
        Self {
            num_markets: 10,
            events_per_market: 100_000,
            warmup_events: 10_000,
            iterations: 3,
        }
    }
}

/// Benchmark results.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchmarkResult {
    pub name: String,
    pub config: BenchmarkParams,
    pub events_per_second: f64,
    pub mean_latency_us: f64,
    pub p99_latency_us: f64,
    pub memory_mb: f64,
    pub iterations: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchmarkParams {
    pub num_markets: usize,
    pub events_per_market: usize,
}

/// Benchmark runner.
pub struct BenchmarkRunner {
    config: BenchmarkConfig,
    results: Vec<BenchmarkResult>,
}

impl BenchmarkRunner {
    pub fn new(config: BenchmarkConfig) -> Self {
        Self {
            config,
            results: Vec::new(),
        }
    }

    /// Run event throughput benchmark.
    pub fn run_throughput(&mut self) -> BenchmarkResult {
        let mut processor = ParallelProcessor::new(ParallelConfig::default());
        processor.enable_profiling();

        // Setup markets
        for i in 0..self.config.num_markets {
            processor.add_market(format!("market_{}", i), MatchingConfig::default(), i as u64);
        }

        // Generate test events
        let events: Vec<MarketEvent> = (0..self.config.events_per_market)
            .flat_map(|i| {
                (0..self.config.num_markets).map(move |m| MarketEvent {
                    market_id: format!("market_{}", m),
                    timestamp: i as i64 * 1_000_000,
                    event_type: MarketEventType::BookUpdate {
                        bids: vec![(0.50, 100.0)],
                        asks: vec![(0.51, 100.0)],
                    },
                })
            })
            .collect();

        // Warmup
        let warmup_events = &events[..self.config.warmup_events.min(events.len())];
        processor.process_batch(warmup_events);

        // Benchmark iterations
        let mut latencies = Vec::new();
        let mut total_events = 0u64;
        let mut total_duration = std::time::Duration::ZERO;

        for _ in 0..self.config.iterations {
            let start = Instant::now();
            let result = processor.process_batch(&events);
            let elapsed = start.elapsed();

            total_events += result.events_processed;
            total_duration += elapsed;
            latencies.push(elapsed.as_micros() as f64 / result.events_processed as f64);
        }

        // Calculate stats
        latencies.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let mean_latency = latencies.iter().sum::<f64>() / latencies.len() as f64;
        let p99_idx = (latencies.len() * 99) / 100;
        let p99_latency = latencies.get(p99_idx).copied().unwrap_or(0.0);

        let events_per_second = total_events as f64 / total_duration.as_secs_f64();
        let memory = processor.memory_usage();

        let result = BenchmarkResult {
            name: "throughput".to_string(),
            config: BenchmarkParams {
                num_markets: self.config.num_markets,
                events_per_market: self.config.events_per_market,
            },
            events_per_second,
            mean_latency_us: mean_latency,
            p99_latency_us: p99_latency,
            memory_mb: memory.total_mb(),
            iterations: self.config.iterations,
        };

        self.results.push(result.clone());
        result
    }

    /// Run memory efficiency benchmark.
    pub fn run_memory(&mut self) -> BenchmarkResult {
        let mut processor = ParallelProcessor::new(ParallelConfig::default());

        // Scale up markets
        for i in 0..self.config.num_markets * 10 {
            processor.add_market(format!("market_{}", i), MatchingConfig::default(), i as u64);
        }

        let memory = processor.memory_usage();

        let result = BenchmarkResult {
            name: "memory".to_string(),
            config: BenchmarkParams {
                num_markets: self.config.num_markets * 10,
                events_per_market: 0,
            },
            events_per_second: 0.0,
            mean_latency_us: 0.0,
            p99_latency_us: 0.0,
            memory_mb: memory.total_mb(),
            iterations: 1,
        };

        self.results.push(result.clone());
        result
    }

    /// Get all results.
    pub fn results(&self) -> &[BenchmarkResult] {
        &self.results
    }

    /// Terminal summary.
    pub fn terminal_summary(&self) -> String {
        let mut s = String::new();
        s.push_str("\n=== BENCHMARK RESULTS ===\n\n");

        for result in &self.results {
            s.push_str(&format!("Benchmark: {}\n", result.name));
            s.push_str(&format!(
                "  Markets:          {:>10}\n",
                result.config.num_markets
            ));
            s.push_str(&format!(
                "  Events/Market:    {:>10}\n",
                result.config.events_per_market
            ));
            s.push_str(&format!(
                "  Events/Second:    {:>10.0}\n",
                result.events_per_second
            ));
            s.push_str(&format!(
                "  Mean Latency:     {:>10.3} us\n",
                result.mean_latency_us
            ));
            s.push_str(&format!(
                "  P99 Latency:      {:>10.3} us\n",
                result.p99_latency_us
            ));
            s.push_str(&format!(
                "  Memory:           {:>10.2} MB\n",
                result.memory_mb
            ));
            s.push_str("\n");
        }

        s
    }
}

// ============================================================================
// Hot Path Optimizations
// ============================================================================

/// Inline event processing without allocations.
#[inline(always)]
pub fn process_event_inline(
    event_type: u8,
    timestamp: Nanos,
    price: Price,
    size: Size,
    side: u8,
    output: &mut [f64; 8],
) {
    // All operations in registers, no heap allocation
    output[0] = timestamp as f64;
    output[1] = price;
    output[2] = size;
    output[3] = side as f64;
    output[4] = event_type as f64;
    output[5] = price * size; // notional
    output[6] = 0.0; // reserved
    output[7] = 0.0; // reserved
}

/// Pre-computed price tick lookup table.
pub struct TickLookup {
    tick_size: Price,
    inverse_tick: Price,
    // Pre-computed ticks for common prices (0.01 to 0.99)
    lookup: [i32; 99],
}

impl TickLookup {
    pub fn new(tick_size: Price) -> Self {
        let inverse_tick = 1.0 / tick_size;
        let mut lookup = [0i32; 99];
        for i in 0..99 {
            let price = (i + 1) as f64 * 0.01;
            lookup[i] = (price * inverse_tick).round() as i32;
        }
        Self {
            tick_size,
            inverse_tick,
            lookup,
        }
    }

    /// Fast price to ticks conversion.
    #[inline(always)]
    pub fn to_ticks(&self, price: Price) -> i32 {
        // Fast path for common prices
        let cents = (price * 100.0).round() as usize;
        if cents >= 1 && cents <= 99 {
            return self.lookup[cents - 1];
        }
        // Fallback
        (price * self.inverse_tick).round() as i32
    }

    /// Fast ticks to price conversion.
    #[inline(always)]
    pub fn to_price(&self, ticks: i32) -> Price {
        ticks as f64 * self.tick_size
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_event_pool() {
        let mut pool = EventPool::new(100);
        assert!(pool.is_empty());

        pool.reset();
        assert_eq!(pool.len(), 0);
    }

    #[test]
    fn test_level_pool() {
        let mut pool = LevelPool::new(10);
        pool.push_bid(0.50, 100.0);
        pool.push_ask(0.51, 100.0);

        assert_eq!(pool.bids().len(), 1);
        assert_eq!(pool.asks().len(), 1);

        pool.reset();
        assert!(pool.bids().is_empty());
    }

    #[test]
    fn test_string_arena() {
        let mut arena = StringArena::new(1024);

        let s1 = arena.alloc("hello").unwrap();
        assert_eq!(s1, "hello");

        let s2 = arena.alloc("world").unwrap();
        assert_eq!(s2, "world");

        assert_eq!(arena.used(), 10);

        arena.reset();
        assert_eq!(arena.used(), 0);
    }

    #[test]
    fn test_profiler() {
        let mut profiler = Profiler::new();
        profiler.start();

        profiler.record(ProfileStage::EventParsing, 1000);
        profiler.record(ProfileStage::EventParsing, 2000);
        profiler.record(ProfileStage::BookUpdate, 500);

        let sample = profiler.get(ProfileStage::EventParsing).unwrap();
        assert_eq!(sample.count, 2);
        assert_eq!(sample.total_ns, 3000);
        assert_eq!(sample.mean_ns(), 1500.0);
    }

    #[test]
    fn test_tick_lookup() {
        let lookup = TickLookup::new(0.01);

        assert_eq!(lookup.to_ticks(0.50), 50);
        assert_eq!(lookup.to_ticks(0.01), 1);
        assert_eq!(lookup.to_ticks(0.99), 99);

        assert!((lookup.to_price(50) - 0.50).abs() < 1e-9);
    }

    #[test]
    fn test_process_event_inline() {
        let mut output = [0.0f64; 8];
        process_event_inline(1, 1_000_000_000, 0.55, 100.0, 0, &mut output);

        assert_eq!(output[0], 1_000_000_000.0);
        assert_eq!(output[1], 0.55);
        assert_eq!(output[2], 100.0);
        assert!((output[5] - 55.0).abs() < 1e-9); // notional
    }

    #[test]
    fn test_parallel_processor() {
        let mut processor = ParallelProcessor::new(ParallelConfig::default());
        processor.add_market("market1", MatchingConfig::default(), 1);
        processor.add_market("market2", MatchingConfig::default(), 2);
        processor.enable_profiling();

        let events = vec![
            MarketEvent {
                market_id: "market1".into(),
                timestamp: 1000,
                event_type: MarketEventType::BookUpdate {
                    bids: vec![(0.50, 100.0)],
                    asks: vec![(0.51, 100.0)],
                },
            },
            MarketEvent {
                market_id: "market2".into(),
                timestamp: 1000,
                event_type: MarketEventType::Trade {
                    price: 0.50,
                    size: 50.0,
                    side: Side::Buy,
                },
            },
        ];

        let result = processor.process_batch(&events);
        assert_eq!(result.events_processed, 2);
    }

    #[test]
    fn test_benchmark_runner() {
        let config = BenchmarkConfig {
            num_markets: 2,
            events_per_market: 100,
            warmup_events: 10,
            iterations: 1,
        };

        let mut runner = BenchmarkRunner::new(config);
        let result = runner.run_throughput();

        assert!(result.events_per_second > 0.0);
        assert!(result.memory_mb > 0.0);
    }
}

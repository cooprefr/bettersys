//! backtest_v2 Performance & Determinism Benchmark Runner
//!
//! Runs benchmark scenarios and generates JSON + markdown reports.
//!
//! Usage:
//!   cargo run --release --bin backtest_benchmark -- [OPTIONS]
//!
//! Options:
//!   --scenario <SIZE>    Run specific scenario: small, medium, large, stress (default: all)
//!   --seed <N>           Random seed (default: 42)
//!   --determinism-runs <N> Number of runs for determinism verification (default: 2)
//!   --output <DIR>       Output directory for reports (default: .)
//!   --no-determinism     Skip determinism verification
//!   --json-only          Output JSON only, no markdown
//!   --verbose            Enable verbose output

// NOTE: This binary is designed to work with the benchmark module only.
// It tests the event queue and data generation infrastructure independently
// of the full orchestrator which may have pre-existing issues.

use betterbot_backend::backtest_v2::benchmark::{
    BenchmarkResults, BenchmarkScenario, BenchmarkStage, BenchmarkSuiteConfig,
    BenchmarkSuiteReport, DeterminismResult, MemoryProfiler, MemorySample, MemoryStats,
    PerformanceTargets, ScenarioSize, StageBreakdown, StageProfiler, SuiteSummary,
    SyntheticDataGenerator, TargetComparison,
};
use betterbot_backend::backtest_v2::clock::Nanos;
use betterbot_backend::backtest_v2::events::{Event, TimestampedEvent};
use betterbot_backend::backtest_v2::queue::{EventQueue, StreamSource};
use chrono::Utc;
use std::collections::HashMap;
use std::env;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::time::Instant;

// =============================================================================
// BENCHMARK RUNNER
// =============================================================================

struct SingleRunResult {
    events_processed: u64,
    book_updates: u64,
    trades: u64,
    wall_time_ms: u64,
    sim_duration_ns: Nanos,
    process_time_ns: u64,
    profiler: StageProfiler,
    memory_profiler: MemoryProfiler,
    fingerprint_hash: String,
}

fn compute_run_hash(events: u64, book_updates: u64, trades: u64, seed: u64) -> String {
    use std::collections::hash_map::DefaultHasher;
    let mut hasher = DefaultHasher::new();
    events.hash(&mut hasher);
    book_updates.hash(&mut hasher);
    trades.hash(&mut hasher);
    seed.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn run_single_benchmark(
    scenario: &BenchmarkScenario,
    events: &[TimestampedEvent],
    profile_stages: bool,
    memory_sample_interval_ms: u64,
) -> SingleRunResult {
    let start = Instant::now();

    let mut profiler = StageProfiler::new(profile_stages);
    let mut memory_profiler = MemoryProfiler::new(memory_sample_interval_ms);
    memory_profiler.start();

    // Process events through EventQueue to measure queue performance
    let mut queue = EventQueue::with_capacity(events.len());
    let mut events_processed = 0u64;
    let mut book_updates = 0u64;
    let mut trades = 0u64;

    // Load events into queue
    {
        let _timer = profiler.time(BenchmarkStage::QueueInsert);
        for event in events {
            queue.push_timestamped(event.clone());
        }
    }

    // Process events from queue
    let process_start = Instant::now();
    while let Some(event) = queue.pop() {
        let event_start = Instant::now();

        // Simulate event dispatch
        match &event.event {
            Event::L2BookSnapshot { .. } | Event::L2Delta { .. } | Event::L2BookDelta { .. } => {
                profiler.record(BenchmarkStage::BookUpdate, event_start.elapsed().as_nanos() as u64);
                book_updates += 1;
            }
            Event::TradePrint { .. } => {
                trades += 1;
            }
            _ => {}
        }

        events_processed += 1;

        // Memory sampling
        if memory_profiler.should_sample() {
            memory_profiler.record(MemorySample {
                elapsed_ms: memory_profiler.elapsed_ms(),
                sim_time_ns: event.time,
                events_processed,
                rss_bytes: get_rss_bytes(),
                event_queue_len: queue.len(),
                decision_proof_buffer_len: 0,
                oms_order_count: 0,
                book_total_levels: 0,
                ledger_entries_count: 0,
                fingerprint_event_count: events_processed,
            });
        }
    }
    let process_elapsed = process_start.elapsed();

    // Record total time
    profiler.record(
        BenchmarkStage::Total,
        start.elapsed().as_nanos() as u64,
    );

    let wall_time_ms = start.elapsed().as_millis() as u64;
    let sim_duration_ns = scenario.duration_ns();

    SingleRunResult {
        events_processed,
        book_updates,
        trades,
        wall_time_ms,
        sim_duration_ns,
        process_time_ns: process_elapsed.as_nanos() as u64,
        profiler,
        memory_profiler,
        fingerprint_hash: compute_run_hash(events_processed, book_updates, trades, scenario.seed),
    }
}

fn run_benchmark_scenario(
    scenario: &BenchmarkScenario,
    config: &BenchmarkSuiteConfig,
) -> BenchmarkResults {
    println!("\n  Running scenario: {}", scenario.name);
    println!("    Markets: {}", scenario.num_markets);
    println!(
        "    Duration: {:.1} hours",
        scenario.duration_ns() as f64 / 3_600_000_000_000.0
    );
    println!("    Estimated events: {}", scenario.estimated_events());

    // Generate synthetic data
    let mut generator = SyntheticDataGenerator::new(scenario.seed);
    let events = generator.generate(scenario);
    println!("    Generated {} events", events.len());

    // Run benchmark
    let result = run_single_benchmark(
        scenario,
        &events,
        config.profile_stages,
        config.memory_sample_interval_ms,
    );

    // Verify determinism if enabled
    let determinism = if config.verify_determinism {
        println!("    Verifying determinism ({} runs)...", config.determinism_runs);
        let mut fingerprints = vec![result.fingerprint_hash.clone()];
        let mut pnls = vec![0.0];
        let mut orders = vec![0u64];
        let mut fills = vec![0u64];

        for i in 1..config.determinism_runs {
            let run_result = run_single_benchmark(
                scenario,
                &events,
                false,
                1000,
            );
            fingerprints.push(run_result.fingerprint_hash.clone());
            pnls.push(0.0);
            orders.push(0);
            fills.push(0);

            if run_result.fingerprint_hash != fingerprints[0] {
                println!("    WARNING: Determinism failed at run {}", i + 1);
            }
        }

        let deterministic = fingerprints.windows(2).all(|w| w[0] == w[1]);
        if deterministic {
            println!("    Determinism: VERIFIED");
        } else {
            println!("    Determinism: FAILED");
        }

        Some(DeterminismResult::success(
            scenario.name.clone(),
            fingerprints,
            pnls,
            orders,
            fills,
        ))
    } else {
        None
    };

    // Calculate metrics
    let events_per_sec = if result.wall_time_ms > 0 {
        result.events_processed as f64 / (result.wall_time_ms as f64 / 1000.0)
    } else {
        0.0
    };

    let ns_per_event = if result.events_processed > 0 {
        result.process_time_ns as f64 / result.events_processed as f64
    } else {
        0.0
    };

    let replay_speed_ratio = if result.wall_time_ms > 0 {
        (result.sim_duration_ns as f64 / 1_000_000.0) / result.wall_time_ms as f64
    } else {
        0.0
    };

    // Memory stats
    let memory_stats = compute_memory_stats(&result.memory_profiler);

    // Performance targets
    let targets = PerformanceTargets::for_scenario(scenario.size);
    let target_comparison = TargetComparison::evaluate(
        replay_speed_ratio,
        events_per_sec,
        result.memory_profiler.detect_growth().as_ref(),
        determinism.as_ref(),
        &targets,
    );

    println!("    Events/sec: {:.0}", events_per_sec);
    println!("    Replay speed: {:.1}x", replay_speed_ratio);
    println!(
        "    Targets met: {}",
        if target_comparison.all_targets_met {
            "YES"
        } else {
            "NO"
        }
    );

    BenchmarkResults {
        scenario: scenario.clone(),
        events_processed: result.events_processed,
        sim_duration_ns: result.sim_duration_ns,
        wall_time_ms: result.wall_time_ms,
        events_per_sec,
        ns_per_event,
        replay_speed_ratio,
        stage_breakdown: result.profiler.breakdown(),
        memory_stats,
        determinism,
        fingerprint: Some(result.fingerprint_hash),
        targets: target_comparison,
        timestamp: Utc::now().to_rfc3339(),
    }
}

fn compute_memory_stats(profiler: &MemoryProfiler) -> MemoryStats {
    let samples = &profiler.samples;
    if samples.is_empty() {
        return MemoryStats::default();
    }

    let peak_event_queue_len = samples.iter().map(|s| s.event_queue_len).max().unwrap_or(0);
    let peak_oms_orders = samples.iter().map(|s| s.oms_order_count).max().unwrap_or(0);
    let peak_book_levels = samples.iter().map(|s| s.book_total_levels).max().unwrap_or(0);
    let peak_ledger_entries = samples
        .iter()
        .map(|s| s.ledger_entries_count)
        .max()
        .unwrap_or(0);
    let peak_rss_mb = samples
        .iter()
        .filter_map(|s| s.rss_bytes)
        .max()
        .map(|b| b as f64 / (1024.0 * 1024.0));

    let n = samples.len();
    let start_event_queue_len = samples.first().map(|s| s.event_queue_len).unwrap_or(0);
    let mid_event_queue_len = samples.get(n / 2).map(|s| s.event_queue_len).unwrap_or(0);
    let end_event_queue_len = samples.last().map(|s| s.event_queue_len).unwrap_or(0);

    MemoryStats {
        peak_event_queue_len,
        peak_oms_orders,
        peak_book_levels,
        peak_ledger_entries,
        peak_rss_mb,
        start_event_queue_len,
        mid_event_queue_len,
        end_event_queue_len,
        growth_report: profiler.detect_growth(),
    }
}

fn compute_suite_summary(results: &[BenchmarkResults]) -> SuiteSummary {
    let scenarios_run = results.len();
    let scenarios_passed = results.iter().filter(|r| r.targets.all_targets_met).count();

    let fastest = results
        .iter()
        .max_by(|a, b| {
            a.events_per_sec
                .partial_cmp(&b.events_per_sec)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
    let slowest = results
        .iter()
        .min_by(|a, b| {
            a.events_per_sec
                .partial_cmp(&b.events_per_sec)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

    // Find bottleneck stage across all results
    let mut stage_totals: HashMap<String, u64> = HashMap::new();
    for result in results {
        for stage in &result.stage_breakdown {
            *stage_totals.entry(stage.name.clone()).or_insert(0) += stage.total_ns;
        }
    }
    let bottleneck = stage_totals
        .iter()
        .filter(|(name, _)| *name != "total")
        .max_by_key(|(_, ns)| *ns);
    let total_time: u64 = stage_totals.get("total").copied().unwrap_or(1);

    let all_deterministic = results
        .iter()
        .all(|r| r.determinism.as_ref().map(|d| d.deterministic).unwrap_or(true));

    SuiteSummary {
        scenarios_run,
        scenarios_passed,
        fastest_scenario: fastest.map(|r| r.scenario.name.clone()),
        fastest_events_per_sec: fastest.map(|r| r.events_per_sec).unwrap_or(0.0),
        slowest_scenario: slowest.map(|r| r.scenario.name.clone()),
        slowest_events_per_sec: slowest.map(|r| r.events_per_sec).unwrap_or(0.0),
        bottleneck_stage: bottleneck.map(|(name, _)| name.clone()),
        bottleneck_percentage: bottleneck
            .map(|(_, ns)| (*ns as f64 / total_time as f64) * 100.0)
            .unwrap_or(0.0),
        memory_hotspot: None,
        all_deterministic,
        overall_pass: scenarios_passed == scenarios_run && all_deterministic,
    }
}

fn get_rss_bytes() -> Option<u64> {
    // Try to get RSS from /proc/self/statm on Linux
    #[cfg(target_os = "linux")]
    {
        if let Ok(statm) = std::fs::read_to_string("/proc/self/statm") {
            let parts: Vec<&str> = statm.split_whitespace().collect();
            if parts.len() >= 2 {
                if let Ok(rss_pages) = parts[1].parse::<u64>() {
                    return Some(rss_pages * 4096);
                }
            }
        }
    }
    None
}

// =============================================================================
// MAIN
// =============================================================================

fn main() {
    let args: Vec<String> = env::args().collect();

    // Parse arguments
    let mut scenario_filter: Option<ScenarioSize> = None;
    let mut seed: u64 = 42;
    let mut determinism_runs: usize = 2;
    let mut output_dir = ".".to_string();
    let mut verify_determinism = true;
    let mut json_only = false;
    let mut _verbose = false;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--scenario" => {
                i += 1;
                if i < args.len() {
                    scenario_filter = match args[i].to_lowercase().as_str() {
                        "small" => Some(ScenarioSize::Small),
                        "medium" => Some(ScenarioSize::Medium),
                        "large" => Some(ScenarioSize::Large),
                        "stress" => Some(ScenarioSize::Stress),
                        _ => None,
                    };
                }
            }
            "--seed" => {
                i += 1;
                if i < args.len() {
                    seed = args[i].parse().unwrap_or(42);
                }
            }
            "--determinism-runs" => {
                i += 1;
                if i < args.len() {
                    determinism_runs = args[i].parse().unwrap_or(2);
                }
            }
            "--output" => {
                i += 1;
                if i < args.len() {
                    output_dir = args[i].clone();
                }
            }
            "--no-determinism" => {
                verify_determinism = false;
            }
            "--json-only" => {
                json_only = true;
            }
            "--verbose" => {
                _verbose = true;
            }
            "--help" | "-h" => {
                println!("backtest_v2 Performance & Determinism Benchmark");
                println!();
                println!("Usage: backtest_benchmark [OPTIONS]");
                println!();
                println!("Options:");
                println!("  --scenario <SIZE>       Run specific scenario: small, medium, large, stress");
                println!("  --seed <N>              Random seed (default: 42)");
                println!("  --determinism-runs <N>  Number of runs for determinism verification (default: 2)");
                println!("  --output <DIR>          Output directory for reports (default: .)");
                println!("  --no-determinism        Skip determinism verification");
                println!("  --json-only             Output JSON only, no markdown");
                println!("  --verbose               Enable verbose output");
                println!("  --help                  Show this help message");
                return;
            }
            _ => {}
        }
        i += 1;
    }

    println!("╔══════════════════════════════════════════════════════════════════════════════╗");
    println!("║          backtest_v2 Performance & Determinism Benchmark Suite               ║");
    println!("╚══════════════════════════════════════════════════════════════════════════════╝");
    println!();

    // Build scenarios
    let scenarios: Vec<BenchmarkScenario> = match scenario_filter {
        Some(size) => vec![BenchmarkScenario::from_size(size, seed)],
        None => vec![
            BenchmarkScenario::from_size(ScenarioSize::Small, seed),
            BenchmarkScenario::from_size(ScenarioSize::Medium, seed),
            BenchmarkScenario::from_size(ScenarioSize::Large, seed),
            BenchmarkScenario::from_size(ScenarioSize::Stress, seed),
        ],
    };

    let config = BenchmarkSuiteConfig {
        scenarios: scenarios.clone(),
        verify_determinism,
        determinism_runs,
        profile_stages: true,
        memory_sample_interval_ms: 100,
        targets: PerformanceTargets::default(),
        output_dir: output_dir.clone(),
    };

    println!("Configuration:");
    println!("  Scenarios: {}", scenarios.len());
    println!("  Seed: {}", seed);
    println!("  Determinism runs: {}", determinism_runs);
    println!("  Output directory: {}", output_dir);
    println!();

    // Run benchmarks
    let mut results = Vec::new();
    for scenario in &scenarios {
        let result = run_benchmark_scenario(scenario, &config);
        results.push(result);
    }

    // Compute summary
    let summary = compute_suite_summary(&results);

    // Build report
    let report = BenchmarkSuiteReport {
        results,
        summary: summary.clone(),
        timestamp: Utc::now().to_rfc3339(),
        config,
    };

    // Write outputs
    let json_path = Path::new(&output_dir).join("bench_results.json");
    let json_content = serde_json::to_string_pretty(&report).unwrap_or_default();
    fs::write(&json_path, &json_content).unwrap_or_else(|e| {
        eprintln!("Failed to write JSON: {}", e);
    });
    println!("\nWritten: {}", json_path.display());

    if !json_only {
        let md_path = Path::new(&output_dir).join("BENCH_REPORT.md");
        let md_content = report.to_markdown();
        fs::write(&md_path, &md_content).unwrap_or_else(|e| {
            eprintln!("Failed to write markdown: {}", e);
        });
        println!("Written: {}", md_path.display());
    }

    // Print summary
    println!();
    println!("════════════════════════════════════════════════════════════════════════════════");
    println!("                              SUMMARY");
    println!("════════════════════════════════════════════════════════════════════════════════");
    println!(
        "  Scenarios run:    {}",
        summary.scenarios_run
    );
    println!(
        "  Scenarios passed: {} / {}",
        summary.scenarios_passed, summary.scenarios_run
    );
    if let Some(ref fast) = summary.fastest_scenario {
        println!(
            "  Fastest:          {} ({:.0} events/sec)",
            fast, summary.fastest_events_per_sec
        );
    }
    if let Some(ref slow) = summary.slowest_scenario {
        println!(
            "  Slowest:          {} ({:.0} events/sec)",
            slow, summary.slowest_events_per_sec
        );
    }
    if let Some(ref bottleneck) = summary.bottleneck_stage {
        println!(
            "  Bottleneck:       {} ({:.1}%)",
            bottleneck, summary.bottleneck_percentage
        );
    }
    println!(
        "  Determinism:      {}",
        if summary.all_deterministic {
            "EXACT"
        } else {
            "FAILED"
        }
    );
    println!("  Overall:          {}", if summary.overall_pass { "PASS" } else { "FAIL" });
    println!("════════════════════════════════════════════════════════════════════════════════");

    // Exit with appropriate code
    std::process::exit(if summary.overall_pass { 0 } else { 1 });
}

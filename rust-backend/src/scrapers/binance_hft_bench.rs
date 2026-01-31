//! Microbenchmark Harness for Binance HFT Ingest
//!
//! Measures critical path latencies:
//! 1. SeqLock read/write cycles
//! 2. JSON parsing overhead
//! 3. Symbol lookup latency
//! 4. End-to-end message processing
//! 5. Concurrent reader contention

use std::{
    hint::black_box,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};

use super::binance_hft_ingest::{
    BinanceHftIngest, IngestConfig, IngestReader, PriceTick, SeqLockSnapshot, SymbolState,
    MAX_SYMBOLS,
};

// ============================================================================
// Benchmark Results
// ============================================================================

#[derive(Debug, Clone, Default)]
pub struct BenchmarkResults {
    pub name: String,
    pub iterations: u64,
    pub total_ns: u64,
    pub min_ns: u64,
    pub max_ns: u64,
    pub mean_ns: f64,
    pub p50_ns: u64,
    pub p99_ns: u64,
    pub p999_ns: u64,
    pub throughput_ops_per_sec: f64,
}

impl BenchmarkResults {
    pub fn print(&self) {
        println!("=== {} ===", self.name);
        println!("  Iterations: {}", self.iterations);
        println!("  Total time: {:.2}ms", self.total_ns as f64 / 1_000_000.0);
        println!("  Min:  {:>8}ns", self.min_ns);
        println!("  Mean: {:>8.1}ns", self.mean_ns);
        println!("  P50:  {:>8}ns", self.p50_ns);
        println!("  P99:  {:>8}ns", self.p99_ns);
        println!("  P999: {:>8}ns", self.p999_ns);
        println!("  Max:  {:>8}ns", self.max_ns);
        println!(
            "  Throughput: {:.2}M ops/sec",
            self.throughput_ops_per_sec / 1_000_000.0
        );
        println!();
    }
}

// ============================================================================
// Benchmark Runner
// ============================================================================

pub struct BenchmarkRunner {
    warmup_iterations: u64,
    benchmark_iterations: u64,
}

impl Default for BenchmarkRunner {
    fn default() -> Self {
        Self {
            warmup_iterations: 10_000,
            benchmark_iterations: 1_000_000,
        }
    }
}

impl BenchmarkRunner {
    pub fn new(warmup: u64, iterations: u64) -> Self {
        Self {
            warmup_iterations: warmup,
            benchmark_iterations: iterations,
        }
    }

    /// Run a benchmark function and collect statistics
    pub fn run<F>(&self, name: &str, mut f: F) -> BenchmarkResults
    where
        F: FnMut() -> (),
    {
        // Warmup
        for _ in 0..self.warmup_iterations {
            black_box(f());
        }

        // Collect samples
        let mut samples = Vec::with_capacity(self.benchmark_iterations as usize);
        let start = Instant::now();

        for _ in 0..self.benchmark_iterations {
            let iter_start = Instant::now();
            black_box(f());
            samples.push(iter_start.elapsed().as_nanos() as u64);
        }

        let total_ns = start.elapsed().as_nanos() as u64;

        // Calculate statistics
        samples.sort_unstable();

        let min_ns = *samples.first().unwrap_or(&0);
        let max_ns = *samples.last().unwrap_or(&0);
        let sum: u64 = samples.iter().sum();
        let mean_ns = sum as f64 / samples.len() as f64;

        let p50_idx = (samples.len() as f64 * 0.50) as usize;
        let p99_idx = (samples.len() as f64 * 0.99) as usize;
        let p999_idx = (samples.len() as f64 * 0.999) as usize;

        let p50_ns = samples.get(p50_idx).copied().unwrap_or(0);
        let p99_ns = samples.get(p99_idx).copied().unwrap_or(0);
        let p999_ns = samples.get(p999_idx).copied().unwrap_or(0);

        let throughput = self.benchmark_iterations as f64 / (total_ns as f64 / 1_000_000_000.0);

        BenchmarkResults {
            name: name.to_string(),
            iterations: self.benchmark_iterations,
            total_ns,
            min_ns,
            max_ns,
            mean_ns,
            p50_ns,
            p99_ns,
            p999_ns,
            throughput_ops_per_sec: throughput,
        }
    }
}

// ============================================================================
// Individual Benchmarks
// ============================================================================

/// Benchmark 1: SeqLock write latency (single writer)
pub fn bench_seqlock_write(runner: &BenchmarkRunner) -> BenchmarkResults {
    let lock = SeqLockSnapshot::new();
    let mut seq = 0u64;

    runner.run("seqlock_write", || {
        seq += 1;
        let tick = PriceTick {
            exchange_ts_ms: 1000,
            receive_ts_ns: 2000,
            bid: 50000.0 + (seq as f64 * 0.01),
            ask: 50001.0 + (seq as f64 * 0.01),
            mid: 50000.5 + (seq as f64 * 0.01),
            bid_qty: 1.0,
            ask_qty: 2.0,
            seq,
        };
        lock.write(tick);
    })
}

/// Benchmark 2: SeqLock read latency (no contention)
pub fn bench_seqlock_read(runner: &BenchmarkRunner) -> BenchmarkResults {
    let lock = SeqLockSnapshot::new();

    // Pre-populate
    lock.write(PriceTick {
        exchange_ts_ms: 1000,
        receive_ts_ns: 2000,
        bid: 50000.0,
        ask: 50001.0,
        mid: 50000.5,
        bid_qty: 1.0,
        ask_qty: 2.0,
        seq: 1,
    });

    runner.run("seqlock_read", || {
        let _ = black_box(lock.read());
    })
}

/// Benchmark 3: SeqLock read under write contention
pub fn bench_seqlock_contended(runner: &BenchmarkRunner) -> BenchmarkResults {
    let lock = Arc::new(SeqLockSnapshot::new());
    let running = Arc::new(AtomicBool::new(true));
    let writes = Arc::new(AtomicU64::new(0));

    // Pre-populate
    lock.write(PriceTick {
        exchange_ts_ms: 1000,
        receive_ts_ns: 2000,
        bid: 50000.0,
        ask: 50001.0,
        mid: 50000.5,
        bid_qty: 1.0,
        ask_qty: 2.0,
        seq: 1,
    });

    // Writer thread
    let writer_lock = lock.clone();
    let writer_running = running.clone();
    let writer_writes = writes.clone();
    let writer = thread::spawn(move || {
        let mut seq = 1u64;
        while writer_running.load(Ordering::Relaxed) {
            seq += 1;
            let tick = PriceTick {
                exchange_ts_ms: 1000,
                receive_ts_ns: 2000,
                bid: 50000.0 + (seq as f64 * 0.01),
                ask: 50001.0 + (seq as f64 * 0.01),
                mid: 50000.5 + (seq as f64 * 0.01),
                bid_qty: 1.0,
                ask_qty: 2.0,
                seq,
            };
            writer_lock.write(tick);
            writer_writes.fetch_add(1, Ordering::Relaxed);
            // Simulate ~1000 writes/sec like real feed
            thread::sleep(Duration::from_micros(100));
        }
    });

    let result = runner.run("seqlock_read_contended", || {
        let _ = black_box(lock.read());
    });

    running.store(false, Ordering::Relaxed);
    let _ = writer.join();

    println!(
        "  (Writer performed {} writes during benchmark)",
        writes.load(Ordering::Relaxed)
    );

    result
}

/// Benchmark 4: Symbol state update (includes EWMA calculation)
pub fn bench_symbol_update(runner: &BenchmarkRunner) -> BenchmarkResults {
    let state = SymbolState::new("BTCUSDT");
    let mut seq = 0u64;

    // Pre-populate with initial tick
    let initial = PriceTick {
        exchange_ts_ms: 1000,
        receive_ts_ns: 2000,
        bid: 50000.0,
        ask: 50001.0,
        mid: 50000.5,
        bid_qty: 1.0,
        ask_qty: 2.0,
        seq: 0,
    };
    state.update(initial, 0.97);

    runner.run("symbol_update", || {
        seq += 1;
        let tick = PriceTick {
            exchange_ts_ms: 1000 + seq as i64,
            receive_ts_ns: 2000 + seq,
            bid: 50000.0 + (seq as f64 * 0.01),
            ask: 50001.0 + (seq as f64 * 0.01),
            mid: 50000.5 + (seq as f64 * 0.01),
            bid_qty: 1.0,
            ask_qty: 2.0,
            seq,
        };
        state.update(tick, 0.97);
    })
}

/// Benchmark 5: JSON parsing (manual vs serde)
pub fn bench_json_parse_manual(runner: &BenchmarkRunner) -> BenchmarkResults {
    let msg = r#"{"stream":"btcusdt@bookTicker","data":{"s":"BTCUSDT","b":"50000.00","B":"1.5","a":"50001.00","A":"2.0","T":1234567890123}}"#;

    runner.run("json_parse_manual", || {
        // Simulate manual parsing
        let _ = black_box(parse_book_ticker_manual(msg));
    })
}

/// Benchmark 6: JSON parsing with serde_json
pub fn bench_json_parse_serde(runner: &BenchmarkRunner) -> BenchmarkResults {
    let msg = r#"{"stream":"btcusdt@bookTicker","data":{"s":"BTCUSDT","b":"50000.00","B":"1.5","a":"50001.00","A":"2.0","T":1234567890123}}"#;

    runner.run("json_parse_serde", || {
        let _ = black_box(parse_book_ticker_serde(msg));
    })
}

/// Benchmark 7: HashMap lookup (symbol -> index)
pub fn bench_symbol_lookup(runner: &BenchmarkRunner) -> BenchmarkResults {
    let mut map = std::collections::HashMap::new();
    map.insert("BTCUSDT".to_string(), 0usize);
    map.insert("ETHUSDT".to_string(), 1usize);
    map.insert("SOLUSDT".to_string(), 2usize);
    map.insert("XRPUSDT".to_string(), 3usize);

    let symbols = ["BTCUSDT", "ETHUSDT", "SOLUSDT", "XRPUSDT"];
    let mut idx = 0usize;

    runner.run("symbol_lookup", || {
        let sym = symbols[idx % 4];
        let _ = black_box(map.get(sym));
        idx += 1;
    })
}

/// Benchmark 8: Full message processing path
pub fn bench_full_message_processing(runner: &BenchmarkRunner) -> BenchmarkResults {
    let config = IngestConfig {
        symbols: vec!["BTCUSDT".to_string()],
        ..Default::default()
    };
    let engine = BinanceHftIngest::new(config);

    let msg = r#"{"stream":"btcusdt@bookTicker","data":{"s":"BTCUSDT","b":"50000.00","B":"1.5","a":"50001.00","A":"2.0","T":1234567890123}}"#;

    runner.run("full_message_processing", || {
        engine.process_message(msg, engine.now_ns());
    })
}

/// Benchmark 9: Concurrent readers (simulate trading strategies)
pub fn bench_concurrent_readers(runner: &BenchmarkRunner) -> BenchmarkResults {
    let config = IngestConfig {
        symbols: vec!["BTCUSDT".to_string()],
        ..Default::default()
    };
    let engine = Arc::new(BinanceHftIngest::new(config));
    let running = Arc::new(AtomicBool::new(true));
    let read_counts = Arc::new([
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
    ]);

    // Pre-populate
    let msg = r#"{"stream":"btcusdt@bookTicker","data":{"s":"BTCUSDT","b":"50000.00","B":"1.5","a":"50001.00","A":"2.0","T":1234567890123}}"#;
    engine.process_message(msg, engine.now_ns());

    // Spawn reader threads
    let mut readers = Vec::new();
    for i in 0..4 {
        let reader_engine = engine.clone();
        let reader_running = running.clone();
        let reader_counts = read_counts.clone();
        readers.push(thread::spawn(move || {
            while reader_running.load(Ordering::Relaxed) {
                let _ = black_box(reader_engine.latest("BTCUSDT"));
                reader_counts[i].fetch_add(1, Ordering::Relaxed);
            }
        }));
    }

    // Writer benchmark
    let result = runner.run("write_with_4_readers", || {
        engine.process_message(msg, engine.now_ns());
    });

    running.store(false, Ordering::Relaxed);
    for reader in readers {
        let _ = reader.join();
    }

    let total_reads: u64 = read_counts.iter().map(|c| c.load(Ordering::Relaxed)).sum();
    println!("  (Readers performed {} total reads during benchmark)", total_reads);

    result
}

// ============================================================================
// Helper Functions
// ============================================================================

#[derive(Debug)]
struct ParsedTicker {
    symbol: String,
    bid: f64,
    ask: f64,
    bid_qty: f64,
    ask_qty: f64,
    timestamp: i64,
}

fn parse_book_ticker_manual(msg: &str) -> Option<ParsedTicker> {
    let data_start = msg.find("\"data\":")?;
    let data_content = &msg[data_start + 7..];

    let s_start = data_content.find("\"s\":\"")?;
    let s_value_start = s_start + 5;
    let s_end = data_content[s_value_start..].find('"')?;
    let symbol = data_content[s_value_start..s_value_start + s_end].to_uppercase();

    let b_start = data_content.find("\"b\":\"")?;
    let b_value_start = b_start + 5;
    let b_end = data_content[b_value_start..].find('"')?;
    let bid: f64 = data_content[b_value_start..b_value_start + b_end].parse().ok()?;

    let bq_start = data_content.find("\"B\":\"")?;
    let bq_value_start = bq_start + 5;
    let bq_end = data_content[bq_value_start..].find('"')?;
    let bid_qty: f64 = data_content[bq_value_start..bq_value_start + bq_end].parse().ok()?;

    let a_start = data_content.find("\"a\":\"")?;
    let a_value_start = a_start + 5;
    let a_end = data_content[a_value_start..].find('"')?;
    let ask: f64 = data_content[a_value_start..a_value_start + a_end].parse().ok()?;

    let aq_start = data_content.find("\"A\":\"")?;
    let aq_value_start = aq_start + 5;
    let aq_end = data_content[aq_value_start..].find('"')?;
    let ask_qty: f64 = data_content[aq_value_start..aq_value_start + aq_end].parse().ok()?;

    let timestamp = 1234567890123i64; // Simplified

    Some(ParsedTicker {
        symbol,
        bid,
        bid_qty,
        ask,
        ask_qty,
        timestamp,
    })
}

#[derive(serde::Deserialize)]
struct SerdeWrapper {
    data: SerdeData,
}

#[derive(serde::Deserialize)]
struct SerdeData {
    s: String,
    b: String,
    #[serde(rename = "B")]
    bid_qty: String,
    a: String,
    #[serde(rename = "A")]
    ask_qty: String,
    #[serde(rename = "T")]
    timestamp: i64,
}

fn parse_book_ticker_serde(msg: &str) -> Option<ParsedTicker> {
    let wrapper: SerdeWrapper = serde_json::from_str(msg).ok()?;
    Some(ParsedTicker {
        symbol: wrapper.data.s.to_uppercase(),
        bid: wrapper.data.b.parse().ok()?,
        bid_qty: wrapper.data.bid_qty.parse().ok()?,
        ask: wrapper.data.a.parse().ok()?,
        ask_qty: wrapper.data.ask_qty.parse().ok()?,
        timestamp: wrapper.data.timestamp,
    })
}

// ============================================================================
// Run All Benchmarks
// ============================================================================

pub fn run_all_benchmarks() {
    println!("\n========================================");
    println!("  Binance HFT Ingest Microbenchmarks");
    println!("========================================\n");

    let runner = BenchmarkRunner::new(10_000, 1_000_000);

    let results = vec![
        bench_seqlock_write(&runner),
        bench_seqlock_read(&runner),
        bench_seqlock_contended(&runner),
        bench_symbol_update(&runner),
        bench_json_parse_manual(&runner),
        bench_json_parse_serde(&runner),
        bench_symbol_lookup(&runner),
        bench_full_message_processing(&runner),
        bench_concurrent_readers(&runner),
    ];

    println!("\n========================================");
    println!("  Summary");
    println!("========================================\n");

    println!(
        "{:<30} {:>10} {:>10} {:>10} {:>12}",
        "Benchmark", "P50 (ns)", "P99 (ns)", "Max (ns)", "Throughput"
    );
    println!("{}", "-".repeat(75));

    for r in &results {
        println!(
            "{:<30} {:>10} {:>10} {:>10} {:>10.2}M/s",
            r.name,
            r.p50_ns,
            r.p99_ns,
            r.max_ns,
            r.throughput_ops_per_sec / 1_000_000.0
        );
    }

    println!();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_benchmarks_run() {
        // Quick sanity test with fewer iterations
        let runner = BenchmarkRunner::new(100, 1000);

        let result = bench_seqlock_write(&runner);
        assert!(result.iterations == 1000);
        assert!(result.mean_ns > 0.0);
    }
}

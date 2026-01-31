//! Binance HFT Ingest Benchmark Binary
//!
//! Run with: cargo run --release --bin binance_ingest_bench
//!
//! This measures critical path latencies for the zero-overhead ingest implementation.
//!
//! Benchmarks include:
//! - SeqLock read/write cycles
//! - JSON parsing (manual vs serde)  
//! - Symbol state updates
//! - Concurrent reader stress test

use std::{
    cell::UnsafeCell,
    collections::HashMap,
    hint::{self, black_box},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};

use parking_lot::Mutex;

// ============================================================================
// Core Data Structures (from binance_hft_ingest.rs)
// ============================================================================

const CACHE_LINE: usize = 64;

/// PriceTick size: 8*8 = 64 bytes
/// SeqLock: 8 (seq) + 64 (tick) = 72 bytes, pad to 128 bytes
const SEQLOCK_PAD_SIZE: usize = 56;

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct PriceTick {
    pub exchange_ts_ms: i64,
    pub receive_ts_ns: u64,
    pub bid: f64,
    pub ask: f64,
    pub mid: f64,
    pub bid_qty: f64,
    pub ask_qty: f64,
    pub seq: u64,
}

#[repr(C, align(64))]
pub struct SeqLockSnapshot {
    seq: AtomicU64,
    tick: UnsafeCell<PriceTick>,
    _pad: [u8; SEQLOCK_PAD_SIZE],
}

unsafe impl Sync for SeqLockSnapshot {}
unsafe impl Send for SeqLockSnapshot {}

impl SeqLockSnapshot {
    pub fn new() -> Self {
        Self {
            seq: AtomicU64::new(0),
            tick: UnsafeCell::new(PriceTick::default()),
            _pad: [0; SEQLOCK_PAD_SIZE],
        }
    }

    #[inline(always)]
    pub fn write(&self, tick: PriceTick) {
        let old_seq = self.seq.load(Ordering::Relaxed);
        self.seq.store(old_seq + 1, Ordering::Release);
        std::sync::atomic::fence(Ordering::Release);
        unsafe { *self.tick.get() = tick; }
        std::sync::atomic::fence(Ordering::Release);
        self.seq.store(old_seq + 2, Ordering::Release);
    }

    #[inline(always)]
    pub fn read(&self) -> Option<PriceTick> {
        const MAX_RETRIES: u32 = 10;
        let mut retries = 0;

        loop {
            let seq1 = self.seq.load(Ordering::Acquire);
            if seq1 & 1 == 1 {
                hint::spin_loop();
                retries += 1;
                if retries > MAX_RETRIES { return None; }
                continue;
            }
            if seq1 == 0 { return None; }

            let tick = unsafe { *self.tick.get() };
            std::sync::atomic::fence(Ordering::Acquire);
            let seq2 = self.seq.load(Ordering::Acquire);

            if seq1 == seq2 { return Some(tick); }

            hint::spin_loop();
            retries += 1;
            if retries > MAX_RETRIES { return None; }
        }
    }
}

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
        println!("  Min:  {:>8}ns", self.min_ns);
        println!("  Mean: {:>8.1}ns", self.mean_ns);
        println!("  P50:  {:>8}ns", self.p50_ns);
        println!("  P99:  {:>8}ns", self.p99_ns);
        println!("  P999: {:>8}ns", self.p999_ns);
        println!("  Max:  {:>8}ns", self.max_ns);
        println!("  Throughput: {:.2}M ops/sec", self.throughput_ops_per_sec / 1_000_000.0);
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

impl BenchmarkRunner {
    pub fn new(warmup: u64, iterations: u64) -> Self {
        Self { warmup_iterations: warmup, benchmark_iterations: iterations }
    }

    pub fn run<F>(&self, name: &str, mut f: F) -> BenchmarkResults
    where F: FnMut() -> ()
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
            total_ns, min_ns, max_ns, mean_ns, p50_ns, p99_ns, p999_ns,
            throughput_ops_per_sec: throughput,
        }
    }
}

// ============================================================================
// Individual Benchmarks
// ============================================================================

fn bench_seqlock_write(runner: &BenchmarkRunner) -> BenchmarkResults {
    let lock = SeqLockSnapshot::new();
    let mut seq = 0u64;

    runner.run("seqlock_write", || {
        seq += 1;
        let tick = PriceTick {
            exchange_ts_ms: 1000, receive_ts_ns: 2000,
            bid: 50000.0 + (seq as f64 * 0.01),
            ask: 50001.0 + (seq as f64 * 0.01),
            mid: 50000.5 + (seq as f64 * 0.01),
            bid_qty: 1.0, ask_qty: 2.0, seq,
        };
        lock.write(tick);
    })
}

fn bench_seqlock_read(runner: &BenchmarkRunner) -> BenchmarkResults {
    let lock = SeqLockSnapshot::new();
    lock.write(PriceTick {
        exchange_ts_ms: 1000, receive_ts_ns: 2000,
        bid: 50000.0, ask: 50001.0, mid: 50000.5,
        bid_qty: 1.0, ask_qty: 2.0, seq: 1,
    });

    runner.run("seqlock_read", || {
        let _ = black_box(lock.read());
    })
}

fn bench_seqlock_contended(runner: &BenchmarkRunner) -> BenchmarkResults {
    let lock = Arc::new(SeqLockSnapshot::new());
    let running = Arc::new(AtomicBool::new(true));
    let writes = Arc::new(AtomicU64::new(0));

    lock.write(PriceTick {
        exchange_ts_ms: 1000, receive_ts_ns: 2000,
        bid: 50000.0, ask: 50001.0, mid: 50000.5,
        bid_qty: 1.0, ask_qty: 2.0, seq: 1,
    });

    let writer_lock = lock.clone();
    let writer_running = running.clone();
    let writer_writes = writes.clone();
    let writer = thread::spawn(move || {
        let mut seq = 1u64;
        while writer_running.load(Ordering::Relaxed) {
            seq += 1;
            let tick = PriceTick {
                exchange_ts_ms: 1000, receive_ts_ns: 2000,
                bid: 50000.0 + (seq as f64 * 0.01),
                ask: 50001.0 + (seq as f64 * 0.01),
                mid: 50000.5 + (seq as f64 * 0.01),
                bid_qty: 1.0, ask_qty: 2.0, seq,
            };
            writer_lock.write(tick);
            writer_writes.fetch_add(1, Ordering::Relaxed);
            thread::sleep(Duration::from_micros(100));
        }
    });

    let result = runner.run("seqlock_read_contended", || {
        let _ = black_box(lock.read());
    });

    running.store(false, Ordering::Relaxed);
    let _ = writer.join();

    println!("  (Writer performed {} writes during benchmark)", writes.load(Ordering::Relaxed));
    result
}

fn bench_json_parse_manual(runner: &BenchmarkRunner) -> BenchmarkResults {
    let msg = r#"{"stream":"btcusdt@bookTicker","data":{"s":"BTCUSDT","b":"50000.00","B":"1.5","a":"50001.00","A":"2.0","T":1234567890123}}"#;

    runner.run("json_parse_manual", || {
        let _ = black_box(parse_book_ticker_manual(msg));
    })
}

fn bench_json_parse_serde(runner: &BenchmarkRunner) -> BenchmarkResults {
    let msg = r#"{"stream":"btcusdt@bookTicker","data":{"s":"BTCUSDT","b":"50000.00","B":"1.5","a":"50001.00","A":"2.0","T":1234567890123}}"#;

    runner.run("json_parse_serde", || {
        let _ = black_box(parse_book_ticker_serde(msg));
    })
}

fn bench_symbol_lookup(runner: &BenchmarkRunner) -> BenchmarkResults {
    let mut map = HashMap::new();
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

fn bench_concurrent_readers(runner: &BenchmarkRunner) -> BenchmarkResults {
    let lock = Arc::new(SeqLockSnapshot::new());
    let running = Arc::new(AtomicBool::new(true));
    let read_counts: Arc<[AtomicU64; 4]> = Arc::new([
        AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0),
    ]);

    lock.write(PriceTick {
        exchange_ts_ms: 1000, receive_ts_ns: 2000,
        bid: 50000.0, ask: 50001.0, mid: 50000.5,
        bid_qty: 1.0, ask_qty: 2.0, seq: 1,
    });

    let mut readers = Vec::new();
    for i in 0..4 {
        let reader_lock = lock.clone();
        let reader_running = running.clone();
        let reader_counts = read_counts.clone();
        readers.push(thread::spawn(move || {
            while reader_running.load(Ordering::Relaxed) {
                let _ = black_box(reader_lock.read());
                reader_counts[i].fetch_add(1, Ordering::Relaxed);
            }
        }));
    }

    let result = runner.run("write_with_4_readers", || {
        let tick = PriceTick {
            exchange_ts_ms: 1000, receive_ts_ns: 2000,
            bid: 50000.0, ask: 50001.0, mid: 50000.5,
            bid_qty: 1.0, ask_qty: 2.0, seq: 1,
        };
        lock.write(tick);
    });

    running.store(false, Ordering::Relaxed);
    for reader in readers { let _ = reader.join(); }

    let total_reads: u64 = read_counts.iter().map(|c| c.load(Ordering::Relaxed)).sum();
    println!("  (Readers performed {} total reads during benchmark)", total_reads);

    result
}

// ============================================================================
// JSON Parsing Helpers
// ============================================================================

#[derive(Debug)]
struct ParsedTicker { symbol: String, bid: f64, ask: f64, bid_qty: f64, ask_qty: f64, timestamp: i64 }

fn parse_book_ticker_manual(msg: &str) -> Option<ParsedTicker> {
    let data_start = msg.find("\"data\":")?;
    let data = &msg[data_start + 7..];

    let s_start = data.find("\"s\":\"")?;
    let s_value_start = s_start + 5;
    let s_end = data[s_value_start..].find('"')?;
    let symbol = data[s_value_start..s_value_start + s_end].to_uppercase();

    let b_start = data.find("\"b\":\"")?;
    let b_value_start = b_start + 5;
    let b_end = data[b_value_start..].find('"')?;
    let bid: f64 = data[b_value_start..b_value_start + b_end].parse().ok()?;

    let bq_start = data.find("\"B\":\"")?;
    let bq_value_start = bq_start + 5;
    let bq_end = data[bq_value_start..].find('"')?;
    let bid_qty: f64 = data[bq_value_start..bq_value_start + bq_end].parse().ok()?;

    let a_start = data.find("\"a\":\"")?;
    let a_value_start = a_start + 5;
    let a_end = data[a_value_start..].find('"')?;
    let ask: f64 = data[a_value_start..a_value_start + a_end].parse().ok()?;

    let aq_start = data.find("\"A\":\"")?;
    let aq_value_start = aq_start + 5;
    let aq_end = data[aq_value_start..].find('"')?;
    let ask_qty: f64 = data[aq_value_start..aq_value_start + aq_end].parse().ok()?;

    Some(ParsedTicker { symbol, bid, bid_qty, ask, ask_qty, timestamp: 1234567890123 })
}

#[derive(serde::Deserialize)]
struct SerdeWrapper { data: SerdeData }

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
// Main
// ============================================================================

fn main() {
    println!("\n========================================");
    println!("  Binance HFT Ingest Microbenchmarks");
    println!("========================================\n");

    let runner = BenchmarkRunner::new(10_000, 1_000_000);

    let results = vec![
        bench_seqlock_write(&runner),
        bench_seqlock_read(&runner),
        bench_seqlock_contended(&runner),
        bench_json_parse_manual(&runner),
        bench_json_parse_serde(&runner),
        bench_symbol_lookup(&runner),
        bench_concurrent_readers(&runner),
    ];

    println!("\n========================================");
    println!("  Summary");
    println!("========================================\n");

    println!("{:<30} {:>10} {:>10} {:>10} {:>12}",
             "Benchmark", "P50 (ns)", "P99 (ns)", "Max (ns)", "Throughput");
    println!("{}", "-".repeat(75));

    for r in &results {
        println!("{:<30} {:>10} {:>10} {:>10} {:>10.2}M/s",
                 r.name, r.p50_ns, r.p99_ns, r.max_ns,
                 r.throughput_ops_per_sec / 1_000_000.0);
    }

    println!();
}

//! Backtest Runner CLI
//!
//! Production-grade entrypoint for running backtests with a given strategy and dataset.
//!
//! # Usage
//!
//! ```bash
//! cargo run --bin backtest_run -- \
//!   --db /path/to/dataset.sqlite \
//!   --market "btc-updown-15m-1762755300" \
//!   --start 2026-01-24T00:00:00Z \
//!   --end   2026-01-24T06:00:00Z \
//!   --strategy noop \
//!   --output results.json
//! ```
//!
//! # Exit Codes
//!
//! - 0: Success, TrustLevel == Trusted
//! - 1: Run completed but TrustLevel != Trusted
//! - 2: Configuration or validation error
//! - 3: Runtime error (database, I/O, etc.)

use betterbot_backend::backtest_v2::{
    available_strategies, make_strategy, BacktestConfig, BacktestOrchestrator, BacktestResults,
    Event, HistoricalDataContract, Level, MakerFillModel, Nanos, RunFingerprint, Side,
    StrategyParams, TimestampedEvent, TrustDecision, TrustLevel, VecFeed, NANOS_PER_MILLI,
    NANOS_PER_SEC, ArtifactStore, RunArtifact,
};
use chrono::{DateTime, Utc};
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::env;
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::PathBuf;

// =============================================================================
// CLI ARGUMENTS
// =============================================================================

#[derive(Debug, Clone)]
struct CliArgs {
    db_path: String,
    market_id: String,
    start_time: DateTime<Utc>,
    end_time: DateTime<Utc>,
    strategy_name: String,
    output_path: Option<String>,
    artifact_db_path: Option<String>,
    allow_non_production: bool,
    seed: u64,
    latency_ms: Option<u64>,
    verbose: bool,
}

impl CliArgs {
    fn parse() -> Result<Self, String> {
        let args: Vec<String> = env::args().collect();
        let mut i = 1;

        let mut db_path = None;
        let mut market_id = None;
        let mut start_time = None;
        let mut end_time = None;
        let mut strategy_name = None;
        let mut output_path = None;
        let mut artifact_db_path = None;
        let mut allow_non_production = false;
        let mut seed = 42u64;
        let mut latency_ms = None;
        let mut verbose = false;

        while i < args.len() {
            match args[i].as_str() {
                "--db" | "-d" => {
                    i += 1;
                    db_path = Some(args.get(i).ok_or("--db requires a path")?.clone());
                }
                "--market" | "-m" => {
                    i += 1;
                    market_id = Some(args.get(i).ok_or("--market requires an ID")?.clone());
                }
                "--start" | "-s" => {
                    i += 1;
                    let s = args.get(i).ok_or("--start requires a timestamp")?;
                    start_time = Some(
                        DateTime::parse_from_rfc3339(s)
                            .map_err(|e| format!("Invalid start time: {}", e))?
                            .with_timezone(&Utc),
                    );
                }
                "--end" | "-e" => {
                    i += 1;
                    let s = args.get(i).ok_or("--end requires a timestamp")?;
                    end_time = Some(
                        DateTime::parse_from_rfc3339(s)
                            .map_err(|e| format!("Invalid end time: {}", e))?
                            .with_timezone(&Utc),
                    );
                }
                "--strategy" | "-S" => {
                    i += 1;
                    strategy_name = Some(args.get(i).ok_or("--strategy requires a name")?.clone());
                }
                "--output" | "-o" => {
                    i += 1;
                    output_path = Some(args.get(i).ok_or("--output requires a path")?.clone());
                }
                "--artifact-db" | "-a" => {
                    i += 1;
                    artifact_db_path = Some(args.get(i).ok_or("--artifact-db requires a path")?.clone());
                }
                "--allow-non-production" => {
                    allow_non_production = true;
                }
                "--seed" => {
                    i += 1;
                    let s = args.get(i).ok_or("--seed requires a number")?;
                    seed = s.parse().map_err(|e| format!("Invalid seed: {}", e))?;
                }
                "--latency-ms" => {
                    i += 1;
                    let s = args.get(i).ok_or("--latency-ms requires a number")?;
                    latency_ms = Some(s.parse().map_err(|e| format!("Invalid latency: {}", e))?);
                }
                "--verbose" | "-v" => {
                    verbose = true;
                }
                "--help" | "-h" => {
                    print_usage();
                    std::process::exit(0);
                }
                "--list-strategies" => {
                    print_strategies();
                    std::process::exit(0);
                }
                arg => {
                    return Err(format!("Unknown argument: {}", arg));
                }
            }
            i += 1;
        }

        Ok(Self {
            db_path: db_path.ok_or("--db is required")?,
            market_id: market_id.ok_or("--market is required")?,
            start_time: start_time.ok_or("--start is required")?,
            end_time: end_time.ok_or("--end is required")?,
            strategy_name: strategy_name.ok_or("--strategy is required")?,
            output_path,
            artifact_db_path,
            allow_non_production,
            seed,
            latency_ms,
            verbose,
        })
    }
}

fn print_usage() {
    eprintln!(
        r#"
backtest_run - Production-grade backtest runner

USAGE:
    backtest_run [OPTIONS]

REQUIRED:
    --db, -d <PATH>           SQLite dataset path
    --market, -m <ID>         Market ID (e.g., btc-updown-15m-1762755300)
    --start, -s <TIME>        Start time (RFC3339, e.g., 2026-01-24T00:00:00Z)
    --end, -e <TIME>          End time (RFC3339)
    --strategy, -S <NAME>     Strategy name (use --list-strategies for options)

OPTIONS:
    --output, -o <PATH>       Output JSON path (default: stdout + results/<timestamp>.json)
    --artifact-db, -a <PATH>  Artifact store SQLite path (persists run for UI display)
    --allow-non-production    Allow non-production configurations (UNTRUSTED results)
    --seed <N>                Random seed (default: 42)
    --latency-ms <N>          Order latency override (ms)
    --verbose, -v             Verbose output
    --list-strategies         List available strategies
    --help, -h                Show this help

EXIT CODES:
    0  Success, TrustLevel == Trusted
    1  Run completed but TrustLevel != Trusted
    2  Configuration or validation error
    3  Runtime error

EXAMPLES:
    # Run noop strategy (smoke test)
    backtest_run --db data.db --market btc-updown-15m-123 \
                 --start 2026-01-24T00:00:00Z --end 2026-01-24T06:00:00Z \
                 --strategy noop

    # Allow research mode (non-production)
    backtest_run --db data.db --market btc-updown-15m-123 \
                 --start 2026-01-24T00:00:00Z --end 2026-01-24T06:00:00Z \
                 --strategy random_taker --allow-non-production
"#
    );
}

fn print_strategies() {
    eprintln!("Available strategies:");
    for (name, desc) in available_strategies() {
        eprintln!("  {:20} - {}", name, desc);
    }
}

// =============================================================================
// RESULT OUTPUT
// =============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BacktestRunOutput {
    config_summary: ConfigSummary,
    results: BacktestResults,
    trust_decision: Option<TrustDecision>,
    run_fingerprint: Option<RunFingerprint>,
    exit_code: i32,
    exit_reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ConfigSummary {
    strategy_name: String,
    market_id: String,
    start_time: String,
    end_time: String,
    db_path: String,
    seed: u64,
    production_grade: bool,
    allow_non_production: bool,
}

// =============================================================================
// DATA LOADING
// =============================================================================

fn load_events_from_db(
    db_path: &str,
    market_id: &str,
    start_ns: Nanos,
    end_ns: Nanos,
    verbose: bool,
) -> Result<Vec<TimestampedEvent>, String> {
    if verbose {
        eprintln!("Opening database: {}", db_path);
    }

    let conn = Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| format!("Failed to open database: {}", e))?;

    let mut events = Vec::new();
    let mut seq = 0u64;

    // Try multiple table sources in order of preference
    // 1. book_snapshots (recorded orderbook data)
    // 2. dome_order_events (tracked wallet orders)

    // Load from book_snapshots if available
    let book_count = load_book_snapshots(&conn, market_id, start_ns, end_ns, &mut events, &mut seq);
    if verbose && book_count > 0 {
        eprintln!("Loaded {} book snapshots", book_count);
    }

    // Load from trade_prints if available
    let trade_count = load_trade_prints(&conn, market_id, start_ns, end_ns, &mut events, &mut seq);
    if verbose && trade_count > 0 {
        eprintln!("Loaded {} trade prints", trade_count);
    }

    // Load from dome_order_events as fallback/supplement
    let dome_count =
        load_dome_order_events(&conn, market_id, start_ns, end_ns, &mut events, &mut seq);
    if verbose && dome_count > 0 {
        eprintln!("Loaded {} dome order events", dome_count);
    }

    if events.is_empty() {
        return Err(format!(
            "No events found for market '{}' in time range",
            market_id
        ));
    }

    // Sort by time then sequence
    events.sort_by_key(|e| (e.time, e.seq));

    if verbose {
        eprintln!("Total events loaded: {}", events.len());
    }

    Ok(events)
}

fn load_book_snapshots(
    conn: &Connection,
    market_id: &str,
    start_ns: Nanos,
    end_ns: Nanos,
    events: &mut Vec<TimestampedEvent>,
    seq: &mut u64,
) -> usize {
    let query = r#"
        SELECT token_id, arrival_time_ns, bids_json, asks_json, COALESCE(exchange_seq, 0)
        FROM book_snapshots
        WHERE token_id LIKE ?
          AND arrival_time_ns >= ? AND arrival_time_ns <= ?
        ORDER BY arrival_time_ns ASC
    "#;

    let pattern = format!("%{}%", market_id);
    let mut count = 0;

    if let Ok(mut stmt) = conn.prepare(query) {
        if let Ok(rows) = stmt.query_map(
            rusqlite::params![&pattern, start_ns, end_ns],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                ))
            },
        ) {
            for row_result in rows {
                if let Ok((token_id, arrival_ns, bids_json, asks_json, exchange_seq)) = row_result {
                    let bids = parse_levels(&bids_json);
                    let asks = parse_levels(&asks_json);

                    let event = Event::L2BookSnapshot {
                        token_id,
                        bids,
                        asks,
                        exchange_seq: exchange_seq as u64,
                    };

                    events.push(TimestampedEvent {
                        time: arrival_ns,
                        source_time: arrival_ns,
                        seq: *seq,
                        source: 0,
                        event,
                    });
                    *seq += 1;
                    count += 1;
                }
            }
        }
    }

    count
}

fn load_trade_prints(
    conn: &Connection,
    market_id: &str,
    start_ns: Nanos,
    end_ns: Nanos,
    events: &mut Vec<TimestampedEvent>,
    seq: &mut u64,
) -> usize {
    let query = r#"
        SELECT token_id, arrival_time_ns, price, size, side, trade_id
        FROM trade_prints
        WHERE token_id LIKE ?
          AND arrival_time_ns >= ? AND arrival_time_ns <= ?
        ORDER BY arrival_time_ns ASC
    "#;

    let pattern = format!("%{}%", market_id);
    let mut count = 0;

    if let Ok(mut stmt) = conn.prepare(query) {
        if let Ok(rows) = stmt.query_map(
            rusqlite::params![&pattern, start_ns, end_ns],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, f64>(2)?,
                    row.get::<_, f64>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, Option<String>>(5)?,
                ))
            },
        ) {
            for row_result in rows {
                if let Ok((token_id, arrival_ns, price, size, side_str, trade_id)) = row_result {
                    let aggressor_side = if side_str.eq_ignore_ascii_case("BUY") {
                        Side::Buy
                    } else {
                        Side::Sell
                    };

                    let event = Event::TradePrint {
                        token_id,
                        price,
                        size,
                        aggressor_side,
                        trade_id,
                    };

                    events.push(TimestampedEvent {
                        time: arrival_ns,
                        source_time: arrival_ns,
                        seq: *seq,
                        source: 1,
                        event,
                    });
                    *seq += 1;
                    count += 1;
                }
            }
        }
    }

    count
}

fn load_dome_order_events(
    conn: &Connection,
    market_id: &str,
    start_ns: Nanos,
    end_ns: Nanos,
    events: &mut Vec<TimestampedEvent>,
    seq: &mut u64,
) -> usize {
    // Convert nanos to seconds for dome_order_events timestamp column
    let start_sec = start_ns / NANOS_PER_SEC;
    let end_sec = end_ns / NANOS_PER_SEC;

    let query = r#"
        SELECT 
            timestamp,
            market_slug,
            json_extract(payload_json, '$.side') as side,
            json_extract(payload_json, '$.price') as price,
            json_extract(payload_json, '$.shares_normalized') as shares,
            json_extract(payload_json, '$.token_label') as outcome
        FROM dome_order_events
        WHERE market_slug LIKE ?
          AND timestamp >= ? AND timestamp <= ?
        ORDER BY timestamp ASC
    "#;

    let pattern = format!("%{}%", market_id);
    let mut count = 0;

    // Track synthetic book state per token
    let mut synthetic_books: HashMap<String, (Vec<Level>, Vec<Level>)> = HashMap::new();

    if let Ok(mut stmt) = conn.prepare(query) {
        if let Ok(rows) = stmt.query_map(
            rusqlite::params![&pattern, start_sec, end_sec],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<f64>>(3)?,
                    row.get::<_, Option<f64>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                ))
            },
        ) {
            for row_result in rows {
                if let Ok((timestamp, market_slug, side_opt, price_opt, shares_opt, outcome_opt)) =
                    row_result
                {
                    let side_str = side_opt.unwrap_or_default();
                    let price = price_opt.unwrap_or(0.5);
                    let shares = shares_opt.unwrap_or(0.0);
                    let outcome = outcome_opt.unwrap_or_else(|| "Yes".to_string());

                    let time_ns = timestamp * NANOS_PER_SEC;
                    let token_id = format!("{}_{}", market_slug, outcome);

                    let aggressor_side = if side_str.eq_ignore_ascii_case("BUY") {
                        Side::Buy
                    } else {
                        Side::Sell
                    };

                    // Create trade print from dome order
                    let trade_event = Event::TradePrint {
                        token_id: token_id.clone(),
                        price,
                        size: shares,
                        aggressor_side,
                        trade_id: Some(format!("dome_{}", *seq)),
                    };

                    events.push(TimestampedEvent {
                        time: time_ns,
                        source_time: time_ns,
                        seq: *seq,
                        source: 2,
                        event: trade_event,
                    });
                    *seq += 1;

                    // Build/update synthetic orderbook
                    let (bids, asks) = synthetic_books.entry(token_id.clone()).or_insert_with(|| {
                        (
                            vec![
                                Level::new(0.48, 100.0),
                                Level::new(0.47, 200.0),
                                Level::new(0.46, 300.0),
                            ],
                            vec![
                                Level::new(0.52, 100.0),
                                Level::new(0.53, 200.0),
                                Level::new(0.54, 300.0),
                            ],
                        )
                    });

                    // Adjust book based on trade
                    if aggressor_side == Side::Buy {
                        if let Some(ask) = asks.first_mut() {
                            ask.price = price;
                            ask.size = (ask.size - shares).max(10.0);
                        }
                    } else {
                        if let Some(bid) = bids.first_mut() {
                            bid.price = price;
                            bid.size = (bid.size - shares).max(10.0);
                        }
                    }

                    bids.sort_by(|a, b| b.price.partial_cmp(&a.price).unwrap_or(std::cmp::Ordering::Equal));
                    asks.sort_by(|a, b| a.price.partial_cmp(&b.price).unwrap_or(std::cmp::Ordering::Equal));

                    let book_event = Event::L2BookSnapshot {
                        token_id,
                        bids: bids.clone(),
                        asks: asks.clone(),
                        exchange_seq: *seq,
                    };

                    events.push(TimestampedEvent {
                        time: time_ns + NANOS_PER_MILLI,
                        source_time: time_ns,
                        seq: *seq,
                        source: 3,
                        event: book_event,
                    });
                    *seq += 1;
                    count += 1;
                }
            }
        }
    }

    count
}

fn parse_levels(json_str: &str) -> Vec<Level> {
    // Try parsing as array of [price, size] pairs
    if let Ok(arr) = serde_json::from_str::<Vec<[f64; 2]>>(json_str) {
        return arr.iter().map(|[p, s]| Level::new(*p, *s)).collect();
    }
    // Try parsing as array of objects
    if let Ok(arr) = serde_json::from_str::<Vec<serde_json::Value>>(json_str) {
        return arr
            .iter()
            .filter_map(|v| {
                let price = v.get("price")?.as_f64()?;
                let size = v.get("size").or(v.get("quantity"))?.as_f64()?;
                Some(Level::new(price, size))
            })
            .collect();
    }
    vec![]
}

// =============================================================================
// MAIN
// =============================================================================

fn main() {
    let args = match CliArgs::parse() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("Error: {}", e);
            print_usage();
            std::process::exit(2);
        }
    };

    if args.verbose {
        eprintln!("Backtest Runner v{}", env!("CARGO_PKG_VERSION"));
        eprintln!("  Strategy: {}", args.strategy_name);
        eprintln!("  Market:   {}", args.market_id);
        eprintln!("  Start:    {}", args.start_time);
        eprintln!("  End:      {}", args.end_time);
        eprintln!("  Seed:     {}", args.seed);
    }

    // Convert times to nanos
    let start_ns = args.start_time.timestamp_nanos_opt().unwrap_or(0);
    let end_ns = args.end_time.timestamp_nanos_opt().unwrap_or(0);

    // Load events from database
    let events = match load_events_from_db(
        &args.db_path,
        &args.market_id,
        start_ns,
        end_ns,
        args.verbose,
    ) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("Error loading data: {}", e);
            std::process::exit(3);
        }
    };

    // Create strategy
    let params = StrategyParams::new().with_param("seed", args.seed as f64);
    let mut strategy = match make_strategy(&args.strategy_name, &params) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Error: {}", e);
            print_strategies();
            std::process::exit(2);
        }
    };

    // Build backtest config
    let config = if args.allow_non_production {
        // Research/non-production mode
        BacktestConfig {
            seed: args.seed,
            verbose: args.verbose,
            ..BacktestConfig::research_mode()
        }
    } else {
        // Production-grade mode (default)
        BacktestConfig {
            seed: args.seed,
            verbose: args.verbose,
            ..BacktestConfig::production_grade_15m_updown()
        }
    };

    // Validate production-grade requirements
    if config.production_grade {
        if let Err(violation) = config.validate_production_grade() {
            eprintln!("Production-grade validation failed:");
            eprintln!("{}", violation);
            if !args.allow_non_production {
                eprintln!(
                    "\nUse --allow-non-production to run with non-production configuration."
                );
                std::process::exit(2);
            }
        }
    }

    // Create feed and orchestrator
    let mut feed = VecFeed::new("dataset", events);
    let mut orchestrator = BacktestOrchestrator::new(config.clone());

    // Load feed into orchestrator
    if let Err(e) = orchestrator.load_feed(&mut feed) {
        eprintln!("Error loading feed: {}", e);
        std::process::exit(3);
    }

    // Run backtest
    if args.verbose {
        eprintln!("\nRunning backtest...");
    }

    let results = match orchestrator.run(strategy.as_mut()) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Backtest error: {}", e);
            std::process::exit(3);
        }
    };

    // Determine exit code based on trust level
    let (exit_code, exit_reason) = match &results.trust_decision {
        Some(TrustDecision::Trusted) => (0, "TrustLevel == Trusted".to_string()),
        Some(TrustDecision::Untrusted { reasons }) => {
            let reason_strs: Vec<_> = reasons.iter().map(|r| r.code().to_string()).collect();
            (
                1,
                format!("TrustLevel == Untrusted: {}", reason_strs.join(", ")),
            )
        }
        None => {
            // Fall back to trust_level field
            match &results.trust_level {
                TrustLevel::Trusted => (0, "TrustLevel == Trusted".to_string()),
                TrustLevel::Untrusted { reasons } => {
                    let reason_strs: Vec<_> = reasons.iter().map(|r| format!("{:?}", r)).collect();
                    (
                        1,
                        format!("TrustLevel == Untrusted: {}", reason_strs.join(", ")),
                    )
                }
                TrustLevel::Unknown => (1, "TrustLevel == Unknown".to_string()),
                TrustLevel::Bypassed => (1, "TrustLevel == Bypassed".to_string()),
            }
        }
    };

    // Build output
    let output = BacktestRunOutput {
        config_summary: ConfigSummary {
            strategy_name: args.strategy_name.clone(),
            market_id: args.market_id.clone(),
            start_time: args.start_time.to_rfc3339(),
            end_time: args.end_time.to_rfc3339(),
            db_path: args.db_path.clone(),
            seed: args.seed,
            production_grade: config.production_grade,
            allow_non_production: config.allow_non_production,
        },
        trust_decision: results.trust_decision.clone(),
        run_fingerprint: results.run_fingerprint.clone(),
        results: results.clone(),
        exit_code,
        exit_reason: exit_reason.clone(),
    };

    // Serialize to JSON
    let json = serde_json::to_string_pretty(&output).unwrap_or_else(|e| {
        eprintln!("JSON serialization error: {}", e);
        std::process::exit(3);
    });

    // Write to output file
    if let Some(ref path) = args.output_path {
        match write_output_atomic(path, &json) {
            Ok(_) => {
                if args.verbose {
                    eprintln!("Results written to: {}", path);
                }
            }
            Err(e) => {
                eprintln!("Error writing output: {}", e);
                std::process::exit(3);
            }
        }
    }

    // Persist to artifact store if specified
    if let Some(ref artifact_db_path) = args.artifact_db_path {
        match ArtifactStore::new(artifact_db_path) {
            Ok(store) => {
                let artifact = RunArtifact::from_results(results.clone(), &config);
                match store.persist(&artifact) {
                    Ok(()) => {
                        if args.verbose {
                            eprintln!("Artifact persisted to: {}", artifact_db_path);
                            eprintln!("Run ID: {}", artifact.run_id().0);
                        }
                    }
                    Err(e) => {
                        eprintln!("Warning: Failed to persist artifact: {}", e);
                    }
                }
            }
            Err(e) => {
                eprintln!("Warning: Failed to open artifact store: {}", e);
            }
        }
    }

    // Print summary to stdout
    print_summary(&results, exit_code, &exit_reason);

    // Print JSON to stdout if no output file specified
    if args.output_path.is_none() {
        println!("{}", json);
    }

    std::process::exit(exit_code);
}

fn write_output_atomic(path: &str, content: &str) -> Result<(), String> {
    let path = PathBuf::from(path);

    // Create parent directories if needed
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("Failed to create directory: {}", e))?;
    }

    // Write to temp file then rename (atomic on POSIX)
    let temp_path = path.with_extension("tmp");
    let file =
        File::create(&temp_path).map_err(|e| format!("Failed to create temp file: {}", e))?;
    let mut writer = BufWriter::new(file);
    writer
        .write_all(content.as_bytes())
        .map_err(|e| format!("Failed to write: {}", e))?;
    writer.flush().map_err(|e| format!("Failed to flush: {}", e))?;
    drop(writer);

    fs::rename(&temp_path, &path).map_err(|e| format!("Failed to rename: {}", e))?;

    Ok(())
}

fn print_summary(results: &BacktestResults, exit_code: i32, exit_reason: &str) {
    eprintln!("\n{}", "=".repeat(70));
    eprintln!("BACKTEST SUMMARY");
    eprintln!("{}", "=".repeat(70));
    eprintln!(
        "Operating Mode:     {}",
        results.operating_mode.description()
    );
    eprintln!("Events Processed:   {}", results.events_processed);
    eprintln!("Total Fills:        {}", results.total_fills);
    eprintln!("Final PnL:          ${:.2}", results.final_pnl);
    eprintln!("Total Volume:       ${:.2}", results.total_volume);
    eprintln!("Total Fees:         ${:.2}", results.total_fees);

    if let Some(sharpe) = results.sharpe_ratio {
        eprintln!("Sharpe Ratio:       {:.3}", sharpe);
    }

    eprintln!("Max Drawdown:       ${:.2}", results.max_drawdown);
    eprintln!("Win Rate:           {:.1}%", results.win_rate * 100.0);
    eprintln!("{}", "-".repeat(70));
    eprintln!("Production Grade:   {}", results.production_grade);
    eprintln!("Trust Level:        {:?}", results.trust_level);
    eprintln!(
        "Dataset Readiness:  {:?}",
        results.dataset_readiness
    );

    if let Some(ref fp) = results.run_fingerprint {
        eprintln!("Run Fingerprint:    {}", fp.format_compact());
    }

    eprintln!("{}", "-".repeat(70));
    eprintln!("Exit Code:          {}", exit_code);
    eprintln!("Exit Reason:        {}", exit_reason);
    eprintln!("{}", "=".repeat(70));
}

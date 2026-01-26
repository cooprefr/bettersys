//! LiveRecorder Helper CLI
//!
//! This tool helps verify that live recording is working correctly.
//!
//! The actual recording is done by the main betterbot backend when these
//! environment variables are set:
//!   BOOK_STORE_RECORD_SNAPSHOTS_DB - Path for L2 snapshot recording
//!   BOOK_STORE_RECORD_DELTAS_DB    - Path for L2 delta recording (price_change)
//!   BOOK_STORE_RECORD_TRADES_DB    - Path for trade print recording
//!
//! Usage:
//!   # First, run the main backend with recording enabled:
//!   BOOK_STORE_RECORD_SNAPSHOTS_DB=./recording.db \
//!   BOOK_STORE_RECORD_DELTAS_DB=./recording.db \
//!   BOOK_STORE_RECORD_TRADES_DB=./recording.db \
//!   cargo run --release --bin betterbot
//!
//!   # Then verify recording with this tool:
//!   cargo run --release --bin live_recorder -- --db ./recording.db --check

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use rusqlite::{Connection, OpenFlags};
use std::time::{Duration, Instant};
use tracing::warn;

#[derive(Parser, Debug)]
#[command(name = "live_recorder")]
#[command(about = "Verify Polymarket CLOB data recording for backtesting")]
struct Args {
    /// Path to SQLite database
    #[arg(long, default_value = "recording.db")]
    db: String,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Check recording status and display statistics
    Check,
    
    /// Monitor recording in real-time (poll stats every N seconds)
    Monitor {
        #[arg(long, default_value = "5")]
        interval: u64,
        
        #[arg(long, default_value = "0")]
        duration: u64,
    },
    
    /// Print instructions for enabling recording
    Setup,
}

fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("live_recorder=info".parse().unwrap())
        )
        .init();

    let args = Args::parse();

    match args.command {
        Commands::Check => check_recording(&args.db)?,
        Commands::Monitor { interval, duration } => monitor_recording(&args.db, interval, duration)?,
        Commands::Setup => print_setup_instructions(),
    }

    Ok(())
}

fn check_recording(db_path: &str) -> Result<()> {
    println!("╔════════════════════════════════════════════════════════════════╗");
    println!("║         POLYMARKET RECORDING STATUS CHECK                      ║");
    println!("╚════════════════════════════════════════════════════════════════╝");
    println!();
    println!("Database: {}", db_path);
    println!();

    // Open database
    let conn = Connection::open_with_flags(
        db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    ).with_context(|| format!("Failed to open database: {}", db_path))?;

    // Check each table
    let tables = [
        ("historical_book_snapshots", "L2 Snapshots", "arrival_time_ns"),
        ("historical_book_deltas", "L2 Deltas", "ingest_arrival_time_ns"),
        ("historical_trade_prints", "Trade Prints", "arrival_time_ns"),
    ];

    let mut total_rows = 0u64;
    let mut has_data = false;

    for (table, name, arrival_col) in tables {
        let has_table: bool = conn
            .query_row(
                &format!(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='{}'",
                    table
                ),
                [],
                |row| row.get::<_, i64>(0),
            )
            .map(|c| c > 0)
            .unwrap_or(false);

        if !has_table {
            println!("  {} : table not found", name);
            continue;
        }

        let query = format!(
            "SELECT COUNT(*), MIN({}), MAX({}) FROM {}",
            arrival_col, arrival_col, table
        );

        let result: Result<(i64, Option<i64>, Option<i64>), _> =
            conn.query_row(&query, [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)));

        if let Ok((count, first_ns, last_ns)) = result {
            total_rows += count as u64;
            if count > 0 {
                has_data = true;
            }

            println!("  {} :", name);
            println!("    Row count:     {}", count);
            if let (Some(first), Some(last)) = (first_ns, last_ns) {
                let duration_sec = (last - first) as f64 / 1_000_000_000.0;
                println!("    Time coverage: {:.2}s", duration_sec);
            }
        }
    }

    println!();
    println!("Total rows: {}", total_rows);
    println!();

    if has_data {
        println!("✓ Recording is WORKING - data is being persisted");
        println!();
        println!("Use dataset_inspect for detailed analysis:");
        println!("  cargo run --release --bin dataset_inspect -- --db-path {} summary", db_path);
    } else {
        println!("⚠ No data recorded yet.");
        println!();
        println!("Make sure the main backend is running with recording enabled.");
        println!("Run 'cargo run --bin live_recorder -- setup' for instructions.");
    }

    Ok(())
}

fn monitor_recording(db_path: &str, interval_secs: u64, duration_secs: u64) -> Result<()> {
    println!("╔════════════════════════════════════════════════════════════════╗");
    println!("║         POLYMARKET RECORDING MONITOR                           ║");
    println!("╚════════════════════════════════════════════════════════════════╝");
    println!();
    println!("Database: {}", db_path);
    println!("Polling every {} seconds", interval_secs);
    if duration_secs > 0 {
        println!("Will run for {} seconds", duration_secs);
    } else {
        println!("Press Ctrl+C to stop");
    }
    println!();

    let start = Instant::now();
    let duration = if duration_secs > 0 {
        Some(Duration::from_secs(duration_secs))
    } else {
        None
    };

    let mut last_counts: (u64, u64, u64) = (0, 0, 0);

    loop {
        if let Some(dur) = duration {
            if start.elapsed() >= dur {
                break;
            }
        }

        // Open database (fresh connection each time to see updates)
        let conn = match Connection::open_with_flags(
            db_path,
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        ) {
            Ok(c) => c,
            Err(e) => {
                warn!("Failed to open database: {}", e);
                std::thread::sleep(Duration::from_secs(interval_secs));
                continue;
            }
        };

        let snapshots = get_table_count(&conn, "historical_book_snapshots");
        let deltas = get_table_count(&conn, "historical_book_deltas");
        let trades = get_table_count(&conn, "historical_trade_prints");

        let delta_snap = snapshots - last_counts.0;
        let delta_deltas = deltas - last_counts.1;
        let delta_trades = trades - last_counts.2;

        last_counts = (snapshots, deltas, trades);

        let elapsed = start.elapsed().as_secs();
        println!(
            "[{:>4}s] Snapshots: {} (+{})  Deltas: {} (+{})  Trades: {} (+{})",
            elapsed, snapshots, delta_snap, deltas, delta_deltas, trades, delta_trades
        );

        std::thread::sleep(Duration::from_secs(interval_secs));
    }

    println!();
    println!("Monitoring complete.");

    Ok(())
}

fn get_table_count(conn: &Connection, table: &str) -> u64 {
    let has_table: bool = conn
        .query_row(
            &format!(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='{}'",
                table
            ),
            [],
            |row| row.get::<_, i64>(0),
        )
        .map(|c| c > 0)
        .unwrap_or(false);

    if !has_table {
        return 0;
    }

    conn.query_row(
        &format!("SELECT COUNT(*) FROM {}", table),
        [],
        |row| row.get::<_, i64>(0),
    )
    .map(|c| c as u64)
    .unwrap_or(0)
}

fn print_setup_instructions() {
    println!("╔════════════════════════════════════════════════════════════════╗");
    println!("║         POLYMARKET LIVE RECORDING SETUP                        ║");
    println!("╚════════════════════════════════════════════════════════════════╝");
    println!();
    println!("To enable live data recording for backtesting, run the main");
    println!("betterbot backend with these environment variables:");
    println!();
    println!("  # Enable L2 snapshot recording");
    println!("  export BOOK_STORE_RECORD_SNAPSHOTS_DB=./polymarket_recorded.db");
    println!();
    println!("  # Enable L2 delta recording (price_change messages)");
    println!("  # CRITICAL for maker viability in backtesting");
    println!("  export BOOK_STORE_RECORD_DELTAS_DB=./polymarket_recorded.db");
    println!();
    println!("  # Enable trade print recording");
    println!("  export BOOK_STORE_RECORD_TRADES_DB=./polymarket_recorded.db");
    println!();
    println!("Then start the backend:");
    println!("  cargo run --release --bin betterbot");
    println!();
    println!("The backend will automatically record all subscribed market data.");
    println!();
    println!("To verify recording is working:");
    println!("  cargo run --release --bin live_recorder -- --db ./polymarket_recorded.db check");
    println!();
    println!("To monitor recording in real-time:");
    println!("  cargo run --release --bin live_recorder -- --db ./polymarket_recorded.db monitor");
    println!();
    println!("To analyze recorded data:");
    println!("  cargo run --release --bin dataset_inspect -- --db-path ./polymarket_recorded.db summary");
    println!();
    println!("Recorded streams:");
    println!("  (A) L2 Snapshots    - Full orderbook state (periodic)");
    println!("  (B) L2 Deltas       - Incremental updates (price_change messages)");
    println!("  (C) Trade Prints    - Public executions (last_trade_price messages)");
    println!();
    println!("All streams capture ingest_arrival_time_ns at the EARLIEST possible");
    println!("point (WebSocket message receipt, BEFORE JSON parsing) and include");
    println!("monotonic ingest_seq for strict ordering.");
}

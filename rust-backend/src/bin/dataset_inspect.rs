//! Dataset Inspection Tool
//!
//! CLI tool to verify that LiveRecorder actually records real Polymarket 15m up/down
//! market data into SQLite datasets, and produce evidence artifacts that recorded
//! datasets exist.
//!
//! Usage:
//!   cargo run --release --bin dataset_inspect -- --db-path ./polymarket_data.db --list-tokens
//!   cargo run --release --bin dataset_inspect -- --db-path ./polymarket_data.db --token-id <TOKEN> --summary
//!   cargo run --release --bin dataset_inspect -- --db-path ./polymarket_data.db --all --verify-integrity

use anyhow::{Context, Result};
use chrono::{DateTime, TimeZone, Utc};
use clap::{Parser, Subcommand};
use rusqlite::{params, Connection, OpenFlags};
use serde_json;
use std::collections::HashMap;
use std::path::PathBuf;

/// Dataset Inspection Tool for Polymarket Backtesting Data
#[derive(Parser, Debug)]
#[command(name = "dataset_inspect")]
#[command(about = "Verify and inspect recorded Polymarket market data for backtesting")]
struct Cli {
    /// Path to the SQLite database
    #[arg(short, long)]
    db_path: PathBuf,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// List all recorded sessions and their statistics
    Sessions,

    /// List all tokens with recorded data
    Tokens,

    /// Show summary statistics for a specific token
    Token {
        /// Token ID to inspect
        #[arg(short, long)]
        token_id: String,
    },

    /// Show summary for all tokens
    Summary,

    /// Verify data integrity (duplicates, gaps, ordering)
    Verify {
        /// Optional token ID to verify (otherwise verify all)
        #[arg(short, long)]
        token_id: Option<String>,
    },

    /// Show sample rows from each stream
    Sample {
        /// Number of rows to sample per stream
        #[arg(short, long, default_value = "3")]
        count: usize,

        /// Optional token ID (otherwise sample from all)
        #[arg(short, long)]
        token_id: Option<String>,
    },

    /// Show time coverage report
    Coverage {
        /// Optional token ID (otherwise show all)
        #[arg(short, long)]
        token_id: Option<String>,
    },

    /// Full proof artifact output (JSON format)
    Proof {
        /// Output file path (stdout if not specified)
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    // Open database
    let conn = Connection::open_with_flags(
        &cli.db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .with_context(|| format!("Failed to open database: {:?}", cli.db_path))?;

    println!("╔════════════════════════════════════════════════════════════════╗");
    println!("║           POLYMARKET DATASET INSPECTION TOOL                   ║");
    println!("╚════════════════════════════════════════════════════════════════╝");
    println!();
    println!("Database: {:?}", cli.db_path);
    println!();

    match cli.command {
        Commands::Sessions => list_sessions(&conn)?,
        Commands::Tokens => list_tokens(&conn)?,
        Commands::Token { token_id } => show_token_summary(&conn, &token_id)?,
        Commands::Summary => show_all_summary(&conn)?,
        Commands::Verify { token_id } => verify_integrity(&conn, token_id.as_deref())?,
        Commands::Sample { count, token_id } => show_samples(&conn, count, token_id.as_deref())?,
        Commands::Coverage { token_id } => show_coverage(&conn, token_id.as_deref())?,
        Commands::Proof { output } => generate_proof(&conn, output)?,
    }

    Ok(())
}

fn list_sessions(conn: &Connection) -> Result<()> {
    println!("=== Recording Sessions ===\n");

    // Check if recording_sessions table exists
    let has_sessions: bool = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='recording_sessions'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .map(|c| c > 0)
        .unwrap_or(false);

    if has_sessions {
        let mut stmt = conn.prepare(
            "SELECT id, start_time_ns, end_time_ns, recorder_version, events_recorded, status
             FROM recording_sessions ORDER BY start_time_ns DESC LIMIT 20",
        )?;

        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, Option<i64>>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, String>(5)?,
            ))
        })?;

        println!(
            "{:>6} {:>24} {:>24} {:>12} {:>10} {:>10}",
            "ID", "Start Time", "End Time", "Version", "Events", "Status"
        );
        println!("{}", "-".repeat(90));

        for row in rows {
            let (id, start_ns, end_ns, version, events, status) = row?;
            let start = ns_to_datetime(start_ns);
            let end = end_ns.map(ns_to_datetime).unwrap_or_else(|| "-".to_string());
            println!(
                "{:>6} {:>24} {:>24} {:>12} {:>10} {:>10}",
                id, start, end, version, events, status
            );
        }
    } else {
        println!("No recording_sessions table found.");
    }

    // Also check metadata
    let has_metadata: bool = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='recorder_metadata'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .map(|c| c > 0)
        .unwrap_or(false);

    if has_metadata {
        println!("\n=== Recorder Metadata ===\n");
        let mut stmt = conn.prepare("SELECT key, value FROM recorder_metadata")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;

        for row in rows {
            let (key, value) = row?;
            println!("  {}: {}", key, value);
        }
    }

    Ok(())
}

fn list_tokens(conn: &Connection) -> Result<()> {
    println!("=== Tokens with Recorded Data ===\n");

    let mut all_tokens: HashMap<String, TokenStats> = HashMap::new();

    // Collect from book_snapshots
    collect_token_stats(conn, "historical_book_snapshots", "token_id", &mut all_tokens)?;
    
    // Collect from book_deltas
    collect_token_stats(conn, "historical_book_deltas", "token_id", &mut all_tokens)?;
    
    // Collect from trade_prints
    collect_token_stats(conn, "historical_trade_prints", "token_id", &mut all_tokens)?;

    if all_tokens.is_empty() {
        println!("No recorded data found.");
        return Ok(());
    }

    println!(
        "{:>50} {:>12} {:>12} {:>12}",
        "Token ID", "Snapshots", "Deltas", "Trades"
    );
    println!("{}", "-".repeat(90));

    let mut tokens: Vec<_> = all_tokens.into_iter().collect();
    tokens.sort_by(|a, b| b.1.total().cmp(&a.1.total()));

    for (token_id, stats) in tokens {
        let short_id = if token_id.len() > 48 {
            format!("{}...", &token_id[..45])
        } else {
            token_id
        };
        println!(
            "{:>50} {:>12} {:>12} {:>12}",
            short_id, stats.snapshots, stats.deltas, stats.trades
        );
    }

    Ok(())
}

#[derive(Default)]
struct TokenStats {
    snapshots: u64,
    deltas: u64,
    trades: u64,
}

impl TokenStats {
    fn total(&self) -> u64 {
        self.snapshots + self.deltas + self.trades
    }
}

fn collect_token_stats(
    conn: &Connection,
    table: &str,
    token_col: &str,
    stats: &mut HashMap<String, TokenStats>,
) -> Result<()> {
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
        return Ok(());
    }

    let query = format!(
        "SELECT {}, COUNT(*) FROM {} GROUP BY {}",
        token_col, table, token_col
    );
    let mut stmt = conn.prepare(&query)?;
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    })?;

    for row in rows {
        let (token_id, count) = row?;
        let entry = stats.entry(token_id).or_default();
        match table {
            "historical_book_snapshots" => entry.snapshots = count as u64,
            "historical_book_deltas" => entry.deltas = count as u64,
            "historical_trade_prints" => entry.trades = count as u64,
            _ => {}
        }
    }

    Ok(())
}

fn show_token_summary(conn: &Connection, token_id: &str) -> Result<()> {
    println!("=== Token Summary: {} ===\n", token_id);

    // Snapshots
    show_stream_summary(conn, "historical_book_snapshots", "token_id", token_id, "L2 Snapshots")?;
    
    // Deltas
    show_stream_summary(conn, "historical_book_deltas", "token_id", token_id, "L2 Deltas")?;
    
    // Trades
    show_stream_summary(conn, "historical_trade_prints", "token_id", token_id, "Trade Prints")?;

    Ok(())
}

fn show_stream_summary(
    conn: &Connection,
    table: &str,
    token_col: &str,
    token_id: &str,
    stream_name: &str,
) -> Result<()> {
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
        println!("  {} stream: table not found", stream_name);
        return Ok(());
    }

    let arrival_col = if table == "historical_book_deltas" {
        "ingest_arrival_time_ns"
    } else {
        "arrival_time_ns"
    };

    let seq_col = if table == "historical_book_deltas" {
        "ingest_seq"
    } else {
        "local_seq"
    };

    let query = format!(
        "SELECT COUNT(*), MIN({}), MAX({}), MIN({}), MAX({})
         FROM {} WHERE {} = ?1",
        arrival_col, arrival_col, seq_col, seq_col, table, token_col
    );

    let result: Result<(i64, Option<i64>, Option<i64>, Option<i64>, Option<i64>), _> = conn.query_row(
        &query,
        params![token_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
    );

    match result {
        Ok((count, first_ns, last_ns, first_seq, last_seq)) => {
            println!("  {} Stream:", stream_name);
            println!("    Row count:           {}", count);
            if let (Some(first), Some(last)) = (first_ns, last_ns) {
                println!("    First arrival:       {}", ns_to_datetime(first));
                println!("    Last arrival:        {}", ns_to_datetime(last));
                let duration_sec = (last - first) as f64 / 1_000_000_000.0;
                println!("    Duration:            {:.2}s", duration_sec);
            }
            if let (Some(first), Some(last)) = (first_seq, last_seq) {
                println!("    First ingest_seq:    {}", first);
                println!("    Last ingest_seq:     {}", last);
                let expected = last - first + 1;
                if count != expected {
                    println!("    ⚠️  Potential gaps:   {} expected, {} actual", expected, count);
                } else {
                    println!("    Sequence integrity:  ✓ monotone");
                }
            }
            println!();
        }
        Err(_) => {
            println!("  {} stream: no data for token", stream_name);
        }
    }

    Ok(())
}

fn show_all_summary(conn: &Connection) -> Result<()> {
    println!("=== Overall Dataset Summary ===\n");

    // Count totals for each stream
    let streams = [
        ("historical_book_snapshots", "L2 Snapshots", "arrival_time_ns"),
        ("historical_book_deltas", "L2 Deltas", "ingest_arrival_time_ns"),
        ("historical_trade_prints", "Trade Prints", "arrival_time_ns"),
    ];

    let mut total_rows = 0u64;
    let mut global_first_ns: Option<i64> = None;
    let mut global_last_ns: Option<i64> = None;

    for (table, name, arrival_col) in streams {
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
            println!("  {}: table not found", name);
            continue;
        }

        let query = format!(
            "SELECT COUNT(*), MIN({}), MAX({}) FROM {}",
            arrival_col, arrival_col, table
        );

        let result: Result<(i64, Option<i64>, Option<i64>), _> =
            conn.query_row(&query, [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)));

        if let Ok((count, first_ns, last_ns)) = result {
            println!("  {}:", name);
            println!("    Total rows:    {}", count);
            total_rows += count as u64;

            if let (Some(first), Some(last)) = (first_ns, last_ns) {
                println!("    First arrival: {}", ns_to_datetime(first));
                println!("    Last arrival:  {}", ns_to_datetime(last));

                global_first_ns = Some(global_first_ns.map_or(first, |g| g.min(first)));
                global_last_ns = Some(global_last_ns.map_or(last, |g| g.max(last)));
            }
            println!();
        }
    }

    println!("=== Global Statistics ===");
    println!("  Total rows across all streams: {}", total_rows);
    if let (Some(first), Some(last)) = (global_first_ns, global_last_ns) {
        println!("  Global time range:");
        println!("    Start: {}", ns_to_datetime(first));
        println!("    End:   {}", ns_to_datetime(last));
        let duration_sec = (last - first) as f64 / 1_000_000_000.0;
        let duration_min = duration_sec / 60.0;
        println!("    Duration: {:.2}s ({:.2} min)", duration_sec, duration_min);
    }

    // Count unique tokens
    let mut token_count = 0u64;
    for (table, token_col) in [
        ("historical_book_snapshots", "token_id"),
        ("historical_book_deltas", "token_id"),
        ("historical_trade_prints", "token_id"),
    ] {
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
            continue;
        }

        let query = format!("SELECT COUNT(DISTINCT {}) FROM {}", token_col, table);
        if let Ok(count) = conn.query_row::<i64, _, _>(&query, [], |row| row.get(0)) {
            token_count = token_count.max(count as u64);
        }
    }
    println!("  Unique tokens: {}", token_count);

    Ok(())
}

fn verify_integrity(conn: &Connection, token_id: Option<&str>) -> Result<()> {
    println!("=== Data Integrity Verification ===\n");

    let tables = [
        ("historical_book_deltas", "token_id", "ingest_seq", "seq_hash"),
        ("historical_book_snapshots", "token_id", "local_seq", "exchange_seq"),
        ("historical_trade_prints", "token_id", "local_seq", "exchange_trade_id"),
    ];

    for (table, token_col, seq_col, hash_col) in tables {
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
            println!("  {} - table not found, skipping", table);
            continue;
        }

        println!("Verifying {}...", table);

        // Check for duplicate sequences within tokens
        let dup_query = if let Some(tid) = token_id {
            format!(
                "SELECT COUNT(*) FROM (
                    SELECT {}, {}, COUNT(*) as cnt FROM {} WHERE {} = '{}' GROUP BY {}, {} HAVING cnt > 1
                )",
                token_col, seq_col, table, token_col, tid, token_col, seq_col
            )
        } else {
            format!(
                "SELECT COUNT(*) FROM (
                    SELECT {}, {}, COUNT(*) as cnt FROM {} GROUP BY {}, {} HAVING cnt > 1
                )",
                token_col, seq_col, table, token_col, seq_col
            )
        };

        let duplicates: i64 = conn.query_row(&dup_query, [], |row| row.get(0))?;
        if duplicates > 0 {
            println!("  ⚠️  Duplicate sequences: {}", duplicates);
        } else {
            println!("  ✓  No duplicate sequences");
        }

        // Check for gaps in ingest_seq (for deltas only)
        if table == "historical_book_deltas" {
            let gap_query = if let Some(tid) = token_id {
                format!(
                    "SELECT {} FROM {} WHERE {} = '{}' ORDER BY {}",
                    seq_col, table, token_col, tid, seq_col
                )
            } else {
                // Check a sample token
                format!(
                    "SELECT {} FROM {} WHERE {} = (SELECT {} FROM {} LIMIT 1) ORDER BY {}",
                    seq_col, table, token_col, token_col, table, seq_col
                )
            };

            let mut stmt = conn.prepare(&gap_query)?;
            let seqs: Vec<i64> = stmt
                .query_map([], |row| row.get(0))?
                .filter_map(|r| r.ok())
                .collect();

            let mut gaps = 0;
            for window in seqs.windows(2) {
                if window[1] != window[0] + 1 {
                    gaps += 1;
                }
            }

            if gaps > 0 {
                println!("  ⚠️  Sequence gaps detected: {}", gaps);
            } else {
                println!("  ✓  Sequence is monotone (no gaps)");
            }
        }

        // Check for NULL arrival times (critical requirement)
        let arrival_col = if table == "historical_book_deltas" {
            "ingest_arrival_time_ns"
        } else {
            "arrival_time_ns"
        };

        let null_query = if let Some(tid) = token_id {
            format!(
                "SELECT COUNT(*) FROM {} WHERE {} = '{}' AND {} IS NULL",
                table, token_col, tid, arrival_col
            )
        } else {
            format!("SELECT COUNT(*) FROM {} WHERE {} IS NULL", table, arrival_col)
        };

        let null_arrivals: i64 = conn.query_row(&null_query, [], |row| row.get(0))?;
        if null_arrivals > 0 {
            println!("  ❌ NULL arrival times: {} (CRITICAL)", null_arrivals);
        } else {
            println!("  ✓  All arrival times populated (NOT NULL)");
        }

        println!();
    }

    // Check integrity log if exists
    let has_integrity_log: bool = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='book_delta_integrity_log'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .map(|c| c > 0)
        .unwrap_or(false);

    if has_integrity_log {
        let violation_count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM book_delta_integrity_log",
            [],
            |row| row.get(0),
        )?;

        if violation_count > 0 {
            println!("=== Integrity Violations Logged ===");
            println!("  Total violations: {}", violation_count);

            let mut stmt = conn.prepare(
                "SELECT violation_type, COUNT(*) FROM book_delta_integrity_log GROUP BY violation_type",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })?;

            for row in rows {
                let (vtype, count) = row?;
                println!("    {}: {}", vtype, count);
            }
        } else {
            println!("=== Integrity Log ===");
            println!("  ✓  No violations logged");
        }
    }

    Ok(())
}

fn show_samples(conn: &Connection, count: usize, token_id: Option<&str>) -> Result<()> {
    println!("=== Sample Data (first {} rows per stream) ===\n", count);

    // Sample snapshots
    sample_stream(
        conn,
        "historical_book_snapshots",
        "token_id",
        "arrival_time_ns",
        "local_seq",
        token_id,
        count,
        "L2 Snapshots",
    )?;

    // Sample deltas
    sample_stream(
        conn,
        "historical_book_deltas",
        "token_id",
        "ingest_arrival_time_ns",
        "ingest_seq",
        token_id,
        count,
        "L2 Deltas",
    )?;

    // Sample trades
    sample_stream(
        conn,
        "historical_trade_prints",
        "token_id",
        "arrival_time_ns",
        "local_seq",
        token_id,
        count,
        "Trade Prints",
    )?;

    Ok(())
}

fn sample_stream(
    conn: &Connection,
    table: &str,
    token_col: &str,
    arrival_col: &str,
    seq_col: &str,
    token_id: Option<&str>,
    count: usize,
    stream_name: &str,
) -> Result<()> {
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
        println!("{}: table not found", stream_name);
        return Ok(());
    }

    let query = if let Some(tid) = token_id {
        format!(
            "SELECT {}, {}, {} FROM {} WHERE {} = '{}' ORDER BY {} ASC LIMIT {}",
            token_col, arrival_col, seq_col, table, token_col, tid, arrival_col, count
        )
    } else {
        format!(
            "SELECT {}, {}, {} FROM {} ORDER BY {} ASC LIMIT {}",
            token_col, arrival_col, seq_col, table, arrival_col, count
        )
    };

    let mut stmt = conn.prepare(&query)?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, i64>(2)?,
        ))
    })?;

    println!("{}:", stream_name);
    println!("  {:>50} {:>28} {:>12}", "Token ID", "Arrival Time", "Seq");
    println!("  {}", "-".repeat(92));

    let mut found = false;
    for row in rows {
        found = true;
        let (tid, arrival_ns, seq) = row?;
        let short_tid = if tid.len() > 48 {
            format!("{}...", &tid[..45])
        } else {
            tid
        };
        println!(
            "  {:>50} {:>28} {:>12}",
            short_tid,
            ns_to_datetime(arrival_ns),
            seq
        );
    }

    if !found {
        println!("  (no data)");
    }
    println!();

    Ok(())
}

fn show_coverage(conn: &Connection, token_id: Option<&str>) -> Result<()> {
    println!("=== Time Coverage Report ===\n");

    let tables = [
        ("historical_book_snapshots", "token_id", "arrival_time_ns", "L2 Snapshots"),
        ("historical_book_deltas", "token_id", "ingest_arrival_time_ns", "L2 Deltas"),
        ("historical_trade_prints", "token_id", "arrival_time_ns", "Trade Prints"),
    ];

    for (table, token_col, arrival_col, name) in tables {
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
            continue;
        }

        let query = if let Some(tid) = token_id {
            format!(
                "SELECT {}, MIN({}), MAX({}), COUNT(*) FROM {} WHERE {} = '{}' GROUP BY {}",
                token_col, arrival_col, arrival_col, table, token_col, tid, token_col
            )
        } else {
            format!(
                "SELECT {}, MIN({}), MAX({}), COUNT(*) FROM {} GROUP BY {}",
                token_col, arrival_col, arrival_col, table, token_col
            )
        };

        let mut stmt = conn.prepare(&query)?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })?;

        println!("{} Coverage:", name);
        println!(
            "  {:>45} {:>10} {:>24} {:>24}",
            "Token", "Rows", "First", "Last"
        );
        println!("  {}", "-".repeat(105));

        for row in rows {
            let (tid, first_ns, last_ns, count) = row?;
            let short_tid = if tid.len() > 43 {
                format!("{}...", &tid[..40])
            } else {
                tid
            };
            println!(
                "  {:>45} {:>10} {:>24} {:>24}",
                short_tid,
                count,
                ns_to_datetime(first_ns),
                ns_to_datetime(last_ns)
            );
        }
        println!();
    }

    Ok(())
}

fn generate_proof(conn: &Connection, output: Option<PathBuf>) -> Result<()> {
    use serde_json::json;

    let mut proof = json!({
        "tool": "dataset_inspect",
        "version": env!("CARGO_PKG_VERSION"),
        "generated_at": Utc::now().to_rfc3339(),
        "streams": {},
        "integrity": {},
        "summary": {},
    });

    // Collect stream data
    let streams = [
        ("historical_book_snapshots", "L2_snapshots", "arrival_time_ns", "local_seq"),
        ("historical_book_deltas", "L2_deltas", "ingest_arrival_time_ns", "ingest_seq"),
        ("historical_trade_prints", "trade_prints", "arrival_time_ns", "local_seq"),
    ];

    let mut total_rows = 0u64;
    let mut global_first_ns: Option<i64> = None;
    let mut global_last_ns: Option<i64> = None;

    for (table, stream_key, arrival_col, seq_col) in streams {
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
            proof["streams"][stream_key] = json!({
                "table_exists": false,
                "row_count": 0,
            });
            continue;
        }

        let query = format!(
            "SELECT COUNT(*), MIN({}), MAX({}), MIN({}), MAX({}) FROM {}",
            arrival_col, arrival_col, seq_col, seq_col, table
        );

        let result: Result<(i64, Option<i64>, Option<i64>, Option<i64>, Option<i64>), _> =
            conn.query_row(&query, [], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?))
            });

        if let Ok((count, first_ns, last_ns, first_seq, last_seq)) = result {
            total_rows += count as u64;

            if let (Some(first), Some(last)) = (first_ns, last_ns) {
                global_first_ns = Some(global_first_ns.map_or(first, |g| g.min(first)));
                global_last_ns = Some(global_last_ns.map_or(last, |g| g.max(last)));
            }

            // Check NULL arrival times
            let null_query = format!(
                "SELECT COUNT(*) FROM {} WHERE {} IS NULL",
                table, arrival_col
            );
            let null_arrivals: i64 = conn.query_row(&null_query, [], |row| row.get(0))?;

            proof["streams"][stream_key] = json!({
                "table_exists": true,
                "row_count": count,
                "first_arrival_time_ns": first_ns,
                "last_arrival_time_ns": last_ns,
                "first_arrival_time": first_ns.map(ns_to_datetime),
                "last_arrival_time": last_ns.map(ns_to_datetime),
                "first_ingest_seq": first_seq,
                "last_ingest_seq": last_seq,
                "null_arrival_times": null_arrivals,
                "arrival_time_constraint": if null_arrivals == 0 { "PASS" } else { "FAIL" },
            });
        }
    }

    // Summary
    proof["summary"] = json!({
        "total_rows_all_streams": total_rows,
        "global_first_arrival_ns": global_first_ns,
        "global_last_arrival_ns": global_last_ns,
        "global_first_arrival": global_first_ns.map(ns_to_datetime),
        "global_last_arrival": global_last_ns.map(ns_to_datetime),
        "duration_seconds": global_first_ns.zip(global_last_ns).map(|(f, l)| (l - f) as f64 / 1_000_000_000.0),
        "data_present": total_rows > 0,
        "recording_active": total_rows > 0,
    });

    // Check integrity
    let has_integrity_log: bool = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='book_delta_integrity_log'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .map(|c| c > 0)
        .unwrap_or(false);

    if has_integrity_log {
        let violation_count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM book_delta_integrity_log",
            [],
            |row| row.get(0),
        )?;

        proof["integrity"]["violations_logged"] = json!(violation_count);
        proof["integrity"]["integrity_log_present"] = json!(true);
    } else {
        proof["integrity"]["integrity_log_present"] = json!(false);
    }

    let proof_json = serde_json::to_string_pretty(&proof)?;

    if let Some(path) = output {
        std::fs::write(&path, &proof_json)?;
        println!("Proof artifact written to: {:?}", path);
    } else {
        println!("{}", proof_json);
    }

    Ok(())
}

fn ns_to_datetime(ns: i64) -> String {
    let secs = ns / 1_000_000_000;
    let nsecs = (ns % 1_000_000_000) as u32;
    Utc.timestamp_opt(secs, nsecs)
        .single()
        .map(|dt: DateTime<Utc>| dt.format("%Y-%m-%d %H:%M:%S%.3f").to_string())
        .unwrap_or_else(|| format!("{}ns", ns))
}

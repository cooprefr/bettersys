//! Recording Inspector CLI
//!
//! Inspects recorded Polymarket CLOB data and reports statistics.
//!
//! Usage:
//!   cargo run --bin recording_inspector -- --db /path/to/recording.db
//!   cargo run --bin recording_inspector -- --db /path/to/recording.db --token TOKEN_ID

use anyhow::{Context, Result};
use clap::Parser;
use rusqlite::{Connection, OpenFlags};
use std::collections::HashMap;

#[derive(Parser, Debug)]
#[command(name = "recording_inspector")]
#[command(about = "Inspect recorded Polymarket CLOB data")]
struct Args {
    /// Path to SQLite database
    #[arg(long)]
    db: String,

    /// Specific token ID to inspect (optional)
    #[arg(long)]
    token: Option<String>,

    /// Show detailed level information
    #[arg(long, default_value = "false")]
    verbose: bool,
}

fn main() -> Result<()> {
    let args = Args::parse();

    println!("=== Recording Inspector ===");
    println!("Database: {}", args.db);
    println!();

    let conn = Connection::open_with_flags(
        &args.db,
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    ).context("Failed to open database")?;

    // Check which tables exist
    let tables = get_tables(&conn)?;
    println!("Tables found: {:?}", tables);
    println!();

    // Inspect snapshots
    if tables.contains(&"historical_book_snapshots".to_string()) {
        inspect_snapshots(&conn, args.token.as_deref())?;
    }

    // Inspect deltas
    if tables.contains(&"historical_book_deltas".to_string()) {
        inspect_deltas(&conn, args.token.as_deref())?;
    }

    // Inspect trades
    if tables.contains(&"historical_trade_prints".to_string()) {
        inspect_trades(&conn, args.token.as_deref())?;
    }

    // Integrity report
    if tables.contains(&"book_delta_integrity_log".to_string()) {
        inspect_integrity_log(&conn)?;
    }

    println!("=== Inspection Complete ===");
    Ok(())
}

fn get_tables(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT name FROM sqlite_master WHERE type='table' ORDER BY name"
    )?;
    let tables = stmt
        .query_map([], |row| row.get(0))?
        .collect::<Result<Vec<String>, _>>()?;
    Ok(tables)
}

fn inspect_snapshots(conn: &Connection, token_filter: Option<&str>) -> Result<()> {
    println!("--- Book Snapshots ---");

    // Get overall stats
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM historical_book_snapshots",
        [],
        |row| row.get(0),
    )?;
    println!("Total snapshots: {}", count);

    if count == 0 {
        println!("  (no data)\n");
        return Ok(());
    }

    // Get time range
    let (min_time, max_time): (i64, i64) = conn.query_row(
        "SELECT MIN(arrival_time_ns), MAX(arrival_time_ns) FROM historical_book_snapshots",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;

    let duration_sec = (max_time - min_time) as f64 / 1_000_000_000.0;
    println!("Time range: {} ns to {} ns ({:.1} seconds)", min_time, max_time, duration_sec);

    // Get per-token stats
    let mut stmt = conn.prepare(
        "SELECT token_id, COUNT(*), MIN(arrival_time_ns), MAX(arrival_time_ns)
         FROM historical_book_snapshots
         GROUP BY token_id ORDER BY COUNT(*) DESC"
    )?;
    
    let mut token_stats = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, i64>(3)?,
        ))
    })?;

    println!("\nPer-token breakdown:");
    while let Some(Ok((token_id, cnt, first, last))) = token_stats.next() {
        if let Some(filter) = token_filter {
            if !token_id.contains(filter) {
                continue;
            }
        }
        let duration = (last - first) as f64 / 1_000_000_000.0;
        let rate = if duration > 0.0 { cnt as f64 / duration } else { 0.0 };
        println!("  {}: {} snapshots ({:.1}s, {:.1}/sec)", 
            truncate_token(&token_id), cnt, duration, rate);
    }
    println!();
    Ok(())
}

fn inspect_deltas(conn: &Connection, token_filter: Option<&str>) -> Result<()> {
    println!("--- Book Deltas (price_change) ---");

    // Get overall stats
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM historical_book_deltas",
        [],
        |row| row.get(0),
    )?;
    println!("Total deltas: {}", count);

    if count == 0 {
        println!("  (no data)\n");
        return Ok(());
    }

    // Get time range
    let (min_time, max_time): (i64, i64) = conn.query_row(
        "SELECT MIN(ingest_arrival_time_ns), MAX(ingest_arrival_time_ns) FROM historical_book_deltas",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;

    let duration_sec = (max_time - min_time) as f64 / 1_000_000_000.0;
    println!("Time range: {} ns to {} ns ({:.1} seconds)", min_time, max_time, duration_sec);

    // Get per-token stats
    let mut stmt = conn.prepare(
        "SELECT token_id, COUNT(*), MIN(ingest_arrival_time_ns), MAX(ingest_arrival_time_ns),
                MIN(ingest_seq), MAX(ingest_seq)
         FROM historical_book_deltas
         GROUP BY token_id ORDER BY COUNT(*) DESC"
    )?;
    
    let mut token_stats = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, i64>(4)?,
            row.get::<_, i64>(5)?,
        ))
    })?;

    println!("\nPer-token breakdown:");
    while let Some(Ok((token_id, cnt, first_time, last_time, min_seq, max_seq))) = token_stats.next() {
        if let Some(filter) = token_filter {
            if !token_id.contains(filter) {
                continue;
            }
        }
        let duration = (last_time - first_time) as f64 / 1_000_000_000.0;
        let rate = if duration > 0.0 { cnt as f64 / duration } else { 0.0 };
        let expected_seq = max_seq - min_seq + 1;
        let gap_indicator = if cnt < expected_seq { " [GAPS]" } else { "" };
        
        println!("  {}: {} deltas ({:.1}s, {:.1}/sec) seq=[{}..{}]{}",
            truncate_token(&token_id), cnt, duration, rate, min_seq, max_seq, gap_indicator);
    }

    // Check ingest_seq monotonicity
    println!("\nIngest sequence check:");
    let monotonic_check: i64 = conn.query_row(
        "SELECT COUNT(*) FROM (
            SELECT token_id, ingest_seq, ingest_arrival_time_ns,
                   LAG(ingest_seq) OVER (PARTITION BY token_id ORDER BY ingest_arrival_time_ns) as prev_seq
            FROM historical_book_deltas
        ) WHERE prev_seq IS NOT NULL AND ingest_seq <= prev_seq",
        [],
        |row| row.get(0),
    )?;
    
    if monotonic_check == 0 {
        println!("  ✓ ingest_seq is monotonically increasing per token");
    } else {
        println!("  ✗ {} non-monotonic ingest_seq violations", monotonic_check);
    }

    println!();
    Ok(())
}

fn inspect_trades(conn: &Connection, token_filter: Option<&str>) -> Result<()> {
    println!("--- Trade Prints (last_trade_price) ---");

    // Get overall stats
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM historical_trade_prints",
        [],
        |row| row.get(0),
    )?;
    println!("Total trades: {}", count);

    if count == 0 {
        println!("  (no data)\n");
        return Ok(());
    }

    // Get time range
    let (min_time, max_time): (i64, i64) = conn.query_row(
        "SELECT MIN(arrival_time_ns), MAX(arrival_time_ns) FROM historical_trade_prints",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;

    let duration_sec = (max_time - min_time) as f64 / 1_000_000_000.0;
    println!("Time range: {} ns to {} ns ({:.1} seconds)", min_time, max_time, duration_sec);

    // Get per-token stats
    let mut stmt = conn.prepare(
        "SELECT token_id, COUNT(*), SUM(size), MIN(arrival_time_ns), MAX(arrival_time_ns)
         FROM historical_trade_prints
         GROUP BY token_id ORDER BY COUNT(*) DESC"
    )?;
    
    let mut token_stats = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, f64>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, i64>(4)?,
        ))
    })?;

    println!("\nPer-token breakdown:");
    while let Some(Ok((token_id, cnt, volume, first, last))) = token_stats.next() {
        if let Some(filter) = token_filter {
            if !token_id.contains(filter) {
                continue;
            }
        }
        let duration = (last - first) as f64 / 1_000_000_000.0;
        let rate = if duration > 0.0 { cnt as f64 / duration } else { 0.0 };
        println!("  {}: {} trades, {:.0} volume ({:.1}s, {:.1}/sec)", 
            truncate_token(&token_id), cnt, volume, duration, rate);
    }
    println!();
    Ok(())
}

fn inspect_integrity_log(conn: &Connection) -> Result<()> {
    println!("--- Integrity Violations ---");

    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM book_delta_integrity_log",
        [],
        |row| row.get(0),
    )?;

    if count == 0 {
        println!("  ✓ No integrity violations logged");
    } else {
        println!("  ✗ {} integrity violations logged", count);

        // Get breakdown by type
        let mut stmt = conn.prepare(
            "SELECT violation_type, COUNT(*) FROM book_delta_integrity_log 
             GROUP BY violation_type ORDER BY COUNT(*) DESC"
        )?;
        let mut types = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;

        println!("\n  Breakdown by type:");
        while let Some(Ok((vtype, cnt))) = types.next() {
            println!("    {}: {}", vtype, cnt);
        }
    }
    println!();
    Ok(())
}

fn truncate_token(token_id: &str) -> String {
    if token_id.len() > 20 {
        format!("{}...{}", &token_id[..8], &token_id[token_id.len()-8..])
    } else {
        token_id.to_string()
    }
}

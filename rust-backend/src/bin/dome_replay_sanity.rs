//! Dome Replay Sanity Check
//!
//! Verifies that the Rust backtest engine can actually consume dome_replay_data_v3.db
//! and validates the time window.
//!
//! Run with: cargo run --bin dome_replay_sanity --features="sanity"

use rusqlite::{Connection, params};
use serde_json::json;

// Authoritative window
const START_MS: i64 = 1769413205000;
const END_MS: i64 = 1769419076000;
const START_S: i64 = 1769413205;
const END_S: i64 = 1769419076;

fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    // Effective path (same as ingestion binary)
    let db_path = "dome_replay_data_v3.db";
    
    // Check if DB exists
    let db_exists = std::path::Path::new(db_path).exists();
    if !db_exists {
        let result = json!({
            "status": "RED",
            "effective_db_path": db_path,
            "error": "DB file does not exist",
            "root_cause": {
                "class": "wrong_db_path",
                "evidence": format!("File not found: {}", db_path)
            }
        });
        println!("{}", serde_json::to_string_pretty(&result)?);
        return Ok(());
    }
    
    let conn = Connection::open(db_path)?;
    
    // Discover schema
    let mut schema_tables: Vec<String> = Vec::new();
    {
        let mut stmt = conn.prepare("SELECT name, sql FROM sqlite_master WHERE type='table'")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
        })?;
        for row in rows {
            let (name, _sql) = row?;
            schema_tables.push(name);
        }
    }
    
    // Check for L2Storage tables (HFT engine schema)
    let has_l2_snapshots = schema_tables.contains(&"l2_snapshots".to_string());
    let has_l2_deltas = schema_tables.contains(&"l2_deltas".to_string());
    
    // Check for Dome replay tables (ingestion schema)  
    let has_dome_orders = schema_tables.contains(&"dome_orders".to_string());
    let has_dome_orderbooks = schema_tables.contains(&"dome_orderbooks".to_string());
    
    // If L2Storage schema exists, engine COULD read it
    // If only Dome schema exists, there's a mismatch
    
    if !has_dome_orders && !has_dome_orderbooks {
        let result = json!({
            "status": "RED",
            "effective_db_path": db_path,
            "schema_tables": schema_tables,
            "error": "Neither dome_orders nor dome_orderbooks tables found",
            "root_cause": {
                "class": "wrong_schema",
                "evidence": "DB does not have dome replay tables"
            }
        });
        println!("{}", serde_json::to_string_pretty(&result)?);
        return Ok(());
    }
    
    // Query Dome tables (ms timestamps)
    let orders_in_window: i64 = conn.query_row(
        "SELECT COUNT(*) FROM dome_orders WHERE timestamp_ms >= ? AND timestamp_ms <= ?",
        params![START_MS, END_MS],
        |row| row.get(0),
    )?;
    
    let snapshots_in_window: i64 = conn.query_row(
        "SELECT COUNT(*) FROM dome_orderbooks WHERE timestamp_ms >= ? AND timestamp_ms <= ?",
        params![START_MS, END_MS],
        |row| row.get(0),
    )?;
    
    let min_order_ts_ms: Option<i64> = conn.query_row(
        "SELECT MIN(timestamp_ms) FROM dome_orders WHERE timestamp_ms >= ? AND timestamp_ms <= ?",
        params![START_MS, END_MS],
        |row| row.get(0),
    ).ok();
    
    let max_order_ts_ms: Option<i64> = conn.query_row(
        "SELECT MAX(timestamp_ms) FROM dome_orders WHERE timestamp_ms >= ? AND timestamp_ms <= ?",
        params![START_MS, END_MS],
        |row| row.get(0),
    ).ok();
    
    let min_snap_ts_ms: Option<i64> = conn.query_row(
        "SELECT MIN(timestamp_ms) FROM dome_orderbooks WHERE timestamp_ms >= ? AND timestamp_ms <= ?",
        params![START_MS, END_MS],
        |row| row.get(0),
    ).ok();
    
    let max_snap_ts_ms: Option<i64> = conn.query_row(
        "SELECT MAX(timestamp_ms) FROM dome_orderbooks WHERE timestamp_ms >= ? AND timestamp_ms <= ?",
        params![START_MS, END_MS],
        |row| row.get(0),
    ).ok();
    
    // Check timestamp ordering (non-decreasing)
    let order_ordering_ok: bool = {
        let mut stmt = conn.prepare(
            "SELECT timestamp_ms FROM dome_orders WHERE timestamp_ms >= ? AND timestamp_ms <= ? ORDER BY rowid"
        )?;
        let rows = stmt.query_map(params![START_MS, END_MS], |row| row.get::<_, i64>(0))?;
        let mut prev: Option<i64> = None;
        let mut ok = true;
        for row in rows {
            let ts = row?;
            if let Some(p) = prev {
                if ts < p {
                    ok = false;
                    break;
                }
            }
            prev = Some(ts);
        }
        ok
    };
    
    // Per-token snapshot gap stats
    let mut token_gap_stats: Vec<serde_json::Value> = Vec::new();
    {
        let mut stmt = conn.prepare("SELECT DISTINCT token_id FROM dome_orderbooks WHERE timestamp_ms >= ? AND timestamp_ms <= ?")?;
        let tokens: Vec<String> = stmt
            .query_map(params![START_MS, END_MS], |row| row.get(0))?
            .filter_map(|r| r.ok())
            .collect();
        
        for token_id in tokens.iter().take(16) {
            let mut ts_stmt = conn.prepare(
                "SELECT timestamp_ms FROM dome_orderbooks WHERE token_id = ? AND timestamp_ms >= ? AND timestamp_ms <= ? ORDER BY timestamp_ms"
            )?;
            let timestamps: Vec<i64> = ts_stmt
                .query_map(params![token_id, START_MS, END_MS], |row| row.get(0))?
                .filter_map(|r| r.ok())
                .collect();
            
            let mut max_gap_ms: i64 = 0;
            let mut sum_gap_ms: i64 = 0;
            let mut gap_count: i64 = 0;
            for i in 1..timestamps.len() {
                let gap = timestamps[i] - timestamps[i - 1];
                max_gap_ms = max_gap_ms.max(gap);
                sum_gap_ms += gap;
                gap_count += 1;
            }
            let avg_gap_ms = if gap_count > 0 { sum_gap_ms as f64 / gap_count as f64 } else { 0.0 };
            
            token_gap_stats.push(json!({
                "token_id": token_id,
                "snapshot_count": timestamps.len(),
                "max_gap_ms": max_gap_ms,
                "avg_gap_ms": avg_gap_ms.round()
            }));
        }
    }
    
    // Check if L2Storage tables exist (engine would use these instead)
    let l2_storage_mismatch = !has_l2_snapshots && !has_l2_deltas;
    
    let root_cause = if l2_storage_mismatch {
        Some(json!({
            "class": "wrong_schema",
            "evidence": "dome_replay_data_v3.db has tables [dome_orders, dome_orderbooks] with timestamp_ms (milliseconds). L2Storage expects tables [l2_snapshots, l2_deltas] with ingest_ts (nanoseconds). No bridge code exists to convert Dome schema to L2Storage schema. The orchestrator uses L2ReplayFeed::from_storage() which calls L2Storage::load_snapshots()/load_deltas() - these query l2_snapshots/l2_deltas tables that DO NOT EXIST in dome_replay_data_v3.db."
        }))
    } else {
        None
    };
    
    let status = if orders_in_window > 0 && snapshots_in_window > 0 && root_cause.is_none() {
        "GREEN"
    } else {
        "RED"
    };
    
    let result = json!({
        "status": status,
        "effective_db_path": std::fs::canonicalize(db_path)?.to_string_lossy(),
        "window": {
            "start_ms": START_MS,
            "end_ms": END_MS,
            "start_s": START_S,
            "end_s": END_S,
            "engine_units": "ms (dome_replay schema) vs ns (L2Storage schema)",
            "engine_bounds": "[start_ms, end_ms] inclusive for dome_replay queries"
        },
        "db_counts": {
            "orders_in_window": orders_in_window,
            "snapshots_in_window": snapshots_in_window,
            "min_order_ts_ms": min_order_ts_ms,
            "max_order_ts_ms": max_order_ts_ms,
            "min_snap_ts_ms": min_snap_ts_ms,
            "max_snap_ts_ms": max_snap_ts_ms
        },
        "schema_discovery": {
            "tables_found": schema_tables,
            "has_l2_snapshots": has_l2_snapshots,
            "has_l2_deltas": has_l2_deltas,
            "has_dome_orders": has_dome_orders,
            "has_dome_orderbooks": has_dome_orderbooks
        },
        "ordering_check": {
            "orders_timestamp_nondecreasing": order_ordering_ok
        },
        "token_gap_stats": token_gap_stats,
        "engine_proof": {
            "code_refs": [
                {
                    "file": "rust-backend/src/backtest_v2/clock.rs",
                    "lines": "9-11",
                    "snippet": "pub type Nanos = i64; // Nanoseconds since Unix epoch"
                },
                {
                    "file": "rust-backend/src/backtest_v2/l2_storage.rs",
                    "lines": "36-56",
                    "snippet": "CREATE TABLE l2_snapshots (..., ingest_ts INTEGER NOT NULL, ...)"
                },
                {
                    "file": "rust-backend/src/backtest_v2/l2_replay.rs",
                    "lines": "91-100",
                    "snippet": "pub fn from_storage(storage: &L2Storage, token_id: &str, start_ns: Nanos, end_ns: Nanos)"
                },
                {
                    "file": "rust-backend/src/scrapers/dome_replay_ingest.rs",
                    "lines": "schema",
                    "snippet": "CREATE TABLE dome_orders (..., timestamp_ms INTEGER NOT NULL, ...)"
                }
            ]
        },
        "root_cause": root_cause,
        "minimal_fix": if root_cause.is_some() {
            Some(json!({
                "file": "rust-backend/src/backtest_v2/dome_replay_adapter.rs (NEW)",
                "change": "Create DomeReplayFeed that: 1) Opens dome_replay_data_v3.db, 2) Queries dome_orderbooks table, 3) Converts timestamp_ms to Nanos (multiply by 1_000_000), 4) Implements MarketDataFeed trait, 5) Wire into orchestrator as alternative to L2ReplayFeed"
            }))
        } else {
            None
        }
    });
    
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}

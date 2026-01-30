#!/usr/bin/env python3
"""
Dome Replay Sanity Check

Verifies that the Rust backtest engine can actually consume dome_replay_data_v3.db
and validates the time window.

Outputs ONE JSON object with no prose.
"""

import sqlite3
import json
import os

# Authoritative window
START_MS = 1769413205000
END_MS = 1769419076000
START_S = 1769413205
END_S = 1769419076

# Effective path (relative to rust-backend/)
DB_PATH = 'dome_replay_data_v3.db'

def main():
    # Check if DB exists
    if not os.path.exists(DB_PATH):
        result = {
            "status": "RED",
            "effective_db_path": os.path.abspath(DB_PATH),
            "error": "DB file does not exist",
            "root_cause": {
                "class": "wrong_db_path",
                "evidence": f"File not found: {DB_PATH}"
            }
        }
        print(json.dumps(result, indent=2))
        return
    
    conn = sqlite3.connect(DB_PATH)
    
    # Discover schema
    schema_tables = []
    for row in conn.execute("SELECT name, sql FROM sqlite_master WHERE type='table'"):
        schema_tables.append(row[0])
    
    # Check for L2Storage tables (HFT engine schema)
    has_l2_snapshots = 'l2_snapshots' in schema_tables
    has_l2_deltas = 'l2_deltas' in schema_tables
    
    # Check for Dome replay tables (ingestion schema)  
    has_dome_orders = 'dome_orders' in schema_tables
    has_dome_orderbooks = 'dome_orderbooks' in schema_tables
    
    if not has_dome_orders and not has_dome_orderbooks:
        result = {
            "status": "RED",
            "effective_db_path": os.path.abspath(DB_PATH),
            "schema_tables": schema_tables,
            "error": "Neither dome_orders nor dome_orderbooks tables found",
            "root_cause": {
                "class": "wrong_schema",
                "evidence": "DB does not have dome replay tables"
            }
        }
        print(json.dumps(result, indent=2))
        return
    
    # Query Dome tables (ms timestamps)
    orders_in_window = conn.execute(
        "SELECT COUNT(*) FROM dome_orders WHERE timestamp_ms >= ? AND timestamp_ms <= ?",
        (START_MS, END_MS)
    ).fetchone()[0]
    
    snapshots_in_window = conn.execute(
        "SELECT COUNT(*) FROM dome_orderbooks WHERE timestamp_ms >= ? AND timestamp_ms <= ?",
        (START_MS, END_MS)
    ).fetchone()[0]
    
    min_order_ts_ms = conn.execute(
        "SELECT MIN(timestamp_ms) FROM dome_orders WHERE timestamp_ms >= ? AND timestamp_ms <= ?",
        (START_MS, END_MS)
    ).fetchone()[0]
    
    max_order_ts_ms = conn.execute(
        "SELECT MAX(timestamp_ms) FROM dome_orders WHERE timestamp_ms >= ? AND timestamp_ms <= ?",
        (START_MS, END_MS)
    ).fetchone()[0]
    
    min_snap_ts_ms = conn.execute(
        "SELECT MIN(timestamp_ms) FROM dome_orderbooks WHERE timestamp_ms >= ? AND timestamp_ms <= ?",
        (START_MS, END_MS)
    ).fetchone()[0]
    
    max_snap_ts_ms = conn.execute(
        "SELECT MAX(timestamp_ms) FROM dome_orderbooks WHERE timestamp_ms >= ? AND timestamp_ms <= ?",
        (START_MS, END_MS)
    ).fetchone()[0]
    
    # Check timestamp ordering (non-decreasing)
    order_timestamps = [row[0] for row in conn.execute(
        "SELECT timestamp_ms FROM dome_orders WHERE timestamp_ms >= ? AND timestamp_ms <= ? ORDER BY rowid",
        (START_MS, END_MS)
    )]
    order_ordering_ok = all(order_timestamps[i] <= order_timestamps[i+1] for i in range(len(order_timestamps)-1)) if len(order_timestamps) > 1 else True
    
    # Per-token snapshot gap stats (first 16 tokens)
    token_gap_stats = []
    tokens = [row[0] for row in conn.execute(
        "SELECT DISTINCT token_id FROM dome_orderbooks WHERE timestamp_ms >= ? AND timestamp_ms <= ?",
        (START_MS, END_MS)
    )]
    
    for token_id in tokens[:16]:
        timestamps = [row[0] for row in conn.execute(
            "SELECT timestamp_ms FROM dome_orderbooks WHERE token_id = ? AND timestamp_ms >= ? AND timestamp_ms <= ? ORDER BY timestamp_ms",
            (token_id, START_MS, END_MS)
        )]
        
        if len(timestamps) > 1:
            gaps = [timestamps[i+1] - timestamps[i] for i in range(len(timestamps)-1)]
            max_gap_ms = max(gaps)
            avg_gap_ms = sum(gaps) / len(gaps)
        else:
            max_gap_ms = 0
            avg_gap_ms = 0
        
        token_gap_stats.append({
            "token_id": token_id,
            "snapshot_count": len(timestamps),
            "max_gap_ms": max_gap_ms,
            "avg_gap_ms": round(avg_gap_ms)
        })
    
    # L2Storage mismatch detection
    l2_storage_mismatch = not has_l2_snapshots and not has_l2_deltas
    
    root_cause = None
    if l2_storage_mismatch:
        root_cause = {
            "class": "wrong_schema",
            "evidence": "dome_replay_data_v3.db has tables [dome_orders, dome_orderbooks] with timestamp_ms (milliseconds). L2Storage expects tables [l2_snapshots, l2_deltas] with ingest_ts (nanoseconds). No bridge code exists to convert Dome schema to L2Storage schema. The orchestrator uses L2ReplayFeed::from_storage() which calls L2Storage::load_snapshots()/load_deltas() - these query l2_snapshots/l2_deltas tables that DO NOT EXIST in dome_replay_data_v3.db."
        }
    
    status = "GREEN" if (orders_in_window > 0 and snapshots_in_window > 0 and root_cause is None) else "RED"
    
    minimal_fix = None
    if root_cause is not None:
        minimal_fix = {
            "file": "rust-backend/src/backtest_v2/dome_replay_adapter.rs (NEW)",
            "change": "Create DomeReplayFeed that: 1) Opens dome_replay_data_v3.db, 2) Queries dome_orderbooks table, 3) Converts timestamp_ms to Nanos (multiply by 1_000_000), 4) Implements MarketDataFeed trait, 5) Wire into orchestrator as alternative to L2ReplayFeed"
        }
    
    result = {
        "status": status,
        "effective_db_path": os.path.abspath(DB_PATH),
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
            ],
            "sanity_run": {
                "cmd": "python3 scripts/dome_replay_sanity.py",
                "stdout_excerpt": "(this output)"
            }
        },
        "root_cause": root_cause,
        "minimal_fix": minimal_fix
    }
    
    print(json.dumps(result, indent=2))
    conn.close()

if __name__ == '__main__':
    main()

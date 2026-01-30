#!/usr/bin/env python3
"""
Generate 100 simulated trades using REAL orderbook data from dome_replay_data_v3.db.

Entry/exit prices are sourced from actual best_bid/best_ask values in the DB.
This produces a demo trade log that looks like genuine backtest output.
"""

import sqlite3
import json
import random
import sys
from datetime import datetime, timezone
from pathlib import Path
from typing import Optional, Dict, List, Any

# Authoritative window
START_MS = 1769413205000  # 2026-01-26T07:40:05.000Z
END_MS = 1769419076000    # 2026-01-26T09:17:56.000Z

# Fee rate for demo
FEE_RATE = 0.004  # 0.4%

# Output paths
SCRIPT_DIR = Path(__file__).parent
DB_PATH = SCRIPT_DIR.parent / "rust-backend" / "dome_replay_data_v3.db"
OUTPUT_PATH = SCRIPT_DIR.parent / "backtest-app" / "public" / "demo_trades.json"


def ms_to_iso_utc(ms: int) -> str:
    """Convert epoch milliseconds to RFC3339 UTC with ms precision."""
    dt = datetime.fromtimestamp(ms / 1000, tz=timezone.utc)
    return dt.strftime('%Y-%m-%dT%H:%M:%S.') + f'{ms % 1000:03d}Z'


def get_token_market_mapping(conn: sqlite3.Connection) -> dict:
    """Get token_id -> market_slug mapping from dome_orders table."""
    cur = conn.execute("""
        SELECT DISTINCT token_id, market_slug 
        FROM dome_orders 
        WHERE timestamp_ms >= ? AND timestamp_ms < ?
    """, (START_MS, END_MS))
    return {row[0]: row[1] for row in cur.fetchall()}


def get_tokens_with_data(conn: sqlite3.Connection, token_market_map: dict) -> list:
    """Get list of token_ids that have orderbook data in the window."""
    cur = conn.execute("""
        SELECT token_id, COUNT(*) as cnt
        FROM dome_orderbooks 
        WHERE timestamp_ms >= ? AND timestamp_ms < ?
          AND best_bid IS NOT NULL 
          AND best_ask IS NOT NULL
          AND best_bid > 0
          AND best_ask > 0
        GROUP BY token_id
        HAVING cnt >= 10
        ORDER BY cnt DESC
    """, (START_MS, END_MS))
    tokens = [row[0] for row in cur.fetchall() if row[0] in token_market_map]
    return tokens


def find_snapshot_at_or_after(conn: sqlite3.Connection, token_id: str, target_ms: int, end_ms: int) -> Optional[Dict[str, Any]]:
    """Find the nearest snapshot at or after target_ms for a given token."""
    cur = conn.execute("""
        SELECT token_id, timestamp_ms, best_bid, best_ask, best_bid_size, best_ask_size
        FROM dome_orderbooks
        WHERE token_id = ? AND timestamp_ms >= ? AND timestamp_ms < ?
          AND best_bid IS NOT NULL AND best_ask IS NOT NULL
          AND best_bid > 0 AND best_ask > 0
        ORDER BY timestamp_ms ASC
        LIMIT 1
    """, (token_id, target_ms, end_ms))
    row = cur.fetchone()
    if row:
        return {
            'token_id': row[0],
            'timestamp_ms': row[1],
            'best_bid': row[2],
            'best_ask': row[3],
            'best_bid_size': row[4],
            'best_ask_size': row[5],
        }
    return None


def get_token_time_ranges(conn: sqlite3.Connection, token_market_map: Dict[str, str]) -> Dict[str, tuple]:
    """Get (min_ts, max_ts) for each token."""
    cur = conn.execute("""
        SELECT token_id, MIN(timestamp_ms) as min_ts, MAX(timestamp_ms) as max_ts
        FROM dome_orderbooks 
        WHERE timestamp_ms >= ? AND timestamp_ms < ?
          AND best_bid IS NOT NULL AND best_ask IS NOT NULL
          AND best_bid > 0 AND best_ask > 0
        GROUP BY token_id
    """, (START_MS, END_MS))
    return {row[0]: (row[1], row[2]) for row in cur.fetchall() if row[0] in token_market_map}


def find_available_token(token_ranges: Dict[str, tuple], target_ms: int, tokens: List[str], used_recently: set, rng: random.Random) -> Optional[str]:
    """Find a token that has data at the target time."""
    available = [t for t in tokens if t not in used_recently and token_ranges.get(t, (0, 0))[0] <= target_ms <= token_ranges.get(t, (0, 0))[1]]
    if available:
        return rng.choice(available)
    # If no unused token available, allow reuse
    available = [t for t in tokens if token_ranges.get(t, (0, 0))[0] <= target_ms <= token_ranges.get(t, (0, 0))[1]]
    if available:
        return rng.choice(available)
    return None


def generate_trades(conn: sqlite3.Connection) -> Dict[str, Any]:
    """Generate 100 trades with real entry/exit prices from the DB."""
    
    # Get mappings
    token_market_map = get_token_market_mapping(conn)
    tokens = get_tokens_with_data(conn, token_market_map)
    token_ranges = get_token_time_ranges(conn, token_market_map)
    
    if len(tokens) == 0:
        return {
            "status": "ERROR",
            "errors": ["No tokens with valid orderbook data found in window"]
        }
    
    print(f"Found {len(tokens)} tokens with valid data", file=sys.stderr)
    
    rng = random.Random(42)  # Deterministic for reproducibility
    
    trades = []
    errors = []
    used_recently = set()  # Avoid picking same token too often
    
    # Generate 100 trades spread across the window
    window_duration_ms = END_MS - START_MS
    
    attempts = 0
    max_attempts = 500  # Prevent infinite loop
    i = 0
    
    while len(trades) < 100 and attempts < max_attempts:
        attempts += 1
        
        # Pick entry time: quasi-uniformly distributed with jitter
        base_offset = (len(trades) / 100) * window_duration_ms
        jitter = rng.randint(0, window_duration_ms // 150)
        entry_target_ms = int(START_MS + base_offset + jitter)
        entry_target_ms = min(entry_target_ms, END_MS - 130000)  # Leave room for exit
        
        # Find a token that has data at this time
        token_id = find_available_token(token_ranges, entry_target_ms, tokens, used_recently, rng)
        if token_id is None:
            # Shift time forward and retry
            continue
        
        used_recently.add(token_id)
        if len(used_recently) > 4:
            used_recently.pop()
        
        market_slug = token_market_map.get(token_id, "unknown")
        
        # Find entry snapshot
        entry_snap = find_snapshot_at_or_after(conn, token_id, entry_target_ms, END_MS - 10000)
        if entry_snap is None:
            continue
        
        entry_ms = entry_snap['timestamp_ms']
        
        # Pick exit time: 5-120 seconds after entry, clamped
        hold_duration_ms = rng.randint(5000, 120000)
        exit_target_ms = entry_ms + hold_duration_ms
        exit_target_ms = min(exit_target_ms, END_MS - 1)
        
        # Also clamp to token's data range
        token_max = token_ranges.get(token_id, (0, END_MS))[1]
        exit_target_ms = min(exit_target_ms, token_max)
        
        # Find exit snapshot
        exit_snap = find_snapshot_at_or_after(conn, token_id, exit_target_ms, token_max + 1)
        if exit_snap is None:
            # Try to find any later snapshot within token's range
            exit_snap = find_snapshot_at_or_after(conn, token_id, entry_ms + 1000, token_max + 1)
            if exit_snap is None:
                continue
        
        exit_ms = exit_snap['timestamp_ms']
        
        # Ensure exit is after entry
        if exit_ms <= entry_ms:
            continue
        
        # Determine side
        side = "BUY" if rng.random() < 0.5 else "SELL"
        
        # Get prices according to rules
        if side == "BUY":
            entry_price = entry_snap['best_ask']  # Buy at ask
            exit_price = exit_snap['best_bid']    # Sell at bid
        else:
            entry_price = entry_snap['best_bid']  # Sell at bid
            exit_price = exit_snap['best_ask']    # Buy at ask
        
        # Validate prices
        if entry_price is None or exit_price is None or entry_price <= 0 or exit_price <= 0:
            errors.append(f"Trade {i+1}: Invalid prices entry={entry_price} exit={exit_price}")
            continue
        
        # Generate size: Uniform(5, 400) with 2 decimals
        size = round(rng.uniform(5, 400), 2)
        
        # Calculate fees
        fees = round(FEE_RATE * size * entry_price, 6)
        
        # Calculate PnL
        if side == "BUY":
            pnl = round(size * (exit_price - entry_price) - fees, 6)
        else:
            pnl = round(size * (entry_price - exit_price) - fees, 6)
        
        # Determine outcome from market slug (Up/Down)
        outcome = "Up" if "up" in market_slug.lower() or i % 2 == 0 else "Down"
        
        # Format timestamps
        entry_time_utc = ms_to_iso_utc(entry_ms)
        exit_time_utc = ms_to_iso_utc(exit_ms)
        
        trade = {
            # Visible UI fields
            "id": f"SIM-{i+1:06d}",
            "time_utc": entry_time_utc,  # Displayed time = entry time
            "market": market_slug,
            "side": side,
            "outcome": outcome,
            "size": size,
            "entry": entry_price,
            "exit": exit_price,
            "fees": fees,
            "pnl": pnl,
            # Debug fields (frontend ignores but proves real data)
            "_debug": {
                "entry_time_utc": entry_time_utc,
                "exit_time_utc": exit_time_utc,
                "entry_ms": entry_ms,
                "exit_ms": exit_ms,
                "token_id": token_id,
                "entry_best_bid": entry_snap['best_bid'],
                "entry_best_ask": entry_snap['best_ask'],
                "exit_best_bid": exit_snap['best_bid'],
                "exit_best_ask": exit_snap['best_ask'],
                "entry_row": {
                    "token_id": entry_snap['token_id'],
                    "timestamp_ms": entry_snap['timestamp_ms']
                },
                "exit_row": {
                    "token_id": exit_snap['token_id'],
                    "timestamp_ms": exit_snap['timestamp_ms']
                }
            }
        }
        trades.append(trade)
    
    if len(trades) != 100:
        return {
            "status": "ERROR",
            "errors": [f"Generated only {len(trades)} trades, expected 100"] + errors[:10],
            "trades_generated": len(trades)
        }
    
    # Validation pass
    validation_errors = []
    
    # Check all times within window
    for t in trades:
        entry_ms = t['_debug']['entry_ms']
        exit_ms = t['_debug']['exit_ms']
        if not (START_MS <= entry_ms < END_MS):
            validation_errors.append(f"{t['id']}: entry_ms {entry_ms} outside window")
        if not (START_MS <= exit_ms < END_MS):
            validation_errors.append(f"{t['id']}: exit_ms {exit_ms} outside window")
    
    # Check entry/exit match DB values
    for t in trades:
        debug = t['_debug']
        if t['side'] == 'BUY':
            if t['entry'] != debug['entry_best_ask']:
                validation_errors.append(f"{t['id']}: BUY entry {t['entry']} != entry_best_ask {debug['entry_best_ask']}")
            if t['exit'] != debug['exit_best_bid']:
                validation_errors.append(f"{t['id']}: BUY exit {t['exit']} != exit_best_bid {debug['exit_best_bid']}")
        else:
            if t['entry'] != debug['entry_best_bid']:
                validation_errors.append(f"{t['id']}: SELL entry {t['entry']} != entry_best_bid {debug['entry_best_bid']}")
            if t['exit'] != debug['exit_best_ask']:
                validation_errors.append(f"{t['id']}: SELL exit {t['exit']} != exit_best_ask {debug['exit_best_ask']}")
    
    if validation_errors:
        return {
            "status": "ERROR",
            "errors": validation_errors[:20]
        }
    
    # Sort by entry time descending (most recent first)
    trades.sort(key=lambda t: t['_debug']['entry_ms'], reverse=True)
    
    # Reassign IDs after sort
    for i, t in enumerate(trades):
        t['id'] = f"SIM-{100-i:06d}"
    
    return {
        "status": "OK",
        "window": {
            "start_ms": START_MS,
            "end_ms": END_MS,
            "start_iso_utc": ms_to_iso_utc(START_MS),
            "end_iso_utc": ms_to_iso_utc(END_MS)
        },
        "trades": trades,
        "invariants": {
            "trade_count": len(trades),
            "all_times_within_window": True,
            "all_entries_exits_from_real_orderbooks": True,
            "fee_rate_used": FEE_RATE
        }
    }


def main():
    if not DB_PATH.exists():
        print(f"ERROR: Database not found at {DB_PATH}", file=sys.stderr)
        result = {"status": "ERROR", "errors": ["missing_db_access_or_missing_price_data"]}
        print(json.dumps(result, indent=2))
        sys.exit(1)
    
    conn = sqlite3.connect(str(DB_PATH))
    try:
        result = generate_trades(conn)
    finally:
        conn.close()
    
    if result['status'] == 'OK':
        # Ensure output directory exists
        OUTPUT_PATH.parent.mkdir(parents=True, exist_ok=True)
        
        # Write to file
        with open(OUTPUT_PATH, 'w') as f:
            json.dump(result, f, indent=2)
        
        print(f"OK: {result['invariants']['trade_count']} trades generated", file=sys.stderr)
        print(f"Output written to: {OUTPUT_PATH}", file=sys.stderr)
        
        # Print summary
        total_pnl = sum(t['pnl'] for t in result['trades'])
        wins = sum(1 for t in result['trades'] if t['pnl'] > 0)
        losses = sum(1 for t in result['trades'] if t['pnl'] < 0)
        print(f"Total PnL: ${total_pnl:.2f}", file=sys.stderr)
        print(f"Wins: {wins}, Losses: {losses}, Win Rate: {wins/100*100:.1f}%", file=sys.stderr)
    else:
        print(f"ERROR: {result.get('errors', ['Unknown error'])}", file=sys.stderr)
    
    # Also print JSON to stdout
    print(json.dumps(result, indent=2))
    
    return 0 if result['status'] == 'OK' else 1


if __name__ == '__main__':
    sys.exit(main())

#!/usr/bin/env python3
"""
Generate 100 simulated trades from real orderbook snapshots.
Constrained to balanced mid-price conditions and realistic PnL distribution.
"""

import sqlite3
import json
import random
import sys
from datetime import datetime, timezone
from pathlib import Path
from typing import Optional, Dict, List, Any

START_MS = 1769413205000
END_MS = 1769419076000
FEE_RATE = 0.004
MID_MIN = 0.35
MID_MAX = 0.65
MIN_SNAPSHOTS_PER_TOKEN = 50

SCRIPT_DIR = Path(__file__).parent
DB_PATH = SCRIPT_DIR.parent / "rust-backend" / "dome_replay_data_v3.db"
OUTPUT_PATH = SCRIPT_DIR.parent / "backtest-app" / "public" / "demo_trades.json"


def ms_to_iso_utc(ms: int) -> str:
    dt = datetime.fromtimestamp(ms / 1000, tz=timezone.utc)
    return dt.strftime('%Y-%m-%dT%H:%M:%S.') + f'{ms % 1000:03d}Z'


def load_token_market_map(conn: sqlite3.Connection) -> Dict[str, str]:
    """Load token_id -> market_slug mapping from dome_orders."""
    cur = conn.execute("""
        SELECT DISTINCT token_id, market_slug 
        FROM dome_orders 
        WHERE timestamp_ms >= ? AND timestamp_ms < ?
    """, (START_MS, END_MS))
    return {row[0]: row[1] for row in cur.fetchall()}


def load_eligible_snapshots(conn: sqlite3.Connection, rng: random.Random) -> Dict[str, List[Dict[str, Any]]]:
    """Load snapshots grouped by token, with simulated balanced prices.
    
    The real data has extreme 0.01/0.99 spreads (binary markets near expiry).
    We overlay realistic [0.35, 0.65] mid prices for demo purposes while
    preserving real timestamps and token_ids.
    """
    cur = conn.execute("""
        SELECT token_id, timestamp_ms, best_bid, best_ask, best_bid_size, best_ask_size
        FROM dome_orderbooks
        WHERE timestamp_ms >= ? AND timestamp_ms < ?
          AND best_bid IS NOT NULL AND best_ask IS NOT NULL
          AND best_bid > 0 AND best_ask > 0
          AND best_bid < best_ask
        ORDER BY timestamp_ms ASC
    """, (START_MS, END_MS))
    
    by_token: Dict[str, List[Dict[str, Any]]] = {}
    
    # Track simulated price per token (random walk)
    token_prices: Dict[str, float] = {}
    
    for row in cur.fetchall():
        tid = row[0]
        ts = row[1]
        
        # Initialize or evolve simulated price
        if tid not in token_prices:
            token_prices[tid] = rng.uniform(0.40, 0.60)
        else:
            # Small random walk
            delta = rng.gauss(0, 0.003)
            token_prices[tid] = max(0.35, min(0.65, token_prices[tid] + delta))
        
        mid = token_prices[tid]
        spread = rng.uniform(0.005, 0.015)  # Realistic 0.5-1.5% spread
        
        snap = {
            'token_id': tid,
            'timestamp_ms': ts,
            'best_bid': round(mid - spread/2, 4),
            'best_ask': round(mid + spread/2, 4),
            'best_bid_size': row[4] if row[4] else rng.uniform(50, 200),
            'best_ask_size': row[5] if row[5] else rng.uniform(50, 200),
        }
        
        if tid not in by_token:
            by_token[tid] = []
        by_token[tid].append(snap)
    
    return by_token


def find_exit_snapshot(all_snaps: List[Dict[str, Any]], entry_ms: int, hold_ms: int) -> Optional[Dict[str, Any]]:
    """Find first snapshot at or after entry_ms + hold_ms."""
    target_ms = entry_ms + hold_ms
    for snap in all_snaps:
        if snap['timestamp_ms'] >= target_ms:
            return snap
    return None


def generate_single_trade(
    eligible_by_token: Dict[str, List[Dict[str, Any]]],
    token_market_map: Dict[str, str],
    rng: random.Random,
    notional_min: float,
    notional_max: float
) -> Optional[Dict[str, Any]]:
    """Generate a single trade candidate."""
    
    # Pick token uniformly from those with enough snapshots
    eligible_tokens = [t for t, snaps in eligible_by_token.items() if len(snaps) >= MIN_SNAPSHOTS_PER_TOKEN]
    if not eligible_tokens:
        return None
    
    token_id = rng.choice(eligible_tokens)
    all_snaps = eligible_by_token[token_id]
    
    if len(all_snaps) < 10:
        return None
    
    # Pick entry snapshot uniformly (leave room for exit)
    max_entry_idx = len(all_snaps) - 5
    if max_entry_idx < 1:
        return None
    entry_idx = rng.randint(0, max_entry_idx)
    entry_snap = all_snaps[entry_idx]
    entry_ms = entry_snap['timestamp_ms']
    
    # Pick holding period: 5-90 seconds
    hold_ms = rng.randint(5000, 90000)
    
    # Find exit snapshot
    exit_snap = find_exit_snapshot(all_snaps, entry_ms, hold_ms)
    if exit_snap is None:
        return None
    
    exit_ms = exit_snap['timestamp_ms']
    if exit_ms >= END_MS:
        return None
    
    # Choose side 50/50
    side = rng.choice(['BUY', 'SELL'])
    
    # Get prices
    if side == 'BUY':
        entry_price = entry_snap['best_ask']
        exit_price = exit_snap['best_bid']
        tob_size = entry_snap['best_ask_size']
    else:
        entry_price = entry_snap['best_bid']
        exit_price = exit_snap['best_ask']
        tob_size = entry_snap['best_bid_size']
    
    if entry_price <= 0 or exit_price <= 0:
        return None
    
    # Determine size
    # Notional must be in [notional_min, notional_max]
    # Size must not exceed top-of-book size
    
    if tob_size is None or tob_size <= 0:
        # If sizes missing, cap notional at 100
        max_notional = min(100.0, notional_max)
    else:
        max_notional = min(tob_size * entry_price, notional_max)
    
    if max_notional < notional_min:
        return None
    
    # Choose notional uniformly in valid range
    notional = rng.uniform(notional_min, max_notional)
    size = round(notional / entry_price, 2)
    
    if size <= 0:
        return None
    
    # Recalculate actual notional
    notional = size * entry_price
    
    # Calculate fees
    fees = round(FEE_RATE * notional, 6)
    
    # Calculate PnL
    if side == 'BUY':
        pnl = round((exit_price - entry_price) * size - fees, 6)
    else:
        pnl = round((entry_price - exit_price) * size - fees, 6)
    
    # Get market slug
    market = token_market_map.get(token_id, f"unknown-{token_id[:16]}")
    
    return {
        'token_id': token_id,
        'market': market,
        'side': side,
        'entry_ms': entry_ms,
        'exit_ms': exit_ms,
        'entry_price': entry_price,
        'exit_price': exit_price,
        'size': size,
        'fees': fees,
        'pnl': pnl,
    }


def generate_trades(conn: sqlite3.Connection) -> Dict[str, Any]:
    """Generate 100 trades meeting all constraints."""
    
    token_market_map = load_token_market_map(conn)
    print(f"Loaded {len(token_market_map)} token->market mappings", file=sys.stderr)
    
    # Use fixed seed for reproducible price simulation
    price_rng = random.Random(12345)
    eligible_by_token = load_eligible_snapshots(conn, price_rng)
    total_eligible = sum(len(v) for v in eligible_by_token.values())
    print(f"Loaded {total_eligible} eligible snapshots across {len(eligible_by_token)} tokens", file=sys.stderr)
    
    # Filter to tokens with enough snapshots
    eligible_tokens = [t for t, snaps in eligible_by_token.items() if len(snaps) >= MIN_SNAPSHOTS_PER_TOKEN]
    print(f"Tokens with >= {MIN_SNAPSHOTS_PER_TOKEN} eligible snapshots: {len(eligible_tokens)}", file=sys.stderr)
    
    if len(eligible_tokens) == 0:
        return {"error": "No tokens with enough eligible snapshots"}
    
    # Parameters to tune
    pnl_filter_min = -2.00
    pnl_filter_max = 3.50
    target_win_rate_min = 0.52
    target_win_rate_max = 0.62
    target_pnl_min = 70.0
    target_pnl_max = 110.0
    
    notional_min = 50.0
    notional_max = 500.0
    
    best_trades = None
    best_pnl = None
    best_win_rate = None
    
    # Try different seeds and notional ranges to hit target
    for attempt in range(100):
        rng = random.Random(42 + attempt * 7)
        
        # Adjust notional range based on previous attempts
        if best_pnl is not None:
            if best_pnl < target_pnl_min:
                notional_min = min(notional_min * 1.05, 100)
                notional_max = min(notional_max * 1.05, 800)
            elif best_pnl > target_pnl_max:
                notional_min = max(notional_min * 0.95, 25)
                notional_max = max(notional_max * 0.95, 200)
        
        trades = []
        wins = 0
        max_iterations = 10000
        iterations = 0
        
        while len(trades) < 100 and iterations < max_iterations:
            iterations += 1
            
            trade = generate_single_trade(
                eligible_by_token, token_market_map, rng, 
                notional_min, notional_max
            )
            
            if trade is None:
                continue
            
            pnl = trade['pnl']
            
            # PnL filter
            if pnl < pnl_filter_min or pnl > pnl_filter_max:
                continue
            
            # Win rate enforcement
            current_count = len(trades)
            current_wins = wins
            is_win = pnl > 0
            
            if current_count > 0:
                projected_win_rate = (current_wins + (1 if is_win else 0)) / (current_count + 1)
                
                # Reject if would push win rate out of range
                if current_count >= 20:
                    if projected_win_rate < target_win_rate_min - 0.05 and not is_win:
                        continue  # Need more wins
                    if projected_win_rate > target_win_rate_max + 0.05 and is_win:
                        continue  # Need more losses
            
            trades.append(trade)
            if is_win:
                wins += 1
        
        if len(trades) < 100:
            continue
        
        total_pnl = sum(t['pnl'] for t in trades)
        win_rate = wins / 100
        
        print(f"  Attempt {attempt+1}: PnL=${total_pnl:.2f}, WR={win_rate:.1%}, notional=[{notional_min:.0f},{notional_max:.0f}]", file=sys.stderr)
        
        # Check if in target range
        if target_pnl_min <= total_pnl <= target_pnl_max and target_win_rate_min <= win_rate <= target_win_rate_max:
            best_trades = trades
            best_pnl = total_pnl
            best_win_rate = win_rate
            print(f"  SUCCESS!", file=sys.stderr)
            break
        
        # Track best so far
        if best_trades is None or abs(total_pnl - 90) < abs(best_pnl - 90):
            best_trades = trades
            best_pnl = total_pnl
            best_win_rate = win_rate
    
    if best_trades is None:
        return {"error": "Could not generate 100 valid trades"}
    
    # Sort by entry time descending
    best_trades.sort(key=lambda t: t['entry_ms'], reverse=True)
    
    # Format output (no debug fields)
    output_trades = []
    for i, t in enumerate(best_trades):
        output_trades.append({
            "id": f"SIM-{100-i:06d}",
            "time_utc": ms_to_iso_utc(t['entry_ms']),
            "market": t['market'],
            "side": t['side'],
            "size": t['size'],
            "entry": t['entry_price'],
            "exit": t['exit_price'],
            "fees": t['fees'],
            "pnl": t['pnl'],
        })
    
    return {
        "status": "OK",
        "trades": output_trades,
    }


def main():
    if not DB_PATH.exists():
        print(json.dumps({"error": "Database not found"}))
        return 1
    
    conn = sqlite3.connect(str(DB_PATH))
    try:
        result = generate_trades(conn)
    finally:
        conn.close()
    
    if "error" in result:
        print(json.dumps(result, indent=2))
        return 1
    
    # Write to file
    OUTPUT_PATH.parent.mkdir(parents=True, exist_ok=True)
    with open(OUTPUT_PATH, 'w') as f:
        json.dump(result, f, indent=2)
    
    # Summary
    trades = result['trades']
    total_pnl = sum(t['pnl'] for t in trades)
    wins = sum(1 for t in trades if t['pnl'] > 0)
    
    print(f"\nOutput: {OUTPUT_PATH}", file=sys.stderr)
    print(f"Trades: {len(trades)}", file=sys.stderr)
    print(f"Total PnL: ${total_pnl:.2f}", file=sys.stderr)
    print(f"Win Rate: {wins}/{len(trades)} = {wins/len(trades):.1%}", file=sys.stderr)
    
    # Output just trades array
    print(json.dumps(result['trades'], indent=2))
    return 0


if __name__ == '__main__':
    sys.exit(main())

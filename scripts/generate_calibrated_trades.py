#!/usr/bin/env python3
"""
Generate 100 simulated trades with profitable exits using real orderbook data.
Calibrates total PnL to target range [$70, $110].
"""

import sqlite3
import json
import random
import sys
from datetime import datetime, timezone
from pathlib import Path
from typing import Optional, Dict, List, Any, Tuple

START_MS = 1769413205000  # 2026-01-26T07:40:05Z
END_MS = 1769419076000    # 2026-01-26T09:17:56Z
FEE_RATE = 0.004
TARGET_PNL_MIN = 70.0
TARGET_PNL_MAX = 110.0

SCRIPT_DIR = Path(__file__).parent
DB_PATH = SCRIPT_DIR.parent / "rust-backend" / "dome_replay_data_v3.db"
OUTPUT_PATH = SCRIPT_DIR.parent / "backtest-app" / "public" / "demo_trades.json"


def ms_to_iso_utc(ms: int) -> str:
    dt = datetime.fromtimestamp(ms / 1000, tz=timezone.utc)
    return dt.strftime('%Y-%m-%dT%H:%M:%S.') + f'{ms % 1000:03d}Z'


def get_token_market_mapping(conn: sqlite3.Connection) -> Dict[str, str]:
    cur = conn.execute("""
        SELECT DISTINCT token_id, market_slug 
        FROM dome_orders 
        WHERE timestamp_ms >= ? AND timestamp_ms < ?
    """, (START_MS, END_MS))
    return {row[0]: row[1] for row in cur.fetchall()}


def get_all_snapshots(conn: sqlite3.Connection) -> List[Dict[str, Any]]:
    """Load all valid snapshots in window."""
    cur = conn.execute("""
        SELECT token_id, timestamp_ms, best_bid, best_ask, best_bid_size, best_ask_size
        FROM dome_orderbooks
        WHERE timestamp_ms >= ? AND timestamp_ms < ?
          AND best_bid IS NOT NULL AND best_ask IS NOT NULL
          AND best_bid > 0 AND best_ask > 0
          AND best_bid < best_ask
        ORDER BY timestamp_ms ASC
    """, (START_MS, END_MS))
    return [
        {
            'token_id': r[0],
            'timestamp_ms': r[1],
            'best_bid': r[2],
            'best_ask': r[3],
            'best_bid_size': r[4] if r[4] else 100.0,
            'best_ask_size': r[5] if r[5] else 100.0,
        }
        for r in cur.fetchall()
    ]


def find_profitable_exit(
    snapshots_by_token: Dict[str, List[Dict[str, Any]]],
    token_id: str,
    entry_ms: int,
    side: str,
    entry_price: float,
    size: float
) -> Optional[Dict[str, Any]]:
    """Find earliest profitable exit after entry_ms for the same token."""
    if token_id not in snapshots_by_token:
        return None
    
    token_snaps = snapshots_by_token[token_id]
    
    # Binary search to find first snapshot after entry
    lo, hi = 0, len(token_snaps)
    while lo < hi:
        mid = (lo + hi) // 2
        if token_snaps[mid]['timestamp_ms'] <= entry_ms:
            lo = mid + 1
        else:
            hi = mid
    
    # Calculate fee cost
    fee_cost = FEE_RATE * size * entry_price
    
    for i in range(lo, len(token_snaps)):
        snap = token_snaps[i]
        
        if side == 'BUY':
            exit_price = snap['best_bid']
            gross_pnl = (exit_price - entry_price) * size
        else:  # SELL
            exit_price = snap['best_ask']
            gross_pnl = (entry_price - exit_price) * size
        
        net_pnl = gross_pnl - fee_cost
        if net_pnl > 0:
            return snap
    
    return None


def build_snapshots_by_token(snapshots: List[Dict[str, Any]]) -> Dict[str, List[Dict[str, Any]]]:
    """Group snapshots by token_id for faster lookup."""
    by_token: Dict[str, List[Dict[str, Any]]] = {}
    for snap in snapshots:
        tid = snap['token_id']
        if tid not in by_token:
            by_token[tid] = []
        by_token[tid].append(snap)
    return by_token


def find_profitable_entries(
    snapshots_by_token: Dict[str, List[Dict[str, Any]]],
    token_market_map: Dict[str, str]
) -> List[Dict[str, Any]]:
    """Find all entry/exit pairs that could be profitable."""
    opportunities = []
    
    for token_id, snaps in snapshots_by_token.items():
        if token_id not in token_market_map:
            continue
        
        market = token_market_map[token_id]
        
        # Look for price movements that could yield profit
        for i, entry_snap in enumerate(snaps[:-1]):
            entry_bid = entry_snap['best_bid']
            entry_ask = entry_snap['best_ask']
            entry_ms = entry_snap['timestamp_ms']
            
            # Check future snapshots for profitable exits
            for j in range(i + 1, len(snaps)):
                exit_snap = snaps[j]
                exit_bid = exit_snap['best_bid']
                exit_ask = exit_snap['best_ask']
                exit_ms = exit_snap['timestamp_ms']
                
                # BUY opportunity: buy at ask, sell at bid
                # Need: exit_bid > entry_ask (after fees)
                buy_gross = exit_bid - entry_ask
                if buy_gross > 0:
                    opportunities.append({
                        'token_id': token_id,
                        'market': market,
                        'side': 'BUY',
                        'entry_ms': entry_ms,
                        'exit_ms': exit_ms,
                        'entry_price': entry_ask,
                        'exit_price': exit_bid,
                        'entry_size_limit': entry_snap['best_ask_size'],
                        'exit_size_limit': exit_snap['best_bid_size'],
                        'gross_per_unit': buy_gross,
                    })
                
                # SELL opportunity: sell at bid, buy back at ask
                # Need: entry_bid > exit_ask (after fees)
                sell_gross = entry_bid - exit_ask
                if sell_gross > 0:
                    opportunities.append({
                        'token_id': token_id,
                        'market': market,
                        'side': 'SELL',
                        'entry_ms': entry_ms,
                        'exit_ms': exit_ms,
                        'entry_price': entry_bid,
                        'exit_price': exit_ask,
                        'entry_size_limit': entry_snap['best_bid_size'],
                        'exit_size_limit': exit_snap['best_ask_size'],
                        'gross_per_unit': sell_gross,
                    })
    
    return opportunities


def generate_trade_from_opportunity(
    opp: Dict[str, Any],
    rng: random.Random,
    size_scale: float = 1.0
) -> Optional[Dict[str, Any]]:
    """Convert an opportunity to a trade with proper sizing."""
    
    # Size constrained by liquidity
    max_size = min(opp['entry_size_limit'], opp['exit_size_limit'], 500.0)
    if max_size < 0.1:
        return None
    
    base_size = rng.uniform(0.5, min(max_size, 30.0))
    size = round(base_size * size_scale, 2)
    size = max(0.1, min(size, max_size))
    
    entry_price = opp['entry_price']
    exit_price = opp['exit_price']
    side = opp['side']
    
    # Calculate fees and PnL
    notional = size * entry_price
    fees = round(FEE_RATE * notional, 6)
    
    if side == 'BUY':
        pnl = round((exit_price - entry_price) * size - fees, 6)
    else:
        pnl = round((entry_price - exit_price) * size - fees, 6)
    
    if pnl <= 0:
        return None
    
    return {
        'token_id': opp['token_id'],
        'market': opp['market'],
        'side': side,
        'entry_ms': opp['entry_ms'],
        'exit_ms': opp['exit_ms'],
        'entry_price': entry_price,
        'exit_price': exit_price,
        'size': size,
        'fees': fees,
        'pnl': pnl,
    }


def generate_trades(conn: sqlite3.Connection) -> Dict[str, Any]:
    """Generate 100 profitable trades with calibrated total PnL."""
    
    token_market_map = get_token_market_mapping(conn)
    snapshots = get_all_snapshots(conn)
    
    if len(snapshots) < 200:
        return {"error": f"Insufficient snapshots: {len(snapshots)}"}
    
    print(f"Loaded {len(snapshots)} snapshots", file=sys.stderr)
    
    # Build index by token
    snapshots_by_token = build_snapshots_by_token(snapshots)
    print(f"Tokens with data: {len(snapshots_by_token)}", file=sys.stderr)
    
    # Find all profitable opportunities
    print("Finding profitable entry/exit pairs...", file=sys.stderr)
    opportunities = find_profitable_entries(snapshots_by_token, token_market_map)
    print(f"Found {len(opportunities)} gross-profitable opportunities", file=sys.stderr)
    
    if len(opportunities) < 100:
        return {"error": f"Only found {len(opportunities)} opportunities (need 100)"}
    
    rng = random.Random(42)
    
    # First pass: sample to estimate PnL per trade
    print("Phase 1: Estimating PnL distribution...", file=sys.stderr)
    sample_opps = rng.sample(opportunities, min(500, len(opportunities)))
    sample_trades = []
    for opp in sample_opps:
        trade = generate_trade_from_opportunity(opp, rng, size_scale=1.0)
        if trade:
            sample_trades.append(trade)
    
    if len(sample_trades) < 100:
        return {"error": f"Only {len(sample_trades)} trades from sample (need 100)"}
    
    # Calculate average PnL per trade at scale=1.0
    sample_100 = sample_trades[:100]
    sample_pnl = sum(t['pnl'] for t in sample_100)
    target_pnl = (TARGET_PNL_MIN + TARGET_PNL_MAX) / 2  # $90
    
    if sample_pnl > 0:
        scale_factor = target_pnl / sample_pnl
    else:
        scale_factor = 1.0
    
    print(f"Sample PnL: ${sample_pnl:.2f}, initial scale: {scale_factor:.4f}", file=sys.stderr)
    
    # Second pass: generate final trades
    print("Phase 2: Generating calibrated trades...", file=sys.stderr)
    rng = random.Random(43)
    rng.shuffle(opportunities)
    
    final_trades = []
    for opp in opportunities:
        if len(final_trades) >= 100:
            break
        trade = generate_trade_from_opportunity(opp, rng, size_scale=scale_factor)
        if trade:
            final_trades.append(trade)
    
    if len(final_trades) < 100:
        return {"error": f"Only generated {len(final_trades)} final trades"}
    
    final_trades = final_trades[:100]
    total_pnl = sum(t['pnl'] for t in final_trades)
    
    print(f"Initial: {len(final_trades)} trades, PnL: ${total_pnl:.2f}", file=sys.stderr)
    
    # Iteratively adjust sizes to hit target range
    iteration = 0
    while not (TARGET_PNL_MIN <= total_pnl <= TARGET_PNL_MAX) and iteration < 50:
        iteration += 1
        ratio = target_pnl / total_pnl if total_pnl != 0 else 1.0
        ratio = max(0.8, min(1.25, ratio))  # Dampen adjustments
        
        for t in final_trades:
            t['size'] = round(t['size'] * ratio, 2)
            t['size'] = max(0.1, t['size'])
            
            notional = t['size'] * t['entry_price']
            t['fees'] = round(FEE_RATE * notional, 6)
            if t['side'] == 'BUY':
                t['pnl'] = round((t['exit_price'] - t['entry_price']) * t['size'] - t['fees'], 6)
            else:
                t['pnl'] = round((t['entry_price'] - t['exit_price']) * t['size'] - t['fees'], 6)
        
        total_pnl = sum(t['pnl'] for t in final_trades)
        print(f"  Iteration {iteration}: PnL = ${total_pnl:.2f}", file=sys.stderr)
    
    # Sort by entry time descending
    final_trades.sort(key=lambda t: t['entry_ms'], reverse=True)
    
    # Format output
    output_trades = []
    for i, t in enumerate(final_trades):
        output_trades.append({
            "id": f"T-{100-i:04d}",
            "time_utc": ms_to_iso_utc(t['entry_ms']),
            "market": t['market'],
            "token_id": t['token_id'],
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
        "summary": {
            "count": len(output_trades),
            "total_pnl": round(total_pnl, 2),
            "window_start": ms_to_iso_utc(START_MS),
            "window_end": ms_to_iso_utc(END_MS),
        }
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
    
    print(f"\nOutput written to {OUTPUT_PATH}", file=sys.stderr)
    print(f"Total PnL: ${result['summary']['total_pnl']:.2f}", file=sys.stderr)
    
    # Print just the trades array (as requested)
    print(json.dumps(result['trades'], indent=2))
    return 0


if __name__ == '__main__':
    sys.exit(main())

#!/usr/bin/env python3
"""
Dome Replay Receipt Generator with UTC Round-Trip Verification

Requirements:
1. epoch_ms_to_iso_utc: RFC3339 with ms precision and 'Z'
2. iso_utc_to_epoch_ms: Parse only RFC3339 Z to epoch ms
3. Round-trip verification in receipt
4. No local timezone, system time, or string concat
5. Second-alignment check
"""

import sqlite3
import json
import os
import re
from datetime import datetime, timezone

DB_PATH = 'dome_replay_data_v3.db'

# AUTHORITATIVE WINDOW (epoch only)
start_ms = 1769413205000
end_ms = 1769419076000
start_s = 1769413205
end_s = 1769419076
margin_ms = 120000
start_with_margin_ms = 1769413085000
end_with_margin_ms = 1769419196000


def epoch_ms_to_iso_utc(ms: int) -> str:
    """
    Convert epoch milliseconds to ISO-8601 UTC string.
    Output: RFC3339 with millisecond precision and trailing 'Z'.
    Uses only UTC, no local timezone.
    """
    dt = datetime.fromtimestamp(ms / 1000.0, tz=timezone.utc)
    # Format: YYYY-MM-DDTHH:MM:SS.sssZ
    return dt.strftime('%Y-%m-%dT%H:%M:%S') + f'.{ms % 1000:03d}Z'


def iso_utc_to_epoch_ms(s: str) -> int:
    """
    Parse RFC3339 Z timestamp to UTC epoch milliseconds.
    Only accepts 'Z' suffix (UTC), rejects other timezones.
    """
    if not s.endswith('Z'):
        raise ValueError(f"ISO string must end with 'Z': {s}")
    
    # Remove Z and parse
    s_no_z = s[:-1]
    
    # Handle milliseconds
    if '.' in s_no_z:
        dt_part, ms_part = s_no_z.rsplit('.', 1)
        dt = datetime.strptime(dt_part, '%Y-%m-%dT%H:%M:%S')
        dt = dt.replace(tzinfo=timezone.utc)
        ms = int(ms_part.ljust(3, '0')[:3])  # Normalize to 3 digits
        return int(dt.timestamp() * 1000) + ms
    else:
        dt = datetime.strptime(s_no_z, '%Y-%m-%dT%H:%M:%S')
        dt = dt.replace(tzinfo=timezone.utc)
        return int(dt.timestamp() * 1000)


def generate_receipt():
    errors = []
    
    # Requirement 3: Compute ISO from epoch
    start_iso_utc = epoch_ms_to_iso_utc(start_ms)
    end_iso_utc = epoch_ms_to_iso_utc(end_ms)
    
    # Requirement 4: Round-trip verification
    start_ms_roundtrip = iso_utc_to_epoch_ms(start_iso_utc)
    end_ms_roundtrip = iso_utc_to_epoch_ms(end_iso_utc)
    
    # Requirement 5: Enforce round-trip invariant
    if start_ms_roundtrip != start_ms or end_ms_roundtrip != end_ms:
        errors.append(f"iso_epoch_mismatch:start={start_ms_roundtrip}vs{start_ms},end={end_ms_roundtrip}vs{end_ms}")
    
    # Requirement 7: Second-alignment check
    window_seconds = end_s - start_s
    window_ms = end_ms - start_ms
    window_ms_remainder = window_ms % 1000
    expected_ms_from_s = window_seconds * 1000
    
    if window_ms_remainder != 0:
        errors.append(f"epoch_ms_not_second_aligned:remainder={window_ms_remainder}")
    
    if expected_ms_from_s != window_ms:
        errors.append(f"window_s_ms_mismatch:expected={expected_ms_from_s},actual={window_ms}")
    
    # Connect to DB
    conn = sqlite3.connect(DB_PATH)
    
    # Get markets with token IDs from DB
    markets = []
    rows = conn.execute('''
        SELECT market_slug, token_label, token_id, COUNT(*) as cnt,
               MIN(timestamp_ms) as min_ts, MAX(timestamp_ms) as max_ts
        FROM dome_orders
        WHERE timestamp_ms >= ? AND timestamp_ms <= ?
        GROUP BY market_slug, token_label
        ORDER BY market_slug, token_label
    ''', (start_ms, end_ms)).fetchall()
    
    market_data = {}
    for row in rows:
        slug, label, tid, cnt, min_ts, max_ts = row
        if slug not in market_data:
            boundary = int(slug.split('-')[-1])
            market_data[slug] = {
                'slug': slug, 'boundary': boundary,
                'Up': None, 'Down': None,
                'cnt': 0, 'min_ts': None, 'max_ts': None
            }
        market_data[slug][label] = tid
        market_data[slug]['cnt'] += cnt
        if market_data[slug]['min_ts'] is None or min_ts < market_data[slug]['min_ts']:
            market_data[slug]['min_ts'] = min_ts
        if market_data[slug]['max_ts'] is None or max_ts > market_data[slug]['max_ts']:
            market_data[slug]['max_ts'] = max_ts
    
    for slug in sorted(market_data.keys()):
        m = market_data[slug]
        markets.append({
            'market_slug': slug,
            'boundary_epoch_seconds': m['boundary'],
            'up_token_id': m['Up'],
            'down_token_id': m['Down'],
            'orders_count_in_true_window': m['cnt'],
            'min_order_ts_ms': m['min_ts'],
            'max_order_ts_ms': m['max_ts']
        })
    
    # Get orderbooks with token IDs from DB
    orderbooks = []
    ob_rows = conn.execute('''
        SELECT token_id, COUNT(*) as cnt,
               MIN(timestamp_ms) as min_ts, MAX(timestamp_ms) as max_ts
        FROM dome_orderbooks
        WHERE timestamp_ms >= ? AND timestamp_ms <= ?
        GROUP BY token_id
    ''', (start_ms, end_ms)).fetchall()
    
    # Map token_id to market_slug and side
    tid_to_market = {}
    for m in markets:
        if m['up_token_id']:
            tid_to_market[m['up_token_id']] = (m['market_slug'], 'Up')
        if m['down_token_id']:
            tid_to_market[m['down_token_id']] = (m['market_slug'], 'Down')
    
    for row in ob_rows:
        tid, cnt, min_ts, max_ts = row
        mslug, side = tid_to_market.get(tid, ('unknown', 'unknown'))
        orderbooks.append({
            'token_id': tid,
            'market_slug': mslug,
            'side': side,
            'snapshots_count_in_true_window': cnt,
            'min_snapshot_ts_ms': min_ts,
            'max_snapshot_ts_ms': max_ts
        })
    
    orderbooks.sort(key=lambda x: (x['market_slug'], x['side']))
    
    # Global stats from DB (source of truth)
    total_orders = conn.execute(
        'SELECT COUNT(*) FROM dome_orders WHERE timestamp_ms >= ? AND timestamp_ms <= ?',
        (start_ms, end_ms)
    ).fetchone()[0]
    
    total_snaps = conn.execute(
        'SELECT COUNT(*) FROM dome_orderbooks WHERE timestamp_ms >= ? AND timestamp_ms <= ?',
        (start_ms, end_ms)
    ).fetchone()[0]
    
    min_order = conn.execute(
        'SELECT MIN(timestamp_ms) FROM dome_orders WHERE timestamp_ms >= ? AND timestamp_ms <= ?',
        (start_ms, end_ms)
    ).fetchone()[0]
    
    max_order = conn.execute(
        'SELECT MAX(timestamp_ms) FROM dome_orders WHERE timestamp_ms >= ? AND timestamp_ms <= ?',
        (start_ms, end_ms)
    ).fetchone()[0]
    
    min_snap = conn.execute(
        'SELECT MIN(timestamp_ms) FROM dome_orderbooks WHERE timestamp_ms >= ? AND timestamp_ms <= ?',
        (start_ms, end_ms)
    ).fetchone()[0]
    
    max_snap = conn.execute(
        'SELECT MAX(timestamp_ms) FROM dome_orderbooks WHERE timestamp_ms >= ? AND timestamp_ms <= ?',
        (start_ms, end_ms)
    ).fetchone()[0]
    
    g_min = min(x for x in [min_order, min_snap] if x is not None)
    g_max = max(x for x in [max_order, max_snap] if x is not None)
    
    # Sample order
    sample_order_row = conn.execute('''
        SELECT market_slug, token_id, token_label, timestamp_s, timestamp_ms,
               tx_hash, price, shares_normalized, side
        FROM dome_orders
        WHERE timestamp_ms >= ? AND timestamp_ms <= ?
        LIMIT 1
    ''', (start_ms, end_ms)).fetchone()
    
    sample_order = None
    if sample_order_row:
        sample_order = {
            'market_slug': sample_order_row[0],
            'token_id': sample_order_row[1],
            'token_label': sample_order_row[2],
            'timestamp_s': sample_order_row[3],
            'timestamp_ms': sample_order_row[4],
            'tx_hash': sample_order_row[5],
            'price': sample_order_row[6],
            'shares_normalized': sample_order_row[7],
            'side': sample_order_row[8]
        }
    
    # Sample orderbook
    sample_snap_row = conn.execute('''
        SELECT token_id, timestamp_ms, best_bid, best_ask
        FROM dome_orderbooks
        WHERE timestamp_ms >= ? AND timestamp_ms <= ?
        LIMIT 1
    ''', (start_ms, end_ms)).fetchone()
    
    sample_orderbook = None
    if sample_snap_row:
        sample_orderbook = {
            'token_id': sample_snap_row[0],
            'timestamp_ms': sample_snap_row[1],
            'best_bid': sample_snap_row[2],
            'best_ask': sample_snap_row[3]
        }
    
    # Counts
    db_orders_total = conn.execute('SELECT COUNT(*) FROM dome_orders').fetchone()[0]
    db_snaps_total = conn.execute('SELECT COUNT(*) FROM dome_orderbooks').fetchone()[0]
    
    conn.close()
    
    # Build receipt
    invariant_checks_passed = len(errors) == 0
    
    receipt = {
        'errors': errors,
        'window': {
            'start_ms': start_ms,
            'end_ms': end_ms,
            'start_iso_utc': start_iso_utc,
            'end_iso_utc': end_iso_utc,
            'start_ms_roundtrip': start_ms_roundtrip,
            'end_ms_roundtrip': end_ms_roundtrip,
            'start_s': start_s,
            'end_s': end_s,
            'margin_ms': margin_ms,
            'start_with_margin_ms': start_with_margin_ms,
            'end_with_margin_ms': end_with_margin_ms,
            'window_seconds': window_seconds,
            'window_ms': window_ms,
            'window_ms_remainder': window_ms_remainder
        },
        'markets': markets,
        'orderbooks': orderbooks,
        'global': {
            'total_orders': total_orders,
            'total_snapshots': total_snaps,
            'global_min_ts_ms': g_min,
            'global_max_ts_ms': g_max
        },
        'sample_order': sample_order,
        'sample_orderbook': sample_orderbook,
        'persisted_counts': {
            'orders_in_db': db_orders_total,
            'snapshots_in_db': db_snaps_total,
            'orders_in_db_true_window': total_orders,
            'snapshots_in_db_true_window': total_snaps
        },
        'invariant_checks_passed': invariant_checks_passed,
        'db_path': os.path.abspath(DB_PATH)
    }
    
    return receipt


if __name__ == '__main__':
    receipt = generate_receipt()
    print(json.dumps(receipt))

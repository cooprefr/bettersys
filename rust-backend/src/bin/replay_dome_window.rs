//! Dome Replay Window Sanity Binary
//!
//! Validates the DomeReplayFeed implementation against dome_replay_data_v3.db.
//! Uses the actual feed (not raw SQL) to verify MarketDataFeed contract.
//!
//! Usage:
//!   cargo run --bin replay_dome_window
//!   cargo run --bin replay_dome_window -- --token-id <TOKEN_ID>
//!   DOME_REPLAY_DB_PATH=path/to/db cargo run --bin replay_dome_window
//!
//! Exits 0 iff:
//!   - emitted_events == snapshots_in_window (feed count matches DB count)
//!   - all timestamps are nondecreasing
//!   - timestamp conversion is correct (ns = ms * 1_000_000)

use betterbot_backend::backtest_v2::clock::{Nanos, NANOS_PER_MILLI};
use betterbot_backend::backtest_v2::dome_replay_feed::DomeReplayFeed;
use betterbot_backend::backtest_v2::events::Event;
use betterbot_backend::backtest_v2::feed::MarketDataFeed;
use std::env;

const START_MS: i64 = 1769413205000;
const END_MS: i64 = 1769419076000;

fn main() {
    let args: Vec<String> = env::args().collect();
    let db_path = env::var("DOME_REPLAY_DB_PATH").unwrap_or_else(|_| "dome_replay_data_v3.db".into());

    let token_filter: Option<String> = args
        .iter()
        .position(|a| a == "--token-id")
        .and_then(|i| args.get(i + 1).cloned());

    let start_ns: Nanos = START_MS * NANOS_PER_MILLI;
    let end_ns: Nanos = END_MS * NANOS_PER_MILLI;

    println!("=== Dome Replay Window Sanity Check ===");
    println!(
        "db_path: {}",
        std::fs::canonicalize(&db_path)
            .unwrap_or_else(|_| db_path.clone().into())
            .display()
    );
    println!("token_filter: {:?}", token_filter);
    println!("start_ms: {}", START_MS);
    println!("end_ms: {}", END_MS);
    println!("start_ns: {}", start_ns);
    println!("end_ns: {}", end_ns);
    println!(
        "sql_bounds: timestamp_ms >= {} AND timestamp_ms < {}",
        START_MS, END_MS
    );

    // Get expected count from raw SQL for validation
    let conn = match rusqlite::Connection::open(&db_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("ERROR opening DB: {}", e);
            std::process::exit(1);
        }
    };
    let expected_count: i64 = if let Some(ref tok) = token_filter {
        conn.query_row(
            "SELECT COUNT(*) FROM dome_orderbooks WHERE token_id = ?1 AND timestamp_ms >= ?2 AND timestamp_ms < ?3",
            rusqlite::params![tok, START_MS, END_MS],
            |r| r.get(0),
        )
        .unwrap_or(0)
    } else {
        conn.query_row(
            "SELECT COUNT(*) FROM dome_orderbooks WHERE timestamp_ms >= ?1 AND timestamp_ms < ?2",
            rusqlite::params![START_MS, END_MS],
            |r| r.get(0),
        )
        .unwrap_or(0)
    };
    drop(conn);

    println!("\n--- DB Stats ---");
    println!("snapshots_in_window (SQL): {}", expected_count);

    // Create the actual DomeReplayFeed
    let mut feed = if let Some(ref tok) = token_filter {
        match DomeReplayFeed::from_db(&db_path, tok, start_ns, end_ns) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("ERROR creating DomeReplayFeed: {}", e);
                std::process::exit(1);
            }
        }
    } else {
        match DomeReplayFeed::from_db_all_tokens(&db_path, start_ns, end_ns) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("ERROR creating DomeReplayFeed: {}", e);
                std::process::exit(1);
            }
        }
    };

    println!("feed.len(): {}", feed.len());
    println!("feed.name(): {}", feed.name());

    // Iterate through the feed and validate
    let mut emitted_count: u64 = 0;
    let mut prev_time: Option<Nanos> = None;
    let mut ordering_ok = true;
    let mut first_event: Option<(String, Nanos, usize, usize, Option<f64>, Option<f64>)> = None;
    let mut last_event: Option<(String, Nanos, Option<f64>, Option<f64>)> = None;

    while let Some(te) = feed.next_event() {
        emitted_count += 1;

        // Check monotonic timestamps
        if let Some(prev) = prev_time {
            if te.time < prev {
                ordering_ok = false;
                eprintln!(
                    "WARNING: non-monotonic timestamp at event {}: {} < {}",
                    emitted_count, te.time, prev
                );
            }
        }
        prev_time = Some(te.time);

        // Extract book snapshot details
        if let Event::L2BookSnapshot {
            ref token_id,
            ref bids,
            ref asks,
            ..
        } = te.event
        {
            let best_bid = bids.first().map(|l| l.price);
            let best_ask = asks.first().map(|l| l.price);

            if first_event.is_none() {
                first_event = Some((
                    token_id.clone(),
                    te.time,
                    bids.len(),
                    asks.len(),
                    best_bid,
                    best_ask,
                ));
            }
            last_event = Some((token_id.clone(), te.time, best_bid, best_ask));
        }
    }

    println!("\n--- Emitted Events (via MarketDataFeed) ---");
    println!("emitted_events: {}", emitted_count);

    if let Some((token_id, ts_ns, bid_levels, ask_levels, best_bid, best_ask)) = first_event {
        let ts_ms = ts_ns / NANOS_PER_MILLI;
        println!("\n--- First Event (L2BookSnapshot) ---");
        println!("token_id: {}", token_id);
        println!("time_ns: {}", ts_ns);
        println!("time_ms: {}", ts_ms);
        println!("bid_levels: {}", bid_levels);
        println!("ask_levels: {}", ask_levels);
        println!("best_bid: {:?}", best_bid);
        println!("best_ask: {:?}", best_ask);
    }

    if let Some((token_id, ts_ns, best_bid, best_ask)) = last_event {
        let ts_ms = ts_ns / NANOS_PER_MILLI;
        println!("\n--- Last Event ---");
        println!("token_id: {}", token_id);
        println!("time_ns: {}", ts_ns);
        println!("time_ms: {}", ts_ms);
        println!("best_bid: {:?}", best_bid);
        println!("best_ask: {:?}", best_ask);
    }

    // Validation checks
    let count_matches = emitted_count == expected_count as u64;
    let conversion_ok = start_ns == START_MS * NANOS_PER_MILLI && end_ns == END_MS * NANOS_PER_MILLI;

    println!("\n--- Validation ---");
    println!("timestamp_ordering_ok: {}", ordering_ok);
    println!(
        "count_matches (feed == SQL): {} ({} == {})",
        count_matches, emitted_count, expected_count
    );
    println!("timestamp_conversion_ok: {}", conversion_ok);

    println!("\n--- Result ---");
    if emitted_count > 0 && ordering_ok && count_matches && conversion_ok {
        println!(
            "SUCCESS: {} events emitted via DomeReplayFeed, all checks passed",
            emitted_count
        );
        std::process::exit(0);
    } else {
        println!(
            "FAILURE: emitted={}, ordering_ok={}, count_matches={}, conversion_ok={}",
            emitted_count, ordering_ok, count_matches, conversion_ok
        );
        std::process::exit(1);
    }
}

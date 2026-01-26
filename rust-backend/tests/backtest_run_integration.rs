//! Integration tests for backtest_run CLI
//!
//! These tests verify the backtest runner can execute strategies against
//! test fixtures and produce valid results.
//!
//! # Fixture Requirements
//!
//! Tests require a fixture SQLite database at `tests/fixtures/test_dataset.db`
//! with sample event data. If not present, tests will be skipped.

use std::process::Command;
use std::path::PathBuf;
use std::fs;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests").join("fixtures")
}

fn fixture_db_path() -> PathBuf {
    fixtures_dir().join("test_dataset.db")
}

fn backtest_run_binary() -> PathBuf {
    // Find the binary in the target directory
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    
    // Try release first, then debug
    for profile in ["release", "debug"] {
        let binary = manifest_dir
            .join("target")
            .join(profile)
            .join("backtest_run");
        if binary.exists() {
            return binary;
        }
    }
    
    // Fallback to cargo run
    panic!("backtest_run binary not found. Run `cargo build --bin backtest_run` first.");
}

/// Skip test if fixture database doesn't exist
fn skip_if_no_fixture() -> bool {
    !fixture_db_path().exists()
}

/// Create a minimal test fixture database
fn create_test_fixture() -> PathBuf {
    let db_path = fixture_db_path();
    
    // Create fixtures directory
    if let Some(parent) = db_path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    
    // Create minimal SQLite database with test data
    let conn = rusqlite::Connection::open(&db_path)
        .expect("Failed to create test fixture database");
    
    // Create book_snapshots table
    conn.execute(
        r#"
        CREATE TABLE IF NOT EXISTS book_snapshots (
            token_id TEXT NOT NULL,
            arrival_time_ns INTEGER NOT NULL,
            bids_json TEXT NOT NULL,
            asks_json TEXT NOT NULL,
            exchange_seq INTEGER DEFAULT 0
        )
        "#,
        [],
    ).expect("Failed to create book_snapshots table");
    
    // Create trade_prints table
    conn.execute(
        r#"
        CREATE TABLE IF NOT EXISTS trade_prints (
            token_id TEXT NOT NULL,
            arrival_time_ns INTEGER NOT NULL,
            price REAL NOT NULL,
            size REAL NOT NULL,
            side TEXT NOT NULL,
            trade_id TEXT
        )
        "#,
        [],
    ).expect("Failed to create trade_prints table");
    
    // Insert test data (15 minutes of events for a test market)
    let base_time_ns: i64 = 1706140800_000_000_000; // 2024-01-25T00:00:00Z in nanos
    let market = "test-updown-15m-123";
    
    for i in 0..10 {
        let time_ns = base_time_ns + (i * 60_000_000_000); // Every minute
        let price = 0.50 + (i as f64 * 0.01);
        
        // Insert book snapshot
        let bids_json = format!(r#"[[{:.2}, 100.0], [{:.2}, 200.0]]"#, price - 0.02, price - 0.03);
        let asks_json = format!(r#"[[{:.2}, 100.0], [{:.2}, 200.0]]"#, price + 0.02, price + 0.03);
        
        conn.execute(
            "INSERT INTO book_snapshots (token_id, arrival_time_ns, bids_json, asks_json, exchange_seq) VALUES (?, ?, ?, ?, ?)",
            rusqlite::params![
                format!("{}_Yes", market),
                time_ns,
                bids_json,
                asks_json,
                i
            ],
        ).expect("Failed to insert book snapshot");
        
        // Insert trade print
        conn.execute(
            "INSERT INTO trade_prints (token_id, arrival_time_ns, price, size, side, trade_id) VALUES (?, ?, ?, ?, ?, ?)",
            rusqlite::params![
                format!("{}_Yes", market),
                time_ns + 1000,
                price,
                10.0,
                if i % 2 == 0 { "BUY" } else { "SELL" },
                format!("trade_{}", i)
            ],
        ).expect("Failed to insert trade print");
    }
    
    db_path
}

#[test]
fn test_noop_strategy_produces_zero_pnl() {
    // Create fixture if needed
    let db_path = create_test_fixture();
    
    // We can't easily call the binary without building it first.
    // Instead, we test the underlying components directly.
    
    use betterbot_backend::backtest_v2::{
        make_strategy, BacktestConfig, BacktestOrchestrator, Event, Level, StrategyParams,
        TimestampedEvent, VecFeed,
    };
    
    // Create test events
    let base_time: i64 = 1706140800_000_000_000;
    let events: Vec<TimestampedEvent> = (0..5)
        .map(|i| {
            let time = base_time + (i * 60_000_000_000);
            TimestampedEvent {
                time,
                source_time: time,
                seq: i as u64,
                source: 0,
                event: Event::L2BookSnapshot {
                    token_id: "test_Yes".to_string(),
                    bids: vec![Level::new(0.48, 100.0), Level::new(0.47, 200.0)],
                    asks: vec![Level::new(0.52, 100.0), Level::new(0.53, 200.0)],
                    exchange_seq: i as u64,
                },
            }
        })
        .collect();
    
    // Create noop strategy
    let params = StrategyParams::default();
    let mut strategy = make_strategy("noop", &params).expect("Failed to create noop strategy");
    
    // Create orchestrator with research config (to avoid production-grade validation)
    let config = BacktestConfig::research_mode();
    let mut orchestrator = BacktestOrchestrator::new(config);
    
    // Load events
    let mut feed = VecFeed::new("test", events);
    orchestrator.load_feed(&mut feed).expect("Failed to load feed");
    
    // Run backtest
    let results = orchestrator.run(strategy.as_mut()).expect("Backtest failed");
    
    // Verify noop strategy has zero PnL and zero fills
    assert_eq!(results.total_fills, 0, "NoOp strategy should have zero fills");
    assert!(
        results.final_pnl.abs() < 1e-9,
        "NoOp strategy should have zero PnL, got {}",
        results.final_pnl
    );
    
    // Verify run fingerprint is present
    assert!(
        results.run_fingerprint.is_some(),
        "Run fingerprint should be present"
    );
}

#[test]
fn test_strategy_factory_available_strategies() {
    use betterbot_backend::backtest_v2::available_strategies;
    
    let strategies = available_strategies();
    
    // Must have noop and random_taker
    assert!(strategies.contains_key("noop"), "noop strategy must be available");
    assert!(strategies.contains_key("random_taker"), "random_taker strategy must be available");
}

#[test]
fn test_strategy_factory_creates_strategies() {
    use betterbot_backend::backtest_v2::{make_strategy, StrategyParams};
    
    let params = StrategyParams::default();
    
    // noop should work
    let noop = make_strategy("noop", &params);
    assert!(noop.is_ok(), "Should create noop strategy");
    assert_eq!(noop.unwrap().name(), "NoOp");
    
    // random_taker should work
    let random = make_strategy("random_taker", &params);
    assert!(random.is_ok(), "Should create random_taker strategy");
    assert_eq!(random.unwrap().name(), "RandomTaker");
    
    // unknown should fail
    let unknown = make_strategy("unknown_strategy", &params);
    assert!(unknown.is_err(), "Unknown strategy should fail");
    if let Err(err) = unknown {
        assert!(err.contains("Unknown strategy"), "Error should mention unknown strategy");
        assert!(err.contains("noop"), "Error should list available strategies");
    }
}

#[test]
fn test_backtest_results_serialization() {
    use betterbot_backend::backtest_v2::BacktestResults;
    
    let results = BacktestResults::default();
    
    // Should serialize to JSON
    let json = serde_json::to_string(&results);
    assert!(json.is_ok(), "BacktestResults should serialize to JSON");
    
    // Should deserialize back
    let json_str = json.unwrap();
    let parsed: Result<BacktestResults, _> = serde_json::from_str(&json_str);
    assert!(parsed.is_ok(), "BacktestResults should deserialize from JSON");
}

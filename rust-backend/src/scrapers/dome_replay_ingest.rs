//! Dome Replay Data Ingestion Module
//!
//! Ingests real public Polymarket BTC 15-minute Up/Down data from DomeAPI
//! with strict invariant enforcement for backtesting.

use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::Duration;

use super::dome_rest::{DomeRestClient, OrdersFilter};

// ============================================================================
// EPOCH <-> ISO CONVERSION (REQUIREMENT 1)
// ============================================================================

/// Convert epoch milliseconds to ISO-8601 UTC string (Z suffix only)
pub fn epoch_ms_to_iso_utc(ms: i64) -> String {
    let secs = ms / 1000;
    let millis = (ms % 1000) as u32;
    let dt = chrono::DateTime::from_timestamp(secs, millis * 1_000_000)
        .expect("valid timestamp");
    dt.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}

/// Parse ISO-8601 UTC string (Z suffix) back to epoch milliseconds
pub fn iso_utc_to_epoch_ms(s: &str) -> Result<i64> {
    let dt = chrono::DateTime::parse_from_rfc3339(s)
        .with_context(|| format!("Failed to parse ISO: {}", s))?;
    Ok(dt.timestamp_millis())
}

/// Validate that a token ID is a non-empty string of digits only
pub fn is_valid_token_id(token_id: &str) -> bool {
    !token_id.is_empty() 
        && token_id != "..."
        && token_id.chars().all(|c| c.is_ascii_digit())
}

// ============================================================================
// DATA STRUCTURES
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngestWindow {
    pub start_ms: i64,
    pub end_ms: i64,
    pub start_iso_utc: String,
    pub end_iso_utc: String,
    pub start_s: i64,
    pub end_s: i64,
    pub margin_ms: i64,
    pub start_with_margin_ms: i64,
    pub end_with_margin_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketInfo {
    pub market_slug: String,
    pub boundary_epoch_seconds: i64,
    pub up_token_id: String,
    pub down_token_id: String,
    pub orders_count_in_true_window: i64,
    pub min_order_ts_ms: Option<i64>,
    pub max_order_ts_ms: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderbookInfo {
    pub token_id: String,
    pub market_slug: String,
    pub side: String,
    pub snapshots_count_in_true_window: i64,
    pub min_snapshot_ts_ms: Option<i64>,
    pub max_snapshot_ts_ms: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SampleOrder {
    pub market_slug: String,
    pub token_id: String,
    pub token_label: String,
    pub timestamp_s: i64,
    pub timestamp_ms: i64,
    pub tx_hash: String,
    pub price: f64,
    pub shares_normalized: f64,
    pub side: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SampleOrderbook {
    pub token_id: String,
    pub timestamp_ms: i64,
    pub best_bid: Option<f64>,
    pub best_ask: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GlobalStats {
    pub total_orders: i64,
    pub total_snapshots: i64,
    pub global_min_ts_ms: Option<i64>,
    pub global_max_ts_ms: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedCounts {
    pub orders_in_db: i64,
    pub snapshots_in_db: i64,
    pub orders_in_db_true_window: i64,
    pub snapshots_in_db_true_window: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngestReceipt {
    pub errors: Vec<String>,
    pub window: IngestWindow,
    pub markets: Vec<MarketInfo>,
    pub orderbooks: Vec<OrderbookInfo>,
    pub global: GlobalStats,
    pub sample_order: Option<SampleOrder>,
    pub sample_orderbook: Option<SampleOrderbook>,
    pub persisted_counts: PersistedCounts,
    pub invariant_checks_passed: bool,
    pub db_path: String,
}

// ============================================================================
// DATABASE SCHEMA
// ============================================================================

const SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS dome_orders (
    market_slug TEXT NOT NULL,
    token_id TEXT NOT NULL,
    token_label TEXT,
    timestamp_s INTEGER NOT NULL,
    timestamp_ms INTEGER NOT NULL,
    tx_hash TEXT NOT NULL,
    price REAL NOT NULL,
    shares_normalized REAL NOT NULL,
    side TEXT NOT NULL,
    PRIMARY KEY (tx_hash, token_id, timestamp_s, price, shares_normalized)
);

CREATE TABLE IF NOT EXISTS dome_orderbooks (
    token_id TEXT NOT NULL,
    timestamp_ms INTEGER NOT NULL,
    best_bid REAL,
    best_bid_size REAL,
    best_ask REAL,
    best_ask_size REAL,
    depth_json TEXT,
    PRIMARY KEY (token_id, timestamp_ms)
);

CREATE INDEX IF NOT EXISTS idx_orders_ts ON dome_orders(timestamp_ms);
CREATE INDEX IF NOT EXISTS idx_orderbooks_ts ON dome_orderbooks(timestamp_ms);
"#;

// ============================================================================
// INGESTION ENGINE
// ============================================================================

pub struct DomeReplayIngestor {
    client: DomeRestClient,
    db_path: String,
    window: IngestWindow,
    errors: Vec<String>,
}

impl DomeReplayIngestor {
    pub fn new(api_key: String, db_path: &str, start_ms: i64, end_ms: i64, margin_ms: i64) -> Result<Self> {
        let client = DomeRestClient::new(api_key)?;
        
        let start_s = start_ms / 1000;
        let end_s = end_ms / 1000;
        let start_with_margin_ms = start_ms - margin_ms;
        let end_with_margin_ms = end_ms + margin_ms;
        
        // Derive ISO from epoch (REQUIREMENT 1)
        let start_iso_utc = epoch_ms_to_iso_utc(start_ms);
        let end_iso_utc = epoch_ms_to_iso_utc(end_ms);
        
        let window = IngestWindow {
            start_ms,
            end_ms,
            start_iso_utc,
            end_iso_utc,
            start_s,
            end_s,
            margin_ms,
            start_with_margin_ms,
            end_with_margin_ms,
        };
        
        Ok(Self {
            client,
            db_path: db_path.to_string(),
            window,
            errors: Vec::new(),
        })
    }
    
    fn validate_iso_roundtrip(&mut self) -> bool {
        // REQUIREMENT 1: Round-trip validation
        let parsed_start = match iso_utc_to_epoch_ms(&self.window.start_iso_utc) {
            Ok(v) => v,
            Err(e) => {
                self.errors.push(format!("iso_parse_failed:start:{}", e));
                return false;
            }
        };
        let parsed_end = match iso_utc_to_epoch_ms(&self.window.end_iso_utc) {
            Ok(v) => v,
            Err(e) => {
                self.errors.push(format!("iso_parse_failed:end:{}", e));
                return false;
            }
        };
        
        if parsed_start != self.window.start_ms || parsed_end != self.window.end_ms {
            self.errors.push(format!(
                "iso_epoch_mismatch:start_parsed={}vs{},end_parsed={}vs{}",
                parsed_start, self.window.start_ms, parsed_end, self.window.end_ms
            ));
            return false;
        }
        true
    }
    
    fn validate_window_units(&mut self) -> bool {
        // REQUIREMENT 4: Validate seconds vs milliseconds consistency
        let expected_diff_ms = (self.window.end_s - self.window.start_s) * 1000;
        let actual_diff_ms = self.window.end_ms - self.window.start_ms;
        let remainder = actual_diff_ms - expected_diff_ms;
        
        // remainder should be 0 for clean second boundaries, or small for sub-second
        if remainder.abs() > 999 {
            self.errors.push(format!(
                "window_units_mismatch:diff_ms={},expected={},remainder={}",
                actual_diff_ms, expected_diff_ms, remainder
            ));
            return false;
        }
        true
    }
    
    pub async fn run(&mut self) -> Result<IngestReceipt> {
        // Validate invariants first
        let iso_ok = self.validate_iso_roundtrip();
        let units_ok = self.validate_window_units();
        
        if !iso_ok || !units_ok {
            return Ok(self.build_failed_receipt());
        }
        
        // Initialize database
        let conn = self.init_db()?;
        
        // Step 1: Enumerate market slugs
        let boundaries = self.enumerate_boundaries();
        let valid_slugs = self.verify_slugs(&boundaries).await?;
        
        // Step 2+3: Resolve tokens and persist orders
        let (markets, order_keys) = self.fetch_and_persist_orders(&conn, &valid_slugs).await?;
        
        // Step 4: Fetch and persist orderbooks
        let (orderbooks, snap_keys) = self.fetch_and_persist_orderbooks(&conn, &markets).await?;
        
        // Query DB counts for true window (REQUIREMENT 3)
        let db_orders_true = conn.query_row(
            "SELECT COUNT(*) FROM dome_orders WHERE timestamp_ms >= ? AND timestamp_ms <= ?",
            params![self.window.start_ms, self.window.end_ms],
            |row| row.get::<_, i64>(0),
        )?;
        
        let db_snaps_true = conn.query_row(
            "SELECT COUNT(*) FROM dome_orderbooks WHERE timestamp_ms >= ? AND timestamp_ms <= ?",
            params![self.window.start_ms, self.window.end_ms],
            |row| row.get::<_, i64>(0),
        )?;
        
        let db_orders_total: i64 = conn.query_row("SELECT COUNT(*) FROM dome_orders", [], |row| row.get(0))?;
        let db_snaps_total: i64 = conn.query_row("SELECT COUNT(*) FROM dome_orderbooks", [], |row| row.get(0))?;
        
        // Compute in-memory true window counts
        let mem_orders_true = order_keys.iter()
            .filter(|(_, _, ts_s, _, _)| {
                let ts_ms = ts_s * 1000;
                ts_ms >= self.window.start_ms && ts_ms <= self.window.end_ms
            })
            .count() as i64;
        
        let mem_snaps_true = snap_keys.iter()
            .filter(|(_, ts_ms)| *ts_ms >= self.window.start_ms && *ts_ms <= self.window.end_ms)
            .count() as i64;
        
        // REQUIREMENT 3: Verify counts match
        if db_orders_true != mem_orders_true {
            self.errors.push(format!(
                "db_count_mismatch:orders mem={} db={}",
                mem_orders_true, db_orders_true
            ));
        }
        if db_snaps_true != mem_snaps_true {
            self.errors.push(format!(
                "db_count_mismatch:snapshots mem={} db={}",
                mem_snaps_true, db_snaps_true
            ));
        }
        
        // Sample order from DB (must be in true window)
        let sample_order = self.get_sample_order(&conn)?;
        let sample_orderbook = self.get_sample_orderbook(&conn)?;
        
        // Compute global stats from DB (source of truth)
        let global = self.compute_global_stats(&conn)?;
        
        let persisted_counts = PersistedCounts {
            orders_in_db: db_orders_total,
            snapshots_in_db: db_snaps_total,
            orders_in_db_true_window: db_orders_true,
            snapshots_in_db_true_window: db_snaps_true,
        };
        
        let invariant_checks_passed = self.errors.is_empty();
        
        Ok(IngestReceipt {
            errors: self.errors.clone(),
            window: self.window.clone(),
            markets,
            orderbooks,
            global,
            sample_order,
            sample_orderbook,
            persisted_counts,
            invariant_checks_passed,
            db_path: std::fs::canonicalize(&self.db_path)
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|_| self.db_path.clone()),
        })
    }
    
    fn init_db(&self) -> Result<Connection> {
        // Remove existing DB for clean ingestion
        if Path::new(&self.db_path).exists() {
            std::fs::remove_file(&self.db_path)?;
        }
        
        let conn = Connection::open(&self.db_path)?;
        conn.execute_batch(SCHEMA_SQL)?;
        Ok(conn)
    }
    
    fn enumerate_boundaries(&self) -> Vec<i64> {
        let mut boundaries = Vec::new();
        let mut b = ((self.window.start_s / 900) - 1) * 900;
        while b < self.window.end_s + 900 {
            // Check if [b, b+900) intersects [start_s, end_s]
            if b < self.window.end_s && (b + 900) > self.window.start_s {
                boundaries.push(b);
            }
            b += 900;
        }
        boundaries
    }
    
    async fn verify_slugs(&mut self, boundaries: &[i64]) -> Result<Vec<(String, i64)>> {
        let mut valid = Vec::new();
        
        for &boundary in boundaries {
            let slug = format!("btc-updown-15m-{}", boundary);
            
            // Check if any orders exist for this slug in window
            let resp = self.client.get_orders(
                OrdersFilter {
                    market_slug: Some(slug.clone()),
                    ..Default::default()
                },
                Some(self.window.start_s),
                Some(self.window.end_s),
                Some(1),
                None,
            ).await;
            
            match resp {
                Ok(r) => {
                    if let Some(pag) = r.pagination {
                        if pag.total.unwrap_or(0) > 0 {
                            valid.push((slug, boundary));
                        }
                    }
                }
                Err(e) => {
                    self.errors.push(format!("slug_check_failed:{}:{}", slug, e));
                }
            }
            
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        
        Ok(valid)
    }
    
    async fn fetch_and_persist_orders(
        &mut self,
        conn: &Connection,
        slugs: &[(String, i64)],
    ) -> Result<(Vec<MarketInfo>, HashSet<(String, String, i64, i64, i64)>)> {
        let mut markets = Vec::new();
        let mut all_keys: HashSet<(String, String, i64, i64, i64)> = HashSet::new();
        
        for (slug, boundary) in slugs {
            // Fetch all orders for this slug
            let orders = self.client.get_all_orders_for_market(
                slug,
                Some(self.window.start_s),
                Some(self.window.end_s),
            ).await?;
            
            // Collect tokens (REQUIREMENT 2)
            let mut tokens: HashMap<String, String> = HashMap::new();
            for o in &orders {
                if let Some(label) = &o.token_label {
                    if !tokens.contains_key(label) {
                        // Validate token ID
                        if !is_valid_token_id(&o.token_id) {
                            self.errors.push(format!("invalid_token_id:{}", o.token_id));
                            continue;
                        }
                        tokens.insert(label.clone(), o.token_id.clone());
                    }
                }
            }
            
            let up_token = tokens.get("Up").cloned();
            let down_token = tokens.get("Down").cloned();
            
            // Validate exactly two tokens
            if up_token.is_none() || down_token.is_none() {
                self.errors.push(format!(
                    "token_resolution_failed:{}:up={:?},down={:?}",
                    slug, up_token, down_token
                ));
                continue;
            }
            
            let up_token = up_token.unwrap();
            let down_token = down_token.unwrap();
            
            // Final validation
            if !is_valid_token_id(&up_token) {
                self.errors.push(format!("invalid_token_id:{}", up_token));
                continue;
            }
            if !is_valid_token_id(&down_token) {
                self.errors.push(format!("invalid_token_id:{}", down_token));
                continue;
            }
            
            // Persist orders
            let mut in_true_window = 0i64;
            let mut min_ts: Option<i64> = None;
            let mut max_ts: Option<i64> = None;
            
            for o in &orders {
                let ts_s = o.timestamp;
                let ts_ms = ts_s * 1000;
                
                // Create unique key (using price and shares as integers for hashing)
                let price_key = (o.price * 1_000_000.0) as i64;
                let shares_key = (o.shares_normalized * 1_000_000.0) as i64;
                let key = (o.tx_hash.clone(), o.token_id.clone(), ts_s, price_key, shares_key);
                
                if all_keys.contains(&key) {
                    continue; // Skip duplicate
                }
                all_keys.insert(key);
                
                // Insert (will ignore duplicates due to PRIMARY KEY)
                conn.execute(
                    "INSERT OR IGNORE INTO dome_orders VALUES (?,?,?,?,?,?,?,?,?)",
                    params![
                        o.market_slug,
                        o.token_id,
                        o.token_label,
                        ts_s,
                        ts_ms,
                        o.tx_hash,
                        o.price,
                        o.shares_normalized,
                        o.side,
                    ],
                )?;
                
                // Track true window stats
                if ts_ms >= self.window.start_ms && ts_ms <= self.window.end_ms {
                    in_true_window += 1;
                    min_ts = Some(min_ts.map_or(ts_ms, |m| m.min(ts_ms)));
                    max_ts = Some(max_ts.map_or(ts_ms, |m| m.max(ts_ms)));
                }
            }
            
            markets.push(MarketInfo {
                market_slug: slug.clone(),
                boundary_epoch_seconds: *boundary,
                up_token_id: up_token,
                down_token_id: down_token,
                orders_count_in_true_window: in_true_window,
                min_order_ts_ms: min_ts,
                max_order_ts_ms: max_ts,
            });
        }
        
        Ok((markets, all_keys))
    }
    
    async fn fetch_and_persist_orderbooks(
        &mut self,
        conn: &Connection,
        markets: &[MarketInfo],
    ) -> Result<(Vec<OrderbookInfo>, HashSet<(String, i64)>)> {
        let mut orderbooks = Vec::new();
        let mut all_keys: HashSet<(String, i64)> = HashSet::new();
        
        // Collect all token IDs
        let mut tokens: Vec<(String, String, String)> = Vec::new();
        for m in markets {
            tokens.push((m.up_token_id.clone(), m.market_slug.clone(), "Up".to_string()));
            tokens.push((m.down_token_id.clone(), m.market_slug.clone(), "Down".to_string()));
        }
        
        for (token_id, market_slug, side) in tokens {
            if !is_valid_token_id(&token_id) {
                self.errors.push(format!("invalid_token_for_orderbook:{}", token_id));
                continue;
            }
            
            // Fetch with retry (REQUIREMENT 5)
            let snapshots = self.fetch_orderbooks_with_retry(&token_id).await;
            
            let snapshots = match snapshots {
                Ok(s) => s,
                Err(e) => {
                    self.errors.push(format!("orderbook_fetch_failed:{}:{}", token_id, e));
                    continue;
                }
            };
            
            let mut in_true_window = 0i64;
            let mut min_ts: Option<i64> = None;
            let mut max_ts: Option<i64> = None;
            
            for snap in &snapshots {
                let ts_ms = snap.timestamp;
                let key = (token_id.clone(), ts_ms);
                
                if all_keys.contains(&key) {
                    continue;
                }
                all_keys.insert(key);
                
                let bids = &snap.bids;
                let asks = &snap.asks;
                
                let best_bid: Option<f64> = bids.first().and_then(|b| b.price.parse().ok());
                let best_bid_size: Option<f64> = bids.first().and_then(|b| b.size.parse().ok());
                let best_ask: Option<f64> = asks.first().and_then(|a| a.price.parse().ok());
                let best_ask_size: Option<f64> = asks.first().and_then(|a| a.size.parse().ok());
                
                // Top 20 depth
                let depth: Vec<((f64, f64), (f64, f64))> = bids.iter().take(20)
                    .zip(asks.iter().take(20))
                    .filter_map(|(b, a)| {
                        let bp: f64 = b.price.parse().ok()?;
                        let bs: f64 = b.size.parse().ok()?;
                        let ap: f64 = a.price.parse().ok()?;
                        let as_: f64 = a.size.parse().ok()?;
                        Some(((bp, bs), (ap, as_)))
                    })
                    .collect();
                
                let depth_json = serde_json::to_string(&depth).unwrap_or_default();
                
                conn.execute(
                    "INSERT OR IGNORE INTO dome_orderbooks VALUES (?,?,?,?,?,?,?)",
                    params![token_id, ts_ms, best_bid, best_bid_size, best_ask, best_ask_size, depth_json],
                )?;
                
                if ts_ms >= self.window.start_ms && ts_ms <= self.window.end_ms {
                    in_true_window += 1;
                    min_ts = Some(min_ts.map_or(ts_ms, |m| m.min(ts_ms)));
                    max_ts = Some(max_ts.map_or(ts_ms, |m| m.max(ts_ms)));
                }
            }
            
            orderbooks.push(OrderbookInfo {
                token_id: token_id.clone(),
                market_slug,
                side,
                snapshots_count_in_true_window: in_true_window,
                min_snapshot_ts_ms: min_ts,
                max_snapshot_ts_ms: max_ts,
            });
        }
        
        Ok((orderbooks, all_keys))
    }
    
    async fn fetch_orderbooks_with_retry(
        &self,
        token_id: &str,
    ) -> Result<Vec<super::dome_rest::OrderbookSnapshot>> {
        let mut all_snapshots = Vec::new();
        let mut pagination_key: Option<String> = None;
        
        for _page in 0..200 {
            let mut last_error = None;
            
            // Retry loop (REQUIREMENT 5)
            for attempt in 0..6 {
                let result = self.client.get_orderbooks(
                    token_id,
                    self.window.start_with_margin_ms,
                    self.window.end_with_margin_ms,
                    Some(200),
                    pagination_key.clone(),
                ).await;
                
                match result {
                    Ok(resp) => {
                        let count = resp.snapshots.len();
                        all_snapshots.extend(resp.snapshots);
                        pagination_key = resp.pagination.and_then(|p| p.pagination_key);
                        
                        if count < 200 || pagination_key.is_none() {
                            return Ok(all_snapshots);
                        }
                        break;
                    }
                    Err(e) => {
                        let err_str = e.to_string();
                        if err_str.contains("502") || err_str.contains("503") || err_str.contains("504") {
                            // Exponential backoff with jitter
                            let backoff = Duration::from_millis(
                                (1 << attempt) * 100 + rand::random::<u64>() % 100
                            );
                            tokio::time::sleep(backoff).await;
                            last_error = Some(e);
                            continue;
                        }
                        return Err(e);
                    }
                }
            }
            
            if let Some(e) = last_error {
                return Err(e);
            }
            
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        
        Ok(all_snapshots)
    }
    
    fn get_sample_order(&self, conn: &Connection) -> Result<Option<SampleOrder>> {
        let mut stmt = conn.prepare(
            "SELECT market_slug, token_id, token_label, timestamp_s, timestamp_ms, tx_hash, price, shares_normalized, side 
             FROM dome_orders WHERE timestamp_ms >= ? AND timestamp_ms <= ? LIMIT 1"
        )?;
        
        let result = stmt.query_row(params![self.window.start_ms, self.window.end_ms], |row| {
            Ok(SampleOrder {
                market_slug: row.get(0)?,
                token_id: row.get(1)?,
                token_label: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                timestamp_s: row.get(3)?,
                timestamp_ms: row.get(4)?,
                tx_hash: row.get(5)?,
                price: row.get(6)?,
                shares_normalized: row.get(7)?,
                side: row.get(8)?,
            })
        });
        
        match result {
            Ok(s) => Ok(Some(s)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
    
    fn get_sample_orderbook(&self, conn: &Connection) -> Result<Option<SampleOrderbook>> {
        let mut stmt = conn.prepare(
            "SELECT token_id, timestamp_ms, best_bid, best_ask 
             FROM dome_orderbooks WHERE timestamp_ms >= ? AND timestamp_ms <= ? LIMIT 1"
        )?;
        
        let result = stmt.query_row(params![self.window.start_ms, self.window.end_ms], |row| {
            Ok(SampleOrderbook {
                token_id: row.get(0)?,
                timestamp_ms: row.get(1)?,
                best_bid: row.get(2)?,
                best_ask: row.get(3)?,
            })
        });
        
        match result {
            Ok(s) => Ok(Some(s)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
    
    fn compute_global_stats(&self, conn: &Connection) -> Result<GlobalStats> {
        let total_orders: i64 = conn.query_row(
            "SELECT COUNT(*) FROM dome_orders WHERE timestamp_ms >= ? AND timestamp_ms <= ?",
            params![self.window.start_ms, self.window.end_ms],
            |row| row.get(0),
        )?;
        
        let total_snapshots: i64 = conn.query_row(
            "SELECT COUNT(*) FROM dome_orderbooks WHERE timestamp_ms >= ? AND timestamp_ms <= ?",
            params![self.window.start_ms, self.window.end_ms],
            |row| row.get(0),
        )?;
        
        let min_order: Option<i64> = conn.query_row(
            "SELECT MIN(timestamp_ms) FROM dome_orders WHERE timestamp_ms >= ? AND timestamp_ms <= ?",
            params![self.window.start_ms, self.window.end_ms],
            |row| row.get(0),
        ).ok();
        
        let max_order: Option<i64> = conn.query_row(
            "SELECT MAX(timestamp_ms) FROM dome_orders WHERE timestamp_ms >= ? AND timestamp_ms <= ?",
            params![self.window.start_ms, self.window.end_ms],
            |row| row.get(0),
        ).ok();
        
        let min_snap: Option<i64> = conn.query_row(
            "SELECT MIN(timestamp_ms) FROM dome_orderbooks WHERE timestamp_ms >= ? AND timestamp_ms <= ?",
            params![self.window.start_ms, self.window.end_ms],
            |row| row.get(0),
        ).ok();
        
        let max_snap: Option<i64> = conn.query_row(
            "SELECT MAX(timestamp_ms) FROM dome_orderbooks WHERE timestamp_ms >= ? AND timestamp_ms <= ?",
            params![self.window.start_ms, self.window.end_ms],
            |row| row.get(0),
        ).ok();
        
        let global_min = [min_order, min_snap].into_iter().flatten().min();
        let global_max = [max_order, max_snap].into_iter().flatten().max();
        
        Ok(GlobalStats {
            total_orders,
            total_snapshots,
            global_min_ts_ms: global_min,
            global_max_ts_ms: global_max,
        })
    }
    
    fn build_failed_receipt(&self) -> IngestReceipt {
        IngestReceipt {
            errors: self.errors.clone(),
            window: self.window.clone(),
            markets: Vec::new(),
            orderbooks: Vec::new(),
            global: GlobalStats {
                total_orders: 0,
                total_snapshots: 0,
                global_min_ts_ms: None,
                global_max_ts_ms: None,
            },
            sample_order: None,
            sample_orderbook: None,
            persisted_counts: PersistedCounts {
                orders_in_db: 0,
                snapshots_in_db: 0,
                orders_in_db_true_window: 0,
                snapshots_in_db_true_window: 0,
            },
            invariant_checks_passed: false,
            db_path: self.db_path.clone(),
        }
    }
}

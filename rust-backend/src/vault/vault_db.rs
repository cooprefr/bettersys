use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, sync::Arc};
use tokio::sync::Mutex;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultActivityRecord {
    pub id: String,
    pub ts: i64,
    pub kind: String,
    pub wallet_address: Option<String>,
    pub amount_usdc: Option<f64>,
    pub shares: Option<f64>,
    pub token_id: Option<String>,
    pub market_slug: Option<String>,
    pub outcome: Option<String>,
    pub side: Option<String>,
    pub price: Option<f64>,
    pub notional_usdc: Option<f64>,
    pub strategy: Option<String>,
    pub decision_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultNavSnapshotRecord {
    pub id: String,
    pub ts: i64,
    pub nav_usdc: f64,
    pub cash_usdc: f64,
    pub positions_value_usdc: f64,
    pub total_shares: f64,
    pub nav_per_share: f64,
    pub source: String,
}

#[derive(Debug, Clone)]
pub struct VaultTokenMeta {
    pub market_slug: String,
    pub strategy: Option<String>,
    pub decision_id: Option<String>,
}

#[derive(Clone)]
pub struct VaultDb {
    conn: Arc<Mutex<Connection>>,
}

impl VaultDb {
    pub fn new(db_path: &str) -> Result<Self> {
        let conn = Connection::open(db_path).context("open vault db")?;
        conn.pragma_update(None, "journal_mode", "WAL").ok();
        conn.pragma_update(None, "synchronous", "NORMAL").ok();

        conn.execute(
            "CREATE TABLE IF NOT EXISTS vault_state (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                cash_usdc REAL NOT NULL,
                total_shares REAL NOT NULL,
                updated_at INTEGER NOT NULL
            )",
            [],
        )?;
        conn.execute(
            "CREATE TABLE IF NOT EXISTS vault_user_shares (
                wallet_address TEXT PRIMARY KEY,
                shares REAL NOT NULL,
                updated_at INTEGER NOT NULL
            )",
            [],
        )?;

        conn.execute(
            "CREATE TABLE IF NOT EXISTS vault_activity (
                id TEXT PRIMARY KEY,
                ts INTEGER NOT NULL,
                kind TEXT NOT NULL,
                wallet_address TEXT,
                amount_usdc REAL,
                shares REAL,
                token_id TEXT,
                market_slug TEXT,
                outcome TEXT,
                side TEXT,
                price REAL,
                notional_usdc REAL,
                strategy TEXT,
                decision_id TEXT
            )",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_vault_activity_ts ON vault_activity(ts DESC)",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_vault_activity_wallet_ts ON vault_activity(wallet_address, ts DESC)",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_vault_activity_token_ts ON vault_activity(token_id, ts DESC)",
            [],
        )?;

        conn.execute(
            "CREATE TABLE IF NOT EXISTS vault_nav_snapshots (
                id TEXT PRIMARY KEY,
                ts INTEGER NOT NULL,
                nav_usdc REAL NOT NULL,
                cash_usdc REAL NOT NULL,
                positions_value_usdc REAL NOT NULL,
                total_shares REAL NOT NULL,
                nav_per_share REAL NOT NULL,
                source TEXT NOT NULL
            )",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_vault_nav_snapshots_ts ON vault_nav_snapshots(ts ASC)",
            [],
        )?;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    pub async fn load_state(&self) -> Result<(f64, f64)> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare_cached(
            "SELECT cash_usdc, total_shares FROM vault_state WHERE id = 1 LIMIT 1",
        )?;
        let mut rows = stmt.query([])?;
        if let Some(row) = rows.next()? {
            let cash: f64 = row.get(0)?;
            let total: f64 = row.get(1)?;
            Ok((cash, total))
        } else {
            Ok((0.0, 0.0))
        }
    }

    pub async fn upsert_state(
        &self,
        cash_usdc: f64,
        total_shares: f64,
        updated_at: i64,
    ) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO vault_state (id, cash_usdc, total_shares, updated_at)
             VALUES (1, ?1, ?2, ?3)
             ON CONFLICT(id) DO UPDATE SET
                cash_usdc = excluded.cash_usdc,
                total_shares = excluded.total_shares,
                updated_at = excluded.updated_at",
            params![cash_usdc, total_shares, updated_at],
        )?;
        Ok(())
    }

    pub async fn load_user_shares(&self) -> Result<HashMap<String, f64>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare_cached(
            "SELECT wallet_address, shares FROM vault_user_shares ORDER BY wallet_address ASC",
        )?;

        let mut out = HashMap::new();
        let rows = stmt
            .query_map([], |row| {
                let wallet: String = row.get(0)?;
                let shares: f64 = row.get(1)?;
                Ok((wallet, shares))
            })?
            .filter_map(|r| r.ok());

        for (wallet, shares) in rows {
            out.insert(wallet, shares);
        }

        Ok(out)
    }

    pub async fn set_user_shares(
        &self,
        wallet_address: &str,
        shares: f64,
        updated_at: i64,
    ) -> Result<()> {
        let wallet = wallet_address.trim().to_lowercase();
        let conn = self.conn.lock().await;

        if shares <= 0.0 {
            conn.execute(
                "DELETE FROM vault_user_shares WHERE wallet_address = ?1",
                [wallet],
            )?;
            return Ok(());
        }

        conn.execute(
            "INSERT INTO vault_user_shares (wallet_address, shares, updated_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(wallet_address) DO UPDATE SET
                shares = excluded.shares,
                updated_at = excluded.updated_at",
            params![wallet, shares, updated_at],
        )?;

        Ok(())
    }

    pub async fn insert_activity(&self, rec: &VaultActivityRecord) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT OR REPLACE INTO vault_activity \
             (id, ts, kind, wallet_address, amount_usdc, shares, token_id, market_slug, outcome, side, price, notional_usdc, strategy, decision_id) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            params![
                &rec.id,
                rec.ts,
                &rec.kind,
                rec.wallet_address.as_deref(),
                rec.amount_usdc,
                rec.shares,
                rec.token_id.as_deref(),
                rec.market_slug.as_deref(),
                rec.outcome.as_deref(),
                rec.side.as_deref(),
                rec.price,
                rec.notional_usdc,
                rec.strategy.as_deref(),
                rec.decision_id.as_deref(),
            ],
        )?;
        Ok(())
    }

    pub async fn list_activity(
        &self,
        limit: usize,
        wallet_address: Option<&str>,
    ) -> Result<Vec<VaultActivityRecord>> {
        let limit = limit.clamp(1, 1000) as i64;
        let conn = self.conn.lock().await;

        let mut out: Vec<VaultActivityRecord> = Vec::new();
        if let Some(wallet) = wallet_address {
            let wallet = wallet.trim().to_lowercase();
            let mut stmt = conn.prepare_cached(
                "SELECT id, ts, kind, wallet_address, amount_usdc, shares, token_id, market_slug, outcome, side, price, notional_usdc, strategy, decision_id \
                 FROM vault_activity WHERE wallet_address = ?1 ORDER BY ts DESC LIMIT ?2",
            )?;
            let rows = stmt.query_map(params![wallet, limit], |row| {
                Ok(VaultActivityRecord {
                    id: row.get(0)?,
                    ts: row.get(1)?,
                    kind: row.get(2)?,
                    wallet_address: row.get(3)?,
                    amount_usdc: row.get(4)?,
                    shares: row.get(5)?,
                    token_id: row.get(6)?,
                    market_slug: row.get(7)?,
                    outcome: row.get(8)?,
                    side: row.get(9)?,
                    price: row.get(10)?,
                    notional_usdc: row.get(11)?,
                    strategy: row.get(12)?,
                    decision_id: row.get(13)?,
                })
            })?;
            for r in rows {
                if let Ok(v) = r {
                    out.push(v);
                }
            }
            return Ok(out);
        }

        let mut stmt = conn.prepare_cached(
            "SELECT id, ts, kind, wallet_address, amount_usdc, shares, token_id, market_slug, outcome, side, price, notional_usdc, strategy, decision_id \
             FROM vault_activity ORDER BY ts DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit], |row| {
            Ok(VaultActivityRecord {
                id: row.get(0)?,
                ts: row.get(1)?,
                kind: row.get(2)?,
                wallet_address: row.get(3)?,
                amount_usdc: row.get(4)?,
                shares: row.get(5)?,
                token_id: row.get(6)?,
                market_slug: row.get(7)?,
                outcome: row.get(8)?,
                side: row.get(9)?,
                price: row.get(10)?,
                notional_usdc: row.get(11)?,
                strategy: row.get(12)?,
                decision_id: row.get(13)?,
            })
        })?;
        for r in rows {
            if let Ok(v) = r {
                out.push(v);
            }
        }

        Ok(out)
    }

    pub async fn insert_nav_snapshot(&self, snap: &VaultNavSnapshotRecord) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT OR REPLACE INTO vault_nav_snapshots \
             (id, ts, nav_usdc, cash_usdc, positions_value_usdc, total_shares, nav_per_share, source) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                &snap.id,
                snap.ts,
                snap.nav_usdc,
                snap.cash_usdc,
                snap.positions_value_usdc,
                snap.total_shares,
                snap.nav_per_share,
                &snap.source,
            ],
        )?;
        Ok(())
    }

    pub async fn list_nav_snapshots(
        &self,
        start_ts: i64,
        end_ts: Option<i64>,
        limit: usize,
    ) -> Result<Vec<VaultNavSnapshotRecord>> {
        let limit = limit.clamp(1, 20_000) as i64;
        let conn = self.conn.lock().await;
        let mut out: Vec<VaultNavSnapshotRecord> = Vec::new();

        if let Some(end_ts) = end_ts {
            let mut stmt = conn.prepare_cached(
                "SELECT id, ts, nav_usdc, cash_usdc, positions_value_usdc, total_shares, nav_per_share, source \
                 FROM vault_nav_snapshots WHERE ts >= ?1 AND ts <= ?2 ORDER BY ts ASC LIMIT ?3",
            )?;
            let rows = stmt.query_map(params![start_ts, end_ts, limit], |row| {
                Ok(VaultNavSnapshotRecord {
                    id: row.get(0)?,
                    ts: row.get(1)?,
                    nav_usdc: row.get(2)?,
                    cash_usdc: row.get(3)?,
                    positions_value_usdc: row.get(4)?,
                    total_shares: row.get(5)?,
                    nav_per_share: row.get(6)?,
                    source: row.get(7)?,
                })
            })?;
            for r in rows {
                if let Ok(v) = r {
                    out.push(v);
                }
            }
            return Ok(out);
        }

        let mut stmt = conn.prepare_cached(
            "SELECT id, ts, nav_usdc, cash_usdc, positions_value_usdc, total_shares, nav_per_share, source \
             FROM vault_nav_snapshots WHERE ts >= ?1 ORDER BY ts ASC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![start_ts, limit], |row| {
            Ok(VaultNavSnapshotRecord {
                id: row.get(0)?,
                ts: row.get(1)?,
                nav_usdc: row.get(2)?,
                cash_usdc: row.get(3)?,
                positions_value_usdc: row.get(4)?,
                total_shares: row.get(5)?,
                nav_per_share: row.get(6)?,
                source: row.get(7)?,
            })
        })?;
        for r in rows {
            if let Ok(v) = r {
                out.push(v);
            }
        }

        Ok(out)
    }

    pub async fn get_token_meta(&self, token_id: &str) -> Result<Option<VaultTokenMeta>> {
        let token_id = token_id.trim();
        if token_id.is_empty() {
            return Ok(None);
        }
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare_cached(
            "SELECT market_slug, strategy, decision_id \
             FROM vault_activity \
             WHERE kind = 'TRADE' AND token_id = ?1 AND market_slug IS NOT NULL AND market_slug != '' \
             ORDER BY ts DESC LIMIT 1",
        )?;
        let mut rows = stmt.query(params![token_id])?;
        let Some(row) = rows.next()? else {
            return Ok(None);
        };
        let market_slug: String = row.get(0)?;
        let strategy: Option<String> = row.get(1)?;
        let decision_id: Option<String> = row.get(2)?;
        Ok(Some(VaultTokenMeta {
            market_slug,
            strategy,
            decision_id,
        }))
    }
}

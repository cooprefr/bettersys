//! User Account Management
//!
//! Handles user registration, deposits, and balance tracking

use anyhow::Result;
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::Mutex;

/// User account structure
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserAccount {
    pub id: i64,
    pub wallet_address: String,
    pub wallet_type: String, // "metamask" or "phantom"
    pub balance_usdc: f64,
    pub balance_better: f64,
    pub total_deposited: f64,
    pub total_withdrawn: f64,
    pub total_pnl: f64,
    pub trade_count: i64,
    pub win_count: i64,
    pub kelly_fraction: f64, // User's preferred Kelly fraction
    pub auto_trade_enabled: bool,
    pub max_position_pct: f64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Deposit record
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Deposit {
    pub id: i64,
    pub user_id: i64,
    pub tx_hash: String,
    pub amount: f64,
    pub token: String,  // "USDC" or "BETTER"
    pub status: String, // "pending", "confirmed", "failed"
    pub created_at: DateTime<Utc>,
}

/// Trade record
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradeRecord {
    pub id: i64,
    pub user_id: i64,
    pub signal_id: String,
    pub market_slug: String,
    pub side: String,    // "BUY" or "SELL"
    pub outcome: String, // "YES" or "NO"
    pub entry_price: f64,
    pub position_size: f64,
    pub kelly_fraction_used: f64,
    pub status: String, // "pending", "filled", "closed", "expired"
    pub exit_price: Option<f64>,
    pub pnl: Option<f64>,
    pub created_at: DateTime<Utc>,
    pub closed_at: Option<DateTime<Utc>>,
}

/// User accounts database manager
pub struct UserAccountsDB {
    conn: Arc<Mutex<Connection>>,
}

impl UserAccountsDB {
    /// Create new instance and initialize tables
    pub fn new(db_path: &str) -> Result<Self> {
        let conn = Connection::open(db_path)?;

        // Create tables
        conn.execute(
            "CREATE TABLE IF NOT EXISTS user_accounts (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                wallet_address TEXT UNIQUE NOT NULL,
                wallet_type TEXT NOT NULL,
                balance_usdc REAL DEFAULT 0.0,
                balance_better REAL DEFAULT 0.0,
                total_deposited REAL DEFAULT 0.0,
                total_withdrawn REAL DEFAULT 0.0,
                total_pnl REAL DEFAULT 0.0,
                trade_count INTEGER DEFAULT 0,
                win_count INTEGER DEFAULT 0,
                kelly_fraction REAL DEFAULT 0.25,
                auto_trade_enabled INTEGER DEFAULT 0,
                max_position_pct REAL DEFAULT 0.10,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            )",
            [],
        )?;

        conn.execute(
            "CREATE TABLE IF NOT EXISTS deposits (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                user_id INTEGER NOT NULL,
                tx_hash TEXT UNIQUE NOT NULL,
                amount REAL NOT NULL,
                token TEXT NOT NULL,
                status TEXT DEFAULT 'pending',
                created_at TEXT NOT NULL,
                FOREIGN KEY (user_id) REFERENCES user_accounts(id)
            )",
            [],
        )?;

        conn.execute(
            "CREATE TABLE IF NOT EXISTS trades (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                user_id INTEGER NOT NULL,
                signal_id TEXT NOT NULL,
                market_slug TEXT NOT NULL,
                side TEXT NOT NULL,
                outcome TEXT NOT NULL,
                entry_price REAL NOT NULL,
                position_size REAL NOT NULL,
                kelly_fraction_used REAL NOT NULL,
                status TEXT DEFAULT 'pending',
                exit_price REAL,
                pnl REAL,
                created_at TEXT NOT NULL,
                closed_at TEXT,
                FOREIGN KEY (user_id) REFERENCES user_accounts(id)
            )",
            [],
        )?;

        // Create indexes
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_deposits_user ON deposits(user_id)",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_trades_user ON trades(user_id)",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_trades_signal ON trades(signal_id)",
            [],
        )?;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Get or create user account by wallet address
    pub async fn get_or_create_user(
        &self,
        wallet_address: &str,
        wallet_type: &str,
    ) -> Result<UserAccount> {
        let conn = self.conn.lock().await;
        let now = Utc::now().to_rfc3339();

        // Try to get existing
        let result: Result<UserAccount, _> = conn.query_row(
            "SELECT * FROM user_accounts WHERE wallet_address = ?",
            [wallet_address],
            |row| {
                Ok(UserAccount {
                    id: row.get(0)?,
                    wallet_address: row.get(1)?,
                    wallet_type: row.get(2)?,
                    balance_usdc: row.get(3)?,
                    balance_better: row.get(4)?,
                    total_deposited: row.get(5)?,
                    total_withdrawn: row.get(6)?,
                    total_pnl: row.get(7)?,
                    trade_count: row.get(8)?,
                    win_count: row.get(9)?,
                    kelly_fraction: row.get(10)?,
                    auto_trade_enabled: row.get::<_, i64>(11)? == 1,
                    max_position_pct: row.get(12)?,
                    created_at: DateTime::parse_from_rfc3339(&row.get::<_, String>(13)?)
                        .unwrap()
                        .with_timezone(&Utc),
                    updated_at: DateTime::parse_from_rfc3339(&row.get::<_, String>(14)?)
                        .unwrap()
                        .with_timezone(&Utc),
                })
            },
        );

        match result {
            Ok(user) => Ok(user),
            Err(_) => {
                // Create new user
                conn.execute(
                    "INSERT INTO user_accounts (wallet_address, wallet_type, created_at, updated_at)
                     VALUES (?, ?, ?, ?)",
                    params![wallet_address, wallet_type, &now, &now],
                )?;

                let id = conn.last_insert_rowid();

                Ok(UserAccount {
                    id,
                    wallet_address: wallet_address.to_string(),
                    wallet_type: wallet_type.to_string(),
                    balance_usdc: 0.0,
                    balance_better: 0.0,
                    total_deposited: 0.0,
                    total_withdrawn: 0.0,
                    total_pnl: 0.0,
                    trade_count: 0,
                    win_count: 0,
                    kelly_fraction: 0.25,
                    auto_trade_enabled: false,
                    max_position_pct: 0.10,
                    created_at: Utc::now(),
                    updated_at: Utc::now(),
                })
            }
        }
    }

    /// Record a deposit
    pub async fn record_deposit(
        &self,
        user_id: i64,
        tx_hash: &str,
        amount: f64,
        token: &str,
    ) -> Result<Deposit> {
        let conn = self.conn.lock().await;
        let now = Utc::now().to_rfc3339();

        conn.execute(
            "INSERT INTO deposits (user_id, tx_hash, amount, token, status, created_at)
             VALUES (?, ?, ?, ?, 'pending', ?)",
            params![user_id, tx_hash, amount, token, &now],
        )?;

        let id = conn.last_insert_rowid();

        Ok(Deposit {
            id,
            user_id,
            tx_hash: tx_hash.to_string(),
            amount,
            token: token.to_string(),
            status: "pending".to_string(),
            created_at: Utc::now(),
        })
    }

    /// Confirm a deposit and update user balance
    pub async fn confirm_deposit(&self, tx_hash: &str) -> Result<()> {
        let conn = self.conn.lock().await;
        let now = Utc::now().to_rfc3339();

        // Get deposit info
        let (user_id, amount, token): (i64, f64, String) = conn.query_row(
            "SELECT user_id, amount, token FROM deposits WHERE tx_hash = ? AND status = 'pending'",
            [tx_hash],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;

        // Update deposit status
        conn.execute(
            "UPDATE deposits SET status = 'confirmed' WHERE tx_hash = ?",
            [tx_hash],
        )?;

        // Update user balance
        let balance_field = if token == "USDC" {
            "balance_usdc"
        } else {
            "balance_better"
        };
        conn.execute(
            &format!(
                "UPDATE user_accounts SET {} = {} + ?, total_deposited = total_deposited + ?, updated_at = ? WHERE id = ?",
                balance_field, balance_field
            ),
            params![amount, amount, &now, user_id],
        )?;

        Ok(())
    }

    /// Record a trade
    pub async fn record_trade(&self, trade: &TradeRecord) -> Result<i64> {
        let conn = self.conn.lock().await;
        let now = Utc::now().to_rfc3339();

        conn.execute(
            "INSERT INTO trades (user_id, signal_id, market_slug, side, outcome, entry_price, 
             position_size, kelly_fraction_used, status, created_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, 'pending', ?)",
            params![
                trade.user_id,
                trade.signal_id,
                trade.market_slug,
                trade.side,
                trade.outcome,
                trade.entry_price,
                trade.position_size,
                trade.kelly_fraction_used,
                &now
            ],
        )?;

        Ok(conn.last_insert_rowid())
    }

    /// Update user settings
    pub async fn update_user_settings(
        &self,
        user_id: i64,
        kelly_fraction: f64,
        auto_trade_enabled: bool,
        max_position_pct: f64,
    ) -> Result<()> {
        let conn = self.conn.lock().await;
        let now = Utc::now().to_rfc3339();

        conn.execute(
            "UPDATE user_accounts SET kelly_fraction = ?, auto_trade_enabled = ?, max_position_pct = ?, updated_at = ?
             WHERE id = ?",
            params![kelly_fraction, auto_trade_enabled as i64, max_position_pct, &now, user_id],
        )?;

        Ok(())
    }

    /// Get users with auto-trade enabled
    pub async fn get_auto_trade_users(&self) -> Result<Vec<UserAccount>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT * FROM user_accounts WHERE auto_trade_enabled = 1 AND balance_usdc > 0",
        )?;

        let users = stmt
            .query_map([], |row| {
                Ok(UserAccount {
                    id: row.get(0)?,
                    wallet_address: row.get(1)?,
                    wallet_type: row.get(2)?,
                    balance_usdc: row.get(3)?,
                    balance_better: row.get(4)?,
                    total_deposited: row.get(5)?,
                    total_withdrawn: row.get(6)?,
                    total_pnl: row.get(7)?,
                    trade_count: row.get(8)?,
                    win_count: row.get(9)?,
                    kelly_fraction: row.get(10)?,
                    auto_trade_enabled: row.get::<_, i64>(11)? == 1,
                    max_position_pct: row.get(12)?,
                    created_at: DateTime::parse_from_rfc3339(&row.get::<_, String>(13)?)
                        .unwrap()
                        .with_timezone(&Utc),
                    updated_at: DateTime::parse_from_rfc3339(&row.get::<_, String>(14)?)
                        .unwrap()
                        .with_timezone(&Utc),
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(users)
    }
}

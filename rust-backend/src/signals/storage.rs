use crate::models::Signal;
use anyhow::Result;
use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::json;
use std::sync::Mutex;

/// SQLite database for signal storage (thread-safe)
pub struct Database {
    conn: Mutex<Connection>,
}

impl Database {
    /// Create or open database and initialize schema
    pub fn new(path: &str) -> Result<Self> {
        let conn = Connection::open(path)?;
        
        // Enable foreign keys
        conn.execute("PRAGMA foreign_keys = ON", [])?;

        // Create signals table
        conn.execute(
            "CREATE TABLE IF NOT EXISTS signals (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                signal_type TEXT NOT NULL,
                source TEXT NOT NULL,
                market_name TEXT,
                description TEXT NOT NULL,
                confidence REAL NOT NULL,
                metadata TEXT,
                created_at TEXT NOT NULL,
                UNIQUE(description, created_at)
            )",
            [],
        )?;

        // Create index for faster queries
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_signals_created_at ON signals(created_at DESC)",
            [],
        )?;

        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_signals_type ON signals(signal_type)",
            [],
        )?;

        tracing::info!("✅ Database initialized at {}", path);

        Ok(Self { conn: Mutex::new(conn) })
    }

    /// Insert a signal
    pub fn insert_signal(&self, signal: &Signal) -> Result<i64> {
        let signal_type = signal.signal_type.as_str();
        let created_at = signal.created_at.to_rfc3339();

        let conn = self.conn.lock().unwrap();
        let result = conn.execute(
            "INSERT INTO signals (signal_type, source, market_name, description, confidence, metadata, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                signal_type,
                &signal.source,
                &signal.market_name,
                &signal.description,
                signal.confidence,
                &signal.metadata,
                created_at,
            ],
        );

        match result {
            Ok(_) => {
                let id = conn.last_insert_rowid();
                Ok(id)
            }
            Err(e) => {
                // Handle unique constraint (duplicate signal)
                if e.to_string().contains("UNIQUE constraint failed") {
                    tracing::debug!("Duplicate signal skipped: {}", signal.description);
                    Ok(-1) // Return -1 to indicate duplicate
                } else {
                    Err(e.into())
                }
            }
        }
    }

    /// Check if signal exists recently (within N hours) - uses EXISTS for efficiency
    pub fn signal_exists_recently(&self, description: &str, hours_lookback: i32) -> Result<bool> {
        let cutoff_time = Utc::now()
            .checked_sub_signed(chrono::Duration::hours(hours_lookback as i64))
            .unwrap()
            .to_rfc3339();

        let conn = self.conn.lock().unwrap();
        let exists: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM signals WHERE description = ?1 AND created_at >= ?2)",
            params![description, cutoff_time],
            |row| row.get(0),
        )?;

        Ok(exists)
    }

    /// Get signal by ID (efficient single-row lookup)
    pub fn get_signal_by_id(&self, id: i64) -> Result<Option<Signal>> {
        let conn = self.conn.lock().unwrap();
        let result = conn.query_row(
            "SELECT id, signal_type, source, market_name, description, confidence, metadata, created_at
             FROM signals WHERE id = ?1",
            params![id],
            |row| {
                Ok(Signal {
                    id: Some(row.get(0)?),
                    signal_type: match row.get::<_, String>(1)?.as_str() {
                        "insider_edge" => crate::models::SignalType::InsiderEdge,
                        "arbitrage" => crate::models::SignalType::Arbitrage,
                        "whale_cluster" => crate::models::SignalType::WhaleCluster,
                        "price_deviation" => crate::models::SignalType::PriceDeviation,
                        "expiry_edge" => crate::models::SignalType::ExpiryEdge,
                        _ => crate::models::SignalType::Arbitrage,
                    },
                    source: row.get(2)?,
                    market_name: row.get(3)?,
                    description: row.get(4)?,
                    confidence: row.get(5)?,
                    metadata: row.get(6)?,
                    created_at: row
                        .get::<_, String>(7)?
                        .parse()
                        .unwrap_or_else(|_| Utc::now()),
                })
            },
        );

        match result {
            Ok(signal) => Ok(Some(signal)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Get total signal count
    pub fn count_signals(&self) -> Result<i64> {
        let conn = self.conn.lock().unwrap();
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM signals",
            [],
            |row| row.get(0),
        )?;

        Ok(count)
    }

    /// Get signals by type
    pub fn get_signals_by_type(&self, signal_type: &str, limit: i32) -> Result<Vec<Signal>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, signal_type, source, market_name, description, confidence, metadata, created_at
             FROM signals WHERE signal_type = ?1 ORDER BY created_at DESC LIMIT ?2"
        )?;

        let signals = stmt.query_map(params![signal_type, limit], |row| {
            Ok(Signal {
                id: Some(row.get(0)?),
                signal_type: match row.get::<_, String>(1)?.as_str() {
                    "insider_edge" => crate::models::SignalType::InsiderEdge,
                    _ => crate::models::SignalType::Arbitrage,
                },
                source: row.get(2)?,
                market_name: row.get(3)?,
                description: row.get(4)?,
                confidence: row.get(5)?,
                metadata: row.get(6)?,
                created_at: row
                    .get::<_, String>(7)?
                    .parse()
                    .unwrap_or_else(|_| Utc::now()),
            })
        })?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(signals)
    }

    /// Get recent signals (last N hours)
    pub fn get_recent_signals(&self, hours: i32, limit: i32) -> Result<Vec<Signal>> {
        let cutoff_time = Utc::now()
            .checked_sub_signed(chrono::Duration::hours(hours as i64))
            .unwrap()
            .to_rfc3339();

        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, signal_type, source, market_name, description, confidence, metadata, created_at
             FROM signals WHERE created_at >= ?1 ORDER BY created_at DESC LIMIT ?2"
        )?;

        let signals = stmt.query_map(params![cutoff_time, limit], |row| {
            Ok(Signal {
                id: Some(row.get(0)?),
                signal_type: match row.get::<_, String>(1)?.as_str() {
                    "insider_edge" => crate::models::SignalType::InsiderEdge,
                    _ => crate::models::SignalType::Arbitrage,
                },
                source: row.get(2)?,
                market_name: row.get(3)?,
                description: row.get(4)?,
                confidence: row.get(5)?,
                metadata: row.get(6)?,
                created_at: row
                    .get::<_, String>(7)?
                    .parse()
                    .unwrap_or_else(|_| Utc::now()),
            })
        })?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(signals)
    }

    /// Delete old signals (older than N days)
    #[allow(dead_code)] // Reserved for maintenance/cleanup tasks
    pub fn cleanup_old_signals(&self, days_old: i32) -> Result<usize> {
        let cutoff_time = Utc::now()
            .checked_sub_signed(chrono::Duration::days(days_old as i64))
            .unwrap()
            .to_rfc3339();

        let conn = self.conn.lock().unwrap();
        let deleted = conn.execute(
            "DELETE FROM signals WHERE created_at < ?1",
            params![cutoff_time],
        )?;

        if deleted > 0 {
            tracing::info!("🗑️  Cleaned up {} old signals", deleted);
        }

        Ok(deleted)
    }

    /// Get summary stats
    pub fn get_stats(&self) -> Result<serde_json::Value> {
        let conn = self.conn.lock().unwrap();
        
        let total: i64 = conn.query_row(
            "SELECT COUNT(*) FROM signals",
            [],
            |row| row.get(0),
        )?;

        let insider: i64 = conn.query_row(
            "SELECT COUNT(*) FROM signals WHERE signal_type = 'insider_edge'",
            [],
            |row| row.get(0),
        )?;

        let arbitrage: i64 = conn.query_row(
            "SELECT COUNT(*) FROM signals WHERE signal_type = 'arbitrage'",
            [],
            |row| row.get(0),
        )?;

        let avg_confidence: Option<f64> = conn.query_row(
            "SELECT AVG(confidence) FROM signals",
            [],
            |row| row.get(0),
        ).optional()?;

        Ok(json!({
            "total": total,
            "insider_edge": insider,
            "arbitrage": arbitrage,
            "avg_confidence": avg_confidence.unwrap_or(0.0),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::SignalType;

    #[test]
    fn test_database_crud() -> Result<()> {
        let db = Database::new(":memory:")?;

        // Create signal
        let signal = Signal::new(
            SignalType::InsiderEdge,
            "testuser".to_string(),
            "Test signal description".to_string(),
            85.0,
        );

        let id = db.insert_signal(&signal)?;
        assert!(id > 0);

        // Count
        let count = db.count_signals()?;
        assert_eq!(count, 1);

        // Get by type
        let signals = db.get_signals_by_type("insider_edge", 10)?;
        assert_eq!(signals.len(), 1);

        Ok(())
    }

    #[test]
    fn test_duplicate_detection() -> Result<()> {
        let db = Database::new(":memory:")?;

        let signal1 = Signal::new(
            SignalType::Arbitrage,
            "user1".to_string(),
            "Same description".to_string(),
            50.0,
        );

        let signal2 = Signal::new(
            SignalType::Arbitrage,
            "user2".to_string(),
            "Same description".to_string(),
            60.0,
        );

        let id1 = db.insert_signal(&signal1)?;
        let id2 = db.insert_signal(&signal2)?;

        // Second insert should be skipped (unique constraint)
        assert!(id2 < 0 || db.count_signals()? == 1);

        Ok(())
    }
}

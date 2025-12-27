//! User Storage
//! Mission: Securely store and manage user accounts with SQLite

use crate::auth::models::{User, UserRole};
use anyhow::{Context, Result};
use bcrypt::{hash, verify, DEFAULT_COST};
use chrono::Utc;
use rusqlite::{params, Connection};
use tracing::{info, warn};
use uuid::Uuid;

/// User storage with SQLite backend
pub struct UserStore {
    db_path: String,
}

impl UserStore {
    /// Create a new user store and initialize database
    pub fn new(db_path: &str) -> Result<Self> {
        let store = Self {
            db_path: db_path.to_string(),
        };
        store.init_db()?;
        Ok(store)
    }

    /// Initialize database schema
    fn init_db(&self) -> Result<()> {
        let conn = Connection::open(&self.db_path)?;

        // Users table
        conn.execute(
            "CREATE TABLE IF NOT EXISTS users (
                id TEXT PRIMARY KEY,
                username TEXT UNIQUE NOT NULL,
                password_hash TEXT NOT NULL,
                role TEXT NOT NULL,
                api_key TEXT,
                created_at TEXT NOT NULL
            )",
            [],
        )?;

        // API keys table
        conn.execute(
            "CREATE TABLE IF NOT EXISTS api_keys (
                id TEXT PRIMARY KEY,
                key TEXT UNIQUE NOT NULL,
                user_id TEXT NOT NULL,
                name TEXT NOT NULL,
                rate_limit INTEGER NOT NULL,
                created_at TEXT NOT NULL,
                last_used TEXT,
                revoked INTEGER NOT NULL DEFAULT 0,
                FOREIGN KEY (user_id) REFERENCES users(id)
            )",
            [],
        )?;

        // Create default admin user if none exists
        self.create_default_admin(&conn)?;

        Ok(())
    }

    /// Create default admin user for initial setup
    fn create_default_admin(&self, conn: &Connection) -> Result<()> {
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM users WHERE role = 'admin'",
                [],
                |row| row.get(0),
            )
            .context("Failed to check for admin users")?;

        if count == 0 {
            let password_hash =
                hash("admin123", DEFAULT_COST).context("Failed to hash password")?;

            let admin = User {
                id: Uuid::new_v4(),
                username: "admin".to_string(),
                password_hash,
                role: UserRole::Admin,
                api_key: None,
                created_at: Utc::now().to_rfc3339(),
            };

            conn.execute(
                "INSERT INTO users (id, username, password_hash, role, api_key, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    admin.id.to_string(),
                    admin.username,
                    admin.password_hash,
                    admin.role.as_str(),
                    admin.api_key,
                    admin.created_at,
                ],
            )
            .context("Failed to insert admin user")?;

            info!("🔐 Default admin user created (username: admin, password: admin123)");
            warn!("⚠️  CHANGE DEFAULT PASSWORD IN PRODUCTION!");
        }

        Ok(())
    }

    /// Get user by username
    pub fn get_user_by_username(&self, username: &str) -> Result<Option<User>> {
        let conn = Connection::open(&self.db_path)?;

        let mut stmt = conn.prepare(
            "SELECT id, username, password_hash, role, api_key, created_at
             FROM users WHERE username = ?1",
        )?;

        let user_result = stmt.query_row(params![username], |row| {
            let role_str: String = row.get(3)?;
            Ok(User {
                id: Uuid::parse_str(&row.get::<_, String>(0)?).unwrap(),
                username: row.get(1)?,
                password_hash: row.get(2)?,
                role: UserRole::from_str(&role_str).unwrap_or(UserRole::Viewer),
                api_key: row.get(4)?,
                created_at: row.get(5)?,
            })
        });

        match user_result {
            Ok(user) => Ok(Some(user)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Verify username and password
    pub fn verify_password(&self, username: &str, password: &str) -> Result<bool> {
        match self.get_user_by_username(username)? {
            Some(user) => {
                let valid =
                    verify(password, &user.password_hash).context("Failed to verify password")?;
                Ok(valid)
            }
            None => Ok(false),
        }
    }

    /// Create a new user
    pub fn create_user(&self, username: &str, password: &str, role: UserRole) -> Result<User> {
        let password_hash = hash(password, DEFAULT_COST).context("Failed to hash password")?;

        let user = User {
            id: Uuid::new_v4(),
            username: username.to_string(),
            password_hash,
            role,
            api_key: None,
            created_at: Utc::now().to_rfc3339(),
        };

        let conn = Connection::open(&self.db_path)?;
        conn.execute(
            "INSERT INTO users (id, username, password_hash, role, api_key, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                user.id.to_string(),
                user.username,
                user.password_hash,
                user.role.as_str(),
                user.api_key,
                user.created_at,
            ],
        )
        .context("Failed to insert user")?;

        info!(
            "✅ Created user: {} ({})",
            user.username,
            user.role.as_str()
        );

        Ok(user)
    }

    /// List all users (admin only)
    pub fn list_users(&self) -> Result<Vec<User>> {
        let conn = Connection::open(&self.db_path)?;

        let mut stmt = conn
            .prepare("SELECT id, username, password_hash, role, api_key, created_at FROM users")?;

        let users = stmt
            .query_map([], |row| {
                let role_str: String = row.get(3)?;
                Ok(User {
                    id: Uuid::parse_str(&row.get::<_, String>(0)?).unwrap(),
                    username: row.get(1)?,
                    password_hash: row.get(2)?,
                    role: UserRole::from_str(&role_str).unwrap_or(UserRole::Viewer),
                    api_key: row.get(4)?,
                    created_at: row.get(5)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(users)
    }

    /// Delete a user by ID (admin only)
    pub fn delete_user(&self, user_id: &Uuid) -> Result<()> {
        let conn = Connection::open(&self.db_path)?;

        let rows_affected = conn.execute(
            "DELETE FROM users WHERE id = ?1",
            params![user_id.to_string()],
        )?;

        if rows_affected == 0 {
            anyhow::bail!("User not found");
        }

        info!("🗑️  Deleted user: {}", user_id);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::NamedTempFile;

    fn create_test_store() -> (UserStore, NamedTempFile) {
        let temp_file = NamedTempFile::new().unwrap();
        let db_path = temp_file.path().to_str().unwrap();
        let store = UserStore::new(db_path).unwrap();
        (store, temp_file)
    }

    #[test]
    fn test_default_admin_created() {
        let (store, _temp) = create_test_store();

        // Admin user should exist
        let admin = store.get_user_by_username("admin").unwrap();
        assert!(admin.is_some());

        let admin = admin.unwrap();
        assert_eq!(admin.username, "admin");
        assert_eq!(admin.role, UserRole::Admin);
    }

    #[test]
    fn test_password_verification() {
        let (store, _temp) = create_test_store();

        // Correct password
        assert!(store.verify_password("admin", "admin123").unwrap());

        // Incorrect password
        assert!(!store.verify_password("admin", "wrongpassword").unwrap());

        // Non-existent user
        assert!(!store.verify_password("nonexistent", "password").unwrap());
    }

    #[test]
    fn test_create_and_retrieve_user() {
        let (store, _temp) = create_test_store();

        // Create trader
        let trader = store
            .create_user("trader1", "password123", UserRole::Trader)
            .unwrap();
        assert_eq!(trader.username, "trader1");
        assert_eq!(trader.role, UserRole::Trader);

        // Retrieve trader
        let retrieved = store.get_user_by_username("trader1").unwrap();
        assert!(retrieved.is_some());

        let retrieved = retrieved.unwrap();
        assert_eq!(retrieved.username, "trader1");
        assert_eq!(retrieved.role, UserRole::Trader);
    }

    #[test]
    fn test_list_users() {
        let (store, _temp) = create_test_store();

        // Create additional users
        store
            .create_user("trader1", "pass", UserRole::Trader)
            .unwrap();
        store
            .create_user("viewer1", "pass", UserRole::Viewer)
            .unwrap();

        // List all users
        let users = store.list_users().unwrap();
        assert_eq!(users.len(), 3); // admin + trader1 + viewer1
    }

    #[test]
    fn test_delete_user() {
        let (store, _temp) = create_test_store();

        // Create a user
        let user = store
            .create_user("tempuser", "pass", UserRole::Viewer)
            .unwrap();

        // Verify user exists
        assert!(store.get_user_by_username("tempuser").unwrap().is_some());

        // Delete user
        store.delete_user(&user.id).unwrap();

        // Verify user is gone
        assert!(store.get_user_by_username("tempuser").unwrap().is_none());
    }
}

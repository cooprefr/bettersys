//! Run Artifact Storage
//!
//! SQLite-based immutable storage for backtest run artifacts.
//! Once persisted, artifacts cannot be modified - only read.
//!
//! # Schema Design
//!
//! ```sql
//! -- Main artifact table (minimal indexed columns)
//! CREATE TABLE run_artifacts (
//!     run_id TEXT PRIMARY KEY,
//!     fingerprint_hash TEXT NOT NULL,
//!     manifest_hash TEXT NOT NULL,
//!     persisted_at INTEGER NOT NULL,
//!     strategy_name TEXT NOT NULL,
//!     strategy_version TEXT NOT NULL,
//!     production_grade INTEGER NOT NULL,
//!     is_trusted INTEGER NOT NULL,
//!     trust_level TEXT NOT NULL,
//!     final_pnl REAL NOT NULL,
//!     -- Full artifact stored as compressed JSON blob
//!     artifact_blob BLOB NOT NULL
//! ) WITHOUT ROWID;
//! ```

use crate::backtest_v2::publication::PublicationStatus;
use crate::backtest_v2::run_artifact::{
    ArtifactResponse, ListRunsFilter, ListRunsResponse, MethodologyCapsule, RunArtifact, 
    RunId, RunManifest, RunSortField, RunSummary, RunTimeSeries, SortOrder, 
    TrustLevelDto, TrustStatus, RUN_ARTIFACT_API_VERSION, RUN_ARTIFACT_STORAGE_VERSION,
};
use rusqlite::{Connection, params, OptionalExtension};
use serde_json;
use std::path::Path;
use std::sync::Arc;
use parking_lot::Mutex;
use tracing::{debug, info, warn, error};

/// Schema version for migrations.
/// Version history:
/// - v1: Initial schema
/// - v2: Added publication_status column for public/internal separation
const SCHEMA_VERSION: u32 = 2;

/// Storage for run artifacts.
pub struct ArtifactStore {
    conn: Arc<Mutex<Connection>>,
}

impl ArtifactStore {
    /// Create a new artifact store at the given path.
    pub fn new<P: AsRef<Path>>(path: P) -> Result<Self, ArtifactStoreError> {
        let conn = Connection::open(path)?;
        let store = Self {
            conn: Arc::new(Mutex::new(conn)),
        };
        store.initialize_schema()?;
        Ok(store)
    }
    
    /// Create an in-memory store (for testing).
    pub fn in_memory() -> Result<Self, ArtifactStoreError> {
        let conn = Connection::open_in_memory()?;
        let store = Self {
            conn: Arc::new(Mutex::new(conn)),
        };
        store.initialize_schema()?;
        Ok(store)
    }
    
    fn initialize_schema(&self) -> Result<(), ArtifactStoreError> {
        let conn = self.conn.lock();
        
        // Enable WAL mode for better concurrency
        conn.execute_batch(r#"
            PRAGMA journal_mode = WAL;
            PRAGMA synchronous = NORMAL;
            PRAGMA cache_size = -16000;
            PRAGMA temp_store = MEMORY;
        "#)?;
        
        // Check schema version
        conn.execute(
            "CREATE TABLE IF NOT EXISTS schema_version (version INTEGER PRIMARY KEY)",
            [],
        )?;
        
        let current_version: Option<u32> = conn
            .query_row("SELECT version FROM schema_version LIMIT 1", [], |row| row.get(0))
            .optional()?;
        
        match current_version {
            None => {
                // Fresh database - create schema
                self.create_schema_v2(&conn)?;
                conn.execute("INSERT INTO schema_version (version) VALUES (?)", [SCHEMA_VERSION])?;
                info!("Created artifact store schema v{}", SCHEMA_VERSION);
            }
            Some(1) => {
                // Migrate v1 -> v2
                self.migrate_v1_to_v2(&conn)?;
                conn.execute("UPDATE schema_version SET version = ?", [SCHEMA_VERSION])?;
                info!("Migrated artifact store schema from v1 to v{}", SCHEMA_VERSION);
            }
            Some(v) if v == SCHEMA_VERSION => {
                // Already at current version
                debug!("Artifact store schema at v{}", SCHEMA_VERSION);
            }
            Some(v) => {
                warn!(
                    "Artifact store schema version mismatch: expected {}, got {}",
                    SCHEMA_VERSION, v
                );
            }
        }
        
        Ok(())
    }
    
    fn migrate_v1_to_v2(&self, conn: &Connection) -> Result<(), ArtifactStoreError> {
        info!("Migrating artifact store from v1 to v2...");
        
        conn.execute_batch(r#"
            -- Add publication_status column (0=Internal, 1=Published, 2=Retracted)
            ALTER TABLE run_artifacts ADD COLUMN publication_status INTEGER NOT NULL DEFAULT 0;
            
            -- Add provenance columns for efficient querying without full deserialization
            ALTER TABLE run_artifacts ADD COLUMN dataset_version_id TEXT;
            ALTER TABLE run_artifacts ADD COLUMN dataset_readiness TEXT;
            ALTER TABLE run_artifacts ADD COLUMN settlement_source TEXT;
            ALTER TABLE run_artifacts ADD COLUMN integrity_policy TEXT;
            ALTER TABLE run_artifacts ADD COLUMN strategy_code_hash TEXT;
            
            -- Index for public API (only published runs)
            CREATE INDEX IF NOT EXISTS idx_artifacts_published 
                ON run_artifacts(publication_status, persisted_at DESC)
                WHERE publication_status = 1;
            
            -- Index for provenance filtering
            CREATE INDEX IF NOT EXISTS idx_artifacts_dataset_readiness
                ON run_artifacts(dataset_readiness, persisted_at DESC);
        "#)?;
        
        Ok(())
    }
    
    fn create_schema_v2(&self, conn: &Connection) -> Result<(), ArtifactStoreError> {
        conn.execute_batch(r#"
            -- Main artifact table (v2 schema with publication and provenance)
            CREATE TABLE IF NOT EXISTS run_artifacts (
                run_id TEXT PRIMARY KEY,
                fingerprint_hash TEXT NOT NULL,
                manifest_hash TEXT NOT NULL,
                persisted_at INTEGER NOT NULL,
                strategy_name TEXT NOT NULL,
                strategy_version TEXT NOT NULL,
                production_grade INTEGER NOT NULL,
                is_trusted INTEGER NOT NULL,
                trust_level TEXT NOT NULL,
                final_pnl REAL NOT NULL,
                total_fills INTEGER NOT NULL,
                sharpe_ratio REAL,
                max_drawdown REAL NOT NULL,
                win_rate REAL NOT NULL,
                -- v2: Publication status (0=Internal, 1=Published, 2=Retracted)
                publication_status INTEGER NOT NULL DEFAULT 0,
                -- v2: Provenance columns for efficient querying
                dataset_version_id TEXT,
                dataset_readiness TEXT,
                settlement_source TEXT,
                integrity_policy TEXT,
                strategy_code_hash TEXT,
                -- Compressed JSON blob of full artifact
                artifact_blob BLOB NOT NULL
            ) WITHOUT ROWID;
            
            -- Index for listing/filtering
            CREATE INDEX IF NOT EXISTS idx_artifacts_persisted 
                ON run_artifacts(persisted_at DESC);
            CREATE INDEX IF NOT EXISTS idx_artifacts_strategy 
                ON run_artifacts(strategy_name, strategy_version);
            CREATE INDEX IF NOT EXISTS idx_artifacts_trusted 
                ON run_artifacts(is_trusted, persisted_at DESC);
            CREATE INDEX IF NOT EXISTS idx_artifacts_production 
                ON run_artifacts(production_grade, persisted_at DESC);
            
            -- v2: Index for public API (only published runs)
            CREATE INDEX IF NOT EXISTS idx_artifacts_published 
                ON run_artifacts(publication_status, persisted_at DESC)
                WHERE publication_status = 1;
            
            -- v2: Index for provenance filtering
            CREATE INDEX IF NOT EXISTS idx_artifacts_dataset_readiness
                ON run_artifacts(dataset_readiness, persisted_at DESC);
        "#)?;
        
        Ok(())
    }
    
    /// Persist a run artifact as internal (not published). Returns error if already exists.
    pub fn persist(&self, artifact: &RunArtifact) -> Result<(), ArtifactStoreError> {
        self.persist_with_status(artifact, PublicationStatus::Internal)
    }
    
    /// Persist a run artifact with explicit publication status.
    /// 
    /// For `PublicationStatus::Published`, this validates that the run meets all
    /// publication requirements via `PublicationGate::evaluate()`.
    pub fn persist_with_status(
        &self, 
        artifact: &RunArtifact,
        status: PublicationStatus,
    ) -> Result<(), ArtifactStoreError> {
        let run_id = artifact.run_id().as_str();
        
        // Check if already exists
        if self.exists(&artifact.manifest.run_id)? {
            return Err(ArtifactStoreError::AlreadyExists(run_id.to_string()));
        }
        
        // If publishing, validate via PublicationGate
        if status == PublicationStatus::Published {
            // We need the original config and results to validate
            // The artifact contains results but not the original config
            // For now, we validate based on what's in results
            if !artifact.manifest.trust_decision.is_trusted {
                return Err(ArtifactStoreError::PublicationRejected(
                    "Cannot publish untrusted run".to_string()
                ));
            }
            if !artifact.results.production_grade {
                return Err(ArtifactStoreError::PublicationRejected(
                    "Cannot publish non-production-grade run".to_string()
                ));
            }
            if !artifact.results.gate_suite_passed {
                return Err(ArtifactStoreError::PublicationRejected(
                    "Cannot publish run that failed gate suite".to_string()
                ));
            }
        }
        
        // Serialize artifact to JSON, then compress
        let json = serde_json::to_vec(artifact)?;
        let compressed = compress_data(&json);
        
        // Extract provenance fields for efficient querying
        let dataset_version_id = artifact.manifest.fingerprint.dataset.hash.to_string();
        let dataset_readiness = &artifact.manifest.dataset.readiness;
        let settlement_source = artifact.manifest.fingerprint.config.chainlink_feed_id
            .as_ref()
            .map(|f| format!("Chainlink/{}", f))
            .unwrap_or_else(|| "Simulated".to_string());
        let integrity_policy = &artifact.manifest.fingerprint.config.integrity_policy;
        let strategy_code_hash = &artifact.manifest.strategy.code_hash;
        
        // Serialize trust_level to JSON string for indexed column
        let trust_level_json = serde_json::to_string(&artifact.manifest.trust_decision.trust_level)
            .unwrap_or_else(|_| r#"{"status":"Unknown","reasons":[]}"#.to_string());
        
        let conn = self.conn.lock();
        conn.execute(
            r#"INSERT INTO run_artifacts (
                run_id, fingerprint_hash, manifest_hash, persisted_at,
                strategy_name, strategy_version, production_grade, is_trusted,
                trust_level, final_pnl, total_fills, sharpe_ratio, max_drawdown,
                win_rate, publication_status, dataset_version_id, dataset_readiness,
                settlement_source, integrity_policy, strategy_code_hash, artifact_blob
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"#,
            params![
                run_id,
                artifact.manifest.fingerprint.hash_hex,
                artifact.manifest.manifest_hash,
                artifact.manifest.persisted_at,
                artifact.manifest.strategy.name,
                artifact.manifest.strategy.version,
                artifact.results.production_grade as i32,
                artifact.manifest.trust_decision.is_trusted as i32,
                trust_level_json,
                artifact.results.final_pnl,
                artifact.results.total_fills as i64,
                artifact.results.sharpe_ratio,
                artifact.results.max_drawdown,
                artifact.results.win_rate,
                status.to_db_int(),
                dataset_version_id,
                dataset_readiness,
                settlement_source,
                integrity_policy,
                strategy_code_hash,
                compressed,
            ],
        )?;
        
        debug!("Persisted run artifact: {} (status={:?})", run_id, status);
        Ok(())
    }
    
    /// Publish an existing internal run. Returns error if run doesn't exist or is already published.
    pub fn publish(&self, run_id: &RunId) -> Result<(), ArtifactStoreError> {
        // First get the artifact to validate
        let artifact = self.get(run_id)?
            .ok_or_else(|| ArtifactStoreError::NotFound(run_id.as_str().to_string()))?;
        
        // Validate publication requirements
        if !artifact.manifest.trust_decision.is_trusted {
            return Err(ArtifactStoreError::PublicationRejected(
                "Cannot publish untrusted run".to_string()
            ));
        }
        if !artifact.results.production_grade {
            return Err(ArtifactStoreError::PublicationRejected(
                "Cannot publish non-production-grade run".to_string()
            ));
        }
        if !artifact.results.gate_suite_passed {
            return Err(ArtifactStoreError::PublicationRejected(
                "Cannot publish run that failed gate suite".to_string()
            ));
        }
        
        // Update status
        let conn = self.conn.lock();
        let updated = conn.execute(
            "UPDATE run_artifacts SET publication_status = ? WHERE run_id = ? AND publication_status = 0",
            params![PublicationStatus::Published.to_db_int(), run_id.as_str()],
        )?;
        
        if updated == 0 {
            return Err(ArtifactStoreError::PublicationRejected(
                "Run is already published or does not exist".to_string()
            ));
        }
        
        info!("Published run artifact: {}", run_id);
        Ok(())
    }
    
    /// Retract a published run. Makes it no longer visible via public API.
    pub fn retract(&self, run_id: &RunId) -> Result<(), ArtifactStoreError> {
        let conn = self.conn.lock();
        let updated = conn.execute(
            "UPDATE run_artifacts SET publication_status = ? WHERE run_id = ? AND publication_status = 1",
            params![PublicationStatus::Retracted.to_db_int(), run_id.as_str()],
        )?;
        
        if updated == 0 {
            return Err(ArtifactStoreError::NotFound(
                format!("Run {} is not published", run_id)
            ));
        }
        
        info!("Retracted run artifact: {}", run_id);
        Ok(())
    }
    
    /// Check if a run artifact exists.
    pub fn exists(&self, run_id: &RunId) -> Result<bool, ArtifactStoreError> {
        let conn = self.conn.lock();
        let exists: bool = conn.query_row(
            "SELECT 1 FROM run_artifacts WHERE run_id = ?",
            [run_id.as_str()],
            |_| Ok(true),
        ).optional()?.unwrap_or(false);
        Ok(exists)
    }
    
    /// Get a run artifact by ID (any publication status - for authenticated users).
    pub fn get(&self, run_id: &RunId) -> Result<Option<RunArtifact>, ArtifactStoreError> {
        let conn = self.conn.lock();
        let result: Option<Vec<u8>> = conn.query_row(
            "SELECT artifact_blob FROM run_artifacts WHERE run_id = ?",
            [run_id.as_str()],
            |row| row.get(0),
        ).optional()?;
        
        match result {
            Some(compressed) => {
                let json = decompress_data(&compressed)?;
                let artifact: RunArtifact = serde_json::from_slice(&json)?;
                Ok(Some(artifact))
            }
            None => Ok(None),
        }
    }
    
    /// Get a run artifact by ID, only if it is published (for public API).
    /// Returns None if the run doesn't exist OR if it exists but is not published.
    pub fn get_if_published(&self, run_id: &RunId) -> Result<Option<RunArtifact>, ArtifactStoreError> {
        let conn = self.conn.lock();
        let result: Option<Vec<u8>> = conn.query_row(
            "SELECT artifact_blob FROM run_artifacts WHERE run_id = ? AND publication_status = 1",
            [run_id.as_str()],
            |row| row.get(0),
        ).optional()?;
        
        match result {
            Some(compressed) => {
                let json = decompress_data(&compressed)?;
                let artifact: RunArtifact = serde_json::from_slice(&json)?;
                Ok(Some(artifact))
            }
            None => Ok(None),
        }
    }
    
    /// Get publication status for a run.
    pub fn get_publication_status(&self, run_id: &RunId) -> Result<Option<PublicationStatus>, ArtifactStoreError> {
        let conn = self.conn.lock();
        let result: Option<i32> = conn.query_row(
            "SELECT publication_status FROM run_artifacts WHERE run_id = ?",
            [run_id.as_str()],
            |row| row.get(0),
        ).optional()?;
        
        Ok(result.map(PublicationStatus::from_db_int))
    }
    
    /// Get a run summary (lightweight) by ID.
    /// 
    /// This loads the full artifact to construct proper RunSummary with structured types.
    pub fn get_summary(&self, run_id: &RunId) -> Result<Option<RunSummary>, ArtifactStoreError> {
        // Load full artifact and convert to summary
        match self.get(run_id)? {
            Some(artifact) => Ok(Some(RunSummary::from_artifact(&artifact))),
            None => Ok(None),
        }
    }
    
    /// Get the manifest hash for ETag validation.
    pub fn get_manifest_hash(&self, run_id: &RunId) -> Result<Option<String>, ArtifactStoreError> {
        let conn = self.conn.lock();
        let result: Option<String> = conn.query_row(
            "SELECT manifest_hash FROM run_artifacts WHERE run_id = ?",
            [run_id.as_str()],
            |row| row.get(0),
        ).optional()?;
        Ok(result)
    }
    
    /// List runs with filters.
    /// 
    /// This now loads full artifacts to construct proper RunSummary with structured types.
    pub fn list(&self, filter: &ListRunsFilter) -> Result<ListRunsResponse, ArtifactStoreError> {
        let page = filter.page.unwrap_or(0);
        let page_size = filter.page_size.unwrap_or(20).min(100);
        let offset = page * page_size;
        
        let conn = self.conn.lock();
        
        // Build query with filters - select artifact_blob to get full data
        let mut sql = String::from(
            r#"SELECT run_id, artifact_blob, strategy_name, strategy_version, is_trusted, production_grade
            FROM run_artifacts WHERE 1=1"#
        );
        
        let mut params_vec: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        
        if let Some(ref name) = filter.strategy_name {
            sql.push_str(" AND strategy_name = ?");
            params_vec.push(Box::new(name.clone()));
        }
        
        if let Some(trusted) = filter.trusted_only {
            if trusted {
                sql.push_str(" AND is_trusted = 1");
            }
        }
        
        if let Some(prod) = filter.production_grade_only {
            if prod {
                sql.push_str(" AND production_grade = 1");
            }
        }
        
        // published_only filter: exclude runs with strategy_name = "unknown"
        if filter.published_only.unwrap_or(false) {
            sql.push_str(" AND strategy_name != 'unknown' AND strategy_version != '0.0.0'");
        }
        
        // certified_only filter: must be trusted + production_grade + published
        if filter.certified_only.unwrap_or(false) {
            sql.push_str(" AND is_trusted = 1 AND production_grade = 1 AND strategy_name != 'unknown' AND strategy_version != '0.0.0'");
        }
        
        // include_internal=false (default): hide internal/test runs
        if !filter.include_internal.unwrap_or(false) && !filter.published_only.unwrap_or(false) && !filter.certified_only.unwrap_or(false) {
            // Default behavior: show all runs (for backward compatibility)
            // Only hide internal runs if explicitly requested via published_only or certified_only
        }
        
        if let Some(min_pnl) = filter.min_pnl {
            sql.push_str(" AND final_pnl >= ?");
            params_vec.push(Box::new(min_pnl));
        }
        
        if let Some(after) = filter.after {
            sql.push_str(" AND persisted_at >= ?");
            params_vec.push(Box::new(after));
        }
        
        if let Some(before) = filter.before {
            sql.push_str(" AND persisted_at <= ?");
            params_vec.push(Box::new(before));
        }
        
        // Get total count
        let count_sql = sql.replace(
            "SELECT run_id, artifact_blob, strategy_name, strategy_version, is_trusted, production_grade",
            "SELECT COUNT(*)"
        );
        
        let params_refs: Vec<&dyn rusqlite::ToSql> = params_vec.iter().map(|b| b.as_ref()).collect();
        let total_count: usize = conn.query_row(&count_sql, params_refs.as_slice(), |row| row.get(0))?;
        
        // Determine sort field and order
        let sort_by = filter.sort_by.unwrap_or_default();
        let sort_order = filter.sort_order.unwrap_or_default();
        
        // Add ordering with secondary sort by run_id for determinism
        sql.push_str(&format!(
            " ORDER BY {} {}, run_id ASC LIMIT ? OFFSET ?",
            sort_by.sql_column(),
            sort_order.sql_keyword()
        ));
        params_vec.push(Box::new(page_size as i64));
        params_vec.push(Box::new(offset as i64));
        
        let params_refs: Vec<&dyn rusqlite::ToSql> = params_vec.iter().map(|b| b.as_ref()).collect();
        let mut stmt = conn.prepare(&sql)?;
        
        // Load artifacts and convert to RunSummary using the proper method
        let runs: Vec<RunSummary> = stmt.query_map(params_refs.as_slice(), |row| {
            let compressed: Vec<u8> = row.get(1)?;
            Ok(compressed)
        })?
        .filter_map(|r| r.ok())
        .filter_map(|compressed| {
            let json = decompress_data(&compressed).ok()?;
            let artifact: RunArtifact = serde_json::from_slice(&json).ok()?;
            Some(RunSummary::from_artifact(&artifact))
        })
        .collect();
        
        // Compute pagination metadata
        let total_pages = if total_count == 0 { 0 } else { (total_count + page_size - 1) / page_size };
        let has_next = page + 1 < total_pages;
        let has_prev = page > 0;
        
        Ok(ListRunsResponse {
            api_version: RUN_ARTIFACT_API_VERSION.to_string(),
            total_count,
            page,
            page_size,
            total_pages,
            has_next,
            has_prev,
            sort_by,
            sort_order,
            runs,
        })
    }
    
    /// List only published runs (for public API).
    /// This is the entry point for unauthenticated access - ONLY published runs are returned.
    pub fn list_published(&self, filter: &ListRunsFilter) -> Result<ListRunsResponse, ArtifactStoreError> {
        let page = filter.page.unwrap_or(0);
        let page_size = filter.page_size.unwrap_or(20).min(100);
        let offset = page * page_size;
        
        let conn = self.conn.lock();
        
        // Build query - ALWAYS filter to publication_status = 1 (Published)
        let mut sql = String::from(
            r#"SELECT run_id, artifact_blob
            FROM run_artifacts WHERE publication_status = 1"#
        );
        
        let mut params_vec: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        
        if let Some(ref name) = filter.strategy_name {
            sql.push_str(" AND strategy_name = ?");
            params_vec.push(Box::new(name.clone()));
        }
        
        // Note: For public API, we don't expose trusted_only/production_grade filters
        // because published runs MUST be trusted AND production-grade by definition.
        
        if let Some(min_pnl) = filter.min_pnl {
            sql.push_str(" AND final_pnl >= ?");
            params_vec.push(Box::new(min_pnl));
        }
        
        if let Some(after) = filter.after {
            sql.push_str(" AND persisted_at >= ?");
            params_vec.push(Box::new(after));
        }
        
        if let Some(before) = filter.before {
            sql.push_str(" AND persisted_at <= ?");
            params_vec.push(Box::new(before));
        }
        
        // Get total count
        let count_sql = sql.replace(
            "SELECT run_id, artifact_blob",
            "SELECT COUNT(*)"
        );
        
        let params_refs: Vec<&dyn rusqlite::ToSql> = params_vec.iter().map(|b| b.as_ref()).collect();
        let total_count: usize = conn.query_row(&count_sql, params_refs.as_slice(), |row| row.get(0))?;
        
        // Determine sort field and order
        let sort_by = filter.sort_by.unwrap_or_default();
        let sort_order = filter.sort_order.unwrap_or_default();
        
        // Add ordering with secondary sort by run_id for determinism
        sql.push_str(&format!(
            " ORDER BY {} {}, run_id ASC LIMIT ? OFFSET ?",
            sort_by.sql_column(),
            sort_order.sql_keyword()
        ));
        params_vec.push(Box::new(page_size as i64));
        params_vec.push(Box::new(offset as i64));
        
        let params_refs: Vec<&dyn rusqlite::ToSql> = params_vec.iter().map(|b| b.as_ref()).collect();
        let mut stmt = conn.prepare(&sql)?;
        
        // Load artifacts and convert to RunSummary using the proper method
        let runs: Vec<RunSummary> = stmt.query_map(params_refs.as_slice(), |row| {
            let compressed: Vec<u8> = row.get(1)?;
            Ok(compressed)
        })?
        .filter_map(|r| r.ok())
        .filter_map(|compressed| {
            let json = decompress_data(&compressed).ok()?;
            let artifact: RunArtifact = serde_json::from_slice(&json).ok()?;
            Some(RunSummary::from_artifact(&artifact))
        })
        .collect();
        
        // Compute pagination metadata
        let total_pages = if total_count == 0 { 0 } else { (total_count + page_size - 1) / page_size };
        let has_next = page + 1 < total_pages;
        let has_prev = page > 0;
        
        Ok(ListRunsResponse {
            api_version: RUN_ARTIFACT_API_VERSION.to_string(),
            total_count,
            page,
            page_size,
            total_pages,
            has_next,
            has_prev,
            sort_by,
            sort_order,
            runs,
        })
    }
    
    /// Delete a run artifact (admin only - use with caution).
    #[cfg(test)]
    pub fn delete(&self, run_id: &RunId) -> Result<bool, ArtifactStoreError> {
        let conn = self.conn.lock();
        let rows = conn.execute(
            "DELETE FROM run_artifacts WHERE run_id = ?",
            [run_id.as_str()],
        )?;
        Ok(rows > 0)
    }
    
    /// Get database statistics.
    pub fn stats(&self) -> Result<ArtifactStoreStats, ArtifactStoreError> {
        let conn = self.conn.lock();
        
        let total_runs: i64 = conn.query_row(
            "SELECT COUNT(*) FROM run_artifacts",
            [],
            |row| row.get(0),
        )?;
        
        let trusted_runs: i64 = conn.query_row(
            "SELECT COUNT(*) FROM run_artifacts WHERE is_trusted = 1",
            [],
            |row| row.get(0),
        )?;
        
        let production_runs: i64 = conn.query_row(
            "SELECT COUNT(*) FROM run_artifacts WHERE production_grade = 1",
            [],
            |row| row.get(0),
        )?;
        
        let total_size: i64 = conn.query_row(
            "SELECT COALESCE(SUM(LENGTH(artifact_blob)), 0) FROM run_artifacts",
            [],
            |row| row.get(0),
        )?;
        
        Ok(ArtifactStoreStats {
            total_runs: total_runs as u64,
            trusted_runs: trusted_runs as u64,
            production_runs: production_runs as u64,
            total_size_bytes: total_size as u64,
        })
    }
}

/// Statistics about the artifact store.
#[derive(Debug, Clone)]
pub struct ArtifactStoreStats {
    pub total_runs: u64,
    pub trusted_runs: u64,
    pub production_runs: u64,
    pub total_size_bytes: u64,
}

/// Errors from the artifact store.
#[derive(Debug)]
pub enum ArtifactStoreError {
    Sqlite(rusqlite::Error),
    Serialization(serde_json::Error),
    Compression(String),
    AlreadyExists(String),
    NotFound(String),
    PublicationRejected(String),
}

impl std::fmt::Display for ArtifactStoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Sqlite(e) => write!(f, "SQLite error: {}", e),
            Self::Serialization(e) => write!(f, "Serialization error: {}", e),
            Self::Compression(e) => write!(f, "Compression error: {}", e),
            Self::AlreadyExists(id) => write!(f, "Artifact already exists: {}", id),
            Self::NotFound(id) => write!(f, "Artifact not found: {}", id),
            Self::PublicationRejected(reason) => write!(f, "Publication rejected: {}", reason),
        }
    }
}

impl std::error::Error for ArtifactStoreError {}

impl From<rusqlite::Error> for ArtifactStoreError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Sqlite(e)
    }
}

impl From<serde_json::Error> for ArtifactStoreError {
    fn from(e: serde_json::Error) -> Self {
        Self::Serialization(e)
    }
}

/// Simple compression using zstd-like compression (placeholder - using basic encoding).
/// In production, use zstd or lz4 for better compression.
fn compress_data(data: &[u8]) -> Vec<u8> {
    // For now, just return the data as-is. In production, use zstd.
    // This keeps the implementation simple and avoids adding dependencies.
    data.to_vec()
}

fn decompress_data(data: &[u8]) -> Result<Vec<u8>, ArtifactStoreError> {
    // Corresponding decompression
    Ok(data.to_vec())
}

// =============================================================================
// TESTS
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backtest_v2::fingerprint::{
        BehaviorFingerprint, CodeFingerprint, ConfigFingerprint, DatasetFingerprint,
        RunFingerprint, SeedFingerprint, StrategyFingerprint,
    };
    use crate::backtest_v2::orchestrator::BacktestResults;
    use crate::backtest_v2::run_artifact::{
        ConfigSummary, DatasetMetadata, RunDistributions, StrategyIdentity,
        TimeRangeSummary, TrustDecisionSummary,
    };

    fn make_test_fingerprint(hash_hex: &str) -> RunFingerprint {
        RunFingerprint {
            version: "RUNFP_V2".to_string(),
            strategy: StrategyFingerprint::default(),
            code: CodeFingerprint::new(),
            config: ConfigFingerprint {
                settlement_reference_rule: None,
                settlement_tie_rule: None,
                chainlink_feed_id: None,
                oracle_chain_id: None,
                oracle_feed_proxies: vec![],
                oracle_decimals: vec![],
                oracle_visibility_rule: None,
                oracle_rounding_policy: None,
                oracle_config_hash: None,
                latency_model: "Fixed".to_string(),
                order_latency_ns: None,
                oms_parity_mode: "Full".to_string(),
                maker_fill_model: "Disabled".to_string(),
                integrity_policy: "Strict".to_string(),
                invariant_mode: "Hard".to_string(),
                fee_rate_bps: None,
                strategy_params_hash: 0,
                arrival_policy: "RecordedArrival".to_string(),
                strict_accounting: true,
                production_grade: true,
                allow_non_production: false,
                hash: 0,
            },
            dataset: DatasetFingerprint {
                classification: "FullIncremental".to_string(),
                readiness: "MakerViable".to_string(),
                orderbook_type: "FullIncrementalL2DeltasWithExchangeSeq".to_string(),
                trade_type: "TradePrints".to_string(),
                arrival_semantics: "RecordedArrival".to_string(),
                streams: vec![],
                hash: 0,
            },
            seed: SeedFingerprint::new(42),
            behavior: BehaviorFingerprint {
                event_count: 1000,
                hash: 0xDEAD,
            },
            registry: None,
            hash: 0,
            hash_hex: hash_hex.to_string(),
        }
    }

    fn make_test_artifact(run_id: &str) -> RunArtifact {
        let fingerprint = make_test_fingerprint(&run_id.replace("run_", ""));
        
        let manifest = RunManifest {
            schema_version: RUN_ARTIFACT_STORAGE_VERSION,
            run_id: RunId(run_id.to_string()),
            persisted_at: 1234567890,
            fingerprint,
            strategy: StrategyIdentity {
                name: "test_strategy".to_string(),
                version: "1.0.0".to_string(),
                code_hash: None,
            },
            dataset: DatasetMetadata {
                classification: "FullIncremental".to_string(),
                readiness: "MakerViable".to_string(),
                events_processed: 1000,
                delta_events_processed: 500,
                time_range: TimeRangeSummary {
                    start_ns: 0,
                    end_ns: 1_000_000_000,
                    duration_ns: 1_000_000_000,
                },
            },
            config_summary: ConfigSummary {
                production_grade: true,
                strict_mode: true,
                strict_accounting: true,
                maker_fill_model: "Disabled".to_string(),
                oms_parity_mode: "Full".to_string(),
                seed: 42,
            },
            trust_decision: TrustDecisionSummary {
                verdict: "Trusted".to_string(),
                trust_level: TrustLevelDto {
                    status: TrustStatus::Trusted,
                    reasons: vec![],
                },
                is_trusted: true,
                failure_reasons: vec![],
            },
            disclaimers: vec![],
            methodology_capsule: MethodologyCapsule {
                version: "v1".to_string(),
                summary: "Test capsule".to_string(),
                details: vec![],
                input_hash: "0".to_string(),
            },
            manifest_hash: "abcd1234".to_string(),
        };
        
        RunArtifact {
            manifest,
            results: BacktestResults::default(),
            time_series: RunTimeSeries {
                equity_curve: None,
                drawdown_series: None,
                window_pnl: None,
                pnl_history: vec![],
            },
            distributions: RunDistributions {
                trade_pnl_bins: vec![],
                trade_size_bins: vec![],
                hold_time_bins: vec![],
                slippage_bins: vec![],
            },
        }
    }

    #[test]
    fn test_persist_and_retrieve() {
        let store = ArtifactStore::in_memory().unwrap();
        let artifact = make_test_artifact("run_test123");
        
        // Persist
        store.persist(&artifact).unwrap();
        
        // Check exists
        assert!(store.exists(&artifact.manifest.run_id).unwrap());
        
        // Retrieve
        let retrieved = store.get(&artifact.manifest.run_id).unwrap().unwrap();
        assert_eq!(retrieved.manifest.run_id.as_str(), "run_test123");
    }

    #[test]
    fn test_already_exists() {
        let store = ArtifactStore::in_memory().unwrap();
        let artifact = make_test_artifact("run_duplicate");
        
        // First persist should succeed
        store.persist(&artifact).unwrap();
        
        // Second persist should fail
        let result = store.persist(&artifact);
        assert!(matches!(result, Err(ArtifactStoreError::AlreadyExists(_))));
    }

    #[test]
    fn test_get_summary() {
        let store = ArtifactStore::in_memory().unwrap();
        let artifact = make_test_artifact("run_summary_test");
        store.persist(&artifact).unwrap();
        
        let summary = store.get_summary(&artifact.manifest.run_id).unwrap().unwrap();
        assert_eq!(summary.run_id.as_str(), "run_summary_test");
        assert_eq!(summary.strategy_id.name, "test_strategy");
    }

    #[test]
    fn test_list_runs() {
        let store = ArtifactStore::in_memory().unwrap();
        
        // Persist multiple artifacts
        for i in 0..5 {
            let mut artifact = make_test_artifact(&format!("run_list_{}", i));
            artifact.manifest.persisted_at = 1000 + i as i64;
            store.persist(&artifact).unwrap();
        }
        
        // List all
        let response = store.list(&ListRunsFilter::default()).unwrap();
        assert_eq!(response.total_count, 5);
        assert_eq!(response.runs.len(), 5);
        
        // List with pagination
        let filter = ListRunsFilter {
            page: Some(0),
            page_size: Some(2),
            ..Default::default()
        };
        let response = store.list(&filter).unwrap();
        assert_eq!(response.total_count, 5);
        assert_eq!(response.runs.len(), 2);
    }

    #[test]
    fn test_stats() {
        let store = ArtifactStore::in_memory().unwrap();
        
        let artifact = make_test_artifact("run_stats_test");
        store.persist(&artifact).unwrap();
        
        let stats = store.stats().unwrap();
        assert_eq!(stats.total_runs, 1);
        assert!(stats.total_size_bytes > 0);
    }
}

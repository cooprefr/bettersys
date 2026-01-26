//! IO Profiling
//!
//! Tracks disk and network IO bottlenecks:
//! - Disk read/write latency and throughput
//! - Network latency and bandwidth
//! - Connection pool utilization

use parking_lot::RwLock;
use serde::Serialize;
use std::{
    collections::HashMap,
    sync::atomic::{AtomicU64, Ordering},
    time::Instant,
};

/// IO profiler tracking disk and network operations
#[derive(Debug)]
pub struct IoProfiler {
    /// Disk IO metrics
    pub disk: DiskMetrics,
    /// Network IO metrics by endpoint
    pub network: RwLock<HashMap<String, NetworkMetrics>>,
    /// SQLite-specific metrics
    pub sqlite: SqliteMetrics,
}

impl IoProfiler {
    pub fn new() -> Self {
        Self {
            disk: DiskMetrics::new(),
            network: RwLock::new(HashMap::new()),
            sqlite: SqliteMetrics::new(),
        }
    }

    /// Record a disk read operation
    pub fn record_disk_read(&self, bytes: u64, latency_us: u64) {
        self.disk.record_read(bytes, latency_us);
    }

    /// Record a disk write operation
    pub fn record_disk_write(&self, bytes: u64, latency_us: u64) {
        self.disk.record_write(bytes, latency_us);
    }

    /// Record a network operation
    pub fn record_network(
        &self,
        endpoint: &str,
        bytes_sent: u64,
        bytes_recv: u64,
        latency_us: u64,
        success: bool,
    ) {
        let mut network = self.network.write();
        network
            .entry(endpoint.to_string())
            .or_insert_with(|| NetworkMetrics::new(endpoint))
            .record(bytes_sent, bytes_recv, latency_us, success);
    }

    /// Record SQLite operation
    pub fn record_sqlite(&self, operation: SqliteOp, latency_us: u64, rows: u64) {
        self.sqlite.record(operation, latency_us, rows);
    }

    /// Get snapshot of IO metrics
    pub fn snapshot(&self) -> IoSnapshot {
        let network = self.network.read();
        IoSnapshot {
            disk: self.disk.snapshot(),
            network: network.values().map(|n| n.snapshot()).collect(),
            sqlite: self.sqlite.snapshot(),
        }
    }
}

impl Default for IoProfiler {
    fn default() -> Self {
        Self::new()
    }
}

/// Disk IO metrics
#[derive(Debug)]
pub struct DiskMetrics {
    pub reads: AtomicU64,
    pub read_bytes: AtomicU64,
    pub read_latency_sum_us: AtomicU64,
    pub read_latency_max_us: AtomicU64,
    pub writes: AtomicU64,
    pub write_bytes: AtomicU64,
    pub write_latency_sum_us: AtomicU64,
    pub write_latency_max_us: AtomicU64,
}

impl DiskMetrics {
    pub fn new() -> Self {
        Self {
            reads: AtomicU64::new(0),
            read_bytes: AtomicU64::new(0),
            read_latency_sum_us: AtomicU64::new(0),
            read_latency_max_us: AtomicU64::new(0),
            writes: AtomicU64::new(0),
            write_bytes: AtomicU64::new(0),
            write_latency_sum_us: AtomicU64::new(0),
            write_latency_max_us: AtomicU64::new(0),
        }
    }

    pub fn record_read(&self, bytes: u64, latency_us: u64) {
        self.reads.fetch_add(1, Ordering::Relaxed);
        self.read_bytes.fetch_add(bytes, Ordering::Relaxed);
        self.read_latency_sum_us
            .fetch_add(latency_us, Ordering::Relaxed);
        self.read_latency_max_us
            .fetch_max(latency_us, Ordering::Relaxed);
    }

    pub fn record_write(&self, bytes: u64, latency_us: u64) {
        self.writes.fetch_add(1, Ordering::Relaxed);
        self.write_bytes.fetch_add(bytes, Ordering::Relaxed);
        self.write_latency_sum_us
            .fetch_add(latency_us, Ordering::Relaxed);
        self.write_latency_max_us
            .fetch_max(latency_us, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> DiskSnapshot {
        let reads = self.reads.load(Ordering::Relaxed);
        let writes = self.writes.load(Ordering::Relaxed);

        DiskSnapshot {
            reads,
            read_bytes: self.read_bytes.load(Ordering::Relaxed),
            read_avg_latency_us: if reads > 0 {
                self.read_latency_sum_us.load(Ordering::Relaxed) / reads
            } else {
                0
            },
            read_max_latency_us: self.read_latency_max_us.load(Ordering::Relaxed),
            writes,
            write_bytes: self.write_bytes.load(Ordering::Relaxed),
            write_avg_latency_us: if writes > 0 {
                self.write_latency_sum_us.load(Ordering::Relaxed) / writes
            } else {
                0
            },
            write_max_latency_us: self.write_latency_max_us.load(Ordering::Relaxed),
        }
    }
}

impl Default for DiskMetrics {
    fn default() -> Self {
        Self::new()
    }
}

/// Network metrics for a single endpoint
#[derive(Debug)]
pub struct NetworkMetrics {
    pub endpoint: String,
    pub requests: AtomicU64,
    pub successes: AtomicU64,
    pub failures: AtomicU64,
    pub bytes_sent: AtomicU64,
    pub bytes_recv: AtomicU64,
    pub latency_sum_us: AtomicU64,
    pub latency_max_us: AtomicU64,
    pub latency_min_us: AtomicU64,
}

impl NetworkMetrics {
    pub fn new(endpoint: &str) -> Self {
        Self {
            endpoint: endpoint.to_string(),
            requests: AtomicU64::new(0),
            successes: AtomicU64::new(0),
            failures: AtomicU64::new(0),
            bytes_sent: AtomicU64::new(0),
            bytes_recv: AtomicU64::new(0),
            latency_sum_us: AtomicU64::new(0),
            latency_max_us: AtomicU64::new(0),
            latency_min_us: AtomicU64::new(u64::MAX),
        }
    }

    pub fn record(&mut self, bytes_sent: u64, bytes_recv: u64, latency_us: u64, success: bool) {
        self.requests.fetch_add(1, Ordering::Relaxed);
        self.bytes_sent.fetch_add(bytes_sent, Ordering::Relaxed);
        self.bytes_recv.fetch_add(bytes_recv, Ordering::Relaxed);
        self.latency_sum_us.fetch_add(latency_us, Ordering::Relaxed);
        self.latency_max_us.fetch_max(latency_us, Ordering::Relaxed);
        self.latency_min_us.fetch_min(latency_us, Ordering::Relaxed);

        if success {
            self.successes.fetch_add(1, Ordering::Relaxed);
        } else {
            self.failures.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn snapshot(&self) -> NetworkSnapshot {
        let requests = self.requests.load(Ordering::Relaxed);
        NetworkSnapshot {
            endpoint: self.endpoint.clone(),
            requests,
            successes: self.successes.load(Ordering::Relaxed),
            failures: self.failures.load(Ordering::Relaxed),
            bytes_sent: self.bytes_sent.load(Ordering::Relaxed),
            bytes_recv: self.bytes_recv.load(Ordering::Relaxed),
            avg_latency_us: if requests > 0 {
                self.latency_sum_us.load(Ordering::Relaxed) / requests
            } else {
                0
            },
            max_latency_us: self.latency_max_us.load(Ordering::Relaxed),
            min_latency_us: {
                let min = self.latency_min_us.load(Ordering::Relaxed);
                if min == u64::MAX {
                    0
                } else {
                    min
                }
            },
            success_rate: if requests > 0 {
                self.successes.load(Ordering::Relaxed) as f64 / requests as f64
            } else {
                0.0
            },
        }
    }
}

/// SQLite operation type
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SqliteOp {
    Select,
    Insert,
    Update,
    Delete,
    Transaction,
}

/// SQLite-specific metrics
#[derive(Debug)]
pub struct SqliteMetrics {
    pub selects: AtomicU64,
    pub select_latency_sum_us: AtomicU64,
    pub select_rows: AtomicU64,
    pub inserts: AtomicU64,
    pub insert_latency_sum_us: AtomicU64,
    pub insert_rows: AtomicU64,
    pub updates: AtomicU64,
    pub update_latency_sum_us: AtomicU64,
    pub deletes: AtomicU64,
    pub delete_latency_sum_us: AtomicU64,
    pub transactions: AtomicU64,
    pub transaction_latency_sum_us: AtomicU64,
    pub max_latency_us: AtomicU64,
}

impl SqliteMetrics {
    pub fn new() -> Self {
        Self {
            selects: AtomicU64::new(0),
            select_latency_sum_us: AtomicU64::new(0),
            select_rows: AtomicU64::new(0),
            inserts: AtomicU64::new(0),
            insert_latency_sum_us: AtomicU64::new(0),
            insert_rows: AtomicU64::new(0),
            updates: AtomicU64::new(0),
            update_latency_sum_us: AtomicU64::new(0),
            deletes: AtomicU64::new(0),
            delete_latency_sum_us: AtomicU64::new(0),
            transactions: AtomicU64::new(0),
            transaction_latency_sum_us: AtomicU64::new(0),
            max_latency_us: AtomicU64::new(0),
        }
    }

    pub fn record(&self, op: SqliteOp, latency_us: u64, rows: u64) {
        self.max_latency_us.fetch_max(latency_us, Ordering::Relaxed);

        match op {
            SqliteOp::Select => {
                self.selects.fetch_add(1, Ordering::Relaxed);
                self.select_latency_sum_us
                    .fetch_add(latency_us, Ordering::Relaxed);
                self.select_rows.fetch_add(rows, Ordering::Relaxed);
            }
            SqliteOp::Insert => {
                self.inserts.fetch_add(1, Ordering::Relaxed);
                self.insert_latency_sum_us
                    .fetch_add(latency_us, Ordering::Relaxed);
                self.insert_rows.fetch_add(rows, Ordering::Relaxed);
            }
            SqliteOp::Update => {
                self.updates.fetch_add(1, Ordering::Relaxed);
                self.update_latency_sum_us
                    .fetch_add(latency_us, Ordering::Relaxed);
            }
            SqliteOp::Delete => {
                self.deletes.fetch_add(1, Ordering::Relaxed);
                self.delete_latency_sum_us
                    .fetch_add(latency_us, Ordering::Relaxed);
            }
            SqliteOp::Transaction => {
                self.transactions.fetch_add(1, Ordering::Relaxed);
                self.transaction_latency_sum_us
                    .fetch_add(latency_us, Ordering::Relaxed);
            }
        }
    }

    pub fn snapshot(&self) -> SqliteSnapshot {
        let selects = self.selects.load(Ordering::Relaxed);
        let inserts = self.inserts.load(Ordering::Relaxed);
        let updates = self.updates.load(Ordering::Relaxed);
        let deletes = self.deletes.load(Ordering::Relaxed);
        let transactions = self.transactions.load(Ordering::Relaxed);

        SqliteSnapshot {
            selects,
            select_avg_latency_us: if selects > 0 {
                self.select_latency_sum_us.load(Ordering::Relaxed) / selects
            } else {
                0
            },
            select_rows: self.select_rows.load(Ordering::Relaxed),
            inserts,
            insert_avg_latency_us: if inserts > 0 {
                self.insert_latency_sum_us.load(Ordering::Relaxed) / inserts
            } else {
                0
            },
            insert_rows: self.insert_rows.load(Ordering::Relaxed),
            updates,
            update_avg_latency_us: if updates > 0 {
                self.update_latency_sum_us.load(Ordering::Relaxed) / updates
            } else {
                0
            },
            deletes,
            delete_avg_latency_us: if deletes > 0 {
                self.delete_latency_sum_us.load(Ordering::Relaxed) / deletes
            } else {
                0
            },
            transactions,
            transaction_avg_latency_us: if transactions > 0 {
                self.transaction_latency_sum_us.load(Ordering::Relaxed) / transactions
            } else {
                0
            },
            max_latency_us: self.max_latency_us.load(Ordering::Relaxed),
        }
    }
}

impl Default for SqliteMetrics {
    fn default() -> Self {
        Self::new()
    }
}

// Snapshot types for serialization

#[derive(Debug, Clone, Serialize)]
pub struct IoSnapshot {
    pub disk: DiskSnapshot,
    pub network: Vec<NetworkSnapshot>,
    pub sqlite: SqliteSnapshot,
}

#[derive(Debug, Clone, Serialize)]
pub struct DiskSnapshot {
    pub reads: u64,
    pub read_bytes: u64,
    pub read_avg_latency_us: u64,
    pub read_max_latency_us: u64,
    pub writes: u64,
    pub write_bytes: u64,
    pub write_avg_latency_us: u64,
    pub write_max_latency_us: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct NetworkSnapshot {
    pub endpoint: String,
    pub requests: u64,
    pub successes: u64,
    pub failures: u64,
    pub bytes_sent: u64,
    pub bytes_recv: u64,
    pub avg_latency_us: u64,
    pub max_latency_us: u64,
    pub min_latency_us: u64,
    pub success_rate: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct SqliteSnapshot {
    pub selects: u64,
    pub select_avg_latency_us: u64,
    pub select_rows: u64,
    pub inserts: u64,
    pub insert_avg_latency_us: u64,
    pub insert_rows: u64,
    pub updates: u64,
    pub update_avg_latency_us: u64,
    pub deletes: u64,
    pub delete_avg_latency_us: u64,
    pub transactions: u64,
    pub transaction_avg_latency_us: u64,
    pub max_latency_us: u64,
}

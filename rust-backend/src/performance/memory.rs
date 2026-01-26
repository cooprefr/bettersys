//! Memory Profiling
//!
//! Tracks heap allocations, peak usage, and memory pressure.
//! Uses a combination of:
//! - System memory stats (via sysinfo crate)
//! - Allocation tracking (optional, via tracking allocator)
//! - Component-level memory estimates

use parking_lot::RwLock;
use serde::Serialize;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

/// Memory profiler tracking allocations and system memory
#[derive(Debug)]
pub struct MemoryProfiler {
    /// Current estimated heap usage (bytes)
    pub heap_bytes: AtomicU64,
    /// Peak heap usage observed
    pub peak_heap_bytes: AtomicU64,
    /// Total allocations since start
    pub total_allocations: AtomicU64,
    /// Total deallocations since start
    pub total_deallocations: AtomicU64,
    /// Total bytes allocated (cumulative)
    pub total_bytes_allocated: AtomicU64,
    /// Total bytes deallocated (cumulative)
    pub total_bytes_deallocated: AtomicU64,
    /// Large allocation threshold (bytes)
    pub large_alloc_threshold: usize,
    /// Count of large allocations
    pub large_allocations: AtomicU64,
    /// Per-component memory tracking
    pub components: RwLock<Vec<ComponentMemory>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ComponentMemory {
    pub name: String,
    pub estimated_bytes: u64,
    pub item_count: u64,
    pub description: String,
}

impl MemoryProfiler {
    pub fn new() -> Self {
        Self {
            heap_bytes: AtomicU64::new(0),
            peak_heap_bytes: AtomicU64::new(0),
            total_allocations: AtomicU64::new(0),
            total_deallocations: AtomicU64::new(0),
            total_bytes_allocated: AtomicU64::new(0),
            total_bytes_deallocated: AtomicU64::new(0),
            large_alloc_threshold: 1024 * 1024, // 1MB
            large_allocations: AtomicU64::new(0),
            components: RwLock::new(Vec::new()),
        }
    }

    /// Record an allocation
    #[inline]
    pub fn record_alloc(&self, size: usize) {
        let size = size as u64;
        self.total_allocations.fetch_add(1, Ordering::Relaxed);
        self.total_bytes_allocated
            .fetch_add(size, Ordering::Relaxed);

        let new_heap = self.heap_bytes.fetch_add(size, Ordering::Relaxed) + size;

        // Update peak if necessary
        let mut peak = self.peak_heap_bytes.load(Ordering::Relaxed);
        while new_heap > peak {
            match self.peak_heap_bytes.compare_exchange_weak(
                peak,
                new_heap,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(p) => peak = p,
            }
        }

        if size as usize >= self.large_alloc_threshold {
            self.large_allocations.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Record a deallocation
    #[inline]
    pub fn record_dealloc(&self, size: usize) {
        let size = size as u64;
        self.total_deallocations.fetch_add(1, Ordering::Relaxed);
        self.total_bytes_deallocated
            .fetch_add(size, Ordering::Relaxed);
        self.heap_bytes.fetch_sub(
            size.min(self.heap_bytes.load(Ordering::Relaxed)),
            Ordering::Relaxed,
        );
    }

    /// Update component memory estimate
    pub fn update_component(&self, name: &str, bytes: u64, items: u64, description: &str) {
        let mut components = self.components.write();
        if let Some(c) = components.iter_mut().find(|c| c.name == name) {
            c.estimated_bytes = bytes;
            c.item_count = items;
            c.description = description.to_string();
        } else {
            components.push(ComponentMemory {
                name: name.to_string(),
                estimated_bytes: bytes,
                item_count: items,
                description: description.to_string(),
            });
        }
    }

    /// Get system memory info using sysinfo crate
    /// Uses a cached System instance to avoid expensive re-initialization
    pub fn system_memory(&self) -> SystemMemory {
        use parking_lot::Mutex;
        use std::sync::OnceLock;
        use sysinfo::{MemoryRefreshKind, Pid, ProcessRefreshKind, RefreshKind, System};

        // Cache the System instance - creating it is expensive
        static CACHED_SYSTEM: OnceLock<Mutex<System>> = OnceLock::new();

        let sys_mutex = CACHED_SYSTEM.get_or_init(|| {
            // Create with minimal initial data
            Mutex::new(System::new())
        });

        let mut sys = sys_mutex.lock();

        // Only refresh what we need - memory info
        sys.refresh_memory();

        // Get system-wide memory
        let total = sys.total_memory();
        let available = sys.available_memory();
        let used = sys.used_memory();

        // Get process-specific memory
        let pid = Pid::from_u32(std::process::id());
        // Only refresh our specific process
        sys.refresh_processes_specifics(
            sysinfo::ProcessesToUpdate::Some(&[pid]),
            ProcessRefreshKind::new().with_memory(),
        );

        let (resident, virtual_mem) = if let Some(process) = sys.process(pid) {
            (process.memory(), process.virtual_memory())
        } else {
            (0, 0)
        };

        SystemMemory {
            total_bytes: total,
            available_bytes: available,
            used_bytes: used,
            process_resident_bytes: resident,
            process_virtual_bytes: virtual_mem,
        }
    }

    /// Get a snapshot of memory state
    pub fn snapshot(&self) -> MemorySnapshot {
        MemorySnapshot {
            heap_bytes: self.heap_bytes.load(Ordering::Relaxed),
            peak_heap_bytes: self.peak_heap_bytes.load(Ordering::Relaxed),
            total_allocations: self.total_allocations.load(Ordering::Relaxed),
            total_deallocations: self.total_deallocations.load(Ordering::Relaxed),
            total_bytes_allocated: self.total_bytes_allocated.load(Ordering::Relaxed),
            total_bytes_deallocated: self.total_bytes_deallocated.load(Ordering::Relaxed),
            large_allocations: self.large_allocations.load(Ordering::Relaxed),
            allocation_rate: self.allocation_rate(),
            components: self.components.read().clone(),
            system: self.system_memory(),
        }
    }

    /// Calculate allocation rate (allocations per second)
    fn allocation_rate(&self) -> f64 {
        // This would need timing context; return 0 for now
        0.0
    }
}

impl Default for MemoryProfiler {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct MemorySnapshot {
    pub heap_bytes: u64,
    pub peak_heap_bytes: u64,
    pub total_allocations: u64,
    pub total_deallocations: u64,
    pub total_bytes_allocated: u64,
    pub total_bytes_deallocated: u64,
    pub large_allocations: u64,
    pub allocation_rate: f64,
    pub components: Vec<ComponentMemory>,
    pub system: SystemMemory,
}

#[derive(Debug, Clone, Serialize)]
pub struct SystemMemory {
    pub total_bytes: u64,
    pub available_bytes: u64,
    pub used_bytes: u64,
    pub process_resident_bytes: u64,
    pub process_virtual_bytes: u64,
}

/// Helper to format bytes as human-readable
pub fn format_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;

    if bytes >= GB {
        format!("{:.2} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.2} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.2} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} B", bytes)
    }
}

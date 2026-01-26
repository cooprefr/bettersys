//! Custom Allocator for Memory Tracking
//!
//! Wraps the global allocator to track allocations.
//! Enable with feature flag for detailed memory profiling.
//!
//! Note: This has overhead and should only be used in profiling builds.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

/// Tracking allocator that wraps System allocator
pub struct TrackingAllocator {
    inner: System,
}

impl TrackingAllocator {
    pub const fn new() -> Self {
        Self { inner: System }
    }
}

// Global counters for allocation tracking
pub static ALLOCATED_BYTES: AtomicUsize = AtomicUsize::new(0);
pub static DEALLOCATED_BYTES: AtomicUsize = AtomicUsize::new(0);
pub static ALLOCATION_COUNT: AtomicUsize = AtomicUsize::new(0);
pub static DEALLOCATION_COUNT: AtomicUsize = AtomicUsize::new(0);
pub static PEAK_ALLOCATED: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for TrackingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = self.inner.alloc(layout);
        if !ptr.is_null() {
            let size = layout.size();
            ALLOCATION_COUNT.fetch_add(1, Ordering::Relaxed);
            let prev = ALLOCATED_BYTES.fetch_add(size, Ordering::Relaxed);
            let current = prev + size;

            // Update peak
            let mut peak = PEAK_ALLOCATED.load(Ordering::Relaxed);
            while current > peak {
                match PEAK_ALLOCATED.compare_exchange_weak(
                    peak,
                    current,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => break,
                    Err(p) => peak = p,
                }
            }
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        let size = layout.size();
        DEALLOCATION_COUNT.fetch_add(1, Ordering::Relaxed);
        DEALLOCATED_BYTES.fetch_add(size, Ordering::Relaxed);
        self.inner.dealloc(ptr, layout);
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let old_size = layout.size();
        let new_ptr = self.inner.realloc(ptr, layout, new_size);

        if !new_ptr.is_null() {
            // Track the size change
            if new_size > old_size {
                let diff = new_size - old_size;
                let prev = ALLOCATED_BYTES.fetch_add(diff, Ordering::Relaxed);
                let current = prev + diff;

                let mut peak = PEAK_ALLOCATED.load(Ordering::Relaxed);
                while current > peak {
                    match PEAK_ALLOCATED.compare_exchange_weak(
                        peak,
                        current,
                        Ordering::Relaxed,
                        Ordering::Relaxed,
                    ) {
                        Ok(_) => break,
                        Err(p) => peak = p,
                    }
                }
            } else if new_size < old_size {
                let diff = old_size - new_size;
                DEALLOCATED_BYTES.fetch_add(diff, Ordering::Relaxed);
            }
        }
        new_ptr
    }
}

/// Get current allocation statistics
pub fn allocation_stats() -> AllocationStats {
    let allocated = ALLOCATED_BYTES.load(Ordering::Relaxed);
    let deallocated = DEALLOCATED_BYTES.load(Ordering::Relaxed);

    AllocationStats {
        current_bytes: allocated.saturating_sub(deallocated),
        peak_bytes: PEAK_ALLOCATED.load(Ordering::Relaxed),
        total_allocated_bytes: allocated,
        total_deallocated_bytes: deallocated,
        allocation_count: ALLOCATION_COUNT.load(Ordering::Relaxed),
        deallocation_count: DEALLOCATION_COUNT.load(Ordering::Relaxed),
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct AllocationStats {
    pub current_bytes: usize,
    pub peak_bytes: usize,
    pub total_allocated_bytes: usize,
    pub total_deallocated_bytes: usize,
    pub allocation_count: usize,
    pub deallocation_count: usize,
}

impl AllocationStats {
    pub fn live_allocations(&self) -> usize {
        self.allocation_count
            .saturating_sub(self.deallocation_count)
    }

    pub fn avg_allocation_size(&self) -> f64 {
        if self.allocation_count > 0 {
            self.total_allocated_bytes as f64 / self.allocation_count as f64
        } else {
            0.0
        }
    }
}

/// Reset allocation counters (useful for benchmarking)
pub fn reset_counters() {
    ALLOCATED_BYTES.store(0, Ordering::Relaxed);
    DEALLOCATED_BYTES.store(0, Ordering::Relaxed);
    ALLOCATION_COUNT.store(0, Ordering::Relaxed);
    DEALLOCATION_COUNT.store(0, Ordering::Relaxed);
    PEAK_ALLOCATED.store(0, Ordering::Relaxed);
}

// To use the tracking allocator, uncomment the following in main.rs:
//
// #[cfg(feature = "tracking_alloc")]
// #[global_allocator]
// static ALLOCATOR: crate::performance::allocator::TrackingAllocator =
//     crate::performance::allocator::TrackingAllocator::new();

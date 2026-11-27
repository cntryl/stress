//! Optional process-wide allocation counters for stress samples.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicU64, Ordering};

static ALLOCS: AtomicU64 = AtomicU64::new(0);
static BYTES: AtomicU64 = AtomicU64::new(0);

/// Link-time marker populated by `stress_allocator!()`.
#[doc(hidden)]
#[linkme::distributed_slice]
pub static STRESS_ALLOCATOR_INSTALLATIONS: [fn()];

/// Global allocator wrapper used by [`crate::stress_allocator!`].
pub struct StressAllocator;

impl StressAllocator {
    /// Create a stress allocation-counting allocator.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl Default for StressAllocator {
    fn default() -> Self {
        Self::new()
    }
}

/// Test-only probe proving `alloc_zeroed` is forwarded to `System`.
#[cfg(test)]
static ZEROED_FORWARDS: AtomicU64 = AtomicU64::new(0);

fn record_allocation(size: usize) {
    ALLOCS.fetch_add(1, Ordering::Relaxed);
    BYTES.fetch_add(saturating_u64(size), Ordering::Relaxed);
}

/// Accounting for a successful `realloc` from `old_size` to `new_size`.
///
/// Returns `(allocations, bytes)`. A growing realloc counts as one allocation
/// event and only the growth (`new_size - old_size`) is counted as newly
/// allocated bytes; a shrinking or same-size realloc counts nothing. This keeps
/// amortized growth (e.g. `Vec::push`) from being reported as re-allocating the
/// whole buffer on every resize.
fn realloc_accounting(old_size: usize, new_size: usize) -> (u64, u64) {
    if new_size > old_size {
        (1, saturating_u64(new_size - old_size))
    } else {
        (0, 0)
    }
}

// SAFETY: this type delegates allocation operations to `std::alloc::System` and
// only updates atomics before returning the allocator result.
unsafe impl GlobalAlloc for StressAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            record_allocation(layout.size());
        }
        ptr
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        #[cfg(test)]
        ZEROED_FORWARDS.fetch_add(1, Ordering::Relaxed);
        let ptr = unsafe { System.alloc_zeroed(layout) };
        if !ptr.is_null() {
            record_allocation(layout.size());
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let new_ptr = unsafe { System.realloc(ptr, layout, new_size) };
        if !new_ptr.is_null() {
            let (allocs, bytes) = realloc_accounting(layout.size(), new_size);
            ALLOCS.fetch_add(allocs, Ordering::Relaxed);
            BYTES.fetch_add(bytes, Ordering::Relaxed);
        }
        new_ptr
    }
}

/// Allocation counter snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AllocationSnapshot {
    allocs: u64,
    bytes: u64,
}

/// Allocation counter delta.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AllocationDelta {
    pub allocs: u64,
    pub bytes: u64,
}

pub(crate) fn snapshot() -> Option<AllocationSnapshot> {
    allocation_tracking_available().then(|| AllocationSnapshot {
        allocs: ALLOCS.load(Ordering::Relaxed),
        bytes: BYTES.load(Ordering::Relaxed),
    })
}

pub(crate) fn delta_since(start: AllocationSnapshot) -> AllocationDelta {
    AllocationDelta {
        allocs: ALLOCS.load(Ordering::Relaxed).saturating_sub(start.allocs),
        bytes: BYTES.load(Ordering::Relaxed).saturating_sub(start.bytes),
    }
}

pub(crate) fn allocation_tracking_available() -> bool {
    !STRESS_ALLOCATOR_INSTALLATIONS.is_empty()
}

/// Marker function referenced by the allocator macro.
#[doc(hidden)]
pub fn stress_allocator_installed_marker() {}

fn saturating_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alloc_zeroed_forwards_to_system_calloc_path() {
        let allocator = StressAllocator::new();
        let layout = Layout::from_size_align(256, 8).unwrap();
        let before = ZEROED_FORWARDS.load(Ordering::SeqCst);
        let ptr = unsafe { allocator.alloc_zeroed(layout) };
        assert!(!ptr.is_null());
        let bytes = unsafe { std::slice::from_raw_parts(ptr, 256) };
        assert!(bytes.iter().all(|b| *b == 0));
        unsafe { allocator.dealloc(ptr, layout) };
        assert!(ZEROED_FORWARDS.load(Ordering::SeqCst) > before);
    }

    #[test]
    fn realloc_accounting_counts_only_growth() {
        assert_eq!(realloc_accounting(64, 100), (1, 36));
        assert_eq!(realloc_accounting(100, 10), (0, 0));
        assert_eq!(realloc_accounting(64, 64), (0, 0));
    }
}

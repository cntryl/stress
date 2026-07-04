//! Optional process-wide allocation counters for stress samples.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

static TRACKING_OBSERVED: AtomicBool = AtomicBool::new(false);
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

// SAFETY: this type delegates allocation operations to `std::alloc::System` and
// only updates atomics before returning the allocator result.
unsafe impl GlobalAlloc for StressAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        TRACKING_OBSERVED.store(true, Ordering::Relaxed);
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
            BYTES.fetch_add(saturating_u64(layout.size()), Ordering::Relaxed);
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        TRACKING_OBSERVED.store(true, Ordering::Relaxed);
        unsafe { System.dealloc(ptr, layout) };
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        TRACKING_OBSERVED.store(true, Ordering::Relaxed);
        let new_ptr = unsafe { System.realloc(ptr, layout, new_size) };
        if !new_ptr.is_null() {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
            BYTES.fetch_add(saturating_u64(new_size), Ordering::Relaxed);
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

//! Observation-window counters, not a measurement of total live heap ownership.
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

pub(super) struct CountingAllocator {
    enabled: AtomicBool,
    allocated: AtomicU64,
    freed: AtomicU64,
}

impl CountingAllocator {
    pub(super) const fn new() -> Self {
        Self {
            enabled: AtomicBool::new(false),
            allocated: AtomicU64::new(0),
            freed: AtomicU64::new(0),
        }
    }
    pub(super) fn track(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Relaxed);
    }
    pub(super) fn allocated(&self) -> u64 {
        self.allocated.load(Ordering::Relaxed)
    }
    pub(super) fn freed(&self) -> u64 {
        self.freed.load(Ordering::Relaxed)
    }
}

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let track = self.enabled.load(Ordering::Relaxed);
        let result = unsafe { System.alloc(layout) };
        if track && !result.is_null() {
            self.allocated
                .fetch_add(layout.size() as u64, Ordering::Relaxed);
        }
        result
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        if self.enabled.load(Ordering::Relaxed) {
            self.freed
                .fetch_add(layout.size() as u64, Ordering::Relaxed);
        }
        unsafe { System.dealloc(pointer, layout) }
    }
    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        let track = self.enabled.load(Ordering::Relaxed);
        let result = unsafe { System.realloc(pointer, layout, size) };
        // A failed realloc leaves the original allocation owned by the caller.
        if track && !result.is_null() {
            self.allocated.fetch_add(size as u64, Ordering::Relaxed);
            self.freed
                .fetch_add(layout.size() as u64, Ordering::Relaxed);
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn observation_counts_successful_realloc_and_releases_without_resetting() {
        let allocator = CountingAllocator::new();
        let layout = Layout::from_size_align(16, 8).unwrap();
        // SAFETY: sizes are nonzero, layouts match, and each nonnull allocation
        // is accessed only while owned, then released exactly once.
        unsafe {
            let p = allocator.alloc(layout);
            assert!(!p.is_null());
            assert_eq!(allocator.allocated(), 0);
            allocator.track(true);
            let p = allocator.realloc(p, layout, 32);
            assert!(!p.is_null());
            assert_eq!((allocator.allocated(), allocator.freed()), (32, 16));
            let p = allocator.realloc(p, Layout::from_size_align(32, 8).unwrap(), 8);
            assert!(!p.is_null());
            allocator.dealloc(p, Layout::from_size_align(8, 8).unwrap());
            assert_eq!((allocator.allocated(), allocator.freed()), (40, 56));
            allocator.track(false);
            let p = allocator.alloc(layout);
            assert!(!p.is_null());
            allocator.dealloc(p, layout);
            assert_eq!((allocator.allocated(), allocator.freed()), (40, 56));
        }
    }
}

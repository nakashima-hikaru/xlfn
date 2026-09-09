//! Allocation counts only. This allocator is never linked into the timing binary.
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use xlfn::benchmark_support::{RtdExistingBreakdownBenchmark, RtdExistingBreakdownCase};

struct CountedSystem;
static TRACK: AtomicBool = AtomicBool::new(false);
static ALLOCS: AtomicUsize = AtomicUsize::new(0);
static DEALLOCS: AtomicUsize = AtomicUsize::new(0);
static REALLOCS: AtomicUsize = AtomicUsize::new(0);
static FREED_BYTES: AtomicUsize = AtomicUsize::new(0);

// SAFETY: every operation forwards the unmodified pointer/layout contract to
// System; the counters neither access allocated memory nor allocate themselves.
unsafe impl GlobalAlloc for CountedSystem {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if TRACK.load(Ordering::Relaxed) {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
        }
        // SAFETY: the caller supplies the layout required by GlobalAlloc.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        if TRACK.load(Ordering::Relaxed) {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
        }
        // SAFETY: the caller supplies the layout required by GlobalAlloc.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if TRACK.load(Ordering::Relaxed) {
            DEALLOCS.fetch_add(1, Ordering::Relaxed);
            FREED_BYTES.fetch_add(layout.size(), Ordering::Relaxed);
        }
        // SAFETY: pointer and layout are forwarded unchanged to their allocator.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if TRACK.load(Ordering::Relaxed) {
            REALLOCS.fetch_add(1, Ordering::Relaxed);
        }
        // SAFETY: the caller guarantees the old allocation and valid new size.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: CountedSystem = CountedSystem;

fn main() {
    for case in RtdExistingBreakdownCase::ALL {
        for parts in [1, 10] {
            let mut fixture = RtdExistingBreakdownBenchmark::new(256, parts, case);
            for count in [&ALLOCS, &DEALLOCS, &REALLOCS, &FREED_BYTES] {
                count.store(0, Ordering::Relaxed);
            }
            TRACK.store(true, Ordering::SeqCst);
            fixture.run(case);
            TRACK.store(false, Ordering::SeqCst);
            println!(
                "{}",
                serde_json::json!({
                    "case": case.name(), "parts": parts, "subscriptions": 256,
                    "allocations": ALLOCS.load(Ordering::Relaxed),
                    "deallocations": DEALLOCS.load(Ordering::Relaxed),
                    "reallocations": REALLOCS.load(Ordering::Relaxed),
                    "freed_requested_bytes": FREED_BYTES.load(Ordering::Relaxed),
                })
            );
        }
    }
}

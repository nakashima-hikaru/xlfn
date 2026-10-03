//! Production task-drain allocation probe, separate from Criterion timing.
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use xlfn::benchmark_support::AsyncTaskDrainBenchmark;

struct CountingAllocator;
static ENABLED: AtomicBool = AtomicBool::new(false);
static ALLOCS: AtomicUsize = AtomicUsize::new(0);
static REALLOCS: AtomicUsize = AtomicUsize::new(0);
static BYTES: AtomicUsize = AtomicUsize::new(0);

// SAFETY: all allocations and layouts are forwarded unchanged to System.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if ENABLED.load(Ordering::Relaxed) {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
            BYTES.fetch_add(layout.size(), Ordering::Relaxed);
        }
        // SAFETY: the allocator contract supplies a valid layout.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        // SAFETY: pointer and layout came from the delegated allocator.
        unsafe { System.dealloc(pointer, layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        if ENABLED.load(Ordering::Relaxed) {
            REALLOCS.fetch_add(1, Ordering::Relaxed);
            BYTES.fetch_add(size, Ordering::Relaxed);
        }
        // SAFETY: the caller supplies a live allocation and valid new size.
        unsafe { System.realloc(pointer, layout, size) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

fn main() {
    let (control_bytes, batch_bytes) = AsyncTaskDrainBenchmark::sizes();
    println!(
        "{}",
        serde_json::json!({"control_bytes": control_bytes, "batch_bytes": batch_bytes})
    );
    for count in [0, 1, 4, 5, 32, 128] {
        // Warm cancellation slot recycling before measuring batch storage.
        // Its free-list growth is independent of the collection under review.
        assert_eq!(AsyncTaskDrainBenchmark::new(count).run(), count);
        let benchmark = AsyncTaskDrainBenchmark::new(count);
        ALLOCS.store(0, Ordering::Relaxed);
        REALLOCS.store(0, Ordering::Relaxed);
        BYTES.store(0, Ordering::Relaxed);
        ENABLED.store(true, Ordering::Relaxed);
        let drained = benchmark.run();
        ENABLED.store(false, Ordering::Relaxed);
        assert_eq!(drained, count);
        assert_eq!(ALLOCS.load(Ordering::Relaxed), usize::from(count > 4));
        assert_eq!(REALLOCS.load(Ordering::Relaxed), 0);
        println!(
            "{}",
            serde_json::json!({
                "tasks": count,
                "allocations": ALLOCS.load(Ordering::Relaxed),
                "reallocations": REALLOCS.load(Ordering::Relaxed),
                "requested_bytes": BYTES.load(Ordering::Relaxed),
            })
        );
    }
}

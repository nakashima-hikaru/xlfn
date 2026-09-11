//! Requested live allocation bytes, separate from uninstrumented timing.
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};
use xlfn::benchmark_support::{HandleColdGrowthBenchmark, HandleRevisionChurnBenchmark};

struct CountingAllocator;
static LIVE: AtomicUsize = AtomicUsize::new(0);

// SAFETY: requests and layouts are forwarded unchanged to System.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller supplies a valid layout.
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            LIVE.fetch_add(layout.size(), Ordering::Relaxed);
        }
        pointer
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
        // SAFETY: pointer and layout came from this delegated allocator.
        unsafe { System.dealloc(pointer, layout) }
    }
    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        // SAFETY: the caller supplies a live allocation and valid new size.
        let result = unsafe { System.realloc(pointer, layout, size) };
        if !result.is_null() {
            LIVE.fetch_add(size, Ordering::Relaxed);
            LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
        }
        result
    }
}
#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

fn live() -> usize {
    LIVE.load(Ordering::Relaxed)
}

fn main() {
    // Warm process/TLS state before taking deltas; print only after snapshots.
    drop(HandleColdGrowthBenchmark::new(1));
    println!("{{\"probe\":\"requested_live_bytes_not_rss\"}}");
    for count in [1_000, 10_000] {
        let before = live();
        let bench = HandleColdGrowthBenchmark::new(count);
        let empty = live();
        bench.run();
        let populated = live();
        drop(bench);
        let closed = live();
        println!(
            "{}",
            serde_json::json!({"case": "growth", "count": count,
            "empty_bytes": empty.saturating_sub(before), "populated_bytes": populated.saturating_sub(before),
            "after_drop_bytes": closed as i128 - before as i128})
        );
    }
    let before = live();
    let bench = HandleRevisionChurnBenchmark::new(10_000, 10_000);
    let populated = live();
    bench.run_republish();
    let first = live();
    for _ in 0..49 {
        bench.run_republish();
    }
    let repeated = live();
    drop(bench);
    let closed = live();
    println!(
        "{}",
        serde_json::json!({"case": "churn", "cycles": 500_000,
        "populated_bytes": populated.saturating_sub(before), "after_first_bytes": first.saturating_sub(before),
        "after_repeated_bytes": repeated.saturating_sub(before), "after_drop_bytes": closed as i128 - before as i128})
    );
}

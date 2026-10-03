//! Caller-thread allocation traffic through actual scoped task commit.
//! Worker-side result encoding/callback allocations are excluded deliberately.
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use xlfn::benchmark_support::HandleScopedDeliveryBenchmark;

thread_local! {
    static ENABLED: Cell<bool> = const { Cell::new(false) };
    static ALLOCS: Cell<usize> = const { Cell::new(0) };
    static REALLOCS: Cell<usize> = const { Cell::new(0) };
    static BYTES: Cell<usize> = const { Cell::new(0) };
}

struct CountingAllocator;
// SAFETY: pointers/layouts are forwarded unchanged to the System allocator;
// TLS counters neither allocate nor access the measured allocation itself.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if ENABLED.try_with(Cell::get).unwrap_or(false) {
            ALLOCS.set(ALLOCS.get() + 1);
            BYTES.set(BYTES.get() + layout.size());
        }
        // SAFETY: the caller supplies a valid allocation layout.
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        // SAFETY: the caller supplies a live System allocation and its layout.
        unsafe { System.dealloc(pointer, layout) }
    }
    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        if ENABLED.try_with(Cell::get).unwrap_or(false) {
            REALLOCS.set(REALLOCS.get() + 1);
            BYTES.set(BYTES.get() + size);
        }
        // SAFETY: the caller supplies a live allocation and valid new size.
        unsafe { System.realloc(pointer, layout, size) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

fn main() {
    for instrumented in [false, true] {
        let benchmark = HandleScopedDeliveryBenchmark::new(instrumented);
        // Complete one task at a time to warm all task shards and recycled
        // cancellation slots without introducing concurrent capacity growth.
        for _ in 0..256 {
            benchmark.run(1);
        }
        for count in [1, 4, 32] {
            ALLOCS.set(0);
            REALLOCS.set(0);
            BYTES.set(0);
            ENABLED.set(true);
            for _ in 0..count {
                benchmark.run(1);
            }
            ENABLED.set(false);
            println!(
                "{}",
                serde_json::json!({
                    "instrumented": instrumented, "tasks": count,
                    "caller_allocations": ALLOCS.get(), "caller_reallocations": REALLOCS.get(),
                    "caller_requested_bytes": BYTES.get(),
                })
            );
            assert_eq!(REALLOCS.get(), 0);
        }
    }
}

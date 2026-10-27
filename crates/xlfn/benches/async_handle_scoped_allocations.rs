//! Caller-thread and process-wide allocation traffic through scoped task commit.
//! Process counters include the worker's future and result delivery allocations.
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use xlfn::benchmark_support::HandleScopedDeliveryBenchmark;

static PROCESS_ENABLED: AtomicBool = AtomicBool::new(false);
static PROCESS_ALLOCS: AtomicUsize = AtomicUsize::new(0);
static PROCESS_REALLOCS: AtomicUsize = AtomicUsize::new(0);
static PROCESS_BYTES: AtomicUsize = AtomicUsize::new(0);

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
        if PROCESS_ENABLED.load(Ordering::Relaxed) {
            PROCESS_ALLOCS.fetch_add(1, Ordering::Relaxed);
            PROCESS_BYTES.fetch_add(layout.size(), Ordering::Relaxed);
        }
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
        if PROCESS_ENABLED.load(Ordering::Relaxed) {
            PROCESS_REALLOCS.fetch_add(1, Ordering::Relaxed);
            PROCESS_BYTES.fetch_add(size, Ordering::Relaxed);
        }
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
            PROCESS_ALLOCS.store(0, Ordering::Relaxed);
            PROCESS_REALLOCS.store(0, Ordering::Relaxed);
            PROCESS_BYTES.store(0, Ordering::Relaxed);
            PROCESS_ENABLED.store(true, Ordering::Relaxed);
            ENABLED.set(true);
            for _ in 0..count {
                benchmark.run(1);
            }
            ENABLED.set(false);
            PROCESS_ENABLED.store(false, Ordering::Relaxed);
            println!(
                "{}",
                serde_json::json!({
                    "instrumented": instrumented, "tasks": count,
                    "caller_allocations": ALLOCS.get(), "caller_reallocations": REALLOCS.get(),
                    "caller_requested_bytes": BYTES.get(),
                    "process_allocations": PROCESS_ALLOCS.load(Ordering::Relaxed),
                    "process_reallocations": PROCESS_REALLOCS.load(Ordering::Relaxed),
                    "process_requested_bytes": PROCESS_BYTES.load(Ordering::Relaxed),
                })
            );
            assert_eq!(REALLOCS.get(), 0);
            assert_eq!(PROCESS_REALLOCS.load(Ordering::Relaxed), 0);
            assert_eq!(
                PROCESS_ALLOCS.load(Ordering::Relaxed),
                ALLOCS.get(),
                "warmed scalar scoped delivery must not allocate on workers"
            );
        }
    }
}

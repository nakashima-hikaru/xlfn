//! Process-wide allocation requests through pending task rescheduling/cancel.
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use xlfn::benchmark_support::{AsyncPendingTaskBenchmark, CancellationLifecycleBenchmark};

static ENABLED: AtomicBool = AtomicBool::new(false);
static ALLOCS: AtomicUsize = AtomicUsize::new(0);
static REALLOCS: AtomicUsize = AtomicUsize::new(0);
static BYTES: AtomicUsize = AtomicUsize::new(0);
struct CountingAllocator;
// SAFETY: pointers/layouts are forwarded unchanged to System; counters do not allocate.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if ENABLED.load(Ordering::Relaxed) {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
            BYTES.fetch_add(layout.size(), Ordering::Relaxed);
        }
        // SAFETY: the caller supplies a valid allocation layout.
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        // SAFETY: the caller supplies a live System allocation and its layout.
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
    let report_only = std::env::var_os("XLFN_BENCH_REPORT_ONLY").is_some();
    for cancel in [false, true] {
        for count in [1, 4, 32] {
            let benchmark = AsyncPendingTaskBenchmark::new();
            ALLOCS.store(0, Ordering::Relaxed);
            REALLOCS.store(0, Ordering::Relaxed);
            BYTES.store(0, Ordering::Relaxed);
            ENABLED.store(true, Ordering::Relaxed);
            let (polls, drops) = benchmark.run(count, cancel);
            ENABLED.store(false, Ordering::Relaxed);
            let allocations = ALLOCS.load(Ordering::Relaxed);
            let reallocations = REALLOCS.load(Ordering::Relaxed);
            println!(
                "{}",
                serde_json::json!({
                    "case": if cancel { "pending_cancel" } else { "pending_reschedule_ready" },
                    "tasks": count, "user_polls": polls, "user_drops": drops,
                    "process_allocations": allocations,
                    "process_reallocations": reallocations,
                    "process_requested_bytes": BYTES.load(Ordering::Relaxed),
                    "cancellation_slot_bytes": CancellationLifecycleBenchmark::slot_bytes(),
                    "report_only": report_only,
                })
            );
            if !report_only {
                assert_eq!(reallocations, 0);
                if count <= 4 {
                    assert_eq!(allocations, count);
                } else {
                    // Queue block turnover and cancellation batch storage are
                    // separate costs, and may depend on worker scheduling.
                    assert!(
                        count <= allocations && allocations < 2 * count,
                        "task allocation reduction must survive batch overhead"
                    );
                }
            }
            drop(benchmark);
        }
    }
}

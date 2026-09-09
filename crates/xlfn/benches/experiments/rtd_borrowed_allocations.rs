//! Allocation counts only. This allocator is never linked into the timing binary.
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use xlfn::benchmark_support::RtdBorrowedBenchmark;

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

fn probe<const N: usize, const BORROWED: bool>() {
    let fixture = RtdBorrowedBenchmark::<N>::new(256, true);
    for count in [&ALLOCS, &DEALLOCS, &REALLOCS, &FREED_BYTES] {
        count.store(0, Ordering::Relaxed);
    }
    TRACK.store(true, Ordering::SeqCst);
    fixture.run::<BORROWED>(false);
    TRACK.store(false, Ordering::SeqCst);
    let counts = [
        ALLOCS.load(Ordering::Relaxed),
        REALLOCS.load(Ordering::Relaxed),
        DEALLOCS.load(Ordering::Relaxed),
    ];
    println!(
        "{}",
        serde_json::json!({
            "case": if BORROWED { "borrowed" } else { "owned" }, "parts": N,
            "subscriptions": 256, "allocations": counts[0], "reallocations": counts[1],
            "deallocations": counts[2], "freed_requested_bytes": FREED_BYTES.load(Ordering::Relaxed),
        })
    );
    if BORROWED {
        assert_eq!(counts, [0, 0, 0], "existing borrowed allocation gate");
    }
}
fn main() {
    probe::<1, false>();
    probe::<10, false>();
    probe::<1, true>();
    probe::<10, true>();
}

//! Allocation and retained-payload probe, separate from Criterion timing.
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use xlfn::benchmark_support::{DiagnosticBenchCase, DiagnosticBenchmark};

struct Allocator;
static ENABLED: AtomicBool = AtomicBool::new(false);
static ALLOCS: AtomicUsize = AtomicUsize::new(0);
static REALLOCS: AtomicUsize = AtomicUsize::new(0);
static BYTES: AtomicUsize = AtomicUsize::new(0);
static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

// SAFETY: valid layouts, allocations and pointer ownership are forwarded to System.
unsafe impl GlobalAlloc for Allocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the allocator caller supplies a valid layout.
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            let live = LIVE.fetch_add(layout.size(), Ordering::Relaxed) + layout.size();
            if ENABLED.load(Ordering::Relaxed) {
                ALLOCS.fetch_add(1, Ordering::Relaxed);
                BYTES.fetch_add(layout.size(), Ordering::Relaxed);
                PEAK.fetch_max(live, Ordering::Relaxed);
            }
        }
        pointer
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
        // SAFETY: pointer and layout refer to the live delegated allocation.
        unsafe { System.dealloc(pointer, layout) }
    }
    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        // SAFETY: pointer/layout are valid and the caller supplies the new size.
        let next = unsafe { System.realloc(pointer, layout, size) };
        if !next.is_null() {
            let live = if size >= layout.size() {
                LIVE.fetch_add(size - layout.size(), Ordering::Relaxed) + size - layout.size()
            } else {
                LIVE.fetch_sub(layout.size() - size, Ordering::Relaxed) - (layout.size() - size)
            };
            if ENABLED.load(Ordering::Relaxed) {
                REALLOCS.fetch_add(1, Ordering::Relaxed);
                BYTES.fetch_add(size, Ordering::Relaxed);
                PEAK.fetch_max(live, Ordering::Relaxed);
            }
        }
        next
    }
}
#[global_allocator]
static ALLOCATOR: Allocator = Allocator;

fn main() {
    // Initialize process-wide router and tracing metadata before establishing
    // the retained-memory baseline. No worker runs past a fixture's joined drop.
    drop(DiagnosticBenchmark::new(DiagnosticBenchCase::Disabled));
    for case in [
        DiagnosticBenchCase::Disabled,
        DiagnosticBenchCase::SmallError,
        DiagnosticBenchCase::FullQueue,
        DiagnosticBenchCase::LargeBurst,
        DiagnosticBenchCase::TraceSmall,
        DiagnosticBenchCase::TraceLarge,
    ] {
        let before = LIVE.load(Ordering::Relaxed);
        let fixture = DiagnosticBenchmark::new(case);
        let initialized = LIVE.load(Ordering::Relaxed);
        ALLOCS.store(0, Ordering::Relaxed);
        REALLOCS.store(0, Ordering::Relaxed);
        BYTES.store(0, Ordering::Relaxed);
        PEAK.store(initialized, Ordering::Relaxed);
        ENABLED.store(true, Ordering::Relaxed);
        let result = fixture.run(32);
        ENABLED.store(false, Ordering::Relaxed);
        let allocated = ALLOCS.load(Ordering::Relaxed);
        let reallocated = REALLOCS.load(Ordering::Relaxed);
        let requested = BYTES.load(Ordering::Relaxed);
        let retained = LIVE.load(Ordering::Relaxed) as i128 - initialized as i128;
        let peak = PEAK.load(Ordering::Relaxed).saturating_sub(initialized);
        drop(fixture);
        let after = LIVE.load(Ordering::Relaxed) as i128 - before as i128;
        println!(
            "{}",
            serde_json::json!({"case": case.name(), "attempts": result.attempts,
            "dropped": result.dropped, "formatted_bytes": result.formatted_bytes,
            "allocations": allocated, "reallocations": reallocated, "requested_bytes": requested,
            "setup_live_requested_bytes": initialized as i128 - before as i128,
            "retained_report_requested_bytes": retained, "peak_report_requested_bytes": peak,
            "after_drop_requested_bytes": after})
        );
    }
}

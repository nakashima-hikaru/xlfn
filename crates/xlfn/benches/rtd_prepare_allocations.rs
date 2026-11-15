//! Allocation regression probe; fixture construction and teardown are excluded.
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use xlfn::benchmark_support::{RtdObservationArgumentsBenchmark, RtdPrepareBenchmark};

thread_local! {
    static ENABLED: Cell<bool> = const { Cell::new(false) };
    static ALLOCS: Cell<usize> = const { Cell::new(0) };
    static REALLOCS: Cell<usize> = const { Cell::new(0) };
    static BYTES: Cell<usize> = const { Cell::new(0) };
    static LIVE: Cell<usize> = const { Cell::new(0) };
    static PEAK: Cell<usize> = const { Cell::new(0) };
}

fn change_live(released: usize, acquired: usize) {
    let live = LIVE.get().saturating_sub(released) + acquired;
    LIVE.set(live);
    PEAK.set(PEAK.get().max(live));
}

struct CountingAllocator;
// SAFETY: pointers/layouts pass unchanged to System; TLS counters do not allocate.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if ENABLED.try_with(Cell::get).unwrap_or(false) {
            ALLOCS.set(ALLOCS.get() + 1);
            BYTES.set(BYTES.get() + layout.size());
            change_live(0, layout.size());
        }
        // SAFETY: the caller supplies a valid allocation layout.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        if ENABLED.try_with(Cell::get).unwrap_or(false) {
            change_live(layout.size(), 0);
        }
        // SAFETY: the caller supplies a live System allocation and its layout.
        unsafe { System.dealloc(pointer, layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        if ENABLED.try_with(Cell::get).unwrap_or(false) {
            REALLOCS.set(REALLOCS.get() + 1);
            BYTES.set(BYTES.get() + size);
            change_live(layout.size(), size);
        }
        // SAFETY: the caller supplies a live allocation and valid new size.
        unsafe { System.realloc(pointer, layout, size) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

fn main() {
    for case in [
        "pending_disabled",
        "pending_full",
        "byte_budget",
        "source_disabled",
        "existing_reuse",
    ] {
        for topic_bytes in [16 * 1024, 1024 * 1024] {
            let benchmark = RtdPrepareBenchmark::new_admission(case, topic_bytes);
            let rejected = case != "existing_reuse";
            for _ in 0..8 {
                benchmark.run_admission(rejected);
            }
            ALLOCS.set(0);
            REALLOCS.set(0);
            BYTES.set(0);
            ENABLED.set(true);
            benchmark.run_admission(rejected);
            ENABLED.set(false);
            if std::env::var_os("XLFN_BENCH_REPORT_ONLY").is_none() {
                assert_eq!(
                    (ALLOCS.get(), REALLOCS.get(), BYTES.get()),
                    (0, 0, 0),
                    "rejected and reused topics must not copy owned storage"
                );
            }
            println!(
                "{}",
                serde_json::json!({
                    "case": case,
                    "input": "prevalidated_borrowed_topic",
                    "topic_bytes": topic_bytes,
                    "rejected": rejected,
                    "caller_allocations": ALLOCS.get(),
                    "caller_reallocations": REALLOCS.get(),
                    "caller_requested_bytes": BYTES.get(),
                })
            );
        }
    }
    measure_observation_arguments();
}

fn measure_observation_arguments() {
    let fixture = RtdObservationArgumentsBenchmark::default();
    let [(old_calls, old_bytes, old_peak), (calls, bytes, peak)] = [
        (
            "legacy_owned",
            RtdObservationArgumentsBenchmark::owned as fn(&_),
        ),
        ("borrowed", RtdObservationArgumentsBenchmark::borrowed),
    ]
    .map(|(case, run)| {
        run(&fixture);
        for counter in [&ALLOCS, &REALLOCS, &BYTES, &LIVE, &PEAK] {
            counter.set(0);
        }
        ENABLED.set(true);
        run(&fixture);
        ENABLED.set(false);
        assert_eq!(LIVE.get(), 0);
        const OPERATIONS: u32 = 100_000;
        let start = std::time::Instant::now();
        for _ in 0..OPERATIONS {
            run(std::hint::black_box(&fixture));
        }
        let average_ns = start.elapsed().as_secs_f64() * 1e9 / f64::from(OPERATIONS);
        println!(
            "{}",
            serde_json::json!({
                "case": case,
                "input": "reused_rtd_handle_callback_arguments",
                "caller_allocations": ALLOCS.get(),
                "caller_reallocations": REALLOCS.get(),
                "caller_requested_bytes": BYTES.get(),
                "caller_peak_live_bytes": PEAK.get(),
                "average_ns": average_ns,
            })
        );
        (ALLOCS.get() + REALLOCS.get(), BYTES.get(), PEAK.get())
    });
    if std::env::var_os("XLFN_BENCH_REPORT_ONLY").is_none() {
        assert!(calls < old_calls && bytes < old_bytes && peak < old_peak);
    }
}

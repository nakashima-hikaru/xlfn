//! Allocation-only companion; the timing benchmark uses the uninstrumented allocator.
use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicUsize, Ordering};
use xlfn::{
    benchmark_support::{
        RtdPrepareBenchmark, RtdRefreshScalingBenchmark, RtdRefreshScalingCase,
        RtdRefreshValueKind, rtd_transport_key,
    },
    rtd::RtdTopic,
};

struct CountingAllocator;
static ENABLED: AtomicBool = AtomicBool::new(false);
static ALLOCS: AtomicUsize = AtomicUsize::new(0);
static REALLOCS: AtomicUsize = AtomicUsize::new(0);
static BYTES: AtomicUsize = AtomicUsize::new(0);
static LIVE_BYTES: AtomicIsize = AtomicIsize::new(0);

// SAFETY: allocation/deallocation and layouts are forwarded unchanged to System.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if ENABLED.load(Ordering::Relaxed) {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
            BYTES.fetch_add(layout.size(), Ordering::Relaxed);
        }
        // SAFETY: the allocator contract supplies a valid layout.
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            LIVE_BYTES.fetch_add(layout.size() as isize, Ordering::Relaxed);
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        // SAFETY: pointer and layout came from the same delegated allocator.
        unsafe { System.dealloc(pointer, layout) }
        LIVE_BYTES.fetch_sub(layout.size() as isize, Ordering::Relaxed);
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        if ENABLED.load(Ordering::Relaxed) {
            REALLOCS.fetch_add(1, Ordering::Relaxed);
            BYTES.fetch_add(size, Ordering::Relaxed);
        }
        // SAFETY: the caller supplies a live allocation and a valid new size.
        let replacement = unsafe { System.realloc(pointer, layout, size) };
        if !replacement.is_null() {
            LIVE_BYTES.fetch_add(size as isize - layout.size() as isize, Ordering::Relaxed);
        }
        replacement
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

fn measure(case: &str, operation: impl FnOnce()) {
    ALLOCS.store(0, Ordering::Relaxed);
    REALLOCS.store(0, Ordering::Relaxed);
    BYTES.store(0, Ordering::Relaxed);
    ENABLED.store(true, Ordering::Relaxed);
    operation();
    ENABLED.store(false, Ordering::Relaxed);
    println!(
        "{}",
        serde_json::json!({
            "case": case,
            "allocations": ALLOCS.load(Ordering::Relaxed),
            "reallocations": REALLOCS.load(Ordering::Relaxed),
            "requested_bytes": BYTES.load(Ordering::Relaxed),
        })
    );
}

fn main() {
    measure("transport_key", || {
        rtd_transport_key(black_box(1), black_box(42))
    });
    for (name, part) in [
        ("ascii8", "x".repeat(8)),
        ("ascii23", "x".repeat(23)),
        ("ascii24", "x".repeat(24)),
        ("ascii128", "x".repeat(128)),
        ("unicode21", "漢".repeat(7)),
        ("unicode24", "漢".repeat(8)),
        ("unicode1536", "漢".repeat(512)),
    ] {
        for count in [1, 10] {
            let parts = vec![part.clone(); count];
            measure(&format!("borrowed/{name}/{count}"), || {
                black_box(RtdTopic::new(parts.iter().map(String::as_str)).unwrap());
            });
            let owned = parts.clone();
            measure(&format!("owned/{name}/{count}"), || {
                black_box(RtdTopic::new(owned).unwrap());
            });
            let topic = RtdTopic::new(parts.iter().map(String::as_str)).unwrap();
            measure(&format!("clone/{name}/{count}"), || {
                black_box(topic.clone());
            });
        }
    }
    for parts in [1, 10] {
        for existing in [false, true] {
            let bench = RtdPrepareBenchmark::new(256, parts, existing);
            measure(
                &format!("subscriptions/{parts}/existing={existing}"),
                || {
                    bench.run_subscribe_input(false);
                },
            );
        }
    }
    for topics in [1, 4096] {
        let before = LIVE_BYTES.load(Ordering::Relaxed);
        let benchmark = RtdRefreshScalingBenchmark::new(
            RtdRefreshScalingCase {
                name: "termination",
                active_topics: topics,
                updated_topics: topics,
                ready_shards: 32,
            },
            RtdRefreshValueKind::Number,
        );
        benchmark.run_end_to_end_cycle();
        benchmark.run_end_to_end_cycle();
        let active = LIVE_BYTES.load(Ordering::Relaxed) - before;
        measure(&format!("termination/{topics}"), || {
            benchmark.terminate_server()
        });
        let terminated = LIVE_BYTES.load(Ordering::Relaxed) - before;
        println!(
            "{}",
            serde_json::json!({
                "case": format!("server_footprint/{topics}"),
                "active_bytes": active,
                "terminated_bytes": terminated,
            })
        );
        drop(benchmark);
    }
}

//! Allocation-only companion; the timing benchmark uses the uninstrumented allocator.
use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use xlfn::{benchmark_support::RtdPrepareBenchmark, rtd::RtdTopic};

struct CountingAllocator;
static ENABLED: AtomicBool = AtomicBool::new(false);
static ALLOCS: AtomicUsize = AtomicUsize::new(0);
static REALLOCS: AtomicUsize = AtomicUsize::new(0);
static BYTES: AtomicUsize = AtomicUsize::new(0);

// SAFETY: allocation/deallocation and layouts are forwarded unchanged to System.
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
        // SAFETY: pointer and layout came from the same delegated allocator.
        unsafe { System.dealloc(pointer, layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        if ENABLED.load(Ordering::Relaxed) {
            REALLOCS.fetch_add(1, Ordering::Relaxed);
            BYTES.fetch_add(size, Ordering::Relaxed);
        }
        // SAFETY: the caller supplies a live allocation and a valid new size.
        unsafe { System.realloc(pointer, layout, size) }
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
}

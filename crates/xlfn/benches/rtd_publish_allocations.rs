#![allow(
    unsafe_code,
    reason = "the standalone probe counts and delegates standard global allocator calls"
)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

use xlfn::benchmark_support::RtdPublishStringBenchmark;

struct CountingAllocator;

static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
static REALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

#[global_allocator]
static GLOBAL: CountingAllocator = CountingAllocator;

// SAFETY: All GlobalAlloc operations delegate unchanged to the system allocator.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Relaxed);
        // SAFETY: The caller supplies a valid allocation layout.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: The caller supplies a pointer and layout for the allocation.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        REALLOCATIONS.fetch_add(1, Relaxed);
        // SAFETY: The caller supplies a live allocation and valid new size.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

fn counts(run: impl FnOnce()) -> (usize, usize) {
    ALLOCATIONS.store(0, Relaxed);
    REALLOCATIONS.store(0, Relaxed);
    run();
    (ALLOCATIONS.load(Relaxed), REALLOCATIONS.load(Relaxed))
}

fn main() {
    const ITERATIONS: usize = 1_000;
    println!("bytes,duplicate_percent,publication,allocations,reallocations");
    for bench in [
        RtdPublishStringBenchmark::new(),
        RtdPublishStringBenchmark::with_payload_len(8 * 1024),
    ] {
        for duplicate_percent in [0, 50, 99, 100] {
            let payloads = bench.mixed_payloads(duplicate_percent, ITERATIONS);
            let warm = || bench.run_borrowed_repeated_same(1);
            // The moved workload represents a caller already owning the data.
            // Its setup allocations are deliberately outside the measurement.
            let owned = payloads
                .iter()
                .map(|text| (*text).to_owned())
                .collect::<Vec<_>>();
            warm();
            let moved = counts(|| bench.run_owned_payloads(owned));
            warm();
            let cloned =
                counts(|| bench.run_owned_payloads(payloads.iter().map(|text| (*text).to_owned())));
            warm();
            let borrowed = counts(|| bench.run_borrowed_payloads(&payloads));

            assert_eq!(moved, (0, 0));
            assert_eq!(cloned, (ITERATIONS, 0));
            assert_eq!(borrowed, (ITERATIONS * (100 - duplicate_percent) / 100, 0));
            for (mode, (allocations, reallocations)) in
                [("moved", moved), ("cloned", cloned), ("borrowed", borrowed)]
            {
                println!(
                    "{},{duplicate_percent},{mode},{allocations},{reallocations}",
                    payloads[0].len()
                );
            }
        }
    }
}

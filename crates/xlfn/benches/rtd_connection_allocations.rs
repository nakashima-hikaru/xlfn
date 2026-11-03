//! Allocation gate for the production source adapter and full connection path.
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use xlfn::benchmark_support::{
    rtd_source_catalog_allocation_probe, with_rtd_connection_benchmark,
    with_rtd_connection_memory_benchmark,
};

static ENABLED: AtomicBool = AtomicBool::new(false);
static ALLOCS: AtomicUsize = AtomicUsize::new(0);
static REALLOCS: AtomicUsize = AtomicUsize::new(0);
static BYTES: AtomicUsize = AtomicUsize::new(0);
static RELEASED_BYTES: AtomicUsize = AtomicUsize::new(0);

struct CountingAllocator;
// SAFETY: all pointers and layouts are forwarded unchanged to System.
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
        if ENABLED.load(Ordering::Relaxed) {
            RELEASED_BYTES.fetch_add(layout.size(), Ordering::Relaxed);
        }
        // SAFETY: the pointer and layout came from the delegated allocator.
        unsafe { System.dealloc(pointer, layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        if ENABLED.load(Ordering::Relaxed) {
            REALLOCS.fetch_add(1, Ordering::Relaxed);
            BYTES.fetch_add(size, Ordering::Relaxed);
            RELEASED_BYTES.fetch_add(layout.size(), Ordering::Relaxed);
        }
        // SAFETY: the caller supplies a live allocation and valid new size.
        unsafe { System.realloc(pointer, layout, size) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

fn begin_counting() {
    ALLOCS.store(0, Ordering::Relaxed);
    REALLOCS.store(0, Ordering::Relaxed);
    BYTES.store(0, Ordering::Relaxed);
    RELEASED_BYTES.store(0, Ordering::Relaxed);
    ENABLED.store(true, Ordering::Relaxed);
}

fn retained_bytes_delta() -> i128 {
    BYTES.load(Ordering::Relaxed) as i128 - RELEASED_BYTES.load(Ordering::Relaxed) as i128
}

fn main() {
    let report_only = std::env::var_os("XLFN_BENCH_REPORT_ONLY").is_some();
    for source_count in [1, 4, 16] {
        for static_catalog in [false, true] {
            rtd_source_catalog_allocation_probe(static_catalog, source_count);
            begin_counting();
            rtd_source_catalog_allocation_probe(static_catalog, source_count);
            ENABLED.store(false, Ordering::Relaxed);
            println!(
                "{}",
                serde_json::json!({
                    "case":"source_catalog_lifecycle",
                    "static_catalog":static_catalog,
                    "sources":source_count,
                    "process_allocations":ALLOCS.load(Ordering::Relaxed),
                    "process_reallocations":REALLOCS.load(Ordering::Relaxed),
                    "process_requested_bytes":BYTES.load(Ordering::Relaxed),
                    "process_retained_bytes_delta":retained_bytes_delta(),
                })
            );
            if !report_only {
                assert_eq!(
                    ALLOCS.load(Ordering::Relaxed),
                    if static_catalog { 1 } else { source_count + 1 }
                );
                assert_eq!(retained_bytes_delta(), 0);
            }
        }
    }

    for count in [1, 32, 256] {
        // Warm process/TLS infrastructure before measuring complete ownership.
        with_rtd_connection_benchmark(count, true, false, |operation| operation());
        begin_counting();
        with_rtd_connection_benchmark(count, true, false, |operation| operation());
        ENABLED.store(false, Ordering::Relaxed);
        println!(
            "{}",
            serde_json::json!({
                "case": "adapter_owner_lifecycle",
                "subscriptions": count,
                "process_allocations": ALLOCS.load(Ordering::Relaxed),
                "process_reallocations": REALLOCS.load(Ordering::Relaxed),
                "process_requested_bytes": BYTES.load(Ordering::Relaxed),
                "process_retained_bytes_delta": retained_bytes_delta(),
                "report_only": report_only,
            })
        );
        if !report_only {
            assert_eq!(
                retained_bytes_delta(),
                0,
                "source teardown must reclaim every connection owner"
            );
        }
        for adapter_only in [true, false] {
            for warm in [false, true] {
                with_rtd_connection_benchmark(count, adapter_only, warm, |operation| {
                    begin_counting();
                    operation();
                    ENABLED.store(false, Ordering::Relaxed);
                    let allocations = ALLOCS.load(Ordering::Relaxed);
                    let reallocations = REALLOCS.load(Ordering::Relaxed);
                    println!(
                        "{}",
                        serde_json::json!({
                            "case": if adapter_only { "adapter" } else { "runtime" },
                            "subscriptions": count,
                            "warm": warm,
                            "process_allocations": allocations,
                            "process_reallocations": reallocations,
                            "process_requested_bytes": BYTES.load(Ordering::Relaxed),
                            "process_retained_bytes_delta": retained_bytes_delta(),
                            "report_only": report_only,
                        })
                    );
                    if (warm || adapter_only) && !report_only {
                        // Runtime connection takes one owned canonical-topic
                        // snapshot per connection; source storage adds none.
                        assert_eq!(allocations, if adapter_only { 0 } else { count });
                        assert_eq!(reallocations, 0);
                        if adapter_only {
                            assert_eq!(BYTES.load(Ordering::Relaxed), 0);
                            assert_eq!(retained_bytes_delta(), 0);
                        }
                    }
                });
            }
        }
    }

    for owner_payload_target in [8, 256, 4096] {
        for cleanup_capacity in [1, 32, 256, 4096] {
            // The active variant always owns the same eight-byte value. Only
            // the add-in's closed owner set and terminal queue limit change.
            with_rtd_connection_memory_benchmark(
                1,
                cleanup_capacity,
                owner_payload_target,
                |operation, _, _| operation(),
            );
            with_rtd_connection_memory_benchmark(
                1,
                cleanup_capacity,
                owner_payload_target,
                |operation, common_owner_bytes, connection_owner_bytes| {
                    begin_counting();
                    operation();
                    ENABLED.store(false, Ordering::Relaxed);
                    println!(
                        "{}",
                        serde_json::json!({
                            "case": "cleanup_capacity_cold_cycle",
                            "subscriptions": 1,
                            "cleanup_capacity": cleanup_capacity,
                            "owner_payload_target": owner_payload_target,
                            "common_owner_bytes": common_owner_bytes,
                            "connection_owner_bytes": connection_owner_bytes,
                            "process_allocations": ALLOCS.load(Ordering::Relaxed),
                            "process_reallocations": REALLOCS.load(Ordering::Relaxed),
                            "process_requested_bytes": BYTES.load(Ordering::Relaxed),
                            "process_retained_bytes_delta": retained_bytes_delta(),
                            "report_only": report_only,
                        })
                    );
                },
            );

            begin_counting();
            let (common_owner_bytes, connection_owner_bytes) = with_rtd_connection_memory_benchmark(
                1,
                cleanup_capacity,
                owner_payload_target,
                |operation, common_owner_bytes, connection_owner_bytes| {
                    operation();
                    (common_owner_bytes, connection_owner_bytes)
                },
            );
            ENABLED.store(false, Ordering::Relaxed);
            println!(
                "{}",
                serde_json::json!({
                    "case": "cleanup_capacity_owner_lifecycle",
                    "subscriptions": 1,
                    "cleanup_capacity": cleanup_capacity,
                    "owner_payload_target": owner_payload_target,
                    "common_owner_bytes": common_owner_bytes,
                    "connection_owner_bytes": connection_owner_bytes,
                    "process_allocations": ALLOCS.load(Ordering::Relaxed),
                    "process_reallocations": REALLOCS.load(Ordering::Relaxed),
                    "process_requested_bytes": BYTES.load(Ordering::Relaxed),
                    "process_retained_bytes_delta": retained_bytes_delta(),
                    "report_only": report_only,
                })
            );
            if !report_only {
                assert_eq!(
                    retained_bytes_delta(),
                    0,
                    "runtime teardown must reclaim its complete terminal queue"
                );
            }
        }
    }
}

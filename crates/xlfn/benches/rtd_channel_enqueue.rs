//! Warm enqueue-only allocation gates and Criterion comparison. Each operation
//! enqueues 32 integers and includes the same receive/drain; no publisher runs.
use criterion::{Criterion, Throughput};
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use xlfn::benchmark_support::{RtdChannelEnqueueBenchmark, benchmark_measurement_time};

static ENABLED: AtomicBool = AtomicBool::new(false);
static ALLOCS: AtomicUsize = AtomicUsize::new(0);
static REALLOCS: AtomicUsize = AtomicUsize::new(0);
static DEALLOCS: AtomicUsize = AtomicUsize::new(0);
static BYTES: AtomicUsize = AtomicUsize::new(0);

struct CountingAllocator;

// SAFETY: every pointer and layout passes unchanged to the system allocator.
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
        if ENABLED.load(Ordering::Relaxed) {
            DEALLOCS.fetch_add(1, Ordering::Relaxed);
        }
        // SAFETY: the live pointer and its layout came from this allocator.
        unsafe { System.dealloc(pointer, layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        if ENABLED.load(Ordering::Relaxed) {
            REALLOCS.fetch_add(1, Ordering::Relaxed);
            BYTES.fetch_add(size, Ordering::Relaxed);
        }
        // SAFETY: the caller supplies a live allocation and its new size.
        unsafe { System.realloc(pointer, layout, size) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

fn storage_probe<D: xlfn::rtd::RtdDeliveryPolicy>(policy: D) {
    let fixture = xlfn::benchmark_support::RtdChannelStorageBenchmark::new(policy);
    let layout = fixture.layout();
    for cold in [true, false] {
        ALLOCS.store(0, Ordering::Relaxed);
        REALLOCS.store(0, Ordering::Relaxed);
        DEALLOCS.store(0, Ordering::Relaxed);
        BYTES.store(0, Ordering::Relaxed);
        ENABLED.store(true, Ordering::Relaxed);
        fixture.send_one(31);
        ENABLED.store(false, Ordering::Relaxed);
        println!(
            "{}",
            serde_json::json!({"probe":"rtd_channel_storage_first_send",
            "policy":format!("{:?}",policy.delivery()), "cold":cold, "layout":layout,
            "allocations":ALLOCS.load(Ordering::Relaxed), "reallocations":REALLOCS.load(Ordering::Relaxed),
            "deallocations":DEALLOCS.load(Ordering::Relaxed), "requested_bytes":BYTES.load(Ordering::Relaxed)})
        );
        fixture.drain();
    }
}

fn main() {
    storage_probe(xlfn::rtd::RtdLatest);
    storage_probe(xlfn::rtd::RtdFifo::new(
        std::num::NonZeroUsize::new(32).unwrap(),
    ));
    for batch in [false, true] {
        let fixture = RtdChannelEnqueueBenchmark::new();
        ALLOCS.store(0, Ordering::Relaxed);
        REALLOCS.store(0, Ordering::Relaxed);
        DEALLOCS.store(0, Ordering::Relaxed);
        BYTES.store(0, Ordering::Relaxed);
        ENABLED.store(true, Ordering::Relaxed);
        for _ in 0..10_000 {
            if batch {
                fixture.run_batch();
            } else {
                fixture.run_individual();
            }
        }
        ENABLED.store(false, Ordering::Relaxed);
        let allocations = ALLOCS.load(Ordering::Relaxed);
        let reallocations = REALLOCS.load(Ordering::Relaxed);
        let deallocations = DEALLOCS.load(Ordering::Relaxed);
        let requested_bytes = BYTES.load(Ordering::Relaxed);
        println!(
            "{}",
            serde_json::json!({
                "probe": "rtd_channel_enqueue_32",
                "mode": if batch { "batch" } else { "individual" },
                "cycles": 10_000,
                "values_per_cycle": 32,
                "allocations": allocations,
                "reallocations": reallocations,
                "deallocations": deallocations,
                "requested_bytes": requested_bytes,
            })
        );
        assert_eq!(
            (allocations, reallocations, deallocations, requested_bytes),
            (0, 0, 0, 0)
        );
    }

    let mut criterion = Criterion::default().configure_from_args();
    let mut group = criterion.benchmark_group("rtd_channel_enqueue_32");
    group.measurement_time(benchmark_measurement_time());
    group.throughput(Throughput::Elements(32));
    let fixture = RtdChannelEnqueueBenchmark::new();
    group.bench_function("individual", |b| b.iter(|| fixture.run_individual()));
    group.bench_function("batch", |b| b.iter(|| fixture.run_batch()));
    group.finish();
    let mut storage = criterion.benchmark_group("rtd_channel_storage_32");
    storage.measurement_time(benchmark_measurement_time());
    storage.throughput(Throughput::Elements(32));
    let latest = xlfn::benchmark_support::RtdChannelStorageBenchmark::new(xlfn::rtd::RtdLatest);
    latest.run_cycle();
    storage.bench_function("latest", |b| b.iter(|| latest.run_cycle()));
    let fifo = xlfn::benchmark_support::RtdChannelStorageBenchmark::new(xlfn::rtd::RtdFifo::new(
        std::num::NonZeroUsize::new(32).unwrap(),
    ));
    fifo.run_cycle();
    storage.bench_function("fifo", |b| b.iter(|| fifo.run_cycle()));
    storage.finish();
    criterion.final_summary();
}

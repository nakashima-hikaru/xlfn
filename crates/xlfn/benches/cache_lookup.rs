#![allow(unsafe_code, reason = "Benchmark-only allocation counter")]

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use xlfn::benchmark_support::{
    ArcCacheBenchmark, ArcCacheEvictionBenchmark, CacheLookupBenchCase, CurrentCacheBenchmark,
    CurrentCacheEvictionBenchmark, NoAdmissionCacheBenchmark, NoPinCacheBenchmark,
    benchmark_measurement_time,
};

struct CountingAllocator;

static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
static PROBE_ACTIVE: AtomicBool = AtomicBool::new(false);

// SAFETY: CountingAllocator forwards every operation to the thread-safe System allocator.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if PROBE_ACTIVE.load(Ordering::Relaxed) {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        }
        // SAFETY: The benchmark allocator forwards the caller-provided layout to System.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        if PROBE_ACTIVE.load(Ordering::Relaxed) {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        }
        // SAFETY: The benchmark allocator forwards the caller-provided layout to System.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if PROBE_ACTIVE.load(Ordering::Relaxed) {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        }
        // SAFETY: The benchmark allocator forwards the caller-provided pointer and layouts to System.
        unsafe { System.realloc(ptr, layout, new_size) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: The benchmark allocator forwards the caller-provided pointer and layout to System.
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL_ALLOCATOR: CountingAllocator = CountingAllocator;

const ITERATIONS_PER_WORKER: usize = 1_000;
const EVICTION_ITERATIONS: usize = 100;
const THREAD_COUNTS: [usize; 4] = [1, 2, 8, 32];

fn report_steady_state_allocations(label: &str, run: impl Fn()) {
    run();
    ALLOCATIONS.store(0, Ordering::Relaxed);
    PROBE_ACTIVE.store(true, Ordering::Relaxed);
    run();
    PROBE_ACTIVE.store(false, Ordering::Relaxed);
    let allocations = ALLOCATIONS.load(Ordering::Relaxed);
    println!("allocation_probe/{label}: {allocations} allocations per warm batch");
}

fn cache_lookup_benchmarks(c: &mut Criterion) {
    let mut group = c.benchmark_group("cache_lookup");
    group.measurement_time(benchmark_measurement_time());

    {
        let current =
            CurrentCacheBenchmark::new(CacheLookupBenchCase::HotKey, 1, ITERATIONS_PER_WORKER);
        let no_admission =
            NoAdmissionCacheBenchmark::new(CacheLookupBenchCase::HotKey, 1, ITERATIONS_PER_WORKER);
        let no_pin =
            NoPinCacheBenchmark::new(CacheLookupBenchCase::HotKey, 1, ITERATIONS_PER_WORKER);
        let arc = ArcCacheBenchmark::new(CacheLookupBenchCase::HotKey, 1, ITERATIONS_PER_WORKER);
        report_steady_state_allocations("warm/current", || current.run());
        report_steady_state_allocations("warm/no_admission_control", || no_admission.run());
        report_steady_state_allocations("warm/no_pin_control", || no_pin.run());
        report_steady_state_allocations("warm/arc_control", || arc.run());
        group.throughput(Throughput::Elements(current.total_iterations() as u64));
        group.bench_function(BenchmarkId::new("cache_hit/u64/current", "warm"), |b| {
            b.iter(|| current.run())
        });
        group.bench_function(
            BenchmarkId::new("cache_hit/u64/no_admission_control", "warm"),
            |b| b.iter(|| no_admission.run()),
        );
        group.bench_function(
            BenchmarkId::new("cache_hit/u64/no_pin_control", "warm"),
            |b| b.iter(|| no_pin.run()),
        );
        group.bench_function(BenchmarkId::new("cache_hit/u64/arc_control", "warm"), |b| {
            b.iter(|| arc.run())
        });
    }

    for (case, group_name) in [
        (CacheLookupBenchCase::HotKey, "cache_hit_hot_key"),
        (CacheLookupBenchCase::DisjointKeys, "cache_hit_disjoint"),
    ] {
        for workers in THREAD_COUNTS {
            let current = CurrentCacheBenchmark::new(case, workers, ITERATIONS_PER_WORKER);
            let no_admission = NoAdmissionCacheBenchmark::new(case, workers, ITERATIONS_PER_WORKER);
            let no_pin = NoPinCacheBenchmark::new(case, workers, ITERATIONS_PER_WORKER);
            let arc = ArcCacheBenchmark::new(case, workers, ITERATIONS_PER_WORKER);
            let id = format!("threads_{workers}/u64");
            report_steady_state_allocations(&format!("{}/current", case.name()), || current.run());
            report_steady_state_allocations(
                &format!("{}/no_admission_control", case.name()),
                || no_admission.run(),
            );
            report_steady_state_allocations(&format!("{}/no_pin_control", case.name()), || {
                no_pin.run()
            });
            report_steady_state_allocations(&format!("{}/arc_control", case.name()), || arc.run());
            group.throughput(Throughput::Elements(current.total_iterations() as u64));
            group.bench_with_input(
                BenchmarkId::new(format!("{group_name}/current"), &id),
                &workers,
                |b, _| b.iter(|| current.run()),
            );
            group.bench_with_input(
                BenchmarkId::new(format!("{group_name}/no_admission_control"), &id),
                &workers,
                |b, _| b.iter(|| no_admission.run()),
            );
            group.bench_with_input(
                BenchmarkId::new(format!("{group_name}/no_pin_control"), &id),
                &workers,
                |b, _| b.iter(|| no_pin.run()),
            );
            group.bench_with_input(
                BenchmarkId::new(format!("{group_name}/arc_control"), &id),
                &workers,
                |b, _| b.iter(|| arc.run()),
            );
        }
    }

    let current = CurrentCacheEvictionBenchmark::new(EVICTION_ITERATIONS);
    let arc = ArcCacheEvictionBenchmark::new(EVICTION_ITERATIONS);
    group.throughput(Throughput::Elements(current.total_iterations() as u64));
    group.bench_function("eviction_with_live_lease/current", |b| {
        b.iter(|| current.run())
    });
    group.bench_function("eviction_with_live_lease/arc_control", |b| {
        b.iter(|| arc.run())
    });

    group.finish();
}

criterion_group!(benches, cache_lookup_benchmarks);
criterion_main!(benches);

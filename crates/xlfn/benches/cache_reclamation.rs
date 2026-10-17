#![allow(unsafe_code, reason = "Benchmark-only allocator instrumentation")]

use criterion::{
    BenchmarkGroup, BenchmarkId, Criterion, SamplingMode, Throughput, criterion_group,
    criterion_main, measurement::WallTime,
};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::hash::Hash;
use std::hint::black_box;
use std::sync::mpsc::{Receiver, SyncSender};
use std::sync::{Arc, Barrier};
use std::thread::{self, JoinHandle};
use std::time::Instant;
#[cfg(feature = "bench-internals")]
use xlfn::benchmark_support::benchmark_measurement_time;
use xlfn::cache::{CacheLease, CalculationCache};

// The cache-only build measures production without observation counters.
// Keep the same measurement policy as the instrumented benchmark fixtures.
#[cfg(not(feature = "bench-internals"))]
fn benchmark_measurement_time() -> std::time::Duration {
    std::env::var("XLFN_BENCH_MEASUREMENT_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|&milliseconds| milliseconds != 0)
        .map(std::time::Duration::from_millis)
        .unwrap_or(std::time::Duration::from_secs(10))
}

#[derive(Clone, Copy, Default)]
struct AllocationCounts {
    calls: usize,
    requested_bytes: u64,
}

thread_local! {
    // A const, destructor-free TLS cell avoids allocator recursion. Only the
    // worker's allocation-probe batch enables it; harness/thread setup and
    // Criterion's timing/latency runs are excluded.
    static ALLOCATION_PROBE: Cell<Option<AllocationCounts>> = const { Cell::new(None) };
}

fn record_allocation(bytes: usize) {
    let _ = ALLOCATION_PROBE.try_with(|probe| {
        if let Some(mut counts) = probe.get() {
            counts.calls = counts.calls.saturating_add(1);
            counts.requested_bytes = counts
                .requested_bytes
                .saturating_add(u64::try_from(bytes).unwrap_or(u64::MAX));
            probe.set(Some(counts));
        }
    });
}

struct CountingAllocator;

// SAFETY: All allocator operations preserve the System allocator's contracts.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record_allocation(layout.size());
        // SAFETY: Forward the caller's valid allocation layout to System.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        record_allocation(layout.size());
        // SAFETY: Forward the caller's valid zeroed-allocation layout to System.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        record_allocation(new_size);
        // SAFETY: Forward the original pointer/layout and requested size unchanged.
        unsafe { System.realloc(ptr, layout, new_size) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: Forward the allocation's original pointer and layout unchanged.
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL_ALLOCATOR: CountingAllocator = CountingAllocator;

struct AllocationProbe;

impl AllocationProbe {
    fn start() -> Self {
        ALLOCATION_PROBE.set(Some(AllocationCounts::default()));
        Self
    }

    fn finish(self) -> AllocationCounts {
        ALLOCATION_PROBE
            .replace(None)
            .expect("allocation probe is active")
    }
}

impl Drop for AllocationProbe {
    fn drop(&mut self) {
        ALLOCATION_PROBE.set(None);
    }
}

const RESIDENT_ENTRIES: usize = 16;
const RETAINED_LEASES_PER_WORKER: usize = 32;
const DEFAULT_OPERATIONS_PER_WORKER: usize = 256;
const THREAD_COUNTS: [usize; 3] = [1, 8, 32];
const PAYLOAD_SIZES: [usize; 2] = [64, 64 * 1024];

#[derive(Clone, Copy)]
enum Workload {
    Churn,
    LiveLeases,
}

impl Workload {
    fn name(self) -> &'static str {
        match self {
            Self::Churn => "churn",
            Self::LiveLeases => "live_leases",
        }
    }

    fn retained_per_worker(self) -> usize {
        match self {
            Self::Churn => 0,
            Self::LiveLeases => RETAINED_LEASES_PER_WORKER,
        }
    }
}

#[derive(Clone, Copy)]
enum RunMode {
    Timing,
    Allocations,
    Latency,
}

struct Payload(Box<[u8]>);

#[derive(Default)]
struct BatchReport {
    allocations: AllocationCounts,
    sampled_peak_pending_nodes: Option<usize>,
    sampled_peak_pending_weight: Option<u64>,
    held_leases: usize,
    latencies_ns: Vec<u64>,
}

fn run_worker_batch<'cache, const ALLOCATIONS: bool, const LATENCY: bool>(
    cache: &'cache CalculationCache<u64, Payload>,
    held: &mut [Option<CacheLease<'cache, Payload>>],
    next_key: &mut u64,
    key_stride: u64,
    payload_bytes: usize,
    operations: usize,
) -> BatchReport {
    let mut report = BatchReport {
        latencies_ns: if LATENCY {
            Vec::with_capacity(operations)
        } else {
            Vec::new()
        },
        ..BatchReport::default()
    };
    let probe = ALLOCATIONS.then(AllocationProbe::start);
    for _ in 0..operations {
        let started = LATENCY.then(Instant::now);
        let key = *next_key;
        *next_key = next_key
            .checked_add(key_stride)
            .expect("benchmark key space exhausted");
        let lease = cache
            .get_or_try_insert_with(
                key,
                |value| value.0.len(),
                || Ok(Payload(vec![key as u8; payload_bytes].into_boxed_slice())),
            )
            .expect("reclamation benchmark initialization succeeds");
        black_box((lease.0[0], lease.0[lease.0.len() - 1]));
        if held.is_empty() {
            drop(lease);
        } else {
            // Retain each lease across 32 subsequent insertions on this
            // worker. Releasing the displaced lease belongs to the timed
            // operation, so its grace-period/reclamation cost is not hidden.
            let index = ((key / key_stride) % held.len() as u64) as usize;
            drop(held[index].replace(lease));
        }
        if let Some(started) = started {
            report
                .latencies_ns
                .push(u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX));
            // Observation is outside the individual operation timer. These
            // counters never flush the cache or advance a grace period.
            #[cfg(feature = "bench-internals")]
            {
                let stats = cache.reclamation_stats();
                report.sampled_peak_pending_nodes = report
                    .sampled_peak_pending_nodes
                    .max(Some(stats.pending_nodes));
                report.sampled_peak_pending_weight = report
                    .sampled_peak_pending_weight
                    .max(Some(stats.pending_weight));
            }
        }
    }
    report.allocations = probe.map(AllocationProbe::finish).unwrap_or_default();
    report.held_leases = held.iter().filter(|lease| lease.is_some()).count();
    report
}

struct WorkerPool {
    // Retains the driver owner through worker shutdown, even without metrics.
    _cache: Arc<CalculationCache<u64, Payload>>,
    start: Vec<SyncSender<RunMode>>,
    done: Receiver<BatchReport>,
    barrier: Arc<Barrier>,
    workers: Vec<JoinHandle<()>>,
    operations: usize,
}

impl WorkerPool {
    fn new(
        workload: Workload,
        worker_count: usize,
        payload_bytes: usize,
        operations: usize,
    ) -> Self {
        let cache = Arc::new(CalculationCache::new(RESIDENT_ENTRIES * payload_bytes));
        let barrier = Arc::new(Barrier::new(worker_count + 1));
        let (done_tx, done) = std::sync::mpsc::sync_channel(worker_count);
        let mut start = Vec::with_capacity(worker_count);
        let mut workers = Vec::with_capacity(worker_count);
        for worker in 0..worker_count {
            let cache = Arc::clone(&cache);
            let barrier = Arc::clone(&barrier);
            let done = done_tx.clone();
            let (sender, receiver) = std::sync::mpsc::sync_channel::<RunMode>(1);
            start.push(sender);
            workers.push(thread::spawn(move || {
                let mut held: Vec<Option<CacheLease<'_, Payload>>> =
                    Vec::with_capacity(workload.retained_per_worker());
                held.resize_with(workload.retained_per_worker(), || None);
                let mut next_key = worker as u64;
                // Both thread startup and retention-buffer allocation finish
                // before the driver returns from WorkerPool::new.
                barrier.wait();
                while let Ok(mode) = receiver.recv() {
                    barrier.wait();
                    let report = match mode {
                        RunMode::Timing => run_worker_batch::<false, false>(
                            &cache,
                            &mut held,
                            &mut next_key,
                            worker_count as u64,
                            payload_bytes,
                            operations,
                        ),
                        RunMode::Allocations => run_worker_batch::<true, false>(
                            &cache,
                            &mut held,
                            &mut next_key,
                            worker_count as u64,
                            payload_bytes,
                            operations,
                        ),
                        RunMode::Latency => run_worker_batch::<false, true>(
                            &cache,
                            &mut held,
                            &mut next_key,
                            worker_count as u64,
                            payload_bytes,
                            operations,
                        ),
                    };
                    if done.send(report).is_err() {
                        break;
                    }
                }
            }));
        }
        drop(done_tx);
        barrier.wait();
        Self {
            _cache: cache,
            start,
            done,
            barrier,
            workers,
            operations: operations * worker_count,
        }
    }

    fn run(&self, mode: RunMode) -> BatchReport {
        let mut combined = BatchReport {
            latencies_ns: if matches!(mode, RunMode::Latency) {
                Vec::with_capacity(self.operations)
            } else {
                Vec::new()
            },
            ..BatchReport::default()
        };
        for sender in &self.start {
            sender.send(mode).expect("benchmark worker is available");
        }
        self.barrier.wait();
        for _ in &self.workers {
            let report = self.done.recv().expect("benchmark worker completed");
            combined.allocations.calls = combined
                .allocations
                .calls
                .saturating_add(report.allocations.calls);
            combined.allocations.requested_bytes = combined
                .allocations
                .requested_bytes
                .saturating_add(report.allocations.requested_bytes);
            combined.sampled_peak_pending_nodes = combined
                .sampled_peak_pending_nodes
                .max(report.sampled_peak_pending_nodes);
            combined.sampled_peak_pending_weight = combined
                .sampled_peak_pending_weight
                .max(report.sampled_peak_pending_weight);
            combined.held_leases += report.held_leases;
            combined.latencies_ns.extend(report.latencies_ns);
        }
        combined
    }
}

impl Drop for WorkerPool {
    fn drop(&mut self) {
        // Closing every channel lets workers release their final retained
        // leases before the driver's cache owner is dropped. This final
        // shutdown cleanup is outside all measured batches.
        self.start.clear();
        for worker in self.workers.drain(..) {
            worker.join().expect("benchmark worker shuts down cleanly");
        }
    }
}

fn percentile(sorted: &[u64], percent: usize) -> u64 {
    let rank = (sorted.len() * percent).div_ceil(100);
    sorted[rank.saturating_sub(1)]
}

fn report_probes(pool: &WorkerPool, workload: Workload, workers: usize, payload_bytes: usize) {
    pool.run(RunMode::Timing);
    let allocation = pool.run(RunMode::Allocations);
    let mut latency = pool.run(RunMode::Latency);
    assert_eq!(latency.latencies_ns.len(), pool.operations);
    assert_eq!(
        latency.held_leases,
        workload.retained_per_worker() * workers
    );
    latency.latencies_ns.sort_unstable();
    #[cfg(feature = "bench-internals")]
    let lifetime_stats = {
        let stats = pool._cache.reclamation_stats();
        serde_json::json!({
            "peak_pending_nodes": stats.peak_pending_nodes,
            "peak_pending_weight": stats.peak_pending_weight,
            "pending_nodes": stats.pending_nodes,
            "pending_weight": stats.pending_weight,
            "reclaimed_nodes": stats.reclaimed_nodes,
            "largest_batch": stats.largest_batch,
            "grace_period_nanos": stats.grace_period_nanos,
        })
    };
    #[cfg(not(feature = "bench-internals"))]
    let lifetime_stats = serde_json::Value::Null;
    println!(
        "cache_reclamation_probe {}",
        serde_json::json!({
            "workload": workload.name(),
            "workers": workers,
            "payload_bytes": payload_bytes,
            "operations_per_batch": pool.operations,
            "weight_budget": RESIDENT_ENTRIES * payload_bytes,
            "instrumented": cfg!(feature = "bench-internals"),
            "allocation_probe": {
                "calls": allocation.allocations.calls,
                "requested_bytes": allocation.allocations.requested_bytes,
            },
            "latency_probe": {
                "samples": latency.latencies_ns.len(),
                "p50_ns": percentile(&latency.latencies_ns, 50),
                "p95_ns": percentile(&latency.latencies_ns, 95),
                "p99_ns": percentile(&latency.latencies_ns, 99),
                "max_ns": latency.latencies_ns.last(),
                "sampled_peak_pending_nodes": latency.sampled_peak_pending_nodes,
                "sampled_peak_pending_weight": latency.sampled_peak_pending_weight,
                "held_leases": latency.held_leases,
                "held_lease_payload_bytes": latency.held_leases * payload_bytes,
            },
            "cache_lifetime_stats_after_probes": lifetime_stats,
        })
    );
}

fn reclamation_benchmarks(c: &mut Criterion) {
    // Isolate entry storage from policy allocation and payload backing
    // buffers. Warm TLS first and keep final-drop maintenance outside the probe.
    let zero_capacity = CalculationCache::<u64, u64>::new(0);
    drop(
        zero_capacity
            .get_or_try_insert_with(0, |_| 8, || Ok(42))
            .unwrap(),
    );
    let allocation_probe = AllocationProbe::start();
    let value = zero_capacity
        .get_or_try_insert_with(1, |_| 8, || Ok(42))
        .unwrap();
    black_box(&*value);
    let allocations = allocation_probe.finish();
    println!(
        "cache_entry_allocation_probe {}",
        serde_json::json!({
            "value": "u64", "capacity": 0,
            "calls": allocations.calls, "requested_bytes": allocations.requested_bytes,
        })
    );
    let release_probe = AllocationProbe::start();
    drop(value);
    let release_allocations = release_probe.finish();
    #[cfg(feature = "bench-internals")]
    let pending_nodes = Some(zero_capacity.reclamation_stats().pending_nodes);
    #[cfg(not(feature = "bench-internals"))]
    let pending_nodes: Option<usize> = None;
    println!(
        "cache_final_lease_drop_probe {}",
        serde_json::json!({
            "value": "u64", "capacity": 0,
            "calls": release_allocations.calls,
            "requested_bytes": release_allocations.requested_bytes,
            "pending_nodes": pending_nodes,
        })
    );

    let mut nonresident = c.benchmark_group("cache_nonresident");
    nonresident.measurement_time(benchmark_measurement_time());
    nonresident.bench_function("roundtrip", |b| {
        b.iter(|| {
            let lease = zero_capacity
                .get_or_try_insert_with(1, |_| 8, || Ok(42))
                .unwrap();
            black_box(*lease);
            drop(lease);
        });
    });
    nonresident.finish();

    let operations = std::env::var("XLFN_CACHE_RECLAIM_OPERATIONS")
        .ok()
        .map(|value| {
            value
                .parse::<usize>()
                .expect("operations must be an integer")
        })
        .unwrap_or(DEFAULT_OPERATIONS_PER_WORKER);
    assert!(
        (64..=65_536).contains(&operations),
        "operations per worker must be 64..=65536"
    );
    let mut group = c.benchmark_group("cache_reclamation");
    group.measurement_time(benchmark_measurement_time());
    group.sample_size(20);
    group.sampling_mode(SamplingMode::Flat);
    for workload in [Workload::Churn, Workload::LiveLeases] {
        for payload_bytes in PAYLOAD_SIZES {
            for workers in THREAD_COUNTS {
                let pool = WorkerPool::new(workload, workers, payload_bytes, operations);
                report_probes(&pool, workload, workers, payload_bytes);
                group.throughput(Throughput::Elements(pool.operations as u64));
                group.bench_function(
                    BenchmarkId::new(
                        format!("{}/payload_{payload_bytes}b", workload.name()),
                        format!("threads_{workers}"),
                    ),
                    |b| b.iter(|| black_box(pool.run(RunMode::Timing))),
                );
            }
        }
    }
    group.finish();
}

fn deferred_queue_churn<K>(
    group: &mut BenchmarkGroup<'_, WallTime>,
    name: &'static str,
    make_key: impl Fn(u64) -> K,
) where
    K: Clone + Eq + Hash + Send + Sync + 'static,
{
    let cache = CalculationCache::<K, u64>::new(RESIDENT_ENTRIES * 8);
    // Keep decimal key width fixed throughout practical timing runs, so a
    // faster variant does not pay for longer strings merely by doing more work.
    let mut next_key = 1_u64 << 60;
    let mut run_operation = || {
        let value = next_key;
        next_key = next_key
            .checked_add(1)
            .expect("benchmark key space exhausted");
        // Construct the owned key inside both the allocation probe and timing
        // loop. Its clones and retirement can produce additional deferred
        // batches, unlike the Copy-key control.
        let lease = cache
            .get_or_try_insert_with(make_key(value), |_| 8, || Ok(value))
            .expect("deferred queue benchmark initialization succeeds");
        black_box(*lease);
        drop(lease);
    };
    for _ in 0..DEFAULT_OPERATIONS_PER_WORKER {
        run_operation();
    }
    let probe = AllocationProbe::start();
    for _ in 0..DEFAULT_OPERATIONS_PER_WORKER {
        run_operation();
    }
    let allocations = probe.finish();
    println!(
        "cache_deferred_queue_probe {}",
        serde_json::json!({
            "workload": name,
            "workers": 1,
            "value": "u64",
            "operations_per_batch": DEFAULT_OPERATIONS_PER_WORKER,
            "weight_budget": RESIDENT_ENTRIES * 8,
            "instrumented": cfg!(feature = "bench-internals"),
            "allocation_probe": {
                "calls": allocations.calls,
                "requested_bytes": allocations.requested_bytes,
            },
        })
    );
    group.bench_function(name, |b| b.iter(&mut run_operation));
    cache.clear();
    assert!(cache.is_empty());
    #[cfg(feature = "bench-internals")]
    {
        cache.maintenance();
        assert_eq!(cache.reclamation_stats().pending_nodes, 0);
        assert_eq!(cache.reclamation_stats().pending_weight, 0);
    }
}

fn deferred_queue_benchmarks(c: &mut Criterion) {
    let mut group = c.benchmark_group("cache_deferred_queue");
    group.measurement_time(benchmark_measurement_time());
    group.throughput(Throughput::Elements(1));
    deferred_queue_churn(&mut group, "owned_key_churn", |key| key.to_string());
    deferred_queue_churn(&mut group, "u64_churn", |key| key);
    group.finish();
}

criterion_group!(benches, reclamation_benchmarks, deferred_queue_benchmarks);
criterion_main!(benches);

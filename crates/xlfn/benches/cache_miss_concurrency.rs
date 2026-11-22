//! Cold, concurrent miss batches with persistent workers and a fixed budget.
//!
//! The timed interval includes dispatch completion and one start barrier per
//! batch, but excludes thread creation, cache clear, input allocation, and probes.
//! A dispatch-only control exposes the synchronization floor; its duration must
//! not be subtracted from the cache cases. Same-key batches include both pending
//! singleflight followers and resident hits after another worker publishes.
//! Mixed batches additionally prewarm alternating keys before the timed run.
//! The separate long-key group measures one thread and distinct cold keys,
//! with cache reset and allocation of caller-owned input outside each interval.

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use std::hint::black_box;
use std::sync::{Arc, Barrier, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use xlfn::benchmark_support::benchmark_measurement_time;
use xlfn::cache::CalculationCache;

const WORKERS: [usize; 4] = [1, 4, 16, 32];
const REQUESTS_PER_WORKER: usize = 256;
const BUDGET_BYTES: usize = 1024 * 1024;
const VALUE_BYTES: usize = size_of::<u64>();
const NUMERIC_ELEMENTS: usize = 4096;

#[derive(Clone, Copy)]
enum Keys {
    Same,
    Distinct,
    Mixed,
}

impl Keys {
    const fn name(self) -> &'static str {
        match self {
            Self::Same => "same_key",
            Self::Distinct => "distinct_keys",
            Self::Mixed => "half_prewarmed_same_key",
        }
    }

    const fn key(self, worker: usize, request: usize) -> u64 {
        match self {
            Self::Same | Self::Mixed => request as u64,
            // Use one fixed stride across all worker counts. The key population
            // changes only by adding workers, rather than changing earlier keys.
            Self::Distinct => (request * WORKERS[WORKERS.len() - 1] + worker) as u64,
        }
    }

    const fn unique_keys(self, workers: usize) -> usize {
        match self {
            Self::Same | Self::Mixed => REQUESTS_PER_WORKER,
            Self::Distinct => REQUESTS_PER_WORKER * workers,
        }
    }

    const fn prewarmed_keys(self) -> usize {
        match self {
            Self::Mixed => REQUESTS_PER_WORKER.div_ceil(2),
            Self::Same | Self::Distinct => 0,
        }
    }
}

#[derive(Clone, Copy)]
enum Compute {
    Cheap,
    NumericReduce,
}

impl Compute {
    const fn name(self) -> &'static str {
        match self {
            Self::Cheap => "cheap_u64",
            Self::NumericReduce => "numeric_reduce_4096",
        }
    }

    fn evaluate(self, key: u64, input: &[f64]) -> u64 {
        let key = black_box(key);
        match self {
            Self::Cheap => key.wrapping_mul(0x9e37_79b9_7f4a_7c15).rotate_left(17),
            Self::NumericReduce => {
                let scale = black_box(1.0 + (key % 64) as f64 / 64.0);
                black_box(input)
                    .iter()
                    .fold(0.0, |sum, value| sum + value * scale)
                    .to_bits()
            }
        }
    }
}

#[derive(Clone, Copy)]
enum Operation {
    Cache,
    ComputeOnly,
    DispatchOnly,
}

impl Operation {
    const fn name(self) -> &'static str {
        match self {
            Self::Cache => "cache",
            Self::ComputeOnly => "compute_only_control",
            Self::DispatchOnly => "dispatch_only_control",
        }
    }
}

#[derive(Default)]
struct Report {
    requests: usize,
    computes: usize,
    checksum: u64,
}

fn batch<const PROBE: bool>(
    cache: &CalculationCache<u64, u64>,
    keys: Keys,
    compute: Compute,
    operation: Operation,
    worker: usize,
    input: &[f64],
) -> Report {
    let mut report = Report::default();
    for request in 0..REQUESTS_PER_WORKER {
        let key = keys.key(worker, request);
        let value = match operation {
            Operation::Cache => {
                let lease = cache
                    .get_or_try_insert_with(key, || {
                        if PROBE {
                            report.computes += 1;
                        }
                        Ok(compute.evaluate(key, input))
                    })
                    .expect("cache miss batch returned a value");
                black_box(*lease)
            }
            Operation::ComputeOnly => {
                if PROBE {
                    report.computes += 1;
                }
                black_box(compute.evaluate(key, input))
            }
            Operation::DispatchOnly => black_box(key),
        };
        if PROBE {
            report.requests += 1;
            report.checksum = report.checksum.wrapping_add(value);
        }
    }
    report
}

struct Pool {
    cache: Arc<CalculationCache<u64, u64>>,
    keys: Keys,
    compute: Compute,
    operation: Operation,
    input: Arc<[f64]>,
    start: Vec<mpsc::SyncSender<bool>>,
    done: Vec<mpsc::Receiver<Report>>,
    barrier: Arc<Barrier>,
    threads: Vec<JoinHandle<()>>,
}

impl Pool {
    fn new(workers: usize, keys: Keys, compute: Compute, operation: Operation) -> Self {
        assert!(workers != 0);
        assert!(REQUESTS_PER_WORKER * workers * VALUE_BYTES < BUDGET_BYTES);
        let cache = Arc::new(CalculationCache::with_weigher(
            xlfn::cache::CacheBudget::weight_units(BUDGET_BYTES),
            |_| VALUE_BYTES,
        ));
        // This is a representative numerical reduction, not an Excel call or
        // input-conversion measurement. The fixed source is allocated once.
        let input: Arc<[f64]> = (0..NUMERIC_ELEMENTS)
            .map(|index| (index as f64 - NUMERIC_ELEMENTS as f64 / 2.0) / 8.0)
            .collect::<Vec<_>>()
            .into();
        let barrier = Arc::new(Barrier::new(workers + 1));
        let mut start = Vec::with_capacity(workers);
        let mut done = Vec::with_capacity(workers);
        let mut threads = Vec::with_capacity(workers);
        for worker in 0..workers {
            let cache = Arc::clone(&cache);
            let input = Arc::clone(&input);
            let barrier = Arc::clone(&barrier);
            // A dedicated completion channel disconnects if this worker panics.
            // Other idle workers cannot keep its sender alive and strand the
            // driver waiting for a completion that will never arrive.
            let (done_tx, done_rx) = mpsc::sync_channel(1);
            done.push(done_rx);
            let (start_tx, start_rx) = mpsc::sync_channel(1);
            start.push(start_tx);
            threads.push(thread::spawn(move || {
                barrier.wait();
                while let Ok(probe) = start_rx.recv() {
                    barrier.wait();
                    let report = if probe {
                        batch::<true>(&cache, keys, compute, operation, worker, &input)
                    } else {
                        batch::<false>(&cache, keys, compute, operation, worker, &input)
                    };
                    done_tx.send(report).expect("driver received batch report");
                }
            }));
        }
        // All persistent workers exist and have reached the startup barrier
        // before Criterion or the untimed validation dispatches its first batch.
        barrier.wait();
        Self {
            cache,
            keys,
            compute,
            operation,
            input,
            start,
            done,
            barrier,
            threads,
        }
    }

    fn run(&self, probe: bool) -> (Duration, Report) {
        if matches!(self.operation, Operation::Cache) {
            self.cache.clear();
            self.cache.maintenance();
            for index in 0..self.keys.prewarmed_keys() {
                let key = self.keys.key(0, index * 2);
                self.cache
                    .get_or_try_insert_with(key, || Ok(self.compute.evaluate(key, &self.input)))
                    .expect("mixed workload prewarm returned a value");
            }
        }
        for worker in &self.start {
            worker.send(probe).expect("worker received batch command");
        }
        let started = Instant::now();
        self.barrier.wait();
        let mut total = Report::default();
        for worker in &self.done {
            let report = worker.recv().expect("cache miss worker completed batch");
            if probe {
                total.requests += report.requests;
                total.computes += report.computes;
                total.checksum = total.checksum.wrapping_add(report.checksum);
            }
        }
        (started.elapsed(), total)
    }

    fn total_requests(&self) -> usize {
        REQUESTS_PER_WORKER * self.threads.len()
    }

    fn verify(&self) {
        let (_, report) = self.run(true);
        let requests = self.total_requests();
        assert_eq!(report.requests, requests);
        let (expected_computes, expected_resident) = match self.operation {
            Operation::Cache => {
                let unique_keys = self.keys.unique_keys(self.threads.len());
                (unique_keys - self.keys.prewarmed_keys(), unique_keys)
            }
            Operation::ComputeOnly => (requests, 0),
            Operation::DispatchOnly => (0, 0),
        };
        assert_eq!(report.computes, expected_computes);
        let mut expected_checksum = 0_u64;
        let mut resident_hits = 0;
        for worker in 0..self.threads.len() {
            for request in 0..REQUESTS_PER_WORKER {
                let key = self.keys.key(worker, request);
                let expected = if matches!(self.operation, Operation::DispatchOnly) {
                    key
                } else {
                    self.compute.evaluate(key, &self.input)
                };
                expected_checksum = expected_checksum.wrapping_add(expected);
                if matches!(self.operation, Operation::Cache) {
                    let lease = self.cache.get(&key).expect("cold result remained resident");
                    assert_eq!(*lease, expected);
                    resident_hits += 1;
                }
            }
        }
        assert_eq!(report.checksum, expected_checksum);
        self.cache.maintenance();
        let resident = self.cache.resident_stats();
        assert!(resident.weight <= BUDGET_BYTES as u64);
        if matches!(self.operation, Operation::Cache) {
            assert_eq!(resident.entries, expected_resident as u64);
            assert_eq!(resident.weight, (expected_resident * VALUE_BYTES) as u64);
            assert_eq!(resident_hits, requests);
        }
        self.cache.clear();
        self.cache.maintenance();
        let debt = self.cache.reclamation_stats();
        assert_eq!((debt.pending_nodes, debt.pending_weight), (0, 0));
        println!(
            "{}",
            serde_json::json!({
                "probe": "cache_miss_concurrency",
                "workers": self.threads.len(), "keys": self.keys.name(),
                "compute": self.compute.name(), "operation": self.operation.name(),
                "requests": requests, "computes": report.computes,
                "prewarmed_keys": self.keys.prewarmed_keys(),
                "prewarmed_requests": self.keys.prewarmed_keys() * self.threads.len(),
                "repeat_resident_hits": resident_hits,
                "coalesced_or_resident_reuses": if matches!(self.operation, Operation::Cache) {
                    Some(requests - report.computes)
                } else {
                    None
                },
                "budget_bytes": BUDGET_BYTES,
                "resident_entries": resident.entries, "resident_weight": resident.weight,
                "pending_nodes_after_clear": debt.pending_nodes,
            })
        );
    }
}

impl Drop for Pool {
    fn drop(&mut self) {
        self.start.clear();
        for worker in self.threads.drain(..) {
            // The private completion channel already reports worker failure.
            // Joining must not panic again while the driver is unwinding.
            let _ = worker.join();
        }
    }
}

fn benchmarks(c: &mut Criterion) {
    let mut group = c.benchmark_group("cache_miss_concurrency");
    group.measurement_time(benchmark_measurement_time());
    for workers in WORKERS {
        let dispatch = Pool::new(
            workers,
            Keys::Distinct,
            Compute::Cheap,
            Operation::DispatchOnly,
        );
        dispatch.verify();
        group.throughput(Throughput::Elements(dispatch.total_requests() as u64));
        group.bench_function(
            BenchmarkId::new("dispatch_only_control", format!("workers_{workers}")),
            |b| {
                b.iter_custom(|iterations| (0..iterations).map(|_| dispatch.run(false).0).sum());
            },
        );
        drop(dispatch);
        let mixed = Pool::new(workers, Keys::Mixed, Compute::Cheap, Operation::Cache);
        mixed.verify();
        group.bench_function(
            BenchmarkId::new(
                "half_prewarmed_same_key/cheap_u64/cache",
                format!("workers_{workers}"),
            ),
            |b| {
                b.iter_custom(|iterations| (0..iterations).map(|_| mixed.run(false).0).sum());
            },
        );
        drop(mixed);
        for compute in [Compute::Cheap, Compute::NumericReduce] {
            for keys in [Keys::Distinct, Keys::Same] {
                for operation in [Operation::Cache, Operation::ComputeOnly] {
                    let pool = Pool::new(workers, keys, compute, operation);
                    pool.verify();
                    group.bench_function(
                        BenchmarkId::new(
                            format!("{}/{}/{}", keys.name(), compute.name(), operation.name()),
                            format!("workers_{workers}"),
                        ),
                        |b| {
                            b.iter_custom(|iterations| {
                                (0..iterations).map(|_| pool.run(false).0).sum()
                            });
                        },
                    );
                }
            }
        }
    }
    group.finish();
}

const HASH_BATCH_KEYS: usize = 128;

fn long_key_misses(c: &mut Criterion) {
    let mut group = c.benchmark_group("cache_miss_key_hash");
    group.measurement_time(benchmark_measurement_time());
    group.throughput(Throughput::Elements(HASH_BATCH_KEYS as u64));
    for len in [16, 1_024, 8_192] {
        let suffix = "x".repeat(len - 16);
        let keys = (0..HASH_BATCH_KEYS)
            .map(|index| format!("{index:016x}{suffix}"))
            .collect::<Vec<_>>();
        for owned in [true, false] {
            let cache =
                CalculationCache::new(xlfn::cache::CacheBudget::weight_units(HASH_BATCH_KEYS));
            let run = |probe| {
                cache.clear();
                // The owned case receives already allocated caller keys. The
                // borrowed case must still acquire ownership on a true miss.
                let input = owned.then(|| keys.clone());
                let mut initialized = 0;
                let start = Instant::now();
                if let Some(input) = input {
                    for key in input {
                        let value = cache
                            .get_or_try_insert_with(black_box(key), || {
                                initialized += 1;
                                Ok(7_u64)
                            })
                            .unwrap();
                        black_box(*value);
                    }
                } else {
                    for key in &keys {
                        let value = cache
                            .get_or_try_insert_borrowed(black_box(key.as_str()), || {
                                initialized += 1;
                                Ok(7_u64)
                            })
                            .unwrap();
                        black_box(*value);
                    }
                }
                let elapsed = start.elapsed();
                if probe {
                    assert_eq!(initialized, HASH_BATCH_KEYS);
                    assert_eq!(cache.len(), HASH_BATCH_KEYS);
                    assert_eq!(*cache.get(keys[0].as_str()).unwrap(), 7);
                }
                elapsed
            };
            run(true);
            group.bench_function(
                BenchmarkId::new(
                    if owned { "owned" } else { "borrowed" },
                    format!("bytes_{len}"),
                ),
                |b| b.iter_custom(|iterations| (0..iterations).map(|_| run(false)).sum()),
            );
        }
    }
    group.finish();
}

criterion_group!(benches, long_key_misses, benchmarks);
criterion_main!(benches);

use crate::unstable::cache::CalculationCache;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, SyncSender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const HOT_KEY: u64 = 42;
const LOOKUP_WEIGHT_BUDGET: usize = 64;
const EVICTION_WEIGHT_BUDGET: usize = 1;
const ENTRY_WEIGHT: u32 = 1;

#[derive(Clone, Copy, Debug)]
pub enum CacheLookupBenchCase {
    HotKey,
    DisjointKeys,
}

impl CacheLookupBenchCase {
    pub const fn name(self) -> &'static str {
        match self {
            Self::HotKey => "hot_key",
            Self::DisjointKeys => "disjoint_keys",
        }
    }

    fn keys(self, worker_count: usize) -> Vec<u64> {
        match self {
            Self::HotKey => vec![HOT_KEY; worker_count],
            Self::DisjointKeys => (0..worker_count).map(|worker| worker as u64).collect(),
        }
    }

    fn warm_keys(self, worker_count: usize) -> Vec<u64> {
        match self {
            Self::HotKey => vec![HOT_KEY],
            Self::DisjointKeys => self.keys(worker_count),
        }
    }
}

struct WorkerPool {
    worker_count: usize,
    start_tx: Vec<SyncSender<()>>,
    done_rx: Receiver<()>,
    workers: Vec<JoinHandle<()>>,
}

impl WorkerPool {
    fn new<F>(worker_count: usize, worker: F) -> Self
    where
        F: Fn(usize, Receiver<()>, SyncSender<()>) + Send + Sync + 'static,
    {
        assert!(worker_count != 0);

        let worker = Arc::new(worker);
        let (done_tx, done_rx) = std::sync::mpsc::sync_channel(worker_count);
        let mut start_tx = Vec::with_capacity(worker_count);
        let mut workers = Vec::with_capacity(worker_count);

        for index in 0..worker_count {
            let (worker_tx, worker_rx) = std::sync::mpsc::sync_channel::<()>(1);
            let done_tx = done_tx.clone();
            let worker = Arc::clone(&worker);
            start_tx.push(worker_tx);
            workers.push(thread::spawn(move || worker(index, worker_rx, done_tx)));
        }

        Self {
            worker_count,
            start_tx,
            done_rx,
            workers,
        }
    }

    fn run(&self) {
        for start in &self.start_tx {
            start
                .send(())
                .expect("cache benchmark worker received start signal");
        }
        for _ in 0..self.worker_count {
            self.done_rx
                .recv()
                .expect("cache benchmark worker finished batch");
        }
    }
}

impl Drop for WorkerPool {
    fn drop(&mut self) {
        self.start_tx.clear();
        for worker in self.workers.drain(..) {
            let _ = crate::panic_boundary::contain_panic(worker.join());
        }
    }
}

fn warmed_calculation_cache(
    case: CacheLookupBenchCase,
    worker_count: usize,
) -> Arc<CalculationCache<u64, u64>> {
    let cache = Arc::new(CalculationCache::<u64, u64>::new(LOOKUP_WEIGHT_BUDGET));
    for key in case.warm_keys(worker_count) {
        cache
            .get_or_try_insert_with(key, |_| ENTRY_WEIGHT as usize, move || Ok(key))
            .expect("cache benchmark warm seed failed");
    }
    cache
}

/// Measures the current raw-pointer cache ownership path with persistent workers.
pub struct CurrentCacheBenchmark {
    workers: WorkerPool,
    total_iterations: usize,
}

impl CurrentCacheBenchmark {
    pub fn new(
        case: CacheLookupBenchCase,
        worker_count: usize,
        iterations_per_worker: usize,
    ) -> Self {
        assert!(worker_count != 0);
        assert!(iterations_per_worker != 0);

        let cache = warmed_calculation_cache(case, worker_count);

        let keys = Arc::new(case.keys(worker_count));
        let worker_cache = Arc::clone(&cache);
        let workers = WorkerPool::new(worker_count, move |worker, receiver, done| {
            let key = keys[worker];
            while receiver.recv().is_ok() {
                for _ in 0..iterations_per_worker {
                    let lease = worker_cache
                        .get(&key)
                        .expect("current cache benchmark warm hit failed");
                    std::hint::black_box(&*lease);
                }
                done.send(())
                    .expect("current cache benchmark driver received completion signal");
            }
        });

        Self {
            workers,
            total_iterations: worker_count * iterations_per_worker,
        }
    }

    pub fn run(&self) {
        self.workers.run();
    }

    pub const fn total_iterations(&self) -> usize {
        self.total_iterations
    }
}

/// Measures scoped cache reads with one scope per lookup or per batch.
pub struct ScopedDurationCacheBenchmark {
    workers: WorkerPool,
    total_iterations: usize,
}

impl ScopedDurationCacheBenchmark {
    pub fn new(
        case: CacheLookupBenchCase,
        worker_count: usize,
        scopes_per_batch: usize,
        lookups_per_scope: usize,
    ) -> Self {
        assert!(worker_count != 0);
        assert!(scopes_per_batch != 0);
        assert!(lookups_per_scope != 0);

        let cache = warmed_calculation_cache(case, worker_count);
        let keys = Arc::new(case.keys(worker_count));
        let worker_cache = Arc::clone(&cache);
        let workers = WorkerPool::new(worker_count, move |worker, receiver, done| {
            let key = keys[worker];
            while receiver.recv().is_ok() {
                for _ in 0..scopes_per_batch {
                    let scope = worker_cache
                        .read_scope()
                        .expect("scoped cache benchmark admission failed");
                    for _ in 0..lookups_per_scope {
                        let value = scope
                            .get(&key)
                            .expect("scoped cache benchmark warm hit failed");
                        std::hint::black_box(value);
                    }
                }
                done.send(())
                    .expect("cache benchmark driver received completion signal");
            }
        });

        Self {
            workers,
            total_iterations: worker_count * scopes_per_batch * lookups_per_scope,
        }
    }

    pub fn run(&self) {
        self.workers.run();
    }

    pub const fn total_iterations(&self) -> usize {
        self.total_iterations
    }
}

/// Measures the pinless scoped path with one scope per lookup.
pub struct ScopedPerLookupCacheBenchmark {
    inner: ScopedDurationCacheBenchmark,
}

impl ScopedPerLookupCacheBenchmark {
    pub fn new(
        case: CacheLookupBenchCase,
        worker_count: usize,
        iterations_per_worker: usize,
    ) -> Self {
        Self {
            inner: ScopedDurationCacheBenchmark::new(case, worker_count, iterations_per_worker, 1),
        }
    }

    pub fn run(&self) {
        self.inner.run();
    }

    pub const fn total_iterations(&self) -> usize {
        self.inner.total_iterations()
    }
}

/// Measures the pinless scoped path with one scope for the whole lookup batch.
pub struct ScopedBatchCacheBenchmark {
    inner: ScopedDurationCacheBenchmark,
}

impl ScopedBatchCacheBenchmark {
    pub fn new(
        case: CacheLookupBenchCase,
        worker_count: usize,
        iterations_per_worker: usize,
    ) -> Self {
        Self {
            inner: ScopedDurationCacheBenchmark::new(case, worker_count, 1, iterations_per_worker),
        }
    }

    pub fn run(&self) {
        self.inner.run();
    }

    pub const fn total_iterations(&self) -> usize {
        self.inner.total_iterations()
    }
}

/// Measures `clear()` while a short-lived scoped reader is active.
pub struct ConcurrentClearLatencyBenchmark {
    cache: CalculationCache<u64, u64>,
    scope_lookups: usize,
}

impl ConcurrentClearLatencyBenchmark {
    pub fn new(scope_lookups: usize) -> Self {
        assert!(scope_lookups != 0);
        let cache = CalculationCache::new(LOOKUP_WEIGHT_BUDGET);
        drop(
            cache
                .get_or_try_insert_with(HOT_KEY, |_| ENTRY_WEIGHT as usize, || Ok(HOT_KEY))
                .expect("clear-latency benchmark warm seed failed"),
        );
        let _ = cache.len();
        Self {
            cache,
            scope_lookups,
        }
    }

    pub fn run(&self) -> Duration {
        drop(
            self.cache
                .get_or_try_insert_with(HOT_KEY, |_| ENTRY_WEIGHT as usize, || Ok(HOT_KEY))
                .expect("clear-latency benchmark warm reseed failed"),
        );
        let _ = self.cache.len();

        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(0);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(0);
        let (quiesce_tx, quiesce_rx) = std::sync::mpsc::sync_channel(0);
        let (clear_done_tx, clear_done_rx) = std::sync::mpsc::sync_channel(0);
        let cache = &self.cache;

        std::thread::scope(|threads| {
            threads.spawn(move || {
                let scope = cache
                    .read_scope()
                    .expect("clear-latency benchmark admission failed");
                let value = scope
                    .get(&HOT_KEY)
                    .expect("clear-latency benchmark warm hit failed");
                std::hint::black_box(value);
                ready_tx
                    .send(())
                    .expect("clear-latency benchmark reader did not start");
                release_rx
                    .recv()
                    .expect("clear-latency benchmark reader was not released");
                for _ in 0..self.scope_lookups {
                    std::hint::black_box(scope.get(&HOT_KEY));
                }
                drop(scope);
            });

            ready_rx
                .recv()
                .expect("clear-latency benchmark reader did not become ready");

            let clearer = threads.spawn(move || {
                let started = Instant::now();
                cache.clear_with_quiesce_hook(|| {
                    quiesce_tx
                        .send(())
                        .expect("clear-latency benchmark did not reach quiescence");
                });
                clear_done_tx
                    .send(started.elapsed())
                    .expect("clear-latency benchmark did not report completion");
            });

            quiesce_rx
                .recv()
                .expect("clear-latency benchmark did not reach quiescence");
            release_tx
                .send(())
                .expect("clear-latency benchmark reader release failed");

            let clear_duration = clear_done_rx
                .recv()
                .expect("clear-latency benchmark did not finish");
            crate::panic_boundary::contain_panic(clearer.join())
                .expect("clear-latency benchmark clearer panicked");

            clear_duration
        })
    }
}

/// Measures a deterministic live-lease retirement on the current cache.
pub struct CurrentCacheEvictionBenchmark {
    cache: CalculationCache<u64, u64>,
    iterations: usize,
}

impl CurrentCacheEvictionBenchmark {
    pub fn new(iterations: usize) -> Self {
        assert!(iterations != 0);
        Self {
            cache: CalculationCache::new(EVICTION_WEIGHT_BUDGET),
            iterations,
        }
    }

    pub fn run(&self) {
        for _ in 0..self.iterations {
            let lease_a = self
                .cache
                .get_or_try_insert_with(0, |_| ENTRY_WEIGHT as usize, || Ok(0))
                .expect("current cache eviction seed A failed");
            // Explicit withdrawal keeps this lifetime workload independent of
            // the native admission policy at capacity one.
            self.cache.clear();
            let lease_b = self
                .cache
                .get_or_try_insert_with(1, |_| ENTRY_WEIGHT as usize, || Ok(1))
                .expect("current cache eviction seed B failed");
            std::hint::black_box(&*lease_a);
            drop(lease_b);
            drop(lease_a);
        }
    }

    pub const fn total_iterations(&self) -> usize {
        self.iterations
    }
}

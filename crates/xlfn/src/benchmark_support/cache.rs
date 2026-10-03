use crate::cache::{CacheEndpoint, CacheRegistry, CalculationCache};

use std::sync::Arc;
use std::sync::mpsc::{Receiver, SyncSender};
use std::thread::{self, JoinHandle};

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

const REGISTRY_ENDPOINT_IDS: [&str; 32] = [
    "registry-00",
    "registry-01",
    "registry-02",
    "registry-03",
    "registry-04",
    "registry-05",
    "registry-06",
    "registry-07",
    "registry-08",
    "registry-09",
    "registry-10",
    "registry-11",
    "registry-12",
    "registry-13",
    "registry-14",
    "registry-15",
    "registry-16",
    "registry-17",
    "registry-18",
    "registry-19",
    "registry-20",
    "registry-21",
    "registry-22",
    "registry-23",
    "registry-24",
    "registry-25",
    "registry-26",
    "registry-27",
    "registry-28",
    "registry-29",
    "registry-30",
    "registry-31",
];

struct RegistryEndpointMarker<const INDEX: usize>;
static REGISTRY_SHARED_ID: &str = "registry-shared-id";

#[inline(always)]
fn visit_registry_marker<const INDEX: usize, const SETUP: bool>(registry: &CacheRegistry) {
    let endpoint =
        CacheEndpoint::<u64, u64, RegistryEndpointMarker<INDEX>>::new(REGISTRY_SHARED_ID);
    if SETUP {
        drop(
            endpoint
                .get_or_try_insert(
                    registry,
                    HOT_KEY,
                    |_| ENTRY_WEIGHT as usize,
                    || Ok(INDEX as u64),
                )
                .expect("registry marker benchmark warm seed failed"),
        );
    }
    let lease = endpoint
        .get(registry, &HOT_KEY)
        .expect("registry marker benchmark endpoint resolution failed")
        .expect("registry marker benchmark warm hit failed");
    if SETUP {
        assert_eq!(*lease, INDEX as u64);
    } else {
        std::hint::black_box(&*lease);
    }
}

#[inline(always)]
fn visit_registry_markers<const COUNT: usize, const SETUP: bool>(registry: &CacheRegistry) {
    // Keep the different endpoint types statically selected. Neither the
    // measured cycle nor its individual gets use function-pointer dispatch.
    visit_registry_marker::<0, SETUP>(registry);
    visit_registry_marker::<1, SETUP>(registry);
    visit_registry_marker::<2, SETUP>(registry);
    if COUNT == 8 {
        visit_registry_marker::<3, SETUP>(registry);
        visit_registry_marker::<4, SETUP>(registry);
        visit_registry_marker::<5, SETUP>(registry);
        visit_registry_marker::<6, SETUP>(registry);
        visit_registry_marker::<7, SETUP>(registry);
    }
}

/// Measures the public endpoint API, including registry resolution and leases.
/// Workers retain their thread-local resolution cache between measured batches.
pub struct RegistryCacheBenchmark {
    workers: WorkerPool,
    total_iterations: usize,
}

impl RegistryCacheBenchmark {
    /// Each worker reads its own endpoint through one shared registry.
    pub fn distinct_endpoints(worker_count: usize, iterations_per_worker: usize) -> Self {
        Self::new(worker_count, 1, iterations_per_worker)
    }

    /// One worker cycles through endpoints to exercise resolution-cache capacity.
    pub fn endpoint_cycle(endpoint_count: usize, iterations: usize) -> Self {
        Self::new(1, endpoint_count, iterations)
    }

    /// One worker cycles through three or eight marker types sharing one ID.
    /// The lookup count must contain a whole number of marker cycles.
    pub fn same_id_marker_cycle(marker_count: usize, iterations: usize) -> Self {
        match marker_count {
            3 => Self::marker_cycle::<3>(iterations),
            8 => Self::marker_cycle::<8>(iterations),
            _ => panic!("registry marker benchmark supports three or eight marker types"),
        }
    }

    fn marker_cycle<const COUNT: usize>(iterations: usize) -> Self {
        assert!(iterations != 0 && iterations.is_multiple_of(COUNT));
        let registry = Arc::new(CacheRegistry::new(LOOKUP_WEIGHT_BUDGET));
        let workers = WorkerPool::new(1, move |_, receiver, done| {
            // Seed and verify on the measured worker, warming its TLS as well.
            visit_registry_markers::<COUNT, true>(&registry);
            while receiver.recv().is_ok() {
                for _ in 0..iterations / COUNT {
                    visit_registry_markers::<COUNT, false>(&registry);
                }
                done.send(())
                    .expect("registry marker benchmark driver received completion signal");
            }
        });
        let benchmark = Self {
            workers,
            total_iterations: iterations,
        };
        benchmark.run();
        benchmark
    }

    /// Distinct names share one starting address but have different lengths.
    pub fn same_address_prefix_cycle(iterations: usize) -> Self {
        const PREFIX: &str = "registry-prefix-endpoint";
        Self::with_ids(1, 3, iterations, &[&PREFIX[..8], &PREFIX[..15], PREFIX])
    }

    fn new(worker_count: usize, endpoints_per_worker: usize, iterations_per_worker: usize) -> Self {
        let endpoint_count = worker_count
            .checked_mul(endpoints_per_worker)
            .expect("registry benchmark endpoint count fits usize");
        assert!(endpoint_count <= REGISTRY_ENDPOINT_IDS.len());
        Self::with_ids(
            worker_count,
            endpoints_per_worker,
            iterations_per_worker,
            &REGISTRY_ENDPOINT_IDS[..endpoint_count],
        )
    }

    fn with_ids(
        worker_count: usize,
        endpoints_per_worker: usize,
        iterations_per_worker: usize,
        ids: &[&'static str],
    ) -> Self {
        assert!(worker_count != 0);
        assert!(endpoints_per_worker != 0);
        assert!(iterations_per_worker != 0);
        let endpoint_count = worker_count
            .checked_mul(endpoints_per_worker)
            .expect("registry benchmark endpoint count fits usize");
        assert_eq!(endpoint_count, ids.len());

        let registry = Arc::new(CacheRegistry::new(LOOKUP_WEIGHT_BUDGET));
        let endpoints: Vec<CacheEndpoint<u64, u64>> =
            ids.iter().map(|id| CacheEndpoint::new(id)).collect();
        for (value, endpoint) in endpoints.iter().enumerate() {
            drop(
                endpoint
                    .get_or_try_insert(
                        &registry,
                        HOT_KEY,
                        |_| ENTRY_WEIGHT as usize,
                        || Ok(value as u64),
                    )
                    .expect("registry cache benchmark warm seed failed"),
            );
        }

        let workers = WorkerPool::new(worker_count, move |worker, receiver, done| {
            let start = worker * endpoints_per_worker;
            let endpoints = &endpoints[start..start + endpoints_per_worker];
            // Seed each worker's TLS on that worker and verify distinct endpoint
            // values before its first batch can be included in a measurement.
            for (offset, endpoint) in endpoints.iter().enumerate() {
                assert_eq!(
                    *endpoint
                        .get(&registry, &HOT_KEY)
                        .expect("registry cache benchmark endpoint resolution failed")
                        .expect("registry cache benchmark warm hit failed"),
                    (start + offset) as u64,
                );
            }
            while receiver.recv().is_ok() {
                for endpoint in endpoints.iter().cycle().take(iterations_per_worker) {
                    let lease = endpoint
                        .get(&registry, &HOT_KEY)
                        .expect("registry cache benchmark endpoint resolution failed")
                        .expect("registry cache benchmark warm hit failed");
                    std::hint::black_box(&*lease);
                }
                done.send(())
                    .expect("registry cache benchmark driver received completion signal");
            }
        });

        let benchmark = Self {
            workers,
            total_iterations: worker_count * iterations_per_worker,
        };
        // Finish startup, hit assertions and one complete workload batch before
        // returning to Criterion. Only subsequent runs are timed.
        benchmark.run();
        benchmark
    }

    pub fn run(&self) {
        self.workers.run();
    }

    pub const fn total_iterations(&self) -> usize {
        self.total_iterations
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
            // Clear retires A deterministically; relying only on admission would make
            // the measured retirement depend on capacity-one admission policy.
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

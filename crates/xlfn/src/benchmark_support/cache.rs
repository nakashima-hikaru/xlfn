use crate::unstable::cache::CalculationCache;
use moka::{Equivalent, sync::Cache};
use std::hash::{Hash, Hasher};
use std::marker::PhantomData;
use std::ops::Deref;
use std::pin::Pin;
use std::ptr::NonNull;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, SyncSender};
use std::thread::{self, JoinHandle};
use xlfn_kernel::drain_gate::{DEFAULT_STRIPE_COUNT, StripedDrainGate, current_thread_stripe};

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
            let _ = worker.join();
        }
    }
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

        let cache = Arc::new(CalculationCache::<u64, u64>::new(LOOKUP_WEIGHT_BUDGET));
        for key in case.warm_keys(worker_count) {
            cache
                .get_or_try_insert_with(key, |_| ENTRY_WEIGHT as usize, move || Ok(key))
                .expect("current cache benchmark warm seed failed");
        }

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

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct DiagnosticVersionedKey<K> {
    epoch: u64,
    key: K,
}

struct DiagnosticVersionedKeyRef<'a, K> {
    epoch: u64,
    key: &'a K,
}

impl<K: Hash> Hash for DiagnosticVersionedKeyRef<'_, K> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.epoch.hash(state);
        self.key.hash(state);
    }
}

impl<K: Eq> Equivalent<DiagnosticVersionedKey<K>> for DiagnosticVersionedKeyRef<'_, K> {
    fn equivalent(&self, owned: &DiagnosticVersionedKey<K>) -> bool {
        self.epoch == owned.epoch && self.key == &owned.key
    }
}

struct DiagnosticLookupDomain {
    generations: [StripedDrainGate<DEFAULT_STRIPE_COUNT>; 2],
    current: AtomicUsize,
    closed: AtomicBool,
}

impl DiagnosticLookupDomain {
    const fn new() -> Self {
        Self {
            generations: [StripedDrainGate::new_open(), StripedDrainGate::new_open()],
            current: AtomicUsize::new(0),
            closed: AtomicBool::new(false),
        }
    }

    fn enter(&self) -> Option<DiagnosticAdmissionPermit<'_>> {
        if self.closed.load(Ordering::Acquire) {
            return None;
        }
        let generation = self.current.load(Ordering::Acquire) & 1;
        let stripe = current_thread_stripe();
        self.generations[generation].try_acquire(stripe).ok()?;
        Some(DiagnosticAdmissionPermit {
            gate: &self.generations[generation],
            stripe,
        })
    }
}

struct DiagnosticAdmissionPermit<'a> {
    gate: &'a StripedDrainGate<DEFAULT_STRIPE_COUNT>,
    stripe: usize,
}

impl Drop for DiagnosticAdmissionPermit<'_> {
    fn drop(&mut self) {
        self.gate.release(self.stripe);
    }
}

struct DiagnosticNode {
    value: Box<u64>,
    pins: AtomicUsize,
    resident: AtomicBool,
    generation: u64,
}

#[derive(Clone, Copy)]
struct DiagnosticNodePtr(NonNull<DiagnosticNode>);

// SAFETY: DiagnosticNode pointers are only published after initialization and are
// reclaimed with the owning benchmark store after all worker leases have ended.
unsafe impl Send for DiagnosticNodePtr {}
// SAFETY: The benchmark stores are immutable after warm-up; the pointed-to node is
// synchronized by its atomics and remains owned by the store for the benchmark lifetime.
unsafe impl Sync for DiagnosticNodePtr {}

struct DiagnosticCacheStore {
    // Keep the Moka cache before `nodes` so cache bookkeeping is dropped first.
    cache: Cache<DiagnosticVersionedKey<u64>, (DiagnosticNodePtr, u32)>,
    epoch: AtomicU64,
    domain: DiagnosticLookupDomain,
    nodes: Vec<Pin<Box<DiagnosticNode>>>,
}

impl DiagnosticCacheStore {
    fn new(weight_budget: usize) -> Self {
        let capacity = u64::try_from(weight_budget).unwrap_or(u64::MAX);
        Self {
            cache: Cache::builder()
                .max_capacity(capacity)
                .weigher(|_, entry: &(DiagnosticNodePtr, u32)| entry.1)
                .support_invalidation_closures()
                .build(),
            epoch: AtomicU64::new(0),
            domain: DiagnosticLookupDomain::new(),
            nodes: Vec::new(),
        }
    }

    fn insert(&mut self, key: u64, value: u64) {
        let epoch = self.epoch.load(Ordering::Acquire);
        let node = Box::pin(DiagnosticNode {
            value: Box::new(value),
            pins: AtomicUsize::new(1),
            resident: AtomicBool::new(true),
            generation: epoch,
        });
        let node_ptr = DiagnosticNodePtr(NonNull::from(node.as_ref().get_ref()));
        self.nodes.push(node);
        self.cache.insert(
            DiagnosticVersionedKey { epoch, key },
            (node_ptr, ENTRY_WEIGHT),
        );
    }

    fn warmed(case: CacheLookupBenchCase, worker_count: usize) -> Arc<Self> {
        let mut cache = Self::new(LOOKUP_WEIGHT_BUDGET);
        for key in case.warm_keys(worker_count) {
            cache.insert(key, key);
        }
        Arc::new(cache)
    }

    fn lookup_node(&self, key: &u64, epoch: u64) -> Option<DiagnosticNodePtr> {
        let lookup = DiagnosticVersionedKeyRef { epoch, key };
        let (node_ptr, _) = self.cache.get(&lookup)?;
        // SAFETY: The diagnostic stores are warmed once and never evicted or mutated
        // during the steady-state lookup benchmarks.
        let node = unsafe { node_ptr.0.as_ref() };
        if node.generation != epoch || !node.resident.load(Ordering::Acquire) {
            return None;
        }
        Some(node_ptr)
    }

    /// Diagnostic B: Moka lookup plus the node pin, without lookup admission.
    fn get_with_pin<'a>(&'a self, key: &u64) -> Option<DiagnosticPinLease<'a>> {
        let epoch = self.epoch.load(Ordering::Acquire);
        let node_ptr = self.lookup_node(key, epoch)?;
        // SAFETY: This control intentionally omits admission and relies on the
        // benchmark's no-eviction steady-state invariant for pointer validity.
        let node = unsafe { node_ptr.0.as_ref() };
        node.pins.fetch_add(1, Ordering::AcqRel);
        if !node.resident.load(Ordering::Acquire) {
            let previous = node.pins.fetch_sub(1, Ordering::AcqRel);
            debug_assert!(previous > 0);
            return None;
        }
        Some(DiagnosticPinLease {
            node: node_ptr,
            _marker: PhantomData,
        })
    }

    /// Diagnostic C: lookup admission plus raw node access, without pin accounting.
    fn get_raw<'a>(&'a self, key: &u64) -> Option<DiagnosticRawLease<'a>> {
        let epoch = self.epoch.load(Ordering::Acquire);
        let permit = self.domain.enter()?;
        let Some(node_ptr) = self.lookup_node(key, epoch) else {
            drop(permit);
            return None;
        };
        Some(DiagnosticRawLease {
            node: node_ptr,
            _permit: permit,
            _marker: PhantomData,
        })
    }
}

struct DiagnosticPinLease<'a> {
    node: DiagnosticNodePtr,
    _marker: PhantomData<&'a DiagnosticCacheStore>,
}

impl Deref for DiagnosticPinLease<'_> {
    type Target = u64;

    fn deref(&self) -> &Self::Target {
        // SAFETY: The diagnostic pin remains held until this lease is dropped.
        unsafe { &self.node.0.as_ref().value }
    }
}

impl Drop for DiagnosticPinLease<'_> {
    fn drop(&mut self) {
        // SAFETY: The node is owned by the benchmark store and is not evicted while
        // the steady-state worker pool is running.
        let node = unsafe { self.node.0.as_ref() };
        let previous = node.pins.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(previous > 0);
    }
}

struct DiagnosticRawLease<'a> {
    node: DiagnosticNodePtr,
    _permit: DiagnosticAdmissionPermit<'a>,
    _marker: PhantomData<&'a DiagnosticCacheStore>,
}

impl Deref for DiagnosticRawLease<'_> {
    type Target = u64;

    fn deref(&self) -> &Self::Target {
        // SAFETY: The admission permit remains held for this lease and the
        // diagnostic store is immutable for the benchmark lifetime.
        unsafe { &self.node.0.as_ref().value }
    }
}

/// Benchmark-only control for Moka lookup plus node pin, without admission.
pub struct NoAdmissionCacheBenchmark {
    workers: WorkerPool,
    total_iterations: usize,
}

impl NoAdmissionCacheBenchmark {
    pub fn new(
        case: CacheLookupBenchCase,
        worker_count: usize,
        iterations_per_worker: usize,
    ) -> Self {
        assert!(worker_count != 0);
        assert!(iterations_per_worker != 0);

        let cache = DiagnosticCacheStore::warmed(case, worker_count);
        let keys = Arc::new(case.keys(worker_count));
        let worker_cache = Arc::clone(&cache);
        let workers = WorkerPool::new(worker_count, move |worker, receiver, done| {
            let key = keys[worker];
            while receiver.recv().is_ok() {
                for _ in 0..iterations_per_worker {
                    let lease = worker_cache
                        .get_with_pin(&key)
                        .expect("no-admission cache benchmark warm hit failed");
                    std::hint::black_box(&*lease);
                }
                done.send(())
                    .expect("cache benchmark driver received completion signal");
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

/// Benchmark-only control for lookup admission plus raw node access, without pins.
pub struct NoPinCacheBenchmark {
    workers: WorkerPool,
    total_iterations: usize,
}

impl NoPinCacheBenchmark {
    pub fn new(
        case: CacheLookupBenchCase,
        worker_count: usize,
        iterations_per_worker: usize,
    ) -> Self {
        assert!(worker_count != 0);
        assert!(iterations_per_worker != 0);

        let cache = DiagnosticCacheStore::warmed(case, worker_count);
        let keys = Arc::new(case.keys(worker_count));
        let worker_cache = Arc::clone(&cache);
        let workers = WorkerPool::new(worker_count, move |worker, receiver, done| {
            let key = keys[worker];
            while receiver.recv().is_ok() {
                for _ in 0..iterations_per_worker {
                    let lease = worker_cache
                        .get_raw(&key)
                        .expect("no-pin cache benchmark warm hit failed");
                    std::hint::black_box(&*lease);
                }
                done.send(())
                    .expect("cache benchmark driver received completion signal");
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

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct ArcVersionedKey<K> {
    epoch: u64,
    key: K,
}

struct ArcCacheLease(Arc<u64>);

impl Deref for ArcCacheLease {
    type Target = u64;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

struct ArcCacheStore {
    cache: Cache<ArcVersionedKey<u64>, (Arc<u64>, u32)>,
    epoch: AtomicU64,
}

impl ArcCacheStore {
    fn new(weight_budget: usize) -> Self {
        let capacity = u64::try_from(weight_budget).unwrap_or(u64::MAX);
        Self {
            cache: Cache::builder()
                .max_capacity(capacity)
                .weigher(|_, entry: &(Arc<u64>, u32)| entry.1)
                .support_invalidation_closures()
                .build(),
            epoch: AtomicU64::new(0),
        }
    }

    fn insert(&self, key: u64, value: u64) {
        let epoch = self.epoch.load(Ordering::Acquire);
        self.cache.insert(
            ArcVersionedKey { epoch, key },
            (Arc::new(value), ENTRY_WEIGHT),
        );
    }

    fn get(&self, key: u64) -> Option<ArcCacheLease> {
        let epoch = self.epoch.load(Ordering::Acquire);
        let vkey = ArcVersionedKey { epoch, key };
        self.cache.get(&vkey).map(|(value, _)| ArcCacheLease(value))
    }

    fn get_or_insert(&self, key: u64, value: u64) -> ArcCacheLease {
        let epoch = self.epoch.load(Ordering::Acquire);
        let vkey = ArcVersionedKey { epoch, key };
        let (value, _) = self
            .cache
            .get_with(vkey, || (Arc::new(value), ENTRY_WEIGHT));
        ArcCacheLease(value)
    }

    fn clear(&self) {
        let epoch = self.epoch.fetch_add(1, Ordering::AcqRel) + 1;
        self.cache
            .invalidate_entries_if(move |key, _| key.epoch < epoch)
            .expect("invalidation closures are enabled");
        self.cache.run_pending_tasks();
    }
}

/// Ownership control using the same versioned Moka lookup and cache capacity.
pub struct ArcCacheBenchmark {
    workers: WorkerPool,
    total_iterations: usize,
}

impl ArcCacheBenchmark {
    pub fn new(
        case: CacheLookupBenchCase,
        worker_count: usize,
        iterations_per_worker: usize,
    ) -> Self {
        assert!(worker_count != 0);
        assert!(iterations_per_worker != 0);

        let cache = Arc::new(ArcCacheStore::new(LOOKUP_WEIGHT_BUDGET));
        for key in case.warm_keys(worker_count) {
            cache.insert(key, key);
        }

        let keys = Arc::new(case.keys(worker_count));
        let worker_cache = Arc::clone(&cache);
        let workers = WorkerPool::new(worker_count, move |worker, receiver, done| {
            let key = keys[worker];
            while receiver.recv().is_ok() {
                for _ in 0..iterations_per_worker {
                    let lease = worker_cache
                        .get(key)
                        .expect("Arc cache benchmark warm hit failed");
                    std::hint::black_box(&*lease);
                }
                done.send(())
                    .expect("Arc cache benchmark driver received completion signal");
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
            // Clear retires A deterministically; relying only on TinyLFU admission would make
            // this ownership comparison depend on which candidate Moka admits at capacity one.
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

/// Arc ownership control for the same deterministic live-lease retirement.
pub struct ArcCacheEvictionBenchmark {
    cache: ArcCacheStore,
    iterations: usize,
}

impl ArcCacheEvictionBenchmark {
    pub fn new(iterations: usize) -> Self {
        assert!(iterations != 0);
        Self {
            cache: ArcCacheStore::new(EVICTION_WEIGHT_BUDGET),
            iterations,
        }
    }

    pub fn run(&self) {
        for _ in 0..self.iterations {
            let lease_a = self.cache.get_or_insert(0, 0);
            self.cache.clear();
            let lease_b = self.cache.get_or_insert(1, 1);
            std::hint::black_box(&*lease_a);
            drop(lease_b);
            drop(lease_a);
        }
    }

    pub const fn total_iterations(&self) -> usize {
        self.iterations
    }
}

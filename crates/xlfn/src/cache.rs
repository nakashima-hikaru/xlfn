use crate::error::InputError;
use crate::panic_boundary::catch_no_unwind;
use crate::{XllError, XllResult};
#[cfg(all(test, feature = "bench-internals"))]
mod backend_tests;
mod resident_index;
use parking_lot::{Mutex, RwLock};
use resident_index::ResidentIndex;
use std::any::{Any, TypeId};
use std::cell::Cell;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::marker::PhantomData;
use std::panic::AssertUnwindSafe;
use std::ptr::NonNull;
#[cfg(feature = "bench-internals")]
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::Instant;
use xlfn_kernel::drain_gate::DEFAULT_STRIPE_COUNT;
use xlfn_kernel::rotating_read_domain::{
    DrainedGeneration, GenerationIndex, RotatingReadDomain, RotatingReadPermit,
};

trait EpochAtomic {
    fn new(value: u64) -> Self;
    fn load(&self) -> u64;
    fn increment(&self);
}

impl EpochAtomic for AtomicU64 {
    fn new(value: u64) -> Self {
        Self::new(value)
    }

    fn load(&self) -> u64 {
        self.load(Ordering::SeqCst)
    }

    fn increment(&self) {
        self.fetch_add(1, Ordering::SeqCst);
    }
}

struct CacheGeneration<A: EpochAtomic = AtomicU64> {
    epoch: A,
}

impl<A: EpochAtomic> CacheGeneration<A> {
    fn new() -> Self {
        Self { epoch: A::new(0) }
    }

    fn snapshot(&self) -> u64 {
        self.epoch.load()
    }

    fn advance(&self) -> u64 {
        self.epoch.increment();
        self.snapshot()
    }

    fn discard_if_stale(&self, snapshot: u64, discard: impl FnOnce()) {
        if self.snapshot() != snapshot {
            discard();
        }
    }
}

thread_local! {
    static ACTIVE_CACHE_INITIALIZATION_DEPTH: Cell<u32> = const { Cell::new(0) };
}

struct ActiveCacheGuard;

impl ActiveCacheGuard {
    fn enter() -> XllResult<Self> {
        if ACTIVE_CACHE_INITIALIZATION_DEPTH.get() != 0 {
            return Err(XllError::Internal {
                diagnostic_id: crate::diagnostics::id::DiagnosticId::CACHE_REENTRANT,
            });
        }
        ACTIVE_CACHE_INITIALIZATION_DEPTH.set(1);
        Ok(Self)
    }
}

impl Drop for ActiveCacheGuard {
    fn drop(&mut self) {
        debug_assert_eq!(ACTIVE_CACHE_INITIALIZATION_DEPTH.get(), 1);
        ACTIVE_CACHE_INITIALIZATION_DEPTH.set(0);
    }
}

pub struct CacheEndpoint<Marker, K, V> {
    id: &'static str,
    _marker: PhantomData<fn() -> Marker>,
    _key: PhantomData<fn() -> K>,
    _value: PhantomData<fn() -> V>,
}

pub struct CacheLease<'a, V> {
    node: NonNull<CacheNode<V>>,
    _marker: PhantomData<&'a V>,
}

// SAFETY: [TR-LEASE-1] Moving a lease permits shared access alongside other
// leases (V: Sync), and its final drop may destroy V on that thread (V: Send).
// The lease's lifetime keeps the owning cache and reclamation domain alive.
unsafe impl<V: Send + Sync> Send for CacheLease<'_, V> {}
// SAFETY: [TR-LEASE-1] CacheLease provides shared access to V and is tied to the node's pin capability.
unsafe impl<V: Sync> Sync for CacheLease<'_, V> {}

impl<V> std::ops::Deref for CacheLease<'_, V> {
    type Target = V;

    fn deref(&self) -> &Self::Target {
        // SAFETY: [TR-LEASE-1] self.node is pinned for the lifetime of this CacheLease;
        // its live pin prevents retirement and reclamation.
        unsafe { &self.node.as_ref().value }
    }
}

unsafe fn reclaim_cache_node<V>(ptr: *mut ()) {
    // SAFETY: ptr points to an allocated CacheNode<V> whose grace period has ended.
    let node = unsafe { Box::from_raw(ptr as *mut CacheNode<V>) };
    let value = node.value;
    if catch_no_unwind(AssertUnwindSafe(|| drop(value))).is_err() {
        let error = XllError::Panic;
        crate::diagnostics::report_no_unwind("calculation cache value final drop", &error);
    }
}

fn reclaim_cache_entries<V>(entries: Vec<ReclaimEntry>) {
    for entry in entries {
        // SAFETY: [TR-RECLAIM-1] entry.0 points to an allocated CacheNode<V> whose grace period has ended.
        unsafe {
            reclaim_cache_node::<V>(entry.0);
        }
    }
}

/// Approximate, side-effect-free reclamation counters. Weights use the
/// caller's estimates, not allocator or process memory measurements.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CacheReclamationStats {
    /// Evicted nodes without lease pins, awaiting their grace period.
    pub pending_nodes: usize,
    /// Sum of their caller-supplied, normalized weights.
    pub pending_weight: u64,
    /// Highest queued node count observed since construction.
    pub peak_pending_nodes: usize,
    /// Highest queued weight observed since construction.
    pub peak_pending_weight: u64,
    /// Nodes handed to reclamation after their grace period completed.
    pub reclaimed_nodes: usize,
    /// Largest completed grace-period batch.
    pub largest_batch: usize,
    /// Cumulative time acquiring/completing successful reclamation batches.
    pub grace_period_nanos: u64,
}

// Mutations periodically flush Moka's deferred eviction work. Retirement also
// requests maintenance on ordinary reads, with a cheap zero-debt fast path.
const MAINTENANCE_INTERVAL: usize = 32;
const RECLAIM_BACKPRESSURE_NODES: usize = 256;

impl<V> Drop for CacheLease<'_, V> {
    fn drop(&mut self) {
        // SAFETY: [TR-LEASE-1] self.node remains valid because a pin capability is held by this lease.
        let node = unsafe { self.node.as_ref() };
        if node.release_pin() {
            // SAFETY: [TR-RECLAIM-1] The last pin was dropped on a retired (non-resident) node.
            let domain = unsafe { node.domain.as_ref() };
            domain.enqueue_reclaim(self.node.as_ptr() as *mut (), node.weight);
            let retired = domain.quiesce_and_drain();
            reclaim_cache_entries::<V>(retired);
        }
    }
}

impl<V: std::fmt::Debug> std::fmt::Debug for CacheLease<'_, V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&**self, f)
    }
}

impl<V: std::fmt::Display> std::fmt::Display for CacheLease<'_, V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&**self, f)
    }
}

impl<V: PartialEq> PartialEq for CacheLease<'_, V> {
    fn eq(&self, other: &Self) -> bool {
        **self == **other
    }
}

impl<V: Eq> Eq for CacheLease<'_, V> {}

/// Benchmark-only scoped read capability for a resident cache value.
///
/// The scope keeps one lookup-domain permit for its entire lifetime, so a
/// pointer observed through [`Self::get`] cannot be reclaimed before the
/// returned reference expires. The type is intentionally neither `Send` nor
/// `Sync`; keep the scope to a short lexical region containing cache reads.
/// Do not clear, initialize entries, drop leases, invoke callbacks, or await
/// within the scope: those operations may wait for this permit to be released.
#[cfg(feature = "bench-internals")]
#[must_use = "a CacheReadScope must stay alive while its references are used"]
pub struct CacheReadScope<'cache, K, V> {
    cache: &'cache CalculationCache<K, V>,
    _permit: CacheDomainPermit<'cache>,
    _not_send_sync: PhantomData<Rc<()>>,
}

#[cfg(feature = "bench-internals")]
impl<K, V> CacheReadScope<'_, K, V>
where
    K: Clone + Eq + Hash + Send + Sync + 'static,
    V: Send + Sync + 'static,
{
    /// Looks up a value while the scope's reclamation permit is held.
    ///
    /// This is the scoped counterpart to [`CalculationCache::get`]. It does
    /// not acquire or release a per-node pin. Keep the returned reference and
    /// the scope within a short lexical region because cache clear waits for
    /// active scopes to leave the lookup domain.
    #[must_use]
    #[inline]
    pub fn get<'scope>(&'scope self, key: &K) -> Option<&'scope V> {
        let epoch = self.cache.generation.snapshot();
        let lookup = VersionedKeyRef { epoch, key };
        let (node_ptr, _) = self.cache.index.get(&lookup)?;

        // SAFETY: [TR-SCOPED-READ-1] The scope owns a CacheDomainPermit for
        // its entire lifetime. The permit prevents the node's allocation from
        // being reclaimed until this returned reference can no longer exist.
        let node = unsafe { node_ptr.0.as_ref() };
        if node.generation != epoch || !node.resident.load(Ordering::Acquire) {
            return None;
        }

        Some(&node.value)
    }
}

pub struct BoundCacheEndpoint<'registry, Marker, K, V> {
    cache: NonNull<StoredCache<Marker, K, V>>,
    _marker: PhantomData<&'registry CacheRegistry>,
}

impl<Marker, K, V> Clone for BoundCacheEndpoint<'_, Marker, K, V> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<Marker, K, V> Copy for BoundCacheEndpoint<'_, Marker, K, V> {}

// SAFETY: StoredCache is thread-safe and bound to 'registry lifetime.
unsafe impl<Marker, K: Send + Sync, V: Send + Sync> Send for BoundCacheEndpoint<'_, Marker, K, V> {}
// SAFETY: StoredCache is thread-safe and bound to 'registry lifetime.
unsafe impl<Marker, K: Send + Sync, V: Send + Sync> Sync for BoundCacheEndpoint<'_, Marker, K, V> {}

impl<Marker, K, V> CacheEndpoint<Marker, K, V> {
    #[must_use]
    pub const fn new(id: &'static str) -> Self {
        Self {
            id,
            _marker: PhantomData,
            _key: PhantomData,
            _value: PhantomData,
        }
    }

    #[must_use]
    pub const fn id(&self) -> &'static str {
        self.id
    }
}

impl<Marker: 'static, K: 'static, V: 'static> CacheEndpoint<Marker, K, V> {
    #[must_use]
    pub fn key(&self) -> (TypeId, &'static str) {
        (TypeId::of::<(Marker, K, V)>(), self.id)
    }
}

impl<Marker, K, V> BoundCacheEndpoint<'_, Marker, K, V>
where
    K: Clone + Eq + Hash + Send + Sync + 'static,
    V: Send + Sync + 'static,
{
    pub fn get_or_try_insert<'a, F, W>(
        &'a self,
        key: K,
        weight: W,
        compute: F,
    ) -> XllResult<CacheLease<'a, V>>
    where
        F: FnOnce() -> XllResult<V>,
        W: FnOnce(&V) -> usize,
    {
        // SAFETY: self.cache is valid for 'registry, and 'a is within 'registry.
        unsafe { self.cache.as_ref() }
            .cache
            .get_or_try_insert_with(key, weight, compute)
    }

    #[must_use]
    pub fn get<'a>(&'a self, key: &K) -> Option<CacheLease<'a, V>> {
        // SAFETY: self.cache is valid for 'registry, and 'a is within 'registry.
        unsafe { self.cache.as_ref() }.cache.get(key)
    }

    #[must_use]
    pub fn reclamation_stats(&self) -> CacheReclamationStats {
        // SAFETY: this bound capability cannot outlive its registry allocation.
        unsafe { self.cache.as_ref() }.cache.reclamation_stats()
    }

    /// Opens a benchmark-only scoped read region for repeated cache hits.
    #[cfg(feature = "bench-internals")]
    pub fn read_scope<'a>(&'a self) -> XllResult<CacheReadScope<'a, K, V>> {
        // SAFETY: self.cache is valid for 'registry, and 'a is within 'registry.
        unsafe { self.cache.as_ref() }.cache.read_scope()
    }
}

struct StoredCache<Marker, K, V> {
    cache: CalculationCache<K, V>,
    _marker: PhantomData<fn() -> Marker>,
}

type ErasedCache = dyn Any + Send + Sync;

#[derive(Clone, Copy)]
struct CacheOps {
    advance_generation: fn(&ErasedCache) -> u64,
    invalidate_before: fn(&ErasedCache, u64),
}

fn advance_generation<Marker, K, V>(erased: &ErasedCache) -> u64
where
    Marker: 'static,
    K: Clone + Eq + Hash + Send + Sync + 'static,
    V: Send + Sync + 'static,
{
    let stored = erased
        .downcast_ref::<StoredCache<Marker, K, V>>()
        .expect("cache entry type invariant violated");
    stored.cache.generation.advance()
}

fn invalidate_before<Marker, K, V>(erased: &ErasedCache, epoch: u64)
where
    Marker: 'static,
    K: Clone + Eq + Hash + Send + Sync + 'static,
    V: Send + Sync + 'static,
{
    let stored = erased
        .downcast_ref::<StoredCache<Marker, K, V>>()
        .expect("cache entry type invariant violated");
    stored.cache.invalidate_before(epoch);
}

impl CacheOps {
    fn of<Marker, K, V>() -> Self
    where
        Marker: 'static,
        K: Clone + Eq + Hash + Send + Sync + 'static,
        V: Send + Sync + 'static,
    {
        Self {
            advance_generation: advance_generation::<Marker, K, V>,
            invalidate_before: invalidate_before::<Marker, K, V>,
        }
    }
}

struct CacheEntry {
    cache: xlfn_kernel::published_owner::PublishedOwner<ErasedCache>,
    ops: CacheOps,
}

type CacheMap = HashMap<(TypeId, &'static str), CacheEntry>;

pub struct CacheRegistry {
    weight_budget_per_endpoint: usize,
    caches: RwLock<CacheMap>,
}

impl CacheRegistry {
    #[must_use]
    pub fn new(weight_budget_per_endpoint: usize) -> Self {
        Self {
            weight_budget_per_endpoint,
            caches: RwLock::new(HashMap::new()),
        }
    }

    pub fn bind<'registry, Marker, K, V>(
        &'registry self,
        endpoint: &CacheEndpoint<Marker, K, V>,
    ) -> XllResult<BoundCacheEndpoint<'registry, Marker, K, V>>
    where
        Marker: 'static,
        K: Clone + Eq + Hash + Send + Sync + 'static,
        V: Send + Sync + 'static,
    {
        let cache_key = endpoint.key();
        let cache = {
            let caches = self.caches.read();
            if let Some(entry) = caches.get(&cache_key) {
                Self::downcast_cache::<Marker, K, V>(entry)?
            } else {
                drop(caches);
                let mut caches = self.caches.write();
                let entry = caches.entry(cache_key).or_insert_with(|| {
                    let cache = Box::new(StoredCache::<Marker, K, V> {
                        cache: CalculationCache::new(self.weight_budget_per_endpoint),
                        _marker: PhantomData,
                    });
                    let erased: Box<ErasedCache> = cache;
                    CacheEntry {
                        cache: xlfn_kernel::published_owner::PublishedOwner::from_box(erased),
                        ops: CacheOps::of::<Marker, K, V>(),
                    }
                });
                Self::downcast_cache::<Marker, K, V>(entry)?
            }
        };
        Ok(BoundCacheEndpoint {
            cache,
            _marker: PhantomData,
        })
    }

    fn downcast_cache<Marker, K, V>(
        entry: &CacheEntry,
    ) -> XllResult<NonNull<StoredCache<Marker, K, V>>>
    where
        Marker: 'static,
        K: Clone + Eq + Hash + Send + Sync + 'static,
        V: Send + Sync + 'static,
    {
        let erased_ref: &ErasedCache = &*entry.cache;
        let stored = erased_ref
            .downcast_ref::<StoredCache<Marker, K, V>>()
            .ok_or(XllError::Internal {
                diagnostic_id: crate::diagnostics::id::DiagnosticId::CACHE_TYPE,
            })?;
        Ok(NonNull::from(stored))
    }

    pub fn clear(&self) {
        // Entries are never removed during the registry's lifetime. Snapshot
        // pointers to their Box allocations, not to the movable map entries.
        // All invalidation, waiting and user value destruction happen after
        // releasing the registry lock, so destructors can bind new endpoints.
        let snapshot: Vec<_> = {
            let caches = self.caches.read();
            caches
                .values()
                .map(|entry| (NonNull::from(&*entry.cache), entry.ops))
                .collect()
        };
        for (cache, ops) in snapshot {
            // SAFETY: the registry retains every boxed cache until its own
            // destruction, and this snapshot cannot outlive the &self borrow.
            let cache = unsafe { cache.as_ref() };
            let epoch = (ops.advance_generation)(cache);
            (ops.invalidate_before)(cache, epoch);
        }
    }

    #[must_use]
    pub fn endpoint_count(&self) -> usize {
        self.caches.read().len()
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct CanonicalF64(u64);

impl CanonicalF64 {
    pub fn new(value: f64) -> XllResult<Self> {
        if !value.is_finite() {
            return Err(XllError::input("cache_key", InputError::NonFinite));
        }
        let normalized = if value == 0.0 { 0.0 } else { value };
        Ok(Self(normalized.to_bits()))
    }

    #[must_use]
    pub fn get(self) -> f64 {
        f64::from_bits(self.0)
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct VersionedKey<K> {
    epoch: u64,
    key: K,
}

struct VersionedKeyRef<'a, K> {
    epoch: u64,
    key: &'a K,
}

impl<K: Hash> Hash for VersionedKeyRef<'_, K> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.epoch.hash(state);
        self.key.hash(state);
    }
}

struct NodePtr<V>(NonNull<CacheNode<V>>);

impl<V> Clone for NodePtr<V> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<V> Copy for NodePtr<V> {}

// SAFETY: NodePtr shares a pinned node with other readers (V: Sync), and the
// receiving thread may retire it and eventually destroy its value (V: Send).
unsafe impl<V: Send + Sync> Send for NodePtr<V> {}
// SAFETY: NodePtr is Copy, so sharing a reference also lets its recipient
// obtain a capability whose eventual retirement can destroy V there.
unsafe impl<V: Send + Sync> Sync for NodePtr<V> {}

// The resident index only selects entries to remove. The residency state,
// pin release, and retirement enqueue remain owned by CalculationCache.
fn retire_resident<V>(node_ptr: NodePtr<V>) {
    // SAFETY: the index transfers its one outstanding residency obligation.
    let node = unsafe { node_ptr.0.as_ref() };
    node.resident.store(false, Ordering::Release);
    if node.release_pin() {
        // SAFETY: the cache retains its domain until every index entry retires.
        let domain = unsafe { node.domain.as_ref() };
        domain.enqueue_reclaim(node_ptr.0.as_ptr() as *mut (), node.weight);
    }
}

struct CacheNode<V> {
    value: Box<V>,
    pins: AtomicUsize,
    resident: AtomicBool,
    weight: u32,
    generation: u64,
    domain: NonNull<CacheLookupDomain>,
}

// SAFETY: Box<V> is Send if V: Send.
unsafe impl<V: Send> Send for CacheNode<V> {}
// SAFETY: Box<V> is Sync if V: Sync.
unsafe impl<V: Sync> Sync for CacheNode<V> {}

#[derive(Debug, PartialEq, Eq)]
struct PinOverflow;

impl<V> CacheNode<V> {
    #[inline]
    fn try_acquire_pin(&self) -> Result<bool, PinOverflow> {
        self.pins
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |pins| {
                // Zero is terminal: resurrecting a node after its final pin
                // is released would let two threads race to retire it, while
                // one of them might still be accessing the allocation.
                (pins != 0).then(|| pins.checked_add(1)).flatten()
            })
            .map(|_| true)
            .or_else(|pins| {
                if pins == 0 {
                    Ok(false)
                } else {
                    Err(PinOverflow)
                }
            })
    }

    #[inline]
    fn release_pin(&self) -> bool {
        let prev = self.pins.fetch_sub(1, Ordering::AcqRel);
        if prev == 0 {
            xlfn_kernel::invariant::fail_stop();
        }
        prev == 1
    }
}

struct ReclaimEntry(*mut (), u32);
// SAFETY: ReclaimEntry holds a raw pointer to a retired CacheNode to be freed on a quiesced domain.
unsafe impl Send for ReclaimEntry {}

type CacheDomainPermit<'domain> = RotatingReadPermit<'domain, DEFAULT_STRIPE_COUNT>;

struct CacheLookupDomain {
    // D1-D5 are provided by RotatingReadDomain. A cache node must be freed
    // only after the grace period covering every reader that could
    // have observed its pointer has ended.
    domain: RotatingReadDomain<DEFAULT_STRIPE_COUNT>,
    pending_reclaims: [Mutex<Vec<ReclaimEntry>>; 2],
    pending_nodes: AtomicUsize,
    pending_weight: AtomicU64,
    peak_pending_nodes: AtomicUsize,
    peak_pending_weight: AtomicU64,
    reclaimed_nodes: AtomicUsize,
    largest_batch: AtomicUsize,
    grace_period_nanos: AtomicU64,
}

impl CacheLookupDomain {
    fn new() -> Self {
        Self {
            domain: RotatingReadDomain::new(),
            pending_reclaims: [Mutex::new(Vec::new()), Mutex::new(Vec::new())],
            pending_nodes: AtomicUsize::new(0),
            pending_weight: AtomicU64::new(0),
            peak_pending_nodes: AtomicUsize::new(0),
            peak_pending_weight: AtomicU64::new(0),
            reclaimed_nodes: AtomicUsize::new(0),
            largest_batch: AtomicUsize::new(0),
            grace_period_nanos: AtomicU64::new(0),
        }
    }

    #[inline]
    fn enter(&self) -> XllResult<CacheDomainPermit<'_>> {
        self.domain
            .enter_current_thread()
            .map_err(|_| XllError::Closing)
    }

    fn enqueue_reclaim(&self, ptr: *mut (), weight: u32) {
        self.enqueue_reclaim_impl(ptr, weight, |_| {});
    }

    #[cfg(test)]
    fn enqueue_reclaim_with_hook(
        &self,
        ptr: *mut (),
        after_generation_load: impl Fn(GenerationIndex),
    ) {
        self.enqueue_reclaim_impl(ptr, 0, after_generation_load);
    }

    fn enqueue_reclaim_impl(
        &self,
        ptr: *mut (),
        weight: u32,
        after_generation_load: impl Fn(GenerationIndex),
    ) {
        // The queue lock is the enqueue linearization point. Revalidate the
        // generation while holding it so a rotation cannot drain the queue
        // just before this retired node is appended.
        loop {
            let generation = self.domain.current_generation();
            after_generation_load(generation);
            let mut queue = self.pending_reclaims[generation.index()].lock();
            if self.domain.current_generation() != generation {
                drop(queue);
                std::hint::spin_loop();
                continue;
            }
            queue.push(ReclaimEntry(ptr, weight));
            let nodes = self.pending_nodes.fetch_add(1, Ordering::Relaxed) + 1;
            let weight = self
                .pending_weight
                .fetch_add(u64::from(weight), Ordering::Relaxed)
                + u64::from(weight);
            self.peak_pending_nodes.fetch_max(nodes, Ordering::Relaxed);
            self.peak_pending_weight
                .fetch_max(weight, Ordering::Relaxed);
            return;
        }
    }

    fn quiesce_and_drain(&self) -> Vec<ReclaimEntry> {
        // A caller may release a lease or clear/read cache metrics from a
        // compute/weight callback. Defer value destruction until that outer
        // singleflight has returned, just as ordinary maintenance does.
        if ACTIVE_CACHE_INITIALIZATION_DEPTH.get() != 0 {
            return Vec::new();
        }
        let start = Instant::now();
        let entries = self
            .domain
            .quiesce(|generation| self.drain_generation(generation))
            .unwrap_or_default();
        self.record_batch(&entries, start);
        entries
    }

    fn try_quiesce_and_drain(&self) -> Vec<ReclaimEntry> {
        if ACTIVE_CACHE_INITIALIZATION_DEPTH.get() != 0
            || self.pending_nodes.load(Ordering::Relaxed) == 0
        {
            return Vec::new();
        }
        let start = Instant::now();
        let Some(result) = self
            .domain
            .try_quiesce_if_idle(|generation| self.drain_generation(generation))
        else {
            return Vec::new();
        };
        let entries = result.unwrap_or_default();
        self.record_batch(&entries, start);
        entries
    }

    fn drain_generation(&self, generation: DrainedGeneration) -> Vec<ReclaimEntry> {
        let mut queue = self.pending_reclaims[generation.index()].lock();
        let entries = std::mem::take(&mut *queue);
        self.pending_nodes
            .fetch_sub(entries.len(), Ordering::Relaxed);
        let weight = entries.iter().map(|entry| u64::from(entry.1)).sum();
        self.pending_weight.fetch_sub(weight, Ordering::Relaxed);
        entries
    }

    fn record_batch(&self, entries: &[ReclaimEntry], start: Instant) {
        if !entries.is_empty() {
            self.reclaimed_nodes
                .fetch_add(entries.len(), Ordering::Relaxed);
            self.largest_batch
                .fetch_max(entries.len(), Ordering::Relaxed);
            let nanos = u64::try_from(start.elapsed().as_nanos()).unwrap_or(u64::MAX);
            self.grace_period_nanos.fetch_add(nanos, Ordering::Relaxed);
        }
    }

    fn stats(&self) -> CacheReclamationStats {
        CacheReclamationStats {
            pending_nodes: self.pending_nodes.load(Ordering::Relaxed),
            pending_weight: self.pending_weight.load(Ordering::Relaxed),
            peak_pending_nodes: self.peak_pending_nodes.load(Ordering::Relaxed),
            peak_pending_weight: self.peak_pending_weight.load(Ordering::Relaxed),
            reclaimed_nodes: self.reclaimed_nodes.load(Ordering::Relaxed),
            largest_batch: self.largest_batch.load(Ordering::Relaxed),
            grace_period_nanos: self.grace_period_nanos.load(Ordering::Relaxed),
        }
    }

    fn seal(&self) {
        self.domain.seal_and_wait();
    }

    fn drain_all(&self) -> Vec<ReclaimEntry> {
        let mut all = Vec::new();
        for gen_idx in 0..2 {
            let items = {
                let mut queue = self.pending_reclaims[gen_idx].lock();
                std::mem::take(&mut *queue)
            };
            all.extend(items);
        }
        all
    }
}

/// Test/benchmark-only resident policy selection. Production uses Moka.
#[cfg(feature = "bench-internals")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CacheBackend {
    Moka,
    Sharded { shards: usize },
}

/// Side-effect-free residency and approximate storage observations.
#[cfg(feature = "bench-internals")]
#[derive(Clone, Copy, Debug)]
pub struct CacheResidentStats {
    pub entries: u64,
    pub weight: u64,
    pub index_bytes_estimate: usize,
    /// Moka does not expose policy/table allocation sizes; its estimate is a
    /// lower bound that excludes opaque metadata. Key heap storage is excluded.
    pub index_metadata_opaque: bool,
    /// Resident node headers plus caller-reported payload weights. Excludes
    /// live non-resident leases and queued retirement debt.
    pub resident_node_bytes_estimate: u64,
}

pub struct CalculationCache<K, V> {
    weight_budget: usize,
    generation: CacheGeneration,
    domain: xlfn_kernel::published_owner::PublishedOwner<CacheLookupDomain>,
    clear_lock: Mutex<()>,
    mutations: AtomicUsize,
    index: ResidentIndex<K, V>,
    clear_fn: Option<fn(*const ())>,
}

impl<K, V> CalculationCache<K, V>
where
    K: Clone + Eq + Hash + Send + Sync + 'static,
    V: Send + Sync + 'static,
{
    /// Creates a concurrent, weighted cache backed by Moka's TinyLFU policy.
    ///
    /// Weight is supplied with each initialization. Values heavier than the
    /// configured budget are returned to the caller but are not retained.
    /// Size and entry metrics are approximate until Moka runs maintenance.
    /// Cache misses cannot start another cache initialization from inside an
    /// initializer. Existing cached values may still be read normally.
    #[must_use]
    pub fn new(weight_budget: usize) -> Self {
        Self::new_with_eviction_hook(weight_budget, || {})
    }

    // The production no-op is eliminated during monomorphization; tests can
    // make Moka's time-limited eviction pass stop before draining the cache.
    fn new_with_eviction_hook(
        weight_budget: usize,
        after_eviction: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        Self::with_index(weight_budget, |capacity| {
            ResidentIndex::moka(capacity, move |(node_ptr, _weight)| {
                retire_resident(node_ptr);
                after_eviction();
            })
        })
    }

    /// Builds a candidate using the identical cache/lifetime implementation.
    #[cfg(feature = "bench-internals")]
    #[must_use]
    pub fn new_with_backend(weight_budget: usize, backend: CacheBackend) -> Self {
        Self::with_index(weight_budget, |capacity| match backend {
            CacheBackend::Moka => ResidentIndex::moka(capacity, |entry| retire_resident(entry.0)),
            CacheBackend::Sharded { shards } => {
                ResidentIndex::sharded(capacity, shards, |entry| retire_resident(entry.0))
            }
        })
    }

    fn with_index(
        weight_budget: usize,
        make_index: impl FnOnce(u64) -> ResidentIndex<K, V>,
    ) -> Self {
        let weight_budget = weight_budget.min(u32::MAX as usize);
        let capacity = u64::try_from(weight_budget).unwrap_or(u64::MAX);
        Self {
            weight_budget,
            generation: CacheGeneration::new(),
            domain: xlfn_kernel::published_owner::PublishedOwner::new(CacheLookupDomain::new()),
            clear_lock: Mutex::new(()),
            mutations: AtomicUsize::new(0),
            index: make_index(capacity),
            clear_fn: Some(|ptr| {
                // SAFETY: [TR-RECLAIM-1] ptr points to a valid CalculationCache<K, V> during Drop.
                let cache = unsafe { &*(ptr as *const Self) };
                cache.domain.seal();
                // Drop has exclusive access, so no new generation can be
                // published. Moka may time-limit a maintenance pass: keep
                // driving invalidation until every resident pin is released.
                cache.index.clear();
                loop {
                    cache.index.maintenance();
                    if cache.index.resident_count() == 0 {
                        break;
                    }
                }
                let retired = cache.domain.drain_all();
                reclaim_cache_entries::<V>(retired);
            }),
        }
    }

    #[must_use]
    pub const fn weight_budget(&self) -> usize {
        self.weight_budget
    }

    /// Observes reclamation without running maintenance. Pending counters
    /// exclude resident nodes and values kept alive by outstanding leases.
    #[must_use]
    pub fn reclamation_stats(&self) -> CacheReclamationStats {
        self.domain.stats()
    }

    #[cfg(feature = "bench-internals")]
    #[must_use]
    pub fn resident_stats(&self) -> CacheResidentStats {
        let entries = self.index.resident_count();
        let weight = self.index.resident_weight();
        let (index_bytes_estimate, index_metadata_opaque) = self.index.memory_estimate();
        CacheResidentStats {
            entries,
            weight,
            index_bytes_estimate,
            index_metadata_opaque,
            resident_node_bytes_estimate: entries
                .saturating_mul(std::mem::size_of::<CacheNode<V>>() as u64)
                .saturating_add(weight),
        }
    }

    /// Removes one current-generation entry and services ordinary maintenance.
    #[cfg(feature = "bench-internals")]
    pub fn invalidate(&self, key: &K) {
        self.index.invalidate(&VersionedKey {
            epoch: self.generation.snapshot(),
            key: key.clone(),
        });
        self.index.maintenance();
        self.maintain(true);
    }

    /// Completes index maintenance and the pending reclamation grace period.
    #[cfg(feature = "bench-internals")]
    pub fn maintenance(&self) {
        self.index.maintenance();
        reclaim_cache_entries::<V>(self.domain.quiesce_and_drain());
    }

    fn maintain(&self, mutation: bool) {
        // A read inside an initializer must not run another value's Drop
        // while Moka is still executing that initializer's singleflight.
        if ACTIVE_CACHE_INITIALIZATION_DEPTH.get() != 0 {
            return;
        }
        if mutation
            && self.mutations.fetch_add(1, Ordering::Relaxed) % MAINTENANCE_INTERVAL
                == MAINTENANCE_INTERVAL - 1
        {
            self.index.maintenance();
        }
        let nodes = self.domain.pending_nodes.load(Ordering::Relaxed);
        if nodes == 0 {
            return;
        }
        // Under sustained contention, stop admitting more mutation debt until
        // the old readers drain. Hot reads only attempt an idle reclamation.
        let retired = if mutation
            && (nodes >= RECLAIM_BACKPRESSURE_NODES
                || self.domain.pending_weight.load(Ordering::Relaxed)
                    >= self.weight_budget.max(1) as u64)
        {
            self.domain.quiesce_and_drain()
        } else {
            self.domain.try_quiesce_and_drain()
        };
        reclaim_cache_entries::<V>(retired);
    }

    #[must_use]
    pub fn used_weight(&self) -> usize {
        self.index.maintenance();
        let retired = self.domain.try_quiesce_and_drain();
        reclaim_cache_entries::<V>(retired);
        usize::try_from(self.index.resident_weight()).unwrap_or(usize::MAX)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.index.maintenance();
        let retired = self.domain.try_quiesce_and_drain();
        reclaim_cache_entries::<V>(retired);
        usize::try_from(self.index.resident_count()).unwrap_or(usize::MAX)
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn clear(&self) {
        let retired = {
            let _guard = self.clear_lock.lock();
            let epoch = self.generation.advance();
            self.index.invalidate_before(epoch);
            self.index.maintenance();
            self.domain.quiesce_and_drain()
        };
        reclaim_cache_entries::<V>(retired);
    }

    /// Benchmark-only synchronization hook for measuring a clear that reaches
    /// reclamation while a scoped reader is still active.
    #[cfg(feature = "bench-internals")]
    pub(crate) fn clear_with_quiesce_hook(&self, before_quiesce: impl FnOnce()) {
        let retired = {
            let _guard = self.clear_lock.lock();
            let epoch = self.generation.advance();
            self.index.invalidate_before(epoch);
            self.index.maintenance();
            before_quiesce();
            self.domain.quiesce_and_drain()
        };
        reclaim_cache_entries::<V>(retired);
    }

    fn invalidate_before(&self, epoch: u64) {
        let retired = {
            let _guard = self.clear_lock.lock();
            self.index.invalidate_before(epoch);
            self.index.maintenance();
            self.domain.quiesce_and_drain()
        };
        reclaim_cache_entries::<V>(retired);
    }

    pub fn get<'a>(&'a self, key: &K) -> Option<CacheLease<'a, V>> {
        let epoch = self.generation.snapshot();
        let lease = self.get_at_epoch(key, epoch);
        self.maintain(false);
        lease
    }

    /// Opens a benchmark-only scoped read region for repeated cache hits.
    ///
    /// The scope holds one lookup-domain permit until it is dropped. It does
    /// not alter the existing `CacheLease`-based insertion or miss path.
    #[cfg(feature = "bench-internals")]
    pub fn read_scope(&self) -> XllResult<CacheReadScope<'_, K, V>> {
        let permit = self.domain.enter()?;
        Ok(CacheReadScope {
            cache: self,
            _permit: permit,
            _not_send_sync: PhantomData,
        })
    }

    fn get_at_epoch<'a>(&'a self, key: &K, epoch: u64) -> Option<CacheLease<'a, V>> {
        let permit = self.domain.enter().ok()?;
        let lookup = VersionedKeyRef { epoch, key };
        let (node_ptr, _) = self.index.get(&lookup)?;
        // SAFETY: [TR-OBSERVE-POINTER] node_ptr is observed only while holding a valid lookup admission domain permit.
        let node = unsafe { node_ptr.0.as_ref() };
        if node.generation != epoch || !node.resident.load(Ordering::Acquire) {
            drop(permit);
            return None;
        }
        // TR-ACQUIRE-PIN: Increment pin capability while still within admission domain.
        match node.try_acquire_pin() {
            Ok(true) => {}
            Ok(false) => return None,
            Err(PinOverflow) => xlfn_kernel::invariant::fail_stop(),
        }
        if !node.resident.load(Ordering::Acquire) {
            let was_last = node.release_pin();
            let domain_ptr = node.domain;
            let weight = node.weight;
            drop(permit);
            if was_last {
                // SAFETY: [TR-RECLAIM-1] The node was retired (non-resident) and this was the last pin.
                let domain = unsafe { domain_ptr.as_ref() };
                domain.enqueue_reclaim(node_ptr.0.as_ptr() as *mut (), weight);
                let retired = domain.quiesce_and_drain();
                reclaim_cache_entries::<V>(retired);
            }
            return None;
        }
        // TR-LOOKUP-LEAVE: Release lookup admission permit now that node is pinned.
        drop(permit);
        Some(CacheLease {
            node: node_ptr.0,
            _marker: PhantomData,
        })
    }

    pub fn get_or_try_insert_with<'a, F, W>(
        &'a self,
        key: K,
        weight: W,
        compute: F,
    ) -> XllResult<CacheLease<'a, V>>
    where
        F: FnOnce() -> XllResult<V>,
        W: FnOnce(&V) -> usize,
    {
        let epoch = self.generation.snapshot();
        self.get_or_try_insert_at_epoch(key, weight, compute, epoch)
    }

    fn get_or_try_insert_at_epoch<'a, F, W>(
        &'a self,
        key: K,
        weight: W,
        compute: F,
        mut epoch: u64,
    ) -> XllResult<CacheLease<'a, V>>
    where
        F: FnOnce() -> XllResult<V>,
        W: FnOnce(&V) -> usize,
    {
        // This guard predates every initialization guard and singleflight
        // frame below, so their unwind cleanup finishes before reclamation.
        // Only drain already-retired nodes here: index maintenance could run
        // unrelated key callbacks while propagating the original panic.
        // Never wait for readers during unwinding: an outer lookup's key
        // callback may have invoked this operation while holding a permit.
        let _reclaim_on_unwind = scopeguard::guard_on_unwind(self, |cache| {
            let _ = catch_no_unwind(AssertUnwindSafe(|| {
                reclaim_cache_entries::<V>(cache.domain.try_quiesce_and_drain());
            }));
        });
        if self.weight_budget == 0 {
            // Moka disables its map at zero capacity and never invokes the
            // eviction listener. Do not create a residency pin that nobody
            // could release. The caller's lease uniquely owns this node.
            let active = ActiveCacheGuard::enter()?;
            let initialized = (|| {
                let value = compute()?;
                let measured = weight(&value);
                let node = Box::new(CacheNode {
                    value: Box::new(value),
                    pins: AtomicUsize::new(1),
                    resident: AtomicBool::new(false),
                    weight: u32::try_from(measured).unwrap_or(u32::MAX).max(1),
                    generation: epoch,
                    domain: NonNull::from(&*self.domain),
                });
                Ok(CacheLease {
                    node: NonNull::from(Box::leak(node)),
                    _marker: PhantomData,
                })
            })();
            drop(active);
            // As with the resident path, callbacks may have dropped another
            // lease whose reclamation was deferred by the initialization
            // guard. Service that debt even if this initializer failed.
            self.maintain(true);
            return initialized;
        }
        let mut compute_opt = Some(compute);
        let mut weight_opt = Some(weight);

        loop {
            if let Some(lease) = self.get_at_epoch(&key, epoch) {
                self.maintain(false);
                return Ok(lease);
            }

            let _active = ActiveCacheGuard::enter()?;
            let vkey = VersionedKey {
                epoch,
                key: key.clone(),
            };
            let domain_ptr = NonNull::from(&*self.domain);
            let mut created = false;
            let mut oversized = false;

            let initialized = self
                .index
                .insert(vkey.clone(), || {
                    let compute_fn = compute_opt.take().expect("compute called once");
                    let weight_fn = weight_opt.take().expect("weight called once");
                    let value = compute_fn()?;
                    let measured = weight_fn(&value);
                    oversized = measured > self.weight_budget;
                    let w = u32::try_from(measured).unwrap_or(u32::MAX).max(1);
                    let boxed = Box::new(value);
                    let node = Box::new(CacheNode {
                        value: boxed,
                        pins: AtomicUsize::new(2), // 1 for Moka residency, 1 for creator lease
                        resident: AtomicBool::new(true),
                        weight: w,
                        generation: epoch,
                        domain: domain_ptr,
                    });
                    created = true;
                    let ptr = NodePtr(NonNull::from(Box::leak(node)));
                    Ok::<_, XllError>((ptr, w))
                })
                .map_err(|error| (*error).clone());

            drop(_active);

            self.maintain(true);

            let (node_ptr, _weight) = initialized?;

            if created {
                // SAFETY: [TR-ACQUIRE-PIN] node was allocated with pins = 2 (1 for Moka, 1 for this lease).
                // Live pin guarantees node cannot be reclaimed by concurrent eviction or clear.
                if oversized {
                    // Compare the original estimate, before clamping it to
                    // Moka's u32 weigher. A larger value must not become a
                    // resident merely because its weight was saturated.
                    self.index.invalidate(&vkey);
                } else {
                    self.generation.discard_if_stale(epoch, || {
                        self.index.invalidate(&vkey);
                    });
                }
                return Ok(CacheLease {
                    node: node_ptr.0,
                    _marker: PhantomData,
                });
            }

            // Another thread initialized the entry; acquire a pin safely through the admission domain.
            if let Some(lease) = self.get_at_epoch(&key, epoch) {
                return Ok(lease);
            }

            // The entry was evicted or invalidated before we could acquire a pin; retry with fresh epoch.
            epoch = self.generation.snapshot();
        }
    }
}

impl<K, V> Drop for CalculationCache<K, V> {
    fn drop(&mut self) {
        if let Some(clean) = self.clear_fn {
            clean(self as *const Self as *const ());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::hash::{Hash, Hasher};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, mpsc};
    use std::time::Duration;

    struct CloneCountedKey {
        value: u64,
        clones: Arc<AtomicUsize>,
    }

    impl Clone for CloneCountedKey {
        fn clone(&self) -> Self {
            self.clones.fetch_add(1, Ordering::SeqCst);
            Self {
                value: self.value,
                clones: Arc::clone(&self.clones),
            }
        }
    }

    impl PartialEq for CloneCountedKey {
        fn eq(&self, other: &Self) -> bool {
            self.value == other.value
        }
    }

    impl Eq for CloneCountedKey {}

    impl Hash for CloneCountedKey {
        fn hash<H: Hasher>(&self, state: &mut H) {
            self.value.hash(state);
        }
    }

    #[test]
    fn cache_capabilities_require_shareable_values_to_cross_threads() {
        static_assertions::assert_impl_all!(CacheLease<'static, u32>: Send, Sync);
        static_assertions::assert_impl_all!(NodePtr<u32>: Send, Sync);
        // Cell can be moved, but shared access from independent leases would
        // race. Guard the capability's own contract, not only its constructors.
        static_assertions::assert_not_impl_any!(CacheLease<'static, Cell<u32>>: Send, Sync);
        static_assertions::assert_not_impl_any!(NodePtr<Cell<u32>>: Send, Sync);
        static_assertions::assert_not_impl_any!(
            NodePtr<std::sync::MutexGuard<'static, ()>>: Send, Sync
        );
        static_assertions::assert_not_impl_any!(CacheLease<'static, std::rc::Rc<u32>>: Send, Sync);
    }

    #[test]
    fn canonical_float_normalizes_signed_zero_and_rejects_nan() {
        assert_eq!(
            CanonicalF64::new(-0.0).unwrap(),
            CanonicalF64::new(0.0).unwrap()
        );
        assert!(CanonicalF64::new(f64::NAN).is_err());
        assert!(CanonicalF64::new(f64::INFINITY).is_err());
    }

    #[test]
    fn endpoint_identity_includes_key_and_value_types() {
        enum Marker {}
        static NUMBERS: CacheEndpoint<Marker, u32, u32> = CacheEndpoint::new("shared-id");
        static TEXT: CacheEndpoint<Marker, String, String> = CacheEndpoint::new("shared-id");
        let registry = CacheRegistry::new(64);

        assert_eq!(
            *registry
                .bind(&NUMBERS)
                .unwrap()
                .get_or_try_insert(1, |_| 4, || Ok(7))
                .unwrap(),
            7
        );
        assert_eq!(
            registry
                .bind(&TEXT)
                .unwrap()
                .get_or_try_insert(String::from("key"), String::len, || Ok(String::from(
                    "value"
                )))
                .unwrap()
                .as_str(),
            "value"
        );
        assert_eq!(registry.endpoint_count(), 2);
    }

    #[test]
    fn zero_weight_entries_still_consume_budget() {
        let cache = CalculationCache::new(1);
        cache
            .get_or_try_insert_with("first", |_| 0, || Ok::<_, XllError>(1_u32))
            .unwrap();
        cache.index.maintenance();
        assert_eq!(cache.used_weight(), 1);
    }

    #[test]
    fn cache_hit_does_not_clone_key() {
        let clones = Arc::new(AtomicUsize::new(0));
        let cache = CalculationCache::<CloneCountedKey, u32>::new(8);
        let key = CloneCountedKey {
            value: 1,
            clones: Arc::clone(&clones),
        };

        assert_eq!(
            *cache.get_or_try_insert_with(key, |_| 1, || Ok(7)).unwrap(),
            7
        );

        let lookup = CloneCountedKey {
            value: 1,
            clones: Arc::clone(&clones),
        };
        let clones_before_hit = clones.load(Ordering::SeqCst);

        assert_eq!(*cache.get(&lookup).unwrap(), 7);
        assert_eq!(clones.load(Ordering::SeqCst), clones_before_hit);
    }

    #[test]
    fn enqueue_reclaim_racing_rotation_is_not_lost() {
        let domain = Arc::new(CacheLookupDomain::new());
        let (loaded_tx, loaded_rx) = mpsc::sync_channel(0);
        let (resume_tx, resume_rx) = mpsc::sync_channel(0);
        let (rotated_tx, rotated_rx) = mpsc::sync_channel(0);

        let enqueuer_domain = Arc::clone(&domain);
        let enqueuer = std::thread::spawn(move || {
            let first_load = std::cell::Cell::new(true);
            enqueuer_domain.enqueue_reclaim_with_hook(std::ptr::null_mut::<()>(), |generation| {
                if first_load.replace(false) {
                    loaded_tx.send(generation).unwrap();
                    resume_rx.recv().unwrap();
                }
            });
        });

        assert_eq!(loaded_rx.recv().unwrap().index(), 0);

        let rotating_domain = Arc::clone(&domain);
        let rotator = std::thread::spawn(move || {
            let _ = rotating_domain.quiesce_and_drain();
            rotated_tx.send(()).unwrap();
        });

        rotated_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("rotation must publish the next generation");
        resume_tx.send(()).unwrap();
        enqueuer.join().unwrap();
        rotator.join().unwrap();

        assert!(domain.pending_reclaims[0].lock().is_empty());
        assert_eq!(domain.pending_reclaims[1].lock().len(), 1);
        let _ = domain.drain_all();
    }

    #[cfg(feature = "bench-internals")]
    #[test]
    fn cache_read_scope_is_not_send_or_sync() {
        static_assertions::assert_not_impl_any!(CacheReadScope<'static, u32, u32>: Send, Sync);
    }

    #[cfg(feature = "bench-internals")]
    #[test]
    fn bound_endpoint_exposes_scoped_reads() {
        enum Marker {}
        static ENDPOINT: CacheEndpoint<Marker, u32, u32> = CacheEndpoint::new("SCOPED_READ");

        let registry = CacheRegistry::new(8);
        let endpoint = registry.bind(&ENDPOINT).unwrap();
        drop(endpoint.get_or_try_insert(1, |_| 1, || Ok(7)).unwrap());

        let scope = endpoint.read_scope().unwrap();
        assert_eq!(*scope.get(&1).unwrap(), 7);
    }

    #[cfg(feature = "bench-internals")]
    #[test]
    fn scoped_reference_survives_eviction_until_scope_drop() {
        struct DropProbe {
            value: u32,
            drops: Arc<AtomicUsize>,
        }

        impl Drop for DropProbe {
            fn drop(&mut self) {
                self.drops.fetch_add(1, Ordering::SeqCst);
            }
        }

        let drops = Arc::new(AtomicUsize::new(0));
        let cache = CalculationCache::<u32, DropProbe>::new(1);
        drop(
            cache
                .get_or_try_insert_with(
                    0,
                    |_| 1,
                    || {
                        Ok(DropProbe {
                            value: 7,
                            drops: Arc::clone(&drops),
                        })
                    },
                )
                .unwrap(),
        );

        let scope = cache.read_scope().unwrap();
        {
            let value = scope.get(&0).unwrap();
            assert_eq!(value.value, 7);

            let (retired_tx, retired_rx) = std::sync::mpsc::sync_channel(0);
            let (reclaim_done_tx, reclaim_done_rx) = std::sync::mpsc::sync_channel(0);
            let cache_ref = &cache;

            std::thread::scope(|threads| {
                let reclaimer = threads.spawn(move || {
                    let epoch = cache_ref.generation.advance();
                    cache_ref.index.invalidate_before(epoch);
                    cache_ref.index.maintenance();
                    assert!(
                        cache_ref
                            .domain
                            .pending_reclaims
                            .iter()
                            .any(|queue| !queue.lock().is_empty())
                    );
                    retired_tx.send(()).unwrap();
                    let retired = cache_ref.domain.quiesce_and_drain();
                    reclaim_cache_entries::<DropProbe>(retired);
                    reclaim_done_tx.send(()).unwrap();
                });

                retired_rx.recv().unwrap();
                assert_eq!(drops.load(Ordering::SeqCst), 0);
                assert!(reclaim_done_rx.try_recv().is_err());

                drop(scope);
                reclaim_done_rx.recv().unwrap();
                reclaimer.join().unwrap();
            });
        }

        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }

    #[cfg(feature = "bench-internals")]
    #[test]
    fn scoped_reference_blocks_clear_until_scope_drop() {
        struct DropProbe {
            value: u32,
            drops: Arc<AtomicUsize>,
        }

        impl Drop for DropProbe {
            fn drop(&mut self) {
                self.drops.fetch_add(1, Ordering::SeqCst);
            }
        }

        let drops = Arc::new(AtomicUsize::new(0));
        let cache = CalculationCache::<u32, DropProbe>::new(8);
        drop(
            cache
                .get_or_try_insert_with(
                    0,
                    |_| 1,
                    || {
                        Ok(DropProbe {
                            value: 11,
                            drops: Arc::clone(&drops),
                        })
                    },
                )
                .unwrap(),
        );

        let scope = cache.read_scope().unwrap();
        {
            let value = scope.get(&0).unwrap();
            assert_eq!(value.value, 11);

            let (clear_done_tx, clear_done_rx) = std::sync::mpsc::sync_channel(0);
            let cache_ref = &cache;

            std::thread::scope(|threads| {
                let clearer = threads.spawn(move || {
                    cache_ref.clear();
                    clear_done_tx.send(()).unwrap();
                });

                let deadline = std::time::Instant::now() + Duration::from_secs(2);
                while !cache
                    .domain
                    .pending_reclaims
                    .iter()
                    .any(|queue| !queue.lock().is_empty())
                    && std::time::Instant::now() < deadline
                {
                    std::thread::yield_now();
                }
                assert!(
                    cache
                        .domain
                        .pending_reclaims
                        .iter()
                        .any(|queue| !queue.lock().is_empty())
                );
                assert!(clear_done_rx.try_recv().is_err());
                assert_eq!(drops.load(Ordering::SeqCst), 0);

                drop(scope);
                clear_done_rx.recv().unwrap();
                clearer.join().unwrap();
            });
        }

        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }

    #[cfg(feature = "bench-internals")]
    #[test]
    fn scoped_read_uses_current_generation_for_each_lookup() {
        let cache = CalculationCache::<u32, u32>::new(8);
        drop(cache.get_or_try_insert_with(1, |_| 1, || Ok(23)).unwrap());

        let scope = cache.read_scope().unwrap();
        assert_eq!(*scope.get(&1).unwrap(), 23);
        cache.generation.advance();
        assert!(scope.get(&1).is_none());
        drop(scope);
    }

    #[test]
    fn singleflight_runs_one_computation() {
        let cache = CalculationCache::<u32, u32>::new(1024);
        let calls = AtomicUsize::new(0);
        std::thread::scope(|s| {
            for _ in 0..16 {
                s.spawn(|| {
                    let lease = cache
                        .get_or_try_insert_with(
                            7,
                            |_| 4,
                            || {
                                calls.fetch_add(1, Ordering::SeqCst);
                                std::thread::sleep(Duration::from_millis(10));
                                Ok(49)
                            },
                        )
                        .unwrap();
                    assert_eq!(*lease, 49);
                });
            }
        });
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn tiny_lfu_eviction_is_bounded_by_approximate_bytes() {
        let cache = CalculationCache::new(8);
        cache
            .get_or_try_insert_with(1, |_| 8, || Ok(10_u32))
            .unwrap();
        cache
            .get_or_try_insert_with(2, |_| 8, || Ok(20_u32))
            .unwrap();
        assert!(cache.used_weight() <= 8);
        assert!(cache.len() <= 1);
        assert!(cache.get(&1).is_some() || cache.get(&2).is_some());
    }

    #[test]
    fn panicking_computation_does_not_leave_a_stuck_singleflight() {
        let cache = CalculationCache::<u32, u32>::new(8);
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = cache.get_or_try_insert_with(
                1,
                |_| 4,
                || -> XllResult<u32> { panic!("injected computation panic") },
            );
        }));
        assert!(panic.is_err());
        assert_eq!(
            *cache.get_or_try_insert_with(1, |_| 4, || Ok(7)).unwrap(),
            7
        );
    }

    #[test]
    fn panicking_weight_does_not_leave_a_stuck_singleflight() {
        let cache = CalculationCache::<u32, u32>::new(8);
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = cache.get_or_try_insert_with(1, |_| panic!("weight panic"), || Ok(7));
        }));
        assert!(panic.is_err());
        assert_eq!(
            *cache.get_or_try_insert_with(1, |_| 4, || Ok(8)).unwrap(),
            8
        );
    }

    #[test]
    fn oversized_values_are_returned_without_being_cached() {
        let cache = CalculationCache::<u32, u32>::new(4);
        assert_eq!(
            *cache.get_or_try_insert_with(1, |_| 8, || Ok(7)).unwrap(),
            7
        );
        assert!(cache.is_empty());
    }

    #[test]
    fn clear_allows_an_inflight_moka_initializer_to_complete() {
        let cache = CalculationCache::<u32, u32>::new(8);
        let (started_tx, started_rx) = std::sync::mpsc::sync_channel(1);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
        let cache_ref = &cache;
        std::thread::scope(|s| {
            let handle = s.spawn(move || {
                let lease = cache_ref
                    .get_or_try_insert_with(
                        1,
                        |_| 4,
                        || {
                            started_tx.send(()).unwrap();
                            release_rx.recv().unwrap();
                            Ok(7)
                        },
                    )
                    .unwrap();
                *lease
            });
            started_rx.recv().unwrap();
            cache.clear();
            release_tx.send(()).unwrap();
            assert_eq!(handle.join().unwrap(), 7);
            assert!(cache.get(&1).is_none());
        });
    }

    #[test]
    fn operation_that_enters_cache_after_clear_cannot_retain_its_old_epoch() {
        let cache = CalculationCache::<u32, u32>::new(8);
        let old_epoch = cache.generation.snapshot();
        cache.clear();

        assert_eq!(
            *cache
                .get_or_try_insert_at_epoch(1, |_| 4, || Ok(7), old_epoch)
                .unwrap(),
            7
        );
        assert!(cache.get(&1).is_none());
    }

    #[test]
    fn delayed_old_epoch_invalidation_preserves_fresh_entries() {
        let cache = CalculationCache::<u32, u32>::new(8);
        let current_epoch = cache.generation.advance();
        assert_eq!(
            *cache.get_or_try_insert_with(1, |_| 4, || Ok(9)).unwrap(),
            9
        );

        cache.invalidate_before(current_epoch);

        assert_eq!(*cache.get(&1).unwrap(), 9);
    }

    #[test]
    fn recursive_same_key_is_rejected_instead_of_deadlocking() {
        let cache = CalculationCache::<u32, u32>::new(8);
        let result = cache.get_or_try_insert_with(
            1,
            |_| 4,
            || cache.get_or_try_insert_with(1, |_| 4, || Ok(2)).map(|v| *v),
        );
        assert!(matches!(result, Err(XllError::Internal { .. })));
        assert!(cache.get(&1).is_none());
    }

    #[test]
    fn reentrant_initialization_on_another_key_is_rejected() {
        let cache = CalculationCache::<u32, u32>::new(8);
        let result = cache.get_or_try_insert_with(
            1,
            |_| 4,
            || {
                cache
                    .get_or_try_insert_with(2, |_| 4, || Ok(20))
                    .map(|v| *v)
            },
        );
        assert!(matches!(result, Err(XllError::Internal { .. })));
        assert!(cache.get(&1).is_none());
        assert!(cache.get(&2).is_none());
    }

    #[test]
    fn initialization_of_a_different_cache_is_rejected_instead_of_deadlocking() {
        let first = CalculationCache::<u32, u32>::new(8);
        let second = CalculationCache::<u32, u32>::new(8);
        let result = first.get_or_try_insert_with(
            1,
            |_| 4,
            || {
                second
                    .get_or_try_insert_with(2, |_| 4, || Ok(20))
                    .map(|value| *value + 1)
            },
        );
        assert!(matches!(result, Err(XllError::Internal { .. })));
        assert!(first.get(&1).is_none());
        assert!(second.get(&2).is_none());
    }

    #[test]
    fn initializer_may_read_an_already_cached_value() {
        let cache = CalculationCache::<u32, u32>::new(8);
        cache.get_or_try_insert_with(2, |_| 4, || Ok(20)).unwrap();
        let result = cache
            .get_or_try_insert_with(1, |_| 4, || Ok(*cache.get(&2).unwrap() + 1))
            .unwrap();
        assert_eq!(*result, 21);
    }

    #[test]
    fn clear_drops_values_outside_the_cache_lock() {
        let cache = CalculationCache::<u32, u32>::new(8);
        cache.get_or_try_insert_with(1, |_| 8, || Ok(7)).unwrap();
        cache.clear();
        assert!(cache.get(&1).is_none());
    }

    #[test]
    fn eviction_drops_values_outside_the_cache_lock() {
        static DROPPED: AtomicUsize = AtomicUsize::new(0);
        struct TrackDrop(u32);
        impl Drop for TrackDrop {
            fn drop(&mut self) {
                DROPPED.fetch_add(1, Ordering::SeqCst);
            }
        }
        DROPPED.store(0, Ordering::SeqCst);
        let cache = CalculationCache::<u32, TrackDrop>::new(8);
        for key in [1, 2] {
            let value = cache
                .get_or_try_insert_with(key, |_| 8, || Ok(TrackDrop(key * 10)))
                .unwrap();
            assert_eq!(value.0, key * 10);
            drop(value);
        }
        cache.index.maintenance();
        assert_eq!(cache.len(), 1);
        assert_eq!(DROPPED.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn registry_keeps_typed_endpoints_independent() {
        enum First {}
        enum Second {}
        static FIRST: CacheEndpoint<First, u32, u32> =
            CacheEndpoint::<First, u32, u32>::new("FIRST");
        static SECOND: CacheEndpoint<Second, u32, String> =
            CacheEndpoint::<Second, u32, String>::new("SECOND");

        let registry = CacheRegistry::new(8);
        let first = registry.bind(&FIRST).unwrap();
        let second = registry.bind(&SECOND).unwrap();

        assert_eq!(*first.get_or_try_insert(1, |_| 4, || Ok(7)).unwrap(), 7);
        assert_eq!(
            second
                .get_or_try_insert(1, String::len, || Ok("seven".to_owned()))
                .unwrap()
                .as_str(),
            "seven"
        );

        let rebound_first = registry.bind(&FIRST).unwrap();
        let rebound_second = registry.bind(&SECOND).unwrap();
        assert_eq!(*rebound_first.get(&1).unwrap(), 7);
        assert_eq!(rebound_second.get(&1).unwrap().as_str(), "seven");
    }

    #[test]
    fn registry_differentiates_endpoints_by_marker_type() {
        enum Number {}
        enum Text {}
        let number = CacheEndpoint::<Number, u32, u32>::new("DUPLICATE");
        let text = CacheEndpoint::<Text, u32, String>::new("DUPLICATE");
        let registry = CacheRegistry::new(1024);

        assert_eq!(
            *registry
                .bind(&number)
                .unwrap()
                .get_or_try_insert(1, |_| 4, || Ok(7))
                .unwrap(),
            7
        );
        assert_eq!(
            registry
                .bind(&text)
                .unwrap()
                .get_or_try_insert(1, String::len, || Ok("seven".to_owned()))
                .unwrap()
                .as_str(),
            "seven"
        );
        assert_eq!(registry.endpoint_count(), 2);
    }

    #[test]
    fn registry_clear_invalidates_bound_endpoint_values() {
        enum FirstUse {}
        static ENDPOINT: CacheEndpoint<FirstUse, u32, u32> =
            CacheEndpoint::<FirstUse, u32, u32>::new("FIRST_USE");

        let registry = CacheRegistry::new(8);
        let endpoint = registry.bind(&ENDPOINT).unwrap();
        let (started_tx, started_rx) = std::sync::mpsc::sync_channel(1);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
        std::thread::scope(|s| {
            let worker = s.spawn(move || {
                let lease = endpoint
                    .get_or_try_insert(
                        1,
                        |_| 4,
                        || {
                            started_tx.send(()).unwrap();
                            release_rx.recv().unwrap();
                            Ok(7)
                        },
                    )
                    .unwrap();
                *lease
            });

            started_rx.recv().unwrap();
            registry.clear();
            release_tx.send(()).unwrap();

            assert_eq!(worker.join().unwrap(), 7);
        });

        assert_eq!(
            *registry
                .bind(&ENDPOINT)
                .unwrap()
                .get_or_try_insert(1, |_| 4, || Ok(9))
                .unwrap(),
            9
        );
    }

    #[test]
    fn clear_generation_isolation() {
        let cache = CalculationCache::<u32, &'static str>::new(1024);
        let (started_tx, started_rx) = std::sync::mpsc::channel();

        let cache_ref = &cache;
        std::thread::scope(|s| {
            let handle_a = s.spawn(move || {
                let lease = cache_ref
                    .get_or_try_insert_with(
                        1,
                        |_| 4,
                        || {
                            started_tx.send(()).unwrap();
                            std::thread::sleep(Duration::from_millis(50));
                            Ok("epoch_0_value")
                        },
                    )
                    .unwrap();
                *lease
            });

            // Wait until A is inside compute() at epoch 0
            started_rx.recv().unwrap();

            // Clear while A's computation is in-flight at epoch 0
            cache.clear();

            let handle_b = s.spawn(move || {
                let lease = cache_ref
                    .get_or_try_insert_with(1, |_| 4, || Ok("epoch_1_value"))
                    .unwrap();
                *lease
            });

            let val_a = handle_a.join().unwrap();
            let val_b = handle_b.join().unwrap();

            assert_eq!(val_a, "epoch_0_value");
            assert_eq!(val_b, "epoch_1_value");
        });
    }

    #[cfg(not(all(target_os = "windows", target_arch = "x86")))]
    #[test]
    fn loom_clear_never_leaves_a_stale_generation_visible() {
        use loom::sync::atomic::{AtomicU64 as LoomAtomicU64, Ordering as LoomOrdering};
        use loom::sync::{Arc as LoomArc, Mutex as LoomMutex};
        use loom::thread as loom_thread;

        struct LoomEpoch {
            epoch: LoomAtomicU64,
        }

        impl EpochAtomic for LoomEpoch {
            fn new(value: u64) -> Self {
                Self {
                    epoch: LoomAtomicU64::new(value),
                }
            }

            fn load(&self) -> u64 {
                self.epoch.load(LoomOrdering::SeqCst)
            }

            fn increment(&self) {
                self.epoch.fetch_add(1, LoomOrdering::SeqCst);
            }
        }

        loom::model(|| {
            let generation = LoomArc::new(CacheGeneration::<LoomEpoch>::new());
            let stored_epoch = LoomArc::new(LoomMutex::new(None));

            let initializer_generation = LoomArc::clone(&generation);
            let initializer_stored = LoomArc::clone(&stored_epoch);
            let initializer = loom_thread::spawn(move || {
                let snapshot = initializer_generation.snapshot();

                // Models Moka publishing the initialized entry before
                // CalculationCache performs its post-initialization epoch check.
                *initializer_stored.lock().unwrap() = Some(snapshot);
                initializer_generation.discard_if_stale(snapshot, || {
                    let mut stored = initializer_stored.lock().unwrap();
                    if *stored == Some(snapshot) {
                        *stored = None;
                    }
                });
            });

            let clearer_generation = LoomArc::clone(&generation);
            let clearer_stored = LoomArc::clone(&stored_epoch);
            let clearer = loom_thread::spawn(move || {
                clearer_generation.advance();
                *clearer_stored.lock().unwrap() = None;
            });

            initializer.join().unwrap();
            clearer.join().unwrap();

            let current = generation.snapshot();
            let stored = *stored_epoch.lock().unwrap();
            assert!(
                stored.is_none() || stored == Some(current),
                "stale stored epoch {stored:?}, current epoch {current}"
            );
        });
    }

    #[cfg(not(all(target_os = "windows", target_arch = "x86")))]
    #[test]
    fn loom_temporal_reclamation_protocol_exhaustive_verification() {
        use loom::sync::Arc;
        use loom::sync::atomic::{AtomicBool, AtomicPtr, AtomicUsize, Ordering};
        use loom::thread;
        use std::ptr;

        struct LoomCacheNode {
            pins: AtomicUsize,
            resident: AtomicBool,
            reclaimed: AtomicBool,
        }

        struct LoomLookupDomain {
            admissions: AtomicUsize,
            closed: AtomicBool,
        }

        impl LoomLookupDomain {
            fn new() -> Self {
                Self {
                    admissions: AtomicUsize::new(0),
                    closed: AtomicBool::new(false),
                }
            }

            fn enter(&self) -> bool {
                if self.closed.load(Ordering::Acquire) {
                    return false;
                }
                self.admissions.fetch_add(1, Ordering::AcqRel);
                if self.closed.load(Ordering::Acquire) {
                    self.admissions.fetch_sub(1, Ordering::AcqRel);
                    return false;
                }
                true
            }

            fn leave(&self) {
                self.admissions.fetch_sub(1, Ordering::AcqRel);
            }

            fn quiesce(&self) {
                while self.admissions.load(Ordering::Acquire) > 0 {
                    thread::yield_now();
                }
            }
        }

        loom::model(|| {
            let node = Box::into_raw(Box::new(LoomCacheNode {
                pins: AtomicUsize::new(1), // 1 for cache resident
                resident: AtomicBool::new(true),
                reclaimed: AtomicBool::new(false),
            }));

            let published = Arc::new(AtomicPtr::new(node));
            let domain = Arc::new(LoomLookupDomain::new());

            let reader_pub = Arc::clone(&published);
            let reader_dom = Arc::clone(&domain);
            let reader = thread::spawn(move || {
                // TR-LOOKUP-ENTER: Acquire admission domain first
                if reader_dom.enter() {
                    // TR-OBSERVE-POINTER: Pointer observed with Acquire while admission held
                    let ptr = reader_pub.load(Ordering::Acquire);
                    if !ptr.is_null() {
                        // SAFETY: ptr was non-null and allocated at start of model run.
                        let n = unsafe { &*ptr };
                        // TR-ACQUIRE-PIN: Increment pins with AcqRel while admission held
                        n.pins.fetch_add(1, Ordering::AcqRel);
                        if n.resident.load(Ordering::Acquire) {
                            // TR-LOOKUP-LEAVE: Leave admission
                            reader_dom.leave();

                            // TR-LEASE-1: Node MUST NOT be reclaimed while pin is held
                            assert!(
                                !n.reclaimed.load(Ordering::Acquire),
                                "UAF: node reclaimed while lease held"
                            );

                            // Simulate lease drop with AcqRel:
                            if n.pins.fetch_sub(1, Ordering::AcqRel) == 1 {
                                n.reclaimed.store(true, Ordering::Release);
                            }
                            return;
                        } else {
                            // Evicted concurrently
                            if n.pins.fetch_sub(1, Ordering::AcqRel) == 1 {
                                n.reclaimed.store(true, Ordering::Release);
                            }
                        }
                    }
                    reader_dom.leave();
                }
            });

            let evictor_pub = Arc::clone(&published);
            let evictor_dom = Arc::clone(&domain);
            let evictor = thread::spawn(move || {
                // TR-RETIRE-1: Evict pointer with AcqRel swap
                let ptr = evictor_pub.swap(ptr::null_mut(), Ordering::AcqRel);
                if !ptr.is_null() {
                    // SAFETY: ptr was non-null and allocated at start of model run.
                    let n = unsafe { &*ptr };
                    n.resident.store(false, Ordering::Release);
                    // Decrement resident pin with AcqRel
                    if n.pins.fetch_sub(1, Ordering::AcqRel) == 1 {
                        // TR-RECLAIM-1: Quiesce domain before reclaim
                        evictor_dom.quiesce();
                        n.reclaimed.store(true, Ordering::Release);
                    }
                }
            });

            reader.join().unwrap();
            evictor.join().unwrap();

            // After both threads finish, node must be reclaimed safely
            // SAFETY: node is valid until dropped below.
            let n = unsafe { &*node };
            assert!(
                n.reclaimed.load(Ordering::Acquire),
                "node should be reclaimed after reader and evictor finish"
            );
            assert_eq!(n.pins.load(Ordering::Acquire), 0);

            // SAFETY: node was allocated with Box::into_raw above and not dropped elsewhere.
            unsafe {
                drop(Box::from_raw(node));
            }
        });
    }

    #[cfg(not(all(target_os = "windows", target_arch = "x86")))]
    #[test]
    #[should_panic(expected = "UAF")]
    fn loom_detects_buggy_unprotected_pointer_observation_race() {
        use loom::sync::Arc;
        use loom::sync::atomic::{AtomicBool, AtomicPtr, AtomicUsize, Ordering};
        use loom::thread;
        use std::ptr;

        struct LoomCacheNode {
            pins: AtomicUsize,
            resident: AtomicBool,
            reclaimed: AtomicBool,
        }

        struct LoomLookupDomain {
            admissions: AtomicUsize,
        }

        impl LoomLookupDomain {
            fn new() -> Self {
                Self {
                    admissions: AtomicUsize::new(0),
                }
            }

            fn enter(&self) {
                self.admissions.fetch_add(1, Ordering::AcqRel);
            }

            fn leave(&self) {
                self.admissions.fetch_sub(1, Ordering::AcqRel);
            }

            fn quiesce(&self) {
                while self.admissions.load(Ordering::Acquire) > 0 {
                    thread::yield_now();
                }
            }
        }

        loom::model(|| {
            let node = Box::into_raw(Box::new(LoomCacheNode {
                pins: AtomicUsize::new(1),
                resident: AtomicBool::new(true),
                reclaimed: AtomicBool::new(false),
            }));

            let published = Arc::new(AtomicPtr::new(node));
            let domain = Arc::new(LoomLookupDomain::new());

            let reader_pub = Arc::clone(&published);
            let reader_dom = Arc::clone(&domain);
            let reader = thread::spawn(move || {
                // BUG: Pointer is observed BEFORE acquiring lookup admission permit!
                let ptr = reader_pub.load(Ordering::Acquire);
                if !ptr.is_null() {
                    reader_dom.enter();
                    // SAFETY: ptr was non-null and points to model node.
                    let n = unsafe { &*ptr };
                    assert!(
                        !n.reclaimed.load(Ordering::Acquire),
                        "UAF: pointer observed before admission was reclaimed by evictor!"
                    );
                    reader_dom.leave();
                }
            });

            let evictor_pub = Arc::clone(&published);
            let evictor_dom = Arc::clone(&domain);
            let evictor = thread::spawn(move || {
                let ptr = evictor_pub.swap(ptr::null_mut(), Ordering::AcqRel);
                if !ptr.is_null() {
                    // SAFETY: ptr was non-null and points to model node.
                    let n = unsafe { &*ptr };
                    n.resident.store(false, Ordering::Release);
                    if n.pins.fetch_sub(1, Ordering::AcqRel) == 1 {
                        // Admissions was 0 when reader hadn't entered yet!
                        evictor_dom.quiesce();
                        n.reclaimed.store(true, Ordering::Release);
                    }
                }
            });

            reader.join().unwrap();
            evictor.join().unwrap();

            // SAFETY: node was allocated with Box::into_raw above and not dropped elsewhere.
            unsafe {
                drop(Box::from_raw(node));
            }
        });
    }

    #[test]
    fn same_endpoint_bound_twice_shares_single_allocation() {
        enum Marker {}
        static ENDPOINT: CacheEndpoint<Marker, u32, u32> = CacheEndpoint::new("SAME_ENDPOINT");
        let registry = CacheRegistry::new(64);
        let a = registry.bind(&ENDPOINT).unwrap();
        let b = registry.bind(&ENDPOINT).unwrap();
        assert_eq!(a.cache, b.cache);
    }

    #[test]
    fn different_marker_types_create_distinct_storage_allocations() {
        enum MarkerA {}
        enum MarkerB {}
        static ENDPOINT_A: CacheEndpoint<MarkerA, u32, u32> = CacheEndpoint::new("SHARED_ID");
        static ENDPOINT_B: CacheEndpoint<MarkerB, u32, u32> = CacheEndpoint::new("SHARED_ID");
        let registry = CacheRegistry::new(64);
        let a = registry.bind(&ENDPOINT_A).unwrap();
        let b = registry.bind(&ENDPOINT_B).unwrap();
        assert_eq!(registry.endpoint_count(), 2);
        a.get_or_try_insert(1, |_| 1, || Ok(10)).unwrap();
        b.get_or_try_insert(1, |_| 1, || Ok(20)).unwrap();
        assert_eq!(*a.get(&1).unwrap(), 10);
        assert_eq!(*b.get(&1).unwrap(), 20);
    }

    #[test]
    fn bound_endpoint_remains_usable_across_registry_clear() {
        enum Marker {}
        static ENDPOINT: CacheEndpoint<Marker, u32, u32> = CacheEndpoint::new("SURVIVE_CLEAR");
        let registry = CacheRegistry::new(64);
        let endpoint = registry.bind(&ENDPOINT).unwrap();

        assert_eq!(
            *endpoint.get_or_try_insert(1, |_| 1, || Ok(100)).unwrap(),
            100
        );
        assert_eq!(*endpoint.get(&1).unwrap(), 100);

        registry.clear();

        // Old generation value is missed
        assert!(endpoint.get(&1).is_none());

        // New value can be inserted in the new generation
        assert_eq!(
            *endpoint.get_or_try_insert(1, |_| 1, || Ok(200)).unwrap(),
            200
        );
        assert_eq!(*endpoint.get(&1).unwrap(), 200);
    }

    #[test]
    fn active_lease_keeps_value_alive_across_clear() {
        use std::sync::atomic::AtomicBool;

        static DROPPED: AtomicBool = AtomicBool::new(false);
        struct TrackDrop;
        impl Drop for TrackDrop {
            fn drop(&mut self) {
                DROPPED.store(true, Ordering::SeqCst);
            }
        }

        DROPPED.store(false, Ordering::SeqCst);
        let cache = CalculationCache::<u32, TrackDrop>::new(1024);
        let lease = cache
            .get_or_try_insert_with(1, |_| 1, || Ok(TrackDrop))
            .unwrap();

        let (clear_started_tx, clear_started_rx) = std::sync::mpsc::channel();
        let (clear_done_tx, clear_done_rx) = std::sync::mpsc::channel();
        let cache_ref = &cache;

        std::thread::scope(|s| {
            s.spawn(move || {
                clear_started_tx.send(()).unwrap();
                cache_ref.clear();
                clear_done_tx.send(()).unwrap();
            });

            clear_started_rx.recv().unwrap();
            // Allow clear thread to run and enter readers.seal_and_wait()
            std::thread::sleep(Duration::from_millis(50));

            // Wait for clear() to complete
            clear_done_rx.recv().unwrap();

            // While lease is held, TrackDrop MUST NOT be dropped!
            assert!(!DROPPED.load(Ordering::SeqCst));

            // Dropping lease drops the node!
            drop(lease);

            assert!(DROPPED.load(Ordering::SeqCst));
        });
    }

    #[test]
    fn independent_keys_reclaim_without_head_of_line_blocking() {
        use std::sync::atomic::AtomicBool;

        static DROPPED_A: AtomicBool = AtomicBool::new(false);
        static DROPPED_B: AtomicBool = AtomicBool::new(false);
        struct TrackDrop(&'static AtomicBool);
        impl Drop for TrackDrop {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }

        DROPPED_A.store(false, Ordering::SeqCst);
        DROPPED_B.store(false, Ordering::SeqCst);
        let cache = CalculationCache::<u32, TrackDrop>::new(1024);
        let lease_a = cache
            .get_or_try_insert_with(1, |_| 1, || Ok(TrackDrop(&DROPPED_A)))
            .unwrap();
        let lease_b = cache
            .get_or_try_insert_with(2, |_| 1, || Ok(TrackDrop(&DROPPED_B)))
            .unwrap();
        drop(lease_b);

        cache.clear();

        // Key B had no lease, so it was dropped immediately upon clear()!
        assert!(DROPPED_B.load(Ordering::SeqCst));
        // Key A still has active lease_a, so it is NOT dropped!
        assert!(!DROPPED_A.load(Ordering::SeqCst));

        // Dropping lease_a drops Key A!
        drop(lease_a);
        assert!(DROPPED_A.load(Ordering::SeqCst));
    }

    #[test]
    fn hit_path_and_clear_race_safety() {
        let cache = CalculationCache::<u32, String>::new(1024);
        assert_eq!(
            *cache
                .get_or_try_insert_with(1, |_| 10, || Ok("initial".to_string()))
                .unwrap(),
            "initial"
        );

        let cache_ref = &cache;
        std::thread::scope(|s| {
            for i in 0..20 {
                let t1 = s.spawn(move || {
                    for j in 0..50 {
                        if let Ok(lease) = cache_ref.get_or_try_insert_with(
                            1,
                            |_| 10,
                            || Ok(format!("val_{i}_{j}")),
                        ) {
                            assert!(!lease.is_empty());
                        }
                    }
                });
                let t2 = s.spawn(move || {
                    for j in 0..50 {
                        if let Ok(lease) = cache_ref.get_or_try_insert_with(
                            1,
                            |_| 10,
                            || Ok(format!("val2_{i}_{j}")),
                        ) {
                            assert!(!lease.is_empty());
                        }
                    }
                });
                let t3 = s.spawn(move || {
                    cache_ref.clear();
                });
                t1.join().unwrap();
                t2.join().unwrap();
                t3.join().unwrap();
            }
        });
    }

    #[test]
    fn reentrant_clear_in_destructor_does_not_deadlock() {
        struct ReentrantClear {
            cache: Arc<CalculationCache<u32, ReentrantClear>>,
            cleared: Arc<AtomicBool>,
        }

        impl Drop for ReentrantClear {
            fn drop(&mut self) {
                // If clear_lock or transition lock were held during reclamation,
                // this reentrant clear() would deadlock.
                self.cache.clear();
                self.cleared.store(true, Ordering::SeqCst);
            }
        }

        let cleared = Arc::new(AtomicBool::new(false));
        let cache = Arc::new(CalculationCache::<u32, ReentrantClear>::new(10));
        let cache_clone = Arc::clone(&cache);
        let cleared_clone = Arc::clone(&cleared);

        drop(
            cache
                .get_or_try_insert_with(
                    1,
                    |_| 1,
                    move || {
                        Ok(ReentrantClear {
                            cache: cache_clone,
                            cleared: cleared_clone,
                        })
                    },
                )
                .unwrap(),
        );

        cache.clear();
        assert!(cleared.load(Ordering::SeqCst));
    }

    #[test]
    fn panicking_destructor_is_contained_during_lease_drop() {
        struct PanickingDrop {
            _unused: u32,
        }

        impl Drop for PanickingDrop {
            fn drop(&mut self) {
                panic!("deliberate panic inside V::drop");
            }
        }

        let cache = CalculationCache::<u32, PanickingDrop>::new(10);
        let lease = cache
            .get_or_try_insert_with(1, |_| 1, || Ok(PanickingDrop { _unused: 42 }))
            .unwrap();

        // Evict/clear the cache entry while lease is held
        cache.clear();

        // Dropping the lease triggers retirement and reclamation of PanickingDrop.
        // The panic in PanickingDrop::drop must be caught and must not unwind out of lease.drop().
        drop(lease);
    }

    #[test]
    fn cache_node_pin_overflow_is_prevented() {
        let node = CacheNode {
            value: Box::new(42u32),
            pins: AtomicUsize::new(usize::MAX),
            resident: AtomicBool::new(true),
            weight: 1,
            generation: 1,
            domain: NonNull::dangling(),
        };
        assert_eq!(node.try_acquire_pin(), Err(PinOverflow));
        assert_eq!(node.pins.load(Ordering::SeqCst), usize::MAX);
    }

    #[test]
    fn final_pin_release_cannot_be_resurrected() {
        let node = CacheNode {
            value: Box::new(42_u32),
            pins: AtomicUsize::new(1),
            resident: AtomicBool::new(false),
            weight: 1,
            generation: 0,
            domain: NonNull::dangling(),
        };
        assert!(node.release_pin());
        assert_eq!(node.try_acquire_pin(), Ok(false));
        assert_eq!(node.pins.load(Ordering::Acquire), 0);
    }

    #[test]
    fn ordinary_cache_churn_reclaims_without_observer_maintenance() {
        struct DropProbe(Arc<AtomicUsize>);
        impl Drop for DropProbe {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::Relaxed);
            }
        }
        let drops = Arc::new(AtomicUsize::new(0));
        let cache = CalculationCache::new(4);
        for key in 0..4096 {
            drop(
                cache
                    .get_or_try_insert_with(key, |_| 1, || Ok(DropProbe(Arc::clone(&drops))))
                    .unwrap(),
            );
            // These are observations only: neither accessor can flush Moka or
            // trigger a grace period and hide unbounded reclamation debt.
            let stats = cache.reclamation_stats();
            assert!(stats.pending_nodes < RECLAIM_BACKPRESSURE_NODES);
            assert!(key as usize + 1 - drops.load(Ordering::Relaxed) < 128);
        }
        assert!(drops.load(Ordering::Relaxed) > 4000);
        assert!(cache.reclamation_stats().reclaimed_nodes > 4000);
    }

    #[test]
    fn ordinary_read_services_an_eviction_request() {
        struct DropProbe(Arc<AtomicUsize>);
        impl Drop for DropProbe {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::Relaxed);
            }
        }
        let drops = Arc::new(AtomicUsize::new(0));
        let cache = CalculationCache::new(4);
        drop(
            cache
                .get_or_try_insert_with(1, |_| 3, || Ok(DropProbe(Arc::clone(&drops))))
                .unwrap(),
        );
        cache.index.clear();
        cache.index.maintenance();
        let stats = cache.reclamation_stats();
        assert_eq!(stats.pending_nodes, 1);
        assert_eq!(stats.pending_weight, 3);
        assert_eq!(cache.reclamation_stats(), stats);
        assert_eq!(drops.load(Ordering::Relaxed), 0);
        assert!(cache.get(&2).is_none());
        assert_eq!(drops.load(Ordering::Relaxed), 1);
        assert_eq!(cache.reclamation_stats().pending_nodes, 0);
    }

    #[test]
    fn zero_budget_values_are_reclaimed_when_their_leases_end() {
        struct DropProbe(Arc<AtomicUsize>);
        impl Drop for DropProbe {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::Relaxed);
            }
        }
        let drops = Arc::new(AtomicUsize::new(0));
        let cache = CalculationCache::new(0);
        for key in 0..128 {
            let lease = cache
                .get_or_try_insert_with(key, |_| 1, || Ok(DropProbe(Arc::clone(&drops))))
                .unwrap();
            assert!(cache.get(&key).is_none());
            assert_eq!(drops.load(Ordering::Relaxed), key as usize);
            drop(lease);
            assert_eq!(drops.load(Ordering::Relaxed), key as usize + 1);
        }
        assert_eq!(cache.reclamation_stats().pending_nodes, 0);
    }

    #[test]
    fn zero_budget_initializer_reclaims_deferred_leases_after_success_or_failure() {
        struct DropProbe(Arc<AtomicUsize>);
        impl Drop for DropProbe {
            fn drop(&mut self) {
                assert_eq!(ACTIVE_CACHE_INITIALIZATION_DEPTH.get(), 0);
                self.0.fetch_add(1, Ordering::Relaxed);
            }
        }

        for succeeds in [false, true] {
            let drops = Arc::new(AtomicUsize::new(0));
            let cache = CalculationCache::new(0);
            let previous = cache
                .get_or_try_insert_with(1, |_| 1, || Ok(DropProbe(Arc::clone(&drops))))
                .unwrap();
            let initialized = cache.get_or_try_insert_with(
                2,
                |_| 1,
                || {
                    drop(previous);
                    assert_eq!(drops.load(Ordering::Relaxed), 0);
                    assert_eq!(cache.reclamation_stats().pending_nodes, 1);
                    if succeeds {
                        Ok(DropProbe(Arc::clone(&drops)))
                    } else {
                        Err(XllError::Closing)
                    }
                },
            );

            assert_eq!(initialized.is_ok(), succeeds);
            assert_eq!(drops.load(Ordering::Relaxed), 1);
            assert_eq!(cache.reclamation_stats().pending_nodes, 0);
            drop(initialized);
            assert_eq!(drops.load(Ordering::Relaxed), 1 + usize::from(succeeds));
        }
    }

    #[test]
    fn panicking_initializer_reclaims_deferred_leases_and_preserves_original_panic() {
        struct DropProbe {
            drops: Arc<AtomicUsize>,
            payload_drops: Arc<AtomicUsize>,
        }
        impl Drop for DropProbe {
            fn drop(&mut self) {
                assert_eq!(ACTIVE_CACHE_INITIALIZATION_DEPTH.get(), 0);
                self.drops.fetch_add(1, Ordering::Relaxed);
                std::panic::panic_any(crate::panic_boundary::tests::PanickingPayload(Arc::clone(
                    &self.payload_drops,
                )));
            }
        }

        for capacity in [0, 8] {
            let drops = Arc::new(AtomicUsize::new(0));
            let payload_drops = Arc::new(AtomicUsize::new(0));
            let cache = CalculationCache::new(capacity);
            let previous = cache
                .get_or_try_insert_with(
                    1,
                    |_| 1,
                    || {
                        Ok(DropProbe {
                            drops: Arc::clone(&drops),
                            payload_drops: Arc::clone(&payload_drops),
                        })
                    },
                )
                .unwrap();
            cache.clear();

            let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
                let _ = cache.get_or_try_insert_with(
                    2,
                    |_| 1,
                    || {
                        drop(previous);
                        assert_eq!(drops.load(Ordering::Relaxed), 0);
                        assert_eq!(cache.reclamation_stats().pending_nodes, 1);
                        panic!("original initializer panic");
                    },
                );
            }));
            assert_eq!(
                result.as_ref().unwrap_err().downcast_ref::<&str>(),
                Some(&"original initializer panic")
            );
            assert!(crate::panic_boundary::contain_panic(result).is_err());
            assert_eq!(drops.load(Ordering::Relaxed), 1);
            assert_eq!(payload_drops.load(Ordering::Relaxed), 0);
            assert_eq!(cache.reclamation_stats().pending_nodes, 0);
        }
    }

    #[test]
    fn panicking_initializer_reclaims_after_singleflight_unlock() {
        struct DropAction {
            cache: std::sync::Weak<CalculationCache<u32, DropProbe>>,
            completed: Arc<AtomicBool>,
            workers: Arc<Mutex<Vec<std::thread::JoinHandle<()>>>>,
        }
        struct DropProbe(Option<DropAction>);
        impl Drop for DropProbe {
            fn drop(&mut self) {
                let Some(action) = self.0.take() else {
                    return;
                };
                assert_eq!(ACTIVE_CACHE_INITIALIZATION_DEPTH.get(), 0);
                let cache = action.cache.upgrade().unwrap();
                let (finished_tx, finished_rx) = mpsc::channel();
                let worker = std::thread::spawn(move || {
                    let initialized =
                        cache.get_or_try_insert_with(2, |_| 1, || Ok(DropProbe(None)));
                    let _ = finished_tx.send(initialized.is_ok());
                });
                action.workers.lock().push(worker);
                // Bound this wait so a regression releases the original
                // singleflight and lets the worker finish before we assert.
                action.completed.store(
                    finished_rx.recv_timeout(Duration::from_secs(1)) == Ok(true),
                    Ordering::Release,
                );
            }
        }

        let cache = Arc::new(CalculationCache::new(8));
        let completed = Arc::new(AtomicBool::new(false));
        let workers = Arc::new(Mutex::new(Vec::new()));
        let previous = cache
            .get_or_try_insert_with(
                1,
                |_| 1,
                || {
                    Ok(DropProbe(Some(DropAction {
                        cache: Arc::downgrade(&cache),
                        completed: Arc::clone(&completed),
                        workers: Arc::clone(&workers),
                    })))
                },
            )
            .unwrap();
        cache.clear();
        assert!(
            catch_no_unwind(AssertUnwindSafe(|| {
                let _ = cache.get_or_try_insert_with(
                    2,
                    |_| 1,
                    || {
                        drop(previous);
                        panic!("initializer panic before singleflight unlock");
                    },
                );
            }))
            .is_err()
        );
        for worker in std::mem::take(&mut *workers.lock()) {
            assert!(crate::panic_boundary::contain_panic(worker.join()).is_ok());
        }
        assert!(completed.load(Ordering::Acquire));
        assert_eq!(cache.reclamation_stats().pending_nodes, 0);
    }

    #[test]
    fn nested_lookup_panic_defers_reclamation_until_outer_initializer_finishes() {
        #[derive(Clone, Eq, PartialEq)]
        struct Key {
            value: u32,
            panic_on_hash: bool,
        }
        impl Hash for Key {
            fn hash<H: Hasher>(&self, state: &mut H) {
                assert!(!self.panic_on_hash, "nested lookup panic");
                self.value.hash(state);
            }
        }
        struct DropProbe(Arc<AtomicUsize>);
        impl Drop for DropProbe {
            fn drop(&mut self) {
                assert_eq!(ACTIVE_CACHE_INITIALIZATION_DEPTH.get(), 0);
                self.0.fetch_add(1, Ordering::Relaxed);
            }
        }

        let key = |value| Key {
            value,
            panic_on_hash: false,
        };
        let drops = Arc::new(AtomicUsize::new(0));
        let cache = CalculationCache::new(8);
        let previous = cache
            .get_or_try_insert_with(key(1), |_| 1, || Ok(DropProbe(Arc::clone(&drops))))
            .unwrap();
        cache.clear();
        let initialized = cache
            .get_or_try_insert_with(
                key(2),
                |_| 1,
                || {
                    drop(previous);
                    assert!(
                        catch_no_unwind(AssertUnwindSafe(|| {
                            let _ = cache.get_or_try_insert_with(
                                Key {
                                    value: 3,
                                    panic_on_hash: true,
                                },
                                |_| 1,
                                || panic!("nested initializer must not run"),
                            );
                        }))
                        .is_err()
                    );
                    assert_eq!(drops.load(Ordering::Relaxed), 0);
                    assert_eq!(cache.reclamation_stats().pending_nodes, 1);
                    Ok(DropProbe(Arc::clone(&drops)))
                },
            )
            .unwrap();
        assert_eq!(drops.load(Ordering::Relaxed), 1);
        assert_eq!(cache.reclamation_stats().pending_nodes, 0);
        drop(initialized);
    }

    #[test]
    fn cache_drop_finishes_time_limited_eviction_passes() {
        struct DropProbe(Arc<AtomicUsize>);
        impl Drop for DropProbe {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::Relaxed);
            }
        }
        let drops = Arc::new(AtomicUsize::new(0));
        let pause = Arc::new(AtomicBool::new(false));
        let pause_once = Arc::clone(&pause);
        let cache = CalculationCache::new_with_eviction_hook(2048, move || {
            if pause_once.swap(false, Ordering::Relaxed) {
                // Moka time-limits a maintenance pass at 100 ms when a
                // listener is installed. Force it to stop after one batch.
                std::thread::sleep(Duration::from_millis(150));
            }
        });
        for key in 0..1024 {
            drop(
                cache
                    .get_or_try_insert_with(key, |_| 1, || Ok(DropProbe(Arc::clone(&drops))))
                    .unwrap(),
            );
        }
        assert_eq!(drops.load(Ordering::Relaxed), 0);
        pause.store(true, Ordering::Relaxed);
        drop(cache);
        assert!(!pause.load(Ordering::Relaxed));
        assert_eq!(drops.load(Ordering::Relaxed), 1024);
    }

    #[test]
    fn pending_weight_preserves_totals_above_the_32_bit_limit() {
        let domain = CacheLookupDomain::new();
        // Sentinel pointers never reach a reclaimer in this queue-only test.
        domain.enqueue_reclaim(std::ptr::null_mut(), u32::MAX);
        domain.enqueue_reclaim(std::ptr::null_mut(), u32::MAX);
        assert_eq!(domain.stats().pending_weight, 2 * u64::from(u32::MAX));
        let entries = domain.quiesce_and_drain();
        assert_eq!(entries.len(), 2);
        assert_eq!(domain.stats().pending_weight, 0);
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn oversized_weight_is_checked_before_moka_saturation() {
        let cache = CalculationCache::<u32, u32>::new(u32::MAX as usize);
        let value = cache
            .get_or_try_insert_with(1, |_| u32::MAX as usize + 1, || Ok(7))
            .unwrap();
        assert_eq!(*value, 7);
        assert!(cache.get(&1).is_none());
        drop(value);
    }

    #[test]
    fn reclamation_continues_after_a_panic_with_a_panicking_payload() {
        struct DropProbe {
            values_dropped: Arc<AtomicUsize>,
            payloads_dropped: Arc<AtomicUsize>,
        }
        impl Drop for DropProbe {
            fn drop(&mut self) {
                self.values_dropped.fetch_add(1, Ordering::Relaxed);
                std::panic::panic_any(crate::panic_boundary::tests::PanickingPayload(Arc::clone(
                    &self.payloads_dropped,
                )));
            }
        }
        let values_dropped = Arc::new(AtomicUsize::new(0));
        let payloads_dropped = Arc::new(AtomicUsize::new(0));
        let cache = CalculationCache::new(8);
        for key in 0..3 {
            drop(
                cache
                    .get_or_try_insert_with(
                        key,
                        |_| 1,
                        || {
                            Ok(DropProbe {
                                values_dropped: Arc::clone(&values_dropped),
                                payloads_dropped: Arc::clone(&payloads_dropped),
                            })
                        },
                    )
                    .unwrap(),
            );
        }
        cache.clear();
        assert_eq!(values_dropped.load(Ordering::Relaxed), 3);
        assert_eq!(payloads_dropped.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn initializer_defers_final_lease_and_observer_reclamation() {
        #[derive(Debug)]
        struct DropProbe(Arc<AtomicUsize>);
        impl Drop for DropProbe {
            fn drop(&mut self) {
                assert_eq!(ACTIVE_CACHE_INITIALIZATION_DEPTH.get(), 0);
                self.0.fetch_add(1, Ordering::Relaxed);
            }
        }
        let drops = Arc::new(AtomicUsize::new(0));
        let cache = CalculationCache::new(8);
        let final_lease = cache
            .get_or_try_insert_with(1, |_| 1, || Ok(DropProbe(Arc::clone(&drops))))
            .unwrap();
        drop(
            cache
                .get_or_try_insert_with(2, |_| 1, || Ok(DropProbe(Arc::clone(&drops))))
                .unwrap(),
        );
        cache
            .get_or_try_insert_with(
                3,
                |_| 1,
                || {
                    cache.clear();
                    drop(final_lease);
                    let _ = (cache.len(), cache.used_weight());
                    assert_eq!(drops.load(Ordering::Relaxed), 0);
                    assert_eq!(cache.reclamation_stats().pending_nodes, 2);
                    Err(XllError::Closing)
                },
            )
            .unwrap_err();
        // Maintenance must also run after a failed outer initializer.
        assert_eq!(drops.load(Ordering::Relaxed), 2);
        assert_eq!(cache.reclamation_stats().pending_nodes, 0);
    }

    #[test]
    fn registry_clear_allows_destructor_to_bind_new_endpoints() {
        enum Marker {}
        enum NewMarker {}
        static EXISTING: CacheEndpoint<Marker, u32, ReentrantBind> = CacheEndpoint::new("existing");
        static NEW: CacheEndpoint<NewMarker, u32, u32> = CacheEndpoint::new("new");
        struct ReentrantBind {
            registry: std::sync::Weak<CacheRegistry>,
            completed: Arc<AtomicBool>,
        }
        impl Drop for ReentrantBind {
            fn drop(&mut self) {
                let registry = self.registry.upgrade().unwrap();
                // Detect the lock regression without hanging the test suite.
                assert!(registry.caches.try_write().is_some());
                let endpoint = registry.bind(&NEW).unwrap();
                drop(endpoint.get_or_try_insert(2, |_| 1, || Ok(9)).unwrap());
                self.completed.store(true, Ordering::Release);
            }
        }
        let registry = Arc::new(CacheRegistry::new(16));
        let completed = Arc::new(AtomicBool::new(false));
        let endpoint = registry.bind(&EXISTING).unwrap();
        drop(
            endpoint
                .get_or_try_insert(
                    1,
                    |_| 1,
                    || {
                        Ok(ReentrantBind {
                            registry: Arc::downgrade(&registry),
                            completed: Arc::clone(&completed),
                        })
                    },
                )
                .unwrap(),
        );
        registry.clear();
        assert!(completed.load(Ordering::Acquire));
        assert_eq!(registry.endpoint_count(), 2);
        assert_eq!(*registry.bind(&NEW).unwrap().get(&2).unwrap(), 9);
    }
}

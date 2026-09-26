use crate::error::InputError;
use crate::panic_boundary::catch_no_unwind;
use crate::{XllError, XllResult};
#[cfg(all(test, feature = "bench-internals"))]
mod backend_tests;
mod node_layout;
mod pin_transitions;
#[cfg(test)]
mod reentrancy_tests;
use node_layout::verus;
mod resident_index;
use crate::sync::{Condvar, Mutex, RwLock};
use resident_index::{ResidentEntry, ResidentIndex};
use rustc_hash::{FxHashMap, FxHashSet};
use smallvec::SmallVec;
use std::any::{Any, TypeId};
use std::borrow::Borrow;
use std::cell::Cell;
use std::hash::{Hash, Hasher};
use std::marker::PhantomData;
use std::panic::AssertUnwindSafe;
use std::ptr::NonNull;
#[cfg(feature = "bench-internals")]
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::Instant;
use triomphe::Arc;
use xlfn_kernel::drain_gate::DEFAULT_STRIPE_COUNT;
use xlfn_kernel::rotating_read_domain::{
    ClosedDomain, DrainedGeneration, GenerationIndex, RotatingReadPermit, RotatingRetirementDomain,
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

pub struct CacheEndpoint<K, V, Marker = ()> {
    id: &'static str,
    _key: PhantomData<fn() -> K>,
    _value: PhantomData<fn() -> V>,
    _marker: PhantomData<fn() -> Marker>,
}

impl<K, V, Marker> Clone for CacheEndpoint<K, V, Marker> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<K, V, Marker> Copy for CacheEndpoint<K, V, Marker> {}

impl<K, V, Marker> std::fmt::Debug for CacheEndpoint<K, V, Marker> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CacheEndpoint")
            .field("id", &self.id)
            .finish()
    }
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
        node_layout::value!(unsafe { self.node.as_ref() })
    }
}

/// Consumes the final-pin retirement entry after all observations have ended.
/// The caller must establish a matching grace period, or that the node was never
/// published. Ownership of the entry alone does not establish quiescence.
unsafe fn reclaim_cache_node<V>(entry: ReclaimEntry<V>) {
    let ReclaimEntry { pointer: ptr, .. } = entry;
    // SAFETY: the caller establishes quiescence; consuming entry transfers the
    // unique retirement ownership into Box without copying the inline payload.
    let node = unsafe { Box::from_raw(ptr) };
    // Drop in place: moving an inline, potentially large V onto the stack
    // just to catch its destructor would add copies and risk stack overflow.
    // Box's drop glue frees the node even if V's destructor unwinds. All
    // framework locks and the grace-period callback have already been left.
    if catch_no_unwind(AssertUnwindSafe(|| drop(node))).is_err() {
        let error = XllError::Panic;
        crate::diagnostics::report_no_unwind("calculation cache value final drop", &error);
    }
}

fn reclaim_cache_entries<V>(entries: ReclaimEntries<'_, V>) {
    for entry in entries.entries {
        // SAFETY: [TR-RECLAIM-1] entry.pointer points to an allocated CacheNode<V> whose grace period has ended.
        unsafe {
            reclaim_cache_node::<V>(entry);
        }
    }
}

/// Approximate, side-effect-free reclamation counters. Weights use the
/// caller's estimates, not allocator or process memory measurements.
#[cfg(any(test, feature = "bench-internals"))]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CacheReclamationStats {
    /// Evicted nodes without lease pins, awaiting their grace period.
    pub pending_nodes: usize,
    /// Sum of their caller-supplied weights. Saturates at `u64::MAX` until
    /// both retirement queues are observed empty under their locks.
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

// Retirement requests maintenance on ordinary reads, with a cheap zero-debt
// fast path. Resident-policy maintenance belongs to the index backend.
const RECLAIM_BACKPRESSURE_NODES: usize = 256;

unsafe fn release_node_pin<V>(node_ptr: NonNull<CacheNode<V>>) {
    // SAFETY: node_ptr points to an allocated CacheNode whose pin is being released.
    let node = unsafe { node_ptr.as_ref() };
    // SAFETY: the caller transfers one live pin to the retirement decision.
    if let Some(entry) = unsafe { ReclaimEntry::release_pin(node_ptr) } {
        if node.published {
            // SAFETY: [TR-RECLAIM-1] The last pin was dropped on a retired (non-resident) node.
            let domain = unsafe { node.domain.as_ref() };
            domain.enqueue_reclaim(entry);
            let retired = domain.quiesce_and_drain();
            reclaim_cache_entries::<V>(retired);
        } else if ACTIVE_CACHE_INITIALIZATION_DEPTH.get() == 0 {
            // SAFETY: A never-published node was never reachable through an index snapshot.
            // Its final pin release can immediately destroy the node.
            unsafe { reclaim_cache_node::<V>(entry) };
        } else {
            // Initializer-local drops still enter deferred reclamation so user destructors
            // never run inside the cache initialization guard.
            // SAFETY: node.domain is a valid pointer to CacheLookupDomain.
            let domain = unsafe { node.domain.as_ref() };
            domain.enqueue_reclaim(entry);
            let retired = domain.quiesce_and_drain();
            reclaim_cache_entries::<V>(retired);
        }
    }
}

impl<V> CacheLease<'_, V> {
    /// Releases the active pin capability without relying on implicit Drop.
    ///
    /// Formal theorem [TR-LEASE-1], [TR-RECLAIM-1]: releasing this pin capability
    /// decrements the node's pin count and enqueues for reclamation only if this is the final pin.
    #[inline]
    pub(crate) unsafe fn release_inner(&mut self) {
        // SAFETY: [TR-LEASE-1] self.node remains valid because a pin capability is held by this lease.
        unsafe { release_node_pin(self.node) };
    }
}

impl<V> Drop for CacheLease<'_, V> {
    #[inline]
    fn drop(&mut self) {
        // Rule 4: Drop is a thin wrapper over release_inner.
        // SAFETY: [TR-LEASE-1] self.node remains valid because a pin capability is held by this lease.
        unsafe { self.release_inner() };
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

        Some(node_layout::value!(node))
    }
}

impl<K, V, Marker> CacheEndpoint<K, V, Marker> {
    #[must_use]
    pub const fn new(id: &'static str) -> Self {
        Self {
            id,
            _key: PhantomData,
            _value: PhantomData,
            _marker: PhantomData,
        }
    }

    #[must_use]
    pub const fn id(&self) -> &'static str {
        self.id
    }
}

impl<K: 'static, V: 'static, Marker: 'static> CacheEndpoint<K, V, Marker> {
    #[must_use]
    pub(crate) fn key(&self) -> (TypeId, &'static str) {
        (TypeId::of::<(Marker, K, V)>(), self.id)
    }
}

impl<K, V, Marker> CacheEndpoint<K, V, Marker>
where
    Marker: 'static,
    K: Clone + Eq + Hash + Send + Sync + 'static,
    V: Send + Sync + 'static,
{
    pub fn get_or_try_insert<'a, F, W>(
        &self,
        registry: &'a CacheRegistry,
        key: K,
        weight: W,
        compute: F,
    ) -> XllResult<CacheLease<'a, V>>
    where
        F: FnOnce() -> XllResult<V>,
        W: FnOnce(&V) -> usize,
    {
        registry.get_or_try_insert(self, key, weight, compute)
    }

    pub fn get<'a>(
        &self,
        registry: &'a CacheRegistry,
        key: &K,
    ) -> XllResult<Option<CacheLease<'a, V>>> {
        registry.get(self, key)
    }

    /// Opens a benchmark-only scoped read region for repeated cache hits.
    #[cfg(feature = "bench-internals")]
    pub fn read_scope<'a>(
        &self,
        registry: &'a CacheRegistry,
    ) -> XllResult<CacheReadScope<'a, K, V>> {
        registry.read_scope(self)
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

type CacheMap = FxHashMap<(TypeId, &'static str), CacheEntry>;

pub struct CacheRegistry {
    weight_budget_per_endpoint: usize,
    caches: RwLock<CacheMap>,
}

impl CacheRegistry {
    #[must_use]
    pub fn new(weight_budget_per_endpoint: usize) -> Self {
        Self {
            weight_budget_per_endpoint,
            caches: RwLock::new(FxHashMap::default()),
        }
    }

    fn resolve_cache<K, V, Marker>(
        &self,
        endpoint: &CacheEndpoint<K, V, Marker>,
    ) -> XllResult<NonNull<StoredCache<Marker, K, V>>>
    where
        Marker: 'static,
        K: Clone + Eq + Hash + Send + Sync + 'static,
        V: Send + Sync + 'static,
    {
        let cache_key = endpoint.key();
        let caches = self.caches.read();
        if let Some(entry) = caches.get(&cache_key) {
            Self::downcast_cache::<Marker, K, V>(entry)
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
            Self::downcast_cache::<Marker, K, V>(entry)
        }
    }

    pub fn get_or_try_insert<'a, K, V, Marker, F, W>(
        &'a self,
        endpoint: &CacheEndpoint<K, V, Marker>,
        key: K,
        weight: W,
        compute: F,
    ) -> XllResult<CacheLease<'a, V>>
    where
        Marker: 'static,
        K: Clone + Eq + Hash + Send + Sync + 'static,
        V: Send + Sync + 'static,
        F: FnOnce() -> XllResult<V>,
        W: FnOnce(&V) -> usize,
    {
        let cache = self.resolve_cache(endpoint)?;
        // SAFETY: self.caches retains the boxed cache for 'a (&'a self).
        let stored: &'a StoredCache<Marker, K, V> = unsafe { cache.as_ref() };
        stored.cache.get_or_try_insert_with(key, weight, compute)
    }

    pub fn get<'a, K, V, Marker>(
        &'a self,
        endpoint: &CacheEndpoint<K, V, Marker>,
        key: &K,
    ) -> XllResult<Option<CacheLease<'a, V>>>
    where
        Marker: 'static,
        K: Clone + Eq + Hash + Send + Sync + 'static,
        V: Send + Sync + 'static,
    {
        let cache = self.resolve_cache(endpoint)?;
        // SAFETY: self.caches retains the boxed cache for 'a (&'a self).
        let stored: &'a StoredCache<Marker, K, V> = unsafe { cache.as_ref() };
        Ok(stored.cache.get(key))
    }

    /// Opens a benchmark-only scoped read region for repeated cache hits.
    #[cfg(feature = "bench-internals")]
    pub fn read_scope<'a, K, V, Marker>(
        &'a self,
        endpoint: &CacheEndpoint<K, V, Marker>,
    ) -> XllResult<CacheReadScope<'a, K, V>>
    where
        Marker: 'static,
        K: Clone + Eq + Hash + Send + Sync + 'static,
        V: Send + Sync + 'static,
    {
        let cache = self.resolve_cache(endpoint)?;
        // SAFETY: self.caches retains the boxed cache for 'a (&'a self).
        let stored: &'a StoredCache<Marker, K, V> = unsafe { cache.as_ref() };
        stored.cache.read_scope()
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

    #[cfg(test)]
    #[must_use]
    pub(crate) fn endpoint_count(&self) -> usize {
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
    // SAFETY: the index transfers its one residency pin exactly once.
    if let Some(entry) = unsafe { ReclaimEntry::release_pin(node_ptr.0) } {
        // SAFETY: the cache retains its domain until every index entry retires.
        let domain = unsafe { node.domain.as_ref() };
        domain.enqueue_reclaim(entry);
    }
}

node_layout::declare_node! {
    struct CacheNode<V> {
        pins: AtomicUsize,
        resident: AtomicBool,
        domain: NonNull<CacheLookupDomain<V>>,
    }
}

// SAFETY: The inline payload is Send if V: Send; the cache owns the domain.
unsafe impl<V: Send> Send for CacheNode<V> {}
// SAFETY: The inline payload is Sync if V: Sync; node metadata is atomic or immutable.
unsafe impl<V: Sync> Sync for CacheNode<V> {}

#[derive(Debug, PartialEq, Eq)]
struct PinOverflow;

impl<V> CacheNode<V> {
    #[inline]
    fn try_acquire_pin(&self) -> Result<bool, PinOverflow> {
        self.acquire_pin_with_ordering(Ordering::Relaxed)
    }

    /// Acquires a resident/flight anchor while the creator still owns a pin.
    /// Flight publication can race index readers, so this increment must use
    /// the same overflow check as reader pins rather than an unchecked add.
    #[inline]
    fn acquire_anchor_pin(&self) {
        if self.acquire_pin_with_ordering(Ordering::Release) != Ok(true) {
            xlfn_kernel::invariant::fail_stop();
        }
    }

    #[inline]
    fn acquire_pin_with_ordering(&self, success: Ordering) -> Result<bool, PinOverflow> {
        // Lookup admission retains the allocation, and the resident index
        // publishes its initialized value. Pins only extend that lifetime.
        use pin_transitions::{Acquire, pin_retry_expr};
        pin_transitions::acquire_retry!(raw, next;
            load = self.pins.load(Ordering::Relaxed),
            classify = pin_transitions::acquire(raw as _),
            attempt = self.pins.compare_exchange_weak(raw, next as usize, success, Ordering::Relaxed),
            success = Ok(true), zero = Ok(false), overflow = Err(PinOverflow);
        )
    }

    #[inline]
    fn release_pin(&self) -> bool {
        use pin_transitions::{Release, pin_retry_expr};
        // Acquire every earlier holder's accesses through the release sequence
        // before publishing the final pin's node to the reclamation queue.
        pin_transitions::release_pin!(previous;
            decrement = self.pins.fetch_sub(1, Ordering::Release),
            classify = pin_transitions::release(previous as _),
            fence = std::sync::atomic::fence(Ordering::Acquire),
            last = true, pinned = false,
            fail_stop = xlfn_kernel::invariant::fail_stop(),
        )
    }
}

enum FlightState<V> {
    Pending,
    Finished(Result<(NodePtr<V>, u64), Arc<XllError>>),
    Retry,
}

struct Flight<K, V> {
    key: VersionedKey<K>,
    state: Mutex<FlightState<V>>,
    changed: Condvar,
}

impl<K, V> Flight<K, V> {
    fn new(key: VersionedKey<K>) -> Self {
        Self {
            key,
            state: Mutex::new(FlightState::Pending),
            changed: Condvar::new(),
        }
    }
}

impl<K, V> Drop for Flight<K, V> {
    fn drop(&mut self) {
        if let FlightState::Finished(Ok((node_ptr, _))) = &*self.state.get_mut() {
            // SAFETY: node_ptr points to a valid CacheNode allocated during this flight.
            unsafe { release_node_pin(node_ptr.0) };
        }
    }
}

struct FlightHandle<K, V>(Arc<Flight<K, V>>);

impl<K, V> Clone for FlightHandle<K, V> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl<K: Hash, V> Hash for FlightHandle<K, V> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.key.hash(state);
    }
}

impl<K: Eq, V> PartialEq for FlightHandle<K, V> {
    fn eq(&self, other: &Self) -> bool {
        self.0.key == other.0.key
    }
}

impl<K: Eq, V> Eq for FlightHandle<K, V> {}

impl<K, V> Borrow<VersionedKey<K>> for FlightHandle<K, V> {
    fn borrow(&self) -> &VersionedKey<K> {
        &self.0.key
    }
}

type FlightSet<K, V> = FxHashSet<FlightHandle<K, V>>;

struct LeaderGuard<'a, K: Clone + Eq + Hash, V> {
    cache: &'a CalculationCache<K, V>,
    flight: &'a Arc<Flight<K, V>>,
    completed: bool,
}

impl<K: Clone + Eq + Hash, V> Drop for LeaderGuard<'_, K, V> {
    fn drop(&mut self) {
        if !self.completed {
            let removed = self.cache.flights.lock().take(&self.flight.key);
            debug_assert!(
                removed
                    .as_ref()
                    .is_some_and(|handle| Arc::ptr_eq(&handle.0, self.flight))
            );
            {
                let mut state = self.flight.state.lock();
                if matches!(*state, FlightState::Pending) {
                    *state = FlightState::Retry;
                }
            }
            self.flight.changed.notify_all();
        }
    }
}

struct CreatorPinGuard<'a, V> {
    node: Option<NonNull<CacheNode<V>>>,
    _marker: PhantomData<&'a V>,
}

impl<'a, V> CreatorPinGuard<'a, V> {
    #[inline]
    fn new(node: NonNull<CacheNode<V>>) -> Self {
        Self {
            node: Some(node),
            _marker: PhantomData,
        }
    }

    #[inline]
    fn into_lease(mut self) -> CacheLease<'a, V> {
        let node = self.node.take().expect("creator pin node present");
        CacheLease {
            node,
            _marker: PhantomData,
        }
    }
}

impl<V> Drop for CreatorPinGuard<'_, V> {
    fn drop(&mut self) {
        if let Some(node) = self.node.take() {
            // SAFETY: node points to an allocated CacheNode whose creator pin is held by this guard.
            unsafe {
                release_node_pin(node);
            }
        }
    }
}

struct ReclaimEntry<V> {
    pointer: *mut CacheNode<V>,
    weight: u64,
    domain: NonNull<CacheLookupDomain<V>>,
}
impl<V> ReclaimEntry<V> {
    /// Consumes one live pin; only the final release yields retirement ownership.
    /// The caller must own that pin and keep the allocation/domain valid.
    unsafe fn release_pin(pointer: NonNull<CacheNode<V>>) -> Option<Self> {
        // SAFETY: guaranteed by the caller's live-pin ownership contract.
        let node = unsafe { pointer.as_ref() };
        if node.release_pin() {
            Some(Self {
                pointer: pointer.as_ptr(),
                weight: node.weight,
                domain: node_layout::domain!(node),
            })
        } else {
            None
        }
    }

    /// Queue-only tests never pass sentinel entries to a node reclaimer.
    #[cfg(test)]
    fn sentinel(domain: &CacheLookupDomain<V>, weight: u64) -> Self {
        Self {
            pointer: std::ptr::null_mut(),
            weight,
            domain: NonNull::from(domain),
        }
    }
}

// SAFETY: the retired node is transferred for later destruction only when V is Send.
// Reader access ends through its domain grace period before that destruction.
unsafe impl<V: Send> Send for ReclaimEntry<V> {}

// Final leases and small eviction batches need no queue allocation. Four is
// Vec's previous initial capacity, so larger batches also skip that allocation
// without adding an extra growth step or a fixed reclamation limit.
type PendingEntries<V> = SmallVec<[ReclaimEntry<V>; 4]>;

/// A batch detached only after a same-domain grace-period certificate.
/// The owner borrow keeps the allocation's domain alive through reclamation.
struct ReclaimEntries<'domain, V> {
    domain: &'domain CacheLookupDomain<V>,
    entries: PendingEntries<V>,
}

impl<'domain, V> ReclaimEntries<'domain, V> {
    fn new(domain: &'domain CacheLookupDomain<V>) -> Self {
        Self {
            domain,
            entries: PendingEntries::new(),
        }
    }
    fn extend(&mut self, mut other: Self) {
        crate::retirement_queue::append_owned_batch!(
            std::ptr::from_ref(self.domain), std::ptr::from_ref(other.domain);
            xlfn_kernel::invariant::fail_stop(),
            self.entries.append(&mut other.entries)
        );
    }
}
impl<V> std::ops::Deref for ReclaimEntries<'_, V> {
    type Target = [ReclaimEntry<V>];
    fn deref(&self) -> &Self::Target {
        &self.entries
    }
}

fn merge_reclaims<'domain, V>(
    domain: &'domain CacheLookupDomain<V>,
    batches: impl IntoIterator<Item = ReclaimEntries<'domain, V>>,
) -> ReclaimEntries<'domain, V> {
    let entries = batches
        .into_iter()
        .filter(|batch| !batch.is_empty())
        .reduce(|mut entries, next| {
            entries.extend(next);
            entries
        })
        .unwrap_or_else(|| ReclaimEntries::new(domain));
    crate::retirement_queue::append_owned_batch!(
        std::ptr::from_ref(entries.domain), std::ptr::from_ref(domain);
        xlfn_kernel::invariant::fail_stop(), entries
    )
}

type CacheDomainPermit<'domain> = RotatingReadPermit<'domain, DEFAULT_STRIPE_COUNT>;

struct CacheLookupDomain<V> {
    // D1-D5 are provided by RotatingReadDomain. A cache node must be freed
    // only after the grace period covering every reader that could
    // have observed its pointer has ended.
    domain: RotatingRetirementDomain<DEFAULT_STRIPE_COUNT, PendingEntries<V>>,
    pending_nodes: AtomicUsize,
    pending_weight: AtomicU64,
    // Once the sum exceeds u64, keep a conservative backpressure signal
    // until both retirement queues are observed empty under their locks.
    pending_weight_saturated: AtomicBool,
    peak_pending_nodes: AtomicUsize,
    peak_pending_weight: AtomicU64,
    reclaimed_nodes: AtomicUsize,
    largest_batch: AtomicUsize,
    grace_period_nanos: AtomicU64,
}

impl<V> CacheLookupDomain<V> {
    fn new() -> Self {
        Self {
            domain: RotatingRetirementDomain::new(),
            pending_nodes: AtomicUsize::new(0),
            pending_weight: AtomicU64::new(0),
            pending_weight_saturated: AtomicBool::new(false),
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

    fn pending_weight(&self) -> u64 {
        if self.pending_weight_saturated.load(Ordering::Relaxed) {
            u64::MAX
        } else {
            self.pending_weight.load(Ordering::Relaxed)
        }
    }

    fn add_pending_weight(&self, weight: u64) -> u64 {
        let previous = self
            .pending_weight
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                if self.pending_weight_saturated.load(Ordering::Relaxed) {
                    Some(current)
                } else if let Some(next) = current.checked_add(weight) {
                    Some(next)
                } else {
                    // This closure may retry. Setting a sticky flag early is
                    // conservative and cannot lose the backpressure signal.
                    self.pending_weight_saturated.store(true, Ordering::Relaxed);
                    Some(u64::MAX)
                }
            })
            .unwrap_or_else(|_| xlfn_kernel::invariant::fail_stop());
        if self.pending_weight_saturated.load(Ordering::Relaxed) {
            u64::MAX
        } else {
            previous.saturating_add(weight)
        }
    }

    fn clear_saturated_weight_if_empty(&self) {
        if !self.pending_weight_saturated.load(Ordering::Relaxed) {
            return;
        }
        let _ = self.domain.try_inspect_both(|first, second| {
            if first.is_empty()
                && second.is_empty()
                && self.pending_nodes.load(Ordering::Relaxed) == 0
            {
                self.pending_weight.store(0, Ordering::Relaxed);
                self.pending_weight_saturated
                    .store(false, Ordering::Relaxed);
            }
        });
    }

    fn enqueue_reclaim(&self, entry: ReclaimEntry<V>) {
        self.enqueue_reclaim_impl(entry, |_| {});
    }

    #[cfg(test)]
    fn enqueue_reclaim_with_hook(
        &self,
        entry: ReclaimEntry<V>,
        after_generation_load: impl Fn(GenerationIndex),
    ) {
        self.enqueue_reclaim_impl(entry, after_generation_load);
    }

    fn enqueue_reclaim_impl(
        &self,
        entry: ReclaimEntry<V>,
        after_generation_load: impl Fn(GenerationIndex),
    ) {
        crate::retirement_queue::append_owned_batch!(
            std::ptr::from_ref(self), entry.domain.as_ptr();
            xlfn_kernel::invariant::fail_stop(), {
                // The queue lock is the enqueue linearization point. Revalidate the
                // generation while holding it so a rotation cannot drain the queue
                // just before this retired node is appended. Rotation also takes this
                // lock before publishing its replacement generation: withdrawals
                // already registered here must happen before new-generation lookups.
                // The index protects its own entries, not the copied non-owning NodePtr.
                self.domain.register_retired_with_hook(
                    after_generation_load,
                    |_, mut queue| {
                        let weight = entry.weight;
                        crate::retirement_queue::append_retired!(&mut *queue, entry);
                        let nodes = self
                            .pending_nodes
                            .try_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                                current.checked_add(1)
                            })
                            .map(|previous| previous + 1)
                            .unwrap_or_else(|_| xlfn_kernel::invariant::fail_stop());
                        let weight = self.add_pending_weight(weight);
                        self.peak_pending_nodes.fetch_max(nodes, Ordering::Relaxed);
                        self.peak_pending_weight
                            .fetch_max(weight, Ordering::Relaxed);
                    },
                );
            }
        );
    }

    fn quiesce_and_drain(&self) -> ReclaimEntries<'_, V> {
        // A caller may release a lease or clear/read cache metrics from a
        // compute/weight callback. Defer value destruction until that outer
        // singleflight has returned, just as ordinary maintenance does.
        if ACTIVE_CACHE_INITIALIZATION_DEPTH.get() != 0 {
            return ReclaimEntries::new(self);
        }
        let start = Instant::now();
        let batches = self
            .domain
            .quiesce(|generation| self.drain_generation(generation))
            .unwrap_or_default()
            .into_iter()
            .flatten();
        // Preserve an existing batch allocation when only one generation has
        // debt, instead of allocating and copying a new flattened Vec.
        let entries = merge_reclaims(self, batches);
        self.record_batch(&entries, start);
        entries
    }

    fn try_quiesce_and_drain(&self) -> ReclaimEntries<'_, V> {
        if ACTIVE_CACHE_INITIALIZATION_DEPTH.get() != 0
            || self.pending_nodes.load(Ordering::Relaxed) == 0
        {
            return ReclaimEntries::new(self);
        }
        let start = Instant::now();
        let Some(result) = self
            .domain
            .try_quiesce_if_idle(|generation| self.drain_generation(generation))
        else {
            return ReclaimEntries::new(self);
        };
        let entries = result.unwrap_or_else(|_| ReclaimEntries::new(self));
        self.record_batch(&entries, start);
        entries
    }

    fn drain_generation(&self, generation: DrainedGeneration<'_>) -> ReclaimEntries<'_, V> {
        self.domain
            .take_queue(&generation, |mut queue| {
                let entries = crate::retirement_queue::take_retired!(&mut *queue);
                self.pending_nodes
                    .try_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                        current.checked_sub(entries.len())
                    })
                    .unwrap_or_else(|_| xlfn_kernel::invariant::fail_stop());
                if !self.pending_weight_saturated.load(Ordering::Relaxed) {
                    let weight = entries
                        .iter()
                        .fold(0_u64, |sum, entry| sum.saturating_add(entry.weight));
                    self.pending_weight
                        .try_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                            current.checked_sub(weight)
                        })
                        .unwrap_or_else(|_| xlfn_kernel::invariant::fail_stop());
                }
                ReclaimEntries {
                    domain: self,
                    entries,
                }
            })
            .unwrap_or_else(|| xlfn_kernel::invariant::fail_stop())
    }

    fn record_batch(&self, entries: &[ReclaimEntry<V>], start: Instant) {
        self.clear_saturated_weight_if_empty();
        if !entries.is_empty() {
            self.reclaimed_nodes
                .fetch_add(entries.len(), Ordering::Relaxed);
            self.largest_batch
                .fetch_max(entries.len(), Ordering::Relaxed);
            let nanos = u64::try_from(start.elapsed().as_nanos()).unwrap_or(u64::MAX);
            self.grace_period_nanos.fetch_add(nanos, Ordering::Relaxed);
        }
    }

    #[cfg(any(test, feature = "bench-internals"))]
    fn stats(&self) -> CacheReclamationStats {
        CacheReclamationStats {
            pending_nodes: self.pending_nodes.load(Ordering::Relaxed),
            pending_weight: self.pending_weight(),
            peak_pending_nodes: self.peak_pending_nodes.load(Ordering::Relaxed),
            peak_pending_weight: self.peak_pending_weight.load(Ordering::Relaxed),
            reclaimed_nodes: self.reclaimed_nodes.load(Ordering::Relaxed),
            largest_batch: self.largest_batch.load(Ordering::Relaxed),
            grace_period_nanos: self.grace_period_nanos.load(Ordering::Relaxed),
        }
    }

    fn seal(&self) -> ClosedDomain<'_> {
        self.domain.seal_and_wait()
    }

    fn drain_all(&self, closed: ClosedDomain<'_>) -> ReclaimEntries<'_, V> {
        let batches = self
            .domain
            .take_queues(&closed, |mut first, mut second| {
                [
                    ReclaimEntries {
                        domain: self,
                        entries: crate::retirement_queue::take_retired!(&mut *first),
                    },
                    ReclaimEntries {
                        domain: self,
                        entries: crate::retirement_queue::take_retired!(&mut *second),
                    },
                ]
            })
            .unwrap_or_else(|| xlfn_kernel::invariant::fail_stop());
        merge_reclaims(self, batches)
    }
}

/// Test/benchmark-only resident policy selection. Production uses Quick Cache.
#[cfg(feature = "bench-internals")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CacheBackend {
    Sharded { shards: usize },
    QuickCache { shards: usize },
}

/// Side-effect-free residency and approximate storage observations.
#[cfg(feature = "bench-internals")]
#[derive(Clone, Copy, Debug)]
pub struct CacheResidentStats {
    pub entries: u64,
    pub weight: u64,
    pub index_bytes_estimate: usize,
    /// Whether the resident index estimate excludes opaque metadata.
    pub index_metadata_opaque: bool,
    /// Resident node headers plus caller-reported payload weights. Excludes
    /// live non-resident leases and queued retirement debt.
    pub resident_node_bytes_estimate: u64,
}

/// A concurrent weighted cache with a bounded resident budget.
///
/// `CalculationCache` provides memoization for expensive computations across
/// UDF invocations. Eviction is based on abstract caller-reported weights rather
/// than physical allocations.
pub struct CalculationCache<K, V> {
    weight_budget: usize,
    generation: CacheGeneration,
    flights: Mutex<FlightSet<K, V>>,
    domain: xlfn_kernel::published_owner::PublishedOwner<CacheLookupDomain<V>>,
    clear_lock: Mutex<()>,
    index: ResidentIndex<K, V>,
    clear_fn: Option<fn(*const ())>,
    follower_hook: Option<Box<dyn Fn() + Send + Sync>>,
}

impl<K, V> CalculationCache<K, V>
where
    K: Clone + Eq + Hash + Send + Sync + 'static,
    V: Send + Sync + 'static,
{
    /// Creates a concurrent weighted cache with a bounded resident budget.
    ///
    /// Weight is supplied with each initialization. Values heavier than the
    /// configured budget are returned to the caller but are not retained.
    /// One global weight budget allows a single value to use the entire cache.
    /// Cache misses cannot start another cache initialization from inside an
    /// initializer. Existing cached values may still be read normally.
    #[must_use]
    pub fn new(weight_budget: usize) -> Self {
        Self::with_index(weight_budget, |capacity| ResidentIndex::quick(capacity, 1))
    }

    #[doc(hidden)]
    pub fn new_with_follower_hook(
        weight_budget: usize,
        hook: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        let mut cache = Self::new(weight_budget);
        cache.follower_hook = Some(Box::new(hook));
        cache
    }

    /// Builds a candidate using the identical cache/lifetime implementation.
    #[cfg(feature = "bench-internals")]
    #[must_use]
    pub fn new_with_backend(weight_budget: usize, backend: CacheBackend) -> Self {
        Self::with_index(weight_budget, |capacity| match backend {
            CacheBackend::Sharded { shards } => ResidentIndex::sharded(capacity, shards),
            CacheBackend::QuickCache { shards } => ResidentIndex::quick(capacity, shards),
        })
    }

    fn with_index(
        weight_budget: usize,
        make_index: impl FnOnce(u64) -> ResidentIndex<K, V>,
    ) -> Self {
        let capacity = u64::try_from(weight_budget).unwrap_or(u64::MAX);
        Self {
            weight_budget,
            generation: CacheGeneration::new(),
            flights: Mutex::new(FxHashSet::default()),
            domain: xlfn_kernel::published_owner::PublishedOwner::new(CacheLookupDomain::new()),
            clear_lock: Mutex::new(()),
            index: make_index(capacity),
            clear_fn: Some(|ptr| {
                // SAFETY: [TR-RECLAIM-1] ptr points to a valid CalculationCache<K, V> during Drop.
                let cache = unsafe { &*(ptr as *const Self) };
                cache.flights.lock().clear();
                let closed = cache.domain.seal();
                // Drop has exclusive access. Clear synchronously releases
                // all residency pins before the final node drain.
                cache.index.clear();
                let retired = cache.domain.drain_all(closed);
                reclaim_cache_entries::<V>(retired);
            }),
            follower_hook: None,
        }
    }

    #[cfg(any(test, feature = "bench-internals"))]
    #[must_use]
    pub const fn weight_budget(&self) -> usize {
        self.weight_budget
    }

    /// Observes reclamation without running maintenance. Pending counters
    /// exclude resident nodes and values kept alive by outstanding leases.
    #[cfg(any(test, feature = "bench-internals"))]
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
                .saturating_mul(
                    (std::mem::size_of::<CacheNode<V>>() - std::mem::size_of::<V>()) as u64,
                )
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
        // while the index is still coordinating that initializer.
        if ACTIVE_CACHE_INITIALIZATION_DEPTH.get() != 0 {
            return;
        }
        if mutation {
            self.index.maintenance_after_mutation();
        }
        let nodes = self.domain.pending_nodes.load(Ordering::Relaxed);
        if nodes == 0 {
            return;
        }
        // Under sustained contention, stop admitting more mutation debt until
        // the old readers drain. Hot reads only attempt an idle reclamation.
        let retired = if mutation
            && (nodes >= RECLAIM_BACKPRESSURE_NODES
                || self.domain.pending_weight() >= self.weight_budget.max(1) as u64)
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

    /// Invalidates all currently resident entries and advances the cache generation.
    ///
    /// Values held by existing [`CacheLease`] instances remain valid and will be safely
    /// retired and reclaimed when their leases drop. In-flight computations started before
    /// this clear will still complete and return valid leases to their concurrent callers,
    /// but their results will not be published into the new cache generation.
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
        pin_transitions::lookup_after_observation!(
            eligible = node_layout::eligible!(node_layout::generation!(node), epoch, node_layout::resident!(node)),
            acquire = node.try_acquire_pin(),
            resident = node_layout::resident!(node),
            // SAFETY: the successful acquisition owns this rollback pin.
            rollback retired = unsafe { ReclaimEntry::release_pin(node_ptr.0) },
            context domain_ptr = node.domain,
            leave = drop(permit),
            reclaim = {
                if let Some(entry) = retired {
                    // SAFETY: the domain outlives the lookup; the last pin owns retirement.
                    let domain = unsafe { domain_ptr.as_ref() };
                    domain.enqueue_reclaim(entry);
                    // The caller may hold the singleflight table lock while
                    // repeating this lookup. Its maintenance runs after that
                    // coordination ends and owns all value destruction.
                }
            },
            success = Some(CacheLease {
                node: node_ptr.0,
                _marker: PhantomData,
            }),
            overflow = xlfn_kernel::invariant::fail_stop(),
        )
    }

    /// Returns a lease to the cached value for `key`, or computes and stores it.
    ///
    /// Concurrent requests for the same key coalesce into a single execution of `compute`.
    /// All concurrent callers receive valid leases, even if the value is overweight or the
    /// cache has zero budget (`weight_budget == 0`).
    ///
    /// If `compute` or key operations unwind/panic, allocated creator and flight pins are
    /// released safely via RAII guards without leaking resources, and waiting callers retry
    /// without leaving permanently blocked flights.
    ///
    /// Requests are anchored to the cache generation snapshot taken at entry. If `clear()`
    /// advances the generation while computation is in progress, the result will be returned
    /// to the caller and its single-flight peers, but will not be retained in the post-clear
    /// generation.
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
        epoch: u64,
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

        let mut compute_opt = Some(compute);
        let mut weight_opt = Some(weight);
        let mut vkey_opt = Some(VersionedKey { epoch, key });

        'outer: loop {
            let current_lookup_epoch = vkey_opt.as_ref().unwrap().epoch;
            if let Some(lease) =
                self.get_at_epoch(&vkey_opt.as_ref().unwrap().key, current_lookup_epoch)
            {
                self.maintain(false);
                return Ok(lease);
            }

            if ACTIVE_CACHE_INITIALIZATION_DEPTH.get() != 0 {
                return Err(XllError::Internal {
                    diagnostic_id: crate::diagnostics::id::DiagnosticId::CACHE_REENTRANT,
                });
            }

            let (flight, is_leader) = {
                let mut flights = self.flights.lock();
                let vkey_ref = vkey_opt.as_ref().unwrap();
                if let Some(lease) = self.get_at_epoch(&vkey_ref.key, current_lookup_epoch) {
                    drop(flights);
                    self.maintain(false);
                    return Ok(lease);
                }
                if let Some(handle) = flights.get(vkey_ref) {
                    (Arc::clone(&handle.0), false)
                } else {
                    let vkey = vkey_opt.take().unwrap();
                    let flight = Arc::new(Flight::new(vkey));
                    let inserted = flights.insert(FlightHandle(Arc::clone(&flight)));
                    debug_assert!(inserted);
                    (flight, true)
                }
            };

            if !is_leader {
                let mut state = flight.state.lock();
                let (node_ptr, _w) = loop {
                    match &*state {
                        FlightState::Pending => flight.changed.wait(&mut state),
                        FlightState::Finished(Ok(entry)) => break *entry,
                        FlightState::Finished(Err(err)) => {
                            let error = (**err).clone();
                            drop(state);
                            self.maintain(false);
                            return Err(error);
                        }
                        FlightState::Retry => {
                            drop(state);
                            // Do NOT update request epoch! Stale requests must not be promoted to newer generations.
                            continue 'outer;
                        }
                    }
                };
                drop(state);

                if let Some(hook) = &self.follower_hook {
                    hook();
                }

                // SAFETY: node_ptr points to an allocated CacheNode kept alive by the flight's anchor pin.
                let node = unsafe { node_ptr.0.as_ref() };
                match node.try_acquire_pin() {
                    Ok(true) => {
                        self.maintain(false);
                        return Ok(CacheLease {
                            node: node_ptr.0,
                            _marker: PhantomData,
                        });
                    }
                    Ok(false) => {
                        // Node pins became 0; retry without mutating the request epoch.
                        continue 'outer;
                    }
                    Err(PinOverflow) => xlfn_kernel::invariant::fail_stop(),
                }
            }

            // Leader path
            let _active = ActiveCacheGuard::enter()?;
            let mut guard = LeaderGuard {
                cache: self,
                flight: &flight,
                completed: false,
            };

            let compute_fn = compute_opt.take().expect("compute called once");
            let weight_fn = weight_opt.take().expect("weight called once");

            let res = (|| -> XllResult<(V, usize)> {
                let value = compute_fn()?;
                let measured = weight_fn(&value);
                Ok((value, measured))
            })();

            match res {
                Ok((value, measured)) => {
                    let current_epoch = self.generation.snapshot();
                    let publish = self.weight_budget != 0
                        && measured <= self.weight_budget
                        && current_epoch == flight.key.epoch;

                    let w = u64::try_from(measured).unwrap_or(u64::MAX).max(1);
                    // 1. Allocate node with 1 pin (for creator).
                    // The CreatorPinGuard immediately assumes RAII ownership of this pin.
                    let node = Box::new(CacheNode {
                        value,
                        pins: AtomicUsize::new(1),
                        resident: AtomicBool::new(publish),
                        published: publish,
                        weight: w,
                        generation: flight.key.epoch,
                        domain: NonNull::from(&*self.domain),
                    });
                    let node_ptr = NodePtr(NonNull::from(Box::leak(node)));
                    let creator_guard = CreatorPinGuard::new(node_ptr.0);

                    // SAFETY: node_ptr points to the newly allocated CacheNode kept alive by creator_guard.
                    let node_ref = unsafe { node_ptr.0.as_ref() };

                    // 2. Publish resident first, if eligible.
                    // ResidentEntry owns the new pin before any key code or index insertion can unwind.
                    if publish {
                        node_ref.acquire_anchor_pin();
                        self.index
                            .insert_resident(&flight.key, ResidentEntry::new((node_ptr, w)));

                        self.generation.discard_if_stale(flight.key.epoch, || {
                            self.index.invalidate(&flight.key);
                        });
                    }

                    // 3. Publish result to followers.
                    // Increment pins for Flight, transferring 1 pin to FlightState::Finished.
                    node_ref.acquire_anchor_pin();
                    guard.completed = true;
                    {
                        let mut state = flight.state.lock();
                        *state = FlightState::Finished(Ok((node_ptr, w)));
                    }
                    flight.changed.notify_all();

                    // 4. Only then remove single-flight registration.
                    let removed = self.flights.lock().take(&flight.key);
                    debug_assert!(
                        removed
                            .as_ref()
                            .is_some_and(|handle| Arc::ptr_eq(&handle.0, &flight))
                    );

                    drop(_active);
                    self.maintain(true);

                    // 5. Creator pin transfers seamlessly to returned CacheLease.
                    return Ok(creator_guard.into_lease());
                }
                Err(err) => {
                    guard.completed = true;
                    let arc_err = Arc::new(err.clone());
                    {
                        let mut state = flight.state.lock();
                        *state = FlightState::Finished(Err(arc_err));
                    }
                    flight.changed.notify_all();

                    let removed = self.flights.lock().take(&flight.key);
                    debug_assert!(
                        removed
                            .as_ref()
                            .is_some_and(|handle| Arc::ptr_eq(&handle.0, &flight))
                    );

                    drop(_active);
                    self.maintain(true);

                    return Err(err);
                }
            }
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
        static NUMBERS: CacheEndpoint<u32, u32, Marker> = CacheEndpoint::new("shared-id");
        static TEXT: CacheEndpoint<String, String, Marker> = CacheEndpoint::new("shared-id");
        let registry = CacheRegistry::new(64);

        assert_eq!(
            *registry
                .get_or_try_insert(&NUMBERS, 1, |_| 4, || Ok(7))
                .unwrap(),
            7
        );
        assert_eq!(
            registry
                .get_or_try_insert(&TEXT, String::from("key"), String::len, || Ok(
                    String::from("value")
                ),)
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
    fn cache_miss_clones_only_the_resident_key() {
        let clones = Arc::new(AtomicUsize::new(0));
        let cache = CalculationCache::<CloneCountedKey, u32>::new(8);
        let key = CloneCountedKey {
            value: 1,
            clones: Arc::clone(&clones),
        };
        let lease = cache.get_or_try_insert_with(key, |_| 1, || Ok(7)).unwrap();
        assert_eq!(*lease, 7);
        // The cache takes ownership of the caller's key for retry/invalidation;
        // only the resident key needs a copy, including heap-backed keys.
        assert_eq!(clones.load(Ordering::SeqCst), 1);
        cache.clear();
        assert_eq!(*lease, 7);
        assert_eq!(clones.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn resident_hit_with_get_or_try_insert_does_not_clone_key() {
        let clones = Arc::new(AtomicUsize::new(0));
        let cache = CalculationCache::<CloneCountedKey, u32>::new(8);
        let key1 = CloneCountedKey {
            value: 1,
            clones: Arc::clone(&clones),
        };
        drop(cache.get_or_try_insert_with(key1, |_| 1, || Ok(7)).unwrap());
        assert_eq!(clones.load(Ordering::SeqCst), 1);

        let key2 = CloneCountedKey {
            value: 1,
            clones: Arc::clone(&clones),
        };
        let lease = cache
            .get_or_try_insert_with(key2, |_| 1, || unreachable!())
            .unwrap();
        assert_eq!(*lease, 7);
        assert_eq!(clones.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn follower_does_not_clone_key() {
        let clones = Arc::new(AtomicUsize::new(0));
        let cache = CalculationCache::<CloneCountedKey, u32>::new(8);
        let (started_tx, started_rx) = std::sync::mpsc::sync_channel(1);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
        let cache_ref = &cache;
        let clones_ref = &clones;

        std::thread::scope(|s| {
            s.spawn(move || {
                let key = CloneCountedKey {
                    value: 1,
                    clones: Arc::clone(clones_ref),
                };
                let lease = cache_ref
                    .get_or_try_insert_with(
                        key,
                        |_| 1,
                        || {
                            started_tx.send(()).unwrap();
                            release_rx.recv().unwrap();
                            Ok(7)
                        },
                    )
                    .unwrap();
                assert_eq!(*lease, 7);
            });

            started_rx.recv().unwrap();

            let follower = s.spawn(move || {
                let key = CloneCountedKey {
                    value: 1,
                    clones: Arc::clone(clones_ref),
                };
                let lease = cache_ref
                    .get_or_try_insert_with(key, |_| 1, || unreachable!())
                    .unwrap();
                assert_eq!(*lease, 7);
            });

            std::thread::sleep(Duration::from_millis(20));
            release_tx.send(()).unwrap();
            follower.join().unwrap();
        });

        // Exactly 1 clone was performed across both leader and follower: by resident index insertion.
        assert_eq!(clones.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn zero_budget_miss_does_not_clone_key() {
        let clones = Arc::new(AtomicUsize::new(0));
        let cache = CalculationCache::<CloneCountedKey, u32>::new(0);
        let key = CloneCountedKey {
            value: 1,
            clones: Arc::clone(&clones),
        };
        let lease = cache.get_or_try_insert_with(key, |_| 1, || Ok(7)).unwrap();
        assert_eq!(*lease, 7);
        assert_eq!(clones.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn overweight_miss_does_not_clone_key() {
        let clones = Arc::new(AtomicUsize::new(0));
        let cache = CalculationCache::<CloneCountedKey, u32>::new(10);
        let key = CloneCountedKey {
            value: 1,
            clones: Arc::clone(&clones),
        };
        let lease = cache
            .get_or_try_insert_with(key, |_| 100, || Ok(7))
            .unwrap();
        assert_eq!(*lease, 7);
        assert_eq!(clones.load(Ordering::SeqCst), 0);
    }

    #[test]
    #[cfg(not(miri))]
    fn foreign_cache_reclaim_batch_is_fail_stop() {
        const CASE: &str = "XLFN_TEST_FOREIGN_CACHE_RECLAIM_BATCH";
        if let Ok(case) = std::env::var(CASE) {
            let owner = CacheLookupDomain::<()>::new();
            let foreign = CacheLookupDomain::<()>::new();
            if case == "register" {
                owner.enqueue_reclaim_with_hook(ReclaimEntry::sentinel(&foreign, 1), |_| {
                    std::process::exit(86); // A distinct failure if registration is reached.
                });
                return;
            }
            foreign.enqueue_reclaim(ReclaimEntry::sentinel(&foreign, 1));
            let foreign_batch = foreign.quiesce_and_drain();
            match case.as_str() {
                "first" => {
                    let _ = merge_reclaims(&owner, [foreign_batch]);
                }
                "append" => {
                    owner.enqueue_reclaim(ReclaimEntry::sentinel(&owner, 2));
                    let own_batch = owner.quiesce_and_drain();
                    let _ = merge_reclaims(&owner, [own_batch, foreign_batch]);
                }
                _ => panic!("unknown foreign-batch test case"),
            }
            return;
        }
        for case in ["first", "append", "register"] {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "cache::tests::foreign_cache_reclaim_batch_is_fail_stop",
                ])
                .env(CASE, case)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .unwrap();
            assert!(!status.success(), "foreign batch must fail-stop: {case}");
            assert_ne!(status.code(), Some(86), "registration ran before rejection");
            #[cfg(unix)]
            {
                use std::os::unix::process::ExitStatusExt;
                assert_eq!(status.signal(), Some(6), "expected SIGABRT: {case}");
            }
        }
    }

    #[test]
    fn drained_cache_batches_keep_owner_and_all_entries_when_merged() {
        let domain = CacheLookupDomain::<()>::new();
        domain.enqueue_reclaim(ReclaimEntry::sentinel(&domain, 11));
        let first = domain.quiesce_and_drain();
        domain.enqueue_reclaim(ReclaimEntry::sentinel(&domain, 17));
        let second = domain.quiesce_and_drain();
        let batch = merge_reclaims(&domain, [first, second]);
        assert!(std::ptr::eq(batch.domain, &domain));
        assert_eq!(batch.len(), 2);
        assert_eq!(batch.iter().map(|entry| entry.weight).sum::<u64>(), 28);
        assert_eq!(domain.pending_nodes.load(Ordering::Relaxed), 0);
        // Sentinel entries exercise queue ownership only, never destruction.
    }

    #[test]
    fn large_retirement_weights_do_not_interrupt_registration_or_drain() {
        let domain = CacheLookupDomain::<()>::new();
        domain.enqueue_reclaim(ReclaimEntry::sentinel(&domain, u64::MAX));
        domain.enqueue_reclaim(ReclaimEntry::sentinel(&domain, u64::MAX));
        assert_eq!(domain.stats().pending_nodes, 2);
        assert_eq!(domain.stats().pending_weight, u64::MAX);

        let retired = domain.quiesce_and_drain();
        assert_eq!(retired.len(), 2);
        assert_eq!(domain.stats().pending_nodes, 0);
        assert_eq!(domain.stats().pending_weight, 0);
        // Sentinel entries exercise queue accounting only, never destruction.
    }

    #[test]
    fn saturated_pending_weight_keeps_backpressure_until_both_queues_empty() {
        let domain = CacheLookupDomain::<()>::new();
        let reader = domain.domain.enter(0).unwrap();
        domain.enqueue_reclaim(ReclaimEntry::sentinel(&domain, u64::MAX));
        assert!(
            domain
                .domain
                .poll_quiesce(|generation| domain.drain_generation(generation))
                .is_none()
        );
        assert_eq!(domain.domain.current_generation().index(), 1);
        domain.enqueue_reclaim(ReclaimEntry::sentinel(&domain, u64::MAX));
        drop(reader);

        let first = domain
            .domain
            .poll_quiesce(|generation| domain.drain_generation(generation))
            .unwrap()
            .unwrap();
        assert_eq!(first.len(), 1);
        assert_eq!(domain.stats().pending_nodes, 1);
        assert_eq!(domain.pending_weight(), u64::MAX);

        let second = domain.quiesce_and_drain();
        assert_eq!(second.len(), 1);
        assert_eq!(domain.stats().pending_nodes, 0);
        assert_eq!(domain.pending_weight(), 0);
    }

    #[test]
    fn enqueue_reclaim_racing_rotation_is_not_lost() {
        let domain = Arc::new(CacheLookupDomain::<()>::new());
        let (loaded_tx, loaded_rx) = mpsc::sync_channel(0);
        let (resume_tx, resume_rx) = mpsc::sync_channel(0);
        let (rotated_tx, rotated_rx) = mpsc::sync_channel(0);

        let enqueuer_domain = Arc::clone(&domain);
        let enqueuer = std::thread::spawn(move || {
            let first_load = std::cell::Cell::new(true);
            enqueuer_domain.enqueue_reclaim_with_hook(
                ReclaimEntry::sentinel(&enqueuer_domain, 0),
                |generation| {
                    if first_load.replace(false) {
                        loaded_tx.send(generation).unwrap();
                        resume_rx.recv().unwrap();
                    }
                },
            );
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

        assert!(domain.domain.inspect_queue(0, |queue| queue.is_empty()));
        assert_eq!(domain.domain.inspect_queue(1, |queue| queue.len()), 1);
        let _ = domain.drain_all(domain.seal());
    }

    #[test]
    fn cache_idle_reclamation_does_not_publish_before_registration_unlocks() {
        let domain = Arc::new(CacheLookupDomain::<()>::new());
        domain.enqueue_reclaim(ReclaimEntry::sentinel(&domain, 7));
        let generation = domain.domain.current_generation();
        let (worker, result, observed_generation) =
            domain.domain.inspect_queue(generation.index(), |_| {
                let (done_tx, done_rx) = mpsc::channel();
                let worker_domain = Arc::clone(&domain);
                let worker = std::thread::spawn(move || {
                    let retired = worker_domain.try_quiesce_and_drain();
                    done_tx
                        .send((retired.len(), worker_domain.domain.current_generation()))
                        .unwrap();
                });
                // An unbarred idle rotation publishes a new generation then
                // blocks on this queue. Release it before assertions so a
                // regression cannot hang.
                let result = done_rx.recv_timeout(Duration::from_secs(1));
                let observed_generation = domain.domain.current_generation();
                (worker, result, observed_generation)
            });
        worker.join().unwrap();
        assert_eq!(result, Ok((0, generation)));
        assert_eq!(observed_generation, generation);
        assert_eq!(domain.quiesce_and_drain().len(), 1);
        assert_eq!(domain.stats().pending_nodes, 0);
    }

    #[cfg(feature = "bench-internals")]
    #[test]
    fn cache_read_scope_is_not_send_or_sync() {
        static_assertions::assert_not_impl_any!(CacheReadScope<'static, u32, u32>: Send, Sync);
    }

    #[cfg(feature = "bench-internals")]
    #[test]
    fn endpoint_exposes_scoped_reads() {
        enum Marker {}
        static ENDPOINT: CacheEndpoint<u32, u32, Marker> = CacheEndpoint::new("SCOPED_READ");

        let registry = CacheRegistry::new(8);
        drop(
            registry
                .get_or_try_insert(&ENDPOINT, 1, |_| 1, || Ok(7))
                .unwrap(),
        );

        let scope = registry.read_scope(&ENDPOINT).unwrap();
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
                    assert!((0..2).any(|index| {
                        cache_ref
                            .domain
                            .domain
                            .inspect_queue(index, |queue| !queue.is_empty())
                    }));
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
                while !(0..2).any(|index| {
                    cache
                        .domain
                        .domain
                        .inspect_queue(index, |queue| !queue.is_empty())
                }) && std::time::Instant::now() < deadline
                {
                    std::thread::yield_now();
                }
                assert!((0..2).any(|index| {
                    cache
                        .domain
                        .domain
                        .inspect_queue(index, |queue| !queue.is_empty())
                }));
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
    fn eviction_is_bounded_by_approximate_bytes() {
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
    fn clear_allows_an_inflight_initializer_to_complete() {
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
        static FIRST: CacheEndpoint<u32, u32, First> =
            CacheEndpoint::<u32, u32, First>::new("FIRST");
        static SECOND: CacheEndpoint<u32, String, Second> =
            CacheEndpoint::<u32, String, Second>::new("SECOND");

        let registry = CacheRegistry::new(8);

        assert_eq!(
            *registry
                .get_or_try_insert(&FIRST, 1, |_| 4, || Ok(7))
                .unwrap(),
            7
        );
        assert_eq!(
            registry
                .get_or_try_insert(&SECOND, 1, String::len, || Ok("seven".to_owned()))
                .unwrap()
                .as_str(),
            "seven"
        );

        assert_eq!(*registry.get(&FIRST, &1).unwrap().unwrap(), 7);
        assert_eq!(
            registry.get(&SECOND, &1).unwrap().unwrap().as_str(),
            "seven"
        );
    }

    #[test]
    fn registry_differentiates_endpoints_by_marker_type() {
        enum Number {}
        enum Text {}
        let number = CacheEndpoint::<u32, u32, Number>::new("DUPLICATE");
        let text = CacheEndpoint::<u32, String, Text>::new("DUPLICATE");
        let registry = CacheRegistry::new(1024);

        assert_eq!(
            *registry
                .get_or_try_insert(&number, 1, |_| 4, || Ok(7))
                .unwrap(),
            7
        );
        assert_eq!(
            registry
                .get_or_try_insert(&text, 1, String::len, || Ok("seven".to_owned()))
                .unwrap()
                .as_str(),
            "seven"
        );
        assert_eq!(registry.endpoint_count(), 2);
    }

    #[test]
    fn registry_clear_invalidates_bound_endpoint_values() {
        enum FirstUse {}
        static ENDPOINT: CacheEndpoint<u32, u32, FirstUse> =
            CacheEndpoint::<u32, u32, FirstUse>::new("FIRST_USE");

        let registry = CacheRegistry::new(8);
        let reg = &registry;
        let (started_tx, started_rx) = std::sync::mpsc::sync_channel(1);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
        std::thread::scope(|s| {
            let worker = s.spawn(move || {
                let lease = reg
                    .get_or_try_insert(
                        &ENDPOINT,
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
                .get_or_try_insert(&ENDPOINT, 1, |_| 4, || Ok(9))
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

                // Models the index publishing the initialized entry before
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
                        let release_pin = || {
                            use pin_transitions::{Release, pin_retry_expr};
                            pin_transitions::release_pin!(previous;
                                decrement = n.pins.fetch_sub(1, Ordering::Release),
                                classify = pin_transitions::release(previous as _),
                                fence = loom::sync::atomic::fence(Ordering::Acquire),
                                last = true, pinned = false,
                                fail_stop = panic!("pin underflow"),
                            )
                        };
                        let lease = pin_transitions::lookup_after_observation!(
                            eligible = node_layout::resident!(n),
                            acquire = (|| {
                                use pin_transitions::{Acquire, pin_retry_expr};
                                pin_transitions::acquire_retry!(raw, next;
                                    load = n.pins.load(Ordering::Relaxed),
                                    classify = pin_transitions::acquire(raw as _),
                                    attempt = n.pins.compare_exchange_weak(raw, next as usize, Ordering::Relaxed, Ordering::Relaxed),
                                    success = Ok(true), zero = Ok(false), overflow = Err(PinOverflow);
                                )
                            })(),
                            resident = node_layout::resident!(n),
                            rollback retired = release_pin(),
                            context _domain = (),
                            leave = reader_dom.leave(),
                            reclaim = {
                                if retired {
                                    reader_dom.quiesce();
                                    n.reclaimed.store(true, Ordering::Release);
                                }
                            },
                            success = Some(()),
                            overflow = panic!("pin overflow"),
                        );
                        if lease.is_some() {
                            assert!(
                                !n.reclaimed.load(Ordering::Acquire),
                                "UAF: node reclaimed while lease held"
                            );
                            if release_pin() {
                                n.reclaimed.store(true, Ordering::Release);
                            }
                        }
                        return;
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
                    // Publish residency release, then acquire earlier holders
                    // before this final releaser hands the node to reclamation.
                    use pin_transitions::{Release, pin_retry_expr};
                    pin_transitions::release_pin!(previous;
                        decrement = n.pins.fetch_sub(1, Ordering::Release),
                        classify = pin_transitions::release(previous as _),
                        fence = loom::sync::atomic::fence(Ordering::Acquire),
                        last = {
                            // TR-RECLAIM-1: Quiesce domain before reclaim
                            evictor_dom.quiesce();
                            n.reclaimed.store(true, Ordering::Release);
                        },
                        pinned = (), fail_stop = panic!("pin underflow"),
                    );
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
                    if n.pins.fetch_sub(1, Ordering::Release) == 1 {
                        loom::sync::atomic::fence(Ordering::Acquire);
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
    fn same_endpoint_resolved_twice_shares_single_allocation() {
        enum Marker {}
        static ENDPOINT: CacheEndpoint<u32, u32, Marker> = CacheEndpoint::new("SAME_ENDPOINT");
        let registry = CacheRegistry::new(64);
        let a = registry.resolve_cache(&ENDPOINT).unwrap();
        let b = registry.resolve_cache(&ENDPOINT).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn different_marker_types_create_distinct_storage_allocations() {
        enum MarkerA {}
        enum MarkerB {}
        static ENDPOINT_A: CacheEndpoint<u32, u32, MarkerA> = CacheEndpoint::new("SHARED_ID");
        static ENDPOINT_B: CacheEndpoint<u32, u32, MarkerB> = CacheEndpoint::new("SHARED_ID");
        let registry = CacheRegistry::new(64);
        ENDPOINT_A
            .get_or_try_insert(&registry, 1, |_| 1, || Ok(10))
            .unwrap();
        ENDPOINT_B
            .get_or_try_insert(&registry, 1, |_| 1, || Ok(20))
            .unwrap();
        assert_eq!(registry.endpoint_count(), 2);
        assert_eq!(*ENDPOINT_A.get(&registry, &1).unwrap().unwrap(), 10);
        assert_eq!(*ENDPOINT_B.get(&registry, &1).unwrap().unwrap(), 20);
    }

    #[test]
    fn endpoint_remains_usable_across_registry_clear() {
        enum Marker {}
        static ENDPOINT: CacheEndpoint<u32, u32, Marker> = CacheEndpoint::new("SURVIVE_CLEAR");
        let registry = CacheRegistry::new(64);

        assert_eq!(
            *ENDPOINT
                .get_or_try_insert(&registry, 1, |_| 1, || Ok(100))
                .unwrap(),
            100
        );
        assert_eq!(*ENDPOINT.get(&registry, &1).unwrap().unwrap(), 100);

        registry.clear();

        // Old generation value is missed
        assert!(ENDPOINT.get(&registry, &1).unwrap().is_none());

        // New value can be inserted in the new generation
        assert_eq!(
            *ENDPOINT
                .get_or_try_insert(&registry, 1, |_| 1, || Ok(200))
                .unwrap(),
            200
        );
        assert_eq!(*ENDPOINT.get(&registry, &1).unwrap().unwrap(), 200);
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
    fn inline_payload_keeps_alignment_address_and_exactly_once_drop() {
        #[repr(align(64))]
        struct Payload {
            bytes: [u8; 8192],
            drops: Arc<AtomicUsize>,
            expected_address: Arc<AtomicUsize>,
        }
        impl Drop for Payload {
            fn drop(&mut self) {
                assert_eq!(self.bytes[8191], 17);
                assert_eq!(
                    std::ptr::from_ref(self).addr(),
                    self.expected_address.load(Ordering::Relaxed),
                    "reclamation must destroy the inline value at its leased address",
                );
                self.drops.fetch_add(1, Ordering::Relaxed);
            }
        }
        for capacity in [0, 16_384] {
            let drops = Arc::new(AtomicUsize::new(0));
            let expected_address = Arc::new(AtomicUsize::new(0));
            let cache = CalculationCache::new(capacity);
            let lease = cache
                .get_or_try_insert_with(
                    1_u32,
                    |_| 8192,
                    || {
                        Ok(Payload {
                            bytes: [17; 8192],
                            drops: Arc::clone(&drops),
                            expected_address: Arc::clone(&expected_address),
                        })
                    },
                )
                .unwrap();
            let address = std::ptr::from_ref(&*lease);
            expected_address.store(address.addr(), Ordering::Relaxed);
            assert_eq!(address.addr() % 64, 0);
            if capacity != 0 {
                let second = cache.get(&1).unwrap();
                assert_eq!(std::ptr::from_ref(&*second), address);
            }
            cache.clear();
            assert_eq!(std::ptr::from_ref(&*lease), address);
            assert_eq!(drops.load(Ordering::Relaxed), 0);
            drop(lease);
            drop(cache);
            assert_eq!(drops.load(Ordering::Relaxed), 1);
        }
    }

    #[test]
    fn miri_resident_insertion_panic_preserves_creator_pin() {
        struct PanicKey(Arc<std::sync::atomic::AtomicU8>);
        impl Clone for PanicKey {
            fn clone(&self) -> Self {
                if self
                    .0
                    .compare_exchange(1, 0, Ordering::SeqCst, Ordering::SeqCst)
                    .is_ok()
                {
                    panic!("injected key clone panic");
                }
                Self(Arc::clone(&self.0))
            }
        }
        impl Hash for PanicKey {
            fn hash<H: Hasher>(&self, state: &mut H) {
                if self
                    .0
                    .compare_exchange(2, 0, Ordering::SeqCst, Ordering::SeqCst)
                    .is_ok()
                {
                    panic!("injected key hash panic");
                }
                0_u8.hash(state);
            }
        }
        impl PartialEq for PanicKey {
            fn eq(&self, _: &Self) -> bool {
                true
            }
        }
        impl Eq for PanicKey {}

        struct DropProbe(Arc<AtomicUsize>);
        impl Drop for DropProbe {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }

        for panic_mode in [1, 2] {
            #[cfg(not(feature = "bench-internals"))]
            let indexes = [ResidentIndex::<PanicKey, DropProbe>::quick(16, 1)];
            #[cfg(feature = "bench-internals")]
            let indexes = [
                ResidentIndex::<PanicKey, DropProbe>::quick(16, 1),
                ResidentIndex::sharded(16, 8),
            ];
            for index in indexes {
                let drops = Arc::new(AtomicUsize::new(0));
                let domain = Box::new(CacheLookupDomain::new());
                let node = NonNull::from(Box::leak(Box::new(CacheNode {
                    value: DropProbe(Arc::clone(&drops)),
                    pins: AtomicUsize::new(1),
                    resident: AtomicBool::new(true),
                    published: true,
                    weight: 1,
                    generation: 0,
                    domain: NonNull::from(&*domain),
                })));
                let creator = CreatorPinGuard::new(node);
                // SAFETY: creator owns the initial pin in this live allocation.
                unsafe { node.as_ref() }.acquire_anchor_pin();
                let entry = ResidentEntry::new((NodePtr(node), 1));
                let snapshot = entry.clone();
                drop(snapshot); // A lookup clone must not discharge residency.
                let key = VersionedKey {
                    epoch: 0,
                    key: PanicKey(Arc::new(std::sync::atomic::AtomicU8::new(panic_mode))),
                };
                let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
                    index.insert_resident(&key, entry);
                }));
                assert!(result.is_err());
                // SAFETY: the insertion must have released only its resident pin.
                assert_eq!(unsafe { node.as_ref() }.pins.load(Ordering::Acquire), 1);
                assert_eq!(drops.load(Ordering::Acquire), 0);
                drop(creator.into_lease());
                index.clear();
                let closed = domain.seal();
                reclaim_cache_entries::<DropProbe>(domain.drain_all(closed));
                assert_eq!(drops.load(Ordering::Acquire), 1);
            }
        }
    }

    #[cfg(feature = "bench-internals")]
    #[test]
    fn retired_node_ownership_retains_payload_type_and_send_bound() {
        static_assertions::assert_not_impl_any!(ReclaimEntry<Rc<()>>: Send, Sync, Copy, Clone);
        static_assertions::assert_impl_all!(ReclaimEntry<u8>: Send);
        assert_eq!(
            std::mem::size_of::<ReclaimEntry<u8>>(),
            std::mem::size_of::<(*mut (), u64, NonNull<CacheLookupDomain<u8>>)>()
        );
    }

    #[test]
    fn cache_node_pin_overflow_is_prevented() {
        let node = CacheNode {
            value: 42u32,
            pins: AtomicUsize::new(usize::MAX),
            resident: AtomicBool::new(true),
            published: true,
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
            value: 42_u32,
            pins: AtomicUsize::new(1),
            resident: AtomicBool::new(false),
            published: false,
            weight: 1,
            generation: 0,
            domain: NonNull::dangling(),
        };
        assert!(node.release_pin());
        assert_eq!(node.try_acquire_pin(), Ok(false));
        assert_eq!(node.pins.load(Ordering::Acquire), 0);
    }

    #[test]
    fn miri_final_cache_pin_acquires_all_holders_before_retirement() {
        let node = CacheNode {
            value: [AtomicUsize::new(0), AtomicUsize::new(0)],
            pins: AtomicUsize::new(2),
            resident: AtomicBool::new(false),
            published: false,
            weight: 1,
            generation: 0,
            domain: NonNull::dangling(),
        };
        std::thread::scope(|scope| {
            for index in 0..2 {
                let node = &node;
                scope.spawn(move || {
                    node.value[index].store(index + 1, Ordering::Relaxed);
                    if node.release_pin() {
                        // Check before either join or queue locking can hide
                        // a missing Acquire fence on the final pin release.
                        assert_eq!(node.value[0].load(Ordering::Relaxed), 1);
                        assert_eq!(node.value[1].load(Ordering::Relaxed), 2);
                    }
                });
            }
        });
    }

    #[cfg(not(all(target_os = "windows", target_arch = "x86")))]
    #[test]
    #[cfg_attr(miri, ignore)]
    fn loom_final_pin_fence_acquires_all_holders_before_retirement() {
        use loom::sync::Arc;
        use loom::sync::atomic::{AtomicUsize, Ordering};

        loom::model(|| {
            let pins = Arc::new(AtomicUsize::new(2));
            let values = Arc::new([AtomicUsize::new(0), AtomicUsize::new(0)]);
            let mut holders = Vec::new();
            for index in 0..2 {
                let pins = Arc::clone(&pins);
                let values = Arc::clone(&values);
                holders.push(loom::thread::spawn(move || {
                    values[index].store(index + 1, Ordering::Relaxed);
                    use pin_transitions::{Release, pin_retry_expr};
                    pin_transitions::release_pin!(previous;
                        decrement = pins.fetch_sub(1, Ordering::Release),
                        classify = pin_transitions::release(previous as _),
                        fence = loom::sync::atomic::fence(Ordering::Acquire),
                        last = {
                            assert_eq!(values[0].load(Ordering::Relaxed), 1);
                            assert_eq!(values[1].load(Ordering::Relaxed), 2);
                        },
                        pinned = (), fail_stop = panic!("pin underflow"),
                    );
                }));
            }
            for holder in holders {
                holder.join().unwrap();
            }
        });
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
            // These are observations only: neither accessor can maintain the index or
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
    #[ignore = "manual release-mode zero-budget cache performance measurement"]
    fn benchmark_zero_budget_cache() {
        for budget in [0, 8] {
            let cache = CalculationCache::new(budget);
            let mut samples = Vec::new();
            for round in 0..12 {
                let start = std::time::Instant::now();
                for key in 0..10_000_u64 {
                    let value = cache
                        .get_or_try_insert_with(std::hint::black_box(key), |_| 1, || Ok(key))
                        .unwrap();
                    std::hint::black_box(*value);
                }
                if round != 0 {
                    samples.push(start.elapsed().as_nanos() / 10_000);
                }
            }
            samples.sort_unstable();
            eprintln!("cache_budget={budget} median_ns={}", samples[5]);
        }
    }

    #[test]
    fn miri_zero_budget_values_are_reclaimed_when_their_leases_end() {
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
        assert_eq!(cache.reclamation_stats().reclaimed_nodes, 0);
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
    fn pending_weight_preserves_totals_above_the_32_bit_limit() {
        let domain = CacheLookupDomain::<()>::new();
        // Sentinel pointers never reach a reclaimer in this queue-only test.
        domain.enqueue_reclaim(ReclaimEntry::sentinel(&domain, u64::from(u32::MAX)));
        domain.enqueue_reclaim(ReclaimEntry::sentinel(&domain, u64::from(u32::MAX)));
        assert_eq!(domain.stats().pending_weight, 2 * u64::from(u32::MAX));
        let entries = domain.quiesce_and_drain();
        assert_eq!(entries.len(), 2);
        assert_eq!(domain.stats().pending_weight, 0);
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn oversized_weight_is_checked_before_weight_saturation() {
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
        static EXISTING: CacheEndpoint<u32, ReentrantBind, Marker> = CacheEndpoint::new("existing");
        static NEW: CacheEndpoint<u32, u32, NewMarker> = CacheEndpoint::new("new");
        struct ReentrantBind {
            registry: std::sync::Weak<CacheRegistry>,
            completed: Arc<AtomicBool>,
        }
        impl Drop for ReentrantBind {
            fn drop(&mut self) {
                let registry = self.registry.upgrade().unwrap();
                // Detect the lock regression without hanging the test suite.
                assert!(registry.caches.try_write().is_some());
                drop(
                    registry
                        .get_or_try_insert(&NEW, 2, |_| 1, || Ok(9))
                        .unwrap(),
                );
                self.completed.store(true, Ordering::Release);
            }
        }
        let registry = Arc::new(CacheRegistry::new(16));
        let completed = Arc::new(AtomicBool::new(false));
        drop(
            registry
                .get_or_try_insert(
                    &EXISTING,
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
        assert_eq!(*registry.get(&NEW, &2).unwrap().unwrap(), 9);
    }
}

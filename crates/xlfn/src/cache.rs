use crate::error::InputError;
use crate::{XllError, XllResult};
use moka::sync::Cache;
use parking_lot::{Mutex, RwLock};
use std::any::{Any, TypeId};
use std::cell::Cell;
use std::collections::HashMap;
use std::hash::Hash;
use std::marker::PhantomData;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicU64, Ordering};
use xlfn_kernel::drain_gate::{StripedDrainGate, StripedDrainPermit};

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
    value: NonNull<V>,
    _permit: StripedDrainPermit<'a, 32>,
}

// SAFETY: StripedDrainPermit ensures V is not accessed after reclaim.
unsafe impl<V: Send> Send for CacheLease<'_, V> {}
// SAFETY: StripedDrainPermit ensures V is not accessed after reclaim.
unsafe impl<V: Sync> Sync for CacheLease<'_, V> {}

impl<V> std::ops::Deref for CacheLease<'_, V> {
    type Target = V;

    fn deref(&self) -> &Self::Target {
        // SAFETY: self._permit ensures value is not reclaimed while lease is live.
        unsafe { self.value.as_ref() }
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
    cache: Box<ErasedCache>,
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
                        cache: erased,
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
        let caches = self.caches.read();
        for entry in caches.values() {
            let epoch = (entry.ops.advance_generation)(&*entry.cache);
            (entry.ops.invalidate_before)(&*entry.cache, epoch);
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

struct ValuePtr<V>(NonNull<V>);

impl<V> Clone for ValuePtr<V> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<V> Copy for ValuePtr<V> {}

// SAFETY: ValuePtr is internal to CalculationCache where V is Send + Sync.
unsafe impl<V: Send> Send for ValuePtr<V> {}
// SAFETY: ValuePtr is internal to CalculationCache where V is Send + Sync.
unsafe impl<V: Sync> Sync for ValuePtr<V> {}

struct CacheNode<V> {
    _value: Box<V>,
    epoch: u64,
}

// SAFETY: Box<V> is Send if V: Send.
unsafe impl<V: Send> Send for CacheNode<V> {}
// SAFETY: Box<V> is Sync if V: Sync.
unsafe impl<V: Sync> Sync for CacheNode<V> {}

pub struct CalculationCache<K, V> {
    weight_budget: usize,
    generation: CacheGeneration,
    readers: StripedDrainGate<32>,
    clear_lock: Mutex<()>,
    arena: Mutex<Vec<CacheNode<V>>>,
    cache: Cache<VersionedKey<K>, (ValuePtr<V>, u32)>,
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
        let weight_budget = weight_budget.min(u32::MAX as usize);
        let capacity = u64::try_from(weight_budget).unwrap_or(u64::MAX);
        Self {
            weight_budget,
            generation: CacheGeneration::new(),
            readers: StripedDrainGate::new_open(),
            clear_lock: Mutex::new(()),
            arena: Mutex::new(Vec::new()),
            cache: Cache::builder()
                .max_capacity(capacity)
                .weigher(|_, entry: &(ValuePtr<V>, u32)| entry.1)
                .support_invalidation_closures()
                .build(),
        }
    }

    #[must_use]
    pub const fn weight_budget(&self) -> usize {
        self.weight_budget
    }

    #[must_use]
    pub fn used_weight(&self) -> usize {
        self.cache.run_pending_tasks();
        usize::try_from(self.cache.weighted_size()).unwrap_or(usize::MAX)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.cache.run_pending_tasks();
        usize::try_from(self.cache.entry_count()).unwrap_or(usize::MAX)
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn clear(&self) {
        let _guard = self.clear_lock.lock();
        let epoch = self.generation.advance();
        self.cache
            .invalidate_entries_if(move |key, _| key.epoch < epoch)
            .expect("invalidation closures are enabled");
        self.cache.run_pending_tasks();

        self.readers.seal_and_wait();
        {
            let mut arena = self.arena.lock();
            arena.retain(|node| node.epoch >= epoch);
        }
        self.readers
            .reopen()
            .unwrap_or_else(|_| xlfn_kernel::invariant::fail_stop());
    }

    fn invalidate_before(&self, epoch: u64) {
        let _guard = self.clear_lock.lock();
        self.cache
            .invalidate_entries_if(move |key, _| key.epoch < epoch)
            .expect("invalidation closures are enabled");
        self.cache.run_pending_tasks();

        self.readers.seal_and_wait();
        {
            let mut arena = self.arena.lock();
            arena.retain(|node| node.epoch >= epoch);
        }
        self.readers
            .reopen()
            .unwrap_or_else(|_| xlfn_kernel::invariant::fail_stop());
    }

    pub fn get<'a>(&'a self, key: &K) -> Option<CacheLease<'a, V>> {
        let permit = self.readers.try_enter_current().ok()?;
        let epoch = self.generation.snapshot();
        let vkey = VersionedKey {
            epoch,
            key: key.clone(),
        };
        let (ptr, _) = self.cache.get(&vkey)?;
        Some(CacheLease {
            value: ptr.0,
            _permit: permit,
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
        epoch: u64,
    ) -> XllResult<CacheLease<'a, V>>
    where
        F: FnOnce() -> XllResult<V>,
        W: FnOnce(&V) -> usize,
    {
        let permit = self
            .readers
            .try_enter_current()
            .map_err(|_| XllError::Closing)?;

        let vkey = VersionedKey { epoch, key };
        if let Some((ptr, _)) = self.cache.get(&vkey) {
            return Ok(CacheLease {
                value: ptr.0,
                _permit: permit,
            });
        }
        drop(permit);

        let _active = ActiveCacheGuard::enter()?;
        let insertion_epoch = std::sync::atomic::AtomicU64::new(epoch);
        let initialized = self
            .cache
            .try_get_with(vkey.clone(), || {
                let value = compute()?;
                let measured = weight(&value);
                let w = u32::try_from(measured).unwrap_or(u32::MAX).max(1);
                let boxed = Box::new(value);
                let ptr = ValuePtr(NonNull::from(&*boxed));
                let current_epoch = self.generation.snapshot();
                insertion_epoch.store(current_epoch, std::sync::atomic::Ordering::Release);
                self.arena.lock().push(CacheNode {
                    _value: boxed,
                    epoch: current_epoch,
                });
                Ok::<_, XllError>((ptr, w))
            })
            .map_err(|error| (*error).clone())?;

        self.generation.discard_if_stale(epoch, || {
            self.cache.invalidate(&vkey);
        });

        let permit = self
            .readers
            .try_enter_current()
            .map_err(|_| XllError::Closing)?;

        if self.generation.snapshot() != insertion_epoch.load(std::sync::atomic::Ordering::Acquire)
        {
            return Err(XllError::Closing);
        }

        Ok(CacheLease {
            value: initialized.0.0,
            _permit: permit,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

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
        cache.cache.run_pending_tasks();
        assert_eq!(cache.used_weight(), 1);
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
        let cache = CalculationCache::<u32, u32>::new(8);
        for key in [1, 2] {
            let value = cache
                .get_or_try_insert_with(key, |_| 8, || Ok(key * 10))
                .unwrap();
            assert_eq!(*value, key * 10);
        }
        assert_eq!(cache.len(), 1);
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

            // While lease is held, clear() MUST be blocked in seal_and_wait()
            // and TrackDrop MUST NOT be dropped!
            assert!(!DROPPED.load(Ordering::SeqCst));
            assert!(clear_done_rx.try_recv().is_err());

            // Dropping lease permits seal_and_wait() to unblock
            drop(lease);

            clear_done_rx.recv().unwrap();
            assert!(DROPPED.load(Ordering::SeqCst));
        });
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
                    for _ in 0..50 {
                        if let Ok(lease) =
                            cache_ref.get_or_try_insert_with(1, |_| 10, || Ok(format!("val_{i}")))
                        {
                            assert!(!lease.is_empty());
                        }
                    }
                });
                let t2 = s.spawn(move || {
                    cache_ref.clear();
                });
                t1.join().unwrap();
                t2.join().unwrap();
            }
        });
    }
}

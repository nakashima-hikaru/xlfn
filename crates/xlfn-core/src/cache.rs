use crate::{InputError, XllError, XllResult};
use moka::sync::Cache;
use parking_lot::RwLock;
use std::any::{Any, TypeId};
use std::cell::Cell;
use std::collections::HashMap;
use std::hash::Hash;
use std::marker::PhantomData;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

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
        ACTIVE_CACHE_INITIALIZATION_DEPTH.with(|depth| {
            if depth.get() != 0 {
                return Err(XllError::Internal {
                    diagnostic_id: crate::DiagnosticId::CACHE_REENTRANT,
                });
            }
            depth.set(1);
            Ok(Self)
        })
    }
}

impl Drop for ActiveCacheGuard {
    fn drop(&mut self) {
        ACTIVE_CACHE_INITIALIZATION_DEPTH.with(|depth| {
            debug_assert_eq!(depth.get(), 1);
            depth.set(0);
        });
    }
}

pub struct CacheEndpoint<Marker, K, V> {
    id: &'static str,
    _marker: PhantomData<fn() -> Marker>,
    _key: PhantomData<fn() -> K>,
    _value: PhantomData<fn() -> V>,
}

pub struct BoundCacheEndpoint<Marker, K, V> {
    cache: Arc<CalculationCache<K, V>>,
    _marker: PhantomData<fn() -> Marker>,
}

impl<Marker, K, V> Clone for BoundCacheEndpoint<Marker, K, V> {
    fn clone(&self) -> Self {
        Self {
            cache: Arc::clone(&self.cache),
            _marker: PhantomData,
        }
    }
}

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

impl<Marker, K, V> BoundCacheEndpoint<Marker, K, V>
where
    K: Clone + Eq + Hash + Send + Sync + 'static,
    V: Send + Sync + 'static,
{
    pub fn get_or_try_insert<F, W>(&self, key: K, weight: W, compute: F) -> XllResult<Arc<V>>
    where
        F: FnOnce() -> XllResult<V>,
        W: FnOnce(&V) -> usize,
    {
        self.cache.get_or_try_insert_with(key, weight, compute)
    }

    #[must_use]
    pub fn get(&self, key: &K) -> Option<Arc<V>> {
        self.cache.get(key)
    }
}

trait ErasedCache: Send + Sync {
    fn as_any(&self) -> &dyn Any;
    fn advance_generation(&self) -> u64;
    fn invalidate_before(&self, epoch: u64);
}

struct StoredCache<K, V>(Arc<CalculationCache<K, V>>);

impl<K, V> ErasedCache for StoredCache<K, V>
where
    K: Clone + Eq + Hash + Send + Sync + 'static,
    V: Send + Sync + 'static,
{
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn advance_generation(&self) -> u64 {
        self.0.generation.advance()
    }

    fn invalidate_before(&self, epoch: u64) {
        self.0.invalidate_before(epoch);
    }
}

type CacheMap = HashMap<(TypeId, &'static str), Arc<dyn ErasedCache>>;

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

    pub fn bind<Marker, K, V>(
        &self,
        endpoint: &CacheEndpoint<Marker, K, V>,
    ) -> XllResult<BoundCacheEndpoint<Marker, K, V>>
    where
        Marker: 'static,
        K: Clone + Eq + Hash + Send + Sync + 'static,
        V: Send + Sync + 'static,
    {
        let cache_key = endpoint.key();
        let cache = {
            let caches = self.caches.read();
            if let Some(stored) = caches.get(&cache_key) {
                Self::downcast_cache::<K, V>(stored)?
            } else {
                drop(caches);
                let mut caches = self.caches.write();
                let stored = caches.entry(cache_key).or_insert_with(|| {
                    Arc::new(StoredCache(Arc::new(CalculationCache::<K, V>::new(
                        self.weight_budget_per_endpoint,
                    )))) as Arc<dyn ErasedCache>
                });
                Self::downcast_cache::<K, V>(stored)?
            }
        };
        Ok(BoundCacheEndpoint {
            cache,
            _marker: PhantomData,
        })
    }

    fn downcast_cache<K, V>(stored: &Arc<dyn ErasedCache>) -> XllResult<Arc<CalculationCache<K, V>>>
    where
        K: Clone + Eq + Hash + Send + Sync + 'static,
        V: Send + Sync + 'static,
    {
        stored
            .as_any()
            .downcast_ref::<StoredCache<K, V>>()
            .map(|stored| Arc::clone(&stored.0))
            .ok_or(XllError::Internal {
                diagnostic_id: crate::DiagnosticId::CACHE_TYPE,
            })
    }

    pub fn clear(&self) {
        let caches = {
            let caches = self.caches.write();
            caches
                .values()
                .map(|cache| (Arc::clone(cache), cache.advance_generation()))
                .collect::<Vec<_>>()
        };
        for (cache, epoch) in caches {
            cache.invalidate_before(epoch);
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

struct WeightedValue<V> {
    value: Arc<V>,
    weight: u32,
}

impl<V> Clone for WeightedValue<V> {
    fn clone(&self) -> Self {
        Self {
            value: Arc::clone(&self.value),
            weight: self.weight,
        }
    }
}

pub struct CalculationCache<K, V> {
    weight_budget: usize,
    generation: CacheGeneration,
    cache: Cache<VersionedKey<K>, WeightedValue<V>>,
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
            cache: Cache::builder()
                .max_capacity(capacity)
                .weigher(|_, value: &WeightedValue<V>| value.weight)
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
        let epoch = self.generation.advance();
        self.invalidate_before(epoch);
    }

    fn invalidate_before(&self, epoch: u64) {
        self.cache
            .invalidate_entries_if(move |key, _| key.epoch < epoch)
            .expect("invalidation closures are enabled");
        self.cache.run_pending_tasks();
    }

    pub fn get(&self, key: &K) -> Option<Arc<V>> {
        let epoch = self.generation.snapshot();
        let vkey = VersionedKey {
            epoch,
            key: key.clone(),
        };
        self.cache.get(&vkey).map(|entry| entry.value)
    }

    pub fn get_or_try_insert_with<F, W>(&self, key: K, weight: W, compute: F) -> XllResult<Arc<V>>
    where
        F: FnOnce() -> XllResult<V>,
        W: FnOnce(&V) -> usize,
    {
        let epoch = self.generation.snapshot();
        self.get_or_try_insert_at_epoch(key, weight, compute, epoch)
    }

    fn get_or_try_insert_at_epoch<F, W>(
        &self,
        key: K,
        weight: W,
        compute: F,
        epoch: u64,
    ) -> XllResult<Arc<V>>
    where
        F: FnOnce() -> XllResult<V>,
        W: FnOnce(&V) -> usize,
    {
        let vkey = VersionedKey { epoch, key };
        if let Some(entry) = self.cache.get(&vkey) {
            return Ok(entry.value);
        }
        let _active = ActiveCacheGuard::enter()?;
        let initialized = self
            .cache
            .try_get_with(vkey.clone(), move || {
                let value = compute()?;
                let measured = weight(&value);
                Ok::<_, XllError>(WeightedValue {
                    value: Arc::new(value),
                    // Moka treats zero as consuming no capacity. Every retained
                    // entry must consume at least one unit so callers cannot
                    // bypass the configured budget accidentally.
                    weight: u32::try_from(measured).unwrap_or(u32::MAX).max(1),
                })
            })
            .map_err(|error| (*error).clone())?;

        self.generation.discard_if_stale(epoch, || {
            self.cache.invalidate(&vkey);
        });
        Ok(initialized.value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::thread;
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
        let cache = Arc::new(CalculationCache::<u32, u32>::new(1024));
        let calls = Arc::new(AtomicUsize::new(0));
        let mut threads = Vec::new();
        for _ in 0..16 {
            let cache = Arc::clone(&cache);
            let calls = Arc::clone(&calls);
            threads.push(thread::spawn(move || {
                cache
                    .get_or_try_insert_with(
                        7,
                        |_| 4,
                        || {
                            calls.fetch_add(1, Ordering::SeqCst);
                            thread::sleep(Duration::from_millis(10));
                            Ok(49)
                        },
                    )
                    .unwrap()
            }));
        }
        for handle in threads {
            assert_eq!(*handle.join().unwrap(), 49);
        }
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
        let cache = Arc::new(CalculationCache::<u32, u32>::new(8));
        let (started_tx, started_rx) = std::sync::mpsc::sync_channel(1);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
        let computing = Arc::clone(&cache);
        let handle = thread::spawn(move || {
            computing
                .get_or_try_insert_with(
                    1,
                    |_| 4,
                    || {
                        started_tx.send(()).unwrap();
                        release_rx.recv().unwrap();
                        Ok(7)
                    },
                )
                .unwrap()
        });
        started_rx.recv().unwrap();
        cache.clear();
        release_tx.send(()).unwrap();
        assert_eq!(*handle.join().unwrap(), 7);
        assert!(cache.get(&1).is_none());
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
            || {
                cache
                    .get_or_try_insert_with(1, |_| 4, || Ok(2))
                    .map(|value| *value)
            },
        );
        assert!(matches!(result, Err(XllError::Internal { .. })));
    }

    #[test]
    fn initialization_of_a_different_key_is_rejected_instead_of_deadlocking() {
        let cache = CalculationCache::<u32, u32>::new(8);
        let result = cache.get_or_try_insert_with(
            1,
            |_| 4,
            || {
                cache
                    .get_or_try_insert_with(2, |_| 4, || Ok(20))
                    .map(|value| *value + 1)
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
        struct ReenterOnDrop {
            cache: std::sync::Weak<CalculationCache<u32, ReenterOnDrop>>,
        }

        impl Drop for ReenterOnDrop {
            fn drop(&mut self) {
                if let Some(cache) = self.cache.upgrade() {
                    assert!(cache.get(&1).is_none());
                }
            }
        }

        let cache = Arc::new(CalculationCache::new(8));
        let value = cache
            .get_or_try_insert_with(
                1,
                |_| 8,
                || {
                    Ok(ReenterOnDrop {
                        cache: Arc::downgrade(&cache),
                    })
                },
            )
            .unwrap();
        drop(value);
        cache.clear();
    }

    #[test]
    fn eviction_drops_values_outside_the_cache_lock() {
        struct ReenterOnDrop {
            cache: std::sync::Weak<CalculationCache<u32, ReenterOnDrop>>,
        }

        impl Drop for ReenterOnDrop {
            fn drop(&mut self) {
                if let Some(cache) = self.cache.upgrade() {
                    let _ = cache.used_weight();
                }
            }
        }

        let cache = Arc::new(CalculationCache::new(8));
        for key in [1, 2] {
            let value = cache
                .get_or_try_insert_with(
                    key,
                    |_| 8,
                    || {
                        Ok(ReenterOnDrop {
                            cache: Arc::downgrade(&cache),
                        })
                    },
                )
                .unwrap();
            drop(value);
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

        let registry = CacheRegistry::new(1024);
        assert_eq!(
            *registry
                .bind(&FIRST)
                .unwrap()
                .get_or_try_insert(1, |_| 4, || Ok(7))
                .unwrap(),
            7
        );
        assert_eq!(
            registry
                .bind(&SECOND)
                .unwrap()
                .get_or_try_insert(1, String::len, || Ok("seven".to_owned()))
                .unwrap()
                .as_str(),
            "seven"
        );
        assert_eq!(registry.endpoint_count(), 2);
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
    fn registry_clear_drops_values_without_holding_the_registry_lock() {
        enum Reentrant {}
        struct ReenterOnDrop {
            registry: std::sync::Weak<CacheRegistry>,
        }
        impl Drop for ReenterOnDrop {
            fn drop(&mut self) {
                if let Some(registry) = self.registry.upgrade() {
                    assert_eq!(registry.endpoint_count(), 1);
                }
            }
        }

        static ENDPOINT: CacheEndpoint<Reentrant, u32, ReenterOnDrop> =
            CacheEndpoint::<Reentrant, u32, ReenterOnDrop>::new("REENTRANT");
        let registry = Arc::new(CacheRegistry::new(8));
        let endpoint = registry.bind(&ENDPOINT).unwrap();
        let value = endpoint
            .get_or_try_insert(
                1,
                |_| 8,
                || {
                    Ok(ReenterOnDrop {
                        registry: Arc::downgrade(&registry),
                    })
                },
            )
            .unwrap();
        drop(value);
        registry.clear();
    }

    #[test]
    fn registry_clear_invalidates_bound_endpoint_values() {
        enum FirstUse {}
        static ENDPOINT: CacheEndpoint<FirstUse, u32, u32> =
            CacheEndpoint::<FirstUse, u32, u32>::new("FIRST_USE");

        let registry = Arc::new(CacheRegistry::new(8));
        let endpoint = registry.bind(&ENDPOINT).unwrap();
        let (started_tx, started_rx) = std::sync::mpsc::sync_channel(1);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
        let worker_endpoint = endpoint.clone();
        let worker = thread::spawn(move || {
            worker_endpoint
                .get_or_try_insert(
                    1,
                    |_| 4,
                    || {
                        started_tx.send(()).unwrap();
                        release_rx.recv().unwrap();
                        Ok(7)
                    },
                )
                .unwrap()
        });

        started_rx.recv().unwrap();
        registry.clear();
        release_tx.send(()).unwrap();

        assert_eq!(*worker.join().unwrap(), 7);
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
        use std::sync::Barrier;

        let cache = Arc::new(CalculationCache::<u32, &'static str>::new(1024));
        let barrier = Arc::new(Barrier::new(2));
        let (started_tx, started_rx) = std::sync::mpsc::channel();

        let cache_a = Arc::clone(&cache);
        let barrier_a = Arc::clone(&barrier);

        // Thread A starts at epoch 0, signals inside compute(), blocks on barrier
        let handle_a = thread::spawn(move || {
            cache_a
                .get_or_try_insert_with(
                    1,
                    |_| 4,
                    || {
                        started_tx.send(()).unwrap();
                        barrier_a.wait();
                        Ok("epoch_0_value")
                    },
                )
                .unwrap()
        });

        // Wait until A is inside compute() at epoch 0
        started_rx.recv().unwrap();

        // Clear while A's computation is in-flight at epoch 0
        cache.clear();

        // Thread B calls get_or_try_insert_with for same key at epoch 1
        let cache_b = Arc::clone(&cache);
        let handle_b = thread::spawn(move || {
            cache_b
                .get_or_try_insert_with(1, |_| 4, || Ok("epoch_1_value"))
                .unwrap()
        });

        // Unblock A
        barrier.wait();

        let val_a = handle_a.join().unwrap();
        let val_b = handle_b.join().unwrap();

        assert_eq!(*val_a, "epoch_0_value");
        assert_eq!(*val_b, "epoch_1_value");
    }

    #[cfg(not(all(target_os = "windows", target_arch = "x86")))]
    #[test]
    fn loom_clear_never_leaves_a_stale_generation_visible() {
        use loom::sync::atomic::{AtomicU64 as LoomAtomicU64, Ordering as LoomOrdering};
        use loom::sync::{Arc as LoomArc, Mutex as LoomMutex};
        use loom::thread as loom_thread;

        struct LoomEpoch(LoomAtomicU64);

        impl EpochAtomic for LoomEpoch {
            fn new(value: u64) -> Self {
                Self(LoomAtomicU64::new(value))
            }

            fn load(&self) -> u64 {
                self.0.load(LoomOrdering::SeqCst)
            }

            fn increment(&self) {
                self.0.fetch_add(1, LoomOrdering::SeqCst);
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
}

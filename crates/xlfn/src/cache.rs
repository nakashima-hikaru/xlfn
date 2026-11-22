//! Typed calculation caches backed by the host-independent service layer.

use crate::{XllError, XllResult};
use std::hash::Hash;
use xlfn_services::cache as services;

pub use services::{CacheBudget, CacheEndpoint, CacheWeightPolicy, CanonicalF64};
#[cfg(feature = "bench-internals")]
pub use services::{CacheReclamationStats, CacheResidentStats};

fn report_drop_panic(context: &'static str, error: &XllError) {
    crate::diagnostics::report_no_unwind(context, error);
}

/// A shared view that pins a cached value until the lease is dropped.
///
/// Eviction and clearing do not invalidate existing leases; the cache owner
/// must remain alive for the lease lifetime.
pub struct CacheLease<'a, V> {
    inner: services::CacheLease<'a, V>,
}

impl<'a, V> CacheLease<'a, V> {
    #[inline]
    fn wrap(inner: services::CacheLease<'a, V>) -> Self {
        Self { inner }
    }
}

// SAFETY: the inner lease is Send under these exact bounds and exclusively
// owns the pin; the facade adds no data or separate synchronization.
unsafe impl<V: Send + Sync> Send for CacheLease<'_, V> {}
// SAFETY: the inner lease allows shared access under V: Sync.
unsafe impl<V: Sync> Sync for CacheLease<'_, V> {}

impl<V> Drop for CacheLease<'_, V> {
    #[inline]
    fn drop(&mut self) {
        // Keep the public Drop contract; the inner lease releases the pin.
    }
}

impl<V> std::ops::Deref for CacheLease<'_, V> {
    type Target = V;

    #[inline]
    fn deref(&self) -> &Self::Target {
        &self.inner
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

/// Owns lazily created endpoint caches for its lifetime.
///
/// Each endpoint follows [`CalculationCache`]'s key and callback contracts.
pub struct CacheRegistry {
    inner: services::CacheRegistry,
}

impl CacheRegistry {
    #[inline]
    #[must_use]
    /// Creates a registry with an independent resident weight budget per endpoint.
    pub fn new(weight_budget_per_endpoint: CacheBudget) -> Self {
        Self {
            inner: services::CacheRegistry::with_reporter(
                weight_budget_per_endpoint,
                report_drop_panic,
            ),
        }
    }

    #[inline]
    /// Resolves an endpoint and returns or initializes a pinned value.
    ///
    /// The endpoint fixes the weight measure for every caller. The
    /// compute, reentry, generation, and retention rules are those of
    /// [`CalculationCache::get_or_try_insert_with`].
    pub fn get_or_try_insert<'a, K, V, Marker, F>(
        &'a self,
        endpoint: &CacheEndpoint<K, V, Marker>,
        key: K,
        compute: F,
    ) -> XllResult<CacheLease<'a, V>>
    where
        Marker: 'static,
        K: Clone + Eq + Hash + Send + Sync + 'static,
        V: Send + Sync + 'static,
        F: FnOnce() -> XllResult<V>,
    {
        self.inner
            .get_or_try_insert(endpoint, key, compute)
            .map(CacheLease::wrap)
    }

    #[inline]
    /// Resolves an endpoint and initializes only after a borrowed resident miss.
    ///
    /// A resident hit does not call [`ToOwned::to_owned`]. Misses use the same
    /// generation snapshot and single-flight rules as the owned-key API.
    pub fn get_or_try_insert_borrowed<'a, K, V, Marker, Q, F>(
        &'a self,
        endpoint: &CacheEndpoint<K, V, Marker>,
        key: &Q,
        compute: F,
    ) -> XllResult<CacheLease<'a, V>>
    where
        Marker: 'static,
        K: std::borrow::Borrow<Q> + Clone + Eq + Hash + Send + Sync + 'static,
        V: Send + Sync + 'static,
        Q: ToOwned<Owned = K> + Hash + Eq + ?Sized,
        F: FnOnce() -> XllResult<V>,
    {
        self.inner
            .get_or_try_insert_borrowed(endpoint, key, compute)
            .map(CacheLease::wrap)
    }

    #[inline]
    /// Resolves an endpoint and looks up a value in its current generation.
    ///
    /// Endpoint resolution can fail; absence is reported as `Ok(None)`.
    pub fn get<'a, K, V, Marker, Q>(
        &'a self,
        endpoint: &CacheEndpoint<K, V, Marker>,
        key: &Q,
    ) -> XllResult<Option<CacheLease<'a, V>>>
    where
        Marker: 'static,
        K: std::borrow::Borrow<Q> + Clone + Eq + Hash + Send + Sync + 'static,
        V: Send + Sync + 'static,
        Q: Hash + Eq + ?Sized,
    {
        self.inner
            .get(endpoint, key)
            .map(|lease| lease.map(CacheLease::wrap))
    }

    #[inline]
    /// Invalidates the currently registered endpoint caches without removing their identities.
    pub fn clear(&self) {
        self.inner.clear();
    }
}

/// A concurrent weighted cache with a bounded resident budget.
///
/// `CalculationCache` provides memoization for expensive computations across
/// UDF invocations. Eviction is based on abstract caller-reported weights rather
/// than physical allocations.
///
/// Keys must keep their equality and hash stable while stored. `Hash`, `Eq`,
/// and `Borrow` on owned or borrowed keys must not reenter this cache, directly
/// or through a registry: they may run while cache locks or read admission are held. `K::Clone` must
/// preserve key identity and must not depend on starting another cache
/// initialization on the same thread, because cloning may occur during
/// publication of an initialized entry.
///
/// Reclamation of removed keys and values runs outside cache coordination
/// locks. Ordinary maintenance also defers their destruction while an
/// initializer is active. Destructors may call a still-live cache, subject to
/// the restrictions of any surrounding key, compute, or weight callback.
pub struct CalculationCache<K, V>
where
    K: Clone + Eq + Hash + Send + Sync + 'static,
    V: Send + Sync + 'static,
{
    inner: services::CalculationCache<K, V>,
}

impl<K, V> Drop for CalculationCache<K, V>
where
    K: Clone + Eq + Hash + Send + Sync + 'static,
    V: Send + Sync + 'static,
{
    #[inline]
    fn drop(&mut self) {
        // Keep the public Drop contract; the inner cache drains its owners.
    }
}

impl<K, V> CalculationCache<K, V>
where
    K: Clone + Eq + Hash + Send + Sync + 'static,
    V: Send + Sync + 'static,
{
    #[inline]
    /// Creates a concurrent weighted cache with a bounded resident budget.
    ///
    /// Every entry has unit weight. Use [`Self::with_weigher`] to fix a different
    /// resource measure at construction. Values heavier than the
    /// configured budget are returned to the caller but are not retained.
    /// One global weight budget allows a single value to use the entire cache.
    /// Cache misses cannot start another cache initialization from inside an
    /// initializer. Existing cached values may still be read normally.
    #[must_use]
    pub fn new(weight_budget: CacheBudget) -> Self {
        Self {
            inner: services::CalculationCache::with_reporter(weight_budget, report_drop_panic),
        }
    }

    #[inline]
    /// Creates a cache whose immutable weigher determines every stored weight.
    ///
    /// The policy must use consistent units for the budget and all values.
    /// A returned zero is normalized to one. The callback may panic, subject
    /// to the same cleanup and reentry rules as initialization.
    #[must_use]
    pub fn with_weigher(weight_budget: CacheBudget, weigher: fn(&V) -> usize) -> Self {
        Self {
            inner: services::CalculationCache::with_weigher_and_reporter(
                weight_budget,
                weigher,
                report_drop_panic,
            ),
        }
    }

    #[inline]
    #[cfg(feature = "bench-internals")]
    #[must_use]
    /// Returns the configured resident weight budget.
    pub const fn weight_budget(&self) -> usize {
        self.inner.weight_budget()
    }

    #[inline]
    /// Observes reclamation without running maintenance. Pending counters
    /// exclude resident nodes and values kept alive by outstanding leases.
    #[cfg(feature = "bench-internals")]
    #[must_use]
    pub fn reclamation_stats(&self) -> CacheReclamationStats {
        self.inner.reclamation_stats()
    }

    #[inline]
    #[cfg(feature = "bench-internals")]
    #[must_use]
    /// Observes resident entry counts, weight, and estimated storage costs.
    pub fn resident_stats(&self) -> CacheResidentStats {
        self.inner.resident_stats()
    }

    #[inline]
    /// Completes index maintenance and the pending reclamation grace period.
    #[cfg(feature = "bench-internals")]
    pub fn maintenance(&self) {
        self.inner.maintenance();
    }

    #[inline]
    #[must_use]
    /// Returns current resident weight after nonblocking maintenance.
    pub fn used_weight(&self) -> usize {
        self.inner.used_weight()
    }

    #[inline]
    #[must_use]
    /// Returns current resident entry count after nonblocking maintenance.
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    #[inline]
    #[must_use]
    /// Returns whether the cache has no resident entries.
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    #[inline]
    /// Invalidates all currently resident entries and advances the cache generation.
    ///
    /// Values held by existing [`CacheLease`] instances remain valid and will be safely
    /// retired and reclaimed when their leases drop. In-flight computations started before
    /// this clear will still complete and return valid leases to their concurrent callers,
    /// but their results will not be published into the new cache generation.
    pub fn clear(&self) {
        self.inner.clear();
    }

    #[inline]
    /// Returns a pinned resident value from the current cache generation.
    ///
    /// For example, a cache with `String` keys accepts both `&str` and `&String`.
    /// Borrowed and owned keys must have equivalent hashes and equality, as
    /// required by [`std::borrow::Borrow`].
    pub fn get<'a, Q>(&'a self, key: &Q) -> Option<CacheLease<'a, V>>
    where
        K: std::borrow::Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.inner.get(key).map(CacheLease::wrap)
    }

    #[inline]
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
    pub fn get_or_try_insert_with<'a, F>(
        &'a self,
        key: K,
        compute: F,
    ) -> XllResult<CacheLease<'a, V>>
    where
        F: FnOnce() -> XllResult<V>,
    {
        self.inner
            .get_or_try_insert_with(key, compute)
            .map(CacheLease::wrap)
    }

    #[inline]
    /// Returns or initializes a value, owning the key only after a resident miss.
    ///
    /// Resident hits and requests joining an already registered computation do
    /// not call [`ToOwned::to_owned`]. If no flight exists, the caller owns the
    /// key outside the coordination lock and rechecks before registering.
    /// The initial lookup and initialization share one generation snapshot, so
    /// `clear()` during `to_owned()` cannot promote this request to a new generation.
    /// Panics and reentry follow [`Self::get_or_try_insert_with`]; `to_owned()`
    /// must preserve key identity according to [`std::borrow::Borrow`].
    pub fn get_or_try_insert_borrowed<'a, Q, F>(
        &'a self,
        key: &Q,
        compute: F,
    ) -> XllResult<CacheLease<'a, V>>
    where
        K: std::borrow::Borrow<Q>,
        Q: ToOwned<Owned = K> + Hash + Eq + ?Sized,
        F: FnOnce() -> XllResult<V>,
    {
        self.inner
            .get_or_try_insert_borrowed(key, compute)
            .map(CacheLease::wrap)
    }
}

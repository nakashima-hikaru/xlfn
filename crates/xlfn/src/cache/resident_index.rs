//! Native resident policy. Node ownership and reclamation remain in xlfn.
//!
//! Insertion transfers one residency obligation. Every removal or rejection
//! notifies exactly once. Copied entries are non-owning; dereferencing them
//! requires the caller's existing lookup-domain admission or creator pin.

use super::{NodePtr, VersionedKey, VersionedKeyRef};
use crate::{XllError, XllResult};
use quick_cache::{Equivalent, Lifecycle, Weighter, sync::Cache};
use smallvec::SmallVec;
use std::{hash::Hash, sync::Arc};

type Entry<V> = (NodePtr<V>, u32);
type Removed<V> = Arc<dyn Fn(Entry<V>) + Send + Sync>;

impl<K: Eq> Equivalent<VersionedKey<K>> for VersionedKeyRef<'_, K> {
    fn equivalent(&self, owned: &VersionedKey<K>) -> bool {
        self.epoch == owned.epoch && self.key == &owned.key
    }
}

#[derive(Clone)]
struct EntryWeight;
impl<K, V> Weighter<VersionedKey<K>, Entry<V>> for EntryWeight {
    fn weight(&self, _: &VersionedKey<K>, entry: &Entry<V>) -> u64 {
        u64::from(entry.1)
    }
}

struct RetirementLifecycle<V>(Removed<V>);
impl<V> Clone for RetirementLifecycle<V> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

struct Evicted<K, V> {
    entries: SmallVec<[(VersionedKey<K>, Entry<V>); 2]>,
    removed: Option<Removed<V>>,
}
impl<K, V> Default for Evicted<K, V> {
    fn default() -> Self {
        Self {
            entries: SmallVec::new(),
            removed: None,
        }
    }
}
impl<K, V> Drop for Evicted<K, V> {
    fn drop(&mut self) {
        if let Some(removed) = &self.removed {
            for (_, entry) in self.entries.drain(..) {
                removed(entry);
            }
        }
    }
}
impl<K, V> Lifecycle<VersionedKey<K>, Entry<V>> for RetirementLifecycle<V> {
    type RequestState = Evicted<K, V>;
    fn on_evict(&self, state: &mut Self::RequestState, key: VersionedKey<K>, entry: Entry<V>) {
        // RequestState is dropped after the native lock is released. The hook
        // only records ownership; even key destructors stay outside the lock.
        state.removed.get_or_insert_with(|| Arc::clone(&self.0));
        state.entries.push((key, entry));
    }
}

type NativeCache<K, V> = Cache<
    VersionedKey<K>,
    Entry<V>,
    EntryWeight,
    quick_cache::DefaultHashBuilder,
    RetirementLifecycle<V>,
>;

pub(super) struct ResidentIndex<K, V> {
    cache: NativeCache<K, V>,
    removed: Removed<V>,
}

impl<K, V> ResidentIndex<K, V>
where
    K: Clone + Eq + Hash + Send + Sync + 'static,
    V: Send + Sync + 'static,
{
    pub(super) fn new(capacity: u64, removed: impl Fn(Entry<V>) + Send + Sync + 'static) -> Self {
        let removed: Removed<V> = Arc::new(removed);
        Self {
            cache: NativeCache::with_options(
                // Native multi-shard budgets round up and cannot share slack.
                // One shard enforces the exact global budget without xlfn quotas.
                quick_cache::OptionsBuilder::new()
                    .shards(1)
                    .estimated_items_capacity(
                        usize::try_from(capacity).expect("capacity came from usize"),
                    )
                    .weight_capacity(capacity)
                    .build()
                    .expect("valid cache capacity"),
                EntryWeight,
                Default::default(),
                RetirementLifecycle(Arc::clone(&removed)),
            ),
            removed,
        }
    }

    #[inline]
    pub(super) fn get(&self, key: &VersionedKeyRef<'_, K>) -> Option<Entry<V>> {
        self.cache.get(key)
    }

    /// The caller owns the per-key flight. Recheck residency after acquiring
    /// leadership, then initialize outside every native cache lock.
    pub(super) fn insert(
        &self,
        key: VersionedKey<K>,
        initialize: impl FnOnce() -> XllResult<Entry<V>>,
    ) -> Result<Entry<V>, Arc<XllError>> {
        if let Some(entry) = self.get(&VersionedKeyRef {
            epoch: key.epoch,
            key: &key.key,
        }) {
            return Ok(entry);
        }
        let entry = initialize().map_err(Arc::new)?;
        // Rejection uses the same lifecycle callback: the creator pin still
        // protects the returned entry while its residency obligation retires.
        self.cache.insert(key, entry);
        Ok(entry)
    }

    pub(super) fn invalidate(&self, key: &VersionedKey<K>) {
        if let Some((_, entry)) = self.cache.remove(key) {
            (self.removed)(entry);
        }
    }

    pub(super) fn invalidate_before(&self, epoch: u64) {
        // retain/clear do not notify Lifecycle. Remove returns entries after
        // unlocking; generation already handles late old initializers.
        for (key, _) in self.cache.iter() {
            if key.epoch < epoch {
                self.invalidate(&key);
            }
        }
    }

    pub(super) fn clear(&self) {
        for (_, entry) in self.cache.drain() {
            (self.removed)(entry);
        }
    }

    pub(super) fn resident_count(&self) -> u64 {
        self.cache.len() as u64
    }
    pub(super) fn resident_weight(&self) -> u64 {
        self.cache.weight()
    }
}

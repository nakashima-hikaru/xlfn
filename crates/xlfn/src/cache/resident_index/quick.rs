//! Quick Cache resident policy. Only stored values own a residency obligation.
//! Cloned lookup/placeholder results are non-owning snapshots, as with Moka.

use super::{Entry, VersionedKey, VersionedKeyRef};
use crate::cache::{NodePtr, retire_resident};
use crate::{XllError, XllResult};
use quick_cache::{
    OptionsBuilder, Weighter,
    sync::{Cache, DefaultLifecycle, GuardResult},
};
use std::collections::hash_map::RandomState;
use std::hash::Hash;
use std::sync::Arc;

// Flat fields preserve the existing pointer + weight entry size on 64-bit hosts.
struct ResidentEntry<V> {
    node: NodePtr<V>,
    weight: u32,
    owns_residency: bool,
}

impl<V> ResidentEntry<V> {
    fn new((node, weight): Entry<V>) -> Self {
        Self {
            node,
            weight,
            owns_residency: true,
        }
    }

    fn snapshot(&self) -> Entry<V> {
        (self.node, self.weight)
    }
}

impl<V> Clone for ResidentEntry<V> {
    fn clone(&self) -> Self {
        Self {
            node: self.node,
            weight: self.weight,
            owns_residency: false,
        }
    }
}

impl<V> Drop for ResidentEntry<V> {
    fn drop(&mut self) {
        if self.owns_residency {
            // Release only index ownership, never the payload. The cache's
            // grace-period machinery destroys values after index locks drop.
            retire_resident(self.node);
        }
    }
}

#[derive(Clone)]
struct EntryWeight;

impl<K, V> Weighter<K, ResidentEntry<V>> for EntryWeight {
    fn weight(&self, _: &K, value: &ResidentEntry<V>) -> u64 {
        u64::from(value.weight)
    }
}

pub(super) struct QuickResidentIndex<K, V> {
    cache: Cache<VersionedKey<K>, ResidentEntry<V>, EntryWeight, RandomState>,
}

impl<K, V> QuickResidentIndex<K, V>
where
    K: Clone + Eq + Hash + Send + Sync + 'static,
    V: Send + Sync + 'static,
{
    pub(super) fn new(capacity: u64, requested_shards: usize) -> Self {
        // Weight is bytes, not an item count: never preallocate the entire
        // byte budget as entries. Match Quick Cache's minimum shard size.
        let estimated = usize::try_from(capacity)
            .unwrap_or(usize::MAX)
            .clamp(1, 1024);
        let mut shards = requested_shards.max(1).next_power_of_two();
        while shards > 1 && estimated.div_ceil(shards) < 32 {
            shards /= 2;
        }
        // Quick Cache rounds each shard capacity up. Round the total down
        // first so aggregate residency cannot exceed the caller's budget.
        let capacity = capacity / shards as u64 * shards as u64;
        let options = OptionsBuilder::new()
            .shards(shards)
            .estimated_items_capacity(estimated)
            .weight_capacity(capacity)
            // The default 97% hot quota rejects a single full-budget entry,
            // even with one shard. Preserve eligibility up to shard capacity.
            .hot_allocation(1.0)
            .build()
            .expect("valid resident cache options");
        Self {
            // Use the same hasher family as the benchmark Moka control.
            cache: Cache::with_options(
                options,
                EntryWeight,
                RandomState::new(),
                DefaultLifecycle::default(),
            ),
        }
    }

    pub(super) fn get(&self, key: &VersionedKeyRef<'_, K>) -> Option<Entry<V>> {
        self.cache.get(key).map(|entry| entry.snapshot())
    }

    pub(super) fn insert(
        &self,
        key: &VersionedKey<K>,
        initialize: impl FnOnce() -> XllResult<Entry<V>>,
    ) -> Result<Entry<V>, Arc<XllError>> {
        match self.cache.get_value_or_guard(key, None) {
            GuardResult::Value(entry) => Ok(entry.snapshot()),
            GuardResult::Guard(guard) => {
                let entry = initialize().map_err(Arc::new)?;
                // Move the sole residency owner into the table. The generic
                // get_or_insert_with API clones before storing and therefore
                // must not be used with non-owning lookup snapshots.
                // If invalidation removed the placeholder, dropping the
                // rejected owner releases residency; the creator pin remains.
                drop(guard.insert(ResidentEntry::new(entry)));
                Ok(entry)
            }
            GuardResult::Timeout => unreachable!("no timeout was requested"),
        }
    }

    pub(super) fn invalidate(&self, key: &VersionedKey<K>) {
        drop(self.cache.remove(key));
    }

    pub(super) fn invalidate_before(&self, epoch: u64) {
        self.cache.retain(|key, _| key.epoch >= epoch);
    }

    pub(super) fn clear(&self) {
        self.cache.clear();
    }

    pub(super) fn resident_count(&self) -> u64 {
        self.cache.len() as u64
    }

    pub(super) fn resident_weight(&self) -> u64 {
        self.cache.weight()
    }

    #[cfg(feature = "bench-internals")]
    pub(super) fn estimated_index_bytes(&self) -> usize {
        self.cache.memory_used().total()
    }
}

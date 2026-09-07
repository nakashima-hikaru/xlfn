//! Resident-set policy boundary. Nodes and their reclamation belong to xlfn.
//!
//! Each successful insertion transfers exactly one residency obligation to
//! the index. Removing/rejecting/replacing that entry must notify exactly once.
//! Copied entries are non-owning: callers must use their lookup domain before
//! dereferencing them. Notifications never reclaim nodes themselves.

use super::{NodePtr, VersionedKey, VersionedKeyRef};
use crate::{XllError, XllResult};
use moka::{Equivalent, sync::Cache};
use std::hash::Hash;
use std::sync::Arc;

#[cfg(feature = "bench-internals")]
mod sharded;
#[cfg(feature = "bench-internals")]
use sharded::ShardedResidentIndex;

type Entry<V> = (NodePtr<V>, u32);

impl<K: Eq> Equivalent<VersionedKey<K>> for VersionedKeyRef<'_, K> {
    fn equivalent(&self, owned: &VersionedKey<K>) -> bool {
        self.epoch == owned.epoch && self.key == &owned.key
    }
}

/// Production has one variant, stored inline: no runtime backend selection.
/// Release codegen is checked against direct Moka calls in the experiment notes.
/// Qualification tests/benchmarks opt into candidates via `bench-internals`.
pub(super) struct ResidentIndex<K, V>(Backend<K, V>);

enum Backend<K, V> {
    Moka(MokaResidentIndex<K, V>),
    #[cfg(feature = "bench-internals")]
    Sharded(Box<ShardedResidentIndex<K, V>>),
}

struct MokaResidentIndex<K, V> {
    cache: Cache<VersionedKey<K>, Entry<V>>,
}

impl<K, V> ResidentIndex<K, V>
where
    K: Clone + Eq + Hash + Send + Sync + 'static,
    V: Send + Sync + 'static,
{
    pub(super) fn moka(capacity: u64, removed: impl Fn(Entry<V>) + Send + Sync + 'static) -> Self {
        Self(Backend::Moka(MokaResidentIndex {
            cache: Cache::builder()
                .max_capacity(capacity)
                .weigher(|_, entry: &Entry<V>| entry.1)
                .support_invalidation_closures()
                .eviction_listener(move |_, entry, _| removed(entry))
                .build(),
        }))
    }

    #[cfg(feature = "bench-internals")]
    pub(super) fn sharded(
        capacity: u64,
        shards: usize,
        removed: impl Fn(Entry<V>) + Send + Sync + 'static,
    ) -> Self {
        Self(Backend::Sharded(Box::new(ShardedResidentIndex::new(
            capacity, shards, removed,
        ))))
    }

    #[cfg(feature = "bench-internals")]
    pub(super) fn memory_estimate(&self) -> (usize, bool) {
        match &self.0 {
            Backend::Moka(_) => (
                std::mem::size_of::<MokaResidentIndex<K, V>>()
                    + self.resident_count() as usize
                        * std::mem::size_of::<(VersionedKey<K>, Entry<V>)>(),
                true,
            ),
            Backend::Sharded(index) => (index.estimated_index_bytes(), false),
        }
    }

    #[inline]
    pub(super) fn get(&self, key: &VersionedKeyRef<'_, K>) -> Option<Entry<V>> {
        match &self.0 {
            Backend::Moka(index) => index.cache.get(key),
            #[cfg(feature = "bench-internals")]
            Backend::Sharded(index) => index.get(key),
        }
    }

    /// Atomically initializes an absent versioned key. Only the initializer
    /// owns its creator pin; followers must re-lookup under their read domain.
    pub(super) fn insert(
        &self,
        key: VersionedKey<K>,
        initialize: impl FnOnce() -> XllResult<Entry<V>>,
    ) -> Result<Entry<V>, Arc<XllError>> {
        match &self.0 {
            Backend::Moka(index) => index.cache.try_get_with(key, initialize),
            #[cfg(feature = "bench-internals")]
            Backend::Sharded(index) => index.insert(key, initialize),
        }
    }

    pub(super) fn invalidate(&self, key: &VersionedKey<K>) {
        match &self.0 {
            Backend::Moka(index) => index.cache.invalidate(key),
            #[cfg(feature = "bench-internals")]
            Backend::Sharded(index) => index.invalidate(key),
        }
    }

    pub(super) fn invalidate_before(&self, epoch: u64) {
        match &self.0 {
            Backend::Moka(index) => {
                index
                    .cache
                    .invalidate_entries_if(move |key, _| key.epoch < epoch)
                    .expect("invalidation closures are enabled");
            }
            #[cfg(feature = "bench-internals")]
            Backend::Sharded(index) => index.invalidate_before(epoch),
        }
    }

    pub(super) fn maintenance(&self) {
        match &self.0 {
            Backend::Moka(index) => index.cache.run_pending_tasks(),
            #[cfg(feature = "bench-internals")]
            Backend::Sharded(_) => {}
        }
    }

    pub(super) fn clear(&self) {
        match &self.0 {
            Backend::Moka(index) => index.cache.invalidate_all(),
            #[cfg(feature = "bench-internals")]
            Backend::Sharded(index) => index.clear(),
        }
    }

    pub(super) fn resident_count(&self) -> u64 {
        match &self.0 {
            Backend::Moka(index) => index.cache.entry_count(),
            #[cfg(feature = "bench-internals")]
            Backend::Sharded(index) => index.resident_count(),
        }
    }

    pub(super) fn resident_weight(&self) -> u64 {
        match &self.0 {
            Backend::Moka(index) => index.cache.weighted_size(),
            #[cfg(feature = "bench-internals")]
            Backend::Sharded(index) => index.resident_weight(),
        }
    }
}

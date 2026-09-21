//! Resident-set policy boundary. Nodes and their reclamation belong to xlfn.
//!
//! Each successful insertion transfers exactly one residency obligation to
//! the index. Removing/rejecting/replacing that entry must notify exactly once.
//! Copied entries are non-owning: callers must use their lookup domain before
//! dereferencing them. Notifications never reclaim nodes themselves.

use super::{NodePtr, VersionedKey, VersionedKeyRef, retire_resident};
#[cfg(feature = "bench-internals")]
use moka::sync::Cache;
use quick_cache::Equivalent;
use std::hash::Hash;
#[cfg(feature = "bench-internals")]
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
#[cfg(feature = "bench-internals")]
use std::sync::atomic::{AtomicUsize, Ordering};

mod quick;
#[cfg(feature = "bench-internals")]
mod sharded;
use quick::QuickResidentIndex;
#[cfg(feature = "bench-internals")]
use sharded::ShardedResidentIndex;

type Entry<V> = (NodePtr<V>, u64);

/// Exactly one stored value owns the resident pin. Lookup clones are snapshots.
/// Ownership begins before calling any user key Clone/Hash/Eq implementation.
/// Moving this value into an index transfers the pin even if insertion unwinds.
pub(super) struct ResidentEntry<V> {
    node: NodePtr<V>,
    weight: u64,
    owns_residency: AtomicBool,
}

impl<V> ResidentEntry<V> {
    pub(super) fn new((node, weight): Entry<V>) -> Self {
        Self {
            node,
            weight,
            owns_residency: AtomicBool::new(true),
        }
    }

    fn snapshot(&self) -> Entry<V> {
        (self.node, self.weight)
    }

    // Moka clones values before storing them. Its values therefore share one
    // owner through Arc, and eviction notification discharges that owner once.
    #[cfg(feature = "bench-internals")]
    fn retire_shared(&self) {
        if self.owns_residency.swap(false, Ordering::Relaxed) {
            retire_resident(self.node);
        }
    }

    fn retire(&mut self) {
        if *self.owns_residency.get_mut() {
            *self.owns_residency.get_mut() = false;
            // This releases only residency; value destruction follows grace.
            retire_resident(self.node);
        }
    }
}

impl<V> Clone for ResidentEntry<V> {
    fn clone(&self) -> Self {
        Self {
            node: self.node,
            weight: self.weight,
            owns_residency: AtomicBool::new(false),
        }
    }
}

impl<V> Drop for ResidentEntry<V> {
    fn drop(&mut self) {
        self.retire();
    }
}

impl<K: Eq> Equivalent<VersionedKey<K>> for VersionedKeyRef<'_, K> {
    fn equivalent(&self, owned: &VersionedKey<K>) -> bool {
        self.epoch == owned.epoch && self.key == &owned.key
    }
}

/// Production has one variant, stored inline: no runtime backend selection.
/// Qualification tests/benchmarks opt into candidates via `bench-internals`.
pub(super) struct ResidentIndex<K, V>(Backend<K, V>);

enum Backend<K, V> {
    #[cfg(feature = "bench-internals")]
    Moka(MokaResidentIndex<K, V>),
    #[cfg(feature = "bench-internals")]
    Sharded(Box<ShardedResidentIndex<K, V>>),
    Quick(QuickResidentIndex<K, V>),
}

#[cfg(feature = "bench-internals")]
struct MokaResidentIndex<K, V> {
    cache: Cache<VersionedKey<K>, Arc<ResidentEntry<V>>>,
    mutations: AtomicUsize,
}

impl<K, V> ResidentIndex<K, V>
where
    K: Clone + Eq + Hash + Send + Sync + 'static,
    V: Send + Sync + 'static,
{
    #[cfg(feature = "bench-internals")]
    pub(super) fn moka(capacity: u64, after_eviction: impl Fn() + Send + Sync + 'static) -> Self {
        Self(Backend::Moka(MokaResidentIndex {
            cache: Cache::builder()
                .max_capacity(capacity)
                .weigher(|_, entry: &Arc<ResidentEntry<V>>| {
                    u32::try_from(entry.weight).unwrap_or(u32::MAX)
                })
                .support_invalidation_closures()
                .eviction_listener(move |_, entry, _| {
                    entry.retire_shared();
                    after_eviction();
                })
                .build(),
            mutations: AtomicUsize::new(0),
        }))
    }

    #[cfg(feature = "bench-internals")]
    pub(super) fn sharded(capacity: u64, shards: usize) -> Self {
        Self(Backend::Sharded(Box::new(ShardedResidentIndex::new(
            capacity, shards,
        ))))
    }

    pub(super) fn quick(capacity: u64, shards: usize) -> Self {
        Self(Backend::Quick(QuickResidentIndex::new(capacity, shards)))
    }

    #[cfg(feature = "bench-internals")]
    pub(super) fn memory_estimate(&self) -> (usize, bool) {
        match &self.0 {
            Backend::Moka(_) => (
                std::mem::size_of::<MokaResidentIndex<K, V>>()
                    + self.resident_count() as usize
                        * (std::mem::size_of::<(VersionedKey<K>, Arc<ResidentEntry<V>>)>()
                            + std::mem::size_of::<ResidentEntry<V>>()
                            + 2 * std::mem::size_of::<usize>()),
                true,
            ),
            Backend::Sharded(index) => (index.estimated_index_bytes(), false),
            Backend::Quick(index) => (index.estimated_index_bytes(), false),
        }
    }

    #[inline]
    pub(super) fn get(&self, key: &VersionedKeyRef<'_, K>) -> Option<Entry<V>> {
        match &self.0 {
            #[cfg(feature = "bench-internals")]
            Backend::Moka(index) => index.cache.get(key).map(|entry| entry.snapshot()),
            #[cfg(feature = "bench-internals")]
            Backend::Sharded(index) => index.get(key),
            Backend::Quick(index) => index.get(key),
        }
    }

    /// Stores an initialized entry in the resident index.
    pub(super) fn insert_resident(&self, key: &VersionedKey<K>, entry: ResidentEntry<V>) {
        match &self.0 {
            #[cfg(feature = "bench-internals")]
            Backend::Moka(index) => {
                index.cache.insert(key.clone(), Arc::new(entry));
            }
            #[cfg(feature = "bench-internals")]
            Backend::Sharded(index) => {
                index.publish(key.clone(), entry);
            }
            Backend::Quick(index) => {
                index.insert_resident(key, entry);
            }
        }
    }

    pub(super) fn invalidate(&self, key: &VersionedKey<K>) {
        match &self.0 {
            #[cfg(feature = "bench-internals")]
            Backend::Moka(index) => index.cache.invalidate(key),
            #[cfg(feature = "bench-internals")]
            Backend::Sharded(index) => index.invalidate(key),
            Backend::Quick(index) => index.invalidate(key),
        }
    }

    pub(super) fn invalidate_before(&self, epoch: u64) {
        match &self.0 {
            #[cfg(feature = "bench-internals")]
            Backend::Moka(index) => {
                index
                    .cache
                    .invalidate_entries_if(move |key, _| key.epoch < epoch)
                    .expect("invalidation closures are enabled");
            }
            #[cfg(feature = "bench-internals")]
            Backend::Sharded(index) => index.invalidate_before(epoch),
            Backend::Quick(index) => index.invalidate_before(epoch),
        }
    }

    pub(super) fn maintenance(&self) {
        match &self.0 {
            #[cfg(feature = "bench-internals")]
            Backend::Moka(index) => index.cache.run_pending_tasks(),
            #[cfg(feature = "bench-internals")]
            Backend::Sharded(_) => {}
            Backend::Quick(_) => {}
        }
    }

    #[inline]
    pub(super) fn maintenance_after_mutation(&self) {
        // Only the benchmark Moka control needs periodic background policy
        // maintenance. Synchronous policies must not pay for its shared RMW.
        #[cfg(feature = "bench-internals")]
        if let Backend::Moka(index) = &self.0 {
            const MAINTENANCE_INTERVAL: usize = 32;
            if index.mutations.fetch_add(1, Ordering::Relaxed) % MAINTENANCE_INTERVAL
                == MAINTENANCE_INTERVAL - 1
            {
                index.cache.run_pending_tasks();
            }
        }
    }

    pub(super) fn clear(&self) {
        match &self.0 {
            #[cfg(feature = "bench-internals")]
            Backend::Moka(index) => {
                index.cache.invalidate_all();
                // Moka may time-limit eviction; exclusive cache Drop must
                // release every residency obligation before draining nodes.
                loop {
                    index.cache.run_pending_tasks();
                    if index.cache.entry_count() == 0 {
                        break;
                    }
                }
            }
            #[cfg(feature = "bench-internals")]
            Backend::Sharded(index) => index.clear(),
            Backend::Quick(index) => index.clear(),
        }
    }

    pub(super) fn resident_count(&self) -> u64 {
        match &self.0 {
            #[cfg(feature = "bench-internals")]
            Backend::Moka(index) => index.cache.entry_count(),
            #[cfg(feature = "bench-internals")]
            Backend::Sharded(index) => index.resident_count(),
            Backend::Quick(index) => index.resident_count(),
        }
    }

    pub(super) fn resident_weight(&self) -> u64 {
        match &self.0 {
            #[cfg(feature = "bench-internals")]
            Backend::Moka(index) => index.cache.weighted_size(),
            #[cfg(feature = "bench-internals")]
            Backend::Sharded(index) => index.resident_weight(),
            Backend::Quick(index) => index.resident_weight(),
        }
    }
}

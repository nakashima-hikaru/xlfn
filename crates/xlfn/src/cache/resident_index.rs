//! Resident-set policy boundary. Nodes and their reclamation belong to xlfn.
//!
//! Each successful insertion transfers exactly one residency obligation to
//! the index. Removing/rejecting/replacing that entry must notify exactly once.
//! Copied entries are non-owning: callers must use their lookup domain before
//! dereferencing them. Notifications never reclaim nodes themselves.

use super::{NodePtr, VersionedKey, VersionedKeyRef, retire_resident};
use quick_cache::Equivalent;
use std::hash::Hash;
use std::sync::atomic::AtomicBool;

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
    Sharded(Box<ShardedResidentIndex<K, V>>),
    Quick(QuickResidentIndex<K, V>),
}

impl<K, V> ResidentIndex<K, V>
where
    K: Clone + Eq + Hash + Send + Sync + 'static,
    V: Send + Sync + 'static,
{
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
            Backend::Sharded(index) => (index.estimated_index_bytes(), false),
            Backend::Quick(index) => (index.estimated_index_bytes(), false),
        }
    }

    #[inline]
    pub(super) fn get(&self, key: &VersionedKeyRef<'_, K>) -> Option<Entry<V>> {
        match &self.0 {
            #[cfg(feature = "bench-internals")]
            Backend::Sharded(index) => index.get(key),
            Backend::Quick(index) => index.get(key),
        }
    }

    /// Stores an initialized entry in the resident index.
    pub(super) fn insert_resident(&self, key: &VersionedKey<K>, entry: ResidentEntry<V>) {
        match &self.0 {
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
            Backend::Sharded(index) => index.invalidate(key),
            Backend::Quick(index) => index.invalidate(key),
        }
    }

    pub(super) fn invalidate_before(&self, epoch: u64) {
        match &self.0 {
            #[cfg(feature = "bench-internals")]
            Backend::Sharded(index) => index.invalidate_before(epoch),
            Backend::Quick(index) => index.invalidate_before(epoch),
        }
    }

    pub(super) fn maintenance(&self) {}

    #[inline]
    pub(super) fn maintenance_after_mutation(&self) {}

    pub(super) fn clear(&self) {
        match &self.0 {
            #[cfg(feature = "bench-internals")]
            Backend::Sharded(index) => index.clear(),
            Backend::Quick(index) => index.clear(),
        }
    }

    pub(super) fn resident_count(&self) -> u64 {
        match &self.0 {
            #[cfg(feature = "bench-internals")]
            Backend::Sharded(index) => index.resident_count(),
            Backend::Quick(index) => index.resident_count(),
        }
    }

    pub(super) fn resident_weight(&self) -> u64 {
        match &self.0 {
            #[cfg(feature = "bench-internals")]
            Backend::Sharded(index) => index.resident_weight(),
            Backend::Quick(index) => index.resident_weight(),
        }
    }
}

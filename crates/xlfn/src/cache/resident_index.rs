//! Resident-set policy boundary. Nodes and their reclamation belong to xlfn.
//!
//! Each successful insertion transfers exactly one residency obligation to
//! the index. Removing/rejecting/replacing that entry must notify exactly once.
//! Copied entries are non-owning: callers must use their lookup domain before
//! dereferencing them. Notifications never reclaim nodes themselves.

use super::{NodePtr, VersionedKey, VersionedKeyRef, retire_resident};
use quick_cache::Equivalent;
use std::borrow::Borrow;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

mod key_retirement;
use key_retirement::RetiredKeys;

mod quick;
#[cfg(feature = "bench-internals")]
mod sharded;
use quick::QuickResidentIndex;
#[cfg(feature = "bench-internals")]
use sharded::ShardedResidentIndex;

type Entry<V> = (NodePtr<V>, u64);

/// Index removal transfers the key to a queue; it never runs user Drop code.
/// The queue owner outlives the backend, including backend destruction.
struct ResidentKey<K> {
    key: Option<VersionedKey<K>>,
    retired: Option<Arc<RetiredKeys<K>>>,
}

impl<K> ResidentKey<K> {
    fn get(&self) -> &VersionedKey<K> {
        self.key.as_ref().expect("resident key owns its payload")
    }
}

impl<K: Clone> Clone for ResidentKey<K> {
    fn clone(&self) -> Self {
        Self {
            key: Some(self.get().clone()),
            retired: self.retired.clone(),
        }
    }
}

impl<K: Hash> Hash for ResidentKey<K> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.get().hash(state);
    }
}

impl<K: PartialEq> PartialEq for ResidentKey<K> {
    fn eq(&self, other: &Self) -> bool {
        self.get() == other.get()
    }
}

impl<K: Eq> Eq for ResidentKey<K> {}

impl<K> Borrow<VersionedKey<K>> for ResidentKey<K> {
    fn borrow(&self) -> &VersionedKey<K> {
        self.get()
    }
}

impl<K: Eq> Equivalent<ResidentKey<K>> for VersionedKeyRef<'_, K> {
    fn equivalent(&self, owned: &ResidentKey<K>) -> bool {
        self.equivalent(owned.get())
    }
}

impl<K> Drop for ResidentKey<K> {
    fn drop(&mut self) {
        if let Some(retired) = &self.retired {
            retired.push(self.key.take().expect("resident key owns its payload"));
        }
    }
}

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
pub(super) struct ResidentIndex<K, V> {
    // Field order keeps the queue alive until every backend key is retired.
    backend: Backend<K, V>,
    retired_keys: Option<Arc<RetiredKeys<K>>>,
}

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
        Self {
            backend: Backend::Sharded(Box::new(ShardedResidentIndex::new(capacity, shards))),
            retired_keys: std::mem::needs_drop::<K>().then(|| Arc::new(RetiredKeys::new())),
        }
    }

    pub(super) fn quick(capacity: u64, shards: usize) -> Self {
        Self {
            backend: Backend::Quick(QuickResidentIndex::new(capacity, shards)),
            retired_keys: std::mem::needs_drop::<K>().then(|| Arc::new(RetiredKeys::new())),
        }
    }

    #[cfg(feature = "bench-internals")]
    pub(super) fn memory_estimate(&self) -> (usize, bool) {
        match &self.backend {
            Backend::Sharded(index) => (index.estimated_index_bytes(), false),
            Backend::Quick(index) => (index.estimated_index_bytes(), false),
        }
    }

    #[inline]
    pub(super) fn get(&self, key: &VersionedKeyRef<'_, K>) -> Option<Entry<V>> {
        match &self.backend {
            #[cfg(feature = "bench-internals")]
            Backend::Sharded(index) => index.get(&self.own_key(VersionedKey {
                epoch: key.epoch,
                key: key.key.clone(),
            })),
            Backend::Quick(index) => index.get(key),
        }
    }

    /// Stores an initialized entry in the resident index.
    pub(super) fn insert_resident(&self, key: &VersionedKey<K>, entry: ResidentEntry<V>) {
        let key = self.own_key(key.clone());
        match &self.backend {
            #[cfg(feature = "bench-internals")]
            Backend::Sharded(index) => {
                index.publish(key, entry);
            }
            Backend::Quick(index) => {
                index.insert_resident(key, entry);
            }
        }
    }

    fn own_key(&self, key: VersionedKey<K>) -> ResidentKey<K> {
        ResidentKey {
            key: Some(key),
            retired: self.retired_keys.clone(),
        }
    }

    pub(super) fn invalidate(&self, key: &VersionedKey<K>) {
        match &self.backend {
            #[cfg(feature = "bench-internals")]
            Backend::Sharded(index) => index.invalidate(key),
            Backend::Quick(index) => index.invalidate(key),
        }
    }

    pub(super) fn invalidate_before(&self, epoch: u64) {
        match &self.backend {
            #[cfg(feature = "bench-internals")]
            Backend::Sharded(index) => index.invalidate_before(epoch),
            Backend::Quick(index) => index.invalidate_before(epoch),
        }
    }

    /// Runs key destructors only after the caller has left all cache locks.
    pub(super) fn maintenance(&self) {
        if super::ACTIVE_CACHE_INITIALIZATION_DEPTH.get() == 0
            && let Some(retired) = &self.retired_keys
        {
            retired.reclaim();
        }
    }

    pub(super) fn has_retired_keys(&self) -> bool {
        self.retired_keys
            .as_ref()
            .is_some_and(|retired| retired.has_pending())
    }

    pub(super) fn clear(&self) {
        match &self.backend {
            #[cfg(feature = "bench-internals")]
            Backend::Sharded(index) => index.clear(),
            Backend::Quick(index) => index.clear(),
        }
    }

    pub(super) fn resident_count(&self) -> u64 {
        match &self.backend {
            #[cfg(feature = "bench-internals")]
            Backend::Sharded(index) => index.resident_count(),
            Backend::Quick(index) => index.resident_count(),
        }
    }

    pub(super) fn resident_weight(&self) -> u64 {
        match &self.backend {
            #[cfg(feature = "bench-internals")]
            Backend::Sharded(index) => index.resident_weight(),
            Backend::Quick(index) => index.resident_weight(),
        }
    }
}

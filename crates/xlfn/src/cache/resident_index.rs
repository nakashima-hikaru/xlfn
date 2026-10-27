//! Resident-set policy boundary. Nodes and their reclamation belong to xlfn.
//!
//! Each successful insertion transfers exactly one residency obligation to
//! the index. Removing/rejecting/replacing that entry must notify exactly once.
//! Copied entries are non-owning: callers must use their lookup domain before
//! dereferencing them. Notifications never reclaim nodes themselves.

use super::{CacheNode, NodePtr, VersionedKey, VersionedKeyRef, retire_resident};
use quick_cache::Equivalent;
use std::borrow::Borrow;
use std::hash::{Hash, Hasher};
use std::ptr::NonNull;
use xlfn_kernel::published_owner::PublishedOwner;

mod key_retirement;
use key_retirement::RetiredKeys;

mod quick;
use quick::QuickResidentIndex;

#[cfg(test)]
mod tests;

type Entry<V> = (NodePtr<V>, u64);

/// Index removal transfers the key to a queue; it never runs user Drop code.
/// The queue owner outlives the backend, including backend destruction. This
/// private wrapper is neither cloned nor returned from the backend boundary.
struct ResidentKey<K> {
    key: Option<VersionedKey<K>>,
    retired: Option<NonNull<RetiredKeys<K>>>,
}

// SAFETY: the containing ResidentIndex retains the stable queue allocation until
// its backend has dropped every key. Transferring K and enqueueing it into the
// queue's mutex require K: Send; no reference to K escapes the wrapper.
unsafe impl<K: Send> Send for ResidentKey<K> {}
// SAFETY: shared access only reads K or the immutable queue pointer. The queue
// synchronizes ownership transfer of K, and the backend cannot outlive its owner.
unsafe impl<K: Send + Sync> Sync for ResidentKey<K> {}

impl<K> ResidentKey<K> {
    fn get(&self) -> &VersionedKey<K> {
        self.key.as_ref().expect("resident key owns its payload")
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
        if let Some(retired) = self.retired {
            // SAFETY: insert_resident creates this pointer from the containing
            // index's PublishedOwner. The private backend retains no keys after
            // its destruction, which precedes queue-owner destruction. Removal
            // and eviction only move this wrapper within a synchronous call;
            // only the owned VersionedKey is passed to later deferred batches.
            let retired = unsafe { retired.as_ref() };
            retired.push(self.key.take().expect("resident key owns its payload"));
        }
    }
}

/// Exactly one stored value owns the resident pin. Lookup clones are snapshots.
/// Ownership begins before calling any user key Clone/Hash/Eq implementation.
/// Moving this value into an index transfers the pin even if insertion unwinds.
pub(super) struct ResidentEntry<V> {
    // Low bit set: this entry owns the residency obligation. Never dereference
    // this pointer directly; node() restores the allocation address first.
    tagged_node: NonNull<CacheNode<V>>,
    weight: u64,
}

// SAFETY: Like NodePtr, this capability shares V and may retire it on another
// thread. The tag is modified only through exclusive access; V must be Send + Sync.
unsafe impl<V: Send + Sync> Send for ResidentEntry<V> {}
// SAFETY: Shared access only reads the tag or creates non-owning snapshots.
// Retirement requires exclusive access, and V has NodePtr's Send + Sync bounds.
unsafe impl<V: Send + Sync> Sync for ResidentEntry<V> {}

impl<V> ResidentEntry<V> {
    const OWNS_RESIDENCY: usize = 1;

    pub(super) fn new((node, weight): Entry<V>) -> Self {
        // CacheNode contains AtomicU32, so bit zero is available for every V.
        // Keep this a per-monomorphization compile-time check if layout changes.
        const { assert!(std::mem::align_of::<CacheNode<V>>() > Self::OWNS_RESIDENCY) };
        Self {
            tagged_node: node.0.map_addr(|addr| addr | Self::OWNS_RESIDENCY),
            weight,
        }
    }

    fn node(&self) -> NodePtr<V> {
        // map_addr preserves provenance, including for a stale non-owning clone.
        // The original address is nonzero and aligned; clearing the tag cannot
        // turn it into null. No allocation access occurs here.
        let pointer = self
            .tagged_node
            .as_ptr()
            .map_addr(|addr| addr & !Self::OWNS_RESIDENCY);
        // SAFETY: new() starts with an aligned, nonnull allocation address.
        // Every mutation only sets or clears the spare bit, so masking it
        // restores that nonnull address even when a snapshot is stale.
        NodePtr(unsafe { NonNull::new_unchecked(pointer) })
    }

    fn snapshot(&self) -> Entry<V> {
        (self.node(), self.weight)
    }

    fn retire(&mut self) {
        if self.tagged_node.addr().get() & Self::OWNS_RESIDENCY != 0 {
            let node = self.node();
            self.tagged_node = node.0;
            // This releases only residency; value destruction follows grace.
            retire_resident(node);
        }
    }
}

impl<V> Clone for ResidentEntry<V> {
    fn clone(&self) -> Self {
        Self {
            tagged_node: self.node().0,
            weight: self.weight,
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

/// Resident policy and deferred key destruction for the production cache.
pub(super) struct ResidentIndex<K, V> {
    // Lifetime invariant: all non-owning ResidentKey queue pointers are confined
    // to this private backend or its synchronous method frames. Field order
    // destroys those keys before their unique queue owner. PublishedOwner keeps
    // the queue address/provenance stable when this enclosing index is moved.
    index: QuickResidentIndex<K, V>,
    retired_keys: Option<PublishedOwner<RetiredKeys<K>>>,
}

impl<K, V> ResidentIndex<K, V>
where
    K: Clone + Eq + Hash + Send + Sync + 'static,
    V: Send + Sync + 'static,
{
    pub(super) fn new(capacity: u64) -> Self {
        Self {
            index: QuickResidentIndex::new(capacity),
            retired_keys: std::mem::needs_drop::<K>()
                .then(|| PublishedOwner::new(RetiredKeys::new())),
        }
    }

    #[cfg(feature = "bench-internals")]
    pub(super) fn memory_estimate(&self) -> usize {
        self.index.estimated_index_bytes()
    }

    #[inline]
    pub(super) fn get(&self, key: &VersionedKeyRef<'_, K>) -> Option<Entry<V>> {
        self.index.get(key)
    }

    /// Stores an initialized entry in the resident index.
    pub(super) fn insert_resident(&self, key: &VersionedKey<K>, entry: ResidentEntry<V>) {
        // Do not expose a constructor returning ResidentKey: its queue pointer
        // is valid only inside this method or the containing backend. Cloning K
        // happens before constructing the wrapper, so a panic cannot publish a
        // partially initialized pointer-bearing key.
        let key = ResidentKey {
            key: Some(key.clone()),
            retired: self
                .retired_keys
                .as_ref()
                .map(|owner| NonNull::from(owner.as_ref())),
        };
        self.index.insert_resident(key, entry);
    }

    pub(super) fn invalidate(&self, key: &VersionedKey<K>) {
        self.index.invalidate(key);
    }

    pub(super) fn invalidate_before(&self, epoch: u64) {
        self.index.invalidate_before(epoch);
    }

    /// Runs key destructors only after the caller has left all cache locks.
    pub(super) fn maintenance(&self) {
        if let Some(retired) = &self.retired_keys {
            retired.reclaim();
        }
    }

    pub(super) fn has_retired_keys(&self) -> bool {
        self.retired_keys
            .as_ref()
            .is_some_and(|retired| retired.has_pending())
    }

    pub(super) fn clear(&self) {
        self.index.clear();
    }

    pub(super) fn resident_count(&self) -> u64 {
        self.index.resident_count()
    }

    pub(super) fn resident_weight(&self) -> u64 {
        self.index.resident_weight()
    }
}

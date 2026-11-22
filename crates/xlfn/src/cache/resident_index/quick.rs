//! Quick Cache resident policy. Only stored values own a residency obligation.
//! Cloned lookup/placeholder results are non-owning snapshots.
//!
//! This is the lifetime boundary for ResidentKey's non-owning retirement queue.
//! Only get/insert/remove/retain/clear and scalar observations are exposed. get
//! clones a ResidentEntry containing no key or queue pointer; remove results and
//! insert's DefaultLifecycle request state are dropped before returning. No
//! entry/placeholder/drain guard, callback-owned item, or backend handle escapes.
//! ResidentKey intentionally has no Clone impl, also ruling out key-cloning
//! iteration. The backend has no background eviction task. Any extension of
//! this boundary must preserve destruction of all keys before the queue owner.

use super::{Entry, ResidentEntry, ResidentKey, VersionedKey, VersionedKeyRef};
#[cfg(not(target_os = "linux"))]
use quick_cache::sync::{Cache, DefaultLifecycle};
#[cfg(target_os = "linux")]
use quick_cache::unsync::{Cache, DefaultLifecycle};
use quick_cache::{OptionsBuilder, Weighter};
use std::borrow::Borrow;
use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hash, Hasher};

#[derive(Clone)]
struct EntryWeight;

impl<K, V> Weighter<K, ResidentEntry<V>> for EntryWeight {
    fn weight(&self, _: &K, value: &ResidentEntry<V>) -> u64 {
        value.weight
    }
}

// The keyed input is already well distributed. Keep a full 64-bit hash
// even on 32-bit Excel: quick_cache uses it to identify evicted ghost entries.
#[cfg(target_pointer_width = "64")]
type PolicyHashBuilder = rustc_hash::FxBuildHasher;
#[cfg(not(target_pointer_width = "64"))]
type PolicyHashBuilder = RandomState;

type PolicyCache<K, V> =
    Cache<HashedKey<ResidentKey<K>>, ResidentEntry<V>, EntryWeight, PolicyHashBuilder>;

/// Only invoke user Hash before taking policy locks. Both backends rehash
/// stored keys under a lock during operations such as growth and eviction;
/// those paths must hash only this precomputed scalar.
struct HashedKey<K> {
    hash: u64,
    key: K,
}

impl<K> Hash for HashedKey<K> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.hash.hash(state);
    }
}

impl<K: PartialEq> PartialEq for HashedKey<K> {
    fn eq(&self, other: &Self) -> bool {
        self.hash == other.hash && self.key == other.key
    }
}

impl<K: Eq> Eq for HashedKey<K> {}

impl<K, Q> quick_cache::Equivalent<HashedKey<ResidentKey<K>>> for HashedKey<&VersionedKeyRef<'_, Q>>
where
    K: Borrow<Q>,
    Q: Eq + ?Sized,
{
    fn equivalent(&self, owned: &HashedKey<ResidentKey<K>>) -> bool {
        self.hash == owned.hash && quick_cache::Equivalent::equivalent(self.key, &owned.key)
    }
}

impl<K: Eq> quick_cache::Equivalent<HashedKey<ResidentKey<K>>> for HashedKey<&VersionedKey<K>> {
    fn equivalent(&self, owned: &HashedKey<ResidentKey<K>>) -> bool {
        self.hash == owned.hash && self.key == owned.key.get()
    }
}

pub(super) struct QuickResidentIndex<K, V> {
    #[cfg(not(target_os = "linux"))]
    cache: PolicyCache<K, V>,
    // Linux/Miri cannot use Quick Cache's older parking_lot backend. Its std
    // fallback poisons after key equality panics, including during final clear.
    // Own synchronization with the same non-poisoning lock as the rest of xlfn.
    #[cfg(target_os = "linux")]
    cache: crate::sync::Mutex<PolicyCache<K, V>>,
    hash_builder: RandomState,
}

impl<K, V> QuickResidentIndex<K, V>
where
    K: Clone + Eq + Hash + Send + Sync + 'static,
    V: Send + Sync + 'static,
{
    pub(super) fn new(capacity: u64) -> Self {
        // Weight is bytes, not an item count: never preallocate the entire
        // byte budget as entries. Match Quick Cache's minimum shard size.
        let estimated = usize::try_from(capacity)
            .unwrap_or(usize::MAX)
            .clamp(1, 1024);
        let options = OptionsBuilder::new()
            .shards(1)
            .estimated_items_capacity(estimated)
            .weight_capacity(capacity)
            // The default 97% hot quota rejects a single full-budget entry,
            // even with one shard. Preserve eligibility up to shard capacity.
            .hot_allocation(1.0)
            .build()
            .expect("valid resident cache options");
        // Only the keyed 64-bit output from hash_builder reaches the policy
        // table. User-provided Hash still runs before taking locks.
        #[cfg(target_pointer_width = "64")]
        let policy_hasher = rustc_hash::FxBuildHasher;
        #[cfg(not(target_pointer_width = "64"))]
        let policy_hasher = RandomState::new();
        let cache = Cache::with_options(
            options,
            EntryWeight,
            policy_hasher,
            DefaultLifecycle::default(),
        );
        Self {
            #[cfg(not(target_os = "linux"))]
            cache,
            #[cfg(target_os = "linux")]
            cache: crate::sync::Mutex::new(cache),
            hash_builder: RandomState::new(),
        }
    }

    #[cfg(not(target_os = "linux"))]
    fn with_cache<T>(&self, operation: impl FnOnce(&PolicyCache<K, V>) -> T) -> T {
        operation(&self.cache)
    }

    #[cfg(target_os = "linux")]
    fn with_cache<T>(&self, operation: impl FnOnce(&mut PolicyCache<K, V>) -> T) -> T {
        operation(&mut self.cache.lock())
    }

    pub(super) fn hash<Q: Hash + ?Sized>(&self, key: &VersionedKeyRef<'_, Q>) -> u64 {
        self.hash_builder.hash_one(key)
    }

    pub(super) fn get<Q>(&self, key: &VersionedKeyRef<'_, Q>, hash: u64) -> Option<Entry<V>>
    where
        K: Borrow<Q>,
        Q: Eq + ?Sized,
    {
        let key = &HashedKey { hash, key };
        self.with_cache(|cache| cache.get(key).map(|entry| entry.snapshot()))
    }

    pub(super) fn insert_resident(&self, key: ResidentKey<K>, hash: u64, entry: ResidentEntry<V>) {
        let key = HashedKey { hash, key };
        self.with_cache(|cache| cache.insert(key, entry));
    }

    pub(super) fn invalidate(&self, key: &VersionedKey<K>, hash: u64) {
        let key = &HashedKey { hash, key };
        self.with_cache(|cache| drop(cache.remove(key)));
    }

    pub(super) fn invalidate_before(&self, epoch: u64) {
        self.with_cache(|cache| {
            cache.retain(|key, _| key.key.get().epoch >= epoch);
        });
    }

    pub(super) fn clear(&self) {
        self.with_cache(|cache| cache.clear());
    }

    pub(super) fn resident_count(&self) -> u64 {
        self.with_cache(|cache| cache.len() as u64)
    }

    pub(super) fn resident_weight(&self) -> u64 {
        self.with_cache(|cache| cache.weight())
    }

    #[cfg(feature = "bench-internals")]
    pub(super) fn estimated_index_bytes(&self) -> usize {
        self.with_cache(|cache| cache.memory_used().total())
    }
}

//! Qualification resident policy, compiled only with `bench-internals`.
//!
//! Reads take one fixed shard's shared lock. Writers serialize capacity
//! decisions, then take shard write locks one at a time. Initialization has
//! separate per-key flights and runs without either policy or shard locks.
//! Stored values own resident pins; their drops retire without destroying values.

use super::{Entry, ResidentEntry, VersionedKey, VersionedKeyRef};
use crate::sync::{Mutex, RwLock};
use rustc_hash::{FxBuildHasher, FxHashMap};
use std::hash::{BuildHasher, Hash};
use std::sync::atomic::{AtomicU64, Ordering};

type ResidentMap<K, V> = FxHashMap<VersionedKey<K>, ResidentEntry<V>>;

struct Policy {
    weight: u64,
    entries: u64,
    next_victim: usize,
}

pub(super) struct ShardedResidentIndex<K, V> {
    shards: Box<[RwLock<ResidentMap<K, V>>]>,
    hash: FxBuildHasher,
    shift: u32,
    capacity: u64,
    policy: Mutex<Policy>,
    entries: AtomicU64,
    weight: AtomicU64,
}

impl<K, V> ShardedResidentIndex<K, V>
where
    K: Clone + Eq + Hash + Send + Sync + 'static,
    V: Send + Sync + 'static,
{
    pub(super) fn new(capacity: u64, shard_count: usize) -> Self {
        assert!(matches!(shard_count, 8 | 16 | 32 | 64));
        let hash = FxBuildHasher;
        Self {
            shards: (0..shard_count)
                .map(|_| RwLock::new(FxHashMap::default()))
                .collect(),
            shift: 64 - shard_count.trailing_zeros(),
            hash,
            capacity,
            policy: Mutex::new(Policy {
                weight: 0,
                entries: 0,
                next_victim: 0,
            }),
            entries: AtomicU64::new(0),
            weight: AtomicU64::new(0),
        }
    }

    #[inline]
    fn shard(&self, hash: u64) -> usize {
        (hash >> self.shift) as usize
    }

    #[inline]
    pub(super) fn get(&self, key: &VersionedKeyRef<'_, K>) -> Option<Entry<V>> {
        let lookup = VersionedKey {
            epoch: key.epoch,
            key: key.key.clone(),
        };
        let hash = self.hash.hash_one(&lookup);
        self.shards[self.shard(hash)]
            .read()
            .get(&lookup)
            .map(|entry| entry.snapshot())
    }

    pub(super) fn publish(&self, key: VersionedKey<K>, entry: ResidentEntry<V>) {
        let hash = self.hash.hash_one(&key);
        let shard_index = self.shard(hash);
        let mut removed = Vec::new();
        {
            let mut policy = self.policy.lock();
            if entry.weight > self.capacity {
                removed.push((key, entry));
            } else {
                {
                    let mut shard = self.shards[shard_index].write();
                    if let Some(old_entry) = shard.remove(&key) {
                        policy.weight -= old_entry.weight;
                        policy.entries -= 1;
                        removed.push((key.clone(), old_entry));
                    }
                }
                while policy.weight + entry.weight > self.capacity {
                    // At least one resident exists when the bounded weight
                    // requires eviction. Rotate the starting shard so policy
                    // does not always charge the first shard for global debt.
                    let index = policy.next_victim;
                    policy.next_victim = (index + 1) % self.shards.len();
                    let victim_key = self.shards[index].read().keys().next().cloned();
                    if let Some(victim_key) = victim_key
                        && let Some(victim_entry) = self.shards[index].write().remove(&victim_key)
                    {
                        policy.weight -= victim_entry.weight;
                        policy.entries -= 1;
                        removed.push((victim_key, victim_entry));
                    }
                }
                let weight = entry.weight;
                self.shards[shard_index].write().insert(key, entry);
                policy.weight += weight;
                policy.entries += 1;
            }
            self.record(&policy);
        }
        self.notify(removed);
    }

    fn record(&self, policy: &Policy) {
        self.entries.store(policy.entries, Ordering::Relaxed);
        self.weight.store(policy.weight, Ordering::Relaxed);
    }

    fn notify(&self, mut removed: Vec<(VersionedKey<K>, ResidentEntry<V>)>) {
        // Transfer every residency obligation before destroying removed keys.
        // In particular, a key destructor cannot skip later notifications.
        // The xlfn callback only retires; it must not unwind or reclaim values.
        for (_, entry) in &mut removed {
            entry.retire();
        }
        drop(removed);
    }

    pub(super) fn invalidate(&self, key: &VersionedKey<K>) {
        let hash = self.hash.hash_one(key);
        let removed = {
            let mut policy = self.policy.lock();
            let removed = self.shards[self.shard(hash)].write().remove_entry(key);
            if let Some((_, entry)) = &removed {
                policy.entries -= 1;
                policy.weight -= entry.weight;
            }
            self.record(&policy);
            removed
        };
        self.notify(removed.into_iter().collect());
    }

    pub(super) fn invalidate_before(&self, epoch: u64) {
        self.remove_matching(|key| key.epoch < epoch);
    }

    pub(super) fn clear(&self) {
        self.remove_matching(|_| true);
    }

    fn remove_matching(&self, select: impl Fn(&VersionedKey<K>) -> bool) {
        let mut removed = Vec::new();
        {
            let mut policy = self.policy.lock();
            for shard in &self.shards {
                let matching: Vec<_> = shard
                    .read()
                    .keys()
                    .filter(|key| select(key))
                    .cloned()
                    .collect();
                if !matching.is_empty() {
                    let mut shard = shard.write();
                    for key in matching {
                        if let Some(entry) = shard.remove(&key) {
                            policy.entries -= 1;
                            policy.weight -= entry.weight;
                            removed.push((key, entry));
                        }
                    }
                }
            }
            self.record(&policy);
        }
        self.notify(removed);
    }

    pub(super) fn resident_count(&self) -> u64 {
        self.entries.load(Ordering::Relaxed)
    }

    pub(super) fn resident_weight(&self) -> u64 {
        self.weight.load(Ordering::Relaxed)
    }

    #[cfg(feature = "bench-internals")]
    pub(super) fn estimated_index_bytes(&self) -> usize {
        // Hashbrown's capacity excludes control bytes and spare buckets. The
        // 8/7 factor estimates its load factor; allocator rounding is excluded.
        let bucket = std::mem::size_of::<(VersionedKey<K>, ResidentEntry<V>)>() + 1;
        std::mem::size_of::<Self>()
            + self.shards.len() * std::mem::size_of::<RwLock<ResidentMap<K, V>>>()
            + self
                .shards
                .iter()
                .map(|shard| shard.read().capacity() * 8 / 7 * bucket)
                .sum::<usize>()
    }
}

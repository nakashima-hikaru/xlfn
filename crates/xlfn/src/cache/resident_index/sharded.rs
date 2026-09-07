//! Qualification resident policy, compiled only with `bench-internals`.
//!
//! Reads take one fixed shard's shared lock. Writers serialize capacity
//! decisions, then take shard write locks one at a time. Initialization has
//! separate per-key flights and runs without either policy or shard locks.
//! This backend never dereferences a node or changes a residency/pin counter.

use super::{Entry, VersionedKey, VersionedKeyRef};
use crate::{XllError, XllResult};
use hashbrown::{HashMap, hash_map::RawEntryMut};
use parking_lot::{Condvar, Mutex, RwLock};
use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hash};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

type Removed<V> = dyn Fn(Entry<V>) + Send + Sync;
type ResidentMap<K, V> = HashMap<VersionedKey<K>, Entry<V>, RandomState>;
type Flights<K, V> = HashMap<VersionedKey<K>, Arc<Flight<V>>, RandomState>;

enum Completion<V> {
    Pending,
    Finished(Result<Entry<V>, Arc<XllError>>),
    Retry,
}

struct Flight<V> {
    state: Mutex<Completion<V>>,
    changed: Condvar,
}

struct Policy {
    weight: u64,
    entries: u64,
    next_victim: usize,
}

pub(super) struct ShardedResidentIndex<K, V> {
    shards: Box<[RwLock<ResidentMap<K, V>>]>,
    hash: RandomState,
    shift: u32,
    capacity: u64,
    policy: Mutex<Policy>,
    flights: Mutex<Flights<K, V>>,
    removed: Arc<Removed<V>>,
    entries: AtomicU64,
    weight: AtomicU64,
}

impl<K, V> ShardedResidentIndex<K, V>
where
    K: Clone + Eq + Hash + Send + Sync + 'static,
    V: Send + Sync + 'static,
{
    pub(super) fn new(
        capacity: u64,
        shard_count: usize,
        removed: impl Fn(Entry<V>) + Send + Sync + 'static,
    ) -> Self {
        assert!(matches!(shard_count, 8 | 16 | 32 | 64));
        let hash = RandomState::new();
        Self {
            shards: (0..shard_count)
                .map(|_| RwLock::new(HashMap::with_hasher(hash.clone())))
                .collect(),
            shift: 64 - shard_count.trailing_zeros(),
            hash: hash.clone(),
            capacity,
            policy: Mutex::new(Policy {
                weight: 0,
                entries: 0,
                next_victim: 0,
            }),
            flights: Mutex::new(HashMap::with_hasher(hash)),
            removed: Arc::new(removed),
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
        let hash = self.hash.hash_one(key);
        self.shards[self.shard(hash)]
            .read()
            .raw_entry()
            .from_hash(hash, |owned| {
                owned.epoch == key.epoch && &owned.key == key.key
            })
            .map(|(_, entry)| *entry)
    }

    pub(super) fn insert(
        &self,
        key: VersionedKey<K>,
        initialize: impl FnOnce() -> XllResult<Entry<V>>,
    ) -> Result<Entry<V>, Arc<XllError>> {
        let mut initialize = Some(initialize);
        loop {
            let (flight, owner) = {
                let mut flights = self.flights.lock();
                // Serialize the second lookup with flight creation/removal.
                // Otherwise a just-completed owner could be initialized twice.
                if let Some(entry) = self.get(&VersionedKeyRef {
                    epoch: key.epoch,
                    key: &key.key,
                }) {
                    return Ok(entry);
                }
                if let Some(flight) = flights.get(&key) {
                    (Arc::clone(flight), false)
                } else {
                    let flight = Arc::new(Flight {
                        state: Mutex::new(Completion::Pending),
                        changed: Condvar::new(),
                    });
                    flights.insert(key.clone(), Arc::clone(&flight));
                    (flight, true)
                }
            };
            if !owner {
                let mut state = flight.state.lock();
                loop {
                    match &*state {
                        Completion::Pending => flight.changed.wait(&mut state),
                        Completion::Finished(result) => return result.clone(),
                        Completion::Retry => break,
                    }
                }
                continue;
            }

            // On an unwinding initializer, followers retry with their own
            // initializer, as with Moka. Never leave a permanently pending flight.
            let _finish = scopeguard::guard((), |_| {
                let removed = self.flights.lock().remove_entry(&key);
                {
                    let mut state = flight.state.lock();
                    if matches!(*state, Completion::Pending) {
                        *state = Completion::Retry;
                    }
                    flight.changed.notify_all();
                }
                // Keys and the last flight/error owner are dropped unlocked.
                drop(removed);
            });
            let result = initialize.take().expect("initializer consumed once")().map_err(Arc::new);
            if let Ok(entry) = result {
                self.publish(key.clone(), entry);
            }
            *flight.state.lock() = Completion::Finished(result.clone());
            return result;
        }
    }

    fn publish(&self, key: VersionedKey<K>, entry: Entry<V>) {
        let hash = self.hash.hash_one(&key);
        let shard_index = self.shard(hash);
        let mut removed = Vec::new();
        {
            let mut policy = self.policy.lock();
            if u64::from(entry.1) > self.capacity {
                removed.push((key, entry));
            } else {
                {
                    let mut shard = self.shards[shard_index].write();
                    match shard.raw_entry_mut().from_hash(hash, |owned| owned == &key) {
                        RawEntryMut::Occupied(old) => {
                            let (old_key, old_entry) = old.remove_entry();
                            policy.weight -= u64::from(old_entry.1);
                            policy.entries -= 1;
                            removed.push((old_key, old_entry));
                        }
                        RawEntryMut::Vacant(_) => {}
                    }
                }
                while policy.weight + u64::from(entry.1) > self.capacity {
                    // At least one resident exists when the bounded weight
                    // requires eviction. Rotate the starting shard so policy
                    // does not always charge the first shard for global debt.
                    let index = policy.next_victim;
                    policy.next_victim = (index + 1) % self.shards.len();
                    let victim = self.shards[index].write().extract_if(|_, _| true).next();
                    if let Some(victim) = victim {
                        policy.weight -= u64::from(victim.1.1);
                        policy.entries -= 1;
                        removed.push(victim);
                    }
                }
                self.shards[shard_index].write().insert(key, entry);
                policy.weight += u64::from(entry.1);
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

    fn notify(&self, removed: Vec<(VersionedKey<K>, Entry<V>)>) {
        // Transfer every residency obligation before destroying removed keys.
        // In particular, a key destructor cannot skip later notifications.
        // The xlfn callback only retires; it must not unwind or reclaim values.
        for (_, entry) in &removed {
            (self.removed)(*entry);
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
                policy.weight -= u64::from(entry.1);
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
                let mut shard = shard.write();
                for item in shard.extract_if(|key, _| select(key)) {
                    policy.entries -= 1;
                    policy.weight -= u64::from(item.1.1);
                    removed.push(item);
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
        let bucket = std::mem::size_of::<(VersionedKey<K>, Entry<V>)>() + 1;
        std::mem::size_of::<Self>()
            + self.shards.len() * std::mem::size_of::<RwLock<ResidentMap<K, V>>>()
            + self
                .shards
                .iter()
                .map(|shard| shard.read().capacity() * 8 / 7 * bucket)
                .sum::<usize>()
            + self.flights.lock().capacity() * 8 / 7
                * (std::mem::size_of::<(VersionedKey<K>, Arc<Flight<V>>)>() + 1)
    }
}

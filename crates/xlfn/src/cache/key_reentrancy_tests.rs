use super::*;
use std::sync::{Arc as StdArc, Weak, mpsc};
use std::thread::JoinHandle;
use std::time::Duration;

#[derive(Clone)]
struct Key {
    id: u32,
    _owner: Option<StdArc<KeyOwner>>,
    on_hash: Option<StdArc<NestedLookup>>,
}

impl Key {
    fn plain(id: u32) -> Self {
        Self {
            id,
            _owner: None,
            on_hash: None,
        }
    }
}

impl PartialEq for Key {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl Eq for Key {}

impl Hash for Key {
    fn hash<H: Hasher>(&self, state: &mut H) {
        if let Some(nested) = &self.on_hash
            && !nested.executed.swap(true, Ordering::Relaxed)
        {
            let cache = nested.cache.upgrade().unwrap();
            let result = catch_no_unwind(AssertUnwindSafe(|| {
                drop(cache.get_or_try_insert_with(
                    Key::plain(2),
                    |_| 1,
                    || {
                        cache.clear();
                        assert!(!nested.panics, "injected nested initializer panic");
                        Ok(2)
                    },
                ));
            }));
            assert_eq!(result.is_err(), nested.panics);
        }
        self.id.hash(state);
    }
}

struct NestedLookup {
    cache: Weak<CalculationCache<Key, u32>>,
    executed: AtomicBool,
    panics: bool,
}

#[derive(Default)]
struct Observation {
    completed_during_drop: AtomicUsize,
    drops: AtomicUsize,
    workers: Mutex<Vec<JoinHandle<()>>>,
}

struct KeyOwner {
    cache: Weak<CalculationCache<Key, u32>>,
    observed: StdArc<Observation>,
    panics: bool,
}

impl Drop for KeyOwner {
    fn drop(&mut self) {
        self.observed.drops.fetch_add(1, Ordering::Relaxed);
        assert!(!self.panics, "injected key destructor panic");
        let Some(cache) = self.cache.upgrade() else {
            return;
        };
        let (sent, received) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            // Exercise both the backend shard lock and the outer clear lock.
            drop(cache.get(&Key::plain(99)));
            cache.clear();
            let _ = sent.send(());
        });
        self.observed.workers.lock().push(worker);
        // The old implementation must fail without leaving a blocked thread:
        // returning after the deadline releases the offending outer lock.
        if received.recv_timeout(Duration::from_secs(5)).is_ok() {
            self.observed
                .completed_during_drop
                .fetch_add(1, Ordering::Release);
        }
    }
}

fn caches(capacity: usize) -> Vec<CalculationCache<Key, u32>> {
    #[cfg(feature = "bench-internals")]
    {
        vec![
            CalculationCache::new_with_backend(capacity, CacheBackend::QuickCache { shards: 1 }),
            CalculationCache::new_with_backend(capacity, CacheBackend::Sharded { shards: 8 }),
        ]
    }
    #[cfg(not(feature = "bench-internals"))]
    {
        vec![CalculationCache::new(capacity)]
    }
}

fn insert_owned_key(
    cache: &StdArc<CalculationCache<Key, u32>>,
    observed: &StdArc<Observation>,
    id: u32,
    panics: bool,
) {
    let key = Key {
        id,
        _owner: Some(StdArc::new(KeyOwner {
            cache: StdArc::downgrade(cache),
            observed: StdArc::clone(observed),
            panics,
        })),
        on_hash: None,
    };
    drop(cache.get_or_try_insert_with(key, |_| 1, || Ok(id)).unwrap());
}

fn assert_reentered(observed: &Observation, expected: usize) {
    for worker in std::mem::take(&mut *observed.workers.lock()) {
        worker.join().unwrap();
    }
    assert_eq!(observed.drops.load(Ordering::Relaxed), expected);
    assert_eq!(
        observed.completed_during_drop.load(Ordering::Acquire),
        expected
    );
}

#[test]
fn clear_destroys_keys_after_releasing_all_cache_locks() {
    for cache in caches(8) {
        let cache = StdArc::new(cache);
        let observed = StdArc::new(Observation::default());
        insert_owned_key(&cache, &observed, 1, false);
        cache.clear();
        assert_reentered(&observed, 1);
        assert_eq!(cache.reclamation_stats().pending_nodes, 0);
    }
}

#[test]
fn eviction_destroys_keys_after_releasing_all_cache_locks() {
    for cache in caches(4) {
        let cache = StdArc::new(cache);
        let observed = StdArc::new(Observation::default());
        for id in 0..4 {
            insert_owned_key(&cache, &observed, id, false);
        }
        // Evict more than Quick Cache's default two-entry deferred batch in
        // one request; every evicted key must still be destroyed lock-free.
        drop(
            cache
                .get_or_try_insert_with(Key::plain(8), |_| 4, || Ok(8))
                .unwrap(),
        );
        assert_reentered(&observed, 4);
        assert_eq!(cache.reclamation_stats().pending_nodes, 0);
    }
}

#[test]
fn panicking_key_does_not_skip_remaining_retirement_or_final_drop() {
    for cache in caches(8) {
        let cache = StdArc::new(cache);
        let observed = StdArc::new(Observation::default());
        for id in 0..3 {
            insert_owned_key(&cache, &observed, id, true);
        }
        cache.clear();
        assert_eq!(observed.drops.load(Ordering::Relaxed), 3);
        assert_eq!(cache.reclamation_stats().pending_nodes, 0);
        insert_owned_key(&cache, &observed, 4, true);
        drop(cache);
        assert_eq!(observed.drops.load(Ordering::Relaxed), 4);
    }
}

#[test]
fn nested_key_callbacks_defer_destructors_until_outer_lookup_leaves() {
    for panics in [false, true] {
        for cache in caches(1).into_iter().chain(caches(8)) {
            let cache = StdArc::new(cache);
            let observed = StdArc::new(Observation::default());
            insert_owned_key(&cache, &observed, 1, false);
            let nested = StdArc::new(NestedLookup {
                cache: StdArc::downgrade(&cache),
                executed: AtomicBool::new(false),
                panics,
            });
            let key = Key {
                id: 99,
                _owner: None,
                on_hash: Some(StdArc::clone(&nested)),
            };
            // Also bound a regression in the value backpressure path: at a
            // one-entry budget the nested writer must not await this reader.
            let worker_cache = StdArc::clone(&cache);
            let (sent, received) = mpsc::channel();
            let worker = std::thread::spawn(move || {
                assert!(worker_cache.get(&key).is_none());
                sent.send(()).unwrap();
            });
            received.recv_timeout(Duration::from_secs(10)).unwrap();
            worker.join().unwrap();
            assert!(nested.executed.load(Ordering::Relaxed));
            assert_reentered(&observed, 1);
        }
    }
}

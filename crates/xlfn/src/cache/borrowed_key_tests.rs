use super::*;
use std::panic::AssertUnwindSafe;
use std::sync::Arc as StdArc;
use std::sync::atomic::AtomicU8;
#[cfg(not(miri))]
use std::sync::mpsc;
#[cfg(not(miri))]
use std::time::Duration;

#[derive(Default)]
struct QueryControl {
    owned: AtomicUsize,
    fault: AtomicU8,
    on_own: Option<Box<dyn Fn() + Send + Sync>>,
}

struct Query {
    text: String,
    control: StdArc<QueryControl>,
}

impl Query {
    fn new(text: &str, control: StdArc<QueryControl>) -> Self {
        Self {
            text: text.to_owned(),
            control,
        }
    }

    fn owned(&self) -> OwnedKey {
        OwnedKey(StdArc::new(Self::new(
            &self.text,
            StdArc::clone(&self.control),
        )))
    }
}

impl Hash for Query {
    fn hash<H: Hasher>(&self, state: &mut H) {
        assert_ne!(self.control.fault.load(Ordering::Relaxed), 1, "hash panic");
        // Collisions ensure that a resident also exercises borrowed equality.
        0_u8.hash(state);
    }
}

impl PartialEq for Query {
    fn eq(&self, other: &Self) -> bool {
        assert_ne!(self.control.fault.load(Ordering::Relaxed), 2, "eq panic");
        self.text == other.text
    }
}

impl Eq for Query {}

impl ToOwned for Query {
    type Owned = OwnedKey;

    fn to_owned(&self) -> OwnedKey {
        self.control.owned.fetch_add(1, Ordering::Relaxed);
        assert_ne!(self.control.fault.load(Ordering::Relaxed), 3, "own panic");
        if let Some(hook) = &self.control.on_own {
            hook();
        }
        self.owned()
    }
}

#[derive(Clone)]
struct OwnedKey(StdArc<Query>);

impl std::borrow::Borrow<Query> for OwnedKey {
    fn borrow(&self) -> &Query {
        &self.0
    }
}

impl Hash for OwnedKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.hash(state);
    }
}

impl PartialEq for OwnedKey {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

impl Eq for OwnedKey {}

#[test]
fn string_cache_and_registry_accept_str_and_string_queries() {
    let cache = CalculationCache::<String, u8>::new(crate::cache::CacheBudget::weight_units(8));
    assert!(cache.get("key").is_none());
    let old = cache
        .get_or_try_insert_with("key".to_owned(), || Ok(7))
        .unwrap();
    assert_eq!(*cache.get("key").unwrap(), 7);
    assert_eq!(*cache.get(&"key".to_owned()).unwrap(), 7);
    let temporary_query_lease = {
        let query = "key".to_owned();
        cache
            .get_or_try_insert_borrowed(query.as_str(), || panic!("resident hit"))
            .unwrap()
    };
    assert_eq!(*temporary_query_lease, 7);
    assert_eq!(
        *cache
            .get_or_try_insert_borrowed("key", || panic!("resident hit"))
            .unwrap(),
        7
    );
    cache.clear();
    assert!(cache.get("key").is_none());
    assert_eq!(*old, 7, "clear must preserve existing leases");

    let registry = CacheRegistry::new(crate::cache::CacheBudget::weight_units(8));
    let endpoint = CacheEndpoint::<String, u8>::new("borrowed");
    assert!(registry.get(&endpoint, "key").unwrap().is_none());
    assert_eq!(
        *registry
            .get_or_try_insert_borrowed(&endpoint, "key", || Ok(9))
            .unwrap(),
        9
    );
    assert_eq!(*registry.get(&endpoint, "key").unwrap().unwrap(), 9);
    assert_eq!(
        *registry.get(&endpoint, &"key".to_owned()).unwrap().unwrap(),
        9
    );
    assert_eq!(
        *registry
            .get_or_try_insert(&endpoint, "key".to_owned(), || panic!("resident hit"))
            .unwrap(),
        9
    );
}

#[test]
fn resident_hits_do_not_own_borrowed_keys() {
    let control = StdArc::new(QueryControl::default());
    let query = Query::new("key", StdArc::clone(&control));
    let cache = CalculationCache::<OwnedKey, u8>::new(crate::cache::CacheBudget::weight_units(8));
    assert_eq!(
        *cache.get_or_try_insert_borrowed(&query, || Ok(1)).unwrap(),
        1
    );
    assert_eq!(control.owned.load(Ordering::Relaxed), 1);
    // A panic in ToOwned would make this fail if a hit owned its query.
    control.fault.store(3, Ordering::Relaxed);
    for _ in 0..10 {
        assert_eq!(*cache.get(&query).unwrap(), 1);
        assert_eq!(
            *cache
                .get_or_try_insert_borrowed(&query, || panic!("resident hit"))
                .unwrap(),
            1
        );
    }
    assert_eq!(control.owned.load(Ordering::Relaxed), 1);
    control.fault.store(0, Ordering::Relaxed);

    let registry = CacheRegistry::new(crate::cache::CacheBudget::weight_units(8));
    let endpoint = CacheEndpoint::<OwnedKey, u8>::new("counted");
    drop(
        registry
            .get_or_try_insert_borrowed(&endpoint, &query, || Ok(2))
            .unwrap(),
    );
    control.fault.store(3, Ordering::Relaxed);
    assert_eq!(
        *registry
            .get_or_try_insert_borrowed(&endpoint, &query, || panic!("resident hit"))
            .unwrap(),
        2
    );
    assert_eq!(control.owned.load(Ordering::Relaxed), 2);
    control.fault.store(0, Ordering::Relaxed);
}

#[test]
fn owned_and_borrowed_versioned_hashes_match() {
    for epoch in [0, 1, u64::MAX] {
        for text in ["", "key", "日本語💡"] {
            let owned = VersionedKey {
                epoch,
                key: text.to_owned(),
            };
            let borrowed = VersionedKeyRef { epoch, key: text };
            let mut hasher = FxHasher::default();
            borrowed.hash(&mut hasher);
            assert_eq!(flight_hash(&owned), hasher.finish());
            assert!(quick_cache::Equivalent::equivalent(&borrowed, &owned));
        }
    }
}

#[test]
fn borrowed_key_panics_leave_no_flights_or_broken_residents() {
    let control = StdArc::new(QueryControl::default());
    let query = Query::new("key", StdArc::clone(&control));
    let cache = CalculationCache::<OwnedKey, u8>::new(crate::cache::CacheBudget::weight_units(8));
    drop(
        cache
            .get_or_try_insert_with(query.owned(), || Ok(1))
            .unwrap(),
    );
    for fault in [1, 2, 3, 4] {
        // ToOwned and compute need a miss; hashing and equality run on a hit.
        if fault >= 3 {
            cache.clear();
        }
        control.fault.store(fault, Ordering::Relaxed);
        let result = catch_no_unwind(AssertUnwindSafe(|| {
            cache.get_or_try_insert_borrowed(&query, || panic!("compute panic"))
        }));
        control.fault.store(0, Ordering::Relaxed);
        assert!(result.is_err(), "fault {fault} must unwind");
        assert!(cache.flights.is_empty());
        assert_eq!(
            *cache.get_or_try_insert_borrowed(&query, || Ok(1)).unwrap(),
            1
        );
    }
}

#[test]
fn borrowed_misses_reject_reentry_but_resident_hits_remain_available() {
    let cache = CalculationCache::<String, u8>::new(crate::cache::CacheBudget::weight_units(8));
    drop(cache.get_or_try_insert_borrowed("warm", || Ok(1)).unwrap());
    let outer = cache
        .get_or_try_insert_borrowed("outer", || {
            assert_eq!(
                *cache
                    .get_or_try_insert_borrowed("warm", || panic!("hit"))
                    .unwrap(),
                1
            );
            let error = cache
                .get_or_try_insert_borrowed("cold", || Ok(2))
                .unwrap_err();
            assert!(matches!(error, XllError::Internal { diagnostic_id }
            if diagnostic_id == crate::diagnostics::id::DiagnosticId::CACHE_REENTRANT));
            Ok(3)
        })
        .unwrap();
    assert_eq!(*outer, 3);
    assert!(cache.flights.is_empty());
}

#[test]
#[cfg(not(miri))]
fn borrowed_ownership_race_rechecks_residency_and_keeps_original_generation() {
    for clear in [false, true] {
        let (started_tx, started_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        let release_rx = Mutex::new(release_rx);
        let control = StdArc::new(QueryControl {
            on_own: Some(Box::new(move || {
                started_tx.send(()).unwrap();
                release_rx
                    .lock()
                    .recv_timeout(Duration::from_secs(5))
                    .unwrap();
            })),
            ..QueryControl::default()
        });
        let query = Query::new("key", control);
        let cache =
            CalculationCache::<OwnedKey, u8>::new(crate::cache::CacheBudget::weight_units(8));
        std::thread::scope(|scope| {
            let worker = scope.spawn(|| {
                *cache
                    .get_or_try_insert_borrowed(&query, || {
                        assert!(
                            clear,
                            "the owned-key recheck must observe concurrent insertion"
                        );
                        Ok(1)
                    })
                    .unwrap()
            });
            started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            if clear {
                cache.clear();
            }
            drop(
                cache
                    .get_or_try_insert_with(query.owned(), || Ok(2))
                    .unwrap(),
            );
            release_tx.send(()).unwrap();
            assert_eq!(worker.join().unwrap(), if clear { 1 } else { 2 });
        });
        assert_eq!(*cache.get(&query).unwrap(), 2);
        assert!(cache.flights.is_empty());
    }
}

#[test]
#[cfg(not(miri))]
fn borrowed_and_owned_misses_share_singleflight_even_without_residency() {
    for budget in [0, 8] {
        let cache =
            CalculationCache::<String, u8>::new(crate::cache::CacheBudget::weight_units(budget));
        let key = VersionedKey {
            epoch: 0,
            key: "key".to_owned(),
        };
        let hash = flight_hash(&key);
        std::thread::scope(|scope| {
            let (started_tx, started_rx) = mpsc::sync_channel(1);
            let (release_tx, release_rx) = mpsc::sync_channel(1);
            let cache_ref = &cache;
            let leader = scope.spawn(move || {
                *cache_ref
                    .get_or_try_insert_borrowed("key", || {
                        started_tx.send(()).unwrap();
                        release_rx.recv().unwrap();
                        Ok(7)
                    })
                    .unwrap()
            });
            started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            let flight = Arc::clone(cache.flights.shard(hash).lock().get(hash, &key).unwrap());
            let borrowed = scope.spawn(|| {
                *cache
                    .get_or_try_insert_borrowed("key", || panic!("borrowed follower"))
                    .unwrap()
            });
            let owned = scope.spawn(|| {
                *cache
                    .get_or_try_insert_with("key".to_owned(), || panic!("owned follower"))
                    .unwrap()
            });
            // Leader, registry, observer, and both enrolled followers.
            let deadline = Instant::now() + Duration::from_secs(5);
            while Arc::strong_count(&flight) < 5 && Instant::now() < deadline {
                std::thread::yield_now();
            }
            let enrolled = Arc::strong_count(&flight) >= 5;
            release_tx.send(()).unwrap();
            assert_eq!(leader.join().unwrap(), 7);
            assert_eq!(borrowed.join().unwrap(), 7);
            assert_eq!(owned.join().unwrap(), 7);
            assert!(enrolled, "both followers must join the pending flight");
        });
        assert_eq!(cache.get("key").is_some(), budget != 0);
        assert!(cache.flights.is_empty());
    }
}

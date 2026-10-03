use super::*;
use std::panic::AssertUnwindSafe;
use std::sync::Arc as StdArc;
use std::sync::atomic::{AtomicU8, AtomicUsize};
#[cfg(not(miri))]
use std::sync::mpsc;
#[cfg(not(miri))]
use std::time::Duration;

#[derive(Clone)]
struct FaultKey {
    id: u32,
    fault: StdArc<AtomicU8>,
}

impl Hash for FaultKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        assert_ne!(self.fault.load(Ordering::Relaxed), 1, "key hash panic");
        // Deliberate collisions also exercise equality and swap removal.
        0_u8.hash(state);
    }
}

impl PartialEq for FaultKey {
    fn eq(&self, other: &Self) -> bool {
        assert_ne!(self.fault.load(Ordering::Relaxed), 2, "key equality panic");
        self.id == other.id
    }
}

impl Eq for FaultKey {}

struct DropProbe(StdArc<AtomicUsize>);

impl Drop for DropProbe {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
}

#[test]
fn success_and_error_completion_do_not_reenter_key_callbacks() {
    for fault_mode in [1, 2] {
        for succeeds in [false, true] {
            let fault = StdArc::new(AtomicU8::new(0));
            let key = FaultKey {
                id: 7,
                fault: StdArc::clone(&fault),
            };
            let cache = CalculationCache::new(0);
            let dropped = StdArc::new(AtomicUsize::new(0));
            let first = catch_no_unwind(AssertUnwindSafe(|| {
                cache.get_or_try_insert_with(
                    key.clone(),
                    |_| 1,
                    || {
                        // Completion previously rehashed/recompared this key
                        // while removing its registered flight.
                        fault.store(fault_mode, Ordering::Relaxed);
                        if succeeds {
                            Ok(DropProbe(StdArc::clone(&dropped)))
                        } else {
                            Err(XllError::Overloaded)
                        }
                    },
                )
            }));
            fault.store(0, Ordering::Relaxed);
            let first = first.expect("completion must not call user Hash or Eq");
            assert_eq!(first.is_ok(), succeeds);
            if !succeeds {
                assert!(matches!(&first, Err(XllError::Overloaded)));
            }
            drop(first);
            assert!(cache.flights.is_empty());
            assert_eq!(dropped.load(Ordering::Relaxed), usize::from(succeeds));

            // An error or an unretained success must not become a permanent
            // result supplied by an abandoned Finished flight.
            let recomputed = AtomicBool::new(false);
            drop(
                cache
                    .get_or_try_insert_with(
                        key,
                        |_| 1,
                        || {
                            recomputed.store(true, Ordering::Relaxed);
                            Ok(DropProbe(StdArc::clone(&dropped)))
                        },
                    )
                    .unwrap(),
            );
            assert!(recomputed.load(Ordering::Relaxed));
            assert!(cache.flights.is_empty());
            assert_eq!(dropped.load(Ordering::Relaxed), usize::from(succeeds) + 1);
        }
    }
}

#[test]
fn colliding_flights_remove_by_identity_after_table_growth() {
    let fault = StdArc::new(AtomicU8::new(0));
    let mut flights = FlightSet::<FaultKey, u8>::default();
    let mut owners = Vec::new();
    for id in 0..128 {
        let key = VersionedKey {
            epoch: 0,
            key: FaultKey {
                id,
                fault: StdArc::clone(&fault),
            },
        };
        let hash = flight_hash(&key);
        assert!(flights.get(hash, &key).is_none());
        let flight = Arc::new(Flight::new(key, hash));
        flights.insert_unique(Arc::clone(&flight));
        owners.push(flight);
    }
    for flight in &owners {
        assert!(Arc::ptr_eq(
            flights.get(flight.hash, &flight.key).unwrap(),
            flight,
        ));
    }
    // Removing in insertion order repeatedly moves the last indexed entry.
    // Both a full hash collision and changing indices must preserve identity.
    for (index, flight) in owners.iter().enumerate() {
        fault.store(1 + (index % 2) as u8, Ordering::Relaxed);
        assert!(Arc::ptr_eq(&flights.remove(flight).unwrap(), flight));
    }
    assert!(flights.entries.is_empty());
}

#[test]
fn coordination_uses_both_hash_halves_without_changing_key_identity() {
    let registry = FlightRegistry::<u64, u8>::new();
    println!(
        "flight coordination: shards={}, previous_table_bytes={}, registry_bytes={}, shard_storage_bytes={}",
        FLIGHT_COORDINATION_SHARDS,
        size_of::<Mutex<FlightSet<u64, u8>>>(),
        size_of::<FlightRegistry<u64, u8>>(),
        std::mem::size_of_val(&*registry.shards),
    );
    let mut visited = [false; FLIGHT_COORDINATION_SHARDS];
    for id in 0..FLIGHT_COORDINATION_SHARDS {
        // Low-bit alignment must not collapse independent keys to one lock.
        let hash = (id as u64) << 32;
        visited[FlightRegistry::<u64, u8>::shard_index(hash)] = true;
        let key = VersionedKey {
            epoch: 0,
            key: id as u64,
        };
        let flight = Arc::new(Flight::new(key, hash));
        registry
            .shard(hash)
            .lock()
            .insert_unique(Arc::clone(&flight));
        assert!(Arc::ptr_eq(
            registry.shard(hash).lock().get(hash, &flight.key).unwrap(),
            &flight,
        ));
        assert!(registry.shard(hash).lock().remove(&flight).is_some());
    }
    assert!(visited.into_iter().all(|visited| visited));
    assert!(registry.is_empty());
}

#[test]
#[cfg(not(miri))]
fn unrelated_coordination_shard_does_not_block_a_cold_miss() {
    let cache = CalculationCache::<u64, u64>::new(1024);
    let epoch = cache.generation.snapshot();
    let blocked_hash = flight_hash(&VersionedKey { epoch, key: 0_u64 });
    let free_key = (1_u64..)
        .find(|&key| {
            let hash = flight_hash(&VersionedKey { epoch, key });
            FlightRegistry::<u64, u64>::shard_index(hash)
                != FlightRegistry::<u64, u64>::shard_index(blocked_hash)
        })
        .unwrap();
    std::thread::scope(|scope| {
        let blocked = cache.flights.shard(blocked_hash).lock();
        let (finished_tx, finished_rx) = mpsc::sync_channel(1);
        let cache = &cache;
        let worker = scope.spawn(move || {
            let result = cache.get_or_try_insert_with(free_key, |_| 8, || Ok(free_key));
            finished_tx.send(result.map(|lease| *lease)).unwrap();
        });
        let result = finished_rx.recv_timeout(Duration::from_secs(5));
        // Release before asserting so a regression fails instead of hanging
        // the scoped join on a worker waiting for the unrelated lock.
        drop(blocked);
        worker.join().unwrap();
        assert!(matches!(result, Ok(Ok(value)) if value == free_key));
    });
    assert!(cache.flights.is_empty());
}

#[test]
#[cfg(not(miri))]
fn clear_keeps_old_followers_and_new_epoch_flights_independent() {
    let cache = CalculationCache::<u64, u64>::new(1024);
    let old_epoch = cache.generation.snapshot();
    let old_key = VersionedKey {
        epoch: old_epoch,
        key: 7_u64,
    };
    let old_hash = flight_hash(&old_key);
    std::thread::scope(|scope| {
        let (started_tx, started_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        let cache_ref = &cache;
        let leader = scope.spawn(move || {
            let lease = cache_ref
                .get_or_try_insert_with(
                    7,
                    |_| 8,
                    || {
                        started_tx.send(()).unwrap();
                        release_rx.recv().unwrap();
                        Ok(1)
                    },
                )
                .unwrap();
            assert_eq!(*lease, 1);
        });
        started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let flight = Arc::clone(
            cache
                .flights
                .shard(old_hash)
                .lock()
                .get(old_hash, &old_key)
                .unwrap(),
        );
        let follower = scope.spawn(|| {
            let lease = cache
                .get_or_try_insert_with(7, |_| 8, || panic!("old follower must coalesce"))
                .unwrap();
            assert_eq!(*lease, 1);
        });
        // The leader, registry and this observer own three references. A
        // fourth proves the follower enrolled before clear advances epoch.
        let deadline = Instant::now() + Duration::from_secs(5);
        while Arc::strong_count(&flight) < 4 && Instant::now() < deadline {
            std::thread::yield_now();
        }
        let enrolled = Arc::strong_count(&flight) >= 4;
        if !enrolled {
            release_tx.send(()).unwrap();
            leader.join().unwrap();
            follower.join().unwrap();
            panic!("old follower did not enroll");
        }
        cache.clear();
        let (fresh_tx, fresh_rx) = mpsc::sync_channel(1);
        let cache_ref = &cache;
        let fresh = scope.spawn(move || {
            let result = cache_ref.get_or_try_insert_with(7, |_| 8, || Ok(2));
            fresh_tx.send(result.map(|lease| *lease)).unwrap();
        });
        // The new epoch must complete while the old leader remains pending.
        // Unlock old callers before asserting to avoid stranding them when a
        // regression incorrectly coalesces the two generations.
        let fresh_result = fresh_rx.recv_timeout(Duration::from_secs(5));
        release_tx.send(()).unwrap();
        leader.join().unwrap();
        follower.join().unwrap();
        fresh.join().unwrap();
        assert!(matches!(fresh_result, Ok(Ok(2))));
        assert_eq!(*cache.get(&7).unwrap(), 2);
    });
    assert!(cache.flights.is_empty());
    cache.clear();
    assert_eq!(cache.domain.stats().pending_nodes, 0);
}

#[test]
#[cfg(not(miri))]
fn unwind_cleanup_does_not_call_faulting_keys_again() {
    const CASE: &str = "XLFN_TEST_SINGLEFLIGHT_CLEANUP_FAULT";
    if let Ok(case) = std::env::var(CASE) {
        let fault_mode: u8 = case.parse().unwrap();
        let fault = StdArc::new(AtomicU8::new(0));
        let key = FaultKey {
            id: 7,
            fault: StdArc::clone(&fault),
        };
        let cache = CalculationCache::<FaultKey, u8>::new(8);
        // Inspect the fixed string payload to distinguish the original panic
        // from any new panic raised while cleaning up the registered flight.
        let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
            cache.get_or_try_insert_with(
                key.clone(),
                |_| 1,
                || {
                    fault.store(fault_mode, Ordering::Relaxed);
                    panic!("original computation panic");
                },
            )
        }));
        fault.store(0, Ordering::Relaxed);
        let payload = result.expect_err("the original computation must unwind");
        assert_eq!(
            payload.downcast_ref::<&str>(),
            Some(&"original computation panic")
        );
        assert!(cache.flights.is_empty());
        assert_eq!(
            *cache.get_or_try_insert_with(key, |_| 1, || Ok(9)).unwrap(),
            9
        );
        return;
    }
    // The old cleanup invoked a faulting key during an existing unwind and
    // aborted. Isolate that possible regression from the rest of the suite.
    for case in ["1", "2"] {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "cache::singleflight_tests::unwind_cleanup_does_not_call_faulting_keys_again",
                "--nocapture",
            ])
            .env(CASE, case)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "cleanup must preserve the original unwind, case {case}: {}",
            String::from_utf8_lossy(&output.stderr),
        );
    }
}

use super::*;
use std::panic::AssertUnwindSafe;
use std::sync::Arc as StdArc;
use std::sync::atomic::{AtomicU8, AtomicUsize};

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
            assert!(cache.flights.lock().entries.is_empty());
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
            assert!(cache.flights.lock().entries.is_empty());
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
        assert!(cache.flights.lock().entries.is_empty());
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

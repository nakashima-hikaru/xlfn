use std::sync::Barrier;
use std::thread;
use xlfn::cache::CalculationCache;

#[test]
fn pre_clear_computation_cannot_republish_stale_value() {
    let cache = CalculationCache::<String, String>::new(1024);
    let start_compute = Barrier::new(2);
    let finish_compute = Barrier::new(2);

    thread::scope(|s| {
        let handle = s.spawn(|| {
            let lease = cache
                .get_or_try_insert_with("k1".to_string(), || {
                    start_compute.wait();
                    finish_compute.wait();
                    Ok("stale_v1".to_string())
                })
                .expect("computation should succeed");

            assert_eq!(&*lease, "stale_v1");
            lease
        });

        // Wait for the compute closure to begin executing under the initial epoch.
        start_compute.wait();

        // Clear the cache, advancing the generation.
        cache.clear();

        // Allow the pre-clear compute closure to finish.
        finish_compute.wait();

        let lease = handle.join().unwrap();
        // Caller still holds the valid lease even after clear.
        assert_eq!(&*lease, "stale_v1");
    });

    // The stale value must NOT have been retained in the cache.
    assert!(cache.get(&"k1".to_string()).is_none());
    assert_eq!(cache.len(), 0);

    // Subsequent request for the same key computes afresh.
    let fresh_lease = cache
        .get_or_try_insert_with("k1".to_string(), || Ok("fresh_v2".to_string()))
        .expect("fresh compute should succeed");
    assert_eq!(&*fresh_lease, "fresh_v2");
    assert_eq!(cache.len(), 1);
    assert_eq!(&*cache.get(&"k1".to_string()).unwrap(), "fresh_v2");
}

#[test]
fn follower_retrying_after_leader_panic_across_clear_does_not_pollute_new_epoch() {
    use super::single_flight::{FlightWitness, WitnessKey};
    use std::sync::{Arc, atomic::AtomicUsize, mpsc};
    use std::time::Duration;
    let cache = CalculationCache::<WitnessKey, String>::new(1024);
    let (matched_tx, matched_rx) = mpsc::sync_channel(2);
    let witness = Arc::new(FlightWitness {
        seen: AtomicUsize::new(1),
        matched: matched_tx,
    });
    let key = |caller| WitnessKey {
        value: 1,
        caller,
        witness: Arc::clone(&witness),
    };
    thread::scope(|scope| {
        let (started_tx, started_rx) = mpsc::channel();
        let (panic_tx, panic_rx) = mpsc::channel();
        let (computing_tx, computing_rx) = mpsc::channel();
        let (finish_tx, finish_rx) = mpsc::channel();
        let cache_ref = &cache;
        let leader_key = key(0);
        let leader = scope.spawn(move || {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                cache_ref
                    .get_or_try_insert_with(leader_key, || {
                        started_tx.send(()).unwrap();
                        panic_rx.recv().unwrap();
                        panic!("injected leader compute panic");
                    })
                    .map(|lease| (*lease).clone())
            }))
        });
        started_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        let follower_key = key(1);
        let follower = scope.spawn(move || {
            cache_ref
                .get_or_try_insert_with(follower_key, || {
                    computing_tx.send(()).unwrap();
                    finish_rx.recv().unwrap();
                    Ok("v1_follower_stale".to_owned())
                })
                .unwrap()
        });
        let enrolled = matched_rx.recv_timeout(Duration::from_secs(10)).is_ok();
        panic_tx.send(()).unwrap();
        let retrying = computing_rx.recv_timeout(Duration::from_secs(10)).is_ok();
        if retrying {
            cache.clear();
        }
        let _ = finish_tx.send(());
        assert!(leader.join().unwrap().is_err());
        assert_eq!(&*follower.join().unwrap(), "v1_follower_stale");
        assert!(enrolled, "follower must join the original pending flight");
        assert!(retrying, "follower must retry after leader panic");
    });
    assert!(cache.get(&key(0)).is_none());
    assert_eq!(cache.len(), 0);
    let fresh = cache
        .get_or_try_insert_with(key(0), || Ok("v2_fresh".to_owned()))
        .unwrap();
    assert_eq!(&*fresh, "v2_fresh");
    assert_eq!(cache.len(), 1);
    assert_eq!(&*cache.get(&key(0)).unwrap(), "v2_fresh");
}

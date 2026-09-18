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
                .get_or_try_insert_with(
                    "k1".to_string(),
                    |_| 1,
                    || {
                        start_compute.wait();
                        finish_compute.wait();
                        Ok("stale_v1".to_string())
                    },
                )
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
        .get_or_try_insert_with("k1".to_string(), |_| 1, || Ok("fresh_v2".to_string()))
        .expect("fresh compute should succeed");
    assert_eq!(&*fresh_lease, "fresh_v2");
    assert_eq!(cache.len(), 1);
    assert_eq!(&*cache.get(&"k1".to_string()).unwrap(), "fresh_v2");
}

#[test]
fn follower_receiving_result_across_clear_does_not_pollute_new_epoch() {
    let at_hook = std::sync::Arc::new(Barrier::new(2));
    let resume = std::sync::Arc::new(Barrier::new(2));

    let at_hook_clone = std::sync::Arc::clone(&at_hook);
    let resume_clone = std::sync::Arc::clone(&resume);
    let cache = CalculationCache::<String, String>::new_with_follower_hook(1024, move || {
        at_hook_clone.wait();
        resume_clone.wait();
    });

    let leader_started = Barrier::new(2);
    let follower_ready = Barrier::new(2);
    let leader_finish = Barrier::new(2);

    let cache_ref = &cache;
    thread::scope(|s| {
        let leader = s.spawn(|| {
            cache_ref
                .get_or_try_insert_with(
                    "k1".to_string(),
                    |_| 1,
                    || {
                        leader_started.wait();
                        leader_finish.wait();
                        Ok("v1_leader".to_string())
                    },
                )
                .unwrap()
        });

        leader_started.wait();

        let follower = s.spawn(|| {
            follower_ready.wait();
            cache_ref
                .get_or_try_insert_with(
                    "k1".to_string(),
                    |_| 1,
                    || panic!("follower compute must not be called when leader succeeds"),
                )
                .unwrap()
        });

        follower_ready.wait();
        // Allow follower to register in flight
        std::thread::sleep(std::time::Duration::from_millis(15));

        // Let leader publish result
        leader_finish.wait();

        // Wait until follower has received the result and paused at hook (before lease acquisition)
        at_hook.wait();

        // Clear the cache while follower is paused at hook
        cache.clear();

        // Resume follower
        resume.wait();

        let leader_lease = leader.join().unwrap();
        let follower_lease = follower.join().unwrap();

        assert_eq!(&*leader_lease, "v1_leader");
        assert_eq!(&*follower_lease, "v1_leader");
    });

    // The pre-clear entry must not be retained in the post-clear generation
    assert!(cache.get(&"k1".to_string()).is_none());
    assert_eq!(cache.len(), 0);

    // New request in the post-clear generation computes afresh
    let fresh = cache
        .get_or_try_insert_with("k1".to_string(), |_| 1, || Ok("v2_fresh".to_string()))
        .unwrap();
    assert_eq!(&*fresh, "v2_fresh");
    assert_eq!(cache.len(), 1);
    assert_eq!(&*cache.get(&"k1".to_string()).unwrap(), "v2_fresh");
}

#[test]
fn follower_retrying_after_leader_panic_across_clear_does_not_pollute_new_epoch() {
    let cache = CalculationCache::<String, String>::new(1024);
    let leader_started = Barrier::new(2);
    let follower_ready = Barrier::new(2);
    let follower_computing = Barrier::new(2);
    let follower_finish = Barrier::new(2);

    let cache_ref = &cache;
    thread::scope(|s| {
        let leader = s.spawn(|| {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                cache_ref
                    .get_or_try_insert_with(
                        "k1".to_string(),
                        |_| 1,
                        || {
                            leader_started.wait();
                            panic!("injected leader compute panic");
                        },
                    )
                    .unwrap();
            }));
        });

        leader_started.wait();

        let follower = s.spawn(|| {
            follower_ready.wait();
            cache_ref
                .get_or_try_insert_with(
                    "k1".to_string(),
                    |_| 1,
                    || {
                        follower_computing.wait();
                        follower_finish.wait();
                        Ok("v1_follower_stale".to_string())
                    },
                )
                .unwrap()
        });

        follower_ready.wait();
        leader.join().unwrap();

        // Follower retries and enters its own compute closure
        follower_computing.wait();

        // Clear the cache while follower is executing pre-clear computation
        cache.clear();

        follower_finish.wait();
        let follower_lease = follower.join().unwrap();

        // Follower receives its computed lease
        assert_eq!(&*follower_lease, "v1_follower_stale");
    });

    // The stale computation from pre-clear must NOT have been published to the new epoch
    assert!(cache.get(&"k1".to_string()).is_none());
    assert_eq!(cache.len(), 0);

    // Subsequent request in the new generation computes freshly
    let fresh = cache
        .get_or_try_insert_with("k1".to_string(), |_| 1, || Ok("v2_fresh".to_string()))
        .unwrap();
    assert_eq!(&*fresh, "v2_fresh");
    assert_eq!(cache.len(), 1);
    assert_eq!(&*cache.get(&"k1".to_string()).unwrap(), "v2_fresh");
}

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

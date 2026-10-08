use super::CalculationCache;
use std::sync::Barrier;
use std::thread;

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

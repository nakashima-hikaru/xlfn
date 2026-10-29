use super::CalculationCache;
use std::sync::{Mutex, mpsc};
use std::time::Duration;

#[test]
fn follower_receiving_result_across_clear_does_not_pollute_new_epoch() {
    let (at_hook_tx, at_hook_rx) = mpsc::channel();
    let (resume_tx, resume_rx) = mpsc::channel();
    let resume_rx = Mutex::new(resume_rx);
    let cache = CalculationCache::<String, String>::new_with_follower_hook(1024, move || {
        at_hook_tx.send(()).unwrap();
        resume_rx.lock().unwrap().recv().unwrap();
    });
    std::thread::scope(|scope| {
        let (started_tx, started_rx) = mpsc::channel();
        let (finish_tx, finish_rx) = mpsc::channel();
        let cache_ref = &cache;
        let leader = scope.spawn(move || {
            cache_ref
                .get_or_try_insert_weighted(
                    "k1".to_owned(),
                    |_| 1,
                    || {
                        started_tx.send(()).unwrap();
                        finish_rx.recv().unwrap();
                        Ok("v1_leader".to_owned())
                    },
                )
                .unwrap()
        });
        started_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        let flight = super::observe_pending_flight(&cache, "k1".to_owned());
        let follower = scope.spawn(|| {
            cache
                .get_or_try_insert_weighted(
                    "k1".to_owned(),
                    |_| 1,
                    || panic!("follower must coalesce"),
                )
                .unwrap()
        });
        let enrolled = super::followers_enrolled(&flight, 1);
        finish_tx.send(()).unwrap();
        let at_hook = at_hook_rx.recv_timeout(Duration::from_secs(10)).is_ok();
        if at_hook {
            cache.clear();
        }
        // Release all controlled waits before assertions, including failure paths.
        let _ = resume_tx.send(());
        assert_eq!(&*leader.join().unwrap(), "v1_leader");
        assert_eq!(&*follower.join().unwrap(), "v1_leader");
        assert!(enrolled, "follower must enroll before leader publication");
        assert!(
            at_hook,
            "follower must pause before acquiring its result lease"
        );
    });
    assert!(cache.get(&"k1".to_owned()).is_none());
    assert_eq!(cache.len(), 0);
    let fresh = cache
        .get_or_try_insert_weighted("k1".to_owned(), |_| 1, || Ok("v2_fresh".to_owned()))
        .unwrap();
    assert_eq!(&*fresh, "v2_fresh");
    assert_eq!(cache.len(), 1);
    assert_eq!(&*cache.get(&"k1".to_owned()).unwrap(), "v2_fresh");
}

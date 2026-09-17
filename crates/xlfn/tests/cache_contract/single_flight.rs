use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Duration;
use xlfn::cache::CalculationCache;

#[test]
fn same_key_concurrent_miss_coalesces() {
    let cache = Arc::new(CalculationCache::<String, String>::new(1024));
    let computes = Arc::new(AtomicUsize::new(0));
    let threads = 8;
    let barrier = Arc::new(Barrier::new(threads));

    let mut handles = Vec::new();
    for _ in 0..threads {
        let cache = Arc::clone(&cache);
        let computes = Arc::clone(&computes);
        let barrier = Arc::clone(&barrier);
        handles.push(thread::spawn(move || {
            barrier.wait();
            let lease = cache
                .get_or_try_insert_with(
                    "shared_key".to_string(),
                    |v| v.len(),
                    || {
                        computes.fetch_add(1, Ordering::SeqCst);
                        thread::sleep(Duration::from_millis(50));
                        Ok("computed_value".to_string())
                    },
                )
                .expect("computation should succeed");
            assert_eq!(&*lease, "computed_value");
        }));
    }

    for h in handles {
        h.join().unwrap();
    }

    assert_eq!(computes.load(Ordering::SeqCst), 1);
    assert_eq!(cache.len(), 1);
}

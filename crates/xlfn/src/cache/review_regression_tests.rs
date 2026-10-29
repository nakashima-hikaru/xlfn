use super::*;
use std::sync::Arc;

struct DropProbe {
    drops: Arc<AtomicUsize>,
    during_initializer: Arc<AtomicUsize>,
}

impl Drop for DropProbe {
    fn drop(&mut self) {
        if ACTIVE_CACHE_INITIALIZATION_DEPTH.get() != 0 {
            self.during_initializer.fetch_add(1, Ordering::Relaxed);
        }
        self.drops.fetch_add(1, Ordering::Release);
    }
}

fn populated_cache(
    drops: &Arc<AtomicUsize>,
    during_initializer: &Arc<AtomicUsize>,
) -> CalculationCache<u32, DropProbe> {
    let cache = CalculationCache::new(1);
    drop(
        cache
            .get_or_try_insert_weighted(
                1,
                |_| 1,
                || {
                    Ok(DropProbe {
                        drops: Arc::clone(drops),
                        during_initializer: Arc::clone(during_initializer),
                    })
                },
            )
            .unwrap(),
    );
    cache
}

#[test]
fn clearing_or_dropping_another_cache_inside_initializer_preserves_owned_retirement() {
    for drop_cache in [false, true] {
        let drops = Arc::new(AtomicUsize::new(0));
        let during_initializer = Arc::new(AtomicUsize::new(0));
        let mut other = Some(populated_cache(&drops, &during_initializer));
        let outer = CalculationCache::<u32, u32>::new(1);
        drop(
            outer
                .get_or_try_insert_weighted(
                    0,
                    |_| 1,
                    || {
                        if drop_cache {
                            drop(other.take());
                        } else {
                            other.as_ref().unwrap().clear();
                        }
                        assert_eq!(drops.load(Ordering::Acquire), 0);
                        Ok(0)
                    },
                )
                .unwrap(),
        );
        assert_eq!(drops.load(Ordering::Acquire), 1);
        assert_eq!(during_initializer.load(Ordering::Relaxed), 0);
        drop(other);
        assert_eq!(drops.load(Ordering::Acquire), 1);
    }
}

#[derive(Clone)]
struct OwnedKey {
    _owner: Arc<DropProbe>,
}

impl Hash for OwnedKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        1_u32.hash(state);
    }
}

impl PartialEq for OwnedKey {
    fn eq(&self, _other: &Self) -> bool {
        true
    }
}

impl Eq for OwnedKey {}

#[test]
fn detached_keys_outlive_their_cache_when_dropped_inside_an_initializer() {
    let drops = Arc::new(AtomicUsize::new(0));
    let during_initializer = Arc::new(AtomicUsize::new(0));
    let other = CalculationCache::<OwnedKey, u32>::new(1);
    let key = OwnedKey {
        _owner: Arc::new(DropProbe {
            drops: Arc::clone(&drops),
            during_initializer: Arc::clone(&during_initializer),
        }),
    };
    drop(
        other
            .get_or_try_insert_weighted(key, |_| 1, || Ok(1))
            .unwrap(),
    );
    let outer = CalculationCache::<u32, u32>::new(1);
    drop(
        outer
            .get_or_try_insert_weighted(
                0,
                |_| 1,
                || {
                    drop(other);
                    assert_eq!(drops.load(Ordering::Acquire), 0);
                    Ok(0)
                },
            )
            .unwrap(),
    );
    assert_eq!(drops.load(Ordering::Acquire), 1);
    assert_eq!(during_initializer.load(Ordering::Relaxed), 0);
}

#[test]
fn unrelated_cache_reader_does_not_delay_a_different_caches_clear() {
    let drops = Arc::new(AtomicUsize::new(0));
    let during_initializer = Arc::new(AtomicUsize::new(0));
    let other = populated_cache(&drops, &during_initializer);
    let reading = CalculationCache::<u32, u32>::new(1);
    let reader = reading.domain.enter().unwrap();
    other.clear();
    assert_eq!(drops.load(Ordering::Acquire), 1);
    assert_eq!(other.reclamation_stats().pending_nodes, 0);
    drop(reader);
}

#[test]
fn final_old_generation_reader_reclaims_while_replacement_reader_remains_active() {
    let drops = Arc::new(AtomicUsize::new(0));
    let during_initializer = Arc::new(AtomicUsize::new(0));
    let cache = populated_cache(&drops, &during_initializer);
    let old_reader = cache.domain.enter().unwrap();
    cache.clear();
    assert_eq!(cache.reclamation_stats().pending_nodes, 1);
    // Start nonblocking quiescence while the original generation is occupied.
    // The replacement generation must admit readers without requiring idle time.
    let old_generation = cache.domain.domain.current_generation();
    assert_eq!(cache.len(), 0);
    assert_ne!(cache.domain.domain.current_generation(), old_generation);
    let replacement_reader = cache.domain.enter().unwrap();
    assert_eq!(drops.load(Ordering::Acquire), 0);
    drop(old_reader);
    assert_eq!(drops.load(Ordering::Acquire), 1);
    assert_eq!(cache.reclamation_stats().pending_nodes, 0);
    drop(replacement_reader);
}

#[test]
#[cfg(not(miri))]
fn thread_local_cache_reclaims_after_reader_tls_teardown() {
    const CHILD: &str = "XLFN_CACHE_TLS_DROP_CHILD";
    if std::env::var_os(CHILD).is_some() {
        struct ThreadLocalCache(CalculationCache<String, u32>);
        impl Drop for ThreadLocalCache {
            fn drop(&mut self) {
                // User TLS may run after bookkeeping TLS was initialized.
                // Exercise both reader admission and the operation's final
                // deferred-destruction flush during that late destructor.
                assert_eq!(*self.0.get(&"key".to_owned()).unwrap(), 1);
            }
        }
        thread_local! {
            static CACHE: ThreadLocalCache = ThreadLocalCache(CalculationCache::new(1));
        }
        std::thread::spawn(|| {
            CACHE.with(|cache| {
                drop(
                    cache
                        .0
                        .get_or_try_insert_weighted("key".to_owned(), |_| 1, || Ok(1))
                        .unwrap(),
                );
            });
        })
        .join()
        .unwrap();
        return;
    }
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "cache::review_regression_tests::thread_local_cache_reclaims_after_reader_tls_teardown",
            "--nocapture",
        ])
        .env(CHILD, "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "TLS cache teardown failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
#[cfg_attr(miri, ignore)]
fn stripe_collision_does_not_suppress_blocking_writer_quiescence() {
    let domain = Arc::new(CacheLookupDomain::<()>::new());
    let stripe = xlfn_kernel::drain_gate::current_thread_stripe();
    let old_generation = domain.domain.current_generation();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let reading = Arc::clone(&domain);
    let reader = std::thread::spawn(move || {
        // Force the same kernel stripe as the writer, as happens after the
        // round-robin thread assignment wraps. TLS ownership is still distinct.
        let permit = reading.domain.enter(stripe).unwrap();
        ready_tx.send(()).unwrap();
        let deadline = Instant::now() + std::time::Duration::from_secs(5);
        while reading.domain.current_generation() == old_generation {
            assert!(
                Instant::now() < deadline,
                "writer never began its grace period"
            );
            std::thread::yield_now();
        }
        drop(permit);
    });
    ready_rx.recv().unwrap();
    assert!(domain.domain.current_thread_may_be_reading());
    assert!(!domain.current_thread_is_reading());
    domain.enqueue_reclaim(ReclaimEntry::sentinel(&domain, 1));
    let batch = domain.quiesce_and_drain();
    reader.join().unwrap();
    assert_eq!(batch.len(), 1);
    assert_eq!(domain.stats().pending_nodes, 0);
    // Queue-only sentinel ownership is never passed to a node destructor.
}

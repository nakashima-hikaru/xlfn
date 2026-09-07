//! The same cache-level contracts run against every resident implementation.
//! Miri runs only sharded candidates, never Moka's dependency-blocked path.

use super::*;
use std::sync::{Arc, Barrier, mpsc};

fn backends() -> Vec<CacheBackend> {
    let mut backends = Vec::new();
    if !cfg!(miri) {
        backends.push(CacheBackend::Moka);
    }
    backends.extend([8, 16, 32, 64].map(|shards| CacheBackend::Sharded { shards }));
    backends
}

#[derive(Debug)]
struct DropProbe {
    id: usize,
    drops: Arc<Vec<AtomicUsize>>,
}

impl Drop for DropProbe {
    fn drop(&mut self) {
        assert_eq!(self.drops[self.id].fetch_add(1, Ordering::SeqCst), 0);
    }
}

fn counters(count: usize) -> Arc<Vec<AtomicUsize>> {
    Arc::new((0..count).map(|_| AtomicUsize::new(0)).collect())
}

#[test]
fn eviction_preserves_live_leases_and_drops_every_value_once() {
    for backend in backends() {
        let count = if cfg!(miri) { 8 } else { 128 };
        let drops = counters(count);
        let cache = CalculationCache::new_with_backend(2, backend);
        let lease = cache
            .get_or_try_insert_with(
                0,
                |_| 1,
                || {
                    Ok(DropProbe {
                        id: 0,
                        drops: Arc::clone(&drops),
                    })
                },
            )
            .unwrap();
        for id in 1..count {
            drop(
                cache
                    .get_or_try_insert_with(
                        id,
                        |_| 1,
                        || {
                            Ok(DropProbe {
                                id,
                                drops: Arc::clone(&drops),
                            })
                        },
                    )
                    .unwrap(),
            );
        }
        // Force withdrawal of this key even if an admission policy chose to
        // retain it during capacity pressure. The lease is independent of policy.
        cache.invalidate(&0);
        cache.clear();
        assert!(cache.get(&0).is_none(), "{backend:?}");
        assert_eq!(lease.id, 0);
        assert_eq!(drops[0].load(Ordering::SeqCst), 0);
        drop(lease);
        drop(cache);
        assert!(
            drops.iter().all(|count| count.load(Ordering::SeqCst) == 1),
            "{backend:?}"
        );
    }
}

#[test]
fn clear_races_concurrent_lookup_and_initialization() {
    for backend in backends() {
        let cache = CalculationCache::new_with_backend(32, backend);
        let rounds = if cfg!(miri) { 3 } else { 128 };
        let barrier = Barrier::new(3);
        std::thread::scope(|scope| {
            for worker in 0..2 {
                let cache = &cache;
                let barrier = &barrier;
                scope.spawn(move || {
                    barrier.wait();
                    for round in 0..rounds {
                        let key = (round + worker) % 4;
                        let lease = cache
                            .get_or_try_insert_with(key, |_| 1, || Ok(key))
                            .unwrap();
                        assert_eq!(*lease, key);
                        if let Some(hit) = cache.get(&key) {
                            assert_eq!(*hit, key);
                        }
                    }
                });
            }
            barrier.wait();
            for _ in 0..rounds {
                cache.clear();
            }
        });
        cache.clear();
        assert_eq!(cache.reclamation_stats().pending_nodes, 0);
        assert_eq!(cache.len(), 0);
    }
}

#[test]
fn generation_rollover_cannot_publish_an_old_initializer_as_current() {
    for backend in backends() {
        let cache = CalculationCache::new_with_backend(16, backend);
        let (started_tx, started_rx) = mpsc::sync_channel(0);
        let (resume_tx, resume_rx) = mpsc::sync_channel(0);
        std::thread::scope(|scope| {
            let cache_ref = &cache;
            let old = scope.spawn(move || {
                let old = cache_ref
                    .get_or_try_insert_with(
                        1,
                        |_| 1,
                        || {
                            started_tx.send(()).unwrap();
                            resume_rx.recv().unwrap();
                            Ok(11)
                        },
                    )
                    .unwrap();
                assert_eq!(*old, 11);
            });
            started_rx.recv().unwrap();
            cache.clear();
            drop(cache.get_or_try_insert_with(1, |_| 1, || Ok(22)).unwrap());
            resume_tx.send(()).unwrap();
            old.join().unwrap();
        });
        assert_eq!(*cache.get(&1).unwrap(), 22, "{backend:?}");
        cache.maintenance();
        assert_eq!(cache.reclamation_stats().pending_nodes, 0);
    }
}

#[test]
fn duplicate_initializers_share_a_single_computation() {
    for backend in backends() {
        let cache = CalculationCache::new_with_backend(16, backend);
        let workers = if cfg!(miri) { 3 } else { 16 };
        let barrier = Barrier::new(workers);
        let computes = AtomicUsize::new(0);
        std::thread::scope(|scope| {
            for _ in 0..workers {
                let cache = &cache;
                let barrier = &barrier;
                let computes = &computes;
                scope.spawn(move || {
                    barrier.wait();
                    let lease = cache
                        .get_or_try_insert_with(
                            7,
                            |_| 1,
                            || {
                                computes.fetch_add(1, Ordering::SeqCst);
                                std::thread::yield_now();
                                Ok(42)
                            },
                        )
                        .unwrap();
                    assert_eq!(*lease, 42);
                });
            }
        });
        assert_eq!(computes.load(Ordering::SeqCst), 1, "{backend:?}");
    }
}

#[test]
fn reentrant_miss_is_rejected_but_hits_and_clear_remain_safe() {
    for backend in backends() {
        let cache = CalculationCache::new_with_backend(16, backend);
        drop(cache.get_or_try_insert_with(1, |_| 1, || Ok(10)).unwrap());
        let outer = cache
            .get_or_try_insert_with(
                2,
                |_| 1,
                || {
                    assert_eq!(*cache.get(&1).unwrap(), 10);
                    assert!(cache.get_or_try_insert_with(3, |_| 1, || Ok(30)).is_err());
                    cache.clear();
                    Ok(20)
                },
            )
            .unwrap();
        assert_eq!(*outer, 20);
        assert!(cache.get(&2).is_none());
        drop(outer);
        cache.maintenance();
        assert_eq!(cache.reclamation_stats().pending_nodes, 0);
    }
}

#[test]
fn scoped_reference_survives_eviction_and_debt_drains_after_scope() {
    for backend in backends() {
        let count = if cfg!(miri) { 4 } else { 64 };
        let drops = counters(count);
        let cache = CalculationCache::new_with_backend(count, backend);
        for id in 0..count {
            drop(
                cache
                    .get_or_try_insert_with(
                        id,
                        |_| 1,
                        || {
                            Ok(DropProbe {
                                id,
                                drops: Arc::clone(&drops),
                            })
                        },
                    )
                    .unwrap(),
            );
        }
        cache.maintenance();
        let scope = cache.read_scope().unwrap();
        let reference = scope.get(&0).unwrap();
        std::thread::scope(|threads| {
            let cache_ref = &cache;
            threads
                .spawn(move || {
                    cache_ref.index.clear();
                    cache_ref.index.maintenance();
                    assert_eq!(cache_ref.reclamation_stats().pending_nodes, count);
                    assert!(cache_ref.domain.try_quiesce_and_drain().is_empty());
                })
                .join()
                .unwrap();
        });
        assert_eq!(reference.id, 0);
        assert!(drops.iter().all(|count| count.load(Ordering::SeqCst) == 0));
        drop(scope);
        cache.maintenance();
        assert_eq!(cache.reclamation_stats().pending_nodes, 0);
        assert_eq!(cache.reclamation_stats().pending_weight, 0);
        assert!(drops.iter().all(|count| count.load(Ordering::SeqCst) == 1));
    }
}

#[test]
fn weighted_capacity_zero_oversized_and_drop_cleanup() {
    for backend in backends() {
        for capacity in [0, 1, 17] {
            let count = if cfg!(miri) { 6 } else { 256 };
            let drops = counters(count);
            let cache = CalculationCache::new_with_backend(capacity, backend);
            for id in 0..count {
                let weight = id % 23 + 1;
                let lease = cache
                    .get_or_try_insert_with(
                        id,
                        |_| weight,
                        || {
                            Ok(DropProbe {
                                id,
                                drops: Arc::clone(&drops),
                            })
                        },
                    )
                    .unwrap();
                if weight > capacity {
                    assert!(cache.get(&id).is_none());
                }
                drop(lease);
                cache.maintenance();
                assert!(cache.used_weight() <= capacity, "{backend:?}");
            }
            drop(cache);
            assert!(drops.iter().all(|count| count.load(Ordering::SeqCst) == 1));
        }
    }
}

#[test]
fn heavy_concurrent_read_write_drains_retirement_debt() {
    for backend in backends() {
        let workers = if cfg!(miri) { 2 } else { 16 };
        let rounds = if cfg!(miri) { 4 } else { 512 };
        let cache = CalculationCache::new_with_backend(64, backend);
        let barrier = Barrier::new(workers);
        std::thread::scope(|scope| {
            for worker in 0..workers {
                let cache = &cache;
                let barrier = &barrier;
                scope.spawn(move || {
                    barrier.wait();
                    for round in 0..rounds {
                        let key = worker * rounds + round;
                        let lease = cache
                            .get_or_try_insert_with(key, |_| 1, || Ok(key))
                            .unwrap();
                        if let Some(hit) = cache.get(&key) {
                            assert_eq!(*hit, key);
                        }
                        if round % 7 == 0 {
                            cache.invalidate(&key);
                        }
                        assert_eq!(*lease, key);
                    }
                });
            }
        });
        cache.clear();
        cache.maintenance();
        assert_eq!(cache.reclamation_stats().pending_nodes, 0, "{backend:?}");
        assert_eq!(cache.reclamation_stats().pending_weight, 0);
    }
}

#[test]
fn failed_and_panicking_initializers_do_not_leave_pending_flights() {
    for backend in backends() {
        let cache = CalculationCache::<u32, u32>::new_with_backend(16, backend);
        assert!(
            cache
                .get_or_try_insert_with(1, |_| 1, || Err(XllError::Closing))
                .is_err()
        );
        assert!(
            catch_no_unwind(AssertUnwindSafe(|| {
                let _ = cache.get_or_try_insert_with(1, |_| 1, || panic!("initializer fixture"));
            }))
            .is_err()
        );
        assert_eq!(
            *cache.get_or_try_insert_with(1, |_| 1, || Ok(9)).unwrap(),
            9
        );
    }
}

#[test]
fn panicking_initializers_drain_retired_values_after_unwinding() {
    struct DeferredDrop(Arc<AtomicUsize>);
    impl Drop for DeferredDrop {
        fn drop(&mut self) {
            assert_eq!(ACTIVE_CACHE_INITIALIZATION_DEPTH.get(), 0);
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    for backend in backends() {
        for capacity in [0, 8] {
            let drops = Arc::new(AtomicUsize::new(0));
            let cache = CalculationCache::new_with_backend(capacity, backend);
            let previous = cache
                .get_or_try_insert_with(1, |_| 1, || Ok(DeferredDrop(Arc::clone(&drops))))
                .unwrap();
            cache.clear();
            assert!(
                catch_no_unwind(AssertUnwindSafe(|| {
                    let _ = cache.get_or_try_insert_with(
                        2,
                        |_| 1,
                        || {
                            drop(previous);
                            assert_eq!(drops.load(Ordering::SeqCst), 0);
                            panic!("initializer panic after final lease release");
                        },
                    );
                }))
                .is_err()
            );
            assert_eq!(drops.load(Ordering::SeqCst), 1, "{backend:?}");
            assert_eq!(cache.reclamation_stats().pending_nodes, 0);
        }
    }
}

#[test]
fn panicking_initializers_do_not_wait_for_active_readers() {
    for backend in backends() {
        for capacity in [0, 8] {
            let drops = counters(1);
            let cache = CalculationCache::new_with_backend(capacity, backend);
            let previous = cache
                .get_or_try_insert_with(
                    0,
                    |_| 1,
                    || {
                        Ok(DropProbe {
                            id: 0,
                            drops: Arc::clone(&drops),
                        })
                    },
                )
                .unwrap();
            cache.clear();
            let permit = cache.domain.enter().unwrap();
            let (completed_tx, completed_rx) = mpsc::channel();
            let completed_before_release = std::thread::scope(|scope| {
                let cache = &cache;
                let worker = scope.spawn(move || {
                    let panicked = catch_no_unwind(AssertUnwindSafe(|| {
                        let _ = cache.get_or_try_insert_with(
                            1,
                            |_| 1,
                            || {
                                drop(previous);
                                panic!("initializer panic while another reader is active");
                            },
                        );
                    }))
                    .is_err();
                    let _ = completed_tx.send(panicked);
                });
                // A reader can be the caller waiting for this operation, or
                // an outer lookup invoking it from a key callback. Unwind
                // cleanup must not wait for that reader's permit. Release it
                // before joining even on timeout so a regression cannot hang.
                let completed =
                    completed_rx.recv_timeout(std::time::Duration::from_secs(1)) == Ok(true);
                assert_eq!(drops[0].load(Ordering::SeqCst), 0);
                drop(permit);
                assert!(crate::panic_boundary::contain_panic(worker.join()).is_ok());
                completed
            });
            // The first operation after the reader leaves services the debt.
            assert!(cache.get(&0).is_none());
            assert_eq!(drops[0].load(Ordering::SeqCst), 1);
            assert_eq!(cache.reclamation_stats().pending_nodes, 0);
            assert!(completed_before_release, "{backend:?}, capacity {capacity}");
        }
    }
}

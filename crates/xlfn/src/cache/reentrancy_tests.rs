use super::*;
use std::sync::{Arc as StdArc, Weak, mpsc};
use std::thread::JoinHandle;
use std::time::Duration;

const DEADLINE: Duration = Duration::from_secs(5);

#[derive(Clone)]
struct Key {
    value: u32,
    first_lookup: Option<StdArc<LookupSignal>>,
}

impl Key {
    fn plain(value: u32) -> Self {
        Self {
            value,
            first_lookup: None,
        }
    }
}

struct LookupSignal {
    sent: AtomicBool,
    observed: mpsc::Sender<()>,
}

impl PartialEq for Key {
    fn eq(&self, other: &Self) -> bool {
        self.value == other.value
    }
}

impl Eq for Key {}

impl Hash for Key {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.value.hash(state);
        if let Some(signal) = &self.first_lookup
            && !signal.sent.swap(true, Ordering::Relaxed)
        {
            signal.observed.send(()).unwrap();
        }
    }
}

struct Reentry {
    cache: Weak<CalculationCache<Key, DropProbe>>,
    key: u32,
    expect_error: bool,
    completed_during_drop: AtomicBool,
    workers: Mutex<Vec<JoinHandle<()>>>,
}

impl Reentry {
    fn new(
        cache: &StdArc<CalculationCache<Key, DropProbe>>,
        key: u32,
        expect_error: bool,
    ) -> StdArc<Self> {
        StdArc::new(Self {
            cache: StdArc::downgrade(cache),
            key,
            expect_error,
            completed_during_drop: AtomicBool::new(false),
            workers: Mutex::new(Vec::new()),
        })
    }

    fn assert_completed(&self) {
        for worker in std::mem::take(&mut *self.workers.lock()) {
            worker.join().unwrap();
        }
        assert!(self.completed_during_drop.load(Ordering::Acquire));
    }
}

struct DropProbe(Option<StdArc<Reentry>>);

impl Drop for DropProbe {
    fn drop(&mut self) {
        let Some(reentry) = self.0.take() else {
            return;
        };
        let cache = reentry.cache.upgrade().unwrap();
        let key = reentry.key;
        let expect_error = reentry.expect_error;
        let (done_tx, done_rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let result =
                cache.get_or_try_insert_with(Key::plain(key), |_| 1, || Ok(DropProbe(None)));
            let expected = if expect_error {
                matches!(result, Err(XllError::Overloaded))
            } else {
                result.is_ok()
            };
            let _ = done_tx.send(expected);
        });
        reentry.workers.lock().push(worker);
        // A regression must fail without hanging the test process: after
        // this bounded wait the outer cache call releases its lock, allowing
        // the worker to finish before assert_completed joins it.
        reentry.completed_during_drop.store(
            done_rx.recv_timeout(DEADLINE) == Ok(true),
            Ordering::Release,
        );
    }
}

#[test]
fn double_checked_hit_reclaims_after_singleflight_unlock() {
    let cache = StdArc::new(CalculationCache::new(8));
    let reentry = Reentry::new(&cache, 3, false);
    let previous = cache
        .get_or_try_insert_with(
            Key::plain(0),
            |_| 1,
            || Ok(DropProbe(Some(StdArc::clone(&reentry)))),
        )
        .unwrap();
    cache.clear();

    let flights = cache.flights.lock();
    let (observed_tx, observed_rx) = mpsc::channel();
    let worker_cache = StdArc::clone(&cache);
    let computed = StdArc::new(AtomicBool::new(false));
    let worker_computed = StdArc::clone(&computed);
    let caller = std::thread::spawn(move || {
        let key = Key {
            value: 1,
            first_lookup: Some(StdArc::new(LookupSignal {
                sent: AtomicBool::new(false),
                observed: observed_tx,
            })),
        };
        worker_cache
            .get_or_try_insert_with(
                key,
                |_| 1,
                || {
                    worker_computed.store(true, Ordering::Relaxed);
                    Ok(DropProbe(None))
                },
            )
            .is_ok()
    });
    observed_rx.recv_timeout(DEADLINE).unwrap();
    // Hash runs under lookup admission. With the flight table locked, an
    // idle read domain proves the first miss finished and the caller cannot
    // have reached its second lookup yet.
    let deadline = Instant::now() + DEADLINE;
    loop {
        if let Some(result) = cache.domain.domain.try_quiesce_if_idle(|_| ()) {
            result.unwrap();
            break;
        }
        assert!(Instant::now() < deadline, "first lookup did not finish");
        std::thread::yield_now();
    }

    // Model another initializer publishing between the two lookups without
    // requiring it to acquire the flight table that this test owns.
    let epoch = cache.generation.snapshot();
    let node = Box::new(CacheNode {
        value: DropProbe(None),
        pins: AtomicUsize::new(1),
        resident: AtomicBool::new(true),
        published: true,
        weight: 1,
        generation: epoch,
        domain: NonNull::from(&*cache.domain),
    });
    let pointer = NodePtr(NonNull::from(Box::leak(node)));
    cache.index.insert_resident(
        &VersionedKey {
            epoch,
            key: Key::plain(1),
        },
        ResidentEntry::new((pointer, 1)),
    );
    // Queue the old value for the caller's maintenance, without reclaiming
    // it on this setup thread.
    let initialization = ActiveCacheGuard::enter().unwrap();
    drop(previous);
    drop(initialization);
    assert_eq!(cache.reclamation_stats().pending_nodes, 1);
    drop(flights);

    assert!(caller.join().unwrap());
    assert!(!computed.load(Ordering::Relaxed));
    reentry.assert_completed();
    assert_eq!(cache.reclamation_stats().pending_nodes, 0);
}

#[test]
fn failed_follower_reclaims_after_flight_state_unlock() {
    let cache = StdArc::new(CalculationCache::new(8));
    let reentry = Reentry::new(&cache, 1, true);
    let previous = cache
        .get_or_try_insert_with(
            Key::plain(0),
            |_| 1,
            || Ok(DropProbe(Some(StdArc::clone(&reentry)))),
        )
        .unwrap();
    cache.clear();

    // Keep the failed flight registered, exactly as between the leader's
    // completion notification and its removal from the flight table.
    let flight = Arc::new(Flight::new(VersionedKey {
        epoch: cache.generation.snapshot(),
        key: Key::plain(1),
    }));
    *flight.state.lock() = FlightState::Finished(Err(Arc::new(XllError::Overloaded)));
    assert!(
        cache
            .flights
            .lock()
            .insert(FlightHandle(Arc::clone(&flight)))
    );
    let initialization = ActiveCacheGuard::enter().unwrap();
    drop(previous);
    drop(initialization);
    assert_eq!(cache.reclamation_stats().pending_nodes, 1);

    let result =
        cache.get_or_try_insert_with(Key::plain(1), |_| 1, || panic!("must join failed flight"));
    assert!(matches!(result, Err(XllError::Overloaded)));
    reentry.assert_completed();
    assert_eq!(cache.reclamation_stats().pending_nodes, 0);
    assert!(cache.flights.lock().take(&flight.key).is_some());
}

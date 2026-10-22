use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Duration;
use xlfn::cache::CalculationCache;

#[test]
fn same_key_concurrent_miss_coalesces() {
    concurrent_initialization(1024, 4);
}

#[test]
fn zero_budget_still_singleflights_concurrent_initialization() {
    concurrent_initialization(0, 4);
}

#[test]
fn overweight_still_singleflights_concurrent_initialization() {
    concurrent_initialization(1, 100);
}

pub(super) struct FlightWitness {
    pub(super) seen: AtomicUsize,
    pub(super) matched: mpsc::SyncSender<()>,
}

#[derive(Clone)]
pub(super) struct WitnessKey {
    pub(super) value: u32,
    pub(super) caller: usize,
    pub(super) witness: Arc<FlightWitness>,
}

impl Hash for WitnessKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.value.hash(state);
    }
}

impl PartialEq for WitnessKey {
    fn eq(&self, other: &Self) -> bool {
        if self.value != other.value {
            return false;
        }
        // Equality and hashing depend only on value. While the leader is held pending, no resident exists, so a successful
        // comparison witnesses a caller finding the registered flight. Record
        // each caller once without waiting or calling back into the cache.
        let compared = (1 << self.caller) | (1 << other.caller);
        let previous = self.witness.seen.fetch_or(compared, Ordering::SeqCst);
        for _ in 0..(compared & !previous).count_ones() {
            // The channel holds every possible unique follower notification,
            // so sending cannot block while the cache holds its table lock.
            let _ = self.witness.matched.send(());
        }
        true
    }
}

impl Eq for WitnessKey {}

fn concurrent_initialization(budget: usize, weight: usize) {
    const THREADS: usize = 16;
    let cache = CalculationCache::<WitnessKey, u32>::new(budget);
    let calls = AtomicUsize::new(0);
    let (matched_tx, matched_rx) = mpsc::sync_channel(THREADS);
    let witness = Arc::new(FlightWitness {
        // Caller zero is the leader. Only follower comparisons notify us.
        seen: AtomicUsize::new(1),
        matched: matched_tx,
    });

    std::thread::scope(|s| {
        let (started_tx, started_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        let leader_key = WitnessKey {
            value: 7,
            caller: 0,
            witness: Arc::clone(&witness),
        };
        let leader_cache = &cache;
        let leader_calls = &calls;
        s.spawn(move || {
            let lease = leader_cache
                .get_or_try_insert_with(
                    leader_key,
                    |_| weight,
                    || {
                        leader_calls.fetch_add(1, Ordering::SeqCst);
                        started_tx.send(()).unwrap();
                        release_rx.recv().expect("driver releases pending leader");
                        Ok(49)
                    },
                )
                .unwrap();
            assert_eq!(*lease, 49);
        });
        started_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("leader registered its pending flight");

        for caller in 1..THREADS {
            let key = WitnessKey {
                value: 7,
                caller,
                witness: Arc::clone(&witness),
            };
            let cache = &cache;
            s.spawn(move || {
                let lease = cache
                    .get_or_try_insert_with(
                        key,
                        |_| weight,
                        || unreachable!("an enrolled follower must reuse its pending flight"),
                    )
                    .unwrap();
                assert_eq!(*lease, 49);
            });
        }
        let enrolled =
            (1..THREADS).all(|_| matched_rx.recv_timeout(Duration::from_secs(10)).is_ok());
        release_tx.send(()).unwrap();
        assert!(enrolled, "each follower matched the still-pending flight");
        assert_eq!(witness.seen.load(Ordering::SeqCst), (1 << THREADS) - 1);
    });

    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(cache.len(), usize::from(weight <= budget));
}

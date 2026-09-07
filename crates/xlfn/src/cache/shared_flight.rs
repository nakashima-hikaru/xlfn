//! Miss-only coordination. Flights own no cache nodes or residency obligations.
use crate::XllError;
use parking_lot::{Condvar, Mutex};
use std::{collections::HashMap, hash::Hash, sync::Arc};

enum State {
    Computing,
    Finished(Result<(), Arc<XllError>>),
    Abandoned,
}

struct Flight {
    state: Mutex<State>,
    wake: Condvar,
}

pub(super) struct Flights<K> {
    pending: Mutex<HashMap<Arc<K>, Arc<Flight>>>,
}

impl<K> Default for Flights<K> {
    fn default() -> Self {
        Self {
            pending: Mutex::new(HashMap::new()),
        }
    }
}

// Keeping the key alive also prevents user key destructors running when the
// registry removes its Arc under the lock. Cleanup never hashes a user key.
struct Leader<'a, K> {
    flights: &'a Flights<K>,
    _key: Arc<K>,
    flight: Arc<Flight>,
    completed: bool,
}

impl<K> Leader<'_, K> {
    fn finish(&mut self, state: State) {
        let mut pending = self.flights.pending.lock();
        *self.flight.state.lock() = state;
        pending.retain(|_, flight| !Arc::ptr_eq(flight, &self.flight));
        self.completed = true;
        drop(pending);
        self.flight.wake.notify_all();
    }
}

impl<K> Drop for Leader<'_, K> {
    fn drop(&mut self) {
        if !self.completed {
            self.finish(State::Abandoned);
        }
    }
}

impl<K: Eq + Hash> Flights<K> {
    /// Some is this call's result; None asks a successful follower to relookup
    /// under its existing admission domain. Errors are shared only by callers
    /// attached to this flight. Unwinding wakes followers to retry leadership.
    pub(super) fn run<T>(
        &self,
        key: K,
        initialize: impl FnOnce() -> Result<T, Arc<XllError>>,
    ) -> Result<Option<T>, Arc<XllError>> {
        let key = Arc::new(key);
        loop {
            let (flight, leader) = {
                let mut pending = self.pending.lock();
                if let Some(flight) = pending.get(&key) {
                    (Arc::clone(flight), false)
                } else {
                    let flight = Arc::new(Flight {
                        state: Mutex::new(State::Computing),
                        wake: Condvar::new(),
                    });
                    pending.insert(Arc::clone(&key), Arc::clone(&flight));
                    (flight, true)
                }
            };
            if leader {
                let mut guard = Leader {
                    flights: self,
                    _key: key,
                    flight,
                    completed: false,
                };
                let result = initialize();
                guard.finish(State::Finished(
                    result.as_ref().map(|_| ()).map_err(Arc::clone),
                ));
                return result.map(Some);
            }
            let mut state = flight.state.lock();
            loop {
                match &*state {
                    State::Computing => flight.wake.wait(&mut state),
                    State::Finished(result) => return result.clone().map(|()| None),
                    State::Abandoned => break,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc,
    };

    impl<K: Eq + Hash> Flights<K> {
        pub(in crate::cache) fn wait_for_followers(&self, key: &K, count: usize) {
            loop {
                if self
                    .pending
                    .lock()
                    .get(key)
                    .is_some_and(|f| Arc::strong_count(f) >= count + 2)
                {
                    return;
                }
                std::thread::yield_now();
            }
        }
        pub(in crate::cache) fn pending_count(&self) -> usize {
            self.pending.lock().len()
        }
    }

    #[test]
    fn errors_share_one_arc_and_completed_flights_are_removed() {
        let flights = Flights::default();
        let error = Arc::new(XllError::Closing);
        let (started_tx, started_rx) = mpsc::sync_channel(0);
        let (release_tx, release_rx) = mpsc::sync_channel(0);
        std::thread::scope(|threads| {
            let flights = &flights;
            let error = &error;
            let leader = threads.spawn(move || {
                flights.run(7, || -> Result<(), _> {
                    started_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    Err(Arc::clone(error))
                })
            });
            started_rx.recv().unwrap();
            let follower = threads.spawn(|| flights.run(7, || Ok(())));
            flights.wait_for_followers(&7, 1);
            // Different keys can complete while the first initializer is blocked.
            assert_eq!(flights.run(8, || Ok(42)).unwrap(), Some(42));
            release_tx.send(()).unwrap();
            assert!(Arc::ptr_eq(&leader.join().unwrap().unwrap_err(), error));
            assert!(Arc::ptr_eq(&follower.join().unwrap().unwrap_err(), error));
        });
        assert_eq!(flights.pending_count(), 0);
        assert_eq!(flights.run(7, || Ok(42)).unwrap(), Some(42));
        assert_eq!(flights.pending_count(), 0);
    }

    #[test]
    fn panic_wakes_followers_and_success_never_stores_the_value() {
        let flights = Flights::default();
        let computes = AtomicUsize::new(0);
        let (started_tx, started_rx) = mpsc::sync_channel(0);
        let (release_tx, release_rx) = mpsc::sync_channel(0);
        std::thread::scope(|threads| {
            let flights = &flights;
            let leader = threads.spawn(move || {
                crate::panic_boundary::catch_no_unwind(std::panic::AssertUnwindSafe(|| {
                    flights.run(7, || -> Result<(), _> {
                        started_tx.send(()).unwrap();
                        release_rx.recv().unwrap();
                        panic!("leader panic");
                    })
                }))
            });
            started_rx.recv().unwrap();
            let follower = threads.spawn(|| {
                flights.run(7, || {
                    computes.fetch_add(1, Ordering::Relaxed);
                    Ok(42)
                })
            });
            flights.wait_for_followers(&7, 1);
            release_tx.send(()).unwrap();
            assert!(leader.join().unwrap().is_err());
            assert_eq!(follower.join().unwrap().unwrap(), Some(42));
        });
        assert_eq!(computes.load(Ordering::Relaxed), 1);
        assert_eq!(flights.pending_count(), 0);
    }
}

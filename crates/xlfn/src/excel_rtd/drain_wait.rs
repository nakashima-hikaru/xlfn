//! Single transfer of a borrowed drain closure across fallible thread startup.
//!
//! `spawn_scoped` drops its capture when startup fails. The closure therefore
//! stays in caller-owned storage until either a successfully started worker or
//! the failed startup owner consumes its exclusive claim.
use std::mem::ManuallyDrop;
use std::sync::Mutex;

struct WaitSlot<F> {
    value: ManuallyDrop<F>,
    armed: bool,
}

impl<F> Drop for WaitSlot<F> {
    fn drop(&mut self) {
        if self.armed {
            // SAFETY: no successful claim has moved the value. The slot is
            // destroyed only after the scoped operation and workers end.
            unsafe { ManuallyDrop::drop(&mut self.value) };
        }
    }
}

pub(super) struct DrainWaitStartup<'wait, F> {
    slot: &'wait Mutex<WaitSlot<F>>,
}

struct WorkerWait<'wait, F> {
    slot: &'wait Mutex<WaitSlot<F>>,
}

pub(super) fn with_drain_wait<F, R>(
    value: F,
    operation: impl for<'wait> FnOnce(DrainWaitStartup<'wait, F>) -> R,
) -> R {
    let slot = Mutex::new(WaitSlot {
        value: ManuallyDrop::new(value),
        armed: true,
    });
    operation(DrainWaitStartup { slot: &slot })
}

impl<F> DrainWaitStartup<'_, F> {
    pub(super) fn recover(self) -> F {
        // SAFETY: this owner has not started a worker, or spawn_scoped returned
        // Err after destroying its worker capture without executing it.
        unsafe { take_claimed(self.slot) }
    }
}

impl<'wait, F: Send> DrainWaitStartup<'wait, F> {
    pub(super) fn spawn<'scope, 'env, T: Send + 'scope>(
        self,
        scope: &'scope std::thread::Scope<'scope, 'env>,
        builder: std::thread::Builder,
        operation: impl FnOnce(F) -> T + Send + 'scope,
    ) -> Result<std::thread::ScopedJoinHandle<'scope, T>, (std::io::Error, F)>
    where
        'wait: 'scope,
        F: 'scope,
    {
        self.spawn_inner(scope, builder, operation, false)
    }

    fn spawn_inner<'scope, 'env, T: Send + 'scope>(
        self,
        scope: &'scope std::thread::Scope<'scope, 'env>,
        builder: std::thread::Builder,
        operation: impl FnOnce(F) -> T + Send + 'scope,
        inject_failure: bool,
    ) -> Result<std::thread::ScopedJoinHandle<'scope, T>, (std::io::Error, F)>
    where
        'wait: 'scope,
        F: 'scope,
    {
        let worker = WorkerWait { slot: self.slot };
        let work = move || operation(worker.take());
        let spawned = if cfg!(test) && inject_failure {
            drop(work);
            Err(std::io::Error::other("injected drain worker spawn failure"))
        } else {
            builder.spawn_scoped(scope, work)
        };
        match spawned {
            Ok(worker) => Ok(worker),
            Err(error) => Err((error, self.recover())),
        }
    }
}

impl<F> WorkerWait<'_, F> {
    fn take(self) -> F {
        // SAFETY: this unique claim runs only after spawn_scoped succeeds.
        // Startup recovery is then unavailable, and this claim is consumed.
        unsafe { take_claimed(self.slot) }
    }
}

unsafe fn take_claimed<F>(slot: &Mutex<WaitSlot<F>>) -> F {
    let mut slot = slot.lock().unwrap_or_else(|error| error.into_inner());
    // The private claim, not a caller-controlled boolean, authorizes moving F.
    // `armed` only ensures ordinary Drop cannot destroy that moved value.
    slot.armed = false;
    // SAFETY: the consuming startup/worker claim proves the slot is initialized
    // and no other claim can consume it on this startup outcome.
    unsafe { ManuallyDrop::take(&mut slot.value) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Probe<'a>(&'a AtomicUsize);
    impl Drop for Probe<'_> {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[test]
    fn miri_drain_startup_recovers_the_unexecuted_owner_once() {
        let dropped = AtomicUsize::new(0);
        let value = with_drain_wait(Probe(&dropped), |startup| startup.recover());
        assert_eq!(dropped.load(Ordering::Relaxed), 0);
        drop(value);
        assert_eq!(dropped.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn miri_failed_drain_spawn_returns_owner_without_running_or_dropping_it() {
        let dropped = AtomicUsize::new(0);
        let called = AtomicUsize::new(0);
        let value = with_drain_wait(Probe(&dropped), |startup| {
            std::thread::scope(|scope| {
                let result = startup.spawn_inner(
                    scope,
                    std::thread::Builder::new(),
                    |value| {
                        called.fetch_add(1, Ordering::Relaxed);
                        drop(value);
                    },
                    true,
                );
                match result {
                    Err((_, value)) => value,
                    Ok(_) => panic!("injected failure starts no worker"),
                }
            })
        });
        assert_eq!(called.load(Ordering::Relaxed), 0);
        assert_eq!(dropped.load(Ordering::Relaxed), 0);
        drop(value);
        assert_eq!(dropped.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn miri_drain_startup_transfers_once_and_worker_panic_does_not_redrop() {
        let dropped = AtomicUsize::new(0);
        with_drain_wait(Probe(&dropped), |startup| {
            std::thread::scope(|scope| {
                let worker = startup
                    .spawn(scope, std::thread::Builder::new(), |value| {
                        drop(value);
                        panic!("injected drain worker panic");
                    })
                    .unwrap_or_else(|_| panic!("worker starts"));
                assert!(crate::panic_boundary::contain_panic(worker.join()).is_err());
            });
        });
        assert_eq!(dropped.load(Ordering::Relaxed), 1);
    }

    #[test]
    #[allow(
        clippy::forget_non_drop,
        reason = "abandoning an unstarted capability leaves its caller-owned closure initialized"
    )]
    fn miri_abandoned_drain_startup_preserves_owner_destruction() {
        let dropped = AtomicUsize::new(0);
        with_drain_wait(Probe(&dropped), |startup| {
            std::mem::forget(startup);
        });
        assert_eq!(dropped.load(Ordering::Relaxed), 1);
    }
}

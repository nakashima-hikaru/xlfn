//! Poller ownership through thread startup, joining, and final self-destruction.

use super::{PoolShared, is_current_scheduler, pool_failure, worker};
#[cfg(test)]
use crate::sync::Mutex;
use crate::{XllError, XllResult};
use async_task::Runnable;
use crossbeam_deque::Worker;
use crossbeam_utils::sync::Parker;
use std::mem::DropGuard;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread::{self, JoinHandle};
use triomphe::Arc;
use xlfn_kernel::invariant::{checked_atomic_dec_release, fail_stop};
use xlfn_kernel::operation_gate::OperationGate;
use xlfn_kernel::rollback::{Rollback, RollbackGuard};

// A panic before either OS operation returns leaves its outcome unknown.
// Abort before unwinding can release allocations used by a live poller.
fn require_thread_outcome<T>(operation: impl FnOnce() -> T) -> T {
    let pending = DropGuard::new((), |()| fail_stop());
    let outcome = operation();
    DropGuard::dismiss(pending);
    outcome
}

pub(super) struct PollerPrepared<'pool> {
    index: usize,
    shared: &'pool Arc<PoolShared>,
    local: Worker<Runnable>,
    parker: Parker,
    #[cfg(test)]
    inject_failure: bool,
}

pub(super) struct RunningPoller(RollbackGuard<UnjoinedThread>);

struct UnjoinedThread(JoinHandle<()>);

impl Rollback for UnjoinedThread {
    fn rollback(self) {
        fail_stop();
    }
}

pub(super) struct JoinedPoller {
    outcome: XllResult<()>,
}

impl PollerPrepared<'_> {
    pub(super) fn spawn(self) -> XllResult<RunningPoller> {
        let Self {
            index,
            shared,
            local,
            parker,
            #[cfg(test)]
            inject_failure,
        } = self;
        let builder = thread::Builder::new().name(format!("xlfn-async-poller-{index}"));
        let poller_shared = Arc::clone(shared);
        shared.live_pollers.fetch_add(1, Ordering::Relaxed);
        let spawned = require_thread_outcome(|| {
            #[cfg(test)]
            if inject_failure {
                return Err(std::io::Error::from(std::io::ErrorKind::Other));
            }
            #[cfg(all(test, not(miri)))]
            if index != 0 {
                tests::inject_thread_fault("spawn");
            }
            builder.spawn(move || worker::run_poller(index, poller_shared, local, parker))
        });
        match spawned {
            Ok(thread) => Ok(RunningPoller(RollbackGuard::new(UnjoinedThread(thread)))),
            Err(_) => {
                let _ = checked_atomic_dec_release(&shared.live_pollers);
                Err(pool_failure())
            }
        }
    }
}

impl RunningPoller {
    fn is_current_thread(&self) -> bool {
        self.0.get().0.thread().id() == thread::current().id()
    }

    pub(super) fn join(self) -> JoinedPoller {
        if self.is_current_thread() {
            fail_stop();
        }
        let outcome = require_thread_outcome(|| {
            let thread = self.0.complete().0;
            #[cfg(all(test, not(miri)))]
            tests::inject_thread_fault("join");
            thread.join()
        });
        JoinedPoller {
            outcome: crate::panic_boundary::contain_panic(outcome).map_err(|_| XllError::Panic),
        }
    }

    fn handoff_to_current_poller(self, shared: &Arc<PoolShared>) {
        if !worker::is_current_poller(shared)
            || !self.is_current_thread()
            || !shared.queue.is_sealed()
        {
            fail_stop();
        }
        // The matching PollerExitGuard still owns the shared allocation and
        // local queue. The sealed loop exits and releases them on this thread.
        drop(self.0.complete().0);
    }
}

pub(super) struct PollerPool<S> {
    state: S,
}

pub(super) struct Running {
    rollback: RollbackGuard<RunningPool>,
}

struct RunningPool {
    shared: Arc<PoolShared>,
    pollers: Vec<RunningPoller>,
}

pub(super) struct Joined {
    outcome: XllResult<()>,
}

enum PoolExit {
    Joined(PollerPool<Joined>),
    DeferredOnCurrentThread,
}

impl PollerPool<Running> {
    pub(super) fn start(
        poller_count: usize,
        #[cfg(test)] fail_at: Option<usize>,
    ) -> XllResult<Self> {
        let mut local_pollers = Vec::with_capacity(poller_count);
        let mut stealers = Vec::with_capacity(poller_count);
        let mut unparkers = Vec::with_capacity(poller_count);
        for _ in 0..poller_count {
            let local = Worker::new_fifo();
            stealers.push(local.stealer());
            let parker = Parker::new();
            unparkers.push(parker.unparker().clone());
            local_pollers.push((local, parker));
        }
        let shared = Arc::new(PoolShared {
            queue: super::RunnableQueue::new(
                stealers.into_boxed_slice(),
                unparkers.into_boxed_slice(),
            ),
            scheduler_callbacks: OperationGate::new(),
            live_pollers: AtomicUsize::new(0),
            failed: AtomicBool::new(false),
            closing: AtomicBool::new(false),
            #[cfg(test)]
            before_poll_hook: Mutex::new(None),
            #[cfg(test)]
            after_scheduler_drop_hook: Mutex::new(None),
        });
        let mut pool = RollbackGuard::new(RunningPool {
            shared,
            pollers: Vec::with_capacity(poller_count),
        });
        for (index, (local, parker)) in local_pollers.into_iter().enumerate() {
            let prepared = PollerPrepared {
                index,
                shared: &pool.get().shared,
                local,
                parker,
                #[cfg(test)]
                inject_failure: fail_at == Some(index),
            };
            // A known spawn failure drops the pool guard, stopping and joining
            // every handle installed by a previous successful spawn.
            let running = prepared.spawn()?;
            pool.get_mut().pollers.push(running);
        }
        Ok(Self {
            state: Running { rollback: pool },
        })
    }

    pub(super) fn shared(&self) -> &Arc<PoolShared> {
        &self.state.rollback.get().shared
    }

    pub(super) fn shutdown(self) -> PollerPool<Joined> {
        match self.state.rollback.complete().stop() {
            PoolExit::Joined(joined) => joined,
            // The executor's explicit shutdown rejects poller and scheduler
            // reentry before extracting the pool from its lifecycle state.
            PoolExit::DeferredOnCurrentThread => fail_stop(),
        }
    }
}

impl PollerPool<Joined> {
    pub(super) fn finish(self) -> XllResult<()> {
        self.state.outcome
    }
}

impl RunningPool {
    fn stop(self) -> PoolExit {
        let Self { shared, pollers } = self;
        shared.closing.store(true, Ordering::Release);
        let self_callback = is_current_scheduler(&shared);
        shared.scheduler_callbacks.begin_close();
        if !self_callback {
            shared.scheduler_callbacks.close_and_wait_begin().wait();
        }
        shared.queue.seal_and_wake_all();
        // A rejected runnable's destructor may own the final executor. The
        // scheduler closure retains shared state through admission release.
        let deferred = self_callback || worker::is_current_poller(&shared);
        let mut outcome = Ok(());
        for poller in pollers {
            if poller.is_current_thread() {
                poller.handoff_to_current_poller(&shared);
            } else {
                outcome = outcome.and(poller.join().outcome);
            }
        }
        if deferred {
            PoolExit::DeferredOnCurrentThread
        } else {
            debug_assert_eq!(shared.live_pollers.load(Ordering::Acquire), 0);
            PoolExit::Joined(PollerPool {
                state: Joined { outcome },
            })
        }
    }
}

impl Rollback for RunningPool {
    fn rollback(self) {
        let _ = self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn miri_worker_panic_returns_a_joined_poller() {
        let thread = thread::spawn(|| panic!("poller panicked"));
        let running = RunningPoller(RollbackGuard::new(UnjoinedThread(thread)));
        assert!(matches!(running.join().outcome, Err(XllError::Panic)));
    }

    #[cfg(not(miri))]
    const THREAD_FAULT: &str = "XLFN_ASYNC_UNKNOWN_THREAD_OUTCOME_CHILD";

    #[cfg(not(miri))]
    pub(super) fn inject_thread_fault(operation: &str) {
        assert!(
            std::env::var(THREAD_FAULT).ok().as_deref() != Some(operation),
            "thread operation did not return"
        );
    }

    #[cfg(not(miri))]
    #[test]
    fn unknown_os_thread_outcome_fail_stops() {
        if std::env::var_os(THREAD_FAULT).is_some() {
            let _ = crate::panic_boundary::catch_no_unwind(|| {
                PollerPool::start(2, None)
                    .unwrap()
                    .shutdown()
                    .finish()
                    .unwrap();
            });
            // A caught unwind has allowed cleanup with an unknown outcome.
            std::process::exit(77);
        }

        for operation in ["spawn", "join"] {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "async_udf::builtin::lifetime::tests::unknown_os_thread_outcome_fail_stops",
                    "--nocapture",
                ])
                .env(THREAD_FAULT, operation)
                .output()
                .unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::process::ExitStatusExt;
                assert_eq!(output.status.signal(), Some(libc::SIGABRT), "{operation}");
            }
            #[cfg(not(unix))]
            {
                assert!(!output.status.success());
                assert_ne!(output.status.code(), Some(77));
            }
            assert!(
                String::from_utf8_lossy(&output.stderr).contains("thread operation did not return")
            );
        }
    }
}

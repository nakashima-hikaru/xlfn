//! The worker and its pointer target are issued and retired as one owner.

use super::{DiagnosticObserver, DiagnosticObserverPtr, QueuedDiagnosticEvent};
use crate::diagnostics::{DiagnosticInitError, DiagnosticSink};
use crate::panic_boundary::{catch_no_unwind, contain_panic};
use std::io;
use std::mem::DropGuard;
use std::panic::AssertUnwindSafe;
use std::sync::mpsc::{self, SyncSender};
use std::thread::{JoinHandle, ThreadId};
use xlfn_kernel::invariant::fail_stop;
use xlfn_kernel::published_owner::PublishedOwner;
use xlfn_kernel::rollback::{Rollback, RollbackGuard};

pub(super) struct DiagnosticWorker {
    running: RollbackGuard<RunningDiagnosticWorker>,
}

struct RunningDiagnosticWorker {
    handle: JoinHandle<()>,
    observer: PublishedOwner<DiagnosticObserver>,
}

/// Joining is the only operation that issues access to a completed worker.
pub(super) struct JoinedDiagnosticWorker {
    observer: PublishedOwner<DiagnosticObserver>,
    panicked: bool,
}

impl DiagnosticWorker {
    pub(super) fn spawn<S: DiagnosticSink>(
        sink: S,
        name: &str,
    ) -> Result<(SyncSender<QueuedDiagnosticEvent>, Self), DiagnosticInitError> {
        if name.as_bytes().contains(&0) {
            return Err(DiagnosticInitError::WorkerSpawn(io::Error::new(
                io::ErrorKind::InvalidInput,
                "diagnostic worker name contains NUL",
            )));
        }
        let (sender, receiver) =
            mpsc::sync_channel::<QueuedDiagnosticEvent>(super::super::DIAGNOSTIC_QUEUE_CAPACITY);
        let observer = PublishedOwner::from_box(DiagnosticObserver::new());
        let pointer = DiagnosticObserverPtr(observer.as_non_null());
        let builder = std::thread::Builder::new().name(name.to_owned());
        let run = move || {
            let worker_observer = pointer;
            while let Ok(queued) = receiver.recv() {
                let credit = queued.credit.dequeue();
                queued.event.deliver(&sink);
                drop(credit);
                crate::ingress::with_diagnostic_linearization(|| {
                    // SAFETY: this module retains the exact pointer target
                    // until its corresponding worker has joined.
                    let observer = unsafe { worker_observer.0.as_ref() };
                    observer.record(crate::shutdown_trace::ShutdownEvent::FlushDiagnostic);
                    observer.decrement_pending();
                });
            }
            if catch_no_unwind(AssertUnwindSafe(|| drop(sink))).is_err() {
                // SAFETY: the joined owner retains the observer through the
                // worker's final cleanup, including this destructor outcome.
                let observer = unsafe { worker_observer.0.as_ref() };
                observer
                    .sink_disposal_panicked
                    .store(true, std::sync::atomic::Ordering::Relaxed);
            }
        };
        // A spawn unwind could leave a live worker borrowing the observer.
        let require_spawn = DropGuard::new((), |()| fail_stop());
        let started = builder.spawn(run);
        #[cfg(test)]
        FAIL_SPAWN.with(|fault| assert!(!fault.replace(false), "injected diagnostic spawn unwind"));
        DropGuard::dismiss(require_spawn);
        let handle = started.map_err(DiagnosticInitError::WorkerSpawn)?;
        Ok((sender, Self::issued(observer, handle)))
    }

    // The observer/worker pairing is private to the operation that spawned it.
    fn issued(observer: PublishedOwner<DiagnosticObserver>, handle: JoinHandle<()>) -> Self {
        Self {
            running: RollbackGuard::new(RunningDiagnosticWorker { handle, observer }),
        }
    }

    pub(super) fn observer(&self) -> &DiagnosticObserver {
        &self.running.get().observer
    }

    pub(super) fn thread_id(&self) -> ThreadId {
        self.running.get().thread_id()
    }

    pub(super) fn is_current_thread(&self) -> bool {
        std::thread::current().id() == self.thread_id()
    }

    pub(super) fn join(self) -> JoinedDiagnosticWorker {
        self.running.complete().join()
    }

    #[cfg(test)]
    pub(super) fn from_parts_for_test(
        observer: PublishedOwner<DiagnosticObserver>,
        handle: JoinHandle<()>,
    ) -> Self {
        Self::issued(observer, handle)
    }
}

impl RunningDiagnosticWorker {
    fn thread_id(&self) -> ThreadId {
        self.handle.thread().id()
    }

    fn join(self) -> JoinedDiagnosticWorker {
        let Self { handle, observer } = self;
        // Only a returned result certifies thread exit. On unwind, abort
        // before releasing the observer that the worker could still access.
        let require_join = DropGuard::new((), |()| fail_stop());
        if std::thread::current().id() == handle.thread().id() {
            fail_stop();
        }
        #[cfg(test)]
        FAIL_JOIN.with(|fault| assert!(!fault.replace(false), "injected diagnostic join unwind"));
        let result = handle.join();
        DropGuard::dismiss(require_join);
        let panicked = contain_panic(result).is_err();
        JoinedDiagnosticWorker { observer, panicked }
    }
}

impl JoinedDiagnosticWorker {
    pub(super) fn observer(&self) -> &DiagnosticObserver {
        &self.observer
    }

    pub(super) fn panicked(&self) -> bool {
        self.panicked
    }
}

impl Rollback for RunningDiagnosticWorker {
    fn rollback(self) {
        let joined = self.join();
        if joined.panicked() {
            let _ = catch_no_unwind(AssertUnwindSafe(|| {
                tracing::error!("diagnostic logger worker panicked during drop");
            }));
        }
    }
}

#[cfg(test)]
std::thread_local! {
    static FAIL_SPAWN: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static FAIL_JOIN: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
mod tests {
    use super::*;

    struct NoopSink;

    impl DiagnosticSink for NoopSink {
        fn report(&self, _: &crate::diagnostics::DiagnosticEvent<'_>) {}
    }

    #[test]
    #[cfg_attr(miri, ignore = "requires a child process to observe fail-stop")]
    fn diagnostic_worker_aborts_on_unknown_spawn_or_join_result() {
        const CHILD: &str = "XLFN_DIAGNOSTIC_WORKER_FAIL_STOP";
        if let Ok(operation) = std::env::var(CHILD) {
            FAIL_SPAWN.with(|fault| fault.set(operation == "spawn"));
            let (sender, worker) =
                DiagnosticWorker::spawn(NoopSink, "xlfn-diagnostic-fault").unwrap();
            FAIL_JOIN.with(|fault| fault.set(operation == "join"));
            // Keep input open while joining so the injected failure happens
            // while the worker may still be accessing its observer.
            let _ = worker.join();
            drop(sender);
            return;
        }

        for operation in ["spawn", "join"] {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "diagnostics::worker::lifetime::tests::diagnostic_worker_aborts_on_unknown_spawn_or_join_result",
                ])
                .env(CHILD, operation)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::process::ExitStatusExt;
                assert_eq!(status.signal(), Some(6), "expected SIGABRT on {operation}");
            }
            #[cfg(not(unix))]
            {
                assert!(!status.success());
                assert_ne!(status.code(), Some(101), "unwound during {operation}");
            }
        }
    }
}

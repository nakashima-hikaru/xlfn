//! The worker and its pointer target are issued and retired as one owner.

use super::{DiagnosticObserver, DiagnosticObserverPtr, QueuedDiagnosticEvent};
use crate::diagnostics::{DiagnosticInitError, DiagnosticSink};
use crate::panic_boundary::{catch_no_unwind, contain_panic};
use std::io;
use std::mem::ManuallyDrop;
use std::panic::AssertUnwindSafe;
use std::ptr::NonNull;
use std::sync::mpsc::{self, SyncSender};
use std::thread::{JoinHandle, ThreadId};
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
        let pointer = DiagnosticObserverPtr(NonNull::from(&*observer));
        let handle = std::thread::Builder::new()
            .name(name.to_owned())
            .spawn(move || {
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
            })
            .map_err(DiagnosticInitError::WorkerSpawn)?;
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
        // Retain the pointer target if the join operation itself unwinds. A
        // returned result, including a worker panic, certifies thread exit.
        let observer = ManuallyDrop::new(observer);
        if std::thread::current().id() == handle.thread().id() {
            xlfn_kernel::invariant::fail_stop();
        }
        let panicked = contain_panic(handle.join()).is_err();
        JoinedDiagnosticWorker {
            observer: ManuallyDrop::into_inner(observer),
            panicked,
        }
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

//! The worker and its pointer target are issued and retired as one owner.

use super::{DiagnosticObserver, DiagnosticObserverPtr, QueuedDiagnosticEvent};
use crate::diagnostics::{DiagnosticInitError, DiagnosticSink};
use crate::panic_boundary::{catch_no_unwind, contain_panic};
use std::io;
use std::panic::AssertUnwindSafe;
use std::ptr::NonNull;
use std::sync::mpsc::{self, SyncSender};
use std::thread::JoinHandle;
use xlfn_kernel::published_owner::PublishedOwner;

pub(super) struct DiagnosticWorker {
    handle: Option<JoinHandle<()>>,
    thread_id: std::thread::ThreadId,
    observer: PublishedOwner<DiagnosticObserver>,
}

/// Joining is the only operation that issues access to a completed worker.
pub(super) struct JoinedDiagnosticWorker {
    owner: DiagnosticWorker,
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
            thread_id: handle.thread().id(),
            handle: Some(handle),
            observer,
        }
    }

    pub(super) fn observer(&self) -> &DiagnosticObserver {
        &self.observer
    }

    pub(super) fn thread_id(&self) -> std::thread::ThreadId {
        self.thread_id
    }

    pub(super) fn is_current_thread(&self) -> bool {
        std::thread::current().id() == self.thread_id
    }

    pub(super) fn join(mut self) -> JoinedDiagnosticWorker {
        self.reject_self_join();
        let panicked = self.join_handle();
        JoinedDiagnosticWorker {
            owner: self,
            panicked,
        }
    }

    fn reject_self_join(&self) {
        if self.is_current_thread() {
            xlfn_kernel::invariant::fail_stop();
        }
    }

    fn join_handle(&mut self) -> bool {
        self.handle
            .take()
            .is_some_and(|handle| contain_panic(handle.join()).is_err())
    }

    #[cfg(test)]
    pub(super) fn from_parts_for_test(
        observer: PublishedOwner<DiagnosticObserver>,
        handle: JoinHandle<()>,
    ) -> Self {
        Self::issued(observer, handle)
    }

    #[cfg(test)]
    pub(super) fn join_for_test(&mut self) -> bool {
        self.reject_self_join();
        self.join_handle()
    }
}

impl JoinedDiagnosticWorker {
    pub(super) fn observer(&self) -> &DiagnosticObserver {
        self.owner.observer()
    }

    pub(super) fn panicked(&self) -> bool {
        self.panicked
    }
}

impl Drop for DiagnosticWorker {
    fn drop(&mut self) {
        self.reject_self_join();
        if self.join_handle() {
            let _ = catch_no_unwind(AssertUnwindSafe(|| {
                tracing::error!("diagnostic logger worker panicked during drop");
            }));
        }
        // Ordinary field destruction may now release the observer. Before
        // join, forgetting this entire owner retains both worker and target.
    }
}

//! Bounded diagnostic worker and owned event handoff.

#![allow(
    unsafe_code,
    reason = "Diagnostic worker uses audited non-owning pointer joined before observer drop"
)]

use super::{DiagnosticEvent, DiagnosticInitError, DiagnosticShutdownError, DiagnosticSink};
use crate::diagnostics::event::DROPPED_EVENTS;
use crate::panic_boundary::{catch_no_unwind, contain_panic};
use std::io;
use std::panic::AssertUnwindSafe;
use std::ptr::NonNull;
#[cfg(any(test, feature = "refinement"))]
use std::sync::atomic::AtomicU64;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, SyncSender};
use std::thread::JoinHandle;
use xlfn_kernel::published_owner::PublishedOwner;

pub(crate) struct OwnedDiagnosticEvent {
    pub(crate) udf_id: &'static str,
    pub(crate) argument: Option<&'static str>,
    pub(crate) error: crate::XllError,
    pub(crate) diagnostic_id: crate::diagnostics::id::DiagnosticId,
    pub(crate) timestamp: std::time::SystemTime,
}

impl OwnedDiagnosticEvent {
    fn deliver<S: DiagnosticSink>(self, sink: &S) {
        let event = DiagnosticEvent {
            udf_id: self.udf_id,
            argument: self.argument,
            error: &self.error,
            diagnostic_id: self.diagnostic_id,
            timestamp: self.timestamp,
        };
        super::deliver_no_unwind(sink, &event);
    }
}

pub(crate) struct AsyncDiagnosticSink {
    pub(crate) sender: Option<SyncSender<OwnedDiagnosticEvent>>,
    pub(crate) worker: Option<JoinHandle<()>>,
    pub(crate) worker_thread_id: std::thread::ThreadId,
    pub(crate) observer: PublishedOwner<DiagnosticObserver>,
}

#[derive(Clone, Copy)]
pub(crate) struct DiagnosticObserverPtr(NonNull<DiagnosticObserver>);

// SAFETY: DiagnosticObserver is Send and Sync, and its address is stable until worker joins.
unsafe impl Send for DiagnosticObserverPtr {}
// SAFETY: DiagnosticObserver is Send and Sync, and its address is stable until worker joins.
unsafe impl Sync for DiagnosticObserverPtr {}

pub(crate) struct DiagnosticObserver {
    // Reservations include builders and queued events, but not delivery.
    // The channel publishes payloads; this counter only enforces capacity.
    reserved: AtomicUsize,
    #[cfg(any(test, feature = "refinement"))]
    pending: AtomicU64,
    sink: crate::shutdown_trace::ObservationSink,
}

impl DiagnosticObserver {
    pub(crate) fn new() -> Box<Self> {
        Box::new(Self {
            reserved: AtomicUsize::new(0),
            #[cfg(any(test, feature = "refinement"))]
            pending: AtomicU64::new(0),
            sink: crate::shutdown_trace::ObservationSink::new(),
        })
    }

    pub(crate) fn set_trace_sink(&self, trace: crate::shutdown_trace::ShutdownTraceHandle) {
        self.sink.set_trace_sink(trace);
    }

    pub(crate) fn trace_handle(&self) -> Option<crate::shutdown_trace::ShutdownTraceHandle> {
        self.sink.trace_handle()
    }

    pub(crate) fn record(&self, event: crate::shutdown_trace::ShutdownEvent) {
        self.sink.record(event);
    }

    fn reserve(&self) -> Option<DiagnosticReservation<'_>> {
        self.reserved
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |reserved| {
                (reserved < super::DIAGNOSTIC_QUEUE_CAPACITY).then_some(reserved + 1)
            })
            .ok()
            .map(|_| DiagnosticReservation {
                observer: self,
                committed: false,
            })
    }

    fn release_reservation(&self) {
        if self.reserved.fetch_sub(1, Ordering::Relaxed) == 0 {
            xlfn_kernel::invariant::fail_stop();
        }
    }

    fn increment_pending(&self) {
        #[cfg(any(test, feature = "refinement"))]
        self.pending.fetch_add(1, Ordering::AcqRel);
    }

    fn decrement_pending(&self) {
        #[cfg(any(test, feature = "refinement"))]
        let _ = xlfn_kernel::invariant::checked_atomic_dec_u64(&self.pending);
    }

    fn take_pending(&self) -> u64 {
        #[cfg(any(test, feature = "refinement"))]
        {
            self.pending.swap(0, Ordering::AcqRel)
        }
        #[cfg(not(any(test, feature = "refinement")))]
        0
    }

    #[cfg(any(test, feature = "refinement"))]
    pub(crate) fn pending(&self) -> u64 {
        self.pending.load(Ordering::Acquire)
    }
}

struct DiagnosticReservation<'a> {
    observer: &'a DiagnosticObserver,
    committed: bool,
}

impl DiagnosticReservation<'_> {
    fn commit(mut self) {
        self.committed = true;
    }
}

impl Drop for DiagnosticReservation<'_> {
    fn drop(&mut self) {
        if !self.committed {
            self.observer.release_reservation();
        }
    }
}

impl AsyncDiagnosticSink {
    pub(crate) fn new<S: DiagnosticSink>(sink: S) -> Result<Self, DiagnosticInitError> {
        Self::new_named(sink, "xlfn-diagnostics")
    }

    pub(crate) fn new_named<S: DiagnosticSink>(
        sink: S,
        worker_name: &str,
    ) -> Result<Self, DiagnosticInitError> {
        if worker_name.as_bytes().contains(&0) {
            return Err(DiagnosticInitError::WorkerSpawn(io::Error::new(
                io::ErrorKind::InvalidInput,
                "diagnostic worker name contains NUL",
            )));
        }
        let (sender, receiver) =
            mpsc::sync_channel::<OwnedDiagnosticEvent>(super::DIAGNOSTIC_QUEUE_CAPACITY);
        let observer = PublishedOwner::from_box(DiagnosticObserver::new());
        let observer_ptr = DiagnosticObserverPtr(NonNull::from(&*observer));
        let worker = std::thread::Builder::new()
            .name(worker_name.to_owned())
            .spawn(move || {
                let worker_observer = observer_ptr;
                while let Ok(event) = receiver.recv() {
                    // SAFETY: the observer lives until the worker is joined.
                    let observer_ref = unsafe { worker_observer.0.as_ref() };
                    observer_ref.release_reservation();
                    event.deliver(&sink);
                    crate::ingress::with_diagnostic_linearization(|| {
                        // SAFETY: the observer lives until the worker thread joins in shutdown or drop
                        let observer_ref = unsafe { worker_observer.0.as_ref() };
                        observer_ref.record(crate::shutdown_trace::ShutdownEvent::FlushDiagnostic);
                        observer_ref.decrement_pending();
                    });
                }
            })
            .map_err(DiagnosticInitError::WorkerSpawn)?;
        let worker_thread_id = worker.thread().id();
        Ok(Self {
            sender: Some(sender),
            worker: Some(worker),
            worker_thread_id,
            observer,
        })
    }

    pub(crate) fn set_trace_sink(&self, trace: crate::shutdown_trace::ShutdownTraceHandle) {
        self.observer.set_trace_sink(trace);
    }

    #[cfg(any(test, feature = "refinement"))]
    pub(crate) fn pending(&self) -> u64 {
        self.observer.pending()
    }

    pub(crate) fn is_current_thread_worker(&self) -> bool {
        std::thread::current().id() == self.worker_thread_id
    }

    pub(crate) fn report(&self, build: impl FnOnce() -> OwnedDiagnosticEvent) {
        let Some(sender) = self.sender.as_ref() else {
            DROPPED_EVENTS.fetch_add(1, Ordering::Relaxed);
            return;
        };
        let Some(reservation) = self.observer.reserve() else {
            DROPPED_EVENTS.fetch_add(1, Ordering::Relaxed);
            return;
        };
        // Expensive error cloning and timestamps happen only after admission,
        // outside the channel and trace locks. Panic returns the reservation.
        let event = build();
        let result = crate::ingress::with_diagnostic_linearization(|| {
            self.observer.increment_pending();
            let result = sender.try_send(event);
            if result.is_err() {
                self.observer.decrement_pending();
            }
            if result.is_ok() {
                reservation.commit();
                self.observer
                    .record(crate::shutdown_trace::ShutdownEvent::EnqueueDiagnostic);
            }
            result
        });
        if result.is_err() {
            DROPPED_EVENTS.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[allow(
        clippy::boxed_local,
        reason = "Explicit Box<Self> represents unique retired ownership transferred from service slot"
    )]
    pub(crate) fn shutdown(mut self: Box<Self>) -> Result<(), DiagnosticShutdownError> {
        if self.is_current_thread_worker() {
            return Err(DiagnosticShutdownError::ReentrantShutdown);
        }
        crate::ingress::with_diagnostic_linearization(|| {
            self.sender.take();
        });
        let worker = self.worker.take();
        if let Some(worker) = worker {
            let result = contain_panic(worker.join());
            if result.is_err() {
                let discarded = self.observer.take_pending();
                if discarded != 0 {
                    crate::ingress::with_diagnostic_linearization(|| {
                        for _ in 0..discarded {
                            self.observer
                                .record(crate::shutdown_trace::ShutdownEvent::DiscardDiagnostic);
                        }
                    });
                }
                return Err(DiagnosticShutdownError::WorkerPanicked);
            }
        }
        Ok(())
    }
}

impl Drop for AsyncDiagnosticSink {
    fn drop(&mut self) {
        if self.is_current_thread_worker() {
            xlfn_kernel::invariant::fail_stop();
        }
        self.sender.take();
        if let Some(worker) = self.worker.take()
            && contain_panic(worker.join()).is_err()
        {
            let _ = catch_no_unwind(AssertUnwindSafe(|| {
                tracing::error!("diagnostic logger worker panicked during drop");
            }));
        }
    }
}

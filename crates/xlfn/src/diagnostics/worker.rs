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

pub(crate) struct DiagnosticEventMetadata {
    pub(crate) udf_id: &'static str,
    pub(crate) argument: Option<&'static str>,
    pub(crate) diagnostic_id: crate::diagnostics::id::DiagnosticId,
    pub(crate) timestamp: std::time::SystemTime,
}

struct OwnedDiagnosticEvent {
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
    pub(crate) sender: Option<SyncSender<QueuedDiagnosticEvent>>,
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
    // Payload credits additionally cover the currently delivered event, until
    // its owned error is destroyed. The channel publishes both payload and credit.
    reserved: AtomicUsize,
    payload_bytes: AtomicUsize,
    #[cfg(any(test, feature = "refinement"))]
    pending: AtomicU64,
    sink: crate::shutdown_trace::ObservationSink,
}

impl DiagnosticObserver {
    pub(crate) fn new() -> Box<Self> {
        Box::new(Self {
            reserved: AtomicUsize::new(0),
            payload_bytes: AtomicUsize::new(0),
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

    fn reserve(&self, error: &crate::XllError) -> Option<DiagnosticReservation<'_>> {
        self.reserved
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |reserved| {
                (reserved < super::DIAGNOSTIC_QUEUE_CAPACITY).then_some(reserved + 1)
            })
            .ok()?;
        let mut reservation = DiagnosticReservation {
            observer: self,
            payload_bytes: 0,
            committed: false,
        };
        let bytes = super::payload::clone_bytes(error)?;
        if bytes != 0 {
            self.payload_bytes
                .try_update(Ordering::Relaxed, Ordering::Relaxed, |used| {
                    used.checked_add(bytes)
                        .filter(|&next| next <= super::DIAGNOSTIC_PAYLOAD_MAX_BYTES)
                })
                .ok()?;
            reservation.payload_bytes = bytes;
        }
        Some(reservation)
    }

    fn release_slot(&self) {
        if self.reserved.fetch_sub(1, Ordering::Relaxed) == 0 {
            xlfn_kernel::invariant::fail_stop();
        }
    }

    fn release_payload(&self, bytes: usize) {
        if bytes != 0 && self.payload_bytes.fetch_sub(bytes, Ordering::Relaxed) < bytes {
            xlfn_kernel::invariant::fail_stop();
        }
    }

    #[cfg(test)]
    pub(crate) fn reserved_payload_bytes(&self) -> usize {
        self.payload_bytes.load(Ordering::Relaxed)
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
    payload_bytes: usize,
    committed: bool,
}

impl DiagnosticReservation<'_> {
    fn commit(mut self, event: OwnedDiagnosticEvent) -> QueuedDiagnosticEvent {
        self.committed = true;
        QueuedDiagnosticEvent {
            event,
            credit: DiagnosticCredit {
                observer: DiagnosticObserverPtr(NonNull::from(self.observer)),
                payload_bytes: self.payload_bytes,
                queued: true,
            },
        }
    }
}

impl Drop for DiagnosticReservation<'_> {
    fn drop(&mut self) {
        if !self.committed {
            self.observer.release_payload(self.payload_bytes);
            self.observer.release_slot();
        }
    }
}

// Field order destroys the owned payload before returning its byte credit,
// including channel discard, send failure and unwinding before delivery.
pub(crate) struct QueuedDiagnosticEvent {
    event: OwnedDiagnosticEvent,
    credit: DiagnosticCredit,
}

struct DiagnosticCredit {
    observer: DiagnosticObserverPtr,
    payload_bytes: usize,
    queued: bool,
}

impl DiagnosticCredit {
    fn dequeue(&mut self) {
        // SAFETY: a credit is private to its sender or worker's channel. The
        // AsyncDiagnosticSink retains the observer through every worker join.
        let observer = unsafe { self.observer.0.as_ref() };
        observer.release_slot();
        self.queued = false;
    }
}

impl Drop for DiagnosticCredit {
    fn drop(&mut self) {
        // SAFETY: unused sends retain the sink borrow; queued credits are
        // destroyed by the joined receiver before the observer owner is dropped.
        let observer = unsafe { self.observer.0.as_ref() };
        observer.release_payload(self.payload_bytes);
        if self.queued {
            observer.release_slot();
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
            mpsc::sync_channel::<QueuedDiagnosticEvent>(super::DIAGNOSTIC_QUEUE_CAPACITY);
        let observer = PublishedOwner::from_box(DiagnosticObserver::new());
        let observer_ptr = DiagnosticObserverPtr(NonNull::from(&*observer));
        let worker = std::thread::Builder::new()
            .name(worker_name.to_owned())
            .spawn(move || {
                let worker_observer = observer_ptr;
                while let Ok(mut queued) = receiver.recv() {
                    queued.credit.dequeue();
                    queued.event.deliver(&sink);
                    drop(queued.credit);
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

    pub(crate) fn report(
        &self,
        error: &crate::XllError,
        build: impl FnOnce() -> DiagnosticEventMetadata,
    ) {
        let Some(sender) = self.sender.as_ref() else {
            DROPPED_EVENTS.fetch_add(1, Ordering::Relaxed);
            return;
        };
        let Some(reservation) = self.observer.reserve(error) else {
            DROPPED_EVENTS.fetch_add(1, Ordering::Relaxed);
            return;
        };
        // Expensive error cloning and timestamps happen only after admission,
        // outside the channel and trace locks. Panic returns the reservation.
        let metadata = build();
        let event = OwnedDiagnosticEvent {
            udf_id: metadata.udf_id,
            argument: metadata.argument,
            error: error.clone(),
            diagnostic_id: metadata.diagnostic_id,
            timestamp: metadata.timestamp,
        };
        let queued = reservation.commit(event);
        let result = crate::ingress::with_diagnostic_linearization(|| {
            self.observer.increment_pending();
            let result = sender.try_send(queued);
            if result.is_err() {
                self.observer.decrement_pending();
            }
            if result.is_ok() {
                self.observer
                    .record(crate::shutdown_trace::ShutdownEvent::EnqueueDiagnostic);
            }
            result.err()
        });
        if result.is_some() {
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

#[cfg(test)]
mod budget_tests {
    use super::*;
    use std::sync::Arc;
    use std::time::SystemTime;

    fn metadata() -> DiagnosticEventMetadata {
        DiagnosticEventMetadata {
            udf_id: "byte-budget",
            argument: None,
            diagnostic_id: crate::diagnostics::id::DiagnosticId::from_u64(1),
            timestamp: SystemTime::UNIX_EPOCH,
        }
    }

    fn event(error: crate::XllError) -> OwnedDiagnosticEvent {
        let metadata = metadata();
        OwnedDiagnosticEvent {
            udf_id: metadata.udf_id,
            argument: metadata.argument,
            error,
            diagnostic_id: metadata.diagnostic_id,
            timestamp: metadata.timestamp,
        }
    }

    #[test]
    fn miri_payload_credit_returns_on_failed_send_and_receiver_discard() {
        let observer = DiagnosticObserver::new();
        let error = crate::XllError::Native {
            code: 1,
            message: "payload".to_owned(),
        };
        let (sender, receiver) = mpsc::sync_channel(1);
        let first = observer
            .reserve(&error)
            .unwrap()
            .commit(event(error.clone()));
        sender
            .try_send(first)
            .unwrap_or_else(|_| panic!("first send failed"));
        assert_eq!(observer.reserved_payload_bytes(), 7);
        let second = observer
            .reserve(&error)
            .unwrap()
            .commit(event(error.clone()));
        let unused = sender.try_send(second).err().unwrap();
        assert_eq!(observer.reserved_payload_bytes(), 14);
        drop(unused);
        assert_eq!(observer.reserved_payload_bytes(), 7);
        drop(receiver);
        assert_eq!(observer.reserved_payload_bytes(), 0);
        assert_eq!(observer.reserved.load(Ordering::Relaxed), 0);
        let third = observer.reserve(&error).unwrap().commit(event(error));
        drop(sender.try_send(third));
        assert_eq!(observer.reserved_payload_bytes(), 0);
        assert_eq!(observer.reserved.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn byte_credit_rolls_back_on_metadata_panic_and_overload() {
        struct Noop;
        impl DiagnosticSink for Noop {
            fn report(&self, _: &DiagnosticEvent<'_>) {}
        }
        let sink = Box::new(AsyncDiagnosticSink::new(Noop).unwrap());
        let error = crate::XllError::Native {
            code: 1,
            message: "payload".to_owned(),
        };
        assert!(
            crate::panic_boundary::catch_no_unwind(AssertUnwindSafe(|| {
                sink.report(&error, || panic!("metadata construction failed"));
            }))
            .is_err()
        );
        assert_eq!(sink.observer.reserved_payload_bytes(), 0);
        assert_eq!(sink.observer.reserved.load(Ordering::Relaxed), 0);
        sink.observer.payload_bytes.store(
            super::super::DIAGNOSTIC_PAYLOAD_MAX_BYTES,
            Ordering::Relaxed,
        );
        sink.report(&error, || panic!("over-budget event must not be built"));
        assert_eq!(sink.observer.reserved.load(Ordering::Relaxed), 0);
        sink.observer.payload_bytes.store(0, Ordering::Relaxed);
        let oversized = crate::XllError::Native {
            code: 1,
            message: "x".repeat(super::super::DIAGNOSTIC_PAYLOAD_MAX_BYTES + 1),
        };
        sink.report(&oversized, || {
            panic!("an oversize error must not be cloned")
        });
        assert_eq!(sink.observer.reserved_payload_bytes(), 0);
        assert_eq!(sink.observer.reserved.load(Ordering::Relaxed), 0);
        sink.shutdown().unwrap();
    }

    #[test]
    fn slow_delivery_holds_payload_credit_and_preserves_full_error() {
        struct SlowSink {
            reports: Arc<AtomicUsize>,
            started: mpsc::SyncSender<()>,
            release: crate::sync::Mutex<mpsc::Receiver<()>>,
            drained: mpsc::SyncSender<()>,
        }
        impl DiagnosticSink for SlowSink {
            fn report(&self, event: &DiagnosticEvent<'_>) {
                if let crate::XllError::Native { message, .. } = event.error() {
                    assert_eq!(message.len(), 1024 * 1024);
                    assert!(message.bytes().all(|byte| byte == b'x'));
                    if self.reports.fetch_add(1, Ordering::Relaxed) == 0 {
                        self.started.send(()).unwrap();
                        self.release.lock().recv().unwrap();
                    }
                } else {
                    self.drained.send(()).unwrap();
                }
            }
        }
        let (started_tx, started_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        let (drained_tx, drained_rx) = mpsc::sync_channel(1);
        let reports = Arc::new(AtomicUsize::new(0));
        let sink = Box::new(
            AsyncDiagnosticSink::new(SlowSink {
                reports: Arc::clone(&reports),
                started: started_tx,
                release: crate::sync::Mutex::new(release_rx),
                drained: drained_tx,
            })
            .unwrap(),
        );
        let error = crate::XllError::Native {
            code: 1,
            message: "x".repeat(1024 * 1024),
        };
        sink.report(&error, metadata);
        started_rx.recv().unwrap();
        assert_eq!(sink.observer.reserved_payload_bytes(), 1024 * 1024);
        for _ in 1..16 {
            sink.report(&error, metadata);
        }
        assert_eq!(
            sink.observer.reserved_payload_bytes(),
            super::super::DIAGNOSTIC_PAYLOAD_MAX_BYTES
        );
        assert_eq!(sink.observer.reserved.load(Ordering::Relaxed), 15);
        sink.report(&error, || {
            panic!("processing bytes must count toward the limit")
        });
        // Zero-payload errors can still be admitted when the byte quota is full.
        sink.report(&crate::XllError::Panic, metadata);
        release_tx.send(()).unwrap();
        drained_rx.recv().unwrap();
        assert_eq!(sink.observer.reserved_payload_bytes(), 0);
        assert_eq!(reports.load(Ordering::Relaxed), 16);
        sink.shutdown().unwrap();
    }

    #[test]
    fn miri_worker_panic_discards_all_payload_credits_before_owner_drop() {
        let observer = PublishedOwner::from_box(DiagnosticObserver::new());
        let (sender, receiver) = mpsc::sync_channel(4);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        let worker = std::thread::spawn(move || {
            release_rx.recv().unwrap();
            let queued: QueuedDiagnosticEvent = receiver.recv().unwrap();
            std::hint::black_box(&queued);
            panic!("injected worker failure with queued payloads");
        });
        let worker_thread_id = worker.thread().id();
        let mut sink = Box::new(AsyncDiagnosticSink {
            sender: Some(sender),
            worker: Some(worker),
            worker_thread_id,
            observer,
        });
        let error = crate::XllError::Native {
            code: 1,
            message: "owned".to_owned(),
        };
        for _ in 0..3 {
            sink.report(&error, metadata);
        }
        assert_eq!(sink.observer.reserved_payload_bytes(), 15);
        release_tx.send(()).unwrap();
        assert!(sink.worker.take().unwrap().join().is_err());
        assert_eq!(sink.observer.reserved_payload_bytes(), 0);
        assert_eq!(sink.observer.reserved.load(Ordering::Relaxed), 0);
        sink.shutdown().unwrap();
    }
}

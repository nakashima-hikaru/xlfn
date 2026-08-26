//! Bounded diagnostic worker and owned event handoff.

use super::{DiagnosticEvent, DiagnosticInitError, DiagnosticShutdownError, DiagnosticSink};
use crate::diagnostics::event::DROPPED_EVENTS;
use parking_lot::Mutex;
use std::io;
use std::panic::{AssertUnwindSafe, catch_unwind};
#[cfg(any(test, feature = "refinement"))]
use std::sync::Arc;
#[cfg(any(test, feature = "refinement"))]
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::thread::JoinHandle;

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
    pub(crate) sender: Mutex<Option<SyncSender<OwnedDiagnosticEvent>>>,
    pub(crate) worker: Mutex<Option<JoinHandle<()>>>,
    pub(crate) worker_thread_id: std::thread::ThreadId,
    #[cfg(any(test, feature = "refinement"))]
    pub(crate) pending: Arc<AtomicU64>,
    #[cfg(any(test, feature = "refinement"))]
    pub(crate) trace: Arc<Mutex<Option<crate::shutdown_trace::ShutdownTraceHandle>>>,
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
        #[cfg(any(test, feature = "refinement"))]
        let pending = Arc::new(AtomicU64::new(0));
        #[cfg(any(test, feature = "refinement"))]
        let worker_pending = Arc::clone(&pending);
        #[cfg(any(test, feature = "refinement"))]
        let trace = Arc::new(Mutex::new(
            None::<crate::shutdown_trace::ShutdownTraceHandle>,
        ));
        #[cfg(any(test, feature = "refinement"))]
        let worker_trace = Arc::clone(&trace);
        let worker = std::thread::Builder::new()
            .name(worker_name.to_owned())
            .spawn(move || {
                while let Ok(event) = receiver.recv() {
                    event.deliver(&sink);
                    crate::ingress::with_diagnostic_linearization(|| {
                        #[cfg(any(test, feature = "refinement"))]
                        if let Some(trace) = worker_trace.lock().as_ref().cloned() {
                            trace.record(crate::shutdown_trace::ShutdownEvent::FlushDiagnostic);
                        }
                        #[cfg(any(test, feature = "refinement"))]
                        let _ = xlfn_kernel::invariant::checked_atomic_dec_u64(&worker_pending);
                    });
                }
            })
            .map_err(DiagnosticInitError::WorkerSpawn)?;
        let worker_thread_id = worker.thread().id();
        Ok(Self {
            sender: Mutex::new(Some(sender)),
            worker: Mutex::new(Some(worker)),
            worker_thread_id,
            #[cfg(any(test, feature = "refinement"))]
            pending,
            #[cfg(any(test, feature = "refinement"))]
            trace,
        })
    }

    #[cfg(any(test, feature = "refinement"))]
    pub(crate) fn set_trace_sink(&self, trace: crate::shutdown_trace::ShutdownTraceHandle) {
        *self.trace.lock() = Some(trace);
    }

    #[cfg(any(test, feature = "refinement"))]
    pub(crate) fn pending(&self) -> u64 {
        self.pending.load(Ordering::Acquire)
    }

    pub(crate) fn is_current_thread_worker(&self) -> bool {
        std::thread::current().id() == self.worker_thread_id
    }

    pub(crate) fn report(&self, event: OwnedDiagnosticEvent) {
        let result = crate::ingress::with_diagnostic_linearization(|| {
            let sender = self.sender.lock();
            #[cfg(any(test, feature = "refinement"))]
            self.pending.fetch_add(1, Ordering::AcqRel);
            let result = match sender.as_ref() {
                Some(sender) => sender.try_send(event),
                None => Err(TrySendError::Disconnected(event)),
            };
            #[cfg(any(test, feature = "refinement"))]
            if result.is_err() {
                let _ = xlfn_kernel::invariant::checked_atomic_dec_u64(&self.pending);
            }
            drop(sender);
            #[cfg(any(test, feature = "refinement"))]
            if result.is_ok()
                && let Some(trace) = self.trace.lock().as_ref().cloned()
            {
                trace.record(crate::shutdown_trace::ShutdownEvent::EnqueueDiagnostic);
            }
            result
        });
        if result.is_err() {
            DROPPED_EVENTS.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub(crate) fn shutdown(&self) -> Result<(), DiagnosticShutdownError> {
        if self.is_current_thread_worker() {
            return Err(DiagnosticShutdownError::ReentrantShutdown);
        }
        crate::ingress::with_diagnostic_linearization(|| {
            self.sender.lock().take();
        });
        let worker = self.worker.lock().take();
        if let Some(worker) = worker {
            let result = worker.join();
            if result.is_err() {
                #[cfg(any(test, feature = "refinement"))]
                {
                    let discarded = self.pending.swap(0, Ordering::AcqRel);
                    if discarded != 0
                        && let Some(trace) = self.trace.lock().as_ref().cloned()
                    {
                        crate::ingress::with_diagnostic_linearization(|| {
                            for _ in 0..discarded {
                                trace.record(
                                    crate::shutdown_trace::ShutdownEvent::DiscardDiagnostic,
                                );
                            }
                        });
                    }
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
            return;
        }
        self.sender.get_mut().take();
        if let Some(worker) = self.worker.get_mut().take()
            && worker.join().is_err()
        {
            let _ = catch_unwind(AssertUnwindSafe(|| {
                tracing::error!("diagnostic logger worker panicked during drop");
            }));
        }
    }
}

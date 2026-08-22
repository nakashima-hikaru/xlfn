use crate::error::{DiagnosticId, IntoXllError};
use crate::{XllError, XllResult};
use arc_swap::ArcSwapOption;
use parking_lot::Mutex;
#[cfg(test)]
use std::fs;
#[cfg(test)]
use std::io;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::SystemTime;

/// Public diagnostic events, sink configuration, and observable statistics.
pub mod event;
/// Private file-sink and startup-log integration.
pub(crate) mod file;
/// Private lifecycle router and worker operations.
pub(crate) mod router;
/// Private bounded worker and ownership handoff.
pub(crate) mod worker;

#[cfg(target_os = "windows")]
pub(crate) use file::append_startup_log;
pub(crate) use file::install_file_diagnostic_sink;
#[cfg(test)]
pub(crate) use file::{FileDiagnosticSink, RotatingLog, install_file_diagnostic_sink_at};

#[cfg(test)]
pub(crate) use event::DiagnosticsDrained;
pub use event::{
    AddinId, DiagnosticEvent, DiagnosticInitError, DiagnosticShutdownError, DiagnosticSink,
    DiagnosticStats, InvalidAddinId, diagnostic_stats,
};
use worker::{AsyncDiagnosticSink, OwnedDiagnosticEvent};

const DIAGNOSTIC_QUEUE_CAPACITY: usize = 1024;
const LOG_MAX_BYTES: u64 = 4 * 1024 * 1024;
const LOG_GENERATIONS: usize = 3;
const DIAGNOSTIC_TEXT_MAX_BYTES: usize = 16 * 1024;
const DIAGNOSTIC_TRUNCATION_SUFFIX: &str = "…[truncated]";

#[derive(Debug)]
pub(crate) struct DiagnosticsStopped {
    _private: (),
}

impl DiagnosticsStopped {
    fn new() -> Self {
        Self { _private: () }
    }

    #[cfg(test)]
    pub(crate) fn for_test() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DiagnosticPhase {
    Open,
    Closing,
    Closed,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum DiagnosticTerminalCloseError {
    #[error("diagnostic logger worker panicked during terminal close")]
    WorkerPanicked,
    #[error("diagnostic logger cannot join itself")]
    ReentrantShutdown,
    #[error("diagnostic router invariant violated during terminal close")]
    InvariantViolation,
}

impl IntoXllError for DiagnosticTerminalCloseError {
    fn into_xll_error(self) -> XllError {
        match self {
            Self::WorkerPanicked => XllError::Panic,
            Self::ReentrantShutdown => XllError::ReentrantCall,
            Self::InvariantViolation => XllError::Internal {
                diagnostic_id: DiagnosticId::DIAGNOSTICS_CLOSE,
            },
        }
    }
}

struct DiagnosticRouter {
    sink: ArcSwapOption<AsyncDiagnosticSink>,
    transition: Mutex<DiagnosticPhase>,
    retiring_workers: Mutex<Vec<std::thread::ThreadId>>,
    #[cfg(any(test, feature = "shutdown-refinement"))]
    ghost: Mutex<Option<crate::shutdown_refinement::GhostHandle>>,
}

impl DiagnosticRouter {
    fn caller_is_worker(&self) -> bool {
        let current = std::thread::current().id();
        if self
            .sink
            .load()
            .as_ref()
            .is_some_and(|sink| sink.worker_thread_id == current)
        {
            return true;
        }
        self.retiring_workers.lock().contains(&current)
    }

    fn mark_retiring(&self, sink: &AsyncDiagnosticSink) {
        let mut workers = self.retiring_workers.lock();
        if !workers.contains(&sink.worker_thread_id) {
            workers.push(sink.worker_thread_id);
        }
    }

    fn unmark_retiring(&self, sink: &AsyncDiagnosticSink) {
        self.retiring_workers
            .lock()
            .retain(|worker| *worker != sink.worker_thread_id);
    }

    #[cfg(any(test, feature = "shutdown-refinement"))]
    fn set_ghost(&self, ghost: crate::shutdown_refinement::GhostHandle) {
        *self.ghost.lock() = Some(Arc::clone(&ghost));
        if let Some(sink) = self.sink.load().as_ref() {
            sink.set_ghost(ghost);
        }
    }

    #[cfg(any(test, feature = "shutdown-refinement"))]
    fn ghost_handle(&self) -> Option<crate::shutdown_refinement::GhostHandle> {
        self.ghost.lock().clone()
    }

    #[cfg(test)]
    fn phase(&self) -> DiagnosticPhase {
        *self.transition.lock()
    }

    fn replace_with<F, H>(&self, make_sink: F, on_published: H) -> Result<(), DiagnosticInitError>
    where
        F: FnOnce() -> Result<Arc<AsyncDiagnosticSink>, DiagnosticInitError>,
        H: FnOnce(bool) -> Result<(), DiagnosticInitError>,
    {
        if self.caller_is_worker() {
            return Err(DiagnosticInitError::ReentrantMutation);
        }
        let phase = self.transition.lock();
        // A transition may have retired the current worker while this caller
        // was waiting for the lock, so worker re-entry must be checked again.
        if self.caller_is_worker() {
            return Err(DiagnosticInitError::ReentrantMutation);
        }
        if *phase != DiagnosticPhase::Open {
            return Err(DiagnosticInitError::RouterClosed);
        }
        // Keep construction in the same linearization region as terminal
        // close. A close that wins the transition lock therefore cannot race a
        // worker that has been created but not yet published.
        let sink = make_sink()?;
        let previous = crate::ingress::with_diagnostic_linearization(|| {
            let previous = self.sink.swap(Some(Arc::clone(&sink)));
            if let Some(previous) = previous.as_ref() {
                self.mark_retiring(previous);
            }

            if let Err(error) = on_published(previous.is_some()) {
                let restored = self
                    .sink
                    .load()
                    .as_ref()
                    .is_some_and(|current| Arc::ptr_eq(current, &sink));
                if restored {
                    self.sink.store(previous.clone());
                }
                if restored && let Some(previous) = previous.as_ref() {
                    self.unmark_retiring(previous);
                }
                return Err(error);
            }
            Ok(previous)
        });

        let previous = match previous {
            Ok(previous) => previous,
            Err(error) => {
                let _ = sink.shutdown();
                return Err(error);
            }
        };

        let Some(previous) = previous else {
            return Ok(());
        };
        match previous.shutdown() {
            Ok(()) => {
                self.unmark_retiring(&previous);
                Ok(())
            }
            Err(DiagnosticShutdownError::ReentrantShutdown) => {
                self.mark_retiring(&sink);
                self.sink.store(Some(Arc::clone(&previous)));
                self.unmark_retiring(&previous);
                let _ = sink.shutdown();
                self.unmark_retiring(&sink);
                Err(DiagnosticInitError::ReentrantMutation)
            }
            Err(DiagnosticShutdownError::WorkerPanicked) => {
                self.unmark_retiring(&previous);
                #[cfg(any(test, feature = "shutdown-refinement"))]
                if let Some(ghost) = self.ghost_handle() {
                    record_ghost_diagnostics_cleanup_issue(ghost);
                }
                sink.report(OwnedDiagnosticEvent {
                    udf_id: "diagnostic sink replacement",
                    argument: None,
                    error: XllError::Panic,
                    diagnostic_id: DiagnosticId::from_u64(NEXT_ID.fetch_add(1, Ordering::Relaxed)),
                    timestamp: SystemTime::now(),
                });
                Ok(())
            }
            Err(
                DiagnosticShutdownError::RouterClosed | DiagnosticShutdownError::InvariantViolation,
            ) => {
                self.unmark_retiring(&previous);
                Err(DiagnosticInitError::RouterClosed)
            }
        }
    }

    fn current(&self) -> arc_swap::Guard<Option<Arc<AsyncDiagnosticSink>>> {
        self.sink.load()
    }

    #[cfg(any(test, feature = "shutdown-refinement"))]
    fn ghost_snapshot(&self) -> GhostDiagnosticsSnapshot {
        let sink = self.sink.load();
        GhostDiagnosticsSnapshot {
            running: sink.is_some(),
            pending: sink.as_ref().map_or(0, |sink| sink.pending()),
        }
    }

    #[cfg(test)]
    fn record_stop_diagnostics(&self) {
        crate::ingress::with_diagnostic_linearization(|| {
            #[cfg(any(test, feature = "shutdown-refinement"))]
            if let Some(ghost) = self.ghost_handle()
                && ghost.state().resources.diagnostics_running
            {
                ghost.record_event(crate::shutdown_refinement::GhostEvent::StopDiagnostics);
            }
        });
    }

    #[cfg(test)]
    fn drain_reopenable(&self) -> Result<DiagnosticsDrained, DiagnosticShutdownError> {
        if self.caller_is_worker() {
            return Err(DiagnosticShutdownError::ReentrantShutdown);
        }
        let mut phase = self.transition.lock();
        if self.caller_is_worker() {
            return Err(DiagnosticShutdownError::ReentrantShutdown);
        }
        match *phase {
            DiagnosticPhase::Open => {}
            DiagnosticPhase::Closed => return Err(DiagnosticShutdownError::RouterClosed),
            DiagnosticPhase::Closing => {
                return Err(DiagnosticShutdownError::InvariantViolation);
            }
        }
        *phase = DiagnosticPhase::Closing;

        let previous = crate::ingress::with_diagnostic_linearization(|| {
            let previous = self.sink.swap(None);
            if let Some(previous) = previous.as_ref() {
                self.mark_retiring(previous);
            }
            previous
        });

        let result = match previous {
            None => Ok(()),
            Some(previous) => match previous.shutdown() {
                Ok(()) => {
                    self.unmark_retiring(&previous);
                    Ok(())
                }
                Err(DiagnosticShutdownError::ReentrantShutdown) => {
                    crate::ingress::with_diagnostic_linearization(|| {
                        self.sink.store(Some(Arc::clone(&previous)));
                    });
                    self.unmark_retiring(&previous);
                    Err(DiagnosticShutdownError::ReentrantShutdown)
                }
                Err(error @ DiagnosticShutdownError::WorkerPanicked) => {
                    self.unmark_retiring(&previous);
                    Err(error)
                }
                Err(error) => {
                    self.unmark_retiring(&previous);
                    Err(error)
                }
            },
        };
        *phase = DiagnosticPhase::Open;
        if result.is_ok() && self.sink.load().is_none() {
            self.record_stop_diagnostics();
        } else if matches!(result, Err(DiagnosticShutdownError::WorkerPanicked)) {
            #[cfg(any(test, feature = "shutdown-refinement"))]
            if let Some(ghost) = self.ghost_handle() {
                record_ghost_diagnostics_cleanup_issue(ghost);
            }
            // A panicked reopenable worker has stopped the current dispatcher.
            // The queue items lost with it are accounted for by the sink
            // before this milestone is recorded.
            self.record_stop_diagnostics();
        }
        result.map(|_| DiagnosticsDrained { _private: () })
    }

    fn close_terminal(
        &self,
    ) -> Result<crate::shutdown::StopOutcome<DiagnosticsStopped>, DiagnosticTerminalCloseError>
    {
        if self.caller_is_worker() {
            return Err(DiagnosticTerminalCloseError::ReentrantShutdown);
        }
        let mut phase = self.transition.lock();
        if self.caller_is_worker() {
            return Err(DiagnosticTerminalCloseError::ReentrantShutdown);
        }
        match *phase {
            DiagnosticPhase::Closed => {
                if self.sink.load().is_some() || !self.retiring_workers.lock().is_empty() {
                    return Err(DiagnosticTerminalCloseError::InvariantViolation);
                }
                return Ok(crate::shutdown::StopOutcome {
                    certificate: DiagnosticsStopped::new(),
                    issues: Vec::new(),
                });
            }
            DiagnosticPhase::Open => {}
            DiagnosticPhase::Closing => {
                return Err(DiagnosticTerminalCloseError::InvariantViolation);
            }
        }
        *phase = DiagnosticPhase::Closing;

        let previous = crate::ingress::with_diagnostic_linearization(|| {
            let previous = self.sink.swap(None);
            if let Some(previous) = previous.as_ref() {
                self.mark_retiring(previous);
            }
            previous
        });
        let issues = Vec::new();
        if let Some(previous) = previous {
            match previous.shutdown() {
                Ok(()) => {}
                Err(DiagnosticShutdownError::WorkerPanicked) => {
                    self.unmark_retiring(&previous);
                    *phase = DiagnosticPhase::Closed;
                    return Err(DiagnosticTerminalCloseError::WorkerPanicked);
                }
                Err(DiagnosticShutdownError::ReentrantShutdown) => {
                    crate::ingress::with_diagnostic_linearization(|| {
                        self.sink.store(Some(Arc::clone(&previous)));
                    });
                    self.unmark_retiring(&previous);
                    return Err(DiagnosticTerminalCloseError::ReentrantShutdown);
                }
                Err(_) => {
                    self.unmark_retiring(&previous);
                    return Err(DiagnosticTerminalCloseError::InvariantViolation);
                }
            }
            self.unmark_retiring(&previous);
        }
        if self.sink.load().is_some() || !self.retiring_workers.lock().is_empty() {
            return Err(DiagnosticTerminalCloseError::InvariantViolation);
        }
        *phase = DiagnosticPhase::Closed;
        Ok(crate::shutdown::StopOutcome {
            certificate: DiagnosticsStopped::new(),
            issues,
        })
    }

    fn reset(&self) -> XllResult<()> {
        let mut phase = self.transition.lock();
        if *phase != DiagnosticPhase::Closed
            || self.sink.load().is_some()
            || !self.retiring_workers.lock().is_empty()
        {
            return Err(XllError::Internal {
                diagnostic_id: crate::error::DiagnosticId::DIAGNOSTICS_RESET,
            });
        }
        *phase = DiagnosticPhase::Open;
        Ok(())
    }
}

static ROUTER: LazyLock<DiagnosticRouter> = LazyLock::new(|| DiagnosticRouter {
    sink: ArcSwapOption::const_empty(),
    transition: Mutex::new(DiagnosticPhase::Closed),
    retiring_workers: Mutex::new(Vec::new()),
    #[cfg(any(test, feature = "shutdown-refinement"))]
    ghost: Mutex::new(None),
});
static NEXT_ID: AtomicU64 = AtomicU64::new(1);
#[cfg(test)]
use event::{DROPPED_EVENTS, FAILED_WRITES};

#[cfg(test)]
pub(crate) static DIAGNOSTIC_TEST_MUTEX: Mutex<()> = Mutex::new(());

fn router() -> &'static DiagnosticRouter {
    &ROUTER
}

fn admit_published_sink(
    router: &DiagnosticRouter,
    ingress: &crate::ingress::ExportIngress,
    had_sink: bool,
) -> Result<(), DiagnosticInitError> {
    if !ingress.allows_diagnostic_mutation() {
        return Err(DiagnosticInitError::RouterClosed);
    }
    #[cfg(not(any(test, feature = "shutdown-refinement")))]
    let _ = (router, had_sink);
    #[cfg(any(test, feature = "shutdown-refinement"))]
    if let Some(ghost) = router.ghost_handle()
        && !had_sink
        && ghost.active()
    {
        ghost.record_event(crate::shutdown_refinement::GhostEvent::StartDiagnostics);
    }
    Ok(())
}

/// Installs or replaces the process-wide diagnostic sink.
///
/// Events are delivered by a bounded background worker. The replacement is
/// fully constructed and installed before the previous worker is flushed and
/// joined, so a terminal previous worker is never restored over a healthy sink.
pub(crate) fn set_diagnostic_sink(sink: impl DiagnosticSink) -> Result<(), DiagnosticInitError> {
    let router = router();
    if router.caller_is_worker() {
        return Err(DiagnosticInitError::ReentrantMutation);
    }
    let admitted = crate::ingress::with_diagnostic_linearization(|| {
        crate::module_runtime::ingress().allows_diagnostic_mutation()
    });
    if !admitted {
        return Err(DiagnosticInitError::RouterClosed);
    }
    router.replace_with(
        || {
            let sink = Arc::new(AsyncDiagnosticSink::new(sink)?);
            #[cfg(any(test, feature = "shutdown-refinement"))]
            if let Some(ghost) = router.ghost_handle() {
                sink.set_ghost(ghost);
            }
            Ok(sink)
        },
        |had_sink| admit_published_sink(router, crate::module_runtime::ingress(), had_sink),
    )
}

/// Flushes queued diagnostics, stops the logger worker, and releases its sink.
#[cfg(test)]
pub(crate) fn clear_diagnostic_sink() -> Result<DiagnosticsDrained, DiagnosticShutdownError> {
    router().drain_reopenable()
}

pub(crate) fn close_diagnostic_router()
-> Result<crate::shutdown::StopOutcome<DiagnosticsStopped>, DiagnosticTerminalCloseError> {
    router().close_terminal()
}

pub(crate) fn reset_diagnostic_router() -> XllResult<()> {
    router().reset()
}

#[cfg(any(test, feature = "shutdown-refinement"))]
#[derive(Clone, Copy)]
pub(crate) struct GhostDiagnosticsSnapshot {
    pub(crate) running: bool,
    pub(crate) pending: u64,
}

#[cfg(any(test, feature = "shutdown-refinement"))]
pub(crate) fn connect_ghost<F>(
    ghost: crate::shutdown_refinement::GhostHandle,
    initialize: F,
) -> XllResult<()>
where
    F: FnOnce(GhostDiagnosticsSnapshot) -> XllResult<()>,
{
    crate::ingress::with_diagnostic_linearization(|| {
        let snapshot = router().ghost_snapshot();
        initialize(snapshot)?;
        router().set_ghost(ghost);
        Ok(())
    })
}

#[cfg(any(test, feature = "shutdown-refinement"))]
pub(crate) fn record_ghost_diagnostics_stopped(
    ghost: crate::shutdown_refinement::GhostHandle,
) -> XllResult<()> {
    crate::ingress::with_diagnostic_linearization(|| {
        let state = ghost.state();
        if !state.resources.diagnostics_running {
            return Ok(());
        }
        if state.resources.diagnostics_pending != 0 {
            ghost
                .fail_stop(crate::shutdown_refinement::GhostFailure::DiagnosticsShutdownFailed)
                .map_err(|_| XllError::Internal {
                    diagnostic_id: crate::error::DiagnosticId::DIAGNOSTICS_FAILURE,
                })?;
            Err(XllError::Internal {
                diagnostic_id: crate::error::DiagnosticId::DIAGNOSTICS_PENDING,
            })
        } else {
            ghost.record_event(crate::shutdown_refinement::GhostEvent::StopDiagnostics);
            Ok(())
        }
    })
}

#[cfg(any(test, feature = "shutdown-refinement"))]
fn record_ghost_diagnostics_cleanup_issue(ghost: crate::shutdown_refinement::GhostHandle) {
    crate::ingress::with_diagnostic_linearization(|| {
        if ghost.active() {
            ghost.record_event(crate::shutdown_refinement::GhostEvent::RecordCleanupIssue);
        }
    });
}

/// Number of diagnostic events dropped because the bounded queue was full or closed.
#[cfg(test)]
#[must_use]
pub(crate) fn dropped_diagnostic_events() -> u64 {
    DROPPED_EVENTS.load(Ordering::Relaxed)
}

/// Number of file diagnostic deliveries that failed during write or rotation.
#[cfg(test)]
#[must_use]
pub(crate) fn failed_diagnostic_writes() -> u64 {
    FAILED_WRITES.load(Ordering::Relaxed)
}

/// Reports a failure without allowing tracing subscribers or user-provided sink code
/// to unwind into an Excel ABI entry point.
pub(crate) fn report_no_unwind(udf_id: &'static str, error: &XllError) -> DiagnosticId {
    let diagnostic_id = DiagnosticId::from_u64(NEXT_ID.fetch_add(1, Ordering::Relaxed));
    let argument = match error {
        XllError::Input { argument, .. } => Some(*argument),
        _ => None,
    };
    let _ = catch_unwind(AssertUnwindSafe(|| {
        tracing::event!(
            tracing::Level::ERROR,
            udf = udf_id,
            argument,
            diagnostic_id = diagnostic_id.as_u64(),
            error = %error,
            error_debug = ?error,
            "XLL invocation failed"
        );
        let current = router().current();
        if let Some(sink) = current.as_ref() {
            sink.report(OwnedDiagnosticEvent {
                udf_id,
                argument,
                error: error.clone(),
                diagnostic_id,
                timestamp: SystemTime::now(),
            });
        }
    }));
    diagnostic_id
}

fn deliver_no_unwind<S: DiagnosticSink>(sink: &S, event: &DiagnosticEvent<'_>) {
    let _ = catch_unwind(AssertUnwindSafe(|| sink.report(event)));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    struct PanickingSink;

    impl DiagnosticSink for PanickingSink {
        fn report(&self, _: &DiagnosticEvent<'_>) {
            panic!("injected diagnostic failure");
        }
    }

    #[test]
    fn panicking_tracing_subscriber_does_not_unwind_report_no_unwind() {
        let _router_guard = prepare_global_router();
        struct PanickingSubscriber;
        impl tracing::Subscriber for PanickingSubscriber {
            fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
                true
            }
            fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
                tracing::span::Id::from_u64(1)
            }
            fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
            fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
            fn event(&self, _: &tracing::Event<'_>) {
                panic!("injected tracing subscriber panic");
            }
            fn enter(&self, _: &tracing::span::Id) {}
            fn exit(&self, _: &tracing::span::Id) {}
        }

        let dispatch = tracing::Dispatch::new(PanickingSubscriber);
        let _guard = tracing::dispatcher::set_default(&dispatch);

        let result = catch_unwind(AssertUnwindSafe(|| {
            report_no_unwind("panicking_subscriber_test", &XllError::Panic)
        }));
        assert!(
            result.is_ok(),
            "report_no_unwind must not unwind when tracing subscriber panics"
        );
    }

    struct ReentrantClearSink {
        clear_result: Arc<Mutex<Option<Result<(), DiagnosticShutdownError>>>>,
    }

    impl DiagnosticSink for ReentrantClearSink {
        fn report(&self, _: &DiagnosticEvent<'_>) {
            let mut guard = self.clear_result.lock();
            if guard.is_none() {
                let res = clear_diagnostic_sink().map(|_| ());
                *guard = Some(res);
            }
        }
    }

    struct GlobalRouterTestGuard {
        _module_lease: crate::ingress::TestModuleLease,
        _diagnostic_lock: parking_lot::MutexGuard<'static, ()>,
    }

    impl Drop for GlobalRouterTestGuard {
        fn drop(&mut self) {
            let ingress = crate::module_runtime::ingress();
            if ingress.phase() != crate::ingress::PHASE_CLOSED {
                ingress.begin_close_with(|| {});
                let _ = ingress.seal_and_drain();
            }
        }
    }

    fn prepare_global_router() -> GlobalRouterTestGuard {
        let module_lease = crate::ingress::acquire_test_module_lease();
        let diagnostic_lock = DIAGNOSTIC_TEST_MUTEX.lock();
        let ingress = crate::module_runtime::ingress();
        if ingress.phase() != crate::ingress::PHASE_CLOSED {
            ingress.begin_close_with(|| {});
            let _ = ingress.seal_and_drain();
        }
        ingress.begin_opening();
        let _ = close_diagnostic_router();
        router().reset().unwrap();
        GlobalRouterTestGuard {
            _module_lease: module_lease,
            _diagnostic_lock: diagnostic_lock,
        }
    }

    #[test]
    fn reentrant_clear_diagnostic_sink_returns_error_and_preserves_sink() {
        let _router_guard = prepare_global_router();
        let clear_result = Arc::new(Mutex::new(None));
        set_diagnostic_sink(ReentrantClearSink {
            clear_result: Arc::clone(&clear_result),
        })
        .unwrap();

        report_no_unwind("reentrant_test", &XllError::Panic);

        loop {
            if clear_result.lock().is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        let res = clear_result.lock().take().unwrap();
        assert_eq!(res, Err(DiagnosticShutdownError::ReentrantShutdown));

        // Clearing from the main thread succeeds (sink was preserved)
        assert!(clear_diagnostic_sink().is_ok());
    }

    struct ReentrantReplaceSink {
        replace_result: Arc<Mutex<Option<Result<(), DiagnosticInitError>>>>,
    }

    impl DiagnosticSink for ReentrantReplaceSink {
        fn report(&self, _: &DiagnosticEvent<'_>) {
            let mut guard = self.replace_result.lock();
            if guard.is_none() {
                let res = set_diagnostic_sink(PanickingSink);
                *guard = Some(res);
            }
        }
    }

    #[test]
    fn reentrant_set_diagnostic_sink_returns_error_and_does_not_commit() {
        let _router_guard = prepare_global_router();
        let replace_result = Arc::new(Mutex::new(None));
        set_diagnostic_sink(ReentrantReplaceSink {
            replace_result: Arc::clone(&replace_result),
        })
        .unwrap();

        report_no_unwind("reentrant_replace_test", &XllError::Panic);

        loop {
            if replace_result.lock().is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        let res = replace_result.lock().take().unwrap();
        assert!(
            matches!(res, Err(DiagnosticInitError::ReentrantMutation)),
            "expected ReentrantMutation, got {res:?}"
        );

        // Clearing from the main thread succeeds since the original sink was preserved
        assert!(clear_diagnostic_sink().is_ok());
    }

    #[test]
    fn published_sink_is_rejected_after_ingress_close() {
        let ingress = crate::ingress::ExportIngress::new();
        ingress.begin_opening();
        ingress.complete_open(|| Ok::<(), ()>(())).unwrap().unwrap();

        let ghost = Arc::new(crate::shutdown_refinement::ShutdownGhost::new());
        ghost
            .begin_generation(1, crate::shutdown_refinement::GhostResources::opened(0, 0))
            .unwrap();
        let ghost_for_close = Arc::clone(&ghost);
        ingress.begin_close_with(move || {
            ghost_for_close.record_event(crate::shutdown_refinement::GhostEvent::BeginClose);
        });

        let router = DiagnosticRouter {
            sink: ArcSwapOption::const_empty(),
            transition: Mutex::new(DiagnosticPhase::Open),
            retiring_workers: Mutex::new(Vec::new()),
            ghost: Mutex::new(Some(Arc::clone(&ghost))),
        };
        let result = router.replace_with(
            || AsyncDiagnosticSink::new(CountingSink(Arc::new(AtomicUsize::new(0)))).map(Arc::new),
            |had_sink| admit_published_sink(&router, &ingress, had_sink),
        );

        assert!(matches!(result, Err(DiagnosticInitError::RouterClosed)));
        assert!(router.current().is_none());
        assert!(matches!(
            ghost.state().phase,
            crate::shutdown_refinement::GhostPhase::Closing(
                crate::shutdown_refinement::GhostStage::DrainCalls
            )
        ));
    }

    #[test]
    fn custom_sink_panic_is_contained() {
        let error = XllError::Panic;
        let event = DiagnosticEvent {
            udf_id: "test",
            argument: None,
            error: &error,
            diagnostic_id: DiagnosticId::from_u64(1),
            timestamp: SystemTime::now(),
        };
        deliver_no_unwind(&PanickingSink, &event);
    }

    #[test]
    fn worker_spawn_failure_is_returned_instead_of_panicking() {
        let error = AsyncDiagnosticSink::new_named(PanickingSink, "invalid\0name")
            .err()
            .unwrap();
        assert!(matches!(error, DiagnosticInitError::WorkerSpawn(_)));
    }

    #[test]
    fn worker_panic_is_observable_during_shutdown() {
        let (sender, _receiver) = std::sync::mpsc::sync_channel(1);
        let worker = std::thread::spawn(|| panic!("injected diagnostic worker panic"));
        let worker_thread_id = worker.thread().id();
        let sink = AsyncDiagnosticSink {
            sender: Mutex::new(Some(sender)),
            worker: Mutex::new(Some(worker)),
            worker_thread_id,
            pending: Arc::new(AtomicU64::new(0)),
            ghost: Arc::new(Mutex::new(None)),
        };
        assert_eq!(
            sink.shutdown(),
            Err(DiagnosticShutdownError::WorkerPanicked)
        );
    }

    struct CountingSink(Arc<AtomicUsize>);

    impl DiagnosticSink for CountingSink {
        fn report(&self, _: &DiagnosticEvent<'_>) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[test]
    fn router_flushes_the_previous_sink_before_replacement() {
        let first = Arc::new(AtomicUsize::new(0));
        let second = Arc::new(AtomicUsize::new(0));
        let router = DiagnosticRouter {
            sink: ArcSwapOption::const_empty(),
            transition: Mutex::new(DiagnosticPhase::Open),
            retiring_workers: Mutex::new(Vec::new()),
            ghost: Mutex::new(None),
        };
        router
            .replace_with(
                || AsyncDiagnosticSink::new(CountingSink(Arc::clone(&first))).map(Arc::new),
                |_| Ok(()),
            )
            .unwrap();
        router
            .current()
            .as_ref()
            .unwrap()
            .report(OwnedDiagnosticEvent {
                udf_id: "reload",
                argument: None,
                error: XllError::Panic,
                diagnostic_id: DiagnosticId::from_u64(1),
                timestamp: SystemTime::now(),
            });
        router
            .replace_with(
                || AsyncDiagnosticSink::new(CountingSink(Arc::clone(&second))).map(Arc::new),
                |_| Ok(()),
            )
            .unwrap();
        router
            .current()
            .as_ref()
            .unwrap()
            .report(OwnedDiagnosticEvent {
                udf_id: "reload",
                argument: None,
                error: XllError::Panic,
                diagnostic_id: DiagnosticId::from_u64(2),
                timestamp: SystemTime::now(),
            });
        router.drain_reopenable().unwrap();
        assert_eq!(first.load(Ordering::Relaxed), 1);
        assert_eq!(second.load(Ordering::Relaxed), 1);
    }

    struct RetiringReentrySink {
        router: Arc<DiagnosticRouter>,
        started: std::sync::mpsc::SyncSender<()>,
        release: Mutex<std::sync::mpsc::Receiver<()>>,
        result: std::sync::mpsc::SyncSender<Result<(), DiagnosticShutdownError>>,
    }

    impl DiagnosticSink for RetiringReentrySink {
        fn report(&self, _: &DiagnosticEvent<'_>) {
            let _ = self.started.send(());
            let _ = self.release.lock().recv();
            let _ = self.result.send(self.router.drain_reopenable().map(|_| ()));
        }
    }

    #[test]
    fn retiring_worker_reentry_is_rejected_before_waiting_for_transition() {
        let router = Arc::new(DiagnosticRouter {
            sink: ArcSwapOption::const_empty(),
            transition: Mutex::new(DiagnosticPhase::Open),
            retiring_workers: Mutex::new(Vec::new()),
            ghost: Mutex::new(None),
        });
        let (started_tx, started_rx) = std::sync::mpsc::sync_channel(1);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
        let (result_tx, result_rx) = std::sync::mpsc::sync_channel(1);
        let old = Arc::new(
            AsyncDiagnosticSink::new(RetiringReentrySink {
                router: Arc::clone(&router),
                started: started_tx,
                release: Mutex::new(release_rx),
                result: result_tx,
            })
            .unwrap(),
        );
        router.replace_with(|| Ok(old), |_| Ok(())).unwrap();
        router
            .current()
            .as_ref()
            .unwrap()
            .report(OwnedDiagnosticEvent {
                udf_id: "retiring",
                argument: None,
                error: XllError::Panic,
                diagnostic_id: DiagnosticId::from_u64(1),
                timestamp: SystemTime::now(),
            });
        started_rx.recv().unwrap();

        let replacement = Arc::new(
            AsyncDiagnosticSink::new(CountingSink(Arc::new(AtomicUsize::new(0)))).unwrap(),
        );
        let replacing_router = Arc::clone(&router);
        let (published_tx, published_rx) = std::sync::mpsc::channel();
        let replacing = std::thread::spawn(move || {
            replacing_router.replace_with(
                || Ok(replacement),
                move |_| {
                    published_tx.send(()).unwrap();
                    Ok(())
                },
            )
        });

        published_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("replacement was not published");

        release_tx.send(()).unwrap();
        assert_eq!(
            result_rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .expect("retiring worker did not report reentry result"),
            Err(DiagnosticShutdownError::ReentrantShutdown)
        );
        replacing.join().unwrap().unwrap();
        router.drain_reopenable().unwrap();
    }

    #[test]
    fn panicked_previous_worker_is_not_restored_over_a_healthy_replacement() {
        let ghost = Arc::new(crate::shutdown_refinement::ShutdownGhost::new());
        let mut resources = crate::shutdown_refinement::GhostResources::opened(0, 0);
        resources.diagnostics_running = true;
        ghost.begin_generation(1, resources).unwrap();
        let worker = std::thread::spawn(|| panic!("injected diagnostic worker panic"));
        let worker_thread_id = worker.thread().id();
        let (sender, _receiver) = std::sync::mpsc::sync_channel(1);
        let terminal = Arc::new(AsyncDiagnosticSink {
            sender: Mutex::new(Some(sender)),
            worker: Mutex::new(Some(worker)),
            worker_thread_id,
            pending: Arc::new(AtomicU64::new(0)),
            ghost: Arc::new(Mutex::new(None)),
        });
        let delivered = Arc::new(AtomicUsize::new(0));
        let replacement =
            Arc::new(AsyncDiagnosticSink::new(CountingSink(Arc::clone(&delivered))).unwrap());
        let router = DiagnosticRouter {
            sink: ArcSwapOption::new(Some(terminal)),
            transition: Mutex::new(DiagnosticPhase::Open),
            retiring_workers: Mutex::new(Vec::new()),
            ghost: Mutex::new(Some(Arc::clone(&ghost))),
        };

        router
            .replace_with(|| Ok(Arc::clone(&replacement)), |_| Ok(()))
            .unwrap();
        let current = router.current();
        assert!(Arc::ptr_eq(current.as_ref().unwrap(), &replacement));
        router
            .current()
            .as_ref()
            .unwrap()
            .report(OwnedDiagnosticEvent {
                udf_id: "replacement",
                argument: None,
                error: XllError::Panic,
                diagnostic_id: DiagnosticId::from_u64(1),
                timestamp: SystemTime::now(),
            });
        assert_eq!(
            ghost.state().phase,
            crate::shutdown_refinement::GhostPhase::Open
        );
        assert_eq!(ghost.state().resources.cleanup_issues, 1);
        assert!(ghost.state().resources.diagnostics_running);
        router.drain_reopenable().unwrap();
        assert_eq!(delivered.load(Ordering::Relaxed), 2);
        assert_eq!(
            ghost.state().phase,
            crate::shutdown_refinement::GhostPhase::Open
        );
        assert_eq!(ghost.state().resources.cleanup_issues, 1);
        assert!(!ghost.state().resources.diagnostics_running);
    }

    #[test]
    fn reopenable_worker_panic_does_not_fail_stop_the_live_router() {
        let ghost = Arc::new(crate::shutdown_refinement::ShutdownGhost::new());
        let mut resources = crate::shutdown_refinement::GhostResources::opened(0, 0);
        resources.diagnostics_running = true;
        ghost.begin_generation(1, resources).unwrap();
        let worker = std::thread::spawn(|| panic!("injected diagnostic worker panic"));
        let worker_thread_id = worker.thread().id();
        let (sender, _receiver) = std::sync::mpsc::sync_channel(1);
        let router = DiagnosticRouter {
            sink: ArcSwapOption::new(Some(Arc::new(AsyncDiagnosticSink {
                sender: Mutex::new(Some(sender)),
                worker: Mutex::new(Some(worker)),
                worker_thread_id,
                pending: Arc::new(AtomicU64::new(0)),
                ghost: Arc::new(Mutex::new(None)),
            }))),
            transition: Mutex::new(DiagnosticPhase::Open),
            retiring_workers: Mutex::new(Vec::new()),
            ghost: Mutex::new(Some(Arc::clone(&ghost))),
        };

        assert!(matches!(
            router.drain_reopenable(),
            Err(DiagnosticShutdownError::WorkerPanicked)
        ));
        assert_eq!(router.phase(), DiagnosticPhase::Open);
        assert!(router.current().is_none());
        assert_eq!(
            ghost.state().phase,
            crate::shutdown_refinement::GhostPhase::Open
        );
        assert_eq!(ghost.state().resources.cleanup_issues, 1);
        assert!(!ghost.state().resources.diagnostics_running);
    }

    #[test]
    fn panicked_worker_accounts_for_queued_events_as_discarded() {
        let ghost = Arc::new(crate::shutdown_refinement::ShutdownGhost::new());
        let mut resources = crate::shutdown_refinement::GhostResources::opened(0, 0);
        resources.diagnostics_running = true;
        ghost.begin_generation(1, resources).unwrap();

        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            entered_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            panic!("injected diagnostic worker panic");
        });
        entered_rx.recv().unwrap();
        let worker_thread_id = worker.thread().id();
        let (sender, _receiver) = std::sync::mpsc::sync_channel(1);
        let sink = AsyncDiagnosticSink {
            sender: Mutex::new(Some(sender)),
            worker: Mutex::new(Some(worker)),
            worker_thread_id,
            pending: Arc::new(AtomicU64::new(0)),
            ghost: Arc::new(Mutex::new(Some(Arc::clone(&ghost)))),
        };
        sink.report(OwnedDiagnosticEvent {
            udf_id: "discarded",
            argument: None,
            error: XllError::Panic,
            diagnostic_id: DiagnosticId::from_u64(1),
            timestamp: SystemTime::now(),
        });
        release_tx.send(()).unwrap();

        assert!(matches!(
            sink.shutdown(),
            Err(DiagnosticShutdownError::WorkerPanicked)
        ));
        assert_eq!(
            ghost.state().phase,
            crate::shutdown_refinement::GhostPhase::Open
        );
        assert_eq!(ghost.state().resources.diagnostics_pending, 0);
        assert!(ghost.trace_json().unwrap().contains("discardDiagnostic"));
    }

    #[test]
    fn failed_file_sink_construction_preserves_the_current_sink() {
        let _router_guard = prepare_global_router();
        let delivered = Arc::new(AtomicUsize::new(0));
        set_diagnostic_sink(CountingSink(Arc::clone(&delivered))).unwrap();
        let directory = tempfile::tempdir().unwrap();

        let result = install_file_diagnostic_sink_at(directory.path().to_path_buf());
        assert!(matches!(result, Err(DiagnosticInitError::Io(_))));
        report_no_unwind("preserved", &XllError::Panic);
        clear_diagnostic_sink().unwrap();
        assert_eq!(delivered.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn file_delivery_failures_are_counted() {
        let before = failed_diagnostic_writes();
        let tempdir = tempfile::tempdir().unwrap();
        let sink = FileDiagnosticSink {
            log: Mutex::new(RotatingLog {
                path: tempdir.path().join("unavailable.log"),
                file: None,
                size: 0,
                maximum_bytes: LOG_MAX_BYTES,
                generations: LOG_GENERATIONS,
            }),
        };
        let error = XllError::Panic;
        sink.report(&DiagnosticEvent {
            udf_id: "failed_write",
            argument: None,
            error: &error,
            diagnostic_id: DiagnosticId::from_u64(1),
            timestamp: SystemTime::now(),
        });
        assert!(failed_diagnostic_writes() > before);
    }

    #[test]
    fn file_diagnostic_records_are_single_bounded_json_lines() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("diagnostics.log");
        let sink = FileDiagnosticSink {
            log: Mutex::new(RotatingLog::open(path.clone()).unwrap()),
        };
        let error = XllError::Native {
            code: 17,
            message: format!(
                "first line\nsecond\tline \"quoted\" {}",
                "x".repeat(32 * 1024)
            ),
        };

        sink.report(&DiagnosticEvent {
            udf_id: "native_failure",
            argument: Some("currency"),
            error: &error,
            diagnostic_id: DiagnosticId::from_u64(42),
            timestamp: SystemTime::UNIX_EPOCH,
        });

        let contents = fs::read_to_string(path).unwrap();
        assert_eq!(contents.lines().count(), 1);
        let record: serde_json::Value = serde_json::from_str(contents.trim_end()).unwrap();
        assert_eq!(record["diagnostic_id"], 42);
        assert_eq!(record["udf"], "native_failure");
        assert_eq!(record["argument"], "currency");
        let logged_error = record["error"].as_str().unwrap();
        assert!(logged_error.len() <= DIAGNOSTIC_TEXT_MAX_BYTES);
        assert!(logged_error.ends_with(DIAGNOSTIC_TRUNCATION_SUFFIX));
        assert!(contents.contains("\\n"));
        assert!(contents.contains("\\t"));
        assert!(contents.len() < LOG_MAX_BYTES as usize);
    }

    struct BlockingSink {
        first: AtomicBool,
        started: std::sync::mpsc::SyncSender<()>,
        release: Mutex<std::sync::mpsc::Receiver<()>>,
    }

    impl DiagnosticSink for BlockingSink {
        fn report(&self, _: &DiagnosticEvent<'_>) {
            if self.first.swap(false, Ordering::AcqRel) {
                let _ = self.started.send(());
                let _ = self.release.lock().recv();
            }
        }
    }

    fn wait_for_transition_lock(router: &DiagnosticRouter) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        while router.transition.try_lock().is_some() {
            assert!(
                std::time::Instant::now() < deadline,
                "transition lock was not acquired"
            );
            std::thread::yield_now();
        }
    }

    #[test]
    fn terminal_close_waiting_does_not_run_a_pending_install_factory() {
        let router = Arc::new(DiagnosticRouter {
            sink: ArcSwapOption::const_empty(),
            transition: Mutex::new(DiagnosticPhase::Open),
            retiring_workers: Mutex::new(Vec::new()),
            ghost: Mutex::new(None),
        });
        let (started_tx, started_rx) = std::sync::mpsc::sync_channel(1);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
        router
            .replace_with(
                || {
                    AsyncDiagnosticSink::new(BlockingSink {
                        first: AtomicBool::new(true),
                        started: started_tx,
                        release: Mutex::new(release_rx),
                    })
                    .map(Arc::new)
                },
                |_| Ok(()),
            )
            .unwrap();
        router
            .current()
            .as_ref()
            .unwrap()
            .report(OwnedDiagnosticEvent {
                udf_id: "terminal-close-race",
                argument: None,
                error: XllError::Panic,
                diagnostic_id: DiagnosticId::from_u64(1),
                timestamp: SystemTime::now(),
            });
        started_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .unwrap();

        let closing_router = Arc::clone(&router);
        let closing = std::thread::spawn(move || closing_router.close_terminal());
        wait_for_transition_lock(&router);

        let factory_calls = Arc::new(AtomicUsize::new(0));
        let factory_calls_for_install = Arc::clone(&factory_calls);
        let installing_router = Arc::clone(&router);
        let (install_started_tx, install_started_rx) = std::sync::mpsc::sync_channel(1);
        let installing = std::thread::spawn(move || {
            install_started_tx.send(()).unwrap();
            installing_router.replace_with(
                || {
                    factory_calls_for_install.fetch_add(1, Ordering::AcqRel);
                    AsyncDiagnosticSink::new(CountingSink(Arc::new(AtomicUsize::new(0))))
                        .map(Arc::new)
                },
                |_| Ok(()),
            )
        });
        install_started_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .unwrap();
        std::thread::yield_now();
        assert_eq!(factory_calls.load(Ordering::Acquire), 0);

        release_tx.send(()).unwrap();
        let outcome = closing.join().unwrap().unwrap();
        let install_result = installing.join().unwrap();
        assert!(matches!(
            install_result,
            Err(DiagnosticInitError::RouterClosed)
        ));
        assert_eq!(factory_calls.load(Ordering::Acquire), 0);
        assert!(router.current().is_none());
        assert_eq!(router.phase(), DiagnosticPhase::Closed);
        assert!(router.retiring_workers.lock().is_empty());
        let _certificate = outcome.certificate;
    }

    #[test]
    fn install_that_linearizes_first_is_joined_by_terminal_close() {
        let router = Arc::new(DiagnosticRouter {
            sink: ArcSwapOption::const_empty(),
            transition: Mutex::new(DiagnosticPhase::Open),
            retiring_workers: Mutex::new(Vec::new()),
            ghost: Mutex::new(None),
        });
        let (factory_entered_tx, factory_entered_rx) = std::sync::mpsc::sync_channel(1);
        let (release_factory_tx, release_factory_rx) = std::sync::mpsc::sync_channel(1);
        let installing_router = Arc::clone(&router);
        let installing = std::thread::spawn(move || {
            installing_router.replace_with(
                || {
                    factory_entered_tx.send(()).unwrap();
                    release_factory_rx.recv().unwrap();
                    AsyncDiagnosticSink::new(CountingSink(Arc::new(AtomicUsize::new(0))))
                        .map(Arc::new)
                },
                |_| Ok(()),
            )
        });
        factory_entered_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .unwrap();

        let closing_router = Arc::clone(&router);
        let closing = std::thread::spawn(move || closing_router.close_terminal());
        wait_for_transition_lock(&router);
        release_factory_tx.send(()).unwrap();

        installing.join().unwrap().unwrap();
        let outcome = closing.join().unwrap().unwrap();
        assert!(outcome.issues.is_empty());
        assert!(router.current().is_none());
        assert_eq!(router.phase(), DiagnosticPhase::Closed);
        assert!(router.retiring_workers.lock().is_empty());
        let _certificate = outcome.certificate;
    }

    #[test]
    fn reset_requires_a_terminally_closed_router_before_reinstall() {
        let router = DiagnosticRouter {
            sink: ArcSwapOption::const_empty(),
            transition: Mutex::new(DiagnosticPhase::Open),
            retiring_workers: Mutex::new(Vec::new()),
            ghost: Mutex::new(None),
        };
        let _certificate = router.close_terminal().unwrap().certificate;

        let factory_calls = AtomicUsize::new(0);
        let result = router.replace_with(
            || {
                factory_calls.fetch_add(1, Ordering::AcqRel);
                AsyncDiagnosticSink::new(CountingSink(Arc::new(AtomicUsize::new(0)))).map(Arc::new)
            },
            |_| Ok(()),
        );
        assert!(matches!(result, Err(DiagnosticInitError::RouterClosed)));
        assert_eq!(factory_calls.load(Ordering::Acquire), 0);

        router.reset().unwrap();
        router
            .replace_with(
                || {
                    AsyncDiagnosticSink::new(CountingSink(Arc::new(AtomicUsize::new(0))))
                        .map(Arc::new)
                },
                |_| Ok(()),
            )
            .unwrap();
        let _certificate = router.close_terminal().unwrap().certificate;
    }

    #[test]
    fn terminal_close_rejects_a_worker_panic() {
        let worker = std::thread::spawn(|| panic!("injected diagnostic worker panic"));
        let worker_thread_id = worker.thread().id();
        let (sender, _receiver) = std::sync::mpsc::sync_channel(1);
        let router = DiagnosticRouter {
            sink: ArcSwapOption::new(Some(Arc::new(AsyncDiagnosticSink {
                sender: Mutex::new(Some(sender)),
                worker: Mutex::new(Some(worker)),
                worker_thread_id,
                pending: Arc::new(AtomicU64::new(0)),
                ghost: Arc::new(Mutex::new(None)),
            }))),
            transition: Mutex::new(DiagnosticPhase::Open),
            retiring_workers: Mutex::new(Vec::new()),
            ghost: Mutex::new(None),
        };

        let error = match router.close_terminal() {
            Ok(_) => panic!("worker panic must fail terminal close"),
            Err(error) => error,
        };
        assert!(matches!(
            error,
            DiagnosticTerminalCloseError::WorkerPanicked
        ));
        assert!(router.current().is_none());
        assert_eq!(router.phase(), DiagnosticPhase::Closed);
        assert!(router.retiring_workers.lock().is_empty());
    }

    #[test]
    fn full_diagnostic_queue_drops_instead_of_blocking_the_caller() {
        let (started_tx, started_rx) = std::sync::mpsc::sync_channel(1);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
        let sink = AsyncDiagnosticSink::new(BlockingSink {
            first: AtomicBool::new(true),
            started: started_tx,
            release: Mutex::new(release_rx),
        })
        .unwrap();
        let before = dropped_diagnostic_events();

        sink.report(OwnedDiagnosticEvent {
            udf_id: "bounded",
            argument: None,
            error: XllError::Panic,
            diagnostic_id: DiagnosticId::from_u64(1),
            timestamp: SystemTime::now(),
        });
        started_rx.recv().unwrap();
        for diagnostic_id in 2..=(DIAGNOSTIC_QUEUE_CAPACITY as u64 + 2) {
            sink.report(OwnedDiagnosticEvent {
                udf_id: "bounded",
                argument: None,
                error: XllError::Panic,
                diagnostic_id: DiagnosticId::from_u64(diagnostic_id),
                timestamp: SystemTime::now(),
            });
        }
        assert!(dropped_diagnostic_events() > before);

        release_tx.send(()).unwrap();
        sink.shutdown().unwrap();
    }

    #[test]
    fn file_log_rotates_and_caps_generations() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("diagnostics.log");

        for value in ["first", "second", "third"] {
            let mut log = RotatingLog::open_with_policy(path.clone(), 12, 2).unwrap();
            log.write_line(value).unwrap();
        }

        assert_eq!(fs::read_to_string(&path).unwrap(), "third\n");
        assert_eq!(
            fs::read_to_string(path.with_file_name("diagnostics.log.1")).unwrap(),
            "second\n"
        );
        assert_eq!(
            fs::read_to_string(path.with_file_name("diagnostics.log.2")).unwrap(),
            "first\n"
        );
        assert!(!path.with_file_name("diagnostics.log.3").exists());
    }

    #[test]
    fn rotating_log_rejects_a_record_larger_than_the_log_limit() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("diagnostics.log");
        let mut log = RotatingLog::open_with_policy(path, 4, 2).unwrap();

        let error = log.write_line("1234").unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn addin_id_rejects_path_traversal_and_reserved_names() {
        for invalid in [
            "..\\..\\outside",
            "../outside",
            "C:\\Windows",
            "\\\\server\\share",
            ".",
            "..",
            "CON",
            "CON.txt",
            "PRN",
            "AUX",
            "NUL",
            "NUL.log",
            "COM1",
            "LPT1.data",
            "CONIN$",
            "CONOUT$",
            "LPT1",
            "addin.",
            "addin ",
            "bad/path",
            "bad\\path",
            "",
        ] {
            assert!(
                AddinId::parse(invalid).is_err(),
                "expected AddinId::parse({invalid:?}) to fail"
            );
        }

        assert!(AddinId::parse("valid-addin-id_123").is_ok());
        assert!(AddinId::parse(" valid-addin-id_123").is_ok());
    }

    #[test]
    fn non_sync_sink_can_be_installed() {
        use std::cell::RefCell;

        struct NonSyncSink {
            _events: RefCell<Vec<DiagnosticId>>,
        }

        // RefCell is Send + !Sync
        impl DiagnosticSink for NonSyncSink {
            fn report(&self, event: &DiagnosticEvent<'_>) {
                self._events.borrow_mut().push(event.diagnostic_id);
            }
        }

        let _router_guard = prepare_global_router();
        set_diagnostic_sink(NonSyncSink {
            _events: RefCell::new(Vec::new()),
        })
        .unwrap();
        clear_diagnostic_sink().unwrap();
    }

    #[test]
    fn diagnostic_stats_snapshot_metrics() {
        let stats = diagnostic_stats();
        assert_eq!(stats.dropped_events, dropped_diagnostic_events());
        assert_eq!(stats.file_write_failures, failed_diagnostic_writes());
    }
}

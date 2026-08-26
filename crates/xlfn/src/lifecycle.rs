use crate::addin::Addin;
use crate::diagnostics::AddinId;
use crate::generation::RuntimeGeneration;
use crate::host_callback::HostCallbackSession;
use crate::registration::HostRegistrar;
use crate::runtime::Runtime;
use crate::{XllError, XllResult};
use std::panic::{AssertUnwindSafe, catch_unwind};
use xlfn_kernel::thread_affine::ThreadAffineError;

mod authority;
mod boundary;
mod open;
mod open_txn;
mod orchestration;
mod phase;
mod removal_txn;
mod rollback;
mod state;
mod teardown;

pub(crate) use authority::LifecycleAuthority;
pub(crate) use boundary::{host_auto_close, host_auto_open, host_auto_remove};
pub(super) use open::open_addin_boundary as open_addin;
pub(crate) use open_txn::{HostOpeningState, OpenAttemptBegun, OpenGenerationStaged, OpeningTxn};
pub(crate) use phase::{HostLifecycleIntent, LifecyclePhase};
pub(crate) use removal_txn::{
    ClosedWitness, FinalRemoval, OpenRollback, QuiescenceProof, RemovalOwner,
    TerminalCertificateKind,
};
use rollback::{active_runtime_generation, rollback_open};
pub(crate) use state::{
    GenerationAdmission, LifecycleAccess, LifecycleCoordinator, LifecycleRemovalState,
    OpenFailureDisposition,
};
use teardown::drain_execution;

macro_rules! private_lifecycle_token {
    ($name:ident) => {
        #[derive(Debug)]
        pub(crate) struct $name {
            _private: (),
        }

        impl $name {
            // Only lifecycle descendants can issue this proof in production.
            const fn issue() -> Self {
                Self { _private: () }
            }

            #[cfg(test)]
            pub(crate) const fn for_test() -> Self {
                Self { _private: () }
            }
        }
    };
}

private_lifecycle_token!(HostCallbacksDetached);
private_lifecycle_token!(AddinQuiesced);
private_lifecycle_token!(GenerationReclaimed);

#[cfg(not(feature = "async"))]
private_lifecycle_token!(AsyncStopped);

fn lifecycle_access_error(error: ThreadAffineError) -> XllError {
    let diagnostic_id = match error {
        ThreadAffineError::WrongThread | ThreadAffineError::StaleAccess => {
            crate::diagnostics::id::DiagnosticId::LIFECYCLE_THREAD
        }
        _ => crate::diagnostics::id::DiagnosticId::LIFECYCLE_SLOT,
    };
    XllError::Internal { diagnostic_id }
}

fn write_startup_log(addin_id: &AddinId, message: &str) {
    #[cfg(target_os = "windows")]
    {
        use std::fs;
        let Some(local) = std::env::var_os("LOCALAPPDATA") else {
            return;
        };
        let directory = std::path::PathBuf::from(local)
            .join(addin_id.as_str())
            .join("logs");
        if fs::create_dir_all(&directory).is_err() {
            return;
        }
        let _ = crate::diagnostics::append_startup_log(&directory.join("startup.log"), message);
    }
    #[cfg(not(target_os = "windows"))]
    let _ = (addin_id, message);
}

fn retry_metadata_debt<A: Addin>(
    runtime: &Runtime<A>,
    callbacks: &mut HostCallbackSession,
) -> XllResult<()> {
    let debts = runtime.metadata_debt();
    if debts.is_empty() {
        return Ok(());
    }
    let outcome = HostRegistrar::retry_metadata_debt(callbacks, &debts);
    runtime.replace_metadata_debt(outcome.remaining);
    for error in outcome.cleanup_issues {
        report_cleanup_issue(&crate::shutdown::CleanupIssue {
            component: "Excel metadata debt result",
            kind: crate::shutdown::CleanupIssueKind::HostMemoryLeak,
            error,
        });
    }
    if let Some(error) = outcome.terminal {
        report_boundary_error("xlAutoOpen metadata debt retry", &error);
        return Err(error);
    }
    if runtime.has_metadata_debt() {
        let count = runtime.metadata_debt().len();
        let _ = catch_unwind(AssertUnwindSafe(|| {
            tracing::warn!(count, "Excel metadata debt remains after retry");
        }));
    }
    Ok(())
}

use orchestration::{
    RemovalControl, RemovalSuccess, open_addin_inner, remove_addin, rollback_active_open,
};
fn report_cleanup_issue(issue: &crate::shutdown::CleanupIssue) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        tracing::warn!(
            component = issue.component,
            kind = ?issue.kind,
            error = %issue.error,
            "cleanup issue during shutdown"
        );
    }));
    report_boundary_error(issue.component, &issue.error);
}

#[cold]
fn handle_unload_hazard<A: Addin>(
    _runtime: &Runtime<A>,
    hazard: crate::shutdown::UnloadHazard,
    boundary: &'static str,
    error: &XllError,
) -> RemovalControl {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        tracing::error!(?hazard, %error, "unload safety could not be established");
    }));
    if hazard == crate::shutdown::UnloadHazard::CloseInvariantViolation {
        #[cfg(any(test, feature = "refinement"))]
        _runtime
            .refinement_hooks()
            .fail_stop(_runtime, hazard.shutdown_failure());
        fail_stop_invariant(boundary, error);
    }

    report_boundary_error(boundary, error);
    RemovalControl::Quarantine {
        hazard,
        boundary,
        error: error.clone(),
    }
}

fn commit_removal_control<A: Addin>(
    runtime: &Runtime<A>,
    control: RemovalControl,
) -> RemovalSuccess<'_, A> {
    match control {
        RemovalControl::Quarantine {
            hazard,
            boundary: _boundary,
            error: _error,
        } => {
            quarantine_for_hazard(runtime, hazard);
            RemovalSuccess::Quarantined
        }
    }
}

fn quarantine_runtime<A: Addin>(runtime: &Runtime<A>) {
    runtime.lifecycle_runtime().quarantine();
    #[cfg(any(test, feature = "refinement"))]
    runtime.refinement_hooks().quarantine(
        runtime,
        crate::shutdown_trace::ShutdownFailure::BoundaryPanic,
    );
    quarantine_runtime_resources(runtime);
}

fn quarantine_for_hazard<A: Addin>(runtime: &Runtime<A>, _hazard: crate::shutdown::UnloadHazard) {
    runtime.lifecycle_runtime().quarantine();
    #[cfg(any(test, feature = "refinement"))]
    runtime
        .refinement_hooks()
        .quarantine(runtime, _hazard.shutdown_failure());
    quarantine_runtime_resources(runtime);
}

fn quarantine_runtime_resources<A: Addin>(runtime: &Runtime<A>) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let module_closing = runtime
            .lifecycle_runtime()
            .take_module_closing_for_quarantine();
        let _ = module_closing.seal_and_drain();
        let quarantined = runtime.quarantine_snapshot();
        if let Some((generation, reason)) = quarantined.last() {
            tracing::error!(
                generation = generation.map_or(0, RuntimeGeneration::get),
                ?reason,
                resource_count = quarantined.len(),
                "runtime resources retained in quarantine vault"
            );
        }
    }));
}

#[allow(
    unsafe_code,
    reason = "Windows diagnostic output is the lifecycle FFI leaf"
)]
fn report_boundary_error(boundary: &'static str, error: &XllError) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        crate::diagnostics::report_no_unwind(boundary, error);
        let message = format!("xlfn {boundary}: {error}\n");
        #[cfg(target_os = "windows")]
        {
            use crate::win32::OutputDebugStringW;

            let mut wide = message.encode_utf16().collect::<Vec<_>>();
            wide.push(0);
            // SAFETY: wide is nul-terminated and live for this synchronous call.
            unsafe { OutputDebugStringW(wide.as_ptr()) };
        }
        #[cfg(not(target_os = "windows"))]
        {
            eprint!("{message}");
        }
    }));
}

#[cold]
pub(crate) fn fail_stop_invariant(boundary: &'static str, error: &XllError) -> ! {
    report_boundary_error(boundary, error);

    // Only an internal invariant or module-bookkeeping corruption reaches this
    // function. Operational teardown hazards are quarantined above while the
    // physical module lease remains held.
    #[cfg(not(test))]
    std::process::abort();

    // Unit tests need an unwindable sentinel instead of terminating the test
    // runner. Production builds always take the abort branch above.
    #[cfg(test)]
    panic!("internal unload invariant failed at {boundary}: {error}");
}

#[cold]
pub(crate) fn fail_stop_module_residency(error: &XllError) -> ! {
    report_boundary_error("xlAutoOpen module residency", error);

    // Without the self-reference, returning from xlAutoOpen would permit the
    // host to unload code that is still executing the generated boundary.
    #[cfg(not(test))]
    std::process::abort();

    #[cfg(test)]
    panic!("module residency acquisition failed: {error}");
}

#[cfg(test)]
#[allow(
    unsafe_code,
    reason = "Lifecycle tests exercise the audited FFI return boundary"
)]
mod tests {
    use super::orchestration::{initialize_addin, remove_addin_inner};
    use super::*;
    use crate::addin::{BuildInfo, OpenContext};
    use crate::runtime::AddinLifecycleAccess;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static COMPOSITION_TRACE_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn check_composition_trace_with_lean(
        checker: &std::ffi::OsString,
        label: &str,
        trace: &str,
        artifact_name: &str,
    ) {
        use std::io::Write;
        use std::process::{Command, Stdio};

        if let Some(directory) = std::env::var_os("XLFN_COMPOSITION_TRACE_DIR") {
            let path = std::path::Path::new(&directory).join(artifact_name);
            std::fs::write(path, trace).expect("write Rust composition trace");
        }
        let mut child = Command::new(checker)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap_or_else(|error| {
                panic!("failed to start composition checker for {label}: {error}")
            });
        child
            .stdin
            .take()
            .expect("composition checker stdin is piped")
            .write_all(trace.as_bytes())
            .unwrap_or_else(|error| panic!("failed to write {label} composition trace: {error}"));
        let output = child.wait_with_output().unwrap_or_else(|error| {
            panic!("failed to wait for {label} composition checker: {error}")
        });
        assert!(
            output.status.success(),
            "Lean checker rejected {label} Rust composition trace: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn assert_commit_open_precedes_lift_shutdown(trace: &str) {
        let document: serde_json::Value =
            serde_json::from_str(trace).expect("composition trace must be valid JSON");
        let events = document["events"]
            .as_array()
            .expect("composition trace must contain an events array");
        let mut committed = false;
        for event in events {
            if event.get("beginOpen").is_some() {
                committed = false;
            }
            if event.get("liftShutdown").is_some() {
                assert!(
                    committed,
                    "LiftShutdown was recorded before CommitOpen: {trace}"
                );
            }
            if event.get("commitOpen").is_some() {
                committed = true;
            }
        }
    }

    struct RetryClose;

    struct RetryState {
        attempts: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    fn test_open_context() -> OpenContext {
        OpenContext::new(
            std::path::PathBuf::from("test.xll"),
            BuildInfo::new(AddinId::parse("test").unwrap(), "0", "test"),
            RuntimeGeneration::new(1).expect("test generation is non-zero"),
        )
    }

    fn lifecycle_access<A: Addin>(runtime: &Runtime<A>) -> AddinLifecycleAccess<'_, A> {
        runtime
            .bind_addin_lifecycle()
            .expect("test runs on the lifecycle thread")
    }

    static LAYERS_PANIC_CLOSES: AtomicUsize = AtomicUsize::new(0);
    static LAYERS_PANIC_QUIESCES: AtomicUsize = AtomicUsize::new(0);
    static LAYERS_PANIC_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct LayersPanic;

    impl Addin for LayersPanic {
        type SharedState = ();
        type LifecycleState = ();
        type Error = XllError;
        type Layers = ();

        fn open(
            _: &OpenContext,
        ) -> Result<
            crate::addin::Opened<Self::SharedState, Self::LifecycleState, Self::Layers>,
            Self::Error,
        > {
            Ok(crate::addin::Opened::new((), (), ()))
        }

        fn quiesce(
            _: &mut Self::SharedState,
            _: &mut Self::LifecycleState,
        ) -> Result<(), Self::Error> {
            LAYERS_PANIC_QUIESCES.fetch_add(1, Ordering::AcqRel);
            Ok(())
        }

        fn cleanup(_: &mut Self::LifecycleState, _: &mut crate::shutdown::CleanupReporter<'_>) {
            assert!(LAYERS_PANIC_QUIESCES.load(Ordering::Acquire) >= 1);
            LAYERS_PANIC_CLOSES.fetch_add(1, Ordering::AcqRel);
        }
    }

    #[test]
    fn xl_auto_close_on_closed_runtime_invalidates_a_pending_open_epoch() {
        let runtime = Runtime::<LayersPanic>::new();
        let stale_epoch = runtime.removal_epoch();

        assert_eq!(host_auto_remove::<LayersPanic>(&runtime), 1);
        assert!(
            runtime
                .lifecycle_runtime()
                .begin_open_if_epoch(stale_epoch)
                .is_err()
        );
        assert_eq!(runtime.phase(), crate::lifecycle::LifecyclePhase::Closed);
    }

    #[test]
    fn failed_concurrent_open_does_not_rollback_the_owner_attempt() {
        let runtime = Runtime::<LayersPanic>::new();
        let mut owner = runtime.lifecycle_runtime().begin_open().unwrap();
        let lifecycle = lifecycle_access(&runtime);

        rollback_active_open::<LayersPanic, crate::lifecycle::OpenAttemptBegun>(&lifecycle, None);
        assert_eq!(runtime.phase(), crate::lifecycle::LifecyclePhase::Opening);

        runtime.publish((), ());
        runtime.finish_open(&mut owner, Vec::new()).unwrap();
        assert_eq!(runtime.phase(), crate::lifecycle::LifecyclePhase::Open);
    }

    #[test]
    fn open_transaction_stages_state_and_layers_together() {
        let _test_guard = LAYERS_PANIC_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        LAYERS_PANIC_CLOSES.store(0, Ordering::Release);
        LAYERS_PANIC_QUIESCES.store(0, Ordering::Release);
        let runtime = Runtime::<LayersPanic>::new();
        let removal_epoch = runtime.removal_epoch();
        let transaction = runtime
            .lifecycle_runtime()
            .begin_open_if_epoch(removal_epoch)
            .unwrap()
            .attach_host();
        let lifecycle = lifecycle_access(&runtime);
        let (transaction, _) =
            match initialize_addin::<LayersPanic>(&test_open_context(), transaction) {
                Ok(result) => result,
                Err(failure) => {
                    let error = failure.rollback(&lifecycle);
                    panic!("unexpected add-in initialization failure: {error}");
                }
            };
        assert!(runtime.has_opening_generation());
        assert!(!runtime.has_current_generation());
        rollback_active_open(&lifecycle, Some(transaction));
        assert_eq!(LAYERS_PANIC_QUIESCES.load(Ordering::Acquire), 1);
        assert_eq!(LAYERS_PANIC_CLOSES.load(Ordering::Acquire), 1);
    }

    #[test]
    fn controlled_reload_reclaims_old_generation_before_new_open() {
        let _test_guard = LAYERS_PANIC_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        LAYERS_PANIC_CLOSES.store(0, Ordering::Release);
        LAYERS_PANIC_QUIESCES.store(0, Ordering::Release);
        let runtime = Runtime::<LayersPanic>::new();
        let mut first_open = runtime.lifecycle_runtime().begin_open().unwrap();
        runtime.publish((), ());
        runtime.finish_open(&mut first_open, Vec::new()).unwrap();
        let first_generation = runtime.last_committed_generation();

        let lifecycle = lifecycle_access(&runtime);
        assert_eq!(remove_addin::<LayersPanic>(&runtime, &lifecycle), 1);
        assert_eq!(runtime.phase(), crate::lifecycle::LifecyclePhase::Closed);
        runtime.lifecycle_runtime().clear_host_intent();
        let mut second_open = runtime.lifecycle_runtime().begin_open().unwrap();
        runtime.publish((), ());
        runtime.finish_open(&mut second_open, Vec::new()).unwrap();
        assert_eq!(runtime.phase(), crate::lifecycle::LifecyclePhase::Open);
        assert!(runtime.last_committed_generation() > first_generation);
        assert_eq!(LAYERS_PANIC_QUIESCES.load(Ordering::Acquire), 1);
        assert_eq!(host_auto_remove::<LayersPanic>(&runtime), 1);
        assert_eq!(runtime.phase(), crate::lifecycle::LifecyclePhase::Closed);
        assert_eq!(LAYERS_PANIC_QUIESCES.load(Ordering::Acquire), 2);
    }

    struct ReloadFailure;

    impl Addin for ReloadFailure {
        type SharedState = ();
        type LifecycleState = ();
        type Error = XllError;
        type Layers = ();

        fn open(
            _: &OpenContext,
        ) -> Result<
            crate::addin::Opened<Self::SharedState, Self::LifecycleState, Self::Layers>,
            Self::Error,
        > {
            Err(XllError::Internal {
                diagnostic_id: crate::diagnostics::id::DiagnosticId::OPEN_STATE,
            })
        }
    }

    #[test]
    fn failed_controlled_reload_quarantines_the_runtime() {
        let runtime = Runtime::<ReloadFailure>::new();
        let mut first_open = runtime.lifecycle_runtime().begin_open().unwrap();
        runtime.publish((), ());
        runtime.finish_open(&mut first_open, Vec::new()).unwrap();

        assert_eq!(
            host_auto_open::<ReloadFailure>(
                &runtime,
                &AddinId::parse("test").unwrap(),
                "0",
                "test",
                &[],
            ),
            0
        );
        assert_eq!(
            runtime.phase(),
            crate::lifecycle::LifecyclePhase::Quarantined
        );
        assert_eq!(host_auto_close::<ReloadFailure>(&runtime), 1);
        assert_eq!(
            runtime.phase(),
            crate::lifecycle::LifecyclePhase::Quarantined
        );
    }

    #[cfg(feature = "async")]
    #[test]
    fn async_worker_policy_is_bounded_before_open() {
        assert!(crate::addin::AsyncWorkerCount::new(0).is_none());
        assert!(crate::addin::AsyncWorkerCount::new(33).is_none());
        assert_eq!(crate::addin::AsyncWorkerCount::new(32).unwrap().get(), 32);
    }

    impl Addin for RetryClose {
        type SharedState = ();
        type LifecycleState = RetryState;
        type Error = XllError;
        type Layers = ();

        fn open(
            _context: &OpenContext,
        ) -> Result<
            crate::addin::Opened<Self::SharedState, Self::LifecycleState, Self::Layers>,
            Self::Error,
        > {
            unreachable!("the close retry test publishes state directly")
        }

        fn cleanup(
            state: &mut Self::LifecycleState,
            reporter: &mut crate::shutdown::CleanupReporter<'_>,
        ) {
            state
                .attempts
                .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
            reporter.warn(
                "test cleanup",
                crate::shutdown::CleanupIssueKind::RegistryCleanup,
                XllError::Internal {
                    diagnostic_id: crate::diagnostics::id::DiagnosticId::TEST_RETRY,
                },
            );
        }
    }

    #[test]
    fn addin_cleanup_issue_does_not_prevent_finalizing_runtime() {
        let runtime = Runtime::<RetryClose>::new();
        let attempts = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut open_attempt = runtime.lifecycle_runtime().begin_open().unwrap();
        runtime.publish_with_lifecycle(
            (),
            RetryState {
                attempts: std::sync::Arc::clone(&attempts),
            },
            (),
        );
        runtime.finish_open(&mut open_attempt, Vec::new()).unwrap();

        let lifecycle = lifecycle_access(&runtime);
        remove_addin_inner::<RetryClose>(&runtime, &lifecycle);
        assert_eq!(runtime.phase(), crate::lifecycle::LifecyclePhase::Closed);
        assert_eq!(attempts.load(std::sync::atomic::Ordering::Acquire), 1);
        assert!(runtime.take_current_generation().is_none() && !runtime.has_opening_generation());
    }

    struct CleanupPanic;

    struct DropObserved(std::sync::Arc<AtomicUsize>);

    impl Drop for DropObserved {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::AcqRel);
        }
    }

    impl Addin for CleanupPanic {
        type SharedState = ();
        type LifecycleState = DropObserved;
        type Error = XllError;
        type Layers = ();

        fn open(
            _: &OpenContext,
        ) -> Result<
            crate::addin::Opened<Self::SharedState, Self::LifecycleState, Self::Layers>,
            Self::Error,
        > {
            unreachable!()
        }

        fn cleanup(_: &mut Self::LifecycleState, _: &mut crate::shutdown::CleanupReporter<'_>) {
            panic!("injected cleanup panic");
        }
    }

    #[test]
    fn cleanup_panic_retains_state_and_quarantines_runtime() {
        let runtime = Runtime::<CleanupPanic>::new();
        let drops = std::sync::Arc::new(AtomicUsize::new(0));
        let mut opening = runtime.lifecycle_runtime().begin_open().unwrap();
        runtime.publish_with_lifecycle((), DropObserved(std::sync::Arc::clone(&drops)), ());
        let lifecycle = lifecycle_access(&runtime);
        assert!(runtime.with_addin_lifecycle(&lifecycle, |_| ()).is_ok());
        runtime.finish_open(&mut opening, Vec::new()).unwrap();

        remove_addin_inner::<CleanupPanic>(&runtime, &lifecycle);

        assert_eq!(
            runtime.phase(),
            crate::lifecycle::LifecyclePhase::Quarantined
        );
        assert_eq!(drops.load(Ordering::Acquire), 0);
        assert!(runtime.with_addin_lifecycle(&lifecycle, |_| ()).is_ok());
    }

    struct WrongThreadRemoval;

    impl Addin for WrongThreadRemoval {
        type SharedState = ();
        type LifecycleState = DropObserved;
        type Error = XllError;
        type Layers = ();

        fn open(
            _: &OpenContext,
        ) -> Result<
            crate::addin::Opened<Self::SharedState, Self::LifecycleState, Self::Layers>,
            Self::Error,
        > {
            unreachable!("the wrong-thread test publishes state directly")
        }

        fn quiesce(
            _: &mut Self::SharedState,
            _: &mut Self::LifecycleState,
        ) -> Result<(), Self::Error> {
            Ok(())
        }

        fn cleanup(_: &mut Self::LifecycleState, _: &mut crate::shutdown::CleanupReporter<'_>) {}
    }

    #[test]
    fn wrong_thread_removal_quarantines_before_touching_lifecycle_state() {
        let runtime = std::sync::Arc::new(Runtime::<WrongThreadRemoval>::new());
        let drops = std::sync::Arc::new(AtomicUsize::new(0));
        let mut opening = runtime.lifecycle_runtime().begin_open().unwrap();
        runtime.publish_with_lifecycle((), DropObserved(std::sync::Arc::clone(&drops)), ());
        runtime.finish_open(&mut opening, Vec::new()).unwrap();
        assert!(
            runtime
                .ensure_module_residency(lifecycle_residency_probe_anchor as *const ())
                .is_ok()
        );
        let lifecycle = lifecycle_access(&runtime);

        let removal_runtime = std::sync::Arc::clone(&runtime);
        std::thread::spawn(move || {
            assert_eq!(host_auto_remove::<WrongThreadRemoval>(&removal_runtime), 1);
        })
        .join()
        .expect("wrong-thread removal worker panicked");

        assert_eq!(
            runtime.phase(),
            crate::lifecycle::LifecyclePhase::Quarantined
        );
        assert!(runtime.module_residency_held());
        assert_eq!(drops.load(Ordering::Acquire), 0);
        assert!(runtime.with_addin_lifecycle(&lifecycle, |_| ()).is_ok());
    }

    struct QuiesceFailure;

    impl Addin for QuiesceFailure {
        type SharedState = ();
        type LifecycleState = DropObserved;
        type Error = XllError;
        type Layers = ();

        fn open(
            _: &OpenContext,
        ) -> Result<
            crate::addin::Opened<Self::SharedState, Self::LifecycleState, Self::Layers>,
            Self::Error,
        > {
            unreachable!()
        }

        fn quiesce(
            _: &mut Self::SharedState,
            _: &mut Self::LifecycleState,
        ) -> Result<(), Self::Error> {
            Err(XllError::Internal {
                diagnostic_id: crate::diagnostics::id::DiagnosticId::QUIESCENCE_FAILURE,
            })
        }
    }

    #[test]
    fn quiesce_failure_enters_quarantine_without_dropping_state() {
        let runtime = Runtime::<QuiesceFailure>::new();
        let drops = std::sync::Arc::new(AtomicUsize::new(0));
        let mut opening = runtime.lifecycle_runtime().begin_open().unwrap();
        runtime.publish_with_lifecycle((), DropObserved(std::sync::Arc::clone(&drops)), ());
        let lifecycle = lifecycle_access(&runtime);
        assert!(runtime.with_addin_lifecycle(&lifecycle, |_| ()).is_ok());
        runtime.finish_open(&mut opening, Vec::new()).unwrap();

        let result = { remove_addin_inner::<QuiesceFailure>(&runtime, &lifecycle) };

        assert!(matches!(result, RemovalSuccess::Quarantined));
        assert_eq!(
            runtime.phase(),
            crate::lifecycle::LifecyclePhase::Quarantined
        );
        assert_eq!(drops.load(Ordering::Acquire), 0);
        assert_eq!(host_auto_close::<QuiesceFailure>(&runtime), 1);
        assert_eq!(
            runtime.phase(),
            crate::lifecycle::LifecyclePhase::Quarantined
        );
    }

    #[test]
    fn open_rollback_cleanup_issue_still_finalizes_without_reinstalling_state() {
        let runtime = Runtime::<RetryClose>::new();
        let attempts = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut open_attempt = runtime.lifecycle_runtime().begin_open().unwrap();
        runtime.publish_with_lifecycle(
            (),
            RetryState {
                attempts: std::sync::Arc::clone(&attempts),
            },
            (),
        );

        assert!(open_attempt.fail().requires_rollback());
        let mut callbacks = HostCallbackSession::new();
        let lifecycle = lifecycle_access(&runtime);
        let outcome = rollback_open::<RetryClose>(
            &runtime,
            &lifecycle,
            &mut callbacks,
            active_runtime_generation(&runtime),
        );
        assert!(outcome.unload_safe());
        assert!(outcome.is_finalized());
        assert_eq!(runtime.phase(), crate::lifecycle::LifecyclePhase::Closed);
        assert_eq!(attempts.load(std::sync::atomic::Ordering::Acquire), 1);
        assert!(runtime.take_current_generation().is_none() && !runtime.has_opening_generation());
    }

    struct CleanClose;

    impl Addin for CleanClose {
        type SharedState = ();
        type LifecycleState = ();
        type Error = XllError;
        type Layers = ();

        fn open(
            _context: &OpenContext,
        ) -> Result<
            crate::addin::Opened<Self::SharedState, Self::LifecycleState, Self::Layers>,
            Self::Error,
        > {
            unreachable!()
        }
    }

    // SAFETY: this test Addin has no application-owned executable sources.
    unsafe impl crate::addin::PhysicallyUnloadableAddin for CleanClose {}

    struct TraceCleanup;

    impl Addin for TraceCleanup {
        type SharedState = ();
        type LifecycleState = ();
        type Error = XllError;
        type Layers = ();

        fn open(
            _context: &OpenContext,
        ) -> Result<
            crate::addin::Opened<Self::SharedState, Self::LifecycleState, Self::Layers>,
            Self::Error,
        > {
            unreachable!()
        }

        fn cleanup(
            _state: &mut Self::LifecycleState,
            reporter: &mut crate::shutdown::CleanupReporter<'_>,
        ) {
            reporter.warn(
                "Lean checker cleanup trace",
                crate::shutdown::CleanupIssueKind::RegistryCleanup,
                XllError::Internal {
                    diagnostic_id: crate::diagnostics::id::DiagnosticId::LEAN_TRACE,
                },
            );
        }
    }

    struct TraceHandle;

    impl crate::handle::ExcelHandleObject for TraceHandle {}

    struct TraceSubscription;

    impl crate::subscription::RtdSubscription for TraceSubscription {
        fn cancellation(&self) -> std::sync::Arc<dyn crate::subscription::RtdCancellation> {
            std::sync::Arc::new(crate::subscription::RtdCancellationHandle::noop())
        }

        fn disconnect_and_wait(self: Box<Self>) -> XllResult<()> {
            Ok(())
        }
    }

    struct TraceSource {
        sink: std::sync::Arc<std::sync::Mutex<Option<crate::subscription::RtdSink<f64>>>>,
    }

    impl crate::subscription::RtdSource for TraceSource {
        type Value = f64;
        type Subscription = TraceSubscription;

        fn subscribe(
            &self,
            _topic: &crate::subscription::RtdTopic,
            sink: crate::subscription::RtdSink<Self::Value>,
        ) -> XllResult<Self::Subscription> {
            self.sink.lock().unwrap().replace(sink);
            Ok(TraceSubscription)
        }
    }

    struct TraceDiagnosticSink;

    impl crate::diagnostics::DiagnosticSink for TraceDiagnosticSink {
        fn report(&self, _event: &crate::diagnostics::DiagnosticEvent<'_>) {}
    }

    #[test]
    #[ignore = "requires XLFN_SHUTDOWN_CHECKER to point to the Lean executable"]
    fn rust_shutdown_resource_traces_are_accepted_by_lean_checker() {
        use std::io::Write;
        use std::process::{Command, Stdio};

        let checker = std::env::var_os("XLFN_SHUTDOWN_CHECKER")
            .expect("XLFN_SHUTDOWN_CHECKER must point to shutdown_trace_checker");

        let check = |label: &str, trace: String| {
            let mut child = Command::new(&checker)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap_or_else(|error| {
                    panic!("failed to start shutdown_trace_checker for {label}: {error}")
                });
            child
                .stdin
                .take()
                .expect("checker stdin is piped")
                .write_all(trace.as_bytes())
                .unwrap_or_else(|error| panic!("failed to write {label} shutdown trace: {error}"));
            let output = child.wait_with_output().unwrap_or_else(|error| {
                panic!("failed to wait for {label} shutdown trace: {error}")
            });
            assert!(
                output.status.success(),
                "Lean checker rejected {label} Rust trace: {}\ntrace:\n{trace}",
                String::from_utf8_lossy(&output.stderr)
            );
        };

        let static_fixture = crate::runtime::StaticTestRuntime::<TraceCleanup>::new();
        let runtime = static_fixture.runtime();
        let mut opening = runtime.lifecycle_runtime().begin_open().unwrap();
        runtime.publish((), ());
        runtime.finish_open(&mut opening, Vec::new()).unwrap();

        crate::diagnostics::reset_diagnostic_router().unwrap();
        crate::diagnostics::set_diagnostic_sink(TraceDiagnosticSink).unwrap();
        crate::diagnostics::report_no_unwind("lean_checker_trace", &XllError::Panic);

        let pointer = crate::return_value::ffi_boundary(runtime, || Ok::<f64, XllError>(1.0));
        // SAFETY: `pointer` is the live DLL-owned block returned by the
        // framework boundary above and is freed exactly once here.
        let free = unsafe { crate::return_value::free_return_boundary(pointer) };
        drop(free);

        let handles = runtime.formula_handle_service().unwrap();
        handles
            .prepare(crate::handle::test_topic_key("lean-checker-handle"), || {
                Ok(TraceHandle)
            })
            .unwrap();

        let notifier_state =
            std::sync::Arc::new(crate::rtd::test_support::TestNotifierState::new());
        let subscriptions = runtime.subscriptions().unwrap();
        let subscriptions = subscriptions.as_arc();
        let server = subscriptions
            .register_server(
                crate::subscription::ServerGeneration::new(1)
                    .expect("non-zero test server generation"),
            )
            .unwrap();
        server
            .attach_update_notifier(crate::rtd::RtdNotifier::for_test(std::sync::Arc::clone(
                &notifier_state,
            )))
            .unwrap();
        let trace_sink = std::sync::Arc::new(std::sync::Mutex::new(None));
        let source = crate::subscription::RtdSourceHandle::for_internal(
            runtime
                .last_committed_generation()
                .expect("test runtime has a generation"),
            TraceSource {
                sink: std::sync::Arc::clone(&trace_sink),
            },
        )
        .unwrap();
        let prepared = subscriptions
            .prepare(
                &source,
                crate::subscription::RtdTopic::single("lean-checker-subscription").unwrap(),
            )
            .unwrap();
        let key = *prepared.key();
        prepared.commit();
        let conn = subscriptions
            .connect_transaction(&server, crate::subscription::TopicId(1), &key)
            .unwrap();
        conn.commit().unwrap();
        trace_sink
            .lock()
            .unwrap()
            .as_ref()
            .expect("trace source must retain its RTD sink")
            .publish(1.0)
            .unwrap();
        assert_eq!(notifier_state.calls.load(Ordering::Acquire), 1);

        #[cfg(feature = "async")]
        {
            let (done_tx, done_rx) = std::sync::mpsc::channel();
            let (cancellation, _token) = crate::cancellation::CancellationSource::new(
                crate::cancellation::CancellationGuarantee::BestEffort,
            );
            runtime.start_async(1).unwrap();
            let ingress = crate::module_runtime::ingress()
                .enter_with(|| {})
                .into_admitted()
                .expect("test call enters during OPEN");
            let call = runtime
                .enter(&ingress)
                .expect("async trace task must be spawned from an admitted call");
            runtime
                .async_manager()
                .spawn(
                    runtime
                        .last_committed_generation()
                        .expect("an open runtime has a published generation")
                        .get(),
                    async move {
                        done_tx.send(()).unwrap();
                    },
                    cancellation,
                )
                .unwrap();
            drop(call);
            done_rx
                .recv_timeout(std::time::Duration::from_secs(1))
                .expect("Lean checker async trace task did not complete");
        }

        assert_eq!(host_auto_remove::<TraceCleanup>(runtime), 1);
        let trace = runtime.shutdown_trace_json();
        for event in [
            "enterExternal",
            "leaveExternal",
            "enterCall",
            "leaveCall",
            "createReturnBlock",
            "beginReturnFree",
            "releaseReturnBlock",
            "endReturnFree",
            "addHandle",
            "removeHandle",
            "beginRtdOperation",
            "endRtdOperation",
            "addSubscription",
            "removeSubscription",
            "beginCallback",
            "endCallback",
            "startDiagnostics",
            "enqueueDiagnostic",
            "flushDiagnostic",
            "recordCleanupIssue",
        ] {
            assert!(
                trace.contains(event),
                "resource trace is missing {event}: {trace}"
            );
        }
        #[cfg(feature = "async")]
        for event in [
            "startAsyncExecutor",
            "startAsyncTask",
            "endAsyncTask",
            "stopAsyncExecutor",
        ] {
            assert!(
                trace.contains(event),
                "resource trace is missing {event}: {trace}"
            );
        }
        check("resourceful", trace);

        crate::diagnostics::reset_diagnostic_router().unwrap();
        let clean_runtime = Runtime::<CleanClose>::new();
        let mut opening = clean_runtime.lifecycle_runtime().begin_open().unwrap();
        clean_runtime.publish((), ());
        clean_runtime.finish_open(&mut opening, Vec::new()).unwrap();
        assert_eq!(host_auto_remove::<CleanClose>(&clean_runtime), 1);
        check("clean", clean_runtime.shutdown_trace_json());

        let failure_runtime = Runtime::<QuiesceFailure>::new();
        let drops = std::sync::Arc::new(AtomicUsize::new(0));
        let mut opening = failure_runtime.lifecycle_runtime().begin_open().unwrap();
        failure_runtime.publish_with_lifecycle((), DropObserved(std::sync::Arc::clone(&drops)), ());
        failure_runtime
            .finish_open(&mut opening, Vec::new())
            .unwrap();
        assert_eq!(host_auto_remove::<QuiesceFailure>(&failure_runtime), 1);
        assert_eq!(
            failure_runtime.phase(),
            crate::lifecycle::LifecyclePhase::Quarantined
        );
        let failure_trace = failure_runtime.shutdown_trace_json();
        assert!(failure_trace.contains("quarantined"));
        check("quarantine", failure_trace);
    }

    #[test]
    #[ignore = "requires XLFN_COMPOSITION_CHECKER to point to the Lean executable"]
    fn rust_composition_traces_are_accepted_by_lean_checker() {
        use std::io::Write;
        use std::process::{Command, Stdio};

        let _test_guard = COMPOSITION_TRACE_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let checker = std::env::var_os("XLFN_COMPOSITION_CHECKER")
            .expect("XLFN_COMPOSITION_CHECKER must point to composition_trace_checker");
        let runtime = Runtime::<CleanClose>::new_with_physical_unload();
        let mut opening = runtime.lifecycle_runtime().begin_open().unwrap();
        runtime.publish((), ());
        runtime.finish_open(&mut opening, Vec::new()).unwrap();
        crate::diagnostics::reset_diagnostic_router().unwrap();
        crate::diagnostics::set_diagnostic_sink(TraceDiagnosticSink).unwrap();
        crate::diagnostics::report_no_unwind("composition_checker_trace", &XllError::Panic);

        assert_eq!(host_auto_remove::<CleanClose>(&runtime), 1);
        let trace = runtime.composition_trace_json();
        if let Some(path) = std::env::var_os("XLFN_COMPOSITION_TRACE") {
            std::fs::write(path, &trace).expect("write Rust composition trace");
        }
        let mut child = Command::new(&checker)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("failed to start composition_trace_checker");
        child
            .stdin
            .take()
            .expect("composition checker stdin is piped")
            .write_all(trace.as_bytes())
            .expect("failed to write composition trace");
        let output = child
            .wait_with_output()
            .expect("failed to wait for composition_trace_checker");
        assert!(
            output.status.success(),
            "Lean checker rejected Rust composition trace: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn composition_commit_open_precedes_lift_shutdown_events() {
        let _runtime_test_guard = crate::runtime::tests::TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let _test_guard = COMPOSITION_TRACE_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let runtime = Runtime::<CleanClose>::new_with_physical_unload();
        let mut opening = runtime.lifecycle_runtime().begin_open().unwrap();
        runtime.publish((), ());
        runtime.finish_open(&mut opening, Vec::new()).unwrap();
        let activity_id = runtime.refinement_hooks().next_activity_id();
        runtime
            .refinement_hooks()
            .call_entered(&runtime, activity_id);
        runtime.refinement_hooks().call_left(&runtime, activity_id);

        assert_eq!(host_auto_remove::<CleanClose>(&runtime), 1);
        assert_commit_open_precedes_lift_shutdown(&runtime.composition_trace_json());
    }

    #[test]
    #[ignore = "requires XLFN_COMPOSITION_CHECKER to point to the Lean executable"]
    fn rust_composition_already_closed_trace_is_accepted_by_lean_checker() {
        let _test_guard = COMPOSITION_TRACE_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let checker = std::env::var_os("XLFN_COMPOSITION_CHECKER")
            .expect("XLFN_COMPOSITION_CHECKER must point to composition_trace_checker");
        let runtime = Runtime::<CleanClose>::new();

        assert_eq!(host_auto_remove::<CleanClose>(&runtime), 1);
        check_composition_trace_with_lean(
            &checker,
            "already closed",
            &runtime.composition_trace_json(),
            "rust-composition-already-closed-trace.json",
        );
    }

    #[test]
    #[ignore = "requires XLFN_COMPOSITION_CHECKER to point to the Lean executable"]
    fn rust_composition_reopen_trace_is_accepted_by_lean_checker() {
        let _test_guard = COMPOSITION_TRACE_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let checker = std::env::var_os("XLFN_COMPOSITION_CHECKER")
            .expect("XLFN_COMPOSITION_CHECKER must point to composition_trace_checker");
        let runtime = Runtime::<CleanClose>::new();

        for label in ["first", "second"] {
            crate::diagnostics::reset_diagnostic_router().unwrap();
            let mut opening = runtime.lifecycle_runtime().begin_open().unwrap();
            runtime.publish((), ());
            runtime.finish_open(&mut opening, Vec::new()).unwrap();
            crate::diagnostics::set_diagnostic_sink(TraceDiagnosticSink).unwrap();
            crate::diagnostics::report_no_unwind(label, &XllError::Panic);
            assert_eq!(host_auto_remove::<CleanClose>(&runtime), 1);
        }

        let trace = runtime.composition_trace_json();
        assert_commit_open_precedes_lift_shutdown(&trace);
        check_composition_trace_with_lean(
            &checker,
            "reopen",
            &trace,
            "rust-composition-reopen-trace.json",
        );
    }

    #[test]
    #[ignore = "requires XLFN_COMPOSITION_CHECKER to point to the Lean executable"]
    fn rust_composition_owner_takeover_trace_is_accepted_by_lean_checker() {
        use std::io::Write;
        use std::process::{Command, Stdio};

        let _test_guard = COMPOSITION_TRACE_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let checker = std::env::var_os("XLFN_COMPOSITION_CHECKER")
            .expect("XLFN_COMPOSITION_CHECKER must point to composition_trace_checker");
        let runtime = Runtime::<CleanClose>::new();
        let mut opening = runtime.lifecycle_runtime().begin_open().unwrap();
        runtime.publish((), ());
        runtime.finish_open(&mut opening, Vec::new()).unwrap();
        crate::diagnostics::reset_diagnostic_router().unwrap();
        crate::diagnostics::set_diagnostic_sink(TraceDiagnosticSink).unwrap();
        crate::diagnostics::report_no_unwind("composition_takeover_trace", &XllError::Panic);

        let first = runtime.lifecycle_runtime().begin_final_removal().unwrap();
        drop(first);
        let second = runtime.lifecycle_runtime().begin_final_removal().unwrap();
        drop(second);

        assert_eq!(host_auto_remove::<CleanClose>(&runtime), 1);
        let trace = runtime.composition_trace_json();
        if let Some(path) = std::env::var_os("XLFN_COMPOSITION_TAKEOVER_TRACE") {
            std::fs::write(path, &trace).expect("write Rust composition takeover trace");
        }
        let mut child = Command::new(&checker)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("failed to start composition_trace_checker");
        child
            .stdin
            .take()
            .expect("composition checker stdin is piped")
            .write_all(trace.as_bytes())
            .expect("failed to write composition takeover trace");
        let output = child
            .wait_with_output()
            .expect("failed to wait for composition_trace_checker");
        assert!(
            output.status.success(),
            "Lean checker rejected Rust composition takeover trace:\n\
             stdout:\n{}\n\
             stderr:\n{}\n\
             trace:\n{trace}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    #[ignore = "requires XLFN_COMPOSITION_CHECKER to point to the Lean executable"]
    fn rust_composition_uncommitted_and_rollback_traces_are_accepted_by_lean_checker() {
        use std::io::Write;
        use std::process::{Command, Stdio};

        let _test_guard = COMPOSITION_TRACE_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let checker = std::env::var_os("XLFN_COMPOSITION_CHECKER")
            .expect("XLFN_COMPOSITION_CHECKER must point to composition_trace_checker");
        let check = |label: &str, trace: String, path_name: &str| {
            if let Some(directory) = std::env::var_os("XLFN_COMPOSITION_TRACE_DIR") {
                let path = std::path::Path::new(&directory).join(path_name);
                std::fs::write(path, &trace).expect("write Rust composition path trace");
            }
            let mut child = Command::new(&checker)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap_or_else(|error| {
                    panic!("failed to start composition checker for {label}: {error}")
                });
            child
                .stdin
                .take()
                .expect("composition checker stdin is piped")
                .write_all(trace.as_bytes())
                .unwrap_or_else(|error| {
                    panic!("failed to write {label} composition trace: {error}")
                });
            let output = child.wait_with_output().unwrap_or_else(|error| {
                panic!("failed to wait for {label} composition checker: {error}")
            });
            assert!(
                output.status.success(),
                "Lean checker rejected {label} Rust composition trace:\n\
                 stdout:\n{}\n\
                 stderr:\n{}\n\
                 trace:\n{trace}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        };

        let uncommitted = std::sync::Arc::new(Runtime::<CleanClose>::new());
        let mut opening = uncommitted.lifecycle_runtime().begin_open().unwrap();
        let closing_runtime = std::sync::Arc::clone(&uncommitted);
        let (owner_tx, owner_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let close_waiter = std::thread::spawn(move || {
            let removal_attempt = closing_runtime
                .lifecycle_runtime()
                .begin_final_removal()
                .expect("final close must acquire after open rejection");
            owner_tx.send(()).expect("final close owner signal");
            release_rx.recv().expect("final close release signal");
            drop(removal_attempt);
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        while uncommitted.phase() != crate::lifecycle::LifecyclePhase::Closing
            && std::time::Instant::now() < deadline
        {
            std::thread::yield_now();
        }
        assert_eq!(
            uncommitted.phase(),
            crate::lifecycle::LifecyclePhase::Closing
        );
        assert!(uncommitted.finish_open(&mut opening, Vec::new()).is_err());
        owner_rx.recv().expect("final close owner was not acquired");
        release_tx.send(()).expect("final close release signal");
        close_waiter.join().expect("final close waiter panicked");
        assert_eq!(host_auto_remove::<CleanClose>(&uncommitted), 1);
        check(
            "uncommitted final close",
            uncommitted.composition_trace_json(),
            "rust-composition-uncommitted-trace.json",
        );

        let rollback = Runtime::<CleanClose>::new();
        let mut opening = rollback.lifecycle_runtime().begin_open().unwrap();
        assert!(opening.fail().requires_rollback());
        let mut callbacks = HostCallbackSession::new();
        let lifecycle = lifecycle_access(&rollback);
        let outcome = rollback_open::<CleanClose>(
            &rollback,
            &lifecycle,
            &mut callbacks,
            active_runtime_generation(&rollback),
        );
        assert!(outcome.unload_safe());
        check(
            "open rollback",
            rollback.composition_trace_json(),
            "rust-composition-open-rollback-trace.json",
        );
    }

    #[test]
    fn close_owner_is_held_until_the_success_boundary_finishes() {
        let runtime = Runtime::<CleanClose>::new();
        let mut opening = runtime.lifecycle_runtime().begin_open().unwrap();
        runtime.publish((), ());
        runtime.finish_open(&mut opening, Vec::new()).unwrap();

        let lifecycle = lifecycle_access(&runtime);
        let success = remove_addin_inner::<CleanClose>(&runtime, &lifecycle);
        assert!(runtime.lifecycle_runtime().begin_open().is_err());
        let RemovalSuccess::Closed {
            witness,
            removal_attempt,
        } = success
        else {
            panic!("test close must own the close attempt");
        };
        runtime.record_returned_success(witness).unwrap();
        drop(removal_attempt);

        let mut reopened = runtime.lifecycle_runtime().begin_open().unwrap();
        runtime.publish((), ());
        runtime.finish_open(&mut reopened, Vec::new()).unwrap();
        assert_eq!(runtime.phase(), crate::lifecycle::LifecyclePhase::Open);
    }

    struct AlwaysFailClose;

    impl Addin for AlwaysFailClose {
        type SharedState = ();
        type LifecycleState = ();
        type Error = XllError;
        type Layers = ();

        fn open(
            _context: &OpenContext,
        ) -> Result<
            crate::addin::Opened<Self::SharedState, Self::LifecycleState, Self::Layers>,
            Self::Error,
        > {
            unreachable!()
        }

        fn cleanup(
            _state: &mut Self::LifecycleState,
            reporter: &mut crate::shutdown::CleanupReporter<'_>,
        ) {
            reporter.warn(
                "always fail cleanup",
                crate::shutdown::CleanupIssueKind::RegistryCleanup,
                XllError::Internal {
                    diagnostic_id: crate::diagnostics::id::DiagnosticId::FAILURE,
                },
            );
        }
    }

    #[test]
    fn failing_open_rollback_is_finalized_by_xl_auto_close() {
        let runtime = Runtime::<AlwaysFailClose>::new();
        let mut open_attempt = runtime.lifecycle_runtime().begin_open().unwrap();
        runtime.publish((), ());

        assert!(open_attempt.fail().requires_rollback());
        let mut callbacks = HostCallbackSession::new();
        let lifecycle = lifecycle_access(&runtime);
        assert!(
            rollback_open::<AlwaysFailClose>(
                &runtime,
                &lifecycle,
                &mut callbacks,
                active_runtime_generation(&runtime),
            )
            .unload_safe()
        );
        assert_eq!(runtime.phase(), crate::lifecycle::LifecyclePhase::Closed);

        assert_eq!(host_auto_remove::<AlwaysFailClose>(&runtime), 1);
        assert_eq!(runtime.phase(), crate::lifecycle::LifecyclePhase::Closed);
    }

    #[test]
    fn xl_auto_close_waits_for_active_call_and_returns_one_after_clean_close() {
        let fixture = crate::runtime::StaticTestRuntime::<CleanClose>::new();
        let runtime = fixture.runtime();
        let mut open_attempt = runtime.lifecycle_runtime().begin_open().unwrap();
        runtime.publish((), ());
        runtime.finish_open(&mut open_attempt, Vec::new()).unwrap();
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let holder = std::thread::spawn(move || {
            let ingress = crate::module_runtime::ingress()
                .enter_udf_with(|| {})
                .into_admitted()
                .expect("test call enters during OPEN");
            let call = runtime.enter(&ingress).unwrap();
            entered_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            drop(call);
            drop(ingress);
        });

        entered_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        let releaser = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(20));
            release_tx.send(()).unwrap();
        });
        let started = std::time::Instant::now();
        assert_eq!(host_auto_remove::<CleanClose>(runtime), 1);
        assert!(started.elapsed() >= std::time::Duration::from_millis(20));
        releaser.join().unwrap();
        holder.join().unwrap();
        assert_eq!(runtime.phase(), crate::lifecycle::LifecyclePhase::Closed);
    }

    #[test]
    fn xl_auto_close_is_a_hint_until_explicit_removal() {
        let runtime = Runtime::<CleanClose>::new();
        let mut open_attempt = runtime.lifecycle_runtime().begin_open().unwrap();
        runtime.publish((), ());
        runtime.finish_open(&mut open_attempt, Vec::new()).unwrap();

        assert_eq!(host_auto_close::<CleanClose>(&runtime), 1);
        assert_eq!(runtime.phase(), crate::lifecycle::LifecyclePhase::Open);
        let ingress = crate::module_runtime::ingress()
            .enter_with(|| {})
            .into_admitted()
            .expect("test call enters during OPEN");
        assert!(runtime.enter(&ingress).is_ok());
        drop(ingress);

        assert_eq!(host_auto_remove::<CleanClose>(&runtime), 1);
        assert_eq!(runtime.phase(), crate::lifecycle::LifecyclePhase::Closed);
    }

    #[inline(never)]
    fn lifecycle_residency_probe_anchor() {}

    #[test]
    fn residency_release_requires_removal_then_close_hint() {
        let runtime = Runtime::<CleanClose>::new_with_physical_unload();
        assert!(
            runtime
                .ensure_module_residency(lifecycle_residency_probe_anchor as *const ())
                .is_ok()
        );
        let mut open_attempt = runtime.lifecycle_runtime().begin_open().unwrap();
        runtime.publish((), ());
        runtime.finish_open(&mut open_attempt, Vec::new()).unwrap();

        let _ = host_auto_close::<CleanClose>(&runtime);
        assert!(runtime.module_residency_held());
        assert_eq!(host_auto_remove::<CleanClose>(&runtime), 1);
        assert!(runtime.module_residency_held());
        let _ = host_auto_close::<CleanClose>(&runtime);
        assert!(!runtime.module_residency_held());
    }

    #[test]
    fn failed_open_after_removal_preserves_the_later_release_marker() {
        let runtime = Runtime::<CleanClose>::new_with_physical_unload();
        assert!(
            runtime
                .ensure_module_residency(lifecycle_residency_probe_anchor as *const ())
                .is_ok()
        );
        let mut open_attempt = runtime.lifecycle_runtime().begin_open().unwrap();
        runtime.publish((), ());
        runtime.finish_open(&mut open_attempt, Vec::new()).unwrap();
        assert_eq!(host_auto_remove::<CleanClose>(&runtime), 1);

        assert_eq!(
            host_auto_open::<CleanClose>(
                &runtime,
                &AddinId::parse("test").unwrap(),
                "0",
                "test",
                &[],
            ),
            0
        );
        assert_eq!(runtime.phase(), crate::lifecycle::LifecyclePhase::Closed);
        assert_eq!(
            runtime.host_intent(),
            HostLifecycleIntent::ExplicitRemovalComplete
        );
        let _ = host_auto_close::<CleanClose>(&runtime);
        assert!(!runtime.module_residency_held());
    }

    #[cfg(all(feature = "async", not(target_os = "windows")))]
    #[test]
    fn close_stops_async_before_terminal_unregister_and_never_calls_excel_afterwards() {
        use xlfn_sys::{
            XL_ASYNC_RETURN, XL_FREE, XLF_UNREGISTER, XLOPER12, XLOPER12BigData,
            XLOPER12BigDataHandle, XLOPER12Value, XLRET_ABORT, XLTYPE_BIG_DATA,
        };

        let runtime = Box::leak(Box::new(Runtime::<CleanClose>::new()));
        let mut open_attempt = runtime.lifecycle_runtime().begin_open().unwrap();
        runtime.publish((), ());
        runtime.finish_open(&mut open_attempt, Vec::new()).unwrap();
        runtime.start_async(1).unwrap();
        let mut journal = crate::registration::HostMutationJournal::default();
        journal.pending_registrations.push(
            crate::registration::RegistrationId {
                id: 1.0,
                excel_name: "TEST.CLOSE.ORDER",
            }
            .into(),
        );
        runtime.retain_host_mutations(journal);

        let _callback_guard = crate::test_callback::lock();
        crate::test_callback::install();
        crate::test_callback::reset();

        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let mut bytes = vec![1_u8, 2, 3, 4];
        let mut handle = XLOPER12 {
            value: XLOPER12Value {
                big_data: XLOPER12BigData {
                    handle: XLOPER12BigDataHandle {
                        data: bytes.as_mut_ptr(),
                    },
                    byte_count: bytes.len() as i32,
                },
            },
            xltype: XLTYPE_BIG_DATA,
        };
        // SAFETY: `handle` is a valid, stack-local XLOPER12 constructed above.
        unsafe {
            crate::async_udf::async_udf_boundary_named(
                runtime,
                "test_async_close_order",
                "TEST.ASYNC.CLOSE.ORDER",
                &mut handle,
                move |_, _, _| {
                    Ok(async move {
                        started_tx.send(()).unwrap();
                        std::future::pending::<()>().await;
                        Ok::<_, XllError>(123.0)
                    })
                },
            );
        }
        started_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("async close-order task did not start");

        crate::test_callback::set_terminal(XLF_UNREGISTER, XLRET_ABORT);
        let lifecycle = lifecycle_access(runtime);
        let close = remove_addin_inner::<CleanClose>(runtime, &lifecycle);

        assert!(matches!(close, RemovalSuccess::Quarantined));
        assert_eq!(
            runtime.phase(),
            crate::lifecycle::LifecyclePhase::Quarantined
        );
        assert_eq!(
            crate::test_callback::callback_order(),
            vec![XL_ASYNC_RETURN, XLF_UNREGISTER, XL_FREE]
        );
        assert_eq!(crate::test_callback::async_return_calls(), 1);
        assert_eq!(crate::test_callback::total_calls(), 3);
        // The test intentionally stops at the quarantine boundary after a
        // terminal unregister. Restore the RTD test epoch for later cases;
        // ingress is already sealed and must remain sealed until the next
        // Runtime::begin_open.
        crate::module_runtime::global()
            .rtd()
            .expect("RTD test state")
            .begin_open();
        runtime.release_test_module_lease();
    }

    struct OrderedClose;

    struct OrderedState {
        events: std::sync::Arc<std::sync::Mutex<Vec<&'static str>>>,
    }

    struct OrderedHandle {
        events: std::sync::Arc<std::sync::Mutex<Vec<&'static str>>>,
    }

    struct OrderedSubscription {
        events: std::sync::Arc<std::sync::Mutex<Vec<&'static str>>>,
    }

    impl crate::subscription::RtdSubscription for OrderedSubscription {
        fn cancellation(&self) -> std::sync::Arc<dyn crate::subscription::RtdCancellation> {
            std::sync::Arc::new(crate::subscription::RtdCancellationHandle::noop())
        }

        fn disconnect_and_wait(self: Box<Self>) -> XllResult<()> {
            self.events.lock().unwrap().push("subscription");
            Ok(())
        }
    }

    struct OrderedSource {
        events: std::sync::Arc<std::sync::Mutex<Vec<&'static str>>>,
    }

    impl crate::subscription::RtdSource for OrderedSource {
        type Value = f64;
        type Subscription = OrderedSubscription;

        fn subscribe(
            &self,
            _topic: &crate::subscription::RtdTopic,
            _sink: crate::subscription::RtdSink<Self::Value>,
        ) -> XllResult<Self::Subscription> {
            Ok(OrderedSubscription {
                events: std::sync::Arc::clone(&self.events),
            })
        }
    }

    impl Drop for OrderedHandle {
        fn drop(&mut self) {
            self.events.lock().unwrap().push("handle");
        }
    }

    impl crate::handle::ExcelHandleObject for OrderedHandle {}

    impl Addin for OrderedClose {
        type SharedState = ();
        type LifecycleState = OrderedState;
        type Error = XllError;
        type Layers = ();

        fn open(
            _context: &OpenContext,
        ) -> Result<
            crate::addin::Opened<Self::SharedState, Self::LifecycleState, Self::Layers>,
            Self::Error,
        > {
            unreachable!()
        }

        fn cleanup(state: &mut Self::LifecycleState, _: &mut crate::shutdown::CleanupReporter<'_>) {
            state.events.lock().unwrap().push("state");
        }
    }

    #[test]
    fn runtime_owned_subscriptions_and_handles_drop_before_addin_state_closes() {
        let runtime = Runtime::<OrderedClose>::new();
        let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut open_attempt = runtime.lifecycle_runtime().begin_open().unwrap();
        runtime.publish_with_lifecycle(
            (),
            OrderedState {
                events: std::sync::Arc::clone(&events),
            },
            (),
        );
        runtime.finish_open(&mut open_attempt, Vec::new()).unwrap();
        runtime
            .formula_handle_service()
            .unwrap()
            .prepare(crate::handle::test_topic_key("ordered"), || {
                Ok(OrderedHandle {
                    events: std::sync::Arc::clone(&events),
                })
            })
            .unwrap();
        let subscriptions = runtime.subscriptions().unwrap();
        let subscriptions = subscriptions.as_arc();
        let server = subscriptions
            .register_server(
                crate::subscription::ServerGeneration::new(1)
                    .expect("non-zero test server generation"),
            )
            .unwrap();
        let source = crate::subscription::RtdSourceHandle::for_internal(
            runtime
                .last_committed_generation()
                .expect("test runtime has a generation"),
            OrderedSource {
                events: std::sync::Arc::clone(&events),
            },
        )
        .unwrap();
        let prepared = subscriptions
            .prepare(
                &source,
                crate::subscription::RtdTopic::single("ordered").unwrap(),
            )
            .unwrap();
        let key = *prepared.key();
        prepared.commit();
        let conn = subscriptions
            .connect_transaction(&server, crate::subscription::TopicId(1), &key)
            .unwrap();
        conn.commit().unwrap();
        drop(server);

        assert_eq!(host_auto_remove::<OrderedClose>(&runtime), 1);
        assert_eq!(*events.lock().unwrap(), ["subscription", "handle", "state"]);
    }

    struct StagedRaceState {
        quiesced: std::sync::Arc<AtomicUsize>,
        cleaned: std::sync::Arc<AtomicUsize>,
        dropped: std::sync::Arc<AtomicUsize>,
        quiesce_entered: Option<std::sync::mpsc::Sender<()>>,
        quiesce_release: Option<std::sync::Arc<std::sync::Mutex<std::sync::mpsc::Receiver<()>>>>,
    }

    impl Drop for StagedRaceState {
        fn drop(&mut self) {
            self.dropped.fetch_add(1, Ordering::SeqCst);
        }
    }

    struct StagedRaceAddin;

    impl Addin for StagedRaceAddin {
        type SharedState = ();
        type LifecycleState = StagedRaceState;
        type Error = XllError;
        type Layers = ();

        fn open(
            _context: &OpenContext,
        ) -> Result<
            crate::addin::Opened<Self::SharedState, Self::LifecycleState, Self::Layers>,
            Self::Error,
        > {
            unreachable!()
        }

        fn quiesce(
            _: &mut Self::SharedState,
            state: &mut Self::LifecycleState,
        ) -> Result<(), Self::Error> {
            state.quiesced.fetch_add(1, Ordering::SeqCst);
            if let Some(entered) = state.quiesce_entered.take() {
                let _ = entered.send(());
            }
            if let Some(release) = state.quiesce_release.as_ref() {
                let _ = release.lock().unwrap().recv();
            }
            Ok(())
        }

        fn cleanup(state: &mut Self::LifecycleState, _: &mut crate::shutdown::CleanupReporter<'_>) {
            state.cleaned.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn close_reclaims_staged_opening_generation_when_finish_open_loses_race() {
        let fixture = crate::runtime::StaticTestRuntime::<StagedRaceAddin>::new();
        let runtime = fixture.runtime();
        let quiesced = std::sync::Arc::new(AtomicUsize::new(0));
        let cleaned = std::sync::Arc::new(AtomicUsize::new(0));
        let dropped = std::sync::Arc::new(AtomicUsize::new(0));

        let (quiesce_entered_tx, _quiesce_entered_rx) = std::sync::mpsc::channel();
        let (quiesce_release_tx, quiesce_release_rx) = std::sync::mpsc::channel();
        let open_attempt = runtime.lifecycle_runtime().begin_open().unwrap();
        let state = StagedRaceState {
            quiesced: std::sync::Arc::clone(&quiesced),
            cleaned: std::sync::Arc::clone(&cleaned),
            dropped: std::sync::Arc::clone(&dropped),
            quiesce_entered: Some(quiesce_entered_tx),
            quiesce_release: Some(std::sync::Arc::new(std::sync::Mutex::new(
                quiesce_release_rx,
            ))),
        };
        let lifecycle = lifecycle_access(runtime);
        assert!(runtime.install_addin_lifecycle(&lifecycle, state).is_ok());
        let mut open_attempt = open_attempt
            .stage(crate::generation::OpeningGeneration {
                shared_state: (),
                layers: (),
                init_config: crate::addin::RuntimeConfig::new(),
            })
            .ok()
            .expect("opening generation must stage");

        let (finish_start_tx, finish_start_rx) = std::sync::mpsc::channel();
        let (finish_result_tx, finish_result_rx) = std::sync::mpsc::channel();
        let finish_runtime = runtime;
        let finisher = std::thread::spawn(move || {
            finish_start_rx.recv().unwrap();
            let result = finish_runtime.finish_open(&mut open_attempt, Vec::new());
            finish_result_tx.send(result.is_err()).unwrap();
            quiesce_release_tx.send(()).unwrap();
        });

        let phase_runtime = runtime;
        let phase_watcher = std::thread::spawn(move || {
            while phase_runtime.phase() != crate::lifecycle::LifecyclePhase::Closing {
                std::thread::yield_now();
            }
            finish_start_tx.send(()).unwrap();
        });

        assert_eq!(host_auto_remove::<StagedRaceAddin>(runtime), 1);
        assert!(
            finish_result_rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap()
        );
        finisher.join().unwrap();
        phase_watcher.join().unwrap();

        assert_eq!(runtime.phase(), crate::lifecycle::LifecyclePhase::Closed);
        assert_eq!(quiesced.load(Ordering::SeqCst), 1);
        assert_eq!(cleaned.load(Ordering::SeqCst), 1);
        assert_eq!(dropped.load(Ordering::SeqCst), 1);
        assert!(runtime.take_current_generation().is_none());
        assert!(!runtime.has_opening_generation());
    }

    struct PanicLayersState {
        quiesced: std::sync::Arc<AtomicUsize>,
        cleaned: std::sync::Arc<AtomicUsize>,
        dropped: std::sync::Arc<AtomicUsize>,
    }

    impl Drop for PanicLayersState {
        fn drop(&mut self) {
            self.dropped.fetch_add(1, Ordering::SeqCst);
        }
    }

    struct PanicLayersAddin;

    impl Addin for PanicLayersAddin {
        type SharedState = ();
        type LifecycleState = PanicLayersState;
        type Error = XllError;
        type Layers = ();

        fn open(
            _context: &OpenContext,
        ) -> Result<
            crate::addin::Opened<Self::SharedState, Self::LifecycleState, Self::Layers>,
            Self::Error,
        > {
            unreachable!()
        }

        fn quiesce(
            _: &mut Self::SharedState,
            state: &mut Self::LifecycleState,
        ) -> Result<(), Self::Error> {
            state.quiesced.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        fn cleanup(state: &mut Self::LifecycleState, _: &mut crate::shutdown::CleanupReporter<'_>) {
            state.cleaned.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn complete_opening_generation_restores_for_rollback() {
        let runtime = Runtime::<PanicLayersAddin>::new();
        let quiesced = std::sync::Arc::new(AtomicUsize::new(0));
        let cleaned = std::sync::Arc::new(AtomicUsize::new(0));
        let dropped = std::sync::Arc::new(AtomicUsize::new(0));

        let open_attempt = runtime.lifecycle_runtime().begin_open().unwrap();
        let state = PanicLayersState {
            quiesced: std::sync::Arc::clone(&quiesced),
            cleaned: std::sync::Arc::clone(&cleaned),
            dropped: std::sync::Arc::clone(&dropped),
        };
        let lifecycle = lifecycle_access(&runtime);
        assert!(runtime.install_addin_lifecycle(&lifecycle, state).is_ok());
        let mut open_attempt = open_attempt
            .stage(crate::generation::OpeningGeneration {
                shared_state: (),
                layers: (),
                init_config: crate::addin::RuntimeConfig::new(),
            })
            .ok()
            .expect("opening generation must stage");
        assert!(runtime.has_opening_generation());

        assert!(open_attempt.fail().requires_rollback());
        let mut callbacks = HostCallbackSession::new();
        let lifecycle = lifecycle_access(&runtime);
        let outcome = rollback_open::<PanicLayersAddin>(
            &runtime,
            &lifecycle,
            &mut callbacks,
            active_runtime_generation(&runtime),
        );
        assert!(outcome.unload_safe());
        assert_eq!(runtime.phase(), crate::lifecycle::LifecyclePhase::Closed);

        assert_eq!(quiesced.load(Ordering::SeqCst), 1);
        assert_eq!(cleaned.load(Ordering::SeqCst), 1);
        assert_eq!(dropped.load(Ordering::SeqCst), 1);
        assert!(!runtime.has_opening_generation());
    }
}

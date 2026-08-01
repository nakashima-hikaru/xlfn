use crate::registration::HostRegistrar;
use crate::{
    Addin, BuildInfo, IntoXllError, OpenContext, RegistrationDescriptor, Runtime, XllError,
    XllResult,
};
use std::panic::{AssertUnwindSafe, catch_unwind};

#[must_use]
pub fn open_addin<A>(
    runtime: &Runtime<A::State>,
    addin_id: &'static str,
    version: &'static str,
    target: &'static str,
    descriptors: &[RegistrationDescriptor],
) -> i32
where
    A: Addin,
{
    std::hint::black_box(crate::crt::effective_crt_policy());
    let close_epoch = runtime.close_epoch();
    let mut open_attempt = None;
    let result = catch_unwind(AssertUnwindSafe(|| {
        if runtime.phase() == crate::LifecyclePhase::OpenRollbackPending
            && !rollback_open::<A>(runtime)
        {
            return Err(XllError::Internal {
                diagnostic_id: 0x4f50_5242_5045_4e44,
            });
        }

        // A final close that overlapped recovery of a previous failed open
        // owns the terminal outcome. Do not resurrect the runtime after that
        // close has already completed.
        if runtime.close_epoch() != close_epoch {
            return Err(XllError::Closing);
        }

        open_attempt = Some(runtime.begin_open_if_epoch(close_epoch)?);
        let registrations = open_addin_inner::<A>(
            runtime,
            BuildInfo {
                addin_id,
                version,
                target,
            },
            descriptors,
        )?;
        runtime.finish_open(
            open_attempt
                .as_mut()
                .expect("the open attempt was installed"),
            registrations,
        )
    }));

    match result {
        Ok(Ok(())) => {
            write_startup_log(addin_id, "xlAutoOpen succeeded");
            1
        }
        Ok(Err(error)) => {
            write_startup_log(addin_id, &format!("xlAutoOpen failed: {error}"));
            report_boundary_error("xlAutoOpen", &error);
            rollback_active_open::<A>(runtime, open_attempt.as_mut());
            0
        }
        Err(_) => {
            let error = XllError::Panic;
            write_startup_log(addin_id, "xlAutoOpen failed: panic at boundary");
            report_boundary_error("xlAutoOpen", &error);
            rollback_active_open::<A>(runtime, open_attempt.as_mut());
            0
        }
    }
}

fn write_startup_log(addin_id: &str, message: &str) {
    #[cfg(target_os = "windows")]
    {
        use std::fs;
        let Some(local) = std::env::var_os("LOCALAPPDATA") else {
            return;
        };
        let directory = std::path::PathBuf::from(local).join(addin_id).join("logs");
        if fs::create_dir_all(&directory).is_err() {
            return;
        }
        let _ = crate::diagnostics::append_startup_log(&directory.join("startup.log"), message);
    }
    #[cfg(not(target_os = "windows"))]
    let _ = (addin_id, message);
}

fn open_addin_inner<A>(
    runtime: &Runtime<A::State>,
    build_info: BuildInfo,
    descriptors: &[RegistrationDescriptor],
) -> XllResult<Vec<crate::RegistrationId>>
where
    A: Addin,
{
    crate::diagnostics::reset_diagnostic_router();
    let _prepared_set = crate::registration::preflight_registration(descriptors)?;
    let registrar = HostRegistrar::connect()?;
    let context = OpenContext::new(registrar.module_path().clone(), build_info);
    initialize_addin::<A>(runtime, &context)?;
    let has_async_functions = descriptors
        .iter()
        .any(|descriptor| descriptor.signature.result == crate::ResultAbi::AsyncVoid);
    if has_async_functions {
        #[cfg(feature = "async")]
        {
            let async_worker_count = async_worker_count::<A>(runtime)?;
            runtime.start_async(async_worker_count)?;
            match registrar.register_async_events() {
                Ok(events) => runtime.set_event_registrations(events),
                Err(error) => return Err(retain_transaction_error(runtime, error)),
            }
        }
        #[cfg(not(feature = "async"))]
        {
            return Err(XllError::Internal {
                diagnostic_id: 0x4153_594e_4645_4154,
            });
        }
    }
    registrar
        .register_all(descriptors)
        .map_err(|error| retain_transaction_error(runtime, error))
}

fn rollback_active_open<A>(
    runtime: &Runtime<A::State>,
    attempt: Option<&mut crate::runtime::OpenAttemptGuard<'_, A::State>>,
) where
    A: Addin,
{
    let Some(attempt) = attempt else {
        return;
    };
    if !attempt.is_active() {
        return;
    }
    if attempt.fail() {
        match catch_unwind(AssertUnwindSafe(|| rollback_open::<A>(runtime))) {
            Ok(true) => {}
            Ok(false) => fatal_unload_hazard(
                crate::shutdown::UnloadHazard::OpenRollbackFailed,
                "xlAutoOpen rollback",
                &XllError::Internal {
                    diagnostic_id: 0x4f50_5242_4641_494c,
                },
            ),
            Err(_) => fatal_unload_hazard(
                crate::shutdown::UnloadHazard::OpenRollbackFailed,
                "xlAutoOpen rollback",
                &XllError::Panic,
            ),
        }
    }
}

fn initialize_addin<A>(runtime: &Runtime<A::State>, context: &OpenContext) -> XllResult<()>
where
    A: Addin,
{
    let state = A::open(context).map_err(IntoXllError::into_xll_error)?;
    // Publish ownership before invoking add-in hooks. If either hook panics,
    // the outer boundary can now roll the state back through quiesce and cleanup.
    runtime.publish_state(state);
    let state = runtime.opening_state().ok_or(XllError::Internal {
        diagnostic_id: 0x4f50_454e_5354_4154,
    })?;
    let layers = A::udf_layers(&state);
    drop(state);
    runtime.publish_layers(layers);
    Ok(())
}

#[cfg(feature = "async")]
fn async_worker_count<A>(runtime: &Runtime<A::State>) -> XllResult<usize>
where
    A: Addin,
{
    let state = runtime.opening_state().ok_or(XllError::Internal {
        diagnostic_id: 0x4f50_454e_5354_4154,
    })?;
    Ok(A::async_worker_count(&state))
}

fn retain_transaction_error<S>(
    runtime: &Runtime<S>,
    error: crate::registration::RegistrationTransactionError,
) -> XllError {
    runtime.retain_registration_debt(error.pending_registrations);
    runtime.retain_event_registration_debt(error.pending_events);
    runtime.retain_metadata_debt_items(error.metadata_debt);
    if !error.unknown_registrations.is_empty() {
        runtime.mark_registration_state_unknown();
        for unknown in error.unknown_registrations {
            let recovery_error = unknown.recovery_error;
            let _ = catch_unwind(AssertUnwindSafe(|| {
                tracing::error!(
                    export = unknown.export_name,
                    excel_name = unknown.excel_name,
                    error = %recovery_error,
                    "xlfRegister may have committed a registration whose ID could not be recovered"
                );
            }));
            report_boundary_error("xlAutoOpen registration recovery", &recovery_error);
        }
    }
    *error.source
}

fn rollback_open<A>(runtime: &Runtime<A::State>) -> bool
where
    A: Addin,
{
    let Some(_rollback_attempt) = runtime.acquire_open_rollback() else {
        return runtime.phase() == crate::LifecyclePhase::Closed;
    };
    crate::ingress::global_ingress().begin_close();

    // A failed open closes return admission before rollback ownership is
    // acquired. Drain every producer and Excel-owned DLL-free return before
    // publishing Closed, just as the terminal xlAutoClose path does.
    runtime.wait_for_calls();
    runtime.wait_for_returns();

    let mut succeeded = true;
    #[cfg(feature = "async")]
    {
        runtime.cancel_async();
        let outcome = runtime.close_async();
        for issue in outcome.issues {
            report_cleanup_issue(&issue);
        }
    }
    if let Err(error) = runtime.close_subscriptions() {
        report_boundary_error("xlAutoOpen subscription rollback", &error);
        succeeded = false;
    }

    // State may own framework Handle leases. Remove and quiesce it before the
    // registry waits for those leases, matching the terminal close ordering.
    let mut addin_state = None;
    if let Some(state) = runtime.take_state() {
        match std::sync::Arc::try_unwrap(state) {
            Ok(mut state) => {
                match catch_unwind(AssertUnwindSafe(|| A::quiesce(&mut state)))
                    .map_err(|_| XllError::Panic)
                    .and_then(|result| result.map_err(IntoXllError::into_xll_error))
                {
                    Ok(()) => addin_state = Some(state),
                    Err(error) => {
                        report_boundary_error("xlAutoOpen rollback quiesce", &error);
                        // A failed quiesce cannot prove that State-owned
                        // execution resources have stopped. Preserve it until
                        // the caller enters the fail-stop path.
                        std::mem::forget(state);
                        succeeded = false;
                    }
                }
            }
            Err(state) => {
                runtime.restore_state_arc(state);
                let error = XllError::Internal {
                    diagnostic_id: 0x5354_4154_4553_4341,
                };
                report_boundary_error("xlAutoOpen rollback state escaped", &error);
                succeeded = false;
            }
        }
    }

    if succeeded && let Err(error) = runtime.close_handles() {
        report_boundary_error("xlAutoOpen handle rollback", &error);
        succeeded = false;
    }

    let registrations = runtime.registrations();
    let outcome = HostRegistrar::unregister_pending(&registrations);
    for (registration, error) in &outcome.failed {
        if registration.cleanup_severity().is_unload_unsafe() {
            report_boundary_error("xlAutoOpen registration rollback", error);
            succeeded = false;
        }
    }
    for (registration, error) in &outcome.metadata_debt {
        if registration.cleanup_severity().is_metadata_debt() {
            report_boundary_error("xlAutoOpen metadata debt rollback", error);
        }
    }
    for error in &outcome.cleanup_issues {
        report_cleanup_issue(&crate::shutdown::CleanupIssue {
            component: "Excel callback result",
            kind: crate::CleanupIssueKind::HostMemoryLeak,
            error: error.clone(),
        });
    }
    runtime.retain_failed_registrations(outcome.failed);
    runtime.retain_metadata_debt(outcome.metadata_debt);

    let events = runtime.event_registrations();
    let event_outcome = HostRegistrar::unregister_events_detailed(&events);
    for (_, error) in &event_outcome.failed {
        report_boundary_error("xlAutoOpen event rollback", error);
        succeeded = false;
    }
    for error in &event_outcome.cleanup_issues {
        report_cleanup_issue(&crate::shutdown::CleanupIssue {
            component: "Excel event callback result",
            kind: crate::CleanupIssueKind::HostMemoryLeak,
            error: error.clone(),
        });
    }
    runtime.retain_failed_event_registrations(event_outcome.failed);

    if succeeded {
        crate::rtd::wait_for_module_quiescence();
    }

    if succeeded && let Some(mut state) = addin_state.take() {
        let mut report = crate::shutdown::CloseReport::default();
        let cleanup = catch_unwind(AssertUnwindSafe(|| {
            let mut reporter = crate::CleanupReporter::new(&mut report);
            A::cleanup(&mut state, &mut reporter);
        }));
        if cleanup.is_err() {
            report.push(
                "Addin::cleanup",
                crate::CleanupIssueKind::DisposalPanicked,
                XllError::Panic,
            );
            std::mem::forget(state);
        } else if catch_unwind(AssertUnwindSafe(|| drop(state))).is_err() {
            report.push(
                "Addin::State::drop",
                crate::CleanupIssueKind::DisposalPanicked,
                XllError::Panic,
            );
        }
        for issue in report.issues() {
            report_cleanup_issue(issue);
        }
    }
    if let Some(state) = addin_state {
        std::mem::forget(state);
    }
    if runtime.registration_state_unknown() {
        let error = XllError::Internal {
            diagnostic_id: 0x5245_4753_554e_4b4e,
        };
        report_boundary_error("xlAutoOpen registration state unknown", &error);
        succeeded = false;
    }
    if succeeded {
        match crate::diagnostics::clear_diagnostic_sink() {
            Ok(_) => {}
            Err(crate::DiagnosticShutdownError::WorkerPanicked) => {
                report_cleanup_issue(&crate::shutdown::CleanupIssue {
                    component: "diagnostics",
                    kind: crate::CleanupIssueKind::WorkerPanickedAfterJoin,
                    error: XllError::Panic,
                });
            }
            Err(crate::DiagnosticShutdownError::ReentrantShutdown) => succeeded = false,
        }
    }
    if succeeded {
        let _exports_drained = crate::ingress::global_ingress().seal_and_drain();
        // Publish Closed only after every rollback action has completed. A
        // concurrent xlAutoClose waiter may return as soon as it observes this
        // terminal transition.
        runtime.finish_open_rollback();
    }
    succeeded
}

#[must_use]
pub fn close_addin<A>(runtime: &Runtime<A::State>) -> i32
where
    A: Addin,
{
    if catch_unwind(AssertUnwindSafe(|| close_addin_inner::<A>(runtime))).is_err() {
        let error = XllError::Panic;
        report_boundary_error("xlAutoClose boundary", &error);
        if catch_unwind(AssertUnwindSafe(|| emergency_close(runtime))).is_err() {
            report_boundary_error("xlAutoClose emergency cleanup", &error);
        }
        // A panic in the normal close path means State-owned resources may not
        // have been quiesced. Returning would let Excel unload this module while
        // detached threads or native callbacks can still execute its code.
        fatal_unload_hazard(
            crate::shutdown::UnloadHazard::UnhandledClosePanic,
            "xlAutoClose boundary",
            &error,
        );
    }
    1
}

fn emergency_close<S>(runtime: &Runtime<S>) {
    let Some(_close_attempt) = runtime.begin_final_close() else {
        return;
    };
    crate::rtd::begin_module_close();
    crate::ingress::global_ingress().begin_close();
    runtime.wait_for_calls();
    runtime.wait_for_returns();
    #[cfg(feature = "async")]
    {
        runtime.cancel_async();
        let _ = runtime.close_async();
    }
    let _ = catch_unwind(AssertUnwindSafe(|| runtime.close_subscriptions()));
    let _ = catch_unwind(AssertUnwindSafe(|| runtime.close_handles()));
    if let Some(state) = runtime.take_state() {
        // The normal close path panicked before quiescence was certified. Keeping a permanent strong
        // reference avoids running unknown destructor code after module unload.
        let _ = std::sync::Arc::into_raw(state);
    }
    let _ = crate::diagnostics::clear_diagnostic_sink();
    let _ = crate::ingress::global_ingress().seal_and_drain();
    let _ = crate::rtd::wait_for_module_quiescence();
}

fn close_addin_inner<A>(runtime: &Runtime<A::State>)
where
    A: Addin,
{
    // Even an apparently closed runtime must pass through begin_final_close:
    // a concurrent xlAutoOpen may already have sampled the previous close
    // epoch without having acquired its open-attempt token yet.
    let Some(_close_attempt) = runtime.begin_final_close() else {
        return;
    };
    crate::rtd::begin_module_close();
    crate::ingress::global_ingress().begin_close();

    let mut report = crate::shutdown::CloseReport::default();
    let mut unload_failure: Option<(crate::shutdown::UnloadHazard, &'static str, XllError)> = None;

    let registrations = runtime.registrations();
    if let Ok(outcome) = catch_unwind(AssertUnwindSafe(|| {
        HostRegistrar::unregister_pending(&registrations)
    })) {
        for (registration, error) in &outcome.failed {
            if registration.cleanup_severity().is_unload_unsafe() {
                report_boundary_error("xlAutoClose unregister", error);
                if unload_failure.is_none() {
                    unload_failure = Some((
                        crate::shutdown::UnloadHazard::HostCallbackStillRegistered,
                        "xlAutoClose unregister",
                        error.clone(),
                    ));
                }
            }
        }
        for (registration, error) in &outcome.metadata_debt {
            if registration.cleanup_severity().is_metadata_debt() {
                report.push(
                    "Excel registered name",
                    crate::CleanupIssueKind::HostMetadata,
                    error.clone(),
                );
            }
        }
        for error in outcome.cleanup_issues {
            report.push(
                "Excel callback result",
                crate::CleanupIssueKind::HostMemoryLeak,
                error,
            );
        }
        runtime.retain_failed_registrations(outcome.failed);
        runtime.retain_metadata_debt(outcome.metadata_debt);
        if runtime.has_metadata_debt() {
            let debt_count = runtime.metadata_debt().len();
            let _ = catch_unwind(AssertUnwindSafe(|| {
                tracing::warn!(
                    count = debt_count,
                    "xlAutoClose completed with host metadata debt"
                );
            }));
        }
    } else {
        let error = XllError::Panic;
        report_boundary_error("xlAutoClose unregister", &error);
        unload_failure = Some((
            crate::shutdown::UnloadHazard::HostCallbackStillRegistered,
            "xlAutoClose unregister",
            error,
        ));
    }
    if runtime.registration_state_unknown() && unload_failure.is_none() {
        let error = XllError::Internal {
            diagnostic_id: 0x5245_4753_554e_4b4e,
        };
        report_boundary_error("xlAutoClose registration state unknown", &error);
        unload_failure = Some((
            crate::shutdown::UnloadHazard::RegistrationStateUnknown,
            "xlAutoClose registration state unknown",
            error,
        ));
    }

    let event_registrations = runtime.event_registrations();
    if let Ok(event_outcome) = catch_unwind(AssertUnwindSafe(|| {
        HostRegistrar::unregister_events_detailed(&event_registrations)
    })) {
        for (_, error) in &event_outcome.failed {
            report_boundary_error("xlAutoClose event unregister", error);
            if unload_failure.is_none() {
                unload_failure = Some((
                    crate::shutdown::UnloadHazard::HostCallbackStillRegistered,
                    "xlAutoClose event unregister",
                    error.clone(),
                ));
            }
        }
        for error in event_outcome.cleanup_issues {
            report.push(
                "Excel event callback result",
                crate::CleanupIssueKind::HostMemoryLeak,
                error,
            );
        }
        runtime.retain_failed_event_registrations(event_outcome.failed);
    } else {
        let error = XllError::Panic;
        report_boundary_error("xlAutoClose event unregister", &error);
        unload_failure = Some((
            crate::shutdown::UnloadHazard::HostCallbackStillRegistered,
            "xlAutoClose event unregister",
            error,
        ));
    }

    // Excel may unload the module as soon as xlAutoClose returns. There is no
    // safe timeout for in-process Rust code: wait until every entered call has
    // released its state before continuing.
    runtime.wait_for_calls();
    runtime.wait_for_returns();

    #[cfg(feature = "async")]
    let async_stopped = {
        runtime.cancel_async();
        let outcome = runtime.close_async();
        report.extend(outcome.issues);
        outcome.certificate
    };
    #[cfg(not(feature = "async"))]
    let async_stopped = crate::shutdown::AsyncStopped::new();

    let subscriptions_stopped = runtime.close_subscriptions().unwrap_or_else(|error| {
        fatal_unload_hazard(
            crate::shutdown::UnloadHazard::SubscriptionProducerStillRunning,
            "xlAutoClose subscription shutdown",
            &error,
        )
    });

    let mut addin_state = None;
    if let Some(state) = runtime.take_state() {
        match std::sync::Arc::try_unwrap(state) {
            Ok(mut state) => {
                if let Err(error) = catch_unwind(AssertUnwindSafe(|| A::quiesce(&mut state)))
                    .map_err(|_| XllError::Panic)
                    .and_then(|result| result.map_err(IntoXllError::into_xll_error))
                {
                    report_boundary_error("xlAutoClose quiesce", &error);
                    std::mem::forget(state);
                    fatal_unload_hazard(
                        crate::shutdown::UnloadHazard::AddinQuiesceFailed,
                        "xlAutoClose quiesce",
                        &error,
                    );
                }
                addin_state = Some(state);
            }
            Err(state) => {
                let error = XllError::Internal {
                    diagnostic_id: 0x5354_4154_4553_4341,
                };
                report_boundary_error("xlAutoClose state escaped", &error);
                let _ = std::sync::Arc::into_raw(state);
                fatal_unload_hazard(
                    crate::shutdown::UnloadHazard::AddinStateEscaped,
                    "xlAutoClose state escaped",
                    &error,
                );
            }
        }
    }

    let handles_quiescent = runtime.close_handles().unwrap_or_else(|error| {
        fatal_unload_hazard(
            crate::shutdown::UnloadHazard::HandleRuntimeNotQuiescent,
            "xlAutoClose handle shutdown",
            &error,
        )
    });

    if let Some((hazard, boundary, error)) = unload_failure {
        fatal_unload_hazard(hazard, boundary, &error);
    }

    let host_callbacks = crate::shutdown::HostCallbacksDetached::new();
    let addin_quiesced = crate::shutdown::AddinQuiesced::new();
    if let Some(mut state) = addin_state {
        let cleanup = catch_unwind(AssertUnwindSafe(|| {
            let mut reporter = crate::CleanupReporter::new(&mut report);
            A::cleanup(&mut state, &mut reporter);
        }));
        if cleanup.is_err() {
            report.push(
                "Addin::cleanup",
                crate::CleanupIssueKind::DisposalPanicked,
                XllError::Panic,
            );
            std::mem::forget(state);
        } else if catch_unwind(AssertUnwindSafe(|| drop(state))).is_err() {
            report.push(
                "Addin::State::drop",
                crate::CleanupIssueKind::DisposalPanicked,
                XllError::Panic,
            );
        }
    }

    for issue in report.issues() {
        report_cleanup_issue(issue);
    }

    let diagnostics_stopped = match crate::diagnostics::clear_diagnostic_sink() {
        Ok(_) => crate::shutdown::DiagnosticsStopped::new(),
        Err(crate::DiagnosticShutdownError::WorkerPanicked) => {
            report_cleanup_issue(&crate::shutdown::CleanupIssue {
                component: "diagnostics",
                kind: crate::CleanupIssueKind::WorkerPanickedAfterJoin,
                error: XllError::Panic,
            });
            crate::shutdown::DiagnosticsStopped::new()
        }
        Err(crate::DiagnosticShutdownError::ReentrantShutdown) => fatal_unload_hazard(
            crate::shutdown::UnloadHazard::DiagnosticWorkerStillRunning,
            "xlAutoClose diagnostic shutdown",
            &XllError::Closing,
        ),
    };

    let exports_drained = crate::ingress::global_ingress().seal_and_drain();
    let rtd_quiescent = crate::rtd::wait_for_module_quiescence();

    let certificate = runtime
        .certify_close(crate::runtime::ClosePrerequisites {
            exports: exports_drained,
            rtd: rtd_quiescent,
            host_callbacks,
            async_stopped,
            subscriptions_stopped,
            handles_quiescent,
            diagnostics_stopped,
            addin_quiesced,
        })
        .unwrap_or_else(|error| {
            fatal_unload_hazard(
                crate::shutdown::UnloadHazard::CloseInvariantViolation,
                "xlAutoClose certification",
                &error,
            )
        });
    runtime.finish_close(certificate).unwrap_or_else(|error| {
        fatal_unload_hazard(
            crate::shutdown::UnloadHazard::CloseInvariantViolation,
            "xlAutoClose finalization",
            &error,
        )
    });
}

fn report_cleanup_issue(issue: &crate::shutdown::CleanupIssue) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        tracing::warn!(
            component = issue.component,
            kind = ?issue.kind,
            error = %issue.error,
            "xlAutoClose completed with a cleanup issue"
        );
    }));
    report_boundary_error(issue.component, &issue.error);
}

#[cold]
fn fatal_unload_hazard(
    hazard: crate::shutdown::UnloadHazard,
    boundary: &'static str,
    error: &XllError,
) -> ! {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        tracing::error!(?hazard, %error, "unload safety could not be established");
    }));
    fatal_unload_failure(boundary, error)
}

fn report_boundary_error(boundary: &'static str, error: &XllError) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        crate::diagnostics::report_no_unwind(boundary, error);
        let message = format!("xlfn {boundary}: {error}\n");
        #[cfg(target_os = "windows")]
        {
            use windows_sys::Win32::System::Diagnostics::Debug::OutputDebugStringW;

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
fn fatal_unload_failure(boundary: &'static str, error: &XllError) -> ! {
    report_boundary_error(boundary, error);

    // Excel has no xlAutoClose return code that can veto module unload. This
    // function is reachable only through an UnloadHazard, meaning executable
    // code could remain live or the quiescence proof could not be completed.
    #[cfg(not(test))]
    std::process::abort();

    // Unit tests need an unwindable sentinel instead of terminating the test
    // runner. Production builds always take the abort branch above.
    #[cfg(test)]
    panic!("fatal unload failure at {boundary}: {error}");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct RetryClose;

    struct RetryState {
        attempts: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    fn test_open_context() -> OpenContext {
        OpenContext::new(
            std::path::PathBuf::from("test.xll"),
            BuildInfo {
                addin_id: "test",
                version: "0",
                target: "test",
            },
        )
    }

    static LAYERS_PANIC_CLOSES: AtomicUsize = AtomicUsize::new(0);
    static LAYERS_PANIC_QUIESCES: AtomicUsize = AtomicUsize::new(0);

    struct LayersPanic;

    impl Addin for LayersPanic {
        type State = ();
        type Error = XllError;

        fn open(_: &OpenContext) -> Result<Self::State, Self::Error> {
            Ok(())
        }

        fn udf_layers(_: &Self::State) -> Vec<std::sync::Arc<dyn crate::UdfLayer>> {
            panic!("injected udf_layers panic")
        }

        fn quiesce(_: &mut Self::State) -> Result<(), Self::Error> {
            LAYERS_PANIC_QUIESCES.fetch_add(1, Ordering::AcqRel);
            Ok(())
        }

        fn cleanup(_: &mut Self::State, _: &mut crate::CleanupReporter<'_>) {
            assert_eq!(LAYERS_PANIC_QUIESCES.load(Ordering::Acquire), 1);
            LAYERS_PANIC_CLOSES.fetch_add(1, Ordering::AcqRel);
        }
    }

    #[test]
    fn xl_auto_close_on_closed_runtime_invalidates_a_pending_open_epoch() {
        let runtime = Runtime::<()>::new();
        let stale_epoch = runtime.close_epoch();

        assert_eq!(close_addin::<LayersPanic>(&runtime), 1);
        assert!(runtime.begin_open_if_epoch(stale_epoch).is_err());
        assert_eq!(runtime.phase(), crate::LifecyclePhase::Closed);
    }

    #[test]
    fn failed_concurrent_open_does_not_rollback_the_owner_attempt() {
        let runtime = Runtime::new();
        let mut owner = runtime.begin_open().unwrap();

        rollback_active_open::<LayersPanic>(&runtime, None);
        assert_eq!(runtime.phase(), crate::LifecyclePhase::Opening);

        runtime.publish((), Vec::new());
        runtime.finish_open(&mut owner, Vec::new()).unwrap();
        assert_eq!(runtime.phase(), crate::LifecyclePhase::Open);
    }

    #[test]
    fn udf_layers_panic_rolls_published_state_back_through_close() {
        LAYERS_PANIC_CLOSES.store(0, Ordering::Release);
        LAYERS_PANIC_QUIESCES.store(0, Ordering::Release);
        let runtime = Runtime::new();
        let mut open_attempt = runtime.begin_open().unwrap();
        let panic = catch_unwind(AssertUnwindSafe(|| {
            let _ = initialize_addin::<LayersPanic>(&runtime, &test_open_context());
        }));
        assert!(panic.is_err());
        assert!(open_attempt.fail());
        assert!(rollback_open::<LayersPanic>(&runtime));
        assert_eq!(LAYERS_PANIC_QUIESCES.load(Ordering::Acquire), 1);
        assert_eq!(LAYERS_PANIC_CLOSES.load(Ordering::Acquire), 1);
    }

    #[cfg(feature = "async")]
    static WORKERS_PANIC_CLOSES: AtomicUsize = AtomicUsize::new(0);

    #[cfg(feature = "async")]
    struct WorkersPanic;

    #[cfg(feature = "async")]
    impl Addin for WorkersPanic {
        type State = ();
        type Error = XllError;

        fn open(_: &OpenContext) -> Result<Self::State, Self::Error> {
            Ok(())
        }

        fn async_worker_count(_: &Self::State) -> usize {
            panic!("injected async_worker_count panic")
        }

        fn cleanup(_: &mut Self::State, _: &mut crate::CleanupReporter<'_>) {
            WORKERS_PANIC_CLOSES.fetch_add(1, Ordering::AcqRel);
        }
    }

    #[cfg(feature = "async")]
    #[test]
    fn async_worker_count_panic_rolls_published_state_back_through_close() {
        WORKERS_PANIC_CLOSES.store(0, Ordering::Release);
        let runtime = Runtime::new();
        let mut open_attempt = runtime.begin_open().unwrap();
        let panic = catch_unwind(AssertUnwindSafe(|| {
            initialize_addin::<WorkersPanic>(&runtime, &test_open_context()).unwrap();
            let _ = async_worker_count::<WorkersPanic>(&runtime);
        }));
        assert!(panic.is_err());
        assert!(open_attempt.fail());
        assert!(rollback_open::<WorkersPanic>(&runtime));
        assert_eq!(WORKERS_PANIC_CLOSES.load(Ordering::Acquire), 1);
    }

    impl Addin for RetryClose {
        type State = RetryState;
        type Error = XllError;

        fn open(_context: &OpenContext) -> Result<Self::State, Self::Error> {
            unreachable!("the close retry test publishes state directly")
        }

        fn cleanup(state: &mut Self::State, reporter: &mut crate::CleanupReporter<'_>) {
            state
                .attempts
                .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
            reporter.warn(
                "test cleanup",
                crate::CleanupIssueKind::RegistryCleanup,
                XllError::Internal {
                    diagnostic_id: 0x5445_5354_5254_5259,
                },
            );
        }
    }

    #[test]
    fn addin_cleanup_issue_does_not_prevent_finalizing_runtime() {
        let runtime = Runtime::new();
        let attempts = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut open_attempt = runtime.begin_open().unwrap();
        runtime.publish(
            RetryState {
                attempts: std::sync::Arc::clone(&attempts),
            },
            Vec::new(),
        );
        runtime.finish_open(&mut open_attempt, Vec::new()).unwrap();

        close_addin_inner::<RetryClose>(&runtime);
        assert_eq!(runtime.phase(), crate::LifecyclePhase::Closed);
        assert_eq!(attempts.load(std::sync::atomic::Ordering::Acquire), 1);
        assert!(runtime.take_state().is_none());
    }

    struct CleanupPanic;

    struct DropObserved(std::sync::Arc<AtomicUsize>);

    impl Drop for DropObserved {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::AcqRel);
        }
    }

    impl Addin for CleanupPanic {
        type State = DropObserved;
        type Error = XllError;

        fn open(_: &OpenContext) -> Result<Self::State, Self::Error> {
            unreachable!()
        }

        fn cleanup(_: &mut Self::State, _: &mut crate::CleanupReporter<'_>) {
            panic!("injected cleanup panic");
        }
    }

    #[test]
    fn cleanup_panic_leaks_state_and_still_finalizes_safe_unload() {
        let runtime = Runtime::new();
        let drops = std::sync::Arc::new(AtomicUsize::new(0));
        let mut opening = runtime.begin_open().unwrap();
        runtime.publish(DropObserved(std::sync::Arc::clone(&drops)), Vec::new());
        runtime.finish_open(&mut opening, Vec::new()).unwrap();

        close_addin_inner::<CleanupPanic>(&runtime);

        assert_eq!(runtime.phase(), crate::LifecyclePhase::Closed);
        assert_eq!(drops.load(Ordering::Acquire), 0);
    }

    struct QuiesceFailure;

    impl Addin for QuiesceFailure {
        type State = DropObserved;
        type Error = XllError;

        fn open(_: &OpenContext) -> Result<Self::State, Self::Error> {
            unreachable!()
        }

        fn quiesce(_: &mut Self::State) -> Result<(), Self::Error> {
            Err(XllError::Internal {
                diagnostic_id: 0x5155_4945_5343_4546,
            })
        }
    }

    #[test]
    fn quiesce_failure_enters_fatal_path_without_dropping_state() {
        let runtime = Runtime::new();
        let drops = std::sync::Arc::new(AtomicUsize::new(0));
        let mut opening = runtime.begin_open().unwrap();
        runtime.publish(DropObserved(std::sync::Arc::clone(&drops)), Vec::new());
        runtime.finish_open(&mut opening, Vec::new()).unwrap();

        let fatal = catch_unwind(AssertUnwindSafe(|| {
            close_addin_inner::<QuiesceFailure>(&runtime);
        }));

        assert!(fatal.is_err());
        assert_eq!(runtime.phase(), crate::LifecyclePhase::Closing);
        assert_eq!(drops.load(Ordering::Acquire), 0);
    }

    #[test]
    fn open_rollback_cleanup_issue_still_finalizes_without_reinstalling_state() {
        let runtime = Runtime::new();
        let attempts = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut open_attempt = runtime.begin_open().unwrap();
        runtime.publish(
            RetryState {
                attempts: std::sync::Arc::clone(&attempts),
            },
            Vec::new(),
        );

        assert!(open_attempt.fail());
        assert!(rollback_open::<RetryClose>(&runtime));
        assert_eq!(runtime.phase(), crate::LifecyclePhase::Closed);
        assert_eq!(attempts.load(std::sync::atomic::Ordering::Acquire), 1);
        assert!(runtime.take_state().is_none());
    }

    struct CleanClose;

    impl Addin for CleanClose {
        type State = ();
        type Error = XllError;

        fn open(_context: &OpenContext) -> Result<Self::State, Self::Error> {
            unreachable!()
        }
    }

    struct StateOwnedHandleObject;

    impl crate::ExcelHandleObject for StateOwnedHandleObject {}

    struct StateWithHandle {
        handle: Option<crate::Handle<StateOwnedHandleObject>>,
        quiesced: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    struct StateHandleRollback;

    impl Addin for StateHandleRollback {
        type State = StateWithHandle;
        type Error = XllError;

        fn open(_context: &OpenContext) -> Result<Self::State, Self::Error> {
            unreachable!()
        }

        fn quiesce(state: &mut Self::State) -> Result<(), Self::Error> {
            drop(state.handle.take());
            state.quiesced.fetch_add(1, Ordering::AcqRel);
            Ok(())
        }
    }

    #[test]
    fn failed_open_quiesces_state_owned_handle_before_registry_shutdown() {
        let runtime = Runtime::new();
        let handles = runtime.handles().unwrap();
        let (token, _) = handles
            .prepare("state-owned".to_owned(), || {
                Ok(std::sync::Arc::new(StateOwnedHandleObject))
            })
            .unwrap();
        let handle = handles.lookup(&token).unwrap();
        let quiesced = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut open_attempt = runtime.begin_open().unwrap();
        runtime.publish(
            StateWithHandle {
                handle: Some(handle),
                quiesced: std::sync::Arc::clone(&quiesced),
            },
            Vec::new(),
        );

        assert!(open_attempt.fail());
        assert!(rollback_open::<StateHandleRollback>(&runtime));
        assert_eq!(quiesced.load(Ordering::Acquire), 1);
        assert_eq!(runtime.phase(), crate::LifecyclePhase::Closed);
    }

    struct AlwaysFailClose;

    impl Addin for AlwaysFailClose {
        type State = ();
        type Error = XllError;

        fn open(_context: &OpenContext) -> Result<Self::State, Self::Error> {
            unreachable!()
        }

        fn cleanup(_state: &mut Self::State, reporter: &mut crate::CleanupReporter<'_>) {
            reporter.warn(
                "always fail cleanup",
                crate::CleanupIssueKind::RegistryCleanup,
                XllError::Internal {
                    diagnostic_id: 0x4641_494c,
                },
            );
        }
    }

    #[test]
    fn failing_open_rollback_is_finalized_by_xl_auto_close() {
        let runtime = Runtime::new();
        let mut open_attempt = runtime.begin_open().unwrap();
        runtime.publish((), Vec::new());

        assert!(open_attempt.fail());
        assert!(rollback_open::<AlwaysFailClose>(&runtime));
        assert_eq!(runtime.phase(), crate::LifecyclePhase::Closed);

        assert_eq!(close_addin::<AlwaysFailClose>(&runtime), 1);
        assert_eq!(runtime.phase(), crate::LifecyclePhase::Closed);
    }

    #[test]
    fn xl_auto_close_waits_for_active_call_and_returns_one_after_clean_close() {
        let runtime = std::sync::Arc::new(Runtime::new());
        let mut open_attempt = runtime.begin_open().unwrap();
        runtime.publish((), Vec::new());
        runtime.finish_open(&mut open_attempt, Vec::new()).unwrap();
        let call = runtime.enter().unwrap();
        let closer_runtime = std::sync::Arc::clone(&runtime);
        let (closed_tx, closed_rx) = std::sync::mpsc::channel();
        let closer = std::thread::spawn(move || {
            closed_tx
                .send(close_addin::<CleanClose>(&closer_runtime))
                .unwrap();
        });

        assert!(
            closed_rx
                .recv_timeout(std::time::Duration::from_millis(20))
                .is_err()
        );
        drop(call);
        assert_eq!(
            closed_rx
                .recv_timeout(std::time::Duration::from_secs(1))
                .unwrap(),
            1
        );
        closer.join().unwrap();
        assert_eq!(runtime.phase(), crate::LifecyclePhase::Closed);
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

    // SAFETY: disconnect_and_wait ensures no background work accesses module code.
    unsafe impl crate::RtdSubscription for OrderedSubscription {
        fn request_cancel(&self) {}

        fn disconnect_and_wait(self: Box<Self>) -> XllResult<()> {
            self.events.lock().unwrap().push("subscription");
            Ok(())
        }
    }

    struct OrderedSource {
        events: std::sync::Arc<std::sync::Mutex<Vec<&'static str>>>,
    }

    impl crate::RtdSource for OrderedSource {
        type Value = f64;

        fn subscribe(
            &self,
            _topic: &crate::RtdTopic,
            _sink: crate::RtdSink<Self::Value>,
        ) -> XllResult<Box<dyn crate::RtdSubscription>> {
            Ok(Box::new(OrderedSubscription {
                events: std::sync::Arc::clone(&self.events),
            }))
        }
    }

    impl Drop for OrderedHandle {
        fn drop(&mut self) {
            self.events.lock().unwrap().push("handle");
        }
    }

    impl crate::ExcelHandleObject for OrderedHandle {}

    impl Addin for OrderedClose {
        type State = OrderedState;
        type Error = XllError;

        fn open(_context: &OpenContext) -> Result<Self::State, Self::Error> {
            unreachable!()
        }

        fn cleanup(state: &mut Self::State, _: &mut crate::CleanupReporter<'_>) {
            state.events.lock().unwrap().push("state");
        }
    }

    #[test]
    fn runtime_owned_subscriptions_and_handles_drop_before_addin_state_closes() {
        let runtime = Runtime::new();
        let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut open_attempt = runtime.begin_open().unwrap();
        runtime.publish(
            OrderedState {
                events: std::sync::Arc::clone(&events),
            },
            Vec::new(),
        );
        runtime.finish_open(&mut open_attempt, Vec::new()).unwrap();
        runtime
            .handles()
            .unwrap()
            .prepare("ordered".to_owned(), || {
                Ok(std::sync::Arc::new(OrderedHandle {
                    events: std::sync::Arc::clone(&events),
                }))
            })
            .unwrap();
        let subscriptions = runtime.subscriptions();
        let key = subscriptions
            .prepare(
                std::sync::Arc::new(OrderedSource {
                    events: std::sync::Arc::clone(&events),
                }),
                crate::RtdTopic::single("ordered").unwrap(),
            )
            .unwrap();
        subscriptions.connect(1, 1, key.key()).unwrap();

        assert_eq!(close_addin::<OrderedClose>(&runtime), 1);
        assert_eq!(*events.lock().unwrap(), ["subscription", "handle", "state"]);
    }
}

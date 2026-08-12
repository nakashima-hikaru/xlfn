use crate::host_callback::{HostCallbackSession, HostCallbackState};
use crate::registration::HostRegistrar;
use crate::{
    Addin, AddinId, BuildInfo, IntoXllError, OpenContext, RegistrationDescriptor, Runtime,
    XllError, XllResult,
};
use std::panic::{AssertUnwindSafe, catch_unwind};

#[must_use]
pub fn open_addin<A>(
    runtime: &Runtime<A::State>,
    addin_id: &AddinId,
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
    let mut callbacks = HostCallbackSession::new();
    let result = catch_unwind(AssertUnwindSafe(|| {
        if runtime.phase() == crate::LifecyclePhase::OpenRollbackPending {
            let outcome = rollback_open::<A>(runtime, &mut callbacks);
            if !outcome.unload_safe() {
                fatal_unload_hazard(
                    runtime,
                    crate::shutdown::UnloadHazard::OpenRollbackFailed,
                    "xlAutoOpen pending rollback",
                    &XllError::Internal {
                        diagnostic_id: 0x4f50_5242_5045_4e44,
                    },
                );
            }
        }

        // A final close that overlapped recovery of a previous failed open
        // owns the terminal outcome. Do not resurrect the runtime after that
        // close has already completed.
        if runtime.close_epoch() != close_epoch {
            return Err(XllError::Closing);
        }

        open_attempt = Some(runtime.begin_open_if_epoch(close_epoch)?);
        retry_metadata_debt(runtime, &mut callbacks)?;
        let registrations = open_addin_inner::<A>(
            runtime,
            BuildInfo {
                addin_id: addin_id.clone(),
                version,
                target,
            },
            descriptors,
            &mut callbacks,
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
            rollback_active_open::<A>(runtime, open_attempt.as_mut(), &mut callbacks);
            0
        }
        Err(_) => {
            let error = XllError::Panic;
            write_startup_log(addin_id, "xlAutoOpen failed: panic at boundary");
            report_boundary_error("xlAutoOpen", &error);
            rollback_active_open::<A>(runtime, open_attempt.as_mut(), &mut callbacks);
            0
        }
    }
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

fn retry_metadata_debt<S>(
    runtime: &Runtime<S>,
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
            kind: crate::CleanupIssueKind::HostMemoryLeak,
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

fn open_addin_inner<A>(
    runtime: &Runtime<A::State>,
    build_info: BuildInfo,
    descriptors: &[RegistrationDescriptor],
    callbacks: &mut HostCallbackSession,
) -> XllResult<Vec<crate::RegistrationId>>
where
    A: Addin,
{
    #[cfg(test)]
    let _diagnostic_test_guard = crate::diagnostics::DIAGNOSTIC_TEST_MUTEX.lock();
    crate::diagnostics::reset_diagnostic_router()?;
    let _prepared_set = crate::registration::preflight_registration(descriptors)?;
    let registrar = HostRegistrar::connect(callbacks)
        .map_err(|error| retain_transaction_error(runtime, error))?;
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
            match registrar.register_async_events(callbacks) {
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
        .register_all(callbacks, descriptors)
        .map_err(|error| retain_transaction_error(runtime, error))
}

fn rollback_active_open<A>(
    runtime: &Runtime<A::State>,
    attempt: Option<&mut crate::runtime::OpenAttemptGuard<'_, A::State>>,
    callbacks: &mut HostCallbackSession,
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
        match catch_unwind(AssertUnwindSafe(|| rollback_open::<A>(runtime, callbacks))) {
            Ok(outcome) if outcome.unload_safe() => {}
            Ok(_) => fatal_unload_hazard(
                runtime,
                crate::shutdown::UnloadHazard::OpenRollbackFailed,
                "xlAutoOpen rollback",
                &XllError::Internal {
                    diagnostic_id: 0x4f50_5242_4641_494c,
                },
            ),
            Err(_) => fatal_unload_hazard(
                runtime,
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
    runtime.retain_metadata_debt(error.metadata_debt);
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

struct OpenRollbackOutcome {
    local_quiescent: bool,
    host_callbacks_detached: bool,
    #[allow(
        dead_code,
        reason = "Host callback session token retained for rollback outcome verification"
    )]
    host_callback_state: HostCallbackState,
    registration_state_known: bool,
    finalized: bool,
}

impl OpenRollbackOutcome {
    fn unload_safe(&self) -> bool {
        self.local_quiescent
            && self.host_callbacks_detached
            && self.registration_state_known
            && self.finalized
    }
}

fn rollback_open<A>(
    runtime: &Runtime<A::State>,
    callbacks: &mut HostCallbackSession,
) -> OpenRollbackOutcome
where
    A: Addin,
{
    #[cfg(test)]
    let _diagnostic_test_guard = crate::diagnostics::DIAGNOSTIC_TEST_MUTEX.lock();
    let Some(_rollback_attempt) = runtime.acquire_open_rollback() else {
        return OpenRollbackOutcome {
            local_quiescent: runtime.phase() == crate::LifecyclePhase::Closed,
            host_callbacks_detached: runtime.registrations().is_empty()
                && runtime.event_registrations().is_empty(),
            host_callback_state: callbacks.state(),
            registration_state_known: !runtime.registration_state_unknown(),
            finalized: runtime.phase() == crate::LifecyclePhase::Closed
                && crate::rtd::module_unload_certified(),
        };
    };
    crate::ingress::global_ingress().begin_close_with(|| {});

    let mut local_quiescent = true;
    let exports_drained = crate::ingress::global_ingress().seal_and_drain();
    runtime.wait_for_returns();

    #[cfg(feature = "async")]
    let async_stopped = {
        runtime.cancel_async();
        let outcome = runtime.close_async();
        for issue in outcome.issues {
            report_cleanup_issue(&issue);
        }
        Some(outcome.certificate)
    };
    #[cfg(not(feature = "async"))]
    let async_stopped = Some(crate::shutdown::AsyncStopped::new());
    let subscriptions_stopped = match runtime.close_subscriptions() {
        Ok(certificate) => Some(certificate),
        Err(error) => {
            report_boundary_error("xlAutoOpen subscription rollback", &error);
            local_quiescent = false;
            None
        }
    };

    let registrations = runtime.registrations();
    let outcome = HostRegistrar::unregister_pending(callbacks, &registrations);
    for (registration, error) in &outcome.failed {
        if registration.cleanup_severity().is_unload_unsafe() {
            report_boundary_error("xlAutoOpen registration rollback", error);
        }
    }
    for debt in &outcome.metadata_debt {
        report_boundary_error("xlAutoOpen metadata debt rollback", debt.last_error());
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
    if callbacks.permits_callbacks() {
        let event_outcome = HostRegistrar::unregister_events_detailed(callbacks, &events);
        for (_, error) in &event_outcome.failed {
            report_boundary_error("xlAutoOpen event rollback", error);
        }
        for error in &event_outcome.cleanup_issues {
            report_cleanup_issue(&crate::shutdown::CleanupIssue {
                component: "Excel event callback result",
                kind: crate::CleanupIssueKind::HostMemoryLeak,
                error: error.clone(),
            });
        }
        runtime.retain_failed_event_registrations(event_outcome.failed);
    } else if !events.is_empty() {
        let error = callbacks
            .terminal_status()
            .map(|status| XllError::ExcelApi {
                function: "xlEventRegister(unregister suppressed)",
                code: status.raw_code(),
            })
            .unwrap_or(XllError::Closing);
        report_boundary_error("xlAutoOpen event rollback", &error);
        runtime.retain_failed_event_registrations(
            events
                .into_iter()
                .map(|event| (event, error.clone()))
                .collect(),
        );
    } else {
        runtime.retain_failed_event_registrations(Vec::new());
    }

    crate::callback_gate::close_from_runtime();

    // State may own framework Handle leases. Remove and quiesce it before the
    // registry waits for those leases, matching the terminal close ordering.
    let mut addin_state = None;
    let mut addin_quiesced = None;
    if let Some(state) = runtime.take_state() {
        match std::sync::Arc::try_unwrap(state) {
            Ok(mut state) => {
                match catch_unwind(AssertUnwindSafe(|| A::quiesce(&mut state)))
                    .map_err(|_| XllError::Panic)
                    .and_then(|result| result.map_err(IntoXllError::into_xll_error))
                {
                    Ok(()) => {
                        addin_state = Some(state);
                        addin_quiesced = Some(crate::shutdown::AddinQuiesced::new());
                    }
                    Err(error) => {
                        report_boundary_error("xlAutoOpen rollback quiesce", &error);
                        // A failed quiesce cannot prove that State-owned
                        // execution resources have stopped. Preserve it until
                        // the caller enters the fail-stop path.
                        std::mem::forget(state);
                        local_quiescent = false;
                    }
                }
            }
            Err(state) => {
                runtime.restore_state_arc(state);
                let error = XllError::Internal {
                    diagnostic_id: 0x5354_4154_4553_4341,
                };
                report_boundary_error("xlAutoOpen rollback state escaped", &error);
                local_quiescent = false;
            }
        }
    } else {
        addin_quiesced = Some(crate::shutdown::AddinQuiesced::new());
    }

    let handles_quiescent = if local_quiescent {
        match runtime.close_handles() {
            Ok(certificate) => Some(certificate),
            Err(error) => {
                report_boundary_error("xlAutoOpen handle rollback", &error);
                local_quiescent = false;
                None
            }
        }
    } else {
        None
    };

    let rtd_quiescent = if local_quiescent {
        match crate::rtd::wait_for_module_quiescence() {
            Ok(certificate) => Some(certificate),
            Err(_) => {
                let error = XllError::Internal {
                    diagnostic_id: 0x5254_445f_4749_5451,
                };
                report_boundary_error("xlAutoOpen RTD quiescence rollback", &error);
                local_quiescent = false;
                None
            }
        }
    } else {
        None
    };

    if local_quiescent && let Some(mut state) = addin_state.take() {
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
    }
    let mut diagnostics_stopped = None;
    if local_quiescent {
        match crate::diagnostics::close_diagnostic_router() {
            Ok(outcome) => {
                for issue in outcome.issues {
                    report_cleanup_issue(&issue);
                }
                diagnostics_stopped = Some(outcome.certificate);
            }
            Err(error) => {
                let error = error.into_xll_error();
                report_boundary_error("xlAutoOpen diagnostic rollback", &error);
                local_quiescent = false;
            }
        }
    }
    let host_callbacks_detached =
        runtime.registrations().is_empty() && runtime.event_registrations().is_empty();
    let registration_state_known = !runtime.registration_state_unknown();
    let mut finalized = false;
    if local_quiescent && host_callbacks_detached && registration_state_known {
        let prerequisites = crate::runtime::OpenRollbackPrerequisites {
            exports: exports_drained,
            rtd: rtd_quiescent
                .expect("RTD certificate is present when rollback is local-quiescent"),
            host_callbacks: crate::shutdown::HostCallbacksDetached::new(),
            async_stopped: {
                #[allow(
                    clippy::unnecessary_literal_unwrap,
                    reason = "Constant Some when feature async is disabled"
                )]
                async_stopped
                    .expect("async certificate is present when rollback is local-quiescent")
            },
            subscriptions_stopped: subscriptions_stopped
                .expect("subscription certificate is present when rollback is local-quiescent"),
            handles_quiescent: handles_quiescent
                .expect("handle certificate is present when rollback is local-quiescent"),
            diagnostics_stopped: diagnostics_stopped
                .expect("diagnostic certificate is present when rollback is local-quiescent"),
            addin_quiesced: addin_quiesced
                .expect("addin certificate is present when rollback is local-quiescent"),
        };
        match runtime
            .certify_open_rollback(prerequisites)
            .and_then(|certificate| runtime.finish_open_rollback(certificate))
        {
            Ok(()) => finalized = true,
            Err(error) => {
                report_boundary_error("xlAutoOpen rollback certification", &error);
                local_quiescent = false;
            }
        }
    }
    OpenRollbackOutcome {
        local_quiescent,
        host_callbacks_detached,
        host_callback_state: callbacks.state(),
        registration_state_known,
        finalized,
    }
}

#[must_use]
pub fn close_addin<A>(runtime: &Runtime<A::State>) -> i32
where
    A: Addin,
{
    let mut callbacks = HostCallbackSession::new();
    let close_result = catch_unwind(AssertUnwindSafe(|| {
        close_addin_inner::<A>(runtime, &mut callbacks)
    }));
    let success = match close_result {
        Ok(success) => success,
        Err(_) => {
            let error = XllError::Panic;
            report_boundary_error("xlAutoClose boundary", &error);
            if catch_unwind(AssertUnwindSafe(|| {
                emergency_close(runtime, &mut callbacks)
            }))
            .is_err()
            {
                report_boundary_error("xlAutoClose emergency cleanup", &error);
            }
            // A panic in the normal close path means State-owned resources may not
            // have been quiesced. Returning would let Excel unload this module while
            // detached threads or native callbacks can still execute its code.
            fatal_unload_hazard(
                runtime,
                crate::shutdown::UnloadHazard::UnhandledClosePanic,
                "xlAutoClose boundary",
                &error,
            );
        }
    };
    match success {
        CloseSuccess::AlreadyClosed => 1,
        #[cfg(not(any(test, feature = "shutdown-refinement")))]
        CloseSuccess::Closed {
            witness: _witness,
            close_attempt: _close_attempt,
        } => 1,
        #[cfg(any(test, feature = "shutdown-refinement"))]
        CloseSuccess::Closed {
            witness,
            close_attempt: _close_attempt,
        } => {
            runtime
                .record_ghost_returned_success(witness)
                .unwrap_or_else(|error| {
                    fatal_unload_hazard(
                        runtime,
                        crate::shutdown::UnloadHazard::CloseInvariantViolation,
                        "xlAutoClose success refinement",
                        &error,
                    )
                });
            1
        }
    }
}

fn emergency_close<S>(runtime: &Runtime<S>, _callbacks: &mut HostCallbackSession) {
    #[cfg(test)]
    let _diagnostic_test_guard = crate::diagnostics::DIAGNOSTIC_TEST_MUTEX.lock();
    let Some(_close_attempt) = runtime.begin_final_close() else {
        return;
    };
    crate::ingress::global_ingress().begin_close_with(|| {
        #[cfg(any(test, feature = "shutdown-refinement"))]
        if runtime.ghost_generation_active() {
            runtime.record_ghost_event(crate::shutdown_refinement::GhostEvent::BeginClose);
        }
    });
    crate::rtd::begin_module_close();
    let _exports_drained = crate::ingress::global_ingress().seal_and_drain();
    runtime.wait_for_returns();
    #[cfg(feature = "async")]
    {
        runtime.cancel_async();
        let _ = runtime.close_async();
    }
    let _ = catch_unwind(AssertUnwindSafe(|| runtime.close_subscriptions()));
    crate::callback_gate::close_from_runtime();
    let _ = catch_unwind(AssertUnwindSafe(|| runtime.close_handles()));
    if let Some(state) = runtime.take_state() {
        // The normal close path panicked before quiescence was certified. Keeping a permanent strong
        // reference avoids running unknown destructor code after module unload.
        let _ = std::sync::Arc::into_raw(state);
    }
    let _ = crate::diagnostics::close_diagnostic_router();
    if let Err(error) = crate::rtd::wait_for_module_quiescence() {
        let hazard = if error.revocation_debt != 0 {
            crate::shutdown::UnloadHazard::RtdGitRevocationDebt
        } else {
            crate::shutdown::UnloadHazard::RtdGitCallbackStillRegistered
        };
        fatal_unload_hazard(
            runtime,
            hazard,
            "xlAutoClose emergency RTD GIT quiescence",
            &XllError::Internal {
                diagnostic_id: 0x5254_445f_4749_5451,
            },
        );
    }
}

enum CloseSuccess<'runtime, S> {
    AlreadyClosed,
    Closed {
        witness: crate::runtime::ClosedWitness,
        close_attempt: crate::runtime::CloseAttemptGuard<'runtime, S>,
    },
}

fn close_addin_inner<'runtime, A>(
    runtime: &'runtime Runtime<A::State>,
    callbacks: &mut HostCallbackSession,
) -> CloseSuccess<'runtime, A::State>
where
    A: Addin,
{
    #[cfg(test)]
    let _diagnostic_test_guard = crate::diagnostics::DIAGNOSTIC_TEST_MUTEX.lock();
    // Even an apparently closed runtime must pass through begin_final_close:
    // a concurrent xlAutoOpen may already have sampled the previous close
    // epoch without having acquired its open-attempt token yet.
    let Some(close_attempt) = runtime.begin_final_close() else {
        return CloseSuccess::AlreadyClosed;
    };
    crate::ingress::global_ingress().begin_close_with(|| {
        #[cfg(any(test, feature = "shutdown-refinement"))]
        if runtime.ghost_generation_active() {
            runtime.record_ghost_event(crate::shutdown_refinement::GhostEvent::BeginClose);
        }
    });
    crate::rtd::begin_module_close();

    let mut report = crate::shutdown::CloseReport::default();
    let mut unload_failure: Option<(crate::shutdown::UnloadHazard, &'static str, XllError)> = None;

    let exports_drained = crate::ingress::global_ingress().seal_and_drain();

    #[cfg(any(test, feature = "shutdown-refinement"))]
    runtime.record_ghost_event_linearized(crate::shutdown_refinement::GhostEvent::CallsDrained);

    runtime.wait_for_returns();

    #[cfg(any(test, feature = "shutdown-refinement"))]
    runtime.record_ghost_event(crate::shutdown_refinement::GhostEvent::ReturnsDrained);

    #[cfg(feature = "async")]
    let async_stopped = {
        runtime.cancel_async();
        let outcome = runtime.close_async();
        report.extend(outcome.issues);
        outcome.certificate
    };
    #[cfg(not(feature = "async"))]
    let async_stopped = crate::shutdown::AsyncStopped::new();

    #[cfg(any(test, feature = "shutdown-refinement"))]
    runtime.record_ghost_async_stopped();
    #[cfg(any(test, feature = "shutdown-refinement"))]
    runtime.record_ghost_event(crate::shutdown_refinement::GhostEvent::AsyncDrained);

    let subscriptions_stopped = runtime.close_subscriptions().unwrap_or_else(|error| {
        fatal_unload_hazard(
            runtime,
            crate::shutdown::UnloadHazard::SubscriptionProducerStillRunning,
            "xlAutoClose subscription shutdown",
            &error,
        )
    });

    #[cfg(any(test, feature = "shutdown-refinement"))]
    runtime.record_ghost_event(crate::shutdown_refinement::GhostEvent::SubscriptionsDrained);

    let registrations = runtime.registrations();
    if let Ok(outcome) = catch_unwind(AssertUnwindSafe(|| {
        HostRegistrar::unregister_pending(callbacks, &registrations)
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
        for debt in &outcome.metadata_debt {
            report.push(
                "Excel registered name",
                crate::CleanupIssueKind::HostMetadata,
                debt.last_error().clone(),
            );
        }
        for error in outcome.cleanup_issues {
            report.push(
                "Excel callback result",
                crate::CleanupIssueKind::HostMemoryLeak,
                error,
            );
        }
        #[cfg(any(test, feature = "shutdown-refinement"))]
        for _ in &outcome.succeeded {
            runtime.record_ghost_event(crate::shutdown_refinement::GhostEvent::UnregisterFunction);
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
    if !callbacks.permits_callbacks() {
        if !event_registrations.is_empty() {
            let error = callbacks
                .terminal_status()
                .map(|status| XllError::ExcelApi {
                    function: "xlEventRegister(unregister suppressed)",
                    code: status.raw_code(),
                })
                .unwrap_or(XllError::Closing);
            for _ in &event_registrations {
                report_boundary_error("xlAutoClose event unregister", &error);
            }
            runtime.retain_failed_event_registrations(
                event_registrations
                    .into_iter()
                    .map(|registration| (registration, error.clone()))
                    .collect(),
            );
            if unload_failure.is_none() {
                unload_failure = Some((
                    crate::shutdown::UnloadHazard::HostCallbackStillRegistered,
                    "xlAutoClose event unregister suppressed",
                    error,
                ));
            }
        }
    } else if let Ok(event_outcome) = catch_unwind(AssertUnwindSafe(|| {
        HostRegistrar::unregister_events_detailed(callbacks, &event_registrations)
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
        #[cfg(any(test, feature = "shutdown-refinement"))]
        for _ in &event_outcome.succeeded {
            runtime.record_ghost_event(crate::shutdown_refinement::GhostEvent::UnregisterEvent);
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

    // Host callbacks are now the final direct Excel C API operations in this
    // lifecycle. A terminal result transitions the module gate immediately;
    // once unregistering is complete, close it unconditionally so all later
    // cleanup is provably callback-free.
    crate::callback_gate::close_from_runtime();

    #[cfg(any(test, feature = "shutdown-refinement"))]
    runtime.record_ghost_event(crate::shutdown_refinement::GhostEvent::CloseCallbackGate);

    if let Some((hazard, boundary, error)) = unload_failure.take() {
        fatal_unload_hazard(runtime, hazard, boundary, &error);
    }

    let host_callbacks = crate::shutdown::HostCallbacksDetached::new();
    #[cfg(any(test, feature = "shutdown-refinement"))]
    runtime.record_ghost_event(crate::shutdown_refinement::GhostEvent::HostDetached);

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
                        runtime,
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
                    runtime,
                    crate::shutdown::UnloadHazard::AddinStateEscaped,
                    "xlAutoClose state escaped",
                    &error,
                );
            }
        }
    } else {
        // A missing runtime root is the already-consumed state case. The
        // abstract proof still records uniqueness and quiescence explicitly
        // before advancing the state milestone.
    }

    #[cfg(any(test, feature = "shutdown-refinement"))]
    runtime.record_ghost_state_unique();
    #[cfg(any(test, feature = "shutdown-refinement"))]
    runtime.record_ghost_addin_quiesced();
    #[cfg(any(test, feature = "shutdown-refinement"))]
    runtime.record_ghost_event(crate::shutdown_refinement::GhostEvent::StateClosed);

    let handles_quiescent = runtime.close_handles().unwrap_or_else(|error| {
        fatal_unload_hazard(
            runtime,
            crate::shutdown::UnloadHazard::HandleRuntimeNotQuiescent,
            "xlAutoClose handle shutdown",
            &error,
        )
    });
    #[cfg(any(test, feature = "shutdown-refinement"))]
    runtime.record_ghost_event(crate::shutdown_refinement::GhostEvent::HandlesDrained);

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
        #[cfg(any(test, feature = "shutdown-refinement"))]
        runtime.record_ghost_event(crate::shutdown_refinement::GhostEvent::RecordCleanupIssue);
        report_cleanup_issue(issue);
    }

    let diagnostics = crate::diagnostics::close_diagnostic_router().unwrap_or_else(|error| {
        let error = error.into_xll_error();
        fatal_unload_hazard(
            runtime,
            crate::shutdown::UnloadHazard::DiagnosticWorkerStillRunning,
            "xlAutoClose diagnostic shutdown",
            &error,
        )
    });
    for issue in &diagnostics.issues {
        #[cfg(any(test, feature = "shutdown-refinement"))]
        runtime.record_ghost_event(crate::shutdown_refinement::GhostEvent::RecordCleanupIssue);
        report_cleanup_issue(issue);
    }
    let diagnostics_stopped = diagnostics.certificate;

    #[cfg(any(test, feature = "shutdown-refinement"))]
    runtime
        .record_ghost_diagnostics_stopped()
        .unwrap_or_else(|error| {
            fatal_unload_hazard(
                runtime,
                crate::shutdown::UnloadHazard::DiagnosticWorkerStillRunning,
                "xlAutoClose diagnostic refinement",
                &error,
            )
        });
    #[cfg(any(test, feature = "shutdown-refinement"))]
    runtime.record_ghost_event(crate::shutdown_refinement::GhostEvent::DiagnosticsDrained);

    let rtd_quiescent = crate::rtd::wait_for_module_quiescence().unwrap_or_else(|error| {
        let hazard = if error.revocation_debt != 0 {
            crate::shutdown::UnloadHazard::RtdGitRevocationDebt
        } else {
            crate::shutdown::UnloadHazard::RtdGitCallbackStillRegistered
        };
        fatal_unload_hazard(
            runtime,
            hazard,
            "xlAutoClose RTD GIT quiescence",
            &XllError::Internal {
                diagnostic_id: 0x5254_445f_4749_5451,
            },
        )
    });

    #[cfg(any(test, feature = "shutdown-refinement"))]
    runtime.record_ghost_event(crate::shutdown_refinement::GhostEvent::RtdDrained);

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
                runtime,
                crate::shutdown::UnloadHazard::CloseInvariantViolation,
                "xlAutoClose certification",
                &error,
            )
        });
    let closed_witness = runtime.finish_close(certificate).unwrap_or_else(|error| {
        fatal_unload_hazard(
            runtime,
            crate::shutdown::UnloadHazard::CloseInvariantViolation,
            "xlAutoClose finalization",
            &error,
        )
    });
    CloseSuccess::Closed {
        witness: closed_witness,
        close_attempt,
    }
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
fn fatal_unload_hazard<S>(
    runtime: &Runtime<S>,
    hazard: crate::shutdown::UnloadHazard,
    boundary: &'static str,
    error: &XllError,
) -> ! {
    let _ = runtime;
    #[cfg(any(test, feature = "shutdown-refinement"))]
    runtime.ghost_fail_stop(hazard.ghost_failure());
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

    static COMPOSITION_TRACE_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct RetryClose;

    struct RetryState {
        attempts: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    fn test_open_context() -> OpenContext {
        OpenContext::new(
            std::path::PathBuf::from("test.xll"),
            BuildInfo {
                addin_id: AddinId::parse("test").unwrap(),
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
        let mut callbacks = HostCallbackSession::new();

        rollback_active_open::<LayersPanic>(&runtime, None, &mut callbacks);
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
        let mut callbacks = HostCallbackSession::new();
        assert!(rollback_open::<LayersPanic>(&runtime, &mut callbacks).unload_safe());
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
        let mut callbacks = HostCallbackSession::new();
        assert!(rollback_open::<WorkersPanic>(&runtime, &mut callbacks).unload_safe());
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

        let mut callbacks = HostCallbackSession::new();
        close_addin_inner::<RetryClose>(&runtime, &mut callbacks);
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

        let mut callbacks = HostCallbackSession::new();
        close_addin_inner::<CleanupPanic>(&runtime, &mut callbacks);

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
            let mut callbacks = HostCallbackSession::new();
            close_addin_inner::<QuiesceFailure>(&runtime, &mut callbacks);
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
        let mut callbacks = HostCallbackSession::new();
        let outcome = rollback_open::<RetryClose>(&runtime, &mut callbacks);
        assert!(outcome.unload_safe());
        assert!(outcome.finalized);
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

    struct TraceCleanup;

    impl Addin for TraceCleanup {
        type State = ();
        type Error = XllError;

        fn open(_context: &OpenContext) -> Result<Self::State, Self::Error> {
            unreachable!()
        }

        fn cleanup(_state: &mut Self::State, reporter: &mut crate::CleanupReporter<'_>) {
            reporter.warn(
                "Lean checker cleanup trace",
                crate::CleanupIssueKind::RegistryCleanup,
                XllError::Internal {
                    diagnostic_id: 0x4c45_414e_5452_4345,
                },
            );
        }
    }

    struct TraceHandle;

    impl crate::ExcelHandleObject for TraceHandle {}

    struct TraceSubscription;

    // SAFETY: this test subscription has no background work to wait for.
    unsafe impl crate::RtdSubscription for TraceSubscription {
        fn request_cancel(&self) {}

        fn disconnect_and_wait(self: Box<Self>) -> XllResult<()> {
            Ok(())
        }
    }

    struct TraceSource {
        sink: std::sync::Arc<std::sync::Mutex<Option<crate::RtdSink<f64>>>>,
    }

    impl crate::RtdSource for TraceSource {
        type Value = f64;

        fn subscribe(
            &self,
            _topic: &crate::RtdTopic,
            sink: crate::RtdSink<Self::Value>,
        ) -> XllResult<Box<dyn crate::RtdSubscription>> {
            self.sink.lock().unwrap().replace(sink);
            Ok(Box::new(TraceSubscription))
        }
    }

    struct TraceDiagnosticSink;

    impl crate::DiagnosticSink for TraceDiagnosticSink {
        fn report(&self, _event: &crate::DiagnosticEvent<'_>) {}
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
                "Lean checker rejected {label} Rust trace: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        };

        let runtime = Runtime::new();
        let mut opening = runtime.begin_open().unwrap();
        runtime.publish((), Vec::new());
        runtime.finish_open(&mut opening, Vec::new()).unwrap();

        crate::diagnostics::reset_diagnostic_router().unwrap();
        crate::set_diagnostic_sink(TraceDiagnosticSink).unwrap();
        crate::diagnostics::report_no_unwind("lean_checker_trace", &XllError::Panic);

        let pointer = crate::ffi_boundary(&runtime, || Ok::<f64, XllError>(1.0));
        // SAFETY: `pointer` is the live DLL-owned block returned by the
        // framework boundary above and is freed exactly once here.
        let free = unsafe { crate::free_return_boundary(pointer) };
        drop(free);

        let handles = runtime.handles().unwrap();
        handles
            .prepare(crate::handle::test_topic_key("lean-checker-handle"), || {
                Ok(std::sync::Arc::new(TraceHandle))
            })
            .unwrap();

        let callback_count = std::sync::Arc::new(AtomicUsize::new(0));
        let subscriptions = runtime.subscriptions();
        let server = subscriptions
            .register_server(crate::subscription::ServerGeneration(1))
            .unwrap();
        server
            .attach_update_callback({
                let callback_count = std::sync::Arc::clone(&callback_count);
                std::sync::Arc::new(move || {
                    callback_count.fetch_add(1, Ordering::AcqRel);
                    Ok(())
                })
            })
            .unwrap();
        let trace_sink = std::sync::Arc::new(std::sync::Mutex::new(None));
        let prepared = subscriptions
            .prepare(
                std::sync::Arc::new(TraceSource {
                    sink: std::sync::Arc::clone(&trace_sink),
                }),
                crate::RtdTopic::single("lean-checker-subscription").unwrap(),
            )
            .unwrap();
        let key = prepared.key().clone();
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
        assert_eq!(callback_count.load(Ordering::Acquire), 1);

        #[cfg(feature = "async")]
        {
            let (done_tx, done_rx) = std::sync::mpsc::channel();
            let (cancellation, _token) = crate::cancellation::CancellationSource::new(
                crate::CancellationGuarantee::BestEffort,
            );
            runtime.start_async(1).unwrap();
            let call = runtime
                .enter()
                .expect("async trace task must be spawned from an admitted call");
            runtime
                .async_manager()
                .spawn(
                    runtime.generation(),
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

        assert_eq!(close_addin::<TraceCleanup>(&runtime), 1);
        let trace = runtime.ghost_trace_json();
        for event in [
            "enterExternal",
            "leaveExternal",
            "enterCall",
            "leaveCall",
            "createReturnBlock",
            "beginReturnFree",
            "releaseReturnBlock",
            "endReturnFree",
            "beginHandleOperation",
            "endHandleOperation",
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
        let clean_runtime = Runtime::new();
        let mut opening = clean_runtime.begin_open().unwrap();
        clean_runtime.publish((), Vec::new());
        clean_runtime.finish_open(&mut opening, Vec::new()).unwrap();
        assert_eq!(close_addin::<CleanClose>(&clean_runtime), 1);
        check("clean", clean_runtime.ghost_trace_json());

        let failure_runtime = Runtime::new();
        let drops = std::sync::Arc::new(AtomicUsize::new(0));
        let mut opening = failure_runtime.begin_open().unwrap();
        failure_runtime.publish(DropObserved(std::sync::Arc::clone(&drops)), Vec::new());
        failure_runtime
            .finish_open(&mut opening, Vec::new())
            .unwrap();
        let failure = std::panic::catch_unwind(AssertUnwindSafe(|| {
            close_addin::<QuiesceFailure>(&failure_runtime)
        }));
        assert!(failure.is_err(), "quiesce failure must fail-stop close");
        let failure_trace = failure_runtime.ghost_trace_json();
        assert!(failure_trace.contains("fail_stopped"));
        check("fail-stop", failure_trace);
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
        let runtime = Runtime::new();
        let mut opening = runtime.begin_open().unwrap();
        runtime.publish((), Vec::new());
        runtime.finish_open(&mut opening, Vec::new()).unwrap();
        crate::diagnostics::reset_diagnostic_router().unwrap();
        crate::set_diagnostic_sink(TraceDiagnosticSink).unwrap();
        crate::diagnostics::report_no_unwind("composition_checker_trace", &XllError::Panic);

        assert_eq!(close_addin::<CleanClose>(&runtime), 1);
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
    #[ignore = "requires XLFN_COMPOSITION_CHECKER to point to the Lean executable"]
    fn rust_composition_owner_takeover_trace_is_accepted_by_lean_checker() {
        use std::io::Write;
        use std::process::{Command, Stdio};

        let _test_guard = COMPOSITION_TRACE_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let checker = std::env::var_os("XLFN_COMPOSITION_CHECKER")
            .expect("XLFN_COMPOSITION_CHECKER must point to composition_trace_checker");
        let runtime = Runtime::new();
        let mut opening = runtime.begin_open().unwrap();
        runtime.publish((), Vec::new());
        runtime.finish_open(&mut opening, Vec::new()).unwrap();
        crate::diagnostics::reset_diagnostic_router().unwrap();
        crate::set_diagnostic_sink(TraceDiagnosticSink).unwrap();
        crate::diagnostics::report_no_unwind("composition_takeover_trace", &XllError::Panic);

        let first = runtime.begin_final_close().unwrap();
        drop(first);
        let second = runtime.begin_final_close().unwrap();
        drop(second);

        assert_eq!(close_addin::<CleanClose>(&runtime), 1);
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
            "Lean checker rejected Rust composition takeover trace: {}\n{}",
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
                "Lean checker rejected {label} Rust composition trace: {}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        };

        let uncommitted = std::sync::Arc::new(Runtime::new());
        let mut opening = uncommitted.begin_open().unwrap();
        let closing_runtime = std::sync::Arc::clone(&uncommitted);
        let (owner_tx, owner_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let close_waiter = std::thread::spawn(move || {
            let close_attempt = closing_runtime
                .begin_final_close()
                .expect("final close must acquire after open rejection");
            owner_tx.send(()).expect("final close owner signal");
            release_rx.recv().expect("final close release signal");
            drop(close_attempt);
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        while uncommitted.phase() != crate::LifecyclePhase::Closing
            && std::time::Instant::now() < deadline
        {
            std::thread::yield_now();
        }
        assert_eq!(uncommitted.phase(), crate::LifecyclePhase::Closing);
        assert!(uncommitted.finish_open(&mut opening, Vec::new()).is_err());
        owner_rx.recv().expect("final close owner was not acquired");
        release_tx.send(()).expect("final close release signal");
        close_waiter.join().expect("final close waiter panicked");
        assert_eq!(close_addin::<CleanClose>(&uncommitted), 1);
        check(
            "uncommitted final close",
            uncommitted.composition_trace_json(),
            "rust-composition-uncommitted-trace.json",
        );

        let rollback = Runtime::new();
        let mut opening = rollback.begin_open().unwrap();
        assert!(opening.fail());
        let mut callbacks = HostCallbackSession::new();
        let outcome = rollback_open::<CleanClose>(&rollback, &mut callbacks);
        assert!(outcome.unload_safe());
        check(
            "open rollback",
            rollback.composition_trace_json(),
            "rust-composition-open-rollback-trace.json",
        );
    }

    #[test]
    fn close_owner_is_held_until_the_success_boundary_finishes() {
        let runtime = Runtime::new();
        let mut opening = runtime.begin_open().unwrap();
        runtime.publish((), Vec::new());
        runtime.finish_open(&mut opening, Vec::new()).unwrap();

        let success = close_addin_inner::<CleanClose>(&runtime, &mut HostCallbackSession::new());
        assert!(runtime.begin_open().is_err());
        let CloseSuccess::Closed {
            witness,
            close_attempt,
        } = success
        else {
            panic!("test close must own the close attempt");
        };
        runtime.record_ghost_returned_success(witness).unwrap();
        drop(close_attempt);

        let mut reopened = runtime.begin_open().unwrap();
        runtime.publish((), Vec::new());
        runtime.finish_open(&mut reopened, Vec::new()).unwrap();
        assert_eq!(runtime.phase(), crate::LifecyclePhase::Open);
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
            .prepare(crate::handle::test_topic_key("state-owned"), || {
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
        let mut callbacks = HostCallbackSession::new();
        assert!(rollback_open::<StateHandleRollback>(&runtime, &mut callbacks).unload_safe());
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
        let mut callbacks = HostCallbackSession::new();
        assert!(rollback_open::<AlwaysFailClose>(&runtime, &mut callbacks).unload_safe());
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
        let (_export_guard, accepted, _concurrent_calls) =
            crate::ingress::global_ingress().enter_udf_with(|| {});
        assert!(accepted);
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
        drop(_export_guard);
        assert_eq!(
            closed_rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap(),
            1
        );
        closer.join().unwrap();
        assert_eq!(runtime.phase(), crate::LifecyclePhase::Closed);
    }

    #[cfg(all(feature = "async", not(target_os = "windows")))]
    #[test]
    fn close_stops_async_before_terminal_unregister_and_never_calls_excel_afterwards() {
        use std::panic::AssertUnwindSafe;
        use xlfn_sys::{
            XL_ASYNC_RETURN, XL_FREE, XLF_UNREGISTER, XLOPER12, XLOPER12BigData,
            XLOPER12BigDataHandle, XLOPER12Value, XLRET_ABORT, XLTYPE_BIG_DATA,
        };

        let runtime = Box::leak(Box::new(Runtime::new()));
        let mut open_attempt = runtime.begin_open().unwrap();
        runtime.publish((), Vec::new());
        runtime.finish_open(&mut open_attempt, Vec::new()).unwrap();
        runtime.start_async(1).unwrap();
        runtime.retain_registration_debt(vec![
            crate::RegistrationId {
                id: 1.0,
                excel_name: "TEST.CLOSE.ORDER",
            }
            .into(),
        ]);

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
                move |_, _| {
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
        let close = std::panic::catch_unwind(AssertUnwindSafe(|| {
            close_addin_inner::<CleanClose>(runtime, &mut HostCallbackSession::new());
        }));

        assert!(close.is_err(), "terminal unregister must fail-stop unload");
        assert_eq!(
            crate::test_callback::callback_order(),
            vec![XL_ASYNC_RETURN, XLF_UNREGISTER, XL_FREE]
        );
        assert_eq!(crate::test_callback::async_return_calls(), 1);
        assert_eq!(crate::test_callback::total_calls(), 3);
        // The test intentionally stops at the fail-stop boundary after a
        // terminal unregister. Restore the RTD test epoch for later cases;
        // ingress is already sealed and must remain sealed until the next
        // Runtime::begin_open.
        crate::rtd::begin_module_open();
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
            .prepare(crate::handle::test_topic_key("ordered"), || {
                Ok(std::sync::Arc::new(OrderedHandle {
                    events: std::sync::Arc::clone(&events),
                }))
            })
            .unwrap();
        let subscriptions = runtime.subscriptions();
        let server = subscriptions
            .register_server(crate::subscription::ServerGeneration(1))
            .unwrap();
        let prepared = subscriptions
            .prepare(
                std::sync::Arc::new(OrderedSource {
                    events: std::sync::Arc::clone(&events),
                }),
                crate::RtdTopic::single("ordered").unwrap(),
            )
            .unwrap();
        let key = prepared.key().clone();
        prepared.commit();
        let conn = subscriptions
            .connect_transaction(&server, crate::subscription::TopicId(1), &key)
            .unwrap();
        conn.commit().unwrap();
        drop(server);

        assert_eq!(close_addin::<OrderedClose>(&runtime), 1);
        assert_eq!(*events.lock().unwrap(), ["subscription", "handle", "state"]);
    }
}

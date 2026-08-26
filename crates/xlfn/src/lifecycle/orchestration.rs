use crate::XllError;
use crate::addin::{Addin, BuildInfo, OpenContext, RuntimeConfig};
use crate::error::IntoXllError;
use crate::generation::RuntimeGeneration;
use crate::host_callback::HostCallbackSession;
use crate::lifecycle::{
    ClosedWitness, FinalRemoval, HostOpeningState, OpenAttemptBegun, OpenGenerationStaged,
    OpeningTxn, RemovalOwner,
};
use crate::registration::HostRegistrar;
use crate::registration::RegistrationDescriptor;
use crate::runtime::{AddinLifecycleAccess, Runtime};
use std::panic::{AssertUnwindSafe, catch_unwind};

use super::open;
use super::rollback::{active_runtime_generation, rollback_open};
use super::teardown::{self, drain_execution};
use super::{
    commit_removal_control, handle_unload_hazard, lifecycle_access_error, quarantine_runtime,
    report_boundary_error, report_cleanup_issue,
};

type StagedOpenResult<'runtime, A> = Result<
    (
        OpeningTxn<'runtime, A, OpenGenerationStaged, HostOpeningState>,
        Vec<crate::registration::RegistrationId>,
    ),
    open::OpenFailure<'runtime, A>,
>;

type InitializedOpenResult<'runtime, A> = Result<
    (
        OpeningTxn<'runtime, A, OpenGenerationStaged, HostOpeningState>,
        RuntimeConfig,
    ),
    open::OpenFailure<'runtime, A>,
>;

pub(super) fn open_addin_inner<'runtime, A>(
    runtime: &'runtime Runtime<A>,
    build_info: BuildInfo,
    descriptors: &[RegistrationDescriptor],
    mut transaction: OpeningTxn<'runtime, A, OpenAttemptBegun, HostOpeningState>,
) -> StagedOpenResult<'runtime, A>
where
    A: Addin,
{
    #[cfg(test)]
    let _diagnostic_test_guard = crate::diagnostics::DIAGNOSTIC_TEST_MUTEX.lock();
    if let Err(error) = crate::diagnostics::reset_diagnostic_router() {
        return Err(transaction.failure(error));
    }
    let prepared_set = match crate::registration::preflight_registration(descriptors) {
        Ok(prepared_set) => prepared_set,
        Err(error) => return Err(transaction.failure(error)),
    };
    let registrar = match HostRegistrar::connect(transaction.callbacks_mut()) {
        Ok(registrar) => registrar,
        Err(error) => {
            let error = retain_transaction_error(&mut transaction, error);
            return Err(transaction.failure(error));
        }
    };
    let generation = runtime.protocol_generation().ok_or(XllError::Internal {
        diagnostic_id: crate::diagnostics::id::DiagnosticId::OPEN_STATE,
    });
    let generation = match generation {
        Ok(generation) => generation,
        Err(error) => return Err(transaction.failure(error)),
    };
    let context = OpenContext::new(registrar.module_path().clone(), build_info, generation);
    let (mut transaction, runtime_config) = initialize_addin::<A>(&context, transaction)?;
    #[cfg(not(feature = "async"))]
    let _ = runtime_config;
    let has_async_functions = prepared_set
        .iter()
        .any(|descriptor| descriptor.signature.execution.is_async());
    if has_async_functions {
        #[cfg(feature = "async")]
        {
            if let Err(error) = runtime.start_async(runtime_config.async_worker_count()) {
                return Err(transaction.failure(error));
            }
            match registrar.register_async_events(transaction.callbacks_mut()) {
                Ok(events) => transaction.stage_events(events),
                Err(error) => {
                    let error = retain_transaction_error(&mut transaction, error);
                    return Err(transaction.failure(error));
                }
            }
        }
        #[cfg(not(feature = "async"))]
        {
            return Err(transaction.failure(XllError::Internal {
                diagnostic_id: crate::diagnostics::id::DiagnosticId::ASYNC_FEATURE,
            }));
        }
    }
    let registrations = match registrar.register_all(transaction.callbacks_mut(), &prepared_set) {
        Ok(registrations) => registrations,
        Err(error) => {
            let error = retain_transaction_error(&mut transaction, error);
            return Err(transaction.failure(error));
        }
    };
    Ok((transaction, registrations))
}

pub(super) fn rollback_active_open<'runtime, A, Stage>(
    lifecycle: &AddinLifecycleAccess<'_, A>,
    attempt: Option<OpeningTxn<'runtime, A, Stage, HostOpeningState>>,
) where
    A: Addin,
{
    let Some(mut attempt) = attempt else {
        return;
    };
    let runtime = attempt.runtime();
    if let Some(lifecycle_state) = attempt.take_lifecycle_state()
        && let Err(error) = runtime.install_addin_lifecycle(lifecycle, lifecycle_state)
    {
        let (lifecycle_state, reason) = error.into_parts();
        #[allow(
            clippy::mem_forget,
            reason = "failed rollback installation; leaking untrusted lifecycle state is safer than running its destructor"
        )]
        std::mem::forget(lifecycle_state);
        let error = lifecycle_access_error(reason);
        report_boundary_error("xlAutoOpen rollback lifecycle installation", &error);
        quarantine_runtime(runtime);
        return;
    }
    runtime.retain_host_mutations(attempt.take_journal());
    let generation = Some(
        RuntimeGeneration::new(attempt.attempt_id().get())
            .expect("an active open attempt has a runtime generation"),
    );
    if attempt.fail().requires_rollback() {
        match catch_unwind(AssertUnwindSafe(|| {
            rollback_open::<A>(runtime, lifecycle, attempt.callbacks_mut(), generation)
        })) {
            Ok(outcome) if outcome.unload_safe() => {}
            Ok(_) => {
                let error = XllError::Internal {
                    diagnostic_id: crate::diagnostics::id::DiagnosticId::OPEN_ROLLBACK_FAILURE,
                };
                report_boundary_error("xlAutoOpen rollback", &error);
                quarantine_runtime(runtime);
            }
            Err(_) => {
                report_boundary_error("xlAutoOpen rollback", &XllError::Panic);
                quarantine_runtime(runtime);
            }
        }
    }
}

pub(super) fn initialize_addin<'runtime, A>(
    context: &OpenContext,
    transaction: OpeningTxn<'runtime, A, OpenAttemptBegun, HostOpeningState>,
) -> InitializedOpenResult<'runtime, A>
where
    A: Addin,
{
    let opened = match A::open(context) {
        Ok(opened) => opened,
        Err(error) => {
            return Err(transaction.failure(IntoXllError::into_xll_error(error)));
        }
    };
    let (shared_state, lifecycle_state, layers, runtime_config) = opened.into_parts();
    // Keep the non-Send lifecycle state in the open transaction until the
    // final pre-publication transfer into the thread-affine slot. It must not
    // become part of the cross-thread generation root.
    let transaction = transaction.with_lifecycle_state(lifecycle_state);
    let opening = crate::generation::OpeningGeneration {
        shared_state,
        layers,
        init_config: runtime_config,
    };
    let transaction = transaction.stage_generation(opening)?;
    Ok((transaction, runtime_config))
}

fn retain_transaction_error<A: Addin, Stage>(
    transaction: &mut OpeningTxn<'_, A, Stage, HostOpeningState>,
    error: crate::registration::RegistrationTransactionError,
) -> XllError {
    if error.journal.is_unknown() {
        for unknown in &error.journal.unknown_registrations {
            let recovery_error = unknown.recovery_error.clone();
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
    let source = *error.source;
    transaction.retain_journal(error.journal);
    source
}

#[must_use]
pub(super) fn remove_addin<A>(runtime: &Runtime<A>, lifecycle: &AddinLifecycleAccess<'_, A>) -> i32
where
    A: Addin,
{
    let close_result = catch_unwind(AssertUnwindSafe(|| {
        remove_addin_inner::<A>(runtime, lifecycle)
    }));
    let success = match close_result {
        Ok(success) => success,
        Err(_) => {
            let error = XllError::Panic;
            report_boundary_error("xlAutoRemove boundary", &error);
            quarantine_runtime(runtime);
            return 1;
        }
    };
    match success {
        RemovalSuccess::AlreadyClosed => {
            if runtime.phase() == crate::lifecycle::LifecyclePhase::Closed
                && let Err(error) = runtime.release_empty_addin_lifecycle(lifecycle)
            {
                let error = lifecycle_access_error(error);
                report_boundary_error("xlAutoRemove closed lifecycle binding", &error);
                quarantine_runtime(runtime);
            }
            #[cfg(any(test, feature = "refinement"))]
            runtime.record_composition_already_closed_return();
            1
        }
        RemovalSuccess::Quarantined => 1,
        #[cfg(not(any(test, feature = "refinement")))]
        RemovalSuccess::Closed {
            witness: _witness,
            removal_attempt: _removal_attempt,
        } => 1,
        #[cfg(any(test, feature = "refinement"))]
        RemovalSuccess::Closed {
            witness,
            removal_attempt: _removal_attempt,
        } => {
            runtime
                .record_returned_success(witness)
                .unwrap_or_else(|error| {
                    let control = handle_unload_hazard(
                        runtime,
                        crate::shutdown::UnloadHazard::CloseInvariantViolation,
                        "xlAutoRemove success refinement",
                        &error,
                    );
                    let _ = commit_removal_control(runtime, control);
                });
            1
        }
    }
}

pub(super) enum RemovalSuccess<'runtime, A: Addin> {
    AlreadyClosed,
    Quarantined,
    Closed {
        witness: ClosedWitness,
        removal_attempt: RemovalOwner<'runtime, A>,
    },
}

pub(super) enum RemovalControl {
    Quarantine {
        hazard: crate::shutdown::UnloadHazard,
        boundary: &'static str,
        error: XllError,
    },
}

/// Owns the terminal-removal attempt and its callback session. Cleanup is
/// explicit: the transaction is consumed only after a close certificate is
/// produced, while an active drop can only preserve quarantine.
struct RemovalTransaction<'runtime, A: Addin> {
    runtime: &'runtime Runtime<A>,
    callbacks: HostCallbackSession,
    attempt: Option<RemovalOwner<'runtime, A>>,
}

impl<'runtime, A: Addin> RemovalTransaction<'runtime, A> {
    fn begin(runtime: &'runtime Runtime<A>) -> Option<Self> {
        Some(Self {
            runtime,
            callbacks: HostCallbackSession::new(),
            attempt: Some(runtime.lifecycle_runtime().begin_final_removal()?),
        })
    }

    fn callbacks(&self) -> &HostCallbackSession {
        &self.callbacks
    }

    fn callbacks_mut(&mut self) -> &mut HostCallbackSession {
        &mut self.callbacks
    }

    fn take_attempt(&mut self) -> RemovalOwner<'runtime, A> {
        self.attempt
            .take()
            .expect("a removal transaction always owns its attempt")
    }
}

impl<A: Addin> Drop for RemovalTransaction<'_, A> {
    fn drop(&mut self) {
        if self.attempt.is_some() {
            // No callback or partial cleanup is legal from Drop. The runtime
            // remains terminally quarantined until an explicit boundary can
            // account for every outstanding resource.
            self.runtime.lifecycle_runtime().quarantine();
        }
    }
}

pub(super) fn remove_addin_inner<'runtime, A>(
    runtime: &'runtime Runtime<A>,
    lifecycle: &AddinLifecycleAccess<'_, A>,
) -> RemovalSuccess<'runtime, A>
where
    A: Addin,
{
    match catch_unwind(AssertUnwindSafe(|| {
        remove_addin_inner_unchecked::<A>(runtime, lifecycle)
    })) {
        Ok(Ok(success)) => success,
        Ok(Err(control)) => commit_removal_control(runtime, control),
        Err(payload) => std::panic::resume_unwind(payload),
    }
}

pub(super) fn remove_addin_inner_unchecked<'runtime, A>(
    runtime: &'runtime Runtime<A>,
    lifecycle: &AddinLifecycleAccess<'_, A>,
) -> Result<RemovalSuccess<'runtime, A>, RemovalControl>
where
    A: Addin,
{
    #[cfg(test)]
    let _diagnostic_test_guard = crate::diagnostics::DIAGNOSTIC_TEST_MUTEX.lock();
    // Even an apparently closed runtime must pass through begin_final_removal:
    // a concurrent xlAutoOpen may already have sampled the previous close
    // epoch without having acquired its open-attempt token yet.
    let Some(mut transaction) = RemovalTransaction::begin(runtime) else {
        return Ok(RemovalSuccess::AlreadyClosed);
    };
    #[cfg(any(test, feature = "refinement"))]
    runtime.refinement_hooks().begin_close(runtime);

    let mut report = crate::shutdown::CloseReport::default();
    let mut unload_failure: Option<(crate::shutdown::UnloadHazard, &'static str, XllError)> = None;
    let lifecycle_present = match runtime.has_addin_lifecycle(lifecycle) {
        Ok(present) => present,
        Err(error) => {
            let error = lifecycle_access_error(error);
            return Err(handle_unload_hazard(
                runtime,
                crate::shutdown::UnloadHazard::CloseInvariantViolation,
                "xlAutoRemove lifecycle slot",
                &error,
            ));
        }
    };

    let mut owner = transaction.take_attempt();
    let module_closing = owner.take_module_closing();
    let execution_drained = match drain_execution(runtime, module_closing, true) {
        Ok(stage) => stage,
        Err(error) => {
            return Err(handle_unload_hazard(
                runtime,
                crate::shutdown::UnloadHazard::CloseInvariantViolation,
                "xlAutoRemove return quiescence",
                &error,
            ));
        }
    };
    let teardown: teardown::TeardownTxn<'runtime, A, FinalRemoval, teardown::ExecutionDrained> =
        teardown::TeardownTxn::new(owner, execution_drained);
    let teardown = match teardown.stop_producers(|issue| {
        report.push(issue.component, issue.kind, issue.error.clone());
    }) {
        Ok(stage) => stage,
        Err(error) => {
            return Err(handle_unload_hazard(
                runtime,
                crate::shutdown::UnloadHazard::SubscriptionProducerStillRunning,
                "xlAutoRemove subscription shutdown",
                &error,
            ));
        }
    };

    #[cfg(any(test, feature = "refinement"))]
    runtime.refinement_hooks().async_drained(runtime);

    #[cfg(any(test, feature = "refinement"))]
    runtime.refinement_hooks().subscriptions_drained(runtime);

    let registrations = runtime.registrations();
    if let Ok(outcome) = catch_unwind(AssertUnwindSafe(|| {
        HostRegistrar::unregister_pending(transaction.callbacks_mut(), &registrations)
    })) {
        for (registration, error) in &outcome.failed {
            if registration.cleanup_severity().is_unload_unsafe() {
                report_boundary_error("xlAutoRemove unregister", error);
                if unload_failure.is_none() {
                    unload_failure = Some((
                        crate::shutdown::UnloadHazard::HostCallbackStillRegistered,
                        "xlAutoRemove unregister",
                        error.clone(),
                    ));
                }
            }
        }
        for debt in &outcome.metadata_debt {
            report.push(
                "Excel registered name",
                crate::shutdown::CleanupIssueKind::HostMetadata,
                debt.last_error().clone(),
            );
        }
        for error in outcome.cleanup_issues {
            report.push(
                "Excel callback result",
                crate::shutdown::CleanupIssueKind::HostMemoryLeak,
                error,
            );
        }
        #[cfg(any(test, feature = "refinement"))]
        for _ in &outcome.succeeded {
            runtime.refinement_hooks().unregister_function(runtime);
        }
        runtime.retain_failed_registrations(outcome.failed);
        runtime.retain_metadata_debt(outcome.metadata_debt);
        if runtime.has_metadata_debt() {
            let debt_count = runtime.metadata_debt().len();
            let _ = catch_unwind(AssertUnwindSafe(|| {
                tracing::warn!(
                    count = debt_count,
                    "xlAutoRemove completed with host metadata debt"
                );
            }));
        }
    } else {
        let error = XllError::Panic;
        report_boundary_error("xlAutoRemove unregister", &error);
        unload_failure = Some((
            crate::shutdown::UnloadHazard::HostCallbackStillRegistered,
            "xlAutoRemove unregister",
            error,
        ));
    }
    if runtime.registration_state_unknown() && unload_failure.is_none() {
        let error = XllError::Internal {
            diagnostic_id: crate::diagnostics::id::DiagnosticId::REGISTRATION_UNKNOWN,
        };
        report_boundary_error("xlAutoRemove registration state unknown", &error);
        unload_failure = Some((
            crate::shutdown::UnloadHazard::RegistrationStateUnknown,
            "xlAutoRemove registration state unknown",
            error,
        ));
    }

    let event_registrations = runtime.event_registrations();
    if !transaction.callbacks().permits_callbacks() {
        if !event_registrations.is_empty() {
            let error = transaction
                .callbacks()
                .terminal_status()
                .map(|status| XllError::ExcelApi {
                    function: crate::error::ExcelApiFunction::EventRegister,
                    failure: crate::error::ExcelApiFailure::Suppressed(status),
                })
                .unwrap_or(XllError::Closing);
            for _ in &event_registrations {
                report_boundary_error("xlAutoRemove event unregister", &error);
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
                    "xlAutoRemove event unregister suppressed",
                    error,
                ));
            }
        }
    } else if let Ok(event_outcome) = catch_unwind(AssertUnwindSafe(|| {
        HostRegistrar::unregister_events_detailed(transaction.callbacks_mut(), &event_registrations)
    })) {
        for (_, error) in &event_outcome.failed {
            report_boundary_error("xlAutoRemove event unregister", error);
            if unload_failure.is_none() {
                unload_failure = Some((
                    crate::shutdown::UnloadHazard::HostCallbackStillRegistered,
                    "xlAutoRemove event unregister",
                    error.clone(),
                ));
            }
        }
        for error in event_outcome.cleanup_issues {
            report.push(
                "Excel event callback result",
                crate::shutdown::CleanupIssueKind::HostMemoryLeak,
                error,
            );
        }
        #[cfg(any(test, feature = "refinement"))]
        for _ in &event_outcome.succeeded {
            runtime.refinement_hooks().unregister_event(runtime);
        }
        runtime.retain_failed_event_registrations(event_outcome.failed);
    } else {
        let error = XllError::Panic;
        report_boundary_error("xlAutoRemove event unregister", &error);
        unload_failure = Some((
            crate::shutdown::UnloadHazard::HostCallbackStillRegistered,
            "xlAutoRemove event unregister",
            error,
        ));
    }

    // Host callbacks are now the final direct Excel C API operations in this
    // lifecycle. A terminal result transitions the module gate immediately;
    // once unregistering is complete, close it unconditionally so all later
    // cleanup is provably callback-free.
    teardown.close_module_callbacks();

    #[cfg(any(test, feature = "refinement"))]
    runtime
        .refinement_hooks()
        .callback_admission_closed(runtime);

    if let Some((hazard, boundary, error)) = unload_failure.take() {
        return Err(handle_unload_hazard(runtime, hazard, boundary, &error));
    }

    let host_callbacks = crate::shutdown::HostCallbacksDetached::issue();
    #[cfg(any(test, feature = "refinement"))]
    runtime.refinement_hooks().host_detached(runtime);

    let addin = if let Some(generation) = runtime.take_generation_for_shutdown() {
        match generation {
            crate::generation::ShutdownGeneration::Open(generation) => {
                match std::sync::Arc::try_unwrap(generation) {
                    Ok(mut generation) => {
                        let quiesce = catch_unwind(AssertUnwindSafe(|| {
                            runtime
                                .with_addin_lifecycle(lifecycle, |lifecycle_state| {
                                    runtime.quiesce_addin(
                                        &mut generation.shared_state,
                                        lifecycle_state,
                                    )
                                })
                                .map_err(lifecycle_access_error)
                        }))
                        .map_err(|_| XllError::Panic)
                        .and_then(|result| result)
                        .and_then(|result| result);
                        if let Err(error) = quiesce {
                            report_boundary_error("xlAutoRemove quiesce", &error);
                            runtime.lifecycle_runtime().quarantine_generation(
                                active_runtime_generation(runtime),
                                generation,
                                crate::runtime_components::QuarantineReason::AddinQuiesceFailed,
                            );
                            return Err(handle_unload_hazard(
                                runtime,
                                crate::shutdown::UnloadHazard::AddinQuiesceFailed,
                                "xlAutoRemove quiesce",
                                &error,
                            ));
                        }
                        drop(generation.layers);
                        teardown::QuiescedAddin::shared(
                            runtime,
                            active_runtime_generation(runtime),
                            generation.shared_state,
                        )
                    }
                    Err(generation) => {
                        let error = XllError::Internal {
                            diagnostic_id: crate::diagnostics::id::DiagnosticId::STATE_SCAN,
                        };
                        report_boundary_error("xlAutoRemove state escaped", &error);
                        runtime.lifecycle_runtime().quarantine_shared_generation(
                            active_runtime_generation(runtime),
                            generation,
                            crate::runtime_components::QuarantineReason::AddinGenerationEscaped,
                        );
                        return Err(handle_unload_hazard(
                            runtime,
                            crate::shutdown::UnloadHazard::AddinGenerationEscaped,
                            "xlAutoRemove state escaped",
                            &error,
                        ));
                    }
                }
            }
            crate::generation::ShutdownGeneration::Opening(opening) => {
                let (mut shared_state, layers, _config) = opening.into_parts();
                let quiesce = catch_unwind(AssertUnwindSafe(|| {
                    runtime
                        .with_addin_lifecycle(lifecycle, |lifecycle_state| {
                            runtime.quiesce_addin(&mut shared_state, lifecycle_state)
                        })
                        .map_err(lifecycle_access_error)
                }))
                .map_err(|_| XllError::Panic)
                .and_then(|result| result)
                .and_then(|result| result);
                if let Err(error) = quiesce {
                    report_boundary_error("xlAutoRemove quiesce", &error);
                    let generation_id = active_runtime_generation(runtime);
                    runtime.lifecycle_runtime().quarantine_layers(
                        generation_id,
                        layers,
                        crate::runtime_components::QuarantineReason::AddinQuiesceFailed,
                    );
                    runtime.lifecycle_runtime().quarantine_shared_state(
                        generation_id,
                        shared_state,
                        crate::runtime_components::QuarantineReason::AddinQuiesceFailed,
                    );
                    return Err(handle_unload_hazard(
                        runtime,
                        crate::shutdown::UnloadHazard::AddinQuiesceFailed,
                        "xlAutoRemove quiesce",
                        &error,
                    ));
                }
                drop(layers);
                teardown::QuiescedAddin::shared(
                    runtime,
                    active_runtime_generation(runtime),
                    shared_state,
                )
            }
        }
    } else {
        if lifecycle_present {
            let error = XllError::Internal {
                diagnostic_id: crate::diagnostics::id::DiagnosticId::LIFECYCLE_SLOT,
            };
            return Err(handle_unload_hazard(
                runtime,
                crate::shutdown::UnloadHazard::CloseInvariantViolation,
                "xlAutoRemove lifecycle state",
                &error,
            ));
        }
        teardown::QuiescedAddin::empty(runtime, active_runtime_generation(runtime))
    };

    #[cfg(any(test, feature = "refinement"))]
    {
        runtime.refinement_hooks().generation_unique(runtime);
        runtime.refinement_hooks().addin_quiesced(runtime);
        runtime.refinement_hooks().generation_reclaimed(runtime);
    }

    if let Err(error) = runtime.shutdown_handle_topics() {
        return Err(handle_unload_hazard(
            runtime,
            crate::shutdown::UnloadHazard::RtdGitCallbackStillRegistered,
            "xlAutoRemove RTD shutdown",
            &error,
        ));
    }

    let teardown = match teardown.seal_services(addin) {
        Ok(stage) => stage,
        Err(error) => {
            return Err(handle_unload_hazard(
                runtime,
                crate::shutdown::UnloadHazard::HandleStoreNotQuiescent,
                "xlAutoRemove handle table shutdown",
                &error,
            ));
        }
    };

    let teardown = match teardown.cleanup_addin(lifecycle, &mut report) {
        Ok(stage) => stage,
        Err(error) => {
            for issue in report.issues() {
                #[cfg(any(test, feature = "refinement"))]
                runtime.refinement_hooks().cleanup_issue(runtime);
                report_cleanup_issue(issue);
            }
            return Err(handle_unload_hazard(
                runtime,
                crate::shutdown::UnloadHazard::AddinCleanupFailed,
                "xlAutoRemove cleanup",
                &error,
            ));
        }
    };
    for issue in report.issues() {
        #[cfg(any(test, feature = "refinement"))]
        runtime.refinement_hooks().cleanup_issue(runtime);
        report_cleanup_issue(issue);
    }

    if let Some((hazard, boundary, error)) = unload_failure.take() {
        return Err(handle_unload_hazard(runtime, hazard, boundary, &error));
    }
    let teardown = match teardown.finish_services() {
        Ok(stage) => stage,
        Err(error) => {
            return Err(handle_unload_hazard(
                runtime,
                crate::shutdown::UnloadHazard::HandleStoreNotQuiescent,
                "xlAutoRemove handle pin quiescence",
                &error,
            ));
        }
    };

    #[cfg(any(test, feature = "refinement"))]
    runtime.refinement_hooks().handles_drained(runtime);

    #[cfg(any(test, feature = "refinement"))]
    let diagnostics_was_running = crate::diagnostics::diagnostic_sink_running();
    let diagnostics_stopped = match crate::diagnostics::close_diagnostic_router().map(|outcome| {
        for issue in outcome.issues {
            #[cfg(any(test, feature = "refinement"))]
            runtime.refinement_hooks().cleanup_issue(runtime);
            report_cleanup_issue(&issue);
        }
        outcome.certificate
    }) {
        Ok(certificate) => certificate,
        Err(error) => {
            let error = error.into_xll_error();
            return Err(handle_unload_hazard(
                runtime,
                crate::shutdown::UnloadHazard::DiagnosticWorkerStillRunning,
                "xlAutoRemove diagnostic refinement",
                &error,
            ));
        }
    };

    #[cfg(any(test, feature = "refinement"))]
    if diagnostics_was_running
        && let Err(error) = runtime.refinement_hooks().diagnostics_stopped(runtime)
    {
        return Err(handle_unload_hazard(
            runtime,
            crate::shutdown::UnloadHazard::DiagnosticWorkerStillRunning,
            "xlAutoRemove diagnostic refinement",
            &error,
        ));
    }
    #[cfg(any(test, feature = "refinement"))]
    runtime.refinement_hooks().diagnostics_drained(runtime);

    let rtd_quiescent = match crate::rtd::wait_for_module_quiescence() {
        Ok(certificate) => certificate,
        Err(error) => {
            let hazard = if error.revocation_debt != 0 {
                crate::shutdown::UnloadHazard::RtdGitRevocationDebt
            } else {
                crate::shutdown::UnloadHazard::RtdGitCallbackStillRegistered
            };
            return Err(handle_unload_hazard(
                runtime,
                hazard,
                "xlAutoRemove RTD GIT quiescence",
                &XllError::Internal {
                    diagnostic_id: crate::diagnostics::id::DiagnosticId::RTD_GIT_QUIESCENCE,
                },
            ));
        }
    };

    #[cfg(any(test, feature = "refinement"))]
    runtime.refinement_hooks().rtd_drained(runtime);

    let teardown = teardown.reclaim(rtd_quiescent, host_callbacks, diagnostics_stopped);
    let certificate = match teardown.certify() {
        Ok(certificate) => certificate,
        Err(error) => {
            return Err(handle_unload_hazard(
                runtime,
                crate::shutdown::UnloadHazard::CloseInvariantViolation,
                "xlAutoRemove certification",
                &error,
            ));
        }
    };
    let (closed_witness, removal_attempt) = match certificate.finish() {
        Ok(result) => result,
        Err((error, _certificate)) => {
            return Err(handle_unload_hazard(
                runtime,
                crate::shutdown::UnloadHazard::CloseInvariantViolation,
                "xlAutoRemove removal completion",
                &error,
            ));
        }
    };

    if let Err(error) = runtime.release_empty_addin_lifecycle(lifecycle) {
        let error = lifecycle_access_error(error);
        return Err(handle_unload_hazard(
            runtime,
            crate::shutdown::UnloadHazard::CloseInvariantViolation,
            "xlAutoRemove lifecycle binding release",
            &error,
        ));
    }

    Ok(RemovalSuccess::Closed {
        witness: closed_witness,
        removal_attempt,
    })
}

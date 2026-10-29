use crate::XllError;
use crate::addin::{Addin, BuildInfo, OpenContext, RuntimeConfig};
use crate::error::IntoXllError;
use crate::generation::RuntimeGeneration;
use crate::host_callback::HostCallbackSession;
use crate::panic_boundary::catch_no_unwind;
use crate::registration::RegistrationDescriptor;
use crate::registration::{HostRegistrar, RegistrationHost};
use crate::runtime::capabilities::ShutdownDeps;
use crate::runtime::open_txn::{
    GenerationStaged, HostAttached, OpeningState, OpeningTxn, RollbackState,
};
use crate::runtime::shutdown::{ClosedWitness, FinalRemoval, RemovalClaimGuard, RemovalOwner};
use crate::runtime::{AddinLifecycleAccess, Runtime};
use std::panic::AssertUnwindSafe;

use crate::boundary::{report_boundary_error, report_cleanup_issue};
use crate::lifecycle::lifecycle_access_error;
use crate::runtime::open;
use crate::runtime::recovery::{commit_removal_control, handle_unload_hazard, quarantine_runtime};
use crate::runtime::rollback::rollback_open;

type StagedOpenResult<'runtime, A> = Result<
    (
        OpeningTxn<'runtime, A, GenerationStaged<A>>,
        Vec<crate::registration::RegistrationId>,
    ),
    open::OpenFailure<'runtime, A>,
>;

type InitializedOpenResult<'runtime, A> = Result<
    (OpeningTxn<'runtime, A, GenerationStaged<A>>, RuntimeConfig),
    open::OpenFailure<'runtime, A>,
>;

pub(crate) fn open_addin_inner<'runtime, A>(
    runtime: &'runtime Runtime<A, impl crate::runtime::UnloadPolicy<A>>,
    build_info: BuildInfo,
    descriptors: &[RegistrationDescriptor],
    mut transaction: OpeningTxn<'runtime, A, HostAttached<A>>,
) -> StagedOpenResult<'runtime, A>
where
    A: Addin,
{
    #[cfg(test)]
    let _diagnostic_test_guard = crate::diagnostics::DIAGNOSTIC_TEST_MUTEX.lock();
    if let Err(error) = crate::diagnostics::reset_diagnostic_router() {
        return Err(transaction.failure(error));
    }
    #[cfg(all(windows, any(feature = "rtd", feature = "handles")))]
    if let Err(error) = crate::excel_rtd::register_server_type::<
        crate::addin::AddinSubscription<A>,
        crate::addin::AddinSources<A>,
    >() {
        return Err(transaction.failure(error));
    }
    let prepare = || crate::registration::preflight_registration(descriptors);
    #[cfg(feature = "lifecycle-diagnostics")]
    let prepared_result =
        crate::lifecycle_diagnostics::registration_preflight(descriptors.len(), prepare);
    #[cfg(not(feature = "lifecycle-diagnostics"))]
    let prepared_result = prepare();
    let prepared_set = match prepared_result {
        Ok(prepared_set) => prepared_set,
        Err(error) => return Err(transaction.failure(error)),
    };
    let registrar_result = {
        let host = RegistrationHost::new(transaction.callbacks_mut());
        HostRegistrar::connect(&host)
    };
    let registrar = match registrar_result {
        Ok(registrar) => registrar,
        Err(error) => {
            let error = retain_transaction_error(&mut transaction, error);
            return Err(transaction.failure(error));
        }
    };
    if runtime.host_intent() == crate::lifecycle::HostLifecycleIntent::ExplicitRemovalRequested {
        return Err(transaction.failure(XllError::Closing));
    }
    let generation = runtime.protocol_generation().ok_or(XllError::Internal {
        diagnostic_id: crate::diagnostics::id::DiagnosticId::OPEN_STATE,
    });
    let generation = match generation {
        Ok(generation) => generation,
        Err(error) => return Err(transaction.failure(error)),
    };
    let context = OpenContext::new(registrar.module_path().clone(), build_info, generation);
    let (mut transaction, _runtime_config) = initialize_addin::<A>(context, transaction)?;
    if runtime.host_intent() == crate::lifecycle::HostLifecycleIntent::ExplicitRemovalRequested {
        return Err(transaction.failure(XllError::Closing));
    }
    let has_async_functions = prepared_set
        .iter()
        .any(|descriptor| descriptor.signature.execution.is_async());
    if !prepared_set.as_slice().is_empty() {
        let event_result = {
            let host = RegistrationHost::new(transaction.callbacks_mut());
            runtime.ensure_calculation_events(&host, has_async_functions)
        };
        match event_result {
            Ok(()) => {}
            Err(error) => return Err(transaction.failure(error)),
        }
    }
    if runtime.host_intent() == crate::lifecycle::HostLifecycleIntent::ExplicitRemovalRequested {
        return Err(transaction.failure(XllError::Closing));
    }
    let registration_result = {
        let host = RegistrationHost::new(transaction.callbacks_mut());
        registrar.register_all(&host, &prepared_set)
    };
    let registrations = match registration_result {
        Ok(registrations) => registrations,
        Err(error) => {
            let error = retain_transaction_error(&mut transaction, error);
            return Err(transaction.failure(error));
        }
    };
    if runtime.host_intent() == crate::lifecycle::HostLifecycleIntent::ExplicitRemovalRequested {
        // A successful Excel mutation still belongs to this transaction when
        // a nested close requested rollback. Preserve every ID so recovery
        // unregisters it using the original callback session.
        transaction.retain_journal(crate::registration::HostMutationJournal {
            pending_registrations: registrations.into_iter().map(Into::into).collect(),
            ..Default::default()
        });
        return Err(transaction.failure(XllError::Closing));
    }
    Ok((transaction, registrations))
}

pub(crate) fn rollback_active_open<'runtime, A, S>(
    runtime: &'runtime Runtime<A, impl crate::runtime::UnloadPolicy<A>>,
    lifecycle: &AddinLifecycleAccess<'_, A>,
    attempt: Option<OpeningTxn<'runtime, A, S>>,
) where
    A: Addin,
    S: RollbackState<A>,
{
    let Some(attempt) = attempt else {
        return;
    };
    let (core, host, lifecycle_ownership) = attempt.into_rollback_parts().into_parts();
    let (callbacks, journal) = host.into_parts();
    let mut callbacks = callbacks;
    if let crate::runtime::open_txn::LifecycleOwnership::Owned(lifecycle_state) =
        lifecycle_ownership
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
        let closing = core.into_parts().1.rollback(|| {});
        runtime.lifecycle_control().complete_open_abort(closing);
        quarantine_runtime(runtime);
        return;
    }
    runtime.host.merge(journal);
    let (attempt_id, module_opening, _service_inputs) = core.into_parts();
    let generation = Some(
        RuntimeGeneration::new(attempt_id.get())
            .expect("an active open attempt has a runtime generation"),
    );
    let disposition = runtime
        .lifecycle_orchestrator()
        .mark_open_failed(attempt_id);
    runtime
        .lifecycle_control()
        .complete_open_abort(module_opening.rollback(|| {}));
    if disposition.requires_rollback() {
        match catch_no_unwind(AssertUnwindSafe(|| {
            rollback_open::<A>(runtime, lifecycle, &mut callbacks, generation)
        })) {
            Ok(outcome) if outcome.is_finalized() || outcome.cleanup_owned_elsewhere() => {}
            Ok(outcome) => {
                let error = XllError::Internal {
                    diagnostic_id: crate::diagnostics::id::DiagnosticId::OPEN_ROLLBACK_FAILURE,
                };
                report_boundary_error("xlAutoOpen rollback", &error);
                if let Some(hazard) = outcome.failure_hazard() {
                    super::recovery::quarantine_for_hazard(runtime, hazard);
                }
            }
            Err(_) => {
                report_boundary_error("xlAutoOpen rollback", &XllError::Panic);
                quarantine_runtime(runtime);
            }
        }
    }
}

pub(crate) fn initialize_addin<'runtime, A>(
    context: OpenContext<A>,
    transaction: OpeningTxn<'runtime, A, HostAttached<A>>,
) -> InitializedOpenResult<'runtime, A>
where
    A: Addin,
{
    let transaction = transaction.begin_initialization();
    let opened = match A::open(&context) {
        Ok(opened) => opened,
        Err(error) => {
            return Err(transaction.failure(IntoXllError::into_xll_error(error)));
        }
    };
    let (shared_state, lifecycle_state, layers, runtime_config, async_executor) =
        opened.into_parts();
    let service_inputs = context.into_service_inputs();
    // Keep the non-Send lifecycle state in the open transaction until the
    // final pre-publication transfer into the thread-affine slot. It must not
    // become part of the cross-thread generation root.
    let transaction = transaction.initialized(lifecycle_state, service_inputs);
    let opening = crate::generation::OpeningGeneration {
        shared_state,
        layers,
        init_config: runtime_config,
    };
    let transaction = transaction.stage_generation(opening)?;
    #[cfg(feature = "async")]
    if let Err(error) = transaction
        .deps()
        .async_runtime()
        .start(async_executor, runtime_config.async_task_limit())
    {
        return Err(transaction.failure(error));
    }
    #[cfg(not(feature = "async"))]
    let _ = async_executor;
    Ok((transaction, runtime_config))
}

fn retain_transaction_error<A: Addin, S>(
    transaction: &mut OpeningTxn<'_, A, S>,
    error: crate::registration::RegistrationTransactionError,
) -> XllError
where
    S: OpeningState<A> + crate::runtime::open_txn::HasHost,
{
    if error.journal.is_unknown() {
        for unknown in &error.journal.unknown_registrations {
            let recovery_error = unknown.recovery_error.clone();
            let _ = catch_no_unwind(AssertUnwindSafe(|| {
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
pub(crate) fn remove_addin<A>(
    runtime: &Runtime<A, impl crate::runtime::UnloadPolicy<A>>,
    lifecycle: &AddinLifecycleAccess<'_, A>,
    callbacks: &mut HostCallbackSession,
) -> i32
where
    A: Addin,
{
    let close_result = catch_no_unwind(AssertUnwindSafe(|| {
        remove_addin_inner::<A>(runtime, lifecycle, callbacks)
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
            runtime.record_composition_already_closed_return();
            1
        }
        RemovalSuccess::Quarantined => 1,
        RemovalSuccess::Closed {
            witness,
            removal_attempt: _removal_attempt,
        } => {
            runtime.record_returned_success(witness);
            1
        }
    }
}

pub(crate) enum RemovalSuccess<'runtime, A: Addin> {
    AlreadyClosed,
    Quarantined,
    Closed {
        witness: ClosedWitness,
        removal_attempt: RemovalClaimGuard<'runtime, A>,
    },
}

pub(crate) enum RemovalControl {
    Quarantine {
        hazard: crate::shutdown::UnloadHazard,
        boundary: &'static str,
        error: XllError,
    },
}

/// Owns the terminal-removal attempt and borrows its Excel-entry session. Cleanup is
/// explicit: the transaction is consumed only after a close certificate is
/// produced, while an active drop can only preserve quarantine.
struct RemovalTransaction<'runtime, 'call, A: Addin> {
    deps: ShutdownDeps<'runtime, A>,
    callbacks: &'call mut HostCallbackSession,
    attempt: Option<RemovalOwner<'runtime, A, FinalRemoval>>,
}

impl<'runtime, 'call, A: Addin> RemovalTransaction<'runtime, 'call, A> {
    fn begin(
        runtime: &'runtime Runtime<A, impl crate::runtime::UnloadPolicy<A>>,
        callbacks: &'call mut HostCallbackSession,
    ) -> Option<Self> {
        let deps = runtime.shutdown_deps();
        let claim = runtime.lifecycle_orchestrator().begin_final_removal()?;
        Some(Self {
            deps,
            callbacks,
            attempt: Some(RemovalOwner::from_claim(deps, claim)),
        })
    }

    fn deps(&self) -> ShutdownDeps<'runtime, A> {
        self.deps
    }

    fn callbacks_mut(&mut self) -> &mut HostCallbackSession {
        self.callbacks
    }

    fn take_attempt(&mut self) -> RemovalOwner<'runtime, A, FinalRemoval> {
        self.attempt
            .take()
            .expect("a removal transaction always owns its attempt")
    }
}

impl<A: Addin> Drop for RemovalTransaction<'_, '_, A> {
    fn drop(&mut self) {
        if self.attempt.is_some() {
            // No callback or partial cleanup is legal from Drop. The runtime
            // remains terminally quarantined until an explicit boundary can
            // account for every outstanding resource.
            self.deps.quarantine_state();
        }
    }
}

pub(crate) fn remove_addin_inner<'runtime, A>(
    runtime: &'runtime Runtime<A, impl crate::runtime::UnloadPolicy<A>>,
    lifecycle: &AddinLifecycleAccess<'_, A>,
    callbacks: &mut HostCallbackSession,
) -> RemovalSuccess<'runtime, A>
where
    A: Addin,
{
    match remove_addin_inner_unchecked::<A>(runtime, lifecycle, callbacks) {
        Ok(success) => success,
        Err(control) => commit_removal_control(runtime, control),
    }
}

pub(crate) fn remove_addin_inner_unchecked<'runtime, A>(
    runtime: &'runtime Runtime<A, impl crate::runtime::UnloadPolicy<A>>,
    lifecycle: &AddinLifecycleAccess<'_, A>,
    callbacks: &mut HostCallbackSession,
) -> Result<RemovalSuccess<'runtime, A>, RemovalControl>
where
    A: Addin,
{
    #[cfg(test)]
    let _diagnostic_test_guard = crate::diagnostics::DIAGNOSTIC_TEST_MUTEX.lock();
    // Even an apparently closed runtime must pass through begin_final_removal:
    // a concurrent xlAutoOpen may already have sampled the previous close
    // epoch without having acquired its open-attempt token yet.
    let Some(mut transaction) = RemovalTransaction::begin(runtime, callbacks) else {
        return Ok(RemovalSuccess::AlreadyClosed);
    };
    let shutdown_deps = transaction.deps();
    runtime.observer().begin_close();

    let mut report = crate::shutdown::CloseReport::default();
    let mut unload_failure: Option<(crate::shutdown::UnloadHazard, &'static str, XllError)> = None;
    let _lifecycle_present = match shutdown_deps.has_addin_lifecycle(lifecycle) {
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

    let teardown = match transaction.take_attempt().drain() {
        Ok(stage) => stage,
        Err(failure) => {
            let hazard = failure.hazard();
            let error = failure.into_error();
            return Err(handle_unload_hazard(
                runtime,
                hazard,
                "xlAutoRemove execution drain",
                &error,
            ));
        }
    };
    let teardown = match teardown.stop_producers(|issue| {
        report.push(issue.component, issue.kind, issue.error.clone());
    }) {
        Ok(stage) => stage,
        Err(failure) => {
            return Err(handle_unload_hazard(
                runtime,
                failure.hazard(),
                "xlAutoRemove producer shutdown",
                &failure.into_error(),
            ));
        }
    };

    runtime.observer().async_drained();

    runtime.observer().subscriptions_drained();

    let registrations = shutdown_deps.host().registrations_snapshot();
    if let Ok(outcome) = catch_no_unwind(AssertUnwindSafe(|| {
        let host = RegistrationHost::new(transaction.callbacks_mut());
        HostRegistrar::unregister_pending(&host, &registrations)
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
        for _ in &outcome.succeeded {
            runtime.observer().unregister_function();
        }
        shutdown_deps
            .host()
            .replace_registrations(outcome.failed.into_iter().map(|(entry, _)| entry).collect());
        shutdown_deps
            .host()
            .retain_metadata_debt(outcome.metadata_debt);
        if shutdown_deps.host().has_metadata_debt() {
            let debt_count = shutdown_deps.host().metadata_debt_snapshot().len();
            let _ = catch_no_unwind(AssertUnwindSafe(|| {
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
    if shutdown_deps.host().registration_state_unknown() && unload_failure.is_none() {
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

    // Calculation event exports are module-owned. They remain registered while
    // the DLL lease is retained; no undocumented host removal is attempted.
    // Generation admission was sealed before draining, so late events cannot
    // reach the state being reclaimed below.

    // Host callbacks are now the final direct Excel C API operations in this
    // lifecycle. A terminal result transitions the module gate immediately;
    // once unregistering is complete, close it unconditionally so all later
    // cleanup is provably callback-free.
    let teardown = teardown.close_module_callbacks();

    runtime.observer().callback_admission_closed();

    if let Some((hazard, boundary, error)) = unload_failure.take() {
        return Err(handle_unload_hazard(runtime, hazard, boundary, &error));
    }

    let teardown = match teardown.detach_host() {
        Ok(teardown) => teardown,
        Err(failure) => {
            return Err(handle_unload_hazard(
                runtime,
                failure.hazard(),
                "xlAutoRemove host detachment",
                &failure.into_error(),
            ));
        }
    };
    runtime.observer().host_detached();

    let teardown = match teardown.quiesce_addin(runtime, lifecycle) {
        Ok(teardown) => teardown,
        Err(failure) => {
            return Err(handle_unload_hazard(
                runtime,
                failure.hazard(),
                "xlAutoRemove quiesce",
                &failure.into_error(),
            ));
        }
    };

    runtime.observer().generation_unique();
    runtime.observer().addin_quiesced();

    let teardown = match teardown.seal_services() {
        Ok(stage) => stage,
        Err(failure) => {
            let hazard = failure.hazard();
            let boundary = match &failure {
                super::shutdown::ServiceSealFailure::HandleTopics(_) => "xlAutoRemove RTD shutdown",
                super::shutdown::ServiceSealFailure::Handles(_) => {
                    "xlAutoRemove handle table shutdown"
                }
            };
            return Err(handle_unload_hazard(
                runtime,
                hazard,
                boundary,
                &failure.into_error(),
            ));
        }
    };

    let teardown = match teardown.cleanup_addin(lifecycle, &mut report) {
        Ok(stage) => stage,
        Err(error) => {
            for issue in report.issues() {
                runtime.observer().cleanup_issue();
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
    runtime.observer().generation_reclaimed();
    for issue in report.issues() {
        runtime.observer().cleanup_issue();
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

    runtime.observer().handles_drained();

    let diagnostics_was_running = crate::diagnostics::diagnostic_sink_running();
    let diagnostics_stopped = match crate::diagnostics::close_diagnostic_router().map(|outcome| {
        for issue in outcome.issues {
            runtime.observer().cleanup_issue();
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

    if diagnostics_was_running {
        runtime.observer().diagnostics_stopped();
    }
    runtime.observer().diagnostics_drained();

    let rtd_quiescent = match crate::excel_rtd_protocol::wait_for_module_quiescence(
        runtime.module_residency_held(),
    ) {
        Ok(certificate) => certificate,
        Err(error) => {
            let hazard = if error.missing_module_residency {
                crate::shutdown::UnloadHazard::RtdComObjectsWithoutResidency
            } else if error.revocation_debt != 0 {
                crate::shutdown::UnloadHazard::RtdGitRevocationDebt
            } else {
                crate::shutdown::UnloadHazard::RtdGitCallbackStillRegistered
            };
            return Err(handle_unload_hazard(
                runtime,
                hazard,
                "xlAutoRemove RTD logical quiescence",
                &XllError::Internal {
                    diagnostic_id: if error.missing_module_residency {
                        crate::diagnostics::id::DiagnosticId::MODULE_RESIDENCY
                    } else {
                        crate::diagnostics::id::DiagnosticId::RTD_GIT_QUIESCENCE
                    },
                },
            ));
        }
    };

    runtime.observer().rtd_drained();

    let teardown = teardown.reclaim(rtd_quiescent, diagnostics_stopped);
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

    if let Err(error) = shutdown_deps.release_empty_addin_lifecycle(lifecycle) {
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

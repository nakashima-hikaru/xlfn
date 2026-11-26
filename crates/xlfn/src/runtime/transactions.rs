use crate::XllError;
use crate::addin_impl::{Addin, BuildInfo, OpenContext};
use crate::error::IntoXllError;
use crate::host_callback::HostCallbackSession;
use crate::panic_boundary::catch_no_unwind;
use crate::registration::RegistrationDescriptor;
use crate::registration::{HostRegistrar, RegistrationHost};
use crate::runtime::capabilities::ShutdownDeps;
#[cfg(test)]
use crate::runtime::open_txn::RollbackState;
use crate::runtime::open_txn::{HostAttached, OpeningState, OpeningTxn, PreparedGeneration};
use crate::runtime::shutdown::{
    ExecutionDrainFailure, ExecutionDrained, FinalRemoval, FinalizedRemoval, RemovalOwner,
    TeardownTxn,
};
use crate::runtime::{AddinLifecycleAccess, Runtime};
use std::panic::AssertUnwindSafe;
use xlfn_kernel::rollback::{Rollback, RollbackGuard};

use crate::boundary::{report_boundary_error, report_cleanup_issue};
use crate::lifecycle::lifecycle_access_error;
use crate::runtime::open;
use crate::runtime::recovery::{commit_removal_control, handle_unload_hazard, quarantine_runtime};

type RegisteredOpenResult<'runtime, A> = Result<
    (
        OpeningTxn<'runtime, A, PreparedGeneration<A>>,
        crate::registration::SuccessfulRegistrations,
    ),
    open::OpenFailure<'runtime, A>,
>;

type PreparedOpenResult<'runtime, A> =
    Result<OpeningTxn<'runtime, A, PreparedGeneration<A>>, open::OpenFailure<'runtime, A>>;

pub(crate) fn open_addin_inner<'runtime, A>(
    build_info: BuildInfo,
    descriptors: &[RegistrationDescriptor],
    mut transaction: OpeningTxn<'runtime, A, HostAttached<A>>,
) -> RegisteredOpenResult<'runtime, A>
where
    A: Addin,
{
    let deps = transaction.deps();
    #[cfg(test)]
    let _diagnostic_test_guard = crate::diagnostics::DIAGNOSTIC_TEST_MUTEX.lock();
    if let Err(error) = crate::diagnostics::reset_diagnostic_router() {
        return Err(transaction.failure(error));
    }
    #[cfg(all(windows, any(feature = "rtd", feature = "handles")))]
    if let Err(error) = crate::excel_rtd::register_server_type::<
        crate::addin_impl::AddinSubscription<A>,
        crate::addin_impl::AddinSources<A>,
    >() {
        return Err(transaction.failure(error));
    }
    let prepared_set = match crate::registration::preflight_registration(descriptors) {
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
    if deps.host_intent() == crate::lifecycle::HostLifecycleIntent::ExplicitRemovalRequested {
        return Err(transaction.failure(XllError::Closing));
    }
    #[cfg(feature = "rtd")]
    let Some(generation) = deps.protocol_generation() else {
        return Err(transaction.failure(crate::error::construct::internal(
            crate::diagnostics::catalog::OPEN_STATE,
        )));
    };
    #[cfg(not(feature = "rtd"))]
    if deps.protocol_generation().is_none() {
        return Err(transaction.failure(crate::error::construct::internal(
            crate::diagnostics::catalog::OPEN_STATE,
        )));
    }
    let context = OpenContext::new(
        registrar.module_path().clone(),
        build_info,
        #[cfg(feature = "rtd")]
        generation,
    );
    let transaction = initialize_addin::<A>(context, transaction)?;
    if deps.host_intent() == crate::lifecycle::HostLifecycleIntent::ExplicitRemovalRequested {
        return Err(transaction.failure(XllError::Closing));
    }
    transaction.register_prepared_host(registrar, &prepared_set)
}

impl<'runtime, A: Addin> OpeningTxn<'runtime, A, PreparedGeneration<A>> {
    pub(crate) fn register_prepared_host(
        self,
        registrar: HostRegistrar,
        prepared_set: &crate::registration::preflight::ValidatedRegistrationSet,
    ) -> RegisteredOpenResult<'runtime, A> {
        let mut transaction = self;
        let deps = transaction.deps();
        let has_async_functions = prepared_set
            .iter()
            .any(|descriptor| descriptor.execution().is_async());
        if has_async_functions
            || (!prepared_set.as_slice().is_empty()
                && transaction.runtime_config().calculation_end_notifications())
        {
            let event_result = transaction.ensure_calculation_events(has_async_functions);
            match event_result {
                Ok(()) => {}
                Err(error) => return Err(transaction.failure(error)),
            }
        }
        if deps.host_intent() == crate::lifecycle::HostLifecycleIntent::ExplicitRemovalRequested {
            return Err(transaction.failure(XllError::Closing));
        }
        let registration_result = {
            let host = RegistrationHost::new(transaction.callbacks_mut());
            registrar.register_all(&host, prepared_set)
        };
        let mut registrations = match registration_result {
            Ok(registrations) => registrations,
            Err(error) => {
                let error = retain_transaction_error(&mut transaction, error);
                return Err(transaction.failure(error));
            }
        };
        // Retain successful Excel mutations before installing the lifecycle
        // state. Either operation can still lose to a nested close.
        transaction.retain_journal(registrations.take_journal());
        if deps.host_intent() == crate::lifecycle::HostLifecycleIntent::ExplicitRemovalRequested {
            return Err(transaction.failure(XllError::Closing));
        }
        Ok((transaction, registrations))
    }
}

#[cfg(test)]
pub(crate) fn rollback_active_open<'runtime, A, S>(
    lifecycle: &AddinLifecycleAccess<'_, A>,
    attempt: Option<OpeningTxn<'runtime, A, S>>,
) where
    A: Addin,
    S: RollbackState<A>,
{
    if let Some(attempt) = attempt {
        attempt.into_rollback().run(lifecycle);
    }
}

pub(crate) fn initialize_addin<'runtime, A>(
    context: OpenContext<A>,
    transaction: OpeningTxn<'runtime, A, HostAttached<A>>,
) -> PreparedOpenResult<'runtime, A>
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
    #[cfg(feature = "async")]
    let (shared_state, lifecycle_state, layers, runtime_config, async_executor) =
        opened.into_parts();
    #[cfg(not(feature = "async"))]
    let (shared_state, lifecycle_state, layers, runtime_config, _) = opened.into_parts();
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
    // Prepare the entire generation service bundle while the transaction
    // still owns the rollback session, before workers or host registration.
    let transaction = transaction.prepare_services()?;
    #[cfg(feature = "async")]
    if let Err(error) = transaction
        .deps()
        .async_runtime()
        .start(async_executor, runtime_config.async_task_limit())
    {
        return Err(transaction.failure(error));
    }
    Ok(transaction)
}

fn retain_transaction_error<A: Addin, S>(
    transaction: &mut OpeningTxn<'_, A, S>,
    error: crate::registration::RegistrationTransactionError,
) -> XllError
where
    S: OpeningState<A> + crate::runtime::open_txn::HasHost,
{
    if error.journal.is_unknown() {
        for unknown in error.journal.unknown_registrations() {
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
        RemovalSuccess::Closed { finalized } => {
            finalized.record_return();
            1
        }
    }
}

pub(crate) enum RemovalSuccess<'runtime, A: Addin> {
    AlreadyClosed,
    Quarantined,
    Closed {
        finalized: FinalizedRemoval<'runtime, A>,
    },
}

pub(crate) enum RemovalControl {
    Quarantine {
        hazard: crate::shutdown::UnloadHazard,
        boundary: &'static str,
        error: XllError,
    },
}

/// Holds the removal claim while checking prerequisites for execution drain.
struct RemovalClaimed<'runtime, 'call, A: Addin> {
    pending: RollbackGuard<UndrainedRemoval<'runtime, A>>,
    callbacks: &'call mut HostCallbackSession,
}

struct UndrainedRemoval<'runtime, A: Addin> {
    deps: ShutdownDeps<'runtime, A>,
    attempt: RemovalOwner<'runtime, A, FinalRemoval>,
}

impl<A: Addin> Rollback for UndrainedRemoval<'_, A> {
    fn rollback(self) {
        // Quarantine before the attempt returns its authority. Host cleanup
        // requires an explicit boundary with a successful drain.
        self.deps.quarantine_state();
    }
}

struct HostCleanupContext<'runtime, 'call, A: Addin> {
    deps: ShutdownDeps<'runtime, A>,
    callbacks: &'call mut HostCallbackSession,
}

type DrainedRemoval<'runtime, 'call, A> = (
    TeardownTxn<'runtime, A, FinalRemoval, ExecutionDrained>,
    HostCleanupContext<'runtime, 'call, A>,
);

impl<'runtime, 'call, A: Addin> RemovalClaimed<'runtime, 'call, A> {
    fn begin(
        runtime: &'runtime Runtime<A, impl crate::runtime::UnloadPolicy<A>>,
        callbacks: &'call mut HostCallbackSession,
    ) -> Option<Self> {
        let deps = runtime.shutdown_deps();
        let attempt = RemovalOwner::acquire_final(deps)?;
        Some(Self {
            pending: RollbackGuard::new(UndrainedRemoval { deps, attempt }),
            callbacks,
        })
    }

    fn deps(&self) -> ShutdownDeps<'runtime, A> {
        self.pending.get().deps
    }

    fn drain(
        self,
    ) -> Result<DrainedRemoval<'runtime, 'call, A>, ExecutionDrainFailure<'runtime, A>> {
        let UndrainedRemoval { deps, attempt } = self.pending.complete();
        let teardown = attempt.drain()?;
        Ok((
            teardown,
            HostCleanupContext {
                deps,
                callbacks: self.callbacks,
            },
        ))
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
    let Some(claim) = RemovalClaimed::begin(runtime, callbacks) else {
        return Ok(RemovalSuccess::AlreadyClosed);
    };
    runtime.observer().begin_close();

    let mut report = crate::shutdown::CloseReport::default();
    let mut unload_failure: Option<(crate::shutdown::UnloadHazard, &'static str, XllError)> = None;
    let _lifecycle_present = match claim.deps().has_addin_lifecycle(lifecycle) {
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

    let (
        teardown,
        HostCleanupContext {
            deps: shutdown_deps,
            callbacks,
        },
    ) = match claim.drain() {
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

    let cleanup = catch_no_unwind(AssertUnwindSafe(|| {
        let host = RegistrationHost::new(callbacks);
        shutdown_deps.host().cleanup_registrations(&host)
    }))
    .map_err(|_| XllError::Panic)
    .and_then(std::convert::identity);
    match cleanup {
        Ok(outcome) => {
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
            let metadata = shutdown_deps.host().metadata_debt_status();
            if metadata.has_pending_work() {
                let _ = catch_no_unwind(AssertUnwindSafe(|| {
                    tracing::warn!(
                        count = metadata.pending_buckets,
                        cleanup_in_progress = metadata.cleanup_in_progress,
                        "xlAutoRemove completed with host metadata debt"
                    );
                }));
            }
        }
        Err(error) => {
            report_boundary_error("xlAutoRemove unregister", &error);
            unload_failure = Some((
                crate::shutdown::UnloadHazard::HostCallbackStillRegistered,
                "xlAutoRemove unregister",
                error,
            ));
        }
    }
    if shutdown_deps.host().registration_state_unknown() && unload_failure.is_none() {
        let error =
            crate::error::construct::internal(crate::diagnostics::catalog::REGISTRATION_UNKNOWN);
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
            let (hazard, boundary, error) =
                failure.into_diagnostic(super::shutdown::ShutdownDiagnosticContext::Removal);
            return Err(handle_unload_hazard(runtime, hazard, boundary, &error));
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

    let teardown = match teardown.reclaim(report_cleanup_issue) {
        Ok(teardown) => teardown,
        Err(failure) => {
            let (hazard, boundary, error) =
                failure.into_diagnostic(super::shutdown::ShutdownDiagnosticContext::Removal);
            return Err(handle_unload_hazard(runtime, hazard, boundary, &error));
        }
    };
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
    let finalized = match certificate.finish() {
        Ok(finalized) => finalized,
        Err(failure) => {
            let error = failure.quarantine();
            return Err(handle_unload_hazard(
                runtime,
                crate::shutdown::UnloadHazard::CloseInvariantViolation,
                "xlAutoRemove finalization",
                &error,
            ));
        }
    };
    Ok(RemovalSuccess::Closed { finalized })
}

#[cfg(test)]
mod removal_tests {
    use super::*;
    use crate::lifecycle::LifecyclePhase;

    #[test]
    fn miri_undrained_removal_claim_quarantines_before_releasing_authority() {
        let runtime = Runtime::<()>::new();
        let opening = runtime.begin_open().unwrap();
        let opening = runtime.publish(opening, (), ());
        runtime.finish_open(opening, Vec::new()).unwrap();
        let mut callbacks = HostCallbackSession::new();
        let claimed = RemovalClaimed::begin(&runtime, &mut callbacks).unwrap();
        assert_eq!(runtime.phase(), LifecyclePhase::Closing);
        assert!(runtime.lifecycle.access().removal_attempt().is_some());

        drop(claimed);
        assert_eq!(runtime.phase(), LifecyclePhase::Quarantined);
        assert!(runtime.lifecycle.access().removal_attempt().is_none());
        assert!(runtime.begin_open().is_err());
    }
}

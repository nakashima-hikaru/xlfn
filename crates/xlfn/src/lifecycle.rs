use crate::addin::{Addin, BuildInfo, OpenContext, RuntimeConfig};
use crate::diagnostics::AddinId;
use crate::error::IntoXllError;
use crate::generation::RuntimeGeneration;
use crate::host_callback::HostCallbackSession;
use crate::registration::HostRegistrar;
use crate::registration::RegistrationDescriptor;
use crate::runtime::{AddinLifecycleAccess, Runtime};
use crate::{XllError, XllResult};
use std::panic::{AssertUnwindSafe, catch_unwind};
use xlfn_kernel::thread_affine::ThreadAffineError;

mod boundary;
mod open;
mod rollback;
mod state;
mod teardown;

pub use boundary::{host_auto_close, host_auto_open, host_auto_remove};
pub(super) use open::open_addin_boundary as open_addin;
use rollback::{active_runtime_generation, rollback_open};
pub(crate) use state::HostLifecycleIntent;
pub use state::LifecyclePhase;
use teardown::drain_execution;

macro_rules! lifecycle_token {
    ($name:ident) => {
        #[derive(Debug)]
        pub(crate) struct $name {
            _private: (),
        }

        impl $name {
            fn new() -> Self {
                Self { _private: () }
            }

            #[cfg(test)]
            pub(crate) const fn for_test() -> Self {
                Self { _private: () }
            }
        }
    };
}

lifecycle_token!(HostCallbacksDetached);
lifecycle_token!(AddinQuiesced);
lifecycle_token!(GenerationReclaimed);

#[cfg(not(feature = "async"))]
lifecycle_token!(AsyncStopped);

fn lifecycle_access_error(error: ThreadAffineError) -> XllError {
    let diagnostic_id = match error {
        ThreadAffineError::WrongThread | ThreadAffineError::StaleAccess => {
            crate::error::DiagnosticId::LIFECYCLE_THREAD
        }
        _ => crate::error::DiagnosticId::LIFECYCLE_SLOT,
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

type StagedOpenResult<'runtime, A> = Result<
    (
        open::OpeningTransaction<'runtime, A, open::AddinStaged>,
        Vec<crate::registration::RegistrationId>,
    ),
    open::OpenFailure<'runtime, A>,
>;

type InitializedOpenResult<'runtime, A> = Result<
    (
        open::OpeningTransaction<'runtime, A, open::AddinStaged>,
        RuntimeConfig,
    ),
    open::OpenFailure<'runtime, A>,
>;

fn open_addin_inner<'runtime, A>(
    runtime: &'runtime Runtime<A>,
    lifecycle: &AddinLifecycleAccess<'_, A>,
    build_info: BuildInfo,
    descriptors: &[RegistrationDescriptor],
    mut transaction: open::OpeningTransaction<'runtime, A, open::OpenBegun>,
) -> StagedOpenResult<'runtime, A>
where
    A: Addin,
{
    #[cfg(test)]
    let _diagnostic_test_guard = crate::diagnostics::DIAGNOSTIC_TEST_MUTEX.lock();
    if let Err(error) = crate::diagnostics::reset_diagnostic_router() {
        return Err(transaction.failure(error));
    }
    let _prepared_set = match crate::registration::preflight_registration(descriptors) {
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
        diagnostic_id: crate::error::DiagnosticId::OPEN_STATE,
    });
    let generation = match generation {
        Ok(generation) => generation,
        Err(error) => return Err(transaction.failure(error)),
    };
    let context = OpenContext::new(registrar.module_path().clone(), build_info, generation);
    let (mut transaction, runtime_config) =
        initialize_addin::<A>(runtime, lifecycle, &context, transaction)?;
    #[cfg(not(feature = "async"))]
    let _ = runtime_config;
    let has_async_functions = descriptors
        .iter()
        .any(|descriptor| descriptor.signature.result == crate::registration::ResultAbi::AsyncVoid);
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
                diagnostic_id: crate::error::DiagnosticId::ASYNC_FEATURE,
            }));
        }
    }
    let registrations = match registrar.register_all(transaction.callbacks_mut(), descriptors) {
        Ok(registrations) => registrations,
        Err(error) => {
            let error = retain_transaction_error(&mut transaction, error);
            return Err(transaction.failure(error));
        }
    };
    Ok((transaction, registrations))
}

fn rollback_active_open<'runtime, A, Stage>(
    runtime: &'runtime Runtime<A>,
    lifecycle: &AddinLifecycleAccess<'_, A>,
    attempt: Option<crate::runtime::OpeningTxn<'runtime, A, Stage>>,
    callbacks: &mut HostCallbackSession,
) where
    A: Addin,
{
    let Some(attempt) = attempt else {
        return;
    };
    let generation = Some(
        RuntimeGeneration::new(attempt.attempt_id().get())
            .expect("an active open attempt has a runtime generation"),
    );
    if attempt.fail().requires_rollback() {
        match catch_unwind(AssertUnwindSafe(|| {
            rollback_open::<A>(runtime, lifecycle, callbacks, generation)
        })) {
            Ok(outcome) if outcome.unload_safe() => {}
            Ok(_) => {
                let error = XllError::Internal {
                    diagnostic_id: crate::error::DiagnosticId::OPEN_ROLLBACK_FAILURE,
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

fn initialize_addin<'runtime, A>(
    runtime: &'runtime Runtime<A>,
    lifecycle: &AddinLifecycleAccess<'_, A>,
    context: &OpenContext,
    transaction: open::OpeningTransaction<'runtime, A, open::OpenBegun>,
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
    // Lifecycle state is deliberately installed in the main-thread slot
    // before the shared generation is staged. It may be non-Send and must not
    // become part of the cross-thread generation root.
    if let Err(error) = runtime.install_addin_lifecycle(lifecycle, lifecycle_state) {
        let (lifecycle_state, _) = error.into_parts();
        std::mem::forget(lifecycle_state);
        runtime.quarantine_shared_state(
            active_runtime_generation(runtime),
            shared_state,
            crate::runtime_components::QuarantineReason::OpenStateInvariant,
        );
        runtime.quarantine_layers(
            active_runtime_generation(runtime),
            layers,
            crate::runtime_components::QuarantineReason::OpenStateInvariant,
        );
        return Err(transaction.failure(XllError::Internal {
            diagnostic_id: crate::error::DiagnosticId::OPEN_STATE,
        }));
    }
    // Stage the complete generation as one owned value.  The opening state
    // cannot be observed in a partially assembled form.
    let opening = crate::runtime::OpeningGeneration {
        shared_state,
        layers,
        init_config: runtime_config,
    };
    let transaction = transaction.stage_generation(opening)?;
    Ok((transaction, runtime_config))
}

fn retain_transaction_error<A: Addin, Stage: open::OpenTransactionStage>(
    transaction: &mut open::OpeningTransaction<'_, A, Stage>,
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
pub fn remove_addin<A>(runtime: &Runtime<A>, lifecycle: &AddinLifecycleAccess<'_, A>) -> i32
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

enum RemovalSuccess<'runtime, A: Addin> {
    AlreadyClosed,
    Quarantined,
    Closed {
        witness: crate::runtime::ClosedWitness,
        removal_attempt: crate::runtime::RemovalOwner<'runtime, A>,
    },
}

enum RemovalControl {
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
    attempt: Option<crate::runtime::RemovalOwner<'runtime, A>>,
}

impl<'runtime, A: Addin> RemovalTransaction<'runtime, A> {
    fn begin(runtime: &'runtime Runtime<A>) -> Option<Self> {
        Some(Self {
            runtime,
            callbacks: HostCallbackSession::new(),
            attempt: Some(runtime.begin_final_removal()?),
        })
    }

    fn callbacks(&self) -> &HostCallbackSession {
        &self.callbacks
    }

    fn callbacks_mut(&mut self) -> &mut HostCallbackSession {
        &mut self.callbacks
    }

    fn take_attempt(&mut self) -> crate::runtime::RemovalOwner<'runtime, A> {
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
            self.runtime.quarantine();
        }
    }
}

fn remove_addin_inner<'runtime, A>(
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

fn remove_addin_inner_unchecked<'runtime, A>(
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
    crate::module_runtime::global().begin_close(|| {
        #[cfg(any(test, feature = "refinement"))]
        if runtime.refinement_hooks().generation_active(runtime) {
            runtime.refinement_hooks().begin_close(runtime);
        }
    });

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

    let execution_drained = drain_execution(runtime, true);
    let owner = transaction.take_attempt();
    let teardown: teardown::TeardownTxn<
        'runtime,
        A,
        crate::runtime::FinalRemoval,
        teardown::ExecutionDrained,
    > = teardown::TeardownTxn::new(owner, execution_drained);
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
    runtime.refinement_hooks().async_stopped(runtime);
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
            diagnostic_id: crate::error::DiagnosticId::REGISTRATION_UNKNOWN,
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
    crate::module_runtime::global().close_callbacks();

    #[cfg(any(test, feature = "refinement"))]
    runtime.refinement_hooks().callback_gate_closed(runtime);

    if let Some((hazard, boundary, error)) = unload_failure.take() {
        return Err(handle_unload_hazard(runtime, hazard, boundary, &error));
    }

    let host_callbacks = crate::shutdown::HostCallbacksDetached::new();
    #[cfg(any(test, feature = "refinement"))]
    runtime.refinement_hooks().host_detached(runtime);

    let addin = if let Some(generation) = runtime.take_generation_for_shutdown() {
        match generation {
            crate::runtime::ShutdownGeneration::Open(generation) => {
                match std::sync::Arc::try_unwrap(generation) {
                    Ok(mut generation) => {
                        let quiesce = catch_unwind(AssertUnwindSafe(|| {
                            runtime
                                .with_addin_lifecycle(lifecycle, |lifecycle_state| {
                                    A::quiesce(&mut generation.shared_state, lifecycle_state)
                                        .map_err(IntoXllError::into_xll_error)
                                })
                                .map_err(lifecycle_access_error)
                        }))
                        .map_err(|_| XllError::Panic)
                        .and_then(|result| result)
                        .and_then(|result| result);
                        if let Err(error) = quiesce {
                            report_boundary_error("xlAutoRemove quiesce", &error);
                            runtime.quarantine_generation(
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
                            diagnostic_id: crate::error::DiagnosticId::STATE_SCAN,
                        };
                        report_boundary_error("xlAutoRemove state escaped", &error);
                        runtime.quarantine_shared_generation(
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
            crate::runtime::ShutdownGeneration::Opening(opening) => {
                let (mut shared_state, layers, _config) = opening.into_parts();
                let quiesce = catch_unwind(AssertUnwindSafe(|| {
                    runtime
                        .with_addin_lifecycle(lifecycle, |lifecycle_state| {
                            A::quiesce(&mut shared_state, lifecycle_state)
                                .map_err(IntoXllError::into_xll_error)
                        })
                        .map_err(lifecycle_access_error)
                }))
                .map_err(|_| XllError::Panic)
                .and_then(|result| result)
                .and_then(|result| result);
                if let Err(error) = quiesce {
                    report_boundary_error("xlAutoRemove quiesce", &error);
                    let generation_id = active_runtime_generation(runtime);
                    runtime.quarantine_layers(
                        generation_id,
                        layers,
                        crate::runtime_components::QuarantineReason::AddinQuiesceFailed,
                    );
                    runtime.quarantine_shared_state(
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
                diagnostic_id: crate::error::DiagnosticId::LIFECYCLE_SLOT,
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

    if let Err(error) = runtime.shutdown_rtd() {
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
    if let Err(error) = runtime.refinement_hooks().diagnostics_stopped(runtime) {
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
                    diagnostic_id: crate::error::DiagnosticId::RTD_GIT_QUIESCENCE,
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
            .fail_stop(_runtime, hazard.ghost_failure());
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
    runtime.quarantine();
    #[cfg(any(test, feature = "refinement"))]
    runtime.refinement_hooks().quarantine(
        runtime,
        crate::shutdown_refinement::GhostFailure::BoundaryPanic,
    );
    quarantine_runtime_resources(runtime);
}

fn quarantine_for_hazard<A: Addin>(runtime: &Runtime<A>, _hazard: crate::shutdown::UnloadHazard) {
    runtime.quarantine();
    #[cfg(any(test, feature = "refinement"))]
    runtime
        .refinement_hooks()
        .quarantine(runtime, _hazard.ghost_failure());
    quarantine_runtime_resources(runtime);
}

fn quarantine_runtime_resources<A: Addin>(runtime: &Runtime<A>) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        crate::module_runtime::global().begin_close(|| {});
        let ingress = crate::module_runtime::ingress();
        if matches!(
            ingress.phase(),
            crate::ingress::PHASE_OPENING | crate::ingress::PHASE_OPEN
        ) {
            ingress.begin_close_with(|| {});
        }
        if ingress.phase() == crate::ingress::PHASE_CLOSING {
            let _ = ingress.seal_and_drain();
        }
        crate::module_runtime::global().close_callbacks();
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
mod tests {
    use super::*;
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
        assert!(runtime.begin_open_if_epoch(stale_epoch).is_err());
        assert_eq!(runtime.phase(), crate::lifecycle::LifecyclePhase::Closed);
    }

    #[test]
    fn failed_concurrent_open_does_not_rollback_the_owner_attempt() {
        let runtime = Runtime::<LayersPanic>::new();
        let mut owner = runtime.begin_open().unwrap();
        let mut callbacks = HostCallbackSession::new();
        let lifecycle = lifecycle_access(&runtime);

        rollback_active_open::<LayersPanic, crate::runtime::OpenAttemptBegun>(
            &runtime,
            &lifecycle,
            None,
            &mut callbacks,
        );
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
        let transaction = open::OpeningTransaction::begin(&runtime, removal_epoch).unwrap();
        let lifecycle = lifecycle_access(&runtime);
        let (transaction, _) = match initialize_addin::<LayersPanic>(
            &runtime,
            &lifecycle,
            &test_open_context(),
            transaction,
        ) {
            Ok(result) => result,
            Err(failure) => {
                let error = failure.rollback(&lifecycle);
                panic!("unexpected add-in initialization failure: {error}");
            }
        };
        assert!(runtime.has_opening_generation());
        assert!(!runtime.has_current_generation());
        transaction.rollback(&lifecycle);
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
        let mut first_open = runtime.begin_open().unwrap();
        runtime.publish((), ());
        runtime.finish_open(&mut first_open, Vec::new()).unwrap();
        let first_generation = runtime.last_committed_generation();

        let lifecycle = lifecycle_access(&runtime);
        assert_eq!(remove_addin::<LayersPanic>(&runtime, &lifecycle), 1);
        assert_eq!(runtime.phase(), crate::lifecycle::LifecyclePhase::Closed);
        runtime.clear_host_intent();
        let mut second_open = runtime.begin_open().unwrap();
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
                diagnostic_id: crate::error::DiagnosticId::OPEN_STATE,
            })
        }
    }

    #[test]
    fn failed_controlled_reload_quarantines_the_runtime() {
        let runtime = Runtime::<ReloadFailure>::new();
        let mut first_open = runtime.begin_open().unwrap();
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
                    diagnostic_id: crate::error::DiagnosticId::TEST_RETRY,
                },
            );
        }
    }

    #[test]
    fn addin_cleanup_issue_does_not_prevent_finalizing_runtime() {
        let runtime = Runtime::<RetryClose>::new();
        let attempts = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut open_attempt = runtime.begin_open().unwrap();
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
        let mut opening = runtime.begin_open().unwrap();
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
        let mut opening = runtime.begin_open().unwrap();
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
                diagnostic_id: crate::error::DiagnosticId::QUIESCENCE_FAILURE,
            })
        }
    }

    #[test]
    fn quiesce_failure_enters_quarantine_without_dropping_state() {
        let runtime = Runtime::<QuiesceFailure>::new();
        let drops = std::sync::Arc::new(AtomicUsize::new(0));
        let mut opening = runtime.begin_open().unwrap();
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
        let open_attempt = runtime.begin_open().unwrap();
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
                    diagnostic_id: crate::error::DiagnosticId::LEAN_TRACE,
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
        let mut opening = runtime.begin_open().unwrap();
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
        let mut opening = clean_runtime.begin_open().unwrap();
        clean_runtime.publish((), ());
        clean_runtime.finish_open(&mut opening, Vec::new()).unwrap();
        assert_eq!(host_auto_remove::<CleanClose>(&clean_runtime), 1);
        check("clean", clean_runtime.ghost_trace_json());

        let failure_runtime = Runtime::<QuiesceFailure>::new();
        let drops = std::sync::Arc::new(AtomicUsize::new(0));
        let mut opening = failure_runtime.begin_open().unwrap();
        failure_runtime.publish_with_lifecycle((), DropObserved(std::sync::Arc::clone(&drops)), ());
        failure_runtime
            .finish_open(&mut opening, Vec::new())
            .unwrap();
        assert_eq!(host_auto_remove::<QuiesceFailure>(&failure_runtime), 1);
        assert_eq!(
            failure_runtime.phase(),
            crate::lifecycle::LifecyclePhase::Quarantined
        );
        let failure_trace = failure_runtime.ghost_trace_json();
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
        let runtime = Runtime::<CleanClose>::new();
        let mut opening = runtime.begin_open().unwrap();
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
        let runtime = Runtime::<CleanClose>::new();
        let mut opening = runtime.begin_open().unwrap();
        runtime.publish((), ());
        runtime.finish_open(&mut opening, Vec::new()).unwrap();
        runtime.refinement_hooks().call_entered(&runtime);
        runtime.refinement_hooks().call_left(&runtime);

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
            let mut opening = runtime.begin_open().unwrap();
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
        let mut opening = runtime.begin_open().unwrap();
        runtime.publish((), ());
        runtime.finish_open(&mut opening, Vec::new()).unwrap();
        crate::diagnostics::reset_diagnostic_router().unwrap();
        crate::diagnostics::set_diagnostic_sink(TraceDiagnosticSink).unwrap();
        crate::diagnostics::report_no_unwind("composition_takeover_trace", &XllError::Panic);

        let first = runtime.begin_final_removal().unwrap();
        drop(first);
        let second = runtime.begin_final_removal().unwrap();
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
        let mut opening = uncommitted.begin_open().unwrap();
        let closing_runtime = std::sync::Arc::clone(&uncommitted);
        let (owner_tx, owner_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let close_waiter = std::thread::spawn(move || {
            let removal_attempt = closing_runtime
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
        let opening = rollback.begin_open().unwrap();
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
        let mut opening = runtime.begin_open().unwrap();
        runtime.publish((), ());
        runtime.finish_open(&mut opening, Vec::new()).unwrap();

        let lifecycle = lifecycle_access(&runtime);
        let success = remove_addin_inner::<CleanClose>(&runtime, &lifecycle);
        assert!(runtime.begin_open().is_err());
        let RemovalSuccess::Closed {
            witness,
            removal_attempt,
        } = success
        else {
            panic!("test close must own the close attempt");
        };
        runtime.record_returned_success(witness).unwrap();
        drop(removal_attempt);

        let mut reopened = runtime.begin_open().unwrap();
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
                    diagnostic_id: crate::error::DiagnosticId::FAILURE,
                },
            );
        }
    }

    #[test]
    fn failing_open_rollback_is_finalized_by_xl_auto_close() {
        let runtime = Runtime::<AlwaysFailClose>::new();
        let open_attempt = runtime.begin_open().unwrap();
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
        let mut open_attempt = runtime.begin_open().unwrap();
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
        let mut open_attempt = runtime.begin_open().unwrap();
        runtime.publish((), ());
        runtime.finish_open(&mut open_attempt, Vec::new()).unwrap();

        assert_eq!(host_auto_close::<CleanClose>(&runtime), 1);
        assert_eq!(runtime.phase(), crate::lifecycle::LifecyclePhase::Open);
        let ingress = crate::module_runtime::ingress()
            .enter_with(|| {})
            .into_admitted()
            .expect("test call enters during OPEN");
        assert!(runtime.enter(&ingress).is_ok());

        assert_eq!(host_auto_remove::<CleanClose>(&runtime), 1);
        assert_eq!(runtime.phase(), crate::lifecycle::LifecyclePhase::Closed);
    }

    #[inline(never)]
    fn lifecycle_residency_probe_anchor() {}

    #[test]
    fn residency_release_requires_removal_then_close_hint() {
        let runtime = Runtime::<CleanClose>::new();
        assert!(
            runtime
                .ensure_module_residency(lifecycle_residency_probe_anchor as *const ())
                .is_ok()
        );
        let mut open_attempt = runtime.begin_open().unwrap();
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
        let runtime = Runtime::<CleanClose>::new();
        assert!(
            runtime
                .ensure_module_residency(lifecycle_residency_probe_anchor as *const ())
                .is_ok()
        );
        let mut open_attempt = runtime.begin_open().unwrap();
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
        let mut open_attempt = runtime.begin_open().unwrap();
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
        crate::module_runtime::global().rtd().begin_open();
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
        let mut open_attempt = runtime.begin_open().unwrap();
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
        let open_attempt = runtime.begin_open().unwrap();
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
            .stage(crate::runtime::OpeningGeneration {
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

        let open_attempt = runtime.begin_open().unwrap();
        let state = PanicLayersState {
            quiesced: std::sync::Arc::clone(&quiesced),
            cleaned: std::sync::Arc::clone(&cleaned),
            dropped: std::sync::Arc::clone(&dropped),
        };
        let lifecycle = lifecycle_access(&runtime);
        assert!(runtime.install_addin_lifecycle(&lifecycle, state).is_ok());
        let open_attempt = open_attempt
            .stage(crate::runtime::OpeningGeneration {
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

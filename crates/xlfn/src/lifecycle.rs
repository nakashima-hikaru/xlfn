use crate::host_callback::{HostCallbackSession, HostCallbackState};
use crate::registration::HostRegistrar;
use crate::{
    Addin, AddinId, BuildInfo, IntoXllError, OpenContext, RegistrationDescriptor, Runtime,
    RuntimeConfig, XllError, XllResult,
};
use std::panic::{AssertUnwindSafe, catch_unwind};

/// Handles the generated `xlAutoOpen` boundary. An open runtime is a
/// controlled reload request: the old logical generation is terminally torn
/// down, then a new generation is opened while the physical residency lease
/// remains held.
#[must_use]
pub fn host_auto_open<A>(
    runtime: &Runtime<A>,
    addin_id: &AddinId,
    version: &'static str,
    target: &'static str,
    descriptors: &[RegistrationDescriptor],
) -> i32
where
    A: Addin,
{
    if runtime.phase() == crate::LifecyclePhase::Quarantined {
        return 0;
    }
    let controlled_reload = runtime.phase() == crate::LifecyclePhase::Open;
    let removal_completed_before_open = runtime.phase() == crate::LifecyclePhase::Closed
        && runtime.host_intent() == crate::runtime::HostLifecycleIntent::ExplicitRemovalComplete;
    if controlled_reload {
        let result = remove_addin::<A>(runtime);
        if result == 0 || runtime.phase() != crate::LifecyclePhase::Closed {
            return 0;
        }
        runtime.clear_host_intent();
    }
    let result = open_addin(runtime, addin_id, version, target, descriptors);
    if controlled_reload && result == 0 && runtime.phase() != crate::LifecyclePhase::Quarantined {
        // A reload has already destroyed the previous generation. A failed
        // replacement must therefore not leave a closed runtime with the old
        // residency lease and no generation owner.
        quarantine_runtime(runtime);
    } else if result == 0
        && removal_completed_before_open
        && runtime.phase() == crate::LifecyclePhase::Closed
    {
        // The old generation was already removed successfully, but Excel
        // attempted a new open before delivering its close hint. Preserve
        // the release marker so that the later hint can release the lease.
        runtime.complete_explicit_removal();
    }
    result
}

/// Handles Excel's ambiguous close/deactivation hint. It is deliberately a
/// no-op for an open runtime; only explicit removal is allowed to run terminal
/// teardown.
#[must_use]
pub fn host_auto_close<A>(runtime: &Runtime<A>) -> i32
where
    A: Addin,
{
    if runtime.phase() == crate::LifecyclePhase::Closed
        && runtime.host_intent() == crate::runtime::HostLifecycleIntent::ExplicitRemovalComplete
    {
        if let Err(error) = runtime.release_module_residency() {
            report_boundary_error("xlAutoClose module residency release", &error);
            quarantine_runtime(runtime);
        } else {
            runtime.clear_host_intent();
        }
    }
    1
}

/// Handles the explicit terminal-removal boundary.
#[must_use]
pub fn host_auto_remove<A>(runtime: &Runtime<A>) -> i32
where
    A: Addin,
{
    if runtime.phase() == crate::LifecyclePhase::Quarantined {
        return 1;
    }
    runtime.request_explicit_removal();
    let result = remove_addin::<A>(runtime);
    if result == 1 && runtime.phase() == crate::LifecyclePhase::Closed {
        runtime.complete_explicit_removal();
    }
    1
}

#[must_use]
pub fn open_addin<A>(
    runtime: &Runtime<A>,
    addin_id: &AddinId,
    version: &'static str,
    target: &'static str,
    descriptors: &[RegistrationDescriptor],
) -> i32
where
    A: Addin,
{
    std::hint::black_box(crate::crt::effective_crt_policy());
    let removal_epoch = runtime.removal_epoch();
    let mut transaction = None;
    let result = catch_unwind(AssertUnwindSafe(|| {
        if runtime.phase() == crate::LifecyclePhase::OpenRollbackPending {
            let mut callbacks = HostCallbackSession::new();
            let outcome = rollback_open::<A>(runtime, &mut callbacks);
            if !outcome.unload_safe() {
                let error = XllError::Internal {
                    diagnostic_id: crate::DiagnosticId::OPEN_ROLLBACK_PENDING,
                };
                report_boundary_error("xlAutoOpen pending rollback", &error);
                quarantine_runtime(runtime);
                return Err(error);
            }
        }

        // A final removal that overlapped recovery of a previous failed open
        // owns the terminal outcome. Do not resurrect the runtime after that
        // close has already completed.
        if runtime.removal_epoch() != removal_epoch {
            return Err(XllError::Closing);
        }

        transaction = Some(OpenTransaction::begin(runtime, removal_epoch)?);
        let transaction = transaction
            .as_mut()
            .expect("the open transaction was installed");
        retry_metadata_debt(runtime, transaction.callbacks_mut())?;
        let registrations = open_addin_inner::<A>(
            runtime,
            BuildInfo::new(addin_id.clone(), version, target),
            descriptors,
            transaction.callbacks_mut(),
        )?;
        transaction.finish(registrations)
    }));

    match result {
        Ok(Ok(())) => {
            write_startup_log(addin_id, "xlAutoOpen succeeded");
            1
        }
        Ok(Err(error)) => {
            write_startup_log(addin_id, &format!("xlAutoOpen failed: {error}"));
            report_boundary_error("xlAutoOpen", &error);
            if let Some(transaction) = transaction.as_mut() {
                transaction.rollback();
            }
            0
        }
        Err(_) => {
            let error = XllError::Panic;
            write_startup_log(addin_id, "xlAutoOpen failed: panic at boundary");
            report_boundary_error("xlAutoOpen", &error);
            if let Some(transaction) = transaction.as_mut() {
                transaction.rollback();
            }
            0
        }
    }
}

/// Owns one logical open attempt, including the callback session that can
/// undo host mutations made by that attempt. The caller must explicitly call
/// [`Self::finish`] or [`Self::rollback`]; dropping an active transaction only
/// quarantines the runtime and never performs implicit callback cleanup.
struct OpenTransaction<'runtime, A: Addin> {
    runtime: &'runtime Runtime<A>,
    callbacks: HostCallbackSession,
    attempt: Option<crate::runtime::OpenAttemptGuard<'runtime, A>>,
}

impl<'runtime, A: Addin> OpenTransaction<'runtime, A> {
    fn begin(runtime: &'runtime Runtime<A>, removal_epoch: u64) -> XllResult<Self> {
        Ok(Self {
            runtime,
            callbacks: HostCallbackSession::new(),
            attempt: Some(runtime.begin_open_if_epoch(removal_epoch)?),
        })
    }

    fn callbacks_mut(&mut self) -> &mut HostCallbackSession {
        &mut self.callbacks
    }

    fn finish(&mut self, registrations: Vec<crate::RegistrationId>) -> XllResult<()> {
        self.runtime.finish_open(
            self.attempt
                .as_mut()
                .expect("an open transaction always owns its attempt"),
            registrations,
        )
    }

    fn rollback(&mut self) {
        rollback_active_open(self.runtime, self.attempt.as_mut(), &mut self.callbacks);
    }
}

impl<A: Addin> Drop for OpenTransaction<'_, A> {
    fn drop(&mut self) {
        if self
            .attempt
            .as_ref()
            .is_some_and(crate::runtime::OpenAttemptGuard::is_active)
        {
            // A dropped transaction must not call Excel. It is an unrecovered
            // protocol failure, so retain the fail-safe terminal state for a
            // later explicit removal/reload decision.
            self.runtime.quarantine();
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
    runtime: &Runtime<A>,
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
    let runtime_config = initialize_addin::<A>(runtime, &context)?;
    #[cfg(not(feature = "async"))]
    let _ = runtime_config;
    let has_async_functions = descriptors
        .iter()
        .any(|descriptor| descriptor.signature.result == crate::ResultAbi::AsyncVoid);
    if has_async_functions {
        #[cfg(feature = "async")]
        {
            runtime.start_async(runtime_config.async_worker_count())?;
            match registrar.register_async_events(callbacks) {
                Ok(events) => runtime.set_event_registrations(events),
                Err(error) => return Err(retain_transaction_error(runtime, error)),
            }
        }
        #[cfg(not(feature = "async"))]
        {
            return Err(XllError::Internal {
                diagnostic_id: crate::DiagnosticId::ASYNC_FEATURE,
            });
        }
    }
    registrar
        .register_all(callbacks, descriptors)
        .map_err(|error| retain_transaction_error(runtime, error))
}

fn rollback_active_open<A>(
    runtime: &Runtime<A>,
    attempt: Option<&mut crate::runtime::OpenAttemptGuard<'_, A>>,
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
            Ok(_) => {
                let error = XllError::Internal {
                    diagnostic_id: crate::DiagnosticId::OPEN_ROLLBACK_FAILURE,
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

fn initialize_addin<A>(runtime: &Runtime<A>, context: &OpenContext) -> XllResult<RuntimeConfig>
where
    A: Addin,
{
    let opened = A::open(context).map_err(IntoXllError::into_xll_error)?;
    let (state, layers, runtime_config) = opened.into_parts();
    runtime.configure_runtime(runtime_config);
    // Stage unique ownership in OpeningGeneration before invoking add-in hooks.
    // If subsequent hooks panic, the outer boundary can roll the state back
    // through quiesce and cleanup with complete unique ownership.
    if let Err((error, state)) = runtime.stage_opening_state(state) {
        drop(layers);
        std::mem::forget(state);
        return Err(error);
    }
    let opening = runtime
        .take_opening_generation()
        .ok_or(XllError::Internal {
            diagnostic_id: crate::DiagnosticId::OPEN_STATE,
        })?;

    let opening = opening.attach_layers(layers);
    if let Err((error, opening)) = runtime.restore_opening_generation(opening) {
        std::mem::forget(opening);
        return Err(error);
    }
    Ok(runtime_config)
}

fn retain_transaction_error<A: Addin>(
    runtime: &Runtime<A>,
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
    runtime: &Runtime<A>,
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
                && crate::rtd::logical_quiescence_certified(),
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

    // Remove and quiesce Add-in state before the registry drops its published
    // object roots, matching the terminal removal ordering. Public Handle values
    // are call-scoped borrows and cannot be stored in state.
    let mut addin_state = None;
    let mut addin_quiesced = None;
    let mut generation_reclaimed = None;
    if let Some(opening) = runtime.take_opening_generation() {
        let (mut state, layers) = opening.into_parts();
        match catch_unwind(AssertUnwindSafe(|| A::quiesce(&mut state)))
            .map_err(|_| XllError::Panic)
            .and_then(|result| result.map_err(IntoXllError::into_xll_error))
        {
            Ok(()) => {
                drop(layers);
                addin_state = Some(state);
                addin_quiesced = Some(crate::shutdown::AddinQuiesced::new());
                generation_reclaimed = Some(crate::shutdown::GenerationReclaimed::new());
            }
            Err(error) => {
                report_boundary_error("xlAutoOpen rollback quiesce", &error);
                // A failed quiesce cannot prove that State-owned
                // execution resources have stopped. Preserve it until
                // the caller enters the quarantine path.
                std::mem::forget(layers);
                std::mem::forget(state);
                local_quiescent = false;
            }
        }
    } else {
        addin_quiesced = Some(crate::shutdown::AddinQuiesced::new());
        generation_reclaimed = Some(crate::shutdown::GenerationReclaimed::new());
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
                    diagnostic_id: crate::DiagnosticId::RTD_GIT_QUIESCENCE,
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
            diagnostic_id: crate::DiagnosticId::REGISTRATION_UNKNOWN,
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
        let prerequisites = crate::runtime::OpenRollbackQuiescencePrerequisites {
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
            generation_reclaimed: generation_reclaimed.expect(
                "generation reclaimed certificate is present when rollback is local-quiescent",
            ),
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
pub fn remove_addin<A>(runtime: &Runtime<A>) -> i32
where
    A: Addin,
{
    let close_result = catch_unwind(AssertUnwindSafe(|| remove_addin_inner::<A>(runtime)));
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
            #[cfg(any(test, feature = "shutdown-refinement"))]
            runtime.record_composition_already_closed_return();
            1
        }
        RemovalSuccess::Quarantined => 1,
        #[cfg(not(any(test, feature = "shutdown-refinement")))]
        RemovalSuccess::Closed {
            witness: _witness,
            removal_attempt: _removal_attempt,
        } => 1,
        #[cfg(any(test, feature = "shutdown-refinement"))]
        RemovalSuccess::Closed {
            witness,
            removal_attempt: _removal_attempt,
        } => {
            runtime
                .record_ghost_returned_success(witness)
                .unwrap_or_else(|error| {
                    handle_unload_hazard(
                        runtime,
                        crate::shutdown::UnloadHazard::CloseInvariantViolation,
                        "xlAutoRemove success refinement",
                        &error,
                    )
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
        removal_attempt: crate::runtime::RemovalAttemptGuard<'runtime, A>,
    },
}

struct QuarantineSignal;

/// Owns the terminal-removal attempt and its callback session. Cleanup is
/// explicit: the transaction is consumed only after a close certificate is
/// produced, while an active drop can only preserve quarantine.
struct RemovalTransaction<'runtime, A: Addin> {
    runtime: &'runtime Runtime<A>,
    callbacks: HostCallbackSession,
    attempt: Option<crate::runtime::RemovalAttemptGuard<'runtime, A>>,
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

    fn into_attempt(mut self) -> crate::runtime::RemovalAttemptGuard<'runtime, A> {
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

fn remove_addin_inner<'runtime, A>(runtime: &'runtime Runtime<A>) -> RemovalSuccess<'runtime, A>
where
    A: Addin,
{
    match catch_unwind(AssertUnwindSafe(|| {
        remove_addin_inner_unchecked::<A>(runtime)
    })) {
        Ok(success) => success,
        Err(payload) => {
            if payload.is::<QuarantineSignal>() {
                RemovalSuccess::Quarantined
            } else {
                std::panic::resume_unwind(payload)
            }
        }
    }
}

fn remove_addin_inner_unchecked<'runtime, A>(
    runtime: &'runtime Runtime<A>,
) -> RemovalSuccess<'runtime, A>
where
    A: Addin,
{
    #[cfg(test)]
    let _diagnostic_test_guard = crate::diagnostics::DIAGNOSTIC_TEST_MUTEX.lock();
    // Even an apparently closed runtime must pass through begin_final_removal:
    // a concurrent xlAutoOpen may already have sampled the previous close
    // epoch without having acquired its open-attempt token yet.
    let Some(mut transaction) = RemovalTransaction::begin(runtime) else {
        return RemovalSuccess::AlreadyClosed;
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
        handle_unload_hazard(
            runtime,
            crate::shutdown::UnloadHazard::SubscriptionProducerStillRunning,
            "xlAutoRemove subscription shutdown",
            &error,
        )
    });

    #[cfg(any(test, feature = "shutdown-refinement"))]
    runtime.record_ghost_event(crate::shutdown_refinement::GhostEvent::SubscriptionsDrained);

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
            diagnostic_id: crate::DiagnosticId::REGISTRATION_UNKNOWN,
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
                    function: "xlEventRegister(unregister suppressed)",
                    code: status.raw_code(),
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
    crate::callback_gate::close_from_runtime();

    #[cfg(any(test, feature = "shutdown-refinement"))]
    runtime.record_ghost_event(crate::shutdown_refinement::GhostEvent::CloseCallbackGate);

    if let Some((hazard, boundary, error)) = unload_failure.take() {
        handle_unload_hazard(runtime, hazard, boundary, &error);
    }

    let host_callbacks = crate::shutdown::HostCallbacksDetached::new();
    #[cfg(any(test, feature = "shutdown-refinement"))]
    runtime.record_ghost_event(crate::shutdown_refinement::GhostEvent::HostDetached);

    let mut addin_state = None;
    if let Some(generation) = runtime.take_generation_for_shutdown() {
        match generation {
            crate::runtime::ShutdownGeneration::Open(generation) => {
                match std::sync::Arc::try_unwrap(generation) {
                    Ok(mut generation) => {
                        if let Err(error) =
                            catch_unwind(AssertUnwindSafe(|| A::quiesce(&mut generation.state)))
                                .map_err(|_| XllError::Panic)
                                .and_then(|result| result.map_err(IntoXllError::into_xll_error))
                        {
                            report_boundary_error("xlAutoRemove quiesce", &error);
                            std::mem::forget(generation);
                            handle_unload_hazard(
                                runtime,
                                crate::shutdown::UnloadHazard::AddinQuiesceFailed,
                                "xlAutoRemove quiesce",
                                &error,
                            );
                        }
                        drop(generation.layers);
                        addin_state = Some(generation.state);
                    }
                    Err(generation) => {
                        let error = XllError::Internal {
                            diagnostic_id: crate::DiagnosticId::STATE_SCAN,
                        };
                        report_boundary_error("xlAutoRemove state escaped", &error);
                        let _ = std::sync::Arc::into_raw(generation);
                        handle_unload_hazard(
                            runtime,
                            crate::shutdown::UnloadHazard::AddinGenerationEscaped,
                            "xlAutoRemove state escaped",
                            &error,
                        );
                    }
                }
            }
            crate::runtime::ShutdownGeneration::Opening(opening) => {
                let (mut state, layers) = opening.into_parts();
                if let Err(error) = catch_unwind(AssertUnwindSafe(|| A::quiesce(&mut state)))
                    .map_err(|_| XllError::Panic)
                    .and_then(|result| result.map_err(IntoXllError::into_xll_error))
                {
                    report_boundary_error("xlAutoRemove quiesce", &error);
                    std::mem::forget(layers);
                    std::mem::forget(state);
                    handle_unload_hazard(
                        runtime,
                        crate::shutdown::UnloadHazard::AddinQuiesceFailed,
                        "xlAutoRemove quiesce",
                        &error,
                    );
                }
                drop(layers);
                addin_state = Some(state);
            }
        }
    } else {
        // A missing runtime root is the already-consumed state case. The
        // abstract proof still records uniqueness and quiescence explicitly
        // before advancing the state milestone.
    }

    let addin_quiesced = crate::shutdown::AddinQuiesced::new();
    let generation_reclaimed = crate::shutdown::GenerationReclaimed::new();

    #[cfg(any(test, feature = "shutdown-refinement"))]
    {
        runtime.record_ghost_generation_unique();
        runtime.record_ghost_addin_quiesced();
        runtime.record_ghost_event(crate::shutdown_refinement::GhostEvent::GenerationReclaimed);
    }

    let handles_quiescent = runtime.close_handles().unwrap_or_else(|error| {
        handle_unload_hazard(
            runtime,
            crate::shutdown::UnloadHazard::HandleRuntimeNotQuiescent,
            "xlAutoRemove handle table shutdown",
            &error,
        )
    });

    #[cfg(any(test, feature = "shutdown-refinement"))]
    runtime.record_ghost_event(crate::shutdown_refinement::GhostEvent::HandlesDrained);

    if let Some(mut state) = addin_state.take() {
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

    let diagnostics_stopped = crate::diagnostics::close_diagnostic_router()
        .map(|outcome| {
            for issue in outcome.issues {
                #[cfg(any(test, feature = "shutdown-refinement"))]
                runtime
                    .record_ghost_event(crate::shutdown_refinement::GhostEvent::RecordCleanupIssue);
                report_cleanup_issue(&issue);
            }
            outcome.certificate
        })
        .unwrap_or_else(|error| {
            let error = error.into_xll_error();
            handle_unload_hazard(
                runtime,
                crate::shutdown::UnloadHazard::DiagnosticWorkerStillRunning,
                "xlAutoRemove diagnostic refinement",
                &error,
            )
        });

    #[cfg(any(test, feature = "shutdown-refinement"))]
    runtime
        .record_ghost_diagnostics_stopped()
        .unwrap_or_else(|error| {
            handle_unload_hazard(
                runtime,
                crate::shutdown::UnloadHazard::DiagnosticWorkerStillRunning,
                "xlAutoRemove diagnostic refinement",
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
        handle_unload_hazard(
            runtime,
            hazard,
            "xlAutoRemove RTD GIT quiescence",
            &XllError::Internal {
                diagnostic_id: crate::DiagnosticId::RTD_GIT_QUIESCENCE,
            },
        )
    });

    #[cfg(any(test, feature = "shutdown-refinement"))]
    runtime.record_ghost_event(crate::shutdown_refinement::GhostEvent::RtdDrained);

    let certificate = runtime
        .certify_logical_quiescence(crate::runtime::RemovalQuiescencePrerequisites {
            exports: exports_drained,
            rtd: rtd_quiescent,
            host_callbacks,
            async_stopped,
            subscriptions_stopped,
            handles_quiescent,
            diagnostics_stopped,
            addin_quiesced,
            generation_reclaimed,
        })
        .unwrap_or_else(|error| {
            handle_unload_hazard(
                runtime,
                crate::shutdown::UnloadHazard::CloseInvariantViolation,
                "xlAutoRemove certification",
                &error,
            )
        });
    let closed_witness = runtime.finish_removal(certificate).unwrap_or_else(|error| {
        handle_unload_hazard(
            runtime,
            crate::shutdown::UnloadHazard::CloseInvariantViolation,
            "xlAutoRemove removal completion",
            &error,
        )
    });

    RemovalSuccess::Closed {
        witness: closed_witness,
        removal_attempt: transaction.into_attempt(),
    }
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
    runtime: &Runtime<A>,
    hazard: crate::shutdown::UnloadHazard,
    boundary: &'static str,
    error: &XllError,
) -> ! {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        tracing::error!(?hazard, %error, "unload safety could not be established");
    }));
    if hazard == crate::shutdown::UnloadHazard::CloseInvariantViolation {
        #[cfg(any(test, feature = "shutdown-refinement"))]
        runtime.ghost_fail_stop(hazard.ghost_failure());
        fail_stop_invariant(boundary, error);
    }

    quarantine_for_hazard(runtime, hazard);
    report_boundary_error(boundary, error);
    std::panic::panic_any(QuarantineSignal)
}

fn quarantine_runtime<A: Addin>(runtime: &Runtime<A>) {
    runtime.quarantine();
    #[cfg(any(test, feature = "shutdown-refinement"))]
    runtime.ghost_quarantine(crate::shutdown_refinement::GhostFailure::BoundaryPanic);
    quarantine_runtime_resources(runtime);
}

fn quarantine_for_hazard<A: Addin>(runtime: &Runtime<A>, _hazard: crate::shutdown::UnloadHazard) {
    runtime.quarantine();
    #[cfg(any(test, feature = "shutdown-refinement"))]
    runtime.ghost_quarantine(_hazard.ghost_failure());
    quarantine_runtime_resources(runtime);
}

fn quarantine_runtime_resources<A: Addin>(_runtime: &Runtime<A>) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        crate::rtd::begin_module_close();
        let ingress = crate::ingress::global_ingress();
        if matches!(
            ingress.phase(),
            crate::ingress::PHASE_OPENING | crate::ingress::PHASE_OPEN
        ) {
            ingress.begin_close_with(|| {});
        }
        if ingress.phase() == crate::ingress::PHASE_CLOSING {
            let _ = ingress.seal_and_drain();
        }
        crate::callback_gate::close_from_runtime();
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
fn fail_stop_invariant(boundary: &'static str, error: &XllError) -> ! {
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
        )
    }

    static LAYERS_PANIC_CLOSES: AtomicUsize = AtomicUsize::new(0);
    static LAYERS_PANIC_QUIESCES: AtomicUsize = AtomicUsize::new(0);

    struct LayersPanic;

    impl Addin for LayersPanic {
        type State = ();
        type Error = XllError;
        type Layers = ();

        fn open(_: &OpenContext) -> Result<crate::Opened<Self::State, Self::Layers>, Self::Error> {
            Ok(crate::Opened::new((), ()))
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
        let runtime = Runtime::<LayersPanic>::new();
        let stale_epoch = runtime.removal_epoch();

        assert_eq!(host_auto_remove::<LayersPanic>(&runtime), 1);
        assert!(runtime.begin_open_if_epoch(stale_epoch).is_err());
        assert_eq!(runtime.phase(), crate::LifecyclePhase::Closed);
    }

    #[test]
    fn failed_concurrent_open_does_not_rollback_the_owner_attempt() {
        let runtime = Runtime::<LayersPanic>::new();
        let mut owner = runtime.begin_open().unwrap();
        let mut callbacks = HostCallbackSession::new();

        rollback_active_open::<LayersPanic>(&runtime, None, &mut callbacks);
        assert_eq!(runtime.phase(), crate::LifecyclePhase::Opening);

        runtime.publish((), ());
        runtime.finish_open(&mut owner, Vec::new()).unwrap();
        assert_eq!(runtime.phase(), crate::LifecyclePhase::Open);
    }

    #[test]
    fn open_transaction_stages_state_and_layers_together() {
        LAYERS_PANIC_CLOSES.store(0, Ordering::Release);
        LAYERS_PANIC_QUIESCES.store(0, Ordering::Release);
        let runtime = Runtime::<LayersPanic>::new();
        let mut open_attempt = runtime.begin_open().unwrap();
        initialize_addin::<LayersPanic>(&runtime, &test_open_context()).unwrap();
        assert!(runtime.has_opening_generation());
        assert!(!runtime.has_current_generation());
        assert!(open_attempt.fail());
        let mut callbacks = HostCallbackSession::new();
        assert!(rollback_open::<LayersPanic>(&runtime, &mut callbacks).unload_safe());
        assert_eq!(LAYERS_PANIC_QUIESCES.load(Ordering::Acquire), 1);
        assert_eq!(LAYERS_PANIC_CLOSES.load(Ordering::Acquire), 1);
    }

    #[test]
    fn controlled_reload_reclaims_old_generation_before_new_open() {
        LAYERS_PANIC_CLOSES.store(0, Ordering::Release);
        LAYERS_PANIC_QUIESCES.store(0, Ordering::Release);
        let runtime = Runtime::<LayersPanic>::new();
        let mut first_open = runtime.begin_open().unwrap();
        runtime.publish((), ());
        runtime.finish_open(&mut first_open, Vec::new()).unwrap();
        let first_generation = runtime.generation();

        assert_eq!(remove_addin::<LayersPanic>(&runtime), 1);
        assert_eq!(runtime.phase(), crate::LifecyclePhase::Closed);
        runtime.clear_host_intent();
        let mut second_open = runtime.begin_open().unwrap();
        runtime.publish((), ());
        runtime.finish_open(&mut second_open, Vec::new()).unwrap();
        assert_eq!(runtime.phase(), crate::LifecyclePhase::Open);
        assert!(runtime.generation() > first_generation);
        assert_eq!(LAYERS_PANIC_QUIESCES.load(Ordering::Acquire), 1);
        assert_eq!(host_auto_remove::<LayersPanic>(&runtime), 1);
        assert_eq!(runtime.phase(), crate::LifecyclePhase::Closed);
    }

    struct ReloadFailure;

    impl Addin for ReloadFailure {
        type State = ();
        type Error = XllError;
        type Layers = ();

        fn open(_: &OpenContext) -> Result<crate::Opened<Self::State, Self::Layers>, Self::Error> {
            Err(XllError::Internal {
                diagnostic_id: crate::DiagnosticId::OPEN_STATE,
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
        assert_eq!(runtime.phase(), crate::LifecyclePhase::Quarantined);
        assert_eq!(host_auto_close::<ReloadFailure>(&runtime), 1);
        assert_eq!(runtime.phase(), crate::LifecyclePhase::Quarantined);
    }

    #[cfg(feature = "async")]
    static WORKERS_PANIC_CLOSES: AtomicUsize = AtomicUsize::new(0);

    #[cfg(feature = "async")]
    struct WorkersPanic;

    #[cfg(feature = "async")]
    impl Addin for WorkersPanic {
        type State = ();
        type Error = XllError;
        type Layers = ();

        fn open(_: &OpenContext) -> Result<crate::Opened<Self::State, Self::Layers>, Self::Error> {
            Ok(crate::Opened::new((), ())
                .with_runtime_config(crate::RuntimeConfig::new().with_async_worker_count(64)))
        }

        fn cleanup(_: &mut Self::State, _: &mut crate::CleanupReporter<'_>) {
            WORKERS_PANIC_CLOSES.fetch_add(1, Ordering::AcqRel);
        }
    }

    #[cfg(feature = "async")]
    #[test]
    fn open_transaction_clamps_async_worker_policy() {
        WORKERS_PANIC_CLOSES.store(0, Ordering::Release);
        let runtime = Runtime::<WorkersPanic>::new();
        let _open_attempt = runtime.begin_open().unwrap();
        initialize_addin::<WorkersPanic>(&runtime, &test_open_context()).unwrap();
        assert!(runtime.has_opening_generation());
    }

    impl Addin for RetryClose {
        type State = RetryState;
        type Error = XllError;
        type Layers = ();

        fn open(
            _context: &OpenContext,
        ) -> Result<crate::Opened<Self::State, Self::Layers>, Self::Error> {
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
                    diagnostic_id: crate::DiagnosticId::TEST_RETRY,
                },
            );
        }
    }

    #[test]
    fn addin_cleanup_issue_does_not_prevent_finalizing_runtime() {
        let runtime = Runtime::<RetryClose>::new();
        let attempts = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut open_attempt = runtime.begin_open().unwrap();
        runtime.publish(
            RetryState {
                attempts: std::sync::Arc::clone(&attempts),
            },
            (),
        );
        runtime.finish_open(&mut open_attempt, Vec::new()).unwrap();

        remove_addin_inner::<RetryClose>(&runtime);
        assert_eq!(runtime.phase(), crate::LifecyclePhase::Closed);
        assert_eq!(attempts.load(std::sync::atomic::Ordering::Acquire), 1);
        assert!(
            runtime.take_current_generation().is_none()
                && runtime.take_opening_generation().is_none()
        );
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
        type Layers = ();

        fn open(_: &OpenContext) -> Result<crate::Opened<Self::State, Self::Layers>, Self::Error> {
            unreachable!()
        }

        fn cleanup(_: &mut Self::State, _: &mut crate::CleanupReporter<'_>) {
            panic!("injected cleanup panic");
        }
    }

    #[test]
    fn cleanup_panic_leaks_state_and_still_finalizes_safe_unload() {
        let runtime = Runtime::<CleanupPanic>::new();
        let drops = std::sync::Arc::new(AtomicUsize::new(0));
        let mut opening = runtime.begin_open().unwrap();
        runtime.publish(DropObserved(std::sync::Arc::clone(&drops)), ());
        runtime.finish_open(&mut opening, Vec::new()).unwrap();

        remove_addin_inner::<CleanupPanic>(&runtime);

        assert_eq!(runtime.phase(), crate::LifecyclePhase::Closed);
        assert_eq!(drops.load(Ordering::Acquire), 0);
    }

    struct QuiesceFailure;

    impl Addin for QuiesceFailure {
        type State = DropObserved;
        type Error = XllError;
        type Layers = ();

        fn open(_: &OpenContext) -> Result<crate::Opened<Self::State, Self::Layers>, Self::Error> {
            unreachable!()
        }

        fn quiesce(_: &mut Self::State) -> Result<(), Self::Error> {
            Err(XllError::Internal {
                diagnostic_id: crate::DiagnosticId::QUIESCENCE_FAILURE,
            })
        }
    }

    #[test]
    fn quiesce_failure_enters_quarantine_without_dropping_state() {
        let runtime = Runtime::<QuiesceFailure>::new();
        let drops = std::sync::Arc::new(AtomicUsize::new(0));
        let mut opening = runtime.begin_open().unwrap();
        runtime.publish(DropObserved(std::sync::Arc::clone(&drops)), ());
        runtime.finish_open(&mut opening, Vec::new()).unwrap();

        let result = { remove_addin_inner::<QuiesceFailure>(&runtime) };

        assert!(matches!(result, RemovalSuccess::Quarantined));
        assert_eq!(runtime.phase(), crate::LifecyclePhase::Quarantined);
        assert_eq!(drops.load(Ordering::Acquire), 0);
        assert_eq!(host_auto_close::<QuiesceFailure>(&runtime), 1);
        assert_eq!(runtime.phase(), crate::LifecyclePhase::Quarantined);
    }

    #[test]
    fn open_rollback_cleanup_issue_still_finalizes_without_reinstalling_state() {
        let runtime = Runtime::<RetryClose>::new();
        let attempts = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut open_attempt = runtime.begin_open().unwrap();
        runtime.publish(
            RetryState {
                attempts: std::sync::Arc::clone(&attempts),
            },
            (),
        );

        assert!(open_attempt.fail());
        let mut callbacks = HostCallbackSession::new();
        let outcome = rollback_open::<RetryClose>(&runtime, &mut callbacks);
        assert!(outcome.unload_safe());
        assert!(outcome.finalized);
        assert_eq!(runtime.phase(), crate::LifecyclePhase::Closed);
        assert_eq!(attempts.load(std::sync::atomic::Ordering::Acquire), 1);
        assert!(
            runtime.take_current_generation().is_none()
                && runtime.take_opening_generation().is_none()
        );
    }

    struct CleanClose;

    impl Addin for CleanClose {
        type State = ();
        type Error = XllError;
        type Layers = ();

        fn open(
            _context: &OpenContext,
        ) -> Result<crate::Opened<Self::State, Self::Layers>, Self::Error> {
            unreachable!()
        }
    }

    struct TraceCleanup;

    impl Addin for TraceCleanup {
        type State = ();
        type Error = XllError;
        type Layers = ();

        fn open(
            _context: &OpenContext,
        ) -> Result<crate::Opened<Self::State, Self::Layers>, Self::Error> {
            unreachable!()
        }

        fn cleanup(_state: &mut Self::State, reporter: &mut crate::CleanupReporter<'_>) {
            reporter.warn(
                "Lean checker cleanup trace",
                crate::CleanupIssueKind::RegistryCleanup,
                XllError::Internal {
                    diagnostic_id: crate::DiagnosticId::LEAN_TRACE,
                },
            );
        }
    }

    struct TraceHandle;

    impl crate::handle::ExcelHandleObject for TraceHandle {}

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
        type Subscription = TraceSubscription;

        fn subscribe(
            &self,
            _topic: &crate::RtdTopic,
            sink: crate::RtdSink<Self::Value>,
        ) -> XllResult<Self::Subscription> {
            self.sink.lock().unwrap().replace(sink);
            Ok(TraceSubscription)
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

        let static_fixture = crate::runtime::StaticTestRuntime::<TraceCleanup>::new();
        let runtime = static_fixture.runtime();
        let mut opening = runtime.begin_open().unwrap();
        runtime.publish((), ());
        runtime.finish_open(&mut opening, Vec::new()).unwrap();

        crate::diagnostics::reset_diagnostic_router().unwrap();
        crate::diagnostics::set_diagnostic_sink(TraceDiagnosticSink).unwrap();
        crate::diagnostics::report_no_unwind("lean_checker_trace", &XllError::Panic);

        let pointer = crate::ffi_boundary(runtime, || Ok::<f64, XllError>(1.0));
        // SAFETY: `pointer` is the live DLL-owned block returned by the
        // framework boundary above and is freed exactly once here.
        let free = unsafe { crate::free_return_boundary(pointer) };
        drop(free);

        let handles = runtime.handles().unwrap();
        handles
            .prepare(crate::handle::test_topic_key("lean-checker-handle"), || {
                Ok(TraceHandle)
            })
            .unwrap();

        let notifier_state =
            std::sync::Arc::new(crate::rtd::test_support::TestNotifierState::new());
        let subscriptions = runtime.subscriptions();
        let subscriptions = subscriptions.as_arc();
        let server = subscriptions
            .register_server(crate::subscription::ServerGeneration(1))
            .unwrap();
        server
            .attach_update_notifier(crate::rtd::RtdNotifier::for_test(std::sync::Arc::clone(
                &notifier_state,
            )))
            .unwrap();
        let trace_sink = std::sync::Arc::new(std::sync::Mutex::new(None));
        let source = crate::RtdSourceHandle::new(TraceSource {
            sink: std::sync::Arc::clone(&trace_sink),
        })
        .unwrap();
        let prepared = subscriptions
            .prepare(
                &source,
                crate::RtdTopic::single("lean-checker-subscription").unwrap(),
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
        failure_runtime.publish(DropObserved(std::sync::Arc::clone(&drops)), ());
        failure_runtime
            .finish_open(&mut opening, Vec::new())
            .unwrap();
        assert_eq!(host_auto_remove::<QuiesceFailure>(&failure_runtime), 1);
        assert_eq!(failure_runtime.phase(), crate::LifecyclePhase::Quarantined);
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
        runtime.record_ghost_event(crate::shutdown_refinement::GhostEvent::EnterCall);
        runtime.record_ghost_event(crate::shutdown_refinement::GhostEvent::LeaveCall);

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
        assert_eq!(host_auto_remove::<CleanClose>(&uncommitted), 1);
        check(
            "uncommitted final close",
            uncommitted.composition_trace_json(),
            "rust-composition-uncommitted-trace.json",
        );

        let rollback = Runtime::<CleanClose>::new();
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
        let runtime = Runtime::<CleanClose>::new();
        let mut opening = runtime.begin_open().unwrap();
        runtime.publish((), ());
        runtime.finish_open(&mut opening, Vec::new()).unwrap();

        let success = remove_addin_inner::<CleanClose>(&runtime);
        assert!(runtime.begin_open().is_err());
        let RemovalSuccess::Closed {
            witness,
            removal_attempt,
        } = success
        else {
            panic!("test close must own the close attempt");
        };
        runtime.record_ghost_returned_success(witness).unwrap();
        drop(removal_attempt);

        let mut reopened = runtime.begin_open().unwrap();
        runtime.publish((), ());
        runtime.finish_open(&mut reopened, Vec::new()).unwrap();
        assert_eq!(runtime.phase(), crate::LifecyclePhase::Open);
    }

    struct AlwaysFailClose;

    impl Addin for AlwaysFailClose {
        type State = ();
        type Error = XllError;
        type Layers = ();

        fn open(
            _context: &OpenContext,
        ) -> Result<crate::Opened<Self::State, Self::Layers>, Self::Error> {
            unreachable!()
        }

        fn cleanup(_state: &mut Self::State, reporter: &mut crate::CleanupReporter<'_>) {
            reporter.warn(
                "always fail cleanup",
                crate::CleanupIssueKind::RegistryCleanup,
                XllError::Internal {
                    diagnostic_id: crate::DiagnosticId::FAILURE,
                },
            );
        }
    }

    #[test]
    fn failing_open_rollback_is_finalized_by_xl_auto_close() {
        let runtime = Runtime::<AlwaysFailClose>::new();
        let mut open_attempt = runtime.begin_open().unwrap();
        runtime.publish((), ());

        assert!(open_attempt.fail());
        let mut callbacks = HostCallbackSession::new();
        assert!(rollback_open::<AlwaysFailClose>(&runtime, &mut callbacks).unload_safe());
        assert_eq!(runtime.phase(), crate::LifecyclePhase::Closed);

        assert_eq!(host_auto_remove::<AlwaysFailClose>(&runtime), 1);
        assert_eq!(runtime.phase(), crate::LifecyclePhase::Closed);
    }

    #[test]
    fn xl_auto_close_waits_for_active_call_and_returns_one_after_clean_close() {
        let runtime = std::sync::Arc::new(Runtime::<CleanClose>::new());
        let mut open_attempt = runtime.begin_open().unwrap();
        runtime.publish((), ());
        runtime.finish_open(&mut open_attempt, Vec::new()).unwrap();
        let (_export_guard, accepted) = crate::ingress::global_ingress().enter_udf_with(|| {});
        assert!(accepted);
        let call = runtime.enter().unwrap();
        let closer_runtime = std::sync::Arc::clone(&runtime);
        let (closed_tx, closed_rx) = std::sync::mpsc::channel();
        let closer = std::thread::spawn(move || {
            closed_tx
                .send(host_auto_remove::<CleanClose>(&closer_runtime))
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

    #[test]
    fn xl_auto_close_is_a_hint_until_explicit_removal() {
        let runtime = Runtime::<CleanClose>::new();
        let mut open_attempt = runtime.begin_open().unwrap();
        runtime.publish((), ());
        runtime.finish_open(&mut open_attempt, Vec::new()).unwrap();

        assert_eq!(host_auto_close::<CleanClose>(&runtime), 1);
        assert_eq!(runtime.phase(), crate::LifecyclePhase::Open);
        assert!(runtime.enter().is_ok());

        assert_eq!(host_auto_remove::<CleanClose>(&runtime), 1);
        assert_eq!(runtime.phase(), crate::LifecyclePhase::Closed);
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
        assert_eq!(runtime.phase(), crate::LifecyclePhase::Closed);
        assert_eq!(
            runtime.host_intent(),
            crate::runtime::HostLifecycleIntent::ExplicitRemovalComplete
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
        let close = remove_addin_inner::<CleanClose>(runtime);

        assert!(matches!(close, RemovalSuccess::Quarantined));
        assert_eq!(runtime.phase(), crate::LifecyclePhase::Quarantined);
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
        type Subscription = OrderedSubscription;

        fn subscribe(
            &self,
            _topic: &crate::RtdTopic,
            _sink: crate::RtdSink<Self::Value>,
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
        type State = OrderedState;
        type Error = XllError;
        type Layers = ();

        fn open(
            _context: &OpenContext,
        ) -> Result<crate::Opened<Self::State, Self::Layers>, Self::Error> {
            unreachable!()
        }

        fn cleanup(state: &mut Self::State, _: &mut crate::CleanupReporter<'_>) {
            state.events.lock().unwrap().push("state");
        }
    }

    #[test]
    fn runtime_owned_subscriptions_and_handles_drop_before_addin_state_closes() {
        let runtime = Runtime::<OrderedClose>::new();
        let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut open_attempt = runtime.begin_open().unwrap();
        runtime.publish(
            OrderedState {
                events: std::sync::Arc::clone(&events),
            },
            (),
        );
        runtime.finish_open(&mut open_attempt, Vec::new()).unwrap();
        runtime
            .handles()
            .unwrap()
            .prepare(crate::handle::test_topic_key("ordered"), || {
                Ok(OrderedHandle {
                    events: std::sync::Arc::clone(&events),
                })
            })
            .unwrap();
        let subscriptions = runtime.subscriptions();
        let subscriptions = subscriptions.as_arc();
        let server = subscriptions
            .register_server(crate::subscription::ServerGeneration(1))
            .unwrap();
        let source = crate::RtdSourceHandle::new(OrderedSource {
            events: std::sync::Arc::clone(&events),
        })
        .unwrap();
        let prepared = subscriptions
            .prepare(&source, crate::RtdTopic::single("ordered").unwrap())
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
    }

    impl Drop for StagedRaceState {
        fn drop(&mut self) {
            self.dropped.fetch_add(1, Ordering::SeqCst);
        }
    }

    struct StagedRaceAddin;

    impl Addin for StagedRaceAddin {
        type State = StagedRaceState;
        type Error = XllError;
        type Layers = ();

        fn open(
            _context: &OpenContext,
        ) -> Result<crate::Opened<Self::State, Self::Layers>, Self::Error> {
            unreachable!()
        }

        fn quiesce(state: &mut Self::State) -> Result<(), Self::Error> {
            state.quiesced.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        fn cleanup(state: &mut Self::State, _: &mut crate::CleanupReporter<'_>) {
            state.cleaned.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn close_reclaims_staged_opening_generation_when_finish_open_loses_race() {
        let runtime = std::sync::Arc::new(Runtime::<StagedRaceAddin>::new());
        let quiesced = std::sync::Arc::new(AtomicUsize::new(0));
        let cleaned = std::sync::Arc::new(AtomicUsize::new(0));
        let dropped = std::sync::Arc::new(AtomicUsize::new(0));

        let mut open_attempt = runtime.begin_open().unwrap();
        let state = StagedRaceState {
            quiesced: std::sync::Arc::clone(&quiesced),
            cleaned: std::sync::Arc::clone(&cleaned),
            dropped: std::sync::Arc::clone(&dropped),
        };
        assert!(runtime.stage_opening_state(state).is_ok());
        let opening = runtime.take_opening_generation().unwrap();
        let opening = opening.attach_layers(());
        assert!(runtime.restore_opening_generation(opening).is_ok());

        let closer_runtime = std::sync::Arc::clone(&runtime);
        let (closed_tx, closed_rx) = std::sync::mpsc::channel();
        let closer = std::thread::spawn(move || {
            closed_tx
                .send(host_auto_remove::<StagedRaceAddin>(&closer_runtime))
                .unwrap();
        });

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        while runtime.phase() != crate::LifecyclePhase::Closing
            && std::time::Instant::now() < deadline
        {
            std::thread::yield_now();
        }
        assert_eq!(runtime.phase(), crate::LifecyclePhase::Closing);

        assert!(matches!(
            runtime.finish_open(&mut open_attempt, Vec::new()),
            Err(XllError::Closing)
        ));
        assert!(!open_attempt.is_active());

        assert_eq!(
            closed_rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap(),
            1
        );
        closer.join().unwrap();

        assert_eq!(runtime.phase(), crate::LifecyclePhase::Closed);
        assert_eq!(quiesced.load(Ordering::SeqCst), 1);
        assert_eq!(cleaned.load(Ordering::SeqCst), 1);
        assert_eq!(dropped.load(Ordering::SeqCst), 1);
        assert!(runtime.take_current_generation().is_none());
        assert!(runtime.take_opening_generation().is_none());
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
        type State = PanicLayersState;
        type Error = XllError;
        type Layers = ();

        fn open(
            _context: &OpenContext,
        ) -> Result<crate::Opened<Self::State, Self::Layers>, Self::Error> {
            unreachable!()
        }

        fn quiesce(state: &mut Self::State) -> Result<(), Self::Error> {
            state.quiesced.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        fn cleanup(state: &mut Self::State, _: &mut crate::CleanupReporter<'_>) {
            state.cleaned.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn complete_opening_generation_restores_for_rollback() {
        let runtime = Runtime::<PanicLayersAddin>::new();
        let quiesced = std::sync::Arc::new(AtomicUsize::new(0));
        let cleaned = std::sync::Arc::new(AtomicUsize::new(0));
        let dropped = std::sync::Arc::new(AtomicUsize::new(0));

        let mut open_attempt = runtime.begin_open().unwrap();
        let state = PanicLayersState {
            quiesced: std::sync::Arc::clone(&quiesced),
            cleaned: std::sync::Arc::clone(&cleaned),
            dropped: std::sync::Arc::clone(&dropped),
        };
        assert!(runtime.stage_opening_state(state).is_ok());
        let opening = runtime.take_opening_generation().unwrap();

        let opening = opening.attach_layers(());
        assert!(runtime.restore_opening_generation(opening).is_ok());
        assert!(runtime.has_opening_generation());

        assert!(open_attempt.fail());
        let mut callbacks = HostCallbackSession::new();
        let outcome = rollback_open::<PanicLayersAddin>(&runtime, &mut callbacks);
        assert!(outcome.unload_safe());
        assert_eq!(runtime.phase(), crate::LifecyclePhase::Closed);

        assert_eq!(quiesced.load(Ordering::SeqCst), 1);
        assert_eq!(cleaned.load(Ordering::SeqCst), 1);
        assert_eq!(dropped.load(Ordering::SeqCst), 1);
        assert!(runtime.take_opening_generation().is_none());
    }
}

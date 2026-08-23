//! Open-attempt rollback transition and its terminal proof.
//!
//! This module owns the rollback-specific state and cleanup pipeline. The
//! lifecycle root retains only the public boundary and shared cleanup helpers.

use super::teardown;
use super::{drain_execution, lifecycle_access_error, report_boundary_error, report_cleanup_issue};
use crate::XllError;
use crate::addin::Addin;
use crate::error::IntoXllError;
use crate::generation::RuntimeGeneration;
use crate::host_callback::HostCallbackSession;
use crate::registration::HostRegistrar;
use crate::runtime::{AddinLifecycleAccess, Runtime};
use std::panic::{AssertUnwindSafe, catch_unwind};

pub(super) enum OpenRollbackStatus {
    /// Every terminal certificate was issued and the lifecycle binding was
    /// released. A later open may safely reuse the runtime.
    Finalized,
    /// Rollback did not produce the complete terminal certificate. The
    /// caller must retain the fail-safe pending state and quarantine policy.
    Incomplete,
}

pub(super) struct OpenRollbackOutcome {
    status: OpenRollbackStatus,
}

pub(super) fn active_runtime_generation<A: Addin>(
    runtime: &Runtime<A>,
) -> Option<RuntimeGeneration> {
    runtime.protocol_generation()
}

impl OpenRollbackOutcome {
    pub(super) fn unload_safe(&self) -> bool {
        matches!(self.status, OpenRollbackStatus::Finalized)
    }

    #[cfg(test)]
    pub(super) fn is_finalized(&self) -> bool {
        self.unload_safe()
    }
}

pub(super) fn rollback_open<A>(
    runtime: &Runtime<A>,
    lifecycle: &AddinLifecycleAccess<'_, A>,
    callbacks: &mut HostCallbackSession,
    generation: Option<RuntimeGeneration>,
) -> OpenRollbackOutcome
where
    A: Addin,
{
    #[cfg(test)]
    let _diagnostic_test_guard = crate::diagnostics::DIAGNOSTIC_TEST_MUTEX.lock();
    let Some(_rollback_attempt) = runtime.acquire_open_rollback() else {
        let finalized = runtime.phase() == crate::lifecycle::LifecyclePhase::Closed
            && runtime.host_callbacks_detached()
            && !runtime.registration_state_unknown()
            && crate::rtd::logical_quiescence_certified();
        return OpenRollbackOutcome {
            status: if finalized {
                OpenRollbackStatus::Finalized
            } else {
                OpenRollbackStatus::Incomplete
            },
        };
    };
    crate::module_runtime::global().begin_close(|| {});

    let mut local_quiescent = true;
    let mut lifecycle_release_ready = match runtime.has_addin_lifecycle(lifecycle) {
        Ok(present) => !present,
        Err(error) => {
            report_boundary_error("xlAutoOpen lifecycle slot", &lifecycle_access_error(error));
            false
        }
    };
    // An opening transaction normally owns the lifecycle payload. It is only
    // safe to release the thread binding after that payload has been
    // explicitly taken and dropped below.
    let execution_drained = drain_execution(runtime, false);

    let producers_stopped = match execution_drained.stop_producers(runtime, |issue| {
        report_cleanup_issue(issue);
    }) {
        Ok(stage) => Some(stage),
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
            kind: crate::shutdown::CleanupIssueKind::HostMemoryLeak,
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
                kind: crate::shutdown::CleanupIssueKind::HostMemoryLeak,
                error: error.clone(),
            });
        }
        runtime.retain_failed_event_registrations(event_outcome.failed);
    } else if !events.is_empty() {
        let error = callbacks
            .terminal_status()
            .map(|status| XllError::ExcelApi {
                function: crate::error::ExcelApiFunction::EventRegister,
                failure: crate::error::ExcelApiFailure::Suppressed(status),
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

    crate::module_runtime::global().close_callbacks();

    // Remove and quiesce Add-in state before the registry drops its published
    // object roots, matching the terminal removal ordering. Public Handle values
    // are call-scoped borrows and cannot be stored in state.
    let mut addin_shared_state = None;
    let mut addin_quiesced = None;
    let mut generation_reclaimed = None;
    if let Some(opening) = runtime.take_opening_for_rollback() {
        let (mut shared_state, layers, _config) = opening.into_parts();
        match catch_unwind(AssertUnwindSafe(|| {
            runtime
                .with_addin_lifecycle(lifecycle, |lifecycle_state| {
                    A::quiesce(&mut shared_state, lifecycle_state)
                        .map_err(IntoXllError::into_xll_error)
                })
                .map_err(lifecycle_access_error)?
        }))
        .map_err(|_| XllError::Panic)
        .and_then(|result| result)
        {
            Ok(()) => {
                drop(layers);
                addin_shared_state = Some(shared_state);
                addin_quiesced = Some(crate::shutdown::AddinQuiesced::new());
                generation_reclaimed = Some(crate::shutdown::GenerationReclaimed::new());
            }
            Err(error) => {
                report_boundary_error("xlAutoOpen rollback quiesce", &error);
                // A failed quiesce cannot prove that shared-state-owned
                // execution resources have stopped. Preserve it until
                // the caller enters the quarantine path.
                runtime.quarantine_layers(
                    generation,
                    layers,
                    crate::runtime_components::QuarantineReason::AddinQuiesceFailed,
                );
                runtime.quarantine_shared_state(
                    generation,
                    shared_state,
                    crate::runtime_components::QuarantineReason::AddinQuiesceFailed,
                );
                local_quiescent = false;
            }
        }
    } else {
        addin_quiesced = Some(crate::shutdown::AddinQuiesced::new());
        generation_reclaimed = Some(crate::shutdown::GenerationReclaimed::new());
    }

    let mut handles_sealed = if local_quiescent {
        match runtime.seal_formula_handle_service() {
            Ok(token) => Some(token),
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
                    diagnostic_id: crate::error::DiagnosticId::RTD_GIT_QUIESCENCE,
                };
                report_boundary_error("xlAutoOpen RTD quiescence rollback", &error);
                local_quiescent = false;
                None
            }
        }
    } else {
        None
    };

    if local_quiescent && let Some(shared_state) = addin_shared_state.take() {
        let mut report = crate::shutdown::CloseReport::default();
        let cleanup = catch_unwind(AssertUnwindSafe(|| {
            runtime
                .with_addin_lifecycle(lifecycle, |lifecycle_state| {
                    let mut reporter = crate::shutdown::CleanupReporter::new(&mut report);
                    A::cleanup(lifecycle_state, &mut reporter);
                })
                .map_err(lifecycle_access_error)
        }));
        if cleanup.is_err() || cleanup.as_ref().is_ok_and(|result| result.is_err()) {
            report.push(
                "Addin::cleanup",
                crate::shutdown::CleanupIssueKind::DisposalPanicked,
                XllError::Panic,
            );
            runtime.quarantine_shared_state(
                generation,
                shared_state,
                crate::runtime_components::QuarantineReason::AddinCleanupPanicked,
            );
            local_quiescent = false;
        } else {
            let lifecycle_dropped = match runtime.take_addin_lifecycle(lifecycle) {
                Ok(lifecycle_state) => {
                    if catch_unwind(AssertUnwindSafe(|| drop(lifecycle_state))).is_err() {
                        report.push(
                            "Addin::LifecycleState::drop",
                            crate::shutdown::CleanupIssueKind::DisposalPanicked,
                            XllError::Panic,
                        );
                        false
                    } else {
                        true
                    }
                }
                Err(error) => {
                    report.push(
                        "Addin::LifecycleState",
                        crate::shutdown::CleanupIssueKind::DisposalPanicked,
                        lifecycle_access_error(error),
                    );
                    false
                }
            };
            let shared_state_dropped =
                catch_unwind(AssertUnwindSafe(|| drop(shared_state))).is_ok();
            if !shared_state_dropped {
                report.push(
                    "Addin::SharedState::drop",
                    crate::shutdown::CleanupIssueKind::DisposalPanicked,
                    XllError::Panic,
                );
            }
            lifecycle_release_ready = lifecycle_dropped && shared_state_dropped;
            if !lifecycle_release_ready {
                local_quiescent = false;
            }
        }
        for issue in report.issues() {
            report_cleanup_issue(issue);
        }
    }
    if let Some(shared_state) = addin_shared_state {
        runtime.quarantine_shared_state(
            generation,
            shared_state,
            crate::runtime_components::QuarantineReason::BoundaryFailure,
        );
    }

    if local_quiescent && let Err(error) = runtime.shutdown_rtd() {
        report_boundary_error("xlAutoOpen RTD rollback", &error);
        local_quiescent = false;
    }

    let handle_store_quiescent = if local_quiescent {
        match runtime.finish_formula_handle_quiescence(
            handles_sealed
                .take()
                .expect("handle seal token is present when rollback is local-quiescent"),
        ) {
            Ok(certificate) => Some(certificate),
            Err(error) => {
                report_boundary_error("xlAutoOpen handle pin rollback", &error);
                local_quiescent = false;
                None
            }
        }
    } else {
        None
    };

    if runtime.registration_state_unknown() {
        let error = XllError::Internal {
            diagnostic_id: crate::error::DiagnosticId::REGISTRATION_UNKNOWN,
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
    let host_callbacks_detached = runtime.host_callbacks_detached();
    let registration_state_known = !runtime.registration_state_unknown();
    let mut finalized = false;
    if local_quiescent
        && lifecycle_release_ready
        && host_callbacks_detached
        && registration_state_known
    {
        let proof = teardown::ResourcesReclaimed::new(
            producers_stopped.expect("producer stage is present when rollback is local-quiescent"),
            rtd_quiescent.expect("RTD certificate is present when rollback is local-quiescent"),
            crate::shutdown::HostCallbacksDetached::new(),
            handle_store_quiescent
                .expect("handle certificate is present when rollback is local-quiescent"),
            diagnostics_stopped
                .expect("diagnostic certificate is present when rollback is local-quiescent"),
            addin_quiesced.expect("addin certificate is present when rollback is local-quiescent"),
            generation_reclaimed.expect(
                "generation reclaimed certificate is present when rollback is local-quiescent",
            ),
        )
        .into_proof();
        match runtime
            .certify::<crate::runtime::OpenRollback>(proof)
            .and_then(|certificate| runtime.finish_open_rollback(certificate))
        {
            Ok(()) => match runtime.release_empty_addin_lifecycle(lifecycle) {
                Ok(()) => finalized = true,
                Err(error) => {
                    report_boundary_error(
                        "xlAutoOpen lifecycle binding release",
                        &lifecycle_access_error(error),
                    );
                }
            },
            Err(error) => {
                report_boundary_error("xlAutoOpen rollback certification", &error);
            }
        }
    }
    OpenRollbackOutcome {
        status: if finalized {
            OpenRollbackStatus::Finalized
        } else {
            OpenRollbackStatus::Incomplete
        },
    }
}

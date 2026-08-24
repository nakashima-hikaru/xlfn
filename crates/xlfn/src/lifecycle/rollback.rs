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

fn incomplete<A: Addin>(runtime: &Runtime<A>) -> OpenRollbackOutcome {
    runtime.quarantine();
    OpenRollbackOutcome {
        status: OpenRollbackStatus::Incomplete,
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
    let Some(rollback_attempt) = runtime.acquire_open_rollback() else {
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

    let lifecycle_present = match runtime.has_addin_lifecycle(lifecycle) {
        Ok(present) => present,
        Err(error) => {
            report_boundary_error("xlAutoOpen lifecycle slot", &lifecycle_access_error(error));
            return incomplete(runtime);
        }
    };
    // An opening transaction normally owns the lifecycle payload. It is only
    // safe to release the thread binding after that payload has been
    // explicitly taken and dropped below.
    let execution_drained = drain_execution(runtime, false);
    let teardown: teardown::TeardownTxn<
        '_,
        A,
        crate::runtime::OpenRollback,
        teardown::ExecutionDrained,
    > = teardown::TeardownTxn::new(rollback_attempt, execution_drained);
    let teardown = match teardown.stop_producers(|issue| {
        report_cleanup_issue(issue);
    }) {
        Ok(stage) => stage,
        Err(error) => {
            report_boundary_error("xlAutoOpen subscription rollback", &error);
            return incomplete(runtime);
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

    if runtime.registration_state_unknown() {
        let error = XllError::Internal {
            diagnostic_id: crate::error::DiagnosticId::REGISTRATION_UNKNOWN,
        };
        report_boundary_error("xlAutoOpen registration state unknown", &error);
        return incomplete(runtime);
    }
    if !runtime.host_callbacks_detached() {
        let error = XllError::Internal {
            diagnostic_id: crate::error::DiagnosticId::REGISTRATION_UNKNOWN,
        };
        report_boundary_error("xlAutoOpen callbacks remain registered", &error);
        return incomplete(runtime);
    }
    let host_callbacks = crate::shutdown::HostCallbacksDetached::new();

    // Remove and quiesce Add-in state before the registry drops its published
    // object roots, matching the terminal removal ordering. Public Handle values
    // are call-scoped borrows and cannot be stored in state.
    let addin = if let Some(opening) = runtime.take_opening_for_rollback() {
        let (mut shared_state, layers, _config) = opening.into_parts();
        let quiesce = catch_unwind(AssertUnwindSafe(|| {
            runtime
                .with_addin_lifecycle(lifecycle, |lifecycle_state| {
                    A::quiesce(&mut shared_state, lifecycle_state)
                        .map_err(IntoXllError::into_xll_error)
                })
                .map_err(lifecycle_access_error)?
        }))
        .map_err(|_| XllError::Panic)
        .and_then(|result| result);
        if let Err(error) = quiesce {
            report_boundary_error("xlAutoOpen rollback quiesce", &error);
            // A failed quiesce cannot prove that shared-state-owned execution
            // resources have stopped. Preserve both parts until quarantine.
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
            return incomplete(runtime);
        }
        drop(layers);
        teardown::QuiescedAddin::shared(runtime, generation, shared_state)
    } else {
        if lifecycle_present {
            let error = XllError::Internal {
                diagnostic_id: crate::error::DiagnosticId::LIFECYCLE_SLOT,
            };
            report_boundary_error("xlAutoOpen lifecycle rollback state", &error);
            return incomplete(runtime);
        }
        teardown::QuiescedAddin::empty(runtime, generation)
    };

    let teardown = match teardown.seal_services(addin) {
        Ok(stage) => stage,
        Err(error) => {
            report_boundary_error("xlAutoOpen handle rollback", &error);
            return incomplete(runtime);
        }
    };

    let rtd_quiescent = match crate::rtd::wait_for_module_quiescence() {
        Ok(certificate) => certificate,
        Err(_) => {
            let error = XllError::Internal {
                diagnostic_id: crate::error::DiagnosticId::RTD_GIT_QUIESCENCE,
            };
            report_boundary_error("xlAutoOpen RTD quiescence rollback", &error);
            return incomplete(runtime);
        }
    };

    let mut cleanup_report = crate::shutdown::CloseReport::default();
    let teardown = match teardown.cleanup_addin(lifecycle, &mut cleanup_report) {
        Ok(stage) => stage,
        Err(error) => {
            for issue in cleanup_report.issues() {
                report_cleanup_issue(issue);
            }
            report_boundary_error("xlAutoOpen rollback cleanup", &error);
            return incomplete(runtime);
        }
    };
    for issue in cleanup_report.issues() {
        report_cleanup_issue(issue);
    }

    if let Err(error) = runtime.shutdown_handle_topics() {
        report_boundary_error("xlAutoOpen RTD rollback", &error);
        return incomplete(runtime);
    }

    let teardown = match teardown.finish_services() {
        Ok(stage) => stage,
        Err(error) => {
            report_boundary_error("xlAutoOpen handle pin rollback", &error);
            return incomplete(runtime);
        }
    };

    let diagnostics_stopped = match crate::diagnostics::close_diagnostic_router() {
        Ok(outcome) => {
            for issue in outcome.issues {
                report_cleanup_issue(&issue);
            }
            outcome.certificate
        }
        Err(error) => {
            let error = error.into_xll_error();
            report_boundary_error("xlAutoOpen diagnostic rollback", &error);
            return incomplete(runtime);
        }
    };

    let teardown = teardown.reclaim(rtd_quiescent, host_callbacks, diagnostics_stopped);
    let certificate = match teardown.certify() {
        Ok(certificate) => certificate,
        Err(error) => {
            report_boundary_error("xlAutoOpen rollback certification", &error);
            return incomplete(runtime);
        }
    };
    let rollback_attempt = match certificate.finish() {
        Ok(attempt) => attempt,
        Err((error, _certificate)) => {
            report_boundary_error("xlAutoOpen rollback completion", &error);
            return incomplete(runtime);
        }
    };
    if let Err(error) = runtime.release_empty_addin_lifecycle(lifecycle) {
        report_boundary_error(
            "xlAutoOpen lifecycle binding release",
            &lifecycle_access_error(error),
        );
        drop(rollback_attempt);
        return incomplete(runtime);
    }
    drop(rollback_attempt);
    OpenRollbackOutcome {
        status: OpenRollbackStatus::Finalized,
    }
}

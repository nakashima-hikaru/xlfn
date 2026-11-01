//! Open-attempt rollback transition and its terminal proof.
//!
//! This module owns the rollback-specific state and cleanup pipeline. It is
//! loaded into the runtime recovery domain; the lifecycle root retains only
//! canonical state and ownership types.

use crate::addin::Addin;
use crate::boundary::{report_boundary_error, report_cleanup_issue};
use crate::generation::RuntimeGeneration;
use crate::host_callback::HostCallbackSession;
use crate::registration::RegistrationHost;
use crate::runtime::{AddinLifecycleAccess, Runtime};

use crate::lifecycle::lifecycle_access_error;

pub(crate) enum OpenRollbackStatus {
    /// Every terminal certificate was issued and the lifecycle binding was
    /// released. Confirmed module events can be reused by a later open; an
    /// indeterminate event slot still prevents a blind registration retry.
    Finalized,
    /// A final removal owner took over between failed-open publication and
    /// rollback acquisition. That owner alone decides cleanup failure policy.
    ClosingOwnsCleanup,
    /// Rollback entered terminal quarantine. The boundary applies this hazard
    /// after the teardown transaction has returned its cleanup authority.
    Incomplete(crate::shutdown::UnloadHazard),
}

pub(crate) struct OpenRollbackOutcome {
    status: OpenRollbackStatus,
}

pub(crate) fn active_runtime_generation<A: Addin>(
    runtime: &Runtime<A, impl crate::runtime::UnloadPolicy<A>>,
) -> Option<RuntimeGeneration> {
    runtime.protocol_generation()
}

impl OpenRollbackOutcome {
    pub(crate) fn is_finalized(&self) -> bool {
        matches!(self.status, OpenRollbackStatus::Finalized)
    }

    pub(crate) fn cleanup_owned_elsewhere(&self) -> bool {
        matches!(self.status, OpenRollbackStatus::ClosingOwnsCleanup)
    }

    pub(crate) fn failure_hazard(&self) -> Option<crate::shutdown::UnloadHazard> {
        match self.status {
            OpenRollbackStatus::Incomplete(hazard) => Some(hazard),
            OpenRollbackStatus::Finalized | OpenRollbackStatus::ClosingOwnsCleanup => None,
        }
    }
}

fn incomplete<A: Addin>(
    runtime: &Runtime<A, impl crate::runtime::UnloadPolicy<A>>,
    hazard: crate::shutdown::UnloadHazard,
) -> OpenRollbackOutcome {
    // The transaction's affine cleanup authority is still held by stack
    // locals here. Publish terminal quarantine now; the boundary applies the
    // recorded hazard after those locals have returned their authority.
    runtime.lifecycle_orchestrator().quarantine();
    OpenRollbackOutcome {
        status: OpenRollbackStatus::Incomplete(hazard),
    }
}

pub(crate) fn rollback_open<A>(
    runtime: &Runtime<A, impl crate::runtime::UnloadPolicy<A>>,
    lifecycle: &AddinLifecycleAccess<'_, A>,
    callbacks: &mut HostCallbackSession,
    generation: Option<RuntimeGeneration>,
) -> OpenRollbackOutcome
where
    A: Addin,
{
    rollback_open_inner(runtime, lifecycle, callbacks, generation, None)
}

pub(in crate::runtime) fn rollback_open_request<'runtime, A: Addin>(
    runtime: &'runtime Runtime<A, impl crate::runtime::UnloadPolicy<A>>,
    lifecycle: &AddinLifecycleAccess<'_, A>,
    callbacks: &mut HostCallbackSession,
    request: super::capabilities::RollbackRequest<'runtime, A>,
) -> OpenRollbackOutcome {
    if !request.matches(runtime.shutdown_deps()) {
        return OpenRollbackOutcome {
            status: OpenRollbackStatus::Incomplete(
                crate::shutdown::UnloadHazard::CloseInvariantViolation,
            ),
        };
    }
    let generation = Some(request.generation());
    rollback_open_inner(runtime, lifecycle, callbacks, generation, Some(request))
}

fn rollback_open_inner<'runtime, A: Addin>(
    runtime: &'runtime Runtime<A, impl crate::runtime::UnloadPolicy<A>>,
    lifecycle: &AddinLifecycleAccess<'_, A>,
    callbacks: &mut HostCallbackSession,
    generation: Option<RuntimeGeneration>,
    request: Option<super::capabilities::RollbackRequest<'runtime, A>>,
) -> OpenRollbackOutcome {
    #[cfg(test)]
    let _diagnostic_test_guard = crate::diagnostics::DIAGNOSTIC_TEST_MUTEX.lock();
    let rollback_attempt = match runtime.acquire_open_rollback() {
        crate::runtime::orchestration::OpenRollbackAcquisition::Acquired(attempt) => attempt,
        crate::runtime::orchestration::OpenRollbackAcquisition::ClosingOwnsCleanup => {
            let finalized = runtime.phase() == crate::lifecycle::LifecyclePhase::Closed
                && runtime.lifecycle.access().removal_attempt().is_none()
                && runtime.host.generation_callbacks_detached()
                && !runtime.host.registration_state_unknown()
                && runtime.residency.callbacks_retained_safely()
                && crate::excel_rtd_protocol::logical_quiescence_certified();
            return OpenRollbackOutcome {
                status: if finalized {
                    OpenRollbackStatus::Finalized
                } else {
                    OpenRollbackStatus::ClosingOwnsCleanup
                },
            };
        }
        crate::runtime::orchestration::OpenRollbackAcquisition::Unavailable => {
            return incomplete(
                runtime,
                crate::shutdown::UnloadHazard::CloseInvariantViolation,
            );
        }
    };
    runtime.observer().begin_close();
    let shutdown_deps = runtime.shutdown_deps();

    let _lifecycle_present = match shutdown_deps.has_addin_lifecycle(lifecycle) {
        Ok(present) => present,
        Err(error) => {
            report_boundary_error("xlAutoOpen lifecycle slot", &lifecycle_access_error(error));
            return incomplete(
                runtime,
                crate::shutdown::UnloadHazard::CloseInvariantViolation,
            );
        }
    };
    // An opening transaction normally owns the lifecycle payload. It is only
    // safe to release the thread binding after that payload has been
    // explicitly taken and dropped below.
    let drain = match request {
        Some(request) => rollback_attempt.drain_opening(request),
        None => rollback_attempt.drain(),
    };
    let teardown = match drain {
        Ok(stage) => stage,
        Err(failure) => {
            let hazard = failure.hazard();
            report_boundary_error("xlAutoOpen execution drain", &failure.into_error());
            return incomplete(runtime, hazard);
        }
    };
    let teardown = match teardown.stop_producers(|issue| {
        report_cleanup_issue(issue);
    }) {
        Ok(stage) => stage,
        Err(failure) => {
            let hazard = failure.hazard();
            report_boundary_error("xlAutoOpen producer rollback", &failure.into_error());
            return incomplete(runtime, hazard);
        }
    };

    let outcome = {
        let host = RegistrationHost::new(callbacks);
        match shutdown_deps.host().cleanup_registrations(&host) {
            Ok(outcome) => outcome,
            Err(error) => {
                report_boundary_error("xlAutoOpen registration rollback", &error);
                return incomplete(
                    runtime,
                    crate::shutdown::UnloadHazard::HostCallbackStillRegistered,
                );
            }
        }
    };
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
    // Native calculation exports survive a failed open, including a partial
    // event-registration sequence. Their module slots prevent duplicate retry
    // and retain residency independently of this generation's rollback.
    let teardown = teardown.close_module_callbacks();

    let teardown = match teardown.detach_host() {
        Ok(teardown) => teardown,
        Err(failure) => {
            let hazard = failure.hazard();
            report_boundary_error("xlAutoOpen host detachment rollback", &failure.into_error());
            return incomplete(runtime, hazard);
        }
    };

    // Quiescence consumes the owned generation only after host detachment.
    let teardown = match teardown.quiesce_addin(runtime, lifecycle, generation) {
        Ok(teardown) => teardown,
        Err(failure) => {
            let hazard = failure.hazard();
            report_boundary_error("xlAutoOpen rollback quiesce", &failure.into_error());
            return incomplete(runtime, hazard);
        }
    };

    let teardown = match teardown.seal_services() {
        Ok(stage) => stage,
        Err(failure) => {
            let hazard = failure.hazard();
            let boundary = match &failure {
                super::shutdown::ServiceSealFailure::HandleTopics(_) => "xlAutoOpen RTD rollback",
                super::shutdown::ServiceSealFailure::Handles(_) => "xlAutoOpen handle rollback",
            };
            report_boundary_error(boundary, &failure.into_error());
            return incomplete(runtime, hazard);
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
            return incomplete(runtime, crate::shutdown::UnloadHazard::AddinCleanupFailed);
        }
    };
    for issue in cleanup_report.issues() {
        report_cleanup_issue(issue);
    }

    let teardown = match teardown.finish_services() {
        Ok(stage) => stage,
        Err(error) => {
            report_boundary_error("xlAutoOpen handle pin rollback", &error);
            return incomplete(
                runtime,
                crate::shutdown::UnloadHazard::HandleStoreNotQuiescent,
            );
        }
    };

    let teardown = match teardown.reclaim(report_cleanup_issue) {
        Ok(teardown) => teardown,
        Err(failure) => {
            let hazard = failure.hazard();
            let boundary = match &failure {
                super::shutdown::ReclaimFailure::Diagnostics(_) => "xlAutoOpen diagnostic rollback",
                super::shutdown::ReclaimFailure::Rtd(_) => "xlAutoOpen RTD quiescence rollback",
            };
            report_boundary_error(boundary, &failure.into_error());
            return incomplete(runtime, hazard);
        }
    };
    let certificate = match teardown.certify() {
        Ok(certificate) => certificate,
        Err(error) => {
            report_boundary_error("xlAutoOpen rollback certification", &error);
            return incomplete(
                runtime,
                crate::shutdown::UnloadHazard::CloseInvariantViolation,
            );
        }
    };
    let finalized = match certificate.finish() {
        Ok(finalized) => finalized,
        Err(failure) => {
            let error = failure.quarantine();
            report_boundary_error("xlAutoOpen rollback completion", &error);
            return incomplete(
                runtime,
                crate::shutdown::UnloadHazard::CloseInvariantViolation,
            );
        }
    };
    drop(finalized);
    OpenRollbackOutcome {
        status: OpenRollbackStatus::Finalized,
    }
}

//! Runtime recovery and unload-hazard policy.
//!
//! This module coordinates lifecycle quarantine with the owned runtime
//! resources. The lifecycle state machine reports transitions; it does not
//! decide how host failures are reported or how resources are retained after
//! an incomplete shutdown.

use crate::XllError;
use crate::addin::Addin;
use crate::boundary::{fail_stop_invariant, report_boundary_error};
use crate::generation::RuntimeGeneration;
use crate::runtime::Runtime;
use crate::runtime_transactions::{RemovalControl, RemovalSuccess};
use std::panic::{AssertUnwindSafe, catch_unwind};

#[cold]
pub(crate) fn handle_unload_hazard<A: Addin>(
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
            .fail_stop(_runtime, hazard.shutdown_failure());
        fail_stop_invariant(boundary, error);
    }

    report_boundary_error(boundary, error);
    RemovalControl::Quarantine {
        hazard,
        boundary,
        error: error.clone(),
    }
}

pub(crate) fn commit_removal_control<A: Addin>(
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

pub(crate) fn quarantine_runtime<A: Addin>(runtime: &Runtime<A>) {
    runtime.runtime_orchestrator().quarantine();
    #[cfg(any(test, feature = "refinement"))]
    runtime.refinement_hooks().quarantine(
        runtime,
        crate::shutdown_trace::ShutdownFailure::BoundaryPanic,
    );
    quarantine_runtime_resources(runtime);
}

pub(crate) fn quarantine_for_hazard<A: Addin>(
    runtime: &Runtime<A>,
    _hazard: crate::shutdown::UnloadHazard,
) {
    runtime.runtime_orchestrator().quarantine();
    #[cfg(any(test, feature = "refinement"))]
    runtime
        .refinement_hooks()
        .quarantine(runtime, _hazard.shutdown_failure());
    quarantine_runtime_resources(runtime);
}

pub(crate) fn quarantine_runtime_resources<A: Addin>(runtime: &Runtime<A>) {
    // Claiming the existing authority is itself an invariant boundary. Keep
    // it outside the best-effort cleanup catch so a missing authority cannot
    // be swallowed as an ordinary quarantine cleanup failure.
    let module_cleanup_authority = runtime.lifecycle_control().take_module_cleanup_authority();
    if module_cleanup_authority.is_none()
        && (runtime
            .lifecycle_control()
            .access()
            .module_epoch_id()
            .is_some()
            || crate::module_runtime::ingress().phase() != crate::ingress::PHASE_CLOSED)
    {
        fail_stop_invariant(
            "active module epoch lacks affine close authority",
            &XllError::Internal {
                diagnostic_id: crate::diagnostics::id::DiagnosticId::CLOSE_RUNTIME,
            },
        );
    }
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if let Some(module_cleanup_authority) = module_cleanup_authority {
            module_cleanup_authority.finish();
        }
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

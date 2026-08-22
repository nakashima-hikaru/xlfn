//! Excel lifecycle callback adapters.
//!
//! These functions are the only lifecycle entry points that translate host
//! callbacks into the runtime protocol. Transition implementation remains in
//! the parent module; this module owns the host-facing policy decisions.

use super::{open_addin, quarantine_runtime, remove_addin, report_boundary_error};
use crate::addin::Addin;
use crate::diagnostics::AddinId;
use crate::registration::RegistrationDescriptor;
use crate::runtime::Runtime;

/// Handles the generated `xlAutoOpen` boundary.
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
    if runtime.phase() == crate::lifecycle::LifecyclePhase::Quarantined {
        return 0;
    }
    let controlled_reload = runtime.phase() == crate::lifecycle::LifecyclePhase::Open;
    let removal_completed_before_open = runtime.phase() == crate::lifecycle::LifecyclePhase::Closed
        && runtime.host_intent() == super::HostLifecycleIntent::ExplicitRemovalComplete;
    if controlled_reload {
        let result = remove_addin::<A>(runtime);
        if result == 0 || runtime.phase() != crate::lifecycle::LifecyclePhase::Closed {
            return 0;
        }
        runtime.clear_host_intent();
    }
    let result = open_addin(runtime, addin_id, version, target, descriptors);
    if controlled_reload
        && result == 0
        && runtime.phase() != crate::lifecycle::LifecyclePhase::Quarantined
    {
        // A reload has already destroyed the previous generation. A failed
        // replacement must therefore not leave a closed runtime with the old
        // residency lease and no generation owner.
        quarantine_runtime(runtime);
    } else if result == 0
        && removal_completed_before_open
        && runtime.phase() == crate::lifecycle::LifecyclePhase::Closed
    {
        // The old generation was already removed successfully, but Excel
        // attempted a new open before delivering its close hint. Preserve
        // the release marker so that the later hint can release the lease.
        runtime.complete_explicit_removal();
    }
    result
}

/// Handles Excel's ambiguous close/deactivation hint.
pub fn host_auto_close<A>(runtime: &Runtime<A>) -> i32
where
    A: Addin,
{
    if runtime.phase() == crate::lifecycle::LifecyclePhase::Closed
        && runtime.host_intent() == super::HostLifecycleIntent::ExplicitRemovalComplete
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
pub fn host_auto_remove<A>(runtime: &Runtime<A>) -> i32
where
    A: Addin,
{
    if runtime.phase() == crate::lifecycle::LifecyclePhase::Quarantined {
        return 1;
    }
    runtime.request_explicit_removal();
    let result = remove_addin::<A>(runtime);
    if result == 1 && runtime.phase() == crate::lifecycle::LifecyclePhase::Closed {
        runtime.complete_explicit_removal();
    }
    1
}

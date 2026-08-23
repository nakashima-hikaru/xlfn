//! Shared teardown stages for rollback and terminal removal.
//!
//! The two boundary pipelines intentionally keep different failure policy and
//! proof certificates. They do, however, share one ordering-sensitive stage:
//! close export admission, drain active calls, and wait for return producers.
//! Keeping that stage here prevents either pipeline from silently changing the
//! unload-safety ordering.

use crate::addin::Addin;
use crate::runtime::Runtime;

/// The concrete stage produced by the common execution-drain transition.
///
/// The exports certificate is deliberately kept behind this stage until the
/// terminal proof is assembled. Both rollback and final removal therefore
/// carry the same execution-drained witness through their remaining cleanup
/// stages instead of immediately unwrapping it at the call site.
pub(super) struct ExecutionDrained {
    exports: crate::ingress::ExportsDrained,
}

impl ExecutionDrained {
    pub(super) fn begin<A: Addin>(runtime: &Runtime<A>, _record_ghost: bool) -> Self {
        let exports = crate::module_runtime::global().seal_and_drain();

        #[cfg(any(test, feature = "unstable"))]
        if _record_ghost {
            runtime.record_ghost_event_linearized(
                crate::shutdown_refinement::GhostEvent::CallsDrained,
            );
        }

        runtime.wait_for_returns();

        #[cfg(any(test, feature = "unstable"))]
        if _record_ghost {
            runtime.record_ghost_event(crate::shutdown_refinement::GhostEvent::ReturnsDrained);
        }

        Self { exports }
    }

    pub(super) fn into_exports(self) -> crate::ingress::ExportsDrained {
        self.exports
    }
}

pub(super) fn drain_execution<A: Addin>(
    runtime: &Runtime<A>,
    record_ghost: bool,
) -> ExecutionDrained {
    ExecutionDrained::begin(runtime, record_ghost)
}

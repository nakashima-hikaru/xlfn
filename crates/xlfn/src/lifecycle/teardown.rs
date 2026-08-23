//! Shared teardown stages for rollback and terminal removal.
//!
//! The two boundary pipelines intentionally keep different failure policy and
//! proof certificates. They do, however, share one ordering-sensitive stage:
//! close export admission, drain active calls, and wait for return producers.
//! Keeping that stage here prevents either pipeline from silently changing the
//! unload-safety ordering.

use crate::addin::Addin;
use crate::runtime::Runtime;

/// The concrete witness produced by the common execution-drain stage.
pub(super) struct ExecutionDrain {
    exports: crate::ingress::ExportsDrained,
}

impl ExecutionDrain {
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
) -> crate::ingress::ExportsDrained {
    ExecutionDrain::begin(runtime, record_ghost).into_exports()
}

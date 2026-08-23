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

/// The producer stage owns the execution-drain witness while async work and
/// subscription producers are stopped. Keeping these certificates together
/// prevents one pipeline from accidentally assembling a terminal proof with
/// only part of the producer shutdown sequence completed.
pub(super) struct ProducersStopped {
    execution: ExecutionDrained,
    async_stopped: crate::shutdown::AsyncStopped,
    subscriptions_stopped: crate::shutdown::SubscriptionsStopped,
}

/// Owns every resource certificate after producers have stopped. The only
/// operation that can expose the aggregate proof is `into_proof`, so callers
/// cannot accidentally certify a partially assembled terminal transition.
pub(super) struct ResourcesReclaimed {
    producers: ProducersStopped,
    rtd: crate::rtd::RtdQuiescent,
    host_callbacks: crate::shutdown::HostCallbacksDetached,
    handle_store: crate::shutdown::HandleStoreQuiescent,
    diagnostics: crate::diagnostics::DiagnosticsStopped,
    addin: crate::shutdown::AddinQuiesced,
    generation: crate::shutdown::GenerationReclaimed,
}

impl ExecutionDrained {
    pub(super) fn begin<A: Addin>(runtime: &Runtime<A>, _record_ghost: bool) -> Self {
        let exports = crate::module_runtime::global().seal_and_drain();

        #[cfg(any(test, feature = "refinement"))]
        if _record_ghost {
            runtime.refinement_hooks().calls_drained(runtime);
        }

        runtime.wait_for_returns();

        #[cfg(any(test, feature = "refinement"))]
        if _record_ghost {
            runtime.refinement_hooks().returns_drained(runtime);
        }

        Self { exports }
    }

    pub(super) fn stop_producers<A: Addin>(
        self,
        runtime: &Runtime<A>,
        report_issue: impl FnMut(&crate::shutdown::CleanupIssue),
    ) -> crate::XllResult<ProducersStopped> {
        #[cfg(feature = "async")]
        let mut report_issue = report_issue;
        #[cfg(not(feature = "async"))]
        let _ = report_issue;

        #[cfg(feature = "async")]
        let async_stopped = {
            runtime.cancel_async();
            let outcome = runtime.close_async();
            for issue in &outcome.issues {
                report_issue(issue);
            }
            outcome.certificate
        };
        #[cfg(not(feature = "async"))]
        let async_stopped = crate::shutdown::AsyncStopped::new();

        let subscriptions_stopped = runtime.close_subscriptions()?;

        Ok(ProducersStopped {
            execution: self,
            async_stopped,
            subscriptions_stopped,
        })
    }
}

impl ProducersStopped {
    fn into_parts(
        self,
    ) -> (
        crate::ingress::ExportsDrained,
        crate::shutdown::AsyncStopped,
        crate::shutdown::SubscriptionsStopped,
    ) {
        (
            self.execution.exports,
            self.async_stopped,
            self.subscriptions_stopped,
        )
    }
}

impl ResourcesReclaimed {
    pub(super) fn new(
        producers: ProducersStopped,
        rtd: crate::rtd::RtdQuiescent,
        host_callbacks: crate::shutdown::HostCallbacksDetached,
        handle_store: crate::shutdown::HandleStoreQuiescent,
        diagnostics: crate::diagnostics::DiagnosticsStopped,
        addin: crate::shutdown::AddinQuiesced,
        generation: crate::shutdown::GenerationReclaimed,
    ) -> Self {
        Self {
            producers,
            rtd,
            host_callbacks,
            handle_store,
            diagnostics,
            addin,
            generation,
        }
    }

    pub(super) fn into_proof(self) -> crate::runtime::QuiescenceProof {
        let (exports, async_stopped, subscriptions_stopped) = self.producers.into_parts();
        crate::runtime::QuiescenceProof {
            exports,
            rtd: self.rtd,
            host_callbacks: self.host_callbacks,
            async_stopped,
            subscriptions_stopped,
            handle_store_quiescent: self.handle_store,
            diagnostics_stopped: self.diagnostics,
            addin_quiesced: self.addin,
            generation_reclaimed: self.generation,
        }
    }
}

pub(super) fn drain_execution<A: Addin>(
    runtime: &Runtime<A>,
    record_ghost: bool,
) -> ExecutionDrained {
    ExecutionDrained::begin(runtime, record_ghost)
}

//! Shared teardown stages for rollback and terminal removal.
//!
//! The two boundary pipelines intentionally keep different failure policy and
//! proof certificates. They do, however, share one ordering-sensitive stage:
//! close export admission, drain active calls, and wait for return producers.
//! Keeping that stage here prevents either pipeline from silently changing the
//! unload-safety ordering.

use crate::addin::Addin;
use crate::runtime::{RemovalOwner, Runtime};
use std::marker::PhantomData;
use std::panic::{AssertUnwindSafe, catch_unwind};

/// Add-in state that has passed the quiesce boundary and is owned by the
/// teardown transaction until best-effort cleanup completes.
pub(super) struct QuiescedAddin<'runtime, A: Addin> {
    runtime: &'runtime Runtime<A>,
    generation: Option<crate::generation::RuntimeGeneration>,
    shared_state: Option<A::SharedState>,
}

pub(super) struct CleanedAddin {
    pub(super) addin_quiesced: crate::shutdown::AddinQuiesced,
    pub(super) generation_reclaimed: crate::shutdown::GenerationReclaimed,
}

impl CleanedAddin {
    fn issued() -> Self {
        Self {
            addin_quiesced: crate::shutdown::AddinQuiesced::issue(),
            generation_reclaimed: crate::shutdown::GenerationReclaimed::issue(),
        }
    }
}

impl<'runtime, A: Addin> QuiescedAddin<'runtime, A> {
    pub(super) fn empty(
        runtime: &'runtime Runtime<A>,
        generation: Option<crate::generation::RuntimeGeneration>,
    ) -> Self {
        Self {
            runtime,
            generation,
            shared_state: None,
        }
    }

    pub(super) fn shared(
        runtime: &'runtime Runtime<A>,
        generation: Option<crate::generation::RuntimeGeneration>,
        shared_state: A::SharedState,
    ) -> Self {
        Self {
            runtime,
            generation,
            shared_state: Some(shared_state),
        }
    }

    pub(super) fn cleanup(
        mut self,
        lifecycle: &crate::runtime::AddinLifecycleAccess<'_, A>,
        report: &mut crate::shutdown::CloseReport,
    ) -> Result<CleanedAddin, crate::XllError> {
        let Some(shared_state) = self.shared_state.take() else {
            return Ok(CleanedAddin::issued());
        };

        let cleanup = catch_unwind(AssertUnwindSafe(|| {
            self.runtime
                .with_addin_lifecycle(lifecycle, |lifecycle_state| {
                    let mut reporter = crate::shutdown::CleanupReporter::new(report);
                    A::cleanup(lifecycle_state, &mut reporter);
                })
                .map_err(super::lifecycle_access_error)
        }));
        if cleanup.is_err() || cleanup.as_ref().is_ok_and(|result| result.is_err()) {
            report.push(
                "Addin::cleanup",
                crate::shutdown::CleanupIssueKind::DisposalPanicked,
                crate::XllError::Panic,
            );
            self.quarantine_shared_state(
                shared_state,
                crate::runtime_components::QuarantineReason::AddinCleanupPanicked,
            );
            return Err(crate::XllError::Panic);
        }

        let lifecycle_dropped = match self.runtime.take_addin_lifecycle(lifecycle) {
            Ok(lifecycle_state) => {
                if catch_unwind(AssertUnwindSafe(|| drop(lifecycle_state))).is_err() {
                    report.push(
                        "Addin::LifecycleState::drop",
                        crate::shutdown::CleanupIssueKind::DisposalPanicked,
                        crate::XllError::Panic,
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
                    super::lifecycle_access_error(error),
                );
                false
            }
        };
        if !lifecycle_dropped {
            self.quarantine_shared_state(
                shared_state,
                crate::runtime_components::QuarantineReason::TeardownIncomplete,
            );
            return Err(crate::XllError::Internal {
                diagnostic_id: crate::diagnostics::id::DiagnosticId::LIFECYCLE_SLOT,
            });
        }
        let shared_state_dropped = catch_unwind(AssertUnwindSafe(|| drop(shared_state))).is_ok();
        if !shared_state_dropped {
            report.push(
                "Addin::SharedState::drop",
                crate::shutdown::CleanupIssueKind::DisposalPanicked,
                crate::XllError::Panic,
            );
        }
        if !lifecycle_dropped || !shared_state_dropped {
            return Err(crate::XllError::Internal {
                diagnostic_id: crate::diagnostics::id::DiagnosticId::LIFECYCLE_SLOT,
            });
        }
        Ok(CleanedAddin::issued())
    }

    fn quarantine_shared_state(
        &self,
        shared_state: A::SharedState,
        reason: crate::runtime_components::QuarantineReason,
    ) {
        self.runtime.lifecycle_runtime().quarantine_shared_state(
            self.generation,
            shared_state,
            reason,
        );
    }
}

impl<A: Addin> Drop for QuiescedAddin<'_, A> {
    fn drop(&mut self) {
        if let Some(shared_state) = self.shared_state.take() {
            self.runtime.lifecycle_runtime().quarantine_shared_state(
                self.generation,
                shared_state,
                crate::runtime_components::QuarantineReason::TeardownIncomplete,
            );
        }
    }
}

/// The concrete stage produced by the common execution-drain transition.
///
/// The exports certificate is deliberately kept behind this stage until the
/// terminal proof is assembled. Both rollback and final removal therefore
/// carry the same execution-drained witness through their remaining cleanup
/// stages instead of immediately unwrapping it at the call site.
pub(super) struct ExecutionDrained {
    exports: crate::ingress::ExportsDrained,
    returns: crate::shutdown::ReturnsQuiescent,
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

/// Owns the generation service seal while the remaining shutdown stages run.
///
/// The subscription certificate is consumed by `Runtime::seal_generation_services`
/// and cannot be separated from the corresponding sealed handle service. This
/// is the close-side counterpart of `ArmedServices` on the open path.
pub(super) struct ServicesSealed<'runtime, A: Addin> {
    execution: ExecutionDrained,
    async_stopped: crate::shutdown::AsyncStopped,
    sealed: crate::runtime_components::SealedGenerationServices,
    addin: QuiescedAddin<'runtime, A>,
}

pub(super) struct ServicesCleaned {
    execution: ExecutionDrained,
    async_stopped: crate::shutdown::AsyncStopped,
    sealed: crate::runtime_components::SealedGenerationServices,
    addin: CleanedAddin,
}

/// The generation services have both been sealed and fully finished.
///
/// At this point the producer certificates and handle-store certificate are
/// one value, so a caller cannot accidentally assemble a terminal proof from
/// a handle certificate belonging to a different producer stage.
pub(super) struct ServicesQuiescent {
    exports: crate::ingress::ExportsDrained,
    returns: crate::shutdown::ReturnsQuiescent,
    async_stopped: crate::shutdown::AsyncStopped,
    subscriptions_stopped: crate::shutdown::SubscriptionsStopped,
    handle_store: crate::shutdown::HandleStoreQuiescent,
    addin: crate::shutdown::AddinQuiesced,
    generation: crate::shutdown::GenerationReclaimed,
}

/// Owns every resource certificate after producers have stopped. The only
/// operation that can expose the aggregate proof is `into_proof`, so callers
/// cannot accidentally certify a partially assembled terminal transition.
pub(super) struct ResourcesReclaimed {
    services: ServicesQuiescent,
    rtd: crate::rtd::RtdQuiescent,
    host_callbacks: crate::shutdown::HostCallbacksDetached,
    diagnostics: crate::diagnostics::DiagnosticsStopped,
}

impl ExecutionDrained {
    pub(super) fn begin<A: Addin>(
        runtime: &Runtime<A>,
        _record_ghost: bool,
    ) -> crate::XllResult<Self> {
        let exports = crate::module_runtime::global().seal_and_drain();

        #[cfg(any(test, feature = "refinement"))]
        if _record_ghost {
            runtime.refinement_hooks().calls_drained(runtime);
        }

        let returns = runtime.wait_for_return_quiescence()?;

        #[cfg(any(test, feature = "refinement"))]
        if _record_ghost {
            runtime.refinement_hooks().returns_drained(runtime);
        }

        Ok(Self { exports, returns })
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
        let async_stopped = crate::shutdown::AsyncStopped::issue();

        let subscriptions_stopped = runtime.close_subscriptions()?;

        Ok(ProducersStopped {
            execution: self,
            async_stopped,
            subscriptions_stopped,
        })
    }
}

impl ProducersStopped {
    pub(super) fn seal_services<'runtime, A: Addin>(
        self,
        runtime: &'runtime Runtime<A>,
        addin: QuiescedAddin<'runtime, A>,
    ) -> crate::XllResult<ServicesSealed<'runtime, A>> {
        let Self {
            execution,
            async_stopped,
            subscriptions_stopped,
        } = self;
        let sealed = runtime.seal_generation_services(subscriptions_stopped)?;
        Ok(ServicesSealed {
            execution,
            async_stopped,
            sealed,
            addin,
        })
    }
}

impl<'runtime, A: Addin> ServicesSealed<'runtime, A> {
    pub(super) fn cleanup(
        self,
        lifecycle: &crate::runtime::AddinLifecycleAccess<'_, A>,
        report: &mut crate::shutdown::CloseReport,
    ) -> crate::XllResult<ServicesCleaned> {
        let Self {
            execution,
            async_stopped,
            sealed,
            addin,
        } = self;
        let addin = addin.cleanup(lifecycle, report)?;
        Ok(ServicesCleaned {
            execution,
            async_stopped,
            sealed,
            addin,
        })
    }
}

impl ServicesCleaned {
    pub(super) fn finish(self) -> crate::XllResult<ServicesQuiescent> {
        let Self {
            execution,
            async_stopped,
            sealed,
            addin,
        } = self;
        let (handle_store, subscriptions_stopped) = sealed.finish()?;
        Ok(ServicesQuiescent {
            exports: execution.exports,
            returns: execution.returns,
            async_stopped,
            subscriptions_stopped,
            handle_store,
            addin: addin.addin_quiesced,
            generation: addin.generation_reclaimed,
        })
    }
}

impl ServicesQuiescent {
    fn into_parts(
        self,
    ) -> (
        crate::ingress::ExportsDrained,
        crate::shutdown::ReturnsQuiescent,
        crate::shutdown::AsyncStopped,
        crate::shutdown::SubscriptionsStopped,
        crate::shutdown::HandleStoreQuiescent,
        crate::shutdown::AddinQuiesced,
        crate::shutdown::GenerationReclaimed,
    ) {
        (
            self.exports,
            self.returns,
            self.async_stopped,
            self.subscriptions_stopped,
            self.handle_store,
            self.addin,
            self.generation,
        )
    }
}

impl ResourcesReclaimed {
    pub(super) fn new(
        services: ServicesQuiescent,
        rtd: crate::rtd::RtdQuiescent,
        host_callbacks: crate::shutdown::HostCallbacksDetached,
        diagnostics: crate::diagnostics::DiagnosticsStopped,
    ) -> Self {
        Self {
            services,
            rtd,
            host_callbacks,
            diagnostics,
        }
    }

    pub(super) fn into_proof(self) -> crate::runtime::QuiescenceProof {
        let (
            exports,
            returns,
            async_stopped,
            subscriptions_stopped,
            handle_store,
            addin,
            generation,
        ) = self.services.into_parts();
        crate::runtime::QuiescenceProof {
            exports,
            returns,
            rtd: self.rtd,
            host_callbacks: self.host_callbacks,
            async_stopped,
            subscriptions_stopped,
            handle_store_quiescent: handle_store,
            diagnostics_stopped: self.diagnostics,
            addin_quiesced: addin,
            generation_reclaimed: generation,
        }
    }
}

/// A linear teardown transaction parameterized by its terminal policy and
/// current stage. Dropping an incomplete transaction quarantines the runtime;
/// successful transitions consume the old stage and return the next one.
pub(super) struct TeardownTxn<'runtime, A: Addin, K, S> {
    owner: TeardownOwner<'runtime, A>,
    stage: S,
    _kind: PhantomData<K>,
}

struct TeardownOwner<'runtime, A: Addin> {
    owner: Option<RemovalOwner<'runtime, A>>,
}

impl<'runtime, A: Addin> TeardownOwner<'runtime, A> {
    fn new(owner: RemovalOwner<'runtime, A>) -> Self {
        Self { owner: Some(owner) }
    }

    fn take(&mut self) -> RemovalOwner<'runtime, A> {
        self.owner
            .take()
            .expect("teardown transaction owns one removal owner")
    }

    fn runtime(&self) -> &'runtime Runtime<A> {
        self.owner
            .as_ref()
            .expect("teardown transaction owns one removal owner")
            .runtime()
    }
}

impl<A: Addin> Drop for TeardownOwner<'_, A> {
    fn drop(&mut self) {
        if let Some(owner) = self.owner.as_ref() {
            owner.runtime().lifecycle_runtime().quarantine();
        }
    }
}

impl<'runtime, A: Addin, K, S> TeardownTxn<'runtime, A, K, S> {
    pub(super) fn new(owner: RemovalOwner<'runtime, A>, stage: S) -> Self {
        Self {
            owner: TeardownOwner::new(owner),
            stage,
            _kind: PhantomData,
        }
    }

    fn from_parts(owner: TeardownOwner<'runtime, A>, stage: S) -> Self {
        Self {
            owner,
            stage,
            _kind: PhantomData,
        }
    }

    fn split(self) -> (TeardownOwner<'runtime, A>, S) {
        let Self { owner, stage, .. } = self;
        (owner, stage)
    }
}

impl<'runtime, A: Addin, K> TeardownTxn<'runtime, A, K, ExecutionDrained> {
    pub(super) fn stop_producers(
        self,
        report_issue: impl FnMut(&crate::shutdown::CleanupIssue),
    ) -> crate::XllResult<TeardownTxn<'runtime, A, K, ProducersStopped>> {
        let (owner, stage) = self.split();
        let runtime = owner.runtime();
        match stage.stop_producers(runtime, report_issue) {
            Ok(stage) => Ok(TeardownTxn::from_parts(owner, stage)),
            Err(error) => {
                drop(owner);
                Err(error)
            }
        }
    }
}

impl<'runtime, A: Addin, K> TeardownTxn<'runtime, A, K, ProducersStopped> {
    pub(super) fn seal_services(
        self,
        addin: QuiescedAddin<'runtime, A>,
    ) -> crate::XllResult<TeardownTxn<'runtime, A, K, ServicesSealed<'runtime, A>>> {
        let (owner, stage) = self.split();
        let runtime = owner.runtime();
        match stage.seal_services(runtime, addin) {
            Ok(stage) => Ok(TeardownTxn::from_parts(owner, stage)),
            Err(error) => {
                drop(owner);
                Err(error)
            }
        }
    }
}

impl<'runtime, A: Addin, K> TeardownTxn<'runtime, A, K, ServicesSealed<'runtime, A>> {
    pub(super) fn cleanup_addin(
        self,
        lifecycle: &crate::runtime::AddinLifecycleAccess<'_, A>,
        report: &mut crate::shutdown::CloseReport,
    ) -> crate::XllResult<TeardownTxn<'runtime, A, K, ServicesCleaned>> {
        let (owner, stage) = self.split();
        match stage.cleanup(lifecycle, report) {
            Ok(stage) => Ok(TeardownTxn::from_parts(owner, stage)),
            Err(error) => {
                drop(owner);
                Err(error)
            }
        }
    }
}

impl<'runtime, A: Addin, K> TeardownTxn<'runtime, A, K, ServicesCleaned> {
    pub(super) fn finish_services(
        self,
    ) -> crate::XllResult<TeardownTxn<'runtime, A, K, ServicesQuiescent>> {
        let (owner, stage) = self.split();
        match stage.finish() {
            Ok(stage) => Ok(TeardownTxn::from_parts(owner, stage)),
            Err(error) => {
                drop(owner);
                Err(error)
            }
        }
    }
}

impl<'runtime, A: Addin, K> TeardownTxn<'runtime, A, K, ServicesQuiescent> {
    pub(super) fn reclaim(
        self,
        rtd: crate::rtd::RtdQuiescent,
        host_callbacks: crate::shutdown::HostCallbacksDetached,
        diagnostics: crate::diagnostics::DiagnosticsStopped,
    ) -> TeardownTxn<'runtime, A, K, ResourcesReclaimed> {
        let (owner, services) = self.split();
        let stage = ResourcesReclaimed::new(services, rtd, host_callbacks, diagnostics);
        TeardownTxn::from_parts(owner, stage)
    }
}

impl<'runtime, A: Addin, K> TeardownTxn<'runtime, A, K, ResourcesReclaimed> {
    pub(super) fn certify(
        mut self,
    ) -> crate::XllResult<<K as crate::runtime::TerminalCertificateKind>::Certificate<'runtime, A>>
    where
        K: crate::runtime::TerminalCertificateKind,
    {
        let proof = self.stage.into_proof();
        let owner = self.owner.take();
        match owner.certify::<K>(proof) {
            Ok(certificate) => Ok(certificate),
            Err((error, owner)) => {
                drop(TeardownOwner::new(owner));
                Err(error)
            }
        }
    }
}

pub(super) fn drain_execution<A: Addin>(
    runtime: &Runtime<A>,
    record_ghost: bool,
) -> crate::XllResult<ExecutionDrained> {
    ExecutionDrained::begin(runtime, record_ghost)
}

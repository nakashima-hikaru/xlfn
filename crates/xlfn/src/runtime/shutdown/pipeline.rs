//! Shared teardown stages for rollback and terminal removal.
//!
//! The two boundary pipelines intentionally keep different failure policy and
//! proof certificates. They do, however, share one ordering-sensitive stage:
//! close export admission, drain active calls, and wait for return producers.
//! Keeping that stage in the runtime shutdown domain prevents either pipeline
//! from silently changing the unload-safety ordering.

use crate::addin::Addin;
use crate::lifecycle::{QuiescenceProof, RemovalOwner, TerminalCertificateKind};
use crate::runtime::Runtime;
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
                .map_err(crate::lifecycle::lifecycle_access_error)
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
                    crate::lifecycle::lifecycle_access_error(error),
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
        self.runtime.runtime_orchestrator().quarantine_shared_state(
            self.generation,
            shared_state,
            reason,
        );
    }
}

impl<A: Addin> Drop for QuiescedAddin<'_, A> {
    fn drop(&mut self) {
        if let Some(shared_state) = self.shared_state.take() {
            self.runtime.runtime_orchestrator().quarantine_shared_state(
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
    module: crate::module_runtime::ModuleExportsDrained,
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
    module: crate::module_runtime::ModuleExportsDrained,
    returns: crate::shutdown::ReturnsQuiescent,
    async_stopped: crate::shutdown::AsyncStopped,
    subscriptions_stopped: crate::shutdown::SubscriptionsStopped,
    handles: crate::shutdown::HandlesQuiescent,
    addin: crate::shutdown::AddinQuiesced,
    generation: crate::shutdown::GenerationReclaimed,
}

/// Owns every resource certificate after producers have stopped. The only
/// operation that can expose the aggregate proof is `into_proof`, so callers
/// cannot accidentally certify a partially assembled terminal transition.
pub(super) struct ResourcesReclaimed {
    services: ServicesQuiescent,
    rtd: crate::excel_rtd::RtdQuiescent,
    host_callbacks: crate::shutdown::HostCallbacksDetached,
    diagnostics: crate::diagnostics::DiagnosticsStopped,
}

pub(super) trait ModuleCloseStage {
    fn close_module_callbacks(&self);
}

impl ModuleCloseStage for ExecutionDrained {
    fn close_module_callbacks(&self) {
        self.module.close_callbacks();
    }
}

impl ModuleCloseStage for ProducersStopped {
    fn close_module_callbacks(&self) {
        self.execution.close_module_callbacks();
    }
}

impl<A: Addin> ModuleCloseStage for ServicesSealed<'_, A> {
    fn close_module_callbacks(&self) {
        self.execution.close_module_callbacks();
    }
}

impl ModuleCloseStage for ServicesCleaned {
    fn close_module_callbacks(&self) {
        self.execution.close_module_callbacks();
    }
}

impl ModuleCloseStage for ServicesQuiescent {
    fn close_module_callbacks(&self) {
        self.module.close_callbacks();
    }
}

impl ModuleCloseStage for ResourcesReclaimed {
    fn close_module_callbacks(&self) {
        self.services.close_module_callbacks();
    }
}

impl ExecutionDrained {
    pub(super) fn begin<A: Addin>(
        runtime: &Runtime<A>,
        module: crate::module_runtime::ModuleClosing,
        _record_trace: bool,
    ) -> Result<Self, (crate::XllError, crate::module_runtime::ModuleExportsDrained)> {
        let module = module.seal_and_drain();

        #[cfg(any(test, feature = "refinement"))]
        if _record_trace {
            runtime.refinement_hooks().calls_drained(runtime);
        }

        let returns = match runtime.wait_for_return_quiescence() {
            Ok(returns) => returns,
            Err(error) => return Err((error, module)),
        };

        #[cfg(any(test, feature = "refinement"))]
        if _record_trace {
            runtime.refinement_hooks().returns_drained(runtime);
        }

        Ok(Self { module, returns })
    }

    pub(super) fn close_module_callbacks(&self) {
        self.module.close_callbacks();
    }

    pub(super) fn stop_producers<A: Addin>(
        self,
        runtime: &Runtime<A>,
        report_issue: impl FnMut(&crate::shutdown::CleanupIssue),
    ) -> Result<ProducersStopped, (crate::XllError, crate::module_runtime::ModuleExportsDrained)>
    {
        #[cfg(feature = "async")]
        let mut report_issue = report_issue;
        #[cfg(not(feature = "async"))]
        let _ = report_issue;

        #[cfg(all(feature = "async", any(test, feature = "refinement")))]
        let async_was_running = runtime.async_manager().is_running();
        #[cfg(feature = "async")]
        let async_stopped = {
            runtime.cancel_async();
            let outcome = runtime.close_async();
            for issue in &outcome.issues {
                report_issue(issue);
            }
            outcome.certificate
        };
        #[cfg(all(feature = "async", any(test, feature = "refinement")))]
        if async_was_running {
            runtime.refinement_hooks().async_stopped(runtime);
        }
        #[cfg(not(feature = "async"))]
        let async_stopped = crate::shutdown::AsyncStopped::issue();

        let subscriptions_stopped = match runtime.close_subscriptions() {
            Ok(subscriptions_stopped) => subscriptions_stopped,
            Err(error) => return Err((error, self.module)),
        };

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
    ) -> Result<
        ServicesSealed<'runtime, A>,
        (crate::XllError, crate::module_runtime::ModuleExportsDrained),
    > {
        let Self {
            execution,
            async_stopped,
            subscriptions_stopped,
        } = self;
        match runtime.seal_generation_services(subscriptions_stopped) {
            Ok(sealed) => Ok(ServicesSealed {
                execution,
                async_stopped,
                sealed,
                addin,
            }),
            Err(error) => Err((error, execution.module)),
        }
    }
}

impl<'runtime, A: Addin> ServicesSealed<'runtime, A> {
    pub(super) fn cleanup(
        self,
        lifecycle: &crate::runtime::AddinLifecycleAccess<'_, A>,
        report: &mut crate::shutdown::CloseReport,
    ) -> Result<ServicesCleaned, (crate::XllError, crate::module_runtime::ModuleExportsDrained)>
    {
        let Self {
            execution,
            async_stopped,
            sealed,
            addin,
        } = self;
        match addin.cleanup(lifecycle, report) {
            Ok(addin) => Ok(ServicesCleaned {
                execution,
                async_stopped,
                sealed,
                addin,
            }),
            Err(error) => Err((error, execution.module)),
        }
    }
}

impl ServicesCleaned {
    pub(super) fn finish(
        self,
    ) -> Result<ServicesQuiescent, (crate::XllError, crate::module_runtime::ModuleExportsDrained)>
    {
        let Self {
            execution,
            async_stopped,
            sealed,
            addin,
        } = self;
        match sealed.finish() {
            Ok((handles, subscriptions_stopped)) => Ok(ServicesQuiescent {
                module: execution.module,
                returns: execution.returns,
                async_stopped,
                subscriptions_stopped,
                handles,
                addin: addin.addin_quiesced,
                generation: addin.generation_reclaimed,
            }),
            Err(error) => Err((error, execution.module)),
        }
    }
}

impl ServicesQuiescent {
    fn into_parts(
        self,
    ) -> (
        crate::module_runtime::ModuleExportsDrained,
        crate::shutdown::ReturnsQuiescent,
        crate::shutdown::AsyncStopped,
        crate::shutdown::SubscriptionsStopped,
        crate::shutdown::HandlesQuiescent,
        crate::shutdown::AddinQuiesced,
        crate::shutdown::GenerationReclaimed,
    ) {
        (
            self.module,
            self.returns,
            self.async_stopped,
            self.subscriptions_stopped,
            self.handles,
            self.addin,
            self.generation,
        )
    }
}

impl ResourcesReclaimed {
    pub(super) fn new(
        services: ServicesQuiescent,
        rtd: crate::excel_rtd::RtdQuiescent,
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

    pub(super) fn into_proof(self) -> QuiescenceProof {
        let (module, returns, async_stopped, subscriptions_stopped, handles, addin, generation) =
            self.services.into_parts();
        let (module_quiescent, exports) = module.certify();
        QuiescenceProof {
            exports,
            returns,
            rtd: self.rtd,
            host_callbacks: self.host_callbacks,
            async_stopped,
            subscriptions_stopped,
            handles_quiescent: handles,
            diagnostics_stopped: self.diagnostics,
            addin_quiesced: addin,
            generation_reclaimed: generation,
            module_quiescent,
        }
    }
}

/// Retains the already-progressed module authority when an incomplete
/// teardown is abandoned. The recovery path is deliberately one-way:
/// `ModuleExportsDrained` is stored as a drained cleanup authority and is
/// never converted back into `ModuleClosing`.
pub(super) trait ModuleAuthorityRecovery<A: Addin> {
    fn recover_module_authority(self, runtime: &Runtime<A>);
}

fn retain_drained_module<A: Addin>(
    runtime: &Runtime<A>,
    module: crate::module_runtime::ModuleExportsDrained,
) {
    runtime
        .lifecycle_control()
        .install_module_cleanup_authority(crate::module_runtime::ModuleCleanupAuthority::Drained(
            module,
        ));
}

impl<A: Addin> ModuleAuthorityRecovery<A> for ExecutionDrained {
    fn recover_module_authority(self, runtime: &Runtime<A>) {
        retain_drained_module(runtime, self.module);
    }
}

impl<A: Addin> ModuleAuthorityRecovery<A> for ProducersStopped {
    fn recover_module_authority(self, runtime: &Runtime<A>) {
        self.execution.recover_module_authority(runtime);
    }
}

impl<A: Addin> ModuleAuthorityRecovery<A> for ServicesSealed<'_, A> {
    fn recover_module_authority(self, runtime: &Runtime<A>) {
        self.execution.recover_module_authority(runtime);
    }
}

impl<A: Addin> ModuleAuthorityRecovery<A> for ServicesCleaned {
    fn recover_module_authority(self, runtime: &Runtime<A>) {
        self.execution.recover_module_authority(runtime);
    }
}

impl<A: Addin> ModuleAuthorityRecovery<A> for ServicesQuiescent {
    fn recover_module_authority(self, runtime: &Runtime<A>) {
        retain_drained_module(runtime, self.module);
    }
}

impl<A: Addin> ModuleAuthorityRecovery<A> for ResourcesReclaimed {
    fn recover_module_authority(self, runtime: &Runtime<A>) {
        self.services.recover_module_authority(runtime);
    }
}

fn recover_stage<A: Addin, S: ModuleAuthorityRecovery<A>>(stage: S, runtime: &Runtime<A>) {
    stage.recover_module_authority(runtime);
}

/// A linear teardown transaction parameterized by its terminal policy and
/// current stage. Dropping an incomplete transaction quarantines the runtime;
/// successful transitions consume the old stage and return the next one.
pub(super) struct TeardownTxn<'runtime, A: Addin, K, S> {
    owner: TeardownOwner<'runtime, A>,
    stage: Option<S>,
    recover: fn(S, &Runtime<A>),
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

    fn empty() -> Self {
        Self { owner: None }
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
            owner.runtime().runtime_orchestrator().quarantine();
        }
    }
}

impl<'runtime, A: Addin, K, S> TeardownTxn<'runtime, A, K, S> {
    pub(super) fn new(owner: RemovalOwner<'runtime, A>, stage: S) -> Self
    where
        S: ModuleAuthorityRecovery<A>,
    {
        Self {
            owner: TeardownOwner::new(owner),
            stage: Some(stage),
            recover: recover_stage::<A, S>,
            _kind: PhantomData,
        }
    }

    fn from_parts(owner: TeardownOwner<'runtime, A>, stage: S) -> Self
    where
        S: ModuleAuthorityRecovery<A>,
    {
        Self {
            owner,
            stage: Some(stage),
            recover: recover_stage::<A, S>,
            _kind: PhantomData,
        }
    }

    fn split(mut self) -> (TeardownOwner<'runtime, A>, S) {
        let owner = std::mem::replace(&mut self.owner, TeardownOwner::empty());
        let stage = self
            .stage
            .take()
            .expect("teardown transaction owns one current stage");
        (owner, stage)
    }

    pub(super) fn close_module_callbacks(&self)
    where
        S: ModuleCloseStage,
    {
        self.stage
            .as_ref()
            .expect("teardown transaction owns one current stage")
            .close_module_callbacks();
    }
}

impl<A: Addin, K, S> Drop for TeardownTxn<'_, A, K, S> {
    fn drop(&mut self) {
        let Some(stage) = self.stage.take() else {
            return;
        };
        let runtime = self.owner.runtime();
        (self.recover)(stage, runtime);
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
            Err((error, module)) => {
                retain_drained_module(runtime, module);
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
            Err((error, module)) => {
                retain_drained_module(runtime, module);
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
        let runtime = owner.runtime();
        match stage.cleanup(lifecycle, report) {
            Ok(stage) => Ok(TeardownTxn::from_parts(owner, stage)),
            Err((error, module)) => {
                retain_drained_module(runtime, module);
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
        let runtime = owner.runtime();
        match stage.finish() {
            Ok(stage) => Ok(TeardownTxn::from_parts(owner, stage)),
            Err((error, module)) => {
                retain_drained_module(runtime, module);
                drop(owner);
                Err(error)
            }
        }
    }
}

impl<'runtime, A: Addin, K> TeardownTxn<'runtime, A, K, ServicesQuiescent> {
    pub(super) fn reclaim(
        self,
        rtd: crate::excel_rtd::RtdQuiescent,
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
    ) -> crate::XllResult<<K as TerminalCertificateKind>::Certificate<'runtime, A>>
    where
        K: TerminalCertificateKind,
    {
        let proof = self
            .stage
            .take()
            .expect("teardown transaction owns one current stage")
            .into_proof();
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
    module: crate::module_runtime::ModuleClosing,
    record_trace: bool,
) -> crate::XllResult<ExecutionDrained> {
    match ExecutionDrained::begin(runtime, module, record_trace) {
        Ok(stage) => Ok(stage),
        Err((error, module)) => {
            retain_drained_module(runtime, module);
            Err(error)
        }
    }
}

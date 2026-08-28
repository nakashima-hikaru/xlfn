//! Shared teardown stages for rollback and terminal removal.
//!
//! The two boundary pipelines intentionally keep different failure policy and
//! proof certificates. They do, however, share one ordering-sensitive stage:
//! close export admission, drain active calls, and wait for return producers.
//! Keeping that stage in the runtime shutdown domain prevents either pipeline
//! from silently changing the unload-safety ordering.

use super::certificate::TerminalCertificateKind;
use super::owner::RemovalOwner;
use crate::addin::Addin;
use crate::runtime::capabilities::ShutdownDeps;
use std::marker::PhantomData;
use std::panic::{AssertUnwindSafe, catch_unwind};

/// The single terminal shutdown proof carried across the lifecycle boundary.
///
/// The concrete subsystem certificates are consumed while this value is
/// assembled. Lifecycle code only needs the identity metadata that relates
/// the proof to the generation and module epoch it retired.
pub(crate) struct QuiescenceProof {
    services_generation: Option<crate::generation::RuntimeGeneration>,
    module_epoch: crate::module_runtime::ModuleEpochId,
}

impl QuiescenceProof {
    pub(crate) const fn services_generation(&self) -> Option<crate::generation::RuntimeGeneration> {
        self.services_generation
    }

    pub(crate) const fn module_epoch(&self) -> crate::module_runtime::ModuleEpochId {
        self.module_epoch
    }

    #[cfg(test)]
    pub(crate) const fn for_test(
        services_generation: Option<crate::generation::RuntimeGeneration>,
        module_epoch: crate::module_runtime::ModuleEpochId,
    ) -> Self {
        Self {
            services_generation,
            module_epoch,
        }
    }
}

/// Add-in state that has passed the quiesce boundary and is owned by the
/// teardown transaction until best-effort cleanup completes.
pub(crate) struct QuiescedAddin<'runtime, A: Addin> {
    deps: ShutdownDeps<'runtime, A>,
    generation: Option<crate::generation::RuntimeGeneration>,
    shared_state: Option<A::SharedState>,
}

pub(crate) struct CleanedAddin {
    pub(crate) addin_quiesced: crate::shutdown::AddinQuiesced,
    pub(crate) generation_reclaimed: crate::shutdown::GenerationReclaimed,
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
    pub(crate) fn empty(
        deps: ShutdownDeps<'runtime, A>,
        generation: Option<crate::generation::RuntimeGeneration>,
    ) -> Self {
        Self {
            deps,
            generation,
            shared_state: None,
        }
    }

    pub(crate) fn shared(
        deps: ShutdownDeps<'runtime, A>,
        generation: Option<crate::generation::RuntimeGeneration>,
        shared_state: A::SharedState,
    ) -> Self {
        Self {
            deps,
            generation,
            shared_state: Some(shared_state),
        }
    }

    pub(crate) fn cleanup(
        mut self,
        lifecycle: &crate::runtime::AddinLifecycleAccess<'_, A>,
        report: &mut crate::shutdown::CloseReport,
    ) -> Result<CleanedAddin, crate::XllError> {
        let Some(shared_state) = self.shared_state.take() else {
            return Ok(CleanedAddin::issued());
        };

        let cleanup = catch_unwind(AssertUnwindSafe(|| {
            self.deps
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

        let lifecycle_dropped = match self.deps.take_addin_lifecycle(lifecycle) {
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
        self.deps
            .quarantine()
            .retain_shared_state(self.generation, shared_state, reason);
    }
}

impl<A: Addin> Drop for QuiescedAddin<'_, A> {
    fn drop(&mut self) {
        if let Some(shared_state) = self.shared_state.take() {
            self.deps.quarantine().retain_shared_state(
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
pub(crate) struct ExecutionDrained {
    module: crate::module_runtime::ModuleExportsDrained,
    returns: crate::shutdown::ReturnsQuiescent,
}

/// The producer stage owns the execution-drain witness while async work and
/// subscription producers are stopped. Keeping these certificates together
/// prevents one pipeline from accidentally assembling a terminal proof with
/// only part of the producer shutdown sequence completed.
pub(crate) struct ProducersStopped {
    execution: ExecutionDrained,
    async_stopped: crate::shutdown::AsyncStopped,
    subscriptions_stopped: crate::shutdown::SubscriptionsStopped,
}

/// Owns the generation service seal while the remaining shutdown stages run.
///
/// The subscription certificate is consumed by `Runtime::seal_generation_services`
/// and cannot be separated from the corresponding sealed handle service. This
/// is the close-side counterpart of `ArmedServices` on the open path.
pub(crate) struct ServicesSealed<'runtime, A: Addin> {
    execution: ExecutionDrained,
    async_stopped: crate::shutdown::AsyncStopped,
    sealed: crate::runtime_components::SealedGenerationServices,
    addin: QuiescedAddin<'runtime, A>,
}

pub(crate) struct ServicesCleaned {
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
pub(crate) struct ServicesQuiescent {
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
pub(crate) struct ResourcesReclaimed {
    services: ServicesQuiescent,
    rtd: crate::excel_rtd::RtdQuiescent,
    host_callbacks: crate::shutdown::HostCallbacksDetached,
    diagnostics: crate::diagnostics::DiagnosticsStopped,
}

pub(crate) trait ModuleCloseStage {
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
    pub(crate) fn begin<A: Addin>(
        deps: ShutdownDeps<'_, A>,
        module: crate::module_runtime::ModuleClosing,
    ) -> Result<Self, (crate::XllError, crate::module_runtime::ModuleExportsDrained)> {
        let module = module.seal_and_drain();

        deps.observer().calls_drained();

        let returns = match deps.wait_for_return_quiescence() {
            Ok(returns) => returns,
            Err(error) => return Err((error, module)),
        };

        deps.observer().returns_drained();

        Ok(Self { module, returns })
    }

    pub(crate) fn close_module_callbacks(&self) {
        self.module.close_callbacks();
    }

    pub(crate) fn stop_producers<A: Addin>(
        self,
        deps: ShutdownDeps<'_, A>,
        report_issue: impl FnMut(&crate::shutdown::CleanupIssue),
    ) -> Result<ProducersStopped, (crate::XllError, crate::module_runtime::ModuleExportsDrained)>
    {
        #[cfg(feature = "async")]
        let mut report_issue = report_issue;
        #[cfg(not(feature = "async"))]
        let _ = report_issue;

        #[cfg(feature = "async")]
        let async_was_running = deps.async_manager().is_running();
        #[cfg(not(feature = "async"))]
        let async_was_running = false;
        #[cfg(feature = "async")]
        let async_stopped = {
            deps.async_manager().cancel_current_generation();
            let outcome = deps.async_manager().close();
            for issue in &outcome.issues {
                report_issue(issue);
            }
            outcome.certificate
        };
        if async_was_running {
            deps.observer().async_stopped();
        }
        #[cfg(not(feature = "async"))]
        let async_stopped = crate::shutdown::AsyncStopped::issue();

        let subscriptions_stopped = match deps.close_subscriptions() {
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
    pub(crate) fn seal_services<'runtime, A: Addin>(
        self,
        deps: ShutdownDeps<'runtime, A>,
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
        match deps.seal_generation_services(subscriptions_stopped) {
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
    pub(crate) fn cleanup(
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
    pub(crate) fn finish(
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
    pub(crate) fn new(
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

    pub(crate) fn into_proof(self) -> crate::XllResult<QuiescenceProof> {
        let Self {
            services,
            rtd,
            host_callbacks,
            diagnostics,
        } = self;
        let (module, returns, async_stopped, subscriptions_stopped, handles, addin, generation) =
            services.into_parts();
        let services_generation = match (subscriptions_stopped.generation(), handles.generation()) {
            (subscriptions_generation, handles_generation)
                if subscriptions_generation == handles_generation =>
            {
                subscriptions_generation
            }
            _ => {
                return Err(crate::XllError::Internal {
                    diagnostic_id: crate::diagnostics::id::DiagnosticId::CLOSE_CERTIFICATE,
                });
            }
        };
        let (module_quiescent, exports) = module.certify();
        let module_epoch = module_quiescent.id();
        let _terminal_resources = (
            exports,
            module_quiescent,
            returns,
            async_stopped,
            subscriptions_stopped,
            handles,
            rtd,
            host_callbacks,
            diagnostics,
            addin,
            generation,
        );
        Ok(QuiescenceProof {
            services_generation,
            module_epoch,
        })
    }
}

/// Returns the already-progressed module authority to the teardown owner when
/// an incomplete teardown is abandoned. The recovery path is deliberately
/// one-way: `ModuleExportsDrained` is stored as a drained cleanup authority and
/// is never converted back into `ModuleClosing`.
pub(crate) trait ModuleAuthorityRecovery {
    fn recover_module_authority(self) -> crate::module_runtime::ModuleCleanupAuthority;
}

impl ModuleAuthorityRecovery for ExecutionDrained {
    fn recover_module_authority(self) -> crate::module_runtime::ModuleCleanupAuthority {
        crate::module_runtime::ModuleCleanupAuthority::Drained(self.module)
    }
}

impl ModuleAuthorityRecovery for ProducersStopped {
    fn recover_module_authority(self) -> crate::module_runtime::ModuleCleanupAuthority {
        self.execution.recover_module_authority()
    }
}

impl<A: Addin> ModuleAuthorityRecovery for ServicesSealed<'_, A> {
    fn recover_module_authority(self) -> crate::module_runtime::ModuleCleanupAuthority {
        self.execution.recover_module_authority()
    }
}

impl ModuleAuthorityRecovery for ServicesCleaned {
    fn recover_module_authority(self) -> crate::module_runtime::ModuleCleanupAuthority {
        self.execution.recover_module_authority()
    }
}

impl ModuleAuthorityRecovery for ServicesQuiescent {
    fn recover_module_authority(self) -> crate::module_runtime::ModuleCleanupAuthority {
        crate::module_runtime::ModuleCleanupAuthority::Drained(self.module)
    }
}

impl ModuleAuthorityRecovery for ResourcesReclaimed {
    fn recover_module_authority(self) -> crate::module_runtime::ModuleCleanupAuthority {
        self.services.recover_module_authority()
    }
}

fn recover_stage<S: ModuleAuthorityRecovery>(
    stage: S,
) -> crate::module_runtime::ModuleCleanupAuthority {
    stage.recover_module_authority()
}

/// A linear teardown transaction parameterized by its terminal policy and
/// current stage. Dropping an incomplete transaction quarantines the runtime;
/// successful transitions consume the old stage and return the next one.
pub(crate) struct TeardownTxn<'runtime, A: Addin, K, S> {
    owner: TeardownOwner<'runtime, A>,
    stage: Option<S>,
    recover: fn(S) -> crate::module_runtime::ModuleCleanupAuthority,
    _kind: PhantomData<K>,
}

struct TeardownOwner<'runtime, A: Addin> {
    owner: Option<RemovalOwner<'runtime, A>>,
    deps: ShutdownDeps<'runtime, A>,
}

impl<'runtime, A: Addin> TeardownOwner<'runtime, A> {
    fn new(deps: ShutdownDeps<'runtime, A>, owner: RemovalOwner<'runtime, A>) -> Self {
        Self {
            owner: Some(owner),
            deps,
        }
    }

    fn take(&mut self) -> RemovalOwner<'runtime, A> {
        self.owner
            .take()
            .expect("teardown transaction owns one removal owner")
    }

    fn empty(deps: ShutdownDeps<'runtime, A>) -> Self {
        Self { owner: None, deps }
    }

    fn deps(&self) -> ShutdownDeps<'runtime, A> {
        self.deps
    }

    fn return_module_authority(
        &mut self,
        authority: crate::module_runtime::ModuleCleanupAuthority,
    ) {
        self.owner
            .as_mut()
            .expect("teardown transaction owns one removal owner")
            .return_module_authority(authority);
    }
}

impl<A: Addin> Drop for TeardownOwner<'_, A> {
    fn drop(&mut self) {
        if self.owner.is_some() {
            self.deps.quarantine_state();
        }
    }
}

impl<'runtime, A: Addin, K, S> TeardownTxn<'runtime, A, K, S> {
    pub(crate) fn new(
        deps: ShutdownDeps<'runtime, A>,
        owner: RemovalOwner<'runtime, A>,
        stage: S,
    ) -> Self
    where
        S: ModuleAuthorityRecovery,
    {
        Self {
            owner: TeardownOwner::new(deps, owner),
            stage: Some(stage),
            recover: recover_stage::<S>,
            _kind: PhantomData,
        }
    }

    fn from_parts(owner: TeardownOwner<'runtime, A>, stage: S) -> Self
    where
        S: ModuleAuthorityRecovery,
    {
        Self {
            owner,
            stage: Some(stage),
            recover: recover_stage::<S>,
            _kind: PhantomData,
        }
    }

    fn split(mut self) -> (TeardownOwner<'runtime, A>, S) {
        let deps = self.owner.deps();
        let owner = std::mem::replace(&mut self.owner, TeardownOwner::empty(deps));
        let stage = self
            .stage
            .take()
            .expect("teardown transaction owns one current stage");
        (owner, stage)
    }

    pub(crate) fn close_module_callbacks(&self)
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
        let authority = (self.recover)(stage);
        self.owner.return_module_authority(authority);
    }
}

impl<'runtime, A: Addin, K> TeardownTxn<'runtime, A, K, ExecutionDrained> {
    pub(crate) fn stop_producers(
        self,
        report_issue: impl FnMut(&crate::shutdown::CleanupIssue),
    ) -> crate::XllResult<TeardownTxn<'runtime, A, K, ProducersStopped>> {
        let (mut owner, stage) = self.split();
        let deps = owner.deps();
        match stage.stop_producers(deps, report_issue) {
            Ok(stage) => Ok(TeardownTxn::from_parts(owner, stage)),
            Err((error, module)) => {
                owner.return_module_authority(
                    crate::module_runtime::ModuleCleanupAuthority::Drained(module),
                );
                drop(owner);
                Err(error)
            }
        }
    }
}

impl<'runtime, A: Addin, K> TeardownTxn<'runtime, A, K, ProducersStopped> {
    pub(crate) fn seal_services(
        self,
        addin: QuiescedAddin<'runtime, A>,
    ) -> crate::XllResult<TeardownTxn<'runtime, A, K, ServicesSealed<'runtime, A>>> {
        let (mut owner, stage) = self.split();
        let deps = owner.deps();
        match stage.seal_services(deps, addin) {
            Ok(stage) => Ok(TeardownTxn::from_parts(owner, stage)),
            Err((error, module)) => {
                owner.return_module_authority(
                    crate::module_runtime::ModuleCleanupAuthority::Drained(module),
                );
                drop(owner);
                Err(error)
            }
        }
    }
}

impl<'runtime, A: Addin, K> TeardownTxn<'runtime, A, K, ServicesSealed<'runtime, A>> {
    pub(crate) fn cleanup_addin(
        self,
        lifecycle: &crate::runtime::AddinLifecycleAccess<'_, A>,
        report: &mut crate::shutdown::CloseReport,
    ) -> crate::XllResult<TeardownTxn<'runtime, A, K, ServicesCleaned>> {
        let (mut owner, stage) = self.split();
        match stage.cleanup(lifecycle, report) {
            Ok(stage) => Ok(TeardownTxn::from_parts(owner, stage)),
            Err((error, module)) => {
                owner.return_module_authority(
                    crate::module_runtime::ModuleCleanupAuthority::Drained(module),
                );
                drop(owner);
                Err(error)
            }
        }
    }
}

impl<'runtime, A: Addin, K> TeardownTxn<'runtime, A, K, ServicesCleaned> {
    pub(crate) fn finish_services(
        self,
    ) -> crate::XllResult<TeardownTxn<'runtime, A, K, ServicesQuiescent>> {
        let (mut owner, stage) = self.split();
        match stage.finish() {
            Ok(stage) => Ok(TeardownTxn::from_parts(owner, stage)),
            Err((error, module)) => {
                owner.return_module_authority(
                    crate::module_runtime::ModuleCleanupAuthority::Drained(module),
                );
                drop(owner);
                Err(error)
            }
        }
    }
}

impl<'runtime, A: Addin, K> TeardownTxn<'runtime, A, K, ServicesQuiescent> {
    pub(crate) fn reclaim(
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
    pub(crate) fn certify(
        mut self,
    ) -> crate::XllResult<<K as TerminalCertificateKind>::Certificate<'runtime, A>>
    where
        K: TerminalCertificateKind,
    {
        let proof = self
            .stage
            .take()
            .expect("teardown transaction owns one current stage")
            .into_proof()?;
        let deps = self.owner.deps();
        let owner = self.owner.take();
        match owner.certify::<K>(proof, deps) {
            Ok(certificate) => Ok(certificate),
            Err((error, owner)) => {
                drop(TeardownOwner::new(deps, owner));
                Err(error)
            }
        }
    }
}

pub(crate) fn drain_execution<A: Addin>(
    deps: ShutdownDeps<'_, A>,
    owner: &mut RemovalOwner<'_, A>,
) -> crate::XllResult<ExecutionDrained> {
    let module = owner.take_module_closing();
    match ExecutionDrained::begin(deps, module) {
        Ok(stage) => Ok(stage),
        Err((error, module)) => {
            owner.return_module_authority(crate::module_runtime::ModuleCleanupAuthority::Drained(
                module,
            ));
            Err(error)
        }
    }
}

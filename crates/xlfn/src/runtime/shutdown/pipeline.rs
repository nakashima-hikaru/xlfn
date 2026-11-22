//! Shared teardown stages for rollback and terminal removal.
//!
//! The two boundaries keep distinct failure policies and terminal certificates,
//! while consuming the same ordered execution, producer, service, add-in, and
//! resource-reclamation stages. Both close export admission and drain returns
//! before stopping producers, then finish add-in/service cleanup and diagnostics
//! before waiting for RTD module quiescence.

use super::certificate::TerminalCertificateKind;
use super::owner::{RemovalClaimGuard, RemovalOwner, ReturnedRemovalClaimGuard};
use crate::addin_impl::Addin;
use crate::error::IntoXllError;
use crate::panic_boundary::catch_no_unwind;
use crate::runtime::capabilities::{
    EmptyLifecycle, InstalledLifecycle, LifecycleFailure, RollbackRequest, ShutdownDeps,
};
use std::marker::PhantomData;
use std::panic::AssertUnwindSafe;

// Only this implementation can issue the certificates for the operations below.
// Re-exporting the types does not expose their constructors to orchestration.
mod certificates {
    #[derive(Debug)]
    pub(crate) struct AddinQuiescence(());
    #[derive(Debug)]
    pub(crate) struct GenerationReclaimed(());
    #[derive(Debug)]
    pub(crate) struct GenerationCallbacksDetached(());
    #[cfg(not(feature = "async"))]
    #[derive(Debug)]
    pub(crate) struct AsyncStopped(());

    impl AddinQuiescence {
        pub(super) const fn issue() -> Self {
            Self(())
        }
    }
    impl GenerationReclaimed {
        pub(super) const fn issue() -> Self {
            Self(())
        }
    }
    impl GenerationCallbacksDetached {
        pub(super) const fn issue() -> Self {
            Self(())
        }
    }
    #[cfg(not(feature = "async"))]
    impl AsyncStopped {
        pub(super) const fn feature_disabled() -> Self {
            Self(())
        }
    }
}
#[cfg(not(feature = "async"))]
pub(crate) use certificates::AsyncStopped;
pub(crate) use certificates::{AddinQuiescence, GenerationCallbacksDetached, GenerationReclaimed};

/// Drain fact and the unique lifecycle claim under which it was obtained.
/// A same-phase result from another runtime or earlier reopen is insufficient.
struct ReturnsQuiescent {
    drained: crate::runtime_components::ReturnsDrained,
    coordinator: usize,
    attempt: crate::generation::RemovalAttemptId,
}

impl ReturnsQuiescent {
    fn wait<A: Addin>(
        deps: ShutdownDeps<'_, A>,
        claim: &RemovalClaimGuard<'_, A>,
    ) -> crate::XllResult<Self> {
        if !claim.belongs_to(deps.lifecycle()) {
            return Err(crate::error::construct::internal(
                crate::diagnostics::catalog::CLOSE_CERTIFICATE,
            ));
        }
        let drained = deps.returns().close_admission().wait()?;
        Ok(Self {
            drained,
            coordinator: std::ptr::from_ref(deps.lifecycle()).addr(),
            attempt: claim.attempt(),
        })
    }

    fn matches<A: Addin>(
        &self,
        protocol: &crate::runtime_components::ReturnProtocol,
        coordinator: &crate::lifecycle::LifecycleCoordinator<A>,
        attempt: Option<crate::generation::RemovalAttemptId>,
    ) -> bool {
        self.coordinator == std::ptr::from_ref(coordinator).addr()
            && attempt == Some(self.attempt)
            && self.drained.matches(protocol)
    }
}

enum ReturnCompletion {
    Drained(ReturnsQuiescent),
    #[cfg(test)]
    ForTest,
}

/// The single terminal shutdown proof carried across the lifecycle boundary.
///
/// The concrete subsystem certificates are consumed while this value is
/// assembled. Lifecycle code only needs the identity metadata that relates
/// the proof to the generation and module epoch it retired.
pub(crate) struct QuiescenceProof {
    services_generation: Option<crate::generation::RuntimeGeneration>,
    module_epoch: crate::module_runtime::ModuleEpochId,
    returns: ReturnCompletion,
}

impl QuiescenceProof {
    pub(super) fn returns_match<A: Addin>(
        &self,
        protocol: &crate::runtime_components::ReturnProtocol,
        coordinator: &crate::lifecycle::LifecycleCoordinator<A>,
        attempt: Option<crate::generation::RemovalAttemptId>,
    ) -> bool {
        match &self.returns {
            ReturnCompletion::Drained(returns) => returns.matches(protocol, coordinator, attempt),
            #[cfg(test)]
            ReturnCompletion::ForTest => true,
        }
    }

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
            returns: ReturnCompletion::ForTest,
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
            addin_quiesced: AddinQuiescence::issue(),
            generation_reclaimed: GenerationReclaimed::issue(),
        }
    }
}

impl<'runtime, A: Addin> QuiescedAddin<'runtime, A> {
    fn empty(
        deps: ShutdownDeps<'runtime, A>,
        generation: Option<crate::generation::RuntimeGeneration>,
    ) -> Self {
        Self {
            deps,
            generation,
            shared_state: None,
        }
    }

    fn shared(
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

    fn quiesce(
        deps: ShutdownDeps<'runtime, A>,
        runtime: &crate::runtime::Runtime<A, impl crate::runtime::UnloadPolicy<A>>,
        lifecycle: &crate::runtime::AddinLifecycleAccess<'_, A>,
        generation_id: Option<crate::generation::RuntimeGeneration>,
        take_generation: impl FnOnce() -> Option<crate::generation::ShutdownGeneration<A>>,
    ) -> Result<Self, AddinQuiesceFailure> {
        // Validate the supplied policy/runtime and thread-affine access before
        // extracting any state that would otherwise be dropped on an error.
        if !std::ptr::eq(&runtime.lifecycle, deps.lifecycle()) {
            return Err(AddinQuiesceFailure::Lifecycle(
                crate::error::construct::internal(crate::diagnostics::catalog::CLOSE_RUNTIME),
            ));
        }
        let lifecycle_present = deps.has_addin_lifecycle(lifecycle).map_err(|error| {
            AddinQuiesceFailure::Lifecycle(crate::lifecycle::lifecycle_access_error(error))
        })?;
        let Some(generation) = take_generation() else {
            if lifecycle_present {
                return Err(AddinQuiesceFailure::Lifecycle(
                    crate::error::construct::internal(crate::diagnostics::catalog::LIFECYCLE_SLOT),
                ));
            }
            return Ok(Self::empty(deps, generation_id));
        };
        match generation {
            crate::generation::ShutdownGeneration::Open(generation) => {
                let mut generation = *generation;
                let quiesce = catch_no_unwind(AssertUnwindSafe(|| {
                    runtime
                        .with_addin_lifecycle(lifecycle, |lifecycle_state| {
                            runtime.quiesce_addin(&mut generation.shared_state, lifecycle_state)
                        })
                        .map_err(crate::lifecycle::lifecycle_access_error)
                }))
                .map_err(|_| crate::XllError::Panic)
                .and_then(|result| result)
                .and_then(|result| result);
                if let Err(error) = quiesce {
                    runtime.lifecycle_orchestrator().quarantine_generation(
                        generation_id,
                        generation,
                        crate::runtime_components::QuarantineReason::AddinQuiesceFailed,
                    );
                    return Err(AddinQuiesceFailure::Quiesce(error));
                }
                let addin = Self::shared(deps, generation_id, generation.shared_state);
                if catch_no_unwind(AssertUnwindSafe(|| drop(generation.layers))).is_err() {
                    return Err(AddinQuiesceFailure::Quiesce(crate::XllError::Panic));
                }
                Ok(addin)
            }
            crate::generation::ShutdownGeneration::Opening(opening) => {
                let (mut shared_state, layers, _config) = opening.into_parts();
                let quiesce = catch_no_unwind(AssertUnwindSafe(|| {
                    runtime
                        .with_addin_lifecycle(lifecycle, |lifecycle_state| {
                            runtime.quiesce_addin(&mut shared_state, lifecycle_state)
                        })
                        .map_err(crate::lifecycle::lifecycle_access_error)
                }))
                .map_err(|_| crate::XllError::Panic)
                .and_then(|result| result)
                .and_then(|result| result);
                if let Err(error) = quiesce {
                    runtime.lifecycle_orchestrator().quarantine_layers(
                        generation_id,
                        layers,
                        crate::runtime_components::QuarantineReason::AddinQuiesceFailed,
                    );
                    runtime.lifecycle_orchestrator().quarantine_shared_state(
                        generation_id,
                        shared_state,
                        crate::runtime_components::QuarantineReason::AddinQuiesceFailed,
                    );
                    return Err(AddinQuiesceFailure::Quiesce(error));
                }
                let addin = Self::shared(deps, generation_id, shared_state);
                if catch_no_unwind(AssertUnwindSafe(|| drop(layers))).is_err() {
                    return Err(AddinQuiesceFailure::Quiesce(crate::XllError::Panic));
                }
                Ok(addin)
            }
        }
    }

    fn cleanup(
        mut self,
        lifecycle: &crate::runtime::AddinLifecycleAccess<'_, A>,
        report: &mut crate::shutdown::CloseReport,
        installed: Option<InstalledLifecycle<'runtime, A>>,
    ) -> Result<(CleanedAddin, EmptyLifecycle<'runtime, A>), LifecycleFailure<'runtime, A>> {
        let Some(shared_state) = self.shared_state.take() else {
            let empty = self
                .deps
                .empty_addin_lifecycle(lifecycle)
                .map_err(LifecycleFailure::access)?;
            return Ok((CleanedAddin::issued(), empty));
        };

        let cleanup = catch_no_unwind(AssertUnwindSafe(|| {
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
            return Err(LifecycleFailure::retain(crate::XllError::Panic, installed));
        }

        let taken = match installed {
            Some(binding) => binding.take(),
            None => self.deps.take_addin_lifecycle(lifecycle),
        };
        let empty_binding = match taken {
            Ok(taken) => match taken.dispose() {
                Ok(empty) => empty,
                Err(error) => {
                    report.push(
                        "Addin::LifecycleState::drop",
                        crate::shutdown::CleanupIssueKind::DisposalPanicked,
                        error.error().clone(),
                    );
                    self.quarantine_shared_state(
                        shared_state,
                        crate::runtime_components::QuarantineReason::TeardownIncomplete,
                    );
                    return Err(error);
                }
            },
            Err(error) => {
                report.push(
                    "Addin::LifecycleState",
                    crate::shutdown::CleanupIssueKind::DisposalPanicked,
                    error.error().clone(),
                );
                self.quarantine_shared_state(
                    shared_state,
                    crate::runtime_components::QuarantineReason::TeardownIncomplete,
                );
                return Err(error);
            }
        };
        let shared_state_dropped = catch_no_unwind(AssertUnwindSafe(|| drop(shared_state))).is_ok();
        if !shared_state_dropped {
            report.push(
                "Addin::SharedState::drop",
                crate::shutdown::CleanupIssueKind::DisposalPanicked,
                crate::XllError::Panic,
            );
        }
        if !shared_state_dropped {
            return Err(LifecycleFailure::disposed(
                crate::error::construct::internal(crate::diagnostics::catalog::LIFECYCLE_SLOT),
                empty_binding,
            ));
        }
        Ok((CleanedAddin::issued(), empty_binding))
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

/// Execution and return admission have both drained. Their proofs stay with
/// the stopping owner; the stage only gates the next transition.
pub(crate) struct ExecutionDrained(());

pub(crate) enum ExecutionDrainError {
    Preparation(crate::XllError),
    ReturnQuiescence(crate::XllError),
}

pub(crate) struct ProducerStopFailure {
    hazard: crate::shutdown::UnloadHazard,
    error: crate::XllError,
}

impl ProducerStopFailure {
    pub(crate) fn hazard(&self) -> crate::shutdown::UnloadHazard {
        self.hazard
    }

    pub(crate) fn into_error(self) -> crate::XllError {
        self.error
    }
}

impl ExecutionDrainError {
    pub(crate) fn hazard(&self) -> crate::shutdown::UnloadHazard {
        match self {
            Self::Preparation(_) => crate::shutdown::UnloadHazard::ExportDrainPreparationFailed,
            Self::ReturnQuiescence(_) => crate::shutdown::UnloadHazard::CloseInvariantViolation,
        }
    }

    pub(crate) fn into_error(self) -> crate::XllError {
        match self {
            Self::Preparation(error) | Self::ReturnQuiescence(error) => error,
        }
    }
}

/// Async work and subscription producers have both stopped.
pub(crate) struct ProducersStopped {
    async_stopped: crate::shutdown::AsyncStopped,
    subscriptions_stopped: crate::shutdown::SubscriptionsStopped,
}

/// Callback admission is closed; host detachment has not yet been checked.
pub(crate) struct CallbacksClosed {
    producers: ProducersStopped,
}

/// Every direct host callback is finished and generation registrations detached.
pub(crate) struct HostDetached {
    producers: ProducersStopped,
    host_callbacks: crate::shutdown::GenerationCallbacksDetached,
}

/// Quiescence is produced only by invoking the selected add-in contract.
pub(crate) struct AddinQuiesced<'runtime, A: Addin> {
    host: HostDetached,
    addin: QuiescedAddin<'runtime, A>,
}

pub(crate) enum HostDetachFailure {
    RegistrationUnknown,
    CallbacksRegistered,
}

impl HostDetachFailure {
    pub(crate) fn hazard(&self) -> crate::shutdown::UnloadHazard {
        match self {
            Self::RegistrationUnknown => crate::shutdown::UnloadHazard::RegistrationStateUnknown,
            Self::CallbacksRegistered => crate::shutdown::UnloadHazard::HostCallbackStillRegistered,
        }
    }

    pub(crate) fn into_error(self) -> crate::XllError {
        crate::error::construct::internal(crate::diagnostics::catalog::REGISTRATION_UNKNOWN)
    }
}

pub(crate) enum AddinQuiesceFailure {
    Lifecycle(crate::XllError),
    Quiesce(crate::XllError),
}

impl AddinQuiesceFailure {
    pub(crate) fn hazard(&self) -> crate::shutdown::UnloadHazard {
        match self {
            Self::Lifecycle(_) => crate::shutdown::UnloadHazard::CloseInvariantViolation,
            Self::Quiesce(_) => crate::shutdown::UnloadHazard::AddinQuiesceFailed,
        }
    }

    pub(crate) fn into_error(self) -> crate::XllError {
        match self {
            Self::Lifecycle(error) | Self::Quiesce(error) => error,
        }
    }
}

pub(crate) enum ServiceSealFailure {
    HandleTopics(crate::XllError),
    Handles(crate::XllError),
}

/// The shutdown boundary selects the report label, not the hazard classification.
pub(crate) enum ShutdownDiagnosticContext {
    OpenRollback,
    Removal,
}

type ShutdownDiagnostic = (crate::shutdown::UnloadHazard, &'static str, crate::XllError);

impl ServiceSealFailure {
    pub(crate) fn into_diagnostic(self, context: ShutdownDiagnosticContext) -> ShutdownDiagnostic {
        use crate::shutdown::UnloadHazard;
        match self {
            Self::HandleTopics(error) => (
                UnloadHazard::RtdGitCallbackStillRegistered,
                match context {
                    ShutdownDiagnosticContext::OpenRollback => "xlAutoOpen RTD rollback",
                    ShutdownDiagnosticContext::Removal => "xlAutoRemove RTD shutdown",
                },
                error,
            ),
            Self::Handles(error) => (
                UnloadHazard::HandleStoreNotQuiescent,
                match context {
                    ShutdownDiagnosticContext::OpenRollback => "xlAutoOpen handle rollback",
                    ShutdownDiagnosticContext::Removal => "xlAutoRemove handle table shutdown",
                },
                error,
            ),
        }
    }
}

/// Owns the generation service seal while the remaining shutdown stages run.
///
/// Handle topics have released their backend references before the seal hides
/// the ready-only handle slot from the topic shutdown probe.
///
/// The subscription certificate is consumed by `Runtime::seal_generation_services`
/// and cannot be separated from the corresponding sealed handle service. This
/// is the close-side counterpart of `ArmedServices` on the open path.
pub(crate) struct ServicesSealed<'runtime, A: Addin> {
    async_stopped: crate::shutdown::AsyncStopped,
    sealed: crate::runtime_components::SealedGenerationServices,
    addin: QuiescedAddin<'runtime, A>,
    host_callbacks: crate::shutdown::GenerationCallbacksDetached,
}

pub(crate) struct ServicesCleaned {
    async_stopped: crate::shutdown::AsyncStopped,
    sealed: crate::runtime_components::SealedGenerationServices,
    addin: CleanedAddin,
    host_callbacks: crate::shutdown::GenerationCallbacksDetached,
}

/// The generation services have both been sealed and fully finished.
///
/// At this point the producer certificates and handle-store certificate are
/// one value, so a caller cannot accidentally assemble a terminal proof from
/// a handle certificate belonging to a different producer stage.
pub(crate) struct ServicesQuiescent {
    async_stopped: crate::shutdown::AsyncStopped,
    subscriptions_stopped: crate::shutdown::SubscriptionsStopped,
    handles: crate::shutdown::HandlesQuiescent,
    addin: crate::shutdown::AddinQuiesced,
    generation: crate::shutdown::GenerationReclaimed,
    host_callbacks: crate::shutdown::GenerationCallbacksDetached,
}

/// Owns every resource certificate after producers have stopped. The only
/// operation that can expose the aggregate proof is `complete`, so callers
/// cannot accidentally certify a partially assembled terminal transition.
pub(crate) struct ResourcesReclaimed {
    services: ServicesQuiescent,
    rtd: crate::excel_rtd_protocol::RtdQuiescent,
    diagnostics: crate::diagnostics::DiagnosticsStopped,
}

impl ExecutionDrained {
    fn stop_producers<A: Addin>(
        self,
        deps: ShutdownDeps<'_, A>,
        mut report_issue: impl FnMut(&crate::shutdown::CleanupIssue),
    ) -> Result<ProducersStopped, ProducerStopFailure> {
        #[cfg(feature = "async")]
        let async_was_running = !deps.async_runtime().is_stopped();
        #[cfg(not(feature = "async"))]
        let async_was_running = false;
        #[cfg(feature = "async")]
        let async_stopped = {
            // Scoped async handle tasks are drained before generation
            // services (including FormulaHandleService/ObjectArena) are
            // sealed and consumed below.
            let outcome = match deps.async_runtime().close() {
                Ok(outcome) => outcome,
                Err(error) => {
                    return Err(ProducerStopFailure {
                        hazard: crate::shutdown::UnloadHazard::AsyncExecutorStillRunning,
                        error,
                    });
                }
            };
            for issue in &outcome.issues {
                report_issue(issue);
            }
            outcome.certificate
        };
        if async_was_running {
            deps.observer().async_stopped();
        }
        #[cfg(not(feature = "async"))]
        let async_stopped = AsyncStopped::feature_disabled();

        let subscription_outcome = match deps.close_subscriptions() {
            Ok(outcome) => outcome,
            Err(error) => {
                return Err(ProducerStopFailure {
                    hazard: crate::shutdown::UnloadHazard::SubscriptionProducerStillRunning,
                    error,
                });
            }
        };
        for issue in &subscription_outcome.issues {
            report_issue(issue);
        }

        Ok(ProducersStopped {
            async_stopped,
            subscriptions_stopped: subscription_outcome.certificate,
        })
    }
}

impl<'runtime, A: Addin> AddinQuiesced<'runtime, A> {
    fn seal_services(
        self,
        deps: ShutdownDeps<'runtime, A>,
    ) -> crate::XllResult<ServicesSealed<'runtime, A>> {
        let Self { host, addin } = self;
        let HostDetached {
            producers,
            host_callbacks,
        } = host;
        let ProducersStopped {
            async_stopped,
            subscriptions_stopped,
        } = producers;
        #[cfg(test)]
        tests::ServiceSealFault::check(tests::ServiceSealPoint::Handles)?;
        let sealed = deps.seal_generation_services(subscriptions_stopped)?;
        Ok(ServicesSealed {
            async_stopped,
            sealed,
            addin,
            host_callbacks,
        })
    }
}

impl<'runtime, A: Addin> ServicesSealed<'runtime, A> {
    fn cleanup(
        self,
        lifecycle: &crate::runtime::AddinLifecycleAccess<'_, A>,
        report: &mut crate::shutdown::CloseReport,
        installed: Option<InstalledLifecycle<'runtime, A>>,
    ) -> Result<(ServicesCleaned, EmptyLifecycle<'runtime, A>), LifecycleFailure<'runtime, A>> {
        let Self {
            async_stopped,
            sealed,
            addin,
            host_callbacks,
        } = self;
        if let Some(issue) = sealed.cleanup_issue() {
            report.push(issue.component, issue.kind, issue.error);
        }
        match addin.cleanup(lifecycle, report, installed) {
            Ok((addin, empty)) => Ok((
                ServicesCleaned {
                    async_stopped,
                    sealed,
                    addin,
                    host_callbacks,
                },
                empty,
            )),
            Err(error) => Err(error),
        }
    }
}

impl ServicesCleaned {
    fn finish(self) -> crate::XllResult<ServicesQuiescent> {
        let Self {
            async_stopped,
            sealed,
            addin,
            host_callbacks,
        } = self;
        match sealed.finish() {
            Ok((handles, subscriptions_stopped)) => Ok(ServicesQuiescent {
                async_stopped,
                subscriptions_stopped,
                handles,
                addin: addin.addin_quiesced,
                generation: addin.generation_reclaimed,
                host_callbacks,
            }),
            Err(error) => Err(error),
        }
    }
}

impl ServicesQuiescent {
    fn into_parts(
        self,
    ) -> (
        crate::shutdown::AsyncStopped,
        crate::shutdown::SubscriptionsStopped,
        crate::shutdown::HandlesQuiescent,
        crate::shutdown::AddinQuiesced,
        crate::shutdown::GenerationReclaimed,
        crate::shutdown::GenerationCallbacksDetached,
    ) {
        (
            self.async_stopped,
            self.subscriptions_stopped,
            self.handles,
            self.addin,
            self.generation,
            self.host_callbacks,
        )
    }
}

/// A complete, affine collection of all non-module teardown certificates.
/// Only the teardown pipeline can construct this value.
pub(crate) struct CompletedResources<'owner> {
    module: &'owner crate::module_runtime::ModuleExportsDrained,
    certificates: CompletedResourceCertificates,
}

pub(crate) struct CompletedResourceCertificates {
    _certificates: (
        crate::shutdown::AsyncStopped,
        crate::shutdown::SubscriptionsStopped,
        crate::shutdown::HandlesQuiescent,
        crate::excel_rtd_protocol::RtdQuiescent,
        crate::shutdown::GenerationCallbacksDetached,
        crate::diagnostics::DiagnosticsStopped,
        crate::shutdown::AddinQuiesced,
        crate::shutdown::GenerationReclaimed,
    ),
}

impl<'owner> CompletedResources<'owner> {
    pub(crate) fn into_module_parts(
        self,
    ) -> (
        &'owner crate::module_runtime::ModuleExportsDrained,
        CompletedResourceCertificates,
    ) {
        (self.module, self.certificates)
    }
}

pub(crate) enum ReclaimFailure {
    Diagnostics(crate::XllError),
    Rtd(crate::excel_rtd_protocol::RtdQuiescenceError),
}

impl ReclaimFailure {
    pub(crate) fn into_diagnostic(self, context: ShutdownDiagnosticContext) -> ShutdownDiagnostic {
        use crate::diagnostics::catalog::{MODULE_RESIDENCY, RTD_GIT_QUIESCENCE};
        use crate::shutdown::UnloadHazard;
        match self {
            Self::Diagnostics(error) => (
                UnloadHazard::DiagnosticWorkerStillRunning,
                match context {
                    ShutdownDiagnosticContext::OpenRollback => "xlAutoOpen diagnostic rollback",
                    ShutdownDiagnosticContext::Removal => "xlAutoRemove diagnostic refinement",
                },
                error,
            ),
            Self::Rtd(error) => {
                let (hazard, diagnostic_id) = if error.missing_module_residency {
                    (
                        UnloadHazard::RtdComObjectsWithoutResidency,
                        MODULE_RESIDENCY,
                    )
                } else if error.revocation_debt != 0 {
                    (UnloadHazard::RtdGitRevocationDebt, RTD_GIT_QUIESCENCE)
                } else {
                    (
                        UnloadHazard::RtdGitCallbackStillRegistered,
                        RTD_GIT_QUIESCENCE,
                    )
                };
                let boundary = match context {
                    ShutdownDiagnosticContext::OpenRollback => "xlAutoOpen RTD quiescence rollback",
                    ShutdownDiagnosticContext::Removal => "xlAutoRemove RTD logical quiescence",
                };
                (
                    hazard,
                    boundary,
                    crate::error::construct::internal(diagnostic_id),
                )
            }
        }
    }
}

impl ResourcesReclaimed {
    pub(crate) fn new(
        services: ServicesQuiescent,
        rtd: crate::excel_rtd_protocol::RtdQuiescent,
        diagnostics: crate::diagnostics::DiagnosticsStopped,
    ) -> Self {
        Self {
            services,
            rtd,
            diagnostics,
        }
    }

    fn complete(
        self,
    ) -> crate::XllResult<(
        Option<crate::generation::RuntimeGeneration>,
        CompletedResourceCertificates,
    )> {
        let Self {
            services,
            rtd,
            diagnostics,
        } = self;
        let (async_stopped, subscriptions_stopped, handles, addin, generation, host_callbacks) =
            services.into_parts();
        let services_generation = handles.generation();
        if subscriptions_stopped.generation() != services_generation {
            return Err(crate::error::construct::internal(
                crate::diagnostics::catalog::CLOSE_CERTIFICATE,
            ));
        }
        Ok((
            services_generation,
            CompletedResourceCertificates {
                _certificates: (
                    async_stopped,
                    subscriptions_stopped,
                    handles,
                    rtd,
                    host_callbacks,
                    diagnostics,
                    addin,
                    generation,
                ),
            },
        ))
    }
}

/// Only genuine teardown stages assign an STA binding phase. Module
/// authority and its recovery do not depend on this phase.
pub(crate) trait BindingPhase {
    type Binding<'runtime, A: Addin>;
}

macro_rules! binding_phase {
    ($binding:ty; $($stage:ty),+ $(,)?) => {$(
        impl BindingPhase for $stage {
            type Binding<'runtime, A: Addin> = $binding;
        }
    )+};
}

binding_phase!(Option<InstalledLifecycle<'runtime, A>>;
    ExecutionDrained, ProducersStopped, CallbacksClosed, HostDetached,
);
binding_phase!(EmptyLifecycle<'runtime, A>;
    ServicesCleaned, ServicesQuiescent, ResourcesReclaimed,
);
impl<B: Addin> BindingPhase for AddinQuiesced<'_, B> {
    type Binding<'runtime, A: Addin> = Option<InstalledLifecycle<'runtime, A>>;
}
impl<B: Addin> BindingPhase for ServicesSealed<'_, B> {
    type Binding<'runtime, A: Addin> = Option<InstalledLifecycle<'runtime, A>>;
}

/// An affine teardown transaction. The stage and its owner stay paired until
/// a transition takes them; only the owner can recover the module capability.
pub(crate) struct TeardownTxn<'runtime, A: Addin, K, S: BindingPhase> {
    // Dispose the stage while the claim is still outstanding. Unwinding
    // through a stage destructor still drops the owner and returns authority.
    stage: S,
    owner: StoppingOwner<'runtime, A, S::Binding<'runtime, A>>,
    _kind: PhantomData<K>,
}

/// The exact canonical claim and progressed module capability travel
/// together, independent of the stage and the STA binding phase.
struct StoppingAuthority<'runtime, A: Addin> {
    claim: RemovalClaimGuard<'runtime, A>,
    module: crate::module_runtime::ModuleExportsDrained,
}

struct StoppingCore<'runtime, A: Addin> {
    deps: ShutdownDeps<'runtime, A>,
    authority: Option<StoppingAuthority<'runtime, A>>,
    returns: Option<ReturnsQuiescent>,
}

struct StoppingOwner<'runtime, A: Addin, B> {
    // Recover and publish quarantine before releasing the binding.
    core: StoppingCore<'runtime, A>,
    binding: B,
}

impl<'runtime, A: Addin> StoppingCore<'runtime, A> {
    fn authority(&self) -> &StoppingAuthority<'runtime, A> {
        self.authority.as_ref().expect("live stopping authority")
    }

    fn into_returned(mut self) -> ReturnedRemovalClaimGuard<'runtime, A> {
        self.recover().expect("live stopping authority")
    }

    fn recover(&mut self) -> Option<ReturnedRemovalClaimGuard<'runtime, A>> {
        // Keep the claim and module together until quarantine is visible.
        self.authority.as_ref()?;
        self.deps.quarantine_state();
        let StoppingAuthority { claim, module } = self.authority.take()?;
        Some(
            claim.return_module_authority(crate::module_runtime::ModuleCleanupAuthority::Drained(
                module,
            )),
        )
    }
}

impl<A: Addin> Drop for StoppingCore<'_, A> {
    fn drop(&mut self) {
        drop(self.recover());
    }
}

impl<'runtime, A: Addin, B> StoppingOwner<'runtime, A, B> {
    fn deps(&self) -> ShutdownDeps<'runtime, A> {
        self.core.deps
    }

    fn retain(self) -> RetainedTeardown<'runtime, A, B> {
        RetainedTeardown {
            _claim: self.core.into_returned(),
            binding: self.binding,
        }
    }
}

impl<'runtime, A: Addin> StoppingOwner<'runtime, A, EmptyLifecycle<'runtime, A>> {
    fn certify<K: TerminalCertificateKind>(
        mut self,
        resources: ResourcesReclaimed,
    ) -> crate::XllResult<super::finalization::CompletionPendingBinding<'runtime, A, K>> {
        let deps = self.deps();
        let (services_generation, certificates) = resources.complete()?;
        let returns = self.core.returns.take().expect("completed return drain");
        let authority = self.core.authority();
        if !returns.matches(
            deps.returns(),
            deps.lifecycle(),
            Some(authority.claim.attempt()),
        ) || !authority.claim.belongs_to(deps.lifecycle())
        {
            return Err(crate::error::construct::internal(
                crate::diagnostics::catalog::CLOSE_CERTIFICATE,
            ));
        }
        // Borrow the module while the final resource checks can still fail.
        // The exact claim and module remain recoverable together on error.
        let (module_quiescent, _exports) =
            crate::module_runtime::ModuleExportsDrained::certify(CompletedResources {
                module: &authority.module,
                certificates,
            })?;
        let proof = QuiescenceProof {
            services_generation,
            module_epoch: module_quiescent.id(),
            returns: ReturnCompletion::Drained(returns),
        };
        let StoppingAuthority { claim, module } =
            self.core.authority.take().expect("live stopping authority");
        let empty = self.binding;
        match claim.certify::<K>(proof, deps) {
            Ok(certificate) => Ok(super::finalization::CompletionPendingBinding::new(
                deps,
                certificate,
                empty,
            )),
            Err((error, claim)) => {
                drop(StoppingOwner {
                    core: StoppingCore {
                        deps,
                        authority: Some(StoppingAuthority { claim, module }),
                        returns: None,
                    },
                    binding: empty,
                });
                Err(error)
            }
        }
    }
}

/// Keeps the claim alive after the module capability has returned on a
/// failed execution drain, until the caller has reported the failure.
struct RetainedTeardown<'runtime, A: Addin, B> {
    _claim: ReturnedRemovalClaimGuard<'runtime, A>,
    binding: B,
}

/// Before the outer drain, protect the claim and binding on errors. This
/// guard never carries a post-drain module capability.
struct PreparingTeardown<'runtime, A: Addin> {
    deps: ShutdownDeps<'runtime, A>,
    claim: Option<RemovalClaimGuard<'runtime, A>>,
}

impl<'runtime, A: Addin> PreparingTeardown<'runtime, A> {
    fn take(&mut self) -> RemovalClaimGuard<'runtime, A> {
        self.claim
            .take()
            .expect("preparing teardown owns the removal claim")
    }

    fn retain<B>(
        mut self,
        authority: crate::module_runtime::ModuleCleanupAuthority,
        binding: B,
    ) -> RetainedTeardown<'runtime, A, B> {
        self.deps.quarantine_state();
        RetainedTeardown {
            _claim: self.take().return_module_authority(authority),
            binding,
        }
    }
}

impl<A: Addin> Drop for PreparingTeardown<'_, A> {
    fn drop(&mut self) {
        if self.claim.is_some() {
            self.deps.quarantine_state();
        }
    }
}

impl<'runtime, A: Addin, K, S: BindingPhase> TeardownTxn<'runtime, A, K, S> {
    fn from_parts(owner: StoppingOwner<'runtime, A, S::Binding<'runtime, A>>, stage: S) -> Self {
        Self {
            stage,
            owner,
            _kind: PhantomData,
        }
    }

    fn owner(&self) -> &StoppingOwner<'runtime, A, S::Binding<'runtime, A>> {
        &self.owner
    }

    fn split(self) -> (StoppingOwner<'runtime, A, S::Binding<'runtime, A>>, S) {
        // Split locals drop in reverse order, so stage still precedes owner.
        let Self { owner, stage, .. } = self;
        #[cfg(test)]
        tests::check_teardown_split_fault();
        (owner, stage)
    }
}

impl<'runtime, A: Addin, K> TeardownTxn<'runtime, A, K, ExecutionDrained> {
    pub(crate) fn stop_producers(
        self,
        report_issue: impl FnMut(&crate::shutdown::CleanupIssue),
    ) -> Result<TeardownTxn<'runtime, A, K, ProducersStopped>, ProducerStopFailure> {
        let (owner, stage) = self.split();
        let deps = owner.deps();
        let stage = stage.stop_producers(deps, report_issue)?;
        Ok(TeardownTxn::from_parts(owner, stage))
    }
}

impl<'runtime, A: Addin, K> TeardownTxn<'runtime, A, K, ProducersStopped> {
    pub(crate) fn close_module_callbacks(self) -> TeardownTxn<'runtime, A, K, CallbacksClosed> {
        let (owner, producers) = self.split();
        owner.core.authority().module.close_callbacks();
        TeardownTxn::from_parts(owner, CallbacksClosed { producers })
    }
}

impl<'runtime, A: Addin, K> TeardownTxn<'runtime, A, K, CallbacksClosed> {
    pub(crate) fn detach_host(
        self,
    ) -> Result<TeardownTxn<'runtime, A, K, HostDetached>, HostDetachFailure> {
        let deps = self.owner().deps();
        if deps.host().registration_state_unknown() {
            return Err(HostDetachFailure::RegistrationUnknown);
        }
        if !deps.host().generation_callbacks_detached() {
            return Err(HostDetachFailure::CallbacksRegistered);
        }
        let (owner, closed) = self.split();
        Ok(TeardownTxn::from_parts(
            owner,
            HostDetached {
                producers: closed.producers,
                host_callbacks: GenerationCallbacksDetached::issue(),
            },
        ))
    }
}

impl<'runtime, A: Addin, K> TeardownTxn<'runtime, A, K, HostDetached> {
    fn quiesce_generation(
        self,
        runtime: &crate::runtime::Runtime<A, impl crate::runtime::UnloadPolicy<A>>,
        lifecycle: &crate::runtime::AddinLifecycleAccess<'_, A>,
        generation_id: Option<crate::generation::RuntimeGeneration>,
        take_generation: impl FnOnce() -> Option<crate::generation::ShutdownGeneration<A>>,
    ) -> Result<TeardownTxn<'runtime, A, K, AddinQuiesced<'runtime, A>>, AddinQuiesceFailure> {
        let addin = QuiescedAddin::quiesce(
            self.owner().deps(),
            runtime,
            lifecycle,
            generation_id,
            take_generation,
        )?;
        let (owner, host) = self.split();
        Ok(TeardownTxn::from_parts(
            owner,
            AddinQuiesced { host, addin },
        ))
    }
}

impl<'runtime, A: Addin> TeardownTxn<'runtime, A, super::certificate::FinalRemoval, HostDetached> {
    pub(crate) fn quiesce_addin(
        self,
        runtime: &crate::runtime::Runtime<A, impl crate::runtime::UnloadPolicy<A>>,
        lifecycle: &crate::runtime::AddinLifecycleAccess<'_, A>,
    ) -> Result<
        TeardownTxn<'runtime, A, super::certificate::FinalRemoval, AddinQuiesced<'runtime, A>>,
        AddinQuiesceFailure,
    > {
        let deps = self.owner().deps();
        self.quiesce_generation(runtime, lifecycle, deps.protocol_generation(), || {
            deps.take_generation_for_shutdown()
        })
    }
}

impl<'runtime, A: Addin> TeardownTxn<'runtime, A, super::certificate::OpenRollback, HostDetached> {
    pub(crate) fn quiesce_addin(
        self,
        runtime: &crate::runtime::Runtime<A, impl crate::runtime::UnloadPolicy<A>>,
        lifecycle: &crate::runtime::AddinLifecycleAccess<'_, A>,
        generation_id: Option<crate::generation::RuntimeGeneration>,
    ) -> Result<
        TeardownTxn<'runtime, A, super::certificate::OpenRollback, AddinQuiesced<'runtime, A>>,
        AddinQuiesceFailure,
    > {
        self.quiesce_generation(runtime, lifecycle, generation_id, || {
            runtime
                .lifecycle_orchestrator()
                .take_opening_for_rollback()
                .map(crate::generation::ShutdownGeneration::Opening)
        })
    }
}

impl<'runtime, A: Addin, K> TeardownTxn<'runtime, A, K, AddinQuiesced<'runtime, A>> {
    pub(crate) fn seal_services(
        self,
    ) -> Result<TeardownTxn<'runtime, A, K, ServicesSealed<'runtime, A>>, ServiceSealFailure> {
        // Keep the transaction intact until topics have detached. Failure must
        // run its normal Drop recovery with both stage and owner still present.
        #[cfg(test)]
        tests::ServiceSealFault::check(tests::ServiceSealPoint::HandleTopics)
            .map_err(ServiceSealFailure::HandleTopics)?;
        self.owner()
            .deps()
            .shutdown_handle_topics()
            .map_err(ServiceSealFailure::HandleTopics)?;
        let (owner, stage) = self.split();
        let deps = owner.deps();
        let stage = stage
            .seal_services(deps)
            .map_err(ServiceSealFailure::Handles)?;
        Ok(TeardownTxn::from_parts(owner, stage))
    }
}

impl<'runtime, A: Addin, K> TeardownTxn<'runtime, A, K, ServicesSealed<'runtime, A>> {
    pub(crate) fn cleanup_addin(
        self,
        lifecycle: &crate::runtime::AddinLifecycleAccess<'_, A>,
        report: &mut crate::shutdown::CloseReport,
    ) -> crate::XllResult<TeardownTxn<'runtime, A, K, ServicesCleaned>> {
        let (owner, stage) = self.split();
        let StoppingOwner {
            core,
            binding: installed,
        } = owner;
        match stage.cleanup(lifecycle, report, installed) {
            Ok((stage, empty)) => Ok(TeardownTxn::from_parts(
                StoppingOwner {
                    core,
                    binding: empty,
                },
                stage,
            )),
            Err(failure) => {
                let error = failure.error().clone();
                // Quarantine and return the module before the failed binding
                // releases its thread-affine responsibility.
                drop(StoppingOwner {
                    core,
                    binding: failure,
                });
                Err(error)
            }
        }
    }
}

impl<'runtime, A: Addin, K> TeardownTxn<'runtime, A, K, ServicesCleaned> {
    pub(crate) fn finish_services(
        self,
    ) -> crate::XllResult<TeardownTxn<'runtime, A, K, ServicesQuiescent>> {
        let (owner, stage) = self.split();
        let stage = stage.finish()?;
        Ok(TeardownTxn::from_parts(owner, stage))
    }
}

impl<'runtime, A: Addin, K: TerminalCertificateKind>
    TeardownTxn<'runtime, A, K, ServicesQuiescent>
{
    pub(crate) fn reclaim(
        self,
        mut report_issue: impl FnMut(&crate::shutdown::CleanupIssue),
    ) -> Result<TeardownTxn<'runtime, A, K, ResourcesReclaimed>, ReclaimFailure> {
        let deps = self.owner().deps();
        let diagnostics_was_running = crate::diagnostics::diagnostic_sink_running();
        #[cfg(test)]
        tests::check_reclaim_fault(false)?;
        let outcome = crate::diagnostics::close_diagnostic_router()
            .map_err(|error| ReclaimFailure::Diagnostics(error.into_xll_error()))?;
        for issue in outcome.issues {
            if K::RECORD_RECLAIM {
                deps.observer().cleanup_issue();
            }
            report_issue(&issue);
        }
        if K::RECORD_RECLAIM {
            if diagnostics_was_running {
                deps.observer().diagnostics_stopped();
            }
            deps.observer().diagnostics_drained();
        }
        #[cfg(test)]
        tests::check_reclaim_fault(true)?;
        let rtd = crate::excel_rtd_protocol::wait_for_module_quiescence(deps.residency().is_held())
            .map_err(ReclaimFailure::Rtd)?;
        if K::RECORD_RECLAIM {
            deps.observer().rtd_drained();
        }
        // Keep owner and stage intact throughout both fallible operations.
        let (owner, services) = self.split();
        Ok(TeardownTxn::from_parts(
            owner,
            ResourcesReclaimed::new(services, rtd, outcome.certificate),
        ))
    }
}

impl<'runtime, A: Addin, K> TeardownTxn<'runtime, A, K, ResourcesReclaimed> {
    pub(crate) fn certify(
        self,
    ) -> crate::XllResult<super::finalization::CompletionPendingBinding<'runtime, A, K>>
    where
        K: TerminalCertificateKind,
    {
        let (owner, resources) = self.split();
        owner.certify::<K>(resources)
    }
}

/// Keeps the returned authority and claim alive while the caller inspects a
/// failed drain. Quarantine is published before this guard can notify waiters.
pub(crate) struct ExecutionDrainFailure<'runtime, A: Addin> {
    error: ExecutionDrainError,
    _owner: Box<RetainedTeardown<'runtime, A, AbortedBinding<'runtime, A>>>,
}

// Only terminal failures may retain either an inherited installation or its
// failed transition. No normal stage has this representation.
#[allow(
    dead_code,
    reason = "terminal alternatives retain binding authority until quarantine"
)]
enum AbortedBinding<'runtime, A: Addin> {
    Installed(Option<InstalledLifecycle<'runtime, A>>),
    Failed(LifecycleFailure<'runtime, A>),
}

impl<'runtime, A: Addin> RetainedTeardown<'runtime, A, Option<InstalledLifecycle<'runtime, A>>> {
    fn abort(self) -> RetainedTeardown<'runtime, A, AbortedBinding<'runtime, A>> {
        RetainedTeardown {
            _claim: self._claim,
            binding: AbortedBinding::Installed(self.binding),
        }
    }
}

impl<A: Addin> ExecutionDrainFailure<'_, A> {
    pub(crate) fn hazard(&self) -> crate::shutdown::UnloadHazard {
        self.error.hazard()
    }

    pub(crate) fn into_error(self) -> crate::XllError {
        self.error.into_error()
    }
}

impl<'runtime, A: Addin, K> RemovalOwner<'runtime, A, K> {
    pub(crate) fn drain(
        self,
    ) -> Result<TeardownTxn<'runtime, A, K, ExecutionDrained>, ExecutionDrainFailure<'runtime, A>>
    {
        self.drain_inner(None)
    }
    fn drain_inner(
        self,
        request: Option<RollbackRequest<'runtime, A>>,
    ) -> Result<TeardownTxn<'runtime, A, K, ExecutionDrained>, ExecutionDrainFailure<'runtime, A>>
    {
        let (deps, claim, module) = self.into_parts();
        // Reverse local drop order on pre-drain unwind: quarantine before
        // releasing an inherited STA binding.
        let mut binding = None;
        let mut preparing = PreparingTeardown {
            deps,
            claim: Some(claim),
        };
        if let Some(request) = request {
            match request.inherit(deps) {
                Ok(inherited) => binding = inherited,
                Err(failure) => {
                    let error = failure.error().clone();
                    let owner = preparing.retain(
                        crate::module_runtime::ModuleCleanupAuthority::Closing(module),
                        AbortedBinding::Failed(failure),
                    );
                    return Err(ExecutionDrainFailure {
                        error: ExecutionDrainError::Preparation(error),
                        _owner: Box::new(owner),
                    });
                }
            }
        }
        // Deferred RTD disconnect jobs can retain an export permit while
        // waiting for a shared source. Signal before this outer barrier.
        deps.request_subscription_stop_before_export_drain();
        let module = match module.seal_and_drain() {
            Ok(module) => module,
            Err((error, closing)) => {
                let owner = preparing.retain(
                    crate::module_runtime::ModuleCleanupAuthority::RetainedUndrained(closing),
                    AbortedBinding::Installed(binding),
                );
                return Err(ExecutionDrainFailure {
                    error: ExecutionDrainError::Preparation(error),
                    _owner: Box::new(owner),
                });
            }
        };
        let mut owner = StoppingOwner {
            core: StoppingCore {
                deps,
                authority: Some(StoppingAuthority {
                    claim: preparing.take(),
                    module,
                }),
                returns: None,
            },
            binding,
        };
        deps.observer().calls_drained();
        match ReturnsQuiescent::wait(deps, &owner.core.authority().claim) {
            Ok(returns) => {
                owner.core.returns = Some(returns);
                deps.observer().returns_drained();
                Ok(TeardownTxn::from_parts(owner, ExecutionDrained(())))
            }
            Err(error) => Err(ExecutionDrainFailure {
                error: ExecutionDrainError::ReturnQuiescence(error),
                _owner: Box::new(owner.retain().abort()),
            }),
        }
    }
}

impl<'runtime, A: Addin> RemovalOwner<'runtime, A, super::certificate::OpenRollback> {
    pub(in crate::runtime) fn drain_opening(
        self,
        request: RollbackRequest<'runtime, A>,
    ) -> Result<
        TeardownTxn<'runtime, A, super::certificate::OpenRollback, ExecutionDrained>,
        ExecutionDrainFailure<'runtime, A>,
    > {
        self.drain_inner(Some(request))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::addin_impl::{Addin, OpenContext, OpenResult};
    use crate::lifecycle::LifecyclePhase;
    use crate::runtime::{Runtime, transactions::RemovalSuccess};
    use std::cell::Cell;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Clone, Copy)]
    enum ReclaimFault {
        Diagnostics,
        Cookies,
        Debt,
        Residency,
    }

    std::thread_local! {
        static RECLAIM_FAULT: std::cell::Cell<Option<ReclaimFault>> = const { std::cell::Cell::new(None) };
        static PANIC_TEARDOWN_SPLIT: Cell<bool> = const { Cell::new(false) };
    }

    pub(super) fn check_teardown_split_fault() {
        PANIC_TEARDOWN_SPLIT.with(|fault| {
            assert!(!fault.replace(false), "injected teardown transition unwind");
        });
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum TeardownStage {
        ExecutionDrained,
        ProducersStopped,
        CallbacksClosed,
        HostDetached,
        AddinQuiesced,
        ServicesSealed,
        ServicesCleaned,
        ServicesQuiescent,
        ResourcesReclaimed,
    }

    struct TeardownFault {
        stage: TeardownStage,
        during_split: bool,
        reached: Cell<bool>,
    }

    impl TeardownFault {
        fn advance<'runtime, K, S: BindingPhase, Next>(
            &self,
            stage: TeardownStage,
            teardown: TeardownTxn<'runtime, (), K, S>,
            advance: impl FnOnce(TeardownTxn<'runtime, (), K, S>) -> Next,
        ) -> Next {
            if self.stage == stage {
                self.reached.set(true);
                assert!(self.during_split, "injected pending teardown unwind");
                PANIC_TEARDOWN_SPLIT.with(|fault| assert!(!fault.replace(true)));
            }
            advance(teardown)
        }

        fn drive<'runtime, K: TerminalCertificateKind>(
            &self,
            teardown: TeardownTxn<'runtime, (), K, ExecutionDrained>,
            lifecycle: &crate::runtime::AddinLifecycleAccess<'_, ()>,
            quiesce: impl FnOnce(
                TeardownTxn<'runtime, (), K, HostDetached>,
            ) -> Result<
                TeardownTxn<'runtime, (), K, AddinQuiesced<'runtime, ()>>,
                AddinQuiesceFailure,
            >,
        ) {
            let teardown = self.advance(TeardownStage::ExecutionDrained, teardown, |teardown| {
                teardown
                    .stop_producers(|_| {})
                    .unwrap_or_else(|_| panic!("stop producers"))
            });
            let teardown = self.advance(
                TeardownStage::ProducersStopped,
                teardown,
                TeardownTxn::close_module_callbacks,
            );
            let teardown = self.advance(TeardownStage::CallbacksClosed, teardown, |teardown| {
                teardown
                    .detach_host()
                    .unwrap_or_else(|_| panic!("detach host"))
            });
            let teardown = self.advance(TeardownStage::HostDetached, teardown, |teardown| {
                quiesce(teardown).unwrap_or_else(|_| panic!("quiesce add-in"))
            });
            let teardown = self.advance(TeardownStage::AddinQuiesced, teardown, |teardown| {
                teardown
                    .seal_services()
                    .unwrap_or_else(|_| panic!("seal services"))
            });
            let teardown = self.advance(TeardownStage::ServicesSealed, teardown, |teardown| {
                teardown
                    .cleanup_addin(lifecycle, &mut crate::shutdown::CloseReport::default())
                    .unwrap_or_else(|_| panic!("clean up add-in"))
            });
            let teardown = self.advance(TeardownStage::ServicesCleaned, teardown, |teardown| {
                teardown
                    .finish_services()
                    .unwrap_or_else(|_| panic!("finish services"))
            });
            let teardown = self.advance(TeardownStage::ServicesQuiescent, teardown, |teardown| {
                teardown
                    .reclaim(|_| {})
                    .unwrap_or_else(|_| panic!("reclaim"))
            });
            let _completion =
                self.advance(TeardownStage::ResourcesReclaimed, teardown, |teardown| {
                    teardown.certify().unwrap_or_else(|_| panic!("certify"))
                });
        }
    }

    #[test]
    fn miri_every_teardown_stage_quarantines_on_unwind() {
        for stage in [
            TeardownStage::ExecutionDrained,
            TeardownStage::ProducersStopped,
            TeardownStage::CallbacksClosed,
            TeardownStage::HostDetached,
            TeardownStage::AddinQuiesced,
            TeardownStage::ServicesSealed,
            TeardownStage::ServicesCleaned,
            TeardownStage::ServicesQuiescent,
            TeardownStage::ResourcesReclaimed,
        ] {
            for during_split in [false, true] {
                for final_removal in [false, true] {
                    assert_teardown_unwind(stage, during_split, final_removal);
                }
            }
        }
    }

    fn assert_teardown_unwind(stage: TeardownStage, during_split: bool, final_removal: bool) {
        let fault = TeardownFault {
            stage,
            during_split,
            reached: Cell::new(false),
        };
        let runtime = Runtime::<()>::new();
        let opening = runtime.begin_open().unwrap();
        let generation = opening.attempt_id().into_runtime_generation();
        let opening = runtime.publish(opening, (), ());
        if final_removal {
            runtime.finish_open(opening, Vec::new()).unwrap();
        } else {
            assert!(opening.fail_for_test().requires_rollback());
        }
        let lifecycle = runtime.bind_addin_lifecycle().unwrap();
        let epoch = runtime.lifecycle.access().module_epoch_id().unwrap();

        let failed = catch_no_unwind(AssertUnwindSafe(|| {
            if final_removal {
                let owner = runtime.begin_final_removal().unwrap();
                fault.drive(
                    owner.drain().unwrap_or_else(|_| panic!("drain")),
                    &lifecycle,
                    |teardown| teardown.quiesce_addin(&runtime, &lifecycle),
                );
            } else {
                let crate::runtime::orchestration::OpenRollbackAcquisition::Acquired(owner) =
                    runtime.acquire_open_rollback()
                else {
                    panic!("rollback owner unavailable");
                };
                fault.drive(
                    owner.drain().unwrap_or_else(|_| panic!("drain")),
                    &lifecycle,
                    |teardown| teardown.quiesce_addin(&runtime, &lifecycle, Some(generation)),
                );
            }
        }));
        assert!(
            failed.is_err() && fault.reached.get(),
            "missing injected unwind at {stage:?}"
        );
        PANIC_TEARDOWN_SPLIT.with(|fault| assert!(!fault.replace(false)));

        let state = runtime.lifecycle.access();
        assert_eq!(state.phase(), LifecyclePhase::Quarantined);
        assert!(state.removal_attempt().is_none());
        assert_eq!(state.module_epoch_id(), Some(epoch));
        drop(state);
        assert_drained_epoch(&runtime, epoch);
        assert!(epoch.is_current());
        assert!(runtime.begin_open().is_err());
    }

    struct StageDropPanics<'runtime>(&'runtime Runtime<()>, &'runtime Cell<bool>);

    impl Drop for StageDropPanics<'_> {
        fn drop(&mut self) {
            assert!(self.0.lifecycle.access().removal_attempt().is_some());
            assert!(matches!(
                self.0.lifecycle_control().take_module_cleanup_authority(),
                crate::module_runtime::ModuleCleanupClaim::DeferredOwner
            ));
            self.1.set(true);
            panic!("injected stage destructor unwind");
        }
    }

    struct BindingReleaseProbe<'runtime, A: Addin> {
        deps: ShutdownDeps<'runtime, A>,
        phase_on_drop: &'runtime Cell<Option<LifecyclePhase>>,
    }

    impl<A: Addin> Drop for BindingReleaseProbe<'_, A> {
        fn drop(&mut self) {
            self.phase_on_drop
                .set(Some(self.deps.lifecycle().access().phase()));
        }
    }

    impl BindingPhase for StageDropPanics<'_> {
        type Binding<'runtime, A: Addin> = BindingReleaseProbe<'runtime, A>;
    }

    #[test]
    fn miri_stage_drop_panic_cannot_interrupt_owner_recovery() {
        let reached = Cell::new(false);
        let phase_on_drop = Cell::new(None);
        let runtime = Runtime::<()>::new();
        let opening = runtime.begin_open().unwrap();
        let opening = runtime.publish(opening, (), ());
        runtime.finish_open(opening, Vec::new()).unwrap();
        let epoch = runtime.lifecycle.access().module_epoch_id().unwrap();
        let teardown = runtime
            .begin_final_removal()
            .unwrap()
            .drain()
            .unwrap_or_else(|_| panic!("drain"));
        let (owner, _) = teardown.split();
        let probing: TeardownTxn<'_, _, super::super::certificate::FinalRemoval, _> =
            TeardownTxn::from_parts(
                StoppingOwner {
                    core: owner.core,
                    binding: BindingReleaseProbe {
                        deps: runtime.shutdown_deps(),
                        phase_on_drop: &phase_on_drop,
                    },
                },
                StageDropPanics(&runtime, &reached),
            );

        let failed = catch_no_unwind(AssertUnwindSafe(|| drop(probing)));
        assert!(failed.is_err() && reached.get());
        assert_eq!(phase_on_drop.get(), Some(LifecyclePhase::Quarantined));
        assert!(runtime.lifecycle.access().removal_attempt().is_none());
        assert_drained_epoch(&runtime, epoch);
    }

    fn assert_drained_epoch<A: Addin>(
        runtime: &Runtime<A>,
        epoch: crate::module_runtime::ModuleEpochId,
    ) {
        let crate::module_runtime::ModuleCleanupClaim::Ready(
            crate::module_runtime::ModuleCleanupReady::Drained(drained),
        ) = runtime.lifecycle_control().take_module_cleanup_authority()
        else {
            panic!("lost drained module authority");
        };
        assert_eq!(drained.id(), epoch);
        runtime
            .lifecycle_control()
            .retain_failed_module_cleanup(crate::module_runtime::ModuleCleanupAuthority::Drained(
                drained,
            ))
            .unwrap_or_else(|_| panic!("canonical authority return refused"));
    }

    pub(super) fn check_reclaim_fault(rtd: bool) -> Result<(), ReclaimFailure> {
        RECLAIM_FAULT.with(|fault| match fault.get() {
            Some(ReclaimFault::Diagnostics) if !rtd => {
                fault.set(None);
                Err(ReclaimFailure::Diagnostics(crate::XllError::Closing))
            }
            Some(point) if rtd => {
                fault.set(None);
                Err(ReclaimFailure::Rtd(
                    crate::excel_rtd_protocol::RtdQuiescenceError {
                        outstanding_git_cookies: usize::from(matches!(
                            point,
                            ReclaimFault::Cookies
                        )),
                        revocation_debt: usize::from(matches!(point, ReclaimFault::Debt)),
                        missing_module_residency: matches!(point, ReclaimFault::Residency),
                    },
                ))
            }
            _ => Ok(()),
        })
    }

    #[test]
    fn mismatched_resource_generations_return_module_authority() {
        invalid_resource_returns_module_authority(false);
    }

    #[test]
    fn reopened_return_epoch_cannot_become_a_terminal_proof() {
        invalid_resource_returns_module_authority(true);
    }

    fn invalid_resource_returns_module_authority(reopen_returns: bool) {
        let runtime = Runtime::<RollbackProbe>::new();
        let opening = runtime.begin_open().unwrap();
        let generation = opening.attempt_id().into_runtime_generation();
        let opening = runtime.publish_with_lifecycle(
            opening,
            ObservedState(Arc::new(AtomicUsize::new(0))),
            Failure::None,
            (),
        );
        runtime.finish_open(opening, Vec::new()).unwrap();
        let epoch = runtime.lifecycle.access().module_epoch_id().unwrap();
        let owner = runtime.begin_final_removal().unwrap();
        let teardown = owner
            .drain()
            .unwrap_or_else(|_| panic!("drain"))
            .stop_producers(|_| {})
            .unwrap_or_else(|_| panic!("stop"))
            .close_module_callbacks()
            .detach_host()
            .unwrap_or_else(|_| panic!("detach"));
        let lifecycle = runtime.bind_addin_lifecycle().unwrap();
        let teardown = teardown
            .quiesce_addin(&runtime, &lifecycle)
            .unwrap_or_else(|_| panic!("quiesce"))
            .seal_services()
            .unwrap_or_else(|_| panic!("seal"));
        let mut report = crate::shutdown::CloseReport::default();
        let teardown = teardown
            .cleanup_addin(&lifecycle, &mut report)
            .unwrap_or_else(|_| panic!("cleanup"))
            .finish_services()
            .unwrap_or_else(|_| panic!("finish"));
        let mut teardown = teardown
            .reclaim(|_| {})
            .unwrap_or_else(|_| panic!("reclaim"));
        if reopen_returns {
            // Even a re-sealed and idle tracker belongs to a later admission
            // epoch. The old drain cannot certify this teardown.
            runtime
                .shutdown_deps()
                .returns()
                .reopen_admission()
                .unwrap();
            runtime.shutdown_deps().returns().close_admission();
        } else {
            let other = crate::generation::RuntimeGeneration::new(generation.get() + 1).unwrap();
            teardown.stage.services.subscriptions_stopped =
                crate::excel_rtd_protocol::stopped_subscriptions(Some(other));
        }
        assert!(teardown.certify().is_err());
        assert_eq!(
            runtime.phase(),
            crate::lifecycle::LifecyclePhase::Quarantined
        );
        assert!(runtime.lifecycle.access().removal_attempt().is_none());
        let crate::module_runtime::ModuleCleanupClaim::Ready(
            crate::module_runtime::ModuleCleanupReady::Drained(drained),
        ) = runtime.lifecycle_control().take_module_cleanup_authority()
        else {
            panic!("lost module");
        };
        assert_eq!(drained.id(), epoch);
        runtime
            .lifecycle_control()
            .retain_failed_module_cleanup(crate::module_runtime::ModuleCleanupAuthority::Drained(
                drained,
            ))
            .unwrap_or_else(|_| panic!("canonical authority return refused"));
    }

    #[test]
    fn reclaim_failures_retain_generation_and_module_before_certification() {
        for point in [
            ReclaimFault::Diagnostics,
            ReclaimFault::Cookies,
            ReclaimFault::Debt,
            ReclaimFault::Residency,
        ] {
            let runtime = Runtime::<RollbackProbe>::new();
            let drops = Arc::new(AtomicUsize::new(0));
            let opening = runtime.begin_open().unwrap();
            let opening = runtime.publish_with_lifecycle(
                opening,
                ObservedState(Arc::clone(&drops)),
                Failure::None,
                (),
            );
            runtime.finish_open(opening, Vec::new()).unwrap();
            let owner = runtime.begin_final_removal().unwrap();
            let epoch = runtime.lifecycle.access().module_epoch_id().unwrap();
            let teardown = owner
                .drain()
                .unwrap_or_else(|_| panic!("drain"))
                .stop_producers(|_| {})
                .unwrap_or_else(|_| panic!("stop"))
                .close_module_callbacks()
                .detach_host()
                .unwrap_or_else(|_| panic!("detach"));
            let lifecycle = runtime.bind_addin_lifecycle().unwrap();
            let teardown = teardown
                .quiesce_addin(&runtime, &lifecycle)
                .unwrap_or_else(|_| panic!("quiesce"))
                .seal_services()
                .unwrap_or_else(|_| panic!("seal"));
            let mut report = crate::shutdown::CloseReport::default();
            let teardown = teardown
                .cleanup_addin(&lifecycle, &mut report)
                .unwrap_or_else(|_| panic!("cleanup"))
                .finish_services()
                .unwrap_or_else(|_| panic!("finish"));
            RECLAIM_FAULT.with(|fault| fault.set(Some(point)));
            let Err(failure) = teardown.reclaim(|_| {}) else {
                panic!("fault did not fire");
            };
            let (hazard, boundary, error) =
                failure.into_diagnostic(ShutdownDiagnosticContext::Removal);
            assert_eq!(
                hazard,
                match point {
                    ReclaimFault::Diagnostics =>
                        crate::shutdown::UnloadHazard::DiagnosticWorkerStillRunning,
                    ReclaimFault::Cookies =>
                        crate::shutdown::UnloadHazard::RtdGitCallbackStillRegistered,
                    ReclaimFault::Debt => crate::shutdown::UnloadHazard::RtdGitRevocationDebt,
                    ReclaimFault::Residency =>
                        crate::shutdown::UnloadHazard::RtdComObjectsWithoutResidency,
                }
            );
            let (expected_boundary, expected_id) = match point {
                ReclaimFault::Diagnostics => ("xlAutoRemove diagnostic refinement", None),
                ReclaimFault::Cookies | ReclaimFault::Debt => (
                    "xlAutoRemove RTD logical quiescence",
                    Some(crate::diagnostics::catalog::RTD_GIT_QUIESCENCE),
                ),
                ReclaimFault::Residency => (
                    "xlAutoRemove RTD logical quiescence",
                    Some(crate::diagnostics::catalog::MODULE_RESIDENCY),
                ),
            };
            assert_eq!(boundary, expected_boundary);
            match expected_id {
                Some(expected) => assert!(matches!(
                    error,
                    crate::XllError::Internal { diagnostic_id, .. } if diagnostic_id == expected
                )),
                None => assert!(matches!(error, crate::XllError::Closing)),
            }
            RECLAIM_FAULT.with(|fault| assert!(fault.get().is_none()));
            assert_eq!(
                runtime.phase(),
                crate::lifecycle::LifecyclePhase::Quarantined
            );
            assert!(runtime.lifecycle.access().removal_attempt().is_none());
            // Addin cleanup already reclaimed the shared state, while terminal
            // canonical retirement and module authority remain recoverable.
            assert_eq!(drops.load(Ordering::SeqCst), 1);
            // Only transport-enabled builds have the RTD publication flag;
            // feature-off builds project just the already-drained ingress.
            #[cfg(any(feature = "rtd", feature = "handles"))]
            assert!(!crate::excel_rtd_protocol::logical_quiescence_certified());
            let crate::module_runtime::ModuleCleanupClaim::Ready(
                crate::module_runtime::ModuleCleanupReady::Drained(drained),
            ) = runtime.lifecycle_control().take_module_cleanup_authority()
            else {
                panic!("lost module");
            };
            assert_eq!(drained.id(), epoch);
            runtime
                .lifecycle_control()
                .retain_failed_module_cleanup(
                    crate::module_runtime::ModuleCleanupAuthority::Drained(drained),
                )
                .unwrap_or_else(|_| panic!("canonical authority return refused"));
        }
    }

    #[derive(Clone, Copy, Eq, PartialEq)]
    pub(super) enum ServiceSealPoint {
        HandleTopics = 1,
        Handles = 2,
    }

    std::thread_local! {
        static SERVICE_SEAL_FAULT: std::cell::Cell<Option<ServiceSealPoint>> = const { std::cell::Cell::new(None) };
        static SERVICE_SEAL_ATTEMPTS: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
    }

    /// Thread-local injection at the actual service operations. The public
    /// transition keeps its production signature and recovery implementation.
    pub(super) struct ServiceSealFault;

    impl ServiceSealFault {
        fn at(point: ServiceSealPoint) -> Self {
            SERVICE_SEAL_FAULT.with(|fault| assert!(fault.replace(Some(point)).is_none()));
            SERVICE_SEAL_ATTEMPTS.with(|attempts| attempts.set(0));
            Self
        }

        pub(super) fn check(point: ServiceSealPoint) -> crate::XllResult<()> {
            SERVICE_SEAL_ATTEMPTS.with(|attempts| attempts.set(attempts.get() | point as u8));
            SERVICE_SEAL_FAULT.with(|fault| {
                if fault.get() == Some(point) {
                    fault.set(None);
                    Err(crate::XllError::Closing)
                } else {
                    Ok(())
                }
            })
        }
    }

    impl Drop for ServiceSealFault {
        fn drop(&mut self) {
            SERVICE_SEAL_FAULT.with(|fault| fault.set(None));
        }
    }

    #[derive(Clone, Copy, Eq, PartialEq)]
    enum Failure {
        None,
        Quiesce,
        Cleanup,
    }

    struct ObservedState(Arc<AtomicUsize>);

    impl Drop for ObservedState {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    struct RollbackProbe;

    impl Addin for RollbackProbe {
        #[cfg(feature = "rtd")]
        type RtdSubscription = ();
        #[cfg(feature = "rtd")]
        type RtdSources = crate::DynamicRtdSources<Self::RtdSubscription>;
        type SharedState = ObservedState;
        type LifecycleState = Failure;
        type Layers = ();
        type Error = crate::XllError;
        #[cfg(feature = "async")]
        type AsyncExecutor = crate::NoAsyncExecutor;

        fn open(_: &OpenContext<Self>) -> OpenResult<Self> {
            unreachable!("test stages state directly")
        }

        fn quiesce(_: &mut ObservedState, failure: &mut Failure) -> Result<(), Self::Error> {
            if *failure == Failure::Quiesce {
                Err(crate::XllError::Closing)
            } else {
                Ok(())
            }
        }

        fn cleanup(failure: &mut Failure, _: &mut crate::shutdown::CleanupReporter<'_>) {
            assert!(*failure != Failure::Cleanup, "injected cleanup panic");
        }
    }

    fn assert_service_seal_failure<'runtime, K>(
        runtime: &'runtime Runtime<RollbackProbe>,
        teardown: TeardownTxn<'runtime, RollbackProbe, K, AddinQuiesced<'runtime, RollbackProbe>>,
        generation: crate::generation::RuntimeGeneration,
        drops: &AtomicUsize,
        point: ServiceSealPoint,
        context: ShutdownDiagnosticContext,
    ) {
        let epoch = runtime.lifecycle.access().module_epoch_id().unwrap();
        #[cfg(feature = "handles")]
        let service_was_ready = runtime.with_formula_handle_service(|_| ()).is_ok();
        let _fault = ServiceSealFault::at(point);
        let Err(failure) = teardown.seal_services() else {
            panic!("injected service seal failure succeeded");
        };
        let expected_hazard = match point {
            ServiceSealPoint::HandleTopics => {
                crate::shutdown::UnloadHazard::RtdGitCallbackStillRegistered
            }
            ServiceSealPoint::Handles => crate::shutdown::UnloadHazard::HandleStoreNotQuiescent,
        };
        let expected_boundary = match (&context, point) {
            (ShutdownDiagnosticContext::OpenRollback, ServiceSealPoint::HandleTopics) => {
                "xlAutoOpen RTD rollback"
            }
            (ShutdownDiagnosticContext::OpenRollback, ServiceSealPoint::Handles) => {
                "xlAutoOpen handle rollback"
            }
            (ShutdownDiagnosticContext::Removal, ServiceSealPoint::HandleTopics) => {
                "xlAutoRemove RTD shutdown"
            }
            (ShutdownDiagnosticContext::Removal, ServiceSealPoint::Handles) => {
                "xlAutoRemove handle table shutdown"
            }
        };
        let (hazard, boundary, error) = failure.into_diagnostic(context);
        assert_eq!(hazard, expected_hazard);
        assert_eq!(boundary, expected_boundary);
        assert!(matches!(error, crate::XllError::Closing));
        SERVICE_SEAL_FAULT.with(|fault| assert!(fault.get().is_none()));
        SERVICE_SEAL_ATTEMPTS.with(|attempts| {
            assert_eq!(
                attempts.get(),
                match point {
                    ServiceSealPoint::HandleTopics => 1,
                    ServiceSealPoint::Handles => 3,
                }
            );
        });
        // Both failure sites preserve the published service, the quiesced
        // shared state and the progressed module authority. The normal Drop
        // path quarantines before releasing the cleanup claim.
        #[cfg(feature = "handles")]
        assert_eq!(
            runtime.with_formula_handle_service(|_| ()).is_ok(),
            service_was_ready,
        );
        assert_eq!(
            runtime.phase(),
            crate::lifecycle::LifecyclePhase::Quarantined
        );
        assert!(runtime.lifecycle.access().removal_attempt().is_none());
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        assert!(runtime.quarantine_snapshot().contains(&(
            Some(generation),
            crate::runtime_components::QuarantineReason::TeardownIncomplete,
        )));
        let crate::module_runtime::ModuleCleanupClaim::Ready(
            crate::module_runtime::ModuleCleanupReady::Drained(drained),
        ) = runtime.lifecycle_control().take_module_cleanup_authority()
        else {
            panic!("service seal failure did not return drained module authority");
        };
        assert_eq!(drained.id(), epoch);
        runtime
            .lifecycle_control()
            .retain_failed_module_cleanup(crate::module_runtime::ModuleCleanupAuthority::Drained(
                drained,
            ))
            .unwrap_or_else(|_| panic!("canonical authority return refused"));
        assert!(runtime.begin_final_removal().is_none());
    }

    #[test]
    fn service_seal_failures_preserve_authority_and_quarantine_for_both_purposes() {
        for rollback in [false, true] {
            for point in [ServiceSealPoint::HandleTopics, ServiceSealPoint::Handles] {
                let runtime = Runtime::<RollbackProbe>::new();
                let drops = Arc::new(AtomicUsize::new(0));
                let opening = runtime.begin_open().unwrap();
                let generation = opening.attempt_id().into_runtime_generation();
                let opening = runtime.publish_with_lifecycle(
                    opening,
                    ObservedState(Arc::clone(&drops)),
                    Failure::None,
                    (),
                );
                if rollback {
                    assert!(opening.fail_for_test().requires_rollback());
                    let crate::runtime::orchestration::OpenRollbackAcquisition::Acquired(owner) =
                        runtime.acquire_open_rollback()
                    else {
                        panic!("rollback cleanup owner unavailable");
                    };
                    let teardown = owner.drain().unwrap_or_else(|_| panic!("execution drain"));
                    let teardown = teardown
                        .stop_producers(|_| {})
                        .unwrap_or_else(|_| panic!("producer stop"))
                        .close_module_callbacks()
                        .detach_host()
                        .unwrap_or_else(|_| panic!("host detach"));
                    let lifecycle = runtime.bind_addin_lifecycle().unwrap();
                    let teardown = teardown
                        .quiesce_addin(&runtime, &lifecycle, Some(generation))
                        .unwrap_or_else(|_| panic!("addin quiesce"));
                    assert_service_seal_failure(
                        &runtime,
                        teardown,
                        generation,
                        &drops,
                        point,
                        ShutdownDiagnosticContext::OpenRollback,
                    );
                } else {
                    runtime.finish_open(opening, Vec::new()).unwrap();
                    let owner = runtime.begin_final_removal().unwrap();
                    let teardown = owner.drain().unwrap_or_else(|_| panic!("execution drain"));
                    let teardown = teardown
                        .stop_producers(|_| {})
                        .unwrap_or_else(|_| panic!("producer stop"))
                        .close_module_callbacks()
                        .detach_host()
                        .unwrap_or_else(|_| panic!("host detach"));
                    let lifecycle = runtime.bind_addin_lifecycle().unwrap();
                    let teardown = teardown
                        .quiesce_addin(&runtime, &lifecycle)
                        .unwrap_or_else(|_| panic!("addin quiesce"));
                    #[cfg(feature = "handles")]
                    runtime.with_formula_handle_service(|_| ()).unwrap();
                    assert_service_seal_failure(
                        &runtime,
                        teardown,
                        generation,
                        &drops,
                        point,
                        ShutdownDiagnosticContext::Removal,
                    );
                }
            }
        }
    }

    #[cfg(feature = "handles")]
    #[test]
    fn handle_disposal_panic_is_reported_without_skipping_addin_cleanup() {
        struct PanicOnDrop;
        impl crate::handle::ExcelHandleObject for PanicOnDrop {}
        impl Drop for PanicOnDrop {
            fn drop(&mut self) {
                panic!("injected handle destructor panic");
            }
        }

        let runtime = Runtime::<RollbackProbe>::new();
        let drops = Arc::new(AtomicUsize::new(0));
        let opening = runtime.begin_open().unwrap();
        let opening = runtime.publish_with_lifecycle(
            opening,
            ObservedState(Arc::clone(&drops)),
            Failure::None,
            (),
        );
        runtime.finish_open(opening, Vec::new()).unwrap();
        runtime
            .with_formula_handle_service(|service| {
                service.prepare(crate::handle::test_topic_key("cleanup-panic"), || {
                    Ok(PanicOnDrop)
                })
            })
            .unwrap()
            .unwrap();
        let teardown = runtime
            .begin_final_removal()
            .unwrap()
            .drain()
            .unwrap_or_else(|_| panic!("drain"))
            .stop_producers(|_| {})
            .unwrap_or_else(|_| panic!("producers"))
            .close_module_callbacks()
            .detach_host()
            .unwrap_or_else(|_| panic!("host detach"));
        let lifecycle = runtime.bind_addin_lifecycle().unwrap();
        let teardown = teardown
            .quiesce_addin(&runtime, &lifecycle)
            .unwrap_or_else(|_| panic!("quiesce"))
            .seal_services()
            .unwrap_or_else(|_| panic!("disposal panic must not fail seal"));
        let mut report = crate::shutdown::CloseReport::default();
        let teardown = teardown.cleanup_addin(&lifecycle, &mut report).unwrap();
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        assert_eq!(report.issues().len(), 1);
        assert_eq!(report.issues()[0].component, "handle object disposal");
        assert_eq!(
            report.issues()[0].kind,
            crate::shutdown::CleanupIssueKind::DisposalPanicked
        );
        let teardown = teardown.finish_services().unwrap();
        // This fixture stops before global diagnostics/reclamation; its guard
        // retains the finished resources through the normal fallback path.
        drop(teardown);
    }

    #[test]
    fn rollback_quarantine_keeps_failed_attempt_generation() {
        for previously_committed in [false, true] {
            for failure in [Failure::Quiesce, Failure::Cleanup] {
                let runtime = Runtime::<RollbackProbe>::new();
                if previously_committed {
                    let opening = runtime.begin_open().unwrap();
                    let opening = runtime.publish_with_lifecycle(
                        opening,
                        ObservedState(Arc::new(AtomicUsize::new(0))),
                        Failure::None,
                        (),
                    );
                    runtime.finish_open(opening, Vec::new()).unwrap();
                    let lifecycle = runtime.bind_addin_lifecycle().unwrap();
                    let closed = crate::runtime::transactions::remove_addin_inner(
                        &runtime,
                        &lifecycle,
                        &mut crate::host_callback::HostCallbackSession::new(),
                    );
                    assert!(matches!(closed, RemovalSuccess::Closed { .. }));
                    drop(closed);
                }

                let drops = Arc::new(AtomicUsize::new(0));
                let opening = runtime.begin_open().unwrap();
                let failed_generation = opening.attempt_id().into_runtime_generation();
                let opening = runtime.publish_with_lifecycle(
                    opening,
                    ObservedState(Arc::clone(&drops)),
                    failure,
                    (),
                );
                assert!(opening.fail_for_test().requires_rollback());
                assert_ne!(runtime.protocol_generation(), Some(failed_generation));
                let lifecycle = runtime.bind_addin_lifecycle().unwrap();
                let outcome = crate::runtime::rollback::rollback_open(
                    &runtime,
                    &lifecycle,
                    &mut crate::host_callback::HostCallbackSession::new(),
                    Some(failed_generation),
                );
                assert!(!outcome.is_finalized());
                let expected_reason = if failure == Failure::Quiesce {
                    crate::runtime_components::QuarantineReason::AddinQuiesceFailed
                } else {
                    crate::runtime_components::QuarantineReason::AddinCleanupPanicked
                };
                let retained = runtime.quarantine_snapshot();
                assert!(retained.contains(&(Some(failed_generation), expected_reason)));
                assert!(
                    retained
                        .iter()
                        .all(|(generation, _)| *generation == Some(failed_generation))
                );
                assert_eq!(drops.load(Ordering::SeqCst), 0);
            }
        }
    }

    #[test]
    fn quiesce_validates_runtime_and_lifecycle_before_extracting_generation() {
        let runtime = Runtime::<RollbackProbe>::new();
        let other = Runtime::<RollbackProbe>::new();
        let lifecycle = runtime.bind_addin_lifecycle().unwrap();
        let other_lifecycle = other.bind_addin_lifecycle().unwrap();
        for (policy_runtime, access) in [(&other, &lifecycle), (&runtime, &other_lifecycle)] {
            let mut extracted = false;
            let result = QuiescedAddin::quiesce(
                runtime.shutdown_deps(),
                policy_runtime,
                access,
                None,
                || {
                    extracted = true;
                    None
                },
            );
            assert!(matches!(result, Err(AddinQuiesceFailure::Lifecycle(_))));
            assert!(!extracted);
        }
    }
}

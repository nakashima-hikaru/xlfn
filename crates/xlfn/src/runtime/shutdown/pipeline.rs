//! Shared teardown stages for rollback and terminal removal.
//!
//! The two boundaries keep distinct failure policies and terminal certificates,
//! while consuming the same ordered execution, producer, service, add-in, and
//! resource-reclamation stages. Both close export admission and drain returns
//! before stopping producers, then finish add-in/service cleanup and diagnostics
//! before waiting for RTD module quiescence.

use super::certificate::TerminalCertificateKind;
use super::owner::{RemovalClaimGuard, RemovalOwner};
use crate::addin::Addin;
use crate::panic_boundary::catch_no_unwind;
use crate::runtime::capabilities::ShutdownDeps;
use std::marker::PhantomData;
use std::panic::AssertUnwindSafe;

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
            return Err(AddinQuiesceFailure::Lifecycle(crate::XllError::Internal {
                diagnostic_id: crate::diagnostics::id::DiagnosticId::CLOSE_RUNTIME,
            }));
        }
        let lifecycle_present = deps.has_addin_lifecycle(lifecycle).map_err(|error| {
            AddinQuiesceFailure::Lifecycle(crate::lifecycle::lifecycle_access_error(error))
        })?;
        let Some(generation) = take_generation() else {
            if lifecycle_present {
                return Err(AddinQuiesceFailure::Lifecycle(crate::XllError::Internal {
                    diagnostic_id: crate::diagnostics::id::DiagnosticId::LIFECYCLE_SLOT,
                }));
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
                drop(generation.layers);
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
                drop(layers);
                Ok(addin)
            }
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
            return Err(crate::XllError::Panic);
        }

        let lifecycle_dropped = match self.deps.take_addin_lifecycle(lifecycle) {
            Ok(lifecycle_state) => {
                if catch_no_unwind(AssertUnwindSafe(|| drop(lifecycle_state))).is_err() {
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
        let shared_state_dropped = catch_no_unwind(AssertUnwindSafe(|| drop(shared_state))).is_ok();
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

/// The producer stage owns the execution-drain witness while async work and
/// subscription producers are stopped. Keeping these certificates together
/// prevents one pipeline from accidentally assembling a terminal proof with
/// only part of the producer shutdown sequence completed.
pub(crate) struct ProducersStopped {
    execution: ExecutionDrained,
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
        crate::XllError::Internal {
            diagnostic_id: crate::diagnostics::id::DiagnosticId::REGISTRATION_UNKNOWN,
        }
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

impl ServiceSealFailure {
    pub(crate) fn hazard(&self) -> crate::shutdown::UnloadHazard {
        match self {
            Self::HandleTopics(_) => crate::shutdown::UnloadHazard::RtdGitCallbackStillRegistered,
            Self::Handles(_) => crate::shutdown::UnloadHazard::HandleStoreNotQuiescent,
        }
    }

    pub(crate) fn into_error(self) -> crate::XllError {
        match self {
            Self::HandleTopics(error) | Self::Handles(error) => error,
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
    execution: ExecutionDrained,
    async_stopped: crate::shutdown::AsyncStopped,
    sealed: crate::runtime_components::SealedGenerationServices,
    addin: QuiescedAddin<'runtime, A>,
    host_callbacks: crate::shutdown::GenerationCallbacksDetached,
}

pub(crate) struct ServicesCleaned {
    execution: ExecutionDrained,
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
    module: crate::module_runtime::ModuleExportsDrained,
    returns: crate::shutdown::ReturnsQuiescent,
    async_stopped: crate::shutdown::AsyncStopped,
    subscriptions_stopped: crate::shutdown::SubscriptionsStopped,
    handles: crate::shutdown::HandlesQuiescent,
    addin: crate::shutdown::AddinQuiesced,
    generation: crate::shutdown::GenerationReclaimed,
    host_callbacks: crate::shutdown::GenerationCallbacksDetached,
}

/// Owns every resource certificate after producers have stopped. The only
/// operation that can expose the aggregate proof is `into_proof`, so callers
/// cannot accidentally certify a partially assembled terminal transition.
pub(crate) struct ResourcesReclaimed {
    services: ServicesQuiescent,
    rtd: crate::excel_rtd_protocol::RtdQuiescent,
    diagnostics: crate::diagnostics::DiagnosticsStopped,
}

impl ExecutionDrained {
    pub(crate) fn begin<A: Addin>(
        deps: ShutdownDeps<'_, A>,
        module: crate::module_runtime::ModuleClosing,
    ) -> Result<
        Self,
        (
            ExecutionDrainError,
            crate::module_runtime::ModuleCleanupAuthority,
        ),
    > {
        let module = match module.seal_and_drain() {
            Ok(module) => module,
            Err((error, closing)) => {
                return Err((
                    ExecutionDrainError::Preparation(error),
                    crate::module_runtime::ModuleCleanupAuthority::RetainedUndrained(closing),
                ));
            }
        };

        deps.observer().calls_drained();

        let returns = match deps.wait_for_return_quiescence() {
            Ok(returns) => returns,
            Err(error) => {
                return Err((
                    ExecutionDrainError::ReturnQuiescence(error),
                    crate::module_runtime::ModuleCleanupAuthority::Drained(module),
                ));
            }
        };

        deps.observer().returns_drained();

        Ok(Self { module, returns })
    }

    fn close_module_callbacks(&self) {
        self.module.close_callbacks();
    }

    pub(crate) fn stop_producers<A: Addin>(
        self,
        deps: ShutdownDeps<'_, A>,
        report_issue: impl FnMut(&crate::shutdown::CleanupIssue),
    ) -> Result<
        ProducersStopped,
        (
            ProducerStopFailure,
            crate::module_runtime::ModuleExportsDrained,
        ),
    > {
        #[cfg(feature = "async")]
        let mut report_issue = report_issue;
        #[cfg(not(feature = "async"))]
        let _ = report_issue;

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
                    return Err((
                        ProducerStopFailure {
                            hazard: crate::shutdown::UnloadHazard::AsyncExecutorStillRunning,
                            error,
                        },
                        self.module,
                    ));
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
        let async_stopped = crate::shutdown::AsyncStopped::issue();

        let subscriptions_stopped = match deps.close_subscriptions() {
            Ok(subscriptions_stopped) => subscriptions_stopped,
            Err(error) => {
                return Err((
                    ProducerStopFailure {
                        hazard: crate::shutdown::UnloadHazard::SubscriptionProducerStillRunning,
                        error,
                    },
                    self.module,
                ));
            }
        };

        Ok(ProducersStopped {
            execution: self,
            async_stopped,
            subscriptions_stopped,
        })
    }
}

impl<'runtime, A: Addin> AddinQuiesced<'runtime, A> {
    fn seal_services(
        self,
        deps: ShutdownDeps<'runtime, A>,
    ) -> Result<
        ServicesSealed<'runtime, A>,
        (crate::XllError, crate::module_runtime::ModuleExportsDrained),
    > {
        let Self { host, addin } = self;
        let HostDetached {
            producers,
            host_callbacks,
        } = host;
        let ProducersStopped {
            execution,
            async_stopped,
            subscriptions_stopped,
        } = producers;
        #[cfg(test)]
        if let Err(error) = tests::ServiceSealFault::check(tests::ServiceSealPoint::Handles) {
            return Err((error, execution.module));
        }
        match deps.seal_generation_services(subscriptions_stopped) {
            Ok(sealed) => Ok(ServicesSealed {
                execution,
                async_stopped,
                sealed,
                addin,
                host_callbacks,
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
            host_callbacks,
        } = self;
        match addin.cleanup(lifecycle, report) {
            Ok(addin) => Ok(ServicesCleaned {
                execution,
                async_stopped,
                sealed,
                addin,
                host_callbacks,
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
            host_callbacks,
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
                host_callbacks,
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
        crate::shutdown::GenerationCallbacksDetached,
    ) {
        (
            self.module,
            self.returns,
            self.async_stopped,
            self.subscriptions_stopped,
            self.handles,
            self.addin,
            self.generation,
            self.host_callbacks,
        )
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

    pub(crate) fn into_proof(self) -> crate::XllResult<QuiescenceProof> {
        let Self {
            services,
            rtd,
            diagnostics,
        } = self;
        let (
            module,
            returns,
            async_stopped,
            subscriptions_stopped,
            handles,
            addin,
            generation,
            host_callbacks,
        ) = services.into_parts();
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

impl ModuleAuthorityRecovery for CallbacksClosed {
    fn recover_module_authority(self) -> crate::module_runtime::ModuleCleanupAuthority {
        self.producers.recover_module_authority()
    }
}

impl ModuleAuthorityRecovery for HostDetached {
    fn recover_module_authority(self) -> crate::module_runtime::ModuleCleanupAuthority {
        self.producers.recover_module_authority()
    }
}

impl<A: Addin> ModuleAuthorityRecovery for AddinQuiesced<'_, A> {
    fn recover_module_authority(self) -> crate::module_runtime::ModuleCleanupAuthority {
        self.host.recover_module_authority()
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

/// A linear teardown transaction parameterized by its terminal policy and
/// current stage. Dropping an incomplete transaction quarantines the runtime;
/// successful transitions consume the old stage and return the next one.
pub(crate) struct TeardownTxn<'runtime, A: Addin, K, S: ModuleAuthorityRecovery> {
    owner: TeardownOwner<'runtime, A>,
    stage: Option<S>,
    _kind: PhantomData<K>,
}

struct TeardownOwner<'runtime, A: Addin> {
    owner: Option<RemovalClaimGuard<'runtime, A>>,
    deps: ShutdownDeps<'runtime, A>,
}

impl<'runtime, A: Addin> TeardownOwner<'runtime, A> {
    fn new(deps: ShutdownDeps<'runtime, A>, owner: RemovalClaimGuard<'runtime, A>) -> Self {
        Self {
            owner: Some(owner),
            deps,
        }
    }

    fn take(&mut self) -> RemovalClaimGuard<'runtime, A> {
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

impl<'runtime, A: Addin, K, S: ModuleAuthorityRecovery> TeardownTxn<'runtime, A, K, S> {
    fn from_parts(owner: TeardownOwner<'runtime, A>, stage: S) -> Self {
        Self {
            owner,
            stage: Some(stage),
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
}

impl<A: Addin, K, S: ModuleAuthorityRecovery> Drop for TeardownTxn<'_, A, K, S> {
    fn drop(&mut self) {
        let Some(stage) = self.stage.take() else {
            return;
        };
        let authority = stage.recover_module_authority();
        self.owner.return_module_authority(authority);
    }
}

impl<'runtime, A: Addin, K> TeardownTxn<'runtime, A, K, ExecutionDrained> {
    pub(crate) fn stop_producers(
        self,
        report_issue: impl FnMut(&crate::shutdown::CleanupIssue),
    ) -> Result<TeardownTxn<'runtime, A, K, ProducersStopped>, ProducerStopFailure> {
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
    pub(crate) fn close_module_callbacks(self) -> TeardownTxn<'runtime, A, K, CallbacksClosed> {
        let (owner, producers) = self.split();
        producers.execution.close_module_callbacks();
        TeardownTxn::from_parts(owner, CallbacksClosed { producers })
    }
}

impl<'runtime, A: Addin, K> TeardownTxn<'runtime, A, K, CallbacksClosed> {
    pub(crate) fn detach_host(
        self,
    ) -> Result<TeardownTxn<'runtime, A, K, HostDetached>, HostDetachFailure> {
        let deps = self.owner.deps();
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
                host_callbacks: crate::shutdown::GenerationCallbacksDetached::issue(),
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
            self.owner.deps(),
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
        let deps = self.owner.deps();
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
        self.owner
            .deps()
            .shutdown_handle_topics()
            .map_err(ServiceSealFailure::HandleTopics)?;
        let (mut owner, stage) = self.split();
        let deps = owner.deps();
        match stage.seal_services(deps) {
            Ok(stage) => Ok(TeardownTxn::from_parts(owner, stage)),
            Err((error, module)) => {
                owner.return_module_authority(
                    crate::module_runtime::ModuleCleanupAuthority::Drained(module),
                );
                drop(owner);
                Err(ServiceSealFailure::Handles(error))
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
        rtd: crate::excel_rtd_protocol::RtdQuiescent,
        diagnostics: crate::diagnostics::DiagnosticsStopped,
    ) -> TeardownTxn<'runtime, A, K, ResourcesReclaimed> {
        let (owner, services) = self.split();
        let stage = ResourcesReclaimed::new(services, rtd, diagnostics);
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

/// Keeps the returned authority and claim alive while the caller inspects a
/// failed drain. Quarantine is published before this guard can notify waiters.
pub(crate) struct ExecutionDrainFailure<'runtime, A: Addin> {
    error: ExecutionDrainError,
    _owner: Box<TeardownOwner<'runtime, A>>,
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
        let (deps, claim, module) = self.into_parts();
        let mut owner = TeardownOwner::new(deps, claim);
        match ExecutionDrained::begin(deps, module) {
            Ok(stage) => Ok(TeardownTxn::from_parts(owner, stage)),
            Err((error, module)) => {
                // Quarantine precedes returning an undrained authority and
                // releasing the claim, so a waiter cannot retry that drain.
                deps.quarantine_state();
                owner.return_module_authority(module);
                Err(ExecutionDrainFailure {
                    error,
                    _owner: Box::new(owner),
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::addin::{Addin, OpenContext, OpenResult};
    use crate::runtime::{Runtime, transactions::RemovalSuccess};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

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
        assert_eq!(failure.hazard(), expected_hazard);
        assert!(matches!(failure.into_error(), crate::XllError::Closing));
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
        runtime.lifecycle_control().retain_failed_module_cleanup(
            crate::module_runtime::ModuleCleanupAuthority::Drained(drained),
        );
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
                let mut opening = runtime.publish_with_lifecycle(
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
                    assert_service_seal_failure(&runtime, teardown, generation, &drops, point);
                } else {
                    runtime.finish_open(&mut opening, Vec::new()).unwrap();
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
                    assert_service_seal_failure(&runtime, teardown, generation, &drops, point);
                }
            }
        }
    }

    #[test]
    fn rollback_quarantine_keeps_failed_attempt_generation() {
        for previously_committed in [false, true] {
            for failure in [Failure::Quiesce, Failure::Cleanup] {
                let runtime = Runtime::<RollbackProbe>::new();
                if previously_committed {
                    let opening = runtime.begin_open().unwrap();
                    let mut opening = runtime.publish_with_lifecycle(
                        opening,
                        ObservedState(Arc::new(AtomicUsize::new(0))),
                        Failure::None,
                        (),
                    );
                    runtime.finish_open(&mut opening, Vec::new()).unwrap();
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

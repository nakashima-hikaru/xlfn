use crate::generation::{OpenAttemptId, RemovalAttemptId, RemovalEpoch, RuntimeGeneration};
use crate::ingress::AdmittedExport;
use crate::lifecycle::{HostLifecycleIntent, LifecyclePhase};
use crate::registration::RegistrationId;
use crate::{XllError, XllResult};
use std::collections::BTreeMap;
use std::marker::PhantomData;
use std::sync::Arc;
#[cfg(not(feature = "async"))]
use std::sync::atomic::Ordering;

#[cfg(feature = "async")]
use crate::runtime_components::RuntimeExecutors;
use crate::runtime_components::{
    GenerationAdmission, GenerationServices, HostLedger, LifecycleCoordinator, LifecycleCore,
    ModuleResidency, QuarantineReason, QuarantineVault, ReturnProtocol, SealedGenerationServices,
};
use crate::runtime_refinement::RuntimeRefinementHooks;
use xlfn_kernel::thread_affine::{
    ThreadAffineAccess, ThreadAffineError, ThreadAffineInstallError, ThreadAffineSlot,
};

#[cold]
fn opening_publication_lost() -> ! {
    #[cfg(not(test))]
    {
        tracing::error!("lifecycle opening publication lost its ingress linearization");
        std::process::abort();
    }
    #[cfg(test)]
    panic!("lifecycle opening publication lost its ingress linearization");
}

/// The published root of an open Add-in generation.
pub struct ExecutionGeneration<A: crate::Addin> {
    pub(crate) id: RuntimeGeneration,
    pub(crate) shared_state: A::SharedState,
    pub(crate) layers: A::Layers,
}

impl<A: crate::Addin> ExecutionGeneration<A> {
    pub(crate) const fn id(&self) -> RuntimeGeneration {
        self.id
    }
}

/// Unique Add-in state staged during `OPENING`.
pub struct OpeningGeneration<A: crate::Addin> {
    pub(crate) shared_state: A::SharedState,
    pub(crate) layers: A::Layers,
    pub(crate) init_config: crate::addin::RuntimeConfig,
}

impl<A: crate::Addin> OpeningGeneration<A> {
    #[must_use]
    pub(crate) fn into_parts(self) -> (A::SharedState, A::Layers, crate::addin::RuntimeConfig) {
        (self.shared_state, self.layers, self.init_config)
    }
}

/// Generation reclaimed during shutdown.
pub(crate) enum ShutdownGeneration<A: crate::Addin> {
    Opening(OpeningGeneration<A>),
    Open(Arc<ExecutionGeneration<A>>),
}

/// Explicit open-generation lifetime lease for call-scoped and asynchronous
/// UDF executions.
pub struct ExecutionLease<A: crate::Addin> {
    pub(crate) generation: Arc<ExecutionGeneration<A>>,
}

impl<A: crate::Addin> Clone for ExecutionLease<A> {
    fn clone(&self) -> Self {
        Self {
            generation: Arc::clone(&self.generation),
        }
    }
}

impl<A: crate::Addin> ExecutionLease<A> {
    #[must_use]
    pub fn state(&self) -> &A::SharedState {
        &self.generation.shared_state
    }

    #[must_use]
    pub fn layers(&self) -> &A::Layers {
        &self.generation.layers
    }
}

pub struct Runtime<A: crate::Addin> {
    pub(crate) lifecycle: LifecycleCoordinator<A>,
    pub(crate) addin_lifecycle: ThreadAffineSlot<A::LifecycleState>,
    pub(crate) host: HostLedger,
    pub(crate) return_protocol: ReturnProtocol,
    #[cfg(feature = "async")]
    pub(crate) executors: RuntimeExecutors,
    pub(crate) residency: ModuleResidency,
    pub(crate) quarantine: QuarantineVault<A>,
    pub(crate) refinement: RuntimeRefinementHooks,
}

pub(crate) type AddinLifecycleAccess<'runtime, A> =
    ThreadAffineAccess<'runtime, <A as crate::Addin>::LifecycleState>;

impl<A: crate::Addin> Runtime<A> {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            lifecycle: LifecycleCoordinator::new(),
            addin_lifecycle: ThreadAffineSlot::new(),
            host: HostLedger::new(),
            return_protocol: ReturnProtocol::new(),
            #[cfg(feature = "async")]
            executors: RuntimeExecutors::new(),
            residency: ModuleResidency::new(),
            quarantine: QuarantineVault::new(),
            refinement: RuntimeRefinementHooks::new(),
        }
    }

    #[cfg(any(test, feature = "refinement"))]
    pub(crate) fn refinement_hooks(&self) -> &RuntimeRefinementHooks {
        &self.refinement
    }

    #[cfg(any(test, feature = "refinement"))]
    pub(crate) fn composition_trace(&self) -> &crate::composition_refinement::CompositionTrace {
        self.refinement.composition_trace()
    }

    #[cfg(any(test, feature = "refinement"))]
    pub(crate) fn record_composition_event(
        &self,
        event: crate::composition_refinement::CompositionEvent,
    ) {
        self.composition_trace().record(event);
    }

    #[cfg(any(test, feature = "refinement"))]
    pub(crate) fn record_composition_begin_open(&self, sampled_epoch: u64, attempt: u64) {
        self.composition_trace().begin_open(sampled_epoch, attempt);
    }

    #[cfg(any(test, feature = "refinement"))]
    fn mark_composition_return_pending(&self) {
        self.composition_trace().mark_return_pending();
    }

    #[cfg(any(test, feature = "refinement"))]
    pub(crate) fn finish_composition_return(&self) {
        self.composition_trace().finish_return();
    }

    // This is called by the explicit removal boundary after the terminal
    // teardown has returned AlreadyClosed; begin_final_removal only records its
    // lifecycle request and does not claim the host call returned successfully.
    #[cfg(any(test, feature = "refinement"))]
    pub(crate) fn record_composition_already_closed_return(&self) {
        self.mark_composition_return_pending();
        self.finish_composition_return();
    }

    #[cfg(any(test, feature = "refinement"))]
    fn mark_composition_terminal_pending(&self) {
        self.composition_trace().mark_terminal_pending();
    }

    #[must_use]
    pub fn phase(&self) -> LifecyclePhase {
        self.lifecycle.observed_phase()
    }

    pub(crate) fn host_intent(&self) -> HostLifecycleIntent {
        self.lifecycle.lock().host_intent()
    }

    pub(crate) fn request_explicit_removal(&self) {
        self.lifecycle
            .set_host_intent(HostLifecycleIntent::ExplicitRemovalRequested);
    }

    pub(crate) fn complete_explicit_removal(&self) {
        self.lifecycle
            .set_host_intent(HostLifecycleIntent::ExplicitRemovalComplete);
    }

    pub(crate) fn clear_host_intent(&self) {
        self.lifecycle.set_host_intent(HostLifecycleIntent::None);
    }

    /// Acquires the DLL's self-reference before a generated `xlAutoOpen`
    /// enters the logical opening transaction.
    pub(crate) fn ensure_module_residency(&self, anchor: *const ()) -> XllResult<bool> {
        self.residency.ensure(anchor)
    }

    /// Releases the physical residency reference after explicit removal has
    /// completed. Ordinary host shutdown hints never call this method.
    pub(crate) fn release_module_residency(&self) -> XllResult<()> {
        self.residency.release()
    }

    pub(crate) fn module_residency_held(&self) -> bool {
        self.residency.is_held()
    }

    /// Publishes the fail-safe terminal state. A quarantined runtime rejects
    /// new opens and calls while retaining the module residency lease and any
    /// resources whose destruction was not proven safe.
    pub(crate) fn quarantine(&self) {
        let mut control = self.lifecycle.lock();
        self.return_protocol.close_admission();
        self.lifecycle.quarantine_core(&mut control);
        self.lifecycle.notify_all();
    }

    pub(crate) fn quarantine_shared_state(
        &self,
        generation: Option<RuntimeGeneration>,
        shared_state: A::SharedState,
        reason: QuarantineReason,
    ) {
        self.quarantine
            .retain_shared_state(generation, shared_state, reason);
    }

    pub(crate) fn quarantine_layers(
        &self,
        generation: Option<RuntimeGeneration>,
        layers: A::Layers,
        reason: QuarantineReason,
    ) {
        self.quarantine.retain_layers(generation, layers, reason);
    }

    pub(crate) fn quarantine_generation(
        &self,
        generation: Option<RuntimeGeneration>,
        root: ExecutionGeneration<A>,
        reason: QuarantineReason,
    ) {
        self.quarantine.retain_generation(generation, root, reason);
    }

    pub(crate) fn quarantine_shared_generation(
        &self,
        generation: Option<RuntimeGeneration>,
        root: Arc<ExecutionGeneration<A>>,
        reason: QuarantineReason,
    ) {
        self.quarantine
            .retain_shared_generation(generation, root, reason);
    }

    pub(crate) fn quarantine_opening_generation(
        &self,
        generation: Option<RuntimeGeneration>,
        opening: OpeningGeneration<A>,
        reason: QuarantineReason,
    ) {
        let OpeningGeneration {
            shared_state,
            layers,
            init_config: _,
        } = opening;
        if let Some(id) = generation {
            self.quarantine.retain_generation(
                Some(id),
                ExecutionGeneration {
                    id,
                    shared_state,
                    layers,
                },
                reason,
            );
        } else {
            self.quarantine
                .retain_shared_state(None, shared_state, reason);
            self.quarantine.retain_layers(None, layers, reason);
        }
    }

    pub(crate) fn quarantine_snapshot(&self) -> Vec<(Option<RuntimeGeneration>, QuarantineReason)> {
        self.quarantine.snapshot()
    }

    pub(crate) fn last_committed_generation(&self) -> Option<RuntimeGeneration> {
        self.lifecycle.lock().last_committed_generation()
    }

    pub(crate) fn protocol_generation(&self) -> Option<RuntimeGeneration> {
        self.lifecycle.lock().protocol_generation()
    }

    #[cfg(any(test, feature = "refinement"))]
    pub(crate) fn open_attempt(&self) -> Option<OpenAttemptId> {
        self.lifecycle.lock().canonical_state().open_attempt()
    }

    pub(crate) fn begin_open_if_epoch(
        &self,
        expected_removal_epoch: RemovalEpoch,
    ) -> XllResult<OpeningTxn<'_, A, OpenAttemptBegun>> {
        #[cfg(test)]
        let test_module_lease = crate::ingress::acquire_test_module_lease();
        let mut control = self.lifecycle.lock();
        if control.removal_epoch() != expected_removal_epoch.get()
            || control.canonical_state().phase() != LifecyclePhase::Closed
            || control.canonical_state().open_attempt().is_some()
            || control.removal_attempt().is_some()
        {
            return Err(XllError::Internal {
                diagnostic_id: crate::error::DiagnosticId::OPEN_PHASE,
            });
        }

        self.lifecycle.prepare_open(&mut control);
        let attempt_id = self.lifecycle.allocate_open_attempt(&mut control)?;
        self.return_protocol.reopen_admission()?;

        let module_opening = crate::module_runtime::global().begin_open();
        #[cfg(test)]
        {
            *self.lifecycle.test_module_lease.lock() = Some(test_module_lease);
        }
        self.lifecycle.begin_opening(&mut control, attempt_id);
        self.refinement
            .begin_open(self, expected_removal_epoch.get(), attempt_id);
        Ok(OpeningTxn {
            runtime: self,
            attempt_id,
            module_opening: Some(module_opening),
            _stage: PhantomData,
        })
    }

    #[cfg(test)]
    pub(crate) fn begin_open(&self) -> XllResult<OpeningTxn<'_, A, OpenAttemptBegun>> {
        self.begin_open_if_epoch(self.removal_epoch())
    }

    pub(crate) fn removal_epoch(&self) -> RemovalEpoch {
        RemovalEpoch::new(self.lifecycle.lock().removal_epoch())
    }

    #[cfg(any(test, feature = "bench-internals"))]
    pub(crate) fn publish(&self, state: A::SharedState, layers: A::Layers)
    where
        A::LifecycleState: Default,
    {
        self.publish_with_lifecycle(state, Default::default(), layers);
    }

    #[cfg(any(test, feature = "bench-internals"))]
    pub(crate) fn publish_with_lifecycle(
        &self,
        state: A::SharedState,
        lifecycle_state: A::LifecycleState,
        layers: A::Layers,
    ) {
        let access = self
            .bind_addin_lifecycle()
            .expect("test runtime binds its lifecycle thread");
        if self.with_addin_lifecycle(&access, |_| ()).is_err() {
            assert!(
                self.install_addin_lifecycle(&access, lifecycle_state)
                    .is_ok(),
                "test runtime has one lifecycle state"
            );
        }
        let mut control = self.lifecycle.lock();
        assert!(
            self.lifecycle
                .stage_opening_generation_locked(
                    &mut control,
                    OpeningGeneration {
                        shared_state: state,
                        layers,
                        init_config: crate::addin::RuntimeConfig::new(),
                    }
                )
                .is_ok()
        );
    }

    pub(crate) fn bind_addin_lifecycle(
        &self,
    ) -> Result<AddinLifecycleAccess<'_, A>, ThreadAffineError> {
        self.addin_lifecycle.bind_current()
    }

    pub(crate) fn install_addin_lifecycle(
        &self,
        access: &AddinLifecycleAccess<'_, A>,
        state: A::LifecycleState,
    ) -> Result<(), ThreadAffineInstallError<A::LifecycleState>> {
        self.addin_lifecycle.install(access, state)
    }

    pub(crate) fn with_addin_lifecycle<R>(
        &self,
        access: &AddinLifecycleAccess<'_, A>,
        operation: impl FnOnce(&mut A::LifecycleState) -> R,
    ) -> Result<R, ThreadAffineError> {
        self.addin_lifecycle.with_mut(access, operation)
    }

    pub(crate) fn has_addin_lifecycle(
        &self,
        access: &AddinLifecycleAccess<'_, A>,
    ) -> Result<bool, ThreadAffineError> {
        self.addin_lifecycle.has_value(access)
    }

    pub(crate) fn take_addin_lifecycle(
        &self,
        access: &AddinLifecycleAccess<'_, A>,
    ) -> Result<A::LifecycleState, ThreadAffineError> {
        self.addin_lifecycle.take(access)
    }

    pub(crate) fn release_empty_addin_lifecycle(
        &self,
        access: &AddinLifecycleAccess<'_, A>,
    ) -> Result<(), ThreadAffineError> {
        self.addin_lifecycle.release_empty_binding(access)
    }

    fn stage_opening_generation_for_attempt(
        &self,
        attempt_id: OpenAttemptId,
        opening: OpeningGeneration<A>,
    ) -> Result<(), (XllError, OpeningGeneration<A>)> {
        let mut control = self.lifecycle.lock();
        if control.canonical_state().open_attempt() != Some(attempt_id)
            || control.canonical_state().phase() != LifecyclePhase::Opening
        {
            return Err((XllError::Closing, opening));
        }
        self.lifecycle
            .stage_opening_generation_locked(&mut control, opening)
    }

    pub(crate) fn publish_opening_generation(
        &self,
        control: &mut LifecycleCore<A>,
        attempt_id: OpenAttemptId,
        module_epoch: crate::module_runtime::ModuleEpochLease,
    ) -> XllResult<()> {
        let generation = attempt_id.into_runtime_generation();
        let config = control.opening_config().ok_or(XllError::Internal {
            diagnostic_id: crate::error::DiagnosticId::OPEN_STATE,
        })?;
        let armed_services = GenerationServices::arm_generation(
            generation,
            config,
            crate::rtd::RtdSubscriptionHost::production(crate::module_runtime::ingress()),
        )?;
        let services = armed_services.commit();
        if let Err(failure) = self.lifecycle.publish_opening_generation_locked(
            control,
            generation,
            std::sync::Arc::clone(&services),
            module_epoch,
        ) {
            services.disarm_or_abort();
            if let Some(opening) = failure.opening {
                self.quarantine_opening_generation(
                    Some(generation),
                    opening,
                    QuarantineReason::OpenStateInvariant,
                );
            }
            return Err(failure.error);
        }
        Ok(())
    }

    #[cfg(test)]
    #[must_use]
    pub(crate) fn has_opening_generation(&self) -> bool {
        self.lifecycle.has_opening_generation()
    }

    #[cfg(test)]
    #[must_use]
    pub(crate) fn has_current_generation(&self) -> bool {
        self.lifecycle.has_current_generation()
    }

    #[cfg(any(test, feature = "bench-internals"))]
    pub(crate) fn arm_test_generation(&self) {
        let services = GenerationServices::arm_generation(
            crate::generation::RuntimeGeneration::new(1).expect("test generation is non-zero"),
            crate::addin::RuntimeConfig::new(),
            crate::rtd::RtdSubscriptionHost::detached(),
        )
        .expect("test runtime generation can be armed once")
        .commit();
        self.lifecycle.install_test_generation_services(services);
    }

    pub(crate) fn take_opening_for_rollback(&self) -> Option<OpeningGeneration<A>> {
        self.lifecycle.take_opening_for_rollback()
    }

    #[cfg(test)]
    pub(crate) fn take_current_generation(&self) -> Option<Arc<ExecutionGeneration<A>>> {
        self.lifecycle.take_current_generation()
    }

    pub(crate) fn take_generation_for_shutdown(&self) -> Option<ShutdownGeneration<A>> {
        self.lifecycle.take_generation_for_shutdown()
    }

    #[cfg(any(test, feature = "bench-internals"))]
    pub(crate) fn finish_open<Stage>(
        &self,
        attempt: &mut OpeningTxn<'_, A, Stage>,
        registrations: Vec<RegistrationId>,
    ) -> XllResult<()> {
        let mut journal = crate::registration::HostMutationJournal {
            pending_registrations: registrations
                .into_iter()
                .map(crate::registration::PendingRegistration::from)
                .collect(),
            ..Default::default()
        };
        attempt.commit_in_place(&mut journal)
    }

    fn finish_open_for_attempt(
        &self,
        attempt_id: OpenAttemptId,
        module_opening: crate::module_runtime::ModuleOpening,
        journal: &mut crate::registration::HostMutationJournal,
    ) -> XllResult<()> {
        let mut control = self.lifecycle.lock();
        if control.canonical_state().open_attempt() != Some(attempt_id) {
            return Err(XllError::Closing);
        }

        // Once this attempt owns the lifecycle slot, retain every host
        // registration even when a concurrent close has already won the phase
        // transition. The close owner needs those IDs to unregister the host
        // mutations before publishing Closed.
        let registration_ids = journal
            .pending_registrations
            .iter()
            .map(|entry| entry.registration)
            .collect::<Vec<_>>();
        self.clear_metadata_debt_for_registrations(&registration_ids);
        self.host.merge(std::mem::take(journal));
        let can_commit = control.canonical_state().phase() == LifecyclePhase::Opening;
        if can_commit {
            let ingress = crate::module_runtime::ingress();
            ingress
                .complete_open(|| {
                    self.publish_opening_generation(
                        &mut control,
                        attempt_id,
                        module_opening.commit(),
                    )?;
                    let generation = attempt_id.into_runtime_generation();
                    self.refinement.commit_open(self, attempt_id, || {
                        self.lifecycle.commit_open(&mut control, generation)?;
                        if control.canonical_state().phase() != LifecyclePhase::Open
                            || control.last_committed_generation() != Some(generation)
                            || control.canonical_state().open_attempt().is_some()
                        {
                            crate::lifecycle::fail_stop_invariant(
                                "xlAutoOpen commit postcondition",
                                &XllError::Internal {
                                    diagnostic_id: crate::error::DiagnosticId::OPEN_STATE,
                                },
                            );
                        }
                        Ok(())
                    })?;
                    Ok::<(), XllError>(())
                })
                .unwrap_or_else(|_| opening_publication_lost())?;
            self.lifecycle.notify_all();
            Ok(())
        } else {
            self.reject_open_attempt(&mut control, module_opening);
            self.lifecycle.notify_all();
            drop(control);
            self.refinement.reject_open(self, attempt_id);
            Err(XllError::Closing)
        }
    }

    pub(crate) fn retain_host_mutations(&self, journal: crate::registration::HostMutationJournal) {
        self.host.merge(journal);
    }

    pub(crate) fn registration_state_unknown(&self) -> bool {
        self.host.registration_state_unknown()
    }

    fn reject_open_attempt(
        &self,
        control: &mut LifecycleCore<A>,
        _module_opening: crate::module_runtime::ModuleOpening,
    ) {
        self.lifecycle.reject_open_attempt(control);
    }

    fn fail_and_record(
        &self,
        attempt_id: OpenAttemptId,
    ) -> crate::runtime_components::OpenFailureDisposition {
        let mut control = self.lifecycle.lock();
        if control.canonical_state().open_attempt() != Some(attempt_id) {
            return crate::runtime_components::OpenFailureDisposition::ClosingOwnsCleanup;
        }

        if control.canonical_state().phase() == LifecyclePhase::Opening {
            self.return_protocol.close_admission();
        }
        let disposition = self.lifecycle.record_open_failure(&mut control);
        self.lifecycle.notify_all();
        drop(control);
        self.refinement.fail_open(self, attempt_id);
        disposition
    }

    pub(crate) fn enter<'call>(
        &'call self,
        ingress: &'call AdmittedExport<'call>,
    ) -> XllResult<CallGuard<'call, A>> {
        ingress.with_linearization(|| {
            let admission = self.lifecycle.try_admit()?;
            #[cfg(any(test, feature = "refinement"))]
            self.refinement_hooks().call_entered(self);
            Ok(CallGuard {
                #[cfg(any(test, feature = "refinement"))]
                runtime: self,
                #[cfg(not(any(test, feature = "refinement")))]
                _runtime: std::marker::PhantomData,
                admission,
                _ingress: ingress,
            })
        })
    }

    #[cfg(test)]
    pub(crate) fn begin_close(&self) -> bool {
        let mut control = self.lifecycle.lock();
        crate::module_runtime::ingress().with_linearization(|| {
            if matches!(
                control.canonical_state().phase(),
                LifecyclePhase::Opening | LifecyclePhase::Open
            ) {
                self.return_protocol.close_admission();
                self.lifecycle.request_closing(&mut control);
                true
            } else {
                false
            }
        })
    }

    pub(crate) fn begin_final_removal(&self) -> Option<RemovalOwner<'_, A>> {
        let mut wait_guard = self.lifecycle.lock();
        // Every final-close invocation invalidates open operations that started
        // before it, including an operation that is between rollback recovery
        // and acquisition of its open-attempt token while the phase is Closed.
        // The removal-request epoch is deliberately not part of TerminalCertificate: a waiting
        // final-close caller may advance it while the active owner finishes.
        self.lifecycle.begin_removal_request(&mut wait_guard);
        self.return_protocol.close_admission();
        let mut request_recorded = false;
        loop {
            let decision = crate::module_runtime::ingress().with_linearization(|| {
                match wait_guard.canonical_state().phase() {
                    LifecyclePhase::Closed => {
                        // A cleanup owner publishes Closed before its guard leaves
                        // the callback stack. A concurrent explicit removal must
                        // not return until that owner has fully exited, because
                        // the host may immediately continue with residency release.
                        if wait_guard.removal_attempt().is_none() && self.returns_are_quiescent() {
                            self.refinement
                                .request_final_close(self, &mut request_recorded);
                            return Some(None);
                        }
                        if wait_guard.removal_attempt().is_none() {
                            self.lifecycle.request_closing(&mut wait_guard);
                        }
                    }
                    LifecyclePhase::Closing => {}
                    LifecyclePhase::Opening
                    | LifecyclePhase::Open
                    | LifecyclePhase::OpenRollbackPending => {
                        self.lifecycle.request_closing(&mut wait_guard);
                    }
                    LifecyclePhase::Quarantined => return Some(None),
                }

                if !request_recorded {
                    if !matches!(
                        wait_guard.canonical_state().phase(),
                        LifecyclePhase::Closed | LifecyclePhase::Closing
                    ) {
                        crate::lifecycle::fail_stop_invariant(
                            "xlAutoRemove close-request postcondition",
                            &XllError::Internal {
                                diagnostic_id: crate::error::DiagnosticId::CLOSE_WAIT,
                            },
                        );
                    }
                    self.refinement
                        .request_final_close(self, &mut request_recorded);
                }

                if wait_guard.canonical_state().phase() != LifecyclePhase::Closed
                    && wait_guard.canonical_state().open_attempt().is_none()
                    && let Some(attempt) = self.lifecycle.claim_removal_owner(&mut wait_guard)
                {
                    self.refinement.acquire_final_close_owner(self);
                    Some(Some(attempt))
                } else {
                    None
                }
            });
            match decision {
                Some(Some(attempt)) => {
                    return Some(RemovalOwner {
                        runtime: self,
                        attempt,
                    });
                }
                Some(None) => return None,
                None => self.lifecycle.wait(&mut wait_guard),
            }
        }
    }

    pub(crate) fn acquire_open_rollback(&self) -> Option<RemovalOwner<'_, A>> {
        let mut wait_guard = self.lifecycle.lock();
        loop {
            match wait_guard.canonical_state().phase() {
                LifecyclePhase::Closed => return None,
                LifecyclePhase::OpenRollbackPending => {}
                LifecyclePhase::Closing
                | LifecyclePhase::Opening
                | LifecyclePhase::Open
                | LifecyclePhase::Quarantined => {
                    return None;
                }
            }
            if let Some(attempt) = self.lifecycle.claim_removal_owner(&mut wait_guard) {
                self.refinement.acquire_open_rollback_owner(self);
                return Some(RemovalOwner {
                    runtime: self,
                    attempt,
                });
            }
            self.lifecycle.wait(&mut wait_guard);
        }
    }

    pub(crate) fn registrations(&self) -> Vec<crate::registration::PendingRegistration> {
        self.host.registrations_snapshot()
    }

    pub(crate) fn retain_failed_registrations(
        &self,
        failed: Vec<(crate::registration::PendingRegistration, XllError)>,
    ) {
        self.host
            .replace_registrations(failed.into_iter().map(|(entry, _)| entry).collect());
    }

    pub(crate) fn retain_metadata_debt(
        &self,
        metadata_debt: Vec<crate::registration::MetadataDebt>,
    ) {
        self.host.retain_metadata_debt(metadata_debt);
    }

    pub(crate) fn metadata_debt(
        &self,
    ) -> BTreeMap<crate::registration::ExcelNameKey, Vec<crate::registration::MetadataDebt>> {
        self.host.metadata_debt_snapshot()
    }

    pub(crate) fn clear_metadata_debt_for_registrations(&self, registrations: &[RegistrationId]) {
        self.host
            .clear_metadata_debt_for_registrations(registrations);
    }

    pub(crate) fn replace_metadata_debt(
        &self,
        debts: BTreeMap<crate::registration::ExcelNameKey, Vec<crate::registration::MetadataDebt>>,
    ) {
        self.host.replace_metadata_debt(debts);
    }

    pub(crate) fn has_metadata_debt(&self) -> bool {
        self.host.has_metadata_debt()
    }

    pub(crate) fn event_registrations(&self) -> Vec<crate::registration::EventRegistration> {
        self.host.event_registrations_snapshot()
    }

    pub(crate) fn host_callbacks_detached(&self) -> bool {
        self.host.callbacks_detached()
    }

    pub(crate) fn retain_failed_event_registrations(
        &self,
        failed: Vec<(crate::registration::EventRegistration, XllError)>,
    ) {
        self.host
            .replace_event_registrations(failed.into_iter().map(|(entry, _)| entry).collect());
    }

    #[inline]
    #[cfg_attr(
        not(test),
        allow(
            dead_code,
            reason = "Method used in test suite and internal diagnostics"
        )
    )]
    pub(crate) const fn return_tracker(&self) -> &crate::return_value::ReturnTracker {
        &self.return_protocol.returns
    }

    #[inline]
    pub(crate) fn enter_return_producer(
        &'static self,
    ) -> Option<crate::return_value::ReturnProducerGuard<'static>> {
        self.return_protocol.enter_producer()
    }

    #[inline]
    pub(crate) fn wait_for_returns(&self) {
        self.return_protocol.wait_for_returns();
    }

    #[inline]
    pub(crate) fn returns_are_quiescent(&self) -> bool {
        self.return_protocol.returns_are_quiescent()
    }

    #[inline]
    fn returns_closed_and_quiescent(&self) -> bool {
        self.return_protocol.returns_closed_and_quiescent()
    }

    #[cfg(test)]
    pub(crate) fn disable_ghost_for_test(&self) {
        self.refinement_hooks().disable_for_test();
    }

    #[cfg(any(test, feature = "refinement"))]
    pub(crate) fn record_returned_success(&self, witness: ClosedWitness) -> XllResult<()> {
        self.refinement_hooks()
            .record_returned_success(self, &witness)?;
        self.mark_composition_return_pending();
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn ghost_trace_json(&self) -> String {
        self.refinement_hooks()
            .ghost_handle()
            .trace_json()
            .expect("ghost trace serialization")
    }

    #[cfg(test)]
    pub(crate) fn composition_trace_json(&self) -> String {
        self.composition_trace()
            .trace_json()
            .expect("composition trace serialization")
    }
}

#[derive(Debug)]
#[allow(
    dead_code,
    reason = "linear proof tokens are consumed by terminal transitions"
)]
pub(crate) struct QuiescenceProof {
    pub(crate) exports: crate::ingress::ExportsDrained,
    pub(crate) rtd: crate::rtd::RtdQuiescent,
    pub(crate) host_callbacks: crate::shutdown::HostCallbacksDetached,
    pub(crate) async_stopped: crate::shutdown::AsyncStopped,
    pub(crate) subscriptions_stopped: crate::shutdown::SubscriptionsStopped,
    pub(crate) handle_store_quiescent: crate::shutdown::HandleStoreQuiescent,
    pub(crate) diagnostics_stopped: crate::diagnostics::DiagnosticsStopped,
    pub(crate) addin_quiesced: crate::shutdown::AddinQuiesced,
    pub(crate) generation_reclaimed: crate::shutdown::GenerationReclaimed,
}

pub(crate) struct FinalRemoval;
pub(crate) struct OpenRollback;

pub(crate) trait TerminalCertificateKind {
    fn accepts_phase(phase: LifecyclePhase) -> bool;
    fn requires_module_epoch() -> bool;
    fn error() -> XllError;
}

impl TerminalCertificateKind for FinalRemoval {
    fn accepts_phase(phase: LifecyclePhase) -> bool {
        phase == LifecyclePhase::Closing
    }

    fn requires_module_epoch() -> bool {
        true
    }

    fn error() -> XllError {
        XllError::Internal {
            diagnostic_id: crate::error::DiagnosticId::CLOSE_CERTIFICATE,
        }
    }
}

impl TerminalCertificateKind for OpenRollback {
    fn accepts_phase(phase: LifecyclePhase) -> bool {
        matches!(
            phase,
            LifecyclePhase::OpenRollbackPending | LifecyclePhase::Closing
        )
    }

    fn requires_module_epoch() -> bool {
        false
    }

    fn error() -> XllError {
        XllError::Internal {
            diagnostic_id: crate::error::DiagnosticId::OPEN_ROLLBACK_CERTIFICATE,
        }
    }
}

pub(crate) struct TerminalCertificate<'runtime, A: crate::Addin, K> {
    #[allow(
        dead_code,
        reason = "linear proof tokens are consumed by terminal transitions"
    )]
    pub(crate) proof: QuiescenceProof,
    #[cfg(any(test, feature = "refinement"))]
    pub(crate) composition_resources: crate::shutdown_refinement::GhostResources,
    pub(crate) owner: RemovalOwner<'runtime, A>,
    pub(crate) generation: Option<RuntimeGeneration>,
    pub(crate) module_epoch: Option<crate::module_runtime::ModuleEpochLease>,
    pub(crate) _kind: std::marker::PhantomData<K>,
}

#[derive(Debug)]
pub(crate) struct ClosedWitness {
    #[cfg(any(test, feature = "refinement"))]
    pub(crate) runtime_address: usize,
    #[cfg(any(test, feature = "refinement"))]
    pub(crate) generation: Option<RuntimeGeneration>,
}

#[cfg(any(test, feature = "refinement"))]
fn composition_resources_from_quiescence_proof(
    proof: &QuiescenceProof,
) -> crate::shutdown_refinement::GhostResources {
    // These linear tokens are the concrete proof that every resource family
    // represented by the abstract snapshot has drained. Keep this projection
    // at certificate issuance so finish events cannot observe a later ad-hoc
    // runtime snapshot.
    let _proofs = (
        &proof.exports,
        &proof.rtd,
        &proof.host_callbacks,
        &proof.async_stopped,
        &proof.subscriptions_stopped,
        &proof.handle_store_quiescent,
        &proof.diagnostics_stopped,
        &proof.addin_quiesced,
        &proof.generation_reclaimed,
    );
    crate::shutdown_refinement::GhostResources::quiescent_snapshot()
}

impl<'runtime, A: crate::Addin> RemovalOwner<'runtime, A> {
    /// Consume the affine removal owner and issue a certificate only for the
    /// runtime and attempt represented by that owner. On failure, return the
    /// owner with the error so the caller can retain the quarantine guard.
    pub(crate) fn certify<K: TerminalCertificateKind>(
        self,
        proof: QuiescenceProof,
    ) -> Result<TerminalCertificate<'runtime, A, K>, (XllError, Self)> {
        let runtime = self.runtime;
        let mut control = runtime.lifecycle.lock();
        let services = runtime
            .lifecycle
            .load_generation_services()
            .or_else(|| control.retiring_services().map(Arc::clone));
        let services_stopped = services.as_ref().is_none_or(|services| services.is_none());
        #[cfg(feature = "async")]
        let async_stopped = runtime.executors.async_manager.is_stopped();
        #[cfg(not(feature = "async"))]
        let async_stopped = true;
        let handles_match_generation = control
            .last_committed_generation()
            .is_none_or(|generation| proof.handle_store_quiescent.generation() == Some(generation));
        let services_owned = services_stopped || control.has_retirement();
        let module_epoch_present = control.has_module_epoch();
        let module_epoch_current = control.module_epoch_is_current();
        let module_epoch_required =
            K::requires_module_epoch() && control.last_committed_generation().is_some();

        let certified = K::accepts_phase(control.canonical_state().phase())
            && control.canonical_state().open_attempt().is_none()
            && control.removal_attempt() == Some(self.attempt)
            && runtime.returns_closed_and_quiescent()
            && async_stopped
            && services_stopped
            && !control.has_opening_generation()
            && !control.has_current_generation()
            && services_owned
            && (!module_epoch_required || (module_epoch_present && module_epoch_current))
            && runtime.host.is_quiescent();
        let certified = certified && handles_match_generation;

        if !certified {
            return Err((K::error(), self));
        }

        // The lease is the canonical owner of the cross-slot generation
        // arming decision. Once every service slot is stopped and all other
        // quiescence proofs hold, consuming it linearizes generation teardown.
        let module_epoch = runtime.lifecycle.take_certified_module_epoch(&mut control);

        #[cfg(any(test, feature = "refinement"))]
        let composition_resources = composition_resources_from_quiescence_proof(&proof);

        Ok(TerminalCertificate {
            proof,
            #[cfg(any(test, feature = "refinement"))]
            composition_resources,
            owner: self,
            generation: control.last_committed_generation(),
            module_epoch,
            _kind: std::marker::PhantomData,
        })
    }
}

impl<'runtime, A: crate::Addin> TerminalCertificate<'runtime, A, OpenRollback> {
    pub(crate) fn finish(self) -> Result<RemovalOwner<'runtime, A>, (XllError, Box<Self>)> {
        let runtime = self.owner.runtime;
        let mut control = runtime.lifecycle.lock();
        if control.removal_attempt() != Some(self.owner.attempt)
            || control.canonical_state().open_attempt().is_some()
            || !matches!(
                control.canonical_state().phase(),
                LifecyclePhase::OpenRollbackPending | LifecyclePhase::Closing
            )
        {
            return Err((
                XllError::Internal {
                    diagnostic_id: crate::error::DiagnosticId::OPEN_ROLLBACK_PHASE,
                },
                Box::new(self),
            ));
        }
        let TerminalCertificate {
            proof: _proof,
            #[cfg(any(test, feature = "refinement"))]
            composition_resources,
            owner,
            generation: _generation,
            module_epoch: _module_epoch,
            _kind: _,
        } = self;
        crate::module_runtime::global().close_callbacks();
        runtime.lifecycle.finish_closed(&mut control);
        #[cfg(any(test, feature = "refinement"))]
        runtime.record_composition_event(
            crate::composition_refinement::CompositionEvent::FinishOpenRollback(
                composition_resources,
            ),
        );
        #[cfg(any(test, feature = "refinement"))]
        if runtime.phase() != LifecyclePhase::Closed {
            crate::lifecycle::fail_stop_invariant(
                "xlAutoOpen rollback close postcondition",
                &XllError::Internal {
                    diagnostic_id: crate::error::DiagnosticId::OPEN_ROLLBACK_PHASE,
                },
            );
        }
        #[cfg(any(test, feature = "refinement"))]
        runtime.mark_composition_terminal_pending();
        runtime.lifecycle.notify_all();
        crate::module_runtime::global().certify_logical_quiescence();
        #[cfg(test)]
        drop(runtime.lifecycle.test_module_lease.lock().take());
        Ok(owner)
    }
}

impl<'runtime, A: crate::Addin> TerminalCertificate<'runtime, A, FinalRemoval> {
    pub(crate) fn finish(
        self,
    ) -> Result<(ClosedWitness, RemovalOwner<'runtime, A>), (XllError, Box<Self>)> {
        let runtime = self.owner.runtime;
        if self.generation != runtime.last_committed_generation() {
            return Err((
                XllError::Internal {
                    diagnostic_id: crate::error::DiagnosticId::CLOSE_LEASE_GATE,
                },
                Box::new(self),
            ));
        }
        #[cfg(any(test, feature = "refinement"))]
        let committed = runtime.refinement_hooks().generation_active(runtime);
        #[cfg(any(test, feature = "refinement"))]
        if committed && let Err(error) = runtime.refinement_hooks().finish_close(runtime) {
            return Err((error, Box::new(self)));
        }
        let mut control = runtime.lifecycle.lock();
        if control.removal_attempt() != Some(self.owner.attempt) {
            return Err((
                XllError::Internal {
                    diagnostic_id: crate::error::DiagnosticId::CLOSE_RUNTIME,
                },
                Box::new(self),
            ));
        }
        let TerminalCertificate {
            proof: _proof,
            #[cfg(any(test, feature = "refinement"))]
            composition_resources,
            owner,
            #[cfg(any(test, feature = "refinement"))]
            generation,
            module_epoch: _module_epoch,
            ..
        } = self;
        crate::module_runtime::global().close_callbacks();
        runtime.lifecycle.finish_closed(&mut control);
        #[cfg(any(test, feature = "refinement"))]
        if runtime.phase() != LifecyclePhase::Closed {
            crate::lifecycle::fail_stop_invariant(
                "xlAutoRemove close postcondition",
                &XllError::Internal {
                    diagnostic_id: crate::error::DiagnosticId::CLOSE_WAIT,
                },
            );
        }
        #[cfg(any(test, feature = "refinement"))]
        if committed {
            runtime.record_composition_event(
                crate::composition_refinement::CompositionEvent::PublishCommittedClosed,
            );
        }
        #[cfg(any(test, feature = "refinement"))]
        if !committed {
            runtime.record_composition_event(
                crate::composition_refinement::CompositionEvent::FinishUncommittedFinalClose(
                    composition_resources,
                ),
            );
        }
        runtime.lifecycle.notify_all();
        crate::module_runtime::global().certify_logical_quiescence();
        #[cfg(test)]
        drop(runtime.lifecycle.test_module_lease.lock().take());
        Ok((
            ClosedWitness {
                #[cfg(any(test, feature = "refinement"))]
                runtime_address: std::ptr::from_ref(runtime).addr(),
                #[cfg(any(test, feature = "refinement"))]
                generation,
            },
            owner,
        ))
    }
}

impl<A: crate::Addin> Runtime<A> {
    pub(crate) fn next_call_id(&self) -> u64 {
        self.return_protocol.next_call_id()
    }

    #[cfg(test)]
    pub(crate) fn peek_next_call_id(&self) -> u64 {
        self.return_protocol.peek_next_call_id()
    }

    pub(crate) fn calculation_id(&self) -> crate::execution::CalculationId {
        #[cfg(feature = "async")]
        {
            crate::execution::CalculationId::new(self.executors.async_manager.current_generation())
        }
        #[cfg(not(feature = "async"))]
        {
            crate::execution::CalculationId::new(
                self.return_protocol.calculation_id.load(Ordering::Acquire),
            )
        }
    }

    #[cfg(feature = "async")]
    pub(crate) fn finish_calculation(&self) {
        let _ = self.executors.async_manager.advance_generation();
    }

    #[cfg(any(test, feature = "bench-internals"))]
    pub(crate) fn formula_handle_service(
        &self,
    ) -> XllResult<Arc<crate::handle::FormulaHandleService>> {
        self.generation_services()?
            .formula_handle_slot()
            .get_owned()
    }

    fn generation_services_snapshot(&self) -> Option<Arc<GenerationServices>> {
        self.lifecycle.load_generation_services().or_else(|| {
            let control = self.lifecycle.lock();
            control.retiring_services().map(Arc::clone)
        })
    }

    #[cfg(any(test, feature = "refinement"))]
    pub(crate) fn generation_services(&self) -> XllResult<Arc<GenerationServices>> {
        let services = self.generation_services_snapshot();
        services.ok_or(XllError::Closing)
    }

    pub(crate) fn seal_generation_services(
        &self,
        subscriptions_stopped: crate::rtd::SubscriptionsStopped,
    ) -> XllResult<SealedGenerationServices> {
        let generation = self.protocol_generation();
        let Some(services) = self.generation_services_snapshot() else {
            return Ok(SealedGenerationServices::empty(
                generation,
                subscriptions_stopped,
            ));
        };
        services.seal(generation, subscriptions_stopped)
    }

    pub(crate) fn shutdown_rtd(&self) -> XllResult<()> {
        let Some(services) = self.generation_services_snapshot() else {
            return Ok(());
        };
        services.shutdown_rtd()
    }

    #[cfg(test)]
    pub(crate) fn finish_generation_services(
        &self,
        sealed: SealedGenerationServices,
    ) -> XllResult<(
        crate::shutdown::HandleStoreQuiescent,
        crate::rtd::SubscriptionsStopped,
    )> {
        sealed.finish()
    }

    #[inline]
    #[cfg(test)]
    pub(crate) fn subscriptions(&self) -> XllResult<crate::rtd::service::SubscriptionRuntimeRead> {
        let services = self.generation_services()?;
        services
            .subscriptions_slot()
            .read(services.subscription_host())
    }

    pub(crate) fn close_subscriptions(&self) -> XllResult<crate::shutdown::SubscriptionsStopped> {
        let Some(services) = self.generation_services_snapshot() else {
            return Ok(crate::rtd::SubscriptionsStopped::new());
        };
        services.subscriptions_slot().seal()
    }

    #[cfg(feature = "async")]
    pub(crate) fn start_async(&self, worker_count: usize) -> XllResult<()> {
        self.executors.async_manager.start(worker_count)
    }

    #[cfg(feature = "async")]
    pub(crate) fn cancel_async(&self) {
        self.executors.async_manager.cancel_current_generation();
    }

    #[cfg(feature = "async")]
    pub(crate) fn close_async(
        &self,
    ) -> crate::shutdown::StopOutcome<crate::shutdown::AsyncStopped> {
        self.executors.async_manager.close()
    }

    #[cfg(feature = "async")]
    pub(crate) fn async_manager(&self) -> &crate::async_udf::AsyncManager {
        &self.executors.async_manager
    }

    #[cfg(test)]
    pub(crate) fn release_test_module_lease(&self) {
        drop(self.lifecycle.test_module_lease.lock().take());
    }
}

impl<A: crate::Addin> Default for Runtime<A> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
impl<A: crate::Addin> Runtime<A> {
    pub(crate) fn cleanup_test_runtime(&self) {
        if !matches!(self.phase(), LifecyclePhase::Closed) {
            let ingress = crate::module_runtime::ingress();
            if matches!(
                ingress.phase(),
                crate::ingress::PHASE_OPENING | crate::ingress::PHASE_OPEN
            ) {
                ingress.begin_close_with(|| {});
            }
            if ingress.phase() == crate::ingress::PHASE_CLOSING {
                let _ = ingress.seal_and_drain();
            }
        }
        drop(self.lifecycle.test_module_lease.lock().take());
    }
}

#[cfg(test)]
impl<A: crate::Addin> Drop for Runtime<A> {
    fn drop(&mut self) {
        self.cleanup_test_runtime();
    }
}

#[cfg(test)]
pub(crate) struct StaticTestRuntime<A: crate::Addin> {
    runtime: &'static Runtime<A>,
}

#[cfg(test)]
impl<A: crate::Addin> StaticTestRuntime<A> {
    pub(crate) fn new() -> Self {
        let runtime = Box::leak(Box::new(Runtime::new()));
        Self { runtime }
    }

    pub(crate) fn runtime(&self) -> &'static Runtime<A> {
        self.runtime
    }
}

#[cfg(test)]
impl<A: crate::Addin> Drop for StaticTestRuntime<A> {
    fn drop(&mut self) {
        self.runtime.cleanup_test_runtime();
    }
}

pub(crate) struct RemovalOwner<'runtime, A: crate::Addin> {
    runtime: &'runtime Runtime<A>,
    attempt: RemovalAttemptId,
}

impl<A: crate::Addin> Drop for RemovalOwner<'_, A> {
    fn drop(&mut self) {
        let mut control = self.runtime.lifecycle.lock();
        self.runtime
            .lifecycle
            .release_removal_owner(&mut control, self.attempt);
        if control.removal_attempt().is_some() {
            crate::lifecycle::fail_stop_invariant(
                "xlAutoRemove removal-owner release",
                &XllError::Internal {
                    diagnostic_id: crate::error::DiagnosticId::CLOSE_WAIT,
                },
            );
        }
        self.runtime.refinement.release_cleanup_owner(self.runtime);
        self.runtime.lifecycle.notify_all();
    }
}

impl<'runtime, A: crate::Addin> RemovalOwner<'runtime, A> {
    pub(crate) fn runtime(&self) -> &'runtime Runtime<A> {
        self.runtime
    }
}

pub(crate) struct OpenAttemptBegun;

pub(crate) struct OpenGenerationStaged;

type OpeningStageFailure<'runtime, A> = (
    XllError,
    Box<OpeningTxn<'runtime, A, OpenAttemptBegun>>,
    Box<OpeningGeneration<A>>,
);

pub(crate) struct OpeningTxn<'runtime, A: crate::Addin, Stage> {
    runtime: &'runtime Runtime<A>,
    attempt_id: OpenAttemptId,
    module_opening: Option<crate::module_runtime::ModuleOpening>,
    _stage: PhantomData<fn() -> Stage>,
}

impl<A: crate::Addin, Stage> OpeningTxn<'_, A, Stage> {
    pub(crate) const fn attempt_id(&self) -> OpenAttemptId {
        self.attempt_id
    }

    pub(crate) fn fail(mut self) -> crate::runtime_components::OpenFailureDisposition {
        let disposition = self.runtime.fail_and_record(self.attempt_id);
        let _ = self.module_opening.take();
        disposition
    }
}

impl<'runtime, A: crate::Addin> OpeningTxn<'runtime, A, OpenAttemptBegun> {
    pub(crate) fn stage(
        mut self,
        opening: OpeningGeneration<A>,
    ) -> Result<OpeningTxn<'runtime, A, OpenGenerationStaged>, OpeningStageFailure<'runtime, A>>
    {
        let result = self
            .runtime
            .stage_opening_generation_for_attempt(self.attempt_id, opening);
        match result {
            Ok(()) => {
                let module_opening = self
                    .module_opening
                    .take()
                    .expect("an open attempt owns the module token before staging");
                Ok(OpeningTxn {
                    runtime: self.runtime,
                    attempt_id: self.attempt_id,
                    module_opening: Some(module_opening),
                    _stage: PhantomData,
                })
            }
            Err((error, opening)) => Err((error, Box::new(self), Box::new(opening))),
        }
    }
}

impl<'runtime, A: crate::Addin, Stage> OpeningTxn<'runtime, A, Stage> {
    pub(crate) fn commit(
        mut self,
        journal: &mut crate::registration::HostMutationJournal,
    ) -> XllResult<()> {
        let Some(module_opening) = self.module_opening.take() else {
            return Err(XllError::Closing);
        };
        self.runtime
            .finish_open_for_attempt(self.attempt_id, module_opening, journal)
    }

    #[cfg(any(test, feature = "bench-internals"))]
    pub(crate) fn commit_in_place(
        &mut self,
        journal: &mut crate::registration::HostMutationJournal,
    ) -> XllResult<()> {
        let Some(module_opening) = self.module_opening.take() else {
            return Err(XllError::Closing);
        };
        self.runtime
            .finish_open_for_attempt(self.attempt_id, module_opening, journal)
    }
}

impl<A: crate::Addin, Stage> Drop for OpeningTxn<'_, A, Stage> {
    fn drop(&mut self) {
        if self.module_opening.is_none() {
            return;
        }
        // Lifecycle rollback is owned by OpeningTxn and must be explicit.
        // Dropping any unfinished stage can only enter the fail-safe state;
        // Drop never invokes host callbacks or resource cleanup.
        self.runtime.quarantine();
    }
}

pub struct CallGuard<'runtime, A: crate::Addin> {
    _ingress: &'runtime AdmittedExport<'runtime>,
    #[cfg(any(test, feature = "refinement"))]
    runtime: &'runtime Runtime<A>,
    #[cfg(not(any(test, feature = "refinement")))]
    _runtime: std::marker::PhantomData<&'runtime Runtime<A>>,
    admission: GenerationAdmission<A>,
}

impl<A: crate::Addin> CallGuard<'_, A> {
    #[must_use]
    pub fn state(&self) -> &A::SharedState {
        &self.generation().shared_state
    }

    #[must_use]
    pub(crate) fn layers(&self) -> &A::Layers {
        &self.generation().layers
    }

    /// Returns the generation services pinned by this call.
    ///
    /// A call must derive every generation-scoped service from the same
    /// publication as its state and layers. Reloading the service projection
    /// through `Runtime` would permit a call to observe a different
    /// generation after it has already entered.
    #[must_use]
    pub(crate) fn services(&self) -> &GenerationServices {
        self.admission.services()
    }

    fn generation(&self) -> &ExecutionGeneration<A> {
        let generation = self.admission.generation();
        let _ = generation.id();
        generation
    }

    #[cfg(feature = "async")]
    #[must_use]
    pub(crate) fn lease(&self) -> ExecutionLease<A> {
        ExecutionLease {
            generation: Arc::clone(self.admission.generation_arc()),
        }
    }
}

impl<A: crate::Addin> Drop for CallGuard<'_, A> {
    fn drop(&mut self) {
        #[cfg(any(test, feature = "refinement"))]
        self.runtime.refinement_hooks().call_left(self.runtime);
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::thread;
    use std::time::{Duration, Instant};

    #[derive(Clone)]
    struct TestU32Addin;

    impl crate::Addin for TestU32Addin {
        type SharedState = u32;
        type LifecycleState = ();
        type Error = XllError;
        type Layers = ();

        fn open(
            _context: &crate::addin::OpenContext,
        ) -> Result<
            crate::addin::Opened<Self::SharedState, Self::LifecycleState, Self::Layers>,
            Self::Error,
        > {
            Ok(crate::addin::Opened::new(0, (), ()))
        }
    }

    fn admitted_export() -> crate::ingress::AdmittedExport<'static> {
        crate::module_runtime::ingress()
            .enter_with(|| {})
            .into_admitted()
            .expect("test call enters during OPEN")
    }

    fn finish_test_close<A: crate::Addin>(
        runtime: &Runtime<A>,
        removal_attempt: RemovalOwner<'_, A>,
    ) {
        let ingress = crate::module_runtime::ingress();
        if matches!(
            ingress.phase(),
            crate::ingress::PHASE_OPENING | crate::ingress::PHASE_OPEN
        ) {
            ingress.begin_close_with(|| {
                #[cfg(any(test, feature = "refinement"))]
                if runtime.refinement_hooks().generation_active(runtime) {
                    runtime.refinement_hooks().begin_close(runtime);
                }
            });
        }
        let exports = ingress.seal_and_drain();
        let subscriptions_stopped = runtime
            .close_subscriptions()
            .expect("test subscriptions stop");
        let _ = runtime.shutdown_rtd();
        let sealed = runtime
            .seal_generation_services(subscriptions_stopped)
            .expect("test generation service seal");
        let _ = runtime.finish_generation_services(sealed);
        // This helper validates Runtime's close certificate in isolation. It
        // deliberately does not synthesize lifecycle ghost milestones; those
        // are exercised by the real lifecycle close path.
        runtime.disable_ghost_for_test();
        let rtd = crate::rtd::wait_for_module_quiescence().expect("RTD module quiescence");
        let certificate = removal_attempt
            .certify::<FinalRemoval>(QuiescenceProof {
                exports,
                rtd,
                host_callbacks: crate::shutdown::HostCallbacksDetached::for_test(),
                async_stopped: crate::shutdown::AsyncStopped::for_test(),
                subscriptions_stopped: crate::shutdown::SubscriptionsStopped::for_test(),
                handle_store_quiescent: crate::shutdown::HandleStoreQuiescent::for_test(Some(
                    crate::generation::RuntimeGeneration::new(1).unwrap(),
                )),
                diagnostics_stopped: crate::diagnostics::DiagnosticsStopped::for_test(),
                addin_quiesced: crate::shutdown::AddinQuiesced::for_test(),
                generation_reclaimed: crate::shutdown::GenerationReclaimed::for_test(),
            })
            .map_err(|(error, _owner)| error)
            .unwrap();
        let (_witness, _removal_attempt) = certificate
            .finish()
            .unwrap_or_else(|(error, _certificate)| panic!("{error}"));
        runtime.release_test_module_lease();
    }

    fn finish_test_open_rollback<'a, A: crate::Addin>(
        runtime: &'a Runtime<A>,
        rollback_attempt: RemovalOwner<'a, A>,
    ) -> RemovalOwner<'a, A> {
        let ingress = crate::module_runtime::ingress();
        if matches!(
            ingress.phase(),
            crate::ingress::PHASE_OPENING | crate::ingress::PHASE_OPEN
        ) {
            ingress.begin_close_with(|| {});
        }
        let exports = ingress.seal_and_drain();
        let certificate = rollback_attempt
            .certify::<OpenRollback>(QuiescenceProof {
                exports,
                rtd: crate::rtd::wait_for_module_quiescence().expect("RTD module quiescence"),
                host_callbacks: crate::shutdown::HostCallbacksDetached::for_test(),
                async_stopped: crate::shutdown::AsyncStopped::for_test(),
                subscriptions_stopped: crate::shutdown::SubscriptionsStopped::for_test(),
                handle_store_quiescent: crate::shutdown::HandleStoreQuiescent::for_test(Some(
                    crate::generation::RuntimeGeneration::new(1).unwrap(),
                )),
                diagnostics_stopped: crate::diagnostics::DiagnosticsStopped::for_test(),
                addin_quiesced: crate::shutdown::AddinQuiesced::for_test(),
                generation_reclaimed: crate::shutdown::GenerationReclaimed::for_test(),
            })
            .map_err(|(error, _owner)| error)
            .unwrap();
        let rollback_attempt = certificate
            .finish()
            .unwrap_or_else(|(error, _certificate)| panic!("{error}"));
        runtime.release_test_module_lease();
        rollback_attempt
    }

    pub(crate) static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn runtime_can_open_close_and_reopen() {
        let _test_guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        struct TestHandle(u32);
        impl crate::handle::ExcelHandleObject for TestHandle {}

        let runtime = Runtime::<TestU32Addin>::new();
        let mut open_attempt = runtime.begin_open().unwrap();
        runtime.publish(1_u32, ());
        runtime.finish_open(&mut open_attempt, Vec::new()).unwrap();
        let ingress = admitted_export();
        assert_eq!(runtime.enter(&ingress).unwrap().state(), &1);
        let old_handles = runtime.formula_handle_service().unwrap();
        let old_token = old_handles
            .prepare(crate::handle::test_topic_key("old"), || Ok(TestHandle(1)))
            .unwrap()
            .into_token();

        let removal_attempt = runtime.begin_final_removal().unwrap();
        assert_eq!(runtime.take_current_generation().unwrap().shared_state, 1);
        finish_test_close(&runtime, removal_attempt);

        let mut open_attempt = runtime.begin_open().unwrap();
        runtime.publish(2_u32, ());
        runtime.finish_open(&mut open_attempt, Vec::new()).unwrap();
        let ingress = admitted_export();
        assert_eq!(runtime.enter(&ingress).unwrap().state(), &2);
        let new_handles = runtime.formula_handle_service().unwrap();
        let new_token = new_handles
            .prepare(crate::handle::test_topic_key("new"), || Ok(TestHandle(2)))
            .unwrap()
            .into_token();
        assert_eq!(
            crate::value::with_excel_call_scope(|scope| {
                new_handles
                    .lookup::<TestHandle>(scope, &new_token)
                    .map(|value| value.0)
            })
            .unwrap(),
            2
        );
        assert!(matches!(
            crate::value::with_excel_call_scope(|scope| {
                new_handles
                    .lookup::<TestHandle>(scope, &old_token)
                    .map(|_| ())
            }),
            Err(XllError::StaleHandle | XllError::InvalidHandle)
        ));
    }

    #[test]
    fn close_on_closed_runtime_invalidates_an_older_open_epoch() {
        let _test_guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let runtime = Runtime::<()>::new();
        let stale_epoch = runtime.removal_epoch();

        assert!(runtime.begin_final_removal().is_none());
        assert!(runtime.begin_open_if_epoch(stale_epoch).is_err());

        let mut current = runtime.begin_open().unwrap();
        runtime.publish((), ());
        runtime.finish_open(&mut current, Vec::new()).unwrap();
        assert_eq!(runtime.phase(), LifecyclePhase::Open);
    }

    #[test]
    fn a_failed_concurrent_open_cannot_rollback_the_active_attempt() {
        let _test_guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let runtime = Runtime::<TestU32Addin>::new();
        let mut first = runtime.begin_open().unwrap();

        assert!(runtime.begin_open().is_err());
        assert_eq!(runtime.phase(), LifecyclePhase::Opening);

        runtime.publish(11_u32, ());
        runtime.finish_open(&mut first, Vec::new()).unwrap();
        assert_eq!(runtime.phase(), LifecyclePhase::Open);
        let ingress = admitted_export();
        assert_eq!(runtime.enter(&ingress).unwrap().state(), &11);
        let close = runtime.begin_final_removal().unwrap();
        let _ = runtime.take_current_generation();
        finish_test_close(&runtime, close);
    }

    #[test]
    fn dropping_open_attempt_quarantines_without_implicit_rollback() {
        let _test_guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let runtime = Runtime::<()>::new();
        let opening = runtime.begin_open().unwrap();

        drop(opening);

        assert_eq!(runtime.phase(), LifecyclePhase::Quarantined);
        let trace = runtime.composition_trace_json();
        assert!(!trace.contains("\"failOpen\""));
        assert!(runtime.acquire_open_rollback().is_none());
    }

    #[test]
    fn final_close_cancels_an_in_flight_open_commit() {
        let _test_guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let runtime = Arc::new(Runtime::<TestU32Addin>::new());
        let mut opening = runtime.begin_open().unwrap();
        runtime.publish(17_u32, ());

        let removal_epoch = runtime.removal_epoch();
        let closing_runtime = Arc::clone(&runtime);
        let (closing_entered_tx, closing_entered_rx) = mpsc::channel();
        let (closing_release_tx, closing_release_rx) = mpsc::channel();
        let (closed_tx, closed_rx) = mpsc::sync_channel(1);
        let closer = thread::spawn(move || {
            let close = closing_runtime
                .begin_final_removal()
                .expect("the opening runtime requires final close");
            closing_entered_tx.send(()).unwrap();
            closing_release_rx.recv().unwrap();
            let state = match closing_runtime
                .take_generation_for_shutdown()
                .expect("shutdown extracts generation")
            {
                ShutdownGeneration::Open(generation) => generation.shared_state,
                ShutdownGeneration::Opening(opening) => opening.into_parts().0,
            };
            assert_eq!(state, 17);
            finish_test_close(&closing_runtime, close);
            closed_tx.send(()).unwrap();
        });

        let deadline = Instant::now() + Duration::from_secs(5);
        while runtime.phase() != LifecyclePhase::Closing && Instant::now() < deadline {
            thread::yield_now();
        }
        assert_eq!(runtime.phase(), LifecyclePhase::Closing);
        assert_ne!(runtime.removal_epoch(), removal_epoch);
        assert!(matches!(
            runtime.finish_open(&mut opening, Vec::new()),
            Err(XllError::Closing)
        ));
        assert_eq!(runtime.open_attempt(), None);

        closing_entered_rx.recv().unwrap();
        closing_release_tx.send(()).unwrap();

        closed_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        closer.join().unwrap();
        assert_eq!(runtime.phase(), LifecyclePhase::Closed);
    }

    #[test]
    fn logical_quiescence_certificate_survives_a_concurrent_removal_epoch_bump() {
        let _test_guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let runtime = Arc::new(Runtime::<()>::new());
        let mut opening = runtime.begin_open().unwrap();
        runtime.publish((), ());
        runtime.finish_open(&mut opening, Vec::new()).unwrap();

        let removal_attempt = runtime.begin_final_removal().unwrap();
        runtime.wait_for_returns();
        let subscriptions_stopped = runtime.close_subscriptions().unwrap();
        runtime.shutdown_rtd().unwrap();
        let sealed = runtime
            .seal_generation_services(subscriptions_stopped)
            .unwrap();
        runtime.finish_generation_services(sealed).unwrap();
        assert!(runtime.take_current_generation().is_some());

        let ingress = crate::module_runtime::ingress();
        ingress.begin_close_with(|| {
            #[cfg(any(test, feature = "refinement"))]
            if runtime.refinement_hooks().generation_active(&runtime) {
                runtime.refinement_hooks().begin_close(&runtime);
            }
        });
        let exports = ingress.seal_and_drain();
        runtime.disable_ghost_for_test();
        let rtd = crate::rtd::wait_for_module_quiescence().expect("RTD module quiescence");
        let certificate = removal_attempt
            .certify::<FinalRemoval>(QuiescenceProof {
                exports,
                rtd,
                host_callbacks: crate::shutdown::HostCallbacksDetached::for_test(),
                async_stopped: crate::shutdown::AsyncStopped::for_test(),
                subscriptions_stopped: crate::shutdown::SubscriptionsStopped::for_test(),
                handle_store_quiescent: crate::shutdown::HandleStoreQuiescent::for_test(Some(
                    crate::generation::RuntimeGeneration::new(1).unwrap(),
                )),
                diagnostics_stopped: crate::diagnostics::DiagnosticsStopped::for_test(),
                addin_quiesced: crate::shutdown::AddinQuiesced::for_test(),
                generation_reclaimed: crate::shutdown::GenerationReclaimed::for_test(),
            })
            .map_err(|(error, _owner)| error)
            .unwrap();

        // A second final-close invocation invalidates stale open attempts, but
        // it must not invalidate the certificate held by the active close
        // owner. The second caller waits until that owner is released.
        let removal_epoch = runtime.removal_epoch();
        let concurrent_runtime = Arc::clone(&runtime);
        let (started_tx, started_rx) = mpsc::sync_channel(1);
        let waiter = thread::spawn(move || {
            started_tx.send(()).unwrap();
            assert!(concurrent_runtime.begin_final_removal().is_none());
        });
        started_rx.recv().unwrap();

        let deadline = Instant::now() + Duration::from_secs(1);
        while runtime.removal_epoch() == removal_epoch && Instant::now() < deadline {
            thread::yield_now();
        }
        assert_ne!(runtime.removal_epoch(), removal_epoch);

        let (_witness, removal_attempt) = certificate
            .finish()
            .unwrap_or_else(|(error, _certificate)| panic!("{error}"));
        drop(removal_attempt);
        waiter.join().unwrap();
        assert_eq!(runtime.phase(), LifecyclePhase::Closed);
    }

    #[test]
    fn close_waiter_is_not_lost_when_open_rollback_finishes() {
        let _test_guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let runtime = Arc::new(Runtime::<()>::new());
        let opening = runtime.begin_open().unwrap();
        assert!(opening.fail().requires_rollback());
        let rollback = runtime.acquire_open_rollback().unwrap();

        let closing_runtime = Arc::clone(&runtime);
        let (closed_tx, closed_rx) = mpsc::sync_channel(1);
        let closer = thread::spawn(move || {
            assert!(closing_runtime.begin_final_removal().is_none());
            closed_tx.send(()).unwrap();
        });

        let deadline = Instant::now() + Duration::from_secs(1);
        while runtime.phase() != LifecyclePhase::Closing && Instant::now() < deadline {
            thread::yield_now();
        }
        assert_eq!(runtime.phase(), LifecyclePhase::Closing);
        let rollback = finish_test_open_rollback(&runtime, rollback);
        assert!(closed_rx.recv_timeout(Duration::from_millis(20)).is_err());
        drop(rollback);

        closed_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        closer.join().unwrap();
        assert_eq!(runtime.phase(), LifecyclePhase::Closed);
    }

    #[test]
    fn abandoned_close_owner_notifies_and_allows_takeover() {
        let _test_guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let runtime = Arc::new(Runtime::<()>::new());
        let mut opening = runtime.begin_open().unwrap();
        runtime.publish((), ());
        runtime.finish_open(&mut opening, Vec::new()).unwrap();

        let first = runtime.begin_final_removal().unwrap();
        drop(first);

        let second = runtime.begin_final_removal().unwrap();
        let _ = runtime.take_current_generation();
        finish_test_close(&runtime, second);
        assert_eq!(runtime.phase(), LifecyclePhase::Closed);
    }

    #[test]
    fn lifecycle_attempt_counter_refuses_exhaustion() {
        let _test_guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let runtime = Runtime::<()>::new();
        runtime
            .lifecycle
            .lock()
            .set_next_lifecycle_attempt_for_test(u64::MAX);
        assert!(runtime.begin_open().is_err());
        assert_eq!(runtime.phase(), LifecyclePhase::Closed);
    }

    #[test]
    fn logical_quiescence_certificate_refuses_to_publish_closed_before_state_is_released() {
        let _test_guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let runtime = Runtime::<()>::new();
        let mut opening = runtime.begin_open().unwrap();
        runtime.publish((), ());
        runtime.finish_open(&mut opening, Vec::new()).unwrap();

        let removal_attempt = runtime.begin_final_removal().unwrap();
        runtime.wait_for_returns();
        let subscriptions_stopped = runtime.close_subscriptions().unwrap();
        runtime.shutdown_rtd().unwrap();
        let sealed = runtime
            .seal_generation_services(subscriptions_stopped)
            .unwrap();
        runtime.finish_generation_services(sealed).unwrap();
        let ingress = crate::module_runtime::ingress();
        ingress.begin_close_with(|| {
            #[cfg(any(test, feature = "refinement"))]
            if runtime.refinement_hooks().generation_active(&runtime) {
                runtime.refinement_hooks().begin_close(&runtime);
            }
        });
        let exports = ingress.seal_and_drain();
        let rtd = crate::rtd::wait_for_module_quiescence().expect("RTD module quiescence");
        let removal_attempt = match removal_attempt.certify::<FinalRemoval>(QuiescenceProof {
            exports,
            rtd,
            host_callbacks: crate::shutdown::HostCallbacksDetached::for_test(),
            async_stopped: crate::shutdown::AsyncStopped::for_test(),
            subscriptions_stopped: crate::shutdown::SubscriptionsStopped::for_test(),
            handle_store_quiescent: crate::shutdown::HandleStoreQuiescent::for_test(Some(
                crate::generation::RuntimeGeneration::new(1).unwrap(),
            )),
            diagnostics_stopped: crate::diagnostics::DiagnosticsStopped::for_test(),
            addin_quiesced: crate::shutdown::AddinQuiesced::for_test(),
            generation_reclaimed: crate::shutdown::GenerationReclaimed::for_test(),
        }) {
            Err((_error, owner)) => owner,
            Ok(_certificate) => panic!("quiescence certificate must reject a live generation"),
        };
        assert_eq!(runtime.phase(), LifecyclePhase::Closing);

        assert!(runtime.take_current_generation().is_some());
        finish_test_close(&runtime, removal_attempt);
        assert_eq!(runtime.phase(), LifecyclePhase::Closed);
    }

    #[test]
    fn close_rejects_new_calls_and_waits_for_existing_call() {
        let _test_guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let runtime = Arc::new(Runtime::<TestU32Addin>::new());
        let mut open_attempt = runtime.begin_open().unwrap();
        runtime.publish(7_u32, ());
        runtime.finish_open(&mut open_attempt, Vec::new()).unwrap();

        let ingress = crate::module_runtime::ingress()
            .enter_with(|| {})
            .into_admitted()
            .expect("test call enters during OPEN");
        let guard = runtime.enter(&ingress).unwrap();
        assert!(runtime.begin_close());
        crate::module_runtime::ingress().begin_close_with(|| {});
        assert!(matches!(runtime.enter(&ingress), Err(XllError::Closing)));

        let (sender, receiver) = mpsc::channel();
        let handle = thread::spawn(move || {
            let _ = crate::module_runtime::ingress().seal_and_drain();
            sender.send(()).unwrap();
        });

        assert!(receiver.recv_timeout(Duration::from_millis(20)).is_err());
        drop(guard);
        drop(ingress);
        receiver.recv_timeout(Duration::from_secs(1)).unwrap();
        handle.join().unwrap();
    }

    #[test]
    fn registration_storage_is_replaceable() {
        let runtime = Runtime::<()>::new();
        let mut journal = crate::registration::HostMutationJournal::default();
        journal
            .pending_registrations
            .push(crate::registration::PendingRegistration::from(
                RegistrationId {
                    id: 1.0,
                    excel_name: "TEST",
                },
            ));
        runtime.retain_host_mutations(journal);
        assert_eq!(runtime.registrations().len(), 1);
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn module_residency_is_independent_from_logical_close() {
        let runtime = Runtime::<()>::new();
        assert!(!runtime.module_residency_held());
        assert!(runtime.ensure_module_residency(std::ptr::null()).unwrap());
        assert!(runtime.module_residency_held());
        assert!(!runtime.ensure_module_residency(std::ptr::null()).unwrap());

        runtime.quarantine();
        assert_eq!(runtime.phase(), LifecyclePhase::Quarantined);
        assert!(runtime.module_residency_held());
        runtime.release_module_residency().unwrap();
        assert!(!runtime.module_residency_held());
    }

    #[test]
    fn metadata_debt_storage_is_queryable() {
        let runtime = Runtime::<()>::new();
        runtime.retain_metadata_debt(vec![
            crate::registration::MetadataDebt::new(
                RegistrationId {
                    id: 1.0,
                    excel_name: "TEST_DEBT",
                },
                XllError::Closing,
            ),
            crate::registration::MetadataDebt::new(
                RegistrationId {
                    id: 2.0,
                    excel_name: "test_debt",
                },
                XllError::Panic,
            ),
        ]);
        assert_eq!(runtime.metadata_debt().len(), 1);
        assert_eq!(runtime.metadata_debt().values().next().unwrap().len(), 2);
        assert_eq!(
            runtime.metadata_debt().values().next().unwrap()[0].expected_registration_id(),
            1.0
        );
        runtime.clear_metadata_debt_for_registrations(&[RegistrationId {
            id: 1.0,
            excel_name: "Test_Debt",
        }]);
        assert!(runtime.metadata_debt().is_empty());
    }

    #[cfg(feature = "async")]
    #[test]
    fn calculation_end_advances_the_async_task_generation() {
        let _test_guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let runtime = Runtime::<()>::new();
        runtime.start_async(1).unwrap();
        let first = runtime.calculation_id().get();
        let (first_source, first_token) = crate::cancellation::CancellationSource::new(
            crate::cancellation::CancellationGuarantee::CalculationScoped,
        );
        runtime
            .async_manager()
            .spawn(first, std::future::pending(), first_source)
            .unwrap();

        runtime.finish_calculation();
        let second = runtime.calculation_id().get();
        assert_eq!(second, first + 1);
        assert!(matches!(
            runtime.async_manager().spawn(
                first,
                std::future::pending(),
                crate::cancellation::CancellationSource::new(
                    crate::cancellation::CancellationGuarantee::CalculationScoped,
                )
                .0,
            ),
            Err(XllError::ExcelValue(crate::ExcelError::NotAvailable))
        ));

        let (second_source, second_token) = crate::cancellation::CancellationSource::new(
            crate::cancellation::CancellationGuarantee::CalculationScoped,
        );
        runtime
            .async_manager()
            .spawn(second, std::future::pending(), second_source)
            .unwrap();
        runtime.cancel_async();
        assert!(second_token.is_cancelled());
        assert!(!first_token.is_cancelled());

        assert!(runtime.close_async().issues.is_empty());
        assert!(first_token.is_cancelled());
    }

    #[cfg(feature = "async")]
    #[test]
    fn published_async_generation_already_has_a_registry_entry() {
        let _test_guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let runtime = Arc::new(Runtime::<()>::new());
        runtime.start_async(1).unwrap();
        let first = runtime.calculation_id().get();
        let (published_tx, published_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        let release_rx = Arc::new(std::sync::Mutex::new(release_rx));
        runtime
            .async_manager()
            .set_after_generation_publish_hook(Some(Arc::new(move || {
                published_tx.send(()).unwrap();
                release_rx
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(1))
                    .unwrap();
            })));

        let advancing_runtime = Arc::clone(&runtime);
        let advancing = thread::spawn(move || advancing_runtime.finish_calculation());
        published_rx.recv_timeout(Duration::from_secs(1)).unwrap();

        let published = runtime.calculation_id().get();
        assert_eq!(published, first + 1);
        let (spawned_tx, spawned_rx) = mpsc::sync_channel(1);
        let spawning_runtime = Arc::clone(&runtime);
        let spawning = thread::spawn(move || {
            let source = crate::cancellation::CancellationSource::new(
                crate::cancellation::CancellationGuarantee::CalculationScoped,
            )
            .0;
            spawned_tx
                .send(
                    spawning_runtime
                        .async_manager()
                        .spawn(published, async {}, source),
                )
                .unwrap();
        });

        let spawn_result = spawned_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("spawn should not wait for the manager state mutex");
        assert!(spawn_result.is_ok());
        release_tx.send(()).unwrap();
        advancing.join().unwrap();
        spawning.join().unwrap();

        runtime
            .async_manager()
            .set_after_generation_publish_hook(None);
        assert!(runtime.close_async().issues.is_empty());
    }
}

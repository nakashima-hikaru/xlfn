use crate::generation::{OpenAttemptId, RemovalEpoch, RuntimeGeneration};
use crate::lifecycle::{HostLifecycleIntent, LifecyclePhase};
use crate::registration::RegistrationId;
use crate::{XllError, XllResult};
use std::collections::BTreeMap;
use std::sync::Arc;
#[cfg(not(feature = "async"))]
use std::sync::atomic::Ordering;

#[cfg(feature = "async")]
use crate::runtime_components::RuntimeExecutors;
use crate::runtime_components::{
    GenerationServices, HostLedger, LifecycleState, LifecycleStateKind, ModuleResidency,
    QuarantineReason, QuarantineVault, ReturnProtocol, ThreadAffineAccess, ThreadAffineError,
    ThreadAffineInstallError,
};
use crate::runtime_refinement::RuntimeRefinementHooks;

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
pub struct OpenGeneration<A: crate::Addin> {
    pub(crate) id: RuntimeGeneration,
    pub(crate) shared_state: A::SharedState,
    pub(crate) layers: A::Layers,
}

impl<A: crate::Addin> OpenGeneration<A> {
    pub(crate) const fn id(&self) -> RuntimeGeneration {
        self.id
    }
}

/// Unique Add-in state staged during `OPENING`.
pub enum OpeningGeneration<A: crate::Addin> {
    SharedStateOnly {
        shared_state: A::SharedState,
        config: crate::addin::RuntimeConfig,
    },
    Ready {
        shared_state: A::SharedState,
        layers: A::Layers,
        config: crate::addin::RuntimeConfig,
    },
}

impl<A: crate::Addin> OpeningGeneration<A> {
    #[must_use]
    pub(crate) fn into_parts(
        self,
    ) -> (
        A::SharedState,
        Option<A::Layers>,
        crate::addin::RuntimeConfig,
    ) {
        match self {
            Self::SharedStateOnly {
                shared_state,
                config,
            } => (shared_state, None, config),
            Self::Ready {
                shared_state,
                layers,
                config,
            } => (shared_state, Some(layers), config),
        }
    }

    #[must_use]
    pub(crate) fn attach_layers(self, layers: A::Layers) -> Self {
        match self {
            Self::SharedStateOnly {
                shared_state,
                config,
            }
            | Self::Ready {
                shared_state,
                config,
                ..
            } => Self::Ready {
                shared_state,
                layers,
                config,
            },
        }
    }
}

/// Generation reclaimed during shutdown.
pub(crate) enum ShutdownGeneration<A: crate::Addin> {
    Opening(OpeningGeneration<A>),
    Open(Arc<OpenGeneration<A>>),
}

/// Explicit open-generation lifetime lease for call-scoped and asynchronous
/// UDF executions.
pub struct GenerationLease<A: crate::Addin> {
    pub(crate) generation: Arc<OpenGeneration<A>>,
}

impl<A: crate::Addin> Clone for GenerationLease<A> {
    fn clone(&self) -> Self {
        Self {
            generation: Arc::clone(&self.generation),
        }
    }
}

impl<A: crate::Addin> GenerationLease<A> {
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
    pub(crate) lifecycle: LifecycleState<A>,
    pub(crate) lifecycle_state: crate::runtime_components::ThreadAffineSlot<A::LifecycleState>,
    pub(crate) host: HostLedger,
    pub(crate) return_protocol: ReturnProtocol,
    pub(crate) generation_services: GenerationServices,
    #[cfg(feature = "async")]
    pub(crate) executors: RuntimeExecutors,
    pub(crate) residency: ModuleResidency,
    pub(crate) quarantine: QuarantineVault<A>,
    pub(crate) refinement: RuntimeRefinementHooks,
}

pub(crate) type LifecycleThreadAccess<'runtime, A> =
    ThreadAffineAccess<'runtime, <A as crate::Addin>::LifecycleState>;

impl<A: crate::Addin> Runtime<A> {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            lifecycle: LifecycleState::new(),
            lifecycle_state: crate::runtime_components::ThreadAffineSlot::new(),
            host: HostLedger::new(),
            return_protocol: ReturnProtocol::new(),
            generation_services: GenerationServices::new(),
            #[cfg(feature = "async")]
            executors: RuntimeExecutors::new(),
            residency: ModuleResidency::new(),
            quarantine: QuarantineVault::new(),
            refinement: RuntimeRefinementHooks::new(),
        }
    }

    #[cfg(any(test, feature = "shutdown-refinement"))]
    pub(crate) fn ghost_handle(&self) -> crate::shutdown_refinement::GhostHandle {
        self.refinement.ghost_handle()
    }

    #[cfg(any(test, feature = "shutdown-refinement"))]
    pub(crate) fn composition_trace(&self) -> &crate::composition_refinement::CompositionTrace {
        self.refinement.composition_trace()
    }

    #[cfg(any(test, feature = "shutdown-refinement"))]
    pub(crate) fn record_composition_event(
        &self,
        event: crate::composition_refinement::CompositionEvent,
    ) {
        self.composition_trace().record(event);
    }

    #[cfg(any(test, feature = "shutdown-refinement"))]
    pub(crate) fn record_composition_begin_open(&self, sampled_epoch: u64, attempt: u64) {
        self.composition_trace().begin_open(sampled_epoch, attempt);
    }

    #[cfg(any(test, feature = "shutdown-refinement"))]
    fn mark_composition_return_pending(&self) {
        self.composition_trace().mark_return_pending();
    }

    #[cfg(any(test, feature = "shutdown-refinement"))]
    pub(crate) fn finish_composition_return(&self) {
        self.composition_trace().finish_return();
    }

    // This is called by the explicit removal boundary after the terminal
    // teardown has returned AlreadyClosed; begin_final_removal only records its
    // lifecycle request and does not claim the host call returned successfully.
    #[cfg(any(test, feature = "shutdown-refinement"))]
    pub(crate) fn record_composition_already_closed_return(&self) {
        self.mark_composition_return_pending();
        self.finish_composition_return();
    }

    #[cfg(any(test, feature = "shutdown-refinement"))]
    fn mark_composition_terminal_pending(&self) {
        self.composition_trace().mark_terminal_pending();
    }

    #[must_use]
    pub fn phase(&self) -> LifecyclePhase {
        self.lifecycle.observed_phase()
    }

    pub(crate) fn host_intent(&self) -> HostLifecycleIntent {
        self.lifecycle.lock().host_intent
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
        let mut lease = self.residency.lease.lock();
        if lease.is_some() {
            return Ok(false);
        }
        *lease = Some(crate::module_residency::ModuleResidencyLease::acquire(
            anchor,
        )?);
        Ok(true)
    }

    /// Releases the physical residency reference after explicit removal has
    /// completed. Ordinary host shutdown hints never call this method.
    pub(crate) fn release_module_residency(&self) -> XllResult<()> {
        let mut lease = self.residency.lease.lock();
        let Some(residency) = lease.as_mut() else {
            return Ok(());
        };
        residency.try_release()?;
        drop(lease.take());
        Ok(())
    }

    pub(crate) fn module_residency_held(&self) -> bool {
        self.residency.lease.lock().is_some()
    }

    /// Publishes the fail-safe terminal state. A quarantined runtime rejects
    /// new opens and calls while retaining the module residency lease and any
    /// resources whose destruction was not proven safe.
    pub(crate) fn quarantine(&self) {
        let mut control = self.lifecycle.lock();
        self.return_protocol.close_admission();
        self.lifecycle
            .set_state(&mut control, LifecycleStateKind::Quarantined);
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
        root: OpenGeneration<A>,
        reason: QuarantineReason,
    ) {
        self.quarantine.retain_generation(generation, root, reason);
    }

    pub(crate) fn quarantine_shared_generation(
        &self,
        generation: Option<RuntimeGeneration>,
        root: Arc<OpenGeneration<A>>,
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
        match opening {
            OpeningGeneration::SharedStateOnly { shared_state, .. } => {
                self.quarantine_shared_state(generation, shared_state, reason);
            }
            OpeningGeneration::Ready {
                shared_state,
                layers,
                config: _,
            } => {
                if let Some(id) = generation {
                    self.quarantine.retain_generation(
                        Some(id),
                        OpenGeneration {
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
        }
    }

    pub(crate) fn quarantine_snapshot(&self) -> Vec<(Option<RuntimeGeneration>, QuarantineReason)> {
        self.quarantine.snapshot()
    }

    pub(crate) fn generation(&self) -> Option<RuntimeGeneration> {
        self.lifecycle.lock().known_generation
    }

    pub(crate) fn active_generation(&self) -> Option<RuntimeGeneration> {
        let control = self.lifecycle.lock();
        control
            .canonical_state()
            .open_attempt()
            .map(OpenAttemptId::into_runtime_generation)
            .or(control.known_generation)
    }

    #[cfg(any(test, feature = "shutdown-refinement"))]
    pub(crate) fn open_attempt(&self) -> Option<OpenAttemptId> {
        self.lifecycle.lock().canonical_state().open_attempt()
    }

    pub(crate) fn begin_open_if_epoch(
        &self,
        expected_removal_epoch: RemovalEpoch,
    ) -> XllResult<OpenAttemptGuard<'_, A>> {
        #[cfg(test)]
        let test_module_lease = crate::ingress::acquire_test_module_lease();
        let mut control = self.lifecycle.lock();
        if control.removal_epoch != expected_removal_epoch.get()
            || control.canonical_state().phase() != LifecyclePhase::Closed
            || control.canonical_state().open_attempt().is_some()
            || control.removal_attempt_active
        {
            return Err(XllError::Internal {
                diagnostic_id: crate::error::DiagnosticId::OPEN_PHASE,
            });
        }

        self.lifecycle
            .set_host_intent_locked(&mut control, HostLifecycleIntent::None);
        let attempt_id = self.lifecycle.next_lifecycle_attempt_id(&mut control)?;
        self.return_protocol.reopen_admission()?;

        crate::rtd::begin_module_open();
        crate::callback_gate::reset_from_runtime();
        crate::ingress::global_ingress().begin_opening();
        #[cfg(test)]
        {
            *self.lifecycle.test_module_lease.lock() = Some(test_module_lease);
        }
        self.lifecycle.set_state(
            &mut control,
            LifecycleStateKind::Opening {
                attempt: attempt_id,
            },
        );
        self.refinement
            .begin_open(self, expected_removal_epoch.get(), attempt_id);
        Ok(OpenAttemptGuard {
            runtime: self,
            attempt_id,
            active: true,
        })
    }

    #[cfg(test)]
    pub(crate) fn begin_open(&self) -> XllResult<OpenAttemptGuard<'_, A>> {
        self.begin_open_if_epoch(self.removal_epoch())
    }

    pub(crate) fn removal_epoch(&self) -> RemovalEpoch {
        RemovalEpoch::new(self.lifecycle.lock().removal_epoch)
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
            .bind_lifecycle_thread()
            .expect("test runtime binds its lifecycle thread");
        if self.with_lifecycle_state(&access, |_| ()).is_err() {
            assert!(
                self.install_lifecycle_state(&access, lifecycle_state)
                    .is_ok(),
                "test runtime has one lifecycle state"
            );
        }
        *self.lifecycle.opening.lock() = Some(OpeningGeneration::Ready {
            shared_state: state,
            layers,
            config: crate::addin::RuntimeConfig::new(),
        });
    }

    pub(crate) fn stage_opening_state(
        &self,
        state: A::SharedState,
        config: crate::addin::RuntimeConfig,
    ) -> Result<(), (XllError, A::SharedState)> {
        self.lifecycle.stage_opening_state(state, config)
    }

    pub(crate) fn bind_lifecycle_thread(
        &self,
    ) -> Result<LifecycleThreadAccess<'_, A>, ThreadAffineError> {
        self.lifecycle_state.bind_current()
    }

    pub(crate) fn install_lifecycle_state(
        &self,
        access: &LifecycleThreadAccess<'_, A>,
        state: A::LifecycleState,
    ) -> Result<(), ThreadAffineInstallError<A::LifecycleState>> {
        self.lifecycle_state.install(access, state)
    }

    pub(crate) fn with_lifecycle_state<R>(
        &self,
        access: &LifecycleThreadAccess<'_, A>,
        operation: impl FnOnce(&mut A::LifecycleState) -> R,
    ) -> Result<R, ThreadAffineError> {
        self.lifecycle_state.with_mut(access, operation)
    }

    pub(crate) fn has_lifecycle_state(
        &self,
        access: &LifecycleThreadAccess<'_, A>,
    ) -> Result<bool, ThreadAffineError> {
        self.lifecycle_state.has_value(access)
    }

    pub(crate) fn take_lifecycle_state(
        &self,
        access: &LifecycleThreadAccess<'_, A>,
    ) -> Result<A::LifecycleState, ThreadAffineError> {
        self.lifecycle_state.take(access)
    }

    pub(crate) fn release_empty_lifecycle_binding(
        &self,
        access: &LifecycleThreadAccess<'_, A>,
    ) -> Result<(), ThreadAffineError> {
        self.lifecycle_state.release_empty_binding(access)
    }

    pub(crate) fn restore_opening_generation(
        &self,
        opening: OpeningGeneration<A>,
    ) -> Result<(), (XllError, OpeningGeneration<A>)> {
        self.lifecycle.restore_opening_generation(opening)
    }

    pub(crate) fn publish_opening_generation(&self, attempt_id: OpenAttemptId) -> XllResult<()> {
        let generation = attempt_id.into_runtime_generation();
        let config = self.lifecycle.opening_config().ok_or(XllError::Internal {
            diagnostic_id: crate::error::DiagnosticId::OPEN_STATE,
        })?;
        let armed_services = self
            .generation_services
            .arm_generation(generation, config)?;
        if let Err(failure) = self.lifecycle.publish_opening_generation(generation) {
            armed_services.rollback();
            if let Some(opening) = failure.opening {
                self.quarantine_opening_generation(
                    Some(generation),
                    opening,
                    QuarantineReason::OpenStateInvariant,
                );
            }
            return Err(failure.error);
        }
        armed_services.commit();
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
        self.generation_services
            .arm_generation(
                crate::generation::RuntimeGeneration::new(1).expect("test generation is non-zero"),
                crate::addin::RuntimeConfig::new(),
            )
            .expect("test runtime generation can be armed once")
            .commit();
    }

    pub(crate) fn take_opening_generation(&self) -> Option<OpeningGeneration<A>> {
        self.lifecycle.take_opening_generation()
    }

    #[cfg(test)]
    pub(crate) fn take_current_generation(&self) -> Option<Arc<OpenGeneration<A>>> {
        self.lifecycle.take_current_generation()
    }

    pub(crate) fn take_generation_for_shutdown(&self) -> Option<ShutdownGeneration<A>> {
        self.lifecycle.take_generation_for_shutdown()
    }

    #[cfg(any(test, feature = "bench-internals"))]
    pub(crate) fn finish_open(
        &self,
        attempt: &mut OpenAttemptGuard<'_, A>,
        registrations: Vec<RegistrationId>,
    ) -> XllResult<()> {
        let mut registrations = registrations;
        self.finish_open_with_registrations(attempt, &mut registrations)
    }

    pub(crate) fn finish_open_with_registrations(
        &self,
        attempt: &mut OpenAttemptGuard<'_, A>,
        registrations: &mut Vec<RegistrationId>,
    ) -> XllResult<()> {
        let mut control = self.lifecycle.lock();
        self.lifecycle.notify_all();
        if control.canonical_state().open_attempt() != Some(attempt.attempt_id) {
            attempt.active = false;
            return Err(XllError::Closing);
        }

        // Once this attempt owns the lifecycle slot, retain every host
        // registration even when a concurrent close has already won the phase
        // transition. The close owner needs those IDs to unregister the host
        // mutations before publishing Closed.
        self.clear_metadata_debt_for_registrations(registrations);
        let new_items: Vec<_> = std::mem::take(registrations)
            .into_iter()
            .map(crate::registration::PendingRegistration::from)
            .collect();
        self.host.append_registrations(new_items);
        let can_commit = control.canonical_state().phase() == LifecyclePhase::Opening;
        if can_commit {
            let ingress = crate::ingress::global_ingress();
            ingress
                .complete_open(|| {
                    self.publish_opening_generation(attempt.attempt_id)?;
                    let generation = attempt.attempt_id.into_runtime_generation();
                    self.refinement.commit_open(self, attempt.attempt_id, || {
                        self.lifecycle
                            .set_known_generation(&mut control, Some(generation));
                        self.lifecycle
                            .set_state(&mut control, LifecycleStateKind::Open { generation });
                        attempt.active = false;
                        debug_assert_eq!(control.canonical_state().phase(), LifecyclePhase::Open);
                        debug_assert_eq!(control.known_generation, Some(generation));
                        debug_assert_eq!(control.canonical_state().open_attempt(), None);
                        Ok(())
                    })?;
                    Ok::<(), XllError>(())
                })
                .unwrap_or_else(|_| opening_publication_lost())?;
        }

        if !can_commit {
            self.reject_open_attempt(&mut control, attempt);
            self.refinement.reject_open(self, attempt.attempt_id);
        }

        if can_commit {
            Ok(())
        } else {
            Err(XllError::Closing)
        }
    }

    #[cfg(feature = "async")]
    pub(crate) fn set_event_registrations(
        &self,
        registrations: Vec<crate::registration::EventRegistration>,
    ) {
        self.host.append_event_registrations(registrations);
    }

    pub(crate) fn retain_registration_debt(
        &self,
        registrations: Vec<crate::registration::PendingRegistration>,
    ) {
        self.host.append_registrations(registrations);
    }

    pub(crate) fn retain_event_registration_debt(
        &self,
        registrations: Vec<crate::registration::EventRegistration>,
    ) {
        self.host.append_event_registrations(registrations);
    }

    pub(crate) fn mark_registration_state_unknown(&self) {
        self.host.mark_registration_state_unknown();
    }

    pub(crate) fn registration_state_unknown(&self) -> bool {
        self.host.registration_state_unknown()
    }

    fn reject_open_attempt(
        &self,
        control: &mut crate::runtime_components::LifecycleControl,
        attempt: &mut OpenAttemptGuard<'_, A>,
    ) {
        let state = match control.canonical_state().phase() {
            LifecyclePhase::Closing => LifecycleStateKind::Closing {
                generation: control.known_generation,
                open_attempt: None,
            },
            LifecyclePhase::OpenRollbackPending => LifecycleStateKind::OpenRollbackPending {
                generation: control.known_generation,
            },
            LifecyclePhase::Quarantined => LifecycleStateKind::Quarantined,
            _ => LifecycleStateKind::Closed,
        };
        self.lifecycle.set_state(control, state);
        attempt.active = false;
    }

    fn fail_and_record(&self, attempt_id: OpenAttemptId) -> bool {
        let mut control = self.lifecycle.lock();
        if control.canonical_state().open_attempt() != Some(attempt_id) {
            return false;
        }

        let should_rollback = match control.canonical_state().phase() {
            LifecyclePhase::Opening => {
                self.return_protocol.close_admission();
                let generation = control.known_generation;
                self.lifecycle.set_state(
                    &mut control,
                    LifecycleStateKind::OpenRollbackPending { generation },
                );
                true
            }
            LifecyclePhase::OpenRollbackPending => true,
            LifecyclePhase::Closing => {
                let generation = control.known_generation;
                self.lifecycle.set_state(
                    &mut control,
                    LifecycleStateKind::Closing {
                        generation,
                        open_attempt: None,
                    },
                );
                false
            }
            LifecyclePhase::Closed | LifecyclePhase::Open | LifecyclePhase::Quarantined => false,
        };
        self.refinement.fail_open(self, attempt_id);
        self.lifecycle.notify_all();
        should_rollback
    }

    pub fn enter(&self) -> XllResult<CallGuard<'_, A>> {
        crate::ingress::global_ingress().with_linearization(|| {
            if self.phase() != LifecyclePhase::Open {
                return Err(XllError::Closing);
            }

            let generation = self.lifecycle.current.load();
            if generation.is_none() {
                return Err(XllError::Internal {
                    diagnostic_id: crate::error::DiagnosticId::MISSING_STATE,
                });
            }
            #[cfg(any(test, feature = "shutdown-refinement"))]
            self.record_ghost_event(crate::shutdown_refinement::GhostEvent::EnterCall);
            Ok(CallGuard {
                #[cfg(any(test, feature = "shutdown-refinement"))]
                runtime: self,
                #[cfg(not(any(test, feature = "shutdown-refinement")))]
                _runtime: std::marker::PhantomData,
                generation,
            })
        })
    }

    #[cfg(test)]
    pub(crate) fn begin_close(&self) -> bool {
        let mut control = self.lifecycle.lock();
        crate::ingress::global_ingress().with_linearization(|| {
            if matches!(
                control.canonical_state().phase(),
                LifecyclePhase::Opening | LifecyclePhase::Open
            ) {
                self.return_protocol.close_admission();
                let generation = control.known_generation;
                let open_attempt = control.canonical_state().open_attempt();
                self.lifecycle.set_state(
                    &mut control,
                    LifecycleStateKind::Closing {
                        generation,
                        open_attempt,
                    },
                );
                true
            } else {
                false
            }
        })
    }

    pub(crate) fn begin_final_removal(&self) -> Option<RemovalAttemptGuard<'_, A>> {
        let mut wait_guard = self.lifecycle.lock();
        // Every final-close invocation invalidates open operations that started
        // before it, including an operation that is between rollback recovery
        // and acquisition of its open-attempt token while the phase is Closed.
        // This epoch is deliberately not part of TerminalCertificate: a waiting
        // final-close caller may advance it while the active owner finishes.
        self.lifecycle.advance_removal_epoch(&mut wait_guard);
        self.return_protocol.close_admission();
        let mut request_recorded = false;
        loop {
            let decision = crate::ingress::global_ingress().with_linearization(|| {
                match wait_guard.canonical_state().phase() {
                    LifecyclePhase::Closed => {
                        // A cleanup owner publishes Closed before its guard leaves
                        // the callback stack. A concurrent explicit removal must
                        // not return until that owner has fully exited, because
                        // the host may immediately continue with residency release.
                        if !wait_guard.removal_attempt_active && self.returns_are_quiescent() {
                            self.refinement
                                .request_final_close(self, &mut request_recorded);
                            return Some(false);
                        }
                        if !wait_guard.removal_attempt_active {
                            let generation = wait_guard.known_generation;
                            let open_attempt = wait_guard.canonical_state().open_attempt();
                            self.lifecycle.set_state(
                                &mut wait_guard,
                                LifecycleStateKind::Closing {
                                    generation,
                                    open_attempt,
                                },
                            );
                        }
                    }
                    LifecyclePhase::Closing => {}
                    LifecyclePhase::Opening
                    | LifecyclePhase::Open
                    | LifecyclePhase::OpenRollbackPending => {
                        let generation = wait_guard.known_generation;
                        let open_attempt = wait_guard.canonical_state().open_attempt();
                        self.lifecycle.set_state(
                            &mut wait_guard,
                            LifecycleStateKind::Closing {
                                generation,
                                open_attempt,
                            },
                        );
                    }
                    LifecyclePhase::Quarantined => return Some(false),
                }

                if !request_recorded {
                    debug_assert!(matches!(
                        wait_guard.canonical_state().phase(),
                        LifecyclePhase::Closed | LifecyclePhase::Closing
                    ));
                    self.refinement
                        .request_final_close(self, &mut request_recorded);
                }

                if wait_guard.canonical_state().phase() != LifecyclePhase::Closed
                    && wait_guard.canonical_state().open_attempt().is_none()
                    && !wait_guard.removal_attempt_active
                {
                    self.lifecycle
                        .set_removal_attempt_active(&mut wait_guard, true);
                    self.refinement.acquire_final_close_owner(self);
                    Some(true)
                } else {
                    None
                }
            });
            match decision {
                Some(true) => return Some(RemovalAttemptGuard { runtime: self }),
                Some(false) => return None,
                None => self.lifecycle.wait(&mut wait_guard),
            }
        }
    }

    pub(crate) fn acquire_open_rollback(&self) -> Option<RemovalAttemptGuard<'_, A>> {
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
            if !wait_guard.removal_attempt_active {
                self.lifecycle
                    .set_removal_attempt_active(&mut wait_guard, true);
                self.refinement.acquire_open_rollback_owner(self);
                return Some(RemovalAttemptGuard { runtime: self });
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

    #[cfg(any(test, feature = "shutdown-refinement"))]
    pub(crate) fn record_ghost_event(&self, event: crate::shutdown_refinement::GhostEvent) {
        self.ghost_handle().record_event(event);
    }

    #[cfg(any(test, feature = "shutdown-refinement"))]
    pub(crate) fn record_ghost_event_linearized(
        &self,
        event: crate::shutdown_refinement::GhostEvent,
    ) {
        crate::ingress::global_ingress().with_linearization(|| self.record_ghost_event(event));
    }

    #[cfg(any(test, feature = "shutdown-refinement"))]
    pub(crate) fn record_ghost_generation_unique(&self) {
        self.record_ghost_event(crate::shutdown_refinement::GhostEvent::ProveGenerationUnique);
    }

    #[cfg(any(test, feature = "shutdown-refinement"))]
    pub(crate) fn record_ghost_addin_quiesced(&self) {
        self.record_ghost_event(crate::shutdown_refinement::GhostEvent::ProveAddinQuiesced);
    }

    #[cfg(any(test, feature = "shutdown-refinement"))]
    pub(crate) fn record_ghost_async_stopped(&self) {
        let ghost = self.ghost_handle();
        if ghost.state().resources.async_executor_running {
            self.record_ghost_event(crate::shutdown_refinement::GhostEvent::StopAsyncExecutor);
        }
    }

    #[cfg(any(test, feature = "shutdown-refinement"))]
    pub(crate) fn record_ghost_diagnostics_stopped(&self) -> XllResult<()> {
        crate::diagnostics::record_ghost_diagnostics_stopped(self.ghost_handle())
    }

    #[cfg(any(test, feature = "shutdown-refinement"))]
    pub(crate) fn ghost_fail_stop(&self, reason: crate::shutdown_refinement::GhostFailure) {
        if let Err(violation) = self.ghost_handle().fail_stop(reason) {
            tracing::error!(%violation, "shutdown ghost fail-stop recording failed");
        }
    }

    #[cfg(any(test, feature = "shutdown-refinement"))]
    pub(crate) fn ghost_quarantine(&self, reason: crate::shutdown_refinement::GhostFailure) {
        if let Err(violation) = self.ghost_handle().quarantine(reason) {
            tracing::error!(%violation, "shutdown ghost quarantine recording failed");
        }
    }

    #[cfg(any(test, feature = "shutdown-refinement"))]
    pub(crate) fn ghost_generation_active(&self) -> bool {
        self.ghost_handle().active()
    }

    #[cfg(test)]
    pub(crate) fn disable_ghost_for_test(&self) {
        self.ghost_handle().disable_for_test();
    }

    #[cfg(any(test, feature = "shutdown-refinement"))]
    pub(crate) fn record_ghost_returned_success(&self, witness: ClosedWitness) -> XllResult<()> {
        if witness.runtime_address != std::ptr::from_ref(self).addr()
            || witness.generation != self.generation()
            || self.phase() != LifecyclePhase::Closed
        {
            return Err(XllError::Internal {
                diagnostic_id: crate::error::DiagnosticId::CLOSE_WAIT,
            });
        }
        if self.ghost_handle().active() {
            self.ghost_handle()
                .record_returned_success()
                .map_err(|_| XllError::Internal {
                    diagnostic_id: crate::error::DiagnosticId::CLOSE_RTD_SUBSCRIPTION,
                })?;
            debug_assert!(!self.ghost_handle().active());
            self.refinement.retire_committed_shutdown(self);
        }
        self.mark_composition_return_pending();
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn ghost_trace_json(&self) -> String {
        self.ghost_handle()
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
    pub(crate) handles_quiescent: crate::shutdown::HandlesQuiescent,
    pub(crate) diagnostics_stopped: crate::diagnostics::DiagnosticsStopped,
    pub(crate) addin_quiesced: crate::shutdown::AddinQuiesced,
    pub(crate) generation_reclaimed: crate::shutdown::GenerationReclaimed,
}

pub(crate) struct FinalRemoval;
pub(crate) struct OpenRollback;

pub(crate) trait TerminalCertificateKind {
    fn accepts_phase(phase: LifecyclePhase) -> bool;
    fn error() -> XllError;
}

impl TerminalCertificateKind for FinalRemoval {
    fn accepts_phase(phase: LifecyclePhase) -> bool {
        phase == LifecyclePhase::Closing
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

    fn error() -> XllError {
        XllError::Internal {
            diagnostic_id: crate::error::DiagnosticId::OPEN_ROLLBACK_CERTIFICATE,
        }
    }
}

#[derive(Debug)]
pub(crate) struct TerminalCertificate<K> {
    #[allow(
        dead_code,
        reason = "linear proof tokens are consumed by terminal transitions"
    )]
    pub(crate) proof: QuiescenceProof,
    #[cfg(any(test, feature = "shutdown-refinement"))]
    pub(crate) composition_resources: crate::shutdown_refinement::GhostResources,
    pub(crate) runtime_address: usize,
    pub(crate) generation: Option<RuntimeGeneration>,
    pub(crate) _kind: std::marker::PhantomData<K>,
}

#[derive(Debug)]
pub(crate) struct ClosedWitness {
    #[cfg(any(test, feature = "shutdown-refinement"))]
    runtime_address: usize,
    #[cfg(any(test, feature = "shutdown-refinement"))]
    generation: Option<RuntimeGeneration>,
}

#[cfg(any(test, feature = "shutdown-refinement"))]
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
        &proof.handles_quiescent,
        &proof.diagnostics_stopped,
        &proof.addin_quiesced,
        &proof.generation_reclaimed,
    );
    crate::shutdown_refinement::GhostResources::quiescent_snapshot()
}

impl<A: crate::Addin> Runtime<A> {
    pub(crate) fn certify<K: TerminalCertificateKind>(
        &self,
        proof: QuiescenceProof,
    ) -> XllResult<TerminalCertificate<K>> {
        let control = self.lifecycle.lock();
        let services_stopped = self.generation_services.handles.is_none()
            && self.generation_services.subscriptions.is_none();
        #[cfg(feature = "async")]
        let async_stopped = self.executors.async_manager.is_stopped();
        #[cfg(not(feature = "async"))]
        let async_stopped = true;
        let handles_match_generation = control
            .known_generation
            .is_none_or(|generation| proof.handles_quiescent.generation() == Some(generation));

        let certified = K::accepts_phase(control.canonical_state().phase())
            && control.canonical_state().open_attempt().is_none()
            && control.removal_attempt_active
            && self.returns_closed_and_quiescent()
            && async_stopped
            && services_stopped
            && self.lifecycle.opening.lock().is_none()
            && self.lifecycle.current.load_full().is_none()
            && self.host.is_quiescent();
        let certified = certified && handles_match_generation;

        if !certified {
            return Err(K::error());
        }

        #[cfg(any(test, feature = "shutdown-refinement"))]
        let composition_resources = composition_resources_from_quiescence_proof(&proof);

        Ok(TerminalCertificate {
            proof,
            #[cfg(any(test, feature = "shutdown-refinement"))]
            composition_resources,
            runtime_address: std::ptr::from_ref(self).addr(),
            generation: control.known_generation,
            _kind: std::marker::PhantomData,
        })
    }

    pub(crate) fn finish_open_rollback(
        &self,
        certificate: TerminalCertificate<OpenRollback>,
    ) -> XllResult<()> {
        if certificate.runtime_address != std::ptr::from_ref(self).addr() {
            return Err(XllError::Internal {
                diagnostic_id: crate::error::DiagnosticId::OPEN_ROLLBACK_CERT_UNKNOWN,
            });
        }
        #[cfg(any(test, feature = "shutdown-refinement"))]
        let composition_resources = certificate.composition_resources;
        let mut control = self.lifecycle.lock();
        debug_assert_eq!(control.canonical_state().open_attempt(), None);
        if !matches!(
            control.canonical_state().phase(),
            LifecyclePhase::OpenRollbackPending | LifecyclePhase::Closing
        ) {
            return Err(XllError::Internal {
                diagnostic_id: crate::error::DiagnosticId::OPEN_ROLLBACK_PHASE,
            });
        }
        crate::callback_gate::close_from_runtime();
        self.lifecycle
            .set_state(&mut control, LifecycleStateKind::Closed);
        #[cfg(any(test, feature = "shutdown-refinement"))]
        self.record_composition_event(
            crate::composition_refinement::CompositionEvent::FinishOpenRollback(
                composition_resources,
            ),
        );
        #[cfg(any(test, feature = "shutdown-refinement"))]
        debug_assert_eq!(self.phase(), LifecyclePhase::Closed);
        #[cfg(any(test, feature = "shutdown-refinement"))]
        self.mark_composition_terminal_pending();
        self.lifecycle.notify_all();
        crate::rtd::certify_logical_quiescence();
        #[cfg(test)]
        drop(self.lifecycle.test_module_lease.lock().take());
        Ok(())
    }

    pub(crate) fn finish_removal(
        &self,
        certificate: TerminalCertificate<FinalRemoval>,
    ) -> XllResult<ClosedWitness> {
        if certificate.runtime_address != std::ptr::from_ref(self).addr() {
            return Err(XllError::Internal {
                diagnostic_id: crate::error::DiagnosticId::CLOSE_RUNTIME,
            });
        }
        if certificate.generation != self.generation() {
            return Err(XllError::Internal {
                diagnostic_id: crate::error::DiagnosticId::CLOSE_LEASE_GATE,
            });
        }
        #[cfg(any(test, feature = "shutdown-refinement"))]
        let composition_resources = certificate.composition_resources;
        let mut control = self.lifecycle.lock();
        #[cfg(any(test, feature = "shutdown-refinement"))]
        let committed = self.ghost_handle().active();
        #[cfg(any(test, feature = "shutdown-refinement"))]
        if committed {
            let event = crate::shutdown_refinement::GhostEvent::FinishClose;
            self.ghost_handle()
                .apply(event.clone())
                .map_err(|_| XllError::Internal {
                    diagnostic_id: crate::error::DiagnosticId::CLOSE_GHOST,
                })?;
            self.record_composition_event(
                crate::composition_refinement::CompositionEvent::FinishCommittedShutdown,
            );
        }
        crate::callback_gate::close_from_runtime();
        self.lifecycle
            .set_state(&mut control, LifecycleStateKind::Closed);
        #[cfg(any(test, feature = "shutdown-refinement"))]
        debug_assert_eq!(self.phase(), LifecyclePhase::Closed);
        #[cfg(any(test, feature = "shutdown-refinement"))]
        if committed {
            self.record_composition_event(
                crate::composition_refinement::CompositionEvent::PublishCommittedClosed,
            );
        }
        #[cfg(any(test, feature = "shutdown-refinement"))]
        {
            if !committed {
                self.record_composition_event(
                    crate::composition_refinement::CompositionEvent::FinishUncommittedFinalClose(
                        composition_resources,
                    ),
                );
            }
        }
        self.lifecycle.notify_all();
        crate::rtd::certify_logical_quiescence();
        #[cfg(test)]
        drop(self.lifecycle.test_module_lease.lock().take());
        Ok(ClosedWitness {
            #[cfg(any(test, feature = "shutdown-refinement"))]
            runtime_address: std::ptr::from_ref(self).addr(),
            #[cfg(any(test, feature = "shutdown-refinement"))]
            generation: certificate.generation,
        })
    }

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
    pub(crate) fn handles(&self) -> XllResult<Arc<crate::handle::HandleRuntime>> {
        self.generation_services.handles.get_owned()
    }

    pub(crate) fn handle_runtime_slot(&self) -> &crate::handle::HandleRuntimeSlot {
        &self.generation_services.handles
    }

    pub(crate) fn seal_handles(&self) -> XllResult<crate::handle::HandleRuntimeSealed> {
        self.generation_services
            .handles
            .seal(self.active_generation())
    }

    pub(crate) fn finish_handle_quiescence(
        &self,
        sealed: crate::handle::HandleRuntimeSealed,
    ) -> XllResult<crate::shutdown::HandlesQuiescent> {
        sealed.finish()
    }

    #[inline]
    pub(crate) fn subscriptions(&self) -> XllResult<crate::subscription::SubscriptionRuntimeRead> {
        self.generation_services.subscriptions.read()
    }

    pub(crate) fn close_subscriptions(&self) -> XllResult<crate::shutdown::SubscriptionsStopped> {
        self.generation_services
            .subscriptions
            .seal(self.active_generation())
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
            let ingress = crate::ingress::global_ingress();
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

pub(crate) struct RemovalAttemptGuard<'runtime, A: crate::Addin> {
    runtime: &'runtime Runtime<A>,
}

impl<A: crate::Addin> Drop for RemovalAttemptGuard<'_, A> {
    fn drop(&mut self) {
        let mut control = self.runtime.lifecycle.lock();
        self.runtime
            .lifecycle
            .set_removal_attempt_active(&mut control, false);
        debug_assert!(!control.removal_attempt_active);
        self.runtime.refinement.release_cleanup_owner(self.runtime);
        self.runtime.lifecycle.notify_all();
    }
}

pub(crate) struct OpenAttemptGuard<'runtime, A: crate::Addin> {
    runtime: &'runtime Runtime<A>,
    attempt_id: OpenAttemptId,
    active: bool,
}

impl<A: crate::Addin> OpenAttemptGuard<'_, A> {
    pub(crate) const fn is_active(&self) -> bool {
        self.active
    }

    pub(crate) const fn attempt_id(&self) -> OpenAttemptId {
        self.attempt_id
    }

    pub(crate) fn fail(&mut self) -> bool {
        if !self.active {
            return false;
        }
        let should_rollback = self.runtime.fail_and_record(self.attempt_id);
        self.active = false;
        should_rollback
    }
}

impl<A: crate::Addin> Drop for OpenAttemptGuard<'_, A> {
    fn drop(&mut self) {
        if self.active {
            // Lifecycle rollback is owned by OpenTxn and must be
            // explicit. A leaked attempt can only enter the fail-safe state;
            // Drop never invokes host callbacks or resource cleanup.
            self.runtime.quarantine();
            self.active = false;
        }
    }
}

pub struct CallGuard<'runtime, A: crate::Addin> {
    #[cfg(any(test, feature = "shutdown-refinement"))]
    runtime: &'runtime Runtime<A>,
    #[cfg(not(any(test, feature = "shutdown-refinement")))]
    _runtime: std::marker::PhantomData<&'runtime Runtime<A>>,
    generation: arc_swap::Guard<Option<Arc<OpenGeneration<A>>>>,
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

    fn generation(&self) -> &OpenGeneration<A> {
        let generation = self
            .generation
            .as_ref()
            .expect("a live CallGuard always observes published runtime generation");
        let _ = generation.id();
        generation
    }

    #[cfg(feature = "async")]
    #[must_use]
    pub(crate) fn lease(&self) -> GenerationLease<A> {
        GenerationLease {
            generation: Arc::clone(
                self.generation
                    .as_ref()
                    .expect("a live CallGuard always observes published runtime generation"),
            ),
        }
    }
}

impl<A: crate::Addin> Drop for CallGuard<'_, A> {
    fn drop(&mut self) {
        #[cfg(any(test, feature = "shutdown-refinement"))]
        self.runtime
            .record_ghost_event(crate::shutdown_refinement::GhostEvent::LeaveCall);
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

    fn finish_test_close<A: crate::Addin>(runtime: &Runtime<A>) {
        let ingress = crate::ingress::global_ingress();
        if matches!(
            ingress.phase(),
            crate::ingress::PHASE_OPENING | crate::ingress::PHASE_OPEN
        ) {
            ingress.begin_close_with(|| {
                #[cfg(any(test, feature = "shutdown-refinement"))]
                if runtime.ghost_generation_active() {
                    runtime.record_ghost_event(crate::shutdown_refinement::GhostEvent::BeginClose);
                }
            });
        }
        let exports = ingress.seal_and_drain();
        let _ = runtime.close_subscriptions();
        let _ = runtime
            .seal_handles()
            .and_then(|sealed| runtime.finish_handle_quiescence(sealed));
        // This helper validates Runtime's close certificate in isolation. It
        // deliberately does not synthesize lifecycle ghost milestones; those
        // are exercised by the real lifecycle close path.
        runtime.disable_ghost_for_test();
        let rtd = crate::rtd::wait_for_module_quiescence().expect("RTD module quiescence");
        let certificate = runtime
            .certify::<FinalRemoval>(QuiescenceProof {
                exports,
                rtd,
                host_callbacks: crate::shutdown::HostCallbacksDetached::for_test(),
                async_stopped: crate::shutdown::AsyncStopped::for_test(),
                subscriptions_stopped: crate::shutdown::SubscriptionsStopped::for_test(),
                handles_quiescent: crate::shutdown::HandlesQuiescent::for_test(Some(
                    crate::generation::RuntimeGeneration::new(1).unwrap(),
                )),
                diagnostics_stopped: crate::diagnostics::DiagnosticsStopped::for_test(),
                addin_quiesced: crate::shutdown::AddinQuiesced::for_test(),
                generation_reclaimed: crate::shutdown::GenerationReclaimed::for_test(),
            })
            .unwrap();
        runtime.finish_removal(certificate).unwrap();
        runtime.release_test_module_lease();
    }

    fn finish_test_open_rollback<A: crate::Addin>(runtime: &Runtime<A>) {
        let ingress = crate::ingress::global_ingress();
        if matches!(
            ingress.phase(),
            crate::ingress::PHASE_OPENING | crate::ingress::PHASE_OPEN
        ) {
            ingress.begin_close_with(|| {});
        }
        let exports = ingress.seal_and_drain();
        let certificate = runtime
            .certify::<OpenRollback>(QuiescenceProof {
                exports,
                rtd: crate::rtd::wait_for_module_quiescence().expect("RTD module quiescence"),
                host_callbacks: crate::shutdown::HostCallbacksDetached::for_test(),
                async_stopped: crate::shutdown::AsyncStopped::for_test(),
                subscriptions_stopped: crate::shutdown::SubscriptionsStopped::for_test(),
                handles_quiescent: crate::shutdown::HandlesQuiescent::for_test(Some(
                    crate::generation::RuntimeGeneration::new(1).unwrap(),
                )),
                diagnostics_stopped: crate::diagnostics::DiagnosticsStopped::for_test(),
                addin_quiesced: crate::shutdown::AddinQuiesced::for_test(),
                generation_reclaimed: crate::shutdown::GenerationReclaimed::for_test(),
            })
            .unwrap();
        runtime.finish_open_rollback(certificate).unwrap();
        runtime.release_test_module_lease();
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
        assert_eq!(runtime.enter().unwrap().state(), &1);
        let old_handles = runtime.handles().unwrap();
        let (old_token, _) = old_handles
            .prepare(crate::handle::test_topic_key("old"), || Ok(TestHandle(1)))
            .unwrap();

        let removal_attempt = runtime.begin_final_removal().unwrap();
        runtime
            .seal_handles()
            .and_then(|sealed| runtime.finish_handle_quiescence(sealed))
            .unwrap();
        assert_eq!(runtime.take_current_generation().unwrap().shared_state, 1);
        finish_test_close(&runtime);
        drop(removal_attempt);

        let mut open_attempt = runtime.begin_open().unwrap();
        runtime.publish(2_u32, ());
        runtime.finish_open(&mut open_attempt, Vec::new()).unwrap();
        assert_eq!(runtime.enter().unwrap().state(), &2);
        let new_handles = runtime.handles().unwrap();
        let (new_token, _) = new_handles
            .prepare(crate::handle::test_topic_key("new"), || Ok(TestHandle(2)))
            .unwrap();
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
        assert_eq!(runtime.enter().unwrap().state(), &11);
        let _close = runtime.begin_final_removal();
        let _ = runtime.take_current_generation();
        finish_test_close(&runtime);
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
            let _close = closing_runtime
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
            finish_test_close(&closing_runtime);
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
        assert!(!opening.is_active());

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
        runtime
            .seal_handles()
            .and_then(|sealed| runtime.finish_handle_quiescence(sealed))
            .unwrap();
        runtime.close_subscriptions().unwrap();
        assert!(runtime.take_current_generation().is_some());

        let ingress = crate::ingress::global_ingress();
        ingress.begin_close_with(|| {
            #[cfg(any(test, feature = "shutdown-refinement"))]
            if runtime.ghost_generation_active() {
                runtime.record_ghost_event(crate::shutdown_refinement::GhostEvent::BeginClose);
            }
        });
        let exports = ingress.seal_and_drain();
        runtime.disable_ghost_for_test();
        let rtd = crate::rtd::wait_for_module_quiescence().expect("RTD module quiescence");
        let certificate = runtime
            .certify::<FinalRemoval>(QuiescenceProof {
                exports,
                rtd,
                host_callbacks: crate::shutdown::HostCallbacksDetached::for_test(),
                async_stopped: crate::shutdown::AsyncStopped::for_test(),
                subscriptions_stopped: crate::shutdown::SubscriptionsStopped::for_test(),
                handles_quiescent: crate::shutdown::HandlesQuiescent::for_test(Some(
                    crate::generation::RuntimeGeneration::new(1).unwrap(),
                )),
                diagnostics_stopped: crate::diagnostics::DiagnosticsStopped::for_test(),
                addin_quiesced: crate::shutdown::AddinQuiesced::for_test(),
                generation_reclaimed: crate::shutdown::GenerationReclaimed::for_test(),
            })
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

        runtime.finish_removal(certificate).unwrap();
        drop(removal_attempt);
        waiter.join().unwrap();
        assert_eq!(runtime.phase(), LifecyclePhase::Closed);
    }

    #[test]
    fn close_waiter_is_not_lost_when_open_rollback_finishes() {
        let _test_guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let runtime = Arc::new(Runtime::<()>::new());
        let mut opening = runtime.begin_open().unwrap();
        assert!(opening.fail());
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
        finish_test_open_rollback(&runtime);
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
        finish_test_close(&runtime);
        drop(second);
        assert_eq!(runtime.phase(), LifecyclePhase::Closed);
    }

    #[test]
    fn lifecycle_attempt_counter_refuses_exhaustion() {
        let _test_guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let runtime = Runtime::<()>::new();
        runtime.lifecycle.lock().next_lifecycle_attempt = u64::MAX;
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
        runtime
            .seal_handles()
            .and_then(|sealed| runtime.finish_handle_quiescence(sealed))
            .unwrap();
        runtime.close_subscriptions().unwrap();
        let ingress = crate::ingress::global_ingress();
        ingress.begin_close_with(|| {
            #[cfg(any(test, feature = "shutdown-refinement"))]
            if runtime.ghost_generation_active() {
                runtime.record_ghost_event(crate::shutdown_refinement::GhostEvent::BeginClose);
            }
        });
        let exports = ingress.seal_and_drain();
        let rtd = crate::rtd::wait_for_module_quiescence().expect("RTD module quiescence");
        assert!(
            runtime
                .certify::<FinalRemoval>(QuiescenceProof {
                    exports,
                    rtd,
                    host_callbacks: crate::shutdown::HostCallbacksDetached::for_test(),
                    async_stopped: crate::shutdown::AsyncStopped::for_test(),
                    subscriptions_stopped: crate::shutdown::SubscriptionsStopped::for_test(),
                    handles_quiescent: crate::shutdown::HandlesQuiescent::for_test(Some(
                        crate::generation::RuntimeGeneration::new(1).unwrap()
                    ),),
                    diagnostics_stopped: crate::diagnostics::DiagnosticsStopped::for_test(),
                    addin_quiesced: crate::shutdown::AddinQuiesced::for_test(),
                    generation_reclaimed: crate::shutdown::GenerationReclaimed::for_test(),
                })
                .is_err()
        );
        assert_eq!(runtime.phase(), LifecyclePhase::Closing);

        assert!(runtime.take_current_generation().is_some());
        finish_test_close(&runtime);
        drop(removal_attempt);
        assert_eq!(runtime.phase(), LifecyclePhase::Closed);
    }

    #[test]
    fn close_rejects_new_calls_and_waits_for_existing_call() {
        let _test_guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let runtime = Arc::new(Runtime::<TestU32Addin>::new());
        let mut open_attempt = runtime.begin_open().unwrap();
        runtime.publish(7_u32, ());
        runtime.finish_open(&mut open_attempt, Vec::new()).unwrap();

        let (_export_guard, accepted) = crate::ingress::global_ingress().enter_with(|| {});
        assert!(accepted);
        let guard = runtime.enter().unwrap();
        assert!(runtime.begin_close());
        crate::ingress::global_ingress().begin_close_with(|| {});
        assert!(matches!(runtime.enter(), Err(XllError::Closing)));

        let (sender, receiver) = mpsc::channel();
        let handle = thread::spawn(move || {
            let _ = crate::ingress::global_ingress().seal_and_drain();
            sender.send(()).unwrap();
        });

        assert!(receiver.recv_timeout(Duration::from_millis(20)).is_err());
        drop(guard);
        drop(_export_guard);
        receiver.recv_timeout(Duration::from_secs(1)).unwrap();
        handle.join().unwrap();
    }

    #[test]
    fn registration_storage_is_replaceable() {
        let runtime = Runtime::<()>::new();
        runtime
            .host
            .append_registrations([crate::registration::PendingRegistration::from(
                RegistrationId {
                    id: 1.0,
                    excel_name: "TEST",
                },
            )]);
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

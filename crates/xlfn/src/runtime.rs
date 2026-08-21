use crate::generation::{OpenAttemptId, RemovalEpoch, RuntimeGeneration};
use crate::{RegistrationId, XllError, XllResult};
#[cfg(test)]
use parking_lot::MutexGuard;
use std::collections::BTreeMap;
use std::sync::Arc;
#[cfg(not(feature = "async"))]
use std::sync::atomic::Ordering;

#[cfg(any(test, feature = "shutdown-refinement"))]
use crate::runtime_components::FormalState;
use crate::runtime_components::{
    HostLedger, LifecycleState, LifecycleStateKind, ModuleResidency, QuarantineReason,
    QuarantineVault, ReturnProtocol, RuntimeServices,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum LifecyclePhase {
    Closed = 0,
    Opening = 1,
    Open = 2,
    Closing = 3,
    OpenRollbackPending = 4,
    Quarantined = 5,
}

impl LifecyclePhase {
    pub(crate) fn from_raw(value: u8) -> Self {
        match value {
            0 => Self::Closed,
            1 => Self::Opening,
            2 => Self::Open,
            3 => Self::Closing,
            4 => Self::OpenRollbackPending,
            5 => Self::Quarantined,
            _ => std::process::abort(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum HostLifecycleIntent {
    None = 0,
    ExplicitRemovalRequested = 1,
    ExplicitRemovalComplete = 2,
}

impl HostLifecycleIntent {
    pub(crate) fn from_raw(value: u8) -> Self {
        match value {
            0 => Self::None,
            1 => Self::ExplicitRemovalRequested,
            2 => Self::ExplicitRemovalComplete,
            _ => std::process::abort(),
        }
    }
}

/// The published root of an open Add-in generation.
pub struct OpenGeneration<A: crate::Addin> {
    pub(crate) id: RuntimeGeneration,
    pub(crate) state: A::State,
    pub(crate) layers: A::Layers,
}

impl<A: crate::Addin> OpenGeneration<A> {
    pub(crate) const fn id(&self) -> RuntimeGeneration {
        self.id
    }
}

/// Unique Add-in state staged during `OPENING`.
pub enum OpeningGeneration<A: crate::Addin> {
    StateOnly {
        state: A::State,
        config: crate::RuntimeConfig,
    },
    Ready {
        state: A::State,
        layers: A::Layers,
        config: crate::RuntimeConfig,
    },
}

impl<A: crate::Addin> OpeningGeneration<A> {
    #[must_use]
    pub(crate) fn into_parts(self) -> (A::State, Option<A::Layers>, crate::RuntimeConfig) {
        match self {
            Self::StateOnly { state, config } => (state, None, config),
            Self::Ready {
                state,
                layers,
                config,
            } => (state, Some(layers), config),
        }
    }

    #[must_use]
    pub(crate) fn attach_layers(self, layers: A::Layers) -> Self {
        match self {
            Self::StateOnly { state, config } | Self::Ready { state, config, .. } => Self::Ready {
                state,
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
    pub fn state(&self) -> &A::State {
        &self.generation.state
    }

    #[must_use]
    pub fn layers(&self) -> &A::Layers {
        &self.generation.layers
    }
}

pub struct Runtime<A: crate::Addin> {
    pub(crate) lifecycle: LifecycleState<A>,
    pub(crate) host: HostLedger,
    pub(crate) return_protocol: ReturnProtocol,
    pub(crate) services: RuntimeServices,
    pub(crate) residency: ModuleResidency,
    pub(crate) quarantine: QuarantineVault<A>,
    #[cfg(any(test, feature = "shutdown-refinement"))]
    pub(crate) formal: FormalState,
}

impl<A: crate::Addin> Runtime<A> {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            lifecycle: LifecycleState::new(),
            host: HostLedger::new(),
            return_protocol: ReturnProtocol::new(),
            services: RuntimeServices::new(),
            residency: ModuleResidency::new(),
            quarantine: QuarantineVault::new(),
            #[cfg(any(test, feature = "shutdown-refinement"))]
            formal: FormalState::new(),
        }
    }

    #[cfg(any(test, feature = "shutdown-refinement"))]
    fn ghost_handle(&self) -> crate::shutdown_refinement::GhostHandle {
        Arc::clone(
            self.formal
                .ghost
                .get_or_init(|| Arc::new(crate::shutdown_refinement::ShutdownGhost::new())),
        )
    }

    #[cfg(any(test, feature = "shutdown-refinement"))]
    fn composition_trace(&self) -> &crate::composition_refinement::CompositionTrace {
        let trace = self
            .formal
            .composition
            .get_or_init(|| Arc::new(crate::composition_refinement::CompositionTrace::new()));
        self.ghost_handle().set_composition(Arc::clone(trace));
        trace.as_ref()
    }

    #[cfg(any(test, feature = "shutdown-refinement"))]
    fn record_composition_event(&self, event: crate::composition_refinement::CompositionEvent) {
        self.composition_trace().record(event);
    }

    #[cfg(any(test, feature = "shutdown-refinement"))]
    fn record_composition_begin_open(&self, sampled_epoch: u64, attempt: u64) {
        self.composition_trace().begin_open(sampled_epoch, attempt);
    }

    #[cfg(any(test, feature = "shutdown-refinement"))]
    fn mark_composition_return_pending(&self) {
        self.composition_trace().mark_return_pending();
    }

    #[cfg(any(test, feature = "shutdown-refinement"))]
    fn finish_composition_return(&self) {
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
        self.lifecycle.phase()
    }

    pub(crate) fn host_intent(&self) -> HostLifecycleIntent {
        self.lifecycle.host_intent()
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

    pub(crate) fn quarantine_state(
        &self,
        generation: Option<RuntimeGeneration>,
        state: A::State,
        reason: QuarantineReason,
    ) {
        self.quarantine.retain_state(generation, state, reason);
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
            OpeningGeneration::StateOnly { state, .. } => {
                self.quarantine_state(generation, state, reason);
            }
            OpeningGeneration::Ready {
                state,
                layers,
                config: _,
            } => {
                if let Some(id) = generation {
                    self.quarantine.retain_generation(
                        Some(id),
                        OpenGeneration { id, state, layers },
                        reason,
                    );
                } else {
                    self.quarantine.retain_state(None, state, reason);
                    self.quarantine.retain_layers(None, layers, reason);
                }
            }
        }
    }

    pub(crate) fn quarantine_snapshot(&self) -> Vec<(Option<RuntimeGeneration>, QuarantineReason)> {
        self.quarantine.snapshot()
    }

    pub(crate) fn generation(&self) -> Option<RuntimeGeneration> {
        self.lifecycle.generation()
    }

    pub(crate) fn active_generation(&self) -> Option<RuntimeGeneration> {
        self.lifecycle
            .open_attempt()
            .map(OpenAttemptId::into_runtime_generation)
            .or_else(|| self.generation())
    }

    pub(crate) fn begin_open_if_epoch(
        &self,
        expected_removal_epoch: RemovalEpoch,
    ) -> XllResult<OpenAttemptGuard<'_, A>> {
        #[cfg(test)]
        let test_module_lease = crate::ingress::acquire_test_module_lease();
        let mut control = self.lifecycle.lock();
        if control.removal_epoch != expected_removal_epoch.get()
            || control.state.phase() != LifecyclePhase::Closed
            || control.state.open_attempt().is_some()
            || control.removal_attempt_active
        {
            return Err(XllError::Internal {
                diagnostic_id: crate::DiagnosticId::OPEN_PHASE,
            });
        }

        self.lifecycle
            .set_host_intent_locked(&mut control, HostLifecycleIntent::None);
        let attempt_id = self.lifecycle.next_lifecycle_attempt_id(&mut control)?;
        #[cfg(any(test, feature = "shutdown-refinement"))]
        {
            let ghost = self.ghost_handle();
            if ghost.active() {
                self.return_protocol.returns.set_ghost(ghost);
            }
        }
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
        #[cfg(any(test, feature = "shutdown-refinement"))]
        self.record_composition_begin_open(expected_removal_epoch.get(), attempt_id.get());
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
        RemovalEpoch::new(self.lifecycle.removal_epoch())
    }

    #[cfg(any(test, feature = "bench-internals"))]
    pub(crate) fn publish(&self, state: A::State, layers: A::Layers) {
        *self.lifecycle.opening.lock() = Some(OpeningGeneration::Ready {
            state,
            layers,
            config: crate::RuntimeConfig::new(),
        });
    }

    pub(crate) fn stage_opening_state(
        &self,
        state: A::State,
        config: crate::RuntimeConfig,
    ) -> Result<(), (XllError, A::State)> {
        self.lifecycle.stage_opening_state(state, config)
    }

    pub(crate) fn restore_opening_generation(
        &self,
        opening: OpeningGeneration<A>,
    ) -> Result<(), (XllError, OpeningGeneration<A>)> {
        self.lifecycle.restore_opening_generation(opening)
    }

    pub(crate) fn publish_opening_generation(&self) -> XllResult<()> {
        let attempt_id = self.lifecycle.open_attempt().ok_or(XllError::Internal {
            diagnostic_id: crate::DiagnosticId::OPEN_STATE,
        })?;
        let generation = attempt_id.into_runtime_generation();
        let config = self.lifecycle.opening_config().ok_or(XllError::Internal {
            diagnostic_id: crate::DiagnosticId::OPEN_STATE,
        })?;
        let armed_services = self.services.arm_generation(generation, config)?;
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
        self.services
            .arm_generation(
                crate::generation::RuntimeGeneration::new(1).expect("test generation is non-zero"),
                crate::RuntimeConfig::new(),
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

    pub(crate) fn finish_open(
        &self,
        attempt: &mut OpenAttemptGuard<'_, A>,
        registrations: Vec<RegistrationId>,
    ) -> XllResult<()> {
        let mut control = self.lifecycle.lock();
        self.lifecycle.notify_all();
        if control.state.open_attempt() != Some(attempt.attempt_id) {
            attempt.active = false;
            return Err(XllError::Closing);
        }

        // Once this attempt owns the lifecycle slot, retain every host
        // registration even when a concurrent close has already won the phase
        // transition. The close owner needs those IDs to unregister the host
        // mutations before publishing Closed.
        self.clear_metadata_debt_for_registrations(&registrations);
        let new_items: Vec<_> = registrations
            .into_iter()
            .map(crate::registration::PendingRegistration::from)
            .collect();
        self.host.append_registrations(new_items);
        let can_commit = self.phase() == LifecyclePhase::Opening;
        if can_commit {
            let ingress = crate::ingress::global_ingress();
            #[cfg(any(test, feature = "shutdown-refinement"))]
            {
                let ghost = self.ghost_handle();
                ingress
                    .complete_open(|| {
                        self.publish_opening_generation()?;
                        let mut resources = crate::shutdown_refinement::GhostResources::opened(
                            self.host.registrations_snapshot().len() as u64,
                            self.host.event_registrations_snapshot().len() as u64,
                        );
                        #[cfg(feature = "async")]
                        {
                            resources.async_executor_running =
                                !self.services.async_manager.is_stopped();
                        }
                        crate::diagnostics::connect_ghost(Arc::clone(&ghost), |snapshot| {
                            resources.diagnostics_running = snapshot.running;
                            resources.diagnostics_pending = snapshot.pending;
                            ghost
                                .begin_generation(attempt.attempt_id.get(), resources.clone())
                                .map_err(|_| XllError::Internal {
                                    diagnostic_id: crate::DiagnosticId::GHOST_GENERATION,
                                })?;
                            let generation = attempt.attempt_id.into_runtime_generation();
                            self.lifecycle
                                .set_known_generation(&mut control, Some(generation));
                            self.lifecycle
                                .set_state(&mut control, LifecycleStateKind::Open { generation });
                            attempt.active = false;
                            // The abstract open publication is complete before
                            // any concurrent producer can observe this ghost.
                            // The diagnostic, RTD, return, handle, subscription,
                            // and async hooks are installed only after this event.
                            debug_assert_eq!(self.phase(), LifecyclePhase::Open);
                            debug_assert_eq!(
                                self.generation(),
                                Some(attempt.attempt_id.into_runtime_generation())
                            );
                            debug_assert_eq!(self.lifecycle.open_attempt(), None);
                            self.record_composition_event(
                                crate::composition_refinement::CompositionEvent::CommitOpen {
                                    attempt: attempt.attempt_id.get(),
                                    resources,
                                },
                            );
                            Ok(())
                        })?;
                        crate::rtd::set_ghost(Arc::clone(&ghost));
                        self.return_protocol.returns.set_ghost(Arc::clone(&ghost));
                        self.services.handles.set_ghost(Arc::clone(&ghost));
                        self.services.subscriptions.set_ghost(Arc::clone(&ghost));
                        #[cfg(feature = "async")]
                        self.services.async_manager.set_ghost(Arc::clone(&ghost));
                        Ok::<(), XllError>(())
                    })
                    .unwrap_or_else(|_| opening_publication_lost())?;
            }
            #[cfg(not(any(test, feature = "shutdown-refinement")))]
            ingress
                .complete_open(|| {
                    self.publish_opening_generation()?;
                    let generation = attempt.attempt_id.into_runtime_generation();
                    self.lifecycle
                        .set_known_generation(&mut control, Some(generation));
                    self.lifecycle
                        .set_state(&mut control, LifecycleStateKind::Open { generation });
                    attempt.active = false;
                    Ok::<(), XllError>(())
                })
                .unwrap_or_else(|_| opening_publication_lost())?;
        }

        if !can_commit {
            self.reject_open_attempt(&mut control, attempt);
            #[cfg(any(test, feature = "shutdown-refinement"))]
            self.record_rejected_open(attempt.attempt_id);
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
        let state = match control.state.phase() {
            LifecyclePhase::Closing => LifecycleStateKind::Closing {
                generation: control.known_generation,
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

    #[cfg(any(test, feature = "shutdown-refinement"))]
    fn record_rejected_open(&self, attempt_id: OpenAttemptId) {
        debug_assert_eq!(self.phase(), LifecyclePhase::Closing);
        debug_assert_eq!(self.lifecycle.open_attempt(), None);
        self.record_composition_event(
            crate::composition_refinement::CompositionEvent::FinishOpenRejectedByClose {
                attempt: attempt_id.get(),
            },
        );
    }

    fn fail_and_record(&self, attempt_id: OpenAttemptId) -> bool {
        let mut control = self.lifecycle.lock();
        if control.state.open_attempt() != Some(attempt_id) {
            return false;
        }

        let should_rollback = match control.state.phase() {
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
            LifecyclePhase::Closed
            | LifecyclePhase::Open
            | LifecyclePhase::Closing
            | LifecyclePhase::Quarantined => false,
        };
        #[cfg(any(test, feature = "shutdown-refinement"))]
        {
            debug_assert_eq!(self.lifecycle.open_attempt(), None);
            debug_assert!(matches!(
                self.phase(),
                LifecyclePhase::OpenRollbackPending | LifecyclePhase::Closing
            ));
            self.record_composition_event(
                crate::composition_refinement::CompositionEvent::FailOpen {
                    attempt: attempt_id.get(),
                },
            );
        }
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
                    diagnostic_id: crate::DiagnosticId::MISSING_STATE,
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
                control.state.phase(),
                LifecyclePhase::Opening | LifecyclePhase::Open
            ) {
                self.return_protocol.close_admission();
                let generation = control.known_generation;
                self.lifecycle
                    .set_state(&mut control, LifecycleStateKind::Closing { generation });
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
        // This epoch is deliberately not part of LogicalQuiescenceCertificate: a waiting
        // final-close caller may advance it while the active owner finishes.
        self.lifecycle.advance_removal_epoch(&mut wait_guard);
        self.return_protocol.close_admission();
        #[cfg(any(test, feature = "shutdown-refinement"))]
        let mut request_recorded = false;
        loop {
            let decision = crate::ingress::global_ingress().with_linearization(|| {
                match wait_guard.state.phase() {
                    LifecyclePhase::Closed => {
                        // A cleanup owner publishes Closed before its guard leaves
                        // the callback stack. A concurrent explicit removal must
                        // not return until that owner has fully exited, because
                        // the host may immediately continue with residency release.
                        if !wait_guard.removal_attempt_active && self.returns_are_quiescent()
                        {
                            #[cfg(any(test, feature = "shutdown-refinement"))]
                            if !request_recorded {
                                self.record_composition_event(
                                    crate::composition_refinement::CompositionEvent::RequestFinalClose,
                                );
                                request_recorded = true;
                            }
                            return Some(false);
                        }
                        if !wait_guard.removal_attempt_active {
                            let generation = wait_guard.known_generation;
                            self.lifecycle.set_state(
                                &mut wait_guard,
                                LifecycleStateKind::Closing { generation },
                            );
                        }
                    }
                    LifecyclePhase::Closing => {}
                    LifecyclePhase::Opening
                    | LifecyclePhase::Open
                    | LifecyclePhase::OpenRollbackPending => {
                        let generation = wait_guard.known_generation;
                        self.lifecycle.set_state(
                            &mut wait_guard,
                            LifecycleStateKind::Closing { generation },
                        );
                    }
                    LifecyclePhase::Quarantined => return Some(false),
                }

                #[cfg(any(test, feature = "shutdown-refinement"))]
                if !request_recorded {
                    debug_assert!(matches!(
                        wait_guard.state.phase(),
                        LifecyclePhase::Closed | LifecyclePhase::Closing
                    ));
                    self.record_composition_event(
                        crate::composition_refinement::CompositionEvent::RequestFinalClose,
                    );
                    request_recorded = true;
                }

                if wait_guard.state.phase() != LifecyclePhase::Closed
                    && wait_guard.state.open_attempt().is_none()
                    && !wait_guard.removal_attempt_active
                {
                    self.lifecycle
                        .set_removal_attempt_active(&mut wait_guard, true);
                    #[cfg(any(test, feature = "shutdown-refinement"))]
                    self.record_composition_event(
                        crate::composition_refinement::CompositionEvent::AcquireFinalCloseOwner,
                    );
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
            match wait_guard.state.phase() {
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
                #[cfg(any(test, feature = "shutdown-refinement"))]
                self.record_composition_event(
                    crate::composition_refinement::CompositionEvent::AcquireOpenRollbackOwner,
                );
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
                diagnostic_id: crate::DiagnosticId::CLOSE_WAIT,
            });
        }
        if self.ghost_handle().active() {
            self.ghost_handle()
                .record_returned_success()
                .map_err(|_| XllError::Internal {
                    diagnostic_id: crate::DiagnosticId::CLOSE_RTD_SUBSCRIPTION,
                })?;
            debug_assert!(!self.ghost_handle().active());
            self.record_composition_event(
                crate::composition_refinement::CompositionEvent::RetireCommittedShutdown,
            );
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
pub struct LogicalQuiescenceCertificate {
    #[allow(
        dead_code,
        reason = "Typestate proof token for linear lifecycle release"
    )]
    pub(crate) exports: crate::ingress::ExportsDrained,
    #[allow(
        dead_code,
        reason = "Typestate proof token for linear lifecycle release"
    )]
    pub(crate) rtd: crate::rtd::RtdQuiescent,
    #[allow(
        dead_code,
        reason = "Typestate proof token for linear lifecycle release"
    )]
    pub(crate) host_callbacks: crate::shutdown::HostCallbacksDetached,
    #[allow(
        dead_code,
        reason = "Typestate proof token for linear lifecycle release"
    )]
    pub(crate) async_stopped: crate::shutdown::AsyncStopped,
    #[allow(
        dead_code,
        reason = "Typestate proof token for linear lifecycle release"
    )]
    pub(crate) subscriptions_stopped: crate::shutdown::SubscriptionsStopped,
    #[allow(
        dead_code,
        reason = "Typestate proof token for linear lifecycle release"
    )]
    pub(crate) handles_quiescent: crate::shutdown::HandlesQuiescent,
    #[allow(
        dead_code,
        reason = "Typestate proof token for linear lifecycle release"
    )]
    pub(crate) diagnostics_stopped: crate::diagnostics::DiagnosticsStopped,
    #[allow(
        dead_code,
        reason = "Typestate proof token for linear lifecycle release"
    )]
    pub(crate) addin_quiesced: crate::shutdown::AddinQuiesced,
    #[allow(
        dead_code,
        reason = "Typestate proof token for linear lifecycle release"
    )]
    pub(crate) generation_reclaimed: crate::shutdown::GenerationReclaimed,
    #[cfg(any(test, feature = "shutdown-refinement"))]
    composition_resources: crate::shutdown_refinement::GhostResources,
    runtime_address: usize,
    generation: Option<RuntimeGeneration>,
}

#[derive(Debug)]
pub(crate) struct ClosedWitness {
    #[cfg(any(test, feature = "shutdown-refinement"))]
    runtime_address: usize,
    #[cfg(any(test, feature = "shutdown-refinement"))]
    generation: Option<RuntimeGeneration>,
}

#[derive(Debug)]
pub(crate) struct OpenRollbackCertificate {
    #[allow(
        dead_code,
        reason = "Typestate proof token for linear lifecycle release"
    )]
    pub(crate) exports: crate::ingress::ExportsDrained,
    #[allow(
        dead_code,
        reason = "Typestate proof token for linear lifecycle release"
    )]
    pub(crate) rtd: crate::rtd::RtdQuiescent,
    #[allow(
        dead_code,
        reason = "Typestate proof token for linear lifecycle release"
    )]
    pub(crate) host_callbacks: crate::shutdown::HostCallbacksDetached,
    #[allow(
        dead_code,
        reason = "Typestate proof token for linear lifecycle release"
    )]
    pub(crate) async_stopped: crate::shutdown::AsyncStopped,
    #[allow(
        dead_code,
        reason = "Typestate proof token for linear lifecycle release"
    )]
    pub(crate) subscriptions_stopped: crate::shutdown::SubscriptionsStopped,
    #[allow(
        dead_code,
        reason = "Typestate proof token for linear lifecycle release"
    )]
    pub(crate) handles_quiescent: crate::shutdown::HandlesQuiescent,
    #[allow(
        dead_code,
        reason = "Typestate proof token for linear lifecycle release"
    )]
    pub(crate) diagnostics_stopped: crate::diagnostics::DiagnosticsStopped,
    #[allow(
        dead_code,
        reason = "Typestate proof token for linear lifecycle release"
    )]
    pub(crate) addin_quiesced: crate::shutdown::AddinQuiesced,
    #[allow(
        dead_code,
        reason = "Typestate proof token for linear lifecycle release"
    )]
    pub(crate) generation_reclaimed: crate::shutdown::GenerationReclaimed,
    #[cfg(any(test, feature = "shutdown-refinement"))]
    composition_resources: crate::shutdown_refinement::GhostResources,
    runtime_address: usize,
}

pub(crate) struct RemovalQuiescencePrerequisites {
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

pub(crate) struct OpenRollbackQuiescencePrerequisites {
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

#[cfg(any(test, feature = "shutdown-refinement"))]
fn composition_resources_from_close_prerequisites(
    prerequisites: &RemovalQuiescencePrerequisites,
) -> crate::shutdown_refinement::GhostResources {
    // These linear tokens are the concrete proof that every resource family
    // represented by the abstract snapshot has drained. Keep this projection
    // at certificate issuance so finish events cannot observe a later ad-hoc
    // runtime snapshot.
    let _proofs = (
        &prerequisites.exports,
        &prerequisites.rtd,
        &prerequisites.host_callbacks,
        &prerequisites.async_stopped,
        &prerequisites.subscriptions_stopped,
        &prerequisites.handles_quiescent,
        &prerequisites.diagnostics_stopped,
        &prerequisites.addin_quiesced,
        &prerequisites.generation_reclaimed,
    );
    crate::shutdown_refinement::GhostResources::quiescent_snapshot()
}

#[cfg(any(test, feature = "shutdown-refinement"))]
fn composition_resources_from_open_rollback_prerequisites(
    prerequisites: &OpenRollbackQuiescencePrerequisites,
) -> crate::shutdown_refinement::GhostResources {
    // Rollback uses the same quiescence certificate boundary as final close;
    // the snapshot is fixed while these linear proof tokens are consumed into
    // the certificate.
    let _proofs = (
        &prerequisites.exports,
        &prerequisites.rtd,
        &prerequisites.host_callbacks,
        &prerequisites.async_stopped,
        &prerequisites.subscriptions_stopped,
        &prerequisites.handles_quiescent,
        &prerequisites.diagnostics_stopped,
        &prerequisites.addin_quiesced,
        &prerequisites.generation_reclaimed,
    );
    crate::shutdown_refinement::GhostResources::quiescent_snapshot()
}

impl<A: crate::Addin> Runtime<A> {
    pub(crate) fn certify_open_rollback(
        &self,
        prerequisites: OpenRollbackQuiescencePrerequisites,
    ) -> XllResult<OpenRollbackCertificate> {
        let control = self.lifecycle.lock();
        let services_stopped =
            self.services.handles.is_none() && self.services.subscriptions.is_none();
        #[cfg(feature = "async")]
        let async_stopped = self.services.async_manager.is_stopped();
        #[cfg(not(feature = "async"))]
        let async_stopped = true;
        let handles_match_generation = control.known_generation.is_none_or(|generation| {
            prerequisites.handles_quiescent.generation() == Some(generation)
        });

        let certified = matches!(
            control.state.phase(),
            LifecyclePhase::OpenRollbackPending | LifecyclePhase::Closing
        ) && control.state.open_attempt().is_none()
            && control.removal_attempt_active
            && self.returns_closed_and_quiescent()
            && async_stopped
            && services_stopped
            && self.lifecycle.opening.lock().is_none()
            && self.lifecycle.current.load_full().is_none()
            && self.host.registrations_empty()
            && self.host.event_registrations_empty()
            && !self.registration_state_unknown();
        let certified = certified && handles_match_generation;

        if !certified {
            return Err(XllError::Internal {
                diagnostic_id: crate::DiagnosticId::OPEN_ROLLBACK_CERTIFICATE,
            });
        }

        #[cfg(any(test, feature = "shutdown-refinement"))]
        let composition_resources =
            composition_resources_from_open_rollback_prerequisites(&prerequisites);

        Ok(OpenRollbackCertificate {
            exports: prerequisites.exports,
            rtd: prerequisites.rtd,
            host_callbacks: prerequisites.host_callbacks,
            async_stopped: prerequisites.async_stopped,
            subscriptions_stopped: prerequisites.subscriptions_stopped,
            handles_quiescent: prerequisites.handles_quiescent,
            diagnostics_stopped: prerequisites.diagnostics_stopped,
            addin_quiesced: prerequisites.addin_quiesced,
            generation_reclaimed: prerequisites.generation_reclaimed,
            #[cfg(any(test, feature = "shutdown-refinement"))]
            composition_resources,
            runtime_address: std::ptr::from_ref(self).addr(),
        })
    }

    pub(crate) fn finish_open_rollback(
        &self,
        certificate: OpenRollbackCertificate,
    ) -> XllResult<()> {
        if certificate.runtime_address != std::ptr::from_ref(self).addr() {
            return Err(XllError::Internal {
                diagnostic_id: crate::DiagnosticId::OPEN_ROLLBACK_CERT_UNKNOWN,
            });
        }
        #[cfg(any(test, feature = "shutdown-refinement"))]
        let composition_resources = certificate.composition_resources;
        let mut control = self.lifecycle.lock();
        debug_assert_eq!(control.state.open_attempt(), None);
        if !matches!(
            control.state.phase(),
            LifecyclePhase::OpenRollbackPending | LifecyclePhase::Closing
        ) {
            return Err(XllError::Internal {
                diagnostic_id: crate::DiagnosticId::OPEN_ROLLBACK_PHASE,
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

    pub(crate) fn certify_logical_quiescence(
        &self,
        prerequisites: RemovalQuiescencePrerequisites,
    ) -> XllResult<LogicalQuiescenceCertificate> {
        let control = self.lifecycle.lock();
        let services_stopped =
            self.services.handles.is_none() && self.services.subscriptions.is_none();
        #[cfg(feature = "async")]
        let async_stopped = self.services.async_manager.is_stopped();
        #[cfg(not(feature = "async"))]
        let async_stopped = true;
        let handles_match_generation = control.known_generation.is_none_or(|generation| {
            prerequisites.handles_quiescent.generation() == Some(generation)
        });

        let certified = control.state.phase() == LifecyclePhase::Closing
            && control.state.open_attempt().is_none()
            && control.removal_attempt_active
            && self.returns_closed_and_quiescent()
            && async_stopped
            && services_stopped
            && self.lifecycle.opening.lock().is_none()
            && self.lifecycle.current.load_full().is_none()
            && self.host.registrations_empty()
            && self.host.event_registrations_empty()
            && !self.registration_state_unknown();
        let certified = certified && handles_match_generation;

        if !certified {
            return Err(XllError::Internal {
                diagnostic_id: crate::DiagnosticId::CLOSE_CERTIFICATE,
            });
        }

        #[cfg(any(test, feature = "shutdown-refinement"))]
        let composition_resources = composition_resources_from_close_prerequisites(&prerequisites);

        Ok(LogicalQuiescenceCertificate {
            exports: prerequisites.exports,
            rtd: prerequisites.rtd,
            host_callbacks: prerequisites.host_callbacks,
            async_stopped: prerequisites.async_stopped,
            subscriptions_stopped: prerequisites.subscriptions_stopped,
            handles_quiescent: prerequisites.handles_quiescent,
            diagnostics_stopped: prerequisites.diagnostics_stopped,
            addin_quiesced: prerequisites.addin_quiesced,
            generation_reclaimed: prerequisites.generation_reclaimed,
            #[cfg(any(test, feature = "shutdown-refinement"))]
            composition_resources,
            runtime_address: std::ptr::from_ref(self).addr(),
            generation: control.known_generation,
        })
    }

    pub(crate) fn finish_removal(
        &self,
        certificate: LogicalQuiescenceCertificate,
    ) -> XllResult<ClosedWitness> {
        if certificate.runtime_address != std::ptr::from_ref(self).addr() {
            return Err(XllError::Internal {
                diagnostic_id: crate::DiagnosticId::CLOSE_RUNTIME,
            });
        }
        if certificate.generation != self.generation() {
            return Err(XllError::Internal {
                diagnostic_id: crate::DiagnosticId::CLOSE_LEASE_GATE,
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
                    diagnostic_id: crate::DiagnosticId::CLOSE_GHOST,
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
            crate::execution::CalculationId::new(self.services.async_manager.current_generation())
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
        let _ = self.services.async_manager.advance_generation();
    }

    #[cfg(any(test, feature = "bench-internals"))]
    pub(crate) fn handles(&self) -> XllResult<Arc<crate::handle::HandleRuntime>> {
        self.services.handles.get_owned()
    }

    pub(crate) fn handle_runtime_slot(&self) -> &crate::handle::HandleRuntimeSlot {
        &self.services.handles
    }

    pub(crate) fn seal_handles(&self) -> XllResult<crate::handle::HandleRuntimeSealed> {
        self.services.handles.seal(self.active_generation())
    }

    pub(crate) fn finish_handle_quiescence(
        &self,
        sealed: crate::handle::HandleRuntimeSealed,
    ) -> XllResult<crate::shutdown::HandlesQuiescent> {
        sealed.finish()
    }

    #[inline]
    pub(crate) fn subscriptions(&self) -> XllResult<crate::subscription::SubscriptionRuntimeRead> {
        self.services.subscriptions.read()
    }

    pub(crate) fn close_subscriptions(&self) -> XllResult<crate::shutdown::SubscriptionsStopped> {
        self.services.subscriptions.seal(self.active_generation())
    }

    #[cfg(feature = "async")]
    pub(crate) fn start_async(&self, worker_count: usize) -> XllResult<()> {
        self.services.async_manager.start(worker_count)
    }

    #[cfg(feature = "async")]
    pub(crate) fn cancel_async(&self) {
        self.services.async_manager.cancel_current_generation();
    }

    #[cfg(feature = "async")]
    pub(crate) fn close_async(
        &self,
    ) -> crate::shutdown::StopOutcome<crate::shutdown::AsyncStopped> {
        self.services.async_manager.close()
    }

    #[cfg(feature = "async")]
    pub(crate) fn async_manager(&self) -> &crate::async_udf::AsyncManager {
        &self.services.async_manager
    }

    #[cfg(test)]
    fn registrations_guard(&self) -> MutexGuard<'_, Vec<crate::registration::PendingRegistration>> {
        self.host.registrations.lock()
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
        #[cfg(any(test, feature = "shutdown-refinement"))]
        {
            debug_assert!(!control.removal_attempt_active);
            self.runtime.record_composition_event(
                crate::composition_refinement::CompositionEvent::ReleaseCleanupOwner,
            );
            self.runtime.finish_composition_return();
        }
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
            // Lifecycle rollback is owned by OpenTransaction and must be
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
    pub fn state(&self) -> &A::State {
        &self.generation().state
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
        type State = u32;
        type Error = XllError;
        type Layers = ();

        fn open(
            _context: &crate::addin::OpenContext,
        ) -> Result<crate::Opened<Self::State, Self::Layers>, Self::Error> {
            Ok(crate::Opened::new(0, ()))
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
            .certify_logical_quiescence(RemovalQuiescencePrerequisites {
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
            .certify_open_rollback(OpenRollbackQuiescencePrerequisites {
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
        assert_eq!(runtime.take_current_generation().unwrap().state, 1);
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
            crate::with_excel_call_scope(|scope| {
                new_handles
                    .lookup::<TestHandle>(scope, &new_token)
                    .map(|value| value.0)
            })
            .unwrap(),
            2
        );
        assert!(matches!(
            crate::with_excel_call_scope(|scope| {
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
        let (closed_tx, closed_rx) = mpsc::sync_channel(1);
        let closer = thread::spawn(move || {
            let _close = closing_runtime
                .begin_final_removal()
                .expect("the opening runtime requires final close");
            let state = match closing_runtime
                .take_generation_for_shutdown()
                .expect("shutdown extracts generation")
            {
                ShutdownGeneration::Open(generation) => generation.state,
                ShutdownGeneration::Opening(opening) => opening.into_parts().0,
            };
            assert_eq!(state, 17);
            finish_test_close(&closing_runtime);
            closed_tx.send(()).unwrap();
        });

        let deadline = Instant::now() + Duration::from_secs(1);
        while runtime.phase() != LifecyclePhase::Closing && Instant::now() < deadline {
            thread::yield_now();
        }
        assert_eq!(runtime.phase(), LifecyclePhase::Closing);
        assert_ne!(runtime.removal_epoch(), removal_epoch);
        assert!(matches!(
            runtime.finish_open(&mut opening, Vec::new()),
            Err(XllError::Closing)
        ));
        assert_eq!(runtime.lifecycle.open_attempt(), None);
        assert!(!opening.is_active());

        closed_rx.recv_timeout(Duration::from_secs(1)).unwrap();
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
            .certify_logical_quiescence(RemovalQuiescencePrerequisites {
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
                .certify_logical_quiescence(RemovalQuiescencePrerequisites {
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
            .registrations_guard()
            .push(crate::registration::PendingRegistration::from(
                RegistrationId {
                    id: 1.0,
                    excel_name: "TEST",
                },
            ));
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
            crate::CancellationGuarantee::CalculationScoped,
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
                    crate::CancellationGuarantee::CalculationScoped,
                )
                .0,
            ),
            Err(XllError::ExcelValue(crate::ExcelError::NotAvailable))
        ));

        let (second_source, second_token) = crate::cancellation::CancellationSource::new(
            crate::CancellationGuarantee::CalculationScoped,
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
                crate::CancellationGuarantee::CalculationScoped,
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

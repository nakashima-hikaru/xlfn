//! Private ownership components for [`crate::Runtime`].
//!
//! These types are intentionally crate-private. They make the protocol
//! membership of the runtime state explicit without creating new public crate
//! boundaries or exposing lifecycle bookkeeping to add-in authors.

use arc_swap::ArcSwapOption;
use parking_lot::{Condvar, Mutex};
use std::collections::BTreeMap;
use std::mem::ManuallyDrop;
use std::ops::Deref;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};

use crate::generation::{
    AtomicOptionalOpenAttemptId, AtomicOptionalRuntimeGeneration, RuntimeGeneration,
};
use crate::module_residency::ModuleResidencyLease;
use crate::registration::{EventRegistration, ExcelNameKey, MetadataDebt, PendingRegistration};
use crate::runtime::{HostLifecycleIntent, LifecyclePhase, OpenGeneration, OpeningGeneration};

/// Shared lifecycle vocabulary for generation-scoped lazy services. The
/// service modules keep their own initialization and teardown policy, while
/// this state machine prevents their public phase vocabulary from diverging.
pub(crate) enum GenerationServiceState<C, T> {
    Closed,
    Cold {
        generation: crate::generation::RuntimeGeneration,
        config: C,
    },
    Initializing {
        generation: crate::generation::RuntimeGeneration,
    },
    Ready {
        generation: crate::generation::RuntimeGeneration,
    },
    Sealing {
        generation: crate::generation::RuntimeGeneration,
    },
    InitFaulted {
        generation: crate::generation::RuntimeGeneration,
        error: crate::XllError,
    },
    TeardownFaulted {
        generation: crate::generation::RuntimeGeneration,
        error: crate::XllError,
        runtime: ManuallyDrop<Arc<T>>,
    },
}

/// A read capability over one lazily published generation service.
///
/// The guard keeps the published `Arc` alive without incrementing its strong
/// count on the warm read path. Service modules expose this concrete read
/// capability under their own names, but the ownership and publication
/// protocol remains shared.
pub(crate) struct GenerationServiceRead<R> {
    guard: arc_swap::Guard<Option<Arc<R>>>,
}

impl<R> Deref for GenerationServiceRead<R> {
    type Target = R;

    #[inline]
    fn deref(&self) -> &Self::Target {
        self.guard
            .as_ref()
            .expect("GenerationServiceRead always contains a runtime")
            .as_ref()
    }
}

impl<R> GenerationServiceRead<R> {
    #[inline]
    pub(crate) fn as_arc(&self) -> &Arc<R> {
        self.guard
            .as_ref()
            .expect("GenerationServiceRead always contains a runtime")
    }
}

/// Common state-machine kernel for a generation-scoped lazy service.
///
/// The service-specific modules provide initialization and shutdown closures,
/// while this type owns every transition involving `Closed`, `Cold`,
/// `Initializing`, `Ready`, `Sealing`, and fault retention. In particular,
/// both handle and subscription services use the same wait protocol for a
/// concurrent initializer or sealer.
pub(crate) struct GenerationServiceSlot<C, R> {
    published: ArcSwapOption<R>,
    state: Mutex<GenerationServiceState<C, R>>,
    changed: Condvar,
}

impl<C, R> GenerationServiceSlot<C, R> {
    pub(crate) const fn new() -> Self {
        Self {
            published: ArcSwapOption::const_empty(),
            state: Mutex::new(GenerationServiceState::Closed),
            changed: Condvar::new(),
        }
    }

    pub(crate) fn arm(&self, generation: RuntimeGeneration, config: C) -> crate::XllResult<()> {
        let mut state = self.state.lock();
        if !matches!(*state, GenerationServiceState::Closed) {
            return Err(crate::XllError::Closing);
        }
        *state = GenerationServiceState::Cold { generation, config };
        self.changed.notify_all();
        Ok(())
    }

    pub(crate) fn disarm(&self, generation: RuntimeGeneration) -> crate::XllResult<()> {
        let mut state = self.state.lock();
        match &*state {
            GenerationServiceState::Cold {
                generation: active, ..
            } if *active == generation => {
                *state = GenerationServiceState::Closed;
                self.changed.notify_all();
                Ok(())
            }
            GenerationServiceState::Closed => Ok(()),
            _ => Err(crate::XllError::Closing),
        }
    }

    pub(crate) fn is_none(&self) -> bool {
        self.published.load().is_none()
            && matches!(
                *self.state.lock(),
                GenerationServiceState::Closed | GenerationServiceState::InitFaulted { .. }
            )
    }

    #[cfg(any(test, feature = "shutdown-refinement"))]
    pub(crate) fn with_published(&self, callback: impl FnOnce(Option<&Arc<R>>)) {
        let published = self.published.load();
        callback(published.as_ref());
    }
}

impl<C: Copy, R> GenerationServiceSlot<C, R> {
    /// Acquires a read guard, lazily initializing the service if this is the
    /// first reader of the armed generation.
    pub(crate) fn read(
        &self,
        initialize: impl FnOnce(C) -> crate::XllResult<Arc<R>>,
        on_initialized: impl FnOnce(&Arc<R>),
    ) -> crate::XllResult<GenerationServiceRead<R>> {
        let guard = self.published.load();
        if guard.is_some() {
            return Ok(GenerationServiceRead { guard });
        }
        drop(guard);
        self.read_slow(initialize, on_initialized)
    }

    #[cold]
    fn read_slow(
        &self,
        initialize: impl FnOnce(C) -> crate::XllResult<Arc<R>>,
        on_initialized: impl FnOnce(&Arc<R>),
    ) -> crate::XllResult<GenerationServiceRead<R>> {
        let mut initialize = Some(initialize);
        let mut on_initialized = Some(on_initialized);
        let mut state = self.state.lock();

        loop {
            match &*state {
                GenerationServiceState::Ready { .. } => {
                    drop(state);
                    let guard = self.published.load();
                    debug_assert!(guard.is_some());
                    return Ok(GenerationServiceRead { guard });
                }
                GenerationServiceState::InitFaulted { error, .. }
                | GenerationServiceState::TeardownFaulted { error, .. } => {
                    return Err(error.clone());
                }
                GenerationServiceState::Initializing { generation }
                | GenerationServiceState::Sealing { generation } => {
                    let _ = generation;
                    self.changed.wait(&mut state);
                }
                GenerationServiceState::Cold { generation, config } => {
                    let generation = *generation;
                    let config = *config;
                    *state = GenerationServiceState::Initializing { generation };
                    drop(state);

                    let candidate = initialize
                        .take()
                        .expect("a service initializer is consumed exactly once")(
                        config
                    );

                    let mut state = self.state.lock();
                    match candidate {
                        Ok(runtime) => {
                            if let Some(on_initialized) = on_initialized.take() {
                                on_initialized(&runtime);
                            }
                            self.published.store(Some(runtime));
                            *state = GenerationServiceState::Ready { generation };
                            self.changed.notify_all();
                            drop(state);

                            let guard = self.published.load();
                            debug_assert!(guard.is_some());
                            return Ok(GenerationServiceRead { guard });
                        }
                        Err(error) => {
                            *state = GenerationServiceState::InitFaulted {
                                generation,
                                error: error.clone(),
                            };
                            self.changed.notify_all();
                            return Err(error);
                        }
                    }
                }
                GenerationServiceState::Closed => return Err(crate::XllError::Closing),
            }
        }
    }

    pub(crate) fn seal<S>(
        &self,
        generation: Option<RuntimeGeneration>,
        missing_runtime_diagnostic: crate::DiagnosticId,
        empty: impl FnOnce() -> S,
        shutdown: impl FnOnce(Arc<R>) -> crate::XllResult<S>,
    ) -> crate::XllResult<S> {
        let runtime = {
            let mut state = self.state.lock();
            while matches!(
                *state,
                GenerationServiceState::Initializing { .. }
                    | GenerationServiceState::Sealing { .. }
            ) {
                self.changed.wait(&mut state);
            }

            match &*state {
                GenerationServiceState::Ready { generation: active } => {
                    if generation != Some(*active) {
                        return Err(crate::XllError::Closing);
                    }
                    let runtime = self.published.swap(None);
                    *state = GenerationServiceState::Sealing {
                        generation: *active,
                    };
                    runtime
                }
                GenerationServiceState::Cold {
                    generation: active, ..
                }
                | GenerationServiceState::InitFaulted {
                    generation: active, ..
                } => {
                    if generation != Some(*active) {
                        return Err(crate::XllError::Closing);
                    }
                    *state = GenerationServiceState::Closed;
                    self.changed.notify_all();
                    return Ok(empty());
                }
                GenerationServiceState::Closed => return Ok(empty()),
                GenerationServiceState::TeardownFaulted {
                    generation: active,
                    error,
                    runtime,
                } => {
                    let _ = runtime;
                    if generation != Some(*active) {
                        return Err(crate::XllError::Closing);
                    }
                    return Err(error.clone());
                }
                GenerationServiceState::Initializing { .. }
                | GenerationServiceState::Sealing { .. } => unreachable!(),
            }
        };

        let Some(runtime) = runtime else {
            return Err(crate::XllError::Internal {
                diagnostic_id: missing_runtime_diagnostic,
            });
        };
        let result = shutdown(Arc::clone(&runtime));
        let mut state = self.state.lock();
        match result {
            Ok(sealed) => {
                *state = GenerationServiceState::Closed;
                self.changed.notify_all();
                Ok(sealed)
            }
            Err(error) => {
                *state = GenerationServiceState::TeardownFaulted {
                    generation: generation.expect("a live service runtime has a generation"),
                    error: error.clone(),
                    runtime: ManuallyDrop::new(runtime),
                };
                self.changed.notify_all();
                Err(error)
            }
        }
    }
}

/// A terminal reason for retaining a resource instead of running its
/// destructor after unload safety could not be established.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum QuarantineReason {
    OpenStateInvariant,
    AddinQuiesceFailed,
    AddinGenerationEscaped,
    AddinCleanupPanicked,
    BoundaryFailure,
}

/// Explicit ownership for resources that are intentionally never dropped
/// after a quarantine decision. `ManuallyDrop` is used as documentation and
/// as a type-level guarantee that dropping the runtime cannot accidentally
/// execute code whose quiescence was not proven.
pub(crate) enum QuarantinedResource<A: crate::Addin> {
    State {
        generation: Option<RuntimeGeneration>,
        state: ManuallyDrop<A::State>,
        reason: QuarantineReason,
    },
    Layers {
        generation: Option<RuntimeGeneration>,
        layers: ManuallyDrop<A::Layers>,
        reason: QuarantineReason,
    },
    Generation {
        generation: Option<RuntimeGeneration>,
        state: ManuallyDrop<A::State>,
        layers: ManuallyDrop<A::Layers>,
        reason: QuarantineReason,
    },
    SharedGeneration {
        generation: Option<RuntimeGeneration>,
        generation_root: ManuallyDrop<Arc<OpenGeneration<A>>>,
        reason: QuarantineReason,
    },
}

pub(crate) struct QuarantineVault<A: crate::Addin> {
    resources: Mutex<Vec<QuarantinedResource<A>>>,
}

impl<A: crate::Addin> QuarantineVault<A> {
    pub(crate) const fn new() -> Self {
        Self {
            resources: Mutex::new(Vec::new()),
        }
    }

    pub(crate) fn retain_state(
        &self,
        generation: Option<RuntimeGeneration>,
        state: A::State,
        reason: QuarantineReason,
    ) {
        self.resources.lock().push(QuarantinedResource::State {
            generation,
            state: ManuallyDrop::new(state),
            reason,
        });
    }

    pub(crate) fn retain_layers(
        &self,
        generation: Option<RuntimeGeneration>,
        layers: A::Layers,
        reason: QuarantineReason,
    ) {
        self.resources.lock().push(QuarantinedResource::Layers {
            generation,
            layers: ManuallyDrop::new(layers),
            reason,
        });
    }

    pub(crate) fn retain_generation(
        &self,
        generation: Option<RuntimeGeneration>,
        root: OpenGeneration<A>,
        reason: QuarantineReason,
    ) {
        let OpenGeneration { state, layers, .. } = root;
        self.resources.lock().push(QuarantinedResource::Generation {
            generation,
            state: ManuallyDrop::new(state),
            layers: ManuallyDrop::new(layers),
            reason,
        });
    }

    pub(crate) fn retain_shared_generation(
        &self,
        generation: Option<RuntimeGeneration>,
        root: Arc<OpenGeneration<A>>,
        reason: QuarantineReason,
    ) {
        self.resources
            .lock()
            .push(QuarantinedResource::SharedGeneration {
                generation,
                generation_root: ManuallyDrop::new(root),
                reason,
            });
    }

    pub(crate) fn snapshot(&self) -> Vec<(Option<RuntimeGeneration>, QuarantineReason)> {
        self.resources
            .lock()
            .iter()
            .map(|resource| match resource {
                QuarantinedResource::State {
                    generation,
                    state,
                    reason,
                } => {
                    let _ = state;
                    (*generation, *reason)
                }
                QuarantinedResource::Layers {
                    generation,
                    layers,
                    reason,
                } => {
                    let _ = layers;
                    (*generation, *reason)
                }
                QuarantinedResource::Generation {
                    generation,
                    state,
                    layers,
                    reason,
                } => {
                    let _ = (state, layers);
                    (*generation, *reason)
                }
                QuarantinedResource::SharedGeneration {
                    generation,
                    generation_root,
                    reason,
                } => {
                    let _ = generation_root;
                    (*generation, *reason)
                }
            })
            .collect()
    }
}

/// Lifecycle synchronization state: phase, epochs, open ownership, and the
/// published generation all move together under the lifecycle protocol.
pub(crate) struct LifecycleState<A: crate::Addin> {
    pub(crate) phase: AtomicU8,
    pub(crate) host_intent: AtomicU8,
    pub(crate) next_lifecycle_attempt: AtomicU64,
    pub(crate) generation: AtomicOptionalRuntimeGeneration,
    pub(crate) open_attempt_id: AtomicOptionalOpenAttemptId,
    pub(crate) removal_epoch: AtomicU64,
    pub(crate) opening: Mutex<Option<OpeningGeneration<A>>>,
    pub(crate) current: ArcSwapOption<OpenGeneration<A>>,
    pub(crate) wait_lock: Mutex<()>,
    pub(crate) lifecycle_changed: Condvar,
    pub(crate) removal_attempt_active: AtomicBool,
    #[cfg(test)]
    pub(crate) test_module_lease: Mutex<Option<crate::ingress::TestModuleLease>>,
}

pub(crate) struct PublishOpeningError<A: crate::Addin> {
    pub(crate) error: crate::XllError,
    pub(crate) opening: Option<OpeningGeneration<A>>,
}

impl<A: crate::Addin> LifecycleState<A> {
    pub(crate) const fn new() -> Self {
        Self {
            phase: AtomicU8::new(LifecyclePhase::Closed as u8),
            host_intent: AtomicU8::new(HostLifecycleIntent::None as u8),
            next_lifecycle_attempt: AtomicU64::new(1),
            generation: AtomicOptionalRuntimeGeneration::empty(),
            open_attempt_id: AtomicOptionalOpenAttemptId::empty(),
            removal_epoch: AtomicU64::new(0),
            opening: Mutex::new(None),
            current: ArcSwapOption::const_empty(),
            wait_lock: Mutex::new(()),
            lifecycle_changed: Condvar::new(),
            removal_attempt_active: AtomicBool::new(false),
            #[cfg(test)]
            test_module_lease: Mutex::new(None),
        }
    }

    pub(crate) fn stage_opening_state(
        &self,
        state: A::State,
        config: crate::RuntimeConfig,
    ) -> Result<(), (crate::XllError, A::State)> {
        let mut slot = self.opening.lock();
        if slot.is_some() || self.current.load().is_some() {
            return Err((
                crate::XllError::Internal {
                    diagnostic_id: crate::DiagnosticId::OPEN_STATE,
                },
                state,
            ));
        }
        *slot = Some(OpeningGeneration::StateOnly { state, config });
        Ok(())
    }

    pub(crate) fn restore_opening_generation(
        &self,
        opening: OpeningGeneration<A>,
    ) -> Result<(), (crate::XllError, OpeningGeneration<A>)> {
        let mut slot = self.opening.lock();
        if slot.is_some() {
            return Err((
                crate::XllError::Internal {
                    diagnostic_id: crate::DiagnosticId::OPEN_STATE,
                },
                opening,
            ));
        }
        *slot = Some(opening);
        Ok(())
    }

    pub(crate) fn publish_opening_generation(
        &self,
        generation: crate::generation::RuntimeGeneration,
    ) -> Result<(), PublishOpeningError<A>> {
        let opening = self.opening.lock().take().ok_or(PublishOpeningError {
            error: crate::XllError::Internal {
                diagnostic_id: crate::DiagnosticId::OPEN_STATE,
            },
            opening: None,
        })?;
        let (state, layers, _config) = match opening {
            OpeningGeneration::Ready {
                state,
                layers,
                config,
            } => (state, layers, config),
            opening @ OpeningGeneration::StateOnly { .. } => {
                return Err(PublishOpeningError {
                    error: crate::XllError::Internal {
                        diagnostic_id: crate::DiagnosticId::OPEN_STATE,
                    },
                    opening: Some(opening),
                });
            }
        };
        self.current.store(Some(Arc::new(OpenGeneration {
            id: generation,
            state,
            layers,
        })));
        Ok(())
    }

    pub(crate) fn opening_config(&self) -> Option<crate::RuntimeConfig> {
        self.opening.lock().as_ref().map(|opening| match opening {
            OpeningGeneration::StateOnly { config, .. }
            | OpeningGeneration::Ready { config, .. } => *config,
        })
    }

    pub(crate) fn has_opening_generation(&self) -> bool {
        self.opening.lock().is_some()
    }

    pub(crate) fn has_current_generation(&self) -> bool {
        self.current.load().is_some()
    }

    pub(crate) fn take_opening_generation(&self) -> Option<OpeningGeneration<A>> {
        self.opening.lock().take()
    }

    pub(crate) fn take_current_generation(&self) -> Option<Arc<OpenGeneration<A>>> {
        self.current.swap(None)
    }

    pub(crate) fn take_generation_for_shutdown(
        &self,
    ) -> Option<crate::runtime::ShutdownGeneration<A>> {
        debug_assert!(!(self.has_opening_generation() && self.has_current_generation()));
        if let Some(generation) = self.take_current_generation() {
            return Some(crate::runtime::ShutdownGeneration::Open(generation));
        }
        self.take_opening_generation()
            .map(crate::runtime::ShutdownGeneration::Opening)
    }
}

/// The Excel host registration protocol and its recovery ledger.
pub(crate) struct HostLedger {
    pub(crate) registrations: Mutex<Vec<PendingRegistration>>,
    pub(crate) metadata_debt: Mutex<BTreeMap<ExcelNameKey, Vec<MetadataDebt>>>,
    pub(crate) event_registrations: Mutex<Vec<EventRegistration>>,
    pub(crate) registration_state_unknown: AtomicBool,
}

impl HostLedger {
    pub(crate) const fn new() -> Self {
        Self {
            registrations: Mutex::new(Vec::new()),
            metadata_debt: Mutex::new(BTreeMap::new()),
            event_registrations: Mutex::new(Vec::new()),
            registration_state_unknown: AtomicBool::new(false),
        }
    }

    pub(crate) fn append_registrations(
        &self,
        registrations: impl IntoIterator<Item = PendingRegistration>,
    ) {
        self.registrations.lock().extend(registrations);
    }

    pub(crate) fn append_event_registrations(
        &self,
        registrations: impl IntoIterator<Item = EventRegistration>,
    ) {
        self.event_registrations.lock().extend(registrations);
    }

    pub(crate) fn registrations_snapshot(&self) -> Vec<PendingRegistration> {
        self.registrations.lock().clone()
    }

    pub(crate) fn event_registrations_snapshot(&self) -> Vec<EventRegistration> {
        self.event_registrations.lock().clone()
    }

    pub(crate) fn registrations_empty(&self) -> bool {
        self.registrations.lock().is_empty()
    }

    pub(crate) fn event_registrations_empty(&self) -> bool {
        self.event_registrations.lock().is_empty()
    }

    pub(crate) fn replace_registrations(&self, registrations: Vec<PendingRegistration>) {
        *self.registrations.lock() = registrations;
    }

    pub(crate) fn replace_event_registrations(&self, registrations: Vec<EventRegistration>) {
        *self.event_registrations.lock() = registrations;
    }

    pub(crate) fn mark_registration_state_unknown(&self) {
        self.registration_state_unknown
            .store(true, Ordering::Release);
    }

    pub(crate) fn registration_state_unknown(&self) -> bool {
        self.registration_state_unknown.load(Ordering::Acquire)
    }

    pub(crate) fn retain_metadata_debt(&self, debts: Vec<MetadataDebt>) {
        let mut retained = self.metadata_debt.lock();
        for debt in debts {
            retained.entry(debt.key()).or_default().push(debt);
        }
    }

    pub(crate) fn metadata_debt_snapshot(&self) -> BTreeMap<ExcelNameKey, Vec<MetadataDebt>> {
        self.metadata_debt.lock().clone()
    }

    pub(crate) fn clear_metadata_debt_for_registrations(
        &self,
        registrations: &[crate::RegistrationId],
    ) {
        let mut debts = self.metadata_debt.lock();
        for registration in registrations {
            debts.remove(&ExcelNameKey::new(registration.excel_name));
        }
    }

    pub(crate) fn replace_metadata_debt(&self, debts: BTreeMap<ExcelNameKey, Vec<MetadataDebt>>) {
        *self.metadata_debt.lock() = debts;
    }

    pub(crate) fn has_metadata_debt(&self) -> bool {
        !self.metadata_debt.lock().is_empty()
    }
}

/// Excel return ownership and call/calculation identity state.
pub(crate) struct ReturnProtocol {
    pub(crate) returns: crate::return_value::ReturnTracker,
    pub(crate) next_call_id: AtomicU64,
    #[cfg(not(feature = "async"))]
    pub(crate) calculation_id: AtomicU64,
}

impl ReturnProtocol {
    pub(crate) const fn new() -> Self {
        Self {
            returns: crate::return_value::ReturnTracker::new_closed(),
            next_call_id: AtomicU64::new(1),
            #[cfg(not(feature = "async"))]
            calculation_id: AtomicU64::new(1),
        }
    }

    pub(crate) fn close_admission(&self) {
        self.returns.close_admission();
    }

    pub(crate) fn reopen_admission(&self) -> crate::XllResult<()> {
        self.returns.reopen_admission()
    }

    pub(crate) fn enter_producer(
        &'static self,
    ) -> Option<crate::return_value::ReturnProducerGuard<'static>> {
        self.returns.try_enter_producer()
    }

    pub(crate) fn wait_for_returns(&self) {
        self.returns.wait_for_quiescence();
    }

    pub(crate) fn returns_are_quiescent(&self) -> bool {
        self.returns.is_quiescent()
    }

    pub(crate) fn returns_closed_and_quiescent(&self) -> bool {
        self.returns.admission_closed() && self.returns.is_quiescent()
    }

    pub(crate) fn next_call_id(&self) -> u64 {
        self.next_call_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }

    #[cfg(test)]
    pub(crate) fn peek_next_call_id(&self) -> u64 {
        self.next_call_id.load(std::sync::atomic::Ordering::Relaxed)
    }
}

/// Framework service slots owned by the runtime and reopened for each
/// generation. Generation-specific policy lives in [`crate::RuntimeConfig`]
/// inside [`crate::runtime::OpenGeneration`], while these slots carry the
/// reusable service state.
pub(crate) struct RuntimeServices {
    pub(crate) handles: crate::handle::HandleRuntimeSlot,
    pub(crate) subscriptions: crate::subscription::SubscriptionRuntimeSlot,
    #[cfg(feature = "async")]
    pub(crate) async_manager: crate::async_udf::AsyncManager,
}

/// Reservation for the two generation-scoped service slots.
///
/// Arming is a transaction: until the reservation is committed, dropping it
/// rolls both slots back.  A rollback failure means the slot protocol has
/// violated its ownership invariant, so it is fail-stopped instead of being
/// silently discarded at an open boundary.
#[must_use = "an armed service reservation must be committed or rolled back"]
pub(crate) struct ArmedServices<'a> {
    services: &'a RuntimeServices,
    generation: RuntimeGeneration,
    committed: bool,
}

impl ArmedServices<'_> {
    pub(crate) fn commit(mut self) {
        self.committed = true;
    }

    pub(crate) fn rollback(mut self) {
        self.committed = true;
        self.services.disarm_or_abort(self.generation);
    }
}

impl Drop for ArmedServices<'_> {
    fn drop(&mut self) {
        if !self.committed {
            self.services.disarm_or_abort(self.generation);
        }
    }
}

impl RuntimeServices {
    pub(crate) const fn new() -> Self {
        Self {
            handles: crate::handle::HandleRuntimeSlot::new(),
            subscriptions: crate::subscription::SubscriptionRuntimeSlot::new(),
            #[cfg(feature = "async")]
            async_manager: crate::async_udf::AsyncManager::new(),
        }
    }

    pub(crate) fn arm_generation(
        &self,
        generation: RuntimeGeneration,
        config: crate::RuntimeConfig,
    ) -> crate::XllResult<ArmedServices<'_>> {
        self.handles.arm(generation, config.handle_config())?;
        let armed = ArmedServices {
            services: self,
            generation,
            committed: false,
        };
        self.subscriptions
            .arm(generation, config.rtd_limits())
            .map(|()| armed)
    }

    fn disarm_or_abort(&self, generation: RuntimeGeneration) {
        if let Err(error) = self.disarm_generation(generation) {
            tracing::error!(
                ?generation,
                %error,
                "generation service rollback violated its state invariant"
            );
            std::process::abort();
        }
    }

    pub(crate) fn disarm_generation(&self, generation: RuntimeGeneration) -> crate::XllResult<()> {
        let handle_result = self.handles.disarm(generation);
        let subscription_result = self.subscriptions.disarm(generation);
        handle_result.and(subscription_result)
    }
}

/// Physical DLL residency is deliberately separate from logical lifecycle
/// state. A quarantined or logically closed runtime can retain this lease.
pub(crate) struct ModuleResidency {
    pub(crate) lease: Mutex<Option<ModuleResidencyLease>>,
}

impl ModuleResidency {
    pub(crate) const fn new() -> Self {
        Self {
            lease: Mutex::new(None),
        }
    }
}

#[cfg(any(test, feature = "shutdown-refinement"))]
/// Verification-only state is isolated from operational runtime components.
pub(crate) struct FormalState {
    pub(crate) ghost: std::sync::OnceLock<crate::shutdown_refinement::GhostHandle>,
    pub(crate) composition:
        std::sync::OnceLock<Arc<crate::composition_refinement::CompositionTrace>>,
}

#[cfg(any(test, feature = "shutdown-refinement"))]
impl FormalState {
    pub(crate) const fn new() -> Self {
        Self {
            ghost: std::sync::OnceLock::new(),
            composition: std::sync::OnceLock::new(),
        }
    }
}

//! Canonical lifecycle state and its read-side phase projection.

use arc_swap::ArcSwapOption;
use parking_lot::{Condvar, Mutex, MutexGuard};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use super::services::GenerationServicesLease;
use crate::generation::{OpenAttemptId, RuntimeGeneration};
use crate::lifecycle::{HostLifecycleIntent, LifecyclePhase};
use crate::module_runtime::ModuleEpochLease;
use crate::runtime::{OpenGeneration, OpeningGeneration};

/// Canonical lifecycle state owned by the lifecycle core mutex.
///
/// The phase atomic in [`LifecycleCoordinator`] is deliberately only a read-side
/// projection. Every writer first updates this state and then publishes the
/// phase through [`LifecycleCoordinator::refresh_projection`]. Correlated lifecycle
/// values remain behind this mutex and are read as one canonical snapshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LifecycleStateKind {
    Closed,
    Opening {
        attempt: OpenAttemptId,
    },
    Open {
        generation: RuntimeGeneration,
    },
    Closing {
        generation: Option<RuntimeGeneration>,
        open_attempt: Option<OpenAttemptId>,
    },
    OpenRollbackPending {
        generation: Option<RuntimeGeneration>,
    },
    Quarantined,
}

impl LifecycleStateKind {
    pub(crate) const fn phase(self) -> LifecyclePhase {
        match self {
            Self::Closed => LifecyclePhase::Closed,
            Self::Opening { .. } => LifecyclePhase::Opening,
            Self::Open { .. } => LifecyclePhase::Open,
            Self::Closing { .. } => LifecyclePhase::Closing,
            Self::OpenRollbackPending { .. } => LifecyclePhase::OpenRollbackPending,
            Self::Quarantined => LifecyclePhase::Quarantined,
        }
    }

    pub(crate) const fn open_attempt(self) -> Option<OpenAttemptId> {
        match self {
            Self::Opening { attempt } => Some(attempt),
            Self::Closing { open_attempt, .. } => open_attempt,
            Self::Closed
            | Self::Open { .. }
            | Self::OpenRollbackPending { .. }
            | Self::Quarantined => None,
        }
    }
}

/// Canonical owner of every mutable lifecycle decision and generation root.
///
/// `known_generation` intentionally survives the transition to `Closed`: it
/// identifies the last generation whose teardown was certified and is used by
/// shutdown certificates and diagnostics. The currently active generation is
/// represented by `current` and the `Open` state. Both generation roots live
/// in this same mutex-protected value; the ArcSwap in [`LifecycleCoordinator`] is
/// only a read-side projection of `current`.
pub(crate) struct LifecycleCore<A: crate::Addin> {
    state: LifecycleStateKind,
    opening: Option<OpeningGeneration<A>>,
    current: Option<Arc<OpenGeneration<A>>>,
    services_lease: Option<GenerationServicesLease>,
    module_epoch: Option<ModuleEpochLease>,
    host_intent: HostLifecycleIntent,
    next_lifecycle_attempt: u64,
    known_generation: Option<RuntimeGeneration>,
    removal_epoch: u64,
    removal_attempt_active: bool,
}

impl<A: crate::Addin> LifecycleCore<A> {
    const fn new() -> Self {
        Self {
            state: LifecycleStateKind::Closed,
            opening: None,
            current: None,
            services_lease: None,
            module_epoch: None,
            host_intent: HostLifecycleIntent::None,
            next_lifecycle_attempt: 1,
            known_generation: None,
            removal_epoch: 0,
            removal_attempt_active: false,
        }
    }

    /// Returns the mutex-protected canonical state. Atomic projections are
    /// intentionally not exposed through this API.
    pub(crate) const fn canonical_state(&self) -> LifecycleStateKind {
        self.state
    }

    pub(crate) const fn host_intent(&self) -> HostLifecycleIntent {
        self.host_intent
    }

    pub(crate) const fn known_generation(&self) -> Option<RuntimeGeneration> {
        self.known_generation
    }

    pub(crate) const fn removal_epoch(&self) -> u64 {
        self.removal_epoch
    }

    pub(crate) const fn removal_attempt_active(&self) -> bool {
        self.removal_attempt_active
    }

    #[cfg(test)]
    pub(crate) fn set_next_lifecycle_attempt_for_test(&mut self, value: u64) {
        self.next_lifecycle_attempt = value;
    }

    pub(crate) fn opening_config(&self) -> Option<crate::addin::RuntimeConfig> {
        self.opening.as_ref().map(|opening| opening.config)
    }

    pub(crate) const fn has_opening_generation(&self) -> bool {
        self.opening.is_some()
    }

    pub(crate) const fn has_current_generation(&self) -> bool {
        self.current.is_some()
    }

    pub(crate) fn generation_services_lease_generation(&self) -> Option<RuntimeGeneration> {
        self.services_lease
            .as_ref()
            .map(GenerationServicesLease::generation)
    }

    pub(crate) fn has_module_epoch(&self) -> bool {
        self.module_epoch.is_some()
    }

    pub(crate) fn module_epoch_is_current(&self) -> bool {
        self.module_epoch
            .as_ref()
            .is_none_or(ModuleEpochLease::is_current)
    }

    fn install_generation_services_lease(&mut self, lease: GenerationServicesLease) {
        debug_assert!(self.services_lease.is_none());
        self.services_lease = Some(lease);
    }

    fn take_generation_services_lease(&mut self) -> Option<GenerationServicesLease> {
        self.services_lease.take()
    }

    fn install_module_epoch(&mut self, lease: ModuleEpochLease) {
        debug_assert!(self.module_epoch.is_none());
        self.module_epoch = Some(lease);
    }

    fn take_module_epoch(&mut self) -> Option<ModuleEpochLease> {
        self.module_epoch.take()
    }
}

/// Lifecycle synchronization state.
///
/// `core` is the canonical ownership boundary. `phase` and `current` are
/// read-side projections used by hot-path admission and generation access;
/// lifecycle writers must mutate the corresponding fields in `core` first.
pub(crate) struct LifecycleCoordinator<A: crate::Addin> {
    phase: AtomicU8,
    current: ArcSwapOption<OpenGeneration<A>>,
    core: Mutex<LifecycleCore<A>>,
    changed: Condvar,
    #[cfg(test)]
    pub(crate) test_module_lease: Mutex<Option<crate::ingress::TestModuleLease>>,
}

pub(crate) struct PublishOpeningError<A: crate::Addin> {
    pub(crate) error: crate::XllError,
    pub(crate) opening: Option<OpeningGeneration<A>>,
}

impl<A: crate::Addin> LifecycleCoordinator<A> {
    pub(crate) const fn new() -> Self {
        Self {
            phase: AtomicU8::new(LifecyclePhase::Closed as u8),
            current: ArcSwapOption::const_empty(),
            core: Mutex::new(LifecycleCore::new()),
            changed: Condvar::new(),
            #[cfg(test)]
            test_module_lease: Mutex::new(None),
        }
    }

    pub(crate) fn lock(&self) -> MutexGuard<'_, LifecycleCore<A>> {
        self.core.lock()
    }

    pub(crate) fn wait<'a>(&self, core: &mut MutexGuard<'a, LifecycleCore<A>>) {
        self.changed.wait(core);
    }

    pub(crate) fn notify_all(&self) {
        self.changed.notify_all();
    }

    /// Returns the read-side phase projection.
    ///
    /// Lifecycle writers must inspect [`LifecycleCore::state`] instead;
    /// this method is intentionally named to make that distinction visible.
    pub(crate) fn observed_phase(&self) -> LifecyclePhase {
        LifecyclePhase::from_raw(self.phase.load(Ordering::Acquire))
    }

    pub(crate) fn set_host_intent(&self, intent: HostLifecycleIntent) {
        let mut core = self.lock();
        self.set_host_intent_locked(&mut core, intent);
    }

    fn set_host_intent_locked(&self, core: &mut LifecycleCore<A>, intent: HostLifecycleIntent) {
        core.host_intent = intent;
        self.refresh_projection(core);
    }

    fn set_state(&self, core: &mut LifecycleCore<A>, state: LifecycleStateKind) {
        core.state = state;
        self.refresh_projection(core);
    }

    fn set_removal_attempt_active(&self, core: &mut LifecycleCore<A>, active: bool) {
        core.removal_attempt_active = active;
        self.refresh_projection(core);
    }

    fn advance_removal_epoch(&self, core: &mut LifecycleCore<A>) {
        core.removal_epoch = core.removal_epoch.checked_add(1).unwrap_or_else(|| {
            tracing::error!("lifecycle close epoch exhausted; fail-stopping");
            std::process::abort();
        });
        self.refresh_projection(core);
    }

    fn next_lifecycle_attempt_id(
        &self,
        core: &mut LifecycleCore<A>,
    ) -> crate::XllResult<OpenAttemptId> {
        let attempt_id = core.next_lifecycle_attempt;
        let next = attempt_id.checked_add(1).ok_or(crate::XllError::Internal {
            diagnostic_id: crate::error::DiagnosticId::ATTEMPT_OVERFLOW,
        })?;
        let attempt = OpenAttemptId::new(attempt_id).ok_or(crate::XllError::Internal {
            diagnostic_id: crate::error::DiagnosticId::ATTEMPT_ZERO,
        })?;
        core.next_lifecycle_attempt = next;
        Ok(attempt)
    }

    fn refresh_projection(&self, core: &LifecycleCore<A>) {
        self.phase
            .store(core.state.phase() as u8, Ordering::Release);
    }

    /// Clears host intent before the external module-open protocol is started.
    /// The state remains `Closed` until [`Self::begin_opening`] linearizes the
    /// opening transition after those external gates have been acquired.
    pub(crate) fn prepare_open(&self, core: &mut LifecycleCore<A>) {
        debug_assert_eq!(core.canonical_state().phase(), LifecyclePhase::Closed);
        self.set_host_intent_locked(core, HostLifecycleIntent::None);
    }

    pub(crate) fn allocate_open_attempt(
        &self,
        core: &mut LifecycleCore<A>,
    ) -> crate::XllResult<OpenAttemptId> {
        self.next_lifecycle_attempt_id(core)
    }

    /// Linearizes the opening state after module-level admission has been
    /// acquired. Runtime code cannot publish an arbitrary `LifecycleStateKind`.
    pub(crate) fn begin_opening(&self, core: &mut LifecycleCore<A>, attempt: OpenAttemptId) {
        debug_assert_eq!(core.canonical_state().phase(), LifecyclePhase::Closed);
        debug_assert!(!core.removal_attempt_active());
        self.set_state(core, LifecycleStateKind::Opening { attempt });
    }

    /// Publishes a successfully assembled generation and its open state as one
    /// lifecycle transition.
    pub(crate) fn commit_open(&self, core: &mut LifecycleCore<A>, generation: RuntimeGeneration) {
        debug_assert_eq!(core.canonical_state().phase(), LifecyclePhase::Opening);
        debug_assert!(core.has_current_generation());
        core.known_generation = Some(generation);
        core.state = LifecycleStateKind::Open { generation };
        self.refresh_projection(core);
    }

    pub(crate) fn reject_open_attempt(&self, core: &mut LifecycleCore<A>) {
        let state = match core.canonical_state().phase() {
            LifecyclePhase::Closing => LifecycleStateKind::Closing {
                generation: core.known_generation(),
                open_attempt: None,
            },
            LifecyclePhase::OpenRollbackPending => LifecycleStateKind::OpenRollbackPending {
                generation: core.known_generation(),
            },
            LifecyclePhase::Quarantined => LifecycleStateKind::Quarantined,
            _ => LifecycleStateKind::Closed,
        };
        self.set_state(core, state);
    }

    /// Records an open failure and returns whether rollback work is required.
    pub(crate) fn record_open_failure(&self, core: &mut LifecycleCore<A>) -> bool {
        match core.canonical_state().phase() {
            LifecyclePhase::Opening => {
                let generation = core.known_generation();
                self.set_state(core, LifecycleStateKind::OpenRollbackPending { generation });
                true
            }
            LifecyclePhase::OpenRollbackPending => true,
            LifecyclePhase::Closing => {
                let generation = core.known_generation();
                self.set_state(
                    core,
                    LifecycleStateKind::Closing {
                        generation,
                        open_attempt: None,
                    },
                );
                false
            }
            LifecyclePhase::Closed | LifecyclePhase::Open | LifecyclePhase::Quarantined => false,
        }
    }

    /// Requests the closing phase while preserving the active generation and
    /// any still-running open attempt in the canonical state.
    pub(crate) fn request_closing(&self, core: &mut LifecycleCore<A>) {
        match core.canonical_state().phase() {
            LifecyclePhase::Closed => {
                if core.removal_attempt_active() {
                    return;
                }
                let generation = core.known_generation();
                let open_attempt = core.canonical_state().open_attempt();
                self.set_state(
                    core,
                    LifecycleStateKind::Closing {
                        generation,
                        open_attempt,
                    },
                );
            }
            LifecyclePhase::Closing | LifecyclePhase::Quarantined => {}
            LifecyclePhase::Opening
            | LifecyclePhase::Open
            | LifecyclePhase::OpenRollbackPending => {
                let generation = core.known_generation();
                let open_attempt = core.canonical_state().open_attempt();
                self.set_state(
                    core,
                    LifecycleStateKind::Closing {
                        generation,
                        open_attempt,
                    },
                );
            }
        }
    }

    pub(crate) fn begin_removal_request(&self, core: &mut LifecycleCore<A>) {
        self.advance_removal_epoch(core);
    }

    pub(crate) fn claim_removal_owner(&self, core: &mut LifecycleCore<A>) -> bool {
        if matches!(
            core.canonical_state().phase(),
            LifecyclePhase::Closed | LifecyclePhase::Quarantined
        ) || core.canonical_state().open_attempt().is_some()
            || core.removal_attempt_active()
        {
            return false;
        }
        self.set_removal_attempt_active(core, true);
        true
    }

    pub(crate) fn release_removal_owner(&self, core: &mut LifecycleCore<A>) {
        self.set_removal_attempt_active(core, false);
    }

    pub(crate) fn finish_closed(&self, core: &mut LifecycleCore<A>) {
        self.set_state(core, LifecycleStateKind::Closed);
    }

    pub(crate) fn quarantine_core(&self, core: &mut LifecycleCore<A>) {
        self.set_state(core, LifecycleStateKind::Quarantined);
    }

    pub(crate) fn install_generation_services_lease(
        &self,
        core: &mut LifecycleCore<A>,
        lease: GenerationServicesLease,
    ) {
        core.install_generation_services_lease(lease);
    }

    pub(crate) fn take_generation_services_lease(
        &self,
        core: &mut LifecycleCore<A>,
    ) -> Option<GenerationServicesLease> {
        core.take_generation_services_lease()
    }

    pub(crate) fn install_module_epoch(
        &self,
        core: &mut LifecycleCore<A>,
        lease: ModuleEpochLease,
    ) {
        core.install_module_epoch(lease);
    }

    pub(crate) fn take_module_epoch(
        &self,
        core: &mut LifecycleCore<A>,
    ) -> Option<ModuleEpochLease> {
        core.take_module_epoch()
    }

    pub(crate) fn stage_opening_generation(
        &self,
        opening: OpeningGeneration<A>,
    ) -> Result<(), (crate::XllError, OpeningGeneration<A>)> {
        let mut core = self.lock();
        if core.has_opening_generation() || core.has_current_generation() {
            return Err((
                crate::XllError::Internal {
                    diagnostic_id: crate::error::DiagnosticId::OPEN_STATE,
                },
                opening,
            ));
        }
        core.opening = Some(opening);
        Ok(())
    }

    pub(crate) fn publish_opening_generation_locked(
        &self,
        core: &mut LifecycleCore<A>,
        generation: RuntimeGeneration,
    ) -> Result<(), PublishOpeningError<A>> {
        if core.has_current_generation() {
            return Err(PublishOpeningError {
                error: crate::XllError::Internal {
                    diagnostic_id: crate::error::DiagnosticId::OPEN_STATE,
                },
                opening: core.opening.take(),
            });
        }
        let opening = core.opening.take().ok_or(PublishOpeningError {
            error: crate::XllError::Internal {
                diagnostic_id: crate::error::DiagnosticId::OPEN_STATE,
            },
            opening: None,
        })?;
        let OpeningGeneration {
            shared_state,
            layers,
            config: _,
        } = opening;
        let published = Arc::new(OpenGeneration {
            id: generation,
            shared_state,
            layers,
        });
        core.current = Some(Arc::clone(&published));
        self.current.store(Some(published));
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn has_opening_generation(&self) -> bool {
        self.lock().has_opening_generation()
    }

    #[cfg(test)]
    pub(crate) fn has_current_generation(&self) -> bool {
        self.lock().has_current_generation()
    }

    pub(crate) fn load_current_generation(
        &self,
    ) -> arc_swap::Guard<Option<Arc<OpenGeneration<A>>>> {
        self.current.load()
    }

    pub(crate) fn take_opening_for_rollback(&self) -> Option<OpeningGeneration<A>> {
        self.lock().opening.take()
    }

    #[cfg(test)]
    pub(crate) fn take_current_generation(&self) -> Option<Arc<OpenGeneration<A>>> {
        let mut core = self.lock();
        let current = core.current.take();
        if current.is_some() {
            self.current.store(None);
        }
        current
    }

    pub(crate) fn take_generation_for_shutdown(
        &self,
    ) -> Option<crate::runtime::ShutdownGeneration<A>> {
        let mut core = self.lock();
        debug_assert!(!(core.has_opening_generation() && core.has_current_generation()));
        if let Some(generation) = core.current.take() {
            self.current.store(None);
            return Some(crate::runtime::ShutdownGeneration::Open(generation));
        }
        core.opening
            .take()
            .map(crate::runtime::ShutdownGeneration::Opening)
    }
}

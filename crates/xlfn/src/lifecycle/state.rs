//! Canonical lifecycle ownership and its read-side projections.

use arc_swap::ArcSwapOption;
use parking_lot::{Condvar, Mutex, MutexGuard};
use std::mem;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

#[cold]
fn lifecycle_invariant_violation(message: &'static str) -> ! {
    #[cfg(not(test))]
    {
        tracing::error!(
            invariant = message,
            "lifecycle ownership invariant violated"
        );
        std::process::abort();
    }
    #[cfg(test)]
    panic!("lifecycle ownership invariant violated: {message}");
}

#[inline]
fn require_lifecycle_invariant(condition: bool, message: &'static str) {
    if !condition {
        lifecycle_invariant_violation(message);
    }
}

use crate::generation::{ExecutionGeneration, OpeningGeneration, ShutdownGeneration};
use crate::generation::{OpenAttemptId, RemovalAttemptId, RuntimeGeneration};
use crate::lifecycle::{HostLifecycleIntent, LifecyclePhase};
use crate::module_runtime::{ModuleAuthority, ModuleEpochId, ModuleEpochLease};
use crate::runtime_components::GenerationServices;

/// A read-side publication of one coherent open generation.
///
/// The root and its generation services are published together. A reader can
/// therefore never observe a generation root from one open attempt with
/// services from another attempt.
pub(crate) struct PublishedGeneration<A: crate::Addin> {
    pub(crate) root: Arc<ExecutionGeneration<A>>,
    pub(crate) services: Arc<GenerationServices>,
}

/// Read-side admission capability for one coherent open generation.
///
/// The admission publication is the sole hot-path witness that UDF calls may
/// enter. It is empty throughout opening and is cleared at the beginning of
/// closing, so a loaded publication already carries the lifecycle decision;
/// callers do not need to combine it with a separately observed phase.
pub(crate) struct GenerationAdmission<A: crate::Addin> {
    publication: arc_swap::Guard<Option<Arc<PublishedGeneration<A>>>>,
}

impl<A: crate::Addin> GenerationAdmission<A> {
    fn new(publication: arc_swap::Guard<Option<Arc<PublishedGeneration<A>>>>) -> Self {
        Self { publication }
    }

    pub(crate) fn generation(&self) -> &ExecutionGeneration<A> {
        &self
            .publication
            .as_ref()
            .expect("a live generation admission always observes a publication")
            .root
    }

    #[cfg(feature = "async")]
    pub(crate) fn generation_arc(&self) -> &Arc<ExecutionGeneration<A>> {
        &self
            .publication
            .as_ref()
            .expect("a live generation admission always observes a publication")
            .root
    }

    pub(crate) fn services(&self) -> &GenerationServices {
        &self
            .publication
            .as_ref()
            .expect("a live generation admission always observes a publication")
            .services
    }
}

/// The complete ownership bundle for a published generation.
pub(crate) struct OpenGeneration<A: crate::Addin> {
    generation: Arc<ExecutionGeneration<A>>,
    services: Arc<GenerationServices>,
    module_epoch: ModuleEpochIdentity,
}

/// Ownership retained after the generation root has been handed to the
/// shutdown/quiesce pipeline. The service root and module epoch identity
/// remain coupled after the close authority moves to the removal owner.
pub(crate) struct OpenRetirement {
    services: Arc<GenerationServices>,
    module_epoch: ModuleEpochIdentity,
}

/// Module epoch identity retained by a generation payload after the affine
/// mutation authority is moved into the canonical generation control block.
/// This is validation evidence only; it cannot close the module.
pub(crate) struct ModuleEpochIdentity {
    id: ModuleEpochId,
}

impl ModuleEpochIdentity {
    fn new(id: ModuleEpochId) -> Self {
        Self { id }
    }

    fn id(&self) -> ModuleEpochId {
        self.id
    }

    fn is_current(&self) -> bool {
        self.id().is_current()
    }
}

/// Payload states that can exist while an open attempt is still active.
///
/// An opening attempt can only be empty, staged, or published. In particular,
/// a retirement lease cannot be constructed under `Opening`, which removes a
/// whole class of lifecycle states that previously required runtime checks.
pub(crate) enum OpeningPayload<A: crate::Addin> {
    Empty,
    Staged(OpeningGeneration<A>),
    Published(OpenGeneration<A>),
}

impl<A: crate::Addin> OpeningPayload<A> {
    fn into_closing(self) -> ClosingPayload<A> {
        match self {
            Self::Empty => ClosingPayload::Empty,
            Self::Staged(opening) => ClosingPayload::Staged(opening),
            Self::Published(bundle) => ClosingPayload::Published(bundle),
        }
    }

    fn take_staged(&mut self) -> Option<OpeningGeneration<A>> {
        let payload = mem::replace(self, Self::Empty);
        match payload {
            Self::Staged(opening) => Some(opening),
            other => {
                *self = other;
                None
            }
        }
    }

    fn take_published(&mut self) -> Option<OpenGeneration<A>> {
        let payload = mem::replace(self, Self::Empty);
        match payload {
            Self::Published(bundle) => Some(bundle),
            other => {
                *self = other;
                None
            }
        }
    }

    fn module_epoch_id(&self) -> Option<ModuleEpochId> {
        match self {
            Self::Published(bundle) => Some(bundle.module_epoch.id()),
            Self::Empty | Self::Staged(_) => None,
        }
    }
}

/// Payload states that can survive into a closing, rollback, or quarantine
/// phase. These phases deliberately share the same retained-resource domain;
/// the active `LifecycleState` variant supplies the failure policy.
pub(crate) enum ClosingPayload<A: crate::Addin> {
    Empty,
    Staged(OpeningGeneration<A>),
    Published(OpenGeneration<A>),
    Retiring(OpenRetirement),
}

impl<A: crate::Addin> ClosingPayload<A> {
    fn is_published(&self) -> bool {
        matches!(self, Self::Published(_))
    }

    fn is_retiring(&self) -> bool {
        matches!(self, Self::Retiring(_))
    }

    fn module_epoch_is_current(&self) -> bool {
        match self {
            Self::Retiring(retirement) => retirement.module_epoch.is_current(),
            Self::Empty | Self::Staged(_) | Self::Published(_) => true,
        }
    }

    fn retiring_services(&self) -> Option<&Arc<GenerationServices>> {
        match self {
            Self::Retiring(retirement) => Some(&retirement.services),
            Self::Published(bundle) => Some(&bundle.services),
            Self::Empty | Self::Staged(_) => None,
        }
    }

    fn module_epoch_id(&self) -> Option<ModuleEpochId> {
        match self {
            Self::Published(bundle) => Some(bundle.module_epoch.id()),
            Self::Retiring(retirement) => Some(retirement.module_epoch.id()),
            Self::Empty | Self::Staged(_) => None,
        }
    }

    fn take_staged(&mut self) -> Option<OpeningGeneration<A>> {
        let payload = mem::replace(self, Self::Empty);
        match payload {
            Self::Staged(opening) => Some(opening),
            other => {
                *self = other;
                None
            }
        }
    }

    fn take_published(&mut self) -> Option<OpenGeneration<A>> {
        let payload = mem::replace(self, Self::Empty);
        match payload {
            Self::Published(bundle) => Some(bundle),
            other => {
                *self = other;
                None
            }
        }
    }

    fn take_retirement(&mut self) -> Option<OpenRetirement> {
        let payload = mem::replace(self, Self::Empty);
        match payload {
            Self::Retiring(retirement) => Some(retirement),
            other => {
                *self = other;
                None
            }
        }
    }
}

/// The canonical lifecycle facts needed by terminal certification.
///
/// Runtime teardown must not reconstruct protocol state by independently
/// asking whether an opening, publication, or retirement exists. This value
/// is derived once from `LifecycleState` while the canonical mutex is held,
/// so certification observes one coherent state-machine snapshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LifecycleResourceState {
    Empty,
    Opening,
    Published,
    Retiring,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LifecycleModuleEpoch {
    Absent,
    Current,
    Stale,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct LifecycleRemovalState {
    pub(crate) phase: LifecyclePhase,
    pub(crate) open_attempt: Option<OpenAttemptId>,
    pub(crate) removal_attempt: Option<RemovalAttemptId>,
    pub(crate) last_committed_generation: Option<RuntimeGeneration>,
    pub(crate) resources: LifecycleResourceState,
    pub(crate) module_epoch: LifecycleModuleEpoch,
}

impl LifecycleRemovalState {
    pub(crate) const fn has_opening_generation(self) -> bool {
        matches!(self.resources, LifecycleResourceState::Opening)
    }

    pub(crate) const fn has_current_generation(self) -> bool {
        matches!(self.resources, LifecycleResourceState::Published)
    }

    pub(crate) const fn has_retirement(self) -> bool {
        matches!(self.resources, LifecycleResourceState::Retiring)
    }

    pub(crate) const fn has_module_epoch(self) -> bool {
        !matches!(self.module_epoch, LifecycleModuleEpoch::Absent)
    }

    pub(crate) const fn module_epoch_is_current(self) -> bool {
        matches!(self.module_epoch, LifecycleModuleEpoch::Current)
    }
}

/// Canonical lifecycle state and its owned generation payload.
///
/// `LifecycleCoordinator::phase` is only a read-side projection of this
/// enum. The state machine below is deliberately non-`Copy`: moving between
/// phases also moves the staged generation, open bundle, or retirement lease.
pub(crate) enum LifecycleState<A: crate::Addin> {
    Closed,
    Opening {
        attempt: OpenAttemptId,
        payload: OpeningPayload<A>,
    },
    Open {
        bundle: OpenGeneration<A>,
    },
    Closing {
        open_attempt: Option<OpenAttemptId>,
        payload: ClosingPayload<A>,
    },
    OpenRollbackPending {
        payload: ClosingPayload<A>,
    },
    Quarantined {
        payload: ClosingPayload<A>,
    },
}

impl<A: crate::Addin> LifecycleState<A> {
    pub(crate) const fn phase(&self) -> LifecyclePhase {
        match self {
            Self::Closed => LifecyclePhase::Closed,
            Self::Opening { .. } => LifecyclePhase::Opening,
            Self::Open { .. } => LifecyclePhase::Open,
            Self::Closing { .. } => LifecyclePhase::Closing,
            Self::OpenRollbackPending { .. } => LifecyclePhase::OpenRollbackPending,
            Self::Quarantined { .. } => LifecyclePhase::Quarantined,
        }
    }

    pub(crate) const fn open_attempt(&self) -> Option<OpenAttemptId> {
        match self {
            Self::Opening { attempt, .. } => Some(*attempt),
            Self::Closing { open_attempt, .. } => *open_attempt,
            Self::Closed
            | Self::Open { .. }
            | Self::OpenRollbackPending { .. }
            | Self::Quarantined { .. } => None,
        }
    }

    fn protocol_generation(
        &self,
        last_committed: Option<RuntimeGeneration>,
    ) -> Option<RuntimeGeneration> {
        match self {
            Self::Opening { attempt, .. } => Some(attempt.into_runtime_generation()),
            Self::Open { bundle } => Some(bundle.generation.id()),
            Self::Closing {
                open_attempt: Some(attempt),
                ..
            } => Some(attempt.into_runtime_generation()),
            Self::Closing {
                open_attempt: None, ..
            }
            | Self::OpenRollbackPending { .. } => last_committed,
            Self::Closed | Self::Quarantined { .. } => None,
        }
    }

    fn opening(&self) -> Option<&OpeningGeneration<A>> {
        match self {
            Self::Opening { payload, .. } => match payload {
                OpeningPayload::Staged(opening) => Some(opening),
                OpeningPayload::Empty | OpeningPayload::Published(_) => None,
            },
            Self::Closing { payload, .. }
            | Self::OpenRollbackPending { payload }
            | Self::Quarantined { payload } => match payload {
                ClosingPayload::Staged(opening) => Some(opening),
                ClosingPayload::Empty
                | ClosingPayload::Published(_)
                | ClosingPayload::Retiring(_) => None,
            },
            Self::Closed | Self::Open { .. } => None,
        }
    }

    fn has_current_generation(&self) -> bool {
        match self {
            Self::Open { .. } => true,
            Self::Opening { payload, .. } => matches!(payload, OpeningPayload::Published(_)),
            Self::Closing { payload, .. }
            | Self::OpenRollbackPending { payload }
            | Self::Quarantined { payload } => payload.is_published(),
            Self::Closed => false,
        }
    }

    fn retiring_services(&self) -> Option<&Arc<GenerationServices>> {
        let payload = match self {
            Self::Closing { payload, .. }
            | Self::OpenRollbackPending { payload }
            | Self::Quarantined { payload } => payload,
            Self::Closed | Self::Open { .. } | Self::Opening { .. } => return None,
        };
        payload.retiring_services()
    }

    fn module_epoch_is_current(&self) -> bool {
        match self {
            Self::Closing { payload, .. }
            | Self::OpenRollbackPending { payload }
            | Self::Quarantined { payload } => payload.module_epoch_is_current(),
            Self::Closed | Self::Open { .. } | Self::Opening { .. } => true,
        }
    }

    fn module_epoch_id(&self) -> Option<ModuleEpochId> {
        match self {
            Self::Opening { payload, .. } => payload.module_epoch_id(),
            Self::Open { bundle } => Some(bundle.module_epoch.id()),
            Self::Closing { payload, .. }
            | Self::OpenRollbackPending { payload }
            | Self::Quarantined { payload } => payload.module_epoch_id(),
            Self::Closed => None,
        }
    }

    fn removal_state(
        &self,
        removal_attempt: Option<RemovalAttemptId>,
        last_committed_generation: Option<RuntimeGeneration>,
    ) -> LifecycleRemovalState {
        let resources = match self {
            Self::Closed => LifecycleResourceState::Empty,
            Self::Opening { payload, .. } => match payload {
                OpeningPayload::Empty => LifecycleResourceState::Empty,
                OpeningPayload::Staged(_) => LifecycleResourceState::Opening,
                OpeningPayload::Published(_) => LifecycleResourceState::Published,
            },
            Self::Open { .. } => LifecycleResourceState::Published,
            Self::Closing { payload, .. }
            | Self::OpenRollbackPending { payload }
            | Self::Quarantined { payload } => match payload {
                ClosingPayload::Empty => LifecycleResourceState::Empty,
                ClosingPayload::Staged(_) => LifecycleResourceState::Opening,
                ClosingPayload::Published(_) => LifecycleResourceState::Published,
                ClosingPayload::Retiring(_) => LifecycleResourceState::Retiring,
            },
        };
        let module_epoch = match self {
            Self::Closing { payload, .. }
            | Self::OpenRollbackPending { payload }
            | Self::Quarantined { payload }
                if payload.is_retiring() =>
            {
                if self.module_epoch_is_current() {
                    LifecycleModuleEpoch::Current
                } else {
                    LifecycleModuleEpoch::Stale
                }
            }
            Self::Closed
            | Self::Opening { .. }
            | Self::Open { .. }
            | Self::Closing { .. }
            | Self::OpenRollbackPending { .. }
            | Self::Quarantined { .. } => LifecycleModuleEpoch::Absent,
        };
        LifecycleRemovalState {
            phase: self.phase(),
            open_attempt: self.open_attempt(),
            removal_attempt,
            last_committed_generation,
            resources,
            module_epoch,
        }
    }

    fn take_opening(&mut self) -> Option<OpeningGeneration<A>> {
        match self {
            Self::Opening { payload, .. } => payload.take_staged(),
            Self::Closing { payload, .. }
            | Self::OpenRollbackPending { payload }
            | Self::Quarantined { payload } => payload.take_staged(),
            Self::Closed | Self::Open { .. } => None,
        }
    }

    fn take_open_bundle(&mut self) -> Option<OpenGeneration<A>> {
        let state = mem::replace(self, Self::Closed);
        match state {
            Self::Open { bundle } => Some(bundle),
            Self::Opening {
                attempt,
                mut payload,
            } => {
                let bundle = payload.take_published();
                *self = Self::Opening { attempt, payload };
                bundle
            }
            Self::Closing {
                open_attempt,
                mut payload,
            } => {
                let bundle = payload.take_published();
                *self = Self::Closing {
                    open_attempt,
                    payload,
                };
                bundle
            }
            Self::OpenRollbackPending { mut payload } => {
                let bundle = payload.take_published();
                *self = Self::OpenRollbackPending { payload };
                bundle
            }
            Self::Quarantined { mut payload } => {
                let bundle = payload.take_published();
                *self = Self::Quarantined { payload };
                bundle
            }
            Self::Closed => None,
        }
    }

    fn take_retirement(&mut self) -> Option<OpenRetirement> {
        match self {
            Self::Closing { payload, .. }
            | Self::OpenRollbackPending { payload }
            | Self::Quarantined { payload } => payload.take_retirement(),
            Self::Closed | Self::Open { .. } | Self::Opening { .. } => None,
        }
    }

    fn install_retirement(&mut self, retirement: OpenRetirement) {
        let state = mem::replace(self, Self::Closed);
        *self = match state {
            Self::Closed | Self::Open { .. } => Self::Closing {
                open_attempt: None,
                payload: ClosingPayload::Retiring(retirement),
            },
            Self::Opening { attempt, payload } => {
                require_lifecycle_invariant(
                    matches!(payload, OpeningPayload::Empty),
                    "retirement installed while opening payload is present",
                );
                Self::Closing {
                    open_attempt: Some(attempt),
                    payload: ClosingPayload::Retiring(retirement),
                }
            }
            Self::Closing {
                open_attempt,
                payload,
            } => {
                require_lifecycle_invariant(
                    matches!(payload, ClosingPayload::Empty),
                    "retirement installed while closing payload is present",
                );
                Self::Closing {
                    open_attempt,
                    payload: ClosingPayload::Retiring(retirement),
                }
            }
            Self::OpenRollbackPending { payload } => {
                require_lifecycle_invariant(
                    matches!(payload, ClosingPayload::Empty),
                    "retirement installed while rollback payload is present",
                );
                Self::OpenRollbackPending {
                    payload: ClosingPayload::Retiring(retirement),
                }
            }
            Self::Quarantined { payload } => {
                require_lifecycle_invariant(
                    matches!(payload, ClosingPayload::Empty),
                    "retirement installed while quarantine payload is present",
                );
                Self::Quarantined {
                    payload: ClosingPayload::Retiring(retirement),
                }
            }
        };
    }

    fn into_payload(self) -> ClosingPayload<A> {
        match self {
            Self::Closed => ClosingPayload::Empty,
            Self::Opening { payload, .. } => payload.into_closing(),
            Self::Closing { payload, .. }
            | Self::OpenRollbackPending { payload }
            | Self::Quarantined { payload } => payload,
            Self::Open { bundle } => ClosingPayload::Published(bundle),
        }
    }
}

/// The owner-side control block for one runtime generation domain.
///
/// Generation payloads retain only the immutable execution root, services,
/// and module epoch identity. The affine module mutation authority lives here
/// beside the canonical lifecycle state, so it cannot be duplicated in a
/// runtime side slot or hidden in a payload.
struct GenerationControl<A: crate::Addin> {
    state: LifecycleState<A>,
    module_authority: Option<ModuleAuthority>,
}

impl<A: crate::Addin> GenerationControl<A> {
    const fn new() -> Self {
        Self {
            state: LifecycleState::Closed,
            module_authority: None,
        }
    }

    fn take_module_closing(&mut self) -> Option<crate::module_runtime::ModuleClosing> {
        self.module_authority
            .take()
            .map(ModuleAuthority::into_closing)
    }

    fn install_open_authority(&mut self, lease: ModuleEpochLease) {
        require_lifecycle_invariant(
            self.module_authority.is_none(),
            "module open authority already exists",
        );
        self.module_authority = Some(ModuleAuthority::Open(lease));
    }

    fn install_closing_authority(&mut self, closing: crate::module_runtime::ModuleClosing) {
        require_lifecycle_invariant(
            self.module_authority.is_none(),
            "module close authority already exists",
        );
        self.module_authority = Some(ModuleAuthority::Closing(closing));
    }
}

/// Canonical owner of every mutable lifecycle decision and generation root.
struct LifecycleCore<A: crate::Addin> {
    generation: GenerationControl<A>,
    host_intent: HostLifecycleIntent,
    next_lifecycle_attempt: u64,
    next_removal_attempt: u64,
    last_committed_generation: Option<RuntimeGeneration>,
    removal_epoch: u64,
    removal_attempt: Option<RemovalAttemptId>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum OpenFailureDisposition {
    RollbackRequired,
    ClosingOwnsCleanup,
}

impl OpenFailureDisposition {
    pub(crate) const fn requires_rollback(self) -> bool {
        matches!(self, Self::RollbackRequired)
    }
}

impl<A: crate::Addin> LifecycleCore<A> {
    const fn new() -> Self {
        Self {
            generation: GenerationControl::new(),
            host_intent: HostLifecycleIntent::None,
            next_lifecycle_attempt: 1,
            next_removal_attempt: 1,
            last_committed_generation: None,
            removal_epoch: 0,
            removal_attempt: None,
        }
    }

    /// Returns the mutex-protected canonical state. It is intentionally a
    /// reference because the state owns the phase payload.
    const fn canonical_state(&self) -> &LifecycleState<A> {
        &self.generation.state
    }

    const fn host_intent(&self) -> HostLifecycleIntent {
        self.host_intent
    }

    const fn last_committed_generation(&self) -> Option<RuntimeGeneration> {
        self.last_committed_generation
    }

    fn protocol_generation(&self) -> Option<RuntimeGeneration> {
        self.generation
            .state
            .protocol_generation(self.last_committed_generation)
    }

    fn removal_state(&self) -> LifecycleRemovalState {
        self.generation
            .state
            .removal_state(self.removal_attempt, self.last_committed_generation)
    }

    const fn removal_epoch(&self) -> u64 {
        self.removal_epoch
    }

    fn take_module_closing(&mut self) -> Option<crate::module_runtime::ModuleClosing> {
        self.generation.take_module_closing()
    }

    fn module_authority_id(&self) -> Option<ModuleEpochId> {
        self.generation
            .module_authority
            .as_ref()
            .map(ModuleAuthority::id)
    }

    fn install_open_authority(&mut self, lease: ModuleEpochLease) {
        self.generation.install_open_authority(lease);
    }

    fn install_closing_authority(&mut self, closing: crate::module_runtime::ModuleClosing) {
        self.generation.install_closing_authority(closing);
    }

    const fn removal_attempt(&self) -> Option<RemovalAttemptId> {
        self.removal_attempt
    }

    #[cfg(test)]
    fn set_next_lifecycle_attempt_for_test(&mut self, value: u64) {
        self.next_lifecycle_attempt = value;
    }

    fn opening_config(&self) -> Option<crate::addin::RuntimeConfig> {
        self.generation
            .state
            .opening()
            .map(|opening| opening.init_config)
    }

    fn has_current_generation(&self) -> bool {
        self.generation.state.has_current_generation()
    }

    fn retiring_services(&self) -> Option<&Arc<GenerationServices>> {
        self.generation.state.retiring_services()
    }
}

/// Opaque access to the canonical lifecycle state.
///
/// Callers can request only protocol observations or invoke coordinator
/// transitions with this guard. The `LifecycleCore` and its mutex remain
/// private to this module, so runtime orchestration cannot construct or
/// mutate an arbitrary core state.
pub(crate) struct LifecycleAccess<'a, A: crate::Addin> {
    coordinator: &'a LifecycleCoordinator<A>,
    core: MutexGuard<'a, LifecycleCore<A>>,
}

impl<A: crate::Addin> LifecycleAccess<'_, A> {
    /// Applies one canonical mutation and commits its read-side projection
    /// before returning. Lifecycle writers must use this boundary instead of
    /// mutating the core and committing later: an early return can no longer
    /// leave `phase` or `publication` stale.
    fn transition<R>(
        &mut self,
        mutation: impl FnOnce(&mut LifecycleCore<A>) -> (R, TransitionEffect<A>),
    ) -> R {
        let (result, effect) = mutation(&mut self.core);
        self.coordinator.commit_transition(self, effect);
        result
    }

    fn canonical_state(&self) -> &LifecycleState<A> {
        self.core.canonical_state()
    }

    pub(crate) fn phase(&self) -> LifecyclePhase {
        self.canonical_state().phase()
    }

    pub(crate) fn host_intent(&self) -> HostLifecycleIntent {
        self.core.host_intent()
    }

    pub(crate) fn last_committed_generation(&self) -> Option<RuntimeGeneration> {
        self.core.last_committed_generation()
    }

    pub(crate) fn protocol_generation(&self) -> Option<RuntimeGeneration> {
        self.core.protocol_generation()
    }

    pub(crate) fn removal_state(&self) -> LifecycleRemovalState {
        self.core.removal_state()
    }

    pub(crate) fn module_epoch_id(&self) -> Option<ModuleEpochId> {
        self.canonical_state()
            .module_epoch_id()
            .or_else(|| self.core.module_authority_id())
    }

    pub(crate) fn removal_epoch(&self) -> u64 {
        self.core.removal_epoch()
    }

    pub(crate) fn open_attempt(&self) -> Option<OpenAttemptId> {
        self.canonical_state().open_attempt()
    }

    pub(crate) fn removal_attempt(&self) -> Option<RemovalAttemptId> {
        self.core.removal_attempt()
    }

    pub(crate) fn opening_config(&self) -> Option<crate::addin::RuntimeConfig> {
        self.core.opening_config()
    }

    #[cfg(test)]
    pub(crate) fn has_current_generation(&self) -> bool {
        self.core.has_current_generation()
    }

    pub(crate) fn retiring_services(&self) -> Option<&Arc<GenerationServices>> {
        self.core.retiring_services()
    }

    #[cfg(test)]
    pub(crate) fn set_next_lifecycle_attempt_for_test(&mut self, value: u64) {
        self.core.set_next_lifecycle_attempt_for_test(value);
    }
}

/// Lifecycle synchronization state.
///
/// `core` is the canonical ownership boundary. `phase` and `publication` are
/// read-side projections used by hot-path admission and generation/service
/// access; lifecycle writers mutate `core` first and then update projections.
pub(crate) struct LifecycleCoordinator<A: crate::Addin> {
    phase: AtomicU8,
    publication: ArcSwapOption<PublishedGeneration<A>>,
    core: Mutex<LifecycleCore<A>>,
    changed: Condvar,
    #[cfg(any(test, feature = "refinement", feature = "bench-internals"))]
    test_services: Mutex<Option<Arc<GenerationServices>>>,
    #[cfg(test)]
    pub(crate) test_module_lease: Mutex<Option<crate::ingress::TestModuleLease>>,
}

pub(crate) struct PublishOpeningError<A: crate::Addin> {
    pub(crate) error: crate::XllError,
    pub(crate) opening: Option<OpeningGeneration<A>>,
}

/// The read-side effect produced by one canonical lifecycle transition.
///
/// A transition mutates `LifecycleCore` first and then commits exactly one of
/// these effects. Keeping the effect with the projection commit prevents a
/// writer from updating `phase` without updating `publication`, or from
/// publishing a generation after the closing phase has become visible.
enum TransitionEffect<A: crate::Addin> {
    Keep,
    ClearPublication,
    Publish(Arc<PublishedGeneration<A>>),
}

impl<A: crate::Addin> LifecycleCoordinator<A> {
    pub(crate) const fn new() -> Self {
        Self {
            phase: AtomicU8::new(LifecyclePhase::Closed as u8),
            publication: ArcSwapOption::const_empty(),
            core: Mutex::new(LifecycleCore::new()),
            changed: Condvar::new(),
            #[cfg(any(test, feature = "refinement", feature = "bench-internals"))]
            test_services: Mutex::new(None),
            #[cfg(test)]
            test_module_lease: Mutex::new(None),
        }
    }

    pub(crate) fn access(&self) -> LifecycleAccess<'_, A> {
        LifecycleAccess {
            coordinator: self,
            core: self.core.lock(),
        }
    }

    pub(in crate::lifecycle) fn take_module_closing_for_close(
        &self,
        access: &mut LifecycleAccess<'_, A>,
    ) -> Option<crate::module_runtime::ModuleClosing> {
        access.transition(|core| (core.take_module_closing(), TransitionEffect::Keep))
    }

    pub(in crate::lifecycle) fn take_module_closing_for_quarantine(
        &self,
    ) -> Option<crate::module_runtime::ModuleClosing> {
        let mut access = self.access();
        access.transition(|core| (core.take_module_closing(), TransitionEffect::Keep))
    }

    pub(in crate::lifecycle) fn install_module_closing(
        &self,
        closing: crate::module_runtime::ModuleClosing,
    ) {
        let mut access = self.access();
        self.install_module_closing_locked(&mut access, closing);
    }

    pub(in crate::lifecycle) fn install_module_closing_locked(
        &self,
        access: &mut LifecycleAccess<'_, A>,
        closing: crate::module_runtime::ModuleClosing,
    ) {
        access.transition(|core| {
            core.install_closing_authority(closing);
            ((), TransitionEffect::Keep)
        });
    }

    pub(in crate::lifecycle) fn clear_certified_retirement(
        &self,
        access: &mut LifecycleAccess<'_, A>,
    ) -> bool {
        access.transition(|core| {
            let Some(retirement) = core.generation.state.take_retirement() else {
                return (false, TransitionEffect::Keep);
            };
            drop(retirement.services);
            (true, TransitionEffect::ClearPublication)
        })
    }

    pub(crate) fn wait<'a>(&self, access: &mut LifecycleAccess<'a, A>) {
        self.changed.wait(&mut access.core);
    }

    pub(crate) fn notify_all(&self) {
        self.changed.notify_all();
    }

    /// Returns the read-side phase projection.
    pub(crate) fn observed_phase(&self) -> LifecyclePhase {
        LifecyclePhase::from_raw(self.phase.load(Ordering::Acquire))
    }

    /// Admits one call from the published generation projection.
    ///
    /// Opening has no publication and closing clears it before the lifecycle
    /// phase changes, so one ArcSwap load is sufficient for the hot path.
    pub(crate) fn try_admit(&self) -> crate::XllResult<GenerationAdmission<A>> {
        let publication = self.publication.load();
        if publication.is_some() {
            Ok(GenerationAdmission::new(publication))
        } else {
            Err(crate::XllError::Closing)
        }
    }

    pub(in crate::lifecycle) fn set_host_intent(&self, intent: HostLifecycleIntent) {
        let mut access = self.access();
        access.transition(|core| {
            core.host_intent = intent;
            ((), TransitionEffect::Keep)
        });
    }

    fn set_removal_attempt(
        &self,
        access: &mut LifecycleAccess<'_, A>,
        attempt: Option<RemovalAttemptId>,
    ) {
        access.transition(|core| {
            core.removal_attempt = attempt;
            ((), TransitionEffect::Keep)
        });
    }

    fn advance_removal_epoch(&self, access: &mut LifecycleAccess<'_, A>) {
        access.transition(|core| {
            core.removal_epoch = core.removal_epoch.checked_add(1).unwrap_or_else(|| {
                tracing::error!("lifecycle close epoch exhausted; fail-stopping");
                std::process::abort();
            });
            ((), TransitionEffect::Keep)
        });
    }

    fn next_lifecycle_attempt_id(
        &self,
        access: &mut LifecycleAccess<'_, A>,
    ) -> crate::XllResult<OpenAttemptId> {
        access.transition(|core| {
            let attempt_id = core.next_lifecycle_attempt;
            let Some(next) = attempt_id.checked_add(1) else {
                return (
                    Err(crate::XllError::Internal {
                        diagnostic_id: crate::diagnostics::id::DiagnosticId::ATTEMPT_OVERFLOW,
                    }),
                    TransitionEffect::Keep,
                );
            };
            let Some(attempt) = OpenAttemptId::new(attempt_id) else {
                return (
                    Err(crate::XllError::Internal {
                        diagnostic_id: crate::diagnostics::id::DiagnosticId::ATTEMPT_ZERO,
                    }),
                    TransitionEffect::Keep,
                );
            };
            core.next_lifecycle_attempt = next;
            (Ok(attempt), TransitionEffect::Keep)
        })
    }

    fn next_removal_attempt_id(&self, access: &mut LifecycleAccess<'_, A>) -> RemovalAttemptId {
        access.transition(|core| {
            let attempt_id = core.next_removal_attempt;
            let next = attempt_id.checked_add(1).unwrap_or_else(|| {
                tracing::error!("lifecycle removal-attempt identity exhausted; fail-stopping");
                std::process::abort();
            });
            core.next_removal_attempt = next;
            let attempt = RemovalAttemptId::new(attempt_id).unwrap_or_else(|| {
                tracing::error!("lifecycle removal-attempt identity reached zero; fail-stopping");
                std::process::abort();
            });
            (attempt, TransitionEffect::Keep)
        })
    }

    /// Commits the read-side projection for a canonical transition.
    ///
    /// The ordering is part of the lifecycle protocol: publication is
    /// changed before the phase projection, and waiters are notified only
    /// after both projections describe the new canonical state. In
    /// particular, closing clears admission before `Closing` becomes visible,
    /// while opening publishes the coherent root/services pair before
    /// `Open` becomes visible.
    fn commit_transition(&self, access: &LifecycleAccess<'_, A>, effect: TransitionEffect<A>) {
        match effect {
            TransitionEffect::Keep => {}
            TransitionEffect::ClearPublication => self.publication.store(None),
            TransitionEffect::Publish(publication) => self.publication.store(Some(publication)),
        }
        self.phase.store(access.phase() as u8, Ordering::Release);
        self.changed.notify_all();
    }

    fn publish_effect(bundle: &OpenGeneration<A>) -> TransitionEffect<A> {
        TransitionEffect::Publish(Arc::new(PublishedGeneration {
            root: Arc::clone(&bundle.generation),
            services: Arc::clone(&bundle.services),
        }))
    }

    /// Clears host intent before the external module-open protocol is started.
    pub(in crate::lifecycle) fn prepare_open(&self, access: &mut LifecycleAccess<'_, A>) {
        require_lifecycle_invariant(
            access.phase() == LifecyclePhase::Closed,
            "open preparation requires the closed lifecycle phase",
        );
        access.transition(|core| {
            core.host_intent = HostLifecycleIntent::None;
            ((), TransitionEffect::Keep)
        });
    }

    pub(in crate::lifecycle) fn allocate_open_attempt(
        &self,
        access: &mut LifecycleAccess<'_, A>,
    ) -> crate::XllResult<OpenAttemptId> {
        self.next_lifecycle_attempt_id(access)
    }

    pub(in crate::lifecycle) fn begin_opening(
        &self,
        access: &mut LifecycleAccess<'_, A>,
        attempt: OpenAttemptId,
    ) {
        require_lifecycle_invariant(
            access.phase() == LifecyclePhase::Closed,
            "opening requires the closed lifecycle phase",
        );
        require_lifecycle_invariant(
            access.removal_attempt().is_none(),
            "opening cannot begin while removal owns the lifecycle",
        );
        access.transition(|core| {
            core.generation.state = LifecycleState::Opening {
                attempt,
                payload: OpeningPayload::Empty,
            };
            ((), TransitionEffect::Keep)
        });
    }

    /// Publishes a successfully assembled generation while retaining the
    /// opening attempt until `commit_open` completes the lifecycle transition.
    pub(in crate::lifecycle) fn commit_open(
        &self,
        core: &mut LifecycleAccess<'_, A>,
        generation: RuntimeGeneration,
    ) -> crate::XllResult<()> {
        core.transition(|core| {
            let state = mem::replace(&mut core.generation.state, LifecycleState::Closed);
            match state {
                LifecycleState::Opening {
                    attempt,
                    payload: OpeningPayload::Published(bundle),
                } => {
                    if attempt.into_runtime_generation() != generation
                        || bundle.generation.id() != generation
                    {
                        core.generation.state = LifecycleState::Opening {
                            attempt,
                            payload: OpeningPayload::Published(bundle),
                        };
                        return (
                            Err(crate::XllError::Internal {
                                diagnostic_id: crate::diagnostics::id::DiagnosticId::OPEN_STATE,
                            }),
                            TransitionEffect::Keep,
                        );
                    }
                    core.last_committed_generation = Some(generation);
                    core.generation.state = LifecycleState::Open { bundle };
                    let effect = if let LifecycleState::Open { bundle } = core.canonical_state() {
                        Self::publish_effect(bundle)
                    } else {
                        unreachable!("open bundle was just installed");
                    };
                    (Ok(()), effect)
                }
                other => {
                    core.generation.state = other;
                    (
                        Err(crate::XllError::Internal {
                            diagnostic_id: crate::diagnostics::id::DiagnosticId::OPEN_STATE,
                        }),
                        TransitionEffect::Keep,
                    )
                }
            }
        })
    }

    pub(in crate::lifecycle) fn reject_open_attempt(&self, core: &mut LifecycleAccess<'_, A>) {
        core.transition(|core| {
            let state = mem::replace(&mut core.generation.state, LifecycleState::Closed);
            core.generation.state = match state {
                LifecycleState::Closing { payload, .. } => LifecycleState::Closing {
                    payload,
                    open_attempt: None,
                },
                LifecycleState::OpenRollbackPending { payload } => {
                    LifecycleState::OpenRollbackPending { payload }
                }
                LifecycleState::Quarantined { payload } => LifecycleState::Quarantined { payload },
                other => other,
            };
            ((), TransitionEffect::Keep)
        });
    }

    /// Records an open failure without discarding the owned staged/published
    /// payload. The rollback pipeline can then take that payload explicitly.
    pub(in crate::lifecycle) fn record_open_failure(
        &self,
        core: &mut LifecycleAccess<'_, A>,
    ) -> OpenFailureDisposition {
        core.transition(|core| {
            let state = mem::replace(&mut core.generation.state, LifecycleState::Closed);
            let (state, disposition) = match state {
                LifecycleState::Opening { payload, .. } => (
                    LifecycleState::OpenRollbackPending {
                        payload: payload.into_closing(),
                    },
                    OpenFailureDisposition::RollbackRequired,
                ),
                LifecycleState::OpenRollbackPending { payload } => (
                    LifecycleState::OpenRollbackPending { payload },
                    OpenFailureDisposition::RollbackRequired,
                ),
                LifecycleState::Closing { payload, .. } => (
                    LifecycleState::Closing {
                        payload,
                        open_attempt: None,
                    },
                    OpenFailureDisposition::ClosingOwnsCleanup,
                ),
                other => (other, OpenFailureDisposition::ClosingOwnsCleanup),
            };
            core.generation.state = state;
            (disposition, TransitionEffect::Keep)
        })
    }

    /// Requests closing while moving the active generation payload under the
    /// closing phase. No payload remains in a separate core field.
    pub(in crate::lifecycle) fn request_closing(&self, core: &mut LifecycleAccess<'_, A>) {
        if core.core.canonical_state().phase() == LifecyclePhase::Closed
            && core.core.removal_attempt().is_some()
        {
            return;
        }
        core.transition(|core| {
            let state = mem::replace(&mut core.generation.state, LifecycleState::Closed);
            core.generation.state = match state {
                LifecycleState::Closed => LifecycleState::Closing {
                    open_attempt: None,
                    payload: ClosingPayload::Empty,
                },
                LifecycleState::Opening { attempt, payload } => LifecycleState::Closing {
                    open_attempt: Some(attempt),
                    payload: payload.into_closing(),
                },
                LifecycleState::Open { bundle } => LifecycleState::Closing {
                    open_attempt: None,
                    payload: ClosingPayload::Published(bundle),
                },
                LifecycleState::Closing {
                    open_attempt,
                    payload,
                } => LifecycleState::Closing {
                    open_attempt,
                    payload,
                },
                LifecycleState::OpenRollbackPending { payload } => LifecycleState::Closing {
                    open_attempt: None,
                    payload,
                },
                LifecycleState::Quarantined { payload } => LifecycleState::Quarantined { payload },
            };
            let effect = if matches!(core.canonical_state(), LifecycleState::Closing { .. }) {
                TransitionEffect::ClearPublication
            } else {
                TransitionEffect::Keep
            };
            ((), effect)
        });
    }

    pub(in crate::lifecycle) fn begin_removal_request(&self, core: &mut LifecycleAccess<'_, A>) {
        self.advance_removal_epoch(core);
    }

    pub(in crate::lifecycle) fn claim_removal_owner(
        &self,
        core: &mut LifecycleAccess<'_, A>,
    ) -> Option<RemovalAttemptId> {
        if matches!(
            core.core.canonical_state().phase(),
            LifecyclePhase::Closed | LifecyclePhase::Quarantined
        ) || core.core.canonical_state().open_attempt().is_some()
            || core.core.removal_attempt().is_some()
        {
            return None;
        }
        let attempt = self.next_removal_attempt_id(core);
        self.set_removal_attempt(core, Some(attempt));
        Some(attempt)
    }

    pub(in crate::lifecycle) fn release_removal_owner(
        &self,
        core: &mut LifecycleAccess<'_, A>,
        attempt: RemovalAttemptId,
    ) {
        require_lifecycle_invariant(
            core.core.removal_attempt() == Some(attempt),
            "removal owner identity does not match the canonical lifecycle owner",
        );
        self.set_removal_attempt(core, None);
    }

    pub(in crate::lifecycle) fn finish_closed(&self, core: &mut LifecycleAccess<'_, A>) {
        require_lifecycle_invariant(
            matches!(
                core.core.canonical_state(),
                LifecycleState::Closed
                    | LifecycleState::Closing {
                        payload: ClosingPayload::Empty,
                        ..
                    }
                    | LifecycleState::OpenRollbackPending {
                        payload: ClosingPayload::Empty
                    }
            ),
            "closed publication requires an empty lifecycle payload",
        );
        core.transition(|core| {
            core.generation.state = LifecycleState::Closed;
            ((), TransitionEffect::ClearPublication)
        });
    }

    pub(in crate::lifecycle) fn quarantine_core(&self, core: &mut LifecycleAccess<'_, A>) {
        core.transition(|core| {
            let state = mem::replace(&mut core.generation.state, LifecycleState::Closed);
            core.generation.state = LifecycleState::Quarantined {
                payload: state.into_payload(),
            };
            ((), TransitionEffect::ClearPublication)
        });
    }

    pub(in crate::lifecycle) fn stage_opening_generation_locked(
        &self,
        core: &mut LifecycleAccess<'_, A>,
        opening: OpeningGeneration<A>,
    ) -> Result<(), (crate::XllError, OpeningGeneration<A>)> {
        core.transition(|core| {
            let state = mem::replace(&mut core.generation.state, LifecycleState::Closed);
            match state {
                LifecycleState::Opening {
                    attempt,
                    payload: OpeningPayload::Empty,
                } => {
                    core.generation.state = LifecycleState::Opening {
                        attempt,
                        payload: OpeningPayload::Staged(opening),
                    };
                    (Ok(()), TransitionEffect::Keep)
                }
                other => {
                    core.generation.state = other;
                    (
                        Err((
                            crate::XllError::Internal {
                                diagnostic_id: crate::diagnostics::id::DiagnosticId::OPEN_STATE,
                            },
                            opening,
                        )),
                        TransitionEffect::Keep,
                    )
                }
            }
        })
    }

    pub(in crate::lifecycle) fn publish_opening_generation_locked(
        &self,
        core: &mut LifecycleAccess<'_, A>,
        generation: RuntimeGeneration,
        services: Arc<GenerationServices>,
        module_epoch: ModuleEpochLease,
    ) -> Result<(), PublishOpeningError<A>> {
        core.transition(|core| {
            let open_state_error = || crate::XllError::Internal {
                diagnostic_id: crate::diagnostics::id::DiagnosticId::OPEN_STATE,
            };
            if core.has_current_generation() {
                return (
                    Err(PublishOpeningError {
                        error: open_state_error(),
                        opening: core.generation.state.take_opening(),
                    }),
                    TransitionEffect::Keep,
                );
            }
            let Some(attempt) = (match core.canonical_state() {
                LifecycleState::Opening { attempt, .. } => Some(*attempt),
                _ => None,
            }) else {
                return (
                    Err(PublishOpeningError {
                        error: open_state_error(),
                        opening: core.generation.state.take_opening(),
                    }),
                    TransitionEffect::Keep,
                );
            };
            let Some(opening) = core.generation.state.take_opening() else {
                return (
                    Err(PublishOpeningError {
                        error: open_state_error(),
                        opening: None,
                    }),
                    TransitionEffect::Keep,
                );
            };
            let OpeningGeneration {
                shared_state,
                layers,
                init_config: _,
            } = opening;
            let published = Arc::new(ExecutionGeneration {
                id: generation,
                shared_state,
                layers,
            });
            let module_epoch_id = module_epoch.id();
            core.install_open_authority(module_epoch);
            let bundle = OpenGeneration {
                generation: Arc::clone(&published),
                services,
                module_epoch: ModuleEpochIdentity::new(module_epoch_id),
            };
            core.generation.state = LifecycleState::Opening {
                attempt,
                payload: OpeningPayload::Published(bundle),
            };
            (Ok(()), TransitionEffect::Keep)
        })
    }

    #[cfg(test)]
    pub(crate) fn has_opening_generation(&self) -> bool {
        self.access().removal_state().has_opening_generation()
    }

    #[cfg(test)]
    pub(crate) fn has_current_generation(&self) -> bool {
        self.access().has_current_generation()
    }

    /// Service access is a cold-path operation. It borrows the coherent
    /// publication long enough to clone the service root; no independent
    /// production projection exists.
    pub(crate) fn load_generation_services(&self) -> Option<Arc<GenerationServices>> {
        let publication = self.publication.load();
        if let Some(publication) = publication.as_ref() {
            return Some(Arc::clone(&publication.services));
        }
        #[cfg(any(test, feature = "refinement", feature = "bench-internals"))]
        {
            return self.test_services.lock().clone();
        }
        #[cfg(not(any(test, feature = "refinement", feature = "bench-internals")))]
        None
    }

    pub(in crate::lifecycle) fn take_opening_for_rollback(&self) -> Option<OpeningGeneration<A>> {
        let mut access = self.access();
        access.transition(|core| (core.generation.state.take_opening(), TransitionEffect::Keep))
    }

    fn take_current_bundle(
        &self,
        core: &mut LifecycleAccess<'_, A>,
    ) -> Option<Arc<ExecutionGeneration<A>>> {
        core.transition(|core| {
            let Some(bundle) = core.generation.state.take_open_bundle() else {
                return (None, TransitionEffect::Keep);
            };
            let OpenGeneration {
                generation,
                services,
                module_epoch,
            } = bundle;
            core.generation.state.install_retirement(OpenRetirement {
                services,
                module_epoch,
            });
            (Some(generation), TransitionEffect::ClearPublication)
        })
    }

    #[cfg(test)]
    pub(crate) fn take_current_generation(&self) -> Option<Arc<ExecutionGeneration<A>>> {
        let mut core = self.access();
        self.take_current_bundle(&mut core)
    }

    #[cfg(any(test, feature = "bench-internals"))]
    pub(crate) fn install_test_generation_services(&self, services: Arc<GenerationServices>) {
        *self.test_services.lock() = Some(services);
    }

    pub(crate) fn take_generation_for_shutdown(&self) -> Option<ShutdownGeneration<A>> {
        let mut core = self.access();
        if let Some(generation) = self.take_current_bundle(&mut core) {
            return Some(ShutdownGeneration::Open(generation));
        }
        core.transition(|core| {
            (
                core.generation
                    .state
                    .take_opening()
                    .map(ShutdownGeneration::Opening),
                TransitionEffect::Keep,
            )
        })
    }
}

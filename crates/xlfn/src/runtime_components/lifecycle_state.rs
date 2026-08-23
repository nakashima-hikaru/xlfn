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

use super::services::GenerationServices;
use crate::generation::{OpenAttemptId, RuntimeGeneration};
use crate::lifecycle::{HostLifecycleIntent, LifecyclePhase};
use crate::module_runtime::ModuleEpochLease;
use crate::runtime::{OpenGeneration, OpeningGeneration};

/// A read-side publication of one coherent open generation.
///
/// The root and its generation services are published together. A reader can
/// therefore never observe a generation root from one open attempt with
/// services from another attempt.
pub(crate) struct GenerationPublication<A: crate::Addin> {
    pub(crate) root: Arc<OpenGeneration<A>>,
    pub(crate) services: Arc<GenerationServices>,
}

/// The complete ownership bundle for a published generation.
pub(crate) struct OpenBundle<A: crate::Addin> {
    generation: Arc<OpenGeneration<A>>,
    services: Arc<GenerationServices>,
    module_epoch: ModuleEpochLease,
}

/// Ownership retained after the generation root has been handed to the
/// shutdown/quiesce pipeline. The service root and module lease remain
/// coupled until the terminal certificate consumes them.
pub(crate) struct OpenRetirement {
    services: Arc<GenerationServices>,
    module_epoch: ModuleEpochLease,
}

/// The only payload that can accompany a lifecycle phase.
///
/// Keeping this payload below the phase enum prevents a generation root,
/// staged opening state, and retirement lease from being represented as three
/// unrelated fields. Each phase owns at most one of these payloads.
pub(crate) enum LifecyclePayload<A: crate::Addin> {
    Empty,
    Opening(OpeningGeneration<A>),
    Open(OpenBundle<A>),
    Retiring(OpenRetirement),
}

impl<A: crate::Addin> LifecyclePayload<A> {
    fn is_open(&self) -> bool {
        matches!(self, Self::Open(_))
    }

    fn is_retiring(&self) -> bool {
        matches!(self, Self::Retiring(_))
    }

    fn module_epoch_is_current(&self) -> bool {
        match self {
            Self::Retiring(retirement) => retirement.module_epoch.is_current(),
            Self::Empty | Self::Opening(_) | Self::Open(_) => true,
        }
    }

    fn take_opening(&mut self) -> Option<OpeningGeneration<A>> {
        let payload = mem::replace(self, Self::Empty);
        match payload {
            Self::Opening(opening) => Some(opening),
            other => {
                *self = other;
                None
            }
        }
    }

    fn take_open(&mut self) -> Option<OpenBundle<A>> {
        let payload = mem::replace(self, Self::Empty);
        match payload {
            Self::Open(bundle) => Some(bundle),
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

/// Canonical lifecycle state and its owned generation payload.
///
/// `LifecycleCoordinator::phase` is only a read-side projection of this
/// enum. The state machine below is deliberately non-`Copy`: moving between
/// phases also moves the staged generation, open bundle, or retirement lease.
pub(crate) enum LifecycleState<A: crate::Addin> {
    Closed,
    Opening {
        attempt: OpenAttemptId,
        payload: LifecyclePayload<A>,
    },
    Open {
        bundle: OpenBundle<A>,
    },
    Closing {
        open_attempt: Option<OpenAttemptId>,
        payload: LifecyclePayload<A>,
    },
    OpenRollbackPending {
        payload: LifecyclePayload<A>,
    },
    Quarantined {
        payload: LifecyclePayload<A>,
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
            Self::Opening { payload, .. }
            | Self::Closing { payload, .. }
            | Self::OpenRollbackPending { payload }
            | Self::Quarantined { payload } => match payload {
                LifecyclePayload::Opening(opening) => Some(opening),
                LifecyclePayload::Empty
                | LifecyclePayload::Open(_)
                | LifecyclePayload::Retiring(_) => None,
            },
            Self::Closed | Self::Open { .. } => None,
        }
    }

    fn has_opening_generation(&self) -> bool {
        self.opening().is_some()
    }

    fn has_current_generation(&self) -> bool {
        match self {
            Self::Open { .. } => true,
            Self::Opening { payload, .. }
            | Self::Closing { payload, .. }
            | Self::OpenRollbackPending { payload }
            | Self::Quarantined { payload } => payload.is_open(),
            Self::Closed => false,
        }
    }

    fn has_retirement(&self) -> bool {
        match self {
            Self::Opening { payload, .. }
            | Self::Closing { payload, .. }
            | Self::OpenRollbackPending { payload }
            | Self::Quarantined { payload } => payload.is_retiring(),
            Self::Closed | Self::Open { .. } => false,
        }
    }

    fn retiring_services(&self) -> Option<&Arc<GenerationServices>> {
        let payload = match self {
            Self::Opening { payload, .. }
            | Self::Closing { payload, .. }
            | Self::OpenRollbackPending { payload }
            | Self::Quarantined { payload } => payload,
            Self::Closed | Self::Open { .. } => return None,
        };
        match payload {
            LifecyclePayload::Retiring(retirement) => Some(&retirement.services),
            LifecyclePayload::Empty | LifecyclePayload::Opening(_) | LifecyclePayload::Open(_) => {
                None
            }
        }
    }

    fn module_epoch_is_current(&self) -> bool {
        match self {
            Self::Opening { payload, .. }
            | Self::Closing { payload, .. }
            | Self::OpenRollbackPending { payload }
            | Self::Quarantined { payload } => payload.module_epoch_is_current(),
            Self::Closed | Self::Open { .. } => true,
        }
    }

    fn take_opening(&mut self) -> Option<OpeningGeneration<A>> {
        match self {
            Self::Opening { payload, .. }
            | Self::Closing { payload, .. }
            | Self::OpenRollbackPending { payload }
            | Self::Quarantined { payload } => payload.take_opening(),
            Self::Closed | Self::Open { .. } => None,
        }
    }

    fn take_open_bundle(&mut self) -> Option<OpenBundle<A>> {
        let state = mem::replace(self, Self::Closed);
        match state {
            Self::Open { bundle } => Some(bundle),
            Self::Opening {
                attempt,
                mut payload,
            } => {
                let bundle = payload.take_open();
                *self = Self::Opening { attempt, payload };
                bundle
            }
            Self::Closing {
                open_attempt,
                mut payload,
            } => {
                let bundle = payload.take_open();
                *self = Self::Closing {
                    open_attempt,
                    payload,
                };
                bundle
            }
            Self::OpenRollbackPending { mut payload } => {
                let bundle = payload.take_open();
                *self = Self::OpenRollbackPending { payload };
                bundle
            }
            Self::Quarantined { mut payload } => {
                let bundle = payload.take_open();
                *self = Self::Quarantined { payload };
                bundle
            }
            Self::Closed => None,
        }
    }

    fn install_retirement(&mut self, retirement: OpenRetirement) {
        let state = mem::replace(self, Self::Closed);
        *self = match state {
            Self::Closed | Self::Open { .. } => Self::Closing {
                open_attempt: None,
                payload: LifecyclePayload::Retiring(retirement),
            },
            Self::Opening { attempt, payload } => {
                require_lifecycle_invariant(
                    matches!(payload, LifecyclePayload::Empty),
                    "retirement installed while opening payload is present",
                );
                Self::Closing {
                    open_attempt: Some(attempt),
                    payload: LifecyclePayload::Retiring(retirement),
                }
            }
            Self::Closing {
                open_attempt,
                payload,
            } => {
                require_lifecycle_invariant(
                    matches!(payload, LifecyclePayload::Empty),
                    "retirement installed while closing payload is present",
                );
                Self::Closing {
                    open_attempt,
                    payload: LifecyclePayload::Retiring(retirement),
                }
            }
            Self::OpenRollbackPending { payload } => {
                require_lifecycle_invariant(
                    matches!(payload, LifecyclePayload::Empty),
                    "retirement installed while rollback payload is present",
                );
                Self::OpenRollbackPending {
                    payload: LifecyclePayload::Retiring(retirement),
                }
            }
            Self::Quarantined { payload } => {
                require_lifecycle_invariant(
                    matches!(payload, LifecyclePayload::Empty),
                    "retirement installed while quarantine payload is present",
                );
                Self::Quarantined {
                    payload: LifecyclePayload::Retiring(retirement),
                }
            }
        };
    }

    fn take_retirement(&mut self) -> Option<OpenRetirement> {
        match self {
            Self::Opening { payload, .. }
            | Self::Closing { payload, .. }
            | Self::OpenRollbackPending { payload }
            | Self::Quarantined { payload } => payload.take_retirement(),
            Self::Closed | Self::Open { .. } => None,
        }
    }

    fn into_payload(self) -> LifecyclePayload<A> {
        match self {
            Self::Closed => LifecyclePayload::Empty,
            Self::Opening { payload, .. }
            | Self::Closing { payload, .. }
            | Self::OpenRollbackPending { payload }
            | Self::Quarantined { payload } => payload,
            Self::Open { bundle } => LifecyclePayload::Open(bundle),
        }
    }
}

/// Canonical owner of every mutable lifecycle decision and generation root.
pub(crate) struct LifecycleCore<A: crate::Addin> {
    state: LifecycleState<A>,
    host_intent: HostLifecycleIntent,
    next_lifecycle_attempt: u64,
    last_committed_generation: Option<RuntimeGeneration>,
    removal_epoch: u64,
    removal_attempt_active: bool,
}

impl<A: crate::Addin> LifecycleCore<A> {
    const fn new() -> Self {
        Self {
            state: LifecycleState::Closed,
            host_intent: HostLifecycleIntent::None,
            next_lifecycle_attempt: 1,
            last_committed_generation: None,
            removal_epoch: 0,
            removal_attempt_active: false,
        }
    }

    /// Returns the mutex-protected canonical state. It is intentionally a
    /// reference because the state owns the phase payload.
    pub(crate) const fn canonical_state(&self) -> &LifecycleState<A> {
        &self.state
    }

    pub(crate) const fn host_intent(&self) -> HostLifecycleIntent {
        self.host_intent
    }

    pub(crate) const fn last_committed_generation(&self) -> Option<RuntimeGeneration> {
        self.last_committed_generation
    }

    pub(crate) fn protocol_generation(&self) -> Option<RuntimeGeneration> {
        self.state
            .protocol_generation(self.last_committed_generation)
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
        self.state.opening().map(|opening| opening.init_config)
    }

    pub(crate) fn has_opening_generation(&self) -> bool {
        self.state.has_opening_generation()
    }

    pub(crate) fn has_current_generation(&self) -> bool {
        self.state.has_current_generation()
    }

    pub(crate) fn has_module_epoch(&self) -> bool {
        self.state.has_retirement()
    }

    pub(crate) fn has_retirement(&self) -> bool {
        self.state.has_retirement()
    }

    pub(crate) fn module_epoch_is_current(&self) -> bool {
        self.state.module_epoch_is_current()
    }

    pub(crate) fn retiring_services(&self) -> Option<&Arc<GenerationServices>> {
        self.state.retiring_services()
    }
}

/// Lifecycle synchronization state.
///
/// `core` is the canonical ownership boundary. `phase` and `publication` are
/// read-side projections used by hot-path admission and generation/service
/// access; lifecycle writers mutate `core` first and then update projections.
pub(crate) struct LifecycleCoordinator<A: crate::Addin> {
    phase: AtomicU8,
    publication: ArcSwapOption<GenerationPublication<A>>,
    core: Mutex<LifecycleCore<A>>,
    changed: Condvar,
    #[cfg(any(test, feature = "unstable"))]
    test_services: Mutex<Option<Arc<GenerationServices>>>,
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
            publication: ArcSwapOption::const_empty(),
            core: Mutex::new(LifecycleCore::new()),
            changed: Condvar::new(),
            #[cfg(any(test, feature = "unstable"))]
            test_services: Mutex::new(None),
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
    pub(crate) fn observed_phase(&self) -> LifecyclePhase {
        LifecyclePhase::from_raw(self.phase.load(Ordering::Acquire))
    }

    pub(crate) fn set_host_intent(&self, intent: HostLifecycleIntent) {
        let mut core = self.lock();
        core.host_intent = intent;
        self.refresh_projection(&core);
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
            .store(core.canonical_state().phase() as u8, Ordering::Release);
    }

    fn clear_publication(&self) {
        self.publication.store(None);
    }

    fn publish_publication(&self, bundle: &OpenBundle<A>) {
        self.publication.store(Some(Arc::new(GenerationPublication {
            root: Arc::clone(&bundle.generation),
            services: Arc::clone(&bundle.services),
        })));
    }

    /// Clears host intent before the external module-open protocol is started.
    pub(crate) fn prepare_open(&self, core: &mut LifecycleCore<A>) {
        require_lifecycle_invariant(
            core.canonical_state().phase() == LifecyclePhase::Closed,
            "open preparation requires the closed lifecycle phase",
        );
        core.host_intent = HostLifecycleIntent::None;
        self.refresh_projection(core);
    }

    pub(crate) fn allocate_open_attempt(
        &self,
        core: &mut LifecycleCore<A>,
    ) -> crate::XllResult<OpenAttemptId> {
        self.next_lifecycle_attempt_id(core)
    }

    pub(crate) fn begin_opening(&self, core: &mut LifecycleCore<A>, attempt: OpenAttemptId) {
        require_lifecycle_invariant(
            core.canonical_state().phase() == LifecyclePhase::Closed,
            "opening requires the closed lifecycle phase",
        );
        require_lifecycle_invariant(
            !core.removal_attempt_active(),
            "opening cannot begin while removal owns the lifecycle",
        );
        core.state = LifecycleState::Opening {
            attempt,
            payload: LifecyclePayload::Empty,
        };
        self.refresh_projection(core);
    }

    /// Publishes a successfully assembled generation while retaining the
    /// opening attempt until `commit_open` completes the lifecycle transition.
    pub(crate) fn commit_open(
        &self,
        core: &mut LifecycleCore<A>,
        generation: RuntimeGeneration,
    ) -> crate::XllResult<()> {
        let state = mem::replace(&mut core.state, LifecycleState::Closed);
        match state {
            LifecycleState::Opening {
                attempt,
                payload: LifecyclePayload::Open(bundle),
            } => {
                if attempt.into_runtime_generation() != generation
                    || bundle.generation.id() != generation
                {
                    core.state = LifecycleState::Opening {
                        attempt,
                        payload: LifecyclePayload::Open(bundle),
                    };
                    return Err(crate::XllError::Internal {
                        diagnostic_id: crate::error::DiagnosticId::OPEN_STATE,
                    });
                }
                core.last_committed_generation = Some(generation);
                core.state = LifecycleState::Open { bundle };
                self.refresh_projection(core);
                Ok(())
            }
            other => {
                core.state = other;
                Err(crate::XllError::Internal {
                    diagnostic_id: crate::error::DiagnosticId::OPEN_STATE,
                })
            }
        }
    }

    pub(crate) fn reject_open_attempt(&self, core: &mut LifecycleCore<A>) {
        let state = mem::replace(&mut core.state, LifecycleState::Closed);
        core.state = match state {
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
        self.refresh_projection(core);
    }

    /// Records an open failure without discarding the owned staged/published
    /// payload. The rollback pipeline can then take that payload explicitly.
    pub(crate) fn record_open_failure(&self, core: &mut LifecycleCore<A>) -> bool {
        let state = mem::replace(&mut core.state, LifecycleState::Closed);
        let (state, should_rollback) = match state {
            LifecycleState::Opening { payload, .. } => {
                (LifecycleState::OpenRollbackPending { payload }, true)
            }
            LifecycleState::OpenRollbackPending { payload } => {
                (LifecycleState::OpenRollbackPending { payload }, true)
            }
            LifecycleState::Closing { payload, .. } => (
                LifecycleState::Closing {
                    payload,
                    open_attempt: None,
                },
                false,
            ),
            other => (other, false),
        };
        core.state = state;
        self.refresh_projection(core);
        should_rollback
    }

    /// Requests closing while moving the active generation payload under the
    /// closing phase. No payload remains in a separate core field.
    pub(crate) fn request_closing(&self, core: &mut LifecycleCore<A>) {
        if core.canonical_state().phase() == LifecyclePhase::Closed && core.removal_attempt_active()
        {
            return;
        }
        let state = mem::replace(&mut core.state, LifecycleState::Closed);
        core.state = match state {
            LifecycleState::Closed => LifecycleState::Closing {
                open_attempt: None,
                payload: LifecyclePayload::Empty,
            },
            LifecycleState::Opening { attempt, payload } => LifecycleState::Closing {
                open_attempt: Some(attempt),
                payload,
            },
            LifecycleState::Open { bundle } => LifecycleState::Closing {
                open_attempt: None,
                payload: LifecyclePayload::Open(bundle),
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
        self.refresh_projection(core);
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
        require_lifecycle_invariant(
            matches!(
                core.canonical_state(),
                LifecycleState::Closed
                    | LifecycleState::Closing {
                        payload: LifecyclePayload::Empty,
                        ..
                    }
                    | LifecycleState::OpenRollbackPending {
                        payload: LifecyclePayload::Empty
                    }
            ),
            "closed publication requires an empty lifecycle payload",
        );
        core.state = LifecycleState::Closed;
        self.refresh_projection(core);
    }

    pub(crate) fn quarantine_core(&self, core: &mut LifecycleCore<A>) {
        let state = mem::replace(&mut core.state, LifecycleState::Closed);
        core.state = LifecycleState::Quarantined {
            payload: state.into_payload(),
        };
        self.refresh_projection(core);
    }

    pub(crate) fn stage_opening_generation_locked(
        &self,
        core: &mut LifecycleCore<A>,
        opening: OpeningGeneration<A>,
    ) -> Result<(), (crate::XllError, OpeningGeneration<A>)> {
        let state = mem::replace(&mut core.state, LifecycleState::Closed);
        match state {
            LifecycleState::Opening {
                attempt,
                payload: LifecyclePayload::Empty,
            } => {
                core.state = LifecycleState::Opening {
                    attempt,
                    payload: LifecyclePayload::Opening(opening),
                };
                Ok(())
            }
            other => {
                core.state = other;
                Err((
                    crate::XllError::Internal {
                        diagnostic_id: crate::error::DiagnosticId::OPEN_STATE,
                    },
                    opening,
                ))
            }
        }
    }

    pub(crate) fn publish_opening_generation_locked(
        &self,
        core: &mut LifecycleCore<A>,
        generation: RuntimeGeneration,
        services: Arc<GenerationServices>,
        module_epoch: ModuleEpochLease,
    ) -> Result<(), PublishOpeningError<A>> {
        if core.has_current_generation() {
            return Err(PublishOpeningError {
                error: crate::XllError::Internal {
                    diagnostic_id: crate::error::DiagnosticId::OPEN_STATE,
                },
                opening: core.state.take_opening(),
            });
        }
        let attempt = match core.canonical_state() {
            LifecycleState::Opening { attempt, .. } => *attempt,
            _ => {
                return Err(PublishOpeningError {
                    error: crate::XllError::Internal {
                        diagnostic_id: crate::error::DiagnosticId::OPEN_STATE,
                    },
                    opening: core.state.take_opening(),
                });
            }
        };
        let opening = core.state.take_opening().ok_or(PublishOpeningError {
            error: crate::XllError::Internal {
                diagnostic_id: crate::error::DiagnosticId::OPEN_STATE,
            },
            opening: None,
        })?;
        let OpeningGeneration {
            shared_state,
            layers,
            init_config: _,
        } = opening;
        let published = Arc::new(OpenGeneration {
            id: generation,
            shared_state,
            layers,
        });
        let bundle = OpenBundle {
            generation: Arc::clone(&published),
            services,
            module_epoch,
        };
        core.state = LifecycleState::Opening {
            attempt,
            payload: LifecyclePayload::Open(bundle),
        };
        if let LifecycleState::Opening {
            payload: LifecyclePayload::Open(bundle),
            ..
        } = core.canonical_state()
        {
            self.publish_publication(bundle);
        } else {
            unreachable!("published opening bundle was just installed");
        }
        Ok(())
    }

    /// Consumes the coupled shutdown ownership only after a terminal
    /// certificate has validated it.
    pub(crate) fn take_certified_module_epoch(
        &self,
        core: &mut LifecycleCore<A>,
    ) -> Option<ModuleEpochLease> {
        let retirement = core.state.take_retirement();
        if retirement.is_some() {
            self.clear_publication();
        }
        retirement.map(|retirement| {
            let OpenRetirement {
                services,
                module_epoch,
            } = retirement;
            drop(services);
            module_epoch
        })
    }

    #[cfg(test)]
    pub(crate) fn has_opening_generation(&self) -> bool {
        self.lock().has_opening_generation()
    }

    #[cfg(test)]
    pub(crate) fn has_current_generation(&self) -> bool {
        self.lock().has_current_generation()
    }

    pub(crate) fn load_generation_publication(
        &self,
    ) -> arc_swap::Guard<Option<Arc<GenerationPublication<A>>>> {
        self.publication.load()
    }

    /// Service access is a cold-path operation. It borrows the coherent
    /// publication long enough to clone the service root; no independent
    /// production projection exists.
    pub(crate) fn load_generation_services(&self) -> Option<Arc<GenerationServices>> {
        let publication = self.publication.load();
        if let Some(publication) = publication.as_ref() {
            return Some(Arc::clone(&publication.services));
        }
        #[cfg(any(test, feature = "unstable"))]
        {
            return self.test_services.lock().clone();
        }
        #[cfg(not(any(test, feature = "unstable")))]
        None
    }

    pub(crate) fn take_opening_for_rollback(&self) -> Option<OpeningGeneration<A>> {
        self.lock().state.take_opening()
    }

    fn take_current_bundle(&self, core: &mut LifecycleCore<A>) -> Option<Arc<OpenGeneration<A>>> {
        let bundle = core.state.take_open_bundle()?;
        let OpenBundle {
            generation,
            services,
            module_epoch,
        } = bundle;
        core.state.install_retirement(OpenRetirement {
            services,
            module_epoch,
        });
        self.clear_publication();
        Some(generation)
    }

    #[cfg(test)]
    pub(crate) fn take_current_generation(&self) -> Option<Arc<OpenGeneration<A>>> {
        let mut core = self.lock();
        self.take_current_bundle(&mut core)
    }

    #[cfg(any(test, feature = "unstable"))]
    pub(crate) fn install_test_generation_services(&self, services: Arc<GenerationServices>) {
        *self.test_services.lock() = Some(services);
    }

    pub(crate) fn take_generation_for_shutdown(
        &self,
    ) -> Option<crate::runtime::ShutdownGeneration<A>> {
        let mut core = self.lock();
        if let Some(generation) = self.take_current_bundle(&mut core) {
            return Some(crate::runtime::ShutdownGeneration::Open(generation));
        }
        core.state
            .take_opening()
            .map(crate::runtime::ShutdownGeneration::Opening)
    }
}

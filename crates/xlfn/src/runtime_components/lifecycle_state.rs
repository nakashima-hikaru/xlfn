//! Canonical lifecycle state and its read-side phase projection.

use arc_swap::ArcSwapOption;
use parking_lot::{Condvar, Mutex, MutexGuard};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use crate::generation::{OpenAttemptId, RuntimeGeneration};
use crate::lifecycle::{HostLifecycleIntent, LifecyclePhase};
use crate::runtime::{OpenGeneration, OpeningGeneration};

/// Canonical lifecycle state owned by the lifecycle control mutex.
///
/// The phase atomic in [`LifecycleState`] is deliberately only a read-side
/// projection. Every writer first updates this state and then publishes the
/// phase through [`LifecycleState::refresh_projection`]. Correlated lifecycle
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

/// All mutable lifecycle decisions are made while this value is locked.
///
/// `known_generation` intentionally survives the transition to `Closed`: it
/// identifies the last generation whose teardown was certified and is used by
/// shutdown certificates and diagnostics. The currently active generation is
/// still exposed separately through the `Open` state and the ArcSwap root.
pub(crate) struct LifecycleControl {
    state: LifecycleStateKind,
    pub(crate) host_intent: HostLifecycleIntent,
    pub(crate) next_lifecycle_attempt: u64,
    pub(crate) known_generation: Option<RuntimeGeneration>,
    pub(crate) removal_epoch: u64,
    pub(crate) removal_attempt_active: bool,
}

impl LifecycleControl {
    const fn new() -> Self {
        Self {
            state: LifecycleStateKind::Closed,
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
}

/// Lifecycle synchronization state: ownership remains in the opening slot
/// and ArcSwap root, while all lifecycle writes are serialized by one control
/// mutex. Only phase is projected for lock-free read-side admission.
pub(crate) struct LifecycleState<A: crate::Addin> {
    phase: AtomicU8,
    pub(crate) opening: Mutex<Option<OpeningGeneration<A>>>,
    pub(crate) current: ArcSwapOption<OpenGeneration<A>>,
    control: Mutex<LifecycleControl>,
    changed: Condvar,
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
            opening: Mutex::new(None),
            current: ArcSwapOption::const_empty(),
            control: Mutex::new(LifecycleControl::new()),
            changed: Condvar::new(),
            #[cfg(test)]
            test_module_lease: Mutex::new(None),
        }
    }

    pub(crate) fn lock(&self) -> MutexGuard<'_, LifecycleControl> {
        self.control.lock()
    }

    pub(crate) fn wait<'a>(&self, control: &mut MutexGuard<'a, LifecycleControl>) {
        self.changed.wait(control);
    }

    pub(crate) fn notify_all(&self) {
        self.changed.notify_all();
    }

    /// Returns the read-side phase projection.
    ///
    /// Lifecycle writers must inspect [`LifecycleControl::state`] instead;
    /// this method is intentionally named to make that distinction visible.
    pub(crate) fn observed_phase(&self) -> LifecyclePhase {
        LifecyclePhase::from_raw(self.phase.load(Ordering::Acquire))
    }

    pub(crate) fn set_host_intent(&self, intent: HostLifecycleIntent) {
        let mut control = self.lock();
        self.set_host_intent_locked(&mut control, intent);
    }

    pub(crate) fn set_host_intent_locked(
        &self,
        control: &mut LifecycleControl,
        intent: HostLifecycleIntent,
    ) {
        control.host_intent = intent;
        self.refresh_projection(control);
    }

    pub(crate) fn set_state(&self, control: &mut LifecycleControl, state: LifecycleStateKind) {
        control.state = state;
        self.refresh_projection(control);
    }

    pub(crate) fn set_known_generation(
        &self,
        control: &mut LifecycleControl,
        generation: Option<RuntimeGeneration>,
    ) {
        control.known_generation = generation;
        self.refresh_projection(control);
    }

    pub(crate) fn set_removal_attempt_active(&self, control: &mut LifecycleControl, active: bool) {
        control.removal_attempt_active = active;
        self.refresh_projection(control);
    }

    pub(crate) fn advance_removal_epoch(&self, control: &mut LifecycleControl) {
        control.removal_epoch = control.removal_epoch.checked_add(1).unwrap_or_else(|| {
            tracing::error!("lifecycle close epoch exhausted; fail-stopping");
            std::process::abort();
        });
        self.refresh_projection(control);
    }

    pub(crate) fn next_lifecycle_attempt_id(
        &self,
        control: &mut LifecycleControl,
    ) -> crate::XllResult<OpenAttemptId> {
        let attempt_id = control.next_lifecycle_attempt;
        let next = attempt_id.checked_add(1).ok_or(crate::XllError::Internal {
            diagnostic_id: crate::error::DiagnosticId::ATTEMPT_OVERFLOW,
        })?;
        let attempt = OpenAttemptId::new(attempt_id).ok_or(crate::XllError::Internal {
            diagnostic_id: crate::error::DiagnosticId::ATTEMPT_ZERO,
        })?;
        control.next_lifecycle_attempt = next;
        Ok(attempt)
    }

    fn refresh_projection(&self, control: &LifecycleControl) {
        self.phase
            .store(control.state.phase() as u8, Ordering::Release);
    }

    pub(crate) fn stage_opening_state(
        &self,
        state: A::SharedState,
        config: crate::addin::RuntimeConfig,
    ) -> Result<(), (crate::XllError, A::SharedState)> {
        let mut slot = self.opening.lock();
        if slot.is_some() || self.current.load().is_some() {
            return Err((
                crate::XllError::Internal {
                    diagnostic_id: crate::error::DiagnosticId::OPEN_STATE,
                },
                state,
            ));
        }
        *slot = Some(OpeningGeneration::SharedStateOnly {
            shared_state: state,
            config,
        });
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
                    diagnostic_id: crate::error::DiagnosticId::OPEN_STATE,
                },
                opening,
            ));
        }
        *slot = Some(opening);
        Ok(())
    }

    pub(crate) fn publish_opening_generation(
        &self,
        generation: RuntimeGeneration,
    ) -> Result<(), PublishOpeningError<A>> {
        let opening = self.opening.lock().take().ok_or(PublishOpeningError {
            error: crate::XllError::Internal {
                diagnostic_id: crate::error::DiagnosticId::OPEN_STATE,
            },
            opening: None,
        })?;
        let (shared_state, layers, _config) = match opening {
            OpeningGeneration::Ready {
                shared_state,
                layers,
                config,
            } => (shared_state, layers, config),
            opening @ OpeningGeneration::SharedStateOnly { .. } => {
                return Err(PublishOpeningError {
                    error: crate::XllError::Internal {
                        diagnostic_id: crate::error::DiagnosticId::OPEN_STATE,
                    },
                    opening: Some(opening),
                });
            }
        };
        self.current.store(Some(Arc::new(OpenGeneration {
            id: generation,
            shared_state,
            layers,
        })));
        Ok(())
    }

    pub(crate) fn opening_config(&self) -> Option<crate::addin::RuntimeConfig> {
        self.opening.lock().as_ref().map(|opening| match opening {
            OpeningGeneration::SharedStateOnly { config, .. }
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

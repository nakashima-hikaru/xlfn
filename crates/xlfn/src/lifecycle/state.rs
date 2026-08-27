//! Canonical lifecycle ownership and its read-side projections.

use arc_swap::ArcSwapOption;
use parking_lot::{Condvar, Mutex, MutexGuard};
use std::mem;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

#[cold]
fn lifecycle_invariant_violation(message: &'static str) -> ! {
    super::lifecycle_invariant_violation(message)
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
use crate::module_runtime::{
    ModuleAuthority, ModuleCleanupAuthority, ModuleEpochId, ModuleEpochLease,
};
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

    #[cfg(any(feature = "handles", feature = "rtd"))]
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
    module_epoch: ModuleEpochLease,
}

/// The generation resources retained after its module lease moves into the
/// closing ownership slot.
pub(crate) struct ClosingGeneration<A: crate::Addin> {
    generation: Arc<ExecutionGeneration<A>>,
    services: Arc<GenerationServices>,
}

/// Ownership retained after the generation root has been handed to the
/// shutdown/quiesce pipeline. The generation identity remains part of the
/// payload while the module authority remains in the lifecycle state.
pub(crate) struct OpenRetirement {
    generation: RuntimeGeneration,
    services: Arc<GenerationServices>,
}

impl<A: crate::Addin> OpenGeneration<A> {
    fn into_closing(self) -> (ClosingGeneration<A>, ModuleAuthority) {
        let Self {
            generation,
            services,
            module_epoch,
        } = self;
        (
            ClosingGeneration {
                generation,
                services,
            },
            ModuleAuthority::Open(module_epoch),
        )
    }
}

impl<A: crate::Addin> ClosingGeneration<A> {
    fn into_retirement(self) -> OpenRetirement {
        OpenRetirement {
            generation: self.generation.id(),
            services: self.services,
        }
    }

    fn into_retirement_with_generation(self) -> (Arc<ExecutionGeneration<A>>, OpenRetirement) {
        let generation = Arc::clone(&self.generation);
        let retirement = self.into_retirement();
        (generation, retirement)
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
    fn into_close_resources(self, attempt: OpenAttemptId) -> CloseResources<A> {
        match self {
            Self::Empty => CloseResources::AwaitingOpenAbort {
                attempt,
                payload: OpenAbortPayload::Empty,
            },
            Self::Staged(opening) => CloseResources::AwaitingOpenAbort {
                attempt,
                payload: OpenAbortPayload::Staged(opening),
            },
            Self::Published(bundle) => {
                let (closing, authority) = bundle.into_closing();
                CloseResources::Available {
                    payload: ClosePayload::Published(closing),
                    authority,
                }
            }
        }
    }

    fn into_closing_state(self, attempt: OpenAttemptId) -> ClosingState<A> {
        match self {
            Self::Empty => ClosingState::OpeningActive {
                attempt,
                resources: ActiveOpeningClose::AwaitingAbort {
                    payload: OpenAbortPayload::Empty,
                },
            },
            Self::Staged(opening) => ClosingState::OpeningActive {
                attempt,
                resources: ActiveOpeningClose::AwaitingAbort {
                    payload: OpenAbortPayload::Staged(opening),
                },
            },
            Self::Published(bundle) => {
                let (closing, authority) = bundle.into_closing();
                ClosingState::OpeningActive {
                    attempt,
                    resources: ActiveOpeningClose::Published {
                        payload: PublishedClosePayload::Published(closing),
                        authority,
                    },
                }
            }
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

    fn module_epoch_id(&self) -> Option<ModuleEpochId> {
        match self {
            Self::Published(bundle) => Some(bundle.module_epoch.id()),
            Self::Empty | Self::Staged(_) => None,
        }
    }
}

/// Payload states that can survive into a closing, rollback, or quarantine
/// phase. The lifecycle state owns this payload together with the only
/// authority state that is valid for it.
pub(crate) enum ClosePayload<A: crate::Addin> {
    Empty,
    Staged(OpeningGeneration<A>),
    Published(ClosingGeneration<A>),
    Retiring(OpenRetirement),
}

impl<A: crate::Addin> ClosePayload<A> {
    fn is_published(&self) -> bool {
        matches!(self, Self::Published(_))
    }

    #[cfg(test)]
    fn has_opening_generation(&self) -> bool {
        matches!(self, Self::Staged(_))
    }

    fn opening(&self) -> Option<&OpeningGeneration<A>> {
        match self {
            Self::Staged(opening) => Some(opening),
            Self::Empty | Self::Published(_) | Self::Retiring(_) => None,
        }
    }

    fn retiring_services(&self) -> Option<&Arc<GenerationServices>> {
        match self {
            Self::Retiring(retirement) => Some(&retirement.services),
            Self::Published(bundle) => Some(&bundle.services),
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

/// The lifecycle state that is visible while a close owner is being
/// finalized. The removal attempt stays in the canonical state until the
/// affine owner is dropped, so a concurrent open cannot observe a transiently
/// ownerless `Closed` phase.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ClosedState {
    Idle,
    Finalizing { attempt: RemovalAttemptId },
}

/// The only payloads that can remain while an open attempt still owns the
/// module close path.
pub(crate) enum OpenAbortPayload<A: crate::Addin> {
    Empty,
    Staged(OpeningGeneration<A>),
}

impl<A: crate::Addin> OpenAbortPayload<A> {
    fn into_close_payload(self) -> ClosePayload<A> {
        match self {
            Self::Empty => ClosePayload::Empty,
            Self::Staged(opening) => ClosePayload::Staged(opening),
        }
    }

    #[cfg(test)]
    fn has_opening_generation(&self) -> bool {
        matches!(self, Self::Staged(_))
    }

    fn opening(&self) -> Option<&OpeningGeneration<A>> {
        match self {
            Self::Staged(opening) => Some(opening),
            Self::Empty => None,
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
}

/// Published or retiring generation payloads that can coexist with an active
/// opening attempt. A staged generation is deliberately not representable in
/// this type.
pub(crate) enum PublishedClosePayload<A: crate::Addin> {
    Published(ClosingGeneration<A>),
    Retiring(OpenRetirement),
}

impl<A: crate::Addin> PublishedClosePayload<A> {
    fn into_close_payload(self) -> ClosePayload<A> {
        match self {
            Self::Published(closing) => ClosePayload::Published(closing),
            Self::Retiring(retirement) => ClosePayload::Retiring(retirement),
        }
    }

    fn retiring_services(&self) -> &Arc<GenerationServices> {
        match self {
            Self::Published(closing) => &closing.services,
            Self::Retiring(retirement) => &retirement.services,
        }
    }
}

/// Module authority retained while a removal owner is active. The returned
/// authority is optional because teardown may move it back to the lifecycle
/// after partially progressing the close pipeline.
pub(crate) struct RemovalClaimState {
    attempt: RemovalAttemptId,
    module_epoch: ModuleEpochId,
    returned: Option<ModuleAuthority>,
}

/// Closing resources and their authority are one state space. Each variant
/// contains only combinations that can actually arise from a preceding
/// lifecycle transition.
pub(crate) enum CloseResources<A: crate::Addin> {
    AwaitingOpenAbort {
        attempt: OpenAttemptId,
        payload: OpenAbortPayload<A>,
    },
    Unowned {
        payload: ClosePayload<A>,
    },
    Available {
        payload: ClosePayload<A>,
        authority: ModuleAuthority,
    },
    Claimed {
        payload: ClosePayload<A>,
        claim: RemovalClaimState,
    },
}

impl<A: crate::Addin> CloseResources<A> {
    fn is_published(&self) -> bool {
        match self {
            Self::AwaitingOpenAbort { .. } => false,
            Self::Unowned { payload }
            | Self::Available { payload, .. }
            | Self::Claimed { payload, .. } => payload.is_published(),
        }
    }

    #[cfg(test)]
    fn has_opening_generation(&self) -> bool {
        match self {
            Self::AwaitingOpenAbort { payload, .. } => payload.has_opening_generation(),
            Self::Unowned { payload }
            | Self::Available { payload, .. }
            | Self::Claimed { payload, .. } => payload.has_opening_generation(),
        }
    }

    fn opening(&self) -> Option<&OpeningGeneration<A>> {
        match self {
            Self::AwaitingOpenAbort { payload, .. } => payload.opening(),
            Self::Unowned { payload }
            | Self::Available { payload, .. }
            | Self::Claimed { payload, .. } => payload.opening(),
        }
    }

    fn retiring_services(&self) -> Option<&Arc<GenerationServices>> {
        match self {
            Self::AwaitingOpenAbort { .. } => None,
            Self::Unowned { payload }
            | Self::Available { payload, .. }
            | Self::Claimed { payload, .. } => payload.retiring_services(),
        }
    }

    fn module_epoch_id(&self) -> Option<ModuleEpochId> {
        match self {
            Self::Available { authority, .. } => Some(authority.id()),
            Self::Claimed { claim, .. } => Some(claim.module_epoch),
            Self::AwaitingOpenAbort { .. } | Self::Unowned { .. } => None,
        }
    }

    fn removal_attempt(&self) -> Option<RemovalAttemptId> {
        match self {
            Self::Claimed { claim, .. } => Some(claim.attempt),
            Self::AwaitingOpenAbort { .. } | Self::Unowned { .. } | Self::Available { .. } => None,
        }
    }

    fn install_authority(self, authority: ModuleAuthority) -> Self {
        match self {
            Self::AwaitingOpenAbort { payload, .. } => Self::Available {
                payload: payload.into_close_payload(),
                authority,
            },
            Self::Unowned { payload } => Self::Available { payload, authority },
            Self::Claimed {
                payload,
                claim:
                    RemovalClaimState {
                        attempt,
                        module_epoch,
                        returned: None,
                    },
            } => {
                require_lifecycle_invariant(
                    authority.id() == module_epoch,
                    "returned module authority belongs to a different epoch",
                );
                Self::Claimed {
                    payload,
                    claim: RemovalClaimState {
                        attempt,
                        module_epoch,
                        returned: Some(authority),
                    },
                }
            }
            Self::Available { .. }
            | Self::Claimed {
                claim:
                    RemovalClaimState {
                        returned: Some(_), ..
                    },
                ..
            } => lifecycle_invariant_violation("module close authority was installed twice"),
        }
    }

    fn claim(self, attempt: RemovalAttemptId) -> (Self, crate::module_runtime::ModuleClosing) {
        let Self::Available { payload, authority } = self else {
            lifecycle_invariant_violation("removal claim requires available module authority")
        };
        let module_epoch = authority.id();
        let closing = authority.into_closing();
        (
            Self::Claimed {
                payload,
                claim: RemovalClaimState {
                    attempt,
                    module_epoch,
                    returned: None,
                },
            },
            closing,
        )
    }

    fn take_cleanup(self) -> (Option<ModuleCleanupAuthority>, Self) {
        match self {
            Self::Available { payload, authority } => {
                (Some(authority.into_cleanup()), Self::Unowned { payload })
            }
            Self::Claimed {
                payload,
                claim:
                    RemovalClaimState {
                        attempt,
                        module_epoch,
                        returned: Some(authority),
                    },
            } => (
                Some(authority.into_cleanup()),
                Self::Claimed {
                    payload,
                    claim: RemovalClaimState {
                        attempt,
                        module_epoch,
                        returned: None,
                    },
                },
            ),
            other => (None, other),
        }
    }

    fn release(
        self,
        expected_attempt: RemovalAttemptId,
        returned: Option<ModuleAuthority>,
    ) -> Self {
        let Self::Claimed {
            payload,
            claim:
                RemovalClaimState {
                    attempt,
                    module_epoch,
                    returned: retained,
                },
        } = self
        else {
            lifecycle_invariant_violation(
                "removal owner release does not match canonical lifecycle ownership",
            )
        };
        validate_removal_owner(attempt, expected_attempt);
        match combine_returned_authority(retained, returned) {
            Some(authority) => {
                require_lifecycle_invariant(
                    authority.id() == module_epoch,
                    "released module authority belongs to a different epoch",
                );
                Self::Available { payload, authority }
            }
            None => Self::Unowned { payload },
        }
    }

    fn final_removal_ready(&self, expected_attempt: RemovalAttemptId) -> Option<FinalRemovalReady> {
        match self {
            Self::Claimed {
                payload: ClosePayload::Retiring(retirement),
                claim:
                    RemovalClaimState {
                        attempt,
                        module_epoch,
                        returned: None,
                    },
            } if *attempt == expected_attempt
                && retirement.services.is_none()
                && module_epoch.is_current() =>
            {
                Some(FinalRemovalReady::Committed {
                    generation: retirement.generation,
                    module_epoch: *module_epoch,
                })
            }
            Self::Claimed {
                payload: ClosePayload::Empty,
                claim:
                    RemovalClaimState {
                        attempt,
                        returned: None,
                        ..
                    },
            } if *attempt == expected_attempt => Some(FinalRemovalReady::Uncommitted),
            _ => None,
        }
    }

    fn open_rollback_ready(&self, expected_attempt: RemovalAttemptId) -> Option<OpenRollbackReady> {
        match self {
            Self::Claimed {
                payload: ClosePayload::Empty,
                claim:
                    RemovalClaimState {
                        attempt,
                        returned: None,
                        ..
                    },
            } if *attempt == expected_attempt => Some(OpenRollbackReady { _private: () }),
            _ => None,
        }
    }

    fn take_staged(&mut self) -> Option<OpeningGeneration<A>> {
        let resources = mem::replace(
            self,
            Self::Unowned {
                payload: ClosePayload::Empty,
            },
        );
        match resources {
            Self::AwaitingOpenAbort {
                attempt,
                mut payload,
            } => {
                let opening = payload.take_staged();
                *self = Self::AwaitingOpenAbort { attempt, payload };
                opening
            }
            Self::Unowned { mut payload } => {
                let opening = payload.take_staged();
                *self = Self::Unowned { payload };
                opening
            }
            Self::Available {
                mut payload,
                authority,
            } => {
                let opening = payload.take_staged();
                *self = Self::Available { payload, authority };
                opening
            }
            Self::Claimed { mut payload, claim } => {
                let opening = payload.take_staged();
                *self = Self::Claimed { payload, claim };
                opening
            }
        }
    }

    fn take_retirement(&mut self) -> Option<OpenRetirement> {
        let resources = mem::replace(
            self,
            Self::Unowned {
                payload: ClosePayload::Empty,
            },
        );
        match resources {
            Self::AwaitingOpenAbort { attempt, payload } => {
                *self = Self::AwaitingOpenAbort { attempt, payload };
                None
            }
            Self::Unowned { mut payload } => {
                let retirement = payload.take_retirement();
                *self = Self::Unowned { payload };
                retirement
            }
            Self::Available {
                mut payload,
                authority,
            } => {
                let retirement = payload.take_retirement();
                *self = Self::Available { payload, authority };
                retirement
            }
            Self::Claimed { mut payload, claim } => {
                let retirement = payload.take_retirement();
                *self = Self::Claimed { payload, claim };
                retirement
            }
        }
    }

    fn into_retiring(self) -> (Option<Arc<ExecutionGeneration<A>>>, Self) {
        match self {
            Self::AwaitingOpenAbort { .. } => (None, self),
            Self::Unowned {
                payload: ClosePayload::Published(closing),
            } => {
                let (generation, retirement) = closing.into_retirement_with_generation();
                (
                    Some(generation),
                    Self::Unowned {
                        payload: ClosePayload::Retiring(retirement),
                    },
                )
            }
            Self::Available {
                payload: ClosePayload::Published(closing),
                authority,
            } => {
                let (generation, retirement) = closing.into_retirement_with_generation();
                (
                    Some(generation),
                    Self::Available {
                        payload: ClosePayload::Retiring(retirement),
                        authority,
                    },
                )
            }
            Self::Claimed {
                payload: ClosePayload::Published(closing),
                claim,
            } => {
                let (generation, retirement) = closing.into_retirement_with_generation();
                (
                    Some(generation),
                    Self::Claimed {
                        payload: ClosePayload::Retiring(retirement),
                        claim,
                    },
                )
            }
            other => (None, other),
        }
    }

    fn finish_closed(self) -> RemovalAttemptId {
        match self {
            Self::Claimed {
                payload: ClosePayload::Empty,
                claim:
                    RemovalClaimState {
                        attempt,
                        returned: None,
                        ..
                    },
            } => attempt,
            _ => lifecycle_invariant_violation(
                "closed publication requires an empty claimed lifecycle payload",
            ),
        }
    }
}

/// The close payload remains opening-active until the open attempt is either
/// committed or explicitly failed. Once resolved, all authority transitions
/// use `CloseResources` directly.
pub(crate) enum ClosingState<A: crate::Addin> {
    OpeningActive {
        attempt: OpenAttemptId,
        resources: ActiveOpeningClose<A>,
    },
    Ready {
        resources: CloseResources<A>,
    },
}

pub(crate) enum ActiveOpeningClose<A: crate::Addin> {
    AwaitingAbort {
        payload: OpenAbortPayload<A>,
    },
    Published {
        payload: PublishedClosePayload<A>,
        authority: ModuleAuthority,
    },
}

impl<A: crate::Addin> ClosingState<A> {
    const fn open_attempt(&self) -> Option<OpenAttemptId> {
        match self {
            Self::OpeningActive { attempt, .. } => Some(*attempt),
            Self::Ready { .. } => None,
        }
    }

    fn module_epoch_id(&self) -> Option<ModuleEpochId> {
        match self {
            Self::OpeningActive {
                resources: ActiveOpeningClose::Published { authority, .. },
                ..
            } => Some(authority.id()),
            Self::OpeningActive {
                resources: ActiveOpeningClose::AwaitingAbort { .. },
                ..
            } => None,
            Self::Ready { resources } => resources.module_epoch_id(),
        }
    }

    fn removal_attempt(&self) -> Option<RemovalAttemptId> {
        match self {
            Self::OpeningActive { .. } => None,
            Self::Ready { resources } => resources.removal_attempt(),
        }
    }

    fn opening(&self) -> Option<&OpeningGeneration<A>> {
        match self {
            Self::OpeningActive {
                resources: ActiveOpeningClose::AwaitingAbort { payload },
                ..
            } => payload.opening(),
            Self::OpeningActive {
                resources: ActiveOpeningClose::Published { .. },
                ..
            } => None,
            Self::Ready { resources } => resources.opening(),
        }
    }

    fn has_current_generation(&self) -> bool {
        match self {
            Self::OpeningActive {
                resources:
                    ActiveOpeningClose::Published {
                        payload: PublishedClosePayload::Published(_),
                        ..
                    },
                ..
            } => true,
            Self::OpeningActive { .. } => false,
            Self::Ready { resources } => resources.is_published(),
        }
    }

    #[cfg(test)]
    fn has_opening_generation(&self) -> bool {
        match self {
            Self::OpeningActive {
                resources: ActiveOpeningClose::AwaitingAbort { payload },
                ..
            } => payload.has_opening_generation(),
            Self::OpeningActive { .. } => false,
            Self::Ready { resources } => resources.has_opening_generation(),
        }
    }

    fn retiring_services(&self) -> Option<&Arc<GenerationServices>> {
        match self {
            Self::OpeningActive {
                resources: ActiveOpeningClose::Published { payload, .. },
                ..
            } => Some(payload.retiring_services()),
            Self::OpeningActive { .. } => None,
            Self::Ready { resources } => resources.retiring_services(),
        }
    }

    fn resolve_open_failure(self) -> Self {
        match self {
            Self::OpeningActive {
                attempt,
                resources: ActiveOpeningClose::AwaitingAbort { payload },
            } => Self::Ready {
                resources: CloseResources::AwaitingOpenAbort { attempt, payload },
            },
            Self::OpeningActive {
                resources: ActiveOpeningClose::Published { payload, authority },
                ..
            } => Self::Ready {
                resources: CloseResources::Available {
                    payload: payload.into_close_payload(),
                    authority,
                },
            },
            Self::Ready { resources } => Self::Ready { resources },
        }
    }

    fn install_authority(self, authority: ModuleAuthority) -> Self {
        match self {
            Self::OpeningActive {
                attempt,
                resources: ActiveOpeningClose::AwaitingAbort { payload },
            } => Self::Ready {
                resources: CloseResources::AwaitingOpenAbort { attempt, payload }
                    .install_authority(authority),
            },
            Self::OpeningActive { .. } => lifecycle_invariant_violation(
                "module authority installed while closing already owns an authority",
            ),
            Self::Ready { resources } => Self::Ready {
                resources: resources.install_authority(authority),
            },
        }
    }

    fn take_cleanup(self) -> (Option<ModuleCleanupAuthority>, Self) {
        match self {
            Self::OpeningActive { .. } => (None, self),
            Self::Ready { resources } => {
                let (authority, resources) = resources.take_cleanup();
                (authority, Self::Ready { resources })
            }
        }
    }

    fn into_quarantine_resources(self) -> CloseResources<A> {
        match self {
            Self::Ready { resources } => resources,
            Self::OpeningActive { .. } => lifecycle_invariant_violation(
                "opening-active close cannot be quarantined before open failure is resolved",
            ),
        }
    }

    fn into_retiring(self) -> (Option<Arc<ExecutionGeneration<A>>>, Self) {
        match self {
            Self::OpeningActive {
                attempt,
                resources:
                    ActiveOpeningClose::Published {
                        payload: PublishedClosePayload::Published(closing),
                        authority,
                    },
            } => {
                let (generation, retirement) = closing.into_retirement_with_generation();
                (
                    Some(generation),
                    Self::OpeningActive {
                        attempt,
                        resources: ActiveOpeningClose::Published {
                            payload: PublishedClosePayload::Retiring(retirement),
                            authority,
                        },
                    },
                )
            }
            other @ Self::OpeningActive { .. } => (None, other),
            Self::Ready { resources } => {
                let (generation, resources) = resources.into_retiring();
                (generation, Self::Ready { resources })
            }
        }
    }

    fn take_staged(&mut self) -> Option<OpeningGeneration<A>> {
        match self {
            Self::OpeningActive {
                resources: ActiveOpeningClose::AwaitingAbort { payload },
                ..
            } => payload.take_staged(),
            Self::OpeningActive { .. } => None,
            Self::Ready { resources } => resources.take_staged(),
        }
    }

    fn take_retirement(&mut self) -> Option<OpenRetirement> {
        match self {
            Self::OpeningActive { .. } => None,
            Self::Ready { resources } => resources.take_retirement(),
        }
    }

    #[cfg(test)]
    fn take_available_module_closing(&mut self) -> Option<crate::module_runtime::ModuleClosing> {
        let state = mem::replace(
            self,
            Self::Ready {
                resources: CloseResources::Unowned {
                    payload: ClosePayload::Empty,
                },
            },
        );
        match state {
            Self::Ready {
                resources: CloseResources::Available { payload, authority },
            } => {
                let closing = authority.into_closing();
                *self = Self::Ready {
                    resources: CloseResources::Unowned { payload },
                };
                Some(closing)
            }
            other => {
                *self = other;
                None
            }
        }
    }
}

/// A removal claim returned by one canonical lifecycle transition. The module
/// close capability and the attempt identity are issued together; callers do
/// not need a second lookup that can observe a different state.
pub(crate) struct RemovalClaim {
    attempt: RemovalAttemptId,
    module_closing: crate::module_runtime::ModuleClosing,
}

impl RemovalClaim {
    pub(crate) fn attempt(&self) -> RemovalAttemptId {
        self.attempt
    }

    pub(crate) fn into_module_closing(self) -> crate::module_runtime::ModuleClosing {
        self.module_closing
    }
}

/// Canonical lifecycle state and its owned generation payload.
///
/// `LifecycleCoordinator::phase` is only a read-side projection of this
/// enum. The state machine below is deliberately non-`Copy`: moving between
/// phases also moves the staged generation, open bundle, or retirement lease.
pub(crate) enum LifecycleState<A: crate::Addin> {
    Closed(ClosedState),
    Opening {
        attempt: OpenAttemptId,
        payload: OpeningPayload<A>,
    },
    Open {
        bundle: OpenGeneration<A>,
    },
    Closing(ClosingState<A>),
    OpenRollbackPending(CloseResources<A>),
    Quarantined(CloseResources<A>),
}

impl<A: crate::Addin> LifecycleState<A> {
    pub(crate) const fn phase(&self) -> LifecyclePhase {
        match self {
            Self::Closed(_) => LifecyclePhase::Closed,
            Self::Opening { .. } => LifecyclePhase::Opening,
            Self::Open { .. } => LifecyclePhase::Open,
            Self::Closing(_) => LifecyclePhase::Closing,
            Self::OpenRollbackPending(_) => LifecyclePhase::OpenRollbackPending,
            Self::Quarantined(_) => LifecyclePhase::Quarantined,
        }
    }

    pub(crate) const fn open_attempt(&self) -> Option<OpenAttemptId> {
        match self {
            Self::Opening { attempt, .. } => Some(*attempt),
            Self::Closing(closing) => closing.open_attempt(),
            Self::Closed(_)
            | Self::Open { .. }
            | Self::OpenRollbackPending(_)
            | Self::Quarantined(_) => None,
        }
    }

    fn protocol_generation(
        &self,
        last_committed: Option<RuntimeGeneration>,
    ) -> Option<RuntimeGeneration> {
        match self {
            Self::Opening { attempt, .. } => Some(attempt.into_runtime_generation()),
            Self::Open { bundle } => Some(bundle.generation.id()),
            Self::Closing(closing) => closing
                .open_attempt()
                .map(OpenAttemptId::into_runtime_generation)
                .or(last_committed),
            Self::OpenRollbackPending(_) => last_committed,
            Self::Closed(_) | Self::Quarantined(_) => None,
        }
    }

    fn opening(&self) -> Option<&OpeningGeneration<A>> {
        match self {
            Self::Opening { payload, .. } => match payload {
                OpeningPayload::Staged(opening) => Some(opening),
                OpeningPayload::Empty | OpeningPayload::Published(_) => None,
            },
            Self::Closing(closing) => closing.opening(),
            Self::OpenRollbackPending(resources) | Self::Quarantined(resources) => {
                resources.opening()
            }
            Self::Closed(_) | Self::Open { .. } => None,
        }
    }

    fn has_current_generation(&self) -> bool {
        match self {
            Self::Open { .. } => true,
            Self::Opening { payload, .. } => matches!(payload, OpeningPayload::Published(_)),
            Self::Closing(closing) => closing.has_current_generation(),
            Self::OpenRollbackPending(resources) | Self::Quarantined(resources) => {
                resources.is_published()
            }
            Self::Closed(_) => false,
        }
    }

    #[cfg(test)]
    fn has_opening_generation(&self) -> bool {
        match self {
            Self::Opening {
                payload: OpeningPayload::Staged(_),
                ..
            } => true,
            Self::Closing(closing) => closing.has_opening_generation(),
            Self::OpenRollbackPending(resources) | Self::Quarantined(resources) => {
                resources.has_opening_generation()
            }
            Self::Closed(_) | Self::Open { .. } | Self::Opening { .. } => false,
        }
    }

    fn retiring_services(&self) -> Option<&Arc<GenerationServices>> {
        match self {
            Self::Closing(closing) => closing.retiring_services(),
            Self::OpenRollbackPending(resources) | Self::Quarantined(resources) => {
                resources.retiring_services()
            }
            Self::Closed(_) | Self::Open { .. } | Self::Opening { .. } => None,
        }
    }

    fn module_epoch_id(&self) -> Option<ModuleEpochId> {
        match self {
            Self::Opening { payload, .. } => payload.module_epoch_id(),
            Self::Open { bundle } => Some(bundle.module_epoch.id()),
            Self::Closing(closing) => closing.module_epoch_id(),
            Self::OpenRollbackPending(resources) | Self::Quarantined(resources) => {
                resources.module_epoch_id()
            }
            Self::Closed(_) => None,
        }
    }

    fn removal_attempt(&self) -> Option<RemovalAttemptId> {
        match self {
            Self::Closed(ClosedState::Finalizing { attempt }) => Some(*attempt),
            Self::Closing(closing) => closing.removal_attempt(),
            Self::OpenRollbackPending(resources) | Self::Quarantined(resources) => {
                resources.removal_attempt()
            }
            Self::Closed(ClosedState::Idle) | Self::Opening { .. } | Self::Open { .. } => None,
        }
    }

    fn take_opening(&mut self) -> Option<OpeningGeneration<A>> {
        match self {
            Self::Opening { payload, .. } => payload.take_staged(),
            Self::Closing(closing) => closing.take_staged(),
            Self::OpenRollbackPending(resources) | Self::Quarantined(resources) => {
                resources.take_staged()
            }
            Self::Closed(_) | Self::Open { .. } => None,
        }
    }

    fn take_retirement(&mut self) -> Option<OpenRetirement> {
        match self {
            Self::Closing(closing) => closing.take_retirement(),
            Self::OpenRollbackPending(resources) | Self::Quarantined(resources) => {
                resources.take_retirement()
            }
            Self::Closed(_) | Self::Open { .. } | Self::Opening { .. } => None,
        }
    }

    #[cfg(test)]
    fn take_available_module_closing(&mut self) -> Option<crate::module_runtime::ModuleClosing> {
        let state = mem::replace(self, Self::Closed(ClosedState::Idle));
        match state {
            Self::Closing(mut closing) => {
                let result = closing.take_available_module_closing();
                *self = Self::Closing(closing);
                result
            }
            other => {
                *self = other;
                None
            }
        }
    }

    fn install_module_authority(&mut self, authority: ModuleAuthority) {
        let state = mem::replace(self, Self::Closed(ClosedState::Idle));
        *self = match state {
            Self::Closed(ClosedState::Idle) => Self::Closing(ClosingState::Ready {
                resources: CloseResources::Available {
                    payload: ClosePayload::Empty,
                    authority,
                },
            }),
            Self::Opening { attempt, payload } => Self::OpenRollbackPending(
                payload
                    .into_close_resources(attempt)
                    .install_authority(authority),
            ),
            Self::Closing(closing) => Self::Closing(closing.install_authority(authority)),
            Self::OpenRollbackPending(resources) => {
                Self::OpenRollbackPending(resources.install_authority(authority))
            }
            Self::Quarantined(resources) => {
                Self::Quarantined(resources.install_authority(authority))
            }
            Self::Open { .. } | Self::Closed(ClosedState::Finalizing { .. }) => {
                lifecycle_invariant_violation(
                    "module authority installed in an incompatible lifecycle state",
                )
            }
        };
    }

    fn into_quarantine_resources(self) -> CloseResources<A> {
        match self {
            Self::Closed(ClosedState::Idle) => CloseResources::Unowned {
                payload: ClosePayload::Empty,
            },
            Self::Closed(ClosedState::Finalizing { .. }) => lifecycle_invariant_violation(
                "finalizing removal cannot be moved to quarantine without its owner",
            ),
            Self::Opening { attempt, payload } => payload.into_close_resources(attempt),
            Self::Open { bundle } => {
                let (closing, authority) = bundle.into_closing();
                CloseResources::Available {
                    payload: ClosePayload::Published(closing),
                    authority,
                }
            }
            Self::Closing(closing) => closing.into_quarantine_resources(),
            Self::OpenRollbackPending(resources) | Self::Quarantined(resources) => resources,
        }
    }
}

/// A terminal-state capability produced only from an exact canonical closing
/// shape. The committed variant carries the identities needed by the final
/// removal certificate; the uncommitted variant carries no generation data.
pub(crate) enum FinalRemovalReady {
    Committed {
        generation: RuntimeGeneration,
        module_epoch: ModuleEpochId,
    },
    Uncommitted,
}

/// A proof that the canonical lifecycle state is ready to finish an open
/// rollback. Its private field prevents callers from minting the capability.
pub(crate) struct OpenRollbackReady {
    _private: (),
}

/// Canonical owner of every mutable lifecycle decision and generation root.
struct LifecycleCore<A: crate::Addin> {
    state: LifecycleState<A>,
    host_intent: HostLifecycleIntent,
    next_lifecycle_attempt: u64,
    next_removal_attempt: u64,
    last_committed_generation: Option<RuntimeGeneration>,
    removal_epoch: u64,
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
            state: LifecycleState::Closed(ClosedState::Idle),
            host_intent: HostLifecycleIntent::None,
            next_lifecycle_attempt: 1,
            next_removal_attempt: 1,
            last_committed_generation: None,
            removal_epoch: 0,
        }
    }

    /// Returns the mutex-protected canonical state. It is intentionally a
    /// reference because the state owns the phase payload.
    const fn canonical_state(&self) -> &LifecycleState<A> {
        &self.state
    }

    const fn host_intent(&self) -> HostLifecycleIntent {
        self.host_intent
    }

    const fn last_committed_generation(&self) -> Option<RuntimeGeneration> {
        self.last_committed_generation
    }

    fn protocol_generation(&self) -> Option<RuntimeGeneration> {
        self.state
            .protocol_generation(self.last_committed_generation)
    }

    fn final_removal_ready(&self, expected_attempt: RemovalAttemptId) -> Option<FinalRemovalReady> {
        match &self.state {
            LifecycleState::Closing(ClosingState::Ready { resources }) => {
                resources.final_removal_ready(expected_attempt)
            }
            _ => None,
        }
    }

    fn open_rollback_ready(&self, expected_attempt: RemovalAttemptId) -> Option<OpenRollbackReady> {
        match &self.state {
            LifecycleState::OpenRollbackPending(resources)
            | LifecycleState::Closing(ClosingState::Ready { resources }) => {
                resources.open_rollback_ready(expected_attempt)
            }
            _ => None,
        }
    }

    const fn removal_epoch(&self) -> u64 {
        self.removal_epoch
    }

    fn removal_attempt(&self) -> Option<RemovalAttemptId> {
        self.state.removal_attempt()
    }

    #[cfg(test)]
    fn set_next_lifecycle_attempt_for_test(&mut self, value: u64) {
        self.next_lifecycle_attempt = value;
    }

    fn opening_config(&self) -> Option<crate::addin::RuntimeConfig> {
        self.state.opening().map(|opening| opening.init_config)
    }

    fn has_current_generation(&self) -> bool {
        self.state.has_current_generation()
    }

    fn retiring_services(&self) -> Option<&Arc<GenerationServices>> {
        self.state.retiring_services()
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

    pub(crate) fn final_removal_ready(
        &self,
        expected_attempt: RemovalAttemptId,
    ) -> Option<FinalRemovalReady> {
        self.core.final_removal_ready(expected_attempt)
    }

    pub(crate) fn open_rollback_ready(
        &self,
        expected_attempt: RemovalAttemptId,
    ) -> Option<OpenRollbackReady> {
        self.core.open_rollback_ready(expected_attempt)
    }

    pub(crate) fn module_epoch_id(&self) -> Option<ModuleEpochId> {
        self.canonical_state().module_epoch_id()
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

    #[cfg(test)]
    pub(crate) fn has_opening_generation(&self) -> bool {
        self.core.state.has_opening_generation()
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
    pub(crate) module_epoch: ModuleEpochLease,
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

    #[cfg(test)]
    pub(in crate::lifecycle) fn take_module_closing_for_close(
        &self,
        access: &mut LifecycleAccess<'_, A>,
    ) -> Option<crate::module_runtime::ModuleClosing> {
        access.transition(|core| {
            (
                core.state.take_available_module_closing(),
                TransitionEffect::Keep,
            )
        })
    }

    pub(in crate::lifecycle) fn take_module_cleanup_for_quarantine(
        &self,
    ) -> Option<ModuleCleanupAuthority> {
        let mut access = self.access();
        access.transition(|core| {
            let state = mem::replace(&mut core.state, LifecycleState::Closed(ClosedState::Idle));
            let (authority, state) = match state {
                LifecycleState::Closing(closing) => {
                    let (authority, closing) = closing.take_cleanup();
                    (authority, LifecycleState::Closing(closing))
                }
                LifecycleState::OpenRollbackPending(resources) => {
                    let (authority, resources) = resources.take_cleanup();
                    (authority, LifecycleState::OpenRollbackPending(resources))
                }
                LifecycleState::Quarantined(resources) => {
                    let (authority, resources) = resources.take_cleanup();
                    (authority, LifecycleState::Quarantined(resources))
                }
                other => (None, other),
            };
            core.state = state;
            (authority, TransitionEffect::Keep)
        })
    }

    pub(in crate::lifecycle) fn complete_open_abort(
        &self,
        closing: crate::module_runtime::ModuleClosing,
    ) {
        let mut access = self.access();
        self.complete_open_abort_locked(&mut access, closing);
    }

    pub(in crate::lifecycle) fn complete_open_abort_locked(
        &self,
        access: &mut LifecycleAccess<'_, A>,
        closing: crate::module_runtime::ModuleClosing,
    ) {
        access.transition(|core| {
            core.state
                .install_module_authority(ModuleAuthority::Closing(closing));
            ((), TransitionEffect::Keep)
        });
    }

    pub(in crate::lifecycle) fn clear_certified_retirement(
        &self,
        access: &mut LifecycleAccess<'_, A>,
    ) -> bool {
        access.transition(|core| {
            let Some(retirement) = core.state.take_retirement() else {
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

    fn advance_removal_epoch(&self, access: &mut LifecycleAccess<'_, A>) {
        access.transition(|core| {
            core.removal_epoch = core.removal_epoch.checked_add(1).unwrap_or_else(|| {
                lifecycle_invariant_violation("lifecycle close epoch exhausted");
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
            core.state = LifecycleState::Opening {
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
            let state = mem::replace(&mut core.state, LifecycleState::Closed(ClosedState::Idle));
            match state {
                LifecycleState::Opening {
                    attempt,
                    payload: OpeningPayload::Published(bundle),
                } => {
                    if attempt.into_runtime_generation() != generation
                        || bundle.generation.id() != generation
                    {
                        core.state = LifecycleState::Opening {
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
                    core.state = LifecycleState::Open { bundle };
                    let effect = if let LifecycleState::Open { bundle } = core.canonical_state() {
                        Self::publish_effect(bundle)
                    } else {
                        unreachable!("open bundle was just installed");
                    };
                    (Ok(()), effect)
                }
                other => {
                    core.state = other;
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

    /// Records an open failure without discarding the owned staged/published
    /// payload. The rollback pipeline can then take that payload explicitly.
    pub(in crate::lifecycle) fn record_open_failure(
        &self,
        core: &mut LifecycleAccess<'_, A>,
    ) -> OpenFailureDisposition {
        core.transition(|core| {
            let state = mem::replace(&mut core.state, LifecycleState::Closed(ClosedState::Idle));
            let (state, disposition) = match state {
                LifecycleState::Opening { attempt, payload } => (
                    LifecycleState::OpenRollbackPending(payload.into_close_resources(attempt)),
                    OpenFailureDisposition::RollbackRequired,
                ),
                LifecycleState::OpenRollbackPending(resources) => (
                    LifecycleState::OpenRollbackPending(resources),
                    OpenFailureDisposition::RollbackRequired,
                ),
                LifecycleState::Closing(closing) => (
                    LifecycleState::Closing(closing.resolve_open_failure()),
                    OpenFailureDisposition::ClosingOwnsCleanup,
                ),
                other => (other, OpenFailureDisposition::ClosingOwnsCleanup),
            };
            core.state = state;
            (disposition, TransitionEffect::Keep)
        })
    }

    /// Requests closing while moving the active generation payload under the
    /// closing phase. No payload remains in a separate core field.
    pub(in crate::lifecycle) fn request_closing(&self, core: &mut LifecycleAccess<'_, A>) {
        core.transition(|core| {
            let state = mem::replace(&mut core.state, LifecycleState::Closed(ClosedState::Idle));
            let next = match state {
                LifecycleState::Closed(ClosedState::Idle) => {
                    LifecycleState::Closing(ClosingState::Ready {
                        resources: CloseResources::Unowned {
                            payload: ClosePayload::Empty,
                        },
                    })
                }
                LifecycleState::Opening { attempt, payload } => {
                    LifecycleState::Closing(payload.into_closing_state(attempt))
                }
                LifecycleState::Open { bundle } => {
                    let (closing, authority) = bundle.into_closing();
                    LifecycleState::Closing(ClosingState::Ready {
                        resources: CloseResources::Available {
                            payload: ClosePayload::Published(closing),
                            authority,
                        },
                    })
                }
                LifecycleState::Closing(closing) => LifecycleState::Closing(closing),
                LifecycleState::OpenRollbackPending(resources) => {
                    LifecycleState::Closing(ClosingState::Ready { resources })
                }
                LifecycleState::Quarantined(resources) => LifecycleState::Quarantined(resources),
                LifecycleState::Closed(ClosedState::Finalizing { attempt }) => {
                    LifecycleState::Closed(ClosedState::Finalizing { attempt })
                }
            };
            core.state = next;
            let effect = if matches!(core.canonical_state(), LifecycleState::Closing(_)) {
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

    pub(in crate::lifecycle) fn claim_removal(
        &self,
        core: &mut LifecycleAccess<'_, A>,
    ) -> Option<RemovalClaim> {
        core.transition(|core| {
            let state = mem::replace(&mut core.state, LifecycleState::Closed(ClosedState::Idle));
            let (claim, state) = match state {
                LifecycleState::Closing(ClosingState::Ready {
                    resources: CloseResources::Available { payload, authority },
                }) => {
                    let attempt =
                        RemovalAttemptId::new(core.next_removal_attempt).unwrap_or_else(|| {
                            lifecycle_invariant_violation(
                                "lifecycle removal-attempt identity reached zero",
                            )
                        });
                    core.next_removal_attempt =
                        core.next_removal_attempt.checked_add(1).unwrap_or_else(|| {
                            lifecycle_invariant_violation(
                                "lifecycle removal-attempt identity exhausted",
                            )
                        });
                    let (resources, module_closing) =
                        CloseResources::Available { payload, authority }.claim(attempt);
                    (
                        Some(RemovalClaim {
                            attempt,
                            module_closing,
                        }),
                        LifecycleState::Closing(ClosingState::Ready { resources }),
                    )
                }
                LifecycleState::OpenRollbackPending(CloseResources::Available {
                    payload,
                    authority,
                }) => {
                    let attempt =
                        RemovalAttemptId::new(core.next_removal_attempt).unwrap_or_else(|| {
                            lifecycle_invariant_violation(
                                "lifecycle removal-attempt identity reached zero",
                            )
                        });
                    core.next_removal_attempt =
                        core.next_removal_attempt.checked_add(1).unwrap_or_else(|| {
                            lifecycle_invariant_violation(
                                "lifecycle removal-attempt identity exhausted",
                            )
                        });
                    let (resources, module_closing) =
                        CloseResources::Available { payload, authority }.claim(attempt);
                    (
                        Some(RemovalClaim {
                            attempt,
                            module_closing,
                        }),
                        LifecycleState::OpenRollbackPending(resources),
                    )
                }
                other => (None, other),
            };
            core.state = state;
            (claim, TransitionEffect::Keep)
        })
    }

    pub(in crate::lifecycle) fn release_removal_claim(
        &self,
        core: &mut LifecycleAccess<'_, A>,
        attempt: RemovalAttemptId,
        returned: Option<ModuleAuthority>,
    ) {
        core.transition(|core| {
            let state = mem::replace(&mut core.state, LifecycleState::Closed(ClosedState::Idle));
            core.state = match state {
                LifecycleState::Closed(ClosedState::Finalizing { attempt: expected }) => {
                    require_lifecycle_invariant(
                        expected == attempt,
                        "removal owner identity does not match the finalizing lifecycle owner",
                    );
                    require_lifecycle_invariant(
                        returned.is_none(),
                        "finalizing removal owner returned module authority",
                    );
                    LifecycleState::Closed(ClosedState::Idle)
                }
                LifecycleState::Closing(ClosingState::Ready { resources }) => {
                    LifecycleState::Closing(ClosingState::Ready {
                        resources: resources.release(attempt, returned),
                    })
                }
                LifecycleState::OpenRollbackPending(resources) => {
                    LifecycleState::OpenRollbackPending(resources.release(attempt, returned))
                }
                LifecycleState::Quarantined(resources) => {
                    LifecycleState::Quarantined(resources.release(attempt, returned))
                }
                _ => lifecycle_invariant_violation(
                    "removal owner release does not match canonical lifecycle ownership",
                ),
            };
            ((), TransitionEffect::Keep)
        });
    }

    pub(in crate::lifecycle) fn finish_final_removal(
        &self,
        core: &mut LifecycleAccess<'_, A>,
        attempt: RemovalAttemptId,
    ) -> crate::XllResult<()> {
        self.finish_closed(
            core,
            attempt,
            false,
            crate::diagnostics::id::DiagnosticId::CLOSE_RUNTIME,
        )
    }

    pub(in crate::lifecycle) fn finish_open_rollback(
        &self,
        core: &mut LifecycleAccess<'_, A>,
        attempt: RemovalAttemptId,
    ) -> crate::XllResult<()> {
        self.finish_closed(
            core,
            attempt,
            true,
            crate::diagnostics::id::DiagnosticId::OPEN_ROLLBACK_PHASE,
        )
    }

    fn finish_closed(
        &self,
        core: &mut LifecycleAccess<'_, A>,
        expected_attempt: RemovalAttemptId,
        allow_open_rollback: bool,
        diagnostic_id: crate::diagnostics::id::DiagnosticId,
    ) -> crate::XllResult<()> {
        core.transition(|core| {
            let state = mem::replace(&mut core.state, LifecycleState::Closed(ClosedState::Idle));
            let (resources, open_rollback_state) = match state {
                LifecycleState::Closing(ClosingState::Ready { resources }) => (resources, false),
                LifecycleState::OpenRollbackPending(resources) if allow_open_rollback => {
                    (resources, true)
                }
                other => {
                    core.state = other;
                    return (
                        Err(crate::XllError::Internal { diagnostic_id }),
                        TransitionEffect::Keep,
                    );
                }
            };
            if resources.removal_attempt() != Some(expected_attempt) {
                core.state = if open_rollback_state {
                    LifecycleState::OpenRollbackPending(resources)
                } else {
                    LifecycleState::Closing(ClosingState::Ready { resources })
                };
                return (
                    Err(crate::XllError::Internal { diagnostic_id }),
                    TransitionEffect::Keep,
                );
            }
            let attempt = resources.finish_closed();
            core.state = LifecycleState::Closed(ClosedState::Finalizing { attempt });
            (Ok(()), TransitionEffect::ClearPublication)
        })
    }

    pub(in crate::lifecycle) fn quarantine_core(&self, core: &mut LifecycleAccess<'_, A>) {
        core.transition(|core| {
            let state = mem::replace(&mut core.state, LifecycleState::Closed(ClosedState::Idle));
            core.state = LifecycleState::Quarantined(state.into_quarantine_resources());
            ((), TransitionEffect::ClearPublication)
        });
    }

    pub(in crate::lifecycle) fn stage_opening_generation_locked(
        &self,
        core: &mut LifecycleAccess<'_, A>,
        opening: OpeningGeneration<A>,
    ) -> Result<(), (crate::XllError, OpeningGeneration<A>)> {
        core.transition(|core| {
            let state = mem::replace(&mut core.state, LifecycleState::Closed(ClosedState::Idle));
            match state {
                LifecycleState::Opening {
                    attempt,
                    payload: OpeningPayload::Empty,
                } => {
                    core.state = LifecycleState::Opening {
                        attempt,
                        payload: OpeningPayload::Staged(opening),
                    };
                    (Ok(()), TransitionEffect::Keep)
                }
                other => {
                    core.state = other;
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
    ) -> Result<(), Box<PublishOpeningError<A>>> {
        core.transition(|core| {
            let open_state_error = || crate::XllError::Internal {
                diagnostic_id: crate::diagnostics::id::DiagnosticId::OPEN_STATE,
            };
            if core.has_current_generation() {
                return (
                    Err(Box::new(PublishOpeningError {
                        error: open_state_error(),
                        opening: core.state.take_opening(),
                        module_epoch,
                    })),
                    TransitionEffect::Keep,
                );
            }
            let Some(attempt) = (match core.canonical_state() {
                LifecycleState::Opening { attempt, .. } => Some(*attempt),
                _ => None,
            }) else {
                return (
                    Err(Box::new(PublishOpeningError {
                        error: open_state_error(),
                        opening: core.state.take_opening(),
                        module_epoch,
                    })),
                    TransitionEffect::Keep,
                );
            };
            let Some(opening) = core.state.take_opening() else {
                return (
                    Err(Box::new(PublishOpeningError {
                        error: open_state_error(),
                        opening: None,
                        module_epoch,
                    })),
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
            let bundle = OpenGeneration {
                generation: Arc::clone(&published),
                services,
                module_epoch,
            };
            core.state = LifecycleState::Opening {
                attempt,
                payload: OpeningPayload::Published(bundle),
            };
            (Ok(()), TransitionEffect::Keep)
        })
    }

    #[cfg(test)]
    pub(crate) fn has_opening_generation(&self) -> bool {
        self.access().has_opening_generation()
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
        access.transition(|core| (core.state.take_opening(), TransitionEffect::Keep))
    }

    fn take_current_bundle(
        &self,
        core: &mut LifecycleAccess<'_, A>,
    ) -> Option<Arc<ExecutionGeneration<A>>> {
        core.transition(|core| {
            let state = mem::replace(&mut core.state, LifecycleState::Closed(ClosedState::Idle));
            match state {
                LifecycleState::Open { bundle } => {
                    let (closing, authority) = bundle.into_closing();
                    let generation = Arc::clone(&closing.generation);
                    core.state = LifecycleState::Closing(ClosingState::Ready {
                        resources: CloseResources::Available {
                            payload: ClosePayload::Retiring(closing.into_retirement()),
                            authority,
                        },
                    });
                    (Some(generation), TransitionEffect::ClearPublication)
                }
                LifecycleState::Closing(closing) => {
                    let (generation, closing) = closing.into_retiring();
                    core.state = LifecycleState::Closing(closing);
                    let effect = if generation.is_some() {
                        TransitionEffect::ClearPublication
                    } else {
                        TransitionEffect::Keep
                    };
                    (generation, effect)
                }
                LifecycleState::OpenRollbackPending(resources) => {
                    let (generation, resources) = resources.into_retiring();
                    core.state = LifecycleState::OpenRollbackPending(resources);
                    let effect = if generation.is_some() {
                        TransitionEffect::ClearPublication
                    } else {
                        TransitionEffect::Keep
                    };
                    (generation, effect)
                }
                LifecycleState::Quarantined(resources) => {
                    let (generation, resources) = resources.into_retiring();
                    core.state = LifecycleState::Quarantined(resources);
                    let effect = if generation.is_some() {
                        TransitionEffect::ClearPublication
                    } else {
                        TransitionEffect::Keep
                    };
                    (generation, effect)
                }
                other => {
                    core.state = other;
                    (None, TransitionEffect::Keep)
                }
            }
        })
    }

    #[cfg(test)]
    pub(crate) fn take_current_generation(&self) -> Option<Arc<ExecutionGeneration<A>>> {
        let mut core = self.access();
        self.take_current_bundle(&mut core)
    }

    #[cfg(any(all(test, feature = "handles"), feature = "bench-internals"))]
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
                core.state.take_opening().map(ShutdownGeneration::Opening),
                TransitionEffect::Keep,
            )
        })
    }
}

fn validate_removal_owner(
    expected: RemovalAttemptId,
    actual: RemovalAttemptId,
) -> RemovalAttemptId {
    require_lifecycle_invariant(
        expected == actual,
        "removal owner identity does not match the canonical lifecycle owner",
    );
    expected
}

fn combine_returned_authority(
    retained: Option<ModuleAuthority>,
    returned: Option<ModuleAuthority>,
) -> Option<ModuleAuthority> {
    match (retained, returned) {
        (None, returned) | (returned, None) => returned,
        (Some(_), Some(_)) => {
            lifecycle_invariant_violation("removal owner returned module authority twice")
        }
    }
}

//! Canonical lifecycle ownership and its non-owning read-side projection.

#![allow(
    unsafe_code,
    reason = "the publication pointer is protected by the lifecycle-owned drain gate"
)]

use crate::sync::{Condvar, Mutex, MutexGuard};
use std::mem;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicPtr, AtomicU8, Ordering};
use xlfn_kernel::drain_gate::{DEFAULT_STRIPE_COUNT, StripedDrainGate, StripedDrainPermit};
use xlfn_kernel::published_owner::PublishedOwner;

mod authority_slot;
use authority_slot::PreparedAuthorityInstall;

mod opening;
pub(crate) use opening::OpenStart;

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
    ClaimableModuleAuthority, ModuleAuthority, ModuleCleanupAuthority, ModuleCleanupClaim,
    ModuleCleanupReady, ModuleEpochId, ModuleEpochLease, UnavailableModuleAuthority,
};
use crate::runtime_components::GenerationServices;

mod removal;
pub(crate) use removal::{FinalCloseDecision, IssuedRemovalClaim, RollbackDecision};

/// A read-side publication of one coherent open generation.
///
/// The root and its generation services are published together. A reader can
/// therefore never observe a generation root from one open attempt with
/// services from another attempt.
struct PublishedGeneration<A: crate::Addin> {
    root: NonNull<ExecutionGeneration<A>>,
    services: NonNull<
        GenerationServices<
            crate::addin_impl::AddinSubscription<A>,
            crate::addin_impl::AddinSources<A>,
        >,
    >,
}

impl<A: crate::Addin> PublishedGeneration<A> {
    fn new(
        root: &ExecutionGeneration<A>,
        services: &GenerationServices<
            crate::addin_impl::AddinSubscription<A>,
            crate::addin_impl::AddinSources<A>,
        >,
    ) -> Self {
        Self {
            root: NonNull::from(root),
            services: NonNull::from(services),
        }
    }

    fn root(&self) -> &ExecutionGeneration<A> {
        // SAFETY: publication implies the generation root is valid for the
        // duration of the read-side lease.
        unsafe { self.root.as_ref() }
    }

    fn services(
        &self,
    ) -> &GenerationServices<
        crate::addin_impl::AddinSubscription<A>,
        crate::addin_impl::AddinSources<A>,
    > {
        // SAFETY: publication implies generation services are valid for the
        // duration of the read-side lease.
        unsafe { self.services.as_ref() }
    }
}

// SAFETY: both pointers target uniquely owned published allocations whose payloads are
// Send + Sync by their contracts. Access is possible only while the
// publication drain gate holds an admission.
unsafe impl<A: crate::Addin> Send for PublishedGeneration<A> {}
// SAFETY: same invariant as Send; concurrent reads are synchronized by admission.
unsafe impl<A: crate::Addin> Sync for PublishedGeneration<A> {}

/// Explicit open-generation lifetime lease for call-scoped and asynchronous
/// UDF executions.
pub struct ExecutionLease<A: crate::Addin> {
    generation: NonNull<ExecutionGeneration<A>>,
    _permit: xlfn_kernel::drain_gate::StripedOwnedDrainPermit<
        { xlfn_kernel::drain_gate::DEFAULT_STRIPE_COUNT },
    >,
}

impl<A: crate::Addin> ExecutionLease<A> {
    /// Creates a temporal lease over the uniquely owned generation root.
    ///
    /// # Safety
    ///
    /// `generation` must remain valid until `permit` is dropped, and the gate
    /// that issued `permit` must be drained before the generation is reclaimed.
    #[cfg(feature = "async")]
    unsafe fn new(
        generation: NonNull<ExecutionGeneration<A>>,
        permit: xlfn_kernel::drain_gate::StripedOwnedDrainPermit<
            { xlfn_kernel::drain_gate::DEFAULT_STRIPE_COUNT },
        >,
    ) -> Self {
        Self {
            generation,
            _permit: permit,
        }
    }

    #[must_use]
    /// Borrows shared state while this execution lease retains the generation.
    pub fn state(&self) -> &A::SharedState {
        // SAFETY: construction couples the pointer to the owned drain permit.
        &unsafe { self.generation.as_ref() }.shared_state
    }

    /// Returns the published runtime-generation identity carried by this
    /// execution lease. This is consumed only by the hidden async-handle
    /// launch boundary when it creates its compile-time scope brand.
    #[cfg(all(feature = "async", feature = "handles"))]
    pub(crate) fn generation(&self) -> RuntimeGeneration {
        // SAFETY: the execution lease's owned drain permit keeps the
        // generation root alive for the duration of this borrow.
        unsafe { self.generation.as_ref() }.id()
    }
}

// SAFETY: the generation payload is Send + Sync by the Addin contract and the
// owned permit prevents reclamation while the pointer crosses threads.
unsafe impl<A: crate::Addin> Send for ExecutionLease<A> {}
// SAFETY: same invariant as Send; shared borrows access immutable shared_state.
unsafe impl<A: crate::Addin> Sync for ExecutionLease<A> {}

/// A non-owning read lease that permits one call from the published generation.
///
/// The lease holds a non-owning pointer to the root published by the current
/// open generation. It is created only by `try_admit` and consumed only by
/// enter. It is empty throughout opening and is cleared at the beginning of
/// closing, so a loaded publication already carries the lifecycle decision;
/// callers do not need to combine it with a separately observed phase.
pub(crate) struct GenerationAdmission<'lifecycle, A: crate::Addin> {
    publication: NonNull<PublishedGeneration<A>>,
    _permit: StripedDrainPermit<'lifecycle, DEFAULT_STRIPE_COUNT>,
    #[cfg(feature = "async")]
    execution_issuer: Option<&'static LifecycleCoordinator<A>>,
}

impl<'lifecycle, A: crate::Addin> GenerationAdmission<'lifecycle, A> {
    fn new(
        publication: NonNull<PublishedGeneration<A>>,
        permit: StripedDrainPermit<'lifecycle, DEFAULT_STRIPE_COUNT>,
    ) -> Self {
        Self {
            publication,
            _permit: permit,
            #[cfg(feature = "async")]
            execution_issuer: None,
        }
    }

    fn publication(&self) -> &PublishedGeneration<A> {
        // SAFETY: the lifecycle owns the pointed-to allocation and cannot reclaim it
        // before every publication permit has drained.
        unsafe { self.publication.as_ref() }
    }

    pub(crate) fn generation(&self) -> &ExecutionGeneration<A> {
        self.publication().root()
    }

    #[cfg(feature = "async")]
    pub(crate) fn try_execution_lease(
        &self,
    ) -> crate::XllResult<crate::generation::ExecutionLease<A>> {
        let issuer = self.execution_issuer.ok_or(crate::XllError::Closing)?;
        // Admission's original permit keeps its publication/root alive while
        // trying a fresh admission on the exact process-lifetime issuer. A
        // close that already sealed the gate must reject this new launch.
        let permit = issuer
            .publication_readers
            .try_enter_owned_current()
            .map_err(|_| crate::XllError::Closing)?;
        // SAFETY: the static issuer was installed by try_admit_async on the
        // same coordinator that issued this admission and owns its root. Its
        // publication gate drains before that root can be reclaimed.
        Ok(unsafe { crate::generation::ExecutionLease::new(self.publication().root, permit) })
    }

    pub(crate) fn services(
        &self,
    ) -> &GenerationServices<
        crate::addin_impl::AddinSubscription<A>,
        crate::addin_impl::AddinSources<A>,
    > {
        self.publication().services()
    }
}

/// The complete ownership bundle for a published generation.
pub(crate) struct OpenGeneration<A: crate::Addin> {
    generation: PublishedOwner<ExecutionGeneration<A>>,
    services: PublishedOwner<
        GenerationServices<
            crate::addin_impl::AddinSubscription<A>,
            crate::addin_impl::AddinSources<A>,
        >,
    >,
    publication: PublishedOwner<PublishedGeneration<A>>,
    module_epoch: ModuleEpochLease,
}

/// The generation resources retained after its module lease moves into the
/// closing ownership slot.
pub(crate) struct ClosingGeneration<A: crate::Addin> {
    generation: PublishedOwner<ExecutionGeneration<A>>,
    services: PublishedOwner<
        GenerationServices<
            crate::addin_impl::AddinSubscription<A>,
            crate::addin_impl::AddinSources<A>,
        >,
    >,
    publication: PublishedOwner<PublishedGeneration<A>>,
}

/// Ownership retained after the generation root has been handed to the
/// shutdown/quiesce pipeline. The generation identity remains part of the
/// payload while the module authority remains in the lifecycle state.
pub(crate) struct OpenRetirement<A: crate::Addin> {
    generation: RuntimeGeneration,
    services: PublishedOwner<
        GenerationServices<
            crate::addin_impl::AddinSubscription<A>,
            crate::addin_impl::AddinSources<A>,
        >,
    >,
    _publication: PublishedOwner<PublishedGeneration<A>>,
}

impl<A: crate::Addin> OpenGeneration<A> {
    fn into_closing(self) -> (ClosingGeneration<A>, ClaimableModuleAuthority) {
        let Self {
            generation,
            services,
            publication,
            module_epoch,
        } = self;
        (
            ClosingGeneration {
                generation,
                services,
                publication,
            },
            ClaimableModuleAuthority::Open(module_epoch),
        )
    }
}

impl<A: crate::Addin> ClosingGeneration<A> {
    fn into_retirement_with_generation(
        self,
    ) -> (PublishedOwner<ExecutionGeneration<A>>, OpenRetirement<A>) {
        let Self {
            generation,
            services,
            publication,
        } = self;
        let generation_id = generation.id();
        (
            generation,
            OpenRetirement {
                generation: generation_id,
                services,
                _publication: publication,
            },
        )
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
                _attempt: attempt,
                payload: OpenAbortPayload::Empty,
            },
            Self::Staged(opening) => CloseResources::AwaitingOpenAbort {
                _attempt: attempt,
                payload: OpenAbortPayload::Staged(opening),
            },
            Self::Published(bundle) => {
                let (closing, authority) = bundle.into_closing();
                CloseResources::Available(AvailableClose {
                    payload: ClosePayload::Published(closing),
                    authority,
                })
            }
        }
    }

    fn into_closing_state(self, attempt: OpenAttemptId) -> ClosingState<A> {
        match self {
            Self::Empty => ClosingState::OpeningActive {
                attempt,
                quarantine_requested: false,
                resources: ActiveOpeningClose::AwaitingAbort {
                    payload: OpenAbortPayload::Empty,
                },
            },
            Self::Staged(opening) => ClosingState::OpeningActive {
                attempt,
                quarantine_requested: false,
                resources: ActiveOpeningClose::AwaitingAbort {
                    payload: OpenAbortPayload::Staged(opening),
                },
            },
            Self::Published(bundle) => {
                let (closing, authority) = bundle.into_closing();
                ClosingState::OpeningActive {
                    attempt,
                    quarantine_requested: false,
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
    Retiring(OpenRetirement<A>),
}

impl<A: crate::Addin> ClosePayload<A> {
    fn is_published(&self) -> bool {
        matches!(self, Self::Published(_))
    }

    #[cfg(any(test, feature = "refinement"))]
    fn has_opening_generation(&self) -> bool {
        matches!(self, Self::Staged(_))
    }

    fn opening(&self) -> Option<&OpeningGeneration<A>> {
        match self {
            Self::Staged(opening) => Some(opening),
            Self::Empty | Self::Published(_) | Self::Retiring(_) => None,
        }
    }

    fn retiring_services(
        &self,
    ) -> Option<
        &GenerationServices<
            crate::addin_impl::AddinSubscription<A>,
            crate::addin_impl::AddinSources<A>,
        >,
    > {
        match self {
            Self::Retiring(retirement) => Some(retirement.services.as_ref()),
            Self::Published(bundle) => Some(bundle.services.as_ref()),
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

    fn take_retirement(&mut self) -> Option<OpenRetirement<A>> {
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
    Finalizing {
        attempt: RemovalAttemptId,
        quarantine_requested: bool,
    },
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

    #[cfg(any(test, feature = "refinement"))]
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
    Retiring(OpenRetirement<A>),
}

impl<A: crate::Addin> PublishedClosePayload<A> {
    fn into_close_payload(self) -> ClosePayload<A> {
        match self {
            Self::Published(closing) => ClosePayload::Published(closing),
            Self::Retiring(retirement) => ClosePayload::Retiring(retirement),
        }
    }

    fn retiring_services(
        &self,
    ) -> &GenerationServices<
        crate::addin_impl::AddinSubscription<A>,
        crate::addin_impl::AddinSources<A>,
    > {
        match self {
            Self::Published(closing) => closing.services.as_ref(),
            Self::Retiring(retirement) => retirement.services.as_ref(),
        }
    }
}

/// A module authority is either held by the pipeline or by canonical state,
/// never an independent return candidate in both the claim guard and state.
pub(crate) enum RemovalClaimState {
    Outstanding(OutstandingModuleClaim),
    Returned(ReturnedModuleClaim),
}

pub(crate) struct OutstandingModuleClaim {
    attempt: RemovalAttemptId,
    module_epoch: ModuleEpochId,
}

pub(crate) struct ReturnedModuleClaim {
    attempt: RemovalAttemptId,
    authority: ModuleAuthority,
}

impl OutstandingModuleClaim {
    fn return_authority(self, authority: ModuleAuthority) -> ReturnedModuleClaim {
        // The authority crossed unlocked cleanup and may originate from an
        // external recovery path. Its epoch is checked at this acquisition.
        require_lifecycle_invariant(
            authority.id() == self.module_epoch,
            "returned module authority belongs to a different epoch",
        );
        ReturnedModuleClaim {
            attempt: self.attempt,
            authority,
        }
    }
}

impl RemovalClaimState {
    fn attempt(&self) -> RemovalAttemptId {
        match self {
            Self::Outstanding(claim) => claim.attempt,
            Self::Returned(claim) => claim.attempt,
        }
    }

    fn module_epoch(&self) -> ModuleEpochId {
        match self {
            Self::Outstanding(claim) => claim.module_epoch,
            Self::Returned(claim) => claim.authority.id(),
        }
    }
}

/// Resources eligible for a new removal claim. The authority contains only
/// epochs whose close transition has not already been drained or retained.
pub(crate) struct AvailableClose<A: crate::Addin> {
    payload: ClosePayload<A>,
    authority: ClaimableModuleAuthority,
}

/// Resources whose removal authority is held by exactly one owner.
pub(crate) struct ClaimedClose<A: crate::Addin> {
    payload: ClosePayload<A>,
    claim: RemovalClaimState,
}

/// The terminal receiver contains no generation payload or retained module
/// authority. It is acquired only while consuming a matching canonical shape.
struct EmptyClaimedClose {
    attempt: RemovalAttemptId,
    module_epoch: ModuleEpochId,
}

impl EmptyClaimedClose {
    fn finish_closed(self) -> ClosedState {
        ClosedState::Finalizing {
            attempt: self.attempt,
            quarantine_requested: false,
        }
    }

    fn into_resources<A: crate::Addin>(self) -> CloseResources<A> {
        CloseResources::Claimed(ClaimedClose {
            payload: ClosePayload::Empty,
            claim: RemovalClaimState::Outstanding(OutstandingModuleClaim {
                attempt: self.attempt,
                module_epoch: self.module_epoch,
            }),
        })
    }
}

/// Progressed or retained authority can be cleaned up or retained, but cannot
/// start another removal transaction.
pub(crate) struct UnavailableClose<A: crate::Addin> {
    payload: ClosePayload<A>,
    authority: UnavailableModuleAuthority,
}

impl<A: crate::Addin> AvailableClose<A> {
    fn claim(
        self,
        attempt: RemovalAttemptId,
    ) -> (ClaimedClose<A>, crate::module_runtime::ModuleClosing) {
        let Self { payload, authority } = self;
        let module_epoch = authority.id();
        let closing = authority.into_closing();
        (
            ClaimedClose {
                payload,
                claim: RemovalClaimState::Outstanding(OutstandingModuleClaim {
                    attempt,
                    module_epoch,
                }),
            },
            closing,
        )
    }
}

impl<A: crate::Addin> ClaimedClose<A> {
    fn release(self, expected_attempt: RemovalAttemptId) -> CloseResources<A> {
        let Self { payload, claim } = self;
        validate_removal_owner(claim.attempt(), expected_attempt);
        match claim {
            RemovalClaimState::Returned(ReturnedModuleClaim { authority, .. }) => {
                CloseResources::with_authority(payload, authority)
            }
            RemovalClaimState::Outstanding(_) => CloseResources::Unowned { payload },
        }
    }
}

/// Closing resources and their authority are one state space. Each variant
/// retains the concrete receiver that permits its next operation.
pub(crate) enum CloseResources<A: crate::Addin> {
    AwaitingOpenAbort {
        _attempt: OpenAttemptId,
        payload: OpenAbortPayload<A>,
    },
    Unowned {
        payload: ClosePayload<A>,
    },
    Available(AvailableClose<A>),
    Claimed(ClaimedClose<A>),
    Unavailable(UnavailableClose<A>),
}

/// Intermediate storage selection; absence becomes a qualified public outcome
/// only after canonical identity and the module ingress are checked together.
enum CleanupSelection {
    Ready(ModuleCleanupReady),
    RetainedUndrained(ModuleEpochId),
    None,
    DeferredOwner,
    AwaitingOpenAbort,
}

impl<A: crate::Addin> CloseResources<A> {
    /// No active opening attempt remains in this resource shape. An outstanding
    /// module abort or removal owner still defers cleanup acquisition.
    fn quarantine(self) -> LifecycleState<A> {
        LifecycleState::Quarantined(self)
    }

    fn with_authority(payload: ClosePayload<A>, authority: ModuleAuthority) -> Self {
        match authority.classify() {
            Ok(authority) => Self::Available(AvailableClose { payload, authority }),
            Err(authority) => Self::Unavailable(UnavailableClose { payload, authority }),
        }
    }

    fn payload(&self) -> Option<&ClosePayload<A>> {
        match self {
            Self::AwaitingOpenAbort { .. } => None,
            Self::Unowned { payload }
            | Self::Available(AvailableClose { payload, .. })
            | Self::Claimed(ClaimedClose { payload, .. })
            | Self::Unavailable(UnavailableClose { payload, .. }) => Some(payload),
        }
    }

    fn is_published(&self) -> bool {
        self.payload().is_some_and(ClosePayload::is_published)
    }

    #[cfg(any(test, feature = "refinement"))]
    fn has_opening_generation(&self) -> bool {
        match self {
            Self::AwaitingOpenAbort { payload, .. } => payload.has_opening_generation(),
            _ => self
                .payload()
                .is_some_and(ClosePayload::has_opening_generation),
        }
    }

    fn opening(&self) -> Option<&OpeningGeneration<A>> {
        match self {
            Self::AwaitingOpenAbort { payload, .. } => payload.opening(),
            _ => self.payload().and_then(ClosePayload::opening),
        }
    }

    fn retiring_services(
        &self,
    ) -> Option<
        &GenerationServices<
            crate::addin_impl::AddinSubscription<A>,
            crate::addin_impl::AddinSources<A>,
        >,
    > {
        self.payload().and_then(ClosePayload::retiring_services)
    }

    fn module_epoch_id(&self) -> Option<ModuleEpochId> {
        match self {
            Self::Available(available) => Some(available.authority.id()),
            Self::Claimed(claimed) => Some(claimed.claim.module_epoch()),
            Self::Unavailable(unavailable) => Some(unavailable.authority.id()),
            Self::AwaitingOpenAbort { .. } | Self::Unowned { .. } => None,
        }
    }

    fn removal_attempt(&self) -> Option<RemovalAttemptId> {
        match self {
            Self::Claimed(claimed) => Some(claimed.claim.attempt()),
            _ => None,
        }
    }

    fn take_cleanup(self) -> (CleanupSelection, Self) {
        match self {
            other @ Self::AwaitingOpenAbort { .. } => (CleanupSelection::AwaitingOpenAbort, other),
            other @ Self::Claimed(ClaimedClose {
                claim: RemovalClaimState::Outstanding(_),
                ..
            }) => (CleanupSelection::DeferredOwner, other),
            Self::Available(AvailableClose { payload, authority }) => (
                CleanupSelection::Ready(ModuleCleanupReady::Closing(authority.into_closing())),
                Self::Unowned { payload },
            ),
            Self::Unavailable(UnavailableClose {
                payload,
                authority: UnavailableModuleAuthority::Drained(drained),
            }) => (
                CleanupSelection::Ready(ModuleCleanupReady::Drained(drained)),
                Self::Unowned { payload },
            ),
            Self::Unavailable(UnavailableClose {
                payload,
                authority: UnavailableModuleAuthority::RetainedUndrained(retained),
            }) => (
                CleanupSelection::RetainedUndrained(retained.id()),
                Self::Unavailable(UnavailableClose {
                    payload,
                    authority: UnavailableModuleAuthority::RetainedUndrained(retained),
                }),
            ),
            Self::Claimed(ClaimedClose {
                payload,
                claim: RemovalClaimState::Returned(ReturnedModuleClaim { attempt, authority }),
            }) => {
                let module_epoch = authority.id();
                match authority.into_cleanup() {
                    Ok(authority) => (
                        CleanupSelection::Ready(authority),
                        Self::Claimed(ClaimedClose {
                            payload,
                            claim: RemovalClaimState::Outstanding(OutstandingModuleClaim {
                                attempt,
                                module_epoch,
                            }),
                        }),
                    ),
                    Err(retained) => (
                        CleanupSelection::RetainedUndrained(retained.id()),
                        Self::Claimed(ClaimedClose {
                            payload,
                            claim: RemovalClaimState::Returned(ReturnedModuleClaim {
                                attempt,
                                authority: ModuleAuthority::RetainedUndrained(retained),
                            }),
                        }),
                    ),
                }
            }
            other => (CleanupSelection::None, other),
        }
    }

    fn release(self, expected_attempt: RemovalAttemptId) -> Self {
        match self {
            Self::Claimed(claimed) => claimed.release(expected_attempt),
            _ => lifecycle_invariant_violation(
                "removal owner release does not match canonical lifecycle ownership",
            ),
        }
    }

    fn final_removal_ready(&self, expected_attempt: RemovalAttemptId) -> Option<FinalRemovalReady> {
        match self {
            Self::Claimed(ClaimedClose {
                payload: ClosePayload::Retiring(retirement),
                claim:
                    RemovalClaimState::Outstanding(OutstandingModuleClaim {
                        attempt,
                        module_epoch,
                    }),
            }) if *attempt == expected_attempt
                && retirement.services.is_none()
                && module_epoch.is_current() =>
            {
                Some(FinalRemovalReady::Committed {
                    generation: retirement.generation,
                    module_epoch: *module_epoch,
                })
            }
            Self::Claimed(ClaimedClose {
                payload: ClosePayload::Empty,
                claim: RemovalClaimState::Outstanding(OutstandingModuleClaim { attempt, .. }),
            }) if *attempt == expected_attempt => Some(FinalRemovalReady::Uncommitted),
            _ => None,
        }
    }

    fn open_rollback_ready(&self, expected_attempt: RemovalAttemptId) -> Option<OpenRollbackReady> {
        match self {
            Self::Claimed(ClaimedClose {
                payload: ClosePayload::Empty,
                claim: RemovalClaimState::Outstanding(OutstandingModuleClaim { attempt, .. }),
            }) if *attempt == expected_attempt => Some(OpenRollbackReady { _private: () }),
            _ => None,
        }
    }

    fn take_staged(&mut self) -> Option<OpeningGeneration<A>> {
        match self {
            Self::AwaitingOpenAbort { payload, .. } => payload.take_staged(),
            Self::Unowned { payload }
            | Self::Available(AvailableClose { payload, .. })
            | Self::Claimed(ClaimedClose { payload, .. })
            | Self::Unavailable(UnavailableClose { payload, .. }) => payload.take_staged(),
        }
    }

    fn take_retirement(&mut self) -> Option<OpenRetirement<A>> {
        match self {
            Self::AwaitingOpenAbort { .. } => None,
            Self::Unowned { payload }
            | Self::Available(AvailableClose { payload, .. })
            | Self::Claimed(ClaimedClose { payload, .. })
            | Self::Unavailable(UnavailableClose { payload, .. }) => payload.take_retirement(),
        }
    }

    fn into_retiring(self) -> (Option<PublishedOwner<ExecutionGeneration<A>>>, Self) {
        let mut generation = None;
        let mut retire = |payload| match payload {
            ClosePayload::Published(closing) => {
                let (root, retirement) = closing.into_retirement_with_generation();
                generation = Some(root);
                ClosePayload::Retiring(retirement)
            }
            other => other,
        };
        let resources = match self {
            other @ Self::AwaitingOpenAbort { .. } => other,
            Self::Unowned { payload } => Self::Unowned {
                payload: retire(payload),
            },
            Self::Available(AvailableClose { payload, authority }) => {
                Self::Available(AvailableClose {
                    payload: retire(payload),
                    authority,
                })
            }
            Self::Claimed(ClaimedClose { payload, claim }) => Self::Claimed(ClaimedClose {
                payload: retire(payload),
                claim,
            }),
            Self::Unavailable(UnavailableClose { payload, authority }) => {
                Self::Unavailable(UnavailableClose {
                    payload: retire(payload),
                    authority,
                })
            }
        };
        (generation, resources)
    }
}

/// The close payload remains opening-active until the open attempt is either
/// committed or explicitly failed. Once resolved, all authority transitions
/// use `CloseResources` directly.
pub(crate) enum ClosingState<A: crate::Addin> {
    OpeningActive {
        attempt: OpenAttemptId,
        quarantine_requested: bool,
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
        authority: ClaimableModuleAuthority,
    },
}

impl<A: crate::Addin> ActiveOpeningClose<A> {
    fn resolve(self, attempt: OpenAttemptId) -> CloseResources<A> {
        match self {
            Self::AwaitingAbort { payload } => CloseResources::AwaitingOpenAbort {
                _attempt: attempt,
                payload,
            },
            Self::Published { payload, authority } => CloseResources::Available(AvailableClose {
                payload: payload.into_close_payload(),
                authority,
            }),
        }
    }
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

    #[cfg(any(test, feature = "refinement"))]
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

    fn retiring_services(
        &self,
    ) -> Option<
        &GenerationServices<
            crate::addin_impl::AddinSubscription<A>,
            crate::addin_impl::AddinSources<A>,
        >,
    > {
        match self {
            Self::OpeningActive {
                resources: ActiveOpeningClose::Published { payload, .. },
                ..
            } => Some(payload.retiring_services()),
            Self::OpeningActive { .. } => None,
            Self::Ready { resources } => resources.retiring_services(),
        }
    }

    fn resolve_open_failure(self) -> LifecycleState<A> {
        match self {
            Self::OpeningActive {
                attempt,
                resources,
                quarantine_requested,
            } => {
                let resources = resources.resolve(attempt);
                if quarantine_requested {
                    resources.quarantine()
                } else {
                    LifecycleState::Closing(Self::Ready { resources })
                }
            }
            Self::Ready { resources } => LifecycleState::Closing(Self::Ready { resources }),
        }
    }

    fn take_cleanup(self) -> (CleanupSelection, Self) {
        match self {
            Self::OpeningActive { .. } => (CleanupSelection::None, self),
            Self::Ready { resources } => {
                let (authority, resources) = resources.take_cleanup();
                (authority, Self::Ready { resources })
            }
        }
    }

    fn request_quarantine(self) -> LifecycleState<A> {
        match self {
            Self::OpeningActive {
                attempt, resources, ..
            } => LifecycleState::Closing(Self::OpeningActive {
                attempt,
                resources,
                quarantine_requested: true,
            }),
            Self::Ready { resources } => resources.quarantine(),
        }
    }

    fn into_retiring(self) -> (Option<PublishedOwner<ExecutionGeneration<A>>>, Self) {
        match self {
            Self::OpeningActive {
                attempt,
                quarantine_requested,
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
                        quarantine_requested,
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

    fn take_retirement(&mut self) -> Option<OpenRetirement<A>> {
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
                resources: CloseResources::Available(AvailableClose { payload, authority }),
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

    #[cfg(any(test, feature = "refinement"))]
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

    fn generation_services(
        &self,
    ) -> Option<
        &GenerationServices<
            crate::addin_impl::AddinSubscription<A>,
            crate::addin_impl::AddinSources<A>,
        >,
    > {
        match self {
            Self::Opening {
                payload: OpeningPayload::Published(bundle),
                ..
            }
            | Self::Open { bundle } => Some(bundle.services.as_ref()),
            Self::Closing(closing) => closing.retiring_services(),
            Self::OpenRollbackPending(resources) | Self::Quarantined(resources) => {
                resources.retiring_services()
            }
            Self::Closed(_) | Self::Opening { .. } => None,
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
            Self::Closed(ClosedState::Finalizing { attempt, .. }) => Some(*attempt),
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

    fn take_retirement(&mut self) -> Option<OpenRetirement<A>> {
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
    next_removal_attempt: RemovalAttemptId,
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
            next_removal_attempt: RemovalAttemptId::ONE,
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

    fn opening_config(&self) -> Option<crate::addin_impl::RuntimeConfig> {
        self.state.opening().map(|opening| opening.init_config)
    }

    fn has_current_generation(&self) -> bool {
        self.state.has_current_generation()
    }

    fn generation_services(
        &self,
    ) -> Option<
        &GenerationServices<
            crate::addin_impl::AddinSubscription<A>,
            crate::addin_impl::AddinSources<A>,
        >,
    > {
        self.state.generation_services()
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

/// One closed, completed removal intent held under its original lifecycle
/// lock. The residency epilogue cannot clear an arbitrary lifecycle's intent.
pub(crate) struct CompletedRemovalIntent<'coordinator, A: crate::Addin> {
    access: LifecycleAccess<'coordinator, A>,
}

impl<A: crate::Addin> CompletedRemovalIntent<'_, A> {
    pub(crate) fn clear(mut self) {
        self.access.transition(|core| {
            core.host_intent = HostLifecycleIntent::None;
            ((), TransitionEffect::Keep)
        });
    }
}

/// Successful canonical open commit while the exact mutation guard remains
/// borrowed. Only the commit operation can issue this observation authority.
pub(crate) struct CommittedOpen<'access, 'coordinator, A: crate::Addin> {
    _access: &'access LifecycleAccess<'coordinator, A>,
    attempt: OpenAttemptId,
}

impl<A: crate::Addin> CommittedOpen<'_, '_, A> {
    pub(crate) fn attempt(&self) -> OpenAttemptId {
        self.attempt
    }

    pub(crate) fn access(&self) -> &LifecycleAccess<'_, A> {
        self._access
    }
}

impl<'coordinator, A: crate::Addin> LifecycleAccess<'coordinator, A> {
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

    #[cfg(test)]
    pub(crate) fn module_epoch_id(&self) -> Option<ModuleEpochId> {
        self.canonical_state().module_epoch_id()
    }

    pub(crate) fn removal_epoch(&self) -> u64 {
        self.core.removal_epoch()
    }

    /// Monotonic identity boundary for residency held across an opening call.
    pub(crate) fn next_open_sequence(&self) -> u64 {
        self.core.next_lifecycle_attempt
    }

    /// Classify once while locked, then retain that exact guard through the
    /// physical release and its consuming intent epilogue.
    pub(crate) fn into_completed_removal_intent(
        self,
    ) -> Result<CompletedRemovalIntent<'coordinator, A>, Self> {
        if matches!(
            self.canonical_state(),
            LifecycleState::Closed(ClosedState::Idle)
        ) && self.host_intent() == HostLifecycleIntent::ExplicitRemovalComplete
        {
            Ok(CompletedRemovalIntent { access: self })
        } else {
            Err(self)
        }
    }

    pub(crate) fn matches_latest_open_attempt(&self, generation: RuntimeGeneration) -> bool {
        generation.get().checked_add(1) == Some(self.core.next_lifecycle_attempt)
    }

    pub(crate) fn open_attempt(&self) -> Option<OpenAttemptId> {
        self.canonical_state().open_attempt()
    }

    /// Observation metadata for the first irreversible quarantine request.
    /// This does not authorize cleanup or resolve the active opening owner.
    #[cfg(any(test, feature = "refinement"))]
    pub(crate) fn unobserved_quarantine_open_attempt(&self) -> Option<OpenAttemptId> {
        match self.canonical_state() {
            LifecycleState::Opening { attempt, .. }
            | LifecycleState::Closing(ClosingState::OpeningActive {
                attempt,
                quarantine_requested: false,
                ..
            }) => Some(*attempt),
            // Failure already relinquished the opening phase, but its owner
            // still owes the module authority. This is diagnostic identity,
            // never a claim or a permission to reclaim the pending payload.
            LifecycleState::OpenRollbackPending(CloseResources::AwaitingOpenAbort {
                _attempt,
                ..
            }) => Some(*_attempt),
            _ => None,
        }
    }

    pub(crate) fn removal_attempt(&self) -> Option<RemovalAttemptId> {
        self.core.removal_attempt()
    }

    pub(crate) fn opening_config(&self) -> Option<crate::addin_impl::RuntimeConfig> {
        self.core.opening_config()
    }

    #[cfg(any(test, feature = "refinement"))]
    pub(crate) fn has_current_generation(&self) -> bool {
        self.core.has_current_generation()
    }

    #[cfg(any(test, feature = "refinement"))]
    pub(crate) fn has_opening_generation(&self) -> bool {
        self.core.state.has_opening_generation()
    }

    pub(crate) fn generation_services(
        &self,
    ) -> Option<
        &GenerationServices<
            crate::addin_impl::AddinSubscription<A>,
            crate::addin_impl::AddinSources<A>,
        >,
    > {
        self.core.generation_services()
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
    publication: AtomicPtr<PublishedGeneration<A>>,
    publication_readers: StripedDrainGate<DEFAULT_STRIPE_COUNT>,
    core: Mutex<LifecycleCore<A>>,
    changed: Condvar,
    #[cfg(test)]
    pub(crate) test_module_lease: Mutex<Option<crate::ingress::TestModuleLease>>,
}

pub(crate) struct PublishOpeningError<A: crate::Addin> {
    pub(crate) error: crate::XllError,
    pub(crate) opening: Option<OpeningGeneration<A>>,
    pub(crate) services: Box<
        GenerationServices<
            crate::addin_impl::AddinSubscription<A>,
            crate::addin_impl::AddinSources<A>,
        >,
    >,
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
    Publish(NonNull<PublishedGeneration<A>>),
}

/// Classified together with the canonical mutation, never reconstructed from
/// the resulting broad lifecycle enum.
enum CloseRequestDisposition {
    ClosedIdle,
    Finalizing,
    Closing,
    Quarantined,
}

impl<A: crate::Addin> LifecycleCoordinator<A> {
    pub(crate) const fn new() -> Self {
        Self {
            phase: AtomicU8::new(LifecyclePhase::Closed as u8),
            publication: AtomicPtr::new(std::ptr::null_mut()),
            publication_readers: StripedDrainGate::new_sealed(),
            core: Mutex::new(LifecycleCore::new()),
            changed: Condvar::new(),
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

    pub(in crate::lifecycle) fn take_module_cleanup_for_quarantine(&self) -> ModuleCleanupClaim {
        let mut access = self.access();
        access.transition(|core| {
            let state = mem::replace(&mut core.state, LifecycleState::Closed(ClosedState::Idle));
            let (authority, state) = match state {
                state @ LifecycleState::Closing(ClosingState::OpeningActive {
                    quarantine_requested: true,
                    ..
                })
                | state @ LifecycleState::Closed(ClosedState::Finalizing {
                    quarantine_requested: true,
                    ..
                }) => (CleanupSelection::DeferredOwner, state),
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
                other => (CleanupSelection::None, other),
            };
            let authority = match authority {
                CleanupSelection::Ready(authority) => ModuleCleanupClaim::Ready(authority),
                CleanupSelection::RetainedUndrained(epoch) => {
                    ModuleCleanupClaim::RetainedUndrained(epoch)
                }
                CleanupSelection::DeferredOwner => ModuleCleanupClaim::DeferredOwner,
                CleanupSelection::AwaitingOpenAbort
                    if crate::module_runtime::ingress().phase() != crate::ingress::PHASE_CLOSED =>
                {
                    ModuleCleanupClaim::DeferredOwner
                }
                CleanupSelection::AwaitingOpenAbort => ModuleCleanupClaim::NoModuleAuthority,
                CleanupSelection::None
                    if state.module_epoch_id().is_none()
                        && crate::module_runtime::ingress().phase()
                            == crate::ingress::PHASE_CLOSED =>
                {
                    ModuleCleanupClaim::NoModuleAuthority
                }
                CleanupSelection::None => ModuleCleanupClaim::Unavailable,
            };
            core.state = state;
            (authority, TransitionEffect::Keep)
        })
    }

    pub(in crate::lifecycle) fn retain_failed_module_cleanup(
        &self,
        authority: ModuleCleanupAuthority,
    ) -> Result<(), ModuleAuthority> {
        let mut access = self.access();
        self.install_returned_authority(&mut access, authority.into_authority())
    }

    pub(in crate::lifecycle) fn complete_open_abort_locked(
        &self,
        access: &mut LifecycleAccess<'_, A>,
        closing: crate::module_runtime::ModuleClosing,
    ) -> Result<(), ModuleAuthority> {
        self.install_returned_authority(access, ModuleAuthority::Closing(closing))
    }

    fn install_returned_authority(
        &self,
        access: &mut LifecycleAccess<'_, A>,
        authority: ModuleAuthority,
    ) -> Result<(), ModuleAuthority> {
        if !std::ptr::eq(access.coordinator, self) {
            return Err(authority);
        }
        access.transition(|core| {
            // If future code unwinds after detaching the protected receiver,
            // the canonical fallback must not permit a new opening attempt.
            let state = mem::replace(
                &mut core.state,
                LifecycleState::Quarantined(CloseResources::Unowned {
                    payload: ClosePayload::Empty,
                }),
            );
            match PreparedAuthorityInstall::acquire(state, authority) {
                Ok(installation) => {
                    core.state = installation.install();
                    (Ok(()), TransitionEffect::Keep)
                }
                Err((state, authority)) => {
                    core.state = state;
                    (Err(authority), TransitionEffect::Keep)
                }
            }
        })
    }

    pub(in crate::lifecycle) fn clear_certified_retirement(
        &self,
        access: &mut LifecycleAccess<'_, A>,
        authorization: crate::runtime::CertifiedRetirement<'_, A>,
    ) -> bool {
        // Neither a stale ready observation nor another coordinator's mutex
        // guard can authorize disposal of these canonical resources.
        if !std::ptr::eq(access.coordinator, self) {
            return false;
        }
        access.transition(|core| {
            let Some(FinalRemovalReady::Committed {
                generation,
                module_epoch,
            }) = core.final_removal_ready(authorization.attempt())
            else {
                return (false, TransitionEffect::Keep);
            };
            if !authorization.matches(self, authorization.attempt(), generation, module_epoch) {
                return (false, TransitionEffect::Keep);
            }
            let Some(retirement) = core.state.take_retirement() else {
                return (false, TransitionEffect::Keep);
            };
            drop(retirement.services);
            (true, TransitionEffect::ClearPublication)
        })
    }

    fn wait<'a>(&self, access: &mut LifecycleAccess<'a, A>) {
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
    /// Opening has no publication and closing seals admission before the
    /// lifecycle phase changes. The permit protects the uniquely owned
    /// generation bundle without participating in its ownership.
    pub(crate) fn try_admit(&self) -> crate::XllResult<GenerationAdmission<'_, A>> {
        let permit = self
            .publication_readers
            .try_enter_current()
            .map_err(|_| crate::XllError::Closing)?;
        let Some(publication) = NonNull::new(self.publication.load(Ordering::Acquire)) else {
            drop(permit);
            return Err(crate::XllError::Closing);
        };
        Ok(GenerationAdmission::new(publication, permit))
    }

    /// Async admission records its process-lifetime issuer before returning it.
    /// There is no API accepting an independently supplied generation pointer.
    #[cfg(feature = "async")]
    pub(crate) fn try_admit_async(
        &'static self,
    ) -> crate::XllResult<GenerationAdmission<'static, A>> {
        let mut admission = self.try_admit()?;
        admission.execution_issuer = Some(self);
        Ok(admission)
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
            TransitionEffect::ClearPublication => {
                drop(self.publication_readers.control().seal());
                self.publication
                    .store(std::ptr::null_mut(), Ordering::Release);
            }
            TransitionEffect::Publish(publication) => {
                let control = self.publication_readers.control();
                require_lifecycle_invariant(
                    self.publication_readers.active() == 0
                        && self.publication.load(Ordering::Acquire).is_null(),
                    "generation publication requires a drained closed projection",
                );
                // Classify the actual gate once, before exposing the pointer.
                // Keep that same exclusive drained authority through reopen.
                let drained = control.wait_if_sealed().unwrap_or_else(|_| {
                    lifecycle_invariant_violation(
                        "generation publication requires sealed admission",
                    )
                });
                self.publication
                    .store(publication.as_ptr(), Ordering::Release);
                drop(drained.reopen());
            }
        }
        self.phase.store(access.phase() as u8, Ordering::Release);
        self.changed.notify_all();
    }

    fn publish_effect(bundle: &OpenGeneration<A>) -> TransitionEffect<A> {
        TransitionEffect::Publish(NonNull::from(bundle.publication.as_ref()))
    }

    /// One locked acquisition: classify ClosedIdle, allocate an attempt, and
    /// install Opening together. Failed acquisition leaves host intent intact.
    fn begin_opening_if_closed(
        &self,
        access: &mut LifecycleAccess<'_, A>,
        expected_removal_epoch: crate::generation::RemovalEpoch,
    ) -> crate::XllResult<OpenAttemptId> {
        access.transition(|core| {
            match (
                &core.state,
                core.removal_epoch == expected_removal_epoch.get(),
            ) {
                (LifecycleState::Closed(ClosedState::Idle), true) => {}
                _ => {
                    return (
                        Err(crate::XllError::Internal {
                            diagnostic_id: crate::diagnostics::id::DiagnosticId::OPEN_PHASE,
                        }),
                        TransitionEffect::Keep,
                    );
                }
            }
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
            core.host_intent = HostLifecycleIntent::None;
            core.state = LifecycleState::Opening {
                attempt,
                payload: OpeningPayload::Empty,
            };
            (Ok(attempt), TransitionEffect::Keep)
        })
    }

    /// Publishes a successfully assembled generation while retaining the
    /// opening attempt until `commit_open` completes the lifecycle transition.
    pub(in crate::lifecycle) fn commit_open<'access, 'coordinator>(
        &self,
        core: &'access mut LifecycleAccess<'coordinator, A>,
        generation: RuntimeGeneration,
    ) -> crate::XllResult<CommittedOpen<'access, 'coordinator, A>> {
        let attempt = core.transition(|core| {
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
                    let effect = Self::publish_effect(&bundle);
                    core.state = LifecycleState::Open { bundle };
                    (Ok(attempt), effect)
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
        })?;
        Ok(CommittedOpen {
            _access: core,
            attempt,
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
                    closing.resolve_open_failure(),
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
    fn request_closing(&self, core: &mut LifecycleAccess<'_, A>) -> CloseRequestDisposition {
        core.transition(|core| {
            let state = mem::replace(&mut core.state, LifecycleState::Closed(ClosedState::Idle));
            let (next, disposition, effect) = match state {
                LifecycleState::Closed(ClosedState::Idle) => (
                    LifecycleState::Closed(ClosedState::Idle),
                    CloseRequestDisposition::ClosedIdle,
                    TransitionEffect::Keep,
                ),
                LifecycleState::Opening { attempt, payload } => (
                    LifecycleState::Closing(payload.into_closing_state(attempt)),
                    CloseRequestDisposition::Closing,
                    TransitionEffect::ClearPublication,
                ),
                LifecycleState::Open { bundle } => {
                    let (closing, authority) = bundle.into_closing();
                    (
                        LifecycleState::Closing(ClosingState::Ready {
                            resources: CloseResources::Available(AvailableClose {
                                payload: ClosePayload::Published(closing),
                                authority,
                            }),
                        }),
                        CloseRequestDisposition::Closing,
                        TransitionEffect::ClearPublication,
                    )
                }
                LifecycleState::Closing(closing) => (
                    LifecycleState::Closing(closing),
                    CloseRequestDisposition::Closing,
                    TransitionEffect::Keep,
                ),
                LifecycleState::OpenRollbackPending(resources) => (
                    LifecycleState::Closing(ClosingState::Ready { resources }),
                    CloseRequestDisposition::Closing,
                    TransitionEffect::ClearPublication,
                ),
                LifecycleState::Quarantined(resources) => (
                    LifecycleState::Quarantined(resources),
                    CloseRequestDisposition::Quarantined,
                    TransitionEffect::Keep,
                ),
                LifecycleState::Closed(finalizing @ ClosedState::Finalizing { .. }) => (
                    LifecycleState::Closed(finalizing),
                    CloseRequestDisposition::Finalizing,
                    TransitionEffect::Keep,
                ),
            };
            core.state = next;
            (disposition, effect)
        })
    }

    fn begin_removal_request(&self, core: &mut LifecycleAccess<'_, A>) {
        self.advance_removal_epoch(core);
    }

    fn claim_removal(&self, core: &mut LifecycleAccess<'_, A>) -> Option<RemovalClaim> {
        core.transition(|core| {
            let state = mem::replace(&mut core.state, LifecycleState::Closed(ClosedState::Idle));
            let (claim, state) = match state {
                LifecycleState::Closing(ClosingState::Ready {
                    resources: CloseResources::Available(available),
                }) => {
                    let attempt = core.next_removal_attempt;
                    core.next_removal_attempt =
                        core.next_removal_attempt.checked_next().unwrap_or_else(|| {
                            lifecycle_invariant_violation(
                                "lifecycle removal-attempt identity exhausted",
                            )
                        });
                    let (resources, module_closing) = available.claim(attempt);
                    (
                        Some(RemovalClaim {
                            attempt,
                            module_closing,
                        }),
                        LifecycleState::Closing(ClosingState::Ready {
                            resources: CloseResources::Claimed(resources),
                        }),
                    )
                }
                LifecycleState::OpenRollbackPending(CloseResources::Available(available)) => {
                    let attempt = core.next_removal_attempt;
                    core.next_removal_attempt =
                        core.next_removal_attempt.checked_next().unwrap_or_else(|| {
                            lifecycle_invariant_violation(
                                "lifecycle removal-attempt identity exhausted",
                            )
                        });
                    let (resources, module_closing) = available.claim(attempt);
                    (
                        Some(RemovalClaim {
                            attempt,
                            module_closing,
                        }),
                        LifecycleState::OpenRollbackPending(CloseResources::Claimed(resources)),
                    )
                }
                other => (None, other),
            };
            core.state = state;
            (claim, TransitionEffect::Keep)
        })
    }

    pub(in crate::lifecycle) fn return_removal_authority(
        &self,
        access: &mut LifecycleAccess<'_, A>,
        expected_attempt: RemovalAttemptId,
        authority: ModuleAuthority,
    ) {
        access.transition(|core| {
            // Shared state can change through quarantine. Acquiring this slot
            // validates the owner; only the outstanding receiver can return.
            let resources = match &mut core.state {
                LifecycleState::Closing(ClosingState::Ready { resources })
                | LifecycleState::OpenRollbackPending(resources)
                | LifecycleState::Quarantined(resources) => resources,
                _ => {
                    lifecycle_invariant_violation("module return lacks its canonical removal claim")
                }
            };
            let old = mem::replace(
                resources,
                CloseResources::Unowned {
                    payload: ClosePayload::Empty,
                },
            );
            *resources = match old {
                CloseResources::Claimed(ClaimedClose {
                    payload,
                    claim: RemovalClaimState::Outstanding(claim),
                }) => {
                    validate_removal_owner(claim.attempt, expected_attempt);
                    CloseResources::Claimed(ClaimedClose {
                        payload,
                        claim: RemovalClaimState::Returned(claim.return_authority(authority)),
                    })
                }
                _ => lifecycle_invariant_violation(
                    "module return does not own an outstanding canonical claim",
                ),
            };
            ((), TransitionEffect::Keep)
        });
    }

    pub(in crate::lifecycle) fn release_removal_claim(
        &self,
        core: &mut LifecycleAccess<'_, A>,
        attempt: RemovalAttemptId,
        finalization_failed: bool,
    ) {
        core.transition(|core| {
            let state = mem::replace(&mut core.state, LifecycleState::Closed(ClosedState::Idle));
            core.state = match state {
                LifecycleState::Closed(ClosedState::Finalizing {
                    attempt: expected,
                    quarantine_requested,
                }) => {
                    require_lifecycle_invariant(
                        expected == attempt,
                        "removal owner identity does not match the finalizing lifecycle owner",
                    );
                    // Commit terminal failure and claim return atomically: no
                    // unclaimed Closed state is observable on release failure.
                    if finalization_failed || quarantine_requested {
                        LifecycleState::Quarantined(CloseResources::Unowned {
                            payload: ClosePayload::Empty,
                        })
                    } else {
                        LifecycleState::Closed(ClosedState::Idle)
                    }
                }
                LifecycleState::Closing(ClosingState::Ready { resources }) => {
                    let resources = resources.release(attempt);
                    if finalization_failed {
                        LifecycleState::Quarantined(resources)
                    } else {
                        LifecycleState::Closing(ClosingState::Ready { resources })
                    }
                }
                LifecycleState::OpenRollbackPending(resources) => {
                    let resources = resources.release(attempt);
                    if finalization_failed {
                        LifecycleState::Quarantined(resources)
                    } else {
                        LifecycleState::OpenRollbackPending(resources)
                    }
                }
                LifecycleState::Quarantined(resources) => {
                    LifecycleState::Quarantined(resources.release(attempt))
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
        authorization: crate::runtime::CanonicalCompletion<'_, A, crate::runtime::FinalRemoval>,
    ) -> crate::XllResult<()> {
        self.finish_closed(
            core,
            authorization,
            false,
            crate::diagnostics::id::DiagnosticId::CLOSE_RUNTIME,
        )
    }

    pub(in crate::lifecycle) fn finish_open_rollback(
        &self,
        core: &mut LifecycleAccess<'_, A>,
        authorization: crate::runtime::CanonicalCompletion<'_, A, crate::runtime::OpenRollback>,
    ) -> crate::XllResult<()> {
        self.finish_closed(
            core,
            authorization,
            true,
            crate::diagnostics::id::DiagnosticId::OPEN_ROLLBACK_PHASE,
        )
    }

    fn finish_closed<K>(
        &self,
        core: &mut LifecycleAccess<'_, A>,
        authorization: crate::runtime::CanonicalCompletion<'_, A, K>,
        allow_open_rollback: bool,
        diagnostic_id: crate::diagnostics::id::DiagnosticId,
    ) -> crate::XllResult<()> {
        if !std::ptr::eq(core.coordinator, self) {
            return Err(crate::XllError::Internal { diagnostic_id });
        }
        core.transition(|core| {
            let state = mem::replace(&mut core.state, LifecycleState::Closed(ClosedState::Idle));
            // This is a dynamic acquisition after callbacks and unlocked work.
            // Once acquired, no broad payload is handed to terminalization.
            let (empty, open_rollback_state) = match state {
                LifecycleState::Closing(ClosingState::Ready {
                    resources:
                        CloseResources::Claimed(ClaimedClose {
                            payload: ClosePayload::Empty,
                            claim:
                                RemovalClaimState::Outstanding(OutstandingModuleClaim {
                                    attempt,
                                    module_epoch,
                                }),
                        }),
                }) => (
                    EmptyClaimedClose {
                        attempt,
                        module_epoch,
                    },
                    false,
                ),
                LifecycleState::OpenRollbackPending(CloseResources::Claimed(ClaimedClose {
                    payload: ClosePayload::Empty,
                    claim:
                        RemovalClaimState::Outstanding(OutstandingModuleClaim {
                            attempt,
                            module_epoch,
                        }),
                })) if allow_open_rollback => (
                    EmptyClaimedClose {
                        attempt,
                        module_epoch,
                    },
                    true,
                ),
                other => {
                    core.state = other;
                    return (
                        Err(crate::XllError::Internal { diagnostic_id }),
                        TransitionEffect::Keep,
                    );
                }
            };
            if !authorization.matches(
                self,
                Some(empty.attempt),
                Some(empty.module_epoch),
                core.last_committed_generation(),
            ) {
                let resources = empty.into_resources();
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
            core.state = LifecycleState::Closed(empty.finish_closed());
            (Ok(()), TransitionEffect::ClearPublication)
        })
    }

    pub(in crate::lifecycle) fn quarantine_core(&self, core: &mut LifecycleAccess<'_, A>) {
        core.transition(|core| {
            let state = mem::replace(&mut core.state, LifecycleState::Closed(ClosedState::Idle));
            let (state, effect) = match state {
                LifecycleState::Closed(ClosedState::Idle) => (
                    CloseResources::Unowned {
                        payload: ClosePayload::Empty,
                    }
                    .quarantine(),
                    TransitionEffect::ClearPublication,
                ),
                LifecycleState::Closed(ClosedState::Finalizing { attempt, .. }) => (
                    LifecycleState::Closed(ClosedState::Finalizing {
                        attempt,
                        quarantine_requested: true,
                    }),
                    TransitionEffect::Keep,
                ),
                LifecycleState::Opening { attempt, payload } => (
                    payload.into_closing_state(attempt).request_quarantine(),
                    TransitionEffect::ClearPublication,
                ),
                LifecycleState::Open { bundle } => {
                    let (closing, authority) = bundle.into_closing();
                    (
                        CloseResources::Available(AvailableClose {
                            payload: ClosePayload::Published(closing),
                            authority,
                        })
                        .quarantine(),
                        TransitionEffect::ClearPublication,
                    )
                }
                LifecycleState::Closing(closing) => (
                    closing.request_quarantine(),
                    TransitionEffect::ClearPublication,
                ),
                LifecycleState::OpenRollbackPending(resources)
                | LifecycleState::Quarantined(resources) => {
                    (resources.quarantine(), TransitionEffect::ClearPublication)
                }
            };
            core.state = state;
            ((), effect)
        });
    }

    pub(in crate::lifecycle) fn complete_boundary_finalization(
        &self,
        access: &LifecycleAccess<'_, A>,
        attempt: RemovalAttemptId,
    ) -> crate::XllResult<()> {
        if std::ptr::eq(access.coordinator, self)
            && matches!(access.core.state, LifecycleState::Closed(ClosedState::Finalizing {
                attempt: expected, quarantine_requested: false,
            }) if expected == attempt)
        {
            Ok(())
        } else {
            Err(crate::XllError::Closing)
        }
    }

    #[allow(
        clippy::result_large_err,
        reason = "Rollback returns the complete affine generation, including RTD limits, without a new allocation"
    )]
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
        services: Box<
            GenerationServices<
                crate::addin_impl::AddinSubscription<A>,
                crate::addin_impl::AddinSources<A>,
            >,
        >,
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
                        services,
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
                        services,
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
                        services,
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
            let generation = PublishedOwner::new(ExecutionGeneration {
                id: generation,
                shared_state,
                layers,
            });
            let services = PublishedOwner::from_box(services);
            let publication = PublishedOwner::new(PublishedGeneration::new(
                generation.as_ref(),
                services.as_ref(),
            ));
            let bundle = OpenGeneration {
                generation,
                services,
                publication,
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

    /// Borrows generation services while either a read admission or the
    /// canonical lifecycle lock proves the unique owner cannot move.
    pub(crate) fn with_generation_services<R>(
        &self,
        operation: impl FnOnce(
            &GenerationServices<
                crate::addin_impl::AddinSubscription<A>,
                crate::addin_impl::AddinSources<A>,
            >,
        ) -> R,
    ) -> Option<R> {
        if let Ok(admission) = self.try_admit() {
            return Some(operation(admission.services()));
        }
        let access = self.access();
        access.generation_services().map(operation)
    }

    pub(in crate::lifecycle) fn take_opening_for_rollback(&self) -> Option<OpeningGeneration<A>> {
        let mut access = self.access();
        access.transition(|core| (core.state.take_opening(), TransitionEffect::Keep))
    }

    fn take_current_bundle(
        &self,
        core: &mut LifecycleAccess<'_, A>,
    ) -> Option<Box<ExecutionGeneration<A>>> {
        let generation = core.transition(|core| {
            let state = mem::replace(&mut core.state, LifecycleState::Closed(ClosedState::Idle));
            match state {
                LifecycleState::Open { bundle } => {
                    let (closing, authority) = bundle.into_closing();
                    let (generation, retirement) = closing.into_retirement_with_generation();
                    core.state = LifecycleState::Closing(ClosingState::Ready {
                        resources: CloseResources::Available(AvailableClose {
                            payload: ClosePayload::Retiring(retirement),
                            authority,
                        }),
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
        });
        if generation.is_some() {
            drop(self.publication_readers.control().wait_idle());
        }
        // Only restore Box's exclusive ownership after every raw publication
        // lease has drained. Moving a Box earlier would invalidate those
        // aliases even though the heap allocation stays at the same address.
        generation.map(PublishedOwner::into_box)
    }

    #[cfg(test)]
    pub(crate) fn take_current_generation(&self) -> Option<Box<ExecutionGeneration<A>>> {
        let mut core = self.access();
        self.take_current_bundle(&mut core)
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

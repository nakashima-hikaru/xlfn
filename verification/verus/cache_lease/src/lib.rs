//! Verus formal verification of CacheLease and Temporal Reclamation protocol.
//!
//! Refines the formal TemporalReclamation protocol from `formal/XlFnFormal/TemporalReclamation/`
//! for `crates/xlfn/src/cache.rs`'s `CacheLease` and `CacheNode`.
//! Proves theorems [TR-OBSERVE-1], [TR-LEASE-1], [TR-ADMISSION-1], [TR-RECLAIM-1], and [TR-NO-UAF].
//! All proofs ensure zero verification failures, zero assumes, and zero runtime overhead.

use vstd::prelude::*;

#[path = "../../../../crates/xlfn/src/cache/pin_transitions.rs"]
pub mod pin_transitions;
pub mod pin_ownership;

verus! {

// ============================================================================
// Abstract Lifecycle Status
// ============================================================================

pub enum ObjectStatus {
    Unpublished,
    Published,
    Retired,
    Reclaimed,
}

pub struct CacheTemporalState {
    pub status: ObjectStatus,
    pub admissions: nat,
    pub observing: nat,
    pub pins: nat,
}

/// Fundamental System Invariant for Temporal Reclamation:
/// - [TR-OBSERVE-1]: Pointer observation implies object is published or retired (not reclaimed or unpublished).
/// - [TR-LEASE-1]: Any creator/resident/flight/lease pin implies the allocation is not reclaimed.
/// - [TR-ADMISSION-1]: Pointer observations need admission coverage; one admission may cover multiple observations.
/// - [TR-RECLAIM-1]: Reclaimed status strictly requires zero admissions, zero observers, and zero pins.
/// - [TR-PUBLISH-1]: Unpublished status requires zero index observers; creator/flight/lease pins are allowed.
pub open spec fn cache_temporal_inv(s: CacheTemporalState) -> bool {
    &&& (s.observing > 0 ==> (s.status is Published || s.status is Retired))
    &&& (s.pins > 0 ==> !(s.status is Reclaimed))
    &&& (s.observing > 0 ==> s.admissions > 0)
    &&& (s.status is Reclaimed ==> (s.admissions == 0 && s.observing == 0 && s.pins == 0))
    &&& (s.status is Unpublished ==> s.observing == 0)
}

// ============================================================================
// Abstract Transitions
// ============================================================================

pub open spec fn initial_state() -> CacheTemporalState {
    CacheTemporalState {
        status: ObjectStatus::Unpublished,
        admissions: 0,
        observing: 0,
        pins: 1,
    }
}

/// Node allocation and index publication.
pub open spec fn step_publish(s: CacheTemporalState) -> Option<CacheTemporalState> {
    if s.status is Unpublished && s.pins > 0 {
        Some(CacheTemporalState {
            status: ObjectStatus::Published,
            ..s
        })
    } else {
        None
    }
}

/// Reader enters lookup domain (acquires StripedDrainGate permit).
pub open spec fn step_enter_lookup(s: CacheTemporalState) -> Option<CacheTemporalState> {
    if s.status is Reclaimed {
        None
    } else {
        Some(CacheTemporalState {
            admissions: (s.admissions + 1) as nat,
            ..s
        })
    }
}

/// Reader observes raw pointer while holding lookup admission.
pub open spec fn step_observe_pointer(s: CacheTemporalState) -> Option<CacheTemporalState> {
    if (s.status is Published || s.status is Retired) && s.admissions > 0 {
        Some(CacheTemporalState {
            observing: (s.observing + 1) as nat,
            ..s
        })
    } else {
        None
    }
}

/// Reader converts observation into an active CacheLease pin (try_acquire_pin).
pub open spec fn step_acquire_pin(s: CacheTemporalState) -> Option<CacheTemporalState> {
    if s.observing > 0 && s.pins > 0 {
        Some(CacheTemporalState {
            observing: (s.observing - 1) as nat,
            pins: (s.pins + 1) as nat,
            ..s
        })
    } else {
        None
    }
}

/// Reader departs lookup domain.
pub open spec fn step_leave_lookup(s: CacheTemporalState) -> Option<CacheTemporalState> {
    if s.admissions > 0 && (s.observing == 0 || s.admissions > 1) {
        Some(CacheTemporalState {
            admissions: (s.admissions - 1) as nat,
            ..s
        })
    } else {
        None
    }
}

/// CacheLease is dropped / release_inner is called.
pub open spec fn step_release_pin(s: CacheTemporalState) -> Option<CacheTemporalState> {
    if s.pins > 0 {
        Some(CacheTemporalState {
            pins: (s.pins - 1) as nat,
            ..s
        })
    } else {
        None
    }
}

/// Eviction from index (transitions from Published to Retired).
pub open spec fn step_retire(s: CacheTemporalState) -> Option<CacheTemporalState> {
    if s.status is Published {
        Some(CacheTemporalState {
            status: ObjectStatus::Retired,
            ..s
        })
    } else {
        None
    }
}

/// Memory reclamation (CacheNode drop and Box free).
pub open spec fn step_reclaim(s: CacheTemporalState) -> Option<CacheTemporalState> {
    if (s.status is Retired || s.status is Unpublished) && s.admissions == 0 && s.observing == 0 && s.pins == 0 {
        Some(CacheTemporalState {
            status: ObjectStatus::Reclaimed,
            ..s
        })
    } else {
        None
    }
}

// ============================================================================
// Formal Safety Theorems (TR-*)
// ============================================================================

/// **[TR-OBSERVE-1] Pointer Observation Soundness**:
/// Any active pointer observation strictly implies the object is live (Published or Retired),
/// never Reclaimed.
pub proof fn tr_observing_implies_live(s: CacheTemporalState)
    requires
        cache_temporal_inv(s),
        s.observing > 0,
    ensures
        s.status is Published || s.status is Retired,
        !(s.status is Reclaimed),
{
}

/// **[TR-LEASE-1] Lease Pin Safety**:
/// Any active pin keeps the allocation live, including never-published nodes.
pub proof fn tr_pin_implies_live(s: CacheTemporalState)
    requires
        cache_temporal_inv(s),
        s.pins > 0,
    ensures
        !(s.status is Reclaimed),
{
}

/// **[TR-ADMISSION-1] Admission Boundedness**:
/// Multiple observations may share one admission; positive observations need coverage.
pub proof fn tr_observation_requires_admission(s: CacheTemporalState)
    requires
        cache_temporal_inv(s),
    ensures
        s.observing > 0 ==> s.admissions > 0,
{
}

/// **[TR-RECLAIM-1] Drained Reclamation Precondition**:
/// Reclaiming a cache node strictly requires all admissions, observers, and pins to be zero.
pub proof fn tr_reclaimed_implies_no_capabilities(s: CacheTemporalState)
    requires
        cache_temporal_inv(s),
        s.status is Reclaimed,
    ensures
        s.admissions == 0,
        s.observing == 0,
        s.pins == 0,
{
}

/// **[TR-NO-UAF] Fundamental Temporal Safety**:
/// Holding any capability (pin or observation) guarantees that the object is not Reclaimed.
pub proof fn tr_no_use_after_reclaim(s: CacheTemporalState)
    requires
        cache_temporal_inv(s),
        s.observing > 0 || s.pins > 0,
    ensures
        !(s.status is Reclaimed),
{
}

/// **[TR-EXCLUSION] Capability Precludes Reclaim**:
/// Reclaim cannot occur while any admission, observation, or pin is active.
pub proof fn tr_observation_precludes_reclaim(s: CacheTemporalState)
    requires
        s.observing > 0,
    ensures
        step_reclaim(s) == None::<CacheTemporalState>,
{
}

pub proof fn tr_pin_precludes_reclaim(s: CacheTemporalState)
    requires
        s.pins > 0,
    ensures
        step_reclaim(s) == None::<CacheTemporalState>,
{
}

pub proof fn tr_admission_precludes_reclaim(s: CacheTemporalState)
    requires
        s.admissions > 0,
    ensures
        step_reclaim(s) == None::<CacheTemporalState>,
{
}

// ============================================================================
// Invariant Preservation
// ============================================================================

pub proof fn tr_initial_state_preserves_inv()
    ensures
        cache_temporal_inv(initial_state()),
{
}

pub proof fn tr_step_publish_preserves_inv(s: CacheTemporalState, s_next: CacheTemporalState)
    requires
        cache_temporal_inv(s),
        step_publish(s) == Some(s_next),
    ensures
        cache_temporal_inv(s_next),
{
}

pub proof fn tr_step_enter_lookup_preserves_inv(s: CacheTemporalState, s_next: CacheTemporalState)
    requires
        cache_temporal_inv(s),
        step_enter_lookup(s) == Some(s_next),
    ensures
        cache_temporal_inv(s_next),
{
}

pub proof fn tr_step_observe_pointer_preserves_inv(s: CacheTemporalState, s_next: CacheTemporalState)
    requires
        cache_temporal_inv(s),
        step_observe_pointer(s) == Some(s_next),
    ensures
        cache_temporal_inv(s_next),
{
}

pub proof fn tr_step_acquire_pin_preserves_inv(s: CacheTemporalState, s_next: CacheTemporalState)
    requires
        cache_temporal_inv(s),
        step_acquire_pin(s) == Some(s_next),
    ensures
        cache_temporal_inv(s_next),
{
}

pub proof fn tr_step_leave_lookup_preserves_inv(s: CacheTemporalState, s_next: CacheTemporalState)
    requires
        cache_temporal_inv(s),
        step_leave_lookup(s) == Some(s_next),
    ensures
        cache_temporal_inv(s_next),
{
}

pub proof fn tr_step_release_pin_preserves_inv(s: CacheTemporalState, s_next: CacheTemporalState)
    requires
        cache_temporal_inv(s),
        step_release_pin(s) == Some(s_next),
    ensures
        cache_temporal_inv(s_next),
{
}

pub proof fn tr_step_retire_preserves_inv(s: CacheTemporalState, s_next: CacheTemporalState)
    requires
        cache_temporal_inv(s),
        step_retire(s) == Some(s_next),
    ensures
        cache_temporal_inv(s_next),
{
}

pub proof fn tr_step_reclaim_preserves_inv(s: CacheTemporalState, s_next: CacheTemporalState)
    requires
        cache_temporal_inv(s),
        step_reclaim(s) == Some(s_next),
    ensures
        cache_temporal_inv(s_next),
{
}

} // verus!

#[path = "../../published_owner/src/heap_permission.rs"]
mod heap_permission;

mod scope_ownership;

#[path = "../../rotating_read_domain/src/lib.rs"]
mod rotation;

mod retirement;

mod observation_coverage;

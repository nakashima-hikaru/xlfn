//! Verus formal verification of DrainGate concurrent protocol and permit ownership.
//!
//! Shared control-flow refinement plus an abstract permit-conservation model.
//! A SeqCst atomic-counter backend owns real linear permits. Native ordering,
//! waiter backend composition and cross-layer raw pointer lifetimes remain separate.

use vstd::prelude::*;

#[path = "../../../../crates/xlfn-kernel/src/drain_gate/protocol.rs"]
mod protocol;
#[path = "../../../../crates/xlfn-kernel/src/sealable_counter/transitions.rs"]
pub(crate) mod transitions;

pub(crate) mod refinement;
pub mod permits;
pub mod stripe_ownership;

verus! {

// ============================================================================
// Abstract Drain Gate State
// ============================================================================

pub struct DrainGateState {
    pub capacity: nat,
    pub sealed: bool,
    pub waiting: bool,
    pub active: nat,
    pub outstanding_permits: nat,
}

/// System Invariant for DrainGate:
/// 1. Every outstanding permit corresponds to an allocated active count.
/// 2. Active count never exceeds the decoded machine capacity.
pub open spec fn drain_gate_inv(s: DrainGateState) -> bool {
    &&& s.outstanding_permits <= s.active
    &&& s.active <= s.capacity
}

// ============================================================================
// Abstract Transitions
// ============================================================================

/// Successful-admission relation (try_enter / try_enter_owned).
/// None means no successful transition, not a runtime rejection classification;
/// the imported kernel retains distinct Rejected and FailStop outcomes.
pub open spec fn step_acquire(s: DrainGateState) -> Option<DrainGateState> {
    if s.sealed || s.active >= s.capacity {
        None
    } else {
        Some(DrainGateState {
            capacity: s.capacity,
            sealed: s.sealed,
            waiting: s.waiting,
            active: (s.active + 1) as nat,
            outstanding_permits: (s.outstanding_permits + 1) as nat,
        })
    }
}

/// State transition on permit release.
pub open spec fn step_release(s: DrainGateState) -> Option<DrainGateState> {
    if s.outstanding_permits == 0 || s.active == 0 {
        None
    } else {
        Some(DrainGateState {
            capacity: s.capacity,
            sealed: s.sealed,
            waiting: s.waiting,
            active: (s.active - 1) as nat,
            outstanding_permits: (s.outstanding_permits - 1) as nat,
        })
    }
}

/// State transition on gate sealing.
pub open spec fn step_seal(s: DrainGateState) -> DrainGateState {
    DrainGateState {
        capacity: s.capacity,
        sealed: true,
        waiting: s.waiting,
        active: s.active,
        outstanding_permits: s.outstanding_permits,
    }
}

/// State transition on waiter registration (mark_waiting).
pub open spec fn step_mark_waiting(s: DrainGateState) -> DrainGateState {
    DrainGateState {
        capacity: s.capacity,
        sealed: s.sealed,
        waiting: true,
        active: s.active,
        outstanding_permits: s.outstanding_permits,
    }
}

/// Completion is an observation, never a transition that drains live permits.
pub open spec fn step_wait_until_idle(s: DrainGateState) -> Option<DrainGateState> {
    if s.waiting && s.active == 0 { Some(s) } else { None }
}

/// State transition on generation reopen.
pub open spec fn step_reopen(s: DrainGateState) -> Option<DrainGateState> {
    if s.sealed && s.active == 0 && s.outstanding_permits == 0 {
        Some(DrainGateState {
            capacity: s.capacity,
            sealed: false,
            waiting: false,
            active: 0,
            outstanding_permits: 0,
        })
    } else {
        None
    }
}

// ============================================================================
// Formal Safety Theorems (DG-1 through DG-5)
// ============================================================================

/// **[DG-1] Permit Liveness**:
/// Holding an active permit strictly implies that the gate's active count is positive (`active > 0`).
/// Consequently, a live permit guarantees the gate cannot be in an idle state.
pub proof fn dg1_permit_implies_active(s: DrainGateState)
    requires
        drain_gate_inv(s),
        s.outstanding_permits > 0,
    ensures
        s.active > 0,
{
    // Follows directly from s.outstanding_permits <= s.active
}

/// **[DG-2] Seal Exclusion**:
/// Once the drain gate is sealed, new permits can never be acquired.
pub proof fn dg2_sealed_precludes_new_permits(s: DrainGateState)
    requires
        s.sealed,
    ensures
        step_acquire(s) == None::<DrainGateState>,
{
    // Follows from the step_acquire guard: if s.sealed { None }
}

/// **[DG-3] Quiescence On Drain**:
/// Completion of `seal_and_wait` guarantees that the gate is both sealed and completely idle,
/// with zero active counts and zero outstanding permits.
pub proof fn dg3_seal_and_wait_establishes_quiescence(s: DrainGateState)
    requires
        drain_gate_inv(s),
        s.sealed,
        step_wait_until_idle(s).is_some(),
    ensures
        s.active == 0,
        s.outstanding_permits == 0,
        step_wait_until_idle(s) == Some(s),
{
}

/// **[DG-4] Drained Reclamation Precondition**:
/// Protected resource memory reclamation is safe only after quiescence has been certified.
/// No reader holds a permit when reclamation capability is acquired.
pub proof fn dg4_reclamation_requires_zero_permits(s: DrainGateState)
    requires
        drain_gate_inv(s),
        s.sealed,
        s.active == 0,
    ensures
        s.outstanding_permits == 0,
{
    // s.outstanding_permits <= s.active == 0
}

// DG-5 is proved against the actual shared 32/64-bit transition functions in
// refinement.rs, and release_tail_refines checks the lock/notify/unlock order.

/// **[DG-INV] Invariant Preservation**:
/// Every valid state transition preserves the DrainGate invariant `drain_gate_inv`.
pub proof fn dg_invariant_preservation_acquire(s: DrainGateState, s_next: DrainGateState)
    requires
        drain_gate_inv(s),
        step_acquire(s) == Some(s_next),
    ensures
        drain_gate_inv(s_next),
{
}

pub proof fn dg_invariant_preservation_release(s: DrainGateState, s_next: DrainGateState)
    requires
        drain_gate_inv(s),
        step_release(s) == Some(s_next),
    ensures
        drain_gate_inv(s_next),
{
}

pub proof fn dg_invariant_preservation_reopen(s: DrainGateState, s_next: DrainGateState)
    requires
        drain_gate_inv(s),
        step_reopen(s) == Some(s_next),
    ensures
        drain_gate_inv(s_next),
{
}

} // verus!

pub(crate) mod permit_shares;

pub mod atomic_counter;

pub mod atomic_stripes;

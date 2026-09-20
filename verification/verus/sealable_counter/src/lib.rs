//! Verus formal verification of SealableCounter state transitions.
//!
//! Models and verifies the mechanical transitions of `crates/xlfn-kernel/src/sealable_counter.rs`.
//! All proofs ensure zero verification failures, zero assumes, and zero runtime overhead.

use vstd::prelude::*;

verus! {

// ============================================================================
// Bit Layout Constants (matching crates/xlfn-kernel/src/sealable_counter.rs)
// ============================================================================

pub const SEALED_BIT: u64 = 0x8000_0000_0000_0000;
pub const WAITING_BIT: u64 = 0x4000_0000_0000_0000;
pub const ACTIVE_COUNT_MASK: u64 = 0x3FFF_FFFF_FFFF_FFFF;

// ============================================================================
// Abstract State & Spec View
// ============================================================================

pub struct CounterState {
    pub sealed: bool,
    pub waiting: bool,
    pub active: nat,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReleaseOutcome {
    StillActive,
    BecameIdle,
}

/// The spec view mapping a 64-bit machine representation to its abstract state:
/// decode(s) = (sealed(s), waiting(s), active(s))
pub open spec fn decode(state: u64) -> CounterState {
    CounterState {
        sealed: (state & SEALED_BIT) != 0,
        waiting: (state & WAITING_BIT) != 0,
        active: (state & ACTIVE_COUNT_MASK) as nat,
    }
}

// ============================================================================
// Pure State Transitions (Specification functions for SMT verification)
// ============================================================================

/// Specification of admission acquire transition.
pub open spec fn acquire_update(state: u64) -> Option<u64> {
    if state & SEALED_BIT != 0 {
        None
    } else if state & ACTIVE_COUNT_MASK == ACTIVE_COUNT_MASK {
        None
    } else {
        Some((state + 1) as u64)
    }
}

/// Specification of release transition.
pub open spec fn release_update(state: u64) -> Option<u64> {
    if state & ACTIVE_COUNT_MASK != 0 {
        Some((state - 1) as u64)
    } else {
        None
    }
}

/// Specification of release without waiter notification.
pub open spec fn release_without_notification_update(state: u64) -> Option<u64> {
    let active = state & ACTIVE_COUNT_MASK;
    if active == 0 {
        None
    } else if active == 1 && state & WAITING_BIT != 0 {
        None
    } else {
        Some((state - 1) as u64)
    }
}

/// Specification of idle observation outcome.
pub open spec fn release_outcome(previous: u64) -> ReleaseOutcome {
    if previous & ACTIVE_COUNT_MASK == 1 {
        ReleaseOutcome::BecameIdle
    } else {
        ReleaseOutcome::StillActive
    }
}

/// Specification of generation reopen transition.
pub open spec fn reopen_update(state: u64) -> Option<u64> {
    if state & SEALED_BIT != 0 && state & ACTIVE_COUNT_MASK == 0 {
        Some(0)
    } else {
        None
    }
}

// ============================================================================
// Executable State Transitions (Refinement of pure transitions)
// ============================================================================

/// Attempts to compute the next state on admission acquire.
#[inline]
pub fn exec_acquire_update(state: u64) -> (res: Option<u64>)
    ensures
        res == acquire_update(state),
{
    if state & SEALED_BIT != 0 {
        return None;
    }
    if state & ACTIVE_COUNT_MASK == ACTIVE_COUNT_MASK {
        return None;
    }
    assert((state & SEALED_BIT == 0) ==> state < 0xFFFF_FFFF_FFFF_FFFF_u64) by(bit_vector);
    Some(state + 1)
}

/// Computes the next state on an ordinary release.
#[inline]
pub fn exec_release_update(state: u64) -> (res: Option<u64>)
    ensures
        res == release_update(state),
{
    if state & ACTIVE_COUNT_MASK != 0 {
        assert((state & ACTIVE_COUNT_MASK != 0) ==> state > 0_u64) by(bit_vector);
        Some(state - 1)
    } else {
        None
    }
}

/// Attempts release without waiter notification.
#[inline]
pub fn exec_release_without_notification_update(state: u64) -> (res: Option<u64>)
    ensures
        res == release_without_notification_update(state),
{
    let active = state & ACTIVE_COUNT_MASK;
    if active == 0 {
        return None;
    }
    if active == 1 && state & WAITING_BIT != 0 {
        return None;
    }
    assert((state & ACTIVE_COUNT_MASK != 0) ==> state > 0_u64) by(bit_vector);
    Some(state - 1)
}

/// Translates the previous state observed before an atomic decrement into
/// an idle observation outcome.
#[inline]
pub fn exec_release_outcome(previous: u64) -> (res: ReleaseOutcome)
    ensures
        res == release_outcome(previous),
{
    if previous & ACTIVE_COUNT_MASK == 1 {
        ReleaseOutcome::BecameIdle
    } else {
        ReleaseOutcome::StillActive
    }
}

/// Computes the state after a generation reopen.
#[inline]
pub fn exec_reopen_update(state: u64) -> (res: Option<u64>)
    ensures
        res == reopen_update(state),
{
    if state & SEALED_BIT != 0 && state & ACTIVE_COUNT_MASK == 0 {
        Some(0)
    } else {
        None
    }
}

// ============================================================================
// Formal Theorems (SC-1 through SC-9)
// ============================================================================

/// **[SC-1] Bounded Active Count**:
/// The decoded active count is strictly bounded by `ACTIVE_COUNT_MASK`.
pub proof fn sc1_bounded_active(state: u64)
    ensures
        decode(state).active <= ACTIVE_COUNT_MASK as nat,
{
    assert((state & ACTIVE_COUNT_MASK) <= ACTIVE_COUNT_MASK) by(bit_vector);
}

/// **[SC-2] Successful Acquire Increments Active**:
/// A successful `acquire_update` increments the active count by 1, while
/// preserving both the `sealed` and `waiting` flags.
pub proof fn sc2_acquire_update_increments_active(state: u64, next: u64)
    requires
        acquire_update(state) == Some(next),
    ensures
        decode(next).active == decode(state).active + 1,
        decode(next).sealed == decode(state).sealed,
        decode(next).waiting == decode(state).waiting,
{
    assert(
        (next == (state + 1) as u64 && (state & ACTIVE_COUNT_MASK != ACTIVE_COUNT_MASK)) ==>
        (
            (next & ACTIVE_COUNT_MASK) == (state & ACTIVE_COUNT_MASK) + 1 &&
            (next & SEALED_BIT) == (state & SEALED_BIT) &&
            (next & WAITING_BIT) == (state & WAITING_BIT)
        )
    ) by(bit_vector);
}

/// **[SC-3] Sealed Rejects Acquire**:
/// If the counter is sealed, admission acquire always fails (`None`).
pub proof fn sc3_sealed_rejects_acquire(state: u64)
    requires
        decode(state).sealed,
    ensures
        acquire_update(state) == None,
{
    // Direct from decode definition and acquire_update guard
}

/// **[SC-4] Release Requires Active > 0**:
/// Release requires at least one active permit; idle counters reject release.
pub proof fn sc4_release_requires_active(state: u64)
    requires
        decode(state).active == 0,
    ensures
        release_update(state) == None,
{
    // Direct from state & ACTIVE_COUNT_MASK == 0
}

/// **[SC-5] Successful Release Decrements Active**:
/// A successful `release_update` decrements the active count by 1, while
/// preserving both the `sealed` and `waiting` flags.
pub proof fn sc5_release_update_decrements_active(state: u64, next: u64)
    requires
        release_update(state) == Some(next),
    ensures
        decode(next).active == decode(state).active - 1,
        decode(next).sealed == decode(state).sealed,
        decode(next).waiting == decode(state).waiting,
{
    assert(
        (next == (state - 1) as u64 && (state & ACTIVE_COUNT_MASK != 0)) ==>
        (
            (next & ACTIVE_COUNT_MASK) == (state & ACTIVE_COUNT_MASK) - 1 &&
            (next & SEALED_BIT) == (state & SEALED_BIT) &&
            (next & WAITING_BIT) == (state & WAITING_BIT)
        )
    ) by(bit_vector);
}

/// **[SC-6] BecameIdle Equivalence**:
/// `release_outcome(previous)` yields `BecameIdle` if and only if the
/// observed previous active count was exactly 1.
pub proof fn sc6_became_idle_equivalence(previous: u64)
    ensures
        release_outcome(previous) == ReleaseOutcome::BecameIdle <==> decode(previous).active == 1,
{
    // Direct from definition
}

/// **[SC-7] Reopen Precondition**:
/// Reopening is only valid when the counter is sealed and idle (`active == 0`).
pub proof fn sc7_reopen_precondition(state: u64, next: u64)
    requires
        reopen_update(state) == Some(next),
    ensures
        decode(state).sealed,
        decode(state).active == 0,
{
    // Direct from guard
}

/// **[SC-8] Reopen Postcondition**:
/// Successful reopen establishes an unsealed, unwaiting, idle counter (`state == 0`).
pub proof fn sc8_reopen_postcondition(state: u64, next: u64)
    requires
        reopen_update(state) == Some(next),
    ensures
        !decode(next).sealed,
        !decode(next).waiting,
        decode(next).active == 0,
{
    assert(next == 0);
    assert((0_u64 & SEALED_BIT) == 0) by(bit_vector);
    assert((0_u64 & WAITING_BIT) == 0) by(bit_vector);
    assert((0_u64 & ACTIVE_COUNT_MASK) == 0) by(bit_vector);
}

/// **[SC-9] Retain Final Capability Under Waiter (Safety Critical)**:
/// When a waiter is registered (`waiting == true`) and the active count is 1,
/// `release_without_notification` strictly refuses to decrement (`returns None`).
/// This guarantees that the final active capability is not discarded before the
/// drain gate acquires its notification mutex.
pub proof fn sc9_waiting_active_one_retains_final_count(state: u64)
    requires
        decode(state).waiting,
        decode(state).active == 1,
    ensures
        release_without_notification_update(state) == None,
{
    // Evaluates directly: active == 1 && state & WAITING_BIT != 0 -> None
}

/// **[SC-9b] Successful Release Without Notification Decrements Active**:
/// When `release_without_notification_update` succeeds, it decrements the active count
/// by 1 and preserves `sealed` and `waiting`.
pub proof fn sc9b_release_without_notification_decrements_active(state: u64, next: u64)
    requires
        release_without_notification_update(state) == Some(next),
    ensures
        decode(next).active == decode(state).active - 1,
        decode(next).sealed == decode(state).sealed,
        decode(next).waiting == decode(state).waiting,
{
    assert(
        (next == (state - 1) as u64 && (state & ACTIVE_COUNT_MASK != 0)) ==>
        (
            (next & ACTIVE_COUNT_MASK) == (state & ACTIVE_COUNT_MASK) - 1 &&
            (next & SEALED_BIT) == (state & SEALED_BIT) &&
            (next & WAITING_BIT) == (state & WAITING_BIT)
        )
    ) by(bit_vector);
}

} // verus!

//! Verus formal verification of SealableCounter state transitions.
//!
//! Direct Single Source of Truth (SSOT) verification of `crates/xlfn-kernel/src/sealable_counter/transitions.rs`.
//! All proofs verify both 32-bit (i686 Windows) and 64-bit platforms with:
//! - Zero verification failures
//! - Zero assumes
//! - Zero runtime overhead
//! - Strict semantic alignment with fail-stop / abort guarantees

use vstd::prelude::*;

#[path = "../../../../crates/xlfn-kernel/src/sealable_counter/transitions.rs"]
pub mod transitions;

use transitions::{
    ReleaseOutcome, TransitionOutcome, ACTIVE_COUNT_MASK_32, ACTIVE_COUNT_MASK_64, SEALED_BIT_32,
    SEALED_BIT_64, WAITING_BIT_32, WAITING_BIT_64,
};

verus! {

// ============================================================================
// Abstract State & Spec Views (32-bit and 64-bit)
// ============================================================================

pub struct CounterState {
    pub sealed: bool,
    pub waiting: bool,
    pub active: nat,
}

/// The spec view mapping a 32-bit machine representation to its abstract state:
/// decode(s) = (sealed(s), waiting(s), active(s))
pub open spec fn decode_32(state: u32) -> CounterState {
    CounterState {
        sealed: (state & SEALED_BIT_32) != 0,
        waiting: (state & WAITING_BIT_32) != 0,
        active: (state & ACTIVE_COUNT_MASK_32) as nat,
    }
}

/// The spec view mapping a 64-bit machine representation to its abstract state:
/// decode(s) = (sealed(s), waiting(s), active(s))
pub open spec fn decode_64(state: u64) -> CounterState {
    CounterState {
        sealed: (state & SEALED_BIT_64) != 0,
        waiting: (state & WAITING_BIT_64) != 0,
        active: (state & ACTIVE_COUNT_MASK_64) as nat,
    }
}

// ============================================================================
// Formal Theorems (SC-1 through SC-9b, proved for both 32-bit and 64-bit)
// ============================================================================

// ----------------------------------------------------------------------------
// [SC-1] Bounded Active Count
// ----------------------------------------------------------------------------

/// **[SC-1-32] Bounded Active Count (32-bit)**:
/// The decoded active count is strictly bounded by `ACTIVE_COUNT_MASK_32`.
pub proof fn sc1_bounded_active_32(state: u32)
    ensures
        decode_32(state).active <= ACTIVE_COUNT_MASK_32 as nat,
{
    assert((state & ACTIVE_COUNT_MASK_32) <= ACTIVE_COUNT_MASK_32) by(bit_vector);
}

/// **[SC-1-64] Bounded Active Count (64-bit)**:
/// The decoded active count is strictly bounded by `ACTIVE_COUNT_MASK_64`.
pub proof fn sc1_bounded_active_64(state: u64)
    ensures
        decode_64(state).active <= ACTIVE_COUNT_MASK_64 as nat,
{
    assert((state & ACTIVE_COUNT_MASK_64) <= ACTIVE_COUNT_MASK_64) by(bit_vector);
}

// ----------------------------------------------------------------------------
// [SC-2] Successful Acquire Increments Active
// ----------------------------------------------------------------------------

/// **[SC-2-32] Successful Acquire Increments Active (32-bit)**:
/// A successful `acquire_step_32` strictly increments active by 1, preserving flags.
pub proof fn sc2_acquire_update_increments_active_32(state: u32, next: u32)
    requires
        transitions::acquire_spec_32(state) == TransitionOutcome::Success(next),
    ensures
        decode_32(next).active == decode_32(state).active + 1,
        decode_32(next).sealed == decode_32(state).sealed,
        decode_32(next).waiting == decode_32(state).waiting,
{
    assert(
        (next == state.wrapping_add(1) && (state & ACTIVE_COUNT_MASK_32 != ACTIVE_COUNT_MASK_32)) ==>
        (
            (next & ACTIVE_COUNT_MASK_32) == (state & ACTIVE_COUNT_MASK_32) + 1 &&
            (next & SEALED_BIT_32) == (state & SEALED_BIT_32) &&
            (next & WAITING_BIT_32) == (state & WAITING_BIT_32)
        )
    ) by(bit_vector);
}

/// **[SC-2-64] Successful Acquire Increments Active (64-bit)**:
/// A successful `acquire_step_64` strictly increments active by 1, preserving flags.
pub proof fn sc2_acquire_update_increments_active_64(state: u64, next: u64)
    requires
        transitions::acquire_spec_64(state) == TransitionOutcome::Success(next),
    ensures
        decode_64(next).active == decode_64(state).active + 1,
        decode_64(next).sealed == decode_64(state).sealed,
        decode_64(next).waiting == decode_64(state).waiting,
{
    assert(
        (next == state.wrapping_add(1) && (state & ACTIVE_COUNT_MASK_64 != ACTIVE_COUNT_MASK_64)) ==>
        (
            (next & ACTIVE_COUNT_MASK_64) == (state & ACTIVE_COUNT_MASK_64) + 1 &&
            (next & SEALED_BIT_64) == (state & SEALED_BIT_64) &&
            (next & WAITING_BIT_64) == (state & WAITING_BIT_64)
        )
    ) by(bit_vector);
}

// ----------------------------------------------------------------------------
// [SC-3] Sealed Rejects Acquire
// ----------------------------------------------------------------------------

/// **[SC-3-32] Sealed Rejects Acquire (32-bit)**:
/// If the counter is sealed, admission acquire always yields `Rejected`.
pub proof fn sc3_sealed_rejects_acquire_32(state: u32)
    requires
        decode_32(state).sealed,
    ensures
        transitions::acquire_spec_32(state) == TransitionOutcome::Rejected,
{
}

/// **[SC-3-64] Sealed Rejects Acquire (64-bit)**:
/// If the counter is sealed, admission acquire always yields `Rejected`.
pub proof fn sc3_sealed_rejects_acquire_64(state: u64)
    requires
        decode_64(state).sealed,
    ensures
        transitions::acquire_spec_64(state) == TransitionOutcome::Rejected,
{
}

// ----------------------------------------------------------------------------
// [SC-4] Release Underflow Abort (FailStop)
// ----------------------------------------------------------------------------

/// **[SC-4-32] Release Requires Active > 0 (32-bit)**:
/// Attempting to release an idle counter triggers FailStop (process abort).
pub proof fn sc4_release_requires_active_32(state: u32)
    requires
        decode_32(state).active == 0,
    ensures
        transitions::release_spec_32(state) == TransitionOutcome::FailStop,
{
}

/// **[SC-4-64] Release Requires Active > 0 (64-bit)**:
/// Attempting to release an idle counter triggers FailStop (process abort).
pub proof fn sc4_release_requires_active_64(state: u64)
    requires
        decode_64(state).active == 0,
    ensures
        transitions::release_spec_64(state) == TransitionOutcome::FailStop,
{
}

// ----------------------------------------------------------------------------
// [SC-5] Successful Release Decrements Active
// ----------------------------------------------------------------------------

/// **[SC-5-32] Successful Release Decrements Active (32-bit)**:
/// A successful `release_step_32` strictly decrements active by 1, preserving flags.
pub proof fn sc5_release_update_decrements_active_32(state: u32, next: u32)
    requires
        transitions::release_spec_32(state) == TransitionOutcome::Success(next),
    ensures
        decode_32(next).active == decode_32(state).active - 1,
        decode_32(next).sealed == decode_32(state).sealed,
        decode_32(next).waiting == decode_32(state).waiting,
{
    assert(
        (next == state.wrapping_sub(1) && (state & ACTIVE_COUNT_MASK_32 != 0)) ==>
        (
            (next & ACTIVE_COUNT_MASK_32) == (state & ACTIVE_COUNT_MASK_32) - 1 &&
            (next & SEALED_BIT_32) == (state & SEALED_BIT_32) &&
            (next & WAITING_BIT_32) == (state & WAITING_BIT_32)
        )
    ) by(bit_vector);
}

/// **[SC-5-64] Successful Release Decrements Active (64-bit)**:
/// A successful `release_step_64` strictly decrements active by 1, preserving flags.
pub proof fn sc5_release_update_decrements_active_64(state: u64, next: u64)
    requires
        transitions::release_spec_64(state) == TransitionOutcome::Success(next),
    ensures
        decode_64(next).active == decode_64(state).active - 1,
        decode_64(next).sealed == decode_64(state).sealed,
        decode_64(next).waiting == decode_64(state).waiting,
{
    assert(
        (next == state.wrapping_sub(1) && (state & ACTIVE_COUNT_MASK_64 != 0)) ==>
        (
            (next & ACTIVE_COUNT_MASK_64) == (state & ACTIVE_COUNT_MASK_64) - 1 &&
            (next & SEALED_BIT_64) == (state & SEALED_BIT_64) &&
            (next & WAITING_BIT_64) == (state & WAITING_BIT_64)
        )
    ) by(bit_vector);
}

// ----------------------------------------------------------------------------
// [SC-6] BecameIdle Equivalence
// ----------------------------------------------------------------------------

/// **[SC-6-32] BecameIdle Equivalence (32-bit)**:
/// `release_outcome_step_32(previous)` yields `BecameIdle` iff previous active was 1.
pub proof fn sc6_became_idle_equivalence_32(previous: u32)
    ensures
        transitions::release_outcome_spec_32(previous) == ReleaseOutcome::BecameIdle <==> decode_32(previous).active == 1,
{
}

/// **[SC-6-64] BecameIdle Equivalence (64-bit)**:
/// `release_outcome_step_64(previous)` yields `BecameIdle` iff previous active was 1.
pub proof fn sc6_became_idle_equivalence_64(previous: u64)
    ensures
        transitions::release_outcome_spec_64(previous) == ReleaseOutcome::BecameIdle <==> decode_64(previous).active == 1,
{
}

// ----------------------------------------------------------------------------
// [SC-7] Reopen Precondition
// ----------------------------------------------------------------------------

/// **[SC-7-32] Reopen Precondition (32-bit)**:
/// Reopening is valid only when sealed and idle (`active == 0`).
pub proof fn sc7_reopen_precondition_32(state: u32, next: u32)
    requires
        transitions::reopen_spec_32(state) == TransitionOutcome::Success(next),
    ensures
        decode_32(state).sealed,
        decode_32(state).active == 0,
{
}

/// **[SC-7-64] Reopen Precondition (64-bit)**:
/// Reopening is valid only when sealed and idle (`active == 0`).
pub proof fn sc7_reopen_precondition_64(state: u64, next: u64)
    requires
        transitions::reopen_spec_64(state) == TransitionOutcome::Success(next),
    ensures
        decode_64(state).sealed,
        decode_64(state).active == 0,
{
}

// ----------------------------------------------------------------------------
// [SC-8] Reopen Postcondition
// ----------------------------------------------------------------------------

/// **[SC-8-32] Reopen Postcondition (32-bit)**:
/// Successful reopen establishes an unsealed, unwaiting, idle counter (`state == 0`).
pub proof fn sc8_reopen_postcondition_32(state: u32, next: u32)
    requires
        transitions::reopen_spec_32(state) == TransitionOutcome::Success(next),
    ensures
        !decode_32(next).sealed,
        !decode_32(next).waiting,
        decode_32(next).active == 0,
{
    assert(next == 0);
    assert((0_u32 & SEALED_BIT_32) == 0) by(bit_vector);
    assert((0_u32 & WAITING_BIT_32) == 0) by(bit_vector);
    assert((0_u32 & ACTIVE_COUNT_MASK_32) == 0) by(bit_vector);
}

/// **[SC-8-64] Reopen Postcondition (64-bit)**:
/// Successful reopen establishes an unsealed, unwaiting, idle counter (`state == 0`).
pub proof fn sc8_reopen_postcondition_64(state: u64, next: u64)
    requires
        transitions::reopen_spec_64(state) == TransitionOutcome::Success(next),
    ensures
        !decode_64(next).sealed,
        !decode_64(next).waiting,
        decode_64(next).active == 0,
{
    assert(next == 0);
    assert((0_u64 & SEALED_BIT_64) == 0) by(bit_vector);
    assert((0_u64 & WAITING_BIT_64) == 0) by(bit_vector);
    assert((0_u64 & ACTIVE_COUNT_MASK_64) == 0) by(bit_vector);
}

// ----------------------------------------------------------------------------
// [SC-9] Retain Final Capability Under Waiter
// ----------------------------------------------------------------------------

/// **[SC-9-32] Retain Final Capability Under Waiter (32-bit)**:
/// When waiting is set and active is 1, release_without_notification yields `Rejected`.
pub proof fn sc9_waiting_active_one_retains_final_count_32(state: u32)
    requires
        decode_32(state).waiting,
        decode_32(state).active == 1,
    ensures
        transitions::release_without_notification_spec_32(state) == TransitionOutcome::Rejected,
{
}

/// **[SC-9-64] Retain Final Capability Under Waiter (64-bit)**:
/// When waiting is set and active is 1, release_without_notification yields `Rejected`.
pub proof fn sc9_waiting_active_one_retains_final_count_64(state: u64)
    requires
        decode_64(state).waiting,
        decode_64(state).active == 1,
    ensures
        transitions::release_without_notification_spec_64(state) == TransitionOutcome::Rejected,
{
}

// ----------------------------------------------------------------------------
// [SC-9b] Successful Release Without Notification Decrements Active
// ----------------------------------------------------------------------------

/// **[SC-9b-32] Successful Release Without Notification Decrements Active (32-bit)**:
pub proof fn sc9b_release_without_notification_decrements_active_32(state: u32, next: u32)
    requires
        transitions::release_without_notification_spec_32(state) == TransitionOutcome::Success(next),
    ensures
        decode_32(next).active == decode_32(state).active - 1,
        decode_32(next).sealed == decode_32(state).sealed,
        decode_32(next).waiting == decode_32(state).waiting,
{
    assert(
        (next == state.wrapping_sub(1) && (state & ACTIVE_COUNT_MASK_32 != 0)) ==>
        (
            (next & ACTIVE_COUNT_MASK_32) == (state & ACTIVE_COUNT_MASK_32) - 1 &&
            (next & SEALED_BIT_32) == (state & SEALED_BIT_32) &&
            (next & WAITING_BIT_32) == (state & WAITING_BIT_32)
        )
    ) by(bit_vector);
}

/// **[SC-9b-64] Successful Release Without Notification Decrements Active (64-bit)**:
pub proof fn sc9b_release_without_notification_decrements_active_64(state: u64, next: u64)
    requires
        transitions::release_without_notification_spec_64(state) == TransitionOutcome::Success(next),
    ensures
        decode_64(next).active == decode_64(state).active - 1,
        decode_64(next).sealed == decode_64(state).sealed,
        decode_64(next).waiting == decode_64(state).waiting,
{
    assert(
        (next == state.wrapping_sub(1) && (state & ACTIVE_COUNT_MASK_64 != 0)) ==>
        (
            (next & ACTIVE_COUNT_MASK_64) == (state & ACTIVE_COUNT_MASK_64) - 1 &&
            (next & SEALED_BIT_64) == (state & SEALED_BIT_64) &&
            (next & WAITING_BIT_64) == (state & WAITING_BIT_64)
        )
    ) by(bit_vector);
}

// ----------------------------------------------------------------------------
// FailStop Overflow / Underflow Semantic Verification
// ----------------------------------------------------------------------------

/// Counter overflow on acquire strictly triggers FailStop (32-bit).
pub proof fn sc_overflow_triggers_failstop_32(state: u32)
    requires
        !decode_32(state).sealed,
        decode_32(state).active == ACTIVE_COUNT_MASK_32 as nat,
    ensures
        transitions::acquire_spec_32(state) == TransitionOutcome::FailStop,
{
}

/// Counter overflow on acquire strictly triggers FailStop (64-bit).
pub proof fn sc_overflow_triggers_failstop_64(state: u64)
    requires
        !decode_64(state).sealed,
        decode_64(state).active == ACTIVE_COUNT_MASK_64 as nat,
    ensures
        transitions::acquire_spec_64(state) == TransitionOutcome::FailStop,
{
}

/// Counter underflow on release_without_notification strictly triggers FailStop (32-bit).
pub proof fn sc_underflow_triggers_failstop_32(state: u32)
    requires
        decode_32(state).active == 0,
    ensures
        transitions::release_without_notification_spec_32(state) == TransitionOutcome::FailStop,
{
}

/// Counter underflow on release_without_notification strictly triggers FailStop (64-bit).
pub proof fn sc_underflow_triggers_failstop_64(state: u64)
    requires
        decode_64(state).active == 0,
    ensures
        transitions::release_without_notification_spec_64(state) == TransitionOutcome::FailStop,
{
}

} // verus!

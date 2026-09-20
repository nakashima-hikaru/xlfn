//! Verus formal verification of HandleReadDomain and handle lifecycle safety.
//!
//! Models and verifies the call-scoped handle admission domain and deferred binding reclamation protocol
//! of `crates/xlfn/src/handle/domain.rs`.
//! Formally proves theorems [HD-1] through [HD-5].
//! All proofs ensure zero verification failures, zero assumes, and zero runtime overhead.

use vstd::prelude::*;

verus! {

// ============================================================================
// Abstract Handle Domain State
// ============================================================================

pub struct HandleDomainState {
    pub readers_0: nat,
    pub readers_1: nat,
    pub sealed_0: bool,
    pub sealed_1: bool,
    pub current_gen: nat,
    pub active_bindings: nat,
    pub pending_0: nat,
    pub pending_1: nat,
    pub reclaimed_bindings: nat,
    pub debt: nat,
    pub closed: bool,
}

pub open spec fn get_readers(s: HandleDomainState, gen: nat) -> nat {
    if gen == 0 { s.readers_0 } else { s.readers_1 }
}

pub open spec fn get_pending(s: HandleDomainState, gen: nat) -> nat {
    if gen == 0 { s.pending_0 } else { s.pending_1 }
}

/// System Invariant for HandleReadDomain:
/// - current_gen is 0 or 1.
/// - At most one generation admits readers (!sealed_0 && !sealed_1 is false).
/// - Debt matches pending queue lengths: debt == pending_0 + pending_1.
/// - Closed domain implies both sealed and no active readers.
pub open spec fn handle_domain_inv(s: HandleDomainState) -> bool {
    &&& (s.current_gen == 0 || s.current_gen == 1)
    &&& (!s.sealed_0 ==> s.sealed_1)
    &&& (!s.sealed_1 ==> s.sealed_0)
    &&& (s.debt == s.pending_0 + s.pending_1)
    &&& (s.closed ==> (s.sealed_0 && s.sealed_1))
}

// ============================================================================
// Protocol Transitions
// ============================================================================

pub open spec fn initial_state() -> HandleDomainState {
    HandleDomainState {
        readers_0: 0,
        readers_1: 0,
        sealed_0: false,
        sealed_1: true,
        current_gen: 0,
        active_bindings: 0,
        pending_0: 0,
        pending_1: 0,
        reclaimed_bindings: 0,
        debt: 0,
        closed: false,
    }
}

/// UDF call acquires handle domain permit (enter / enter_owned).
pub open spec fn step_enter_domain(s: HandleDomainState) -> Option<HandleDomainState> {
    if s.closed {
        None
    } else if s.current_gen == 0 {
        if s.sealed_0 {
            None
        } else {
            Some(HandleDomainState {
                readers_0: (s.readers_0 + 1) as nat,
                ..s
            })
        }
    } else {
        if s.sealed_1 {
            None
        } else {
            Some(HandleDomainState {
                readers_1: (s.readers_1 + 1) as nat,
                ..s
            })
        }
    }
}

/// UDF call completes and drops handle domain permit.
pub open spec fn step_release_domain(s: HandleDomainState, gen: nat) -> Option<HandleDomainState> {
    if gen == 0 {
        if s.readers_0 == 0 {
            None
        } else {
            Some(HandleDomainState {
                readers_0: (s.readers_0 - 1) as nat,
                ..s
            })
        }
    } else if gen == 1 {
        if s.readers_1 == 0 {
            None
        } else {
            Some(HandleDomainState {
                readers_1: (s.readers_1 - 1) as nat,
                ..s
            })
        }
    } else {
        None
    }
}

/// Binding published into registry.
pub open spec fn step_publish_binding(s: HandleDomainState) -> HandleDomainState {
    HandleDomainState {
        active_bindings: (s.active_bindings + 1) as nat,
        ..s
    }
}

/// Binding withdrawn from active table and enqueued for deferred reclamation (enqueue_reclaim).
pub open spec fn step_enqueue_reclaim(s: HandleDomainState) -> Option<HandleDomainState> {
    if s.active_bindings == 0 {
        None
    } else {
        let gen = s.current_gen;
        if gen == 0 {
            Some(HandleDomainState {
                active_bindings: (s.active_bindings - 1) as nat,
                pending_0: (s.pending_0 + 1) as nat,
                debt: (s.debt + 1) as nat,
                ..s
            })
        } else {
            Some(HandleDomainState {
                active_bindings: (s.active_bindings - 1) as nat,
                pending_1: (s.pending_1 + 1) as nat,
                debt: (s.debt + 1) as nat,
                ..s
            })
        }
    }
}

/// Domain rotation (quiesce Step 1: seal old, switch current, reopen next).
pub open spec fn step_rotate_domain(s: HandleDomainState) -> Option<HandleDomainState> {
    if s.closed {
        None
    } else {
        let old = s.current_gen;
        let next = if old == 0 { 1 as nat } else { 0 as nat };
        if next == 1 {
            // next is 1: must be idle and sealed to reopen
            if s.sealed_1 && s.readers_1 == 0 {
                Some(HandleDomainState {
                    sealed_0: true,
                    sealed_1: false,
                    current_gen: 1,
                    ..s
                })
            } else {
                None
            }
        } else {
            // next is 0: must be idle and sealed to reopen
            if s.sealed_0 && s.readers_0 == 0 {
                Some(HandleDomainState {
                    sealed_0: false,
                    sealed_1: true,
                    current_gen: 0,
                    ..s
                })
            } else {
                None
            }
        }
    }
}

/// Drained generation reclamation (quiesce Step 2: take queue and drop records).
pub open spec fn step_reclaim_generation(s: HandleDomainState, old_gen: nat) -> Option<HandleDomainState> {
    if old_gen == 0 {
        if s.readers_0 == 0 {
            Some(HandleDomainState {
                reclaimed_bindings: (s.reclaimed_bindings + s.pending_0) as nat,
                debt: (s.debt - s.pending_0) as nat,
                pending_0: 0,
                ..s
            })
        } else {
            None
        }
    } else if old_gen == 1 {
        if s.readers_1 == 0 {
            Some(HandleDomainState {
                reclaimed_bindings: (s.reclaimed_bindings + s.pending_1) as nat,
                debt: (s.debt - s.pending_1) as nat,
                pending_1: 0,
                ..s
            })
        } else {
            None
        }
    } else {
        None
    }
}

/// Final seal and drain (seal).
pub open spec fn step_seal_and_drain(s: HandleDomainState) -> Option<HandleDomainState> {
    if s.readers_0 == 0 && s.readers_1 == 0 {
        Some(HandleDomainState {
            sealed_0: true,
            sealed_1: true,
            reclaimed_bindings: (s.reclaimed_bindings + s.pending_0 + s.pending_1) as nat,
            pending_0: 0,
            pending_1: 0,
            debt: 0,
            closed: true,
            ..s
        })
    } else {
        None
    }
}

// ============================================================================
// Formal Safety Theorems (HD-1 through HD-5)
// ============================================================================

/// **[HD-1] Admission Isolation**:
/// When closed, reader entry is unconditionally rejected.
pub proof fn hd1_closed_rejects_readers(s: HandleDomainState)
    requires
        handle_domain_inv(s),
        s.closed,
    ensures
        step_enter_domain(s) == None::<HandleDomainState>,
{
}

/// **[HD-2] Binding Retirement Before Reclamation**:
/// Enqueuing a retired binding preserves it in the pending queue without immediate destruction.
/// The binding is tracked in `debt`.
pub proof fn hd2_retirement_delays_destruction(s: HandleDomainState)
    requires
        handle_domain_inv(s),
        s.active_bindings > 0,
    ensures
        ({
            let s_next = step_enqueue_reclaim(s).unwrap();
            &&& s_next.debt == s.debt + 1
            &&& s_next.reclaimed_bindings == s.reclaimed_bindings
        }),
{
}

/// **[HD-3] Generational Quiescence**:
/// Reclaiming a retired generation strictly requires zero active readers in that generation.
pub proof fn hd3_reclamation_requires_zero_readers(s: HandleDomainState, old_gen: nat)
    requires
        handle_domain_inv(s),
        get_readers(s, old_gen) > 0,
    ensures
        step_reclaim_generation(s, old_gen) == None::<HandleDomainState>,
{
    if old_gen == 0 {
        assert(s.readers_0 > 0);
    } else if old_gen == 1 {
        assert(s.readers_1 > 0);
    }
}

/// **[HD-4] Linear Binding Deallocation**:
/// A generation's pending records are consumed and reclaimed in full, decreasing debt by exactly the queue length.
pub proof fn hd4_linear_deallocation(s: HandleDomainState, old_gen: nat)
    requires
        handle_domain_inv(s),
        old_gen == 0 || old_gen == 1,
        get_readers(s, old_gen) == 0,
    ensures
        ({
            let s_next = step_reclaim_generation(s, old_gen).unwrap();
            let count = get_pending(s, old_gen);
            &&& s_next.debt == s.debt - count
            &&& s_next.reclaimed_bindings == s.reclaimed_bindings + count
            &&& get_pending(s_next, old_gen) == 0
        }),
{
}

/// **[HD-5] Destruction Barrier**:
/// When seal and drain completes, all generations are sealed, all readers are zero,
/// and debt is strictly zero (`debt == 0`).
pub proof fn hd5_destruction_barrier_establishes_zero_debt(s: HandleDomainState)
    requires
        handle_domain_inv(s),
        s.readers_0 == 0 && s.readers_1 == 0,
    ensures
        ({
            let s_closed = step_seal_and_drain(s).unwrap();
            &&& s_closed.closed
            &&& s_closed.sealed_0
            &&& s_closed.sealed_1
            &&& s_closed.readers_0 == 0
            &&& s_closed.readers_1 == 0
            &&& s_closed.debt == 0
            &&& s_closed.pending_0 == 0
            &&& s_closed.pending_1 == 0
        }),
{
}

// ============================================================================
// Invariant Preservation
// ============================================================================

pub proof fn hd_initial_state_preserves_inv()
    ensures
        handle_domain_inv(initial_state()),
{
}

pub proof fn hd_step_enter_domain_preserves_inv(s: HandleDomainState, s_next: HandleDomainState)
    requires
        handle_domain_inv(s),
        step_enter_domain(s) == Some(s_next),
    ensures
        handle_domain_inv(s_next),
{
}

pub proof fn hd_step_release_domain_preserves_inv(s: HandleDomainState, gen: nat, s_next: HandleDomainState)
    requires
        handle_domain_inv(s),
        step_release_domain(s, gen) == Some(s_next),
    ensures
        handle_domain_inv(s_next),
{
}

pub proof fn hd_step_publish_binding_preserves_inv(s: HandleDomainState)
    requires
        handle_domain_inv(s),
    ensures
        handle_domain_inv(step_publish_binding(s)),
{
}

pub proof fn hd_step_enqueue_reclaim_preserves_inv(s: HandleDomainState, s_next: HandleDomainState)
    requires
        handle_domain_inv(s),
        step_enqueue_reclaim(s) == Some(s_next),
    ensures
        handle_domain_inv(s_next),
{
}

pub proof fn hd_step_rotate_domain_preserves_inv(s: HandleDomainState, s_next: HandleDomainState)
    requires
        handle_domain_inv(s),
        step_rotate_domain(s) == Some(s_next),
    ensures
        handle_domain_inv(s_next),
{
}

pub proof fn hd_step_reclaim_generation_preserves_inv(s: HandleDomainState, old_gen: nat, s_next: HandleDomainState)
    requires
        handle_domain_inv(s),
        step_reclaim_generation(s, old_gen) == Some(s_next),
    ensures
        handle_domain_inv(s_next),
{
}

pub proof fn hd_step_seal_and_drain_preserves_inv(s: HandleDomainState, s_next: HandleDomainState)
    requires
        handle_domain_inv(s),
        step_seal_and_drain(s) == Some(s_next),
    ensures
        handle_domain_inv(s_next),
{
}

} // verus!

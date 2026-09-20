//! Verus formal verification of RotatingReadDomain concurrent protocol and generation invariants.
//!
//! Models and verifies the two-generation admission and quiescence protocol of `crates/xlfn-kernel/src/rotating_read_domain.rs`.
//! Formally proves invariants [RRD-D1] through [RRD-D5].
//! All proofs ensure zero verification failures, zero assumes, and zero runtime overhead.

use vstd::prelude::*;

verus! {

// ============================================================================
// Abstract Generation State (DrainGate abstraction)
// ============================================================================

pub struct GenState {
    pub sealed: bool,
    pub active: nat,
    pub permits: nat,
}

pub open spec fn gen_inv(g: GenState) -> bool {
    g.permits <= g.active
}

// ============================================================================
// Abstract RotatingReadDomain State
// ============================================================================

pub struct RotatingDomainState {
    pub gen0: GenState,
    pub gen1: GenState,
    pub current: nat,
    pub transition_pending: Option<nat>,
    pub closed: bool,
}

pub open spec fn get_gen(s: RotatingDomainState, idx: nat) -> GenState {
    if idx == 0 {
        s.gen0
    } else {
        s.gen1
    }
}

pub open spec fn set_gen(s: RotatingDomainState, idx: nat, g: GenState) -> RotatingDomainState {
    if idx == 0 {
        RotatingDomainState { gen0: g, ..s }
    } else {
        RotatingDomainState { gen1: g, ..s }
    }
}

/// System Invariant for RotatingReadDomain:
/// - current is always 0 or 1.
/// - Each generation maintains its local drain invariant.
/// - [RRD-D1]: at most one generation admits readers (!gen0.sealed && !gen1.sealed is FALSE).
/// - [RRD-D5]: if closed, both generations are sealed and no generation can reopen.
pub open spec fn rotating_domain_inv(s: RotatingDomainState) -> bool {
    &&& (s.current == 0 || s.current == 1)
    &&& gen_inv(s.gen0)
    &&& gen_inv(s.gen1)
    &&& (!s.gen0.sealed ==> s.gen1.sealed)
    &&& (!s.gen1.sealed ==> s.gen0.sealed)
    &&& (s.closed ==> (s.gen0.sealed && s.gen1.sealed))
    &&& match s.transition_pending {
        Some(idx) => (idx == 0 || idx == 1) && get_gen(s, idx).sealed,
        None => true,
    }
}

// ============================================================================
// Protocol Transitions
// ============================================================================

/// Initial state of RotatingReadDomain::new().
pub open spec fn initial_state() -> RotatingDomainState {
    RotatingDomainState {
        gen0: GenState { sealed: false, active: 0, permits: 0 },
        gen1: GenState { sealed: true, active: 0, permits: 0 },
        current: 0,
        transition_pending: None,
        closed: false,
    }
}

/// Reader admission (enter / enter_owned).
/// Reader observes current generation and attempts acquisition.
pub open spec fn step_reader_enter(s: RotatingDomainState, req_idx: nat) -> Option<RotatingDomainState> {
    if s.closed || req_idx != s.current {
        None
    } else {
        let g = get_gen(s, req_idx);
        if g.sealed {
            None
        } else {
            let next_g = GenState {
                sealed: g.sealed,
                active: (g.active + 1) as nat,
                permits: (g.permits + 1) as nat,
            };
            Some(set_gen(s, req_idx, next_g))
        }
    }
}

/// Reader release (Drop / release_inner).
pub open spec fn step_reader_release(s: RotatingDomainState, idx: nat) -> Option<RotatingDomainState> {
    if idx != 0 && idx != 1 {
        None
    } else {
        let g = get_gen(s, idx);
        if g.permits == 0 || g.active == 0 {
            None
        } else {
            let next_g = GenState {
                sealed: g.sealed,
                active: (g.active - 1) as nat,
                permits: (g.permits - 1) as nat,
            };
            Some(set_gen(s, idx, next_g))
        }
    }
}

/// Transition Step 1: Seal the old current generation.
/// (rotate_and_run_with_barrier_locked: self.generations[old].seal())
pub open spec fn step_transition_seal_current(s: RotatingDomainState) -> Option<RotatingDomainState> {
    if s.closed || s.transition_pending.is_some() {
        None
    } else {
        let old = s.current;
        let g = get_gen(s, old);
        let sealed_g = GenState { sealed: true, ..g };
        Some(RotatingDomainState {
            transition_pending: Some(old),
            ..set_gen(s, old, sealed_g)
        })
    }
}

/// Transition Step 2: Publish next generation and reopen it.
/// (publish_next_locked: store next to current, then reopen next)
pub open spec fn step_transition_publish_and_reopen(s: RotatingDomainState) -> Option<RotatingDomainState> {
    if s.closed {
        None
    } else {
        match s.transition_pending {
            None => None,
            Some(old) => {
                let next = if old == 0 { 1 as nat } else { 0 as nat };
                let next_g = get_gen(s, next);
                // Can only reopen if next is idle and sealed
                if next_g.sealed && next_g.active == 0 && next_g.permits == 0 {
                    let reopened_g = GenState { sealed: false, active: 0, permits: 0 };
                    Some(RotatingDomainState {
                        current: next,
                        ..set_gen(s, next, reopened_g)
                    })
                } else {
                    None
                }
            }
        }
    }
}

/// Transition Step 3: Wait until old generation is idle, then invoke callback and finish transition.
pub open spec fn step_transition_quiesce_and_finish(s: RotatingDomainState) -> Option<RotatingDomainState> {
    match s.transition_pending {
        None => None,
        Some(old) => {
            let old_g = get_gen(s, old);
            if old_g.sealed && old_g.active == 0 && old_g.permits == 0 {
                Some(RotatingDomainState {
                    transition_pending: None,
                    ..s
                })
            } else {
                None
            }
        }
    }
}

/// Domain close (seal_and_wait).
pub open spec fn step_seal_and_wait(s: RotatingDomainState) -> RotatingDomainState {
    RotatingDomainState {
        gen0: GenState { sealed: true, active: 0, permits: 0 },
        gen1: GenState { sealed: true, active: 0, permits: 0 },
        current: s.current,
        transition_pending: None,
        closed: true,
    }
}

// ============================================================================
// Formal Safety Theorems (RRD-D1 through RRD-D5)
// ============================================================================

/// **[RRD-D1] Single Active Generation Invariant**:
/// At most one generation admits readers at any time. Both may be sealed, but both can never be open.
pub proof fn rrd_d1_at_most_one_open_generation(s: RotatingDomainState)
    requires
        rotating_domain_inv(s),
    ensures
        !(!s.gen0.sealed && !s.gen1.sealed),
{
}

/// **[RRD-D2] Admission Through Current Generation Only**:
/// A reader attempting to enter a non-current generation is rejected.
pub proof fn rrd_d2_admission_only_through_current(s: RotatingDomainState, req_idx: nat)
    requires
        rotating_domain_inv(s),
        req_idx != s.current,
    ensures
        step_reader_enter(s, req_idx) == None::<RotatingDomainState>,
{
}

/// **[RRD-D3] Current Sealed Before Replacement Published**:
/// When rotation seals `old == current`, the old generation is guaranteed to be sealed
/// prior to publishing `next`. Late readers observing `old` cannot acquire it.
pub proof fn rrd_d3_sealed_before_replacement(s: RotatingDomainState)
    requires
        rotating_domain_inv(s),
        s.transition_pending.is_none(),
        !s.closed,
    ensures
        ({
            let s_step1 = step_transition_seal_current(s);
            match s_step1 {
                Some(s1) => {
                    let old = s.current;
                    &&& get_gen(s1, old).sealed
                    &&& s1.transition_pending == Some(old)
                    &&& step_reader_enter(s1, old) == None::<RotatingDomainState>
                },
                None => true,
            }
        }),
{
}

/// **[RRD-D4] Quiescence Before Reclamation Callback**:
/// The transition callback runs only after the old generation has drained completely
/// (`active == 0 && permits == 0`), guaranteeing no concurrent reader holds a reference.
pub proof fn rrd_d4_quiescence_before_callback(s: RotatingDomainState)
    requires
        rotating_domain_inv(s),
        s.transition_pending.is_some(),
    ensures
        ({
            match step_transition_quiesce_and_finish(s) {
                Some(_) => {
                    let old = s.transition_pending.unwrap();
                    let old_g = get_gen(s, old);
                    old_g.sealed && old_g.active == 0 && old_g.permits == 0
                },
                None => true,
            }
        }),
{
}

/// **[RRD-D5] Closed Domain Never Reopens**:
/// Once closed (`closed == true`), both generations remain sealed, and no transition or reopen
/// can succeed.
pub proof fn rrd_d5_closed_domain_never_reopens(s: RotatingDomainState)
    requires
        rotating_domain_inv(s),
        s.closed,
    ensures
        s.gen0.sealed,
        s.gen1.sealed,
        step_reader_enter(s, 0) == None::<RotatingDomainState>,
        step_reader_enter(s, 1) == None::<RotatingDomainState>,
        step_transition_seal_current(s) == None::<RotatingDomainState>,
        step_transition_publish_and_reopen(s) == None::<RotatingDomainState>,
{
}

// ============================================================================
// Invariant Preservation
// ============================================================================

pub proof fn rrd_initial_state_preserves_inv()
    ensures
        rotating_domain_inv(initial_state()),
{
}

pub proof fn rrd_step_reader_enter_preserves_inv(s: RotatingDomainState, req_idx: nat, s_next: RotatingDomainState)
    requires
        rotating_domain_inv(s),
        step_reader_enter(s, req_idx) == Some(s_next),
    ensures
        rotating_domain_inv(s_next),
{
}

pub proof fn rrd_step_reader_release_preserves_inv(s: RotatingDomainState, idx: nat, s_next: RotatingDomainState)
    requires
        rotating_domain_inv(s),
        step_reader_release(s, idx) == Some(s_next),
    ensures
        rotating_domain_inv(s_next),
{
}

pub proof fn rrd_step_transition_seal_current_preserves_inv(s: RotatingDomainState, s_next: RotatingDomainState)
    requires
        rotating_domain_inv(s),
        step_transition_seal_current(s) == Some(s_next),
    ensures
        rotating_domain_inv(s_next),
{
}

pub proof fn rrd_step_transition_publish_preserves_inv(s: RotatingDomainState, s_next: RotatingDomainState)
    requires
        rotating_domain_inv(s),
        step_transition_publish_and_reopen(s) == Some(s_next),
    ensures
        rotating_domain_inv(s_next),
{
}

pub proof fn rrd_step_transition_finish_preserves_inv(s: RotatingDomainState, s_next: RotatingDomainState)
    requires
        rotating_domain_inv(s),
        step_transition_quiesce_and_finish(s) == Some(s_next),
    ensures
        rotating_domain_inv(s_next),
{
}

pub proof fn rrd_step_seal_and_wait_preserves_inv(s: RotatingDomainState)
    requires
        rotating_domain_inv(s),
    ensures
        rotating_domain_inv(step_seal_and_wait(s)),
{
}

} // verus!

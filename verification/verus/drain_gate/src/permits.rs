//! Linear gate permits backed by the production counter transition kernel.
use vstd::prelude::*;
use vstd::multiset::*;
use verus_state_machines_macros::tokenized_state_machine;
use super::transitions::*;
verus! {
tokenized_state_machine!(admission {
    fields {
        #[sharding(variable)] pub active: nat,
        #[sharding(multiset)] pub permits: Multiset<()>,
    }
    #[invariant]
    pub fn conservation(&self) -> bool { self.active == self.permits.len() }
    init! { initialize() {
        init active = 0;
        init permits = Multiset::empty();
    } }
    transition! { acquire() {
        update active = pre.active + 1;
        add permits += {()};
    } }
    transition! { release() {
        remove permits -= {()};
        assert(pre.active > 0);
        update active = (pre.active - 1) as nat;
    } }
    property! { positive() {
        have permits >= {()};
        assert(pre.active > 0);
    } }
    #[inductive(initialize)] fn initialize_inductive(post: Self) {}
    #[inductive(acquire)] fn acquire_inductive(pre: Self, post: Self) {}
    #[inductive(release)] fn release_inductive(pre: Self, post: Self) {}
});

/// A zero observation excludes any still-live permit of this gate instance.
pub proof fn idle_excludes_permit(tracked gate: &admission::Instance,
    tracked active: &admission::active, tracked permit: &admission::permits)
    requires active.instance_id() == gate.id(), permit.instance_id() == gate.id(),
        active.value() == 0,
    ensures false,
{ gate.positive(active, permit); }
}
macro_rules! width {
    ($module:ident, $word:ty, $mask:ident, $acquire:ident, $release:ident) => {
    pub mod $module {
    use super::*;
    verus! {
    pub fn acquire(raw: $word,
        Tracked(gate): Tracked<&admission::Instance>,
        Tracked(active): Tracked<&mut admission::active>,
    ) -> (result: (TransitionOutcome<$word>, Tracked<Option<admission::permits>>))
        requires old(active).instance_id() == gate.id(),
            old(active).value() == (raw & $mask) as nat,
        ensures final(active).instance_id() == gate.id(),
            match result.0 {
                TransitionOutcome::Success(next) =>
                    final(active).value() == (next & $mask) as nat
                    && result.1@.is_some()
                    && result.1@.unwrap().instance_id() == gate.id(),
                _ => final(active).value() == old(active).value() && result.1@.is_none(),
            },
    {
        let outcome = $acquire(raw);
        let tracked mut permit = None;
        match outcome {
            TransitionOutcome::Success(next) => { proof {
                assert((raw & $mask != $mask && next == raw.wrapping_add(1)) ==>
                    (next & $mask) == (raw & $mask) + 1) by(bit_vector);
                permit = Some(gate.acquire(active));
            } },
            _ => {},
        }
        (outcome, Tracked(permit))
    }

    pub fn release(raw: $word,
        Tracked(gate): Tracked<&admission::Instance>,
        Tracked(active): Tracked<&mut admission::active>,
        Tracked(permit): Tracked<admission::permits>,
    ) -> (outcome: TransitionOutcome<$word>)
        requires old(active).instance_id() == gate.id(),
            old(active).value() == (raw & $mask) as nat,
            permit.instance_id() == gate.id(),
        ensures final(active).instance_id() == gate.id(),
            match outcome {
                TransitionOutcome::Success(next) => final(active).value() == (next & $mask) as nat,
                _ => false,
            },
    {
        proof { gate.positive(active, &permit); }
        let outcome = $release(raw);
        match outcome {
            TransitionOutcome::Success(next) => { proof {
                assert((raw & $mask > 0 && next == raw.wrapping_sub(1)) ==>
                    (next & $mask) == (raw & $mask) - 1) by(bit_vector);
                gate.release(active, permit);
            } },
            _ => { assert(false); },
        }
        outcome
    }
    }
    }
};
}
width!(word32, u32, ACTIVE_COUNT_MASK_32, acquire_step_32, release_step_32);
width!(word64, u64, ACTIVE_COUNT_MASK_64, acquire_step_64, release_step_64);

//! Actual atomic admission backend for the shared machine-word transition kernel.
//! vstd supplies SeqCst semantics; native memory ordering and waiter composition
//! remain separate obligations. Permits are issued only by a successful CAS.
use vstd::prelude::*;
use vstd::atomic_ghost::*;
use vstd::tokens::UniqueValueToken;
use super::transitions::*;
use super::permits::admission;
use verus_state_machines_macros::tokenized_state_machine;
verus! {
tokenized_state_machine!(lifecycle {
    fields {
        #[sharding(variable)] pub sealed: bool,
        #[sharding(variable)] pub control: bool,
    }
    #[invariant] pub fn agreement(&self) -> bool { self.sealed == self.control }
    init! { initialize(value: bool) { init sealed = value; init control = value; } }
    transition! { set(value: bool) { update sealed = value; update control = value; } }
    property! { agrees() { assert(pre.sealed == pre.control); } }
    #[inductive(initialize)] fn initialize_inductive(post: Self, value: bool) {}
    #[inductive(set)] fn set_inductive(pre: Self, post: Self, value: bool) {}
});

pub tracked struct FrozenResources { pub active: admission::active, pub control: lifecycle::control }
tokenized_state_machine!(frozen {
    fields {
        #[sharding(constant)] pub gate: vstd::tokens::InstanceId,
        #[sharding(constant)] pub authority: vstd::tokens::InstanceId,
        #[sharding(variable)] pub state: Option<FrozenResources>,
        #[sharding(storage_option)] pub held: Option<FrozenResources>,
        #[sharding(option)] pub lease: Option<FrozenResources>,
    }
    #[invariant] pub fn agreement(&self) -> bool { self.state == self.held && self.state == self.lease }
    #[invariant] pub fn valid(&self) -> bool {
        self.state.is_some() ==> self.state.unwrap().active.instance_id() == self.gate
            && self.state.unwrap().active.value() == 0
            && self.state.unwrap().control.instance_id() == self.authority && self.state.unwrap().control.value()
    }
    init! { initialize(gate: vstd::tokens::InstanceId, authority: vstd::tokens::InstanceId) {
        init gate = gate; init authority = authority; init state = None; init held = None; init lease = None;
    } }
    transition! { freeze(payload: FrozenResources) {
        require(pre.state.is_none()); require(payload.active.instance_id() == pre.gate && payload.active.value() == 0);
        require(payload.control.instance_id() == pre.authority && payload.control.value());
        update state = Some(payload); deposit held += Some(payload); add lease += Some(payload);
    } }
    transition! { thaw(payload: FrozenResources) {
        remove lease -= Some(payload); withdraw held -= Some(payload); update state = None;
    } }
    property! { leased(payload: FrozenResources) {
        have lease >= Some(payload); assert(pre.state == Some(payload));
    } }
    property! { guard_state(payload: FrozenResources) {
        require(pre.state == Some(payload)); guard held >= Some(payload);
    } }
    property! { guard_lease(payload: FrozenResources) {
        have lease >= Some(payload); guard held >= Some(payload);
        assert(payload.active.instance_id() == pre.gate && payload.active.value() == 0);
        assert(payload.control.instance_id() == pre.authority && payload.control.value());
    } }
    #[inductive(initialize)] fn initialize_inductive(post: Self, gate: vstd::tokens::InstanceId, authority: vstd::tokens::InstanceId) {}
    #[inductive(freeze)] fn freeze_inductive(pre: Self, post: Self, payload: FrozenResources) {}
    #[inductive(thaw)] fn thaw_inductive(pre: Self, post: Self, payload: FrozenResources) {}
});
pub tracked struct DrainLease { instance: frozen::Instance, ticket: frozen::lease, gate: admission::Instance }
impl DrainLease {
    pub closed spec fn inv(&self) -> bool { self.ticket.instance_id() == self.instance.id() && self.gate.id() == self.instance.gate() }
    pub closed spec fn gate_id(&self) -> vstd::tokens::InstanceId { self.instance.gate() }
    pub closed spec fn authority_id(&self) -> vstd::tokens::InstanceId { self.instance.authority() }
    pub proof fn excludes_share(tracked &self, tracked share: &super::permit_shares::Share)
        requires self.inv(), share.inv(), share.gate_id() == self.gate_id(),
        ensures false,
    { share.positive(&self.gate, self.active()); }
    pub proof fn active(tracked &self) -> (tracked active: &admission::active)
        requires self.inv(), ensures active.instance_id() == self.gate_id(), active.value() == 0,
    {
        let tracked payload = self.instance.guard_lease(self.ticket.value(), &self.ticket);
        &payload.active
    }
}
pub tracked struct DrainSet { leases: Map<vstd::tokens::InstanceId, DrainLease> }
impl DrainSet {
    pub closed spec fn inv(&self) -> bool {
        forall|id: vstd::tokens::InstanceId| #[trigger] self.leases.dom().contains(id)
            ==> self.leases[id].inv() && self.leases[id].gate_id() == id
    }
    pub closed spec fn domain(&self) -> Set<vstd::tokens::InstanceId> { self.leases.dom() }
    pub closed spec fn at(&self, id: vstd::tokens::InstanceId) -> DrainLease { self.leases[id] }
    pub open spec fn covers(&self, gates: Set<vstd::tokens::InstanceId>) -> bool { gates.subset_of(self.domain()) }
    pub proof fn empty() -> (tracked set: Self)
        ensures set.inv(), set.domain() == Set::<vstd::tokens::InstanceId>::empty(),
    { DrainSet { leases: Map::tracked_empty() } }
    pub proof fn insert(tracked &mut self, tracked lease: DrainLease)
        requires old(self).inv(), lease.inv(), !old(self).domain().contains(lease.gate_id()),
        ensures final(self).inv(), final(self).domain() == old(self).domain().insert(lease.gate_id()),
            final(self).at(lease.gate_id()) == lease,
            forall|id: vstd::tokens::InstanceId| #[trigger] old(self).domain().contains(id) ==> final(self).at(id) == old(self).at(id),
    { self.leases.tracked_insert(lease.gate_id(), lease); }
    pub proof fn remove(tracked &mut self, id: vstd::tokens::InstanceId) -> (tracked lease: DrainLease)
        requires old(self).inv(), old(self).domain().contains(id),
        ensures final(self).inv(), final(self).domain() == old(self).domain().remove(id), lease.inv(), lease.gate_id() == id,
            lease == old(self).at(id),
            forall|other: vstd::tokens::InstanceId| #[trigger] final(self).domain().contains(other) ==> final(self).at(other) == old(self).at(other),
    { self.leases.tracked_remove(id) }
    pub proof fn excludes_share(tracked &self, tracked share: &super::permit_shares::Share)
        requires self.inv(), share.inv(), self.domain().contains(share.gate_id()),
        ensures false,
    { self.leases.tracked_borrow(share.gate_id()).excludes_share(share); }
}
}
macro_rules! width {
    ($module:ident, $word:ty, $atomic:ident, $mask:ident, $sealed:ident, $acquire:ident, $release:ident, $reopen:ident) => {
    pub mod $module {
    use super::*;
    verus! {
    pub tracked struct AtomicState { active: Option<admission::active>, sealed: lifecycle::sealed, frozen: frozen::state }
    pub struct Predicate;
    impl AtomicInvariantPredicate<(vstd::tokens::InstanceId, vstd::tokens::InstanceId, vstd::tokens::InstanceId), $word, AtomicState> for Predicate {
        closed spec fn atomic_inv(key: (vstd::tokens::InstanceId, vstd::tokens::InstanceId, vstd::tokens::InstanceId), raw: $word, state: AtomicState) -> bool {
            state.frozen.instance_id() == key.2 && state.active.is_some() == state.frozen.value().is_none()
            && (state.active.is_some() ==> state.active.unwrap().instance_id() == key.0 && state.active.unwrap().value() == (raw & $mask) as nat)
            && (state.frozen.value().is_some() ==> raw & $mask == 0 && raw & $sealed != 0
                && state.frozen.value().unwrap().active.instance_id() == key.0 && state.frozen.value().unwrap().active.value() == 0
                && state.frozen.value().unwrap().control.instance_id() == key.1 && state.frozen.value().unwrap().control.value())
            && state.sealed.instance_id() == key.1 && state.sealed.value() == (raw & $sealed != 0)
        }
    }
    pub struct Counter {
        atomic: $atomic<(vstd::tokens::InstanceId, vstd::tokens::InstanceId, vstd::tokens::InstanceId), AtomicState, Predicate>,
        lifecycle: Tracked<lifecycle::Instance>,
        frozen: Tracked<frozen::Instance>,
        instance: Tracked<admission::Instance>,
    }
    impl Counter {
        pub closed spec fn id(&self) -> vstd::tokens::InstanceId { self.instance@.id() }
        pub closed spec fn authority_id(&self) -> vstd::tokens::InstanceId { self.lifecycle@.id() }
        pub closed spec fn inv(&self) -> bool { self.atomic.well_formed() && self.atomic.constant() == (self.id(), self.authority_id(), self.frozen@.id())
            && self.frozen@.gate() == self.id() && self.frozen@.authority() == self.authority_id() }
        pub fn new(sealed: bool) -> (result: (Self, Tracked<lifecycle::control>))
            ensures result.0.inv(), result.1@.instance_id() == result.0.authority_id(), result.1@.value() == sealed,
        {
            let tracked (Tracked(instance), Tracked(active), Tracked(permits)) = admission::Instance::initialize();
            let raw: $word = if sealed { $sealed } else { 0 };
            proof { assert(($sealed & $mask) == 0 && ((0 as $word) & $mask) == 0
                && ($sealed & $sealed) != 0 && ((0 as $word) & $sealed) == 0) by(bit_vector); }
            let tracked (Tracked(lifecycle), Tracked(mode), Tracked(control)) = lifecycle::Instance::initialize(sealed);
            let tracked (Tracked(frozen), Tracked(frozen_state), Tracked(lease)) = frozen::Instance::initialize(instance.id(), lifecycle.id(), None);
            let atomic = $atomic::new(Ghost((instance.id(), lifecycle.id(), frozen.id())), raw, Tracked(AtomicState { active: Some(active), sealed: mode, frozen: frozen_state }));
            (Counter { atomic, instance: Tracked(instance), lifecycle: Tracked(lifecycle), frozen: Tracked(frozen) }, Tracked(control))
        }
        pub closed spec fn accepts_lease(&self, lease: DrainLease) -> bool {
            lease.inv() && lease.instance == self.frozen@
        }
        pub fn try_drain(&self, Tracked(control): Tracked<lifecycle::control>) -> (result: Result<Tracked<DrainLease>, Tracked<lifecycle::control>>)
            requires self.inv(), control.instance_id() == self.authority_id(), control.value(),
            ensures match result {
                Ok(lease) => lease@.inv() && self.accepts_lease(lease@) && lease@.gate_id() == self.id() && lease@.authority_id() == self.authority_id(),
                Err(returned) => returned@ == control,
            },
        {
            let tracked mut remaining = Some(control);
            let tracked mut leased = None;
            let raw = atomic_with_ghost!(self.atomic => load(); returning raw; ghost state => {
                if raw & $mask == 0 {
                    let tracked mut control = remaining.tracked_take();
                    if state.active.is_none() {
                        let tracked held = self.frozen.borrow().guard_state(state.frozen.value().unwrap(), &state.frozen);
                        control.unique(&held.control);
                        assert(false);
                    }
                    self.lifecycle.borrow().agrees(&state.sealed, &control);
                    let tracked payload = FrozenResources { active: state.active.tracked_take(), control };
                    let tracked ticket = self.frozen.borrow().freeze(payload, &mut state.frozen, payload);
                    leased = Some(DrainLease { instance: self.frozen.borrow().clone(), ticket, gate: self.instance.borrow().clone() });
                }
            });
            if raw & $mask == 0 { Ok(Tracked(leased.tracked_unwrap())) }
            else { Err(Tracked(remaining.tracked_unwrap())) }
        }
        pub fn restore(&self, Tracked(lease): Tracked<DrainLease>) -> (control: Tracked<lifecycle::control>)
            requires self.inv(), self.accepts_lease(lease),
            ensures control@.instance_id() == self.authority_id(), control@.value(),
        {
            let tracked mut returned = None;
            atomic_with_ghost!(self.atomic => no_op(); ghost state => {
                self.frozen.borrow().leased(lease.ticket.value(), &state.frozen, &lease.ticket);
                let tracked held = self.frozen.borrow().guard_lease(lease.ticket.value(), &lease.ticket);
                let tracked payload = self.frozen.borrow().thaw(lease.ticket.value(), &mut state.frozen, lease.ticket);
                state.active = Some(payload.active);
                returned = Some(payload.control);
            });
            Tracked(returned.tracked_unwrap())
        }
        pub fn seal(&self, Tracked(control): Tracked<&mut lifecycle::control>)
            requires self.inv(), old(control).instance_id() == self.authority_id(),
            ensures final(control).instance_id() == self.authority_id(), final(control).value(),
        {
            atomic_with_ghost!(self.atomic => fetch_or($sealed); update raw -> next; ghost state => {
                assert((raw | $sealed) & $mask == raw & $mask) by(bit_vector);
                assert((raw | $sealed) & $sealed != 0) by(bit_vector);
                if state.active.is_none() {
                    let tracked held = self.frozen.borrow().guard_state(state.frozen.value().unwrap(), &state.frozen);
                    control.unique(&held.control);
                    assert(false);
                }
                self.lifecycle.borrow().set(true, &mut state.sealed, control);
            });
        }
        #[verifier::exec_allows_no_decreases_clause]
        pub fn reopen(&self, Tracked(control): Tracked<&mut lifecycle::control>) -> (opened: bool)
            requires self.inv(), old(control).instance_id() == self.authority_id(),
            ensures final(control).instance_id() == self.authority_id(),
                opened ==> !final(control).value(), !opened ==> *final(control) == *old(control),
        {
            loop
                invariant self.inv(), control.instance_id() == self.authority_id(), *control == *old(control),
            {
                let raw = atomic_with_ghost!(self.atomic => load(); ghost state => {});
                match $reopen(raw) {
                    TransitionOutcome::Success(next) => {
                        let result = atomic_with_ghost!(self.atomic => compare_exchange(raw, next); returning result; ghost state => {
                            if result is Ok {
                                assert(((0 as $word) & $mask) == 0 && ((0 as $word) & $sealed) == 0) by(bit_vector);
                                if state.active.is_none() {
                                    let tracked held = self.frozen.borrow().guard_state(state.frozen.value().unwrap(), &state.frozen);
                                    control.unique(&held.control);
                                    assert(false);
                                }
                                self.lifecycle.borrow().set(false, &mut state.sealed, control);
                            }
                        });
                        if result.is_ok() { return true; }
                    },
                    _ => { return false; },
                }
            }
        }
        pub fn observe_control(&self, Tracked(control): Tracked<&lifecycle::control>) -> (raw: $word)
            requires self.inv(), control.instance_id() == self.authority_id(),
            ensures (raw & $sealed != 0) == control.value(),
        {
            atomic_with_ghost!(self.atomic => load(); ghost state => {
                self.lifecycle.borrow().agrees(&state.sealed, control);
            })
        }
        #[verifier::exec_allows_no_decreases_clause]
        fn acquire_observing(&self, Tracked(control): Tracked<Option<&lifecycle::control>>) -> (result: (TransitionOutcome<$word>, Tracked<Option<admission::permits>>))
            requires self.inv(), control.is_some() ==> control.unwrap().instance_id() == self.authority_id(),
            ensures control.is_some() && control.unwrap().value() ==> result.0 == TransitionOutcome::Rejected,
                match result.0 {
                TransitionOutcome::Success(_) => result.1@.is_some() && result.1@.unwrap().instance_id() == self.id(),
                _ => result.1@.is_none(),
            },
        {
            loop
                invariant self.inv(), control.is_some() ==> control.unwrap().instance_id() == self.authority_id(),
            {
                let raw = atomic_with_ghost!(self.atomic => load(); ghost state => {
                    if control.is_some() { self.lifecycle.borrow().agrees(&state.sealed, *control.tracked_borrow()); }
                });
                match $acquire(raw) {
                    TransitionOutcome::Success(next) => {
                        let tracked mut permit = None;
                        let result = atomic_with_ghost!(self.atomic => compare_exchange(raw, next); returning result; ghost state => {
                            if result is Ok {
                                assert((raw & $mask != $mask && next == raw.wrapping_add(1)) ==>
                                    ((next & $mask) == (raw & $mask) + 1 && (next & $sealed) == (raw & $sealed))) by(bit_vector);
                                let tracked mut active = state.active.tracked_take();
                                permit = Some(self.instance.borrow().acquire(&mut active));
                                state.active = Some(active);
                            }
                        });
                        if result.is_ok() { return (TransitionOutcome::Success(next), Tracked(permit)); }
                    },
                    TransitionOutcome::Rejected => return (TransitionOutcome::Rejected, Tracked(None)),
                    TransitionOutcome::FailStop => return (TransitionOutcome::FailStop, Tracked(None)),
                }
            }
        }
        #[verifier::exec_allows_no_decreases_clause]
        pub fn acquire(&self) -> (result: (TransitionOutcome<$word>, Tracked<Option<admission::permits>>))
            requires self.inv(),
            ensures match result.0 {
                TransitionOutcome::Success(_) => result.1@.is_some() && result.1@.unwrap().instance_id() == self.id(),
                _ => result.1@.is_none(),
            },
        { self.acquire_observing(Tracked(None)) }
        #[verifier::exec_allows_no_decreases_clause]
        pub fn acquire_controlled(&self, Tracked(control): Tracked<&lifecycle::control>)
            -> (result: (TransitionOutcome<$word>, Tracked<Option<admission::permits>>))
            requires self.inv(), control.instance_id() == self.authority_id(),
            ensures control.value() ==> result.0 == TransitionOutcome::Rejected,
                match result.0 {
                    TransitionOutcome::Success(_) => result.1@.is_some() && result.1@.unwrap().instance_id() == self.id(),
                    _ => result.1@.is_none(),
                },
        { self.acquire_observing(Tracked(Some(control))) }
        #[verifier::exec_allows_no_decreases_clause]
        pub fn acquire_scope(&self) -> (result: (TransitionOutcome<$word>, Tracked<Option<super::super::permit_shares::Scope>>))
            requires self.inv(),
            ensures match result.0 {
                TransitionOutcome::Success(_) => result.1@.is_some() && result.1@.unwrap().inv()
                    && result.1@.unwrap().gate_id() == self.id() && result.1@.unwrap().len() == 0,
                _ => result.1@.is_none(),
            },
        {
            let (outcome, Tracked(permit)) = self.acquire();
            let tracked scope = if permit.is_some() {
                Some(super::super::permit_shares::Scope::new(permit.tracked_unwrap()))
            } else { None };
            (outcome, Tracked(scope))
        }
        pub fn observe_with_share(&self, Tracked(share): Tracked<&super::super::permit_shares::Share>) -> (raw: $word)
            requires self.inv(), share.inv(), share.gate_id() == self.id(),
            ensures raw & $mask > 0,
        {
            atomic_with_ghost!(self.atomic => load(); ghost state => {
                if state.active.is_none() {
                    let tracked held = self.frozen.borrow().guard_state(state.frozen.value().unwrap(), &state.frozen);
                    share.positive(self.instance.borrow(), &held.active);
                    assert(false);
                }
                share.positive(self.instance.borrow(), state.active.tracked_borrow());
            })
        }
        #[verifier::exec_allows_no_decreases_clause]
        pub fn release_scope(&self, Tracked(scope): Tracked<super::super::permit_shares::Scope>) -> (next: $word)
            requires self.inv(), scope.inv(), scope.gate_id() == self.id(), scope.len() == 0,
        {
            let tracked permit = scope.close();
            self.release(Tracked(permit))
        }
        #[verifier::exec_allows_no_decreases_clause]
        pub fn release(&self, Tracked(permit): Tracked<admission::permits>) -> (next: $word)
            requires self.inv(), permit.instance_id() == self.id(),
        {
            let tracked mut pending = Some(permit);
            loop
                invariant self.inv(), pending.is_some(), pending.unwrap().instance_id() == self.id(),
            {
                let raw = atomic_with_ghost!(self.atomic => load(); returning raw; ghost state => {
                    if state.active.is_none() {
                        let tracked held = self.frozen.borrow().guard_state(state.frozen.value().unwrap(), &state.frozen);
                        self.instance.borrow().positive(&held.active, pending.tracked_borrow());
                        assert(false);
                    }
                    self.instance.borrow().positive(state.active.tracked_borrow(), pending.tracked_borrow());
                });
                match $release(raw) {
                    TransitionOutcome::Success(next) => {
                        let result = atomic_with_ghost!(self.atomic => compare_exchange(raw, next); returning result; ghost state => {
                            if result is Ok {
                                assert((raw & $mask > 0 && next == raw.wrapping_sub(1)) ==>
                                    ((next & $mask) == (raw & $mask) - 1 && (next & $sealed) == (raw & $sealed))) by(bit_vector);
                                let tracked mut active = state.active.tracked_take();
                                self.instance.borrow().release(&mut active, pending.tracked_take());
                                state.active = Some(active);
                            }
                        });
                        if result.is_ok() { return next; }
                    },
                    _ => { assert(false); },
                }
            }
        }
    }
    }
    }
};
}
width!(word32, u32, AtomicU32, ACTIVE_COUNT_MASK_32, SEALED_BIT_32, acquire_step_32, release_step_32, reopen_step_32);
width!(word64, u64, AtomicU64, ACTIVE_COUNT_MASK_64, SEALED_BIT_64, acquire_step_64, release_step_64, reopen_step_64);

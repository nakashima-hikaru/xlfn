//! Shared publication order with actual counters and lock-owned lifecycle tokens.
use vstd::prelude::*;
use vstd::rwlock::{RwLock, RwLockPredicate, WriteHandle};
use super::current_atomic::{Current, ReservedQueue};
use super::barrier_ownership::{QueueLock, QueueHandle};
use super::queue_preparation::phase;
use super::drain::atomic_counter::lifecycle;
macro_rules! width {
    ($module:ident) => {
    pub mod $module {
    use super::*;
    use super::super::drain::atomic_counter::$module::Counter;
    verus! {
    pub struct State { zero: Tracked<lifecycle::control>, one: Tracked<lifecycle::control>, pending: Option<bool> }
    impl State {
        pub closed spec fn sealed(&self, index: bool) -> bool { if index { self.one@.value() } else { self.zero@.value() } }
        pub closed spec fn pending(&self) -> Option<bool> { self.pending }
    }
    pub struct Predicate { pub domain: *const u8, pub zero: vstd::tokens::InstanceId, pub one: vstd::tokens::InstanceId }
    impl RwLockPredicate<State> for Predicate {
        closed spec fn inv(self, state: State) -> bool {
            state.zero@.instance_id() == self.zero && state.one@.instance_id() == self.one
            && (state.pending.is_some() ==> state.sealed(state.pending.unwrap()))
        }
    }
    pub type TransitionLock = RwLock<State, Predicate>;
    pub type TransitionHandle<'a> = WriteHandle<'a, State, Predicate>;
    pub fn new(domain: *const u8, zero: &Counter, one: &Counter,
        Tracked(zero_control): Tracked<lifecycle::control>, Tracked(one_control): Tracked<lifecycle::control>) -> (lock: TransitionLock)
        requires zero.inv(), one.inv(), zero_control.instance_id() == zero.authority_id(), one_control.instance_id() == one.authority_id(),
        ensures lock.pred().domain == domain, lock.pred().zero == zero.authority_id(), lock.pred().one == one.authority_id(),
    {
        RwLock::new(State { zero: Tracked(zero_control), one: Tracked(one_control), pending: None },
            Ghost(Predicate { domain, zero: zero.authority_id(), one: one.authority_id() }))
    }
    fn seal(state: &mut State, lock: &TransitionLock, index: bool, zero: &Counter, one: &Counter)
        requires lock.inv(*old(state)), zero.inv(), one.inv(), lock.pred().zero == zero.authority_id(), lock.pred().one == one.authority_id(),
        ensures lock.inv(*final(state)), final(state).pending() == old(state).pending(), final(state).sealed(index),
            final(state).sealed(!index) == old(state).sealed(!index),
    {
        if index { one.seal(Tracked(state.one.borrow_mut())); }
        else { zero.seal(Tracked(state.zero.borrow_mut())); }
    }
    fn mark_pending(state: &mut State, lock: &TransitionLock, index: bool)
        requires lock.inv(*old(state)), old(state).pending().is_none(), old(state).sealed(index),
        ensures lock.inv(*final(state)), final(state).pending() == Some(index),
            final(state).sealed(false) == old(state).sealed(false), final(state).sealed(true) == old(state).sealed(true),
    { state.pending = Some(index); }
    fn publish<P>(current: &Current, reserved: ReservedQueue<P>, queue_lock: &QueueLock<P>, queue_handle: &QueueHandle<'_, P>,
        state: &State, transition: &TransitionLock, handle: &TransitionHandle<'_>, Tracked(ready): Tracked<phase::ready>)
        -> (result: (super::super::barrier_ownership::QueueState<P>, Tracked<phase::prepared>))
        requires current.inv(), reserved.inv(current, queue_lock), queue_handle.rwlock() == *queue_lock,
            ready.instance_id() == current.gate(!reserved.index()), lock_matches(current, transition), transition.inv(*state), handle.rwlock() == *transition,
            state.pending() == Some(reserved.index()), state.sealed(false), state.sealed(true),
        ensures queue_lock.inv(result.0), result.0.index == reserved.index(), result.0.records@ == reserved.records(),
            result.1@.instance_id() == queue_lock.pred().preparation.id(), result.1@.value() == reserved.bound(),
    { current.publish_reserved(reserved, queue_lock, queue_handle, Tracked(ready)) }
    pub open spec fn lock_matches(current: &Current, lock: &TransitionLock) -> bool { current.owner() == lock.pred().domain }
    #[verifier::exec_allows_no_decreases_clause]
    fn reopen_next(state: &mut State, lock: &TransitionLock, index: bool, zero: &Counter, one: &Counter)
        requires lock.inv(*old(state)), old(state).pending() == Some(index), old(state).sealed(!index),
            zero.inv(), one.inv(), lock.pred().zero == zero.authority_id(), lock.pred().one == one.authority_id(),
        ensures lock.inv(*final(state)), final(state).pending() == Some(index), !final(state).sealed(!index), final(state).sealed(index),
    {
        let opened = if index { zero.reopen(Tracked(state.zero.borrow_mut())) } else { one.reopen(Tracked(state.one.borrow_mut())) };
        // Native reopen failure is fail-stop; model its non-returning branch.
        if !opened { loop {} }
    }
    #[verifier::exec_allows_no_decreases_clause]
    pub fn begin<P>(state: &mut State, transition: &TransitionLock, handle: &TransitionHandle<'_>,
        current: &Current, index: bool, zero: &Counter, one: &Counter,
        reserved: ReservedQueue<P>, queue_lock: &QueueLock<P>, queue_handle: QueueHandle<'_, P>, Tracked(ready): Tracked<phase::ready>)
        -> (ticket: Tracked<phase::prepared>)
        requires transition.inv(*old(state)), handle.rwlock() == *transition, old(state).pending().is_none(), old(state).sealed(!index),
            zero.inv(), one.inv(), transition.pred().zero == zero.authority_id(), transition.pred().one == one.authority_id(),
            current.inv(), lock_matches(current, transition), reserved.inv(current, queue_lock), reserved.index() == index,
            queue_handle.rwlock() == *queue_lock, ready.instance_id() == current.gate(!index),
        ensures transition.inv(*final(state)), final(state).pending() == Some(index), final(state).sealed(index), !final(state).sealed(!index),
            ticket@.instance_id() == queue_lock.pred().preparation.id(), ticket@.value() == reserved.bound(),
    {
        let ready = Tracked(ready);
        let mut published = None;
        let mut ticket = None;
        super::super::protocol::begin_rotation!(
            seal(state, transition, index, zero, one),
            mark_pending(state, transition, index),
            super::super::protocol::publish_release!(
                super::super::protocol::publish_reopen!(
                    published = Some(publish(current, reserved, queue_lock, &queue_handle, state, transition, handle, ready)),
                    (),
                    vstd::prelude::verus_exec_expr!({
                        assert(published.is_some());
                        reopen_next(state, transition, index, zero, one);
                    })
                ),
                vstd::prelude::verus_exec_expr!({
                    let (queue, prepared) = published.unwrap();
                    queue_handle.release_write(queue);
                    ticket = Some(prepared);
                })
            )
        );
        ticket.unwrap()
    }
    }
    }
};
}
width!(word32);
width!(word64);

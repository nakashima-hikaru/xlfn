//! Transition-lock ownership for arbitrary stripe groups and actual publication.
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
    use super::super::drain::atomic_stripes::$module as stripes;
    verus! {
    pub struct State {
        zero: Tracked<Map<nat, lifecycle::control>>, one: Tracked<Map<nat, lifecycle::control>>,
        pending: Option<bool>,
    }
    impl State {
        pub closed spec fn pending(&self) -> Option<bool> { self.pending }
        pub closed spec fn controls(&self, index: bool) -> Map<nat, lifecycle::control> {
            if index { self.one@ } else { self.zero@ }
        }
        pub open spec fn sealed(&self, counters: Seq<Counter>, index: bool) -> bool {
            stripes::controls_match(counters, self.controls(index))
        }
        pub open spec fn opened(&self, counters: Seq<Counter>, index: bool) -> bool {
            forall|i: int| 0 <= i < counters.len() ==> #[trigger] stripes::reopen_row(counters, self.controls(index), i, counters.len())
        }
    }
    pub struct Predicate { pub domain: *const u8, pub zero: Seq<Counter>, pub one: Seq<Counter> }
    impl Predicate {
        pub open spec fn counters(&self, index: bool) -> Seq<Counter> { if index { self.one } else { self.zero } }
    }
    impl RwLockPredicate<State> for Predicate {
        closed spec fn inv(self, state: State) -> bool {
            stripes::controls_owned(self.zero, state.zero@) && stripes::controls_owned(self.one, state.one@)
            && stripes::distinct(self.zero + self.one)
            && (forall|i: int| 0 <= i < self.zero.len() ==> (#[trigger] self.zero[i]).inv())
            && (forall|i: int| 0 <= i < self.one.len() ==> (#[trigger] self.one[i]).inv())
            && (state.pending.is_some() ==> state.sealed(self.counters(state.pending.unwrap()), state.pending.unwrap()))
        }
    }
    pub type TransitionLock = RwLock<State, Predicate>;
    pub type TransitionHandle<'a> = WriteHandle<'a, State, Predicate>;
    pub fn new(domain: *const u8, zero: &Vec<Counter>, one: &Vec<Counter>,
        Tracked(zero_controls): Tracked<Map<nat, lifecycle::control>>, Tracked(one_controls): Tracked<Map<nat, lifecycle::control>>)
        -> (lock: TransitionLock)
        requires stripes::controls_owned(zero@, zero_controls), stripes::controls_owned(one@, one_controls), stripes::distinct(zero@ + one@),
            forall|i: int| 0 <= i < zero.len() ==> (#[trigger] zero@[i]).inv(),
            forall|i: int| 0 <= i < one.len() ==> (#[trigger] one@[i]).inv(),
        ensures lock.pred().domain == domain, lock.pred().zero == zero@, lock.pred().one == one@,
    {
        RwLock::new(State { zero: Tracked(zero_controls), one: Tracked(one_controls), pending: None },
            Ghost(Predicate { domain, zero: zero@, one: one@ }))
    }
    /// Own the other generation while its replacement's controllers are in
    /// collection custody. Borrowing the real handle prevents early unlock.
    pub struct CollectionHandoff<'a> {
        transition: &'a TransitionLock, handle: &'a TransitionHandle<'a>, index: bool,
        old_controls: Tracked<Map<nat, lifecycle::control>>, original: Ghost<State>,
    }
    impl<'a> CollectionHandoff<'a> {
        pub closed spec fn inv(&self) -> bool {
            self.handle.rwlock() == *self.transition && self.transition.inv(self.original@)
                && self.original@.pending().is_none()
                && self.old_controls@ == self.original@.controls(self.index)
                && self.original@.sealed(self.next(), !self.index)
        }
        pub closed spec fn index(&self) -> bool { self.index }
        pub closed spec fn next(&self) -> Seq<Counter> { self.transition.pred().counters(!self.index) }
        pub closed spec fn original(&self) -> State { self.original@ }
        pub closed spec fn lock(&self) -> TransitionLock { *self.transition }
        pub fn cancel(self, mut collection: stripes::Collection, counters: &Vec<Counter>) -> (state: State)
            requires self.inv(), counters@ == self.next(), collection.inv(counters@),
            ensures self.lock().inv(state), state.pending().is_none(), state.sealed(self.next(), !self.index()),
                state.controls(self.index()) == self.original().controls(self.index()),
        {
            collection.restore_all(counters);
            let controllers = collection.into_controls(counters);
            self.restore(controllers)
        }
        pub fn restore(self, Tracked(controls): Tracked<Map<nat, lifecycle::control>>) -> (state: State)
            requires self.inv(), stripes::controls_match(self.next(), controls),
            ensures self.lock().inv(state), state.pending().is_none(), state.sealed(self.next(), !self.index()),
                state.controls(self.index()) == self.original().controls(self.index()),
        {
            assert forall|i: int| 0 <= i < self.next().len() implies #[trigger] stripes::control_row(self.next(), controls, i) by {
                assert(controls.dom().contains(i as nat));
            };
            let CollectionHandoff { transition: _, handle: _, index, old_controls, original: _ } = self;
            if index { State { zero: Tracked(controls), one: old_controls, pending: None } }
            else { State { zero: old_controls, one: Tracked(controls), pending: None } }
        }
    }
    pub fn collect_next<'a>(state: State, transition: &'a TransitionLock, handle: &'a TransitionHandle<'a>,
        index: bool, next: &Vec<Counter>) -> (result: (CollectionHandoff<'a>, stripes::Collection))
        requires transition.inv(state), handle.rwlock() == *transition, state.pending().is_none(),
            next@ == transition.pred().counters(!index), state.sealed(next@, !index),
        ensures result.0.inv(), result.0.index() == index, result.0.next() == next@,
            result.0.original() == state, result.0.lock() == *transition, result.1.inv(next@),
    {
        let ghost original = state;
        assert forall|i: int, j: int| 0 <= i < next.len() && 0 <= j < next.len() && i != j
            implies (#[trigger] next@[i].id()) != (#[trigger] next@[j].id()) by {
            let all = transition.pred().zero + transition.pred().one;
            let offset = if index { 0int } else { transition.pred().zero.len() as int };
            assert(all[i + offset] == next@[i]);
            assert(all[j + offset] == next@[j]);
        };
        let State { zero, one, pending: _ } = state;
        let (old_controls, next_controls) = if index { (one, zero) } else { (zero, one) };
        let collection = stripes::Collection::new(next, next_controls);
        (CollectionHandoff { transition, handle, index, old_controls, original: Ghost(original) }, collection)
    }
    pub struct PendingHandoff<'a> {
        transition: &'a TransitionLock, handle: &'a TransitionHandle<'a>, index: bool,
        live_controls: Tracked<Map<nat, lifecycle::control>>, original: Ghost<State>,
    }
    impl<'a> PendingHandoff<'a> {
        pub closed spec fn inv(&self) -> bool {
            self.handle.rwlock() == *self.transition && self.transition.inv(self.original@)
                && self.original@.pending() == Some(self.index)
                && self.live_controls@ == self.original@.controls(!self.index)
        }
        pub closed spec fn index(&self) -> bool { self.index }
        pub closed spec fn counters(&self) -> Seq<Counter> { self.transition.pred().counters(self.index) }
        pub closed spec fn original(&self) -> State { self.original@ }
        pub closed spec fn lock(&self) -> TransitionLock { *self.transition }
        pub fn finish<P>(self, mut collection: stripes::Collection, counters: &Vec<Counter>,
            current: &Current, queue: &QueueLock<P>, Tracked(ready): Tracked<&phase::ready>) -> (state: State)
            requires self.inv(), counters@ == self.counters(), collection.inv(counters@),
                forall|i: int| 0 <= i < counters.len() ==> collection.ready(i),
                current.inv(), current.owner() == self.lock().pred().domain,
                queue.pred().domain == current.owner(), queue.pred().index == self.index(),
                queue.pred().preparation.id() == current.gate(self.index()), ready.instance_id() == queue.pred().preparation.id(),
            ensures self.lock().inv(state), state.pending().is_none(), state.sealed(self.counters(), self.index()),
                state.controls(!self.index()) == self.original().controls(!self.index()),
        {
            collection.restore_all(counters);
            let mut state = self.restore(collection.into_controls(counters));
            state.pending = None;
            state
        }
        /// Restoring controllers alone does not complete the pending callback.
        pub fn restore(self, Tracked(controls): Tracked<Map<nat, lifecycle::control>>) -> (state: State)
            requires self.inv(), stripes::controls_match(self.counters(), controls),
            ensures self.lock().inv(state), state.pending() == Some(self.index()), state.sealed(self.counters(), self.index()),
                state.controls(!self.index()) == self.original().controls(!self.index()),
        {
            assert forall|i: int| 0 <= i < self.counters().len() implies #[trigger] stripes::control_row(self.counters(), controls, i) by {
                assert(controls.dom().contains(i as nat));
            };
            let PendingHandoff { transition: _, handle: _, index, live_controls, original: _ } = self;
            if index { State { zero: live_controls, one: Tracked(controls), pending: Some(index) } }
            else { State { zero: Tracked(controls), one: live_controls, pending: Some(index) } }
        }
        pub fn cancel(self, mut collection: stripes::Collection, counters: &Vec<Counter>) -> (state: State)
            requires self.inv(), counters@ == self.counters(), collection.inv(counters@),
            ensures self.lock().inv(state), state.pending() == Some(self.index()), state.sealed(self.counters(), self.index()),
                state.controls(!self.index()) == self.original().controls(!self.index()),
        {
            collection.restore_all(counters);
            self.restore(collection.into_controls(counters))
        }
    }
    pub fn collect_pending<'a>(state: State, transition: &'a TransitionLock, handle: &'a TransitionHandle<'a>,
        index: bool, counters: &Vec<Counter>) -> (result: (PendingHandoff<'a>, stripes::Collection))
        requires transition.inv(state), handle.rwlock() == *transition, state.pending() == Some(index),
            counters@ == transition.pred().counters(index),
        ensures result.0.inv(), result.0.index() == index, result.0.counters() == counters@,
            result.0.original() == state, result.0.lock() == *transition, result.1.inv(counters@),
    {
        let ghost original = state;
        assert forall|i: int, j: int| 0 <= i < counters.len() && 0 <= j < counters.len() && i != j
            implies (#[trigger] counters@[i].id()) != (#[trigger] counters@[j].id()) by {
            let all = transition.pred().zero + transition.pred().one;
            let offset = if index { transition.pred().zero.len() as int } else { 0int };
            assert(all[i + offset] == counters@[i]);
            assert(all[j + offset] == counters@[j]);
        };
        let State { zero, one, pending: _ } = state;
        let (live_controls, pending_controls) = if index { (zero, one) } else { (one, zero) };
        let collection = stripes::Collection::new(counters, pending_controls);
        (PendingHandoff { transition, handle, index, live_controls, original: Ghost(original) }, collection)
    }
    fn seal(state: &mut State, lock: &TransitionLock, index: bool, zero: &Vec<Counter>, one: &Vec<Counter>)
        requires lock.inv(*old(state)), zero@ == lock.pred().zero, one@ == lock.pred().one,
        ensures lock.inv(*final(state)), final(state).pending() == old(state).pending(),
            final(state).sealed(lock.pred().counters(index), index), final(state).controls(!index) == old(state).controls(!index),
    {
        if index { stripes::seal_all(one, Tracked(state.one.borrow_mut())); }
        else { stripes::seal_all(zero, Tracked(state.zero.borrow_mut())); }
    }
    fn mark_pending(state: &mut State, lock: &TransitionLock, index: bool)
        requires lock.inv(*old(state)), old(state).pending().is_none(), old(state).sealed(lock.pred().counters(index), index),
        ensures lock.inv(*final(state)), final(state).pending() == Some(index),
            final(state).controls(false) == old(state).controls(false), final(state).controls(true) == old(state).controls(true),
    { state.pending = Some(index); }
    fn publish<P>(current: &Current, reserved: ReservedQueue<P>, queue: &QueueLock<P>, queue_handle: &QueueHandle<'_, P>,
        state: &State, transition: &TransitionLock, handle: &TransitionHandle<'_>, Tracked(ready): Tracked<phase::ready>)
        -> (result: (super::super::barrier_ownership::QueueState<P>, Tracked<phase::prepared>))
        requires current.inv(), reserved.inv(current, queue), queue_handle.rwlock() == *queue,
            transition.inv(*state), handle.rwlock() == *transition, current.owner() == transition.pred().domain,
            reserved.bound() == stripes::gate_ids(transition.pred().counters(reserved.index())),
            state.pending() == Some(reserved.index()), state.sealed(transition.pred().zero, false), state.sealed(transition.pred().one, true),
            ready.instance_id() == current.gate(!reserved.index()),
        ensures queue.inv(result.0), result.0.records@ == reserved.records(), result.0.index == reserved.index(),
            result.1@.instance_id() == queue.pred().preparation.id(), result.1@.value() == reserved.bound(),
    { current.publish_reserved(reserved, queue, queue_handle, Tracked(ready)) }
    #[verifier::exec_allows_no_decreases_clause]
    fn reopen_next(state: &mut State, lock: &TransitionLock, index: bool, zero: &Vec<Counter>, one: &Vec<Counter>)
        requires lock.inv(*old(state)), old(state).pending() == Some(index), old(state).sealed(lock.pred().counters(!index), !index),
            zero@ == lock.pred().zero, one@ == lock.pred().one,
        ensures lock.inv(*final(state)), final(state).pending() == Some(index),
            final(state).opened(lock.pred().counters(!index), !index), final(state).sealed(lock.pred().counters(index), index),
            final(state).controls(index) == old(state).controls(index),
    {
        let opened = if index { stripes::reopen_all(zero, Tracked(state.zero.borrow_mut())) }
            else { stripes::reopen_all(one, Tracked(state.one.borrow_mut())) };
        let len = if index { zero.len() } else { one.len() };
        if opened != len { loop {} }
        let ghost counters = lock.pred().counters(!index);
        let ghost controls = state.controls(!index);
        assert forall|i: int| 0 <= i < counters.len() implies #[trigger] stripes::control_row(counters, controls, i) by {
            assert(stripes::reopen_row(counters, controls, i, counters.len()));
        };
    }
    #[verifier::exec_allows_no_decreases_clause]
    pub fn begin<P>(state: &mut State, transition: &TransitionLock, handle: &TransitionHandle<'_>, current: &Current,
        index: bool, zero: &Vec<Counter>, one: &Vec<Counter>, reserved: ReservedQueue<P>, queue: &QueueLock<P>,
        queue_handle: QueueHandle<'_, P>, Tracked(ready): Tracked<phase::ready>) -> (ticket: Tracked<phase::prepared>)
        requires transition.inv(*old(state)), handle.rwlock() == *transition, old(state).pending().is_none(),
            old(state).sealed(transition.pred().counters(!index), !index), zero@ == transition.pred().zero, one@ == transition.pred().one,
            current.inv(), current.owner() == transition.pred().domain, reserved.inv(current, queue), reserved.index() == index,
            reserved.bound() == stripes::gate_ids(transition.pred().counters(index)),
            queue_handle.rwlock() == *queue, ready.instance_id() == current.gate(!index),
        ensures transition.inv(*final(state)), final(state).pending() == Some(index),
            final(state).sealed(transition.pred().counters(index), index), final(state).opened(transition.pred().counters(!index), !index),
            ticket@.instance_id() == queue.pred().preparation.id(), ticket@.value() == reserved.bound(),
    {
        let ready = Tracked(ready);
        let mut published = None;
        let mut ticket = None;
        super::super::protocol::begin_rotation!(
            seal(state, transition, index, zero, one),
            mark_pending(state, transition, index),
            super::super::protocol::publish_release!(
                super::super::protocol::publish_reopen!(
                    published = Some(publish(current, reserved, queue, &queue_handle, state, transition, handle, ready)), (),
                    vstd::prelude::verus_exec_expr!({
                        assert(published.is_some());
                        reopen_next(state, transition, index, zero, one);
                    })
                ),
                vstd::prelude::verus_exec_expr!({
                    let (contents, prepared) = published.unwrap();
                    queue_handle.release_write(contents);
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

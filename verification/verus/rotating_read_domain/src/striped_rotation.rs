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
    use super::super::locked_detachment::Withdrawal;
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
    /// Hold the matching transition handle while the open generation's
    /// controllers move into zero-count leases. The state cannot be returned
    /// to the lock until failure has rolled back or success has completed its
    /// protected callback and restored the controllers.
    pub struct IdleHandoff<'a> {
        transition: &'a TransitionLock, handle: &'a TransitionHandle<'a>, index: bool,
        other_controls: Tracked<Map<nat, lifecycle::control>>, original: Ghost<State>,
    }
    impl<'a> IdleHandoff<'a> {
        pub closed spec fn inv(&self) -> bool {
            self.handle.rwlock() == *self.transition && self.transition.inv(self.original@)
                && self.original@.pending().is_none()
                && self.original@.opened(self.target(), self.index)
                && self.original@.sealed(self.other(), !self.index)
                && self.other_controls@ == self.original@.controls(!self.index)
        }
        pub closed spec fn index(&self) -> bool { self.index }
        pub closed spec fn target(&self) -> Seq<Counter> { self.transition.pred().counters(self.index) }
        pub closed spec fn other(&self) -> Seq<Counter> { self.transition.pred().counters(!self.index) }
        pub closed spec fn lock(&self) -> TransitionLock { *self.transition }
        pub closed spec fn original(&self) -> State { self.original@ }
        pub fn restore_open(self, Tracked(controls): Tracked<Map<nat, lifecycle::control>>) -> (state: State)
            requires self.inv(), stripes::open_controls_match(self.target(), controls),
            ensures self.lock().inv(state), state.pending().is_none(),
                state.opened(self.target(), self.index()), state.sealed(self.other(), !self.index()),
                state.controls(!self.index()) == self.original().controls(!self.index()),
        {
            let IdleHandoff { transition: _, handle: _, index, other_controls, original: _ } = self;
            if index { State { zero: other_controls, one: Tracked(controls), pending: None } }
            else { State { zero: Tracked(controls), one: other_controls, pending: None } }
        }
        pub fn attempt(self, mut collection: stripes::IdleCollection, counters: &Vec<Counter>)
            -> (result: Result<(Self, stripes::IdleCollection), State>)
            requires self.inv(), counters@ == self.target(), collection.inv(counters@),
                forall|i: int| 0 <= i < counters.len() ==> !collection.ready(i),
            ensures match result {
                Ok((handoff, sealed)) => handoff.inv() && sealed.inv(counters@)
                    && (forall|i: int| 0 <= i < counters.len() ==> sealed.ready(i)),
                Err(state) => self.lock().inv(state) && state.pending().is_none()
                    && state.opened(self.target(), self.index())
                    && state.sealed(self.other(), !self.index()),
            },
        {
            if collection.try_seal_all(counters) {
                Ok((self, collection))
            } else {
                let controls = collection.into_controls(counters);
                Err(self.restore_open(controls))
            }
        }
        pub fn cancel(self, mut collection: stripes::IdleCollection, counters: &Vec<Counter>) -> (state: State)
            requires self.inv(), counters@ == self.target(), collection.inv(counters@),
                forall|i: int| 0 <= i < counters.len() ==> collection.ready(i),
            ensures self.lock().inv(state), state.pending().is_none(),
                state.opened(self.target(), self.index()), state.sealed(self.other(), !self.index()),
        {
            collection.undo_all(counters);
            self.restore_open(collection.into_controls(counters))
        }
        pub fn publish<'b, P>(self, collection: stripes::IdleCollection, counters: &Vec<Counter>,
            current: &'b Current, reserved: ReservedQueue<P>, queue: &'b QueueLock<P>,
            queue_handle: QueueHandle<'_, P>, Tracked(ready): Tracked<phase::ready>) -> (published: IdlePublished<'a, 'b, P>)
            requires self.inv(), counters@ == self.target(), collection.inv(counters@),
                forall|i: int| 0 <= i < counters.len() ==> collection.ready(i),
                current.inv(), current.owner() == self.lock().pred().domain,
                reserved.inv(current, queue), reserved.index() == self.index(),
                reserved.bound() == stripes::gate_ids(counters@),
                queue.pred().domain == current.owner(), queue.pred().index == self.index(),
                queue.pred().preparation.id() == current.gate(self.index()),
                queue_handle.rwlock() == *queue,
                ready.instance_id() == current.gate(!self.index()),
            ensures published.inv(), published.index() == self.index(),
                published.bound() == stripes::gate_ids(counters@),
        {
            let mut published = None;
            let mut prepared = None;
            super::super::protocol::publish_release!(
                vstd::prelude::verus_exec_expr!({
                    published = Some(current.publish_reserved(reserved, queue, &queue_handle, Tracked(ready)));
                }),
                vstd::prelude::verus_exec_expr!({
                    let (contents, ticket) = published.unwrap();
                    queue_handle.release_write(contents);
                    prepared = Some(ticket);
                })
            );
            IdlePublished { handoff: self, collection, current, queue, prepared: prepared.unwrap() }
        }
    }
    /// Publication has consumed the prepared old queue phase while all old
    /// stripe leases remain owned. A matching queue detachment can now run.
    #[verifier::reject_recursive_types(P)]
    pub struct IdlePublished<'a, 'b, P> {
        handoff: IdleHandoff<'a>, collection: stripes::IdleCollection,
        current: &'b Current, queue: &'b QueueLock<P>, prepared: Tracked<phase::prepared>,
    }
    impl<'a, 'b, P> IdlePublished<'a, 'b, P> {
        pub closed spec fn inv(&self) -> bool {
            self.handoff.inv() && self.collection.inv(self.handoff.target())
                && (forall|i: int| 0 <= i < self.handoff.target().len() ==> self.collection.ready(i))
                && self.current.inv() && self.current.owner() == self.handoff.lock().pred().domain
                && self.queue.pred().domain == self.current.owner()
                && self.queue.pred().index == self.handoff.index()
                && self.queue.pred().preparation.id() == self.current.gate(self.handoff.index())
                && self.prepared@.instance_id() == self.queue.pred().preparation.id()
                && self.prepared@.value() == stripes::gate_ids(self.handoff.target())
        }
        pub closed spec fn index(&self) -> bool { self.handoff.index() }
        pub closed spec fn target(&self) -> Seq<Counter> { self.handoff.target() }
        pub closed spec fn owner(&self) -> *const u8 { self.current.owner() }
        pub closed spec fn lock(&self) -> TransitionLock { self.handoff.lock() }
        pub closed spec fn other(&self) -> Seq<Counter> { self.handoff.other() }
        pub closed spec fn bound(&self) -> Set<vstd::tokens::InstanceId> { self.prepared@.value() }
        pub closed spec fn prepared_id(&self) -> vstd::tokens::InstanceId {
            self.queue.pred().preparation.id()
        }
        pub closed spec fn prepared_inv(&self) -> spec_fn(P, Set<vstd::tokens::InstanceId>) -> bool {
            self.queue.pred().prepared_inv
        }
        pub closed spec fn payload_inv(&self) -> spec_fn(P) -> bool { self.queue.pred().payload_inv }
        pub open spec fn callback_input(&self, detached: &IdleDetached<'a, 'b, P>, withdrawal: Withdrawal<P>) -> bool {
            detached.inv() && detached.index() == self.index() && detached.target() == self.target()
                && detached.lock() == self.lock() && detached.other() == self.other()
                && detached.prepared_id() == self.prepared_id()
                && detached.owner() == self.owner() && detached.bound() == self.bound()
                && detached.payload_inv() == self.payload_inv()
                && detached.prepared_inv() == self.prepared_inv()
                && withdrawal.inv() && withdrawal.owner() == self.owner() && withdrawal.index() == self.index()
                && (forall|i: int| 0 <= i < withdrawal.source().len() ==>
                    (self.payload_inv())(#[trigger] withdrawal.source()[i])
                    && (self.prepared_inv())(withdrawal.source()[i], self.bound()))
        }
        pub fn drains<'c>(&'c self, counters: &Vec<Counter>) -> (leases: Tracked<&'c super::super::drain::atomic_counter::DrainSet>)
            requires self.inv(), counters@ == self.target(),
            ensures leases@.inv(), leases@.domain() == self.bound(),
        { self.collection.drains(counters) }
        #[verifier::exec_allows_no_decreases_clause]
        pub fn detach(self) -> (result: (IdleDetached<'a, 'b, P>, super::super::locked_detachment::Withdrawal<P>))
            requires self.inv(),
            ensures result.0.inv(), result.0.index() == self.index(), result.0.target() == self.target(),
                result.0.lock() == self.lock(), result.0.other() == self.other(),
                result.0.prepared_id() == self.prepared_id(),
                result.1.inv(), result.1.owner() == self.owner(), result.1.index() == self.index(),
                result.0.bound() == self.bound(), result.0.owner() == self.owner(),
                result.0.prepared_inv() == self.prepared_inv(), result.0.payload_inv() == self.payload_inv(),
                forall|i: int| 0 <= i < result.1.source().len() ==>
                    (self.payload_inv())(#[trigger] result.1.source()[i]),
                forall|i: int| 0 <= i < result.1.source().len() ==>
                    (self.prepared_inv())(#[trigger] result.1.source()[i], self.bound()),
        {
            let IdlePublished { handoff, collection, current, queue, prepared: Tracked(prepared) } = self;
            let held = queue.acquire_write();
            let owner = held.0.domain;
            let tracked mut ticket = Some(prepared);
            let (withdrawal, ready) = super::super::locked_detachment::take_prepared(
                owner, held, queue, Tracked(&mut ticket));
            (IdleDetached { handoff, collection, current, queue, ready }, withdrawal)
        }
        #[verifier::exec_allows_no_decreases_clause]
        pub fn run_callback<R, F: for<'c> FnOnce(&'c IdleDetached<'a, 'b, P>, Withdrawal<P>) -> R>(
            self, counters: &Vec<Counter>, callback: F, Ghost(callback_post): Ghost<spec_fn(Seq<P>, R) -> bool>)
            -> (result: (State, Tracked<phase::ready>, R, Ghost<Seq<P>>))
            requires self.inv(), counters@ == self.target(),
                forall|detached: &IdleDetached<'a, 'b, P>, withdrawal: Withdrawal<P>|
                    self.callback_input(detached, withdrawal) ==>
                        call_requires(callback, (detached, withdrawal)),
                forall|detached: &IdleDetached<'a, 'b, P>, withdrawal: Withdrawal<P>, value: R|
                    (#[trigger] call_ensures(callback, (detached, withdrawal), value)) ==>
                        callback_post(withdrawal.source(), value),
            ensures self.lock().inv(result.0), result.0.pending().is_none(),
                result.0.sealed(self.target(), self.index()),
                result.0.sealed(self.other(), !self.index()),
                result.1@.instance_id() == self.prepared_id(),
                callback_post(result.3@, result.2),
                forall|i: int| 0 <= i < result.3@.len() ==>
                    (self.payload_inv())(#[trigger] result.3@[i])
                    && (self.prepared_inv())(result.3@[i], self.bound()),
        {
            let ghost original = self;
            let (detached, withdrawal) = self.detach();
            assert(original.callback_input(&detached, withdrawal));
            let ghost source = withdrawal.source();
            let mut restored = None;
            let value = super::super::protocol::finish_rotation!(value;
                vstd::prelude::verus_exec_expr!({ callback(&detached, withdrawal) }),
                vstd::prelude::verus_exec_expr!({
                    restored = Some(detached.restore_after_callback(counters));
                })
            );
            let (state, ready) = restored.unwrap();
            assert(original.lock().inv(state));
            assert(state.sealed(original.other(), !original.index()));
            (state, ready, value, Ghost(source))
        }
    }
    #[verifier::reject_recursive_types(P)]
    pub struct IdleDetached<'a, 'b, P> {
        handoff: IdleHandoff<'a>, collection: stripes::IdleCollection,
        current: &'b Current, queue: &'b QueueLock<P>, ready: Tracked<phase::ready>,
    }
    impl<'a, 'b, P> IdleDetached<'a, 'b, P> {
        pub closed spec fn inv(&self) -> bool {
            self.handoff.inv() && self.collection.inv(self.handoff.target())
                && (forall|i: int| 0 <= i < self.handoff.target().len() ==> self.collection.ready(i))
                && self.current.inv() && self.current.owner() == self.handoff.lock().pred().domain
                && self.queue.pred().domain == self.current.owner()
                && self.queue.pred().index == self.handoff.index()
                && self.queue.pred().preparation.id() == self.current.gate(self.handoff.index())
                && self.ready@.instance_id() == self.queue.pred().preparation.id()
        }
        pub closed spec fn index(&self) -> bool { self.handoff.index() }
        pub closed spec fn target(&self) -> Seq<Counter> { self.handoff.target() }
        pub closed spec fn lock(&self) -> TransitionLock { self.handoff.lock() }
        pub closed spec fn other(&self) -> Seq<Counter> { self.handoff.other() }
        pub closed spec fn prepared_id(&self) -> vstd::tokens::InstanceId {
            self.queue.pred().preparation.id()
        }
        pub closed spec fn owner(&self) -> *const u8 { self.current.owner() }
        pub closed spec fn prepared_inv(&self) -> spec_fn(P, Set<vstd::tokens::InstanceId>) -> bool {
            self.queue.pred().prepared_inv
        }
        pub closed spec fn payload_inv(&self) -> spec_fn(P) -> bool { self.queue.pred().payload_inv }
        pub closed spec fn bound(&self) -> Set<vstd::tokens::InstanceId> {
            stripes::gate_ids(self.handoff.target())
        }
        pub fn drains<'c>(&'c self, counters: &Vec<Counter>) -> (leases: Tracked<&'c super::super::drain::atomic_counter::DrainSet>)
            requires self.inv(), counters@ == self.target(),
            ensures leases@.inv(), leases@.domain() == self.bound(),
        { self.collection.drains(counters) }
        /// Private completion step: only the enclosing callback driver may
        /// restore the sealed controller map and return the queue-ready token.
        fn restore_after_callback(self, counters: &Vec<Counter>)
            -> (result: (State, Tracked<phase::ready>))
            requires self.inv(), counters@ == self.target(),
            ensures self.handoff.lock().inv(result.0), result.0.pending().is_none(),
                result.0.sealed(self.target(), self.index()),
                result.0.sealed(self.handoff.other(), !self.index()),
                result.0.controls(!self.index()) == self.handoff.original().controls(!self.index()),
                result.1@.instance_id() == self.queue.pred().preparation.id(),
        {
            let IdleDetached { handoff, collection, current: _, queue: _, ready } = self;
            let mut restored = collection.into_polled_collection(counters);
            restored.restore_all(counters);
            let Tracked(controls) = restored.into_controls(counters);
            let IdleHandoff { transition: _, handle: _, index, other_controls, original: _ } = handoff;
            let state = if index { State { zero: other_controls, one: Tracked(controls), pending: None } }
                else { State { zero: Tracked(controls), one: other_controls, pending: None } };
            (state, ready)
        }
    }
    pub fn collect_idle<'a>(state: State, transition: &'a TransitionLock, handle: &'a TransitionHandle<'a>,
        index: bool, counters: &Vec<Counter>) -> (result: (IdleHandoff<'a>, stripes::IdleCollection))
        requires transition.inv(state), handle.rwlock() == *transition, state.pending().is_none(),
            counters@ == transition.pred().counters(index), state.opened(counters@, index),
            state.sealed(transition.pred().counters(!index), !index),
        ensures result.0.inv(), result.0.index() == index, result.0.target() == counters@,
            result.0.original() == state, result.0.lock() == *transition, result.1.inv(counters@),
            forall|i: int| 0 <= i < counters.len() ==> !result.1.ready(i),
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
        let (other_controls, target_controls) = if index { (zero, one) } else { (one, zero) };
        assert(target_controls@ == original.controls(index));
        assert(original.opened(counters@, index));
        assert forall|i: int| #![auto] 0 <= i < counters.len() implies counters@[i].inv()
            && target_controls@.dom().contains(i as nat)
            && target_controls@[i as nat].instance_id() == counters@[i].authority_id()
            && !target_controls@[i as nat].value() by {
            assert(stripes::control_row(counters@, target_controls@, i));
            assert(stripes::reopen_row(counters@, target_controls@, i, counters@.len()));
        };
        let collection = stripes::IdleCollection::new(counters, target_controls);
        (IdleHandoff { transition, handle, index, other_controls, original: Ghost(original) }, collection)
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

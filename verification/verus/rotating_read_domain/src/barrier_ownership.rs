//! Registration queue guard ownership across the production-shared publication.
//! Library lock semantics are trusted; holding the matching handle is checked.
use vstd::prelude::*;
use vstd::rwlock::{RwLock, RwLockPredicate, WriteHandle};
use super::queue_preparation::phase;
verus! {
pub struct QueueContents<P> { pub domain: *const u8, pub index: bool, pub records: Vec<P> }
pub struct QueueState<P> { pub domain: *const u8, pub index: bool, pub records: Vec<P>, pub preparation: Tracked<phase::state>, pub instance: Tracked<phase::Instance> }
#[verifier::reject_recursive_types(P)]
pub struct QueuePredicate<P> { pub domain: *const u8, pub index: bool, pub payload_inv: spec_fn(P) -> bool,
    pub preparation: phase::Instance, pub prepared_inv: spec_fn(P, Set<vstd::tokens::InstanceId>) -> bool }
impl<P> RwLockPredicate<QueueState<P>> for QueuePredicate<P> {
    open spec fn inv(self, state: QueueState<P>) -> bool {
        state.domain == self.domain && state.index == self.index
        && state.instance@ == self.preparation
        && state.preparation@.instance_id() == self.preparation.id()
        && (forall|i: int| 0 <= i < state.records.len() ==> (self.payload_inv)(#[trigger] state.records@[i]))
        && (state.preparation@.value().is_some() ==> forall|i: int| 0 <= i < state.records.len()
            ==> (self.prepared_inv)(#[trigger] state.records@[i], state.preparation@.value().unwrap()))
    }
}
pub type QueueLock<P> = RwLock<QueueState<P>, QueuePredicate<P>>;
pub type QueueHandle<'a, P> = WriteHandle<'a, QueueState<P>, QueuePredicate<P>>;
#[verifier::reject_recursive_types(P)]
pub struct HeldBarrier<'a, P> {
    queue: &'a QueueState<P>, lock: &'a QueueLock<P>, handle: &'a QueueHandle<'a, P>,
}
impl<'a, P> HeldBarrier<'a, P> {
    pub closed spec fn inv(&self) -> bool {
        self.handle.rwlock() == *self.lock && self.lock.inv(*self.queue)
    }
    pub closed spec fn domain(&self) -> *const u8 { self.queue.domain }
    pub closed spec fn index(&self) -> bool { self.queue.index }
    pub fn issue(queue: &'a QueueState<P>, lock: &'a QueueLock<P>, handle: &'a QueueHandle<'a, P>) -> (barrier: Self)
        requires handle.rwlock() == *lock, lock.inv(*queue),
        ensures barrier.inv(), barrier.domain() == queue.domain, barrier.index() == queue.index,
    { HeldBarrier { queue, lock, handle } }
}
pub fn new_preparable<P>(contents: QueueContents<P>, Ghost(payload_inv): Ghost<spec_fn(P) -> bool>,
    Ghost(prepared_inv): Ghost<spec_fn(P, Set<vstd::tokens::InstanceId>) -> bool>)
    -> (result: (QueueLock<P>, Tracked<phase::ready>))
    requires forall|i: int| 0 <= i < contents.records.len() ==> payload_inv(#[trigger] contents.records@[i]),
    ensures result.0.pred().domain == contents.domain, result.0.pred().index == contents.index,
        result.0.pred().payload_inv == payload_inv, result.0.pred().prepared_inv == prepared_inv,
        result.1@.instance_id() == result.0.pred().preparation.id(),
{
    let tracked (Tracked(instance), Tracked(state), Tracked(ready), Tracked(prepared)) = phase::Instance::initialize();
    let ghost domain = contents.domain;
    let ghost index = contents.index;
    let queue = QueueState { domain: contents.domain, index: contents.index, records: contents.records, preparation: Tracked(state), instance: Tracked(instance) };
    let lock = RwLock::new(queue, Ghost(QueuePredicate { domain, index, payload_inv, preparation: instance, prepared_inv }));
    (lock, Tracked(ready.tracked_unwrap()))
}
pub fn new<P>(state: QueueContents<P>) -> (lock: QueueLock<P>)
    ensures lock.pred().domain == state.domain, lock.pred().index == state.index,
{
    let (lock, _) = new_preparable(state, Ghost(|_payload: P| true), Ghost(|_payload: P, _bound: Set<vstd::tokens::InstanceId>| false));
    lock
}
pub fn new_with_payload<P>(state: QueueContents<P>, Ghost(payload_inv): Ghost<spec_fn(P) -> bool>) -> (lock: QueueLock<P>)
    requires forall|i: int| 0 <= i < state.records.len() ==> payload_inv(#[trigger] state.records@[i]),
    ensures lock.pred().domain == state.domain, lock.pred().index == state.index, lock.pred().payload_inv == payload_inv,
{
    let (lock, _) = new_preparable(state, Ghost(payload_inv), Ghost(|_payload: P, _bound: Set<vstd::tokens::InstanceId>| false));
    lock
}

pub fn ready<P>(lock: &QueueLock<P>, queue: &QueueState<P>, Tracked(ticket): Tracked<&phase::ready>)
    requires lock.inv(*queue), ticket.instance_id() == lock.pred().preparation.id(),
    ensures queue.preparation@.value().is_none(),
{ proof { queue.instance.borrow().is_ready(queue.preparation.borrow(), ticket); } }
pub fn prepared<P>(lock: &QueueLock<P>, queue: &QueueState<P>, Tracked(ticket): Tracked<&phase::prepared>)
    requires lock.inv(*queue), ticket.instance_id() == lock.pred().preparation.id(),
    ensures queue.preparation@.value() == Some(ticket.value()),
        forall|i: int| 0 <= i < queue.records.len() ==> (lock.pred().prepared_inv)(#[trigger] queue.records@[i], ticket.value()),
{ proof { queue.instance.borrow().is_prepared(ticket.value(), queue.preparation.borrow(), ticket); } }
pub fn freeze<P>(queue: &mut QueueState<P>, lock: &QueueLock<P>, Tracked(ticket): Tracked<phase::ready>,
    Ghost(bound): Ghost<Set<vstd::tokens::InstanceId>>) -> (result: Tracked<phase::prepared>)
    requires lock.inv(*old(queue)), ticket.instance_id() == lock.pred().preparation.id(),
        forall|i: int| 0 <= i < old(queue).records.len() ==> (lock.pred().prepared_inv)(#[trigger] old(queue).records@[i], bound),
    ensures lock.inv(*final(queue)), final(queue).records@ == old(queue).records@,
        final(queue).domain == old(queue).domain, final(queue).index == old(queue).index,
        result@.instance_id() == lock.pred().preparation.id(), result@.value() == bound,
        final(queue).preparation@.value() == Some(bound),
{
    let tracked result = queue.instance.borrow().freeze(bound, queue.preparation.borrow_mut(), ticket);
    Tracked(result)
}
pub fn reset_empty<P>(queue: &mut QueueState<P>, lock: &QueueLock<P>, Tracked(ticket): Tracked<phase::prepared>)
    -> (result: Tracked<phase::ready>)
    requires lock.inv(*old(queue)), old(queue).records.len() == 0, ticket.instance_id() == lock.pred().preparation.id(),
    ensures lock.inv(*final(queue)), final(queue).records@ == old(queue).records@,
        final(queue).domain == old(queue).domain, final(queue).index == old(queue).index,
        result@.instance_id() == lock.pred().preparation.id(), final(queue).preparation@.value().is_none(),
{
    let tracked result = queue.instance.borrow().reset(ticket.value(), queue.preparation.borrow_mut(), ticket);
    Tracked(result)
}

pub fn append_open<P>(queue: &mut QueueState<P>, lock: &QueueLock<P>, handle: &QueueHandle<'_, P>, current: bool, payload: P)
    requires lock.inv(*old(queue)), handle.rwlock() == *lock, old(queue).index == current,
        old(queue).preparation@.value().is_none(), (lock.pred().payload_inv)(payload),
    ensures lock.inv(*final(queue)), final(queue).records@ == old(queue).records@.push(payload),
        final(queue).domain == old(queue).domain, final(queue).index == old(queue).index,
        final(queue).preparation == old(queue).preparation,
{ super::queue_transitions::append_retired!(&mut queue.records, payload); }

pub fn append_ready<P>(queue: &mut QueueState<P>, lock: &QueueLock<P>, handle: &QueueHandle<'_, P>,
    current: bool, Tracked(ticket): Tracked<&phase::ready>, payload: P)
    requires lock.inv(*old(queue)), handle.rwlock() == *lock, old(queue).index == current,
        ticket.instance_id() == lock.pred().preparation.id(), (lock.pred().payload_inv)(payload),
    ensures lock.inv(*final(queue)), final(queue).records@ == old(queue).records@.push(payload),
        final(queue).domain == old(queue).domain, final(queue).index == old(queue).index,
        final(queue).preparation == old(queue).preparation,
{
    ready(lock, queue, Tracked(ticket));
    append_open(queue, lock, handle, current, payload);
}

}
macro_rules! width {
    ($module:ident, $mask:ident) => {
    pub mod $module {
    use super::*;
    use super::super::refinement::$module::{Rotation, shared_begin_and_publication};
    use super::super::lock_ownership::$module::{TransitionLock, TransitionHandle};
    use super::super::transitions::$mask;
    verus! {
    /// Borrow both matching real lock handles while publishing. Neither handle
    /// can be released by the caller until this borrow ends.
    pub fn publish<P>(model: &mut Rotation, transition: &TransitionLock, transition_handle: &TransitionHandle<'_>,
        barrier: &HeldBarrier<'_, P>)
        requires old(model).inv(), old(model).locked, !old(model).barrier,
            !old(model).closed, old(model).pending.is_none(),
            transition_handle.rwlock() == *transition, transition.pred().domain == old(model).domain,
            barrier.inv(), barrier.domain() == old(model).domain, barrier.index() == old(model).current,
        ensures final(model).inv(), final(model).domain == old(model).domain,
            final(model).current == !old(model).current, final(model).pending == Some(old(model).current),
            final(model).zero & $mask == old(model).zero & $mask,
            final(model).one & $mask == old(model).one & $mask,
            final(model).sealed(old(model).current), final(model).locked, !final(model).barrier,
    {
        model.barrier = true;
        shared_begin_and_publication(model);
    }
    #[verifier::exec_allows_no_decreases_clause]
    pub fn begin<P>(model: &mut Rotation, transition: &TransitionLock, transition_handle: &TransitionHandle<'_>,
        lock: &QueueLock<P>)
        requires old(model).inv(), old(model).locked, !old(model).barrier,
            !old(model).closed, old(model).pending.is_none(),
            transition_handle.rwlock() == *transition, transition.pred().domain == old(model).domain,
            lock.pred().domain == old(model).domain, lock.pred().index == old(model).current,
        ensures final(model).inv(), final(model).domain == old(model).domain,
            final(model).current == !old(model).current, final(model).pending == Some(old(model).current),
            final(model).zero & $mask == old(model).zero & $mask,
            final(model).one & $mask == old(model).one & $mask,
            final(model).sealed(old(model).current), final(model).locked, !final(model).barrier,
    {
        let (queue, handle) = lock.acquire_write();
        let barrier = HeldBarrier::issue(&queue, lock, &handle);
        super::super::protocol::publish_release!(
            publish(model, transition, transition_handle, &barrier),
            handle.release_write(queue));
    }
    }
    }
    };
}
width!(word32, ACTIVE_COUNT_MASK_32);
width!(word64, ACTIVE_COUNT_MASK_64);

//! Atomic current selection derives queue readiness from owned resources.
use vstd::prelude::*;
use vstd::atomic_ghost::*;
use vstd::tokens::UniqueValueToken;
use super::queue_preparation::phase;
use super::current_authority::publication;
use super::barrier_ownership::{QueueState, QueueLock, QueueHandle};
verus! {
pub tracked struct CurrentGhost {
    mode: publication::mode,
    ready: Option<phase::ready>,
    held: Option<phase::state>,
}
pub struct CurrentPredicate;
impl AtomicInvariantPredicate<(vstd::tokens::InstanceId, vstd::tokens::InstanceId, vstd::tokens::InstanceId), bool, CurrentGhost> for CurrentPredicate {
    closed spec fn atomic_inv(k: (vstd::tokens::InstanceId, vstd::tokens::InstanceId, vstd::tokens::InstanceId), value: bool, g: CurrentGhost) -> bool {
        g.mode.instance_id() == k.2
        && g.ready.is_some() == g.mode.value().is_none()
        && g.held.is_some() == g.mode.value().is_some()
        && (g.ready.is_some() ==> g.ready.unwrap().instance_id() == if value { k.1 } else { k.0 })
        && (g.held.is_some() ==> g.held.unwrap().instance_id() == if value { k.1 } else { k.0 })
        && (g.mode.value().is_some() ==> g.mode.value() == Some(value))
    }
}
pub struct Current {
    atomic: AtomicBool<(vstd::tokens::InstanceId, vstd::tokens::InstanceId, vstd::tokens::InstanceId), CurrentGhost, CurrentPredicate>,
    authority: Tracked<publication::Instance>,
    owner: *const u8,
}
pub struct ReservedQueue<P> {
    domain: *const u8, index: bool, records: Vec<P>, instance: Tracked<phase::Instance>,
    prepared: Tracked<phase::prepared>, reservation: Tracked<publication::reservation>,
}
impl<P> ReservedQueue<P> {
    pub closed spec fn inv(&self, current: &Current, lock: &QueueLock<P>) -> bool {
        self.domain == current.owner() && self.domain == lock.pred().domain && self.index == lock.pred().index
        && self.instance@ == lock.pred().preparation && self.instance@.id() == current.gate(self.index)
        && self.prepared@.instance_id() == self.instance@.id()
        && self.reservation@.instance_id() == current.authority@.id() && self.reservation@.value() == self.index
        && (forall|i: int| 0 <= i < self.records.len() ==> (lock.pred().payload_inv)(#[trigger] self.records@[i]))
        && (forall|i: int| 0 <= i < self.records.len() ==> (lock.pred().prepared_inv)(#[trigger] self.records@[i], self.prepared@.value()))
    }
    pub closed spec fn records(&self) -> Seq<P> { self.records@ }
    pub closed spec fn index(&self) -> bool { self.index }
    pub closed spec fn bound(&self) -> Set<vstd::tokens::InstanceId> { self.prepared@.value() }
}
impl Current {
    pub closed spec fn owner(&self) -> *const u8 { self.owner }
    pub closed spec fn gate(&self, index: bool) -> vstd::tokens::InstanceId { if index { self.atomic.constant().1 } else { self.atomic.constant().0 } }
    pub closed spec fn inv(&self) -> bool { self.atomic.well_formed() && self.atomic.constant().2 == self.authority@.id() && self.gate(false) != self.gate(true) }
    pub fn new<P>(zero: &QueueLock<P>, one: &QueueLock<P>, owner: *const u8, Tracked(ready): Tracked<phase::ready>) -> (current: Self)
        requires zero.pred().domain == owner, one.pred().domain == owner, !zero.pred().index, one.pred().index,
            zero.pred().preparation.id() != one.pred().preparation.id(), ready.instance_id() == zero.pred().preparation.id(),
        ensures current.inv(), current.owner() == owner,
            current.gate(false) == zero.pred().preparation.id(), current.gate(true) == one.pred().preparation.id(),
    {
        let tracked (Tracked(authority), Tracked(mode), Tracked(reservation)) = publication::Instance::initialize();
        let ghost key = (zero.pred().preparation.id(), one.pred().preparation.id(), authority.id());
        let atomic = AtomicBool::new(Ghost(key), false, Tracked(CurrentGhost { mode, ready: Some(ready), held: None }));
        Current { atomic, authority: Tracked(authority), owner }
    }
    pub fn select(&self) -> (index: bool)
        requires self.inv(),
    { atomic_with_ghost!(self.atomic => load(); ghost g => { }) }

    /// A current queue cannot simultaneously be held by the publishing side:
    /// two phase-state tokens of the same instance contradict uniqueness.
    pub fn recheck<P>(&self, queue: &QueueState<P>, lock: &QueueLock<P>, handle: &QueueHandle<'_, P>) -> (observed: bool)
        requires self.inv(), lock.inv(*queue), handle.rwlock() == *lock,
            queue.domain == self.owner(), lock.pred().preparation.id() == self.gate(queue.index),
        ensures observed == queue.index ==> queue.preparation@.value().is_none(),
    {
        let selected = queue.index;
        let value = atomic_with_ghost!(self.atomic => load(); returning value; ghost g => {
            if value == selected {
                if g.held.is_some() {
                    let tracked mut held = g.held.tracked_take();
                    held.unique(queue.preparation.borrow());
                    assert(false);
                }
                queue.instance.borrow().is_ready(queue.preparation.borrow(), g.ready.tracked_borrow());
            }
        });
        value
    }
    /// Move the protected phase state into the atomic invariant before publication.
    /// The caller retains the queue handle, but cannot return an invalid queue state.
    pub fn reserve<P>(&self, queue: QueueState<P>, lock: &QueueLock<P>, handle: &QueueHandle<'_, P>,
        Ghost(bound): Ghost<Set<vstd::tokens::InstanceId>>) -> (result: Result<ReservedQueue<P>, QueueState<P>>)
        requires self.inv(), lock.inv(queue), handle.rwlock() == *lock, queue.domain == self.owner(),
            lock.pred().preparation.id() == self.gate(queue.index),
            forall|i: int| 0 <= i < queue.records.len() ==> (lock.pred().prepared_inv)(#[trigger] queue.records@[i], bound),
        ensures match result {
            Ok(reserved) => reserved.inv(self, lock) && reserved.records() == queue.records@ && reserved.index() == queue.index && reserved.bound() == bound,
            Err(returned) => returned == queue,
        },
    {
        let QueueState { domain, index, records, preparation: Tracked(state), instance: Tracked(instance) } = queue;
        let tracked mut remaining = Some(state);
        let tracked mut prepared = None;
        let tracked mut reservation = None;
        let observed = atomic_with_ghost!(self.atomic => load(); returning observed; ghost g => {
            if observed == index {
                let tracked mut state = remaining.tracked_take();
                if g.held.is_some() {
                    state.unique(g.held.tracked_borrow());
                    assert(false);
                }
                let tracked ready = g.ready.tracked_take();
                prepared = Some(instance.freeze(bound, &mut state, ready));
                reservation = Some(self.authority.borrow().reserve(index, &mut g.mode));
                g.held = Some(state);
            }
        });
        if observed == index {
            Ok(ReservedQueue { domain, index, records, instance: Tracked(instance),
                prepared: Tracked(prepared.tracked_unwrap()), reservation: Tracked(reservation.tracked_unwrap()) })
        } else {
            Err(QueueState { domain, index, records, instance: Tracked(instance), preparation: Tracked(remaining.tracked_unwrap()) })
        }
    }
    pub(super) fn publish_reserved<P>(&self, reserved: ReservedQueue<P>, lock: &QueueLock<P>, handle: &QueueHandle<'_, P>,
        Tracked(next_ready): Tracked<phase::ready>) -> (result: (QueueState<P>, Tracked<phase::prepared>))
        requires self.inv(), reserved.inv(self, lock), handle.rwlock() == *lock,
            next_ready.instance_id() == self.gate(!reserved.index()),
        ensures lock.inv(result.0), result.0.records@ == reserved.records(), result.0.domain == self.owner(),
            result.0.index == reserved.index(), result.1@.instance_id() == lock.pred().preparation.id(), result.1@.value() == reserved.bound(),
    {
        let ReservedQueue { domain, index, records, instance: Tracked(instance), prepared: Tracked(prepared), reservation: Tracked(reservation) } = reserved;
        let tracked mut state = None;
        atomic_with_ghost!(self.atomic => store(!index); update previous -> next; ghost g => {
            self.authority.borrow().reserved(index, &g.mode, &reservation);
            self.authority.borrow().finish(index, &mut g.mode, reservation);
            let tracked held = g.held.tracked_take();
            instance.is_prepared(prepared.value(), &held, &prepared);
            state = Some(held);
            g.ready = Some(next_ready);
        });
        (QueueState { domain, index, records, instance: Tracked(instance), preparation: Tracked(state.tracked_unwrap()) }, Tracked(prepared))
    }

}
}

//! Shared registration retry against actual atomic current and library queue locks.
use vstd::prelude::*;
use super::current_atomic::Current;
use super::barrier_ownership::{QueueLock, QueueState, QueueHandle};
use super::protocol::protocol_expr;
verus! {
#[verifier::reject_recursive_types(P)]
struct Held<'a, P> { queue: QueueState<P>, handle: QueueHandle<'a, P>, lock: &'a QueueLock<P> }
impl<'a, P> Held<'a, P> {
    closed spec fn inv(&self) -> bool { self.lock.inv(self.queue) && self.handle.rwlock() == *self.lock }
}
pub struct Registered<P> { index: bool, before: Ghost<Seq<P>>, after: Ghost<Seq<P>> }
impl<P> Registered<P> {
    pub closed spec fn index(&self) -> bool { self.index }
    pub closed spec fn before(&self) -> Seq<P> { self.before@ }
    pub closed spec fn after(&self) -> Seq<P> { self.after@ }
}
pub open spec fn compatible<P>(current: &Current, zero: &QueueLock<P>, one: &QueueLock<P>, payload: P) -> bool {
    current.inv() && zero.pred().domain == current.owner() && one.pred().domain == current.owner()
    && !zero.pred().index && one.pred().index
    && zero.pred().preparation.id() == current.gate(false) && one.pred().preparation.id() == current.gate(true)
    && (zero.pred().payload_inv)(payload) && (one.pred().payload_inv)(payload)
}
#[verifier::exec_allows_no_decreases_clause]
fn acquire<'a, P>(zero: &'a QueueLock<P>, one: &'a QueueLock<P>, selected: bool) -> (held: Held<'a, P>)
    requires !zero.pred().index, one.pred().index,
    ensures held.inv(), held.queue.index == selected, held.lock == if selected { one } else { zero },
{
    let lock = if selected { one } else { zero };
    let (queue, handle) = lock.acquire_write();
    Held { queue, handle, lock }
}
fn release<P>(held: Held<'_, P>)
    requires held.inv(),
{ held.handle.release_write(held.queue); }
fn recheck<P>(current: &Current, held: &Held<'_, P>) -> (observed: bool)
    requires current.inv(), held.inv(), held.queue.domain == current.owner(),
        held.lock.pred().preparation.id() == current.gate(held.queue.index),
    ensures observed == held.queue.index ==> held.queue.preparation@.value().is_none(),
{ current.recheck(&held.queue, held.lock, &held.handle) }
fn append<P>(mut held: Held<'_, P>, selected: bool, payload: P) -> (result: Registered<P>)
    requires held.inv(), held.queue.index == selected, held.queue.preparation@.value().is_none(),
        (held.lock.pred().payload_inv)(payload),
    ensures result.index() == selected, result.before() == held.queue.records@,
        result.after() == result.before().push(payload),
{
    let ghost before = held.queue.records@;
    super::barrier_ownership::append_open(&mut held.queue, held.lock, &held.handle, selected, payload);
    let ghost after = held.queue.records@;
    release(held);
    Registered { index: selected, before: Ghost(before), after: Ghost(after) }
}
#[verifier::exec_allows_no_decreases_clause]
pub fn register<P>(current: &Current, zero: &QueueLock<P>, one: &QueueLock<P>, payload: P) -> (receipt: Registered<P>)
    requires compatible(current, zero, one, payload),
    ensures receipt.after() == receipt.before().push(payload),
{
    super::protocol::register_retired!(selected, guard;
        current.select(), acquire(zero, one, selected), recheck(current, &guard), release(guard), (), append(guard, selected, payload);
        invariant compatible(current, zero, one, payload),
    )
}
}

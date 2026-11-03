//! Shared certified detachment backed by actual library queue handles.
use vstd::prelude::*;
use super::barrier_ownership::{QueueLock, QueueState, QueueHandle};
verus! {
pub struct Withdrawal<P> {
    records: Vec<P>, source: Ghost<Seq<P>>, owner: Ghost<*const u8>, index: Ghost<bool>,
}
impl<P> Withdrawal<P> {
    pub closed spec fn inv(&self) -> bool { self.records@ == self.source@ }
    pub closed spec fn source(&self) -> Seq<P> { self.source@ }
    pub closed spec fn owner(&self) -> *const u8 { self.owner@ }
    pub closed spec fn index(&self) -> bool { self.index@ }
    pub fn into_records(self) -> (records: Vec<P>)
        requires self.inv(), ensures records@ == self.source(),
    { self.records }
}
#[verifier::exec_allows_no_decreases_clause]
pub fn acquire_index<P>(lock: &QueueLock<P>, index: usize) -> (held: (QueueState<P>, QueueHandle<'_, P>))
    requires index < 2, lock.pred().index == (index == 1),
    ensures lock.inv(held.0), held.1.rwlock() == *lock,
        held.0.index == (index == 1), held.0.domain == lock.pred().domain,
{ lock.acquire_write() }

pub fn take_locked<P>(owner: *const u8, held: (QueueState<P>, QueueHandle<'_, P>)) -> (records: Vec<P>)
    requires held.1.rwlock().inv(held.0), held.0.domain == owner,
    ensures records@ == held.0.records@,
        forall|i: int| 0 <= i < records.len() ==> (held.1.rwlock().pred().payload_inv)(#[trigger] records@[i]),
{
    let (mut queue, handle) = held;
    let records = super::queue_transitions::take_retired!(&mut queue.records);
    handle.release_write(queue);
    records
}
pub fn take_pair<P>(owner: *const u8, first: (QueueState<P>, QueueHandle<'_, P>), second: (QueueState<P>, QueueHandle<'_, P>)) -> (records: [Vec<P>; 2])
    requires first.1.rwlock().inv(first.0), second.1.rwlock().inv(second.0),
        !first.0.index, second.0.index, first.0.domain == owner, second.0.domain == owner,
    ensures records[0]@ == first.0.records@, records[1]@ == second.0.records@,
        forall|i: int| 0 <= i < records[0].len() ==> (first.1.rwlock().pred().payload_inv)(#[trigger] records[0]@[i]),
        forall|i: int| 0 <= i < records[1].len() ==> (second.1.rwlock().pred().payload_inv)(#[trigger] records[1]@[i]),
{
    let (mut zero, first_handle) = first;
    let (mut one, second_handle) = second;
    let zero_records = super::queue_transitions::take_retired!(&mut zero.records);
    let one_records = super::queue_transitions::take_retired!(&mut one.records);
    second_handle.release_write(one);
    first_handle.release_write(zero);
    [zero_records, one_records]
}

pub fn take_receipt<P>(owner: *const u8, held: (QueueState<P>, QueueHandle<'_, P>)) -> (withdrawal: Withdrawal<P>)
    requires held.1.rwlock().inv(held.0), held.0.domain == owner,
    ensures withdrawal.inv(), withdrawal.source() == held.0.records@, withdrawal.owner() == owner,
        withdrawal.index() == held.0.index,
        forall|i: int| 0 <= i < withdrawal.source().len() ==> (held.1.rwlock().pred().payload_inv)(#[trigger] withdrawal.source()[i]),
{
    let ghost source = held.0.records@;
    let ghost index = held.0.index;
    let records = take_locked(owner, held);
    Withdrawal { records, source: Ghost(source), owner: Ghost(owner), index: Ghost(index) }
}
pub fn take_pair_receipts<P>(owner: *const u8, first: (QueueState<P>, QueueHandle<'_, P>), second: (QueueState<P>, QueueHandle<'_, P>))
    -> (withdrawals: (Withdrawal<P>, Withdrawal<P>))
    requires first.1.rwlock().inv(first.0), second.1.rwlock().inv(second.0),
        !first.0.index, second.0.index, first.0.domain == owner, second.0.domain == owner,
    ensures withdrawals.0.inv(), withdrawals.1.inv(), withdrawals.0.owner() == owner, withdrawals.1.owner() == owner,
        !withdrawals.0.index(), withdrawals.1.index(),
        withdrawals.0.source() == first.0.records@, withdrawals.1.source() == second.0.records@,
        forall|i: int| 0 <= i < withdrawals.0.source().len() ==> (first.1.rwlock().pred().payload_inv)(#[trigger] withdrawals.0.source()[i]),
        forall|i: int| 0 <= i < withdrawals.1.source().len() ==> (second.1.rwlock().pred().payload_inv)(#[trigger] withdrawals.1.source()[i]),
{
    let ghost first_source = first.0.records@;
    let ghost second_source = second.0.records@;
    let mut pair = take_pair(owner, first, second);
    let mut zero = Vec::new();
    let mut one = Vec::new();
    core::mem::swap(&mut pair[0], &mut zero);
    core::mem::swap(&mut pair[1], &mut one);
    (Withdrawal { records: zero, source: Ghost(first_source), owner: Ghost(owner), index: Ghost(false) },
     Withdrawal { records: one, source: Ghost(second_source), owner: Ghost(owner), index: Ghost(true) })
}

pub fn take_prepared<P>(owner: *const u8, held: (QueueState<P>, QueueHandle<'_, P>), lock: &QueueLock<P>,
    Tracked(ticket): Tracked<&mut Option<super::queue_preparation::phase::prepared>>)
    -> (result: (Withdrawal<P>, Tracked<super::queue_preparation::phase::ready>))
    requires held.1.rwlock() == *lock, lock.inv(held.0), held.0.domain == owner,
        old(ticket).is_some(), old(ticket).unwrap().instance_id() == lock.pred().preparation.id(),
    ensures final(ticket).is_none(), result.0.inv(), result.0.source() == held.0.records@,
        result.0.owner() == owner, result.0.index() == held.0.index,
        result.1@.instance_id() == lock.pred().preparation.id(),
        forall|i: int| 0 <= i < result.0.source().len() ==> (lock.pred().payload_inv)(#[trigger] result.0.source()[i]),
        forall|i: int| 0 <= i < result.0.source().len() ==> (lock.pred().prepared_inv)(#[trigger] result.0.source()[i], old(ticket).unwrap().value()),
{
    let (mut queue, handle) = held;
    let ghost source = queue.records@;
    let ghost index = queue.index;
    let tracked prepared = ticket.tracked_take();
    super::barrier_ownership::prepared(lock, &queue, Tracked(&prepared));
    let records = super::queue_transitions::take_retired!(&mut queue.records);
    let ready = super::barrier_ownership::reset_empty(&mut queue, lock, Tracked(prepared));
    handle.release_write(queue);
    (Withdrawal { records, source: Ghost(source), owner: Ghost(owner), index: Ghost(index) }, ready)
}

}
macro_rules! width {
    ($module:ident) => {
    pub mod $module {
    use super::*;
    use super::super::identity::$module::{Drained, Closed};
    verus! {
    #[verifier::exec_allows_no_decreases_clause]
    pub fn terminal<P>(zero: &QueueLock<P>, one: &QueueLock<P>, owner: *const u8, certificate: &Closed<'_>)
        -> (records: Option<(Withdrawal<P>, Withdrawal<P>)>)
        requires certificate.inv(), zero.pred().domain == owner, one.pred().domain == owner,
            !zero.pred().index, one.pred().index,
        ensures records.is_some() == (owner.addr() == certificate.state().domain.addr()),
            records.is_some() ==> records.unwrap().0.inv() && records.unwrap().1.inv()
                && records.unwrap().0.owner() == owner && records.unwrap().1.owner() == owner
                && !records.unwrap().0.index() && records.unwrap().1.index(),
            records.is_some() ==> forall|i: int| 0 <= i < records.unwrap().0.source().len()
                ==> (zero.pred().payload_inv)(#[trigger] records.unwrap().0.source()[i]),
            records.is_some() ==> forall|i: int| 0 <= i < records.unwrap().1.source().len()
                ==> (one.pred().payload_inv)(#[trigger] records.unwrap().1.source()[i]),
    {
        super::super::protocol::take_authorized_queues!(indices, first, second;
            certificate.indices_for(owner), acquire_index(zero, indices[0]),
            acquire_index(one, indices[1]), take_pair_receipts(owner, first, second))
    }
    #[verifier::exec_allows_no_decreases_clause]
    pub fn detach_prepared<P>(lock: &QueueLock<P>, owner: *const u8, certificate: &Drained<'_>,
        Tracked(prepared): Tracked<super::super::queue_preparation::phase::prepared>)
        -> (result: Result<(Withdrawal<P>, Tracked<super::super::queue_preparation::phase::ready>), Tracked<super::super::queue_preparation::phase::prepared>>)
        requires certificate.inv(), lock.pred().domain == owner, lock.pred().index == (certificate.index() == 1),
            prepared.instance_id() == lock.pred().preparation.id(),
        ensures result.is_ok() == (owner.addr() == certificate.state().domain.addr()),
            match result { Err(token) => token@ == prepared, _ => true },
            result.is_ok() ==> result.unwrap().0.inv() && result.unwrap().0.owner() == owner
                && result.unwrap().0.index() == (certificate.index() == 1)
                && result.unwrap().1@.instance_id() == lock.pred().preparation.id(),
            result.is_ok() ==> forall|i: int| 0 <= i < result.unwrap().0.source().len()
                ==> (lock.pred().payload_inv)(#[trigger] result.unwrap().0.source()[i]),
            result.is_ok() ==> forall|i: int| 0 <= i < result.unwrap().0.source().len()
                ==> (lock.pred().prepared_inv)(#[trigger] result.unwrap().0.source()[i], prepared.value()),
    {
        let tracked mut ticket = Some(prepared);
        let result = super::super::protocol::take_authorized_queue!(index, guard;
            certificate.index_for(owner), acquire_index(lock, index),
            verus_exec_expr!(take_prepared(owner, guard, lock, Tracked(&mut ticket))));
        match result {
            Some(withdrawal) => Ok(withdrawal),
            None => Err(Tracked(ticket.tracked_unwrap())),
        }
    }
    #[verifier::exec_allows_no_decreases_clause]
    pub fn detach<P>(lock: &QueueLock<P>, owner: *const u8, certificate: &Drained<'_>) -> (records: Option<Withdrawal<P>>)
        requires certificate.inv(), lock.pred().domain == owner, lock.pred().index == (certificate.index() == 1),
        ensures records.is_some() == (owner.addr() == certificate.state().domain.addr()),
            records.is_some() ==> records.unwrap().inv() && records.unwrap().owner() == owner
                && records.unwrap().index() == (certificate.index() == 1),
            records.is_some() ==> forall|i: int| 0 <= i < records.unwrap().source().len()
                ==> (lock.pred().payload_inv)(#[trigger] records.unwrap().source()[i]),
    {
        super::super::protocol::take_authorized_queue!(index, guard;
            certificate.index_for(owner), acquire_index(lock, index), take_receipt(owner, guard))
    }
    }
    }
    };
}
width!(word32);
width!(word64);

//! Verified write-lock ownership supplies the exclusive publication authority.
use vstd::prelude::*;
use vstd::rwlock::{RwLock, RwLockPredicate};
use super::writer_authority::publication;
use super::atomic_publication::{Slot, Prepared, PublishStatus};
use super::atomic_publication::RetiredRecord;
verus! {
pub struct WriterState<T> { pub capability: Tracked<publication::writer<T>> }
pub struct WriterPredicate { pub authority: vstd::tokens::InstanceId }
impl<T> RwLockPredicate<WriterState<T>> for WriterPredicate {
    open spec fn inv(self, state: WriterState<T>) -> bool { state.capability@.instance_id() == self.authority }
}
pub type WriterLock<T> = RwLock<WriterState<T>, WriterPredicate>;
pub fn new<T>(Tracked(capability): Tracked<publication::writer<T>>) -> (lock: WriterLock<T>)
    ensures lock.pred().authority == capability.instance_id(),
{
    let ghost authority = capability.instance_id();
    RwLock::new(WriterState { capability: Tracked(capability) }, Ghost(WriterPredicate { authority }))
}
#[verifier::exec_allows_no_decreases_clause]
pub fn publish_locked<T>(lock: &WriterLock<T>, slot: &Slot<T>, pointer: *mut T,
    Tracked(prepared): Tracked<Prepared<T>>) -> (result: (PublishStatus, Tracked<Option<Prepared<T>>>))
    requires slot.inv(), lock.pred().authority == slot.authority_id(),
        prepared.valid(slot.registry_id(), slot.owner(), slot.domain()),
        prepared.published.value().ptr() == pointer, pointer.addr() != 0,
    ensures (result.0 is Published) == result.1@.is_none(), result.1@.is_some() ==> result.1@.unwrap() == prepared,
{
    let (state, handle) = lock.acquire_write();
    let WriterState { capability: Tracked(mut capability) } = state;
    let result = slot.publish(pointer, Tracked(prepared), Tracked(&mut capability));
    handle.release_write(WriterState { capability: Tracked(capability) });
    result
}
#[verifier::exec_allows_no_decreases_clause]
pub fn retire_locked<T>(lock: &WriterLock<T>, slot: &Slot<T>, expected: *mut T) -> (entry: Option<RetiredRecord<T>>)
    requires slot.inv(), lock.pred().authority == slot.authority_id(), expected.addr() != 0,
    ensures entry.is_some() ==> entry.unwrap().inv() && entry.unwrap().registry_id() == slot.registry_id() && entry.unwrap().owner() == slot.owner() && entry.unwrap().pointer().addr() == expected.addr(),
{
    let (state, handle) = lock.acquire_write();
    let WriterState { capability: Tracked(mut capability) } = state;
    let result = slot.retire(expected, Tracked(&mut capability));
    handle.release_write(WriterState { capability: Tracked(capability) });
    result
}
}

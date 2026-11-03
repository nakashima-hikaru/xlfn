//! Real heap-owner payloads through shared registration and certified batches.
//! Native BindingRecord/arena/reader representation remains to be connected.
use vstd::prelude::*;
use super::batches::{Batch, DomainOwner};
use super::heap_permission::HeapPermission;
use super::ownership::LinearOwner;
use super::rotation::registration::{Registration, shared_registration};
verus! {
pub struct BindingOwner<'domain, T> {
    domain: &'domain DomainOwner,
    memory: LinearOwner<T>,
}
impl<'domain, T> BindingOwner<'domain, T> {
    pub closed spec fn domain(&self) -> &'domain DomainOwner { self.domain }
    pub closed spec fn inv(&self) -> bool { self.memory.inv() }
    pub closed spec fn pointer(&self) -> *mut T { self.memory.pointer() }
    pub fn adopt(domain: &'domain DomainOwner, pointer: *mut T, Tracked(memory): Tracked<HeapPermission<T>>)
        -> (owner: Self)
        requires memory.inv(), memory.ptr() == pointer, memory.is_init(),
        ensures owner.inv(), owner.domain() == domain, owner.pointer() == pointer,
    { BindingOwner { domain, memory: LinearOwner::adopt(pointer, Tracked(memory)) } }
}
pub open spec fn valid_records<T>(records: Seq<BindingOwner<'_, T>>, domain: &DomainOwner) -> bool {
    forall|i: int| 0 <= i < records.len() ==> (#[trigger] records[i]).inv() && records[i].domain() == domain
}
pub open spec fn valid_queue<T>(queue: &Registration<BindingOwner<'_, T>>, domain: &DomainOwner) -> bool {
    queue.owner == domain.rotation && valid_records(queue.zero@, domain) && valid_records(queue.one@, domain)
}

#[verifier::exec_allows_no_decreases_clause]
pub fn register<'domain, T>(queue: &mut Registration<BindingOwner<'domain, T>>, domain: &'domain DomainOwner,
    record: BindingOwner<'domain, T>) -> (generation: bool)
    requires valid_queue(old(queue), domain), record.inv(), record.domain() == domain,
        old(queue).held.is_none(), !old(queue).both_held, old(queue).registered.is_none(),
        old(queue).cursor <= old(queue).samples.len(),
    ensures valid_queue(final(queue), domain), final(queue).held.is_none(), !final(queue).both_held,
        final(queue).registered == Some(generation), generation == final(queue).current,
        final(queue).zero@ == if generation { old(queue).zero@ } else { old(queue).zero@.push(record) },
        final(queue).one@ == if generation { old(queue).one@.push(record) } else { old(queue).one@ },
{ shared_registration(queue, record) }
}
macro_rules! width {
    ($module:ident, $word:ty) => {
    pub mod $module {
    use super::*;
    use super::super::rotation::identity::$module::{Drained, Closed};
    verus! {
    pub fn complete_batch<'domain, T>(batch: Batch<'domain, BindingOwner<'domain, T>>, debt: $word)
        -> (completion: super::super::batches::$module::BoundCompletion<'domain, BindingOwner<'domain, T>>)
        requires valid_records(batch.records(), batch.owner()), batch.records().len() <= debt as nat,
        ensures completion.owner() == batch.owner(), completion.inv(), completion.phase() == 2,
            completion.debt() + batch.records().len() == debt as nat,
            completion.notified(), !completion.locked(),
    {
        let mut work = super::super::batches::$module::start_completion(batch, debt);
        work.complete();
        work
    }

    pub fn ordinary<'domain, T>(queue: &mut Registration<BindingOwner<'domain, T>>, domain: &'domain DomainOwner,
        certificate: &Drained<'_>) -> (batch: Option<Batch<'domain, BindingOwner<'domain, T>>>)
        requires valid_queue(old(queue), domain), certificate.inv(), old(queue).held.is_none(), !old(queue).both_held,
        ensures valid_queue(final(queue), domain), final(queue).held.is_none(), !final(queue).both_held,
            batch.is_some() == (domain.rotation.addr() == certificate.state().domain.addr()),
            batch.is_some() ==> batch.unwrap().owner() == domain && valid_records(batch.unwrap().records(), domain),
    { super::super::batches::$module::ordinary(queue, domain, certificate) }

    pub fn terminal<'domain, T>(queue: &mut Registration<BindingOwner<'domain, T>>, domain: &'domain DomainOwner,
        certificate: &Closed<'_>) -> (batches: Option<[Batch<'domain, BindingOwner<'domain, T>>; 2]>)
        requires valid_queue(old(queue), domain), certificate.inv(), old(queue).held.is_none(), !old(queue).both_held,
        ensures valid_queue(final(queue), domain), final(queue).held.is_none(), !final(queue).both_held,
            batches.is_some() == (domain.rotation.addr() == certificate.state().domain.addr()),
            batches.is_some() ==> batches.unwrap()[0].owner() == domain && batches.unwrap()[1].owner() == domain
                && valid_records(batches.unwrap()[0].records(), domain) && valid_records(batches.unwrap()[1].records(), domain),
    { super::super::batches::$module::terminal(queue, domain, certificate) }
    }
    }
    };
}
width!(word32, u32);
width!(word64, u64);

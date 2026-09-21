//! Certificate-to-batch ownership transport. Native address/Drop adapters remain open.
use vstd::prelude::*;
use super::rotation::registration::Registration;
verus! {
/// Separate the Handle completion owner from its embedded rotation identity.
/// Native mapping of these two addresses is a representation obligation.
pub struct DomainOwner {
    pub identity: *const u8,
    pub rotation: *const u8,
}

pub struct Batch<'domain, P> {
    owner: &'domain DomainOwner,
    records: Vec<P>,
}
impl<'domain, P> Batch<'domain, P> {
    pub closed spec fn owner(&self) -> &'domain DomainOwner { self.owner }
    pub closed spec fn records(&self) -> Seq<P> { self.records@ }
    /// Move the certified payload and its owner together into a resource adapter.
    /// This transfer performs no destructor work or completion-debt discharge.
    pub fn into_parts(self) -> (parts: (&'domain DomainOwner, Vec<P>))
        ensures parts.0 == self.owner(), parts.1@ == self.records(),
    { (self.owner, self.records) }
    pub fn empty(owner: &'domain DomainOwner) -> (batch: Self)
        ensures batch.owner() == owner, batch.records() == Seq::<P>::empty(),
    { Batch { owner, records: Vec::new() } }
}

/// Keep the protected-queue snapshot through wrapping and later recovery.
pub struct LockedBatch<'domain, P> {
    batch: Batch<'domain, P>, source: Ghost<Seq<P>>, index: Ghost<bool>,
}
impl<'domain, P> LockedBatch<'domain, P> {
    pub closed spec fn inv(&self) -> bool { self.batch.records() == self.source@ }
    pub closed spec fn owner(&self) -> &'domain DomainOwner { self.batch.owner() }
    pub closed spec fn records(&self) -> Seq<P> { self.batch.records() }
    pub closed spec fn source(&self) -> Seq<P> { self.source@ }
    pub closed spec fn index(&self) -> bool { self.index@ }
    pub fn into_batch(self) -> (batch: Batch<'domain, P>)
        requires self.inv(),
        ensures batch.owner() == self.owner(), batch.records() == self.source(), batch.records() == self.records(),
    { self.batch }
}
pub fn bind_withdrawal<'domain, P>(owner: &'domain DomainOwner,
    withdrawal: super::rotation::locked_detachment::Withdrawal<P>) -> (batch: LockedBatch<'domain, P>)
    requires withdrawal.inv(), withdrawal.owner() == owner.rotation,
    ensures batch.inv(), batch.owner() == owner, batch.source() == withdrawal.source(), batch.index() == withdrawal.index(),
{
    let ghost source = withdrawal.source();
    let ghost index = withdrawal.index();
    let records = withdrawal.into_records();
    LockedBatch { batch: Batch { owner, records }, source: Ghost(source), index: Ghost(index) }
}

/// Same identity expression as native DrainedBindings::extend. Vec ownership
/// moves each payload once; the emptied source contains no destruction work.
pub fn append<P>(left: &mut Batch<'_, P>, right: &mut Batch<'_, P>) -> (accepted: bool)
    ensures final(left).owner() == old(left).owner(), final(right).owner() == old(right).owner(),
        accepted == (old(left).owner().identity.addr() == old(right).owner().identity.addr()),
        accepted ==> final(left).records() == old(left).records() + old(right).records()
            && final(right).records() == Seq::<P>::empty(),
        !accepted ==> final(left).records() == old(left).records() && final(right).records() == old(right).records(),
{
    super::completion_protocol::append_owned_batch!(left.owner.identity, right.owner.identity;
        return false, left.records.append(&mut right.records));
    true
}
}
macro_rules! detach {
    ($module:ident, $word:ty) => {
    pub mod $module {
    use super::*;
    use super::super::rotation::identity::$module::{Drained, Closed};
    verus! {
    pub struct BoundCompletion<'domain, P> {
        owner: &'domain DomainOwner,
        work: super::super::completion::$module::Completion<P>,
    }
    impl<'domain, P> BoundCompletion<'domain, P> {
        pub closed spec fn owner(&self) -> &'domain DomainOwner { self.owner }
        pub closed spec fn inv(&self) -> bool { self.work.inv() }
        pub closed spec fn phase(&self) -> nat { self.work.phase() }
        pub closed spec fn debt(&self) -> nat { self.work.debt() as nat }
        pub closed spec fn count(&self) -> nat { self.work.count() }
        pub closed spec fn notified(&self) -> bool { self.work.notified() }
        pub closed spec fn locked(&self) -> bool { self.work.locked() }
        pub fn complete(&mut self)
            requires old(self).inv(), old(self).phase() == 0, !old(self).notified(), !old(self).locked(),
            ensures final(self).owner() == old(self).owner(), final(self).inv(), final(self).phase() == 2,
                final(self).debt() + old(self).count() == old(self).debt(),
                final(self).notified(), !final(self).locked(),
        { super::super::completion::$module::complete(&mut self.work); }
    }

    /// Only an authorized Batch (or its empty/merged form) enters this adapter.
    /// The owner borrow survives payload movement into the completion backend.
    pub fn start_completion<'domain, P>(batch: Batch<'domain, P>, debt: $word)
        -> (completion: BoundCompletion<'domain, P>)
        requires batch.records().len() <= debt as nat,
        ensures completion.owner() == batch.owner(), completion.inv(), completion.phase() == 0,
            completion.debt() == debt as nat, completion.count() == batch.records().len(),
            !completion.notified(), !completion.locked(),
    {
        BoundCompletion { owner: batch.owner, work: super::super::completion::$module::Completion::new(batch.records, debt) }
    }

    #[verifier::exec_allows_no_decreases_clause]
    pub fn ordinary_locked<'domain, P>(lock: &super::super::rotation::barrier_ownership::QueueLock<P>,
        owner: &'domain DomainOwner, certificate: &Drained<'_>) -> (batch: Option<LockedBatch<'domain, P>>)
        requires certificate.inv(), lock.pred().domain == owner.rotation, lock.pred().index == (certificate.index() == 1),
        ensures batch.is_some() == (owner.rotation.addr() == certificate.state().domain.addr()),
            batch.is_some() ==> batch.unwrap().inv() && batch.unwrap().owner() == owner
                && batch.unwrap().index() == (certificate.index() == 1),
            batch.is_some() ==> forall|i: int| 0 <= i < batch.unwrap().records().len()
                ==> (lock.pred().payload_inv)(#[trigger] batch.unwrap().records()[i]),
    {
        match super::super::rotation::locked_detachment::$module::detach(lock, owner.rotation, certificate) {
            Some(withdrawal) => Some(bind_withdrawal(owner, withdrawal)),
            None => None,
        }
    }
    #[verifier::exec_allows_no_decreases_clause]
    pub fn terminal_locked<'domain, P>(zero: &super::super::rotation::barrier_ownership::QueueLock<P>,
        one: &super::super::rotation::barrier_ownership::QueueLock<P>, owner: &'domain DomainOwner,
        certificate: &Closed<'_>) -> (batches: Option<(LockedBatch<'domain, P>, LockedBatch<'domain, P>)>)
        requires certificate.inv(), zero.pred().domain == owner.rotation, one.pred().domain == owner.rotation,
            !zero.pred().index, one.pred().index,
        ensures batches.is_some() == (owner.rotation.addr() == certificate.state().domain.addr()),
            batches.is_some() ==> batches.unwrap().0.inv() && batches.unwrap().1.inv()
                && batches.unwrap().0.owner() == owner && batches.unwrap().1.owner() == owner
                && !batches.unwrap().0.index() && batches.unwrap().1.index(),
            batches.is_some() ==> forall|i: int| 0 <= i < batches.unwrap().0.records().len()
                ==> (zero.pred().payload_inv)(#[trigger] batches.unwrap().0.records()[i]),
            batches.is_some() ==> forall|i: int| 0 <= i < batches.unwrap().1.records().len()
                ==> (one.pred().payload_inv)(#[trigger] batches.unwrap().1.records()[i]),
    {
        match super::super::rotation::locked_detachment::$module::terminal(zero, one, owner.rotation, certificate) {
            Some((first, second)) => Some((bind_withdrawal(owner, first), bind_withdrawal(owner, second))),
            None => None,
        }
    }
    /// Construction transports the exact payload returned by the already
    /// verified shared detachment; a pending Vec alone cannot construct Batch.
    pub fn ordinary<'domain, P>(queue: &mut Registration<P>, owner: &'domain DomainOwner,
        certificate: &Drained<'_>) -> (batch: Option<Batch<'domain, P>>)
        requires certificate.inv(), old(queue).held.is_none(), !old(queue).both_held,
            old(queue).owner == owner.rotation,
        ensures final(queue).held.is_none(), !final(queue).both_held, final(queue).owner == old(queue).owner,
            batch.is_some() == (owner.rotation.addr() == certificate.state().domain.addr()),
            match batch {
                Some(batch) => batch.owner() == owner
                    && batch.records() == if certificate.index() == 1 { old(queue).one@ } else { old(queue).zero@ }
                    && final(queue).zero@ == if certificate.index() == 1 { old(queue).zero@ } else { Seq::empty() }
                    && final(queue).one@ == if certificate.index() == 1 { Seq::empty() } else { old(queue).one@ },
                None => final(queue).zero@ == old(queue).zero@ && final(queue).one@ == old(queue).one@,
            },
    {
        match super::super::rotation::detachment::$module::detach(queue, certificate) {
            Some(records) => Some(Batch { owner, records }),
            None => None,
        }
    }

    pub fn terminal<'domain, P>(queue: &mut Registration<P>, owner: &'domain DomainOwner,
        certificate: &Closed<'_>) -> (batches: Option<[Batch<'domain, P>; 2]>)
        requires certificate.inv(), old(queue).held.is_none(), !old(queue).both_held,
            old(queue).owner == owner.rotation,
        ensures final(queue).held.is_none(), !final(queue).both_held, final(queue).owner == old(queue).owner,
            batches.is_some() == (owner.rotation.addr() == certificate.state().domain.addr()),
            match batches {
                Some(pair) => pair[0].owner() == owner && pair[1].owner() == owner
                    && pair[0].records() == old(queue).zero@ && pair[1].records() == old(queue).one@
                    && final(queue).zero@ == Seq::<P>::empty() && final(queue).one@ == Seq::<P>::empty(),
                None => final(queue).zero@ == old(queue).zero@ && final(queue).one@ == old(queue).one@,
            },
    {
        match super::super::rotation::detachment::$module::detach_terminal(queue, certificate) {
            Some(mut pair) => {
                let mut zero = Vec::new();
                let mut one = Vec::new();
                core::mem::swap(&mut pair[0], &mut zero);
                core::mem::swap(&mut pair[1], &mut one);
                Some([Batch { owner, records: zero }, Batch { owner, records: one }])
            },
            None => None,
        }
    }
    }
    }
    };
}
detach!(word32, u32);
detach!(word64, u64);

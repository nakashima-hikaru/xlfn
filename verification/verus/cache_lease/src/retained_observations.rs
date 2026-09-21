//! Observations own admission shares, so a node ledger can outlive any one borrow.
use vstd::prelude::*;
use verus_state_machines_macros::tokenized_state_machine;
use super::heap_permission::HeapPermission;
use super::pin_ownership::{cache_pins, PinKind};
use super::rotation::drain::permit_shares::{Scope, Share};
use super::rotation::drain::atomic_counter::DrainSet;
verus! {
tokenized_state_machine!(receipts<Perm> {
    fields {
        #[sharding(variable)] pub keys: Map<nat, (vstd::tokens::InstanceId, vstd::tokens::InstanceId, Perm)>,
        #[sharding(map)] pub tickets: Map<nat, (vstd::tokens::InstanceId, vstd::tokens::InstanceId, Perm)>,
    }
    #[invariant] pub fn exact(&self) -> bool { self.keys == self.tickets }
    init! { initialize() { init keys = Map::empty(); init tickets = Map::empty(); } }
    transition! { issue(key: nat, identity: (vstd::tokens::InstanceId, vstd::tokens::InstanceId, Perm)) {
        require(!pre.keys.dom().contains(key));
        update keys = pre.keys.insert(key, identity); add tickets += [key => identity];
    } }
    property! { contains(key: nat, identity: (vstd::tokens::InstanceId, vstd::tokens::InstanceId, Perm)) {
        have tickets >= [key => identity]; assert(pre.keys.dom().contains(key)); assert(pre.keys[key] == identity);
    } }
    transition! { finish(key: nat, identity: (vstd::tokens::InstanceId, vstd::tokens::InstanceId, Perm)) {
        remove tickets -= [key => identity]; update keys = pre.keys.remove(key);
    } }
    #[inductive(initialize)] fn initialize_inductive(post: Self) {}
    #[inductive(issue)] fn issue_inductive(pre: Self, post: Self, key: nat, identity: (vstd::tokens::InstanceId, vstd::tokens::InstanceId, Perm)) {}
    #[inductive(finish)] fn finish_inductive(pre: Self, post: Self, key: nat, identity: (vstd::tokens::InstanceId, vstd::tokens::InstanceId, Perm)) {}
});
tracked struct Observed { share: Share }
pub tracked struct Ticket<T> {
    token: receipts::tickets<HeapPermission<T>>,
    observation: cache_pins::observations<HeapPermission<T>>,
}
impl<T> Ticket<T> {
    #[verifier::type_invariant]
    pub closed spec fn inv(&self) -> bool { self.observation.element() == self.token.value().2 }
    pub closed spec fn node_id(&self) -> vstd::tokens::InstanceId { self.observation.instance_id() }
    pub proof fn observation(tracked &self) -> (tracked observation: &cache_pins::observations<HeapPermission<T>>)
        ensures observation.instance_id() == self.node_id(), observation.element() == self.memory(),
    {
        use_type_invariant(self);
        &self.observation
    }
    pub closed spec fn ledger_id(&self) -> vstd::tokens::InstanceId { self.token.instance_id() }
    pub closed spec fn memory(&self) -> HeapPermission<T> { self.token.value().2 }
    pub closed spec fn scope_id(&self) -> vstd::tokens::InstanceId { self.token.value().1 }
}
pub tracked struct Ledger<T> {
    ghost domain: Set<vstd::tokens::InstanceId>, ghost coverage: Set<vstd::tokens::InstanceId>, ghost next: nat,
    observing: cache_pins::observing<HeapPermission<T>>,
    retired: Option<cache_pins::retired_history<HeapPermission<T>>>,
    entries: Map<nat, Observed>, instance: receipts::Instance<HeapPermission<T>>, keys: receipts::keys<HeapPermission<T>>,
}
impl<T> Ledger<T> {
    pub closed spec fn inv(&self) -> bool {
        self.observing.value() == self.entries.len() && self.coverage.subset_of(self.domain)
        && self.keys.instance_id() == self.instance.id() && self.keys.value().dom() == self.entries.dom()
        && (self.retired.is_some() ==> self.retired.unwrap().instance_id() == self.node_id())
        && (!self.frozen() ==> self.coverage == self.domain)
        && forall|key: nat| #[trigger] self.entries.dom().contains(key) ==> (key < self.next
            && self.entries[key].share.inv()
            && self.coverage.contains(self.entries[key].share.gate_id())
            && self.keys.value()[key].0 == self.entries[key].share.gate_id()
            && self.keys.value()[key].1 == self.entries[key].share.scope_id())
    }
    pub closed spec fn id(&self) -> vstd::tokens::InstanceId { self.instance.id() }
    pub closed spec fn node_id(&self) -> vstd::tokens::InstanceId { self.observing.instance_id() }
    pub closed spec fn domain(&self) -> Set<vstd::tokens::InstanceId> { self.domain }
    pub closed spec fn coverage(&self) -> Set<vstd::tokens::InstanceId> { self.coverage }
    pub closed spec fn frozen(&self) -> bool { self.retired.is_some() }
    pub closed spec fn len(&self) -> nat { self.observing.value() }
    pub proof fn new(tracked node: &cache_pins::Instance<HeapPermission<T>>, tracked observing: cache_pins::observing<HeapPermission<T>>)
        -> (tracked ledger: Self)
        requires observing.instance_id() == node.id(), observing.value() == 0,
        ensures ledger.inv(), ledger.node_id() == node.id(), ledger.domain() == node.domain(),
            ledger.coverage() == node.domain(), !ledger.frozen(), ledger.len() == 0,
    {
        let tracked (Tracked(instance), Tracked(keys), Tracked(tickets)) = receipts::Instance::initialize();
        Ledger { domain: node.domain(), coverage: node.domain(), next: 0, observing, retired: None,
            entries: Map::tracked_empty(), instance, keys }
    }
    pub proof fn observe(tracked &mut self, tracked node: &cache_pins::Instance<HeapPermission<T>>,
        tracked scope: &mut Scope, tracked resident: &cache_pins::pins<HeapPermission<T>>) -> (tracked ticket: Ticket<T>)
        requires old(self).inv(), old(self).node_id() == node.id(), old(self).domain() == node.domain(),
            old(scope).inv(), node.domain().contains(old(scope).gate_id()),
            resident.instance_id() == node.id(), resident.element().1 == PinKind::Resident,
        ensures final(self).inv(), final(self).id() == old(self).id(), final(self).node_id() == old(self).node_id(),
            final(self).domain() == old(self).domain(), final(self).coverage() == old(self).coverage(),
            !old(self).frozen(), !final(self).frozen(), final(self).len() == old(self).len() + 1,
            final(scope).inv(), final(scope).id() == old(scope).id(), final(scope).gate_id() == old(scope).gate_id(),
            final(scope).len() == old(scope).len() + 1, ticket.ledger_id() == final(self).id(), ticket.node_id() == node.id(), ticket.scope_id() == final(scope).id(), ticket.memory() == resident.element().0,
    {
        if self.retired.is_some() { node.live_excludes_retirement(resident.element().0, PinKind::Resident, resident, self.retired.tracked_borrow()); }
        let tracked share = scope.issue();
        let tracked observation = node.observe(resident.element().0, resident, &mut self.observing);
        let key = self.next;
        let tracked token = self.instance.issue(key, (share.gate_id(), share.scope_id(), observation.element()), &mut self.keys);
        self.entries.tracked_insert(key, Observed { share });
        self.next = key + 1;
        Ticket { token, observation }
    }
    pub proof fn end(tracked &mut self, tracked node: &cache_pins::Instance<HeapPermission<T>>,
        tracked scope: &mut Scope, tracked ticket: Ticket<T>)
        requires old(self).inv(), old(self).node_id() == node.id(), ticket.ledger_id() == old(self).id(), ticket.node_id() == node.id(),
            old(scope).inv(), ticket.scope_id() == old(scope).id(),
        ensures final(self).inv(), final(self).id() == old(self).id(), final(self).node_id() == old(self).node_id(),
            final(self).domain() == old(self).domain(), final(self).coverage() == old(self).coverage(), final(self).frozen() == old(self).frozen(),
            final(self).len() + 1 == old(self).len(), final(scope).inv(), final(scope).id() == old(scope).id(),
            final(scope).gate_id() == old(scope).gate_id(), final(scope).len() + 1 == old(scope).len(),
    {
        use_type_invariant(&ticket);
        let key = ticket.token.key();
        let identity = ticket.token.value();
        self.instance.contains(key, identity, &self.keys, &ticket.token);
        let tracked entry = self.entries.tracked_remove(key);
        self.instance.finish(key, identity, &mut self.keys, ticket.token);
        node.leave_observation(ticket.observation.element(), ticket.observation, &mut self.observing);
        scope.finish(entry.share);
    }
    pub proof fn freeze(tracked &mut self, tracked node: &cache_pins::Instance<HeapPermission<T>>,
        tracked ticket: &cache_pins::retirement<HeapPermission<T>>)
        requires old(self).inv(), old(self).node_id() == node.id(), ticket.instance_id() == node.id(),
        ensures final(self).inv(), final(self).id() == old(self).id(), final(self).node_id() == old(self).node_id(),
            final(self).domain() == old(self).domain(), final(self).coverage() == old(self).coverage(),
            final(self).frozen(), final(self).len() == old(self).len(),
    { self.retired = Some(node.remember_retirement(ticket.value(), ticket)); }
    pub proof fn narrow(tracked &mut self, keep: Set<vstd::tokens::InstanceId>, tracked idle: &DrainSet)
        requires old(self).inv(), old(self).frozen(), idle.inv(), idle.covers(old(self).coverage().difference(keep)),
        ensures final(self).inv(), final(self).id() == old(self).id(), final(self).node_id() == old(self).node_id(),
            final(self).domain() == old(self).domain(), final(self).coverage() == old(self).coverage().intersect(keep),
            final(self).frozen(), final(self).len() == old(self).len(),
    {
        if exists|key: nat| self.entries.dom().contains(key) && !keep.contains((#[trigger] self.entries[key]).share.gate_id()) {
            let key = choose|key: nat| self.entries.dom().contains(key) && !keep.contains((#[trigger] self.entries[key]).share.gate_id());
            idle.excludes_share(&self.entries.tracked_borrow(key).share);
        }
        self.coverage = self.coverage.intersect(keep);
    }
    pub proof fn zero_after_drain(tracked &self, tracked drains: &DrainSet)
        requires self.inv(), drains.inv(), drains.covers(self.coverage()), ensures self.len() == 0,
    {
        if self.entries.len() > 0 {
            assert(exists|key: nat| self.entries.dom().contains(key)) by {
                if !(exists|key: nat| self.entries.dom().contains(key)) { assert(self.entries =~= Map::<nat, Observed>::empty()); }
            }
            let key = choose|key: nat| self.entries.dom().contains(key);
            drains.excludes_share(&self.entries.tracked_borrow(key).share);
        }
    }
    pub proof fn coverage_within_domain(tracked &self)
        requires self.inv(), ensures self.coverage().subset_of(self.domain()), {}
    pub proof fn count(tracked &self) -> (tracked count: &cache_pins::observing<HeapPermission<T>>)
        requires self.inv(), ensures count.instance_id() == self.node_id(), count.value() == self.len(),
    { &self.observing }
}
}

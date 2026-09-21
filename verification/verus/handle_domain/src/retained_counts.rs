//! Exact admission-share coverage of the atomic publication counter registry.
use vstd::prelude::*;
use verus_state_machines_macros::tokenized_state_machine;
use super::heap_permission::HeapPermission;
use super::observations::{bindings, OwnedObservation};
use super::publication_counts::{Counts, membership};
use super::rotation::drain::permit_shares::Share;
verus! {
tokenized_state_machine!(read_index {
    fields {
        #[sharding(variable)] pub keys: Map<nat, (vstd::tokens::InstanceId, vstd::tokens::InstanceId)>,
        #[sharding(map)] pub readers: Map<nat, (vstd::tokens::InstanceId, vstd::tokens::InstanceId)>,
        #[sharding(variable)] pub coverage: Set<vstd::tokens::InstanceId>,
        #[sharding(persistent_map)] pub bounds: Map<Set<vstd::tokens::InstanceId>, ()>,
    }
    #[invariant] pub fn exact_keys(&self) -> bool { self.keys == self.readers }
    #[invariant] pub fn covered(&self) -> bool {
        (forall|key: nat| #[trigger] self.keys.dom().contains(key) ==> self.coverage.contains(self.keys[key].0))
        && (forall|bound: Set<vstd::tokens::InstanceId>| #[trigger] self.bounds.dom().contains(bound) ==> self.coverage.subset_of(bound))
    }
    init! { initialize(domain: Set<vstd::tokens::InstanceId>) {
        init keys = Map::empty(); init readers = Map::empty(); init coverage = domain; init bounds = Map::empty();
    } }
    transition! { issue(key: nat, identity: (vstd::tokens::InstanceId, vstd::tokens::InstanceId)) {
        require(!pre.keys.dom().contains(key)); require(pre.coverage.contains(identity.0));
        update keys = pre.keys.insert(key, identity); add readers += [key => identity];
    } }
    transition! { finish(key: nat, identity: (vstd::tokens::InstanceId, vstd::tokens::InstanceId)) {
        remove readers -= [key => identity]; update keys = pre.keys.remove(key);
    } }
    property! { contains(key: nat, identity: (vstd::tokens::InstanceId, vstd::tokens::InstanceId)) { have readers >= [key => identity]; assert(pre.keys.dom().contains(key)); assert(pre.keys[key] == identity); } }
    transition! { narrow(bound: Set<vstd::tokens::InstanceId>) {
        require(forall|key: nat| #[trigger] pre.keys.dom().contains(key) ==> bound.contains(pre.keys[key].0));
        update coverage = pre.coverage.intersect(bound); add bounds (union)= [bound => ()];
    } }
    property! { within(bound: Set<vstd::tokens::InstanceId>) {
        have bounds >= [bound => ()]; assert(pre.coverage.subset_of(bound));
    } }
    #[inductive(narrow)] fn narrow_inductive(pre: Self, post: Self, bound: Set<vstd::tokens::InstanceId>) {}
    #[inductive(initialize)] fn initialize_inductive(post: Self, domain: Set<vstd::tokens::InstanceId>) {}
    #[inductive(issue)] fn issue_inductive(pre: Self, post: Self, key: nat, identity: (vstd::tokens::InstanceId, vstd::tokens::InstanceId)) {}
    #[inductive(finish)] fn finish_inductive(pre: Self, post: Self, key: nat, identity: (vstd::tokens::InstanceId, vstd::tokens::InstanceId)) {}
});
tokenized_state_machine!(indices {
    fields {
        #[sharding(variable)] pub entries: Map<vstd::tokens::InstanceId, vstd::tokens::InstanceId>,
        #[sharding(persistent_map)] pub known: Map<vstd::tokens::InstanceId, vstd::tokens::InstanceId>,
    }
    #[invariant] pub fn exact(&self) -> bool { self.entries == self.known }
    init! { initialize() { init entries = Map::empty(); init known = Map::empty(); } }
    transition! { register(node: vstd::tokens::InstanceId, index: vstd::tokens::InstanceId) {
        require(!pre.entries.dom().contains(node));
        update entries = pre.entries.insert(node, index); add known (union)= [node => index];
    } }
    property! { agrees(node: vstd::tokens::InstanceId, index: vstd::tokens::InstanceId) {
        have known >= [node => index];
        assert(pre.entries.dom().contains(node)); assert(pre.entries[node] == index);
    } }
    #[inductive(initialize)] fn initialize_inductive(post: Self) {}
    #[inductive(register)] fn register_inductive(pre: Self, post: Self, node: vstd::tokens::InstanceId, index: vstd::tokens::InstanceId) {}
});
tracked struct Allocation {
    index: read_index::Instance,
    keys: read_index::keys,
    coverage: read_index::coverage,
    shares: Map<nat, Share>,
    ghost next: nat,
}
impl Allocation {
    closed spec fn inv(&self, domain: Set<vstd::tokens::InstanceId>) -> bool {
        &&& self.keys.instance_id() == self.index.id()
        &&& self.keys.value().dom() == self.shares.dom()
        &&& self.coverage.instance_id() == self.index.id()
        &&& self.coverage.value().subset_of(domain)
        &&& forall|key: nat| #[trigger] self.shares.dom().contains(key) ==> (
            key < self.next && self.shares[key].inv()
            && self.coverage.value().contains(self.shares[key].gate_id())
            && self.keys.value()[key] == (self.shares[key].gate_id(), self.shares[key].scope_id()))
    }
    proof fn new(domain: Set<vstd::tokens::InstanceId>) -> (tracked result: Self)
        ensures result.shares.len() == 0, result.inv(domain), result.coverage.value() == domain,
    {
        let tracked (Tracked(index), Tracked(keys), Tracked(readers), Tracked(coverage), Tracked(bounds)) = read_index::Instance::initialize(domain);
        Allocation { index, keys, coverage, shares: Map::tracked_empty(), next: 0 }
    }
}
pub tracked struct IndexedObservation<T> {
    observation: OwnedObservation<T>,
    ticket: read_index::readers,
    receipt: membership::known,
    index_receipt: indices::known,
    ghost scope: vstd::tokens::InstanceId,
}
impl<T> IndexedObservation<T> {
    pub closed spec fn valid(&self, registry: vstd::tokens::InstanceId,
        index_registry: vstd::tokens::InstanceId, domain: Set<vstd::tokens::InstanceId>) -> bool {
        self.receipt.instance_id() == registry && self.receipt.key() == self.node_id()
        && self.index_receipt.instance_id() == index_registry && self.index_receipt.key() == self.node_id()
        && self.index_receipt.value() == self.ticket.instance_id()
        && self.ticket.value() == (self.gate_id(), self.scope_id())
        && self.observation.domain() == domain
    }
    pub proof fn observation(tracked &self) -> (tracked observation: &OwnedObservation<T>)
        ensures observation.node_id() == self.node_id(), observation.memory() == self.memory(),
    { &self.observation }
    pub closed spec fn node_id(&self) -> vstd::tokens::InstanceId { self.observation.node_id() }
    pub closed spec fn gate_id(&self) -> vstd::tokens::InstanceId { self.observation.gate_id() }
    pub closed spec fn scope_id(&self) -> vstd::tokens::InstanceId { self.scope }
    pub closed spec fn memory(&self) -> HeapPermission<T> { self.observation.memory() }
}
pub tracked struct CoverageBound {
    index_receipt: indices::known,
    token: read_index::bounds,
}
impl CoverageBound {
    pub closed spec fn valid(&self, registry: vstd::tokens::InstanceId, node: vstd::tokens::InstanceId) -> bool {
        self.index_receipt.instance_id() == registry && self.index_receipt.key() == node
        && self.token.instance_id() == self.index_receipt.value()
    }
    pub closed spec fn limit(&self) -> Set<vstd::tokens::InstanceId> { self.token.key() }
}
pub tracked struct RetainedCounts<T> {
    counts: Counts<T>,
    indices: indices::Instance,
    index_entries: indices::entries,
    index_receipts: Map<vstd::tokens::InstanceId, indices::known>,
    allocations: Map<vstd::tokens::InstanceId, Allocation>,
    ghost domain: Set<vstd::tokens::InstanceId>,
    retired: Map<vstd::tokens::InstanceId, bindings::retired_history<HeapPermission<T>>>,
}
impl<T> RetainedCounts<T> {
    pub closed spec fn inv(&self) -> bool {
        self.counts.inv() && self.counts.allocations() == self.allocations.dom()
        && self.index_entries.instance_id() == self.indices.id()
        && self.index_entries.value().dom() == self.allocations.dom()
        && self.index_receipts.dom() == self.allocations.dom()
        && self.retired.dom().subset_of(self.allocations.dom())
        && (forall|id: vstd::tokens::InstanceId| #[trigger] self.retired.dom().contains(id) ==> self.retired[id].instance_id() == id)
        && forall|id: vstd::tokens::InstanceId| self.allocations.dom().contains(id) ==>
            (#[trigger] self.allocations[id]).inv(self.domain)
            && self.counts.value(id) == self.allocations[id].shares.len()
            && (!self.retired.dom().contains(id) ==> self.allocations[id].coverage.value() == self.domain)
            && self.index_entries.value()[id] == self.allocations[id].index.id()
            && self.index_receipts[id].instance_id() == self.indices.id()
            && self.index_receipts[id].key() == id
            && self.index_receipts[id].value() == self.allocations[id].index.id()
    }
    pub closed spec fn frozen(&self, id: vstd::tokens::InstanceId) -> bool { self.retired.dom().contains(id) }
    pub closed spec fn coverage(&self, id: vstd::tokens::InstanceId) -> Set<vstd::tokens::InstanceId> { self.allocations[id].coverage.value() }
    pub proof fn prove_bound(tracked &self, node: vstd::tokens::InstanceId, tracked bound: &CoverageBound)
        requires self.inv(), bound.valid(self.index_registry_id(), node),
        ensures self.contains(node), self.coverage(node).subset_of(bound.limit()),
    {
        self.indices.agrees(node, bound.token.instance_id(), &self.index_entries, &bound.index_receipt);
        let tracked allocation = self.allocations.tracked_borrow(node);
        allocation.index.within(bound.limit(), &allocation.coverage, &bound.token);
    }
    pub closed spec fn all_shares_within(&self, id: vstd::tokens::InstanceId, keep: Set<vstd::tokens::InstanceId>) -> bool {
        forall|key: nat| #[trigger] self.allocations[id].shares.dom().contains(key) ==>
            keep.contains(self.allocations[id].shares[key].gate_id())
    }
    pub proof fn coverage_in_domain(tracked &self, id: vstd::tokens::InstanceId)
        requires self.inv(), self.contains(id), ensures self.coverage(id).subset_of(self.domain()),
    {}
    pub proof fn narrow_from_exclusion(tracked &mut self, id: vstd::tokens::InstanceId, keep: Set<vstd::tokens::InstanceId>)
        -> (tracked bound: CoverageBound)
        requires old(self).inv(), old(self).contains(id), old(self).frozen(id), old(self).all_shares_within(id, keep),
        ensures final(self).inv(), final(self).registry_id() == old(self).registry_id(),
            final(self).index_registry_id() == old(self).index_registry_id(), final(self).domain() == old(self).domain(),
            bound.valid(final(self).index_registry_id(), id), bound.limit() == keep,
            forall|node: vstd::tokens::InstanceId| #[trigger] old(self).contains(node) ==>
                final(self).contains(node) && final(self).value(node) == old(self).value(node),
    {
        let tracked mut allocation = self.allocations.tracked_remove(id);
        assert forall|key: nat| #[trigger] allocation.keys.value().dom().contains(key) implies
            keep.contains(allocation.keys.value()[key].0) by {
            assert(keep.contains(allocation.shares[key].gate_id()));
        }
        let tracked token = allocation.index.narrow(keep, &allocation.keys, &mut allocation.coverage);
        assert forall|key: nat| #[trigger] allocation.shares.dom().contains(key) implies
            allocation.coverage.value().contains(allocation.shares[key].gate_id()) by {
            assert(keep.contains(allocation.keys.value()[key].0));
        }
        assert(allocation.inv(self.domain));
        self.allocations.tracked_insert(id, allocation);
        CoverageBound { index_receipt: *self.index_receipts.tracked_borrow(id), token }
    }
    pub proof fn note_retired(tracked &mut self, tracked history: bindings::retired_history<HeapPermission<T>>)
        requires old(self).inv(), old(self).contains(history.instance_id()),
        ensures final(self).inv(), final(self).frozen(history.instance_id()),
            final(self).registry_id() == old(self).registry_id(), final(self).index_registry_id() == old(self).index_registry_id(),
            final(self).domain() == old(self).domain(),
            forall|id: vstd::tokens::InstanceId| #[trigger] old(self).contains(id) ==> final(self).contains(id)
                && final(self).value(id) == old(self).value(id) && final(self).coverage(id) == old(self).coverage(id),
    { self.retired.tracked_insert(history.instance_id(), history); }
    pub closed spec fn index_registry_id(&self) -> vstd::tokens::InstanceId { self.indices.id() }
    pub closed spec fn registry_id(&self) -> vstd::tokens::InstanceId { self.counts.registry_id() }
    pub closed spec fn domain(&self) -> Set<vstd::tokens::InstanceId> { self.domain }
    pub closed spec fn contains(&self, id: vstd::tokens::InstanceId) -> bool { self.counts.contains(id) }
    pub closed spec fn value(&self, id: vstd::tokens::InstanceId) -> nat { self.counts.value(id) }
    pub closed spec fn accepts(&self, observation: IndexedObservation<T>) -> bool {
        self.contains(observation.node_id())
        && observation.receipt.instance_id() == self.registry_id()
        && observation.receipt.key() == observation.node_id()
        && observation.ticket.instance_id() == self.allocations[observation.node_id()].index.id()
        && observation.observation.domain() == self.domain
        && self.allocations[observation.node_id()].shares.contains_key(observation.ticket.key())
        && self.allocations[observation.node_id()].shares[observation.ticket.key()].gate_id() == observation.gate_id()
        && self.allocations[observation.node_id()].shares[observation.ticket.key()].scope_id() == observation.scope_id()
    }
    pub proof fn new(domain: Set<vstd::tokens::InstanceId>) -> (tracked result: Self)
        ensures result.inv(), result.domain() == domain, forall|id: vstd::tokens::InstanceId| !result.contains(id),
    {
        let tracked counts = Counts::new();
        assert forall|id: vstd::tokens::InstanceId| !counts.allocations().contains(id) by { assert(!counts.contains(id)); }
        assert(counts.allocations() =~= Set::<vstd::tokens::InstanceId>::empty());
        let tracked (Tracked(indices), Tracked(index_entries), Tracked(known)) = indices::Instance::initialize();
        RetainedCounts { counts, indices, index_entries, index_receipts: Map::tracked_empty(), allocations: Map::tracked_empty(), domain, retired: Map::tracked_empty() }
    }
    pub proof fn remembered(tracked &self, tracked receipt: &membership::known)
        requires self.inv(), receipt.instance_id() == self.registry_id(),
        ensures self.contains(receipt.key()),
    { self.counts.remembered(receipt); }
    pub proof fn recover(tracked &self, tracked instance: &bindings::Instance<HeapPermission<T>>,
        tracked retired: bindings::retired<HeapPermission<T>>)
        -> (tracked result: Result<HeapPermission<T>, bindings::retired<HeapPermission<T>>>)
        requires self.inv(), retired.instance_id() == instance.id(),
        ensures result is Ok ==> result->Ok_0 == retired.value(),
            result is Err ==> result->Err_0 == retired,
            (result is Ok) == (self.contains(instance.id()) && self.value(instance.id()) == 0),
    { self.counts.recover(instance, retired) }
    pub proof fn observed_is_positive(tracked &self,
        tracked observation: &IndexedObservation<T>)
        requires self.inv(), observation.valid(self.registry_id(), self.index_registry_id(), self.domain()),
        ensures self.contains(observation.node_id()), self.value(observation.node_id()) > 0,
    {
        self.validate(observation);
        let tracked allocation = self.allocations.tracked_borrow(observation.node_id());
        assert(allocation.shares.len() > 0) by {
            if allocation.shares.len() == 0 { assert(allocation.shares =~= Map::<nat, Share>::empty()); }
        }
    }
    pub proof fn register(tracked &mut self, tracked count: bindings::observing<HeapPermission<T>>)
        -> (tracked receipt: membership::known)
        requires old(self).inv(), !old(self).contains(count.instance_id()), count.value() == 0,
        ensures forall|id: vstd::tokens::InstanceId| #[trigger] old(self).contains(id) ==> final(self).contains(id),
            final(self).inv(), final(self).registry_id() == old(self).registry_id(), final(self).index_registry_id() == old(self).index_registry_id(),
            final(self).domain() == old(self).domain(), final(self).contains(count.instance_id()),
            final(self).value(count.instance_id()) == 0,
            receipt.instance_id() == final(self).registry_id(), receipt.key() == count.instance_id(),
    {

        let tracked receipt = self.counts.register(count);
        let tracked allocation = Allocation::new(self.domain);
        assert(allocation.inv(self.domain));
        let tracked index_receipt = self.indices.register(count.instance_id(), allocation.index.id(), &mut self.index_entries);
        self.index_receipts.tracked_insert(count.instance_id(), index_receipt);
        self.allocations.tracked_insert(count.instance_id(), allocation);
        assert(self.counts.allocations() =~= self.allocations.dom());
        assert forall|id: vstd::tokens::InstanceId| self.allocations.dom().contains(id) implies
            (#[trigger] self.allocations[id]).inv(self.domain)
            && self.counts.value(id) == self.allocations[id].shares.len() by {
            if id != count.instance_id() {
                assert(old(self).counts.contains(id));
                assert(old(self).allocations[id].inv(old(self).domain));
                assert(self.counts.value(id) == old(self).counts.value(id));
            }
        }
        receipt
    }
    pub proof fn observe(tracked &mut self,
        tracked instance: &bindings::Instance<HeapPermission<T>>,
        tracked published: &bindings::published<HeapPermission<T>>,
        tracked receipt: &membership::known, tracked share: Share)
        -> (tracked observation: IndexedObservation<T>)
        requires old(self).inv(), receipt.instance_id() == old(self).registry_id(), receipt.key() == instance.id(),
            published.instance_id() == instance.id(), instance.domain() == old(self).domain(),
            share.inv(), old(self).domain().contains(share.gate_id()),
        ensures forall|id: vstd::tokens::InstanceId| #[trigger] old(self).contains(id) ==> final(self).contains(id),
            final(self).inv(), final(self).registry_id() == old(self).registry_id(), final(self).index_registry_id() == old(self).index_registry_id(), final(self).domain() == old(self).domain(),
            final(self).accepts(observation), observation.valid(final(self).registry_id(), final(self).index_registry_id(), final(self).domain()), observation.node_id() == instance.id(),
            observation.memory() == published.value(), observation.gate_id() == share.gate_id(), observation.scope_id() == share.scope_id(),
            final(self).contains(instance.id()), final(self).value(instance.id()) == old(self).value(instance.id()) + 1,
    {

        self.counts.remembered(receipt);
        if self.retired.dom().contains(instance.id()) {
            instance.publication_excludes_retirement(published.value(), published, self.retired.tracked_borrow(instance.id()));
        }
        assert(!self.retired.dom().contains(instance.id()));
        let tracked mut allocation = self.allocations.tracked_remove(instance.id());
        let key = allocation.next;
        assert(!allocation.shares.dom().contains(key)) by {
            if allocation.shares.dom().contains(key) { assert(allocation.shares[key].inv()); }
        }
        assert(!allocation.keys.value().dom().contains(key));
        let tracked ticket = allocation.index.issue(key, (share.gate_id(), share.scope_id()), &mut allocation.keys, &allocation.coverage);
        let tracked observation = self.counts.observe(instance, published, receipt, share.permit()).into_owned();
        let scope = share.scope_id();
        allocation.shares.tracked_insert(key, share);
        allocation.next = key + 1;
        assert(allocation.keys.value().dom() =~= allocation.shares.dom());
        assert forall|k: nat| #[trigger] allocation.shares.dom().contains(k) implies k < allocation.next
            && allocation.shares[k].inv() && allocation.coverage.value().contains(allocation.shares[k].gate_id())
            && allocation.keys.value()[k] == (allocation.shares[k].gate_id(), allocation.shares[k].scope_id()) by {
            if k != key { assert(old(self).allocations[instance.id()].shares[k].inv()); }
        }
        assert(allocation.keys.instance_id() == allocation.index.id());
        assert(allocation.keys.value().dom() == allocation.shares.dom());
        assert(allocation.inv(self.domain));
        self.allocations.tracked_insert(instance.id(), allocation);
        assert(self.counts.allocations() =~= self.allocations.dom());
        assert forall|id: vstd::tokens::InstanceId| self.allocations.dom().contains(id) implies
            (#[trigger] self.allocations[id]).inv(self.domain)
            && self.counts.value(id) == self.allocations[id].shares.len() by {
            if id != instance.id() {
                assert(old(self).counts.contains(id));
                assert(old(self).allocations[id].inv(old(self).domain));
                assert(self.counts.value(id) == old(self).counts.value(id));
            }
        }
        IndexedObservation { observation, ticket, receipt: *receipt, index_receipt: *self.index_receipts.tracked_borrow(instance.id()), scope }
    }
    /// Stable receipts and the live linear reader ticket derive membership;
    /// callers do not assume the mutable share-map contents at completion.
    pub proof fn validate(tracked &self, tracked observation: &IndexedObservation<T>)
        requires self.inv(), observation.valid(self.registry_id(), self.index_registry_id(), self.domain()),
        ensures self.accepts(*observation),
    {
        self.counts.remembered(&observation.receipt);
        self.indices.agrees(observation.node_id(), observation.ticket.instance_id(), &self.index_entries, &observation.index_receipt);
        let tracked allocation = self.allocations.tracked_borrow(observation.node_id());
        allocation.index.contains(observation.ticket.key(), observation.ticket.value(), &allocation.keys, &observation.ticket);
    }
    pub proof fn end(tracked &mut self,
        tracked instance: &bindings::Instance<HeapPermission<T>>,
        tracked observation: IndexedObservation<T>) -> (tracked share: Share)
        requires old(self).inv(), observation.valid(old(self).registry_id(), old(self).index_registry_id(), old(self).domain()), observation.node_id() == instance.id(),
        ensures forall|id: vstd::tokens::InstanceId| #[trigger] old(self).contains(id) ==> final(self).contains(id),
            final(self).inv(), final(self).registry_id() == old(self).registry_id(), final(self).index_registry_id() == old(self).index_registry_id(), final(self).domain() == old(self).domain(),
            share.inv(), share.gate_id() == observation.gate_id(), share.scope_id() == observation.scope_id(),
            final(self).contains(instance.id()), final(self).value(instance.id()) + 1 == old(self).value(instance.id()),
    {
        self.validate(&observation);
        let tracked mut allocation = self.allocations.tracked_remove(instance.id());
        let key = observation.ticket.key();
        let tracked share = allocation.shares.tracked_remove(key);
        let tracked borrowed = observation.observation.with_permit(share.permit());
        self.counts.end(instance, &observation.receipt, borrowed);
        allocation.index.finish(key, observation.ticket.value(), &mut allocation.keys, observation.ticket);
        assert(allocation.keys.value().dom() =~= allocation.shares.dom());
        assert forall|k: nat| #[trigger] allocation.shares.dom().contains(k) implies k < allocation.next
            && allocation.shares[k].inv() && allocation.coverage.value().contains(allocation.shares[k].gate_id())
            && allocation.keys.value()[k] == (allocation.shares[k].gate_id(), allocation.shares[k].scope_id()) by {
            if k != key { assert(old(self).allocations[instance.id()].shares[k].inv()); }
        }
        assert(allocation.keys.instance_id() == allocation.index.id());
        assert(allocation.keys.value().dom() == allocation.shares.dom());
        assert(allocation.inv(self.domain));
        self.allocations.tracked_insert(instance.id(), allocation);
        assert(self.counts.allocations() =~= self.allocations.dom());
        assert forall|id: vstd::tokens::InstanceId| self.allocations.dom().contains(id) implies
            (#[trigger] self.allocations[id]).inv(self.domain)
            && self.counts.value(id) == self.allocations[id].shares.len() by {
            if id != instance.id() {
                assert(old(self).counts.contains(id));
                assert(old(self).allocations[id].inv(old(self).domain));
                assert(self.counts.value(id) == old(self).counts.value(id));
            }
        }
        share
    }
}
}
macro_rules! drain {
    ($module:ident, $word:ty) => {
    pub mod $module {
    use super::*;
    use super::super::rotation::drain::stripe_ownership::$module as stripes;
    verus! {
    pub proof fn narrow_before_rotation<T>(tracked counts: &mut RetainedCounts<T>, id: vstd::tokens::InstanceId,
        rotation: &super::super::rotation::refinement::$module::Rotation,
        tracked ledgers: &super::super::rotation::refinement::$module::GenerationLedgers)
        -> (tracked bound: CoverageBound)
        requires old(counts).inv(), old(counts).contains(id), old(counts).frozen(id),
            rotation.inv(), rotation.pending.is_none(), ledgers.matches(rotation),
            old(counts).domain() == super::super::coverage::$module::generation_domain(ledgers),
        ensures final(counts).inv(), final(counts).registry_id() == old(counts).registry_id(),
            final(counts).index_registry_id() == old(counts).index_registry_id(), final(counts).domain() == old(counts).domain(),
            bound.valid(final(counts).index_registry_id(), id),
            bound.limit() == Set::empty().insert(ledgers.selected_id(rotation.current)),
            forall|node: vstd::tokens::InstanceId| #[trigger] old(counts).contains(node) ==>
                final(counts).contains(node) && final(counts).value(node) == old(counts).value(node),
    {
        reveal(RetainedCounts::inv); reveal(RetainedCounts::contains); reveal(RetainedCounts::frozen);
        reveal(RetainedCounts::registry_id); reveal(RetainedCounts::index_registry_id);
        reveal(RetainedCounts::domain); reveal(RetainedCounts::value); reveal(Allocation::inv);
        let keep = Set::empty().insert(ledgers.selected_id(rotation.current));
        let tracked allocation = counts.allocations.tracked_borrow(id);
        if exists|key: nat| allocation.shares.dom().contains(key)
            && !keep.contains((#[trigger] allocation.shares[key]).gate_id()) {
            let key = choose|key: nat| allocation.shares.dom().contains(key)
                && !keep.contains((#[trigger] allocation.shares[key]).gate_id());
            let tracked share = allocation.shares.tracked_borrow(key);
            assert(share.gate_id() == ledgers.selected_id(!rotation.current));
            rotation.idle_excludes_generation_permit(!rotation.current, ledgers, share.permit());
        }
        reveal(RetainedCounts::all_shares_within);
        assert(counts.all_shares_within(id, keep)) by {
            assert forall|key: nat| #[trigger] allocation.shares.dom().contains(key) implies
                keep.contains(allocation.shares[key].gate_id()) by {}
        }
        counts.narrow_from_exclusion(id, keep)
    }
    /// Exclude every stripe of the idle generation; keep the complete other
    /// generation's identity set, rather than one representative logical gate.
    pub proof fn narrow_after_idle_stripes<T>(tracked counts: &mut RetainedCounts<T>, id: vstd::tokens::InstanceId,
        keep: Set<vstd::tokens::InstanceId>, histories: Seq<Seq<$word>>, tracked idle: &stripes::StripeLedgers)
        -> (tracked bound: CoverageBound)
        requires old(counts).inv(), old(counts).contains(id), old(counts).frozen(id),
            stripes::drained_histories(histories), idle.matches(stripes::final_states(histories)),
            super::super::coverage::$module::covers(old(counts).domain().difference(keep), idle),
        ensures final(counts).inv(), final(counts).registry_id() == old(counts).registry_id(),
            final(counts).index_registry_id() == old(counts).index_registry_id(), final(counts).domain() == old(counts).domain(),
            bound.valid(final(counts).index_registry_id(), id), bound.limit() == keep,
            forall|node: vstd::tokens::InstanceId| #[trigger] old(counts).contains(node) ==>
                final(counts).contains(node) && final(counts).value(node) == old(counts).value(node),
    {
        reveal(RetainedCounts::inv); reveal(RetainedCounts::contains); reveal(RetainedCounts::domain);
        reveal(RetainedCounts::all_shares_within); reveal(Allocation::inv);
        let tracked allocation = counts.allocations.tracked_borrow(id);
        if exists|key: nat| allocation.shares.dom().contains(key)
            && !keep.contains((#[trigger] allocation.shares[key]).gate_id()) {
            let key = choose|key: nat| allocation.shares.dom().contains(key)
                && !keep.contains((#[trigger] allocation.shares[key]).gate_id());
            let tracked share = allocation.shares.tracked_borrow(key);
            assert(counts.domain().difference(keep).contains(share.gate_id()));
            let stripe = choose|stripe: int| #![auto] 0 <= stripe < idle.gates.len()
                && idle.gates[stripe].id() == share.gate_id();
            stripes::histories_exclude_permit(histories, idle, share.permit(), stripe);
        }
        assert(counts.all_shares_within(id, keep)) by {
            assert forall|key: nat| #[trigger] allocation.shares.dom().contains(key) implies
                keep.contains(allocation.shares[key].gate_id()) by {}
        }
        counts.narrow_from_exclusion(id, keep)
    }
    pub proof fn zero_after_pending<T>(tracked counts: &RetainedCounts<T>, id: vstd::tokens::InstanceId,
        rotation: &super::super::rotation::refinement::$module::Rotation,
        tracked ledgers: &super::super::rotation::refinement::$module::GenerationLedgers)
        requires counts.inv(), counts.contains(id), rotation.inv(), ledgers.matches(rotation),
            rotation.pending == Some(!rotation.current), rotation.idle(!rotation.current),
            counts.coverage(id).subset_of(Set::empty().insert(ledgers.selected_id(!rotation.current))),
        ensures counts.value(id) == 0,
    {
        reveal(RetainedCounts::inv); reveal(RetainedCounts::contains); reveal(RetainedCounts::coverage);
        reveal(RetainedCounts::value); reveal(Allocation::inv);
        let tracked allocation = counts.allocations.tracked_borrow(id);
        if allocation.shares.len() > 0 {
            assert(exists|key: nat| allocation.shares.dom().contains(key)) by {
                if !(exists|key: nat| allocation.shares.dom().contains(key)) {
                    assert(allocation.shares =~= Map::<nat, Share>::empty());
                }
            }
            let key = choose|key: nat| allocation.shares.dom().contains(key);
            let tracked share = allocation.shares.tracked_borrow(key);
            assert(share.gate_id() == ledgers.selected_id(!rotation.current));
            rotation.idle_excludes_generation_permit(!rotation.current, ledgers, share.permit());
        }
    }
    pub proof fn recover_after_pending<T>(tracked counts: &RetainedCounts<T>,
        tracked instance: &bindings::Instance<HeapPermission<T>>,
        tracked retired: bindings::retired<HeapPermission<T>>, tracked bound: &CoverageBound,
        rotation: &super::super::rotation::refinement::$module::Rotation,
        tracked ledgers: &super::super::rotation::refinement::$module::GenerationLedgers)
        -> (tracked memory: HeapPermission<T>)
        requires counts.inv(), retired.instance_id() == instance.id(), bound.valid(counts.index_registry_id(), instance.id()),
            rotation.inv(), ledgers.matches(rotation), rotation.pending == Some(!rotation.current), rotation.idle(!rotation.current),
            bound.limit().subset_of(Set::empty().insert(ledgers.selected_id(!rotation.current))),
        ensures memory == retired.value(),
    {
        counts.prove_bound(instance.id(), bound);
        zero_after_pending(counts, instance.id(), rotation, ledgers);
        reveal(RetainedCounts::inv); reveal(RetainedCounts::contains); reveal(RetainedCounts::value);
        let tracked memory = counts.counts.recover(instance, retired);
        match memory { Ok(memory) => memory, Err(_) => { assert(false); proof_from_false() } }
    }
    pub proof fn zero_after_histories<T>(tracked counts: &RetainedCounts<T>,
        id: vstd::tokens::InstanceId, histories: Seq<Seq<$word>>, tracked ledgers: &stripes::StripeLedgers)
        requires counts.inv(), counts.contains(id), stripes::drained_histories(histories),
            ledgers.matches(stripes::final_states(histories)),
            super::super::coverage::$module::covers(counts.coverage(id), ledgers),
        ensures counts.value(id) == 0,
    {
        reveal(RetainedCounts::inv); reveal(RetainedCounts::contains); reveal(RetainedCounts::domain);
        reveal(RetainedCounts::value); reveal(RetainedCounts::coverage);
        reveal(Allocation::inv);
        let tracked allocation = counts.allocations.tracked_borrow(id);
        if allocation.shares.len() > 0 {
            assert(exists|key: nat| allocation.shares.dom().contains(key)) by {
                if !(exists|key: nat| allocation.shares.dom().contains(key)) {
                    assert(allocation.shares =~= Map::<nat, Share>::empty());
                }
            }
            let key = choose|key: nat| allocation.shares.dom().contains(key);
            let tracked share = allocation.shares.tracked_borrow(key);
            let index = choose|index: int| #![auto] 0 <= index < ledgers.gates.len()
                && ledgers.gates[index].id() == share.gate_id();
            stripes::histories_exclude_permit(histories, ledgers, share.permit(), index);
        }
    }
    pub proof fn recover_after_bounded_stripes<T>(tracked counts: &RetainedCounts<T>,
        tracked instance: &bindings::Instance<HeapPermission<T>>,
        tracked retired: bindings::retired<HeapPermission<T>>, tracked bound: &CoverageBound,
        histories: Seq<Seq<$word>>, tracked pending: &stripes::StripeLedgers)
        -> (tracked memory: HeapPermission<T>)
        requires counts.inv(), retired.instance_id() == instance.id(), bound.valid(counts.index_registry_id(), instance.id()),
            stripes::drained_histories(histories), pending.matches(stripes::final_states(histories)),
            super::super::coverage::$module::covers(bound.limit(), pending),
        ensures memory == retired.value(),
    {
        counts.prove_bound(instance.id(), bound);
        assert(super::super::coverage::$module::covers(counts.coverage(instance.id()), pending));
        zero_after_histories(counts, instance.id(), histories, pending);
        reveal(RetainedCounts::inv); reveal(RetainedCounts::contains); reveal(RetainedCounts::value);
        let tracked memory = counts.counts.recover(instance, retired);
        match memory { Ok(memory) => memory, Err(_) => { assert(false); proof_from_false() } }
    }
    pub proof fn recover_after_histories<T>(tracked counts: &RetainedCounts<T>,
        tracked instance: &bindings::Instance<HeapPermission<T>>,
        tracked retired: bindings::retired<HeapPermission<T>>,
        histories: Seq<Seq<$word>>, tracked ledgers: &stripes::StripeLedgers)
        -> (tracked memory: HeapPermission<T>)
        requires counts.inv(), counts.contains(instance.id()), retired.instance_id() == instance.id(),
            stripes::drained_histories(histories), ledgers.matches(stripes::final_states(histories)),
            super::super::coverage::$module::covers(counts.domain(), ledgers),
        ensures memory == retired.value(),
    {
        counts.coverage_in_domain(instance.id());
        assert(super::super::coverage::$module::covers(counts.coverage(instance.id()), ledgers));
        zero_after_histories(counts, instance.id(), histories, ledgers);
        reveal(RetainedCounts::inv); reveal(RetainedCounts::contains); reveal(RetainedCounts::value);
        let tracked result = counts.counts.recover(instance, retired);
        match result { Ok(memory) => memory, Err(_) => { assert(false); proof_from_false() } }
    }
    }
    }
    };
}
drain!(word32, u32);
drain!(word64, u64);

verus! {
/// Non-vacuous composition: allocate a binding and an actual gate admission,
/// retain one owned observation, retire it, end it, release that same admission,
/// and recover the original initialized heap permission through Counts.
pub proof fn retained_admission_recovery<T>(tracked memory: HeapPermission<T>,
    owner: *const u8, tracked gate: &super::rotation::gate_permits::admission::Instance,
    tracked active: &mut super::rotation::gate_permits::admission::active)
    -> (tracked result: HeapPermission<T>)
    requires memory.is_init(), old(active).instance_id() == gate.id(), old(active).value() == 0,
    ensures result == memory, final(active).instance_id() == gate.id(), final(active).value() == 0,
{
    let tracked permit = gate.acquire(active);
    let tracked mut scope = super::rotation::drain::permit_shares::Scope::new(permit);
    let domain = Set::empty().insert(gate.id());
    let tracked (Tracked(instance), Tracked(published), Tracked(retired), Tracked(entries), Tracked(count), Tracked(history))
        = bindings::Instance::allocate(memory, domain, owner, Some(memory));
    let tracked published = published.tracked_unwrap();
    let tracked mut counts = RetainedCounts::new(domain);
    let tracked receipt = counts.register(count);
    let tracked share = scope.issue();
    let tracked observation = counts.observe(&instance, &published, &receipt, share);
    let tracked (Tracked(retired), Tracked(history)) = instance.retire(memory, published);
    let tracked share = counts.end(&instance, observation);
    scope.finish(share);
    gate.release(active, scope.close());
    let tracked recovered = counts.counts.recover(&instance, retired);
    match recovered { Ok(memory) => memory, Err(_) => { assert(false); proof_from_false() } }
}
}

verus! {
pub proof fn zero_after_drain_leases<T>(tracked counts: &RetainedCounts<T>, id: vstd::tokens::InstanceId,
    tracked drains: &super::rotation::drain::atomic_counter::DrainSet)
    requires counts.inv(), counts.contains(id), drains.inv(), drains.covers(counts.coverage(id)),
    ensures counts.value(id) == 0,
{
    reveal(RetainedCounts::inv); reveal(RetainedCounts::contains); reveal(RetainedCounts::value);
    reveal(RetainedCounts::coverage); reveal(Allocation::inv);
    let tracked allocation = counts.allocations.tracked_borrow(id);
    if allocation.shares.len() > 0 {
        assert(exists|key: nat| allocation.shares.dom().contains(key)) by {
            if !(exists|key: nat| allocation.shares.dom().contains(key)) {
                assert(allocation.shares =~= Map::<nat, Share>::empty());
            }
        }
        let key = choose|key: nat| allocation.shares.dom().contains(key);
        let tracked share = allocation.shares.tracked_borrow(key);
        drains.excludes_share(share);
    }
}
pub proof fn recover_after_drain_leases<T>(tracked counts: &RetainedCounts<T>,
    tracked instance: &bindings::Instance<HeapPermission<T>>, tracked retired: bindings::retired<HeapPermission<T>>,
    tracked drains: &super::rotation::drain::atomic_counter::DrainSet) -> (tracked memory: HeapPermission<T>)
    requires counts.inv(), counts.contains(instance.id()), retired.instance_id() == instance.id(),
        drains.inv(), drains.covers(counts.coverage(instance.id())),
    ensures memory == retired.value(),
{
    zero_after_drain_leases(counts, instance.id(), drains);
    reveal(RetainedCounts::inv); reveal(RetainedCounts::contains); reveal(RetainedCounts::value);
    let tracked memory = counts.counts.recover(instance, retired);
    match memory { Ok(memory) => memory, Err(_) => { assert(false); proof_from_false() } }
}
}

verus! {
pub proof fn narrow_after_drain_leases<T>(tracked counts: &mut RetainedCounts<T>, id: vstd::tokens::InstanceId,
        keep: Set<vstd::tokens::InstanceId>, tracked idle: &super::rotation::drain::atomic_counter::DrainSet)
        -> (tracked bound: CoverageBound)
        requires old(counts).inv(), old(counts).contains(id), old(counts).frozen(id),
            idle.inv(), idle.covers(old(counts).domain().difference(keep)),
        ensures final(counts).inv(), final(counts).registry_id() == old(counts).registry_id(),
            final(counts).index_registry_id() == old(counts).index_registry_id(), final(counts).domain() == old(counts).domain(),
            bound.valid(final(counts).index_registry_id(), id), bound.limit() == keep,
            forall|node: vstd::tokens::InstanceId| #[trigger] old(counts).contains(node) ==>
                final(counts).contains(node) && final(counts).value(node) == old(counts).value(node),
    {
        reveal(RetainedCounts::inv); reveal(RetainedCounts::contains); reveal(RetainedCounts::domain);
        reveal(RetainedCounts::all_shares_within); reveal(Allocation::inv);
        let tracked allocation = counts.allocations.tracked_borrow(id);
        if exists|key: nat| allocation.shares.dom().contains(key)
            && !keep.contains((#[trigger] allocation.shares[key]).gate_id()) {
            let key = choose|key: nat| allocation.shares.dom().contains(key)
                && !keep.contains((#[trigger] allocation.shares[key]).gate_id());
            let tracked share = allocation.shares.tracked_borrow(key);
            assert(counts.domain().difference(keep).contains(share.gate_id()));
            idle.excludes_share(share);
        }
        assert(counts.all_shares_within(id, keep)) by {
            assert forall|key: nat| #[trigger] allocation.shares.dom().contains(key) implies
                keep.contains(allocation.shares[key].gate_id()) by {}
        }
        counts.narrow_from_exclusion(id, keep)
    }
}

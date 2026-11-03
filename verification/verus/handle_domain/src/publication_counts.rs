//! Per-allocation observation counters survive publication-slot reuse.
use vstd::prelude::*;
use verus_state_machines_macros::tokenized_state_machine;
use super::heap_permission::HeapPermission;
use super::observations::{bindings, Observation};
use super::rotation::gate_permits::admission;
verus! {
tokenized_state_machine!(membership {
    fields {
        #[sharding(variable)] pub keys: Set<vstd::tokens::InstanceId>,
        #[sharding(persistent_map)] pub known: Map<vstd::tokens::InstanceId, ()>,
    }
    #[invariant] pub fn exact_keys(&self) -> bool { self.keys == self.known.dom() }
    init! { initialize() { init keys = Set::empty(); init known = Map::empty(); } }
    transition! { remember(id: vstd::tokens::InstanceId) {
        update keys = pre.keys.insert(id); add known (union)= [id => ()];
    } }
    property! { contains(id: vstd::tokens::InstanceId) {
        have known >= [id => ()]; assert(pre.keys.contains(id));
    } }
    #[inductive(initialize)] fn initialize_inductive(post: Self) {}
    #[inductive(remember)] fn remember_inductive(pre: Self, post: Self, id: vstd::tokens::InstanceId) {}
});
pub tracked struct Counts<T> {
    instance: membership::Instance,
    keys: membership::keys,
    counters: Map<vstd::tokens::InstanceId, bindings::observing<HeapPermission<T>>>,
}
impl<T> Counts<T> {
    pub closed spec fn inv(&self) -> bool {
        self.keys.instance_id() == self.instance.id() && self.keys.value() == self.counters.dom()
        && forall|id: vstd::tokens::InstanceId| self.counters.dom().contains(id) ==>
            (#[trigger] self.counters[id]).instance_id() == id
    }
    pub closed spec fn registry_id(&self) -> vstd::tokens::InstanceId { self.instance.id() }
    pub closed spec fn allocations(&self) -> Set<vstd::tokens::InstanceId> { self.counters.dom() }
    pub open spec fn contains(&self, id: vstd::tokens::InstanceId) -> bool { self.allocations().contains(id) }
    pub closed spec fn value(&self, id: vstd::tokens::InstanceId) -> nat { self.counters[id].value() }
    pub proof fn new() -> (tracked counts: Self)
        ensures counts.inv(), forall|id: vstd::tokens::InstanceId| !counts.contains(id),
    {
        let tracked (Tracked(instance), Tracked(keys), Tracked(known)) = membership::Instance::initialize();
        Counts { instance, keys, counters: Map::tracked_empty() }
    }
    /// The durable membership receipt follows a reader, not the current pointer.
    pub proof fn register(tracked &mut self, tracked count: bindings::observing<HeapPermission<T>>)
        -> (tracked receipt: membership::known)
        requires old(self).inv(), !old(self).contains(count.instance_id()),
        ensures final(self).inv(), final(self).registry_id() == old(self).registry_id(),
            final(self).allocations() == old(self).allocations().insert(count.instance_id()),
            receipt.instance_id() == final(self).registry_id(), receipt.key() == count.instance_id(),
            final(self).contains(count.instance_id()), final(self).value(count.instance_id()) == count.value(),
            forall|id: vstd::tokens::InstanceId| #[trigger] old(self).contains(id) ==>
                final(self).contains(id) && final(self).value(id) == old(self).value(id),
    {
        let tracked receipt = self.instance.remember(count.instance_id(), &mut self.keys);
        self.counters.tracked_insert(count.instance_id(), count);
        receipt
    }
    pub proof fn remembered(tracked &self, tracked receipt: &membership::known)
        requires self.inv(), receipt.instance_id() == self.registry_id(),
        ensures self.contains(receipt.key()),
    { self.instance.contains(receipt.key(), &self.keys, receipt); }
    pub proof fn observe<'scope>(tracked &mut self,
        tracked instance: &bindings::Instance<HeapPermission<T>>,
        tracked published: &bindings::published<HeapPermission<T>>,
        tracked receipt: &membership::known,
        tracked permit: &'scope admission::permits) -> (tracked observation: Observation<'scope, T>)
        requires old(self).inv(), receipt.instance_id() == old(self).registry_id(), receipt.key() == instance.id(),
            published.instance_id() == instance.id(), instance.domain().contains(permit.instance_id()),
        ensures final(self).inv(), final(self).registry_id() == old(self).registry_id(),
            final(self).allocations() == old(self).allocations(),
            observation.node_id() == instance.id(), observation.memory() == published.value(),
            observation.gate_id() == permit.instance_id(), observation.domain() == instance.domain(),
            final(self).contains(instance.id()), final(self).value(instance.id()) == old(self).value(instance.id()) + 1,
            forall|id: vstd::tokens::InstanceId| id != instance.id() && #[trigger] old(self).contains(id) ==>
                final(self).contains(id) && final(self).value(id) == old(self).value(id),
    {
        self.instance.contains(instance.id(), &self.keys, receipt);
        let tracked mut count = self.counters.tracked_remove(instance.id());
        let tracked observation = Observation::observe(instance, published, &mut count, permit);
        self.counters.tracked_insert(instance.id(), count);
        observation
    }
    pub proof fn observed_is_positive(tracked &self,
        tracked instance: &bindings::Instance<HeapPermission<T>>,
        tracked receipt: &membership::known,
        tracked observation: &Observation<'_, T>)
        requires self.inv(), receipt.instance_id() == self.registry_id(),
            receipt.key() == instance.id(), observation.node_id() == instance.id(),
        ensures self.contains(instance.id()), self.value(instance.id()) > 0,
    {
        self.remembered(receipt);
        observation.positive_count(instance, self.counters.tracked_borrow(instance.id()));
    }
    /// Recovery consults the same counter that atomic loads incremented. A
    /// missing allocation or a live reader preserves the exact retirement token.
    pub proof fn recover(tracked &self,
        tracked instance: &bindings::Instance<HeapPermission<T>>,
        tracked retired: bindings::retired<HeapPermission<T>>)
        -> (tracked result: Result<HeapPermission<T>, bindings::retired<HeapPermission<T>>>)
        requires self.inv(), retired.instance_id() == instance.id(),
        ensures result is Ok ==> result->Ok_0 == retired.value()
                && self.contains(instance.id()) && self.value(instance.id()) == 0,
            result is Err ==> result->Err_0 == retired,
            (result is Ok) == (self.contains(instance.id()) && self.value(instance.id()) == 0),
    {
        if self.contains(instance.id()) && self.value(instance.id()) == 0 {
            let tracked count = self.counters.tracked_borrow(instance.id());
            Ok(instance.reclaim(retired.value(), retired, count))
        } else {
            Err(retired)
        }
    }
    pub proof fn end(tracked &mut self, tracked instance: &bindings::Instance<HeapPermission<T>>,
        tracked receipt: &membership::known, tracked observation: Observation<'_, T>)
        requires old(self).inv(), receipt.instance_id() == old(self).registry_id(), receipt.key() == instance.id(),
            observation.node_id() == instance.id(),
        ensures final(self).inv(), final(self).registry_id() == old(self).registry_id(),
            final(self).allocations() == old(self).allocations(),
            final(self).contains(instance.id()), final(self).value(instance.id()) + 1 == old(self).value(instance.id()),
            forall|id: vstd::tokens::InstanceId| id != instance.id() && #[trigger] old(self).contains(id) ==>
                final(self).contains(id) && final(self).value(id) == old(self).value(id),
    {
        self.instance.contains(instance.id(), &self.keys, receipt);
        let tracked mut count = self.counters.tracked_remove(instance.id());
        observation.end(instance, &mut count);
        self.counters.tracked_insert(instance.id(), count);
    }
}
}

verus! {
/// Ending an old reader after registering a replacement allocation changes
/// only the old count. Both registration receipts stay valid.
pub proof fn old_reader_after_new_registration<T>(tracked counts: &mut Counts<T>,
    tracked old_instance: &bindings::Instance<HeapPermission<T>>,
    tracked old_published: &bindings::published<HeapPermission<T>>,
    tracked old_receipt: &membership::known, tracked permit: &admission::permits,
    tracked replacement_count: bindings::observing<HeapPermission<T>>)
    -> (tracked replacement_receipt: membership::known)
    requires old(counts).inv(), old_receipt.instance_id() == old(counts).registry_id(),
        old_receipt.key() == old_instance.id(), old(counts).contains(old_instance.id()),
        old_published.instance_id() == old_instance.id(), old_instance.domain().contains(permit.instance_id()),
        !old(counts).contains(replacement_count.instance_id()),
    ensures final(counts).inv(), final(counts).registry_id() == old(counts).registry_id(),
        final(counts).contains(old_instance.id()), final(counts).value(old_instance.id()) == old(counts).value(old_instance.id()),
        replacement_receipt.instance_id() == final(counts).registry_id(), replacement_receipt.key() == replacement_count.instance_id(),
        final(counts).contains(replacement_count.instance_id()), final(counts).value(replacement_count.instance_id()) == replacement_count.value(),
{
    let tracked observation = counts.observe(old_instance, old_published, old_receipt, permit);
    let tracked receipt = counts.register(replacement_count);
    counts.end(old_instance, old_receipt, observation);
    receipt
}
}

verus! {
/// A complete resource path using one counter: publication, observation,
/// retirement, rejected recovery, reader completion, and exact memory recovery.
pub proof fn observe_retire_end_recover<T>(tracked counts: &mut Counts<T>,
    tracked instance: &bindings::Instance<HeapPermission<T>>,
    tracked published: bindings::published<HeapPermission<T>>,
    tracked receipt: &membership::known, tracked permit: &admission::permits)
    -> (tracked memory: HeapPermission<T>)
    requires old(counts).inv(), receipt.instance_id() == old(counts).registry_id(),
        receipt.key() == instance.id(), old(counts).contains(instance.id()),
        old(counts).value(instance.id()) == 0, published.instance_id() == instance.id(),
        instance.domain().contains(permit.instance_id()),
    ensures memory == published.value(), final(counts).inv(),
        final(counts).registry_id() == old(counts).registry_id(),
        final(counts).allocations() == old(counts).allocations(),
        final(counts).value(instance.id()) == 0,
{
    let tracked observation = counts.observe(instance, &published, receipt, permit);
    let tracked (Tracked(ticket), Tracked(history)) = instance.retire(published.value(), published);
    counts.observed_is_positive(instance, receipt, &observation);
    let tracked refused = counts.recover(instance, ticket);
    assert(refused is Err);
    let tracked ticket = match refused { Err(ticket) => ticket, Ok(_) => { assert(false); proof_from_false() } };
    counts.end(instance, receipt, observation);
    let tracked recovered = counts.recover(instance, ticket);
    assert(recovered is Ok);
    match recovered { Ok(memory) => memory, Err(_) => { assert(false); proof_from_false() } }
}
}

//! Binding memory remains in storage while publication ownership moves to retirement.
use vstd::prelude::*;
use vstd::multiset::*;
use verus_state_machines_macros::tokenized_state_machine;
use super::heap_permission::HeapPermission;
use super::rotation::gate_permits::admission;
verus! {
tokenized_state_machine!(bindings<Perm> {
    fields {
        #[sharding(constant)] pub domain: Set<vstd::tokens::InstanceId>,
        #[sharding(constant)] pub owner: *const u8,
        #[sharding(storage_option)] pub memory: Option<Perm>,
        #[sharding(option)] pub published: Option<Perm>,
        #[sharding(option)] pub retired: Option<Perm>,
        #[sharding(multiset)] pub observations: Multiset<Perm>,
        #[sharding(variable)] pub observing: nat,
        #[sharding(persistent_map)] pub retired_history: Map<(), ()>,
    }
    #[invariant] pub fn owner_conservation(&self) -> bool {
        (self.published.is_some() ==> self.retired.is_none())
        && self.memory == if self.published.is_some() { self.published } else { self.retired }
    }
    #[invariant] pub fn observations_have_memory(&self) -> bool {
        forall|p: Perm| #[trigger] self.observations.count(p) > 0 ==> self.memory == Some(p)
    }
    #[invariant] pub fn retired_is_terminal(&self) -> bool {
        self.retired_history.dom().contains(()) == self.published.is_none()
    }
    #[invariant] pub fn exact_count(&self) -> bool { self.observing == self.observations.len() }
    init! { allocate(x: Perm, domain: Set<vstd::tokens::InstanceId>, owner: *const u8) {
        init domain = domain; init owner = owner; init memory = Some(x); init published = Some(x);
        init retired = None; init observations = Multiset::empty(); init observing = 0; init retired_history = Map::empty();
    } }
    transition! { observe(x: Perm) {
        have published >= Some(x);
        add observations += {x}; update observing = pre.observing + 1;
    } }
    transition! { retire(x: Perm) {
        remove published -= Some(x); add retired += Some(x); add retired_history (union)= [() => ()];
    } }
    transition! { end(x: Perm) {
        remove observations -= {x}; assert(pre.observing > 0);
        update observing = (pre.observing - 1) as nat;
    } }
    transition! { reclaim(x: Perm) {
        require(pre.observing == 0); remove retired -= Some(x);
        withdraw memory -= Some(x);
    } }
    property! { guard_observation(x: Perm) {
        have observations >= {x}; guard memory >= Some(x);
    } }
    property! { positive_observation(x: Perm) {
        have observations >= {x}; assert(pre.observing > 0);
    } }
    property! { publication_excludes_retirement(x: Perm) {
        have published >= Some(x); have retired_history >= [() => ()]; assert(false);
    } }
    #[inductive(allocate)] fn allocate_inductive(post: Self, x: Perm, domain: Set<vstd::tokens::InstanceId>, owner: *const u8) {}
    #[inductive(observe)] fn observe_inductive(pre: Self, post: Self, x: Perm) {}
    #[inductive(retire)] fn retire_inductive(pre: Self, post: Self, x: Perm) {}
    #[inductive(end)] fn end_inductive(pre: Self, post: Self, x: Perm) {}
    #[inductive(reclaim)] fn reclaim_inductive(pre: Self, post: Self, x: Perm) {}
});
pub tracked struct Observation<'scope, T> {
    token: bindings::observations<HeapPermission<T>>,
    permit: &'scope admission::permits,
    ghost domain: Set<vstd::tokens::InstanceId>,
}
impl<'scope, T> Observation<'scope, T> {
    #[verifier::type_invariant]
    pub closed spec fn inv(self) -> bool { self.domain.contains(self.permit.instance_id()) }
    pub closed spec fn node_id(&self) -> vstd::tokens::InstanceId { self.token.instance_id() }
    pub closed spec fn gate_id(&self) -> vstd::tokens::InstanceId { self.permit.instance_id() }
    pub closed spec fn memory(&self) -> HeapPermission<T> { self.token.element() }
    pub closed spec fn domain(&self) -> Set<vstd::tokens::InstanceId> { self.domain }
    pub proof fn permit(tracked &self) -> (tracked permit: &'scope admission::permits)
        ensures permit.instance_id() == self.gate_id(),
    { self.permit }

    pub proof fn observe(tracked instance: &bindings::Instance<HeapPermission<T>>,
        tracked published: &bindings::published<HeapPermission<T>>,
        tracked count: &mut bindings::observing<HeapPermission<T>>,
        tracked permit: &'scope admission::permits) -> (tracked observation: Self)
        requires published.instance_id() == instance.id(), old(count).instance_id() == instance.id(),
            instance.domain().contains(permit.instance_id()),
        ensures observation.node_id() == instance.id(), observation.gate_id() == permit.instance_id(),
            observation.domain() == instance.domain(), observation.memory() == published.value(),
            final(count).instance_id() == instance.id(), final(count).value() == old(count).value() + 1,
    {
        let tracked token = instance.observe(published.value(), published, count);
        Observation { token, permit, domain: instance.domain() }
    }
    pub proof fn into_owned(tracked self) -> (tracked owned: OwnedObservation<T>)
        ensures owned.node_id() == self.node_id(), owned.gate_id() == self.gate_id(),
            owned.memory() == self.memory(), owned.domain() == self.domain(),
    { OwnedObservation { token: self.token, gate: self.permit.instance_id(), domain: self.domain } }
    pub proof fn end(tracked self, tracked instance: &bindings::Instance<HeapPermission<T>>,
        tracked count: &mut bindings::observing<HeapPermission<T>>)
        requires self.node_id() == instance.id(), old(count).instance_id() == instance.id(),
        ensures final(count).instance_id() == instance.id(), final(count).value() + 1 == old(count).value(),
    { instance.end(self.token.element(), self.token, count); }
    pub proof fn positive_count(tracked &self,
        tracked instance: &bindings::Instance<HeapPermission<T>>,
        tracked count: &bindings::observing<HeapPermission<T>>)
        requires self.node_id() == instance.id(), count.instance_id() == instance.id(),
        ensures count.value() > 0,
    { instance.positive_observation(self.token.element(), &self.token, count); }
    pub proof fn excludes_idle(tracked &self, tracked gate: &admission::Instance,
        tracked active: &admission::active)
        requires self.gate_id() == gate.id(), active.instance_id() == gate.id(),
        ensures active.value() > 0,
    { gate.positive(active, self.permit); }
}
/// Owns the memory observation token without borrowing an admission. Its
/// admission share must be conserved separately by the issuing registry.
pub tracked struct OwnedObservation<T> {
    token: bindings::observations<HeapPermission<T>>,
    ghost gate: vstd::tokens::InstanceId,
    ghost domain: Set<vstd::tokens::InstanceId>,
}
impl<T> OwnedObservation<T> {
    pub closed spec fn node_id(&self) -> vstd::tokens::InstanceId { self.token.instance_id() }
    pub closed spec fn gate_id(&self) -> vstd::tokens::InstanceId { self.gate }
    pub closed spec fn domain(&self) -> Set<vstd::tokens::InstanceId> { self.domain }
    pub closed spec fn memory(&self) -> HeapPermission<T> { self.token.element() }
    pub proof fn with_permit<'scope>(tracked self, tracked permit: &'scope admission::permits)
        -> (tracked observation: Observation<'scope, T>)
        requires permit.instance_id() == self.gate_id(), self.domain().contains(self.gate_id()),
        ensures observation.node_id() == self.node_id(), observation.gate_id() == self.gate_id(),
            observation.memory() == self.memory(), observation.domain() == self.domain(),
    { Observation { token: self.token, permit, domain: self.domain } }
}
pub fn borrow_owned_observation<'a, T>(pointer: *const T,
    Tracked(instance): Tracked<&'a bindings::Instance<HeapPermission<T>>>,
    Tracked(observation): Tracked<&'a OwnedObservation<T>>) -> (value: &'a T)
    requires observation.node_id() == instance.id(), observation.memory().ptr() == pointer as *mut T,
        observation.memory().is_init(),
    ensures *value == observation.memory().value(),
{
    let tracked memory = instance.guard_observation(observation.token.element(), &observation.token);
    vstd::raw_ptr::ptr_ref(pointer, Tracked(memory.borrow()))
}
pub fn borrow_observed<'a, T>(pointer: *const T,
    Tracked(instance): Tracked<&'a bindings::Instance<HeapPermission<T>>>,
    Tracked(observation): Tracked<&'a Observation<'_, T>>) -> (value: &'a T)
    requires observation.node_id() == instance.id(),
        observation.memory().ptr() == pointer as *mut T, observation.memory().is_init(),
    ensures *value == observation.memory().value(),
{
    let tracked memory = instance.guard_observation(observation.token.element(), &observation.token);
    vstd::raw_ptr::ptr_ref(pointer, Tracked(memory.borrow()))
}
}

verus! {
/// Queue payload ownership is independent of active Observation borrows.
pub struct RetiredRecord<T> {
    owner: Ghost<*const u8>,
    pointer: *mut T,
    ticket: Tracked<bindings::retired<HeapPermission<T>>>,
}
impl<T> RetiredRecord<T> {
    pub closed spec fn owner(&self) -> *const u8 { self.owner@ }
    pub closed spec fn node_id(&self) -> vstd::tokens::InstanceId { self.ticket@.instance_id() }
    pub closed spec fn memory(&self) -> HeapPermission<T> { self.ticket@.value() }
    pub closed spec fn pointer(&self) -> *mut T { self.pointer }
    pub fn into_ticket(self) -> (ticket: Tracked<bindings::retired<HeapPermission<T>>>)
        ensures ticket@.instance_id() == self.node_id(), ticket@.value() == self.memory(),
    { self.ticket }
    pub fn new(pointer: *mut T, Tracked(instance): Tracked<&bindings::Instance<HeapPermission<T>>>, Tracked(ticket): Tracked<bindings::retired<HeapPermission<T>>>) -> (entry: Self)
        requires ticket.instance_id() == instance.id(), ticket.value().ptr() == pointer, ticket.value().is_init(),
        ensures entry.owner() == instance.owner(), entry.node_id() == ticket.instance_id(), entry.memory() == ticket.value(), entry.pointer() == pointer,
    { RetiredRecord { owner: Ghost(instance.owner()), pointer, ticket: Tracked(ticket) } }
}
#[verifier::exec_allows_no_decreases_clause]
pub fn register_retired<T>(queue: &mut super::rotation::registration::Registration<RetiredRecord<T>>,
    entry: RetiredRecord<T>) -> (generation: bool)
    requires old(queue).owner == entry.owner(), old(queue).held.is_none(), !old(queue).both_held, old(queue).registered.is_none(),
        old(queue).cursor <= old(queue).samples.len(),
    ensures final(queue).owner == entry.owner(), final(queue).held.is_none(), !final(queue).both_held,
        final(queue).registered == Some(generation), generation == final(queue).current,
        final(queue).zero@ == if generation { old(queue).zero@ } else { old(queue).zero@.push(entry) },
        final(queue).one@ == if generation { old(queue).one@.push(entry) } else { old(queue).one@ },
{ super::rotation::registration::shared_registration(queue, entry) }
// Detachment uses the existing generic certificate-authorized Batch constructors.
// Recovery still requires this exact allocation's zero-observation token.
pub fn recover_retired<T>(entry: RetiredRecord<T>,
    Tracked(instance): Tracked<&bindings::Instance<HeapPermission<T>>>,
    Tracked(count): Tracked<&bindings::observing<HeapPermission<T>>>) -> (memory: Tracked<HeapPermission<T>>)
    requires entry.node_id() == instance.id(), count.instance_id() == instance.id(), count.value() == 0,
    ensures memory@ == entry.memory(),
{
    let tracked memory = instance.reclaim(entry.ticket@.value(), entry.ticket.get(), count);
    Tracked(memory)
}
}

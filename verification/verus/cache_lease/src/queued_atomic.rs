//! Cache queue payloads refer to a node with ghost invariant-protected resources.
//! Native node/index layout and weak memory correspondence remain separate.
use vstd::prelude::*;
use vstd::invariant::{AtomicInvariant, InvariantPredicate};
use vstd::open_atomic_invariant;
use super::heap_permission::HeapPermission;
use super::pin_ownership::{cache_pins, PinKind};
use super::retained_observations::{Ledger as ObservationLedger, Ticket};
use super::rotation::drain::permit_shares::Scope;
use super::retirement::RetiredNode;
use super::rotation::drain::atomic_counter::DrainSet;
use verus_state_machines_macros::tokenized_state_machine;
verus! {
tokenized_state_machine!(coverage {
    fields {
        #[sharding(variable)] pub snapshot: (Set<vstd::tokens::InstanceId>, bool),
        #[sharding(persistent_map)] pub bounds: Map<Set<vstd::tokens::InstanceId>, ()>,
    }
    #[invariant] pub fn bounded(&self) -> bool {
        forall|bound: Set<vstd::tokens::InstanceId>| #[trigger] self.bounds.dom().contains(bound)
            ==> self.snapshot.1 && self.snapshot.0.subset_of(bound)
    }
    init! { initialize(domain: Set<vstd::tokens::InstanceId>) {
        init snapshot = (domain, false); init bounds = Map::empty();
    } }
    transition! { freeze() { update snapshot = (pre.snapshot.0, true); } }
    transition! { narrow(bound: Set<vstd::tokens::InstanceId>) {
        require(pre.snapshot.1);
        update snapshot = (pre.snapshot.0.intersect(bound), true);
        add bounds (union)= [bound => ()];
    } }
    property! { within(bound: Set<vstd::tokens::InstanceId>) {
        have bounds >= [bound => ()];
        assert(pre.snapshot.1); assert(pre.snapshot.0.subset_of(bound));
    } }
    #[inductive(initialize)] fn initialize_inductive(post: Self, domain: Set<vstd::tokens::InstanceId>) {}
    #[inductive(freeze)] fn freeze_inductive(pre: Self, post: Self) {}
    #[inductive(narrow)] fn narrow_inductive(pre: Self, post: Self, bound: Set<vstd::tokens::InstanceId>) {}
});
}
macro_rules! width {
    ($module:ident, $word:ty) => {
    pub mod $module {
    use super::*;
    use super::super::atomic_pins::$module::{Pins, initialize_owned};
    use super::super::rotation::striped_rotation::$module as owned_rotation;
    use super::super::rotation::drain::atomic_stripes::$module as atomic_stripes;
    use super::super::rotation::drain::atomic_counter::$module::Counter;
    verus! {
    pub struct Ledger<T> {
        allocation: Tracked<cache_pins::allocation<HeapPermission<T>>>,
        observations: Tracked<ObservationLedger<T>>,
        coverage: Tracked<coverage::snapshot>,
    }
    pub struct Predicate { ledger_id: vstd::tokens::InstanceId, id: vstd::tokens::InstanceId, domain: Set<vstd::tokens::InstanceId>, coverage: coverage::Instance }
    impl Predicate {
        closed spec fn inv<T>(self, state: Ledger<T>) -> bool {
            state.allocation@.instance_id() == self.id && state.observations@.inv()
                && state.observations@.id() == self.ledger_id
                && state.observations@.node_id() == self.id && state.observations@.domain() == self.domain
                && state.coverage@.instance_id() == self.coverage.id()
                && state.coverage@.value() == (state.observations@.coverage(), state.observations@.frozen())
        }
    }
    pub struct LedgerPredicate {}
    impl<T> InvariantPredicate<Predicate, Ledger<T>> for LedgerPredicate {
        closed spec fn inv(predicate: Predicate, state: Ledger<T>) -> bool { predicate.inv(state) }
    }
    pub struct Node<T> {
        pins: Pins<T>, ledger: Tracked<AtomicInvariant<Predicate, Ledger<T>, LedgerPredicate>>, owner: *const u8,
        coverage: Tracked<coverage::Instance>,
    }
    impl<T> Node<T> {
        pub closed spec fn inv(&self) -> bool {
            self.pins.inv() && self.ledger@.namespace() != self.pins.namespace()
                && self.ledger@.constant().id == self.pins.id()
                && self.ledger@.constant().domain == self.pins.domain() && self.coverage@ == self.ledger@.constant().coverage
        }
        pub closed spec fn id(&self) -> vstd::tokens::InstanceId { self.pins.id() }
        pub closed spec fn gates(&self) -> Set<vstd::tokens::InstanceId> { self.pins.domain() }
        pub closed spec fn owner(&self) -> *const u8 { self.owner }
        pub fn new(owner: *const u8, Tracked(memory): Tracked<HeapPermission<T>>,
            Ghost(domain): Ghost<Set<vstd::tokens::InstanceId>>)
            -> (result: (Self, Tracked<cache_pins::pins<HeapPermission<T>>>))
            requires memory.is_init(),
            ensures result.0.inv(), result.0.owner() == owner, result.0.gates() == domain,
                result.1@.instance_id() == result.0.id(), result.1@.element() == (memory, PinKind::Creator),
        {
            let (pins, Tracked(resources)) = initialize_owned(Tracked(memory), Ghost(domain));
            let ghost id = pins.id();
            let ghost ledger_id = resources.observations.id();
            let tracked (Tracked(coverage), Tracked(snapshot), Tracked(bounds)) = coverage::Instance::initialize(domain);
            let tracked ledger = AtomicInvariant::new(Predicate { ledger_id, id, domain, coverage },
                Ledger { allocation: Tracked(resources.allocation), observations: Tracked(resources.observations), coverage: Tracked(snapshot) },
                pins.namespace() + 1);
            (Node { pins, ledger: Tracked(ledger), owner, coverage: Tracked(coverage) }, Tracked(resources.creator))
        }
        pub fn borrow_pin<'a>(&'a self, pointer: *const T,
            Tracked(pin): Tracked<&'a cache_pins::pins<HeapPermission<T>>>) -> (value: &'a T)
            requires self.inv(), pin.instance_id() == self.id(), pin.element().0.ptr() == pointer as *mut T,
                pin.element().0.is_init(),
            ensures *value == pin.element().0.value(),
        { super::super::pin_ownership::borrow_from_pin(pointer, self.pins.instance(), Tracked(pin)) }
        pub closed spec fn observation_id(&self) -> vstd::tokens::InstanceId { self.ledger@.constant().ledger_id }
        pub fn observe(&self, Tracked(scope): Tracked<&mut Scope>,
            Tracked(resident): Tracked<&cache_pins::pins<HeapPermission<T>>>) -> (ticket: Tracked<Ticket<T>>)
            requires self.inv(), old(scope).inv(), self.gates().contains(old(scope).gate_id()),
                resident.instance_id() == self.id(), resident.element().1 == PinKind::Resident,
            ensures final(scope).inv(), final(scope).id() == old(scope).id(), final(scope).gate_id() == old(scope).gate_id(),
                final(scope).len() == old(scope).len() + 1, ticket@.ledger_id() == self.observation_id(), ticket@.node_id() == self.id(),
                ticket@.scope_id() == final(scope).id(), ticket@.memory() == resident.element().0,
        {
            let Tracked(instance) = self.pins.instance();
            let tracked ticket;
            open_atomic_invariant!(self.ledger.borrow() => state => {
                proof { ticket = state.observations.borrow_mut().observe(instance, scope, resident); }
            });
            Tracked(ticket)
        }
        pub fn end_observation(&self, Tracked(scope): Tracked<&mut Scope>, Tracked(ticket): Tracked<Ticket<T>>)
            requires self.inv(), old(scope).inv(), ticket.ledger_id() == self.observation_id(), ticket.node_id() == self.id(), ticket.scope_id() == old(scope).id(),
            ensures final(scope).inv(), final(scope).id() == old(scope).id(), final(scope).gate_id() == old(scope).gate_id(),
                final(scope).len() + 1 == old(scope).len(),
        {
            let Tracked(instance) = self.pins.instance();
            open_atomic_invariant!(self.ledger.borrow() => state => {
                proof { state.observations.borrow_mut().end(instance, scope, ticket); }
            });
        }
        #[verifier::exec_allows_no_decreases_clause]
        pub(crate) fn acquire_from_pin(&self, Tracked(source): Tracked<&cache_pins::pins<HeapPermission<T>>>, Ghost(kind): Ghost<PinKind>)
            -> (result: (super::super::pin_transitions::Acquire<$word>, Tracked<Option<cache_pins::pins<HeapPermission<T>>>>, Ghost<$word>))
            requires self.inv(), source.instance_id() == self.id(), !(kind is Creator),
            ensures result.0 == super::super::pin_transitions::$module::acquire_spec(result.2@), match result.0 {
                super::super::pin_transitions::Acquire::Acquired(_) => result.1@.is_some() && result.1@.unwrap().instance_id() == self.id()
                    && result.1@.unwrap().element() == (source.element().0, kind),
                _ => result.1@.is_none(),
            },
        { self.pins.acquire_anchored(Tracked(source), Ghost(kind)) }
        #[verifier::exec_allows_no_decreases_clause]
        pub(crate) fn acquire_observed(&self, Tracked(ticket): Tracked<&Ticket<T>>)
            -> (result: (super::super::pin_transitions::Acquire<$word>, Tracked<Option<cache_pins::pins<HeapPermission<T>>>>, Ghost<$word>))
            requires self.inv(), ticket.ledger_id() == self.observation_id(), ticket.node_id() == self.id(),
            ensures result.0 == super::super::pin_transitions::$module::acquire_spec(result.2@), match result.0 {
                super::super::pin_transitions::Acquire::Acquired(_) => result.1@.is_some() && result.1@.unwrap().instance_id() == self.id()
                    && result.1@.unwrap().element() == (ticket.memory(), PinKind::Lease),
                _ => result.1@.is_none(),
            },
        {
            let tracked observation = ticket.observation();
            self.pins.acquire_observed(Tracked(observation))
        }
        pub fn release<'node>(&'node self, pointer: *mut T, Tracked(pin): Tracked<cache_pins::pins<HeapPermission<T>>>)
            -> (result: Option<Entry<'node, T>>)
            requires self.inv(), pin.instance_id() == self.id(), pin.element().0.ptr() == pointer, pin.element().0.is_init(),
            ensures result.is_some() ==> result.unwrap().inv() && result.unwrap().owner() == self.owner()
                && result.unwrap().gates() == self.gates() && result.unwrap().memory() == pin.element().0,
        {
            let (outcome, Tracked(ticket)) = self.pins.release(Tracked(pin));
            if let super::super::pin_transitions::Release::LastPin = outcome {
                let record = RetiredNode::from_ticket(pointer, self.pins.instance(), Tracked(ticket.tracked_unwrap()));
                let Tracked(instance) = self.pins.instance();
                let Tracked(retirement) = record.ticket();
                let tracked bound;
                open_atomic_invariant!(self.ledger.borrow() => state => {
                    proof {
                        state.observations.borrow_mut().freeze(instance, retirement);
                        self.coverage.borrow().freeze(state.coverage.borrow_mut());
                        state.observations.borrow().coverage_within_domain();
                        bound = self.coverage.borrow().narrow(self.gates(), state.coverage.borrow_mut());
                        assert(state.observations@.coverage().intersect(self.gates()) =~= state.observations@.coverage());
                    }
                });
                Some(Entry { node: self, record, bound: Tracked(bound) })
            } else { None }
        }
    }
    /// A lookup transfers its protection from actual admission to a lease pin.
    /// The caller still establishes the resident fragment from index membership.
    #[verifier::exec_allows_no_decreases_clause]
    pub(crate) fn lookup_pin<T>(node: &Node<T>, counter: &Counter,
        Tracked(resident): Tracked<&cache_pins::pins<HeapPermission<T>>>)
        -> (result: (super::super::rotation::drain::transitions::TransitionOutcome<$word>,
            Option<(super::super::pin_transitions::Acquire<$word>, Tracked<Option<cache_pins::pins<HeapPermission<T>>>>, Ghost<$word>)>))
        requires node.inv(), counter.inv(), node.gates().contains(counter.id()),
            resident.instance_id() == node.id(), resident.element().1 == PinKind::Resident,
        ensures match result.0 {
            super::super::rotation::drain::transitions::TransitionOutcome::Success(_) => result.1.is_some()
                && result.1.unwrap().0 == super::super::pin_transitions::$module::acquire_spec(result.1.unwrap().2@)
                && (match result.1.unwrap().0 {
                    super::super::pin_transitions::Acquire::Acquired(_) => result.1.unwrap().1@.is_some()
                        && result.1.unwrap().1@.unwrap().instance_id() == node.id()
                        && result.1.unwrap().1@.unwrap().element() == (resident.element().0, PinKind::Lease),
                    _ => result.1.unwrap().1@.is_none(),
                }),
            _ => result.1.is_none(),
        },
    {
        let (outcome, Tracked(scope)) = counter.acquire_scope();
        match outcome {
            super::super::rotation::drain::transitions::TransitionOutcome::Success(next) => {
                let tracked mut scope = scope.tracked_unwrap();
                let ticket = node.observe(Tracked(&mut scope), Tracked(resident));
                let acquired = node.acquire_observed(Tracked(ticket.borrow()));
                node.end_observation(Tracked(&mut scope), ticket);
                counter.release_scope(Tracked(scope));
                (super::super::rotation::drain::transitions::TransitionOutcome::Success(next), Some(acquired))
            },
            super::super::rotation::drain::transitions::TransitionOutcome::Rejected => (outcome, None),
            super::super::rotation::drain::transitions::TransitionOutcome::FailStop => (outcome, None),
        }
    }
    /// Production-shared eligibility/recheck/rollback flow, after index observation.
    /// Boolean samples and index-to-ticket transfer remain external premises here.
    #[verifier::exec_allows_no_decreases_clause]
    pub(crate) fn complete_lookup<'node, T>(node: &'node Node<T>, counter: &Counter, pointer: *mut T,
        eligible: bool, resident_after: bool, Tracked(scope): Tracked<Scope>, Tracked(ticket): Tracked<Ticket<T>>)
        -> (result: (Option<Tracked<cache_pins::pins<HeapPermission<T>>>>, Option<Entry<'node, T>>))
        requires node.inv(), counter.inv(), scope.inv(), scope.gate_id() == counter.id(), scope.len() == 1,
            ticket.ledger_id() == node.observation_id(), ticket.node_id() == node.id(), ticket.scope_id() == scope.id(),
            ticket.memory().ptr() == pointer, ticket.memory().is_init(),
        ensures result.0.is_some() ==> eligible && resident_after && result.1.is_none()
                && result.0.unwrap()@.instance_id() == node.id()
                && result.0.unwrap()@.element() == (ticket.memory(), PinKind::Lease),
            result.1.is_some() ==> eligible && !resident_after && result.0.is_none()
                && result.1.unwrap().inv() && result.1.unwrap().memory() == ticket.memory()
                && result.1.unwrap().owner() == node.owner(),
            !eligible ==> result.0.is_none() && result.1.is_none(),
    {
        let tracked mut scope = scope;
        let tracked mut pin = None;
        let mut pending = None;
        let acquired = super::super::pin_transitions::lookup_after_observation!(
            eligible = eligible,
            acquire = vstd::prelude::verus_exec_expr!({
                let (outcome, fragment, _) = node.acquire_observed(Tracked(&ticket));
                proof { pin = fragment.get(); }
                match outcome {
                    super::super::pin_transitions::Acquire::Acquired(_) => Ok(true),
                    super::super::pin_transitions::Acquire::Zero => Ok(false),
                    super::super::pin_transitions::Acquire::Overflow => Err(()),
                }
            }),
            resident = resident_after,
            rollback retired = vstd::prelude::verus_exec_expr!(node.release(pointer, Tracked(pin.tracked_unwrap()))),
            context _context = (),
            leave = vstd::prelude::verus_exec_expr!({
                node.end_observation(Tracked(&mut scope), Tracked(ticket));
                counter.release_scope(Tracked(scope));
            }),
            reclaim = vstd::prelude::verus_exec_expr!({ pending = retired; }),
            success = vstd::prelude::verus_exec_expr!(Some(Tracked(pin.tracked_unwrap()))),
            overflow = vstd::prelude::verus_exec_expr!({ loop {} }),
        );
        (acquired, pending)
    }
    #[verifier::exec_allows_no_decreases_clause]
    pub(crate) fn complete_inline_lookup<'node, V>(node: &'node Node<super::super::inline_value::Allocation<V>>, counter: &Counter, pointer: *mut super::super::inline_value::Allocation<V>,
        eligible: bool, Tracked(scope): Tracked<Scope>, Tracked(ticket): Tracked<Ticket<super::super::inline_value::Allocation<V>>>)
        -> (result: (Option<Tracked<cache_pins::pins<HeapPermission<super::super::inline_value::Allocation<V>>>>>, Option<Entry<'node, super::super::inline_value::Allocation<V>>>, Ghost<bool>))
        requires node.inv(), counter.inv(), scope.inv(), scope.gate_id() == counter.id(), scope.len() == 1,
            ticket.ledger_id() == node.observation_id(), ticket.node_id() == node.id(), ticket.scope_id() == scope.id(),
            ticket.memory().ptr() == pointer, ticket.memory().is_init(),
        ensures result.0.is_some() ==> eligible && result.2@ && result.1.is_none()
                && result.0.unwrap()@.instance_id() == node.id()
                && result.0.unwrap()@.element() == (ticket.memory(), PinKind::Lease),
            result.1.is_some() ==> eligible && !result.2@ && result.0.is_none()
                && result.1.unwrap().inv() && result.1.unwrap().memory() == ticket.memory()
                && result.1.unwrap().owner() == node.owner(),
            !eligible ==> result.0.is_none() && result.1.is_none(),
    {
        let mut resident_after = false;
        let tracked mut scope = scope;
        let tracked mut pin = None;
        let mut pending = None;
        let acquired = super::super::pin_transitions::lookup_after_observation!(
            eligible = eligible,
            acquire = vstd::prelude::verus_exec_expr!({
                let (outcome, fragment, _) = node.acquire_observed(Tracked(&ticket));
                proof { pin = fragment.get(); }
                match outcome {
                    super::super::pin_transitions::Acquire::Acquired(_) => Ok(true),
                    super::super::pin_transitions::Acquire::Zero => Ok(false),
                    super::super::pin_transitions::Acquire::Overflow => Err(()),
                }
            }),
            resident = vstd::prelude::verus_exec_expr!({
                resident_after = node.observed_resident(pointer, Tracked(&ticket));
                resident_after
            }),
            rollback retired = vstd::prelude::verus_exec_expr!(node.release(pointer, Tracked(pin.tracked_unwrap()))),
            context _context = (),
            leave = vstd::prelude::verus_exec_expr!({
                node.end_observation(Tracked(&mut scope), Tracked(ticket));
                counter.release_scope(Tracked(scope));
            }),
            reclaim = vstd::prelude::verus_exec_expr!({ pending = retired; }),
            success = vstd::prelude::verus_exec_expr!(Some(Tracked(pin.tracked_unwrap()))),
            overflow = vstd::prelude::verus_exec_expr!({ loop {} }),
        );
        (acquired, pending, Ghost(resident_after))
    }
    /// Read immutable metadata while the retained observation guards the allocation.
    impl<V> Node<super::super::inline_value::Allocation<V>> {
        pub fn observed_resident(&self, pointer: *mut super::super::inline_value::Allocation<V>,
            Tracked(ticket): Tracked<&Ticket<super::super::inline_value::Allocation<V>>>) -> bool
            requires self.inv(), ticket.ledger_id() == self.observation_id(), ticket.node_id() == self.id(),
                ticket.memory().ptr() == pointer, ticket.memory().is_init(),
        {
            let tracked observation = ticket.observation();
            let allocation = super::super::pin_ownership::borrow_from_observation(
                pointer, self.pins.instance(), Tracked(observation));
            super::super::node_layout::resident!(allocation)
        }
        pub fn observed_generation(&self, pointer: *mut super::super::inline_value::Allocation<V>,
            Tracked(ticket): Tracked<&Ticket<super::super::inline_value::Allocation<V>>>) -> (generation: u64)
            requires self.inv(), ticket.ledger_id() == self.observation_id(), ticket.node_id() == self.id(),
                ticket.memory().ptr() == pointer, ticket.memory().is_init(),
            ensures generation == ticket.memory().value().generation,
        {
            let tracked observation = ticket.observation();
            let allocation = super::super::pin_ownership::borrow_from_observation(
                pointer, self.pins.instance(), Tracked(observation));
            super::super::node_layout::generation!(allocation)
        }
    }
    /// An owned lease keeps its exact pin borrowed for every typed reference.
    pub struct Lease<'node, T> { node: &'node Node<T>, pointer: *mut T, pin: Tracked<cache_pins::pins<HeapPermission<T>>> }
    impl<'node, T> Lease<'node, T> {
        pub closed spec fn inv(&self) -> bool {
            self.node.inv() && self.pin@.instance_id() == self.node.id()
                && self.pin@.element().1 == PinKind::Lease && self.pin@.element().0.ptr() == self.pointer
                && self.pin@.element().0.is_init()
        }
        pub closed spec fn memory(&self) -> HeapPermission<T> { self.pin@.element().0 }
        pub closed spec fn owner(&self) -> *const u8 { self.node.owner() }
        pub fn new(node: &'node Node<T>, pointer: *mut T, Tracked(pin): Tracked<cache_pins::pins<HeapPermission<T>>>) -> (lease: Self)
            requires node.inv(), pin.instance_id() == node.id(), pin.element().1 == PinKind::Lease,
                pin.element().0.ptr() == pointer, pin.element().0.is_init(),
            ensures lease.inv(), lease.memory() == pin.element().0, lease.owner() == node.owner(),
        { Lease { node, pointer, pin: Tracked(pin) } }
        pub fn read<'a>(&'a self) -> (value: &'a T)
            requires self.inv(), ensures *value == self.memory().value(),
        { self.node.borrow_pin(self.pointer as *const T, Tracked(self.pin.borrow())) }
        pub fn release(self) -> (retired: Option<Entry<'node, T>>)
            requires self.inv(), ensures retired.is_some() ==> retired.unwrap().inv()
                && retired.unwrap().memory() == self.memory() && retired.unwrap().owner() == self.owner(),
        { self.node.release(self.pointer, self.pin) }
    }
    pub struct Entry<'node, T> { node: &'node Node<T>, record: RetiredNode<T>, bound: Tracked<coverage::bounds> }
    impl<'node, T> Entry<'node, T> {
        pub closed spec fn inv(&self) -> bool {
            self.node.inv() && self.record.node_id() == self.node.id() && self.record.domain() == self.node.gates()
                && self.bound@.instance_id() == self.node.ledger@.constant().coverage.id()
        }
        pub closed spec fn memory(&self) -> HeapPermission<T> { self.record.memory() }
        pub closed spec fn owner(&self) -> *const u8 { self.node.owner() }
        pub closed spec fn gates(&self) -> Set<vstd::tokens::InstanceId> { self.node.gates() }
        pub closed spec fn prepared_for(&self, gates: Set<vstd::tokens::InstanceId>) -> bool { self.bound@.key() == gates }
        pub fn prepare(&mut self, Ghost(keep): Ghost<Set<vstd::tokens::InstanceId>>, Tracked(idle): Tracked<&DrainSet>)
            requires old(self).inv(), idle.inv(), idle.covers(old(self).gates().difference(keep)),
            ensures final(self).inv(), final(self).prepared_for(keep), final(self).memory() == old(self).memory(),
                final(self).owner() == old(self).owner(), final(self).gates() == old(self).gates(),
        {
            let tracked bound;
            open_atomic_invariant!(self.node.ledger.borrow() => state => {
                proof {
                    self.node.coverage.borrow().within(self.bound@.key(), state.coverage.borrow(), self.bound.borrow());
                    state.observations.borrow().coverage_within_domain();
                    state.observations.borrow_mut().narrow(keep, idle);
                    bound = self.node.coverage.borrow().narrow(keep, state.coverage.borrow_mut());
                }
            });
            self.bound = Tracked(bound);
        }
        pub fn recover_prepared(self, Tracked(drains): Tracked<&DrainSet>) -> (memory: Tracked<HeapPermission<T>>)
            requires self.inv(), drains.inv(), self.prepared_for(drains.domain()),
            ensures memory@ == self.memory(),
        {
            let Tracked(instance) = self.node.pins.instance();
            let Tracked(ticket) = self.record.into_ticket();
            let memory;
            open_atomic_invariant!(self.node.ledger.borrow() => state => {
                proof {
                    instance.retirement_allocation(ticket.value(), state.allocation.borrow(), &ticket);
                    self.node.coverage.borrow().within(self.bound@.key(), state.coverage.borrow(), self.bound.borrow());
                }
                memory = self.node.pins.recover_owned(Tracked(ticket), Tracked(state.allocation.borrow_mut()),
                    Tracked(state.observations.borrow()), Tracked(drains));
            });
            memory
        }
        pub fn recover(self, Tracked(drains): Tracked<&DrainSet>) -> (memory: Tracked<HeapPermission<T>>)
            requires self.inv(), drains.inv(), drains.covers(self.gates()),
            ensures memory@ == self.memory(),
        {
            let Tracked(instance) = self.node.pins.instance();
            let Tracked(ticket) = self.record.into_ticket();
            let memory;
            open_atomic_invariant!(self.node.ledger.borrow() => state => {
                proof {
                    instance.retirement_allocation(ticket.value(), state.allocation.borrow(), &ticket);
                    state.observations.borrow().coverage_within_domain();
                    assert(drains.covers(state.observations@.coverage()));
                }
                memory = self.node.pins.recover_owned(Tracked(ticket), Tracked(state.allocation.borrow_mut()),
                    Tracked(state.observations.borrow()), Tracked(drains));
            });
            memory
        }
    }
    pub fn new_queue<'node, T>(owner: *const u8, index: bool,
        Ghost(gates): Ghost<Set<vstd::tokens::InstanceId>>)
        -> (result: (super::super::rotation::barrier_ownership::QueueLock<Entry<'node, T>>,
            Tracked<super::super::rotation::queue_preparation::phase::ready>))
        ensures result.0.pred().domain == owner, result.0.pred().index == index,
            result.1@.instance_id() == result.0.pred().preparation.id(),
            forall|entry: Entry<'node, T>| (#[trigger] (result.0.pred().payload_inv)(entry))
                == (entry.inv() && entry.owner() == owner && entry.gates() == gates),
            forall|entry: Entry<'node, T>, bound: Set<vstd::tokens::InstanceId>|
                (#[trigger] (result.0.pred().prepared_inv)(entry, bound)) == entry.prepared_for(bound),
    {
        super::super::rotation::barrier_ownership::new_preparable(
            super::super::rotation::barrier_ownership::QueueContents { domain: owner, index, records: Vec::new() },
            Ghost(|entry: Entry<'node, T>| entry.inv() && entry.owner() == owner && entry.gates() == gates),
            Ghost(|entry: Entry<'node, T>, bound: Set<vstd::tokens::InstanceId>| entry.prepared_for(bound)))
    }
    /// Keep the actual publication barrier borrowed while preparing all records.
    pub fn prepare_queue<'node, T>(
        queue: &mut super::super::rotation::barrier_ownership::QueueState<Entry<'node, T>>,
        lock: &super::super::rotation::barrier_ownership::QueueLock<Entry<'node, T>>,
        handle: &super::super::rotation::barrier_ownership::QueueHandle<'_, Entry<'node, T>>,
        Ghost(gates): Ghost<Set<vstd::tokens::InstanceId>>, Ghost(keep): Ghost<Set<vstd::tokens::InstanceId>>, Tracked(idle): Tracked<&DrainSet>)
        requires lock.inv(*old(queue)), handle.rwlock() == *lock, old(queue).preparation@.value().is_none(), idle.inv(), idle.covers(gates.difference(keep)),
            forall|entry: Entry<'node, T>| (#[trigger] (lock.pred().payload_inv)(entry)) ==
                (entry.inv() && entry.owner() == old(queue).domain && entry.gates() == gates),
        ensures lock.inv(*final(queue)), final(queue).domain == old(queue).domain, final(queue).index == old(queue).index,
            final(queue).preparation == old(queue).preparation, final(queue).instance == old(queue).instance,
            final(queue).records.len() == old(queue).records.len(),
            forall|i: int| 0 <= i < final(queue).records.len() ==> (#[trigger] final(queue).records@[i]).prepared_for(keep)
                && final(queue).records@[i].memory() == old(queue).records@[i].memory(),
    {
        let mut next = 0;
        while next < queue.records.len()
            invariant next <= queue.records.len(), lock.inv(*queue), handle.rwlock() == *lock, idle.inv(), idle.covers(gates.difference(keep)),
                queue.domain == old(queue).domain, queue.index == old(queue).index,
                queue.preparation == old(queue).preparation, queue.instance == old(queue).instance,
                queue.preparation@.value().is_none(), queue.records.len() == old(queue).records.len(),
                forall|entry: Entry<'node, T>| (#[trigger] (lock.pred().payload_inv)(entry)) ==
                    (entry.inv() && entry.owner() == old(queue).domain && entry.gates() == gates),
                forall|i: int| 0 <= i < queue.records.len() ==> (#[trigger] queue.records@[i]).memory() == old(queue).records@[i].memory(),
                forall|i: int| 0 <= i < next ==> (#[trigger] queue.records@[i]).prepared_for(keep),
            decreases queue.records.len() - next,
        {
            queue.records[next].prepare(Ghost(keep), Tracked(idle));
            next += 1;
        }
    }
    pub fn prepare_collected_queue<'node, T>(
        mut collection: super::super::rotation::drain::atomic_stripes::$module::Collection,
        counters: &Vec<super::super::rotation::drain::atomic_counter::$module::Counter>,
        queue: &mut super::super::rotation::barrier_ownership::QueueState<Entry<'node, T>>,
        lock: &super::super::rotation::barrier_ownership::QueueLock<Entry<'node, T>>,
        handle: &super::super::rotation::barrier_ownership::QueueHandle<'_, Entry<'node, T>>,
        Ghost(gates): Ghost<Set<vstd::tokens::InstanceId>>, Ghost(keep): Ghost<Set<vstd::tokens::InstanceId>>)
        -> (result: Result<Tracked<Map<nat, super::super::rotation::drain::atomic_counter::lifecycle::control>>,
            super::super::rotation::drain::atomic_stripes::$module::Collection>)
        requires collection.inv(counters@), lock.inv(*old(queue)), handle.rwlock() == *lock,
            old(queue).preparation@.value().is_none(),
            forall|entry: Entry<'node, T>| (#[trigger] (lock.pred().payload_inv)(entry)) ==
                (entry.inv() && entry.owner() == old(queue).domain && entry.gates() == gates),
            forall|gate: vstd::tokens::InstanceId| #[trigger] gates.difference(keep).contains(gate)
                ==> exists|i: int| #![auto] 0 <= i < counters.len() && counters@[i].id() == gate,
        ensures lock.inv(*final(queue)), final(queue).domain == old(queue).domain, final(queue).index == old(queue).index,
            final(queue).preparation == old(queue).preparation, final(queue).instance == old(queue).instance,
            final(queue).records.len() == old(queue).records.len(),
            forall|i: int| 0 <= i < final(queue).records.len() ==> (#[trigger] final(queue).records@[i]).memory() == old(queue).records@[i].memory(),
            match result {
                Ok(controls) => super::super::rotation::drain::atomic_stripes::$module::controls_match(counters@, controls@)
                    && forall|i: int| 0 <= i < final(queue).records.len() ==> (#[trigger] final(queue).records@[i]).prepared_for(keep),
                Err(remaining) => remaining.inv(counters@) && *final(queue) == *old(queue),
            },
    {
        if !collection.poll_all(counters) { return Err(collection); }
        {
            let Tracked(idle) = collection.drains(counters);
            assert forall|gate: vstd::tokens::InstanceId| #[trigger] gates.difference(keep).contains(gate)
                implies idle.domain().contains(gate) by {
                let i = choose|i: int| #![auto] 0 <= i < counters.len() && counters@[i].id() == gate;
                assert(idle.domain().contains(counters@[i].id()));
            };
            prepare_queue(queue, lock, handle, Ghost(gates), Ghost(keep), Tracked(idle));
        }
        collection.restore_all(counters);
        Ok(collection.into_controls(counters))
    }
    pub open spec fn prepared_records<T>(records: Seq<Entry<'_, T>>, gates: Set<vstd::tokens::InstanceId>) -> bool {
        forall|i: int| 0 <= i < records.len() ==> (#[trigger] records[i]).prepared_for(gates)
    }
    pub fn prepare_reserve_collected<'node, T>(
        current: &super::super::rotation::current_atomic::Current,
        mut queue: super::super::rotation::barrier_ownership::QueueState<Entry<'node, T>>,
        lock: &super::super::rotation::barrier_ownership::QueueLock<Entry<'node, T>>,
        handle: &super::super::rotation::barrier_ownership::QueueHandle<'_, Entry<'node, T>>,
        collection: super::super::rotation::drain::atomic_stripes::$module::Collection,
        counters: &Vec<super::super::rotation::drain::atomic_counter::$module::Counter>,
        Ghost(gates): Ghost<Set<vstd::tokens::InstanceId>>,
        Ghost(keep): Ghost<Set<vstd::tokens::InstanceId>>)
        -> (result: (
            Result<super::super::rotation::current_atomic::ReservedQueue<Entry<'node, T>>,
                super::super::rotation::barrier_ownership::QueueState<Entry<'node, T>>>,
            Result<Tracked<Map<nat, super::super::rotation::drain::atomic_counter::lifecycle::control>>,
                super::super::rotation::drain::atomic_stripes::$module::Collection>))
        requires current.inv(), current.owner() == queue.domain,
            lock.inv(queue), handle.rwlock() == *lock, collection.inv(counters@),
            lock.pred().preparation.id() == current.gate(queue.index),
            forall|entry: Entry<'node, T>| (#[trigger] (lock.pred().payload_inv)(entry)) ==
                (entry.inv() && entry.owner() == queue.domain && entry.gates() == gates),
            forall|entry: Entry<'node, T>, bound: Set<vstd::tokens::InstanceId>|
                (#[trigger] (lock.pred().prepared_inv)(entry, bound)) == entry.prepared_for(bound),
            forall|gate: vstd::tokens::InstanceId| #[trigger] gates.difference(keep).contains(gate)
                ==> exists|i: int| #![auto] 0 <= i < counters.len() && counters@[i].id() == gate,
        ensures match result.0 {
                Ok(reserved) => result.1.is_ok() && reserved.inv(current, lock)
                    && reserved.index() == queue.index && reserved.bound() == keep
                    && reserved.records().len() == queue.records.len()
                    && (forall|i: int| 0 <= i < queue.records.len()
                        ==> (#[trigger] reserved.records()[i]).memory() == queue.records@[i].memory()),
                Err(returned) => lock.inv(returned) && returned.domain == queue.domain && returned.index == queue.index
                    && returned.preparation == queue.preparation && returned.instance == queue.instance
                    && returned.records.len() == queue.records.len()
                    && (forall|i: int| 0 <= i < queue.records.len()
                        ==> (#[trigger] returned.records@[i]).memory() == queue.records@[i].memory())
                    && (result.1.is_err() ==> returned.records@ == queue.records@),
            },
            match result.1 {
                Ok(controls) => super::super::rotation::drain::atomic_stripes::$module::controls_match(counters@, controls@)
                    && prepared_records(match result.0 { Ok(reserved) => reserved.records(), Err(returned) => returned.records@ }, keep),
                Err(remaining) => remaining.inv(counters@) && result.0.is_err(),
            },
    {
        if current.recheck(&queue, lock, handle) != queue.index {
            return (Err(queue), Err(collection));
        }
        let preparation = prepare_collected_queue(collection, counters, &mut queue, lock, handle, Ghost(gates), Ghost(keep));
        match preparation {
            Err(remaining) => (Err(queue), Err(remaining)),
            Ok(controls) => (current.reserve(queue, lock, handle, Ghost(keep)), Ok(controls)),
        }
    }
    pub enum StripedAttempt<'a> {
        Published(owned_rotation::State, Tracked<super::super::rotation::queue_preparation::phase::prepared>),
        Retry(owned_rotation::CollectionHandoff<'a>, atomic_stripes::Collection, Tracked<super::super::rotation::queue_preparation::phase::ready>),
        Stale(owned_rotation::State, Tracked<super::super::rotation::queue_preparation::phase::ready>),
    }
    #[verifier::exec_allows_no_decreases_clause]
    pub fn prepare_publish_striped<'a, 'node, T>(
        current: &super::super::rotation::current_atomic::Current, state: owned_rotation::State,
        transition: &'a owned_rotation::TransitionLock, transition_handle: &'a owned_rotation::TransitionHandle<'a>,
        zero: &Vec<Counter>, one: &Vec<Counter>,
        lock: &super::super::rotation::barrier_ownership::QueueLock<Entry<'node, T>>,
        owner: *const u8, Tracked(next_ready): Tracked<super::super::rotation::queue_preparation::phase::ready>)
        -> (result: StripedAttempt<'a>)
        requires transition.inv(state), transition_handle.rwlock() == *transition, state.pending().is_none(),
            state.sealed(transition.pred().counters(!lock.pred().index), !lock.pred().index),
            transition.pred().zero == zero@, transition.pred().one == one@, transition.pred().domain == owner,
            current.inv(), current.owner() == owner, lock.pred().domain == owner,
            lock.pred().preparation.id() == current.gate(lock.pred().index), next_ready.instance_id() == current.gate(!lock.pred().index),
            forall|entry: Entry<'node, T>, bound: Set<vstd::tokens::InstanceId>| (#[trigger] (lock.pred().prepared_inv)(entry, bound)) == entry.prepared_for(bound),
            forall|entry: Entry<'node, T>| (#[trigger] (lock.pred().payload_inv)(entry)) ==
                (entry.inv() && entry.owner() == owner && entry.gates() == atomic_stripes::gate_ids(zero@).union(atomic_stripes::gate_ids(one@))),
        ensures match result {
            StripedAttempt::Published(done, ticket) => transition.inv(done) && done.pending() == Some(lock.pred().index)
                && done.sealed(transition.pred().counters(lock.pred().index), lock.pred().index)
                && done.opened(transition.pred().counters(!lock.pred().index), !lock.pred().index)
                && ticket@.instance_id() == lock.pred().preparation.id()
                && ticket@.value() == atomic_stripes::gate_ids(transition.pred().counters(lock.pred().index)),
            StripedAttempt::Retry(handoff, collection, ready) => handoff.inv() && handoff.lock() == *transition
                && handoff.index() == lock.pred().index && handoff.original() == state
                && handoff.next() == transition.pred().counters(!lock.pred().index) && collection.inv(handoff.next()) && ready@ == next_ready,
            StripedAttempt::Stale(returned, ready) => transition.inv(returned) && returned.pending().is_none()
                && returned.sealed(transition.pred().counters(!lock.pred().index), !lock.pred().index)
                && returned.controls(lock.pred().index) == state.controls(lock.pred().index) && ready@ == next_ready,
        },
    {
        let (queue, handle) = lock.acquire_write();
        let index = queue.index;
        let next = if index { zero } else { one };
        let (handoff, collection) = owned_rotation::collect_next(state, transition, transition_handle, index, next);
        resume_publish_striped(current, handoff, collection, queue, handle, transition, transition_handle,
            zero, one, lock, owner, Tracked(next_ready))
    }
    #[verifier::exec_allows_no_decreases_clause]
    pub fn resume_publish_striped<'a, 'node, T>(
        current: &super::super::rotation::current_atomic::Current,
        handoff: owned_rotation::CollectionHandoff<'a>, collection: atomic_stripes::Collection,
        queue: super::super::rotation::barrier_ownership::QueueState<Entry<'node, T>>,
        handle: super::super::rotation::barrier_ownership::QueueHandle<'_, Entry<'node, T>>,
        transition: &'a owned_rotation::TransitionLock, transition_handle: &'a owned_rotation::TransitionHandle<'a>,
        zero: &Vec<Counter>, one: &Vec<Counter>,
        lock: &super::super::rotation::barrier_ownership::QueueLock<Entry<'node, T>>,
        owner: *const u8, Tracked(next_ready): Tracked<super::super::rotation::queue_preparation::phase::ready>)
        -> (result: StripedAttempt<'a>)
        requires handoff.inv(), handoff.lock() == *transition, handoff.index() == lock.pred().index,
            handoff.next() == transition.pred().counters(!lock.pred().index), collection.inv(handoff.next()),
            transition_handle.rwlock() == *transition, lock.inv(queue), handle.rwlock() == *lock,
            transition.pred().zero == zero@, transition.pred().one == one@, transition.pred().domain == owner,
            current.inv(), current.owner() == owner, lock.pred().domain == owner,
            lock.pred().preparation.id() == current.gate(lock.pred().index), next_ready.instance_id() == current.gate(!lock.pred().index),
            forall|entry: Entry<'node, T>, bound: Set<vstd::tokens::InstanceId>| (#[trigger] (lock.pred().prepared_inv)(entry, bound)) == entry.prepared_for(bound),
            forall|entry: Entry<'node, T>| (#[trigger] (lock.pred().payload_inv)(entry)) ==
                (entry.inv() && entry.owner() == owner && entry.gates() == atomic_stripes::gate_ids(zero@).union(atomic_stripes::gate_ids(one@))),
        ensures match result {
            StripedAttempt::Published(done, ticket) => transition.inv(done) && done.pending() == Some(lock.pred().index)
                && done.sealed(transition.pred().counters(lock.pred().index), lock.pred().index)
                && done.opened(transition.pred().counters(!lock.pred().index), !lock.pred().index)
                && ticket@.instance_id() == lock.pred().preparation.id()
                && ticket@.value() == atomic_stripes::gate_ids(transition.pred().counters(lock.pred().index)),
            StripedAttempt::Retry(waiting, remaining, ready) => waiting.inv() && waiting.lock() == *transition
                && waiting.index() == lock.pred().index && waiting.original() == handoff.original()
                && waiting.next() == transition.pred().counters(!lock.pred().index) && remaining.inv(waiting.next()) && ready@ == next_ready,
            StripedAttempt::Stale(returned, ready) => transition.inv(returned) && returned.pending().is_none()
                && returned.sealed(transition.pred().counters(!lock.pred().index), !lock.pred().index)
                && returned.controls(lock.pred().index) == handoff.original().controls(lock.pred().index) && ready@ == next_ready,
        },
    {
        let index = queue.index;
        let next = if index { zero } else { one };
        let ghost gates = atomic_stripes::gate_ids(zero@).union(atomic_stripes::gate_ids(one@));
        let ghost keep = atomic_stripes::gate_ids(transition.pred().counters(index));
        assert forall|gate: vstd::tokens::InstanceId| #[trigger] gates.difference(keep).contains(gate)
            implies exists|i: int| #![auto] 0 <= i < next.len() && next@[i].id() == gate by {
            let ids = next@.map(|i: int, counter: Counter| counter.id());
            assert(atomic_stripes::gate_ids(next@).contains(gate));
            let i = choose|i: int| 0 <= i < ids.len() && ids[i] == gate;
            assert(next@[i].id() == gate);
        };
        let (reservation, collected) = prepare_reserve_collected(current, queue, lock, &handle, collection, next, Ghost(gates), Ghost(keep));
        match collected {
            Err(remaining) => {
                let queue = reservation.err().unwrap();
                handle.release_write(queue);
                StripedAttempt::Retry(handoff, remaining, Tracked(next_ready))
            },
            Ok(controls) => {
                let mut restored = handoff.restore(controls);
                match reservation {
                    Err(queue) => {
                        handle.release_write(queue);
                        StripedAttempt::Stale(restored, Tracked(next_ready))
                    },
                    Ok(reserved) => {
                        let prepared = owned_rotation::begin(&mut restored, transition, transition_handle, current, index, zero, one,
                            reserved, lock, handle, Tracked(next_ready));
                        StripedAttempt::Published(restored, prepared)
                    },
                }
            },
        }
    }
    pub open spec fn valid_records<T>(records: Seq<Entry<'_, T>>, owner: *const u8) -> bool {
        forall|i: int| 0 <= i < records.len() ==> (#[trigger] records[i]).inv() && records[i].owner() == owner
    }
    pub fn recover_pending_striped<'a, 'node, T>(
        current: &super::super::rotation::current_atomic::Current,
        handoff: owned_rotation::PendingHandoff<'a>, mut collection: atomic_stripes::Collection, counters: &Vec<Counter>,
        lock: &super::super::rotation::barrier_ownership::QueueLock<Entry<'node, T>>,
        owner: *const u8, Tracked(prepared): Tracked<super::super::rotation::queue_preparation::phase::prepared>)
        -> (result: Result<(owned_rotation::State, Vec<Tracked<HeapPermission<T>>>,
            Tracked<super::super::rotation::queue_preparation::phase::ready>, Ghost<Seq<Entry<'node, T>>>),
            (owned_rotation::PendingHandoff<'a>, atomic_stripes::Collection, Tracked<super::super::rotation::queue_preparation::phase::prepared>)>)
        requires handoff.inv(), counters@ == handoff.counters(), collection.inv(counters@),
            current.inv(), current.owner() == owner, lock.pred().preparation.id() == current.gate(handoff.index()),
            handoff.lock().pred().domain == owner, lock.pred().domain == owner, lock.pred().index == handoff.index(),
            prepared.instance_id() == lock.pred().preparation.id(), prepared.value() == atomic_stripes::gate_ids(counters@),
            forall|entry: Entry<'node, T>| (#[trigger] (lock.pred().payload_inv)(entry)) ==> entry.inv() && entry.owner() == owner,
            forall|entry: Entry<'node, T>, bound: Set<vstd::tokens::InstanceId>| (#[trigger] (lock.pred().prepared_inv)(entry, bound)) == entry.prepared_for(bound),
        ensures match result {
            Err((waiting, remaining, ticket)) => waiting == handoff && remaining.inv(counters@) && ticket@ == prepared,
            Ok((state, recovered, ready, source)) => handoff.lock().inv(state) && state.pending().is_none()
                && state.sealed(counters@, handoff.index()) && state.controls(!handoff.index()) == handoff.original().controls(!handoff.index())
                && ready@.instance_id() == lock.pred().preparation.id()
                && valid_records(source@, owner) && prepared_records(source@, prepared.value())
                && recovered.len() == source@.len()
                && (forall|i: int| 0 <= i < source@.len() ==> (#[trigger] recovered@[i])@ == source@[source@.len() - 1 - i].memory()),
        },
    {
        if !collection.poll_all(counters) { return Err((handoff, collection, Tracked(prepared))); }
        let mut completed: Option<(Vec<Tracked<HeapPermission<T>>>,
            Tracked<super::super::rotation::queue_preparation::phase::ready>, Ghost<Seq<Entry<'node, T>>>)> = None;
        let mut finished = None;
        super::super::rotation::finish_rotation!(callback_result;
            vstd::prelude::verus_exec_expr!({
                completed = Some({
                    let Tracked(drains) = collection.drains(counters);
                    let held = lock.acquire_write();
                    super::super::rotation::barrier_ownership::prepared(lock, &held.0, Tracked(&prepared));
                    let ghost source = held.0.records@;
                    let tracked mut ticket = Some(prepared);
                    let (recovered, ready) = recover_prepared_locked(owner, held, lock, Tracked(&mut ticket), Tracked(drains));
                    (recovered, ready, Ghost(source))
                });
            }),
            vstd::prelude::verus_exec_expr!({
                assert(completed.is_some());
                let callback = completed.as_ref().unwrap();
                finished = Some(handoff.finish(collection, counters, current, lock, Tracked(callback.1.borrow())));
            })
        );
        let (recovered, ready, source) = completed.unwrap();
        Ok((finished.unwrap(), recovered, ready, source))
    }
    pub fn recover_prepared_locked<'node, T>(owner: *const u8,
        held: (super::super::rotation::barrier_ownership::QueueState<Entry<'node, T>>,
            super::super::rotation::barrier_ownership::QueueHandle<'_, Entry<'node, T>>),
        lock: &super::super::rotation::barrier_ownership::QueueLock<Entry<'node, T>>,
        Tracked(prepared): Tracked<&mut Option<super::super::rotation::queue_preparation::phase::prepared>>,
        Tracked(drains): Tracked<&DrainSet>)
        -> (result: (Vec<Tracked<HeapPermission<T>>>, Tracked<super::super::rotation::queue_preparation::phase::ready>))
        requires held.1.rwlock() == *lock, lock.inv(held.0), held.0.domain == owner, drains.inv(),
            old(prepared).is_some(), old(prepared).unwrap().instance_id() == lock.pred().preparation.id(),
            old(prepared).unwrap().value() == drains.domain(),
            forall|entry: Entry<'node, T>| (#[trigger] (lock.pred().payload_inv)(entry)) ==> entry.inv() && entry.owner() == owner,
            forall|entry: Entry<'node, T>, bound: Set<vstd::tokens::InstanceId>| (#[trigger] (lock.pred().prepared_inv)(entry, bound))
                ==> entry.prepared_for(bound),
        ensures valid_records(held.0.records@, owner), final(prepared).is_none(), result.1@.instance_id() == lock.pred().preparation.id(),
            result.0.len() == held.0.records.len(),
            forall|i: int| 0 <= i < result.0.len() ==> (#[trigger] result.0@[i])@ == held.0.records@[held.0.records.len() - 1 - i].memory(),
    {
        let ghost source = held.0.records@;
        let (withdrawal, ready) = super::super::rotation::locked_detachment::take_prepared(owner, held, lock, Tracked(prepared));
        let mut records = withdrawal.into_records();
        let mut memories: Vec<Tracked<HeapPermission<T>>> = Vec::new();
        while records.len() > 0
            invariant drains.inv(), records@ == source.take(records.len() as int),
                records.len() + memories.len() == source.len(),
                ready@.instance_id() == lock.pred().preparation.id(), prepared.is_none(),
                forall|i: int| 0 <= i < records.len() ==> (#[trigger] records@[i]).inv() && records@[i].prepared_for(drains.domain()),
                forall|i: int| 0 <= i < memories.len() ==> (#[trigger] memories@[i])@ == source[source.len() - 1 - i].memory(),
            decreases records.len(),
        {
            let entry = records.pop().unwrap();
            let memory = entry.recover_prepared(Tracked(drains));
            memories.push(memory);
        }
        (memories, ready)
    }
    /// Detachment transfers the exact queue contents; drain authority is borrowed
    /// throughout recovery of each allocation through its node's resource lock.
    pub fn recover_locked<'node, T>(owner: *const u8,
        held: (super::super::rotation::barrier_ownership::QueueState<Entry<'node, T>>,
            super::super::rotation::barrier_ownership::QueueHandle<'_, Entry<'node, T>>),
        Tracked(drains): Tracked<&DrainSet>)
        -> (memories: Vec<Tracked<HeapPermission<T>>>)
        requires held.1.rwlock().inv(held.0), held.0.domain == owner, drains.inv(),
            forall|entry: Entry<'node, T>| (#[trigger] (held.1.rwlock().pred().payload_inv)(entry))
                ==> entry.inv() && entry.owner() == owner && drains.covers(entry.gates()),
        ensures valid_records(held.0.records@, owner), memories.len() == held.0.records.len(),
            forall|i: int| 0 <= i < memories.len() ==> (#[trigger] memories@[i])@ == held.0.records@[held.0.records.len() - 1 - i].memory(),
    {
        let ghost source = held.0.records@;
        let mut records = super::super::rotation::locked_detachment::take_locked(owner, held);
        let mut memories: Vec<Tracked<HeapPermission<T>>> = Vec::new();
        while records.len() > 0
            invariant drains.inv(), records@ == source.take(records.len() as int),
                records.len() + memories.len() == source.len(),
                forall|i: int| 0 <= i < records.len() ==> (#[trigger] records@[i]).inv() && drains.covers(records@[i].gates()),
                forall|i: int| 0 <= i < memories.len() ==> (#[trigger] memories@[i])@ == source[source.len() - 1 - i].memory(),
            decreases records.len(),
        {
            let entry = records.pop().unwrap();
            let memory = entry.recover(Tracked(drains));
            memories.push(memory);
        }
        memories
    }
    #[verifier::exec_allows_no_decreases_clause]
    pub fn register<'node, T>(current: &super::super::rotation::current_atomic::Current,
        zero: &super::super::rotation::barrier_ownership::QueueLock<Entry<'node, T>>,
        one: &super::super::rotation::barrier_ownership::QueueLock<Entry<'node, T>>,
        entry: Entry<'node, T>)
        -> (receipt: super::super::rotation::atomic_registration::Registered<Entry<'node, T>>)
        requires super::super::rotation::atomic_registration::compatible(current, zero, one, entry),
            entry.inv(), current.owner() == entry.owner(),
        ensures receipt.after() == receipt.before().push(entry),
    { super::super::rotation::atomic_registration::register(current, zero, one, entry) }
    }
    }
};
}
width!(word32, u32);
width!(word64, u64);

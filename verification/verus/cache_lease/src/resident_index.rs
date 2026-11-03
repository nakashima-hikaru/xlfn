//! Guarded resident-to-observation transfer. Hash policy and native representation
//! are not verified here; a cell models one protected index entry.
use vstd::prelude::*;
use vstd::rwlock::{RwLock, RwLockPredicate};
use super::heap_permission::HeapPermission;
use super::pin_ownership::{cache_pins, PinKind};
use super::retained_observations::Ticket;
use super::rotation::drain::permit_shares::Scope;
macro_rules! width {
    ($module:ident) => {
    pub mod $module {
    use super::*;
    use super::super::queued_atomic::$module::{Node, Entry, Lease, complete_lookup};
    use super::super::rotation::drain::atomic_counter::$module::Counter;
    verus! {
    pub struct Resident<'node, T> {
        node: &'node Node<T>, pointer: *mut T, pin: Tracked<cache_pins::pins<HeapPermission<T>>>,
    }
    impl<'node, T> Resident<'node, T> {
        pub closed spec fn inv(&self) -> bool {
            self.node.inv() && self.pin@.instance_id() == self.node.id() && self.pin@.element().1 == PinKind::Resident
                && self.pin@.element().0.ptr() == self.pointer && self.pin@.element().0.is_init()
        }
        pub closed spec fn node_id(&self) -> vstd::tokens::InstanceId { self.node.id() }
        pub closed spec fn owner(&self) -> *const u8 { self.node.owner() }
        pub closed spec fn gates(&self) -> Set<vstd::tokens::InstanceId> { self.node.gates() }
        pub closed spec fn memory(&self) -> HeapPermission<T> { self.pin@.element().0 }
        pub fn new(node: &'node Node<T>, pointer: *mut T, Tracked(pin): Tracked<cache_pins::pins<HeapPermission<T>>>) -> (value: Self)
            requires node.inv(), pin.instance_id() == node.id(), pin.element().1 == PinKind::Resident,
                pin.element().0.ptr() == pointer, pin.element().0.is_init(),
            ensures value.inv(), value.owner() == node.owner(), value.gates() == node.gates(), value.memory() == pin.element().0,
        { Resident { node, pointer, pin: Tracked(pin) } }
        pub fn retire(self) -> (entry: Option<Entry<'node, T>>)
            requires self.inv(), ensures entry.is_some() ==> entry.unwrap().inv() && entry.unwrap().memory() == self.memory()
                && entry.unwrap().owner() == self.owner() && entry.unwrap().gates() == self.gates(),
        { self.node.release(self.pointer, self.pin) }
    }
    #[verifier::reject_recursive_types(T)]
    pub struct Predicate<T> { pub owner: *const u8, pub gates: Set<vstd::tokens::InstanceId>,
        pub memory_inv: spec_fn(HeapPermission<T>) -> bool }
    impl<'node, T> RwLockPredicate<Option<Resident<'node, T>>> for Predicate<T> {
        closed spec fn inv(self, value: Option<Resident<'node, T>>) -> bool {
            value.is_some() ==> value.unwrap().inv() && value.unwrap().owner() == self.owner && value.unwrap().gates() == self.gates && (self.memory_inv)(value.unwrap().memory())
        }
    }
    pub type Cell<'node, T> = RwLock<Option<Resident<'node, T>>, Predicate<T>>;
    pub fn new<'node, T>(value: Resident<'node, T>) -> (cell: Cell<'node, T>)
        requires value.inv(), ensures cell.pred().owner == value.owner(), cell.pred().gates == value.gates(),
    { RwLock::new(Some(value), Ghost(Predicate { owner: value.owner(), gates: value.gates(), memory_inv: |memory: HeapPermission<T>| true })) }
    pub fn new_checked<'node, T>(value: Resident<'node, T>, Ghost(memory_inv): Ghost<spec_fn(HeapPermission<T>) -> bool>)
        -> (cell: Cell<'node, T>)
        requires value.inv(), memory_inv(value.memory()),
        ensures cell.pred().owner == value.owner(), cell.pred().gates == value.gates(), cell.pred().memory_inv == memory_inv,
    { RwLock::new(Some(value), Ghost(Predicate { owner: value.owner(), gates: value.gates(), memory_inv })) }
    /// A snapshot owns an observation receipt, never the stored resident pin.
    pub struct Snapshot<'node, T> { node: &'node Node<T>, pointer: *mut T, ticket: Tracked<Ticket<T>> }
    impl<'node, T> Snapshot<'node, T> {
        pub closed spec fn inv(&self) -> bool {
            self.node.inv() && self.ticket@.ledger_id() == self.node.observation_id()
                && self.ticket@.node_id() == self.node.id()
                && self.ticket@.memory().ptr() == self.pointer && self.ticket@.memory().is_init()
        }
        pub closed spec fn node_id(&self) -> vstd::tokens::InstanceId { self.node.id() }
        pub closed spec fn owner(&self) -> *const u8 { self.node.owner() }
        pub closed spec fn gates(&self) -> Set<vstd::tokens::InstanceId> { self.node.gates() }
        pub closed spec fn scope_id(&self) -> vstd::tokens::InstanceId { self.ticket@.scope_id() }
        pub closed spec fn memory(&self) -> HeapPermission<T> { self.ticket@.memory() }
        #[verifier::exec_allows_no_decreases_clause]
        pub fn into_lease(self, counter: &Counter, eligible: bool, resident_after: bool, Tracked(scope): Tracked<Scope>)
            -> (result: (Option<Lease<'node, T>>, Option<Entry<'node, T>>))
            requires self.inv(), counter.inv(), scope.inv(), scope.len() == 1,
                scope.id() == self.scope_id(), scope.gate_id() == counter.id(),
            ensures result.0.is_some() ==> eligible && resident_after && result.1.is_none()
                && result.0.unwrap().inv() && result.0.unwrap().memory() == self.memory() && result.0.unwrap().owner() == self.owner(),
                result.1.is_some() ==> eligible && !resident_after && result.0.is_none()
                    && result.1.unwrap().inv() && result.1.unwrap().memory() == self.memory() && result.1.unwrap().owner() == self.owner(),
                !eligible ==> result.0.is_none() && result.1.is_none(),
        {
            let node = self.node;
            let pointer = self.pointer;
            let (pin, retired) = self.complete(counter, eligible, resident_after, Tracked(scope));
            match pin {
                Some(pin) => (Some(Lease::new(node, pointer, pin)), retired),
                None => (None, retired),
            }
        }
        #[verifier::exec_allows_no_decreases_clause]
        pub fn complete(self, counter: &Counter, eligible: bool, resident_after: bool, Tracked(scope): Tracked<Scope>)
            -> (result: (Option<Tracked<cache_pins::pins<HeapPermission<T>>>>, Option<Entry<'node, T>>))
            requires self.inv(), counter.inv(), scope.inv(), scope.len() == 1, scope.id() == self.scope_id(), scope.gate_id() == counter.id(),
            ensures result.0.is_some() ==> eligible && resident_after && result.1.is_none()
                && result.0.unwrap()@.instance_id() == self.node_id()
                && result.0.unwrap()@.element() == (self.memory(), PinKind::Lease),
                result.1.is_some() ==> eligible && !resident_after && result.0.is_none()
                    && result.1.unwrap().inv() && result.1.unwrap().memory() == self.memory() && result.1.unwrap().owner() == self.owner(),
                !eligible ==> result.0.is_none() && result.1.is_none(),
        { complete_lookup(self.node, counter, self.pointer, eligible, resident_after, Tracked(scope), self.ticket) }
    }
    impl<'node, V> Snapshot<'node, super::super::inline_value::Allocation<V>> {
        pub fn resident(&self) -> bool
            requires self.inv(),
        { self.node.observed_resident(self.pointer, Tracked(self.ticket.borrow())) }
        #[verifier::exec_allows_no_decreases_clause]
        pub fn complete_inline(self, counter: &Counter, eligible: bool, Tracked(scope): Tracked<Scope>)
            -> (result: (Option<Lease<'node, super::super::inline_value::Allocation<V>>>, Option<Entry<'node, super::super::inline_value::Allocation<V>>>))
            requires self.inv(), counter.inv(), scope.inv(), scope.len() == 1,
                scope.id() == self.scope_id(), scope.gate_id() == counter.id(),
            ensures result.0.is_some() ==> eligible && result.1.is_none()
                && result.0.unwrap().inv() && result.0.unwrap().memory() == self.memory() && result.0.unwrap().owner() == self.owner(),
                result.1.is_some() ==> eligible && result.0.is_none()
                    && result.1.unwrap().inv() && result.1.unwrap().memory() == self.memory() && result.1.unwrap().owner() == self.owner(),
                !eligible ==> result.0.is_none() && result.1.is_none(),
        {
            let (pin, retired, _) = super::super::queued_atomic::$module::complete_inline_lookup(
                self.node, counter, self.pointer, eligible, Tracked(scope), self.ticket);
            match pin {
                Some(pin) => (Some(Lease::new(self.node, self.pointer, pin)), retired),
                None => (None, retired),
            }
        }
        pub fn generation(&self) -> (value: u64)
            requires self.inv(), ensures value == self.memory().value().generation,
        { self.node.observed_generation(self.pointer, Tracked(self.ticket.borrow())) }
    }
    /// Observation is issued while the read guard still protects residency.
    /// Eviction may consume the resident pin immediately after release_read.
    pub fn lookup<'node, T>(cell: &Cell<'node, T>, Tracked(scope): Tracked<&mut Scope>) -> (result: Option<Snapshot<'node, T>>)
        requires old(scope).inv(), cell.pred().gates.contains(old(scope).gate_id()),
        ensures final(scope).inv(), final(scope).id() == old(scope).id(), final(scope).gate_id() == old(scope).gate_id(),
            result.is_some() ==> result.unwrap().inv() && result.unwrap().owner() == cell.pred().owner
                && result.unwrap().gates() == cell.pred().gates && result.unwrap().scope_id() == final(scope).id()
                && (cell.pred().memory_inv)(result.unwrap().memory())
                && final(scope).len() == old(scope).len() + 1,
            result.is_none() ==> final(scope).len() == old(scope).len(),
    {
        let guard = cell.acquire_read();
        let stored = guard.borrow();
        let result = match stored {
            Some(value) => {
                let ticket = value.node.observe(Tracked(scope), Tracked(value.pin.borrow()));
                Some(Snapshot { node: value.node, pointer: value.pointer, ticket })
            },
            None => None,
        };
        guard.release_read();
        result
    }
    /// Acquire actual admission, transfer a guarded snapshot, release the index
    /// guard, then use the production-shared post-observation lookup expression.
    #[verifier::exec_allows_no_decreases_clause]
    pub fn lookup_complete<'node, T>(cell: &Cell<'node, T>, counter: &Counter, eligible: bool, resident_after: bool)
        -> (result: (Option<Lease<'node, T>>, Option<Entry<'node, T>>, Ghost<Option<HeapPermission<T>>>))
        requires counter.inv(), cell.pred().gates.contains(counter.id()),
        ensures result.2@.is_some() ==> (cell.pred().memory_inv)(result.2@.unwrap()),
            result.0.is_some() ==> result.2@.is_some() && eligible && resident_after && result.1.is_none()
                && result.0.unwrap().inv() && result.0.unwrap().memory() == result.2@.unwrap()
                && result.0.unwrap().owner() == cell.pred().owner,
            result.1.is_some() ==> result.2@.is_some() && eligible && !resident_after && result.0.is_none()
                && result.1.unwrap().inv() && result.1.unwrap().memory() == result.2@.unwrap()
                && result.1.unwrap().owner() == cell.pred().owner,
            !eligible ==> result.0.is_none() && result.1.is_none(),
    {
        let (outcome, Tracked(scope)) = counter.acquire_scope();
        match outcome {
            super::super::rotation::drain::transitions::TransitionOutcome::Success(_) => {
                let tracked mut scope = scope.tracked_unwrap();
                match lookup(cell, Tracked(&mut scope)) {
                    Some(snapshot) => {
                        let ghost memory = snapshot.memory();
                        let (lease, retired) = snapshot.into_lease(counter, eligible, resident_after, Tracked(scope));
                        (lease, retired, Ghost(Some(memory)))
                    },
                    None => {
                        counter.release_scope(Tracked(scope));
                        (None, None, Ghost(None))
                    },
                }
            },
            _ => (None, None, Ghost(None)),
        }
    }
    /// Extraction transfers the owning resident fragment out of the write lock.
    pub fn remove<'node, T>(cell: &Cell<'node, T>) -> (value: Option<Resident<'node, T>>)
        ensures value.is_some() ==> value.unwrap().inv() && value.unwrap().owner() == cell.pred().owner && value.unwrap().gates() == cell.pred().gates,
    {
        let (value, handle) = cell.acquire_write();
        handle.release_write(None);
        value
    }
    }
    }
};
}
width!(word32);
width!(word64);

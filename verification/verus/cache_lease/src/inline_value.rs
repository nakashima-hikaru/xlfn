//! The proof borrows an entire initialized allocation before projecting its value.
//! Atomic/address correspondence is not inferred from sharing the field declaration.
use vstd::prelude::*;
use super::heap_permission::HeapPermission;
super::node_layout::declare_node! {
    pub struct Allocation<V> {
        pins: std::sync::atomic::AtomicUsize,
        resident: std::sync::atomic::AtomicBool,
        domain: *mut u8,
    }
}
macro_rules! width {
    ($module:ident) => {
    pub mod $module {
    use super::*;
    use super::super::queued_atomic::$module::{Node, Lease, Entry};
    use super::super::pin_ownership::{cache_pins, PinKind};
    use super::super::resident_index::$module as index;
    use super::super::rotation::drain::atomic_counter::$module::Counter;
    verus! {
    /// Read the immutable owner from the same initialized allocation that enters
    /// the pin ledger, instead of supplying an independent owner argument.
    pub fn initialize<V>(pointer: *mut Allocation<V>, Tracked(memory): Tracked<HeapPermission<Allocation<V>>>,
        Ghost(gates): Ghost<Set<vstd::tokens::InstanceId>>)
        -> (result: (Node<Allocation<V>>, Tracked<cache_pins::pins<HeapPermission<Allocation<V>>>>))
        requires memory.is_init(), memory.ptr() == pointer,
        ensures result.0.inv(), result.0.gates() == gates,
            result.0.owner() == memory.value().domain as *const u8,
            result.1@.instance_id() == result.0.id(), result.1@.element() == (memory, PinKind::Creator),
    {
        let allocation = vstd::raw_ptr::ptr_ref(pointer, Tracked(memory.borrow()));
        let owner = super::super::node_layout::domain!(allocation);
        Node::new(owner as *const u8, Tracked(memory), Ghost(gates))
    }
    /// Store the allocation/owner relation in the protected index predicate.
    pub fn new_index<'node, V>(resident: index::Resident<'node, Allocation<V>>) -> (cell: index::Cell<'node, Allocation<V>>)
        requires resident.inv(), resident.memory().value().domain as *const u8 == resident.owner(),
        ensures cell.pred().owner == resident.owner(), cell.pred().gates == resident.gates(),
            forall|memory: HeapPermission<Allocation<V>>| (#[trigger] (cell.pred().memory_inv)(memory))
                == (memory.value().domain as *const u8 == cell.pred().owner),
    {
        let ghost owner = resident.owner();
        index::new_checked(resident, Ghost(|memory: HeapPermission<Allocation<V>>| memory.value().domain as *const u8 == owner))
    }
    #[verifier::exec_allows_no_decreases_clause]
    pub fn lookup<'node, V>(cell: &index::Cell<'node, Allocation<V>>, counter: &Counter, epoch: u64)
        -> (result: (Option<ValueLease<'node, V>>, Option<Entry<'node, Allocation<V>>>, Ghost<Option<HeapPermission<Allocation<V>>>>, Ghost<Option<bool>>))
        requires counter.inv(), cell.pred().gates.contains(counter.id()),
            forall|memory: HeapPermission<Allocation<V>>| (#[trigger] (cell.pred().memory_inv)(memory))
                ==> memory.value().domain as *const u8 == cell.pred().owner,
        ensures result.3@ == Some(false) ==> result.0.is_none() && result.1.is_none(),
            result.0.is_some() ==> result.3@ == Some(true) && result.1.is_none() && result.2@.is_some()
                && result.0.unwrap().inv() && result.0.unwrap().memory() == result.2@.unwrap()
                && result.0.unwrap().memory().value().domain as *const u8 == cell.pred().owner
                && result.0.unwrap().memory().value().generation == epoch,
            result.1.is_some() ==> result.0.is_none() && result.2@.is_some()
                && result.1.unwrap().inv() && result.1.unwrap().memory() == result.2@.unwrap()
                && result.1.unwrap().owner() == result.1.unwrap().memory().value().domain as *const u8,
            result.2@.is_some() && result.2@.unwrap().value().generation != epoch ==> result.0.is_none() && result.1.is_none(),
    {
        let (outcome, Tracked(scope)) = counter.acquire_scope();
        match outcome {
            super::super::rotation::drain::transitions::TransitionOutcome::Success(_) => {
                let tracked mut scope = scope.tracked_unwrap();
                match index::lookup(cell, Tracked(&mut scope)) {
                    Some(snapshot) => {
                        let ghost memory = snapshot.memory();
                        let generation = snapshot.generation();
                        let mut before = None;
                        let eligible = super::super::node_layout::eligible!(generation, epoch, vstd::prelude::verus_exec_expr!({
                            let sample = snapshot.resident();
                            before = Some(sample);
                            sample
                        }));
                        let (lease, retired) = snapshot.complete_inline(counter, eligible, Tracked(scope));
                        match lease {
                            Some(lease) => (Some(ValueLease::new(lease)), retired, Ghost(Some(memory)), Ghost(before)),
                            None => (None, retired, Ghost(Some(memory)), Ghost(before)),
                        }
                    },
                    None => {
                        counter.release_scope(Tracked(scope));
                        (None, None, Ghost(None), Ghost(None))
                    },
                }
            },
            _ => (None, None, Ghost(None), Ghost(None)),
        }
    }
    pub struct ValueLease<'node, V> { lease: Lease<'node, Allocation<V>> }
    impl<'node, V> ValueLease<'node, V> {
        pub closed spec fn inv(&self) -> bool {
            self.lease.inv() && self.memory().value().domain as *const u8 == self.lease.owner()
        }
        pub closed spec fn memory(&self) -> HeapPermission<Allocation<V>> { self.lease.memory() }
        pub fn new(lease: Lease<'node, Allocation<V>>) -> (result: Self)
            requires lease.inv(), lease.memory().value().domain as *const u8 == lease.owner(),
            ensures result.inv(), result.memory() == lease.memory(),
        { ValueLease { lease } }
        pub fn read<'a>(&'a self) -> (value: &'a V)
            requires self.inv(), ensures *value == self.memory().value().value,
        {
            let allocation = self.lease.read();
            super::super::node_layout::value!(allocation)
        }
        pub fn release(self) -> (entry: Option<Entry<'node, Allocation<V>>>)
            requires self.inv(), ensures entry.is_some() ==> entry.unwrap().inv() && entry.unwrap().memory() == self.memory()
                && entry.unwrap().owner() == self.memory().value().domain as *const u8,
        { self.lease.release() }
    }
    }
    }
};
}
width!(word32);
width!(word64);

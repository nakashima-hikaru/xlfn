//! Actual atomic pin updates conserve the same node's storage-backed tokens.
//! vstd atomics are SeqCst; native release sequences and fences remain separate.
use vstd::prelude::*;
use vstd::atomic_ghost::*;
use super::heap_permission::HeapPermission;
use super::pin_ownership::{cache_pins, PinKind};
use super::pin_transitions::{Acquire, Release, pin_retry_expr};
macro_rules! width {
    ($module:ident, $word:ty, $atomic:ident) => {
    pub mod $module {
    use super::*;
    use super::super::pin_transitions::$module as kernel;
    verus! {
    pub tracked struct AtomicState<T> {
        count: cache_pins::count<HeapPermission<T>>, retiring: cache_pins::retiring<HeapPermission<T>>,
    }
    pub struct Predicate<T> { marker: core::marker::PhantomData<T> }
    impl<T> AtomicInvariantPredicate<vstd::tokens::InstanceId, $word, AtomicState<T>> for Predicate<T> {
        closed spec fn atomic_inv(id: vstd::tokens::InstanceId, raw: $word, state: AtomicState<T>) -> bool {
            state.count.instance_id() == id && state.count.value() == raw as nat
                && state.retiring.instance_id() == id && state.retiring.value() == (raw == 0)
        }
    }
    pub struct Pins<T> {
        atomic: $atomic<vstd::tokens::InstanceId, AtomicState<T>, Predicate<T>>,
        instance: Tracked<cache_pins::Instance<HeapPermission<T>>>,
    }
    /// Resources not stored inside the atomic; all belong to its freshly created node.
    pub tracked struct Resources<'scope, T> {
        pub allocation: cache_pins::allocation<HeapPermission<T>>,
        pub creator: cache_pins::pins<HeapPermission<T>>,
        pub observations: super::super::observation_coverage::ObservationLedger<'scope, T>,
    }
    pub fn initialize<'scope, T>(Tracked(memory): Tracked<HeapPermission<T>>,
        Ghost(domain): Ghost<Set<vstd::tokens::InstanceId>>)
        -> (result: (Pins<T>, Tracked<Resources<'scope, T>>))
        requires memory.is_init(),
        ensures result.0.inv(), result.0.domain() == domain,
            result.1@.allocation.instance_id() == result.0.id(), result.1@.allocation.value() == Some(memory),
            result.1@.creator.instance_id() == result.0.id(), result.1@.creator.element() == (memory, PinKind::Creator),
            result.1@.observations.inv(), result.1@.observations.node_id() == result.0.id(),
            result.1@.observations.domain() == domain, result.1@.observations.len() == 0,
            !result.1@.observations.frozen(), result.1@.observations.coverage() == domain,
    {
        let tracked covered = super::super::observation_coverage::initialize_covered_node(memory, domain);
        let pins = Pins::new(Tracked(covered.instance), Tracked(covered.count), Tracked(covered.retiring));
        (pins, Tracked(Resources { allocation: covered.allocation, creator: covered.creator,
            observations: covered.observations }))
    }
    pub tracked struct OwnedResources<T> {
        pub allocation: cache_pins::allocation<HeapPermission<T>>,
        pub creator: cache_pins::pins<HeapPermission<T>>,
        pub observations: super::super::retained_observations::Ledger<T>,
    }
    pub fn initialize_owned<T>(Tracked(memory): Tracked<HeapPermission<T>>,
        Ghost(domain): Ghost<Set<vstd::tokens::InstanceId>>)
        -> (result: (Pins<T>, Tracked<OwnedResources<T>>))
        requires memory.is_init(),
        ensures result.0.inv(), result.0.domain() == domain,
            result.1@.allocation.instance_id() == result.0.id(), result.1@.allocation.value() == Some(memory),
            result.1@.creator.instance_id() == result.0.id(), result.1@.creator.element() == (memory, PinKind::Creator),
            result.1@.observations.inv(), result.1@.observations.node_id() == result.0.id(),
            result.1@.observations.domain() == domain, result.1@.observations.len() == 0,
            !result.1@.observations.frozen(), result.1@.observations.coverage() == domain,
    {
        let tracked node = super::super::scope_ownership::initialize_node(memory, domain);
        let tracked observations = super::super::retained_observations::Ledger::new(&node.instance, node.observing);
        let pins = Pins::new(Tracked(node.instance), Tracked(node.count), Tracked(node.retiring));
        (pins, Tracked(OwnedResources { allocation: node.allocation, creator: node.creator, observations }))
    }
    impl<T> Pins<T> {
        pub closed spec fn id(&self) -> vstd::tokens::InstanceId { self.instance@.id() }
        pub closed spec fn inv(&self) -> bool { self.atomic.well_formed() && self.atomic.constant() == self.id() }
        pub fn new(Tracked(instance): Tracked<cache_pins::Instance<HeapPermission<T>>>,
            Tracked(count): Tracked<cache_pins::count<HeapPermission<T>>>,
            Tracked(retiring): Tracked<cache_pins::retiring<HeapPermission<T>>>) -> (pins: Self)
            requires count.instance_id() == instance.id(), count.value() == 1,
                retiring.instance_id() == instance.id(), !retiring.value(),
            ensures pins.inv(), pins.id() == instance.id(), pins.domain() == instance.domain(),
        {
            let atomic = $atomic::new(Ghost(instance.id()), 1, Tracked(AtomicState { count, retiring }));
            Pins { atomic, instance: Tracked(instance) }
        }
        pub closed spec fn domain(&self) -> Set<vstd::tokens::InstanceId> { self.instance@.domain() }
        pub fn recover(&self, entry: super::super::retirement::RetiredNode<T>,
            Tracked(allocation): Tracked<&mut cache_pins::allocation<HeapPermission<T>>>,
            Tracked(observations): Tracked<&super::super::observation_coverage::ObservationLedger<'_, T>>,
            Tracked(drains): Tracked<&super::super::rotation::drain::atomic_counter::DrainSet>) -> (memory: Tracked<HeapPermission<T>>)
            requires self.inv(), entry.node_id() == self.id(), old(allocation).instance_id() == self.id(),
                old(allocation).value() == Some(entry.memory()), observations.inv(), observations.node_id() == self.id(),
                observations.domain() == self.domain(), drains.inv(), drains.covers(observations.coverage()),
            ensures memory@ == entry.memory(), final(allocation).instance_id() == self.id(), final(allocation).value().is_none(),
        {
            proof { super::super::observation_coverage::zero_after_drain(observations, drains); }
            let tracked observing = observations.count();
            let Tracked(ticket) = entry.into_ticket();
            let tracked mut memory = None;
            atomic_with_ghost!(self.atomic => no_op(); ghost state => {
                self.instance.borrow().retirement_zero(ticket.value(), &state.count, &state.retiring, &ticket);
                memory = Some(self.instance.borrow().reclaim(ticket.value(), allocation, &state.count, observing, &state.retiring, ticket));
            });
            Tracked(memory.tracked_unwrap())
        }
        pub fn instance(&self) -> (instance: Tracked<&cache_pins::Instance<HeapPermission<T>>>)
            requires self.inv(), ensures instance@.id() == self.id(), instance@.domain() == self.domain(),
        { Tracked(self.instance.borrow()) }
        #[verifier::exec_allows_no_decreases_clause]
        pub(crate) fn acquire_observed(&self, Tracked(observation): Tracked<&cache_pins::observations<HeapPermission<T>>>)
            -> (result: (Acquire<$word>, Tracked<Option<cache_pins::pins<HeapPermission<T>>>>, Ghost<$word>))
            requires self.inv(), observation.instance_id() == self.id(),
            ensures result.0 == kernel::acquire_spec(result.2@), match result.0 {
                Acquire::Acquired(_) => result.1@.is_some() && result.1@.unwrap().instance_id() == self.id()
                    && result.1@.unwrap().element() == (observation.element(), PinKind::Lease),
                _ => result.1@.is_none(),
            },
        {
            let tracked mut pin = None;
            super::super::pin_transitions::acquire_retry!(raw, next;
                load = atomic_with_ghost!(self.atomic => load(); ghost state => {  }),
                classify = kernel::acquire(raw),
                attempt = atomic_with_ghost!(self.atomic => compare_exchange_weak(raw, next); returning result; ghost state => {
                    
                    if result is Ok {
                        kernel::successful_acquire_adds_one(raw, next);
                        pin = Some(self.instance.borrow().acquire_from_observation(observation.element(), &mut state.count, observation));
                    }
                }),
                success = (Acquire::Acquired(next), Tracked(pin), Ghost(raw)),
                zero = (Acquire::Zero, Tracked(None), Ghost(raw)),
                overflow = (Acquire::Overflow, Tracked(None), Ghost(raw));
                invariant self.inv(), observation.instance_id() == self.id(),
            )
        }
        #[verifier::exec_allows_no_decreases_clause]
        pub(crate) fn acquire_anchored(&self, Tracked(source): Tracked<&cache_pins::pins<HeapPermission<T>>>, Ghost(target): Ghost<PinKind>)
            -> (result: (Acquire<$word>, Tracked<Option<cache_pins::pins<HeapPermission<T>>>>, Ghost<$word>))
            requires self.inv(), source.instance_id() == self.id(), !(target is Creator),
            ensures result.0 != Acquire::Zero, result.0 == kernel::acquire_spec(result.2@), match result.0 {
                Acquire::Acquired(_) => result.1@.is_some() && result.1@.unwrap().instance_id() == self.id()
                    && result.1@.unwrap().element() == (source.element().0, target),
                _ => result.1@.is_none(),
            },
        {
            let tracked mut pin = None;
            super::super::pin_transitions::acquire_retry!(raw, next;
                load = atomic_with_ghost!(self.atomic => load(); ghost state => { self.instance.borrow().pin_positive(source.element().0, source.element().1, &state.count, source); }),
                classify = kernel::acquire(raw),
                attempt = atomic_with_ghost!(self.atomic => compare_exchange_weak(raw, next); returning result; ghost state => {
                    self.instance.borrow().pin_positive(source.element().0, source.element().1, &state.count, source);
                    if result is Ok {
                        kernel::successful_acquire_adds_one(raw, next);
                        pin = Some(self.instance.borrow().acquire_from_pin(source.element().0, source.element().1, target, &mut state.count, source));
                    }
                }),
                success = (Acquire::Acquired(next), Tracked(pin), Ghost(raw)),
                zero = (Acquire::Zero, Tracked(None), Ghost(raw)),
                overflow = (Acquire::Overflow, Tracked(None), Ghost(raw));
                invariant self.inv(), source.instance_id() == self.id(), !(target is Creator), raw > 0,
            )
        }
        pub fn release_covered(&self, pointer: *mut T,
            Tracked(pin): Tracked<cache_pins::pins<HeapPermission<T>>>,
            Tracked(observations): Tracked<&mut super::super::observation_coverage::ObservationLedger<'_, T>>)
            -> (entry: Option<super::super::retirement::RetiredNode<T>>)
            requires self.inv(), pin.instance_id() == self.id(), pin.element().0.ptr() == pointer, pin.element().0.is_init(),
                old(observations).inv(), old(observations).node_id() == self.id(), old(observations).domain() == self.domain(),
            ensures final(observations).inv(), final(observations).node_id() == self.id(), final(observations).domain() == self.domain(),
                final(observations).len() == old(observations).len(), final(observations).coverage() == old(observations).coverage(),
                entry.is_some() ==> final(observations).frozen() && entry.unwrap().node_id() == self.id()
                    && entry.unwrap().memory() == pin.element().0 && entry.unwrap().pointer() == pointer && entry.unwrap().domain() == self.domain(),
                entry.is_none() ==> final(observations).frozen() == old(observations).frozen(),
        {
            let (outcome, Tracked(ticket)) = self.release(Tracked(pin));
            if let Release::LastPin = outcome {
                let entry = super::super::retirement::RetiredNode::from_ticket(pointer, self.instance(), Tracked(ticket.tracked_unwrap()));
                proof { observations.freeze(&entry); }
                Some(entry)
            } else { None }
        }
        pub closed spec fn namespace(&self) -> int { self.atomic.atomic_inv@.namespace() }
        #[verifier::atomic]
        pub fn recover_owned(&self, Tracked(ticket): Tracked<cache_pins::retirement<HeapPermission<T>>>,
            Tracked(allocation): Tracked<&mut cache_pins::allocation<HeapPermission<T>>>,
            Tracked(observations): Tracked<&super::super::retained_observations::Ledger<T>>,
            Tracked(drains): Tracked<&super::super::rotation::drain::atomic_counter::DrainSet>) -> (memory: Tracked<HeapPermission<T>>)
            requires self.inv(), ticket.instance_id() == self.id(), old(allocation).instance_id() == self.id(),
                old(allocation).value() == Some(ticket.value()), observations.inv(), observations.node_id() == self.id(),
                drains.inv(), observations.domain() == self.domain(), drains.covers(observations.coverage()),
            ensures memory@ == ticket.value(), final(allocation).instance_id() == self.id(), final(allocation).value().is_none(),
            opens_invariants [self.namespace()]
            no_unwind
        {
            proof { observations.zero_after_drain(drains); }
            let tracked observing = observations.count();
            let tracked mut memory = None;
            atomic_with_ghost!(self.atomic => no_op(); ghost state => {
                self.instance.borrow().retirement_zero(ticket.value(), &state.count, &state.retiring, &ticket);
                memory = Some(self.instance.borrow().reclaim(ticket.value(), allocation, &state.count, observing, &state.retiring, ticket));
            });
            Tracked(memory.tracked_unwrap())
        }
        pub(crate) fn release(&self, Tracked(pin): Tracked<cache_pins::pins<HeapPermission<T>>>)
            -> (result: (Release, Tracked<Option<cache_pins::retirement<HeapPermission<T>>>>))
            requires self.inv(), pin.instance_id() == self.id(),
            ensures result.0 != Release::FailStop,
                result.1@.is_some() == (result.0 == Release::LastPin),
                result.1@.is_some() ==> result.1@.unwrap().instance_id() == self.id()
                    && result.1@.unwrap().value() == pin.element().0,
        {
            let tracked mut retirement = None;
            super::super::pin_transitions::release_pin!(previous;
                decrement = atomic_with_ghost!(self.atomic => fetch_sub(1); update previous -> next; returning previous; ghost state => {
                self.instance.borrow().pin_positive(pin.element().0, pin.element().1, &state.count, &pin);
                if previous == 1 {
                    retirement = Some(self.instance.borrow().release_final(pin.element().0, pin.element().1, &mut state.count, pin, &mut state.retiring));
                } else {
                    self.instance.borrow().release_nonfinal(pin.element().0, pin.element().1, &mut state.count, pin);
                }
            }),
                classify = kernel::release(previous),
                // vstd RMW is SeqCst; weak-memory fence behavior is checked by Loom.
                fence = (),
                last = (Release::LastPin, Tracked(retirement)),
                pinned = (Release::StillPinned, Tracked(retirement)),
                fail_stop = (Release::FailStop, Tracked(retirement)),
            )
        }
    }
    }
    }
};
}
width!(word32, u32, AtomicU32);
width!(word64, u64, AtomicU64);

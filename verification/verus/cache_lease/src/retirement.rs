//! Linear retirement payloads carried through the production-shared queue loop.
use vstd::prelude::*;
use super::heap_permission::HeapPermission;
use super::pin_ownership::{cache_pins, recover_memory};
use super::rotation::registration::{Registration, shared_registration};
verus! {
pub struct RetiredNode<T> {
    pointer: *mut T,
    domain: Ghost<Set<vstd::tokens::InstanceId>>,
    ticket: Tracked<cache_pins::retirement<HeapPermission<T>>>,
}
impl<T> RetiredNode<T> {
    pub closed spec fn pointer(&self) -> *mut T { self.pointer }
    pub closed spec fn domain(&self) -> Set<vstd::tokens::InstanceId> { self.domain@ }
    pub closed spec fn node_id(&self) -> vstd::tokens::InstanceId { self.ticket@.instance_id() }
    pub closed spec fn memory(&self) -> HeapPermission<T> { self.ticket@.value() }
    #[verifier::type_invariant]
    pub closed spec fn inv(self) -> bool {
        self.ticket@.value().ptr() == self.pointer && self.ticket@.value().is_init()
    }

    /// The final-pin ticket is moved into the payload, not represented by a
    /// copied node ID. Its memory/allocator ownership remains in node storage.
    pub fn from_ticket(pointer: *mut T,
        Tracked(node): Tracked<&cache_pins::Instance<HeapPermission<T>>>,
        Tracked(ticket): Tracked<cache_pins::retirement<HeapPermission<T>>>,
    ) -> (entry: Self)
        requires ticket.instance_id() == node.id(), ticket.value().ptr() == pointer,
            ticket.value().is_init(),
        ensures entry.node_id() == node.id(), entry.domain() == node.domain(),
            entry.memory() == ticket.value(), entry.pointer() == pointer,
    { RetiredNode { pointer, domain: Ghost(node.domain()), ticket: Tracked(ticket) } }
}

pub open spec fn queue_domain<T>(queue: &Registration<RetiredNode<T>>,
    domain: Set<vstd::tokens::InstanceId>) -> bool {
    (forall|i: int| 0 <= i < queue.zero.len() ==> (#[trigger] queue.zero[i]).domain() == domain)
    && (forall|i: int| 0 <= i < queue.one.len() ==> (#[trigger] queue.one[i]).domain() == domain)
}

/// Retries preserve both queues. Exactly one matching-domain payload is
/// appended under the revalidated current generation's queue lock.
#[verifier::exec_allows_no_decreases_clause]
pub fn register_retirement<T>(queue: &mut Registration<RetiredNode<T>>, entry: RetiredNode<T>,
    Ghost(domain): Ghost<Set<vstd::tokens::InstanceId>>) -> (generation: bool)
    requires old(queue).held.is_none() && !old(queue).both_held, old(queue).registered.is_none(),
        old(queue).cursor <= old(queue).samples.len(),
        queue_domain(old(queue), domain), entry.domain() == domain,
    ensures queue_domain(final(queue), domain), generation == final(queue).current,
        final(queue).held.is_none(), final(queue).registered == Some(generation),
        final(queue).zero@ == if generation { old(queue).zero@ } else { old(queue).zero@.push(entry) },
        final(queue).one@ == if generation { old(queue).one@.push(entry) } else { old(queue).one@ },
{ shared_registration(queue, entry) }

/// Queue removal alone grants no memory access: recovery additionally needs
/// the zero-pin/zero-observation ledgers of this exact node.
pub fn recover_retired<T>(entry: RetiredNode<T>,
    Tracked(node): Tracked<&cache_pins::Instance<HeapPermission<T>>>,
    Tracked(allocation): Tracked<&mut cache_pins::allocation<HeapPermission<T>>>,
    Tracked(count): Tracked<&cache_pins::count<HeapPermission<T>>>,
    Tracked(observing): Tracked<&cache_pins::observing<HeapPermission<T>>>,
    Tracked(retiring): Tracked<&cache_pins::retiring<HeapPermission<T>>>,
) -> (memory: Tracked<HeapPermission<T>>)
    requires entry.node_id() == node.id(), old(allocation).instance_id() == node.id(),
        old(allocation).value() == Some(entry.memory()), count.instance_id() == node.id(),
        observing.instance_id() == node.id(), retiring.instance_id() == node.id(),
        count.value() == 0, observing.value() == 0, retiring.value(),
    ensures memory@ == entry.memory(), memory@.ptr() == entry.pointer(),
        final(allocation).instance_id() == node.id(), final(allocation).value().is_none(),
{
    proof { use_type_invariant(&entry); }
    let tracked memory = recover_memory(node, allocation, count, observing, retiring, entry.ticket.get());
    Tracked(memory)
}
}


macro_rules! release_to_entry {
    ($module:ident, $word:ty) => {
    pub mod $module {
    use super::*;
    use super::super::pin_ownership::$module as pins;
    use super::super::pin_transitions::Release;
    verus! {
    /// Final shared pin release freezes the exact node's observation ledger.
    pub fn release_covered<T>(previous: $word, pointer: *mut T,
        Tracked(node): Tracked<&cache_pins::Instance<HeapPermission<T>>>,
        Tracked(count): Tracked<&mut cache_pins::count<HeapPermission<T>>>,
        Tracked(retiring): Tracked<&mut cache_pins::retiring<HeapPermission<T>>>,
        Tracked(pin): Tracked<cache_pins::pins<HeapPermission<T>>>,
        Tracked(observations): Tracked<&mut super::super::observation_coverage::ObservationLedger<'_, T>>,
    ) -> (entry: Option<RetiredNode<T>>)
        requires old(count).instance_id() == node.id(), old(count).value() == previous as nat,
            old(retiring).instance_id() == node.id(), pin.instance_id() == node.id(),
            pin.element().0.ptr() == pointer, pin.element().0.is_init(),
            old(observations).inv(), old(observations).node_id() == node.id(),
            old(observations).domain() == node.domain(),
        ensures previous > 0, final(count).instance_id() == node.id(), final(count).value() + 1 == previous as nat,
            final(retiring).instance_id() == node.id(), entry.is_some() == (previous == 1),
            final(observations).inv(), final(observations).node_id() == node.id(),
            final(observations).domain() == node.domain(), final(observations).coverage() == old(observations).coverage(),
            final(observations).len() == old(observations).len(),
            entry.is_some() ==> final(observations).frozen()
                && entry.unwrap().node_id() == node.id() && entry.unwrap().domain() == node.domain()
                && entry.unwrap().pointer() == pointer,
            entry.is_none() ==> final(observations).frozen() == old(observations).frozen(),
    {
        let entry = release_to_entry(previous, pointer, Tracked(node), Tracked(count), Tracked(retiring), Tracked(pin));
        match entry {
            Some(entry) => {
                proof { observations.freeze(&entry); }
                Some(entry)
            },
            None => None,
        }
    }

    /// Production ReclaimEntry::release_pin creates a queue entry only for
    /// the final release. Use the same width-specific kernel and its token.
    pub fn release_to_entry<T>(previous: $word, pointer: *mut T,
        Tracked(node): Tracked<&cache_pins::Instance<HeapPermission<T>>>,
        Tracked(count): Tracked<&mut cache_pins::count<HeapPermission<T>>>,
        Tracked(retiring): Tracked<&mut cache_pins::retiring<HeapPermission<T>>>,
        Tracked(pin): Tracked<cache_pins::pins<HeapPermission<T>>>,
    ) -> (entry: Option<RetiredNode<T>>)
        requires old(count).instance_id() == node.id(), old(count).value() == previous as nat,
            old(retiring).instance_id() == node.id(), pin.instance_id() == node.id(),
            pin.element().0.ptr() == pointer, pin.element().0.is_init(),
        ensures previous > 0, final(count).instance_id() == node.id(),
            final(count).value() + 1 == previous as nat,
            final(retiring).instance_id() == node.id(),
            entry.is_some() == (previous == 1),
            entry.is_some() ==> entry.unwrap().node_id() == node.id()
                && entry.unwrap().pointer() == pointer && entry.unwrap().domain() == node.domain(),
    {
        let (_next, outcome, Tracked(ticket)) = pins::release_owned::<T>(previous,
            Tracked(node), Tracked(count), Tracked(retiring), Tracked(pin));
        match outcome {
            Release::LastPin => {
                let tracked ticket = ticket.tracked_unwrap();
                Some(RetiredNode::<T>::from_ticket(pointer, Tracked(node), Tracked(ticket)))
            },
            _ => None,
        }
    }
    }
    }
    };
}
release_to_entry!(word32, u32);
release_to_entry!(word64, u64);


macro_rules! detach_retired {
    ($function:ident, $terminal:ident, $locked:ident, $width:ident) => {
    verus! {
    pub fn $locked<T>(queue: &mut Registration<RetiredNode<T>>,
        lock: &super::rotation::lock_ownership::$width::TransitionLock,
        Ghost(domain): Ghost<Set<vstd::tokens::InstanceId>>,
    ) -> (result: Option<(usize, Vec<RetiredNode<T>>)>)
        requires old(queue).held.is_none(), !old(queue).both_held, queue_domain(old(queue), domain),
        ensures queue_domain(final(queue), domain), final(queue).held.is_none(), !final(queue).both_held,
            result.is_some() ==> result.unwrap().0 < 2
                && old(queue).owner.addr() == lock.pred().domain.addr()
                && (forall|i: int| 0 <= i < result.unwrap().1.len() ==>
                    (#[trigger] result.unwrap().1[i]).domain() == domain),
    { super::rotation::lock_ownership::$width::try_detach_pending(lock, queue) }

    pub fn $terminal<T>(queue: &mut Registration<RetiredNode<T>>,
        certificate: &super::rotation::identity::$width::Closed<'_>,
        Ghost(domain): Ghost<Set<vstd::tokens::InstanceId>>,
    ) -> (entries: Option<[Vec<RetiredNode<T>>; 2]>)
        requires certificate.inv(), old(queue).held.is_none(), !old(queue).both_held,
            queue_domain(old(queue), domain),
        ensures queue_domain(final(queue), domain), final(queue).held.is_none(), !final(queue).both_held,
            entries.is_some() == (old(queue).owner.addr() == certificate.state().domain.addr()),
            entries.is_some() ==> (
                (forall|i: int| 0 <= i < entries.unwrap()[0].len() ==>
                    (#[trigger] entries.unwrap()[0][i]).domain() == domain)
                && (forall|i: int| 0 <= i < entries.unwrap()[1].len() ==>
                    (#[trigger] entries.unwrap()[1][i]).domain() == domain)),
    { super::rotation::detachment::$width::detach_terminal(queue, certificate) }

    /// The authorized generation's entire linear payload batch is moved out;
    /// neither detachment nor its certificate alone supplies zero observations.
    pub fn $function<T>(queue: &mut Registration<RetiredNode<T>>,
        certificate: &super::rotation::identity::$width::Drained<'_>,
        Ghost(domain): Ghost<Set<vstd::tokens::InstanceId>>,
    ) -> (entries: Option<Vec<RetiredNode<T>>>)
        requires certificate.inv(), old(queue).held.is_none() && !old(queue).both_held, queue_domain(old(queue), domain),
        ensures queue_domain(final(queue), domain), final(queue).held.is_none(),
            entries.is_some() == (old(queue).owner.addr() == certificate.state().domain.addr()),
            entries.is_some() ==> forall|i: int| 0 <= i < entries.unwrap().len() ==>
                (#[trigger] entries.unwrap()[i]).domain() == domain,
    { super::rotation::detachment::$width::detach(queue, certificate) }
    }
    };
}
detach_retired!(detach_retired_32, detach_terminal_retired_32, detach_under_lock_32, word32);
detach_retired!(detach_retired_64, detach_terminal_retired_64, detach_under_lock_64, word64);


macro_rules! recover_after_covered_drain {
    ($function:ident, $pending:ident, $width:ident, $word:ty) => {
    verus! {
    /// The earlier narrowing survives gate reuse: only pending must be idle.
    pub fn $pending<T>(entry: RetiredNode<T>,
        Tracked(node): Tracked<&cache_pins::Instance<HeapPermission<T>>>,
        Tracked(allocation): Tracked<&mut cache_pins::allocation<HeapPermission<T>>>,
        Tracked(count): Tracked<&cache_pins::count<HeapPermission<T>>>,
        Tracked(observations): Tracked<&super::observation_coverage::ObservationLedger<'_, T>>,
        Tracked(retiring): Tracked<&cache_pins::retiring<HeapPermission<T>>>,
        rotation: &super::rotation::refinement::$width::Rotation,
        Tracked(ledgers): Tracked<&super::rotation::refinement::$width::GenerationLedgers>,
    ) -> (memory: Tracked<HeapPermission<T>>)
        requires entry.node_id() == node.id(), old(allocation).instance_id() == node.id(),
            old(allocation).value() == Some(entry.memory()), count.instance_id() == node.id(),
            retiring.instance_id() == node.id(), count.value() == 0, retiring.value(),
            observations.inv(), observations.node_id() == node.id(), observations.domain() == node.domain(),
            rotation.inv(), ledgers.matches(rotation), rotation.pending == Some(!rotation.current),
            rotation.idle(!rotation.current),
            observations.coverage().subset_of(Set::empty().insert(ledgers.selected_id(!rotation.current))),
        ensures memory@ == entry.memory(), memory@.ptr() == entry.pointer(),
            final(allocation).instance_id() == node.id(), final(allocation).value().is_none(),
    {
        proof { super::observation_coverage::$width::zero_after_pending(observations, rotation, ledgers); }
        let tracked observing = observations.count();
        recover_retired(entry, Tracked(node), Tracked(allocation), Tracked(count), Tracked(observing), Tracked(retiring))
    }

    /// Derive this node's zero-observation ledger from conserved live permits
    /// and sealed-zero histories covering every remaining observation gate.
    pub fn $function<T>(entry: RetiredNode<T>,
        Tracked(node): Tracked<&cache_pins::Instance<HeapPermission<T>>>,
        Tracked(allocation): Tracked<&mut cache_pins::allocation<HeapPermission<T>>>,
        Tracked(count): Tracked<&cache_pins::count<HeapPermission<T>>>,
        Tracked(observations): Tracked<&super::observation_coverage::ObservationLedger<'_, T>>,
        Tracked(retiring): Tracked<&cache_pins::retiring<HeapPermission<T>>>,
        Ghost(histories): Ghost<Seq<Seq<$word>>>,
        Tracked(ledgers): Tracked<&super::rotation::drain::stripe_ownership::$width::StripeLedgers>,
    ) -> (memory: Tracked<HeapPermission<T>>)
        requires entry.node_id() == node.id(), old(allocation).instance_id() == node.id(),
            old(allocation).value() == Some(entry.memory()), count.instance_id() == node.id(),
            retiring.instance_id() == node.id(), count.value() == 0, retiring.value(),
            observations.inv(), observations.node_id() == node.id(), observations.domain() == node.domain(),
            super::rotation::drain::stripe_ownership::$width::drained_histories(histories),
            ledgers.matches(super::rotation::drain::stripe_ownership::$width::final_states(histories)),
            super::observation_coverage::$width::covers(observations.coverage(), ledgers),
        ensures memory@ == entry.memory(), memory@.ptr() == entry.pointer(),
            final(allocation).instance_id() == node.id(), final(allocation).value().is_none(),
    {
        proof { super::observation_coverage::$width::zero_after_histories(observations, histories, ledgers); }
        let tracked observing = observations.count();
        recover_retired(entry, Tracked(node), Tracked(allocation), Tracked(count), Tracked(observing), Tracked(retiring))
    }
    }
    };
}
recover_after_covered_drain!(recover_after_covered_drain_32, recover_after_pending_32, word32, u32);
recover_after_covered_drain!(recover_after_covered_drain_64, recover_after_pending_64, word64, u64);

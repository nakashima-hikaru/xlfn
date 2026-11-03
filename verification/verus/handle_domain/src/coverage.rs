//! Conserves every Handle observation and derives empty storage from actual drain histories.
use vstd::prelude::*;
use super::heap_permission::HeapPermission;
use super::observations::{bindings, Observation, RetiredRecord};
use super::rotation::gate_permits::admission;
verus! {
pub tracked struct CoveredBinding<'scope, T> {
    pub instance: bindings::Instance<HeapPermission<T>>,
    pub published: bindings::published<HeapPermission<T>>,
    pub observations: ObservationLedger<'scope, T>,
}
pub proof fn initialize<'scope, T>(tracked memory: HeapPermission<T>,
    domain: Set<vstd::tokens::InstanceId>, owner: *const u8) -> (tracked covered: CoveredBinding<'scope, T>)
    requires memory.is_init(),
    ensures covered.instance.domain() == domain, covered.instance.owner() == owner,
        covered.published.instance_id() == covered.instance.id(), covered.published.value() == memory,
        covered.observations.inv(), covered.observations.node_id() == covered.instance.id(),
        covered.observations.domain() == domain, covered.observations.len() == 0,
        !covered.observations.frozen(), covered.observations.coverage() == domain,
{
    let tracked (Tracked(instance), Tracked(published), Tracked(retired), Tracked(entries), Tracked(count), Tracked(history))
        = bindings::Instance::allocate(memory, domain, owner, Some(memory));
    let tracked published = published.tracked_unwrap();
    let tracked observations = ObservationLedger::new(&instance, count);
    CoveredBinding { instance, published, observations }
}
pub tracked struct ObservationLedger<'scope, T> {
    ghost domain: Set<vstd::tokens::InstanceId>,
    ghost coverage: Set<vstd::tokens::InstanceId>,
    ghost frozen: bool,
    count: bindings::observing<HeapPermission<T>>,
    entries: Map<nat, Observation<'scope, T>>,
}
impl<'scope, T> ObservationLedger<'scope, T> {
    pub closed spec fn inv(&self) -> bool {
        self.count.value() == self.entries.len()
        && self.coverage.subset_of(self.domain)
        && (!self.frozen ==> self.coverage == self.domain)
        && forall|key: nat| self.entries.dom().contains(key) ==> (
            (#[trigger] self.entries[key]).node_id() == self.count.instance_id()
            && self.entries[key].domain() == self.domain
            && self.coverage.contains(self.entries[key].gate_id()))
    }
    pub closed spec fn node_id(&self) -> vstd::tokens::InstanceId { self.count.instance_id() }
    pub closed spec fn domain(&self) -> Set<vstd::tokens::InstanceId> { self.domain }
    pub closed spec fn coverage(&self) -> Set<vstd::tokens::InstanceId> { self.coverage }
    pub closed spec fn frozen(&self) -> bool { self.frozen }
    pub closed spec fn gate_id(&self, key: nat) -> vstd::tokens::InstanceId { self.entries[key].gate_id() }
    pub closed spec fn len(&self) -> nat { self.count.value() }
    pub closed spec fn contains(&self, key: nat) -> bool { self.entries.dom().contains(key) }
    pub closed spec fn memory(&self, key: nat) -> HeapPermission<T> { self.entries[key].memory() }
    pub proof fn new(tracked instance: &bindings::Instance<HeapPermission<T>>,
        tracked count: bindings::observing<HeapPermission<T>>) -> (tracked ledger: Self)
        requires count.instance_id() == instance.id(), count.value() == 0,
        ensures ledger.inv(), ledger.node_id() == instance.id(), ledger.domain() == instance.domain(), ledger.len() == 0, !ledger.frozen(), ledger.coverage() == instance.domain(),
    { ObservationLedger { domain: instance.domain(), coverage: instance.domain(), frozen: false, count, entries: Map::tracked_empty() } }
    pub proof fn observe(tracked &mut self, key: nat,
        tracked instance: &bindings::Instance<HeapPermission<T>>,
        tracked published: &bindings::published<HeapPermission<T>>,
        tracked permit: &'scope admission::permits)
        requires old(self).inv(), !old(self).frozen(), !old(self).contains(key), old(self).node_id() == instance.id(),
            old(self).domain() == instance.domain(), published.instance_id() == instance.id(),
            instance.domain().contains(permit.instance_id()),
        ensures final(self).inv(), final(self).contains(key), final(self).len() == old(self).len() + 1,
            final(self).node_id() == old(self).node_id(), final(self).domain() == old(self).domain(),
            final(self).frozen() == old(self).frozen(), final(self).coverage() == old(self).coverage(),
            final(self).memory(key) == published.value(),
    {
        let tracked observation = Observation::observe(instance, published, &mut self.count, permit);
        use_type_invariant(&observation);
        self.entries.tracked_insert(key, observation);
    }
    pub proof fn end(tracked &mut self, key: nat, tracked instance: &bindings::Instance<HeapPermission<T>>)
        requires old(self).inv(), old(self).contains(key), old(self).node_id() == instance.id(),
        ensures final(self).inv(), !final(self).contains(key), final(self).len() + 1 == old(self).len(),
            final(self).node_id() == old(self).node_id(), final(self).domain() == old(self).domain(),
            final(self).frozen() == old(self).frozen(), final(self).coverage() == old(self).coverage(),
    {
        let tracked observation = self.entries.tracked_remove(key);
        observation.end(instance, &mut self.count);
    }
    pub proof fn retire(tracked &mut self, tracked instance: &bindings::Instance<HeapPermission<T>>,
        tracked published: bindings::published<HeapPermission<T>>) -> (tracked retired: bindings::retired<HeapPermission<T>>)
        requires old(self).inv(), old(self).node_id() == instance.id(), published.instance_id() == instance.id(),
        ensures final(self).inv(), final(self).frozen(), final(self).coverage() == old(self).coverage(),
            final(self).node_id() == old(self).node_id(), final(self).domain() == old(self).domain(),
            final(self).len() == old(self).len(), retired.instance_id() == instance.id(), retired.value() == published.value(),
    {
        let tracked (Tracked(retired), Tracked(history)) = instance.retire(published.value(), published);
        self.frozen = true;
        retired
    }
    pub proof fn observation(tracked &self, key: nat) -> (tracked observation: &Observation<'scope, T>)
        requires self.inv(), self.contains(key),
        ensures observation.node_id() == self.node_id(), observation.memory() == self.memory(key),
            observation.domain() == self.domain(), observation.gate_id() == self.gate_id(key),
    { self.entries.tracked_borrow(key) }
    pub proof fn rebind_empty<'next>(tracked self) -> (tracked next: ObservationLedger<'next, T>)
        requires self.inv(), self.len() == 0,
        ensures next.inv(), next.node_id() == self.node_id(), next.domain() == self.domain(), next.len() == 0, next.coverage() == self.coverage(), next.frozen() == self.frozen(),
    { ObservationLedger { domain: self.domain, coverage: self.coverage, frozen: self.frozen, count: self.count, entries: Map::tracked_empty() } }
}
}
macro_rules! drain {
    ($module:ident, $word:ty) => {
    pub mod $module {
    use super::*;
    use super::super::rotation::drain::stripe_ownership::$module as stripes;
    verus! {
    pub open spec fn covers(domain: Set<vstd::tokens::InstanceId>, ledgers: &stripes::StripeLedgers) -> bool {
        forall|id: vstd::tokens::InstanceId| domain.contains(id) ==>
            exists|index: int| 0 <= index < ledgers.gates.len() && (#[trigger] ledgers.gates[index]).id() == id
    }
    pub open spec fn generation_domain(ledgers: &super::super::rotation::refinement::$module::GenerationLedgers)
        -> Set<vstd::tokens::InstanceId> {
        Set::empty().insert(ledgers.selected_id(false)).insert(ledgers.selected_id(true))
    }

    /// Before a new rotation, inv + no pending entails the previous generation
    /// is idle. Remove that generation from the frozen node's coverage before
    /// it can be reused for new readers.
    pub proof fn narrow_before_rotation<T>(tracked observations: &mut ObservationLedger<'_, T>,
        rotation: &super::super::rotation::refinement::$module::Rotation,
        tracked ledgers: &super::super::rotation::refinement::$module::GenerationLedgers)
        requires old(observations).inv(), old(observations).frozen(), rotation.inv(),
            rotation.pending.is_none(), ledgers.matches(rotation),
            old(observations).domain() == generation_domain(ledgers),
        ensures final(observations).inv(), final(observations).frozen(),
            final(observations).coverage() == old(observations).coverage().intersect(
                Set::empty().insert(ledgers.selected_id(rotation.current))),
            final(observations).domain() == old(observations).domain(),
            final(observations).node_id() == old(observations).node_id(),
            final(observations).len() == old(observations).len(),
    {
        reveal(ObservationLedger::inv);
        reveal(ObservationLedger::coverage);
        reveal(ObservationLedger::frozen);
        reveal(ObservationLedger::domain);
        reveal(ObservationLedger::node_id);
        reveal(ObservationLedger::len);
        let keep = Set::empty().insert(ledgers.selected_id(rotation.current));
        if exists|key: nat| observations.entries.dom().contains(key)
            && !keep.contains((#[trigger] observations.entries[key]).gate_id()) {
            let key = choose|key: nat| observations.entries.dom().contains(key)
                && !keep.contains((#[trigger] observations.entries[key]).gate_id());
            let tracked observation = observations.entries.tracked_borrow(key);
            assert(generation_domain(ledgers).contains(observation.gate_id()));
            assert(observation.gate_id() == ledgers.selected_id(!rotation.current));
            rotation.idle_excludes_generation_permit(!rotation.current, ledgers, observation.permit());
        }
        observations.coverage = observations.coverage.intersect(keep);
    }

    /// Compose narrowing with the exact production-shared begin/publication
    /// expression. Gate identities and active ledgers survive seal/reopen.
    pub fn narrow_and_begin<T>(rotation: &mut super::super::rotation::refinement::$module::Rotation,
        Tracked(observations): Tracked<&mut ObservationLedger<'_, T>>,
        Tracked(ledgers): Tracked<&super::super::rotation::refinement::$module::GenerationLedgers>)
        requires old(rotation).inv(), old(rotation).pending.is_none(), old(rotation).locked,
            old(rotation).barrier, !old(rotation).closed, ledgers.matches(old(rotation)),
            old(observations).inv(), old(observations).frozen(),
            old(observations).domain() == generation_domain(ledgers),
        ensures final(rotation).inv(), final(rotation).current == !old(rotation).current,
            final(rotation).pending == Some(old(rotation).current), ledgers.matches(final(rotation)),
            final(observations).inv(), final(observations).frozen(),
            final(observations).coverage().subset_of(Set::empty().insert(ledgers.selected_id(!final(rotation).current))),
            final(observations).domain() == old(observations).domain(),
            final(observations).node_id() == old(observations).node_id(),
            final(observations).len() == old(observations).len(),
    {
        proof { narrow_before_rotation(observations, rotation, ledgers); }
        super::super::rotation::refinement::$module::shared_begin_and_publication(rotation);
    }

    /// Only the selected pending generation must now be idle. The reopened
    /// current generation may have active readers and is not required to drain.
    pub proof fn zero_after_pending<T>(tracked observations: &ObservationLedger<'_, T>,
        rotation: &super::super::rotation::refinement::$module::Rotation,
        tracked ledgers: &super::super::rotation::refinement::$module::GenerationLedgers)
        requires observations.inv(), rotation.inv(), ledgers.matches(rotation),
            rotation.pending == Some(!rotation.current), rotation.idle(!rotation.current),
            observations.coverage().subset_of(Set::empty().insert(ledgers.selected_id(!rotation.current))),
        ensures observations.len() == 0,
    {
        reveal(ObservationLedger::inv);
        reveal(ObservationLedger::coverage);
        reveal(ObservationLedger::len);
        if observations.entries.len() > 0 {
            assert(exists|key: nat| observations.entries.dom().contains(key)) by {
                if !(exists|key: nat| observations.entries.dom().contains(key)) {
                    assert(observations.entries =~= Map::<nat, Observation<'_, T>>::empty());
                }
            }
            let key = choose|key: nat| observations.entries.dom().contains(key);
            let tracked observation = observations.entries.tracked_borrow(key);
            assert(observation.gate_id() == ledgers.selected_id(!rotation.current));
            rotation.idle_excludes_generation_permit(!rotation.current, ledgers, observation.permit());
        }
    }

    pub proof fn zero_after_histories<T>(tracked observations: &ObservationLedger<'_, T>,
        histories: Seq<Seq<$word>>, tracked ledgers: &stripes::StripeLedgers)
        requires observations.inv(), stripes::drained_histories(histories),
            ledgers.matches(stripes::final_states(histories)), covers(observations.coverage(), ledgers),
        ensures observations.len() == 0,
    {
        reveal(ObservationLedger::inv); reveal(ObservationLedger::coverage); reveal(ObservationLedger::len);
        if observations.entries.len() > 0 {
            assert(exists|key: nat| observations.entries.dom().contains(key)) by {
                if !(exists|key: nat| observations.entries.dom().contains(key)) {
                    assert(observations.entries =~= Map::<nat, Observation<'_, T>>::empty());
                }
            }
            let key = choose|key: nat| observations.entries.dom().contains(key);
            let tracked observation = observations.entries.tracked_borrow(key);
            let index = choose|index: int| #![auto] 0 <= index < ledgers.gates.len()
                && ledgers.gates[index].id() == observation.gate_id();
            stripes::histories_exclude_permit(histories, ledgers, observation.permit(), index);
        }
    }
    pub fn recover_after_pending<T>(entry: RetiredRecord<T>,
        Tracked(instance): Tracked<&bindings::Instance<HeapPermission<T>>>,
        Tracked(observations): Tracked<&ObservationLedger<'_, T>>,
        rotation: &super::super::rotation::refinement::$module::Rotation,
        Tracked(ledgers): Tracked<&super::super::rotation::refinement::$module::GenerationLedgers>)
        -> (memory: Tracked<HeapPermission<T>>)
        requires entry.node_id() == instance.id(), observations.node_id() == instance.id(), observations.inv(),
            rotation.inv(), ledgers.matches(rotation), rotation.pending == Some(!rotation.current),
            rotation.idle(!rotation.current),
            observations.coverage().subset_of(Set::empty().insert(ledgers.selected_id(!rotation.current))),
        ensures memory@ == entry.memory(),
    {
        proof { zero_after_pending(observations, rotation, ledgers); }
        super::super::observations::recover_retired(entry, Tracked(instance), Tracked(&observations.count))
    }
    pub fn recover_after_histories<T>(entry: RetiredRecord<T>,
        Tracked(instance): Tracked<&bindings::Instance<HeapPermission<T>>>,
        Tracked(observations): Tracked<&ObservationLedger<'_, T>>,
        Ghost(histories): Ghost<Seq<Seq<$word>>>, Tracked(ledgers): Tracked<&stripes::StripeLedgers>)
        -> (memory: Tracked<HeapPermission<T>>)
        requires entry.node_id() == instance.id(), observations.node_id() == instance.id(), observations.inv(),
            stripes::drained_histories(histories), ledgers.matches(stripes::final_states(histories)),
            covers(observations.coverage(), ledgers),
        ensures memory@ == entry.memory(),
    {
        proof { zero_after_histories(observations, histories, ledgers); }
        super::super::observations::recover_retired(entry, Tracked(instance), Tracked(&observations.count))
    }
    }
    }
    };
}
drain!(word32, u32);
drain!(word64, u64);

verus! {
/// Ending the final observation permits actual admission consumption, while
/// preserving the same count token for subsequent drain/recovery composition.
pub proof fn observe_end_and_release<'next, T>(tracked instance: &bindings::Instance<HeapPermission<T>>,
    tracked count: bindings::observing<HeapPermission<T>>, tracked published: &bindings::published<HeapPermission<T>>,
    tracked gate: &admission::Instance, tracked active: &mut admission::active)
    -> (tracked ledger: ObservationLedger<'next, T>)
    requires count.instance_id() == instance.id(), count.value() == 0, published.instance_id() == instance.id(),
        instance.domain().contains(gate.id()), old(active).instance_id() == gate.id(), old(active).value() == 0,
    ensures ledger.inv(), ledger.node_id() == instance.id(), ledger.len() == 0, ledger.domain() == instance.domain(),
        final(active).instance_id() == gate.id(), final(active).value() == 0,
{
    let tracked permit = gate.acquire(active);
    let tracked mut ledger = ObservationLedger::new(instance, count);
    ledger.observe(0, instance, published, &permit);
    ledger.end(0, instance);
    let tracked result = ledger.rebind_empty();
    gate.release(active, permit);
    result
}
}

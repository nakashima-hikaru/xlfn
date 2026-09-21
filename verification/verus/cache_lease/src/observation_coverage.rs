//! Conservation of all node observations, with their borrowed admission permits.
//! This resource ledger does not infer native generation cutoff coverage.
use vstd::prelude::*;
use super::heap_permission::HeapPermission;
use super::pin_ownership::{cache_pins, PinKind};
use super::scope_ownership::ScopedObservation;
use super::rotation::gate_permits::admission;
verus! {
/// Allocation exposes the observation counter only through the conserving ledger.
pub tracked struct CoveredNode<'scope, T> {
    pub instance: cache_pins::Instance<HeapPermission<T>>,
    pub allocation: cache_pins::allocation<HeapPermission<T>>,
    pub count: cache_pins::count<HeapPermission<T>>,
    pub creator: cache_pins::pins<HeapPermission<T>>,
    pub observations: ObservationLedger<'scope, T>,
    pub retiring: cache_pins::retiring<HeapPermission<T>>,
}
pub proof fn initialize_covered_node<'scope, T>(tracked memory: HeapPermission<T>,
    domain: Set<vstd::tokens::InstanceId>) -> (tracked covered: CoveredNode<'scope, T>)
    requires memory.is_init(),
    ensures covered.instance.domain() == domain,
        covered.allocation.instance_id() == covered.instance.id(), covered.allocation.value() == Some(memory),
        covered.count.instance_id() == covered.instance.id(), covered.count.value() == 1,
        covered.creator.instance_id() == covered.instance.id(), covered.creator.element() == (memory, PinKind::Creator),
        covered.observations.inv(), covered.observations.node_id() == covered.instance.id(),
        covered.observations.domain() == domain, covered.observations.len() == 0,
        covered.retiring.instance_id() == covered.instance.id(), !covered.retiring.value(),
{
    let tracked node = super::scope_ownership::initialize_node(memory, domain);
    let tracked observations = ObservationLedger::new(&node.instance, node.observing);
    CoveredNode { instance: node.instance, allocation: node.allocation, count: node.count,
        creator: node.creator, observations, retiring: node.retiring }
}

pub tracked struct ObservationLedger<'scope, T> {
    ghost domain: Set<vstd::tokens::InstanceId>,
    ghost coverage: Set<vstd::tokens::InstanceId>,
    ghost frozen: bool,
    observing: cache_pins::observing<HeapPermission<T>>,
    entries: Map<nat, ScopedObservation<'scope, T>>,
}
impl<'scope, T> ObservationLedger<'scope, T> {
    pub closed spec fn inv(&self) -> bool {
        self.observing.value() == self.entries.len()
        && self.coverage.subset_of(self.domain)
        && (!self.frozen ==> self.coverage == self.domain)
        && forall|key: nat| self.entries.dom().contains(key) ==> (
            (#[trigger] self.entries[key]).node_id() == self.observing.instance_id()
            && self.entries[key].domain() == self.domain
            && self.entries[key].inv()
            && self.coverage.contains(self.entries[key].gate_id()))
    }
    pub closed spec fn node_id(&self) -> vstd::tokens::InstanceId { self.observing.instance_id() }
    pub closed spec fn domain(&self) -> Set<vstd::tokens::InstanceId> { self.domain }
    pub closed spec fn coverage(&self) -> Set<vstd::tokens::InstanceId> { self.coverage }
    pub closed spec fn frozen(&self) -> bool { self.frozen }
    pub closed spec fn len(&self) -> nat { self.observing.value() }
    pub closed spec fn contains(&self, key: nat) -> bool { self.entries.dom().contains(key) }
    pub closed spec fn memory(&self, key: nat) -> HeapPermission<T> { self.entries[key].memory() }

    pub proof fn new(tracked node: &cache_pins::Instance<HeapPermission<T>>,
        tracked observing: cache_pins::observing<HeapPermission<T>>) -> (tracked ledger: Self)
        requires observing.instance_id() == node.id(), observing.value() == 0,
        ensures ledger.inv(), ledger.node_id() == node.id(), ledger.domain() == node.domain(), ledger.len() == 0, !ledger.frozen(), ledger.coverage() == node.domain(),
    {
        ObservationLedger { domain: node.domain(), coverage: node.domain(), frozen: false, observing, entries: Map::tracked_empty() }
    }

    pub proof fn observe(tracked &mut self, key: nat,
        tracked permit: &'scope admission::permits,
        tracked node: &cache_pins::Instance<HeapPermission<T>>,
        tracked resident: &cache_pins::pins<HeapPermission<T>>)
        requires old(self).inv(), !old(self).frozen(), !old(self).contains(key),
            old(self).node_id() == node.id(), old(self).domain() == node.domain(),
            node.domain().contains(permit.instance_id()),
            resident.instance_id() == node.id(), resident.element().1 == PinKind::Resident,
        ensures final(self).inv(), final(self).node_id() == old(self).node_id(),
            final(self).domain() == old(self).domain(), final(self).contains(key),
            final(self).len() == old(self).len() + 1,
            final(self).frozen() == old(self).frozen(), final(self).coverage() == old(self).coverage(),
    {
        let tracked observation = ScopedObservation::observe(permit, node, &mut self.observing, resident);
        observation.domain_covers_gate();
        self.entries.tracked_insert(key, observation);
    }

    pub proof fn end(tracked &mut self, key: nat,
        tracked node: &cache_pins::Instance<HeapPermission<T>>)
        requires old(self).inv(), old(self).contains(key), old(self).node_id() == node.id(),
        ensures final(self).inv(), final(self).node_id() == old(self).node_id(),
            final(self).domain() == old(self).domain(), !final(self).contains(key),
            final(self).len() + 1 == old(self).len(),
            final(self).frozen() == old(self).frozen(), final(self).coverage() == old(self).coverage(),
    {
        let tracked observation = self.entries.tracked_remove(key);
        observation.end(node, &mut self.observing);
    }

    /// The real final-pin ticket closes this ledger to future observation.
    pub proof fn freeze(tracked &mut self, entry: &super::retirement::RetiredNode<T>)
        requires old(self).inv(), old(self).node_id() == entry.node_id(), old(self).domain() == entry.domain(),
        ensures final(self).inv(), final(self).node_id() == old(self).node_id(),
            final(self).domain() == old(self).domain(), final(self).coverage() == old(self).coverage(),
            final(self).len() == old(self).len(), final(self).frozen(),
    { self.frozen = true; }

    pub proof fn observation(tracked &self, key: nat) -> (tracked observation: &ScopedObservation<'scope, T>)
        requires self.inv(), self.contains(key),
        ensures observation.node_id() == self.node_id(), observation.domain() == self.domain(),
            observation.memory() == self.memory(key),
    { self.entries.tracked_borrow(key) }

    /// Once all entries have ended, erase their old borrow lifetime while
    /// retaining the exact counter resource. Otherwise the type alone would
    /// unnecessarily keep ended permits borrowed through later recovery.
    pub proof fn rebind_empty<'next>(tracked self) -> (tracked next: ObservationLedger<'next, T>)
        requires self.inv(), self.len() == 0,
        ensures next.inv(), next.node_id() == self.node_id(), next.domain() == self.domain(),
            next.coverage() == self.coverage(), next.frozen() == self.frozen(), next.len() == 0,
    {
        ObservationLedger { domain: self.domain, coverage: self.coverage, frozen: self.frozen,
            observing: self.observing, entries: Map::tracked_empty() }
    }

    pub proof fn count(tracked &self) -> (tracked count: &cache_pins::observing<HeapPermission<T>>)
        ensures count.instance_id() == self.node_id(), count.value() == self.len(),
    { &self.observing }
}

/// Usability witness: ending the last observation really permits consuming
/// its admission, while returning the same node counter for later recovery.
pub proof fn observe_end_and_release<'next, T>(
    tracked node: &cache_pins::Instance<HeapPermission<T>>,
    tracked observing: cache_pins::observing<HeapPermission<T>>,
    tracked resident: &cache_pins::pins<HeapPermission<T>>,
    tracked gate: &admission::Instance, tracked active: &mut admission::active,
) -> (tracked ledger: ObservationLedger<'next, T>)
    requires observing.instance_id() == node.id(), observing.value() == 0,
        resident.instance_id() == node.id(), resident.element().1 == PinKind::Resident,
        node.domain().contains(gate.id()), old(active).instance_id() == gate.id(), old(active).value() == 0,
    ensures ledger.inv(), ledger.node_id() == node.id(), ledger.domain() == node.domain(), ledger.len() == 0,
        final(active).instance_id() == gate.id(), final(active).value() == 0,
{
    let tracked permit = gate.acquire(active);
    let tracked mut observations = ObservationLedger::new(node, observing);
    observations.observe(0, &permit, node, resident);
    observations.end(0, node);
    let tracked result = observations.rebind_empty();
    gate.release(active, permit);
    result
}

pub fn borrow_covered<'a, 'scope, T>(pointer: *const T,
    Tracked(node): Tracked<&'a cache_pins::Instance<HeapPermission<T>>>,
    Tracked(observations): Tracked<&'a ObservationLedger<'scope, T>>,
    Ghost(key): Ghost<nat>) -> (value: &'a T)
    requires observations.inv(), observations.contains(key), observations.node_id() == node.id(),
        observations.domain() == node.domain(), observations.memory(key).ptr() == pointer as *mut T,
        observations.memory(key).is_init(),
    ensures *value == observations.memory(key).value(),
{
    let tracked observation = observations.observation(key);
    super::scope_ownership::borrow_scoped(pointer, Tracked(node), Tracked(observation))
}
}
macro_rules! drained_coverage {
    ($module:ident, $word:ty, $exclude:ident) => {
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
            super::super::scope_ownership::$exclude(
                observation, rotation, ledgers, !rotation.current);
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
                    assert(observations.entries =~= Map::<nat, ScopedObservation<'_, T>>::empty());
                }
            }
            let key = choose|key: nat| observations.entries.dom().contains(key);
            let tracked observation = observations.entries.tracked_borrow(key);
            assert(observation.gate_id() == ledgers.selected_id(!rotation.current));
            super::super::scope_ownership::$exclude(
                observation, rotation, ledgers, !rotation.current);
        }
    }

    /// Drop already-drained gates from a frozen node's coverage without dropping
    /// any observation. Once frozen, no API can add a new entry after gate reuse.
    pub proof fn narrow_after_histories<T>(tracked observations: &mut ObservationLedger<'_, T>,
        keep: Set<vstd::tokens::InstanceId>, histories: Seq<Seq<$word>>,
        tracked ledgers: &stripes::StripeLedgers)
        requires old(observations).inv(), old(observations).frozen(),
            stripes::drained_histories(histories), ledgers.matches(stripes::final_states(histories)),
            covers(old(observations).coverage().difference(keep), ledgers),
        ensures final(observations).inv(), final(observations).frozen(),
            final(observations).coverage() == old(observations).coverage().intersect(keep),
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
        if exists|key: nat| observations.entries.dom().contains(key)
            && !keep.contains((#[trigger] observations.entries[key]).gate_id()) {
            let key = choose|key: nat| observations.entries.dom().contains(key)
                && !keep.contains((#[trigger] observations.entries[key]).gate_id());
            let tracked observation = observations.entries.tracked_borrow(key);
            assert(observations.coverage.difference(keep).contains(observation.gate_id()));
            let index = choose|index: int| #![auto] 0 <= index < ledgers.gates.len()
                && ledgers.gates[index].id() == observation.gate_id();
            super::super::scope_ownership::$module::exclude_observation_after_histories(
                histories, observation, ledgers, index);
        }
        observations.coverage = observations.coverage.intersect(keep);
    }

    /// Every nonempty entry holds a real permit covered by these sealed-zero
    /// histories. Conservation then establishes zero, rather than assuming it.
    pub proof fn zero_after_histories<T>(tracked observations: &ObservationLedger<'_, T>,
        histories: Seq<Seq<$word>>, tracked ledgers: &stripes::StripeLedgers)
        requires observations.inv(), stripes::drained_histories(histories),
            ledgers.matches(stripes::final_states(histories)), covers(observations.coverage(), ledgers),
        ensures observations.len() == 0,
    {
        reveal(ObservationLedger::inv);
        reveal(ObservationLedger::coverage);
        reveal(ObservationLedger::len);
        if observations.entries.len() > 0 {
            assert(exists|key: nat| observations.entries.dom().contains(key)) by {
                if !(exists|key: nat| observations.entries.dom().contains(key)) {
                    assert(observations.entries =~= Map::<nat, ScopedObservation<'_, T>>::empty());
                }
            }
            let key = choose|key: nat| observations.entries.dom().contains(key);
            let tracked observation = observations.entries.tracked_borrow(key);
            assert(observation.inv());
            observation.domain_covers_gate();
            assert(observations.coverage().contains(observation.gate_id()));
            let index = choose|index: int| #![auto] 0 <= index < ledgers.gates.len()
                && ledgers.gates[index].id() == observation.gate_id();
            super::super::scope_ownership::$module::exclude_observation_after_histories(
                histories, observation, ledgers, index);
        }
        assert(observations.entries.len() == 0);
    }
    }
    }
    };
}
drained_coverage!(word32, u32, excludes_drained_generation_32);
drained_coverage!(word64, u64, excludes_drained_generation_64);

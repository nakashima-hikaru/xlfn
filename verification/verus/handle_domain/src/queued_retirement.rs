//! Atomic retirement resources transported through the shared registration queue.
use vstd::prelude::*;
use super::atomic_publication::{Slot, RetiredRecord};
use super::batches::{Batch, DomainOwner};
use super::heap_permission::HeapPermission;
use super::retained_counts::CoverageBound;
use super::rotation::registration::{Registration, shared_registration};
verus! {
pub struct Retirement<'slot, 'domain, T> {
    slot: &'slot Slot<T>,
    domain: &'domain DomainOwner,
    record: RetiredRecord<T>,
    bound: Tracked<Option<CoverageBound>>,
}
impl<'slot, 'domain, T> Retirement<'slot, 'domain, T> {
    pub closed spec fn inv(&self) -> bool {
        self.slot.inv() && self.record.inv() && self.record.registry_id() == self.slot.registry_id()
        && self.record.owner() == self.domain.identity && self.slot.owner() == self.domain.identity
        && (self.bound@.is_some() ==> self.bound@.unwrap().valid(self.slot.index_registry_id(), self.record.node_id()))
    }
    pub closed spec fn domain(&self) -> &'domain DomainOwner { self.domain }
    pub closed spec fn memory(&self) -> HeapPermission<T> { self.record.memory() }
    pub closed spec fn record(&self) -> RetiredRecord<T> { self.record }
    pub closed spec fn gates(&self) -> Set<vstd::tokens::InstanceId> { self.slot.domain() }
    pub closed spec fn prepared_for(&self, gates: Set<vstd::tokens::InstanceId>) -> bool {
        self.bound@.is_some() && self.bound@.unwrap().limit() == gates
    }
    pub fn new(slot: &'slot Slot<T>, domain: &'domain DomainOwner, record: RetiredRecord<T>) -> (entry: Self)
        requires slot.inv(), record.inv(), record.registry_id() == slot.registry_id(),
            record.owner() == domain.identity, slot.owner() == domain.identity,
        ensures entry.inv(), entry.domain() == domain, entry.record() == record, entry.gates() == slot.domain(),
            forall|gates: Set<vstd::tokens::InstanceId>| !entry.prepared_for(gates),
    { Retirement { slot, domain, record, bound: Tracked(None) } }
}
pub fn prepare_drain_leases<T>(entry: &mut Retirement<'_, '_, T>, Ghost(keep): Ghost<Set<vstd::tokens::InstanceId>>,
    Tracked(idle): Tracked<&super::rotation::drain::atomic_counter::DrainSet>)
    requires old(entry).inv(), idle.inv(), idle.covers(old(entry).gates().difference(keep)),
    ensures final(entry).inv(), final(entry).domain() == old(entry).domain(), final(entry).record() == old(entry).record(),
        final(entry).gates() == old(entry).gates(), final(entry).prepared_for(keep),
{
    let bound = entry.slot.narrow_drain_leases(&entry.record, Ghost(keep), Tracked(idle));
    entry.bound = Tracked(Some(bound.get()));
}
/// Recover the same queued allocation using actual atomic drain authority.
pub fn recover_drain_leases<T>(entry: Retirement<'_, '_, T>,
    Tracked(drains): Tracked<&super::rotation::drain::atomic_counter::DrainSet>) -> (memory: Tracked<HeapPermission<T>>)
    requires entry.inv(), drains.inv(), drains.covers(entry.gates()),
    ensures memory@ == entry.memory(),
{
    let Retirement { slot, domain: _, record, bound: _ } = entry;
    slot.recover_drain_leases(record, Tracked(drains))
}
pub fn recover_prepared_drain_leases<T>(entry: Retirement<'_, '_, T>,
    Tracked(drains): Tracked<&super::rotation::drain::atomic_counter::DrainSet>) -> (memory: Tracked<HeapPermission<T>>)
    requires entry.inv(), drains.inv(), entry.prepared_for(drains.domain()),
    ensures memory@ == entry.memory(),
{
    let Retirement { slot, domain: _, record, bound } = entry;
    let tracked bound = bound.get().tracked_unwrap();
    slot.recover_bounded_drain_leases(record, Tracked(&bound), Tracked(drains))
}
/// Recovered heap permissions still require destruction; this is not completion.
pub struct RecoveredBatch<'domain, T> {
    owner: &'domain DomainOwner,
    memories: Vec<Tracked<HeapPermission<T>>>,
}
impl<'domain, T> RecoveredBatch<'domain, T> {
    pub closed spec fn owner(&self) -> &'domain DomainOwner { self.owner }
    pub closed spec fn memories(&self) -> Seq<Tracked<HeapPermission<T>>> { self.memories@ }
}
pub open spec fn valid_records<T>(records: Seq<Retirement<'_, '_, T>>, domain: &DomainOwner) -> bool {
    forall|i: int| 0 <= i < records.len() ==> (#[trigger] records[i]).inv() && records[i].domain() == domain
}
pub open spec fn records_domain<T>(records: Seq<Retirement<'_, '_, T>>, gates: Set<vstd::tokens::InstanceId>) -> bool {
    forall|i: int| 0 <= i < records.len() ==> (#[trigger] records[i]).gates() == gates
}
pub fn new_queue_lock<'slot, 'domain, T>(domain: &'domain DomainOwner, index: bool,
    records: Vec<Retirement<'slot, 'domain, T>>, Ghost(gates): Ghost<Set<vstd::tokens::InstanceId>>)
    -> (result: (super::rotation::barrier_ownership::QueueLock<Retirement<'slot, 'domain, T>>, Tracked<super::rotation::queue_preparation::phase::ready>))
    requires valid_records(records@, domain), records_domain(records@, gates),
    ensures result.1@.instance_id() == result.0.pred().preparation.id(),
        forall|entry: Retirement<'slot, 'domain, T>, bound: Set<vstd::tokens::InstanceId>| (#[trigger] (result.0.pred().prepared_inv)(entry, bound)) == entry.prepared_for(bound),
        result.0.pred().domain == domain.rotation, result.0.pred().index == index,
        forall|entry: Retirement<'slot, 'domain, T>| (#[trigger] (result.0.pred().payload_inv)(entry)) ==
            (entry.inv() && entry.domain() == domain && entry.gates() == gates),
{
    let ghost payload_inv = |entry: Retirement<'slot, 'domain, T>| entry.inv() && entry.domain() == domain && entry.gates() == gates;
    super::rotation::barrier_ownership::new_preparable(
        super::rotation::barrier_ownership::QueueContents { domain: domain.rotation, index, records }, Ghost(payload_inv), Ghost(|entry: Retirement<'slot, 'domain, T>, bound: Set<vstd::tokens::InstanceId>| entry.prepared_for(bound)))
}
pub open spec fn valid_queue<T>(queue: &Registration<Retirement<'_, '_, T>>, domain: &DomainOwner) -> bool {
    queue.owner == domain.rotation && valid_records(queue.zero@, domain) && valid_records(queue.one@, domain)
}
#[verifier::exec_allows_no_decreases_clause]
pub fn register<'slot, 'domain, T>(queue: &mut Registration<Retirement<'slot, 'domain, T>>,
    domain: &'domain DomainOwner, entry: Retirement<'slot, 'domain, T>) -> (generation: bool)
    requires valid_queue(old(queue), domain), entry.inv(), entry.domain() == domain,
        old(queue).held.is_none(), !old(queue).both_held, old(queue).registered.is_none(),
        old(queue).cursor <= old(queue).samples.len(),
    ensures valid_queue(final(queue), domain), final(queue).held.is_none(), !final(queue).both_held,
        final(queue).registered == Some(generation), generation == final(queue).current,
        final(queue).zero@ == if generation { old(queue).zero@ } else { old(queue).zero@.push(entry) },
        final(queue).one@ == if generation { old(queue).one@.push(entry) } else { old(queue).one@ },
{ shared_registration(queue, entry) }
#[verifier::exec_allows_no_decreases_clause]
pub fn register_atomic<'slot, 'domain, T>(current: &super::rotation::current_atomic::Current,
    zero: &super::rotation::barrier_ownership::QueueLock<Retirement<'slot, 'domain, T>>,
    one: &super::rotation::barrier_ownership::QueueLock<Retirement<'slot, 'domain, T>>,
    domain: &'domain DomainOwner, entry: Retirement<'slot, 'domain, T>)
    -> (receipt: super::rotation::atomic_registration::Registered<Retirement<'slot, 'domain, T>>)
    requires super::rotation::atomic_registration::compatible(current, zero, one, entry),
        current.owner() == domain.rotation, entry.inv(), entry.domain() == domain,
    ensures receipt.after() == receipt.before().push(entry),
{ super::rotation::atomic_registration::register(current, zero, one, entry) }

}
macro_rules! width {
    ($module:ident, $word:ty, $narrow:ident, $recover:ident, $recover_all:ident) => {
    pub mod $module {
    use super::*;
    use super::super::rotation::drain::stripe_ownership::$module as stripes;
    use super::super::rotation::identity::$module::{Drained, Closed};
    use super::super::rotation::striped_rotation::$module as owned_rotation;
    use super::super::rotation::drain::atomic_stripes::$module as atomic_stripes;
    use super::super::rotation::drain::atomic_counter::$module::Counter;
    verus! {
    pub fn prepare<T>(entry: &mut Retirement<'_, '_, T>, Ghost(kept_raw): Ghost<Seq<$word>>,
        Tracked(kept): Tracked<&stripes::StripeLedgers>, Ghost(idle_histories): Ghost<Seq<Seq<$word>>>,
        Tracked(idle): Tracked<&stripes::StripeLedgers>)
        requires old(entry).inv(), kept.matches(kept_raw), stripes::drained_histories(idle_histories),
            idle.matches(stripes::final_states(idle_histories)),
            old(entry).gates() == stripes::domain(kept).union(stripes::domain(idle)),
            stripes::domain(kept).disjoint(stripes::domain(idle)),
        ensures final(entry).inv(), final(entry).domain() == old(entry).domain(),
            final(entry).record() == old(entry).record(), final(entry).gates() == old(entry).gates(),
            final(entry).prepared_for(stripes::domain(kept)),
    {
        let bound = entry.slot.$narrow(&entry.record, Ghost(kept_raw), Tracked(kept), Ghost(idle_histories), Tracked(idle));
        entry.bound = Tracked(Some(bound.get()));
    }
    pub open spec fn prepared_records<T>(records: Seq<Retirement<'_, '_, T>>, gates: Set<vstd::tokens::InstanceId>) -> bool {
        forall|i: int| 0 <= i < records.len() ==> (#[trigger] records[i]).prepared_for(gates)
    }
    pub fn prepare_records<T>(records: &mut Vec<Retirement<'_, '_, T>>, domain: &DomainOwner,
        Ghost(kept_raw): Ghost<Seq<$word>>, Tracked(kept): Tracked<&stripes::StripeLedgers>,
        Ghost(idle_histories): Ghost<Seq<Seq<$word>>>, Tracked(idle): Tracked<&stripes::StripeLedgers>)
        requires valid_records(old(records)@, domain), kept.matches(kept_raw),
            stripes::drained_histories(idle_histories), idle.matches(stripes::final_states(idle_histories)),
            stripes::domain(kept).disjoint(stripes::domain(idle)),
            forall|i: int| 0 <= i < old(records).len() ==> (#[trigger] old(records)@[i]).gates()
                == stripes::domain(kept).union(stripes::domain(idle)),
        ensures valid_records(final(records)@, domain), final(records).len() == old(records).len(),
            records_domain(final(records)@, stripes::domain(kept).union(stripes::domain(idle))),
            prepared_records(final(records)@, stripes::domain(kept)),
            forall|i: int| 0 <= i < old(records).len() ==> (#[trigger] final(records)@[i]).record() == old(records)@[i].record(),
    {
        let mut index = 0;
        while index < records.len()
            invariant index <= records.len(), records.len() == old(records).len(), valid_records(records@, domain),
                kept.matches(kept_raw), stripes::drained_histories(idle_histories),
                idle.matches(stripes::final_states(idle_histories)), stripes::domain(kept).disjoint(stripes::domain(idle)),
                forall|i: int| 0 <= i < records.len() ==> (#[trigger] records@[i]).gates()
                    == stripes::domain(kept).union(stripes::domain(idle)),
                forall|i: int| 0 <= i < records.len() ==> (#[trigger] records@[i]).record() == old(records)@[i].record(),
                forall|i: int| 0 <= i < index ==> (#[trigger] records@[i]).prepared_for(stripes::domain(kept)),
            decreases records.len() - index,
        {
            prepare(&mut records[index], Ghost(kept_raw), Tracked(kept), Ghost(idle_histories), Tracked(idle));
            index += 1;
        }
    }
    pub fn prepare_records_drain_leases<T>(records: &mut Vec<Retirement<'_, '_, T>>, domain: &DomainOwner,
        Ghost(gates): Ghost<Set<vstd::tokens::InstanceId>>, Ghost(keep): Ghost<Set<vstd::tokens::InstanceId>>,
        Tracked(idle): Tracked<&super::super::rotation::drain::atomic_counter::DrainSet>)
        requires valid_records(old(records)@, domain), records_domain(old(records)@, gates), idle.inv(), idle.covers(gates.difference(keep)),
        ensures valid_records(final(records)@, domain), records_domain(final(records)@, gates), prepared_records(final(records)@, keep),
            final(records).len() == old(records).len(),
            forall|i: int| 0 <= i < old(records).len() ==> (#[trigger] final(records)@[i]).record() == old(records)@[i].record(),
    {
        let mut next = 0;
        while next < records.len()
            invariant next <= records.len(), records.len() == old(records).len(), valid_records(records@, domain), records_domain(records@, gates),
                idle.inv(), idle.covers(gates.difference(keep)),
                forall|i: int| 0 <= i < records.len() ==> (#[trigger] records@[i]).record() == old(records)@[i].record(),
                forall|i: int| 0 <= i < next ==> (#[trigger] records@[i]).prepared_for(keep),
            decreases records.len() - next,
        {
            prepare_drain_leases(&mut records[next], Ghost(keep), Tracked(idle));
            next += 1;
        }
    }
    pub fn prepare_collected_records<T>(mut collection: super::super::rotation::drain::atomic_stripes::$module::Collection,
        counters: &Vec<super::super::rotation::drain::atomic_counter::$module::Counter>,
        records: &mut Vec<Retirement<'_, '_, T>>, domain: &DomainOwner,
        Ghost(gates): Ghost<Set<vstd::tokens::InstanceId>>, Ghost(keep): Ghost<Set<vstd::tokens::InstanceId>>)
        -> (result: Result<Tracked<Map<nat, super::super::rotation::drain::atomic_counter::lifecycle::control>>, super::super::rotation::drain::atomic_stripes::$module::Collection>)
        requires collection.inv(counters@), valid_records(old(records)@, domain), records_domain(old(records)@, gates),
            forall|gate: vstd::tokens::InstanceId| #[trigger] gates.difference(keep).contains(gate)
                ==> exists|i: int| 0 <= i < counters.len() && counters@[i].id() == gate,
        ensures valid_records(final(records)@, domain), records_domain(final(records)@, gates), final(records).len() == old(records).len(),
            forall|i: int| 0 <= i < old(records).len() ==> (#[trigger] final(records)@[i]).record() == old(records)@[i].record(),
            match result {
                Ok(controls) => super::super::rotation::drain::atomic_stripes::$module::controls_match(counters@, controls@)
                    && prepared_records(final(records)@, keep),
                Err(remaining) => remaining.inv(counters@) && final(records)@ == old(records)@,
            },
    {
        if !collection.poll_all(counters) { return Err(collection); }
        {
            let Tracked(idle) = collection.drains(counters);
            assert forall|gate: vstd::tokens::InstanceId| #[trigger] gates.difference(keep).contains(gate)
                implies idle.domain().contains(gate) by {
                let i = choose|i: int| 0 <= i < counters.len() && counters@[i].id() == gate;
                assert(idle.domain().contains(counters@[i].id()));
            };
            prepare_records_drain_leases(records, domain, Ghost(gates), Ghost(keep), Tracked(idle));
        }
        collection.restore_all(counters);
        Ok(collection.into_controls(counters))
    }
    /// Prepare the protected queue without releasing its publication barrier.
    pub fn prepare_collected_queue<'slot, 'domain, T>(
        collection: super::super::rotation::drain::atomic_stripes::$module::Collection,
        counters: &Vec<super::super::rotation::drain::atomic_counter::$module::Counter>,
        queue: &mut super::super::rotation::barrier_ownership::QueueState<Retirement<'slot, 'domain, T>>,
        lock: &super::super::rotation::barrier_ownership::QueueLock<Retirement<'slot, 'domain, T>>,
        handle: &super::super::rotation::barrier_ownership::QueueHandle<'_, Retirement<'slot, 'domain, T>>,
        domain: &DomainOwner, Ghost(gates): Ghost<Set<vstd::tokens::InstanceId>>,
        Ghost(keep): Ghost<Set<vstd::tokens::InstanceId>>)
        -> (result: Result<Tracked<Map<nat, super::super::rotation::drain::atomic_counter::lifecycle::control>>, super::super::rotation::drain::atomic_stripes::$module::Collection>)
        requires collection.inv(counters@), handle.rwlock() == *lock,
            lock.inv(*old(queue)), old(queue).preparation@.value().is_none(),
            old(queue).domain == domain.rotation,
            forall|entry: Retirement<'slot, 'domain, T>| (#[trigger] (lock.pred().payload_inv)(entry)) ==
                (entry.inv() && entry.domain() == domain && entry.gates() == gates),
            forall|gate: vstd::tokens::InstanceId| #[trigger] gates.difference(keep).contains(gate)
                ==> exists|i: int| #![auto] 0 <= i < counters.len() && counters@[i].id() == gate,
        ensures lock.inv(*final(queue)), final(queue).domain == old(queue).domain,
            final(queue).index == old(queue).index, final(queue).preparation == old(queue).preparation,
            final(queue).instance == old(queue).instance,
            final(queue).records.len() == old(queue).records.len(),
            forall|i: int| 0 <= i < old(queue).records.len()
                ==> (#[trigger] final(queue).records@[i]).record() == old(queue).records@[i].record(),
            match result {
                Ok(controls) => super::super::rotation::drain::atomic_stripes::$module::controls_match(counters@, controls@)
                    && prepared_records(final(queue).records@, keep),
                Err(remaining) => remaining.inv(counters@) && final(queue).records@ == old(queue).records@,
            },
    {
        let _barrier = super::super::rotation::barrier_ownership::HeldBarrier::issue(queue, lock, handle);
        prepare_collected_records(collection, counters, &mut queue.records, domain, Ghost(gates), Ghost(keep))
    }
    /// Keep the queue handle borrowed through recheck, actual drain preparation
    /// and atomic reservation. On retry, return either partial collection or
    /// restored controllers together with the still-owned queue state.
    pub fn prepare_reserve_collected<'slot, 'domain, T>(
        current: &super::super::rotation::current_atomic::Current,
        mut queue: super::super::rotation::barrier_ownership::QueueState<Retirement<'slot, 'domain, T>>,
        lock: &super::super::rotation::barrier_ownership::QueueLock<Retirement<'slot, 'domain, T>>,
        handle: &super::super::rotation::barrier_ownership::QueueHandle<'_, Retirement<'slot, 'domain, T>>,
        collection: super::super::rotation::drain::atomic_stripes::$module::Collection,
        counters: &Vec<super::super::rotation::drain::atomic_counter::$module::Counter>,
        domain: &DomainOwner, Ghost(gates): Ghost<Set<vstd::tokens::InstanceId>>,
        Ghost(keep): Ghost<Set<vstd::tokens::InstanceId>>)
        -> (result: (
            Result<super::super::rotation::current_atomic::ReservedQueue<Retirement<'slot, 'domain, T>>,
                super::super::rotation::barrier_ownership::QueueState<Retirement<'slot, 'domain, T>>>,
            Result<Tracked<Map<nat, super::super::rotation::drain::atomic_counter::lifecycle::control>>,
                super::super::rotation::drain::atomic_stripes::$module::Collection>))
        requires current.inv(), current.owner() == domain.rotation, queue.domain == domain.rotation,
            lock.inv(queue), handle.rwlock() == *lock, collection.inv(counters@),
            lock.pred().preparation.id() == current.gate(queue.index),
            forall|entry: Retirement<'slot, 'domain, T>| (#[trigger] (lock.pred().payload_inv)(entry)) ==
                (entry.inv() && entry.domain() == domain && entry.gates() == gates),
            forall|entry: Retirement<'slot, 'domain, T>, bound: Set<vstd::tokens::InstanceId>|
                (#[trigger] (lock.pred().prepared_inv)(entry, bound)) == entry.prepared_for(bound),
            forall|gate: vstd::tokens::InstanceId| #[trigger] gates.difference(keep).contains(gate)
                ==> exists|i: int| #![auto] 0 <= i < counters.len() && counters@[i].id() == gate,
        ensures match result.0 {
                Ok(reserved) => result.1.is_ok() && reserved.inv(current, lock)
                    && reserved.index() == queue.index && reserved.bound() == keep
                    && reserved.records().len() == queue.records.len()
                    && (forall|i: int| 0 <= i < queue.records.len()
                        ==> (#[trigger] reserved.records()[i]).record() == queue.records@[i].record()),
                Err(returned) => lock.inv(returned) && returned.domain == queue.domain && returned.index == queue.index
                    && returned.preparation == queue.preparation && returned.instance == queue.instance
                    && returned.records.len() == queue.records.len()
                    && (forall|i: int| 0 <= i < queue.records.len()
                        ==> (#[trigger] returned.records@[i]).record() == queue.records@[i].record())
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
        let preparation = prepare_collected_queue(collection, counters, &mut queue, lock, handle, domain, Ghost(gates), Ghost(keep));
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
    pub fn prepare_publish_striped<'a, 'slot, 'domain, T>(
        current: &super::super::rotation::current_atomic::Current, state: owned_rotation::State,
        transition: &'a owned_rotation::TransitionLock, transition_handle: &'a owned_rotation::TransitionHandle<'a>,
        zero: &Vec<Counter>, one: &Vec<Counter>,
        lock: &super::super::rotation::barrier_ownership::QueueLock<Retirement<'slot, 'domain, T>>,
        domain: &DomainOwner, Tracked(next_ready): Tracked<super::super::rotation::queue_preparation::phase::ready>)
        -> (result: StripedAttempt<'a>)
        requires transition.inv(state), transition_handle.rwlock() == *transition, state.pending().is_none(),
            state.sealed(transition.pred().counters(!lock.pred().index), !lock.pred().index),
            transition.pred().zero == zero@, transition.pred().one == one@, transition.pred().domain == domain.rotation,
            current.inv(), current.owner() == domain.rotation, lock.pred().domain == domain.rotation,
            lock.pred().preparation.id() == current.gate(lock.pred().index), next_ready.instance_id() == current.gate(!lock.pred().index),
            forall|entry: Retirement<'slot, 'domain, T>, bound: Set<vstd::tokens::InstanceId>| (#[trigger] (lock.pred().prepared_inv)(entry, bound)) == entry.prepared_for(bound),
            forall|entry: Retirement<'slot, 'domain, T>| (#[trigger] (lock.pred().payload_inv)(entry)) ==
                (entry.inv() && entry.domain() == domain && entry.gates() == atomic_stripes::gate_ids(zero@).union(atomic_stripes::gate_ids(one@))),
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
            zero, one, lock, domain, Tracked(next_ready))
    }
    #[verifier::exec_allows_no_decreases_clause]
    pub fn resume_publish_striped<'a, 'slot, 'domain, T>(
        current: &super::super::rotation::current_atomic::Current,
        handoff: owned_rotation::CollectionHandoff<'a>, collection: atomic_stripes::Collection,
        queue: super::super::rotation::barrier_ownership::QueueState<Retirement<'slot, 'domain, T>>,
        handle: super::super::rotation::barrier_ownership::QueueHandle<'_, Retirement<'slot, 'domain, T>>,
        transition: &'a owned_rotation::TransitionLock, transition_handle: &'a owned_rotation::TransitionHandle<'a>,
        zero: &Vec<Counter>, one: &Vec<Counter>,
        lock: &super::super::rotation::barrier_ownership::QueueLock<Retirement<'slot, 'domain, T>>,
        domain: &DomainOwner, Tracked(next_ready): Tracked<super::super::rotation::queue_preparation::phase::ready>)
        -> (result: StripedAttempt<'a>)
        requires handoff.inv(), handoff.lock() == *transition, handoff.index() == lock.pred().index,
            handoff.next() == transition.pred().counters(!lock.pred().index), collection.inv(handoff.next()),
            transition_handle.rwlock() == *transition, lock.inv(queue), handle.rwlock() == *lock,
            transition.pred().zero == zero@, transition.pred().one == one@, transition.pred().domain == domain.rotation,
            current.inv(), current.owner() == domain.rotation, lock.pred().domain == domain.rotation,
            lock.pred().preparation.id() == current.gate(lock.pred().index), next_ready.instance_id() == current.gate(!lock.pred().index),
            forall|entry: Retirement<'slot, 'domain, T>, bound: Set<vstd::tokens::InstanceId>| (#[trigger] (lock.pred().prepared_inv)(entry, bound)) == entry.prepared_for(bound),
            forall|entry: Retirement<'slot, 'domain, T>| (#[trigger] (lock.pred().payload_inv)(entry)) ==
                (entry.inv() && entry.domain() == domain && entry.gates() == atomic_stripes::gate_ids(zero@).union(atomic_stripes::gate_ids(one@))),
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
        let (reservation, collected) = prepare_reserve_collected(current, queue, lock, &handle, collection, next, domain, Ghost(gates), Ghost(keep));
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
    /// The ordinary publication barrier holds the selected OLD queue only.
    pub fn prepare_locked<T>(queue: &mut Registration<Retirement<'_, '_, T>>, domain: &DomainOwner,
        Ghost(kept_raw): Ghost<Seq<$word>>, Tracked(kept): Tracked<&stripes::StripeLedgers>,
        Ghost(idle_histories): Ghost<Seq<Seq<$word>>>, Tracked(idle): Tracked<&stripes::StripeLedgers>)
        requires valid_queue(old(queue), domain), old(queue).held == Some(old(queue).current), !old(queue).both_held,
            kept.matches(kept_raw), stripes::drained_histories(idle_histories), idle.matches(stripes::final_states(idle_histories)),
            stripes::domain(kept).disjoint(stripes::domain(idle)),
            records_domain(if old(queue).current { old(queue).one@ } else { old(queue).zero@ },
                stripes::domain(kept).union(stripes::domain(idle))),
        ensures valid_queue(final(queue), domain), final(queue).current == old(queue).current,
            final(queue).held == old(queue).held, !final(queue).both_held,
            prepared_records(if final(queue).current { final(queue).one@ } else { final(queue).zero@ }, stripes::domain(kept)),
            final(queue).zero.len() == old(queue).zero.len(), final(queue).one.len() == old(queue).one.len(),
            forall|i: int| 0 <= i < old(queue).zero.len() ==> (#[trigger] final(queue).zero@[i]).record() == old(queue).zero@[i].record(),
            forall|i: int| 0 <= i < old(queue).one.len() ==> (#[trigger] final(queue).one@[i]).record() == old(queue).one@[i].record(),
            if old(queue).current { final(queue).zero@ == old(queue).zero@ } else { final(queue).one@ == old(queue).one@ },
    {
        if queue.current { prepare_records(&mut queue.one, domain, Ghost(kept_raw), Tracked(kept), Ghost(idle_histories), Tracked(idle)); }
        else { prepare_records(&mut queue.zero, domain, Ghost(kept_raw), Tracked(kept), Ghost(idle_histories), Tracked(idle)); }
    }
    /// Prepare under the held transition, then seal/publish/reopen through the
    /// actual atomic rotation driver before releasing the queue barrier.
    #[verifier::exec_allows_no_decreases_clause]
    pub fn prepare_publish_atomic<'slot, 'domain, T>(
        current: &super::super::rotation::current_atomic::Current,
        state: &mut super::super::rotation::atomic_rotation::$module::State,
        transition: &super::super::rotation::atomic_rotation::$module::TransitionLock,
        transition_handle: &super::super::rotation::atomic_rotation::$module::TransitionHandle<'_>,
        zero: &super::super::rotation::drain::atomic_counter::$module::Counter,
        one: &super::super::rotation::drain::atomic_counter::$module::Counter,
        lock: &super::super::rotation::barrier_ownership::QueueLock<Retirement<'slot, 'domain, T>>,
        domain: &DomainOwner, Tracked(next_ready): Tracked<super::super::rotation::queue_preparation::phase::ready>,
        Ghost(kept_raw): Ghost<Seq<$word>>, Tracked(kept): Tracked<&stripes::StripeLedgers>,
        Ghost(idle_histories): Ghost<Seq<Seq<$word>>>, Tracked(idle): Tracked<&stripes::StripeLedgers>)
        -> (result: Result<(Ghost<Seq<Retirement<'slot, 'domain, T>>>, Tracked<super::super::rotation::queue_preparation::phase::prepared>), Tracked<super::super::rotation::queue_preparation::phase::ready>>)
        requires transition.inv(*old(state)), transition_handle.rwlock() == *transition,
            old(state).pending().is_none(), old(state).sealed(!lock.pred().index), transition.pred().domain == domain.rotation,
            zero.inv(), one.inv(), transition.pred().zero == zero.authority_id(), transition.pred().one == one.authority_id(),
            current.inv(), current.owner() == domain.rotation, lock.pred().domain == domain.rotation,
            lock.pred().preparation.id() == current.gate(lock.pred().index),
            next_ready.instance_id() == current.gate(!lock.pred().index),
            forall|entry: Retirement<'slot, 'domain, T>, bound: Set<vstd::tokens::InstanceId>| (#[trigger] (lock.pred().prepared_inv)(entry, bound)) == entry.prepared_for(bound),
            forall|entry: Retirement<'slot, 'domain, T>| (#[trigger] (lock.pred().payload_inv)(entry)) ==
                (entry.inv() && entry.domain() == domain && entry.gates() == stripes::domain(kept).union(stripes::domain(idle))),
            kept.matches(kept_raw), stripes::drained_histories(idle_histories), idle.matches(stripes::final_states(idle_histories)),
            stripes::domain(kept).disjoint(stripes::domain(idle)),
        ensures transition.inv(*final(state)), match result {
            Ok(published) => published.1@.instance_id() == lock.pred().preparation.id()
                && published.1@.value() == stripes::domain(kept)
                && valid_records(published.0@, domain) && prepared_records(published.0@, stripes::domain(kept))
                && final(state).pending() == Some(lock.pred().index) && final(state).sealed(lock.pred().index)
                && !final(state).sealed(!lock.pred().index),
            Err(ready) => ready@ == next_ready && *final(state) == *old(state),
        },
    {
        let (mut queue, handle) = lock.acquire_write();
        if current.recheck(&queue, lock, &handle) != queue.index {
            handle.release_write(queue);
            return Err(Tracked(next_ready));
        }
        prepare_records(&mut queue.records, domain, Ghost(kept_raw), Tracked(kept), Ghost(idle_histories), Tracked(idle));
        let ghost atomic_source = queue.records@;
        let index = queue.index;
        match current.reserve(queue, lock, &handle, Ghost(stripes::domain(kept))) {
            Ok(reserved) => {
                let ticket = super::super::rotation::atomic_rotation::$module::begin(state, transition, transition_handle,
                    current, index, zero, one, reserved, lock, handle, Tracked(next_ready));
                Ok((Ghost(atomic_source), ticket))
            },
            Err(queue) => {
                handle.release_write(queue);
                Err(Tracked(next_ready))
            },
        }
    }
    /// Prepare the very queue protected by the borrowed publication handle.
    pub fn prepare_barrier_queue<'slot, 'domain, T>(queue: &mut super::super::rotation::barrier_ownership::QueueState<Retirement<'slot, 'domain, T>>,
        lock: &super::super::rotation::barrier_ownership::QueueLock<Retirement<'slot, 'domain, T>>,
        handle: &super::super::rotation::barrier_ownership::QueueHandle<'_, Retirement<'slot, 'domain, T>>,
        model: &super::super::rotation::refinement::$module::Rotation, domain: &DomainOwner,
        Ghost(kept_raw): Ghost<Seq<$word>>, Tracked(kept): Tracked<&stripes::StripeLedgers>,
        Ghost(idle_histories): Ghost<Seq<Seq<$word>>>, Tracked(idle): Tracked<&stripes::StripeLedgers>)
        requires handle.rwlock() == *lock, lock.inv(*old(queue)), old(queue).preparation@.value().is_none(),
            forall|entry: Retirement<'slot, 'domain, T>| (#[trigger] (lock.pred().payload_inv)(entry)) ==
                (entry.inv() && entry.domain() == domain && entry.gates() == stripes::domain(kept).union(stripes::domain(idle))),
            old(queue).domain == domain.rotation, model.domain == domain.rotation,
            old(queue).index == model.current, model.inv(), model.locked, !model.closed, model.pending.is_none(),
            valid_records(old(queue).records@, domain), records_domain(old(queue).records@, stripes::domain(kept).union(stripes::domain(idle))),
            kept.matches(kept_raw), stripes::drained_histories(idle_histories), idle.matches(stripes::final_states(idle_histories)),
            stripes::domain(kept).disjoint(stripes::domain(idle)),
        ensures lock.inv(*final(queue)), final(queue).preparation == old(queue).preparation, final(queue).domain == old(queue).domain, final(queue).index == old(queue).index,
            valid_records(final(queue).records@, domain), prepared_records(final(queue).records@, stripes::domain(kept)),
            final(queue).records.len() == old(queue).records.len(),
            forall|i: int| 0 <= i < old(queue).records.len() ==> (#[trigger] final(queue).records@[i]).record() == old(queue).records@[i].record(),
    {
        prepare_records(&mut queue.records, domain, Ghost(kept_raw), Tracked(kept), Ghost(idle_histories), Tracked(idle));
    }
    pub fn prepare_and_publish<'slot, 'domain, T>(Tracked(ready): Tracked<super::super::rotation::queue_preparation::phase::ready>, queue: &mut super::super::rotation::barrier_ownership::QueueState<Retirement<'slot, 'domain, T>>,
        lock: &super::super::rotation::barrier_ownership::QueueLock<Retirement<'slot, 'domain, T>>,
        handle: &super::super::rotation::barrier_ownership::QueueHandle<'_, Retirement<'slot, 'domain, T>>,
        model: &mut super::super::rotation::refinement::$module::Rotation,
        transition: &super::super::rotation::lock_ownership::$module::TransitionLock,
        transition_handle: &super::super::rotation::lock_ownership::$module::TransitionHandle<'_>, domain: &DomainOwner,
        Ghost(kept_raw): Ghost<Seq<$word>>, Tracked(kept): Tracked<&stripes::StripeLedgers>,
        Ghost(idle_histories): Ghost<Seq<Seq<$word>>>, Tracked(idle): Tracked<&stripes::StripeLedgers>) -> (ticket: Tracked<super::super::rotation::queue_preparation::phase::prepared>)
        requires ready.instance_id() == lock.pred().preparation.id(),
            forall|entry: Retirement<'slot, 'domain, T>, bound: Set<vstd::tokens::InstanceId>| (#[trigger] (lock.pred().prepared_inv)(entry, bound)) == entry.prepared_for(bound),
            handle.rwlock() == *lock, lock.inv(*old(queue)),
            forall|entry: Retirement<'slot, 'domain, T>| (#[trigger] (lock.pred().payload_inv)(entry)) ==
                (entry.inv() && entry.domain() == domain && entry.gates() == stripes::domain(kept).union(stripes::domain(idle))),
            old(queue).domain == domain.rotation, old(model).domain == domain.rotation,
            old(queue).index == old(model).current, old(model).inv(), old(model).locked, !old(model).barrier,
            !old(model).closed, old(model).pending.is_none(),
            transition_handle.rwlock() == *transition, transition.pred().domain == old(model).domain,
            valid_records(old(queue).records@, domain), records_domain(old(queue).records@, stripes::domain(kept).union(stripes::domain(idle))),
            kept.matches(kept_raw), stripes::drained_histories(idle_histories), idle.matches(stripes::final_states(idle_histories)),
            stripes::domain(kept).disjoint(stripes::domain(idle)),
        ensures ticket@.instance_id() == lock.pred().preparation.id(), ticket@.value() == stripes::domain(kept),
            lock.inv(*final(queue)), final(queue).domain == old(queue).domain, final(queue).index == old(queue).index,
            valid_records(final(queue).records@, domain), prepared_records(final(queue).records@, stripes::domain(kept)),
            final(queue).records.len() == old(queue).records.len(),
            forall|i: int| 0 <= i < old(queue).records.len() ==> (#[trigger] final(queue).records@[i]).record() == old(queue).records@[i].record(),
            final(model).inv(), final(model).domain == old(model).domain,
            final(model).current == !old(model).current, final(model).pending == Some(final(queue).index),
            final(model).sealed(final(queue).index), final(model).locked, !final(model).barrier,
    {
        super::super::rotation::barrier_ownership::ready(lock, queue, Tracked(&ready));
        prepare_barrier_queue(queue, lock, handle, model, domain, Ghost(kept_raw), Tracked(kept), Ghost(idle_histories), Tracked(idle));
        let ticket = super::super::rotation::barrier_ownership::freeze(queue, lock, Tracked(ready), Ghost(stripes::domain(kept)));
        let barrier = super::super::rotation::barrier_ownership::HeldBarrier::issue(queue, lock, handle);
        super::super::rotation::barrier_ownership::$module::publish(model, transition, transition_handle, &barrier);
        ticket
    }
    #[verifier::exec_allows_no_decreases_clause]
    pub fn begin_prepared<'slot, 'domain, T>(Tracked(ready): Tracked<super::super::rotation::queue_preparation::phase::ready>, lock: &super::super::rotation::barrier_ownership::QueueLock<Retirement<'slot, 'domain, T>>,
        model: &mut super::super::rotation::refinement::$module::Rotation,
        transition: &super::super::rotation::lock_ownership::$module::TransitionLock,
        transition_handle: &super::super::rotation::lock_ownership::$module::TransitionHandle<'_>, domain: &DomainOwner,
        Ghost(kept_raw): Ghost<Seq<$word>>, Tracked(kept): Tracked<&stripes::StripeLedgers>,
        Ghost(idle_histories): Ghost<Seq<Seq<$word>>>, Tracked(idle): Tracked<&stripes::StripeLedgers>)
        -> (result: (Ghost<Seq<Retirement<'slot, 'domain, T>>>, Tracked<super::super::rotation::queue_preparation::phase::prepared>))
        requires ready.instance_id() == lock.pred().preparation.id(),
            forall|entry: Retirement<'slot, 'domain, T>, bound: Set<vstd::tokens::InstanceId>| (#[trigger] (lock.pred().prepared_inv)(entry, bound)) == entry.prepared_for(bound),
            old(model).inv(), old(model).locked, !old(model).barrier, !old(model).closed, old(model).pending.is_none(),
            old(model).domain == domain.rotation, lock.pred().domain == domain.rotation, lock.pred().index == old(model).current,
            transition_handle.rwlock() == *transition, transition.pred().domain == old(model).domain,
            forall|entry: Retirement<'slot, 'domain, T>| (#[trigger] (lock.pred().payload_inv)(entry)) ==
                (entry.inv() && entry.domain() == domain && entry.gates() == stripes::domain(kept).union(stripes::domain(idle))),
            kept.matches(kept_raw), stripes::drained_histories(idle_histories), idle.matches(stripes::final_states(idle_histories)),
            stripes::domain(kept).disjoint(stripes::domain(idle)),
        ensures result.1@.instance_id() == lock.pred().preparation.id(), result.1@.value() == stripes::domain(kept),
            final(model).inv(), final(model).domain == old(model).domain,
            final(model).current == !old(model).current, final(model).pending == Some(lock.pred().index),
            final(model).sealed(lock.pred().index), final(model).locked, !final(model).barrier,
            valid_records(result.0@, domain), prepared_records(result.0@, stripes::domain(kept)),
    {
        let (mut queue, handle) = lock.acquire_write();
        let ghost original = queue.records@;
        let ticket = prepare_and_publish(Tracked(ready), &mut queue, lock, &handle, model, transition, transition_handle, domain,
            Ghost(kept_raw), Tracked(kept), Ghost(idle_histories), Tracked(idle));
        let ghost prepared = queue.records@;
        assert(prepared.len() == original.len());
        assert forall|i: int| 0 <= i < original.len() implies (#[trigger] prepared[i]).record() == original[i].record() by {};
        handle.release_write(queue);
        (Ghost(prepared), ticket)
    }
    pub fn recover<T>(entry: Retirement<'_, '_, T>, Ghost(histories): Ghost<Seq<Seq<$word>>>,
        Tracked(pending): Tracked<&stripes::StripeLedgers>) -> (memory: Tracked<HeapPermission<T>>)
        requires entry.inv(), entry.prepared_for(stripes::domain(pending)),
            stripes::drained_histories(histories), pending.matches(stripes::final_states(histories)),
        ensures memory@ == entry.memory(),
    {
        let Retirement { slot, domain: _, record, bound } = entry;
        let tracked bound = bound.get().tracked_unwrap();
        slot.$recover(record, Tracked(&bound), Ghost(histories), Tracked(pending))
    }
    pub fn recover_all<T>(entry: Retirement<'_, '_, T>, Ghost(histories): Ghost<Seq<Seq<$word>>>,
        Tracked(ledgers): Tracked<&stripes::StripeLedgers>) -> (memory: Tracked<HeapPermission<T>>)
        requires entry.inv(), stripes::drained_histories(histories), ledgers.matches(stripes::final_states(histories)),
            super::super::coverage::$module::covers(entry.gates(), ledgers),
        ensures memory@ == entry.memory(),
    {
        let Retirement { slot, domain: _, record, bound: _ } = entry;
        slot.$recover_all(record, Ghost(histories), Tracked(ledgers))
    }
    pub fn recover_all_batch<'slot, 'domain, T>(batch: Batch<'domain, Retirement<'slot, 'domain, T>>,
        Ghost(gates): Ghost<Set<vstd::tokens::InstanceId>>, Ghost(histories): Ghost<Seq<Seq<$word>>>,
        Tracked(ledgers): Tracked<&stripes::StripeLedgers>) -> (recovered: RecoveredBatch<'domain, T>)
        requires valid_records(batch.records(), batch.owner()), records_domain(batch.records(), gates),
            stripes::drained_histories(histories), ledgers.matches(stripes::final_states(histories)),
            super::super::coverage::$module::covers(gates, ledgers),
        ensures recovered.owner() == batch.owner(), recovered.memories().len() == batch.records().len(),
            forall|i: int| 0 <= i < batch.records().len() ==> (#[trigger] recovered.memories()[i])@
                == batch.records()[batch.records().len() - 1 - i].memory(),
    {
        let (owner, mut records) = batch.into_parts();
        let mut memories: Vec<Tracked<HeapPermission<T>>> = Vec::new();
        while !records.is_empty()
            invariant owner == batch.owner(), records@ == batch.records().take(records.len() as int),
                records.len() + memories.len() == batch.records().len(), valid_records(records@, owner), records_domain(records@, gates),
                stripes::drained_histories(histories), ledgers.matches(stripes::final_states(histories)),
                super::super::coverage::$module::covers(gates, ledgers),
                forall|i: int| 0 <= i < memories.len() ==> (#[trigger] memories@[i])@
                    == batch.records()[batch.records().len() - 1 - i].memory(),
            decreases records.len(),
        {
            let entry = records.pop().unwrap();
            memories.push(recover_all(entry, Ghost(histories), Tracked(ledgers)));
        }
        RecoveredBatch { owner, memories }
    }
    #[verifier::exec_allows_no_decreases_clause]
    pub fn terminal_recover_locked<'slot, 'domain, T>(
        zero: &super::super::rotation::barrier_ownership::QueueLock<Retirement<'slot, 'domain, T>>,
        one: &super::super::rotation::barrier_ownership::QueueLock<Retirement<'slot, 'domain, T>>,
        domain: &'domain DomainOwner, certificate: &Closed<'_>, Ghost(gates): Ghost<Set<vstd::tokens::InstanceId>>,
        Ghost(histories): Ghost<Seq<Seq<$word>>>, Tracked(ledgers): Tracked<&stripes::StripeLedgers>)
        -> (recovered: Option<[RecoveredBatch<'domain, T>; 2]>)
        requires certificate.inv(), zero.pred().domain == domain.rotation, one.pred().domain == domain.rotation,
            !zero.pred().index, one.pred().index,
            forall|entry: Retirement<'slot, 'domain, T>| (#[trigger] (zero.pred().payload_inv)(entry)) ==
                (entry.inv() && entry.domain() == domain && entry.gates() == gates),
            forall|entry: Retirement<'slot, 'domain, T>| (#[trigger] (one.pred().payload_inv)(entry)) ==
                (entry.inv() && entry.domain() == domain && entry.gates() == gates),
            stripes::drained_histories(histories), ledgers.matches(stripes::final_states(histories)),
            super::super::coverage::$module::covers(gates, ledgers),
        ensures recovered.is_some() == (domain.rotation.addr() == certificate.state().domain.addr()),
            recovered.is_some() ==> recovered.unwrap()[0].owner() == domain && recovered.unwrap()[1].owner() == domain,
    {
        match super::super::batches::$module::terminal_locked(zero, one, domain, certificate) {
            Some((first, second)) => {
                Some([recover_all_batch(first.into_batch(), Ghost(gates), Ghost(histories), Tracked(ledgers)),
                    recover_all_batch(second.into_batch(), Ghost(gates), Ghost(histories), Tracked(ledgers))])
            },
            None => None,
        }
    }
    #[verifier::exec_allows_no_decreases_clause]
    pub fn recover_prepared_locked<'slot, 'domain, T>(
        lock: &super::super::rotation::barrier_ownership::QueueLock<Retirement<'slot, 'domain, T>>,
        domain: &'domain DomainOwner, certificate: &Drained<'_>,
        Tracked(prepared): Tracked<super::super::rotation::queue_preparation::phase::prepared>,
        Ghost(histories): Ghost<Seq<Seq<$word>>>, Tracked(pending): Tracked<&stripes::StripeLedgers>)
        -> (result: Result<(RecoveredBatch<'domain, T>, Tracked<super::super::rotation::queue_preparation::phase::ready>),
            Tracked<super::super::rotation::queue_preparation::phase::prepared>>)
        requires certificate.inv(), lock.pred().domain == domain.rotation, lock.pred().index == (certificate.index() == 1),
            prepared.instance_id() == lock.pred().preparation.id(), prepared.value() == stripes::domain(pending),
            forall|entry: Retirement<'slot, 'domain, T>| (#[trigger] (lock.pred().payload_inv)(entry)) ==> entry.inv() && entry.domain() == domain,
            forall|entry: Retirement<'slot, 'domain, T>, bound: Set<vstd::tokens::InstanceId>| (#[trigger] (lock.pred().prepared_inv)(entry, bound)) == entry.prepared_for(bound),
            stripes::drained_histories(histories), pending.matches(stripes::final_states(histories)),
        ensures result.is_ok() == (domain.rotation.addr() == certificate.state().domain.addr()),
            match result { Err(token) => token@ == prepared, _ => true },
            result.is_ok() ==> result.unwrap().0.owner() == domain && result.unwrap().1@.instance_id() == lock.pred().preparation.id(),
    {
        match super::super::rotation::locked_detachment::$module::detach_prepared(lock, domain.rotation, certificate, Tracked(prepared)) {
            Ok((withdrawal, ready)) => {
                let batch = super::super::batches::bind_withdrawal(domain, withdrawal);
                let recovered = recover_batch(batch.into_batch(), Ghost(histories), Tracked(pending));
                Ok((recovered, ready))
            },
            Err(ticket) => Err(ticket),
        }
    }
    pub fn recover_batch<'slot, 'domain, T>(batch: Batch<'domain, Retirement<'slot, 'domain, T>>,
        Ghost(histories): Ghost<Seq<Seq<$word>>>, Tracked(pending): Tracked<&stripes::StripeLedgers>)
        -> (recovered: RecoveredBatch<'domain, T>)
        requires valid_records(batch.records(), batch.owner()), prepared_records(batch.records(), stripes::domain(pending)),
            stripes::drained_histories(histories), pending.matches(stripes::final_states(histories)),
        ensures recovered.owner() == batch.owner(), recovered.memories().len() == batch.records().len(),
            forall|i: int| 0 <= i < batch.records().len() ==> (#[trigger] recovered.memories()[i])@
                == batch.records()[batch.records().len() - 1 - i].memory(),
    {
        let (owner, mut records) = batch.into_parts();
        let mut memories: Vec<Tracked<HeapPermission<T>>> = Vec::new();
        while records.len() > 0
            invariant owner == batch.owner(), records@ == batch.records().take(records.len() as int),
                records.len() + memories.len() == batch.records().len(),
                valid_records(records@, owner), prepared_records(records@, stripes::domain(pending)),
                stripes::drained_histories(histories), pending.matches(stripes::final_states(histories)),
                forall|i: int| 0 <= i < memories.len() ==> (#[trigger] memories@[i])@
                    == batch.records()[batch.records().len() - 1 - i].memory(),
            decreases records.len(),
        {
            let entry = records.pop().unwrap();
            let memory = recover(entry, Ghost(histories), Tracked(pending));
            memories.push(memory);
        }
        RecoveredBatch { owner, memories }
    }
    /// Recover exact pending allocations and clear pending only after the
    /// shared callback completes with matching empty-queue authority.
    /// Recover the exact old queue from an idle publication while the
    /// transition handle and zero-count stripe leases remain held.
    pub fn recover_idle_published<'a, 'b, 'slot, 'domain, T>(
        published: owned_rotation::IdlePublished<'a, 'b, Retirement<'slot, 'domain, T>>,
        counters: &Vec<Counter>, domain: &'domain DomainOwner)
        -> (result: (owned_rotation::IdleDetached<'a, 'b, Retirement<'slot, 'domain, T>>,
            RecoveredBatch<'domain, T>, Ghost<Seq<Retirement<'slot, 'domain, T>>>))
        requires published.inv(), counters@ == published.target(), published.owner() == domain.rotation,
            forall|entry: Retirement<'slot, 'domain, T>| (#[trigger] (published.payload_inv())(entry))
                ==> entry.inv() && entry.domain() == domain,
            forall|entry: Retirement<'slot, 'domain, T>, bound: Set<vstd::tokens::InstanceId>|
                (#[trigger] (published.prepared_inv())(entry, bound)) == entry.prepared_for(bound),
        ensures result.0.inv(), result.0.owner() == domain.rotation,
            result.0.bound() == published.bound(), result.0.target() == counters@,
            result.1.owner() == domain,
            valid_records(result.2@, domain), prepared_records(result.2@, published.bound()),
            result.1.memories().len() == result.2@.len(),
            forall|i: int| 0 <= i < result.2@.len() ==>
                (#[trigger] result.1.memories()[i])@ == result.2@[result.2@.len() - 1 - i].memory(),
    {
        let ghost bound = published.bound();
        let (detached, withdrawal) = published.detach();
        let ghost source = withdrawal.source();
        assert(valid_records(source, domain));
        assert(prepared_records(source, bound));
        let (recovered, source) = recover_idle_detached(&detached, withdrawal, counters, domain);
        (detached, recovered, source)
    }
    fn recover_idle_detached<'a, 'b, 'slot, 'domain, T>(
        detached: &owned_rotation::IdleDetached<'a, 'b, Retirement<'slot, 'domain, T>>,
        withdrawal: super::super::rotation::locked_detachment::Withdrawal<Retirement<'slot, 'domain, T>>,
        counters: &Vec<Counter>, domain: &'domain DomainOwner)
        -> (result: (RecoveredBatch<'domain, T>, Ghost<Seq<Retirement<'slot, 'domain, T>>>))
        requires detached.inv(), detached.target() == counters@,
            withdrawal.inv(), withdrawal.owner() == domain.rotation,
            withdrawal.index() == detached.index(),
            valid_records(withdrawal.source(), domain),
            prepared_records(withdrawal.source(), detached.bound()),
        ensures result.1@ == withdrawal.source(), result.0.owner() == domain,
            valid_records(result.1@, domain), prepared_records(result.1@, detached.bound()),
            result.0.memories().len() == result.1@.len(),
            forall|i: int| 0 <= i < result.1@.len() ==>
                (#[trigger] result.0.memories()[i])@ == result.1@[result.1@.len() - 1 - i].memory(),
    {
        let ghost source = withdrawal.source();
        let batch = super::super::batches::bind_withdrawal(domain, withdrawal);
        let Tracked(drains) = detached.drains(counters);
        let recovered = recover_drain_batch(batch.into_batch(), Tracked(drains));
        (recovered, Ghost(source))
    }
    /// Reclaim the exact old queue inside the idle callback; sealed controls
    /// become available only after the matching heap permissions are returned.
    pub fn recover_idle_and_restore<'a, 'b, 'slot, 'domain, T>(
        published: owned_rotation::IdlePublished<'a, 'b, Retirement<'slot, 'domain, T>>,
        counters: &Vec<Counter>, domain: &'domain DomainOwner)
        -> (result: (owned_rotation::State, RecoveredBatch<'domain, T>,
            Tracked<super::super::rotation::queue_preparation::phase::ready>,
            Ghost<Seq<Retirement<'slot, 'domain, T>>>))
        requires published.inv(), counters@ == published.target(), published.owner() == domain.rotation,
            forall|entry: Retirement<'slot, 'domain, T>| (#[trigger] (published.payload_inv())(entry))
                ==> entry.inv() && entry.domain() == domain,
            forall|entry: Retirement<'slot, 'domain, T>, bound: Set<vstd::tokens::InstanceId>|
                (#[trigger] (published.prepared_inv())(entry, bound)) == entry.prepared_for(bound),
        ensures published.lock().inv(result.0), result.0.pending().is_none(),
            result.0.sealed(published.target(), published.index()),
            result.0.sealed(published.other(), !published.index()),
            result.2@.instance_id() == published.prepared_id(), result.1.owner() == domain,
            valid_records(result.3@, domain), prepared_records(result.3@, published.bound()),
            result.1.memories().len() == result.3@.len(),
            forall|i: int| 0 <= i < result.3@.len() ==>
                (#[trigger] result.1.memories()[i])@ == result.3@[result.3@.len() - 1 - i].memory(),
    {
        let ghost bound = published.bound();
        let ghost callback_post: spec_fn(Seq<Retirement<'slot, 'domain, T>>, RecoveredBatch<'domain, T>) -> bool =
            |source: Seq<Retirement<'slot, 'domain, T>>, recovered: RecoveredBatch<'domain, T>|
                recovered.owner() == domain && valid_records(source, domain)
                && prepared_records(source, bound)
                && recovered.memories().len() == source.len()
                && (forall|i: int| 0 <= i < source.len() ==>
                    (#[trigger] recovered.memories()[i])@ == source[source.len() - 1 - i].memory());
        let callback = |detached: &owned_rotation::IdleDetached<'a, 'b, Retirement<'slot, 'domain, T>>,
                        withdrawal: super::super::rotation::locked_detachment::Withdrawal<Retirement<'slot, 'domain, T>>|
            -> (value: RecoveredBatch<'domain, T>)
            requires detached.inv(), detached.target() == counters@, detached.bound() == bound,
                withdrawal.inv(), withdrawal.owner() == domain.rotation,
                withdrawal.index() == detached.index(),
                valid_records(withdrawal.source(), domain),
                prepared_records(withdrawal.source(), detached.bound()),
            ensures callback_post(withdrawal.source(), value),
        {
            let ghost source = withdrawal.source();
            let (recovered, Ghost(recovered_source)) = recover_idle_detached(detached, withdrawal, counters, domain);
            assert(recovered_source == source);
            assert(callback_post(source, recovered));
            recovered
        };
        assert forall|detached: &owned_rotation::IdleDetached<'a, 'b, Retirement<'slot, 'domain, T>>,
                      withdrawal: super::super::rotation::locked_detachment::Withdrawal<Retirement<'slot, 'domain, T>>|
            published.callback_input(detached, withdrawal) implies
                call_requires(callback, (detached, withdrawal)) by {
            if published.callback_input(detached, withdrawal) {
                assert forall|i: int| 0 <= i < withdrawal.source().len() implies
                    (#[trigger] withdrawal.source()[i]).inv()
                    && withdrawal.source()[i].domain() == domain by {
                    assert((published.payload_inv())(withdrawal.source()[i]));
                };
                assert forall|i: int| 0 <= i < withdrawal.source().len() implies
                    (#[trigger] withdrawal.source()[i]).prepared_for(detached.bound()) by {
                    assert((published.prepared_inv())(withdrawal.source()[i], bound));
                };
            }
        };
        assert forall|detached: &owned_rotation::IdleDetached<'a, 'b, Retirement<'slot, 'domain, T>>,
                      withdrawal: super::super::rotation::locked_detachment::Withdrawal<Retirement<'slot, 'domain, T>>,
                      value: RecoveredBatch<'domain, T>|
            (#[trigger] call_ensures(callback, (detached, withdrawal), value)) implies
                callback_post(withdrawal.source(), value) by {};
        let (state, ready, recovered, source) = published.run_callback(counters, callback, Ghost(callback_post));
        (state, recovered, ready, source)
    }
    pub fn recover_pending_striped<'a, 'slot, 'domain, T>(
        current: &super::super::rotation::current_atomic::Current,
        handoff: owned_rotation::PendingHandoff<'a>, mut collection: atomic_stripes::Collection, counters: &Vec<Counter>,
        lock: &super::super::rotation::barrier_ownership::QueueLock<Retirement<'slot, 'domain, T>>,
        domain: &'domain DomainOwner, Tracked(prepared): Tracked<super::super::rotation::queue_preparation::phase::prepared>)
        -> (result: Result<(owned_rotation::State, RecoveredBatch<'domain, T>,
            Tracked<super::super::rotation::queue_preparation::phase::ready>, Ghost<Seq<Retirement<'slot, 'domain, T>>>),
            (owned_rotation::PendingHandoff<'a>, atomic_stripes::Collection, Tracked<super::super::rotation::queue_preparation::phase::prepared>)>)
        requires handoff.inv(), counters@ == handoff.counters(), collection.inv(counters@),
            current.inv(), current.owner() == domain.rotation, lock.pred().preparation.id() == current.gate(handoff.index()),
            handoff.lock().pred().domain == domain.rotation, lock.pred().domain == domain.rotation, lock.pred().index == handoff.index(),
            prepared.instance_id() == lock.pred().preparation.id(), prepared.value() == atomic_stripes::gate_ids(counters@),
            forall|entry: Retirement<'slot, 'domain, T>| (#[trigger] (lock.pred().payload_inv)(entry)) ==> entry.inv() && entry.domain() == domain,
            forall|entry: Retirement<'slot, 'domain, T>, bound: Set<vstd::tokens::InstanceId>| (#[trigger] (lock.pred().prepared_inv)(entry, bound)) == entry.prepared_for(bound),
        ensures match result {
            Err((waiting, remaining, ticket)) => waiting == handoff && remaining.inv(counters@) && ticket@ == prepared,
            Ok((state, recovered, ready, source)) => handoff.lock().inv(state) && state.pending().is_none()
                && state.sealed(counters@, handoff.index()) && state.controls(!handoff.index()) == handoff.original().controls(!handoff.index())
                && ready@.instance_id() == lock.pred().preparation.id() && recovered.owner() == domain
                && valid_records(source@, domain) && prepared_records(source@, prepared.value())
                && recovered.memories().len() == source@.len()
                && (forall|i: int| 0 <= i < source@.len() ==> (#[trigger] recovered.memories()[i])@ == source@[source@.len() - 1 - i].memory()),
        },
    {
        if !collection.poll_all(counters) { return Err((handoff, collection, Tracked(prepared))); }
        let mut completed: Option<(RecoveredBatch<'domain, T>,
            Tracked<super::super::rotation::queue_preparation::phase::ready>, Ghost<Seq<Retirement<'slot, 'domain, T>>>)> = None;
        let mut finished = None;
        super::super::rotation::finish_rotation!(callback_result;
            vstd::prelude::verus_exec_expr!({
                completed = Some({
                    let Tracked(drains) = collection.drains(counters);
                    let held = lock.acquire_write();
                    let tracked mut ticket = Some(prepared);
                    let (withdrawal, ready) = super::super::rotation::locked_detachment::take_prepared(domain.rotation, held, lock, Tracked(&mut ticket));
                    let ghost source = withdrawal.source();
                    let batch = super::super::batches::bind_withdrawal(domain, withdrawal);
                    (recover_drain_batch(batch.into_batch(), Tracked(drains)), ready, Ghost(source))
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
    pub fn recover_drain_batch<'slot, 'domain, T>(batch: Batch<'domain, Retirement<'slot, 'domain, T>>,
        Tracked(pending): Tracked<&super::super::rotation::drain::atomic_counter::DrainSet>)
        -> (recovered: RecoveredBatch<'domain, T>)
        requires valid_records(batch.records(), batch.owner()), prepared_records(batch.records(), pending.domain()),
            pending.inv(),
        ensures recovered.owner() == batch.owner(), recovered.memories().len() == batch.records().len(),
            forall|i: int| 0 <= i < batch.records().len() ==> (#[trigger] recovered.memories()[i])@
                == batch.records()[batch.records().len() - 1 - i].memory(),
    {
        let (owner, mut records) = batch.into_parts();
        let mut memories: Vec<Tracked<HeapPermission<T>>> = Vec::new();
        while records.len() != 0
            invariant owner == batch.owner(), records@ == batch.records().take(records.len() as int),
                records.len() + memories.len() == batch.records().len(),
                valid_records(records@, owner), prepared_records(records@, pending.domain()),
                pending.inv(),
                forall|i: int| 0 <= i < memories.len() ==> (#[trigger] memories@[i])@
                    == batch.records()[batch.records().len() - 1 - i].memory(),
            decreases records.len(),
        {
            let entry = records.pop().unwrap();
            let memory = recover_prepared_drain_leases(entry, Tracked(pending));
            memories.push(memory);
        }
        RecoveredBatch { owner, memories }
    }
    pub fn ordinary<'slot, 'domain, T>(queue: &mut Registration<Retirement<'slot, 'domain, T>>,
        domain: &'domain DomainOwner, certificate: &Drained<'_>) -> (batch: Option<Batch<'domain, Retirement<'slot, 'domain, T>>>)
        requires valid_queue(old(queue), domain), certificate.inv(), old(queue).held.is_none(), !old(queue).both_held,
        ensures valid_queue(final(queue), domain), final(queue).held.is_none(), !final(queue).both_held,
            batch.is_some() == (domain.rotation.addr() == certificate.state().domain.addr()),
            batch.is_some() ==> batch.unwrap().owner() == domain && valid_records(batch.unwrap().records(), domain)
                && batch.unwrap().records() == if certificate.index() == 1 { old(queue).one@ } else { old(queue).zero@ },
    { super::super::batches::$module::ordinary(queue, domain, certificate) }
    pub fn ordinary_recover<'slot, 'domain, T>(queue: &mut Registration<Retirement<'slot, 'domain, T>>,
        domain: &'domain DomainOwner, certificate: &Drained<'_>, Ghost(histories): Ghost<Seq<Seq<$word>>>,
        Tracked(pending): Tracked<&stripes::StripeLedgers>) -> (recovered: Option<RecoveredBatch<'domain, T>>)
        requires valid_queue(old(queue), domain), certificate.inv(), old(queue).held.is_none(), !old(queue).both_held,
            prepared_records(if certificate.index() == 1 { old(queue).one@ } else { old(queue).zero@ }, stripes::domain(pending)),
            stripes::drained_histories(histories), pending.matches(stripes::final_states(histories)),
        ensures valid_queue(final(queue), domain), final(queue).held.is_none(), !final(queue).both_held,
            recovered.is_some() == (domain.rotation.addr() == certificate.state().domain.addr()),
            recovered.is_some() ==> recovered.unwrap().owner() == domain
                && recovered.unwrap().memories().len() == (if certificate.index() == 1 { old(queue).one@ } else { old(queue).zero@ }).len(),
            recovered.is_some() ==> forall|i: int| 0 <= i < recovered.unwrap().memories().len()
                ==> (#[trigger] recovered.unwrap().memories()[i])@ ==
                    (if certificate.index() == 1 { old(queue).one@ } else { old(queue).zero@ })[
                        recovered.unwrap().memories().len() - 1 - i].memory(),
    {
        match ordinary(queue, domain, certificate) {
            Some(batch) => Some(recover_batch(batch, Ghost(histories), Tracked(pending))),
            None => None,
        }
    }
    pub fn terminal<'slot, 'domain, T>(queue: &mut Registration<Retirement<'slot, 'domain, T>>,
        domain: &'domain DomainOwner, certificate: &Closed<'_>) -> (batches: Option<[Batch<'domain, Retirement<'slot, 'domain, T>>; 2]>)
        requires valid_queue(old(queue), domain), certificate.inv(), old(queue).held.is_none(), !old(queue).both_held,
        ensures valid_queue(final(queue), domain), final(queue).held.is_none(), !final(queue).both_held,
            batches.is_some() == (domain.rotation.addr() == certificate.state().domain.addr()),
            batches.is_some() ==> batches.unwrap()[0].owner() == domain && batches.unwrap()[1].owner() == domain
                && batches.unwrap()[0].records() == old(queue).zero@ && batches.unwrap()[1].records() == old(queue).one@,
    { super::super::batches::$module::terminal(queue, domain, certificate) }
    }
    }
    };
}
width!(word32, u32, narrow_stripes_32, recover_pending_stripes_32, recover_after_histories_32);
width!(word64, u64, narrow_stripes_64, recover_pending_stripes_64, recover_after_histories_64);

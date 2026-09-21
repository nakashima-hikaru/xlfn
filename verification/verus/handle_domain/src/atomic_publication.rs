//! Resource acquisition at a real vstd atomic load, including allocation reuse.
//! vstd uses SeqCst: native Acquire/Release/AcqRel refinement remains open.
use vstd::prelude::*;
use vstd::atomic_with_ghost;
use vstd::atomic_ghost::{AtomicPtr, AtomicInvariantPredicate};
use super::heap_permission::HeapPermission;
use super::observations::{bindings, borrow_owned_observation, RetiredRecord as QueueRetiredRecord};
use super::publication_counts::membership;
use super::retained_counts::{RetainedCounts, IndexedObservation, CoverageBound};
use super::rotation::drain::permit_shares::{Scope, Share};
use super::writer_authority::publication;
verus! {
pub tracked struct Prepared<T> {
    pub instance: bindings::Instance<HeapPermission<T>>,
    pub published: bindings::published<HeapPermission<T>>,
    pub receipt: membership::known,
}
impl<T> Prepared<T> {
    pub open spec fn valid(&self, registry: vstd::tokens::InstanceId, owner: *const u8,
        domain: Set<vstd::tokens::InstanceId>) -> bool {
        self.instance.owner() == owner && self.instance.domain() == domain
        && self.published.instance_id() == self.instance.id()
        && self.receipt.instance_id() == registry && self.receipt.key() == self.instance.id()
        && self.published.value().is_init()
    }
}
/// Retirement carries the persistent receipt of the allocation's issuing counter
/// registry, so drain recovery cannot substitute another slot's zero counter.
pub struct RetiredRecord<T> {
    entry: QueueRetiredRecord<T>,
    instance: Tracked<bindings::Instance<HeapPermission<T>>>,
    receipt: Tracked<membership::known>,
    history: Tracked<bindings::retired_history<HeapPermission<T>>>,
}
impl<T> RetiredRecord<T> {
    pub closed spec fn inv(&self) -> bool {
        self.entry.node_id() == self.instance@.id() && self.receipt@.key() == self.instance@.id()
        && self.entry.pointer() == self.entry.memory().ptr() && self.entry.memory().is_init()
        && self.entry.owner() == self.instance@.owner() && self.history@.instance_id() == self.instance@.id()
    }
    pub closed spec fn registry_id(&self) -> vstd::tokens::InstanceId { self.receipt@.instance_id() }
    pub closed spec fn node_id(&self) -> vstd::tokens::InstanceId { self.entry.node_id() }
    pub closed spec fn memory(&self) -> HeapPermission<T> { self.entry.memory() }
    pub closed spec fn pointer(&self) -> *mut T { self.entry.pointer() }
    pub closed spec fn owner(&self) -> *const u8 { self.entry.owner() }
    fn new(pointer: *mut T, Tracked(instance): Tracked<&bindings::Instance<HeapPermission<T>>>,
        Tracked(ticket): Tracked<bindings::retired<HeapPermission<T>>>, Tracked(receipt): Tracked<membership::known>,
        Tracked(history): Tracked<bindings::retired_history<HeapPermission<T>>>) -> (entry: Self)
        requires ticket.instance_id() == instance.id(), receipt.key() == instance.id(), history.instance_id() == instance.id(),
            ticket.value().ptr() == pointer, ticket.value().is_init(),
        ensures entry.inv(), entry.registry_id() == receipt.instance_id(), entry.node_id() == instance.id(),
            entry.memory() == ticket.value(), entry.pointer() == pointer, entry.owner() == instance.owner(),
    { RetiredRecord { entry: QueueRetiredRecord::new(pointer, Tracked(instance), Tracked(ticket)), instance: Tracked(instance.clone()), receipt: Tracked(receipt), history: Tracked(history) } }
    fn into_ticket(self) -> (ticket: Tracked<bindings::retired<HeapPermission<T>>>)
        ensures ticket@.instance_id() == self.node_id(), ticket@.value() == self.memory(),
    { self.entry.into_ticket() }
}
pub tracked struct SlotGhost<T> {
    authority: publication::atomic<T>,
    published: Option<Prepared<T>>,
    counts: RetainedCounts<T>,
}
pub enum PublishStatus { Published, FailStop }
pub fn some_prepared<T>(Tracked(prepared): Tracked<Prepared<T>>) -> (result: Tracked<Option<Prepared<T>>>)
    ensures result@ == Some(prepared),
{ Tracked(Some(prepared)) }
pub struct SlotPredicate;
impl<T> AtomicInvariantPredicate<(vstd::tokens::InstanceId, *const u8, Set<vstd::tokens::InstanceId>, vstd::tokens::InstanceId, vstd::tokens::InstanceId), *mut T, SlotGhost<T>> for SlotPredicate {
    closed spec fn atomic_inv(k: (vstd::tokens::InstanceId, *const u8, Set<vstd::tokens::InstanceId>, vstd::tokens::InstanceId, vstd::tokens::InstanceId), value: *mut T, g: SlotGhost<T>) -> bool {
        g.authority.instance_id() == k.3 && g.authority.value() == value
        && g.counts.inv() && g.counts.registry_id() == k.0
        && g.counts.domain() == k.2 && g.counts.index_registry_id() == k.4
        && (g.published.is_some() == (value.addr() != 0))
        && (g.published.is_some() ==> g.published.unwrap().valid(k.0, k.1, k.2)
            && g.counts.contains(g.published.unwrap().instance.id())
            && g.published.unwrap().published.value().ptr() == value)
    }
}
pub struct Slot<T> {
    authority: Tracked<publication::Instance<T>>,
    owner: *const u8,
    atomic: AtomicPtr<T, (vstd::tokens::InstanceId, *const u8, Set<vstd::tokens::InstanceId>, vstd::tokens::InstanceId, vstd::tokens::InstanceId), SlotGhost<T>, SlotPredicate>,
}
pub struct Read<'a, T> {
    slot: &'a Slot<T>,
    pointer: *mut T,
    instance: Tracked<bindings::Instance<HeapPermission<T>>>,
    observation: Tracked<IndexedObservation<T>>,
}
impl<T> Slot<T> {
    pub closed spec fn inv(&self) -> bool { self.atomic.well_formed() && self.owner == self.atomic.constant().1 && self.authority@.id() == self.atomic.constant().3 }
    pub closed spec fn owner(&self) -> *const u8 { self.owner }
    pub closed spec fn domain(&self) -> Set<vstd::tokens::InstanceId> { self.atomic.constant().2 }
    pub closed spec fn authority_id(&self) -> vstd::tokens::InstanceId { self.authority@.id() }
    pub closed spec fn index_registry_id(&self) -> vstd::tokens::InstanceId { self.atomic.constant().4 }
    pub closed spec fn registry_id(&self) -> vstd::tokens::InstanceId { self.atomic.constant().0 }
    pub fn new(owner: *const u8, pointer: *mut T, Tracked(instance): Tracked<bindings::Instance<HeapPermission<T>>>,
        Tracked(published): Tracked<bindings::published<HeapPermission<T>>>,
        Tracked(count): Tracked<bindings::observing<HeapPermission<T>>>) -> (result: (Self, Tracked<publication::writer<T>>))
        requires instance.owner() == owner, published.instance_id() == instance.id(), count.instance_id() == instance.id(), count.value() == 0,
            published.value().ptr() == pointer, published.value().is_init(), pointer.addr() != 0,
        ensures result.0.inv(), result.0.owner() == owner, result.0.domain() == instance.domain(),
            result.1@.instance_id() == result.0.authority_id(), result.1@.value() == pointer,
    {
        let tracked mut counts = RetainedCounts::new(instance.domain());
        let tracked receipt = counts.register(count);
        let tracked (Tracked(authority), Tracked(atomic_view), Tracked(writer)) = publication::Instance::initialize(pointer);
        let ghost k = (counts.registry_id(), owner, instance.domain(), authority.id(), counts.index_registry_id());
        let tracked g = SlotGhost { authority: atomic_view, published: Some(Prepared { instance, published, receipt }), counts };
        let atomic = AtomicPtr::new(Ghost(k), pointer, Tracked(g));
        (Slot { authority: Tracked(authority), owner, atomic }, Tracked(writer))
    }
    pub fn register_count(&self, Tracked(count): Tracked<bindings::observing<HeapPermission<T>>>)
        -> (result: Tracked<Result<membership::known, bindings::observing<HeapPermission<T>>>>)
        requires self.inv(), count.value() == 0,
        ensures result@ is Ok ==> result@->Ok_0.instance_id() == self.registry_id()
                && result@->Ok_0.key() == count.instance_id(),
            result@ is Err ==> result@->Err_0 == count,
    {
        let tracked mut result = None;
        atomic_with_ghost!(self.atomic => no_op(); ghost g => {
            if g.counts.contains(count.instance_id()) { result = Some(Err(count)); }
            else { result = Some(Ok(g.counts.register(count))); }
        });
        Tracked(result.tracked_unwrap())
    }
    /// Resource-level recovery attempt. The ghost result is not a native
    /// readiness test: native drain must still establish that it is Ok.
    pub fn recover_resource(&self, entry: RetiredRecord<T>,
        Tracked(instance): Tracked<&bindings::Instance<HeapPermission<T>>>)
        -> (result: Tracked<Result<HeapPermission<T>, bindings::retired<HeapPermission<T>>>>)
        requires self.inv(), entry.node_id() == instance.id(),
        ensures result@ is Ok ==> result@->Ok_0 == entry.memory(),
            result@ is Err ==> result@->Err_0.instance_id() == entry.node_id()
                && result@->Err_0.value() == entry.memory(),
    {
        let ticket = entry.into_ticket();
        let tracked mut result = None;
        atomic_with_ghost!(self.atomic => no_op(); ghost g => {
            result = Some(g.counts.recover(instance, ticket.get()));
        });
        Tracked(result.tracked_unwrap())
    }
    pub fn load_for_writer(&self, Tracked(writer): Tracked<&publication::writer<T>>) -> (pointer: *mut T)
        requires self.inv(), writer.instance_id() == self.authority_id(),
        ensures pointer == writer.value(),
    {
        atomic_with_ghost!(self.atomic => load(); ghost g => {
            self.authority.borrow().agree(&g.authority, writer);
        })
    }
    fn store_empty(&self, pointer: *mut T, Tracked(prepared): Tracked<Prepared<T>>,
        Tracked(writer): Tracked<&mut publication::writer<T>>)
        requires self.inv(), old(writer).instance_id() == self.authority_id(), old(writer).value().addr() == 0,
            prepared.valid(self.registry_id(), self.owner(), self.domain()), prepared.published.value().ptr() == pointer,
            pointer.addr() != 0,
        ensures final(writer).instance_id() == self.authority_id(), final(writer).value() == pointer,
    {
        atomic_with_ghost!(self.atomic => store(pointer); ghost g => {
            self.authority.borrow().agree(&g.authority, writer);
            assert(g.published.is_none());
            g.counts.remembered(&prepared.receipt);
            g.published = Some(prepared);
            self.authority.borrow().set(pointer, &mut g.authority, writer);
        });
    }
    /// Occupied publication is FailStop,
    /// corresponding to the native abort branch of the shared expression.
    pub fn publish(&self, pointer: *mut T, Tracked(prepared): Tracked<Prepared<T>>,
        Tracked(writer): Tracked<&mut publication::writer<T>>)
        -> (result: (PublishStatus, Tracked<Option<Prepared<T>>>))
        requires self.inv(), old(writer).instance_id() == self.authority_id(),
            prepared.valid(self.registry_id(), self.owner(), self.domain()),
            prepared.published.value().ptr() == pointer, pointer.addr() != 0,
        ensures final(writer).instance_id() == self.authority_id(), (result.0 is Published) == result.1@.is_none(),
            (result.0 is Published) == (old(writer).value().addr() == 0),
            result.1@.is_some() ==> result.1@.unwrap() == prepared,
    {
        let loaded = self.load_for_writer(Tracked(&*writer));
        let prepared_arg = Tracked(prepared);
        let writer_arg = Tracked(writer);
        super::read_protocol::publish_binding!(loaded.addr() == 0,
            return (PublishStatus::FailStop, some_prepared(prepared_arg)),
            self.publish_empty_result(pointer, prepared_arg, writer_arg)
        )
    }
    fn publish_empty_result(&self, pointer: *mut T, prepared: Tracked<Prepared<T>>,
        Tracked(writer): Tracked<&mut publication::writer<T>>) -> (result: (PublishStatus, Tracked<Option<Prepared<T>>>))
        requires self.inv(), old(writer).instance_id() == self.authority_id(), old(writer).value().addr() == 0,
            prepared@.valid(self.registry_id(), self.owner(), self.domain()), prepared@.published.value().ptr() == pointer,
            pointer.addr() != 0,
        ensures final(writer).instance_id() == self.authority_id(), result.0 is Published, result.1@.is_none(),
    {
        self.store_empty(pointer, prepared, Tracked(writer));
        (PublishStatus::Published, Tracked(None))
    }
    pub fn load<'a>(&'a self, Tracked(scope): Tracked<&mut Scope>)
        -> (read: Option<Read<'a, T>>)
        requires self.inv(), old(scope).inv(), self.domain().contains(old(scope).gate_id()),
        ensures final(scope).inv(), final(scope).id() == old(scope).id(), final(scope).gate_id() == old(scope).gate_id(),
            final(scope).len() == old(scope).len() + if read.is_some() { 1nat } else { 0nat },
            read.is_some() ==> read.unwrap().inv() && read.unwrap().gate_id() == old(scope).gate_id()
                && read.unwrap().scope_id() == old(scope).id(),
    {
        let tracked mut acquired = None;
        let pointer = atomic_with_ghost!(self.atomic => load(); returning pointer; ghost g => {
            if pointer.addr() != 0 {
                let tracked p = g.published.tracked_borrow();
                let tracked share = scope.issue();
                let tracked observation = g.counts.observe(&p.instance, &p.published, &p.receipt, share);
                acquired = Some((observation, p.instance.clone()));
            }
        });
        if pointer.addr() == 0 { None } else {
            let tracked (observation, instance) = acquired.tracked_unwrap();
            Some(Read { slot: self, pointer, instance: Tracked(instance), observation: Tracked(observation) })
        }
    }
    /// CAS compares addresses; the returned entry uses the actual removed
    /// pointer's provenance, never the caller's expected pointer provenance.
    pub fn retire(&self, expected: *mut T, Tracked(writer): Tracked<&mut publication::writer<T>>) -> (entry: Option<RetiredRecord<T>>)
        requires self.inv(), old(writer).instance_id() == self.authority_id(), expected.addr() != 0,
        ensures final(writer).instance_id() == self.authority_id(), entry.is_some() ==> entry.unwrap().inv() && entry.unwrap().registry_id() == self.registry_id()
            && entry.unwrap().owner() == self.owner()
            && entry.unwrap().pointer().addr() == expected.addr(),
    {
        let tracked mut retired = None;
        let result = atomic_with_ghost!(self.atomic => compare_exchange(expected, core::ptr::null_mut());
            returning result; ghost g => {
                if result is Ok {
                    let tracked published = g.published;
                    g.published = None;
                    self.authority.borrow().set(core::ptr::null_mut(), &mut g.authority, writer);
                    let tracked p = published.tracked_unwrap();
                    let tracked (Tracked(ticket), Tracked(history)) = p.instance.retire(p.published.value(), p.published);
                    g.counts.note_retired(history);
                    retired = Some((p.instance, ticket, p.receipt, history));
                }
            });
        match result {
            Ok(pointer) => {
                let tracked (instance, ticket, receipt, history) = retired.tracked_unwrap();
                Some(RetiredRecord::new(pointer, Tracked(&instance), Tracked(ticket), Tracked(receipt), Tracked(history)))
            },
            Err(_) => None,
        }
    }
}
impl<'a, T> Read<'a, T> {
    pub closed spec fn inv(&self) -> bool {
        self.slot.inv() && self.observation@.node_id() == self.instance@.id()
        && self.observation@.valid(self.slot.registry_id(), self.slot.index_registry_id(), self.slot.domain())
        && self.observation@.memory().ptr() == self.pointer && self.observation@.memory().is_init()
    }
    pub closed spec fn registry_id(&self) -> vstd::tokens::InstanceId { self.slot.registry_id() }
    pub closed spec fn node_id(&self) -> vstd::tokens::InstanceId { self.observation@.node_id() }
    pub closed spec fn scope_id(&self) -> vstd::tokens::InstanceId { self.observation@.scope_id() }
    pub closed spec fn gate_id(&self) -> vstd::tokens::InstanceId { self.observation@.gate_id() }
    pub closed spec fn pointer(&self) -> *mut T { self.pointer }
    pub closed spec fn value(&self) -> T { self.observation@.memory().value() }
    pub closed spec fn authority_id(&self) -> vstd::tokens::InstanceId { self.slot.authority_id() }
    pub closed spec fn accepts(&self, prepared: Prepared<T>) -> bool {
        prepared.valid(self.slot.registry_id(), self.slot.owner(), self.slot.domain())
    }
    pub fn borrow(&self) -> (value: &T)
        requires self.inv(), ensures *value == self.value(),
    { borrow_owned_observation(self.pointer as *const T, Tracked(self.instance.borrow()), Tracked(self.observation.borrow().observation())) }
    /// An actual reader issued by this atomic slot prevents its allocation's
    /// retirement token from yielding memory, including after slot reuse.
    pub fn reject_recovery_while_observed(&self, entry: RetiredRecord<T>)
        -> (ticket: Tracked<bindings::retired<HeapPermission<T>>>)
        requires self.inv(), entry.node_id() == self.node_id(),
        ensures ticket@.instance_id() == entry.node_id(), ticket@.value() == entry.memory(),
    {
        let retired = entry.into_ticket();
        let tracked mut result = None;
        atomic_with_ghost!(self.slot.atomic => no_op(); ghost g => {
            g.counts.observed_is_positive(self.observation.borrow());
            let tracked recovery = g.counts.recover(self.instance.borrow(), retired.get());
            assert(recovery is Err);
            result = Some(match recovery { Err(ticket) => ticket, Ok(_) => { assert(false); proof_from_false() } });
        });
        Tracked(result.tracked_unwrap())
    }
    pub fn end_in_scope(self, Tracked(scope): Tracked<&mut Scope>)
        requires self.inv(), old(scope).inv(), old(scope).id() == self.scope_id(),
        ensures final(scope).inv(), final(scope).id() == old(scope).id(),
            final(scope).gate_id() == old(scope).gate_id(), final(scope).len() + 1 == old(scope).len(),
    {
        let share = self.end();
        proof { scope.finish(share.get()); }
    }
    pub fn end(self) -> (share: Tracked<Share>)
        requires self.inv(),
        ensures share@.inv(), share@.scope_id() == self.scope_id(), share@.gate_id() == self.gate_id(),
    {
        let tracked mut share = None;
        atomic_with_ghost!(self.slot.atomic => no_op(); ghost g => {
            share = Some(g.counts.end(self.instance.borrow(), self.observation.get()));
        });
        Tracked(share.tracked_unwrap())
    }
}
}
verus! {
pub enum ReadOutcome<'a> { FailStop, Stale, Live(Read<'a, super::reading::Record>) }
impl Slot<super::reading::Record> {
    pub fn read<'a>(&'a self, domain: *const u8, id: u64, sampled_live: bool,
        Tracked(scope): Tracked<&mut Scope>) -> (result: ReadOutcome<'a>)
        requires self.inv(), old(scope).inv(), domain.addr() == self.owner().addr() ==> self.domain().contains(old(scope).gate_id()),
        ensures final(scope).inv(), final(scope).id() == old(scope).id(),
            final(scope).gate_id() == old(scope).gate_id(),
            final(scope).len() == old(scope).len() + if result is Live { 1nat } else { 0nat },
            domain.addr() != self.owner().addr() ==> result is FailStop,
            result is Live ==> result->Live_0.inv() && result->Live_0.value().id == id
                && result->Live_0.gate_id() == old(scope).gate_id() && result->Live_0.scope_id() == old(scope).id() && sampled_live,
    {
        let scope_arg = Tracked(&mut *scope);
        super::read_protocol::read_binding!(read, value;
            domain.addr() == self.owner.addr(), return ReadOutcome::FailStop,
            self.load(scope_arg), return ReadOutcome::Stale,
            read.borrow(), value.id == id && sampled_live,
            { verus_exec_expr!(read.end_in_scope(Tracked(&mut *scope))); return ReadOutcome::Stale; }, ReadOutcome::Live(read)
        )
    }
}
pub fn inspect_after_retire<T: Copy>(read: Read<'_, T>, expected: *mut T, Tracked(writer): Tracked<&mut publication::writer<T>>,
    Tracked(scope): Tracked<&mut Scope>)
    -> (result: (T, Option<RetiredRecord<T>>))
    requires read.inv(), old(scope).inv(), old(scope).id() == read.scope_id(), old(writer).instance_id() == read.authority_id(), expected == read.pointer(), expected.addr() != 0,
    ensures final(scope).inv(), final(scope).id() == old(scope).id(), final(scope).len() + 1 == old(scope).len(),
        result.0 == read.value(), result.1.is_some() ==> result.1.unwrap().pointer().addr() == expected.addr(),
{
    let retired = read.slot.retire(expected, Tracked(writer));
    let value = *read.borrow();
    read.end_in_scope(Tracked(scope));
    (value, retired)
}
}

verus! {
/// The reader keeps its allocation even when the same slot accepts a different
/// publication. Failure/racing publication also leaves that reader intact.
pub fn inspect_across_republication<T: Copy>(read: Read<'_, T>, replacement: *mut T,
    Tracked(prepared): Tracked<Prepared<T>>, Tracked(writer): Tracked<&mut publication::writer<T>>,
    Tracked(scope): Tracked<&mut Scope>)
    -> (result: (T, PublishStatus, Option<RetiredRecord<T>>, Tracked<Option<Prepared<T>>>))
    requires read.inv(), old(scope).inv(), old(scope).id() == read.scope_id(), old(writer).instance_id() == read.authority_id(), read.pointer().addr() != 0, read.accepts(prepared),
        prepared.instance.id() != read.node_id(), prepared.published.value().ptr() == replacement, replacement.addr() != 0,
    ensures final(scope).inv(), final(scope).id() == old(scope).id(), final(scope).len() + 1 == old(scope).len(),
        result.0 == read.value(), (result.1 is Published) == result.3@.is_none(),
        result.3@.is_some() ==> result.3@.unwrap() == prepared,
{
    let retired = read.slot.retire(read.pointer, Tracked(writer));
    let (published, remaining) = read.slot.publish(replacement, Tracked(prepared), Tracked(writer));
    let value = *read.borrow();
    read.end_in_scope(Tracked(scope));
    (value, published, retired, remaining)
}
}

macro_rules! drained_recovery {
    ($method:ident, $module:ident, $word:ty) => {
    verus! {
    impl<T> Slot<T> {
        /// All-stripe drain is connected to the very registry used by load/end.
        pub fn $method(&self, entry: RetiredRecord<T>,
            Ghost(histories): Ghost<Seq<Seq<$word>>>,
            Tracked(ledgers): Tracked<&super::rotation::drain::stripe_ownership::$module::StripeLedgers>)
            -> (memory: Tracked<HeapPermission<T>>)
            requires self.inv(), entry.inv(), entry.registry_id() == self.registry_id(),
                super::rotation::drain::stripe_ownership::$module::drained_histories(histories),
                ledgers.matches(super::rotation::drain::stripe_ownership::$module::final_states(histories)),
                super::coverage::$module::covers(self.domain(), ledgers),
            ensures memory@ == entry.memory(),
        {
            let RetiredRecord { entry, instance, receipt, history: _ } = entry;
            let ticket = entry.into_ticket();
            let tracked mut memory = None;
            atomic_with_ghost!(self.atomic => no_op(); ghost g => {
                g.counts.remembered(receipt.borrow());
                memory = Some(super::retained_counts::$module::recover_after_histories(
                    &g.counts, instance.borrow(), ticket.get(), histories, ledgers));
            });
            Tracked(memory.tracked_unwrap())
        }
    }
    }
    };
}
drained_recovery!(recover_after_histories_32, word32, u32);
drained_recovery!(recover_after_histories_64, word64, u64);

macro_rules! pending_recovery {
    ($narrow:ident, $recover:ident, $live:ident, $module:ident) => {
    verus! {
    impl<T> Slot<T> {
        pub fn $narrow(&self, entry: &RetiredRecord<T>,
            rotation: &mut super::rotation::refinement::$module::Rotation,
            Tracked(ledgers): Tracked<&super::rotation::refinement::$module::GenerationLedgers>)
            -> (bound: Tracked<CoverageBound>)
            requires self.inv(), entry.inv(), entry.registry_id() == self.registry_id(),
                old(rotation).inv(), old(rotation).pending.is_none(), old(rotation).locked,
                old(rotation).barrier, !old(rotation).closed, ledgers.matches(old(rotation)),
                self.domain() == super::coverage::$module::generation_domain(ledgers),
            ensures final(rotation).inv(), ledgers.matches(final(rotation)),
                final(rotation).current == !old(rotation).current,
                final(rotation).pending == Some(old(rotation).current),
                bound@.valid(self.index_registry_id(), entry.node_id()),
                bound@.limit() == Set::empty().insert(ledgers.selected_id(!final(rotation).current)),
        {
            let tracked mut bound = None;
            atomic_with_ghost!(self.atomic => no_op(); ghost g => {
                g.counts.remembered(entry.receipt.borrow());
                g.counts.note_retired(*entry.history.borrow());
                bound = Some(super::retained_counts::$module::narrow_before_rotation(
                    &mut g.counts, entry.node_id(), rotation, ledgers));
            });
            super::rotation::refinement::$module::shared_begin_and_publication(rotation);
            Tracked(bound.tracked_unwrap())
        }
        pub fn $recover(&self, entry: RetiredRecord<T>, Tracked(bound): Tracked<&CoverageBound>,
            rotation: &super::rotation::refinement::$module::Rotation,
            Tracked(ledgers): Tracked<&super::rotation::refinement::$module::GenerationLedgers>)
            -> (memory: Tracked<HeapPermission<T>>)
            requires self.inv(), entry.inv(), entry.registry_id() == self.registry_id(),
                bound.valid(self.index_registry_id(), entry.node_id()),
                rotation.inv(), ledgers.matches(rotation), rotation.pending == Some(!rotation.current),
                rotation.idle(!rotation.current),
                bound.limit().subset_of(Set::empty().insert(ledgers.selected_id(!rotation.current))),
            ensures memory@ == entry.memory(),
        {
            let RetiredRecord { entry, instance, receipt: _, history: _ } = entry;
            let ticket = entry.into_ticket();
            let tracked mut memory = None;
            atomic_with_ghost!(self.atomic => no_op(); ghost g => {
                memory = Some(super::retained_counts::$module::recover_after_pending(
                    &g.counts, instance.borrow(), ticket.get(), bound, rotation, ledgers));
            });
            Tracked(memory.tracked_unwrap())
        }
    }
    impl<T: Copy> Slot<T> {
        /// Recover the old allocation, then dereference a still-live reader
        /// admitted in the reopened current generation of this publication slot.
        pub fn $live<'a>(&self, entry: RetiredRecord<T>, Tracked(bound): Tracked<&CoverageBound>,
            rotation: &super::rotation::refinement::$module::Rotation,
            Tracked(ledgers): Tracked<&super::rotation::refinement::$module::GenerationLedgers>,
            current: Read<'a, T>) -> (result: (Tracked<HeapPermission<T>>, T, Read<'a, T>))
            requires self.inv(), entry.inv(), entry.registry_id() == self.registry_id(),
                bound.valid(self.index_registry_id(), entry.node_id()),
                rotation.inv(), ledgers.matches(rotation), rotation.pending == Some(!rotation.current), rotation.idle(!rotation.current),
                bound.limit().subset_of(Set::empty().insert(ledgers.selected_id(!rotation.current))),
                current.inv(), current.registry_id() == self.registry_id(),
                current.gate_id() == ledgers.selected_id(rotation.current),
            ensures result.0@ == entry.memory(), result.1 == current.value(), result.2 == current, result.2.inv(),
        {
            let memory = self.$recover(entry, Tracked(bound), rotation, Tracked(ledgers));
            let value = *current.borrow();
            (memory, value, current)
        }
    }
    }
    };
}
pending_recovery!(narrow_and_begin_32, recover_after_pending_32, recover_with_current_reader_32, word32);
pending_recovery!(narrow_and_begin_64, recover_after_pending_64, recover_with_current_reader_64, word64);

macro_rules! striped_recovery {
    ($narrow:ident, $recover:ident, $live:ident, $module:ident, $word:ty) => {
    verus! {
    impl<T> Slot<T> {
        /// Both generations retain their full stripe identity sets. The kept
        /// generation may still have active readers at this point.
        pub fn $narrow(&self, entry: &RetiredRecord<T>,
            Ghost(kept_raw): Ghost<Seq<$word>>,
            Tracked(kept): Tracked<&super::rotation::drain::stripe_ownership::$module::StripeLedgers>,
            Ghost(idle_histories): Ghost<Seq<Seq<$word>>>,
            Tracked(idle): Tracked<&super::rotation::drain::stripe_ownership::$module::StripeLedgers>)
            -> (bound: Tracked<CoverageBound>)
            requires self.inv(), entry.inv(), entry.registry_id() == self.registry_id(), kept.matches(kept_raw),
                super::rotation::drain::stripe_ownership::$module::drained_histories(idle_histories),
                idle.matches(super::rotation::drain::stripe_ownership::$module::final_states(idle_histories)),
                self.domain() == super::rotation::drain::stripe_ownership::$module::domain(kept).union(
                    super::rotation::drain::stripe_ownership::$module::domain(idle)),
                super::rotation::drain::stripe_ownership::$module::domain(kept).disjoint(
                    super::rotation::drain::stripe_ownership::$module::domain(idle)),
            ensures bound@.valid(self.index_registry_id(), entry.node_id()),
                bound@.limit() == super::rotation::drain::stripe_ownership::$module::domain(kept),
        {
            let ghost keep = super::rotation::drain::stripe_ownership::$module::domain(kept);
            let tracked mut bound = None;
            atomic_with_ghost!(self.atomic => no_op(); ghost g => {
                g.counts.remembered(entry.receipt.borrow());
                g.counts.note_retired(*entry.history.borrow());
                assert(super::coverage::$module::covers(g.counts.domain().difference(keep), idle));
                bound = Some(super::retained_counts::$module::narrow_after_idle_stripes(
                    &mut g.counts, entry.node_id(), keep, idle_histories, idle));
            });
            Tracked(bound.tracked_unwrap())
        }
        /// Only the kept/pending generation must drain. No raw state or idle
        /// premise for the reopened generation is accepted by this operation.
        pub fn $recover(&self, entry: RetiredRecord<T>, Tracked(bound): Tracked<&CoverageBound>,
            Ghost(histories): Ghost<Seq<Seq<$word>>>,
            Tracked(pending): Tracked<&super::rotation::drain::stripe_ownership::$module::StripeLedgers>)
            -> (memory: Tracked<HeapPermission<T>>)
            requires self.inv(), entry.inv(), entry.registry_id() == self.registry_id(),
                bound.valid(self.index_registry_id(), entry.node_id()),
                super::rotation::drain::stripe_ownership::$module::drained_histories(histories),
                pending.matches(super::rotation::drain::stripe_ownership::$module::final_states(histories)),
                bound.limit() == super::rotation::drain::stripe_ownership::$module::domain(pending),
            ensures memory@ == entry.memory(),
        {
            let RetiredRecord { entry, instance, receipt: _, history: _ } = entry;
            let ticket = entry.into_ticket();
            let tracked mut memory = None;
            atomic_with_ghost!(self.atomic => no_op(); ghost g => {
                assert(super::coverage::$module::covers(bound.limit(), pending));
                memory = Some(super::retained_counts::$module::recover_after_bounded_stripes(
                    &g.counts, instance.borrow(), ticket.get(), bound, histories, pending));
            });
            Tracked(memory.tracked_unwrap())
        }
    }
    impl<T: Copy> Slot<T> {
        /// A reader in any other/current stripe remains usable after recovery.
        pub fn $live<'a>(&self, entry: RetiredRecord<T>, Tracked(bound): Tracked<&CoverageBound>,
            Ghost(histories): Ghost<Seq<Seq<$word>>>,
            Tracked(pending): Tracked<&super::rotation::drain::stripe_ownership::$module::StripeLedgers>,
            current: Read<'a, T>) -> (result: (Tracked<HeapPermission<T>>, T, Read<'a, T>))
            requires self.inv(), entry.inv(), entry.registry_id() == self.registry_id(),
                bound.valid(self.index_registry_id(), entry.node_id()),
                super::rotation::drain::stripe_ownership::$module::drained_histories(histories),
                pending.matches(super::rotation::drain::stripe_ownership::$module::final_states(histories)),
                bound.limit() == super::rotation::drain::stripe_ownership::$module::domain(pending),
                current.inv(), current.registry_id() == self.registry_id(), !bound.limit().contains(current.gate_id()),
            ensures result.0@ == entry.memory(), result.1 == current.value(), result.2 == current, result.2.inv(),
        {
            let memory = self.$recover(entry, Tracked(bound), Ghost(histories), Tracked(pending));
            let value = *current.borrow();
            (memory, value, current)
        }
    }
    }
    };
}
striped_recovery!(narrow_stripes_32, recover_pending_stripes_32, recover_stripes_with_current_reader_32, word32, u32);
striped_recovery!(narrow_stripes_64, recover_pending_stripes_64, recover_stripes_with_current_reader_64, word64, u64);

verus! {
impl<T> Slot<T> {
    /// Recover from actual sealed-zero leases of every gate in this slot domain.
    pub fn recover_drain_leases(&self, retired: RetiredRecord<T>,
        Tracked(drains): Tracked<&super::rotation::drain::atomic_counter::DrainSet>) -> (memory: Tracked<HeapPermission<T>>)
        requires self.inv(), retired.inv(), retired.registry_id() == self.registry_id(), drains.inv(), drains.covers(self.domain()),
        ensures memory@ == retired.memory(),
    {
        let RetiredRecord { entry, instance, receipt, history: _ } = retired;
        let ticket = entry.into_ticket();
        let tracked mut recovered = None;
        atomic_with_ghost!(self.atomic => no_op(); ghost g => {
            g.counts.remembered(receipt.borrow());
            g.counts.coverage_in_domain(instance@.id());
            recovered = Some(super::retained_counts::recover_after_drain_leases(&g.counts, instance.borrow(), ticket.get(), drains));
        });
        Tracked(recovered.tracked_unwrap())
    }
    /// A persistent retirement bound permits draining only the pending gates.
    pub fn recover_bounded_drain_leases(&self, retired: RetiredRecord<T>, Tracked(bound): Tracked<&CoverageBound>,
        Tracked(drains): Tracked<&super::rotation::drain::atomic_counter::DrainSet>) -> (memory: Tracked<HeapPermission<T>>)
        requires self.inv(), retired.inv(), retired.registry_id() == self.registry_id(),
            bound.valid(self.index_registry_id(), retired.node_id()), drains.inv(), drains.covers(bound.limit()),
        ensures memory@ == retired.memory(),
    {
        let RetiredRecord { entry, instance, receipt: _, history: _ } = retired;
        let ticket = entry.into_ticket();
        let tracked mut recovered = None;
        atomic_with_ghost!(self.atomic => no_op(); ghost g => {
            g.counts.prove_bound(instance@.id(), bound);
            recovered = Some(super::retained_counts::recover_after_drain_leases(&g.counts, instance.borrow(), ticket.get(), drains));
        });
        Tracked(recovered.tracked_unwrap())
    }
}
}

verus! {
impl<T: Copy> Slot<T> {
    pub fn recover_drain_leases_with_current_reader<'a>(&self, retired: RetiredRecord<T>, Tracked(bound): Tracked<&CoverageBound>,
        Tracked(drains): Tracked<&super::rotation::drain::atomic_counter::DrainSet>, current: Read<'a, T>)
        -> (result: (Tracked<HeapPermission<T>>, T, Read<'a, T>))
        requires self.inv(), retired.inv(), retired.registry_id() == self.registry_id(),
            bound.valid(self.index_registry_id(), retired.node_id()), drains.inv(), drains.covers(bound.limit()),
            current.inv(), current.registry_id() == self.registry_id(), !drains.domain().contains(current.gate_id()),
        ensures result.0@ == retired.memory(), result.1 == current.value(), result.2 == current, result.2.inv(),
    {
        let memory = self.recover_bounded_drain_leases(retired, Tracked(bound), Tracked(drains));
        let value = *current.borrow();
        (memory, value, current)
    }
}
}

verus! {
impl<T> Slot<T> {
    pub fn narrow_drain_leases(&self, retired: &RetiredRecord<T>, Ghost(keep): Ghost<Set<vstd::tokens::InstanceId>>,
        Tracked(idle): Tracked<&super::rotation::drain::atomic_counter::DrainSet>) -> (bound: Tracked<CoverageBound>)
        requires self.inv(), retired.inv(), retired.registry_id() == self.registry_id(), idle.inv(), idle.covers(self.domain().difference(keep)),
        ensures bound@.valid(self.index_registry_id(), retired.node_id()), bound@.limit() == keep,
    {
        let tracked bound;
        atomic_with_ghost!(self.atomic => no_op(); ghost g => {
            g.counts.remembered(retired.receipt.borrow());
            g.counts.note_retired(*retired.history.borrow());
            bound = super::retained_counts::narrow_after_drain_leases(&mut g.counts, retired.node_id(), keep, idle);
        });
        Tracked(bound)
    }
}
}

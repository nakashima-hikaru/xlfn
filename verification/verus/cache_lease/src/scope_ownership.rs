//! Borrowed gate permits keep scoped node observations inside admission.
//! Node-to-domain registration remains a caller composition obligation: this
//! module does not infer it merely from two non-null pointers.
use vstd::prelude::*;
use super::heap_permission::HeapPermission;
use super::pin_ownership::{cache_pins, PinKind, borrow_from_observation};
use super::rotation::gate_permits::admission;
verus! {
pub tracked struct InitializedNode<T> {
    pub instance: cache_pins::Instance<HeapPermission<T>>,
    pub allocation: cache_pins::allocation<HeapPermission<T>>,
    pub count: cache_pins::count<HeapPermission<T>>,
    pub creator: cache_pins::pins<HeapPermission<T>>,
    pub observing: cache_pins::observing<HeapPermission<T>>,
    pub retiring: cache_pins::retiring<HeapPermission<T>>,
}

/// Deposit real allocator/memory ownership while fixing the node's domain.
/// The returned creator is the only initial pin; no observation or retirement
/// ticket is created at allocation.
pub proof fn initialize_node<T>(tracked memory: HeapPermission<T>, domain: Set<vstd::tokens::InstanceId>)
    -> (tracked node: InitializedNode<T>)
    requires memory.is_init(),
    ensures node.instance.domain() == domain,
        node.allocation.instance_id() == node.instance.id(), node.allocation.value() == Some(memory),
        node.count.instance_id() == node.instance.id(), node.count.value() == 1,
        node.creator.instance_id() == node.instance.id(), node.creator.element() == (memory, PinKind::Creator),
        node.observing.instance_id() == node.instance.id(), node.observing.value() == 0,
        node.retiring.instance_id() == node.instance.id(), !node.retiring.value(),
{
    let tracked (Tracked(instance), Tracked(allocation), Tracked(count), Tracked(mut pins),
        Tracked(observations), Tracked(observing), Tracked(retiring), Tracked(retirement))
        = cache_pins::Instance::allocate(memory, domain, Some(memory));
    let tracked creator = pins.remove((memory, PinKind::Creator));
    InitializedNode { instance, allocation, count, creator, observing, retiring }
}

pub tracked struct ScopedObservation<'scope, T> {
    ghost domain: Set<vstd::tokens::InstanceId>,
    permit: &'scope admission::permits,
    observation: cache_pins::observations<HeapPermission<T>>,
}
impl<'scope, T> ScopedObservation<'scope, T> {
    #[verifier::type_invariant]
    pub closed spec fn inv(self) -> bool { self.domain.contains(self.permit.instance_id()) }
    pub closed spec fn domain(&self) -> Set<vstd::tokens::InstanceId> { self.domain }

    pub closed spec fn gate_id(&self) -> vstd::tokens::InstanceId { self.permit.instance_id() }
    pub closed spec fn node_id(&self) -> vstd::tokens::InstanceId { self.observation.instance_id() }
    pub closed spec fn memory(&self) -> HeapPermission<T> { self.observation.element() }

    pub proof fn domain_covers_gate(tracked &self)
        ensures self.domain().contains(self.gate_id()),
    { use_type_invariant(self); }

    /// A single borrowed admission may cover multiple node observations.
    /// Each observation still requires its own resident fragment.
    pub proof fn observe(tracked permit: &'scope admission::permits,
        tracked node: &cache_pins::Instance<HeapPermission<T>>,
        tracked observing: &mut cache_pins::observing<HeapPermission<T>>,
        tracked resident: &cache_pins::pins<HeapPermission<T>>,
    ) -> (tracked observation: Self)
        requires old(observing).instance_id() == node.id(),
            node.domain().contains(permit.instance_id()),
            resident.instance_id() == node.id(), resident.element().1 == PinKind::Resident,
        ensures observation.gate_id() == permit.instance_id(), observation.node_id() == node.id(),
            observation.domain() == node.domain(), observation.inv(),
            observation.memory() == resident.element().0,
            final(observing).instance_id() == node.id(),
            final(observing).value() == old(observing).value() + 1,
    {
        let tracked observation = node.observe(resident.element().0, resident, observing);
        ScopedObservation { domain: node.domain(), permit, observation }
    }

    pub proof fn end(tracked self,
        tracked node: &cache_pins::Instance<HeapPermission<T>>,
        tracked observing: &mut cache_pins::observing<HeapPermission<T>>,
    )
        requires old(observing).instance_id() == node.id(), self.node_id() == node.id(),
        ensures final(observing).instance_id() == node.id(),
            final(observing).value() + 1 == old(observing).value(),
    { node.leave_observation(self.observation.element(), self.observation, observing); }

    pub proof fn excludes_idle(tracked &self,
        tracked gate: &admission::Instance, tracked active: &admission::active)
        requires self.gate_id() == gate.id(), active.instance_id() == gate.id(),
        ensures active.value() > 0,
    { gate.positive(active, self.permit); }
}

pub fn borrow_scoped<'a, 'scope, T>(ptr: *const T,
    Tracked(node): Tracked<&'a cache_pins::Instance<HeapPermission<T>>>,
    Tracked(observation): Tracked<&'a ScopedObservation<'scope, T>>,
) -> (value: &'a T)
    requires observation.node_id() == node.id(), observation.memory().ptr() == ptr as *mut T,
        observation.memory().is_init(), observation.domain() == node.domain(),
    ensures *value == observation.memory().value(),
        node.domain().contains(observation.gate_id()),
{
    proof { use_type_invariant(observation); }
    borrow_from_observation(ptr, Tracked(node), Tracked(&observation.observation))
}
}


macro_rules! excludes_drained_generation {
    ($function:ident, $width:ident) => {
    verus! {
    /// Compose the actual rotation ledger's zero observation with the same
    /// gate permit borrowed by a storage-backed cache observation.
    pub proof fn $function<T>(tracked observation: &ScopedObservation<'_, T>,
        rotation: &super::rotation::refinement::$width::Rotation,
        tracked ledgers: &super::rotation::refinement::$width::GenerationLedgers,
        selected: bool,
    )
        requires ledgers.matches(rotation), rotation.idle(selected),
            observation.gate_id() == ledgers.selected_id(selected),
        ensures false,
    {
        rotation.idle_excludes_generation_permit(selected, ledgers, observation.permit);
    }
    }
    };
}
excludes_drained_generation!(excludes_drained_generation_32, word32);
excludes_drained_generation!(excludes_drained_generation_64, word64);


macro_rules! scan_while_observed {
    ($function:ident, $width:ident, $word:ty) => {
    verus! {
    /// Execute the shared production all-stripe scan while retaining the
    /// actual Cache observation's permit. A successful idle result is impossible.
    pub fn $function<T>(raw: &mut Vec<$word>,
        Tracked(observation): Tracked<&ScopedObservation<'_, T>>,
        Tracked(ledgers): Tracked<&super::rotation::drain::stripe_ownership::$width::StripeLedgers>,
        Ghost(index): Ghost<int>,
    ) -> (idle: bool)
        requires ledgers.matches(old(raw)@), 0 <= index < ledgers.gates.len(),
            observation.gate_id() == ledgers.gates[index].id(),
        ensures !idle, ledgers.matches(final(raw)@),
    {
        let idle = super::rotation::drain::stripe_ownership::$width::scan_with_ledgers(raw, Tracked(ledgers));
        proof {
            if idle {
                super::rotation::drain::stripe_ownership::$width::all_idle_excludes_permit(
                    ledgers, observation.permit, index);
            }
        }
        idle
    }
    }
    };
}
scan_while_observed!(scan_while_observed_32, word32, u32);
scan_while_observed!(scan_while_observed_64, word64, u64);


macro_rules! exclude_observation_after_histories {
    ($module:ident, $word:ty) => {
    pub mod $module {
    use super::*;
    use super::super::rotation::drain::stripe_ownership::$module as stripes;
    verus! {
    pub proof fn exclude_observation_after_histories<T>(histories: Seq<Seq<$word>>,
        tracked observation: &ScopedObservation<'_, T>,
        tracked ledgers: &stripes::StripeLedgers, index: int)
        requires stripes::drained_histories(histories),
            ledgers.matches(stripes::final_states(histories)),
            0 <= index < histories.len(),
            observation.gate_id() == ledgers.gates[index].id(),
        ensures false,
    {
        stripes::histories_exclude_permit(histories, ledgers, observation.permit, index);
    }
    }
    }
    };
}
exclude_observation_after_histories!(word32, u32);
exclude_observation_after_histories!(word64, u64);

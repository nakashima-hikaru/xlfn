//! Linear pin/observation ownership with storage-backed memory permissions.
//!
//! The generated instance identity prevents mixing capabilities from two nodes.
//! Zero pins produces retirement eligibility, not permission to free memory:
//! outstanding pointer observations continue guarding the same stored permission.

use vstd::prelude::*;
use vstd::multiset::*;
use vstd::raw_ptr::ptr_ref;
use super::heap_permission::HeapPermission;
use verus_state_machines_macros::tokenized_state_machine;
use super::pin_transitions::{self, Acquire, Release};

verus! {

pub enum PinKind { Creator, Resident, Flight, Lease }

tokenized_state_machine!(cache_pins<Perm> {
    fields {
        #[sharding(constant)]
        pub domain: Set<vstd::tokens::InstanceId>,
        #[sharding(variable)]
        pub allocation: Option<Perm>,
        #[sharding(variable)]
        pub count: nat,
        #[sharding(storage_option)]
        pub memory: Option<Perm>,
        #[sharding(multiset)]
        pub pins: Multiset<(Perm, PinKind)>,
        #[sharding(multiset)]
        pub observations: Multiset<Perm>,
        #[sharding(variable)]
        pub observing: nat,
        #[sharding(variable)]
        pub retiring: bool,
        #[sharding(option)]
        pub retirement: Option<Perm>,
    }

    #[invariant]
    pub fn allocation_agrees_memory(&self) -> bool { self.allocation == self.memory }

    #[invariant]
    pub fn pins_agree_memory(&self) -> bool {
        forall|p: (Perm, PinKind)| #[trigger] self.pins.count(p) > 0 ==> self.memory == Some(p.0)
    }

    #[invariant]
    pub fn observations_agree_memory(&self) -> bool {
        forall|p: Perm| #[trigger] self.observations.count(p) > 0 ==> self.memory == Some(p)
    }

    #[invariant]
    pub fn count_conserves_all_roles(&self) -> bool { self.count == self.pins.len() }

    #[invariant]
    pub fn observation_count_conserved(&self) -> bool { self.observing == self.observations.len() }

    #[invariant]
    pub fn zero_is_terminal(&self) -> bool { self.retiring == (self.count == 0) }

    #[invariant]
    pub fn retirement_owns_zero_pin_allocation(&self) -> bool {
        self.retirement == (if self.retiring { self.memory } else { None })
    }

    #[invariant]
    pub fn freed_has_no_capabilities(&self) -> bool {
        self.memory.is_none() ==> self.count == 0 && self.observations.len() == 0
    }

    init! {
        allocate(x: Perm, domain: Set<vstd::tokens::InstanceId>) {
            init domain = domain;
            init allocation = Some(x);
            init count = 1;
            init memory = Some(x);
            init pins = Multiset::singleton((x, PinKind::Creator));
            init observations = Multiset::empty();
            init observing = 0;
            init retiring = false;
            init retirement = None;
        }
    }

    property! {
        pin_positive(x: Perm, kind: PinKind) {
            have pins >= {(x, kind)};
            assert(pre.count > 0);
        }
    }

    property! {
        pin_guard(x: Perm, kind: PinKind) {
            have pins >= {(x, kind)};
            guard memory >= Some(x);
        }
    }

    property! {
        observation_guard(x: Perm) {
            have observations >= {x};
            guard memory >= Some(x);
        }
    }

    transition! {
        acquire_from_pin(x: Perm, source: PinKind, target: PinKind) {
            have pins >= {(x, source)};
            require(!(target is Creator));
            add pins += {(x, target)};
            update count = pre.count + 1;
        }
    }

    transition! {
        creator_to_lease(x: Perm) {
            remove pins -= {(x, PinKind::Creator)};
            add pins += {(x, PinKind::Lease)};
        }
    }

    // The domain/index layer must establish this precondition before creating
    // an observation. It is not inferred from a non-null integer pointer.
    transition! {
        observe(x: Perm) {
            have pins >= {(x, PinKind::Resident)};
            add observations += {x};
            update observing = pre.observing + 1;
        }
    }

    transition! {
        acquire_from_observation(x: Perm) {
            have observations >= {x};
            require(pre.count > 0);
            add pins += {(x, PinKind::Lease)};
            update count = pre.count + 1;
        }
    }

    transition! {
        leave_observation(x: Perm) {
            remove observations -= {x};
            assert(pre.observing > 0);
            update observing = (pre.observing - 1) as nat;
        }
    }

    transition! {
        release_nonfinal(x: Perm, kind: PinKind) {
            require(pre.count > 1);
            remove pins -= {(x, kind)};
            update count = (pre.count - 1) as nat;
        }
    }

    transition! {
        release_final(x: Perm, kind: PinKind) {
            require(pre.count == 1);
            remove pins -= {(x, kind)};
            update count = 0;
            update retiring = true;
            add retirement += Some(x);
        }
    }

    transition! {
        reclaim(x: Perm) {
            require(pre.allocation == Some(x));
            require(pre.retiring);
            require(pre.count == 0);
            require(pre.observing == 0);
            update allocation = None;
            remove retirement -= Some(x);
            withdraw memory -= Some(x);
        }
    }

    #[inductive(allocate)]
    fn allocate_inductive(post: Self, x: Perm, domain: Set<vstd::tokens::InstanceId>) {}
    #[inductive(acquire_from_pin)]
    fn acquire_from_pin_inductive(pre: Self, post: Self, x: Perm, source: PinKind, target: PinKind) {
        assert(pre.pins.count((x, source)) > 0);
        assert(pre.count > 0);
    }
    #[inductive(creator_to_lease)]
    fn creator_to_lease_inductive(pre: Self, post: Self, x: Perm) {
        assert(pre.pins.count((x, PinKind::Creator)) > 0);
    }
    #[inductive(observe)]
    fn observe_inductive(pre: Self, post: Self, x: Perm) {
        assert(pre.pins.count((x, PinKind::Resident)) > 0);
    }
    #[inductive(acquire_from_observation)]
    fn acquire_from_observation_inductive(pre: Self, post: Self, x: Perm) {
        assert(pre.observations.count(x) > 0);
    }
    #[inductive(leave_observation)]
    fn leave_observation_inductive(pre: Self, post: Self, x: Perm) {}
    #[inductive(release_nonfinal)]
    fn release_nonfinal_inductive(pre: Self, post: Self, x: Perm, kind: PinKind) {}
    #[inductive(release_final)]
    fn release_final_inductive(pre: Self, post: Self, x: Perm, kind: PinKind) {}
    #[inductive(reclaim)]
    fn reclaim_inductive(pre: Self, post: Self, x: Perm) {}
});

/// Real typed memory can be borrowed only through a pin from this instance.
/// The storage guard borrows PointsTo; it cannot clone or consume that permission.
pub fn borrow_from_pin<'a, T>(ptr: *const T,
    Tracked(instance): Tracked<&'a cache_pins::Instance<HeapPermission<T>>>,
    Tracked(pin): Tracked<&'a cache_pins::pins<HeapPermission<T>>>,
) -> (value: &'a T)
    requires pin.instance_id() == instance.id(),
        pin.element().0.ptr() == ptr as *mut T, pin.element().0.is_init(),
    ensures *value == pin.element().0.value(),
{
    let tracked permission = instance.pin_guard(pin.element().0, pin.element().1, pin);
    ptr_ref(ptr, Tracked(permission.borrow()))
}

pub fn borrow_from_observation<'a, T>(ptr: *const T,
    Tracked(instance): Tracked<&'a cache_pins::Instance<HeapPermission<T>>>,
    Tracked(observation): Tracked<&'a cache_pins::observations<HeapPermission<T>>>,
) -> (value: &'a T)
    requires observation.instance_id() == instance.id(),
        observation.element().ptr() == ptr as *mut T, observation.element().is_init(),
    ensures *value == observation.element().value(),
{
    let tracked permission = instance.observation_guard(observation.element(), observation);
    ptr_ref(ptr, Tracked(permission.borrow()))
}

/// Withdraw the actual stored memory permission once, after all four pin
/// roles and every pointer observation have ended. This consumes the unique
/// retirement ticket emitted by final pin release and empties allocation storage.
pub proof fn recover_memory<T>(
    tracked instance: &cache_pins::Instance<HeapPermission<T>>,
    tracked allocation: &mut cache_pins::allocation<HeapPermission<T>>,
    tracked count: &cache_pins::count<HeapPermission<T>>,
    tracked observing: &cache_pins::observing<HeapPermission<T>>,
    tracked retiring: &cache_pins::retiring<HeapPermission<T>>,
    tracked retirement: cache_pins::retirement<HeapPermission<T>>,
) -> (tracked permission: HeapPermission<T>)
    requires old(allocation).instance_id() == instance.id(),
        count.instance_id() == instance.id(), observing.instance_id() == instance.id(),
        retiring.instance_id() == instance.id(),
        retirement.instance_id() == instance.id(),
        retirement.value() == old(allocation).value().unwrap(),
        old(allocation).value().is_some(), count.value() == 0,
        observing.value() == 0, retiring.value(),
    ensures final(allocation).instance_id() == instance.id(), final(allocation).value().is_none(),
        permission == old(allocation).value().unwrap(),
{
    instance.reclaim(allocation.value().unwrap(), allocation, count, observing, retiring, retirement)
}

} // verus!

macro_rules! width {
    ($module:ident, $word:ty) => {
    pub mod $module {
    use super::*;
    use pin_transitions::$module as kernel;
    verus! {
        pub(crate) fn acquire_observed<T>(pins: $word,
            Tracked(instance): Tracked<&cache_pins::Instance<HeapPermission<T>>>,
            Tracked(count): Tracked<&mut cache_pins::count<HeapPermission<T>>>,
            Tracked(observation): Tracked<&cache_pins::observations<HeapPermission<T>>>,
        ) -> (result: (Acquire<$word>, Tracked<Option<cache_pins::pins<HeapPermission<T>>>>))
            requires old(count).instance_id() == instance.id(),
                old(count).value() == pins as nat,
                observation.instance_id() == instance.id(),
            ensures final(count).instance_id() == instance.id(),
                match result.0 {
                    Acquire::Acquired(next) => final(count).value() == next as nat
                        && result.1@.is_some()
                        && result.1@.unwrap().instance_id() == instance.id()
                        && result.1@.unwrap().element() == (observation.element(), PinKind::Lease),
                    _ => final(count).value() == pins as nat && result.1@.is_none(),
                },
        {
            let outcome = kernel::acquire(pins);
            let tracked mut pin = None;
            match outcome {
                Acquire::Acquired(next) => {
                    proof {
                        kernel::successful_acquire_adds_one(pins, next);
                        pin = Some(instance.acquire_from_observation(observation.element(), count, observation));
                    }
                },
                _ => {},
            }
            (outcome, Tracked(pin))
        }

        pub(crate) fn acquire_anchored<T>(pins: $word,
            Tracked(instance): Tracked<&cache_pins::Instance<HeapPermission<T>>>,
            Tracked(count): Tracked<&mut cache_pins::count<HeapPermission<T>>>,
            Tracked(source): Tracked<&cache_pins::pins<HeapPermission<T>>>,
            Ghost(target): Ghost<PinKind>,
        ) -> (result: (Acquire<$word>, Tracked<Option<cache_pins::pins<HeapPermission<T>>>>))
            requires old(count).instance_id() == instance.id(),
                old(count).value() == pins as nat,
                source.instance_id() == instance.id(), !(target is Creator),
            ensures final(count).instance_id() == instance.id(),
                match result.0 {
                    Acquire::Acquired(next) => final(count).value() == next as nat
                        && result.1@.is_some()
                        && result.1@.unwrap().instance_id() == instance.id()
                        && result.1@.unwrap().element() == (source.element().0, target),
                    _ => final(count).value() == pins as nat && result.1@.is_none(),
                },
        {
            let outcome = kernel::acquire(pins);
            let tracked mut pin = None;
            match outcome {
                Acquire::Acquired(next) => {
                    proof {
                        kernel::successful_acquire_adds_one(pins, next);
                        pin = Some(instance.acquire_from_pin(source.element().0, source.element().1, target, count, source));
                    }
                },
                _ => {},
            }
            (outcome, Tracked(pin))
        }

        pub(crate) fn release_owned<T>(previous: $word,
            Tracked(instance): Tracked<&cache_pins::Instance<HeapPermission<T>>>,
            Tracked(count): Tracked<&mut cache_pins::count<HeapPermission<T>>>,
            Tracked(retiring): Tracked<&mut cache_pins::retiring<HeapPermission<T>>>,
            Tracked(pin): Tracked<cache_pins::pins<HeapPermission<T>>>,
        ) -> (result: ($word, Release, Tracked<Option<cache_pins::retirement<HeapPermission<T>>>>))
            requires old(count).instance_id() == instance.id(),
                old(count).value() == previous as nat,
                old(retiring).instance_id() == instance.id(),
                pin.instance_id() == instance.id(),
            ensures previous > 0, result.0 as nat == previous as nat - 1,
                final(count).instance_id() == instance.id(),
                final(retiring).instance_id() == instance.id(),
                final(count).value() == result.0 as nat,
                result.1 == kernel::release_spec(previous),
                result.1 == Release::LastPin ==> final(retiring).value(),
                result.2@.is_some() == (result.1 == Release::LastPin),
                result.2@.is_some() ==> result.2@.unwrap().instance_id() == instance.id(),
                result.2@.is_some() ==> result.2@.unwrap().value() == pin.element().0,
        {
            proof { instance.pin_positive(pin.element().0, pin.element().1, count, &pin); }
            let outcome = kernel::release(previous);
            let tracked mut retirement = None;
            proof {
                match outcome {
                    Release::LastPin => {
                        retirement = Some(instance.release_final(pin.element().0, pin.element().1, count, pin, retiring));
                    },
                    Release::StillPinned => instance.release_nonfinal(pin.element().0, pin.element().1, count, pin),
                    Release::FailStop => { assert(false); },
                }
                assert(previous > 0 ==> previous.wrapping_sub(1) == previous - 1) by(bit_vector);
            }
            (previous.wrapping_sub(1), outcome, Tracked(retirement))
        }

        /// Bridge the shared executable acquire result to the natural-number
        /// count consumed by the tokenized machine. No boundedness assumption.
        pub(crate) proof fn acquire_count_bridge(pins: $word, next: $word)
            requires kernel::acquire_spec(pins) == Acquire::Acquired(next),
            ensures pins > 0, next as nat == pins as nat + 1,
        { kernel::successful_acquire_adds_one(pins, next); }

        pub(crate) proof fn release_count_bridge(previous: $word)
            requires previous > 0,
            ensures previous.wrapping_sub(1) as nat == previous as nat - 1,
                (kernel::release_spec(previous) == Release::LastPin) <==> previous == 1,
                (kernel::release_spec(previous) == Release::StillPinned) <==> previous > 1,
        {
            assert(previous > 0 ==> previous.wrapping_sub(1) == previous - 1) by(bit_vector);
        }
    }
    }
    };
}

width!(word32, u32);
width!(word64, u64);

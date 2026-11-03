//! An actual counter permit retained while Cache observations borrow it.
use vstd::prelude::*;
use super::rotation::gate_permits::admission;
use super::rotation::drain::transitions::TransitionOutcome;
macro_rules! width {
    ($module:ident, $word:ty) => {
    pub mod $module {
    use super::*;
    use super::super::rotation::drain::atomic_counter::$module::Counter;
    verus! {
    pub struct Admission<'a> { counter: &'a Counter, permit: Tracked<admission::permits> }
    impl<'a> Admission<'a> {
        pub closed spec fn inv(&self) -> bool { self.counter.inv() && self.permit@.instance_id() == self.counter.id() }
        pub closed spec fn gate_id(&self) -> vstd::tokens::InstanceId { self.counter.id() }
        pub fn permit<'b>(&'b self) -> (permit: Tracked<&'b admission::permits>)
            requires self.inv(), ensures permit@.instance_id() == self.gate_id(),
        { Tracked(self.permit.borrow()) }
        #[verifier::exec_allows_no_decreases_clause]
        pub fn release(self)
            requires self.inv(),
        { self.counter.release(self.permit); }
    }
    #[verifier::exec_allows_no_decreases_clause]
    pub fn acquire<'a>(counter: &'a Counter) -> (result: (TransitionOutcome<$word>, Option<Admission<'a>>))
        requires counter.inv(),
        ensures match result.0 {
            TransitionOutcome::Success(_) => result.1.is_some() && result.1.unwrap().inv() && result.1.unwrap().gate_id() == counter.id(),
            _ => result.1.is_none(),
        },
    {
        let (outcome, Tracked(permit)) = counter.acquire();
        match outcome {
            TransitionOutcome::Success(next) => {
                let tracked permit = permit.tracked_unwrap();
                (TransitionOutcome::Success(next), Some(Admission { counter, permit: Tracked(permit) }))
            },
            TransitionOutcome::Rejected => (TransitionOutcome::Rejected, None),
            TransitionOutcome::FailStop => (TransitionOutcome::FailStop, None),
        }
    }
    pub fn observe<'scope, T>(Tracked(observations): Tracked<&mut super::super::observation_coverage::ObservationLedger<'scope, T>>,
        Ghost(key): Ghost<nat>, admission: &'scope Admission<'_>,
        Tracked(node): Tracked<&super::super::pin_ownership::cache_pins::Instance<super::super::heap_permission::HeapPermission<T>>>,
        Tracked(resident): Tracked<&super::super::pin_ownership::cache_pins::pins<super::super::heap_permission::HeapPermission<T>>>)
        requires admission.inv(), old(observations).inv(), !old(observations).frozen(), !old(observations).contains(key),
            old(observations).node_id() == node.id(), old(observations).domain() == node.domain(), node.domain().contains(admission.gate_id()),
            resident.instance_id() == node.id(), resident.element().1 == super::super::pin_ownership::PinKind::Resident,
        ensures final(observations).inv(), final(observations).contains(key), final(observations).len() == old(observations).len() + 1,
            final(observations).node_id() == old(observations).node_id(), final(observations).domain() == old(observations).domain(),
            final(observations).coverage() == old(observations).coverage(), !final(observations).frozen(),
    {
        let Tracked(permit) = admission.permit();
        proof { observations.observe(key, permit, node, resident); }
    }
    }
    }
};
}
width!(word32, u32);
width!(word64, u64);

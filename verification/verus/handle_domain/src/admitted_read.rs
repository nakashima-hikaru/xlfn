//! Actual gate CAS -> retained scope -> AtomicPtr load -> pointer borrow -> release.
//! The caller still supplies the generation-selected counter/domain mapping.
use vstd::prelude::*;
use super::atomic_publication::{Slot, Read};
use super::rotation::drain::permit_shares::Scope;
use super::rotation::drain::transitions::TransitionOutcome;
macro_rules! width {
    ($module:ident) => {
    pub mod $module {
    use super::*;
    use super::super::rotation::drain::atomic_counter::$module::Counter;
    verus! {
    pub struct AdmittedRead<'a, T> {
        counter: &'a Counter,
        scope: Tracked<Scope>,
        read: Read<'a, T>,
    }
    pub enum Outcome<'a, T> { Rejected, FailStop, Empty, Live(AdmittedRead<'a, T>) }
    impl<'a, T> AdmittedRead<'a, T> {
        pub closed spec fn inv(&self) -> bool {
            self.counter.inv() && self.scope@.inv() && self.scope@.len() == 1
            && self.scope@.gate_id() == self.counter.id() && self.read.inv()
            && self.read.scope_id() == self.scope@.id() && self.read.gate_id() == self.counter.id()
        }
        pub closed spec fn gate_id(&self) -> vstd::tokens::InstanceId { self.counter.id() }
        pub closed spec fn value(&self) -> T { self.read.value() }
        pub fn borrow(&self) -> (value: &T)
            requires self.inv(), ensures *value == self.value(),
        { self.read.borrow() }
        #[verifier::exec_allows_no_decreases_clause]
        pub fn end(self)
            requires self.inv(),
        {
            let AdmittedRead { counter, scope: Tracked(mut scope), read } = self;
            read.end_in_scope(Tracked(&mut scope));
            counter.release_scope(Tracked(scope));
        }
    }
    #[verifier::exec_allows_no_decreases_clause]
    pub fn begin<'a, T>(counter: &'a Counter, slot: &'a Slot<T>) -> (result: Outcome<'a, T>)
        requires counter.inv(), slot.inv(), slot.domain().contains(counter.id()),
        ensures match result {
            Outcome::Live(read) => read.inv() && read.gate_id() == counter.id(),
            _ => true,
        },
    {
        let (outcome, Tracked(scope)) = counter.acquire_scope();
        match outcome {
            TransitionOutcome::Rejected => Outcome::Rejected,
            TransitionOutcome::FailStop => Outcome::FailStop,
            TransitionOutcome::Success(_) => {
                let tracked mut scope = scope.tracked_unwrap();
                match slot.load(Tracked(&mut scope)) {
                    Some(read) => Outcome::Live(AdmittedRead { counter, scope: Tracked(scope), read }),
                    None => { counter.release_scope(Tracked(scope)); Outcome::Empty },
                }
            },
        }
    }
    }
    }
};
}
width!(word32);
width!(word64);

//! Linear destructor-return receipts for the production-shared completion tail.
//! This verifies ownership/control flow, not arbitrary user Drop implementations.
use vstd::prelude::*;
use verus_state_machines_macros::tokenized_state_machine;
verus! {
tokenized_state_machine!(destruction {
    fields {
        #[sharding(constant)] pub count: nat,
        #[sharding(variable)] pub phase: nat,
        #[sharding(option)] pub returned: Option<()>,
    }
    #[invariant]
    pub fn receipt_matches_phase(&self) -> bool {
        self.phase <= 2 && self.returned == (if self.phase == 1 { Some(()) } else { None })
    }
    init! { initialize(count: nat) {
        init count = count; init phase = 0; init returned = None;
    } }
    transition! { destructor_returned() {
        require(pre.phase == 0); update phase = 1; add returned += Some(());
    } }
    transition! { discharge() {
        require(pre.phase == 1); remove returned -= Some(()); update phase = 2;
    } }
    #[inductive(initialize)] fn initialize_inductive(post: Self, count: nat) {}
    #[inductive(destructor_returned)] fn destructor_returned_inductive(pre: Self, post: Self) {}
    #[inductive(discharge)] fn discharge_inductive(pre: Self, post: Self) {}
});
}
macro_rules! width {
    ($module:ident, $word:ty) => {
    pub mod $module {
    use super::*;
    verus! {
    pub struct Completion<P> {
        records: Option<Vec<P>>,
        count: usize,
        debt: $word,
        locked: bool,
        notified: bool,
        instance: Tracked<destruction::Instance>,
        phase: Tracked<destruction::phase>,
    }
    impl<P> Completion<P> {
        pub closed spec fn inv(&self) -> bool {
            self.phase@.instance_id() == self.instance@.id()
            && self.instance@.count() == self.count as nat
            && self.phase@.value() <= 2
            && (self.phase@.value() < 2 ==> self.count as nat <= self.debt as nat)
            && (self.phase@.value() == 0 ==> self.records.is_some()
                && self.records.unwrap().len() == self.count)
            && (self.phase@.value() != 0 ==> self.records.is_none())
            && (self.locked ==> self.phase@.value() == 2)
            && (self.notified ==> self.phase@.value() == 2)
        }
        pub closed spec fn debt(&self) -> $word { self.debt }
        pub closed spec fn count(&self) -> nat { self.count as nat }
        pub closed spec fn phase(&self) -> nat { self.phase@.value() }
        pub closed spec fn locked(&self) -> bool { self.locked }
        pub closed spec fn notified(&self) -> bool { self.notified }

        pub fn new(records: Vec<P>, debt: $word) -> (state: Self)
            requires records.len() as nat <= debt as nat,
            ensures state.inv(), state.phase() == 0, state.debt() == debt,
                state.count() == records.len(), !state.locked(), !state.notified(),
        {
            let count = records.len();
            let tracked (Tracked(instance), Tracked(phase), Tracked(returned)) = destruction::Instance::initialize(count as nat);
            Completion { records: Some(records), count, debt, locked: false, notified: false,
                instance: Tracked(instance), phase: Tracked(phase) }
        }

        fn destroy(&mut self) -> (receipt: Tracked<destruction::returned>)
            requires old(self).inv(), old(self).phase() == 0, !old(self).locked(), !old(self).notified(),
            ensures final(self).inv(), final(self).phase() == 1,
                final(self).debt == old(self).debt, final(self).count == old(self).count,
                !final(self).locked(), !final(self).notified(),
                receipt@.instance_id() == final(self).instance@.id(),
        {
            // This scope models normal destructor return. Verus does not
            // verify P::drop or native Box recovery; no trusted Drop spec is added.
            { let _records = self.records.take().unwrap(); }
            let tracked receipt = self.instance.borrow().destructor_returned(self.phase.borrow_mut());
            Tracked(receipt)
        }

        fn discharge(&mut self, Tracked(receipt): Tracked<destruction::returned>)
            requires old(self).inv(), old(self).phase() == 1, !old(self).locked(), !old(self).notified(),
                receipt.instance_id() == old(self).instance@.id(),
            ensures final(self).inv(), final(self).phase() == 2,
                final(self).debt as nat + old(self).count as nat == old(self).debt as nat,
                final(self).count == old(self).count, !final(self).locked(), !final(self).notified(),
        {
            match super::super::counters::$module::subtract(self.debt, self.count as $word) {
                super::super::counters::CountStep::Success(next) => self.debt = next,
                super::super::counters::CountStep::FailStop => { assert(false); },
            }
            proof { self.instance.borrow().discharge(self.phase.borrow_mut(), receipt); }
        }

        fn lock(&mut self)
            requires old(self).inv(), old(self).phase() == 2, !old(self).locked(), !old(self).notified(),
            ensures final(self).inv(), final(self).locked(), !final(self).notified(),
                final(self).debt == old(self).debt, final(self).count == old(self).count,
        { self.locked = true; }

        fn notify(&mut self)
            requires old(self).inv(), old(self).locked(), !old(self).notified(),
            ensures final(self).inv(), final(self).locked(), final(self).notified(),
                final(self).debt == old(self).debt, final(self).count == old(self).count,
        { self.notified = true; }

        fn unlock(&mut self, _guard: ())
            requires old(self).inv(), old(self).locked(), old(self).notified(),
            ensures final(self).inv(), !final(self).locked(), final(self).notified(),
                final(self).debt == old(self).debt, final(self).count == old(self).count,
        { self.locked = false; }
    }

    /// Relate the shared completion tail to the corrected destruction-in-flight
    /// model without assigning debt zero or ignoring other reclaimers' batches.
    pub fn complete_refines<P>(state: &mut Completion<P>, Ghost(model): Ghost<super::super::HandleDomainState>)
        requires old(state).inv(), old(state).phase() == 0, !old(state).locked(), !old(state).notified(),
            super::super::handle_domain_inv(model), model.debt == old(state).debt() as nat,
            old(state).count() <= model.reclaiming_bindings,
        ensures final(state).inv(), final(state).phase() == 2, !final(state).locked(), final(state).notified(),
            super::super::step_complete_destruction(model, old(state).count()).is_some(),
            super::super::handle_domain_inv(super::super::step_complete_destruction(model, old(state).count()).unwrap()),
            final(state).debt() as nat == super::super::step_complete_destruction(model, old(state).count()).unwrap().debt,
    {
        complete(state);
        proof { super::super::hd_step_complete_destruction_preserves_inv(model, old(state).count(),
            super::super::step_complete_destruction(model, old(state).count()).unwrap()); }
    }

    pub fn complete<P>(state: &mut Completion<P>)
        requires old(state).inv(), old(state).phase() == 0, !old(state).locked(), !old(state).notified(),
        ensures final(state).inv(), final(state).phase() == 2,
            final(state).debt() as nat + old(state).count() == old(state).debt() as nat,
            !final(state).locked(), final(state).notified(),
    {
        super::super::completion_protocol::complete_reclamation!(receipt, guard;
            state.destroy(), state.discharge(receipt), state.lock(), state.notify(), state.unlock(guard));
    }
    }
    }
    };
}
width!(word32, u32);
width!(word64, u64);

//! Resource-preserving collection of actual per-stripe drain observations.
use vstd::prelude::*;
use super::atomic_counter::{lifecycle, DrainSet};
use super::protocol::protocol_expr;
macro_rules! width {
    ($module:ident) => {
    pub mod $module {
    use super::*;
    use super::super::atomic_counter::$module::Counter;
    verus! {
    pub struct Collection {
        completed: Vec<bool>,
        controls: Tracked<Map<nat, lifecycle::control>>,
        drains: Tracked<DrainSet>,
    }
    pub open spec fn distinct(counters: Seq<Counter>) -> bool {
        forall|i: int, j: int| 0 <= i < counters.len() && 0 <= j < counters.len() && i != j
            ==> (#[trigger] counters[i].id()) != (#[trigger] counters[j].id())
    }
    pub open spec fn indices(counters: Seq<Counter>) -> Set<nat> { Seq::new(counters.len(), |i: int| i as nat).to_set() }
    pub open spec fn gate_ids(counters: Seq<Counter>) -> Set<vstd::tokens::InstanceId> {
        counters.map(|i: int, counter: Counter| counter.id()).to_set()
    }
    proof fn index_bounds(counters: Seq<Counter>)
        ensures forall|i: nat| #[trigger] indices(counters).contains(i) <==> i < counters.len(),
    {
        let positions = Seq::new(counters.len(), |i: int| i as nat);
        assert forall|i: nat| #[trigger] indices(counters).contains(i) <==> i < counters.len() by {
            if indices(counters).contains(i) {
                let j = choose|j: int| 0 <= j < positions.len() && positions[j] == i;
                assert(positions[j] == j as nat);
            } else if i < counters.len() { assert(positions[i as int] == i); }
        };
    }
    proof fn contains_gate(counters: Seq<Counter>, i: int)
        requires 0 <= i < counters.len(),
        ensures gate_ids(counters).contains(counters[i].id()),
    {
        let ids = counters.map(|i: int, counter: Counter| counter.id());
        assert(ids[i] == counters[i].id());
    }
    pub open spec fn controls_match(counters: Seq<Counter>, controls: Map<nat, lifecycle::control>) -> bool {
        controls.dom() == indices(counters)
        && (forall|i: int| 0 <= i < counters.len() ==> #[trigger] controls.dom().contains(i as nat)
            && controls[i as nat].instance_id() == counters[i].authority_id() && controls[i as nat].value())
    }
    pub open spec fn control_row(counters: Seq<Counter>, controls: Map<nat, lifecycle::control>, i: int) -> bool {
        controls.dom().contains(i as nat) && controls[i as nat].instance_id() == counters[i].authority_id()
    }
    pub open spec fn controls_owned(counters: Seq<Counter>, controls: Map<nat, lifecycle::control>) -> bool {
        controls.dom() == indices(counters)
            && (forall|i: int| 0 <= i < counters.len() ==> #[trigger] control_row(counters, controls, i))
    }
    pub fn seal_all(counters: &Vec<Counter>, Tracked(controls): Tracked<&mut Map<nat, lifecycle::control>>)
        requires controls_owned(counters@, *old(controls)),
            forall|i: int| 0 <= i < counters.len() ==> (#[trigger] counters@[i]).inv(),
        ensures controls_match(counters@, *final(controls)),
    {
        super::super::protocol::seal_all_stripes!(index; counters.len(), vstd::prelude::verus_exec_expr!({
            assert(control_row(counters@, *controls, index as int));
            let ghost before = *controls;
            let tracked mut control = controls.tracked_remove(index as nat);
            counters[index].seal(Tracked(&mut control));
            proof { controls.tracked_insert(index as nat, control); }
            assert forall|i: int| 0 <= i < counters.len() implies #[trigger] control_row(counters@, *controls, i) by {
                assert(control_row(counters@, before, i));
            };
        });
            invariant index <= counters.len(), controls_owned(counters@, *controls),
                forall|i: int| 0 <= i < counters.len() ==> (#[trigger] counters@[i]).inv(),
                forall|i: int| 0 <= i < index ==> #[trigger] controls.dom().contains(i as nat) && controls[i as nat].value(),
            decreases counters.len() - index,
        );
        assert forall|i: int| 0 <= i < counters.len() implies #[trigger] controls.dom().contains(i as nat)
            && controls[i as nat].instance_id() == counters@[i].authority_id() && controls[i as nat].value() by {
            assert(control_row(counters@, *controls, i));
        };
    }
    fn reopen_one(counters: &Vec<Counter>, index: usize,
        Tracked(controls): Tracked<&mut Map<nat, lifecycle::control>>) -> (opened: bool)
        requires index < counters.len(), counters@[index as int].inv(), old(controls).dom().contains(index as nat),
            old(controls)[index as nat].instance_id() == counters@[index as int].authority_id(), old(controls)[index as nat].value(),
        ensures final(controls).dom() == old(controls).dom(),
            final(controls)[index as nat].instance_id() == counters@[index as int].authority_id(),
            final(controls)[index as nat].value() == !opened,
            forall|j: nat| j != index && old(controls).dom().contains(j) ==> (#[trigger] final(controls)[j]) == old(controls)[j],
    {
        let tracked mut control = controls.tracked_remove(index as nat);
        let opened = counters[index].reopen(Tracked(&mut control));
        proof { controls.tracked_insert(index as nat, control); }
        opened
    }
    pub open spec fn reopen_row(counters: Seq<Counter>, controls: Map<nat, lifecycle::control>, i: int, opened: nat) -> bool {
        controls.dom().contains(i as nat) && controls[i as nat].instance_id() == counters[i].authority_id()
            && controls[i as nat].value() == (i >= opened)
    }
    /// A rejected stripe and its suffix retain sealed controllers.
    pub fn reopen_all(counters: &Vec<Counter>, Tracked(controls): Tracked<&mut Map<nat, lifecycle::control>>) -> (opened: usize)
        requires controls_match(counters@, *old(controls)),
            forall|i: int| 0 <= i < counters.len() ==> (#[trigger] counters@[i]).inv(),
        ensures opened <= counters.len(), final(controls).dom() == old(controls).dom(),
            forall|i: int| 0 <= i < counters.len() ==> #[trigger] reopen_row(counters@, *final(controls), i, opened as nat),
            forall|j: nat| j >= counters.len() && old(controls).dom().contains(j)
                ==> (#[trigger] final(controls)[j]) == old(controls)[j],
    {
        assert forall|i: int| 0 <= i < counters.len() implies #[trigger] reopen_row(counters@, *controls, i, 0) by {
            assert(controls.dom().contains(i as nat));
        };
        let ghost mut rejected = false;
        let opened_count = super::super::protocol::reopen_stripes!(index; counters.len(), vstd::prelude::verus_exec_expr!({
            assert(reopen_row(counters@, *controls, index as int, index as nat));
            let ghost before = *controls;
            let opened = reopen_one(counters, index, Tracked(controls));
            proof { rejected = !opened; }
            let ghost next = index as nat + if opened { 1nat } else { 0nat };
            assert forall|i: int| 0 <= i < counters.len() implies #[trigger] reopen_row(counters@, *controls, i, next) by {
                assert(reopen_row(counters@, before, i, index as nat));
                if i != index { assert(controls[i as nat] == before[i as nat]); }
            };
            opened
        });
            invariant_except_break !rejected,
            invariant index <= counters.len(), controls.dom() == old(controls).dom(),
                forall|i: int| 0 <= i < counters.len() ==> (#[trigger] counters@[i]).inv(),
                forall|i: int| 0 <= i < counters.len() ==> #[trigger] reopen_row(counters@, *controls, i, index as nat),
                forall|j: nat| j >= counters.len() && old(controls).dom().contains(j)
                    ==> (#[trigger] controls[j]) == old(controls)[j],
            ensures index == counters.len() || rejected,
            decreases counters.len() - index,
        );
        assert(opened_count == counters.len() || rejected);
        opened_count
    }
    impl Collection {
        closed spec fn row(&self, counters: Seq<Counter>, i: int) -> bool {
            counters[i].inv()
            && (self.completed@[i] == self.drains@.domain().contains(counters[i].id()))
            && (self.controls@.dom().contains(i as nat) == !self.completed@[i])
            && (self.completed@[i] ==> counters[i].accepts_lease(self.drains@.at(counters[i].id())))
            && (!self.completed@[i] ==> self.controls@[i as nat].instance_id() == counters[i].authority_id()
                && self.controls@[i as nat].value())
        }
        pub closed spec fn inv(&self, counters: Seq<Counter>) -> bool {
            self.completed.len() == counters.len() && distinct(counters) && self.drains@.inv()
            && self.controls@.dom().subset_of(indices(counters))
            && self.drains@.domain().subset_of(gate_ids(counters))
            && (forall|i: int| 0 <= i < counters.len() ==> #[trigger] self.row(counters, i))
        }
        pub closed spec fn ready(&self, index: int) -> bool { self.completed@[index] }
        pub fn new(counters: &Vec<Counter>, Tracked(controls): Tracked<Map<nat, lifecycle::control>>) -> (collection: Self)
            requires distinct(counters@), controls.dom() == indices(counters@),
                forall|i: int| #![auto] 0 <= i < counters.len() ==> counters@[i].inv()
                    && controls.dom().contains(i as nat) && controls[i as nat].instance_id() == counters@[i].authority_id() && controls[i as nat].value(),
            ensures collection.inv(counters@), forall|i: int| 0 <= i < counters.len() ==> !collection.ready(i),
        {
            let mut completed: Vec<bool> = Vec::new();
            while completed.len() < counters.len()
                invariant completed.len() <= counters.len(), forall|i: int| 0 <= i < completed.len() ==> !completed@[i],
                decreases counters.len() - completed.len(),
            { completed.push(false); }
            let collection = Collection { completed, controls: Tracked(controls), drains: Tracked(DrainSet::empty()) };
            assert forall|i: int| 0 <= i < counters.len() implies #[trigger] collection.row(counters@, i) by {};
            collection
        }
        fn poll(&mut self, counters: &Vec<Counter>, index: usize) -> (busy: usize)
            requires old(self).inv(counters@), index < counters.len(),
            ensures final(self).inv(counters@), (busy == 0) == final(self).ready(index as int),
                forall|j: int| 0 <= j < counters.len() && j != index ==> final(self).ready(j) == old(self).ready(j),
        {
            proof { index_bounds(counters@); contains_gate(counters@, index as int); }
            assert(self.row(counters@, index as int));
            if self.completed[index] { return 0; }
            let tracked control = self.controls.borrow_mut().tracked_remove(index as nat);
            match counters[index].try_drain(Tracked(control)) {
                Ok(lease) => {
                    proof { self.drains.borrow_mut().insert(lease.get()); }
                    self.completed.set(index, true);
                    assert forall|j: int| 0 <= j < counters.len() implies #[trigger] self.row(counters@, j) by {
                        assert(old(self).row(counters@, j));
                        if j != index { assert(counters@[j].id() != counters@[index as int].id()); }
                    };
                    0
                },
                Err(control) => {
                    proof { self.controls.borrow_mut().tracked_insert(index as nat, control.get()); }
                    assert forall|j: int| 0 <= j < counters.len() implies #[trigger] self.row(counters@, j) by {
                        assert(old(self).row(counters@, j));
                    };
                    1
                },
            }
        }
        pub fn poll_all(&mut self, counters: &Vec<Counter>) -> (idle: bool)
            requires old(self).inv(counters@),
            ensures final(self).inv(counters@), idle == (forall|i: int| 0 <= i < counters.len() ==> final(self).ready(i)),
        {
            super::super::protocol::observe_stripes!(index, idle; counters.len(), self.poll(counters, index);
                invariant self.inv(counters@), index <= counters.len(),
                    idle == (forall|i: int| 0 <= i < index ==> self.ready(i)),
                decreases counters.len() - index,
            )
        }
        fn restore_one(&mut self, counters: &Vec<Counter>, index: usize)
            requires old(self).inv(counters@), index < counters.len(),
            ensures final(self).inv(counters@), !final(self).ready(index as int),
                forall|j: int| 0 <= j < counters.len() && j != index ==> final(self).ready(j) == old(self).ready(j),
        {
            proof { index_bounds(counters@); contains_gate(counters@, index as int); }
            assert(self.row(counters@, index as int));
            if self.completed[index] {
                let tracked lease = self.drains.borrow_mut().remove(counters@[index as int].id());
                let control = counters[index].restore(Tracked(lease));
                proof { self.controls.borrow_mut().tracked_insert(index as nat, control.get()); }
                self.completed.set(index, false);
                assert forall|j: int| 0 <= j < counters.len() implies #[trigger] self.row(counters@, j) by {
                    assert(old(self).row(counters@, j));
                    if j != index { assert(counters@[j].id() != counters@[index as int].id()); }
                };
            }
        }
        pub fn restore_all(&mut self, counters: &Vec<Counter>)
            requires old(self).inv(counters@),
            ensures final(self).inv(counters@), forall|i: int| 0 <= i < counters.len() ==> !final(self).ready(i),
        {
            let mut index = 0;
            while index < counters.len()
                invariant self.inv(counters@), index <= counters.len(), forall|i: int| 0 <= i < index ==> !self.ready(i),
                decreases counters.len() - index,
            {
                self.restore_one(counters, index);
                index += 1;
            }
        }
        pub fn into_controls(self, counters: &Vec<Counter>) -> (result: Tracked<Map<nat, lifecycle::control>>)
            requires self.inv(counters@), forall|i: int| 0 <= i < counters.len() ==> !self.ready(i),
            ensures controls_match(counters@, result@),
        {
            assert forall|i: int| 0 <= i < counters.len() implies #[trigger] self.controls@.dom().contains(i as nat)
                && self.controls@[i as nat].instance_id() == counters@[i].authority_id() && self.controls@[i as nat].value() by {
                assert(self.row(counters@, i));
                assert(!self.ready(i));
            };
            proof {
                assert(self.drains@.domain() =~= Set::empty()) by {
                    assert forall|gate: vstd::tokens::InstanceId| !self.drains@.domain().contains(gate) by {
                        if self.drains@.domain().contains(gate) {
                            let ids = counters@.map(|i: int, counter: Counter| counter.id());
                            let i = choose|i: int| 0 <= i < ids.len() && ids[i] == gate;
                            assert(self.row(counters@, i));
                            assert(!self.ready(i));
                        }
                    };
                };
                index_bounds(counters@);
                assert(self.controls@.dom() =~= indices(counters@)) by {
                    assert forall|i: nat| indices(counters@).contains(i) implies self.controls@.dom().contains(i) by {
                        assert(self.row(counters@, i as int));
                    };
                };
            }
            self.controls
        }
        pub fn drains<'a>(&'a self, counters: &Vec<Counter>) -> (drains: Tracked<&'a DrainSet>)
            requires self.inv(counters@), forall|i: int| 0 <= i < counters.len() ==> self.ready(i),
            ensures drains@.inv(), drains@.domain() == gate_ids(counters@),
                forall|i: int| #![auto] 0 <= i < counters.len() ==> drains@.domain().contains(counters@[i].id()),
        {
            assert forall|i: int| #![auto] 0 <= i < counters.len() implies self.drains@.domain().contains(counters@[i].id()) by {
                assert(self.row(counters@, i));
                assert(self.ready(i));
            };
            assert(self.drains@.domain() =~= gate_ids(counters@)) by {
                assert forall|gate: vstd::tokens::InstanceId| gate_ids(counters@).contains(gate)
                    implies self.drains@.domain().contains(gate) by {
                    let ids = counters@.map(|i: int, counter: Counter| counter.id());
                    let i = choose|i: int| 0 <= i < ids.len() && ids[i] == gate;
                    assert(self.row(counters@, i));
                    assert(self.ready(i));
                };
            };
            Tracked(self.drains.borrow())
        }
    }
    }
    }
};
}
width!(word32);
width!(word64);

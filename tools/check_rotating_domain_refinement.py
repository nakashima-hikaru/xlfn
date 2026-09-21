#!/usr/bin/env python3
"""Reject unsafe ordering changes in production-shared rotation control flow."""

from pathlib import Path

from check_drain_gate_refinement import check_mutations


MUTATIONS = {
    "terminal detachment skips domain authorization": (
        "$authorize:expr, $lock_first:expr, $lock_second:expr, $take:expr) => {{\n        match $authorize {",
        "$authorize:expr, $lock_first:expr, $lock_second:expr, $take:expr) => {{\n        match Some([0usize, 1usize]) {",
    ),
    "terminal detachment locks the first queue twice": (
        "let $second = $lock_second;", "let $second = $lock_first;",
    ),
    "queue detached without certificate authorization": (
        "($index:ident, $guard:ident; $authorize:expr, $lock:expr, $take:expr) => {{\n        match $authorize {",
        "($index:ident, $guard:ident; $authorize:expr, $lock:expr, $take:expr) => {{\n        match Some(0usize) {",
    ),
    "certificate detaches the other generation queue": (
        "Some($index) => {\n                let $guard = $lock;",
        "Some($index) => {\n                let $index = 1usize - $index;\n                let $guard = $lock;",
    ),
    "drain certificate authorizes a foreign domain": (
        "if issuer == owner", "if true",
    ),
    "reader constructs permit without acquiring": (
        "match $acquire {", "match Ok::<(), ()>(()) {",
    ),
    "reader returns permit for other generation": (
        "Ok(()) => return Ok($permit),",
        "Ok(()) => { let $generation = !$generation; return Ok($permit); },",
    ),

    "registration skips generation recheck": (
        "if $current != $generation", "if false",
    ),
    "registration retries without releasing stale queue": (
        "                $unlock;", "                ();",
    ),
    "publication before seal": ("        $seal;", "        ();"),
    "publication without pending registration": ("        $pending;", "        ();"),
    "reopen before publication": (
        "$publish;\n        $between;\n        $reopen;",
        "$reopen;\n        $between;\n        $publish;",
    ),
    "clear pending before callback": (
        "let $result = $operation;\n        $clear;",
        "$clear;\n        let $result = $operation;",
    ),
    "finish without idle observation": ("if !$observe", "if false"),
    "seal without closing writer admission": ("        $close;", "        ();"),
}


if __name__ == "__main__":
    check_mutations(
        Path("verification/verus/rotating_read_domain"),
        Path("crates/xlfn-kernel/src/rotating_read_domain/protocol.rs"),
        (Path("crates/xlfn/src/retirement_queue.rs"), Path("crates/xlfn-kernel/src/sealable_counter/transitions.rs"),
         *tuple(Path("verification/verus/drain_gate/src").glob("*.rs")), Path("crates/xlfn-kernel/src/drain_gate/protocol.rs")),
        MUTATIONS,
    )

    check_mutations(
        Path("verification/verus/rotating_read_domain"),
        Path("verification/verus/rotating_read_domain/src/refinement.rs"),
        (Path("crates/xlfn/src/retirement_queue.rs"), Path("crates/xlfn-kernel/src/rotating_read_domain/protocol.rs"),
         Path("crates/xlfn-kernel/src/sealable_counter/transitions.rs"),
         *tuple(Path("verification/verus/drain_gate/src").glob("*.rs")), Path("crates/xlfn-kernel/src/drain_gate/protocol.rs")),
        {
            "reader receives the other generation ledger permit": (
                "permit = Some(ledgers.one.acquire(&mut ledgers.active_one));",
                "permit = Some(ledgers.zero.acquire(&mut ledgers.active_zero));",
            ),
            "release decrements the other generation counter": (
                "self.reader_release(selected);", "self.reader_release(!selected);",
            ),
            "release accepts a foreign generation permit": (
                "permit.instance_id() == old(ledgers).selected_id(selected),",
                "true,",
            ),
        },
    )

    check_mutations(
        Path("verification/verus/rotating_read_domain"),
        Path("verification/verus/rotating_read_domain/src/registration.rs"),
        (Path("crates/xlfn/src/retirement_queue.rs"), Path("crates/xlfn-kernel/src/rotating_read_domain/protocol.rs"),
         Path("crates/xlfn-kernel/src/sealable_counter/transitions.rs"),
         Path("crates/xlfn-kernel/src/drain_gate/protocol.rs"),
         *tuple(Path("verification/verus/drain_gate/src").glob("*.rs"))),
        {
            "registration loses held-state authority before append": (
                "model.append(selected, guard, payload);",
                "protocol_expr!({ model.held = None; model.append(selected, guard, payload) });",
            ),
            "retirement payload appended to the other generation": (
                "if selected { super::queue_transitions::append_retired!(&mut self.one, payload); } else { super::queue_transitions::append_retired!(&mut self.zero, payload); }",
                "if selected { super::queue_transitions::append_retired!(&mut self.zero, payload); } else { super::queue_transitions::append_retired!(&mut self.one, payload); }",
            ),
            "retirement payload dropped instead of queued": (
                "super::queue_transitions::append_retired!(&mut self.one, payload);", "();",
            ),
        },
    )

    check_mutations(
        Path("verification/verus/rotating_read_domain"),
        Path("verification/verus/rotating_read_domain/src/identity.rs"),
        (Path("crates/xlfn/src/retirement_queue.rs"), Path("crates/xlfn-kernel/src/rotating_read_domain/protocol.rs"),
         Path("crates/xlfn-kernel/src/sealable_counter/transitions.rs"),
         Path("crates/xlfn-kernel/src/drain_gate/protocol.rs"),
         *tuple(Path("verification/verus/drain_gate/src").glob("*.rs"))),
        {
            "terminal certificate issued before both generations drain": (
                "state.sealed(false), state.sealed(true), state.idle(false), state.idle(true),",
                "state.sealed(false), state.sealed(true), state.idle(false),",
            ),
            "drain certificate uses another exclusive lock handle": (
                "handle.rwlock() == *lock, lock.pred().domain == state.domain,",
                "lock.pred().domain == state.domain,",
            ),
            "drain certificate lock protects another domain": (
                "handle.rwlock() == *lock, lock.pred().domain == state.domain,",
                "handle.rwlock() == *lock,",
            ),
            "drain certificate issued before idle": (
                "state.pending == Some(index == 1), state.idle(index == 1),",
                "state.pending == Some(index == 1),",
            ),
            "drain certificate issued for another domain": (
                "requires index < 2, issuer == state.domain, state.inv(), state.locked,",
                "requires index < 2, state.inv(), state.locked,",
            ),
            "drain certificate issued without transition lock": (
                "requires index < 2, issuer == state.domain, state.inv(), state.locked,",
                "requires index < 2, issuer == state.domain, state.inv(),",
            ),
        },
    )

    check_mutations(
        Path("verification/verus/rotating_read_domain"),
        Path("verification/verus/rotating_read_domain/src/lock_ownership.rs"),
        (Path("crates/xlfn/src/retirement_queue.rs"), Path("crates/xlfn-kernel/src/rotating_read_domain/protocol.rs"),
         Path("crates/xlfn-kernel/src/sealable_counter/transitions.rs"),
         Path("crates/xlfn-kernel/src/drain_gate/protocol.rs"),
         *tuple(Path("verification/verus/drain_gate/src").glob("*.rs"))),
        {
            "exclusive path issues a certificate for active readers": (
                "if raw & $mask == 0 {", "if true {",
            ),
            "transition release returns state under another domain": (
                "state.domain == handle.rwlock().pred().domain,", "true,",
            ),
        },
    )

    check_mutations(
        Path("verification/verus/rotating_read_domain"),
        Path("verification/verus/rotating_read_domain/src/barrier_ownership.rs"),
        (Path("crates/xlfn/src/retirement_queue.rs"), Path("crates/xlfn-kernel/src/rotating_read_domain/protocol.rs"),
         Path("crates/xlfn-kernel/src/sealable_counter/transitions.rs"),
         Path("crates/xlfn-kernel/src/drain_gate/protocol.rs"),
         *tuple(Path("verification/verus/drain_gate/src").glob("*.rs"))),
        {
            "barrier certificate borrows a different queue lock": (
                "requires handle.rwlock() == *lock, lock.inv(*queue),",
                "requires lock.inv(*queue),",
            ),
            "barrier certificate accepts unprotected queue state": (
                "requires handle.rwlock() == *lock, lock.inv(*queue),",
                "requires handle.rwlock() == *lock,",
            ),
            "publication locks a foreign domain queue": (
                "lock.pred().domain == old(model).domain, lock.pred().index == old(model).current,",
                "lock.pred().index == old(model).current,",
            ),
            "publication locks the next generation queue": (
                "lock.pred().domain == old(model).domain, lock.pred().index == old(model).current,",
                "lock.pred().domain == old(model).domain, lock.pred().index != old(model).current,",
            ),
        },
    )

    check_mutations(
        Path("verification/verus/rotating_read_domain"),
        Path("verification/verus/rotating_read_domain/src/refinement.rs"),
        (Path("crates/xlfn/src/retirement_queue.rs"), Path("crates/xlfn-kernel/src/rotating_read_domain/protocol.rs"),
         Path("crates/xlfn-kernel/src/sealable_counter/transitions.rs"),
         Path("crates/xlfn-kernel/src/drain_gate/protocol.rs"),
         *tuple(Path("verification/verus/drain_gate/src").glob("*.rs"))),
        {
            "transition model releases barrier before publication": (
                "publish_reopen!(model.publish(), model.publication_window(), model.reopen()),\n            model.release_barrier()",
                "model.release_barrier(),\n            publish_reopen!(model.publish(), model.publication_window(), model.reopen())",
            ),
        },
    )

    check_mutations(
        Path("verification/verus/rotating_read_domain"),
        Path("verification/verus/rotating_read_domain/src/locked_detachment.rs"),
        (Path("crates/xlfn/src/retirement_queue.rs"), Path("crates/xlfn-kernel/src/rotating_read_domain/protocol.rs"),
         Path("crates/xlfn-kernel/src/sealable_counter/transitions.rs"),
         Path("crates/xlfn-kernel/src/drain_gate/protocol.rs"),
         *tuple(Path("verification/verus/drain_gate/src").glob("*.rs"))),
        {
            "locked detach accepts a different queue generation": (
                "requires index < 2, lock.pred().index == (index == 1),", "requires index < 2,",
            ),
            "locked detach loses the protected payload": (
                "let (mut queue, handle) = held;\n    let records = super::queue_transitions::take_retired!(&mut queue.records);", "let (mut queue, handle) = held;\n    let mut records = Vec::new();",
            ),
            "locked terminal detach exchanges queue payloads": (
                "[zero_records, one_records]", "[one_records, zero_records]",
            ),
            "locked terminal detach accepts another domain second queue": (
                "zero.pred().domain == owner, one.pred().domain == owner,", "zero.pred().domain == owner,",
            ),
        },
    )

    check_mutations(
        Path("verification/verus/rotating_read_domain"),
        Path("verification/verus/rotating_read_domain/src/locked_detachment.rs"),
        (Path("crates/xlfn/src/retirement_queue.rs"), Path("crates/xlfn-kernel/src/rotating_read_domain/protocol.rs"),
         Path("crates/xlfn-kernel/src/sealable_counter/transitions.rs"),
         Path("crates/xlfn-kernel/src/drain_gate/protocol.rs"),
         *tuple(Path("verification/verus/drain_gate/src").glob("*.rs"))),
        {
            "withdrawal receipt omits a protected record": (
                "let ghost source = held.0.records@;", "let ghost source = held.0.records@.drop_last();",
            ),
            "terminal receipt stamps the wrong generation": (
                "owner: Ghost(owner), index: Ghost(true) }", "owner: Ghost(owner), index: Ghost(false) }",
            ),
        },
    )

    check_mutations(
        Path("verification/verus/rotating_read_domain"),
        Path("verification/verus/rotating_read_domain/src/queue_preparation.rs"),
        (Path("crates/xlfn/src/retirement_queue.rs"), Path("crates/xlfn-kernel/src/rotating_read_domain/protocol.rs"),
         Path("crates/xlfn-kernel/src/sealable_counter/transitions.rs"),
         Path("crates/xlfn-kernel/src/drain_gate/protocol.rs"),
         *tuple(Path("verification/verus/drain_gate/src").glob("*.rs"))),
        {
            "preparation leaves the protected queue phase open": (
                "remove ready -= Some(()); update state = Some(bound);", "remove ready -= Some(()); update state = None;",
            ),
            "reset leaves the protected queue phase prepared": (
                "remove prepared -= Some(bound); update state = None;", "remove prepared -= Some(bound); update state = Some(bound);",
            ),
            "preparation authority disagrees with protected state": (
                "self.prepared == self.state && self.ready.is_some() == self.state.is_none()", "true",
            ),
        },
    )

    check_mutations(
        Path("verification/verus/rotating_read_domain"),
        Path("verification/verus/rotating_read_domain/src/locked_detachment.rs"),
        (Path("crates/xlfn/src/retirement_queue.rs"), Path("crates/xlfn-kernel/src/rotating_read_domain/protocol.rs"),
         Path("crates/xlfn-kernel/src/sealable_counter/transitions.rs"),
         Path("crates/xlfn-kernel/src/drain_gate/protocol.rs"),
         *tuple(Path("verification/verus/drain_gate/src").glob("*.rs"))),
        {
            "prepared detachment accepts another queue ticket": (
                "prepared.instance_id() == lock.pred().preparation.id(),", "true,",
            ),
            "prepared detachment resets before emptying queue": (
                "let records = super::queue_transitions::take_retired!(&mut queue.records);\n    let ready = super::barrier_ownership::reset_empty(&mut queue, lock, Tracked(prepared));",
                "let ready = super::barrier_ownership::reset_empty(&mut queue, lock, Tracked(prepared));\n    let records = super::queue_transitions::take_retired!(&mut queue.records);",
            ),
        },
    )

    check_mutations(
        Path("verification/verus/rotating_read_domain"), Path("crates/xlfn/src/retirement_queue.rs"),
        (Path("crates/xlfn-kernel/src/rotating_read_domain/protocol.rs"),
         Path("crates/xlfn-kernel/src/sealable_counter/transitions.rs"),
         Path("crates/xlfn-kernel/src/drain_gate/protocol.rs"),
         *tuple(Path("verification/verus/drain_gate/src").glob("*.rs"))),
        {
            "production append discards retirement": ("($queue).push($payload);", "let _payload = $payload;"),
            "production take omits the ownership transfer": ("core::mem::swap($queue, &mut records);", "let _queue = $queue;"),
        },
    )

    check_mutations(
        Path("verification/verus/rotating_read_domain"), Path("verification/verus/rotating_read_domain/src/current_atomic.rs"),
        (Path("crates/xlfn/src/retirement_queue.rs"), Path("crates/xlfn-kernel/src/rotating_read_domain/protocol.rs"),
         Path("crates/xlfn-kernel/src/sealable_counter/transitions.rs"), Path("crates/xlfn-kernel/src/drain_gate/protocol.rs"),
         *tuple(Path("verification/verus/drain_gate/src").glob("*.rs"))),
        {
            "atomic current ready authority belongs to another queue": (
                "&& (g.ready.is_some() ==> g.ready.unwrap().instance_id() == if value { k.1 } else { k.0 })", "&& true",
            ),
            "atomic preparation custody loses its queue identity": (
                "&& (g.held.is_some() ==> g.held.unwrap().instance_id() == if value { k.1 } else { k.0 })", "&& true",
            ),
            "current recheck treats another queue as open": ("if value == selected {", "if true {"),
            "publication installs old queue readiness for new generation": (
                "next_ready.instance_id() == self.gate(!reserved.index()),", "next_ready.instance_id() == self.gate(reserved.index()),",
            ),
            "publication reservation loses its selected generation": (
                "self.reservation@.instance_id() == current.authority@.id() && self.reservation@.value() == self.index", "self.reservation@.instance_id() == current.authority@.id()",
            ),
        },
    )

    check_mutations(
        Path("verification/verus/rotating_read_domain"), Path("verification/verus/rotating_read_domain/src/atomic_rotation.rs"),
        (Path("crates/xlfn/src/retirement_queue.rs"), Path("crates/xlfn-kernel/src/rotating_read_domain/protocol.rs"),
         Path("crates/xlfn-kernel/src/sealable_counter/transitions.rs"), Path("crates/xlfn-kernel/src/drain_gate/protocol.rs"),
         *tuple(Path("verification/verus/drain_gate/src").glob("*.rs"))),
        {
            "atomic rotation accepts an unrelated transition handle": (
                "requires transition.inv(*old(state)), handle.rwlock() == *transition, old(state).pending().is_none(),",
                "requires transition.inv(*old(state)), old(state).pending().is_none(),",
            ),
            "atomic rotation loses pending sealed authority": (
                "&& (state.pending.is_some() ==> state.sealed(state.pending.unwrap()))", "&& true",
            ),
            "atomic rotation seals a different counter instance": (
                "zero.inv(), one.inv(), transition.pred().zero == zero.authority_id(), transition.pred().one == one.authority_id(),",
                "zero.inv(), one.inv(), transition.pred().one == one.authority_id(),",
            ),
            "atomic rotation reserves a different generation": (
                "reserved.inv(current, queue_lock), reserved.index() == index,", "reserved.inv(current, queue_lock),",
            ),
        },
    )

    check_mutations(
        Path("verification/verus/rotating_read_domain"), Path("verification/verus/rotating_read_domain/src/striped_rotation.rs"),
        (Path("crates/xlfn/src/retirement_queue.rs"), Path("crates/xlfn-kernel/src/rotating_read_domain/protocol.rs"),
         Path("crates/xlfn-kernel/src/sealable_counter/transitions.rs"), Path("crates/xlfn-kernel/src/drain_gate/protocol.rs"),
         *tuple(Path("verification/verus/drain_gate/src").glob("*.rs"))),
        {
            "striped rotation accepts a foreign transition handle": (
                "requires transition.inv(*old(state)), handle.rwlock() == *transition, old(state).pending().is_none(),",
                "requires transition.inv(*old(state)), old(state).pending().is_none(),",
            ),
            "striped rotation loses pending sealed controllers": (
                "&& (state.pending.is_some() ==> state.sealed(self.counters(state.pending.unwrap()), state.pending.unwrap()))", "&& true",
            ),
            "striped rotation seals a different generation vector": (
                "zero@ == transition.pred().zero, one@ == transition.pred().one,", "one@ == transition.pred().one,",
            ),
            "striped rotation publishes a different reserved generation": (
                "reserved.inv(current, queue), reserved.index() == index,", "reserved.inv(current, queue),",
            ),
            "striped rotation reserves coverage for another stripe set": (
                "reserved.bound() == stripes::gate_ids(transition.pred().counters(index)),", "true,",
            ),
            "striped rotation returns after partial reopen": (
                "if opened != len { loop {} }", "",
            ),
        },
    )

    check_mutations(
        Path("verification/verus/rotating_read_domain"), Path("verification/verus/rotating_read_domain/src/striped_rotation.rs"),
        (Path("crates/xlfn/src/retirement_queue.rs"), Path("crates/xlfn-kernel/src/rotating_read_domain/protocol.rs"),
         Path("crates/xlfn-kernel/src/sealable_counter/transitions.rs"), Path("crates/xlfn-kernel/src/drain_gate/protocol.rs"),
         *tuple(Path("verification/verus/drain_gate/src").glob("*.rs"))),
        {
            "collection handoff accepts a foreign transition handle": (
                "requires transition.inv(state), handle.rwlock() == *transition, state.pending().is_none(),",
                "requires transition.inv(state), state.pending().is_none(),",
            ),
            "collection handoff restores an unrelated controller map": (
                "requires self.inv(), stripes::controls_match(self.next(), controls),", "requires self.inv(),",
            ),
            "collection handoff drains the wrong generation vector": (
                "next@ == transition.pred().counters(!index), state.sealed(next@, !index),", "state.sealed(next@, !index),",
            ),
            "collection handoff swaps old and next controllers": (
                "let (old_controls, next_controls) = if index { (one, zero) } else { (zero, one) };",
                "let (old_controls, next_controls) = if index { (zero, one) } else { (one, zero) };",
            ),
        },
    )

    check_mutations(
        Path("verification/verus/rotating_read_domain"), Path("verification/verus/rotating_read_domain/src/striped_rotation.rs"),
        (Path("crates/xlfn/src/retirement_queue.rs"), Path("crates/xlfn-kernel/src/rotating_read_domain/protocol.rs"),
         Path("crates/xlfn-kernel/src/sealable_counter/transitions.rs"), Path("crates/xlfn-kernel/src/drain_gate/protocol.rs"),
         *tuple(Path("verification/verus/drain_gate/src").glob("*.rs"))),
        {
            "collection cancellation skips actual lease restoration": (
                "collection.restore_all(counters);\n            let controllers", "let controllers",
            ),
            "collection cancellation restores another generation": (
                "requires self.inv(), counters@ == self.next(), collection.inv(counters@),",
                "requires self.inv(), collection.inv(counters@),",
            ),
        },
    )

    check_mutations(
        Path("verification/verus/rotating_read_domain"), Path("verification/verus/rotating_read_domain/src/striped_rotation.rs"),
        (Path("crates/xlfn/src/retirement_queue.rs"), Path("crates/xlfn-kernel/src/rotating_read_domain/protocol.rs"),
         Path("crates/xlfn-kernel/src/sealable_counter/transitions.rs"), Path("crates/xlfn-kernel/src/drain_gate/protocol.rs"),
         *tuple(Path("verification/verus/drain_gate/src").glob("*.rs"))),
        {
            "pending collection accepts the wrong pending state": (
                "requires transition.inv(state), handle.rwlock() == *transition, state.pending() == Some(index),",
                "requires transition.inv(state), handle.rwlock() == *transition,",
            ),
            "pending restoration clears callback state too early": (
                "if index { State { zero: live_controls, one: Tracked(controls), pending: Some(index) } }",
                "if index { State { zero: live_controls, one: Tracked(controls), pending: None } }",
            ),
            "pending restoration accepts another controller set": (
                "requires self.inv(), stripes::controls_match(self.counters(), controls),", "requires self.inv(),",
            ),
        },
    )

    check_mutations(
        Path("verification/verus/rotating_read_domain"), Path("verification/verus/rotating_read_domain/src/striped_rotation.rs"),
        (Path("crates/xlfn/src/retirement_queue.rs"), Path("crates/xlfn-kernel/src/rotating_read_domain/protocol.rs"),
         Path("crates/xlfn-kernel/src/sealable_counter/transitions.rs"), Path("crates/xlfn-kernel/src/drain_gate/protocol.rs"),
         *tuple(Path("verification/verus/drain_gate/src").glob("*.rs"))),
        {"completed pending callback never clears its state": ("            state.pending = None;", "")},
    )

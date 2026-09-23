#!/usr/bin/env python3
"""Check sensitivity of the storage-backed pin/observation ownership proof.

These mutate the ownership proof, not the production pin transition expressions.
"""

from pathlib import Path

from check_drain_gate_refinement import check_mutations


if __name__ == "__main__":
    check_mutations(
        Path("verification/verus/cache_lease"),
        Path("verification/verus/cache_lease/src/pin_ownership.rs"),
        (Path("crates/xlfn/src/retirement_queue.rs"), Path("crates/xlfn/src/cache/pin_transitions.rs"), Path("crates/xlfn/src/cache/node_layout.rs"),
         Path("verification/verus/published_owner/src/heap_permission.rs"), *tuple(Path("verification/verus/drain_gate/src").glob("*.rs")), Path("crates/xlfn-kernel/src/drain_gate/protocol.rs"), Path("crates/xlfn-kernel/src/rotating_read_domain/protocol.rs"),
         *tuple(Path("verification/verus/rotating_read_domain/src").glob("*.rs")), Path("crates/xlfn-kernel/src/sealable_counter/transitions.rs")),
        {
            "duplicated creator-to-lease capability": (
                "remove pins -= {(x, PinKind::Creator)};",
                "have pins >= {(x, PinKind::Creator)};",
            ),
            "retirement accepted from a nonfinal pin": (
                "require(pre.count == 1);",
                "require(pre.count >= 1);",
            ),
            "memory reclaimed while observed": (
                "require(pre.observing == 0);",
                "require(pre.observing >= 0);",
            ),
            "pin from a different allocation instance": (
                "requires pin.instance_id() == instance.id(),",
                "requires true,",
            ),
        },
    )

    check_mutations(
        Path("verification/verus/cache_lease"),
        Path("verification/verus/cache_lease/src/scope_ownership.rs"),
        (Path("crates/xlfn/src/retirement_queue.rs"), Path("crates/xlfn/src/cache/pin_transitions.rs"), Path("crates/xlfn/src/cache/node_layout.rs"),
         Path("verification/verus/published_owner/src/heap_permission.rs"),
         *tuple(Path("verification/verus/drain_gate/src").glob("*.rs")), Path("crates/xlfn-kernel/src/drain_gate/protocol.rs"), Path("crates/xlfn-kernel/src/rotating_read_domain/protocol.rs"),
         *tuple(Path("verification/verus/rotating_read_domain/src").glob("*.rs")),
         Path("crates/xlfn-kernel/src/sealable_counter/transitions.rs")),
        {
            "cache observation assigned to an unrelated stripe": (
                "requires ledgers.matches(old(raw)@), 0 <= index < ledgers.gates.len(),\n            observation.gate_id() == ledgers.gates[index].id(),",
                "requires ledgers.matches(old(raw)@), 0 <= index < ledgers.gates.len(),",
            ),
            "cache observation attributed to another drained generation": (
                "observation.gate_id() == ledgers.selected_id(selected),",
                "true,",
            ),
            "generation considered drained without zero observation": (
                "requires ledgers.matches(rotation), rotation.idle(selected),",
                "requires ledgers.matches(rotation),",
            ),
            "scope accepts a foreign domain permit": (
                "            node.domain().contains(permit.instance_id()),",
                "            true,",
            ),
            "allocation forgets its assigned domain": (
                "cache_pins::Instance::allocate(memory, domain, Some(memory))",
                "cache_pins::Instance::allocate(memory, Set::empty(), Some(memory))",
            ),
            "scope used to exclude idle in another gate": (
                "requires self.gate_id() == gate.id(), active.instance_id() == gate.id(),",
                "requires active.instance_id() == gate.id(),",
            ),
            "scoped read uses another node instance": (
                "requires observation.node_id() == node.id(), observation.memory().ptr() == ptr as *mut T,",
                "requires observation.memory().ptr() == ptr as *mut T,",
            ),
        },
    )

    check_mutations(
        Path("verification/verus/cache_lease"),
        Path("verification/verus/cache_lease/src/retirement.rs"),
        (Path("crates/xlfn/src/retirement_queue.rs"), Path("crates/xlfn/src/cache/pin_transitions.rs"), Path("crates/xlfn/src/cache/node_layout.rs"),
         Path("verification/verus/published_owner/src/heap_permission.rs"),
         Path("crates/xlfn-kernel/src/sealable_counter/transitions.rs"),
         Path("crates/xlfn-kernel/src/drain_gate/protocol.rs"),
         Path("crates/xlfn-kernel/src/rotating_read_domain/protocol.rs"),
         *tuple(Path("verification/verus/drain_gate/src").glob("*.rs")),
         *tuple(Path("verification/verus/rotating_read_domain/src").glob("*.rs"))),
        {
            "retirement entry accepts another node's ticket": (
                "requires ticket.instance_id() == node.id(), ticket.value().ptr() == pointer,",
                "requires ticket.value().ptr() == pointer,",
            ),
            "retirement enters a foreign domain queue": (
                "queue_domain(old(queue), domain), entry.domain() == domain,",
                "queue_domain(old(queue), domain),",
            ),
            "queued node recovered with outstanding observations": (
                "count.value() == 0, observing.value() == 0, retiring.value(),",
                "count.value() == 0, retiring.value(),",
            ),
            "final release leaves observation ledger open": (
                "                proof { observations.freeze(&entry); }",
                "                proof {}",
            ),
            "nonfinal release creates a retirement entry": (
                "Release::LastPin => {", "Release::StillPinned => {",
            ),
        },
    )


    check_mutations(
        Path("verification/verus/cache_lease"),
        Path("verification/verus/cache_lease/src/observation_coverage.rs"),
        (Path("crates/xlfn/src/retirement_queue.rs"), Path("crates/xlfn/src/cache/pin_transitions.rs"), Path("crates/xlfn/src/cache/node_layout.rs"),
         Path("verification/verus/published_owner/src/heap_permission.rs"),
         Path("crates/xlfn-kernel/src/sealable_counter/transitions.rs"),
         Path("crates/xlfn-kernel/src/drain_gate/protocol.rs"),
         Path("crates/xlfn-kernel/src/rotating_read_domain/protocol.rs"),
         *tuple(Path("verification/verus/drain_gate/src").glob("*.rs")),
         *tuple(Path("verification/verus/rotating_read_domain/src").glob("*.rs"))),
        {
            "live observation omitted from coverage ledger": (
                "        self.entries.tracked_insert(key, observation);",
                "        let _ = observation;",
            ),
            "observation count not decremented on scope exit": (
                "        observation.end(node, &mut self.observing);",
                "        let _ = observation;",
            ),
            "observation ledger loses count conservation": (
                "        self.observing.value() == self.entries.len()",
                "        self.observing.value() >= self.entries.len()",
            ),
            "borrow lifetime erased while observations are still live": (
                "requires self.inv(), self.len() == 0,",
                "requires self.inv(),",
            ),
            "new observation allowed after coverage was frozen": (
                "requires old(self).inv(), !old(self).frozen(), !old(self).contains(key),",
                "requires old(self).inv(), !old(self).contains(key),",
            ),
            "live observation ledger narrowed before final retirement": (
                "requires old(observations).inv(), old(observations).frozen(),\n            stripes",
                "requires old(observations).inv(),\n            stripes",
            ),
            "previous generation reused while pending is still active": (
                "rotation.pending.is_none(), ledgers.matches(rotation),",
                "ledgers.matches(rotation),",
            ),
            "narrowing uses another domains generation identities": (
                "rotation.pending.is_none(), ledgers.matches(rotation),\n            old(observations).domain() == generation_domain(ledgers),",
                "rotation.pending.is_none(), ledgers.matches(rotation),",
            ),
            "publication begins before narrowing retired observation coverage": (
                "        proof { narrow_before_rotation(observations, rotation, ledgers); }",
                "        proof {}",
            ),
            "pending drain accepted with uncovered observations": (
                "            observations.coverage().subset_of(Set::empty().insert(ledgers.selected_id(!rotation.current))),",
                "            true,",
            ),
            "coverage narrowed without draining excluded gates": (
                "            covers(old(observations).coverage().difference(keep), ledgers),",
                "            true,",
            ),
            "drain omits gates assigned to node observations": (
                "ledgers.matches(stripes::final_states(histories)), covers(observations.coverage(), ledgers),",
                "ledgers.matches(stripes::final_states(histories)),",
            ),
        },
    )

    check_mutations(Path("verification/verus/cache_lease"), Path("verification/verus/cache_lease/src/scope_ownership.rs"),
        (Path("crates/xlfn/src/retirement_queue.rs"), Path("crates/xlfn/src/cache/pin_transitions.rs"), Path("crates/xlfn/src/cache/node_layout.rs"),
         Path("verification/verus/published_owner/src/heap_permission.rs"), *tuple(Path("verification/verus/drain_gate/src").glob("*.rs")),
         Path("crates/xlfn-kernel/src/drain_gate/protocol.rs"), Path("crates/xlfn-kernel/src/rotating_read_domain/protocol.rs"),
         *tuple(Path("verification/verus/rotating_read_domain/src").glob("*.rs")), Path("crates/xlfn-kernel/src/sealable_counter/transitions.rs")), {
            'cache observation excludes a foreign actual drain': (
                'requires drains.inv(), drains.domain().contains(self.gate_id()),', 'requires drains.inv(),',
            ),
        })

    check_mutations(Path("verification/verus/cache_lease"), Path("verification/verus/cache_lease/src/observation_coverage.rs"),
        (Path("crates/xlfn/src/retirement_queue.rs"), Path("crates/xlfn/src/cache/pin_transitions.rs"), Path("crates/xlfn/src/cache/node_layout.rs"),
         Path("verification/verus/published_owner/src/heap_permission.rs"), *tuple(Path("verification/verus/drain_gate/src").glob("*.rs")),
         Path("crates/xlfn-kernel/src/drain_gate/protocol.rs"), Path("crates/xlfn-kernel/src/rotating_read_domain/protocol.rs"),
         *tuple(Path("verification/verus/rotating_read_domain/src").glob("*.rs")), Path("crates/xlfn-kernel/src/sealable_counter/transitions.rs")), {
            'cache actual narrowing omits an excluded gate': (
                'drains.covers(old(observations).coverage().difference(keep)),', 'true,',
            ),
            'cache actual drain omits an observed gate': (
                'requires observations.inv(), drains.inv(), drains.covers(observations.coverage()),', 'requires observations.inv(), drains.inv(),',
            ),
        })

    check_mutations(Path("verification/verus/cache_lease"), Path("verification/verus/cache_lease/src/retirement.rs"),
        (Path("crates/xlfn/src/retirement_queue.rs"), Path("crates/xlfn/src/cache/pin_transitions.rs"), Path("crates/xlfn/src/cache/node_layout.rs"),
         Path("verification/verus/published_owner/src/heap_permission.rs"), *tuple(Path("verification/verus/drain_gate/src").glob("*.rs")),
         Path("crates/xlfn-kernel/src/drain_gate/protocol.rs"), Path("crates/xlfn-kernel/src/rotating_read_domain/protocol.rs"),
         *tuple(Path("verification/verus/rotating_read_domain/src").glob("*.rs")), Path("crates/xlfn-kernel/src/sealable_counter/transitions.rs")), {
            'cache recovery lacks actual observation coverage': (
                'drains.inv(), drains.covers(observations.coverage()),', 'drains.inv(),',
            ),
        })

    check_mutations(Path("verification/verus/cache_lease"), Path("verification/verus/cache_lease/src/atomic_admission.rs"),
        (Path("crates/xlfn/src/retirement_queue.rs"), Path("crates/xlfn/src/cache/pin_transitions.rs"), Path("crates/xlfn/src/cache/node_layout.rs"),
         Path("verification/verus/published_owner/src/heap_permission.rs"), *tuple(Path("verification/verus/drain_gate/src").glob("*.rs")),
         Path("crates/xlfn-kernel/src/drain_gate/protocol.rs"), Path("crates/xlfn-kernel/src/rotating_read_domain/protocol.rs"),
         *tuple(Path("verification/verus/rotating_read_domain/src").glob("*.rs")), Path("crates/xlfn-kernel/src/sealable_counter/transitions.rs")), {
            'cache atomic admission loses counter identity': (
                'self.counter.inv() && self.permit@.instance_id() == self.counter.id()', 'self.counter.inv()',
            ),
            'cache atomic observation enters an unrelated node domain': (
                'node.domain().contains(admission.gate_id()),', 'true,',
            ),
        })

    check_mutations(Path("verification/verus/cache_lease"), Path("verification/verus/cache_lease/src/atomic_pins.rs"),
        (Path("crates/xlfn/src/retirement_queue.rs"), Path("crates/xlfn/src/cache/pin_transitions.rs"), Path("crates/xlfn/src/cache/node_layout.rs"),
         Path("verification/verus/published_owner/src/heap_permission.rs"), *tuple(Path("verification/verus/drain_gate/src").glob("*.rs")),
         Path("crates/xlfn-kernel/src/drain_gate/protocol.rs"), Path("crates/xlfn-kernel/src/rotating_read_domain/protocol.rs"),
         *tuple(Path("verification/verus/rotating_read_domain/src").glob("*.rs")), Path("crates/xlfn-kernel/src/sealable_counter/transitions.rs")), {
            'Cache atomic constructor changes the drain domain': (
                'initialize_covered_node(memory, domain);', 'initialize_covered_node(memory, Set::empty());',
            ),
            'Cache atomic constructor starts with two pins': (
                '$atomic::new(Ghost(instance.id()), 1, Tracked(AtomicState { count, retiring }))',
                '$atomic::new(Ghost(instance.id()), 2, Tracked(AtomicState { count, retiring }))',
            ),
            'failed observed CAS issues a Cache pin': (
                'if result is Ok {\n                        kernel::successful_acquire_adds_one(raw, next);\n                        pin = Some(self.instance.borrow().acquire_from_observation', 'if true {\n                        kernel::successful_acquire_adds_one(raw, next);\n                        pin = Some(self.instance.borrow().acquire_from_observation',
            ),
            'failed anchored CAS issues a Cache pin': (
                'if result is Ok {\n                        kernel::successful_acquire_adds_one(raw, next);\n                        pin = Some(self.instance.borrow().acquire_from_pin', 'if true {\n                        kernel::successful_acquire_adds_one(raw, next);\n                        pin = Some(self.instance.borrow().acquire_from_pin',
            ),
            'Cache atomic pin count loses conservation': (
                'state.count.instance_id() == id && state.count.value() == raw as nat', 'state.count.instance_id() == id',
            ),
            'Cache atomic release forgets a nonfinal pin': (
                'self.instance.borrow().release_nonfinal(pin.element().0, pin.element().1, &mut state.count, pin);', '',
            ),
            'Cache atomic release accepts another node pin': (
                'requires self.inv(), pin.instance_id() == self.id(),\n            ensures result.0 != Release::FailStop,', 'requires self.inv(),\n            ensures result.0 != Release::FailStop,',
            ),
            'Cache final atomic release does not freeze observations': (
                'proof { observations.freeze(&entry); }', '',
            ),
            'Cache atomic recovery omits drain coverage': (
                'observations.domain() == self.domain(), drains.inv(), drains.covers(observations.coverage()),', 'observations.domain() == self.domain(), drains.inv(),',
            ),
            'Cache atomic overflow is confused with zero': (
                'overflow = (Acquire::Overflow, Tracked(None), Ghost(raw));\n                invariant self.inv(), observation.instance_id()',
                'overflow = (Acquire::Zero, Tracked(None), Ghost(raw));\n                invariant self.inv(), observation.instance_id()',
            ),
        })

    check_mutations(Path("verification/verus/cache_lease"), Path("verification/verus/cache_lease/src/queued_atomic.rs"),
        (Path("crates/xlfn/src/retirement_queue.rs"), Path("crates/xlfn/src/cache/pin_transitions.rs"), Path("crates/xlfn/src/cache/node_layout.rs"),
         Path("verification/verus/published_owner/src/heap_permission.rs"), *tuple(Path("verification/verus/drain_gate/src").glob("*.rs")),
         Path("crates/xlfn-kernel/src/drain_gate/protocol.rs"), Path("crates/xlfn-kernel/src/rotating_read_domain/protocol.rs"),
         *tuple(Path("verification/verus/rotating_read_domain/src").glob("*.rs")), Path("crates/xlfn-kernel/src/sealable_counter/transitions.rs")), {
            'Cache ledger invariant collides with pin invariant': (
                'pins.namespace() + 1);', 'pins.namespace());',
            ),
            'Cache final pin omits retained-observation freezing': (
                'state.observations.borrow_mut().freeze(instance, retirement);', '',
            ),
            'Cache nonfinal release enters retirement completion': (
                'if let super::super::pin_transitions::Release::LastPin = outcome {',
                'if let super::super::pin_transitions::Release::StillPinned = outcome {',
            ),
            'Cache resource invariant accepts a foreign allocation': (
                'state.allocation@.instance_id() == self.id && state.observations@.inv()', 'state.observations@.inv()',
            ),
            'Cache queue payload accepts a foreign node': (
                'self.node.inv() && self.record.node_id() == self.node.id() && self.record.domain() == self.node.gates()',
                'self.node.inv() && self.record.domain() == self.node.gates()',
            ),
            'Cache locked recovery omits actual drain coverage': (
                'requires self.inv(), drains.inv(), drains.covers(self.gates()),', 'requires self.inv(), drains.inv(),',
            ),
            'Cache detached batch loses a recovered allocation': (
                'let memory = entry.recover(Tracked(drains));\n            memories.push(memory);', 'let memory = entry.recover(Tracked(drains));',
            ),
            'Cache coverage receipt loses its monotonic bound': (
                '==> self.snapshot.1 && self.snapshot.0.subset_of(bound)', '==> self.snapshot.1',
            ),
            'Cache resource invariant loses coverage correspondence': (
                '&& state.coverage@.value() == (state.observations@.coverage(), state.observations@.frozen())', '',
            ),
            'Cache entry uses another coverage instance': (
                '&& self.bound@.instance_id() == self.node.ledger@.constant().coverage.id()', '',
            ),
            'Cache preparation omits observation narrowing': (
                'state.observations.borrow_mut().narrow(keep, idle);', '',
            ),
            'Cache queue preparation skips a record': (
                'queue.records[next].prepare(Ghost(keep), Tracked(idle));', '',
            ),
            'Cache prepared batch accepts a different drain domain': (
                'old(prepared).unwrap().value() == drains.domain(),', 'true,',
            ),
            'Cache preparation skips the current-generation recheck': (
                'if current.recheck(&queue, lock, handle) != queue.index {', 'if false {',
            ),
            'Cache reservation changes the prepared coverage': (
                'current.reserve(queue, lock, handle, Ghost(keep))', 'current.reserve(queue, lock, handle, Ghost(gates))',
            ),
            'Cache preparation borrows incomplete drain collection': (
                'if !collection.poll_all(counters) { return Err(collection); }', 'if false { return Err(collection); }',
            ),
            'Cache lookup releases admission before ending observation': (
                'node.end_observation(Tracked(&mut scope), ticket);', '',
            ),
            'Cache lookup enters an unrelated counter domain': (
                'node.gates().contains(counter.id()),', 'true,',
            ),
            'Cache pending recovery skips complete stripe collection': (
                'if !collection.poll_all(counters) { return Err((handoff, collection, Tracked(prepared))); }',
                'if false { return Err((handoff, collection, Tracked(prepared))); }',
            ),
            'Cache pending ticket has unrelated stripe coverage': (
                'prepared.value() == atomic_stripes::gate_ids(counters@),', 'true,',
            ),
            'Cache pending finish accepts another Current queue phase': (
                'lock.pred().preparation.id() == current.gate(handoff.index()),', 'true,',
            ),
            'Cache successful callback leaves pending uncleared': (
                'finished = Some(handoff.finish(collection, counters, current, lock, Tracked(callback.1.borrow())));',
                'finished = Some(handoff.cancel(collection, counters));',
            ),
        })

    check_mutations(Path("verification/verus/cache_lease"), Path("verification/verus/cache_lease/src/retained_observations.rs"),
        (Path("crates/xlfn/src/retirement_queue.rs"), Path("crates/xlfn/src/cache/pin_transitions.rs"), Path("crates/xlfn/src/cache/node_layout.rs"),
         Path("verification/verus/published_owner/src/heap_permission.rs"), *tuple(Path("verification/verus/drain_gate/src").glob("*.rs")),
         Path("crates/xlfn-kernel/src/drain_gate/protocol.rs"), Path("crates/xlfn-kernel/src/rotating_read_domain/protocol.rs"),
         *tuple(Path("verification/verus/rotating_read_domain/src").glob("*.rs")), Path("crates/xlfn-kernel/src/sealable_counter/transitions.rs")), {
            'Cache retained observations lose count conservation': (
                'self.observing.value() == self.entries.len() && self.coverage.subset_of(self.domain)', 'self.coverage.subset_of(self.domain)',
            ),
            'Cache retained observation completion forgets its admission share': (
                'scope.finish(entry.share);', '',
            ),
            'Cache retained observation ends in another scope': (
                'old(scope).inv(), ticket.scope_id() == old(scope).id(),', 'old(scope).inv(),',
            ),
            'Cache retained observations lose gate coverage': (
                '&& self.coverage.contains(self.entries[key].share.gate_id())', '',
            ),
            'Cache retained observation accepts another node resident': (
                'resident.instance_id() == node.id(), resident.element().1 == PinKind::Resident,', 'resident.element().1 == PinKind::Resident,',
            ),
            'Cache retained retirement history belongs to another node': (
                '&& (self.retired.is_some() ==> self.retired.unwrap().instance_id() == self.node_id())', '',
            ),
            'Cache retained completion forgets its reader observation': (
                'node.leave_observation(ticket.observation.element(), ticket.observation, &mut self.observing);', '',
            ),
            'Cache retained completion accepts another node ticket': (
                'ticket.ledger_id() == old(self).id(), ticket.node_id() == node.id(),',
                'ticket.ledger_id() == old(self).id(),',
            ),
            'Cache retained receipt loses the exact memory identity': (
                'self.observation.element() == self.token.value().2', 'true',
            ),
        })

    check_mutations(Path("verification/verus/cache_lease"), Path("verification/verus/cache_lease/src/resident_index.rs"),
        (Path("crates/xlfn/src/retirement_queue.rs"), Path("crates/xlfn/src/cache/pin_transitions.rs"), Path("crates/xlfn/src/cache/node_layout.rs"),
         Path("verification/verus/published_owner/src/heap_permission.rs"), *tuple(Path("verification/verus/drain_gate/src").glob("*.rs")),
         Path("crates/xlfn-kernel/src/drain_gate/protocol.rs"), Path("crates/xlfn-kernel/src/rotating_read_domain/protocol.rs"),
         *tuple(Path("verification/verus/rotating_read_domain/src").glob("*.rs")), Path("crates/xlfn-kernel/src/sealable_counter/transitions.rs")), {
            'Cache index lock loses its protected allocation predicate': (
                '&& (self.memory_inv)(value.unwrap().memory())', '',
            ),
            'Cache index cell forgets its stored resident invariant': (
                'value.is_some() ==> value.unwrap().inv() && value.unwrap().owner() == self.owner',
                'value.is_some() ==> value.unwrap().owner() == self.owner',
            ),
            'Cache index stores a lease as residency': (
                'self.pin@.element().1 == PinKind::Resident', 'self.pin@.element().1 == PinKind::Lease',
            ),
            'Cache index lookup accepts an unrelated admission domain': (
                'old(scope).inv(), cell.pred().gates.contains(old(scope).gate_id()),', 'old(scope).inv(),',
            ),
            'Cache index snapshot loses node observation identity': (
                'self.node.inv() && self.ticket@.ledger_id() == self.node.observation_id()', 'self.node.inv()',
            ),
            'Cache index snapshot completes in a foreign scope': (
                'requires self.inv(), counter.inv(), scope.inv(), scope.len() == 1, scope.id() == self.scope_id(), scope.gate_id() == counter.id(),',
                'requires self.inv(), counter.inv(), scope.inv(), scope.len() == 1, scope.gate_id() == counter.id(),',
            ),
            'Cache admitted index lookup uses an unrelated counter': (
                'requires counter.inv(), cell.pred().gates.contains(counter.id()),', 'requires counter.inv(),',
            ),
        })

    check_mutations(Path("verification/verus/cache_lease"), Path("verification/verus/cache_lease/src/queued_atomic.rs"),
        (Path("crates/xlfn/src/retirement_queue.rs"), Path("crates/xlfn/src/cache/pin_transitions.rs"), Path("crates/xlfn/src/cache/node_layout.rs"),
         Path("verification/verus/published_owner/src/heap_permission.rs"), *tuple(Path("verification/verus/drain_gate/src").glob("*.rs")),
         Path("crates/xlfn-kernel/src/drain_gate/protocol.rs"), Path("crates/xlfn-kernel/src/rotating_read_domain/protocol.rs"),
         *tuple(Path("verification/verus/rotating_read_domain/src").glob("*.rs")), Path("crates/xlfn-kernel/src/sealable_counter/transitions.rs")), {
            'Cache lease loses its exact node identity': (
                'self.node.inv() && self.pin@.instance_id() == self.node.id()', 'self.node.inv()',
            ),
            'Cache lease pointer differs from its allocation': (
                '&& self.pin@.element().1 == PinKind::Lease && self.pin@.element().0.ptr() == self.pointer',
                '&& self.pin@.element().1 == PinKind::Lease',
            ),
            'Cache lease references uninitialized memory': (
                '&& self.pin@.element().0.is_init()', '',
            ),
        })

    check_mutations(Path("verification/verus/cache_lease"), Path("verification/verus/cache_lease/src/inline_value.rs"),
        (Path("crates/xlfn/src/retirement_queue.rs"), Path("crates/xlfn/src/cache/pin_transitions.rs"), Path("crates/xlfn/src/cache/node_layout.rs"),
         Path("verification/verus/published_owner/src/heap_permission.rs"), *tuple(Path("verification/verus/drain_gate/src").glob("*.rs")),
         Path("crates/xlfn-kernel/src/drain_gate/protocol.rs"), Path("crates/xlfn-kernel/src/rotating_read_domain/protocol.rs"),
         *tuple(Path("verification/verus/rotating_read_domain/src").glob("*.rs")), Path("crates/xlfn-kernel/src/sealable_counter/transitions.rs")), {
            'Cache inline index omits its allocation owner predicate': (
                'Ghost(|memory: HeapPermission<Allocation<V>>| memory.value().domain as *const u8 == owner)',
                'Ghost(|memory: HeapPermission<Allocation<V>>| true)',
            ),
            'Cache inline lookup accepts an unrelated allocation owner': (
                '==> memory.value().domain as *const u8 == cell.pred().owner,',
                '==> true,',
            ),
            'Cache inline initializer substitutes allocation address for domain': (
                'let owner = super::super::node_layout::domain!(allocation);',
                'let owner = pointer as *mut u8;',
            ),
            'Cache inline lease loses allocation owner agreement': (
                'self.lease.inv() && self.memory().value().domain as *const u8 == self.lease.owner()',
                'self.lease.inv()',
            ),
            'Cache inline value read loses its allocation pin invariant': (
                'self.lease.inv() && self.memory().value().domain as *const u8 == self.lease.owner()',
                'self.memory().value().domain as *const u8 == self.lease.owner()',
            ),
        })

    check_mutations(Path("verification/verus/cache_lease"), Path("verification/verus/cache_lease/src/queued_atomic.rs"),
        (Path("crates/xlfn/src/retirement_queue.rs"), Path("crates/xlfn/src/cache/pin_transitions.rs"), Path("crates/xlfn/src/cache/node_layout.rs"),
         Path("verification/verus/published_owner/src/heap_permission.rs"), *tuple(Path("verification/verus/drain_gate/src").glob("*.rs")),
         Path("crates/xlfn-kernel/src/drain_gate/protocol.rs"), Path("crates/xlfn-kernel/src/rotating_read_domain/protocol.rs"),
         *tuple(Path("verification/verus/rotating_read_domain/src").glob("*.rs")), Path("crates/xlfn-kernel/src/sealable_counter/transitions.rs")), {
            'Cache prepared recovery permits a foreign-owner payload': (
                'old(prepared).unwrap().value() == drains.domain(),\n'
                "            forall|entry: Entry<'node, T>| (#[trigger] (lock.pred().payload_inv)(entry)) ==> entry.inv() && entry.owner() == owner,",
                'old(prepared).unwrap().value() == drains.domain(),\n'
                "            forall|entry: Entry<'node, T>| (#[trigger] (lock.pred().payload_inv)(entry)) ==> entry.inv(),",
            ),
            'Cache full-drain recovery permits a foreign-owner payload': (
                '==> entry.inv() && entry.owner() == owner && drains.covers(entry.gates()),',
                '==> entry.inv() && drains.covers(entry.gates()),',
            ),
        })

    check_mutations(Path("verification/verus/cache_lease"), Path("crates/xlfn/src/cache/node_layout.rs"),
        (Path("crates/xlfn/src/retirement_queue.rs"), Path("crates/xlfn/src/cache/pin_transitions.rs"), Path("crates/xlfn/src/cache/node_layout.rs"),
         Path("verification/verus/published_owner/src/heap_permission.rs"), *tuple(Path("verification/verus/drain_gate/src").glob("*.rs")),
         Path("crates/xlfn-kernel/src/drain_gate/protocol.rs"), Path("crates/xlfn-kernel/src/rotating_read_domain/protocol.rs"),
         *tuple(Path("verification/verus/rotating_read_domain/src").glob("*.rs")), Path("crates/xlfn-kernel/src/sealable_counter/transitions.rs")), {
            'Cache observed generation reads weight instead': (
                '($node).generation', '($node).weight',
            ),
            'Cache eligibility ignores allocation generation': (
                '$generation == $epoch && $resident', '$resident',
            ),
            'Cache eligibility ignores observed residency': (
                '$generation == $epoch && $resident', '$generation == $epoch',
            ),
        })

    check_mutations(Path("verification/verus/cache_lease"), Path("verification/verus/cache_lease/src/queued_atomic.rs"),
        (Path("crates/xlfn/src/retirement_queue.rs"), Path("crates/xlfn/src/cache/pin_transitions.rs"), Path("crates/xlfn/src/cache/node_layout.rs"),
         Path("verification/verus/published_owner/src/heap_permission.rs"), *tuple(Path("verification/verus/drain_gate/src").glob("*.rs")),
         Path("crates/xlfn-kernel/src/drain_gate/protocol.rs"), Path("crates/xlfn-kernel/src/rotating_read_domain/protocol.rs"),
         *tuple(Path("verification/verus/rotating_read_domain/src").glob("*.rs")), Path("crates/xlfn-kernel/src/sealable_counter/transitions.rs")), {
            'Cache idle recovery uses another stripe bound': (
                'let ghost bound = published.bound();\n        let (detached, withdrawal) = published.detach();',
                'let ghost bound = Set::empty();\n        let (detached, withdrawal) = published.detach();',
            ),
            'Cache idle recovery skips the last withdrawn node': (
                'while records.len() > 0\n            invariant detached.inv()',
                'while records.len() > 1\n            invariant detached.inv()',
            ),
            'Cache idle callback loses exact withdrawn source': (
                'ensures result.1@ == withdrawal.source(),\n            valid_records(result.1@, owner)',
                'ensures valid_records(result.1@, owner)',
            ),
        })

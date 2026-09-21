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
        (Path("crates/xlfn/src/retirement_queue.rs"), Path("crates/xlfn/src/cache/pin_transitions.rs"),
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
        (Path("crates/xlfn/src/retirement_queue.rs"), Path("crates/xlfn/src/cache/pin_transitions.rs"),
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
        (Path("crates/xlfn/src/retirement_queue.rs"), Path("crates/xlfn/src/cache/pin_transitions.rs"),
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
        (Path("crates/xlfn/src/retirement_queue.rs"), Path("crates/xlfn/src/cache/pin_transitions.rs"),
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

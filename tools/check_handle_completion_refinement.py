#!/usr/bin/env python3
"""Reject early destruction completion and shared completion-tail regressions."""
from pathlib import Path
from check_drain_gate_refinement import check_mutations

PROOF = Path("verification/verus/handle_domain")
PROTOCOL = Path("crates/xlfn/src/handle/domain/protocol.rs")
COUNTERS = Path("crates/xlfn/src/handle/domain/counters.rs")

DEPENDENCIES = (
    Path("crates/xlfn/src/retirement_queue.rs"),
    PROTOCOL, COUNTERS,
    Path("crates/xlfn/src/handle/binding/protocol.rs"),
    Path("crates/xlfn/src/call/permits.rs"),
    Path("crates/xlfn-kernel/src/rotating_read_domain/protocol.rs"),
    Path("crates/xlfn-kernel/src/drain_gate/protocol.rs"),
    Path("crates/xlfn-kernel/src/sealable_counter/transitions.rs"),
    *tuple(Path("verification/verus/rotating_read_domain/src").glob("*.rs")),
    *tuple(Path("verification/verus/drain_gate/src").glob("*.rs")),
    Path("verification/verus/published_owner/src/heap_permission.rs"),
    Path("verification/verus/published_owner/src/permission.rs"),
)

if __name__ == "__main__":
    check_mutations(PROOF, PROTOCOL, DEPENDENCIES, {
        "completion mutex acquired before user destruction": (
            "        let $receipt = $destroy;\n        $discharge;\n        let $guard = $lock;",
            "        let $guard = $lock;\n        let $receipt = $destroy;\n        $discharge;",
        ),
        "debt discharge omitted": ("        $discharge;", "        ();"),
        "completion notification omitted": ("        $notify;", "        ();"),
        "completion notification after unlock": (
            "        $notify;\n        $unlock;", "        $unlock;\n        $notify;",
        ),
    })
    check_mutations(PROOF, PROTOCOL, DEPENDENCIES, {
        "batch merge accepts a foreign domain": (
            "if left != right {", "if false {",
        ),
        "batch merge forgets payload transfer": (
            "        $append;", "        ();",
        ),
        "batch merge moves payload before checking owner": (
            "        let left: *const _ = $left;\n        let right: *const _ = $right;\n        if left != right {\n            $reject;\n        }\n        $append;",
            "        $append;\n        let left: *const _ = $left;\n        let right: *const _ = $right;\n        if left != right {\n            $reject;\n        }",
        ),
    })
    check_mutations(PROOF, PROOF / "src/lib.rs", DEPENDENCIES, {
        "detachment incorrectly discharges destruction debt": (
            "reclaiming_bindings: s.reclaiming_bindings + s.pending_0,\n            pending_0: 0,",
            "reclaiming_bindings: s.reclaiming_bindings + s.pending_0,\n            pending_0: 0,\n            debt: (s.debt - s.pending_0) as nat,",
        ),
        "seal ignores destruction in flight": (
            "&& s.pending_0 == 0 && s.pending_1 == 0 && s.reclaiming_bindings == 0",
            "&& s.pending_0 == 0 && s.pending_1 == 0",
        ),
    })
    check_mutations(PROOF, PROOF / "src/completion.rs", DEPENDENCIES, {
        "foreign batch receipt discharges debt": (
            "                receipt.instance_id() == old(self).instance@.id(),",
            "                true,",
        ),
        "completed receipt remains reusable": (
            "remove returned -= Some(()); update phase = 2;",
            "remove returned -= Some(()); add returned += Some(()); update phase = 2;",
        ),
        "destructor return retains owned batch": (
            "{ let _records = self.records.take().unwrap(); }",
            "{ let _records = &self.records; }",
        ),
    })

    check_mutations(PROOF, COUNTERS, DEPENDENCIES, {
        "counter addition loses its increment": (
            "CountStep::Success($state + $amount)", "CountStep::Success($state)",
        ),
        "counter subtraction loses its decrement": (
            "CountStep::Success($state - $amount)", "CountStep::Success($state)",
        ),
        "exact-capacity addition incorrectly fails": (
            "$amount > <$word>::MAX - $state", "$amount >= <$word>::MAX - $state",
        ),
        "final counter subtraction incorrectly fails": (
            "$amount > $state", "$amount >= $state",
        ),
    })

    check_mutations(PROOF, PROOF / "src/counter_refinement.rs", DEPENDENCIES, {
        "enqueue counter uses unrelated destruction debt": (
            "debt as nat == model.debt, queued as nat", "queued as nat",
        ),
        "detachment subtracts an unrelated batch size": (
            ", amount as nat == get_pending(model, generation),", ",",
        ),
    })

    check_mutations(PROOF, PROOF / "src/batches.rs", DEPENDENCIES, {
        "certified batch binds a foreign completion owner": (
            "certificate: &Drained<'_>) -> (batch: Option<Batch<'domain, P>>)\n        requires certificate.inv(), old(queue).held.is_none(), !old(queue).both_held,\n            old(queue).owner == owner.rotation,",
            "certificate: &Drained<'_>) -> (batch: Option<Batch<'domain, P>>)\n        requires certificate.inv(), old(queue).held.is_none(), !old(queue).both_held,",
        ),
        "certified batch discards detached payload": (
            "match super::super::rotation::detachment::$module::detach(queue, certificate) {\n            Some(records) => Some(Batch { owner, records }),",
            "match super::super::rotation::detachment::$module::detach(queue, certificate) {\n            Some(records) => Some(Batch { owner, records: Vec::new() }),",
        ),
        "terminal binding batches exchange generations": (
            "Some([Batch { owner, records: zero }, Batch { owner, records: one }])",
            "Some([Batch { owner, records: one }, Batch { owner, records: zero }])",
        ),
    })
    check_mutations(PROOF, PROOF / "src/binding_ownership.rs", DEPENDENCIES, {
        "binding adoption accepts unrelated heap pointer": (
            "requires memory.inv(), memory.ptr() == pointer, memory.is_init(),",
            "requires memory.inv(), memory.is_init(),",
        ),
        "binding retirement accepts foreign domain owner": (
            "requires valid_queue(old(queue), domain), record.inv(), record.domain() == domain,",
            "requires valid_queue(old(queue), domain), record.inv(),",
        ),
        "binding completion skips destruction tail": (
            "        work.complete();", "        ();",
        ),
    })

    check_mutations(PROOF, Path("crates/xlfn/src/call/permits.rs"), DEPENDENCIES, {
        "call retention loses the first domain permit": (
            "                        list.push(existing);", "                        let _ = existing;",
        ),
        "call retention loses the newly admitted domain": (
            "                        list.push($permit);", "                        let _ = $permit;",
        ),
        "call retention drops previously retained domains on append": (
            "Retained::Multiple(list) => list.push($permit),",
            "Retained::Multiple(list) => { list.clear(); list.push($permit); },",
        ),
    })

    check_mutations(PROOF, Path("crates/xlfn/src/handle/binding/protocol.rs"), DEPENDENCIES, {
        "binding read bypasses domain authorization": (
            "if !$authorized {", "if false {",
        ),
        "binding publication load precedes authorization": (
            "        if !$authorized {\n            $foreign;\n        }\n        let $record = match $load {\n            Some(record) => record,\n            None => {\n                $missing;\n            }\n        };",
            "        let $record = match $load {\n            Some(record) => record,\n            None => {\n                $missing;\n            }\n        };\n        if !$authorized {\n            $foreign;\n        }",
        ),
        "binding read accepts stale identity or state": (
            "if !$valid {", "if false {",
        ),
    })
    check_mutations(PROOF, PROOF / "src/reading.rs", DEPENDENCIES, {
        "binding read ignores retired state": (
            "value.id == id && publication.sampled_live,", "value.id == id,",
        ),
        "binding read ignores identity": (
            "value.id == id && publication.sampled_live,", "publication.sampled_live,",
        ),
        "binding read accepts pointer unrelated to allocation": (
            "publication.present, pointer == observation.memory().ptr(),",
            "publication.present,",
        ),
    })

    check_mutations(PROOF, PROOF / "src/observations.rs", DEPENDENCIES, {
        "binding reclamation ignores outstanding observations": (
            "require(pre.observing == 0); remove retired -= Some(x);",
            "require(pre.observing >= 0); remove retired -= Some(x);",
        ),
        "binding retirement incorrectly assumes observers have ended": (
            "(self.published.is_some() ==> self.retired.is_none())",
            "(self.published.is_some() ==> self.retired.is_none()) && (self.retired.is_some() ==> self.observing == 0)",
        ),
        "binding observation forgets its count": (
            "add observations += {x}; update observing = pre.observing + 1;",
            "add observations += {x}; update observing = pre.observing;",
        ),
        "binding observation accepts foreign gate": (
            "instance.domain().contains(permit.instance_id()),",
            "true,",
        ),
        "retired binding enters a foreign queue": (
            "requires old(queue).owner == entry.owner(), old(queue).held.is_none(),",
            "requires old(queue).held.is_none(),",
        ),
        "retired binding recovers with active observations": (
            "count.instance_id() == instance.id(), count.value() == 0,",
            "count.instance_id() == instance.id(),",
        ),
        "retired binding recovers another allocation": (
            "requires entry.node_id() == instance.id(), count.instance_id() == instance.id(),",
            "requires count.instance_id() == instance.id(),",
        ),
    })

    check_mutations(PROOF, PROOF / "src/coverage.rs", DEPENDENCIES, {
        "handle ledger forgets observation count conservation": (
            "self.count.value() == self.entries.len()", "true",
        ),
        "handle ledger permits observations outside drain coverage": (
            "&& self.coverage.contains(self.entries[key].gate_id()))", "&& true)",
        ),
        "handle ledger permits new observations after freezing": (
            "requires old(self).inv(), !old(self).frozen(), !old(self).contains(key),",
            "requires old(self).inv(), !old(self).contains(key),",
        ),
        "handle retirement leaves ledger open": (
            "self.frozen = true;", "self.frozen = false;",
        ),
        "handle ledger treats arbitrary histories as drained": (
            "requires observations.inv(), stripes::drained_histories(histories),",
            "requires observations.inv(),",
        ),
        "handle narrowing ignores pending generation": (
            "rotation.pending.is_none(), ledgers.matches(rotation),",
            "ledgers.matches(rotation),",
        ),
        "handle recovery skips pending observation exclusion": (
            "proof { zero_after_pending(observations, rotation, ledgers); }",
            "proof { }",
        ),
    })

    check_mutations(PROOF, PROOF / "src/atomic_publication.rs", DEPENDENCIES, {
        "atomic binding pointer loses allocation correspondence": (
            "&& g.published.unwrap().published.value().ptr() == value)",
            "&& true)",
        ),
        "atomic binding load skips observation acquisition": (
            "acquired = Some((observation, p.instance.clone()));", "acquired = None;",
        ),
        "atomic binding null load fabricates a reader": (
            "if pointer.addr() == 0 { None } else {", "if false { None } else {",
        ),
        "atomic binding clear keeps pointer published": (
            "compare_exchange(expected, core::ptr::null_mut())",
            "compare_exchange(expected, expected)",
        ),
        "atomic binding reads through wrong allocation": (
            "self.observation@.node_id() == self.instance@.id()", "true",
        ),
        "atomic binding reader ignores observed pointer identity": (
            "&& self.observation@.memory().ptr() == self.pointer && self.observation@.memory().is_init()",
            "&& self.observation@.memory().is_init()",
        ),
    })

    check_mutations(PROOF, PROOF / "src/publication_counts.rs", DEPENDENCIES, {
        "replacement allocation discards old reader counters": (
            "self.counters.tracked_insert(count.instance_id(), count);",
            "self.counters = Map::tracked_empty(); self.counters.tracked_insert(count.instance_id(), count);",
        ),
        "allocation registry forgets counter identity": (
            "(#[trigger] self.counters[id]).instance_id() == id", "true",
        ),
        "old reader completion is lost after new registration": (
            "counts.end(old_instance, old_receipt, observation);", "",
        ),
        "allocation registration overwrites an existing counter": (
            "old(self).inv(), !old(self).contains(count.instance_id()),", "old(self).inv(),",
        ),
    })

    check_mutations(PROOF, PROOF / "src/atomic_publication.rs", DEPENDENCIES, {
        "republished binding loses gate-domain correspondence": (
            "self.instance.owner() == owner && self.instance.domain() == domain",
            "self.instance.owner() == owner",
        ),
        "retirement substitutes expected pointer provenance": (
            "RetiredRecord::new(pointer, Tracked(&instance), Tracked(ticket), Tracked(receipt), Tracked(history))",
            "RetiredRecord::new(expected, Tracked(&instance), Tracked(ticket), Tracked(receipt), Tracked(history))",
        ),
        "failed republication reports success": (
            "return (PublishStatus::FailStop, some_prepared(prepared_arg))",
            "return (PublishStatus::Published, some_prepared(prepared_arg))",
        ),
    })

    check_mutations(PROOF, Path("crates/xlfn/src/handle/binding/protocol.rs"), DEPENDENCIES, {
        "publication skips the empty-slot check": ("if !$empty {", "if false {"),
    })
    check_mutations(PROOF, PROOF / "src/writer_authority.rs", DEPENDENCIES, {
        "publication writer view fails to follow atomic update": (
            "update atomic = pointer; update writer = pointer;",
            "update atomic = pointer; update writer = pre.writer;",
        ),
    })
    check_mutations(PROOF, PROOF / "src/writer_lock.rs", DEPENDENCIES, {
        "foreign writer lock authorizes publication": (
            "requires slot.inv(), lock.pred().authority == slot.authority_id(),\n        prepared.valid",
            "requires slot.inv(),\n        prepared.valid",
        ),
        "foreign writer lock authorizes retirement": (
            "requires slot.inv(), lock.pred().authority == slot.authority_id(), expected.addr() != 0,",
            "requires slot.inv(), expected.addr() != 0,",
        ),
    })

    check_mutations(PROOF, PROOF / "src/publication_counts.rs", DEPENDENCIES, {
        "atomic recovery ignores outstanding observations": (
            "if self.contains(instance.id()) && self.value(instance.id()) == 0 {",
            "if self.contains(instance.id()) {",
        ),
        "atomic recovery borrows an unregistered allocation count": (
            "if self.contains(instance.id()) && self.value(instance.id()) == 0 {",
            "if self.value(instance.id()) == 0 {",
        ),
        "atomic recovery accepts a foreign retirement ticket": (
            "requires self.inv(), retired.instance_id() == instance.id(),",
            "requires self.inv(),",
        ),
        "atomic recovery loses the final reader completion": (
            "counts.end(instance, receipt, observation);", "",
        ),
    })
    check_mutations(PROOF, PROOF / "src/observations.rs", DEPENDENCIES, {
        "live atomic reader count loses observation correspondence": (
            "self.observing == self.observations.len()", "true",
        ),
    })

    check_mutations(PROOF, Path("verification/verus/drain_gate/src/permit_shares.rs"), DEPENDENCIES, {
        "scope closes while owned observation shares remain": (
            "require(pre.outstanding == 0); remove scope -= Some(());",
            "require(pre.outstanding >= 0); remove scope -= Some(());",
        ),
        "observation share is issued without being counted": (
            "update outstanding = pre.outstanding + 1;", "update outstanding = pre.outstanding;",
        ),
        "finished admission share is not counted down": (
            "update outstanding = (pre.outstanding - 1) as nat;",
            "update outstanding = pre.outstanding;",
        ),
        "foreign scope consumes an admission share": (
            "requires old(self).inv(), share.inv(), share.scope_id() == old(self).id(),",
            "requires old(self).inv(), share.inv(),",
        ),
    })
    check_mutations(PROOF, PROOF / "src/retained_counts.rs", DEPENDENCIES, {
        "atomic count loses exhaustive admission share coverage": (
            "&& self.counts.value(id) == self.allocations[id].shares.len()\n            && (!self.retired", "&& true\n            && (!self.retired",
        ),
        "atomic observation fails to retain its admission share": (
            "allocation.shares.tracked_insert(key, share);", "",
        ),
        "reader completion returns a share from an unrelated scope": (
            "&& self.allocations[observation.node_id()].shares[observation.ticket.key()].scope_id() == observation.scope_id()",
            "&& true",
        ),
        "retained counter drain accepts arbitrary histories": (
            "counts.contains(id), stripes::drained_histories(histories),",
            "counts.contains(id),",
        ),
        "retained counter drain omits an admitted gate": (
            "super::super::coverage::$module::covers(counts.coverage(id), ledgers),\n        ensures counts.value(id) == 0,", "true,\n        ensures counts.value(id) == 0,",
        ),
        "scope releases before observation share completion": (
            "scope.finish(share);", "",
        ),
    })

    check_mutations(PROOF, PROOF / "src/retained_counts.rs", DEPENDENCIES, {
        "reader ticket forgets its exact gate and scope": (
            "&& self.ticket.value() == (self.gate_id(), self.scope_id())", "&& true",
        ),
        "reader index receipt authorizes another allocation": (
            "&& self.index_receipt.key() == self.node_id()", "&& true",
        ),
        "reader index receipt authorizes a foreign registry": (
            "self.index_receipt.instance_id() == index_registry", "true",
        ),
    })
    check_mutations(PROOF, PROOF / "src/atomic_publication.rs", DEPENDENCIES, {
        "atomic drain recovery accepts another slot registry": (
            "requires self.inv(), entry.inv(), entry.registry_id() == self.registry_id(),\n                super::rotation::drain::stripe_ownership",
            "requires self.inv(), entry.inv(),\n                super::rotation::drain::stripe_ownership",
        ),
        "atomic drain recovery accepts undrained histories": (
            "super::rotation::drain::stripe_ownership::$module::drained_histories(histories),\n                ledgers.matches", "true,\n                ledgers.matches",
        ),
        "reader end drops its share without completing scope": (
            "proof { scope.finish(share.get()); }", "proof { }",
        ),
        "reader end completes an unrelated scope": (
            "requires self.inv(), old(scope).inv(), old(scope).id() == self.scope_id(),",
            "requires self.inv(), old(scope).inv(),",
        ),
    })

    check_mutations(PROOF, PROOF / "src/observations.rs", DEPENDENCIES, {
        "retirement history permits publication to reappear": (
            "self.retired_history.dom().contains(()) == self.published.is_none()", "true",
        ),
    })
    check_mutations(PROOF, PROOF / "src/retained_counts.rs", DEPENDENCIES, {
        "narrowing expands a previously certified coverage bound": (
            "update coverage = pre.coverage.intersect(bound);", "update coverage = bound;",
        ),
        "narrowing accepts an allocation still admitting observations": (
            "old(counts).contains(id), old(counts).frozen(id),\n            rotation.inv()", "old(counts).contains(id),\n            rotation.inv()",
        ),
        "narrowing runs while a previous generation remains pending": (
            "rotation.inv(), rotation.pending.is_none(), ledgers.matches(rotation),",
            "rotation.inv(), ledgers.matches(rotation),",
        ),
        "pending recovery substitutes another allocation bound": (
            "bound.valid(counts.index_registry_id(), instance.id()),\n            rotation.inv()", "true,\n            rotation.inv()",
        ),
        "pending recovery treats the current generation as drained": (
            "counts.coverage(id).subset_of(Set::empty().insert(ledgers.selected_id(!rotation.current))),",
            "counts.coverage(id).subset_of(Set::empty().insert(ledgers.selected_id(rotation.current))),",
        ),
    })
    check_mutations(PROOF, PROOF / "src/atomic_publication.rs", DEPENDENCIES, {
        "atomic narrowing has no retirement history evidence": (
            "g.counts.note_retired(*entry.history.borrow());\n                bound = Some(super::retained_counts::$module::narrow_before_rotation", "bound = Some(super::retained_counts::$module::narrow_before_rotation",
        ),
        "atomic pending recovery accepts a foreign coverage registry": (
            "bound.valid(self.index_registry_id(), entry.node_id()),\n                rotation.inv(), ledgers.matches(rotation), rotation.pending == Some(!rotation.current),\n", "true,\n                rotation.inv(), ledgers.matches(rotation), rotation.pending == Some(!rotation.current),\n",
        ),
    })

    check_mutations(PROOF, PROOF / "src/atomic_publication.rs", DEPENDENCIES, {
        "coverage narrowing happens after generation publication": (
            '            let tracked mut bound = None;\n            atomic_with_ghost!(self.atomic => no_op(); ghost g => {\n                g.counts.remembered(entry.receipt.borrow());\n                g.counts.note_retired(*entry.history.borrow());\n                bound = Some(super::retained_counts::$module::narrow_before_rotation(\n                    &mut g.counts, entry.node_id(), rotation, ledgers));\n            });\n            super::rotation::refinement::$module::shared_begin_and_publication(rotation);\n',
            '            super::rotation::refinement::$module::shared_begin_and_publication(rotation);\n            let tracked mut bound = None;\n            atomic_with_ghost!(self.atomic => no_op(); ghost g => {\n                g.counts.remembered(entry.receipt.borrow());\n                g.counts.note_retired(*entry.history.borrow());\n                bound = Some(super::retained_counts::$module::narrow_before_rotation(\n                    &mut g.counts, entry.node_id(), rotation, ledgers));\n            });\n',
        ),
    })

    check_mutations(PROOF, Path("verification/verus/drain_gate/src/stripe_ownership.rs"), DEPENDENCIES, {
        "generation stripe identity set omits its final stripe": (
            "ledgers.gates.map(|index: int, gate: admission::Instance| gate.id()).to_set()",
            "if ledgers.gates.len() == 0 { Set::empty() } else { ledgers.gates.drop_last().map(|index: int, gate: admission::Instance| gate.id()).to_set() }",
        ),
    })
    check_mutations(PROOF, PROOF / "src/retained_counts.rs", DEPENDENCIES, {
        "stripe narrowing lacks evidence for an excluded gate": (
            "super::super::coverage::$module::covers(old(counts).domain().difference(keep), idle),", "true,",
        ),
        "stripe narrowing excludes a generation that is not drained": (
            "stripes::drained_histories(histories), idle.matches(stripes::final_states(histories)),",
            "idle.matches(stripes::final_states(histories)),",
        ),
        "stripe recovery omits a gate from the persistent bound": (
            "super::super::coverage::$module::covers(bound.limit(), pending),", "true,",
        ),
    })
    check_mutations(PROOF, PROOF / "src/atomic_publication.rs", DEPENDENCIES, {
        "atomic stripe partition omits part of the allocation domain": (
            "self.domain() == super::rotation::drain::stripe_ownership::$module::domain(kept).union(\n                    super::rotation::drain::stripe_ownership::$module::domain(idle)),", "true,",
        ),
        "atomic stripe recovery drains a different identity set": (
            "bound.limit() == super::rotation::drain::stripe_ownership::$module::domain(pending),\n            ensures memory@ == entry.memory(),",
            "true,\n            ensures memory@ == entry.memory(),",
        ),
    })

    check_mutations(PROOF, PROOF / "src/queued_retirement.rs", DEPENDENCIES, {
        "queued atomic retirement accepts another counter registry": (
            "requires slot.inv(), record.inv(), record.registry_id() == slot.registry_id(),",
            "requires slot.inv(), record.inv(),",
        ),
        "queued atomic retirement accepts another completion owner": (
            "record.owner() == domain.identity, slot.owner() == domain.identity,",
            "slot.owner() == domain.identity,",
        ),
        "queued preparation skips the final retirement": (
            "while index < records.len()", "while index < records.len() && index != records.len() - 1",
        ),
        "queued preparation discards the certified bound": (
            "let bound = entry.slot.$narrow(&entry.record, Ghost(kept_raw), Tracked(kept), Ghost(idle_histories), Tracked(idle));\n        entry.bound = Tracked(Some(bound.get()));",
            "let bound = entry.slot.$narrow(&entry.record, Ghost(kept_raw), Tracked(kept), Ghost(idle_histories), Tracked(idle));\n        entry.bound = Tracked(None);",
        ),
        "queued recovery uses a different pending stripe domain": (
            "requires entry.inv(), entry.prepared_for(stripes::domain(pending)),", "requires entry.inv(),",
        ),
        "batch recovery stops with one unrecovered allocation": (
            "while records.len() > 0", "while records.len() > 1",
        ),
    })

    check_mutations(PROOF, PROOF / "src/queued_retirement.rs", DEPENDENCIES, {
        "queue lock protects completion address instead of rotation address": (
            "QueueContents { domain: domain.rotation, index, records }", "QueueContents { domain: domain.identity, index, records }",
        ),
        "publication omits protected retirement preparation": (
            "        prepare_barrier_queue(queue, lock, handle, model, domain, Ghost(kept_raw), Tracked(kept), Ghost(idle_histories), Tracked(idle));\n", "",
        ),
        "protected retirement preparation follows generation publication": ('        prepare_barrier_queue(queue, lock, handle, model, domain, Ghost(kept_raw), Tracked(kept), Ghost(idle_histories), Tracked(idle));\n        let ticket = super::super::rotation::barrier_ownership::freeze(queue, lock, Tracked(ready), Ghost(stripes::domain(kept)));\n        let barrier = super::super::rotation::barrier_ownership::HeldBarrier::issue(queue, lock, handle);\n        super::super::rotation::barrier_ownership::$module::publish(model, transition, transition_handle, &barrier);\n', '        let barrier = super::super::rotation::barrier_ownership::HeldBarrier::issue(queue, lock, handle);\n        super::super::rotation::barrier_ownership::$module::publish(model, transition, transition_handle, &barrier);\n        prepare_barrier_queue(queue, lock, handle, model, domain, Ghost(kept_raw), Tracked(kept), Ghost(idle_histories), Tracked(idle));\n        let ticket = super::super::rotation::barrier_ownership::freeze(queue, lock, Tracked(ready), Ghost(stripes::domain(kept)));\n'),
        "prepared queue snapshot loses a retirement": (
            "let ghost prepared = queue.records@;", "let ghost prepared = queue.records@.drop_last();",
        ),
    })
    check_mutations(PROOF, Path("verification/verus/rotating_read_domain/src/barrier_ownership.rs"), DEPENDENCIES, {
        "queue invariant forgets final payload": (
            "0 <= i < state.records.len() ==> (self.payload_inv)",
            "0 <= i < state.records.len() - 1 ==> (self.payload_inv)",
        ),
    })

    check_mutations(PROOF, PROOF / "src/queued_retirement.rs", DEPENDENCIES, {
        "terminal heap recovery omits a gate from the drain": (
            "super::super::coverage::$module::covers(entry.gates(), ledgers),", "true,",
        ),
        "terminal heap batch loses an allocation": (
            "while !records.is_empty()", "while records.len() > 1",
        ),
    })

    check_mutations(PROOF, PROOF / "src/batches.rs", DEPENDENCIES, {
        "withdrawal wrapper drops extracted records": (
            "let records = withdrawal.into_records();", "let _records = withdrawal.into_records(); let records = Vec::new();",
        ),
        "withdrawal wrapper truncates its source snapshot": (
            "source: Ghost(source), index: Ghost(index) }", "source: Ghost(source.drop_last()), index: Ghost(index) }",
        ),
    })

    check_mutations(PROOF, PROOF / "src/queued_retirement.rs", DEPENDENCIES, {
        "pending queue ticket names another stripe domain": (
            "prepared.instance_id() == lock.pred().preparation.id(), prepared.value() == stripes::domain(pending),",
            "prepared.instance_id() == lock.pred().preparation.id(),",
        ),
    })
    check_mutations(PROOF, Path("verification/verus/rotating_read_domain/src/barrier_ownership.rs"), DEPENDENCIES, {
        "frozen queue payload loses prepared coverage": (
            "&& (state.preparation@.value().is_some() ==> forall|i: int| 0 <= i < state.records.len()\n            ==> (self.prepared_inv)(#[trigger] state.records@[i], state.preparation@.value().unwrap()))", "&& true",
        ),
    })

    check_mutations(PROOF, PROOF / "src/queued_retirement.rs", DEPENDENCIES, {
        "handle publication accepts a foreign transition handle": (
            "requires transition.inv(*old(state)), transition_handle.rwlock() == *transition,",
            "requires transition.inv(*old(state)),",
        ),
        "handle publication reopens an unsealed replacement": (
            "old(state).pending().is_none(), old(state).sealed(!lock.pred().index), transition.pred().domain == domain.rotation,",
            "old(state).pending().is_none(), transition.pred().domain == domain.rotation,",
        ),
        "handle publication uses another old counter authority": (
            "zero.inv(), one.inv(), transition.pred().zero == zero.authority_id(), transition.pred().one == one.authority_id(),",
            "zero.inv(), one.inv(), transition.pred().one == one.authority_id(),",
        ),
        "atomic queue publication skips locked current recheck": (
            "if current.recheck(&queue, lock, &handle) != queue.index {", "if false {",
        ),
        "atomic queue publication omits record preparation": (
            "        prepare_records(&mut queue.records, domain, Ghost(kept_raw), Tracked(kept), Ghost(idle_histories), Tracked(idle));\n        let ghost atomic_source = queue.records@;",
            "        let ghost atomic_source = queue.records@;",
        ),
        "atomic queue publication supplies another ready authority": (
            "\n            next_ready.instance_id() == current.gate(!lock.pred().index),", "\n            true,",
        ),
    })

    check_mutations(PROOF, PROOF / "src/admitted_read.rs", DEPENDENCIES, {
        "atomic admission reads outside its slot gate domain": (
            "requires counter.inv(), slot.inv(), slot.domain().contains(counter.id()),", "requires counter.inv(), slot.inv(),",
        ),
        "admitted read loses retained observation count": (
            "self.scope@.inv() && self.scope@.len() == 1", "self.scope@.inv()",
        ),
    })

    check_mutations(PROOF, PROOF / "src/retained_counts.rs", DEPENDENCIES, {
        "actual drain narrowing omits an excluded gate": (
            "idle.inv(), idle.covers(old(counts).domain().difference(keep)),", "idle.inv(),",
        ),
        "actual drain leases omit an observed gate": (
            "requires counts.inv(), counts.contains(id), drains.inv(), drains.covers(counts.coverage(id)),",
            "requires counts.inv(), counts.contains(id), drains.inv(),",
        ),
    })
    check_mutations(PROOF, PROOF / "src/atomic_publication.rs", DEPENDENCIES, {
        "actual drain recovery uses another atomic registry": (
            "requires self.inv(), retired.inv(), retired.registry_id() == self.registry_id(), drains.inv(), drains.covers(self.domain()),",
            "requires self.inv(), retired.inv(), drains.inv(), drains.covers(self.domain()),",
        ),
        "bounded drain recovery omits part of the persistent bound": (
            "bound.valid(self.index_registry_id(), retired.node_id()), drains.inv(), drains.covers(bound.limit()),\n        ensures memory@ == retired.memory(),",
            "bound.valid(self.index_registry_id(), retired.node_id()), drains.inv(),\n        ensures memory@ == retired.memory(),",
        ),
    })
    check_mutations(PROOF, Path("verification/verus/drain_gate/src/atomic_counter.rs"), DEPENDENCIES, {
        "drain set accepts unrelated gate leases": (
            "==> self.leases[id].inv() && self.leases[id].gate_id() == id", "==> self.leases[id].inv()",
        ),
        "drain set loses an inserted linear lease": (
            "self.leases.tracked_insert(lease.gate_id(), lease);", "",
        ),
    })

    check_mutations(PROOF, PROOF / "src/queued_retirement.rs", DEPENDENCIES, {
        "collected preparation skips the final retirement": (
            "while next < records.len()", "while next < records.len() && records.len() - next > 1",
        ),
        "collected preparation discards the persistent bound": (
            "let bound = entry.slot.narrow_drain_leases(&entry.record, Ghost(keep), Tracked(idle));\n    entry.bound = Tracked(Some(bound.get()));",
            "let bound = entry.slot.narrow_drain_leases(&entry.record, Ghost(keep), Tracked(idle));\n    entry.bound = Tracked(None);",
        ),
        "collected preparation skips actual drain observation": (
            "if !collection.poll_all(counters) { return Err(collection); }", "if false { return Err(collection); }",
        ),
        "collected preparation returns controllers before restoration": (
            "collection.restore_all(counters);\n        Ok(collection.into_controls(counters))", "Ok(collection.into_controls(counters))",
        ),
        "collected preparation omits an excluded counter": (
            "forall|gate: vstd::tokens::InstanceId| #[trigger] gates.difference(keep).contains(gate)\n                ==> exists|i: int| 0 <= i < counters.len() && counters@[i].id() == gate,",
            "true,",
        ),
    })

    check_mutations(PROOF, PROOF / "src/queued_retirement.rs", DEPENDENCIES, {
        "collected queue preparation accepts a foreign lock handle": (
            "requires collection.inv(counters@), handle.rwlock() == *lock,",
            "requires collection.inv(counters@),",
        ),
        "collected queue preparation overwrites a frozen bound": (
            "lock.inv(*old(queue)), old(queue).preparation@.value().is_none(),\n            old(queue).domain == domain.rotation,",
            "lock.inv(*old(queue)),\n            old(queue).domain == domain.rotation,",
        ),
        "collected queue preparation lacks the protected payload invariant": (
            "(entry.inv() && entry.domain() == domain && entry.gates() == gates),\n            forall|gate: vstd::tokens::InstanceId| #[trigger] gates.difference(keep).contains(gate)",
            "true,\n            forall|gate: vstd::tokens::InstanceId| #[trigger] gates.difference(keep).contains(gate)",
        ),
    })

    check_mutations(PROOF, PROOF / "src/queued_retirement.rs", DEPENDENCIES, {
        "collected reservation skips locked generation recheck": (
            "if current.recheck(&queue, lock, handle) != queue.index {", "if false {",
        ),
        "collected reservation freezes a different coverage bound": (
            "Ok(controls) => (current.reserve(queue, lock, handle, Ghost(keep)), Ok(controls)),",
            "Ok(controls) => (current.reserve(queue, lock, handle, Ghost(Set::empty())), Ok(controls)),",
        ),
        "collected reservation proceeds while drain is busy": (
            "Err(remaining) => (Err(queue), Err(remaining)),",
            "Err(remaining) => (current.reserve(queue, lock, handle, Ghost(keep)), Err(remaining)),",
        ),
    })

    check_mutations(PROOF, PROOF / "src/queued_retirement.rs", DEPENDENCIES, {
        "striped handle preparation accepts an unsealed next generation": (
            "state.sealed(transition.pred().counters(!lock.pred().index), !lock.pred().index),", "true,",
        ),
        "striped handle preparation uses another rotation domain": (
            "state.sealed(transition.pred().counters(!lock.pred().index), !lock.pred().index),\n            transition.pred().zero == zero@, transition.pred().one == one@, transition.pred().domain == domain.rotation,",
            "state.sealed(transition.pred().counters(!lock.pred().index), !lock.pred().index),\n            transition.pred().zero == zero@, transition.pred().one == one@,",
        ),
    })

    check_mutations(PROOF, PROOF / "src/queued_retirement.rs", DEPENDENCIES, {
        "striped retry accepts a different handoff generation": (
            "requires handoff.inv(), handoff.lock() == *transition, handoff.index() == lock.pred().index,",
            "requires handoff.inv(), handoff.lock() == *transition,",
        ),
        "striped retry accepts an unrelated queue handle": (
            "transition_handle.rwlock() == *transition, lock.inv(queue), handle.rwlock() == *lock,",
            "transition_handle.rwlock() == *transition, lock.inv(queue),",
        ),
    })

    check_mutations(PROOF, PROOF / "src/queued_retirement.rs", DEPENDENCIES, {
        "pending recovery skips actual drain collection": (
            "if !collection.poll_all(counters) { return Err((handoff, collection, Tracked(prepared))); }",
            "if false { return Err((handoff, collection, Tracked(prepared))); }",
        ),
        "pending recovery uses a different prepared coverage set": (
            "prepared.instance_id() == lock.pred().preparation.id(), prepared.value() == atomic_stripes::gate_ids(counters@),",
            "prepared.instance_id() == lock.pred().preparation.id(),",
        ),
        "pending lease recovery omits the final allocation": (
            "while records.len() != 0", "while records.len() > 1",
        ),
    })

#!/usr/bin/env python3
"""Require the DrainGate proof to reject unsafe shared-control-flow mutations.

Each mutation is checked in a temporary source tree; production is never edited.
Compiler/parser failures do not count as successful negative verification,
except for explicitly named ownership-order mutations rejected by Rust E0382.
"""

from pathlib import Path
import re
import shutil
import subprocess
import tempfile


ROOT = Path(__file__).resolve().parents[1]
PROTOCOL = Path("crates/xlfn-kernel/src/drain_gate/protocol.rs")
PROOF = Path("verification/verus/drain_gate")
TRANSITIONS = Path("crates/xlfn-kernel/src/sealable_counter/transitions.rs")
MUTATIONS = {
    "release before lock": (
        "let $guard = $lock;\n        let $outcome = $release;",
        "let $outcome = $release;\n        let $guard = $lock;",
    ),
    "missing notification": ("$notify;", "() ;"),
    "notification after unlock": (
        "        if let ReleaseOutcome::BecameIdle = $outcome {\n"
        "            $notify;\n        }\n        $unlock;",
        "        $unlock;\n"
        "        if let ReleaseOutcome::BecameIdle = $outcome {\n"
        "            $notify;\n        }",
    ),
    "return while active": ("while $active != 0", "while false"),
    "stripe rollback omits undo": (
        "                    $undo;", "                    ();",
    ),
    "stripe rollback skips last sealed stripe": (
        "while $rollback < $index", "while $rollback < $index && $index - $rollback > 1",
    ),
    "stripe rollback includes failed stripe": (
        "while $rollback < $index", "while $rollback <= $index",
    ),
    "stripe scan skips final stripe": (
        "while $index < $len\n            $($annotations)*\n        {\n            if $observe",
        "while $index < $len && $len - $index > 1\n            $($annotations)*\n        {\n            if $observe",
    ),
    "stripe scan ignores busy stripe": (
        "                $idle = false;", "                $idle = true;",
    ),
    "stripe scan forgets earlier busy stripe": (
        "if $observe != 0 {\n                $idle = false;\n            }",
        "if $observe != 0 {\n                $idle = false;\n            } else { $idle = true; }",
    ),
    "missing registration after wake": (
        "$guard = $wait;\n            $active = $observe;",
        "$guard = $wait;",
    ),
}



def is_verification_failure(returncode: int, output: str) -> bool:
    """Require a failed proof result plus a specific verifier diagnostic."""
    proof_failure = any(marker in output for marker in (
        "precondition not satisfied", "postcondition not satisfied",
        "invariant not satisfied", "assertion failed", "bitvector assertion not satisfied",
        "could not show invariant", "Cannot show invariant holds at end of block",
        "constructed value may fail to meet its declared type invariant",
        "unable to prove assertion safety condition",
    ))
    verified_failure = re.search(r"verification results:: \d+ verified, [1-9]\d* errors", output)
    return returncode != 0 and proof_failure and verified_failure is not None


def is_idle_callback_ownership_rejection(returncode: int, output: str) -> bool:
    """The reordered callback must move its linear handoff before borrowing it."""
    return returncode != 0 and "error[E0382]: borrow of moved value: `detached`" in output


def check_mutations(proof: Path, protocol: Path, dependencies: tuple[Path, ...],
                    mutations: dict[str, tuple[str, str]],
                    ownership_rejections: frozenset[str] = frozenset()) -> None:
    source = (ROOT / protocol).read_text()
    with tempfile.TemporaryDirectory(prefix="xlfn-refinement-") as directory:
        tree = Path(directory)
        shutil.copytree(ROOT / proof, tree / proof)
        for relative in (protocol, *dependencies):
            (tree / relative).parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(ROOT / relative, tree / relative)
        command = ["verus", "--crate-type=lib", str(proof / "src/lib.rs")]
        baseline = subprocess.run(command, cwd=tree, capture_output=True, text=True, timeout=120)
        baseline_output = baseline.stdout + baseline.stderr
        if baseline.returncode != 0 or re.search(r"verification results:: [1-9]\d* verified, 0 errors", baseline_output) is None:
            raise SystemExit(f"FAIL: unmodified baseline must verify before mutations:\n{baseline_output}")
        for name, (before, after) in mutations.items():
            if source.count(before) != 1:
                raise SystemExit(f"FAIL: mutation anchor changed: {name}")
            (tree / protocol).write_text(source.replace(before, after))
            result = subprocess.run(
                command,
                cwd=tree, capture_output=True, text=True, timeout=120,
            )
            output = result.stdout + result.stderr
            if name in ownership_rejections:
                if not is_idle_callback_ownership_rejection(result.returncode, output):
                    raise SystemExit(f"FAIL: {name} was not rejected by ownership checking:\n{output}")
                print(f"PASS: ownership rejected {name}", flush=True)
                continue
            if not is_verification_failure(result.returncode, output):
                raise SystemExit(f"FAIL: {name} did not fail verification:\n{output}")
            print(f"PASS: rejected {name}", flush=True)


def main() -> None:
    check_mutations(PROOF, PROTOCOL, (TRANSITIONS,), MUTATIONS)
    check_mutations(PROOF, PROOF / "src/stripe_ownership.rs", (PROTOCOL, TRANSITIONS), {
        "stripe history starts before sealing": (
            "histories[i][0] & $sealed != 0", "true",
        ),
        "stripe history starts before zero observation": (
            "histories[i][0] & $mask == 0", "true",
        ),
        "stripe ledger count differs from its raw counter": (
            "self.counts[i].value() == (raw[i] & $mask) as nat)",
            "self.counts[i].value() == (raw[i] & $mask) as nat + 1)",
        ),
        "all-stripe idle excludes a permit from another gate": (
            "            permit.instance_id() == ledgers.gates[index].id(),", "            true,",
        ),
    })
    check_mutations(PROOF, PROOF / "src/permits.rs", (PROTOCOL, TRANSITIONS), {
        "release preserves count after consuming permit": (
            "update active = (pre.active - 1) as nat;", "update active = pre.active;",
        ),
        "acquire miscounts its issued permit": (
            "update active = pre.active + 1;", "update active = pre.active + 2;",
        ),
        "counter release omits permit ledger release": (
            "gate.release(active, permit);", "();",
        ),
    })
    check_mutations(PROOF, TRANSITIONS, (PROTOCOL,), {
        "idle seal admits an active counter": (
            "if $state & ($sealed | $mask) == 0", "if $state & $sealed == 0",
        ),
        "idle seal CAS does not set sealed": (
            "state, next, Ordering::AcqRel, Ordering::Acquire",
            "state, state, Ordering::AcqRel, Ordering::Acquire",
        ),
        "idle seal CAS weakens success ordering": (
            "state, next, Ordering::AcqRel, Ordering::Acquire",
            "state, next, Ordering::Relaxed, Ordering::Acquire",
        ),
        "idle seal rollback discards waiting": (
            "Some($state & !$sealed)", "Some(0)",
        ),
        "waiting registration uses relaxed ordering": (
            "$state.fetch_or($waiting, Ordering::AcqRel) & $mask",
            "$state.fetch_or($waiting, Ordering::Relaxed) & $mask",
        ),
        "waiting registration omits waiting bit": (
            "$state.fetch_or($waiting, Ordering::AcqRel) & $mask",
            "$state.fetch_or(0, Ordering::AcqRel) & $mask",
        ),
        "waiting observation fabricates zero": (
            "$state.fetch_or($waiting, Ordering::AcqRel) & $mask",
            "$state.fetch_or($waiting, Ordering::AcqRel) & 0",
        ),
    })

    check_mutations(PROOF, PROOF / "src/refinement.rs", (PROTOCOL, TRANSITIONS), {
        "sealed history permits reopen": (
            "    ||| after == before\n",
            "    ||| after == before\n    ||| $reopen_spec(before) == TransitionOutcome::Success(after)\n",
        ),
        "sealed history permits count resurrection": (
            "    ||| after == before\n",
            "    ||| after == before\n    ||| after == before.wrapping_add(1)\n",
        ),
    })


    check_mutations(PROOF, PROOF / "src/atomic_counter.rs", (PROTOCOL, TRANSITIONS), {
        "atomic drain freezes a nonzero observation": (
            "if raw & $mask == 0 {\n                    let tracked mut control", "if true {\n                    let tracked mut control",
        ),
        "atomic drain lacks sealed authority": (
            "requires self.inv(), control.instance_id() == self.authority_id(), control.value(),",
            "requires self.inv(), control.instance_id() == self.authority_id(),",
        ),
        "atomic restore accepts another counter drain lease": (
            "pub fn restore(&self, Tracked(lease): Tracked<DrainLease>) -> (control: Tracked<lifecycle::control>)\n            requires self.inv(), self.accepts_lease(lease),",
            "pub fn restore(&self, Tracked(lease): Tracked<DrainLease>) -> (control: Tracked<lifecycle::control>)\n            requires self.inv(),",
        ),
        "frozen atomic forgets zero and sealed state": (
            "state.frozen.value().is_some() ==> raw & $mask == 0 && raw & $sealed != 0",
            "state.frozen.value().is_some() ==> true",
        ),
        "drain lease excludes a foreign admission share": (
            "requires self.inv(), share.inv(), share.gate_id() == self.gate_id(),",
            "requires self.inv(), share.inv(),",
        ),
        "controlled acquire discards sealed authority": (
            "self.acquire_observing(Tracked(Some(control)))", "self.acquire_observing(Tracked(None))",
        ),
        "atomic seal disagrees with its controller": (
            "\n                self.lifecycle.borrow().set(true, &mut state.sealed, control);", "\n                self.lifecycle.borrow().set(false, &mut state.sealed, control);",
        ),
        "failed idle-seal CAS changes lifecycle authority": (
            "if result is Ok {\n                            assert(next == raw | $sealed);",
            "if true {\n                            assert(next == raw | $sealed);",
        ),
        "successful idle-seal CAS omits lifecycle update": (
            "                            self.lifecycle.borrow().set(true, &mut state.sealed, &mut mode);",
            "                            ();",
        ),
        "idle-seal rollback accepts another counter lease": (
            "pub fn undo_idle_seal(&self, Tracked(lease): Tracked<DrainLease>) -> (control: Tracked<lifecycle::control>)\n            requires self.inv(), self.accepts_lease(lease),",
            "pub fn undo_idle_seal(&self, Tracked(lease): Tracked<DrainLease>) -> (control: Tracked<lifecycle::control>)\n            requires self.inv(),",
        ),
        "idle-seal rollback leaves lifecycle sealed": (
            "self.lifecycle.borrow().set(false, &mut state.sealed, &mut mode);",
            "self.lifecycle.borrow().set(true, &mut state.sealed, &mut mode);",
        ),
        "idle-seal rollback loses zero-count token": (
            "                                state.active = Some(payload.active);",
            "                                state.active = None;",
        ),
        "atomic reopen uses admission instead of sealed-zero transition": (
            "match $reopen(raw) {", "match $acquire(raw) {",
        ),
        "atomic sealed flag loses lifecycle agreement": (
            "&& state.sealed.instance_id() == key.1 && state.sealed.value() == (raw & $sealed != 0)", "&& state.sealed.instance_id() == key.1",
        ),
        "atomic sealed observation uses a foreign controller": (
            "requires self.inv(), control.instance_id() == self.authority_id(),\n            ensures (raw & $sealed != 0) == control.value(),", "requires self.inv(),\n            ensures (raw & $sealed != 0) == control.value(),",
        ),
        "atomic count loses permit conservation": (
            "state.active.unwrap().instance_id() == key.0 && state.active.unwrap().value() == (raw & $mask) as nat", "state.active.unwrap().instance_id() == key.0",
        ),
        "failed acquire CAS issues a permit": (
            "if result is Ok {\n                                assert((raw & $mask != $mask", "if true {\n                                assert((raw & $mask != $mask",
        ),
        "failed release CAS consumes a permit": (
            "if result is Ok {\n                                assert((raw & $mask > 0", "if true {\n                                assert((raw & $mask > 0",
        ),
        "atomic release accepts a foreign permit": (
            "requires self.inv(), permit.instance_id() == self.id(),", "requires self.inv(),",
        ),
        "atomic scope release ignores live observations": (
            "scope.gate_id() == self.id(), scope.len() == 0,", "scope.gate_id() == self.id(),",
        ),
    })


    check_mutations(PROOF, PROOF / "src/atomic_stripes.rs", (PROTOCOL, TRANSITIONS), {
        "stripe collection marks a busy counter drained": (
            "// A busy polled stripe retains its own controller.\n                    proof { self.controls.borrow_mut().tracked_insert(index as nat, control.get()); }\n                    assert forall",
            "// A busy polled stripe retains its own controller.\n                    proof { self.controls.borrow_mut().tracked_insert(index as nat, control.get()); }\n                    self.completed.set(index, true);\n                    assert forall",
        ),
        "stripe collection loses busy controller ownership": (
            "// A busy polled stripe retains its own controller.\n                    proof { self.controls.borrow_mut().tracked_insert(index as nat, control.get()); }\n                    assert forall",
            "// A busy polled stripe retains its own controller.\n                    assert forall",
        ),
        "stripe restoration skips the final counter": (
            "// Restore every polled stripe, including the final position.\n            while index < counters.len()",
            "// Restore every polled stripe, including the final position.\n            while index < counters.len() && index + 1 < counters.len()",
        ),
        "stripe drain view escapes before every counter is ready": (
            "requires self.inv(counters@), forall|i: int| 0 <= i < counters.len() ==> self.ready(i),",
            "requires self.inv(counters@),",
        ),
        "stripe collection accepts duplicate gate identities": (
            "// Polled collection resources remain within this exact vector.\n            self.completed.len() == counters.len() && distinct(counters) && self.drains@.inv()",
            "// Polled collection resources remain within this exact vector.\n            self.completed.len() == counters.len() && self.drains@.inv()",
        ),
    })


    check_mutations(PROOF, PROTOCOL, (TRANSITIONS,), {
        "stripe reopen skips the last counter": (
            "while $index < $len\n            $($annotations)*\n        {\n            if !$reopen",
            "while $index < $len && $len - $index > 1\n            $($annotations)*\n        {\n            if !$reopen",
        ),
        "stripe reopen ignores a rejected counter": (
            "if !$reopen { break; }", "if !$reopen { }",
        ),
    })
    check_mutations(PROOF, PROOF / "src/atomic_stripes.rs", (PROTOCOL, TRANSITIONS), {
        "stripe reopen uses an unrelated controller": (
            "old(controls)[index as nat].instance_id() == counters@[index as int].authority_id(), old(controls)[index as nat].value(),",
            "old(controls)[index as nat].value(),",
        ),
        "stripe reopen loses controller ownership": (
            "let opened = counters[index].reopen(Tracked(&mut control));\n        proof { controls.tracked_insert(index as nat, control); }",
            "let opened = counters[index].reopen(Tracked(&mut control));",
        ),
    })


    check_mutations(PROOF, PROOF / "src/atomic_stripes.rs", (PROTOCOL, TRANSITIONS), {
        "stripe collection accepts controllers outside its vector": (
            "requires distinct(counters@), controls.dom() == indices(counters@),\n                forall|i: int| #![auto] 0 <= i < counters.len() ==> counters@[i].inv()\n                    && controls.dom().contains(i as nat) && controls[i as nat].instance_id() == counters@[i].authority_id() && controls[i as nat].value(),",
            "requires distinct(counters@),\n                forall|i: int| #![auto] 0 <= i < counters.len() ==> counters@[i].inv()\n                    && controls.dom().contains(i as nat) && controls[i as nat].instance_id() == counters@[i].authority_id() && controls[i as nat].value(),",
        ),
        "stripe collection invariant allows unrelated controllers": (
            "// Polled collection resources remain within this exact vector.\n            self.completed.len() == counters.len() && distinct(counters) && self.drains@.inv()\n            && self.controls@.dom().subset_of(indices(counters))",
            "// Polled collection resources remain within this exact vector.\n            self.completed.len() == counters.len() && distinct(counters) && self.drains@.inv()",
        ),
        "stripe collection invariant allows unrelated drain leases": (
            "// Polled collection resources remain within this exact vector.\n            self.completed.len() == counters.len() && distinct(counters) && self.drains@.inv()\n            && self.controls@.dom().subset_of(indices(counters))\n            && self.drains@.domain().subset_of(gate_ids(counters))",
            "// Polled collection resources remain within this exact vector.\n            self.completed.len() == counters.len() && distinct(counters) && self.drains@.inv()\n            && self.controls@.dom().subset_of(indices(counters))",
        ),
    })


    check_mutations(PROOF, PROTOCOL, (TRANSITIONS,), {
        "stripe seal skips the final counter": (
            "while $index < $len\n            $($annotations)*\n        {\n            $seal;",
            "while $index < $len && $len - $index > 1\n            $($annotations)*\n        {\n            $seal;",
        ),
        "stripe seal omits the actual atomic action": (
            "            $seal;\n            $index += 1;", "            $index += 1;",
        ),
    })
    check_mutations(PROOF, PROOF / "src/atomic_stripes.rs", (PROTOCOL, TRANSITIONS), {
        "stripe seal accepts a foreign controller map": (
            "requires controls_owned(counters@, *old(controls)),", "requires true,",
        ),
    })

    check_mutations(PROOF, PROOF / "src/atomic_stripes.rs", (PROTOCOL, TRANSITIONS), {
        "idle stripe seals another counter's controller": (
            "match counters[index].try_seal_if_idle(Tracked(control))",
            "match counters[0].try_seal_if_idle(Tracked(control))",
        ),
        "idle stripe undoes another counter's lease": (
            "let control = counters[index].undo_idle_seal(Tracked(lease));",
            "let control = counters[0].undo_idle_seal(Tracked(lease));",
        ),
        "idle rollback leaves its stripe marked complete": (
            "let control = counters[index].undo_idle_seal(Tracked(lease));\n            proof { self.controls.borrow_mut().tracked_insert(index as nat, control.get()); }\n            proof { self.completed = Ghost(self.completed@.update(index as int, false)); }",
            "let control = counters[index].undo_idle_seal(Tracked(lease));\n            proof { self.controls.borrow_mut().tracked_insert(index as nat, control.get()); }\n            proof { self.completed = Ghost(self.completed@.update(index as int, true)); }",
        ),
        "idle lease conversion forgets a completed stripe": (
            "{ completed.push(true); }\n            let IdleCollection",
            "{ completed.push(false); }\n            let IdleCollection",
        ),
        "idle lease conversion drops its drain authority": (
            "let result = Collection { completed, controls, drains };",
            "let result = Collection { completed, controls, drains: Tracked(DrainSet::empty()) };",
        ),
    })


if __name__ == "__main__":
    main()

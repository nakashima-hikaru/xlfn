#!/usr/bin/env python3
"""Check the specific Rust lifetime fence, separately from negative SMT proofs."""
from pathlib import Path
import shutil
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]
PROOF = Path("verification/verus/rotating_read_domain")


def main() -> None:
    with tempfile.TemporaryDirectory(prefix="xlfn-transition-borrow-") as directory:
        tree = Path(directory)
        for source in (PROOF, Path("verification/verus/drain_gate")):
            shutil.copytree(ROOT / source, tree / source)
        for source in (
            Path("crates/xlfn/src/retirement_queue.rs"),
            Path("crates/xlfn-kernel/src/rotating_read_domain/protocol.rs"),
            Path("crates/xlfn-kernel/src/drain_gate/protocol.rs"),
            Path("crates/xlfn-kernel/src/sealable_counter/transitions.rs"),
        ):
            (tree / source).parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(ROOT / source, tree / source)

        command = ["verus", "--crate-type=lib", str(PROOF / "src/lib.rs")]
        baseline = subprocess.run(command, cwd=tree, capture_output=True, text=True, timeout=120)
        if baseline.returncode:
            raise SystemExit("FAIL: baseline must verify before checking lifetime rejection\n"
                             + baseline.stdout + baseline.stderr)

        path = tree / PROOF / "src/lock_ownership.rs"
        source = path.read_text()
        anchor = "                if let Some(batch) = detachment::"
        if source.count(anchor) != 1:
            raise SystemExit("FAIL: transition lifetime mutation anchor changed")
        path.write_text(source.replace(anchor, "                release(state, handle);\n" + anchor))
        rejected = subprocess.run(command, cwd=tree, capture_output=True, text=True, timeout=120)
        output = rejected.stdout + rejected.stderr
        required = (
            "error[E0505]: cannot move out of `handle` because it is borrowed",
            "error[E0505]: cannot move out of `state` because it is borrowed",
            "borrow later used here",
        )
        if rejected.returncode == 0 or not all(marker in output for marker in required):
            raise SystemExit("FAIL: expected specific live-certificate borrow errors\n" + output)
        print("PASS: borrow checker rejects transition release while certificate remains in use")

        path.write_text(source)
        path = tree / "crates/xlfn-kernel/src/rotating_read_domain/protocol.rs"
        source = path.read_text()
        anchor = "$publish;\n        $release_barrier;"
        if source.count(anchor) != 1:
            raise SystemExit("FAIL: publication barrier lifetime mutation anchor changed")
        path.write_text(source.replace(anchor, "$release_barrier;\n        $publish;"))
        rejected = subprocess.run(command, cwd=tree, capture_output=True, text=True, timeout=120)
        output = rejected.stdout + rejected.stderr
        required = (
            "error[E0505]: cannot move out of `handle` because it is borrowed",
            "error[E0505]: cannot move out of `queue` because it is borrowed",
            "atomic_rotation.rs",
            "error[E0382]: borrow of moved value: `queue_handle`",
            "borrow later used here",
        )
        if rejected.returncode == 0 or not all(marker in output for marker in required):
            raise SystemExit("FAIL: expected specific live-barrier borrow errors\n" + output)
        print("PASS: borrow checker rejects shared barrier release before publication")

        path.write_text(source)
        anchor = "            return $append;"
        if source.count(anchor) != 1:
            raise SystemExit("FAIL: registration guard lifetime mutation anchor changed")
        path.write_text(source.replace(anchor, "            $unlock; return $append;"))
        rejected = subprocess.run(command, cwd=tree, capture_output=True, text=True, timeout=120)
        output = rejected.stdout + rejected.stderr
        required = (
            "error[E0382]: use of moved value: `guard`",
            "atomic_registration.rs",
            "value used here after move",
        )
        if rejected.returncode == 0 or not all(marker in output for marker in required):
            raise SystemExit("FAIL: expected specific consumed-registration-guard error\n" + output)
        print("PASS: borrow checker rejects registration release before append")

        path.write_text(source)
        path = tree / "verification/verus/drain_gate/src/atomic_counter.rs"
        source = path.read_text()
        start = "            let tracked mut returned = None;"
        finish = "            Tracked(returned.tracked_unwrap())"
        if source.count(start) != 1 or source.count(finish) != 1:
            raise SystemExit("FAIL: drain lease lifetime mutation anchor changed")
        path.write_text(source.replace("    impl Counter {", "    impl Counter {\n        proof fn use_zero(tracked active: &admission::active) requires active.value() == 0 {}")
                        .replace(start, "            let tracked active = lease.active();\n" + start)
                        .replace(finish, "            proof { Self::use_zero(active); }\n" + finish))
        rejected = subprocess.run(command, cwd=tree, capture_output=True, text=True, timeout=120)
        output = rejected.stdout + rejected.stderr
        required = (
            "error[E0505]: cannot move out of `lease.ticket` because it is borrowed",
            "borrow later used here",
        )
        if rejected.returncode == 0 or not all(marker in output for marker in required):
            raise SystemExit("FAIL: expected specific borrowed-drain-lease error\n" + output)
        print("PASS: borrow checker rejects restoring a drain lease while zero authority is borrowed")


        path.write_text(source)
        path = tree / PROOF / "src/striped_rotation.rs"
        source = path.read_text()
        anchor = "    fn seal(state:"
        probe = """
    pub fn handoff_lifetime_probe<'a>(state: State, transition: &'a TransitionLock,
        handle: TransitionHandle<'a>, index: bool, counters: &Vec<Counter>)
        requires transition.inv(state), handle.rwlock() == *transition, state.pending().is_none(),
            counters@ == transition.pred().counters(!index), state.sealed(counters@, !index),
    {
        let (handoff, mut collection) = collect_next(state, transition, &handle, index, counters);
        let moved_handle = handle;
        collection.restore_all(counters);
        let controls = collection.into_controls(counters);
        let restored = handoff.restore(controls);
        moved_handle.release_write(restored);
    }
"""
        if source.count(anchor) != 1:
            raise SystemExit("FAIL: collection handoff lifetime mutation anchor changed")
        path.write_text(source.replace(anchor, probe + anchor))
        rejected = subprocess.run(command, cwd=tree, capture_output=True, text=True, timeout=120)
        output = rejected.stdout + rejected.stderr
        required = (
            "error[E0505]: cannot move out of `handle` because it is borrowed",
            "striped_rotation.rs",
            "borrow later used here",
        )
        if rejected.returncode == 0 or not all(marker in output for marker in required):
            raise SystemExit("FAIL: expected specific live-collection-handoff borrow error\n" + output)
        print("PASS: borrow checker rejects moving transition handle while collection handoff remains live")


        path.write_text(source)
        from check_handle_completion_refinement import PROOF as HANDLE_PROOF, DEPENDENCIES
        shutil.copytree(ROOT / HANDLE_PROOF, tree / HANDLE_PROOF, dirs_exist_ok=True)
        for dependency in DEPENDENCIES:
            (tree / dependency).parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(ROOT / dependency, tree / dependency)
        command = ["verus", "--crate-type=lib", str(HANDLE_PROOF / "src/lib.rs")]
        baseline = subprocess.run(command, cwd=tree, capture_output=True, text=True, timeout=120)
        if baseline.returncode:
            raise SystemExit("FAIL: Handle baseline must verify before finish-order rejection\n"
                             + baseline.stdout + baseline.stderr)
        path = tree / "crates/xlfn-kernel/src/rotating_read_domain/protocol.rs"
        source = path.read_text()
        anchor = "        let $result = $operation;\n        $clear;"
        if source.count(anchor) != 1:
            raise SystemExit("FAIL: pending finish lifetime mutation anchor changed")
        path.write_text(source.replace(anchor, "        $clear;\n        let $result = $operation;"))
        rejected = subprocess.run(command, cwd=tree, capture_output=True, text=True, timeout=120)
        output = rejected.stdout + rejected.stderr
        required = (
            "error[E0382]: borrow of moved value: `collection`",
            "queued_retirement.rs",
        )
        if rejected.returncode == 0 or not all(marker in output for marker in required):
            raise SystemExit("FAIL: expected specific collection use-after-finish error\n" + output)
        print("PASS: borrow checker rejects clearing pending before its collection-backed recovery callback")


        path.write_text(source)
        cache_proof = Path("verification/verus/cache_lease")
        shutil.copytree(ROOT / cache_proof, tree / cache_proof, dirs_exist_ok=True)
        for dependency in (Path("crates/xlfn/src/cache/pin_transitions.rs"),
                           Path("crates/xlfn/src/cache/node_layout.rs")):
            (tree / dependency).parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(ROOT / dependency, tree / dependency)
        command = ["verus", "--crate-type=lib", str(cache_proof / "src/lib.rs")]
        baseline = subprocess.run(command, cwd=tree, capture_output=True, text=True, timeout=120)
        if baseline.returncode:
            raise SystemExit("FAIL: Cache baseline must verify before admission lifetime rejection\n"
                             + baseline.stdout + baseline.stderr)
        path = tree / cache_proof / "src/atomic_admission.rs"
        source = path.read_text()
        anchor = "    pub fn observe<'scope, T>"
        probe = """
    #[verifier::exec_allows_no_decreases_clause]
    pub fn early_release_probe<T>(admission: Admission<'_>,
        Tracked(node): Tracked<&super::super::pin_ownership::cache_pins::Instance<super::super::heap_permission::HeapPermission<T>>>,
        Tracked(observing): Tracked<super::super::pin_ownership::cache_pins::observing<super::super::heap_permission::HeapPermission<T>>>,
        Tracked(resident): Tracked<&super::super::pin_ownership::cache_pins::pins<super::super::heap_permission::HeapPermission<T>>>)
        requires admission.inv(), observing.instance_id() == node.id(), observing.value() == 0,
            node.domain().contains(admission.gate_id()), resident.instance_id() == node.id(),
            resident.element().1 == super::super::pin_ownership::PinKind::Resident,
    {
        let tracked mut ledger = super::super::observation_coverage::ObservationLedger::new(node, observing);
        observe(Tracked(&mut ledger), Ghost(0), &admission, Tracked(node), Tracked(resident));
        admission.release();
        proof { ledger.end(0, node); }
    }
"""
        if source.count(anchor) != 1:
            raise SystemExit("FAIL: Cache admission lifetime mutation anchor changed")
        path.write_text(source.replace(anchor, probe + anchor))
        rejected = subprocess.run(command, cwd=tree, capture_output=True, text=True, timeout=120)
        output = rejected.stdout + rejected.stderr
        required = (
            "error[E0505]: cannot move out of `admission` because it is borrowed",
            "atomic_admission.rs", "borrow later used here",
        )
        if rejected.returncode == 0 or not all(marker in output for marker in required):
            raise SystemExit("FAIL: expected specific live-Cache-observation borrow error\n" + output)
        print("PASS: borrow checker rejects releasing atomic Cache admission before its observation ends")

        path.write_text(source)
        path = tree / "crates/xlfn-kernel/src/rotating_read_domain/protocol.rs"
        source = path.read_text()
        anchor = "        let $result = $operation;\n        $clear;"
        if source.count(anchor) != 1:
            raise SystemExit("FAIL: Cache pending finish lifetime mutation anchor changed")
        path.write_text(source.replace(anchor, "        $clear;\n        let $result = $operation;"))
        rejected = subprocess.run(command, cwd=tree, capture_output=True, text=True, timeout=120)
        output = rejected.stdout + rejected.stderr
        required = (
            "error[E0382]: borrow of moved value: `collection`",
            "queued_atomic.rs",
        )
        if rejected.returncode == 0 or not all(marker in output for marker in required):
            raise SystemExit("FAIL: expected specific Cache collection use-after-finish error\n" + output)
        print("PASS: borrow checker rejects clearing Cache pending before its recovery callback")

        path.write_text(source)
        path = tree / cache_proof / "src/queued_atomic.rs"
        source = path.read_text()
        anchor = "        pub fn observe(&self,"
        probe = """
        #[verifier::exec_allows_no_decreases_clause]
        pub fn ended_observation_probe(&self, Tracked(scope): Tracked<&mut Scope>, Tracked(ticket): Tracked<Ticket<T>>)
            requires self.inv(), old(scope).inv(), ticket.ledger_id() == self.observation_id(), ticket.scope_id() == old(scope).id(),
        {
            self.end_observation(Tracked(scope), Tracked(ticket));
            self.acquire_observed(Tracked(&ticket));
        }
"""
        if source.count(anchor) != 1:
            raise SystemExit("FAIL: retained Cache observation lifetime mutation anchor changed")
        path.write_text(source.replace(anchor, probe + anchor))
        rejected = subprocess.run(command, cwd=tree, capture_output=True, text=True, timeout=120)
        output = rejected.stdout + rejected.stderr
        required = (
            "error[E0382]: borrow of moved value: `ticket`",
            "queued_atomic.rs",
        )
        if rejected.returncode == 0 or not all(marker in output for marker in required):
            raise SystemExit("FAIL: expected specific ended-Cache-observation error\n" + output)
        print("PASS: borrow checker rejects acquiring a Cache pin after ending its observation")

        path.write_text(source)
        path = tree / "crates/xlfn/src/cache/pin_transitions.rs"
        source = path.read_text()
        anchor = "            match $acquire {"
        if source.count(anchor) != 1:
            raise SystemExit("FAIL: shared Cache lookup lifetime mutation anchor changed")
        path.write_text(source.replace(anchor, "            $leave;\n" + anchor))
        rejected = subprocess.run(command, cwd=tree, capture_output=True, text=True, timeout=120)
        output = rejected.stdout + rejected.stderr
        required = (
            "error[E0382]: borrow of moved value: `ticket`",
            "queued_atomic.rs",
        )
        if rejected.returncode == 0 or not all(marker in output for marker in required):
            raise SystemExit("FAIL: expected specific lookup-after-admission-release error\n" + output)
        print("PASS: borrow checker rejects leaving admission before shared Cache pin acquisition")

        path.write_text(source)
        path = tree / cache_proof / "src/resident_index.rs"
        source = path.read_text()
        anchor = "                let ticket = value.node.observe(Tracked(scope), Tracked(value.pin.borrow()));"
        if source.count(anchor) != 1:
            raise SystemExit("FAIL: Cache index read-guard lifetime mutation anchor changed")
        path.write_text(source.replace(anchor, "                guard.release_read();\n" + anchor))
        rejected = subprocess.run(command, cwd=tree, capture_output=True, text=True, timeout=120)
        output = rejected.stdout + rejected.stderr
        required = (
            "error[E0505]: cannot move out of `guard` because it is borrowed",
            "resident_index.rs", "borrow later used here",
        )
        if rejected.returncode == 0 or not all(marker in output for marker in required):
            raise SystemExit("FAIL: expected specific index-unlock-before-observation error\n" + output)
        print("PASS: borrow checker rejects releasing the index read guard before observation transfer")

        path.write_text(source)
        path = tree / cache_proof / "src/queued_atomic.rs"
        source = path.read_text()
        anchor = "        pub fn read<'a>(&'a self)"
        probe = """
        fn use_borrowed_value(_value: &T) {}
        pub fn release_while_borrowed_probe(lease: Self)
            requires lease.inv(),
        {
            let value = lease.read();
            let _retired = lease.release();
            Self::use_borrowed_value(value);
        }
"""
        if source.count(anchor) != 1:
            raise SystemExit("FAIL: Cache lease read lifetime mutation anchor changed")
        path.write_text(source.replace(anchor, probe + anchor))
        rejected = subprocess.run(command, cwd=tree, capture_output=True, text=True, timeout=120)
        output = rejected.stdout + rejected.stderr
        required = (
            "error[E0505]: cannot move out of `lease` because it is borrowed",
            "queued_atomic.rs", "borrow later used here",
        )
        if rejected.returncode == 0 or not all(marker in output for marker in required):
            raise SystemExit("FAIL: expected specific live-Cache-lease-reference error\n" + output)
        print("PASS: borrow checker rejects releasing a Cache lease while its typed reference remains live")


        path.write_text(source)
        path = tree / cache_proof / "src/inline_value.rs"
        source = path.read_text()
        anchor = "        pub fn read<'a>(&'a self)"
        probe = """
        fn use_inline_value(_value: &V) {}
        pub fn release_inline_while_borrowed_probe(lease: Self)
            requires lease.inv(),
        {
            let value = lease.read();
            let _retired = lease.release();
            Self::use_inline_value(value);
        }
"""
        if source.count(anchor) != 1:
            raise SystemExit("FAIL: inline Cache value lifetime mutation anchor changed")
        path.write_text(source.replace(anchor, probe + anchor))
        rejected = subprocess.run(command, cwd=tree, capture_output=True, text=True, timeout=120)
        output = rejected.stdout + rejected.stderr
        required = (
            "error[E0505]: cannot move out of `lease` because it is borrowed",
            "inline_value.rs", "borrow later used here",
        )
        if rejected.returncode == 0 or not all(marker in output for marker in required):
            raise SystemExit("FAIL: expected specific live-inline-value-reference error\n" + output)
        print("PASS: borrow checker rejects releasing the allocation while its inline value remains borrowed")


        path.write_text(source)
        path = tree / cache_proof / "src/queued_atomic.rs"
        source = path.read_text()
        anchor = "        pub fn recover(self, Tracked(drains): Tracked<&DrainSet>)"
        probe = """
        pub fn duplicate_retirement_recovery_probe(entry: Self, Tracked(drains): Tracked<&DrainSet>)
            requires entry.inv(), drains.inv(), drains.covers(entry.gates()),
        {
            let _first = entry.recover(Tracked(drains));
            let _second = entry.recover(Tracked(drains));
        }
"""
        if source.count(anchor) != 1:
            raise SystemExit("FAIL: Cache retirement recovery ownership anchor changed")
        path.write_text(source.replace(anchor, probe + anchor))
        rejected = subprocess.run(command, cwd=tree, capture_output=True, text=True, timeout=120)
        output = rejected.stdout + rejected.stderr
        required = ("error[E0382]: use of moved value: `entry`", "queued_atomic.rs")
        if rejected.returncode == 0 or not all(marker in output for marker in required):
            raise SystemExit("FAIL: expected duplicate-Cache-retirement recovery error\n" + output)
        print("PASS: borrow checker rejects recovering a Cache retirement entry twice")


        path.write_text(source)
        path = tree / cache_proof / "src/queued_atomic.rs"
        source = path.read_text()
        anchor = "        pub fn observed_resident(&self,"
        probe = """
        pub fn end_ticket_before_resident_load_probe(&self,
            pointer: *mut super::super::inline_value::Allocation<V>,
            Tracked(scope): Tracked<&mut Scope>,
            Tracked(ticket): Tracked<Ticket<super::super::inline_value::Allocation<V>>>)
            requires self.inv(), old(scope).inv(), ticket.ledger_id() == self.observation_id(),
                ticket.node_id() == self.id(), ticket.scope_id() == old(scope).id(),
                ticket.memory().ptr() == pointer, ticket.memory().is_init(),
        {
            let tracked observation = ticket.observation();
            let allocation = super::super::pin_ownership::borrow_from_observation(
                pointer, self.pins.instance(), Tracked(observation));
            self.end_observation(Tracked(scope), Tracked(ticket));
            let _resident = super::super::node_layout::resident!(allocation);
        }
"""
        if source.count(anchor) != 1:
            raise SystemExit("FAIL: Cache resident load permission anchor changed")
        path.write_text(source.replace(anchor, probe + anchor))
        rejected = subprocess.run(command, cwd=tree, capture_output=True, text=True, timeout=120)
        output = rejected.stdout + rejected.stderr
        required = ("error[E0505]: cannot move out of `ticket` because it is borrowed",
                    "queued_atomic.rs", "borrow later used here")
        if rejected.returncode == 0 or not all(marker in output for marker in required):
            raise SystemExit("FAIL: expected resident-load-after-ticket-consumption error\n" + output)
        print("PASS: borrow checker retains the reader observation through its resident atomic load")


if __name__ == "__main__":
    main()

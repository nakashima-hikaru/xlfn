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


if __name__ == "__main__":
    main()

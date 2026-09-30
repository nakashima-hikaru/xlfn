"""Exercise isolation and failure handling without depending on an installed verifier."""

from contextlib import redirect_stdout
import copy
import io
from pathlib import Path
from subprocess import CompletedProcess, TimeoutExpired
import tempfile
import threading
import unittest
from unittest.mock import patch

import verus_mutations as gate
from check_verus_reports import check_reports


GOOD = "verification results:: 3 verified, 0 errors"
BAD = "error: assertion failed\nverification results:: 2 verified, 1 errors"


class RunnerTests(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)
        (self.root / "proof/src").mkdir(parents=True)
        (self.root / "proof/src/lib.rs").write_text("proof")
        (self.root / "protocol.rs").write_text("alpha beta")
        (self.root / "dependency.rs").write_text("dependency")
        self.addCleanup(patch.stopall)
        patch.object(gate, "ROOT", self.root).start()

    def group(self, plan, mutations=None):
        plan.add_group(Path("proof"), Path("protocol.rs"), (Path("dependency.rs"),),
                       mutations or {"alpha": ("alpha", "ALPHA")})

    def run_plan(self, plan, invoke, jobs=2):
        with patch.object(gate.subprocess, "run", side_effect=invoke), redirect_stdout(io.StringIO()):
            return gate.MutationRunner(jobs, threads=1).run(plan)

    def test_groups_share_only_identical_baselines(self):
        plan = gate.MutationPlan()
        self.group(plan)
        self.group(plan, {"beta": ("beta", "BETA")})
        self.assertEqual(len(plan.snapshots), 1)
        (self.root / "dependency.rs").write_text("changed dependency")
        self.group(plan, {"changed dependency": ("alpha", "ALPHA")})
        self.assertEqual(len(plan.snapshots), 2)
        (self.root / "proof/src/extra.rs").write_text("new proof module")
        self.group(plan, {"changed proof": ("alpha", "ALPHA")})
        self.assertEqual(len(plan.snapshots), 3)
        baseline_inputs = []

        def invoke(command, cwd, **_kwargs):
            source = (cwd / "protocol.rs").read_text()
            if source == "alpha beta":
                baseline_inputs.append((cwd / "dependency.rs").read_text())
                return CompletedProcess(command, 0, GOOD, "")
            return CompletedProcess(command, 1, BAD, "")

        report = self.run_plan(plan, invoke)
        self.assertEqual(baseline_inputs, ["dependency", "changed dependency", "changed dependency"])
        self.assertEqual(len(report["baselines"]), 3)
        self.assertEqual(len(report["mutations"]), 4)

    def test_two_workers_never_share_mutable_sources(self):
        plan = gate.MutationPlan()
        self.group(plan, {"a1": ("alpha", "A1"), "a2": ("alpha", "A2")})
        self.group(plan, {"b1": ("beta", "B1"), "b2": ("beta", "B2")})
        barrier = threading.Barrier(2)
        lock = threading.Lock()
        trees = set()
        active = 0
        peak = 0
        baselines = 0

        def invoke(command, cwd, **kwargs):
            nonlocal active, peak, baselines
            self.assertEqual(command[:4], ["verus", "--crate-type=lib", "--num-threads", "1"])
            self.assertEqual(kwargs["timeout"], 120)
            source = (cwd / "protocol.rs").read_text()
            if source == "alpha beta":
                baselines += 1
                return CompletedProcess(command, 0, GOOD, "")
            self.assertEqual(baselines, 1)
            with lock:
                self.assertNotIn(cwd, trees)
                trees.add(cwd)
                active += 1
                peak = max(peak, active)
            barrier.wait(timeout=5)
            self.assertEqual((cwd / "protocol.rs").read_text(), source)
            self.assertEqual((cwd / "dependency.rs").read_text(), "dependency")
            self.assertEqual((cwd / "proof/src/lib.rs").read_text(), "proof")
            with lock:
                active -= 1
            return CompletedProcess(command, 1, BAD, "")

        report = self.run_plan(plan, invoke)
        self.assertEqual(peak, 2)
        self.assertEqual(len(trees), 4)
        self.assertEqual([m["name"] for m in report["mutations"]], ["a1", "a2", "b1", "b2"])
        self.assertEqual((self.root / "protocol.rs").read_text(), "alpha beta")
        self.assertTrue(all(not tree.exists() for tree in trees))

    def test_failing_baseline_prevents_all_mutations(self):
        plan = gate.MutationPlan()
        self.group(plan)
        calls = []

        def invoke(command, cwd, **_kwargs):
            calls.append((cwd / "protocol.rs").read_text())
            return CompletedProcess(command, 1, BAD, "")

        with self.assertRaisesRegex(SystemExit, "unmodified baseline must verify"):
            self.run_plan(plan, invoke)
        self.assertEqual(calls, ["alpha beta"])

    def test_compile_failure_or_unexpected_success_cannot_pass(self):
        for code, output in ((1, "error[E0308]: mismatched types"), (0, GOOD),
                             (1, "error: assertion failed")):
            with self.subTest(code=code, output=output):
                plan = gate.MutationPlan()
                self.group(plan)

                def invoke(command, cwd, **_kwargs):
                    if (cwd / "protocol.rs").read_text() == "alpha beta":
                        return CompletedProcess(command, 0, GOOD, "")
                    return CompletedProcess(command, code, output, "")

                with self.assertRaisesRegex(SystemExit, "did not fail verification"):
                    self.run_plan(plan, invoke)

    def test_timeout_cannot_pass(self):
        plan = gate.MutationPlan()
        self.group(plan)

        def invoke(command, cwd, **_kwargs):
            if (cwd / "protocol.rs").read_text() == "alpha beta":
                return CompletedProcess(command, 0, GOOD, "")
            raise TimeoutExpired(command, 120)

        with self.assertRaisesRegex(SystemExit, "verifier timed out"):
            self.run_plan(plan, invoke)

    def test_ownership_exception_stays_case_specific(self):
        plan = gate.MutationPlan()
        plan.add_group(Path("proof"), Path("protocol.rs"), (),
                       {"ownership": ("alpha", "ALPHA")}, frozenset({"ownership"}))

        def invoke(command, cwd, **_kwargs):
            if (cwd / "protocol.rs").read_text() == "alpha beta":
                return CompletedProcess(command, 0, GOOD, "")
            return CompletedProcess(command, 1, "error[E0382]: borrow of moved value: `detached`", "")

        self.assertEqual(len(self.run_plan(plan, invoke)["mutations"]), 1)
        ordinary = gate.MutationPlan()
        self.group(ordinary)
        with self.assertRaisesRegex(SystemExit, "did not fail verification"):
            self.run_plan(ordinary, invoke)

    def test_input_validation_happens_before_verification(self):
        for mutations, message in (({"missing": ("absent", "broken")}, "mutation anchor changed"),
                                   ({"noop": ("alpha", "alpha")}, "does not change source")):
            with self.subTest(mutations=mutations), patch.object(gate.subprocess, "run") as invoke:
                with self.assertRaisesRegex(SystemExit, message):
                    self.group(gate.MutationPlan(), mutations)
                invoke.assert_not_called()
        with self.assertRaisesRegex(SystemExit, "names have no mutation"):
            gate.MutationPlan().add_group(Path("proof"), Path("protocol.rs"), (),
                                         {"a": ("alpha", "A")}, frozenset({"unknown"}))

    def test_collection_does_not_invoke_verifier_and_restores_on_failure(self):
        plan = gate.MutationPlan()
        with patch.object(gate.subprocess, "run") as invoke:
            with self.assertRaisesRegex(RuntimeError, "probe"):
                with gate.collect_mutations(plan):
                    gate.check_mutations(Path("proof"), Path("protocol.rs"), (), {"a": ("alpha", "A")})
                    raise RuntimeError("probe")
            invoke.assert_not_called()
        self.assertEqual(len(plan.mutations), 1)
        self.assertIsNone(gate._collecting)

    def test_worker_settings_reject_zero(self):
        for jobs, threads in ((0, 1), (1, 0), (-1, None)):
            with self.assertRaises(ValueError):
                gate.MutationRunner(jobs, threads)

    def test_shards_cover_every_case_once_and_keep_baselines(self):
        plan = gate.MutationPlan()
        self.group(plan, {str(i): ("alpha", str(i)) for i in range(11)})
        for count in (1, 2, 4, 12):
            shards = [plan.shard(index, count) for index in range(count)]
            cases = [mutation for shard in shards for mutation in shard.mutations]
            self.assertCountEqual(cases, plan.mutations)
            self.assertEqual(len(cases), len(set(cases)))
            self.assertTrue(all(shard.snapshots == plan.snapshots for shard in shards))
        for index, count in ((0, 0), (-1, 4), (4, 4)):
            with self.assertRaises(ValueError):
                plan.shard(index, count)

    def test_snapshot_includes_linked_modules_and_rejects_parent_paths(self):
        (self.root / "linked").mkdir()
        (self.root / "linked/module.rs").write_text("linked module")
        (self.root / "proof/src/linked").symlink_to(self.root / "linked", target_is_directory=True)
        snapshot = gate.Snapshot.read(Path("proof"), ())
        self.assertEqual(dict(snapshot.files)[Path("proof/src/linked/module.rs")], b"linked module")
        with self.assertRaisesRegex(SystemExit, "must remain relative"):
            gate.Snapshot.read(Path("proof"), (Path("../escape.rs"),))

    def test_ci_reports_require_complete_exact_inputs(self):
        plan = gate.MutationPlan()
        self.group(plan, {str(i): ("alpha", str(i)) for i in range(7)})
        snapshots = [{"proof": str(s.proof), "sha256": s.digest, "verified": 3}
                     for s in plan.snapshots.values()]
        reports = [{"shard_index": i, "shard_count": 4, "total_mutations": 7,
                    "verus_version": "test-version", "baselines": snapshots,
                    "mutations": [{**m.describe(), "seconds": 1.0} for m in plan.shard(i, 4).mutations]}
                   for i in range(4)]
        check_reports(plan, reports, 4)
        with self.assertRaisesRegex(ValueError, "expected 4 shard reports"):
            check_reports(plan, reports[:-1], 4)
        changes = (
            (lambda r: r[0].update(shard_index=1), "shard indices"),
            (lambda r: r[0].update(shard_count=3), "incorrect shard count"),
            (lambda r: r[0].update(total_mutations=6), "inventory size"),
            (lambda r: r[0].update(verus_version="different-version"), "same Verus version"),
            (lambda r: r[0]["baselines"][0].update(sha256="wrong"), "baseline inputs"),
            (lambda r: r[0]["baselines"][0].update(verified=0), "baseline inputs"),
            (lambda r: r[0]["mutations"].pop(), "mutation inputs"),
            (lambda r: r[0]["mutations"].append(r[0]["mutations"][0]), "mutation inputs"),
            (lambda r: r[0]["mutations"][0].update(rejection="compiler-error"), "mutation inputs"),
            (lambda r: r[0]["mutations"][0].update(changed_source_sha256="wrong"), "mutation inputs"),
        )
        for change, message in changes:
            with self.subTest(message=message):
                changed = copy.deepcopy(reports)
                change(changed)
                with self.assertRaisesRegex(ValueError, message):
                    check_reports(plan, changed, 4)

"""Unit tests for the Verus proof runner."""

from __future__ import annotations

import subprocess
import unittest
from unittest.mock import patch

from xlfn_tools import check_verus_refinement as runner
from xlfn_tools._repository import ROOT


class VerusRunnerTests(unittest.TestCase):
    def test_every_configured_proof_entry_exists(self) -> None:
        for name in runner.PROOFS:
            self.assertTrue((ROOT / runner.proof_entry(name)).is_file(), name)

    def test_verify_proof_accepts_positive_verified_count(self) -> None:
        completed = subprocess.CompletedProcess(
            ["verus"],
            0,
            "verification results:: 12 verified, 0 errors\n",
            "",
        )
        with patch.object(runner.subprocess, "run", return_value=completed) as run:
            result = runner.verify_proof("drain_gate", threads=2)
        self.assertEqual(result["proof"], "drain_gate")
        self.assertEqual(result["verified"], 12)
        command = run.call_args.args[0]
        self.assertEqual(
            command,
            [
                "verus",
                "--crate-type=lib",
                "--num-threads",
                "2",
                "verification/verus/drain_gate/src/lib.rs",
            ],
        )

    def test_verify_proof_rejects_zero_verified_or_errors(self) -> None:
        for code, stdout in (
            (0, "verification results:: 0 verified, 0 errors\n"),
            (1, "verification results:: 10 verified, 1 errors\n"),
            (1, "error: parse failure\n"),
        ):
            completed = subprocess.CompletedProcess(["verus"], code, stdout, "")
            with (
                patch.object(runner.subprocess, "run", return_value=completed),
                self.assertRaises(SystemExit),
            ):
                runner.verify_proof("drain_gate", threads=1)

    def test_run_all_preserves_proof_order(self) -> None:
        completed = subprocess.CompletedProcess(
            ["verus"],
            0,
            "verification results:: 5 verified, 0 errors\n",
            "",
        )
        with patch.object(runner.subprocess, "run", return_value=completed):
            report = runner.run_all(jobs=2, threads=1)
        proofs = report["proofs"]
        assert isinstance(proofs, list)
        self.assertEqual([entry["proof"] for entry in proofs], list(runner.PROOFS))


if __name__ == "__main__":
    unittest.main()

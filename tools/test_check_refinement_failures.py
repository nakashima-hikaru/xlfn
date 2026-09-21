import unittest
from check_drain_gate_refinement import is_verification_failure


class VerificationFailureTests(unittest.TestCase):
    def test_atomic_invariant_failure_is_a_proof_failure(self):
        self.assertTrue(is_verification_failure(1,
            "verification results:: 417 verified, 1 errors\n"
            "error: Cannot show invariant holds at end of block"))

    def test_state_machine_assertion_safety_is_a_proof_failure(self):
        diagnostic = "error: unable to prove assertion safety condition"
        self.assertTrue(is_verification_failure(1,
            "verification results:: 512 verified, 1 errors\n" + diagnostic))
        self.assertFalse(is_verification_failure(1, diagnostic))

    def test_contract_failure_remains_recognized(self):
        self.assertTrue(is_verification_failure(1,
            "error: precondition not satisfied\nverification results:: 2 verified, 1 errors"))

    def test_compiler_failure_does_not_count(self):
        self.assertFalse(is_verification_failure(1,
            "error[E0308]: mismatched types\nerror: aborting due to 1 previous error"))

    def test_diagnostic_without_failed_verification_summary_does_not_count(self):
        self.assertFalse(is_verification_failure(1, "error: Cannot show invariant holds at end of block"))

    def test_success_does_not_count(self):
        self.assertFalse(is_verification_failure(0,
            "verification results:: 417 verified, 1 errors\nerror: assertion failed"))
        self.assertFalse(is_verification_failure(1,
            "verification results:: 417 verified, 0 errors\nerror: assertion failed"))


class BaselineGateTests(unittest.TestCase):
    def check_rejected_baseline(self, returncode, output):
        import tempfile
        from pathlib import Path
        from subprocess import CompletedProcess
        from unittest.mock import patch
        import check_drain_gate_refinement as gate
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "proof/src").mkdir(parents=True)
            (root / "proof/src/lib.rs").write_text("baseline")
            (root / "protocol.rs").write_text("original")
            with patch.object(gate, "ROOT", root), patch.object(gate.subprocess, "run",
                    return_value=CompletedProcess([], returncode, output, "")) as run:
                with self.assertRaisesRegex(SystemExit, "unmodified baseline must verify"):
                    gate.check_mutations(Path("proof"), Path("protocol.rs"), (), {"mutation": ("original", "broken")})
                run.assert_called_once()

    def test_existing_proof_failure_stops_before_mutations(self):
        self.check_rejected_baseline(1, "error: assertion failed\nverification results:: 5 verified, 1 errors")

    def test_exit_zero_without_verification_summary_is_not_a_baseline(self):
        self.check_rejected_baseline(0, "no verification performed")

import unittest

from xlfn_tools.verus_mutations import is_idle_callback_ownership_rejection, is_verification_failure


class VerificationFailureTests(unittest.TestCase):
    def test_proof_failure_requires_failure_exit_summary_and_recognized_diagnostic(self):
        failed = "verification results:: 512 verified, 1 errors"
        passed = "verification results:: 512 verified, 0 errors"
        invariant = "error: cannot show invariant holds at end of block"
        assertion = "error: unable to prove assertion safety condition"
        for status, output, expected in (
            (1, f"{failed}\n{invariant}", True),
            (1, f"{failed}\n{invariant.replace('cannot', 'Cannot')}", True),
            (1, f"{failed}\n{assertion}", True),
            (1, f"error: precondition not satisfied\n{failed}", True),
            (1, "error[E0308]: mismatched types", False),
            (1, f"{failed}\nerror[E0308]: mismatched types", False),
            (1, assertion, False),
            (1, invariant, False),
            (0, f"{failed}\n{invariant}", False),
            (1, f"{passed}\n{invariant}", False),
        ):
            with self.subTest(status=status, output=output):
                self.assertEqual(is_verification_failure(status, output), expected)

    def test_idle_callback_ownership_failure_is_narrow(self):
        diagnostic = "error[E0382]: borrow of moved value: `detached`"
        self.assertTrue(is_idle_callback_ownership_rejection(1, diagnostic))
        self.assertFalse(is_idle_callback_ownership_rejection(0, diagnostic))
        self.assertFalse(is_idle_callback_ownership_rejection(1, "error[E0308]: mismatched types"))


class BaselineGateTests(unittest.TestCase):
    def check_rejected_baseline(self, returncode, output):
        import tempfile
        from pathlib import Path
        from subprocess import CompletedProcess
        from unittest.mock import patch

        from xlfn_tools import verus_mutations as gate

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "proof/src").mkdir(parents=True)
            (root / "proof/src/lib.rs").write_text("baseline")
            (root / "protocol.rs").write_text("original")
            with (
                patch.object(gate, "ROOT", root),
                patch.object(
                    gate.subprocess,
                    "run",
                    return_value=CompletedProcess([], returncode, output, ""),
                ) as run,
            ):
                with self.assertRaisesRegex(SystemExit, "unmodified baseline must verify"):
                    gate.check_mutations(
                        Path("proof"), Path("protocol.rs"), (), {"mutation": ("original", "broken")}
                    )
                run.assert_called_once()

    def test_existing_proof_failure_stops_before_mutations(self):
        self.check_rejected_baseline(
            1, "error: assertion failed\nverification results:: 5 verified, 1 errors"
        )

    def test_exit_zero_without_verification_summary_is_not_a_baseline(self):
        self.check_rejected_baseline(0, "no verification performed")

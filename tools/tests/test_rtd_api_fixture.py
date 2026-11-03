"""The external consumer must fetch its own lock before offline checks."""

import contextlib
import importlib.util
import io
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location(
    "rtd_api_fixture", Path(__file__).resolve().parents[2] / "tests/rtd-api-fixture/check.py"
)
assert SPEC is not None and SPEC.loader is not None
CHECK = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CHECK)


class RtdApiFixtureTests(unittest.TestCase):
    def run_checker(self, run):
        with tempfile.TemporaryDirectory() as directory:
            with (
                patch(
                    "sys.argv", ["check.py", "--target-dir", directory, "--report-dir", directory]
                ),
                patch.object(CHECK.subprocess, "check_output", return_value="rustc test"),
                patch.object(CHECK.subprocess, "run", side_effect=run),
                contextlib.redirect_stdout(io.StringIO()) as output,
            ):
                result = CHECK.main()
            return result, output.getvalue()

    def test_fetch_uses_consumer_lock_before_offline_checks(self):
        calls = []

        def run(command, **kwargs):
            calls.append(command)
            manifest = Path(command[command.index("--manifest-path") + 1])
            self.assertEqual(
                manifest.with_name("Cargo.lock").read_bytes(),
                (CHECK.ROOT / "Cargo.lock").read_bytes(),
            )
            self.assertTrue((manifest.parent / "src/lib.rs").is_file())
            if command[1] == "fetch":
                self.assertEqual(len(calls), 1)
                self.assertTrue(kwargs["check"])
                self.assertIn("--locked", command)
                self.assertNotIn("--offline", command)
                return subprocess.CompletedProcess(command, 0)
            self.assertEqual(calls[0][1], "fetch")
            self.assertIn("--locked", command)
            self.assertIn("--offline", command)
            return subprocess.CompletedProcess(command, 101, "", "compiler failure details\n")

        result, output = self.run_checker(run)
        self.assertEqual(result, 1)
        self.assertEqual(len(calls), 21)
        self.assertIn("FAIL basic-core", output)
        self.assertIn("compiler failure details", output)

    def test_fetch_failure_stops_before_feature_checks(self):
        calls = []

        def run(command, **kwargs):
            calls.append(command)
            raise subprocess.CalledProcessError(101, command)

        with self.assertRaises(subprocess.CalledProcessError):
            self.run_checker(run)
        self.assertEqual(len(calls), 1)
        self.assertEqual(calls[0][1], "fetch")

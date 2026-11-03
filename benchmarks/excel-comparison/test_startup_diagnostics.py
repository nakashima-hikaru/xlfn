import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from startup_diagnostics import MAX_LOG_BYTES, read_startup_log_delta, snapshot_startup_log


class StartupDiagnosticsTest(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.environment = patch.dict(os.environ, {"LOCALAPPDATA": self.directory.name})
        self.environment.start()
        self.addCleanup(self.environment.stop)
        self.path = Path(self.directory.name) / "xlfn-excel-comparison" / "logs" / "startup.log"
        self.path.parent.mkdir(parents=True)

    def test_existing_sessions_are_excluded_from_json_serializable_delta(self):
        self.path.write_bytes(b"previous failure\n")
        baseline = snapshot_startup_log()
        with self.path.open("ab") as stream:
            stream.write("xlAutoOpen failed: 登録エラー\n".encode())
        result = read_startup_log_delta(baseline)
        self.assertEqual(result["text"], "xlAutoOpen failed: 登録エラー\n")
        self.assertFalse(result["file_reset"])
        self.assertEqual(result["skipped_bytes"], 0)
        json.dumps(result)

    def test_unchanged_log_does_not_repeat_an_old_failure(self):
        self.path.write_bytes(b"old failure\n")
        result = read_startup_log_delta(snapshot_startup_log())
        self.assertEqual(result["text"], "")
        self.assertEqual(result["bytes_added"], 0)

    def test_log_created_during_session_is_read(self):
        # Write exact bytes: diagnostics preserve LF and Windows CRLF alike.
        for newline in (b"\n", b"\r\n"):
            with self.subTest(newline=newline):
                self.path.unlink(missing_ok=True)
                baseline = snapshot_startup_log()
                content = b"new failure" + newline
                self.path.write_bytes(content)
                result = read_startup_log_delta(baseline)
                self.assertEqual(result["text"], content.decode("utf-8"))
                self.assertEqual(result["bytes_added"], len(content))

    def test_rotated_log_reads_new_file_only(self):
        for index, newline in enumerate((b"\n", b"\r\n")):
            with self.subTest(newline=newline):
                self.path.write_bytes(b"previous session" + newline)
                baseline = snapshot_startup_log()
                self.path.rename(self.path.with_suffix(f".log.{index}"))
                content = b"new failure after rotation" + newline
                self.path.write_bytes(content)
                result = read_startup_log_delta(baseline)
                self.assertTrue(result["file_reset"])
                self.assertEqual(result["text"], content.decode("utf-8"))
                self.assertEqual(result["bytes_added"], len(content))

    def test_truncated_log_reads_new_bytes(self):
        for newline in (b"\n", b"\r\n"):
            with self.subTest(newline=newline):
                self.path.write_bytes(b"previous session with a long message" + newline)
                baseline = snapshot_startup_log()
                content = b"new failure" + newline
                self.path.write_bytes(content)
                result = read_startup_log_delta(baseline)
                self.assertTrue(result["file_reset"])
                self.assertEqual(result["text"], content.decode("utf-8"))
                self.assertEqual(result["bytes_added"], len(content))

    def test_large_delta_is_bounded_to_new_bytes(self):
        self.path.write_bytes(b"previous session\n")
        baseline = snapshot_startup_log()
        new = b"x" * (MAX_LOG_BYTES + 10) + b"new failure\n"
        with self.path.open("ab") as stream:
            stream.write(new)
        result = read_startup_log_delta(baseline)
        self.assertEqual(len(result["text"].encode()), MAX_LOG_BYTES)
        self.assertTrue(result["text"].endswith("new failure\n"))
        self.assertEqual(result["bytes_added"], len(new))
        self.assertEqual(result["skipped_bytes"], len(new) - MAX_LOG_BYTES)

    def test_absent_log_and_environment_are_diagnostics(self):
        result = read_startup_log_delta(snapshot_startup_log())
        self.assertEqual(result["status"], "missing")
        with patch.dict(os.environ, {}, clear=True):
            result = read_startup_log_delta(snapshot_startup_log())
        self.assertEqual(result["status"], "unavailable")

    def test_snapshot_error_does_not_read_unknown_previous_bytes(self):
        self.path.write_bytes(b"old failure\n")
        with patch.object(Path, "stat", side_effect=PermissionError("denied")):
            baseline = snapshot_startup_log()
        result = read_startup_log_delta(baseline)
        self.assertEqual(result["status"], "unavailable")
        self.assertNotIn("text", result)

    def test_read_failure_does_not_mask_benchmark_failure(self):
        baseline = snapshot_startup_log()
        with patch.object(Path, "open", side_effect=PermissionError("denied")):
            result = read_startup_log_delta(baseline)
        self.assertEqual(result["status"], "error")
        self.assertIn("denied", result["error"])


if __name__ == "__main__":
    unittest.main()

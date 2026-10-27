from __future__ import annotations

from pathlib import Path
import struct
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

from shutdown_diagnostics import capture_shutdown_dump, process_creation_ticks, write_dump


class ShutdownDiagnosticsTest(unittest.TestCase):
    def test_full_memory_capture_is_explicit_and_bounded(self):
        def helper(cmd, **kwargs):
            self.assertIn("--full-memory", cmd)
            self.assertEqual(kwargs["timeout"], 60)
            Path(cmd[cmd.index("--out") + 1]).write_bytes(b"MDMP")
            return subprocess.CompletedProcess(cmd, 0, "", "")

        with tempfile.TemporaryDirectory() as directory:
            with patch("shutdown_diagnostics.subprocess.run", side_effect=helper):
                result = capture_shutdown_dump({"pid": 123, "creation_ticks": 456},
                                               Path(directory), full_memory=True)
            self.assertEqual(result["status"], "ok")
            self.assertEqual(result["mode"], "full_memory")
            with patch("shutdown_diagnostics.subprocess.run",
                       side_effect=subprocess.TimeoutExpired("dump helper", 60)):
                result = capture_shutdown_dump({"pid": 123, "creation_ticks": 456},
                                               Path(directory), full_memory=True)
            self.assertEqual(result["status"], "error")
            self.assertIn("TimeoutExpired", result["error"])

    def test_capture_timeout_and_helper_failure_are_reported(self):
        identity = {"pid": 123, "creation_ticks": 456}
        for error in (subprocess.TimeoutExpired("dump helper", 15), OSError("cannot spawn")):
            with self.subTest(error=error), tempfile.TemporaryDirectory() as directory:
                with patch("shutdown_diagnostics.subprocess.run", side_effect=error) as run:
                    result = capture_shutdown_dump(identity, Path(directory))
                self.assertEqual(result["status"], "error")
                self.assertIn(type(error).__name__, result["error"])
                self.assertEqual(run.call_args.kwargs["timeout"], 15)
        with tempfile.TemporaryDirectory() as directory:
            failure = subprocess.CompletedProcess([], 1, "", "access denied")
            with patch("shutdown_diagnostics.subprocess.run", return_value=failure):
                result = capture_shutdown_dump(identity, Path(directory))
            self.assertEqual(result["status"], "error")
            self.assertIn("access denied", result["error"])

    def test_missing_identity_never_starts_a_dump(self):
        with tempfile.TemporaryDirectory() as directory:
            with patch("shutdown_diagnostics.subprocess.run") as run:
                result = capture_shutdown_dump({"pid": 123}, Path(directory))
            run.assert_not_called()
            self.assertEqual(result["status"], "error")

    def test_success_requires_nonempty_file_and_uses_unique_directories(self):
        identity = {"pid": 123, "creation_ticks": 456}
        paths = []

        def helper(cmd, **_):
            path = Path(cmd[-1])
            paths.append(path)
            path.write_bytes(b"MDMP")
            return subprocess.CompletedProcess(cmd, 0, "", "")

        with tempfile.TemporaryDirectory() as directory:
            with patch("shutdown_diagnostics.subprocess.run", side_effect=helper):
                for _ in range(2):
                    result = capture_shutdown_dump(identity, Path(directory))
                    self.assertEqual(result["status"], "ok")
                    self.assertEqual(result["bytes"], 4)
            self.assertNotEqual(paths[0], paths[1])
            with patch("shutdown_diagnostics.subprocess.run",
                       return_value=subprocess.CompletedProcess([], 0, "", "")):
                result = capture_shutdown_dump(identity, Path(directory))
            self.assertEqual(result["status"], "error")

    @unittest.skipUnless(sys.platform == "win32", "native Windows dump API")
    def test_native_dump_of_child_contains_thread_and_module_streams(self):
        # Exercise the real handle identity and DbgHelp ABI without Excel or
        # psutil, including in the Windows benchmark-artifact CI job.
        child = subprocess.Popen([sys.executable, "-c",
            "import sys; print('ready', flush=True); sys.stdin.read()"],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
        try:
            self.assertEqual(child.stdout.readline().strip(), "ready")
            ticks = process_creation_ticks(child.pid)
            with tempfile.TemporaryDirectory() as directory:
                path = Path(directory) / "wrong-process.dmp"
                with self.assertRaisesRegex(ValueError, "PID was reused"):
                    write_dump(child.pid, ticks + 1, path)
                self.assertFalse(path.exists())
                for full_memory in (False, True):
                    result = capture_shutdown_dump(
                        {"pid": child.pid, "creation_ticks": ticks}, Path(directory),
                        full_memory=full_memory)
                    self.assertEqual(result["status"], "ok", result)
                    data = Path(result["path"]).read_bytes()
                    self.assertEqual(data[:4], b"MDMP")
                    count, offset = struct.unpack_from("<II", data, 8)
                    streams = {struct.unpack_from("<I", data, offset + 12 * i)[0]
                               for i in range(count)}
                    self.assertTrue({3, 4, 12, 17}.issubset(streams), streams)
                    flags = struct.unpack_from("<Q", data, 24)[0]
                    self.assertEqual(bool(flags & 2), full_memory)
                    if full_memory:
                        self.assertTrue({9, 16}.issubset(streams), streams)
        finally:
            child.stdin.close()
            try:
                child.wait(timeout=5)
            except subprocess.TimeoutExpired:
                child.kill()
                child.wait(timeout=5)
            child.stdout.close()


if __name__ == "__main__":
    unittest.main()

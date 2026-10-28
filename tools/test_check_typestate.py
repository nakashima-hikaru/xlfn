"""Exercise the typestate checker's UTF-8 contract on a legacy-code-page host."""

from contextlib import ExitStack
import io
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

import check_typestate as checker


class Utf8TypestateTests(unittest.TestCase):
    def run_checker(self, *, target=None, wrong_error=False):
        with tempfile.TemporaryDirectory(prefix="xlfn-typestate-test-") as directory:
            root = Path(directory)
            report_path = root / "結果あ.json"
            source_text = "// 元のソースあ\n"
            positive = Path("tools/typestate/pass/正常あ.rs")
            negative = Path("tools/typestate/fail/不正あ.rs")
            windows = Path("tools/typestate/pass/windowsあ.rs")
            sources = {relative: source_text for relative in checker.MODULES.values()}
            sources.update({
                positive: "// module: registration/host\n// 正常あ\nfn valid() {}\n",
                negative: "// module: registration/host\n// 不正あ\nfn invalid() {} // error: E0308\n",
                windows: "// module: excel_rtd/windows\n// Windowsあ\nfn valid() {}\n",
            })
            for relative, content in sources.items():
                path = root / relative
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(content.encode("utf-8"))

            original_open = Path.open
            original_run = subprocess.run
            compiled = []

            def legacy_open(path, mode="r", buffering=-1, encoding=None, errors=None, newline=None):
                # Path.read_text can pass "locale" instead of None on newer
                # Python versions. Both mean the simulated Windows default.
                if "b" not in mode and encoding in (None, "locale"):
                    encoding = "cp1252"
                # Exercise Windows text-write translation on every host, too.
                if "b" not in mode and newline is None and any(flag in mode for flag in "wax"):
                    newline = "\r\n"
                return original_open(path, mode, buffering, encoding, errors, newline)

            def command_output(command, **kwargs):
                if command == ["rustc", "-Vv"]:
                    content = "rustc 1.99.1\nhost: x86_64-unknown-linux-gnu\nnote: 診断あ\n".encode("utf-8")
                elif command == ["git", "rev-parse", "HEAD"]:
                    content = b"0123456789abcdef\n"
                else:
                    self.assertEqual(command, ["git", "ls-files", "-z", "--cached", "--others", "--exclude-standard"])
                    content = ("\0".join(map(str, sources)) + "\0").encode("utf-8")
                if kwargs.get("text"):
                    return content.decode(kwargs.get("encoding") or "cp1252")
                return content

            def cargo_run(command, **kwargs):
                self.assertEqual(command[0:5], ["cargo", "check", "-p", "xlfn", "--lib"])
                self.assertIn("--message-format=json", command)
                if target:
                    self.assertEqual(command[-2:], ["--target", target])
                else:
                    self.assertNotIn("--target", command)
                overlay = Path(kwargs["cwd"])
                injections = []
                for relative in checker.MODULES.values():
                    text = (overlay / relative).read_bytes().decode("utf-8")
                    self.assertEqual(text.splitlines()[:1], source_text.splitlines())
                    injections.extend(line for line in text.splitlines() if line.startswith("#[path = "))
                # Restoring each original must leave exactly one current fixture.
                self.assertEqual(len(injections), 1)
                literal = injections[0][len("#[path = "):-1]
                self.assertIn("あ", literal, "Rust path literals must retain Unicode instead of JSON escapes")
                fixture = Path(json.loads(literal)).relative_to(overlay)
                compiled.append(fixture)
                events = []
                if fixture == negative:
                    events.append({
                        "reason": "compiler-message",
                        "message": {
                            "level": "error", "message": "型が違いますあ",
                            "rendered": "error: 型が違いますあ\n",
                            "code": {"code": "E0425" if wrong_error else "E0308"},
                            "spans": [{"is_primary": True, "file_name": str(overlay / fixture), "line_start": 3}],
                        },
                    })
                stdout = ("\n".join(json.dumps(event, ensure_ascii=False) for event in events) + "\n").encode("utf-8")
                stderr = "コンパイラの診断あ\n".encode("utf-8")
                # Use actual byte pipes so missing subprocess encoding regresses
                # even when these tests run on a UTF-8 Unix machine.
                kwargs.setdefault("encoding", "cp1252")
                script = (
                    "import sys; "
                    f"sys.stdout.buffer.write({stdout!r}); "
                    f"sys.stderr.buffer.write({stderr!r}); "
                    f"sys.exit({int(bool(events))})"
                )
                return original_run([sys.executable, "-c", script], **kwargs)

            stdout_bytes, stderr_bytes = io.BytesIO(), io.BytesIO()
            stdout = io.TextIOWrapper(stdout_bytes, encoding="cp1252")
            stderr = io.TextIOWrapper(stderr_bytes, encoding="cp1252")
            argv = ["check_typestate.py", "--target-dir", str(root / "target"), "--report", str(report_path)]
            if target:
                argv.extend(["--target", target])
            with ExitStack() as stack:
                stack.enter_context(patch.object(
                    io, "text_encoding",
                    lambda encoding, stacklevel=2: "cp1252" if encoding is None else encoding,
                ))
                stack.enter_context(patch.object(Path, "open", legacy_open))
                # Prove the simulated default rejects the UTF-8 fixture bytes.
                with self.assertRaises(UnicodeDecodeError):
                    (root / positive).read_text()
                stack.enter_context(patch.object(checker, "ROOT", root))
                stack.enter_context(patch.object(checker.subprocess, "check_output", command_output))
                stack.enter_context(patch.object(checker.subprocess, "run", cargo_run))
                stack.enter_context(patch.object(sys, "argv", argv))
                stack.enter_context(patch.object(sys, "stdout", stdout))
                stack.enter_context(patch.object(sys, "stderr", stderr))
                status = checker.main()
                stdout.flush()
                stderr.flush()
            output = stdout_bytes.getvalue().decode("utf-8")
            error_output = stderr_bytes.getvalue().decode("utf-8")
            raw_report = report_path.read_bytes()
            self.assertIn("型が違いますあ".encode("utf-8"), raw_report)
            report = json.loads(raw_report.decode("utf-8"))
            self.assertIn("診断あ", report["rustc"])
            return status, report, compiled, output, error_output

    def test_utf8_sources_fixtures_and_diagnostics_with_cp1252_defaults(self):
        status, report, compiled, output, _ = self.run_checker()
        self.assertEqual(status, 0)
        self.assertTrue(report["passed"])
        self.assertEqual(len(compiled), 2)
        self.assertEqual(len(report["skipped_fixtures"]), 1)
        negative = next(case for case in report["fixtures"] if case["expected"])
        self.assertEqual(negative["expected"], [[3, "E0308"]])
        self.assertEqual(negative["observed"], [[3, "E0308"]])
        self.assertIn("正常あ.rs", output)
        self.assertIn("不正あ.rs", output)

    def test_windows_target_retains_its_fixtures_with_cp1252_defaults(self):
        status, report, compiled, _, _ = self.run_checker(target="x86_64-pc-windows-msvc")
        self.assertEqual(status, 0)
        self.assertTrue(report["passed"])
        self.assertEqual(len(compiled), 3)
        self.assertEqual(report["skipped_fixtures"], [])

    def test_non_ascii_failure_keeps_exact_error_code_and_utf8_report(self):
        status, report, _, output, error_output = self.run_checker(wrong_error=True)
        self.assertEqual(status, 1)
        self.assertFalse(report["passed"])
        negative = next(case for case in report["fixtures"] if case["expected"])
        self.assertEqual(negative["observed"], [])
        self.assertEqual(negative["unexpected_errors"], ["error: 型が違いますあ\n"])
        self.assertEqual(negative["stderr"], "コンパイラの診断あ\n")
        self.assertIn("FAIL", output)
        self.assertIn("型が違いますあ", error_output)
        self.assertIn("コンパイラの診断あ", error_output)


if __name__ == "__main__":
    unittest.main()

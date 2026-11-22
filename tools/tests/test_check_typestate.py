"""Exercise the typestate checker's UTF-8 contract on a legacy-code-page host."""

import io
import json
import subprocess
import sys
import tempfile
import unittest
from contextlib import ExitStack
from pathlib import Path
from threading import Barrier, Lock
from unittest.mock import patch

from xlfn_tools import check_typestate as checker


class PackageRoutingTests(unittest.TestCase):
    def test_kernel_and_facade_compile_the_owning_package(self):
        for module, package, features in (
            ("kernel/service_slot", "xlfn-kernel", ()),
            ("kernel/service_slot/transaction", "xlfn-kernel", ()),
            ("registration/host", "xlfn", (*checker.FEATURES, "blake3/pure")),
            ("async_udf/builtin", "xlfn", (*checker.FEATURES, "blake3/pure", "async-builtin")),
            (
                "subscription/data_plane/refresh/windows",
                "xlfn",
                (*checker.FEATURES, "blake3/pure"),
            ),
        ):
            with self.subTest(module=module), tempfile.TemporaryDirectory() as directory:
                overlay = Path(directory)
                relative = checker.MODULES[module]
                source = overlay / relative
                source.parent.mkdir(parents=True)
                source.write_text("// original\n", encoding="utf-8")
                fixture = checker.Fixture(Path("tools/typestate/pass/control.rs"), module, ())
                events = [
                    {"reason": "compiler-artifact", "fresh": fresh, "manifest_path": str(path)}
                    for fresh, path in (
                        (False, overlay / "Cargo.toml"),
                        (True, overlay / "Cargo.toml"),
                        (False, overlay.parent / "external" / "Cargo.toml"),
                    )
                ]
                stdout = "\n".join(json.dumps(event) for event in events)
                completed = subprocess.CompletedProcess([], 0, stdout, "")

                def compile_fixture(*_args, **_kwargs):
                    self.assertIn(
                        "mod typestate_compile_fixture;", source.read_text(encoding="utf-8")
                    )
                    return completed

                with patch.object(checker.subprocess, "run", side_effect=compile_fixture) as run:
                    result = checker.check_fixture(
                        fixture,
                        overlay,
                        overlay / "target",
                        "x86_64-pc-windows-msvc",
                        ("blake3/pure",),
                    )
                command = run.call_args.args[0]
                self.assertEqual(command[:5], ["cargo", "check", "-p", package, "--lib"])
                self.assertEqual(command[-2:], ["--target", "x86_64-pc-windows-msvc"])
                if features:
                    self.assertEqual(command[command.index("--features") + 1], ",".join(features))
                else:
                    self.assertNotIn("--features", command)
                self.assertTrue(result["passed"])
                self.assertEqual(result["package"], package)
                self.assertEqual(result["workspace_artifacts_rebuilt"], 1)
                self.assertEqual(result["features"], list(features))
                self.assertEqual(source.read_bytes(), b"// original\n")


class OverlayCleanupTests(unittest.TestCase):
    def test_only_injected_source_is_written_and_its_bytes_survive_cargo_failure(self):
        with tempfile.TemporaryDirectory() as directory:
            overlay = Path(directory)
            first = "registration/host"
            second = "kernel/service_slot"
            source = overlay / checker.MODULES[first]
            untouched = overlay / checker.MODULES[second]
            original = "// 元のソース\r\n".encode()
            for path in (source, untouched):
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(original)
            fixture = checker.Fixture(Path("tools/typestate/pass/control.rs"), first, ())
            before = untouched.stat().st_mtime_ns
            writes = []
            write_bytes = Path.write_bytes

            def record_write(path, data):
                writes.append(path)
                return write_bytes(path, data)

            with (
                patch.object(Path, "write_bytes", record_write),
                patch.object(checker.subprocess, "run", side_effect=OSError("no cargo")),
                self.assertRaisesRegex(OSError, "no cargo"),
            ):
                checker.check_fixture(fixture, overlay, overlay / "target")
            self.assertEqual(writes, [source])
            self.assertEqual(source.read_bytes(), original)
            self.assertEqual(untouched.read_bytes(), original)
            self.assertEqual(untouched.stat().st_mtime_ns, before)


class BatchAndWorkerTests(unittest.TestCase):
    def setUp(self):
        self.overlay = Path(self.enterContext(tempfile.TemporaryDirectory()))
        self.target = self.overlay.parent / f"{self.overlay.name}-target"
        self.facade = "runtime"
        self.builtin = "async_udf/builtin"
        self.source = self.overlay / checker.MODULES[self.facade]
        self.original = b"// untouched\r\n"
        for module in (self.facade, self.builtin):
            source = self.overlay / checker.MODULES[module]
            source.parent.mkdir(parents=True, exist_ok=True)
            source.write_bytes(self.original)

    def fixture(self, name, *, module=None, negative=False):
        kind = "fail" if negative else "pass"
        return checker.Fixture(
            Path(f"tools/typestate/{kind}/{name}.rs"),
            module or self.facade,
            ((3, "E0308"),) if negative else (),
        )

    def run_cases(self, *cases, jobs=1):
        return checker.check_cases(list(cases), self.overlay, self.target, None, (), jobs)

    def test_failed_batch_only_retries_members_and_still_blocks_negatives(self):
        one, two = self.fixture("a"), self.fixture("c")
        other = self.fixture("b", module=self.builtin)
        negative = self.fixture("d", negative=True)
        payloads = []

        def cargo(_command, **_kwargs):
            payload = self.source.read_bytes()
            payloads.append(payload)
            if b"a.rs" in payload and b"c.rs" in payload:
                self.assertIn(b"mod typestate_compile_fixture_0;", payload)
                self.assertIn(b"mod typestate_compile_fixture_1;", payload)
                return subprocess.CompletedProcess([], 1, "", "failed batch")
            return subprocess.CompletedProcess([], 0, "", "")

        with patch.object(checker.subprocess, "run", side_effect=cargo):
            results, attempts = self.run_cases(one, other, two, negative, jobs=2)
        self.assertEqual(
            [result["fixture"] for result in results],
            [
                str(one.path),
                str(other.path),
                str(two.path),
            ],
        )
        self.assertTrue(all(result["passed"] for result in results))
        self.assertFalse(attempts[0]["passed"])
        self.assertEqual(attempts[0]["stderr"], "failed batch")
        self.assertEqual(
            [attempt["kind"] for attempt in attempts],
            [
                "positive-batch",
                "positive-single",
                "positive-single",
                "positive-single",
            ],
        )
        self.assertEqual(
            [attempt["fixtures"] for attempt in attempts],
            [
                [str(one.path), str(two.path)],
                [str(one.path)],
                [str(two.path)],
                [str(other.path)],
            ],
        )
        self.assertNotIn(b"c.rs", payloads[1])
        self.assertNotIn(b"a.rs", payloads[2])
        self.assertEqual(payloads[3], self.original)
        self.assertEqual(self.source.read_bytes(), self.original)

    def test_two_negative_workers_compile_concurrently_with_isolated_files_and_targets(self):
        positive = self.fixture("control")
        negatives = [self.fixture(f"case_{index}", negative=True) for index in range(4)]
        barrier = Barrier(2, timeout=10)
        lock = Lock()
        seen: dict[Path, set[Path]] = {}

        def cargo(command, *, cwd, **_kwargs):
            overlay = Path(cwd)
            target = Path(command[command.index("--target-dir") + 1])
            source = overlay / checker.MODULES[self.facade]
            contents = source.read_bytes().decode()
            if "control.rs" in contents:
                return subprocess.CompletedProcess([], 0, "", "")
            found = [fixture for fixture in negatives if fixture.path.name in contents]
            self.assertEqual(len(found), 1)
            fixture = found[0]
            with lock:
                seen.setdefault(overlay, set()).add(target)
            barrier.wait()
            self.assertEqual(source.read_bytes().decode(), contents)
            event = {
                "reason": "compiler-message",
                "message": {
                    "level": "error",
                    "message": "expected compiler error",
                    "code": {"code": "E0308"},
                    "spans": [
                        {
                            "is_primary": True,
                            "file_name": str(overlay / fixture.path),
                            "line_start": 3,
                        }
                    ],
                },
            }
            return subprocess.CompletedProcess([], 1, json.dumps(event), "")

        with patch.object(checker.subprocess, "run", side_effect=cargo):
            results, attempts = self.run_cases(positive, *negatives, jobs=2)
        self.assertEqual(len(seen), 2)
        self.assertEqual(
            {target for targets in seen.values() for target in targets},
            {
                self.target,
                self.target / "worker-2",
            },
        )
        self.assertEqual({len(targets) for targets in seen.values()}, {1})
        self.assertTrue(all(result["passed"] for result in results))
        self.assertEqual(
            [result["fixture"] for result in results],
            [str(fixture.path) for fixture in (positive, *negatives)],
        )
        self.assertEqual(
            [attempt["kind"] for attempt in attempts], ["positive-single"] + ["negative"] * 4
        )
        self.assertEqual(self.source.read_bytes(), self.original)


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
            sources.update(
                {
                    positive: "// module: registration/host\n// 正常あ\nfn valid() {}\n",
                    negative: "// module: registration/host\n// 不正あ\nfn invalid() {} // error: E0308\n",
                    windows: "// module: excel_rtd/windows\n// Windowsあ\nfn valid() {}\n",
                }
            )
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
                    content = (
                        "rustc 1.99.1\nhost: x86_64-unknown-linux-gnu\nnote: 診断あ\n".encode()
                    )
                elif command == ["git", "rev-parse", "HEAD"]:
                    content = b"0123456789abcdef\n"
                else:
                    self.assertEqual(
                        command,
                        ["git", "ls-files", "-z", "--cached", "--others", "--exclude-standard"],
                    )
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
                for relative in set(checker.MODULES.values()):
                    text = (overlay / relative).read_bytes().decode("utf-8")
                    self.assertEqual(text.splitlines()[:1], source_text.splitlines())
                    injections.extend(
                        line for line in text.splitlines() if line.startswith("#[path = ")
                    )
                batch = []
                for injection in injections:
                    literal = injection[len("#[path = ") : -1]
                    self.assertIn(
                        "あ",
                        literal,
                        "Rust path literals must retain Unicode instead of JSON escapes",
                    )
                    batch.append(Path(json.loads(literal)).relative_to(overlay))
                self.assertTrue(batch)
                compiled.append(batch)
                events = []
                if negative in batch:
                    self.assertEqual(batch, [negative])
                    events.append(
                        {
                            "reason": "compiler-message",
                            "message": {
                                "level": "error",
                                "message": "型が違いますあ",
                                "rendered": "error: 型が違いますあ\n",
                                "code": {"code": "E0425" if wrong_error else "E0308"},
                                "spans": [
                                    {
                                        "is_primary": True,
                                        "file_name": str(overlay / negative),
                                        "line_start": 3,
                                    }
                                ],
                            },
                        }
                    )
                stdout = (
                    "\n".join(json.dumps(event, ensure_ascii=False) for event in events) + "\n"
                ).encode("utf-8")
                stderr = "コンパイラの診断あ\n".encode()
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
            argv = [
                "check_typestate.py",
                "--target-dir",
                str(root / "target"),
                "--report",
                str(report_path),
            ]
            if target:
                argv.extend(["--target", target])
            with ExitStack() as stack:
                stack.enter_context(
                    patch.object(
                        io,
                        "text_encoding",
                        lambda encoding, stacklevel=2: "cp1252" if encoding is None else encoding,
                    )
                )
                stack.enter_context(patch.object(Path, "open", legacy_open))
                # Prove the simulated default rejects the UTF-8 fixture bytes.
                with self.assertRaises(UnicodeDecodeError):
                    (root / positive).read_text()
                stack.enter_context(patch.object(checker, "ROOT", root))
                stack.enter_context(
                    patch.object(checker.subprocess, "check_output", command_output)
                )
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
            self.assertIn("型が違いますあ".encode(), raw_report)
            report = json.loads(raw_report.decode("utf-8"))
            self.assertIn("診断あ", report["rustc"])
            return status, report, compiled, output, error_output

    def test_utf8_sources_fixtures_and_diagnostics_with_cp1252_defaults(self):
        status, report, compiled, output, _ = self.run_checker()
        self.assertEqual(status, 0)
        self.assertTrue(report["passed"])
        self.assertEqual(len(compiled), 2)
        for count in ("cargo_invocations", "fixture_injections", "fixture_restores"):
            self.assertEqual(report["runner_cost"][count], 2)
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
        self.assertEqual(list(map(len, compiled)), [2, 1])
        self.assertEqual(report["runner_cost"]["cargo_invocations"], 2)
        self.assertEqual(report["runner_cost"]["fixture_injections"], 3)
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

#!/usr/bin/env python3
"""Compile actual private lifecycle APIs with valid and invalid internal callers.

Fixtures are injected into a temporary copy of the current working tree. This
keeps production internals private and ensures a privacy error on an ancestor
module cannot accidentally stand in for the intended typestate diagnostic.
Each negative line declares the exact Rust error it must produce.
"""

from __future__ import annotations

import argparse
from dataclasses import dataclass
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile


ROOT = Path(__file__).resolve().parents[1]
MODULES = {
    "async_udf": Path("crates/xlfn/src/async_udf.rs"),
    "async_udf/runtime": Path("crates/xlfn/src/async_udf/runtime.rs"),
    "subscription": Path("crates/xlfn/src/subscription.rs"),
    "subscription/channel": Path("crates/xlfn/src/subscription/channel.rs"),
    "subscription/runtime": Path("crates/xlfn/src/subscription/runtime.rs"),
    "handle": Path("crates/xlfn/src/handle.rs"),
    "registration/host": Path("crates/xlfn/src/registration/host.rs"),
    "value": Path("crates/xlfn/src/value.rs"),
    "cache": Path("crates/xlfn/src/cache.rs"),
    "runtime": Path("crates/xlfn/src/runtime.rs"),
    "lifecycle/state": Path("crates/xlfn/src/lifecycle/state.rs"),
    "runtime/open_txn": Path("crates/xlfn/src/runtime/open_txn.rs"),
    "runtime/shutdown/pipeline": Path("crates/xlfn/src/runtime/shutdown/pipeline.rs"),
    "excel_rtd/windows": Path("crates/xlfn/src/excel_rtd/windows.rs"),
}
WINDOWS_MODULES = {"excel_rtd/windows"}
FEATURES = ("async", "rtd", "handles", "cache")
EXPECTED = re.compile(r"// error: (E\d{4})\s*$")


@dataclass(frozen=True)
class Fixture:
    path: Path
    module: str
    errors: tuple[tuple[int, str], ...]


def fixtures(root: Path) -> list[Fixture]:
    result = []
    for path in sorted((root / "tools/typestate").glob("*/*.rs")):
        lines = path.read_text(encoding="utf-8").splitlines()
        if not lines or not lines[0].startswith("// module: "):
            raise ValueError(f"{path}: missing module header")
        module = lines[0].removeprefix("// module: ")
        if module not in MODULES:
            raise ValueError(f"{path}: unknown injection module {module}")
        errors = tuple(
            (number, match.group(1))
            for number, line in enumerate(lines, 1)
            if (match := EXPECTED.search(line))
        )
        if (path.parent.name == "fail") != bool(errors):
            raise ValueError(f"{path}: invalid pass/fail error markers")
        result.append(Fixture(path.relative_to(root), module, errors))
    if not result or not any(not fixture.errors for fixture in result):
        raise ValueError("at least one positive fixture is required")
    # Controls must succeed before any expected compilation failure is accepted.
    return sorted(result, key=lambda fixture: (bool(fixture.errors), str(fixture.path)))


def copy_sources(root: Path, destination: Path) -> str:
    paths = subprocess.check_output(
        ["git", "ls-files", "-z", "--cached", "--others", "--exclude-standard"],
        cwd=root,
    ).decode("utf-8").split("\0")
    digest = hashlib.sha256()
    for name in sorted(set(paths) - {""}):
        source = root / name
        if not source.is_file():
            continue
        data = source.read_bytes()
        digest.update(name.encode("utf-8") + b"\0" + data + b"\0")
        target = destination / name
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(source, target)
    return digest.hexdigest()


def error_spans(message: dict) -> list[dict]:
    return [span for span in message.get("spans", []) if span.get("is_primary")]


def check_fixture(
    fixture: Fixture, overlay: Path, originals: dict[Path, str], target: Path,
    compile_target: str | None = None, extra_features: tuple[str, ...] = (),
) -> dict:
    for relative, source in originals.items():
        (overlay / relative).write_text(source, encoding="utf-8")
    injected = overlay / MODULES[fixture.module]
    # Rust accepts literal Unicode; JSON's ASCII-only \uXXXX escapes are not
    # Rust string escapes and would reject non-ASCII checkout/fixture paths.
    path_literal = json.dumps(str(overlay / fixture.path), ensure_ascii=False)
    with injected.open("a", encoding="utf-8") as source:
        source.write(
            f"\n#[allow(dead_code)]\n#[path = {path_literal}]\n"
            "mod typestate_compile_fixture;\n"
        )
    command = [
        "cargo", "check", "-p", "xlfn", "--lib", "--locked", "--offline",
        "--no-default-features", "--features", ",".join((*FEATURES, *extra_features)),
        "--target-dir", str(target), "--message-format=json",
    ]
    if compile_target:
        command.extend(["--target", compile_target])
    completed = subprocess.run(
        command, cwd=overlay, text=True, encoding="utf-8", capture_output=True,
        env={**os.environ, "CARGO_TERM_COLOR": "never"},
    )
    errors = []
    for line in completed.stdout.splitlines():
        try:
            event = json.loads(line)
        except json.JSONDecodeError:
            continue
        if event.get("reason") == "compiler-message":
            message = event["message"]
            if message.get("level") == "error":
                errors.append(message)
    expected = set(fixture.errors)
    observed = set()
    unexpected = []
    for message in errors:
        code = (message.get("code") or {}).get("code")
        matching = {
            (span["line_start"], code)
            for span in error_spans(message)
            if Path(span["file_name"]).name == fixture.path.name
            and (span["line_start"], code) in expected
        }
        observed.update(matching)
        if not matching:
            unexpected.append(message.get("rendered", message["message"]))
    passed = (
        completed.returncode == 0 and not errors
        if not expected
        else completed.returncode != 0 and observed == expected and not unexpected
    )
    return {
        "fixture": str(fixture.path), "target": compile_target or "host", "passed": passed,
        "command": command,
        "exit_code": completed.returncode,
        "expected": sorted(expected), "observed": sorted(observed),
        "unexpected_errors": unexpected,
        "diagnostics": [error.get("rendered", error["message"]) for error in errors],
        "stderr": completed.stderr if not passed else "",
    }


def main() -> int:
    # Windows CI may inherit a legacy console code page even though repository
    # files and Cargo's JSON diagnostics are UTF-8. Preserve their text in logs.
    for stream in (sys.stdout, sys.stderr):
        if hasattr(stream, "reconfigure"):
            stream.reconfigure(encoding="utf-8")
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target-dir", type=Path, default=ROOT / "target/typestate-check")
    parser.add_argument("--report", type=Path)
    parser.add_argument("--target", help="Cargo compile target; defaults to the rustc host")
    parser.add_argument("--extra-features", default="", help="Additional comma-separated Cargo features")
    args = parser.parse_args()
    cases = fixtures(ROOT)
    target = args.target_dir.resolve()
    rustc = subprocess.check_output(["rustc", "-Vv"], text=True, encoding="utf-8").strip()
    host = next(line.removeprefix("host: ") for line in rustc.splitlines() if line.startswith("host: "))
    compile_target = args.target or host
    skipped = [
        fixture for fixture in cases
        if fixture.module in WINDOWS_MODULES and "-windows-" not in compile_target
    ]
    cases = [fixture for fixture in cases if fixture not in skipped]
    extra_features = tuple(filter(None, args.extra_features.split(",")))
    environment_keys = (
        "SDKROOT", "CC", "CFLAGS", "AR",
        *(f"{prefix}_{compile_target.replace('-', '_')}" for prefix in ("CC", "CFLAGS", "AR")),
    )
    report = {
        "rustc": rustc, "target": compile_target,
        "git_head": subprocess.check_output(
            ["git", "rev-parse", "HEAD"], cwd=ROOT, text=True, encoding="utf-8",
        ).strip(),
        "features": [*FEATURES, *extra_features], "fixtures": [],
        "build_environment": {key: os.environ[key] for key in environment_keys if key in os.environ},
        "skipped_fixtures": [
            {"fixture": str(fixture.path), "reason": "requires a Windows target"}
            for fixture in skipped
        ],
    }
    if skipped:
        print(f"SKIP {len(skipped)} Windows fixtures on {compile_target}; run on Windows or use --target", flush=True)
    with tempfile.TemporaryDirectory(prefix="xlfn-typestate-") as directory:
        overlay = Path(directory)
        report["source_sha256"] = copy_sources(ROOT, overlay)
        originals = {
            path: (overlay / path).read_text(encoding="utf-8") for path in MODULES.values()
        }
        for fixture in cases:
            result = check_fixture(fixture, overlay, originals, target, args.target, extra_features)
            report["fixtures"].append(result)
            print(f"{'PASS' if result['passed'] else 'FAIL'} {fixture.path}", flush=True)
            if not result["passed"]:
                print("\n".join(result["diagnostics"]), file=sys.stderr)
                print(result["stderr"], file=sys.stderr)
                # A broken positive control invalidates all negative evidence.
                if not fixture.errors:
                    break
    report["passed"] = len(report["fixtures"]) == len(cases) and all(
        result["passed"] for result in report["fixtures"]
    )
    if args.report:
        args.report.parent.mkdir(parents=True, exist_ok=True)
        args.report.write_text(json.dumps(report, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    return 0 if report["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())

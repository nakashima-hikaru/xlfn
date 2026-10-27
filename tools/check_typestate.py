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
    "subscription": Path("crates/xlfn/src/subscription.rs"),
    "subscription/runtime": Path("crates/xlfn/src/subscription/runtime.rs"),
    "handle": Path("crates/xlfn/src/handle.rs"),
    "value": Path("crates/xlfn/src/value.rs"),
    "cache": Path("crates/xlfn/src/cache.rs"),
    "runtime": Path("crates/xlfn/src/runtime.rs"),
    "runtime/shutdown/pipeline": Path("crates/xlfn/src/runtime/shutdown/pipeline.rs"),
}
EXPECTED = re.compile(r"// error: (E\d{4})\s*$")


@dataclass(frozen=True)
class Fixture:
    path: Path
    module: str
    errors: tuple[tuple[int, str], ...]


def fixtures(root: Path) -> list[Fixture]:
    result = []
    for path in sorted((root / "tools/typestate").glob("*/*.rs")):
        lines = path.read_text().splitlines()
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
    ).decode().split("\0")
    digest = hashlib.sha256()
    for name in sorted(set(paths) - {""}):
        source = root / name
        if not source.is_file():
            continue
        data = source.read_bytes()
        digest.update(name.encode() + b"\0" + data + b"\0")
        target = destination / name
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(source, target)
    return digest.hexdigest()


def error_spans(message: dict) -> list[dict]:
    return [span for span in message.get("spans", []) if span.get("is_primary")]


def check_fixture(
    fixture: Fixture, overlay: Path, originals: dict[Path, str], target: Path
) -> dict:
    for relative, source in originals.items():
        (overlay / relative).write_text(source)
    injected = overlay / MODULES[fixture.module]
    path_literal = json.dumps(str(overlay / fixture.path))
    with injected.open("a") as source:
        source.write(
            f"\n#[allow(dead_code)]\n#[path = {path_literal}]\n"
            "mod typestate_compile_fixture;\n"
        )
    command = [
        "cargo", "check", "-p", "xlfn", "--lib", "--locked", "--offline",
        "--no-default-features", "--features", "async,rtd,handles,cache",
        "--target-dir", str(target), "--message-format=json",
    ]
    completed = subprocess.run(
        command, cwd=overlay, text=True, capture_output=True,
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
        "fixture": str(fixture.path), "passed": passed,
        "exit_code": completed.returncode,
        "expected": sorted(expected), "observed": sorted(observed),
        "unexpected_errors": unexpected,
        "diagnostics": [error.get("rendered", error["message"]) for error in errors],
        "stderr": completed.stderr if not passed else "",
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target-dir", type=Path, default=ROOT / "target/typestate-check")
    parser.add_argument("--report", type=Path)
    args = parser.parse_args()
    cases = fixtures(ROOT)
    target = args.target_dir.resolve()
    report = {
        "rustc": subprocess.check_output(["rustc", "-Vv"], text=True).strip(),
        "git_head": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip(),
        "features": ["async", "rtd", "handles", "cache"], "fixtures": [],
    }
    with tempfile.TemporaryDirectory(prefix="xlfn-typestate-") as directory:
        overlay = Path(directory)
        report["source_sha256"] = copy_sources(ROOT, overlay)
        originals = {
            path: (overlay / path).read_text() for path in MODULES.values()
        }
        for fixture in cases:
            result = check_fixture(fixture, overlay, originals, target)
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
        args.report.write_text(json.dumps(report, indent=2) + "\n")
    return 0 if report["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())

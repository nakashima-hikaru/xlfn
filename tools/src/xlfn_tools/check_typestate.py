#!/usr/bin/env python3
"""Compile actual private lifecycle APIs with valid and invalid internal callers.

Fixtures are injected into a temporary copy of the current working tree. This
keeps production internals private and ensures a privacy error on an ancestor
module cannot accidentally stand in for the intended typestate diagnostic.
Each negative line declares the exact Rust error it must produce.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time
from concurrent.futures import ThreadPoolExecutor
from contextlib import ExitStack
from dataclasses import dataclass
from pathlib import Path

from ._repository import ROOT

MODULES = {
    "boundary/host": Path("crates/xlfn/src/boundary/host.rs"),
    "kernel/lib": Path("crates/xlfn-kernel/src/lib.rs"),
    "kernel/drain_gate": Path("crates/xlfn-kernel/src/drain_gate.rs"),
    "kernel/operation_gate": Path("crates/xlfn-kernel/src/operation_gate.rs"),
    "kernel/rotating_read_domain": Path("crates/xlfn-kernel/src/rotating_read_domain.rs"),
    "kernel/rotating_read_domain/transition": Path(
        "crates/xlfn-kernel/src/rotating_read_domain/transition.rs"
    ),
    "kernel/thread_affine": Path("crates/xlfn-kernel/src/thread_affine.rs"),
    "kernel/quota": Path("crates/xlfn-kernel/src/quota.rs"),
    "kernel/service_slot": Path("crates/xlfn-kernel/src/service_slot.rs"),
    "kernel/service_slot/transaction": Path("crates/xlfn-kernel/src/service_slot/transaction.rs"),
    "async_udf": Path("crates/xlfn/src/async_udf.rs"),
    "async_udf/boundary": Path("crates/xlfn/src/async_udf/boundary.rs"),
    "async_udf/builtin": Path("crates/xlfn/src/async_udf/builtin.rs"),
    "async_udf/completion": Path("crates/xlfn/src/async_udf/completion.rs"),
    "async_udf/instrumentation": Path("crates/xlfn/src/async_udf/instrumentation.rs"),
    "cancellation": Path("crates/xlfn/src/cancellation.rs"),
    "host/lib": Path("crates/xlfn-host/src/lib.rs"),
    "host/drain/windows": Path("crates/xlfn-host/src/drain/windows.rs"),
    "excel_rtd/windows/server": Path("crates/xlfn/src/excel_rtd/windows/server.rs"),
    "excel_rtd/windows/server/termination": Path(
        "crates/xlfn/src/excel_rtd/windows/server/termination.rs"
    ),
    "excel_rtd/windows/server_gate": Path("crates/xlfn/src/excel_rtd/windows/server_gate.rs"),
    "async_udf/runtime": Path("crates/xlfn/src/async_udf/runtime.rs"),
    "subscription": Path("crates/xlfn/src/subscription.rs"),
    "subscription/channel": Path("crates/xlfn/src/subscription/channel.rs"),
    "subscription/disconnect": Path("crates/xlfn/src/subscription/disconnect.rs"),
    "subscription/multiplex": Path("crates/xlfn/src/subscription/multiplex.rs"),
    "subscription/source": Path("crates/xlfn/src/subscription/source.rs"),
    "subscription/runtime": Path("crates/xlfn/src/subscription/runtime.rs"),
    "runtime/residency": Path("crates/xlfn/src/runtime/residency.rs"),
    "return_abi/numeric": Path("crates/xlfn/src/return_abi/numeric.rs"),
    "host/process_quota/windows": Path("crates/xlfn-host/src/process_quota/windows.rs"),
    "return_abi": Path("crates/xlfn/src/return_abi.rs"),
    "return_abi/ownership": Path("crates/xlfn/src/return_abi/ownership.rs"),
    "macro_abi": Path("crates/xlfn/src/__private.rs"),
    "macros/lib": Path("crates/xlfn-macros/src/lib.rs"),
    "subscription/data_plane": Path("crates/xlfn/src/subscription/data_plane.rs"),
    "subscription/data_plane/refresh": Path("crates/xlfn/src/subscription/data_plane/refresh.rs"),
    # This scope exposes Windows-only child capabilities to a sibling caller
    # without granting that caller access to the child's private fields.
    "subscription/data_plane/refresh/windows": Path(
        "crates/xlfn/src/subscription/data_plane/refresh.rs"
    ),
    "subscription/data_plane/refresh/publication": Path(
        "crates/xlfn/src/subscription/data_plane/refresh/publication.rs"
    ),
    "diagnostics": Path("crates/xlfn/src/diagnostics.rs"),
    "diagnostics/worker": Path("crates/xlfn/src/diagnostics/worker.rs"),
    "diagnostics/worker/lifetime": Path("crates/xlfn/src/diagnostics/worker/lifetime.rs"),
    "handle/connection": Path("crates/xlfn/src/handle/connection.rs"),
    "host_callback": Path("crates/xlfn/src/host_callback.rs"),
    "callback_value": Path("crates/xlfn/src/callback_value.rs"),
    "registration": Path("crates/xlfn/src/registration.rs"),
    "runtime_components": Path("crates/xlfn/src/runtime_components.rs"),
    "runtime_components/host_ledger": Path("crates/xlfn/src/runtime_components/host_ledger.rs"),
    "registration/ledger": Path("crates/xlfn/src/registration/ledger.rs"),
    "handle": Path("crates/xlfn/src/handle.rs"),
    "handle/registry": Path("crates/xlfn/src/handle/registry.rs"),
    "handle/topic": Path("crates/xlfn/src/handle/topic.rs"),
    "handle/runtime": Path("crates/xlfn/src/handle/runtime.rs"),
    "handle/domain": Path("crates/xlfn/src/handle/domain.rs"),
    "registration/host": Path("crates/xlfn/src/registration/host.rs"),
    "value": Path("crates/xlfn/src/value.rs"),
    "cache": Path("crates/xlfn-services/src/cache.rs"),
    "runtime": Path("crates/xlfn/src/runtime.rs"),
    "lifecycle/state": Path("crates/xlfn/src/lifecycle/state.rs"),
    "runtime/open_txn": Path("crates/xlfn/src/runtime/open_txn.rs"),
    "runtime/shutdown/pipeline": Path("crates/xlfn/src/runtime/shutdown/pipeline.rs"),
    "runtime/shutdown/finalization": Path("crates/xlfn/src/runtime/shutdown/finalization.rs"),
    "runtime/transactions": Path("crates/xlfn/src/runtime/transactions.rs"),
    "excel_rtd/windows": Path("crates/xlfn/src/excel_rtd/windows.rs"),
}
WINDOWS_MODULES = {
    "host/drain/windows",
    "excel_rtd/windows/server/termination",
    "excel_rtd/windows/server_gate",
    "host/process_quota/windows",
    "excel_rtd/windows",
    "excel_rtd/windows/server",
    "subscription/data_plane/refresh/publication",
    "subscription/data_plane/refresh/windows",
}
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
    paths = (
        subprocess.check_output(
            ["git", "ls-files", "-z", "--cached", "--others", "--exclude-standard"],
            cwd=root,
        )
        .decode("utf-8")
        .split("\0")
    )
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


@dataclass(frozen=True)
class CompileConfig:
    package: str
    features: tuple[str, ...]
    target: str | None


def compile_config(
    fixture: Fixture, compile_target: str | None, extra_features: tuple[str, ...]
) -> CompileConfig:
    package = (
        "xlfn-kernel"
        if fixture.module.startswith("kernel/")
        else "xlfn-macros"
        if fixture.module.startswith("macros/")
        else "xlfn-host"
        if fixture.module.startswith("host/")
        else "xlfn-services"
        if fixture.module == "cache"
        else "xlfn"
    )
    if package == "xlfn":
        features = (*FEATURES, *extra_features)
    elif package == "xlfn-services":
        features = ("cache",)
    else:
        features = ()
    if fixture.module == "async_udf/builtin" and "async-builtin" not in features:
        features = (*features, "async-builtin")
    return CompileConfig(package, features, compile_target)


def compile_fixtures(
    cases: list[Fixture], overlay: Path, target: Path, config: CompileConfig
) -> tuple[dict, list[dict]]:
    package, features, compile_target = config.package, config.features, config.target
    command = [
        "cargo",
        "check",
        "-p",
        package,
        "--lib",
        "--locked",
        "--offline",
        "--no-default-features",
    ]
    if features:
        command.extend(["--features", ",".join(features)])
    command.extend(["--target-dir", str(target), "--message-format=json"])
    if compile_target:
        command.extend(["--target", compile_target])
    # Rust accepts literal Unicode; JSON's ASCII-only \uXXXX escapes are not
    # Rust string escapes and would reject non-ASCII checkout/fixture paths.
    declarations: dict[Path, list[bytes]] = {}
    for index, fixture in enumerate(cases):
        injected = overlay / MODULES[fixture.module]
        path_literal = json.dumps(str(overlay / fixture.path), ensure_ascii=False)
        # Multiple scopes can map to the same source file; namespace by index
        # so the fixture's own item names, imports, and inner attrs stay isolated.
        suffix = f"_{index}" if len(cases) > 1 else ""
        declarations.setdefault(injected, []).append(
            f"\n#[allow(dead_code)]\n#[path = {path_literal}]\n"
            f"mod typestate_compile_fixture{suffix};\n".encode()
        )
    with ExitStack() as restore:
        for source, items in declarations.items():
            # Restore only touched sources, byte-for-byte including CRLF.
            restore.callback(source.write_bytes, source.read_bytes())
            with source.open("ab") as stream:
                stream.writelines(items)
        completed = subprocess.run(
            command,
            cwd=overlay,
            text=True,
            encoding="utf-8",
            capture_output=True,
            env={**os.environ, "CARGO_TERM_COLOR": "never"},
        )
    errors = []
    workspace_artifacts_rebuilt = 0
    for line in completed.stdout.splitlines():
        try:
            event = json.loads(line)
        except json.JSONDecodeError:
            continue
        if (
            event.get("reason") == "compiler-artifact"
            and event.get("fresh") is False
            and (manifest := event.get("manifest_path"))
            and Path(manifest).is_relative_to(overlay)
        ):
            workspace_artifacts_rebuilt += 1
        if event.get("reason") == "compiler-message":
            message = event["message"]
            if message.get("level") == "error":
                errors.append(message)
    return {
        "target": compile_target or "host",
        "package": package,
        "features": list(features),
        "command": command,
        "exit_code": completed.returncode,
        "workspace_artifacts_rebuilt": workspace_artifacts_rebuilt,
        "diagnostics": [error.get("rendered", error["message"]) for error in errors],
        "stderr": completed.stderr,
    }, errors


def check_fixture(
    fixture: Fixture,
    overlay: Path,
    target: Path,
    compile_target: str | None = None,
    extra_features: tuple[str, ...] = (),
) -> dict:
    build, errors = compile_fixtures(
        [fixture], overlay, target, compile_config(fixture, compile_target, extra_features)
    )
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
        build["exit_code"] == 0 and not errors
        if not expected
        else build["exit_code"] != 0 and observed == expected and not unexpected
    )
    return {
        **build,
        "fixture": str(fixture.path),
        "passed": passed,
        "expected": sorted(expected),
        "observed": sorted(observed),
        "unexpected_errors": unexpected,
        "stderr": build["stderr"] if not passed else "",
    }


def check_cases(
    cases: list[Fixture],
    overlay: Path,
    target: Path,
    compile_target: str | None,
    extra_features: tuple[str, ...],
    jobs: int,
) -> tuple[list[dict], list[dict]]:
    positive = [fixture for fixture in cases if not fixture.errors]
    negative = [fixture for fixture in cases if fixture.errors]
    groups: dict[CompileConfig, list[Fixture]] = {}
    for fixture in positive:
        config = compile_config(fixture, compile_target, extra_features)
        groups.setdefault(config, []).append(fixture)

    results: dict[Path, dict] = {}
    attempts: list[dict] = []

    def record_attempt(group: list[Fixture], build: dict, kind: str, passed: bool) -> None:
        attempts.append(
            {
                **build,
                "fixtures": [str(fixture.path) for fixture in group],
                "kind": kind,
                "passed": passed,
                "source_restores": len({MODULES[fixture.module] for fixture in group}),
            }
        )

    for config, group in groups.items():
        if len(group) > 1:
            build, errors = compile_fixtures(group, overlay, target, config)
            passed = build["exit_code"] == 0 and not errors
            record_attempt(group, build, "positive-batch", passed)
            if passed:
                for fixture in group:
                    results[fixture.path] = {
                        **build,
                        "fixture": str(fixture.path),
                        "passed": True,
                        "expected": [],
                        "observed": [],
                        "unexpected_errors": [],
                        "stderr": "",
                        # The batch's cost belongs to its single invocation.
                        "workspace_artifacts_rebuilt": 0,
                    }
                continue

        # If the batch fails, attribute each failing control in an isolated
        # build. Do not accept negative evidence if the batch itself is broken,
        # even if every fixture passes in isolation.
        for fixture in group:
            result = check_fixture(fixture, overlay, target, compile_target, extra_features)
            record_attempt([fixture], result, "positive-single", result["passed"])
            results[fixture.path] = result

    if all(attempt["passed"] for attempt in attempts) and negative:
        workers = min(jobs, len(negative))

        def check_shard(
            shard: list[Fixture], worker_overlay: Path, worker_target: Path
        ) -> list[dict]:
            return [
                check_fixture(
                    fixture, worker_overlay, worker_target, compile_target, extra_features
                )
                for fixture in shard
            ]

        if workers == 1:
            negative_results = check_shard(negative, overlay, target)
        else:
            with tempfile.TemporaryDirectory(prefix="xlfn-typestate-worker-") as directory:
                # Clone the already hashed snapshot after complete restoration.
                # Each worker owns both a mutable overlay and a Cargo target
                # directory; otherwise either source injection or Cargo locks race.
                second_overlay = Path(directory)
                shutil.copytree(overlay, second_overlay, dirs_exist_ok=True)
                shards = [(overlay, target), (second_overlay, target / "worker-2")]
                with ThreadPoolExecutor(max_workers=workers) as executor:
                    futures = [
                        executor.submit(check_shard, negative[index::workers], *shards[index])
                        for index in range(workers)
                    ]
                    negative_results = [result for future in futures for result in future.result()]

        negative_by_path = {result["fixture"]: result for result in negative_results}
        for fixture in negative:
            result = negative_by_path[str(fixture.path)]
            record_attempt([fixture], result, "negative", result["passed"])
            results[fixture.path] = result

    # Worker completion and package batching never reorder fixture evidence.
    return [results[fixture.path] for fixture in cases if fixture.path in results], attempts


def main() -> int:
    # Windows CI may inherit a legacy console code page even though repository
    # files and Cargo's JSON diagnostics are UTF-8. Preserve their text in logs.
    for stream in (sys.stdout, sys.stderr):
        reconfigure = getattr(stream, "reconfigure", None)
        if callable(reconfigure):
            reconfigure(encoding="utf-8")
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target-dir", type=Path, default=ROOT / "target/typestate-check")
    parser.add_argument(
        "--jobs",
        type=int,
        choices=(1, 2),
        default=1,
        help="Independent negative-check workers (default: 1)",
    )
    parser.add_argument("--report", type=Path)
    parser.add_argument("--target", help="Cargo compile target; defaults to the rustc host")
    parser.add_argument(
        "--extra-features", default="", help="Additional comma-separated Cargo features"
    )
    args = parser.parse_args()
    cases = fixtures(ROOT)
    target = args.target_dir.resolve()
    rustc = subprocess.check_output(["rustc", "-Vv"], text=True, encoding="utf-8").strip()
    host = next(
        line.removeprefix("host: ") for line in rustc.splitlines() if line.startswith("host: ")
    )
    compile_target = args.target or host
    skipped = [
        fixture
        for fixture in cases
        if fixture.module in WINDOWS_MODULES and "-windows-" not in compile_target
    ]
    cases = [fixture for fixture in cases if fixture not in skipped]
    extra_features = tuple(filter(None, args.extra_features.split(",")))
    environment_keys = (
        "SDKROOT",
        "CC",
        "CFLAGS",
        "AR",
        *(f"{prefix}_{compile_target.replace('-', '_')}" for prefix in ("CC", "CFLAGS", "AR")),
    )
    report = {
        "rustc": rustc,
        "target": compile_target,
        "git_head": subprocess.check_output(
            ["git", "rev-parse", "HEAD"],
            cwd=ROOT,
            text=True,
            encoding="utf-8",
        ).strip(),
        "features": [*FEATURES, *extra_features],
        "fixtures": [],
        "build_environment": {
            key: os.environ[key] for key in environment_keys if key in os.environ
        },
        "skipped_fixtures": [
            {"fixture": str(fixture.path), "reason": "requires a Windows target"}
            for fixture in skipped
        ],
    }
    if skipped:
        print(
            f"SKIP {len(skipped)} Windows fixtures on {compile_target}; run on Windows or use --target",
            flush=True,
        )
    started = time.perf_counter()
    with tempfile.TemporaryDirectory(prefix="xlfn-typestate-") as directory:
        overlay = Path(directory)
        report["source_sha256"] = copy_sources(ROOT, overlay)
        results, attempts = check_cases(
            cases, overlay, target, args.target, extra_features, args.jobs
        )
        report["fixtures"] = results
        report["attempts"] = attempts
        for attempt in attempts:
            if attempt["kind"] == "positive-batch" and not attempt["passed"]:
                print(
                    f"FAIL positive batch for {attempt['package']}; isolated results follow",
                    flush=True,
                )
                print("\n".join(attempt["diagnostics"]), file=sys.stderr)
                print(attempt["stderr"], file=sys.stderr)
        for result in results:
            print(f"{'PASS' if result['passed'] else 'FAIL'} {result['fixture']}", flush=True)
            if not result["passed"]:
                print("\n".join(result["diagnostics"]), file=sys.stderr)
                print(result["stderr"], file=sys.stderr)
    report["runner_cost"] = {
        "cargo_invocations": len(attempts),
        "fixture_injections": sum(len(attempt["fixtures"]) for attempt in attempts),
        "fixture_restores": sum(attempt["source_restores"] for attempt in attempts),
        "workspace_artifacts_rebuilt": sum(
            attempt["workspace_artifacts_rebuilt"] for attempt in attempts
        ),
        "elapsed_seconds": time.perf_counter() - started,
    }
    report["passed"] = len(results) == len(cases) and all(attempt["passed"] for attempt in attempts)
    if args.report:
        args.report.parent.mkdir(parents=True, exist_ok=True)
        args.report.write_text(
            json.dumps(report, indent=2, ensure_ascii=False) + "\n", encoding="utf-8"
        )
    return 0 if report["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())

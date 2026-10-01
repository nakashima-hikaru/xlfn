#!/usr/bin/env python3
"""Freeze two source/toolchain builds, compare Criterion cases and add-in size."""

from __future__ import annotations

import argparse
import hashlib
import io
import json
import os
from pathlib import Path
import platform
import shutil
import statistics
import subprocess
import sys
import tarfile


FEATURES = "bench-internals,async,cache,rtd"
FILTERS = {
    "sync_boundary": r"^sync_boundary/(admission|scalar_return/no_subscriber)/1$",
    "cache_lookup": r"^cache_lookup/cache_hit/u64/current/warm$",
    "cache_miss_concurrency": r"^cache_miss_concurrency/(distinct_keys/cheap_u64/cache/workers_1|same_key/cheap_u64/cache/workers_4)$",
    "async_spawn": r"^async_spawn/(per_iteration/1|matrix_reschedule/workers_4/16)$",
    "rtd_publish": r"^rtd_publish/number/(changing|same_value)$",
    "rtd_refresh": r"^rtd_refresh/number/end_to_end/dense$",
}
# This older harness sets its group duration directly rather than consulting
# XLFN_BENCH_MEASUREMENT_MS. Preserve the same harness in both source snapshots.
FIXED_MEASUREMENT_SECONDS = {"sync_boundary": 10.0}
REPOSITORY = Path(__file__).resolve().parents[2]


def output(command: list[str], *, cwd: Path = REPOSITORY) -> str:
    return subprocess.check_output(command, cwd=cwd, text=True).strip()


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def write_json(path: Path, data: dict) -> None:
    path.write_text(json.dumps(data, indent=2) + "\n", encoding="utf-8")


def source_digest(source: Path) -> str:
    digest = hashlib.sha256()
    for path in sorted(source.rglob("*")):
        if path.is_file():
            digest.update(path.relative_to(source).as_posix().encode() + b"\0")
            digest.update(path.read_bytes())
    return digest.hexdigest()


def snapshot_revision(destination: Path, revision: str) -> str:
    commit = output(["git", "rev-parse", f"{revision}^{{commit}}"])
    data = subprocess.check_output(["git", "archive", commit], cwd=REPOSITORY)
    destination.mkdir()
    with tarfile.open(fileobj=io.BytesIO(data)) as archive:
        archive.extractall(destination, filter="data")
    return commit


def snapshot_worktree(destination: Path) -> dict:
    destination.mkdir()
    names = subprocess.check_output(
        ["git", "ls-files", "-z", "--cached", "--others", "--exclude-standard"],
        cwd=REPOSITORY,
    ).split(b"\0")
    for name in names:
        if not name:
            continue
        relative = Path(os.fsdecode(name))
        source = REPOSITORY / relative
        if not source.is_file():
            continue
        target = destination / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(source, target)
    diff = subprocess.check_output(["git", "diff", "--binary", "HEAD"], cwd=REPOSITORY)
    return {"revision": output(["git", "rev-parse", "HEAD"]),
            "working_tree_diff_sha256": hashlib.sha256(diff).hexdigest(),
            "working_tree_status": output(["git", "status", "--short"])}


def cargo_build(command: list[str], source: Path, target_dir: Path, log: Path,
                env: dict[str, str] | None = None) -> None:
    print("Building:", " ".join(command), flush=True)
    build_env = dict(os.environ, CARGO_TARGET_DIR=str(target_dir))
    if env:
        build_env.update(env)
    # Cargo's encoded form takes precedence over the whitespace-delimited form.
    # Keep the caller's flags and append identical virtual source/build roots
    # so differing snapshot path lengths cannot skew the size comparison.
    original_encoded = build_env.get("CARGO_ENCODED_RUSTFLAGS")
    flags = (original_encoded.split("\x1f") if original_encoded else
             build_env.get("RUSTFLAGS", "").split())
    if original_encoded is not None and not original_encoded:
        flags = []
    flags.extend([f"--remap-path-prefix={source}=/xlfn/source",
                  f"--remap-path-prefix={target_dir}=/xlfn/target"])
    build_env["CARGO_ENCODED_RUSTFLAGS"] = "\x1f".join(flags)
    write_json(log.with_suffix(".command.json"), {"argv": command, "cwd": str(source),
               "target_directory": str(target_dir), "environment_overrides": env or {},
               "effective_encoded_rustflags": build_env["CARGO_ENCODED_RUSTFLAGS"]})
    with log.open("w", encoding="utf-8") as stdout, log.with_suffix(".stderr").open("w", encoding="utf-8") as stderr:
        subprocess.run(command, cwd=source, env=build_env, stdout=stdout,
                       stderr=stderr, check=True)


def artifacts(log: Path) -> list[dict]:
    return [record for line in log.read_text(encoding="utf-8").splitlines()
            if (record := json.loads(line)).get("reason") == "compiler-artifact"]


def describe_build(kind: str, source: Path, toolchain: str, target: str,
                   source_info: dict, log: Path) -> dict:
    executables = {record["target"]["name"]: record["executable"]
                   for record in artifacts(log) if record.get("executable")}
    if not set(FILTERS).issubset(executables):
        raise ValueError(f"{kind} build omitted requested Criterion executables")
    return {"source": source_info, "source_sha256": source_digest(source),
            "toolchain": toolchain,
            "rustc_vv": output(["rustc", f"+{toolchain}", "-vV"]),
            "target": target, "features": FEATURES.split(","),
            "profile": "release", "environment": {
                name: os.environ.get(name)
                for name in ("SDKROOT", "RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS")},
            "build_command": json.loads(log.with_suffix(".command.json").read_text(encoding="utf-8")),
            "executables": {name: {"path": path, "sha256": sha256(Path(path))}
                            for name, path in executables.items() if name in FILTERS}}


def build_sizes(root: Path, kind: str, source: Path, toolchain: str,
                target: str, counts: list[int]) -> dict:
    manifest = source / "benchmarks/excel-comparison/xlfn/Cargo.toml"
    result = {"extra_registration_counts": counts, "target": target,
              "fixture_manifest": str(manifest), "features": ["async", "rtd"], "files": []}
    destination = root / "size-binaries" / kind
    destination.mkdir(parents=True)
    for count in counts:
        log = root / f"{kind}-size-{count}.jsonl"
        cargo_build(["cargo", f"+{toolchain}", "build", "--manifest-path", str(manifest),
                     "--release", "--target", target, "--locked", "--message-format=json"],
                    source, root / f"{kind}-size-build", log,
                    {"BENCH_EXTRA_FUNCTIONS": str(count)})
        libraries = [Path(name) for record in artifacts(log)
                     if record["target"]["name"] == "xlfn_excel_comparison"
                     for name in record["filenames"] if Path(name).suffix in (".dll", ".dylib", ".so")]
        if len(libraries) != 1:
            raise ValueError(f"expected one add-in library, got {libraries}")
        frozen = destination / f"registration-{count}{libraries[0].suffix}"
        shutil.copy2(libraries[0], frozen)
        result["files"].append({"extra_registrations": count,
                                "bytes": frozen.stat().st_size,
                                "sha256": sha256(frozen), "path": str(frozen),
                                "build_command": json.loads(log.with_suffix(".command.json").read_text(encoding="utf-8"))})
    if len(counts) > 1:
        xs = [entry["extra_registrations"] for entry in result["files"]]
        ys = [entry["bytes"] for entry in result["files"]]
        mean_x, mean_y = statistics.mean(xs), statistics.mean(ys)
        result["size_slope_bytes_per_extra_registration"] = (
            sum((x - mean_x) * (y - mean_y) for x, y in zip(xs, ys)) /
            sum((x - mean_x) ** 2 for x in xs))
        result["interval_slopes"] = [
            {"from": left["extra_registrations"], "to": right["extra_registrations"],
             "bytes_per_extra_registration": (right["bytes"] - left["bytes"]) /
             (right["extra_registrations"] - left["extra_registrations"])}
            for left, right in zip(result["files"], result["files"][1:])]
    return result


def prepare(args: argparse.Namespace) -> None:
    root = args.work_dir
    root.mkdir(parents=True, exist_ok=False)
    baseline = root / "baseline-source"
    candidate = root / "candidate-source"
    baseline_info = {"revision": snapshot_revision(baseline, args.baseline_ref)}
    if args.candidate_ref:
        candidate_info = {"revision": snapshot_revision(candidate, args.candidate_ref)}
    else:
        candidate_info = snapshot_worktree(candidate)
    target = args.target or next(line[6:] for line in
                                output(["rustc", f"+{args.candidate_toolchain}", "-vV"]).splitlines()
                                if line.startswith("host: "))
    for kind, source, toolchain, info in (
        ("baseline", baseline, args.baseline_toolchain, baseline_info),
        ("candidate", candidate, args.candidate_toolchain, candidate_info),
    ):
        command = ["cargo", f"+{toolchain}", "bench", "-p", "xlfn", "--features", FEATURES,
                   "--target", target, "--no-run", "--locked", "--message-format=json"]
        for bench in FILTERS:
            command.extend(["--bench", bench])
        log = root / f"{kind}-build.jsonl"
        cargo_build(command, source, root / f"{kind}-build", log)
        provenance = describe_build(kind, source, toolchain, target, info, log)
        provenance["sizes"] = build_sizes(root, kind, source, toolchain, target,
                                          args.registration_counts)
        write_json(root / f"{kind}-provenance.json", provenance)
    write_json(root / "plan.json", {"target": target, "filters": FILTERS,
                                    "effective_measurement_seconds": {
                                        bench: FIXED_MEASUREMENT_SECONDS.get(bench, args.measurement_seconds)
                                        for bench in FILTERS},
                                    "measurement_seconds": args.measurement_seconds,
                                    "warmup_seconds": args.warmup_seconds,
                                    "samples": args.samples})


def measure(root: Path) -> None:
    plan = json.loads((root / "plan.json").read_text(encoding="utf-8"))
    builds = {kind: json.loads((root / f"{kind}-provenance.json").read_text(encoding="utf-8"))
              for kind in ("baseline", "candidate")}
    for provenance in builds.values():
        for record in provenance["executables"].values():
            if sha256(Path(record["path"])) != record["sha256"]:
                raise ValueError("built benchmark changed after provenance was recorded")
        for record in provenance["sizes"]["files"]:
            if sha256(Path(record["path"])) != record["sha256"]:
                raise ValueError("frozen add-in changed after provenance was recorded")
    command = [sys.executable, "-B", str(REPOSITORY / "benchmarks/performance/compare.py"),
               "--baseline-build", str(root / "baseline-build.jsonl"),
               "--candidate-build", str(root / "candidate-build.jsonl"),
               "--baseline-provenance", str(root / "baseline-provenance.json"),
               "--candidate-provenance", str(root / "candidate-provenance.json"),
               "--features", FEATURES, "--baseline-revision", builds["baseline"]["source"]["revision"],
               "--work-dir", str(root / "paired"), "--output", str(root / "paired.json"),
               "--measurement-seconds", str(plan["measurement_seconds"]),
               "--warmup-seconds", str(plan["warmup_seconds"]), "--samples", str(plan["samples"])]
    for bench, pattern in plan["filters"].items():
        command.extend(["--filter", f"{bench}={pattern}"])
        seconds = plan.get("effective_measurement_seconds", {}).get(
            bench, FIXED_MEASUREMENT_SECONDS.get(bench, plan["measurement_seconds"]))
        command.extend(["--benchmark-measurement", f"{bench}={seconds}"])
    subprocess.run(command, check=True)
    paired = json.loads((root / "paired.json").read_text(encoding="utf-8"))
    paired["interpretation"] = (
        "Source and compiler adoption comparison; source changes and LLVM effects are combined. "
        "Criterion times describe framework batches on this host, not live Excel UDF latency.")
    paired["target"] = plan["target"]
    paired["measurement_host"] = platform.platform()
    paired["processor"] = (output(["sysctl", "-n", "machdep.cpu.brand_string"])
                           if platform.system() == "Darwin" else platform.processor())
    write_json(root / "paired.json", paired)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--work-dir", type=lambda path: Path(path).resolve(), required=True)
    parser.add_argument("--phase", choices=("prepare", "measure", "all"), default="all")
    parser.add_argument("--baseline-ref")
    parser.add_argument("--candidate-ref", help="Omit to snapshot the current worktree.")
    parser.add_argument("--baseline-toolchain", default="1.98.1")
    parser.add_argument("--candidate-toolchain", default="1.99.0")
    parser.add_argument("--target", help="Defaults to the candidate compiler host target.")
    parser.add_argument("--registration-counts", type=int, nargs="+", default=[0, 10, 100, 1000, 5000])
    parser.add_argument("--measurement-seconds", type=float, default=1.0)
    parser.add_argument("--warmup-seconds", type=float, default=0.3)
    parser.add_argument("--samples", type=int, default=50)
    args = parser.parse_args()
    if args.phase != "measure":
        if not args.baseline_ref:
            parser.error("--baseline-ref is required when preparing builds")
        if args.registration_counts != sorted(set(args.registration_counts)) or not all(
                0 <= count <= 5000 for count in args.registration_counts):
            parser.error("registration counts must be increasing, unique and within 0..5000")
        if args.measurement_seconds <= 0 or args.warmup_seconds <= 0 or args.samples < 10:
            parser.error("measurement/warmup must be positive; samples must be at least 10")
        prepare(args)
    if args.phase != "prepare":
        measure(args.work_dir)


if __name__ == "__main__":
    main()

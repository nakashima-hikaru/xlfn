#!/usr/bin/env python3
"""Compare consumer clean builds using separate source trees and Cargo targets."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import statistics
import subprocess
import time
import tomllib


def run(command, cwd, env):
    result = subprocess.run(command, cwd=cwd, env=env, capture_output=True, text=True)
    if result.returncode:
        raise RuntimeError(result.stdout + result.stderr)
    return result


def source_digest(source):
    digest = hashlib.sha256()
    files = sorted(
        p for p in (source / "crates").rglob("*")
        if p.is_file() and "target" not in p.relative_to(source).parts
        and (p.suffix == ".rs" or p.name == "Cargo.toml")
    )
    files += [source / "Cargo.toml", source / "Cargo.lock", source / "rust-toolchain.toml"]
    for path in files:
        digest.update(str(path.relative_to(source)).encode())
        digest.update(b"\0")
        digest.update(path.read_bytes())
        digest.update(b"\0")
    return digest.hexdigest()


def make_probe(directory, source, fixture, functions, statements):
    (directory / "src").mkdir(parents=True, exist_ok=True)
    # JSON string quoting is also valid for these TOML strings.
    dependency = json.dumps(str(source / "crates/xlfn"))
    (directory / "Cargo.toml").write_text(
        '[package]\nname="build-time-consumer"\nversion="0.0.0"\nedition="2024"\n'
        f'[workspace]\n[features]\nasync=["xlfn/async"]\n'
        'async-builtin=["async","xlfn/async-builtin"]\n'
        f'[dependencies]\nxlfn={{path={dependency}}}\n'
    )
    (directory / "rust-toolchain.toml").write_bytes((source / "rust-toolchain.toml").read_bytes())
    body = fixture
    for n in range(functions):
        body += f'\n#[excel_function(thread_safe)]\nfn build_probe_{n}(x: f64) -> f64 {{\nlet v0=x;\n'
        for i in range(1, statements + 1):
            body += f"let v{i}=v{i-1}+x*{i}.0;\n"
        body += f"v{statements}\n}}\n"
    (directory / "src/lib.rs").write_text(body)
    # Start each probe with its source tree's pinned dependency versions.
    (directory / "Cargo.lock").write_bytes((source / "Cargo.lock").read_bytes())
    return hashlib.sha256(body.encode()).hexdigest()


def timing_units(report):
    """Read Cargo's embedded JSON without truncating strings containing `];`."""
    content = report.read_text()
    assignment = re.search(r"\bconst UNIT_DATA\s*=\s*", content)
    if assignment is None:
        raise RuntimeError(f"Cargo timing report has no UNIT_DATA: {report}")
    units, _ = json.JSONDecoder().raw_decode(content[assignment.end():])
    if not isinstance(units, list):
        raise RuntimeError(f"Cargo timing UNIT_DATA is not a list: {report}")
    return units


def clean_command(package, cache, rebuild, profile):
    command = ["cargo", "clean"]
    if cache == "dependencies":
        packages = [package]
        if rebuild == "framework":
            packages += ["xlfn", "xlfn-macros"]
        for selected in packages:
            command += ["-p", selected]
        if profile == "release":
            command += ["--release"]
    return command


def feature_arguments(directory, features):
    if not features:
        return []
    manifest = tomllib.loads((directory / "Cargo.toml").read_text())
    local = manifest.get("features", {})
    return ["--features", ",".join(
        feature if feature in local else "xlfn/" + feature for feature in features
    )]


def measure(directory, package, target, mode, iteration, label, args):
    env = dict(os.environ, CARGO_TARGET_DIR=str(target),
               RUSTC_WRAPPER="", RUSTC_WORKSPACE_WRAPPER="")
    if args.incremental == "off":
        env["CARGO_INCREMENTAL"] = "0"
    run(clean_command(package, args.cache, args.rebuild, args.profile), directory, env)
    command = ["cargo", mode, "--locked", "--offline", "--timings"]
    if args.profile == "release":
        command += ["--release"]
    features = args.baseline_features if label == "baseline" and args.baseline_features is not None else args.features
    command += feature_arguments(directory, features)
    start = time.perf_counter()
    run(command, directory, env)
    elapsed = time.perf_counter() - start
    report = max((target / "cargo-timings").glob("cargo-timing-2*.html"), key=lambda p: p.stat().st_mtime_ns)
    units = timing_units(report)
    consumer = next((unit for unit in units if unit["name"] == package), None)
    if consumer is None:
        raise RuntimeError(
            f"Cargo timing report has no rebuilt consumer unit {package}; "
            "check profile-specific cache invalidation"
        )
    critical = [
        {key: unit[key] for key in ["name", "features", "start", "duration", "sections"]}
        for unit in units
        if unit["name"] in {"xlfn", "xlfn-macros", "syn", "serde_derive", "serde_core"}
    ]
    return dict(label=label, mode=mode, iteration=iteration, wall_seconds=elapsed,
                unit_seconds=consumer["duration"], sections=consumer["sections"],
                serde_derive_compiled=any(u["name"] == "serde_derive" for u in units),
                critical_units=critical, all_units=units,
                slowest_units=sorted(units, key=lambda u: u["duration"], reverse=True)[:20])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--baseline", type=Path, required=True)
    parser.add_argument("--candidate", type=Path, default=Path.cwd())
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--functions", type=int, default=100)
    parser.add_argument("--statements", type=int, default=50)
    parser.add_argument("--repeat", type=int, default=5)
    parser.add_argument("--cache", choices=["clean", "dependencies"], default="clean",
                        help="clean removes all artifacts; dependencies warms before timed rebuilds")
    parser.add_argument("--rebuild", choices=["consumer", "framework"], default="consumer",
                        help="with --cache dependencies, rebuild only the consumer or also xlfn and xlfn-macros")
    parser.add_argument("--workload", choices=["basic", "rtd", "generated"], default="basic")
    parser.add_argument("--profile", choices=["dev", "release"], default="dev")
    parser.add_argument("--incremental", choices=["auto", "off"], default="auto")
    parser.add_argument("--features", nargs="*", default=[], help="additional xlfn features")
    parser.add_argument("--baseline-features", nargs="*", default=None,
                        help="baseline feature set when names changed; defaults to --features")
    parser.add_argument("--allow-fixture-differences", action="store_true",
                        help="compare API-migrated fixtures while recording their distinct hashes")
    parser.add_argument("--modes", nargs="+", choices=["check", "build"], default=["build"])
    args = parser.parse_args()
    if min(args.functions, args.statements, args.repeat) < 1:
        parser.error("functions, statements, and repeat must be positive")
    sources = {"baseline": args.baseline.resolve(), "candidate": args.candidate.resolve()}
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    probes = {}
    report = dict(platform=platform.platform(), machine=platform.machine(),
                  rustc=run(["rustc", "-vV"], sources["candidate"], os.environ).stdout,
                  functions=args.functions, statements=args.statements, repeat=args.repeat,
                  workload=args.workload, cache=args.cache, profile=args.profile,
                  rebuild=args.rebuild,
                  features=args.features, baseline_features=args.baseline_features,
                  allow_fixture_differences=args.allow_fixture_differences,
                  incremental=args.incremental,
                  compiler_cache_wrappers=False, sources={}, samples=[], summary={})
    for label, source in sources.items():
        if args.workload == "generated":
            directory = output / (label + "-consumer")
            fixture = (source / "examples/basic-xll/src/lib.rs").read_text()
            fixture_hash = make_probe(directory, source, fixture, args.functions, args.statements)
            package = "build-time-consumer"
        else:
            example = "basic-xll" if args.workload == "basic" else "rtd-source"
            directory = source / "examples" / example
            package = "basic-xlfn" if args.workload == "basic" else "rtd-source"
            digest = hashlib.sha256()
            for path in sorted((directory / "src").rglob("*.rs")):
                digest.update(path.relative_to(directory).as_posix().encode())
                digest.update(path.read_bytes())
            fixture_hash = digest.hexdigest()
        probes[label] = (directory, package)
        metadata = ["cargo", "metadata", "--format-version", "1", "--offline"]
        features = args.baseline_features if label == "baseline" and args.baseline_features is not None else args.features
        metadata += feature_arguments(directory, features)
        # Resolve the standalone package before timing; cold samples still
        # remove all compiled artifacts, including proc macros and build scripts.
        run(metadata, directory, os.environ)
        report["sources"][label] = dict(path=str(source), sha256=source_digest(source))
        report["sources"][label]["fixture_sha256"] = fixture_hash
    if (not args.allow_fixture_differences
            and report["sources"]["baseline"]["fixture_sha256"] != report["sources"]["candidate"]["fixture_sha256"]):
        raise RuntimeError("baseline and candidate consumer sources differ")
    for mode in args.modes:
        first = 0 if args.cache == "clean" else -1
        for iteration in range(first, args.repeat):
            order = ["baseline", "candidate"] if iteration % 2 == 0 or iteration == -1 else ["candidate", "baseline"]
            for label in order:
                directory, package = probes[label]
                expected_digest = report["sources"][label]["sha256"]
                if source_digest(sources[label]) != expected_digest:
                    raise RuntimeError(f"{label} source changed after provenance was recorded")
                sample = measure(directory, package,
                                 output / (label + "-target-" + mode + "-" + args.profile),
                                 mode, iteration, label, args)
                if source_digest(sources[label]) != expected_digest:
                    raise RuntimeError(f"{label} source changed during the measured build")
                report["samples"].append(sample)
                print(json.dumps({key: sample[key] for key in
                                  ["label", "mode", "iteration", "wall_seconds", "unit_seconds",
                                   "serde_derive_compiled"]}), flush=True)
                (output / "results.json").write_text(json.dumps(report, indent=2) + "\n")
        medians = {
            label: statistics.median(s["wall_seconds"] for s in report["samples"]
                                     if s["label"] == label and s["mode"] == mode and s["iteration"] >= 0)
            for label in sources
        }
        report["summary"][mode] = dict(median_seconds=medians,
                                       change_percent=100 * (medians["candidate"] / medians["baseline"] - 1))
    (output / "results.json").write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(report["summary"], indent=2))


if __name__ == "__main__":
    main()

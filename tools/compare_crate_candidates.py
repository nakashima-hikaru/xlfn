#!/usr/bin/env python3
"""Compare already-built xlfn benchmark executables without concurrent builds."""

import argparse
import hashlib
import json
import os
import platform
import statistics
import subprocess
import tempfile
from pathlib import Path


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--baseline-bins", type=Path, required=True)
    parser.add_argument("--candidate-bins", type=Path, required=True)
    parser.add_argument("--area", choices=["rtd", "handles", "publication"], required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--measurement-ms", type=int, default=400)
    parser.add_argument("--repetitions", type=int, default=3)
    args = parser.parse_args()
    if args.measurement_ms <= 0 or args.repetitions <= 0:
        parser.error("measurement time and repetitions must be positive")
    benches = (
        [("rtd_topic_storage", "^rtd_topic_storage/", 42),
         ("rtd_prepare", "^rtd_subscribe_input/", 9)]
        if args.area == "rtd"
        else [("handle_lookup", "^handle_lookup/", 8),
              ("handle_prepare", "^handle_prepare/(cold_miss_batch_100|warm_hit_batch_100|distinct_key/|revision_churn/|cold_grow/(1000|10000)$)", 9),
              ("handle_call_resolution", "^handle_call_resolution/", None)]
    )
    if args.area == "publication":
        benches = [("handle_prepare", "^handle_prepare/republish/", 1)]
    bins = {"baseline": args.baseline_bins.resolve(), "candidate": args.candidate_bins.resolve()}
    scratch = Path(tempfile.mkdtemp(prefix="xlfn-paired-bench-"))
    result = {
        "host": platform.platform(), "machine": platform.machine(),
        "measurement_ms": args.measurement_ms, "warmup_ms": 100,
        "sample_size_cli": 20, "repetitions": args.repetitions,
        "method": "alternating processes; median of per-run Criterion medians; no builds during timing",
        "logs_directory": str(scratch), "executables": {}, "runs": [],
    }
    for label, directory in bins.items():
        result["executables"][label] = {
            name: hashlib.sha256((directory / name).read_bytes()).hexdigest()
            for name, _, _ in benches
        }
    args.output.parent.mkdir(parents=True, exist_ok=True)
    env = dict(os.environ, XLFN_BENCH_MEASUREMENT_MS=str(args.measurement_ms))
    for repetition in range(args.repetitions):
        order = ["baseline", "candidate"] if repetition % 2 == 0 else ["candidate", "baseline"]
        for name, filter_pattern, expected in benches:
            for label in order:
                directory = scratch / f"{repetition}-{name}-{label}"
                directory.mkdir()
                print(f"repetition {repetition + 1}: {name} / {label}", flush=True)
                with (directory / "benchmark.log").open("w") as log:
                    subprocess.run(
                        [str(bins[label] / name), "--bench", "--warm-up-time", "0.1",
                         "--sample-size", "20", filter_pattern],
                        cwd=directory, env=env, stdout=log, stderr=subprocess.STDOUT, check=True,
                    )
                estimates = {}
                for metadata_path in (directory / "target/criterion").glob("**/new/benchmark.json"):
                    metadata = json.loads(metadata_path.read_text())
                    estimates[metadata["full_id"]] = json.loads(metadata_path.with_name("estimates.json").read_text())
                if expected is not None and len(estimates) != expected:
                    raise RuntimeError(f"{name}: expected {expected} cases, received {len(estimates)}")
                if not estimates:
                    raise RuntimeError(f"{name}: no Criterion estimates")
                result["runs"].append({"repetition": repetition, "variant": label, "bench": name, "estimates": estimates})
                args.output.write_text(json.dumps(result, indent=2) + "\n")
    cases = sorted({case for run in result["runs"] for case in run["estimates"]})
    result["summary"] = {}
    for case in cases:
        medians = {
            label: statistics.median(
                run["estimates"][case]["median"]["point_estimate"]
                for run in result["runs"] if run["variant"] == label and case in run["estimates"]
            ) for label in bins
        }
        result["summary"][case] = {
            "baseline_ns": medians["baseline"], "candidate_ns": medians["candidate"],
            "change_percent": (medians["candidate"] / medians["baseline"] - 1) * 100,
        }
    if args.area == "rtd":
        result["allocation_probes"] = {}
        for label, directory in bins.items():
            executable = directory / "rtd_topic_allocations"
            output = subprocess.check_output([str(executable)], text=True)
            result["executables"][label]["rtd_topic_allocations"] = hashlib.sha256(executable.read_bytes()).hexdigest()
            result["allocation_probes"][label] = [json.loads(line) for line in output.splitlines()]
    args.output.write_text(json.dumps(result, indent=2) + "\n")
    for case, metrics in result["summary"].items():
        print(f"{case}: {metrics['baseline_ns']:.1f} -> {metrics['candidate_ns']:.1f} ns ({metrics['change_percent']:+.1f}%)")


if __name__ == "__main__":
    main()

#!/usr/bin/env python3
"""Run resident-backend benchmarks serially; retain timings and probe JSONL.

Run from the workspace root. No production selection is changed by this tool.
Raw Criterion console logs stay beside the machine-readable results.
"""
import argparse
import json
import os
from pathlib import Path
import subprocess

BACKENDS = ["moka", "sharded8", "sharded16", "sharded32", "sharded64"]
LOOKUP = r"^cache_lookup/(cache_hit/u64/current/warm|cache_hit_hot_key/current/threads_(1|8|32)/u64|cache_hit_disjoint/current/threads_(8|32)/u64|eviction_with_live_lease/current|concurrent_clear_latency/scope_100)$"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--supplemental-only", action="store_true")
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=True)
    features = "unstable-cache bench-internals"
    base = ["cargo", "bench", "-p", "xlfn", "--features", features, "--locked"]
    if not args.supplemental_only:
        with (args.output / "criterion.jsonl").open("w") as out:
            for repetition in range(3):
                for backend in BACKENDS[repetition:] + BACKENDS[:repetition]:
                    for bench in ["cache_lookup", "cache_reclamation"]:
                        print(f"repetition={repetition} backend={backend} bench={bench}", flush=True)
                        env = dict(os.environ, XLFN_CACHE_BACKEND=backend, XLFN_BENCH_MEASUREMENT_MS="1000")
                        command = base + ["--bench", bench, "--", "--warm-up-time", "0.2", "--sample-size", "20", "--noplot"]
                        if bench == "cache_lookup":
                            command.append(LOOKUP)
                        log = args.output / f"{repetition}-{backend}-{bench}.log"
                        with log.open("w") as stream:
                            subprocess.run(command, env=env, stdout=stream, stderr=subprocess.STDOUT, check=True)
                        for line in log.read_text().splitlines():
                            if line.startswith("cache_reclamation_probe "):
                                probe = json.loads(line.split(" ", 1)[1])
                                probe.update(kind="reclamation_probe", repetition=repetition, backend=backend)
                                out.write(json.dumps(probe) + "\n")
                        expected = 8 if bench == "cache_lookup" else 12
                        records = []
                        import re
                        for metadata_path in (Path("target/criterion") / bench).glob("**/new/benchmark.json"):
                            metadata = json.loads(metadata_path.read_text())
                            identifier = metadata["full_id"]
                            if bench == "cache_lookup" and not re.fullmatch(LOOKUP, identifier):
                                continue
                            estimates_path = metadata_path.with_name("estimates.json")
                            records.append(dict(kind="criterion", backend=backend, repetition=repetition,
                                benchmark=identifier, estimates=json.loads(estimates_path.read_text())))
                        assert len(records) == expected, (bench, len(records), expected)
                        for record in sorted(records, key=lambda record: record["benchmark"]):
                            out.write(json.dumps(record) + "\n")
                        out.flush()
    print("supplemental throughput, tails, residency and controlled debt", flush=True)
    with (args.output / "supplemental.log").open("w") as stream:
        subprocess.run(base + ["--bench", "cache_backends"], stdout=stream, stderr=subprocess.STDOUT, check=True)
    with (args.output / "supplemental.jsonl").open("w") as out:
        for line in (args.output / "supplemental.log").read_text().splitlines():
            if line.startswith("{"):
                out.write(json.dumps(json.loads(line)) + "\n")
    print("completed", flush=True)


if __name__ == "__main__":
    main()

#!/usr/bin/env python3
"""Run all eight proof targets and all mutation suites in one bounded invocation."""

import argparse
import json
import os
from pathlib import Path
import platform
import runpy
import subprocess

from verus_mutations import ROOT, MutationPlan, MutationRunner, Snapshot, collect_mutations


SUITES = (
    "check_drain_gate_refinement.py",
    "check_published_owner_permission.py",
    "check_rotating_domain_refinement.py",
    "check_cache_pin_refinement.py",
    "check_cache_ownership_refinement.py",
    "check_handle_completion_refinement.py",
)
PROOFS = ("sealable_counter", "drain_gate", "published_owner", "operation_gate",
          "rotating_read_domain", "service_slot", "cache_lease", "handle_domain")


def build_plan() -> MutationPlan:
    plan = MutationPlan()
    previous = Path.cwd()
    try:
        # Existing suite dependency globs are repository-relative.
        os.chdir(ROOT)
        with collect_mutations(plan):
            for suite in SUITES:
                runpy.run_path(str(ROOT / "tools" / suite), run_name="__main__")
        covered = {snapshot.proof.name for snapshot in plan.snapshots.values()}
        for name in PROOFS:
            if name not in covered:
                dependencies = (Path("crates/xlfn-kernel/src/sealable_counter/transitions.rs"),) if name == "sealable_counter" else ()
                plan.add_baseline(Snapshot.read(Path("verification/verus") / name, dependencies))
    finally:
        os.chdir(previous)
    return plan


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--jobs", type=int, default=2, help="concurrent verifier processes (default: 2)")
    parser.add_argument("--threads", type=int, help="verifier threads per process (default: CPUs / jobs)")
    parser.add_argument("--report", type=Path, help="write successful baseline and mutation timings as JSON")
    parser.add_argument("--list", action="store_true", help="list exact mutation inputs without running Verus")
    parser.add_argument("--shard-index", type=int, default=0, help="zero-based mutation shard (default: 0)")
    parser.add_argument("--shard-count", type=int, default=1, help="number of disjoint mutation shards (default: 1)")
    args = parser.parse_args()
    if args.jobs < 1 or (args.threads is not None and args.threads < 1):
        parser.error("jobs and threads must be positive")
    if args.shard_count < 1 or not 0 <= args.shard_index < args.shard_count:
        parser.error("shard index must be in [0, shard count)")
    plan = build_plan()
    total = len(plan.mutations)
    plan = plan.shard(args.shard_index, args.shard_count)
    if args.list:
        print(json.dumps([mutation.describe() for mutation in plan.mutations], indent=2))
        return
    report = MutationRunner(args.jobs, args.threads).run(plan)
    if args.report:
        report.update({"shard_index": args.shard_index, "shard_count": args.shard_count,
                       "total_mutations": total, "platform": platform.platform(),
                       "verus_version": subprocess.check_output(["verus", "--version"], text=True, timeout=30)})
        args.report.write_text(json.dumps(report, indent=2) + "\n")


if __name__ == "__main__":
    main()

#!/usr/bin/env python3
"""Require all CI shards to report exactly the current mutation inventory."""

import argparse
import json
from pathlib import Path

from check_verus_refinement import build_plan
from verus_mutations import MutationPlan


def check_reports(plan: MutationPlan, reports: list[dict], count: int) -> None:
    if count < 1 or len(reports) != count:
        raise ValueError(f"expected {count} shard reports, got {len(reports)}")
    indices = [report["shard_index"] for report in reports]
    if sorted(indices) != list(range(count)):
        raise ValueError("missing or duplicate shard indices")
    versions = {report["verus_version"] for report in reports}
    if len(versions) != 1 or not next(iter(versions)).strip():
        raise ValueError("shards must use the same Verus version")
    expected_baselines = {(str(snapshot.proof), snapshot.digest) for snapshot in plan.snapshots.values()}
    for report in reports:
        index = report["shard_index"]
        if report["shard_count"] != count or report["total_mutations"] != len(plan.mutations):
            raise ValueError(f"shard {index}: incorrect shard count or inventory size")
        baselines = report["baselines"]
        actual_baselines = {(baseline["proof"], baseline["sha256"]) for baseline in baselines}
        if (actual_baselines != expected_baselines or len(baselines) != len(expected_baselines)
                or any(baseline["verified"] < 1 for baseline in baselines)):
            raise ValueError(f"shard {index}: baseline inputs do not match")
        expected = [mutation.describe() for mutation in plan.shard(index, count).mutations]
        actual = [{key: mutation[key] for key in expected[0]} for mutation in report["mutations"]] if expected else report["mutations"]
        if actual != expected:
            raise ValueError(f"shard {index}: missing, duplicate or changed mutation inputs")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directory", type=Path)
    parser.add_argument("--shard-count", type=int, default=4)
    args = parser.parse_args()
    reports = [json.loads(path.read_text()) for path in sorted(args.directory.glob("verus-timings-*.json"))]
    plan = build_plan()
    try:
        check_reports(plan, reports, args.shard_count)
    except (ValueError, KeyError, TypeError) as error:
        raise SystemExit(f"FAIL: {error}") from error
    print(f"PASS: all {len(plan.mutations)} mutation cases reported exactly once across {len(reports)} shards")


if __name__ == "__main__":
    main()

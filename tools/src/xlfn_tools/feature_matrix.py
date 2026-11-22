"""Run the xlfn library's feature powerset, balanced across deterministic shards."""

import argparse
import itertools
import json
import subprocess
import time
import tomllib
from dataclasses import dataclass

from ._repository import ROOT

INTERNAL = frozenset(("bench-internals", "refinement"))
# Relative cost above the base crate, not measured Windows durations. Handles and
# RTD include the Windows RTD transport; count that shared cost only once.
FEATURE_COST = {"async": 2, "async-builtin": 1, "bench-internals": 1, "cache": 2, "handles": 2}


@dataclass(frozen=True)
class Case:
    requested: tuple[str, ...]
    enabled: tuple[str, ...]

    @property
    def weight(self) -> int:
        features = set(self.enabled)
        transport = 3 if features & {"rtd", "handles"} else 0
        return 10 + transport + sum(FEATURE_COST.get(feature, 1) for feature in features - {"rtd"})

    def command(self, mode: str) -> list[str]:
        command = ["cargo", mode, "--package", "xlfn", "--no-default-features", "--lib", "--locked"]
        if self.requested:
            command += ["--features", ",".join(self.requested)]
        if mode == "test":
            command += ["--", "--test-threads=1"]
        return command


def inventory(features: dict[str, list[str]], mode: str) -> list[Case]:
    excluded = {"default"} | (INTERNAL if mode == "test" else frozenset())
    selectable = sorted(features.keys() - excluded)

    def closure(selection: tuple[str, ...]) -> tuple[str, ...]:
        enabled = set(selection)
        pending = list(selection)
        while pending:
            for dependency in features[pending.pop()]:
                # Only exact local names participate. dep:foo and foo/bar (or
                # foo?/bar) configure dependencies, not local feature edges.
                if dependency in features and dependency not in enabled:
                    enabled.add(dependency)
                    pending.append(dependency)
        if enabled & excluded:
            raise ValueError(f"{mode} features enable excluded features: {enabled & excluded}")
        return tuple(sorted(enabled))

    by_enabled = {}
    for size in range(len(selectable) + 1):
        for requested in itertools.combinations(selectable, size):
            enabled = closure(requested)
            by_enabled.setdefault(enabled, Case(requested, enabled))
    return list(by_enabled.values())


def historical_test_seconds() -> dict[tuple[str, ...], int]:
    """Load the normalized per-configuration Windows test costs."""
    table = json.loads((ROOT / "tools/feature-test-costs.json").read_text(encoding="utf-8"))
    return {
        tuple(key.split(",")) if key else (): seconds for key, seconds in table["seconds"].items()
    }


def scheduling_weight(case: Case, historical: dict[tuple[str, ...], int]) -> int:
    if not historical:
        return case.weight
    # New configurations not present in the historical run use a proxy on the
    # same approximate scale (baseline cost 10; most test cases cost 40–100 s).
    return historical.get(case.enabled, 4 * case.weight)


def shards(
    cases: list[Case],
    count: int,
    historical: dict[tuple[str, ...], int] | None = None,
) -> list[list[Case]]:
    """Longest predicted cases first, with equal shard cardinality to within one."""
    if not 1 <= count <= len(cases):
        raise ValueError("shard count must be in [1, number of cases]")
    groups: list[list[Case]] = [[] for _ in range(count)]
    loads = [0] * count
    quotient, remainder = divmod(len(cases), count)
    capacities = [quotient + (index < remainder) for index in range(count)]
    weights = historical or {}
    for case in sorted(cases, key=lambda c: (-scheduling_weight(c, weights), c.enabled)):
        index = min(
            (i for i in range(count) if len(groups[i]) < capacities[i]),
            key=lambda i: (loads[i], len(groups[i]), i),
        )
        groups[index].append(case)
        loads[index] += scheduling_weight(case, weights)
    if historical:
        # Greedy placement respects the case count; exchanges can correct the
        # final imbalance without changing that count or duplicating any case.
        while True:
            best_gain = 0
            swap = None
            for left, right in itertools.combinations(range(count), 2):
                for i, a in enumerate(groups[left]):
                    for j, b in enumerate(groups[right]):
                        delta = scheduling_weight(a, weights) - scheduling_weight(b, weights)
                        gain = delta * (loads[left] - loads[right] - delta)
                        if gain > best_gain:
                            best_gain = gain
                            swap = (left, right, i, j, delta)
            if swap is None:
                break
            left, right, i, j, delta = swap
            groups[left][i], groups[right][j] = groups[right][j], groups[left][i]
            loads[left] -= delta
            loads[right] += delta
    return groups


def parse_shard(value: str) -> tuple[int, int]:
    try:
        index, count = map(int, value.split("/"))
    except ValueError as error:
        raise argparse.ArgumentTypeError("shard must be INDEX/COUNT (one-based)") from error
    if not 1 <= index <= count:
        raise argparse.ArgumentTypeError("shard index must be in [1, shard count]")
    return index, count


def run_cases(cases: list[Case], mode: str) -> int:
    failures = []
    for case in cases:
        command = case.command(mode)
        print(f"+ {' '.join(command)}", flush=True)
        started = time.monotonic()
        result = subprocess.run(command, cwd=ROOT, check=False)
        elapsed = time.monotonic() - started
        print(f"exit={result.returncode} seconds={elapsed:.2f}", flush=True)
        if result.returncode:
            failures.append(", ".join(case.enabled) or "(no features)")
    if failures:
        print(f"FAIL: {len(failures)} feature cases failed: {'; '.join(failures)}", flush=True)
        return 1
    return 0


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mode", choices=("check", "test"))
    parser.add_argument("--shard", type=parse_shard, default="1/1")
    parser.add_argument(
        "--list", action="store_true", help="emit the shard plan without running Cargo"
    )
    args = parser.parse_args()
    index, count = args.shard
    with (ROOT / "crates/xlfn/Cargo.toml").open("rb") as manifest:
        features = tomllib.load(manifest)["features"]
    historical = historical_test_seconds() if args.mode == "test" else {}
    try:
        selected = shards(inventory(features, args.mode), count, historical)[index - 1]
    except ValueError as error:
        parser.error(str(error))
    if args.list:
        print(
            json.dumps(
                [
                    {
                        "enabled": case.enabled,
                        "weight": scheduling_weight(case, historical),
                        "command": case.command(args.mode),
                    }
                    for case in selected
                ],
                indent=2,
            )
        )
        return
    print(f"xlfn feature {args.mode}: shard {index}/{count}, {len(selected)} cases", flush=True)
    raise SystemExit(run_cases(selected, args.mode))


if __name__ == "__main__":
    main()

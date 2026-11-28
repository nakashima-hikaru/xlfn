#!/usr/bin/env python3
"""Run all eight Verus formal verification proof targets."""

from __future__ import annotations

import argparse
import json
import os
import platform
import re
import subprocess
import time
from concurrent.futures import ThreadPoolExecutor, as_completed
from pathlib import Path

from ._repository import ROOT

PROOFS: tuple[str, ...] = (
    "sealable_counter",
    "drain_gate",
    "published_owner",
    "operation_gate",
    "rotating_read_domain",
    "service_slot",
    "cache_lease",
    "handle_domain",
)

VERIFIED_RE = re.compile(r"verification results:: ([1-9]\d*) verified, 0 errors")


def cpu_count() -> int:
    if hasattr(os, "sched_getaffinity"):
        return len(os.sched_getaffinity(0))
    return getattr(os, "process_cpu_count", os.cpu_count)() or 1


def proof_entry(name: str) -> Path:
    return Path("verification/verus") / name / "src/lib.rs"


def verify_proof(name: str, threads: int, root: Path = ROOT) -> dict[str, object]:
    entry = proof_entry(name)
    command = [
        "verus",
        "--crate-type=lib",
        "--num-threads",
        str(threads),
        str(entry),
    ]
    started = time.monotonic()
    try:
        result = subprocess.run(command, cwd=root, capture_output=True, text=True, timeout=120)
    except subprocess.TimeoutExpired as error:
        raise SystemExit(f"FAIL: verifier timed out for {name}") from error
    elapsed = time.monotonic() - started
    output = result.stdout + result.stderr
    summary = VERIFIED_RE.search(output)
    if result.returncode != 0 or summary is None:
        raise SystemExit(f"FAIL: Verus verification failed for {name}:\n{output}")
    verified = int(summary.group(1))
    print(f"PASS: {name}, {verified} verified ({elapsed:.2f}s)", flush=True)
    return {"proof": name, "entry": str(entry), "verified": verified, "seconds": elapsed}


def run_all(jobs: int = 2, threads: int | None = None, root: Path = ROOT) -> dict[str, object]:
    if jobs < 1 or (threads is not None and threads < 1):
        raise ValueError("jobs and threads must be positive")
    verifier_threads = threads if threads is not None else max(1, cpu_count() // jobs)
    started = time.monotonic()
    print(
        f"Verus: {len(PROOFS)} proofs, {jobs} workers x {verifier_threads} verifier threads",
        flush=True,
    )
    results: dict[str, dict[str, object]] = {}
    with ThreadPoolExecutor(max_workers=jobs) as executor:
        pending = {
            executor.submit(verify_proof, name, verifier_threads, root): name for name in PROOFS
        }
        try:
            for future in as_completed(pending):
                results[pending[future]] = future.result()
        except BaseException:
            for future in pending:
                future.cancel()
            raise
    elapsed = time.monotonic() - started
    print(f"PASS: {len(results)} proofs verified ({elapsed:.2f}s)", flush=True)
    return {
        "jobs": jobs,
        "threads": verifier_threads,
        "seconds": elapsed,
        "proofs": [results[name] for name in PROOFS],
    }


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--jobs", type=int, default=2, help="concurrent verifier processes (default: 2)"
    )
    parser.add_argument(
        "--threads", type=int, help="verifier threads per process (default: CPUs / jobs)"
    )
    parser.add_argument("--report", type=Path, help="write proof timings as JSON")
    parser.add_argument(
        "--list", action="store_true", help="list proof entry points without running"
    )
    args = parser.parse_args()
    if args.jobs < 1 or (args.threads is not None and args.threads < 1):
        parser.error("jobs and threads must be positive")
    if args.list:
        print(json.dumps([str(proof_entry(name)) for name in PROOFS], indent=2))
        return
    report = run_all(args.jobs, args.threads)
    if args.report:
        report.update(
            {
                "platform": platform.platform(),
                "verus_version": subprocess.check_output(
                    ["verus", "--version"], text=True, timeout=30
                ),
            }
        )
        args.report.write_text(json.dumps(report, indent=2) + "\n")


if __name__ == "__main__":
    main()

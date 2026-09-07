#!/usr/bin/env python3
"""Summarize the predeclared resident-index gates without changing thresholds."""
import argparse
import collections
import json
import math
import statistics
from pathlib import Path

BACKENDS = ["moka", "sharded8", "sharded16", "sharded32", "sharded64"]


def rows(path):
    return [json.loads(line) for line in path.read_text().splitlines() if line]


def normalize(name):
    if name == "Moka":
        return "moka"
    return "sharded" + name.split(": ")[1].split()[0]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directory", type=Path)
    args = parser.parse_args()
    criterion = rows(args.directory / "criterion.jsonl")
    supplemental = rows(args.directory / "supplemental.jsonl")
    assert len(criterion) == 480, len(criterion)
    assert len(supplemental) == 225, len(supplemental)
    timings = collections.defaultdict(list)
    probes = collections.defaultdict(list)
    extra = collections.defaultdict(list)
    for row in criterion:
        if row["kind"] == "criterion":
            timings[(row["backend"], row["benchmark"])].append(row["estimates"]["mean"]["point_estimate"])
        else:
            probes[(row["backend"], row["workload"], row["workers"], row["payload_bytes"])].append(row)
    for row in supplemental:
        assert not row["smoke"]
        extra[(normalize(row["backend"]), row["workload"], row["workers"])].append(row)
    assert all(len(values) == 3 for group in [timings, probes, extra] for values in group.values())
    median = statistics.median
    def metric(backend, case, field):
        values = extra[(backend, *case)]
        for part in field.split("."):
            values = [value[part] for value in values]
        return median(values)
    def ratio(backend, case, field):
        return metric(backend, case, field) / metric("moka", case, field)
    cases = sorted({(workload, workers) for _, workload, workers in extra if workload != "ControlledDebt"})
    ids = sorted({identifier for _, identifier in timings})
    reads = [identifier for identifier in ids if "/current/" in identifier and "cache_hit" in identifier]
    assert len(reads) == 6
    gates = {}
    for backend in BACKENDS:
        assert all(row["pending_nodes_after_drain"] == row["pending_weight_after_drain"] == 0
                   and row["resident_weight"] <= 512 for case in cases for row in extra[(backend, *case)])
    for backend in BACKENDS[1:]:
        failures = []
        slowdowns = [median(timings[(backend, identifier)]) / median(timings[("moka", identifier)]) for identifier in reads]
        if max(slowdowns) > 1.1:
            failures.append("lookup case slowdown exceeds 10%")
        geomean = math.prod(slowdowns) ** (1 / len(slowdowns))
        if geomean > 1.05:
            failures.append("lookup geometric mean slowdown exceeds 5%")
        mixed = ("Mixed", 32)
        if ratio(backend, mixed, "ops_per_second") < .9:
            failures.append("mixed32 throughput below 90%")
        if ratio(backend, mixed, "writer_latency.p99_ns") > 1.25:
            failures.append("mixed32 writer p99 above 125%")
        if any(ratio(backend, mixed, f"hit_latency.p{p}_ns") > 1.1 for p in [50, 95, 99]):
            failures.append("mixed32 median hit latency/tail above 110%")
        if ratio(backend, mixed, "hit_rate") < .99:
            failures.append("mixed32 hit rate below 99% of baseline")
        write_cases = [case for case in cases if case[0] in ["Eviction", "LiveEviction", "Invalidate", "Clear"]]
        if any(ratio(backend, case, "ops_per_second") < .9 for case in write_cases):
            failures.append("supplemental write/clear throughput below 90%")
        write_ids = [identifier for identifier in ids if identifier not in reads]
        if any(median(timings[("moka", identifier)]) / median(timings[(backend, identifier)]) < .9 for identifier in write_ids):
            failures.append("existing eviction/clear/reclamation throughput below 90%")
        debt_failures = []
        for case in cases:
            for field in ["peak_pending_nodes", "peak_pending_weight"]:
                baseline = max(row[field] for row in extra[("moka", *case)])
                candidate = max(row[field] for row in extra[(backend, *case)])
                if candidate > baseline:
                    debt_failures.append(f"{case[0]}{case[1]}:{field}={candidate}>{baseline}")
        for (base, workload, workers, payload), records in probes.items():
            if base != "moka":
                continue
            for field in ["peak_pending_nodes", "peak_pending_weight"]:
                baseline = max(row["cache_lifetime_stats_after_probes"][field] for row in records)
                candidate = max(row["cache_lifetime_stats_after_probes"][field] for row in probes[(backend, workload, workers, payload)])
                if candidate > baseline:
                    debt_failures.append(f"{workload}/{workers}T/{payload}B:{field}={candidate}>{baseline}")
        if debt_failures:
            failures.append("peak retirement debt exceeds baseline")
        assert all(row["pending_nodes_after_drain"] == row["pending_weight_after_drain"] == 0
                   and row["resident_weight"] <= 512 for case in cases for row in extra[(backend, *case)])
        gates[backend] = dict(qualifies=not failures, failures=failures,
            lookup_geomean_slowdown=geomean, worst_lookup_slowdown=max(slowdowns), debt_failures=debt_failures)
    (args.directory / "decision.json").write_text(json.dumps(gates, indent=2) + "\n")
    lines = ["## Measured results (2026-09-07)", "",
        "Host: Apple M1, 8 logical CPUs, 16 GiB RAM, aarch64-apple-darwin. The 32-thread cases oversubscribe this host. Three repetitions; each table reports the median unless labeled maximum. These short local windows support this host's decision, not a universal ranking.", "",
        "**Decision: " + ("retain Moka in production." if not any(gate["qualifies"] for gate in gates.values()) else "candidate qualifies; review the recorded gates before switching.") + "**", "",
        "| Shards | Worst lookup time / Moka | Geometric mean time / Moka | Qualifies |",
        "|---|---:|---:|---|"]
    for backend, gate in gates.items():
        lines.append(f"| {backend[7:]} | {gate['worst_lookup_slowdown']:.3f} | {gate['lookup_geomean_slowdown']:.3f} | {gate['qualifies']} |")
    lines += ["", "Failures by candidate:", ""]
    for backend, gate in gates.items():
        lines.append(f"- {backend}: " + "; ".join(gate["failures"]) + ".")
    lines += ["", "### Existing Criterion workloads", "",
        "Batch-time ratios below 1 favor the candidate. Moka time is the median of three mean estimates in microseconds. Reclamation IDs encode payload bytes and worker counts.", "",
        "| Case | Moka µs/batch | N8 / Moka | N16 / Moka | N32 / Moka | N64 / Moka |",
        "|---|---:|---:|---:|---:|---:|"]
    for identifier in ids:
        baseline = median(timings[("moka", identifier)])
        values = [median(timings[(backend, identifier)]) / baseline for backend in BACKENDS[1:]]
        lines.append(f"| {identifier.removeprefix('cache_lookup/').removeprefix('cache_reclamation/')} | {baseline/1000:.2f} | " + " | ".join(f"{value:.3f}" for value in values) + " |")
    lines += ["", "### Supplemental throughput", "",
        "Ratios above 1 favor the candidate. Each operation includes lease release or replacement; retained lease buffers have 32 slots per worker. Latency clocks and statistics observation are excluded from these timing runs.", "",
        "| Case | Moka Mops/s | N8 / Moka | N16 / Moka | N32 / Moka | N64 / Moka |",
        "|---|---:|---:|---:|---:|---:|"]
    for case in cases:
        lines.append(f"| {case[0]} {case[1]}T | {metric('moka', case, 'ops_per_second')/1e6:.3f} | " + " | ".join(f"{ratio(backend, case, 'ops_per_second'):.3f}" for backend in BACKENDS[1:]) + " |")
    lines += ["", "### Read-heavy 32T tails and writer contention", "",
        "One fresh write per 32 worker operations; other operations read key 0 and refill after misses. Hit rate counts misses before refill. Latencies are microseconds; p99 writer contention compares the 32T and 1T mixed probes. Three latency batches per repetition are separate from throughput.", "",
        "| Backend | Hit rate | Hit p50 / p95 / p99 µs | Writer p50 / p95 / p99 µs | Writer p99 32T / 1T |",
        "|---|---:|---|---|---:|"]
    for backend in BACKENDS:
        hits = " / ".join(f"{metric(backend, ('Mixed',32), f'hit_latency.p{p}_ns')/1000:.3f}" for p in [50,95,99])
        writes = " / ".join(f"{metric(backend, ('Mixed',32), f'writer_latency.p{p}_ns')/1000:.3f}" for p in [50,95,99])
        contention = metric(backend, ('Mixed',32), 'writer_latency.p99_ns') / metric(backend, ('Mixed',1), 'writer_latency.p99_ns')
        lines.append(f"| {backend} | {metric(backend, ('Mixed',32), 'hit_rate'):.4%} | {hits} | {writes} | {contention:.1f} |")
    lines += ["", "### Retirement debt and storage", "",
        "Maximum queued nodes across three repetitions, measured before final clear. Weights are eight times the node counts in this u64 matrix. Counts exclude resident nodes and nodes still pinned by live leases. All final pending-node/weight counts were zero after readers and leases exited and maintenance completed.", "",
        "| Case | Moka | N8 | N16 | N32 | N64 |", "|---|---:|---:|---:|---:|---:|"]
    for case in cases:
        lines.append(f"| {case[0]} {case[1]}T | " + " | ".join(str(max(row['peak_pending_nodes'] for row in extra[(backend,*case)])) for backend in BACKENDS) + " |")
    for backend in BACKENDS:
        controlled = [row['debt'] for row in extra[(backend,'ControlledDebt',1)]]
        assert all(row['pending_nodes_before_reader_release'] == 64 and row['pending_weight_before_reader_release'] == 512 and row['pending_nodes_after_drain'] == row['pending_weight_after_drain'] == 0 for row in controlled)
    lines += ["", "The controlled scoped-reference clear probe queued exactly 64 nodes / 512 weighted bytes for every backend, then drained to zero after releasing the reader.", "",
        "Storage estimates below use the mixed 32T cache after maintenance. Moka index storage is a lower bound excluding opaque policy/table metadata; sharded estimates include shard headers, hash-table capacity/control-byte estimates and flight-map capacity. Neither includes allocator rounding, heap storage in keys, or process RSS. Resident node bytes include caller weights and headers. Retained payload estimates overlap resident storage when a lease is still resident, so these columns must not be summed as distinct allocations.", "",
        "| Backend | Resident entries | Weight | Index estimate bytes | Resident node estimate bytes | Live-eviction 32T held payload upper bound |",
        "|---|---:|---:|---:|---:|---:|"]
    for backend in BACKENDS:
        values = [metric(backend, ('Mixed',32), field) for field in ['resident_entries','resident_weight','index_bytes_estimate','resident_node_bytes_estimate']]
        values.append(metric(backend, ('LiveEviction',32), 'held_payload_bytes_upper_bound'))
        lines.append(f"| {backend} | " + " | ".join(str(value) for value in values) + " |")
    lines += ["", "Raw `criterion.jsonl` retains all 300 timing estimates and 180 allocation/latency/reclamation probes (including 64 B and 64 KiB payloads at 1/8/32T). `supplemental.jsonl` retains all 225 observations with hit/write p50/p95/p99, resident counts, debt and estimates. `decision.json` retains every failed gate and debt comparison. Console logs remain in the run directory; raw data copied beside this report is sufficient to recompute these tables.", ""]
    (args.directory / "summary.md").write_text("\n".join(lines))
    print(json.dumps(gates, indent=2))


if __name__ == "__main__":
    main()

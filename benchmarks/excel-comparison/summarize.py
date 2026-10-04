"""Create paired CSV results, preserving failed/unpaired cases."""

import argparse
import csv
import json
from pathlib import Path


PRIMARY = {
    "S01": ("p50_s", "lower"), "S02": ("ns_per_call", "lower"),
    "S03": ("p50_s", "lower"), "S04": ("calls_per_s", "higher"),
    "M01": ("ns_per_element", "lower"), "M02": ("ns_per_element", "lower"),
    "M03": ("ns_per_element", "lower"), "M04": ("ns_per_element", "lower"),
    "M05": ("p50_s", "lower"),
    "T01": ("ns_per_char", "lower"), "T02": ("ns_per_char", "lower"),
    "T03": ("ns_per_char", "lower"),
    "P01": ("calls_per_s", "higher"), "P02": ("calls_per_s", "higher"),
    "P03": ("calls_per_s", "higher"), "P04": ("calls_per_s", "higher"),
    "A01": ("throughput_per_s", "higher"), "A02": ("cell_latency_p50_s", "lower"),
    "A03": ("throughput_per_s", "higher"), "A04": ("cell_latency_p99_s", "lower"),
    "A05": ("task_drain_after_release_s", "lower"),
    "A06": ("dirty_generations_per_s", "higher"),
    "R01": ("subscription_s", "lower"), "R02": ("update_settle_s", "lower"),
    "R03": ("update_cells_per_s", "higher"),
    "R04": ("observed_updates_per_s", "higher"),
    "R05": ("cell_latency_p99_s", "lower"), "R06": ("ops_per_s", "higher"),
    "R07": ("p95_s", "lower"),
    "C01": ("startup_to_ready_s", "lower"),
    "C02": ("registration_load_s", "lower"),
    "C03": ("first_call_s", "lower"),
    "C04": ("open_to_complete_s", "lower"),
    "W01": ("end_to_end_s", "lower"), "W02": ("end_to_end_s", "lower"),
    "W03": ("p50_s", "lower"),
    "L01": ("peak_rss_per_cell_bytes", "lower"),
    "L02": ("latency_drift_ratio", "lower"),
    "L03": ("p99_s", "lower"),
}


def summarize(records: list[dict]) -> list[dict]:
    grouped = {}
    for record in records:
        key = (record["id"], record["variant"], record["implementation"])
        grouped[key] = record
    pairs = []
    for id, variant in sorted({(record["id"], record["variant"]) for record in records}):
        xlfn = grouped.get((id, variant, "xlfn"))
        dna = grouped.get((id, variant, "excel_dna"))
        metric, direction = PRIMARY[id]
        row = {"id": id, "variant": variant, "metric": metric, "direction": direction,
               "xlfn_status": xlfn["status"] if xlfn else "missing",
               "excel_dna_status": dna["status"] if dna else "missing",
               "xlfn_value": None, "excel_dna_value": None, "xlfn_advantage_ratio": None,
               "comparable": False, "reason": "missing or failed run"}
        unsupported = [f"{record['implementation']}: {record.get('unsupported_reason', 'unsupported')}"
                       for record in (xlfn, dna) if record and record["status"] == "unsupported"]
        if unsupported:
            row["reason"] = "; ".join(unsupported)
        if xlfn and dna and xlfn["status"] == dna["status"] == "ok":
            # A pair is comparable only when the run settings and variants match.
            conditions = ("profile", "params", "threads", "rtd_throttle_ms",
                          "excel_version", "excel_build", "excel_bitness", "host", "os",
                          "artifact_source", "artifact_architecture", "ci_commit", "ci_run_id")
            conditions += ("runner_commit", "calculation_mode")
            if all(xlfn.get(key) == dna.get(key) for key in conditions):
                left = xlfn["metrics"].get(metric)
                right = dna["metrics"].get(metric)
                if isinstance(left, (float, int)) and isinstance(right, (float, int)) and left > 0 and right > 0:
                    row.update({"xlfn_value": left, "excel_dna_value": right,
                                "xlfn_advantage_ratio": right / left if direction == "lower" else left / right,
                                "comparable": True, "reason": ""})
                else:
                    row["reason"] = "primary metric missing or nonpositive"
            else:
                row["reason"] = "measurement settings or Excel build differ"
        if id in ("P01", "P02", "P03", "P04"):
            try:
                threads = int(variant.rsplit("-", 1)[-1])
                baseline_variant = f"{variant.rsplit('-', 1)[0]}-1" if id == "P03" else "1"
                for impl, record in (("xlfn", xlfn), ("excel_dna", dna)):
                    base = grouped.get((id, baseline_variant, impl))
                    if record and base and record["status"] == base["status"] == "ok":
                        now = record["metrics"]["calls_per_s"]
                        one = base["metrics"]["calls_per_s"]
                        row[f"{impl}_speedup"] = now / one
                        row[f"{impl}_parallel_efficiency"] = now / one / threads
            except (KeyError, ValueError, ZeroDivisionError):
                pass
        if id == "S03":
            for impl, record in (("xlfn", xlfn), ("excel_dna", dna)):
                base = grouped.get((id, "0us", impl))
                if record and base and record["status"] == base["status"] == "ok":
                    row[f"{impl}_baseline_fraction"] = base["metrics"]["p50_s"] / record["metrics"]["p50_s"]
        pairs.append(row)
    return pairs


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("input", type=Path)
    parser.add_argument("--csv", type=Path)
    args = parser.parse_args()
    records = [json.loads(line) for line in args.input.read_text(encoding="utf-8").splitlines() if line.strip()]
    rows = summarize(records)
    output = args.csv or args.input.with_suffix(".csv")
    columns = list(dict.fromkeys(key for row in rows for key in row))
    with output.open("w", encoding="utf-8-sig", newline="") as file:
        writer = csv.DictWriter(file, fieldnames=columns)
        writer.writeheader()
        writer.writerows(rows)
    print(f"{len(rows)} pairs -> {output}")


if __name__ == "__main__":
    main()

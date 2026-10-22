"""Export one shutdown row per run; never collapse repetitions or failed cases."""
import argparse
import csv
import json
from pathlib import Path


def elapsed(start, end):
    if not start or not end or start.get("qpc_frequency") != end.get("qpc_frequency"):
        return None
    return (end["qpc_ticks"] - start["qpc_ticks"]) / start["qpc_frequency"]


def summarize_record(record):
    timing = record.get("shutdown_timing", {})
    exit = record.get("process_exit_observation", {})
    trace = record.get("lifecycle_trace", {})
    events = trace.get("events", [])
    boundary = trace.get("boundary", "xlAutoClose" if record.get("implementation") == "xlfn" else "IExcelAddIn.AutoClose")
    entries = [e for e in events if e.get("event") == boundary + ".enter"]
    returns = [e for e in events if e.get("event") == boundary + ".return"]
    checkpoints = [e.get("details", {}).get("checkpoint") for e in events if e.get("event") == "shutdown.checkpoint"]
    row = {key: record.get(key) for key in ("id", "variant", "implementation", "timestamp_utc", "status", "execution_status", "runner_commit", "ci_commit", "xll_sha256")}
    row.update(observer_status=exit.get("status"), observer_error=exit.get("error"),
        measurement_error=exit.get("measurement_error"), termination=exit.get("termination"), exit_code=exit.get("exit_code"),
        quit_to_exit_s=exit.get("quit_to_exit_s"), quit_to_natural_exit_s=exit.get("quit_to_natural_exit_s"),
        quit_call_s=elapsed(timing.get("quit_begin"), timing.get("quit_call_finished")),
        references_release_s=elapsed(timing.get("references_release_begin"), timing.get("gc_finished")),
        application_release_s=elapsed(timing.get("bootstrap_reference_released"), timing.get("application_reference_released")),
        gc_s=elapsed(timing.get("application_reference_released"), timing.get("gc_finished")),
        trace_status=trace.get("status"), trace_error=trace.get("error"), boundary_scope=trace.get("boundary_scope"), auto_close=trace.get("auto_close"),
        auto_close_first_enter_since_quit_s=entries[0].get("since_quit_s") if entries else None,
        auto_close_last_return_since_quit_s=returns[-1].get("since_quit_s") if returns else None,
        auto_close_final_phase=returns[-1].get("details", {}).get("phase") if returns else None,
        last_shutdown_checkpoint=checkpoints[-1] if checkpoints else None,
        parent_exit_deadline_missed=exit.get("parent_exit_deadline_missed", False),
        dump_path=record.get("excel_process_cleanup", {}).get("shutdown_dump", {}).get("path"))
    # Missing outer return (e.g. nested close) must not claim a completed callback.
    if trace.get("auto_close") == "returned" and returns and exit.get("exit_filetime_ticks") is not None:
        returned_at = returns[-1].get("filetime_ticks")
        if returned_at is not None:
            row["last_auto_close_return_to_exit_s"] = (exit["exit_filetime_ticks"] - returned_at) / 10_000_000
    return row


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("inputs", type=Path, nargs="+")
    parser.add_argument("--csv", type=Path, required=True)
    args = parser.parse_args()
    rows = [summarize_record(json.loads(line)) for path in args.inputs
            for line in path.read_text(encoding="utf-8").splitlines() if line.strip()]
    if not rows: parser.error("no records")
    columns = list(dict.fromkeys(key for row in rows for key in row))
    with args.csv.open("w", encoding="utf-8-sig", newline="") as output:
        writer = csv.DictWriter(output, fieldnames=columns)
        writer.writeheader()
        writer.writerows(rows)


if __name__ == "__main__":
    main()

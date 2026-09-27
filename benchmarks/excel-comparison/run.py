"""Windows Excel COM runner for paired xlfn / Excel-DNA measurements.

Run each case in its own process and Excel instance. `--plan` needs no Excel.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import platform
import statistics
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path
from typing import Any

import artifact_manifest
from workloads import Case, cases

ROOT = Path(__file__).resolve().parent
XL_DONE = 0
XL_MANUAL = -4135
XL_AUTOMATIC = -4105


def checkout_commit() -> str | None:
    result = subprocess.run(["git", "rev-parse", "HEAD"], cwd=ROOT.parents[1],
                            capture_output=True, text=True, check=False)
    return result.stdout.strip() if result.returncode == 0 else None


def percentile(values: list[float], pct: float) -> float | None:
    if not values:
        return None
    ordered = sorted(values)
    pos = (len(ordered) - 1) * pct / 100
    lo = math.floor(pos)
    hi = math.ceil(pos)
    return ordered[lo] + (ordered[hi] - ordered[lo]) * (pos - lo)


def formula(id: str, row: int, params: dict[str, Any]) -> str:
    if id in ("S01", "P02", "W03", "L02", "C04"):
        return f"=BENCH.ID({row})"
    if id == "S02":
        argc = params["argc"]
        return f'=BENCH.SUM{argc}({",".join(str(row + i) for i in range(argc))})'
    if id in ("S03", "P01", "P03"):
        return f'=BENCH.CPU({row},{params["delay_us"]})'
    if id == "S04":
        return f'=BENCH.ERROR({row},{params["period"]})'
    if id == "P04":
        return f'=BENCH.CONTENDED({row})'
    if id.startswith("A") or id == "L03" and params.get("workload") == "async":
        return f'=BENCH.ASYNC({row},{params.get("delay_us", 10000)})'
    if id.startswith("R") or id == "L03" and params.get("workload") == "rtd":
        topic = (row - 1) % params.get("topics", params["cells"])
        return f'=BENCH.RTD("topic-{topic}",{params.get("period_ms", 0)})'
    if id in ("T01", "T03"):
        return f'=BENCH.STR.{"IN" if id == "T01" else "COPY"}(B{row})'
    if id == "T02":
        return f'=BENCH.STR.OUT({params["length"]},{"TRUE" if params["kind"] == "ja" else "FALSE"})'
    raise ValueError(id)


def flatten(value: Any) -> list[Any]:
    if isinstance(value, tuple):
        return [item for row in value for item in flatten(row)]
    return [value]


def interval(values: list[float], elapsed: float, unit: str) -> dict[str, Any]:
    return {
        "elapsed_s": elapsed,
        "observations": len(values),
        "p50_s": percentile(values, 50),
        "p95_s": percentile(values, 95),
        "p99_s": percentile(values, 99),
        "max_s": max(values) if values else None,
        "observation_unit": unit,
    }


class ExcelSession:
    def __init__(self, xll: Path, threads: int, throttle_ms: int, pid_file: Path):
        import psutil
        import win32com.client

        self.psutil = psutil
        self.started = time.perf_counter()
        self.app = win32com.client.DispatchEx("Excel.Application")
        self.app.Visible = False
        self.app.DisplayAlerts = False
        self.app.AskToUpdateLinks = False
        self.app.EnableEvents = False
        self.pid = int(self.app.Hwnd)
        import win32process

        _, self.pid = win32process.GetWindowThreadProcessId(self.pid)
        pid_file.write_text(str(self.pid), encoding="ascii")
        self.process = psutil.Process(self.pid)
        # Excel cannot switch calculation mode before its first workbook exists.
        # Keep this book open while benchmark books are opened and closed.
        self.bootstrap_book = self.app.Workbooks.Add()
        self.original_calculation = self.app.Calculation
        self.original_throttle = self.app.RTD.ThrottleInterval
        self.original_mtr_enabled = self.app.MultiThreadedCalculation.Enabled
        self.original_thread_mode = self.app.MultiThreadedCalculation.ThreadMode
        self.original_threads = self.app.MultiThreadedCalculation.ThreadCount
        self.app.Calculation = XL_MANUAL
        self.app.MultiThreadedCalculation.Enabled = True
        self.app.MultiThreadedCalculation.ThreadMode = 1  # xlThreadModeManual
        self.app.MultiThreadedCalculation.ThreadCount = threads
        self.app.RTD.ThrottleInterval = throttle_ms
        t = time.perf_counter()
        self.addin = self.app.AddIns.Add(str(xll.resolve()))
        self.addin.Installed = True
        self.load_s = time.perf_counter() - t
        self.ready_s = time.perf_counter() - self.started
        self.book = None
        self.peak_rss = self.process.memory_info().rss
        self.cpu_start = self.process.cpu_times()
        self._stop_sampler = threading.Event()
        self._sampler = threading.Thread(target=self._sample_memory, daemon=True)
        self._sampler.start()

    def _sample_memory(self):
        while not self._stop_sampler.wait(0.01):
            try:
                self.peak_rss = max(self.peak_rss, self.process.memory_info().rss)
            except self.psutil.Error:
                return

    def memory(self) -> int:
        rss = self.process.memory_info().rss
        self.peak_rss = max(self.peak_rss, rss)
        return rss

    def cpu_s(self) -> float:
        now = self.process.cpu_times()
        return now.user + now.system - self.cpu_start.user - self.cpu_start.system

    def new_book(self):
        self.book = self.app.Workbooks.Add()
        return self.book.Worksheets(1)

    def close_book(self):
        if self.book is not None:
            self.book.Close(SaveChanges=False)
            self.book = None

    def add_formulas(self, sheet: Any, count: int, make: Any, column: str = "A") -> Any:
        for first in range(1, count + 1, 5_000):
            last = min(count, first + 4_999)
            block = tuple((make(row),) for row in range(first, last + 1))
            sheet.Range(f"{column}{first}:{column}{last}").Formula = block
        return sheet.Range(f"{column}1:{column}{count}")

    def calculate(self, target: Any | None = None, full: bool = False) -> float:
        if target is not None:
            target.Dirty()
        start = time.perf_counter()
        if full:
            self.app.CalculateFull()
        else:
            self.app.Calculate()
        while self.app.CalculationState != XL_DONE:
            time.sleep(0.005)
        self.memory()
        return time.perf_counter() - start

    def wait_values(self, target: Any, count: int, expected: Any, timeout_s: float,
                    sample_every: int = 1, start_time: float | None = None) -> dict[str, Any]:
        start = start_time if start_time is not None else time.perf_counter()
        seen: dict[int, float] = {}
        indices = list(range(0, count, sample_every))
        if indices[-1] != count - 1:
            indices.append(count - 1)
        while time.perf_counter() - start < timeout_s:
            values = flatten(target.Value2)
            at = time.perf_counter() - start
            for i in indices:
                if i not in seen and expected(values[i], i):
                    seen[i] = at
            self.memory()
            if len(seen) == len(indices):
                return interval(list(seen.values()), at, "COM-observed cell") | {
                    "sampled_cells": len(indices), "settled_cells": len(seen), "poll_s": at,
                }
            time.sleep(0.01)
        raise TimeoutError(f"only {len(seen)}/{len(indices)} observed after {timeout_s}s")

    def close(self):
        self._stop_sampler.set()
        self._sampler.join(timeout=1)
        try:
            self.close_book()
            self.app.RTD.ThrottleInterval = self.original_throttle
            self.app.MultiThreadedCalculation.ThreadCount = self.original_threads
            self.app.MultiThreadedCalculation.ThreadMode = self.original_thread_mode
            self.app.MultiThreadedCalculation.Enabled = self.original_mtr_enabled
            self.app.Calculation = self.original_calculation
            self.addin.Installed = False
            self.bootstrap_book.Close(SaveChanges=False)
            self.app.Quit()
        except Exception:
            try:
                self.app.Quit()
            except Exception:
                pass


def check_scalar(target: Any, case: Case) -> None:
    values = flatten(target.Value2)
    expected = case.params.get("cells", 1)
    if len(values) != expected:
        raise AssertionError(f"expected {expected} cells, got {len(values)}")
    if case.id == "S04":
        period = case.params["period"]
        errors = sum(isinstance(value, int) and value < 0 for value in values)
        if errors != (expected // period if period else 0):
            raise AssertionError(
                f"error count {errors}, expected {expected // period if period else 0}; "
                f"first values: {values[:5]!r}"
            )
    elif case.id not in ("P04",):
        if not isinstance(values[0], (int, float)) or not isinstance(values[-1], (int, float)):
            raise AssertionError("scalar result is not numeric")


def run_scalar(session: ExcelSession, case: Case, repeat: int) -> dict[str, Any]:
    sheet = session.new_book()
    count = case.params["cells"]
    target = session.add_formulas(sheet, count, lambda row: formula(case.id, row, case.params))
    session.calculate(target)
    check_scalar(target, case)
    samples = []
    rss = []
    for _ in range(repeat):
        samples.append(session.calculate(target, full=case.id == "L02"))
        if case.id == "L02":
            rss.append(session.memory())
    check_scalar(target, case)
    result = interval(samples, sum(samples), "recalculation") | {"formula_count": count,
        "calls_per_s": count / statistics.median(samples), "ns_per_call": statistics.median(samples) * 1e9 / count}
    if case.id == "S03":
        result["nominal_cpu_s"] = count * case.params["delay_us"] / 1e6
        result["overhead_note"] = "Compare the 0-us baseline against total time; nominal busy-loop duration is not a calibrated CPU timer"
    if case.id == "L02":
        result["first_recalc_s"] = samples[0]
        result["last_recalc_s"] = samples[-1]
        result["latency_drift_ratio"] = samples[-1] / samples[0]
        result["rss_growth_bytes"] = rss[-1] - rss[0]
    return result


def run_matrix(session: ExcelSession, case: Case, repeat: int) -> dict[str, Any]:
    sheet = session.new_book()
    rows, cols = case.params["rows"], case.params["cols"]
    count = rows * cols
    input_area = sheet.Range(sheet.Cells(1, 2), sheet.Cells(rows, cols + 1))
    if case.id != "M02":
        for first in range(1, rows + 1, max(1, 5_000 // cols)):
            last = min(rows, first + max(1, 5_000 // cols) - 1)
            data = tuple(tuple(float((r - 1) * cols + c) for c in range(cols)) for r in range(first, last + 1))
            sheet.Range(sheet.Cells(first, 2), sheet.Cells(last, cols + 1)).Value2 = data
    if case.id == "M05":
        # A separate small repeated pattern makes every Excel cell type explicit.
        values = (1.0, "text", True, None, "=NA()")
        for first in range(1, rows + 1, max(1, 5_000 // cols)):
            last = min(rows, first + max(1, 5_000 // cols) - 1)
            block = tuple(tuple(values[((r - 1) * cols + c) % 5] for c in range(cols)) for r in range(first, last + 1))
            sheet.Range(sheet.Cells(first, 2), sheet.Cells(last, cols + 1)).Formula = block
    source = input_area.Address
    if case.id == "M01" or case.id == "M04":
        anchor = sheet.Range("A1")
        anchor.Formula = f"=BENCH.MAT.SUM({source})"
        verify = lambda: abs(float(anchor.Value2) - (count - 1) * count / 2) < 1
    elif case.id == "M05":
        anchor = sheet.Range("A1")
        anchor.Formula = f"=BENCH.MAT.MIXED({source})"
        expected = 15 * (count // 5) + sum((1, 2, 3, 4, 5)[:count % 5])
        verify = lambda: anchor.Value2 == expected
    else:
        anchor = sheet.Cells(1, cols + 3)
        if case.id == "M02":
            anchor.Formula2 = f"=BENCH.MAT.MAKE({rows},{cols},0)"
        else:
            anchor.Formula2 = f"=BENCH.MAT.COPY({source})"
        last = sheet.Cells(rows, 2 * cols + 2)
        verify = lambda: float(last.Value2) == count - 1
    session.calculate(anchor)
    if not verify():
        raise AssertionError(f"matrix result check failed: {case.key}")
    allocations_before = session.app.Evaluate("BENCH.ALLOC.BYTES()") if case.id == "M05" else None
    samples = [session.calculate(anchor) for _ in range(repeat)]
    if not verify():
        raise AssertionError("matrix result changed")
    result = interval(samples, sum(samples), "recalculation") | {
        "elements": count, "ns_per_element": statistics.median(samples) * 1e9 / count,
    }
    if case.id == "M05":
        result["fixture_allocated_bytes"] = session.app.Evaluate("BENCH.ALLOC.BYTES()") - allocations_before
        result["fixture_allocated_bytes_per_element_per_recalc"] = result["fixture_allocated_bytes"] / count / repeat
        result["allocation_note"] = "Rust process allocator and .NET managed allocation counters have different scope; do not compare ratio"
    return result


def run_text(session: ExcelSession, case: Case, repeat: int) -> dict[str, Any]:
    sheet = session.new_book()
    count = case.params["cells"]
    value = ("日" if case.params["kind"] == "ja" else "a") * case.params["length"]
    if case.id != "T02":
        sheet.Range(f"B1:B{count}").Value2 = tuple((value,) for _ in range(count))
    target = session.add_formulas(sheet, count, lambda row: formula(case.id, row, case.params))
    session.calculate(target)
    actual = flatten(target.Value2)
    if actual[0] != (case.params["length"] if case.id == "T01" else value):
        raise AssertionError("text result mismatch")
    samples = [session.calculate(target) for _ in range(repeat)]
    return interval(samples, sum(samples), "recalculation") | {
        "characters": count * case.params["length"],
        "ns_per_char": statistics.median(samples) * 1e9 / (count * case.params["length"]),
    }


def run_async(session: ExcelSession, case: Case, repeat: int) -> dict[str, Any]:
    sheet = session.new_book()
    count = case.params["cells"]
    target = session.add_formulas(sheet, count, lambda row: formula(case.id, row, case.params))
    timeout = min(case.timeout_s, max(30, count * case.params.get("delay_us", 10_000) / 1e6 / 16))
    all_results = []
    submissions = []
    for rep in range(repeat):
        # Different arguments force a new async invocation even if Excel caches.
        offset = rep * count + 1
        submit_start = time.perf_counter()
        target = session.add_formulas(sheet, count,
            lambda row: f'=BENCH.ASYNC({offset + row},{case.params.get("delay_us", 10000)})')
        submissions.append(time.perf_counter() - submit_start)
        start = time.perf_counter()
        session.app.Calculate()
        observed = session.wait_values(target, count,
            lambda value, i: value == float(offset + i + 1), timeout,
            sample_every=max(1, count // 2_000), start_time=start)
        observed["settle_from_submit_s"] = time.perf_counter() - start
        all_results.append(observed)
    settles = [item["settle_from_submit_s"] for item in all_results]
    return interval(settles, sum(settles), "batch") | {
        "formula_count": count, "throughput_per_s": count / statistics.median(settles),
        "submission_s": statistics.median(submissions),
        "submission_cells_per_s": count / statistics.median(submissions),
        "cell_latency_p50_s": statistics.median([item["p50_s"] for item in all_results]),
        "cell_latency_p95_s": statistics.median([item["p95_s"] for item in all_results]),
        "cell_latency_p99_s": statistics.median([item["p99_s"] for item in all_results]),
        "sampled_cells": all_results[0]["sampled_cells"],
        "latency_note": "COM polling gives upper-bound arrival observations at ~10 ms resolution",
    }


def run_async_gate(session: ExcelSession, case: Case) -> dict[str, Any]:
    sheet = session.new_book()
    n = case.params["cells"]
    submit_start = time.perf_counter()
    target = session.add_formulas(sheet, n, lambda row: f"=BENCH.ASYNC({row},-1)")
    submission_s = time.perf_counter() - submit_start
    ready_start = time.perf_counter()
    session.app.Calculate()
    while time.perf_counter() - ready_start < 120:
        active = int(session.app.Evaluate("BENCH.ASYNC.ACTIVE()"))
        if active == n:
            break
        time.sleep(0.01)
    if active != n:
        raise TimeoutError(f"only {active}/{n} async calls were active before release")
    ready_s = time.perf_counter() - ready_start
    release_start = time.perf_counter()
    gate = sheet.Range("ZZ1")
    gate.Formula = "=BENCH.ASYNC.RELEASE(1)"
    gate.Calculate()
    if gate.Value2 != 1:
        raise AssertionError("async release failed")
    observed = session.wait_values(target, n, lambda value, i: value == float(i + 1),
                                   120, sample_every=max(1, n // 2_000), start_time=release_start)
    total_s = submission_s + ready_s + observed["poll_s"]
    return {"formula_count": n, "active_before_release": active,
            "submission_s": submission_s, "time_until_all_active_s": ready_s,
            "release_to_settle_s": observed["poll_s"],
            "total_to_settle_s": total_s,
            "throughput_per_s": n / (total_s if case.id == "A03" else observed["poll_s"]),
            "cell_latency_p50_s": observed["p50_s"],
            "cell_latency_p95_s": observed["p95_s"],
            "cell_latency_p99_s": observed["p99_s"],
            "sampled_cells": observed["sampled_cells"],
            "latency_note": "COM-observed cell arrival after gate release, ~10 ms polling granularity"}


def run_async_cancel(session: ExcelSession, case: Case) -> dict[str, Any]:
    sheet = session.new_book()
    n = case.params["cells"]
    target = session.add_formulas(sheet, n,
        lambda row: f'=BENCH.ASYNC({row},{case.params["delay_us"]})')
    session.app.Calculate()
    active_before = int(session.app.Evaluate("BENCH.ASYNC.ACTIVE()"))
    if active_before == 0:
        raise AssertionError("no pending async work at cancellation; increase delay")
    begin = time.perf_counter()
    rss_before = session.memory()
    mode = case.params["mode"]
    if mode == "recalc":
        target = session.add_formulas(sheet, n,
            lambda row: f'=BENCH.ASYNC({n + row},{case.params["delay_us"]})')
        session.app.Calculate()
    elif mode == "clear":
        target.ClearContents()
    else:
        session.close_book()
    action_s = time.perf_counter() - begin
    if mode == "close":
        session.new_book()
    # A successful cancellation in one implementation may still leave the
    # underlying task running. Report active task cleanup separately.
    while time.perf_counter() - begin < 30:
        active = int(session.app.Evaluate("BENCH.ASYNC.ACTIVE()"))
        if active == 0:
            break
        time.sleep(0.01)
    cleanup_s = time.perf_counter() - begin
    if active != 0:
        raise TimeoutError(f"{active} async tasks still active 30 s after cancellation")
    stale = None
    if mode == "recalc":
        observed = session.wait_values(target, n, lambda value, i: value == float(n + i + 1),
                                       30, sample_every=max(1, n // 2_000))
        stale = sum(value == float(i + 1) for i, value in enumerate(flatten(target.Value2)))
    elif mode == "clear":
        stale = sum(value is not None for value in flatten(target.Value2))
    return {"formula_count": n, "pending_before": active_before,
            "action_s": action_s, "task_cleanup_observed_s": cleanup_s,
            "active_after_observation": active, "stale_cells_after_observation": stale,
            "rss_before_action_bytes": rss_before, "rss_after_observation_bytes": session.memory(),
            "settle_s": observed["poll_s"] if mode == "recalc" else None,
            "cancellation_note": "Task cleanup and Excel callback invalidation are distinct; close mode has no workbook cells to inspect"}


def run_async_repeated(session: ExcelSession, case: Case) -> dict[str, Any]:
    sheet = session.new_book()
    n = case.params["cells"]
    start = time.perf_counter()
    for generation in range(case.params["repetitions"]):
        target = session.add_formulas(sheet, n,
            lambda row: f'=BENCH.ASYNC({generation * n + row},{case.params["delay_us"]})')
        target.Dirty()
        session.app.Calculate()
    target = sheet.Range(f"A1:A{n}")
    expected_start = (case.params["repetitions"] - 1) * n
    observed = session.wait_values(target, n,
        lambda value, i: value == float(expected_start + i + 1), 60,
        sample_every=max(1, n // 2_000))
    values = flatten(target.Value2)
    stale = sum(value != float(expected_start + i + 1) for i, value in enumerate(values))
    elapsed = time.perf_counter() - start
    return {"recalculations": case.params["repetitions"], "formula_count": n,
            "elapsed_s": elapsed, "dirty_generations_per_s": case.params["repetitions"] / elapsed,
            "stale_cells_after_settle": stale, "sampled_cells": observed["sampled_cells"]}


def rtd_count(session: ExcelSession) -> int:
    return int(session.app.Evaluate("BENCH.RTD.COUNT()"))


def wait_count(session: ExcelSession, expected: int, timeout_s: float = 120) -> float:
    start = time.perf_counter()
    while time.perf_counter() - start < timeout_s:
        count = rtd_count(session)
        if count == expected:
            return time.perf_counter() - start
        time.sleep(0.02)
    raise TimeoutError(f"RTD topics {count}, expected {expected}")


def pulse(session: ExcelSession, sequence: int) -> None:
    # Excel serializes this main-thread UDF after formula entry.
    cell = session.book.Worksheets(1).Range("ZZ1")
    cell.Formula = f"=BENCH.RTD.PULSE({sequence})"
    cell.Calculate()
    if cell.Value2 != sequence:
        raise AssertionError("RTD pulse was not accepted")


def run_rtd(session: ExcelSession, case: Case, repeat: int) -> dict[str, Any]:
    sheet = session.new_book()
    n, topics = case.params["cells"], case.params["topics"]
    rss_before = session.memory()
    start = time.perf_counter()
    target = session.add_formulas(sheet, n, lambda row: formula(case.id, row, case.params))
    session.calculate()
    subscribed_s = wait_count(session, topics, min(case.timeout_s, 180))
    subscription_s = time.perf_counter() - start
    samples = []
    if case.id == "R04":
        duration = case.params["duration_s"]
        emitted_before = float(session.app.Evaluate("BENCH.RTD.EMITTED()"))
        begin = time.perf_counter()
        observed = set()
        while time.perf_counter() - begin < duration:
            value = target.Cells(1, 1).Value2
            if isinstance(value, (float, int)) and value >= 0:
                observed.add(value)
            session.memory()
            time.sleep(0.005)
        emitted_after = float(session.app.Evaluate("BENCH.RTD.EMITTED()"))
        if emitted_after <= emitted_before:
            raise AssertionError("RTD source emitted no periodic updates")
        return {"subscription_s": subscription_s, "observed_updates_per_s": len(observed) / duration,
            "source_emissions_per_s": (emitted_after - emitted_before) / duration,
            "source_emissions_per_topic_s": (emitted_after - emitted_before) / duration / topics,
            "requested_updates_per_topic_s": case.params["requested_hz"],
            "observed_distinct_values": len(observed), "duration_s": duration,
            "formula_count": n, "topic_count": topics,
            "delivery_note": "Cell polling is a lower bound; Excel RTD throttle may coalesce updates"}
    if case.id == "R06":
        for _ in range(case.params["repetitions"]):
            begin = time.perf_counter()
            target.ClearContents()
            session.calculate()
            wait_count(session, 0)
            target = session.add_formulas(sheet, n, lambda row: formula(case.id, row, case.params))
            session.calculate()
            wait_count(session, topics)
            samples.append(time.perf_counter() - begin)
        return interval(samples, sum(samples), "unsubscribe/resubscribe cycle") | {
            "ops_per_s": 2 * n / statistics.median(samples), "topic_count": topics,
            "rss_after_bytes": session.memory(),
        }
    if case.id == "R07":
        begin = time.perf_counter()
        rss = []
        latencies = []
        last = None
        last_change = begin
        while time.perf_counter() - begin < case.params["duration_s"]:
            value = target.Cells(1, 1).Value2
            now = time.perf_counter()
            if value != last:
                if last is not None:
                    latencies.append(now - last_change)
                last, last_change = value, now
            rss.append((now - begin, session.memory()))
            time.sleep(0.02)
        if len(latencies) < 2:
            raise AssertionError("too few RTD updates for latency drift")
        return interval(latencies, case.params["duration_s"], "observed update interval") | {
            "rss_start_bytes": rss[0][1], "rss_end_bytes": rss[-1][1],
            "rss_growth_bytes": rss[-1][1] - rss[0][1], "formula_count": n,
            "first_half_p95_s": percentile(latencies[:len(latencies)//2], 95),
            "last_half_p95_s": percentile(latencies[len(latencies)//2:], 95),
        }
    if case.id in ("R01", "R02", "R03"):
        result = {"subscription_s": subscription_s, "subscription_count_s": subscribed_s,
            "formula_count": n, "topic_count": topics,
            "subscription_cells_per_s": n / subscription_s, "rss_bytes": session.memory(),
            "incremental_rss_bytes": session.memory() - rss_before,
            "incremental_rss_per_topic_bytes": (session.memory() - rss_before) / topics}
        if case.id in ("R02", "R03"):
            begin = time.perf_counter()
            pulse(session, 1_000_000)
            observed = session.wait_values(target, n, lambda value, i: value == 1_000_000.0,
                120, sample_every=max(1, n // 2_000), start_time=begin)
            result["update_settle_s"] = time.perf_counter() - begin
            result["update_p95_s"] = observed["p95_s"]
            result["update_cells_per_s"] = n / result["update_settle_s"]
        return result
    for iteration in range(repeat):
        seq = 1_000_000 + iteration
        begin = time.perf_counter()
        pulse(session, seq)
        observed = session.wait_values(target, n, lambda value, i: value == float(seq),
            min(case.timeout_s, 120), sample_every=max(1, n // 2_000), start_time=begin)
        samples.append(time.perf_counter() - begin)
    return interval(samples, sum(samples), "pulse batch") | {
        "formula_count": n, "topic_count": topics,
        "sampled_cells": observed["sampled_cells"],
        "cell_latency_p50_s": observed["p50_s"],
        "cell_latency_p95_s": observed["p95_s"],
        "cell_latency_p99_s": observed["p99_s"],
    }


def run_cold(session: ExcelSession, case: Case, repeat: int) -> dict[str, Any]:
    if case.id == "C01":
        sheet = session.new_book()
        sheet.Range("A1").Formula = "=BENCH.ID(1)"
        session.calculate()
        if sheet.Range("A1").Value2 != 1:
            raise AssertionError("add-in ready check failed")
        return {"startup_to_ready_s": session.ready_s,
            "startup_to_verified_first_result_s": time.perf_counter() - session.started,
            "load_s": session.load_s}
    if case.id == "C02":
        last = case.params["registrations"] - 1
        session.new_book()
        if session.app.Evaluate(f"BENCH.EXTRA.{last:04}(42)") != 42:
            raise AssertionError("last generated registration is unavailable")
        return {"registration_load_s": session.load_s,
            "requested_extra_functions": case.params["registrations"],
            "baseline_functions_included": True}
    if case.id == "C03":
        sheet = session.new_book()
        sheet.Range("A1").Formula = "=BENCH.ID(1)"
        begin = time.perf_counter()
        sheet.Range("A1").Calculate()
        elapsed = time.perf_counter() - begin
        if sheet.Range("A1").Value2 != 1:
            raise AssertionError("first call failed")
        return {"first_call_s": elapsed}
    sheet = session.new_book()
    target = session.add_formulas(sheet, case.params["cells"],
        lambda row: formula("C04", row, case.params))
    session.calculate(target)
    with tempfile.TemporaryDirectory() as tmp:
        path = str(Path(tmp) / "warm.xlsx")
        session.book.SaveAs(path, FileFormat=51)
        session.close_book()
        begin = time.perf_counter()
        session.book = session.app.Workbooks.Open(path, UpdateLinks=0, ReadOnly=True)
        session.calculate(full=True)
        elapsed = time.perf_counter() - begin
        if session.book.Worksheets(1).Range(f"A{case.params['cells']}").Value2 != case.params["cells"]:
            raise AssertionError("warm workbook did not calculate")
        return {"open_to_complete_s": elapsed, "formula_count": case.params["cells"]}


def run_mixed(session: ExcelSession, case: Case, repeat: int) -> dict[str, Any]:
    if case.id == "W03":
        return run_scalar(session, case, repeat)
    sheet = session.new_book()
    n = case.params["cells"]
    scalar = session.add_formulas(sheet, n, lambda row: f"=BENCH.ID({row})")
    auxiliary = max(1, n // 10)
    session.add_formulas(sheet, auxiliary, lambda row: f'=BENCH.ASYNC({row},10000)', "C")
    rtd_target = session.add_formulas(sheet, auxiliary, lambda row: f'=BENCH.RTD("market-{row % 10}",0)', "D")
    sheet.Range("F1").Formula2 = "=BENCH.MAT.MAKE(100,10,0)"
    if case.id == "W02":
        session.add_formulas(sheet, n,
            lambda row: f"=BENCH.SHARED(D{1 + ((row - 1) % auxiliary)})", "E")
    begin = time.perf_counter()
    session.calculate()
    session.wait_values(sheet.Range(f"C1:C{auxiliary}"), auxiliary,
        lambda value, i: value == float(i + 1), 120, sample_every=max(1, n // 2_000))
    wait_count(session, 10)
    pulse(session, 1_000_000)
    session.wait_values(rtd_target, auxiliary, lambda value, i: value == 1_000_000.0,
                        120, sample_every=max(1, auxiliary // 2_000))
    if case.id == "W02":
        session.calculate()
        if sheet.Range(f"E{n}").Value2 != 1_000_000:
            raise AssertionError("shared market-data dependency failed")
    elapsed = time.perf_counter() - begin
    if scalar.Cells(n, 1).Value2 != n:
        raise AssertionError("mixed scalar result failed")
    return {"end_to_end_s": elapsed, "formula_count": n + 2 * auxiliary + 1 + (n if case.id == "W02" else 0),
        "cpu_s": session.cpu_s(), "rss_bytes": session.memory()}


def run_memory(session: ExcelSession, case: Case, repeat: int) -> dict[str, Any]:
    kind = case.params["workload"]
    before = session.memory()
    if kind == "scalar":
        result = run_scalar(session, Case("S01", case.variant, case.params), repeat)
    elif kind == "matrix":
        side = math.isqrt(case.params["cells"])
        result = run_matrix(session, Case("M03", case.variant, {"rows": side, "cols": side}), repeat)
    elif kind == "async":
        result = run_async(session, Case("A03", case.variant, case.params | {"delay_us": 10_000}), repeat)
    else:
        result = run_rtd(session, Case("R01", case.variant, case.params | {"topics": case.params["cells"]}), repeat)
    return result | {"rss_before_bytes": before, "peak_rss_bytes": session.peak_rss,
                     "peak_rss_per_cell_bytes": (session.peak_rss - before) / case.params["cells"]}


def run_tail(session: ExcelSession, case: Case, repeat: int) -> dict[str, Any]:
    kind = case.params["workload"]
    if kind == "scalar":
        return run_scalar(session, Case("S01", case.variant, case.params), case.params["repetitions"])
    if kind == "async":
        return run_async(session, Case("A01", case.variant,
            case.params | {"delay_us": 0}), case.params["repetitions"])
    return run_rtd(session, Case("R05", case.variant,
        case.params | {"topics": case.params["cells"]}), case.params["repetitions"])


def execute(session: ExcelSession, case: Case, repeat: int) -> dict[str, Any]:
    if case.id in ("S01", "S02", "S03", "S04", "P01", "P02", "P03", "P04", "W03", "L02"):
        return run_scalar(session, case, case.params.get("repetitions", repeat))
    if case.id.startswith("M"):
        return run_matrix(session, case, repeat)
    if case.id.startswith("T"):
        return run_text(session, case, repeat)
    if case.id == "A05":
        return run_async_cancel(session, case)
    if case.id == "A06":
        return run_async_repeated(session, case)
    if case.id in ("A03", "A04"):
        return run_async_gate(session, case)
    if case.id.startswith("A"):
        return run_async(session, case, case.params.get("repetitions", repeat))
    if case.id.startswith("R"):
        return run_rtd(session, case, repeat)
    if case.id.startswith("C"):
        return run_cold(session, case, repeat)
    if case.id.startswith("W"):
        return run_mixed(session, case, repeat)
    if case.id == "L01":
        return run_memory(session, case, repeat)
    if case.id == "L03":
        return run_tail(session, case, repeat)
    raise ValueError(case.key)


def artifact(root: Path, implementation: str, case: Case) -> Path:
    filename = f"registration-{case.params['registrations']}.xll" if case.id == "C02" else "benchmark.xll"
    return root / implementation / filename


def worker(args: argparse.Namespace, case: Case) -> int:
    artifact_root = Path(args.artifacts)
    manifest_path = artifact_root / "manifest.json"
    manifest = json.loads(manifest_path.read_text(encoding="utf-8")) if manifest_path.is_file() else None
    xll = artifact(artifact_root, args.implementation, case)
    record: dict[str, Any] = {
        "id": case.id, "variant": case.variant, "implementation": args.implementation,
        "params": case.params, "profile": args.profile, "repeat": args.repeat,
        "timestamp_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "host": platform.node(), "os": platform.platform(),
        "xll": str(xll.resolve()), "xll_sha256": hashlib.sha256(xll.read_bytes()).hexdigest(),
        "threads": case.params.get("threads", args.threads), "rtd_throttle_ms": args.throttle_ms,
        "artifact_source": manifest["source"] if manifest else "local-unverified",
        "artifact_architecture": manifest["architecture"] if manifest else "unknown",
        "ci_commit": manifest["commit"] if manifest else None,
        "ci_run_id": manifest["run_id"] if manifest else None,
        "ci_run_attempt": manifest["run_attempt"] if manifest else None,
        "runner_commit": checkout_commit(),
    }
    session = None
    try:
        if manifest is not None:
            relative = f"{args.implementation}/{xll.name}"
            if record["xll_sha256"] != manifest["files"][relative]["sha256"]:
                raise ValueError(f"CI XLL changed after verification: {relative}")
        session = ExcelSession(xll, record["threads"], args.throttle_ms, Path(args.pid_file))
        record["excel_version"] = str(session.app.Version)
        record["excel_build"] = str(session.app.Build)
        record["excel_bitness"] = session.app.OperatingSystem
        record["metrics"] = execute(session, case, args.repeat)
        record["metrics"]["peak_excel_rss_bytes"] = session.peak_rss
        record["metrics"]["excel_cpu_s"] = session.cpu_s()
        record["status"] = "ok"
    except Exception as error:
        record["status"] = "error"
        record["error"] = f"{type(error).__name__}: {error}"
    finally:
        if session is not None:
            session.close()
    print(json.dumps(record, ensure_ascii=False), flush=True)
    return 0 if record["status"] == "ok" else 1


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--profile", choices=("full", "smoke"), default="full")
    parser.add_argument("--artifacts", default=str(ROOT / "artifacts"))
    parser.add_argument("--allow-local-artifacts", action="store_true",
                        help="development only: run locally built XLLs without a CI manifest")
    parser.add_argument("--allow-commit-mismatch", action="store_true",
                        help="run a CI XLL from a different source commit; record both revisions")
    parser.add_argument("--out", default=str(ROOT / "results.jsonl"))
    parser.add_argument("--append", action="store_true", help="append to an existing JSONL result file")
    parser.add_argument("--implementation", choices=("xlfn", "excel_dna"))
    parser.add_argument("--id", action="append", help="Benchmark ID; may be repeated")
    parser.add_argument("--variant", help="Exact variant after ID selection")
    parser.add_argument("--repeat", type=int, default=5)
    parser.add_argument("--threads", type=int, default=1)
    parser.add_argument("--throttle-ms", type=int, default=100)
    parser.add_argument("--plan", action="store_true")
    parser.add_argument("--worker", action="store_true", help=argparse.SUPPRESS)
    parser.add_argument("--case-key", help=argparse.SUPPRESS)
    parser.add_argument("--pid-file", help=argparse.SUPPRESS)
    args = parser.parse_args()
    selected = [case for case in cases(args.profile)
                if (not args.id or case.id in args.id) and (not args.variant or case.variant == args.variant)]
    if not selected:
        parser.error("no cases matched")
    if args.plan:
        for case in selected:
            print(json.dumps({"key": case.key, "params": case.params}, ensure_ascii=False))
        return 0
    if sys.platform != "win32":
        parser.error("Excel execution requires Windows; --plan works on this machine")
    if args.worker:
        case = next((case for case in selected if case.key == args.case_key), None)
        if case is None or args.implementation is None or args.pid_file is None:
            parser.error("invalid worker arguments")
        return worker(args, case)
    if args.repeat < 1 or args.threads < 1 or args.throttle_ms < 1:
        parser.error("repeat, threads, and throttle-ms must be positive")
    artifact_root = Path(args.artifacts)
    if (artifact_root / "manifest.json").is_file():
        try:
            manifest = artifact_manifest.verify(artifact_root)
        except (OSError, ValueError, KeyError, TypeError) as error:
            parser.error(f"CI artifact verification failed: {error}")
        runner_commit = checkout_commit()
        if not args.allow_commit_mismatch and runner_commit != manifest["commit"]:
            parser.error(f"checkout commit {runner_commit} differs from CI artifact commit {manifest['commit']}")
    elif not args.allow_local_artifacts:
        parser.error("CI artifact manifest is missing; download a CI run with fetch-ci.ps1")
    out = Path(args.out)
    if out.exists() and out.stat().st_size > 0 and not args.append:
        parser.error(f"{out} already contains results; choose a new --out or pass --append")
    out.parent.mkdir(parents=True, exist_ok=True)
    errors = 0
    implementations = [args.implementation] if args.implementation else ["xlfn", "excel_dna"]
    for case in selected:
        for implementation in implementations:
            xll = artifact(Path(args.artifacts), implementation, case)
            if not xll.is_file():
                parser.error(f"missing {xll}; build artifacts first")
            with tempfile.TemporaryDirectory() as tmp:
                pid_file = Path(tmp) / "excel.pid"
                cmd = [sys.executable, str(Path(__file__).resolve()), "--worker",
                       "--profile", args.profile, "--artifacts", args.artifacts,
                       "--implementation", implementation, "--case-key", case.key,
                       "--repeat", str(args.repeat), "--threads", str(args.threads),
                       "--throttle-ms", str(args.throttle_ms), "--pid-file", str(pid_file)]
                if args.allow_local_artifacts:
                    cmd.append("--allow-local-artifacts")
                if args.allow_commit_mismatch:
                    cmd.append("--allow-commit-mismatch")
                try:
                    child = subprocess.run(cmd, capture_output=True, text=True,
                                           timeout=case.timeout_s + 60)
                    rows = [line for line in child.stdout.splitlines() if line.startswith("{")]
                    if not rows:
                        raise RuntimeError(child.stderr or "worker returned no result")
                    record = json.loads(rows[-1])
                except (subprocess.TimeoutExpired, RuntimeError) as error:
                    if pid_file.exists():
                        import psutil
                        try:
                            psutil.Process(int(pid_file.read_text())).kill()
                        except psutil.Error:
                            pass
                    record = {"id": case.id, "variant": case.variant,
                              "implementation": implementation, "status": "error",
                              "error": f"worker timeout/failure: {error}"}
                with out.open("a", encoding="utf-8") as file:
                    file.write(json.dumps(record, ensure_ascii=False) + "\n")
                if record["status"] != "ok" and pid_file.exists():
                    import psutil
                    try:
                        process = psutil.Process(int(pid_file.read_text()))
                        if process.is_running():
                            process.kill()
                    except psutil.Error:
                        pass
                print(f"{case.key} {implementation}: {record['status']}", flush=True)
                errors += record["status"] != "ok"
    return 1 if errors else 0


if __name__ == "__main__":
    raise SystemExit(main())

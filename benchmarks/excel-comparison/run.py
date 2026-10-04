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
from startup_diagnostics import read_startup_log_delta, snapshot_startup_log
from workloads import Case, cases

ROOT = Path(__file__).resolve().parent
XL_DONE = 0
XL_MANUAL = -4135
XL_AUTOMATIC = -4105
XL_ERR_NUM = -2146826252  # COM Value2 representation of #NUM! (xlErrNum = 2036).
CELL_ERRORS = {
    -2146826288: "#NULL!", -2146826281: "#DIV/0!", -2146826273: "#VALUE!",
    -2146826265: "#REF!", -2146826259: "#NAME?", XL_ERR_NUM: "#NUM!",
    -2146826246: "#N/A", -2146826243: "#SPILL!",
}
COM_BUSY_HRESULTS = {0x80010001, 0x8001010A}  # rejected / retry later, not executed


class WorkerProgress:
    """Keep the current record available even if a COM call or teardown hangs."""
    def __init__(self, path: Path, record: dict[str, Any]):
        self.path = path
        self.record = record
        self.started = time.perf_counter()
        self.lock = threading.RLock()

    def save(self) -> None:
        with self.lock:
            temporary = self.path.with_suffix(".tmp")
            temporary.write_text(json.dumps(self.record, ensure_ascii=False), encoding="utf-8")
            temporary.replace(self.path)

    def stage(self, name: str, **details: Any) -> None:
        with self.lock:
            self.record["stage"] = name
            self.record["stage_details"] = details
            self.record.setdefault("stage_history", []).append(
                {"stage": name, "elapsed_s": time.perf_counter() - self.started, **details})
            self.save()

    def update(self, **fields: Any) -> None:
        # Callers pass complete snapshots, never live mutable counter maps.
        with self.lock:
            self.record.update(fields)
            self.save()


class ComRetry:
    """Retry only calls Excel explicitly rejected before executing them."""
    def __init__(self, timeout_s: float = 10, progress: WorkerProgress | None = None):
        self.timeout_s = timeout_s
        self.progress = progress
        self.stats: dict[str, Any] = {"rejected_calls": 0, "retry_wait_s": 0.0,
                                      "retry_timeout_s": timeout_s}

    def call(self, operation: str, function: Any, *args: Any, **kwargs: Any) -> Any:
        started = None
        while True:
            try:
                return function(*args, **kwargs)
            except Exception as error:
                hresult = getattr(error, "hresult", None)
                if not isinstance(hresult, int) or hresult & 0xFFFFFFFF not in COM_BUSY_HRESULTS:
                    raise
                now = time.perf_counter()
                if started is None:
                    started = now
                self.stats["rejected_calls"] += 1
                self.stats["last_rejected_operation"] = operation
                self.stats["last_rejected_hresult"] = f"0x{hresult & 0xFFFFFFFF:08X}"
                remaining = self.timeout_s - (now - started)
                if self.progress is not None:
                    self.progress.update(com_retry=dict(self.stats))
                if remaining <= 0:
                    raise TimeoutError(f"Excel COM remained busy for {self.timeout_s}s: {operation}") from error
                wait = min(0.05, remaining)
                time.sleep(wait)
                self.stats["retry_wait_s"] += wait
                if self.progress is not None:
                    self.progress.update(com_retry=dict(self.stats))


class ComObject:
    """Apply the same bounded rejection policy to nested Excel dispatch objects."""
    def __init__(self, raw: Any, retry: ComRetry, name: str = "Excel"):
        object.__setattr__(self, "_raw", raw)
        object.__setattr__(self, "_retry", retry)
        object.__setattr__(self, "_name", name)

    @staticmethod
    def _unwrap(value: Any) -> Any:
        return value._raw if isinstance(value, ComObject) else value

    def _wrap(self, value: Any, name: str) -> Any:
        if hasattr(value, "_oleobj_"):
            return ComObject(value, self._retry, name)
        if callable(value):
            def invoke(*args: Any, **kwargs: Any) -> Any:
                result = self._retry.call(name, value,
                    *(self._unwrap(arg) for arg in args),
                    **{key: self._unwrap(arg) for key, arg in kwargs.items()})
                return self._wrap(result, name)
            return invoke
        return value

    def __getattr__(self, name: str) -> Any:
        operation = f"{self._name}.{name}"
        return self._wrap(self._retry.call(operation, getattr, self._raw, name), operation)

    def __setattr__(self, name: str, value: Any) -> None:
        self._retry.call(f"{self._name}.{name}=", setattr, self._raw, name, self._unwrap(value))

    def __call__(self, *args: Any, **kwargs: Any) -> Any:
        result = self._retry.call(self._name, self._raw,
            *(self._unwrap(arg) for arg in args),
            **{key: self._unwrap(arg) for key, arg in kwargs.items()})
        return self._wrap(result, self._name)


def describe_value(value: Any) -> str:
    if type(value) is int and value in CELL_ERRORS:
        return f"{CELL_ERRORS[value]} ({value})"
    return repr(value)


def calculation_mode(case: Case) -> int:
    # Task UDFs in the Excel-DNA NativeAOT fixture deliver through RTD.
    # Manual mode prevents pending RTD results from reaching worksheet cells.
    live = (case.id.startswith(("A", "R")) or case.id in ("W01", "W02")
            or case.id in ("L01", "L03") and case.params.get("workload") in ("async", "rtd"))
    return XL_AUTOMATIC if live else XL_MANUAL


def register_xll(app: Any, xll: Path) -> None:
    path = str(xll.resolve())
    # Load into this Excel process and check the host's result. An AddIns
    # collection entry alone is not evidence that the XLL was loaded.
    if not app.RegisterXLL(path):
        raise RuntimeError(f"Excel RegisterXLL returned False: {path}")


def registration_diagnostics(app: Any) -> dict[str, Any]:
    """Read registration metadata after a failure without executing UDFs."""
    result: dict[str, Any] = {}
    try:
        entries = app.RegisteredFunctions
        if entries is None:
            result["registered_functions"] = []
        elif isinstance(entries, (tuple, list)):
            result["registered_functions"] = [row for row in entries
                if isinstance(row, (tuple, list)) and len(row) >= 2
                and str(row[1]).startswith("xll_")]
        else:
            result["registered_functions_error"] = f"unexpected metadata: {type(entries).__name__}"
    except Exception as error:
        result["registered_functions_error"] = str(error)
    bindings = {}
    for name in ("BENCH.ALLOC.BYTES", "BENCH.ASYNC", "BENCH.ID", "BENCH.ERRNUM"):
        try:
            # Without parentheses, Evaluate reads the hidden registration ID
            # name. It must not call a UDF on a failed/rolled-back runtime.
            value = app.Evaluate(name)
            bindings[name] = {"type": type(value).__name__, "value": value}
        except Exception as error:
            bindings[name] = {"error": str(error)}
    result["name_bindings"] = bindings
    return result


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
        return f'=BENCH.ERRNUM({row},{params["period"]})'
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


def check_final_values(target: Any, count: int, expected: Any) -> None:
    """Validate the complete final snapshot after sampled arrival timing stops."""
    values = flatten(target.Value2)
    if len(values) != count:
        raise AssertionError(f"final snapshot contains {len(values)}/{count} cells")
    for index, value in enumerate(values):
        if not expected(value, index):
            raise AssertionError(f"final snapshot row {index + 1}: unexpected {describe_value(value)}")


class ExcelSession:
    def __init__(self, xll: Path, threads: int, throttle_ms: int, pid_file: Path,
                 progress: WorkerProgress | None = None):
        import psutil
        import win32com.client

        self.psutil = psutil
        self.control_root = pid_file.parent
        self.progress = progress
        self.com_retry = ComRetry(progress=progress)
        if progress is not None:
            progress.update(com_retry=dict(self.com_retry.stats))
        self.started = time.perf_counter()
        self.stage("excel_start")
        self.app = ComObject(win32com.client.DispatchEx("Excel.Application"), self.com_retry)
        self.hwnd = int(self.app.Hwnd)
        import win32process

        _, self.pid = win32process.GetWindowThreadProcessId(self.hwnd)
        self.process = psutil.Process(self.pid)
        pid_file.write_text(json.dumps({"pid": self.pid, "create_time": self.process.create_time()}),
                            encoding="ascii")
        self.app.Visible = False
        self.app.DisplayAlerts = False
        self.app.AskToUpdateLinks = False
        self.app.EnableEvents = False
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
        self.stage("register_xll", xll=str(xll))
        t = time.perf_counter()
        register_xll(self.app, xll)
        self.load_s = time.perf_counter() - t
        self.ready_s = time.perf_counter() - self.started
        self.book = None
        self.peak_rss = self.process.memory_info().rss
        self.cpu_start = self.process.cpu_times()
        self._stop_sampler = threading.Event()
        self._sampler = threading.Thread(target=self._sample_memory, daemon=True)
        self._sampler.start()

    def stage(self, name: str, **details: Any) -> None:
        self.last_stage = name
        if self.progress is not None:
            self.progress.stage(name, **details)

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
        elapsed = time.perf_counter() - start
        self.memory()
        return elapsed

    def wait_values(self, target: Any, count: int, expected: Any, timeout_s: float,
                    sample_every: int = 1, start_time: float | None = None) -> dict[str, Any]:
        start = start_time if start_time is not None else time.perf_counter()
        seen: dict[int, float] = {}
        indices = list(range(0, count, sample_every))
        if indices[-1] != count - 1:
            indices.append(count - 1)
        values = []
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
        pending = [i for i in indices if i not in seen][:5]
        detail = ", ".join(f"row {i + 1}: {describe_value(values[i])}" for i in pending
                           if i < len(values))
        raise TimeoutError(f"only {len(seen)}/{len(indices)} observed after {timeout_s}s; "
                           f"unsettled values: [{detail}]")

    def close(self):
        self.stage("close_begin")
        self._stop_sampler.set()
        self._sampler.join(timeout=1)
        failures = []
        try:
            self.stage("close_workbook")
            self.close_book()
            self.stage("restore_excel_settings")
            self.app.RTD.ThrottleInterval = self.original_throttle
            self.app.MultiThreadedCalculation.ThreadCount = self.original_threads
            self.app.MultiThreadedCalculation.ThreadMode = self.original_thread_mode
            self.app.MultiThreadedCalculation.Enabled = self.original_mtr_enabled
            self.app.Calculation = self.original_calculation
            self.bootstrap_book.Close(SaveChanges=False)
            # This dedicated Excel process owns the RegisterXLL load.
            self.stage("excel_quit")
            self.app.Quit()
        except Exception as error:
            failures.append({"stage": self.last_stage, "error": f"{type(error).__name__}: {error}"})
            try:
                self.stage("excel_quit_after_cleanup_error")
                self.app.Quit()
            except Exception as error:
                failures.append({"stage": self.last_stage, "error": f"{type(error).__name__}: {error}"})
        if failures:
            self.stage("close_failed", failures=failures)
            raise RuntimeError(f"Excel teardown failed: {json.dumps(failures)}")
        self.stage("closed")


def check_scalar(target: Any, case: Case) -> None:
    values = flatten(target.Value2)
    expected = case.params.get("cells", 1)
    if len(values) != expected:
        raise AssertionError(f"expected {expected} cells, got {len(values)}")
    for row, value in enumerate(values, 1):
        wanted = row
        if case.id == "S02":
            argc = case.params["argc"]
            wanted = argc * row + argc * (argc - 1) // 2
        elif case.id == "S04":
            period = case.params["period"]
            if period > 0 and row % period == 0:
                wanted = XL_ERR_NUM
        if case.id == "P04":
            # Scheduling changes the shared counter; all valid outputs are
            # finite positive numbers. COM cell errors are negative integers.
            valid = type(value) in (int, float) and math.isfinite(value) and value >= row
        elif wanted == XL_ERR_NUM:
            valid = type(value) is int and value == XL_ERR_NUM
        else:
            valid = type(value) in (int, float) and value == wanted
        if not valid:
            expectation = f"a finite number >= {row}" if case.id == "P04" else describe_value(wanted)
            raise AssertionError(
                f"{case.key} row {row}: expected {expectation}, got {describe_value(value)}"
            )


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
        raise AssertionError(
            f"matrix result check failed: {case.key}; anchor: {describe_value(anchor.Value2)}"
        )
    try:
        if case.id == "M05":
            if session.app.Evaluate("BENCH.ALLOC.TRACK(TRUE)") != 1:
                raise AssertionError("allocation tracking could not be enabled")
            allocations_before = session.app.Evaluate("BENCH.ALLOC.BYTES()")
        samples = [session.calculate(anchor) for _ in range(repeat)]
        if not verify():
            raise AssertionError(f"matrix result changed; anchor: {describe_value(anchor.Value2)}")
        result = interval(samples, sum(samples), "recalculation") | {
            "elements": count, "ns_per_element": statistics.median(samples) * 1e9 / count,
        }
        if case.id == "M05":
            result["fixture_allocated_bytes"] = session.app.Evaluate("BENCH.ALLOC.BYTES()") - allocations_before
            result["fixture_allocated_bytes_per_element_per_recalc"] = result["fixture_allocated_bytes"] / count / repeat
            result["allocation_note"] = "Rust allocation tracking enabled only for this observation window; .NET managed allocation counters have different scope; do not compare ratio"
        return result
    finally:
        if case.id == "M05":
            if session.app.Evaluate("BENCH.ALLOC.TRACK(FALSE)") != 0:
                raise AssertionError("allocation tracking could not be disabled")


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
    timeout = min(case.timeout_s, max(30, count * case.params.get("delay_us", 10_000) / 1e6 / 16))
    all_results = []
    submissions = []
    for rep in range(repeat):
        # Different arguments force a new async invocation even if Excel caches.
        offset = rep * count + 1
        session.stage("async_submit", repetition=rep, cells=count)
        submit_start = time.perf_counter()
        target = session.add_formulas(sheet, count,
            lambda row: f'=BENCH.ASYNC({offset + row},{case.params.get("delay_us", 10000)})')
        submissions.append(time.perf_counter() - submit_start)
        session.stage("async_observe", repetition=rep)
        # Automatic calculation can start during formula entry. Include that
        # time, and do not trigger another calculation while callbacks arrive.
        start = submit_start
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
        "latency_note": "From formula submission, including entry time; COM polling gives upper-bound arrival observations at ~10 ms resolution",
    }


class AsyncControl:
    """Observe and release fixture gates without calling Excel from this thread."""
    def __init__(self, session: ExcelSession, expected: int, timeout_s: float = 120):
        self.session = session
        self.expected = expected
        self.timeout_s = timeout_s
        self.path = Path(tempfile.mkdtemp(prefix="async-", dir=session.control_root))
        self.ready: dict[str, Any] | None = None
        self.ready_at: float | None = None
        self.release_at: float | None = None
        self.error: Exception | None = None
        self.done = threading.Event()
        self.stop = threading.Event()
        self.thread: threading.Thread | None = None
        progress = getattr(session, "progress", None)
        self.progress = progress if isinstance(progress, WorkerProgress) else None
        self.diagnostics: dict[str, Any] = {"directory": str(self.path), "expected": expected}
        self.diagnostic_lock = threading.Lock()
        if self.progress is not None:
            self.progress.update(async_control=dict(self.diagnostics))

    def note(self, **details: Any) -> None:
        with self.diagnostic_lock:
            self.diagnostics.update(details)
            if self.progress is not None:
                self.progress.update(async_control=dict(self.diagnostics))

    def read(self, name: str) -> dict[str, Any] | None:
        if self.error is not None:
            raise self.error
        try:
            error = (self.path / "control-error.txt").read_text(encoding="utf-8")
        except FileNotFoundError:
            pass
        else:
            raise RuntimeError(f"fixture async control failed: {error}")
        try:
            return json.loads((self.path / name).read_text(encoding="utf-8"))
        except FileNotFoundError:
            return None

    def arm(self) -> None:
        self.session.stage("async_arm", expected=self.expected, control_dir=str(self.path))
        directory = str(self.path).replace('"', '""')
        if self.session.app.Evaluate(f'BENCH.ASYNC.ARM("{directory}",{self.expected})') != 1:
            raise AssertionError("async control could not be armed")

    def release(self) -> None:
        if self.release_at is None:
            self.release_at = time.perf_counter()
            (self.path / "release").write_text("", encoding="ascii")
            self.note(release_requested=True)

    def start(self, release_when_ready: bool = True) -> None:
        def control() -> None:
            try:
                deadline = time.perf_counter() + self.timeout_s
                while not self.stop.is_set() and time.perf_counter() < deadline:
                    ready = self.read("ready.json")
                    if ready is not None:
                        if ready.get("expected") != self.expected or ready.get("active") != self.expected:
                            raise AssertionError(f"invalid async ready evidence: {ready}")
                        self.ready = ready
                        self.ready_at = time.perf_counter()
                        self.note(ready=ready)
                        if release_when_ready:
                            self.release()
                        self.done.set()
                        # Keep an independent escape path while the cancellation
                        # action runs on Excel's COM thread. A stalled action
                        # must become an error, never an indefinitely held gate.
                        while (not release_when_ready and self.release_at is None
                               and not self.stop.is_set() and time.perf_counter() < deadline):
                            self.stop.wait(0.005)
                        if not self.stop.is_set() and self.release_at is None:
                            raise TimeoutError("async cancellation action did not release the gate before its deadline")
                        return
                    self.stop.wait(0.005)
                if not self.stop.is_set():
                    raise TimeoutError(f"async ready marker missing after {self.timeout_s}s; "
                                       f"last state: {self.read('state.json')}")
            except Exception as error:
                self.error = error
                self.note(error=f"{type(error).__name__}: {error}")
                # Unblock outstanding calls so the main COM thread can report
                # the error. This emergency release can never count as a pass.
                self.release()
            finally:
                self.done.set()

        self.thread = threading.Thread(target=control, daemon=True)
        self.thread.start()

    def wait_ready(self) -> dict[str, Any]:
        if not self.done.wait(self.timeout_s + 1):
            raise TimeoutError("async controller did not finish")
        if self.error is not None:
            raise self.error
        if self.ready is None:
            raise AssertionError("async controller stopped without ready evidence")
        return self.ready

    def wait_released(self) -> dict[str, Any]:
        deadline = time.perf_counter() + 30
        while time.perf_counter() < deadline:
            released = self.read("released.json")
            if released is not None:
                self.note(released=released)
                return released
            time.sleep(0.005)
        raise TimeoutError("fixture did not acknowledge async gate release")

    def wait_drained(self) -> dict[str, Any]:
        deadline = time.perf_counter() + 30
        state = None
        while time.perf_counter() < deadline:
            state = self.read("state.json")
            if state is not None:
                if state.get("control_error"):
                    raise AssertionError(f"async control failed: {state}")
                # The release acknowledgement can become visible before the
                # controller replaces its initial zero-active snapshot.
                if (state.get("released") is True and state.get("expected") == self.expected
                        and state.get("started", 0) >= self.expected
                        and state.get("finished") == state.get("started")
                        and state.get("active") == 0):
                    self.note(drained=state)
                    return state
            time.sleep(0.01)
        raise TimeoutError(f"async tasks did not drain after release: {state}")

    def close(self) -> None:
        self.stop.set()
        self.release()
        if self.thread is not None:
            self.thread.join(timeout=1)


class UnsupportedCase(RuntimeError):
    pass


def run_async_gate(session: ExcelSession, case: Case) -> dict[str, Any]:
    sheet = session.new_book()
    n = case.params["cells"]
    control = AsyncControl(session, n)
    try:
        control.arm()
        session.stage("async_gated_submit", cells=n)
        submit_start = time.perf_counter()
        control.start()
        target = session.add_formulas(sheet, n, lambda row: f"=BENCH.ASYNC({row},-1)")
        submission_s = time.perf_counter() - submit_start
        ready = control.wait_ready()
        released = control.wait_released()
        if released.get("active_before_release") != n:
            raise AssertionError(f"async calls were not all pending at release: {released}")
        session.stage("async_gate_observe", ready=ready, released=released)
        observed = session.wait_values(target, n, lambda value, i: value == float(i + 1),
            120, sample_every=max(1, n // 2_000), start_time=control.release_at)
        total_s = time.perf_counter() - submit_start
        ready_s = control.ready_at - submit_start
        state = control.wait_drained()
        check_final_values(target, n, lambda value, i: value == float(i + 1))
    finally:
        control.close()
    return {"formula_count": n, "active_before_release": ready["active"],
            "submission_s": submission_s, "time_until_all_active_s": ready_s,
            "release_to_settle_s": observed["poll_s"],
            "total_to_settle_s": total_s,
            "throughput_per_s": n / (total_s if case.id == "A03" else observed["poll_s"]),
            "cell_latency_p50_s": observed["p50_s"],
            "cell_latency_p95_s": observed["p95_s"],
            "cell_latency_p99_s": observed["p99_s"],
            "sampled_cells": observed["sampled_cells"],
            "control_channel": "fixture-file-v1",
            "fixture_after_drain": state,
            "latency_note": "COM-observed cell arrival from independent file release; includes file polling and Excel scheduling, ~10 ms cell polling"}


def run_async_cancel(session: ExcelSession, case: Case) -> dict[str, Any]:
    if session.implementation == "xlfn":
        raise UnsupportedCase(
            "A05 requires a user-driven Excel cancellation while native async calls are pending. "
            "Excel COM formula entry may wait for native completion, and programmatic recalculation "
            "does not raise CalculationCanceled/CalculationEnded events. A COM-only run cannot "
            "qualify recalc/clear/close cancellation; run the documented interactive procedure.")
    sheet = session.new_book()
    n = case.params["cells"]
    rss_before = session.memory()
    mode = case.params["mode"]
    control = AsyncControl(session, n)
    try:
        control.arm()
        session.stage("async_cancel_submit", cells=n)
        control.start(release_when_ready=False)
        target = session.add_formulas(sheet, n, lambda row: f"=BENCH.ASYNC({row},-1)")
        ready = control.wait_ready()
        session.stage("async_cancel_action", mode=mode, ready=ready)
        begin = time.perf_counter()
        if mode == "recalc":
            # The new generation is finite; only the old generation uses the
            # gate. Do not conflate its normal completion with old-task cancel.
            delay = case.params.get("replacement_delay_us", 0)
            target = session.add_formulas(sheet, n, lambda row: f"=BENCH.ASYNC({n + row},{delay})")
        elif mode == "clear":
            target.ClearContents()
        else:
            session.close_book()
        action_s = time.perf_counter() - begin
        before_release = control.read("state.json")
        control.release()
        released = control.wait_released()
        session.stage("async_cancel_drain", before_release=before_release, released=released)
        if mode == "close":
            session.new_book()
        state = control.wait_drained()
        cleanup_s = time.perf_counter() - control.release_at
        stale = None
        observed = None
        if mode == "recalc":
            observed = session.wait_values(target, n, lambda value, i: value == float(n + i + 1),
                30, sample_every=max(1, n // 2_000))
            stale = sum(value != float(n + i + 1) for i, value in enumerate(flatten(target.Value2)))
        elif mode == "clear":
            stale = sum(value is not None for value in flatten(target.Value2))
        if stale:
            raise AssertionError(f"{stale} cells violated the post-cancellation result")
    finally:
        control.close()
    return {"formula_count": n, "pending_before": ready["active"],
            "action_s": action_s, "task_drain_after_release_s": cleanup_s,
            "active_after_observation": state["active"], "stale_cells_after_observation": stale,
            "fixture_before_release": before_release, "fixture_after_drain": state,
            "rss_before_action_bytes": rss_before, "rss_after_observation_bytes": session.memory(),
            "settle_s": observed["poll_s"] if observed is not None else None,
            "control_channel": "fixture-file-v1",
            "cancellation_note": "Excel-DNA RTD-task result invalidation after recalc/clear/close. Old tasks are deliberately released after the action; drain timing does not prove task cancellation. Close has no cells to inspect."}


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
    if len(values) != n or stale:
        raise AssertionError(f"final repeated-async snapshot has {len(values)}/{n} cells and {stale} stale values")
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


def run_rtd_rate(session: ExcelSession, target: Any, case: Case, subscription_s: float) -> dict[str, Any]:
    n, topics = case.params["cells"], case.params["topics"]
    duration = case.params["duration_s"]
    throttle_s = float(session.app.RTD.ThrottleInterval) / 1000
    period_s = case.params["period_ms"] / 1000
    # The first `topics` rows contain one representative for every topic.
    sampled = target.Resize(topics, 1)

    def numeric(value: Any) -> bool:
        return (isinstance(value, (float, int)) and not isinstance(value, bool)
                and math.isfinite(value) and value >= 0)

    emitted_before = float(session.app.Evaluate("BENCH.RTD.EMITTED()"))
    baseline = flatten(sampled.Value2)
    if len(baseline) != topics:
        raise AssertionError(f"RTD sample has {len(baseline)} cells, expected {topics}")
    last = [value if numeric(value) else None for value in baseline]
    observed = [{value} if numeric(value) else set() for value in baseline]
    changes = [0] * topics
    last_change_s = [0.0] * topics
    begin = time.perf_counter()
    while time.perf_counter() - begin < duration:
        values = flatten(sampled.Value2)
        if len(values) != topics:
            raise AssertionError(f"RTD sample has {len(values)} cells, expected {topics}")
        at = time.perf_counter() - begin
        for topic, value in enumerate(values):
            if numeric(value):
                observed[topic].add(value)
                if last[topic] is None and value == 0:
                    # Zero can become visible after the initial #N/A without
                    # a source publication. Periodic source sequences start
                    # at one; the first positive sequence is real delivery.
                    last[topic] = value
                elif value != last[topic]:
                    changes[topic] += 1
                    last_change_s[topic] = at
                    last[topic] = value
        session.memory()
        time.sleep(0.005)
    elapsed = time.perf_counter() - begin
    emitted_after = float(session.app.Evaluate("BENCH.RTD.EMITTED()"))
    emissions = emitted_after - emitted_before
    stalled = [topic for topic, count in enumerate(changes) if count == 0]
    # This is a diagnostic for sparse delivery, not a requested-cadence test.
    # In particular, two distinct values in a ten-second 1Hz run stay visible
    # as weak progress even though one observed transition is a valid minimum.
    weak = (min(changes) <= 1 and emissions >= 3 * topics
            and elapsed >= 3 * max(period_s, throttle_s, 0.005))
    result = {"subscription_s": subscription_s,
        "observed_updates_per_s": changes[0] / elapsed,
        "source_emissions_per_s": emissions / elapsed,
        "source_emissions_per_topic_s": emissions / elapsed / topics,
        "requested_updates_per_topic_s": case.params["requested_hz"],
        "observed_distinct_values": len(observed[0]), "duration_s": duration,
        "observation_s": elapsed, "formula_count": n, "topic_count": topics,
        "sampled_topics": topics, "topics_with_observed_progress": topics - len(stalled),
        "observed_changes_by_topic": changes, "observed_last_values_by_topic": last,
        "last_change_age_s_by_topic": [elapsed - at for at in last_change_s],
        "source_emissions": emissions,
        "delivery_observation": "incomplete_progress" if stalled else "weak_progress" if weak else "progress",
        "delivery_note": "One cell per topic; changes exclude the initial snapshot and initial zero. A first positive sequence after #N/A is delivery. Polling is a lower bound and RTD throttle may coalesce updates. Weak progress is diagnostic, not a cadence failure."}
    session.stage("rtd_rate_observation", **result)
    if emissions <= 0:
        raise AssertionError("RTD source emitted no periodic updates")
    if stalled:
        detail = f"no observed numeric progress for topics {stalled}; last values: {last}"
        if elapsed <= period_s + throttle_s + 0.005:
            raise UnsupportedCase(f"RTD observation window is shorter than a source period plus throttle: {detail}")
        raise AssertionError(f"RTD source emitted {emissions} updates but {detail}")
    return result


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
        return run_rtd_rate(session, target, case, subscription_s)
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
    begin = time.perf_counter()
    scalar = session.add_formulas(sheet, n, lambda row: f"=BENCH.ID({row})")
    auxiliary = max(1, n // 10)
    session.add_formulas(sheet, auxiliary, lambda row: f'=BENCH.ASYNC({row},10000)', "C")
    rtd_target = session.add_formulas(sheet, auxiliary, lambda row: f'=BENCH.RTD("market-{row % 10}",0)', "D")
    sheet.Range("F1").Formula2 = "=BENCH.MAT.MAKE(100,10,0)"
    if case.id == "W02":
        session.add_formulas(sheet, n,
            lambda row: f"=BENCH.SHARED(D{1 + ((row - 1) % auxiliary)})", "E")
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


def recover_worker_record(path: Path, case: Case, implementation: str,
                          error: Exception) -> dict[str, Any]:
    try:
        record = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError):
        record = {"id": case.id, "variant": case.variant, "implementation": implementation}
    if "error" in record:
        record["worker_error"] = record["error"]
    record["status"] = "error"
    record["error"] = f"worker timeout/failure: {error}"
    record["recovered_partial_record"] = path.is_file()
    return record


def close_worker_session(session: ExcelSession, progress: WorkerProgress) -> None:
    try:
        session.close()
    except Exception as error:
        record = progress.record
        cleanup_error = f"{type(error).__name__}: {error}"
        # Preserve an execution error as the primary cause when both fail.
        progress.update(status="error", execution_status=record.get("status"),
            error=record.get("error", cleanup_error), cleanup_error=cleanup_error,
            failure_phase=record.get("failure_phase", "close"),
            failure_stage=record.get("failure_stage", record.get("stage")))


def reap_excel_process(pid_file: Path, grace_s: float) -> dict[str, Any]:
    """Reap only the exact Excel process created by this worker, including on success."""
    if not pid_file.is_file():
        return {"pid_recorded": False}
    import psutil

    result: dict[str, Any] = {"forced_kill": False}
    try:
        identity = json.loads(pid_file.read_text(encoding="ascii"))
        result["pid"] = identity["pid"]
        process = psutil.Process(identity["pid"])
        if process.create_time() != identity["create_time"]:
            return result | {"original_process_exited": True, "pid_reused": True}
        try:
            process.wait(timeout=grace_s)
        except psutil.TimeoutExpired:
            process.kill()
            result["forced_kill"] = True
            process.wait(timeout=5)
        result["original_process_exited"] = True
    except psutil.NoSuchProcess:
        result["original_process_exited"] = True
    except (psutil.Error, OSError, ValueError, KeyError, TypeError) as error:
        result["error"] = f"{type(error).__name__}: {error}"
    return result


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
    progress = WorkerProgress(Path(args.progress_file), record)
    startup_log = snapshot_startup_log()
    session = None
    try:
        if manifest is not None:
            relative = f"{args.implementation}/{xll.name}"
            if record["xll_sha256"] != manifest["files"][relative]["sha256"]:
                raise ValueError(f"CI XLL changed after verification: {relative}")
        progress.update(phase="load_xll", xll_load_method="RegisterXLL")
        progress.stage("load_begin")
        session = ExcelSession(xll, record["threads"], args.throttle_ms, Path(args.pid_file), progress)
        session.implementation = args.implementation
        progress.update(excel_version=str(session.app.Version), excel_build=str(session.app.Build),
                        excel_bitness=session.app.OperatingSystem)
        mode = calculation_mode(case)
        session.app.Calculation = mode
        progress.update(calculation_mode="automatic" if mode == XL_AUTOMATIC else "manual")
        if case.id.startswith("A") or case.params.get("workload") == "async":
            progress.update(async_delivery="native" if args.implementation == "xlfn" else "rtd-task")
        progress.update(phase="execute")
        progress.stage("execute_begin")
        metrics = execute(session, case, args.repeat)
        metrics["peak_excel_rss_bytes"] = session.peak_rss
        metrics["excel_cpu_s"] = session.cpu_s()
        progress.update(metrics=metrics, status="ok")
    except UnsupportedCase as error:
        progress.update(status="unsupported", unsupported_reason=str(error), error=str(error))
    except Exception as error:
        # Persist the cause before diagnostic COM calls or teardown can hang.
        progress.update(status="error", error=f"{type(error).__name__}: {error}",
                        failure_phase=record.get("phase"), failure_stage=record.get("stage"))
        if args.implementation == "xlfn":
            progress.update(startup_log=read_startup_log_delta(startup_log))
        if session is not None and args.implementation == "xlfn":
            progress.stage("registration_diagnostics")
            progress.update(registration_diagnostics=registration_diagnostics(session.app))
    finally:
        if session is not None:
            progress.update(phase="close")
            close_worker_session(session, progress)
    progress.update(phase="complete" if record["status"] == "ok" else record.get("failure_phase", "execute"))
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
    parser.add_argument("--progress-file", help=argparse.SUPPRESS)
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
        if case is None or args.implementation is None or args.pid_file is None or args.progress_file is None:
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
                progress_file = Path(tmp) / "progress.json"
                cmd = [sys.executable, str(Path(__file__).resolve()), "--worker",
                       "--profile", args.profile, "--artifacts", args.artifacts,
                       "--implementation", implementation, "--case-key", case.key,
                       "--repeat", str(args.repeat), "--threads", str(args.threads),
                       "--throttle-ms", str(args.throttle_ms), "--pid-file", str(pid_file),
                       "--progress-file", str(progress_file)]
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
                    record = recover_worker_record(progress_file, case, implementation, error)
                cleanup = reap_excel_process(pid_file, grace_s=5 if record["status"] == "ok" else 0)
                record["excel_process_cleanup"] = cleanup
                if cleanup.get("forced_kill") or cleanup.get("error"):
                    error = cleanup.get("error", "Excel survived worker teardown and required forced termination")
                    if record["status"] != "error":
                        record.update(execution_status=record["status"], status="error",
                                      error=record.get("error", error), failure_phase="close", phase="close")
                    record.setdefault("cleanup_error", error)
                with out.open("a", encoding="utf-8") as file:
                    file.write(json.dumps(record, ensure_ascii=False) + "\n")
                print(f"{case.key} {implementation}: {record['status']}", flush=True)
                if record["status"] != "ok":
                    print(record.get("error", "unknown error"), flush=True)
                    if "registration_diagnostics" in record:
                        print(json.dumps(record["registration_diagnostics"], ensure_ascii=False), flush=True)
                errors += record["status"] != "ok"
    return 1 if errors else 0


if __name__ == "__main__":
    raise SystemExit(main())

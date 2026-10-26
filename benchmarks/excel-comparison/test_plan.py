from __future__ import annotations

import json
import gc
import os
import re
import struct
import subprocess
import tempfile
import threading
import time
from typing import Any
import unittest
import weakref
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import Mock, call, patch

from artifact_manifest import FILES, create, verify
from generate_registration import source
from run import (XL_AUTOMATIC, XL_MANUAL, XL_ERR_NUM, AsyncControl, ComObject, ComRetry,
                 ExcelSession, UnsupportedCase, WorkerProgress, apply_excel_cleanup, calculation_mode,
                 check_final_values, check_scalar, close_worker_session, formula, register_xll,
                 registration_diagnostics, run_async, reap_excel_process, run_scalar, run_text, allocation_snapshot,
                 recover_worker_record, run_async_cancel, run_async_gate, run_async_repeated, run_matrix, run_numeric_output_pair,
                 native_abi_formula, run_native_abi_pair, classify_abi_semantics, run_native_abi_semantics, artifact)
from run import main as benchmark_main
from summarize import PRIMARY, summarize
from workloads import IDS, Case, cases


def atomic_write_json(path: Path, payload: Any) -> None:
    temporary = path.with_suffix(path.suffix + ".tmp")
    temporary.write_text(json.dumps(payload), encoding="utf-8")
    temporary.replace(path)


class PlanTest(unittest.TestCase):
    def test_dump_only_records_native_identity_without_enabling_lifecycle_observation(self):
        for dump_only, full_diagnostics in ((False, False), (True, False), (False, True), (True, True)):
            with self.subTest(dump_only=dump_only, full_diagnostics=full_diagnostics), \
                    tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                pid_file = root / "excel.pid"
                progress = WorkerProgress(root / "progress.json", {})
                app = SimpleNamespace(Hwnd=99, Calculation=XL_AUTOMATIC,
                    Workbooks=SimpleNamespace(Add=lambda: object()),
                    RTD=SimpleNamespace(ThrottleInterval=100),
                    MultiThreadedCalculation=SimpleNamespace(Enabled=True, ThreadMode=0, ThreadCount=1))
                process = Mock()
                process.create_time.return_value = 42.0
                process.memory_info.return_value = SimpleNamespace(rss=1024)
                client = SimpleNamespace(DispatchEx=Mock(return_value=app))
                modules = {"psutil": SimpleNamespace(Process=Mock(return_value=process)),
                           "win32com": SimpleNamespace(client=client), "win32com.client": client,
                           "win32process": SimpleNamespace(GetWindowThreadProcessId=lambda hwnd: (1, 123))}
                with patch.dict("sys.modules", modules), \
                        patch("run.process_creation_ticks", return_value=123456) as ticks, \
                        patch("run.await_observer", return_value={"status": "watching"}) as observe, \
                        patch("run.prepare_lifecycle_trace", return_value={"path": "trace"}) as trace, \
                        patch("run.threading.Thread"):
                    session = ExcelSession(None, 1, 100, pid_file, progress,
                        shutdown_diagnostics=full_diagnostics, lifecycle_trace_file=root / "trace.jsonl",
                        dump_on_failure=dump_only)
                identity = json.loads(pid_file.read_text())
                self.assertEqual(identity["pid"], 123)
                self.assertEqual("creation_ticks" in identity, dump_only or full_diagnostics)
                self.assertEqual(ticks.call_count, int(dump_only or full_diagnostics))
                self.assertEqual(observe.call_count, int(full_diagnostics))
                self.assertEqual(trace.call_count, int(full_diagnostics))
                self.assertEqual(session.shutdown_diagnostics, full_diagnostics)
                self.assertEqual("lifecycle_trace_config" in progress.record, full_diagnostics)

    def test_dump_only_cli_passes_mode_to_worker_and_capture_to_reaper_without_observer(self):
        for dump_only in (False, True):
            for failed in (False, True):
                with self.subTest(dump_only=dump_only, failed=failed), tempfile.TemporaryDirectory() as directory:
                    root = Path(directory)
                    xll = root / "xlfn" / "benchmark.xll"
                    xll.parent.mkdir()
                    xll.touch()
                    output = root / "result.jsonl"
                    args = ["run.py", "--profile", "smoke", "--id", "S01", "--implementation", "xlfn",
                            "--allow-local-artifacts", "--artifacts", str(root), "--out", str(output)]
                    if dump_only:
                        args.append("--dump-on-failure")
                    cleanup = {"forced_kill": failed}
                    if failed:
                        cleanup["exit_timeout_s"] = 5
                    child = SimpleNamespace(stdout=json.dumps({"status": "ok", "id": "S01"}), stderr="")
                    with patch("run.sys.argv", args), patch("run.sys.platform", "win32"), \
                            patch("run.subprocess.run", return_value=child) as launch, \
                            patch("run.reap_excel_process", return_value=cleanup) as reap, \
                            patch("run.ProcessExitObserver") as observer, patch("builtins.print"):
                        self.assertEqual(benchmark_main(), int(failed))
                    cmd = launch.call_args.args[0]
                    self.assertEqual("--dump-on-failure" in cmd, dump_only)
                    self.assertNotIn("--shutdown-diagnostics", cmd)
                    self.assertNotIn("--lifecycle-trace-file", cmd)
                    observer.assert_not_called()
                    self.assertEqual(reap.call_args.kwargs["grace_s"], 5)
                    self.assertEqual(reap.call_args.kwargs["dump_directory"],
                                     output.with_suffix(".diagnostics") if dump_only else None)
                    record = json.loads(output.read_text())
                    self.assertEqual(record["status"], "error" if failed else "ok")
                    if failed:
                        self.assertEqual(record["execution_status"], "ok")
                        self.assertEqual(record["failure_phase"], "close")

    def test_unsupported_case_gets_normal_exit_grace_and_reports_cleanup_failure(self):
        for survives_deadline in (False, True):
            with self.subTest(survives_deadline=survives_deadline), \
                    tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                xll = root / "xlfn" / "benchmark.xll"
                xll.parent.mkdir()
                xll.touch()
                output = root / "result.jsonl"
                args = ["run.py", "--profile", "smoke", "--id", "A05", "--variant", "recalc",
                        "--implementation", "xlfn", "--allow-local-artifacts",
                        "--artifacts", str(root), "--out", str(output), "--dump-on-failure"]
                reason = "interactive cancellation required"
                child = SimpleNamespace(stdout=json.dumps({"status": "unsupported", "id": "A05",
                    "unsupported_reason": reason, "error": reason}), stderr="")
                cleanup = {"forced_kill": survives_deadline, "original_process_exited": True}
                if survives_deadline:
                    cleanup["exit_timeout_s"] = 5
                with patch("run.sys.argv", args), patch("run.sys.platform", "win32"), \
                        patch("run.subprocess.run", return_value=child), \
                        patch("run.reap_excel_process", return_value=cleanup) as reap, \
                        patch("builtins.print") as printed:
                    # Unsupported coverage still produces a nonzero exit code.
                    self.assertEqual(benchmark_main(), 1)
                self.assertEqual(reap.call_args.kwargs["grace_s"], 5)
                self.assertEqual(reap.call_args.kwargs["dump_directory"],
                                 output.with_suffix(".diagnostics"))
                record = json.loads(output.read_text())
                self.assertEqual(record["unsupported_reason"], reason)
                self.assertEqual(record["status"], "error" if survives_deadline else "unsupported")
                if survives_deadline:
                    self.assertEqual(record["execution_status"], "unsupported")
                    self.assertEqual(record["failure_phase"], "close")
                    printed.assert_any_call("cleanup: " + record["cleanup_error"], flush=True)
                else:
                    self.assertNotIn("cleanup_error", record)

    def test_numeric_output_pair_verifies_every_cell_and_uses_balanced_order(self):
        anchors = {}
        areas = []
        class Sheet:
            def Cells(self, row, column):
                return anchors.setdefault((row, column), SimpleNamespace())
            def Range(self, first, last):
                area = SimpleNamespace(Value2=((0.0, 1.0),))
                areas.append(area)
                return area
        sheet = Sheet()
        order = []
        def calculate(target):
            order.append("Q" if target is anchors[(1, 1)] else "FP12")
            return 1.0 if order[-1] == "Q" else 0.5
        session = SimpleNamespace(implementation="xlfn", new_book=lambda: sheet,
                                  calculate=calculate, last_calculation_timing={})
        case = Case("M06", "1x2", {"rows": 1, "cols": 2})
        result = run_numeric_output_pair(session, case, 2)
        self.assertEqual(order[2:], ["Q", "FP12", "FP12", "Q", "FP12", "Q", "Q", "FP12"])
        self.assertEqual(result["fp12_over_q_ratio"], 0.5)
        self.assertEqual(anchors[(1, 1)].Formula2, "=BENCH.MAT.MAKE.Q(1,2,0)")
        with self.assertRaises(UnsupportedCase):
            run_numeric_output_pair(SimpleNamespace(implementation="excel_dna"), case, 1)
        areas.clear()
        with patch.object(Sheet, "Range", return_value=SimpleNamespace(Value2=((0.0, -1.0),))):
            with self.assertRaisesRegex(AssertionError, "spill verification"):
                run_numeric_output_pair(session, case, 1)

    def test_numeric_output_pair_summary_retains_within_xlfn_metrics(self):
        record = {"id": "M06", "variant": "1000x100", "implementation": "xlfn", "status": "ok",
                  "metrics": {"q_p50_s": 2.0, "fp12_p50_s": 1.0,
                              "fp12_over_q_ratio": 0.5, "fp12_change_percent": -50.0}}
        row = summarize([record])[0]
        self.assertEqual(row["comparison"], "within_xlfn_fp12_vs_q")
        self.assertEqual(row["fp12_over_q_ratio"], 0.5)
        self.assertFalse(row["comparable"])

    def test_close_releases_excel_references_including_cycles_before_worker_exit(self):
        calls = []

        class App:
            def __init__(self):
                self.RTD = SimpleNamespace(ThrottleInterval=100)
                self.MultiThreadedCalculation = SimpleNamespace()
                self.cycle = self

            def Quit(self):
                calls.append("quit")

        class Book:
            def __init__(self, app, name):
                self.app, self.name = app, name
                self.cycle = self

            def Close(self, **kwargs):
                calls.append(self.name)

        session = object.__new__(ExcelSession)
        session.progress = None
        session._stop_sampler = Mock()
        session._sampler = Mock()
        session.app = App()
        session.book = Book(session.app, "book")
        session.bootstrap_book = Book(session.app, "bootstrap")
        session.original_throttle = 2000
        session.original_threads = 1
        session.original_thread_mode = 0
        session.original_mtr_enabled = True
        session.original_calculation = XL_AUTOMATIC
        references = [weakref.ref(value) for value in
                      (session.app, session.book, session.bootstrap_book)]
        # Require close itself to collect wrapper cycles, independently of
        # Python's automatic collection or interpreter shutdown.
        enabled = gc.isenabled()
        gc.disable()
        try:
            session.close()
            self.assertEqual(calls, ["book", "bootstrap", "quit"])
            self.assertTrue(all(reference() is None for reference in references))
            self.assertIsNone(session.app)
            self.assertIsNone(session.bootstrap_book)
            self.assertIsNone(session.book)
            self.assertEqual(session.last_stage, "closed")
        finally:
            if enabled:
                gc.enable()

    def test_teardown_failure_survives_successful_fallback_quit(self):
        for quit_error in (None, RuntimeError("quit also failed")):
            with self.subTest(quit_error=quit_error):
                session = object.__new__(ExcelSession)
                session.progress = None
                session._stop_sampler = Mock()
                session._sampler = Mock()
                session.close_book = Mock(side_effect=RuntimeError("workbook close failed"))
                session.app = app = Mock()
                session.bootstrap_book = bootstrap = Mock()
                session.book = Mock()
                session.original_throttle = 2000
                session.original_threads = 1
                session.original_thread_mode = 0
                session.original_mtr_enabled = True
                session.original_calculation = XL_AUTOMATIC
                app.Quit.side_effect = quit_error
                detail = "workbook close failed" + (".*quit also failed" if quit_error else "")
                with self.assertRaisesRegex(RuntimeError, detail):
                    session.close()
                self.assertEqual(app.Quit.call_count, 2 if quit_error else 1)
                bootstrap.Close.assert_called_once_with(SaveChanges=False)
                self.assertEqual(session.last_stage, "close_failed")
                self.assertIsNone(session.app)
                self.assertIsNone(session.book)
                self.assertIsNone(session.bootstrap_book)

    def test_worker_close_cannot_preserve_ok_or_hide_execution_failure(self):
        for original_error in (None, "original execution failed"):
            with self.subTest(original_error=original_error), tempfile.TemporaryDirectory() as directory:
                record = {"status": "ok", "phase": "close", "stage": "excel_quit"}
                if original_error is not None:
                    record.update(status="error", error=original_error, failure_phase="execute")
                progress = WorkerProgress(Path(directory) / "progress.json", record)
                session = SimpleNamespace(close=Mock(side_effect=RuntimeError("quit failed")))
                close_worker_session(session, progress)
                self.assertEqual(record["status"], "error")
                self.assertIn("quit failed", record["cleanup_error"])
                if original_error is not None:
                    self.assertEqual(record["error"], original_error)
                    self.assertEqual(record["failure_phase"], "execute")
                else:
                    self.assertEqual(record["failure_phase"], "close")

    def test_reaper_kills_only_the_exact_surviving_worker_process(self):
        class ProcessError(Exception):
            pass
        class Timeout(ProcessError):
            pass
        class Missing(ProcessError):
            pass
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "excel.pid"
            path.write_text(json.dumps({"pid": 123, "create_time": 42.0}))
            process = Mock()
            process.create_time.return_value = 42.0
            process.wait.side_effect = [Timeout(), None]
            module = SimpleNamespace(Process=Mock(return_value=process), Error=ProcessError,
                                     TimeoutExpired=Timeout, NoSuchProcess=Missing)
            with patch.dict("sys.modules", {"psutil": module}):
                result = reap_excel_process(path, grace_s=5)
                self.assertTrue(result["forced_kill"])
                process.kill.assert_called_once_with()
                process.reset_mock()
                process.create_time.return_value = 43.0
                result = reap_excel_process(path, grace_s=5)
                self.assertTrue(result["pid_reused"])
                process.kill.assert_not_called()
                process.wait.assert_not_called()

    def test_shutdown_dump_precedes_kill_and_cannot_hide_a_missed_exit_deadline(self):
        class ProcessError(Exception):
            pass
        class Timeout(ProcessError):
            pass
        class Missing(ProcessError):
            pass

        for capture_failure in (False, True):
            for exits_during_capture in (False, True):
                with self.subTest(capture_failure=capture_failure,
                                  exits_during_capture=exits_during_capture), \
                        tempfile.TemporaryDirectory() as directory:
                    root = Path(directory)
                    path = root / "excel.pid"
                    identity = {"pid": 123, "create_time": 42.0, "creation_ticks": 123456}
                    path.write_text(json.dumps(identity))
                    events = []
                    process = Mock()
                    process.create_time.return_value = 42.0
                    process.wait.side_effect = [Timeout(), None]

                    def capture(*args):
                        events.append("capture")
                        self.assertEqual(args, (identity, root))
                        if capture_failure:
                            raise RuntimeError("dump failed")
                        return {"status": "ok", "path": "shutdown.dmp"}

                    def kill():
                        events.append("kill")
                        if exits_during_capture:
                            raise Missing()

                    process.kill.side_effect = kill
                    module = SimpleNamespace(Process=Mock(return_value=process), Error=ProcessError,
                                             TimeoutExpired=Timeout, NoSuchProcess=Missing)
                    with patch.dict("sys.modules", {"psutil": module}), \
                            patch("run.capture_shutdown_dump", side_effect=capture):
                        cleanup = reap_excel_process(path, grace_s=5, dump_directory=root)
                    self.assertEqual(events, ["capture", "kill"])
                    self.assertEqual(cleanup["exit_timeout_s"], 5)
                    self.assertEqual(cleanup["forced_kill"], not exits_during_capture)
                    self.assertTrue(cleanup["original_process_exited"])
                    self.assertEqual(cleanup["shutdown_dump"]["status"],
                                     "error" if capture_failure else "ok")
                    for original_error in (None, "calculation failed"):
                        record = {"status": "ok"}
                        if original_error:
                            record.update(status="error", error=original_error, failure_phase="execute")
                        apply_excel_cleanup(record, cleanup)
                        self.assertEqual(record["status"], "error")
                        self.assertIn("cleanup_error", record)
                        if original_error:
                            self.assertEqual(record["error"], original_error)
                            self.assertEqual(record["failure_phase"], "execute")
                        else:
                            self.assertEqual(record["execution_status"], "ok")
                            self.assertEqual(record["failure_phase"], "close")

    def test_no_shutdown_dump_for_an_exited_or_reused_process(self):
        class ProcessError(Exception):
            pass
        class Timeout(ProcessError):
            pass
        class Missing(ProcessError):
            pass

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            path = root / "excel.pid"
            path.write_text(json.dumps({"pid": 123, "create_time": 42.0}))
            for created in (42.0, 43.0):
                process = Mock()
                process.create_time.return_value = created
                module = SimpleNamespace(Process=Mock(return_value=process), Error=ProcessError,
                                         TimeoutExpired=Timeout, NoSuchProcess=Missing)
                with patch.dict("sys.modules", {"psutil": module}), \
                        patch("run.capture_shutdown_dump") as capture:
                    cleanup = reap_excel_process(path, grace_s=5, dump_directory=root)
                capture.assert_not_called()
                process.kill.assert_not_called()
                record = {"status": "ok"}
                apply_excel_cleanup(record, cleanup)
                self.assertEqual(record["status"], "ok")

    def test_progress_serializes_concurrent_stage_and_snapshot_updates(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "progress.json"
            progress = WorkerProgress(path, {})
            errors = []
            def write(prefix):
                try:
                    for index in range(20):
                        progress.stage(prefix, index=index)
                        progress.update(**{prefix: {"index": index}})
                except Exception as error:
                    errors.append(error)
            workers = [threading.Thread(target=write, args=(name,)) for name in ("main", "controller")]
            for thread in workers:
                thread.start()
            for thread in workers:
                thread.join(timeout=3)
            self.assertFalse(errors)
            result = json.loads(path.read_text())
            self.assertEqual(len(result["stage_history"]), 40)
            self.assertEqual(result["main"], {"index": 19})
            self.assertEqual(result["controller"], {"index": 19})

    def test_only_explicit_com_rejections_are_retried_with_a_deadline(self):
        class ComError(Exception):
            def __init__(self, hresult):
                self.hresult = hresult

        for hresult in (-2147418111, -2147417846):
            with self.subTest(hresult=hresult), patch("run.time.sleep"):
                retry = ComRetry()
                operation = Mock(side_effect=[ComError(hresult), 7])
                self.assertEqual(retry.call("Excel.Value2", operation), 7)
                self.assertEqual(retry.stats["rejected_calls"], 1)
                self.assertEqual(operation.call_count, 2)
        retry = ComRetry(timeout_s=0)
        operation = Mock(side_effect=ComError(-2147418111))
        with self.assertRaisesRegex(TimeoutError, "Excel.Value2"):
            retry.call("Excel.Value2", operation)
        operation.assert_called_once_with()
        operation = Mock(side_effect=ComError(-2147352567))
        with self.assertRaises(ComError):
            retry.call("Excel.Formula=", operation)
        operation.assert_called_once_with()

    def test_com_policy_reaches_nested_dispatch_and_unwraps_arguments(self):
        class Dispatch:
            _oleobj_ = object()
            Value2 = 5
            def Range(self, start, end):
                self.args = (start, end)
                return self

        raw = Dispatch()
        app = ComObject(raw, ComRetry())
        target = app.Range(app, app)
        self.assertEqual(raw.args, (raw, raw))
        self.assertEqual(target.Value2, 5)
        target.Value2 = 9
        self.assertEqual(raw.Value2, 9)

    def test_partial_worker_record_preserves_metadata_and_original_failure(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "progress.json"
            record = {"id": "A03", "variant": "3", "implementation": "xlfn",
                      "xll_sha256": "abc", "excel_build": "123", "phase": "execute",
                      "status": "error", "error": "gate failed"}
            progress = WorkerProgress(path, record)
            progress.stage("excel_quit")
            recovered = recover_worker_record(path, Case("A03", "3", {}), "xlfn", TimeoutError("expired"))
            self.assertEqual(recovered["stage"], "excel_quit")
            self.assertEqual(recovered["xll_sha256"], "abc")
            self.assertEqual(recovered["excel_build"], "123")
            self.assertEqual(recovered["worker_error"], "gate failed")
            self.assertEqual(recovered["status"], "error")

    def test_partial_record_keeps_rtd_failure_before_diagnostic_com_call_returns(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "progress.json"
            progress = WorkerProgress(path, {"id": "R03", "variant": "10000-10"})
            progress.stage("rtd_pulse_failed", error="TimeoutError: RTD delivery stopped",
                           traceback="original execution traceback")
            recovered = recover_worker_record(path, Case("R03", "10000-10", {}),
                                              "xlfn", TimeoutError("worker expired"))
            self.assertEqual(recovered["worker_error"], "TimeoutError: RTD delivery stopped")
            self.assertEqual(recovered["traceback"], "original execution traceback")
            self.assertIn("worker expired", recovered["error"])

    def test_calculation_timing_includes_state_queries_and_waits(self):
        clock = [0.0]
        states = iter((1, 1, 0))

        class App:
            def Calculate(self):
                clock[0] += 0.25

            @property
            def CalculationState(self):
                clock[0] += 0.01
                return next(states)

        session = object.__new__(ExcelSession)
        session.app = App()
        session.memory = Mock()
        with patch("run.time.perf_counter", side_effect=lambda: clock[0]), \
                patch("run.time.sleep", side_effect=lambda delay: clock.__setitem__(0, clock[0] + delay)):
            elapsed = session.calculate()
        timing = session.last_calculation_timing
        self.assertAlmostEqual(elapsed, 0.29)
        self.assertEqual(timing["completion_polls"], 3)
        self.assertAlmostEqual(timing["calculate_call_s"], 0.25)
        self.assertAlmostEqual(timing["completion_wait_s"], 0.04)
        self.assertEqual(timing["total_s"], elapsed)

    def test_calculation_timer_stops_before_process_sampling(self):
        for full in (False, True):
            with self.subTest(full=full):
                clock = [10.0]
                session = object.__new__(ExcelSession)
                session.app = Mock()
                session.app.CalculationState = 0

                def calculate():
                    clock[0] += 0.25

                def sample_memory():
                    clock[0] += 50.0

                session.app.Calculate.side_effect = calculate
                session.app.CalculateFull.side_effect = calculate
                session.memory = Mock(side_effect=sample_memory)
                target = Mock()
                with patch("run.time.perf_counter", side_effect=lambda: clock[0]):
                    elapsed = session.calculate(target, full=full)
                self.assertEqual(elapsed, 0.25)
                self.assertEqual(session.last_calculation_timing, {
                    "total_s": 0.25, "calculate_call_s": 0.25,
                    "completion_wait_s": 0.0, "completion_polls": 1,
                })
                target.Dirty.assert_called_once_with()
                session.memory.assert_called_once_with()
                if full:
                    session.app.CalculateFull.assert_called_once_with()
                    session.app.Calculate.assert_not_called()
                else:
                    session.app.Calculate.assert_called_once_with()
                    session.app.CalculateFull.assert_not_called()

    def test_live_workloads_allow_rtd_completion(self):
        for case in cases("smoke"):
            with self.subTest(case=case.key):
                live = (case.id[0] in "AR" or case.id in ("W01", "W02")
                        or case.id in ("L01", "L03") and case.params["workload"] in ("async", "rtd"))
                self.assertEqual(calculation_mode(case), XL_AUTOMATIC if live else XL_MANUAL)

    def test_async_measures_from_entry_without_redundant_invocations(self):
        session = Mock()
        submitted = []
        def submit(sheet, count, make):
            submitted.append([make(row) for row in range(1, count + 1)])
            return object()
        session.add_formulas.side_effect = submit
        def observe(target, count, expected, timeout, **kwargs):
            base = 2 if len(submitted) == 1 else 5
            for i in range(count):
                self.assertTrue(expected(float(base + i), i))
            return {"p50_s": 0.1, "p95_s": 0.1, "p99_s": 0.1, "sampled_cells": 3}
        session.wait_values.side_effect = observe
        with patch("run.time.perf_counter", side_effect=[10, 10.05, 10.1, 20, 20.05, 20.1]):
            result = run_async(session, Case("A02", "100", {"cells": 3, "delay_us": 100}), 2)
        self.assertEqual(submitted, [["=BENCH.ASYNC(2,100)", "=BENCH.ASYNC(3,100)", "=BENCH.ASYNC(4,100)"],
                                     ["=BENCH.ASYNC(5,100)", "=BENCH.ASYNC(6,100)", "=BENCH.ASYNC(7,100)"]])
        session.app.Calculate.assert_not_called()
        for call, start in zip(session.wait_values.call_args_list, (10, 20)):
            self.assertEqual(call.kwargs["start_time"], start)
        self.assertAlmostEqual(result["p50_s"], 0.1)

    def test_async_timeout_reports_unsettled_values(self):
        session = SimpleNamespace(memory=Mock())
        target = SimpleNamespace(Value2=((1.0,), (-2146826246,), (99.0,)))
        with patch("run.time.perf_counter", side_effect=[0, 0, 0.2, 31]), patch("run.time.sleep"):
            with self.assertRaisesRegex(TimeoutError, r"only 1/3.*row 2: #N/A.*row 3: 99"):
                ExcelSession.wait_values(session, target, 3, lambda v, i: v == i + 1, 30)

    def test_async_arm_avoids_evaluate_reentering_a_one_shot_initializer(self):
        with tempfile.TemporaryDirectory() as directory:
            session = Mock(control_root=Path(directory))
            control = AsyncControl(session, 100)
            calls = []

            def invoke(name, path, count):
                calls.append((name, path, count))
                return 1.0 if len(calls) == 1 else -2146826273  # #VALUE! on re-arm

            def evaluate(expression):
                # Model the host formula-evaluation behavior that would make
                # the first ARM succeed and its repeated invocation fail.
                invoke("BENCH.ASYNC.ARM", str(control.path), 100)
                return invoke("BENCH.ASYNC.ARM", str(control.path), 100)

            session.app.Run.side_effect = invoke
            session.app.Evaluate.side_effect = evaluate
            control.arm()
            self.assertEqual(calls, [("BENCH.ASYNC.ARM", str(control.path), 100)])
            session.app.Evaluate.assert_not_called()
            self.assertEqual(control.diagnostics["arm_result"], {"type": "float", "value": "1.0"})

    def test_async_arm_failure_retains_return_value_and_existing_state(self):
        for value in (-2146826273, -2146826259, True, None, "1"):
            with self.subTest(value=value), tempfile.TemporaryDirectory() as directory:
                progress = WorkerProgress(Path(directory) / "progress.json", {})
                session = Mock(control_root=Path(directory), progress=progress)
                session.app.Run.return_value = value
                control = AsyncControl(session, 100)
                state = {"active": 0, "expected": 100, "released": False}
                (control.path / "state.json").write_text(json.dumps(state), encoding="utf-8")
                with self.assertRaisesRegex(AssertionError, "via Application.Run; fixture state") as caught:
                    control.arm()
                saved = json.loads(progress.path.read_text(encoding="utf-8"))["async_control"]
                self.assertEqual(saved["arm_result"]["type"], type(value).__name__)
                self.assertEqual(saved["arm_failure_state"], state)
                if value == -2146826273:
                    self.assertIn("#VALUE! (-2146826273)", str(caught.exception))
                if value == -2146826259:
                    self.assertIn("#NAME? (-2146826259)", str(caught.exception))
                session.app.Run.assert_called_once()
                session.app.Evaluate.assert_not_called()

    def test_async_arm_preserves_com_error_when_state_observation_also_fails(self):
        with tempfile.TemporaryDirectory() as directory:
            session = Mock(control_root=Path(directory))
            original = RuntimeError("COM invocation failed")
            session.app.Run.side_effect = original
            control = AsyncControl(session, 100)
            (control.path / "control-error.txt").write_text("state write failed", encoding="utf-8")
            with self.assertRaises(RuntimeError) as caught:
                control.arm()
            self.assertIs(caught.exception, original)
            self.assertEqual(control.diagnostics["arm_error"], "RuntimeError: COM invocation failed")
            self.assertIn("state write failed", control.diagnostics["arm_failure_state"]["observation_error"])
            session.app.Run.assert_called_once()

    def test_async_gate_releases_existing_calls_without_recalculation(self):
        for id in ("A03", "A04"):
            with self.subTest(id=id), tempfile.TemporaryDirectory() as directory:
                session = Mock()
                session.control_root = Path(directory)
                target = SimpleNamespace(Value2=((1.0,), (2.0,), (3.0,)))
                session.app.Calculate.side_effect = AssertionError(
                    "recalculation while native async handles are pending")
                control_path = None

                def invoke(name, directory, expected):
                    nonlocal control_path
                    self.assertIs(threading.current_thread(), threading.main_thread())
                    self.assertEqual(name, "BENCH.ASYNC.ARM")
                    self.assertEqual(expected, 3)
                    self.assertIsNone(control_path, "ARM must be invoked only once")
                    control_path = Path(directory)
                    return 1.0

                session.app.Run.side_effect = invoke
                session.app.Evaluate.side_effect = AssertionError("ARM must not use formula evaluation")

                def submit(sheet, count, make):
                    atomic_write_json(control_path / "ready.json", {"active": 3, "expected": 3})
                    deadline = time.perf_counter() + 2
                    # Simulate Excel blocking the caller in formula assignment
                    # until the independent controller releases all work.
                    while not (control_path / "release").exists():
                        self.assertLess(time.perf_counter(), deadline, "gate needs blocked COM caller")
                        time.sleep(0.001)
                    atomic_write_json(control_path / "released.json", {"active_before_release": 3})
                    atomic_write_json(control_path / "state.json", {"active": 0, "expected": 3, "started": 3, "finished": 3, "released": True, "control_error": None})
                    return target

                session.add_formulas.side_effect = submit

                def observe(actual_target, count, expected, timeout, **kwargs):
                    self.assertIs(actual_target, target)
                    self.assertTrue((control_path / "release").exists())
                    self.assertEqual(count, 3)
                    self.assertEqual(timeout, 120)
                    for i in range(count):
                        self.assertTrue(expected(float(i + 1), i))
                    return {"poll_s": 0.1, "p50_s": 0.1, "p95_s": 0.1,
                            "p99_s": 0.1, "sampled_cells": count}

                session.wait_values.side_effect = observe
                result = run_async_gate(session, Case(id, "test", {"cells": 3}))
                self.assertEqual(result["active_before_release"], 3)
                self.assertEqual(result["control_channel"], "fixture-file-v1")
                session.app.Calculate.assert_not_called()
                session.app.Evaluate.assert_not_called()
                session.app.Run.assert_called_once_with("BENCH.ASYNC.ARM", str(control_path), 3)
                make = session.add_formulas.call_args.args[2]
                self.assertEqual([make(row) for row in range(1, 4)],
                                 ["=BENCH.ASYNC(1,-1)", "=BENCH.ASYNC(2,-1)", "=BENCH.ASYNC(3,-1)"])

    def test_gate_final_snapshot_rejects_late_or_unsampled_wrong_values(self):
        session = Mock()
        session.add_formulas.return_value = SimpleNamespace(Value2=((1.0,), (99.0,), (3.0,)))
        session.wait_values.return_value = {"poll_s": 0.1}
        control = Mock(ready_at=1.0, release_at=2.0)
        control.wait_ready.return_value = {"active": 3}
        control.wait_released.return_value = {"active_before_release": 3}
        with patch("run.AsyncControl", return_value=control):
            with self.assertRaisesRegex(AssertionError, "final snapshot row 2"):
                run_async_gate(session, Case("A04", "test", {"cells": 3}))
        control.close.assert_called_once_with()

    def test_repeated_async_rejects_late_wrong_values_after_arrival_observation(self):
        session = Mock()
        session.new_book.return_value.Range.return_value.Value2 = ((4.0,), (2.0,), (6.0,))
        session.wait_values.return_value = {"sampled_cells": 3}
        with self.assertRaisesRegex(AssertionError, "1 stale values"):
            run_async_repeated(session, Case("A06", "test", {
                "cells": 3, "repetitions": 2, "delay_us": 10000}))

    def test_full_snapshot_checks_cells_between_latency_samples(self):
        values = [(float(row),) for row in range(1, 4097)]
        values[1] = (99.0,)
        with self.assertRaisesRegex(AssertionError, "row 2"):
            check_final_values(SimpleNamespace(Value2=tuple(values)), 4096,
                               lambda value, i: value == float(i + 1))

    def test_async_controller_releases_on_failed_readiness_without_passing(self):
        for evidence in (None, {"active": 2, "expected": 3}):
            with self.subTest(evidence=evidence), tempfile.TemporaryDirectory() as directory:
                control = AsyncControl(SimpleNamespace(control_root=Path(directory)), 3, timeout_s=0.02)
                if evidence is not None:
                    (control.path / "ready.json").write_text(json.dumps(evidence))
                control.start()
                with self.assertRaises((TimeoutError, AssertionError)):
                    control.wait_ready()
                self.assertTrue((control.path / "release").exists())
                control.close()

    def test_async_control_failure_rejects_even_valid_release_evidence(self):
        with tempfile.TemporaryDirectory() as directory:
            control = AsyncControl(SimpleNamespace(control_root=Path(directory)), 3)
            (control.path / "released.json").write_text(json.dumps({"active_before_release": 3}))
            (control.path / "control-error.txt").write_text("state write failed")
            with self.assertRaisesRegex(RuntimeError, "state write failed"):
                control.wait_released()
            control.close()

    def test_async_read_ignores_empty_or_partial_transient_files(self):
        with tempfile.TemporaryDirectory() as directory:
            control = AsyncControl(SimpleNamespace(control_root=Path(directory)), 3)
            (control.path / "ready.json").write_text("", encoding="utf-8")
            self.assertIsNone(control.read("ready.json"))
            (control.path / "ready.json").write_text('{"active":', encoding="utf-8")
            self.assertIsNone(control.read("ready.json"))
            (control.path / "ready.json").write_text('{"active": 3}', encoding="utf-8")
            self.assertEqual(control.read("ready.json"), {"active": 3})
            control.close()

    def test_async_drain_does_not_accept_initial_zero_active_snapshot(self):
        with tempfile.TemporaryDirectory() as directory:
            control = AsyncControl(SimpleNamespace(control_root=Path(directory)), 3)
            initial = {"active": 0, "started": 0, "finished": 0, "released": False, "expected": 3}
            drained = {"active": 0, "started": 3, "finished": 3, "released": True, "expected": 3}
            with patch.object(control, "read", side_effect=[initial, drained]) as read, patch("run.time.sleep"):
                self.assertEqual(control.wait_drained(), drained)
            self.assertEqual(read.call_count, 2)
            control.close()

    def test_async_cancel_watchdog_unblocks_a_stalled_action_and_rejects_it(self):
        with tempfile.TemporaryDirectory() as directory:
            control = AsyncControl(SimpleNamespace(control_root=Path(directory)), 3, timeout_s=0.02)
            (control.path / "ready.json").write_text(json.dumps({"active": 3, "expected": 3}))
            control.start(release_when_ready=False)
            self.assertEqual(control.wait_ready()["active"], 3)
            control.thread.join(timeout=1)
            self.assertTrue((control.path / "release").exists())
            with self.assertRaisesRegex(TimeoutError, "cancellation action"):
                control.read("state.json")
            control.close()

    def test_native_cancel_is_explicitly_unsupported_before_starting_work(self):
        session = Mock(implementation="xlfn")
        with self.assertRaisesRegex(UnsupportedCase, "user-driven.*pending"):
            run_async_cancel(session, Case("A05", "clear", {"cells": 3, "mode": "clear"}))
        session.new_book.assert_not_called()
        session.add_formulas.assert_not_called()

    def test_dna_cancel_action_precedes_gate_release_and_checks_late_results(self):
        for mode in ("recalc", "clear", "close"):
            with self.subTest(mode=mode), tempfile.TemporaryDirectory() as directory:
                session = Mock(implementation="excel_dna", control_root=Path(directory))
                target = Mock(Value2=((1.0,), (2.0,), (3.0,)))
                control_path = None
                fixture_thread = None
                acted = threading.Event()

                def invoke(name, directory, expected):
                    nonlocal control_path, fixture_thread
                    self.assertEqual(name, "BENCH.ASYNC.ARM")
                    self.assertEqual(expected, 3)
                    self.assertIsNone(control_path, "ARM must be invoked only once")
                    control_path = Path(directory)
                    def fixture():
                        deadline = time.perf_counter() + 2
                        while not (control_path / "release").exists() and time.perf_counter() < deadline:
                            time.sleep(0.001)
                        atomic_write_json(control_path / "released.json", {"active_before_release": 3})
                        atomic_write_json(control_path / "state.json", {"active": 0, "expected": 3, "started": 3, "finished": 3, "released": True, "control_error": None})
                    fixture_thread = threading.Thread(target=fixture)
                    fixture_thread.start()
                    return 1.0

                def action():
                    self.assertFalse((control_path / "release").exists())
                    self.assertTrue((control_path / "ready.json").exists())
                    acted.set()

                def submit(sheet, count, make):
                    if session.add_formulas.call_count == 1:
                        self.assertEqual(make(1), "=BENCH.ASYNC(1,-1)")
                        atomic_write_json(control_path / "state.json", {"active": 3})
                        atomic_write_json(control_path / "ready.json", {"active": 3, "expected": 3})
                    else:
                        action()
                        self.assertEqual(make(1), "=BENCH.ASYNC(4,0)")
                        target.Value2 = ((4.0,), (5.0,), (6.0,))
                    return target

                def clear():
                    action()
                    target.Value2 = ((None,), (None,), (None,))

                session.app.Run.side_effect = invoke
                session.app.Evaluate.side_effect = AssertionError("ARM must not use formula evaluation")
                session.add_formulas.side_effect = submit
                target.ClearContents.side_effect = clear
                session.close_book.side_effect = action
                session.wait_values.return_value = {"poll_s": 0.01}
                try:
                    result = run_async_cancel(session, Case("A05", mode, {"cells": 3, "mode": mode}))
                finally:
                    if fixture_thread is not None:
                        fixture_thread.join(timeout=3)
                self.assertTrue(acted.is_set())
                self.assertEqual(result["pending_before"], 3)
                self.assertEqual(result["active_after_observation"], 0)
                self.assertEqual(result["stale_cells_after_observation"], None if mode == "close" else 0)
                self.assertNotIn("task_cleanup_observed_s", result)
                session.app.Calculate.assert_not_called()

    def test_every_requested_id_has_a_case_and_primary_metric(self):
        expected = {f"S{i:02}" for i in range(1, 5)}
        expected |= {f"M{i:02}" for i in range(1, 7)}
        expected |= {f"T{i:02}" for i in range(1, 4)}
        expected |= {f"P{i:02}" for i in range(1, 5)}
        expected |= {f"A{i:02}" for i in range(1, 7)}
        expected |= {f"R{i:02}" for i in range(1, 8)}
        expected |= {f"C{i:02}" for i in range(1, 5)}
        expected |= {f"W{i:02}" for i in range(1, 4)}
        expected |= {f"L{i:02}" for i in range(1, 4)}
        expected |= {f"N{i:02}" for i in range(1, 6)}
        self.assertEqual(set(IDS), expected)
        self.assertEqual(set(PRIMARY), expected | {"N06"})
        for profile in ("full", "smoke"):
            plan = cases(profile)
            self.assertEqual(len(plan), len({case.key for case in plan}))
            self.assertEqual({case.id for case in plan}, expected)

    def test_extreme_cases(self):
        plan = {case.key: case for case in cases()}
        self.assertEqual(plan["S01/100000"].params["cells"], 100_000)
        self.assertEqual(plan["M04/1000x10"].params["rows"] * plan["M04/1000x10"].params["cols"], 10_000)
        self.assertEqual(plan["M04/100x100"].params["rows"] * plan["M04/100x100"].params["cols"], 10_000)
        self.assertEqual(plan["M04/10x1000"].params["rows"] * plan["M04/10x1000"].params["cols"], 10_000)
        self.assertEqual(plan["R01/100000"].params["topics"], 100_000)
        self.assertEqual(plan["C02/5000"].params["registrations"], 5_000)
        self.assertEqual(plan["R07/long"].params["duration_s"], 1_800)

    def test_gated_async_cases_fit_native_pending_capacity(self):
        full = {case.key: case for case in cases("full") if case.id in ("A03", "A04")}
        self.assertEqual(set(full), {"A03/100", "A03/1000", "A03/4096", "A04/burst-4096"})
        self.assertEqual(full["A03/4096"].params["cells"], 4_096)
        self.assertEqual(full["A04/burst-4096"].params["cells"], 4_096)
        for profile in ("full", "smoke"):
            for case in cases(profile):
                if case.id in ("A03", "A04"):
                    with self.subTest(case=case.key, profile=profile):
                        self.assertLessEqual(case.params["cells"], 4_096)
                        self.assertEqual(case.params["delay_us"], -1)
                if case.id == "A05":
                    with self.subTest(case=case.key, profile=profile):
                        self.assertEqual(case.params["delay_us"], -1)
                        self.assertEqual(case.params["replacement_delay_us"], 0)

    def test_formula_keys(self):
        self.assertEqual(formula("S02", 3, {"argc": 4}), "=BENCH.SUM4(3,4,5,6)")
        self.assertEqual(formula("S04", 3, {"period": 2}), "=BENCH.ERRNUM(3,2)")
        self.assertEqual(formula("R03", 3, {"cells": 10, "topics": 2}),
                         '=BENCH.RTD("topic-0",0)')
        self.assertEqual(formula("T02", 1, {"length": 8, "kind": "ja"}),
                         "=BENCH.STR.OUT(8,TRUE)")

    def test_text_retains_each_calculation_timing_and_validates_outside_measurement(self):
        for id in ("T01", "T02", "T03"):
            with self.subTest(id=id):
                events = []
                expected = 8.0 if id == "T01" else "日" * 8

                class Target:
                    @property
                    def Value2(self):
                        events.append("validate")
                        return ((expected,), (expected,))

                target = Target()
                session = Mock()
                session.add_formulas.return_value = target
                session.last_calculation_timing = {}
                durations = iter((10.0, 0.25, 0.5))

                def calculate(actual_target):
                    self.assertIs(actual_target, target)
                    events.append("calculate")
                    duration = next(durations)
                    # Reuse the map to catch records that retain a mutable view.
                    session.last_calculation_timing.clear()
                    session.last_calculation_timing.update({
                        "total_s": duration, "calculate_call_s": duration * 0.8,
                        "completion_wait_s": duration * 0.2, "completion_polls": 1,
                    })
                    return duration

                session.calculate.side_effect = calculate
                result = run_text(session, Case(id, "ja-8", {"cells": 2, "length": 8, "kind": "ja"}), 2)
                self.assertEqual(events, ["calculate", "validate", "calculate", "calculate", "validate"])
                self.assertEqual(result["recalculation_samples"], [
                    {"iteration": 1, "total_s": 0.25, "calculate_call_s": 0.2,
                     "completion_wait_s": 0.05, "completion_polls": 1},
                    {"iteration": 2, "total_s": 0.5, "calculate_call_s": 0.4,
                     "completion_wait_s": 0.1, "completion_polls": 1},
                ])
                self.assertEqual(result["elapsed_s"], 0.75)
                self.assertEqual(result["p50_s"], 0.375)
                self.assertEqual(result["ns_per_char"], 0.375 * 1e9 / 16)
                self.assertEqual(result["characters"], 16)
                self.assertEqual(result["character_unit"], "Unicode code points")
                self.assertEqual(result["formula_count"], 2)
                self.assertEqual(result["validated_cells"], 2)

    def test_text_rejects_later_wrong_or_missing_cells_after_warmup_and_measurement(self):
        for id in ("T01", "T02", "T03"):
            for phase in ("warmup", "final"):
                for failure in ("wrong", "missing", "wrong_type"):
                    with self.subTest(id=id, phase=phase, failure=failure):
                        expected = 1.0 if id == "T01" else "a"
                        target = SimpleNamespace(Value2=((expected,), (expected,)))
                        session = Mock(last_calculation_timing={})
                        session.add_formulas.return_value = target
                        calls = 0

                        def calculate(_):
                            nonlocal calls
                            calls += 1
                            if calls == (1 if phase == "warmup" else 2):
                                if failure == "missing":
                                    target.Value2 = ((expected,),)
                                else:
                                    wrong = (True if id == "T01" else None) if failure == "wrong_type" else "incorrect"
                                    target.Value2 = ((expected,), (wrong,))
                            return 0.1

                        session.calculate.side_effect = calculate
                        message = "1/2 cells" if failure == "missing" else "row 2"
                        with self.assertRaisesRegex(AssertionError, message):
                            run_text(session, Case(id, "ascii-1", {"cells": 2, "length": 1, "kind": "ascii"}), 1)
                        self.assertEqual(calls, 1 if phase == "warmup" else 2)

    def test_native_abi_pair_uses_alternating_balanced_blocks(self):
        session = Mock(implementation="xlfn")
        session.last_calculation_timing = {"total_s": 1.0}
        q = SimpleNamespace(Value2=((1.0,), (2.0,)))
        native = SimpleNamespace(Value2=((1.0,), (2.0,)))
        session.add_formulas.side_effect = [q, native]
        session.calculate.side_effect = [0.1, 0.1, 1, 2, 4, 3, 6, 5, 7, 8]
        case = Case("N01", "f64-1", {"cells": 2, "argc": 1})
        result = run_native_abi_pair(session, case, 2)
        self.assertEqual([sample["abi"] for sample in result["recalculation_samples"]],
                         ["Q", "NATIVE", "NATIVE", "Q", "NATIVE", "Q", "Q", "NATIVE"])
        self.assertEqual(result["q_samples_s"], [1, 3, 5, 7])
        self.assertEqual(result["native_samples_s"], [2, 4, 6, 8])
        self.assertEqual(result["native_over_q_ratio"], 1.25)
        self.assertEqual(result["native_change_percent"], 25)
        self.assertEqual(native_abi_formula(case, 3, "Q"), "=BENCH.ABI.F641.Q(3)")
        self.assertEqual(native_abi_formula(Case("N04", "text", {}), 3, "NATIVE"),
                         "=BENCH.ABI.STR.NATIVE(B3)")

    def test_native_abi_pair_rejects_cell_errors_and_other_implementations(self):
        case = Case("N01", "f64-1", {"cells": 1, "argc": 1})
        with self.assertRaises(UnsupportedCase):
            run_native_abi_pair(Mock(implementation="excel_dna"), case, 1)
        session = Mock(implementation="xlfn")
        session.add_formulas.return_value = SimpleNamespace(Value2=-2146826259)
        with self.assertRaisesRegex(AssertionError, "unexpected worksheet result"):
            run_native_abi_pair(session, case, 1)

    def test_semantics_profile_is_separate_from_performance_workloads(self):
        self.assertEqual([case.id for case in cases("semantics")], ["N06"])
        case = cases("semantics")[0]
        self.assertEqual(artifact(Path("root"), "xlfn", case), Path("root/xlfn/abi-semantics.xll"))
        for profile in ("full", "smoke"):
            self.assertNotIn("N06", {case.id for case in cases(profile)})
        with self.assertRaisesRegex(AssertionError, "instrumented XLL"):
            session = Mock(implementation="xlfn")
            session.app.Evaluate.return_value = 0
            run_native_abi_semantics(session)

    def test_semantics_distinguishes_input_rejection_from_unobserved_admission(self):
        counts = {"entered": 0, "body": 0, "input_errors": 0, "other_errors": 0}
        self.assertEqual(classify_abi_semantics(counts, XL_ERR_NUM), "no_observed_layer_entry")
        counts.update(entered=1, input_errors=1)
        self.assertEqual(classify_abi_semantics(counts, XL_ERR_NUM), "rust_input_rejection")
        counts.update(input_errors=0, body=1)
        self.assertEqual(classify_abi_semantics(counts, 42.0), "rust_body_completed")
        counts.update(other_errors=1)
        with self.assertRaisesRegex(AssertionError, "unexpected instrumented outcome"):
            classify_abi_semantics(counts, XL_ERR_NUM)
        with self.assertRaisesRegex(AssertionError, "without observed layer"):
            classify_abi_semantics({key: 0 for key in counts}, 42.0)

    def test_native_abi_summaries_preserve_signed_change_and_semantics(self):
        records = [
            {"id": "N01", "variant": "f64-1", "implementation": "xlfn", "status": "ok",
             "metrics": {"q_p50_s": 1.0, "native_p50_s": 1.25,
                         "native_over_q_ratio": 1.25, "native_change_percent": 25.0}},
            {"id": "N06", "variant": "input-semantics", "implementation": "xlfn", "status": "ok",
             "metrics": {"checks": 152}},
        ]
        timing, qualification = summarize(records)
        self.assertEqual(timing["native_change_percent"], 25.0)
        self.assertFalse(timing["comparable"])
        self.assertEqual(qualification["comparison"], "input_semantics")
        self.assertEqual(qualification["checks"], 152)
        self.assertNotIn("q_p50_s", qualification)

    def test_repeat_observations_include_first_recalculation_and_phase_times(self):
        for tracking in (False, True):
            session = Mock(repeat_diagnostics=tracking, implementation="xlfn")
            session.add_formulas.return_value.Value2 = ((1.0,),)
            session.memory_snapshot.side_effect = [
                {"rss_bytes": 100, "private_bytes": 200},
                {"rss_bytes": 150, "private_bytes": 280},
                {"rss_bytes": 160, "private_bytes": 290}]
            durations = iter((0.5, 1.0, 2.0))
            def calculate(target, full=False):
                duration = next(durations)
                session.last_calculation_timing = {"total_s": duration,
                    "calculate_call_s": duration - 0.1, "completion_wait_s": 0.1,
                    "completion_polls": 1}
                return duration
            session.calculate.side_effect = calculate
            counters = iter((100, 50, 400, 250))
            def evaluate(expression):
                if expression == "BENCH.ALLOC.TRACK(TRUE)": return 1
                if expression == "BENCH.ALLOC.TRACK(FALSE)": return 0
                return next(counters)
            session.app.Evaluate.side_effect = evaluate
            result = run_scalar(session, Case("L02", "test", {"cells": 1}), 2)
            self.assertEqual(session.calculate.call_args_list,
                [call(session.add_formulas.return_value, full=True)] * 3)
            self.assertEqual(result["warmup_calculation"], "CalculateFull")
            self.assertEqual(result["rss_growth_bytes"], 10)
            self.assertEqual(result["post_warmup_rss_bytes_growth"], 60)
            self.assertEqual(result["post_warmup_private_bytes_growth"], 90)
            self.assertEqual(result["recalculation_samples"][1]["calculate_call_s"], 1.9)
            self.assertEqual(result["recalculation_blocks"][1]["p99_s"], 2.0)
            if tracking:
                self.assertEqual(result["fixture_allocation_window"],
                    {"allocated_bytes": 300, "freed_bytes": 200, "net_requested_bytes": 100})
            else:
                session.app.Evaluate.assert_not_called()

    def test_repeat_tracking_disabled_on_counter_and_calculation_failures(self):
        for failure in ("enable", "counter", "calculate"):
            session = Mock(repeat_diagnostics=True, implementation="xlfn")
            session.add_formulas.return_value.Value2 = ((1.0,),)
            session.calculate.side_effect = [0.1, RuntimeError("calculation failed")]
            def evaluate(expression):
                if expression == "BENCH.ALLOC.TRACK(FALSE)": return 0
                if expression == "BENCH.ALLOC.TRACK(TRUE)": return -1 if failure == "enable" else 1
                if failure == "counter": raise RuntimeError("counter failed")
                return 100
            session.app.Evaluate.side_effect = evaluate
            with self.assertRaises((AssertionError, RuntimeError)):
                run_scalar(session, Case("L02", "test", {"cells": 1}), 1)
            session.app.Evaluate.assert_called_with("BENCH.ALLOC.TRACK(FALSE)")

    def test_repeat_counter_scope_and_invalid_excel_values(self):
        session = Mock(implementation="excel_dna")
        session.app.Evaluate.return_value = 100
        self.assertEqual(allocation_snapshot(session), {"allocated_bytes": 100})
        session.app.Evaluate.assert_called_once_with("BENCH.ALLOC.BYTES()")
        for value in (True, XL_ERR_NUM, "#NAME?", float("nan")):
            session.app.Evaluate.return_value = value
            with self.assertRaises(AssertionError):
                allocation_snapshot(session)

    def test_repeat_diagnostics_are_excluded_from_framework_ratios(self):
        for tracked in ((True, False), (True, True)):
            records = [{"id": "L02", "variant": "1000", "implementation": impl,
                        "repeat_diagnostics": enabled, "status": "ok",
                        "metrics": {"latency_drift_ratio": 1.0}}
                       for impl, enabled in zip(("xlfn", "excel_dna"), tracked)]
            row, = summarize(records)
            self.assertFalse(row["comparable"])
            self.assertIsNone(row["xlfn_advantage_ratio"])
            self.assertIn("diagnostic", row["reason"])

    def test_memory_snapshot_does_not_substitute_vms_for_private_commit(self):
        session = ExcelSession.__new__(ExcelSession)
        session.process = Mock()
        session.peak_rss = 0
        session.process.memory_info.return_value = SimpleNamespace(rss=10, vms=30)
        self.assertEqual(session.memory_snapshot(), {"rss_bytes": 10})
        session.process.memory_info.return_value = SimpleNamespace(rss=20, private=40)
        self.assertEqual(session.memory_snapshot(), {"rss_bytes": 20, "private_bytes": 40})
        self.assertEqual(session.peak_rss, 20)

    def test_matrix_allocation_tracking_is_limited_to_mixed_measurement(self):
        for id in ("M01", "M05"):
            with self.subTest(id=id):
                session = Mock()
                anchor = SimpleNamespace(Value2=15.0 if id == "M05" else 10.0)
                input_area = SimpleNamespace(Address="$B$1:$F$1")
                session.new_book.return_value.Range.side_effect = (
                    lambda *args: anchor if args == ("A1",) else input_area)
                events = []

                def calculate(target):
                    events.append("calculate")
                    session.last_calculation_timing = {
                        "total_s": 0.1, "calculate_call_s": 0.08,
                        "completion_wait_s": 0.02, "completion_polls": 1,
                    }
                    return 0.1

                reads = iter((100.0, 300.0))

                def evaluate(expression):
                    events.append(expression)
                    if expression == "BENCH.ALLOC.TRACK(TRUE)":
                        return 1.0
                    if expression == "BENCH.ALLOC.TRACK(FALSE)":
                        return 0.0
                    self.assertEqual(expression, "BENCH.ALLOC.BYTES()")
                    return next(reads)

                session.calculate.side_effect = calculate
                session.app.Evaluate.side_effect = evaluate
                result = run_matrix(session, Case(id, "test", {"rows": 1, "cols": 5}), 2)
                self.assertEqual(result["formula_count"], 1)
                self.assertEqual(len(result["recalculation_samples"]), 2)
                self.assertEqual(result["recalculation_samples"][0]["completion_wait_s"], 0.02)
                if id == "M05":
                    self.assertEqual(events, ["calculate", "BENCH.ALLOC.TRACK(TRUE)",
                                             "BENCH.ALLOC.BYTES()", "calculate", "calculate",
                                             "BENCH.ALLOC.BYTES()", "BENCH.ALLOC.TRACK(FALSE)"])
                    self.assertEqual(result["fixture_allocated_bytes"], 200.0)
                    self.assertEqual(result["fixture_allocated_bytes_per_element_per_recalc"], 20.0)
                else:
                    self.assertEqual(events, ["calculate"] * 3)
                    self.assertNotIn("fixture_allocated_bytes", result)

    def test_matrix_allocation_tracking_stops_after_measurement_errors(self):
        for failure in ("enable", "before", "calculate", "verify", "after"):
            with self.subTest(failure=failure):
                session = Mock()
                anchor = SimpleNamespace(Value2=15.0)
                input_area = SimpleNamespace(Address="$B$1:$F$1")
                session.new_book.return_value.Range.side_effect = (
                    lambda *args: anchor if args == ("A1",) else input_area)
                reads = 0

                def evaluate(expression):
                    nonlocal reads
                    if expression == "BENCH.ALLOC.TRACK(TRUE)":
                        if failure == "enable":
                            return -1.0
                        return 1.0
                    if expression == "BENCH.ALLOC.TRACK(FALSE)":
                        return 0.0
                    reads += 1
                    if (reads == 1 and failure == "before") or (reads == 2 and failure == "after"):
                        raise RuntimeError("allocation read failed")
                    return 100.0

                def measured_calculation(target):
                    if failure == "calculate":
                        raise RuntimeError("calculation failed")
                    if failure == "verify":
                        anchor.Value2 = 0.0
                    return 0.1

                session.calculate.side_effect = lambda target: (
                    0.1 if session.calculate.call_count == 1 else measured_calculation(target))
                session.app.Evaluate.side_effect = evaluate
                with self.assertRaises((AssertionError, RuntimeError)):
                    run_matrix(session, Case("M05", "test", {"rows": 1, "cols": 5}), 1)
                session.app.Evaluate.assert_called_with("BENCH.ALLOC.TRACK(FALSE)")

    def test_error_workload_checks_each_cell(self):
        class Target:
            def __init__(self, values):
                self.Value2 = tuple((value,) for value in values)

        case = Case("S04", "period-2", {"cells": 4, "period": 2})
        check_scalar(Target([1.0, XL_ERR_NUM, 3.0, XL_ERR_NUM]), case)
        with self.assertRaises(AssertionError):
            check_scalar(Target([XL_ERR_NUM, 2.0, XL_ERR_NUM, 4.0]), case)
        with self.assertRaises(AssertionError):
            check_scalar(Target([-2146826259] * 4), Case("S04", "period-0", {"cells": 4, "period": 0}))

    def test_scalar_checks_reject_excel_errors_and_wrong_values(self):
        for id in ("S01", "S03", "P01", "P02", "P03", "P04", "W03", "L02"):
            case = Case(id, "test", {"cells": 3})
            with self.subTest(id=id):
                check_scalar(SimpleNamespace(Value2=((1.0,), (2.0,), (3.0,))), case)
                for values in ((-2146826259,) * 3, (1.0, -2146826259, 3.0),
                               (1.0, float("nan"), 3.0), (True, 2.0, 3.0)):
                    with self.assertRaises(AssertionError):
                        check_scalar(SimpleNamespace(Value2=tuple((v,) for v in values)), case)
        with self.assertRaisesRegex(AssertionError, "row 2: expected 2, got 99"):
            check_scalar(SimpleNamespace(Value2=((1.0,), (99,), (3.0,))),
                         Case("S01", "test", {"cells": 3}))

    def test_multi_argument_expected_sums(self):
        for argc in (2, 4, 8):
            case = Case("S02", str(argc), {"cells": 3, "argc": argc})
            check_scalar(SimpleNamespace(Value2=tuple(
                (float(sum(range(row, row + argc))),) for row in range(1, 4))), case)
            with self.assertRaises(AssertionError):
                check_scalar(SimpleNamespace(Value2=((1.0,), (2.0,), (3.0,))), case)

    def test_xll_load_failure_stops_before_benchmark(self):
        app = SimpleNamespace(RegisterXLL=Mock(return_value=False))
        with self.assertRaisesRegex(RuntimeError, "RegisterXLL returned False"):
            register_xll(app, Path("benchmark.xll"))
        app.RegisterXLL.assert_called_once_with(str(Path("benchmark.xll").resolve()))
        app.RegisterXLL.return_value = True
        register_xll(app, Path("benchmark.xll"))

    def test_registration_diagnostics_preserves_value_types_without_calling_udfs(self):
        app = SimpleNamespace(RegisteredFunctions=(("fixture.xll", "xll_identity", "QQ$"),),
                              Evaluate=Mock(side_effect=[-123.0, 456.0, -123.0, -2146826259, 789.0]))
        result = registration_diagnostics(app)
        self.assertEqual(result["name_bindings"]["BENCH.ALLOC.BYTES"], {"type": "float", "value": -123.0})
        self.assertEqual(result["name_bindings"]["BENCH.ERRNUM"]["type"], "int")
        self.assertEqual(result["name_bindings"]["BENCH.ASYNC.ARM"], {"type": "float", "value": 789.0})
        for call in app.Evaluate.call_args_list:
            self.assertNotIn("(", call.args[0])
        app.Evaluate.side_effect = RuntimeError("probe unavailable")
        self.assertEqual(registration_diagnostics(app)["name_bindings"]["BENCH.ID"],
                         {"error": "probe unavailable"})

    def test_registration_names_match_rust_generator(self):
        build_script = Path(__file__).parent / "xlfn" / "build.rs"
        with tempfile.TemporaryDirectory(prefix="xlfn-registration-") as directory:
            output = Path(directory)
            executable = output / ("registration-builder.exe" if os.name == "nt"
                                   else "registration-builder")
            compiled = subprocess.run(
                ["rustc", "--edition=2024", str(build_script), "-o", str(executable)],
                capture_output=True, text=True, timeout=60,
            )
            self.assertEqual(compiled.returncode, 0, compiled.stderr)
            for count in (0, 10, 5_000):
                with self.subTest(count=count):
                    generated = subprocess.run(
                        [str(executable)],
                        env=os.environ | {"OUT_DIR": directory, "BENCH_EXTRA_FUNCTIONS": str(count)},
                        capture_output=True, text=True, timeout=10,
                    )
                    self.assertEqual(generated.returncode, 0, generated.stderr)
                    rust_names = re.findall(r'name = "(BENCH\.EXTRA\.\d+)"',
                                            (output / "extra.rs").read_text(encoding="utf-8"))
                    csharp_names = re.findall(r'Name = "(BENCH\.EXTRA\.\d+)"', source(count))
                    self.assertEqual(rust_names, [f"BENCH.EXTRA.{i:04}" for i in range(count)])
                    self.assertEqual(csharp_names, rust_names)
            for count in (-1, 5_001):
                with self.subTest(rejected_count=count):
                    with self.assertRaises(ValueError):
                        source(count)
                    rejected = subprocess.run(
                        [str(executable)],
                        env=os.environ | {"OUT_DIR": directory, "BENCH_EXTRA_FUNCTIONS": str(count)},
                        capture_output=True, text=True, timeout=10,
                    )
                    self.assertNotEqual(rejected.returncode, 0)

    def test_summary_requires_matching_conditions(self):
        common = {"id": "S01", "variant": "1000", "status": "ok", "profile": "full",
                  "params": {"cells": 1_000}, "threads": 1, "rtd_throttle_ms": 100,
                  "excel_version": "16.0", "excel_build": "1",
                  "artifact_source": "github-actions", "artifact_architecture": "x86_64",
                  "ci_commit": "abc123", "ci_run_id": "42", "runner_commit": "abc123"}
        left = common | {"implementation": "xlfn", "metrics": {"p50_s": 1.0}}
        right = common | {"implementation": "excel_dna", "metrics": {"p50_s": 2.0}}
        self.assertEqual(summarize([left, right])[0]["xlfn_advantage_ratio"], 2.0)
        right["excel_build"] = "2"
        self.assertFalse(summarize([left, right])[0]["comparable"])
        right["excel_build"] = "1"
        right["ci_run_id"] = "43"
        self.assertFalse(summarize([left, right])[0]["comparable"])

    def test_summary_rejects_mixed_capture_modes_sources_and_full_lifecycle_tracing(self):
        common = {"id": "S01", "variant": "1000", "status": "ok", "dump_on_failure": True,
                  "shutdown_diagnostics": False, "runner_sources": {"run.py": "same-hash"}}
        left = common | {"implementation": "xlfn", "metrics": {"p50_s": 1.0}}
        right = common | {"implementation": "excel_dna", "metrics": {"p50_s": 2.0}}
        self.assertTrue(summarize([left, right])[0]["comparable"])
        for change in ({"dump_on_failure": False}, {"runner_sources": {"run.py": "different-hash"}},
                       {"shutdown_diagnostics": True}, {"status": "error", "execution_status": "ok"}):
            with self.subTest(change=change):
                self.assertFalse(summarize([left, right | change])[0]["comparable"])
        traced = [r | {"shutdown_diagnostics": True} for r in (left, right)]
        self.assertIn("lifecycle-instrumented", summarize(traced)[0]["reason"])
        # Older diagnostic records lacked the explicit mode flag.
        for field in ("lifecycle_trace", "process_exit_observation"):
            legacy = [{k: v for k, v in r.items() if k != "shutdown_diagnostics"} | {field: {}}
                      for r in (left, right)]
            self.assertFalse(summarize(legacy)[0]["comparable"])

    def test_unsupported_cancellation_cannot_produce_a_comparison(self):
        native = {"id": "A05", "variant": "clear", "implementation": "xlfn",
                  "status": "unsupported", "unsupported_reason": "user-driven cancellation required"}
        dna = {"id": "A05", "variant": "clear", "implementation": "excel_dna",
               "status": "ok", "metrics": {"task_drain_after_release_s": 0.1}}
        row = summarize([native, dna])[0]
        self.assertEqual(row["xlfn_status"], "unsupported")
        self.assertFalse(row["comparable"])
        self.assertIsNone(row["xlfn_advantage_ratio"])
        self.assertIn("user-driven cancellation required", row["reason"])

    def test_ci_artifact_manifest_requires_complete_matching_x64_set(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            image = bytearray(0x86)
            image[:2] = b"MZ"
            struct.pack_into("<I", image, 0x3C, 0x80)
            image[0x80:0x84] = b"PE\0\0"
            struct.pack_into("<H", image, 0x84, 0x8664)
            for relative in FILES:
                path = root / relative
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(image)
            create(root, "abc123", "42", "1")
            manifest = verify(root, run_id="42", commit="abc123")
            self.assertEqual(manifest["architecture"], "x86_64")
            self.assertEqual(manifest["excel_dna_compilation"], "NativeAOT")
            with self.assertRaises(ValueError):
                verify(root, run_id="43")
            (root / FILES[0]).write_bytes(b"changed")
            with self.assertRaises(ValueError):
                verify(root)


if __name__ == "__main__":
    unittest.main()

import json
import re
import struct
import tempfile
import threading
import time
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import Mock, patch

from artifact_manifest import FILES, create, verify
from generate_registration import source
from run import (XL_AUTOMATIC, XL_MANUAL, XL_ERR_NUM, AsyncControl, ComObject, ComRetry,
                 ExcelSession, UnsupportedCase, WorkerProgress, calculation_mode,
                 check_final_values, check_scalar, close_worker_session, formula, register_xll,
                 registration_diagnostics, run_async, reap_excel_process,
                 recover_worker_record, run_async_cancel, run_async_gate, run_async_repeated, run_matrix)
from summarize import PRIMARY, summarize
from workloads import IDS, Case, cases


class PlanTest(unittest.TestCase):
    def test_teardown_failure_survives_successful_fallback_quit(self):
        session = object.__new__(ExcelSession)
        session.progress = None
        session._stop_sampler = Mock()
        session._sampler = Mock()
        session.close_book = Mock(side_effect=RuntimeError("workbook close failed"))
        session.app = Mock()
        with self.assertRaisesRegex(RuntimeError, "workbook close failed"):
            session.close()
        session.app.Quit.assert_called_once_with()
        self.assertEqual(session.last_stage, "close_failed")
        session.app.Quit.side_effect = RuntimeError("quit also failed")
        with self.assertRaisesRegex(RuntimeError, "workbook close failed.*quit also failed"):
            session.close()

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

    def test_async_gate_releases_existing_calls_without_recalculation(self):
        for id in ("A03", "A04"):
            with self.subTest(id=id), tempfile.TemporaryDirectory() as directory:
                session = Mock()
                session.control_root = Path(directory)
                target = SimpleNamespace(Value2=((1.0,), (2.0,), (3.0,)))
                session.app.Calculate.side_effect = AssertionError(
                    "recalculation while native async handles are pending")
                control_path = None

                def evaluate(expression):
                    nonlocal control_path
                    self.assertIs(threading.current_thread(), threading.main_thread())
                    match = re.fullmatch(r'BENCH.ASYNC.ARM\("(.+)",3\)', expression)
                    self.assertIsNotNone(match)
                    control_path = Path(match[1])
                    return 1.0

                session.app.Evaluate.side_effect = evaluate

                def submit(sheet, count, make):
                    (control_path / "ready.json").write_text(json.dumps({"active": 3, "expected": 3}))
                    deadline = time.perf_counter() + 2
                    # Simulate Excel blocking the caller in formula assignment
                    # until the independent controller releases all work.
                    while not (control_path / "release").exists():
                        self.assertLess(time.perf_counter(), deadline, "gate needs blocked COM caller")
                        time.sleep(0.001)
                    (control_path / "released.json").write_text(json.dumps({"active_before_release": 3}))
                    (control_path / "state.json").write_text(json.dumps({"active": 0, "expected": 3, "started": 3, "finished": 3, "released": True, "control_error": None}))
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
                self.assertEqual(session.app.Evaluate.call_count, 1)
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

                def evaluate(expression):
                    nonlocal control_path, fixture_thread
                    match = re.fullmatch(r'BENCH.ASYNC.ARM\("(.+)",3\)', expression)
                    self.assertIsNotNone(match)
                    control_path = Path(match[1])
                    def fixture():
                        deadline = time.perf_counter() + 2
                        while not (control_path / "release").exists() and time.perf_counter() < deadline:
                            time.sleep(0.001)
                        (control_path / "released.json").write_text(json.dumps({"active_before_release": 3}))
                        (control_path / "state.json").write_text(json.dumps({"active": 0, "expected": 3, "started": 3, "finished": 3, "released": True, "control_error": None}))
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
                        (control_path / "state.json").write_text(json.dumps({"active": 3}))
                        (control_path / "ready.json").write_text(json.dumps({"active": 3, "expected": 3}))
                    else:
                        action()
                        self.assertEqual(make(1), "=BENCH.ASYNC(4,0)")
                        target.Value2 = ((4.0,), (5.0,), (6.0,))
                    return target

                def clear():
                    action()
                    target.Value2 = ((None,), (None,), (None,))

                session.app.Evaluate.side_effect = evaluate
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
        expected |= {f"M{i:02}" for i in range(1, 6)}
        expected |= {f"T{i:02}" for i in range(1, 4)}
        expected |= {f"P{i:02}" for i in range(1, 5)}
        expected |= {f"A{i:02}" for i in range(1, 7)}
        expected |= {f"R{i:02}" for i in range(1, 8)}
        expected |= {f"C{i:02}" for i in range(1, 5)}
        expected |= {f"W{i:02}" for i in range(1, 4)}
        expected |= {f"L{i:02}" for i in range(1, 4)}
        self.assertEqual(set(IDS), expected)
        self.assertEqual(set(PRIMARY), expected)
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
                              Evaluate=Mock(side_effect=[-123.0, 456.0, -123.0, -2146826259]))
        result = registration_diagnostics(app)
        self.assertEqual(result["name_bindings"]["BENCH.ALLOC.BYTES"], {"type": "float", "value": -123.0})
        self.assertEqual(result["name_bindings"]["BENCH.ERRNUM"]["type"], "int")
        for call in app.Evaluate.call_args_list:
            self.assertNotIn("(", call.args[0])
        app.Evaluate.side_effect = RuntimeError("probe unavailable")
        self.assertEqual(registration_diagnostics(app)["name_bindings"]["BENCH.ID"],
                         {"error": "probe unavailable"})

    def test_registration_names_match_rust_generator(self):
        generated = source(10)
        self.assertIn('Name = "BENCH.EXTRA.0000"', generated)
        self.assertIn('Name = "BENCH.EXTRA.0009"', generated)
        self.assertNotIn('BENCH.EXTRA.0010', generated)
        with self.assertRaises(ValueError):
            source(5_001)

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

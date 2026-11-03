from __future__ import annotations

import json
from pathlib import Path
import subprocess
import sys
import tempfile
import threading
import unittest
from unittest.mock import patch
from unittest.mock import Mock

from shutdown_observation import (ProcessExitObserver, atomic_json, await_observer, clock_stamp,
                                  collect_lifecycle_trace, prepare_lifecycle_trace, summarize_exit)
from shutdown_diagnostics import process_creation_ticks
from run import ExcelSession


class ExitObservationTest(unittest.TestCase):
    def test_fallback_quit_preserves_first_start_and_disabled_diagnostics_do_not_sample(self):
        session = object.__new__(ExcelSession)
        session.progress = None
        session.shutdown_diagnostics = False
        with patch("run.clock_stamp") as stamp:
            session.mark_shutdown("quit_begin")
            stamp.assert_not_called()
        session.shutdown_diagnostics = True
        session._stop_sampler = Mock()
        session._sampler = Mock()
        session.book = None
        session.bootstrap_book = Mock()
        session.app = app = Mock()
        session.original_throttle = session.original_threads = session.original_thread_mode = 1
        session.original_mtr_enabled = True
        session.original_calculation = -4135
        app.Quit.side_effect = [RuntimeError("first Quit failed"), None]
        counter = iter(range(100, 200))
        with patch("run.clock_stamp", side_effect=lambda: {"qpc_ticks": next(counter), "qpc_frequency": 1}):
            with self.assertRaisesRegex(RuntimeError, "first Quit failed"):
                session.close()
        self.assertEqual(session.shutdown_timing["quit_begin"]["qpc_ticks"], 100)
        self.assertEqual(session.shutdown_timing["quit_call_finished"]["qpc_ticks"], 102)
        self.assertIsNone(session.app)
        self.assertIn("gc_finished", session.shutdown_timing)

    def test_handle_is_acquired_before_work_and_exit_is_observed_while_worker_still_busy(self):
        signaled = threading.Event()
        times_read = threading.Event()
        released = threading.Event()
        class Handle:
            def __init__(self, pid, **kwargs):
                self.pid = pid
            def __enter__(self): return self
            def __exit__(self, *_): released.set()
            def creation_ticks(self): return 1234
            def wait(self, timeout): return signaled.wait(timeout / 1000)
            def times(self):
                if not signaled.is_set(): raise AssertionError("exit time read before signal")
                times_read.set()
                return 1234, 9000
            def exit_code(self): return 0
        with tempfile.TemporaryDirectory() as directory, patch("shutdown_observation.ProcessHandle", Handle):
            pid_file = Path(directory) / "excel.pid"
            observer = ProcessExitObserver(pid_file).start()
            identity = {"pid": 42, "creation_ticks": 1234}
            atomic_json(pid_file, identity)
            try:
                self.assertEqual(await_observer(pid_file, identity)["status"], "watching")
                signaled.set()
                # Simulates Excel exit while Python is blocked releasing COM.
                self.assertTrue(times_read.wait(2))
            finally:
                result = observer.finish()
            self.assertEqual(result["status"], "exited")
            self.assertEqual(result["exit_filetime_ticks"], 9000)
            self.assertTrue(released.is_set())

    def test_reused_pid_never_waits_or_reads_exit_time(self):
        class WrongHandle:
            def __init__(self, *args, **kwargs): pass
            def __enter__(self): return self
            def __exit__(self, *_): pass
            def creation_ticks(self): return 99
            def wait(self, *_): raise AssertionError("reused process observed")
        with tempfile.TemporaryDirectory() as directory, patch("shutdown_observation.ProcessHandle", WrongHandle):
            path = Path(directory) / "excel.pid"
            identity = {"pid": 42, "creation_ticks": 1}
            atomic_json(path, identity)
            observer = ProcessExitObserver(path).start()
            try:
                result = await_observer(path, identity)
                self.assertEqual(result["status"], "error")
                self.assertIn("PID was reused", result["error"])
            finally:
                observer.finish()

    def test_no_identity_can_stop_without_leaking_an_observer(self):
        with tempfile.TemporaryDirectory() as directory:
            observer = ProcessExitObserver(Path(directory) / "missing.pid").start()
            self.assertEqual(observer.finish()["status"], "unavailable")
            self.assertFalse(observer.thread.is_alive())

    def test_common_clock_includes_release_time_and_keeps_forced_death_distinct(self):
        record = {"shutdown_timing": {"quit_begin": {"filetime_ticks": 100_000_000,
            "qpc_ticks": 1000, "qpc_frequency": 100}}}
        observation = {"status": "exited", "exit_code": 0, "exit_filetime_ticks": 700_000_000,
                       "exit_observed": {"qpc_ticks": 7001, "qpc_frequency": 100}}
        result = summarize_exit(record, observation, {"forced_kill": False})
        self.assertEqual(result["quit_to_natural_exit_s"], 60)
        self.assertEqual(result["quit_to_exit_observed_s"], 60.01)
        forced = summarize_exit(record, observation, {"forced_kill": True, "exit_timeout_s": 5})
        self.assertEqual(forced["termination"], "forced")
        self.assertNotIn("quit_to_natural_exit_s", forced)
        self.assertTrue(forced["parent_exit_deadline_missed"])
        during_dump = summarize_exit(record, observation, {"forced_kill": False, "exit_timeout_s": 5})
        self.assertTrue(during_dump["parent_exit_deadline_missed"])
        bad_exit = summarize_exit(record, observation | {"exit_code": 1}, {})
        self.assertNotIn("quit_to_natural_exit_s", bad_exit)
        self.assertEqual(bad_exit["termination"], "unforced_nonzero_exit")

    def test_missing_quit_or_clock_reversal_never_becomes_a_natural_latency(self):
        self.assertIn("measurement_error", summarize_exit({}, {"status": "exited"}, {}))
        result = summarize_exit({"shutdown_timing": {"quit_begin": {
            "filetime_ticks": 200, "qpc_ticks": 50, "qpc_frequency": 1}}},
            {"status": "exited", "exit_code": 0, "exit_filetime_ticks": 100,
             "exit_observed": {"qpc_ticks": 60, "qpc_frequency": 1}}, {})
        self.assertIn("measurement_error", result)
        self.assertNotIn("quit_to_natural_exit_s", result)

    @unittest.skipUnless(sys.platform == "win32", "native Windows retained process handle")
    def test_native_exit_remains_readable_after_the_worker_process_exits(self):
        child = subprocess.Popen([sys.executable, "-c", "import sys; print('ready', flush=True); sys.stdin.read()"],
                                 stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
        observer = None
        try:
            self.assertEqual(child.stdout.readline().strip(), "ready")
            with tempfile.TemporaryDirectory() as directory:
                path = Path(directory) / "excel.pid"
                identity = {"pid": child.pid, "creation_ticks": process_creation_ticks(child.pid)}
                atomic_json(path, identity)
                observer = ProcessExitObserver(path).start()
                self.assertEqual(await_observer(path, identity)["status"], "watching")
                begin = clock_stamp()
                child.stdin.close()
                child.wait(timeout=5)
                result = observer.finish()
                self.assertEqual(result["status"], "exited", result)
                self.assertEqual(result["exit_code"], 0)
                self.assertGreaterEqual(result["exit_filetime_ticks"], begin["filetime_ticks"])
        finally:
            if child.poll() is None:
                child.kill()
                child.wait(timeout=5)
            if observer is not None: observer.finish()
            child.stdout.close()
            if not child.stdin.closed: child.stdin.close()


class LifecycleTraceTest(unittest.TestCase):
    def test_nested_close_return_does_not_hide_a_stuck_outer_callback(self):
        with tempfile.TemporaryDirectory() as directory, patch.dict("os.environ", {"LOCALAPPDATA": directory}):
            config = prepare_lifecycle_trace({"pid": 42, "creation_ticks": 123}, Path(directory) / "events.jsonl")
            base = {key: config[key] for key in ("pid", "creation_ticks", "token")}
            base.update(schema=1, source="xlfn", qpc_ticks=150, qpc_frequency=100)
            events = [base | {"event": name, "details": {"invocation": invocation}}
                      for name, invocation in [("xlAutoClose.enter", 1), ("xlAutoClose.enter", 2), ("xlAutoClose.return", 2)]]
            Path(config["path"]).write_text("\n".join(json.dumps(e) for e in events))
            result = collect_lifecycle_trace(config, None)
            self.assertEqual(result["auto_close"], "entered_without_return")
            self.assertEqual(result["unreturned_invocations"], [1])

    def test_trace_identity_partial_tail_and_callback_scope(self):
        for source, boundary in [("xlfn", "xlAutoClose"), ("excel_dna", "IExcelAddIn.AutoClose")]:
            with self.subTest(source=source), tempfile.TemporaryDirectory() as directory, patch.dict("os.environ", {"LOCALAPPDATA": directory}):
                config = prepare_lifecycle_trace({"pid": 42, "creation_ticks": 123}, Path(directory) / "events.jsonl")
                base = {key: config[key] for key in ("pid", "creation_ticks", "token")}
                base.update(schema=1, source=source, qpc_ticks=150, qpc_frequency=100)
                events = [base | {"event": boundary + ".enter"},
                          base | {"event": boundary + ".return", "qpc_ticks": 200},
                          base | {"event": "wrong", "token": "wrong"}]
                Path(config["path"]).write_text("\n".join(json.dumps(e) for e in events) + '\n{"partial":')
                result = collect_lifecycle_trace(config, {"qpc_ticks": 100, "qpc_frequency": 100})
                self.assertEqual(result["status"], "partial")
                self.assertEqual(result["auto_close"], "returned")
                self.assertEqual(len(result["events"]), 2)
                self.assertEqual(result["events"][1]["since_quit_s"], 1)
                self.assertEqual(result["boundary_scope"], "managed_callback_only" if source == "excel_dna" else "generated_native_body")
                self.assertFalse(Path(config["control_path"]).exists())

    def test_no_events_does_not_claim_auto_close_was_never_called(self):
        with tempfile.TemporaryDirectory() as directory, patch.dict("os.environ", {"LOCALAPPDATA": directory}):
            config = prepare_lifecycle_trace({"pid": 42, "creation_ticks": 123}, Path(directory) / "events.jsonl")
            result = collect_lifecycle_trace(config, None)
            self.assertEqual(result["status"], "unavailable")
            self.assertEqual(result["auto_close"], "instrumentation_unverified")

    def test_collect_does_not_delete_another_sessions_marker(self):
        with tempfile.TemporaryDirectory() as directory, patch.dict("os.environ", {"LOCALAPPDATA": directory}):
            config = prepare_lifecycle_trace({"pid": 42, "creation_ticks": 123}, Path(directory) / "events.jsonl")
            control = Path(config["control_path"])
            atomic_json(control, config | {"token": "replacement"})
            collect_lifecycle_trace(config, None)
            self.assertTrue(control.exists())


if __name__ == "__main__":
    unittest.main()

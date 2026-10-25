from __future__ import annotations

import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

from memory_trace import TraceError, WprTrace, require_native_x64, windows_preflight
from shutdown_diagnostics import ProcessHandle, process_creation_ticks


def stamp():
    return {'filetime_ticks': 1000, 'qpc_ticks': 100, 'qpc_frequency': 10}


class FakeProcess:
    def __init__(self, *, exited=False, ticks=123):
        self.exited, self.ticks = exited, ticks
    def creation_ticks(self):
        return self.ticks
    def wait(self, timeout):
        return self.exited


class FakeWpr:
    def __init__(self, fail=None):
        self.calls, self.fail = [], fail
    def __call__(self, argv, **kwargs):
        self.calls.append(argv)
        if self.fail and self.fail(argv):
            raise subprocess.TimeoutExpired(argv, kwargs['timeout'])
        if '-stop' in argv:
            Path(argv[argv.index('-stop') + 1]).write_bytes(b'test-etl')
        return subprocess.CompletedProcess(argv, 0, 'WPR output', '')


class MemoryTraceTest(unittest.TestCase):
    def test_named_recording_and_pid_snapshots_preserve_commands_and_evidence(self):
        with tempfile.TemporaryDirectory() as temporary:
            runner = FakeWpr()
            trace = WprTrace('wpr.exe', Path(temporary), run=runner, stamp=stamp)
            trace.bind(42, 123, FakeProcess())
            trace.start()
            trace.snapshot('post_warmup', {'memory_before_snapshot': {'private_bytes': 500}})
            trace.snapshot('after_1000', {'memory_before_snapshot': {'private_bytes': 1000}})
            trace.stop()
            trace.cleanup()
            self.assertEqual(trace.record['capture_status'], 'saved')
            self.assertEqual(trace.record['cleanup_errors'], [])
            self.assertFalse(trace.record['trace_contents_verified'])
            self.assertEqual(trace.record['attribution_status'], 'not_analyzed')
            self.assertEqual(trace.record['etl']['bytes'], 8)
            self.assertEqual(trace.record['snapshot_cleanup'], 'disabled')
            for argv in runner.calls:
                self.assertNotIn('-heaptracingconfig', argv)
                self.assertNotIn('-name', argv)
                if argv[1] not in ('-snapshotconfig', '-profiles'):
                    self.assertEqual(argv[-2:], ['-instancename', trace.instance])
            commands = [argv[1] for argv in runner.calls]
            self.assertEqual(commands.count('-singlesnapshot'), 2)
            self.assertNotIn('-cancel', commands)
            saved = json.loads((Path(temporary) / 'trace.json').read_text())
            self.assertEqual(saved['snapshots'][0]['observation']['memory_before_snapshot']['private_bytes'], 500)

    def test_partial_start_cancels_only_our_unique_instance(self):
        with tempfile.TemporaryDirectory() as temporary:
            runner = FakeWpr(lambda argv: argv[1] == '-start')
            trace = WprTrace('wpr.exe', Path(temporary), run=runner, stamp=stamp)
            with self.assertRaises(subprocess.TimeoutExpired):
                trace.start()
            trace.cleanup()
            self.assertEqual(runner.calls[-1], ['wpr.exe', '-cancel', '-instancename', trace.instance])
            self.assertEqual(trace.record['capture_status'], 'incomplete')

    def test_enable_timeout_still_disables_pid_and_saves_partial_trace(self):
        with tempfile.TemporaryDirectory() as temporary:
            runner = FakeWpr(lambda argv: argv[1] == '-snapshotconfig' and argv[-1] == 'enable')
            trace = WprTrace('wpr.exe', Path(temporary), run=runner, stamp=stamp)
            trace.start()
            with self.assertRaises(subprocess.TimeoutExpired):
                trace.bind(42, 123, FakeProcess())
            trace.cleanup()
            self.assertIn(['wpr.exe', '-snapshotconfig', 'heap', '-pid', '42', 'disable'], runner.calls)
            self.assertEqual(trace.record['capture_status'], 'saved')

    def test_pid_mismatch_exit_and_duplicate_snapshot_never_trigger_another_capture(self):
        with tempfile.TemporaryDirectory() as temporary:
            runner = FakeWpr()
            trace = WprTrace('wpr.exe', Path(temporary), run=runner, stamp=stamp)
            trace.start()
            with self.assertRaisesRegex(TraceError, 'identity mismatch'):
                trace.bind(42, 123, FakeProcess(ticks=999))
            self.assertFalse(any(a[1] == '-snapshotconfig' for a in runner.calls))
            process = FakeProcess()
            trace.bind(42, 123, process)
            trace.snapshot('post_warmup', {})
            count = len(runner.calls)
            with self.assertRaisesRegex(TraceError, 'Duplicate'):
                trace.snapshot('post_warmup', {})
            self.assertEqual(len(runner.calls), count)
            process.exited = True
            with self.assertRaisesRegex(TraceError, 'unavailable'):
                trace.snapshot('after_1000', {})
            self.assertEqual(len(runner.calls), count)
            trace.cleanup()
            self.assertEqual(trace.record['snapshot_cleanup'], 'process_exited_configuration_expired')

    def test_virtual_mode_has_markers_without_heap_configuration(self):
        with tempfile.TemporaryDirectory() as temporary:
            runner = FakeWpr()
            trace = WprTrace('wpr.exe', Path(temporary), 'virtual', run=runner, stamp=stamp)
            trace.start()
            trace.bind(42, 123, FakeProcess())
            trace.snapshot('post_warmup', {})
            trace.stop()
            self.assertFalse(any(a[1] in ('-snapshotconfig', '-singlesnapshot') for a in runner.calls))
            self.assertEqual(sum(a[1] == '-marker' for a in runner.calls), 2)

    def test_stop_timeout_preserves_cleanup_failure_and_scoped_cancel(self):
        with tempfile.TemporaryDirectory() as temporary:
            runner = FakeWpr(lambda argv: argv[1] == '-stop')
            trace = WprTrace('wpr.exe', Path(temporary), run=runner, stamp=stamp)
            trace.start()
            trace.bind(42, 123, FakeProcess())
            trace.cleanup()
            self.assertEqual(trace.record['capture_status'], 'incomplete')
            self.assertTrue(any('trace save' in e for e in trace.record['cleanup_errors']))
            self.assertEqual(runner.calls[-1], ['wpr.exe', '-cancel', '-instancename', trace.instance])

    def test_log_write_failure_cannot_prevent_pid_and_session_cleanup(self):
        with tempfile.TemporaryDirectory() as temporary:
            runner = FakeWpr()
            trace = WprTrace('wpr.exe', Path(temporary), run=runner, stamp=stamp)
            trace.bind(42, 123, FakeProcess())
            trace.start()
            with patch('memory_trace.atomic_json', side_effect=OSError('disk full')):
                trace.cleanup()
            self.assertIn(['wpr.exe', '-snapshotconfig', 'heap', '-pid', '42', 'disable'], runner.calls)
            self.assertTrue(any(a[1] == '-stop' for a in runner.calls))
            self.assertTrue(any('disk full' in e for e in trace.record['cleanup_errors']))

    @unittest.skipUnless(sys.platform == 'win32' and os.environ.get('XLFN_WPR_SMOKE') == '1',
                         'explicit elevated Windows WPR smoke test')
    def test_native_wpr_snapshots_of_owned_child(self):
        # Exercise real WPR command syntax, identity binding, heap/VA profiles,
        # snapshot barriers and cleanup in Windows CI without starting Excel.
        wpr = windows_preflight()
        destination = os.environ.get('XLFN_WPR_SMOKE_DIR')
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(destination) if destination else Path(temporary)
            directory.mkdir(parents=True, exist_ok=True)
            trace = WprTrace(wpr, directory)
            child = process = None
            try:
                child = subprocess.Popen([sys.executable, '-u', '-c',
                    "import sys; data=[]; print('ready'); "
                    "[(data.append(bytearray(2*1024*1024)), print('allocated')) for line in sys.stdin]"],
                    stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
                self.assertEqual(child.stdout.readline().strip(), 'ready')
                process = ProcessHandle(child.pid, synchronize=True)
                require_native_x64(process)
                trace.bind(child.pid, process_creation_ticks(child.pid), process)
                trace.start()
                trace.snapshot('post_warmup', {})
                child.stdin.write('grow\n'); child.stdin.flush()
                self.assertEqual(child.stdout.readline().strip(), 'allocated')
                trace.snapshot('after_growth', {})
                trace.stop()
                self.assertGreater((directory / 'memory.etl').stat().st_size, 0)
                self.assertEqual(trace.record['capture_status'], 'saved')
            finally:
                trace.cleanup()
                if child is not None:
                    child.stdin.close()
                    try:
                        child.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        child.kill(); child.wait(timeout=5)
                    child.stdout.close()
                if process is not None:
                    process.__exit__(None, None, None)
            self.assertEqual(trace.record['cleanup_errors'], [])


if __name__ == '__main__':
    unittest.main()

from __future__ import annotations

import copy
import json
from pathlib import Path
import struct
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

from artifact_manifest import FILES, create, verify, verify_symbols
from memory_probe import (TraceError, bind_fresh_process, collect_workload, phase_labels,
                          run_case, validate_capture, validate_request)
from memory_trace import WprTrace
from test_memory_trace import FakeWpr, stamp


class Progress:
    def __init__(self):
        self.record = {}
    def update(self, **fields):
        self.record.update(fields)
    def stage(self, name, **details):
        self.record['stage'] = name


class Session:
    def __init__(self, cells, bad_last=False):
        self.cells, self.bad_last = cells, bad_last
        self.formulas, self.calls = [], []
        self.last_calculation_timing = {'total_s': 1, 'calculate_call_s': 0.75,
                                        'completion_wait_s': 0.25, 'completion_polls': 1}
        self.closed = False
    def new_book(self):
        return 'sheet'
    def add_formulas(self, sheet, count, make):
        self.formulas = [make(row) for row in range(1, count + 1)]
        values = tuple((i,) for i in range(1, count + 1))
        if self.bad_last:
            values = values[:-1] + ((-1,),)
        return SimpleNamespace(Value2=values)
    def calculate(self, target, *, full):
        self.calls.append(full)
    def memory_snapshot(self):
        return {'rss_bytes': len(self.calls) * 100, 'private_bytes': len(self.calls) * 50}
    def close_book(self):
        self.closed = True


class MemoryProbeTest(unittest.TestCase):
    def test_control_and_udf_use_full_calculation_and_ordered_snapshot_barriers(self):
        for implementation in ('excel', 'xlfn', 'excel_dna'):
            with self.subTest(implementation=implementation):
                session = Session(4)
                progress = Progress()
                requests = []
                channel = SimpleNamespace(request=lambda kind, **fields: requests.append((kind, fields)))
                with patch('memory_probe.clock_stamp', return_value={'qpc_ticks': 1}), patch('memory_probe.time.sleep'):
                    collect_workload(session, channel, progress, implementation=implementation,
                                     cells=4, repetitions=100, idle_seconds=5)
                self.assertEqual(session.calls, [True] * 101)
                self.assertEqual(session.formulas, ['=ROW()'] * 4 if implementation == 'excel' else
                                 ['=BENCH.ID(1)', '=BENCH.ID(2)', '=BENCH.ID(3)', '=BENCH.ID(4)'])
                self.assertEqual([p['label'] for k, p in requests if k == 'snapshot'], phase_labels(100))
                self.assertEqual([p['observation']['completed_recalculations'] for k, p in requests if k == 'snapshot'],
                                 [0, 25, 50, 100, 100, 100])
                self.assertEqual(requests[-1][0], 'finish_trace')
                self.assertTrue(session.closed)
                self.assertEqual(progress.record['validated_cells'], 4)
                self.assertEqual(len(progress.record['recalculation_samples']), 100)

    def test_incorrect_cell_or_failed_snapshot_cannot_finish_capture(self):
        for bad_cell in (False, True):
            with self.subTest(bad_cell=bad_cell):
                session, progress, requests = Session(3, bad_last=bad_cell), Progress(), []
                def request(kind, **fields):
                    requests.append(kind)
                    raise TraceError('snapshot failed')
                with patch('memory_probe.clock_stamp', return_value={}), patch('memory_probe.time.sleep'):
                    with self.assertRaises((AssertionError, TraceError)):
                        collect_workload(session, SimpleNamespace(request=request), progress,
                                         implementation='xlfn', cells=3, repetitions=100, idle_seconds=0)
                self.assertNotIn('finish_trace', requests)
                self.assertNotIn('validated_cells', progress.record)

    def test_protocol_rejects_stale_foreign_and_reordered_requests(self):
        valid = {'token': 'owned', 'sequence': 2, 'kind': 'snapshot'}
        validate_request(valid, 'owned', 1)
        for change in ({'token': 'foreign'}, {'sequence': 1}, {'sequence': 3},
                       {'sequence': True}, {'kind': 'unknown'}):
            with self.assertRaises(TraceError):
                validate_request(valid | change, 'owned', 1)

    def test_preexisting_process_refused_before_opening_a_handle(self):
        for identity, existing, earliest in (
                ({'pid': 42, 'creation_ticks': 200}, {42}, 100),
                ({'pid': 42, 'creation_ticks': 99}, set(), 100),
                ({'pid': True, 'creation_ticks': 200}, set(), 100)):
            with patch('memory_probe.ProcessHandle') as handle:
                with self.assertRaises(TraceError):
                    bind_fresh_process(identity, existing, earliest)
                handle.assert_not_called()

    def test_capture_requires_each_snapshot_complete_samples_and_final_validation(self):
        labels = phase_labels(100)
        worker = {'status': 'complete', 'validated_cells': 4,
                  'process': {'pid': 42, 'creation_ticks': 123},
                  'snapshots': [{'label': label, 'completed_recalculations': count, 'clock': {},
                                 'memory_before_snapshot': {'private_bytes': 50}}
                                for label, count in zip(labels, (0, 25, 50, 100, 100, 100))],
                  'recalculation_samples': [{'iteration': i} for i in range(1, 101)]}
        trace = {'capture_status': 'saved', 'cleanup_errors': [], 'process': worker['process'],
                 'snapshots': [{'label': snapshot['label'], 'status': 'recorded',
                                 'observation': {k: v for k, v in snapshot.items() if k != 'label'}}
                                for snapshot in worker['snapshots']]}
        validate_capture(worker, trace, 100, 4)
        for which in ('worker_snapshot', 'wpr_snapshot', 'cleanup', 'sample', 'cell', 'observation', 'identity'):
            a, b = copy.deepcopy(worker), copy.deepcopy(trace)
            if which == 'worker_snapshot': a['snapshots'].pop()
            if which == 'wpr_snapshot': b['snapshots'][2]['status'] = 'requested'
            if which == 'cleanup': b['cleanup_errors'].append('disable failed')
            if which == 'sample': a['recalculation_samples'].pop()
            if which == 'cell': a['validated_cells'] = 1
            if which == 'observation': b['snapshots'][2]['observation']['completed_recalculations'] = 49
            if which == 'identity': b['process'] = {'pid': 43, 'creation_ticks': 123}
            with self.subTest(which=which), self.assertRaises(TraceError):
                validate_capture(a, b, 100, 4)

    def test_parent_worker_barriers_save_before_quit_and_cleanup_on_snapshot_failure(self):
        for fail in (False, True):
            with self.subTest(fail=fail), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                executable = root / 'EXCEL.EXE'
                executable.write_bytes(b'fake image')
                directory = root / 'excel'
                identity = {'pid': 42, 'creation_ticks': 2000, 'create_time': 1.0}
                labels = phase_labels(100)
                snapshots = [{'label': label, 'completed_recalculations': count, 'clock': {},
                              'memory_before_snapshot': {'private_bytes': 50}}
                             for label, count in zip(labels, (0, 25, 50, 100, 100, 100))]
                finished = []

                class Handle:
                    def creation_ticks(self): return 2000
                    def wait(self, timeout): return bool(finished)
                    def exit_code(self): return 0
                    def __exit__(self, *args): pass

                class Child:
                    returncode = None
                    def __init__(self, *args, **kwargs):
                        self.token = json.loads((directory / 'config.json').read_text())['token']
                        self.index = 0
                        self.requests = [{'kind': 'process_ready', 'process': identity}]
                        self.requests += [{'kind': 'snapshot', 'label': s['label'],
                                           'observation': {k: v for k, v in s.items() if k != 'label'}}
                                          for s in snapshots]
                        self.requests += [{'kind': 'finish_trace'}]
                    def poll(self):
                        if self.returncode is not None: return self.returncode
                        reply = directory / 'reply.json'
                        if self.index and not reply.exists(): return None
                        if self.index:
                            message = json.loads(reply.read_text())
                            if message['sequence'] != self.index: return None
                            if message['status'] != 'ok':
                                self.end(False)
                                return self.returncode
                        if self.index == len(self.requests):
                            self.end(True)
                            return self.returncode
                        request = self.requests[self.index]
                        self.index += 1
                        (directory / 'request.json').write_text(json.dumps(
                            {'token': self.token, 'sequence': self.index, **request}))
                        return None
                    def end(self, success):
                        record = {'status': 'complete' if success else 'error', 'validated_cells': 4,
                                  'process': identity, 'snapshots': snapshots,
                                  'recalculation_samples': [{'iteration': i} for i in range(1, 101)]}
                        (directory / 'worker.json').write_text(json.dumps(record))
                        finished.append(success)
                        self.returncode = 0 if success else 1
                    def wait(self, **kwargs):
                        self.poll()
                        return self.returncode

                runner = FakeWpr(lambda argv: fail and argv[1] == '-singlesnapshot')
                fake_psutil = SimpleNamespace(pids=lambda: [10], Process=lambda pid:
                                              SimpleNamespace(exe=lambda: str(executable)))
                args = SimpleNamespace(artifacts=root, cells=4, repetitions=100, idle_seconds=0,
                                       timeout=30, max_trace_mib=64, trace='both')
                with patch.dict(sys.modules, {'psutil': fake_psutil}), \
                        patch('memory_probe.subprocess.Popen', Child), \
                        patch('memory_probe.ProcessHandle', return_value=Handle()), \
                        patch('memory_probe.require_native_x64'), \
                        patch('memory_probe.clock_stamp', side_effect=stamp), \
                        patch('memory_probe.WprTrace', side_effect=lambda exe, folder, mode:
                              WprTrace(exe, folder, mode, run=runner, stamp=stamp)):
                    result = run_case(args, 'excel', directory, 'wpr.exe')
                self.assertEqual(result['capture_status'], 'error' if fail else 'complete')
                self.assertEqual(result['process_exit']['termination'], 'natural')
                self.assertEqual(result['trace']['capture_status'], 'saved')
                self.assertEqual(result['trace']['snapshot_cleanup'], 'disabled')
                self.assertFalse(result['trace']['trace_contents_verified'])
                commands = [c[1] for c in runner.calls]
                self.assertLess(commands.index('-snapshotconfig'), commands.index('-start'))
                self.assertIn('-stop', commands)

    def test_symbol_provenance_is_optional_for_old_runs_but_required_for_attribution(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            header = bytearray(128)
            header[:2] = b'MZ'
            struct.pack_into('<I', header, 0x3c, 64)
            header[64:68] = b'PE\0\0'
            struct.pack_into('<H', header, 68, 0x8664)
            for name in FILES:
                p = root / name
                p.parent.mkdir(parents=True, exist_ok=True)
                p.write_bytes(header)
            manifest = create(root, 'commit', 'run', '1')
            verify(root)
            with self.assertRaisesRegex(ValueError, 'Matching PDBs'):
                verify_symbols(root, manifest, required=('xlfn',))
            pdb = root / 'symbols/xlfn/xlfn_excel_comparison.pdb'
            pdb.parent.mkdir(parents=True)
            pdb.write_bytes(b'matching-build-pdb')
            manifest = create(root, 'commit', 'run', '1')
            verify(root)
            verify_symbols(root, manifest, required=('xlfn',))
            with self.assertRaisesRegex(ValueError, 'Matching PDBs'):
                verify_symbols(root, manifest, required=('excel_dna',))
            changed = copy.deepcopy(manifest)
            changed['symbols']['symbols/xlfn/xlfn_excel_comparison.pdb']['image_sha256'] = 'other-build'
            with self.assertRaisesRegex(ValueError, 'binding mismatch'):
                verify_symbols(root, changed)
            pdb.write_bytes(b'wrong-pdb')
            with self.assertRaisesRegex(ValueError, 'checksum mismatch'):
                verify(root)
            pdb.write_bytes(b'matching-build-pdb')
            outside = root.parent / (root.name + '-outside.pdb')
            try:
                outside.write_bytes(b'matching-build-pdb')
                pdb.unlink()
                pdb.symlink_to(outside)
                with self.assertRaisesRegex(ValueError, 'escapes'):
                    verify_symbols(root, manifest)
            finally:
                outside.unlink(missing_ok=True)


if __name__ == '__main__':
    unittest.main()

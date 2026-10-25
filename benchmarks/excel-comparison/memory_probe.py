"""Capture WPR heap snapshots/VirtualAlloc stacks around repeated Excel calculations.

Run from elevated 64-bit PowerShell on Windows x64. The parent owns the WPR
instance and native process handle; a bounded child owns Excel COM. Output is
an attribution experiment, not a performance-comparison result.
"""
from __future__ import annotations

import argparse
from datetime import datetime, timezone
import gc
import importlib
import json
from pathlib import Path
import platform
import subprocess
import sys
import time
import traceback
import uuid

from artifact_manifest import sha256, verify, verify_symbols
from memory_trace import TraceError, WprTrace, require_native_x64, windows_preflight
from run import ExcelSession, WorkerProgress, check_final_values
from shutdown_diagnostics import ProcessHandle, process_creation_ticks
from shutdown_observation import atomic_json, clock_stamp

ROOT = Path(__file__).resolve().parent
IMPLEMENTATIONS = ('excel', 'xlfn', 'excel_dna')
SOURCE_FILES = ('memory_probe.py', 'memory_trace.py', 'run.py', 'artifact_manifest.py',
                'shutdown_diagnostics.py', 'shutdown_observation.py', 'workloads.py')


def read_json(path: Path, maximum: int = 8_000_000):
    with path.open('rb') as stream:
        raw = stream.read(maximum + 1)
    if len(raw) > maximum:
        raise ValueError(f'Oversized control/result file: {path.name}')
    return json.loads(raw)


def phase_labels(repetitions: int) -> list[str]:
    return ['post_warmup', *(f'after_{i}' for i in (repetitions // 4, repetitions // 2, repetitions)),
            'workbook_closed', 'after_idle']


class Channel:
    """Atomic, sequenced parent/child barrier; pause Excel during snapshots."""
    def __init__(self, directory: Path, token: str):
        self.directory, self.token, self.sequence = directory, token, 0

    def request(self, kind: str, **payload):
        self.sequence += 1
        request = {'token': self.token, 'sequence': self.sequence, 'kind': kind,
                   'clock': clock_stamp(), **payload}
        atomic_json(self.directory / 'request.json', request)
        deadline = time.monotonic() + 180
        while time.monotonic() < deadline:
            try:
                reply = read_json(self.directory / 'reply.json', 100_000)
            except FileNotFoundError:
                time.sleep(0.05)
                continue
            if reply.get('token') != self.token:
                raise ValueError('Parent reply token mismatch')
            if reply.get('sequence') == self.sequence:
                if reply.get('status') != 'ok':
                    raise TraceError(reply.get('error', 'Parent rejected request'))
                return
            if reply.get('sequence', -1) > self.sequence:
                raise ValueError('Parent reply sequence mismatch')
            time.sleep(0.05)
        raise TimeoutError('Memory-trace parent did not acknowledge the request')


def collect_workload(session, channel, progress, *, implementation: str, cells: int,
                     repetitions: int, idle_seconds: float):
    """Use the L02 CalculateFull/Dirty sequence, with explicit snapshot pauses."""
    samples = []
    snapshots = []
    progress.update(recalculation_samples=samples, snapshots=snapshots)

    def capture(label: str, completed: int):
        observation = {'completed_recalculations': completed, 'clock': clock_stamp(),
                       'memory_before_snapshot': session.memory_snapshot()}
        channel.request('snapshot', label=label, observation=observation)
        observation['memory_after_snapshot'] = session.memory_snapshot()
        snapshots.append({'label': label, **observation})
        progress.update(recalculation_samples=list(samples), snapshots=list(snapshots))

    sheet = target = None
    try:
        sheet = session.new_book()
        target = session.add_formulas(sheet, cells, lambda row:
            '=ROW()' if implementation == 'excel' else f'=BENCH.ID({row})')
        session.calculate(target, full=True)
        check_final_values(target, cells, lambda value, index:
            type(value) in (int, float) and value == index + 1)
        progress.stage('warmup_complete')
        capture('post_warmup', 0)
        boundaries = (repetitions // 4, repetitions // 2, repetitions)
        for index in range(1, repetitions + 1):
            session.calculate(target, full=True)
            samples.append({'iteration': index, **session.last_calculation_timing,
                            **session.memory_snapshot()})
            if index in boundaries:
                capture(f'after_{index}', index)
        check_final_values(target, cells, lambda value, index:
            type(value) in (int, float) and value == index + 1)
        progress.update(validated_cells=cells)
    finally:
        # Release every worksheet/range reference before closing the owned book.
        target = sheet = None
        gc.collect()
    session.close_book()
    capture('workbook_closed', repetitions)
    progress.stage('idle_after_workbook_close', seconds=idle_seconds)
    time.sleep(idle_seconds)
    capture('after_idle', repetitions)
    channel.request('finish_trace')
    progress.stage('trace_saved_before_excel_quit')


def worker(args) -> int:
    import psutil
    config = read_json(args.worker)
    directory = args.worker.parent
    progress = WorkerProgress(directory / 'worker.json', {
        'schema': 1, 'implementation': config['implementation'], 'status': 'running',
        'cells': config['cells'], 'repetitions': config['repetitions'],
        'repeat_diagnostics': False, 'warmup_calculation': 'CalculateFull',
        'performance_comparable': False,
        'timing_note': 'WPR allocation stacks and snapshot pauses perturb this experiment',
    })
    progress.save()
    channel = Channel(directory, config['token'])
    existing = set(psutil.pids())
    session = None

    def process_ready(identity):
        if identity['pid'] in existing:
            raise TraceError('DispatchEx returned a pre-existing process; refusing to alter it')
        bound = dict(identity, creation_ticks=process_creation_ticks(identity['pid']))
        progress.update(process=bound)
        channel.request('process_ready', process=bound)

    try:
        implementation = config['implementation']
        xll = None if implementation == 'excel' else Path(config['artifacts']) / implementation / 'benchmark.xll'
        session = ExcelSession(xll, 1, 100, directory / 'pid.json', progress=progress,
                               process_ready=process_ready)
        session.implementation = implementation
        progress.update(excel_version=session.app.Version, excel_build=session.app.Build,
                        excel_bitness=session.app.OperatingSystem,
                        calculation_mode='manual', threads=1,
                        loaded_target_xll=str(xll) if xll else None)
        # Record image paths for unexpected auto-loaded add-ins; the ETL loader
        # records remain the authority for resolving stack addresses over time.
        try:
            progress.update(mapped_images=sorted({m.path for m in session.process.memory_maps()
                if Path(m.path).suffix.lower() in ('.exe', '.dll', '.xll')}))
        except (OSError, psutil.Error) as error:
            progress.update(mapped_images_error=str(error))
        collect_workload(session, channel, progress, implementation=implementation,
                         cells=config['cells'], repetitions=config['repetitions'],
                         idle_seconds=config['idle_seconds'])
        progress.update(status='complete')
    except BaseException as error:
        progress.update(status='error', error=f'{type(error).__name__}: {error}',
                        traceback=traceback.format_exc())
    finally:
        if session is not None:
            try:
                session.close()
            except BaseException as error:
                progress.update(status='error', cleanup_error=f'{type(error).__name__}: {error}')
        progress.save()
    return 0 if progress.record['status'] == 'complete' else 1


def validate_request(request: dict, token: str, last_sequence: int):
    if request.get('token') != token:
        raise TraceError('Worker request token mismatch')
    sequence = request.get('sequence')
    if type(sequence) is not int or sequence != last_sequence + 1:
        raise TraceError('Worker request sequence mismatch')
    if request.get('kind') not in ('process_ready', 'snapshot', 'finish_trace'):
        raise TraceError('Unknown worker request')


def bind_fresh_process(identity: dict, existing: set[int], earliest_ticks: int):
    pid, ticks = identity.get('pid'), identity.get('creation_ticks')
    if type(pid) is not int or pid <= 0 or type(ticks) is not int:
        raise TraceError('Invalid worker process identity')
    if pid in existing or ticks < earliest_ticks:
        raise TraceError('Refusing to trace a process that predates this worker')
    process = ProcessHandle(pid, synchronize=True)
    try:
        if process.creation_ticks() != ticks or process.wait(0):
            raise TraceError('Excel identity changed or exited before binding')
        require_native_x64(process)
        return process
    except BaseException:
        process.__exit__(None, None, None)
        raise


def validate_capture(worker_record: dict, trace_record: dict, repetitions: int, cells: int):
    if worker_record.get('status') != 'complete' or worker_record.get('validated_cells') != cells:
        raise TraceError('Worker did not validate the complete final worksheet')
    labels = phase_labels(repetitions)
    if [s['label'] for s in worker_record.get('snapshots', [])] != labels:
        raise TraceError('Worker snapshot sequence is incomplete')
    if [s['label'] for s in trace_record['snapshots']] != labels:
        raise TraceError('WPR snapshot sequence is incomplete')
    if any(s['status'] != 'recorded' for s in trace_record['snapshots']):
        raise TraceError('A WPR snapshot command did not finish')
    for index, (worker_snapshot, trace_snapshot) in enumerate(zip(
            worker_record['snapshots'], trace_record['snapshots'], strict=True)):
        expected = (0, repetitions // 4, repetitions // 2, repetitions, repetitions, repetitions)[index]
        observation = trace_snapshot['observation']
        if (worker_snapshot.get('completed_recalculations') != expected
                or any(worker_snapshot.get(k) != observation.get(k) for k in
                       ('completed_recalculations', 'clock', 'memory_before_snapshot'))):
            raise TraceError('Worker and WPR snapshot observations differ')
    identity = worker_record.get('process', {})
    if any(type(identity.get(k)) is not int or identity[k] <= 0
           or identity[k] != trace_record.get('process', {}).get(k) for k in ('pid', 'creation_ticks')):
        raise TraceError('Worker and WPR process identities differ')
    if trace_record['capture_status'] != 'saved' or trace_record['cleanup_errors']:
        raise TraceError('WPR capture or cleanup did not finish')
    samples = worker_record.get('recalculation_samples', [])
    if [s['iteration'] for s in samples] != list(range(1, repetitions + 1)):
        raise TraceError('Recalculation samples are incomplete')


def run_case(args, implementation: str, directory: Path, wpr: str) -> dict:
    import psutil
    directory.mkdir()
    token = uuid.uuid4().hex
    config = {'token': token, 'implementation': implementation, 'artifacts': str(args.artifacts),
              'cells': args.cells, 'repetitions': args.repetitions, 'idle_seconds': args.idle_seconds}
    atomic_json(directory / 'config.json', config)
    result = {'implementation': implementation, 'capture_status': 'running',
              'attribution_status': 'not_analyzed', 'directory': str(directory),
              'performance_comparable': False}
    atomic_json(directory / 'case.json', result)
    trace = WprTrace(wpr, directory, args.trace)
    existing = set(psutil.pids())
    child = process = None
    identity = None
    trace_finished = False
    sequence = 0
    phase_index = 0
    request = None
    labels = phase_labels(args.repetitions)
    log = (directory / 'worker.log').open('w', encoding='utf-8')
    try:
        earliest_ticks = clock_stamp()['filetime_ticks']
        child = subprocess.Popen([sys.executable, str(Path(__file__).resolve()),
                                  '--worker', str(directory / 'config.json')], stdout=log, stderr=log)
        deadline = time.monotonic() + args.timeout
        while child.poll() is None:
            if time.monotonic() >= deadline:
                raise TimeoutError(f'Memory probe exceeded {args.timeout}s')
            # Bound diagnostic storage; the per-command timeout also bounds a
            # snapshot/stop that spends time between these watchdog checks.
            used = sum(p.stat().st_size for p in directory.rglob('*') if p.is_file())
            if used > args.max_trace_mib * 1024 * 1024:
                raise TraceError('Trace size budget exceeded; partial recording will be saved')
            try:
                current = read_json(directory / 'request.json', 100_000)
            except FileNotFoundError:
                time.sleep(0.05)
                continue
            if current.get('sequence') == sequence:
                time.sleep(0.05)
                continue
            request = current
            validate_request(request, token, sequence)
            kind = request['kind']
            if kind == 'process_ready':
                if process is not None or sequence != 0:
                    raise TraceError('Process identity may only be bound once')
                identity = request['process']
                bound = bind_fresh_process(identity, existing, earliest_ticks)
                try:
                    executable = Path(psutil.Process(identity['pid']).exe())
                    if executable.name.lower() != 'excel.exe':
                        raise TraceError('Worker process is not Excel')
                except BaseException:
                    bound.__exit__(None, None, None)
                    raise
                process = bound
                result['process'] = identity
                result['excel_executable'] = {'path': str(executable), 'sha256': sha256(executable)}
                atomic_json(directory / 'case.json', result)
                trace.bind(identity['pid'], identity['creation_ticks'], process)
                trace.start()
            elif kind == 'snapshot':
                if trace_finished or phase_index >= len(labels) or request.get('label') != labels[phase_index]:
                    raise TraceError('Unexpected snapshot order')
                trace.snapshot(request['label'], request['observation'])
                phase_index += 1
            else:
                if trace_finished or phase_index != len(labels):
                    raise TraceError('Trace finish requested before every snapshot')
                trace.stop()
                trace_finished = True
            sequence = request['sequence']
            atomic_json(directory / 'reply.json', {'token': token, 'sequence': sequence, 'status': 'ok'})
            request = None
        child.wait()
        result['worker_returncode'] = child.returncode
        record = read_json(directory / 'worker.json')
        result['worker'] = record
        validate_capture(record, trace.record, args.repetitions, args.cells)
        if child.returncode != 0:
            raise TraceError(f'Excel worker exited with status {child.returncode}')
        result['capture_status'] = 'complete'
    except BaseException as error:
        result.update(capture_status='error', error=f'{type(error).__name__}: {error}')
        if request is not None:
            atomic_json(directory / 'reply.json', {'token': token, 'sequence': request.get('sequence'),
                                                  'status': 'error', 'error': result['error']})
    finally:
        # Trace control stays out of the COM child, so even a blocked COM call
        # cannot prevent saving/canceling our own WPR instance.
        try:
            trace.cleanup()
        except BaseException as error:
            result['trace_cleanup_error'] = f'{type(error).__name__}: {error}'
            result['capture_status'] = 'error'
        if child is not None and child.poll() is None:
            try:
                child.wait(timeout=5)
            except subprocess.TimeoutExpired:
                child.terminate()
                try:
                    child.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    child.kill()
                    child.wait(timeout=5)
                result['worker_terminated'] = True
        if process is not None:
            try:
                if process.wait(10_000):
                    result['process_exit'] = {'termination': 'natural', 'exit_code': process.exit_code()}
                    if result['process_exit']['exit_code'] != 0:
                        result.update(capture_status='error', error='Excel exited with a nonzero status')
                else:
                    # Retained native handle plus both identities prevents PID
                    # reuse from authorizing termination of an unrelated Excel.
                    target = psutil.Process(identity['pid'])
                    if (process.creation_ticks() != identity['creation_ticks']
                            or target.create_time() != identity['create_time']):
                        raise TraceError('Process identity changed; forced cleanup refused')
                    target.kill()
                    exited = process.wait(5_000)
                    result['process_exit'] = {'termination': 'forced', 'exited': exited}
                    result['capture_status'] = 'error'
            except BaseException as error:
                result['cleanup_error'] = f'{type(error).__name__}: {error}'
                result['capture_status'] = 'error'
            finally:
                process.__exit__(None, None, None)
        log.close()
        if (directory / 'worker.json').is_file():
            result['worker'] = read_json(directory / 'worker.json')
        result['trace'] = trace.record
        if trace.record['cleanup_errors']:
            result['capture_status'] = 'error'
        atomic_json(directory / 'case.json', result)
    return result


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--artifacts', type=Path, default=ROOT / 'artifacts')
    parser.add_argument('--out', type=Path)
    parser.add_argument('--implementation', choices=IMPLEMENTATIONS, action='append',
                        help='Default: fresh Excel-only, xlfn and Excel-DNA processes')
    parser.add_argument('--trace', choices=('heap', 'virtual', 'both'), default='both')
    parser.add_argument('--cells', type=int, default=1000)
    parser.add_argument('--repetitions', type=int, choices=(100, 1000, 10000), default=1000)
    parser.add_argument('--idle-seconds', type=float, default=5)
    parser.add_argument('--timeout', type=float, default=300, help='Per-case worker deadline in seconds')
    parser.add_argument('--max-trace-mib', type=int, default=1024)
    parser.add_argument('--wpr', help='Explicit WPR executable, if not on PATH')
    parser.add_argument('--worker', type=Path, help=argparse.SUPPRESS)
    args = parser.parse_args()
    if args.worker:
        return worker(args)
    if args.out is None:
        parser.error('--out must name a new output directory')
    if (not 1 <= args.cells <= 100000 or not 0 <= args.idle_seconds <= 60
            or not 30 <= args.timeout <= 1800 or not 64 <= args.max_trace_mib <= 8192):
        parser.error('cells 1..100000, idle-seconds 0..60, timeout 30..1800, max-trace-mib 64..8192')
    try:
        wpr = windows_preflight(args.wpr)
        # Check prerequisites before any WPR recording starts.
        importlib.import_module('psutil')
        importlib.import_module('win32com.client')
        args.artifacts = args.artifacts.resolve()
        args.out = args.out.resolve()
        implementations = args.implementation or list(IMPLEMENTATIONS)
        if len(set(implementations)) != len(implementations):
            raise ValueError('Duplicate implementation')
        commit = subprocess.check_output(['git', '-C', str(ROOT), 'rev-parse', 'HEAD'], text=True).strip()
        manifest = verify(args.artifacts, commit=commit)
        verify_symbols(args.artifacts, manifest, required=tuple(i for i in implementations if i != 'excel'))
        args.out.mkdir(parents=True, exist_ok=False)
        report = {'schema': 1, 'timestamp_utc': datetime.now(timezone.utc).isoformat(),
            'capture_status': 'running', 'attribution_status': 'not_analyzed',
            'runner_commit': commit, 'artifact_manifest': manifest,
            'artifact_manifest_sha256': sha256(args.artifacts / 'manifest.json'),
            'runner_sources': {name: sha256(ROOT / name) for name in SOURCE_FILES},
            'runner_worktree_status': subprocess.check_output(
                ['git', '-C', str(ROOT), 'status', '--porcelain', '--untracked-files=normal'], text=True),
            'os': platform.platform(), 'python': sys.version,
            'wpr': {'path': wpr, 'sha256': sha256(Path(wpr))},
            'settings': {'cells': args.cells, 'repetitions': args.repetitions, 'trace': args.trace,
                         'idle_seconds': args.idle_seconds, 'timeout': args.timeout,
                         'max_trace_mib': args.max_trace_mib},
            'performance_comparable': False,
            'limitations': ['Heap snapshots cover allocations after PID tracing is enabled.',
                'VirtualAllocation records system-wide events; analyze only the bound Excel process.',
                'Review WPR event loss, snapshot contents and symbol resolution in WPA before attribution.',
                'Snapshot pauses, tracing and the post-close idle period change execution timing.',
                'Closing the test workbook leaves the XLL and bootstrap workbook loaded.',
                'This does not qualify add-in close/reopen, cancellation or physical DLL unload.'],
            'cases': []}
        atomic_json(args.out / 'report.json', report)
        for implementation in implementations:
            print(f'Capturing {implementation}: {args.repetitions} recalculations', flush=True)
            case = run_case(args, implementation, args.out / implementation, wpr)
            report['cases'].append(case)
            atomic_json(args.out / 'report.json', report)
            if case['capture_status'] != 'complete':
                report['capture_status'] = 'error'
                atomic_json(args.out / 'report.json', report)
                print(f"Capture stopped: {case.get('error', case.get('cleanup_error', 'see report.json'))}", file=sys.stderr)
                return 1
        report['capture_status'] = 'complete'
        atomic_json(args.out / 'report.json', report)
        print(f'Saved {args.out / "report.json"}; inspect each memory.etl in WPA before attributing allocations')
        return 0
    except (Exception, KeyboardInterrupt) as error:
        print(f'{type(error).__name__}: {error}', file=sys.stderr)
        return 1


if __name__ == '__main__':
    raise SystemExit(main())

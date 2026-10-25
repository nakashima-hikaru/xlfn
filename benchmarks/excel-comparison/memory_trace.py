"""PID-scoped heap snapshots and a private WPR recording for the memory probe.

WPR changes only the newly created process's snapshot configuration. Every
recording command uses our unique instance name; never stop/cancel the default
recording or write image-wide IFEO tracing settings.
"""
from __future__ import annotations

import ctypes
from pathlib import Path
import shutil
import subprocess
import sys
import uuid

from artifact_manifest import sha256
from shutdown_observation import atomic_json, clock_stamp


class TraceError(RuntimeError):
    pass


def windows_preflight(wpr: str | None = None) -> str:
    if sys.platform != 'win32' or ctypes.sizeof(ctypes.c_void_p) != 8:
        raise TraceError('Use 64-bit Python on Windows x64 in an elevated PowerShell window')
    if not ctypes.windll.shell32.IsUserAnAdmin():
        raise TraceError('WPR needs administrator rights; reopen PowerShell as administrator')
    executable = shutil.which(wpr or 'wpr.exe')
    if executable is None:
        raise TraceError('wpr.exe was not found; install the Windows Performance Toolkit')
    return executable


def require_native_x64(process) -> None:
    """PID-mode snapshots require process and OS architectures to match."""
    from ctypes import wintypes
    kernel = process.kernel
    kernel.IsWow64Process2.argtypes = [wintypes.HANDLE, ctypes.POINTER(wintypes.USHORT),
                                      ctypes.POINTER(wintypes.USHORT)]
    kernel.IsWow64Process2.restype = wintypes.BOOL
    machine, native = wintypes.USHORT(), wintypes.USHORT()
    if not kernel.IsWow64Process2(process.handle, ctypes.byref(machine), ctypes.byref(native)):
        raise ctypes.WinError(ctypes.get_last_error())
    if machine.value != 0 or native.value != 0x8664:
        raise TraceError('This probe requires native x64 Excel on Windows x64')


class WprTrace:
    def __init__(self, executable: str, directory: Path, mode: str = 'both', *, run=None,
                 stamp=None):
        if mode not in ('heap', 'virtual', 'both'):
            raise ValueError('Unknown tracing mode')
        self.executable = executable
        self.directory = directory.resolve()
        self.directory.mkdir(parents=True, exist_ok=True)
        self.mode = mode
        self.instance = 'xlfn-memory-' + uuid.uuid4().hex
        self.run = run or subprocess.run
        self.stamp = stamp or clock_stamp
        self.record = {'schema': 1, 'instance': self.instance, 'mode': mode,
                       'commands': [], 'snapshots': [], 'cleanup_errors': [],
                       'capture_status': 'not_started', 'attribution_status': 'not_analyzed',
                       'trace_contents_verified': False}
        self.started = False
        self.start_attempted = False
        self.snapshot_enabled = False
        self.process = None
        self.pid = None
        self.creation_ticks = None
        self.cleaning = False
        self.save()

    def save(self):
        try:
            atomic_json(self.directory / 'trace.json', self.record)
        except OSError as error:
            # A full/unavailable output volume must not prevent disabling the
            # PID instrumentation or stopping our recording during cleanup.
            if not self.cleaning:
                raise
            message = f'trace log persistence: {type(error).__name__}: {error}'
            if message not in self.record['cleanup_errors']:
                self.record['cleanup_errors'].append(message)

    def command(self, *args: str, session: bool = True, timeout: float = 60) -> dict:
        argv = [self.executable, *map(str, args)]
        if session:
            argv += ['-instancename', self.instance]
        entry = {'argv': argv, 'begin': self.stamp()}
        self.record['commands'].append(entry)
        self.save()
        try:
            result = self.run(argv, capture_output=True, text=True, errors='replace',
                              timeout=timeout, check=False)
            entry.update(returncode=result.returncode, stdout=result.stdout, stderr=result.stderr)
            if result.returncode:
                raise TraceError(f"WPR {args[0]} failed ({result.returncode}): "
                                 f"{result.stderr.strip() or result.stdout.strip()}")
        except BaseException as error:
            entry['error'] = f'{type(error).__name__}: {error}'
            raise
        finally:
            entry['end'] = self.stamp()
            self.save()
        return entry

    def start(self):
        if self.start_attempted:
            raise TraceError('A trace can only be started once')
        profiles = []
        if self.mode in ('heap', 'both'):
            profiles += ['-start', 'heapsnapshot']
        if self.mode in ('virtual', 'both'):
            profiles += ['-start', 'VirtualAllocation']
        temporary = self.directory / 'wpr-temporary'
        temporary.mkdir()
        # Retain the installed profile inventory and WPR version banner.
        self.command('-profiles', session=False)
        self.start_attempted = True
        self.command(*profiles, '-filemode', '-recordtempto', str(temporary))
        self.started = True
        self.record['capture_status'] = 'recording'
        self.save()

    def bind(self, pid: int, creation_ticks: int, process):
        if self.process is not None:
            raise TraceError('Bind exactly one process per recording')
        if process.creation_ticks() != creation_ticks or process.wait(0):
            raise TraceError('Process identity mismatch or process already exited')
        self.pid, self.creation_ticks, self.process = pid, creation_ticks, process
        self.record['process'] = {'pid': pid, 'creation_ticks': creation_ticks}
        if self.mode in ('heap', 'both'):
            # Mark before invoking: a timeout may have enabled tracing already.
            self.snapshot_enabled = True
            self.command('-snapshotconfig', 'heap', '-pid', str(pid), 'enable', session=False)
        self.save()

    def check_process(self):
        if self.process is None or self.process.creation_ticks() != self.creation_ticks or self.process.wait(0):
            raise TraceError('Bound process is unavailable; refusing a PID-directed command')

    def snapshot(self, label: str, observation: dict):
        self.check_process()
        if not self.started:
            raise TraceError('Recording is not active')
        if not label or any(c not in 'abcdefghijklmnopqrstuvwxyz0123456789_-' for c in label):
            raise ValueError('Invalid snapshot label')
        if any(s['label'] == label for s in self.record['snapshots']):
            raise TraceError('Duplicate snapshot label')
        entry = {'label': label, 'observation': observation, 'begin': self.stamp(),
                 'status': 'requested'}
        self.record['snapshots'].append(entry)
        self.save()
        self.command('-marker', f'{self.instance}:{label}:begin')
        if self.mode in ('heap', 'both'):
            self.command('-singlesnapshot', 'heap', str(self.pid))
        self.command('-marker', f'{self.instance}:{label}:end')
        entry.update(status='recorded', end=self.stamp())
        self.save()

    def disable_snapshots(self):
        if not self.snapshot_enabled:
            return
        if self.process is not None and self.process.wait(0):
            self.record['snapshot_cleanup'] = 'process_exited_configuration_expired'
        else:
            self.check_process()
            self.command('-snapshotconfig', 'heap', '-pid', str(self.pid), 'disable', session=False)
            self.record['snapshot_cleanup'] = 'disabled'
        self.snapshot_enabled = False
        self.save()

    def stop(self):
        if not self.started:
            raise TraceError('Recording is not active')
        self.disable_snapshots()
        # Keep unlocalized raw event-loss/status output. A nonempty ETL is not
        # proof that all requested stacks/snapshots reached it; inspect in WPA.
        self.command('-status', 'collectors', '-details')
        path = self.directory / 'memory.etl'
        if path.exists():
            raise TraceError('Refusing to overwrite an existing trace')
        self.command('-stop', str(path), 'xlfn repeated-calculation memory attribution',
                     '-skipPdbGen', timeout=120)
        self.started = False
        if not path.is_file() or path.stat().st_size == 0:
            raise TraceError('WPR did not produce a nonempty ETL')
        self.record.update(capture_status='saved', etl={'path': str(path),
            'bytes': path.stat().st_size, 'sha256': sha256(path)})
        self.save()

    def cleanup(self):
        """Best effort with persistent errors, scoped to this instance and PID."""
        self.cleaning = True
        try:
            self.disable_snapshots()
        except BaseException as error:
            self.record['cleanup_errors'].append(f'snapshot disable: {type(error).__name__}: {error}')
        if self.started:
            try:
                self.stop()
            except BaseException as error:
                self.record['cleanup_errors'].append(f'trace save: {type(error).__name__}: {error}')
        if self.started or (self.start_attempted and self.record['capture_status'] != 'saved'):
            try:
                self.command('-cancel')
                self.started = False
            except BaseException as error:
                self.record['cleanup_errors'].append(f'instance cancel: {type(error).__name__}: {error}')
        if self.record['capture_status'] != 'saved':
            self.record['capture_status'] = 'incomplete'
        self.save()

"""Opt-in, out-of-process Windows stack dumps before Excel is reaped."""

from __future__ import annotations

import argparse
import ctypes
import json
from pathlib import Path
import subprocess
import sys
import tempfile


class ProcessHandle:
    """Hold one kernel process identity across validation and dump capture."""

    def __init__(self, pid: int, *, read_memory: bool = False):
        from ctypes import wintypes

        self.kernel = ctypes.WinDLL("kernel32", use_last_error=True)
        self.kernel.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
        self.kernel.OpenProcess.restype = wintypes.HANDLE
        self.kernel.CloseHandle.argtypes = [wintypes.HANDLE]
        self.kernel.CloseHandle.restype = wintypes.BOOL
        self.kernel.GetProcessTimes.argtypes = [wintypes.HANDLE] + [
            ctypes.POINTER(wintypes.FILETIME)] * 4
        self.kernel.GetProcessTimes.restype = wintypes.BOOL
        # Dump capture requires QUERY_INFORMATION | VM_READ. Identity alone
        # needs only QUERY_LIMITED_INFORMATION.
        self.handle = self.kernel.OpenProcess(0x0410 if read_memory else 0x1000, False, pid)
        if not self.handle:
            raise ctypes.WinError(ctypes.get_last_error())

    def __enter__(self):
        return self

    def __exit__(self, *_):
        self.kernel.CloseHandle(self.handle)

    def creation_ticks(self) -> int:
        from ctypes import wintypes

        times = [wintypes.FILETIME() for _ in range(4)]
        if not self.kernel.GetProcessTimes(self.handle, *(ctypes.byref(t) for t in times)):
            raise ctypes.WinError(ctypes.get_last_error())
        return (times[0].dwHighDateTime << 32) | times[0].dwLowDateTime


def process_creation_ticks(pid: int) -> int:
    with ProcessHandle(pid) as process:
        return process.creation_ticks()


def write_dump(pid: int, creation_ticks: int, path: Path) -> None:
    import msvcrt
    from ctypes import wintypes

    with ProcessHandle(pid, read_memory=True) as process:
        # Compare the creation time on the very handle passed to DbgHelp.
        # A recycled PID must never authorize inspecting another process.
        if process.creation_ticks() != creation_ticks:
            raise ValueError("Excel PID was reused; dump refused")
        dbghelp = ctypes.WinDLL("dbghelp", use_last_error=True)
        dump = dbghelp.MiniDumpWriteDump
        dump.argtypes = [wintypes.HANDLE, wintypes.DWORD, wintypes.HANDLE, wintypes.DWORD,
                         ctypes.c_void_p, ctypes.c_void_p, ctypes.c_void_p]
        dump.restype = wintypes.BOOL
        # Normal stacks/modules plus thread metadata and unloaded modules.
        # Do not capture the entire Excel heap.
        with path.open("xb") as output:
            if not dump(process.handle, pid, msvcrt.get_osfhandle(output.fileno()),
                        0x1000 | 0x0020, None, None, None):
                raise ctypes.WinError(ctypes.get_last_error())


def capture_shutdown_dump(identity: dict, directory: Path, timeout_s: float = 15) -> dict:
    """Bound DbgHelp in a helper so a dump failure cannot prevent reaping."""
    result = {"status": "error"}
    try:
        ticks = identity.get("creation_ticks")
        if ticks is None:
            detail = identity.get("dump_identity_error", "native process creation time unavailable")
            raise ValueError(f"{detail}; dump refused")
        directory.mkdir(parents=True, exist_ok=True)
        folder = Path(tempfile.mkdtemp(prefix=f"excel-{identity['pid']}-", dir=directory))
        path = folder / "shutdown.dmp"
        result["directory"] = str(folder.resolve())
        child = subprocess.run(
            [sys.executable, str(Path(__file__).resolve()), "--pid", str(identity["pid"]),
             "--creation-ticks", str(ticks), "--out", str(path.resolve())],
            capture_output=True, text=True, timeout=timeout_s, check=False)
        if child.returncode != 0:
            raise RuntimeError(child.stderr.strip() or f"dump helper exited {child.returncode}")
        if path.stat().st_size == 0:
            raise RuntimeError("dump helper produced an empty dump")
        result.update(status="ok", path=str(path.resolve()), bytes=path.stat().st_size)
    except (OSError, ValueError, RuntimeError, subprocess.TimeoutExpired) as error:
        result["error"] = f"{type(error).__name__}: {error}"
    return result


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--pid", required=True, type=int)
    parser.add_argument("--creation-ticks", required=True, type=int)
    parser.add_argument("--out", required=True, type=Path)
    args = parser.parse_args()
    try:
        write_dump(args.pid, args.creation_ticks, args.out)
    except Exception as error:
        print(json.dumps({"error": f"{type(error).__name__}: {error}"}), file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

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

    def __init__(self, pid: int, *, read_memory: bool = False, synchronize: bool = False):
        from ctypes import wintypes

        self.kernel = ctypes.WinDLL("kernel32", use_last_error=True)
        self.kernel.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
        self.kernel.OpenProcess.restype = wintypes.HANDLE
        self.kernel.CloseHandle.argtypes = [wintypes.HANDLE]
        self.kernel.CloseHandle.restype = wintypes.BOOL
        self.kernel.GetProcessTimes.argtypes = [wintypes.HANDLE] + [
            ctypes.POINTER(wintypes.FILETIME)] * 4
        self.kernel.GetProcessTimes.restype = wintypes.BOOL
        self.kernel.WaitForSingleObject.argtypes = [wintypes.HANDLE, wintypes.DWORD]
        self.kernel.WaitForSingleObject.restype = wintypes.DWORD
        self.kernel.GetExitCodeProcess.argtypes = [wintypes.HANDLE, ctypes.POINTER(wintypes.DWORD)]
        self.kernel.GetExitCodeProcess.restype = wintypes.BOOL
        # Dump capture requires QUERY_INFORMATION | VM_READ | DUP_HANDLE for
        # handle metadata. Identity alone
        # needs only QUERY_LIMITED_INFORMATION.
        access = (0x0450 if read_memory else 0x1000) | (0x100000 if synchronize else 0)
        self.handle = self.kernel.OpenProcess(access, False, pid)
        if not self.handle:
            raise ctypes.WinError(ctypes.get_last_error())

    def __enter__(self):
        return self

    def __exit__(self, *_):
        self.kernel.CloseHandle(self.handle)

    def times(self) -> tuple[int, int]:
        from ctypes import wintypes

        times = [wintypes.FILETIME() for _ in range(4)]
        if not self.kernel.GetProcessTimes(self.handle, *(ctypes.byref(t) for t in times)):
            raise ctypes.WinError(ctypes.get_last_error())
        return tuple((t.dwHighDateTime << 32) | t.dwLowDateTime for t in times[:2])

    def creation_ticks(self) -> int:
        return self.times()[0]

    def wait(self, timeout_ms: int) -> bool:
        result = self.kernel.WaitForSingleObject(self.handle, timeout_ms)
        if result == 0:
            return True
        if result == 258:
            return False
        raise ctypes.WinError(ctypes.get_last_error())

    def exit_code(self) -> int:
        from ctypes import wintypes
        code = wintypes.DWORD()
        if not self.kernel.GetExitCodeProcess(self.handle, ctypes.byref(code)):
            raise ctypes.WinError(ctypes.get_last_error())
        return code.value


def process_creation_ticks(pid: int) -> int:
    with ProcessHandle(pid) as process:
        return process.creation_ticks()


def write_dump(pid: int, creation_ticks: int, path: Path, *, full_memory: bool = False) -> None:
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
        # Stacks/modules, thread metadata, unloaded modules and handle data.
        # Handle data helps identify the thread behind an Office shutdown wait.
        # Full memory is opt-in: ordinary stack dumps omit lock objects in
        # module data/heap pages. Include memory-region metadata in that mode.
        flags = 0x1000 | 0x0020 | 0x0004
        if full_memory:
            flags |= 0x0002 | 0x0800
        with path.open("xb") as output:
            if not dump(process.handle, pid, msvcrt.get_osfhandle(output.fileno()),
                        flags, None, None, None):
                raise ctypes.WinError(ctypes.get_last_error())


def capture_shutdown_dump(identity: dict, directory: Path, timeout_s: float | None = None,
                          *, full_memory: bool = False) -> dict:
    """Bound DbgHelp in a helper so a dump failure cannot prevent reaping."""
    if timeout_s is None:
        timeout_s = 60 if full_memory else 15
    result = {"status": "error", "mode": "full_memory" if full_memory else "stacks",
              "capture_timeout_s": timeout_s}
    try:
        ticks = identity.get("creation_ticks")
        if ticks is None:
            detail = identity.get("dump_identity_error", "native process creation time unavailable")
            raise ValueError(f"{detail}; dump refused")
        directory.mkdir(parents=True, exist_ok=True)
        folder = Path(tempfile.mkdtemp(prefix=f"excel-{identity['pid']}-", dir=directory))
        path = folder / "shutdown.dmp"
        result["directory"] = str(folder.resolve())
        cmd = [sys.executable, str(Path(__file__).resolve()), "--pid", str(identity["pid"]),
               "--creation-ticks", str(ticks)]
        if full_memory:
            cmd.append("--full-memory")
        cmd.extend(["--out", str(path.resolve())])
        child = subprocess.run(cmd,
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
    parser.add_argument("--full-memory", action="store_true")
    args = parser.parse_args()
    try:
        write_dump(args.pid, args.creation_ticks, args.out, full_memory=args.full_memory)
    except Exception as error:
        print(json.dumps({"error": f"{type(error).__name__}: {error}"}), file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

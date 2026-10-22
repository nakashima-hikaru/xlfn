#!/usr/bin/env python3
"""Run the physical DLL reload probe in a disposable native Windows process."""

from __future__ import annotations

import argparse
import ctypes
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile


METRIC_FIELDS = (
    "abi_version",
    "process_reserved_bytes",
    "process_reserved_buffers",
    "process_peak_reserved_bytes",
    "process_published_bytes",
    "process_published_buffers",
    "process_peak_published_bytes",
    "process_limit_bytes",
)


class Metrics(ctypes.Structure):
    _fields_ = [(name, ctypes.c_uint64) for name in METRIC_FIELDS]

    def snapshot(self) -> dict[str, int]:
        return {name: int(getattr(self, name)) for name in METRIC_FIELDS}


def require(condition: bool, message: str) -> None:
    if not condition:
        raise RuntimeError(message)


class Windows:
    def __init__(self) -> None:
        from ctypes import wintypes

        self.dll = ctypes.WinDLL("kernel32", use_last_error=True)
        signatures = {
            "LoadLibraryW": (wintypes.HMODULE, [wintypes.LPCWSTR]),
            "FreeLibrary": (wintypes.BOOL, [wintypes.HMODULE]),
            "GetProcAddress": (ctypes.c_void_p, [wintypes.HMODULE, ctypes.c_char_p]),
            "GetModuleHandleW": (wintypes.HMODULE, [wintypes.LPCWSTR]),
            "GetCurrentProcess": (wintypes.HANDLE, []),
            "GetProcessHandleCount": (
                wintypes.BOOL,
                [wintypes.HANDLE, ctypes.POINTER(wintypes.DWORD)],
            ),
        }
        for name, (result, arguments) in signatures.items():
            function = getattr(self.dll, name)
            function.restype = result
            function.argtypes = arguments

    def error(self, operation: str) -> OSError:
        return ctypes.WinError(ctypes.get_last_error(), operation)

    def handles(self) -> int:
        from ctypes import wintypes

        count = wintypes.DWORD()
        if not self.dll.GetProcessHandleCount(
            self.dll.GetCurrentProcess(), ctypes.byref(count)
        ):
            raise self.error("GetProcessHandleCount")
        return int(count.value)


class Probe:
    def __init__(self, windows: Windows, path: Path) -> None:
        self.windows = windows
        self.path = path
        self.handle = windows.dll.LoadLibraryW(str(path))
        if not self.handle:
            raise windows.error(f"LoadLibraryW({path})")
        self.read = None
        self.allocate = None
        try:
            self.read = self.export(
                "fp12_probe_read_metrics", ctypes.POINTER(Metrics)
            )
            self.allocate = self.export(
                "fp12_probe_retain_allocation", ctypes.c_uint64
            )
        except BaseException:
            self.close()
            raise

    def export(self, name: str, argument):
        # x86 stdcall linkers may expose decorated names; x64 is undecorated.
        stack_bytes = ctypes.sizeof(argument)
        candidates = (name, f"_{name}@{stack_bytes}", f"{name}@{stack_bytes}")
        for candidate in candidates:
            address = self.windows.dll.GetProcAddress(
                self.handle, candidate.encode("ascii")
            )
            if address:
                return ctypes.WINFUNCTYPE(ctypes.c_int32, argument)(address)
        raise self.windows.error(f"GetProcAddress({name})")

    def metrics(self) -> dict[str, int]:
        output = Metrics()
        require(self.read(ctypes.byref(output)) == 1, "metrics attachment failed")
        snapshot = output.snapshot()
        require(snapshot["abi_version"] == 1, "unsupported probe snapshot ABI")
        return snapshot

    def close(self) -> None:
        if not self.handle:
            return
        # Discard callable pointers before their executable module disappears.
        self.read = None
        self.allocate = None
        if not self.windows.dll.FreeLibrary(self.handle):
            raise self.windows.error("FreeLibrary")
        self.handle = None
        require(
            not self.windows.dll.GetModuleHandleW(str(self.path)),
            f"DLL stayed loaded after FreeLibrary: {self.path}",
        )

    def __enter__(self):
        return self

    def __exit__(self, exception_type, exception, traceback) -> None:
        self.close()


def verify_success(
    before: dict[str, int], after: dict[str, int], charge: int | None, cells: int
) -> int:
    observed = after["process_reserved_bytes"] - before["process_reserved_bytes"]
    require(observed >= cells * 8 + 8, "allocation charge omits FP12 bytes")
    require(observed <= cells * 8 + 256, "unexpected private allocation header")
    if charge is not None:
        require(observed == charge, "allocation charge changed across images")
    require(
        after["process_reserved_buffers"] == before["process_reserved_buffers"] + 1,
        "reservation count did not increase exactly once",
    )
    require(
        after["process_peak_reserved_bytes"] == after["process_reserved_bytes"],
        "monotonic allocation peak does not equal retained bytes",
    )
    require(
        after["process_reserved_bytes"] <= after["process_limit_bytes"],
        "aggregate process cap was exceeded",
    )
    require(
        after["process_limit_bytes"] == before["process_limit_bytes"],
        "process cap changed across images",
    )
    require(
        all(after[field] == 0 for field in (
            "process_published_bytes",
            "process_published_buffers",
            "process_peak_published_bytes",
        )),
        "unpublished probe allocation was counted as published",
    )
    return observed


def run_worker(arguments) -> dict:
    dll = arguments.dll.resolve(strict=True)
    windows = Windows()
    rows = []
    with tempfile.TemporaryDirectory(prefix="xlfn-fp12-quota-") as temporary:
        paths = [dll]
        if not arguments.same_path_only:
            copied = Path(temporary) / "fp12_process_quota_independent_copy.dll"
            shutil.copyfile(dll, copied)
            paths.append(copied)

        with Probe(windows, dll) as probe:
            handles_before = windows.handles()
            baseline = probe.metrics()
            loaded_handles = windows.handles()
            require(
                loaded_handles == handles_before + 1,
                "first attachment must retain exactly one mapping handle",
            )
        unloaded_handles = windows.handles()
        require(baseline["process_reserved_bytes"] == 0, "worker ledger was not empty")
        require(baseline["process_reserved_buffers"] == 0, "worker had reservations")
        require(
            baseline["process_limit_bytes"] == 512 * 1024 * 1024,
            "probe expects the provisional 512 MiB process cap",
        )
        last = baseline
        charge = None

        def cycle() -> bool:
            nonlocal last, charge
            path = paths[len(rows) % len(paths)]
            with Probe(windows, path) as probe:
                before = probe.metrics()
                require(before == last, "ledger changed or reset across physical reload")
                require(
                    windows.handles() == loaded_handles,
                    "attachment retained an additional process handle",
                )
                success = probe.allocate(arguments.cells)
                require(success in (0, 1), "invalid allocation status")
                after = probe.metrics()
                require(
                    windows.handles() == loaded_handles,
                    "allocation or metrics query leaked a process handle",
                )
                if success:
                    charge = verify_success(before, after, charge, arguments.cells)
                else:
                    require(after == before, "rejected allocation changed the ledger")
                    require(charge is not None, "first allocation failed unexpectedly")
                    require(
                        before["process_limit_bytes"] - before["process_reserved_bytes"]
                        < charge,
                        "allocation failed before exhausting the aggregate cap",
                    )
                last = after
            require(
                windows.handles() == unloaded_handles,
                "physical unload retained an additional process handle",
            )
            rows.append({
                "iteration": len(rows) + 1,
                "image": "original" if path == dll else "independent_copy",
                "allocation_succeeded": bool(success),
                "physical_unload_verified": True,
                "process_reserved_bytes": after["process_reserved_bytes"],
                "process_reserved_buffers": after["process_reserved_buffers"],
            })
            return bool(success)

        for _ in range(arguments.reloads):
            require(cycle(), "requested reload count exhausted the quota early")
        rejected = False
        if arguments.exhaust_quota:
            # The first successful allocation fixes the exact architecture-
            # dependent charge. One further attempt must reach the shared cap.
            maximum_attempts = baseline["process_limit_bytes"] // charge + 1
            for _ in range(maximum_attempts):
                if not cycle():
                    rejected = True
                    break
            require(rejected, "allocation never reached the aggregate process cap")
            # A fresh image must still attach/read the complete rejected ledger.
            with Probe(windows, dll) as probe:
                require(probe.metrics() == last, "metrics unavailable after quota rejection")
                require(windows.handles() == loaded_handles, "final attachment leaked a handle")
            require(windows.handles() == unloaded_handles, "final unload leaked a handle")

        return {
            "status": "passed",
            "platform": sys.platform,
            "pointer_bits": ctypes.sizeof(ctypes.c_void_p) * 8,
            "cells_per_allocation": arguments.cells,
            "bytes_per_allocation_including_header": charge,
            "independent_image_copy_tested": len(paths) == 2,
            "initial_attachment_handle_delta": loaded_handles - handles_before,
            "stable_loaded_handle_count": loaded_handles,
            "stable_unloaded_handle_count": unloaded_handles,
            "quota_rejection_verified": rejected,
            "final_metrics": last,
            "iterations": rows,
        }


def emit(report: dict, destination: Path | None) -> None:
    rendered = json.dumps(report, indent=2)
    print(rendered, flush=True)
    if destination is not None:
        destination.write_text(rendered + "\n", encoding="utf-8")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("dll", nargs="?", type=Path, help="built native probe DLL")
    parser.add_argument("--reloads", type=int, default=8, help="successful reload allocations")
    parser.add_argument("--cells", type=int, default=1_048_576, help="cells in each column")
    parser.add_argument("--exhaust-quota", action="store_true", help="retain up to 512 MiB")
    parser.add_argument("--same-path-only", action="store_true", help="omit independent DLL copy")
    parser.add_argument("--report", type=Path, help="save the JSON result")
    parser.add_argument("--worker", action="store_true", help=argparse.SUPPRESS)
    arguments = parser.parse_args()
    if os.name != "nt":
        emit({"status": "skipped", "reason": "native Windows is required"}, arguments.report)
        return 0
    if arguments.dll is None:
        parser.error("the built native probe DLL is required")
    minimum_reloads = 2 if arguments.same_path_only else 4
    if arguments.reloads < minimum_reloads:
        parser.error(f"--reloads must be at least {minimum_reloads}")
    if not 1 <= arguments.cells <= 1_048_576:
        parser.error("--cells must be between 1 and 1048576")
    if not arguments.worker:
        # All intentionally retained allocations, the mapping view, and its
        # handle disappear with the worker. Never retain them in a caller host.
        return subprocess.run(
            [sys.executable, str(Path(__file__).resolve()), *sys.argv[1:], "--worker"],
            check=False,
        ).returncode
    try:
        report = run_worker(arguments)
    except Exception as error:
        emit({"status": "failed", "error": str(error)}, arguments.report)
        return 1
    emit(report, arguments.report)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

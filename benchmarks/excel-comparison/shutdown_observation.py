"""Passive, identity-bound shutdown timing shared by both benchmark fixtures."""
from __future__ import annotations

import ctypes
import json
import os
from pathlib import Path
import threading
import time
import uuid

from shutdown_diagnostics import ProcessHandle

FILETIME_EPOCH = 116444736000000000


def clock_stamp() -> dict:
    # GetProcessTimes reports FILETIME. Keep QPC as an independent monotonic
    # timeline, including the observer's scheduling delay after process exit.
    if os.name != "nt":
        return {"filetime_ticks": FILETIME_EPOCH + time.time_ns() // 100,
                "qpc_ticks": time.perf_counter_ns(), "qpc_frequency": 1_000_000_000}
    from ctypes import wintypes
    kernel = ctypes.WinDLL("kernel32", use_last_error=True)
    now = wintypes.FILETIME()
    counter, frequency = ctypes.c_longlong(), ctypes.c_longlong()
    kernel.GetSystemTimePreciseAsFileTime.argtypes = [ctypes.POINTER(wintypes.FILETIME)]
    kernel.GetSystemTimePreciseAsFileTime.restype = None
    kernel.QueryPerformanceCounter.argtypes = [ctypes.POINTER(ctypes.c_longlong)]
    kernel.QueryPerformanceCounter.restype = wintypes.BOOL
    kernel.QueryPerformanceFrequency.argtypes = [ctypes.POINTER(ctypes.c_longlong)]
    kernel.QueryPerformanceFrequency.restype = wintypes.BOOL
    kernel.GetSystemTimePreciseAsFileTime(ctypes.byref(now))
    if not kernel.QueryPerformanceCounter(ctypes.byref(counter)) or not kernel.QueryPerformanceFrequency(ctypes.byref(frequency)):
        raise ctypes.WinError(ctypes.get_last_error())
    return {"filetime_ticks": (now.dwHighDateTime << 32) | now.dwLowDateTime,
            "qpc_ticks": counter.value, "qpc_frequency": frequency.value}


def atomic_json(path: Path, value: dict) -> None:
    temporary = path.with_suffix(path.suffix + ".tmp")
    temporary.write_text(json.dumps(value), encoding="utf-8")
    temporary.replace(path)


def ready_path(pid_file: Path) -> Path:
    return pid_file.with_suffix(".observer-ready.json")


def await_observer(pid_file: Path, identity: dict, timeout_s: float = 5) -> dict:
    deadline = time.monotonic() + timeout_s
    while time.monotonic() < deadline:
        try:
            result = json.loads(ready_path(pid_file).read_text(encoding="utf-8"))
            if result.get("pid") != identity["pid"] or result.get("creation_ticks") != identity.get("creation_ticks"):
                return {"status": "error", "error": "process observer identity mismatch"}
            return result
        except FileNotFoundError:
            time.sleep(0.01)
        except (OSError, ValueError) as error:
            return {"status": "error", "error": f"process observer acknowledgement: {error}"}
    return {"status": "error", "error": "process observer did not acquire a handle before workload"}


class ProcessExitObserver:
    """Retain a native handle before RegisterXLL, even while COM release blocks.

    The observer never calls COM, terminates Excel, or changes the exit deadline.
    GetProcessTimes is read only after the same handle has become signaled.
    """
    def __init__(self, pid_file: Path):
        self.pid_file = pid_file
        self.stop = threading.Event()
        self.result = {"status": "unavailable", "error": "Excel identity was not published"}
        self.thread = threading.Thread(target=self._run, name="excel-exit-observer", daemon=True)

    def start(self):
        self.thread.start()
        return self

    def _run(self):
        identity = None
        try:
            while not self.stop.is_set():
                try:
                    identity = json.loads(self.pid_file.read_text(encoding="ascii"))
                    break
                except FileNotFoundError:
                    self.stop.wait(0.01)
            if identity is None:
                return
            ticks = identity.get("creation_ticks")
            if ticks is None:
                raise ValueError(identity.get("dump_identity_error", "native identity unavailable"))
            with ProcessHandle(identity["pid"], synchronize=True) as process:
                if process.creation_ticks() != ticks:
                    raise ValueError("Excel PID was reused; observation refused")
                self.result = {"status": "watching", "pid": identity["pid"], "creation_ticks": ticks}
                atomic_json(ready_path(self.pid_file), self.result)
                while True:
                    if process.wait(50):
                        observed = clock_stamp()
                        creation, exit_ticks = process.times()
                        if creation != ticks or not exit_ticks:
                            raise ValueError("invalid process exit identity/time")
                        self.result = self.result | {"status": "exited", "exit_filetime_ticks": exit_ticks,
                                                     "exit_observed": observed, "exit_code": process.exit_code()}
                        return
                    if self.stop.is_set():
                        self.result = self.result | {"status": "still_running"}
                        return
        except Exception as error:
            self.result = {"status": "error", "error": f"{type(error).__name__}: {error}"}
            if identity is not None:
                self.result.update(pid=identity.get("pid"), creation_ticks=identity.get("creation_ticks"))
                try:
                    atomic_json(ready_path(self.pid_file), self.result)
                except OSError:
                    pass

    def finish(self) -> dict:
        self.stop.set()
        self.thread.join(timeout=2)
        if self.thread.is_alive():
            return {"status": "error", "error": "process observer did not stop"}
        return dict(self.result)


def summarize_exit(record: dict, observation: dict, cleanup: dict) -> dict:
    result = dict(observation)
    quit_start = record.get("shutdown_timing", {}).get("quit_begin")
    result["termination"] = "forced" if cleanup.get("forced_kill") else (
        ("natural" if observation.get("exit_code") == 0 else "unforced_nonzero_exit")
        if observation.get("status") == "exited" else "unconfirmed")
    if "exit_timeout_s" in cleanup:
        result["parent_exit_deadline_missed"] = True
    if quit_start is None:
        result["measurement_error"] = "Quit was not observed"
    elif observation.get("status") == "exited":
        delta = (observation["exit_filetime_ticks"] - quit_start["filetime_ticks"]) / 10_000_000
        result["quit_to_exit_s"] = delta
        observed = observation["exit_observed"]
        if observed["qpc_frequency"] == quit_start["qpc_frequency"]:
            result["quit_to_exit_observed_s"] = (observed["qpc_ticks"] - quit_start["qpc_ticks"]) / observed["qpc_frequency"]
        if delta < 0:
            result["measurement_error"] = "exit predates Quit; inspect clock adjustment or premature exit"
        # Forced death is not a successful natural-shutdown latency sample.
        if result["termination"] == "natural" and delta >= 0:
            result["quit_to_natural_exit_s"] = delta
    return result


def lifecycle_control_path(pid: int) -> Path:
    return Path(os.environ["LOCALAPPDATA"]) / "xlfn" / "lifecycle-diagnostics" / f"{pid}.json"


def prepare_lifecycle_trace(identity: dict, output: Path) -> dict:
    if "creation_ticks" not in identity:
        raise ValueError("native identity unavailable; lifecycle tracing refused")
    token = uuid.uuid4().hex
    path = output.resolve()
    path.parent.mkdir(parents=True, exist_ok=True)
    path.touch(exist_ok=False)
    control = lifecycle_control_path(identity["pid"])
    control.parent.mkdir(parents=True, exist_ok=True)
    config = {"schema": 1, "pid": identity["pid"], "creation_ticks": identity["creation_ticks"],
              "token": token, "path": str(path)}
    atomic_json(control, config)
    return config | {"control_path": str(control)}


def collect_lifecycle_trace(config: dict, quit_start: dict | None) -> dict:
    result = {"path": config["path"], "status": "ok", "events": []}
    try:
        # Keep complete lines from a forcibly terminated writer, flag its tail.
        with Path(config["path"]).open("rb") as stream:
            raw = stream.read(4_000_001)
        if len(raw) > 4_000_000:
            raise ValueError("lifecycle trace exceeds 4 MB limit")
        for line in raw.splitlines():
            try:
                event = json.loads(line)
                if not isinstance(event, dict) or not isinstance(event.get("details", {}), dict):
                    raise ValueError("invalid lifecycle event")
                if event.get("schema") != 1 or event.get("source") not in ("xlfn", "excel_dna"):
                    raise ValueError("unsupported lifecycle event schema/source")
                if any(event.get(key) != config[key] for key in ("pid", "creation_ticks", "token")):
                    raise ValueError("lifecycle trace identity mismatch")
                if not isinstance(event.get("qpc_ticks"), int) or not isinstance(event.get("qpc_frequency"), int) or event["qpc_frequency"] <= 0:
                    raise ValueError("invalid lifecycle clock")
                if quit_start and event.get("qpc_frequency") == quit_start["qpc_frequency"]:
                    event["since_quit_s"] = (event["qpc_ticks"] - quit_start["qpc_ticks"]) / event["qpc_frequency"]
                result["events"].append(event)
            except (ValueError, KeyError, TypeError) as error:
                result.update(status="partial", error=str(error))
        if not result["events"] and result["status"] == "ok":
            result.update(status="unavailable", error="no lifecycle events; check fixture build and diagnostic initialization")
        boundary = "IExcelAddIn.AutoClose" if any(event.get("source") == "excel_dna" for event in result["events"]) else "xlAutoClose"
        result["boundary"] = boundary
        result["boundary_scope"] = "managed_callback_only" if boundary.startswith("IExcelAddIn") else "generated_native_body"
        entered, returned, incomplete = set(), set(), set()
        for event in result["events"]:
            name = event.get("event")
            invocation = event.get("details", {}).get("invocation")
            if name == boundary + ".enter": entered.add(invocation)
            elif name == boundary + ".return": returned.add(invocation)
            elif name == boundary + ".scope_exit": incomplete.add(invocation)
        result["unreturned_invocations"] = sorted(entered - returned, key=str)
        result["auto_close"] = (
            "entered_without_return" if entered - returned - incomplete else
            "scope_exit_without_return" if incomplete - returned else
            "returned" if entered and entered <= returned else
            "not_observed" if result["events"] else "instrumentation_unverified")
    except (OSError, ValueError) as error:
        result.update(status="error", error=str(error))
    finally:
        try:
            control = Path(config["control_path"])
            current = json.loads(control.read_text(encoding="utf-8"))
            if isinstance(current, dict) and current.get("token") == config["token"]:
                control.unlink()
        except (OSError, ValueError):
            pass
    return result

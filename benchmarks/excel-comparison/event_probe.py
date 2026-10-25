"""Interactive Windows Excel event-removal experiment; never qualifies production unload."""
from __future__ import annotations

import argparse
from datetime import datetime, timezone
import json
import os
from pathlib import Path
import platform
import subprocess
import time
import uuid

from artifact_manifest import sha256, verify
from run import ComObject, ComRetry
from shutdown_diagnostics import ProcessHandle, process_creation_ticks


def read_trace(config: dict, *, live: bool = False) -> list[dict]:
    path = Path(config["path"])
    for attempt in range(101 if live else 1):
        with path.open("rb") as stream:
            raw = stream.read(4_000_001)
        if not raw or raw.endswith(b"\n") or not live or attempt == 100:
            break
        # A worker may be appending the final line. Never silently discard it.
        time.sleep(0.01)
    if len(raw) > 4_000_000:
        raise ValueError("probe trace exceeds 4 MB")
    events = []
    for line in raw.splitlines():
        e = json.loads(line)
        if (e.get("schema") != 1 or e.get("source") != "event-probe"
                or any(e.get(k) != config[k] for k in ("pid", "creation_ticks", "token"))):
            raise ValueError("probe trace identity mismatch")
        events.append(e)
    return events


def assess(phases: list[dict], events: list[dict], cycles: int) -> dict:
    """Require positive controls on BOTH sides; absence alone never passes."""
    reasons = []
    names = [p["name"] for p in phases]
    expected = ["programmatic", "registered.normal", "registered.cancel"]
    for cycle in range(1, cycles + 1):
        expected.extend(f"{mode}{cycle}.{kind}" for mode in ("removed", "restored") for kind in ("normal", "cancel"))
    if names != expected:
        reasons.append("missing, reordered or duplicate observation phases")
    after_removal = False
    for p in phases:
        name = p["name"]
        before, after = p["before"], p["after"]
        generation = 1 if name.startswith(("programmatic", "registered", "removed1")) else (
            int(name.split(".")[0].removeprefix("removed")) if name.startswith("removed") else
            int(name.split(".")[0].removeprefix("restored")) + 1)
        if before["generation"] != generation or after["generation"] != generation:
            reasons.append(f"{name}: registration generation changed or mismatched")
        delta = {k: after[k] - before[k] for k in ("ended", "canceled", "started")}
        if before["active"] or after["active"] or delta["started"] != 1:
            reasons.append(f"{name}: expected exactly one completed workload window")
        callbacks = [e["event"] for e in p["events"] if e["event"].startswith("callback.")]
        if any(e["details"].get("handler_generation") != generation
               for e in p["events"] if e["event"].startswith("callback.")):
            reasons.append(f"{name}: callback from a stale handler generation")
        if callbacks.count("callback.ended") != delta["ended"] or callbacks.count("callback.canceled") != delta["canceled"]:
            reasons.append(f"{name}: callback trace and counters disagree")
        if name.endswith(".normal") or name == "programmatic":
            if type(p["cell_value"]) not in (int, float) or p["cell_value"] != 1:
                reasons.append(f"{name}: completed worksheet value missing")
        if name == "programmatic":
            if callbacks:
                reasons.append("programmatic control raised callbacks")
        elif name.startswith("removed"):
            after_removal |= bool(callbacks)
            if name.endswith(".cancel") and not callbacks:
                # Without the canceled handler, the worker tries its delayed return.
                # Require Excel's explicit invalid-context status, independently of
                # the operator's Esc assertion, to prove the cancellation stimulus.
                returns = [e for e in p["events"] if e["event"] == "task.return"]
                if not any(e["details"].get("invoked") and e["details"].get("transport") == 256 for e in returns):
                    reasons.append(f"{name}: cancellation not corroborated by invalid async context")
        elif name.endswith(".normal"):
            if delta["ended"] < 1 or delta["canceled"]:
                reasons.append(f"{name}: normal event positive control failed")
        elif name.endswith(".cancel"):
            if "callback.canceled" not in callbacks or "callback.ended" not in callbacks:
                reasons.append(f"{name}: cancellation event positive control failed")
            elif callbacks.index("callback.ended") < callbacks.index("callback.canceled"):
                reasons.append(f"{name}: unexpected cancellation/end ordering")
    operations = [e for e in events if e["event"] in ("event.register", "event.remove")]
    expected_operations = ["event.register"] * 2
    for _ in range(cycles):
        expected_operations += ["event.remove"] * 2 + ["event.register"] * 2
    if [e["event"] for e in operations] != expected_operations:
        reasons.append("incomplete registration/removal call sequence")
    for index, e in enumerate(operations):
        d = e["details"]
        if (not d.get("invoked") or d.get("transport") != 0 or d.get("cleanup") != 0
                or d.get("selector") != index % 2 + 1 or d.get("event_xltype") != 0x800):
            reasons.append("event call transport, arguments or cleanup failed")
        if e["event"] == "event.remove":
            if d.get("procedure_xltype") != 0x100 or d.get("procedure") is not None:
                reasons.append("removal did not use the nil-procedure convention")
        elif (d.get("raw", {}).get("xltype", 0) & 0xfff) != 0x800 or not d["raw"].get("value"):
            reasons.append("registration acknowledgement was not a nonzero integer")
    if reasons:
        verdict = "inconclusive"
    elif after_removal:
        verdict = "callbacks_observed_after_removal"
    else:
        verdict = "no_callbacks_observed_after_removal"
    return {"verdict": verdict, "reasons": reasons,
            "callbacks_observed_after_removal": after_removal,
            "production_shutdown_qualified": False,
            "scope": "Standalone pinned probe, command-context nil removal, normal/cancel controls and re-registration; not production xlAutoClose ordering or unload qualification"}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--artifacts", type=Path, required=True)
    parser.add_argument("--out", type=Path, default=Path("event-probe-results"))
    parser.add_argument("--cycles", type=int, choices=range(1, 4), default=2)
    args = parser.parse_args()
    if os.name != "nt":
        parser.error("Windows desktop Excel is required")
    manifest = verify(args.artifacts)
    commit = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=Path(__file__).parent, text=True).strip()
    if commit != manifest["commit"]:
        parser.error("runner checkout and CI artifact commits must match")
    root = args.out.resolve()
    root.mkdir(parents=True, exist_ok=False)
    xll = (args.artifacts / "event_probe/event-probe.xll").resolve()
    import pythoncom
    import psutil
    import win32com.client
    import win32process
    previous_pids = {p.pid for p in psutil.process_iter()}
    pythoncom.CoInitialize()
    app = book = sheet = cell = process = None
    config = control = None
    report = {"schema": 1, "timestamp_utc": datetime.now(timezone.utc).isoformat(),
              "os": platform.platform(), "runner_commit": commit, "manifest": manifest,
              "runner_sources_sha256": {name: sha256(Path(__file__).with_name(name)) for name in
                                        ("event_probe.py", "artifact_manifest.py", "run.py", "shutdown_diagnostics.py")},
              "xll_sha256": sha256(xll), "cycles": args.cycles, "phases": [], "status": "incomplete"}

    def save():
        (root / "report.json").write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")

    def command(name):
        if app.Run(name) != 1:
            raise RuntimeError(f"{name} refused or failed; inspect probe.jsonl")

    def snap():
        command("XEP.SNAPSHOT")
        return next(e["details"] for e in reversed(read_trace(config, live=True)) if e["event"] == "snapshot")

    def observe(name, cancel=False, programmatic=False):
        before = snap()
        start = len(read_trace(config, live=True))
        sheet.Range("B6").Value2 = 10_000 if cancel else 1_000
        cell.Dirty()
        sheet.Range("A3").Value2 = name
        if programmatic:
            app.CalculateFull()
        else:
            message = ("ExcelでF9を押し、計算開始後すぐにEscを押してください。" if cancel else
                       "ExcelでF9を押し、A6が1になるまで待ってください。")
            input(f"\n{name}: {message} 終わったら、この端末でEnter: ")
        deadline = time.monotonic() + 40
        while True:
            after = snap()
            if after["active"] == 0:
                break
            if time.monotonic() >= deadline:
                raise TimeoutError("probe workers did not drain")
            time.sleep(0.1)
        phase = {"name": name, "before": before, "after": after,
                 "cell_value": cell.Value2, "events": read_trace(config, live=True)[start:]}
        report["phases"].append(phase)
        save()
        print(f"  ended +{after['ended']-before['ended']}, canceled +{after['canceled']-before['canceled']}, workload +{after['started']-before['started']}")
        return phase

    try:
        app = ComObject(win32com.client.DispatchEx("Excel.Application"), ComRetry(timeout_s=40))
        _, pid = win32process.GetWindowThreadProcessId(app.Hwnd)
        if pid in previous_pids:
            # Do not modify or quit a user's pre-existing Excel process.
            app = None
            raise RuntimeError("Excel reused an existing process; no experiment performed")
        process = ProcessHandle(pid, synchronize=True)
        config = {"schema": 1, "pid": pid, "creation_ticks": process_creation_ticks(pid),
                  "token": uuid.uuid4().hex, "path": str(root / "probe.jsonl")}
        Path(config["path"]).touch(exist_ok=False)
        control = Path(os.environ["LOCALAPPDATA"]) / "xlfn/event-probe" / f"{pid}.json"
        control.parent.mkdir(parents=True, exist_ok=True)
        control.write_text(json.dumps(config), encoding="utf-8")
        report["identity"] = config
        app.Visible = True
        book = app.Workbooks.Add()
        app.Calculation = -4135
        app.MultiThreadedCalculation.Enabled = False
        report.update(excel_version=str(app.Version), excel_build=str(app.Build),
                      excel_bitness=str(app.OperatingSystem), excel_executable=psutil.Process(pid).exe(),
                      excel_update_channel=None, calculation_mode="manual", threads=1)
        sheet = book.Worksheets(1)
        sheet.Range("A1").Value2 = "イベント解除検証 — 端末の案内に従ってF9 / Escを操作"
        sheet.Range("A5").Value2 = "結果（通常完了は1）"
        sheet.Range("B5").Value2 = "待機ms"
        sheet.Columns("A").ColumnWidth = 65
        sheet.Columns("B").ColumnWidth = 16
        if not app.RegisterXLL(str(xll)):
            raise RuntimeError("probe RegisterXLL failed; inspect probe.jsonl")
        cell = sheet.Range("A6")
        sheet.Range("B6").Value2 = 1_000
        cell.Formula = "=XEP.DELAY(B6)"
        observe("programmatic", programmatic=True)
        observe("registered.normal")
        observe("registered.cancel", cancel=True)
        # A failed initial control cannot establish absence after removal.
        controls = report["phases"][1:]
        if any(p["after"]["ended"] <= p["before"]["ended"] for p in controls) or controls[1]["after"]["canceled"] <= controls[1]["before"]["canceled"]:
            raise RuntimeError("initial interactive controls failed; no removal attempted")
        for cycle in range(1, args.cycles + 1):
            command("XEP.REMOVE")
            observe(f"removed{cycle}.normal")
            observe(f"removed{cycle}.cancel", cancel=True)
            command("XEP.REGISTER")
            observe(f"restored{cycle}.normal")
            observe(f"restored{cycle}.cancel", cancel=True)
        report["assessment"] = assess(report["phases"], read_trace(config), args.cycles)
        report["status"] = "complete"
    except (Exception, KeyboardInterrupt) as error:
        report["error"] = f"{type(error).__name__}: {error}"
        report["assessment"] = {"verdict": "inconclusive", "production_shutdown_qualified": False}
    finally:
        save()
        if app is not None:
            try:
                app.DisplayAlerts = False
                if book is not None:
                    book.Close(SaveChanges=False)
                app.Quit()
            except Exception as error:
                report["cleanup_error"] = str(error)
        cell = sheet = book = app = None
        import gc
        gc.collect()
        if process is not None:
            try:
                report["process_exited"] = process.wait(10_000)
            finally:
                process.__exit__()
        if config is not None:
            try:
                report["events"] = read_trace(config)
                report["trace_sha256"] = sha256(Path(config["path"]))
            except Exception as error:
                report["trace_error"] = str(error)
                report["status"] = "incomplete"
                report["assessment"] = {"verdict": "inconclusive", "production_shutdown_qualified": False}
        if control is not None:
            control.unlink(missing_ok=True)
        save()
        pythoncom.CoUninitialize()
    print(json.dumps(report["assessment"], ensure_ascii=False, indent=2))
    print(f"結果: {root / 'report.json'}\n生ログ: {root / 'probe.jsonl'}")
    return 0 if report["status"] == "complete" else 1


if __name__ == "__main__":
    raise SystemExit(main())

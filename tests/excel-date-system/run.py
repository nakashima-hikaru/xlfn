#!/usr/bin/env python3
"""Qualify workbook date-system callbacks in an isolated Windows Excel process.

This script deliberately imports pywin32 only when running the native probe, so
syntax and the calendar oracle can be checked on other platforms. Native Excel
execution is the qualification gate; compiling the fixture does not satisfy it.
"""

from __future__ import annotations

import argparse
import datetime as dt
import gc
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import subprocess
import sys
import tempfile
import time
import traceback
from typing import Any


SERIALS = (-0.25, 0.0, 1.0, 59.5, 59.999999999, 60.0, 60.5,
           60.999999999, 61.0, 61.5, 61.999999999, 62.0, 1462.0)
PHASES = (
    ("baseline", (False, True)),
    ("settings_swapped", (True, False)),
    ("settings_restored", (False, True)),
)


def digest(path: Path) -> str:
    value = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            value.update(chunk)
    return value.hexdigest()


def expected_date(serial: float, mac1904: bool) -> str | None:
    """Independent Gregorian oracle; None denotes Excel's fictitious leap day."""
    day = math.floor(serial)
    if not mac1904 and day == 60:
        return None
    epoch = dt.date(1904, 1, 1) if mac1904 else dt.date(1899, 12, 31)
    if not mac1904 and day > 60:
        day -= 1
    return (epoch + dt.timedelta(days=day)).isoformat()


def source_identity(root: Path) -> dict[str, Any]:
    def git(*args: str) -> bytes:
        return subprocess.check_output(["git", "-C", str(root), *args], stderr=subprocess.PIPE)

    try:
        revision = git("rev-parse", "HEAD").decode().strip()
        diff = git("diff", "--binary", "HEAD", "--")
        paths = git("ls-files", "--others", "--exclude-standard", "-z").split(b"\0")
        untracked = {
            os.fsdecode(name): digest(root / os.fsdecode(name))
            for name in paths if name and (root / os.fsdecode(name)).is_file()
        }
        return {
            "root": str(root),
            "revision": revision,
            "working_diff_sha256": hashlib.sha256(diff).hexdigest(),
            "working_diff_bytes": len(diff),
            "untracked_sha256": untracked,
            "note": "Observed checkout identity; artifact build association requires supplied provenance.",
        }
    except (OSError, subprocess.CalledProcessError) as error:
        return {"root": str(root), "error": str(error)}


class Evidence:
    def __init__(self, path: Path):
        path.parent.mkdir(parents=True, exist_ok=True)
        self.stream = path.open("x", encoding="utf-8")
        self.sequence = 0

    def emit(self, event: str, **fields: Any) -> None:
        record = {
            "schema_version": 1,
            "sequence": self.sequence,
            "timestamp_utc": dt.datetime.now(dt.timezone.utc).isoformat(),
            "event": event,
            **fields,
        }
        self.sequence += 1
        self.stream.write(json.dumps(record, ensure_ascii=False, allow_nan=False) + "\n")
        self.stream.flush()

    def close(self) -> None:
        self.stream.close()


def qualified_reference(book: Any, sheet: Any, row: int) -> str:
    name = f"[{book.Name}]{sheet.Name}".replace("'", "''")
    return f"'{name}'!$A${row}"


def qualify(args: argparse.Namespace, evidence: Evidence) -> tuple[int, int, dict[str, Any]]:
    import pythoncom
    import win32api
    import win32com.client
    import win32event
    import win32process

    pythoncom.CoInitialize()
    excel = None
    may_quit_excel = False
    process_handle = None
    temporary_directory = None
    owned_books: list[Any] = []
    sheets: list[Any] = []
    checks = 0
    failures = 0
    cleanup: dict[str, Any] = {"quit_requested": False, "process_exited": None, "failures": []}
    case: dict[str, Any] = {}
    try:
        excel = win32com.client.DispatchEx("Excel.Application")
        _, pid = win32process.GetWindowThreadProcessId(int(excel.Hwnd))
        # A retained process handle distinguishes process exit from COM release.
        process_handle = win32api.OpenProcess(0x00100000, False, pid)  # SYNCHRONIZE
        evidence.emit(
            "excel_started", pid=pid, hwnd=int(excel.Hwnd), version=str(excel.Version),
            build=str(excel.Build), installation_path=str(excel.Path),
            operating_system=str(excel.OperatingSystem), initial_workbooks=int(excel.Workbooks.Count),
            ownership="isolated DispatchEx instance",
        )
        if int(excel.Workbooks.Count) != 0:
            raise RuntimeError("New Excel instance already has workbooks; refusing to modify them")
        may_quit_excel = True
        excel.Visible = False
        excel.DisplayAlerts = False
        excel.AskToUpdateLinks = False
        temporary_directory = tempfile.TemporaryDirectory(prefix="xlfn-date-system-")
        directory = temporary_directory.name
        for filename in ("dates-1900.xlsx", "dates-1904-O'Brien.xlsx", "formula-host.xlsx"):
            book = excel.Workbooks.Add(-4167)  # xlWBATWorksheet: one sheet
            owned_books.append(book)
            sheet = book.Worksheets(1)
            sheets.append(sheet)
            sheet.Name = "Dates O'Brien"
            sheet.Range(f"A1:A{len(SERIALS)}").Value2 = tuple((serial,) for serial in SERIALS)
            book.SaveAs(str(Path(directory) / filename), FileFormat=51)  # xlOpenXMLWorkbook
        excel.Calculation = -4135  # xlCalculationManual
        if not excel.RegisterXLL(str(args.xll)):
            raise RuntimeError("RegisterXLL returned False")
        evidence.emit("xll_registered", xll=str(args.xll), xll_sha256=digest(args.xll))

        # Every source is read from every formula workbook. Explicit workbook
        # qualifiers include spaces/apostrophes, including local references.
        for source_index, source in enumerate(owned_books[:2]):
            source_sheet = sheets[source_index]
            system_column, date_column = (("C", "D"), ("F", "G"))[source_index]
            for host_index, host in enumerate(owned_books):
                host_sheet = sheets[host_index]
                for row in range(1, len(SERIALS) + 1):
                    reference = qualified_reference(source, source_sheet, row)
                    case = {"stage": "assign_formula", "source": str(source.Name),
                            "formula_host": str(host.Name), "row": row}
                    host_sheet.Range(f"{system_column}{row}").Formula2 = (
                        f"=FRAMEWORK.DATE.SYSTEM({reference})"
                    )
                    host_sheet.Range(f"{date_column}{row}").Formula2 = (
                        f"=FRAMEWORK.DATE.YMD({reference},{reference})"
                    )

        for phase, settings in PHASES:
            for index, mac1904 in enumerate((*settings, False)):
                book = owned_books[index]
                case = {"stage": "set_workbook_date_system", "phase": phase,
                        "workbook": str(book.Name), "expected_date1904": mac1904}
                book.Date1904 = mac1904
                actual = bool(book.Date1904)
                evidence.emit("date_system_setting", **case, observed_date1904=actual)
                if actual != mac1904:
                    raise RuntimeError("Date1904 readback differs from the assigned workbook setting")
            for active_index, active in enumerate(owned_books):
                active.Activate()
                case = {"stage": "calculate", "phase": phase, "active_workbook": str(active.Name)}
                excel.CalculateFullRebuild()
                if int(excel.CalculationState) != 0:  # xlDone
                    raise RuntimeError("CalculateFullRebuild returned before calculation completed")
                for source_index, source in enumerate(owned_books[:2]):
                    mac1904 = settings[source_index]
                    source_sheet = sheets[source_index]
                    system_column, date_column = (("C", "D"), ("F", "G"))[source_index]
                    for host_index, host in enumerate(owned_books):
                        host_sheet = sheets[host_index]
                        for row, requested_serial in enumerate(SERIALS, start=1):
                            source_serial = float(source_sheet.Range(f"A{row}").Value2)
                            system = host_sheet.Range(f"{system_column}{row}").Value2
                            calendar = host_sheet.Range(f"{date_column}{row}").Value2
                            error_type = int(host_sheet.Evaluate(
                                f"IFERROR(ERROR.TYPE({date_column}{row}),0)"
                            ))
                            expected_system = 1904 if mac1904 else 1900
                            expected_calendar = expected_date(source_serial, mac1904)
                            date_passed = (
                                error_type == 3 if expected_calendar is None else
                                error_type == 0 and calendar == expected_calendar
                            )
                            passed = system == expected_system and date_passed
                            checks += 1
                            failures += int(not passed)
                            evidence.emit(
                                "case", phase=phase, active_workbook=str(active.Name),
                                source_workbook=str(source.Name), formula_workbook=str(host.Name),
                                source_is_active=source_index == active_index,
                                source_is_formula_host=source_index == host_index,
                                source_date1904=bool(source.Date1904),
                                formula_date1904=bool(host.Date1904),
                                row=row, requested_serial=requested_serial, source_serial=source_serial,
                                system_formula=str(host_sheet.Range(f"{system_column}{row}").Formula2),
                                date_formula=str(host_sheet.Range(f"{date_column}{row}").Formula2),
                                system=system, expected_system=expected_system,
                                calendar=calendar, calendar_error_type=error_type,
                                expected_calendar=expected_calendar,
                                expected_calendar_error_type=3 if expected_calendar is None else 0,
                                passed=passed,
                            )
        return checks, failures, cleanup
    except BaseException as error:
        evidence.emit("native_failure", case=case, error_type=type(error).__name__,
                      error=str(error), traceback=traceback.format_exc())
        raise
    finally:
        for book in reversed(owned_books):
            try:
                book.Close(SaveChanges=False)
            except Exception as error:
                cleanup["failures"].append(f"close_owned_workbook: {error}")
                evidence.emit("cleanup_failure", stage="close_owned_workbook", error=str(error))
        owned_books.clear()
        sheets.clear()
        if excel is not None and may_quit_excel:
            try:
                excel.Quit()
                cleanup["quit_requested"] = True
                evidence.emit("excel_quit_requested")
            except Exception as error:
                cleanup["failures"].append(f"quit_owned_excel: {error}")
                evidence.emit("cleanup_failure", stage="quit_owned_excel", error=str(error))
        elif excel is not None:
            evidence.emit("excel_quit_skipped", reason="Startup workbook ownership was not verified; instance left untouched")
        # Release all local COM proxies before observing physical process exit.
        excel = None
        book = sheet = source = source_sheet = host = host_sheet = active = None
        gc.collect()
        pythoncom.CoUninitialize()
        if process_handle is not None:
            try:
                exited = win32event.WaitForSingleObject(process_handle, 10000) == 0
                cleanup["process_exited"] = exited
                evidence.emit("process_exit_observed", exited=exited, timeout_ms=10000)
                if may_quit_excel and not exited:
                    cleanup["failures"].append("Owned Excel process exit was not observed within 10000 ms")
            finally:
                process_handle.Close()
        if temporary_directory is not None:
            try:
                temporary_directory.cleanup()
            except Exception as error:
                cleanup["failures"].append(f"remove_owned_temporary_files: {error}")
                evidence.emit("cleanup_failure", stage="remove_owned_temporary_files", error=str(error))


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--xll", required=True, type=Path, help="FrameworkFixture XLL built for this Excel architecture")
    parser.add_argument("--output", required=True, type=Path, help="New JSONL evidence path; existing files are never overwritten")
    parser.add_argument("--source-root", type=Path, default=Path(__file__).resolve().parents[2])
    parser.add_argument("--source-revision", help="Revision used to build the XLL, supplied by its builder")
    parser.add_argument("--source-diff-sha256", help="SHA-256 of `git diff --binary HEAD` at XLL build time")
    args = parser.parse_args()
    if sys.platform != "win32":
        parser.error("Native qualification requires Windows, desktop Excel, and pywin32")
    args.xll = args.xll.resolve(strict=True)
    if args.source_diff_sha256 and (
        len(args.source_diff_sha256) != 64 or
        any(character not in "0123456789abcdefABCDEF" for character in args.source_diff_sha256)
    ):
        parser.error("--source-diff-sha256 must be a 64-digit hexadecimal SHA-256")
    if bool(args.source_revision) != bool(args.source_diff_sha256):
        parser.error("Supply --source-revision and --source-diff-sha256 together")
    identity = source_identity(args.source_root.resolve())
    evidence = Evidence(args.output.resolve())
    started = time.monotonic()
    try:
        evidence.emit(
            "provenance", xll=str(args.xll), xll_sha256=digest(args.xll),
            xll_bytes=args.xll.stat().st_size, runner_sha256=digest(Path(__file__)),
            python=sys.version, platform=platform.platform(), checkout=identity,
            artifact_source={"revision": args.source_revision,
                             "working_diff_sha256": args.source_diff_sha256,
                             "origin": "user-supplied build provenance" if args.source_revision else "unavailable"},
            serials=SERIALS, phases=PHASES,
        )
        checks, failures, cleanup = qualify(args, evidence)
        date_passed = failures == 0 and checks == 702
        cleanup_passed = not cleanup["failures"] and cleanup["process_exited"] is True
        status = "failed" if not date_passed else "passed" if cleanup_passed else "cleanup_failed"
        evidence.emit("summary", status=status,
                      checks=checks, failures=failures, elapsed_s=time.monotonic() - started,
                      date_system_status="passed" if date_passed else "failed",
                      cleanup_status="passed" if cleanup_passed else "failed", cleanup=cleanup,
                      scope="Workbook date-system and serial-day native Excel qualification")
        print(f"{checks} checks; {failures} comparison failures; status: {status}; evidence: {args.output.resolve()}")
        return 0 if status == "passed" else 1
    except BaseException as error:
        evidence.emit("summary", status="incomplete", elapsed_s=time.monotonic() - started,
                      error_type=type(error).__name__, error=str(error))
        print(f"Native qualification incomplete: {error}; evidence: {args.output.resolve()}", file=sys.stderr)
        return 1
    finally:
        evidence.close()


if __name__ == "__main__":
    raise SystemExit(main())

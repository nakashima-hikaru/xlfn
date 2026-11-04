"""Write the Excel-DNA counterpart of xlfn/build.rs registration fixtures."""

from __future__ import annotations

from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
DEFAULT_OUT = ROOT / "excel_dna" / "GeneratedRegistration.cs"


def source(count: int) -> str:
    if not 0 <= count <= 5_000:
        raise ValueError("extra registration count must be 0..5000")
    lines = [
        "using ExcelDna.Integration;",
        "namespace ExcelComparison;",
        "public static class GeneratedRegistration",
        "{",
    ]
    for index in range(count):
        lines.extend(
            (
                f'    [ExcelFunction(Name = "BENCH.EXTRA.{index:04}", IsThreadSafe = true)]',
                f"    public static double Extra{index:04}(double value) => value;",
            )
        )
    lines.append("}")
    return "\n".join(lines) + "\n"

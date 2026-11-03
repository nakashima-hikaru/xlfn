"""Write the Excel-DNA counterpart of xlfn/build.rs registration fixtures."""

import argparse
from pathlib import Path


def source(count: int) -> str:
    if not 0 <= count <= 5_000:
        raise ValueError("extra registration count must be 0..5000")
    lines = ["using ExcelDna.Integration;", "namespace ExcelComparison;",
             "public static class GeneratedRegistration", "{"]
    for index in range(count):
        lines.extend((
            f'    [ExcelFunction(Name = "BENCH.EXTRA.{index:04}", IsThreadSafe = true)]',
            f"    public static double Extra{index:04}(double value) => value;",
        ))
    lines.append("}")
    return "\n".join(lines) + "\n"


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("count", type=int)
    parser.add_argument("--out", type=Path,
                        default=Path(__file__).parent / "excel_dna" / "GeneratedRegistration.cs")
    args = parser.parse_args()
    args.out.write_text(source(args.count), encoding="utf-8")


if __name__ == "__main__":
    main()

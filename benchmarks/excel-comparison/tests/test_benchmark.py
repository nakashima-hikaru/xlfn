"""Unit tests for xlfn-excel-comparison pure benchmark logic."""

from __future__ import annotations

import json
import struct
import tempfile
import unittest
from pathlib import Path

from typer.testing import CliRunner

from xlfn_excel_comparison import (
    artifact_manifest,
    cli,
    generate_registration,
    runner,
    summarize,
    workloads,
)


def fake_x64_pe(tag: str) -> bytes:
    mz = bytearray(64)
    mz[:2] = b"MZ"
    mz[0x3C:0x40] = struct.pack("<I", 64)
    pe = b"PE\0\0" + struct.pack("<H", 0x8664)
    return bytes(mz) + pe + tag.encode("ascii")


def write_fake_ci_artifacts(root: Path, *, include_symbols: bool = False) -> None:
    for relative in artifact_manifest.FILES:
        path = root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(fake_x64_pe(relative))
    if include_symbols:
        for impl in ("xlfn", "excel_dna"):
            path = root / "symbols" / impl / "benchmark.pdb"
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(f"pdb:{impl}".encode("ascii"))


class WorkloadTests(unittest.TestCase):
    def test_every_id_has_cases_and_primary_metric(self) -> None:
        full_cases = workloads.cases("full")
        smoke_cases = workloads.cases("smoke")
        self.assertEqual({case.id for case in full_cases}, set(workloads.IDS))
        self.assertEqual({case.id for case in smoke_cases}, set(workloads.IDS))
        self.assertEqual(set(summarize.PRIMARY), set(workloads.IDS))
        self.assertLess(len(smoke_cases), len(full_cases))

    def test_gated_async_cases_stay_within_thread_budget(self) -> None:
        for profile in ("smoke", "full"):
            for case in workloads.cases(profile):
                if case.id in ("A03", "A04", "A05"):
                    self.assertLessEqual(case.params["cells"], 4_096)

    def test_formulas_cover_scalar_text_async_rtd_and_native_abi(self) -> None:
        self.assertEqual(runner.formula("S01", 3, {"cells": 10}), "=BENCH.ID(3)")
        self.assertEqual(runner.formula("S02", 2, {"argc": 4}), "=BENCH.SUM4(2,3,4,5)")
        self.assertEqual(runner.formula("S03", 5, {"delay_us": 10}), "=BENCH.CPU(5,10)")
        self.assertEqual(runner.formula("S04", 7, {"period": 2}), "=BENCH.ERRNUM(7,2)")
        self.assertEqual(
            runner.formula("T01", 4, {"kind": "ascii", "length": 8}),
            "=BENCH.STR.IN(B4)",
        )
        self.assertEqual(
            runner.formula("T02", 4, {"kind": "ja", "length": 16}),
            "=BENCH.STR.OUT(16,TRUE)",
        )
        self.assertEqual(
            runner.formula("A01", 2, {"cells": 10, "delay_us": 1000}),
            "=BENCH.ASYNC(2,1000)",
        )
        self.assertEqual(
            runner.formula("R01", 3, {"cells": 10, "topics": 2}),
            '=BENCH.RTD("topic-0",0)',
        )
        n01 = workloads.Case("N01", "f64-4", {"cells": 10, "argc": 4})
        self.assertEqual(
            runner.native_abi_formula(n01, 2, "NATIVE"),
            "=BENCH.ABI.F644.NATIVE(2,3,4,5)",
        )
        n05 = workloads.Case("N05", "matrix", {"rows": 10, "cols": 10})
        self.assertEqual(
            runner.native_abi_formula(n05, 1, "Q", "$E$1:$N$10"),
            "=BENCH.ABI.MATRIX.Q($E$1:$N$10)",
        )


class RegistrationGeneratorTests(unittest.TestCase):
    def test_generated_registration_matches_rust_build_script(self) -> None:
        rust_build = (runner.ROOT / "xlfn" / "build.rs").read_text(encoding="utf-8")
        self.assertIn("BENCH.EXTRA.{index:04}", rust_build)
        cs = generate_registration.source(3)
        self.assertIn('Name = "BENCH.EXTRA.0002", IsThreadSafe = true', cs)
        self.assertIn("public static double Extra0002(double value) => value;", cs)
        with self.assertRaises(ValueError):
            generate_registration.source(-1)
        with self.assertRaises(ValueError):
            generate_registration.source(5_001)


class ArtifactManifestTests(unittest.TestCase):
    def test_create_and_verify_manifest(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            write_fake_ci_artifacts(root, include_symbols=True)
            created = artifact_manifest.create(root, "0123456789abcdef", "99", "2")
            verified = artifact_manifest.verify(root, commit="0123456789abcdef", run_id="99")
            artifact_manifest.verify_symbols(root, verified, required=("xlfn", "excel_dna"))
            self.assertEqual(created, verified)

            (root / "xlfn" / "benchmark.xll").write_bytes(fake_x64_pe("tampered"))
            with self.assertRaisesRegex(ValueError, "artifact checksum mismatch"):
                artifact_manifest.verify(root)


class SummarizeTests(unittest.TestCase):
    @staticmethod
    def _base_record(
        id: str,
        variant: str,
        impl: str,
        metrics: dict[str, float],
        **overrides: object,
    ) -> dict[str, object]:
        record: dict[str, object] = {
            "id": id,
            "variant": variant,
            "implementation": impl,
            "status": "ok",
            "params": {"cells": 100},
            "profile": "smoke",
            "repeat": 5,
            "threads": 1,
            "rtd_throttle_ms": 100,
            "host": "bench-host",
            "os": "Windows-11",
            "excel_version": "16.0",
            "excel_build": "17000",
            "excel_bitness": "Windows (64-bit) NT 10.00",
            "calculation_mode": "manual",
            "artifact_source": "github-actions",
            "artifact_architecture": "x86_64",
            "ci_commit": "0123456789abcdef",
            "ci_run_id": "42",
            "ci_run_attempt": "1",
            "runner_commit": "0123456789abcdef",
            "runner_sources": {"runner.py": "abc"},
            "metrics": metrics,
        }
        record.update(overrides)
        return record

    def test_summarize_pairs_compatible_records_and_flags_mismatches(self) -> None:
        rows = summarize.summarize(
            [
                self._base_record("S01", "100", "xlfn", {"p50_s": 0.01}),
                self._base_record("S01", "100", "excel_dna", {"p50_s": 0.02}),
                self._base_record("P01", "2", "xlfn", {"calls_per_s": 200.0}, threads=2),
                self._base_record("P01", "2", "excel_dna", {"calls_per_s": 100.0}, threads=1),
                self._base_record(
                    "A05",
                    "recalc",
                    "xlfn",
                    {},
                    status="unsupported",
                    unsupported_reason="interactive only",
                ),
                self._base_record(
                    "A05", "recalc", "excel_dna", {"task_drain_after_release_s": 0.25}
                ),
            ]
        )
        by_id = {row["id"]: row for row in rows}
        self.assertTrue(by_id["S01"]["comparable"])
        self.assertEqual(by_id["S01"]["xlfn_advantage_ratio"], 2.0)
        self.assertFalse(by_id["P01"]["comparable"])
        self.assertEqual(by_id["P01"]["reason"], "measurement settings or Excel build differ")
        self.assertFalse(by_id["A05"]["comparable"])
        self.assertIn("xlfn: interactive only", by_id["A05"]["reason"])

    def test_summarize_handles_within_xlfn_comparisons(self) -> None:
        rows = summarize.summarize(
            [
                self._base_record(
                    "M06",
                    "10x10",
                    "xlfn",
                    {
                        "fp12_p50_s": 0.8,
                        "q_p50_s": 1.0,
                        "fp12_over_q_ratio": 0.8,
                        "fp12_change_percent": -20.0,
                    },
                ),
                self._base_record(
                    "N01",
                    "f64-1",
                    "xlfn",
                    {
                        "native_p50_s": 0.9,
                        "q_p50_s": 1.0,
                        "native_over_q_ratio": 0.9,
                        "native_change_percent": -10.0,
                    },
                ),
            ]
        )
        by_id = {row["id"]: row for row in rows}
        self.assertEqual(by_id["M06"]["fp12_over_q_ratio"], 0.8)
        self.assertEqual(by_id["M06"]["comparison"], "within_xlfn_fp12_vs_q")
        self.assertEqual(by_id["N01"]["native_over_q_ratio"], 0.9)
        self.assertEqual(by_id["N01"]["comparison"], "within_xlfn_native_vs_q")


class CliTests(unittest.TestCase):
    def test_run_plan_outputs_selected_cases(self) -> None:
        cli_runner = CliRunner()
        result = cli_runner.invoke(cli.app, ["run", "--profile", "smoke", "--id", "S01", "--plan"])
        self.assertEqual(result.exit_code, 0, msg=result.output)
        lines = [json.loads(line) for line in result.stdout.splitlines() if line.strip()]
        self.assertEqual([item["key"] for item in lines], ["S01/100"])


if __name__ == "__main__":
    unittest.main()

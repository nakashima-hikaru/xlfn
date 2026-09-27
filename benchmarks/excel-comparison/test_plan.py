import struct
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import Mock

from artifact_manifest import FILES, create, verify
from generate_registration import source
from run import XL_ERR_NUM, check_scalar, formula, register_xll
from summarize import PRIMARY, summarize
from workloads import IDS, Case, cases


class PlanTest(unittest.TestCase):
    def test_every_requested_id_has_a_case_and_primary_metric(self):
        expected = {f"S{i:02}" for i in range(1, 5)}
        expected |= {f"M{i:02}" for i in range(1, 6)}
        expected |= {f"T{i:02}" for i in range(1, 4)}
        expected |= {f"P{i:02}" for i in range(1, 5)}
        expected |= {f"A{i:02}" for i in range(1, 7)}
        expected |= {f"R{i:02}" for i in range(1, 8)}
        expected |= {f"C{i:02}" for i in range(1, 5)}
        expected |= {f"W{i:02}" for i in range(1, 4)}
        expected |= {f"L{i:02}" for i in range(1, 4)}
        self.assertEqual(set(IDS), expected)
        self.assertEqual(set(PRIMARY), expected)
        for profile in ("full", "smoke"):
            plan = cases(profile)
            self.assertEqual(len(plan), len({case.key for case in plan}))
            self.assertEqual({case.id for case in plan}, expected)

    def test_extreme_cases(self):
        plan = {case.key: case for case in cases()}
        self.assertEqual(plan["S01/100000"].params["cells"], 100_000)
        self.assertEqual(plan["M04/1000x10"].params["rows"] * plan["M04/1000x10"].params["cols"], 10_000)
        self.assertEqual(plan["M04/100x100"].params["rows"] * plan["M04/100x100"].params["cols"], 10_000)
        self.assertEqual(plan["M04/10x1000"].params["rows"] * plan["M04/10x1000"].params["cols"], 10_000)
        self.assertEqual(plan["R01/100000"].params["topics"], 100_000)
        self.assertEqual(plan["C02/5000"].params["registrations"], 5_000)
        self.assertEqual(plan["R07/long"].params["duration_s"], 1_800)

    def test_formula_keys(self):
        self.assertEqual(formula("S02", 3, {"argc": 4}), "=BENCH.SUM4(3,4,5,6)")
        self.assertEqual(formula("S04", 3, {"period": 2}), "=BENCH.ERRNUM(3,2)")
        self.assertEqual(formula("R03", 3, {"cells": 10, "topics": 2}),
                         '=BENCH.RTD("topic-0",0)')
        self.assertEqual(formula("T02", 1, {"length": 8, "kind": "ja"}),
                         "=BENCH.STR.OUT(8,TRUE)")

    def test_error_workload_checks_each_cell(self):
        class Target:
            def __init__(self, values):
                self.Value2 = tuple((value,) for value in values)

        case = Case("S04", "period-2", {"cells": 4, "period": 2})
        check_scalar(Target([1.0, XL_ERR_NUM, 3.0, XL_ERR_NUM]), case)
        with self.assertRaises(AssertionError):
            check_scalar(Target([XL_ERR_NUM, 2.0, XL_ERR_NUM, 4.0]), case)
        with self.assertRaises(AssertionError):
            check_scalar(Target([-2146826259] * 4), Case("S04", "period-0", {"cells": 4, "period": 0}))

    def test_scalar_checks_reject_excel_errors_and_wrong_values(self):
        for id in ("S01", "S03", "P01", "P02", "P03", "P04", "W03", "L02"):
            case = Case(id, "test", {"cells": 3})
            with self.subTest(id=id):
                check_scalar(SimpleNamespace(Value2=((1.0,), (2.0,), (3.0,))), case)
                for values in ((-2146826259,) * 3, (1.0, -2146826259, 3.0),
                               (1.0, float("nan"), 3.0), (True, 2.0, 3.0)):
                    with self.assertRaises(AssertionError):
                        check_scalar(SimpleNamespace(Value2=tuple((v,) for v in values)), case)
        with self.assertRaisesRegex(AssertionError, "row 2: expected 2, got 99"):
            check_scalar(SimpleNamespace(Value2=((1.0,), (99,), (3.0,))),
                         Case("S01", "test", {"cells": 3}))

    def test_multi_argument_expected_sums(self):
        for argc in (2, 4, 8):
            case = Case("S02", str(argc), {"cells": 3, "argc": argc})
            check_scalar(SimpleNamespace(Value2=tuple(
                (float(sum(range(row, row + argc))),) for row in range(1, 4))), case)
            with self.assertRaises(AssertionError):
                check_scalar(SimpleNamespace(Value2=((1.0,), (2.0,), (3.0,))), case)

    def test_xll_load_failure_stops_before_benchmark(self):
        app = SimpleNamespace(RegisterXLL=Mock(return_value=False))
        with self.assertRaisesRegex(RuntimeError, "RegisterXLL returned False"):
            register_xll(app, Path("benchmark.xll"))
        app.RegisterXLL.assert_called_once_with(str(Path("benchmark.xll").resolve()))
        app.RegisterXLL.return_value = True
        register_xll(app, Path("benchmark.xll"))

    def test_registration_names_match_rust_generator(self):
        generated = source(10)
        self.assertIn('Name = "BENCH.EXTRA.0000"', generated)
        self.assertIn('Name = "BENCH.EXTRA.0009"', generated)
        self.assertNotIn('BENCH.EXTRA.0010', generated)
        with self.assertRaises(ValueError):
            source(5_001)

    def test_summary_requires_matching_conditions(self):
        common = {"id": "S01", "variant": "1000", "status": "ok", "profile": "full",
                  "params": {"cells": 1_000}, "threads": 1, "rtd_throttle_ms": 100,
                  "excel_version": "16.0", "excel_build": "1",
                  "artifact_source": "github-actions", "artifact_architecture": "x86_64",
                  "ci_commit": "abc123", "ci_run_id": "42", "runner_commit": "abc123"}
        left = common | {"implementation": "xlfn", "metrics": {"p50_s": 1.0}}
        right = common | {"implementation": "excel_dna", "metrics": {"p50_s": 2.0}}
        self.assertEqual(summarize([left, right])[0]["xlfn_advantage_ratio"], 2.0)
        right["excel_build"] = "2"
        self.assertFalse(summarize([left, right])[0]["comparable"])
        right["excel_build"] = "1"
        right["ci_run_id"] = "43"
        self.assertFalse(summarize([left, right])[0]["comparable"])

    def test_ci_artifact_manifest_requires_complete_matching_x64_set(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            image = bytearray(0x86)
            image[:2] = b"MZ"
            struct.pack_into("<I", image, 0x3C, 0x80)
            image[0x80:0x84] = b"PE\0\0"
            struct.pack_into("<H", image, 0x84, 0x8664)
            for relative in FILES:
                path = root / relative
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(image)
            create(root, "abc123", "42", "1")
            manifest = verify(root, run_id="42", commit="abc123")
            self.assertEqual(manifest["architecture"], "x86_64")
            self.assertEqual(manifest["excel_dna_compilation"], "NativeAOT")
            with self.assertRaises(ValueError):
                verify(root, run_id="43")
            (root / FILES[0]).write_bytes(b"changed")
            with self.assertRaises(ValueError):
                verify(root)


if __name__ == "__main__":
    unittest.main()

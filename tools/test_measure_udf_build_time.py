import json
from pathlib import Path
from tempfile import TemporaryDirectory
from types import SimpleNamespace
import unittest
from unittest.mock import patch

from measure_udf_build_time import measure, source_digest, timing_units


class BuildTimeMeasurementTests(unittest.TestCase):
    def setUp(self):
        self.temporary = TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)

    def write_report(self, units):
        directory = self.root / "cargo-timings"
        directory.mkdir(exist_ok=True)
        report = directory / "cargo-timing-20261007.html"
        report.write_text("const UNIT_DATA = " + json.dumps(units) + ";\n")
        return report

    def test_source_digest_ignores_build_artifacts_but_tracks_source_edits(self):
        for name in ["Cargo.toml", "Cargo.lock", "rust-toolchain.toml",
                     "crates/example/Cargo.toml", "crates/example/src/lib.rs"]:
            path = self.root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text("original")
        before = source_digest(self.root)
        for name in ["crates/example/target/debug/build/dependency/out/private.rs",
                     "crates/example/target/tests/trybuild/Cargo.toml"]:
            path = self.root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text("generated")
        self.assertEqual(source_digest(self.root), before)
        (self.root / "crates/example/src/lib.rs").write_text("changed")
        self.assertNotEqual(source_digest(self.root), before)

    def test_embedded_json_retains_strings_and_unknown_fields(self):
        units = [{"name": "build-script", "features": "a];b", "custom": {"value": 42}}]
        self.assertEqual(timing_units(self.write_report(units)), units)

    def test_missing_assignment_has_useful_error(self):
        report = self.root / "missing.html"
        report.write_text("<html></html>")
        with self.assertRaisesRegex(RuntimeError, "no UNIT_DATA"):
            timing_units(report)

    def test_non_list_data_has_useful_error(self):
        with self.assertRaisesRegex(RuntimeError, "not a list"):
            timing_units(self.write_report({"duration": 1}))

    def test_sample_records_every_unit_and_only_cleans_selected_packages(self):
        units = [dict(name=f"dependency-{n}", features=[], start=n / 10,
                      duration=n / 10, sections=[], target="build-script")
                 for n in range(25)]
        units += [dict(name=name, features=[], start=0, duration=0.5, sections=[])
                  for name in ["consumer", "xlfn", "syn", "serde_derive"]]
        self.write_report(units)
        args = SimpleNamespace(incremental="off", cache="dependencies",
                               rebuild="framework", profile="dev", features=[])
        with patch("measure_udf_build_time.run") as run:
            sample = measure(self.root, "consumer", self.root, "build", 0, "candidate", args)
        self.assertEqual(run.call_args_list[0].args[0],
                         ["cargo", "clean", "-p", "consumer", "-p", "xlfn", "-p", "xlfn-macros"])
        self.assertEqual(sample["all_units"], units)
        self.assertEqual(len(sample["slowest_units"]), 20)
        self.assertEqual(sample["slowest_units"][0]["name"], "dependency-24")
        self.assertEqual([unit["name"] for unit in sample["critical_units"]],
                         ["xlfn", "syn", "serde_derive"])
        self.assertTrue(sample["serde_derive_compiled"])
        self.assertEqual(sample["unit_seconds"], 0.5)


if __name__ == "__main__":
    unittest.main()

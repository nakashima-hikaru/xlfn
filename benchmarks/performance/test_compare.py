"""Check executable provenance and effective Criterion duration metadata."""

from contextlib import redirect_stdout
import hashlib
import io
import json
from pathlib import Path
import runpy
import tempfile
import unittest
from unittest.mock import patch


SCRIPT = Path(__file__).with_name("compare.py")


class CompareTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.binary = self.root / "binary.exe"
        self.binary.write_bytes(b"frozen benchmark")
        self.digest = hashlib.sha256(self.binary.read_bytes()).hexdigest()
        self.argv = [str(SCRIPT), "--work-dir", str(self.root / "measurements"),
                     "--output", str(self.root / "paired.json"),
                     "--baseline-revision", "baseline", "--filter", "case=^selected$",
                     "--benchmark-measurement", "case=10"]
        for kind in ("baseline", "candidate"):
            log = self.root / f"{kind}.jsonl"
            log.write_text(json.dumps({"executable": str(self.binary),
                                       "target": {"name": "case"}}) + "\n")
            provenance = self.root / f"{kind}-provenance.json"
            provenance.write_text(json.dumps({"executables": {"case": {
                "path": str(self.binary), "sha256": self.digest}}}))
            self.argv.extend([f"--{kind}-build", str(log),
                              f"--{kind}-provenance", str(provenance)])

    def invoke(self):
        with patch("sys.argv", self.argv), redirect_stdout(io.StringIO()):
            runpy.run_path(str(SCRIPT), run_name="__main__")

    def test_rejects_cargo_log_that_points_to_another_executable(self):
        another = self.root / "stale-binary"
        another.write_bytes(b"different build")
        (self.root / "candidate.jsonl").write_text(json.dumps({
            "executable": str(another), "target": {"name": "case"}}) + "\n")
        with patch("subprocess.run") as run:
            with self.assertRaisesRegex(ValueError, "Cargo executable.*provenance"):
                self.invoke()
            run.assert_not_called()

    def test_rejects_missing_executable_provenance(self):
        (self.root / "candidate-provenance.json").write_text("{}")
        with patch("subprocess.run") as run:
            with self.assertRaisesRegex(ValueError, "Cargo executable.*provenance"):
                self.invoke()
            run.assert_not_called()

    def test_records_frozen_hashes_and_effective_group_duration(self):
        def criterion(arguments, *, env, **_kwargs):
            self.assertEqual(Path(arguments[0]).suffix, ".exe")
            self.assertEqual(arguments[arguments.index("--measurement-time") + 1], "10.0")
            self.assertEqual(env["XLFN_BENCH_MEASUREMENT_MS"], "10000")
            result = Path(env["CARGO_TARGET_DIR"]) / "criterion" / "selected" / "new"
            result.mkdir(parents=True)
            (result / "benchmark.json").write_text(json.dumps({"full_id": "selected"}))
            (result / "estimates.json").write_text(json.dumps({
                "median": {"point_estimate": 42}, "mean": {
                    "point_estimate": 42, "confidence_interval": {
                        "lower_bound": 41, "upper_bound": 43}}}))

        with patch("subprocess.run", side_effect=criterion) as run:
            self.invoke()
            self.assertEqual(run.call_count, 4)
        result = json.loads((self.root / "paired.json").read_text())
        self.assertEqual(result["effective_measurement_seconds"], {"case": 10.0})
        self.assertEqual(set(result["executable_sha256"].values()), {self.digest})
        self.assertEqual(result["summary"][0]["change_percent"], 0)


if __name__ == "__main__":
    unittest.main()

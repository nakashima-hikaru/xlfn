"""Check paired builds retain caller flags while normalizing filesystem paths."""

import importlib.util
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch


SPEC = importlib.util.spec_from_file_location("toolchain_compare", Path(__file__).with_name("toolchain_compare.py"))
COMPARE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(COMPARE)


class BuildFlagTests(unittest.TestCase):
    def check_flags(self, environment, expected):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source, target, log = root / "source", root / "target", root / "build.jsonl"
            remaps = [f"--remap-path-prefix={source}=/xlfn/source",
                      f"--remap-path-prefix={target}=/xlfn/target"]
            with patch.dict(os.environ, environment, clear=True), patch("subprocess.run") as run:
                COMPARE.cargo_build(["cargo", "build"], source, target, log)
                flags = run.call_args.kwargs["env"]["CARGO_ENCODED_RUSTFLAGS"]
            self.assertEqual(flags.split("\x1f"), expected + remaps)
            command = json.loads(log.with_suffix(".command.json").read_text())
            self.assertEqual(command["effective_encoded_rustflags"], flags)

    def test_preserves_whitespace_delimited_caller_flags(self):
        self.check_flags({"RUSTFLAGS": "-C opt-level=2 -C target-cpu=native"},
                         ["-C", "opt-level=2", "-C", "target-cpu=native"])

    def test_preserves_encoded_flags_and_their_precedence(self):
        self.check_flags({"RUSTFLAGS": "ignored", "CARGO_ENCODED_RUSTFLAGS": '--cfg\x1flabel="two words"'},
                         ["--cfg", 'label="two words"'])

    def test_empty_encoded_flags_still_override_rustflags(self):
        self.check_flags({"RUSTFLAGS": "ignored", "CARGO_ENCODED_RUSTFLAGS": ""}, [])


if __name__ == "__main__":
    unittest.main()

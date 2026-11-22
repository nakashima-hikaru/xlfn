"""Verify the legacy feature inventory, deterministic balance and failure propagation."""

import io
import itertools
import tomllib
import unittest
from contextlib import redirect_stdout
from subprocess import CompletedProcess
from unittest.mock import patch

from xlfn_tools import feature_matrix as matrix


class FeatureMatrixTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        with (matrix.ROOT / "crates/xlfn/Cargo.toml").open("rb") as manifest:
            cls.features = tomllib.load(manifest)["features"]

    def test_effective_legacy_inventory(self):
        # Independently enumerate the transitive choices: async-builtin implies
        # async, bench-internals implies handles, and each other switch is free.
        async_cluster = ((), ("async",), ("async", "async-builtin"))
        for mode, count, expected_weight_range in (("check", 288, 2), ("test", 96, 2)):
            with self.subTest(mode=mode):
                independent = ["cache", "jiff", "rtd", "serde"]
                handles_cluster = [(), ("handles",)]
                if mode == "check":
                    independent.append("refinement")
                    handles_cluster.append(("bench-internals", "handles"))
                expected = {
                    tuple(sorted(async_features + handles_features + free_features))
                    for async_features in async_cluster
                    for handles_features in handles_cluster
                    for size in range(len(independent) + 1)
                    for free_features in itertools.combinations(independent, size)
                }
                cases = matrix.inventory(self.features, mode)
                self.assertEqual(len(cases), count)
                self.assertEqual({case.enabled for case in cases}, expected)
                shards = matrix.shards(cases, 8)
                self.assertEqual([len(group) for group in shards], [count // 8] * 8)
                self.assertCountEqual(itertools.chain.from_iterable(shards), cases)
                self.assertEqual(shards, matrix.shards(cases[::-1], 8))
                weights = [sum(case.weight for case in group) for group in shards]
                self.assertLessEqual(max(weights) - min(weights), expected_weight_range)
                self.assertIn(matrix.Case(("async-builtin",), ("async", "async-builtin")), cases)

    def test_historical_test_costs_balance_all_observed_configurations(self):
        observed = matrix.historical_test_seconds()
        cases = matrix.inventory(self.features, "test")
        self.assertEqual(set(observed), {case.enabled for case in cases})
        self.assertTrue(all(seconds > 0 for seconds in observed.values()))
        groups = matrix.shards(cases, 8, observed)
        self.assertEqual([len(group) for group in groups], [12] * 8)
        self.assertCountEqual(itertools.chain.from_iterable(groups), cases)
        self.assertEqual(groups, matrix.shards(cases[::-1], 8, observed))
        weights = [sum(matrix.scheduling_weight(c, observed) for c in group) for group in groups]
        self.assertLessEqual(max(weights) - min(weights), 10)

    def test_command_preserves_locked_library_and_serial_tests(self):
        case = matrix.Case(("async-builtin", "handles"), ("async", "async-builtin", "handles"))
        self.assertEqual(
            case.command("test"),
            [
                "cargo",
                "test",
                "--package",
                "xlfn",
                "--no-default-features",
                "--lib",
                "--locked",
                "--features",
                "async-builtin,handles",
                "--",
                "--test-threads=1",
            ],
        )
        self.assertEqual(
            matrix.Case((), ()).command("check"),
            ["cargo", "check", "--package", "xlfn", "--no-default-features", "--lib", "--locked"],
        )

    def test_command_failure_is_not_hidden_and_remaining_cases_run(self):
        cases = [matrix.Case((), ()), matrix.Case(("handles",), ("handles",))]
        log = io.StringIO()
        with (
            patch.object(
                matrix.subprocess,
                "run",
                side_effect=[CompletedProcess([], 1), CompletedProcess([], 0)],
            ) as run,
            redirect_stdout(log),
        ):
            self.assertEqual(matrix.run_cases(cases, "test"), 1)
            self.assertEqual(run.call_count, 2)
        self.assertIn("FAIL: 1 feature cases failed: (no features)", log.getvalue())

    def test_invalid_shard_or_excluded_transitive_feature(self):
        from argparse import ArgumentTypeError

        for value in ("0/8", "9/8", "x/8", "2"):
            with self.subTest(value=value), self.assertRaises(ArgumentTypeError):
                matrix.parse_shard(value)
        with self.assertRaises(ValueError):
            matrix.shards([matrix.Case((), ())], 2)
        with self.assertRaisesRegex(ValueError, "enable excluded features"):
            matrix.inventory({"a": ["refinement"], "refinement": []}, "test")

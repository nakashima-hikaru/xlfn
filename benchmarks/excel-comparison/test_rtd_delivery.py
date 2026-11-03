import math
import unittest
from types import SimpleNamespace
from unittest.mock import Mock, patch

from run import (ComObject, ComRetry, ExcelSession, UnsupportedCase, observe_rtd_pulse,
                 pulse, run_rtd_rate)
from workloads import Case


class Clock:
    def __init__(self):
        self.now = 0.0

    def sleep(self, duration):
        self.now += duration


class SampledCells:
    _oleobj_ = object()

    def __init__(self, clock, values):
        self.clock = clock
        self.values = values
        self.Worksheet = SimpleNamespace(Range=Mock(return_value=self))

    @property
    def Resize(self):
        # Dynamic dispatch gets this optional-argument property immediately.
        # Calling the returned Range invokes Item, selecting one cell.
        return self

    def __call__(self, row, column):
        if column != 1:
            raise IndexError(column)
        return SampledCells(self.clock, lambda at: [self.values(at)[row - 1]])

    @property
    def Value2(self):
        return tuple((value,) for value in self.values(self.clock.now))


class RtdDeliveryTest(unittest.TestCase):
    def observation(self, values, *, duration=10, emissions=27, throttle_ms=100):
        clock = Clock()
        sampled = SampledCells(clock, values)
        target = ComObject(sampled, ComRetry())
        session = SimpleNamespace(
            app=SimpleNamespace(RTD=SimpleNamespace(ThrottleInterval=throttle_ms),
                                Evaluate=Mock(side_effect=[0, emissions])),
            memory=Mock(return_value=100), stage=Mock())
        case = Case("R04", "1hz", {"cells": 30, "topics": 3, "duration_s": duration,
                                  "period_ms": 1000, "requested_hz": 1})
        return clock, target, session, case

    def run_observation(self, observation):
        clock, target, session, case = observation
        with patch("run.time.perf_counter", side_effect=lambda: clock.now), \
                patch("run.time.sleep", side_effect=clock.sleep):
            return run_rtd_rate(session, target, case, 0.1)

    def test_source_emissions_without_numeric_cell_delivery_fail(self):
        observation = self.observation(lambda _: [-2146826246] * 3)
        with self.assertRaisesRegex(AssertionError, "no observed numeric progress"):
            self.run_observation(observation)
        diagnostics = observation[2].stage.call_args.kwargs
        self.assertEqual(diagnostics["source_emissions"], 27)
        self.assertEqual(diagnostics["topics_with_observed_progress"], 0)
        self.assertEqual(diagnostics["delivery_observation"], "incomplete_progress")

    def test_one_frozen_numeric_value_is_not_an_observed_update(self):
        observation = self.observation(lambda _: [42] * 3)
        with self.assertRaisesRegex(AssertionError, "no observed numeric progress"):
            self.run_observation(observation)
        diagnostics = observation[2].stage.call_args.kwargs
        self.assertEqual(diagnostics["observed_distinct_values"], 1)
        self.assertEqual(diagnostics["observed_changes_by_topic"], [0, 0, 0])

    def test_initial_error_then_only_initial_zero_does_not_qualify_delivery(self):
        observation = self.observation(lambda at: [-2146826246 if at == 0 else 0] * 3)
        with self.assertRaisesRegex(AssertionError, "no observed numeric progress"):
            self.run_observation(observation)
        diagnostics = observation[2].stage.call_args.kwargs
        self.assertEqual(diagnostics["observed_last_values_by_topic"], [0, 0, 0])
        self.assertEqual(diagnostics["observed_changes_by_topic"], [0, 0, 0])

    def test_each_topic_needs_observed_progress(self):
        observation = self.observation(lambda at: [math.floor(at), math.floor(at), 0])
        with self.assertRaisesRegex(AssertionError, r"topics \[2\]"):
            self.run_observation(observation)
        observation[1].Worksheet.Range.assert_called_once_with("A1:A3")
        self.assertEqual(observation[2].stage.call_args.kwargs["topics_with_observed_progress"], 2)

    def test_regular_numeric_delivery_reports_transitions_without_counting_baseline(self):
        observation = self.observation(lambda at: [math.floor(at)] * 3)
        result = self.run_observation(observation)
        self.assertEqual(result["observed_changes_by_topic"], [9, 9, 9])
        self.assertEqual(result["observed_distinct_values"], 10)
        self.assertEqual(result["sampled_topics"], 3)
        self.assertEqual(result["delivery_observation"], "progress")
        self.assertAlmostEqual(result["observed_updates_per_s"], 9 / result["observation_s"])

    def test_two_distinct_values_over_ten_seconds_are_weak_progress(self):
        observation = self.observation(lambda at: [int(at >= 1)] * 3)
        result = self.run_observation(observation)
        self.assertEqual(result["observed_distinct_values"], 2)
        self.assertEqual(result["observed_changes_by_topic"], [1, 1, 1])
        self.assertEqual(result["delivery_observation"], "weak_progress")
        self.assertLess(result["observed_updates_per_s"], 0.11)
        self.assertGreater(min(result["last_change_age_s_by_topic"]), 8.9)

    def test_initial_error_followed_by_a_numeric_value_counts_as_delivery(self):
        observation = self.observation(
            lambda at: [-2146826246 if at < 1 else 1] * 3, duration=2, emissions=3)
        result = self.run_observation(observation)
        self.assertEqual(result["observed_changes_by_topic"], [1, 1, 1])
        self.assertEqual(result["observed_distinct_values"], 1)
        self.assertEqual(result["delivery_observation"], "progress")

    def test_coalesced_delivery_is_not_required_to_match_requested_hz(self):
        observation = self.observation(lambda at: [0 if at < 9 else 9] * 3)
        result = self.run_observation(observation)
        self.assertEqual(result["topics_with_observed_progress"], 3)
        self.assertEqual(result["requested_updates_per_topic_s"], 1)
        self.assertLess(result["observed_updates_per_s"], 0.11)

    def test_insufficient_window_without_progress_is_unsupported(self):
        observation = self.observation(lambda _: [0] * 3, duration=0.05, emissions=3)
        with self.assertRaisesRegex(UnsupportedCase, "source period plus throttle"):
            self.run_observation(observation)

    def test_boolean_and_nonfinite_values_are_not_numeric_delivery(self):
        for value in (True, float("nan"), float("inf")):
            with self.subTest(value=value):
                observation = self.observation(lambda _, value=value: [value] * 3)
                with self.assertRaisesRegex(AssertionError, "no observed numeric progress"):
                    self.run_observation(observation)

    def test_no_source_emissions_still_fail_even_if_cell_values_change(self):
        observation = self.observation(lambda at: [math.floor(at)] * 3, emissions=0)
        with self.assertRaisesRegex(AssertionError, "source emitted no periodic updates"):
            self.run_observation(observation)

    def test_sample_shape_must_cover_every_topic(self):
        observation = self.observation(lambda _: [0, 0])
        with self.assertRaisesRegex(AssertionError, "2 cells, expected 3"):
            self.run_observation(observation)


class RtdPulseTest(unittest.TestCase):
    def session(self):
        session = object.__new__(ExcelSession)
        session.app = SimpleNamespace(Run=Mock(return_value=1_000_000.0),
                                      Evaluate=Mock(side_effect=[11, 12]))
        session.memory = Mock(return_value=100)
        session.stage = Mock()
        return session

    def observe(self, session, values, count=4_000):
        clock = Clock()
        target = SampledCells(clock, values)
        with patch("run.time.perf_counter", side_effect=lambda: clock.now), \
                patch("run.time.sleep", side_effect=clock.sleep):
            return observe_rtd_pulse(session, target, count, 1_000_000, 0.03)

    def test_control_runs_once_without_a_formula_or_recalculation(self):
        session = self.session()
        pulse(session, 1_000_000)
        session.app.Run.assert_called_once_with("BENCH.RTD.PULSE", 1_000_000.0)
        session.app.Evaluate.assert_not_called()

    def test_rejected_control_reports_the_actual_value(self):
        session = self.session()
        session.app.Run.return_value = -2146826259
        with self.assertRaisesRegex(AssertionError, "#NAME"):
            pulse(session, 1_000_000)

    def test_success_waits_for_a_late_cell_outside_the_latency_sample(self):
        session = self.session()
        result = self.observe(session, lambda at:
            [1_000_000, 0 if at < 0.02 else 1_000_000] + [1_000_000] * 3998)
        self.assertEqual(result["validated_cells"], 4_000)
        self.assertEqual(result["sampled_cells"], 2_001)
        self.assertGreaterEqual(result["pulse_settle_s"], 0.02)

    def test_frozen_unsampled_cell_cannot_qualify_a_pulse(self):
        session = self.session()
        with self.assertRaisesRegex(TimeoutError, r"row 2: 0"):
            self.observe(session, lambda _: [1_000_000, 0] + [1_000_000] * 3998)
        diagnostics = session.stage.call_args.kwargs
        self.assertEqual(diagnostics["source_publish_returns"], 1)
        failure = session.stage.call_args_list[-2].kwargs
        self.assertIn("TimeoutError", failure["traceback"])
        self.assertIn("row 2: 0", failure["error"])

    def test_diagnostic_failure_preserves_the_original_timeout(self):
        session = self.session()
        session.app.Evaluate.side_effect = [0, RuntimeError("Excel unavailable")]
        with self.assertRaisesRegex(TimeoutError, "row 1: 0"):
            self.observe(session, lambda _: [0], count=1)
        self.assertIn("Excel unavailable", session.stage.call_args.kwargs["observation_error"])
        failure = session.stage.call_args_list[-2].kwargs
        self.assertIn("TimeoutError", failure["error"])


if __name__ == "__main__":
    unittest.main()

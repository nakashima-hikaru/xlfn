import copy
import json
from pathlib import Path
import tempfile
import unittest

from event_probe import assess, read_trace


def evidence():
    phases, events = [], []
    names = ["programmatic", "registered.normal", "registered.cancel",
             "removed1.normal", "removed1.cancel", "restored1.normal", "restored1.cancel"]
    for name in names:
        generation = 2 if name.startswith("restored") else 1
        before = dict(generation=generation, ended=0, canceled=0, started=0, active=0)
        after = dict(before, started=1)
        callbacks = []
        if name.startswith(("registered", "restored")):
            callbacks = (["callback.canceled"] if name.endswith("cancel") else []) + ["callback.ended"]
            after.update(ended=1, canceled=int(name.endswith("cancel")))
        trace = [{"event": e, "details": {"handler_generation": generation}} for e in callbacks]
        if name == "removed1.cancel":
            trace.append({"event": "task.return", "details": {"transport": 256, "invoked": True}})
        phases.append(dict(name=name, before=before, after=after, events=trace, cell_value=1))
    for operation in ("event.register", "event.remove", "event.register"):
        for selector in (1, 2):
            removal = operation.endswith("remove")
            events.append({"event": operation, "details": {"selector": selector,
                "procedure": None if removal else "handler", "procedure_xltype": 256 if removal else 2,
                "event_xltype": 2048, "transport": 0, "invoked": True, "cleanup": 0,
                "raw": {"xltype": 2048, "value": 0 if removal else -1657274367}}})
    return phases, events


class EventProbeTest(unittest.TestCase):
    def test_live_nil_removal_counterexample_is_not_inconclusive(self):
        record = json.loads((Path(__file__).parent / "event-qualification/probe-2026-10-25-nil.json").read_text())
        phases = [{k: v for k, v in p.items() if k not in ("event_start", "event_stop")} |
                  {"events": record["events"][p["event_start"]:p["event_stop"]]} for p in record["phases"]]
        result = assess(phases, record["events"], record["cycles"])
        self.assertEqual(result["verdict"], "callbacks_observed_after_removal")
        self.assertEqual(result["reasons"], [])
        self.assertEqual(len(result["stale_handler_phases"]), 6)
        self.assertFalse(result["production_shutdown_qualified"])

    def test_candidate_requires_exact_signed_registration_token(self):
        phases, events = evidence()
        for e in events:
            if e["event"] == "event.remove":
                e["details"] = dict(selector=e["details"]["selector"], method="unregister-id",
                                    api="xlfUnregister", registration_id=-1657274367,
                                    argument_xltype=1, argument_value=-1657274367.0,
                                    transport=0, invoked=True, cleanup=0, raw={"xltype": 4, "value": 1})
        self.assertEqual(assess(phases, events, 1, "unregister-id")["verdict"], "no_callbacks_observed_after_removal")
        events[2]["details"]["argument_value"] = 1
        self.assertEqual(assess(phases, events, 1, "unregister-id")["verdict"], "inconclusive")

    def test_zero_ack_does_not_override_observed_callback_absence(self):
        phases, events = evidence()
        result = assess(phases, events, 1)
        self.assertEqual(result["verdict"], "no_callbacks_observed_after_removal")
        self.assertFalse(result["production_shutdown_qualified"])

    def test_callbacks_after_zero_ack_are_retained_as_counterexample(self):
        phases, events = evidence()
        phases[3]["events"].append({"event": "callback.ended", "details": {"handler_generation": 1}})
        phases[3]["after"]["ended"] = 1
        self.assertEqual(assess(phases, events, 1)["verdict"], "callbacks_observed_after_removal")

    def test_missing_controls_cancellation_and_workload_cannot_pass(self):
        for failure in ("control", "restore", "cancel", "workload", "cleanup", "sequence", "trace"):
            with self.subTest(failure=failure):
                phases, events = evidence()
                if failure == "control": phases[1]["after"]["ended"] = 0
                if failure == "restore": phases.pop()
                if failure == "cancel": phases[4]["events"] = []
                if failure == "workload": phases[3]["after"]["started"] = 0
                if failure == "cleanup": events[2]["details"]["cleanup"] = 32
                if failure == "sequence": events[2], events[4] = events[4], events[2]
                if failure == "trace": phases[1]["events"] = []
                self.assertEqual(assess(phases, events, 1)["verdict"], "inconclusive")

    def test_stale_handlers_are_failure_evidence_not_missing_evidence(self):
        phases, events = evidence()
        phases[5]["events"][0]["details"]["handler_generation"] = 1
        result = assess(phases, events, 1)
        self.assertEqual(result["verdict"], "stale_callbacks_after_reregistration")
        self.assertEqual(result["stale_handler_phases"], ["restored1.normal"])

    def test_programmatic_events_do_not_hide_interactive_removal_failure(self):
        phases, events = evidence()
        for index in (0, 3):
            phases[index]["events"].append({"event": "callback.ended", "details": {"handler_generation": 1}})
            phases[index]["after"]["ended"] = 1
        phases[5]["events"][0]["details"]["handler_generation"] = 1
        result = assess(phases, events, 1)
        self.assertEqual(result["verdict"], "callbacks_observed_after_removal")
        self.assertEqual(result["observations"], ["programmatic control raised callbacks"])
        # Counter corruption still invalidates the evidence, even with callbacks.
        phases[3]["after"]["ended"] = 2
        self.assertEqual(assess(phases, events, 1)["verdict"], "inconclusive")

    def test_identity_mismatch_and_partial_trace_are_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "probe.jsonl"
            config = dict(path=str(path), pid=123, creation_ticks=456, token="0" * 32)
            event = {k: v for k, v in config.items() if k != "path"} | dict(schema=1, source="event-probe")
            path.write_text(json.dumps(event) + "\n")
            self.assertEqual(read_trace(config), [event])
            changed = copy.copy(event)
            changed["creation_ticks"] += 1
            path.write_text(json.dumps(changed) + "\n")
            with self.assertRaisesRegex(ValueError, "identity mismatch"):
                read_trace(config)
            path.write_text(json.dumps(event) + '\n{"partial":')
            with self.assertRaises(ValueError):
                read_trace(config)

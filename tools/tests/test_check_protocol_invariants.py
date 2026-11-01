import tempfile
import unittest
from collections import Counter
from pathlib import Path
from unittest.mock import patch

from xlfn_tools.check_protocol_invariants import (
    check,
    references,
    repository_references,
    test_only,
    test_source_paths,
)


def review(actual, decision="acquisition"):
    return {
        "id": "LIFE-TEST",
        "proposition": "A canonical shared state must be classified under its lock.",
        "established_by": "The locked acquisition operation.",
        "owner": "The original lifecycle coordinator and its access guard.",
        "decision": decision,
        "layer": "canonical acquisition",
        "reason": "Concurrent state changes require a fresh acquisition decision.",
        "tests": ["the acquisition regression"],
        "references": [
            {**reference.__dict__, "count": count} for reference, count in actual.items()
        ],
    }


class ProtocolInvariantAuditTests(unittest.TestCase):
    def test_lexes_comments_literals_and_macro_bodies(self):
        source = r"""
        // unreachable!(); state.phase();
        fn guarded<'a>(value: &'a str) {
            let text = r#".expect("not a check"); fail_stop();"#;
            let character = '\'';
            quote! { require_lifecycle_invariant(valid(), "canonical"); }
        }
        """
        actual = references("source.rs", source)
        self.assertEqual(sum(actual.values()), 1)
        self.assertEqual(next(iter(actual)).kind, "require_lifecycle_invariant")
        self.assertIn("fn guarded", next(iter(actual)).context)

    def test_observations_are_reviewed_not_forbidden(self):
        actual = references("source.rs", "fn diagnostic() { let phase = state.phase(); }")
        self.assertEqual(check(actual, [review(actual, "observation")]), [])
        self.assertTrue(check(actual, []))

    def test_new_guard_stale_entry_and_relocation_require_review(self):
        actual = references("source.rs", "fn acquire() { assert!(idle()); }")
        entries = [review(actual)]
        self.assertEqual(check(actual, entries), [])
        added = actual + references("source.rs", "fn acquire() { unreachable!(); }")
        self.assertTrue(any("unclassified" in error for error in check(added, entries)))
        self.assertTrue(any("stale" in error for error in check(Counter(), entries)))
        moved = references("source.rs", "fn publish() { assert!(idle()); }")
        self.assertEqual(len(check(moved, entries)), 2)

    def test_condition_change_is_not_the_same_reference(self):
        before = references("source.rs", "fn acquire() { if state.phase() == Closed { stop(); } }")
        after = references("source.rs", "fn acquire() { if state.phase() != Closed { stop(); } }")
        self.assertTrue(check(after, [review(before)]))

    def test_feature_gate_literals_remain_part_of_the_reviewed_context(self):
        before = references(
            "source.rs", '#[cfg(feature = "refinement")] fn trace() { state.phase(); }'
        )
        after = references(
            "source.rs", '#[cfg(feature = "production")] fn trace() { state.phase(); }'
        )
        self.assertTrue(check(after, [review(before)]))

    def test_distinct_protocol_expectations_are_distinct_sites(self):
        actual = references(
            "source.rs",
            'fn acquire() { owner.expect("owner live"); owner.expect("rollback armed"); }',
        )
        self.assertEqual(len(actual), 2)
        changed = references(
            "source.rs",
            'fn acquire() { owner.expect("different premise"); owner.expect("rollback armed"); }',
        )
        self.assertTrue(check(changed, [review(actual)]))

    def test_pure_tests_excluded_but_refinement_is_not(self):
        actual = references(
            "source.rs",
            """
            #[cfg(test)] mod tests { fn test_only() { unreachable!(); } }
            #[test] fn local() { panic!("test"); }
            #[cfg(any(test, feature = "refinement"))]
            fn production_refinement() { state.is_closed(); }
            """,
        )
        self.assertEqual(sum(actual.values()), 1)
        self.assertIn("production_refinement", next(iter(actual)).context)

    def test_functions_named_like_checks_are_not_calls(self):
        self.assertFalse(references("source.rs", "fn fail_stop() {} fn status() {}"))

    def test_aliasing_a_guard_does_not_hide_new_calls(self):
        actual = references("source.rs", "use crate::fail_stop as stop; fn acquire() { stop(); }")
        self.assertEqual(sum(actual.values()), 2)
        self.assertEqual({reference.kind for reference in actual}, {"fail_stop", "alias:fail_stop"})
        changed = references(
            "source.rs", "use crate::fail_stop as stop; fn acquire() { stop(); stop(); }"
        )
        self.assertTrue(check(changed, [review(actual)]))

    def test_nested_cfg_is_conservative_and_order_independent(self):
        self.assertTrue(test_only('#[cfg(all(feature = "handles", test))] fn only_test'))
        self.assertTrue(test_only('#[cfg(any(all(test, feature = "handles"), test))]'))
        self.assertFalse(test_only('#[cfg(any(test, feature = "refinement"))]'))
        self.assertFalse(test_only('#[cfg(all(any(test, feature = "refinement"), unix))]'))
        self.assertFalse(test_only("#[cfg(not(test))]"))

    def test_missing_or_duplicate_semantic_decisions_fail(self):
        actual = references("source.rs", "fn acquire() { state.is_idle(); }")
        entry = review(actual)
        del entry["owner"]
        self.assertTrue(check(actual, [entry]))
        self.assertTrue(check(actual, [review(actual, "magic")]))
        self.assertTrue(check(actual, [review(actual), review(actual)]))
        self.assertTrue(check(actual, [review(actual, "protocol_debt")]))

    def test_new_enrolled_child_file_is_not_silently_exempt(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            child = root / "src/phase/child.rs"
            child.parent.mkdir(parents=True)
            child.write_text("fn acquire() { state.status(); }")
            with patch(
                "xlfn_tools.check_protocol_invariants.source_paths",
                return_value=["src/phase/child.rs"],
            ):
                actual = repository_references(root, ["src/**/*.rs"])
            self.assertEqual(sum(actual.values()), 1)

    def test_file_names_do_not_exempt_production_modules(self):
        sources = {
            "src/lib.rs": "mod tests; mod test_support; mod test_child;",
            "src/tests.rs": "fn check() { unreachable!(); }",
            "src/test_support.rs": "fn check() { unreachable!(); }",
            "src/test_child.rs": "#[cfg(test)] mod tests;",
            "src/test_child/tests.rs": "mod nested;",
            "src/test_child/tests/nested.rs": "fn check() { unreachable!(); }",
        }
        self.assertEqual(
            test_source_paths(sources),
            {"src/test_child/tests.rs", "src/test_child/tests/nested.rs"},
        )
        sources["src/test_child.rs"] = "mod tests;"
        self.assertEqual(test_source_paths(sources), set())

    def test_ambiguous_module_path_is_never_silently_excluded(self):
        sources = {
            "src/lib.rs": '#[cfg(test)] #[path = "custom.rs"] mod custom;',
            "src/custom.rs": "fn check() { unreachable!(); }",
        }
        self.assertEqual(test_source_paths(sources), set())


if __name__ == "__main__":
    unittest.main()

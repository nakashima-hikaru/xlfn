#!/usr/bin/env python3
import tempfile
import unittest
from pathlib import Path

from check_verus_tcb import audit_verus_files


class TestCheckVerusTcb(unittest.TestCase):
    def test_clean_file(self):
        with tempfile.TemporaryDirectory() as tmpdir:
            path = Path(tmpdir) / "test.rs"
            path.write_text("proof fn foo() { assert(1 + 1 == 2); }")
            assumes, externals = audit_verus_files([Path(tmpdir)], set())
            self.assertEqual(len(assumes), 0)
            self.assertEqual(len(externals), 0)

    def test_detects_assume(self):
        with tempfile.TemporaryDirectory() as tmpdir:
            path = Path(tmpdir) / "test.rs"
            path.write_text("proof fn foo() { assume(false); }")
            assumes, externals = audit_verus_files([Path(tmpdir)], set())
            self.assertEqual(len(assumes), 1)

    def test_rejects_alternative_and_multiline_assumptions(self):
        examples = [
            "proof fn f() { assume\n/* boundary */ (false); }",
            "pub assume_specification[std::ptr::read](p: *const u8) -> u8;",
            "pub axiom\nfn fabricated() ensures false;",
        ]
        for source in examples:
            with self.subTest(source=source), tempfile.TemporaryDirectory() as tmpdir:
                path = Path(tmpdir) / "test.rs"
                path.write_text("// preface\n" + source)
                assumes, _ = audit_verus_files([Path(tmpdir)], set())
                self.assertEqual(len(assumes), 1)
                self.assertIn("test.rs:2:", assumes[0])

    def test_external_specs_require_approval(self):
        for attribute in ("external_type_specification", "external_fn_specification"):
            with self.subTest(attribute=attribute), tempfile.TemporaryDirectory() as tmpdir:
                path = Path(tmpdir) / "test.rs"
                path.write_text(f"#[verifier::{attribute}]\nstruct Bridge;")
                _, externals = audit_verus_files([Path(tmpdir)], set())
                self.assertEqual(len(externals), 1)
                _, approved = audit_verus_files([Path(tmpdir)], {"test.rs"})
                self.assertEqual(approved, [])

    def test_assumption_words_in_literals_are_not_code(self):
        with tempfile.TemporaryDirectory() as tmpdir:
            path = Path(tmpdir) / "test.rs"
            path.write_text('const TEXT: &str = r###"assume_specification axiom fn external_type_specification"###;\n'
                            '/* axiom fn fake(); /* assume(false) */ */\n'
                            'fn assume_specification_helper() {}')
            self.assertEqual(audit_verus_files([Path(tmpdir)], set()), ([], []))

    def test_ignores_assume_in_comments(self):
        with tempfile.TemporaryDirectory() as tmpdir:
            path = Path(tmpdir) / "test.rs"
            path.write_text("// assume(false);\n/* assume(true); */\nproof fn bar() {}")
            assumes, externals = audit_verus_files([Path(tmpdir)], set())
            self.assertEqual(len(assumes), 0)

    def test_detects_unapproved_external_body(self):
        with tempfile.TemporaryDirectory() as tmpdir:
            path = Path(tmpdir) / "test.rs"
            path.write_text("#[verifier::external_body]\nfn foo() {}")
            assumes, externals = audit_verus_files([Path(tmpdir)], set())
            self.assertEqual(len(externals), 1)

    def test_approved_external_body(self):
        with tempfile.TemporaryDirectory() as tmpdir:
            path = Path(tmpdir) / "test.rs"
            path.write_text("#[verifier::external_body]\nfn foo() {}")
            assumes, externals = audit_verus_files([Path(tmpdir)], {"test.rs"})
            self.assertEqual(len(externals), 0)


if __name__ == "__main__":
    unittest.main()

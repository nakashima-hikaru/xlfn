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

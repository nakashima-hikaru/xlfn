#!/usr/bin/env python3
from contextlib import redirect_stderr, redirect_stdout
import io
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

import check_verus_tcb

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
                with patch.object(check_verus_tcb, "ROOT", Path(tmpdir)):
                    _, approved = audit_verus_files([Path(tmpdir)], {"test.rs:1"})
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
            with patch.object(check_verus_tcb, "ROOT", Path(tmpdir)):
                assumes, externals = audit_verus_files([Path(tmpdir)], {"test.rs:1"})
                self.assertEqual(len(externals), 0)

    def audit_source(self, source):
        with tempfile.TemporaryDirectory() as tmpdir:
            path = Path(tmpdir) / "test.rs"
            path.write_text(source)
            return audit_verus_files([Path(tmpdir)], set())

    def test_direct_qualified_and_aliased_primitives(self):
        cases = [
            "proof fn f() { admit(); }",
            "proof fn f() { ::vstd::pervasive::admit\n(); }",
            "proof fn f() { verus_builtin::assume(false); }",
            "use vstd::pervasive::assume as trusted; proof fn f() { trusted(false); }",
            "use vstd::{pervasive::{admit as trusted}}; proof fn f() { trusted(); }",
            "use vstd::pervasive as pv; use pv::assume as trusted;",
            "use vstd as vs; use vs::pervasive::admit as trusted;",
            "use verus_builtin::{assume as r#trusted};",
            "use vstd::pervasive as p; let trusted = p::assume; trusted(false);",
            "let trusted = ::vstd::pervasive::admit; trusted();",
            "use verus_builtin as vb; let trusted = vb::assume; trusted(false);",
            "#[verifier::axiom] proof fn fabricated() ensures false;",
            "#[verifier::assume_termination] fn f() {}",
            "#![verifier::assume(externals_available_without_declaration)]",
            "assume_specification<T>[foreign::<T>]() ensures false;",
            "pub axiom\nfn fabricated() ensures false;",
        ]
        for source in cases:
            with self.subTest(source=source):
                assumptions, _ = self.audit_source(source)
                self.assertTrue(assumptions)

    def test_extern_crate_aliases_do_not_hide_primitives(self):
        cases = [
            "extern crate vstd as renamed;\nproof fn fake() ensures false { renamed::pervasive::assume(false); }",
            "extern crate vstd as renamed;\nproof fn fake() ensures false { ::renamed::pervasive::admit(); }",
            "extern crate builtin as renamed;\nproof fn fake() ensures false { renamed::assume(false); }",
            "extern crate verus_builtin as renamed;\nproof fn fake() ensures false { renamed::assume(false); }",
            "pub extern\ncrate vstd\nas r#renamed;\nproof fn fake() ensures false { r#renamed::pervasive::assume(false); }",
            "extern crate vstd as renamed;\nuse renamed::pervasive::assume as trusted;",
            "use renamed::pervasive as p;\nextern crate vstd as renamed;\nproof fn fake() ensures false { p::admit(); }",
            "extern crate vstd as renamed;\nuse renamed as again;\nlet trusted = again::pervasive::assume;",
        ]
        for source in cases:
            with self.subTest(source=source):
                assumptions, _ = self.audit_source(source)
                self.assertTrue(assumptions)

    def test_extern_crate_aliases_allow_ordinary_references_and_ignore_literals(self):
        source = r'''
            extern crate vstd as renamed;
            extern crate unrelated as ordinary;
            use renamed::prelude::*;
            fn f() {
                ordinary::check(false);
                ordinary::accept();
                let _ = renamed::math::add(1, 2);
            }
            // extern crate vstd as disguised;
            const TEXT: &str = "extern crate vstd as disguised;";
            disguised::check(false);
        '''
        self.assertEqual(self.audit_source(source), ([], []))

    def test_module_qualified_aliases_do_not_hide_primitives(self):
        cases = [
            "extern crate vstd as renamed; verus! { proof fn fake() ensures false { crate::renamed::pervasive::assume(false); } }",
            "extern crate vstd as renamed; verus! { proof fn fake() ensures false { self::renamed::pervasive::admit(); } }",
            "extern crate builtin as renamed; proof fn fake() ensures false { crate::renamed::assume(false); }",
            "extern crate verus_builtin as renamed; let trusted = self::renamed::assume;",
            "use vstd as renamed; use crate::renamed::pervasive::assume as trusted;",
            "use vstd as renamed; use self::{renamed::pervasive as p}; proof fn fake() ensures false { p::admit(); }",
            "mod bridge { pub extern crate vstd as renamed; } proof fn fake() ensures false { crate::bridge::renamed::pervasive::assume(false); }",
            "mod bridge { pub use vstd as renamed; } use self::bridge as b; let trusted = b::renamed::pervasive::assume;",
            "mod bridge { use vstd as renamed; mod inner { proof fn fake() ensures false { super::renamed::pervasive::assume(false); } } }",
            "extern crate vstd as renamed; mod outer { mod inner { let trusted = super::super::renamed::pervasive::assume; } }",
            "extern crate vstd as renamed; mod inner { use crate::renamed::{pervasive::{assume as trusted}}; }",
            "extern crate vstd; let trusted = crate::vstd::pervasive::assume;",
            "extern crate vstd as renamed; mod inner { proof fn fake() ensures false { renamed::pervasive::assume(false); } }",
            "extern crate vstd as renamed; mod inner { use unrelated as renamed; proof fn fake() ensures false { ::renamed::pervasive::assume(false); } }",
            "extern crate vstd as renamed; mod inner { use unrelated as renamed; use ::renamed::pervasive::assume as trusted; }",
            "extern crate vstd as renamed; use self as here; let trusted = here::renamed::pervasive::assume;",
            "mod bridge { use vstd as renamed; use self as here; let trusted = here::renamed::pervasive::assume; }",
            "extern crate vstd as renamed; mod inner { use super as parent; let trusted = parent::renamed::pervasive::assume; }",
            "mod bridge { pub use vstd as renamed; } use crate::bridge::{self as b}; let trusted = b::renamed::pervasive::assume;",
            "extern crate vstd as renamed; use crate as root; let trusted = root::root::renamed::pervasive::assume;",
        ]
        for source in cases:
            with self.subTest(source=source):
                assumptions, _ = self.audit_source(source)
                self.assertTrue(assumptions)

    def test_qualified_primitive_names_are_reserved_even_in_unrelated_modules(self):
        source = r'''
            mod first { use vstd as same; }
            mod second {
                use unrelated as same;
                fn f() { self::same::assume(false); same::admit(); }
            }
            extern crate unrelated as ordinary;
            fn f() {
                crate::ordinary::assume(false);
                self::ordinary::admit();
                crate::second::same::assume(false);
            }
            mod inner { fn f() { super::ordinary::assume(false); } }
        '''
        assumptions, externals = self.audit_source(source)
        self.assertEqual(len(assumptions), 6)
        self.assertEqual(externals, [])

    def test_module_qualified_call_keeps_its_diagnostic_location(self):
        source = "extern crate vstd as renamed;\nproof fn fake() ensures false { crate::renamed::pervasive::assume(false); }"
        assumptions, _ = self.audit_source(source)
        self.assertEqual(len(assumptions), 1)
        self.assertIn("test.rs:2:", assumptions[0])

    def test_cross_file_reexports_cannot_hide_trust_primitives(self):
        cases = [
            ("pub extern crate vstd as renamed;",
             "crate::bridge::renamed::pervasive::assume(false);"),
            ("pub use vstd as renamed;",
             "crate::bridge::renamed::pervasive::admit();"),
            ("pub use vstd::pervasive as api;",
             "use crate::bridge::api::{assume as trusted}; trusted(false);"),
            ("pub use verus_builtin as api;",
             "let trusted = crate::bridge::api::assume;"),
            ("pub use verus_builtin as api;",
             "crate::bridge::api::assume_specification[foreign]() ensures false;"),
        ]
        for bridge, expression in cases:
            with self.subTest(expression=expression), tempfile.TemporaryDirectory() as tmpdir:
                root = Path(tmpdir)
                (root / "bridge.rs").write_text(bridge)
                (root / "main.rs").write_text(
                    "use vstd::prelude::*;\nmod bridge;\nverus! {\n"
                    f"proof fn false_claim() ensures false {{ {expression} }}\n}}\n"
                )
                assumptions, externals = audit_verus_files([root], set())
                self.assertTrue(assumptions)
                self.assertTrue(any("main.rs:4:" in item for item in assumptions))
                self.assertEqual(externals, [])

    def test_cli_fails_for_cross_file_assumption_and_accepts_clean_reexport(self):
        with tempfile.TemporaryDirectory() as tmpdir:
            root = Path(tmpdir)
            sources = root / "verification" / "verus"
            sources.mkdir(parents=True)
            (sources / "bridge.rs").write_text("pub extern crate vstd as renamed;\n")
            for expression, status in [
                ("crate::bridge::renamed::pervasive::assume(false);", 1),
                ("assert(true);", 0),
            ]:
                with self.subTest(expression=expression):
                    (sources / "main.rs").write_text(
                        "use vstd::prelude::*;\nmod bridge;\n"
                        f"verus! {{ proof fn example() {{ {expression} }} }}\n"
                    )
                    output, errors = io.StringIO(), io.StringIO()
                    with (
                        patch.object(check_verus_tcb, "ROOT", root),
                        patch("sys.argv", ["check_verus_tcb.py", "--allowlist", str(root / "allowlist.json")]),
                        redirect_stdout(output),
                        redirect_stderr(errors),
                    ):
                        self.assertEqual(check_verus_tcb.main(), status)
                    if status:
                        self.assertIn("verification/verus/main.rs:3:", errors.getvalue())
                        self.assertNotIn("PASS: 0 project-local", output.getvalue())
                    else:
                        self.assertEqual(errors.getvalue(), "")
                        self.assertIn("PASS: 0 project-local", output.getvalue())

    def test_conditional_aliases_preserve_every_trust_target(self):
        for declaration in ("use {root} as renamed;", "extern crate {root} as renamed;"):
            for roots in (("vstd", "unrelated"), ("unrelated", "vstd")):
                source = (
                    '#[cfg(feature = "first")] ' + declaration.format(root=roots[0]) + '\n'
                    '#[cfg(not(feature = "first"))] ' + declaration.format(root=roots[1]) + '\n'
                    'proof fn fake() ensures false { crate::renamed::pervasive::assume(false); }'
                )
                with self.subTest(declaration=declaration, roots=roots):
                    assumptions, _ = self.audit_source(source)
                    self.assertTrue(assumptions)

    def test_alias_cycle_does_not_hide_a_conditional_trust_target(self):
        source = r'''
            #[cfg(feature = "first")] use crate::b as a;
            #[cfg(not(feature = "first"))] use vstd as a;
            use crate::a as b;
            proof fn fake() ensures false { crate::b::pervasive::assume(false); }
        '''
        assumptions, _ = self.audit_source(source)
        self.assertTrue(assumptions)

    def test_external_attributes_and_cfg_attr(self):
        for source in [
            "#[verifier::external] struct S;",
            "#[cfg_attr(verus_only, verifier::external_body)] fn f() {}",
            "#[verifier\n::\nexternal_fn_specification] fn f() {}",
            "#[verifier(external_body)] fn f() {}",
            "#[verifier::external_trait_specification] trait T {}",
            "#[verifier::external_trait_extension(T via TImpl)] trait T {}",
            "#[verifier::external_trait_private_bound(crate::sealed::Sealed)] trait T {}",
            "#[verifier::external_derive] struct S;",
            "#[verus::internal(external_body)] fn f() {}",
            "#[verus::internal(external_fn_specification)] fn f() {}",
            "#[verus::internal(external_trait_blanket)] impl T for S {}",
            "#[verus::trusted] fn f() {}",
        ]:
            with self.subTest(source=source):
                _, external = self.audit_source(source)
                self.assertEqual(len(external), 1)

    def test_unrelated_identifiers_and_literals_are_not_boundaries(self):
        source = r'''
            // admit(); #[verifier::external]
            /* assume(false); /* admit() */ */
            const A: &str = "admit(); #[verifier::external_body]";
            const B: &str = r###"use vstd::pervasive::assume as trusted;"###;
            fn external_body() {}
            fn assume() {}
            fn admit() {}
            fn normal() {
                unrelated::check(false);
                unrelated::accept();
                value.assume(false);
                let external_fn_specification = 1;
                let external = verifier::external;
            }
            use unrelated::check as ordinary;
        '''
        self.assertEqual(self.audit_source(source), ([], []))

    def test_bare_primitive_calls_are_reserved_even_with_local_shadowing(self):
        # The audit is deliberately conservative about unqualified proof
        # primitive names; qualified references are reserved as well.
        for source in ("fn assume(_: bool) {} fn f() { assume(true); }",
                       "fn admit() {} fn f() { admit(); }"):
            with self.subTest(source=source):
                assumptions, _ = self.audit_source(source)
                self.assertEqual(len(assumptions), 1)

    def test_allowlist_is_exact_repository_path_and_line(self):
        with tempfile.TemporaryDirectory() as tmpdir:
            root = Path(tmpdir)
            for name in ("first", "second"):
                (root / name).mkdir()
                (root / name / "same.rs").write_text("#[verifier::external]\nfn f() {}")
            with patch.object(check_verus_tcb, "ROOT", root):
                _, external = audit_verus_files([root], {"same.rs"})
                self.assertEqual(len(external), 2)
                _, external = audit_verus_files([root], {"first/same.rs:1"})
                self.assertEqual(len(external), 1)
                self.assertIn("second/same.rs:1", external[0])
                _, external = audit_verus_files([root], {"first/same.rs:2"})
                self.assertEqual(len(external), 2)


if __name__ == "__main__":
    unittest.main()

import unittest
from collections import Counter

from check_panic_boundaries import check, references


class PanicBoundaryGuardTests(unittest.TestCase):
    def test_comments_literals_and_lifetimes(self):
        source = r'''
            // catch_unwind
            /* outer /* catch_unwind */ catch_unwind */
            fn active<'call>(x: &'call str) {
                let _ = "catch_unwind \\\"";
                let _ = r###"catch_unwind /* */"###;
                let _ = br#"catch_unwind"#;
                let _ = b'\'';
                let _ = '\u{7b}';
                std::panic::catch_unwind(|| ());
            }
        '''
        found = references("source.rs", source)
        self.assertEqual(len(found), 1)
        self.assertEqual(found[0].context, "fn active")

    def test_aliases_and_macro_token_streams_are_checked(self):
        found = references("source.rs", '''
            use std::panic::catch_unwind as alias;
            fn generate() { quote! { catch_unwind(|| generated()); } }
        ''')
        self.assertEqual(len(found), 2)

    def test_context_retains_test_scope_and_import_attributes(self):
        found = references("source.rs", '''
            #[cfg(test)] use std::panic::{catch_unwind};
            #[cfg(test)] mod tests {
                #[test] fn contract() { catch_unwind(|| ()); }
            }
        ''')
        self.assertIn("cfg", found[0].context)
        self.assertIn("test", found[0].context)
        self.assertIn("mod tests", found[1].context)
        self.assertIn("fn contract", found[1].context)

    def test_added_removed_and_moved_references_fail(self):
        original = Counter(references("source.rs", "#[test] fn original() { catch_unwind(|| ()); }"))
        entry = {**next(iter(original)).__dict__, "count": 1, "category": "test", "reason": "known string panic"}
        self.assertEqual(check(original, [entry]), [])
        self.assertTrue(check(original + original, [entry]))
        self.assertTrue(check(Counter(), [entry]))
        moved = Counter(references("source.rs", "fn production() { catch_unwind(|| ()); }"))
        self.assertTrue(check(moved, [entry]))

    def test_unterminated_input_fails_closed(self):
        for source in ['/* catch_unwind', 'r#"catch_unwind', '"catch_unwind', 'fn x() {']:
            with self.subTest(source=source), self.assertRaises(ValueError):
                references("source.rs", source)


if __name__ == "__main__":
    unittest.main()

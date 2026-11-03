from pathlib import Path
from tempfile import TemporaryDirectory
import unittest
from unittest.mock import patch

import check_examples


class ExampleCheckTests(unittest.TestCase):
    def setUp(self):
        directory = TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name).resolve()
        self.guide = self.root / "guide"
        self.chapter = self.guide / "src/example.md"
        self.chapter.parent.mkdir(parents=True)
        (self.guide / "fixtures").mkdir()
        self.enterContext(patch.object(check_examples, "ROOT", self.root))
        self.enterContext(patch.object(check_examples, "GUIDE", self.guide))

    def prepare(self, text):
        self.chapter.write_text(text, encoding="utf-8")
        return check_examples.prepare_chapter(self.chapter)

    def test_hidden_setup_keeps_the_visible_code_under_test(self):
        (self.guide / "fixtures/setup.md").write_text("# let x = 42;\n", encoding="utf-8")
        text, projects = self.prepare(
            "```rust\n{{#include ../fixtures/setup.md}}\nassert_eq!(x, 42);\n```"
        )
        self.assertEqual(text, "```rust\n# let x = 42;\nassert_eq!(x, 42);\n```")
        self.assertEqual(projects, set())

    def test_source_include_uses_its_standalone_project(self):
        project = self.root / "examples/source"
        (project / "src").mkdir(parents=True)
        (project / "Cargo.toml").touch()
        (project / "src/lib.rs").write_text("fn example() {}", encoding="utf-8")
        text, projects = self.prepare(
            "```rust\n{{#include ../../examples/source/src/lib.rs}}\n```"
        )
        self.assertEqual(projects, {project / "Cargo.toml"})
        self.assertIn("Compiled in examples/source/Cargo.toml", text)
        self.assertNotIn("```rust", text)

    def test_unchecked_examples_and_unsupported_includes_fail(self):
        for text in (
            "```rust,ignore\ninvalid code\n```",
            "```rust,ignore-windows\ninvalid code\n```",
            "```rust\n{{#include ../fixtures/setup.md:anchor}}\n```",
            "```rust\n{{#include ../../Cargo.toml}}\nlet x = 1;\n```",
        ):
            with self.subTest(text=text), self.assertRaises(ValueError):
                self.prepare(text)

    def test_no_run_still_reaches_rustdoc(self):
        source = "```rust,no_run\nwait_for_excel();\n```"
        self.assertEqual(self.prepare(source), (source, set()))


if __name__ == "__main__":
    unittest.main()

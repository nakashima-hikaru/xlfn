"""Focused regressions for rendered mdBook navigation."""

from pathlib import Path
import tempfile
import unittest

from check import html_check


class RenderedLinkTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.source = self.root / "guide" / "src"
        self.book = self.root / "guide" / "book"
        self.source.mkdir(parents=True)
        self.book.mkdir(parents=True)
        self.chapter = self.source / "chapter.md"
        self.chapter.write_text("# Chapter\n", encoding="utf-8")
        (self.book / "index.html").write_text("<h1 id='home'>Home</h1>", encoding="utf-8")
        (self.book / "toc.html").write_text(
            "<a href='chapter.html'>Chapter</a>", encoding="utf-8"
        )

    def check(self, body):
        (self.book / "chapter.html").write_text(body, encoding="utf-8")
        return html_check(self.root / "guide", self.book, [self.chapter], {})

    def test_existing_chapter_fragment_and_asset_pass(self):
        (self.book / "site.css").write_text("", encoding="utf-8")
        self.assertEqual(
            self.check("<h1 id='section'>Section</h1><a href='#section'>Jump</a>"
                       "<link href='site.css' rel='stylesheet'>"),
            [],
        )

    def test_broken_fragment_is_reported(self):
        errors = self.check("<h1 id='section'>Section</h1><a href='#missing'>Jump</a>")
        self.assertTrue(any("Missing HTML fragment" in error for error in errors), errors)

    def test_missing_asset_and_escaped_path_are_reported(self):
        errors = self.check("<img src='missing.svg'><a href='../../outside.html'>Out</a>")
        self.assertTrue(any("Broken local URL" in error for error in errors), errors)
        self.assertTrue(any("escapes the HTML output" in error for error in errors), errors)


if __name__ == "__main__":
    unittest.main()

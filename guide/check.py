#!/usr/bin/env python3
"""Validate guide sources and, with --html, the generated mdBook site."""

from __future__ import annotations

import argparse
from html.parser import HTMLParser
from pathlib import Path
import re
import sys
import tomllib
from urllib.parse import unquote, urlsplit

GUIDE = Path(__file__).resolve().parent
LINK = re.compile(r"!?\[[^\]]*\]\(<?([^\s)>]+)>?(?:\s+[^)]*)?\)")
REFERENCE = re.compile(r"^\s{0,3}\[[^\]]+\]:\s*<?([^\s>]+)", re.MULTILINE)
SUMMARY_LINK = re.compile(r"^\s*(?:[-*]\s+)?\[[^\]]+\]\(([^)]+)\)", re.MULTILINE)
INCLUDE = re.compile(r"\{\{#include\s+([^}:]+)(?::[^}]*)?\}\}")
FENCE = re.compile(r"^ {0,3}(`{3,}|~{3,})(.*)$")
CSS_URL = re.compile(r"url\(\s*['\"]?([^'\")]+)['\"]?\s*\)", re.IGNORECASE)
GENERATED_PAGES = {"index.html", "print.html", "404.html", "toc.html"}


def without_code(text: str) -> tuple[str, bool]:
    """Mask fenced examples before treating Markdown as navigation."""
    fence = ""
    result = []
    for line in text.splitlines():
        match = FENCE.match(line)
        if fence:
            if match and match[1][0] == fence[0] and len(match[1]) >= len(fence) and not match[2].strip():
                fence = ""
            result.append("")
        elif match:
            fence = match[1]
            result.append("")
        else:
            result.append(line)
    return "\n".join(result), not fence


def local_url(target: str):
    url = urlsplit(target)
    return None if url.scheme or url.netloc else url


def source_check(guide: Path) -> tuple[list[str], list[Path], dict]:
    errors: list[str] = []
    src = guide / "src"
    try:
        config = tomllib.loads((guide / "book.toml").read_text(encoding="utf-8"))
    except (OSError, tomllib.TOMLDecodeError) as error:
        return [f"Cannot read book.toml: {error}"], [], {}
    if config.get("book", {}).get("src") != "src":
        errors.append('book.toml must use src = "src"')
    if config.get("build", {}).get("build-dir") != "book":
        errors.append('book.toml must use build-dir = "book"')
    html = config.get("output", {}).get("html", {})
    for setting in ("additional-css", "additional-js"):
        for asset in html.get(setting, []):
            if not (guide / asset).is_file():
                errors.append(f"Configured {setting} file does not exist: {asset}")

    summary = src / "SUMMARY.md"
    if not summary.is_file():
        return [*errors, "SUMMARY.md does not exist"], [], config
    chapters: list[Path] = []
    seen: set[Path] = set()
    for target in SUMMARY_LINK.findall(summary.read_text(encoding="utf-8")):
        url = local_url(target)
        if url is None:
            errors.append(f"SUMMARY target must be local: {target}")
            continue
        path = (src / unquote(url.path)).resolve()
        if not path.is_relative_to(src.resolve()):
            errors.append(f"SUMMARY target escapes the source directory: {target}")
            continue
        chapters.append(path)
        if path in seen:
            errors.append(f"Duplicate SUMMARY target: {target}")
        seen.add(path)
        if not path.is_file():
            errors.append(f"SUMMARY target does not exist: {target}")
    markdown_files = sorted(src.rglob("*.md"))
    for path in markdown_files:
        if path != summary and path.resolve() not in seen:
            errors.append(f"Markdown file is not listed in SUMMARY.md: {path.relative_to(src)}")
    maintainer_docs = [*guide.parent.glob("*.md"), *(guide.parent / "docs").rglob("*.md")]
    for path in [guide / "README.md", *sorted(maintainer_docs), *markdown_files]:
        if not path.is_file():
            errors.append(f"Markdown file does not exist: {path}")
            continue
        text = path.read_text(encoding="utf-8")
        prose, balanced = without_code(text)
        if not balanced:
            errors.append(f"Unbalanced fenced code block: {path}")
        if path in markdown_files and path != summary:
            if sum(line.startswith("# ") for line in prose.splitlines()) != 1:
                errors.append(f"Chapter must contain exactly one H1: {path.relative_to(src)}")
        for number, line in enumerate(text.splitlines(), 1):
            if line != line.rstrip():
                errors.append(f"Trailing whitespace: {path}:{number}")
        for target in [*LINK.findall(prose), *REFERENCE.findall(prose)]:
            url = local_url(target)
            if url is not None and url.path and not (path.parent / unquote(url.path)).exists():
                errors.append(f"Broken local link in {path}: {target}")
        # Includes inside code fences are expanded by mdBook as well.
        for target in INCLUDE.findall(text):
            if not (path.parent / target).is_file():
                errors.append(f"Broken mdBook include in {path}: {target}")
    return errors, chapters, config


class HtmlPage(HTMLParser):
    def __init__(self, text: str):
        super().__init__(convert_charrefs=True)
        self.ids: set[str] = set()
        self.duplicates: set[str] = set()
        self.links: list[str] = []
        self.feed(text)
        self.close()

    def handle_starttag(self, tag: str, attrs: list[tuple[str, str | None]]):
        for name, value in attrs:
            if value is None:
                continue
            if name == "id" or (tag == "a" and name == "name"):
                if value in self.ids:
                    self.duplicates.add(value)
                self.ids.add(value)
            if name in {"href", "src", "poster"} or (tag == "object" and name == "data"):
                self.links.append(value)

    handle_startendtag = handle_starttag


def html_check(guide: Path, book: Path, chapters: list[Path], config: dict) -> list[str]:
    errors: list[str] = []
    book = book.resolve()
    if not book.is_dir():
        return [f"HTML output directory does not exist: {book}; run mdbook build guide first"]
    pages = {
        path.resolve(): HtmlPage(path.read_text(encoding="utf-8"))
        for path in sorted(book.rglob("*.html"))
    }
    expected = {
        Path(path.resolve().relative_to((guide / "src").resolve())).with_suffix(".html")
        for path in chapters
    }
    redirects = {Path(name) for name in config.get("output", {}).get("html", {}).get("redirect", {})}
    for path in sorted(expected):
        if book / path not in pages:
            errors.append(f"Missing rendered chapter: {path}")
    for path, page in pages.items():
        relative = path.relative_to(book)
        if relative not in expected and relative.as_posix() not in GENERATED_PAGES and relative not in redirects:
            errors.append(f"HTML page is not listed in SUMMARY.md: {relative}")
        # mdBook's print view concatenates chapters with repeated heading IDs.
        if relative.as_posix() != "print.html":
            for name in sorted(page.duplicates):
                errors.append(f"Duplicate HTML id in {relative}: {name}")

    site_url = config.get("output", {}).get("html", {}).get("site-url", "/")
    site_path = urlsplit(site_url).path.rstrip("/") + "/"

    def resolve_link(source: Path, target: str, check_fragment: bool = True) -> Path | None:
        url = local_url(target)
        if url is None:
            return None
        decoded = unquote(url.path)
        if decoded.startswith("/"):
            if not decoded.startswith(site_path):
                errors.append(f"Local URL is outside site-url in {source.relative_to(book)}: {target}")
                return None
            resolved = (book / decoded[len(site_path):]).resolve()
        else:
            resolved = (source.parent / decoded).resolve() if decoded else source
        if not resolved.is_relative_to(book):
            errors.append(f"Local URL escapes the HTML output in {source.relative_to(book)}: {target}")
            return None
        if resolved.is_dir():
            resolved /= "index.html"
        if not resolved.is_file():
            errors.append(f"Broken local URL in {source.relative_to(book)}: {target}")
            return None
        if check_fragment and url.fragment and resolved in pages:
            fragment = unquote(url.fragment)
            # Browser text fragments do not name a document ID.
            fragment = fragment.split(":~:text=", 1)[0]
            if fragment and fragment not in pages[resolved].ids:
                errors.append(f"Missing HTML fragment in {source.relative_to(book)}: {target}")
        return resolved

    toc_targets: set[Path] = set()
    for path, page in pages.items():
        for target in page.links:
            resolved = resolve_link(path, target)
            if path == book / "toc.html" and resolved:
                toc_targets.add(resolved)
    for path in sorted(book.rglob("*.css")):
        for target in CSS_URL.findall(path.read_text(encoding="utf-8")):
            if not target.startswith("#"):
                resolve_link(path, target.strip(), check_fragment=False)
    if book / "toc.html" in pages:
        for path in sorted(expected):
            if book / path not in toc_targets:
                errors.append(f"Rendered chapter is absent from the sidebar: {path}")
    return errors


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--html", nargs="?", const=str(GUIDE / "book"), type=Path,
                        help="also validate generated HTML (default: guide/book)")
    args = parser.parse_args(argv)
    errors, chapters, config = source_check(GUIDE)
    if args.html is not None and not errors:
        errors.extend(html_check(GUIDE, args.html, chapters, config))
    if errors:
        print("Guide validation failed:", file=sys.stderr)
        for error in errors:
            print(f"  - {error}", file=sys.stderr)
        return 1
    suffix = " and generated HTML" if args.html is not None else ""
    print(f"Guide validation passed: {len(chapters)} chapters{suffix}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

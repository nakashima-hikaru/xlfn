#!/usr/bin/env python3
"""Validate the mdBook source without requiring mdBook."""

from __future__ import annotations

import re
import sys
from pathlib import Path

GUIDE = Path(__file__).resolve().parent
SRC = GUIDE / "src"
SUMMARY = SRC / "SUMMARY.md"
LINK = re.compile(r"(?<!!)\[[^\]]*\]\(([^)]+)\)")
SUMMARY_LINK = re.compile(r"^\s*(?:[-*]\s+)?\[[^\]]+\]\(([^)]+)\)", re.MULTILINE)

errors: list[str] = []

def parse_simple_toml(text: str) -> dict:
    config: dict = {"book": {}, "build": {}, "output": {"html": {}}}
    current_section = None
    for line in text.splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        if line.startswith("[") and line.endswith("]"):
            current_section = line[1:-1]
        elif "=" in line:
            k, v = [x.strip() for x in line.split("=", 1)]
            v = v.strip('"\'')
            if current_section == "book" and k == "src":
                config["book"]["src"] = v
            elif current_section == "build" and k == "build-dir":
                config["build"]["build-dir"] = v
    return config

try:
    import tomllib
    book_config = tomllib.loads((GUIDE / "book.toml").read_text(encoding="utf-8"))
except Exception:
    book_config = parse_simple_toml((GUIDE / "book.toml").read_text(encoding="utf-8"))

if book_config.get("book", {}).get("src") != "src":
    errors.append("book.toml must use src = \"src\"")
if book_config.get("build", {}).get("build-dir") != "book":
    errors.append("book.toml must use build-dir = \"book\"")
for css in book_config.get("output", {}).get("html", {}).get("additional-css", []):
    if not (GUIDE / css).is_file():
        errors.append(f"Configured CSS file does not exist: {css}")

summary_text = SUMMARY.read_text(encoding="utf-8")
summary_targets = []
summary_seen: set[Path] = set()

for target in SUMMARY_LINK.findall(summary_text):
    target = target.split("#", 1)[0]
    path = (SRC / target).resolve()
    try:
        path.relative_to(SRC.resolve())
    except ValueError:
        errors.append(f"SUMMARY target escapes the source directory: {target}")
        continue
    summary_targets.append(path)
    if path in summary_seen:
        errors.append(f"Duplicate SUMMARY target: {target}")
    summary_seen.add(path)
    if not path.is_file():
        errors.append(f"SUMMARY target does not exist: {target}")

markdown_files = sorted(SRC.rglob("*.md"))
listed = {path for path in summary_targets}
for path in markdown_files:
    if path == SUMMARY:
        continue
    if path.resolve() not in listed:
        errors.append(f"Markdown file is not listed in SUMMARY.md: {path.relative_to(SRC)}")

for path in [GUIDE / "README.md", *markdown_files]:
    text = path.read_text(encoding="utf-8")
    lines = text.splitlines()
    fence_count = sum(line.startswith("```") for line in lines)
    if fence_count % 2 != 0:
        errors.append(f"Unbalanced fenced code block: {path}")
    if path.parent == SRC and path != SUMMARY:
        h1_count = sum(line.startswith("# ") for line in lines)
        if h1_count != 1:
            errors.append(f"Chapter must contain exactly one H1: {path.relative_to(SRC)}")
    for line_number, line in enumerate(lines, 1):
        if line != line.rstrip():
            errors.append(f"Trailing whitespace: {path}:{line_number}")
    for target in LINK.findall(text):
        if target.startswith(("http://", "https://", "mailto:")) or target.startswith("#"):
            continue
        target_path = target.split("#", 1)[0]
        if not target_path:
            continue
        resolved = (path.parent / target_path).resolve()
        if not resolved.exists():
            errors.append(f"Broken local link in {path.relative_to(GUIDE)}: {target}")

if errors:
    print("Guide validation failed:", file=sys.stderr)
    for error in errors:
        print(f"  - {error}", file=sys.stderr)
    raise SystemExit(1)

print(f"Guide validation passed: {len(markdown_files) - 1} chapters")

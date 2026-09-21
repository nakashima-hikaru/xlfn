#!/usr/bin/env python3
"""Audit Verus formal verification codebase for TCB boundary violations.

Enforces:
1. Zero `assume(...)` directives in verification code.
2. Only approved `external_body` declarations listed in `verus_external_allowlist.json`.
"""

from __future__ import annotations

import argparse
import json
from pathlib import Path
import re
import sys

ROOT = Path(__file__).resolve().parents[1]
ALLOWLIST_PATH = ROOT / "tools/verus_external_allowlist.json"

RAW_STRING = re.compile(r'(?:br|cr|r)(#*)"')
CHARACTER = re.compile(r"(?:b)?'(?:[^'\\\n]|\\(?:u\{[0-9a-fA-F_]+\}|x[0-9a-fA-F]{2}|.))'")
ASSUME_PATTERN = re.compile(r"\bassume\s*\(")
EXTERNAL_BODY_PATTERN = re.compile(r"\bexternal_body\b")


def strip_rust_comments_and_strings(source: str) -> str:
    """Blank comments and literals while preserving line numbers and offsets."""
    output = list(source)
    size = len(source)
    index = 0

    def blank(start: int, end: int) -> None:
        for position in range(start, end):
            if source[position] not in "\r\n":
                output[position] = " "

    while index < size:
        if source.startswith("//", index):
            end = source.find("\n", index)
            end = size if end == -1 else end
            blank(index, end)
            index = end
        elif source.startswith("/*", index):
            start = index
            index += 2
            depth = 1
            while index < size and depth:
                if source.startswith("/*", index):
                    depth += 1
                    index += 2
                elif source.startswith("*/", index):
                    depth -= 1
                    index += 2
                else:
                    index += 1
            blank(start, index)
        else:
            raw = RAW_STRING.match(source, index)
            if raw:
                hashes = len(raw.group(1))
                end_marker = '"' + "#" * hashes
                end = source.find(end_marker, raw.end())
                if end == -1:
                    raise ValueError("unterminated raw string")
                blank(index, end + len(end_marker))
                index = end + len(end_marker)
            elif source[index] == '"':
                start = index
                index += 1
                while index < size:
                    char = source[index]
                    if char == "\\":
                        index += 2
                    elif char == '"':
                        index += 1
                        break
                    else:
                        index += 1
                blank(start, index)
            else:
                char_match = CHARACTER.match(source, index)
                if char_match:
                    blank(index, char_match.end())
                    index = char_match.end()
                else:
                    index += 1

    return "".join(output)


def audit_verus_files(search_paths: list[Path], allowlist: set[str]) -> tuple[list[str], list[str]]:
    assumes: list[str] = []
    unapproved_externals: list[str] = []

    for base_path in search_paths:
        if not base_path.exists():
            continue
        for file_path in base_path.rglob("*.rs"):
            content = file_path.read_text(encoding="utf-8")
            sanitized = strip_rust_comments_and_strings(content)

            try:
                display_path = file_path.relative_to(ROOT)
            except ValueError:
                display_path = file_path

            lines = sanitized.splitlines()
            for line_no, line in enumerate(lines, start=1):
                if ASSUME_PATTERN.search(line):
                    assumes.append(f"{display_path}:{line_no}: {line.strip()}")
                if EXTERNAL_BODY_PATTERN.search(line):
                    loc_id = f"{display_path}:{line_no}"
                    if loc_id not in allowlist and file_path.name not in allowlist:
                        unapproved_externals.append(f"{loc_id}: {line.strip()}")

    return assumes, unapproved_externals


def main() -> int:
    parser = argparse.ArgumentParser(description="Audit Verus verification TCB compliance.")
    parser.add_argument("--allowlist", type=Path, default=ALLOWLIST_PATH)
    args = parser.parse_args()

    allowlist_items: set[str] = set()
    if args.allowlist.exists():
        with open(args.allowlist, encoding="utf-8") as f:
            data = json.load(f)
            allowlist_items = set(data.get("external_bodies", []))

    search_dirs = [
        ROOT / "verification/verus",
        ROOT / "crates/xlfn-kernel",
        ROOT / "crates/xlfn/src",
    ]

    assumes, unapproved_externals = audit_verus_files(search_dirs, allowlist_items)

    failed = False
    if assumes:
        print("ERROR: Verus verification contains 'assume' directives (must be 0):", file=sys.stderr)
        for entry in assumes:
            print(f"  {entry}", file=sys.stderr)
        failed = True
    else:
        print("PASS: 0 'assume' directives found.")

    if unapproved_externals:
        print("ERROR: Unapproved 'external_body' found in Verus code:", file=sys.stderr)
        for entry in unapproved_externals:
            print(f"  {entry}", file=sys.stderr)
        failed = True
    else:
        print("PASS: All external_body declarations are approved or 0 found.")

    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())

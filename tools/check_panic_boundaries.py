#!/usr/bin/env python3
"""Reject unreviewed direct Rust catch_unwind references, including aliases.

The inventory is deliberately occurrence-specific: no source directory or test
file is exempt. Comments and literals are lexed out; macro token streams remain
visible. This is an introduction guard and review inventory, not a Rust proof.
"""

from __future__ import annotations

import argparse
from collections import Counter
from dataclasses import dataclass
import json
from pathlib import Path
import re
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]
INVENTORY = ROOT / "tools/panic_boundary_allowlist.json"
CATEGORIES = {"implementation", "resume", "test", "experiment"}
TOKEN = re.compile(r"[A-Za-z_][A-Za-z_0-9]*|[^\s]")
RAW_STRING = re.compile(r'(?:br|cr|r)(#*)"')
CHARACTER = re.compile(r"(?:b)?'(?:[^'\\\n]|\\(?:u\{[0-9a-fA-F_]+\}|x[0-9a-fA-F]{2}|.))'")


def rust_code(source: str) -> str:
    """Blank comments/string/character literals while preserving line offsets."""
    output = list(source)
    size = len(source)
    index = 0

    def blank(start: int, end: int) -> None:
        for position in range(start, end):
            if source[position] not in "\r\n":
                output[position] = " "

    while index < size:
        start = index
        if source.startswith("//", index):
            end = source.find("\n", index)
            index = size if end == -1 else end
        elif source.startswith("/*", index):
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
            if depth:
                raise ValueError("unterminated block comment")
        else:
            raw = RAW_STRING.match(source, index)
            if raw:
                closing = '"' + raw.group(1)
                end = source.find(closing, raw.end())
                if end == -1:
                    raise ValueError("unterminated raw string")
                index = end + len(closing)
            elif source[index] == '"' or source[index:index + 2] in {'b"', 'c"'}:
                index += 1 if source[index] == '"' else 2
                while index < size:
                    if source[index] == "\\":
                        index += 2
                    elif source[index] == '"':
                        index += 1
                        break
                    else:
                        index += 1
                else:
                    raise ValueError("unterminated string")
            else:
                character = CHARACTER.match(source, index)
                if character:
                    index = character.end()
                else:
                    index += 1
                    continue
        blank(start, min(index, size))
    return "".join(output)


def attributes(tokens: list[str]) -> str:
    text = " ".join(tokens)
    # Attribute contents are retained in the context so removing #[cfg(test)]
    # or moving an occurrence out of a test module invalidates the allowance.
    return " ".join(re.findall(r"#\s*!?\s*\[[^\]]*\]", text))


@dataclass(frozen=True, order=True)
class Reference:
    path: str
    context: str
    code: str


def references(path: str, source: str) -> list[Reference]:
    code = rust_code(source)
    lines = code.splitlines()
    frames: list[str] = []
    pending: list[str] = []
    found: list[Reference] = []
    for match in TOKEN.finditer(code):
        token = match.group()
        if token == "catch_unwind":
            line = code.count("\n", 0, match.start())
            context = [frame for frame in frames if frame]
            attrs = attributes(pending)
            if attrs:
                context.append(attrs)
            found.append(Reference(path, " / ".join(context) or "module root", " ".join(lines[line].split())))
        if token == "{":
            declaration = " ".join(pending)
            named = re.search(r"\b(fn|mod)\s+([A-Za-z_][A-Za-z_0-9]*)", declaration)
            attrs = attributes(pending)
            label = " ".join(part for part in [attrs, named.group() if named else ""] if part)
            frames.append(label)
            pending = []
        elif token == "}":
            if not frames:
                raise ValueError(f"unbalanced closing brace in {path}")
            frames.pop()
            pending = []
        elif token == ";":
            pending = []
        else:
            pending.append(token)
    if frames:
        raise ValueError(f"unbalanced opening brace in {path}")
    return found


def repository_references(root: Path) -> Counter[Reference]:
    # Include new source files as well as tracked ones, excluding build output
    # according to the repository's normal ignore policy.
    listing = subprocess.run(
        ["git", "ls-files", "--cached", "--others", "--exclude-standard", "-z", "--", "*.rs"],
        cwd=root, check=True, capture_output=True,
    ).stdout.decode().split("\0")
    found: Counter[Reference] = Counter()
    for path in sorted(set(filter(None, listing))):
        source = root / path
        if source.is_file():
            content = source.read_text(encoding="utf-8")
            if "catch_unwind" in content:
                found.update(references(path, content))
    return found


def check(actual: Counter[Reference], entries: list[dict]) -> list[str]:
    expected: Counter[Reference] = Counter()
    errors = []
    for entry in entries:
        reference = Reference(entry["path"], entry["context"], entry["code"])
        if reference in expected:
            errors.append(f"duplicate allowance: {reference}")
        if entry.get("category") not in CATEGORIES or not entry.get("reason", "").strip():
            errors.append(f"allowance needs an audited category and reason: {reference}")
        count = entry.get("count")
        if not isinstance(count, int) or count < 1:
            errors.append(f"allowance needs a positive count: {reference}")
            continue
        expected[reference] = count
    for reference, count in sorted((actual - expected).items()):
        errors.append(f"unreviewed catch_unwind ({count}): {reference.path}: {reference.context}: {reference.code}")
    for reference, count in sorted((expected - actual).items()):
        errors.append(f"stale allowance ({count}): {reference.path}: {reference.context}: {reference.code}")
    return errors


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--list", action="store_true", help="print references for manual audit; never rewrites the allowlist")
    args = parser.parse_args()
    actual = repository_references(ROOT)
    if args.list:
        print(json.dumps([{**reference.__dict__, "count": count} for reference, count in sorted(actual.items())], indent=2))
        return 0
    entries = json.loads(INVENTORY.read_text(encoding="utf-8"))
    errors = check(actual, entries)
    if errors:
        print("\n".join(errors), file=sys.stderr)
        print("Use catch_no_unwind/contain_panic for consumed payloads; review any intentional resume/test exception.", file=sys.stderr)
        return 1
    print(f"Panic boundaries: {sum(actual.values())} reviewed direct references; no unreviewed catch_unwind.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

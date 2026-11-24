#!/usr/bin/env python3
"""Audit Verus formal verification codebase for TCB boundary violations.

Enforces:
1. Zero project-local assume/admit calls, their imports, specifications or axioms.
2. Only approved Verus external attributes, identified by exact path and line.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from dataclasses import dataclass
from pathlib import Path

from ._repository import ROOT
from ._rust_lexer import blank_rust_comments_and_literals

ALLOWLIST_PATH = ROOT / "tools/verus_external_allowlist.json"

TOKEN = re.compile(r"r#[A-Za-z_][A-Za-z_0-9]*|[A-Za-z_][A-Za-z_0-9]*|::|[^\s]")
ASSUMPTIONS = {"assume", "admit", "assume_specification"}
# Audited against rust_verify/src/attributes.rs at the pinned d17fbd0 commit.
# Trait adapters and ignored private bounds are trust boundaries too, even
# when a particular annotation does not by itself assert a new axiom.
EXTERNALS = {
    "external",
    "external_body",
    "external_type_specification",
    "external_fn_specification",
    "external_trait_specification",
    "external_trait_extension",
    "external_trait_private_bound",
    "external_trait_blanket",
    "external_derive",
    "trusted",
}
ASSUMPTION_ATTRIBUTES = {"axiom", "assume", "assume_termination"}


@dataclass(frozen=True)
class Token:
    value: str
    offset: int


def use_tree(tokens: list[Token], prefix: tuple[str, ...] = ()):
    """Flatten Rust use trees, including groups, self imports and renaming."""
    index = 0
    while index < len(tokens):
        path = list(prefix)
        start = tokens[index].offset
        while index < len(tokens) and tokens[index].value not in {"{", ",", "as"}:
            value = tokens[index].value
            if value != "::" or not path:
                path.append(value)
            index += 1
        if index < len(tokens) and tokens[index].value == "{":
            index += 1
            begin = index
            depth = 1
            while index < len(tokens) and depth:
                depth += (tokens[index].value == "{") - (tokens[index].value == "}")
                index += 1
            if depth:
                raise ValueError("unclosed use tree")
            yield from use_tree(tokens[begin : index - 1], tuple(path))
        else:
            alias = None
            if index < len(tokens) and tokens[index].value == "as":
                index += 1
                if index >= len(tokens):
                    raise ValueError("missing import alias")
                alias = tokens[index].value
                index += 1
            if len(path) > 1 and path[-1] == "self":
                path.pop()
            if path:
                yield tuple(path), alias or path[-1], start
        if index < len(tokens) and tokens[index].value == ",":
            index += 1


def trust_boundaries(sanitized: str) -> tuple[set[int], set[int]]:
    """Inspect reserved trust-primitive syntax without guessing name resolution.

    Qualified references and imports named assume/admit/assume_specification
    are forbidden regardless of their namespace, including unrelated functions.
    This makes cross-file reexports, crate renaming and cfg branches irrelevant
    to detection. Bare calls and specification declarations are reserved too.
    Method calls, ordinary declarations, comments and literals remain allowed.
    This source inventory does not replace Rust/Verus semantic verification.
    """
    tokens = [Token(m.group().removeprefix("r#"), m.start()) for m in TOKEN.finditer(sanitized)]
    assumptions: set[int] = set()
    externals: set[int] = set()
    for index, token in enumerate(tokens):
        if token.value == "use" and index + 1 < len(tokens) and tokens[index + 1].value != "<":
            end = index + 1
            while end < len(tokens) and tokens[end].value != ";":
                end += 1
            if end == len(tokens):
                raise ValueError("unterminated use declaration")
            # Flatten groups to see the original name before `as` renaming,
            # even when the exporting module is defined in another file.
            for path, _, offset in use_tree(tokens[index + 1 : end]):
                if path[-1] in ASSUMPTIONS:
                    assumptions.add(offset)

    attribute_depth = 0
    for index, token in enumerate(tokens):
        value = token.value
        previous = tokens[index - 1].value if index else None
        following = tokens[index + 1].value if index + 1 < len(tokens) else None
        if value == "[" and (attribute_depth or previous in {"#", "!"}):
            attribute_depth += 1
        elif value == "]" and attribute_depth:
            attribute_depth -= 1
        if value == "axiom" and following == "fn":
            assumptions.add(token.offset)
        # Only verifier attributes are trust boundaries, not variables or
        # unrelated libraries' attributes with these same names. cfg_attr is
        # covered because the verifier path is inspected inside its token tree.
        if (
            attribute_depth
            and value in {"verifier", "verus"}
            and following in {"::", "("}
            and index + 2 < len(tokens)
        ):
            attribute_index = index + 2
            # The verifier accepts macro-generated internal forms directly in
            # source too; handwritten internal(external_body) cannot bypass us.
            if (
                value == "verus"
                and tokens[attribute_index].value == "internal"
                and attribute_index + 2 < len(tokens)
                and tokens[attribute_index + 1].value == "("
            ):
                attribute_index += 2
            # Inspect every top-level token inside `#[verifier(...)]` or
            # `#[verus::internal(...)]` (such as `#[verifier(inline, external_body)]`),
            # not just the first attribute name after `(`.
            if tokens[attribute_index - 1].value == "(":
                paren_depth = 1
                scan = attribute_index
                while scan < len(tokens) and paren_depth > 0:
                    scan_value = tokens[scan].value
                    if scan_value == "(":
                        paren_depth += 1
                    elif scan_value == ")":
                        paren_depth -= 1
                        if paren_depth == 0:
                            break
                    elif paren_depth == 1:
                        if scan_value in EXTERNALS:
                            externals.add(tokens[scan].offset)
                        elif scan_value in ASSUMPTION_ATTRIBUTES:
                            assumptions.add(tokens[scan].offset)
                    scan += 1
            else:
                attribute = tokens[attribute_index]
                if attribute.value in EXTERNALS:
                    externals.add(token.offset)
                elif attribute.value in ASSUMPTION_ATTRIBUTES:
                    assumptions.add(token.offset)
        if value not in ASSUMPTIONS or previous in {".", "fn"}:
            continue
        start = index
        while (
            start >= 2
            and tokens[start - 1].value == "::"
            and tokens[start - 2].value.isidentifier()
        ):
            start -= 2
        path = tuple(t.value for t in tokens[start : index + 1 : 2])
        if start and tokens[start - 1].value == "::":
            path = ("::",) + path
        # Qualified primitive references are forbidden even when used as a
        # function value rather than called here. Otherwise a local variable
        # could rename a primitive after importing only its parent module.
        if len(path) > 1:
            assumptions.add(tokens[start].offset)
        elif len(path) == 1 and (
            (value == "assume_specification" and following in {"[", "<"})
            or (
                value != "assume_specification"
                and (
                    following == "("
                    or (previous == "(" and following == ")")
                    or previous == "="
                    or (previous in {"(", ",", "["} and following in {",", ")", "]"})
                )
            )
        ):
            assumptions.add(tokens[start].offset)
    return assumptions, externals


def audit_verus_files(
    search_paths: list[Path],
    allowlist: set[str],
    matched_allowlist: set[str] | None = None,
) -> tuple[list[str], list[str]]:
    assumes: list[str] = []
    unapproved_externals: list[str] = []

    for base_path in search_paths:
        if not base_path.exists():
            continue
        for file_path in base_path.rglob("*.rs"):
            content = file_path.read_text(encoding="utf-8")
            sanitized = blank_rust_comments_and_literals(content)

            try:
                display_path = file_path.relative_to(ROOT)
            except ValueError:
                display_path = file_path

            lines = sanitized.splitlines()
            assumption_offsets, external_offsets = trust_boundaries(sanitized)
            for offset in sorted(assumption_offsets):
                line_no = sanitized.count("\n", 0, offset) + 1
                assumes.append(f"{display_path}:{line_no}: {lines[line_no - 1].strip()}")
            for offset in sorted(external_offsets):
                line_no = sanitized.count("\n", 0, offset) + 1
                loc_id = f"{display_path}:{line_no}"
                # Only repository-relative file:line entries authorize a TCB
                # boundary. A basename must never authorize another directory.
                if file_path.is_relative_to(ROOT) and loc_id in allowlist:
                    if matched_allowlist is not None:
                        matched_allowlist.add(loc_id)
                    continue
                else:
                    unapproved_externals.append(f"{loc_id}: {lines[line_no - 1].strip()}")

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
        ROOT / "crates/xlfn-services",
        ROOT / "crates/xlfn-engine",
        ROOT / "crates/xlfn/src",
    ]

    matched_allowlist: set[str] = set()
    assumes, unapproved_externals = audit_verus_files(
        search_dirs,
        allowlist_items,
        matched_allowlist=matched_allowlist,
    )
    stale_allowlist = sorted(allowlist_items - matched_allowlist)

    failed = False
    if assumes:
        print(
            "ERROR: Verus verification contains local assumptions, admits or axioms (must be 0):",
            file=sys.stderr,
        )
        for entry in assumes:
            print(f"  {entry}", file=sys.stderr)
        failed = True
    else:
        print("PASS: 0 project-local assumptions, admits or axioms found.")

    if unapproved_externals:
        print(
            "ERROR: Unapproved external body/type/function specification found in Verus code:",
            file=sys.stderr,
        )
        for entry in unapproved_externals:
            print(f"  {entry}", file=sys.stderr)
        failed = True
    else:
        print("PASS: All external body/type/function specifications are approved or 0 found.")

    if stale_allowlist:
        print(
            "ERROR: Allowlist contains stale external entries with no matching annotation:",
            file=sys.stderr,
        )
        for entry in stale_allowlist:
            print(f"  {entry}", file=sys.stderr)
        failed = True

    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())

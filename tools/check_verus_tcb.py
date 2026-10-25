#!/usr/bin/env python3
"""Audit Verus formal verification codebase for TCB boundary violations.

Enforces:
1. Zero project-local assume/admit calls, their imports, specifications or axioms.
2. Only approved Verus external attributes, identified by exact path and line.
"""

from __future__ import annotations

import argparse
from dataclasses import dataclass
import json
from pathlib import Path
import re
import sys

ROOT = Path(__file__).resolve().parents[1]
ALLOWLIST_PATH = ROOT / "tools/verus_external_allowlist.json"

RAW_STRING = re.compile(r'(?:br|cr|r)(#*)"')
CHARACTER = re.compile(r"(?:b)?'(?:[^'\\\n]|\\(?:u\{[0-9a-fA-F_]+\}|x[0-9a-fA-F]{2}|.))'")
TOKEN = re.compile(r"r#[A-Za-z_][A-Za-z_0-9]*|[A-Za-z_][A-Za-z_0-9]*|::|[^\s]")
ASSUMPTIONS = {"assume", "admit", "assume_specification"}
# Audited against rust_verify/src/attributes.rs at the pinned 671956ec commit.
# Trait adapters and ignored private bounds are trust boundaries too, even
# when a particular annotation does not by itself assert a new axiom.
EXTERNALS = {
    "external", "external_body", "external_type_specification", "external_fn_specification",
    "external_trait_specification", "external_trait_extension", "external_trait_private_bound",
    "external_trait_blanket", "external_derive", "trusted",
}
ASSUMPTION_ATTRIBUTES = {"axiom", "assume", "assume_termination"}
VERUS_ROOTS = {"vstd", "builtin", "verus_builtin"}


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
            yield from use_tree(tokens[begin:index - 1], tuple(path))
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


def module_paths(tokens: list[Token]) -> list[tuple[str, ...]]:
    """Track inline module namespaces; ordinary blocks/macros do not add one."""
    paths = []
    module: tuple[str, ...] = ()
    blocks = []
    for index, token in enumerate(tokens):
        paths.append(module)
        if token.value == "{":
            blocks.append(module)
            if (index >= 2 and tokens[index - 2].value == "mod"
                    and tokens[index - 1].value.isidentifier()):
                module += (tokens[index - 1].value,)
        elif token.value == "}" and blocks:
            module = blocks.pop()
    return paths


def trust_boundaries(sanitized: str) -> tuple[set[int], set[int]]:
    """Inspect token structure, rather than matching words in Rust identifiers.

    This is a conservative source audit, not a replacement for name resolution.
    Explicit trust-primitive imports are forbidden at their declaration, so
    renaming, re-exporting, function-value indirection and cross-file aliases
    cannot hide their use. Prelude globs remain supported; bare trust calls are
    always inspected. Bare assume/admit calls are reserved by this audit even
    when a local function shadows their name: accepting them would require full
    Rust/Verus name resolution. Qualified unrelated functions and methods are
    not trust primitives. Other vstd globs do not hide direct primitive names.
    """
    tokens = [Token(m.group().removeprefix("r#"), m.start()) for m in TOKEN.finditer(sanitized)]
    modules = module_paths(tokens)
    imports = []
    # The sentinel distinguishes a project module from an external crate with
    # a similar path. Import bindings are keyed by their full module location.
    local_root = ("<crate>",)
    external_aliases: dict[tuple[str, ...], set[tuple[str, ...]]] = {}
    external_offsets: set[int] = set()
    assumptions: set[int] = set()
    externals: set[int] = set()
    for index, token in enumerate(tokens):
        module = modules[index]
        # Rust permits crate renaming independently of use trees. Feed these
        # aliases into the same resolution pass so later use aliases and
        # qualified function-value references cannot hide a trust primitive.
        if (token.value == "extern" and index + 3 < len(tokens)
                and tokens[index + 1].value == "crate"):
            root = tokens[index + 2]
            alias = root
            end = index + 3
            if tokens[end].value == "as" and end + 2 < len(tokens):
                alias = tokens[end + 1]
                end += 2
            if (tokens[end].value == ";" and root.value.isidentifier()
                    and alias.value.isidentifier() and alias.value != "_"):
                imports.append((module, (root.value,), alias.value, root.offset))
                external_offsets.add(root.offset)
                external_aliases.setdefault(local_root + module + (alias.value,), set()).add((root.value,))
        if token.value == "use" and index + 1 < len(tokens) and tokens[index + 1].value != "<":
            end = index + 1
            while end < len(tokens) and tokens[end].value != ";":
                end += 1
            if end == len(tokens):
                raise ValueError("unterminated use declaration")
            imports.extend((module, path, alias, offset)
                           for path, alias, offset in use_tree(tokens[index + 1:end]))

    bindings = {local_root + module + (alias,)
                for module, _, alias, _ in imports if alias not in {"*", "_"}}
    external_prelude = {key[-1] for key in external_aliases if len(key) == 2}

    def canonical(path: tuple[str, ...], module: tuple[str, ...],
                  importing: tuple[str, ...] = ()) -> tuple[str, ...]:
        if path[0] == "::":
            return local_root + path[1:] if path[1] in external_prelude else path[1:]
        if path[0] == "crate":
            return local_root + path[1:]
        if path[0] == "self":
            return local_root + module + path[1:]
        if path[0] == "super":
            while path and path[0] == "super":
                module = module[:-1]
                path = path[1:]
            return local_root + module + path
        if path[0] in VERUS_ROOTS:
            return path
        binding = local_root + module + path[:1]
        if binding != importing and binding in bindings:
            return local_root + module + path
        if path[0] in external_prelude:
            return local_root + path
        return local_root + module + path

    aliases = dict(external_aliases)
    for module, path, alias, offset in imports:
        binding = local_root + module + (alias,)
        if alias not in {"*", "_"} and offset not in external_offsets:
            aliases.setdefault(binding, set()).add(canonical(path, module, importing=binding))

    def resolved(path: tuple[str, ...], module: tuple[str, ...]) -> set[tuple[str, ...]]:
        # cfg branches can declare different targets for the same binding.
        # Inspect every target rather than letting source order hide one.
        pending = [(canonical(path, module), {})]
        results = set()
        while pending:
            path, seen = pending.pop()
            if path[:1] != local_root:
                results.add(path)
                continue
            # A module alias can cover any prefix, not just the first name:
            # crate::bridge::renamed and bridge_alias::renamed both work.
            for end in range(len(path), 1, -1):
                binding = path[:end]
                # Repeated crate aliases can consume a path (root::root::x).
                # A repeated binding that cannot shorten it is an import cycle.
                if binding in aliases and (binding not in seen or len(path) < seen[binding]):
                    pending.extend((target + path[end:], seen | {binding: len(path)})
                                   for target in aliases[binding])
                    break
            else:
                results.add(path)
        return results

    for module, path, _, offset in imports:
        if any(target[0] in VERUS_ROOTS and target[-1] in ASSUMPTIONS
               for target in resolved(path, module)):
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
        if attribute_depth and value in {"verifier", "verus"} and following in {"::", "("} and index + 2 < len(tokens):
            attribute_index = index + 2
            # The verifier accepts macro-generated internal forms directly in
            # source too; handwritten internal(external_body) cannot bypass us.
            if (value == "verus" and tokens[attribute_index].value == "internal"
                    and attribute_index + 2 < len(tokens)
                    and tokens[attribute_index + 1].value == "("):
                attribute_index += 2
            attribute = tokens[attribute_index]
            if attribute.value in EXTERNALS:
                externals.add(token.offset)
            elif attribute.value in ASSUMPTION_ATTRIBUTES:
                assumptions.add(token.offset)
        if value not in ASSUMPTIONS or previous in {".", "fn"}:
            continue
        start = index
        while (start >= 2 and tokens[start - 1].value == "::"
               and tokens[start - 2].value.isidentifier()):
            start -= 2
        path = tuple(t.value for t in tokens[start:index + 1:2])
        if start and tokens[start - 1].value == "::":
            path = ("::",) + path
        # Qualified primitive references are forbidden even when used as a
        # function value rather than called here. Otherwise a local variable
        # could rename a primitive after importing only its parent module.
        if len(path) > 1 and any(target[0] in VERUS_ROOTS
                                 for target in resolved(path, modules[index])):
            assumptions.add(tokens[start].offset)
        elif len(path) == 1 and (
            (value == "assume_specification" and following in {"[", "<"})
            or (value != "assume_specification" and following == "(")
        ):
            assumptions.add(tokens[start].offset)
    return assumptions, externals


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
        ROOT / "crates/xlfn/src",
    ]

    assumes, unapproved_externals = audit_verus_files(search_dirs, allowlist_items)

    failed = False
    if assumes:
        print("ERROR: Verus verification contains local assumptions, admits or axioms (must be 0):", file=sys.stderr)
        for entry in assumes:
            print(f"  {entry}", file=sys.stderr)
        failed = True
    else:
        print("PASS: 0 project-local assumptions, admits or axioms found.")

    if unapproved_externals:
        print("ERROR: Unapproved external body/type/function specification found in Verus code:", file=sys.stderr)
        for entry in unapproved_externals:
            print(f"  {entry}", file=sys.stderr)
        failed = True
    else:
        print("PASS: All external body/type/function specifications are approved or 0 found.")

    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())

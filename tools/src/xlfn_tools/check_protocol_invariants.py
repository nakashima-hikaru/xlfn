"""Require semantic review of protocol checks in the enrolled Rust implementation.

This lexical introduction guard is not a Rust proof or a ban on runtime checks.
It includes macro bodies, ignores comments/literals and pure test scopes, and
requires every enrolled check/observation to have an occurrence-specific review.
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
from collections import Counter
from dataclasses import asdict, dataclass
from pathlib import Path

from ._repository import ROOT
from .check_panic_boundaries import TOKEN, attributes, rust_code

INVENTORY = ROOT / "tools/protocol_invariants.json"
KINDS = {
    "lifecycle_invariant_violation",
    "require_lifecycle_invariant",
    "unreachable",
    "unreachable_unchecked",
    "expect",
    "unwrap",
    "panic",
    "assert",
    "assert_eq",
    "assert_ne",
    "debug_assert",
    "debug_assert_eq",
    "debug_assert_ne",
    "abort",
}
CATEGORIES = {
    "observation",
    "acquisition",
    "revalidation",
    "exhaustion",
    "low_level",
    "local_recovery",
    "protocol_debt",
}
REQUIRED = {"id", "proposition", "established_by", "owner", "decision", "layer", "reason", "tests"}


def test_only(context: str) -> bool:
    """Exclude a cfg only when test=false makes it impossible; unknowns stay enrolled."""
    for attribute in re.findall(r"#\s*!?\s*\[([^]]*)\]", context):
        tokens = TOKEN.findall(attribute)
        if tokens == ["test"]:
            return True
        if tokens[:2] != ["cfg", "("]:
            continue
        position = 2

        def expression() -> bool | None:
            nonlocal position
            if position >= len(tokens):
                return None
            name = tokens[position]
            position += 1
            if name in {"all", "any", "not"} and tokens[position : position + 1] == ["("]:
                position += 1
                values = []
                while position < len(tokens) and tokens[position] != ")":
                    if tokens[position] == ",":
                        position += 1
                    else:
                        values.append(expression())
                position += 1
                if name == "not":
                    return not values[0] if len(values) == 1 and values[0] is not None else None
                if name == "all":
                    return False if False in values else None if None in values else True
                return True if True in values else None if None in values else False
            if name == "test":
                return False
            depth = 0
            while position < len(tokens):
                token = tokens[position]
                if depth == 0 and token in {",", ")"}:
                    break
                depth += (token == "(") - (token == ")")
                position += 1
            return None

        if expression() is False:
            return True
    return False


@dataclass(frozen=True, order=True)
class Reference:
    path: str
    context: str
    kind: str
    code: str


def references(path: str, source: str) -> Counter[Reference]:
    code = rust_code(source)
    annotated = rust_code(source, keep_literals=True)
    tokens = list(TOKEN.finditer(code))
    aliases = {
        tokens[index + 2].group(): match.group()
        for index, match in enumerate(tokens[:-2])
        if (match.group() in KINDS or match.group().startswith("fail_stop"))
        and tokens[index + 1].group() == "as"
    }
    frames: list[str] = []
    pending: list[str] = []
    pending_start = 0
    statement = 0
    found: Counter[Reference] = Counter()
    for index, match in enumerate(tokens):
        token = match.group()
        previous = tokens[index - 1].group() if index else ""
        following = tokens[index + 1].group() if index + 1 < len(tokens) else ""
        predicate = previous == "." and (
            token
            in {
                "phase",
                "phase_snapshot",
                "status",
                "matches",
                "matches_start",
                "matches_latest_open_attempt",
                "belongs_to",
                "admission_closed",
                "callbacks_retained_safely",
                "generation_callbacks_detached",
                "registration_state_unknown",
                "returns_match",
                "returns_are_quiescent",
                "returns_closed_and_quiescent",
            }
            or token.startswith(("is_", "has_", "can_"))
        )
        guard = token in KINDS or token.startswith("fail_stop") or token in aliases
        if (predicate or guard) and following in {"(", "!", "as"} and previous != "fn":
            context = " / ".join(frame for frame in frames if frame) or "module root"
            attrs = attributes(TOKEN.findall(annotated[pending_start : match.start()]))
            if attrs:
                context += " / " + attrs
            if not test_only(context):
                end = index + 1
                prefix = [entry.group() for entry in tokens[statement : index + 1]]
                parentheses = max(0, prefix.count("(") - prefix.count(")"))
                brackets = max(0, prefix.count("[") - prefix.count("]"))
                braces = 0
                while end < len(tokens):
                    suffix = tokens[end].group()
                    if not (parentheses or brackets or braces) and suffix in {";", ",", "{", "}"}:
                        break
                    parentheses = max(0, parentheses + (suffix == "(") - (suffix == ")"))
                    brackets = max(0, brackets + (suffix == "[") - (suffix == "]"))
                    braces = max(0, braces + (suffix == "{") - (suffix == "}"))
                    end += 1
                fragment = annotated[tokens[statement].start() : tokens[end - 1].end()]
                normalized = " ".join(TOKEN.findall(fragment))
                kind = (
                    "predicate"
                    if predicate
                    else f"alias:{aliases[token]}"
                    if token in aliases
                    else token
                )
                found[Reference(path, context, kind, normalized)] += 1
        if token == "{":
            named = re.search(r"\b(fn|mod)\s+([A-Za-z_][A-Za-z_0-9]*)", " ".join(pending))
            attrs = attributes(TOKEN.findall(annotated[pending_start : match.start()]))
            frames.append(
                " ".join(part for part in (attrs, named.group() if named else "") if part)
            )
            pending = []
            pending_start = match.end()
            statement = index + 1
        elif token == "}":
            if not frames:
                raise ValueError(f"unbalanced closing brace in {path}")
            frames.pop()
            pending = []
            pending_start = match.end()
            statement = index + 1
        elif token == ";":
            pending = []
            pending_start = match.end()
            statement = index + 1
        else:
            pending.append(token)
    if frames:
        raise ValueError(f"unbalanced opening brace in {path}")
    return found


def source_paths(root: Path) -> list[str]:
    listing = (
        subprocess.check_output(
            ["git", "ls-files", "--cached", "--others", "--exclude-standard", "-z", "--", "*.rs"],
            cwd=root,
        )
        .decode()
        .split("\0")
    )
    return sorted(path for path in set(filter(None, listing)) if (root / path).is_file())


def external_modules(path: str, source: str) -> list[tuple[str, bool]]:
    """Resolve only ordinary external module declarations; uncertain paths stay enrolled."""
    source_path = Path(path)
    base = (
        source_path.parent
        if source_path.name in {"lib.rs", "main.rs", "mod.rs"}
        else source_path.parent / source_path.stem
    )
    frames: list[str] = []
    module_names: list[str] = []
    pending: list[str] = []
    result: list[tuple[str, bool]] = []
    for token in TOKEN.findall(rust_code(source)):
        if token == "{":
            named = re.search(r"\bmod\s+([A-Za-z_][A-Za-z_0-9]*)", " ".join(pending))
            frames.append(attributes(pending))
            module_names.append(named.group(1) if named else "")
            pending = []
        elif token == "}":
            if not frames:
                raise ValueError(f"unbalanced closing brace in {path}")
            frames.pop()
            module_names.pop()
            pending = []
        elif token == ";":
            declaration = " ".join(pending)
            named = re.search(r"\bmod\s+([A-Za-z_][A-Za-z_0-9]*)\s*$", declaration)
            # #[path] and macro-produced paths need manual review, never an exclusion.
            if named and "path" not in TOKEN.findall(attributes(pending)):
                directory = base.joinpath(*filter(None, module_names))
                child = directory / named.group(1)
                guarded = test_only(" / ".join([*frames, attributes(pending)]))
                result.extend(
                    [(str(child.with_suffix(".rs")), guarded), (str(child / "mod.rs"), guarded)]
                )
            pending = []
        else:
            pending.append(token)
    return result


def test_source_paths(sources: dict[str, str]) -> set[str]:
    """Exclude a file only when every resolved incoming module edge is test-only."""
    edges = [
        (parent, child, guarded)
        for parent, source in sources.items()
        for child, guarded in external_modules(parent, source)
        if child in sources
    ]
    excluded: set[str] = set()
    while True:
        candidates = {child for parent, child, guarded in edges if guarded or parent in excluded}
        candidates -= {
            child for parent, child, guarded in edges if not guarded and parent not in excluded
        }
        if candidates == excluded:
            return excluded
        excluded = candidates


def repository_references(root: Path, scopes: list[str]) -> Counter[Reference]:
    found: Counter[Reference] = Counter()
    sources = {path: (root / path).read_text(encoding="utf-8") for path in source_paths(root)}
    excluded = test_source_paths(sources)
    for path, source in sources.items():
        if not any(Path(path).full_match(scope) for scope in scopes):
            continue
        if path in excluded:
            continue
        found.update(references(path, source))
    return found


def check(actual: Counter[Reference], reviews: list[dict]) -> list[str]:
    expected: Counter[Reference] = Counter()
    identifiers: set[str] = set()
    errors: list[str] = []
    for review in reviews:
        identifier = review.get("id", "")
        if identifier in identifiers:
            errors.append(f"duplicate review id: {identifier}")
        identifiers.add(identifier)
        if REQUIRED - review.keys() or any(not review.get(key) for key in REQUIRED):
            errors.append(f"incomplete semantic review: {identifier}")
        if review.get("decision") not in CATEGORIES:
            errors.append(f"invalid decision: {identifier}")
        if review.get("decision") == "protocol_debt" and not review.get("type_alternative"):
            errors.append(
                f"protocol debt needs a concrete receiver/payload alternative: {identifier}"
            )
        if not review.get("references"):
            errors.append(f"semantic review has no live references: {identifier}")
        for entry in review.get("references", []):
            reference = Reference(entry["path"], entry["context"], entry["kind"], entry["code"])
            count = entry.get("count")
            if reference in expected or not isinstance(count, int) or count < 1:
                errors.append(f"duplicate/invalid occurrence: {identifier}: {reference}")
                continue
            expected[reference] = count
    for reference, count in sorted((actual - expected).items()):
        errors.append(f"unclassified protocol check ({count}): {reference}")
    for reference, count in sorted((expected - actual).items()):
        errors.append(f"stale protocol review ({count}): {reference}")
    return errors


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--list",
        action="store_true",
        help="show enrolled references for manual review; never update the ledger",
    )
    parser.add_argument(
        "--all",
        action="store_true",
        help="survey other production Rust files; does not expand the enrolled gate",
    )
    args = parser.parse_args()
    inventory = json.loads(INVENTORY.read_text(encoding="utf-8"))
    actual = repository_references(
        ROOT, ["crates/*/src/**/*.rs"] if args.all else inventory["scopes"]
    )
    if args.list or args.all:
        print(
            json.dumps(
                [
                    {**asdict(reference), "count": count}
                    for reference, count in sorted(actual.items())
                ],
                indent=2,
            )
        )
        return 0
    errors = check(actual, inventory["reviews"])
    if errors:
        print("\n".join(errors), file=sys.stderr)
        return 1
    print(
        f"Protocol invariants: {sum(actual.values())} classified references in {len(inventory['reviews'])} semantic reviews."
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

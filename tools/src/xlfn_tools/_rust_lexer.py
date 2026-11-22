"""Minimal fail-closed Rust comment and literal blanker for static audits."""

from __future__ import annotations

import re

RAW_STRING = re.compile(r'(?:br|cr|r)(#*)"')
CHARACTER = re.compile(r"(?:b)?'(?:[^'\\\n]|\\(?:u\{[0-9a-fA-F_]+\}|x[0-9a-fA-F]{2}|.))'")


def blank_rust_comments_and_literals(source: str, *, keep_literals: bool = False) -> str:
    """Blank comments and (by default) literals while preserving line offsets."""
    output = list(source)
    size = len(source)
    index = 0

    def blank(start: int, end: int) -> None:
        for position in range(start, end):
            if source[position] not in "\r\n":
                output[position] = " "

    while index < size:
        start = index
        literal = False
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
                literal = True
                closing = '"' + raw.group(1)
                end = source.find(closing, raw.end())
                if end == -1:
                    raise ValueError("unterminated raw string")
                index = end + len(closing)
            elif source[index] == '"' or source[index : index + 2] in {'b"', 'c"'}:
                literal = True
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
                    literal = True
                    index = character.end()
                else:
                    index += 1
                    continue
        if not (keep_literals and literal):
            blank(start, min(index, size))
    return "".join(output)

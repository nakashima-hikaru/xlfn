#!/usr/bin/env python3
"""Compile and run guide Rust examples against this checkout's production API."""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile

from check import FENCE, GUIDE, source_check

ROOT = GUIDE.parent
WHOLE_FILE = re.compile(r"```rust\n\{\{#include ([^}:]+)\}\}\n```", re.MULTILINE)


def prepare_chapter(path: Path) -> tuple[str, set[Path]]:
    """Whole source-file examples are checked in their actual Cargo project."""
    projects: set[Path] = set()

    def source_example(match: re.Match) -> str:
        source = (path.parent / match[1]).resolve()
        if not source.is_relative_to(ROOT) or not source.is_file():
            raise ValueError(f"{path}: Rust source must be a file in this checkout: {source}")
        for parent in source.parents:
            if parent == ROOT:
                break
            manifest = parent / "Cargo.toml"
            if manifest.is_file():
                projects.add(manifest)
                return "```text\nCompiled in " + str(manifest.relative_to(ROOT)) + "\n```"
        raise ValueError(f"{path}: included Rust source has no standalone Cargo project: {source}")

    text = WHOLE_FILE.sub(source_example, path.read_text(encoding="utf-8"))
    # Hidden doctest setup is ordinary Markdown, expanded for both mdBook and
    # rustdoc. Keep unsupported include syntax from silently skipping examples.
    def setup(match: re.Match) -> str:
        include = (path.parent / match[1]).resolve()
        if not include.is_relative_to(GUIDE / "fixtures") or include.suffix != ".md":
            raise ValueError(f"{path}: unsupported doctest include: {include}")
        return include.read_text(encoding="utf-8").rstrip()

    text = re.sub(r"\{\{#include ([^}:]+)\}\}", setup, text)
    if "{{#include" in text:
        raise ValueError(f"{path}: unsupported include syntax")
    for line in text.splitlines():
        if match := FENCE.match(line):
            options = {option.strip() for option in match[2].split(",")}
            if "rust" in options and any(option.startswith("ignore") for option in options):
                raise ValueError(f"{path}: Rust examples must compile; use no_run for host-dependent code")
    return text, projects


def build_libraries() -> tuple[dict[str, str], set[str]]:
    command = ["cargo", "build", "-p", "xlfn", "--lib", "--no-default-features",
               "--features", "async,cache,handles,rtd", "--locked",
               "--message-format=json-render-diagnostics"]
    result = subprocess.run(command, cwd=ROOT, text=True, stdout=subprocess.PIPE)
    libraries: dict[str, str] = {}
    directories: set[str] = set()
    for line in result.stdout.splitlines():
        message = json.loads(line)
        if message.get("reason") == "compiler-message":
            print(message["message"].get("rendered", ""), end="")
        if message.get("reason") != "compiler-artifact":
            continue
        for filename in message["filenames"]:
            path = Path(filename)
            directories.add(str(path.parent))
            if (path.parent / "deps").is_dir():
                directories.add(str(path.parent / "deps"))
            if path.suffix == ".rlib" and message["target"]["name"] in {"xlfn", "tracing"}:
                libraries[message["target"]["name"]] = filename
    result.check_returncode()
    if set(libraries) != {"xlfn", "tracing"}:
        raise ValueError("Cargo did not report the required xlfn and tracing library artifacts")
    return libraries, directories


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--chapter", action="append", help="source filename, e.g. values.md")
    args = parser.parse_args()
    errors, chapters, _ = source_check(GUIDE)
    if errors:
        raise SystemExit("\n".join(errors))
    if args.chapter:
        selected = set(args.chapter)
        known = {path.name for path in chapters}
        if selected - known:
            raise SystemExit(f"Unknown chapters: {sorted(selected - known)}")
        chapters = [path for path in chapters if path.name in selected]
    libraries, directories = build_libraries()
    flags = [flag for name, path in sorted(libraries.items()) for flag in ["--extern", f"{name}={path}"]]
    flags += [flag for path in sorted(directories) for flag in ["-L", f"dependency={path}"]]
    environment = dict(os.environ, CARGO_PKG_VERSION="0.0.0")
    projects: set[Path] = set()
    failures: list[str] = []
    with tempfile.TemporaryDirectory(prefix="xlfn-guide-") as directory:
        for chapter in chapters:
            text, included_projects = prepare_chapter(chapter)
            projects.update(included_projects)
            prepared = Path(directory) / chapter.name
            prepared.write_text(text, encoding="utf-8")
            print(f"Checking guide examples: {chapter.name}", flush=True)
            result = subprocess.run(
                ["rustdoc", "--test", "--edition=2024", *flags, str(prepared)],
                cwd=ROOT, env=environment,
            )
            if result.returncode:
                failures.append(chapter.name)
    for manifest in sorted(projects):
        subprocess.run(["cargo", "check", "--manifest-path", str(manifest), "--locked"],
                       cwd=ROOT, check=True)
    if failures:
        raise SystemExit(f"Guide example failures: {', '.join(failures)}")
    print(f"Guide examples passed: {len(chapters)} chapters, {len(projects)} standalone source projects")


if __name__ == "__main__":
    main()

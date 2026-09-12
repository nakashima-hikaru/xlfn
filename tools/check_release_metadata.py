#!/usr/bin/env python3
"""Check release files and exact internal dependency versions without publishing.

This checks the repository's explicit workspace-member layout. Cargo package
remains responsible for assembling and building the actual crate archives.
"""

from __future__ import annotations

from pathlib import Path
import sys
import tomllib

ROOT = Path(__file__).resolve().parents[1]
LICENSES = ("LICENSE-MIT", "LICENSE-APACHE")
DEPENDENCIES = ("dependencies", "build-dependencies", "dev-dependencies")


def manifest(path: Path) -> dict:
    with path.open("rb") as source:
        return tomllib.load(source)


def package_value(package: dict, workspace: dict, key: str, default=None):
    value = package.get(key, default)
    if isinstance(value, dict) and value.get("workspace") is True:
        return workspace["package"][key]
    return value


def dependency_tables(document: dict):
    for section in DEPENDENCIES:
        yield section, document.get(section, {})
    for target, configuration in document.get("target", {}).items():
        for section in DEPENDENCIES:
            yield f"target.{target}.{section}", configuration.get(section, {})


def check(root: Path) -> list[str]:
    root = root.resolve()
    workspace = manifest(root / "Cargo.toml")["workspace"]
    members = {
        (root / member).resolve(): manifest(root / member / "Cargo.toml")
        for member in workspace["members"]
    }
    originals = {name: (root / name).read_bytes() for name in LICENSES}
    errors = []

    def check_dependencies(directory: Path, label: str, dependencies: dict) -> None:
        for name, dependency in dependencies.items():
            if not isinstance(dependency, dict) or "path" not in dependency:
                continue
            target = (directory / dependency["path"]).resolve()
            if target not in members:
                continue
            version = package_value(members[target]["package"], workspace, "version")
            expected = f"={version}"
            if dependency.get("version") != expected:
                errors.append(
                    f"{label}.{name}: internal path dependency needs version {expected!r}, "
                    f"found {dependency.get('version')!r}"
                )

    check_dependencies(root, "workspace.dependencies", workspace.get("dependencies", {}))
    for directory, document in members.items():
        package = document["package"]
        publish = package_value(package, workspace, "publish", True)
        if publish is False or publish == []:
            continue
        relative = directory.relative_to(root).as_posix()
        if not (directory / "README.md").is_file():
            errors.append(f"{relative}/README.md: missing release readme")
        for name, original in originals.items():
            path = directory / name
            if not path.is_file():
                errors.append(f"{relative}/{name}: missing release license")
            elif path.read_bytes() != original:
                errors.append(f"{relative}/{name}: differs from repository {name}")
        for section, dependencies in dependency_tables(document):
            check_dependencies(directory, f"{relative}/Cargo.toml:{section}", dependencies)
    return errors


def main() -> int:
    try:
        errors = check(ROOT)
    except (OSError, KeyError, TypeError, ValueError) as error:
        print(f"Release metadata: {error}", file=sys.stderr)
        return 1
    if errors:
        print("\n".join(errors), file=sys.stderr)
        return 1
    print("Release metadata: readmes, license copies, and internal version pins are consistent.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

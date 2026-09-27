"""Record and validate the CI-built x86_64 XLL artifact set."""

from __future__ import annotations

import argparse
import hashlib
import json
import struct
from pathlib import Path


ARCHITECTURE = "x86_64"
TARGET = "x86_64-pc-windows-msvc"
ARTIFACT_NAME = "excel-comparison-x86_64"
REGISTRATION_COUNTS = (10, 100, 1_000, 5_000)
FILES = tuple(
    f"{implementation}/{name}"
    for implementation in ("xlfn", "excel_dna")
    for name in ("benchmark.xll", *(f"registration-{count}.xll" for count in REGISTRATION_COUNTS))
)


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def pe_machine(path: Path) -> int:
    with path.open("rb") as stream:
        header = stream.read(64)
        if len(header) != 64 or header[:2] != b"MZ":
            raise ValueError(f"{path} has no DOS header")
        offset = struct.unpack_from("<I", header, 0x3C)[0]
        stream.seek(offset)
        pe = stream.read(6)
        if len(pe) != 6 or pe[:4] != b"PE\0\0":
            raise ValueError(f"{path} has no PE header")
        return struct.unpack_from("<H", pe, 4)[0]


def create(root: Path, commit: str, run_id: str, run_attempt: str) -> dict:
    if not commit or not run_id or not run_attempt:
        raise ValueError("CI commit, run ID, and attempt are required")
    files = {}
    for relative in FILES:
        path = root / relative
        if not path.is_file():
            raise FileNotFoundError(path)
        if pe_machine(path) != 0x8664:
            raise ValueError(f"{path} is not an x86_64 PE image")
        files[relative] = {"sha256": sha256(path), "bytes": path.stat().st_size}
    manifest = {
        "schema": 1,
        "artifact_name": ARTIFACT_NAME,
        "architecture": ARCHITECTURE,
        "target": TARGET,
        "source": "github-actions",
        "commit": commit,
        "run_id": str(run_id),
        "run_attempt": str(run_attempt),
        "files": files,
    }
    (root / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
    return manifest


def verify(root: Path, *, run_id: str | None = None, commit: str | None = None) -> dict:
    manifest = json.loads((root / "manifest.json").read_text(encoding="utf-8"))
    expected = {
        "schema": 1,
        "artifact_name": ARTIFACT_NAME,
        "architecture": ARCHITECTURE,
        "target": TARGET,
        "source": "github-actions",
    }
    for key, value in expected.items():
        if manifest.get(key) != value:
            raise ValueError(f"manifest {key} is {manifest.get(key)!r}, expected {value!r}")
    if not manifest.get("commit") or not manifest.get("run_id") or not manifest.get("run_attempt"):
        raise ValueError("manifest lacks CI provenance")
    if run_id is not None and manifest["run_id"] != str(run_id):
        raise ValueError(f"artifact came from run {manifest['run_id']}, expected {run_id}")
    if commit is not None and manifest["commit"].lower() != commit.lower():
        raise ValueError(f"artifact came from commit {manifest['commit']}, expected {commit}")
    if set(manifest.get("files", {})) != set(FILES):
        raise ValueError("artifact file set is incomplete or contains unexpected entries")
    for relative in FILES:
        path = root / relative
        if not path.is_file():
            raise FileNotFoundError(path)
        entry = manifest["files"][relative]
        if path.stat().st_size != entry["bytes"] or sha256(path) != entry["sha256"]:
            raise ValueError(f"artifact checksum mismatch: {relative}")
        if pe_machine(path) != 0x8664:
            raise ValueError(f"artifact is not x86_64: {relative}")
    return manifest


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    make = commands.add_parser("create")
    make.add_argument("--root", type=Path, required=True)
    make.add_argument("--commit", required=True)
    make.add_argument("--run-id", required=True)
    make.add_argument("--run-attempt", required=True)
    check = commands.add_parser("verify")
    check.add_argument("--root", type=Path, required=True)
    check.add_argument("--commit")
    check.add_argument("--run-id")
    args = parser.parse_args()
    if args.command == "create":
        manifest = create(args.root, args.commit, args.run_id, args.run_attempt)
    else:
        manifest = verify(args.root, commit=args.commit, run_id=args.run_id)
    print(f"Verified {len(manifest['files'])} {ARCHITECTURE} XLLs from CI run {manifest['run_id']}")


if __name__ == "__main__":
    main()

"""Record and validate the CI-built x86_64 XLL artifact set."""

from __future__ import annotations

import hashlib
import json
import struct
from pathlib import Path
from typing import Any

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
        return int(struct.unpack_from("<H", pe, 4)[0])


def create(root: Path, commit: str, run_id: str, run_attempt: str) -> dict[str, Any]:
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
    manifest: dict[str, Any] = {
        "schema": 1,
        "artifact_name": ARTIFACT_NAME,
        "architecture": ARCHITECTURE,
        "target": TARGET,
        "source": "github-actions",
        "excel_dna_compilation": "NativeAOT",
        "commit": commit,
        "run_id": str(run_id),
        "run_attempt": str(run_attempt),
        "files": files,
    }
    symbols = {}
    for implementation in ("xlfn", "excel_dna"):
        for path in sorted((root / "symbols" / implementation).rglob("*.pdb")):
            symbols[path.relative_to(root).as_posix()] = {
                "sha256": sha256(path),
                "bytes": path.stat().st_size,
                "image": f"{implementation}/benchmark.xll",
                "image_sha256": files[f"{implementation}/benchmark.xll"]["sha256"],
            }
    if symbols:
        manifest["symbols"] = symbols
    (root / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
    return manifest


def verify(root: Path, *, run_id: str | None = None, commit: str | None = None) -> dict[str, Any]:
    # Reject links (including Windows junctions) before opening any payload.
    for path in (root, *root.rglob("*")):
        if path.is_symlink() or path.is_junction():
            raise ValueError(f"artifact contains a link: {path}")
        if not (path.is_dir() or path.is_file()):
            raise ValueError(f"artifact contains a special file: {path}")
    manifest = json.loads((root / "manifest.json").read_text(encoding="utf-8"))
    expected = {
        "schema": 1,
        "artifact_name": ARTIFACT_NAME,
        "architecture": ARCHITECTURE,
        "target": TARGET,
        "source": "github-actions",
        "excel_dna_compilation": "NativeAOT",
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
    allowed = {"manifest.json", *FILES, *manifest.get("symbols", {})}
    actual = {p.relative_to(root).as_posix() for p in root.rglob("*") if p.is_file()}
    if actual != allowed:
        raise ValueError("artifact contains missing or unexpected files")
    for relative in FILES:
        path = root / relative
        if not path.is_file():
            raise FileNotFoundError(path)
        entry = manifest["files"][relative]
        if path.stat().st_size != entry["bytes"] or sha256(path) != entry["sha256"]:
            raise ValueError(f"artifact checksum mismatch: {relative}")
        if pe_machine(path) != 0x8664:
            raise ValueError(f"artifact is not x86_64: {relative}")
    verify_symbols(root, manifest)
    return manifest


def verify_symbols(root: Path, manifest: dict[str, Any], *, required: tuple[str, ...] = ()) -> None:
    """Verify optional symbols, bound to the baseline XLL from the same CI build."""
    symbols = manifest.get("symbols", {})
    if not isinstance(symbols, dict):
        raise ValueError("manifest symbols must be an object")
    actual = {p.relative_to(root).as_posix() for p in (root / "symbols").rglob("*.pdb")}
    if actual != set(symbols):
        raise ValueError("artifact symbols differ from the manifest")
    available = set()
    for relative, entry in symbols.items():
        parts = relative.split("/")
        if (
            len(parts) < 3
            or parts[0] != "symbols"
            or parts[1] not in ("xlfn", "excel_dna")
            or any(p in ("", ".", "..") or "\\" in p or ":" in p for p in parts)
            or not relative.endswith(".pdb")
        ):
            raise ValueError(f"invalid symbol path: {relative}")
        path = root / relative
        if not path.resolve().is_relative_to(root.resolve()):
            raise ValueError(f"symbol escapes artifact root: {relative}")
        image = f"{parts[1]}/benchmark.xll"
        if (
            entry.get("image") != image
            or entry.get("image_sha256") != manifest["files"][image]["sha256"]
        ):
            raise ValueError(f"symbol image binding mismatch: {relative}")
        if path.stat().st_size != entry.get("bytes") or sha256(path) != entry.get("sha256"):
            raise ValueError(f"symbol checksum mismatch: {relative}")
        available.add(parts[1])
    if set(required) - available:
        raise ValueError(
            "Matching PDBs are required; download a new symbol-bearing CI artifact: "
            + ", ".join(sorted(set(required) - available))
        )

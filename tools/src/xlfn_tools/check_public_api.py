"""Compare the facade's supported API to reviewed, platform-specific snapshots."""

import argparse
import difflib
import os
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
TOOLCHAIN = "nightly-2026-09-28"
PUBLIC_API_VERSION = "cargo-public-api 0.52.0"
TARGETS = ("aarch64-apple-darwin", "x86_64-pc-windows-msvc")
FEATURES = {"core": "", "full": "async-builtin,rtd,handles,cache,serde,jiff"}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--update", action="store_true", help="write snapshots for review")
    args = parser.parse_args()
    version = subprocess.check_output(["cargo", "public-api", "--version"], text=True).strip()
    if version != PUBLIC_API_VERSION:
        parser.error(f"expected {PUBLIC_API_VERSION}, got {version}")
    lockfile = ROOT / "Cargo.lock"
    original_lock = lockfile.read_bytes()
    directory = ROOT / "crates/xlfn/public-api"
    directory.mkdir(exist_ok=True)
    failed = False
    for target in TARGETS:
        for configuration, features in FEATURES.items():
            # Rust-only BLAKE3 avoids requiring an MSVC assembler for rustdoc on macOS.
            if features and target.endswith("windows-msvc"):
                features += ",blake3/pure"
            command = [
                "cargo",
                f"+{TOOLCHAIN}",
                "public-api",
                "-p",
                "xlfn",
                "--target",
                target,
                "--no-default-features",
                "--omit",
                "blanket-impls",
                "--color",
                "never",
            ]
            if features:
                command += ["--features", features]
            output = subprocess.check_output(
                command,
                cwd=ROOT,
                text=True,
                env=dict(os.environ, CARGO_TARGET_DIR=str(ROOT / "target/public-api")),
            )
            if lockfile.read_bytes() != original_lock:
                raise RuntimeError(
                    "API extraction changed Cargo.lock; review dependency resolution"
                )
            name = f"{target}-{configuration}.txt"
            actual = (
                f"# {TOOLCHAIN}; {PUBLIC_API_VERSION}; features={features or '(none)'}\n{output}"
            )
            path = directory / name
            if args.update:
                path.write_text(actual)
                print(f"updated {name}")
            else:
                expected = path.read_text() if path.exists() else ""
                if actual != expected:
                    failed = True
                    print(
                        "".join(
                            difflib.unified_diff(
                                expected.splitlines(keepends=True),
                                actual.splitlines(keepends=True),
                                fromfile=str(path),
                                tofile="current public API",
                            )
                        )
                    )
                else:
                    print(f"verified {name}")
    if failed:
        print("Review API changes, then run just public-api-update to accept the snapshots.")
    return int(failed)


if __name__ == "__main__":
    raise SystemExit(main())

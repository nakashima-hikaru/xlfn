"""Compare the facade's supported API to reviewed, platform-specific snapshots."""

import argparse
import difflib
import os
import re
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
TOOLCHAIN = "nightly-2026-09-28"
PUBLIC_API_VERSION = "cargo-public-api 0.52.0"
TARGETS = ("aarch64-apple-darwin", "x86_64-pc-windows-msvc")
FEATURES = {"core": "", "full": "async-builtin,rtd,handles,cache,serde,jiff"}


# rustdoc JSON only names cross-crate re-exports. It does not include their
# definitions or implementation blocks in the facade JSON. Track the upstream
# definitions that are part of the supported xlfn surface separately.
ENGINE_CONTRACTS = re.compile(
    r"\bxlfn_engine::(?:diagnostic_id|error|value_types)(?:::|$)"
)
SERVICE_CONTRACTS = re.compile(
    r"^[^(]*\bxlfn_services::cache::(?:CacheBudget|CacheEndpoint|CacheWeightPolicy|CanonicalF64)\b"
)


def contracts(output: str, pattern: re.Pattern[str], crate: str) -> str:
    lines = [line for line in output.splitlines() if pattern.search(line)]
    if not lines:
        raise RuntimeError(f"{crate} public-api output omitted its facade contracts")
    return "\n".join(lines) + "\n"


def extract(package: str, target: str, features: str, original_lock: bytes) -> str:
    command = [
        "cargo",
        f"+{TOOLCHAIN}",
        "public-api",
        "-p",
        package,
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
        env=dict(
            os.environ,
            CARGO_TARGET_DIR=os.environ.get("CARGO_TARGET_DIR", str(ROOT / "target/public-api")),
        ),
    )
    if (ROOT / "Cargo.lock").read_bytes() != original_lock:
        raise RuntimeError("API extraction changed Cargo.lock; review dependency resolution")
    return output


def write_or_compare(path: Path, actual: str, update: bool) -> bool:
    if update:
        path.write_text(actual)
        print(f"updated {path.name}")
        return True
    expected = path.read_text() if path.exists() else ""
    if actual == expected:
        print(f"verified {path.name}")
        return True
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
    return False


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--update", action="store_true", help="write snapshots for review")
    args = parser.parse_args()
    version = subprocess.check_output(["cargo", "public-api", "--version"], text=True).strip()
    if version != PUBLIC_API_VERSION:
        parser.error(f"expected {PUBLIC_API_VERSION}, got {version}")
    original_lock = (ROOT / "Cargo.lock").read_bytes()
    directory = ROOT / "crates/xlfn/public-api"
    directory.mkdir(exist_ok=True)
    passed = True
    for target in TARGETS:
        for configuration, features in FEATURES.items():
            # Rust-only BLAKE3 lets rustdoc run without an assembler or macOS compiler.
            facade_features = f"{features},blake3/pure" if features else ""
            header = (
                f"# {TOOLCHAIN}; {PUBLIC_API_VERSION}; features={facade_features or '(none)'}\n"
            )
            name = f"{target}-{configuration}"
            facade = extract("xlfn", target, facade_features, original_lock)
            passed &= write_or_compare(directory / f"{name}.txt", header + facade, args.update)

            # Cache definitions are present only with the public feature enabled.
            engine_features = "serde" if features else ""
            engine = contracts(
                extract("xlfn-engine", target, engine_features, original_lock),
                ENGINE_CONTRACTS,
                "xlfn-engine",
            )
            service = ""
            if features:
                service = contracts(
                    extract("xlfn-services", target, "cache", original_lock),
                    SERVICE_CONTRACTS,
                    "xlfn-services",
                )
            passed &= write_or_compare(
                directory / f"{name}-reexports.txt", header + engine + service, args.update
            )
    if not passed:
        print("Review API changes, then run just public-api-update to accept the snapshots.")
    return int(not passed)


if __name__ == "__main__":
    raise SystemExit(main())

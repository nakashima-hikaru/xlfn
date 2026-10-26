#!/usr/bin/env bash
set -euo pipefail

# install_verus.sh: Downloads and installs the Verus verification toolchain.
# Usage: ./tools/install_verus.sh [INSTALL_DIR]
# INSTALL_DIR must be a new dedicated directory. Existing paths are never changed.
# Default: ~/.local/share/xlfn/verus/<version>/<platform>

VERUS_VERSION="${VERUS_VERSION:-0.2026.10.26.d17fbd0}"
SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"

OS="$(uname -s)"
ARCH="$(uname -m)"

case "${OS}" in
    Darwin)
        case "${ARCH}" in
            arm64) ASSET_NAME="verus-${VERUS_VERSION}-arm64-macos.zip" ;;
            x86_64) ASSET_NAME="verus-${VERUS_VERSION}-x86-macos.zip" ;;
            *) echo "Unsupported Darwin architecture: ${ARCH}" >&2; exit 1 ;;
        esac
        ;;
    Linux)
        case "${ARCH}" in
            x86_64) ASSET_NAME="verus-${VERUS_VERSION}-x86-linux.zip" ;;
            *) echo "Unsupported Linux architecture: ${ARCH}" >&2; exit 1 ;;
        esac
        ;;
    *)
        echo "Unsupported OS: ${OS}" >&2
        exit 1
        ;;
esac

PLATFORM="${ASSET_NAME#verus-${VERUS_VERSION}-}"
PLATFORM="${PLATFORM%.zip}"
INSTALL_DIR="${1:-${HOME}/.local/share/xlfn/verus/${VERUS_VERSION}/${PLATFORM}}"
if [ -e "${INSTALL_DIR}" ] || [ -L "${INSTALL_DIR}" ]; then
    echo "Install destination already exists; choose a new dedicated directory: ${INSTALL_DIR}" >&2
    exit 1
fi

# The manifest is reviewed with the installer. A version override cannot bypass
# verification: new versions/platforms require a committed trusted digest.
EXPECTED_SHA256="$(python3 -B - "${SCRIPT_DIR}/verus_release_sha256.json" "${VERUS_VERSION}" "${ASSET_NAME}" <<'PYHASH'
import json
from pathlib import Path
import re
import sys

try:
    digest = json.loads(Path(sys.argv[1]).read_text())[sys.argv[2]][sys.argv[3]]
    if not isinstance(digest, str) or re.fullmatch(r"[0-9a-f]{64}", digest) is None:
        raise ValueError("invalid SHA-256 digest")
except (OSError, KeyError, TypeError, ValueError) as error:
    raise SystemExit(f"No trusted Verus SHA-256 for the requested release: {error}")
print(digest)
PYHASH
)"

DOWNLOAD_URL="https://github.com/verus-lang/verus/releases/download/release/${VERUS_VERSION}/${ASSET_NAME}"
TMP_DIR="$(mktemp -d)"
trap 'rm -rf "${TMP_DIR}"' EXIT

echo "==> Downloading Verus ${VERUS_VERSION} (${ASSET_NAME})..."
curl -fsSL --retry 3 "${DOWNLOAD_URL}" -o "${TMP_DIR}/${ASSET_NAME}"

echo "==> Verifying release SHA-256..."
python3 -B - "${TMP_DIR}/${ASSET_NAME}" "${EXPECTED_SHA256}" <<'PYHASH'
import hashlib
from pathlib import Path
import sys

with Path(sys.argv[1]).open("rb") as archive:
    actual = hashlib.file_digest(archive, "sha256").hexdigest()
if actual != sys.argv[2]:
    raise SystemExit("Verus release SHA-256 mismatch; refusing to extract or execute")
PYHASH

echo "==> Extracting Verus..."
unzip -q "${TMP_DIR}/${ASSET_NAME}" -d "${TMP_DIR}/extracted"

# Use the release's exact platform directory.
VERUS_EXTRACTED_DIR="${TMP_DIR}/extracted/verus-${PLATFORM}"

if [ -z "${VERUS_EXTRACTED_DIR}" ] || [ ! -d "${VERUS_EXTRACTED_DIR}" ]; then
    echo "Failed to locate extracted Verus directory" >&2
    exit 1
fi

# Verus launches the Rust compiler it was built against, independently of the
# workspace toolchain. Read the release's own metadata so VERUS_VERSION overrides
# also install the matching compiler. The toolchain string can contain a rustup
# annotation after its exact name, e.g. "1.98.1-x86_64-unknown-linux-gnu (default)".
VERUS_RUST_TOOLCHAIN="$(python3 -B - "${VERUS_EXTRACTED_DIR}/version.json" "${VERUS_VERSION}" <<'PY'
import json
from pathlib import Path
import re
import sys

try:
    release = json.loads(Path(sys.argv[1]).read_text())["verus"]
    if release["version"] != sys.argv[2]:
        raise ValueError("release version does not match the requested Verus version")
    toolchain = release["toolchain"].split()[0]
    if re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._+-]*", toolchain) is None:
        raise ValueError("invalid Rust toolchain name")
except (OSError, KeyError, TypeError, ValueError, IndexError, AttributeError) as error:
    raise SystemExit(f"Invalid Verus release metadata: {error}")
print(toolchain)
PY
)"

# These are the upstream required runtime components plus the solver and
# release metadata. Do not copy build directories, arbitrary sibling binaries,
# or archive extras into the developer's command directory.
if [ "${OS}" = "Darwin" ]; then
    LIB_EXTENSION=dylib
else
    LIB_EXTENSION=so
fi
COMPONENTS=(verus rust_verify z3 libverus_builtin.rlib
    "libverus_builtin_macros.${LIB_EXTENSION}"
    "libverus_state_machines_macros.${LIB_EXTENSION}"
    libvstd.rlib vstd.vir version.json)
for component in "${COMPONENTS[@]}"; do
    if [ ! -f "${VERUS_EXTRACTED_DIR}/${component}" ] || [ -L "${VERUS_EXTRACTED_DIR}/${component}" ]; then
        echo "Missing or invalid Verus runtime component: ${component}" >&2
        exit 1
    fi
done

echo "==> Installing Verus Rust toolchain ${VERUS_RUST_TOOLCHAIN}..."
rustup toolchain install "${VERUS_RUST_TOOLCHAIN}" --profile minimal

# Exclusive mkdir refuses a path created since the earlier check. Only this
# newly created directory is populated; no existing binary is overwritten.
mkdir -p -- "$(dirname -- "${INSTALL_DIR}")"
mkdir -- "${INSTALL_DIR}"
echo "==> Installing runtime to ${INSTALL_DIR}..."
for component in "${COMPONENTS[@]}"; do
    cp -- "${VERUS_EXTRACTED_DIR}/${component}" "${INSTALL_DIR}/${component}"
done

if [ "${OS}" = "Darwin" ]; then
    echo "==> Clearing macOS quarantine attributes..."
    for component in "${COMPONENTS[@]}"; do
        xattr -d com.apple.quarantine "${INSTALL_DIR}/${component}" 2>/dev/null || true
    done
fi

echo "==> Verifying installed Verus..."
"${INSTALL_DIR}/verus" --version
echo "==> Verus installed successfully to ${INSTALL_DIR}."
echo "Add this directory to PATH: ${INSTALL_DIR}"

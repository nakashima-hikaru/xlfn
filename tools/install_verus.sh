#!/usr/bin/env bash
set -euo pipefail

# install_verus.sh: Downloads and installs the Verus verification toolchain.
# Usage: ./tools/install_verus.sh [INSTALL_DIR]
# Default INSTALL_DIR is ~/.cargo/bin

VERUS_VERSION="${VERUS_VERSION:-0.2026.09.13.671956e}"
INSTALL_DIR="${1:-${HOME}/.cargo/bin}"

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

DOWNLOAD_URL="https://github.com/verus-lang/verus/releases/download/release/${VERUS_VERSION}/${ASSET_NAME}"
TMP_DIR="$(mktemp -d)"
trap 'rm -rf "${TMP_DIR}"' EXIT

echo "==> Downloading Verus ${VERUS_VERSION} (${ASSET_NAME})..."
curl -sSL --retry 3 "${DOWNLOAD_URL}" -o "${TMP_DIR}/${ASSET_NAME}"

echo "==> Extracting Verus..."
unzip -q "${TMP_DIR}/${ASSET_NAME}" -d "${TMP_DIR}/extracted"

# Find extracted folder (typically verus-<arch>-<os>)
VERUS_EXTRACTED_DIR="$(find "${TMP_DIR}/extracted" -mindepth 1 -maxdepth 1 -type d | head -n 1)"

if [ -z "${VERUS_EXTRACTED_DIR}" ] || [ ! -d "${VERUS_EXTRACTED_DIR}" ]; then
    echo "Failed to locate extracted Verus directory" >&2
    exit 1
fi

mkdir -p "${INSTALL_DIR}"

echo "==> Installing binaries to ${INSTALL_DIR}..."
cp -R "${VERUS_EXTRACTED_DIR}"/* "${INSTALL_DIR}/"

if [ "${OS}" = "Darwin" ]; then
    echo "==> Clearing macOS quarantine attributes..."
    xattr -dr com.apple.quarantine "${INSTALL_DIR}" 2>/dev/null || true
fi

echo "==> Verus installed successfully to ${INSTALL_DIR}."
"${INSTALL_DIR}/verus" --version || true

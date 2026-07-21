#!/usr/bin/env bash
# Extract the raw prepared kernel bundle from the pinned official libkrunfw
# release. This script intentionally does not call the result "vmlinux": the
# krunfw_get_kernel() API returns flattened guest-memory bytes, not an ELF file.

set -euo pipefail

LIBKRUNFW_VERSION="5.5.0"
ARCHIVE_NAME="libkrunfw-x86_64.tgz"
ARCHIVE_SHA256="c169206b01c89fbe134f1728bf4f988702bc7f73b4cf73e6fdece447d6fceca1"
LIBRARY_MEMBER="lib64/libkrunfw.so.5.5.0"
LIBRARY_SHA256="6df51f65d7f99fc22215e69a4236c770b1588ceb6777eca014f92b366517d237"
RAW_BUNDLE_SHA256="781375ea09f4279ec5bfeab26ecc7067358a3fc98190467e2ab01cc6e98936dd"
RAW_BUNDLE_SIZE="21364736"
RAW_BUNDLE_GUEST_LOAD_ADDR="0x0000000001000000"
RAW_BUNDLE_ENTRY_ADDR="0x0000000001000123"
RELEASE_URL="https://github.com/libkrun/libkrunfw/releases/download/v${LIBKRUNFW_VERSION}/${ARCHIVE_NAME}"

if [[ $# -ne 0 ]]; then
    echo "Usage: bash scripts/extract_kernel.sh" >&2
    echo "The source version, URL, member, and SHA-256 are pinned in this script." >&2
    exit 2
fi
if [[ "$(uname -s)" != "Linux" ]]; then
    echo "extract_kernel.sh requires Linux because the pinned archive contains an ELF .so." >&2
    exit 1
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
KERNEL_DIR="${PROJECT_ROOT}/src/libkrunfw-win/kernel"
BUNDLE_PATH="${KERNEL_DIR}/kernel.bundle"
METADATA_PATH="${KERNEL_DIR}/kernel.bundle.metadata"
VMLINUX_PATH="${KERNEL_DIR}/vmlinux"
WORK_DIR="$(mktemp -d)"
BUNDLE_STAGE=""
METADATA_STAGE=""

cleanup() {
    rm -rf -- "${WORK_DIR}"
    if [[ -n "${BUNDLE_STAGE}" ]]; then
        rm -f -- "${BUNDLE_STAGE}"
    fi
    if [[ -n "${METADATA_STAGE}" ]]; then
        rm -f -- "${METADATA_STAGE}"
    fi
}
trap cleanup EXIT

sha256_file() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum -- "$1" | awk '{print $1}'
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 -- "$1" | awk '{print $1}'
    else
        echo "sha256sum or shasum is required" >&2
        return 1
    fi
}

if [[ -e "${VMLINUX_PATH}" || -L "${VMLINUX_PATH}" ]]; then
    echo "Refusing to create a raw bundle while kernel/vmlinux exists." >&2
    echo "Move the ELF file out of ${KERNEL_DIR} before running this extractor." >&2
    exit 1
fi

mkdir -p -- "${KERNEL_DIR}"
ARCHIVE_PATH="${WORK_DIR}/${ARCHIVE_NAME}"
LIBRARY_PATH="${WORK_DIR}/libkrunfw.so.5.5.0"
RAW_OUTPUT="${WORK_DIR}/kernel.bundle"
BASE_METADATA="${WORK_DIR}/kernel.bundle.metadata.base"
FINAL_METADATA="${WORK_DIR}/kernel.bundle.metadata"
EXTRACTOR="${WORK_DIR}/extract_kernel"

echo "==> Downloading official libkrunfw v${LIBKRUNFW_VERSION} archive"
echo "    ${RELEASE_URL}"
curl --proto '=https' --tlsv1.2 --fail --location --retry 3 \
    --output "${ARCHIVE_PATH}" "${RELEASE_URL}"

ACTUAL_ARCHIVE_SHA256="$(sha256_file "${ARCHIVE_PATH}")"
if [[ "${ACTUAL_ARCHIVE_SHA256}" != "${ARCHIVE_SHA256}" ]]; then
    echo "Archive SHA-256 mismatch." >&2
    echo "Expected: ${ARCHIVE_SHA256}" >&2
    echo "Actual:   ${ACTUAL_ARCHIVE_SHA256}" >&2
    exit 1
fi
echo "==> Archive SHA-256 verified: ${ACTUAL_ARCHIVE_SHA256}"

tar -xOzf "${ARCHIVE_PATH}" "${LIBRARY_MEMBER}" > "${LIBRARY_PATH}"
if [[ ! -s "${LIBRARY_PATH}" ]]; then
    echo "Pinned library member is missing or empty: ${LIBRARY_MEMBER}" >&2
    exit 1
fi
ACTUAL_LIBRARY_SHA256="$(sha256_file "${LIBRARY_PATH}")"
if [[ "${ACTUAL_LIBRARY_SHA256}" != "${LIBRARY_SHA256}" ]]; then
    echo "Extracted library SHA-256 mismatch." >&2
    echo "Expected: ${LIBRARY_SHA256}" >&2
    echo "Actual:   ${ACTUAL_LIBRARY_SHA256}" >&2
    exit 1
fi
echo "==> Library SHA-256 verified: ${ACTUAL_LIBRARY_SHA256}"

echo "==> Compiling extractor"
cc -std=c11 -Wall -Wextra -Werror -o "${EXTRACTOR}" \
    "${SCRIPT_DIR}/extract_kernel.c" -ldl

echo "==> Extracting raw prepared bundle and load/entry metadata"
"${EXTRACTOR}" "${LIBRARY_PATH}" "${RAW_OUTPUT}" "${BASE_METADATA}"
RAW_SHA256="$(sha256_file "${RAW_OUTPUT}")"
if [[ "${RAW_SHA256}" != "${RAW_BUNDLE_SHA256}" ]]; then
    echo "Extracted raw bundle SHA-256 mismatch." >&2
    echo "Expected: ${RAW_BUNDLE_SHA256}" >&2
    echo "Actual:   ${RAW_SHA256}" >&2
    exit 1
fi
if ! grep -Fqx "guest_load_addr=${RAW_BUNDLE_GUEST_LOAD_ADDR}" "${BASE_METADATA}" ||
   ! grep -Fqx "entry_addr=${RAW_BUNDLE_ENTRY_ADDR}" "${BASE_METADATA}" ||
   ! grep -Fqx "bundle_size=${RAW_BUNDLE_SIZE}" "${BASE_METADATA}"; then
    echo "Extracted raw bundle load/entry/size metadata does not match pinned v5.5.0." >&2
    cat "${BASE_METADATA}" >&2
    exit 1
fi
{
    cat "${BASE_METADATA}"
    printf 'bundle_sha256=%s\n' "${RAW_SHA256}"
    printf 'source_url=%s\n' "${RELEASE_URL}"
    printf 'source_archive_sha256=%s\n' "${ARCHIVE_SHA256}"
    printf 'source_library_member=%s\n' "${LIBRARY_MEMBER}"
    printf 'source_library_sha256=%s\n' "${LIBRARY_SHA256}"
} > "${FINAL_METADATA}"

# Stage in the destination directory so each final rename is atomic.
BUNDLE_STAGE="$(mktemp "${KERNEL_DIR}/.kernel.bundle.XXXXXX")"
METADATA_STAGE="$(mktemp "${KERNEL_DIR}/.kernel.bundle.metadata.XXXXXX")"
cp -- "${RAW_OUTPUT}" "${BUNDLE_STAGE}"
cp -- "${FINAL_METADATA}" "${METADATA_STAGE}"
chmod 0644 "${BUNDLE_STAGE}" "${METADATA_STAGE}"
mv -f -- "${BUNDLE_STAGE}" "${BUNDLE_PATH}"
BUNDLE_STAGE=""
mv -f -- "${METADATA_STAGE}" "${METADATA_PATH}"
METADATA_STAGE=""

echo
echo "==> Generated validated raw kernel input"
ls -lh -- "${BUNDLE_PATH}" "${METADATA_PATH}"
echo
cat "${METADATA_PATH}"
echo
echo "Build on Windows with:"
echo "  cargo build --release -p libkrunfw-windows --target x86_64-pc-windows-msvc"

#!/usr/bin/env bash
# extract_kernel.sh
#
# Downloads the upstream libkrunfw release, calls krunfw_get_kernel() to get
# the embedded Linux kernel image and its load addresses, then writes:
#   kernel/vmlinux   — raw kernel ELF bytes
#   kernel/addrs.rs  — Rust constants (KERNEL_GUEST_ADDR, KERNEL_ENTRY_ADDR)
#
# Must run on Linux or macOS (needs dlopen).
#
# Usage:
#   bash scripts/extract_kernel.sh [VERSION]
#
# Examples:
#   bash scripts/extract_kernel.sh            # uses default version below
#   bash scripts/extract_kernel.sh 5.5.0

set -euo pipefail

# ---- config ----------------------------------------------------------------
DEFAULT_VERSION="5.5.0"
LIBKRUNFW_VERSION="${1:-$DEFAULT_VERSION}"
GITHUB_REPO="containers/libkrunfw"

case "$(uname -s)" in
    Linux)  LIB_NAME="libkrunfw.so.5" ;;
    Darwin) LIB_NAME="libkrunfw.5.dylib" ;;
    *)      echo "Unsupported OS: $(uname -s)" >&2; exit 1 ;;
esac

RELEASE_URL="https://github.com/${GITHUB_REPO}/releases/download/v${LIBKRUNFW_VERSION}/${LIB_NAME}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
KRUNFW_WIN_DIR="${PROJECT_ROOT}/src/libkrunfw-win"
TMPDIR_WORK="$(mktemp -d)"
trap 'rm -rf "${TMPDIR_WORK}"' EXIT

# ---- download --------------------------------------------------------------
echo "==> Downloading libkrunfw v${LIBKRUNFW_VERSION} ..."
echo "    ${RELEASE_URL}"
curl -fsSL -o "${TMPDIR_WORK}/${LIB_NAME}" "${RELEASE_URL}"

# ---- build extractor -------------------------------------------------------
echo "==> Compiling extract_kernel.c ..."
cc -o "${TMPDIR_WORK}/extract_kernel" \
    "${SCRIPT_DIR}/extract_kernel.c" \
    -ldl

# ---- run extractor ---------------------------------------------------------
echo "==> Extracting kernel ..."
cd "${KRUNFW_WIN_DIR}"

# On Linux, the .so must be on the dynamic linker search path or given as an
# absolute path.  We always pass an absolute path so dlopen() finds it.
"${TMPDIR_WORK}/extract_kernel" "${TMPDIR_WORK}/${LIB_NAME}"

# ---- verify ----------------------------------------------------------------
echo ""
echo "==> Verification:"
ls -lh "${KRUNFW_WIN_DIR}/kernel/vmlinux"
echo ""
cat "${KRUNFW_WIN_DIR}/kernel/addrs.rs"
echo ""
echo "Done.  You can now build the DLL on a Windows machine:"
echo "  cd src/libkrunfw-win"
echo "  cargo build --release --target x86_64-pc-windows-msvc"

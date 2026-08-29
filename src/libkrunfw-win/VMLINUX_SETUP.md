# Windows `vmlinux` Setup

> **Provenance warning:** this WSL-based procedure is an optional development
> recipe, not the source recipe for the current A3S Box prebuilt
> `libkrunfw.dll`. The current DLL (SHA-256
> `44f25540f58155c01258fe123617636fdc6cff27873e38e71dbc75f139602077`)
> contains the byte-identical libkrunfw v5.5.0 generic x86_64 kernel bundle:
> Linux 6.12.91, `config-libkrunfw_x86_64`, and the upstream 30-patch series.
> See `a3s-libkrun-sys/SOURCE-PROVENANCE.md`. A DLL produced with the recipe
> below is a different artifact and must receive its own source mapping,
> checksums, and WHPX validation before distribution.

This document explains how to prepare a Windows guest kernel input used by
`libkrunfw-windows`. The build accepts either:

- a Linux x86_64 ELF kernel at `src/libkrunfw-win/kernel/vmlinux`; or
- the extractor-generated raw pair
  `src/libkrunfw-win/kernel/kernel.bundle` and
  `src/libkrunfw-win/kernel/kernel.bundle.metadata`.

These generated binary inputs are intentionally not stored in normal Git
history. Provide exactly one format before building the Windows companion
library; the build rejects ambiguous or incomplete pairs.

## Reproduce the current official raw bundle

On a Linux x86_64 host, run from the repository root:

```bash
bash scripts/extract_kernel.sh
```

The script downloads the official libkrunfw v5.5.0
`libkrunfw-x86_64.tgz`, verifies the pinned archive SHA-256, extracts its
versioned `libkrunfw.so`, and calls `krunfw_get_kernel()`. That API returns a
prepared raw guest-memory bundle, not ELF bytes. The extractor therefore writes
`kernel.bundle` and records the API's guest load address, entry address, byte
length, bundle SHA-256, and pinned source identity in `kernel.bundle.metadata`.
The Rust build validates every field before embedding it; addresses are never
guessed from raw bytes.

The remainder of this guide describes the alternative custom ELF build path.

## What kernel is required

The Windows WHPX backend currently depends on an x86_64 Linux kernel that:

- supports `virtio_mmio.device=` command-line discovery
- supports the legacy x86 `_MP_` parsing path used by the current Windows boot flow
- implements NUMA policy syscalls used by OCI `linux.memoryPolicy`
- includes the required virtio drivers as built-in drivers, not loadable modules

The file must be a little-endian x86_64 ET_EXEC ELF. Its bounded PT_LOAD ranges
must not overlap, its entry point must translate through file-backed executable
bytes, and its lowest guest physical load address must be 4096-byte aligned.

The minimum config requirements enforced by `src/libkrunfw-win/build.rs` are:

- `CONFIG_VIRTIO_MMIO=y`
- `CONFIG_VIRTIO_MMIO_CMDLINE_DEVICES=y`
- `CONFIG_X86_MPPARSE=y`
- `CONFIG_NUMA=y`

The repository also provides the config fragment used for local builds:

`src/libkrunfw-win/kernel/config-wsl-libkrun-x86_64.fragment`

For an official libkrunfw v5.5.0 generic x86_64 base, use the smaller
`src/libkrunfw-win/kernel/config-libkrunfw-numa-x86_64.fragment` instead.

## Recommended source

Use the Microsoft WSL2 kernel source as the base, then apply the libkrun
fragment on top.

Repository:

`https://github.com/microsoft/WSL2-Linux-Kernel`

The exact tag does not need to match a specific release for local experiments,
but using a known WSL2 tag keeps the Hyper-V related baseline close to what the
Windows backend expects.

## Build in WSL

Run the following in a Linux environment, usually WSL Ubuntu.

### 1. Install build dependencies

```bash
sudo apt update
sudo apt install -y \
  bc \
  bison \
  build-essential \
  flex \
  git \
  libelf-dev \
  libncurses-dev \
  libssl-dev \
  pahole
```

### 2. Clone the WSL2 kernel source

```bash
cd ~
git clone https://github.com/microsoft/WSL2-Linux-Kernel.git
cd WSL2-Linux-Kernel
```

Optional: checkout this example WSL2 tag (it does not reproduce the current
distributed DLL).

```bash
git checkout linux-msft-wsl-6.6.87.2
```

### 3. Start from the Microsoft WSL config

```bash
cp Microsoft/config-wsl .config
```

### 4. Merge the libkrun config fragment

Replace `/mnt/c/path/to/libkrun` below with your actual repo path as mounted in
WSL.

```bash
scripts/kconfig/merge_config.sh \
  -m \
  .config \
  /mnt/c/path/to/libkrun/src/libkrunfw-win/kernel/config-wsl-libkrun-x86_64.fragment
make olddefconfig
```

### 5. Build `vmlinux`

```bash
make -j"$(nproc)" vmlinux
```

The resulting file is:

`~/WSL2-Linux-Kernel/vmlinux`

## Validate the kernel before copying it

### Check that it is an x86_64 ELF image

```bash
file vmlinux
readelf -h vmlinux | grep "Machine\|Entry point"
```

### Check the required config flags

If the kernel tree provides `scripts/extract-ikconfig`, use:

```bash
scripts/extract-ikconfig vmlinux | grep -E \
  'CONFIG_NUMA=|CONFIG_VIRTIO_MMIO=|CONFIG_VIRTIO_MMIO_CMDLINE_DEVICES=|CONFIG_X86_MPPARSE='
```

Expected output:

```text
CONFIG_VIRTIO_MMIO=y
CONFIG_VIRTIO_MMIO_CMDLINE_DEVICES=y
CONFIG_X86_MPPARSE=y
CONFIG_NUMA=y
```

If `extract-ikconfig` is unavailable, keep `CONFIG_IKCONFIG=y` and
`CONFIG_IKCONFIG_PROC=y` enabled in the kernel config so the Rust build can
validate the embedded config automatically. When IKCONFIG markers exist, the
build rejects corrupt gzip/UTF-8 data and decompressed configs larger than 4 MiB.

## Copy the file into the libkrun repo

From WSL:

```bash
cp vmlinux /mnt/c/path/to/libkrun/src/libkrunfw-win/kernel/vmlinux
```

Or from Windows Explorer / PowerShell, copy the built file to:

`C:\path\to\libkrun\src\libkrunfw-win\kernel\vmlinux`

## Build and verify from Windows

After the file is in place:

```powershell
cd C:\path\to\libkrun
winget install --id zig.zig --exact --version 0.16.0
powershell.exe -NoProfile -ExecutionPolicy Bypass -File scripts/build-windows-init.ps1
powershell.exe -NoProfile -ExecutionPolicy Bypass -File scripts/build-windows.ps1 -Packages libkrunfw-windows,libkrun
```

If the required kernel config is missing, `src/libkrunfw-win/build.rs` fails
early with a descriptive error.

## Common failures

### `kernel/vmlinux not found`

You did not provide either supported input. Place an ELF file at:

`src/libkrunfw-win/kernel/vmlinux`

or run `bash scripts/extract_kernel.sh` on Linux to create the validated raw
bundle pair.

### Raw bundle metadata is missing or rejected

Do not create or edit `kernel.bundle.metadata` manually. Re-run
`scripts/extract_kernel.sh`; it binds the official archive provenance and the
exported guest load/entry addresses to the exact raw bytes. A stale size,
digest, source identity, duplicate key, unknown key, or incomplete pair is
rejected deliberately.

### Missing `CONFIG_VIRTIO_MMIO_CMDLINE_DEVICES=y`

The guest boots but never discovers the virtio-mmio devices provided by the
Windows backend.

### Missing `CONFIG_X86_MPPARSE=y`

The guest does not parse the legacy `_MP_` table used by the current Windows
boot path, which breaks LAPIC/PIT based bring-up.

### Missing `CONFIG_NUMA=y`

OCI memory-policy requests fail with `ENOSYS` because the guest kernel omits
the NUMA policy syscall implementation.

### Built `bzImage` instead of `vmlinux`

The project expects the uncompressed ELF kernel image named `vmlinux`, not the
compressed boot image.

## Notes

- Do not commit `kernel/vmlinux`, `kernel/kernel.bundle`, or
  `kernel/kernel.bundle.metadata` to the normal Git history.
- Keep only the config fragment and build instructions in Git.
- If a prebuilt kernel needs to be shared later, use a release artifact,
  external download URL, or another binary distribution channel instead of a
  normal repository blob.

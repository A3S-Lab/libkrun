# Windows `vmlinux` Setup

This document explains how to prepare the Windows guest kernel file used by
`libkrunfw-windows`.

The build expects a Linux x86_64 ELF kernel at:

`src/libkrunfw-win/kernel/vmlinux`

This file is intentionally not stored in Git because it is too large for normal
repository storage. You need to build or provide it locally before building the
Windows companion library.

## What kernel is required

The Windows WHPX backend currently depends on an x86_64 Linux kernel that:

- supports `virtio_mmio.device=` command-line discovery
- supports the legacy x86 `_MP_` parsing path used by the current Windows boot flow
- includes the required virtio drivers as built-in drivers, not loadable modules

The minimum config requirements enforced by `src/libkrunfw-win/build.rs` are:

- `CONFIG_VIRTIO_MMIO=y`
- `CONFIG_VIRTIO_MMIO_CMDLINE_DEVICES=y`
- `CONFIG_X86_MPPARSE=y`

The repository also provides the config fragment used for local builds:

`src/libkrunfw-win/kernel/config-wsl-libkrun-x86_64.fragment`

## Recommended source

Use the Microsoft WSL2 kernel source as the base, then apply the libkrun
fragment on top.

Repository:

`https://github.com/microsoft/WSL2-Linux-Kernel`

The exact tag does not need to match a specific release, but using a known WSL2
tag keeps the Hyper-V related baseline close to what the Windows backend expects.

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

Optional: checkout a specific WSL2 tag.

```bash
git checkout linux-msft-wsl-6.6.87.2
```

### 3. Start from the Microsoft WSL config

```bash
cp Microsoft/config-wsl .config
```

### 4. Merge the libkrun config fragment

Replace `/mnt/d/code/libkrun` below with your actual repo path as mounted in
WSL.

```bash
scripts/kconfig/merge_config.sh \
  -m \
  .config \
  /mnt/d/code/libkrun/src/libkrunfw-win/kernel/config-wsl-libkrun-x86_64.fragment
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
  'CONFIG_VIRTIO_MMIO=|CONFIG_VIRTIO_MMIO_CMDLINE_DEVICES=|CONFIG_X86_MPPARSE='
```

Expected output:

```text
CONFIG_VIRTIO_MMIO=y
CONFIG_VIRTIO_MMIO_CMDLINE_DEVICES=y
CONFIG_X86_MPPARSE=y
```

If `extract-ikconfig` is unavailable, keep `CONFIG_IKCONFIG=y` and
`CONFIG_IKCONFIG_PROC=y` enabled in the kernel config so the Rust build can
validate the embedded config automatically.

## Copy the file into the libkrun repo

From WSL:

```bash
cp vmlinux /mnt/d/code/libkrun/src/libkrunfw-win/kernel/vmlinux
```

Or from Windows Explorer / PowerShell, copy the built file to:

`D:\code\libkrun\src\libkrunfw-win\kernel\vmlinux`

## Build and verify from Windows

After the file is in place:

```powershell
cd D:\code\libkrun
cargo build --target x86_64-pc-windows-msvc -p libkrunfw-windows
cargo build --target x86_64-pc-windows-msvc -p libkrun
```

If the required kernel config is missing, `src/libkrunfw-win/build.rs` fails
early with a descriptive error.

## Common failures

### `kernel/vmlinux not found`

You did not place the file at:

`src/libkrunfw-win/kernel/vmlinux`

### Missing `CONFIG_VIRTIO_MMIO_CMDLINE_DEVICES=y`

The guest boots but never discovers the virtio-mmio devices provided by the
Windows backend.

### Missing `CONFIG_X86_MPPARSE=y`

The guest does not parse the legacy `_MP_` table used by the current Windows
boot path, which breaks LAPIC/PIT based bring-up.

### Built `bzImage` instead of `vmlinux`

The project expects the uncompressed ELF kernel image named `vmlinux`, not the
compressed boot image.

## Notes

- Do not commit `src/libkrunfw-win/kernel/vmlinux` to the normal Git history.
- Keep only the config fragment and build instructions in Git.
- If a prebuilt kernel needs to be shared later, use a release artifact,
  external download URL, or another binary distribution channel instead of a
  normal repository blob.

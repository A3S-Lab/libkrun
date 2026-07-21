# Windows kernel bundle

> **Current prebuilt provenance:** the A3S Box `libkrunfw.dll` with SHA-256
> `44f25540f58155c01258fe123617636fdc6cff27873e38e71dbc75f139602077`
> contains a kernel bundle byte-identical to the official libkrunfw v5.5.0
> x86_64 bundle. Its corresponding source is Linux 6.12.91,
> `config-libkrunfw_x86_64`, and the upstream 30-patch series. It is **not** a
> build of the WSL 6.6 recipe below. See the outer `a3s-libkrun-sys`
> `SOURCE-PROVENANCE.md` for immutable hashes and source locations. The WSL
> instructions are an alternative development recipe; replacing the kernel
> requires new runtime validation and provenance hashes.

`libkrunfw-windows` accepts exactly one of two x86_64 kernel inputs:

- `kernel/vmlinux`: an ELF image whose `PT_LOAD` segments are parsed and
  flattened by the Windows wrapper.
- `kernel/kernel.bundle` plus `kernel/kernel.bundle.metadata`: an already
  flattened libkrunfw bundle. The build accepts this form only when the
  extractor metadata, source provenance, size, SHA-256, guest load address, and
  entry address all validate.

The raw bundle is intentionally not named `vmlinux`. The official
`krunfw_get_kernel()` API returns prepared guest-memory bytes, not the original
ELF file. Run `bash scripts/extract_kernel.sh` on Linux to download the pinned
official v5.5.0 x86_64 archive, verify its SHA-256, and generate the raw pair.

Full setup guide:

- [VMLINUX_SETUP.md](./VMLINUX_SETUP.md)

For the current Windows/WHPX backend, the guest must support
`virtio_mmio.device=` kernel command-line discovery on x86_64. The stock WSL2
kernel does not enable this, so a plain upstream `microsoft-standard-WSL2`
`vmlinux` will not boot libkrun guests far enough to discover virtio devices.

## Recommended source

As an alternative custom-kernel development recipe, use the official Microsoft
WSL2 kernel source. For example:

- Repository: `https://github.com/microsoft/WSL2-Linux-Kernel`
- Example baseline tag: `linux-msft-wsl-6.6.87.2`

This keeps the Hyper-V/WSL-specific patch set and configuration baseline, while
allowing a minimal libkrun-specific config delta.

## Required config delta

Apply [config-wsl-libkrun-x86_64.fragment](kernel/config-wsl-libkrun-x86_64.fragment).

The critical setting is:

- `CONFIG_VIRTIO_MMIO_CMDLINE_DEVICES=y`
- `CONFIG_X86_MPPARSE=y`

Without it, Linux ignores the `virtio_mmio.device=...` entries emitted by
libkrun's Windows MMIO device manager.

Without `CONFIG_X86_MPPARSE=y`, the guest kernel does not parse the legacy
`_MP_` table that libkrun exposes in low memory on Windows, so boot falls back
to "virtual wire mode with no configuration" and the LAPIC timer path never
comes up correctly.

The fragment also forces a few virtio drivers to built-in `=y` to avoid
depending on external modules in the bundled `vmlinux`.

## Example build flow

Run this in a Linux environment, typically WSL:

```bash
git clone https://github.com/microsoft/WSL2-Linux-Kernel.git
cd WSL2-Linux-Kernel
git checkout linux-msft-wsl-6.6.87.2

cp Microsoft/config-wsl .config
scripts/kconfig/merge_config.sh -m .config /path/to/libkrun/src/libkrunfw-win/kernel/config-wsl-libkrun-x86_64.fragment
make olddefconfig

make -j"$(nproc)" vmlinux
```

Then copy the resulting `vmlinux` to:

`src/libkrunfw-win/kernel/vmlinux`

For a step-by-step guide including dependency installation, validation, and
common failure modes, see [VMLINUX_SETUP.md](./VMLINUX_SETUP.md).

## Validation

Building `libkrunfw-windows` validates the selected input during the Rust build.
ELF inputs must be little-endian x86_64 ET_EXEC images with bounded,
non-overlapping PT_LOAD segments, a file-backed executable entry point, and a
4096-byte-aligned guest load address. Raw inputs fail closed unless their
extractor metadata is complete and exactly matches the bytes and pinned official
source archive. If an IKCONFIG marker is present, corrupt gzip/UTF-8 data or
more than 4 MiB of decompressed config is rejected; missing required settings
also fail early instead of producing a DLL that stalls at guest boot.

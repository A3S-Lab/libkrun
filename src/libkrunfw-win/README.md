# Windows kernel bundle

`libkrunfw-windows` embeds a single x86_64 Linux `vmlinux` at
`src/libkrunfw-win/kernel/vmlinux`.

Full setup guide:

- [VMLINUX_SETUP.md](./VMLINUX_SETUP.md)

For the current Windows/WHPX backend, the guest must support
`virtio_mmio.device=` kernel command-line discovery on x86_64. The stock WSL2
kernel does not enable this, so a plain upstream `microsoft-standard-WSL2`
`vmlinux` will not boot libkrun guests far enough to discover virtio devices.

## Recommended source

Use the official Microsoft WSL2 kernel source, matching the version already
embedded here when possible. For example:

- Repository: `https://github.com/microsoft/WSL2-Linux-Kernel`
- Matching tag for the current bundled kernel: `linux-msft-wsl-6.6.87.2`

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

Building `libkrunfw-windows` now validates the embedded kernel config during the
Rust build. If the bundled `vmlinux` is missing required settings, the build
fails early with a descriptive error instead of producing a DLL that stalls at
guest boot.

# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

libkrun is a dynamic library for running processes in a partially isolated environment using hardware virtualization (KVM on Linux, HVF on macOS ARM64, WHPX on Windows, Nitro Enclaves on AWS).

It exposes a stable C API defined in `include/libkrun.h` and integrates a VMM with minimal emulated devices for lightweight VMs.

**Version**: 1.17.5

## Build Commands

```bash
# Build with default features
make

# Build with specific features
make BLK=1 NET=1      # virtio-block and virtio-net
make GPU=1            # virtio-gpu (requires virglrenderer)
make EFI=1            # EFI variant (macOS only)

# Build with TEE support
make SEV=1            # AMD SEV
make TDX=1            # Intel TDX
make NITRO=1          # AWS Nitro Enclaves

# Install
sudo make install

# Unit tests
cargo test

# Integration tests
make test
cd tests && ./run.sh test
```

## Architecture

### Multi-crate Workspace

The main crates in `src/` are:

| Crate | Purpose |
|-------|---------|
| `libkrun` | Public C API layer |
| `vmm` | Core VMM: VM/vCPU lifecycle, memory, IRQ chip, platform backends |
| `devices` | Virtio devices (block, net, console, fs, gpu, balloon, rng, snd, vsock) |
| `arch` | Boot protocols and memory layout for x86_64, aarch64, riscv64 |
| `kernel` | Kernel image loader (ELF, raw, PeGz, bz2, gz, zstd) |
| `cpuid` | x86_64 CPUID leaf emulation |
| `polly` | Epoll/event-manager abstraction for non-blocking IO |
| `hvf` | Rust bindings to Apple Hypervisor.framework |
| `nitro` | AWS Nitro Enclave support |
| `rutabaga_gfx` | GPU virtualization (Venus Vulkan-over-virtio) |

### Key Entry Points

- **C API**: `src/libkrun/lib.rs` - exports functions from `include/libkrun.h`
- **VM creation flow**: `krun_create_ctx()` → `krun_set_vm_config()` → `krun_start_enter()`
- **Platform backends**: `src/vmm/src/platform/` - KVM, HVF, WHPX, Nitro implementations
- **Guest init**: `init/` directory - C code that runs inside the VM

### Guest VM Lifecycle

1. Host creates VM context via C API
2. Host configures kernel, rootfs, devices
3. `krun_start_enter()` starts the VM and transitions execution to guest
4. Guest runs `init/init.c` which launches the specified program

## Testing

Tests use a custom framework in `tests/test_cases/src/` where each test implements `start_vm()` (host-side) and `in_guest()` (guest-side) functions using `#[host]` and `#[guest]` macros. Register new tests in `test_cases()` function in `lib.rs`.

## Platform-Specific Notes

- **Linux**: Requires KVM kernel module; patchelf needed for install
- **macOS**: Requires macOS 14+; HVF backend for ARM64
- **Windows**: Requires WHPX and x86_64-pc-windows-msvc Rust target
- **Build dependencies**: Rust toolchain, libkrunfw companion library

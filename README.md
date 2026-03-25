<picture>
   <source media="(prefers-color-scheme: dark)" srcset="docs/images/libkrun_logo_horizontal_darkmode.png">
   <source media="(prefers-color-scheme: light)" srcset="docs/images/libkrun_logo_horizontal.png">
   <img alt="libkrun logo" src="docs/images/libkrun_logo_horizontal_200.png">
</picture>

# libkrun

```libkrun``` is a dynamic library that allows programs to easily acquire the ability to run processes in a partially isolated environment using [KVM](https://www.kernel.org/doc/Documentation/virtual/kvm/api.txt) Virtualization on Linux, [HVF](https://developer.apple.com/documentation/hypervisor) on macOS/ARM64, and [WHPX](https://learn.microsoft.com/en-us/virtualization/api/) on Windows x86_64.

It integrates a VMM (Virtual Machine Monitor, the userspace side of an Hypervisor) with the minimum amount of emulated devices required to its purpose, abstracting most of the complexity that comes from Virtual Machine management, offering users a simple C API.

## Architecture

```
┌──────────────────────────────────────────────────────────────────────────────┐
│                        Host Application  (C / Rust)                          │
│                   links against libkrun.so / libkrun.dll                     │
└────────────────────────────────┬─────────────────────────────────────────────┘
                                 │  include/libkrun.h  (stable C API)
┌────────────────────────────────▼─────────────────────────────────────────────┐
│                    src/libkrun  ·  Public C API layer                        │
│   krun_create_ctx · krun_set_vm_config · krun_set_root · krun_set_kernel     │
│   krun_add_virtiofs · krun_add_disk · krun_add_net · krun_start_enter …      │
└──────┬──────────────────┬──────────────────┬──────────────┬───────────────── ┘
       │                  │                  │              │
┌──────▼──────┐  ┌────────▼────────┐  ┌─────▼──────┐  ┌───▼──────────────────┐
│  src/vmm    │  │  src/devices    │  │  src/arch  │  │  src/kernel           │
│             │  │                 │  │            │  │                        │
│ VM & vCPU   │  │ virtio-console  │  │ x86_64     │  │ Kernel / initrd       │
│ lifecycle   │  │ virtio-block    │  │ aarch64    │  │ loader (ELF, Image,   │
│             │  │ virtio-fs       │  │ riscv64    │  │ PeGz, Bz2, Gz, Zstd) │
│ Guest memory│  │ virtio-net      │  │            │  │                        │
│ management  │  │ virtio-vsock    │  │ Boot state │  │ Kernel command-line   │
│             │  │   └─ TSI proxy  │  │ setup      │  │ builder               │
│ IRQ chip    │  │ virtio-gpu      │  │            │  └────────────────────────┘
│ (KVM IOAPIC │  │ virtio-balloon  │  │ Memory     │
│  WHPX APIC  │  │ virtio-rng      │  │ layout     │  ┌────────────────────────┐
│  HVF APIC)  │  │ virtio-snd      │  │ constants  │  │  src/cpuid             │
│             │  │                 │  │            │  │  CPUID leaf emulation  │
│ IO / MMIO   │  │ Legacy devices  │  │ configure_ │  │  and templates         │
│ bus routing │  │  serial (8250)  │  │ system()   │  └────────────────────────┘
│             │  │  i8042 keyboard │  │            │
│ vCPU event  │  │  CMOS (RTC)    │  └────────────┘  ┌────────────────────────┐
│ loop        │  │  PIT 8254       │                  │  src/smbios            │
│             │  │  PIC 8259A      │                  │  SMBIOS table builder  │
└──────┬──────┘  └────────┬────────┘                  └────────────────────────┘
       │                  │
       │         ┌────────▼────────────────────────────────────────────────────┐
       │         │  src/rutabaga_gfx  ·  GPU virtualization                   │
       │         │  Venus (Vulkan-over-virtio) and native-context backends     │
       │         │  used by virtio-gpu on Linux and macOS                      │
       │         └─────────────────────────────────────────────────────────────┘
       │
┌──────▼──────────────────────────────────────────────────────────────────────┐
│               Hypervisor / Platform Backend                                  │
│                                                                              │
│  ┌───────────────┐  ┌───────────────┐  ┌───────────────┐  ┌──────────────┐ │
│  │  KVM          │  │  HVF          │  │  WHPX         │  │  Nitro       │ │
│  │  Linux        │  │  macOS/ARM64  │  │  Windows      │  │  AWS Enclave │ │
│  │  x86_64 +     │  │  Apple M-     │  │  x86_64       │  │  (NE API)    │ │
│  │  aarch64 +    │  │  series SoC   │  │  Production   │  │              │ │
│  │  riscv64      │  │               │  │               │  │              │ │
│  └───────────────┘  └───────────────┘  └───────────────┘  └──────────────┘ │
└──────────────────────────────────────────────────────────────────────────────┘
```

### Module summary

| Crate | Role |
|-------|------|
| **libkrun** | Public C API surface. Translates C calls into `VmResources` configuration and calls `build_microvm`. |
| **vmm** | Core VMM: VM and vCPU lifecycle, guest memory allocation, IO/MMIO bus, IRQ chip abstraction, platform-specific backends (KVM, WHPX, HVF, Nitro). |
| **devices** | All virtio device implementations (console, block, fs, net, vsock/TSI, gpu, balloon, rng, snd) plus legacy PC devices (8250 serial, i8042, CMOS, PIT 8254, PIC 8259A). |
| **arch** | Architecture-specific boot protocol: x86_64 zero-page / GDT / page tables, aarch64 FDT, RISC-V devicetree; memory region layout constants. |
| **kernel** | Kernel image loader supporting raw, ELF, PeGz, ImageBz2/Gz/Zstd formats; kernel command-line builder. |
| **cpuid** | x86_64 CPUID leaf emulation and per-vCPU template application. |
| **smbios** | SMBIOS 3.0 table construction for guest firmware. |
| **polly** | Epoll/event-manager abstraction used by virtio device backends for non-blocking IO. |
| **utils** | Cross-platform utilities: `EventFd`, epoll wrappers, timestamps, byte helpers. |
| **hvf** | Thin Rust bindings to Apple Hypervisor.framework (macOS). |
| **rutabaga_gfx** | GPU virtualization via the [rutabaga](https://crates.io/crates/rutabaga_gfx) library, powering Venus (Vulkan-over-virtio) and native-context GPU acceleration. |
| **nitro** | AWS Nitro Enclave attestation and NE API integration. |

## Use cases

* [crun](https://github.com/containers/crun/blob/main/krun.1.md): Adding Virtualization-based isolation to container and confidential workloads.
* [krunkit](https://github.com/containers/krunkit): Running GPU-enabled (via [venus](https://docs.mesa3d.org/drivers/venus.html)) lightweight VMs on macOS.
* [muvm](https://github.com/AsahiLinux/muvm): Launching a microVM with GPU acceleration (via [native context](https://www.youtube.com/watch?v=9sFP_yddLLQ)) for running games that require 4k pages.

## Goals and non-goals

### Goals

* Enable other projects to easily gain KVM-based process isolation capabilities.
* Be self-sufficient (no need for calling to an external VMM) and very simple to use.
* Be as small as possible, implementing only the features required to achieve its goals.
* Have the smallest possible footprint in every aspect (RAM consumption, CPU usage and boot time).
* Be compatible with a reasonable amount of workloads.

### Non-goals

* Become a generic VMM.
* Be compatible with all kinds of workloads.

## Variants

This project provides the following variants of the library:

- **libkrun**: Generic variant compatible with all Virtualization-capable systems.
- **libkrun-sev**: Variant including support for AMD SEV (SEV, SEV-ES and SEV-SNP) memory encryption and remote attestation. Requires an SEV-capable CPU.
- **libkrun-tdx**: Variant including support for Intel TDX memory encryption. Requires a TDX-capable CPU.
- **libkrun-efi**: Variant that bundles OVMF/EDK2 for booting a distribution-provided kernel (only available on macOS).

Each variant generates a dynamic library with a different name (and ```soname```), so both can be installed at the same time in the same system.

## Virtio device support

### Linux and macOS

* virtio-console
* virtio-block
* virtio-fs
* virtio-gpu (venus and native-context)
* virtio-net
* virtio-vsock (for TSI and socket redirection)
* virtio-balloon (only free-page reporting)
* virtio-rng
* virtio-snd

### Windows (x86_64)

* virtio-console
* virtio-block
* virtio-fs (Windows passthrough, full read/write/symlink/fsync)
* virtio-net (via TcpStream backend, with checksum offload and TSO)
* virtio-vsock (TSI for TCP/UDP; Named Pipe backend for AF_UNIX; DGRAM support)
* virtio-balloon (free-page reporting and page-hinting)
* virtio-rng
* virtio-snd (NullBackend)

## Networking

In ```libkrun```, networking is provided by two different, mutually exclusive techniques: **virtio-vsock + TSI** and **virtio-net + passt/gvproxy**.

### virtio-vsock + TSI

This is a novel technique called **Transparent Socket Impersonation** which allows the VM to have network connectivity without a virtual interface. This technique supports both outgoing and incoming connections. It's possible for userspace applications running in the VM to transparently connect to endpoints outside the VM and receive connections from the outside to ports listening inside the VM.

#### Enabling TSI

TSI for AF_INET and AF_INET6 is automatically enabled when no network interface is added to the VM. TSI for AF_UNIX is enabled when, in addition to the previous condition, `krun_set_root` has been used to set `/` as root filesystem.

#### Known limitations

- Requires a custom kernel (like the one bundled in **libkrunfw**).
- It's limited to SOCK_DGRAM and SOCK_STREAM sockets and AF_INET, AF_INET6 and AF_UNIX address families (for instance, raw sockets aren't supported).
- Listening on SOCK_DGRAM sockets from the guest is not supported.
- When TSI is enabled for AF_UNIX sockets, only absolute path are supported as addresses.

### **virtio-net + passt/gvproxy**

A conventional virtual interface that allows the guest to communicate with the outside through the VMM using a supporting application like [passt](https://passt.top/passt/about/) or [gvproxy](https://github.com/containers/gvisor-tap-vsock).

#### Enabling virtio-net

Use `krun_add_net_unixstream` and/or `krun_add_net_unixdgram` to add a virtio-net interface connected to the userspace network proxy.

## Security model

The libkrun security model is primarily defined by the consideration that both the guest and the VMM pertain to the same security context. For many operations, the VMM acts as a proxy for the guest within the host. Host resources that are accessible to the VMM can potentially be accessed by the guest through it.

While defining the security implementation of your environment, you should think about the guest and the VMM as a single entity. To prevent the guest from accessing host's resources, you need to use the host's OS security features to run the VMM inside an isolated context. On Linux, the primary mechanism to be used for this purpose is namespaces. Single-user systems may have a more relaxed security policy and just ensure the VMM runs with a particular UID/GID.

While most virtio devices allow the guest to access resources from the host, two of them require special consideration when used: virtio-fs and virtio-vsock+TSI.

### virtio-fs

When exposing a directory in a filesystem from the host to the guest through virtio-fs devices configured with `krun_set_root` and/or `krun_add_virtiofs`, libkrun **does not** provide any protection against the guest attempting to access other directories in the same filesystem, or even other filesystems in the host.

A mount point isolation mechanism from the host should be used in combination with virtio-fs.

In addition, when using virtio-fs, a guest may exhaust filesystem resources such as inode limits and disk capacity. Controls should be implemented on the host to mitigate this.

### virtio-vsock + TSI

When TSI is enabled, the VMM acts as a proxy for AF_INET, AF_INET6 and AF_UNIX sockets, for both incoming and outgoing connections. For all that matters, the VMM and the guest should be considered to be running in the network context. As such, you should apply on the VMM whatever restrictions you want to apply on the guest.

## Building and installing

### Linux (generic variant)

#### Requirements

* [libkrunfw](https://github.com/containers/libkrunfw)
* A working [Rust](https://www.rust-lang.org/) toolchain
* C Library static libraries, as the [init](init/init.c) binary is statically linked (package ```glibc-static``` in Fedora)
* patchelf

#### Optional features

* **GPU=1**: Enables virtio-gpu. Requires virglrenderer-devel.
* **VIRGL_RESOURCE_MAP2=1**: Uses virgl_resource_map2 function. Requires a virglrenderer-devel patched with [1374](https://gitlab.freedesktop.org/virgl/virglrenderer/-/merge_requests/1374)
* **BLK=1**: Enables virtio-block.
* **NET=1**: Enables virtio-net.
* **SND=1**: Enables virtio-snd.

#### Compiling

```
make [FEATURE_OPTIONS]
```

#### Installing

```
sudo make [FEATURE_OPTIONS] install
```

### Linux (SEV variant)

#### Requirements

* The SEV variant of [libkrunfw](https://github.com/containers/libkrunfw), which provides a ```libkrunfw-sev.so``` library.
* A working [Rust](https://www.rust-lang.org/) toolchain
* C Library static libraries, as the [init](init/init.c) binary is statically linked (package ```glibc-static``` in Fedora)
* patchelf
* OpenSSL headers and libraries (package ```openssl-devel``` in Fedora).

#### Compiling

```
make SEV=1
```

#### Installing

```
sudo make SEV=1 install
```

### Linux (TDX variant)

#### Requirements

* The TDX variant of [libkrunfw](https://github.com/containers/libkrunfw), which provides a ```libkrunfw-tdx.so``` library.
* A working [Rust](https://www.rust-lang.org/) toolchain
* C Library static libraries, as the [init](init/init.c) binary is statically linked (package ```glibc-static``` in Fedora)
* patchelf
* OpenSSL headers and libraries (package ```openssl-devel``` in Fedora).

#### Compiling

```
make TDX=1
```

#### Installing

```
sudo make TDX=1 install
```

#### Limitations

The TDX flavor of libkrun only supports guests with 1 vCPU and memory less than or equal to 3072mib.

### macOS (EFI variant)

#### Requirements

* A working [Rust](https://www.rust-lang.org/) toolchain
* A host running macOS 14 or newer

#### Compiling

```
make EFI=1
```

#### Installing

```
sudo make EFI=1 install

```

### macOS (generic variant)

#### Requirements

* A working [Rust](https://www.rust-lang.org/) toolchain
* A host running macOS 14 or newer
* Homebrew packages `lld` and `xz`

#### Compiling

```
make [FEATURE_OPTIONS]
```

The [init](init/init.c) binary is cross-compiled using clang and lld.
A suitable sysroot is automatically generated by the Makefile from Debian repository.

#### Installing

```
sudo make [FEATURE_OPTIONS] install
```

### Windows (x86_64)

> **Status**: Production-ready. Core virtualization (WHPX), all key virtio devices,
> TSI-based vsock networking, and virtiofs are fully implemented and tested.
> Multi-vCPU support is planned for future releases.

#### Requirements

* Windows 10 version 2004 or later, or Windows 11
* **Windows Hypervisor Platform** enabled (Settings → Optional Features, or `DISM /Online /Enable-Feature /FeatureName:HypervisorPlatform`)
* A working [Rust](https://www.rust-lang.org/) toolchain with the `x86_64-pc-windows-msvc` target (`rustup target add x86_64-pc-windows-msvc`)
* MSVC build tools (Visual Studio Build Tools 2019 or later)
* A Windows-capable x86_64 ELF guest kernel at `src/libkrunfw-win/kernel/vmlinux`

#### Windows kernel setup

The Windows companion library expects a local `vmlinux` file at:

`src/libkrunfw-win/kernel/vmlinux`

That file is not stored in normal Git history because it is too large. Build it
locally from the WSL2 kernel source, then copy it into place before building
`libkrunfw-windows` / `libkrun`.

Setup guide:

- [`src/libkrunfw-win/VMLINUX_SETUP.md`](src/libkrunfw-win/VMLINUX_SETUP.md)

#### Compiling

```powershell
cargo build -p libkrunfw-windows --target x86_64-pc-windows-msvc --release
cargo build -p libkrun --target x86_64-pc-windows-msvc --release
```

#### Running tests

```powershell
# Requires Windows Hypervisor Platform; must use --test-threads=1
cargo test -p vmm --target x86_64-pc-windows-msvc --lib -- test_whpx_ --ignored --test-threads=1
```

For a minimal end-to-end `a3s-box` nginx smoke flow on Windows, see:

- [`WINDOWS_A3S_NGINX_TEST_COMMANDS.md`](WINDOWS_A3S_NGINX_TEST_COMMANDS.md)

#### API differences from Linux/macOS

| API | Windows equivalent |
|-----|--------------------|
| `krun_add_net_unixstream` | `krun_add_net_tcp` (TcpStream address or NULL for disconnected) |
| `krun_add_vsock_port` | `krun_add_vsock_port_windows` (Named Pipe name for AF_UNIX) |
| `krun_add_disk` | same (uses file-backed block device) |
| libkrunfw (bundled kernel) | Build `libkrunfw-windows` with a local `src/libkrunfw-win/kernel/vmlinux`, ship `libkrunfw.dll` next to `krun.dll` for automatic kernel loading, or call `krun_set_kernel(ctx, path, KRUN_KERNEL_FORMAT_ELF, NULL, cmdline)` explicitly |

TSI for AF_INET/AF_INET6 (TCP and UDP) is enabled automatically when no virtio-net device is added, identical to Linux/macOS behavior.

#### Known limitations

* x86_64 only (no ARM64/WHPX support on Windows)
* Single vCPU (multi-vCPU support planned)
* virtio-gpu is not supported
* virtio-snd has a NullBackend only (no audio output)

## Using the library

Despite being written in Rust, this library provides a simple C API defined in [include/libkrun.h](include/libkrun.h)

## Examples

### chroot_vm

This is a simple example providing ```chroot```-like functionality using ```libkrun```.

#### Building chroot_vm

To be able to ```chroot_vm```, you need need to build libkrun with the `virtio-block` and `virtio-net` optional features:

```
make BLK=1 NET=1
sudo make BLK=1 NET=1 install
cd examples
make
```

#### Running chroot_vm

To be able to ```chroot_vm```, you need first a directory to act as the root filesystem for your isolated program.

Use the ```rootfs``` target to get a rootfs prepared from the Fedora container image (note: you must have [podman](https://podman.io/) installed):

```
make rootfs
```

Now you can use ```chroot_vm``` to run a process within this new root filesystem:

```
./chroot_vm ./rootfs_fedora /bin/sh
```

If the ```libkrun``` and/or ```libkrunfw``` libraries were installed on a path that's not included in your ```/etc/ld.so.conf``` configuration, you may get an error like this one:

```
./chroot_vm: error while loading shared libraries: libkrun.so: cannot open shared object file: No such file or directory
```

To avoid this problem, use the ```LD_LIBRARY_PATH``` environment variable to point to the location where the libraries were installed. For example, if the libraries were installed in ```/usr/local/lib64```, use something like this:

```
LD_LIBRARY_PATH=/usr/local/lib64 ./chroot_vm rootfs_fedora/ /bin/sh
```

## Status

```libkrun``` has achieved maturity and starting version ```1.0.0``` the public API is guaranteed to be stable, following [SemVer](https://semver.org/).

## Roadmap

The items below reflect known gaps and planned improvements. They are not
binding commitments; priorities may shift based on upstream needs and
contributor availability.

### Near-term

| Item | Platform | Notes |
|------|----------|-------|
| Windows multi-vCPU | Windows | WHPX supports multiple virtual processors; libkrun currently wires a single vCPU on Windows. SMP boot protocol (INIT/SIPI) needs to be implemented. |
| libkrunfw-windows distribution | Windows | The companion library exists, but a stable binary distribution path for the Windows guest `vmlinux` is still needed so callers do not have to build and place `src/libkrunfw-win/kernel/vmlinux` manually. |
| virtio-snd real backend | Windows | The current Windows backend is a NullBackend (no audio). Wiring to Windows Audio Session API (WASAPI) is planned. |
| virtiofs: mknod / link / copy_file_range | Windows | Three syscalls are not yet implemented in the Windows passthrough driver (`fs/windows/passthrough.rs`): `mknod` (special file creation), `link` (hard links), and `copy_file_range` (in-kernel file copy). |
| virtio-net TSO on Windows | Windows | Packet segmentation for TCP Segment Offload (TSO) is not yet implemented in the Windows TcpStream backend (`net_windows.rs`). |
| ACPI table generation | All | Generating a minimal ACPI table set (RSDP/RSDT/FADT/MADT) would allow guests to use ACPI power management and CPU hotplug without relying on the legacy PIC/PIT path. |

### Medium-term

| Item | Platform | Notes |
|------|----------|-------|
| virtio-gpu on Windows | Windows | Requires porting rutabaga_gfx to Windows or integrating a WGPU-based backend. Blocked on upstream rutabaga Windows support. |
| Windows ARM64 | Windows | WHPX does not currently expose an ARM64 partition type on Windows on ARM. Tracking Microsoft's roadmap. |
| RISC-V improvements | Linux | Expand RISC-V support beyond the current single-hart proof-of-concept: SMP, AIA interrupt controller, and a broader device set. |
| SEV-SNP live migration | Linux | Encrypted live migration of SEV-SNP guests requires coordinating attestation across source and destination VMMs. |

### Long-term / exploratory

| Item | Notes |
|------|-------|
| Stable ABI versioning | The current C API is stable at the function level; a formal ABI stability guarantee with soname policies across all variants is planned. |
| Confidential containers integration | Deeper integration with OCI runtime standards for confidential workloads (SEV-SNP / TDX / Nitro). |
| Nested virtualization | Allow libkrun guests to themselves run hypervisors, gated on host KVM nested-virt support. |

## Getting in contact

The main communication channel is the [libkrun Matrix channel](https://matrix.to/#/#libkrun:matrix.org).

## Acknowledgments

```libkrun``` incorporates code from [Firecracker](https://github.com/firecracker-microvm/firecracker), [rust-vmm](https://github.com/rust-vmm/) and [Cloud-Hypervisor](https://github.com/cloud-hypervisor/).

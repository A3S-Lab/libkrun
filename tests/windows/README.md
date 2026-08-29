# Windows smoke tests

This directory contains helper scripts for Windows WHPX integration smoke tests.

## `run_whpx_smoke.ps1`

- Creates a minimal placeholder rootfs directory for smoke workflows.
- Reuses an existing rootfs when it contains `.libkrun-smoke-rootfs`.
- Rebuilds rootfs when marker format mismatches or marker age exceeds the limit.
- Exports `KRUN_WINDOWS_SMOKE_ROOTFS` for follow-up tests/scripts.
- Runs ignored WHPX smoke tests from the `vmm` crate.
- Stores command output and metadata in a log directory.
- Emits phase markers (`phases.log`) and a final status file (`summary.txt`).
- Emits `summary.json` for machine-readable CI parsing.
- Includes runner identity and git SHA in metadata/summary outputs.

The generated rootfs is a placeholder used to exercise test orchestration; it
does not prove that a Linux userspace command ran. Release qualification must
also boot a real, checksum-pinned rootfs, run a guest command, and verify a
guest-written marker on the host. A zero `krun_start_enter` result alone is not
sufficient evidence because the marker distinguishes workload completion from
an early VMM shutdown.

Usage example:

```powershell
./tests/windows/run_whpx_smoke.ps1 -Target x86_64-pc-windows-msvc -TestFilter "test_whpx_vm_"
```

Optional log output directory:

```powershell
./tests/windows/run_whpx_smoke.ps1 -LogDir "$env:TEMP\libkrun-whpx-smoke"
```

Optional pre-created rootfs directory:

```powershell
./tests/windows/run_whpx_smoke.ps1 -RootfsDir "D:\libkrun-smoke-rootfs"
```

Optional rootfs max age policy (hours):

```powershell
./tests/windows/run_whpx_smoke.ps1 -MaxRootfsAgeHours 24
```

Optional marker format/version override:

```powershell
./tests/windows/run_whpx_smoke.ps1 -RootfsMarkerFormat "libkrun-windows-smoke-rootfs-v2"
```

Optional compatible marker formats for gradual rollout:

```powershell
./tests/windows/run_whpx_smoke.ps1 -RootfsMarkerFormat "libkrun-windows-smoke-rootfs-v2" -CompatibleRootfsMarkerFormats "libkrun-windows-smoke-rootfs-v1"
```

Promote compatible markers to the primary marker format:

```powershell
./tests/windows/run_whpx_smoke.ps1 -PromoteCompatibleMarker
```

Dry-run rootfs reuse decision (no rootfs writes, no test execution):

```powershell
./tests/windows/run_whpx_smoke.ps1 -DryRunRootfsDecision
```

Fail immediately if rootfs decision is `rebuild`:

```powershell
./tests/windows/run_whpx_smoke.ps1 -DryRunRootfsDecision -FailIfRootfsRebuild
```

Optional cleanup of rootfs directory after run:

```powershell
./tests/windows/run_whpx_smoke.ps1 -CleanupRootfs
```

## Test inventory

Tests in `src/vmm/src/windows/vstate.rs` are split into two categories:

### Regular tests (run on every PR, no WHPX required)

These run automatically in the `windows-build-and-tests` CI job on `windows-latest`:

| Test | What it validates |
|------|-------------------|
| `test_elf_loader_smoke` | ELF64 load via `linux_loader::Elf::load` on a 4 MiB `GuestMemoryMmap` |
| `test_whpx_blk_init_smoke` | `BlockWindows::new()`: device type, features, config-space capacity |
| `test_whpx_blk_read_smoke` | `BlockWindows` reads sector 0 via EventManager; verifies status byte + data |
| `test_whpx_net_init_smoke` | `NetWindows::new()`: device type, features, MAC / link-up in config space |
| `test_whpx_net_tx_smoke` | `NetWindows` TX: descriptor chain consumed, used ring advances to 1 |
| `test_whpx_console_init_smoke` | `Console::new()`: device type (3), VIRTIO_F_VERSION_1 feature bit |
| `test_whpx_console_tx_smoke` | `Console` TX (port 0): descriptor chain written to output, used ring advances to 1 |
| `test_whpx_stdin_reader_smoke` | `WindowsStdinInput`: empty buffer returns 0 bytes; EventFd fd is valid |

### WHPX smoke tests (`#[ignore]` — require Hyper-V/WHPX)

`test_whpx_in_process_handle_reclamation` warms up WinHvPlatform, then creates,
runs, joins, and destroys eight HLT-only VMs in the same process. It samples
`GetProcessHandleCount` after every cycle and fails if the final handle count
does not return to the warmed baseline within a two-handle runtime margin.

These require a self-hosted runner with HyperV enabled and are only run manually
via `workflow_dispatch`.  Run them with `--ignored --test-threads=1`.

### `test_whpx_vm_hlt_boot`

Validates the synchronous WHPX vCPU execution path: writes a single `HLT`
instruction at guest address `0x10000`, sets up long-mode boot state via
`configure_x86_64`, runs the vCPU synchronously, and asserts
`VcpuEmulation::Halted` is returned.

### `test_whpx_vm_threaded_boot`

Validates the **threaded VM startup path** (`start_threaded()`), which is the
production code path used by the VMM. The test:

1. Creates a WHPX partition and maps 4 MB of guest memory.
2. Writes a single `HLT` (`F4`) at the entry address.
3. Calls `start_threaded()`, which spawns the vCPU thread, internally calls
   `configure_x86_64`, then runs the vCPU loop.
4. `VcpuEmulation::Halted` causes the thread to exit with `FC_EXIT_CODE_OK`.
5. Asserts `VcpuResponse::Exited(FC_EXIT_CODE_OK)` is received within 5 s.

### `test_whpx_vm_com1_serial_boot`

Validates the **`OUT DX, AL` instruction path** used by real Linux kernels for
COM1 serial output. Port 0x3F8 (COM1) requires the DX-register form of `OUT`
because the address exceeds the 8-bit immediate limit.

Payload (9 bytes):
```
BA F8 03 00 00   mov edx, 0x3F8   ; COM1 base
B0 48            mov al, 'H'
EE               out dx, al
F4               hlt
```

A `CaptureDevice` registered at 0x3F8 (size 8) records the byte, which is then
asserted to equal `'H'`. The run must end with `Halted`.

### `test_whpx_io_port_write_smoke`

Validates the `OUT imm8, AL` instruction path (port ≤ 0xFF, immediate port):

```
B0 48   mov al, 'H'
E6 30   out 0x30, al
F4      hlt
```

A `CaptureDevice` at port 0x30 captures the byte. After the `WHvEmulatorTryIoEmulation`
fix, RIP is correctly advanced past `OUT`, so the subsequent `HLT` is reached and
the run ends with `Halted`.

### `test_whpx_minimal_kernel_boot`

Full closed-loop integration test: ELF load → `configure_system` (Linux boot
protocol zero page) → `configure_x86_64` → IO capture → HLT.

Loads a 125-byte ELF64 binary with a 5-byte `PT_LOAD` payload at `p_paddr=0x1000`:
```
B0 48   mov al, 'H'
E6 30   out 0x30, al   ; port outside COM ranges to avoid string-IO fallback
F4      hlt
```

Asserts `kernel_load == GuestAddress(0x1000)`, captured byte equals `'H'`, and
the run ends with `Halted`.

### `test_whpx_vcpu_create_smoke`

Validates that `Vcpu::new()` (including `WHvCreateVirtualProcessor`) succeeds
after a partition is set up with guest memory.

### `test_whpx_vcpu_configure_smoke`

Validates that `Vcpu::configure_x86_64()` (`WHvSetVirtualProcessorRegisters`
with full 64-bit boot register state) succeeds without crashing.

### Prerequisites

- Windows 10/11 or Windows Server 2016+ with Hyper-V and Windows Hypervisor Platform enabled:

```powershell
# Check feature status
Get-WindowsOptionalFeature -Online -FeatureName HypervisorPlatform

# Enable if not already on (requires reboot)
Enable-WindowsOptionalFeature -Online -FeatureName Microsoft-Hyper-V -All
Enable-WindowsOptionalFeature -Online -FeatureName HypervisorPlatform
```

- Rust toolchain with the MSVC target:

```powershell
rustup target add x86_64-pc-windows-msvc
winget install --id zig.zig --exact --version 0.16.0
```

### Run individual tests locally

```powershell
# Clone the revision under test
git clone https://github.com/A3S-Lab/libkrun.git
cd libkrun

# Build the real, stripped Linux init payload required by the build
powershell.exe -NoProfile -ExecutionPolicy Bypass -File scripts/build-windows-init.ps1

# Run only the HLT boot test (synchronous path)
cargo test -p vmm --target x86_64-pc-windows-msvc --lib test_whpx_vm_hlt_boot -- --ignored --test-threads=1

# Run only the threaded boot test (start_threaded production path)
cargo test -p vmm --target x86_64-pc-windows-msvc --lib test_whpx_vm_threaded_boot -- --ignored --test-threads=1
```

Expected output for the threaded boot test:

```
running 1 test
test windows::tests::test_whpx_vm_threaded_boot ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

### Run all WHPX smoke tests locally

```powershell
# All WHPX-dependent tests (requires Hyper-V)
cargo test -p vmm --target x86_64-pc-windows-msvc --lib -- test_whpx_ --ignored --test-threads=1

# All tests including non-ignored (blk/net) — no WHPX needed
cargo test -p vmm --target x86_64-pc-windows-msvc --lib -- windows::
```

> **Note:** `--test-threads=1` is required. WHPX has system-level limits on the
> number of concurrent partitions and GPA mappings; running tests in parallel
> causes `WHvMapGpaRange` failures and access violations.

Host-side debug files are opt-in. Set
`LIBKRUN_WINDOWS_DEBUG_LOG_DIR` to an explicit directory before launching the
test process when diagnostics are needed; by default libkrun writes no host
debug log files. The environment variable is read once per process.

### Run via the smoke script

```powershell
./tests/windows/run_whpx_smoke.ps1 -TestFilter "test_whpx_vm_hlt_boot"
```

Results are written to `$env:TEMP\libkrun-whpx-smoke\`:

| File | Contents |
|------|----------|
| `whpx-smoke.log` | Full `cargo test` output |
| `phases.log` | Phase timeline with timestamps |
| `summary.txt` | Key=value result summary |
| `summary.json` | Machine-readable result summary |

### Run via GitHub Actions (requires self-hosted runner)

The `windows-whpx-smoke` job in `.github/workflows/windows_ci.yml` requires a
self-hosted runner with labels `[self-hosted, windows, hyperv]`.

Register a runner on a Hyper-V capable Windows machine:

```powershell
# Generate registration token
gh api -X POST repos/A3S-Lab/libkrun/actions/runners/registration-token --jq '.token'

# Configure the runner (on the Windows machine)
./config.cmd --url https://github.com/A3S-Lab/libkrun --token <TOKEN> \
  --labels self-hosted,windows,hyperv
```

Then trigger the job:

```bash
gh workflow run windows_ci.yml \
  --ref chore/windows-ci-smoke-validation \
  -f run_whpx_smoke=true \
  -f whpx_test_filter=test_whpx_vm_hlt_boot
```

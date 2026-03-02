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

## WHPX HLT boot test

`test_whpx_vm_hlt_boot` validates the full WHPX vCPU execution path end-to-end:
writes a single `HLT` instruction at guest address `0x10000`, sets up long-mode
boot state via `configure_x86_64`, runs the vCPU, and asserts `VcpuEmulation::Halted`
is returned.

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
```

### Run the test locally

```powershell
# Clone and switch to the branch
git clone https://github.com/A3S-Lab/libkrun.git
cd libkrun
git checkout chore/windows-ci-smoke-validation

# Create the fake init required by the build
New-Item -ItemType File -Path "init/init" -Force

# Run only the HLT boot test
cargo test -p vmm --target x86_64-pc-windows-msvc --lib test_whpx_vm_hlt_boot -- --ignored
```

Expected output:

```
running 1 test
test windows::tests::test_whpx_vm_hlt_boot ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

### Run all WHPX smoke tests locally

```powershell
cargo test -p vmm --target x86_64-pc-windows-msvc --lib test_whpx_vm_ -- --ignored
```

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

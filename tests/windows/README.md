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

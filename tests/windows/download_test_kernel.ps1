<#
.SYNOPSIS
    Download a prebuilt x86_64 ELF vmlinux for the WHPX real-kernel e2e test.

.DESCRIPTION
    Fetches a Firecracker CI kernel (vmlinux ELF, ~25 MB) and sets
    TEST_VMLINUX_PATH so that cargo test can pick it up.

    Run this script once, then execute:
        cargo test -p vmm --target x86_64-pc-windows-msvc --lib `
            -- test_whpx_real_kernel_e2e --ignored --test-threads=1

.PARAMETER KernelDir
    Directory where the kernel file is stored.  Defaults to %TEMP%\libkrun-kernels.

.PARAMETER KernelVersion
    Kernel version string used to construct the download URL.

.PARAMETER Force
    Re-download even if the kernel file already exists.
#>
param(
    [string]$KernelDir     = "$env:TEMP\libkrun-kernels",
    [string]$KernelVersion = "vmlinux-5.10.225",
    [switch]$Force
)

$ErrorActionPreference = "Stop"

# ---------------------------------------------------------------------------
# Candidate download URLs (tried in order).
# ---------------------------------------------------------------------------
$Candidates = @(
    # Firecracker hello-vmlinux (verified working)
    "https://s3.amazonaws.com/spec.ccfc.min/img/hello/kernel/hello-vmlinux.bin",
    # Firecracker CI kernel bucket (us-east-1) — may require exact version path
    "https://s3.amazonaws.com/spec.ccfc.min/firecracker-ci/v1.10/x86_64/$KernelVersion"
)

# ---------------------------------------------------------------------------
# Prepare destination
# ---------------------------------------------------------------------------
New-Item -ItemType Directory -Path $KernelDir -Force | Out-Null
$dest = Join-Path $KernelDir $KernelVersion

if ((Test-Path $dest) -and -not $Force) {
    Write-Host "Kernel already present at: $dest"
    Write-Host "Use -Force to re-download."
} else {
    $downloaded = $false
    foreach ($url in $Candidates) {
        Write-Host "Trying: $url"
        try {
            Invoke-WebRequest -Uri $url -OutFile $dest -UseBasicParsing
            $downloaded = $true
            Write-Host "Downloaded to: $dest"
            break
        } catch {
            Write-Warning "  Failed: $($_.Exception.Message)"
        }
    }

    if (-not $downloaded) {
        Write-Error @"
All download attempts failed.

To run the e2e test manually, obtain an x86_64 ELF vmlinux and set:
    `$env:TEST_VMLINUX_PATH = "C:\path\to\vmlinux"

Options:
  A) Build with WSL:
       wsl -- bash -c "apt-get install -y linux-image-\$(uname -r)-dbg && \
         cp /usr/lib/debug/boot/vmlinux-\$(uname -r) /mnt/c/tmp/vmlinux"
  B) Extract from a kernel package:
       # Inside WSL: apt-get download linux-image-<ver>
       # dpkg-deb --extract linux-image-*.deb /tmp/kpkg
       # cp /tmp/kpkg/usr/lib/debug/boot/vmlinux-* /mnt/c/tmp/vmlinux
"@
    }
}

# ---------------------------------------------------------------------------
# Verify it looks like an ELF
# ---------------------------------------------------------------------------
$magic = [System.IO.File]::ReadAllBytes($dest)[0..3]
$isElf = ($magic[0] -eq 0x7F) -and ($magic[1] -eq 0x45) -and `
         ($magic[2] -eq 0x4C) -and ($magic[3] -eq 0x46)

if (-not $isElf) {
    $hex = ($magic | ForEach-Object { $_.ToString("X2") }) -join " "
    Write-Error "File at $dest does not start with ELF magic (got: $hex). Check the download URL."
}

$sizeMB = [math]::Round((Get-Item $dest).Length / 1MB, 1)
Write-Host "Verified ELF vmlinux: $dest ($sizeMB MB)"

# ---------------------------------------------------------------------------
# Export for the current session and print cargo invocation
# ---------------------------------------------------------------------------
$env:TEST_VMLINUX_PATH = $dest
Write-Host ""
Write-Host "TEST_VMLINUX_PATH set to: $dest"
Write-Host ""
Write-Host "Run the e2e test with:"
Write-Host "  cargo test -p vmm --target x86_64-pc-windows-msvc --lib ``"
Write-Host "    -- test_whpx_real_kernel_e2e --ignored --test-threads=1"

# Download Linux kernel from libkrunfw releases for testing
# This script downloads a prebuilt Linux kernel that can be used with libkrun on Windows

$ErrorActionPreference = "Stop"

$KERNEL_DIR = "C:\vms"
$KERNEL_URL = "https://github.com/containers/libkrunfw/releases/download/v5.2.1/libkrunfw-x86_64.tgz"
$KERNEL_ARCHIVE = "$KERNEL_DIR\libkrunfw.tgz"
$KERNEL_EXTRACT_DIR = "$KERNEL_DIR\libkrunfw"

Write-Host "[INFO] Creating kernel directory: $KERNEL_DIR"
New-Item -ItemType Directory -Force -Path $KERNEL_DIR | Out-Null

Write-Host "[INFO] Downloading kernel from: $KERNEL_URL"
Invoke-WebRequest -Uri $KERNEL_URL -OutFile $KERNEL_ARCHIVE

Write-Host "[INFO] Extracting kernel archive..."
# Extract using tar (available in Windows 10+)
tar -xzf $KERNEL_ARCHIVE -C $KERNEL_DIR

Write-Host "[INFO] Looking for vmlinux..."
$vmlinux = Get-ChildItem -Path $KERNEL_DIR -Recurse -Filter "vmlinux*" | Select-Object -First 1

if ($vmlinux) {
    Write-Host "[SUCCESS] Kernel found at: $($vmlinux.FullName)"
    Write-Host ""
    Write-Host "To test kernel boot, run:"
    Write-Host "  cargo run --release --example test_kernel_boot -- `"$($vmlinux.FullName)`""
} else {
    Write-Host "[ERROR] vmlinux not found in extracted archive"
    exit 1
}

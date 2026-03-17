# Direct test run
$env:RUST_LOG = "info"
Write-Host "Running test directly..." -ForegroundColor Cyan

# Change to the directory
Set-Location D:\code\libkrun

# Run directly and capture output
.\target\release\examples\test_kernel_boot.exe 2>&1 | Tee-Object -FilePath "test_direct.log"

# Test runner that mimics the successful test_30s.log run
$env:RUST_LOG = "info"
$env:PATH = "$PWD\target\release;$env:PATH"

Write-Host "=== Kernel Boot Test with RIP Tracking ===" -ForegroundColor Cyan
Write-Host "RUST_LOG=$env:RUST_LOG" -ForegroundColor Yellow
Write-Host ""

# Run cargo to ensure it's built
Write-Host "Building..." -ForegroundColor Gray
cargo build --release --example test_kernel_boot 2>&1 | Select-String -Pattern "(Compiling|Finished)"

Write-Host ""
Write-Host "Running test (will timeout after 10 seconds)..." -ForegroundColor Green
Write-Host ""

# Run the test directly
$process = Start-Process -FilePath ".\target\release\examples\test_kernel_boot.exe" `
    -NoNewWindow -PassThru -Wait -TimeoutSec 10 `
    -RedirectStandardOutput "test_new.log" `
    -RedirectStandardError "test_new_err.log"

Write-Host ""
Write-Host "Test stopped. Exit code: $($process.ExitCode)" -ForegroundColor Yellow
Write-Host ""

# Combine and show output
Write-Host "=== Output ===" -ForegroundColor Cyan
Get-Content "test_new.log" -ErrorAction SilentlyContinue
Get-Content "test_new_err.log" -ErrorAction SilentlyContinue

# Analysis
Write-Host ""
Write-Host "=== Analysis ===" -ForegroundColor Magenta

$allOutput = @()
$allOutput += Get-Content "test_new.log" -ErrorAction SilentlyContinue
$allOutput += Get-Content "test_new_err.log" -ErrorAction SilentlyContinue

$stuck = $allOutput | Select-String -Pattern "STUCK"
$exits = $allOutput | Select-String -Pattern "Exit #"

Write-Host "Total lines: $($allOutput.Count)"
Write-Host "STUCK messages: $($stuck.Count)"
Write-Host "Exit messages: $($exits.Count)"

if ($stuck.Count -gt 0) {
    Write-Host ""
    Write-Host "First 10 STUCK messages:" -ForegroundColor Red
    $stuck | Select-Object -First 10 | ForEach-Object { Write-Host $_.Line }
}

if ($exits.Count -gt 0) {
    Write-Host ""
    Write-Host "First 10 exits:" -ForegroundColor Cyan
    $exits | Select-Object -First 10 | ForEach-Object { Write-Host $_.Line }

    if ($exits.Count -gt 10) {
        Write-Host ""
        Write-Host "Last 5 exits:" -ForegroundColor Cyan
        $exits | Select-Object -Last 5 | ForEach-Object { Write-Host $_.Line }
    }
}

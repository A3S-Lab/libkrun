# Run kernel boot test with logging
$env:RUST_LOG = "info"
$timeout = 5  # seconds

Write-Host "Starting kernel boot test (${timeout}s timeout)..."
Write-Host "Logs will be saved to test_output.log"

$process = Start-Process -FilePath ".\target\release\examples\test_kernel_boot.exe" `
    -RedirectStandardOutput "test_output.log" `
    -RedirectStandardError "test_error.log" `
    -PassThru `
    -NoNewWindow

# Wait for timeout
$process | Wait-Process -Timeout $timeout -ErrorAction SilentlyContinue

# Kill if still running
if (!$process.HasExited) {
    Write-Host "Timeout reached, stopping process..."
    $process | Stop-Process -Force
}

Write-Host "`nTest completed. Checking output..."

# Show relevant output
Write-Host "`n=== Loop Address Analysis ==="
Get-Content test_output.log | Select-String "LOOP" | Select-Object -First 20

Write-Host "`n=== vCPU Progress ==="
Get-Content test_output.log | Select-String "progress" | Select-Object -First 10

Write-Host "`n=== PIT Timer ==="
Get-Content test_output.log | Select-String "PIT timer" | Select-Object -First 5

Write-Host "`n=== Full log saved to test_output.log ==="

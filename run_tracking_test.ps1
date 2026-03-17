# Run kernel boot test with tracking for 10 seconds
$env:RUST_LOG = "info"

Write-Host "Starting kernel boot test with RIP tracking..." -ForegroundColor Cyan
Write-Host "Test will run for 10 seconds, then we'll analyze the output..." -ForegroundColor Yellow
Write-Host ""

# Run the test with output to file
$output = "test_tracking_full.log"
$job = Start-Job -ScriptBlock {
    param($exePath, $logLevel)
    $env:RUST_LOG = $logLevel
    & $exePath 2>&1
} -ArgumentList (Resolve-Path ".\target\release\examples\test_kernel_boot.exe"), "info"

# Wait 10 seconds
Start-Sleep -Seconds 10

# Stop the job
Write-Host "Stopping test..." -ForegroundColor Yellow
Stop-Job $job
$result = Receive-Job $job
Remove-Job $job

# Save output
$result | Out-File -FilePath $output -Encoding UTF8

Write-Host "`nTest complete. Output saved to $output" -ForegroundColor Green
Write-Host "Total output lines: $($result.Count)" -ForegroundColor Cyan

# Show first 30 lines
Write-Host "`n=== First 30 lines ===" -ForegroundColor Cyan
$result | Select-Object -First 30

# Analyze for stuck patterns
Write-Host "`n=== STUCK Analysis ===" -ForegroundColor Magenta
$stuck = $result | Select-String -Pattern "STUCK"
if ($stuck) {
    Write-Host "Found $($stuck.Count) STUCK messages:" -ForegroundColor Red
    $stuck | Select-Object -First 10 | ForEach-Object { Write-Host $_.Line }
} else {
    Write-Host "No STUCK patterns found" -ForegroundColor Green
}

# Count exits
Write-Host "`n=== Exit Analysis ===" -ForegroundColor Magenta
$exits = $result | Select-String -Pattern "Exit #"
if ($exits) {
    Write-Host "Found $($exits.Count) exit messages" -ForegroundColor Cyan
    Write-Host "First 10 exits:"
    $exits | Select-Object -First 10 | ForEach-Object { Write-Host $_.Line }
    Write-Host "`nLast 5 exits:"
    $exits | Select-Object -Last 5 | ForEach-Object { Write-Host $_.Line }
} else {
    Write-Host "No exit messages found" -ForegroundColor Yellow
}

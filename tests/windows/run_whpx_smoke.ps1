param(
    [string]$Target = "x86_64-pc-windows-msvc",
    [string]$TestFilter = "test_whpx_vm_",
    [string]$RootfsDir = "$env:TEMP\\libkrun-rootfs-smoke",
    [string]$LogDir = "$env:TEMP\\libkrun-whpx-smoke",
    [string]$RootfsMarkerFormat = "libkrun-windows-smoke-rootfs-v1",
    [string]$CompatibleRootfsMarkerFormats = "",
    [int]$MaxRootfsAgeHours = 168,
    [switch]$DryRunRootfsDecision,
    [switch]$FailIfRootfsRebuild,
    [switch]$PromoteCompatibleMarker,
    [switch]$CleanupRootfs
)

$ErrorActionPreference = "Stop"

$gitSha = if ($env:GITHUB_SHA) { $env:GITHUB_SHA } else { "unknown" }
$runnerName = if ($env:RUNNER_NAME) { $env:RUNNER_NAME } else { "unknown" }
$runnerOs = if ($env:RUNNER_OS) { $env:RUNNER_OS } else { "unknown" }

if ($MaxRootfsAgeHours -lt 0) {
    throw "MaxRootfsAgeHours must be >= 0"
}

function Write-Marker {
    param(
        [string]$Phase,
        [string]$State,
        [string]$Details,
        [string]$PhaseLog
    )

    $line = "$(Get-Date -Format o) phase=$Phase state=$State details=$Details"
    Add-Content -Path $PhaseLog -Value $line -Encoding ASCII
    Write-Host $line
}

function New-MinimalRootfs {
    param([string]$Path)

    New-Item -ItemType Directory -Path $Path -Force | Out-Null
    foreach ($dir in @("bin", "dev", "etc", "proc", "sys", "tmp")) {
        New-Item -ItemType Directory -Path (Join-Path $Path $dir) -Force | Out-Null
    }

    $initPath = Join-Path $Path "init.cmd"
    @(
        "@echo off",
        "echo libkrun windows smoke rootfs placeholder",
        "exit /b 0"
    ) | Set-Content -Path $initPath -Encoding ASCII

    $markerPath = Join-Path $Path ".libkrun-smoke-rootfs"
    @(
        "format=$RootfsMarkerFormat",
        "created=$(Get-Date -Format o)"
    ) | Set-Content -Path $markerPath -Encoding ASCII
}

function Write-RootfsMarker {
    param(
        [string]$Path,
        [string]$Format,
        [DateTimeOffset]$Created
    )

    $markerPath = Join-Path $Path ".libkrun-smoke-rootfs"
    @(
        "format=$Format",
        "created=$($Created.ToString('o'))"
    ) | Set-Content -Path $markerPath -Encoding ASCII
}

function Read-MarkerMap {
    param([string]$MarkerPath)

    $marker = @{}
    Get-Content $MarkerPath | ForEach-Object {
        if ($_ -match "^([^=]+)=(.*)$") {
            $marker[$matches[1]] = $matches[2]
        }
    }
    return $marker
}

function Parse-CompatibleFormats {
    param(
        [string]$Primary,
        [string]$CompatibleCsv
    )

    $formats = New-Object System.Collections.Generic.List[string]
    if ($Primary) {
        $formats.Add($Primary)
    }

    if ($CompatibleCsv) {
        foreach ($value in $CompatibleCsv.Split(',')) {
            $trimmed = $value.Trim()
            if ($trimmed -and -not $formats.Contains($trimmed)) {
                $formats.Add($trimmed)
            }
        }
    }

    return $formats
}

Write-Host "Preparing minimal Windows smoke rootfs at: $RootfsDir"
New-Item -ItemType Directory -Path $LogDir -Force | Out-Null
$logPath = Join-Path $LogDir "whpx-smoke.log"
$metaPath = Join-Path $LogDir "metadata.txt"
$phaseLogPath = Join-Path $LogDir "phases.log"
$summaryPath = Join-Path $LogDir "summary.txt"

@(
    "timestamp=$(Get-Date -Format o)",
    "target=$Target",
    "test_filter=$TestFilter",
    "rootfs_dir=$RootfsDir",
    "rootfs_marker_format=$RootfsMarkerFormat",
    "compatible_rootfs_marker_formats=$CompatibleRootfsMarkerFormats",
    "max_rootfs_age_hours=$MaxRootfsAgeHours",
    "dry_run_rootfs_decision=$DryRunRootfsDecision",
    "fail_if_rootfs_rebuild=$FailIfRootfsRebuild",
    "promote_compatible_marker=$PromoteCompatibleMarker",
    "git_sha=$gitSha",
    "runner_name=$runnerName",
    "runner_os=$runnerOs"
) | Set-Content -Path $metaPath -Encoding ASCII

Set-Content -Path $phaseLogPath -Value "" -Encoding ASCII

$overallStatus = "failed"
$rootfsPrepared = $false
$rootfsReused = $false
$rootfsReuseReason = "none"
$rootfsAction = "unknown"
$markerPromoted = $false

try {
    Write-Marker -Phase "prepare_rootfs" -State "start" -Details "creating minimal rootfs" -PhaseLog $phaseLogPath
    $markerPath = Join-Path $RootfsDir ".libkrun-smoke-rootfs"
    $allowedFormats = Parse-CompatibleFormats -Primary $RootfsMarkerFormat -CompatibleCsv $CompatibleRootfsMarkerFormats

    if ((Test-Path $RootfsDir) -and (Test-Path $markerPath)) {
        $marker = Read-MarkerMap -MarkerPath $markerPath
        $formatOk = ($marker.ContainsKey("format") -and $allowedFormats.Contains($marker["format"]))
        $primaryFormat = ($marker.ContainsKey("format") -and $marker["format"] -eq $RootfsMarkerFormat)
        $ageOk = $false
        $createdForPromotion = [DateTimeOffset]::Now

        if ($marker.ContainsKey("created")) {
            try {
                $created = [DateTimeOffset]::Parse($marker["created"])
                $ageHours = ((Get-Date).ToUniversalTime() - $created.UtcDateTime).TotalHours
                $ageOk = $ageHours -le $MaxRootfsAgeHours
                $createdForPromotion = $created
                if (-not $ageOk) {
                    $rootfsReuseReason = "marker_expired"
                }
            }
            catch {
                $rootfsReuseReason = "marker_invalid_created"
            }
        }
        else {
            $rootfsReuseReason = "marker_missing_created"
        }

        if (-not $formatOk -and $rootfsReuseReason -eq "none") {
            $rootfsReuseReason = "marker_format_mismatch"
        }

        if ($formatOk -and $ageOk) {
            $rootfsReused = $true
            $rootfsAction = "reuse"
            $rootfsReuseReason = "marker_valid"
            if ((-not $primaryFormat) -and $PromoteCompatibleMarker) {
                Write-RootfsMarker -Path $RootfsDir -Format $RootfsMarkerFormat -Created $createdForPromotion
                $markerPromoted = $true
                $rootfsReuseReason = "marker_compatible_promoted"
            }
            Write-Marker -Phase "prepare_rootfs" -State "ok" -Details "reused existing rootfs" -PhaseLog $phaseLogPath
        }
        else {
            $rootfsAction = "rebuild"
            if ($DryRunRootfsDecision) {
                Write-Marker -Phase "prepare_rootfs" -State "ok" -Details "dry-run would rebuild rootfs ($rootfsReuseReason)" -PhaseLog $phaseLogPath
            }
            else {
                New-MinimalRootfs -Path $RootfsDir
                $rootfsPrepared = $true
                Write-Marker -Phase "prepare_rootfs" -State "ok" -Details "rebuilt rootfs ($rootfsReuseReason)" -PhaseLog $phaseLogPath
            }
        }
    }
    else {
        if (-not (Test-Path $RootfsDir)) {
            $rootfsReuseReason = "rootfs_missing"
        }
        else {
            $rootfsReuseReason = "marker_missing"
        }
        $rootfsAction = "rebuild"
        if ($DryRunRootfsDecision) {
            Write-Marker -Phase "prepare_rootfs" -State "ok" -Details "dry-run would rebuild rootfs ($rootfsReuseReason)" -PhaseLog $phaseLogPath
        }
        else {
            New-MinimalRootfs -Path $RootfsDir
            $rootfsPrepared = $true
            Write-Marker -Phase "prepare_rootfs" -State "ok" -Details "rootfs ready ($rootfsReuseReason)" -PhaseLog $phaseLogPath
        }
    }

    if ($FailIfRootfsRebuild -and $rootfsAction -eq "rebuild") {
        Write-Marker -Phase "policy" -State "fail" -Details "rootfs rebuild blocked by policy" -PhaseLog $phaseLogPath
        throw "Rootfs rebuild was required but FailIfRootfsRebuild is set"
    }

    if ($DryRunRootfsDecision) {
        Write-Marker -Phase "dry_run" -State "ok" -Details "decision_only rootfs_action=$rootfsAction reason=$rootfsReuseReason" -PhaseLog $phaseLogPath
        $overallStatus = "passed"
        return
    }

    $env:KRUN_WINDOWS_SMOKE_ROOTFS = $RootfsDir

    Write-Marker -Phase "run_tests" -State "start" -Details "running cargo test" -PhaseLog $phaseLogPath
    Write-Host "Running WHPX smoke tests with filter: $TestFilter"
    $output = & cargo test -p vmm --target $Target --lib $TestFilter -- --ignored 2>&1
    $output | Tee-Object -FilePath $logPath

    if ($LASTEXITCODE -ne 0) {
        Write-Marker -Phase "run_tests" -State "fail" -Details "cargo test exit code $LASTEXITCODE" -PhaseLog $phaseLogPath
        throw "WHPX smoke tests failed with exit code $LASTEXITCODE"
    }

    if (-not ($output -join "`n").Contains("test result: ok")) {
        Write-Marker -Phase "assert_result" -State "fail" -Details "missing test result marker" -PhaseLog $phaseLogPath
        throw "WHPX smoke tests did not report a successful test summary"
    }

    Write-Marker -Phase "run_tests" -State "ok" -Details "cargo test completed" -PhaseLog $phaseLogPath
    Write-Marker -Phase "assert_result" -State "ok" -Details "success marker found" -PhaseLog $phaseLogPath
    $overallStatus = "passed"
}
catch {
    Write-Marker -Phase "smoke" -State "fail" -Details $_.Exception.Message -PhaseLog $phaseLogPath
    throw
}
finally {
    if ($CleanupRootfs -and $rootfsPrepared -and (Test-Path $RootfsDir)) {
        try {
            Remove-Item -Path $RootfsDir -Recurse -Force
            Write-Marker -Phase "cleanup_rootfs" -State "ok" -Details "rootfs removed" -PhaseLog $phaseLogPath
        }
        catch {
            Write-Marker -Phase "cleanup_rootfs" -State "fail" -Details $_.Exception.Message -PhaseLog $phaseLogPath
        }
    }

    $summaryMap = @{
        status                            = "$overallStatus"
        timestamp                         = "$(Get-Date -Format o)"
        target                            = "$Target"
        test_filter                       = "$TestFilter"
        git_sha                           = "$gitSha"
        runner_name                       = "$runnerName"
        runner_os                         = "$runnerOs"
        log_path                          = "$logPath"
        phase_log_path                    = "$phaseLogPath"
        cleanup_rootfs                    = "$CleanupRootfs"
        dry_run_rootfs_decision           = "$DryRunRootfsDecision"
        fail_if_rootfs_rebuild            = "$FailIfRootfsRebuild"
        rootfs_reused                     = "$rootfsReused"
        rootfs_action                     = "$rootfsAction"
        rootfs_reuse_reason               = "$rootfsReuseReason"
        marker_promoted                   = "$markerPromoted"
        max_rootfs_age_hours              = "$MaxRootfsAgeHours"
        rootfs_marker_format              = "$RootfsMarkerFormat"
        compatible_rootfs_marker_formats  = "$CompatibleRootfsMarkerFormats"
        promote_compatible_marker         = "$PromoteCompatibleMarker"
    }

    @(
        "status=$overallStatus",
        "timestamp=$(Get-Date -Format o)",
        "target=$Target",
        "test_filter=$TestFilter",
        "git_sha=$gitSha",
        "runner_name=$runnerName",
        "runner_os=$runnerOs",
        "log_path=$logPath",
        "phase_log_path=$phaseLogPath",
        "cleanup_rootfs=$CleanupRootfs",
        "dry_run_rootfs_decision=$DryRunRootfsDecision",
        "fail_if_rootfs_rebuild=$FailIfRootfsRebuild",
        "rootfs_reused=$rootfsReused",
        "rootfs_action=$rootfsAction",
        "rootfs_reuse_reason=$rootfsReuseReason",
        "marker_promoted=$markerPromoted",
        "max_rootfs_age_hours=$MaxRootfsAgeHours",
        "rootfs_marker_format=$RootfsMarkerFormat",
        "compatible_rootfs_marker_formats=$CompatibleRootfsMarkerFormats",
        "promote_compatible_marker=$PromoteCompatibleMarker"
    ) | Set-Content -Path $summaryPath -Encoding ASCII

    $summaryJsonPath = Join-Path $LogDir "summary.json"
    $summaryMap | ConvertTo-Json | Set-Content -Path $summaryJsonPath -Encoding ASCII
}

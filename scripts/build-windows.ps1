[CmdletBinding()]
param(
    [string[]]$Packages = @("libkrun"),
    [string]$Target = "x86_64-pc-windows-msvc",
    [string]$Cargo = "cargo",
    [string]$Zig = "zig"
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$repoRoot = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot ".."))
& (Join-Path $PSScriptRoot "build-windows-init.ps1") -Zig $Zig

$cargoCommand = Get-Command $Cargo -ErrorAction Stop
$separator = [char]0x1f
$previousEncodedRustFlags = [Environment]::GetEnvironmentVariable("CARGO_ENCODED_RUSTFLAGS")
$previousRustFlags = [Environment]::GetEnvironmentVariable("RUSTFLAGS")

$rustFlags = @()
if ($previousEncodedRustFlags) {
    $rustFlags += $previousEncodedRustFlags.Split($separator, [StringSplitOptions]::RemoveEmptyEntries)
}
elseif ($previousRustFlags) {
    # Cargo itself splits RUSTFLAGS on whitespace. Preserve those same tokens
    # before switching to the encoded form, which safely supports paths with spaces.
    $rustFlags += $previousRustFlags -split '\s+' | Where-Object { $_ }
}

$rustFlags += "--remap-path-prefix=$repoRoot=libkrun"
$userProfile = [Environment]::GetFolderPath([Environment+SpecialFolder]::UserProfile)
if ($userProfile) {
    $rustFlags += "--remap-path-prefix=$userProfile=."
}
# rust-lld otherwise writes the current time into the PE/COFF header and derives
# other debug-directory bytes from it. /Brepro replaces those values with a
# content hash so clean builds in different directories are bit-for-bit equal.
$rustFlags += "-Clink-arg=/Brepro"

$normalizedPackages = @(
    foreach ($packageList in $Packages) {
        $packageList.Split(',') | ForEach-Object { $_.Trim() } | Where-Object { $_ }
    }
)
if ($normalizedPackages.Count -eq 0) {
    throw "At least one Cargo package is required."
}

$cargoArguments = @("build", "--release", "--target", $Target)
foreach ($package in $normalizedPackages) {
    $cargoArguments += @("-p", $package)
}

Push-Location $repoRoot
try {
    [Environment]::SetEnvironmentVariable(
        "CARGO_ENCODED_RUSTFLAGS",
        [string]::Join($separator, $rustFlags)
    )
    Remove-Item Env:RUSTFLAGS -ErrorAction SilentlyContinue

    & $cargoCommand.Source @cargoArguments
    if ($LASTEXITCODE -ne 0) {
        throw "Cargo Windows build failed (exit code $LASTEXITCODE)."
    }

    $artifactNames = @()
    if ($normalizedPackages -contains "libkrun") {
        $artifactNames += @("krun.dll", "krun.dll.lib")
    }
    if ($normalizedPackages -contains "libkrunfw-windows") {
        $artifactNames += "libkrunfw.dll"
    }

    $sensitivePrefixes = @(
        $repoRoot,
        $repoRoot.Replace('\', '/'),
        $userProfile,
        $(if ($userProfile) { $userProfile.Replace('\', '/') }),
        'C:\Users\',
        'C:/Users/'
    ) | Where-Object { $_ } | Select-Object -Unique

    foreach ($artifactName in $artifactNames) {
        $artifact = Join-Path $repoRoot "target\$Target\release\$artifactName"
        if (-not (Test-Path -LiteralPath $artifact -PathType Leaf)) {
            throw "Expected Windows artifact was not produced: $artifact"
        }
        $bytes = [System.IO.File]::ReadAllBytes($artifact)
        $ascii = [System.Text.Encoding]::ASCII.GetString($bytes)
        $utf16 = [System.Text.Encoding]::Unicode.GetString($bytes)
        foreach ($prefix in $sensitivePrefixes) {
            if ($ascii.IndexOf($prefix, [StringComparison]::OrdinalIgnoreCase) -ge 0 -or
                $utf16.IndexOf($prefix, [StringComparison]::OrdinalIgnoreCase) -ge 0) {
                throw "Host path leaked into release artifact ${artifactName}: $prefix"
            }
        }
        $hash = (Get-FileHash -LiteralPath $artifact -Algorithm SHA256).Hash
        Write-Host "Verified $artifact ($($bytes.Length) bytes, SHA256 $hash)"
    }
}
finally {
    Pop-Location
    [Environment]::SetEnvironmentVariable("CARGO_ENCODED_RUSTFLAGS", $previousEncodedRustFlags)
    [Environment]::SetEnvironmentVariable("RUSTFLAGS", $previousRustFlags)
}

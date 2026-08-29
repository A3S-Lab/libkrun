[CmdletBinding()]
param(
    [string[]]$Packages = @("libkrun"),
    [string]$Target = "x86_64-pc-windows-msvc",
    [string]$Cargo = "cargo",
    [string]$Zig = "zig"
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Assert-PeHasNoCodeView {
    param(
        [Parameter(Mandatory)] [byte[]]$Bytes,
        [Parameter(Mandatory)] [string]$ArtifactName
    )

    if ($Bytes.Length -lt 64 -or $Bytes[0] -ne 0x4d -or $Bytes[1] -ne 0x5a) {
        throw "Expected a PE image for ${ArtifactName}."
    }

    $peOffset = [BitConverter]::ToInt32($Bytes, 0x3c)
    if ($peOffset -lt 0 -or $peOffset + 24 -gt $Bytes.Length -or
        $Bytes[$peOffset] -ne 0x50 -or $Bytes[$peOffset + 1] -ne 0x45 -or
        $Bytes[$peOffset + 2] -ne 0 -or $Bytes[$peOffset + 3] -ne 0) {
        throw "Invalid PE header in ${ArtifactName}."
    }

    $optionalHeaderSize = [BitConverter]::ToUInt16($Bytes, $peOffset + 20)
    $optionalHeaderOffset = $peOffset + 24
    if ($optionalHeaderOffset + $optionalHeaderSize -gt $Bytes.Length) {
        throw "Truncated PE optional header in ${ArtifactName}."
    }

    $optionalHeaderMagic = [BitConverter]::ToUInt16($Bytes, $optionalHeaderOffset)
    $dataDirectoryOffset = switch ($optionalHeaderMagic) {
        0x10b { $optionalHeaderOffset + 96 }
        0x20b { $optionalHeaderOffset + 112 }
        default { throw "Unsupported PE optional-header magic in ${ArtifactName}: $optionalHeaderMagic" }
    }
    $debugDirectoryOffset = $dataDirectoryOffset + (6 * 8)
    if ($debugDirectoryOffset + 8 -gt $optionalHeaderOffset + $optionalHeaderSize) {
        throw "PE data directories are truncated in ${ArtifactName}."
    }

    $debugRva = [BitConverter]::ToUInt32($Bytes, $debugDirectoryOffset)
    $debugSize = [BitConverter]::ToUInt32($Bytes, $debugDirectoryOffset + 4)
    if ($debugRva -eq 0 -and $debugSize -eq 0) {
        return
    }
    if ($debugRva -eq 0 -or $debugSize -eq 0 -or $debugSize % 28 -ne 0) {
        throw "Malformed PE debug directory in ${ArtifactName}."
    }

    $sectionCount = [BitConverter]::ToUInt16($Bytes, $peOffset + 6)
    $sectionTableOffset = $optionalHeaderOffset + $optionalHeaderSize
    if ($sectionTableOffset + (40 * $sectionCount) -gt $Bytes.Length) {
        throw "Truncated PE section table in ${ArtifactName}."
    }

    $debugFileOffset = $null
    for ($sectionIndex = 0; $sectionIndex -lt $sectionCount; $sectionIndex++) {
        $sectionOffset = $sectionTableOffset + (40 * $sectionIndex)
        $virtualSize = [BitConverter]::ToUInt32($Bytes, $sectionOffset + 8)
        $virtualAddress = [BitConverter]::ToUInt32($Bytes, $sectionOffset + 12)
        $rawSize = [BitConverter]::ToUInt32($Bytes, $sectionOffset + 16)
        $rawOffset = [BitConverter]::ToUInt32($Bytes, $sectionOffset + 20)
        $mappedSize = [Math]::Max([uint64]$virtualSize, [uint64]$rawSize)
        if ([uint64]$debugRva -ge $virtualAddress -and
            [uint64]$debugRva + $debugSize -le [uint64]$virtualAddress + $mappedSize) {
            $candidateOffset = [uint64]$rawOffset + ([uint64]$debugRva - $virtualAddress)
            if ($candidateOffset + $debugSize -gt $Bytes.Length) {
                throw "PE debug directory points outside ${ArtifactName}."
            }
            $debugFileOffset = [int64]$candidateOffset
            break
        }
    }
    if ($null -eq $debugFileOffset) {
        throw "PE debug directory is not mapped by a section in ${ArtifactName}."
    }

    for ($entryOffset = $debugFileOffset;
         $entryOffset -lt $debugFileOffset + $debugSize;
         $entryOffset += 28) {
        $debugType = [BitConverter]::ToUInt32($Bytes, $entryOffset + 12)
        if ($debugType -eq 2) {
            throw "Release artifact ${ArtifactName} contains PDB/CodeView metadata."
        }
    }
}

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

$userProfile = [Environment]::GetFolderPath([Environment+SpecialFolder]::UserProfile)
if ($userProfile) {
    $rustFlags += "--remap-path-prefix=$userProfile=."
}
# rustc uses the last matching path remap, so the repository-specific mapping
# must follow the broader user-profile mapping.
$rustFlags += "--remap-path-prefix=$repoRoot=libkrun"
# /Brepro makes the PE/COFF timestamp content-derived. Rust's MSVC target also
# requests a linker PDB whose command stream contains the absolute working and
# temporary paths; /DEBUG:NONE omits that release-only PDB/CodeView metadata.
$rustFlags += "-Clink-arg=/Brepro"
$rustFlags += "-Clink-arg=/DEBUG:NONE"

$normalizedPackages = @(
    foreach ($packageList in $Packages) {
        $packageList.Split(',') | ForEach-Object { $_.Trim() } | Where-Object { $_ }
    }
)
if ($normalizedPackages.Count -eq 0) {
    throw "At least one Cargo package is required."
}

$artifactNames = @()
if ($normalizedPackages -contains "libkrun") {
    $artifactNames += @("krun.dll", "krun.dll.lib")
}
if ($normalizedPackages -contains "libkrunfw-windows") {
    $artifactNames += "libkrunfw.dll"
}

$pdbPaths = @(
    foreach ($artifactName in $artifactNames) {
        if ($artifactName.EndsWith(".dll", [StringComparison]::OrdinalIgnoreCase)) {
            $pdbName = [System.IO.Path]::GetFileNameWithoutExtension($artifactName) + ".pdb"
            Join-Path $repoRoot "target\$Target\release\$pdbName"
            Join-Path $repoRoot "target\$Target\release\deps\$pdbName"
        }
    }
)
foreach ($pdbPath in $pdbPaths) {
    Remove-Item -LiteralPath $pdbPath -Force -ErrorAction SilentlyContinue
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
        if ($artifactName.EndsWith(".dll", [StringComparison]::OrdinalIgnoreCase)) {
            Assert-PeHasNoCodeView -Bytes $bytes -ArtifactName $artifactName
        }
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
    foreach ($pdbPath in $pdbPaths) {
        if (Test-Path -LiteralPath $pdbPath -PathType Leaf) {
            throw "Release linker unexpectedly produced PDB metadata: $pdbPath"
        }
    }
}
finally {
    Pop-Location
    [Environment]::SetEnvironmentVariable("CARGO_ENCODED_RUSTFLAGS", $previousEncodedRustFlags)
    [Environment]::SetEnvironmentVariable("RUSTFLAGS", $previousRustFlags)
}

[CmdletBinding()]
param(
    [string]$Zig = "zig"
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$repoRoot = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot ".."))
$source = Join-Path $repoRoot "init\init.c"
$output = Join-Path $repoRoot "init\init"
$temporaryOutput = "$output.tmp-$PID"

if (-not (Test-Path -LiteralPath $source -PathType Leaf)) {
    throw "Guest init source not found: $source"
}

$zigCommand = Get-Command $Zig -ErrorAction Stop

try {
    & $zigCommand.Source cc `
        -target x86_64-linux-musl `
        -O2 `
        -static `
        -s `
        -Wall `
        -o $temporaryOutput `
        $source
    if ($LASTEXITCODE -ne 0) {
        throw "Zig failed to build the Linux guest init (exit code $LASTEXITCODE)."
    }

    $bytes = [System.IO.File]::ReadAllBytes($temporaryOutput)
    if ($bytes.Length -lt 20 -or
        $bytes[0] -ne 0x7f -or
        $bytes[1] -ne [byte][char]'E' -or
        $bytes[2] -ne [byte][char]'L' -or
        $bytes[3] -ne [byte][char]'F' -or
        $bytes[4] -ne 2 -or
        $bytes[5] -ne 1 -or
        $bytes[18] -ne 0x3e -or
        $bytes[19] -ne 0x00) {
        throw "Zig output is not a little-endian x86_64 ELF binary: $temporaryOutput"
    }

    # `-s` is mandatory: besides keeping the embedded payload small, it prevents
    # host-specific source and profile paths from leaking into release DLLs.
    $ascii = [System.Text.Encoding]::ASCII.GetString($bytes)
    $utf16 = [System.Text.Encoding]::Unicode.GetString($bytes)
    $sensitivePrefixes = @(
        $repoRoot,
        $repoRoot.Replace('\', '/'),
        [Environment]::GetFolderPath([Environment+SpecialFolder]::UserProfile),
        [Environment]::GetFolderPath([Environment+SpecialFolder]::UserProfile).Replace('\', '/'),
        'C:\Users\',
        'C:/Users/'
    ) | Where-Object { $_ } | Select-Object -Unique
    foreach ($prefix in $sensitivePrefixes) {
        if ($ascii.IndexOf($prefix, [StringComparison]::OrdinalIgnoreCase) -ge 0 -or
            $utf16.IndexOf($prefix, [StringComparison]::OrdinalIgnoreCase) -ge 0) {
            throw "Host path leaked into the stripped guest init: $prefix"
        }
    }

    Move-Item -LiteralPath $temporaryOutput -Destination $output -Force
}
finally {
    if (Test-Path -LiteralPath $temporaryOutput) {
        Remove-Item -LiteralPath $temporaryOutput -Force
    }
}

$hash = (Get-FileHash -LiteralPath $output -Algorithm SHA256).Hash
$length = (Get-Item -LiteralPath $output).Length
Write-Host "Built $output ($length bytes, SHA256 $hash)"

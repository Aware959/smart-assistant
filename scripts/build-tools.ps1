<#
.SYNOPSIS
    Build Zhiwei maintenance tools without rebuilding the app release.

.DESCRIPTION
    Builds the single zhiwei-cli maintenance executable in release mode. It is
    optional and not required for zhiwei-service to run. Use
    -InstallDir to copy them into an existing release directory.
#>
[CmdletBinding()]
param(
    [string]$InstallDir = '',
    [switch]$SkipTests
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$backend = Join-Path $root 'backend'
$tools = @('zhiwei-cli.exe')

function Assert-LastExitCode([string]$Step) {
    if ($LASTEXITCODE -ne 0) {
        throw "$Step failed (exit $LASTEXITCODE)"
    }
}

Push-Location $backend
try {
    if (-not $SkipTests) {
        & cargo fmt --all -- --check
        Assert-LastExitCode 'cargo fmt --all -- --check'
        & cargo test
        Assert-LastExitCode 'cargo test'
        & cargo check --all-targets
        Assert-LastExitCode 'cargo check --all-targets'
    }

    Write-Host '.. Building maintenance CLI' -ForegroundColor Yellow
    & cargo build --release --no-default-features --features telegram `
        --bin zhiwei-cli
    Assert-LastExitCode 'cargo build --release zhiwei-cli'
}
finally {
    Pop-Location
}

if ($InstallDir) {
    New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
    foreach ($tool in $tools) {
        Copy-Item -LiteralPath (Join-Path $backend "target\release\$tool") `
            -Destination $InstallDir -Force
    }
}

Write-Host ''
Write-Host "Tools built: $($tools -join ', ')" -ForegroundColor Green
if ($InstallDir) {
    Write-Host "Installed to: $InstallDir" -ForegroundColor Green
}

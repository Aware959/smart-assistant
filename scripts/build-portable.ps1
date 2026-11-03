<#
.SYNOPSIS
    Build a portable Zhiwei desktop release.

.DESCRIPTION
    Produces a directly runnable directory containing zhiwei-desktop.exe and
    zhiwei-service.exe. This is not an installer: existing .env and data files in
    the output directory are preserved. The database lives under data/.
#>
[CmdletBinding()]
param(
    [string]$OutputDir = 'D:\Zhiwei',
    [string]$MigrationSource = 'D:\Zhiwei',
    [switch]$SkipBuild,
    [switch]$SkipTests,
    [switch]$WithMediaGateway,
    [ValidateSet('onefile', 'onedir')]
    [string]$MediaGatewayMode = 'onefile',
    [switch]$MediaGatewayWithAsr,
    [switch]$WithTools,
    [switch]$NoShortcut
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$backend = Join-Path $root 'backend'
$desktop = Join-Path $root 'desktop'
$tauri = Join-Path $desktop 'src-tauri'
$requiredBinaries = @('zhiwei-service.exe')
$toolBinaries = @('zhiwei-cli.exe')
$binaries = if ($WithTools) { $requiredBinaries + $toolBinaries } else { $requiredBinaries }

function Assert-LastExitCode([string]$Step) {
    if ($LASTEXITCODE -ne 0) {
        throw "$Step failed (exit $LASTEXITCODE)"
    }
}

function Set-EnvValue([string]$Path, [string]$Name, [string]$Value) {
    $utf8 = [Text.UTF8Encoding]::new($false)
    $text = [IO.File]::ReadAllText($Path, $utf8)
    $pattern = "(?m)^[ \t]*$([regex]::Escape($Name))[ \t]*=.*$"
    $line = "$Name=$Value"
    if ([regex]::IsMatch($text, $pattern)) {
        $text = [regex]::Replace($text, $pattern, $line)
    } else {
        if (-not $text.EndsWith("`n")) { $text += "`n" }
        $text += "$line`n"
    }
    [IO.File]::WriteAllText($Path, $text, $utf8)
}

if (-not $SkipBuild) {
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

        Write-Host '.. Building runtime service' -ForegroundColor Yellow
        & cargo build --release --no-default-features --features telegram,ilink `
            --bin zhiwei-service
        Assert-LastExitCode 'cargo build --release'

        if ($WithTools) {
            Write-Host '.. Building maintenance CLI' -ForegroundColor Yellow
            & cargo build --release --no-default-features --features telegram `
                --bin zhiwei-cli
            Assert-LastExitCode 'cargo build --release zhiwei-cli'
        }
    }
    finally {
        Pop-Location
    }

    Push-Location $desktop
    try {
        if (-not (Test-Path -LiteralPath 'node_modules')) {
            & npm ci
            Assert-LastExitCode 'npm ci'
        }

        Write-Host '.. Building desktop frontend' -ForegroundColor Yellow
        & npm run build
        Assert-LastExitCode 'npm run build'

        Write-Host '.. Building zhiwei-desktop.exe' -ForegroundColor Yellow
        & cargo build --manifest-path (Join-Path $tauri 'Cargo.toml') `
            --release --features custom-protocol
        Assert-LastExitCode 'cargo build zhiwei-desktop'
    }
    finally {
        Pop-Location
    }

    if ($WithMediaGateway) {
        Write-Host '.. Building media-gateway.exe' -ForegroundColor Yellow
        & (Join-Path $PSScriptRoot 'build-media-gateway.ps1') `
            -OneDir:($MediaGatewayMode -eq 'onedir') `
            -WithAsr:$MediaGatewayWithAsr `
            -SkipTests:$SkipTests
        Assert-LastExitCode 'build media-gateway'
    }
}

$desktopExe = Join-Path $tauri 'target\release\zhiwei-desktop.exe'
if (-not (Test-Path -LiteralPath $desktopExe)) {
    throw "Missing desktop executable: $desktopExe"
}
foreach ($binary in $binaries) {
    $source = Join-Path $backend "target\release\$binary"
    if (-not (Test-Path -LiteralPath $source)) {
        throw "Missing build artifact: $source"
    }
}

$releaseService = Join-Path $OutputDir 'zhiwei-service.exe'
$running = Get-Process -Name 'zhiwei-service' -ErrorAction SilentlyContinue |
    Where-Object {
        $_.Path -and
        (
            ([IO.Path]::GetFullPath($_.Path) -eq [IO.Path]::GetFullPath($releaseService)) -or
            ([IO.Path]::GetFullPath($_.Path) -eq [IO.Path]::GetFullPath(
                (Join-Path $MigrationSource 'zhiwei-service.exe')
            ))
        )
    }
if ($running) {
    throw 'A release zhiwei-service.exe is running. Stop it before rebuilding or migrating.'
}

New-Item -ItemType Directory -Path $OutputDir -Force | Out-Null
Copy-Item -LiteralPath $desktopExe -Destination $OutputDir -Force
foreach ($binary in $binaries) {
    Copy-Item -LiteralPath (Join-Path $backend "target\release\$binary") `
        -Destination $OutputDir -Force
}
if ($WithMediaGateway) {
    if ($MediaGatewayMode -eq 'onedir') {
        $gatewaySource = Join-Path $root 'media-gateway\dist\media-gateway'
        if (-not (Test-Path -LiteralPath $gatewaySource)) {
            throw "Missing media-gateway build directory: $gatewaySource"
        }
        Copy-Item -LiteralPath $gatewaySource -Destination $OutputDir -Recurse -Force
    } else {
        $gatewaySource = Join-Path $root 'media-gateway\dist\media-gateway.exe'
        if (-not (Test-Path -LiteralPath $gatewaySource)) {
            throw "Missing media-gateway executable: $gatewaySource"
        }
        Copy-Item -LiteralPath $gatewaySource -Destination $OutputDir -Force
    }
}

$releaseEnv = Join-Path $OutputDir '.env'
if (-not (Test-Path -LiteralPath $releaseEnv)) {
    $sourceEnv = @(
        (Join-Path $MigrationSource '.env'),
        (Join-Path $backend '.env')
    ) | Where-Object { Test-Path -LiteralPath $_ } | Select-Object -First 1
    if (-not $sourceEnv) {
        throw 'Missing both the legacy release .env and backend/.env.'
    }
    Copy-Item -LiteralPath $sourceEnv -Destination $releaseEnv
}
$releaseDataDir = Join-Path $OutputDir 'data'
New-Item -ItemType Directory -Path $releaseDataDir -Force | Out-Null
$releaseDb = Join-Path $releaseDataDir 'smart_assistant.db'
Set-EnvValue -Path $releaseEnv -Name 'SMART_ASSISTANT_DB' `
    -Value $releaseDb.Replace('\', '/')
Set-EnvValue -Path $releaseEnv -Name 'SMART_ASSISTANT_LOG_FILE' -Value 'logs/zhiwei-service.log'
Set-EnvValue -Path $releaseEnv -Name 'SMART_ASSISTANT_WATCHDOG_FILE' -Value 'logs/zhiwei-service.heartbeat'
Set-EnvValue -Path $releaseEnv -Name 'CONTROL_ENABLED' -Value '1'
Set-EnvValue -Path $releaseEnv -Name 'CONTROL_ADDR' -Value '127.0.0.1:0'
Set-EnvValue -Path $releaseEnv -Name 'CONTROL_STATE_FILE' -Value 'logs/control.json'
Set-EnvValue -Path $releaseEnv -Name 'CONTROL_TOKEN_FILE' -Value 'logs/control.token'

if ($WithMediaGateway) {
    $gatewayEnvDest = if ($MediaGatewayMode -eq 'onedir') {
        Join-Path $OutputDir 'media-gateway\media-gateway.env'
    } else {
        Join-Path $OutputDir 'media-gateway.env'
    }
    if (-not (Test-Path -LiteralPath $gatewayEnvDest)) {
        $gatewayEnvSource = @(
            (Join-Path $root 'media-gateway\.env'),
            (Join-Path $root 'media-gateway\.env.example')
        ) | Where-Object { Test-Path -LiteralPath $_ } | Select-Object -First 1
        if (-not $gatewayEnvSource) {
            throw 'Missing media-gateway/.env and media-gateway/.env.example.'
        }
        Copy-Item -LiteralPath $gatewayEnvSource -Destination $gatewayEnvDest
    }
    $gatewayPort = 8787
    $portLine = Get-Content -LiteralPath $gatewayEnvDest -Encoding UTF8 |
        Where-Object { $_ -match '^\s*PORT\s*=\s*\d+\s*$' } |
        Select-Object -Last 1
    if ($portLine -and $portLine -match '=\s*(\d+)') {
        $gatewayPort = [int]$Matches[1]
    }
    Set-EnvValue -Path $releaseEnv -Name 'MEDIA_GATEWAY_URL' `
        -Value "http://127.0.0.1:$gatewayPort"
}

if (-not (Test-Path -LiteralPath $releaseDb)) {
    $sourceDb = @(
        (Join-Path $MigrationSource 'data\smart_assistant.db'),
        (Join-Path $MigrationSource 'smart_assistant.db'),
        (Join-Path $backend 'smart_assistant.db')
    ) | Where-Object { Test-Path -LiteralPath $_ } | Select-Object -First 1
    if ($sourceDb) {
        foreach ($suffix in @('', '-wal', '-shm')) {
            $source = "$sourceDb$suffix"
            if (Test-Path -LiteralPath $source) {
                Copy-Item -LiteralPath $source -Destination "$releaseDb$suffix"
            }
        }
    }
}

foreach ($localFile in @('sticker_catalog.json', 'ilink_session.json')) {
    $destination = Join-Path $OutputDir $localFile
    if (-not (Test-Path -LiteralPath $destination)) {
        $source = @(
            (Join-Path $MigrationSource $localFile),
            (Join-Path $backend $localFile)
        ) | Where-Object { Test-Path -LiteralPath $_ } | Select-Object -First 1
        if ($source) {
            Copy-Item -LiteralPath $source -Destination $destination
        }
    }
}

$desktopExe = Join-Path $OutputDir 'zhiwei-desktop.exe'
if (-not $NoShortcut) {
    $shortcutPath = Join-Path ([Environment]::GetFolderPath('Desktop')) 'Zhiwei.lnk'
    $shell = New-Object -ComObject WScript.Shell
    $shortcut = $shell.CreateShortcut($shortcutPath)
    $shortcut.TargetPath = $desktopExe
    $shortcut.WorkingDirectory = $OutputDir
    $shortcut.IconLocation = "$desktopExe,0"
    $shortcut.Description = '知微 Zhiwei'
    $shortcut.Save()
}

Write-Host ''
Write-Host "Portable release: $OutputDir" -ForegroundColor Green
Write-Host "Launch:           $desktopExe" -ForegroundColor Green
if (-not $NoShortcut) {
    Write-Host "Desktop shortcut: $(Join-Path ([Environment]::GetFolderPath('Desktop')) 'Zhiwei.lnk')" `
        -ForegroundColor Green
}
Write-Host 'No installer or service registration was created.' -ForegroundColor DarkGray

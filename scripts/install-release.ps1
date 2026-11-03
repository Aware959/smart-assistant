<#
.SYNOPSIS
    Build and install a standalone Telegram release for this Windows user.

.DESCRIPTION
    Installs to D:\SmartAssistant, keeps release data separate from backend/,
    copies the current .env and database on first install, and creates a desktop
    shortcut. The database lives under data/.
#>
[CmdletBinding()]
param(
    [switch]$SkipBuild,
    [switch]$SkipTests,
    [switch]$WithTools
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$backend = Join-Path $root 'backend'
$installDir = 'D:\SmartAssistant'
$desktop = [Environment]::GetFolderPath('Desktop')
$releaseTemplates = Join-Path $PSScriptRoot 'release'
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

function Get-PwshPath {
    $candidates = @()
    $storeRoot = Join-Path $env:ProgramFiles 'WindowsApps'
    $candidates += Get-ChildItem -LiteralPath $storeRoot -Directory -Force -ErrorAction SilentlyContinue |
        Where-Object { $_.Name -match '^Microsoft\.PowerShell_.+_x64__8wekyb3d8bbwe$' } |
        Sort-Object LastWriteTime -Descending |
        ForEach-Object { Join-Path $_.FullName 'pwsh.exe' }
    $candidates += @(
        (Join-Path $env:ProgramFiles 'PowerShell\7\pwsh.exe'),
        (Join-Path $env:ProgramFiles 'PowerShell\7-preview\pwsh.exe'),
        (Join-Path $env:LOCALAPPDATA 'Microsoft\WindowsApps\pwsh.exe')
    )
    $command = Get-Command pwsh.exe -All -ErrorAction SilentlyContinue |
        Where-Object { $_.Version -and $_.Version.Major -ge 7 } |
        Select-Object -First 1
    if ($command) { $candidates += $command.Source }
    foreach ($candidate in $candidates) {
        if ($candidate -and (Test-Path -LiteralPath $candidate)) {
            return $candidate
        }
    }
    throw 'PowerShell 7 (pwsh.exe) was not found.'
}

if (-not $SkipBuild) {
    Push-Location $backend
    try {
        if (-not $SkipTests) {
            Write-Host '.. Checking formatting' -ForegroundColor Yellow
            & cargo fmt --all -- --check
            Assert-LastExitCode 'cargo fmt --all -- --check'

            Write-Host '.. Running tests' -ForegroundColor Yellow
            & cargo test
            Assert-LastExitCode 'cargo test'

            Write-Host '.. Checking all targets' -ForegroundColor Yellow
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
}

foreach ($binary in $binaries) {
    $source = Join-Path $backend "target\release\$binary"
    if (-not (Test-Path -LiteralPath $source)) {
        throw "Missing build artifact: $source"
    }
}

$running = Get-Process -Name zhiwei-service -ErrorAction SilentlyContinue
if ($running) {
    throw "zhiwei-service is already running. Stop the current instance before installing."
}

New-Item -ItemType Directory -Path $installDir -Force | Out-Null
foreach ($binary in $binaries) {
    Copy-Item -LiteralPath (Join-Path $backend "target\release\$binary") `
        -Destination $installDir -Force
}
Copy-Item -LiteralPath (Join-Path $releaseTemplates 'start.ps1') -Destination $installDir -Force
Copy-Item -LiteralPath (Join-Path $releaseTemplates 'stop.ps1') -Destination $installDir -Force
Copy-Item -LiteralPath (Join-Path $releaseTemplates 'tray.ps1') -Destination $installDir -Force
Copy-Item -LiteralPath (Join-Path $releaseTemplates 'settings.ps1') -Destination $installDir -Force

$releaseEnv = Join-Path $installDir '.env'
$sourceEnv = Join-Path $backend '.env'
$releaseDataDir = Join-Path $installDir 'data'
New-Item -ItemType Directory -Path $releaseDataDir -Force | Out-Null
$releaseDb = Join-Path $releaseDataDir 'smart_assistant.db'
$dbEnvValue = $releaseDb.Replace('\', '/')

if (-not (Test-Path -LiteralPath $releaseEnv)) {
    if (-not (Test-Path -LiteralPath $sourceEnv)) {
        throw "Missing backend/.env. Configure the development instance first."
    }

    Write-Host '.. Creating release .env from backend/.env' -ForegroundColor Yellow
    Copy-Item -LiteralPath $sourceEnv -Destination $releaseEnv
}
Set-EnvValue -Path $releaseEnv -Name 'SMART_ASSISTANT_DB' -Value $dbEnvValue
Set-EnvValue -Path $releaseEnv -Name 'SMART_ASSISTANT_LOG_FILE' -Value 'logs/zhiwei-service.log'
Set-EnvValue -Path $releaseEnv -Name 'SMART_ASSISTANT_WATCHDOG_FILE' -Value 'logs/zhiwei-service.heartbeat'
Set-EnvValue -Path $releaseEnv -Name 'CONTROL_ENABLED' -Value '1'
Set-EnvValue -Path $releaseEnv -Name 'CONTROL_ADDR' -Value '127.0.0.1:0'
Set-EnvValue -Path $releaseEnv -Name 'CONTROL_STATE_FILE' -Value 'logs/control.json'
Set-EnvValue -Path $releaseEnv -Name 'CONTROL_TOKEN_FILE' -Value 'logs/control.token'

if (-not (Test-Path -LiteralPath $releaseDb)) {
    $sourceDb = Join-Path $backend 'smart_assistant.db'
    if (Test-Path -LiteralPath $sourceDb) {
        Write-Host '.. Copying current database into release data directory' -ForegroundColor Yellow
        foreach ($suffix in @('', '-wal', '-shm')) {
            $source = "$sourceDb$suffix"
            if (Test-Path -LiteralPath $source) {
                Copy-Item -LiteralPath $source -Destination "$releaseDb$suffix"
            }
        }
    }
}

foreach ($localFile in @('sticker_catalog.json', 'ilink_session.json')) {
    $source = Join-Path $backend $localFile
    $destination = Join-Path $installDir $localFile
    if ((Test-Path -LiteralPath $source) -and -not (Test-Path -LiteralPath $destination)) {
        Copy-Item -LiteralPath $source -Destination $destination
    }
}

$trayScript = Join-Path $installDir 'tray.ps1'
$shortcutPath = Join-Path $desktop 'Zhiwei.lnk'
$oldShortcutPath = Join-Path $desktop 'Smart Assistant.lnk'
$oldShortcutPaths = @(
    $oldShortcutPath,
    (Join-Path $desktop '鐭ュ井 Zhiwei.lnk'),
    (Join-Path $desktop '知微 Zhiwei.lnk')
)
foreach ($path in $oldShortcutPaths) {
    if (Test-Path -LiteralPath $path) {
        Remove-Item -LiteralPath $path -Force
    }
}
$pwshPath = Get-PwshPath
$shell = New-Object -ComObject WScript.Shell
$shortcut = $shell.CreateShortcut($shortcutPath)
$shortcut.TargetPath = $pwshPath
$shortcut.Arguments = "-Sta -NoLogo -NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File `"$trayScript`""
$shortcut.WorkingDirectory = $installDir
$shortcut.IconLocation = "$(Join-Path $installDir 'zhiwei-service.exe'),0"
$shortcut.WindowStyle = 7
$shortcut.Description = 'Start Zhiwei tray host'
$shortcut.Save()

$buildNumber = (& git -C $root rev-list --count HEAD).Trim()
$buildCommit = (& git -C $root rev-parse --short=10 HEAD).Trim()
$version = "smart-assistant v0.2.$buildNumber+$buildCommit"

Write-Host ''
Write-Host "Installed: $installDir" -ForegroundColor Green
Write-Host "Version:   $version" -ForegroundColor Green
Write-Host "Data:      $releaseDb" -ForegroundColor Green
Write-Host "PowerShell:$pwshPath" -ForegroundColor Green
Write-Host "Desktop:   $shortcutPath" -ForegroundColor Green
Write-Host 'Double-click the desktop shortcut to start the tray host.' -ForegroundColor DarkGray

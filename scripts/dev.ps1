<#
.SYNOPSIS
    Start the development backend and Vite frontend.
    Ctrl+C stops the backend processes started by this run.

.DESCRIPTION
    1. Generate a TLS certificate if it is missing.
    2. Start zhiwei-web in the background unless port 3000 is already in use.
    3. Start zhiwei-service only when Telegram/iLink is configured and the D: release
       instance is not already running.
    4. Run Vite in the foreground.
#>
[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$backend = Join-Path $root 'backend'
$frontend = Join-Path $root 'frontend'
$envFile = Join-Path $backend '.env'

# 1) Certificate
$cert = Join-Path $backend 'certs/server.pem'
$key = Join-Path $backend 'certs/server.key'
if (-not (Test-Path $cert) -or -not (Test-Path $key)) {
    Write-Host '.. TLS certificate missing; generating it' -ForegroundColor Yellow
    & (Join-Path $backend 'scripts/gen-cert.ps1')
}

# Start one backend binary in the background.
function Start-BackendBin([string]$bin, [string]$logName) {
    $log = Join-Path $root $logName
    $arguments = @(
        '-NoProfile',
        '-Command',
        "cargo run --bin $bin 2>&1 | Tee-Object -FilePath '$log'"
    )
    $proc = Start-Process -FilePath 'pwsh' -WorkingDirectory $backend -ArgumentList $arguments -WindowStyle Hidden -PassThru
    Start-Sleep -Seconds 3
    if ($proc.HasExited) {
        if (Test-Path $log) { Get-Content $log -Tail 30 | Write-Host }
        Write-Error "$bin failed to start; see $logName"
    }
    Write-Host "  -> $bin log: $log"
    return $proc
}

$procs = @()

# 2) zhiwei-web
$port3000 = Get-NetTCPConnection -LocalPort 3000 -State Listen -ErrorAction SilentlyContinue
if (-not $port3000) {
    Write-Host '.. Starting zhiwei-web https://127.0.0.1:3000 (cargo run)' -ForegroundColor Yellow
    $procs += Start-BackendBin 'zhiwei-web' '.dev-server.log'
} else {
    Write-Host '.. Port 3000 is already in use; reusing it' -ForegroundColor DarkGray
}

# 3) zhiwei-service
$needsLink = $false
if (Test-Path $envFile) {
    $envText = Get-Content -Raw $envFile
    if ($envText -match '(?im)^TELEGRAM_BOT_TOKEN\s*=\s*\S') { $needsLink = $true }
    if ($envText -match '(?im)^ILINK_ENABLED\s*=\s*(1|true)') { $needsLink = $true }
}
$releaseService = Get-Process -Name 'zhiwei-service' -ErrorAction SilentlyContinue |
    Where-Object {
        $_.Path -and
        ([IO.Path]::GetFullPath($_.Path) -eq [IO.Path]::GetFullPath('D:\Zhiwei\zhiwei-service.exe'))
    } |
    Select-Object -First 1
if ($releaseService) {
    Write-Host ".. Release zhiwei-service is running (PID $($releaseService.Id)); skipping dev zhiwei-service" -ForegroundColor DarkGray
} elseif ($needsLink) {
    Write-Host '.. Starting dev zhiwei-service' -ForegroundColor Yellow
    $procs += Start-BackendBin 'zhiwei-service' '.dev-link.log'
} else {
    Write-Host '.. Telegram/iLink is not configured; skipping zhiwei-service' -ForegroundColor DarkGray
}

# 4) Frontend
try {
    Write-Host '.. Starting frontend at http://localhost:5173' -ForegroundColor Yellow
    Push-Location $frontend
    npm run dev
}
finally {
    Pop-Location
    foreach ($p in $procs) {
        if ($p -and -not $p.HasExited) {
            Write-Host '.. Stopping backend processes started by this run' -ForegroundColor DarkGray
            Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue
        }
    }
}

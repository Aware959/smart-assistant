<#
.SYNOPSIS
    停止由 start.ps1 启动的 zhiwei-service。

.DESCRIPTION
    只按 zhiwei-service.pid 定位进程，并校验可执行文件路径，避免误杀其他进程。
#>
[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
$root = $PSScriptRoot
$exe = Join-Path $root 'zhiwei-service.exe'
$pidFile = Join-Path $root 'zhiwei-service.pid'

if (-not (Test-Path -LiteralPath $pidFile)) {
    Write-Host 'zhiwei-service.pid not found; zhiwei-service is not running' -ForegroundColor DarkGray
    return
}

$rawPid = Get-Content -LiteralPath $pidFile -ErrorAction SilentlyContinue |
    Select-Object -First 1
if ($rawPid -notmatch '^[0-9]+$') {
    Remove-Item -LiteralPath $pidFile -Force
    Write-Host 'Invalid PID file removed' -ForegroundColor Yellow
    return
}

$proc = Get-Process -Id ([int]$rawPid) -ErrorAction SilentlyContinue
if (-not $proc) {
    Remove-Item -LiteralPath $pidFile -Force
    Write-Host 'zhiwei-service is stopped; PID file removed' -ForegroundColor DarkGray
    return
}

$procPath = $proc.Path
if (-not $procPath -or ([IO.Path]::GetFullPath($procPath) -ne [IO.Path]::GetFullPath($exe))) {
    throw "PID $($proc.Id) is not this package's zhiwei-service; refusing to stop it."
}

Stop-Process -Id $proc.Id
if (-not $proc.WaitForExit(5000)) {
    Write-Host 'Graceful exit timed out; forcing stop' -ForegroundColor Yellow
    Stop-Process -Id $proc.Id -Force
}
Remove-Item -LiteralPath $pidFile -Force -ErrorAction SilentlyContinue
Write-Host "zhiwei-service stopped (PID $($proc.Id))" -ForegroundColor Green

<#
.SYNOPSIS
    跨进程冒烟：临时库起 zhiwei-service，验证它能独立拉起并完成数据库初始化。

.DESCRIPTION
    不碰真实 smart_assistant.db（SMART_ASSISTANT_DB 指向临时目录），不连 Telegram
    （TELEGRAM_ENABLED=0），不需要 PyInstaller。临时目录在退出时清理，-KeepTemp
    可保留用于排查。
#>
[CmdletBinding()]
param(
    [switch]$SkipBuild,
    [switch]$KeepTemp
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$backend = Join-Path $root 'backend'
$serviceExe = Join-Path $backend 'target\debug\zhiwei-service.exe'

if (-not $SkipBuild) {
    Push-Location $backend
    try {
        & cargo build --bin zhiwei-service
        if ($LASTEXITCODE -ne 0) { throw 'cargo build --bin zhiwei-service 失败' }
    }
    finally {
        Pop-Location
    }
}
if (-not (Test-Path -LiteralPath $serviceExe)) {
    throw "未找到 $serviceExe；请先执行 cargo build --bin zhiwei-service 或去掉 -SkipBuild"
}

$temp = Join-Path ([IO.Path]::GetTempPath()) ("zhiwei-smoke-" + [Guid]::NewGuid().ToString('N').Substring(0, 8))
New-Item -ItemType Directory -Path $temp | Out-Null
$serviceOut = Join-Path $temp 'service.out.log'
$serviceErr = Join-Path $temp 'service.err.log'

$serviceProc = $null
try {
    Write-Host '.. 启动 zhiwei-service（临时库，Telegram 关闭）' -ForegroundColor Yellow
    $env:SMART_ASSISTANT_DB = Join-Path $temp 'smoke.db'
    $env:SMART_ASSISTANT_LOG_FILE = Join-Path $temp 'service.log'
    $env:RUST_LOG = 'info'
    $env:TELEGRAM_ENABLED = '0'
    $env:CONTROL_ENABLED = '0'
    $serviceProc = Start-Process -FilePath $serviceExe `
        -WorkingDirectory $temp `
        -WindowStyle Hidden `
        -RedirectStandardOutput $serviceOut `
        -RedirectStandardError $serviceErr `
        -PassThru

    Start-Sleep -Seconds 6
    if ($serviceProc.HasExited) {
        throw "zhiwei-service 提前退出（代码 $($serviceProc.ExitCode)），见 $serviceErr"
    }
    $serviceLog = Join-Path $temp 'service.log'
    if (-not (Test-Path -LiteralPath $serviceLog) -or
        -not (Select-String -LiteralPath $serviceLog -Pattern 'database ready' -Quiet)) {
        throw "zhiwei-service 未完成数据库初始化，见 $serviceLog"
    }
    Write-Host '.. zhiwei-service 拉起并完成数据库初始化' -ForegroundColor Green
    Write-Host '冒烟通过' -ForegroundColor Green
}
finally {
    if ($serviceProc -and -not $serviceProc.HasExited) {
        Stop-Process -Id $serviceProc.Id -Force -ErrorAction SilentlyContinue
    }
    if (-not $KeepTemp -and (Test-Path -LiteralPath $temp)) {
        Remove-Item -LiteralPath $temp -Recurse -Force
    }
}

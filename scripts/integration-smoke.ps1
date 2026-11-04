<#
.SYNOPSIS
    跨进程冒烟：源码态起媒体网关 + 临时库起 zhiwei-service，验证独立拉起与健康检查。

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
$gateway = Join-Path $root 'media-gateway'
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

$venvPython = Join-Path $gateway '.venv\Scripts\python.exe'
$python = 'python'
if (Test-Path -LiteralPath $venvPython) {
    & $venvPython --version *> $null
    if ($LASTEXITCODE -eq 0) { $python = $venvPython }
}

$portProbe = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback, 0)
$portProbe.Start()
$port = $portProbe.LocalEndpoint.Port
$portProbe.Stop()

$temp = Join-Path ([IO.Path]::GetTempPath()) ("zhiwei-smoke-" + [Guid]::NewGuid().ToString('N').Substring(0, 8))
New-Item -ItemType Directory -Path $temp | Out-Null
$gatewayEnv = Join-Path $temp 'gateway.env'
[IO.File]::WriteAllText($gatewayEnv, "PORT=$port`n", [Text.UTF8Encoding]::new($false))
$gatewayOut = Join-Path $temp 'gateway.out.log'
$gatewayErr = Join-Path $temp 'gateway.err.log'
$serviceOut = Join-Path $temp 'service.out.log'
$serviceErr = Join-Path $temp 'service.err.log'

$gatewayProc = $null
$serviceProc = $null
try {
    Write-Host ".. 启动媒体网关（127.0.0.1:$port，源码态）" -ForegroundColor Yellow
    $gatewayProc = Start-Process -FilePath $python `
        -ArgumentList @('main.py', '--env', $gatewayEnv) `
        -WorkingDirectory $gateway `
        -WindowStyle Hidden `
        -RedirectStandardOutput $gatewayOut `
        -RedirectStandardError $gatewayErr `
        -PassThru

    $handler = [Net.Http.SocketsHttpHandler]::new()
    $handler.UseProxy = $false
    $client = [Net.Http.HttpClient]::new($handler)
    $client.Timeout = [TimeSpan]::FromSeconds(2)
    $healthy = $false
    foreach ($attempt in 1..20) {
        Start-Sleep -Milliseconds 500
        try {
            $health = $client.GetStringAsync("http://127.0.0.1:$port/health").GetAwaiter().GetResult() |
                ConvertFrom-Json
            if ($health.ok) {
                $healthy = $true
                break
            }
        }
        catch {
            if ($gatewayProc.HasExited) { break }
        }
    }
    if (-not $healthy) {
        throw "媒体网关健康检查未通过（端口 $port），见 $gatewayErr"
    }
    Write-Host '.. 媒体网关健康检查通过' -ForegroundColor Green

    Write-Host '.. 启动 zhiwei-service（临时库，Telegram 关闭）' -ForegroundColor Yellow
    $env:SMART_ASSISTANT_DB = Join-Path $temp 'smoke.db'
    $env:SMART_ASSISTANT_LOG_FILE = Join-Path $temp 'service.log'
    $env:RUST_LOG = 'info'
    $env:TELEGRAM_ENABLED = '0'
    $env:CONTROL_ENABLED = '0'
    $env:MEDIA_GATEWAY_URL = "http://127.0.0.1:$port"
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
    foreach ($proc in @($serviceProc, $gatewayProc)) {
        if ($proc -and -not $proc.HasExited) {
            Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue
        }
    }
    if (-not $KeepTemp -and (Test-Path -LiteralPath $temp)) {
        Remove-Item -LiteralPath $temp -Recurse -Force
    }
}

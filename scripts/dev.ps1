<#
.SYNOPSIS
    开发态一键编排：编译 zhiwei-service 调试构建，并用桌面壳拉起全套。

.DESCRIPTION
    桌面壳会自动发现 backend/target/debug 的服务，全程不需要 PyInstaller 产物。
    脚本先按开发态配置拉起 service，再启动 tauri dev（桌面壳只当 UI，
    不再自启这个进程）；配置与数据走 backend/.env.dev + backend/smart_assistant.db，
    Telegram 默认关闭以免开发态误发消息，需要时加 -Telegram 启用。
    （本机 MCP 服务器不常驻：后端在 MCP_ENABLED=1 时按需拉起 mcp-servers/ 里的服务器。）
#>
[CmdletBinding()]
param(
    [switch]$SkipBuild,
    [switch]$Telegram,
    [int]$WebPort = 3002,
    [string]$LogLevel = 'info'
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$backend = Join-Path $root 'backend'
$app = Join-Path $root 'app'
$serviceExe = Join-Path $backend 'target\debug\zhiwei-service.exe'

$devEnv = Join-Path $backend '.env.dev'
if (-not (Test-Path -LiteralPath $devEnv)) {
    throw "未找到开发态环境文件：$devEnv"
}

function Stop-RepoDevProcesses {
    # 上一轮开发实例会占住 backend/app 的调试产物，cargo 无法覆盖重建；
    # 这里只清理仓库目录内的进程，不碰已安装的正式版（D:\Zhiwei 等）。
    $repoPrefix = [IO.Path]::GetFullPath($root).TrimEnd('\') + '\'
    $victims = Get-CimInstance Win32_Process -ErrorAction SilentlyContinue |
        Where-Object {
            $path = $_.ExecutablePath
            $inRepo = $path -and $path.StartsWith($repoPrefix, [StringComparison]::OrdinalIgnoreCase)
            $inRepo
        }
    foreach ($victim in $victims) {
        Write-Host "停止上一轮开发进程：$($victim.ProcessId) $($victim.Name)" -ForegroundColor DarkGray
        Stop-Process -Id $victim.ProcessId -Force -ErrorAction SilentlyContinue
    }
    if ($victims) { Start-Sleep -Seconds 1 }
}

if (-not $SkipBuild) {
    Stop-RepoDevProcesses
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

$env:ZHIWEI_SERVICE = $serviceExe
# 选 backend/.env.dev；桌面壳与服务进程都继承这个变量，不会误读 .env.prod。
$env:ZHIWEI_ENV = 'dev'
# 运行目录 = 仓库 backend/：服务进程从**当前工作目录**按 ZHIWEI_ENV 读 .env.dev，桌面壳以
# ZHIWEI_HOME 为工作目录启动它。库 / 日志 / 控制面 / 心跳都落在 backend/，与正式版隔离。
$env:ZHIWEI_HOME = $backend
$env:TELEGRAM_ENABLED = if ($Telegram) { '1' } else { '0' }
# 开发态独占的端口与路径，避免和正式版(8787/3000)、测试版(8789/3001) 串。
$env:SMART_ASSISTANT_ADDR = "127.0.0.1:$WebPort"
# 本机可能全局设了 RUST_LOG=warn，会把启动日志全滤掉；开发态默认要看得见。
$env:RUST_LOG = $LogLevel

Write-Host "ZHIWEI_SERVICE      = $serviceExe" -ForegroundColor Yellow
Write-Host "ZHIWEI_HOME         = $backend（读 backend/.env.dev）" -ForegroundColor Yellow
Write-Host "TELEGRAM_ENABLED    = $env:TELEGRAM_ENABLED" -ForegroundColor Yellow
Write-Host "RUST_LOG            = $env:RUST_LOG" -ForegroundColor Yellow
# tauri dev 不再自启 service：这里按开发态配置先拉起，退出时一并收尾。
$serviceStdout = Join-Path $backend 'logs\zhiwei-service.stdout.log'
New-Item -ItemType Directory -Path (Split-Path -Parent $serviceStdout) -Force | Out-Null
$serviceProc = Start-Process -FilePath $serviceExe -WorkingDirectory $backend -PassThru `
    -WindowStyle Hidden -RedirectStandardOutput $serviceStdout -RedirectStandardError "$serviceStdout.err"
Start-Sleep -Seconds 2
Write-Host "开发进程            = service pid=$($serviceProc.Id)" -ForegroundColor Cyan

Push-Location $app
try {
    & npm run app:dev
    if ($LASTEXITCODE -ne 0) { throw 'npm run app:dev 失败' }
}
finally {
    Pop-Location
    if ($serviceProc -and -not $serviceProc.HasExited) {
        Stop-Process -Id $serviceProc.Id -Force -ErrorAction SilentlyContinue
    }
    Write-Host '已停止脚本拉起的 service' -ForegroundColor DarkGray
}

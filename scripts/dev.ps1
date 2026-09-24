<#
.SYNOPSIS
    在根目录一键启动前后端：后端进程后台运行，前端 vite 前台运行。
    退出（Ctrl+C）时自动清理本次启动的后端进程。

.DESCRIPTION
    1. TLS 证书缺失时自动调用 backend/scripts/gen-cert.ps1 生成；
    2. 端口 3000 空闲则后台启动 sa-web（HTTP 会话主进程，日志写入 .dev-server.log）；
       若 3000 已被占用（已有 sa-web 在跑），会直接复用；
    3. backend/.env 里配置了 Telegram token 或 ILINK_ENABLED=1 时，再后台启动
       sa-link（通道守护），避免 Web 开发场景空转一个无通道进程；
    4. 端口 5173 前台启动 vite dev。
#>
[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$backend = Join-Path $root 'backend'
$frontend = Join-Path $root 'frontend'
$envFile = Join-Path $backend '.env'

# 1) 证书
$cert = Join-Path $backend 'certs/server.pem'
$key = Join-Path $backend 'certs/server.key'
if (-not (Test-Path $cert) -or -not (Test-Path $key)) {
    Write-Host '.. 未找到 TLS 证书，正在生成 ...' -ForegroundColor Yellow
    & (Join-Path $backend 'scripts/gen-cert.ps1')
}

# 后台启动一个后端 bin；返回进程对象。
function Start-BackendBin([string]$bin, [string]$logName) {
    $log = Join-Path $root $logName
    $proc = Start-Process -FilePath 'pwsh' -WorkingDirectory $backend `
        -ArgumentList @('-NoProfile', '-Command', "cargo run --bin $bin 2>&1 | Tee-Object -FilePath '$log'") `
        -WindowStyle Hidden -PassThru
    Start-Sleep -Seconds 3
    if ($proc.HasExited) {
        if (Test-Path $log) { Get-Content $log -Tail 30 | Write-Host }
        Write-Error "$bin 启动失败，请查看 $logName"
    }
    Write-Host "  -> $bin 日志: $log"
    return $proc
}

$procs = @()

# 2) sa-web（会话主进程）
$port3000 = Get-NetTCPConnection -LocalPort 3000 -State Listen -ErrorAction SilentlyContinue
if (-not $port3000) {
    Write-Host '.. 启动 sa-web https://127.0.0.1:3000 （cargo run，首次需编译）...' -ForegroundColor Yellow
    $procs += Start-BackendBin 'sa-web' '.dev-server.log'
} else {
    Write-Host '.. 端口 3000 已有 sa-web 在运行，直接复用' -ForegroundColor DarkGray
}

# 3) sa-link（通道守护：仅当配置了 Telegram token 或开启 iLink 时才启动）
$needsLink = $false
if (Test-Path $envFile) {
    $envText = Get-Content -Raw $envFile
    if ($envText -match '(?im)^TELEGRAM_BOT_TOKEN\s*=\s*\S') { $needsLink = $true }
    if ($envText -match '(?im)^ILINK_ENABLED\s*=\s*(1|true)') { $needsLink = $true }
}
if ($needsLink) {
    Write-Host '.. 检测到 Telegram/iLink 配置，后台启动 sa-link（通道守护）...' -ForegroundColor Yellow
    $procs += Start-BackendBin 'sa-link' '.dev-link.log'
} else {
    Write-Host '.. 未配置 Telegram/iLink，跳过 sa-link（通道守护）' -ForegroundColor DarkGray
}

# 4) 前端（前台）
try {
    Write-Host '.. 启动前端 http://localhost:5173 （退出时自动停止后端）...' -ForegroundColor Yellow
    Push-Location $frontend
    npm run dev
}
finally {
    Pop-Location
    foreach ($p in $procs) {
        if ($p -and -not $p.HasExited) {
            Write-Host '.. 停止本次启动的后端' -ForegroundColor DarkGray
            Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue
        }
    }
}
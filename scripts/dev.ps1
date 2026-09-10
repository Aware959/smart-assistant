<#
.SYNOPSIS
    在根目录一键启动前后端：后端以后台进程运行（cargo run），前端 vite 前台运行。
    退出（Ctrl+C）时自动清理本次启动的后端进程。

.DESCRIPTION
    1. TLS 证书缺失时自动调用 backend/scripts/gen-cert.ps1 生成；
    2. 端口 3000 空闲则后台启动后端（日志写入根目录 .dev-server.log）；
    3. 端口 5173 前台启动 vite dev。
    若 3000 已被占用（已有后端在跑），会直接复用。
#>
[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$backend = Join-Path $root 'backend'
$frontend = Join-Path $root 'frontend'

# 1) 证书
$cert = Join-Path $backend 'certs/server.pem'
$key = Join-Path $backend 'certs/server.key'
if (-not (Test-Path $cert) -or -not (Test-Path $key)) {
    Write-Host '.. 未找到 TLS 证书，正在生成 ...' -ForegroundColor Yellow
    & (Join-Path $backend 'scripts/gen-cert.ps1')
}

# 2) 后端
$serverProc = $null
$port3000 = Get-NetTCPConnection -LocalPort 3000 -State Listen -ErrorAction SilentlyContinue
if (-not $port3000) {
    Write-Host '.. 启动后端 https://127.0.0.1:3000 （cargo run，首次需编译）...' -ForegroundColor Yellow
    $log = Join-Path $root '.dev-server.log'
    $serverProc = Start-Process -FilePath 'pwsh' -WorkingDirectory $backend `
        -ArgumentList @('-NoProfile', '-Command', "cargo run --bin smart-assistant-server 2>&1 | Tee-Object -FilePath '$log'") `
        -WindowStyle Hidden -PassThru
    Start-Sleep -Seconds 3
    if ($serverProc.HasExited) {
        if (Test-Path $log) { Get-Content $log -Tail 30 | Write-Host }
        Write-Error '后端启动失败，请查看 .dev-server.log'
    }
    Write-Host "  -> 日志: $log"
} else {
    Write-Host '.. 端口 3000 已有后端在运行，直接复用' -ForegroundColor DarkGray
}

# 3) 前端（前台）
try {
    Write-Host '.. 启动前端 http://localhost:5173 （退出时自动停止后端）...' -ForegroundColor Yellow
    Push-Location $frontend
    npm run dev
}
finally {
    Pop-Location
    if ($serverProc -and -not $serverProc.HasExited) {
        Write-Host '.. 停止本次启动的后端' -ForegroundColor DarkGray
        Stop-Process -Id $serverProc.Id -Force -ErrorAction SilentlyContinue
    }
}
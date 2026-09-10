<#
.SYNOPSIS
    停止占用 3000 / 5173 端口的进程（后端 server 与 vite dev）。
    仅按端口定位，避免误杀无关 node 进程。
#>
[CmdletBinding()]
param()

$pids = Get-NetTCPConnection -LocalPort 3000, 5173 -State Listen -ErrorAction SilentlyContinue |
    Select-Object -ExpandProperty OwningProcess -Unique

if (-not $pids) {
    Write-Host '没有检测到占用 3000 / 5173 的进程'
    return
}

foreach ($procId in $pids) {
    if ($procId -eq $PID) { continue }
    $name = (Get-Process -Id $procId -ErrorAction SilentlyContinue)?.ProcessName
    Write-Host "停止进程 $procId ($name)"
    Stop-Process -Id $procId -Force -ErrorAction SilentlyContinue
}
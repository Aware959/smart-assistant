<#
.SYNOPSIS
    停止占用 3000 / 5173 端口的进程（zhiwei-web 与 vite dev），
    并清理仓库 backend/target 下的开发版 zhiwei-service（通道守护）。
    正式版 D:\Zhiwei 不受影响。
#>
[CmdletBinding()]
param()

# 1) 按端口定位 zhiwei-web / vite
$pids = Get-NetTCPConnection -LocalPort 3000, 5173 -State Listen -ErrorAction SilentlyContinue |
    Select-Object -ExpandProperty OwningProcess -Unique

# 2) 只定位仓库 target 目录里的开发版 zhiwei-service，不碰 D:\Zhiwei
$backendTarget = Join-Path (Split-Path -Parent $PSScriptRoot) 'backend\target'
$linkPids = Get-Process -Name 'zhiwei-service' -ErrorAction SilentlyContinue |
    Where-Object {
        $_.Path -and
        ([IO.Path]::GetFullPath($_.Path).StartsWith(
            [IO.Path]::GetFullPath($backendTarget).TrimEnd('\') + '\',
            [StringComparison]::OrdinalIgnoreCase
        ))
    } |
    Select-Object -ExpandProperty ProcessId -Unique

$ids = @($pids) + @($linkPids) | Select-Object -Unique

if (-not $ids) {
    Write-Host '没有检测到 zhiwei-web / zhiwei-service / vite (3000, 5173) 进程'
    return
}

foreach ($procId in $ids) {
    if ($procId -eq $PID) { continue }
    $proc = Get-Process -Id $procId -ErrorAction SilentlyContinue
    $name = if ($proc) { $proc.ProcessName } else { '<exited>' }
    Write-Host "停止进程 $procId ($name)"
    Stop-Process -Id $procId -Force -ErrorAction SilentlyContinue
}

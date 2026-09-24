<#
.SYNOPSIS
    停止占用 3000 / 5173 端口的进程（sa-web 与 vite dev），
    并按命令行特征清理 sa-link（通道守护，无固定端口）。
    仅按端口/命令行定位，避免误杀无关进程。
#>
[CmdletBinding()]
param()

# 1) 按端口定位 sa-web / vite
$pids = Get-NetTCPConnection -LocalPort 3000, 5173 -State Listen -ErrorAction SilentlyContinue |
    Select-Object -ExpandProperty OwningProcess -Unique

# 2) 按命令行定位 sa-link（cargo run --bin sa-link 或直接运行的 sa-link.exe）
$linkPids = Get-CimInstance Win32_Process -ErrorAction SilentlyContinue |
    Where-Object { $_.CommandLine -match 'sa-link' -and $_.ProcessId -ne $PID } |
    Select-Object -ExpandProperty ProcessId -Unique

$ids = @($pids) + @($linkPids) | Select-Object -Unique

if (-not $ids) {
    Write-Host '没有检测到 sa-web / sa-link / vite (3000, 5173) 进程'
    return
}

foreach ($procId in $ids) {
    if ($procId -eq $PID) { continue }
    $name = (Get-Process -Id $procId -ErrorAction SilentlyContinue)?.ProcessName
    Write-Host "停止进程 $procId ($name)"
    Stop-Process -Id $procId -Force -ErrorAction SilentlyContinue
}
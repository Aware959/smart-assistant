<#
.SYNOPSIS
    后台启动发行包里的 zhiwei-service。

.DESCRIPTION
    使用当前目录的 .env 与 data/smart_assistant.db；PID 写入
    zhiwei-service.pid，标准输出/错误写入 logs/。重复运行会复用已在运行的
    同一份 zhiwei-service。
#>
[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
$root = $PSScriptRoot
$exe = Join-Path $root 'zhiwei-service.exe'
$envFile = Join-Path $root '.env'
$pidFile = Join-Path $root 'zhiwei-service.pid'
$logDir = Join-Path $root 'logs'
$stdoutLog = Join-Path $logDir 'zhiwei-service.stdout.log'
$stderrLog = Join-Path $logDir 'zhiwei-service.err.log'

if (-not (Test-Path -LiteralPath $exe)) {
    throw "Missing zhiwei-service.exe: $exe"
}
if (-not (Test-Path -LiteralPath $envFile)) {
    throw "Missing .env. Run: Copy-Item .env.example .env, then configure Telegram and LLM."
}

if (Test-Path -LiteralPath $pidFile) {
    $oldPid = Get-Content -LiteralPath $pidFile -ErrorAction SilentlyContinue |
        Select-Object -First 1
    if ($oldPid -match '^[0-9]+$') {
        $old = Get-Process -Id ([int]$oldPid) -ErrorAction SilentlyContinue
        if ($old) {
            $oldPath = $old.Path
            if ($oldPath -and ([IO.Path]::GetFullPath($oldPath) -eq [IO.Path]::GetFullPath($exe))) {
                Write-Host "zhiwei-service is already running (PID $($old.Id))" -ForegroundColor DarkGray
                return
            }
        }
    }
    Remove-Item -LiteralPath $pidFile -Force
}

New-Item -ItemType Directory -Path $logDir -Force | Out-Null
$proc = Start-Process -FilePath $exe -WorkingDirectory $root -WindowStyle Hidden -PassThru `
    -RedirectStandardOutput $stdoutLog -RedirectStandardError $stderrLog
Set-Content -LiteralPath $pidFile -Value $proc.Id -Encoding ascii

Start-Sleep -Seconds 3
$proc.Refresh()
if ($proc.HasExited) {
    Remove-Item -LiteralPath $pidFile -Force -ErrorAction SilentlyContinue
    Write-Host 'zhiwei-service failed to start. Recent logs:' -ForegroundColor Red
    foreach ($log in @($stdoutLog, $stderrLog)) {
        if (Test-Path -LiteralPath $log) {
            Write-Host "--- $log ---" -ForegroundColor DarkGray
            Get-Content -LiteralPath $log -Tail 40
        }
    }
    throw "zhiwei-service exited early (exit $($proc.ExitCode))"
}

Write-Host "zhiwei-service started (PID $($proc.Id))" -ForegroundColor Green
Write-Host "Log: $(Join-Path $logDir 'zhiwei-service.log')" -ForegroundColor DarkGray
Write-Host "Control: $(Join-Path $logDir 'control.json')" -ForegroundColor DarkGray

<#
.SYNOPSIS
    Zhiwei tray host.

.DESCRIPTION
    Keeps zhiwei-service running, shows a notification-area icon, and exposes
    settings, console, logs, restart, and quit actions.
#>
[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
$root = $PSScriptRoot
$exe = Join-Path $root 'zhiwei-service.exe'
$pidFile = Join-Path $root 'zhiwei-service.pid'
$logDir = Join-Path $root 'logs'
$stdoutLog = Join-Path $logDir 'zhiwei-service.stdout.log'
$stderrLog = Join-Path $logDir 'zhiwei-service.err.log'
$settingsScript = Join-Path $root 'settings.ps1'
$startScript = Join-Path $root 'start.ps1'
$stopScript = Join-Path $root 'stop.ps1'

Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing

$restartEvent = [System.Threading.EventWaitHandle]::new(
    $false,
    [System.Threading.EventResetMode]::AutoReset,
    'Local\SmartAssistantTrayRestart'
)
$mutex = [System.Threading.Mutex]::new($false, 'Local\SmartAssistantTray')
if (-not $mutex.WaitOne(0)) {
    $restartEvent.Set() | Out-Null
    return
}

function Get-LinkProcess {
    if (Test-Path -LiteralPath $pidFile) {
        $rawPid = Get-Content -LiteralPath $pidFile -ErrorAction SilentlyContinue |
            Select-Object -First 1
        if ($rawPid -match '^[0-9]+$') {
            $proc = Get-Process -Id ([int]$rawPid) -ErrorAction SilentlyContinue
            if ($proc -and $proc.Path -and
                ([IO.Path]::GetFullPath($proc.Path) -eq [IO.Path]::GetFullPath($exe))) {
                return $proc
            }
        }
    }
    Get-Process -Name 'zhiwei-service' -ErrorAction SilentlyContinue |
        Where-Object {
            $_.Path -and
            ([IO.Path]::GetFullPath($_.Path) -eq [IO.Path]::GetFullPath($exe))
        } |
        Select-Object -First 1
}

function Get-PwshExe {
    $local = Join-Path $PSHOME 'pwsh.exe'
    if (Test-Path -LiteralPath $local) {
        return $local
    }
    return (Get-Command pwsh.exe -ErrorAction Stop).Source
}

function Stop-Link {
    $proc = Get-LinkProcess
    if ($proc) {
        Stop-Process -Id $proc.Id -ErrorAction SilentlyContinue
        if (-not $proc.WaitForExit(5000)) {
            Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue
        }
    }
    Remove-Item -LiteralPath $pidFile -Force -ErrorAction SilentlyContinue
}

function Start-Link {
    $proc = Get-LinkProcess
    if ($proc) {
        return $proc
    }
    New-Item -ItemType Directory -Path $logDir -Force | Out-Null
    $newProc = Start-Process -FilePath $exe -WorkingDirectory $root `
        -WindowStyle Hidden -PassThru `
        -RedirectStandardOutput $stdoutLog -RedirectStandardError $stderrLog
    Set-Content -LiteralPath $pidFile -Value $newProc.Id -Encoding ascii
    Start-Sleep -Seconds 2
    $newProc.Refresh()
    if ($newProc.HasExited) {
        Remove-Item -LiteralPath $pidFile -Force -ErrorAction SilentlyContinue
        throw "zhiwei-service exited during startup (exit $($newProc.ExitCode))"
    }
    return $newProc
}

function Open-Settings {
    if (-not (Test-Path -LiteralPath $settingsScript)) {
        throw "Missing settings script: $settingsScript"
    }
    $pwsh = Get-PwshExe
    Start-Process -FilePath $pwsh -WorkingDirectory $root `
        -ArgumentList @('-Sta', '-NoLogo', '-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', $settingsScript) |
        Out-Null
}

function Open-Console {
    $pwsh = Get-PwshExe
    Start-Process -FilePath $pwsh -WorkingDirectory $root -ArgumentList @('-Sta', '-NoLogo', '-NoProfile') |
        Out-Null
}

function Update-Status {
    $proc = Get-LinkProcess
    if ($proc) {
        $script:notify.Text = "知微 Zhiwei - running (PID $($proc.Id))"
        $script:statusItem.Text = "Status: running (PID $($proc.Id))"
    } else {
        $script:notify.Text = '知微 Zhiwei - stopped'
        $script:statusItem.Text = 'Status: stopped'
    }
}

function Show-Balloon([string]$Title, [string]$Text, [string]$Icon = 'Info') {
    $script:notify.BalloonTipTitle = $Title
    $script:notify.BalloonTipText = $Text
    $script:notify.BalloonTipIcon = $Icon
    $script:notify.ShowBalloonTip(3000)
}

try {
    $icon = [System.Drawing.Icon]::ExtractAssociatedIcon($exe)
} catch {
    $icon = [System.Drawing.SystemIcons]::Application
}

$script:notify = New-Object System.Windows.Forms.NotifyIcon
$script:notify.Icon = $icon
$script:notify.Text = '知微 Zhiwei'
$script:notify.Visible = $true

$menu = New-Object System.Windows.Forms.ContextMenuStrip
$script:statusItem = New-Object System.Windows.Forms.ToolStripMenuItem
$script:statusItem.Enabled = $false
$menu.Items.Add($script:statusItem) | Out-Null
$menu.Items.Add((New-Object System.Windows.Forms.ToolStripSeparator)) | Out-Null

$settingsItem = New-Object System.Windows.Forms.ToolStripMenuItem
$settingsItem.Text = 'Settings'
$settingsItem.add_Click({ Open-Settings })
$menu.Items.Add($settingsItem) | Out-Null

$consoleItem = New-Object System.Windows.Forms.ToolStripMenuItem
$consoleItem.Text = 'Open PowerShell console'
$consoleItem.add_Click({ Open-Console })
$menu.Items.Add($consoleItem) | Out-Null

$logItem = New-Object System.Windows.Forms.ToolStripMenuItem
$logItem.Text = 'Open log file'
$logItem.add_Click({
    $log = Join-Path $logDir 'zhiwei-service.log'
    if (-not (Test-Path -LiteralPath $log)) {
        $log = $stdoutLog
    }
    if (Test-Path -LiteralPath $log) {
        Start-Process -FilePath 'notepad.exe' -ArgumentList "`"$log`"" | Out-Null
    } else {
        [System.Windows.Forms.MessageBox]::Show('No log file yet.', '知微 Zhiwei')
    }
})
$menu.Items.Add($logItem) | Out-Null

$folderItem = New-Object System.Windows.Forms.ToolStripMenuItem
$folderItem.Text = 'Open install folder'
$folderItem.add_Click({ Start-Process -FilePath 'explorer.exe' -ArgumentList $root | Out-Null })
$menu.Items.Add($folderItem) | Out-Null

$restartItem = New-Object System.Windows.Forms.ToolStripMenuItem
$restartItem.Text = 'Restart service'
$restartItem.add_Click({
    try {
        Stop-Link
        Start-Link | Out-Null
        Update-Status
        Show-Balloon '知微 Zhiwei' 'Service restarted.'
    } catch {
        Show-Balloon '知微 Zhiwei' $_.Exception.Message 'Error'
    }
})
$menu.Items.Add($restartItem) | Out-Null

$menu.Items.Add((New-Object System.Windows.Forms.ToolStripSeparator)) | Out-Null
$quitItem = New-Object System.Windows.Forms.ToolStripMenuItem
$quitItem.Text = 'Quit'
$quitItem.add_Click({
    Stop-Link
    $script:timer.Stop()
    $script:notify.Visible = $false
    $script:notify.Dispose()
    $script:context.ExitThread()
})
$menu.Items.Add($quitItem) | Out-Null

$script:notify.ContextMenuStrip = $menu
$script:notify.add_MouseDoubleClick({ Open-Settings })

$script:timer = New-Object System.Windows.Forms.Timer
$script:timer.Interval = 5000
$script:timer.add_Tick({
    try {
        if ($script:restartEvent.WaitOne(0)) {
            Stop-Link
            Start-Link | Out-Null
            Update-Status
            Show-Balloon '知微 Zhiwei' 'Service restarted.'
            return
        }
        if (-not (Get-LinkProcess)) {
            Start-Link | Out-Null
            Update-Status
            Show-Balloon '知微 Zhiwei' 'Service restarted automatically.'
        } else {
            Update-Status
        }
    } catch {
        Update-Status
        Show-Balloon '知微 Zhiwei' $_.Exception.Message 'Error'
    }
})
$script:timer.Start()

$script:context = New-Object System.Windows.Forms.ApplicationContext

try {
    Start-Link | Out-Null
    Update-Status
    Show-Balloon '知微 Zhiwei' 'Started. Right-click the tray icon for settings and controls.'
} catch {
    Update-Status
    Show-Balloon '知微 Zhiwei' $_.Exception.Message 'Error'
}

[System.Windows.Forms.Application]::Run($script:context)

<#
.SYNOPSIS
    Edit Zhiwei startup settings in a WinForms dialog.

.DESCRIPTION
    Writes startup-only values back into .env in place, preserving comments and
    unknown keys. Runtime settings are edited in the tray UI and stored in the
    database through zhiwei-service's local control API.
#>
[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
$root = $PSScriptRoot
$envPath = Join-Path $root '.env'
$logDir = Join-Path $root 'logs'
$stopScript = Join-Path $root 'stop.ps1'
$startScript = Join-Path $root 'start.ps1'

Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing

function Read-EnvLines {
    $utf8 = [Text.UTF8Encoding]::new($false)
    return [IO.File]::ReadAllText($envPath, $utf8) -split "`n"
}

function Get-EnvValue([string[]]$Lines, [string]$Name) {
    foreach ($line in $Lines) {
        if ($line -match "^\s*$([regex]::Escape($Name))\s*=\s*(.*)$") {
            return $Matches[1].TrimEnd("`r")
        }
    }
    return ''
}

function Set-EnvValue([string[]]$Lines, [string]$Name, [string]$Value) {
    $found = $false
    for ($i = 0; $i -lt $Lines.Count; $i++) {
        if ($Lines[$i] -match "^\s*$([regex]::Escape($Name))\s*=") {
            $Lines[$i] = "$Name=$Value"
            $found = $true
            break
        }
    }
    if (-not $found) {
        $Lines += "$Name=$Value"
    }
    return ,$Lines
}

function Get-PwshExe {
    $local = Join-Path $PSHOME 'pwsh.exe'
    if (Test-Path -LiteralPath $local) {
        return $local
    }
    return (Get-Command pwsh.exe -ErrorAction Stop).Source
}

if (-not (Test-Path -LiteralPath $envPath)) {
    [System.Windows.Forms.MessageBox]::Show(
        "Missing .env: $envPath",
        '知微 Zhiwei'
    ) | Out-Null
    return
}

$lines = Read-EnvLines

$form = New-Object System.Windows.Forms.Form
$form.Text = '知微 Zhiwei Settings'
$form.Width = 700
$form.Height = 760
$form.StartPosition = 'CenterScreen'
$form.FormBorderStyle = 'FixedDialog'
$form.MaximizeBox = $false
$form.MinimizeBox = $false
$form.AutoScroll = $true
$form.Font = New-Object System.Drawing.Font('Segoe UI', 9)
$form.ShowInTaskbar = $true

$title = New-Object System.Windows.Forms.Label
$title.Text = 'Startup settings are saved to .env. Runtime settings are edited in the tray UI and stored in the database.'
$title.AutoSize = $true
$title.Location = New-Object System.Drawing.Point(20, 15)
$form.Controls.Add($title)

$y = 45
$inputs = @{}

function Add-Text([string]$Label, [string]$Name, [bool]$Secret = $false, [string]$Value = '') {
    $script:yLabel = New-Object System.Windows.Forms.Label
    $script:yLabel.Text = $Label
    $script:yLabel.AutoSize = $false
    $script:yLabel.Width = 190
    $script:yLabel.Location = New-Object System.Drawing.Point(20, ($script:y + 3))
    $form.Controls.Add($script:yLabel)

    $box = New-Object System.Windows.Forms.TextBox
    $box.Width = 440
    $box.Location = New-Object System.Drawing.Point(220, $script:y)
    $box.Text = $Value
    if ($Secret) { $box.UseSystemPasswordChar = $true }
    $form.Controls.Add($box)
    $script:inputs[$Name] = $box
    $script:y += 32
}

function Add-Combo([string]$Label, [string]$Name, [string[]]$Items, [string]$Value) {
    $script:yLabel = New-Object System.Windows.Forms.Label
    $script:yLabel.Text = $Label
    $script:yLabel.AutoSize = $false
    $script:yLabel.Width = 190
    $script:yLabel.Location = New-Object System.Drawing.Point(20, ($script:y + 3))
    $form.Controls.Add($script:yLabel)

    $box = New-Object System.Windows.Forms.ComboBox
    $box.Width = 440
    $box.DropDownStyle = 'DropDownList'
    $box.Location = New-Object System.Drawing.Point(220, $script:y)
    $box.Items.AddRange($Items)
    if ($Items -contains $Value) { $box.SelectedItem = $Value } elseif ($Items.Count -gt 0) { $box.SelectedIndex = 0 }
    $form.Controls.Add($box)
    $script:inputs[$Name] = $box
    $script:y += 32
}

function Add-Check([string]$Label, [string]$Name, [bool]$Checked) {
    $box = New-Object System.Windows.Forms.CheckBox
    $box.Text = $Label
    $box.AutoSize = $true
    $box.Location = New-Object System.Drawing.Point(220, $script:y)
    $box.Checked = $Checked
    $form.Controls.Add($box)
    $script:inputs[$Name] = $box
    $script:y += 28
}

Add-Combo 'LLM provider' 'LLM_PROVIDER' @('openai', 'google') (Get-EnvValue $lines 'LLM_PROVIDER')
Add-Text 'LLM API URL' 'LLM_API_URL' $false (Get-EnvValue $lines 'LLM_API_URL')
Add-Text 'LLM API key' 'LLM_API_KEY' $true (Get-EnvValue $lines 'LLM_API_KEY')
Add-Text 'LLM model' 'LLM_MODEL' $false (Get-EnvValue $lines 'LLM_MODEL')
Add-Combo 'Structured output' 'LLM_STRUCTURED_OUTPUT' @('json_schema', 'json_object') (Get-EnvValue $lines 'LLM_STRUCTURED_OUTPUT')
Add-Text 'Embedding API URL' 'EMBEDDING_API_URL' $false (Get-EnvValue $lines 'EMBEDDING_API_URL')
Add-Text 'Embedding API key' 'EMBEDDING_API_KEY' $true (Get-EnvValue $lines 'EMBEDDING_API_KEY')
Add-Text 'Embedding model' 'EMBEDDING_MODEL' $false (Get-EnvValue $lines 'EMBEDDING_MODEL')
Add-Text 'Embedding dimension' 'EMBEDDING_DIM' $false (Get-EnvValue $lines 'EMBEDDING_DIM')
Add-Text 'Telegram bot token' 'TELEGRAM_BOT_TOKEN' $true (Get-EnvValue $lines 'TELEGRAM_BOT_TOKEN')
Add-Text 'Telegram allowed IDs' 'TELEGRAM_ALLOWED_IDS' $false (Get-EnvValue $lines 'TELEGRAM_ALLOWED_IDS')
Add-Text 'Telegram proxy' 'TELEGRAM_PROXY' $false (Get-EnvValue $lines 'TELEGRAM_PROXY')
Add-Check 'Enable Telegram' 'TELEGRAM_ENABLED' ((Get-EnvValue $lines 'TELEGRAM_ENABLED') -notin @('0', 'false', 'False'))
Add-Check 'Enable proactive messages' 'INTENT_ENABLED' ((Get-EnvValue $lines 'INTENT_ENABLED') -in @('1', 'true', 'True'))
Add-Text 'Proactive min minutes' 'INTENT_MIN_MINUTES' $false (Get-EnvValue $lines 'INTENT_MIN_MINUTES')
Add-Text 'Proactive max minutes' 'INTENT_MAX_MINUTES' $false (Get-EnvValue $lines 'INTENT_MAX_MINUTES')
Add-Text 'Quiet hours (HH-HH)' 'INTENT_QUIET_HOURS' $false (Get-EnvValue $lines 'INTENT_QUIET_HOURS')
Add-Combo 'Log level' 'RUST_LOG' @('info', 'debug', 'warn', 'error') (Get-EnvValue $lines 'RUST_LOG')

$dbLabel = New-Object System.Windows.Forms.Label
$dbLabel.Text = 'Database (read-only)'
$dbLabel.AutoSize = $false
$dbLabel.Width = 190
$dbLabel.Location = New-Object System.Drawing.Point(20, ($y + 3))
$form.Controls.Add($dbLabel)
$dbBox = New-Object System.Windows.Forms.TextBox
$dbBox.Width = 440
$dbBox.Location = New-Object System.Drawing.Point(220, $y)
$dbBox.ReadOnly = $true
$dbBox.Text = Get-EnvValue $lines 'SMART_ASSISTANT_DB'
$form.Controls.Add($dbBox)
$y += 38

$restart = New-Object System.Windows.Forms.CheckBox
$restart.Text = 'Restart service after saving'
$restart.Checked = $true
$restart.AutoSize = $true
$restart.Location = New-Object System.Drawing.Point(220, $y)
$form.Controls.Add($restart)
$y += 40

$save = New-Object System.Windows.Forms.Button
$save.Text = 'Save'
$save.Width = 110
$save.Location = New-Object System.Drawing.Point(220, $y)
$form.Controls.Add($save)

$cancel = New-Object System.Windows.Forms.Button
$cancel.Text = 'Cancel'
$cancel.Width = 110
$cancel.Location = New-Object System.Drawing.Point(340, $y)
$form.Controls.Add($cancel)

$openEnv = New-Object System.Windows.Forms.Button
$openEnv.Text = 'Open .env'
$openEnv.Width = 110
$openEnv.Location = New-Object System.Drawing.Point(460, $y)
$form.Controls.Add($openEnv)

$openLogs = New-Object System.Windows.Forms.Button
$openLogs.Text = 'Open logs'
$openLogs.Width = 110
$openLogs.Location = New-Object System.Drawing.Point(580, $y)
$form.Controls.Add($openLogs)

$cancel.add_Click({ $form.Close() })
$openEnv.add_Click({ Start-Process -FilePath 'notepad.exe' -ArgumentList "`"$envPath`"" | Out-Null })
$openLogs.add_Click({
    $log = Join-Path $logDir 'zhiwei-service.log'
    if (Test-Path -LiteralPath $log) {
        Start-Process -FilePath 'notepad.exe' -ArgumentList "`"$log`"" | Out-Null
    } else {
        [System.Windows.Forms.MessageBox]::Show('No log file yet.', '知微 Zhiwei') | Out-Null
    }
})

$save.add_Click({
    try {
        $values = @{}
        foreach ($name in $script:inputs.Keys) {
            $control = $script:inputs[$name]
            if ($control -is [System.Windows.Forms.CheckBox]) {
                $values[$name] = if ($control.Checked) { '1' } else { '0' }
            } else {
                $values[$name] = $control.Text.Trim()
            }
        }
        foreach ($name in @('EMBEDDING_DIM', 'INTENT_MIN_MINUTES', 'INTENT_MAX_MINUTES')) {
            if ($values[$name] -notmatch '^[0-9]+$') {
                throw "$name must be a positive integer."
            }
        }
        if ([int]$values['INTENT_MIN_MINUTES'] -gt [int]$values['INTENT_MAX_MINUTES']) {
            throw 'Proactive min minutes cannot exceed max minutes.'
        }
        if ($values['LLM_API_URL'] -eq '' -or $values['LLM_MODEL'] -eq '') {
            throw 'LLM API URL and model cannot be empty.'
        }

        foreach ($name in $values.Keys) {
            $lines = Set-EnvValue $lines $name $values[$name]
        }
        $utf8 = [Text.UTF8Encoding]::new($false)
        [IO.File]::WriteAllText($envPath, ($lines -join "`n"), $utf8)

        if ($restart.Checked) {
            $pwsh = Get-PwshExe
            Start-Process -FilePath $pwsh -WorkingDirectory $root -WindowStyle Hidden -Wait `
                -ArgumentList @('-NoLogo', '-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', $stopScript) |
                Out-Null
            Start-Process -FilePath $pwsh -WorkingDirectory $root -WindowStyle Hidden -Wait `
                -ArgumentList @('-NoLogo', '-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', $startScript) |
                Out-Null
        }
        [System.Windows.Forms.MessageBox]::Show('Settings saved.', '知微 Zhiwei') | Out-Null
        $form.Close()
    } catch {
        [System.Windows.Forms.MessageBox]::Show($_.Exception.Message, '知微 Zhiwei', 'OK', 'Error') | Out-Null
    }
})

$form.add_Shown({
    $form.Visible = $true
    $form.Activate()
    $form.TopMost = $true
    $form.TopMost = $false
})
[void]$form.ShowDialog()

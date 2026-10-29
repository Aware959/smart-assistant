param(
    [Parameter(Mandatory = $true)]
    [ValidateSet("pre-edit", "post-edit", "stop")]
    [string]$Mode,
    [switch]$SkipTooling
)

$ErrorActionPreference = "Stop"
$scriptPath = $MyInvocation.MyCommand.Path
if ($scriptPath) {
    $scriptDirectory = Split-Path -Parent $scriptPath
} else {
    $scriptDirectory = $PSScriptRoot
}
$script:RepoRoot = (Resolve-Path (Join-Path $scriptDirectory "..\..")).Path

function Write-HookJson {
    param([hashtable]$Value)
    $Value | ConvertTo-Json -Compress -Depth 8
}

function Read-HookInput {
    $raw = [Console]::In.ReadToEnd()
    if ([string]::IsNullOrWhiteSpace($raw)) {
        return @{}
    }
    return $raw | ConvertFrom-Json
}

function Get-PatchText {
    param($ToolInput)
    if ($null -eq $ToolInput) { return "" }
    if ($ToolInput -is [string]) { return $ToolInput }
    foreach ($name in @("input", "patch", "command")) {
        $property = $ToolInput.PSObject.Properties[$name]
        if ($null -ne $property -and $property.Value -is [string]) {
            return $property.Value
        }
    }
    return ""
}

function Get-NormalizedPath {
    param([string]$RawPath)
    $raw = $RawPath.Trim().Trim('"')
    if ($raw.StartsWith("a/") -or $raw.StartsWith("b/")) {
        $raw = $raw.Substring(2)
    }
    if ([IO.Path]::IsPathRooted($raw)) {
        $full = [IO.Path]::GetFullPath($raw)
    } else {
        $full = [IO.Path]::GetFullPath((Join-Path $script:RepoRoot $raw))
    }
    $root = [IO.Path]::GetFullPath($script:RepoRoot).TrimEnd("\")
    $prefix = $root + "\"
    if ($full.StartsWith($prefix, [StringComparison]::OrdinalIgnoreCase)) {
        return $full.Substring($prefix.Length).Replace("\", "/")
    }
    if ($full.Equals($root, [StringComparison]::OrdinalIgnoreCase)) {
        return ""
    }
    return $full.Replace("\", "/")
}

function Get-PatchEntries {
    param([string]$Patch)
    $entries = @()
    foreach ($match in [regex]::Matches($Patch, "(?m)^\*\*\* (Add File|Update File|Delete File|Move to):\s*(.+?)\s*$")) {
        $action = $match.Groups[1].Value
        $relative = Get-NormalizedPath -RawPath $match.Groups[2].Value
        $entries += [pscustomobject]@{
            Action = $action
            Path = $relative
            IsRust = ($relative -match "^backend/src/.*\.rs$")
        }
    }
    return @($entries)
}

function Test-ProtectedPath {
    param([string]$Path)
    $pathLower = $Path.ToLowerInvariant()
    if ($pathLower -eq "frontend" -or $pathLower.StartsWith("frontend/")) {
        return "frontend/** belongs to the Web client and is protected during Telegram-first work"
    }
    if ($pathLower -eq "backend/src/api" -or $pathLower.StartsWith("backend/src/api/")) {
        return "backend/src/api/** belongs to the Web API and is protected during Telegram-first work"
    }
    if ($pathLower -eq "backend/src/channels/ilink" -or $pathLower.StartsWith("backend/src/channels/ilink/")) {
        return "backend/src/channels/ilink/** belongs to the iLink channel and is protected during Telegram-first work"
    }
    return $null
}

function Test-RustPathNaming {
    param([string]$Path)
    if ($Path -notmatch "^backend/src/(.*\.rs)$") { return $null }
    $tail = $Matches[1]
    $parts = $tail.Split("/")
    $fileName = $parts[-1]
    $isBin = $tail.StartsWith("bin/")
    $filePattern = if ($isBin) {
        "^[a-z0-9][a-z0-9-]*\.rs$"
    } else {
        "^[a-z][a-z0-9_]*\.rs$"
    }
    if ($fileName -cnotmatch $filePattern) {
        return "New Rust file names must use lowercase snake_case: $Path"
    }
    if (-not $isBin -and $fileName -cmatch "^[a-z0-9_]+_tests\.rs$") {
        return "New unit tests belong in the source file's #[cfg(test)] mod tests: $Path"
    }
    for ($i = 0; $i -lt ($parts.Count - 1); $i++) {
        if ($parts[$i] -cnotmatch "^[a-z][a-z0-9_]*$") {
            return "New Rust directory names must use lowercase snake_case: $Path"
        }
    }
    return $null
}

function Test-ArchiveDirectoryNaming {
    param([string]$Path)
    if ($Path -notmatch "^docs/archive/(.*)$") { return $null }
    $tail = $Matches[1]
    $parts = $tail.Split("/")
    if ($parts.Count -lt 2) { return $null }
    for ($i = 0; $i -lt ($parts.Count - 1); $i++) {
        if ($parts[$i] -cnotmatch "^[A-Za-z][A-Za-z0-9_-]*$") {
            return "Directory names under docs/archive/ must use English words: $Path"
        }
    }
    return $null
}

function Get-AddedTrailingWhitespace {
    param([string]$Patch)
    $lines = $Patch -split "`r`n|`n|`r"
    $lineNumber = 0
    $result = @()
    foreach ($line in $lines) {
        $lineNumber++
        if ($line -match "^\+(?!\+\+)(.*[ \t]+)$") {
            $result += "patch line $lineNumber has trailing whitespace"
        }
    }
    return @($result)
}

function Get-GuardStatePath {
    param($Payload)
    $session = [string]$Payload.session_id
    $turn = [string]$Payload.turn_id
    if (-not $turn) { $turn = $session }
    if (-not $turn) { $turn = "unknown" }
    $key = [regex]::Replace("$session-$turn", "[^A-Za-z0-9_-]", "_")
    if ($key.Length -gt 100) { $key = $key.Substring(0, 100) }
    $tempRoot = $env:TEMP
    if (-not $tempRoot) { $tempRoot = $env:TMP }
    if (-not $tempRoot) { $tempRoot = [IO.Path]::GetTempPath() }
    return Join-Path $tempRoot "codex-repo-guard-$key.json"
}

function Add-GuardStatePaths {
    param($Payload, [string[]]$Paths)
    $statePath = Get-GuardStatePath -Payload $Payload
    $state = $null
    if (Test-Path -LiteralPath $statePath) {
        try { $state = Get-Content -Raw -LiteralPath $statePath | ConvertFrom-Json } catch { $state = $null }
    }
    $existing = @()
    if ($null -ne $state -and $null -ne $state.rustPaths) {
        $existing = @($state.rustPaths)
    }
    $all = @($existing + $Paths | Where-Object { $_ } | Sort-Object -Unique)
    @{ rustPaths = $all } | ConvertTo-Json -Compress | Set-Content -LiteralPath $statePath -Encoding UTF8
}

function Read-GuardState {
    param($Payload)
    $statePath = Get-GuardStatePath -Payload $Payload
    if (-not (Test-Path -LiteralPath $statePath)) {
        return @{ Path = $statePath; RustPaths = @() }
    }
    try {
        $state = Get-Content -Raw -LiteralPath $statePath | ConvertFrom-Json
        return @{ Path = $statePath; RustPaths = @($state.rustPaths) }
    } catch {
        Remove-Item -LiteralPath $statePath -Force -ErrorAction SilentlyContinue
        return @{ Path = $statePath; RustPaths = @() }
    }
}

function Remove-GuardState {
    param([string]$Path)
    Remove-Item -LiteralPath $Path -Force -ErrorAction SilentlyContinue
}

function Invoke-CargoCheck {
    param([string[]]$Arguments)
    if ($SkipTooling -or -not (Get-Command cargo -ErrorAction SilentlyContinue)) {
        return $null
    }
    $backend = Join-Path $script:RepoRoot "backend"
    if (-not (Test-Path -LiteralPath $backend)) { return $null }
    Push-Location $backend
    try {
        $output = @(& cargo @Arguments 2>&1)
        $code = $LASTEXITCODE
    } finally {
        Pop-Location
    }
    if ($code -eq 0) { return $null }
    $summary = @($output | Where-Object { $_ -match "error|warning|Diff in|failed|not formatted" } | Select-Object -First 12)
    if ($summary.Count -eq 0) { $summary = @($output | Select-Object -First 12) }
    $text = ($summary -join " | ").Trim()
    if (-not $text) { $text = "cargo $($Arguments -join ' ') failed" }
    return $text
}

function Invoke-PreEdit {
    param($Payload)
    $patch = Get-PatchText -ToolInput $Payload.tool_input
    if (-not $patch) { return }
    $entries = Get-PatchEntries -Patch $patch
    $reasons = @()
    $rustPaths = @()
    foreach ($entry in $entries) {
        if (-not $entry.Path) { continue }
        if ($entry.IsRust) { $rustPaths += $entry.Path }
        $protected = Test-ProtectedPath -Path $entry.Path
        if ($protected) { $reasons += $protected }
        if ($entry.Action -eq "Add File" -or $entry.Action -eq "Move to") {
            $nameIssue = Test-RustPathNaming -Path $entry.Path
            if ($nameIssue) { $reasons += $nameIssue }
            $archiveIssue = Test-ArchiveDirectoryNaming -Path $entry.Path
            if ($archiveIssue) { $reasons += $archiveIssue }
        }
    }
    if ($reasons.Count -gt 0) {
        $reason = ($reasons | Sort-Object -Unique) -join "; "
        Write-HookJson @{
            hookSpecificOutput = @{
                hookEventName = "PreToolUse"
                permissionDecision = "deny"
                permissionDecisionReason = $reason
            }
        }
        return
    }
    if ($rustPaths.Count -gt 0) {
        Add-GuardStatePaths -Payload $Payload -Paths $rustPaths
    }
}

function Invoke-PostEdit {
    param($Payload)
    $patch = Get-PatchText -ToolInput $Payload.tool_input
    if (-not $patch) { return }
    $entries = Get-PatchEntries -Patch $patch
    $rustPaths = @($entries | Where-Object { $_.IsRust } | ForEach-Object { $_.Path })
    if ($rustPaths.Count -gt 0) {
        Add-GuardStatePaths -Payload $Payload -Paths $rustPaths
    }
    $reasons = @(Get-AddedTrailingWhitespace -Patch $patch)
    $existingRust = @($rustPaths | Where-Object { Test-Path -LiteralPath (Join-Path $script:RepoRoot $_) })
    if ($existingRust.Count -gt 0) {
        $fmtError = Invoke-CargoCheck -Arguments @("fmt", "--all", "--", "--check")
        if ($fmtError) { $reasons += "cargo fmt check failed: $fmtError" }
    }
    if ($reasons.Count -gt 0) {
        Write-HookJson @{
            decision = "block"
            reason = ($reasons -join "; ")
        }
    }
}

function Invoke-Stop {
    param($Payload)
    $state = Read-GuardState -Payload $Payload
    if ($state.RustPaths.Count -eq 0) {
        Remove-GuardState -Path $state.Path
        return
    }
    $fmtError = Invoke-CargoCheck -Arguments @("fmt", "--all", "--", "--check")
    $checkError = $null
    if (-not $fmtError) {
        $checkError = Invoke-CargoCheck -Arguments @("check", "--all-targets")
    }
    $errorText = @($fmtError, $checkError | Where-Object { $_ }) -join "; "
    Remove-GuardState -Path $state.Path
    if (-not $errorText) { return }
    if ([bool]$Payload.stop_hook_active) {
        Write-HookJson @{ systemMessage = "repo guard: $errorText" }
    } else {
        Write-HookJson @{
            decision = "block"
            reason = "repo guard: $errorText"
        }
    }
}

try {
    $payload = Read-HookInput
    switch ($Mode) {
        "pre-edit" { Invoke-PreEdit -Payload $payload }
        "post-edit" { Invoke-PostEdit -Payload $payload }
        "stop" { Invoke-Stop -Payload $payload }
    }
    exit 0
} catch {
    $message = $_.Exception.Message
    if ($Mode -eq "pre-edit") {
        Write-HookJson @{
            hookSpecificOutput = @{
                hookEventName = "PreToolUse"
                permissionDecision = "deny"
                permissionDecisionReason = "repo guard error: $message"
            }
        }
    } else {
        Write-HookJson @{ systemMessage = "repo guard error: $message" }
    }
    exit 0
}


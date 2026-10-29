param(
    [string]$Root = ""
)

$ErrorActionPreference = "Stop"
$scriptPath = $MyInvocation.MyCommand.Path

if (-not $Root) {
    if ($scriptPath) {
        $scriptDirectory = Split-Path -Parent $scriptPath
    } else {
        $scriptDirectory = $PSScriptRoot
    }
    $Root = (Resolve-Path (Join-Path $scriptDirectory "..\..\..\..")).Path
}

$Root = (Resolve-Path -LiteralPath $Root).Path

function Get-RepoFiles {
    param([string]$Path)

    Get-ChildItem -LiteralPath $Path -Recurse -File -ErrorAction SilentlyContinue |
        Where-Object {
            $relative = $_.FullName.Substring($Root.Length).TrimStart("\")
            $relative -notmatch "^\.git\\" -and
            $relative -notmatch "^target\\" -and
            $relative -notmatch "^backend\\target" -and
            $relative -notmatch "^frontend\\node_modules" -and
            $relative -notmatch "^frontend\\dist" -and
            $relative -notmatch "^media-gateway\\.venv" -and
            $relative -notmatch "^media-gateway\\__pycache__" -and
            $relative -notmatch "^backend\\logs" -and
            $relative -notmatch "^backend\\certs" -and
            $relative -notmatch "^backend\\bindings" -and
            $relative -notmatch "\.db(-wal|-shm)?$" -and
            $relative -notmatch "\.env$" -and
            $relative -notmatch "sticker_catalog\.json$" -and
            $relative -notmatch "ilink_session\.json$"
        }
}

function Get-RelativePath {
    param([string]$FullName)
    $FullName.Substring($Root.Length).TrimStart("\").Replace("\", "/")
}

function Get-LineCount {
    param([string]$Path)
    if (-not (Test-Path -LiteralPath $Path)) { return 0 }
    return @([IO.File]::ReadAllLines($Path)).Count
}

$files = @(Get-RepoFiles -Path $Root)
$rustFiles = @($files | Where-Object { $_.Extension -eq ".rs" })
$textFiles = @($files | Where-Object {
    $_.Extension -in @(".rs", ".md", ".toml", ".json", ".ps1", ".yaml", ".yml", ".ts", ".tsx", ".js", ".jsx", ".css")
})
$scanScript = if ($scriptPath) { Get-RelativePath -FullName $scriptPath } else { "" }
$auditReference = ".agents/skills/repo-governance/references/audit-checklist.md"

$statusLines = @()
if (Get-Command git -ErrorAction SilentlyContinue) {
    $statusLines = @(& git -C $Root status --short 2>$null)
}

$largeFiles = @(
    $rustFiles |
        ForEach-Object {
            [pscustomobject]@{
                Lines = Get-LineCount -Path $_.FullName
                Path = Get-RelativePath -FullName $_.FullName
            }
        } |
        Sort-Object Lines -Descending |
        Select-Object -First 20
)

$badRustNames = @(
    $rustFiles |
        ForEach-Object {
            $name = $_.Name
            $isBin = (Get-RelativePath -FullName $_.FullName) -match "^backend/src/bin/"
            $valid = if ($isBin) {
                $name -cmatch "^[a-z0-9][a-z0-9-]*\.rs$"
            } else {
                $name -cmatch "^[a-z][a-z0-9_]*\.rs$"
            }
            if (-not $valid) {
                Get-RelativePath -FullName $_.FullName
            }
        }
)

$legacyTestFiles = @(
    $rustFiles |
        Where-Object { $_.Name -match "^[a-z0-9_]+_tests\.rs$" } |
        ForEach-Object { Get-RelativePath -FullName $_.FullName }
)

$markers = @(
    $textFiles |
        Select-String -Pattern "\b(TODO|FIXME|HACK|XXX)\b" -CaseSensitive:$true |
        ForEach-Object {
            "{0}:{1}: {2}" -f (Get-RelativePath -FullName $_.Path), $_.LineNumber, $_.Line.Trim()
        } |
        Where-Object {
            $relative = ($_ -split ":\d+:", 2)[0]
            $relative -ne $scanScript -and $relative -ne $auditReference
        } |
        Select-Object -First 80
)

$archiveDirNames = @()
$archiveRoot = Join-Path $Root "docs\archive"
if (Test-Path -LiteralPath $archiveRoot) {
    $archiveDirNames = @(
        Get-ChildItem -LiteralPath $archiveRoot -Recurse -Directory |
            ForEach-Object { $_.Name } |
            Sort-Object -Unique
    )
}
$badArchiveDirs = @($archiveDirNames | Where-Object { $_ -cnotmatch "^[A-Za-z][A-Za-z0-9_-]*$" })

$duplicateNames = @(
    $rustFiles |
        Group-Object Name |
        Where-Object { $_.Count -gt 1 } |
        Sort-Object Count -Descending |
        Select-Object -First 20 |
        ForEach-Object {
            "{0} ({1})" -f $_.Name, $_.Count
        }
)

Write-Output "REPO_GOVERNANCE_SCAN"
Write-Output ("root: {0}" -f $Root)
Write-Output ("generated_at: {0}" -f (Get-Date).ToString("yyyy-MM-ddTHH:mm:ss"))
Write-Output ("tracked_or_dirty_files: {0}" -f $files.Count)
Write-Output ("rust_files: {0}" -f $rustFiles.Count)
Write-Output ("git_status_entries: {0}" -f $statusLines.Count)
Write-Output ""

Write-Output "GIT_STATUS"
if ($statusLines.Count -eq 0) {
    Write-Output "(clean)"
} else {
    $statusLines | Select-Object -First 40 | ForEach-Object { $_ }
}

Write-Output ""
Write-Output "LARGEST_RUST_FILES"
if ($largeFiles.Count -eq 0) {
    Write-Output "(none)"
} else {
    $largeFiles | ForEach-Object { "{0,6}  {1}" -f $_.Lines, $_.Path }
}

Write-Output ""
Write-Output "RUST_NAMING_ANOMALIES"
if ($badRustNames.Count -eq 0) {
    Write-Output "(none)"
} else {
    $badRustNames | ForEach-Object { $_ }
}

Write-Output ""
Write-Output "LEGACY_SIBLING_TEST_FILES"
if ($legacyTestFiles.Count -eq 0) {
    Write-Output "(none)"
} else {
    Write-Output ("count: {0} (historical files are not forced to migrate)" -f $legacyTestFiles.Count)
    $legacyTestFiles | Select-Object -First 30 | ForEach-Object { $_ }
}

Write-Output ""
Write-Output "TODO_FIXME_HACK"
if ($markers.Count -eq 0) {
    Write-Output "(none)"
} else {
    $markers | ForEach-Object { $_ }
}

Write-Output ""
Write-Output "ARCHIVE_DIRECTORY_NAMES"
if ($badArchiveDirs.Count -eq 0) {
    Write-Output "(none)"
} else {
    $badArchiveDirs | ForEach-Object { "non-English directory: {0}" -f $_ }
}

Write-Output ""
Write-Output "DUPLICATE_RUST_FILENAMES"
if ($duplicateNames.Count -eq 0) {
    Write-Output "(none)"
} else {
    $duplicateNames | ForEach-Object { $_ }
}

Write-Output ""
Write-Output "NOTE"
Write-Output "Large files and duplicate names are signals, not automatic refactor decisions."
Write-Output "Audit evidence must be verified at file:line before proposing cleanup."


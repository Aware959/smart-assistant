<#
.SYNOPSIS
    Build media-gateway as a standalone Windows executable.

.DESCRIPTION
    Uses PyInstaller from the selected Python interpreter. The default output is
    a single media-gateway.exe; -OneDir produces a directory instead, which
    starts faster and is easier to inspect.

    The gateway itself only needs the Python standard library. ASR is optional:
    pass -WithAsr to bundle faster-whisper / ctranslate2 / av and their native
    dependencies. The Whisper model is still downloaded on first use (or read
    from ASR_MODEL_DIR).
#>
[CmdletBinding()]
param(
    [string]$Python = '',
    [string]$OutputDir = '',
    [switch]$OneDir,
    [switch]$WithAsr,
    [switch]$SkipInstall,
    [switch]$SkipTests
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$source = Join-Path $root 'media-gateway'
$buildRoot = Join-Path $source 'build'
$distRoot = if ($OutputDir) {
    [IO.Path]::GetFullPath($OutputDir)
} else {
    Join-Path $source 'dist'
}

function Assert-LastExitCode([string]$Step) {
    if ($LASTEXITCODE -ne 0) {
        throw "$Step failed (exit $LASTEXITCODE)"
    }
}

function Resolve-Python {
    $candidates = @()
    if ($Python) {
        $candidates += $Python
    } else {
        $venvPython = Join-Path $source '.venv\Scripts\python.exe'
        if (Test-Path -LiteralPath $venvPython) {
            $candidates += $venvPython
        }
        $command = Get-Command python.exe -ErrorAction SilentlyContinue |
            Select-Object -First 1
        if ($command) {
            $candidates += $command.Source
        }
    }

    foreach ($candidate in $candidates) {
        if (-not (Test-Path -LiteralPath $candidate)) {
            continue
        }
        try {
            & $candidate --version *> $null
            if ($LASTEXITCODE -eq 0) {
                return (Resolve-Path -LiteralPath $candidate).Path
            }
        } catch {
            continue
        }
    }

    throw 'No usable Python interpreter found. Pass -Python <path-to-python.exe>.'
}

function Test-PythonModule([string]$PythonPath, [string]$Module) {
    & $PythonPath -c "import importlib.util, sys; sys.exit(0 if importlib.util.find_spec('$Module') else 1)" *> $null
    return $LASTEXITCODE -eq 0
}

$pythonPath = Resolve-Python
Write-Host ".. Using Python: $pythonPath" -ForegroundColor DarkGray

if (-not (Test-PythonModule $pythonPath 'PyInstaller')) {
    if ($SkipInstall) {
        throw 'PyInstaller is not installed in the selected interpreter. Install it or omit -SkipInstall.'
    }
    Write-Host '.. Installing PyInstaller' -ForegroundColor Yellow
    & $pythonPath -m pip install --upgrade pyinstaller
    Assert-LastExitCode 'pip install pyinstaller'
}

if ($WithAsr -and -not (Test-PythonModule $pythonPath 'faster_whisper')) {
    if ($SkipInstall) {
        throw 'faster-whisper is not installed. Install faster-whisper "av<19" or build without -WithAsr.'
    }
    Write-Host '.. Installing faster-whisper and AV dependencies' -ForegroundColor Yellow
    & $pythonPath -m pip install 'faster-whisper' 'av<19'
    Assert-LastExitCode 'pip install faster-whisper av<19'
}

if (-not $SkipTests) {
    Push-Location $source
    try {
        Write-Host '.. Running media-gateway tests' -ForegroundColor Yellow
        & $pythonPath -m unittest discover -v
        Assert-LastExitCode 'python -m unittest discover'
    }
    finally {
        Pop-Location
    }
}

$arguments = @(
    '-m', 'PyInstaller',
    '--noconfirm',
    '--clean',
    '--name', 'media-gateway',
    '--console',
    '--paths', $source,
    '--distpath', $distRoot,
    '--workpath', (Join-Path $buildRoot 'pyinstaller'),
    '--specpath', $buildRoot,
    '--collect-submodules', 'gateway'
)
if (-not $OneDir) {
    $arguments += '--onefile'
}

$dataFiles = @(
    'README.md',
    '.env.example',
    'SD模板2.json'
)
foreach ($name in $dataFiles) {
    $path = Join-Path $source $name
    if (Test-Path -LiteralPath $path) {
        $arguments += @('--add-data', "$path;.")
    }
}

$optionalCollections = @()
if (Test-PythonModule $pythonPath 'edge_tts') {
    $optionalCollections += 'edge_tts'
}
if ($WithAsr) {
    foreach ($module in @(
        'faster_whisper',
        'ctranslate2',
        'av',
        'tokenizers',
        'huggingface_hub',
        'onnxruntime',
        'numpy'
    )) {
        if (Test-PythonModule $pythonPath $module) {
            $optionalCollections += $module
        }
    }
}
foreach ($module in $optionalCollections) {
    $arguments += @('--collect-all', $module)
}

$arguments += (Join-Path $source 'main.py')

Push-Location $source
try {
    Write-Host '.. Building media-gateway.exe' -ForegroundColor Yellow
    & $pythonPath @arguments
    Assert-LastExitCode 'PyInstaller media-gateway'
}
finally {
    Pop-Location
}

$artifact = if ($OneDir) {
    Join-Path $distRoot 'media-gateway\media-gateway.exe'
} else {
    Join-Path $distRoot 'media-gateway.exe'
}
if (-not (Test-Path -LiteralPath $artifact)) {
    throw "Missing build artifact: $artifact"
}

Write-Host ''
Write-Host "Artifact: $artifact" -ForegroundColor Green
if ($WithAsr) {
    Write-Host 'ASR dependencies were bundled. The model is still downloaded on first use.' -ForegroundColor DarkGray
}

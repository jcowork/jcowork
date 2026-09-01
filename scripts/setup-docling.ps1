# Bootstrap the Python environment required by the bundled Docling service.
#
# Creates ~/.jcowork/venv (if missing) and installs the Docling service
# dependencies into it. Runs unattended from the desktop app on first launch.
# Idempotent - a marker file is written on success so subsequent runs skip.
#
# Usage:
#   powershell -ExecutionPolicy Bypass -File setup-docling.ps1 [requirements.txt]

$ErrorActionPreference = "Stop"

$VenvDir = Join-Path $HOME ".jcowork\venv"
$Marker = Join-Path $VenvDir ".docling-setup-ok"
$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path

if (Test-Path $Marker) {
    Write-Host "Docling Python environment already set up - skipping."
    exit 0
}

# Resolve requirements.txt: explicit arg > bundled layout > repo layout.
$ReqFile = if ($args.Count -ge 1) { $args[0] } else { $null }
if (-not $ReqFile) {
    foreach ($candidate in @(
        (Join-Path $ScriptDir "requirements.txt"),
        (Join-Path $ScriptDir "..\services\docling\requirements.txt")
    )) {
        if (Test-Path $candidate) { $ReqFile = $candidate; break }
    }
}
if (-not $ReqFile -or -not (Test-Path $ReqFile)) {
    Write-Error "requirements.txt not found (looked next to $ScriptDir)"
    exit 1
}
Write-Host "Using requirements file: $ReqFile"

# Detect system Python (py launcher first, then PATH entries).
$SysPython = $null
foreach ($cmd in @("py", "python3", "python")) {
    $found = Get-Command $cmd -ErrorAction SilentlyContinue
    if ($found) { $SysPython = $found.Source; break }
}
if (-not $SysPython) {
    Write-Error "python not found. Install Python 3.10+ first."
    exit 1
}
Write-Host "Using system Python: $SysPython"

New-Item -ItemType Directory -Force -Path $VenvDir | Out-Null
$VenvPython = Join-Path $VenvDir "Scripts\python.exe"

if (-not (Test-Path $VenvPython)) {
    Write-Host "Creating virtual environment at $VenvDir ..."
    & $SysPython -m venv $VenvDir
    if ($LASTEXITCODE -ne 0) { throw "Failed to create virtual environment" }
}

Write-Host "Upgrading pip..."
& $VenvPython -m pip install --upgrade pip --quiet

Write-Host "Installing Docling service dependencies (this may take several minutes)..."
& $VenvPython -m pip install --quiet -r $ReqFile
if ($LASTEXITCODE -ne 0) { throw "Failed to install dependencies" }

# Non-Docling tool dependencies that share this venv:
# playwright -> web_search tool, pdftext -> pdf_parse tool.
Write-Host "Installing tool dependencies (playwright, pdftext)..."
& $VenvPython -m pip install --quiet playwright pdftext
if ($LASTEXITCODE -ne 0) { throw "Failed to install tool dependencies" }

# web_search.py prefers the system Chrome; only download Playwright's
# Chromium when no system browser is available.
$ChromeCandidates = @(
    "$env:ProgramFiles\Google\Chrome\Application\chrome.exe",
    "${env:ProgramFiles(x86)}\Google\Chrome\Application\chrome.exe",
    "$env:LOCALAPPDATA\Google\Chrome\Application\chrome.exe",
    "$env:ProgramFiles\Chromium\Application\chrome.exe"
)
$HasChrome = $false
foreach ($c in $ChromeCandidates) { if (Test-Path $c) { $HasChrome = $true; break } }
if (-not $HasChrome) {
    Write-Host "System Chrome not found - downloading Playwright Chromium..."
    & $VenvPython -m playwright install chromium
    if ($LASTEXITCODE -ne 0) { Write-Host "WARNING: Chromium download failed; web_search needs Chrome" }
}

New-Item -ItemType File -Force -Path $Marker | Out-Null
Write-Host "=== Docling Python environment ready: $VenvDir ==="

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
$PdfMarker = Join-Path $VenvDir ".docling-pdftext-ok"
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

# Detect a *working* system Python. On Windows `Get-Command python` often
# resolves to the Microsoft Store alias stub (WindowsApps\python.exe) which
# merely opens the Store, so each candidate is validated with --version.
function Find-WorkingPython {
    foreach ($cmd in @("py", "python3", "python")) {
        $found = Get-Command $cmd -ErrorAction SilentlyContinue
        if (-not $found) { continue }
        if ($found.Source -like "*\WindowsApps\*") { continue }  # Store stub
        try {
            $ver = & $found.Source --version 2>&1
            if ($LASTEXITCODE -eq 0 -and "$ver" -match "Python\s+3\.") {
                return $found.Source
            }
        } catch {}
    }
    return $null
}

$SysPython = Find-WorkingPython

# No usable Python: install it automatically so the packaged app works on a
# fresh machine (the Tauri installer does not run scripts/install.ps1).
if (-not $SysPython) {
    if (Get-Command winget -ErrorAction SilentlyContinue) {
        Write-Host "Python not found - installing Python 3.12 via winget..."
        winget install --id Python.Python.3.12 -e --accept-source-agreements --accept-package-agreements --silent
        # Refresh this session's PATH so the new interpreter is discoverable.
        $env:Path = [System.Environment]::GetEnvironmentVariable("Path", "Machine") + ";" +
                    [System.Environment]::GetEnvironmentVariable("Path", "User")
        $SysPython = Find-WorkingPython
    }
}

if (-not $SysPython) {
    Write-Error "python not found and automatic install failed. Install Python 3.10+ from https://python.org manually, then restart the app."
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

# Phase 1 (fast): lightweight PDF text extraction.
# pdftext (pypdfium2-based, no ML models) installs in seconds and lets the app
# parse PDFs immediately, while the heavy Docling stack downloads in phase 2.
# A partial marker is written so the backend can offer pdftext fallback early.
Write-Host "Installing lightweight PDF parser (pdftext)..."
& $VenvPython -m pip install --quiet pdftext
if ($LASTEXITCODE -eq 0) {
    New-Item -ItemType File -Force -Path $PdfMarker | Out-Null
    Write-Host "pdftext ready - basic PDF parsing is now available."
} else {
    Write-Host "WARNING: pdftext install failed; continuing with Docling setup"
}

# Phase 1b: playwright (web_search tool). Skipped when a system Chrome exists.
Write-Host "Installing playwright..."
& $VenvPython -m pip install --quiet playwright

Write-Host "Installing Docling service dependencies (this may take several minutes)..."
& $VenvPython -m pip install --quiet -r $ReqFile
if ($LASTEXITCODE -ne 0) { throw "Failed to install dependencies" }

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

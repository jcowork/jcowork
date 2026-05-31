# Jcowork Python Environment Setup (Windows)
# Creates ~/.jcowork/venv with playwright + pdftext installed.
#
# Usage:
#   powershell -ExecutionPolicy Bypass -File scripts\setup-python.ps1

$ErrorActionPreference = "Stop"

$VenvDir = Join-Path $env:USERPROFILE ".jcowork\venv"
$Python = Join-Path $VenvDir "Scripts\python.exe"

Write-Host "=== Jcowork Python Environment Setup ===" -ForegroundColor Cyan
Write-Host "Venv directory: $VenvDir"

# Detect system Python
$SysPython = $null
foreach ($cmd in @("python", "python3", "py")) {
    try {
        $result = Get-Command $cmd -ErrorAction SilentlyContinue
        if ($result) {
            $SysPython = $result.Source
            break
        }
    } catch {}
}

if (-not $SysPython) {
    Write-Host "ERROR: Python not found. Install Python 3.12+ from https://python.org" -ForegroundColor Red
    exit 1
}

$version = & $SysPython --version 2>&1
Write-Host "Using system Python: $SysPython ($version)"

# Create venv if it doesn't exist
if (-not (Test-Path $Python)) {
    Write-Host "Creating virtual environment..."
    & $SysPython -m venv $VenvDir
}

# Upgrade pip
Write-Host "Upgrading pip..."
$pip = Join-Path $VenvDir "Scripts\pip.exe"
& $pip install --upgrade pip --quiet

# Install packages
Write-Host "Installing Python packages (playwright, pdftext)..."
& $pip install --quiet playwright pdftext

# Install Playwright Chromium browser
Write-Host "Installing Playwright Chromium browser..."
& (Join-Path $VenvDir "Scripts\playwright.exe") install chromium

Write-Host ""
Write-Host "=== Setup Complete ===" -ForegroundColor Green
Write-Host "Python venv: $VenvDir"
Write-Host "Python binary: $Python"

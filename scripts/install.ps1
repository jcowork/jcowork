# Jcowork one-click installer for Windows 11 (PowerShell).
#
# Installs all dependencies (Rust + MSVC build tools, Python 3.12, Node LTS,
# Python venv, Playwright Chromium), builds backend and frontend, and writes
# a default .env configuration.
#
# Usage (in PowerShell):
#   powershell -ExecutionPolicy Bypass -File scripts\install.ps1
#   powershell -ExecutionPolicy Bypass -File scripts\install.ps1 -Start
#
# After installation:
#   powershell -ExecutionPolicy Bypass -File scripts\start.ps1
#   powershell -ExecutionPolicy Bypass -File scripts\stop.ps1

param(
    [switch]$Start
)

$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
Set-Location $Root

function Info($msg) { Write-Host "==> $msg" -ForegroundColor Cyan }
function Ok($msg)   { Write-Host "  ✓ $msg" -ForegroundColor Green }
function Warn($msg) { Write-Host "  ! $msg" -ForegroundColor Yellow }

function Refresh-SessionPath {
    $env:Path = [System.Environment]::GetEnvironmentVariable("Path", "Machine") + ";" +
                [System.Environment]::GetEnvironmentVariable("Path", "User")
}

function Test-Command($name) {
    return $null -ne (Get-Command $name -ErrorAction SilentlyContinue)
}

if (-not (Test-Command "winget")) {
    Write-Host "ERROR: winget not found. Install 'App Installer' from the Microsoft Store first." -ForegroundColor Red
    exit 1
}

# ---------------------------------------------------------------------------
# 1. System dependencies
# ---------------------------------------------------------------------------
Info "Step 1/5: Checking system dependencies"

# Python 3.12+
if (Test-Command "python") {
    Ok "Python already installed ($((& python --version) 2>&1))"
} else {
    Info "Installing Python 3.12..."
    winget install --id Python.Python.3.12 -e --accept-source-agreements --accept-package-agreements --silent
    Refresh-SessionPath
}

# Node 20+
$nodeOk = $false
if (Test-Command "node") {
    $major = [int]((& node --version).TrimStart('v').Split('.')[0])
    if ($major -ge 20) { $nodeOk = $true; Ok "Node already installed ($((& node --version)))" }
}
if (-not $nodeOk) {
    Info "Installing Node.js LTS..."
    winget install --id OpenJS.NodeJS.LTS -e --accept-source-agreements --accept-package-agreements --silent
    Refresh-SessionPath
}

# Rust (needs MSVC build tools for the linker)
if (Test-Command "cargo") {
    Ok "Rust already installed ($((& cargo --version)))"
} else {
    Info "Installing Visual Studio Build Tools (C++ workload, large download)..."
    winget install --id Microsoft.VisualStudio.2022.BuildTools -e --accept-source-agreements --accept-package-agreements `
        --override "--add Microsoft.VisualStudio.Workload.VCTools --includeRecommended --passive --wait"
    Info "Installing Rust via rustup..."
    winget install --id Rustlang.Rustup -e --accept-source-agreements --accept-package-agreements --silent
    Refresh-SessionPath
    $env:Path += ";$env:USERPROFILE\.cargo\bin"
}

# ---------------------------------------------------------------------------
# 2. Python virtual environment (~/.jcowork/venv)
# ---------------------------------------------------------------------------
Info "Step 2/5: Setting up Python environment (venv + docling + playwright)"
& powershell -ExecutionPolicy Bypass -File "$Root\scripts\setup-python.ps1"
if ($LASTEXITCODE -ne 0) { throw "Python environment setup failed" }

# ---------------------------------------------------------------------------
# 3. Configuration (.env)
# ---------------------------------------------------------------------------
Info "Step 3/5: Preparing configuration"
New-Item -ItemType Directory -Force -Path "$env:USERPROFILE\.jcowork\data", "$env:USERPROFILE\.jcowork\logs", "$env:USERPROFILE\.jcowork\run" | Out-Null

if (-not (Test-Path "$Root\.env")) {
    Copy-Item "$Root\.env.example" "$Root\.env"
    $secret = -join ((1..64) | ForEach-Object { "0123456789abcdef"[(Get-Random -Maximum 16)] })
    (Get-Content "$Root\.env") -replace '^JCWORK_JWT_SECRET=.*', "JCWORK_JWT_SECRET=$secret" | Set-Content "$Root\.env"
    Ok "Created .env with a random JWT secret"
    Warn "Edit $Root\.env and fill in at least one LLM API key (DEEPSEEK_API_KEY / QWEN_API_KEY / MOONSHOT_API_KEY)"
} else {
    Ok ".env already exists"
}

# ---------------------------------------------------------------------------
# 4. Build backend (Rust, release)
# ---------------------------------------------------------------------------
Info "Step 4/5: Building backend (cargo build --release, first build takes a while)"
& cargo build --release --workspace
if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }
Ok "Backend built: target\release\jcowork.exe"

# ---------------------------------------------------------------------------
# 5. Build frontend (web\dist)
# ---------------------------------------------------------------------------
Info "Step 5/5: Building frontend"
Set-Location "$Root\web"
if (Test-Path "package-lock.json") { & npm ci --no-audit --no-fund } else { & npm install --no-audit --no-fund }
if ($LASTEXITCODE -ne 0) { Set-Location $Root; throw "npm install failed" }
& npm run build
if ($LASTEXITCODE -ne 0) { Set-Location $Root; throw "frontend build failed" }
Set-Location $Root
Ok "Frontend built: web\dist"

# ---------------------------------------------------------------------------
# Done
# ---------------------------------------------------------------------------
Write-Host ""
Info "Installation complete!"
Write-Host ""
Write-Host "  Start services:   powershell -ExecutionPolicy Bypass -File scripts\start.ps1"
Write-Host "  Stop services:    powershell -ExecutionPolicy Bypass -File scripts\stop.ps1"
Write-Host "  Web UI:           http://localhost:3000"
Write-Host ""
Warn "Make sure at least one LLM API key is set in $Root\.env"

if ($Start) {
    Write-Host ""
    & powershell -ExecutionPolicy Bypass -File "$Root\scripts\start.ps1"
}

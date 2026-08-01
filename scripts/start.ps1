# Start jcowork services on Windows: docling (port 50060) + main server (port 3000).
#
# Usage:
#   powershell -ExecutionPolicy Bypass -File scripts\start.ps1

$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
$VenvPy = "$env:USERPROFILE\.jcowork\venv\Scripts\python.exe"
$RunDir = "$env:USERPROFILE\.jcowork\run"
$LogDir = "$env:USERPROFILE\.jcowork\logs"
$AssetsDir = "$env:USERPROFILE\.jcowork\data\docling_assets"

New-Item -ItemType Directory -Force -Path $RunDir, $LogDir, $AssetsDir | Out-Null

function Info($msg) { Write-Host "==> $msg" -ForegroundColor Cyan }
function Ok($msg)   { Write-Host "  ✓ $msg" -ForegroundColor Green }
function Warn($msg) { Write-Host "  ! $msg" -ForegroundColor Yellow }

function Test-PidRunning($pidFile) {
    if (Test-Path $pidFile) {
        $pidValue = [int](Get-Content $pidFile)
        $proc = Get-Process -Id $pidValue -ErrorAction SilentlyContinue
        return $null -ne $proc
    }
    return $false
}

# ---------------------------------------------------------------------------
# Docling service
# ---------------------------------------------------------------------------
if (-not (Test-Path $VenvPy)) {
    Write-Host "ERROR: Python venv not found at $VenvPy — run scripts\install.ps1 first" -ForegroundColor Red
    exit 1
}

if (Test-PidRunning "$RunDir\docling.pid") {
    Ok "docling already running (pid $(Get-Content "$RunDir\docling.pid"))"
} else {
    Info "Starting docling service on port 50060..."
    # Child process inherits these environment variables
    $env:ASSETS_DIR = $AssetsDir
    $env:PORT = "50060"
    $proc = Start-Process -FilePath $VenvPy `
        -ArgumentList "-m","uvicorn","app:app","--host","127.0.0.1","--port","50060" `
        -WorkingDirectory "$Root\services\docling" `
        -WindowStyle Hidden `
        -RedirectStandardOutput "$LogDir\docling.out.log" `
        -RedirectStandardError "$LogDir\docling.err.log" `
        -PassThru
    $proc.Id | Set-Content "$RunDir\docling.pid"
}

Info "Waiting for docling to become healthy (first run downloads the model, may take minutes)..."
$healthy = $false
foreach ($i in 1..100) {
    try {
        $null = Invoke-WebRequest -Uri "http://127.0.0.1:50060/health" -TimeoutSec 2 -UseBasicParsing
        $healthy = $true
        break
    } catch {
        Start-Sleep -Seconds 3
    }
}
if ($healthy) { Ok "docling is healthy" } else {
    Warn "docling not healthy yet — it may still be downloading the embedding model."
    Warn "Check progress: Get-Content $LogDir\docling.err.log -Wait"
}

# ---------------------------------------------------------------------------
# Main server
# ---------------------------------------------------------------------------
$Binary = "$Root\target\release\jcowork.exe"
if (-not (Test-Path $Binary)) { $Binary = "$Root\target\debug\jcowork.exe" }
if (-not (Test-Path $Binary)) {
    Write-Host "ERROR: jcowork binary not found — run scripts\install.ps1 (or cargo build) first" -ForegroundColor Red
    exit 1
}

if (Test-PidRunning "$RunDir\server.pid") {
    Ok "jcowork already running (pid $(Get-Content "$RunDir\server.pid"))"
} else {
    Info "Starting jcowork server on port 3000..."
    $proc = Start-Process -FilePath $Binary `
        -WorkingDirectory $Root `
        -WindowStyle Hidden `
        -RedirectStandardOutput "$LogDir\server.out.log" `
        -RedirectStandardError "$LogDir\server.err.log" `
        -PassThru
    $proc.Id | Set-Content "$RunDir\server.pid"
    Start-Sleep -Seconds 2
    if (Test-PidRunning "$RunDir\server.pid") {
        Ok "jcowork started (pid $(Get-Content "$RunDir\server.pid"))"
    } else {
        Write-Host "ERROR: jcowork failed to start — check $LogDir\server.err.log" -ForegroundColor Red
        exit 1
    }
}

Write-Host ""
Info "Services are up:"
Write-Host "  Web UI:     http://localhost:3000"
Write-Host "  Logs:       $LogDir\server.out.log, $LogDir\docling.err.log"
Write-Host "  Stop:       powershell -ExecutionPolicy Bypass -File scripts\stop.ps1"

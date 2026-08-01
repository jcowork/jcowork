# Stop jcowork services started by scripts\start.ps1.
#
# Usage:
#   powershell -ExecutionPolicy Bypass -File scripts\stop.ps1

$ErrorActionPreference = "Continue"
$RunDir = "$env:USERPROFILE\.jcowork\run"

function Info($msg) { Write-Host "==> $msg" -ForegroundColor Cyan }
function Ok($msg)   { Write-Host "  ✓ $msg" -ForegroundColor Green }

function Stop-One($name, $pidFile) {
    if (Test-Path $pidFile) {
        $pidValue = [int](Get-Content $pidFile)
        $proc = Get-Process -Id $pidValue -ErrorAction SilentlyContinue
        if ($proc) {
            Info "Stopping $name (pid $pidValue)..."
            Stop-Process -Id $pidValue -Force -ErrorAction SilentlyContinue
            Ok "$name stopped"
        } else {
            Ok "$name was not running"
        }
        Remove-Item $pidFile -Force -ErrorAction SilentlyContinue
    } else {
        Ok "$name was not running"
    }
}

Stop-One "jcowork server" "$RunDir\server.pid"
Stop-One "docling service" "$RunDir\docling.pid"

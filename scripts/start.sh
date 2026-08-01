#!/usr/bin/env bash
# Start jcowork services: docling (PDF/embedding, port 50060) + main server (port 3000).
#
# Usage:
#   bash scripts/start.sh

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VENV_PY="$HOME/.jcowork/venv/bin/python"
RUN_DIR="$HOME/.jcowork/run"
LOG_DIR="$HOME/.jcowork/logs"
ASSETS_DIR="$HOME/.jcowork/data/docling_assets"

mkdir -p "$RUN_DIR" "$LOG_DIR" "$ASSETS_DIR"

info() { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
ok()   { printf '\033[1;32m  ✓\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33m  !\033[0m %s\n' "$*" >&2; }
die()  { printf '\033[1;31mERROR:\033[0m %s\n' "$*" >&2; exit 1; }

is_running() {
    # $1 = pid file
    [ -f "$1" ] && kill -0 "$(cat "$1")" 2>/dev/null
}

# ---------------------------------------------------------------------------
# Docling service (PDF parsing & embeddings)
# ---------------------------------------------------------------------------
if [ ! -x "$VENV_PY" ]; then
    die "Python venv not found at $VENV_PY — run scripts/install.sh first"
fi

if is_running "$RUN_DIR/docling.pid"; then
    ok "docling already running (pid $(cat "$RUN_DIR/docling.pid"))"
else
    info "Starting docling service on port 50060..."
    cd "$ROOT/services/docling"
    ASSETS_DIR="$ASSETS_DIR" PORT=50060 \
        nohup "$VENV_PY" -m uvicorn app:app --host 127.0.0.1 --port 50060 \
        >> "$LOG_DIR/docling.log" 2>&1 &
    echo $! > "$RUN_DIR/docling.pid"
    cd "$ROOT"
fi

# Wait for docling health (first start downloads the embedding model, be patient)
info "Waiting for docling to become healthy (first run downloads the model, may take minutes)..."
HEALTHY=false
for _ in $(seq 1 100); do
    if curl -fsS -m 2 http://127.0.0.1:50060/health &>/dev/null; then
        HEALTHY=true
        break
    fi
    sleep 3
done
if $HEALTHY; then
    ok "docling is healthy"
else
    warn "docling not healthy yet — it may still be downloading the embedding model."
    warn "Check progress: tail -f $LOG_DIR/docling.log"
fi

# ---------------------------------------------------------------------------
# Main server (jcowork, port 3000)
# ---------------------------------------------------------------------------
BINARY="$ROOT/target/release/jcowork"
if [ ! -x "$BINARY" ]; then
    # Fall back to debug build for development checkouts
    BINARY="$ROOT/target/debug/jcowork"
fi
[ -x "$BINARY" ] || die "jcowork binary not found — run scripts/install.sh (or cargo build) first"

if is_running "$RUN_DIR/server.pid"; then
    ok "jcowork already running (pid $(cat "$RUN_DIR/server.pid"))"
else
    info "Starting jcowork server on port 3000..."
    cd "$ROOT"   # .env is loaded from the working directory
    nohup "$BINARY" >> "$LOG_DIR/server.log" 2>&1 &
    echo $! > "$RUN_DIR/server.pid"
    sleep 2
    if is_running "$RUN_DIR/server.pid"; then
        ok "jcowork started (pid $(cat "$RUN_DIR/server.pid"))"
    else
        die "jcowork failed to start — check $LOG_DIR/server.log"
    fi
fi

echo ""
info "Services are up:"
echo "  Web UI:     http://localhost:3000"
echo "  Logs:       tail -f $LOG_DIR/server.log  |  tail -f $LOG_DIR/docling.log"
echo "  Stop:       bash scripts/stop.sh"

#!/usr/bin/env bash
# Stop jcowork services started by scripts/start.sh.

set -euo pipefail

RUN_DIR="$HOME/.jcowork/run"

info() { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
ok()   { printf '\033[1;32m  ✓\033[0m %s\n' "$*"; }

stop_one() {
    # $1 = name, $2 = pid file
    local name="$1" pidfile="$2"
    if [ -f "$pidfile" ]; then
        local pid
        pid="$(cat "$pidfile")"
        if kill -0 "$pid" 2>/dev/null; then
            info "Stopping $name (pid $pid)..."
            kill "$pid" 2>/dev/null || true
            for _ in $(seq 1 10); do
                kill -0 "$pid" 2>/dev/null || break
                sleep 1
            done
            kill -9 "$pid" 2>/dev/null || true
            ok "$name stopped"
        else
            ok "$name was not running"
        fi
        rm -f "$pidfile"
    else
        ok "$name was not running"
    fi
}

stop_one "jcowork server" "$RUN_DIR/server.pid"
stop_one "docling service" "$RUN_DIR/docling.pid"

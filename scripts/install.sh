#!/usr/bin/env bash
# Jcowork one-click installer for macOS and Ubuntu 24.04+.
#
# Installs all dependencies (Rust, Python 3.12+, Node 20+, Python venv,
# Playwright Chromium), builds the backend and frontend, and writes a
# default .env configuration.
#
# Usage:
#   bash scripts/install.sh            # install everything
#   bash scripts/install.sh --start    # install, then start services
#
# After installation:
#   bash scripts/start.sh              # start jcowork + docling services
#   bash scripts/stop.sh               # stop them

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

START_AFTER=false
[ "${1:-}" = "--start" ] && START_AFTER=true

info()  { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
ok()    { printf '\033[1;32m  ✓\033[0m %s\n' "$*"; }
warn()  { printf '\033[1;33m  !\033[0m %s\n' "$*" >&2; }
die()   { printf '\033[1;31mERROR:\033[0m %s\n' "$*" >&2; exit 1; }

OS="$(uname -s)"
case "$OS" in
    Darwin|Linux) ;;
    *) die "Unsupported OS: $OS (use scripts\\install.ps1 on Windows)" ;;
esac

# ---------------------------------------------------------------------------
# 1. System dependencies
# ---------------------------------------------------------------------------
info "Step 1/5: Checking system dependencies ($OS)"

install_rust() {
    if command -v cargo &>/dev/null; then
        ok "Rust already installed ($(cargo --version))"
    else
        info "Installing Rust via rustup..."
        curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --quiet
        # shellcheck source=/dev/null
        . "$HOME/.cargo/env"
        ok "Rust installed ($(cargo --version))"
    fi
}

if [ "$OS" = "Darwin" ]; then
    # Homebrew
    BREW=""
    for candidate in /opt/homebrew/bin/brew /usr/local/bin/brew; do
        [ -x "$candidate" ] && BREW="$candidate" && break
    done
    if [ -z "$BREW" ] && command -v brew &>/dev/null; then BREW="$(command -v brew)"; fi
    if [ -z "$BREW" ]; then
        info "Installing Homebrew (may ask for your password)..."
        /bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)"
        for candidate in /opt/homebrew/bin/brew /usr/local/bin/brew; do
            [ -x "$candidate" ] && BREW="$candidate" && break
        done
        [ -z "$BREW" ] && die "Homebrew installation failed"
        eval "$("$BREW" shellenv)"
    fi
    ok "Homebrew: $("$BREW" --version | head -1)"

    # Python 3.12+
    if command -v python3.12 &>/dev/null || command -v python3.13 &>/dev/null; then
        ok "Python already installed"
    else
        info "Installing Python 3.12..."
        "$BREW" install python@3.12
    fi

    # Node 20+
    NODE_MAJOR="$(node --version 2>/dev/null | sed 's/v\([0-9]*\).*/\1/' || echo 0)"
    if [ "${NODE_MAJOR:-0}" -ge 20 ]; then
        ok "Node already installed ($(node --version))"
    else
        info "Installing Node.js..."
        "$BREW" install node
    fi

    install_rust

elif [ "$OS" = "Linux" ]; then
    command -v apt-get &>/dev/null || die "Only Debian/Ubuntu (apt-get) is supported by this script"

    info "Installing build tools and Python (requires sudo)..."
    sudo apt-get update -qq
    sudo apt-get install -y -qq build-essential pkg-config libssl-dev curl ca-certificates \
        python3 python3-venv python3-pip

    # Node 20+ (Ubuntu 24.04 ships Node 18, which is too old for Vite 8)
    NODE_MAJOR="$(node --version 2>/dev/null | sed 's/v\([0-9]*\).*/\1/' || echo 0)"
    if [ "${NODE_MAJOR:-0}" -ge 20 ]; then
        ok "Node already installed ($(node --version))"
    else
        info "Installing Node.js 22 (NodeSource)..."
        curl -fsSL https://deb.nodesource.com/setup_22.x | sudo -E bash -
        sudo apt-get install -y -qq nodejs
    fi

    install_rust
fi

# ---------------------------------------------------------------------------
# 2. Python virtual environment (~/.jcowork/venv)
# ---------------------------------------------------------------------------
info "Step 2/5: Setting up Python environment (venv + docling + playwright)"
bash "$ROOT/scripts/setup-python.sh"

# ---------------------------------------------------------------------------
# 3. Configuration (.env)
# ---------------------------------------------------------------------------
info "Step 3/5: Preparing configuration"
mkdir -p "$HOME/.jcowork/data" "$HOME/.jcowork/logs" "$HOME/.jcowork/run"

if [ ! -f "$ROOT/.env" ]; then
    cp "$ROOT/.env.example" "$ROOT/.env"
    # Generate a random JWT secret
    SECRET="$(openssl rand -hex 32 2>/dev/null || python3 -c 'import secrets; print(secrets.token_hex(32))')"
    # Portable in-place edit (works on both GNU and BSD sed)
    sed -i.bak "s/^JCWORK_JWT_SECRET=.*/JCWORK_JWT_SECRET=${SECRET}/" "$ROOT/.env" && rm -f "$ROOT/.env.bak"
    ok "Created .env with a random JWT secret"
    warn "Edit $ROOT/.env and fill in at least one LLM API key (DEEPSEEK_API_KEY / QWEN_API_KEY / MOONSHOT_API_KEY)"
else
    ok ".env already exists"
fi

# ---------------------------------------------------------------------------
# 4. Build backend (Rust, release)
# ---------------------------------------------------------------------------
info "Step 4/5: Building backend (cargo build --release, first build takes a while)"
# shellcheck source=/dev/null
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
cargo build --release --workspace
ok "Backend built: target/release/jcowork"

# ---------------------------------------------------------------------------
# 5. Build frontend (web/dist)
# ---------------------------------------------------------------------------
info "Step 5/5: Building frontend"
cd "$ROOT/web"
if [ -f package-lock.json ]; then
    npm ci --no-audit --no-fund
else
    npm install --no-audit --no-fund
fi
npm run build
cd "$ROOT"
ok "Frontend built: web/dist"

# ---------------------------------------------------------------------------
# Done
# ---------------------------------------------------------------------------
echo ""
info "Installation complete!"
echo ""
echo "  Start services:   bash scripts/start.sh"
echo "  Stop services:    bash scripts/stop.sh"
echo "  Web UI:           http://localhost:3000"
echo ""
warn "Make sure at least one LLM API key is set in $ROOT/.env"

if $START_AFTER; then
    echo ""
    bash "$ROOT/scripts/start.sh"
fi

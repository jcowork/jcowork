#!/usr/bin/env bash
# Setup Python virtual environment for jcowork tools (web_search, pdf_parse).
# Creates ~/.jcowork/venv with playwright + pdftext installed.
#
# Usage:
#   bash scripts/setup-python.sh

set -euo pipefail

VENV_DIR="${HOME}/.jcowork/venv"
PYTHON="${VENV_DIR}/bin/python"

echo "=== Jcowork Python Environment Setup ==="
echo "Venv directory: ${VENV_DIR}"

# Detect system Python (prefer 3.12+ for compatibility)
detect_python() {
    for cmd in python3.12 python3.13 python3.14 python3; do
        if command -v "$cmd" &>/dev/null; then
            echo "$cmd"
            return
        fi
    done
    echo "ERROR: python3 not found. Install Python 3.12+ first." >&2
    exit 1
}

SYS_PYTHON=$(detect_python)
echo "Using system Python: ${SYS_PYTHON} ($(${SYS_PYTHON} --version 2>&1))"

# Install python3-venv on Debian/Ubuntu if missing
if ! ${SYS_PYTHON} -m venv --help &>/dev/null; then
    echo "Installing python3-venv (requires sudo)..."
    if command -v apt-get &>/dev/null; then
        sudo apt-get update -qq && sudo apt-get install -y -qq python3-venv
    elif command -v dnf &>/dev/null; then
        sudo dnf install -y python3-venv
    else
        echo "WARNING: Cannot install python3-venv automatically. Install it manually." >&2
    fi
fi

# Create venv if it doesn't exist
if [ ! -f "${PYTHON}" ]; then
    echo "Creating virtual environment..."
    "${SYS_PYTHON}" -m venv "${VENV_DIR}"
fi

# Upgrade pip
echo "Upgrading pip..."
"${VENV_DIR}/bin/pip" install --upgrade pip --quiet

# Install packages
echo "Installing Python packages (playwright, pdftext)..."
"${VENV_DIR}/bin/pip" install --quiet playwright pdftext

# Install Playwright browsers with system dependencies
# --with-deps auto-installs OS libs (libatk, libnspr, etc.) on Linux
echo "Installing Playwright Chromium browser (+ system dependencies)..."
"${VENV_DIR}/bin/playwright" install --with-deps chromium

echo ""
echo "=== Setup Complete ==="
echo "Python venv: ${VENV_DIR}"
echo "Python binary: ${PYTHON}"
echo "Playwright version: $(${VENV_DIR}/bin/python -c 'import playwright; print(playwright.__version__)' 2>/dev/null || echo 'not installed')"

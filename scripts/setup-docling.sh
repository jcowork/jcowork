#!/usr/bin/env bash
# Bootstrap the Python environment required by the bundled Docling service.
#
# Creates ~/.jcowork/venv (if missing) and installs the Docling service
# dependencies into it. Designed to run unattended from the desktop app on
# first launch ("install the app and it just works"). Idempotent — a marker
# file is written on success so subsequent runs are skipped.
#
# Usage:
#   bash setup-docling.sh [path/to/requirements.txt]

set -euo pipefail

VENV_DIR="${HOME}/.jcowork/venv"
MARKER="${VENV_DIR}/.docling-setup-ok"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

# Resolve requirements.txt: explicit arg > next to this script (bundled
# layout puts services/docling/ alongside scripts/) > repo layout.
REQ_FILE="${1:-}"
if [ -z "${REQ_FILE}" ]; then
    for candidate in \
        "${SCRIPT_DIR}/requirements.txt" \
        "${SCRIPT_DIR}/../services/docling/requirements.txt"; do
        if [ -f "${candidate}" ]; then
            REQ_FILE="${candidate}"
            break
        fi
    done
fi
if [ -z "${REQ_FILE}" ] || [ ! -f "${REQ_FILE}" ]; then
    echo "ERROR: requirements.txt not found (looked next to ${SCRIPT_DIR})" >&2
    exit 1
fi
echo "Using requirements file: ${REQ_FILE}"

if [ -f "${MARKER}" ]; then
    echo "Docling Python environment already set up — skipping."
    exit 0
fi

# Detect system Python (prefer newer versions for dependency compatibility).
detect_python() {
    for cmd in python3.12 python3.13 python3.14 python3; do
        if command -v "${cmd}" &>/dev/null; then
            echo "${cmd}"
            return
        fi
    done
    echo "ERROR: python3 not found. Install Python 3.10+ first." >&2
    exit 1
}

SYS_PYTHON="$(detect_python)"
echo "Using system Python: ${SYS_PYTHON} ($("${SYS_PYTHON}" --version 2>&1))"

mkdir -p "${VENV_DIR}"

if [ ! -x "${VENV_DIR}/bin/python" ]; then
    echo "Creating virtual environment at ${VENV_DIR} ..."
    "${SYS_PYTHON}" -m venv "${VENV_DIR}"
fi

echo "Upgrading pip..."
"${VENV_DIR}/bin/python" -m pip install --upgrade pip --quiet

echo "Installing Docling service dependencies (this may take several minutes)..."
"${VENV_DIR}/bin/python" -m pip install --quiet -r "${REQ_FILE}"

touch "${MARKER}"
echo "=== Docling Python environment ready: ${VENV_DIR} ==="

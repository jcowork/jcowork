#!/usr/bin/env bash
# Run the local voice sidecar (sets it up first if needed).
#
# Usage:
#   bash scripts/start-voice-lite.sh          # foreground
#   VOICE_LITE_PORT=50061 ...                 # env overrides honored

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
VENV_DIR="${HOME}/.jcowork/venv-voice-lite"

if [ ! -f "${VENV_DIR}/.voice-lite-setup-ok" ]; then
    bash "${SCRIPT_DIR}/setup-voice-lite.sh"
fi

exec "${VENV_DIR}/bin/python" "${SCRIPT_DIR}/../services/voice-lite/app.py"
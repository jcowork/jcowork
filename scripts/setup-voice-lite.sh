#!/usr/bin/env bash
# Bootstrap the local voice sidecar (Plan B: sherpa-onnx, pure CPU).
#
# Creates ~/.jcowork/venv-voice-lite, installs services/voice-lite deps and
# downloads the speech models into $JCWORK_DATA_DIR/models/voice-lite.
# Idempotent — marker files skip completed steps.
#
# Usage:
#   bash scripts/setup-voice-lite.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
DATA_DIR="${JCWORK_DATA_DIR:-${HOME}/.jcowork/data}"
VENV_DIR="${HOME}/.jcowork/venv-voice-lite"
MODELS_DIR="${DATA_DIR}/models/voice-lite"
REQ_FILE="${SCRIPT_DIR}/../services/voice-lite/requirements.txt"
MARKER="${VENV_DIR}/.voice-lite-setup-ok"

ASR_NAME="sherpa-onnx-streaming-paraformer-bilingual-zh-en"
ASR_FALLBACK_NAME="sherpa-onnx-streaming-zipformer-bilingual-zh-en-2023-02-20"
TTS_NAME="vits-melo-tts-zh_en"
RELEASE_BASE="https://github.com/k2-fsa/sherpa-onnx/releases/download"

detect_python() {
    for cmd in python3.12 python3.13 python3.11 python3; do
        if command -v "${cmd}" &>/dev/null; then
            echo "${cmd}"
            return
        fi
    done
    echo "ERROR: python3 not found. Install Python 3.10+ first." >&2
    exit 1
}

# ── Python environment ──
if [ -f "${MARKER}" ]; then
    echo "Voice-lite Python environment already set up — skipping."
else
    SYS_PYTHON="$(detect_python)"
    echo "Using system Python: ${SYS_PYTHON}"
    if [ ! -x "${VENV_DIR}/bin/python" ]; then
        "${SYS_PYTHON}" -m venv "${VENV_DIR}"
    fi
    "${VENV_DIR}/bin/pip" install --quiet --upgrade pip
    "${VENV_DIR}/bin/pip" install --quiet -r "${REQ_FILE}"
    touch "${MARKER}"
    echo "Voice-lite dependencies installed."
fi

# ── Models ──
mkdir -p "${MODELS_DIR}"

fetch_extract() {
    local url="$1" name="$2"
    if [ -d "${MODELS_DIR}/${name}" ]; then
        echo "Model ${name} already present — skipping."
        return
    fi
    echo "Downloading ${name} ..."
    local tmp="${MODELS_DIR}/${name}.tar.bz2"
    curl -fL --retry 3 -o "${tmp}" "${url}"
    tar -xjf "${tmp}" -C "${MODELS_DIR}"
    rm -f "${tmp}"
}

fetch_extract "${RELEASE_BASE}/asr-models/${ASR_NAME}.tar.bz2" "${ASR_NAME}"
fetch_extract "${RELEASE_BASE}/asr-models/${ASR_FALLBACK_NAME}.tar.bz2" "${ASR_FALLBACK_NAME}"
fetch_extract "${RELEASE_BASE}/tts-models/${TTS_NAME}.tar.bz2" "${TTS_NAME}"

if [ ! -f "${MODELS_DIR}/silero_vad.onnx" ]; then
    echo "Downloading silero_vad.onnx ..."
    curl -fL --retry 3 -o "${MODELS_DIR}/silero_vad.onnx" \
        "${RELEASE_BASE}/asr-models/silero_vad.onnx"
fi

echo ""
echo "Voice-lite ready. Start it with:"
echo "  bash scripts/start-voice-lite.sh"
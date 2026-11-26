#!/usr/bin/env bash
# Bootstrap the local voice sidecar (Plan B: local speech).
#
# Creates ~/.jcowork/venv-voice-lite, installs services/voice-lite deps and
# downloads the speech models into $JCWORK_DATA_DIR/models/voice-lite.
# On Apple Silicon the primary ASR is Qwen3-ASR-1.7B (5-bit MLX) and the
# primary TTS is Kokoro-82M-v1.1-zh (MLX); elsewhere (or when the MLX models
# are absent) sherpa-onnx streaming ASR / melo TTS take over.
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
MARKER="${VENV_DIR}/.voice-lite-setup-ok-mlx2"

ASR_NAME="sherpa-onnx-streaming-paraformer-bilingual-zh-en"
ASR_FALLBACK_NAME="sherpa-onnx-streaming-zipformer-bilingual-zh-en-2023-02-20"
QWEN_NAME="Qwen3-ASR-1.7B-5bit"
KOKORO_NAME="Kokoro-82M-v1.1-zh-mlx"
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

# Qwen3-ASR (MLX) only runs on Apple Silicon; other platforms keep sherpa ASR.
if [ "$(uname -s)" = "Darwin" ] && [ "$(uname -m)" = "arm64" ]; then
    if [ -f "${MODELS_DIR}/${QWEN_NAME}/config.json" ]; then
        echo "Model ${QWEN_NAME} already present — skipping."
    else
        echo "Downloading ${QWEN_NAME} (MLX, ~1.2 GB) ..."
        "${VENV_DIR}/bin/python" - "${MODELS_DIR}" "${QWEN_NAME}" <<'PY'
import os, sys
from huggingface_hub import snapshot_download
models_dir, name = sys.argv[1], sys.argv[2]
try:
    snapshot_download(f"mlx-community/{name}", local_dir=os.path.join(models_dir, name), max_workers=4)
except Exception:
    # huggingface.co is unreachable from some networks; the mirror carries
    # the same mlx-community repos.
    os.environ["HF_ENDPOINT"] = "https://hf-mirror.com"
    snapshot_download(f"mlx-community/{name}", local_dir=os.path.join(models_dir, name), max_workers=4)
PY
    fi
    if [ -f "${MODELS_DIR}/${KOKORO_NAME}/config.json" ]; then
        echo "Model ${KOKORO_NAME} already present — skipping."
    else
        echo "Downloading ${KOKORO_NAME} (MLX zh TTS, ~350 MB) ..."
        "${VENV_DIR}/bin/python" - "${MODELS_DIR}" "${KOKORO_NAME}" <<'PY'
import os, sys
from huggingface_hub import snapshot_download
models_dir, name = sys.argv[1], sys.argv[2]
try:
    snapshot_download(f"FluidInference/{name.lower()}", local_dir=os.path.join(models_dir, name), max_workers=4)
except Exception:
    os.environ["HF_ENDPOINT"] = "https://hf-mirror.com"
    snapshot_download(f"FluidInference/{name.lower()}", local_dir=os.path.join(models_dir, name), max_workers=4)
PY
    fi
fi

if [ ! -f "${MODELS_DIR}/silero_vad.onnx" ]; then
    echo "Downloading silero_vad.onnx ..."
    curl -fL --retry 3 -o "${MODELS_DIR}/silero_vad.onnx" \
        "${RELEASE_BASE}/asr-models/silero_vad.onnx"
fi

echo ""
echo "Voice-lite ready. Start it with:"
echo "  bash scripts/start-voice-lite.sh"
#!/usr/bin/env bash
# Bootstrap the local voice sidecar (Plan B: local speech).
#
# Creates ~/.jcowork/venv-voice-lite, installs services/voice-lite deps and
# downloads the speech models into $JCWORK_DATA_DIR/models/voice-lite.
# On Apple Silicon the primary ASR is Qwen3-ASR-1.7B (5-bit MLX) and the
# primary TTS is Qwen3-TTS-12Hz-1.7B-CustomVoice (8-bit MLX), with Kokoro-82M-v1.1-zh
# (MLX) as TTS fallback; elsewhere (or when the MLX models are absent)
# sherpa-onnx streaming ASR / melo TTS take over.
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
QWEN3_TTS_NAME="Qwen3-TTS-12Hz-1.7B-CustomVoice-8bit"
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
    if [ -f "${MODELS_DIR}/${QWEN3_TTS_NAME}/config.json" ]; then
        echo "Model ${QWEN3_TTS_NAME} already present — skipping."
    else
        echo "Downloading ${QWEN3_TTS_NAME} (MLX zh TTS, ~3 GB) ..."
        "${VENV_DIR}/bin/python" - "${MODELS_DIR}" "${QWEN3_TTS_NAME}" <<'PY'
import os, sys, json, time, urllib.request
from concurrent.futures import ThreadPoolExecutor
models_dir, name = sys.argv[1], sys.argv[2]
dst = os.path.join(models_dir, name)
base = f"https://modelscope.cn/models/mlx-community/{name}/resolve/master/"
api = f"https://modelscope.cn/api/v1/models/mlx-community/{name}/repo/files?Revision=master&Root="
CH = 64 * 1024 * 1024
def listfiles(root=""):
    d = json.load(urllib.request.urlopen(api + root, timeout=30))["Data"]["Files"]
    out = []
    for f in d:
        if f["Type"] == "tree":
            out += listfiles(f["Path"])
        else:
            out.append(f["Path"])
    return out
def true_size(rel):
    # The files API "Size" can report the safetensors data-section size
    # (header excluded), which truncates the download; probe the real total
    # from Content-Range instead.
    req = urllib.request.Request(base + rel, headers={"Range": "bytes=0-0"})
    with urllib.request.urlopen(req, timeout=30) as r:
        cr = r.headers.get("Content-Range")  # bytes 0-0/<total>
        if cr and "/" in cr:
            return int(cr.rsplit("/", 1)[1])
    raise RuntimeError(f"cannot determine size of {rel}")
def fetch(url, s, e, p, expect):
    if os.path.exists(p) and os.path.getsize(p) == expect:
        return
    for attempt in range(8):
        try:
            req = urllib.request.Request(url, headers={"Range": f"bytes={s}-{e}"})
            with urllib.request.urlopen(req, timeout=120) as r, open(p, "wb") as f:
                while True:
                    b = r.read(1 << 20)
                    if not b:
                        break
                    f.write(b)
            if os.path.getsize(p) == expect:
                return
        except Exception:
            pass
        time.sleep(1.5 * (attempt + 1))
    raise RuntimeError(f"chunk failed: {p}")
def get(rel):
    url = base + rel
    out = os.path.join(dst, rel)
    os.makedirs(os.path.dirname(out), exist_ok=True)
    size = true_size(rel)
    if os.path.exists(out) and os.path.getsize(out) == size:
        return
    if size <= CH:
        urllib.request.urlretrieve(url, out)
        return
    n = (size + CH - 1) // CH
    def task(i):
        s = i * CH
        e = min(size - 1, s + CH - 1)
        p = f"{out}.part{i:03d}"
        fetch(url, s, e, p, e - s + 1)
        return p
    with ThreadPoolExecutor(8) as ex:
        parts = list(ex.map(task, range(n)))
    tmp = out + ".assembling"
    with open(tmp, "wb") as f:
        for p in parts:
            with open(p, "rb") as pf:
                f.write(pf.read())
    assert os.path.getsize(tmp) == size, f"size mismatch {rel}"
    os.replace(tmp, out)
    for p in parts:
        os.remove(p)
try:
    # ModelScope's CN CDN is far faster than the HF mirror for large weights.
    for rel in listfiles():
        get(rel)
except Exception:
    os.environ["HF_ENDPOINT"] = "https://hf-mirror.com"
    from huggingface_hub import snapshot_download
    snapshot_download(f"mlx-community/{name}", local_dir=dst, max_workers=4)
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
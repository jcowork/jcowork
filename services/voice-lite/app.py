"""
Voice-Lite: local full-duplex speech sidecar (Plan B, pure CPU).

One process serves both speech transports over plain WebSocket, speaking the
contract from docs/LOCAL_VOICE_DEPLOYMENT.md §4 so the jcowork gateway can
swap cloud <-> local without any orchestration change:

- /asr : binary 16 kHz s16le mono PCM upstream; JSON ready / speech_started /
         speech_stopped / partial / final / error downstream.
         Primary: Qwen3-ASR-1.7B (5-bit MLX, Apple Silicon) transcribing
         silero-VAD-segmented utterances; throttled re-transcription of the
         growing buffer provides live partials. Fallback: streaming
         paraformer / zipformer (sherpa-onnx, pure CPU) elsewhere.
- /tts : JSON start / text / finish upstream; binary 24 kHz s16le mono PCM
         + JSON done / error downstream. Sentence-level synthesis with
         Kokoro-82M-v1.1-zh (MLX, Apple Silicon) or vits-melo-tts-zh_en
         (sherpa-onnx) elsewhere; every sentence is loudness-normalized
         so playback volume stays steady across sentences.
- /health : readiness probe for setup scripts and operators.

Usage:
    python app.py            # models auto-discovered under MODELS_DIR
Environment:
    VOICE_LITE_MODELS_DIR  (default ~/.jcowork/data/models/voice-lite)
    VOICE_LITE_HOST        (default 127.0.0.1)
    VOICE_LITE_PORT        (default 50061)
    VOICE_LITE_THREADS     (default 2, per model)
    VOICE_LITE_ASR         ("qwen" | "sherpa"; default auto: qwen when available)
    VOICE_LITE_TTS         ("qwen3" | "kokoro" | "melo"; default auto: qwen3 when available)
    VOICE_LITE_TTS_VOICE   (MLX TTS voice pin: Qwen3 speaker or Kokoro pack name)
"""

import asyncio
import glob
import json
import logging
import os
import platform
import threading
from concurrent.futures import ThreadPoolExecutor
from contextlib import asynccontextmanager
from pathlib import Path

import numpy as np
import sherpa_onnx
import uvicorn
from fastapi import FastAPI, WebSocket, WebSocketDisconnect

logging.basicConfig(level=logging.INFO, format="%(asctime)s %(levelname)s %(message)s")
log = logging.getLogger("voice-lite")

MODELS_DIR = Path(
    os.environ.get(
        "VOICE_LITE_MODELS_DIR",
        str(Path.home() / ".jcowork" / "data" / "models" / "voice-lite"),
    )
)
HOST = os.environ.get("VOICE_LITE_HOST", "127.0.0.1")
PORT = int(os.environ.get("VOICE_LITE_PORT", "50061"))
THREADS = int(os.environ.get("VOICE_LITE_THREADS", "2"))

ASR_DIR = MODELS_DIR / "sherpa-onnx-streaming-zipformer-bilingual-zh-en-2023-02-20"
PARAFORMER_DIR = MODELS_DIR / "sherpa-onnx-streaming-paraformer-bilingual-zh-en"
QWEN_DIR = MODELS_DIR / "Qwen3-ASR-1.7B-5bit"
KOKORO_DIR = MODELS_DIR / "Kokoro-82M-v1.1-zh-mlx"
QWEN3_TTS_DIR = MODELS_DIR / "Qwen3-TTS-12Hz-1.7B-CustomVoice-8bit"
TTS_DIR = MODELS_DIR / "vits-melo-tts-zh_en"
VAD_PATH = MODELS_DIR / "silero_vad.onnx"
ASR_PREF = os.environ.get("VOICE_LITE_ASR", "auto").lower()
TTS_PREF = os.environ.get("VOICE_LITE_TTS", "auto").lower()
TTS_VOICE_PIN = os.environ.get("VOICE_LITE_TTS_VOICE", "").strip()
# Speaker ids from the gateway map onto MLX TTS voices in this order.
QWEN3_VOICES = ["Vivian", "Serena", "Uncle_Fu", "Dylan"]
KOKORO_VOICES = ["zf_001", "zf_002", "zm_010", "zm_011"]

TARGET_RATE = 24000  # gateway/browser contract: 24 kHz s16le mono
FRAME_BYTES = 1920  # 40 ms at 24 kHz

recognizer = None
qwen_model = None
asr_backend = "none"
vad_model_path = None
tts = None
mlx_tts = None
tts_backend = "none"
tts_lock = threading.Lock()  # OfflineTts.generate is not re-entrant
# MLX generate is not re-entrant either; one worker serializes all Qwen3-ASR
# calls (partials skip when a job is in flight, finals queue behind it).
mlx_lock = threading.Lock()
asr_executor = ThreadPoolExecutor(max_workers=1, thread_name_prefix="qwen-asr")


def _pick(patterns):
    for p in patterns:
        hits = sorted(glob.glob(str(p)))
        if hits:
            return hits[0]
    return None


def _pick_fp32(directory, stem):
    hits = [p for p in sorted(glob.glob(str(directory / f"{stem}*.onnx"))) if "int8" not in p]
    return hits[0] if hits else None


def _load_qwen():
    """Qwen3-ASR-1.7B (5-bit MLX) — Apple Silicon only, offline utterance ASR."""
    global qwen_model, asr_backend
    if ASR_PREF == "sherpa":
        return
    if platform.system() != "Darwin" or platform.machine() != "arm64":
        log.info("Qwen3-ASR skipped: MLX requires Apple Silicon")
        return
    if not (QWEN_DIR / "config.json").exists():
        log.info("Qwen3-ASR model absent at %s — run scripts/setup-voice-lite.sh", QWEN_DIR)
        return
    try:
        from mlx_audio.stt.utils import load_model

        qwen_model = load_model(str(QWEN_DIR))
        # Warm the Metal kernels so the first real utterance pays no compile cost.
        qwen_model.generate(np.zeros(16000, dtype=np.float32), verbose=False)
        asr_backend = "qwen"
        log.info("ASR ready: Qwen3-ASR-1.7B-5bit (MLX)")
    except Exception:
        qwen_model = None
        log.exception("Qwen3-ASR load failed — falling back to sherpa-onnx")


def _load_sherpa_asr():
    global recognizer, asr_backend
    try:
        # Streaming paraformer (FunASR export) is markedly stronger on Chinese
        # than the 2023-02 zipformer; fp32 because int8 quantization of the
        # zipformer produced duplicated tokens ("大大大模型型") on real mic
        # audio. Endpoint rules mirror the cloud server_vad feel (~700 ms).
        endpoint_rules = dict(
            enable_endpoint_detection=True,
            rule1_min_trailing_silence=1.2,
            rule2_min_trailing_silence=0.7,
            rule3_min_utterance_length=60,
        )
        p_enc = _pick([PARAFORMER_DIR / "encoder.onnx"])
        p_dec = _pick([PARAFORMER_DIR / "decoder.onnx"])
        p_tokens = PARAFORMER_DIR / "tokens.txt"
        if p_enc and p_dec and p_tokens.exists():
            recognizer = sherpa_onnx.OnlineRecognizer.from_paraformer(
                tokens=str(p_tokens),
                encoder=p_enc,
                decoder=p_dec,
                num_threads=THREADS,
                sample_rate=16000,
                feature_dim=80,
                decoding_method="greedy_search",
                provider="cpu",
                **endpoint_rules,
            )
            log.info("ASR ready: streaming-paraformer-bilingual-zh-en (fp32)")
        else:
            encoder = _pick_fp32(ASR_DIR, "encoder")
            decoder = _pick_fp32(ASR_DIR, "decoder")
            joiner = _pick_fp32(ASR_DIR, "joiner")
            tokens = ASR_DIR / "tokens.txt"
            if encoder and decoder and joiner and tokens.exists():
                recognizer = sherpa_onnx.OnlineRecognizer.from_transducer(
                    encoder=encoder,
                    decoder=decoder,
                    joiner=joiner,
                    tokens=str(tokens),
                    num_threads=THREADS,
                    sample_rate=16000,
                    feature_dim=80,
                    decoding_method="greedy_search",
                    provider="cpu",
                    **endpoint_rules,
                )
                log.info("ASR ready: %s (fp32 fallback)", Path(encoder).name)
            else:
                log.error("ASR model files missing under %s — run scripts/setup-voice-lite.sh", ASR_DIR)
        if recognizer is not None:
            asr_backend = "sherpa"
    except Exception:
        log.exception("ASR model load failed")


def _kokoro_en_callable():
    """Lightweight English G2P for mixed zh/en sentences (no spacy/espeak).

    misaki's bundled gold/silver lexicons cover common words in the exact
    notation the Kokoro vocab expects; unknown words (acronyms like GPT)
    are spelled out letter by letter.
    """
    import importlib.resources
    import re

    from misaki import data

    lex = {}
    for name in ("us_silver.json", "us_gold.json"):
        with importlib.resources.open_text(data, name) as r:
            lex.update(json.load(r))
    letters = {
        "A": "eɪ", "B": "bi", "C": "si", "D": "di", "E": "i", "F": "ɛf",
        "G": "ʤi", "H": "eɪʧ", "I": "aɪ", "J": "ʤeɪ", "K": "keɪ", "L": "ɛl",
        "M": "ɛm", "N": "ɛn", "O": "oʊ", "P": "pi", "Q": "kju", "R": "ɑɹ",
        "S": "ɛs", "T": "ti", "U": "ju", "V": "vi", "W": "dʌbᵊlju",
        "X": "ɛks", "Y": "waɪ", "Z": "zi",
    }

    def word_ps(w):
        e = lex.get(w.lower())
        if isinstance(e, dict):  # POS-tagged entries: take the default
            e = e.get("DEFAULT") or next(iter(e.values()), None)
        if isinstance(e, str) and e:
            return e
        return " ".join(letters[c] for c in w.upper() if c in letters)

    def en(text):
        return " ".join(word_ps(w) for w in re.findall(r"[A-Za-z]+", text))

    return en


def _load_qwen3_tts():
    """Qwen3-TTS-12Hz-1.7B-CustomVoice (8-bit MLX) — Apple Silicon primary TTS."""
    global mlx_tts, tts_backend
    if TTS_PREF in ("melo", "kokoro"):
        return
    if platform.system() != "Darwin" or platform.machine() != "arm64":
        log.info("Qwen3-TTS skipped: MLX requires Apple Silicon")
        return
    if not (QWEN3_TTS_DIR / "config.json").exists():
        log.info("Qwen3-TTS model absent at %s — run scripts/setup-voice-lite.sh", QWEN3_TTS_DIR)
        return
    try:
        from mlx_audio.tts.utils import load_model

        model = load_model(str(QWEN3_TTS_DIR))
        mlx_tts = model
        # Warm Metal kernels so the first real sentence pays no compile cost.
        for _ in mlx_tts.generate("你好。", voice=QWEN3_VOICES[0], lang_code="auto"):
            pass
        tts_backend = "qwen3"
        log.info("TTS ready: Qwen3-TTS-12Hz-1.7B-CustomVoice-8bit (MLX)")
    except Exception:
        mlx_tts = None
        log.exception("Qwen3-TTS load failed — falling back to Kokoro/melo TTS")


def _load_kokoro():
    """Kokoro-82M-v1.1-zh (MLX) — Apple Silicon only, natural zh timbre."""
    global mlx_tts, tts_backend
    if TTS_PREF == "melo":
        return
    if platform.system() != "Darwin" or platform.machine() != "arm64":
        log.info("Kokoro TTS skipped: MLX requires Apple Silicon")
        return
    if not (KOKORO_DIR / "config.json").exists():
        log.info("Kokoro model absent at %s — run scripts/setup-voice-lite.sh", KOKORO_DIR)
        return
    try:
        import mlx.core as mx
        from mlx.utils import tree_flatten
        from mlx_audio.tts.models.kokoro import Model, ModelConfig
        from misaki import zh
        from safetensors.numpy import save_file

        # The zh voice packs ship as .npy; mlx-audio reads voices/{name}.safetensors.
        needed = set(KOKORO_VOICES)
        if TTS_VOICE_PIN:
            needed.add(TTS_VOICE_PIN)
        for name in sorted(needed):
            npy = KOKORO_DIR / "voices" / f"{name}.npy"
            st = KOKORO_DIR / "voices" / f"{name}.safetensors"
            if npy.exists() and not st.exists():
                save_file({"voice": np.load(npy)}, str(st))
        cfg = json.loads((KOKORO_DIR / "config.json").read_text())
        cfg["model_path"] = str(KOKORO_DIR)
        model = Model(ModelConfig.from_dict(cfg))
        weights = model.sanitize(mx.load(str(KOKORO_DIR / "model.safetensors")))
        # This zh conversion stores conv weights in MLX (out,k,in) layout;
        # sanitize's square-kernel heuristic assumes torch layout and would
        # transpose them a second time. Undo exactly the shape-mismatching ones.
        params = dict(tree_flatten(model.parameters()))
        fixed = {}
        for k, v in weights.items():
            p = params.get(k)
            if (
                p is not None
                and p.shape != v.shape
                and v.ndim == 3
                and (v.shape[0], v.shape[2], v.shape[1]) == p.shape
            ):
                v = v.transpose(0, 2, 1)
            fixed[k] = v
        model.load_weights(list(fixed.items()), strict=True)
        # v1.1-zh tokenizes with the 1.1 bopomofo frontend; mlx-audio's default
        # legacy G2P emits IPA+tone-arrows the v1.1 vocab silently drops,
        # which garbles the speech. Verified via ASR round-trip (20/20 exact).
        pipe = model._get_pipeline("z")
        pipe.g2p = zh.ZHG2P(version="1.1", en_callable=_kokoro_en_callable())
        mlx_tts = model
        # Warm Metal kernels so the first real sentence pays no compile cost.
        for _ in mlx_tts.generate("你好。", voice=KOKORO_VOICES[0], lang_code="z"):
            pass
        tts_backend = "kokoro"
        log.info("TTS ready: Kokoro-82M-v1.1-zh (MLX)")
    except Exception:
        mlx_tts = None
        log.exception("Kokoro load failed — falling back to melo TTS")


def load_models():
    global vad_model_path, tts, tts_backend
    _load_qwen()
    if qwen_model is None:
        _load_sherpa_asr()
    if VAD_PATH.exists():
        vad_model_path = str(VAD_PATH)
    else:
        log.error("silero VAD missing: %s", VAD_PATH)
    _load_qwen3_tts()
    if mlx_tts is None:
        _load_kokoro()
    if mlx_tts is not None:
        return
    try:
        model = _pick([TTS_DIR / "model.onnx", TTS_DIR / "*.onnx"])
        if model and (TTS_DIR / "lexicon.txt").exists():
            fsts = ",".join(
                str(TTS_DIR / name)
                for name in ("date.fst", "number.fst", "phone.fst")
                if (TTS_DIR / name).exists()
            )
            tts = sherpa_onnx.OfflineTts(
                sherpa_onnx.OfflineTtsConfig(
                    model=sherpa_onnx.OfflineTtsModelConfig(
                        vits=sherpa_onnx.OfflineTtsVitsModelConfig(
                            model=model,
                            lexicon=str(TTS_DIR / "lexicon.txt"),
                            tokens=str(TTS_DIR / "tokens.txt"),
                        ),
                        num_threads=THREADS,
                        provider="cpu",
                    ),
                    rule_fsts=fsts,
                    max_num_sentences=1,
                )
            )
            log.info("TTS ready: %s (%d speakers)", Path(model).name, tts.num_speakers)
            tts_backend = "melo"
        else:
            log.error("TTS model files missing under %s — run scripts/setup-voice-lite.sh", TTS_DIR)
    except Exception:
        log.exception("TTS model load failed")


@asynccontextmanager
async def lifespan(app: FastAPI):
    load_models()
    yield


app = FastAPI(lifespan=lifespan)


@app.get("/health")
async def health():
    return {
        "status": "ok",
        "asr": qwen_model is not None or recognizer is not None,
        "asr_backend": asr_backend,
        "tts": tts is not None or mlx_tts is not None,
        "tts_backend": tts_backend,
    }


@app.websocket("/asr")
async def asr_ws(ws: WebSocket):
    await ws.accept()
    if (qwen_model is None and recognizer is None) or vad_model_path is None:
        await ws.send_json({"type": "error", "message": "local ASR model not loaded"})
        await ws.close(code=1011)
        return
    # sherpa-onnx 2.x exposes the low-level window classifier; hysteresis
    # (min speech / min silence runs) turns it into start/stop edges, the
    # same role the cloud server_vad plays for barge-in.
    vad = sherpa_onnx.VadModel.create(
        sherpa_onnx.VadModelConfig(
            silero_vad=sherpa_onnx.SileroVadModelConfig(
                model=vad_model_path,
                threshold=0.5,
                min_silence_duration=0.3,
                min_speech_duration=0.15,
                window_size=512,
            ),
            sample_rate=16000,
            num_threads=1,
        )
    )
    await ws.send_json({"type": "ready"})
    try:
        if qwen_model is not None:
            await _asr_qwen(ws, vad)
        else:
            await _asr_sherpa(ws, vad)
    except WebSocketDisconnect:
        pass
    except Exception as e:
        log.exception("ASR session failed")
        try:
            await ws.send_json({"type": "error", "message": str(e)})
        except Exception:
            pass


async def _asr_sherpa(ws: WebSocket, vad):
    """Streaming sherpa-onnx path: partials per decode, finals on endpoint."""
    window = vad.window_size()
    need_speech = max(1, int(0.15 * 16000 / window))
    need_silence = max(1, int(0.3 * 16000 / window))
    speech_run = 0
    silence_run = 0
    vad_buf = np.zeros(0, dtype=np.float32)
    stream = recognizer.create_stream()
    in_speech = False
    last_partial = ""
    while True:
        msg = await ws.receive()
        if msg["type"] == "websocket.disconnect":
            break
        data = msg.get("bytes")
        if not data:
            continue
        samples = np.frombuffer(data, dtype=np.int16).astype(np.float32) / 32768.0
        # VAD drives barge-in (speech_started), mirroring cloud server_vad.
        vad_buf = np.concatenate([vad_buf, samples])
        while len(vad_buf) >= window:
            chunk = vad_buf[:window]
            vad_buf = vad_buf[window:]
            if vad.is_speech(chunk):
                speech_run += 1
                silence_run = 0
            else:
                silence_run += 1
                speech_run = 0
            if not in_speech and speech_run >= need_speech:
                in_speech = True
                speech_run = 0
                await ws.send_json({"type": "speech_started"})
            elif in_speech and silence_run >= need_silence:
                in_speech = False
                silence_run = 0
                await ws.send_json({"type": "speech_stopped"})
        # Streaming transcription with endpoint-based finals.
        stream.accept_waveform(16000, samples)
        while recognizer.is_ready(stream):
            recognizer.decode_stream(stream)
        text = recognizer.get_result(stream)
        if text != last_partial:
            last_partial = text
            if text:
                await ws.send_json({"type": "partial", "text": text, "stash": ""})
        if recognizer.is_endpoint(stream):
            if text.strip():
                await ws.send_json({"type": "final", "text": text.strip()})
            recognizer.reset(stream)
            last_partial = ""


# Qwen3-ASR transcribes whole utterances, so silero VAD doubles as the
# segmenter: capture from speech onset (with pre-roll) until 0.7 s of
# trailing silence — the endpoint feel of cloud server_vad / paraformer
# rule2. The 0.3 s speech_stopped edge is unchanged for barge-in.
QWEN_PRE_ROLL = int(0.32 * 16000)
QWEN_END_SILENCE = 0.7
QWEN_MAX_SEG = 30 * 16000  # force a final on marathon utterances
QWEN_PARTIAL_EVERY = 1.0 * 16000  # samples of growth between partial jobs


def _qwen_transcribe(audio: np.ndarray) -> str:
    with mlx_lock:
        out = qwen_model.generate(audio, verbose=False)
    return (out.text or "").strip()


async def _asr_qwen(ws: WebSocket, vad):
    """VAD-segmented Qwen3-ASR: finals per utterance, throttled partials."""
    window = vad.window_size()
    need_speech = max(1, int(0.15 * 16000 / window))
    need_silence = max(1, int(0.3 * 16000 / window))
    need_end = max(1, int(QWEN_END_SILENCE * 16000 / window))
    loop = asyncio.get_running_loop()
    speech_run = 0
    silence_run = 0
    vad_buf = np.zeros(0, dtype=np.float32)
    in_speech = False
    capturing = False
    pre_buf = np.zeros(0, dtype=np.float32)
    seg = []
    seg_len = 0
    gen = 0  # segment generation; stale partials are dropped
    inflight = 0
    last_partial = ""
    partial_mark = 0
    closed = False
    jobs = set()

    async def run_job(audio, job_gen, kind):
        nonlocal inflight, last_partial
        inflight += 1
        try:
            text = await loop.run_in_executor(asr_executor, _qwen_transcribe, audio)
        except Exception:
            log.exception("Qwen3-ASR transcribe failed")
            return
        finally:
            inflight -= 1
        if closed:
            return
        if kind == "final":
            if text:
                await ws.send_json({"type": "final", "text": text})
        elif job_gen == gen and capturing and text and text != last_partial:
            last_partial = text
            await ws.send_json({"type": "partial", "text": text, "stash": ""})

    def submit(audio, job_gen, kind):
        task = asyncio.create_task(run_job(audio, job_gen, kind))
        jobs.add(task)
        task.add_done_callback(jobs.discard)

    def flush():
        nonlocal capturing, seg, seg_len, gen, last_partial, partial_mark
        audio = np.concatenate(seg) if seg else np.zeros(0, dtype=np.float32)
        gen += 1
        capturing = False
        seg = []
        seg_len = 0
        last_partial = ""
        partial_mark = 0
        if len(audio) >= 1600:  # >=100 ms; ignore VAD blips
            submit(audio, gen, "final")

    try:
        while True:
            msg = await ws.receive()
            if msg["type"] == "websocket.disconnect":
                break
            data = msg.get("bytes")
            if not data:
                continue
            samples = np.frombuffer(data, dtype=np.int16).astype(np.float32) / 32768.0
            vad_buf = np.concatenate([vad_buf, samples])
            while len(vad_buf) >= window:
                chunk = vad_buf[:window]
                vad_buf = vad_buf[window:]
                if vad.is_speech(chunk):
                    speech_run += 1
                    silence_run = 0
                else:
                    silence_run += 1
                    speech_run = 0
                if not in_speech and speech_run >= need_speech:
                    in_speech = True
                    speech_run = 0
                    if not capturing:
                        capturing = True
                        seg = [pre_buf]
                        seg_len = len(pre_buf)
                        partial_mark = seg_len
                        pre_buf = np.zeros(0, dtype=np.float32)
                    await ws.send_json({"type": "speech_started"})
                elif in_speech and silence_run >= need_silence:
                    in_speech = False
                    await ws.send_json({"type": "speech_stopped"})
                # silence_run keeps counting past speech_stopped so the
                # utterance ends at 0.7 s, not 0.3 s + 0.7 s.
                if capturing and not in_speech and silence_run >= need_end:
                    flush()
            if capturing:
                seg.append(samples)
                seg_len += len(samples)
                if seg_len >= QWEN_MAX_SEG:
                    flush()
                    capturing = True  # seamless continuation of long speech
                elif inflight == 0 and seg_len - partial_mark >= QWEN_PARTIAL_EVERY:
                    partial_mark = seg_len
                    submit(np.concatenate(seg), gen, "partial")
            else:
                pre_buf = np.concatenate([pre_buf, samples])[-QWEN_PRE_ROLL:]
    finally:
        closed = True
        for task in jobs:
            task.cancel()


# Sentence-to-sentence loudness drifts with both VITS and Kokoro prosody;
# without normalization playback volume audibly jumps between sentences.
TARGET_RMS = 0.10  # ~-20 dBFS speech loudness, steady across sentences
PEAK_CEIL = 0.95


def _normalize_loudness(samples: np.ndarray, sr: int) -> np.ndarray:
    """RMS-normalize one sentence, cap peaks, fade edges to avoid clicks."""
    if len(samples) == 0:
        return samples
    peak = float(np.max(np.abs(samples)))
    if peak > 1e-4:
        # Measure loudness on voiced samples only — leading/trailing silence
        # must not dilute the RMS and make quiet sentences louder still.
        active = samples[np.abs(samples) > 0.05 * peak]
        rms = float(np.sqrt(np.mean(active * active)))
        if rms > 1e-6:
            samples = samples * (TARGET_RMS / rms)
        peak = float(np.max(np.abs(samples)))
        if peak > PEAK_CEIL:
            samples = samples * (PEAK_CEIL / peak)
    fade = min(int(0.005 * sr), len(samples) // 2)
    if fade > 0:
        ramp = np.linspace(0.0, 1.0, fade, dtype=np.float32)
        samples[:fade] *= ramp
        samples[-fade:] *= ramp[::-1]
    return samples


def _resample_to_24k(samples: np.ndarray, src_rate: int) -> bytes:
    if src_rate == TARGET_RATE:
        out = samples
    else:
        n_out = int(round(len(samples) * TARGET_RATE / src_rate))
        x_src = np.arange(len(samples), dtype=np.float64)
        x_dst = np.linspace(0, len(samples) - 1, n_out)
        out = np.interp(x_dst, x_src, samples)
    return np.clip(out * 32768.0, -32768, 32767).astype(np.int16).tobytes()


@app.websocket("/tts")
async def tts_ws(ws: WebSocket):
    await ws.accept()
    if tts is None and mlx_tts is None:
        await ws.send_json({"type": "error", "message": "local TTS model not loaded"})
        await ws.close(code=1011)
        return
    queue: asyncio.Queue = asyncio.Queue()
    state = {"voice": 0, "speed": 1.0}
    loop = asyncio.get_running_loop()

    def generate(text: str):
        if mlx_tts is not None:
            if tts_backend == "qwen3":
                voice = TTS_VOICE_PIN or QWEN3_VOICES[state["voice"] % len(QWEN3_VOICES)]
                lang = "auto"
            else:
                voice = TTS_VOICE_PIN or KOKORO_VOICES[state["voice"] % len(KOKORO_VOICES)]
                lang = "z"
            with mlx_lock:
                parts = [
                    np.asarray(r.audio, dtype=np.float32)
                    for r in mlx_tts.generate(
                        text, voice=voice, speed=state["speed"], lang_code=lang
                    )
                ]
            return np.concatenate(parts), mlx_tts.sample_rate
        with tts_lock:
            audio = tts.generate(text, sid=state["voice"], speed=state["speed"])
        return np.asarray(audio.samples, dtype=np.float32), audio.sample_rate

    async def worker():
        # Sentence-level streaming: synthesize each queued sentence and push
        # its PCM immediately, so playback starts after the first sentence.
        while True:
            item = await queue.get()
            if item is None:
                await ws.send_json({"type": "done"})
                return
            samples, sr = await loop.run_in_executor(None, generate, item)
            samples = _normalize_loudness(samples, sr)
            pcm = _resample_to_24k(samples, sr)
            for i in range(0, len(pcm), FRAME_BYTES):
                await ws.send_bytes(pcm[i : i + FRAME_BYTES])

    task = asyncio.create_task(worker())
    try:
        while True:
            msg = await ws.receive()
            if msg["type"] == "websocket.disconnect":
                break
            raw = msg.get("text")
            if not raw:
                continue
            v = json.loads(raw)
            ty = v.get("type")
            if ty == "start":
                try:
                    state["voice"] = int(v.get("voice", 0))
                except (TypeError, ValueError):
                    state["voice"] = 0
                state["speed"] = float(v.get("speed", 1.0))
            elif ty == "text":
                text = (v.get("text") or "").strip()
                if text:
                    await queue.put(text)
            elif ty == "finish":
                await queue.put(None)
    except WebSocketDisconnect:
        pass
    except Exception as e:
        log.exception("TTS session failed")
        try:
            await ws.send_json({"type": "error", "message": str(e)})
        except Exception:
            pass
    finally:
        # Barge-in = the gateway drops the connection; stop synthesizing.
        task.cancel()


if __name__ == "__main__":
    uvicorn.run(app, host=HOST, port=PORT, log_level="info")
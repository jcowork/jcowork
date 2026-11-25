"""
Voice-Lite: local full-duplex speech sidecar (Plan B, pure CPU).

One process serves both speech transports over plain WebSocket, speaking the
contract from docs/LOCAL_VOICE_DEPLOYMENT.md §4 so the jcowork gateway can
swap cloud <-> local without any orchestration change:

- /asr : binary 16 kHz s16le mono PCM upstream; JSON ready / speech_started /
         speech_stopped / partial / final / error downstream.
         Streaming paraformer (zh-en) + silero VAD for barge-in detection;
         falls back to streaming zipformer when the paraformer export is absent.
- /tts : JSON start / text / finish upstream; binary 24 kHz s16le mono PCM
         + JSON done / error downstream. Sentence-level synthesis with
         vits-melo-tts-zh_en (multi-speaker).
- /health : readiness probe for setup scripts and operators.

Usage:
    python app.py            # models auto-discovered under MODELS_DIR
Environment:
    VOICE_LITE_MODELS_DIR  (default ~/.jcowork/data/models/voice-lite)
    VOICE_LITE_HOST        (default 127.0.0.1)
    VOICE_LITE_PORT        (default 50061)
    VOICE_LITE_THREADS     (default 2, per model)
"""

import asyncio
import glob
import json
import logging
import os
import threading
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
TTS_DIR = MODELS_DIR / "vits-melo-tts-zh_en"
VAD_PATH = MODELS_DIR / "silero_vad.onnx"

TARGET_RATE = 24000  # gateway/browser contract: 24 kHz s16le mono
FRAME_BYTES = 1920  # 40 ms at 24 kHz

recognizer = None
vad_model_path = None
tts = None
tts_lock = threading.Lock()  # OfflineTts.generate is not re-entrant


def _pick(patterns):
    for p in patterns:
        hits = sorted(glob.glob(str(p)))
        if hits:
            return hits[0]
    return None


def _pick_fp32(directory, stem):
    hits = [p for p in sorted(glob.glob(str(directory / f"{stem}*.onnx"))) if "int8" not in p]
    return hits[0] if hits else None


def load_models():
    global recognizer, vad_model_path, tts
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
    except Exception:
        log.exception("ASR model load failed")
    if VAD_PATH.exists():
        vad_model_path = str(VAD_PATH)
    else:
        log.error("silero VAD missing: %s", VAD_PATH)
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
    return {"status": "ok", "asr": recognizer is not None, "tts": tts is not None}


@app.websocket("/asr")
async def asr_ws(ws: WebSocket):
    await ws.accept()
    if recognizer is None or vad_model_path is None:
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
    window = vad.window_size()
    need_speech = max(1, int(0.15 * 16000 / window))
    need_silence = max(1, int(0.3 * 16000 / window))
    speech_run = 0
    silence_run = 0
    vad_buf = np.zeros(0, dtype=np.float32)
    stream = recognizer.create_stream()
    await ws.send_json({"type": "ready"})
    in_speech = False
    last_partial = ""
    try:
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
    except WebSocketDisconnect:
        pass
    except Exception as e:
        log.exception("ASR session failed")
        try:
            await ws.send_json({"type": "error", "message": str(e)})
        except Exception:
            pass


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
    if tts is None:
        await ws.send_json({"type": "error", "message": "local TTS model not loaded"})
        await ws.close(code=1011)
        return
    queue: asyncio.Queue = asyncio.Queue()
    state = {"voice": 0, "speed": 1.0}
    loop = asyncio.get_running_loop()

    def generate(text: str):
        with tts_lock:
            return tts.generate(text, sid=state["voice"], speed=state["speed"])

    async def worker():
        # Sentence-level streaming: synthesize each queued sentence and push
        # its PCM immediately, so playback starts after the first sentence.
        while True:
            item = await queue.get()
            if item is None:
                await ws.send_json({"type": "done"})
                return
            audio = await loop.run_in_executor(None, generate, item)
            pcm = _resample_to_24k(np.asarray(audio.samples, dtype=np.float32), audio.sample_rate)
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
// Full-duplex voice call channel to the gateway's /api/voice WebSocket.
//
// Capture: getUserMedia + AudioWorklet downsamples mic audio to 16 kHz
// s16le mono and sends ~40 ms binary frames (640 samples).
// Playback: a dedicated 24 kHz AudioContext queues the base64 PCM chunks
// from `tts_audio` events back-to-back; `tts_cancel` flushes the queue.
//
// The server drives the state machine (voice_ready / user_speaking /
// asr_final / tts_audio / done); this class translates those events into
// VoiceState transitions and forwards transcripts/reply text to the UI.

import { WS_BASE } from '../config';

export type VoiceState = 'connecting' | 'listening' | 'thinking' | 'speaking';

export interface VoiceHistoryMessage {
  role: string;
  content: string;
}

export interface VoiceClientOptions {
  token: string;
  conversationId: string;
  history: VoiceHistoryMessage[];
  model?: string;
}

export interface VoiceCallbacks {
  onState?: (state: VoiceState) => void;
  /** Live (partial) speech-to-text while the user is speaking. */
  onAsrPartial?: (text: string) => void;
  onAsrFinal?: (text: string) => void;
  onTextDelta?: (content: string) => void;
  /** Reply turn finished (done) or interrupted (stopped/barge-in). */
  onTurnEnd?: () => void;
  onError?: (message: string) => void;
  /** Socket closed for any reason — the call is over. */
  onClose?: () => void;
}

// Inline worklet: downsample the mic stream to 16 kHz Int16 and post
// 640-sample (~40 ms) frames. Keeps a fractional read position across
// process() calls so non-integer ratios (44.1 kHz) don't drift.
const WORKLET_CODE = `
class PcmCapture extends AudioWorkletProcessor {
  constructor() {
    super();
    this.ratio = sampleRate / 16000;
    this.pending = new Float32Array(0);
    this.pos = 0;
    this.out = [];
  }
  process(inputs) {
    const ch = inputs[0] && inputs[0][0];
    if (!ch) return true;
    const buf = new Float32Array(this.pending.length + ch.length);
    buf.set(this.pending);
    buf.set(ch, this.pending.length);
    let pos = this.pos;
    while (pos + this.ratio <= buf.length) {
      const start = Math.floor(pos);
      const end = Math.floor(pos + this.ratio);
      let sum = 0;
      for (let i = start; i < end; i++) sum += buf[i];
      this.out.push(sum / (end - start));
      pos += this.ratio;
    }
    const dropped = Math.floor(pos);
    this.pending = buf.slice(dropped);
    this.pos = pos - dropped;
    while (this.out.length >= 640) {
      const frame = this.out.splice(0, 640);
      const pcm = new Int16Array(640);
      for (let i = 0; i < 640; i++) {
        const v = Math.max(-1, Math.min(1, frame[i]));
        pcm[i] = v < 0 ? v * 0x8000 : v * 0x7fff;
      }
      this.port.postMessage(pcm.buffer, [pcm.buffer]);
    }
    return true;
  }
}
registerProcessor('pcm-capture', PcmCapture);
`;

export class VoiceClient {
  private opts: VoiceClientOptions;
  private cb: VoiceCallbacks;
  private ws: WebSocket | null = null;
  private ready = false;
  private ttsMuted = false;
  private closed = false;

  // Capture graph
  private micStream: MediaStream | null = null;
  private captureCtx: AudioContext | null = null;
  private workletNode: AudioWorkletNode | null = null;
  private workletUrl: string | null = null;

  // Playback graph (24 kHz PCM from the TTS stream)
  private playCtx: AudioContext | null = null;
  private nextPlayTime = 0;
  private activeSources = new Set<AudioBufferSourceNode>();
  private turnDone = false;

  constructor(opts: VoiceClientOptions, cb: VoiceCallbacks) {
    this.opts = opts;
    this.cb = cb;
  }

  /** Open the socket and start mic capture. Call from a user gesture. */
  async start(): Promise<void> {
    this.cb.onState?.('connecting');

    // Mic permission first so a denial fails fast before any WS traffic.
    // On denial the overlay stays open showing the error banner — closing it
    // here would make the failure look like an unresponsive button.
    try {
      this.micStream = await navigator.mediaDevices.getUserMedia({
        audio: { echoCancellation: true, noiseSuppression: true, autoGainControl: true },
      });
    } catch {
      this.cb.onError?.('MIC_DENIED');
      return;
    }
    if (this.closed) {
      this.micStream.getTracks().forEach((t) => t.stop());
      return;
    }

    let wsUrl: string;
    const query = `token=${encodeURIComponent(this.opts.token)}&conv=${encodeURIComponent(this.opts.conversationId)}`;
    if (WS_BASE) {
      wsUrl = `${WS_BASE}/api/voice?${query}`;
    } else {
      const protocol = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
      wsUrl = `${protocol}//${window.location.host}/api/voice?${query}`;
    }
    const ws = new WebSocket(wsUrl);
    this.ws = ws;

    ws.onopen = () => {
      ws.send(JSON.stringify({
        type: 'start',
        history: this.opts.history,
        model: this.opts.model,
      }));
      this.startCapture().catch(() => {
        this.cb.onError?.('MIC_DENIED');
        this.hangup();
      });
    };
    ws.onmessage = (event) => {
      if (typeof event.data !== 'string') return;
      let data: { type?: string; text?: string; content?: string; message?: string; data?: string };
      try {
        data = JSON.parse(event.data);
      } catch {
        return;
      }
      this.handleEvent(data);
    };
    ws.onclose = () => {
      if (this.closed) return;
      this.closed = true;
      this.teardownMedia();
      this.cb.onClose?.();
    };
    ws.onerror = () => {
      // onclose follows and reports the teardown.
    };
  }

  /** Gate speaker playback only — the mic stays live so barge-in works. */
  setTtsMuted(muted: boolean) {
    this.ttsMuted = muted;
    if (muted) {
      this.stopSources();
      this.maybeIdle();
    }
  }

  /** End the call: tell the server, then release mic + playback.
   *  Does NOT fire onClose — the initiator already knows; onClose is reserved
   *  for unexpected/server-side disconnects. */
  hangup() {
    if (this.closed) return;
    this.closed = true;
    try {
      if (this.ws?.readyState === WebSocket.OPEN) {
        this.ws.send(JSON.stringify({ type: 'stop' }));
      }
      this.ws?.close();
    } catch {}
    this.ws = null;
    this.teardownMedia();
  }

  // ── Server events ──

  private handleEvent(data: { type?: string; text?: string; content?: string; message?: string; data?: string }) {
    switch (data.type) {
      case 'voice_ready':
        this.ready = true;
        this.cb.onState?.('listening');
        break;
      case 'user_speaking':
        // Barge-in: the previous reply (if any) is cut short.
        this.flushPlayback();
        this.cb.onTurnEnd?.();
        this.cb.onState?.('listening');
        break;
      case 'asr_partial':
        this.cb.onAsrPartial?.(data.text ?? '');
        break;
      case 'asr_final':
        this.turnDone = false;
        this.cb.onAsrFinal?.(data.text ?? '');
        this.cb.onState?.('thinking');
        break;
      case 'text_delta':
        this.cb.onTextDelta?.(data.content ?? '');
        break;
      case 'tts_audio':
        if (data.data && !this.ttsMuted) {
          this.playPcm(data.data);
          this.cb.onState?.('speaking');
        }
        break;
      case 'tts_cancel':
        this.flushPlayback();
        break;
      case 'done':
        this.turnDone = true;
        this.cb.onTurnEnd?.();
        this.maybeIdle();
        break;
      case 'stopped':
        this.turnDone = true;
        this.flushPlayback();
        this.cb.onTurnEnd?.();
        this.cb.onState?.('listening');
        break;
      case 'error':
        this.cb.onError?.(data.message ?? 'Unknown error');
        break;
      default:
        // status / tool_call_* etc. — not surfaced in the call UI.
        break;
    }
  }

  // ── Capture ──

  private async startCapture() {
    if (!this.micStream) return;
    const ctx = new AudioContext();
    this.captureCtx = ctx;
    if (ctx.state === 'suspended') await ctx.resume();
    const url = URL.createObjectURL(new Blob([WORKLET_CODE], { type: 'application/javascript' }));
    this.workletUrl = url;
    await ctx.audioWorklet.addModule(url);
    if (this.closed) return;
    const source = ctx.createMediaStreamSource(this.micStream);
    const node = new AudioWorkletNode(ctx, 'pcm-capture');
    this.workletNode = node;
    node.port.onmessage = (e: MessageEvent<ArrayBuffer>) => {
      if (!this.ready) return;
      if (this.ws?.readyState === WebSocket.OPEN) {
        this.ws.send(e.data);
      }
    };
    // Worklets only run when pulled by the destination; a zero-gain node
    // keeps the graph live without echoing the mic to the speakers.
    const sink = ctx.createGain();
    sink.gain.value = 0;
    source.connect(node);
    node.connect(sink);
    sink.connect(ctx.destination);
  }

  // ── Playback ──

  private playPcm(base64: string) {
    if (this.ttsMuted) return;
    let bytes: Uint8Array;
    try {
      const bin = atob(base64);
      bytes = new Uint8Array(bin.length);
      for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
    } catch {
      return;
    }
    if (bytes.length < 2) return;
    if (!this.playCtx || this.playCtx.state === 'closed') {
      this.playCtx = new AudioContext({ sampleRate: 24000 });
      this.nextPlayTime = 0;
    }
    const ctx = this.playCtx;
    if (ctx.state === 'suspended') ctx.resume();
    const samples = new Int16Array(bytes.buffer, bytes.byteOffset, Math.floor(bytes.length / 2));
    const buffer = ctx.createBuffer(1, samples.length, 24000);
    const channel = buffer.getChannelData(0);
    for (let i = 0; i < samples.length; i++) channel[i] = samples[i] / 0x8000;
    const source = ctx.createBufferSource();
    source.buffer = buffer;
    source.connect(ctx.destination);
    const startAt = Math.max(ctx.currentTime + 0.02, this.nextPlayTime);
    source.start(startAt);
    this.nextPlayTime = startAt + buffer.duration;
    this.activeSources.add(source);
    source.onended = () => {
      this.activeSources.delete(source);
      this.maybeIdle();
    };
  }

  /** Reply text is done and the audio queue drained → back to listening. */
  private maybeIdle() {
    if (this.closed) return;
    if (this.turnDone && this.activeSources.size === 0) {
      this.cb.onState?.('listening');
    }
  }

  private flushPlayback() {
    this.stopSources();
    this.turnDone = false;
  }

  private stopSources() {
    this.activeSources.forEach((s) => {
      try { s.onended = null; s.stop(); } catch {}
    });
    this.activeSources.clear();
    this.nextPlayTime = 0;
  }

  private teardownMedia() {
    this.flushPlayback();
    this.ready = false;
    if (this.workletNode) {
      this.workletNode.port.onmessage = null;
      this.workletNode.disconnect();
      this.workletNode = null;
    }
    if (this.workletUrl) {
      URL.revokeObjectURL(this.workletUrl);
      this.workletUrl = null;
    }
    this.micStream?.getTracks().forEach((t) => t.stop());
    this.micStream = null;
    this.captureCtx?.close().catch(() => {});
    this.captureCtx = null;
    this.playCtx?.close().catch(() => {});
    this.playCtx = null;
  }
}
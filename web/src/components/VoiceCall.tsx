// Floating voice-call controls (mounted inside Chat).
//
// Only two buttons float above the chat input row: TTS playback mute and
// hang-up, with the live speech-to-text transcript streaming underneath
// them while the user speaks. There is no full-screen overlay — finalized
// transcripts and streamed replies render in the normal chat message list
// via onUserMessage / onAssistantDelta / onAssistantEnd, so voice turns
// persist exactly like typed ones. The mic stays live for the whole call
// (full duplex); the mute button gates speaker playback only.

import { useEffect, useRef, useState } from 'react';
import { useT } from '../i18n';
import { VoiceClient, type VoiceState, type VoiceHistoryMessage } from '../utils/voiceClient';

// App keeps every account's Chat mounted (hidden ones keep streaming), so a
// call left open on a hidden account would keep the mic and speaker live:
// the room hears two answers and both sessions barge-in on each other.
// One live call per browser: claiming hangs up every other mounted
// VoiceCall in this tab, and other tabs of this origin via BroadcastChannel.
const voiceBus = typeof BroadcastChannel !== 'undefined' ? new BroadcastChannel('jcowork-voice-call') : null;
interface VoicePeer { id: number; yield: () => void }
const voicePeers = new Set<VoicePeer>();
let voicePeerSeq = 1;

interface VoiceCallProps {
  token: string;
  conversationId: string;
  /** Persisted chat history sent to the voice gateway on start. */
  history: VoiceHistoryMessage[];
  model?: string;
  onUserMessage: (text: string) => void;
  onAssistantDelta: (content: string) => void;
  /** Finalize the streaming assistant message (turn done or interrupted). */
  onAssistantEnd: () => void;
  /** Surface call errors as a chat toast ('MIC_DENIED' or server message). */
  onError: (message: string) => void;
  /** Call ended (hang-up or disconnect) — Chat resyncs server history. */
  onClose: () => void;
}

export default function VoiceCall({
  token, conversationId, history, model,
  onUserMessage, onAssistantDelta, onAssistantEnd, onError, onClose,
}: VoiceCallProps) {
  const t = useT();
  const [state, setState] = useState<VoiceState>('connecting');
  const [ttsMuted, setTtsMuted] = useState(false);
  const [partial, setPartial] = useState('');
  const clientRef = useRef<VoiceClient | null>(null);
  // Callbacks may arrive from a stale client (StrictMode remount) — ignore.
  const onCloseRef = useRef(onClose);
  onCloseRef.current = onClose;
  const onErrorRef = useRef(onError);
  onErrorRef.current = onError;

  useEffect(() => {
    let client: VoiceClient;
    const current = () => clientRef.current === client;
    client = new VoiceClient(
      { token, conversationId, history, model },
      {
        onState: (s) => { if (current()) setState(s); },
        onAsrPartial: (text) => { if (current()) setPartial(text); },
        onAsrFinal: (text) => {
          if (!current()) return;
          setPartial('');
          onUserMessage(text);
        },
        onTextDelta: (content) => { if (current()) onAssistantDelta(content); },
        onTurnEnd: () => { if (current()) onAssistantEnd(); },
        onError: (message) => {
          if (!current()) return;
          onErrorRef.current(message);
          // Without a microphone there is no call — end it after the toast.
          if (message === 'MIC_DENIED') onCloseRef.current();
        },
        onClose: () => { if (current()) onCloseRef.current(); },
      },
    );
    clientRef.current = client;
    // Register before claiming so a concurrent claim can yield us, then
    // take over: every other live call in this browser ends right now.
    const peer: VoicePeer = {
      id: voicePeerSeq++,
      yield: () => {
        client.hangup();
        if (clientRef.current === client) onCloseRef.current();
      },
    };
    voicePeers.add(peer);
    const onBus = (ev: MessageEvent) => {
      if (ev.data?.claim !== peer.id) peer.yield();
    };
    voiceBus?.addEventListener('message', onBus);
    for (const p of voicePeers) if (p !== peer) p.yield();
    voiceBus?.postMessage({ claim: peer.id });
    client.start();
    return () => {
      voicePeers.delete(peer);
      voiceBus?.removeEventListener('message', onBus);
      if (clientRef.current === client) clientRef.current = null;
      client.hangup();
    };
    // History/model are a snapshot taken when the call opens; the call must
    // not restart on every Chat re-render.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [token, conversationId]);

  const toggleTts = () => {
    const next = !ttsMuted;
    setTtsMuted(next);
    clientRef.current?.setTtsMuted(next);
  };

  const hangup = () => {
    clientRef.current?.hangup();
    onClose();
  };

  // Glow/pulse on the speaker button while audio is actually playing.
  const speaking = state === 'speaking' && !ttsMuted;

  return (
    <div
      style={{
        position: 'fixed',
        left: '50%',
        transform: 'translateX(-50%)',
        bottom: 76,
        zIndex: 1000,
        display: 'flex',
        flexDirection: 'column',
        alignItems: 'center',
        gap: 10,
        maxWidth: '92vw',
      }}
    >
      <style>{`@keyframes voicePulse { 0%,100% { opacity: 1; } 50% { opacity: 0.45; } }`}</style>
      <div style={{ display: 'flex', gap: 32, alignItems: 'center' }}>
      <div style={{ display: 'flex', flexDirection: 'column', alignItems: 'center', gap: 6 }}>
        <button
          onClick={toggleTts}
          title={ttsMuted ? t('voiceTtsUnmute') : t('voiceTtsMute')}
          style={{
            width: 56,
            height: 56,
            borderRadius: '50%',
            border: '1px solid #444',
            background: ttsMuted ? '#3a3a1a' : '#1f3a2a',
            color: ttsMuted ? '#f5d76e' : '#7fd58f',
            fontSize: 22,
            cursor: 'pointer',
            boxShadow: speaking
              ? '0 0 16px rgba(127, 213, 143, 0.55)'
              : '0 4px 14px rgba(0, 0, 0, 0.5)',
            animation: speaking ? 'voicePulse 1.2s ease-in-out infinite' : undefined,
          }}
        >
          {ttsMuted ? '🔇' : '🔊'}
        </button>
        <span style={{ fontSize: 11, color: '#999', textShadow: '0 1px 4px rgba(0,0,0,0.8)' }}>
          {ttsMuted ? t('voiceTtsUnmute') : t('voiceTtsMute')}
        </span>
      </div>
      <div style={{ display: 'flex', flexDirection: 'column', alignItems: 'center', gap: 6 }}>
        <button
          onClick={hangup}
          title={t('voiceHangup')}
          style={{
            width: 64,
            height: 64,
            borderRadius: '50%',
            border: 'none',
            background: '#e53935',
            color: '#fff',
            fontSize: 26,
            cursor: 'pointer',
            boxShadow: '0 4px 18px rgba(229, 57, 53, 0.4)',
          }}
        >
          ✕
        </button>
        <span style={{ fontSize: 11, color: '#999', textShadow: '0 1px 4px rgba(0,0,0,0.8)' }}>
          {t('voiceHangup')}
        </span>
      </div>
      </div>

      {/* Live speech-to-text while the user is speaking */}
      {partial && (
        <div
          style={{
            maxWidth: 560,
            maxHeight: 84,
            overflowY: 'auto',
            padding: '6px 12px',
            borderRadius: 10,
            background: 'rgba(20, 32, 24, 0.88)',
            border: '1px solid #2c4a34',
            color: '#a8c8ae',
            fontSize: 13,
            lineHeight: 1.5,
            textAlign: 'center',
            whiteSpace: 'pre-wrap',
            wordBreak: 'break-word',
          }}
        >
          {partial}
        </div>
      )}
    </div>
  );
}
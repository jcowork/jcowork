import { useState, useEffect, useRef, useCallback } from 'react';
import ReactMarkdown from 'react-markdown';
import remarkGfm from 'remark-gfm';

interface Message {
  role: 'user' | 'assistant' | 'system';
  content: string;
  timestamp: number;
  streaming?: boolean;
}

interface ChatProps {
  userId: string;
  token: string;
}

const STORAGE_KEY = (userId: string) => `jcowork_chat_${userId}`;

function loadMessages(userId: string): Message[] {
  try {
    const saved = localStorage.getItem(STORAGE_KEY(userId));
    if (saved) {
      const parsed = JSON.parse(saved);
      // Filter out streaming messages from previous sessions
      return parsed.filter((m: Message) => !m.streaming);
    }
  } catch {}
  return [];
}

function saveMessages(userId: string, messages: Message[]) {
  try {
    // Don't save streaming or system tool messages
    const toSave = messages.filter(
      (m) => !m.streaming && m.role !== 'system'
    );
    localStorage.setItem(STORAGE_KEY(userId), JSON.stringify(toSave));
  } catch {}
}

export default function Chat({ userId, token }: ChatProps) {
  const [messages, setMessages] = useState<Message[]>(() => loadMessages(userId));
  const [input, setInput] = useState('');
  const [connected, setConnected] = useState(false);
  const [streaming, setStreaming] = useState(false);
  const [alarmActive, setAlarmActive] = useState(false);
  const wsRef = useRef<WebSocket | null>(null);
  const messagesEndRef = useRef<HTMLDivElement>(null);
  const alarmRef = useRef<{ ctx: AudioContext | null; timer: number | null; playing: boolean; beepInterval: number | null }>({ ctx: null, timer: null, playing: false, beepInterval: null });

  const connect = useCallback(() => {
    const protocol = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
    const ws = new WebSocket(`${protocol}//${window.location.host}/api/ws?token=${encodeURIComponent(token)}`);

    ws.onopen = () => setConnected(true);
    ws.onclose = () => {
      setConnected(false);
      setTimeout(connect, 3000);
    };
    ws.onmessage = (event) => {
      const data = JSON.parse(event.data);
      if (data.type === 'text_delta') {
        setMessages((prev) => {
          const last = prev[prev.length - 1];
          if (last?.role === 'assistant' && last.streaming) {
            return [
              ...prev.slice(0, -1),
              { ...last, content: last.content + data.content },
            ];
          }
          return [
            ...prev,
            { role: 'assistant', content: data.content, timestamp: Date.now(), streaming: true } as Message,
          ];
        });
      } else if (data.type === 'done') {
        setStreaming(false);
        setMessages((prev) => {
          const last = prev[prev.length - 1];
          if (last?.role === 'assistant') {
            return [...prev.slice(0, -1), { ...last, streaming: false }];
          }
          return prev;
        });
      } else if (data.type === 'tool_call_start') {
        setMessages((prev) => [
          ...prev,
          { role: 'system', content: `Calling tool: ${data.name}`, timestamp: Date.now() },
        ]);
      } else if (data.type === 'tool_call_end') {
        setMessages((prev) => [
          ...prev,
          { role: 'system', content: `Tool ${data.name} completed`, timestamp: Date.now() },
        ]);
      } else if (data.type === 'reminder') {
        setMessages((prev) => [
          ...prev,
          { role: 'system', content: `🔔 Reminder: ${data.message}`, timestamp: Date.now() },
        ]);
        playAlarm();
      } else if (data.type === 'error') {
        setStreaming(false);
        setMessages((prev) => [
          ...prev,
          { role: 'system', content: `Error: ${data.message}`, timestamp: Date.now() },
        ]);
      }
    };

    wsRef.current = ws;
  }, [userId, token]);

  useEffect(() => {
    connect();
    return () => wsRef.current?.close();
  }, [connect]);

  useEffect(() => {
    messagesEndRef.current?.scrollIntoView({ behavior: 'smooth' });
  }, [messages]);

  // Persist messages to localStorage when they change
  useEffect(() => {
    saveMessages(userId, messages);
  }, [userId, messages]);

  const clearHistory = () => {
    setMessages([]);
    localStorage.removeItem(STORAGE_KEY(userId));
  };

  const playAlarm = () => {
    // Stop any existing alarm first
    stopAlarm();
    try {
      // Resume or create AudioContext (handle autoplay policy)
      let ctx = alarmRef.current.ctx;
      if (!ctx || ctx.state === 'closed') {
        ctx = new AudioContext();
        alarmRef.current.ctx = ctx;
      }
      if (ctx.state === 'suspended') {
        ctx.resume();
      }
      alarmRef.current.playing = true;
      setAlarmActive(true);

      const playBeep = () => {
        if (!alarmRef.current.playing || !alarmRef.current.ctx) return;
        const currentCtx = alarmRef.current.ctx;
        if (currentCtx.state === 'closed') return;
        try {
          const osc = currentCtx.createOscillator();
          const gain = currentCtx.createGain();
          osc.connect(gain);
          gain.connect(currentCtx.destination);
          osc.frequency.value = 880; // A5 note
          osc.type = 'sine';
          gain.gain.setValueAtTime(0.3, currentCtx.currentTime);
          gain.gain.exponentialRampToValueAtTime(0.01, currentCtx.currentTime + 0.5);
          osc.start(currentCtx.currentTime);
          osc.stop(currentCtx.currentTime + 0.5);
        } catch (e) {
          // Ignore errors from closed context
        }
      };

      // Play beep immediately and every 1 second
      playBeep();
      const beepInterval = window.setInterval(() => {
        if (!alarmRef.current.playing) {
          clearInterval(beepInterval);
          return;
        }
        playBeep();
      }, 1000);

      // Auto-stop after 30 seconds
      alarmRef.current.timer = window.setTimeout(() => {
        clearInterval(beepInterval);
        stopAlarm();
      }, 30000);

      // Store interval for cleanup
      alarmRef.current.beepInterval = beepInterval;
    } catch (e) {
      console.error('Failed to play alarm:', e);
    }
  };

  const stopAlarm = () => {
    alarmRef.current.playing = false;
    if (alarmRef.current.beepInterval) {
      clearInterval(alarmRef.current.beepInterval);
      alarmRef.current.beepInterval = null;
    }
    if (alarmRef.current.timer) {
      clearTimeout(alarmRef.current.timer);
      alarmRef.current.timer = null;
    }
    if (alarmRef.current.ctx) {
      alarmRef.current.ctx.close();
      alarmRef.current.ctx = null;
    }
    setAlarmActive(false);
  };

  const sendMessage = () => {
    if (!input.trim() || !connected || streaming) return;

    const msg: Message = { role: 'user', content: input, timestamp: Date.now() };
    setMessages((prev) => [...prev, msg]);
    setStreaming(true);

    // Read model from localStorage (set by Settings page)
    let model: string | undefined;
    const saved = localStorage.getItem(`jcowork_model_${userId}`);
    if (saved) {
      try {
        const parsed = JSON.parse(saved);
        if (parsed.provider && parsed.model) {
          model = `${parsed.provider}:${parsed.model}`;
        }
      } catch {}
    }

    wsRef.current?.send(
      JSON.stringify({ content: input, model })
    );
    setInput('');
  };

  return (
    <div style={{ display: 'flex', flexDirection: 'column', height: '100%' }}>
      <div style={{ padding: '8px 16px', borderBottom: '1px solid #333', display: 'flex', alignItems: 'center', gap: 8, justifyContent: 'space-between' }}>
        <div style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
          <span style={{ fontWeight: 600 }}>Jcowork Agent</span>
          <span style={{ width: 8, height: 8, borderRadius: '50%', background: connected ? '#4caf50' : '#f44336' }} />
          {alarmActive && (
            <button
              onClick={stopAlarm}
              style={{
                padding: '4px 12px',
                borderRadius: 6,
                border: 'none',
                background: '#e53935',
                color: '#fff',
                cursor: 'pointer',
                fontSize: 12,
                fontWeight: 600,
                animation: 'pulse 1s infinite',
              }}
            >
              🔔 Stop Alarm
            </button>
          )}
        </div>
        <button
          onClick={clearHistory}
          disabled={messages.length === 0}
          style={{
            padding: '4px 12px',
            borderRadius: 6,
            border: '1px solid #555',
            background: 'transparent',
            color: messages.length === 0 ? '#555' : '#e57373',
            cursor: messages.length === 0 ? 'not-allowed' : 'pointer',
            fontSize: 12,
          }}
        >
          Clear
        </button>
      </div>

      <div style={{ flex: 1, overflowY: 'auto', padding: 16 }}>
        {messages.map((msg, i) => {
          const isUser = msg.role === 'user';
          const isSystem = msg.role === 'system';
          return (
            <div
              key={i}
              style={{
                marginBottom: 12,
                padding: isUser ? '8px 12px' : '12px 16px',
                borderRadius: 8,
                background: isUser ? '#1a73e8' : isSystem ? '#333' : '#2a2a2a',
                color: '#eee',
                marginLeft: isUser ? 'auto' : 0,
                marginRight: isUser ? 0 : 'auto',
                maxWidth: isUser ? '80%' : '85%',
                lineHeight: 1.6,
              }}
            >
              {isUser || isSystem ? (
                <span style={{ whiteSpace: 'pre-wrap' }}>{msg.content}</span>
              ) : (
                <div className="markdown-body">
                  <ReactMarkdown remarkPlugins={[remarkGfm]}>{msg.content}</ReactMarkdown>
                </div>
              )}
              {msg.streaming && <span className="cursor">|</span>}
            </div>
          );
        })}
        <div ref={messagesEndRef} />
      </div>

      <div style={{ padding: 16, borderTop: '1px solid #333', display: 'flex', gap: 8 }}>
        <input
          type="text"
          value={input}
          onChange={(e) => setInput(e.target.value)}
          onKeyDown={(e) => e.key === 'Enter' && sendMessage()}
          placeholder={connected ? 'Type a message...' : 'Connecting...'}
          disabled={!connected || streaming}
          style={{
            flex: 1,
            padding: '8px 12px',
            borderRadius: 8,
            border: '1px solid #555',
            background: '#1a1a1a',
            color: '#eee',
            fontSize: 14,
            outline: 'none',
          }}
        />
        <button
          onClick={sendMessage}
          disabled={!connected || streaming || !input.trim()}
          style={{
            padding: '8px 20px',
            borderRadius: 8,
            border: 'none',
            background: connected ? '#1a73e8' : '#555',
            color: '#fff',
            cursor: connected ? 'pointer' : 'not-allowed',
          }}
        >
          Send
        </button>
      </div>
    </div>
  );
}

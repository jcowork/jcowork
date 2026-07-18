import { useState, useEffect, useRef, useCallback } from 'react';
import ReactMarkdown from 'react-markdown';
import remarkGfm from 'remark-gfm';

interface Message {
  role: 'user' | 'assistant' | 'system';
  content: string;
  timestamp: number;
  streaming?: boolean;
}

interface ContextDoc {
  name: string;
  path: string;
  content: string;
}

interface ChatProps {
  userId: string;
  token: string;
}

// Copy button for message bubbles
function CopyButton({ text }: { text: string }) {
  const [copied, setCopied] = useState(false);
  const handleCopy = async (e: React.MouseEvent) => {
    e.stopPropagation();
    try {
      await navigator.clipboard.writeText(text);
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    } catch {
      // fallback
      const ta = document.createElement('textarea');
      ta.value = text;
      document.body.appendChild(ta);
      ta.select();
      document.execCommand('copy');
      document.body.removeChild(ta);
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    }
  };
  return (
    <button
      onClick={handleCopy}
      title={copied ? 'Copied!' : 'Copy'}
      style={{
        background: 'none',
        border: 'none',
        color: copied ? '#4caf50' : '#888',
        cursor: 'pointer',
        fontSize: 12,
        padding: '2px 6px',
        borderRadius: 4,
        transition: 'color 0.2s',
      }}
    >
      {copied ? '✓' : '⧉'}
    </button>
  );
}

// Download button for message bubbles
function DownloadButton({ text, filename }: { text: string; filename?: string }) {
  const handleDownload = (e: React.MouseEvent) => {
    e.stopPropagation();
    // Detect HTML content
    const isHtml = text.trimStart().startsWith('<!DOCTYPE') || text.trimStart().startsWith('<html');
    const blob = new Blob([text], { type: isHtml ? 'text/html;charset=utf-8' : 'text/plain;charset=utf-8' });
    const url = URL.createObjectURL(blob);
    const a = document.createElement('a');
    a.href = url;
    a.download = filename || (isHtml ? 'presentation.html' : 'message.txt');
    document.body.appendChild(a);
    a.click();
    document.body.removeChild(a);
    URL.revokeObjectURL(url);
  };
  return (
    <button
      onClick={handleDownload}
      title="Download"
      style={{
        background: 'none',
        border: 'none',
        color: '#888',
        cursor: 'pointer',
        fontSize: 12,
        padding: '2px 6px',
        borderRadius: 4,
        transition: 'color 0.2s',
      }}
      onMouseEnter={e => (e.currentTarget.style.color = '#ccc')}
      onMouseLeave={e => (e.currentTarget.style.color = '#888')}
    >
      ⬇
    </button>
  );
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

  // Document picker state
  const [showDocPicker, setShowDocPicker] = useState(false);
  const [workspaceFiles, setWorkspaceFiles] = useState<string[]>([]);
  const [workspaceFilesLoading, setWorkspaceFilesLoading] = useState(false);
  const [selectedDocs, setSelectedDocs] = useState<ContextDoc[]>([]);
  const [docPickerSearch, setDocPickerSearch] = useState('');
  const pdfInputRef = useRef<HTMLInputElement>(null);
  const [pdfUploading, setPdfUploading] = useState(false);
  const docPickerRef = useRef<HTMLDivElement>(null);

  // URL input state
  const [showUrlInput, setShowUrlInput] = useState(false);
  const [urlInput, setUrlInput] = useState('');
  const [urlFetching, setUrlFetching] = useState(false);
  const urlInputRef = useRef<HTMLDivElement>(null);

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

  // Close doc picker on outside click
  useEffect(() => {
    const handleClickOutside = (e: MouseEvent) => {
      if (docPickerRef.current && !docPickerRef.current.contains(e.target as Node)) {
        setShowDocPicker(false);
      }
      if (urlInputRef.current && !urlInputRef.current.contains(e.target as Node)) {
        setShowUrlInput(false);
      }
    };
    document.addEventListener('mousedown', handleClickOutside);
    return () => document.removeEventListener('mousedown', handleClickOutside);
  }, []);

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

    // Build context_documents from selected docs
    const context_documents = selectedDocs.length > 0
      ? selectedDocs.map(d => ({ name: d.name, path: d.path, content: d.content }))
      : undefined;

    wsRef.current?.send(
      JSON.stringify({ content: input, model, context_documents })
    );
    setInput('');
    // Keep selected docs visible until the user manually removes them
  };

  // Fetch workspace files for the doc picker
  const fetchWorkspaceFiles = async () => {
    setWorkspaceFilesLoading(true);
    try {
      const res = await fetch('/api/workspace/files-recursive?path=.', {
        headers: { Authorization: `Bearer ${token}` },
      });
      if (res.ok) {
        const files: string[] = await res.json();
        setWorkspaceFiles(files);
      }
    } catch (e) {
      console.error('Failed to fetch workspace files:', e);
    }
    setWorkspaceFilesLoading(false);
  };

  const openDocPicker = () => {
    setShowDocPicker(true);
    setDocPickerSearch('');
    fetchWorkspaceFiles();
  };

  const selectDoc = async (filePath: string) => {
    // Don't add if already selected
    if (selectedDocs.some(d => d.path === filePath)) return;

    // Fetch file content
    try {
      const res = await fetch(`/api/workspace/download?path=${encodeURIComponent(filePath)}`, {
        headers: { Authorization: `Bearer ${token}` },
      });
      if (res.ok) {
        const content = await res.text();
        const name = filePath.split('/').pop() || filePath;
        setSelectedDocs(prev => [...prev, { name, path: filePath, content }]);
      }
    } catch (e) {
      console.error('Failed to fetch doc:', e);
    }
    setShowDocPicker(false);
  };

  const removeDoc = (index: number) => {
    setSelectedDocs(prev => prev.filter((_, i) => i !== index));
  };

  const handlePdfUpload = async (e: React.ChangeEvent<HTMLInputElement>) => {
    const files = e.target.files;
    if (!files || files.length === 0) return;

    setPdfUploading(true);
    const formData = new FormData();
    for (let i = 0; i < files.length; i++) {
      formData.append('file', files[i]);
    }

    try {
      const res = await fetch('/api/workspace/upload-pdf', {
        method: 'POST',
        headers: { Authorization: `Bearer ${token}` },
        body: formData,
      });
      if (res.ok) {
        const data = await res.json();
        if (data.files) {
          for (const f of data.files) {
            setSelectedDocs(prev => [...prev, {
              name: f.filename,
              path: f.path,
              content: f.text || '[PDF parsing failed]',
            }]);
          }
        }
      }
    } catch (e) {
      console.error('PDF upload failed:', e);
    }
    setPdfUploading(false);
    // Reset input so same file can be re-uploaded
    if (pdfInputRef.current) pdfInputRef.current.value = '';
  };

  // Fetch URL content and add as context document
  const handleUrlSubmit = async () => {
    const url = urlInput.trim();
    if (!url) return;

    // Don't add if already selected
    if (selectedDocs.some(d => d.path === url)) {
      setShowUrlInput(false);
      setUrlInput('');
      return;
    }

    setUrlFetching(true);
    try {
      const res = await fetch('/api/fetch-url', {
        method: 'POST',
        headers: {
          'Content-Type': 'application/json',
          Authorization: `Bearer ${token}`,
        },
        body: JSON.stringify({ url }),
      });
      if (res.ok) {
        const data = await res.json();
        const name = data.title || url;
        setSelectedDocs(prev => [...prev, {
          name,
          path: url,
          content: data.text || '[Failed to extract content]',
        }]);
      } else {
        const err = await res.json().catch(() => ({}));
        console.error('URL fetch failed:', err.error || res.statusText);
      }
    } catch (e) {
      console.error('URL fetch error:', e);
    }
    setUrlFetching(false);
    setShowUrlInput(false);
    setUrlInput('');
  };

  const filteredFiles = workspaceFiles.filter(f =>
    f.toLowerCase().includes(docPickerSearch.toLowerCase())
  );

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
        <div className="chat-messages-inner">
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
                maxWidth: isUser ? '75%' : '72%',
                lineHeight: 1.6,
                position: 'relative',
              }}
            >
              <div style={{ userSelect: 'text', WebkitUserSelect: 'text' }}>
                {isUser || isSystem ? (
                  <span style={{ whiteSpace: 'pre-wrap' }}>{msg.content}</span>
                ) : (
                  <div className="markdown-body">
                    <ReactMarkdown remarkPlugins={[remarkGfm]}>{msg.content}</ReactMarkdown>
                  </div>
                )}
              </div>
              {msg.streaming && <span className="cursor">|</span>}
              {/* Copy button — top-right of bubble, shown on hover */}
              {!msg.streaming && (
                <div
                  style={{
                    position: 'absolute',
                    top: 6,
                    right: 6,
                    opacity: 0,
                    transition: 'opacity 0.15s',
                  }}
                  className="msg-copy-btn"
                >
                  <DownloadButton text={msg.content} />
                  <CopyButton text={msg.content} />
                </div>
              )}
            </div>
          );
        })}
        <div ref={messagesEndRef} />
        </div>
      </div>

      <div style={{ padding: 16, borderTop: '1px solid #333' }}>
        {/* Selected documents chips */}
        {selectedDocs.length > 0 && (
          <div style={{ display: 'flex', flexWrap: 'wrap', gap: 6, marginBottom: 8 }}>
            {selectedDocs.map((doc, i) => (
              <div
                key={i}
                style={{
                  display: 'flex',
                  alignItems: 'center',
                  gap: 4,
                  padding: '4px 4px 4px 10px',
                  borderRadius: 12,
                  background: '#1e3a5a',
                  border: '1px solid #2d5a8a',
                  fontSize: 12,
                  color: '#8ab4f8',
                }}
              >
                <span style={{ fontSize: 11 }}>{doc.path.startsWith('http') ? '🔗' : doc.path.endsWith('.pdf') ? '📕' : '📄'}</span>
                <span style={{ maxWidth: 140, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
                  {doc.name}
                </span>
                <button
                  onClick={() => removeDoc(i)}
                  title="Remove this document from context"
                  style={{
                    display: 'flex',
                    alignItems: 'center',
                    justifyContent: 'center',
                    width: 18,
                    height: 18,
                    borderRadius: '50%',
                    border: 'none',
                    background: 'transparent',
                    color: '#8ab4f8',
                    cursor: 'pointer',
                    fontSize: 13,
                    lineHeight: 1,
                    padding: 0,
                    transition: 'background 0.15s, color 0.15s',
                  }}
                  onMouseEnter={e => { e.currentTarget.style.background = '#e53935'; e.currentTarget.style.color = '#fff'; }}
                  onMouseLeave={e => { e.currentTarget.style.background = 'transparent'; e.currentTarget.style.color = '#8ab4f8'; }}
                >
                  ✕
                </button>
              </div>
            ))}
          </div>
        )}

        {/* Input row */}
        <div style={{ display: 'flex', gap: 8, alignItems: 'flex-end' }}>
          {/* Doc picker button */}
          <div style={{ position: 'relative' }} ref={docPickerRef}>
            <button
              onClick={openDocPicker}
              disabled={!connected || streaming}
              title="Attach document from workspace"
              style={{
                padding: '8px 10px',
                borderRadius: 8,
                border: '1px solid #555',
                background: selectedDocs.length > 0 ? '#1e3a5a' : '#1a1a1a',
                color: selectedDocs.length > 0 ? '#8ab4f8' : '#aaa',
                cursor: 'pointer',
                fontSize: 14,
                lineHeight: 1,
                flexShrink: 0,
              }}
            >
              📎
            </button>

            {/* Doc picker dropdown */}
            {showDocPicker && (
              <div
                style={{
                  position: 'absolute',
                  bottom: '100%',
                  left: 0,
                  marginBottom: 4,
                  width: 280,
                  maxHeight: 320,
                  background: '#1a1a1a',
                  border: '1px solid #333',
                  borderRadius: 8,
                  boxShadow: '0 4px 16px rgba(0,0,0,0.5)',
                  display: 'flex',
                  flexDirection: 'column',
                  zIndex: 100,
                }}
              >
                <div style={{ padding: '8px 10px', borderBottom: '1px solid #333' }}>
                  <input
                    type="text"
                    placeholder="Search files..."
                    value={docPickerSearch}
                    onChange={e => setDocPickerSearch(e.target.value)}
                    style={{
                      width: '100%',
                      padding: '5px 8px',
                      borderRadius: 4,
                      border: '1px solid #444',
                      background: '#111',
                      color: '#eee',
                      fontSize: 12,
                      outline: 'none',
                      boxSizing: 'border-box',
                    }}
                    autoFocus
                  />
                </div>
                <div style={{ flex: 1, overflowY: 'auto', padding: '4px' }}>
                  {workspaceFilesLoading ? (
                    <div style={{ padding: 12, textAlign: 'center', color: '#555', fontSize: 12 }}>Loading...</div>
                  ) : filteredFiles.length === 0 ? (
                    <div style={{ padding: 12, textAlign: 'center', color: '#555', fontSize: 12 }}>No files found</div>
                  ) : (
                    filteredFiles.map(f => (
                      <div
                        key={f}
                        onClick={() => selectDoc(f)}
                        style={{
                          padding: '5px 8px',
                          borderRadius: 4,
                          cursor: 'pointer',
                          fontSize: 12,
                          color: selectedDocs.some(d => d.path === f) ? '#4caf50' : '#ccc',
                          display: 'flex',
                          alignItems: 'center',
                          gap: 6,
                        }}
                        onMouseEnter={e => { e.currentTarget.style.background = '#2a2a2a'; }}
                        onMouseLeave={e => { e.currentTarget.style.background = 'transparent'; }}
                      >
                        <span>{f.endsWith('.pdf') ? '📕' : f.endsWith('.html') ? '🌐' : f.endsWith('.py') ? '🐍' : '📄'}</span>
                        <span style={{ overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap', flex: 1 }}>{f}</span>
                        {selectedDocs.some(d => d.path === f) && <span style={{ color: '#4caf50', fontSize: 10 }}>✓</span>}
                      </div>
                    ))
                  )}
                </div>
              </div>
            )}
          </div>

          {/* PDF upload button */}
          <button
            onClick={() => pdfInputRef.current?.click()}
            disabled={!connected || streaming || pdfUploading}
            title="Upload PDF file"
            style={{
              padding: '8px 10px',
              borderRadius: 8,
              border: '1px solid #555',
              background: '#1a1a1a',
              color: '#aaa',
              cursor: 'pointer',
              fontSize: 14,
              lineHeight: 1,
              flexShrink: 0,
            }}
          >
            {pdfUploading ? '⏳' : '📕'}
          </button>
          <input
            ref={pdfInputRef}
            type="file"
            accept=".pdf"
            multiple
            style={{ display: 'none' }}
            onChange={handlePdfUpload}
          />

          {/* URL reference button */}
          <div style={{ position: 'relative' }} ref={urlInputRef}>
            <button
              onClick={() => { setShowUrlInput(!showUrlInput); setUrlInput(''); }}
              disabled={!connected || streaming || urlFetching}
              title="Add web page URL as context"
              style={{
                padding: '8px 10px',
                borderRadius: 8,
                border: '1px solid #555',
                background: '#1a1a1a',
                color: '#aaa',
                cursor: 'pointer',
                fontSize: 14,
                lineHeight: 1,
                flexShrink: 0,
              }}
            >
              {urlFetching ? '⏳' : '🔗'}
            </button>

            {/* URL input popover */}
            {showUrlInput && (
              <div
                style={{
                  position: 'absolute',
                  bottom: '100%',
                  left: 0,
                  marginBottom: 4,
                  width: 300,
                  background: '#1a1a1a',
                  border: '1px solid #333',
                  borderRadius: 8,
                  boxShadow: '0 4px 16px rgba(0,0,0,0.5)',
                  padding: 12,
                  zIndex: 100,
                }}
              >
                <div style={{ fontSize: 12, color: '#888', marginBottom: 6 }}>Enter web page URL:</div>
                <div style={{ display: 'flex', gap: 6 }}>
                  <input
                    type="text"
                    placeholder="https://example.com/page"
                    value={urlInput}
                    onChange={e => setUrlInput(e.target.value)}
                    onKeyDown={e => { if (e.key === 'Enter') handleUrlSubmit(); }}
                    autoFocus
                    style={{
                      flex: 1,
                      padding: '6px 8px',
                      borderRadius: 4,
                      border: '1px solid #444',
                      background: '#111',
                      color: '#eee',
                      fontSize: 12,
                      outline: 'none',
                    }}
                  />
                  <button
                    onClick={handleUrlSubmit}
                    disabled={!urlInput.trim() || urlFetching}
                    style={{
                      padding: '6px 12px',
                      borderRadius: 4,
                      border: 'none',
                      background: urlInput.trim() ? '#1a73e8' : '#333',
                      color: '#fff',
                      cursor: urlInput.trim() ? 'pointer' : 'not-allowed',
                      fontSize: 12,
                      whiteSpace: 'nowrap',
                    }}
                  >
                    {urlFetching ? '...' : 'Add'}
                  </button>
                </div>
              </div>
            )}
          </div>

          {/* Text input */}
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
    </div>
  );
}

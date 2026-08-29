import { useState, useEffect, useCallback } from 'react';
import { useT } from '../i18n';

interface Connector {
  id: string;
  user_id: string;
  name: string;
  ctype: string; // 'api' | 'mcp'
  description: string;
  config: any;
  tool_states: Record<string, boolean>;
  enabled: boolean;
  created_at: string;
  updated_at: string;
}

interface ToolInfo {
  name: string;
  description: string;
  parameters: any;
  enabled: boolean;
}

interface ApiToolForm {
  name: string;
  description: string;
  method: string;
  url: string;
  headersText: string;
  paramsText: string;
  bodyTemplate: string;
  enabled: boolean;
}

interface FormState {
  name: string;
  description: string;
  ctype: 'api' | 'mcp';
  tools: ApiToolForm[];
  transport: 'stdio' | 'http';
  command: string;
  argsText: string;
  envText: string;
  mcpUrl: string;
  mcpHeadersText: string;
}

interface ConnectorsProps {
  userId: string;
  token: string;
}

// Parse "Key: Value" or "KEY=VALUE" lines into an object.
const parseKV = (text: string, sep: string): Record<string, string> => {
  const out: Record<string, string> = {};
  for (const line of text.split('\n')) {
    const idx = line.indexOf(sep);
    if (idx <= 0) continue;
    const k = line.slice(0, idx).trim();
    const v = line.slice(idx + sep.length).trim();
    if (k) out[k] = v;
  }
  return out;
};

const formatKV = (obj: Record<string, string> | undefined, sep: string): string =>
  obj ? Object.entries(obj).map(([k, v]) => `${k}${sep}${v}`).join('\n') : '';

const tryParseJSON = (text: string): any => {
  if (!text.trim()) return null;
  try {
    return JSON.parse(text);
  } catch {
    return null;
  }
};

const emptyTool = (): ApiToolForm => ({
  name: '',
  description: '',
  method: 'GET',
  url: '',
  headersText: '',
  paramsText: '{\n  "type": "object",\n  "properties": {}\n}',
  bodyTemplate: '',
  enabled: true,
});

const emptyForm = (): FormState => ({
  name: '',
  description: '',
  ctype: 'api',
  tools: [emptyTool()],
  transport: 'stdio',
  command: '',
  argsText: '',
  envText: '',
  mcpUrl: '',
  mcpHeadersText: '',
});

const formFromConnector = (c: Connector): FormState => {
  const base = emptyForm();
  base.name = c.name;
  base.description = c.description;
  base.ctype = c.ctype === 'mcp' ? 'mcp' : 'api';
  if (c.ctype === 'api') {
    const tools = (c.config?.tools || []) as any[];
    base.tools = tools.length > 0 ? tools.map(t => ({
      name: t.name || '',
      description: t.description || '',
      method: t.method || 'GET',
      url: t.url || '',
      headersText: formatKV(t.headers, ':'),
      paramsText: t.params ? JSON.stringify(t.params, null, 2) : emptyTool().paramsText,
      bodyTemplate: t.body_template || '',
      enabled: t.enabled !== false,
    })) : [emptyTool()];
  } else {
    base.transport = c.config?.transport === 'http' ? 'http' : 'stdio';
    base.command = c.config?.command || '';
    base.argsText = (c.config?.args || []).join(' ');
    base.envText = formatKV(c.config?.env, '=');
    base.mcpUrl = c.config?.url || '';
    base.mcpHeadersText = formatKV(c.config?.headers, ':');
  }
  return base;
};

export default function Connectors({ userId: _userId, token }: ConnectorsProps) {
  const t = useT();
  const [connectors, setConnectors] = useState<Connector[]>([]);
  const [loading, setLoading] = useState(true);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [tools, setTools] = useState<ToolInfo[]>([]);
  const [toolsLoading, setToolsLoading] = useState(false);
  const [toolsError, setToolsError] = useState('');
  const [showForm, setShowForm] = useState(false);
  const [editingId, setEditingId] = useState<string | null>(null);
  const [form, setForm] = useState<FormState>(emptyForm());
  const [submitting, setSubmitting] = useState(false);
  const [formError, setFormError] = useState('');
  const [testState, setTestState] = useState<'idle' | 'testing' | 'ok' | 'error'>('idle');
  const [testMsg, setTestMsg] = useState('');
  const [expandedParams, setExpandedParams] = useState<Record<string, boolean>>({});

  const authHeaders = { 'Authorization': `Bearer ${token}` };

  const fetchConnectors = useCallback(async () => {
    try {
      const res = await fetch('/api/connectors', { headers: authHeaders });
      if (res.ok) setConnectors(await res.json());
    } catch (err) {
      console.error('Failed to fetch connectors:', err);
    }
  }, [token]);

  const fetchTools = useCallback(async (id: string) => {
    setToolsLoading(true);
    setToolsError('');
    try {
      const res = await fetch(`/api/connectors/${id}/tools`, { headers: authHeaders });
      if (res.ok) {
        setTools(await res.json());
      } else {
        const data = await res.json().catch(() => ({}));
        setTools([]);
        setToolsError(data.error || 'Failed to load tools');
      }
    } catch (err) {
      setTools([]);
      setToolsError(String(err));
    } finally {
      setToolsLoading(false);
    }
  }, [token]);

  useEffect(() => {
    fetchConnectors().finally(() => setLoading(false));
  }, [fetchConnectors]);

  const selectConnector = (id: string) => {
    setSelectedId(id);
    setExpandedParams({});
    fetchTools(id);
  };

  const toggleConnector = async (c: Connector) => {
    try {
      const res = await fetch(`/api/connectors/${c.id}/toggle`, {
        method: 'POST',
        headers: { ...authHeaders, 'Content-Type': 'application/json' },
        body: JSON.stringify({ enabled: !c.enabled }),
      });
      if (res.ok) {
        setConnectors(prev => prev.map(x => x.id === c.id ? { ...x, enabled: !c.enabled } : x));
      }
    } catch (err) {
      console.error('Failed to toggle connector:', err);
    }
  };

  const toggleTool = async (tool: ToolInfo) => {
    if (!selectedId) return;
    try {
      const res = await fetch(`/api/connectors/${selectedId}/tools/${encodeURIComponent(tool.name)}/toggle`, {
        method: 'POST',
        headers: { ...authHeaders, 'Content-Type': 'application/json' },
        body: JSON.stringify({ enabled: !tool.enabled }),
      });
      if (res.ok) {
        setTools(prev => prev.map(x => x.name === tool.name ? { ...x, enabled: !tool.enabled } : x));
      }
    } catch (err) {
      console.error('Failed to toggle tool:', err);
    }
  };

  const deleteConnector = async (id: string) => {
    if (!confirm(t('confirmDeleteConnector'))) return;
    try {
      const res = await fetch(`/api/connectors/${id}`, { method: 'DELETE', headers: authHeaders });
      if (res.ok) {
        setConnectors(prev => prev.filter(c => c.id !== id));
        if (selectedId === id) {
          setSelectedId(null);
          setTools([]);
        }
      }
    } catch (err) {
      console.error('Failed to delete connector:', err);
    }
  };

  const buildPayload = () => {
    let config: any;
    if (form.ctype === 'api') {
      config = {
        tools: form.tools.map(tl => {
          const params = tryParseJSON(tl.paramsText) || { type: 'object', properties: {} };
          const item: any = {
            name: tl.name.trim(),
            description: tl.description.trim(),
            method: tl.method,
            url: tl.url.trim(),
            headers: parseKV(tl.headersText, ':'),
            params,
            enabled: tl.enabled,
          };
          if (tl.bodyTemplate.trim()) item.body_template = tl.bodyTemplate;
          return item;
        }),
      };
    } else if (form.transport === 'stdio') {
      config = {
        transport: 'stdio',
        command: form.command.trim(),
        args: form.argsText.trim() ? form.argsText.trim().split(/\s+/) : [],
        env: parseKV(form.envText, '='),
      };
    } else {
      config = {
        transport: 'http',
        url: form.mcpUrl.trim(),
        headers: parseKV(form.mcpHeadersText, ':'),
      };
    }
    return {
      name: form.name.trim(),
      ctype: form.ctype,
      description: form.description.trim(),
      config,
    };
  };

  const handleTest = async () => {
    setTestState('testing');
    setTestMsg('');
    try {
      const res = await fetch('/api/connectors/test', {
        method: 'POST',
        headers: { ...authHeaders, 'Content-Type': 'application/json' },
        body: JSON.stringify(buildPayload()),
      });
      const data = await res.json().catch(() => ({}));
      if (res.ok) {
        setTestState('ok');
        setTestMsg(data.message || 'OK');
      } else {
        setTestState('error');
        setTestMsg(data.error || 'Test failed');
      }
    } catch (err) {
      setTestState('error');
      setTestMsg(String(err));
    }
  };

  const handleSubmit = async () => {
    if (!form.name.trim()) {
      setFormError(t('connectorNameRequired'));
      return;
    }
    setSubmitting(true);
    setFormError('');
    try {
      const url = editingId ? `/api/connectors/${editingId}` : '/api/connectors';
      const method = editingId ? 'PUT' : 'POST';
      const res = await fetch(url, {
        method,
        headers: { ...authHeaders, 'Content-Type': 'application/json' },
        body: JSON.stringify(buildPayload()),
      });
      const data = await res.json().catch(() => ({}));
      if (res.ok) {
        setShowForm(false);
        setEditingId(null);
        setForm(emptyForm());
        fetchConnectors();
        if (editingId && selectedId === editingId) fetchTools(editingId);
      } else {
        setFormError(data.error || t('saveFailed'));
      }
    } catch (err) {
      setFormError(String(err));
    } finally {
      setSubmitting(false);
    }
  };

  const openCreate = () => {
    setForm(emptyForm());
    setEditingId(null);
    setFormError('');
    setTestState('idle');
    setTestMsg('');
    setShowForm(true);
  };

  const openEdit = (c: Connector) => {
    setForm(formFromConnector(c));
    setEditingId(c.id);
    setFormError('');
    setTestState('idle');
    setTestMsg('');
    setShowForm(true);
  };

  const updateTool = (idx: number, patch: Partial<ApiToolForm>) => {
    setForm(f => ({ ...f, tools: f.tools.map((tl, i) => i === idx ? { ...tl, ...patch } : tl) }));
  };

  const selectedConnector = connectors.find(c => c.id === selectedId);

  // ── Styles ──
  const inputStyle: React.CSSProperties = {
    width: '100%', padding: '8px 10px', borderRadius: 6,
    border: '1px solid #444', background: '#1a1a1a', color: '#eee',
    fontSize: 13, outline: 'none', boxSizing: 'border-box',
  };
  const labelStyle: React.CSSProperties = {
    display: 'block', color: '#888', fontSize: 11, marginBottom: 4,
    fontWeight: 500, textTransform: 'uppercase' as const, letterSpacing: '0.5px',
  };
  const btnPrimary: React.CSSProperties = {
    padding: '8px 16px', borderRadius: 6, border: 'none',
    background: '#1f6feb', color: '#fff', cursor: 'pointer',
    fontSize: 13, fontWeight: 500,
  };
  const btnSecondary: React.CSSProperties = {
    padding: '6px 14px', borderRadius: 6, border: '1px solid #444',
    background: 'transparent', color: '#ccc', cursor: 'pointer', fontSize: 13,
  };
  const toggleBtn = (on: boolean): React.CSSProperties => ({
    padding: '3px 10px', borderRadius: 12, fontSize: 11, cursor: 'pointer',
    border: on ? '1px solid #2ea04366' : '1px solid #444',
    background: on ? '#2ea04322' : 'transparent',
    color: on ? '#3fb950' : '#888',
  });
  const typeBadge = (ctype: string): React.CSSProperties => ({
    padding: '2px 8px', borderRadius: 4, fontSize: 10, fontWeight: 600,
    letterSpacing: '0.5px', textTransform: 'uppercase' as const,
    background: ctype === 'mcp' ? '#8250df22' : '#1f6feb22',
    color: ctype === 'mcp' ? '#a371f7' : '#58a6ff',
    border: `1px solid ${ctype === 'mcp' ? '#8250df44' : '#1f6feb44'}`,
  });
  const segBtn = (active: boolean): React.CSSProperties => ({
    flex: '1 1 auto', padding: '8px 12px', borderRadius: 6, cursor: 'pointer',
    fontSize: 12, fontWeight: 500, textAlign: 'center', whiteSpace: 'nowrap',
    border: active ? '1px solid #1f6feb' : '1px solid #444',
    background: active ? '#1f6feb22' : 'transparent',
    color: active ? '#58a6ff' : '#aaa',
  });

  return (
    <div style={{ display: 'flex', height: '100%' }}>
      <style>{`
        .connector-row:hover { background: #262626; }
      `}</style>
      {/* Left panel: form + connector list */}
      <div style={{
        width: 420, flexShrink: 0, borderRight: '1px solid #333',
        display: 'flex', flexDirection: 'column', overflow: 'hidden',
      }}>
        {/* Header */}
        <div style={{
          padding: '12px 16px', borderBottom: '1px solid #333',
          display: 'flex', alignItems: 'center', justifyContent: 'space-between',
        }}>
          <span style={{ fontWeight: 600, fontSize: 16 }}>{t('connectors')}</span>
          <div style={{ display: 'flex', gap: 8 }}>
            <button onClick={() => showForm ? setShowForm(false) : openCreate()} style={{
              ...btnPrimary,
              background: showForm ? '#444' : '#1f6feb',
            }}>
              {showForm ? '✕' : `+ ${t('addConnector')}`}
            </button>
            <button onClick={fetchConnectors} style={{
              padding: '4px 12px', borderRadius: 6, border: '1px solid #555',
              background: 'transparent', color: '#aaa', cursor: 'pointer', fontSize: 12,
            }}>
              {t('refresh')}
            </button>
          </div>
        </div>

        {/* Content */}
        <div style={{ flex: 1, overflowY: 'auto', padding: 16 }}>
          {/* ── Create/Edit Form ── */}
          {showForm && (
            <div style={{
              padding: 16, borderRadius: 8, border: '1px solid #1f6feb44',
              background: '#0d1117', marginBottom: 20,
            }}>
              <h4 style={{ fontSize: 14, margin: '0 0 14px 0', color: '#58a6ff' }}>
                {editingId ? t('editConnector') : t('addConnector')}
              </h4>

              {/* Name */}
              <div style={{ marginBottom: 12 }}>
                <label style={labelStyle}>{t('connectorName')}</label>
                <input value={form.name}
                  onChange={e => setForm({ ...form, name: e.target.value })}
                  placeholder={t('connectorNamePh')} style={inputStyle} />
              </div>

              {/* Description */}
              <div style={{ marginBottom: 12 }}>
                <label style={labelStyle}>{t('connectorDesc')}</label>
                <input value={form.description}
                  onChange={e => setForm({ ...form, description: e.target.value })}
                  placeholder={t('connectorDescPh')} style={inputStyle} />
              </div>

              {/* Type selector */}
              <div style={{ marginBottom: 12 }}>
                <label style={labelStyle}>{t('connectorType')}</label>
                <div style={{ display: 'flex', gap: 6 }}>
                  <button onClick={() => setForm({ ...form, ctype: 'api' })} style={segBtn(form.ctype === 'api')}>
                    {t('typeApi')}
                  </button>
                  <button onClick={() => setForm({ ...form, ctype: 'mcp' })} style={segBtn(form.ctype === 'mcp')}>
                    {t('typeMcp')}
                  </button>
                </div>
              </div>

              {/* ── API tools editor ── */}
              {form.ctype === 'api' && (
                <div style={{ marginBottom: 12 }}>
                  <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', marginBottom: 8 }}>
                    <label style={{ ...labelStyle, margin: 0 }}>{t('apiTools')}</label>
                    <button onClick={() => setForm({ ...form, tools: [...form.tools, emptyTool()] })} style={{
                      ...btnSecondary, padding: '3px 10px', fontSize: 12, color: '#58a6ff', borderColor: '#1f6feb44',
                    }}>
                      + {t('addTool')}
                    </button>
                  </div>
                  {form.tools.map((tl, idx) => (
                    <div key={idx} style={{
                      padding: 12, borderRadius: 6, border: '1px solid #333',
                      background: '#161b22', marginBottom: 10,
                    }}>
                      <div style={{ display: 'flex', gap: 8, marginBottom: 8 }}>
                        <div style={{ flex: 1 }}>
                          <label style={labelStyle}>{t('toolName')}</label>
                          <input value={tl.name}
                            onChange={e => updateTool(idx, { name: e.target.value })}
                            placeholder="get_weather" style={inputStyle} />
                        </div>
                        <div style={{ width: 100 }}>
                          <label style={labelStyle}>{t('httpMethod')}</label>
                          <select value={tl.method}
                            onChange={e => updateTool(idx, { method: e.target.value })}
                            style={{ ...inputStyle, cursor: 'pointer' }}>
                            {['GET', 'POST', 'PUT', 'PATCH', 'DELETE'].map(m => (
                              <option key={m} value={m}>{m}</option>
                            ))}
                          </select>
                        </div>
                      </div>
                      <div style={{ marginBottom: 8 }}>
                        <label style={labelStyle}>{t('toolDescription')}</label>
                        <textarea value={tl.description} rows={2}
                          onChange={e => updateTool(idx, { description: e.target.value })}
                          placeholder={t('toolDescPh')}
                          style={{ ...inputStyle, resize: 'vertical', fontFamily: 'inherit' }} />
                      </div>
                      <div style={{ marginBottom: 8 }}>
                        <label style={labelStyle}>{t('toolUrl')}</label>
                        <input value={tl.url}
                          onChange={e => updateTool(idx, { url: e.target.value })}
                          placeholder={t('toolUrlPh')} style={inputStyle} />
                      </div>
                      <div style={{ marginBottom: 8 }}>
                        <label style={labelStyle}>{t('headersLine')}</label>
                        <textarea value={tl.headersText} rows={2}
                          onChange={e => updateTool(idx, { headersText: e.target.value })}
                          placeholder="Authorization: Bearer xxx"
                          style={{ ...inputStyle, resize: 'vertical', fontFamily: 'monospace', fontSize: 12 }} />
                      </div>
                      <div style={{ marginBottom: 8 }}>
                        <label style={labelStyle}>{t('paramsSchema')}</label>
                        <textarea value={tl.paramsText} rows={4}
                          onChange={e => updateTool(idx, { paramsText: e.target.value })}
                          style={{ ...inputStyle, resize: 'vertical', fontFamily: 'monospace', fontSize: 12 }} />
                      </div>
                      {['POST', 'PUT', 'PATCH'].includes(tl.method) && (
                        <div style={{ marginBottom: 8 }}>
                          <label style={labelStyle}>{t('bodyTemplate')} ({t('optional')})</label>
                          <textarea value={tl.bodyTemplate} rows={2}
                            onChange={e => updateTool(idx, { bodyTemplate: e.target.value })}
                            placeholder={t('bodyTemplatePh')}
                            style={{ ...inputStyle, resize: 'vertical', fontFamily: 'monospace', fontSize: 12 }} />
                        </div>
                      )}
                      {form.tools.length > 1 && (
                        <button onClick={() => setForm({ ...form, tools: form.tools.filter((_, i) => i !== idx) })} style={{
                          ...btnSecondary, padding: '3px 10px', fontSize: 12, color: '#f85149', borderColor: '#f8514944',
                        }}>
                          {t('removeTool')}
                        </button>
                      )}
                    </div>
                  ))}
                </div>
              )}

              {/* ── MCP config editor ── */}
              {form.ctype === 'mcp' && (
                <div style={{ marginBottom: 12 }}>
                  <label style={labelStyle}>{t('mcpTransport')}</label>
                  <div style={{ display: 'flex', gap: 6, marginBottom: 10 }}>
                    <button onClick={() => setForm({ ...form, transport: 'stdio' })} style={segBtn(form.transport === 'stdio')}>
                      {t('transportStdio')}
                    </button>
                    <button onClick={() => setForm({ ...form, transport: 'http' })} style={segBtn(form.transport === 'http')}>
                      {t('transportHttp')}
                    </button>
                  </div>
                  {form.transport === 'stdio' ? (
                    <>
                      <div style={{ marginBottom: 8 }}>
                        <label style={labelStyle}>{t('mcpCommand')}</label>
                        <input value={form.command}
                          onChange={e => setForm({ ...form, command: e.target.value })}
                          placeholder={t('mcpCommandPh')} style={inputStyle} />
                      </div>
                      <div style={{ marginBottom: 8 }}>
                        <label style={labelStyle}>{t('mcpArgs')}</label>
                        <input value={form.argsText}
                          onChange={e => setForm({ ...form, argsText: e.target.value })}
                          placeholder={t('mcpArgsPh')} style={inputStyle} />
                      </div>
                      <div style={{ marginBottom: 8 }}>
                        <label style={labelStyle}>{t('mcpEnv')} ({t('optional')})</label>
                        <textarea value={form.envText} rows={2}
                          onChange={e => setForm({ ...form, envText: e.target.value })}
                          placeholder="API_KEY=xxx"
                          style={{ ...inputStyle, resize: 'vertical', fontFamily: 'monospace', fontSize: 12 }} />
                      </div>
                    </>
                  ) : (
                    <>
                      <div style={{ marginBottom: 8 }}>
                        <label style={labelStyle}>{t('mcpUrl')}</label>
                        <input value={form.mcpUrl}
                          onChange={e => setForm({ ...form, mcpUrl: e.target.value })}
                          placeholder={t('mcpUrlPh')} style={inputStyle} />
                      </div>
                      <div style={{ marginBottom: 8 }}>
                        <label style={labelStyle}>{t('headersLine')} ({t('optional')})</label>
                        <textarea value={form.mcpHeadersText} rows={2}
                          onChange={e => setForm({ ...form, mcpHeadersText: e.target.value })}
                          placeholder="Authorization: Bearer xxx"
                          style={{ ...inputStyle, resize: 'vertical', fontFamily: 'monospace', fontSize: 12 }} />
                      </div>
                    </>
                  )}
                </div>
              )}

              {/* Test result */}
              {testState !== 'idle' && (
                <div style={{
                  padding: '8px 10px', borderRadius: 6, marginBottom: 10, fontSize: 12,
                  background: testState === 'ok' ? '#2ea04322' : testState === 'error' ? '#f8514922' : '#1f6feb11',
                  color: testState === 'ok' ? '#3fb950' : testState === 'error' ? '#f85149' : '#888',
                  border: `1px solid ${testState === 'ok' ? '#2ea04344' : testState === 'error' ? '#f8514944' : '#1f6feb33'}`,
                }}>
                  {testState === 'testing' ? t('testing') : testMsg}
                </div>
              )}

              {/* Form error */}
              {formError && (
                <div style={{
                  padding: '8px 10px', borderRadius: 6, marginBottom: 10, fontSize: 12,
                  background: '#f8514922', color: '#f85149', border: '1px solid #f8514944',
                }}>
                  {formError}
                </div>
              )}

              {/* Actions */}
              <div style={{ display: 'flex', gap: 8 }}>
                <button onClick={handleTest} disabled={testState === 'testing'} style={{
                  ...btnSecondary,
                  opacity: testState === 'testing' ? 0.6 : 1,
                }}>
                  {t('testConnection')}
                </button>
                <div style={{ flex: 1 }} />
                <button onClick={() => { setShowForm(false); setEditingId(null); }} style={btnSecondary}>
                  {t('cancel')}
                </button>
                <button onClick={handleSubmit} disabled={submitting} style={{
                  ...btnPrimary,
                  opacity: submitting ? 0.6 : 1,
                }}>
                  {submitting ? t('saving') : t('save')}
                </button>
              </div>
            </div>
          )}

          {/* ── Connector list ── */}
          {loading ? (
            <div style={{ color: '#888', fontSize: 13 }}>{t('loading')}</div>
          ) : connectors.length === 0 ? (
            <div style={{ color: '#888', fontSize: 13, textAlign: 'center', marginTop: 40 }}>
              <div style={{ fontSize: 28, marginBottom: 8 }}>🔌</div>
              <div>{t('noConnectors')}</div>
              <div style={{ fontSize: 12, marginTop: 4, color: '#666' }}>{t('connectorsHint')}</div>
            </div>
          ) : (
            connectors.map(c => (
              <div key={c.id}
                className="connector-row"
                onClick={() => selectConnector(c.id)}
                style={{
                  padding: '10px 12px', borderRadius: 8, cursor: 'pointer',
                  border: selectedId === c.id ? '1px solid #1f6feb66' : '1px solid #333',
                  background: selectedId === c.id ? '#1f6feb11' : '#1e1e1e',
                  marginBottom: 8,
                }}>
                <div style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
                  <span style={typeBadge(c.ctype)}>{c.ctype}</span>
                  <span style={{ fontWeight: 600, fontSize: 13, flex: 1, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
                    {c.name}
                  </span>
                  <button onClick={e => { e.stopPropagation(); toggleConnector(c); }} style={toggleBtn(c.enabled)}>
                    {c.enabled ? t('enabled') : t('disabled')}
                  </button>
                </div>
                {c.description && (
                  <div style={{ color: '#888', fontSize: 12, marginTop: 4, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
                    {c.description}
                  </div>
                )}
                <div style={{ display: 'flex', gap: 10, marginTop: 6 }}>
                  {c.ctype === 'api' && (
                    <span style={{ color: '#666', fontSize: 11 }}>
                      {(c.config?.tools || []).length} {t('toolsUnit')}
                    </span>
                  )}
                  <button onClick={e => { e.stopPropagation(); openEdit(c); }} style={{
                    background: 'none', border: 'none', color: '#58a6ff', fontSize: 11, cursor: 'pointer', padding: 0,
                  }}>
                    {t('edit')}
                  </button>
                  <button onClick={e => { e.stopPropagation(); deleteConnector(c.id); }} style={{
                    background: 'none', border: 'none', color: '#f85149', fontSize: 11, cursor: 'pointer', padding: 0,
                  }}>
                    {t('delete')}
                  </button>
                </div>
              </div>
            ))
          )}
        </div>
      </div>

      {/* Right panel: tool details */}
      <div style={{ flex: 1, display: 'flex', flexDirection: 'column', overflow: 'hidden' }}>
        {selectedConnector ? (
          <>
            <div style={{
              padding: '12px 16px', borderBottom: '1px solid #333',
              display: 'flex', alignItems: 'center', gap: 10,
            }}>
              <span style={typeBadge(selectedConnector.ctype)}>{selectedConnector.ctype}</span>
              <span style={{ fontWeight: 600, fontSize: 15 }}>{selectedConnector.name}</span>
              <span style={{ color: '#888', fontSize: 12 }}>
                {tools.length} {t('toolsUnit')}
              </span>
              <div style={{ flex: 1 }} />
              <button onClick={() => fetchTools(selectedConnector.id)} style={btnSecondary}>
                {t('refresh')}
              </button>
            </div>
            <div style={{ flex: 1, overflowY: 'auto', padding: 16 }}>
              {!selectedConnector.enabled && (
                <div style={{
                  padding: '8px 12px', borderRadius: 6, marginBottom: 12, fontSize: 12,
                  background: '#44444422', color: '#aaa', border: '1px solid #444',
                }}>
                  {t('connectorDisabledHint')}
                </div>
              )}
              {toolsLoading ? (
                <div style={{ color: '#888', fontSize: 13 }}>{t('loading')}</div>
              ) : toolsError ? (
                <div style={{
                  padding: '10px 12px', borderRadius: 6, fontSize: 12,
                  background: '#f8514922', color: '#f85149', border: '1px solid #f8514944',
                }}>
                  {toolsError}
                </div>
              ) : tools.length === 0 ? (
                <div style={{ color: '#888', fontSize: 13, textAlign: 'center', marginTop: 40 }}>
                  {t('noTools')}
                </div>
              ) : (
                tools.map(tool => (
                  <div key={tool.name} style={{
                    padding: 12, borderRadius: 8, border: '1px solid #333',
                    background: '#1e1e1e', marginBottom: 10,
                    opacity: tool.enabled ? 1 : 0.55,
                  }}>
                    <div style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
                      <span style={{ fontWeight: 600, fontSize: 13, fontFamily: 'monospace', flex: 1 }}>
                        {tool.name}
                      </span>
                      <button onClick={() => toggleTool(tool)} style={toggleBtn(tool.enabled)}>
                        {tool.enabled ? t('enabled') : t('disabled')}
                      </button>
                    </div>
                    {tool.description && (
                      <div style={{ color: '#aaa', fontSize: 12, marginTop: 6, whiteSpace: 'pre-wrap' }}>
                        {tool.description}
                      </div>
                    )}
                    {tool.parameters && Object.keys(tool.parameters || {}).length > 0 && (
                      <div style={{ marginTop: 8 }}>
                        <button
                          onClick={() => setExpandedParams(p => ({ ...p, [tool.name]: !p[tool.name] }))}
                          style={{ background: 'none', border: 'none', color: '#58a6ff', fontSize: 11, cursor: 'pointer', padding: 0 }}>
                          {expandedParams[tool.name] ? t('collapse') : t('toolParams')}
                        </button>
                        {expandedParams[tool.name] && (
                          <pre style={{
                            marginTop: 6, padding: 10, borderRadius: 6, background: '#0d1117',
                            border: '1px solid #333', fontSize: 11, color: '#ccc',
                            overflowX: 'auto', maxHeight: 240, overflowY: 'auto',
                          }}>
                            {JSON.stringify(tool.parameters, null, 2)}
                          </pre>
                        )}
                      </div>
                    )}
                  </div>
                ))
              )}
            </div>
          </>
        ) : (
          <div style={{
            flex: 1, display: 'flex', alignItems: 'center', justifyContent: 'center',
            color: '#666', fontSize: 13,
          }}>
            {t('selectConnectorHint')}
          </div>
        )}
      </div>
    </div>
  );
}

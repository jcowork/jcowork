import { useState, useEffect, useCallback } from 'react';
import { useT } from '../i18n';

// ── Types ──

interface ModelInfo {
  id: string;
  name: string;
  context_length: number;
}

interface ProviderEntry {
  id: string;
  name: string;
  api_key: string;
  api_key_set: boolean;
  base_url: string;
  default_model: string;
  context_length: number;
  models: ModelInfo[];
}

interface ProviderFormState {
  id: string;
  name: string;
  api_key: string;
  base_url: string;
  default_model: string;
  context_length: number;
  models: ModelInfo[];
}

interface ProviderManagerProps {
  token: string;
  onClose: () => void;
  onSaved: () => void;
}

const emptyForm: ProviderFormState = {
  id: '',
  name: '',
  api_key: '',
  base_url: '',
  default_model: '',
  context_length: 128000,
  models: [],
};

export default function ProviderManager({ token, onClose, onSaved }: ProviderManagerProps) {
  const t = useT();
  const [entries, setEntries] = useState<ProviderEntry[]>([]);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [status, setStatus] = useState<'idle' | 'saved' | 'error'>('idle');
  const [editing, setEditing] = useState<ProviderFormState | null>(null);
  const [isLocal, setIsLocal] = useState(false);
  const [deleteConfirm, setDeleteConfirm] = useState<string | null>(null);

  const loadEntries = useCallback(() => {
    fetch('/api/providers/entries', {
      headers: { 'Authorization': `Bearer ${token}` },
    })
      .then(r => r.json())
      .then((data: { entries: ProviderEntry[] }) => {
        setEntries(data.entries || []);
        setLoading(false);
      })
      .catch(() => setLoading(false));
  }, [token]);

  useEffect(() => { loadEntries(); }, [loadEntries]);

  const handleSave = () => {
    // Build full entries from current list + any edits
    // We send the complete list of entries to the server
    const entriesToSave = entries.map(e => ({
      id: e.id,
      name: e.name,
      api_key: e.api_key || '', // masked keys from GET are not real keys
      base_url: e.base_url,
      default_model: e.default_model,
      context_length: e.context_length,
      models: e.models,
    }));

    // If we have an unsaved edit, include it
    if (editing) {
      const existingIdx = entriesToSave.findIndex(e => e.id === editing.id);
      if (existingIdx >= 0) {
        entriesToSave[existingIdx] = {
          ...entriesToSave[existingIdx],
          id: editing.id,
          name: editing.name,
          api_key: editing.api_key, // real key from form
          base_url: editing.base_url,
          default_model: editing.default_model,
          context_length: editing.context_length,
          models: editing.models,
        };
      } else {
        entriesToSave.push({
          id: editing.id,
          name: editing.name,
          api_key: editing.api_key,
          base_url: editing.base_url,
          default_model: editing.default_model,
          context_length: editing.context_length,
          models: editing.models,
        });
      }
    }

    setSaving(true);
    setStatus('idle');
    fetch('/api/providers', {
      method: 'POST',
      headers: {
        'Authorization': `Bearer ${token}`,
        'Content-Type': 'application/json',
      },
      body: JSON.stringify({ entries: entriesToSave }),
    })
      .then(r => {
        if (!r.ok) throw new Error(`HTTP ${r.status}`);
        return r.json();
      })
      .then(() => {
        setStatus('saved');
        setEditing(null);
        loadEntries();
        onSaved();
        setTimeout(() => setStatus('idle'), 2000);
      })
      .catch(() => setStatus('error'))
      .finally(() => setSaving(false));
  };

  const handleDelete = (id: string) => {
    const remaining = entries.filter(e => e.id !== id);
    setEntries(remaining);
    setDeleteConfirm(null);
    if (editing?.id === id) setEditing(null);
  };

  const startEdit = (entry: ProviderEntry) => {
    setEditing({
      id: entry.id,
      name: entry.name,
      api_key: '', // never pre-fill real key
      base_url: entry.base_url,
      default_model: entry.default_model,
      context_length: entry.context_length,
      models: entry.models.map(m => ({ ...m })),
    });
    setIsLocal(entry.id === 'llamacpp' || entry.id === 'local' || entry.api_key_set === false);
  };

  const startAdd = () => {
    setEditing({ ...emptyForm, models: [] });
    setIsLocal(false);
  };

  const cancelEdit = () => {
    setEditing(null);
    setIsLocal(false);
  };

  const addModel = () => {
    if (!editing) return;
    setEditing({
      ...editing,
      models: [...editing.models, { id: '', name: '', context_length: editing.context_length }],
    });
  };

  const removeModel = (idx: number) => {
    if (!editing) return;
    const newModels = editing.models.filter((_, i) => i !== idx);
    setEditing({ ...editing, models: newModels });
  };

  const updateModel = (idx: number, field: keyof ModelInfo, value: string | number) => {
    if (!editing) return;
    const newModels = editing.models.map((m, i) => i === idx ? { ...m, [field]: value } : m);
    setEditing({ ...editing, models: newModels });
  };

  const formatContext = (len: number) => {
    if (len >= 1_000_000) return `${(len / 1_000_000).toFixed(1)}M`;
    if (len >= 1000) return `${Math.round(len / 1000)}K`;
    return `${len}`;
  };

  const inputStyle: React.CSSProperties = {
    width: '100%',
    padding: '8px 10px',
    borderRadius: 6,
    border: '1px solid #444',
    background: '#1a1a1a',
    color: '#eee',
    fontSize: 13,
    outline: 'none',
    boxSizing: 'border-box',
  };

  const labelStyle: React.CSSProperties = {
    display: 'block',
    color: '#888',
    fontSize: 11,
    marginBottom: 4,
    fontWeight: 500,
    textTransform: 'uppercase' as const,
    letterSpacing: '0.5px',
  };

  const btnPrimary: React.CSSProperties = {
    padding: '6px 14px',
    borderRadius: 6,
    border: 'none',
    background: '#1f6feb',
    color: '#fff',
    cursor: 'pointer',
    fontSize: 13,
    fontWeight: 500,
  };

  const btnSecondary: React.CSSProperties = {
    padding: '6px 14px',
    borderRadius: 6,
    border: '1px solid #444',
    background: 'transparent',
    color: '#ccc',
    cursor: 'pointer',
    fontSize: 13,
  };

  const btnDanger: React.CSSProperties = {
    padding: '4px 10px',
    borderRadius: 6,
    border: '1px solid #f87171',
    background: 'transparent',
    color: '#f87171',
    cursor: 'pointer',
    fontSize: 12,
  };

  return (
    <div style={{
      position: 'fixed',
      inset: 0,
      background: 'rgba(0,0,0,0.6)',
      display: 'flex',
      alignItems: 'center',
      justifyContent: 'center',
      zIndex: 1000,
    }} onClick={e => { if (e.target === e.currentTarget) onClose(); }}>
      <div style={{
        background: '#121212',
        borderRadius: 12,
        width: '90%',
        maxWidth: 640,
        maxHeight: '85vh',
        overflow: 'auto',
        border: '1px solid #333',
      }}>
        {/* Header */}
        <div style={{
          display: 'flex',
          justifyContent: 'space-between',
          alignItems: 'center',
          padding: '16px 20px',
          borderBottom: '1px solid #2a2a2a',
        }}>
          <h3 style={{ fontSize: 16, fontWeight: 600, margin: 0 }}>{t('manageProviders')}</h3>
          <button onClick={onClose} style={{
            background: 'none', border: 'none', color: '#888', cursor: 'pointer', fontSize: 20,
          }}>✕</button>
        </div>

        <div style={{ padding: 20 }}>
          {loading ? (
            <div style={{ color: '#888', textAlign: 'center', padding: 20 }}>{t('loading')}</div>
          ) : entries.length === 0 && !editing ? (
            <div style={{ textAlign: 'center', padding: 30 }}>
              <div style={{ color: '#888', marginBottom: 16 }}>{t('noProviders')}</div>
              <button onClick={startAdd} style={btnPrimary}>{t('addProvider')}</button>
            </div>
          ) : (
            <>
              {/* Provider List */}
              <div style={{ marginBottom: 16 }}>
                {entries.map(entry => (
                  <div key={entry.id} style={{
                    display: 'flex',
                    alignItems: 'center',
                    padding: '10px 12px',
                    borderRadius: 8,
                    border: '1px solid #2a2a2a',
                    marginBottom: 8,
                    background: editing?.id === entry.id ? '#1a2332' : '#1a1a1a',
                  }}>
                    <div style={{ flex: 1 }}>
                      <div style={{ fontSize: 14, fontWeight: 500, color: '#eee' }}>
                        {entry.name}
                        {entry.api_key_set && (
                          <span style={{
                            display: 'inline-block',
                            padding: '1px 6px',
                            borderRadius: 4,
                            fontSize: 10,
                            marginLeft: 8,
                            background: '#23863622',
                            color: '#3fb950',
                          }}>{t('apiKeySet')}</span>
                        )}
                      </div>
                      <div style={{ fontSize: 11, color: '#666', marginTop: 2 }}>
                        {entry.id} · {entry.models.length} {t('models')} · {formatContext(entry.context_length)} ctx
                      </div>
                    </div>
                    <div style={{ display: 'flex', gap: 6 }}>
                      <button onClick={() => startEdit(entry)} style={{
                        ...btnSecondary, padding: '4px 10px', fontSize: 12,
                      }}>{t('edit')}</button>
                      {deleteConfirm === entry.id ? (
                        <div style={{ display: 'flex', gap: 4, alignItems: 'center' }}>
                          <span style={{ color: '#f87171', fontSize: 11 }}>{t('confirmDeleteProvider')}</span>
                          <button onClick={() => handleDelete(entry.id)} style={{
                            ...btnDanger, background: '#f87171', color: '#fff',
                          }}>OK</button>
                          <button onClick={() => setDeleteConfirm(null)} style={btnSecondary}>✕</button>
                        </div>
                      ) : (
                        <button onClick={() => setDeleteConfirm(entry.id)} style={btnDanger}>
                          {t('deleteProvider')}
                        </button>
                      )}
                    </div>
                  </div>
                ))}
              </div>

              {/* Add / Edit Form */}
              {editing && (
                <div style={{
                  padding: 16,
                  borderRadius: 8,
                  border: '1px solid #1f6feb44',
                  background: '#0d1117',
                  marginBottom: 16,
                }}>
                  <h4 style={{ fontSize: 14, marginBottom: 12, color: '#58a6ff' }}>
                    {entries.find(e => e.id === editing.id) ? t('editProvider') : t('addProvider')}
                  </h4>

                  {/* Local provider toggle */}
                  <label style={{
                    display: 'flex', alignItems: 'center', gap: 8, marginBottom: 12,
                    color: '#888', fontSize: 13, cursor: 'pointer',
                  }}>
                    <input
                      type="checkbox"
                      checked={isLocal}
                      onChange={e => setIsLocal(e.target.checked)}
                    />
                    {t('localProvider')} — {t('localProviderDesc')}
                  </label>

                  <div style={{ display: 'grid', gridTemplateColumns: '1fr 1fr', gap: 10, marginBottom: 10 }}>
                    <div>
                      <label style={labelStyle}>{t('providerId')}</label>
                      <input
                        value={editing.id}
                        onChange={e => setEditing({ ...editing, id: e.target.value.toLowerCase().replace(/[^a-z0-9_-]/g, '') })}
                        placeholder="e.g. deepseek"
                        style={inputStyle}
                        disabled={!!entries.find(e => e.id === editing.id)}
                      />
                    </div>
                    <div>
                      <label style={labelStyle}>{t('providerName')}</label>
                      <input
                        value={editing.name}
                        onChange={e => setEditing({ ...editing, name: e.target.value })}
                        placeholder="e.g. DeepSeek"
                        style={inputStyle}
                      />
                    </div>
                  </div>

                  {!isLocal && (
                    <div style={{ marginBottom: 10 }}>
                      <label style={labelStyle}>{t('apiKey')}</label>
                      <input
                        type="password"
                        value={editing.api_key}
                        onChange={e => setEditing({ ...editing, api_key: e.target.value })}
                        placeholder={t('apiKeyPlaceholder')}
                        style={inputStyle}
                      />
                    </div>
                  )}

                  <div style={{ display: 'grid', gridTemplateColumns: '2fr 1fr', gap: 10, marginBottom: 10 }}>
                    <div>
                      <label style={labelStyle}>{t('baseUrl')}</label>
                      <input
                        value={editing.base_url}
                        onChange={e => setEditing({ ...editing, base_url: e.target.value })}
                        placeholder={t('baseUrlPlaceholder')}
                        style={inputStyle}
                      />
                    </div>
                    <div>
                      <label style={labelStyle}>{t('contextLength')}</label>
                      <input
                        type="number"
                        value={editing.context_length}
                        onChange={e => setEditing({ ...editing, context_length: parseInt(e.target.value) || 0 })}
                        style={inputStyle}
                      />
                    </div>
                  </div>

                  <div style={{ marginBottom: 12 }}>
                    <label style={labelStyle}>{t('defaultModel')}</label>
                    <input
                      value={editing.default_model}
                      onChange={e => setEditing({ ...editing, default_model: e.target.value })}
                      placeholder="e.g. deepseek-chat"
                      style={inputStyle}
                    />
                  </div>

                  {/* Models List */}
                  <div style={{ marginBottom: 10 }}>
                    <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center', marginBottom: 6 }}>
                      <label style={{ ...labelStyle, marginBottom: 0 }}>{t('models')}</label>
                      <button onClick={addModel} style={{ ...btnSecondary, padding: '2px 8px', fontSize: 11 }}>
                        + {t('addModel')}
                      </button>
                    </div>
                    {editing.models.map((model, idx) => (
                      <div key={idx} style={{
                        display: 'grid',
                        gridTemplateColumns: '1fr 1fr 100px 30px',
                        gap: 6,
                        marginBottom: 6,
                        alignItems: 'center',
                      }}>
                        <input
                          value={model.id}
                          onChange={e => updateModel(idx, 'id', e.target.value)}
                          placeholder={t('modelId')}
                          style={{ ...inputStyle, fontSize: 12, padding: '6px 8px' }}
                        />
                        <input
                          value={model.name}
                          onChange={e => updateModel(idx, 'name', e.target.value)}
                          placeholder={t('modelName')}
                          style={{ ...inputStyle, fontSize: 12, padding: '6px 8px' }}
                        />
                        <input
                          type="number"
                          value={model.context_length}
                          onChange={e => updateModel(idx, 'context_length', parseInt(e.target.value) || 0)}
                          placeholder="ctx"
                          title={t('modelContextLength')}
                          style={{ ...inputStyle, fontSize: 12, padding: '6px 8px' }}
                        />
                        <button onClick={() => removeModel(idx)} style={{
                          background: 'none', border: 'none', color: '#f87171', cursor: 'pointer', fontSize: 14,
                        }}>✕</button>
                      </div>
                    ))}
                  </div>

                  {/* Form Actions */}
                  <div style={{ display: 'flex', gap: 8, justifyContent: 'flex-end' }}>
                    <button onClick={cancelEdit} style={btnSecondary}>{t('cancelProvider')}</button>
                    <button
                      onClick={handleSave}
                      disabled={saving || !editing.id || !editing.name || (!isLocal && !editing.api_key)}
                      style={{
                        ...btnPrimary,
                        opacity: (saving || !editing.id || !editing.name || (!isLocal && !editing.api_key)) ? 0.5 : 1,
                        cursor: (saving || !editing.id || !editing.name || (!isLocal && !editing.api_key)) ? 'not-allowed' : 'pointer',
                      }}
                    >
                      {saving ? t('saving') : t('saveProvider')}
                    </button>
                  </div>

                  {status === 'saved' && <div style={{ color: '#3fb950', fontSize: 13, marginTop: 8 }}>{t('providersSaved')}</div>}
                  {status === 'error' && <div style={{ color: '#f87171', fontSize: 13, marginTop: 8 }}>{t('saveFailed')}</div>}
                </div>
              )}

              {/* Bottom Actions */}
              {!editing && (
                <div style={{ display: 'flex', gap: 8 }}>
                  <button onClick={startAdd} style={btnPrimary}>+ {t('addProvider')}</button>
                </div>
              )}
            </>
          )}
        </div>
      </div>
    </div>
  );
}

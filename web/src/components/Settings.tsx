import { useState, useEffect } from 'react';
import { useT } from '../i18n';
import ProviderManager from './ProviderManager';

interface ModelInfo {
  id: string;
  name: string;
  context_length: number;
}

interface ProviderInfo {
  id: string;
  name: string;
  models: ModelInfo[];
}

interface SettingsProps {
  onClose: () => void;
  userId: string;
  token: string;
}

export default function Settings({ onClose, userId, token }: SettingsProps) {
  const t = useT();
  const [providers, setProviders] = useState<ProviderInfo[]>([]);
  const [selectedProvider, setSelectedProvider] = useState<string>('');
  const [selectedModel, setSelectedModel] = useState<string>('');
  const [loading, setLoading] = useState(true);
  const [agentIdentity, setAgentIdentity] = useState<string>('');
  const [identityLoading, setIdentityLoading] = useState(true);
  const [identitySaving, setIdentitySaving] = useState(false);
  const [identityStatus, setIdentityStatus] = useState<'idle' | 'saved' | 'error'>('idle');
  const [showProviderManager, setShowProviderManager] = useState(false);
  const [loadError, setLoadError] = useState('');

  // Feishu config state
  const [feishuAppId, setFeishuAppId] = useState('');
  const [feishuAppSecret, setFeishuAppSecret] = useState('');
  const [feishuVerificationToken, setFeishuVerificationToken] = useState('');
  const [feishuEncryptKey, setFeishuEncryptKey] = useState('');
  const [feishuConfigured, setFeishuConfigured] = useState(false);
  const [feishuLoading, setFeishuLoading] = useState(true);
  const [feishuSaving, setFeishuSaving] = useState(false);
  const [feishuStatus, setFeishuStatus] = useState<'idle' | 'saved' | 'deleted' | 'error'>('idle');

  const MODEL_KEY = `jcowork_model_${userId}`;

  // Save model selection to localStorage whenever it changes
  const saveModel = (provider: string, model: string) => {
    localStorage.setItem(MODEL_KEY, JSON.stringify({ provider, model }));
  };

  useEffect(() => {
    fetch('/api/agent-identity', {
      headers: { 'Authorization': `Bearer ${token}` },
    })
      .then(r => {
        if (!r.ok) throw new Error(`HTTP ${r.status}`);
        return r.json();
      })
      .then((data: { identity: string }) => {
        setAgentIdentity(data.identity || '');
        setIdentityLoading(false);
      })
      .catch(() => setIdentityLoading(false));
  }, []);

  useEffect(() => {
    fetch('/api/feishu/config', {
      headers: { 'Authorization': `Bearer ${token}` },
    })
      .then(r => {
        if (!r.ok) throw new Error(`HTTP ${r.status}`);
        return r.json();
      })
      .then((data: { app_id: string; verification_token: string; encrypt_key: string; is_configured: boolean }) => {
        setFeishuAppId(data.app_id || '');
        setFeishuVerificationToken(data.verification_token || '');
        setFeishuEncryptKey(data.encrypt_key || '');
        setFeishuConfigured(data.is_configured);
        setFeishuLoading(false);
      })
      .catch(() => setFeishuLoading(false));
  }, []);

  const saveAgentIdentity = () => {
    setIdentitySaving(true);
    setIdentityStatus('idle');
    fetch('/api/agent-identity', {
      method: 'PUT',
      headers: {
        'Authorization': `Bearer ${token}`,
        'Content-Type': 'application/json',
      },
      body: JSON.stringify({ identity: agentIdentity }),
    })
      .then(r => {
        if (!r.ok) throw new Error(`HTTP ${r.status}`);
        return r.json();
      })
      .then(() => {
        setIdentityStatus('saved');
        setTimeout(() => setIdentityStatus('idle'), 2000);
      })
      .catch(() => setIdentityStatus('error'))
      .finally(() => setIdentitySaving(false));
  };

  const saveFeishuConfig = () => {
    setFeishuSaving(true);
    setFeishuStatus('idle');
    fetch('/api/feishu/config', {
      method: 'PUT',
      headers: {
        'Authorization': `Bearer ${token}`,
        'Content-Type': 'application/json',
      },
      body: JSON.stringify({
        app_id: feishuAppId,
        app_secret: feishuAppSecret,
        verification_token: feishuVerificationToken,
        encrypt_key: feishuEncryptKey || undefined,
      }),
    })
      .then(r => {
        if (!r.ok) return r.json().then(e => { throw new Error(e.error || `HTTP ${r.status}`); });
        return r.json();
      })
      .then(() => {
        setFeishuConfigured(true);
        setFeishuAppSecret(''); // Clear secret after save
        setFeishuStatus('saved');
        setTimeout(() => setFeishuStatus('idle'), 2000);
      })
      .catch(() => setFeishuStatus('error'))
      .finally(() => setFeishuSaving(false));
  };

  const deleteFeishuConfig = () => {
    setFeishuSaving(true);
    setFeishuStatus('idle');
    fetch('/api/feishu/config', {
      method: 'DELETE',
      headers: { 'Authorization': `Bearer ${token}` },
    })
      .then(r => {
        if (!r.ok) throw new Error(`HTTP ${r.status}`);
        return r.json();
      })
      .then(() => {
        setFeishuAppId('');
        setFeishuAppSecret('');
        setFeishuVerificationToken('');
        setFeishuEncryptKey('');
        setFeishuConfigured(false);
        setFeishuStatus('deleted');
        setTimeout(() => setFeishuStatus('idle'), 2000);
      })
      .catch(() => setFeishuStatus('error'))
      .finally(() => setFeishuSaving(false));
  };

  useEffect(() => {
    fetch('/api/providers', {
      headers: { 'Authorization': `Bearer ${token}` },
    })
      .then(r => {
        if (!r.ok) throw new Error(`HTTP ${r.status}`);
        return r.json();
      })
      .then((data: { providers: ProviderInfo[]; default_model: string }) => {
        const providers = data.providers;
        const serverDefault = data.default_model || ''; // e.g. "moonshot:kimi-k2.6"
        setProviders(providers);
        setLoading(false);
        // Auto-select from localStorage or server default
        const saved = localStorage.getItem(MODEL_KEY);
        let initialProvider = '';
        let initialModel = '';
        if (saved) {
          try {
            const parsed = JSON.parse(saved);
            initialProvider = parsed.provider || '';
            initialModel = parsed.model || '';
          } catch {}
        }
        // If no saved preference, use server default
        if (!initialProvider && serverDefault.includes(':')) {
          const colonIdx = serverDefault.indexOf(':');
          initialProvider = serverDefault.slice(0, colonIdx);
          initialModel = serverDefault.slice(colonIdx + 1);
        }
        // Validate saved/default values exist in providers
        const validProvider = providers.find(p => p.id === initialProvider);
        if (validProvider) {
          setSelectedProvider(initialProvider);
          const validModel = validProvider.models.find(m => m.id === initialModel);
          if (validModel) {
            setSelectedModel(initialModel);
          } else if (validProvider.models.length > 0) {
            setSelectedModel(validProvider.models[0].id);
            saveModel(initialProvider, validProvider.models[0].id);
          }
        } else if (providers.length > 0) {
          setSelectedProvider(providers[0].id);
          if (providers[0].models.length > 0) {
            setSelectedModel(providers[0].models[0].id);
            saveModel(providers[0].id, providers[0].models[0].id);
          }
        }
      })
      .catch(() => {
        setLoading(false);
        setLoadError('Failed to load settings. Please try refreshing the page.');
      });
  }, []);

  const currentProvider = providers.find(p => p.id === selectedProvider);
  const currentModel = currentProvider?.models.find(m => m.id === selectedModel);

  const handleProviderChange = (providerId: string) => {
    setSelectedProvider(providerId);
    const provider = providers.find(p => p.id === providerId);
    if (provider && provider.models.length > 0) {
      const model = provider.models[0].id;
      setSelectedModel(model);
      saveModel(providerId, model);
    } else {
      setSelectedModel('');
      saveModel(providerId, '');
    }
  };

  const formatContext = (len: number) => {
    if (len >= 1_000_000) return `${len / 1_000_000}M`;
    if (len >= 1000) return `${len / 1000}K`;
    return `${len}`;
  };

  const selectStyle: React.CSSProperties = {
    width: '100%',
    padding: '10px 12px',
    borderRadius: 8,
    border: '1px solid #444',
    background: '#1a1a1a',
    color: '#eee',
    fontSize: 14,
    outline: 'none',
    cursor: 'pointer',
    appearance: 'none',
    backgroundImage: 'url("data:image/svg+xml,%3Csvg xmlns=\'http://www.w3.org/2000/svg\' width=\'12\' height=\'12\' fill=\'%23888\' viewBox=\'0 0 16 16\'%3E%3Cpath d=\'M8 11L3 6h10z\'/%3E%3C/svg%3E")',
    backgroundRepeat: 'no-repeat',
    backgroundPosition: 'right 12px center',
  };

  const cardStyle: React.CSSProperties = {
    background: '#1a1a1a',
    borderRadius: 12,
    padding: 20,
    border: '1px solid #2a2a2a',
  };

  const labelStyle: React.CSSProperties = {
    display: 'block',
    color: '#888',
    fontSize: 12,
    marginBottom: 8,
    fontWeight: 500,
    textTransform: 'uppercase' as const,
    letterSpacing: '0.5px',
  };

  const tagStyle: React.CSSProperties = {
    display: 'inline-block',
    padding: '2px 8px',
    borderRadius: 4,
    fontSize: 11,
    marginLeft: 8,
    verticalAlign: 'middle',
  };

  return (
    <div style={{ padding: 24, maxWidth: 600, margin: '0 auto' }}>
      <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center', marginBottom: 28 }}>
        <h2 style={{ fontSize: 22, fontWeight: 600 }}>{t('settings')}</h2>
        <button
          onClick={onClose}
          style={{
            padding: '8px 18px', borderRadius: 8, border: '1px solid #444',
            background: 'transparent', color: '#ccc', cursor: 'pointer', fontSize: 14,
          }}
        >
          {t('backToChat')}
        </button>
      </div>

      {loadError && (
        <div style={{
          padding: '12px 16px', marginBottom: 16, borderRadius: 8,
          background: '#3b1111', border: '1px solid #f87171', color: '#fca5a5', fontSize: 14,
        }}>
          ⚠️ {loadError}
        </div>
      )}

      {/* LLM Provider Section */}
      <div style={cardStyle}>
        <h3 style={{ fontSize: 16, marginBottom: 16, display: 'flex', alignItems: 'center', gap: 8 }}>
          <span style={{ fontSize: 20 }}>🤖</span> LLM Provider
        </h3>

        {loading ? (
          <div style={{ color: '#888', textAlign: 'center', padding: 20 }}>{t('loadingProviders')}</div>
        ) : providers.length === 0 ? (
          <div style={{ color: '#f87171', textAlign: 'center', padding: 20 }}>
            No LLM providers registered. Please set API keys in .env
          </div>
        ) : (
          <>
            <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', marginBottom: 20 }}>
              <div style={{ flex: 1 }}>
                <label style={labelStyle}>{t('provider')}</label>
                <select
                  value={selectedProvider}
                  onChange={e => handleProviderChange(e.target.value)}
                  style={selectStyle}
                >
                  {providers.map(p => (
                    <option key={p.id} value={p.id}>{p.name}</option>
                  ))}
                </select>
                <div style={{ color: '#666', fontSize: 12, marginTop: 4 }}>
                  {providers.length} provider{providers.length !== 1 ? 's' : ''} available
                </div>
              </div>
              <button
                onClick={() => setShowProviderManager(true)}
                style={{
                  padding: '8px 14px',
                  borderRadius: 8,
                  border: '1px solid #1f6feb',
                  background: 'transparent',
                  color: '#58a6ff',
                  cursor: 'pointer',
                  fontSize: 13,
                  fontWeight: 500,
                  marginLeft: 12,
                  whiteSpace: 'nowrap',
                  alignSelf: 'flex-end',
                }}
              >
                ⚙ {t('manageProviders')}
              </button>
            </div>

            {/* Model Select */}
            {currentProvider && currentProvider.models.length > 0 && (
              <div style={{ marginBottom: 16 }}>
                <label style={labelStyle}>{t('model')}</label>
                <select
                  value={selectedModel}
                  onChange={e => { setSelectedModel(e.target.value); saveModel(selectedProvider, e.target.value); }}
                  style={selectStyle}
                >
                  {currentProvider.models.map(m => (
                    <option key={m.id} value={m.id}>
                      {m.name} ({formatContext(m.context_length)} context)
                    </option>
                  ))}
                </select>
              </div>
            )}

            {/* Current Selection Summary */}
            {currentProvider && currentModel && (
              <div style={{
                marginTop: 16,
                padding: '12px 16px',
                borderRadius: 8,
                background: '#0d1117',
                border: '1px solid #30363d',
              }}>
                <div style={{ fontSize: 13, color: '#888', marginBottom: 4 }}>{t('activeSelection')}</div>
                <div style={{ fontSize: 15, fontWeight: 500 }}>
                  {currentProvider.name}
                  <span style={{ ...tagStyle, background: '#1f6feb22', color: '#58a6ff' }}>
                    {currentModel.name}
                  </span>
                  <span style={{ ...tagStyle, background: '#23863622', color: '#3fb950' }}>
                    {formatContext(currentModel.context_length)} ctx
                  </span>
                </div>
                <div style={{ fontSize: 12, color: '#666', marginTop: 4 }}>
                  Model ID: <code style={{ color: '#c9d1d9', background: '#161b22', padding: '2px 6px', borderRadius: 4 }}>
                    {selectedProvider}:{selectedModel}
                  </code>
                </div>
              </div>
            )}
          </>
        )}
      </div>

      {/* Provider Manager Modal */}
      {showProviderManager && (
        <ProviderManager
          token={token}
          onClose={() => setShowProviderManager(false)}
          onSaved={() => {
            // Refresh providers list
            fetch('/api/providers', {
              headers: { 'Authorization': `Bearer ${token}` },
            })
              .then(r => r.json())
              .then((data: { providers: ProviderInfo[]; default_model: string }) => {
                setProviders(data.providers || []);
              })
              .catch(() => {});
          }}
        />
      )}

      {/* Agent Identity Section */}
      <div style={{ ...cardStyle, marginTop: 16 }}>
        <h3 style={{ fontSize: 16, marginBottom: 8, display: 'flex', alignItems: 'center', gap: 8 }}>
          <span style={{ fontSize: 20 }}>🪪</span> {t('agentIdentity')}
        </h3>
        <div style={{ color: '#888', fontSize: 13, lineHeight: 1.6, marginBottom: 12 }}>
          {t('agentIdentityDesc')}
        </div>
        {identityLoading ? (
          <div style={{ color: '#888', fontSize: 13 }}>{t('loading')}</div>
        ) : (
          <>
            <textarea
              value={agentIdentity}
              onChange={e => setAgentIdentity(e.target.value)}
              placeholder={t('identityPlaceholder')}
              rows={5}
              style={{
                width: '100%',
                padding: '10px 12px',
                borderRadius: 8,
                border: '1px solid #444',
                background: '#1a1a1a',
                color: '#eee',
                fontSize: 13,
                outline: 'none',
                resize: 'vertical',
                fontFamily: 'inherit',
                lineHeight: 1.6,
                boxSizing: 'border-box',
              }}
            />
            <div style={{ display: 'flex', alignItems: 'center', gap: 12, marginTop: 10 }}>
              <button
                onClick={saveAgentIdentity}
                disabled={identitySaving}
                style={{
                  padding: '8px 20px',
                  borderRadius: 8,
                  border: 'none',
                  background: identitySaving ? '#333' : '#1f6feb',
                  color: '#fff',
                  cursor: identitySaving ? 'not-allowed' : 'pointer',
                  fontSize: 14,
                  fontWeight: 500,
                }}
              >
                {identitySaving ? t('saving') : t('saveIdentity')}
              </button>
              {agentIdentity && (
                <button
                  onClick={() => setAgentIdentity('')}
                  style={{
                    padding: '8px 16px',
                    borderRadius: 8,
                    border: '1px solid #444',
                    background: 'transparent',
                    color: '#888',
                    cursor: 'pointer',
                    fontSize: 14,
                  }}
                >
                  {t('resetDefault')}
                </button>
              )}
              {identityStatus === 'saved' && (
                <span style={{ color: '#3fb950', fontSize: 13 }}>{t('saved')}</span>
              )}
              {identityStatus === 'error' && (
                <span style={{ color: '#f87171', fontSize: 13 }}>{t('failedToSave')}</span>
              )}
            </div>
          </>
        )}
      </div>

      {/* Feishu Integration Section */}
      <div style={{ ...cardStyle, marginTop: 16 }}>
        <h3 style={{ fontSize: 16, marginBottom: 8, display: 'flex', alignItems: 'center', gap: 8 }}>
          <span style={{ fontSize: 20 }}>🐤</span> {t('feishuIntegration')}
          {feishuConfigured && (
            <span style={{ ...tagStyle, background: '#23863622', color: '#3fb950' }}>{t('connected')}</span>
          )}
        </h3>
        <div style={{ color: '#888', fontSize: 13, lineHeight: 1.6, marginBottom: 12 }}>
          {t('feishuDesc')}
        </div>
        {feishuLoading ? (
          <div style={{ color: '#888', fontSize: 13 }}>{t('loading')}</div>
        ) : (
          <>
            <div style={{ marginBottom: 12 }}>
              <label style={labelStyle}>App ID</label>
              <input
                value={feishuAppId}
                onChange={e => setFeishuAppId(e.target.value)}
                placeholder="cli_xxxxx"
                style={{
                  width: '100%', padding: '10px 12px', borderRadius: 8,
                  border: '1px solid #444', background: '#1a1a1a', color: '#eee',
                  fontSize: 13, outline: 'none', boxSizing: 'border-box',
                }}
              />
            </div>
            <div style={{ marginBottom: 12 }}>
              <label style={labelStyle}>App Secret</label>
              <input
                type="password"
                value={feishuAppSecret}
                onChange={e => setFeishuAppSecret(e.target.value)}
                placeholder={feishuConfigured ? t('keepCurrent') : t('enterSecret')}
                style={{
                  width: '100%', padding: '10px 12px', borderRadius: 8,
                  border: '1px solid #444', background: '#1a1a1a', color: '#eee',
                  fontSize: 13, outline: 'none', boxSizing: 'border-box',
                }}
              />
            </div>
            <div style={{ marginBottom: 12 }}>
              <label style={labelStyle}>Verification Token</label>
              <input
                value={feishuVerificationToken}
                onChange={e => setFeishuVerificationToken(e.target.value)}
                placeholder={t('tokenFromFeishu')}
                style={{
                  width: '100%', padding: '10px 12px', borderRadius: 8,
                  border: '1px solid #444', background: '#1a1a1a', color: '#eee',
                  fontSize: 13, outline: 'none', boxSizing: 'border-box',
                }}
              />
            </div>
            <div style={{ marginBottom: 16 }}>
              <label style={labelStyle}>{t('encryptKey')} ({t('optional')})</label>
              <input
                value={feishuEncryptKey}
                onChange={e => setFeishuEncryptKey(e.target.value)}
                placeholder={t('optional')}
                style={{
                  width: '100%', padding: '10px 12px', borderRadius: 8,
                  border: '1px solid #444', background: '#1a1a1a', color: '#eee',
                  fontSize: 13, outline: 'none', boxSizing: 'border-box',
                }}
              />
            </div>
            <div style={{ display: 'flex', alignItems: 'center', gap: 12 }}>
              <button
                onClick={saveFeishuConfig}
                disabled={feishuSaving || !feishuAppId || (!feishuAppSecret && !feishuConfigured)}
                style={{
                  padding: '8px 20px', borderRadius: 8, border: 'none',
                  background: (feishuSaving || !feishuAppId || (!feishuAppSecret && !feishuConfigured)) ? '#333' : '#1f6feb',
                  color: '#fff', cursor: (feishuSaving || !feishuAppId || (!feishuAppSecret && !feishuConfigured)) ? 'not-allowed' : 'pointer',
                  fontSize: 14, fontWeight: 500,
                }}
              >
                {feishuSaving ? t('saving') : t('saveFeishu')}
              </button>
              {feishuConfigured && (
                <button
                  onClick={deleteFeishuConfig}
                  disabled={feishuSaving}
                  style={{
                    padding: '8px 16px', borderRadius: 8, border: '1px solid #f87171',
                    background: 'transparent', color: '#f87171', cursor: feishuSaving ? 'not-allowed' : 'pointer',
                    fontSize: 14,
                  }}
                >
                  {t('deleteFeishu')}
                </button>
              )}
              {feishuStatus === 'saved' && <span style={{ color: '#3fb950', fontSize: 13 }}>{t('saved')}</span>}
              {feishuStatus === 'deleted' && <span style={{ color: '#f87171', fontSize: 13 }}>{t('deleted')}</span>}
              {feishuStatus === 'error' && <span style={{ color: '#f87171', fontSize: 13 }}>{t('failedToSave')}</span>}
            </div>
          </>
        )}
      </div>

      {/* Memory Section */}
      <div style={{ ...cardStyle, marginTop: 16 }}>
        <h3 style={{ fontSize: 16, marginBottom: 8, display: 'flex', alignItems: 'center', gap: 8 }}>
          <span style={{ fontSize: 20 }}>🧠</span> {t('memory')}
        </h3>
        <div style={{ color: '#888', fontSize: 13, lineHeight: 1.6 }}>
          {t('feishuMemoryDesc')}
        </div>
      </div>

      {/* Skills Section */}
      <div style={{ ...cardStyle, marginTop: 16 }}>
        <h3 style={{ fontSize: 16, marginBottom: 8, display: 'flex', alignItems: 'center', gap: 8 }}>
          <span style={{ fontSize: 20 }}>⚡</span> {t('skills')}
        </h3>
        <div style={{ color: '#888', fontSize: 13, lineHeight: 1.6 }}>
          {t('feishuSkillsDesc')}
        </div>
      </div>
    </div>
  );
}
